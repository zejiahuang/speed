//! UDP relay, including DNS interception.
//!
//! UDP has no connection, so there is nothing to terminate. The relay keeps a
//! NAT style table keyed by the five tuple, opens one connected upstream socket
//! per entry, and copies datagrams across. QUIC rides on this path unchanged,
//! which is why HTTP/3 works without the kernel knowing anything about it.
//!
//! # Why connected sockets
//!
//! The upstream socket is `connect`ed rather than used with `sendto`. A connected
//! UDP socket only accepts datagrams from the peer it was connected to, so an
//! off-path attacker cannot inject a reply into a flow. The cost is that a server
//! which answers from a different address than the one dialled is dropped, which
//! is rare and preferable to accepting anything that arrives.
//!
//! # DNS
//!
//! Port 53 is special cased twice over:
//!
//! * **Answered locally** when the rule set owns the queried name and carries
//!   concrete addresses of the requested family. The rule's `ips` list is handed
//!   straight to the client, and the mapping is recorded so the connection that
//!   follows is recognised and steered.
//! * **Observed** when the query is forwarded. Every address in the answer is
//!   recorded against the queried name with the record's own TTL, which is how
//!   the data plane later recovers a domain from an address it was given.
//!
//! # Fragmentation
//!
//! A fragmented datagram cannot be relayed, because a datagram socket only
//! accepts whole payloads and reassembly is not implemented. Datagrams that
//! arrive as IP fragments are therefore dropped. This is a deliberate trade: the
//! protocols that matter here — DNS and QUIC — keep their datagrams under the
//! path MTU on purpose, and the client-to-tunnel direction is the only one where
//! the kernel would have to reassemble, because replies are read from a real
//! socket whose own stack already reassembled them.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::io::RawFd;
use std::time::{Duration, Instant};

use watt_rules::{Family, Strategy};

use crate::config::{StackConfig, Stats};
use crate::dns;
use crate::flow::FlowKey;
use crate::packet::{self, IpHeader, PROTO_UDP};
use crate::planner::Planner;
use crate::poller::{Poller, INTEREST_READ, INTEREST_WRITE};
use crate::upstream::{is_retryable, Protector, UpstreamSocket};

/// The port whose datagrams are inspected as DNS.
pub const DNS_PORT: u16 = 53;

/// Largest UDP payload an IP packet can carry.
const MAX_DATAGRAM: usize = 65_535;

/// Datagrams read from one upstream socket per service pass.
///
/// A busy flow can otherwise monopolise the loop while other flows starve.
const DRAIN_BATCH: usize = 32;

/// Tunables copied out of [`StackConfig`].
#[derive(Debug, Clone, Copy)]
struct Tuning {
    mtu: usize,
    max_flows: usize,
    idle_timeout: Duration,
    buffer_limit: usize,
    answer_dns: bool,
    dns_ttl: u32,
    observe_dns: bool,
}

/// One relayed UDP conversation.
#[derive(Debug)]
struct UdpFlow {
    key: FlowKey,
    /// Where the client sent the datagram. Replies are emitted from here, which
    /// is what keeps the relay transparent.
    requested: SocketAddr,
    /// Where the kernel actually sends. Differs from `requested` when a rule or a
    /// static rewrite redirected the flow.
    target: SocketAddr,
    socket: UpstreamSocket,
    steered: bool,
    reason: &'static str,
    /// True when this flow carries DNS, so replies are inspected.
    dns: bool,
    to_upstream: VecDeque<Vec<u8>>,
    queued_upstream: usize,
    opened_at: Instant,
    last_activity: Instant,
}

impl UdpFlow {
    /// Queue a datagram for the upstream side, refusing it when the flow is
    /// already holding as much as it is allowed to.
    fn queue_upstream(&mut self, payload: &[u8], limit: usize) -> bool {
        if self.queued_upstream + payload.len() > limit {
            return false;
        }
        self.queued_upstream += payload.len();
        self.to_upstream.push_back(payload.to_vec());
        true
    }
}

/// A read-only summary of one open flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpFlowInfo {
    pub key: FlowKey,
    /// Where the client sent the datagram.
    pub requested: SocketAddr,
    /// Where the kernel sends it.
    pub target: SocketAddr,
    pub steered: bool,
    pub reason: &'static str,
    pub dns: bool,
    pub queued_upstream: usize,
    pub age: Duration,
    pub idle: Duration,
}

/// The UDP half of the kernel.
pub struct UdpRelay {
    tuning: Tuning,
    flows: HashMap<FlowKey, UdpFlow>,
    /// Maps an upstream descriptor back to its flow, so poll events are O(1).
    fd_index: HashMap<RawFd, FlowKey>,
    /// Reused read buffer, so a service pass does not allocate 64 KiB each time.
    read_buf: Vec<u8>,
}

impl std::fmt::Debug for UdpRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpRelay")
            .field("flows", &self.flows.len())
            .finish_non_exhaustive()
    }
}

impl UdpRelay {
    /// Build the relay from the engine configuration.
    pub fn new(config: &StackConfig) -> Self {
        let tuning = Tuning {
            mtu: config.mtu.max(576),
            max_flows: config.max_udp_flows.max(1),
            idle_timeout: config.udp_idle_timeout,
            buffer_limit: config.flow_buffer_limit.max(2048),
            answer_dns: config.answer_dns_from_rules,
            dns_ttl: config.dns_answer_ttl.as_secs().clamp(1, 3600) as u32,
            observe_dns: config.observe_dns,
        };
        Self {
            tuning,
            flows: HashMap::new(),
            fd_index: HashMap::new(),
            read_buf: vec![0u8; MAX_DATAGRAM],
        }
    }

    /// Number of conversations currently open.
    pub fn active_flows(&self) -> usize {
        self.flows.len()
    }

    /// A read-only view of the open flows, for logs and the status screen.
    pub fn snapshot(&self, now: Instant) -> Vec<UdpFlowInfo> {
        self.flows
            .values()
            .map(|flow| UdpFlowInfo {
                key: flow.key,
                requested: flow.requested,
                target: flow.target,
                steered: flow.steered,
                reason: flow.reason,
                dns: flow.dns,
                queued_upstream: flow.queued_upstream,
                age: now.saturating_duration_since(flow.opened_at),
                idle: now.saturating_duration_since(flow.last_activity),
            })
            .collect()
    }

    /// True when `fd` belongs to this relay.
    pub fn owns_fd(&self, fd: RawFd) -> bool {
        self.fd_index.contains_key(&fd)
    }

    /// Largest payload the client can be handed in one packet.
    ///
    /// Anything larger would produce a packet past the tunnel MTU, which the
    /// kernel refuses to write. IPv4 gets 28 bytes of header, IPv6 48; the
    /// smaller of the two is used so the answer does not depend on the family.
    fn client_payload_limit(&self) -> usize {
        self.tuning.mtu.saturating_sub(48)
    }

    /// Hand a packet read from the TUN to the relay.
    ///
    /// The packet is parsed here rather than by the caller, so the relay is usable
    /// on its own and the engine's dispatch needs no special case for UDP.
    ///
    /// Returns `false` when the packet cannot be relayed at all — an IP fragment,
    /// or something that is not a well formed UDP datagram — so the caller can
    /// count it as ignored rather than silently dropped.
    pub fn handle(
        &mut self,
        packet: &[u8],
        now: Instant,
        out: &mut Vec<Vec<u8>>,
        planner: &mut Planner,
        protector: &mut dyn Protector,
        stats: &mut Stats,
    ) -> bool {
        let Ok(header) = packet::parse_ip_header(packet) else {
            return false;
        };
        // Only the first fragment carries the transport header, and the rest
        // cannot be reassembled here, so a fragmented datagram is not relayable.
        if header.is_later_fragment(packet) {
            return false;
        }
        let Ok(datagram) = packet::parse_udp(packet, &header) else {
            return false;
        };

        let requested = SocketAddr::new(header.dst, datagram.dst_port);
        let key = FlowKey::new(
            PROTO_UDP,
            header.src,
            header.dst,
            datagram.src_port,
            datagram.dst_port,
        );

        // An established conversation just takes the datagram.
        if let Some(flow) = self.flows.get_mut(&key) {
            if flow.queue_upstream(datagram.payload, self.tuning.buffer_limit) {
                flow.last_activity = now;
                stats.bytes_client_to_upstream += datagram.payload.len() as u64;
                stats.udp_datagrams_reused += 1;
            } else {
                stats.udp_datagrams_dropped += 1;
            }
            return true;
        }

        let is_dns = datagram.dst_port == DNS_PORT;
        if is_dns {
            stats.dns_queries += 1;
            if self.tuning.answer_dns
                && self.answer_dns_locally(
                    datagram.payload,
                    datagram.dst_port,
                    datagram.src_port,
                    &header,
                    now,
                    out,
                    planner,
                    stats,
                )
            {
                return true;
            }
        }

        let decision = planner.decide(now, requested);
        if !planner.can_relay(&decision) {
            // A destination the kernel refuses to relay is a deliberate answer,
            // and saying so at once beats letting the client wait for a reply that
            // is never coming.
            stats.udp_flows_rejected += 1;
            emit_icmp_unreachable(out, &header, packet);
            return true;
        }

        // A full table reclaims what is finished rather than refusing what is new.
        // The failure path is deliberately silent: the condition is transient, and
        // an ICMP port unreachable would tell the client the service is down — a
        // lie it may act on permanently.
        if !self.make_room(now, stats) {
            stats.udp_flows_rejected += 1;
            return true;
        }

        let family = Family::of(decision.target.ip());
        let mut socket = match UpstreamSocket::udp(family, protector) {
            Ok(socket) => socket,
            Err(_) => {
                stats.udp_flows_rejected += 1;
                emit_icmp_unreachable(out, &header, packet);
                return true;
            }
        };
        // For UDP, `connect` only records the peer locally: there is no handshake,
        // so a success here means the socket is ready to send.
        if socket.start_connect(decision.target).is_err() {
            stats.udp_flows_rejected += 1;
            emit_icmp_unreachable(out, &header, packet);
            return true;
        }

        let mut flow = UdpFlow {
            key,
            requested,
            target: decision.target,
            socket,
            steered: decision.is_steered(),
            reason: decision.reason(),
            dns: is_dns,
            to_upstream: VecDeque::new(),
            queued_upstream: 0,
            opened_at: now,
            last_activity: now,
        };
        if flow.queue_upstream(datagram.payload, self.tuning.buffer_limit) {
            stats.bytes_client_to_upstream += datagram.payload.len() as u64;
        } else {
            stats.udp_datagrams_dropped += 1;
        }

        self.fd_index.insert(flow.socket.raw_fd(), key);
        self.flows.insert(key, flow);

        stats.udp_flows_opened += 1;
        if is_dns {
            stats.dns_forwarded += 1;
        }
        if decision.is_steered() {
            stats.flows_matched_rules += 1;
        } else {
            stats.flows_direct += 1;
        }
        true
    }

    /// Answer a DNS query from the rule set, when it can be answered at all.
    ///
    /// Returns `true` when a reply was emitted, in which case no upstream flow is
    /// created and the query never leaves the device.
    #[allow(clippy::too_many_arguments)]
    fn answer_dns_locally(
        &mut self,
        payload: &[u8],
        dst_port: u16,
        src_port: u16,
        header: &IpHeader,
        now: Instant,
        out: &mut Vec<Vec<u8>>,
        planner: &mut Planner,
        stats: &mut Stats,
    ) -> bool {
        let Ok(query) = dns::Message::parse(payload) else {
            stats.dns_unparsable += 1;
            return false;
        };
        // Only plain queries are answered. Anything else — a response that leaked
        // into the tunnel, an update, a notify — is forwarded untouched.
        if !query.is_query() || query.opcode() != 0 {
            return false;
        }
        let Some(question) = query.first_question() else {
            return false;
        };
        if question.qclass != dns::CLASS_IN {
            return false;
        }
        // Only address questions can be answered from a rule's `ips` list. A
        // query for a CNAME or an HTTPS record has to reach a real resolver.
        let family = match question.qtype {
            dns::TYPE_A => Family::V4,
            dns::TYPE_AAAA => Family::V6,
            _ => return false,
        };

        let plan = planner.router().plan(now, &question.name, family);

        // A rule matched the domain but has nothing in this address family.
        //
        // Answer NODATA rather than forwarding, and this is the difference
        // between a rule working and a rule being bypassed. Measured on
        // `github.com`: the rule carries thirty-nine IPv4 addresses and no IPv6,
        // so the A query was answered locally and the AAAA query was forwarded —
        // and the client, holding a real IPv6 address, connected over IPv6, where
        // no rule applies, straight to an unreachable address. The tunnel looked
        // like it was failing to relay when in fact it was never asked to.
        //
        // NODATA is also what a dual-stack-correct resolver returns for a name
        // with no records of that type, so the client does the right thing by
        // itself: it falls back to the family the rule does cover.
        if plan.strategy == Strategy::FamilyFallback {
            let limit = self.client_payload_limit();
            let Some((response, _)) =
                dns::build_response_fitting(&query, &question.name, &[], self.tuning.dns_ttl, limit)
            else {
                stats.udp_datagrams_dropped += 1;
                return false;
            };
            stats.dns_answered_locally += 1;
            emit_udp(out, header.dst, header.src, dst_port, src_port, &response);
            return true;
        }

        // A placeholder entry has nothing concrete to hand out. Inventing an
        // answer would be worse than letting a real resolver answer, so the query
        // is forwarded.
        if plan.strategy != Strategy::RuleAddresses || plan.addresses.is_empty() {
            return false;
        }
        let addresses = plan.addresses;

        // Trim the answer to what one datagram can carry, rather than truncating
        // it. A truncation flag asks the client to retry over TCP, and there is
        // no TCP retry to make here: this answer never left the device. For a
        // rule with hundreds of addresses, truncating would turn a usable answer
        // into an empty one, which is the worst possible outcome for the client.
        let limit = self.client_payload_limit();
        let Some((response, delivered)) = dns::build_response_fitting(
            &query,
            &question.name,
            &addresses,
            self.tuning.dns_ttl,
            limit,
        ) else {
            stats.udp_datagrams_dropped += 1;
            return false;
        };

        // Record the mapping before the client can act on it. Every address is
        // observed, not only the delivered ones: a client that learned the rest
        // some other way — over HTTPS, or before this rule loaded — is still
        // recognised by address when it connects.
        for address in &addresses {
            planner.observe(
                *address,
                &question.name,
                Duration::from_secs(u64::from(self.tuning.dns_ttl)),
                now,
            );
        }

        stats.dns_answered_locally += 1;
        stats.dns_addresses_observed += addresses.len() as u64;
        if delivered < addresses.len() {
            stats.dns_answers_trimmed += 1;
        }

        emit_udp(out, header.dst, header.src, dst_port, src_port, &response);
        true
    }

    /// Register every upstream descriptor with the poll loop.
    pub fn register(&self, poller: &mut Poller) {
        for flow in self.flows.values() {
            // A backlogged flow wants to know the moment the socket drains. The
            // read side is always of interest: replies are the reason the loop
            // exists.
            let interests = if flow.queued_upstream > 0 {
                INTEREST_READ | INTEREST_WRITE
            } else {
                INTEREST_READ
            };
            poller.add(flow.socket.raw_fd(), flow.socket.raw_fd() as u64, interests);
        }
    }

    /// Flush pending datagrams and collect replies.
    ///
    /// Unlike the TCP relay there is no `note_writable` handshake: a UDP socket
    /// has no connection to confirm, so a writable descriptor only ever means
    /// "the send buffer has room again", and retrying the flush on every pass is
    /// both correct and simpler.
    pub fn service(
        &mut self,
        now: Instant,
        out: &mut Vec<Vec<u8>>,
        planner: &mut Planner,
        stats: &mut Stats,
    ) {
        let mut read_buf = std::mem::take(&mut self.read_buf);
        let keys: Vec<FlowKey> = self.flows.keys().copied().collect();
        let mut dead: Vec<FlowKey> = Vec::new();
        let limit = self.client_payload_limit();

        for key in keys {
            // --- flush what the client sent --------------------------------
            let mut send_failed = false;
            if let Some(flow) = self.flows.get_mut(&key) {
                while let Some(datagram) = flow.to_upstream.front() {
                    match flow.socket.send(datagram) {
                        Ok(n) if n == datagram.len() => {
                            flow.queued_upstream = flow.queued_upstream.saturating_sub(n);
                            flow.to_upstream.pop_front();
                            flow.last_activity = now;
                        }
                        // A datagram socket never accepts a partial write; if the
                        // kernel says otherwise the flow is in an unknown state
                        // and is better off abandoned than half written.
                        Ok(_) => {
                            send_failed = true;
                            break;
                        }
                        Err(err) if is_retryable(&err) => break,
                        Err(_) => {
                            send_failed = true;
                            break;
                        }
                    }
                }
            }
            if send_failed {
                dead.push(key);
                continue;
            }

            // --- collect replies -------------------------------------------
            let Some((flow_key, is_dns)) = self.flows.get(&key).map(|f| (f.key, f.dns)) else {
                continue;
            };

            for _ in 0..DRAIN_BATCH {
                let Some(flow) = self.flows.get_mut(&key) else {
                    break;
                };
                match flow.socket.read(&mut read_buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        flow.last_activity = now;
                        stats.bytes_upstream_to_client += n as u64;
                        if is_dns && self.tuning.observe_dns {
                            observe_dns_answer(&read_buf[..n], now, planner, stats);
                        }
                        match fit_payload(&read_buf[..n], limit) {
                            Some(fitted) => emit_udp(
                                out,
                                flow_key.dst,
                                flow_key.src,
                                flow_key.dst_port,
                                flow_key.src_port,
                                &fitted,
                            ),
                            None => stats.udp_datagrams_dropped += 1,
                        }
                    }
                    Err(err) if is_retryable(&err) => break,
                    // An ICMP port unreachable for a connected socket surfaces
                    // here. The destination is genuinely gone, so the flow is
                    // finished rather than retried.
                    Err(err) if err.kind() == io::ErrorKind::ConnectionRefused => {
                        dead.push(key);
                        break;
                    }
                    Err(_) => break,
                }
            }
        }

        self.read_buf = read_buf;

        for key in dead {
            self.forget(key, stats);
        }

        self.reap(now, stats);
    }

    /// Make sure there is room for one more flow, forgetting the least recently
    /// used flow when the table is full.
    ///
    /// Returns `false` when the table is full and every flow is still in use. The
    /// idle guard is what keeps a busy table from thrashing: without it, a client
    /// with more live conversations than the table can hold would have them evicted
    /// and rebuilt on every packet, which is worse than refusing the excess.
    fn make_room(&mut self, now: Instant, stats: &mut Stats) -> bool {
        if self.flows.len() < self.tuning.max_flows {
            return true;
        }

        let guard = self.tuning.idle_timeout / 4;
        let victim = self
            .flows
            .iter()
            .filter(|(_, flow)| now.saturating_duration_since(flow.last_activity) >= guard)
            .min_by_key(|(_, flow)| flow.last_activity)
            .map(|(key, _)| *key);

        let Some(key) = victim else {
            return false;
        };
        self.forget(key, stats);
        stats.udp_flows_evicted += 1;
        true
    }

    /// Drop flows that have been quiet for longer than the idle timeout.
    fn reap(&mut self, now: Instant, stats: &mut Stats) {
        let timeout = self.tuning.idle_timeout;
        let stale: Vec<FlowKey> = self
            .flows
            .iter()
            .filter(|(_, flow)| now.saturating_duration_since(flow.last_activity) > timeout)
            .map(|(key, _)| *key)
            .collect();

        for key in stale {
            self.forget(key, stats);
        }
    }

    /// Remove one flow and its descriptor registration.
    fn forget(&mut self, key: FlowKey, stats: &mut Stats) {
        if let Some(flow) = self.flows.remove(&key) {
            self.fd_index.remove(&flow.socket.raw_fd());
            stats.udp_flows_closed += 1;
        }
    }

    /// Drop every flow, used when the tunnel is torn down.
    pub fn reset(&mut self) {
        self.flows.clear();
        self.fd_index.clear();
    }
}

/// Build and queue a UDP packet from `src`:`src_port` to `dst`:`dst_port`.
fn emit_udp(
    out: &mut Vec<Vec<u8>>,
    src: IpAddr,
    dst: IpAddr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) {
    if let Ok(bytes) = packet::build_udp_packet(src, dst, src_port, dst_port, payload) {
        out.push(bytes);
    }
}

/// Answer a datagram nobody could handle, so the client fails fast.
fn emit_icmp_unreachable(out: &mut Vec<Vec<u8>>, header: &IpHeader, original: &[u8]) {
    // The ICMP message travels from the address the client dialled back to the
    // client, and carries the packet that provoked it.
    if let Ok(bytes) = packet::build_icmp_port_unreachable(header.dst, header.src, original) {
        out.push(bytes);
    }
}

/// Record the addresses a forwarded DNS answer carried.
fn observe_dns_answer(payload: &[u8], now: Instant, planner: &mut Planner, stats: &mut Stats) {
    let Ok(message) = dns::Message::parse(payload) else {
        return;
    };
    if message.is_query() || message.rcode() != dns::RCODE_NOERROR {
        return;
    }
    let Some(question) = message.first_question() else {
        return;
    };

    let mut observed = 0u64;
    for record in &message.answers {
        let Some(address) = record.address() else {
            continue;
        };
        // The record's own TTL is the best estimate of how long the mapping is
        // valid. A zero TTL still gets clamped upward inside the cache, because
        // an observation that expires before the connection it describes is
        // useless.
        planner.observe(
            address,
            &question.name,
            Duration::from_secs(u64::from(record.ttl)),
            now,
        );
        observed += 1;
    }
    stats.dns_addresses_observed += observed;
}

/// Return `payload` if it fits, else a truncated DNS version of it.
///
/// A DNS message that does not fit is answered with the truncation bit set and no
/// records, which tells the client to retry over TCP — and TCP is relayed too, so
/// the answer still arrives. Anything else that does not fit has no fallback and
/// is dropped.
fn fit_payload(payload: &[u8], limit: usize) -> Option<Vec<u8>> {
    if payload.len() <= limit {
        return Some(payload.to_vec());
    }
    let mut message = dns::Message::parse(payload).ok()?;
    if message.is_query() {
        return None;
    }
    message.flags |= dns::FLAG_TC;
    message.answers.clear();
    message.authorities.clear();
    // The OPT record carries the server's advertised receive size, which the
    // client needs in order to retry sensibly.
    message
        .additionals
        .retain(|record| record.rtype == dns::TYPE_OPT);

    let encoded = message.encode().ok()?;
    if encoded.len() <= limit {
        Some(encoded)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DestinationOverride;
    use crate::upstream::NoProtector;
    use std::net::{Ipv4Addr, UdpSocket};
    use watt_rules::{Router, RuleSet, RuleSource};

    const DOC: &str = r#"{
      "meta": { "version": "t", "update_time": "t" },
      "groups": [
        { "group": "g", "entries": [
          { "id": "1", "name": "CDN", "domains": ["cdn.example"], "ips": ["203.0.113.10", "203.0.113.20"], "port": "443", "isPlaceholder": false },
          { "id": "2", "name": "Placeholder", "domains": ["ph.example"], "ips": ["{Cloudflare}"], "port": "443", "isPlaceholder": true }
        ] }
      ]
    }"#;

    fn v4(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, last))
    }

    /// Drives the relay the way the engine does, without a TUN device.
    struct Harness {
        relay: UdpRelay,
        planner: Planner,
        protector: NoProtector,
        stats: Stats,
        now: Instant,
        client_ip: IpAddr,
        client_port: u16,
    }

    impl Harness {
        fn new(overrides: Vec<DestinationOverride>) -> Self {
            Self::with_config(overrides, |_| {})
        }

        /// Build a harness on a configuration the test adjusts before use.
        fn with_config(
            overrides: Vec<DestinationOverride>,
            adjust: impl FnOnce(&mut StackConfig),
        ) -> Self {
            let mut config = StackConfig {
                overrides,
                ..StackConfig::default()
            };
            adjust(&mut config);
            let rules = RuleSet::from_str(DOC, RuleSource::Provided).unwrap();
            Self {
                relay: UdpRelay::new(&config),
                planner: Planner::new(Router::new(rules), &config),
                protector: NoProtector,
                stats: Stats::default(),
                now: Instant::now(),
                client_ip: "10.0.0.2".parse().unwrap(),
                client_port: 40000,
            }
        }

        fn tick(&mut self, millis: u64) {
            self.now += Duration::from_millis(millis);
        }

        /// Send one client datagram and return everything addressed back.
        fn send(&mut self, dst: IpAddr, dst_port: u16, payload: &[u8]) -> Vec<Vec<u8>> {
            self.send_from(self.client_port, dst, dst_port, payload)
        }

        /// Send one client datagram from a chosen source port.
        ///
        /// Distinct source ports are distinct flows, which is the only way to
        /// exercise a table that has a ceiling.
        fn send_from(
            &mut self,
            src_port: u16,
            dst: IpAddr,
            dst_port: u16,
            payload: &[u8],
        ) -> Vec<Vec<u8>> {
            let packet =
                packet::build_udp_packet(self.client_ip, dst, src_port, dst_port, payload).unwrap();
            let mut out = Vec::new();
            let now = self.now;
            let consumed = self.relay.handle(
                &packet,
                now,
                &mut out,
                &mut self.planner,
                &mut self.protector,
                &mut self.stats,
            );
            assert!(consumed, "a well formed UDP packet must be handled");
            self.service(&mut out);
            out
        }

        fn service(&mut self, out: &mut Vec<Vec<u8>>) {
            let now = self.now;
            self.relay
                .service(now, out, &mut self.planner, &mut self.stats);
        }

        fn poll(&mut self) -> Vec<Vec<u8>> {
            let mut out = Vec::new();
            self.service(&mut out);
            out
        }
    }

    fn dns_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
        dns::Message {
            id,
            flags: dns::FLAG_RD,
            questions: vec![dns::Question {
                name: name.to_string(),
                qtype,
                qclass: dns::CLASS_IN,
            }],
            answers: Vec::new(),
            authorities: Vec::new(),
            additionals: Vec::new(),
        }
        .encode()
        .unwrap()
    }

    fn only_packet(packets: &[Vec<u8>]) -> IpHeader {
        assert_eq!(packets.len(), 1, "expected exactly one reply");
        packet::parse_ip_header(&packets[0]).unwrap()
    }

    fn only_datagram(packets: &[Vec<u8>]) -> (IpHeader, packet::UdpDatagram<'_>) {
        let header = only_packet(packets);
        let datagram = packet::parse_udp(&packets[0], &header)
            .expect("the reply must be a UDP datagram");
        (header, datagram)
    }

    #[test]
    fn relays_a_datagram_to_a_real_server_and_back() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dst = v4(50);

        let mut harness = Harness::new(vec![DestinationOverride::address(
            fake_dst,
            server_addr.ip(),
        )]);

        let out = harness.send(fake_dst, server_addr.port(), b"ping");
        assert!(out.is_empty(), "nothing to send back yet");

        let mut buf = [0u8; 64];
        let (n, from) = server.recv_from(&mut buf).expect("the server must be reached");
        assert_eq!(&buf[..n], b"ping");

        server.send_to(b"pong", from).unwrap();
        let out = harness.poll();

        let (header, datagram) = only_datagram(&out);
        // The reply must appear to come from the address the client dialled.
        assert_eq!(header.src, fake_dst);
        assert_eq!(header.dst, harness.client_ip);
        assert_eq!(datagram.src_port, server_addr.port());
        assert_eq!(datagram.dst_port, harness.client_port);
        assert_eq!(datagram.payload, b"pong");

        assert_eq!(harness.stats.bytes_client_to_upstream, 4);
        assert_eq!(harness.stats.bytes_upstream_to_client, 4);
        assert_eq!(harness.stats.udp_flows_opened, 1);
        // The rewrite steered the flow, so it counts as steered rather than
        // direct even though no rule was involved.
        assert_eq!(harness.stats.flows_matched_rules, 1);

        let info = harness.relay.snapshot(harness.now);
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].target.ip(), server_addr.ip());
        assert_eq!(info[0].requested.ip(), fake_dst);
        assert!(info[0].steered);
        assert_eq!(info[0].reason, "override");
    }

    #[test]
    fn a_second_datagram_reuses_the_same_flow() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dst = v4(51);

        let mut harness = Harness::new(vec![DestinationOverride::address(
            fake_dst,
            server_addr.ip(),
        )]);

        harness.send(fake_dst, server_addr.port(), b"one");
        harness.send(fake_dst, server_addr.port(), b"two");

        let mut buf = [0u8; 64];
        let (n, _) = server.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"one");
        let (n, _) = server.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"two");

        assert_eq!(harness.relay.active_flows(), 1, "one flow, two datagrams");
        assert_eq!(harness.stats.udp_flows_opened, 1);
        assert_eq!(
            harness.stats.udp_datagrams_reused, 1,
            "the second datagram has to be visible somewhere, or the accounting \
             cannot tell a reused flow from a lost datagram"
        );
    }

    #[test]
    fn a_rule_domain_is_answered_locally_and_needs_no_upstream() {
        let mut harness = Harness::new(Vec::new());
        let query = dns_query(0x1234, "cdn.example", dns::TYPE_A);

        let out = harness.send(v4(10), DNS_PORT, &query);
        let (header, datagram) = only_datagram(&out);

        assert_eq!(header.src, v4(10), "the answer comes from the address dialled");
        assert_eq!(datagram.src_port, DNS_PORT);
        assert_eq!(datagram.dst_port, harness.client_port);

        let response = dns::Message::parse(datagram.payload).unwrap();
        assert!(!response.is_query());
        assert_eq!(response.id, 0x1234);
        assert_eq!(response.rcode(), dns::RCODE_NOERROR);
        let mut addresses = response.answer_addresses();
        addresses.sort();
        assert_eq!(addresses, vec![v4(10), v4(20)]);

        assert_eq!(harness.relay.active_flows(), 0, "a local answer opens no flow");
        assert_eq!(harness.stats.dns_answered_locally, 1);
        assert_eq!(harness.stats.dns_forwarded, 0);
    }

    #[test]
    fn addresses_handed_out_by_the_local_answering_are_observable() {
        let mut harness = Harness::new(Vec::new());
        let query = dns_query(0x1234, "cdn.example", dns::TYPE_A);
        harness.send(v4(10), DNS_PORT, &query);

        // Without this the connection that follows would not be recognised, and
        // the rule's address list would never be applied to it.
        let now = harness.now;
        assert_eq!(harness.planner.domains_for(v4(20), now), vec!["cdn.example"]);
    }

    #[test]
    fn an_aaaa_query_for_a_v4_only_rule_is_forwarded() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dns = v4(53);

        // Port 53 on the fake address is rewritten to the test server's real port,
        // so the relay still sees a DNS query while the socket reaches loopback.
        let mut harness = Harness::new(vec![DestinationOverride::endpoint(
            fake_dns,
            DNS_PORT,
            server_addr.ip(),
            server_addr.port(),
        )]);

        let query = dns_query(0x5150, "cdn.example", dns::TYPE_AAAA);
        let out = harness.send(fake_dns, DNS_PORT, &query);
        assert!(out.is_empty(), "a forwarded query produces no immediate reply");

        let mut buf = [0u8; 512];
        let (n, _) = server
            .recv_from(&mut buf)
            .expect("the query must reach a real resolver");
        assert_eq!(&buf[..n], &query[..]);
        assert_eq!(harness.stats.dns_forwarded, 1);
        assert_eq!(harness.stats.dns_answered_locally, 0);
    }

    #[test]
    fn a_placeholder_only_rule_is_forwarded_rather_than_invented() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dns = v4(53);

        let mut harness = Harness::new(vec![DestinationOverride::endpoint(
            fake_dns,
            DNS_PORT,
            server_addr.ip(),
            server_addr.port(),
        )]);

        let query = dns_query(0x6001, "ph.example", dns::TYPE_A);
        harness.send(fake_dns, DNS_PORT, &query);

        let mut buf = [0u8; 512];
        server
            .recv_from(&mut buf)
            .expect("a placeholder rule must fall through to a real resolver");
        assert_eq!(harness.stats.dns_forwarded, 1);
        assert_eq!(harness.stats.dns_answered_locally, 0);
    }

    #[test]
    fn a_forwarded_answer_is_observed_so_the_next_connection_is_steered() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dns = v4(53);

        let mut harness = Harness::new(vec![DestinationOverride::endpoint(
            fake_dns,
            DNS_PORT,
            server_addr.ip(),
            server_addr.port(),
        )]);

        let query = dns_query(0x7001, "unknown.example", dns::TYPE_A);
        harness.send(fake_dns, DNS_PORT, &query);

        let mut buf = [0u8; 512];
        let (n, from) = server.recv_from(&mut buf).unwrap();
        let parsed = dns::Message::parse(&buf[..n]).unwrap();

        let resolved = v4(200);
        let response = dns::build_response(
            &parsed,
            dns::address_records("unknown.example", &[resolved], 300),
            dns::RCODE_NOERROR,
        )
        .unwrap();
        server.send_to(&response, from).unwrap();

        let out = harness.poll();
        let (_, datagram) = only_datagram(&out);
        assert_eq!(datagram.payload, &response[..], "the answer is relayed verbatim");

        let now = harness.now;
        assert_eq!(
            harness.planner.domains_for(resolved, now),
            vec!["unknown.example"],
            "the answer must be recorded for the data plane"
        );
        assert_eq!(harness.stats.dns_addresses_observed, 1);
    }

    #[test]
    fn a_blocked_destination_gets_an_icmp_port_unreachable() {
        let mut harness = Harness::new(Vec::new());
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();

        let out = harness.send(loopback, 9999, b"x");
        let header = only_packet(&out);
        assert_eq!(header.protocol, packet::PROTO_ICMP);
        assert_eq!(header.src, loopback, "the refusal comes from the address dialled");
        assert_eq!(header.dst, harness.client_ip);

        assert_eq!(harness.stats.udp_flows_rejected, 1);
        assert_eq!(harness.relay.active_flows(), 0);
    }

    #[test]
    fn a_full_table_forgets_the_least_recently_used_flow_rather_than_refusing_a_new_one() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dst = v4(80);

        let mut harness = Harness::with_config(
            vec![DestinationOverride::address(fake_dst, server_addr.ip())],
            |config| config.max_udp_flows = 2,
        );

        // Two conversations fill the table. The first one is used again, which
        // makes the second the least recently used.
        harness.send_from(40001, fake_dst, server_addr.port(), b"first");
        harness.tick(1_000);
        harness.send_from(40002, fake_dst, server_addr.port(), b"second");
        harness.tick(1_000);
        harness.send_from(40001, fake_dst, server_addr.port(), b"first again");
        assert_eq!(harness.relay.active_flows(), 2);

        // Long enough that both count as finished for eviction, well short of the
        // idle timeout, so nothing has been reaped yet.
        harness.tick(20_000);
        let out = harness.send_from(40003, fake_dst, server_addr.port(), b"third");

        assert!(out.is_empty(), "a new conversation must not be refused");
        assert_eq!(harness.stats.udp_flows_rejected, 0);
        assert_eq!(harness.stats.udp_flows_evicted, 1);
        assert_eq!(harness.relay.active_flows(), 2, "the table stays at its ceiling");
        assert_eq!(
            harness.stats.udp_flows_closed, 1,
            "an evicted flow is closed exactly once"
        );
    }

    #[test]
    fn a_full_table_of_flows_all_in_use_refuses_rather_than_thrashing() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dst = v4(81);

        let mut harness = Harness::with_config(
            vec![DestinationOverride::address(fake_dst, server_addr.ip())],
            |config| config.max_udp_flows = 1,
        );

        harness.send_from(40001, fake_dst, server_addr.port(), b"first");
        assert_eq!(harness.relay.active_flows(), 1);

        // The only flow was touched this instant, so it is not a candidate for
        // eviction: rebuilding it on every packet would be worse than dropping the
        // excess.
        let out = harness.send_from(40002, fake_dst, server_addr.port(), b"second");

        assert!(out.is_empty(), "a dropped datagram is not answered");
        assert_eq!(harness.stats.udp_flows_rejected, 1);
        assert_eq!(harness.stats.udp_flows_evicted, 0);
        assert_eq!(harness.relay.active_flows(), 1, "the flow in use is left alone");
    }

    #[test]
    fn an_idle_flow_is_reaped() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dst = v4(60);

        let mut harness = Harness::new(vec![DestinationOverride::address(
            fake_dst,
            server_addr.ip(),
        )]);
        harness.send(fake_dst, server_addr.port(), b"hello");
        assert_eq!(harness.relay.active_flows(), 1);

        harness.tick(61_000);
        harness.poll();

        assert_eq!(harness.relay.active_flows(), 0);
        assert_eq!(harness.stats.udp_flows_closed, 1);
    }

    #[test]
    fn reset_clears_every_flow() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dst = v4(61);

        let mut harness = Harness::new(vec![DestinationOverride::address(
            fake_dst,
            server_addr.ip(),
        )]);
        harness.send(fake_dst, server_addr.port(), b"hello");
        assert_eq!(harness.relay.active_flows(), 1);
        let fd = harness
            .relay
            .flows
            .values()
            .next()
            .expect("a flow must exist")
            .socket
            .raw_fd();
        assert!(harness.relay.owns_fd(fd));

        harness.relay.reset();
        assert_eq!(harness.relay.active_flows(), 0);
        assert!(!harness.relay.owns_fd(fd), "the descriptor index must be cleared too");
    }

    #[test]
    fn an_ip_fragment_is_not_relayed() {
        let mut harness = Harness::new(Vec::new());
        let mut bytes =
            packet::build_udp_packet(harness.client_ip, v4(70), harness.client_port, 3478, b"x")
                .unwrap();
        // Set the more-fragments bit: the datagram is now a fragment and cannot be
        // reassembled by the relay.
        bytes[6] |= 0x20;
        let header = packet::parse_ip_header(&bytes).unwrap();
        assert!(header.is_later_fragment(&bytes));

        let mut out = Vec::new();
        let now = harness.now;
        let consumed = harness.relay.handle(
            &bytes,
            now,
            &mut out,
            &mut harness.planner,
            &mut harness.protector,
            &mut harness.stats,
        );
        assert!(!consumed, "a fragment must be reported as ignored");
        assert!(out.is_empty());
    }

    #[test]
    fn a_large_rule_answer_fits_because_the_name_is_written_once() {
        // 80 address records, each repeating the question's name, is exactly
        // what used to push an answer past the MTU. Written once and pointed at
        // thereafter, it fits — and rules really do list this many.
        let addresses: Vec<IpAddr> = (1..=80u8).map(v4).collect();
        let query = dns::Message::parse(&dns_query(0x8001, "big.example", dns::TYPE_A)).unwrap();
        let response = dns::build_response(
            &query,
            dns::address_records("big.example", &addresses, 60),
            dns::RCODE_NOERROR,
        )
        .unwrap();
        assert!(
            response.len() <= 1452,
            "compression must keep a large answer inside one datagram, got {}",
            response.len()
        );

        let parsed = dns::Message::parse(&response).unwrap();
        assert_eq!(parsed.answer_addresses().len(), 80);
        assert!(parsed.flags & dns::FLAG_TC == 0);
    }

    #[test]
    fn an_answer_too_large_to_carry_whole_is_trimmed_not_truncated() {
        // Truncation asks the client to retry over TCP. An answer built from the
        // rule set never left the device, so there is nothing to retry against,
        // and a rule with hundreds of addresses would become an answer with
        // none. Real entries go up to 971 addresses.
        let addresses: Vec<IpAddr> = (0..200u16)
            .map(|i| IpAddr::V4(Ipv4Addr::new(198, 51, 100, (i % 256) as u8)))
            .collect();
        let query = dns::Message::parse(&dns_query(0x8002, "huge.example", dns::TYPE_A)).unwrap();
        let (response, delivered) =
            dns::build_response_fitting(&query, "huge.example", &addresses, 60, 1452)
                .expect("an oversized answer must still produce one");

        assert!(delivered > 0, "trimming must never empty the answer");
        assert!(delivered < addresses.len(), "the answer did not fit whole");
        assert!(response.len() <= 1452);

        let parsed = dns::Message::parse(&response).unwrap();
        assert!(parsed.flags & dns::FLAG_TC == 0, "a trimmed answer is complete");
        assert_eq!(parsed.answer_addresses().len(), delivered);
    }

    #[test]
    fn an_oversized_upstream_dns_reply_is_truncated_for_the_client() {
        // The opposite case: this reply came back from a real resolver, so the
        // client can retry it over TCP and TC is the right thing to say.
        let addresses: Vec<IpAddr> = (0..200u16)
            .map(|i| IpAddr::V4(Ipv4Addr::new(198, 51, 100, (i % 256) as u8)))
            .collect();
        let query =
            dns::Message::parse(&dns_query(0x8003, "upstream.example", dns::TYPE_A)).unwrap();
        let response = dns::build_response(
            &query,
            dns::address_records("upstream.example", &addresses, 60),
            dns::RCODE_NOERROR,
        )
        .unwrap();
        assert!(response.len() > 1452);

        let fitted = fit_payload(&response, 1452).expect("a DNS answer must be truncatable");
        assert!(fitted.len() <= 1452);

        let parsed = dns::Message::parse(&fitted).unwrap();
        assert!(parsed.flags & dns::FLAG_TC != 0, "the truncation bit must be set");
        assert!(parsed.answers.is_empty());
        assert_eq!(parsed.first_question().unwrap().name, "upstream.example");
    }

    #[test]
    fn an_oversized_non_dns_datagram_is_dropped() {
        let payload = vec![0u8; 4096];
        assert!(fit_payload(&payload, 1452).is_none());
    }

    #[test]
    fn a_datagram_that_fits_is_passed_through_untouched() {
        let payload = b"small".to_vec();
        assert_eq!(fit_payload(&payload, 1452).unwrap(), payload);
    }
}

//! The kernel's main loop.
//!
//! One iteration is: wait for something to become ready, take in whatever the
//! tunnel has, let both relays move data, write the resulting packets back, and
//! do the timer work. Everything else in this crate is a component that the loop
//! drives.
//!
//! ```text
//!   TUN fd ──read──▶ dispatch ──┬──▶ TcpRelay (userspace stack) ──▶ upstream socket
//!                               └──▶ UdpRelay (NAT table)        ──▶ upstream socket
//!
//!   TUN fd ◀─write── flush ◀────┬─── TcpRelay  ◀── upstream socket
//!                               └─── UdpRelay  ◀── upstream socket
//! ```
//!
//! # Waiting
//!
//! The loop waits on the tunnel descriptor and every upstream descriptor at once,
//! with a timeout that doubles as the timer tick. A timeout is not a fallback for
//! a missing wakeup: smoltcp's retransmission deadlines and the idle flow sweep
//! only advance when the loop runs, so the tick is load bearing.
//!
//! # What the engine does not do
//!
//! It does not configure the interface. Assigning the address, setting the MTU
//! and installing a default route are the caller's job — on Android that happens
//! inside `VpnService.Builder`, on a host it is `ip addr` and `ip route`. Keeping
//! it out of the library means the kernel never has to shell out, and it is the
//! one part of the setup that differs between the two.

use std::io;
use std::net::IpAddr;
use std::os::unix::io::RawFd;
use std::time::{Duration, Instant};

use watt_net::{PacketDevice, TunConfig, TunDevice};
use watt_rules::{IpStat, Router, RuleSet};

use crate::config::{StackConfig, Stats};
use crate::packet::{self, PROTO_ICMP, PROTO_ICMPV6, PROTO_TCP, PROTO_UDP};
use crate::planner::Planner;
use crate::poller::{PollEvent, Poller, INTEREST_READ};
use crate::tcp::{TcpFlowInfo, TcpRelay};
use crate::udp::{UdpFlowInfo, UdpRelay};
use crate::upstream::{NoProtector, Protector};

/// Poll key reserved for the tunnel descriptor.
///
/// Upstream descriptors are keyed by their own descriptor number, so this value
/// can never collide with one in practice. Events carrying it are the "there is
/// a packet to read" signal and are not forwarded to the relays.
const DEVICE_KEY: u64 = 0;

/// Packets read from the tunnel in one iteration.
///
/// A saturating client can otherwise keep the read loop busy indefinitely and
/// starve the poll that drives retransmits, so the loop takes a bounded bite and
/// lets the next iteration finish the queue.
const MAX_PACKETS_PER_STEP: usize = 256;

/// A view of everything the kernel is currently relaying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowSnapshot {
    pub tcp: Vec<TcpFlowInfo>,
    pub udp: Vec<UdpFlowInfo>,
    /// Spare listening sockets held for future connections.
    pub tcp_listeners: usize,
}

impl FlowSnapshot {
    /// Total flows in the snapshot.
    pub fn len(&self) -> usize {
        self.tcp.len() + self.udp.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tcp.is_empty() && self.udp.is_empty()
    }
}

/// The full-traffic kernel.
pub struct Engine<D: PacketDevice> {
    device: D,
    planner: Planner,
    tcp: TcpRelay,
    udp: UdpRelay,
    poller: Poller,
    protector: Box<dyn Protector>,
    stats: Stats,
    poll_timeout: Duration,
    prune_interval: Duration,
    last_prune: Instant,
    read_buf: Vec<u8>,
    pending: Vec<Vec<u8>>,
}

impl<D: PacketDevice> std::fmt::Debug for Engine<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("device", &self.device.name())
            .field("tcp_flows", &self.tcp.active_flows())
            .field("udp_flows", &self.udp.active_flows())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl<D: PacketDevice> Engine<D> {
    /// Build an engine on an already constructed device.
    pub fn new(
        config: StackConfig,
        router: Router,
        device: D,
        protector: Box<dyn Protector>,
    ) -> Self {
        let epoch = Instant::now();
        // smoltcp derives initial sequence numbers and ephemeral ports from this.
        // Two kernels starting in the same millisecond would otherwise pick the
        // same ones, which matters once a flow is re-established.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);

        Self {
            planner: Planner::new(router, &config),
            tcp: TcpRelay::new(&config, epoch, seed),
            udp: UdpRelay::new(&config),
            poller: Poller::new(),
            device,
            protector,
            stats: Stats::default(),
            poll_timeout: config.poll_timeout,
            prune_interval: config.prune_interval,
            last_prune: epoch,
            read_buf: vec![0u8; config.mtu.max(576) + 128],
            pending: Vec::new(),
        }
    }

    /// The device the engine is reading from.
    pub fn device(&self) -> &D {
        &self.device
    }

    /// The device the engine is reading from, mutably.
    pub fn device_mut(&mut self) -> &mut D {
        &mut self.device
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn planner(&self) -> &Planner {
        &self.planner
    }

    pub fn planner_mut(&mut self) -> &mut Planner {
        &mut self.planner
    }

    pub fn protector_mut(&mut self) -> &mut dyn Protector {
        self.protector.as_mut()
    }

    /// Everything currently being relayed.
    pub fn flows(&self, now: Instant) -> FlowSnapshot {
        FlowSnapshot {
            tcp: self.tcp.snapshot(now),
            udp: self.udp.snapshot(now),
            tcp_listeners: self.tcp.pending_listeners(),
        }
    }

    /// Per-address byte history from the selector. See `IpSelector::snapshot`.
    ///
    /// Read-only and separate from `flows`: this is the long-lived judgement the
    /// kernel has built up about each address, which is what the shell wants to
    /// show *next to* a rule entry — the live connections say nothing about an
    /// address that has been tried and rejected before this session's traffic.
    pub fn ip_stats(&self) -> Vec<(IpAddr, IpStat)> {
        self.planner.router().selector().snapshot()
    }

    /// Per-(host, address) certificate verdicts. See `Verdicts::snapshot`.
    ///
    /// The counterpart to [`Self::ip_stats`] for the other kind of judgement the
    /// kernel makes: whether an address actually serves the host it was dialled
    /// for. Expired conclusions are omitted, so this never disagrees with the
    /// dial path about what is still known.
    pub fn certificate_verdicts(&self) -> Vec<(String, IpAddr, &'static str, Duration)> {
        self.tcp.verdicts().snapshot()
    }

    /// Swap in a freshly downloaded rule set.
    ///
    /// Open flows are untouched: they already hold the addresses they were
    /// planned with, and killing a working connection to apply a rule update
    /// would be worse than letting it finish. Address history is carried over, so
    /// the new rules start with what the kernel already learned about the network.
    pub fn replace_rules(&mut self, rules: RuleSet) {
        self.planner.replace_rules(rules);
    }

    /// Re-resolve the rules' dial names, caching the addresses in the planner.
    ///
    /// The caller drives this on a refresh tick rather than the data path calling
    /// it, because resolving blocks and [`Self::step`] must not. Returns how many
    /// entries now have at least one address from a name.
    pub fn resolve_dial_names<F>(&mut self, resolve: F) -> usize
    where
        F: FnMut(&str) -> Vec<std::net::IpAddr>,
    {
        self.planner.resolve_dial_names(resolve)
    }

    /// Drop every flow and listener, for a clean restart.
    pub fn reset(&mut self) {
        self.tcp.reset();
        self.udp.reset();
        self.pending.clear();
    }

    /// Run one iteration.
    ///
    /// Returns the number of packets read from the tunnel. `timeout` bounds the
    /// wait for readiness; pass [`Duration::ZERO`] to poll without blocking, which
    /// is what a device without a descriptor requires.
    pub fn step(&mut self, timeout: Duration) -> io::Result<usize> {
        // --- 1. Wait for something to happen. ----------------------------
        self.register();
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let events: Vec<PollEvent> = self.poller.wait(timeout_ms)?.to_vec();
        let now = Instant::now();
        for event in &events {
            if event.key == DEVICE_KEY {
                continue;
            }
            // Writable, errored or hung up: all three mean a connect attempt
            // finished and `SO_ERROR` now holds the verdict.
            if event.writable || event.error || event.hangup {
                self.tcp.note_ready(event.key as RawFd);
            }
        }

        // --- 2. Take in whatever the tunnel has. -------------------------
        let taken = self.drain(now)?;

        // --- 3. Move data, then write the results back. ------------------
        self.pump(now)?;

        // --- 4. Housekeeping. --------------------------------------------
        if now.saturating_duration_since(self.last_prune) >= self.prune_interval {
            self.planner.prune(now);
            self.last_prune = now;
        }

        Ok(taken)
    }

    /// Run until `deadline`.
    pub fn run_until(&mut self, deadline: Instant) -> io::Result<()> {
        while Instant::now() < deadline {
            self.step(self.poll_timeout)?;
        }
        Ok(())
    }

    /// Run until the process is stopped.
    pub fn run(&mut self) -> io::Result<()> {
        loop {
            self.step(self.poll_timeout)?;
        }
    }

    /// Register the tunnel and every upstream descriptor for the next wait.
    fn register(&mut self) {
        self.poller.clear();
        if let Some(fd) = self.device.raw_fd() {
            self.poller.add(fd, DEVICE_KEY, INTEREST_READ);
        }
        self.tcp.register(&mut self.poller);
        self.udp.register(&mut self.poller);
    }

    /// Read packets from the tunnel until it runs dry or the batch is full.
    fn drain(&mut self, now: Instant) -> io::Result<usize> {
        let mut buf = std::mem::take(&mut self.read_buf);
        let mut taken = 0usize;
        let mut failure: Option<io::Error> = None;

        loop {
            match self.device.read_packet(&mut buf) {
                Ok(Some(len)) => {
                    self.stats.packets_in += 1;
                    taken += 1;
                    self.dispatch(&buf[..len], now);
                    if taken >= MAX_PACKETS_PER_STEP {
                        break;
                    }
                }
                Ok(None) => break,
                // A signal arriving mid-read is not a failure.
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
        }

        self.read_buf = buf;
        match failure {
            Some(err) => Err(err),
            None => Ok(taken),
        }
    }

    /// Route one packet from the tunnel to the relay that owns it.
    fn dispatch(&mut self, packet: &[u8], now: Instant) {
        let Ok(header) = packet::parse_ip_header(packet) else {
            self.stats.packets_unparsable += 1;
            return;
        };

        match header.protocol {
            PROTO_TCP => self.tcp.feed(packet, now, &mut self.planner, &mut self.stats),
            PROTO_UDP => {
                let handled = self.udp.handle(
                    packet,
                    now,
                    &mut self.pending,
                    &mut self.planner,
                    self.protector.as_mut(),
                    &mut self.stats,
                );
                if !handled {
                    self.stats.packets_ignored += 1;
                }
            }
            // ICMP is deliberately dropped. The kernel is a relay, not a router:
            // it has no route table to forward along, and answering for addresses
            // it does not own would be a lie that breaks path MTU discovery in
            // confusing ways. A client that needs a reply gets one from the real
            // destination, because the flow it belongs to is relayed.
            PROTO_ICMP | PROTO_ICMPV6 => self.stats.packets_ignored += 1,
            _ => self.stats.packets_ignored += 1,
        }
    }

    /// Let both relays exchange data with their upstream sockets.
    fn pump(&mut self, now: Instant) -> io::Result<()> {
        let mut pending = std::mem::take(&mut self.pending);

        self.udp
            .service(now, &mut pending, &mut self.planner, &mut self.stats);
        self.tcp.service(
            now,
            &mut pending,
            &mut self.planner,
            self.protector.as_mut(),
            &mut self.stats,
        );

        self.pending = pending;
        self.flush()
    }

    /// Write everything the relays produced back into the tunnel.
    fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let packets = std::mem::take(&mut self.pending);
        let mut failure: Option<io::Error> = None;

        for packet in &packets {
            match self.device.write_packet(packet) {
                Ok(()) => self.stats.packets_out += 1,
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
        }

        // Reuse the allocation: the buffers are already free, only the outer
        // vector would otherwise be rebuilt every iteration.
        self.pending = packets;
        self.pending.clear();

        match failure {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

impl Engine<TunDevice> {
    /// Build an engine on a plain Linux TUN device.
    ///
    /// The interface still has to be configured by the caller: this only creates
    /// it and brings it up. Assigning the address and installing the default route
    /// are `ip addr` and `ip route` invocations, which is the one piece of setup
    /// that differs between a host and Android.
    pub fn open_tun(
        config: StackConfig,
        router: Router,
        protector: Box<dyn Protector>,
    ) -> io::Result<Self> {
        let tun_config = TunConfig {
            name: config.tun_name.clone(),
            mtu: config.mtu,
        };
        let device = TunDevice::create(&tun_config)?;
        device.bring_up()?;
        Ok(Self::new(config, router, device, protector))
    }

    /// Build an engine on a descriptor supplied by Android's `VpnService`.
    ///
    /// The descriptor is adopted, not owned: closing it is `VpnService`'s job
    /// when the tunnel is torn down, and closing it twice would take a descriptor
    /// belonging to something else.
    pub fn adopt_tun(
        fd: RawFd,
        name: impl Into<String>,
        config: StackConfig,
        router: Router,
        protector: Box<dyn Protector>,
    ) -> io::Result<Self> {
        let device = TunDevice::adopt(fd, name, config.mtu)?;
        Ok(Self::new(config, router, device, protector))
    }
}

/// Build an engine on a Linux TUN device that does not need descriptor
/// protection.
///
/// Only correct on a host, where the kernel's own sockets cannot be captured by
/// its own tunnel. On Android the descriptor must be protected instead, which is
/// why [`Engine::adopt_tun`] takes a [`Protector`].
pub fn open_tun_unprotected(
    config: StackConfig,
    router: Router,
) -> io::Result<Engine<TunDevice>> {
    Engine::open_tun(config, router, Box::new(NoProtector))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DestinationOverride;
    use crate::dns;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
    use watt_net::MemoryDevice;
    use watt_rules::RuleSource;

    const DOC: &str = r#"{
      "meta": { "version": "t", "update_time": "t" },
      "groups": [
        { "group": "g", "entries": [
          { "id": "1", "name": "CDN", "domains": ["cdn.example"], "ips": ["203.0.113.10", "203.0.113.20"], "port": "443", "isPlaceholder": false }
        ] }
      ]
    }"#;

    fn v4(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, last))
    }

    fn engine_with(config: StackConfig) -> Engine<MemoryDevice> {
        let rules = RuleSet::from_str(DOC, RuleSource::Provided).unwrap();
        Engine::new(config, Router::new(rules), MemoryDevice::new(), Box::new(NoProtector))
    }

    fn dns_query(id: u16, name: &str) -> Vec<u8> {
        dns::Message {
            id,
            flags: dns::FLAG_RD,
            questions: vec![dns::Question {
                name: name.to_string(),
                qtype: dns::TYPE_A,
                qclass: dns::CLASS_IN,
            }],
            answers: Vec::new(),
            authorities: Vec::new(),
            additionals: Vec::new(),
        }
        .encode()
        .unwrap()
    }

    fn inject(engine: &mut Engine<MemoryDevice>, packet: Vec<u8>) {
        engine.device_mut().inject(packet);
    }

    #[test]
    fn a_tunnel_owned_dns_query_is_answered_without_touching_the_network() {
        let mut engine = engine_with(StackConfig::default());
        let client_ip: IpAddr = "10.0.0.2".parse().unwrap();

        let query = dns_query(0x4242, "cdn.example");
        inject(
            &mut engine,
            packet::build_udp_packet(client_ip, v4(10), 40000, 53, &query).unwrap(),
        );

        let taken = engine.step(Duration::ZERO).unwrap();
        assert_eq!(taken, 1);

        let sent = engine.device_mut().drain_sent();
        assert_eq!(sent.len(), 1);
        let header = packet::parse_ip_header(&sent[0]).unwrap();
        assert_eq!(header.src, v4(10), "the answer must come from the address dialled");
        assert_eq!(header.dst, client_ip);
        let datagram = packet::parse_udp(&sent[0], &header).unwrap();
        let response = dns::Message::parse(datagram.payload).unwrap();
        assert_eq!(response.id, 0x4242);
        assert_eq!(response.answer_addresses().len(), 2);

        assert_eq!(engine.stats().dns_answered_locally, 1);
        assert_eq!(engine.stats().packets_in, 1);
        assert_eq!(engine.stats().packets_out, 1);
        assert_eq!(engine.flows(Instant::now()).len(), 0);
    }

    #[test]
    fn a_udp_flow_is_relayed_and_its_reply_comes_back_from_the_address_dialled() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server_addr = server.local_addr().unwrap();

        let fake_dst = v4(60);
        let config = StackConfig::default()
            .with_override(DestinationOverride::address(fake_dst, server_addr.ip()));
        let mut engine = engine_with(config);
        let client_ip: IpAddr = "10.0.0.2".parse().unwrap();

        inject(
            &mut engine,
            packet::build_udp_packet(client_ip, fake_dst, 40000, server_addr.port(), b"ping")
                .unwrap(),
        );
        engine.step(Duration::ZERO).unwrap();

        let mut buf = [0u8; 64];
        let (n, from) = server.recv_from(&mut buf).expect("the server must be reached");
        assert_eq!(&buf[..n], b"ping");

        server.send_to(b"pong", from).unwrap();
        engine.step(Duration::ZERO).unwrap();

        let sent = engine.device_mut().drain_sent();
        assert_eq!(sent.len(), 1);
        let header = packet::parse_ip_header(&sent[0]).unwrap();
        assert_eq!(header.src, fake_dst);
        assert_eq!(header.dst, client_ip);
        let datagram = packet::parse_udp(&sent[0], &header).unwrap();
        assert_eq!(datagram.payload, b"pong");
        assert_eq!(engine.stats().bytes_upstream_to_client, 4);
    }

    #[test]
    fn a_udp_packet_does_not_reach_the_tcp_relay_and_vice_versa() {
        let mut engine = engine_with(StackConfig::default());
        let client_ip: IpAddr = "10.0.0.2".parse().unwrap();

        // A UDP datagram to a blocked destination is answered with ICMP, which
        // proves the UDP path handled it.
        inject(
            &mut engine,
            packet::build_udp_packet(client_ip, IpAddr::V4(Ipv4Addr::LOCALHOST), 40000, 9999, b"x")
                .unwrap(),
        );
        // A TCP SYN to the same address is refused by the TCP relay, which is a
        // silent abort rather than a packet.
        inject(
            &mut engine,
            packet::build_tcp_segment(
                client_ip,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                40001,
                9999,
                1,
                0,
                packet::TcpFlags {
                    syn: true,
                    ..Default::default()
                },
                65535,
                &[],
            )
            .unwrap(),
        );

        engine.step(Duration::ZERO).unwrap();

        assert_eq!(engine.stats().udp_flows_rejected, 1);
        assert_eq!(engine.stats().tcp_flows_rejected, 1);
        assert_eq!(engine.stats().packets_in, 2);

        // Two refusals come back, one per protocol, and neither is a data packet.
        let sent = engine.device_mut().drain_sent();
        assert_eq!(engine.stats().packets_out, 2);
        let protocols: Vec<u8> = sent
            .iter()
            .map(|bytes| packet::parse_ip_header(bytes).unwrap().protocol)
            .collect();
        assert!(protocols.contains(&packet::PROTO_ICMP), "the datagram is refused with ICMP");
        assert!(protocols.contains(&packet::PROTO_TCP), "the connection is refused with a reset");
    }

    #[test]
    fn unparsable_and_unhandled_packets_are_counted_not_relayed() {
        let mut engine = engine_with(StackConfig::default());

        // Too short to hold an IP header.
        inject(&mut engine, vec![0x45, 0x00, 0x00]);
        // IPv4 but an ICMP payload: the relay has nowhere to forward it.
        inject(
            &mut engine,
            packet::build_icmp_port_unreachable(v4(10), v4(20), &[0u8; 28]).unwrap(),
        );

        engine.step(Duration::ZERO).unwrap();

        assert_eq!(engine.stats().packets_unparsable, 1);
        assert_eq!(engine.stats().packets_ignored, 1);
        assert_eq!(engine.stats().packets_out, 0);
    }

    #[test]
    fn a_batch_of_packets_is_bounded_so_the_loop_keeps_turning() {
        let mut engine = engine_with(StackConfig::default());
        let client_ip: IpAddr = "10.0.0.2".parse().unwrap();
        let query = dns_query(1, "cdn.example");

        for port in 0..(MAX_PACKETS_PER_STEP + 10) {
            inject(
                &mut engine,
                packet::build_udp_packet(
                    client_ip,
                    v4(10),
                    1024 + port as u16,
                    53,
                    &query,
                )
                .unwrap(),
            );
        }

        let taken = engine.step(Duration::ZERO).unwrap();
        assert_eq!(taken, MAX_PACKETS_PER_STEP);
        assert_eq!(engine.device().pending(), 10, "the rest waits for the next pass");

        // The next pass picks up where the last one stopped.
        let taken = engine.step(Duration::ZERO).unwrap();
        assert_eq!(taken, 10);
        assert_eq!(engine.device().pending(), 0);
    }

    #[test]
    fn a_rule_update_keeps_the_flows_that_are_already_running() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dst = v4(70);
        let config = StackConfig::default()
            .with_override(DestinationOverride::address(fake_dst, server_addr.ip()));
        let mut engine = engine_with(config);
        let client_ip: IpAddr = "10.0.0.2".parse().unwrap();

        inject(
            &mut engine,
            packet::build_udp_packet(client_ip, fake_dst, 40000, server_addr.port(), b"hi")
                .unwrap(),
        );
        engine.step(Duration::ZERO).unwrap();
        assert_eq!(engine.flows(Instant::now()).len(), 1);

        let replacement =
            RuleSet::from_str(DOC, RuleSource::Cache).expect("the same document must compile");
        engine.replace_rules(replacement);

        assert_eq!(
            engine.flows(Instant::now()).len(),
            1,
            "applying a rule update must not kill a working flow"
        );
    }

    #[test]
    fn reset_drops_every_flow() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dst = v4(71);
        let config = StackConfig::default()
            .with_override(DestinationOverride::address(fake_dst, server_addr.ip()));
        let mut engine = engine_with(config);
        let client_ip: IpAddr = "10.0.0.2".parse().unwrap();

        inject(
            &mut engine,
            packet::build_udp_packet(client_ip, fake_dst, 40000, server_addr.port(), b"hi")
                .unwrap(),
        );
        engine.step(Duration::ZERO).unwrap();
        assert_eq!(engine.flows(Instant::now()).len(), 1);

        engine.reset();
        let snapshot = engine.flows(Instant::now());
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.tcp_listeners, 0);
    }

    #[test]
    fn a_device_read_error_is_reported_rather_than_swallowed() {
        let mut engine = engine_with(StackConfig::default());
        engine.device_mut().fail_next_read(io::ErrorKind::Other);

        let err = engine.step(Duration::ZERO).expect_err("the error must surface");
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn a_zero_timeout_poll_does_not_block() {
        let mut engine = engine_with(StackConfig::default());
        let start = Instant::now();
        engine.step(Duration::ZERO).unwrap();
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn the_flow_snapshot_describes_what_is_being_relayed() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let fake_dst = v4(72);
        let config = StackConfig::default()
            .with_override(DestinationOverride::address(fake_dst, server_addr.ip()));
        let mut engine = engine_with(config);
        let client_ip: IpAddr = "10.0.0.2".parse().unwrap();

        inject(
            &mut engine,
            packet::build_udp_packet(client_ip, fake_dst, 40000, server_addr.port(), b"hi")
                .unwrap(),
        );
        engine.step(Duration::ZERO).unwrap();

        let snapshot = engine.flows(Instant::now());
        assert_eq!(snapshot.udp.len(), 1);
        assert_eq!(snapshot.udp[0].requested, SocketAddr::new(fake_dst, server_addr.port()));
        assert_eq!(snapshot.udp[0].target, SocketAddr::new(server_addr.ip(), server_addr.port()));
        assert_eq!(snapshot.udp[0].reason, "override");
        assert!(snapshot.tcp.is_empty());
    }
}

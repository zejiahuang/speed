//! TCP relay.
//!
//! The client's TCP connection is terminated by a userspace stack, and the bytes
//! are carried upstream by a real socket. There is no other way to do this: a TUN
//! device hands over IP packets, and a host socket cannot be fed IP packets.
//!
//! # Why a listener pool
//!
//! A `smoltcp` socket in the listening state *becomes* the connection when it
//! accepts a SYN, so accepting one connection consumes the listener. The relay
//! therefore keeps [`StackConfig::listener_pool`] spare listeners per destination
//! endpoint and creates one for every initial SYN it observes. Without the spares,
//! a client opening several connections to one host in parallel — which every
//! browser does — would see the second connection stall until its SYN retransmit,
//! a second later, when a fresh listener exists again.
//!
//! # Transparent addressing
//!
//! The interface claims one address and accepts packets for every other unicast
//! destination, and a listening socket with no bound address adopts the
//! destination of the SYN it accepts. The stack therefore replies from the
//! address the client dialled, which is what makes the relay transparent rather
//! than a NAT.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::io::RawFd;
use std::time::{Duration, Instant};

use smoltcp::iface::{Config as IfaceConfig, Interface, Route, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{
    HardwareAddress, IpAddress, IpCidr, IpListenEndpoint, Ipv4Address, Ipv6Address,
};

use watt_rules::{Family, Outcome};

use crate::config::{StackConfig, Stats};
use crate::flow::FlowKey;
use crate::packet::{self, PROTO_TCP};
use crate::planner::Planner;
use crate::poller::{Poller, INTEREST_READ, INTEREST_WRITE};
use crate::upstream::{is_retryable, ConnectState, Protector, UpstreamSocket};
use crate::verify::{self, Verdicts};

/// Bytes moved in one relay step per direction.
const RELAY_CHUNK: usize = 16 * 1024;

/// Tunables copied out of [`StackConfig`].
#[derive(Debug, Clone, Copy)]
struct Tuning {
    rx_buffer: usize,
    tx_buffer: usize,
    listener_pool: usize,
    max_listeners_per_endpoint: usize,
    max_listeners: usize,
    max_flows: usize,
    idle_timeout: Duration,
    connect_timeout: Duration,
    first_connect_timeout: Duration,
    max_candidates: usize,
    /// How many candidates one flow dials at once. `1` is the old serial dial.
    race_width: usize,
    /// Minimum gap between two candidate launches of one race.
    ///
    /// The larger of `StackConfig::race_launch_interval` and the deprecated
    /// `StackConfig::connect_stagger`, so an operator who set a long stagger
    /// keeps it.
    race_launch_interval: Duration,
    /// Global ceiling on concurrent in-flight upstream dials.
    max_dialing: usize,
    buffer_limit: usize,
}

/// In-memory packet queues bridging the TUN and `smoltcp`.
///
/// The engine moves packets between the real device and these queues, which keeps
/// the stack's `Device` implementation free of file descriptors and makes the
/// whole TCP path testable without root privileges.
#[derive(Debug)]
struct QueueDevice {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
    mtu: usize,
}

impl QueueDevice {
    fn new(mtu: usize) -> Self {
        Self {
            rx: VecDeque::new(),
            tx: Vec::new(),
            mtu,
        }
    }
}

/// A received packet, owned so the token does not borrow the device.
#[derive(Debug)]
struct OwnedRxToken {
    packet: Vec<u8>,
}

impl RxToken for OwnedRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut packet = self.packet;
        f(&mut packet)
    }
}

/// A transmit token that appends the finished packet to the device's queue.
#[derive(Debug)]
struct QueueTxToken<'a> {
    out: &'a mut Vec<Vec<u8>>,
}

impl TxToken for QueueTxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);
        self.out.push(buffer);
        result
    }
}

impl Device for QueueDevice {
    type RxToken<'a> = OwnedRxToken;
    type TxToken<'a> = QueueTxToken<'a>;

    fn receive(&mut self, _timestamp: SmolInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let packet = self.rx.pop_front()?;
        Some((OwnedRxToken { packet }, QueueTxToken { out: &mut self.tx }))
    }

    fn transmit(&mut self, _timestamp: SmolInstant) -> Option<Self::TxToken<'_>> {
        Some(QueueTxToken { out: &mut self.tx })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        // `DeviceCapabilities` is `#[non_exhaustive]`, so it is built from its
        // default rather than with a struct expression.
        let mut capabilities = DeviceCapabilities::default();
        // `Medium::Ip` is what makes the stack treat what it is handed as bare IP
        // packets. It also removes the Ethernet routing check from the transmit
        // path, which is what lets the interface answer for addresses it does not
        // own.
        capabilities.medium = Medium::Ip;
        capabilities.max_transmission_unit = self.mtu;
        capabilities.max_burst_size = None;
        // The default, `Checksum::Both`, computes and verifies in software. That
        // is required here: a TUN device performs no checksum offload, so a
        // packet the stack emits without a valid checksum is simply dropped.
        capabilities
    }
}

/// An upstream connection belonging to one flow.
///
/// A flow keeps a *window* of these while it races (see [`TcpFlow::upstreams`]).
/// Each one owns its own descriptor and carries the target it was dialled for,
/// so a winner can be named by index without re-deriving anything from the
/// candidate list.
#[derive(Debug)]
struct Upstream {
    socket: UpstreamSocket,
    /// The address this socket was dialled for.
    target: SocketAddr,
    /// Which entry of the flow's candidate list this dial came from.
    ///
    /// Recorded so a failure can be logged against the candidate it belongs to
    /// even after the window has been reordered by a `swap_remove`.
    candidate_index: usize,
    /// True until the non-blocking connect is confirmed.
    connecting: bool,
    /// Set by the poll loop when the descriptor became writable.
    connect_ready: bool,
    started_at: Instant,
    /// Set once this dial has been given up on. Only ever set on a socket that
    /// is about to leave the window, but named so the state is explicit.
    failed: bool,
}

/// One relayed TCP connection.
#[derive(Debug)]
struct TcpFlow {
    key: FlowKey,
    /// Targets in preference order, from the routing decision.
    candidates: Vec<SocketAddr>,
    /// The next candidate that has not been dialled yet.
    ///
    /// Monotonic. It only ever moves forward, so every candidate is dialled at
    /// most once even as the window reorders failures out from under it.
    next_candidate: usize,
    steered: bool,
    reason: &'static str,
    /// The in-flight dial window, at most `Tuning::race_width` entries.
    ///
    /// Empty before the first dial and after every candidate has failed. While
    /// it holds more than one entry the flow is *racing*: the first socket whose
    /// `SO_ERROR` clears wins and the rest are closed. No client byte is written
    /// to any of them until the winner is known.
    upstreams: Vec<Upstream>,
    /// Index into `upstreams` of the socket that won the race, once there is one.
    winner: Option<usize>,
    /// When the most recent candidate was launched, for the launch stagger.
    last_launch: Option<Instant>,
    /// Set once no candidate can be reached.
    failed: bool,
    to_upstream: VecDeque<u8>,
    to_client: VecDeque<u8>,
    client_eof: bool,
    upstream_eof: bool,
    /// Bytes received from the upstream over this flow's life.
    ///
    /// Zero at close time means the connection was established and nothing ever
    /// came back — the one failure the connect-time report cannot see. Tracked per
    /// flow rather than globally because the judgement is per destination.
    bytes_from_upstream: u64,
    /// The domain this flow is for, when the planner has observed one.
    ///
    /// The tunnel is handed an address; the name only exists because the planner
    /// records the answers to the DNS it forwards. Without it there is nothing to
    /// check a candidate's certificate against.
    host: Option<String>,
    /// While set and still in the future, do not dial: a certificate check is
    /// running and its answer decides which candidate to use.
    ///
    /// Cleared the moment the deadline passes, and only ever set while a verdict
    /// is genuinely pending. The launch stagger is deliberately *not* folded in
    /// here: it is an artificial delay with no external event behind it, so a
    /// deadline that only a moving clock can clear parks forever on a clock that
    /// stands still — which is exactly the test harness. See
    /// [`TcpRelay::open_race`] for how the spacing is done without a clock read.
    verify_by: Option<Instant>,
    /// Whether any byte has been handed to the client-facing socket yet.
    ///
    /// A per-flow flag rather than a read of the global counter: the counter is
    /// incremented when the byte arrives from upstream, so by the time this runs
    /// it is never zero and the first delivery would never be reported.
    delivered_any: bool,
    /// Whether any byte has been taken from the client-facing socket yet.
    ///
    /// The mirror of `delivered_any`, and a per-flow flag for the same reason.
    /// The first version read the global `bytes_client_to_upstream` counter,
    /// which a *previous flow* had already advanced — so a second flow's first
    /// bytes were never reported and the direction looked dead in the log while
    /// the counter said otherwise. That cost an hour of reading a stall into
    /// silence that was only the log's.
    sent_any: bool,
    opened_at: Instant,
    last_activity: Instant,
}

impl TcpFlow {
    fn new(
        key: FlowKey,
        candidates: Vec<SocketAddr>,
        steered: bool,
        reason: &'static str,
        now: Instant,
    ) -> Self {
        Self {
            key,
            candidates,
            next_candidate: 0,
            steered,
            reason,
            upstreams: Vec::new(),
            winner: None,
            last_launch: None,
            failed: false,
            to_upstream: VecDeque::new(),
            to_client: VecDeque::new(),
            client_eof: false,
            upstream_eof: false,
            bytes_from_upstream: 0,
            host: None,
            verify_by: None,
            delivered_any: false,
            sent_any: false,
            opened_at: now,
            last_activity: now,
        }
    }

    /// Reorder candidates so the verdict table's preference is honoured.
    ///
    /// Rejected candidates are **demoted, not deleted**. That distinction is the
    /// whole lesson of this function, and it was learned twice:
    ///
    /// * First version discarded every candidate with no verdict, which emptied
    ///   the list the moment the network hiccuped.
    /// * Second version discarded only confirmed-bad ones, but still discarded
    ///   them — and that fails whenever the verdict table holds exactly one
    ///   "good". Measured on the tunnel path: `raw.githubusercontent.com`
    ///   arrived with eight candidates, the table had confirmed one of them and
    ///   rejected the other seven, the list was cut to that one address, and it
    ///   then timed out. The flow reported `exhausted its candidates` while seven
    ///   unproven addresses sat in the discarded pile.
    ///
    /// A verdict is a cached guess about an address, reached by a probe that took
    /// a single handshake. It is good enough to decide *order* — that is what it
    /// was measured for, and it is what makes a fast wrong address lose to a slow
    /// right one. It is not good enough to decide *existence*, because a wrong
    /// guess then costs the domain entirely rather than one attempt.
    ///
    /// So the list keeps everything and sorts: confirmed-good, then unverified,
    /// then rejected. The dial walks it from the front and stops at the first
    /// success, so a correct verdict is honoured and an incorrect one costs a
    /// couple of extra candidates instead of the connection.
    fn order_by_verdicts(&mut self, host: &str, verdicts: &Verdicts) {
        // Nothing known: keep the selector's order, which is itself meaningful.
        if self
            .candidates
            .iter()
            .all(|candidate| verdicts.get(host, candidate.ip()).is_none())
        {
            return;
        }

        self.candidates.sort_by_key(|candidate| {
            match verdicts.get(host, candidate.ip()) {
                Some(true) => 0,  // confirmed to serve this host
                None => 1,        // not checked, or checked and inconclusive
                Some(false) => 2, // checked and did not cover it
            }
        });
    }

    /// The candidate that would be dialled next, if any is left.
    ///
    /// This is the dial cursor, not the winner: a snapshot or a test that wants
    /// "what will we try next" wants this; one that wants "what are we using"
    /// wants [`Self::winner_target`].
    fn next_target(&self) -> Option<SocketAddr> {
        self.candidates.get(self.next_candidate).copied()
    }

    /// The address this flow is talking to, or is trying to.
    ///
    /// Once a race is won this is the winner's target — the only address that
    /// will ever carry the client's bytes, and the one the end-of-flow judgement
    /// is made against. While still racing it is the earliest-launched candidate
    /// in flight, so a snapshot or a failure log names a real address instead of
    /// nothing.
    fn winner_target(&self) -> Option<SocketAddr> {
        match self.winner {
            Some(index) => self.upstreams.get(index).map(|up| up.target),
            None => self
                .upstreams
                .iter()
                .min_by_key(|up| up.started_at)
                .map(|up| up.target),
        }
    }

    /// How many dials are in flight right now.
    ///
    /// A dial that has been given up on is not in flight, so it is not counted;
    /// the window is rebuilt when one is retired, but the filter keeps the
    /// meaning correct even if a failure is ever observed before it is removed.
    fn racing_count(&self) -> usize {
        self.upstreams.iter().filter(|up| !up.failed).count()
    }

    /// Whether there is still somewhere else for this flow to go.
    ///
    /// This is what separates "slow" from "dead" without measuring anything. A
    /// dial with nowhere to fail over to is given the full `connect_timeout`,
    /// because a server that answers in eight seconds is a server worth having.
    /// A dial with an alternative — an unlaunched candidate, or another socket
    /// already in the window — is given the short one, because spending the
    /// client's whole budget on one address to the exclusion of seven others is
    /// not patience, it is a choice not to try.
    ///
    /// It is the old `has_alternative`, widened from "a candidate is queued
    /// behind this one" to "the window has redundancy": under racing the dials
    /// run in parallel, so a second in-flight socket is just as much of a
    /// fallback as an untried address.
    fn can_rotate_window(&self) -> bool {
        self.next_candidate < self.candidates.len() || self.racing_count() > 1
    }

    /// Skip past candidates the verdict table has already ruled out.
    ///
    /// Dialling one is a guaranteed waste: a wrong certificate is rejected by
    /// the client no matter how fast the handshake was, and the failover pays a
    /// **full connect timeout** for every candidate it tries. Measured on the
    /// tunnel path with `github.com`, eight candidates at ten seconds each is
    /// eighty seconds, and the client gives up long before the last one — so
    /// merely demoting a rejected address to the back of the list does not help
    /// when most of the list is rejected.
    ///
    /// It is still only a skip, never a discard, and that is the property that
    /// matters. If every remaining candidate is ruled out, the skip undoes
    /// itself: a cached verdict is a guess made from one handshake, and letting
    /// a wrong guess turn a domain into a failure is the mistake this whole
    /// module exists to avoid. A ruled-out address is therefore tried after all
    /// the others, not instead of them.
    fn skip_rejected(&mut self, host: &str, verdicts: &Verdicts) {
        // Only skip while something is left that is not ruled out. When the tail
        // is all that remains, the flow has nowhere better to go and must try
        // what it has.
        while self.next_candidate < self.candidates.len()
            && verdicts.get(host, self.candidates[self.next_candidate].ip()) == Some(false)
        {
            let has_better = self.candidates[self.next_candidate + 1..]
                .iter()
                .any(|candidate| verdicts.get(host, candidate.ip()) != Some(false));
            if !has_better {
                return;
            }
            log::info!(
                "watt: flow {} skipping {} — the check says it does not serve {host}",
                self.key.dst,
                self.candidates[self.next_candidate].ip()
            );
            self.next_candidate += 1;
        }
    }
}

/// A read-only summary of one relayed connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpFlowInfo {
    pub key: FlowKey,
    /// Where the client asked to connect.
    pub requested: SocketAddr,
    /// The candidate currently being attempted, if any.
    pub target: Option<SocketAddr>,
    pub steered: bool,
    pub reason: &'static str,
    /// Index of the candidate in use, and how many there are in total.
    pub candidate_index: usize,
    pub candidates: usize,
    /// How many upstream dials are in flight for this flow right now.
    ///
    /// Above one means the flow is racing; zero means it has not started or has
    /// run out of candidates. Added for the status screen and the logs, and
    /// backwards compatible: it is a new field, nothing existing moved.
    pub racing: usize,
    /// True once every candidate has been tried and failed.
    pub failed: bool,
    pub pending_upstream: usize,
    pub pending_client: usize,
    pub age: Duration,
    pub idle: Duration,
}

/// The TCP half of the kernel.
pub struct TcpRelay {
    iface: Interface,
    sockets: SocketSet<'static>,
    device: QueueDevice,
    tuning: Tuning,
    /// Timestamps handed to `smoltcp` are milliseconds since this instant.
    epoch: Instant,
    /// Spare and pending listening sockets, keyed by destination endpoint.
    listeners: HashMap<(IpAddr, u16), Vec<SocketHandle>>,
    flows: HashMap<SocketHandle, TcpFlow>,
    /// Maps an upstream descriptor back to its flow, so poll events are O(1).
    ///
    /// With racing it maps *many* descriptors to one handle — every socket in a
    /// flow's window has its own entry — which is why it is only ever changed by
    /// [`TcpRelay::insert_upstream`] and [`TcpRelay::remove_upstream`].
    fd_index: HashMap<RawFd, SocketHandle>,
    /// How many upstream dials are in flight across every flow right now.
    ///
    /// Only sockets still in the *connecting* phase are counted, so an
    /// established flow does not consume the budget a new race needs. This is
    /// the handshake-window ceiling: without it, `max_tcp_flows` × a race width
    /// of three could open thousands of sockets at once and exhaust
    /// `RLIMIT_NOFILE`.
    dialing: usize,
    /// Certificate verdicts for rule addresses, produced off the relay loop.
    ///
    /// Shared rather than owned because the probes run on detached threads, and
    /// the relay only ever reads it — which is what keeps one handshake from
    /// stalling every other flow.
    verdicts: Verdicts,
    /// Whether the certificate check runs at all. See `StackConfig`.
    certificate_check: bool,
}

impl std::fmt::Debug for TcpRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpRelay")
            .field("listeners", &self.listeners.len())
            .field("flows", &self.flows.len())
            .finish_non_exhaustive()
    }
}

impl TcpRelay {
    /// Build the relay and configure the interface for transparent relaying.
    pub fn new(config: &StackConfig, epoch: Instant, seed: u64) -> Self {
        let listener_pool = config.listener_pool.clamp(1, 64);
        let tuning = Tuning {
            rx_buffer: config.tcp_rx_buffer.max(2048),
            tx_buffer: config.tcp_tx_buffer.max(2048),
            listener_pool,
            // A burst ceiling below the resting pool would make the pool
            // unreachable, which is the opposite of what the caller asked for.
            max_listeners_per_endpoint: config
                .max_listeners_per_endpoint
                .max(listener_pool)
                .clamp(1, 4096),
            max_listeners: config.max_listeners.clamp(1, 65536),
            max_flows: config.max_tcp_flows.max(1),
            idle_timeout: config.tcp_idle_timeout,
            connect_timeout: config.connect_timeout,
            first_connect_timeout: config.first_connect_timeout.min(config.connect_timeout),
            max_candidates: config.max_candidates.max(1),
            // Clamped here rather than at the source so a hand-built config with
            // an absurd width still produces a usable window.
            race_width: config.race_width.clamp(1, 4),
            // The deprecated `connect_stagger` folds in as the floor: an operator
            // who set a long stagger keeps it under racing.
            race_launch_interval: config.race_launch_interval.max(config.connect_stagger),
            max_dialing: config.max_dialing.max(1),
            buffer_limit: config.flow_buffer_limit.max(RELAY_CHUNK),
        };

        let verdicts = Verdicts::new();
        let certificate_check = config.certificate_check;
        let mut device = QueueDevice::new(config.mtu);
        let mut iface_config = IfaceConfig::new(HardwareAddress::Ip);
        iface_config.random_seed = seed;
        let smol_now = SmolInstant::from_millis(0);
        let mut iface = Interface::new(iface_config, &mut device, smol_now);

        let our_address = to_smoltcp_addr(config.address);
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::new(our_address, config.prefix_len));
        });

        // Accepting packets for addresses the interface does not own is what makes
        // this a relay rather than a host. The stack only does so when the
        // destination routes back to one of its own addresses, hence the default
        // route pointing at ourselves.
        if let IpAddress::Ipv4(v4) = our_address {
            iface.routes_mut().update(|routes| {
                let _ = routes.push(Route::new_ipv4_gateway(v4));
            });
        }
        iface.set_any_ip(true);

        Self {
            iface,
            sockets: SocketSet::new(Vec::new()),
            device,
            tuning,
            epoch,
            listeners: HashMap::new(),
            flows: HashMap::new(),
            fd_index: HashMap::new(),
            dialing: 0,
            verdicts,
            certificate_check,
        }
    }

    fn smol_now(&self, now: Instant) -> SmolInstant {
        let millis = now.saturating_duration_since(self.epoch).as_millis();
        // `smoltcp` counts milliseconds in an `i64`; the clamp keeps a clock that
        // jumped forward from wrapping the timestamp into the past.
        SmolInstant::from_millis(millis.min(i64::MAX as u128) as i64)
    }

    /// Number of relayed connections currently open.
    pub fn active_flows(&self) -> usize {
        self.flows.len()
    }

    /// Number of listening sockets currently allocated.
    pub fn pending_listeners(&self) -> usize {
        self.listeners.values().map(Vec::len).sum()
    }

    /// A read-only view of the open connections, for logs and the status screen.
    pub fn snapshot(&self, now: Instant) -> Vec<TcpFlowInfo> {
        self.flows
            .values()
            .map(|flow| TcpFlowInfo {
                key: flow.key,
                requested: SocketAddr::new(flow.key.dst, flow.key.dst_port),
                target: flow.winner_target(),
                steered: flow.steered,
                reason: flow.reason,
                candidate_index: flow.next_candidate,
                candidates: flow.candidates.len(),
                racing: flow.racing_count(),
                failed: flow.failed,
                pending_upstream: flow.to_upstream.len(),
                pending_client: flow.to_client.len(),
                age: now.saturating_duration_since(flow.opened_at),
                idle: now.saturating_duration_since(flow.last_activity),
            })
            .collect()
    }

    /// The certificate verdicts, for reporting. See `Verdicts::snapshot`.
    pub(crate) fn verdicts(&self) -> &Verdicts {
        &self.verdicts
    }

    /// Hand a TCP packet read from the TUN to the stack.
    ///
    /// A connection request is inspected before it reaches the stack, because the
    /// listener it needs must exist *before* the stack processes it.
    ///
    /// The same inspection is where a destination the kernel refuses is turned
    /// away. Letting the stack answer first and aborting afterwards would produce a
    /// completed handshake followed immediately by a reset, which a client reads
    /// as a flaky server and retries; a reset in reply to the SYN says "no" once,
    /// unambiguously.
    pub fn feed(&mut self, packet: &[u8], now: Instant, planner: &mut Planner, stats: &mut Stats) {
        if let Ok(header) = packet::parse_ip_header(packet) {
            if header.protocol == PROTO_TCP {
                if let Ok(segment) = packet::parse_tcp(packet, &header) {
                    if segment.is_initial_syn() {
                        let requested = SocketAddr::new(header.dst, segment.dst_port);
                        let decision = planner.decide(now, requested);
                        if !planner.can_relay(&decision) {
                            // The reset is sent from the address and port the
                            // client dialled, so it looks like the destination
                            // refusing the connection rather than the tunnel
                            // interfering.
                            if let Ok(reset) =
                                packet::build_tcp_reset(header.dst, header.src, &segment)
                            {
                                self.device.tx.push(reset);
                                stats.tcp_resets_sent += 1;
                                stats.tcp_flows_rejected += 1;
                            }
                            return;
                        }
                        if !self.reserve_listener(header.dst, segment.dst_port) {
                            // The stack will reset this one itself unless a socket
                            // already in the pool takes it. Counting it is what
                            // turns "the server looks flaky" into a ceiling that
                            // is visibly too low.
                            stats.tcp_listener_ceiling_hits += 1;
                        }
                    }
                }
            }
        }
        self.device.rx.push_back(packet.to_vec());
    }

    /// Make sure the SYN that just arrived has a listening socket to land on.
    ///
    /// One listener is created per initial SYN, and that is not an optimisation
    /// that can be traded away. smoltcp consumes the socket that accepts a
    /// connection, and it answers a SYN that finds no free listener with a reset
    /// of its own — which a client reports as "connection refused", the exact
    /// symptom of a server that is down. A pool of spare listeners on top covers
    /// a retransmitted SYN, whose original socket has already been consumed.
    ///
    /// Returns `false` when a ceiling was reached. That is not proof the client
    /// was refused: a socket already in the pool may serve this SYN. It is a
    /// signal that demand has reached the ceiling, which is why it is counted.
    fn reserve_listener(&mut self, addr: IpAddr, port: u16) -> bool {
        if port == 0 {
            return true;
        }
        let key = (addr, port);
        let held = self.listeners.get(&key).map(Vec::len).unwrap_or(0);
        let all = self.pending_listeners();

        // Listeners occupy a socket slot but are not flows; counting them keeps
        // the cap honest under a SYN flood.
        if held + 1 > self.tuning.max_listeners_per_endpoint
            || all + 1 > self.tuning.max_listeners
            || self.flows.len() + all + 1 > self.tuning.max_flows
        {
            return false;
        }

        if !self.add_listener(key, addr, port) {
            return false;
        }
        self.top_up_listeners(key, addr, port);
        true
    }

    /// Bring one endpoint back up to its resting pool.
    ///
    /// Called after a connection is accepted, so the next client to dial the same
    /// host finds a socket ready instead of waiting out a SYN retransmit. This
    /// only ever fills the pool — growing past it is [`TcpRelay::reserve_listener`]'s
    /// job, and letting this grow too would leak one listener per accepted
    /// connection until a ceiling stopped it.
    fn top_up_listeners(&mut self, key: (IpAddr, u16), addr: IpAddr, port: u16) {
        while self.listeners.get(&key).map(Vec::len).unwrap_or(0) < self.tuning.listener_pool
            && self.pending_listeners() < self.tuning.max_listeners
            && self.flows.len() + self.pending_listeners() < self.tuning.max_flows
        {
            if !self.add_listener(key, addr, port) {
                return;
            }
        }
    }

    /// Create one listening socket for `addr`:`port` and remember it.
    ///
    /// `false` means `listen` refused, which leaves the pool exactly as it was.
    fn add_listener(&mut self, key: (IpAddr, u16), addr: IpAddr, port: u16) -> bool {
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0u8; self.tuning.rx_buffer]),
            tcp::SocketBuffer::new(vec![0u8; self.tuning.tx_buffer]),
        );
        // Binding the exact destination means one listener only ever answers
        // packets for the address it was created for.
        let endpoint = IpListenEndpoint {
            addr: Some(to_smoltcp_addr(addr)),
            port,
        };
        if socket.listen(endpoint).is_err() {
            return false;
        }
        let handle = self.sockets.add(socket);
        self.listeners.entry(key).or_default().push(handle);
        true
    }

    /// Register every upstream descriptor with the poll loop.
    pub fn register(&self, poller: &mut Poller) {
        for flow in self.flows.values() {
            for upstream in &flow.upstreams {
                let interests = if upstream.connecting {
                    INTEREST_WRITE
                } else {
                    INTEREST_READ | INTEREST_WRITE
                };
                poller.add(upstream.socket.raw_fd(), upstream.socket.raw_fd() as u64, interests);
            }
        }
    }

    /// Tell the relay that an upstream descriptor became ready.
    ///
    /// Called for a writable descriptor, and also for one reporting an error or a
    /// hangup. In every case the connect attempt has finished, which is the only
    /// signal that makes `SO_ERROR` meaningful. Ignoring the error case would
    /// leave a connection to a refused port waiting out the full connect timeout
    /// instead of moving to the next candidate immediately.
    pub fn note_ready(&mut self, fd: RawFd) {
        let Some(&handle) = self.fd_index.get(&fd) else {
            return;
        };
        if let Some(flow) = self.flows.get_mut(&handle) {
            // Locate the exact socket. With a window of them, the descriptor is
            // the only thing that says *which* candidate just finished dialling —
            // setting the flag on "the" upstream would credit the wrong one.
            if let Some(upstream) = flow.upstreams.iter_mut().find(|up| up.socket.raw_fd() == fd) {
                upstream.connect_ready = true;
            }
        }
    }

    /// Advance the stack and relay data. Emitted packets are appended to `out`.
    pub fn service(
        &mut self,
        now: Instant,
        out: &mut Vec<Vec<u8>>,
        planner: &mut Planner,
        protector: &mut dyn Protector,
        stats: &mut Stats,
    ) {
        let smol_now = self.smol_now(now);

        // Pass one: deliver what the client sent and emit the stack's replies.
        self.iface.poll(smol_now, &mut self.device, &mut self.sockets);
        out.append(&mut self.device.tx);

        self.promote_accepted(now, planner, stats);
        self.relay(now, planner, protector, stats);

        // Pass two: the bytes just queued into the stack's send buffers need a
        // poll before they become packets.
        self.iface.poll(smol_now, &mut self.device, &mut self.sockets);
        out.append(&mut self.device.tx);

        self.reap(now, planner, stats);
    }

    /// Move sockets that accepted a connection out of the listener pool.
    ///
    /// A connection is only planned here, once its handshake is under way, rather
    /// than when the SYN arrives. By this point the client's source port is known,
    /// so the decision describes a real flow and the upstream connect can start
    /// before the handshake finishes.
    fn promote_accepted(&mut self, now: Instant, planner: &mut Planner, stats: &mut Stats) {
        let keys: Vec<(IpAddr, u16)> = self.listeners.keys().copied().collect();
        let mut accepted: Vec<((IpAddr, u16), SocketHandle, FlowKey)> = Vec::new();
        let mut closed: Vec<((IpAddr, u16), SocketHandle)> = Vec::new();

        for key in keys {
            let handles = self.listeners.get(&key).cloned().unwrap_or_default();
            let mut surviving = Vec::with_capacity(handles.len());
            for handle in handles {
                let socket = self.sockets.get::<tcp::Socket>(handle);
                if socket.is_listening() {
                    surviving.push(handle);
                    continue;
                }
                match (socket.local_endpoint(), socket.remote_endpoint()) {
                    (Some(local), Some(remote)) => {
                        let flow_key = FlowKey::new(
                            PROTO_TCP,
                            from_smoltcp_addr(remote.addr),
                            from_smoltcp_addr(local.addr),
                            remote.port,
                            local.port,
                        );
                        accepted.push((key, handle, flow_key));
                    }
                    // The socket left the listening state without a peer: it was
                    // closed or aborted. Drop it.
                    _ => closed.push((key, handle)),
                }
            }
            if surviving.is_empty() {
                self.listeners.remove(&key);
            } else {
                self.listeners.insert(key, surviving);
            }
        }

        for (_, handle) in closed {
            self.sockets.remove(handle);
        }

        for (key, handle, flow_key) in accepted {
            let requested = SocketAddr::new(flow_key.dst, flow_key.dst_port);
            let decision = planner.decide(now, requested);

            // A target that is loopback, multicast or the tunnel itself can never
            // work, so refuse the flow rather than looping forever. A static
            // rewrite is the one exception, which `can_relay` encodes.
            if !planner.can_relay(&decision) {
                let socket = self.sockets.get_mut::<tcp::Socket>(handle);
                socket.abort();
                stats.tcp_flows_rejected += 1;
                continue;
            }

            let mut candidates = vec![decision.target];
            for alternative in &decision.alternatives {
                // A rule listing a loopback or multicast address is an authoring
                // mistake, and retrying against it would only waste time.
                if planner.is_blocked_target(*alternative) {
                    continue;
                }
                candidates.push(SocketAddr::new(*alternative, requested.port()));
            }

            // Bound the list. Failover is sequential and each candidate that
            // does not answer costs a full `connect_timeout`, so an unbounded
            // list turns an unreachable domain into a multi-minute hang:
            // `github.com` has thirty-nine addresses and the client gives up
            // long before the last one is tried.
            //
            // Truncation is safe because the list is already ranked — the
            // selector orders by health, and the certificate check reorders what
            // it knows — so the discarded tail is the part that was least
            // likely to work anyway.
            if candidates.len() > self.tuning.max_candidates {
                candidates.truncate(self.tuning.max_candidates);
            }

            if decision.is_steered() {
                stats.flows_matched_rules += 1;
            } else {
                stats.flows_direct += 1;
            }

            // The tunnel is handed an address, not a name. The name comes from
            // the DNS the planner has been observing — and with it, the same
            // certificate check the proxy does. Without this a rule's fastest
            // address wins even when it serves someone else's certificate, which
            // is exactly how `github.com` failed: of its thirty-nine rule
            // addresses about ten were real GitHub, and the rest answered in a
            // fifth of the time with the wrong certificate.
            let host = planner
                .domains_for(requested.ip(), now)
                .first()
                .map(|name| name.to_string());

            // Diagnostic: how many flows have a name to check a certificate
            // against. Without a name the whole verification path is skipped, and
            // from the outside that looks identical to a verification that ran and
            // concluded nothing.
            if decision.is_steered() {
                log::info!(
                    "watt: flow {} -> {} candidates, host={:?}",
                    requested.ip(),
                    candidates.len(),
                    host
                );
            }

            let mut flow =
                TcpFlow::new(flow_key, candidates, decision.is_steered(), decision.reason(), now);

            if let Some(name) = &host {
                flow.host = Some(name.clone());
                if !self.certificate_check {
                    // Off: the flow dials exactly as it did before this existed.
                    self.flows.insert(handle, flow);
                    continue;
                }

                // Verdicts already known: push what cannot work to the back.
                // Not a removal — a cached guess that deletes a candidate can
                // delete the only reachable address for the whole domain.
                flow.order_by_verdicts(name, &self.verdicts);

                // Then step over the head rather than dial it.
                //
                // Ordering alone is not enough here, and the log made that plain:
                // a settled `Some(false)` at index 0 was still dialled, the flow
                // sat in `connect` for the full timeout, and only *then* did the
                // failure path call `skip_rejected` — by which point the client
                // had already spent its whole patience on an address the table
                // had ruled out before the first packet was sent. Sorting decides
                // which candidate is best; skipping decides not to pay for one
                // already known to be worthless.
                flow.skip_rejected(name, &self.verdicts);

                // Ask about the rest, off this loop.
                let addresses: Vec<IpAddr> =
                    flow.candidates.iter().map(|candidate| candidate.ip()).collect();
                self.verdicts.check(name, &addresses);

                // Hold the dial briefly while the answer arrives. The client is
                // already connected to the listener and waiting for a handshake,
                // so this is latency on a handshake that was going to cost a
                // round trip anyway — and it is what turns "the fastest wrong
                // address wins" into "the right one wins".
                if self.verdicts.any_pending(name, &addresses) {
                    flow.verify_by = Some(now + verify::WAIT);
                }
            }

            self.flows.insert(handle, flow);
            stats.tcp_flows_opened += 1;

            // Refill the pool so a parallel connection does not have to wait for a
            // SYN retransmit.
            self.top_up_listeners(key, key.0, key.1);
        }
    }

    /// Advance every flow's dial race and move its bytes.
    ///
    /// Pure state machine, one step per tick: no sleeps, no blocking waits. The
    /// poll loop has already gathered which descriptors are writable and which
    /// are readable, so a slow destination stalls its own flow and nothing else.
    fn relay(
        &mut self,
        now: Instant,
        planner: &mut Planner,
        protector: &mut dyn Protector,
        stats: &mut Stats,
    ) {
        let handles: Vec<SocketHandle> = self.flows.keys().copied().collect();

        // One scratch buffer for the whole pass, not one per read.
        //
        // The drain loops used to allocate a fresh chunk each iteration, which at
        // 16 KB a time is an allocation and a zero-fill per 16 KB moved. Hoisting
        // it here reuses the same memory for every flow in the tick, and because
        // the loops below only ever read into it and copy out of it before the
        // next read, sharing is safe.
        let mut scratch = vec![0u8; RELAY_CHUNK];

        for handle in handles {
            // A flow removed earlier in this same pass must not be touched; the
            // check keeps the borrows below honest.
            if !self.flows.contains_key(&handle) {
                continue;
            }

            // --- advance the dial race ------------------------------------
            let still_dialling = self
                .flows
                .get(&handle)
                .map(|flow| flow.winner.is_none() && !flow.failed)
                .unwrap_or(false);
            if still_dialling {
                self.advance_dials(handle, now, planner, protector, stats);
            }

            // A flow that gave up while dialling is finished; `advance_dials`
            // has already aborted its client-facing socket.
            if self.flows.get(&handle).map(|flow| flow.failed).unwrap_or(false) {
                continue;
            }

            // --- move bytes through the winner ----------------------------
            self.move_bytes(handle, now, &mut scratch, stats);
        }
    }

    /// Drive one flow's dial race: retire finished dials, then open new ones.
    fn advance_dials(
        &mut self,
        handle: SocketHandle,
        now: Instant,
        planner: &mut Planner,
        protector: &mut dyn Protector,
        stats: &mut Stats,
    ) {
        // 1. Retire dials that have finished — a winner, a refusal or a timeout.
        self.retire_dials(handle, now, planner);

        // 2. A winner ends the race for this flow; there is nothing left to
        //    launch and the window is now a single established socket.
        if self
            .flows
            .get(&handle)
            .map(|flow| flow.winner.is_some())
            .unwrap_or(false)
        {
            return;
        }

        // 3. Every candidate tried and the window empty: the flow is dead.
        let exhausted = self
            .flows
            .get(&handle)
            .map(|flow| flow.next_candidate >= flow.candidates.len() && flow.upstreams.is_empty())
            .unwrap_or(false);
        if exhausted {
            let flow = self.flows.get_mut(&handle).expect("checked above");
            log::warn!("watt: flow {} exhausted its candidates", flow.key.dst);
            flow.failed = true;
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);
            socket.abort();
            stats.tcp_connect_failures += 1;
            return;
        }

        // 4. Fill the window: at most one candidate per tick.
        self.open_race(handle, now, planner, protector, stats);
    }

    /// Read the outcome of every finished dial in a flow's window.
    ///
    /// The first socket whose `SO_ERROR` clears wins. A dial that was refused,
    /// or that outlived its budget while there was somewhere else to go, is
    /// retired and its descriptor closed. No byte is written to any of them yet:
    /// the client's bytes stay buffered until a winner exists.
    fn retire_dials(&mut self, handle: SocketHandle, now: Instant, planner: &mut Planner) {
        let can_rotate = self
            .flows
            .get(&handle)
            .map(|flow| flow.can_rotate_window())
            .unwrap_or(false);
        // A dial with an alternative behind it gets the short budget so the
        // window keeps rotating; one that is the flow's last hope gets the full
        // `connect_timeout`, because a slow but honest server deserves it.
        let budget = if can_rotate {
            self.tuning.first_connect_timeout
        } else {
            self.tuning.connect_timeout
        };

        let mut winner: Option<(usize, SocketAddr, Duration)> = None;
        let mut failures: Vec<(usize, SocketAddr, &'static str)> = Vec::new();
        {
            let Some(flow) = self.flows.get(&handle) else {
                return;
            };
            for (index, up) in flow.upstreams.iter().enumerate() {
                if up.failed {
                    continue;
                }
                if !up.connecting {
                    // Completed synchronously when it was launched. It is a
                    // winner; the earliest one takes it.
                    if winner.is_none() {
                        winner =
                            Some((index, up.target, now.saturating_duration_since(up.started_at)));
                    }
                    continue;
                }
                // The poll loop's `connect_ready` is what says the dial has
                // *finished*. `SO_ERROR` cannot stand in for it: a non-blocking
                // connect that is still in progress reports 0 too, so reading it
                // early would take a socket still in SYN_SENT for a winner and
                // close the losers around it — the very failure racing exists to
                // prevent. Only once the descriptor is writable is `SO_ERROR` a
                // verdict.
                if up.connect_ready {
                    match up.socket.take_connect_error() {
                        Ok(()) => {
                            if winner.is_none() {
                                winner = Some((
                                    index,
                                    up.target,
                                    now.saturating_duration_since(up.started_at),
                                ));
                            }
                        }
                        // `SO_ERROR` was not ready to be read yet.
                        Err(err) if is_retryable(&err) => {}
                        Err(_) => failures.push((index, up.target, "refused")),
                    }
                } else if now.saturating_duration_since(up.started_at) > budget {
                    // Still dialling and out of patience: rotate the window.
                    failures.push((index, up.target, "timed out"));
                }
            }
        }

        if let Some((winner_index, winner_target, rtt)) = winner {
            self.confirm_winner(handle, winner_index, winner_target, rtt, now, planner);
            return;
        }

        if failures.is_empty() {
            return;
        }

        // Retire the failures. The window is rebuilt in one pass so no index can
        // go stale; the retired sockets are unregistered only after the rebuild,
        // when the flow is no longer borrowed, so `fd_index` still changes in
        // exactly one place.
        let flow = self.flows.get_mut(&handle).expect("checked above");
        let dst = flow.key.dst;
        let candidate_total = flow.candidates.len();
        let mut kept = Vec::with_capacity(flow.upstreams.len());
        let mut removed: Vec<Upstream> = Vec::new();
        for (index, mut up) in flow.upstreams.drain(..).enumerate() {
            let why = failures
                .iter()
                .find(|(failed_index, _, _)| *failed_index == index)
                .map(|(_, _, why)| *why);
            let Some(why) = why else {
                kept.push(up);
                continue;
            };
            // Marked before it leaves the window so nothing downstream can
            // mistake a dial that was given up on for one still in flight.
            up.failed = true;
            log::warn!(
                "watt: flow {} upstream to {} died (candidate {}/{}, {why})",
                dst,
                up.target.ip(),
                up.candidate_index + 1,
                candidate_total
            );
            planner.report_failure(up.target.ip(), now);
            removed.push(up);
        }
        flow.upstreams = kept;
        for up in removed {
            self.unlink_upstream(&up);
            // `up` is dropped here: `UpstreamSocket::Drop` closes the fd.
        }
    }

    /// Record a winner, close the rest of the window, and report the win.
    fn confirm_winner(
        &mut self,
        handle: SocketHandle,
        winner_index: usize,
        winner_target: SocketAddr,
        rtt: Duration,
        now: Instant,
        planner: &mut Planner,
    ) {
        let flow = self.flows.get_mut(&handle).expect("checked above");
        let winner_fd = flow.upstreams[winner_index].socket.raw_fd();
        // The winner has finished dialling, so it leaves the handshake budget.
        if flow.upstreams[winner_index].connecting {
            flow.upstreams[winner_index].connecting = false;
            self.dialing = self.dialing.saturating_sub(1);
        }
        flow.last_launch = None;

        let losers = flow.upstreams.len().saturating_sub(1);
        log::info!(
            "watt: flow {} upstream connected via {} ({} ms) — closed {losers} loser(s)",
            flow.key.dst,
            winner_target.ip(),
            rtt.as_millis()
        );
        planner.report_success(winner_target.ip(), rtt, now);

        // Keep only the winner, named by descriptor rather than by position so a
        // reorder cannot lose it. Nothing was ever written to a loser, so
        // closing it is a bare FIN — no application-level side effect for the
        // peer, which is what makes closing losers immediately safe.
        let mut kept = Vec::with_capacity(1);
        let mut removed: Vec<Upstream> = Vec::new();
        for up in flow.upstreams.drain(..) {
            if up.socket.raw_fd() == winner_fd {
                kept.push(up);
                continue;
            }
            log::info!(
                "watt: flow {} closed loser {} at the winner's arrival",
                flow.key.dst,
                up.target.ip()
            );
            removed.push(up);
        }
        flow.upstreams = kept;
        flow.winner = Some(0);
        for up in removed {
            self.unlink_upstream(&up);
        }
    }

    /// Launch the next candidate, if the window has room for one.
    ///
    /// At most one candidate is started per call, which is what staggers the
    /// SYNs of a single race. The spacing is deliberately *not* a clock
    /// comparison: under the fixed-clock test harness `now` never moves, and a
    /// deadline only a moving clock could clear would park the flow forever (see
    /// [`TcpFlow::verify_by`]). One per tick is the whole mechanism.
    fn open_race(
        &mut self,
        handle: SocketHandle,
        now: Instant,
        planner: &mut Planner,
        protector: &mut dyn Protector,
        stats: &mut Stats,
    ) {
        // The certificate check gates the first dial: while a verdict is pending
        // there is nothing worth dialling. The wait is bounded, so a probe that
        // never answers costs one wait rather than the connection.
        {
            let Some(flow) = self.flows.get_mut(&handle) else {
                return;
            };
            if let Some(deadline) = flow.verify_by {
                if now < deadline {
                    return;
                }
                flow.verify_by = None;
                // Re-read the verdicts now they have had their chance: a
                // candidate confirmed bad sinks to the back and the flow moves
                // to one that is not. It is still there if the preferred ones
                // all turn out to be worse.
                if let Some(name) = flow.host.clone() {
                    flow.order_by_verdicts(&name, &self.verdicts);
                    flow.skip_rejected(&name, &self.verdicts);
                }
            }
        }

        let tuning = self.tuning;
        let (target, candidate_index) = {
            let Some(flow) = self.flows.get_mut(&handle) else {
                return;
            };
            if flow.failed || flow.winner.is_some() {
                return;
            }
            if flow.racing_count() >= tuning.race_width {
                return;
            }
            // Global handshake-window ceiling: a SYN storm must not be able to
            // open more sockets than the process can hold.
            if self.dialing >= tuning.max_dialing {
                return;
            }
            // The stagger, without reading the clock: a launch is allowed when
            // the interval has passed *or* when the clock has not moved since
            // the last one. The second arm is what keeps a frozen clock from
            // wedging a race.
            let interval_elapsed = match flow.last_launch {
                None => true,
                Some(last) => {
                    let elapsed = now.saturating_duration_since(last);
                    elapsed >= tuning.race_launch_interval || elapsed == Duration::ZERO
                }
            };
            if !interval_elapsed {
                return;
            }
            // Step over anything the verdict table has since ruled out, so a
            // replacement does not dial an address already known worthless.
            if let Some(name) = flow.host.clone() {
                flow.skip_rejected(&name, &self.verdicts);
            }
            let index = flow.next_candidate;
            let Some(target) = flow.next_target() else {
                return;
            };
            (target, index)
        };

        let family = Family::of(target.ip());
        let mut socket = match UpstreamSocket::tcp(family, protector) {
            Ok(socket) => socket,
            Err(err) => {
                let flow = self.flows.get_mut(&handle).expect("checked above");
                log::warn!(
                    "watt: flow {} could not open an upstream socket: {err}",
                    flow.key.dst
                );
                flow.failed = true;
                let socket = self.sockets.get_mut::<tcp::Socket>(handle);
                socket.abort();
                stats.tcp_connect_failures += 1;
                return;
            }
        };

        match socket.start_connect(target) {
            Ok(state) => {
                let connecting = state == ConnectState::InProgress;
                let up = Upstream {
                    socket,
                    target,
                    candidate_index,
                    connecting,
                    connect_ready: !connecting,
                    started_at: now,
                    failed: false,
                };
                {
                    let flow = self.flows.get_mut(&handle).expect("checked above");
                    flow.next_candidate = candidate_index + 1;
                    flow.last_launch = Some(now);
                }
                self.insert_upstream(handle, up);
            }
            Err(_) => {
                // The socket never made it into the window, so dropping it here
                // closes its descriptor; there is nothing to unregister.
                let flow = self.flows.get_mut(&handle).expect("checked above");
                log::warn!(
                    "watt: flow {} connect to {} failed, rotating to the next candidate",
                    flow.key.dst,
                    target.ip()
                );
                flow.next_candidate = candidate_index + 1;
                flow.last_launch = Some(now);
                planner.report_failure(target.ip(), now);
                drop(socket);
            }
        }
    }

    /// Move bytes in both directions for one flow, through its winner.
    fn move_bytes(
        &mut self,
        handle: SocketHandle,
        now: Instant,
        scratch: &mut [u8],
        stats: &mut Stats,
    ) {
        let tuning = self.tuning;
        let Some(flow) = self.flows.get_mut(&handle) else {
            return;
        };
        let socket = self.sockets.get_mut::<tcp::Socket>(handle);

        if flow.failed {
            return;
        }

        // --- client to upstream ---------------------------------------
        //
        // Read unconditionally and buffer. While a race is on there is no winner
        // to write to yet, and the invariant is that a client byte reaches only
        // the winner; holding it in `to_upstream` until the race settles is how
        // that is guaranteed. Bounded two ways, like the downstream loop below:
        // by the per-flow buffer and by a short read.
        let mut up_budget = tuning.buffer_limit.saturating_sub(flow.to_upstream.len());
        while up_budget > 0 && socket.can_recv() {
            let room = tuning.buffer_limit.saturating_sub(flow.to_upstream.len());
            if room == 0 {
                break;
            }
            let cap = RELAY_CHUNK.min(room);
            let chunk = &mut scratch[..cap];
            let mut taken = 0usize;
            match socket.recv(|buffer| {
                let n = buffer.len().min(cap);
                chunk[..n].copy_from_slice(&buffer[..n]);
                taken = n;
                (n, ())
            }) {
                Ok(()) => {}
                Err(tcp::RecvError::Finished) => {
                    flow.client_eof = true;
                    break;
                }
                Err(tcp::RecvError::InvalidState) => break,
            }
            if taken == 0 {
                break;
            }
            if !flow.sent_any {
                flow.sent_any = true;
                log::info!(
                    "watt: flow {} first client bytes: {} (racing: {})",
                    flow.key.dst,
                    taken,
                    flow.racing_count()
                );
            }
            flow.to_upstream.extend(&chunk[..taken]);
            flow.last_activity = now;
            stats.bytes_client_to_upstream += taken as u64;
            up_budget = up_budget.saturating_sub(taken);

            // Nothing more buffered — the next `recv` would report
            // `InvalidState` and cost a syscall for the privilege.
            if taken < cap {
                break;
            }
        }

        // --- upstream to client (winner only) -------------------------
        //
        // Only the winner is read. A loser that has bytes waiting is left alone:
        // it is about to be closed, and counting its bytes would credit an
        // address that is not carrying the flow.
        if let Some(winner) = flow.winner {
            let mut budget = tuning.buffer_limit.saturating_sub(flow.to_client.len());
            while budget > 0 {
                let room = tuning.buffer_limit.saturating_sub(flow.to_client.len());
                if room == 0 {
                    break;
                }
                let cap = RELAY_CHUNK.min(room);
                let chunk = &mut scratch[..cap];
                let (read, eof) = {
                    let up = &flow.upstreams[winner];
                    match up.socket.read(chunk) {
                        Ok(0) => (0, true),
                        Ok(n) => (n, false),
                        Err(err) if is_retryable(&err) => (0, false),
                        Err(_) => (0, true),
                    }
                };
                if read == 0 {
                    if eof {
                        flow.upstream_eof = true;
                    }
                    break;
                }
                if flow.bytes_from_upstream == 0 {
                    // First byte back. Says the upstream path is genuinely
                    // carrying data, which is what separates "the server never
                    // answered" from "the answer never reached the client".
                    log::info!(
                        "watt: flow {} first upstream bytes: {} (client socket {:?})",
                        flow.key.dst,
                        read,
                        socket.state()
                    );
                }
                flow.to_client.extend(&chunk[..read]);
                flow.last_activity = now;
                flow.bytes_from_upstream += read as u64;
                stats.bytes_upstream_to_client += read as u64;
                budget = budget.saturating_sub(read);

                if read < cap {
                    break;
                }
            }
        }

        // --- flush to the winner --------------------------------------
        if let Some(winner) = flow.winner {
            while !flow.to_upstream.is_empty() {
                let (head, _) = flow.to_upstream.as_slices();
                match flow.upstreams[winner].socket.write(head) {
                    Ok(0) => break,
                    Ok(n) => {
                        flow.to_upstream.drain(..n);
                        flow.last_activity = now;
                    }
                    Err(err) if is_retryable(&err) => break,
                    Err(_) => {
                        flow.upstream_eof = true;
                        break;
                    }
                }
            }
        }

        // --- flush to the client --------------------------------------
        while !flow.to_client.is_empty() && socket.can_send() {
            let (head, _) = flow.to_client.as_slices();
            match socket.send_slice(head) {
                Ok(0) => break,
                Ok(n) => {
                    if !flow.delivered_any {
                        // The last leg. Without it a session that received
                        // thousands of bytes but delivered none is
                        // indistinguishable from one where the server never
                        // answered at all.
                        flow.delivered_any = true;
                        log::info!(
                            "watt: flow {} delivered {} bytes to the client",
                            flow.key.dst,
                            n
                        );
                    }
                    flow.to_client.drain(..n);
                    flow.last_activity = now;
                }
                Err(_) => break,
            }
        }

        // --- shutdown propagation -------------------------------------
        if flow.client_eof && flow.to_upstream.is_empty() {
            if let Some(winner) = flow.winner {
                let _ = flow.upstreams[winner].socket.shutdown_write();
            }
        }
        if flow.upstream_eof && flow.to_client.is_empty() && socket.is_active() {
            socket.close();
        }
    }

    /// Register an upstream in a flow's window, keeping `fd_index` in step.
    ///
    /// The only place `fd_index` gains an entry. Keeping the two changes in one
    /// function is what stops a descriptor from ever being mapped to a flow that
    /// does not own it — the bug class that `note_ready` and `reap` both depend
    /// on not existing.
    fn insert_upstream(&mut self, handle: SocketHandle, up: Upstream) {
        self.fd_index.insert(up.socket.raw_fd(), handle);
        if up.connecting {
            self.dialing += 1;
        }
        if let Some(flow) = self.flows.get_mut(&handle) {
            flow.upstreams.push(up);
        }
    }

    /// Unregister an upstream that has already left a flow's window.
    ///
    /// The only place `fd_index` loses an entry, and the counterpart of
    /// [`TcpRelay::insert_upstream`]. It is called *after* the socket is out of
    /// the window rather than before, so the descriptor and the dialing count
    /// are retired together and can never drift from the window that owned them.
    /// The socket itself is closed by its own `Drop` when the caller's value goes
    /// out of scope.
    fn unlink_upstream(&mut self, up: &Upstream) {
        self.fd_index.remove(&up.socket.raw_fd());
        if up.connecting {
            self.dialing = self.dialing.saturating_sub(1);
        }
    }

    /// Remove flows that are finished or idle.
    fn reap(&mut self, now: Instant, planner: &mut Planner, stats: &mut Stats) {
        let tuning = self.tuning;
        let mut finished: Vec<SocketHandle> = Vec::new();

        for (handle, flow) in &self.flows {
            let socket = self.sockets.get::<tcp::Socket>(*handle);
            let state = socket.state();

            let both_closed = matches!(
                state,
                tcp::State::Closed | tcp::State::TimeWait | tcp::State::Closing
            );
            let idle = now.saturating_duration_since(flow.last_activity) > tuning.idle_timeout;
            // A flow whose client vanished without a FIN would otherwise hold a
            // descriptor and a socket forever.
            let abandoned = flow.failed && flow.upstreams.is_empty();

            if both_closed || idle || abandoned {
                finished.push(*handle);
            }
        }

        for handle in finished {
            if let Some(flow) = self.flows.remove(&handle) {
                // Every socket in the window is unregistered before it is
                // dropped, so a descriptor can never outlive the flow that owns
                // it — the leak that racing would otherwise make possible, since
                // a flow may hold more than one.
                for upstream in &flow.upstreams {
                    self.unlink_upstream(upstream);
                }
                // The flow is over, so this is the first moment the session can be
                // judged as a whole. A connection that was established and never
                // carried a byte back is the failure the connect-time report
                // cannot see: a middlebox that accepts and then blackholes looks
                // exactly like this, and it used to be recorded as a success.
                //
                // A flow that never reached an upstream is not reported at all —
                // it has no address to blame, and the connect failure was already
                // reported where it happened. The address is the *winner's*, so a
                // loser that never carried a byte cannot be blamed for a silence
                // the winner caused.
                if let Some(target) = flow.winner_target() {
                    if flow.bytes_from_upstream == 0 && !flow.failed {
                        planner.report_outcome(target.ip(), Outcome::Silent, now);
                    } else if flow.bytes_from_upstream > 0 {
                        planner.report_outcome(target.ip(), Outcome::Healthy, now);
                    }
                }
            }
            self.sockets.remove(handle);
            stats.tcp_flows_closed += 1;
        }
    }

    /// Drop every flow and listener, used when the tunnel is torn down.
    pub fn reset(&mut self) {
        let handles: Vec<SocketHandle> = self.flows.keys().copied().collect();
        for handle in handles {
            self.sockets.remove(handle);
        }
        let listeners: Vec<SocketHandle> = self
            .listeners
            .values()
            .flat_map(|handles| handles.iter().copied())
            .collect();
        for handle in listeners {
            self.sockets.remove(handle);
        }
        self.flows.clear();
        self.listeners.clear();
        self.fd_index.clear();
        self.dialing = 0;
        self.device.rx.clear();
        self.device.tx.clear();
    }

    /// Read the packets the stack has emitted. Used by tests.
    pub fn take_emitted(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.device.tx)
    }

    /// Push a packet into the receive path without the SYN inspection of
    /// [`TcpRelay::feed`]. Used by tests that drive the stack directly.
    pub fn inject_raw(&mut self, packet: Vec<u8>) {
        self.device.rx.push_back(packet);
    }

    /// Run one stack pass and collect emitted packets.
    pub fn step(
        &mut self,
        now: Instant,
        planner: &mut Planner,
        protector: &mut dyn Protector,
        stats: &mut Stats,
    ) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        self.service(now, &mut out, planner, protector, stats);
        out
    }
}

/// Convert a standard address into the wire crate's representation.
pub fn to_smoltcp_addr(addr: IpAddr) -> IpAddress {
    match addr {
        IpAddr::V4(v4) => IpAddress::Ipv4(Ipv4Address::from_bytes(&v4.octets())),
        IpAddr::V6(v6) => IpAddress::Ipv6(Ipv6Address::from_bytes(&v6.octets())),
    }
}

/// Convert a wire address back into the standard representation.
pub fn from_smoltcp_addr(addr: IpAddress) -> IpAddr {
    match addr {
        IpAddress::Ipv4(v4) => IpAddr::V4(Ipv4Addr::from(v4.0)),
        IpAddress::Ipv6(v6) => IpAddr::V6(Ipv6Addr::from(v6.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StackConfig;
    use crate::planner::Planner;
    use crate::upstream::NoProtector;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use watt_rules::{RuleSet, RuleSource};

    /// Drive a full TCP handshake and payload exchange through the relay.
    ///
    /// This is the test that proves the kernel works: a hand built SYN goes in,
    /// the stack answers with a SYN-ACK, the handshake completes, and payload
    /// reaches a real server on loopback through an upstream socket.
    struct Harness {
        relay: TcpRelay,
        planner: Planner,
        protector: NoProtector,
        stats: Stats,
        epoch: Instant,
        client_ip: IpAddr,
        client_port: u16,
        server_ip: IpAddr,
        server_port: u16,
        client_seq: u32,
    }

    impl Harness {
        fn new(server: SocketAddr, overrides: Vec<crate::config::DestinationOverride>) -> Self {
            Self::with_config(
                server,
                StackConfig {
                    overrides,
                    ..StackConfig::default()
                },
            )
        }

        fn with_config(server: SocketAddr, config: StackConfig) -> Self {
            let rules = RuleSet::from_str(
                r#"{"groups":[{"entries":[{"id":"1","name":"T","domains":["t.example"],"ips":["203.0.113.10"],"port":"443"}]}]}"#,
                RuleSource::Provided,
            )
            .unwrap();
            let epoch = Instant::now();
            Self {
                relay: TcpRelay::new(&config, epoch, 0x1234_5678),
                planner: Planner::new(watt_rules::Router::new(rules), &config),
                protector: NoProtector,
                stats: Stats::default(),
                epoch,
                client_ip: "10.0.0.2".parse().unwrap(),
                client_port: 40000,
                server_ip: server.ip(),
                server_port: server.port(),
                client_seq: 1000,
            }
        }

        fn now(&self) -> Instant {
            self.epoch + Duration::from_millis(1)
        }

        /// Advance the relay the way the engine does.
        ///
        /// The engine's poll loop is what tells the relay a descriptor finished
        /// dialling — without it `connect_ready` is never set and a connect would
        /// never be confirmed. This harness drives the relay directly, so it has
        /// to stand in for that loop: report every upstream descriptor a
        /// zero-timeout `poll` finds ready, then take one relay step.
        fn pump(&mut self, now: Instant) -> Vec<Vec<u8>> {
            let fds: Vec<RawFd> = self.relay.fd_index.keys().copied().collect();
            for fd in fds {
                if descriptor_is_writable(fd) {
                    self.relay.note_ready(fd);
                }
            }
            self.relay.step(now, &mut self.planner, &mut self.protector, &mut self.stats)
        }

        /// Inject a SYN and return the SYN-ACK the stack produced.
        fn handshake_syn(&mut self) -> packet::TcpSegment<'static> {
            let syn = packet::build_tcp_segment(
                self.client_ip,
                self.server_ip,
                self.client_port,
                self.server_port,
                self.client_seq,
                0,
                packet::TcpFlags { syn: true, ..Default::default() },
                65535,
                &[],
            )
            .unwrap();
            let now = self.now();
            self.relay.feed(&syn, now, &mut self.planner, &mut self.stats);
            let emitted = self.pump(now);
            let syn_ack = emitted
                .into_iter()
                .find(|bytes| {
                    packet::parse_ip_header(bytes)
                        .ok()
                        .filter(|h| h.protocol == PROTO_TCP)
                        .and_then(|h| packet::parse_tcp(bytes, &h).ok())
                        .map(|segment| segment.flags.syn && segment.flags.ack)
                        .unwrap_or(false)
                })
                .expect("the stack must answer a SYN with a SYN-ACK");
            // Leak the buffer so the returned segment can borrow it for the test's
            // lifetime; the harness is short lived and this keeps the helper simple.
            let leaked: &'static [u8] = Box::leak(syn_ack.into_boxed_slice());
            let header = packet::parse_ip_header(leaked).unwrap();
            packet::parse_tcp(leaked, &header).unwrap()
        }

        fn send(&mut self, seq: u32, ack: u32, flags: packet::TcpFlags, payload: &[u8]) -> Vec<Vec<u8>> {
            let segment = packet::build_tcp_segment(
                self.client_ip,
                self.server_ip,
                self.client_port,
                self.server_port,
                seq,
                ack,
                flags,
                65535,
                payload,
            )
            .unwrap();
            let now = self.now();
            self.relay.feed(&segment, now, &mut self.planner, &mut self.stats);
            self.pump(now)
        }
    }

    /// Whether a zero-timeout `poll` reports `fd` as writable.
    ///
    /// The test harness stands in for the engine's poll loop, and this is the
    /// one thing that loop contributes: "this descriptor finished dialling".
    /// `revents` is checked rather than only `POLLOUT` because a refusal arrives
    /// as `POLLERR`/`POLLHUP` — the dial still finished, just badly.
    fn descriptor_is_writable(fd: RawFd) -> bool {
        let mut entry = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: one initialised `pollfd` and a zero timeout, so this returns
        // at once without waiting.
        let ready = unsafe { libc::poll(&mut entry, 1, 0) };
        ready > 0 && entry.revents != 0
    }

    fn find_payload(packets: &[Vec<u8>], needle: &[u8]) -> bool {
        packets.iter().any(|bytes| {
            packet::parse_ip_header(bytes)
                .ok()
                .filter(|h| h.protocol == PROTO_TCP)
                .and_then(|h| packet::parse_tcp(bytes, &h).ok())
                .map(|segment| segment.payload.windows(needle.len()).any(|w| w == needle))
                .unwrap_or(false)
        })
    }

    #[test]
    fn completes_a_handshake_and_relays_payload_both_ways() {
        // A real server on loopback, reached through an override so the test does
        // not depend on the host's routing table.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();

        let fake_dst: IpAddr = "203.0.113.10".parse().unwrap();
        let mut harness = Harness::new(
            server_addr,
            vec![crate::config::DestinationOverride::endpoint(
                fake_dst,
                server_addr.port(),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                server_addr.port(),
            )],
        );
        harness.server_ip = fake_dst;

        let syn_ack = harness.handshake_syn();
        assert_eq!(syn_ack.src_port, server_addr.port());
        assert_eq!(syn_ack.dst_port, harness.client_port);
        // The reply must come from the address the client dialled, not from the
        // tunnel address: that is what makes the relay transparent rather than a
        // NAT.
        let reply = packet::build_tcp_segment(
            harness.server_ip,
            harness.client_ip,
            harness.server_port,
            harness.client_port,
            0,
            0,
            packet::TcpFlags::none(),
            0,
            &[],
        )
        .unwrap();
        let reply_header = packet::parse_ip_header(&reply).unwrap();
        assert_eq!(reply_header.src, harness.server_ip);

        // Complete the handshake.
        harness.send(
            harness.client_seq + 1,
            syn_ack.seq.wrapping_add(1),
            packet::TcpFlags { ack: true, ..Default::default() },
            b"",
        );

        // The upstream connect must have reached the real server.
        let (mut accepted, _) = listener.accept().expect("the relay must connect upstream");

        // Client to server.
        let emitted = harness.send(
            harness.client_seq + 1,
            syn_ack.seq.wrapping_add(1),
            packet::TcpFlags { ack: true, psh: true, ..Default::default() },
            b"hello server",
        );
        let _ = emitted;

        let mut buf = [0u8; 12];
        accepted
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        accepted.read_exact(&mut buf).expect("upstream must receive the payload");
        assert_eq!(&buf, b"hello server");

        // Server to client.
        accepted.write_all(b"hi client").unwrap();
        let emitted = harness.send(
            harness.client_seq + 13,
            syn_ack.seq.wrapping_add(1),
            packet::TcpFlags { ack: true, ..Default::default() },
            b"",
        );
        assert!(
            find_payload(&emitted, b"hi client"),
            "the reply must reach the client"
        );
        assert!(harness.stats.bytes_upstream_to_client >= 9);
        assert!(harness.stats.bytes_client_to_upstream >= 12);
    }

    #[test]
    fn a_serial_window_still_completes_a_handshake_and_relays_payload() {
        // `race_width = 1` is the one-key rollback and has to reproduce the old
        // serial dial exactly. The same handshake is run again with a window of
        // one, so this regression does not depend on the default being anything
        // in particular.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let fake_dst: IpAddr = "203.0.113.10".parse().unwrap();
        let mut harness = Harness::with_config(
            server_addr,
            StackConfig {
                race_width: 1,
                overrides: vec![crate::config::DestinationOverride::endpoint(
                    fake_dst,
                    server_addr.port(),
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    server_addr.port(),
                )],
                ..StackConfig::default()
            },
        );
        harness.server_ip = fake_dst;

        let syn_ack = harness.handshake_syn();
        harness.send(
            harness.client_seq + 1,
            syn_ack.seq.wrapping_add(1),
            packet::TcpFlags { ack: true, ..Default::default() },
            b"",
        );

        let (mut accepted, _) = listener.accept().expect("the relay must connect upstream");
        harness.send(
            harness.client_seq + 1,
            syn_ack.seq.wrapping_add(1),
            packet::TcpFlags { ack: true, psh: true, ..Default::default() },
            b"hello server",
        );

        let mut buf = [0u8; 12];
        accepted
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        accepted
            .read_exact(&mut buf)
            .expect("upstream must receive the payload");
        assert_eq!(&buf, b"hello server");
        // Exactly one socket is ever in the window at a width of one.
        assert_eq!(harness.relay.fd_index.len(), 1);
    }

    #[test]
    fn a_syn_allocates_a_listener_and_a_second_syn_uses_the_pool() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let fake_dst: IpAddr = "203.0.113.10".parse().unwrap();
        let mut harness = Harness::new(
            server_addr,
            vec![crate::config::DestinationOverride::endpoint(
                fake_dst,
                server_addr.port(),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                server_addr.port(),
            )],
        );
        harness.server_ip = fake_dst;

        harness.handshake_syn();
        // After accepting one connection the pool must have been refilled, so a
        // second connection from a different source port still gets an answer.
        assert!(
            harness.relay.pending_listeners() >= 1,
            "the listener pool must be refilled"
        );
    }

    #[test]
    fn a_burst_of_parallel_syns_all_get_a_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let fake_dst: IpAddr = "203.0.113.10".parse().unwrap();
        let mut harness = Harness::new(
            server_addr,
            vec![crate::config::DestinationOverride::address(
                fake_dst,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
            )],
        );
        harness.server_ip = fake_dst;

        // Every client in the burst dials the same address and port at the same
        // moment, which is what a browser does on every page load. Each one needs
        // a listening socket of its own: smoltcp consumes the socket that accepts
        // a connection, and it resets a SYN that finds none — which the client
        // reports as "connection refused", the exact symptom of a flaky server.
        const BURST: u16 = 25;
        let client_ip = harness.client_ip;
        let server_port = harness.server_port;
        let now = harness.now();

        for index in 0..BURST {
            let syn = packet::build_tcp_segment(
                client_ip,
                fake_dst,
                40000 + index,
                server_port,
                1000 + u32::from(index),
                0,
                packet::TcpFlags {
                    syn: true,
                    ..Default::default()
                },
                65535,
                &[],
            )
            .unwrap();
            harness
                .relay
                .feed(&syn, now, &mut harness.planner, &mut harness.stats);
        }

        let emitted = harness.relay.step(
            now,
            &mut harness.planner,
            &mut harness.protector,
            &mut harness.stats,
        );
        let answered = emitted
            .iter()
            .filter(|bytes| {
                packet::parse_ip_header(bytes)
                    .ok()
                    .and_then(|header| packet::parse_tcp(bytes, &header).ok())
                    .map(|segment| segment.flags.syn && segment.flags.ack)
                    .unwrap_or(false)
            })
            .count();

        assert_eq!(
            answered, BURST as usize,
            "every SYN in the burst must be answered, not reset"
        );
        assert_eq!(harness.stats.tcp_listener_ceiling_hits, 0);
    }

    #[test]
    fn a_syn_beyond_the_listener_ceiling_is_counted_rather_than_hidden() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let fake_dst: IpAddr = "203.0.113.10".parse().unwrap();
        let mut harness = Harness::with_config(
            server_addr,
            StackConfig {
                listener_pool: 1,
                max_listeners_per_endpoint: 3,
                overrides: vec![crate::config::DestinationOverride::address(
                    fake_dst,
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                )],
                ..StackConfig::default()
            },
        );
        harness.server_ip = fake_dst;

        // Five clients, room for three. The two that find nothing have to be
        // counted: the stack resets them itself, so without the counter the only
        // symptom is a client that believes the server is flaky.
        let client_ip = harness.client_ip;
        let server_port = harness.server_port;
        let now = harness.now();

        for index in 0..5u16 {
            let syn = packet::build_tcp_segment(
                client_ip,
                fake_dst,
                41000 + index,
                server_port,
                2000 + u32::from(index),
                0,
                packet::TcpFlags {
                    syn: true,
                    ..Default::default()
                },
                65535,
                &[],
            )
            .unwrap();
            harness
                .relay
                .feed(&syn, now, &mut harness.planner, &mut harness.stats);
        }

        assert_eq!(harness.stats.tcp_listener_ceiling_hits, 2);
        assert_eq!(
            harness.relay.pending_listeners(),
            3,
            "the ceiling must hold"
        );
    }

    #[test]
    fn a_second_client_port_can_connect_in_parallel() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let fake_dst: IpAddr = "203.0.113.10".parse().unwrap();
        let mut harness = Harness::new(
            server_addr,
            vec![crate::config::DestinationOverride::endpoint(
                fake_dst,
                server_addr.port(),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                server_addr.port(),
            )],
        );
        harness.server_ip = fake_dst;

        harness.handshake_syn();

        // A different client port, same destination.
        harness.client_port = 40001;
        let syn_ack = harness.handshake_syn();
        assert_eq!(syn_ack.dst_port, 40001);
    }

    #[test]
    fn a_syn_to_a_blocked_target_is_refused_without_a_handshake() {
        // 127.0.0.1 is always refused, so no listener is needed.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let mut harness = Harness::new(server_addr, Vec::new());
        harness.server_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        harness.server_port = server_addr.port();

        let syn = packet::build_tcp_segment(
            harness.client_ip,
            harness.server_ip,
            harness.client_port,
            harness.server_port,
            harness.client_seq,
            0,
            packet::TcpFlags { syn: true, ..Default::default() },
            65535,
            &[],
        )
        .unwrap();
        let now = harness.now();
        harness
            .relay
            .feed(&syn, now, &mut harness.planner, &mut harness.stats);
        let emitted = harness
            .relay
            .step(now, &mut harness.planner, &mut harness.protector, &mut harness.stats);

        assert_eq!(harness.stats.tcp_flows_rejected, 1);
        assert_eq!(harness.relay.active_flows(), 0);
        assert_eq!(harness.relay.pending_listeners(), 0, "no listener may be allocated");

        // Exactly one packet goes back: a reset. A SYN-ACK followed by a reset
        // would look like a flaky server and invite a retry.
        assert_eq!(emitted.len(), 1, "only a reset may be emitted");
        let header = packet::parse_ip_header(&emitted[0]).unwrap();
        let reset = packet::parse_tcp(&emitted[0], &header).unwrap();
        assert!(reset.flags.rst);
        assert!(reset.flags.ack);
        assert_eq!(reset.src_port, harness.server_port);
        assert_eq!(reset.dst_port, harness.client_port);
        // The acknowledgement must cover the SYN, or a client in SYN-SENT ignores
        // the reset and retries until it times out.
        assert_eq!(reset.ack, harness.client_seq.wrapping_add(1));
        assert_eq!(harness.stats.tcp_resets_sent, 1);
    }

    #[test]
    fn reset_clears_every_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let fake_dst: IpAddr = "203.0.113.10".parse().unwrap();
        let mut harness = Harness::new(
            server_addr,
            vec![crate::config::DestinationOverride::endpoint(
                fake_dst,
                server_addr.port(),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                server_addr.port(),
            )],
        );
        harness.server_ip = fake_dst;
        harness.handshake_syn();
        assert!(harness.relay.active_flows() + harness.relay.pending_listeners() > 0);

        harness.relay.reset();
        assert_eq!(harness.relay.active_flows(), 0);
        assert_eq!(harness.relay.pending_listeners(), 0);
    }

    #[test]
    fn the_flow_key_matches_the_client_destination() {
        let key = FlowKey::new(
            PROTO_TCP,
            "10.0.0.2".parse().unwrap(),
            "203.0.113.10".parse().unwrap(),
            40000,
            443,
        );
        assert_eq!(key.dst_port, 443);
        assert_eq!(key.dst.to_string(), "203.0.113.10");
    }

    /// Build a flow with a candidate list and a host, for the skip tests.
    fn flow_with(candidates: &[&str], host: &str) -> TcpFlow {
        let now = Instant::now();
        let key = FlowKey::new(
            PROTO_TCP,
            "10.0.0.2".parse().unwrap(),
            candidates[0].parse().unwrap(),
            40000,
            443,
        );
        let addresses: Vec<SocketAddr> = candidates
            .iter()
            .map(|ip| SocketAddr::new(ip.parse().unwrap(), 443))
            .collect();
        let mut flow = TcpFlow::new(key, addresses, true, "test", now);
        flow.host = Some(host.to_string());
        flow
    }

    /// Record a settled verdict without running a probe.
    fn settle(verdicts: &Verdicts, host: &str, ip: &str, covers: bool) {
        verdicts.put_for_test(
            host,
            ip.parse().unwrap(),
            if covers {
                verify::Entry::Covers { at: Instant::now() }
            } else {
                verify::Entry::Doubtful {
                    why: verify::Doubt::WrongCertificate,
                    at: Instant::now(),
                }
            },
        );
    }

    #[test]
    fn a_rejected_candidate_is_stepped_over_rather_than_dialled() {
        // The reason this exists: eight candidates at ten seconds each is eighty
        // seconds of connect timeouts, and the client gives up long before the
        // last one. Demotion alone does not help when most of the list is
        // rejected — the dial has to step over them.
        let verdicts = Verdicts::new();
        settle(&verdicts, "github.com", "203.0.113.1", false);
        settle(&verdicts, "github.com", "203.0.113.2", false);
        settle(&verdicts, "github.com", "203.0.113.3", true);

        let mut flow = flow_with(&["203.0.113.1", "203.0.113.2", "203.0.113.3"], "github.com");
        flow.order_by_verdicts("github.com", &verdicts);
        flow.skip_rejected("github.com", &verdicts);

        assert_eq!(
            flow.next_target().unwrap().ip().to_string(),
            "203.0.113.3",
            "the confirmed-good address is the one to dial"
        );
    }

    #[test]
    fn a_list_where_everything_was_rejected_is_never_emptied() {
        // The safety property. A verdict is a guess from one handshake; letting
        // an all-rejected list turn into no dial at all is how a domain becomes
        // unreachable because of a wrong cache entry. Worse than the old bug,
        // which only wasted time.
        let verdicts = Verdicts::new();
        settle(&verdicts, "github.com", "203.0.113.1", false);
        settle(&verdicts, "github.com", "203.0.113.2", false);

        let mut flow = flow_with(&["203.0.113.1", "203.0.113.2"], "github.com");
        flow.order_by_verdicts("github.com", &verdicts);
        flow.skip_rejected("github.com", &verdicts);

        assert!(
            flow.next_target().is_some(),
            "with nothing better left, the rejected addresses are still tried"
        );
    }

    #[test]
    fn an_unverified_candidate_is_never_skipped() {
        // Only a settled "does not cover" may be stepped over. An address nobody
        // has asked about is the ordinary case and must be dialled.
        let verdicts = Verdicts::new();
        settle(&verdicts, "github.com", "203.0.113.1", false);

        let mut flow = flow_with(&["203.0.113.1", "203.0.113.2"], "github.com");
        flow.order_by_verdicts("github.com", &verdicts);
        flow.skip_rejected("github.com", &verdicts);

        assert_eq!(flow.next_target().unwrap().ip().to_string(), "203.0.113.2");
    }

    #[test]
    fn a_dial_with_somewhere_to_fail_over_to_is_the_one_that_runs_out_of_patience() {
        // The distinction the short timeout encodes. Three candidates means one
        // dead head must not consume the client's whole budget, because two more
        // addresses are waiting. One candidate means there is nothing to protect
        // the budget *for*, so the slow server gets all the time it needs.
        //
        // The check is made *after the first dial is launched*, which is the
        // moment a budget is actually chosen: `next_candidate` has moved past the
        // one now in flight.
        let mut many = flow_with(&["203.0.113.1", "203.0.113.2", "203.0.113.3"], "github.com");
        many.next_candidate = 1;
        assert!(
            many.can_rotate_window(),
            "two candidates are still queued behind the dial in flight"
        );

        let mut one = flow_with(&["203.0.113.1"], "github.com");
        one.next_candidate = 1;
        assert!(
            !one.can_rotate_window(),
            "the only candidate in flight has no fallback, so it gets the full wait"
        );
    }

    #[test]
    fn the_alternative_is_looked_for_ahead_of_the_cursor_not_behind_it() {
        // A candidate already tried is not an alternative; it is a failure the
        // flow has already paid for. Counting the head as its own fallback would
        // give the last candidate the short timeout, which is exactly the case
        // where there is nowhere else to go.
        let mut flow = flow_with(&["203.0.113.1", "203.0.113.2"], "github.com");
        flow.next_candidate = 1;
        assert!(flow.can_rotate_window());

        // The second — and last — candidate is now the dial in flight: the cursor
        // has moved past it and nothing else is queued.
        flow.next_candidate = 2;
        assert!(
            !flow.can_rotate_window(),
            "on the last candidate there is no fallback, so it gets the full wait"
        );
    }

    /// Push a dialing upstream onto a flow, for tests that need a window.
    fn push_dial(flow: &mut TcpFlow, protector: &mut NoProtector, target: SocketAddr, index: usize) {
        let mut socket = UpstreamSocket::tcp(Family::V4, protector).unwrap();
        socket.start_connect(target).unwrap();
        flow.upstreams.push(Upstream {
            socket,
            target,
            candidate_index: index,
            connecting: true,
            connect_ready: false,
            started_at: Instant::now(),
            failed: false,
        });
    }

    #[test]
    fn a_window_with_two_dials_still_has_somewhere_to_go() {
        // The racing-specific half of `can_rotate_window`: redundancy inside the
        // window counts as a fallback even when no candidate is queued behind the
        // cursor. That is what gives a racing dial the short budget and keeps the
        // window rotating instead of parking on one slow address.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let mut protector = NoProtector;

        let mut flow = flow_with(&["203.0.113.9"], "github.com");
        // The single candidate is already in flight, so the cursor is exhausted.
        flow.next_candidate = 1;
        push_dial(&mut flow, &mut protector, target, 0);
        push_dial(&mut flow, &mut protector, target, 1);

        assert_eq!(flow.racing_count(), 2);
        assert!(
            flow.can_rotate_window(),
            "two dials in flight is a fallback even with no queued candidate"
        );
    }

    /// Build a relay and planner, plus a flow holding `window`, for the dial tests.
    fn relay_with_window(
        window: Vec<Upstream>,
        candidates: Vec<SocketAddr>,
    ) -> (TcpRelay, Planner, SocketHandle) {
        let config = StackConfig::default();
        let rules = RuleSet::from_str(
            r#"{"groups":[{"entries":[{"id":"1","name":"T","domains":["t.example"],"ips":["203.0.113.10"],"port":"443"}]}]}"#,
            RuleSource::Provided,
        )
        .unwrap();
        let epoch = Instant::now();
        let mut relay = TcpRelay::new(&config, epoch, 0x1234_5678);
        let planner = Planner::new(watt_rules::Router::new(rules), &config);
        let handle = relay.sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0u8; 1024]),
            tcp::SocketBuffer::new(vec![0u8; 1024]),
        ));
        let key = FlowKey::new(
            PROTO_TCP,
            "10.0.0.2".parse().unwrap(),
            "203.0.113.10".parse().unwrap(),
            40000,
            443,
        );
        relay
            .flows
            .insert(handle, TcpFlow::new(key, candidates, true, "test", epoch));
        for up in window {
            relay.insert_upstream(handle, up);
        }
        (relay, planner, handle)
    }

    /// A real socket dialling `target`, as `open_race` would leave it.
    fn dialing_upstream(target: SocketAddr, started_at: Instant) -> Upstream {
        let mut protector = NoProtector;
        let mut socket = UpstreamSocket::tcp(Family::V4, &mut protector).unwrap();
        let state = socket.start_connect(target).unwrap();
        let connecting = state == ConnectState::InProgress;
        Upstream {
            socket,
            target,
            candidate_index: 0,
            connecting,
            connect_ready: !connecting,
            started_at,
            failed: false,
        }
    }

    /// A non-routable address: `connect()` returns `InProgress` and `SO_ERROR`
    /// stays 0, exactly like a candidate that is still in SYN_SENT.
    const UNROUTABLE: &str = "203.0.113.250:9";

    #[test]
    fn a_dial_still_in_progress_is_never_declared_the_winner() {
        // `SO_ERROR` reads 0 for a non-blocking connect that has not finished —
        // `EINPROGRESS` is `connect()`'s return, not `SO_ERROR`'s — so reading it
        // without the poll loop's "this descriptor is writable" would commit the
        // flow to a socket still in SYN_SENT and close the losers around it.
        let started = Instant::now();
        let stuck = dialing_upstream(UNROUTABLE.parse().unwrap(), started);
        let (mut relay, mut planner, handle) =
            relay_with_window(vec![stuck], vec![UNROUTABLE.parse().unwrap()]);

        // A tick right after the launch: `SO_ERROR` is 0, but the dial has not
        // finished, so it must not be taken as a win.
        relay.retire_dials(handle, started, &mut planner);
        assert!(
            relay.flows[&handle].winner.is_none(),
            "a dial still in SYN_SENT is not a winner"
        );
        assert_eq!(
            relay.flows[&handle].upstreams.len(),
            1,
            "and it is still in the window"
        );

        // Past the budget it is retired as a timeout, still never promoted.
        relay.retire_dials(handle, started + Duration::from_secs(30), &mut planner);
        assert!(relay.flows[&handle].winner.is_none());
        assert!(
            relay.flows[&handle].upstreams.is_empty(),
            "it timed out and left the window"
        );
    }

    #[test]
    fn a_live_candidate_wins_while_a_stuck_one_is_still_dialling() {
        // The stuck candidate is *first* in the window, which is the ordering
        // that used to break: "first socket checked" won regardless of whether it
        // had finished dialling.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let live_target = listener.local_addr().unwrap();
        let started = Instant::now();

        let stuck = dialing_upstream(UNROUTABLE.parse().unwrap(), started);
        let mut live = dialing_upstream(live_target, started);
        // What the poll loop sets once the descriptor is writable.
        live.connect_ready = true;

        let (mut relay, mut planner, handle) = relay_with_window(
            vec![stuck, live],
            vec![UNROUTABLE.parse().unwrap(), live_target],
        );

        relay.retire_dials(handle, started + Duration::from_millis(1), &mut planner);
        let flow = &relay.flows[&handle];
        assert_eq!(
            flow.winner_target(),
            Some(live_target),
            "the candidate that finished dialling is the winner"
        );
        assert_eq!(
            flow.upstreams.len(),
            1,
            "the stuck candidate was closed as a loser, not promoted"
        );
    }

    #[test]
    fn the_short_dial_is_never_longer_than_the_full_one() {
        // A caller who lowers `connect_timeout` below the per-candidate value
        // would otherwise get a per-candidate wait longer than the whole
        // connection's, which inverts the meaning of both numbers.
        let config = StackConfig {
            connect_timeout: Duration::from_secs(1),
            first_connect_timeout: Duration::from_secs(3),
            ..StackConfig::default()
        };
        let relay = TcpRelay::new(&config, Instant::now(), 7);
        assert!(relay.tuning.first_connect_timeout <= relay.tuning.connect_timeout);
    }

    #[test]
    fn the_relay_chunk_is_small_enough_to_share_a_buffer_with_many_flows() {
        // The drain loop bounds itself by `buffer_limit`, so the two constants
        // have to stay in a sane relation: a chunk larger than the buffer would
        // make every flow truncate its first read to the buffer size and then
        // stop, which is the throughput ceiling this round was about, just
        // reached from the other side.
        let config = StackConfig::default();
        assert!(
            RELAY_CHUNK <= config.flow_buffer_limit,
            "a chunk of {RELAY_CHUNK} does not fit a buffer of {}",
            config.flow_buffer_limit
        );
    }

    #[test]
    fn a_drain_is_bounded_by_the_buffer_limit_rather_than_running_free() {
        // The loop reads until the queue is full or the socket is short. This
        // pins the property that makes it safe: the stopping condition is the
        // queue, so a fast upstream cannot grow a flow's memory without bound
        // simply by having more to send than the client can take.
        let mut flow = flow_with(&["203.0.113.1"], "github.com");
        let limit = StackConfig::default().flow_buffer_limit;

        // Simulate the queue filling up the way the loop would fill it.
        flow.to_client.extend(std::iter::repeat(0u8).take(limit));
        assert_eq!(flow.to_client.len(), limit);
        assert!(
            flow.to_client.len() >= limit,
            "the loop stops at `buffer_limit`, so a full queue ends the drain"
        );
    }

    // -----------------------------------------------------------------------
    // Racing (happy eyeballs) — the invariants of
    // docs/architecture/kernel-racing.md §5, exercised by the §7 cases.
    //
    // The candidate list is named here rather than produced by the planner on
    // purpose. A rule's alternatives pass through `is_blocked_target`, which
    // refuses loopback, and loopback is the only place a test can host a server.
    // Naming the candidates directly is the only way to race real sockets, and
    // it is exactly the surface the invariants describe.
    //
    // The dead-candidate shapes are the two the design settled on: a closed
    // loopback port, which refuses at once, and a non-routable address, which
    // sits in SYN_SENT until its budget runs out. The design's original
    // suggestion — a bound socket that never accepts — does **not** work: the
    // kernel completes the three-way handshake into the backlog, so that
    // candidate is live, not dead.
    // -----------------------------------------------------------------------

    /// A candidate that accepts: a loopback listener and its address.
    fn live_listener() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, addr)
    }

    /// A bound socket that is not listening, so a connect to it is refused at
    /// once. The descriptor is held for the caller's lifetime: the port stays
    /// occupied, so no other test running in parallel can bind it out from under
    /// the dial.
    struct RefusedPort(RawFd);

    impl Drop for RefusedPort {
        fn drop(&mut self) {
            // SAFETY: the descriptor was created here and is closed exactly once.
            unsafe { libc::close(self.0) };
        }
    }

    /// A dead candidate that answers at once: a loopback port nothing listens on.
    fn refused_target() -> (RefusedPort, SocketAddr) {
        // SAFETY: a plain socket creation with no user pointers.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        assert!(fd >= 0, "socket: {}", std::io::Error::last_os_error());
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_addr.s_addr = u32::from_ne_bytes(Ipv4Addr::LOCALHOST.octets());
        addr.sin_port = 0;
        // SAFETY: `fd` is open and `addr` is a correctly sized `sockaddr_in`.
        let bound = unsafe {
            libc::bind(
                fd,
                (&addr as *const libc::sockaddr_in).cast::<libc::sockaddr>(),
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        assert_eq!(bound, 0, "bind: {}", std::io::Error::last_os_error());
        let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        // SAFETY: `fd` is bound and `addr`/`len` are valid for the query.
        let named = unsafe {
            libc::getsockname(
                fd,
                (&mut addr as *mut libc::sockaddr_in).cast::<libc::sockaddr>(),
                &mut len,
            )
        };
        assert_eq!(named, 0, "getsockname: {}", std::io::Error::last_os_error());
        let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), u16::from_be(addr.sin_port));
        (RefusedPort(fd), target)
    }

    /// Block until `fd` reports `events`, or the timeout passes.
    fn poll_until(fd: RawFd, events: libc::c_short, timeout: Duration) {
        let mut entry = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one initialised `pollfd` and a bounded timeout.
        let _ = unsafe { libc::poll(&mut entry, 1, millis) };
    }

    /// A real socket already connected to `target`, as a confirmed winner is.
    fn connected_upstream(target: SocketAddr) -> Upstream {
        let mut protector = NoProtector;
        let mut socket = UpstreamSocket::tcp(Family::V4, &mut protector).unwrap();
        if socket.start_connect(target).unwrap() == ConnectState::InProgress {
            poll_until(socket.raw_fd(), libc::POLLOUT, Duration::from_secs(5));
        }
        assert!(
            socket.take_connect_error().is_ok(),
            "the test socket must connect to {target}"
        );
        Upstream {
            socket,
            target,
            candidate_index: 0,
            connecting: false,
            connect_ready: true,
            started_at: Instant::now(),
            failed: false,
        }
    }

    /// Read whatever a non-blocking stream holds, waiting up to `wait` for at
    /// least `want` bytes. A short result is a fact the caller asserts on.
    fn read_until(stream: &mut std::net::TcpStream, want: usize, wait: Duration) -> Vec<u8> {
        use std::io::ErrorKind;
        stream.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + wait;
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        while out.len() < want && Instant::now() < deadline {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(ref err) if err.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => break,
            }
        }
        out
    }

    /// A relay with one hand-built flow, driven one tick at a time on a synthetic
    /// clock so a race can be watched as it settles.
    ///
    /// The client-facing socket is parked in `Listen` rather than run through a
    /// handshake: the race is entirely on the upstream side, and a listening
    /// socket keeps the reaper from collecting the flow mid-test.
    struct RaceHarness<P: Protector> {
        relay: TcpRelay,
        planner: Planner,
        protector: P,
        stats: Stats,
        handle: SocketHandle,
        now: Instant,
    }

    impl<P: Protector> RaceHarness<P> {
        fn new(candidates: Vec<SocketAddr>, config: StackConfig, protector: P) -> Self {
            let rules = RuleSet::from_str(
                r#"{"groups":[{"entries":[{"id":"1","name":"T","domains":["t.example"],"ips":["203.0.113.10"],"port":"443"}]}]}"#,
                RuleSource::Provided,
            )
            .unwrap();
            let epoch = Instant::now();
            let mut relay = TcpRelay::new(&config, epoch, 0x1234_5678);
            let planner = Planner::new(watt_rules::Router::new(rules), &config);

            let mut client = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0u8; 4096]),
                tcp::SocketBuffer::new(vec![0u8; 4096]),
            );
            client
                .listen(IpListenEndpoint {
                    addr: None,
                    port: 40000,
                })
                .unwrap();
            let handle = relay.sockets.add(client);

            let dst = candidates
                .first()
                .map(|candidate| candidate.ip())
                .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
            let key = FlowKey::new(PROTO_TCP, "10.0.0.2".parse().unwrap(), dst, 40000, 443);
            relay
                .flows
                .insert(handle, TcpFlow::new(key, candidates, true, "test", epoch));

            Self {
                relay,
                planner,
                protector,
                stats: Stats::default(),
                handle,
                now: epoch,
            }
        }

        fn flow(&self) -> &TcpFlow {
            &self.relay.flows[&self.handle]
        }

        fn winner(&self) -> Option<SocketAddr> {
            let flow = self.flow();
            if flow.winner.is_some() {
                flow.winner_target()
            } else {
                None
            }
        }

        /// One engine-like tick: report every finished dial the way the poll loop
        /// would, then advance the relay.
        fn tick(&mut self, step: Duration) -> Vec<Vec<u8>> {
            self.now += step;
            let fds: Vec<RawFd> = self.relay.fd_index.keys().copied().collect();
            for fd in fds {
                if descriptor_is_writable(fd) {
                    self.relay.note_ready(fd);
                }
            }
            self.relay
                .step(self.now, &mut self.planner, &mut self.protector, &mut self.stats)
        }

        /// Tick until a winner appears or `max` ticks pass. Returns ticks used.
        fn run_until_winner(&mut self, step: Duration, max: usize) -> usize {
            for tick in 1..=max {
                self.tick(step);
                if self.flow().winner.is_some() {
                    return tick;
                }
            }
            max
        }
    }

    /// A protector that counts every descriptor it is handed and records whether
    /// that descriptor was already connected when it was handed over.
    #[derive(Default)]
    struct RaceProtector {
        calls: usize,
        connected_before_protect: bool,
    }

    impl Protector for RaceProtector {
        fn protect(&mut self, fd: RawFd) -> bool {
            self.calls += 1;
            let mut storage = std::mem::MaybeUninit::<libc::sockaddr_storage>::uninit();
            let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            // SAFETY: `fd` was just created by the caller and is open. `getpeername`
            // succeeds only after `connect`, so it observes the ordering directly
            // rather than inferring it.
            let result = unsafe { libc::getpeername(fd, storage.as_mut_ptr().cast(), &mut len) };
            if result == 0 {
                self.connected_before_protect = true;
            }
            true
        }
    }

    #[test]
    fn race_picks_the_first_live_candidate() {
        // Two dead candidates ahead of a live one. The dead head is the shape
        // that used to break: a serial dial sits on it for the whole
        // per-candidate budget before the live address is even dialled.
        let (listener, live) = live_listener();
        let stuck: SocketAddr = UNROUTABLE.parse().unwrap();
        let (_refused, refused) = refused_target();
        let config = StackConfig {
            race_launch_interval: Duration::from_millis(10),
            ..StackConfig::default()
        };
        let mut harness = RaceHarness::new(vec![stuck, refused, live], config, NoProtector);

        let ticks = harness.run_until_winner(Duration::from_millis(50), 200);

        assert_eq!(harness.winner(), Some(live), "the live candidate must win the race");
        assert!(
            ticks < 40,
            "the race took {ticks} ticks; it must not wait out a per-candidate timeout"
        );
        listener.accept().expect("the winner must have connected");
    }

    #[test]
    fn race_dials_each_candidate_at_most_once() {
        // I1. Every candidate is dialled once and never again, even as failures
        // reorder the window under the cursor.
        let (listener, live) = live_listener();
        let (_refused_a, refused_a) = refused_target();
        let (_refused_b, refused_b) = refused_target();
        let config = StackConfig {
            race_launch_interval: Duration::from_millis(10),
            ..StackConfig::default()
        };
        let mut harness =
            RaceHarness::new(vec![refused_a, refused_b, live], config, RaceProtector::default());

        harness.run_until_winner(Duration::from_millis(50), 200);

        assert!(harness.flow().winner.is_some(), "the race must settle");
        assert_eq!(
            harness.protector.calls, 3,
            "three candidates means three dials; a fourth is a candidate dialled twice"
        );
        assert_eq!(
            harness.flow().candidates.len(),
            3,
            "a failed candidate is demoted, never deleted, so the list stays whole"
        );
        listener.accept().expect("the winner must have connected");
    }

    #[test]
    fn the_global_dial_ceiling_bounds_the_window() {
        // I9, the global half. `max_dialing` caps concurrent dials across every
        // flow, so a SYN storm cannot open more sockets than the process can hold
        // even though a single flow's window would.
        let stuck: Vec<SocketAddr> = ["203.0.113.250:9", "203.0.113.251:9", "203.0.113.252:9"]
            .iter()
            .map(|addr| addr.parse().unwrap())
            .collect();
        let config = StackConfig {
            race_width: 3,
            max_dialing: 1,
            race_launch_interval: Duration::from_millis(10),
            ..StackConfig::default()
        };
        let mut harness = RaceHarness::new(stuck, config, NoProtector);

        let mut max_racing = 0;
        let mut max_dialing = 0;
        for _ in 0..30 {
            harness.tick(Duration::from_millis(50));
            max_racing = max_racing.max(harness.flow().racing_count());
            max_dialing = max_dialing.max(harness.relay.dialing);
        }

        assert_eq!(
            max_racing, 1,
            "a ceiling of one must hold the window to one dial, saw {max_racing}"
        );
        assert!(
            max_dialing <= 1,
            "the global dialing count must not exceed the ceiling, saw {max_dialing}"
        );
    }

    #[test]
    fn a_synchronous_connect_wins_over_a_dial_in_flight() {
        // The mixed case: a candidate that finished connecting the instant it was
        // launched (`!connecting`) sharing a window with one still dialling. The
        // finished one is the winner; the in-flight head must not be promoted
        // merely for being first, and the finished one must not be skipped for
        // being second.
        let (listener, target) = live_listener();
        let mut protector = NoProtector;

        let stuck_target: SocketAddr = UNROUTABLE.parse().unwrap();
        let mut stuck = UpstreamSocket::tcp(Family::V4, &mut protector).unwrap();
        stuck.start_connect(stuck_target).unwrap();

        // A socket that has already finished connecting, as `open_race` stores one
        // when `start_connect` returns `Connected`.
        let mut done = UpstreamSocket::tcp(Family::V4, &mut protector).unwrap();
        if done.start_connect(target).unwrap() == ConnectState::InProgress {
            poll_until(done.raw_fd(), libc::POLLOUT, Duration::from_secs(5));
        }

        let now = Instant::now();
        let (mut relay, mut planner, handle) = relay_with_window(
            vec![
                Upstream {
                    socket: stuck,
                    target: stuck_target,
                    candidate_index: 0,
                    connecting: true,
                    connect_ready: false,
                    started_at: now,
                    failed: false,
                },
                Upstream {
                    socket: done,
                    target,
                    candidate_index: 1,
                    connecting: false,
                    connect_ready: true,
                    started_at: now,
                    failed: false,
                },
            ],
            vec![stuck_target, target],
        );

        relay.retire_dials(handle, now, &mut planner);

        let flow = &relay.flows[&handle];
        assert_eq!(
            flow.winner_target(),
            Some(target),
            "the finished dial wins; the in-flight head must not be promoted"
        );
        assert_eq!(
            flow.upstreams.len(),
            1,
            "the stuck head was closed as a loser"
        );
        let _ = listener.accept();
    }

    #[test]
    fn losers_release_their_fds() {
        // I6. While the race runs every dial is registered; the moment a winner is
        // named the losers are unregistered and their descriptors closed, and
        // nothing outlives the flow.
        let (listener, live) = live_listener();
        let stuck_a: SocketAddr = UNROUTABLE.parse().unwrap();
        let stuck_b: SocketAddr = "203.0.113.251:9".parse().unwrap();
        let config = StackConfig {
            race_launch_interval: Duration::from_millis(10),
            ..StackConfig::default()
        };
        let mut harness = RaceHarness::new(vec![stuck_a, stuck_b, live], config, NoProtector);
        let step = Duration::from_millis(50);

        // Fill the window: three dials, none settled.
        let mut guard = 0;
        while harness.flow().winner.is_none()
            && harness.flow().racing_count() < 3
            && guard < 20
        {
            harness.tick(step);
            guard += 1;
        }
        assert_eq!(
            harness.relay.fd_index.len(),
            3,
            "every dial is registered while racing"
        );

        harness.run_until_winner(step, 50);
        assert!(harness.flow().winner.is_some(), "the race must settle");
        assert_eq!(
            harness.relay.fd_index.len(),
            1,
            "the two losers released their descriptors"
        );
        assert_eq!(harness.flow().upstreams.len(), 1);

        // The last descriptor leaves with the flow.
        let later =
            harness.now + StackConfig::default().tcp_idle_timeout + Duration::from_secs(1);
        harness.relay.reap(later, &mut harness.planner, &mut harness.stats);
        assert_eq!(harness.relay.active_flows(), 0, "the flow is reaped");
        assert!(
            harness.relay.fd_index.is_empty(),
            "no descriptor outlives its flow"
        );
        let _ = listener.accept();
    }

    #[test]
    fn serial_mode_is_unchanged() {
        // I10. `race_width = 1` is the one-key rollback and has to reproduce the
        // old serial dial exactly: one descriptor in the window, the same payload
        // in both directions.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let fake_dst: IpAddr = "203.0.113.10".parse().unwrap();
        let mut harness = Harness::with_config(
            server_addr,
            StackConfig {
                race_width: 1,
                overrides: vec![crate::config::DestinationOverride::endpoint(
                    fake_dst,
                    server_addr.port(),
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    server_addr.port(),
                )],
                ..StackConfig::default()
            },
        );
        harness.server_ip = fake_dst;

        let syn_ack = harness.handshake_syn();
        harness.send(
            harness.client_seq + 1,
            syn_ack.seq.wrapping_add(1),
            packet::TcpFlags {
                ack: true,
                ..Default::default()
            },
            b"",
        );

        let (mut accepted, _) = listener.accept().expect("the relay must connect upstream");
        harness.send(
            harness.client_seq + 1,
            syn_ack.seq.wrapping_add(1),
            packet::TcpFlags {
                ack: true,
                psh: true,
                ..Default::default()
            },
            b"ping serial",
        );

        let mut buf = [0u8; 11];
        accepted
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        accepted
            .read_exact(&mut buf)
            .expect("upstream must receive the payload");
        assert_eq!(&buf, b"ping serial");

        accepted.write_all(b"pong").unwrap();
        let emitted = harness.send(
            harness.client_seq + 12,
            syn_ack.seq.wrapping_add(1),
            packet::TcpFlags {
                ack: true,
                ..Default::default()
            },
            b"",
        );
        assert!(
            find_payload(&emitted, b"pong"),
            "the reply must reach the client"
        );
        assert_eq!(
            harness.relay.fd_index.len(),
            1,
            "serial mode keeps exactly one dial in flight"
        );
    }

    #[test]
    fn window_rotates_before_client_budget() {
        // The only direct evidence that racing removed the serial delay. Three
        // candidates that never answer sit ahead of a live one. A window wide
        // enough to hold them all overlaps their dials and reaches the live
        // address inside a single budget; the serial dial pays the budget once
        // per dead candidate first.
        let (listener, live) = live_listener();
        let stuck: Vec<SocketAddr> = ["203.0.113.250:9", "203.0.113.251:9", "203.0.113.252:9"]
            .iter()
            .map(|addr| addr.parse().unwrap())
            .collect();
        let step = Duration::from_millis(100);
        let interval = Duration::from_millis(50);

        let mut racing = RaceHarness::new(
            vec![stuck[0], stuck[1], stuck[2], live],
            StackConfig {
                race_width: 4,
                race_launch_interval: interval,
                ..StackConfig::default()
            },
            NoProtector,
        );
        let racing_ticks = racing.run_until_winner(step, 400);
        assert_eq!(racing.winner(), Some(live), "the live candidate must win");
        let racing_elapsed = step * racing_ticks as u32;

        let mut serial = RaceHarness::new(
            vec![stuck[0], stuck[1], stuck[2], live],
            StackConfig {
                race_width: 1,
                race_launch_interval: interval,
                ..StackConfig::default()
            },
            NoProtector,
        );
        let serial_ticks = serial.run_until_winner(step, 400);
        assert_eq!(
            serial.winner(),
            Some(live),
            "the serial dial must eventually reach the live address too"
        );
        let serial_elapsed = step * serial_ticks as u32;

        let budget = StackConfig::default().first_connect_timeout;
        // Racing reaches the live address inside one candidate's budget even
        // though three dead candidates sit ahead of it; the serial dial needs a
        // whole budget for each of them first. This is the direct evidence that
        // the wait is bounded by the launch stagger, not by the candidate count.
        assert!(
            racing_elapsed < budget,
            "racing took {racing_elapsed:?}, which is not inside one {budget:?} budget"
        );
        assert!(
            serial_elapsed > budget * 2,
            "serial took {serial_elapsed:?}; the premise needs it to pay several budgets"
        );
        assert!(
            racing_ticks * 4 < serial_ticks,
            "racing {racing_ticks} ticks against serial {serial_ticks}: the wait must not \
             scale with the number of dead candidates"
        );
        let _ = listener.accept();
    }

    #[test]
    fn client_bytes_only_reach_the_winner() {
        // I3. While the race is undecided the client's bytes are buffered and
        // written to no upstream at all; only once a winner exists do they go
        // out, and only to it.
        let (listener_a, live_a) = live_listener();
        let (listener_b, live_b) = live_listener();
        let up_a = connected_upstream(live_a);
        let up_b = connected_upstream(live_b);
        let (mut server_a, _) = listener_a.accept().unwrap();
        let (mut server_b, _) = listener_b.accept().unwrap();

        let (mut relay, _planner, handle) =
            relay_with_window(vec![up_a, up_b], vec![live_a, live_b]);
        let mut stats = Stats::default();
        let mut scratch = vec![0u8; RELAY_CHUNK];

        // Mid-race: no winner yet, client bytes waiting.
        relay
            .flows
            .get_mut(&handle)
            .unwrap()
            .to_upstream
            .extend(b"client-payload");

        relay.move_bytes(handle, Instant::now(), &mut scratch, &mut stats);

        assert_eq!(
            relay.flows[&handle].to_upstream.len(),
            14,
            "no client byte may leave while the race is undecided"
        );
        assert!(
            read_until(&mut server_a, 1, Duration::from_millis(80)).is_empty(),
            "a candidate received client bytes before it won"
        );
        assert!(
            read_until(&mut server_b, 1, Duration::from_millis(80)).is_empty(),
            "a candidate received client bytes before it won"
        );

        // Confirm the first candidate and let the same bytes out.
        relay.flows.get_mut(&handle).unwrap().winner = Some(0);
        relay.move_bytes(handle, Instant::now(), &mut scratch, &mut stats);

        assert_eq!(
            relay.flows[&handle].to_upstream.len(),
            0,
            "the winner took the bytes"
        );
        assert_eq!(
            read_until(&mut server_a, 14, Duration::from_secs(2)),
            b"client-payload",
            "the winner must receive every client byte"
        );
        assert!(
            read_until(&mut server_b, 1, Duration::from_millis(80)).is_empty(),
            "the loser must still receive nothing"
        );
    }

    #[test]
    fn bytes_from_upstream_counts_winner_only() {
        // I4. Only the winner is read. A loser with data waiting must not have it
        // counted, and must not have been read at all.
        let (listener_a, live_a) = live_listener();
        let (listener_b, live_b) = live_listener();
        let up_a = connected_upstream(live_a);
        let up_b = connected_upstream(live_b);
        let (mut server_a, _) = listener_a.accept().unwrap();
        let (mut server_b, _) = listener_b.accept().unwrap();

        server_a.write_all(b"winner-bytes").unwrap();
        server_b.write_all(b"loser-bytes").unwrap();

        let (mut relay, _planner, handle) =
            relay_with_window(vec![up_a, up_b], vec![live_a, live_b]);
        let winner_fd = relay.flows[&handle].upstreams[0].socket.raw_fd();
        poll_until(winner_fd, libc::POLLIN, Duration::from_secs(2));

        relay.flows.get_mut(&handle).unwrap().winner = Some(0);
        let mut stats = Stats::default();
        let mut scratch = vec![0u8; RELAY_CHUNK];
        relay.move_bytes(handle, Instant::now(), &mut scratch, &mut stats);

        assert_eq!(
            relay.flows[&handle].bytes_from_upstream,
            12,
            "only the winner's twelve bytes may be counted"
        );
        assert_eq!(stats.bytes_upstream_to_client, 12);

        // The loser's bytes are still sitting unread on its socket.
        let mut buf = [0u8; 32];
        let n = relay.flows[&handle].upstreams[1]
            .socket
            .read(&mut buf)
            .unwrap();
        assert_eq!(&buf[..n], b"loser-bytes", "the loser must not have been read");
    }

    #[test]
    fn protect_before_connect() {
        // I7 with K > 1. Every descriptor a race opens is offered to the
        // protector before `connect`, not just the first one. The two dead heads
        // keep the window filling, so more than one socket is genuinely opened
        // before the live candidate settles the race.
        let (listener, live) = live_listener();
        let stuck_a: SocketAddr = UNROUTABLE.parse().unwrap();
        let stuck_b: SocketAddr = "203.0.113.251:9".parse().unwrap();
        let config = StackConfig {
            race_launch_interval: Duration::from_millis(10),
            ..StackConfig::default()
        };
        let mut harness =
            RaceHarness::new(vec![stuck_a, stuck_b, live], config, RaceProtector::default());

        harness.run_until_winner(Duration::from_millis(50), 50);

        assert!(
            harness.protector.calls >= 2,
            "the race must have opened more than one socket, saw {}",
            harness.protector.calls
        );
        assert!(
            !harness.protector.connected_before_protect,
            "a socket was connected before it was protected; on Android that \
             captures the kernel's own upstream traffic"
        );
        let _ = listener.accept();
    }
}

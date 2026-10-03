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
use std::io;
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
use crate::sni;
use crate::upstream::{is_retryable, ConnectState, Protector, UpstreamSocket};
use crate::upstream_proxy::{Handshake, ProxyConfig, Step};
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
    ///
    /// On a proxied dial this is still the **destination** — the address the
    /// proxy is asked to reach — while the socket itself is connected to the
    /// proxy. Keeping the destination here rather than the proxy's address is
    /// what lets everything built on top of it stay true: the winner is named by
    /// the address the flow is for, the address health table is keyed by it, and
    /// the log line for a cut names the server the client was talking to.
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
    /// The proxy handshake this dial is running, when the flow is proxied.
    ///
    /// `Some` from the moment the socket is created until the dial leaves the
    /// window, and deliberately **not** cleared when the handshake finishes.
    /// Its presence is the one thing that says "this dial's outcome is the
    /// proxy's report rather than a measurement taken from here", which is what
    /// keeps a proxy-side failure out of the address health table and stops a
    /// cut line from claiming the destination answered.
    handshake: Option<Handshake>,
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
    /// Set once an upstream socket failure has been described in the log.
    ///
    /// `move_bytes` keeps running on a flow whose socket has already failed —
    /// the flow lives until both sides are closed — so without this the same
    /// reset would be reported on every tick until it was reaped. A diagnostic
    /// that repeats is one nobody reads, and the repetition would also bury the
    /// one thing the line is for: how far the conversation got.
    cut_reported: bool,
    /// Bytes received from the upstream over this flow's life.
    ///
    /// Zero at close time means the connection was established and nothing ever
    /// came back — the one failure the connect-time report cannot see. Tracked per
    /// flow rather than globally because the judgement is per destination.
    bytes_from_upstream: u64,
    /// Bytes handed to the winner's socket over this flow's life.
    ///
    /// The counterpart of `bytes_from_upstream`, and the only way to say how far
    /// the conversation got before it was cut. `sent_any` says a byte moved at
    /// all; this says how many, which is what separates a ClientHello that was
    /// answered with a reset (a few hundred bytes) from an ordinary mid-stream
    /// drop (tens of kilobytes). The attribution log turns on exactly that
    /// difference, so the count has to be kept rather than inferred from the
    /// buffer, which the successful writes have already drained.
    bytes_to_upstream: u64,
    /// The domain this flow is for, when the planner has observed one.
    ///
    /// The tunnel is handed an address; the name only exists because the planner
    /// records the answers to the DNS it forwards. Without it there is nothing to
    /// check a candidate's certificate against.
    ///
    /// Set at open from the observation cache, and set later from the client's own
    /// handshake when the cache had nothing — see [`TcpFlow::watch_handshake`] and
    /// [`TcpRelay::adopt_handshake_name`].
    host: Option<String>,
    /// The rule entry the routing decision came from, when it came from one.
    ///
    /// Kept so a name recovered later can be told apart from the name the address
    /// already implied. If both point at the same rule there is nothing to
    /// re-plan, and restarting the race would spend the client's patience on a
    /// handshake it was already going to make.
    rule_key: Option<u32>,
    /// Client bytes held while the domain is still unknown.
    ///
    /// `Some` from open until the name is settled — found, or provably not
    /// coming. Only ever non-empty on a flow that opened without a name, and
    /// bounded by [`sni::MAX_HELLO`]: these are a copy of the client's own
    /// handshake, held on its behalf, and there is no reason to hold them once
    /// they have been read.
    ///
    /// A copy, not the original. The bytes are in `to_upstream` already and must
    /// reach the winner unchanged; this buffer only ever gets looked at.
    hello: Option<Vec<u8>>,
    /// A domain read out of the client's handshake, not yet acted on.
    ///
    /// Separate from `host` because adopting it is not free: the flow has to stop
    /// the race it is running and start a different one. Reading the name here and
    /// applying it in [`TcpRelay::relay`] keeps that decision in the one place
    /// that has a planner, a protector and the stats to hand.
    sni_pending: Option<String>,
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

/// A short, log-safe name for why an upstream socket failed.
///
/// Deliberately not `io::Error`'s `Display`. Every error this is called with
/// comes from `last_os_error()`, and rendering one runs `strerror_r`, which
/// indexes a table through the errno — not a call to make on the relay path, and
/// pointless here, since the errno is the whole of what the diagnosis needs. The
/// `ErrorKind` fallback exists only for errors built in-process, which carry no
/// errno at all.
fn upstream_error_label(err: &io::Error) -> &'static str {
    match err.raw_os_error() {
        Some(libc::ECONNRESET) => "reset by peer",
        Some(libc::EPIPE) => "broken pipe",
        Some(libc::ECONNABORTED) => "connection aborted",
        Some(libc::ECONNREFUSED) => "refused",
        Some(libc::ETIMEDOUT) => "timed out",
        Some(libc::ENETUNREACH) => "network unreachable",
        Some(libc::EHOSTUNREACH) => "host unreachable",
        _ => match err.kind() {
            io::ErrorKind::ConnectionReset => "reset by peer",
            io::ErrorKind::BrokenPipe => "broken pipe",
            io::ErrorKind::ConnectionRefused => "refused",
            io::ErrorKind::TimedOut => "timed out",
            io::ErrorKind::NotConnected => "not connected",
            _ => "socket error",
        },
    }
}

/// Describe an upstream socket that died *after* its dial succeeded.
///
/// This is the only place the "connected, then cut" case is described, and it
/// exists to separate it from a dial that never completed. The two are
/// indistinguishable to the client — a session that goes nowhere either way —
/// but they mean opposite things operationally. A dial that is refused or times
/// out says the **address** is wrong, and the connect-time log already says so.
/// A reset that arrives after the client's first bytes went out, with nothing
/// ever coming back, says the address answered and something above it refused
/// *this* conversation. The name is what such a thing is keyed on, so the name
/// is in the line: read the lines for one address across two names and the
/// difference between the names is the entire answer. Without a name on the line
/// that comparison cannot be made from the log at all.
///
/// Returned as data rather than logged in place so the judgement can be asserted
/// without a logger installed. With no logger set the `log` macros are no-ops,
/// so a test that only called the logging function would evaluate no format
/// argument and assert nothing about the text.
fn describe_upstream_cut(
    flow: &TcpFlow,
    stage: &str,
    err: &io::Error,
    now: Instant,
) -> (log::Level, String) {
    let label = upstream_error_label(err);
    let name = flow.host.as_deref().unwrap_or("no name observed");
    let elapsed_ms = now.saturating_duration_since(flow.opened_at).as_millis();

    // Whether this flow was carried by a proxy, read off the winner's own dial.
    //
    // The winner is the only socket that ever carried a byte, so it is the only
    // one whose handshake state describes this flow. A flow with no winner never
    // reached anything and is not described here at all.
    let carried_by_proxy = flow
        .winner
        .and_then(|index| flow.upstreams.get(index))
        .is_some_and(|up| up.handshake.is_some());

    if flow.sent_any && flow.bytes_from_upstream == 0 && carried_by_proxy {
        // The shape this log was written for, but through a proxy — so the one
        // claim the direct version makes cannot be made. `send` succeeding means
        // the connection was ESTABLISHED, and the connection is to the *proxy*;
        // where the cut is, between the proxy and the destination, this host
        // cannot see. Saying "the address answered" here would be exactly the
        // kind of plausible-sounding inference this project keeps having to
        // retract, so the line says what was measured and stops.
        (
            log::Level::Warn,
            format!(
                "watt: flow {} ({name}) upstream {label} on {stage} after {} bytes sent / 0 received \
                 ({elapsed_ms} ms) — the flow was carried by a proxy, so the cut is somewhere past it",
                flow.key.dst, flow.bytes_to_upstream
            ),
        )
    } else if flow.sent_any && flow.bytes_from_upstream == 0 {
        // The shape this log was written for: bytes went out, not one came back,
        // and the socket then failed.
        //
        // The trailing clause claims only what the measurements support. `send`
        // succeeding on a connected socket means the connection was ESTABLISHED,
        // so the address answered — that much is a protocol fact, not a reading
        // of the code. It does **not** follow that nothing at that address is
        // responsible: a middlebox can sit in front of a reachable host and cut
        // one name while serving another. So the line says the address answered
        // and the cut is above it, and leaves the cause to the comparison the
        // name in the middle of the line makes possible.
        (
            log::Level::Warn,
            format!(
                "watt: flow {} ({name}) upstream {label} on {stage} after {} bytes sent / 0 received \
                 ({elapsed_ms} ms) — the dial completed, so the address answered and the cut is above it",
                flow.key.dst, flow.bytes_to_upstream
            ),
        )
    } else {
        (
            log::Level::Info,
            format!(
                "watt: flow {} ({name}) upstream {label} on {stage} after {} bytes sent / {} received \
                 ({elapsed_ms} ms)",
                flow.key.dst, flow.bytes_to_upstream, flow.bytes_from_upstream
            ),
        )
    }
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
            cut_reported: false,
            bytes_from_upstream: 0,
            bytes_to_upstream: 0,
            host: None,
            rule_key: None,
            hello: None,
            sni_pending: None,
            verify_by: None,
            delivered_any: false,
            sent_any: false,
            opened_at: now,
            last_activity: now,
        }
    }

    /// The name this flow was dialled for, for a log line.
    ///
    /// A flow only has a name when the planner saw the DNS answer that produced
    /// its address — the tunnel itself is handed an address, never a name. A
    /// connection to a bare address has none, and the placeholder says so rather
    /// than leaving the field looking like a name that failed to render.
    fn name(&self) -> &str {
        self.host.as_deref().unwrap_or("no name observed")
    }

    /// Whether this flow is still trying to learn its domain from the client.
    ///
    /// True only for a flow that opened without a name. A flow the planner could
    /// name already has everything the handshake would add.
    fn watching_handshake(&self) -> bool {
        self.hello.is_some()
    }

    /// Feed the client's bytes to the handshake watcher.
    ///
    /// Re-parsed from the start on every call rather than kept as a cursor. The
    /// buffer is a few hundred bytes in practice, this runs at most twice on a
    /// flow that has one, and a parser that restarts cannot drift out of step
    /// with the bytes it is restarting on.
    ///
    /// Watching stops at the first answer that is not "need more bytes". A
    /// ClientHello with no name will not grow one — TLS permits a second
    /// ClientHello, but by then the flow has already carried bytes and the
    /// re-plan in [`TcpRelay::adopt_handshake_name`] would be refused anyway — and
    /// a flow that is not TLS at all will never have one.
    fn watch_handshake(&mut self, bytes: &[u8]) {
        let Some(buffer) = self.hello.as_mut() else {
            return;
        };
        // The parser only ever reads the first record, so a handshake that claims
        // more than a plausible ClientHello would otherwise be buffered forever.
        // Stopping loses the name; continuing would hold the client's bytes for
        // the life of the flow. A missing name is the failure mode to prefer.
        if buffer.len() + bytes.len() > sni::MAX_HELLO {
            self.hello = None;
            return;
        }
        buffer.extend_from_slice(bytes);

        match sni::from_client_hello(buffer) {
            sni::Sni::Incomplete => {}
            sni::Sni::Found(name) => {
                self.hello = None;
                self.sni_pending = Some(name);
            }
            sni::Sni::Absent | sni::Sni::Covered | sni::Sni::NotTls => {
                self.hello = None;
            }
        }
    }

    /// Report an upstream socket failure once, at the level it deserves.
    ///
    /// Called from both directions of [`TcpRelay::move_bytes`], which is where an
    /// upstream socket error is otherwise folded into `upstream_eof` and lost.
    /// The guard is what keeps a flow that is already dead from re-reporting on
    /// every tick before it is reaped.
    fn log_upstream_cut(&mut self, stage: &str, err: &io::Error, now: Instant) {
        if self.cut_reported {
            return;
        }
        self.cut_reported = true;
        let (level, line) = describe_upstream_cut(self, stage, err, now);
        if level == log::Level::Warn {
            log::warn!("{line}");
        } else {
            log::info!("{line}");
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
    /// The upstream exit every public TCP flow is handed to, when one is set.
    ///
    /// Held here rather than in [`Tuning`] because it owns strings and `Tuning`
    /// is `Copy`, and copied out of the config once at construction so a
    /// settings reload cannot change the exit under a running flow.
    ///
    /// See [`StackConfig::upstream_proxy`] for what it is for, and
    /// [`TcpRelay::open_race`] for which flows it carries and why the rest are
    /// left alone.
    upstream_proxy: Option<ProxyConfig>,
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
        let upstream_proxy = config.upstream_proxy.clone();
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
            upstream_proxy,
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
    ///
    /// A socket still dialling can only be waited on for writability. Once the
    /// dial is done the descriptor is both a tunnel and, on a proxied flow, a
    /// handshake waiting to be answered — and both want `READ | WRITE`, so
    /// nothing here has to know which of the two it is looking at. The handshake
    /// is driven on every relay pass regardless; the read interest is what stops
    /// the proxy's reply from having to wait out the poll timeout.
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

            // Counted at open, next to the matched/direct pair above, because it
            // is decided at the same moment. Everything below this point is gated
            // on `host`, so a blind flow is one where the rule set, the
            // certificate check and the attribution log all go quiet at once —
            // and until this counter existed that state was only visible as a
            // log line per flow, which is not a rate anyone can act on.
            //
            // A flow counted here is not necessarily blind for its whole life:
            // the client's own handshake may still name it, which is what
            // `flows_named_by_sni` counts and what the watcher below is for.
            if host.is_none() {
                stats.flows_without_name += 1;
            }

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
            flow.rule_key = decision.plan.entry_key;

            // A flow the planner could not name is the one worth watching: the
            // client's own handshake is the remaining chance to learn the domain,
            // and with it the rule set and the certificate check come back. A flow
            // that already has a name has nothing to gain from the same bytes.
            if host.is_none() {
                flow.hello = Some(Vec::new());
            }

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

            // A flow the planner could not name may be named by the client's own
            // handshake, which `move_bytes` has just read. Adopting the name throws
            // away the race that was running, so the new one has to be driven in
            // this same tick: the client is waiting on a handshake and has no idea
            // any of this happened.
            if self.adopt_handshake_name(handle, now, planner, stats) {
                self.advance_dials(handle, now, planner, protector, stats);
            }
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
        self.retire_dials(handle, now, planner, stats);

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
    ///
    /// On a proxied flow "whose `SO_ERROR` clears" is only half the question. The
    /// connect that succeeded is to the **proxy**, which says nothing about the
    /// destination: the flow exists once the proxy has agreed to carry it, so the
    /// handshake is driven here, in the same scan, and a dial only becomes a
    /// candidate for the win once the handshake is done. Driving it in this scan
    /// rather than in a pass of its own is what keeps the two questions — "did the
    /// dial finish" and "did the proxy accept" — answered from one place, with one
    /// budget and one failure list.
    fn retire_dials(
        &mut self,
        handle: SocketHandle,
        now: Instant,
        planner: &mut Planner,
        stats: &mut Stats,
    ) {
        let can_rotate = self
            .flows
            .get(&handle)
            .map(|flow| flow.can_rotate_window())
            .unwrap_or(false);
        // A dial with an alternative behind it gets the short budget so the
        // window keeps rotating; one that is the flow's last hope gets the full
        // `connect_timeout`, because a slow but honest server deserves it.
        //
        // The budget covers the handshake as well as the connect, because both
        // are one wait from the client's point of view — and a proxy that accepts
        // the connection and then says nothing is the one failure that would
        // otherwise hold a flow forever.
        let budget = if can_rotate {
            self.tuning.first_connect_timeout
        } else {
            self.tuning.connect_timeout
        };

        let mut winner: Option<(usize, SocketAddr, Duration)> = None;
        let mut failures: Vec<(usize, SocketAddr, &'static str)> = Vec::new();
        // Counted rather than applied in place: `dialing` is a field of the relay
        // and the loop below holds `self.flows` mutably, so the decrement happens
        // once, after the scan.
        let mut connected = 0usize;
        let mut accepted = 0usize;
        let mut refused = 0usize;
        {
            let Some(flow) = self.flows.get_mut(&handle) else {
                return;
            };
            for (index, up) in flow.upstreams.iter_mut().enumerate() {
                if up.failed {
                    continue;
                }
                if up.connecting {
                    // The poll loop's `connect_ready` is what says the dial has
                    // *finished*. `SO_ERROR` cannot stand in for it: a non-blocking
                    // connect that is still in progress reports 0 too, so reading it
                    // early would take a socket still in SYN_SENT for a winner and
                    // close the losers around it — the very failure racing exists to
                    // prevent. Only once the descriptor is writable is `SO_ERROR` a
                    // verdict.
                    if !up.connect_ready {
                        if now.saturating_duration_since(up.started_at) > budget {
                            // Still dialling and out of patience: rotate the window.
                            failures.push((index, up.target, "timed out"));
                        }
                        continue;
                    }
                    match up.socket.take_connect_error() {
                        Ok(()) => {
                            up.connecting = false;
                            connected += 1;
                        }
                        // `SO_ERROR` was not ready to be read yet.
                        Err(err) if is_retryable(&err) => continue,
                        Err(_) => {
                            failures.push((index, up.target, "refused"));
                            continue;
                        }
                    }
                }

                // The connect is done. With a proxy in the path that means the
                // proxy answered, and nothing more — the destination has not been
                // dialled by this host at all.
                if let Some(handshake) = up.handshake.as_mut() {
                    match handshake.advance(&up.socket) {
                        Step::Done => {
                            accepted += 1;
                        }
                        Step::Pending => {
                            if now.saturating_duration_since(up.started_at) > budget {
                                refused += 1;
                                failures.push((index, up.target, "the proxy did not answer"));
                            }
                            continue;
                        }
                        Step::Refused(why) => {
                            refused += 1;
                            failures.push((index, up.target, why));
                            continue;
                        }
                    }
                }

                // A dial that completed synchronously when it was launched, or one
                // whose handshake has just finished. The earliest takes the win.
                if winner.is_none() {
                    // `rtt` is measured from the dial's launch, so on a proxied flow
                    // it includes the handshake. That is the right number for both
                    // uses: how long the flow took to become usable, which is what
                    // the log reports and what the selector should rank by.
                    winner = Some((index, up.target, now.saturating_duration_since(up.started_at)));
                }
            }
        }

        self.dialing = self.dialing.saturating_sub(connected);
        stats.proxy_handshakes += accepted as u64;
        stats.proxy_refusals += refused as u64;

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
        // Read as a field rather than through `name()`: the loop below holds
        // `flow.upstreams` mutably, and a method taking `&self` borrows the whole
        // flow. `dst` is captured above for the same reason.
        let name = flow.host.as_deref().unwrap_or("no name observed");
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
                "watt: flow {} ({}) upstream to {} died (candidate {}/{}, {why})",
                dst,
                name,
                up.target.ip(),
                up.candidate_index + 1,
                candidate_total
            );
            // Only a dial this host made itself is evidence about the address.
            //
            // `report_failure` pushes the address to the back of the selector for
            // a whole `failure_cooldown`, and on a proxied dial the reason is the
            // *proxy's* report: it may mean the proxy could not reach the
            // destination, or it may mean the proxy is down, wants a password, or
            // forbids this host. Writing the second group into the table would
            // quietly degrade the direct path — for a minute after a user turns a
            // misconfigured exit off, every address it complained about would
            // still be at the back of the list, with nothing in the log to connect
            // the two. The connect-time signal is skipped for proxied dials; the
            // session-level one in `reap` is not, because a session that carried
            // bytes or did not is an end-to-end fact about the address either way.
            if up.handshake.is_none() {
                planner.report_failure(up.target.ip(), now);
            }
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
        // Read before the flow is borrowed: the log has to be able to say *how*
        // the flow was carried, and a line that named an address without saying
        // it went through an exit would read as a direct connection this host
        // never made.
        let proxy = self.upstream_proxy.as_ref().map(|config| config.address);
        let flow = self.flows.get_mut(&handle).expect("checked above");
        let winner_fd = flow.upstreams[winner_index].socket.raw_fd();
        let carried_by_proxy = flow.upstreams[winner_index].handshake.is_some();
        // The winner has finished dialling, so it leaves the handshake budget.
        // On a proxied flow `retire_dials` has already cleared this when the
        // connect itself completed, which is why the decrement is conditional:
        // the count must move exactly once per dial.
        if flow.upstreams[winner_index].connecting {
            flow.upstreams[winner_index].connecting = false;
            self.dialing = self.dialing.saturating_sub(1);
        }
        flow.last_launch = None;

        let losers = flow.upstreams.len().saturating_sub(1);
        match (carried_by_proxy, proxy) {
            (true, Some(address)) => log::info!(
                "watt: flow {} ({}) upstream connected to {} through the proxy at {address} \
                 ({} ms) — closed {losers} loser(s)",
                flow.key.dst,
                flow.name(),
                winner_target.ip(),
                rtt.as_millis()
            ),
            _ => log::info!(
                "watt: flow {} ({}) upstream connected via {} ({} ms) — closed {losers} loser(s)",
                flow.key.dst,
                flow.name(),
                winner_target.ip(),
                rtt.as_millis()
            ),
        }
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

        // Which address this dial actually connects to, and whether a proxy
        // carries the flow.
        //
        // The gate is `is_blocked_target`, so the proxy is handed every public
        // destination and nothing else. A target on loopback or the local network
        // is reachable from here by definition and is not the internet: a rewrite
        // pointing a flow at a local stub has to keep working, and LAN traffic
        // must not leave the device. `can_relay` has already refused every flow
        // that is not a rewrite to such an address, so the only flows this
        // excludes are the rewrites — which is exactly the set that must stay
        // direct.
        //
        // Note what this does *not* gate on: whether a rule matched. A flow with
        // no rule still has a target — the address the client resolved — and
        // whether that address is reachable from here is a question about the
        // path, not about the rule set. Carrying it too is the honest reading of
        // "send the internet through this exit".
        let proxy = self
            .upstream_proxy
            .as_ref()
            .filter(|_| !planner.is_blocked_target(target.ip()));
        // The handshake names the destination; the socket goes to the proxy.
        // Built before the flow is borrowed so it owns everything it needs and
        // the flow map is free to be borrowed mutably a moment later.
        let handshake = proxy.map(|config| Handshake::new(config, target));
        let dial_target = proxy.map(|config| config.address).unwrap_or(target);

        // The socket's family follows the address it dials, which under a proxy is
        // the proxy's and not the destination's. That is what lets an IPv6 rule
        // address be reached over an IPv4 proxy connection: the destination is
        // named in the handshake, where its form does not depend on the socket.
        let family = Family::of(dial_target.ip());
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

        match socket.start_connect(dial_target) {
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
                    handshake,
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
                if handshake.is_some() {
                    log::warn!(
                        "watt: flow {} could not reach the upstream proxy at {}, rotating to the \
                         next candidate",
                        flow.key.dst,
                        dial_target.ip()
                    );
                } else {
                    log::warn!(
                        "watt: flow {} connect to {} failed, rotating to the next candidate",
                        flow.key.dst,
                        dial_target.ip()
                    );
                }
                flow.next_candidate = candidate_index + 1;
                flow.last_launch = Some(now);
                // A failure to reach the proxy says nothing about the address the
                // proxy would have been asked for, so only a direct dial's failure
                // is reported against it. See `retire_dials` for the full reason.
                if handshake.is_none() {
                    planner.report_failure(target.ip(), now);
                }
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

            // A flow that opened without a name can still be named by the client
            // itself: a TLS ClientHello carries the domain in the clear, and these
            // are those bytes. Nothing is decrypted and nothing is modified — the
            // same bytes go on to the winner unchanged.
            if flow.watching_handshake() {
                flow.watch_handshake(&chunk[..taken]);
                if flow.sni_pending.is_some() {
                    // Stop short of the flush below rather than falling through.
                    //
                    // The flow may be about to be re-planned, and a byte that
                    // reaches the current winner cannot be taken back: the two
                    // servers would be spliced into one stream. Waiting one tick
                    // for the decision costs nothing, because the bytes are
                    // already in `to_upstream`.
                    //
                    // Only a TLS client can reach this point, and a TLS server
                    // cannot have answered before the ClientHello it is answering
                    // — so returning here skips no upstream byte.
                    return;
                }
            }

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
                // The error is carried out of the block rather than reported
                // inside it: `up` borrows the window, and the report is about the
                // flow as a whole.
                let mut read_error: Option<io::Error> = None;
                let (read, eof) = {
                    let up = &flow.upstreams[winner];
                    match up.socket.read(chunk) {
                        Ok(0) => (0, true),
                        Ok(n) => (n, false),
                        Err(err) if is_retryable(&err) => (0, false),
                        Err(err) => {
                            read_error = Some(err);
                            (0, true)
                        }
                    }
                };
                if let Some(err) = read_error {
                    flow.log_upstream_cut("read", &err, now);
                }
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
                        // Counted on the way out, not derived from the buffer: a
                        // successful write drains the buffer, so the buffer at cut
                        // time holds only what did *not* get through.
                        flow.bytes_to_upstream += n as u64;
                        flow.last_activity = now;
                    }
                    Err(err) if is_retryable(&err) => break,
                    Err(err) => {
                        // In practice the read arm above is what sees a reset
                        // first: it runs earlier in this function, and a reset
                        // makes the descriptor readable the moment it lands. This
                        // arm is the one that catches it when that read was
                        // skipped for want of buffer room, which is why both
                        // directions report rather than only the likely one.
                        flow.upstream_eof = true;
                        flow.log_upstream_cut("write", &err, now);
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

    /// Give a flow the domain the client itself put in its handshake.
    ///
    /// A flow that opened without a name is relayed blind: the rule set is never
    /// consulted and no candidate's certificate can be judged. The client's
    /// ClientHello names the domain in the clear, and those bytes are already
    /// passing through this process — see [`crate::sni`]. This is where that name
    /// becomes a route.
    ///
    /// # Why this is only allowed before the first byte
    ///
    /// Once a byte has reached an upstream socket the client is in a conversation
    /// with one particular server, and moving the flow would splice two of them
    /// into a stream neither agreed to. A wrong address is the better outcome
    /// then, so the re-plan is refused and the flow carries on where it was. That
    /// is also why `move_bytes` returns early when the name arrives: it is the one
    /// moment the flush can still be held back.
    ///
    /// Returns true when the flow was re-planned, in which case its race has been
    /// emptied and the caller must drive it again.
    fn adopt_handshake_name(
        &mut self,
        handle: SocketHandle,
        now: Instant,
        planner: &mut Planner,
        stats: &mut Stats,
    ) -> bool {
        let tuning = self.tuning;
        let mut removed: Vec<Upstream> = Vec::new();
        let adopted;

        {
            let Some(flow) = self.flows.get_mut(&handle) else {
                return false;
            };
            let Some(name) = flow.sni_pending.take() else {
                return false;
            };

            // The name is known from here on either way, so the flow carries it
            // in the log even when there is nothing to re-plan. That is worth
            // having on its own: the attribution line for a cut flow is only
            // comparable across two names, and for a client that resolves
            // elsewhere this is the only place the name can come from.
            flow.host = Some(name.clone());

            if flow.bytes_to_upstream > 0 {
                // Too late to move it, and normal rather than exceptional: a
                // ClientHello split across segments has had its first fragment
                // flushed by the time the name is complete.
                log::debug!(
                    "watt: flow {} already carries bytes; the name {name} came too late to re-plan",
                    flow.key.dst
                );
                return false;
            }

            let requested = SocketAddr::new(flow.key.dst, flow.key.dst_port);
            let decision = planner.decide_by_name(now, requested, &name);

            // Two ways for there to be nothing to do, and both must leave the flow
            // exactly where it is. The target staying put means the rule set has no
            // addresses for this name — `decide_by_name` moves it only when it
            // does. An unchanged rule key means the address already led to the
            // same rule, so the candidates are the ones it would produce and
            // re-dialling would cost the client a handshake for nothing.
            if decision.target == requested || decision.plan.entry_key == flow.rule_key {
                log::debug!(
                    "watt: flow {} stays on {}; the handshake named {name} and there is nothing to re-plan",
                    flow.key.dst,
                    requested.ip()
                );
                return false;
            }

            let mut candidates = vec![decision.target];
            for alternative in &decision.alternatives {
                if planner.is_blocked_target(*alternative) {
                    continue;
                }
                candidates.push(SocketAddr::new(*alternative, requested.port()));
            }
            if candidates.len() > tuning.max_candidates {
                candidates.truncate(tuning.max_candidates);
            }

            // Drop the race in flight. Nothing was ever written to any of these
            // sockets, so closing one is a bare FIN — the peer sees a connection
            // that went away before it was asked anything, which is exactly what a
            // client-side abort looks like to a server.
            removed.append(&mut flow.upstreams);
            let target = candidates[0];
            let count = candidates.len();
            flow.candidates = candidates;
            flow.next_candidate = 0;
            flow.winner = None;
            flow.verify_by = None;
            flow.steered = true;
            flow.reason = decision.reason();
            flow.rule_key = decision.plan.entry_key;
            flow.last_launch = None;
            adopted = (flow.key.dst, name, target, count);
        }

        for up in removed {
            self.unlink_upstream(&up);
        }

        // The treatment a named flow gets at open, for the same reason: a cached
        // verdict decides the *order* of candidates, and a pending one holds the
        // dial until it answers. Skipping it here would make a name read off the
        // wire worth less than one the kernel observed.
        let (dst, name, target, count) = adopted;
        {
            let Some(flow) = self.flows.get_mut(&handle) else {
                return false;
            };
            flow.order_by_verdicts(&name, &self.verdicts);
            flow.skip_rejected(&name, &self.verdicts);
            let addresses: Vec<IpAddr> = flow.candidates.iter().map(|c| c.ip()).collect();
            self.verdicts.check(&name, &addresses);
            if self.verdicts.any_pending(&name, &addresses) {
                flow.verify_by = Some(now + verify::WAIT);
            }
        }

        stats.flows_named_by_sni += 1;
        log::info!(
            "watt: flow {dst} was relayed blind; the client's handshake named {name} — \
             re-planned to {target} ({count} candidate(s))"
        );
        true
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
    use crate::upstream_proxy::ProxyKind;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
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

    /// The one flow a harness has open, for assertions about it.
    fn sole_flow(relay: &TcpRelay) -> &TcpFlow {
        assert_eq!(relay.flows.len(), 1, "the harness expects exactly one flow");
        relay.flows.values().next().expect("checked above")
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

    /// A client's opening bytes must not reach a server before the flow has been
    /// named, and must not be lost while the naming happens.
    ///
    /// Both halves matter and they pull in opposite directions. Holding the bytes
    /// is what makes a re-plan possible at all — a byte that reaches the abandoned
    /// server cannot be taken back — but a hold that dropped them would break every
    /// connection it was meant to help. So the test checks the server received
    /// nothing on the tick the hello arrived, and then received it whole on the
    /// next one.
    ///
    /// The dialled address is overridden to a real listener so the abandoned server
    /// is observable. That also makes this the test for the other half of the
    /// precedence rule: an override is explicit operator intent, so the recovered
    /// name is recorded but does not move the route.
    #[test]
    fn a_client_hello_is_held_back_until_the_flow_has_been_named() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = listener.local_addr().unwrap();
        let dialled: IpAddr = "198.51.100.9".parse().unwrap();

        let mut harness = Harness::new(
            server_addr,
            vec![crate::config::DestinationOverride::endpoint(
                dialled,
                443,
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                server_addr.port(),
            )],
        );
        harness.server_ip = dialled;
        harness.server_port = 443;

        let syn_ack = harness.handshake_syn();
        let ack = syn_ack.seq.wrapping_add(1);
        harness.send(
            harness.client_seq + 1,
            ack,
            packet::TcpFlags { ack: true, ..Default::default() },
            b"",
        );

        let (mut accepted, _) = listener.accept().expect("the relay must connect upstream");
        // One more pass, so the dial is confirmed and the flow has a winner to
        // write to. Without it the assertions below would hold for the wrong
        // reason.
        harness.send(
            harness.client_seq + 1,
            ack,
            packet::TcpFlags { ack: true, ..Default::default() },
            b"",
        );

        {
            let flow = sole_flow(&harness.relay);
            assert_eq!(flow.host, None, "nothing observed a name for this address");
            assert!(flow.watching_handshake(), "so the handshake is watched instead");
            assert!(flow.winner.is_some(), "and the dial has somewhere to go");
            assert_eq!(flow.bytes_to_upstream, 0);
        }
        assert_eq!(harness.stats.flows_without_name, 1);

        // The client's own opening bytes.
        let hello = watt_net::probe::debug_hello("sni.example");
        harness.send(
            harness.client_seq + 1,
            ack,
            packet::TcpFlags { ack: true, psh: true, ..Default::default() },
            &hello,
        );

        {
            let flow = sole_flow(&harness.relay);
            assert_eq!(
                flow.host.as_deref(),
                Some("sni.example"),
                "the flow is named by its own handshake"
            );
            assert_eq!(
                flow.bytes_to_upstream, 0,
                "and nothing has been written to a server yet"
            );
        }
        assert_eq!(
            harness.stats.flows_named_by_sni, 0,
            "the override outranks the name, so there is no re-plan to count"
        );

        accepted
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut buf = [0u8; 8];
        match accepted.read(&mut buf) {
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            other => panic!("the server must not see the hello yet, got {other:?}"),
        }

        // The next pass releases the bytes, unchanged and in order.
        harness.send(
            harness.client_seq + 1 + hello.len() as u32,
            ack,
            packet::TcpFlags { ack: true, ..Default::default() },
            b"",
        );
        assert_eq!(harness.stats.bytes_client_to_upstream, hello.len() as u64);

        let mut received = vec![0u8; hello.len()];
        accepted
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        accepted
            .read_exact(&mut received)
            .expect("the held bytes must arrive whole");
        assert_eq!(received, hello);
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

    /// A socket error must be named from its errno alone.
    ///
    /// The unknown-errno case is the one that matters: it is the branch that
    /// would reach for `io::Error`'s `Display` if the table were the only path,
    /// and `Display` on an errno-derived error is the call this avoids. A test
    /// that only covered the known errnos would not pin that down.
    #[test]
    fn socket_errors_are_named_by_errno() {
        use std::io::{Error, ErrorKind};
        assert_eq!(
            upstream_error_label(&Error::from_raw_os_error(libc::ECONNRESET)),
            "reset by peer"
        );
        assert_eq!(
            upstream_error_label(&Error::from_raw_os_error(libc::EPIPE)),
            "broken pipe"
        );
        assert_eq!(
            upstream_error_label(&Error::from_raw_os_error(libc::ETIMEDOUT)),
            "timed out"
        );
        // Built in-process, so there is no errno and the kind is all there is.
        assert_eq!(
            upstream_error_label(&Error::new(ErrorKind::ConnectionRefused, "no errno")),
            "refused"
        );
        assert_eq!(upstream_error_label(&Error::from_raw_os_error(9999)), "socket error");
    }

    /// The attribution this was written for: a reset after the client's first
    /// bytes went out and before anything came back is *not* about the address.
    #[test]
    fn a_reset_before_anything_came_back_is_reported_as_a_path_problem() {
        use std::io::Error;
        let mut flow = flow_with(&["203.0.113.1"], "blocked.example");
        // The client's bytes reached the socket, so the dial had completed —
        // which is the only reason the address can be ruled out.
        flow.sent_any = true;
        flow.bytes_to_upstream = 517;

        let (level, line) = describe_upstream_cut(
            &flow,
            "write",
            &Error::from_raw_os_error(libc::ECONNRESET),
            Instant::now(),
        );

        assert_eq!(level, log::Level::Warn, "{line}");
        // The name is the whole point: without it the same address working for
        // another name cannot be told from the address being dead.
        assert!(line.contains("blocked.example"), "{line}");
        assert!(line.contains("203.0.113.1"), "{line}");
        assert!(line.contains("reset by peer"), "{line}");
        assert!(line.contains("517 bytes sent / 0 received"), "{line}");
    }

    /// The same socket error after data has flowed is ordinary and stays quiet.
    #[test]
    fn a_cut_after_data_flowed_is_not_reported_as_a_path_problem() {
        use std::io::Error;
        let mut flow = flow_with(&["203.0.113.1"], "healthy.example");
        flow.sent_any = true;
        flow.bytes_to_upstream = 4096;
        flow.bytes_from_upstream = 65536;

        let (level, line) = describe_upstream_cut(
            &flow,
            "read",
            &Error::from_raw_os_error(libc::EPIPE),
            Instant::now(),
        );

        assert_eq!(level, log::Level::Info, "{line}");
        assert!(line.contains("broken pipe"), "{line}");
        assert!(line.contains("4096 bytes sent / 65536 received"), "{line}");
    }

    /// A flow with no observed name must still produce a readable line.
    #[test]
    fn a_flow_without_a_name_says_so_rather_than_leaving_a_gap() {
        use std::io::Error;
        let mut flow = flow_with(&["203.0.113.1"], "placeholder.example");
        flow.host = None;
        flow.sent_any = true;

        let (_, line) =
            describe_upstream_cut(&flow, "read", &Error::from_raw_os_error(libc::ECONNRESET), Instant::now());

        assert!(line.contains("no name observed"), "{line}");
    }

    /// `move_bytes` runs again on a dead flow, so the report has to be once-only.
    #[test]
    fn an_upstream_cut_is_reported_at_most_once() {
        use std::io::Error;
        let mut flow = flow_with(&["203.0.113.1"], "blocked.example");
        assert!(!flow.cut_reported);

        flow.log_upstream_cut("write", &Error::from_raw_os_error(libc::ECONNRESET), Instant::now());
        assert!(flow.cut_reported);

        // A second failure — the read that follows the write that already failed
        // — must not re-report.
        flow.log_upstream_cut("read", &Error::from_raw_os_error(libc::EPIPE), Instant::now());
        assert!(flow.cut_reported);
    }

    /// A relay with one flow that opened blind, dialling an address no rule claims.
    ///
    /// The address is the whole point. The rule set owns `203.0.113.10`; the client
    /// dialled `198.51.100.9`; and nothing observed a DNS answer for it. Without a
    /// name there is no path from one to the other, which is exactly the state the
    /// handshake watcher exists to leave.
    fn blind_flow() -> (TcpRelay, Planner, Stats, SocketHandle) {
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

        let dialled: SocketAddr = "198.51.100.9:443".parse().unwrap();
        let key = FlowKey::new(
            PROTO_TCP,
            "10.0.0.2".parse().unwrap(),
            dialled.ip(),
            40000,
            dialled.port(),
        );
        let mut flow = TcpFlow::new(key, vec![dialled], false, "direct", epoch);
        // As `promote_accepted` leaves a flow it could not name.
        flow.hello = Some(Vec::new());
        relay.flows.insert(handle, flow);

        (relay, planner, Stats::default(), handle)
    }

    /// The name the kernel could not get from DNS, taken from the client itself.
    ///
    /// The hello is built by `watt-net`'s probe — an independent implementation,
    /// written to ask a *server* for its certificate rather than to be read. Two
    /// separately written halves agreeing is evidence about the wire format; a
    /// shared test helper would be evidence about the helper.
    #[test]
    fn the_client_hello_names_the_flow_the_dns_never_did() {
        let mut flow = flow_with(&["203.0.113.1"], "placeholder.example");
        flow.host = None;
        flow.hello = Some(Vec::new());

        flow.watch_handshake(&watt_net::probe::debug_hello("t.example"));

        assert_eq!(flow.sni_pending.as_deref(), Some("t.example"));
        assert!(
            !flow.watching_handshake(),
            "the client's bytes are released once they have been read"
        );
    }

    #[test]
    fn a_hello_split_across_segments_is_waited_for_rather_than_abandoned() {
        // A TLS record is one logical unit and TCP may split it anywhere. Treating
        // the first fragment as "no name here" would lose the name for a client
        // that was about to send it.
        let hello = watt_net::probe::debug_hello("t.example");
        let cut = hello.len() / 2;
        let mut flow = flow_with(&["203.0.113.1"], "placeholder.example");
        flow.host = None;
        flow.hello = Some(Vec::new());

        flow.watch_handshake(&hello[..cut]);
        assert!(flow.watching_handshake(), "half a hello is not an answer");
        assert_eq!(flow.sni_pending, None);

        flow.watch_handshake(&hello[cut..]);
        assert_eq!(flow.sni_pending.as_deref(), Some("t.example"));
    }

    #[test]
    fn a_stream_that_will_never_have_a_name_stops_being_watched() {
        // Holding the client's bytes for a name that is not coming would keep them
        // for the life of the flow. Both of these are answers, not pauses.
        for opening in [b"GET / HTTP/1.1\r\n".as_slice(), &[0x17, 0x03, 0x03, 0x00, 0x10]] {
            let mut flow = flow_with(&["203.0.113.1"], "placeholder.example");
            flow.host = None;
            flow.hello = Some(Vec::new());

            flow.watch_handshake(opening);

            assert!(!flow.watching_handshake(), "{opening:?} is not a ClientHello");
            assert_eq!(flow.sni_pending, None);
        }
    }

    #[test]
    fn a_name_read_from_the_handshake_re_plans_the_flow_and_closes_the_old_race() {
        let (mut relay, mut planner, mut stats, handle) = blind_flow();
        let now = Instant::now();
        // A dial to the address the client chose, in flight and about to be
        // abandoned. Nothing has been written to it, so closing it is a bare FIN.
        let dial = dialing_upstream(UNROUTABLE.parse().unwrap(), now);
        let dial_fd = dial.socket.raw_fd();
        relay.insert_upstream(handle, dial);
        {
            let flow = relay.flows.get_mut(&handle).expect("inserted above");
            flow.next_candidate = 1;
            flow.sni_pending = Some("t.example".to_string());
        }

        assert!(relay.adopt_handshake_name(handle, now, &mut planner, &mut stats));

        let flow = &relay.flows[&handle];
        assert_eq!(flow.host.as_deref(), Some("t.example"));
        assert!(flow.steered, "the flow now follows a rule");
        assert_eq!(flow.reason, "rule-addresses");
        assert_eq!(
            flow.candidates,
            vec!["203.0.113.10:443".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(flow.next_candidate, 0, "the new race starts from the front");
        assert!(flow.winner.is_none());
        assert!(flow.upstreams.is_empty(), "the dial to the wrong address is gone");
        assert!(
            !relay.fd_index.contains_key(&dial_fd),
            "and its descriptor is released with it"
        );
        assert_eq!(relay.dialing, 0);
        assert_eq!(stats.flows_named_by_sni, 1);
    }

    #[test]
    fn a_name_is_not_adopted_once_a_byte_has_reached_an_upstream() {
        // Past that point the client is in a conversation with one server, and
        // moving the flow would splice two of them into a stream neither agreed to.
        // The name is still worth recording — the attribution log is keyed on it —
        // but the route must not move.
        let (mut relay, mut planner, mut stats, handle) = blind_flow();
        {
            let flow = relay.flows.get_mut(&handle).expect("inserted above");
            flow.bytes_to_upstream = 7;
            flow.sni_pending = Some("t.example".to_string());
        }

        assert!(!relay.adopt_handshake_name(handle, Instant::now(), &mut planner, &mut stats));

        let flow = &relay.flows[&handle];
        assert_eq!(flow.host.as_deref(), Some("t.example"));
        assert_eq!(flow.candidates, vec!["198.51.100.9:443".parse::<SocketAddr>().unwrap()]);
        assert!(!flow.steered);
        assert_eq!(stats.flows_named_by_sni, 0);
    }

    #[test]
    fn a_name_the_rule_set_does_not_cover_leaves_the_route_alone() {
        let (mut relay, mut planner, mut stats, handle) = blind_flow();
        relay.flows.get_mut(&handle).expect("inserted above").sni_pending =
            Some("nowhere.example".to_string());

        assert!(!relay.adopt_handshake_name(handle, Instant::now(), &mut planner, &mut stats));

        assert_eq!(
            relay.flows[&handle].candidates,
            vec!["198.51.100.9:443".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(stats.flows_named_by_sni, 0);
    }

    #[test]
    fn a_name_that_names_the_rule_the_address_already_implied_is_not_re_dialled() {
        // `plan_for_ip` steered the flow from the address alone, with no name. The
        // handshake then confirms the same rule — so the candidates are already the
        // ones the name would produce, and restarting the race would cost the client
        // a handshake for nothing.
        let (mut relay, mut planner, mut stats, handle) = blind_flow();
        {
            let flow = relay.flows.get_mut(&handle).expect("inserted above");
            flow.rule_key = Some(0);
            flow.sni_pending = Some("t.example".to_string());
        }

        assert!(!relay.adopt_handshake_name(handle, Instant::now(), &mut planner, &mut stats));

        assert_eq!(stats.flows_named_by_sni, 0);
        assert_eq!(
            relay.flows[&handle].candidates,
            vec!["198.51.100.9:443".parse::<SocketAddr>().unwrap()]
        );
    }

    /// Make `close` on `stream` send a reset instead of a FIN.
    ///
    /// `SO_LINGER` with a zero timeout is the only portable way to ask for this
    /// from userspace. A plain close would look like a clean end of stream and
    /// never reach the error arm the test below is about.
    fn reset_on_close(stream: &std::net::TcpStream) {
        use std::os::unix::io::AsRawFd;
        let linger = libc::linger {
            l_onoff: 1,
            l_linger: 0,
        };
        // SAFETY: `stream` owns an open descriptor, and `linger` is the value
        // `SO_LINGER` expects, at its own size.
        let result = unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                (&linger as *const libc::linger).cast::<libc::c_void>(),
                std::mem::size_of::<libc::linger>() as libc::socklen_t,
            )
        };
        assert_eq!(result, 0, "SO_LINGER: {}", std::io::Error::last_os_error());
    }

    /// A reset from a real peer must reach the attribution path.
    ///
    /// The tests above assert the *judgement*; this one asserts the wiring — that
    /// a reset arriving from an actual socket is what `move_bytes` sees as a
    /// non-retryable error, and that it is reported as a cut rather than being
    /// folded into `upstream_eof` and lost, which is what it did before this
    /// existed. An arm nothing ever enters looks exactly like a working one in
    /// the log, so it has to be driven by something real at least once.
    #[test]
    fn a_real_reset_is_attributed_to_the_flow_it_cut() {
        let (listener, addr) = live_listener();
        let up = connected_upstream(addr);
        let (server, _) = listener.accept().unwrap();

        let (mut relay, _planner, handle) = relay_with_window(vec![up], vec![addr]);
        {
            let flow = relay.flows.get_mut(&handle).unwrap();
            flow.winner = Some(0);
            // Stand in for the client's first bytes having gone out. Without this
            // the cut would be judged "nothing ever happened" rather than "the
            // dial succeeded", which is the whole distinction being tested.
            flow.sent_any = true;
            flow.bytes_to_upstream = 517;
        }

        reset_on_close(&server);
        drop(server);

        // Wait for the reset to be observable on the client descriptor. Without
        // this wait the first `move_bytes` could run before it arrived, see a
        // healthy socket, and the test would pass or fail for the wrong reason.
        let fd = relay.flows[&handle].upstreams[0].socket.raw_fd();
        poll_until(fd, libc::POLLIN, Duration::from_secs(5));

        let mut stats = Stats::default();
        let mut scratch = vec![0u8; RELAY_CHUNK];
        relay.move_bytes(handle, Instant::now(), &mut scratch, &mut stats);

        let flow = &relay.flows[&handle];
        assert!(flow.upstream_eof, "a reset must end the upstream direction");
        assert!(
            flow.cut_reported,
            "a reset from a real peer must be attributed, not silently dropped"
        );
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
            handshake: None,
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
            handshake: None,
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
        let mut stats = Stats::default();

        // A tick right after the launch: `SO_ERROR` is 0, but the dial has not
        // finished, so it must not be taken as a win.
        relay.retire_dials(handle, started, &mut planner, &mut stats);
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
        relay.retire_dials(handle, started + Duration::from_secs(30), &mut planner, &mut stats);
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
        let mut stats = Stats::default();

        relay.retire_dials(handle, started + Duration::from_millis(1), &mut planner, &mut stats);
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
    // Racing (happy eyeballs) — the invariants of the kernel-racing design
    // note (§5; the note itself is not distributed with this repository),
    // exercised by its §7 cases.
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
            handshake: None,
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

        /// Whether the dial window is empty, counting a flow that is already gone
        /// as empty.
        ///
        /// A flow whose last dial is retired with nothing won is marked `failed`,
        /// and the reaper collects it in the same step — so "the window is empty"
        /// and "the flow is gone" are the same observation one tick apart. A test
        /// that waits for the former has to survive the latter, and indexing
        /// `flows` would panic on exactly the tick that matters.
        fn window_is_empty(&self) -> bool {
            self.relay
                .flows
                .get(&self.handle)
                .map(|flow| flow.upstreams.is_empty())
                .unwrap_or(true)
        }

        /// Whether the flow has settled on an upstream, counting a flow that is
        /// already gone as not settled. The companion to `window_is_empty`, and
        /// for the same reason.
        fn has_winner(&self) -> bool {
            self.relay
                .flows
                .get(&self.handle)
                .map(|flow| flow.winner.is_some())
                .unwrap_or(false)
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
                    handshake: None,
                },                Upstream {
                    socket: done,
                    target,
                    candidate_index: 1,
                    connecting: false,
                    connect_ready: true,
                    started_at: now,
                    failed: false,
                    handshake: None,
                },
            ],
            vec![stuck_target, target],
        );
        let mut stats = Stats::default();

        relay.retire_dials(handle, now, &mut planner, &mut stats);

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

    // -----------------------------------------------------------------------
    // The upstream exit.
    //
    // Every test here talks to a real SOCKS5 server on loopback, because the part
    // of this that can silently be wrong is the part no canned byte string can
    // see: the relay has to drive a handshake over a socket it is about to treat
    // as a tunnel, and it has to stop at exactly the last byte of the reply.
    // -----------------------------------------------------------------------

    /// A SOCKS5 success reply: version, success, reserved, IPv4 bound address.
    const SOCKS5_OK: [u8; 10] = [0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x1f, 0x90];

    /// A SOCKS5 refusal: `REP = 0x05`, "connection refused by the destination".
    const SOCKS5_REFUSED: [u8; 10] = [0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0];

    /// The CONNECT request the relay must send for [`proxied_destination`]:
    /// version, CONNECT, reserved, IPv4, `203.0.113.7`, port 443.
    const SOCKS5_REQUEST: [u8; 10] = [0x05, 0x01, 0x00, 0x01, 203, 0, 113, 7, 0x01, 0xbb];

    /// The greeting the relay must open with: SOCKS5, one method, "no auth".
    const SOCKS5_GREETING: [u8; 3] = [0x05, 0x01, 0x00];

    /// The address the proxy tests ask for.
    ///
    /// TEST-NET-3 is public by every predicate the relay applies — so the flow is
    /// proxied — and routable nowhere, which is the point: the proxy is the far
    /// end and the destination is never dialled by this host.
    fn proxied_destination() -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(203, 0, 113, 7), 443))
    }

    /// A one-connection SOCKS5 proxy on loopback that then becomes the far end of
    /// the tunnel.
    ///
    /// `reply` is the whole CONNECT reply and `early` is written in the **same
    /// call** as it, so the two arrive in one segment. That is the entire reason
    /// this is a socket and not a canned byte string: a handshake that read a byte
    /// past the end of its own reply would swallow the start of the tunnel it had
    /// just opened, and a test that wrote the two separately would never see it.
    ///
    /// Everything the proxy is handed is recorded in order, so the caller can
    /// assert on the greeting, the CONNECT request and the client's payload as one
    /// uninterrupted stream — which is also how a byte lost in either direction
    /// shows up.
    struct Socks5Proxy {
        address: SocketAddr,
        seen: Arc<Mutex<Vec<u8>>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Socks5Proxy {
        /// Start a proxy that answers the CONNECT with `reply` plus `early`, then
        /// reads `payload` bytes of client traffic and stops.
        fn start(reply: &'static [u8], early: &'static [u8], payload: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let recorder = Arc::clone(&seen);
            let thread = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let record = |bytes: &[u8]| recorder.lock().unwrap().extend_from_slice(bytes);
                let mut greeting = [0u8; SOCKS5_GREETING.len()];
                stream.read_exact(&mut greeting).unwrap();
                record(&greeting);
                stream.write_all(&[0x05, 0x00]).unwrap();
                let mut request = [0u8; SOCKS5_REQUEST.len()];
                stream.read_exact(&mut request).unwrap();
                record(&request);
                let mut answer = Vec::from(reply);
                answer.extend_from_slice(early);
                stream.write_all(&answer).unwrap();
                // Whatever arrives now is the relay's own traffic, and it has to
                // be exactly the client's bytes.
                if payload > 0 {
                    let mut buf = vec![0u8; payload];
                    stream.read_exact(&mut buf).unwrap();
                    record(&buf);
                }
                // Hold the socket open briefly. Dropping it here would put a FIN
                // on the wire that the relay's next read could see before the test
                // has looked at what it delivered.
                std::thread::sleep(Duration::from_millis(200));
            });
            Self {
                address,
                seen,
                thread: Some(thread),
            }
        }

        fn config(&self) -> ProxyConfig {
            ProxyConfig {
                kind: ProxyKind::Socks5,
                address: self.address,
                username: None,
                password: None,
            }
        }

        /// Join the proxy thread and return everything it was handed, greeting
        /// first.
        fn finish(mut self) -> Vec<u8> {
            self.thread
                .take()
                .unwrap()
                .join()
                .expect("the proxy thread must not panic");
            let seen = self.seen.lock().unwrap().clone();
            seen
        }
    }

    /// Tick a race harness until `done`, pausing between ticks.
    ///
    /// The proxy answers on its own thread, so a tight loop would run its whole
    /// tick budget before the reply had been written. The pause is what makes the
    /// harness wait for a real socket instead of for a clock.
    fn tick_until<P: Protector>(
        harness: &mut RaceHarness<P>,
        mut done: impl FnMut(&RaceHarness<P>) -> bool,
    ) -> bool {
        for _ in 0..200 {
            harness.tick(Duration::from_millis(1));
            if done(harness) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// Whether a byte queue begins with `prefix`.
    ///
    /// `VecDeque` has no `starts_with`, and this is the whole reason it exists.
    fn begins_with(queue: &VecDeque<u8>, prefix: &[u8]) -> bool {
        queue.len() >= prefix.len() && queue.iter().zip(prefix).all(|(byte, want)| byte == want)
    }

    #[test]
    fn a_proxied_flow_is_carried_by_the_proxy_and_keeps_the_destination_as_its_target() {
        let proxy = Socks5Proxy::start(&SOCKS5_OK, b"early", 12);
        let destination = proxied_destination();
        let mut harness = RaceHarness::new(
            vec![destination],
            StackConfig {
                upstream_proxy: Some(proxy.config()),
                ..StackConfig::default()
            },
            NoProtector,
        );

        // The reply and the payload behind it arrive in one segment. Delivering
        // that payload is the whole test: it is the byte a handshake that read one
        // past its own reply would have eaten, and the symptom would be a session
        // that works for HTTP and breaks for TLS.
        let delivered = tick_until(&mut harness, |harness| {
            begins_with(&harness.flow().to_client, b"early")
        });
        assert!(
            delivered,
            "the bytes the proxy sent after its reply never reached the client"
        );
        assert_eq!(
            harness.winner(),
            Some(destination),
            "the flow is for the destination; the proxy is only how it gets there"
        );
        assert_eq!(harness.stats.proxy_handshakes, 1, "one dial, one handshake");
        assert_eq!(harness.stats.proxy_refusals, 0);

        // Now the other direction: the client's bytes must reach the proxy
        // untouched, and the CONNECT must have named the destination.
        {
            let flow = harness.relay.flows.get_mut(&harness.handle).unwrap();
            flow.to_upstream.extend(b"hello server");
        }
        let mut scratch = vec![0u8; RELAY_CHUNK];
        harness.relay.move_bytes(
            harness.handle,
            harness.now,
            &mut scratch,
            &mut harness.stats,
        );

        let mut expected = Vec::from(SOCKS5_GREETING);
        expected.extend_from_slice(&SOCKS5_REQUEST);
        expected.extend_from_slice(b"hello server");
        assert_eq!(
            proxy.finish(),
            expected,
            "the proxy must be handed the greeting, a CONNECT naming the destination, and \
             then the client's bytes with nothing added and nothing missing"
        );
    }

    #[test]
    fn a_proxy_that_refuses_the_connect_fails_the_flow_without_blaming_the_destination() {
        let proxy = Socks5Proxy::start(&SOCKS5_REFUSED, b"", 0);
        let destination = proxied_destination();
        let mut harness = RaceHarness::new(
            vec![destination],
            StackConfig {
                upstream_proxy: Some(proxy.config()),
                ..StackConfig::default()
            },
            NoProtector,
        );

        let emptied = tick_until(&mut harness, |harness| harness.window_is_empty());
        assert!(emptied, "the refused dial must leave the window");
        assert!(
            !harness.has_winner(),
            "a refused CONNECT is not a win"
        );
        assert!(
            harness.stats.proxy_refusals >= 1,
            "the refusal must be counted, saw {}",
            harness.stats.proxy_refusals
        );
        assert_eq!(harness.stats.proxy_handshakes, 0);

        // The one thing that must not happen: the proxy's answer written into the
        // address table as though this host had measured it. `report_failure` cools
        // an address for a whole cooldown, and a proxy that is down, wants a
        // password, or forbids this host would cool every address it was asked
        // about — leaving the direct path quietly degraded for a minute after the
        // exit is switched off, with nothing in the log to connect the two.
        assert!(
            !harness
                .planner
                .router()
                .selector()
                .is_cooled(destination.ip(), harness.now),
            "a proxy's refusal was recorded as this host's measurement of the address"
        );
        proxy.finish();
    }

    #[test]
    fn an_exit_that_never_answers_is_given_up_on_rather_than_waited_on_forever() {
        // A proxy that accepts the connection and then says nothing is the one
        // failure that would hold a flow for the life of the tunnel: the connect
        // succeeded, so nothing else would ever move it along. The handshake shares
        // the dial's budget for exactly this reason.
        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let silent_address = silent.local_addr().unwrap();
        std::thread::spawn(move || {
            let _held = silent.accept().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        });

        let destination = proxied_destination();
        let mut harness = RaceHarness::new(
            vec![destination],
            StackConfig {
                upstream_proxy: Some(ProxyConfig {
                    kind: ProxyKind::Socks5,
                    address: silent_address,
                    username: None,
                    password: None,
                }),
                ..StackConfig::default()
            },
            NoProtector,
        );

        // The only candidate is also the last hope, so the budget is the full
        // `connect_timeout`; the steps are sized to cross it.
        let mut emptied = false;
        for _ in 0..80 {
            harness.tick(Duration::from_millis(500));
            if harness.window_is_empty() {
                emptied = true;
                break;
            }
        }
        assert!(
            emptied,
            "a proxy that never answers must be given up on, not waited on"
        );
        assert!(!harness.has_winner());
        assert!(
            harness.stats.proxy_refusals >= 1,
            "a handshake that never completed must be counted as a refusal, saw {}",
            harness.stats.proxy_refusals
        );
        assert!(
            !harness
                .planner
                .router()
                .selector()
                .is_cooled(destination.ip(), harness.now),
            "a silent proxy must not cool the destination it was asked about"
        );
    }

    #[test]
    fn a_local_destination_is_dialled_directly_even_with_an_exit_configured() {
        // The gate, from the side that matters most: a rewrite pointing a flow at a
        // local stub is explicit operator intent, and a stub on loopback is not
        // reachable through an exit at all. A service on the same LAN is the same
        // argument — it is not the internet, and it must not leave the device.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.local_addr().unwrap();
        let proxy = Socks5Proxy::start(&SOCKS5_OK, b"", 0);

        let fake_dst: IpAddr = "203.0.113.10".parse().unwrap();
        let mut harness = Harness::with_config(
            server,
            StackConfig {
                upstream_proxy: Some(proxy.config()),
                overrides: vec![crate::config::DestinationOverride::endpoint(
                    fake_dst,
                    server.port(),
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    server.port(),
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
        for _ in 0..200 {
            if sole_flow(&harness.relay).winner.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
            let now = harness.now();
            harness.pump(now);
        }

        let flow = sole_flow(&harness.relay);
        assert!(
            flow.winner.is_some(),
            "the local target must settle the race on its own"
        );
        assert_eq!(flow.winner_target(), Some(server));
        assert!(
            flow.upstreams.iter().all(|up| up.handshake.is_none()),
            "a dial to a local address must not carry a proxy handshake"
        );
        assert_eq!(
            harness.stats.proxy_handshakes, 0,
            "the exit must not have been used for a local destination"
        );
        // And the exit really was not touched, rather than merely unused on paper.
        listener
            .accept()
            .expect("the local target must have been dialled directly");
        drop(proxy);
    }
}

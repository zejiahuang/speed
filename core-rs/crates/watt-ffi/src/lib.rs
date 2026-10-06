//! C ABI for the Android shell, plus the JNI entry points behind the
//! `jni-bridge` feature.
//!
//! The kernel is a library, and on Android the shell is Kotlin: `VpnService`
//! owns the tunnel descriptor, and `VpnService.protect(fd)` is the platform's
//! way to keep the kernel's own upstream sockets out of the tunnel it created.
//! Neither of those exists in Rust, so this crate is the seam.
//!
//! # Why a callback and not a mark
//!
//! On a plain Linux host the kernel exempts its upstream sockets by setting
//! `SO_MARK` and routing the mark outside the tunnel (`MarkProtector`). **That
//! does not work on Android** — measured on a real emulator, the tunnel's TCP
//! relay loops: sixteen client connections produced `tcp_open=2048` and not one
//! byte came back upstream. Android routes by socket ownership rather than by
//! mark, which is exactly why the platform provides `VpnService.protect`.
//!
//! So the protector here is a callback the shell supplies, and it is the one
//! piece of this ABI that cannot be defaulted or worked around.
//!
//! # Lifetime rules
//!
//! * Every pointer returned by `watt_engine_new` must be released with
//!   `watt_engine_free`, exactly once. Nothing else frees it.
//! * The protect callback and its context must stay valid for as long as the
//!   engine does. The engine calls it from its own thread.
//! * The rule document is copied, not borrowed. The caller may free it as soon
//!   as the call returns.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::net::{IpAddr, SocketAddr, TcpListener, ToSocketAddrs};
use std::os::unix::io::RawFd;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use watt_rules::{IpSelectorConfig, RuleSet, RuleSource};
use watt_stack::{DohEndpoint, Engine, Protector, ProxyConfig, ProxyKind, StackConfig, TunDevice};

#[cfg(feature = "jni-bridge")]
mod jni_bridge;

/// Called with a fresh upstream descriptor, before it is connected.
///
/// This is where the shell calls `VpnService.protect(fd)`. Returning zero means
/// the descriptor could not be protected, and the engine treats that as fatal
/// for that connection — connecting anyway would hand the socket back to the
/// tunnel and start the loop described above.
pub type WattProtectFn = extern "C" fn(ctx: *mut c_void, fd: c_int) -> c_int;

/// Counters, filled in by [`watt_engine_stats`].
///
/// Laid out as plain integers rather than a nested struct so that JNI can read
/// it without matching Rust's field ordering.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct WattStats {
    pub packets_in: u64,
    pub packets_out: u64,
    pub tcp_flows_opened: u64,
    pub tcp_flows_closed: u64,
    pub tcp_flows_rejected: u64,
    pub tcp_connect_failures: u64,
    pub udp_flows_opened: u64,
    pub udp_flows_evicted: u64,
    pub dns_queries: u64,
    pub dns_answered_locally: u64,
    pub dns_trimmed: u64,
    pub bytes_client_to_upstream: u64,
    pub bytes_upstream_to_client: u64,
    pub live_flows: u64,
    /// Flows that matched a rule and were relayed to a rule address.
    pub flows_matched_rules: u64,
    /// Flows that matched nothing and were relayed to what the client asked for.
    ///
    /// The pair is the quickest answer to "is the rule set doing anything", which
    /// is otherwise invisible: a flow that is not steered looks exactly like one
    /// that is, from the outside.
    pub flows_direct: u64,
    /// Flows relayed with no name known for the destination.
    ///
    /// Sizes the DoH problem: a client resolving over HTTPS never shows the
    /// kernel a query, so nothing can be looked up in the rule set and no
    /// candidate can be checked against a certificate.
    pub flows_without_name: u64,
    /// Flows that opened with no name and recovered one from the client's own TLS
    /// handshake.
    ///
    /// The part of `flows_without_name` the handshake rescues. The two together
    /// are the measurement: without this one, the only evidence the recovery works
    /// at all is a log line per flow.
    pub flows_named_by_sni: u64,
    /// Flows the handshake named without moving.
    ///
    /// Together with `flows_named_by_sni` this is the honest numerator: the
    /// counter that existed measured only the names that changed a route, so a
    /// handshake that named 89 flows and moved 1 read as "1".
    pub flows_named_without_move: u64,
    /// Of `flows_named_by_sni`, the names that arrived alongside
    /// `encrypted_client_hello`.
    ///
    /// Reported separately because the confidence differs: GREASE ECH carries the
    /// real name, real ECH carries a cover name, and RFC 9849 makes the two
    /// indistinguishable from the kernel. A non-zero value here is the size of a
    /// deliberate bet, not an error.
    pub flows_named_under_ech: u64,
    /// Handshakes watched that ended with no name to take.
    ///
    /// Separates "the handshake could not help" from "the handshake was never
    /// given a chance".
    pub hellos_without_name: u64,
    /// Flows that the configured upstream exit agreed to carry.
    ///
    /// Zero whenever no exit is configured, so this is also the answer to "is the
    /// exit actually in use" — the question a switch cannot answer, because a
    /// switch only reports what the user asked for.
    pub proxy_handshakes: u64,
    /// Handshakes the exit answered and refused, or never answered at all.
    ///
    /// Read together with `proxy_handshakes`: a non-zero value here means the
    /// exit is reachable and the problem is the request — a wrong protocol, a
    /// missing credential, or the exit itself being unable to reach the
    /// destination. That is a different diagnosis from an exit that is
    /// unreachable, and the pair is what tells the two apart on a device.
    pub proxy_refusals: u64,
    /// Questions handed to the configured upstream resolver.
    ///
    /// Zero whenever no resolver is configured, so this is also the answer to
    /// "is the resolver actually in use" — which a switch cannot answer, because
    /// a switch only reports what the user asked for.
    pub dns_upstream_queries: u64,
    /// Questions the upstream answered, and whose answer reached the client.
    pub dns_upstream_answered: u64,
    /// Questions the upstream used every attempt on and still did not answer.
    ///
    /// The client was told nothing. Read with `dns_upstream_answered` this is the
    /// resolver's real success rate on this network, which is the number that
    /// decides whether the endpoint was worth configuring at all.
    pub dns_upstream_failed: u64,
    /// Attempts after the first, across every resolution.
    ///
    /// Non-zero is normal — a DoH gateway is measurably unreliable per request.
    /// A value approaching `dns_upstream_queries` means the endpoint is barely
    /// working and the retry is the only thing making it usable.
    pub dns_upstream_retries: u64,
    /// Questions the upstream was never offered, because too many resolutions
    /// were already in flight. Each was forwarded instead.
    pub dns_upstream_overflowed: u64,
}

/// A protector supplied from outside Rust.
///
/// The C ABI supplies a function pointer and a context; the JNI bridge supplies
/// a closure that calls into the JVM. Both end up here, which is what lets the
/// two entry points share one engine.
struct BoxedProtector(Box<dyn FnMut(RawFd) -> bool + Send>);

impl Protector for BoxedProtector {
    fn protect(&mut self, fd: RawFd) -> bool {
        (self.0)(fd)
    }

    /// A protector supplied by the shell has work to do. Its failure is not
    /// something to shrug at: the connection would be captured by the tunnel.
    fn required(&self) -> bool {
        true
    }
}

/// An engine the shell owns.
pub struct WattEngine {
    engine: Engine<TunDevice>,
    started: Instant,
    /// This engine's own identity, read by [`watt_engine_free`] to tell a
    /// double free from a legitimate free of a new engine that the allocator
    /// happened to place at a recycled address. See [`FreeId`].
    ///
    /// Never reused, so the guard cannot be fooled by address reuse the way the
    /// pointer-keyed version was.
    free_id: FreeId,
    /// How many calls are inside this engine right now.
    ///
    /// # Why a counter and not a `Mutex`
    ///
    /// `step` and `free` are called from **different threads**: the shell drives
    /// `step` from its run loop, while `close` runs on whichever thread noticed
    /// the tunnel going away — the main thread on `onDestroy`, or the run loop's
    /// own thread when `step` reports a failure. The two are not ordered by
    /// anything, and `free` reconstructs the box and drops it. If it does that
    /// while a step is still inside, the step is reading freed memory.
    ///
    /// The device showed exactly that: `#00 BoxedProtector::protect+7` under
    /// `#01 TcpRelay::service` under `#02 Engine::step`, on the same run loop
    /// that a concurrent `teardown()` had just freed. The same window also
    /// double-released the protector's JNI global reference, which surfaced
    /// separately as `decStrong() called too many times` on the JVM's reference
    /// queue thread.
    ///
    /// A `Mutex` around the whole engine would serialise `step` behind `free`
    /// and fix the ordering too, but `step` blocks for up to a hundred
    /// milliseconds waiting for a packet, and taking a lock for that long on the
    /// main thread is the freeze this design exists to avoid. A counter is
    /// enough: the shell only has to *see the busy edge* and refuse the free,
    /// rather than wait for it.
    ///
    /// `InFlight` because two threads touch it and neither may block the
    /// other — the run loop increments and decrements without ever waiting.
    in_flight: InFlight,
}

impl WattEngine {
    /// Take a claim on the engine, or refuse when it is being freed.
    ///
    /// Returns `false` once `free` has begun. A caller that gets `false` must do
    /// nothing with the pointer at all.
    fn enter(&self) -> bool {
        self.in_flight.enter()
    }

    fn leave(&self) {
        self.in_flight.leave();
    }
}

/// The busy counter behind [`WattEngine::in_flight`], on its own so the
/// invariant can be tested without a live tunnel.
///
/// The rule it enforces is small and absolute: a claim is granted only while no
/// free has been claimed, and a free is safe only once every claim has been
/// given back.
///
/// # Why two fields and not one
///
/// The first version folded both facts into one counter with a sentinel value
/// for "a free has been claimed" — the same shape `claim_for_free` uses for
/// identities. That is wrong here, and a test caught it: storing the sentinel
/// *overwrites* the count of work in flight, so `close_and_drain` could not tell
/// "nothing was inside" from "I just destroyed the record of what was inside",
/// and it would report a safe free while a call was still running. A `FreeId`
/// leaves no trace when it is freed, so a sentinel in a set is fine there; here
/// the count is live data that must survive being closed.
///
/// So the door and the count are separate. `closing` is the door — once set, no
/// new claim is granted. `busy` is the count — it only ever goes up on a claim
/// and down on a release, and never holds a magic value.
struct InFlight {
    busy: AtomicUsize,
    closing: AtomicBool,
}

impl InFlight {
    fn new() -> Self {
        Self {
            busy: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
        }
    }

    /// Claim, or refuse once a free has started.
    fn enter(&self) -> bool {
        // The fast path. An uncontended claim is one load and one add.
        if self.closing.load(Ordering::Acquire) {
            return false;
        }
        self.busy.fetch_add(1, Ordering::AcqRel);

        // The check after the add is the part that closes the race, and it is
        // not redundant with the one above. Two interleavings matter:
        //
        //   * `close` set the door between our load and our add. Then our add
        //     landed with the door already shut, and this second load sees it —
        //     we give the claim back and refuse, and the count returning to its
        //     previous value is what lets `close` proceed.
        //   * `close` sets the door after our add. Then it sees a non-zero count
        //     and waits for us, which is what we want.
        //
        // Without the second check, the first interleaving would hand out a claim
        // on memory that was already being freed. That is not a small window —
        // it is the window the device crashed in.
        if self.closing.load(Ordering::Acquire) {
            self.busy.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    /// Give a claim back.
    fn leave(&self) {
        self.busy.fetch_sub(1, Ordering::AcqRel);
    }

    /// Shut the door, then wait for anything already inside to come out.
    ///
    /// Returns `true` when nothing is in flight, which is the caller's licence
    /// to free. `false` means someone is still inside after `limit` polls and
    /// the caller must leak instead.
    ///
    /// The count is read, never written: a free that clobbered it could not tell
    /// a busy engine from an idle one.
    fn close_and_drain(&self, limit: u32) -> bool {
        self.closing.store(true, Ordering::Release);
        let mut waited = 0u32;
        while self.busy.load(Ordering::Acquire) != 0 && waited < limit {
            std::hint::spin_loop();
            waited += 1;
            if waited % 4096 == 0 {
                thread::yield_now();
            }
        }
        self.busy.load(Ordering::Acquire) == 0
    }
}

/// A claim on an engine, released when it goes out of scope.
///
/// Holding one of these is the caller's proof that `free` has not started, and
/// that it will not start until the guard drops — `free` waits for the count to
/// reach the sentinel and back to zero before it frees. So a function that owns
/// a guard may dereference the engine for its whole body, including across a
/// blocking JNI call, which is exactly what `step` needs.
///
/// This is a borrow expressed at runtime because the borrow is across threads
/// that the Rust type system cannot see: the shell reaches the engine through a
/// `jlong`, so there is no `&WattEngine` for the compiler to tie the free to.
///
/// Only the JNI bridge needs it. The C surface hands out a pointer the caller
/// is expected to keep alive by its own protocol, and a C caller that frees an
/// engine while stepping it has no claim to protect anyway.
#[cfg_attr(not(feature = "jni-bridge"), allow(dead_code))]
#[must_use = "dropping the guard immediately releases the claim, which is the whole point"]
pub(crate) struct EngineClaim<'a> {
    engine: &'a WattEngine,
}

#[cfg_attr(not(feature = "jni-bridge"), allow(dead_code))]
impl<'a> EngineClaim<'a> {
    /// Claim `engine`, or `None` when it is already being freed.
    pub(crate) fn acquire(engine: &'a WattEngine) -> Option<Self> {
        if engine.enter() {
            Some(Self { engine })
        } else {
            None
        }
    }

    /// The engine this claim covers.
    pub(crate) fn engine(&self) -> &'a WattEngine {
        self.engine
    }
}

impl Drop for EngineClaim<'_> {
    fn drop(&mut self) {
        self.engine.leave();
    }
}

thread_local! {
    /// The last failure, as a C string the caller may read but not free.
    ///
    /// Thread-local rather than global: the shell may drive several engines, and
    /// a shared slot would let one thread's failure overwrite another's before it
    /// was read. The string lives until the next failure on the same thread.
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Record a failure for [`last_error`].
pub(crate) fn set_last_error(message: impl Into<String>) {
    let text = message.into();
    // A NUL inside the message would truncate it; dropping them is better than
    // losing the whole message.
    let sanitised = text.replace('\0', " ");
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = CString::new(sanitised).ok();
    });
}

/// The last failure on this thread.
///
/// Only the JNI bridge reads this — the C surface hands it out through
/// `watt_last_error` directly — so a build without that feature sees it as dead.
#[cfg_attr(not(feature = "jni-bridge"), allow(dead_code))]
pub(crate) fn last_error() -> Option<String> {
    LAST_ERROR.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|text| text.to_string_lossy().into_owned())
    })
}

// --- shared implementation -------------------------------------------------
//
// Everything below is called by both the C exports and the JNI exports. Keeping
// one copy is what stops the two surfaces from drifting apart.

/// Build an engine on a descriptor the shell owns, with the shell's settings.
///
/// The descriptor is **adopted, not owned**: closing it stays the shell's job,
/// because `VpnService` is what opened it and closing it twice would take a
/// descriptor belonging to something else.
///
/// `config_json` is the user's kernel settings, as a flat JSON object. Every key
/// is optional and an unparsable document falls back to the defaults — a typo in
/// a settings field should not stop the tunnel, and the defaults are the values
/// the kernel was tuned around.
pub(crate) fn build_engine_with_config(
    tun_fd: c_int,
    name: Option<String>,
    document: &[u8],
    config_json: &str,
    protector: Box<dyn FnMut(RawFd) -> bool + Send>,
) -> Result<Box<WattEngine>, String> {
    let rules = parse_rules(document)?;
    let config = apply_settings(StackConfig::default(), config_json);

    // The selector is created with the router and the router with the engine, so
    // the user's failure cooldown has to be handed over *here* — there is no
    // later point at which the selector could be retuned. Everything else in
    // `IpSelectorConfig` stays at its default; the shell only exposes this knob.
    let failure_cooldown = config.failure_cooldown;
    // Read before `config` is moved into the engine: resolving dial names has to
    // happen before the run loop takes its `&mut` on the engine.
    let resolve_dial = config.dial_names;
    let router = watt_rules::Router::with_selector(
        rules,
        IpSelectorConfig {
            failure_cooldown,
            ..IpSelectorConfig::default()
        },
    );
    let mut engine = Engine::adopt_tun(
        tun_fd,
        name.unwrap_or_else(|| "detour0".to_string()),
        config,
        router,
        Box::new(BoxedProtector(protector)),
    )
    .map_err(|err| format!("adopting the tunnel descriptor: {err}"))?;

    // Resolve the rules' dial names, if the user wants them. This is the only
    // place it can be done: `resolve_dial_names` needs `&mut Engine`, and once
    // this function returns, the shell's run loop owns that borrow. The lookup
    // blocks, which is why it is here and not inside a step — and it is a no-op
    // for the common rule set, whose entries list literal addresses.
    if resolve_dial {
        let resolved = engine.resolve_dial_names(|host| {
            (host, 0u16)
                .to_socket_addrs()
                .map(|addrs| addrs.map(|addr| addr.ip()).collect())
                .unwrap_or_default()
        });
        if resolved > 0 {
            log::info!("watt: resolved dial names for {resolved} entries");
        }
    }

    Ok(Box::new(WattEngine {
        engine,
        started: Instant::now(),
        free_id: next_free_id(),
        in_flight: InFlight::new(),
    }))
}

/// The per-candidate dial budget, derived from the user's whole-connection one.
///
/// A third, floored at one second and capped at three: enough for a live server
/// on a slow mobile link (working connects sit near half a second), short enough
/// that several dead candidates still fit inside a client's own timeout. See
/// `StackConfig::first_connect_timeout`.
fn first_dial_from(full: Duration) -> Duration {
    let third = full / 3;
    third.clamp(Duration::from_secs(1), Duration::from_secs(3))
}

/// Overlay the shell's settings onto a default config.
///
/// Unknown keys are ignored rather than rejected, and a value of the wrong type
/// leaves that one field at its default. Both are deliberate: a settings file
/// written by a newer build, or edited by hand, should not be able to stop the
/// tunnel from starting.
pub(crate) fn apply_settings(mut config: StackConfig, json: &str) -> StackConfig {
    if json.trim().is_empty() {
        return config;
    }
    let Ok(values) = serde_json::from_str::<serde_json::Value>(json) else {
        set_last_error("the settings document is not valid JSON; using the defaults");
        return config;
    };
    let Some(object) = values.as_object() else {
        return config;
    };

    let number = |key: &str| object.get(key).and_then(|v| v.as_u64());
    let flag = |key: &str| object.get(key).and_then(|v| v.as_bool());
    let text = |key: &str| object.get(key).and_then(|v| v.as_str());

    if let Some(mtu) = number("mtu") {
        // Below 576 the kernel cannot carry a full-sized TCP segment; above the
        // tunnel's own MTU the device fragments. Clamped rather than refused.
        config.mtu = (mtu as usize).clamp(576, 9000);
    }
    if let Some(seconds) = number("connect_timeout_seconds") {
        let full = Duration::from_secs(seconds.clamp(1, 120));
        config.connect_timeout = full;
        // The per-candidate wait has to move with the user's number, or a user
        // who raises "connect timeout" to sixty seconds because their server is
        // slow would leave the failover budget at three and get *worse* failover
        // than the default. It stays a fraction of the whole, because the
        // distinction it encodes is "there is somewhere else to try" not "the
        // user thinks the network is slow".
        config.first_connect_timeout = first_dial_from(full);
    }
    if let Some(seconds) = number("tcp_idle_seconds") {
        config.tcp_idle_timeout = Duration::from_secs(seconds.clamp(10, 3600));
    }
    if let Some(seconds) = number("udp_idle_seconds") {
        // The UDP idle timeout is the floor for how long a silent flow holds a
        // descriptor, so it also decides how long the "descriptors returned to
        // baseline" judgement has to wait.
        config.udp_idle_timeout = Duration::from_secs(seconds.clamp(5, 600));
    }
    if let Some(flows) = number("max_tcp_flows") {
        config.max_tcp_flows = (flows as usize).clamp(16, 8192);
    }
    if let Some(flows) = number("max_udp_flows") {
        config.max_udp_flows = (flows as usize).clamp(16, 8192);
    }
    if let Some(addresses) = number("max_observed_addresses") {
        config.max_observed_addresses = (addresses as usize).clamp(64, 1 << 20);
    }
    if let Some(count) = number("max_candidates") {
        // At least one, or a steered flow would have nothing to dial. The upper
        // bound is generous because a large rule entry is not itself wrong —
        // what matters is that the list is finite.
        config.max_candidates = (count as usize).clamp(1, 256);
    }
    if let Some(millis) = number("connect_stagger_milliseconds") {
        config.connect_stagger = Duration::from_millis(millis.clamp(0, 5000));
    }
    if let Some(width) = number("race_width") {
        // One is the serial rollback and must be reachable; four is where the
        // extra SYNs stop buying anything on a mobile link.
        config.race_width = (width as usize).clamp(1, 4);
    }
    if let Some(millis) = number("race_launch_milliseconds") {
        // Zero is legal (first batch together) but not recommended; the ceiling
        // keeps a window of four from taking longer than a connect timeout.
        config.race_launch_interval = Duration::from_millis(millis.clamp(0, 1000));
    }
    if let Some(count) = number("max_dialing") {
        // A floor well above one window so racing still works, and a ceiling that
        // stays under any plausible `RLIMIT_NOFILE`.
        config.max_dialing = (count as usize).clamp(16, 1024);
    }
    if let Some(on) = flag("answer_dns_from_rules") {
        config.answer_dns_from_rules = on;
    }
    if let Some(on) = flag("observe_dns") {
        config.observe_dns = on;
    }
    if let Some(on) = flag("certificate_check") {
        config.certificate_check = on;
    }
    if let Some(seconds) = number("failure_cooldown_seconds") {
        // How long a failed address is pushed to the back of the candidate list.
        // Five seconds is a floor that still lets a dead address be retried
        // within a session; ten minutes is a ceiling past which "cooled down" is
        // indistinguishable from "removed".
        config.failure_cooldown = Duration::from_secs(seconds.clamp(5, 600));
    }
    if let Some(on) = flag("dial_names") {
        // Whether the kernel resolves the rules' dial names itself. Off means a
        // rule that names a host rather than listing addresses contributes
        // nothing, which is the honest consequence of turning it off.
        config.dial_names = on;
    }
    if let Some(address) = text("upstream_proxy_address") {
        // The upstream exit. A kind and an address are both required, so there is
        // exactly one rule to state: an exit exists when the settings name a
        // protocol this kernel speaks **and** an endpoint it can dial. Anything
        // less leaves `None`, which is the same state as never having configured
        // one — and that is the point. A half-configured exit would put a switch
        // on screen that reads as on while nothing is proxied, which is the one
        // thing this project does not allow a control to do.
        //
        // The shell is expected to gate its own switch on the endpoint for the
        // same reason; this side does not trust that, because a hand-edited
        // settings document reaches here without passing a switch at all.
        match (
            text("upstream_proxy_kind").and_then(ProxyKind::parse),
            address.parse::<SocketAddr>(),
        ) {
            (Some(kind), Ok(address)) => {
                config.upstream_proxy = Some(ProxyConfig {
                    kind,
                    address,
                    // Passed through exactly as written, including empty strings:
                    // `ProxyConfig::credentials` is the one place that decides a
                    // half-filled pair is no pair at all, and a second copy of
                    // that rule here would be a copy that drifts.
                    username: text("upstream_proxy_username").map(str::to_string),
                    password: text("upstream_proxy_password").map(str::to_string),
                });
            }
            // Named in the log rather than swallowed. Every other key here may be
            // ignored in silence because ignoring it leaves the kernel on a value
            // the user never asked to change; this one is the opposite — the user
            // asked for an exit and is not getting one, and without a line in the
            // log the only symptom is that the proxy appears not to help.
            (None, _) => log::warn!(
                "watt: the upstream exit at {address} was ignored — \
                 upstream_proxy_kind is missing or names a protocol this kernel does not speak"
            ),
            (_, Err(_)) => log::warn!(
                "watt: the upstream exit was ignored — \
                 upstream_proxy_address is not an address of the form host:port"
            ),
        }
    }

    if let Some(url) = text("dns_upstream_url") {
        // The upstream resolver, for names the rule set does not own. As with the
        // exit above there is exactly one rule: a resolver exists when the
        // settings name a URL this kernel can parse **and**, when that URL names a
        // host rather than an address, a bootstrap address to reach it by.
        //
        // The bootstrap is not a convenience. A DoH URL is a name, a name has to
        // be resolved before it can be connected to, and resolving is the thing
        // being configured — so without an address the endpoint is unreachable by
        // construction, and a switch built on it would be a control that cannot
        // take effect.
        let bootstrap = text("dns_upstream_address")
            .and_then(|raw| raw.trim().parse::<IpAddr>().ok());
        match DohEndpoint::parse(url, bootstrap) {
            Some(endpoint) => {
                log::info!("watt: upstream resolver {}", endpoint.describe());
                config.dns_upstream = Some(endpoint);
            }
            // An empty value is how the shell clears the setting, and that is a
            // request being honoured rather than refused, so it gets no line.
            // Anything else does: the user asked for a resolver and is not
            // getting one, and without a warning the only symptom is that names
            // still come back forged.
            None if !url.trim().is_empty() => log::warn!(
                "watt: the upstream resolver at {url} was ignored — \
                 it is not an https:// URL, or it names a host with no \
                 dns_upstream_address to reach it by"
            ),
            None => {}
        }
    }

    config
}

/// Build an engine on a descriptor the shell owns.
///
/// The descriptor is **adopted, not owned**: closing it stays the shell's job,
/// because `VpnService` is what opened it and closing it twice would take a
/// descriptor belonging to something else.
pub(crate) fn build_engine(
    tun_fd: c_int,
    name: Option<String>,
    document: &[u8],
    protector: Box<dyn FnMut(RawFd) -> bool + Send>,
) -> Result<Box<WattEngine>, String> {
    build_engine_with_config(tun_fd, name, document, "", protector)
}

/// Parse a rule document, or report why not.
fn parse_rules(document: &[u8]) -> Result<RuleSet, String> {
    if document.is_empty() {
        return Err("the rule document is empty".to_string());
    }
    RuleSet::from_slice(document, RuleSource::Provided).map_err(|err| err.to_string())
}

pub(crate) fn step(engine: &mut WattEngine) -> i32 {
    // Claim the engine before touching it. `free` may be running on another
    // thread right now — see `WattEngine::in_flight` — and the only safe answer
    // when it is, is to do nothing at all.
    if !engine.enter() {
        set_last_error("the engine is being freed");
        return -1;
    }

    let result = match catch_unwind(AssertUnwindSafe(|| {
        engine.engine.step(Duration::from_millis(100))
    })) {
        Ok(Ok(count)) => count as i32,
        Ok(Err(err)) => {
            // Describe the error by its `kind`, never by its `Display`.
            //
            // `io::Error`'s `Display` dereferences an internal pointer stored in
            // the value's niche-optimised representation. Reading that pointer
            // when the value is not well formed is a segfault in this very
            // function — which is what a device crash log showed:
            //
            //     signal 11 (SIGSEGV), fault addr 0x0, "null pointer dereference"
            //       #00 core::io::error::Error::fmt
            //       #05 watt_ffi::step
            //
            // A native crash here takes the whole app down, and the run loop
            // calls this thousands of times a session, so the one thing this
            // path must never do is fault. `kind()` reads a small field and
            // produces an enum with no pointers in it; it cannot fault, and it
            // still says whether the tunnel went away, a write failed or the
            // poll timed out. The full text is not worth an app-wide crash.
            set_last_error(format!("stepping the engine: {:?}", err.kind()));
            -1
        }
        Err(_) => {
            set_last_error("the engine step panicked");
            -1
        }
    };

    // Always give the claim back, including on the error paths above: a leaked
    // claim would make the engine look permanently busy and the next `free`
    // would refuse for the lifetime of the process.
    engine.leave();
    result
}

pub(crate) fn replace_rules(engine: &mut WattEngine, document: &[u8]) -> bool {
    match parse_rules(document) {
        Ok(rules) => {
            engine.engine.replace_rules(rules);
            true
        }
        Err(message) => {
            set_last_error(message);
            false
        }
    }
}

pub(crate) fn collect_stats(engine: &WattEngine) -> Option<WattStats> {
    let stats = engine.engine.stats();
    Some(WattStats {
        packets_in: stats.packets_in,
        packets_out: stats.packets_out,
        tcp_flows_opened: stats.tcp_flows_opened,
        tcp_flows_closed: stats.tcp_flows_closed,
        tcp_flows_rejected: stats.tcp_flows_rejected,
        tcp_connect_failures: stats.tcp_connect_failures,
        udp_flows_opened: stats.udp_flows_opened,
        udp_flows_evicted: stats.udp_flows_evicted,
        dns_queries: stats.dns_queries,
        dns_answered_locally: stats.dns_answered_locally,
        dns_trimmed: stats.dns_answers_trimmed,
        bytes_client_to_upstream: stats.bytes_client_to_upstream,
        bytes_upstream_to_client: stats.bytes_upstream_to_client,
        live_flows: stats.open_flows(),
        flows_matched_rules: stats.flows_matched_rules,
        flows_direct: stats.flows_direct,
        flows_without_name: stats.flows_without_name,
        flows_named_by_sni: stats.flows_named_by_sni,
        flows_named_without_move: stats.flows_named_without_move,
        flows_named_under_ech: stats.flows_named_under_ech,
        hellos_without_name: stats.hellos_without_name,
        proxy_handshakes: stats.proxy_handshakes,
        proxy_refusals: stats.proxy_refusals,
        dns_upstream_queries: stats.dns_upstream_queries,
        dns_upstream_answered: stats.dns_upstream_answered,
        dns_upstream_failed: stats.dns_upstream_failed,
        dns_upstream_retries: stats.dns_upstream_retries,
        dns_upstream_overflowed: stats.dns_upstream_overflowed,
    })
}

pub(crate) fn uptime_seconds(engine: &WattEngine) -> f64 {
    engine.started.elapsed().as_secs_f64()
}

/// Render the kernel's per-address history and certificate verdicts as JSON.
///
/// Built by hand rather than through a serialiser: this is one fixed shape the
/// shell reads as a plain C string, with a closed set of keys, and the numbers
/// it carries have their own formatting rules — `health` is a ratio the screen
/// shows to three places, not a value that should print as
/// `0.7500000000000001`. Neither array is on a data path: taking the selector
/// lock and the verdict lock copies what they already hold, and nothing here
/// feeds back into a dial decision.
pub(crate) fn ip_stats_json(engine: &WattEngine) -> String {
    // The cooldown the engine is actually running with, not the selector's
    // default. The selector is built with the user's `failure_cooldown` at engine
    // construction (see `build_engine_with_config`), so reading it back here
    // reports the configured value — the whole point is to show what this engine
    // is doing, and the screen must not disagree with the dial path.
    let cooldown = engine
        .engine
        .planner()
        .router()
        .selector()
        .config()
        .failure_cooldown;
    let now = Instant::now();

    let mut json = String::from("{\"addresses\":[");
    for (index, (ip, stat)) in engine.engine.ip_stats().into_iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str("{\"ip\":");
        push_json_string(&mut json, &ip.to_string());
        json.push_str(",\"successes\":");
        json.push_str(&stat.successes.to_string());
        json.push_str(",\"failures\":");
        json.push_str(&stat.failures.to_string());
        json.push_str(",\"silent\":");
        json.push_str(&stat.silent.to_string());
        json.push_str(",\"consecutive_failures\":");
        json.push_str(&stat.consecutive_failures.to_string());
        json.push_str(",\"ewma_rtt_ms\":");
        match stat.ewma_rtt_ms {
            Some(rtt) => json.push_str(&json_float(rtt, 3)),
            // No sample yet. `null`, not zero: a measured round trip of zero
            // milliseconds is not a thing, and the shell has to be able to tell
            // "never measured" from "measured, and it was instant".
            None => json.push_str("null"),
        }
        json.push_str(",\"health\":");
        json.push_str(&json_float(stat.health(), 3));
        json.push_str(",\"in_cooldown\":");
        json.push_str(if stat.in_cooldown(now, cooldown) {
            "true"
        } else {
            "false"
        });
        json.push_str(",\"since_success_ms\":");
        push_json_millis(&mut json, stat.last_success.map(|at| at.elapsed()));
        json.push_str(",\"since_failure_ms\":");
        push_json_millis(&mut json, stat.last_failure.map(|at| at.elapsed()));
        json.push('}');
    }

    json.push_str("],\"certificates\":[");
    for (index, (host, ip, verdict, age)) in
        engine.engine.certificate_verdicts().into_iter().enumerate()
    {
        if index > 0 {
            json.push(',');
        }
        json.push_str("{\"host\":");
        push_json_string(&mut json, &host);
        json.push_str(",\"ip\":");
        push_json_string(&mut json, &ip.to_string());
        json.push_str(",\"verdict\":");
        push_json_string(&mut json, verdict);
        json.push_str(",\"age_ms\":");
        json.push_str(&age.as_millis().to_string());
        json.push('}');
    }
    json.push_str("]}");
    json
}

/// Append `value` to `out` as a JSON string literal, escaped.
///
/// Addresses cannot need escaping, but host names can. A name reaches the
/// verdict table from `dns::read_name`, which maps each raw label byte straight
/// to a `char`, so a query naming a host with a quote or a newline in it would
/// otherwise splice into the document and break it. Escaping here keeps the
/// report valid JSON whatever the query contained — the alternative is a parse
/// failure on the shell side, which reads as "the kernel reported nothing".
fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let code = control as u32;
                out.push_str(&format!("\\u{code:04x}"));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Append an optional span as whole milliseconds, or `null` for "never".
///
/// `null` rather than zero: zero milliseconds is a real age — a success that
/// just happened — and the shell has to be able to tell "just now" from "this
/// address has never succeeded".
fn push_json_millis(out: &mut String, elapsed: Option<Duration>) {
    match elapsed {
        Some(span) => out.push_str(&span.as_millis().to_string()),
        None => out.push_str("null"),
    }
}

/// Format a float for JSON with at most `places` decimals, dropping trailing
/// zeros. JSON cannot spell a bare `1.`, and a screen showing a ratio does not
/// want `0.750` where `0.75` will do.
fn json_float(value: f64, places: usize) -> String {
    format!("{:.1$}", value, places)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

// --- C ABI -----------------------------------------------------------------

/// The last error on this thread, or NULL when there has not been one.
///
/// Valid until the next failing call on the same thread. Never free it.
#[no_mangle]
pub extern "C" fn watt_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| match slot.borrow().as_ref() {
        Some(text) => text.as_ptr(),
        None => ptr::null(),
    })
}

/// Read a caller-supplied string. `NULL` is treated as absent, not as an error.
///
/// # Safety
///
/// `ptr` must be NULL or point at a NUL-terminated string.
unsafe fn optional_str(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
}

/// Build an engine on a descriptor from `VpnService.establish()`.
///
/// Returns NULL on failure; read [`watt_last_error`] for the reason.
///
/// # Safety
///
/// * `tun_fd` must be an open, unowned descriptor for a TUN device.
/// * `tun_name` must be NULL or a NUL-terminated string.
/// * `rules` must point at `rules_len` readable bytes.
/// * `protect` must be a valid callback, and `protect_ctx` must stay valid and
///   usable from another thread for as long as the returned engine lives.
#[no_mangle]
pub unsafe extern "C" fn watt_engine_new(
    tun_fd: c_int,
    tun_name: *const c_char,
    rules: *const u8,
    rules_len: usize,
    protect: Option<WattProtectFn>,
    protect_ctx: *mut c_void,
) -> *mut WattEngine {
    // A panic across the FFI boundary is undefined behaviour, and the shell has
    // no way to see one. Turn it into a NULL and a message instead.
    let built = catch_unwind(AssertUnwindSafe(|| {
        let name = optional_str(tun_name);
        if rules.is_null() || rules_len == 0 {
            return Err("the rule document is empty".to_string());
        }
        let document = std::slice::from_raw_parts(rules, rules_len);

        let Some(protect) = protect else {
            // Refusing is the point: an engine without a protector on Android
            // would relay into itself, and a loop is worse than an error.
            return Err(
                "no protector: on Android the shell must supply VpnService.protect".to_string(),
            );
        };
        // The context is a raw pointer the caller guarantees stays valid, so it
        // is moved into the closure rather than borrowed.
        let context = protect_ctx as usize;
        let handler = move |fd: RawFd| protect(context as *mut c_void, fd) != 0;

        build_engine(tun_fd, name, document, Box::new(handler))
    }));

    match built {
        Ok(Ok(engine)) => Box::into_raw(engine),
        Ok(Err(message)) => {
            set_last_error(message);
            ptr::null_mut()
        }
        Err(_) => {
            set_last_error("the engine constructor panicked");
            ptr::null_mut()
        }
    }
}

/// Run one pass over the tunnel. Returns the number of packets emitted.
///
/// Returns -1 on failure; read [`watt_last_error`].
///
/// The shell drives this from its own thread rather than handing over the loop,
/// so that it keeps control of shutdown and can report progress.
///
/// # Safety
///
/// `engine` must be a pointer from [`watt_engine_new`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn watt_engine_step(engine: *mut WattEngine) -> c_int {
    if engine.is_null() {
        set_last_error("watt_engine_step called with a NULL engine");
        return -1;
    }
    step(&mut *engine)
}

/// Swap in a new rule document, without disturbing flows already running.
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `engine` must be live, and `rules` must point at `rules_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn watt_engine_replace_rules(
    engine: *mut WattEngine,
    rules: *const u8,
    rules_len: usize,
) -> c_int {
    if engine.is_null() {
        set_last_error("watt_engine_replace_rules called with a NULL engine");
        return -1;
    }
    if rules.is_null() || rules_len == 0 {
        set_last_error("the rule document is empty");
        return -1;
    }
    let document = std::slice::from_raw_parts(rules, rules_len);
    if replace_rules(&mut *engine, document) {
        0
    } else {
        -1
    }
}

/// Copy the counters out. Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `engine` must be live and `out` must point at a writable [`WattStats`].
#[no_mangle]
pub unsafe extern "C" fn watt_engine_stats(
    engine: *mut WattEngine,
    out: *mut WattStats,
) -> c_int {
    if engine.is_null() || out.is_null() {
        set_last_error("watt_engine_stats needs both an engine and an output");
        return -1;
    }
    match collect_stats(&*engine) {
        Some(stats) => {
            *out = stats;
            0
        }
        None => {
            set_last_error("the kernel returned no counters");
            -1
        }
    }
}

/// Seconds since the engine was built. For the shell's diagnostics.
///
/// # Safety
///
/// `engine` must be live.
#[no_mangle]
pub unsafe extern "C" fn watt_engine_uptime_seconds(engine: *mut WattEngine) -> f64 {
    if engine.is_null() {
        return 0.0;
    }
    uptime_seconds(&*engine)
}

/// The kernel's per-address history and certificate verdicts, as JSON.
///
/// Read-only: it reports what the kernel has already learned and changes
/// nothing on any data path. Returns NULL when `engine` is NULL.
///
/// The caller owns the returned NUL-terminated buffer and releases it with
/// [`watt_free_buffer`], passing `strlen(buffer) + 1` as the length — the
/// terminator is part of the buffer, so it counts.
///
/// Both keys are always present, even when the arrays are empty, so the shell
/// can decode without a null check per field:
///
/// ```json
/// {
///   "addresses": [
///     { "ip": "140.82.112.6", "successes": 3, "failures": 1, "silent": 0,
///       "consecutive_failures": 0, "ewma_rtt_ms": 247.3, "health": 0.75,
///       "in_cooldown": false, "since_success_ms": 1234,
///       "since_failure_ms": null }
///   ],
///   "certificates": [
///     { "host": "github.com", "ip": "140.82.112.6", "verdict": "covers",
///       "age_ms": 12000 }
///   ]
/// }
/// ```
///
/// # Safety
///
/// `engine` must be a pointer from [`watt_engine_new`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn watt_engine_ip_stats(engine: *mut WattEngine) -> *mut c_char {
    if engine.is_null() {
        return ptr::null_mut();
    }
    let mut bytes = ip_stats_json(&*engine).into_bytes();
    // NUL-terminate so a C caller can read it as a string. The terminator is
    // part of the buffer, which is why the free takes `strlen + 1`.
    bytes.push(0);
    // The same allocation `watt_merge_documents` hands out: a boxed slice whose
    // capacity equals its length, so `watt_free_buffer`'s
    // `Vec::from_raw_parts(buffer, len, len)` reconstructs exactly what was
    // leaked. Anything else — a `CString`, a `Vec` with spare capacity — would
    // be freed with a different layout.
    let mut boxed = bytes.into_boxed_slice();
    let pointer = boxed.as_mut_ptr() as *mut c_char;
    std::mem::forget(boxed);
    pointer
}

/// Release an engine. Passing NULL is a no-op.
///
/// # Safety
///
/// `engine` must be a pointer from [`watt_engine_new`] that has not already been
/// freed. The descriptor it adopted is **not** closed — that stays the shell's.
///
/// A second free of the same pointer is refused rather than performed.
///
/// This is a belt to the shell's braces, and it is here because the braces were
/// missing once. A Kotlin `var handle: Long` read and then zeroed is not a
/// guard: two threads can both read a live value, and both then call this
/// function on one pointer. The result was not a clean double-free report but a
/// corrupted allocator — the app died later and elsewhere, in `RawVec::grow_one`
/// and in a `Planner` drop, which is what made it take a tombstone reading to
/// find. The shell now uses an atomic, so this should never fire; it exists so
/// that if it ever does, the symptom is a message rather than a mystery.
#[no_mangle]
pub unsafe extern "C" fn watt_engine_free(engine: *mut WattEngine) {
    if engine.is_null() {
        return;
    }
    // Read the id *before* dropping the box: it is the identity of the object
    // being freed, and after `Box::from_raw` returns it there is nothing left to
    // read it from. Reading it here also means the claim is taken on this exact
    // engine rather than on whatever `engine` happens to point at, which is the
    // distinction the pointer-keyed version got wrong.
    let free_id = (*engine).free_id;
    if !claim_for_free(free_id) {
        // Not an abort. The caller is already in a broken state, and taking the
        // process down here would hide the reason; leaking one engine is the
        // smaller harm and the message says which pointer leaked.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            log::error!(
                "watt: refusing to free an engine that was already freed ({engine:p}, \
                 id {free_id:?}); the shell called free twice"
            );
        }));
        return;
    }

    // Close the door before opening it: a call that arrives from here on is
    // refused, and one already inside is visible in the counter below.
    if !(*engine).in_flight.close_and_drain(FREE_SPIN_LIMIT) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            log::error!(
                "watt: an engine ({engine:p}) was still being stepped when it was freed; \
                 leaking it rather than dropping memory a live call is reading"
            );
        }));
        return;
    }

    drop(Box::from_raw(engine));
}

/// How long `free` waits for an in-flight call before giving up and leaking.
///
/// Generous compared to any real step: a blocking step returns within a hundred
/// milliseconds, and a protector call is a binder round trip. This exists only
/// so a leaked claim cannot turn shutdown into a hang.
const FREE_SPIN_LIMIT: u32 = 4_000_000;

/// The identity used to decide whether a free has already happened.
///
/// # Why an id and not the pointer
///
/// This guard used to key on the *address*, and an address outlives the object
/// at it. The device showed the consequence:
///
/// ```text
/// watt: refusing to free an engine that was already freed (0x738786478800);
///      the shell called free twice
/// ```
///
/// on a free that was not a double free at all. The sequence is ordinary:
/// engine A is created at address P and freed, P rejoins the allocator, the next
/// `Box::new` lands at **P again** (LIFO reuse within the same size class, which
/// is the common case rather than the exotic one), and when that second engine
/// is freed the address-keyed set still holds P and refuses it.
///
/// The refusal is not merely a wrong log line: refusing means `watt_engine_free`
/// returns *without* dropping the box, so every reconnect-then-disconnect cycle
/// leaked one engine. That is the harm worth removing.
///
/// An id is minted once per object and never reused, so the two cases the
/// address conflated stay distinct: the *same object* freed twice carries the
/// same id and is still refused, while a *new* object at a recycled address
/// carries a new id and is freed normally.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct FreeId(u64);

/// Mint a fresh [`FreeId`].
///
/// `Relaxed` is enough: the only thing that must hold is that no two calls
/// return the same value, and `fetch_add` is a single atomic read-modify-write
/// whose result is unique regardless of how it is ordered against anything else.
/// There is no data being published alongside the id — the object whose id this
/// is is constructed afterwards and handed over by a pointer, whose own
/// happens-before edges already order the field write.
///
/// The counter starts at 1 so that a default-initialised `FreeId(0)` is never a
/// live identity; nothing depends on that today, but a zero that is *not* a real
/// id is one fewer way for a future caller to create a false match.
fn next_free_id() -> FreeId {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    FreeId(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Track which identities have already been freed, so a second free is
/// detectable.
///
/// A `HashSet` behind a `Mutex`, not a lock-free structure: frees are rare
/// (one per tunnel lifetime) and a wrong answer here means memory corruption,
/// so the simple thing is the right thing.
///
/// # Bounds
///
/// The set grows by one entry per freed object and **nothing removes a single
/// entry** — that is not an omission but the point, since a freed id has to stay
/// remembered for a late second free to be caught. When it passes a few thousand
/// entries the whole set is cleared, which buys back the memory at the cost of
/// no longer noticing a double free of an object freed thousands of tunnels ago.
/// That window is the deliberate trade: the set is a diagnostic belt, and an
/// unbounded set would be a leak that grows for the life of the process.
fn claim_for_free(id: FreeId) -> bool {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static FREED: OnceLock<Mutex<HashSet<FreeId>>> = OnceLock::new();
    let freed = FREED.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut set) = freed.lock() else {
        // A poisoned lock means a previous free panicked mid-claim; refusing is
        // the safe direction, since freeing a possibly-live object corrupts.
        return false;
    };
    if set.len() > 4096 {
        set.clear();
    }
    set.insert(id)
}

/// Merge two rule documents into one.
///
/// The upstream publishes two *rule sets* at the short paths `/1` and `/2` (the
/// `/rules` and `/hosts?all=1` endpoints this used to name are retired). They are
/// not subsets of each other, but **merging is not automatically the goal**: `/2`
/// is the S302 hijack block and every one of its 862 addresses is `127.0.0.1`,
/// which this kernel refuses to relay (`Planner::can_relay`). So the app's
/// default merges `/1` with the built-in set, not `/1` with `/2` — see
/// `watt_rules::merge_documents` for the measured 675-domain harm.
///
/// `second_is_hosts` says how to parse the second input. Both default inputs are
/// hosts text (`/1` is fetched in hosts form, because the endpoints'
/// `?format=json` is a schema the kernel cannot parse). The flag exists because
/// the first input is not necessarily hosts text — a caller may hand a real JSON
/// document — and parsing the wrong way fails on the first byte. It is a
/// parameter rather than a guess because a wrong guess produces a confusing parse
/// error instead of an obviously wrong answer.
///
/// Exposed rather than reimplemented in Kotlin because the merge has rules that
/// are easy to get subtly wrong — it is per *domain*, not per entry, because the
/// compiler keeps only the first entry that claims a domain and keying on the
/// entry would discard the addresses the merge exists to keep. The Rust version
/// has tests; a second implementation would not.
///
/// On success `*out_len` is set and the caller owns the buffer, releasing it with
/// [`watt_free_buffer`]. Returns NULL on failure; read [`watt_last_error`].
///
/// # Safety
///
/// Both inputs must point at their stated number of readable bytes, and `out_len`
/// must be writable.
#[no_mangle]
pub unsafe extern "C" fn watt_merge_documents(
    first: *const u8,
    first_len: usize,
    second: *const u8,
    second_len: usize,
    second_is_hosts: c_int,
    out_len: *mut usize,
) -> *mut u8 {
    if first.is_null() || first_len == 0 || second.is_null() || second_len == 0 || out_len.is_null()
    {
        set_last_error("watt_merge_documents needs two documents and an output length");
        return ptr::null_mut();
    }

    let merged = catch_unwind(AssertUnwindSafe(|| {
        let a = watt_rules::parse_document(std::slice::from_raw_parts(first, first_len))
            .map_err(|err| format!("the first document: {err}"))?;
        let b = if second_is_hosts != 0 {
            let raw = std::slice::from_raw_parts(second, second_len);
            let text = std::str::from_utf8(raw)
                .map_err(|err| format!("the hosts document is not UTF-8: {err}"))?;
            watt_rules::parse_hosts(text)
                .map(|(document, _stats)| document)
                .map_err(|err| format!("the hosts document: {err}"))?
        } else {
            watt_rules::parse_document(std::slice::from_raw_parts(second, second_len))
                .map_err(|err| format!("the second document: {err}"))?
        };
        let merged = watt_rules::merge_documents(&[a, b]);
        serde_json::to_vec(&merged).map_err(|err| format!("re-encoding the merge: {err}"))
    }));

    let bytes = match merged {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(message)) => {
            set_last_error(message);
            return ptr::null_mut();
        }
        Err(_) => {
            set_last_error("the merge panicked");
            return ptr::null_mut();
        }
    };

    let mut boxed = bytes.into_boxed_slice();
    *out_len = boxed.len();
    let pointer = boxed.as_mut_ptr();
    std::mem::forget(boxed);
    pointer
}

/// Release a buffer from [`watt_merge_documents`].
///
/// # Safety
///
/// `buffer` must have come from [`watt_merge_documents`], with the same length it
/// reported, and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn watt_free_buffer(buffer: *mut u8, len: usize) {
    if buffer.is_null() || len == 0 {
        return;
    }
    drop(Vec::from_raw_parts(buffer, len, len));
}

/// A running local proxy.
///
/// Separate from [`WattEngine`] because the two modes share nothing but the rule
/// set: the proxy has no tunnel, no protector and no data plane of its own, and
/// pretending otherwise would mean a struct where half the fields are `None` in
/// either mode.
pub struct WattProxy {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    port: u16,
    /// This proxy's own identity, for the same reason [`WattEngine::free_id`]
    /// exists: proxy handles are created and stopped once per session, so an
    /// address they recycle is just as ordinary as one a tunnel recycles, and
    /// the guard they share must not confuse the two.
    free_id: FreeId,
    /// The same `Arc` the serve loop holds, so the rules can be swapped while it
    /// runs. Without this the handle could only be stopped, never retargeted —
    /// and a rule switch in proxy mode would appear to do nothing until the user
    /// reconnected.
    router: Arc<Mutex<watt_rules::Router>>,
}

/// Start a local HTTP proxy on `port`, serving `rules`.
///
/// Returns NULL on failure; read [`watt_last_error`]. Release the handle with
/// [`watt_proxy_stop`].
///
/// This is the path that needs **neither TUN nor root**: an app points its HTTP
/// proxy setting at this port and its requests are relayed — `CONNECT` tunnels
/// unchanged, plain-HTTP requests forwarded to their origin. Domains outside the
/// rule set are not refused; they go out directly, which is what the tunnel does
/// with one. The cost is that only apps that can be pointed at a proxy benefit.
///
/// It takes `rules` and nothing else: the settings document the tunnel reads —
/// timeouts, candidate counts, certificate pre-check, the upstream exit, the
/// upstream resolver — has no effect here. See `watt-proxy`'s module docs.
///
/// # Safety
///
/// `rules` must point at `rules_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn watt_proxy_start(
    port: c_int,
    rules: *const u8,
    rules_len: usize,
) -> *mut WattProxy {
    if rules.is_null() || rules_len == 0 {
        set_last_error("the rule document is empty");
        return ptr::null_mut();
    }
    if !(0..=65535).contains(&port) {
        set_last_error("the port must be between 0 and 65535");
        return ptr::null_mut();
    }

    let started = catch_unwind(AssertUnwindSafe(|| -> Result<*mut WattProxy, String> {
        let document = std::slice::from_raw_parts(rules, rules_len);
        let parsed = parse_rules(document)?;

        // Port 0 means "any free port"; the kernel picks one and the real number
        // is read back from the listener. Binding to a fixed port is what a
        // product does, and asking for 0 is what a test does.
        let listener = TcpListener::bind(("127.0.0.1", port as u16))
            .map_err(|err| format!("binding port {port}: {err}"))?;
        let bound = listener
            .local_addr()
            .map_err(|err| format!("reading the bound port: {err}"))?
            .port();

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let router = Arc::new(Mutex::new(watt_rules::Router::new(parsed)));
        // `serve_until` wants the `Arc`, and the handle keeps a clone so it can
        // reach in later. Cloned before the move, not reconstructed after.
        let handle_router = Arc::clone(&router);
        let worker = thread::Builder::new()
            .name("watt-proxy".to_string())
            .spawn(move || {
                if let Err(err) = watt_proxy::serve_until(listener, router, worker_stop) {
                    set_last_error(format!("the proxy stopped: {err}"));
                }
            })
            .map_err(|err| format!("spawning the proxy thread: {err}"))?;

        Ok(Box::into_raw(Box::new(WattProxy {
            stop,
            worker: Some(worker),
            port: bound,
            free_id: next_free_id(),
            router: handle_router,
        })))
    }));

    match started {
        Ok(Ok(handle)) => handle,
        Ok(Err(message)) => {
            set_last_error(message);
            ptr::null_mut()
        }
        Err(_) => {
            set_last_error("the proxy constructor panicked");
            ptr::null_mut()
        }
    }
}

/// Swap the rule set of a running proxy.
///
/// Returns 0 on success, -1 on failure (read [`watt_last_error`]).
///
/// The point of doing this rather than stopping and restarting: an in-flight
/// CONNECT would be cut, and a user who moved a switch expects the streams they
/// are using to keep working. Only *routing decisions not yet made* change —
/// which is the same promise `Engine::replace_rules` makes for the tunnel.
///
/// # Safety
///
/// `proxy` must be a live handle from [`watt_proxy_start`], and `rules` must
/// point at `rules_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn watt_proxy_replace_rules(
    proxy: *mut WattProxy,
    rules: *const u8,
    rules_len: usize,
) -> c_int {
    if proxy.is_null() {
        set_last_error("watt_proxy_replace_rules called with a NULL proxy");
        return -1;
    }
    if rules.is_null() || rules_len == 0 {
        set_last_error("the rule document is empty");
        return -1;
    }
    let document = std::slice::from_raw_parts(rules, rules_len);
    let parsed = match parse_rules(document) {
        Ok(parsed) => parsed,
        Err(message) => {
            set_last_error(message);
            return -1;
        }
    };
    match (*proxy).router.lock() {
        Ok(mut router) => {
            router.replace_rules(parsed);
            0
        }
        Err(_) => {
            // A poisoned lock means a previous holder panicked mid-update. The
            // rule set is then in an unknown state, and silently writing into it
            // would be worse than reporting the failure.
            set_last_error("the proxy's rule table is poisoned");
            -1
        }
    }
}

/// The port the proxy is actually listening on.
///
/// Worth asking rather than assuming: passing 0 asks the kernel to choose, and
/// the caller has no other way to learn what it chose.
///
/// # Safety
///
/// `proxy` must be a live handle from [`watt_proxy_start`].
#[no_mangle]
pub unsafe extern "C" fn watt_proxy_port(proxy: *mut WattProxy) -> c_int {
    if proxy.is_null() {
        return 0;
    }
    (*proxy).port as c_int
}

/// Stop the proxy and release the handle.
///
/// Blocks until the accept loop has noticed, which is at most one idle poll.
/// Passing NULL is a no-op so teardown can call it unconditionally.
///
/// # Safety
///
/// `proxy` must be a handle from [`watt_proxy_start`] that has not been stopped.
#[no_mangle]
pub unsafe extern "C" fn watt_proxy_stop(proxy: *mut WattProxy) {
    if proxy.is_null() {
        return;
    }
    // See `watt_engine_free` for why this check exists and why it is here
    // rather than only in the shell. The id is read before the box is
    // reconstructed so the claim is on this proxy, not on its address — a
    // stopped proxy's address is recycled by the next `watt_proxy_start` just as
    // readily as a freed engine's is.
    let free_id = (*proxy).free_id;
    if !claim_for_free(free_id) {
        log::error!(
            "watt: refusing to stop a proxy that was already stopped ({proxy:p}, id {free_id:?})"
        );
        return;
    }
    let mut boxed = Box::from_raw(proxy);
    boxed.stop.store(true, Ordering::SeqCst);
    if let Some(worker) = boxed.worker.take() {
        // Joined rather than detached: the caller is about to tear down whatever
        // the proxy was using, and a worker still holding a listener would keep
        // the port occupied past the point the caller believes it is free.
        let _ = worker.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn count_protect(_ctx: *mut c_void, _fd: c_int) -> c_int {
        1
    }

    const DOC: &str = r#"{"meta":{"version":"t"},"groups":[{"group":"g","entries":[
        {"id":"a","name":"a","ips":["203.0.113.10"],"domains":["a.example"]}]}]}"#;

    #[test]
    fn a_missing_protector_is_refused_rather_than_defaulted() {
        // The one thing this ABI must not do is quietly build an engine that
        // will relay into itself. A NULL callback has to be an error.
        let engine = unsafe {
            watt_engine_new(
                -1,
                c"watt0".as_ptr(),
                DOC.as_ptr(),
                DOC.len(),
                None,
                ptr::null_mut(),
            )
        };
        assert!(engine.is_null());
        let message = last_error().expect("an error was recorded");
        assert!(message.contains("protector"), "{message}");
    }

    #[test]
    fn an_empty_document_is_refused() {
        let engine = unsafe {
            watt_engine_new(
                -1,
                ptr::null(),
                ptr::null(),
                0,
                Some(count_protect),
                ptr::null_mut(),
            )
        };
        assert!(engine.is_null());
        let message = last_error().expect("an error was recorded");
        assert!(message.contains("empty"), "{message}");
    }

    #[test]
    fn a_bad_document_is_refused_with_the_reason() {
        let bad = b"not json at all";
        let engine = unsafe {
            watt_engine_new(
                -1,
                ptr::null(),
                bad.as_ptr(),
                bad.len(),
                Some(count_protect),
                ptr::null_mut(),
            )
        };
        assert!(engine.is_null());
        assert!(last_error().is_some());
    }

    #[test]
    fn a_bad_descriptor_is_refused_rather_than_accepted() {
        // The document is fine and the protector is real, so the only thing left
        // to fail is the descriptor — which is -1.
        let engine = unsafe {
            watt_engine_new(
                -1,
                c"watt0".as_ptr(),
                DOC.as_ptr(),
                DOC.len(),
                Some(count_protect),
                ptr::null_mut(),
            )
        };
        assert!(engine.is_null(), "a closed descriptor must not be adopted");
        let message = last_error().expect("an error was recorded");
        assert!(message.contains("descriptor"), "{message}");
    }

    #[test]
    fn null_pointers_are_errors_not_crashes() {
        unsafe {
            assert_eq!(watt_engine_step(ptr::null_mut()), -1);
            assert_eq!(
                watt_engine_replace_rules(ptr::null_mut(), DOC.as_ptr(), DOC.len()),
                -1
            );
            let mut stats = WattStats::default();
            assert_eq!(
                watt_engine_stats(ptr::null_mut(), &mut stats as *mut WattStats),
                -1
            );
            assert_eq!(watt_engine_uptime_seconds(ptr::null_mut()), 0.0);
            // Freeing NULL is explicitly a no-op, so that the shell can call it
            // unconditionally during teardown.
            watt_engine_free(ptr::null_mut());
        }
    }

    #[test]
    fn stats_refuses_a_missing_output() {
        unsafe {
            assert_eq!(watt_engine_stats(ptr::null_mut(), ptr::null_mut()), -1);
        }
    }

    #[test]
    fn claiming_an_id_twice_yields_once() {
        // The property the whole guard rests on. An identity, once claimed, must
        // never be claimable again — that is what makes a second free a no-op
        // instead of a corrupted heap.
        //
        // The claim is keyed on `FreeId` rather than on an address. The bare
        // constant addresses this test used before could not have caught the
        // reuse bug at all: `claim_for_free(0xdeadbeef)` and
        // `claim_for_free(0xdeadbeef)` are the same call whether or not the
        // identity is an address, so the test passed on a guard that was wrong.
        // Minting real ids exercises the ABI the production paths use.
        let id = next_free_id();
        assert!(claim_for_free(id), "the first claim owns the identity");
        assert!(
            !claim_for_free(id),
            "a second claim on the same identity must be refused"
        );
        assert!(!claim_for_free(id), "and refusal is stable, not one-shot");

        // Distinct identities are independent: refusing one must not refuse the
        // next, or the first double-free would leak every engine after it.
        let other = next_free_id();
        assert!(
            claim_for_free(other),
            "an unrelated identity is unaffected"
        );
    }

    #[test]
    fn a_recycled_address_frees_cleanly_because_the_identity_is_not_the_address() {
        // The regression test for the device failure:
        //
        //   watt: refusing to free an engine that was already freed (0x7387...);
        //        the shell called free twice
        //
        // No crash accompanied it and no heap corruption followed, which is the
        // tell: a real double free corrupts and crashes *later and elsewhere*.
        // The sequence that produced it was ordinary --
        //
        //   1. engine A is created at address P and freed. P rejoins the
        //      allocator.
        //   2. the next `Box::new` lands at **P again**. Same size class, freed a
        //      moment ago: this is the common case, not the exotic one.
        //   3. engine B is legitimately freed. The address-keyed guard still held
        //      P, refused, and logged "the shell called free twice" about a shell
        //      that called free exactly as many times as it should have.
        //
        // Refusing was also *harmful*, not merely noisy: `watt_engine_free`
        // returns without dropping the box when the claim is refused, so every
        // cycle that recycled an address leaked one engine.
        //
        // The fix is that the claim keys on a `FreeId`, not on the address, so
        // the two cases the address conflated are now distinct. Two ids stand in
        // for the two engines that shared address P: same address in production,
        // different ids, and both must free.
        let first_engine_at_p = next_free_id();
        assert!(
            claim_for_free(first_engine_at_p),
            "the first engine at P frees"
        );

        let second_engine_at_p = next_free_id();
        assert!(
            claim_for_free(second_engine_at_p),
            "a new engine at the recycled address P must free, not be refused as \
             if it were the first one freed twice -- this is the assertion that \
             fails on the address-keyed guard the device exposed"
        );

        // And the refusal that is still *supposed* to happen, so the fix cannot
        // be mistaken for having disabled the guard: freeing either engine twice
        // is still caught.
        assert!(
            !claim_for_free(first_engine_at_p),
            "a genuine second free of the first engine is still refused"
        );
        assert!(
            !claim_for_free(second_engine_at_p),
            "a genuine second free of the second engine is still refused"
        );
    }

    #[test]
    fn freeing_an_engine_twice_is_refused_rather_than_performed() {
        // The regression test for the double free that corrupted the allocator
        // on the device. The shell's atomic guard means this should not be
        // reachable, but the belt has to hold even when the braces are missing.
        //
        // The engine here is built through the *rejecting* path on purpose.
        // A real engine needs a live TUN descriptor, which a unit test has no
        // way to invent, and a test that cannot obtain a non-NULL pointer would
        // pass vacuously — it would never reach the code it claims to cover.
        // The rejected construction instead gives a NULL, which the ABI treats
        // as a no-op, so this asserts the part that *is* reachable here: that
        // free is idempotent from the caller's point of view. The guard itself
        // is covered directly by `claiming_a_pointer_twice_yields_once`, and the
        // end-to-end version runs on the device.
        unsafe {
            let refused = watt_engine_new(
                -1,
                c"watt0".as_ptr(),
                DOC.as_ptr(),
                DOC.len(),
                None,
                ptr::null_mut(),
            );
            assert!(refused.is_null(), "the fixture relies on this being refused");

            // Both calls must be safe. Freeing NULL is documented as a no-op,
            // and the second call has to be one too.
            watt_engine_free(refused);
            watt_engine_free(refused);
        }
    }

    #[test]
    fn an_empty_settings_document_leaves_the_defaults_alone() {
        let defaults = StackConfig::default();
        let configured = apply_settings(defaults.clone(), "");
        assert_eq!(configured.mtu, defaults.mtu);
        assert_eq!(configured.connect_timeout, defaults.connect_timeout);
    }

    #[test]
    fn settings_that_are_not_json_leave_the_defaults_alone() {
        // A settings file written by a newer build, or edited by hand, must not
        // be able to stop the tunnel from starting.
        let defaults = StackConfig::default();
        let configured = apply_settings(defaults.clone(), "not json at all");
        assert_eq!(configured.mtu, defaults.mtu);
        assert!(last_error().is_some(), "the fallback was recorded");
    }

    #[test]
    fn known_settings_are_applied() {
        let configured = apply_settings(
            StackConfig::default(),
            r#"{"mtu":1280,"connect_timeout_seconds":7,"max_tcp_flows":256,
                "answer_dns_from_rules":false,"observe_dns":false,
                "failure_cooldown_seconds":120,"dial_names":false}"#,
        );
        assert_eq!(configured.mtu, 1280);
        assert_eq!(configured.connect_timeout, Duration::from_secs(7));
        assert_eq!(configured.max_tcp_flows, 256);
        assert!(!configured.answer_dns_from_rules);
        assert!(!configured.observe_dns);
        // The two keys added for the settings screen. Asserted here rather than in
        // a test of their own so the count stays put and the coverage is obvious.
        assert_eq!(configured.failure_cooldown, Duration::from_secs(120));
        assert!(!configured.dial_names);
    }

    #[test]
    fn unknown_keys_and_wrong_types_are_ignored() {
        let defaults = StackConfig::default();
        let configured = apply_settings(
            defaults.clone(),
            r#"{"nonsense":1,"mtu":"big","connect_timeout_seconds":true}"#,
        );
        assert_eq!(configured.mtu, defaults.mtu);
        assert_eq!(configured.connect_timeout, defaults.connect_timeout);
    }

    #[test]
    fn settings_outside_the_sane_range_are_clamped_not_refused() {
        // A 100-byte MTU is not a preference, it is a broken tunnel. Clamping
        // keeps the app usable; refusing would leave the user with a setting they
        // cannot fix from inside the app.
        let tiny = apply_settings(StackConfig::default(), r#"{"mtu":100}"#);
        assert!(tiny.mtu >= 576, "mtu was {}", tiny.mtu);

        let huge = apply_settings(StackConfig::default(), r#"{"mtu":1000000}"#);
        assert!(huge.mtu <= 9000, "mtu was {}", huge.mtu);

        // The failure cooldown is clamped at both ends for the same reason: a
        // zero-second cooldown is "never deprioritize" and an hour-long one is
        // "write the address off", and neither is what the slider offers.
        let too_short = apply_settings(StackConfig::default(), r#"{"failure_cooldown_seconds":1}"#);
        assert_eq!(too_short.failure_cooldown, Duration::from_secs(5));
        let too_long = apply_settings(StackConfig::default(), r#"{"failure_cooldown_seconds":9999}"#);
        assert_eq!(too_long.failure_cooldown, Duration::from_secs(600));
    }

    #[test]
    fn the_per_candidate_dial_moves_with_the_whole_connection_timeout() {
        // The shell exposes one number and the kernel uses two. Deriving the
        // second instead of leaving it at a fixed value is what stops a user who
        // raises the timeout because their server is slow from accidentally
        // shrinking their failover budget.
        let short = apply_settings(StackConfig::default(), r#"{"connect_timeout_seconds":3}"#);
        assert_eq!(short.connect_timeout, Duration::from_secs(3));
        assert_eq!(short.first_connect_timeout, Duration::from_secs(1));

        let long = apply_settings(StackConfig::default(), r#"{"connect_timeout_seconds":60}"#);
        assert_eq!(long.connect_timeout, Duration::from_secs(60));
        assert_eq!(long.first_connect_timeout, Duration::from_secs(3));

        assert!(
            short.first_connect_timeout <= short.connect_timeout
                && long.first_connect_timeout <= long.connect_timeout,
            "a per-candidate wait longer than the whole connection's inverts both numbers"
        );
    }

    #[test]
    fn the_upstream_exit_needs_both_a_protocol_and_an_endpoint() {
        // The exit exists when, and only when, the settings name a protocol this
        // kernel speaks *and* an address it can dial. Each failure below is a state
        // a shell or a hand-edited document can produce by accident, and every one
        // of them has to leave **no** exit rather than a half-built one: a
        // half-built exit is what puts a switch on screen that reads as on while
        // nothing is proxied.
        let exit = |json: &str| apply_settings(StackConfig::default(), json).upstream_proxy;

        assert!(
            exit(r#"{"upstream_proxy_address":"203.0.113.7:1080"}"#).is_none(),
            "an endpoint with no protocol is not an exit"
        );
        assert!(
            exit(r#"{"upstream_proxy_kind":"socks5","upstream_proxy_address":"203.0.113.7:1080"}"#)
                .is_some(),
            "a protocol and a literal endpoint is the one shape that is an exit"
        );
        assert!(
            exit(r#"{"upstream_proxy_kind":"socks4","upstream_proxy_address":"203.0.113.7:1080"}"#)
                .is_none(),
            "a misspelled protocol must not fall back to one this kernel does speak"
        );
        assert!(
            exit(r#"{"upstream_proxy_kind":"socks5","upstream_proxy_address":"proxy.example.com:1080"}"#)
                .is_none(),
            "a name is not an endpoint: nothing here can resolve it"
        );
        assert!(
            exit(r#"{"upstream_proxy_kind":"socks5","upstream_proxy_address":"203.0.113.7"}"#)
                .is_none(),
            "a port is required"
        );
    }

    #[test]
    fn the_upstream_exit_carries_its_protocol_and_credentials() {
        let exit = apply_settings(
            StackConfig::default(),
            r#"{"upstream_proxy_kind":"http-connect","upstream_proxy_address":"203.0.113.7:8080",
                "upstream_proxy_username":"u","upstream_proxy_password":"p"}"#,
        )
        .upstream_proxy
        .expect("the exit must have been configured");

        assert_eq!(exit.kind, ProxyKind::HttpConnect);
        assert_eq!(exit.address, "203.0.113.7:8080".parse().unwrap());
        assert_eq!(exit.credentials(), Some(("u", "p")));

        // A half-filled pair is no pair — the kernel's rule, asserted here because
        // this is the layer that hands the strings over, and a second opinion about
        // what half a pair means is exactly the kind of copy that drifts. Note that
        // the *exit* survives: a missing password is a reason to offer no
        // credentials, not a reason to drop the endpoint the user typed.
        let half = apply_settings(
            StackConfig::default(),
            r#"{"upstream_proxy_kind":"socks5","upstream_proxy_address":"203.0.113.7:1080",
                "upstream_proxy_username":"u"}"#,
        )
        .upstream_proxy
        .expect("a missing password is not a reason to drop the exit");
        assert_eq!(half.credentials(), None);
    }

    #[test]
    fn a_one_second_timeout_still_leaves_a_usable_per_candidate_wait() {
        // The clamp matters at the bottom of the range: a third of one second is
        // a third of a second, which is short enough that a live server on a
        // mobile link would be abandoned mid-handshake.
        let configured = apply_settings(StackConfig::default(), r#"{"connect_timeout_seconds":1}"#);
        assert_eq!(configured.first_connect_timeout, Duration::from_secs(1));
        assert!(configured.first_connect_timeout <= configured.connect_timeout);
    }

    #[test]
    fn the_race_window_is_parsed_and_clamped() {
        // The three knobs the shell exposes for happy-eyeballs racing.
        let configured = apply_settings(
            StackConfig::default(),
            r#"{"race_width":2,"race_launch_milliseconds":300,"max_dialing":64}"#,
        );
        assert_eq!(configured.race_width, 2);
        assert_eq!(configured.race_launch_interval, Duration::from_millis(300));
        assert_eq!(configured.max_dialing, 64);

        // Out of range is clamped, not refused: a settings file edited by hand
        // must not be able to stop the tunnel.
        let clamped = apply_settings(
            StackConfig::default(),
            r#"{"race_width":99,"race_launch_milliseconds":99999,"max_dialing":100000}"#,
        );
        assert_eq!(clamped.race_width, 4);
        assert_eq!(clamped.race_launch_interval, Duration::from_millis(1000));
        assert_eq!(clamped.max_dialing, 1024);
    }

    #[test]
    fn a_race_width_of_one_is_the_serial_rollback() {
        // One has to survive as the one-key way back to the old serial dial, and
        // zero has to clamp up to it rather than to an empty window.
        assert_eq!(
            apply_settings(StackConfig::default(), r#"{"race_width":1}"#).race_width,
            1
        );
        assert_eq!(
            apply_settings(StackConfig::default(), r#"{"race_width":0}"#).race_width,
            1,
            "zero has to mean the serial path, not a window nothing can fill"
        );
    }

    #[test]
    fn a_claim_is_granted_and_released() {
        let gate = InFlight::new();
        assert!(gate.enter(), "an idle engine can be claimed");
        gate.leave();
        assert!(gate.enter(), "the claim can be taken again");
    }

    #[test]
    fn a_free_closes_the_door_on_further_claims() {
        // The regression test for the device crash: `#00 BoxedProtector::protect`
        // under a `TcpRelay::service` that was still running when the teardown
        // freed the engine. Once a free is claimed, no new work may start.
        let gate = InFlight::new();
        assert!(gate.close_and_drain(1_000_000), "an idle engine drains at once");
        assert!(!gate.enter(), "a claim after the free is refused");
    }

    #[test]
    fn a_free_waits_for_work_already_inside() {
        // The other half of the same race. A claim taken *before* the free must
        // be honoured: `free` has to see it and wait, not free underneath it.
        let gate = InFlight::new();
        assert!(gate.enter(), "work is inside");

        // A budget of zero polls cannot succeed while a claim is outstanding, so
        // this returning false is the gate refusing to free into live work.
        assert!(
            !gate.close_and_drain(0),
            "a free must not proceed while a call is in flight"
        );

        gate.leave();
        assert!(
            gate.close_and_drain(1_000_000),
            "once the work leaves, the free proceeds"
        );
    }

    #[test]
    fn claims_and_frees_can_race_without_handing_out_a_live_claim() {
        // The exact interleaving the check after the add exists for: `close` sets
        // the door between a claim's first check and its add. Simulated
        // directly, because the window is a few instructions wide and a threaded
        // test would only sometimes hit it — a flaky test would be worse than no
        // test, and this pins the invariant deterministically.
        let gate = InFlight::new();

        // Put the gate in the state `enter`'s fast path would see as open.
        assert!(!gate.closing.load(Ordering::Acquire));

        // `close` shuts the door now, while nothing is in flight.
        gate.closing.store(true, Ordering::Release);

        // The claim's add now lands behind the shut door.
        gate.busy.fetch_add(1, Ordering::AcqRel);

        // The second check is what notices. Doing `enter`'s tail by hand keeps
        // the interleaving we want instead of hoping for it.
        assert!(
            gate.closing.load(Ordering::Acquire),
            "the door shut while the claim was in between its two checks"
        );
        gate.busy.fetch_sub(1, Ordering::AcqRel);
        assert_eq!(
            gate.busy.load(Ordering::Acquire),
            0,
            "giving the claim back must leave the count at zero so the free can proceed"
        );
        assert!(!gate.enter(), "the shut door refuses the next claim");
        assert!(
            gate.close_and_drain(1_000_000),
            "and the free proceeds once the count is zero"
        );
    }

    /// A second document naming a different host, for the swap tests.
    const OTHER_DOC: &str = r#"{"meta":{"version":"t"},"groups":[{"group":"g","entries":[
        {"id":"b","name":"b","ips":["203.0.113.20"],"domains":["b.example"]}]}]}"#;

    #[test]
    fn a_running_proxy_takes_a_new_rule_set_without_being_restarted() {
        // The bug this guards: the reload path only knew about the tunnel engine,
        // so in proxy mode a rule switch was written to preferences and never
        // reached the running router. The switch looked applied and was not.
        let proxy = unsafe {
            watt_proxy_start(0, DOC.as_ptr(), DOC.len())
        };
        assert!(!proxy.is_null(), "{:?}", last_error());

        let port_before = unsafe { watt_proxy_port(proxy) };
        assert!(port_before > 0);

        let result = unsafe {
            watt_proxy_replace_rules(proxy, OTHER_DOC.as_ptr(), OTHER_DOC.len())
        };
        assert_eq!(result, 0, "{:?}", last_error());

        // The port must not move: a reload is not a restart, and a client
        // already pointing at it has no way to learn a new one.
        assert_eq!(
            unsafe { watt_proxy_port(proxy) },
            port_before,
            "replacing the rules must not rebind the listener"
        );

        unsafe { watt_proxy_stop(proxy) };
    }

    #[test]
    fn replacing_a_proxys_rules_with_a_bad_document_is_refused_and_keeps_the_old_ones() {
        let proxy = unsafe { watt_proxy_start(0, DOC.as_ptr(), DOC.len()) };
        assert!(!proxy.is_null(), "{:?}", last_error());

        // A document the rule compiler cannot read. Refusing it has to leave the
        // proxy serving the good set rather than half-applying garbage.
        let broken = b"not a rules document at all";
        let result = unsafe {
            watt_proxy_replace_rules(proxy, broken.as_ptr(), broken.len())
        };
        assert_eq!(result, -1, "a malformed document must be refused");
        assert!(last_error().is_some());

        // Still alive and still listening on the same port.
        assert!(unsafe { watt_proxy_port(proxy) } > 0);

        unsafe { watt_proxy_stop(proxy) };
    }

    #[test]
    fn replacing_rules_on_a_null_proxy_is_refused_rather_than_crashing() {
        let result = unsafe {
            watt_proxy_replace_rules(ptr::null_mut(), DOC.as_ptr(), DOC.len())
        };
        assert_eq!(result, -1);
        assert!(last_error().is_some());
    }

    #[test]
    fn the_report_escapes_names_and_leaves_missing_samples_null() {
        // The report is the one document the shell parses by hand, so its
        // encoding is worth pinning directly. A host name reaches the table from
        // `dns::read_name`, which maps each raw label byte to a `char` — so a
        // name can carry a quote or a newline, and splicing it in raw would
        // produce a document the shell cannot parse. A parse failure reads as
        // "the kernel reported nothing", which is exactly the wrong answer.
        let mut text = String::new();
        push_json_string(&mut text, "ex\"ample\\co\n");
        assert_eq!(text, r#""ex\"ample\\co\n""#);

        // `null`, not zero: zero milliseconds is a real age — a success that
        // just happened — and the shell has to tell "just now" from "never".
        let mut millis = String::new();
        push_json_millis(&mut millis, None);
        assert_eq!(millis, "null");
        let mut millis = String::new();
        push_json_millis(&mut millis, Some(Duration::from_millis(1234)));
        assert_eq!(millis, "1234");

        // At most three places, trailing zeros dropped, and never a bare `1.`.
        assert_eq!(json_float(0.75, 3), "0.75");
        assert_eq!(json_float(1.0, 3), "1");
        assert_eq!(json_float(247.2999, 3), "247.3");
        assert_eq!(json_float(0.0, 3), "0");
    }
}

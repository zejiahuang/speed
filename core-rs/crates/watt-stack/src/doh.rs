//! Resolving names through an encrypted upstream.
//!
//! # Why this exists
//!
//! Everything else in this kernel decides *where* a query goes or *which*
//! address a name maps to. Neither is enough on the network this was written
//! against, and the measurement that settled it is worth stating plainly
//! (`memory/2026-10-02.md` §14):
//!
//! ```text
//! one resolver, three transports -- plain UDP/53, plain TCP/53, DoH -- and all
//! three returned the same forged addresses. Changing the transport changed
//! nothing. The forgery is produced by the resolver, not by the path to it.
//! ```
//!
//! So this module is **not** "enable DoH". Encryption is not the variable; the
//! **resolver** is. What matters is that the recursive exit is outside the wall
//! and that its own name is not blocked. DoH is only how that resolver is
//! spoken to, and it is used here because port 53 to a foreign resolver is
//! intercepted while 443 to a name that is not blocked is not.
//!
//! # What this is not
//!
//! Not interception, and not a change to what the client sees. The kernel asks
//! a resolver a question the client already asked and hands the client the
//! answer. No client traffic is decrypted; the DoH session is the kernel's own.
//! See `memory/NET-NOTES.md`, "架构边界：不解密 TLS".
//!
//! # Why a thread and not the poll loop
//!
//! A DoH exchange is a full TLS session, which the poll loop cannot drive
//! without becoming a TLS state machine. [`crate::verify`] already settled this
//! question for the certificate probe: the work goes on a detached thread and
//! the answer is published into shared state that the kernel drains on its next
//! pass. This does the same, for the same reason.
//!
//! The sockets here are plain `std::net` ones, deliberately. The shell registers
//! its own package with `addDisallowedApplication`, so this process's traffic
//! never enters the tunnel it is running — which is the same property that lets
//! `verify`'s probe use a plain socket.
//!
//! # Why the retry is not optional
//!
//! A DoH gateway is measurably not reliable per request. Cloudflare's answered
//! between 67% and 100% of identical requests within one sitting. A resolver
//! that fails a third of the time is worse than no resolver at all: the client
//! has already given up by the time the failure is known, and it has no way to
//! tell "the name does not exist" from "the kernel dropped it". So a failed
//! attempt is retried, and the whole resolution is bounded by [`DEADLINE`] — a
//! resolution that outlives the client's own timeout is work done for nobody.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::dns;

/// The path a DoH endpoint is served on unless the URL says otherwise.
const DEFAULT_PATH: &str = "/dns-query";

/// How many attempts one resolution gets, the first included.
const ATTEMPTS: usize = 3;

/// How long the whole resolution -- every attempt and every backoff -- may take.
///
/// A DNS client gives up well before this. Retrying past its patience turns a
/// failure the client could have worked around into a silent black hole.
const DEADLINE: Duration = Duration::from_secs(5);

/// Pause between attempts. Short: the point is to step over a lost request, not
/// to wait out an outage, and the endpoint's own failures are per-request.
/// Doubled per attempt (capped by [`DEADLINE`]), so a second attempt that also
/// times out does not spend the whole budget on the same wait.
const BACKOFF: Duration = Duration::from_millis(150);

/// Consecutive failed *resolutions* before the endpoint is treated as unhealthy.
///
/// One failed resolution is a lost request; three in a row, with no answer in
/// between, is an endpoint that is not reachable from here. At that point trying
/// every attempt again for every client question turns a DNS outage into a
/// handshake storm — the client has already timed out, and the extra sessions
/// buy nothing.
const FAILURES_BEFORE_COOLDOWN: u64 = 3;

/// First cooldown after an endpoint is considered unhealthy. Doubles with every
/// further consecutive failure, up to [`MAX_COOLDOWN`], so a long outage settles
/// down instead of staying at the initial rate.
const FIRST_COOLDOWN: Duration = Duration::from_secs(5);

/// Longest time an unhealthy endpoint is still only tried once per resolution.
const MAX_COOLDOWN: Duration = Duration::from_secs(60);

/// How many resolutions may be in flight at once.
///
/// A ceiling on threads rather than on queries: every in-flight resolution owns
/// one. Past this the caller falls back to forwarding, which is a worse answer
/// but an answer.
const MAX_IN_FLIGHT: usize = 32;

/// Largest reply head that will be buffered before the exchange is abandoned.
const MAX_HEAD: usize = 8 * 1024;

/// Largest answer body that will be accepted. A DNS message is nowhere near it;
/// this is the bound that stops a hostile endpoint turning a query into an
/// allocation.
const MAX_BODY: usize = 64 * 1024;

/// Connect timeout within one attempt. The rest of the budget is the exchange.
const CONNECT_BUDGET: Duration = Duration::from_millis(1500);

/// A configured DoH endpoint.
///
/// The address is a **literal**, never a name. A name would have to be resolved
/// before it could be connected to, and the resolver is the thing being
/// configured — that circle is why [`DohEndpoint::parse`] takes a bootstrap
/// address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DohEndpoint {
    /// Where the kernel's socket connects. This is the address that has to be
    /// reachable, not any name.
    pub addr: SocketAddr,
    /// The name for SNI and the `Host` header. **Empty when the address is its
    /// own identity**, which is what a URL like `https://1.1.1.1/dns-query`
    /// means: the certificate carries the address, and inventing a name for it
    /// would fail a check that should pass.
    pub host: String,
    /// Request path, beginning with `/`.
    pub path: String,
}

impl DohEndpoint {
    /// Read an endpoint out of a URL and an optional bootstrap address.
    ///
    /// Returns `None` for anything it cannot fully understand, rather than
    /// guessing. A half-understood endpoint is a resolver that is configured, a
    /// switch that says it is on, and no way to tell that every query is being
    /// sent somewhere unintended.
    ///
    /// `bootstrap` is only consulted when the URL's authority is a name. It is
    /// the same endpoint, stated as an address, and it is what breaks the
    /// resolve-the-resolver circle.
    pub fn parse(url: &str, bootstrap: Option<IpAddr>) -> Option<Self> {
        // `http://` is refused rather than accepted. A plaintext DoH endpoint is
        // reachable by exactly the interception this exists to avoid, so
        // accepting one would be offering a control that cannot do its job.
        let rest = url.trim().strip_prefix("https://")?;
        if rest.is_empty() {
            return None;
        }

        // Split authority from path at the first `/`.
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, DEFAULT_PATH),
        };
        if authority.is_empty() {
            return None;
        }

        // An explicit port is honoured; otherwise 443. A bare `:` in an
        // authority is always a port -- a host name cannot contain one -- so
        // the only case needing care is an IPv6 literal, which is bracketed.
        let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
            let (address, tail) = bracketed.split_once(']')?;
            let port = match tail.strip_prefix(':') {
                Some(text) => text.parse::<u16>().ok()?,
                None if tail.is_empty() => 443,
                None => return None,
            };
            (address.to_string(), port)
        } else if let Some((head, tail)) = authority.rsplit_once(':') {
            (head.to_string(), tail.parse::<u16>().ok()?)
        } else {
            (authority.to_string(), 443)
        };

        let literal: Option<IpAddr> = host.parse().ok();
        let addr = match literal {
            Some(ip) => SocketAddr::new(ip, port),
            None => SocketAddr::new(bootstrap?, port),
        };

        Some(Self {
            addr,
            // Lower-cased because a name is case-insensitive and both the SNI
            // and the `Host` header are compared by servers that may not be.
            host: match literal {
                Some(_) => String::new(),
                None => host.to_ascii_lowercase(),
            },
            path: path.to_string(),
        })
    }

    /// A one-line description for logs and the status screen.
    pub fn describe(&self) -> String {
        if self.host.is_empty() {
            format!("https://{}{}", self.addr, self.path)
        } else {
            format!("https://{}{} via {}", self.host, self.path, self.addr)
        }
    }

    /// The `Host` header for a request, which carries the port when it is not
    /// the default one.
    fn host_header(&self) -> String {
        let name = if self.host.is_empty() {
            self.addr.ip().to_string()
        } else {
            self.host.clone()
        };
        if self.addr.port() == 443 {
            name
        } else {
            format!("{name}:{}", self.addr.port())
        }
    }

    /// The name to validate the certificate against.
    fn server_name(&self) -> Option<ServerName<'static>> {
        if self.host.is_empty() {
            return Some(ServerName::IpAddress(self.addr.ip().into()));
        }
        ServerName::try_from(self.host.clone()).ok()
    }
}

/// Identifies one client question, and carries everything needed to answer it.
///
/// A waiter is the key for "is this already being asked": a client that repeats
/// a query while the first is in flight must not start a second resolution, and
/// the answer has to find its way back to the exact socket that asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Waiter {
    /// The client's address, as the tunnel saw it.
    pub client: SocketAddr,
    /// The address the reply must appear to come from — the tunnel's own.
    pub from: IpAddr,
    /// The DNS transaction id, which the answer has to echo.
    pub id: u16,
}

/// What came back for one question.
#[derive(Debug)]
pub struct Answer {
    pub waiter: Waiter,
    /// The upstream's response, verbatim, or the reason there is none.
    ///
    /// Verbatim on purpose: the query was sent as the client wrote it, so the
    /// response already carries the client's transaction id and there is no
    /// second encoder here to disagree with the one in [`crate::dns`].
    pub outcome: Result<Vec<u8>, &'static str>,
}

/// A snapshot of what a [`DohResolver`] has done, for folding into `Stats`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DohCounters {
    /// Questions handed to the upstream.
    pub queries: u64,
    /// Resolutions that produced an answer.
    pub answered: u64,
    /// Resolutions that used every attempt and still failed.
    pub failed: u64,
    /// Attempts after the first. Non-zero is normal; a value near `queries` is
    /// an endpoint that is barely working.
    pub retries: u64,
    /// Questions the upstream was not offered, because too many were already in
    /// flight. Each one was forwarded instead.
    pub overflowed: u64,
}

#[derive(Default)]
struct Counters {
    queries: AtomicU64,
    answered: AtomicU64,
    failed: AtomicU64,
    retries: AtomicU64,
    overflowed: AtomicU64,
}

/// What the outcome of one resolution did to the endpoint's health.
///
/// Returned so the caller can log *changes* rather than every failure: an
/// unreachable endpoint would otherwise write a line per client question, which
/// is the log equivalent of the handshake storm this exists to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthChange {
    /// Nothing worth a line: a failure while already unhealthy, or a success
    /// while healthy.
    None,
    /// The first failure after a healthy period.
    FirstFailure,
    /// Enough consecutive failures to stop retrying every attempt.
    EnteredCooldown,
    /// A success after one or more failures.
    Recovered,
}

/// Tracks whether the endpoint is answering, so resolutions can back off.
///
/// A `streak` of zero means healthy. Once [`FAILURES_BEFORE_COOLDOWN`] is
/// reached, `open_until` holds the end of the cooldown and further consecutive
/// failures widen it exponentially. The endpoint is never disabled: every
/// resolution still makes one encrypted attempt, so it recovers on its own.
#[derive(Default)]
struct Health {
    streak: AtomicU64,
    open_until: Mutex<Option<Instant>>,
}

impl Health {
    /// Attempts one resolution gets right now.
    ///
    /// One during cooldown. Not zero: the endpoint may have recovered a second
    /// ago, and the first question after a recovery should get an answer rather
    /// than be forwarded in plaintext because a breaker said so.
    fn attempts(&self) -> usize {
        match self.open_until.lock() {
            // A poisoned lock means a thread panicked holding it. Treat the
            // endpoint as healthy rather than as permanently degraded.
            Ok(open) if open.is_some_and(|end| end > Instant::now()) => 1,
            _ => ATTEMPTS,
        }
    }

    /// The cooldown still to run, if the endpoint is unhealthy.
    ///
    /// Test-only: production code asks [`attempts`](Self::attempts), which is
    /// what the resolution loop needs. Keeping this lets the cooldown's growth
    /// be asserted without sleeping through it.
    #[cfg(test)]
    fn remaining(&self) -> Option<Duration> {
        let open = self.open_until.lock().ok()?;
        let end = (*open)?;
        Some(end.saturating_duration_since(Instant::now()))
    }

    fn record_success(&self) -> HealthChange {
        let streak = self.streak.swap(0, Ordering::Relaxed);
        if let Ok(mut open) = self.open_until.lock() {
            *open = None;
        }
        if streak == 0 { HealthChange::None } else { HealthChange::Recovered }
    }

    fn record_failure(&self) -> HealthChange {
        let streak = self.streak.fetch_add(1, Ordering::Relaxed) + 1;
        if streak == 1 {
            return HealthChange::FirstFailure;
        }
        if streak < FAILURES_BEFORE_COOLDOWN {
            return HealthChange::None;
        }

        // 5s, 10s, 20s, ... capped. The exponent is bounded before the shift so
        // a very long outage cannot overflow it.
        let doublings = (streak - FAILURES_BEFORE_COOLDOWN).min(4) as u32;
        let cooldown = (FIRST_COOLDOWN * (1u32 << doublings)).min(MAX_COOLDOWN);
        if let Ok(mut open) = self.open_until.lock() {
            *open = Some(Instant::now() + cooldown);
        }
        if streak == FAILURES_BEFORE_COOLDOWN {
            HealthChange::EnteredCooldown
        } else {
            HealthChange::None
        }
    }
}

#[derive(Default)]
struct State {
    /// Answers that have come back and not yet been handed to a client.
    ready: Vec<Answer>,
    /// Questions with a resolution in flight.
    in_flight: HashSet<Waiter>,
}

/// Resolves names through one configured upstream.
pub struct DohResolver {
    endpoint: DohEndpoint,
    config: Arc<ClientConfig>,
    state: Arc<Mutex<State>>,
    counters: Arc<Counters>,
    health: Arc<Health>,
}

impl std::fmt::Debug for DohResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let in_flight = self.state.lock().map(|state| state.in_flight.len()).unwrap_or(0);
        f.debug_struct("DohResolver")
            .field("endpoint", &self.endpoint.describe())
            .field("in_flight", &in_flight)
            .finish_non_exhaustive()
    }
}

impl DohResolver {
    /// Build a resolver for one endpoint.
    pub fn new(endpoint: DohEndpoint) -> Self {
        // The provider is named rather than taken from the process default, so
        // the kernel does not require a global to have been installed by
        // whoever embeds it.
        let config = Arc::new(
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("the ring provider supports the default protocol versions")
                .with_root_certificates(RootCertStore {
                    roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
                })
                .with_no_client_auth(),
        );
        Self {
            endpoint,
            config,
            state: Arc::new(Mutex::new(State::default())),
            counters: Arc::new(Counters::default()),
            health: Arc::new(Health::default()),
        }
    }

    /// The endpoint this resolver speaks to.
    pub fn endpoint(&self) -> &DohEndpoint {
        &self.endpoint
    }

    /// Start resolving `query`, unless the same question is already in flight.
    ///
    /// Returns `false` when the request was not started — the in-flight ceiling
    /// was reached, or this exact question is already being asked. The caller
    /// forwards the query as it would have without an upstream: a worse answer,
    /// but an answer.
    pub fn resolve(&self, waiter: Waiter, query: &[u8]) -> bool {
        {
            let Ok(mut state) = self.state.lock() else {
                return false;
            };
            if state.in_flight.len() >= MAX_IN_FLIGHT {
                self.counters.overflowed.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            if state.in_flight.contains(&waiter) {
                return false;
            }
            state.in_flight.insert(waiter);
        }
        self.counters.queries.fetch_add(1, Ordering::Relaxed);

        let endpoint = self.endpoint.clone();
        let config = Arc::clone(&self.config);
        let state = Arc::clone(&self.state);
        let counters = Arc::clone(&self.counters);
        let health = Arc::clone(&self.health);
        let query = query.to_vec();

        let spawned = std::thread::Builder::new()
            .name("watt-doh".to_string())
            .spawn(move || {
                // Read before the first attempt: once the endpoint is in
                // cooldown this resolution gets one attempt instead of three.
                let attempts = health.attempts();
                let outcome = attempt_all(&endpoint, &config, &query, &counters, attempts);
                match &outcome {
                    Ok(_) => {
                        counters.answered.fetch_add(1, Ordering::Relaxed);
                        if health.record_success() == HealthChange::Recovered {
                            log::info!("watt: doh {} is answering again", endpoint.describe());
                        }
                    }
                    Err(reason) => {
                        counters.failed.fetch_add(1, Ordering::Relaxed);
                        // Only state changes are logged. A resolution per client
                        // question would otherwise fill the log for as long as
                        // the outage lasts, which is what the cooldown is for.
                        match health.record_failure() {
                            HealthChange::FirstFailure => {
                                log::info!("watt: doh {} failed: {}", endpoint.describe(), reason);
                            }
                            HealthChange::EnteredCooldown => {
                                log::warn!(
                                    "watt: doh {} failed {} times in a row; trying once per query",
                                    endpoint.describe(),
                                    FAILURES_BEFORE_COOLDOWN
                                );
                            }
                            HealthChange::None | HealthChange::Recovered => {}
                        }
                    }
                }
                if let Ok(mut state) = state.lock() {
                    state.in_flight.remove(&waiter);
                    state.ready.push(Answer { waiter, outcome });
                }
            });

        if spawned.is_err() {
            // Out of threads. Give the slot back so a later query can try, and
            // let the caller fall back rather than wait for an answer that will
            // never be produced.
            if let Ok(mut state) = self.state.lock() {
                state.in_flight.remove(&waiter);
            }
            return false;
        }
        true
    }

    /// Collect the answers that have come back since the last call.
    pub fn take_ready(&self) -> Vec<Answer> {
        match self.state.lock() {
            Ok(mut state) => std::mem::take(&mut state.ready),
            // A poisoned lock means a thread panicked holding it. Returning
            // nothing is the only option that does not propagate the panic into
            // the engine loop.
            Err(_) => Vec::new(),
        }
    }

    /// Read and reset the counters, so the engine can fold them into its own.
    pub fn take_counters(&self) -> DohCounters {
        DohCounters {
            queries: self.counters.queries.swap(0, Ordering::Relaxed),
            answered: self.counters.answered.swap(0, Ordering::Relaxed),
            failed: self.counters.failed.swap(0, Ordering::Relaxed),
            retries: self.counters.retries.swap(0, Ordering::Relaxed),
            overflowed: self.counters.overflowed.swap(0, Ordering::Relaxed),
        }
    }
}

/// Resolve once, retrying until the attempts or the deadline run out.
fn attempt_all(
    endpoint: &DohEndpoint,
    config: &Arc<ClientConfig>,
    query: &[u8],
    counters: &Counters,
    attempts: usize,
) -> Result<Vec<u8>, &'static str> {
    let started = Instant::now();
    let mut last = "unreachable";

    for attempt in 0..attempts {
        if attempt > 0 {
            counters.retries.fetch_add(1, Ordering::Relaxed);
            // Never sleep past the deadline: a pause that outlives the client's
            // patience is the same as a failed attempt, but slower.
            let wait = backoff_for(attempt).min(DEADLINE.saturating_sub(started.elapsed()));
            std::thread::sleep(wait);
        }
        let left = DEADLINE.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        match attempt_once(endpoint, config, query, left) {
            Ok(body) => return Ok(body),
            Err(reason) => last = reason,
        }
    }
    Err(last)
}

/// The pause before attempt `attempt` of one resolution.
///
/// Linear in the attempt index would keep retrying an endpoint that just timed
/// out at the same pace; doubling gives a lost request a second chance without
/// spending the entire budget waiting. The exponent is bounded before the shift
/// so this cannot overflow on a caller that asks for many attempts.
fn backoff_for(attempt: usize) -> Duration {
    BACKOFF * (1u32 << attempt.min(4))
}

/// One attempt: connect, handshake, exchange, and check what came back.
fn attempt_once(
    endpoint: &DohEndpoint,
    config: &Arc<ClientConfig>,
    query: &[u8],
    budget: Duration,
) -> Result<Vec<u8>, &'static str> {
    let name = endpoint.server_name().ok_or("bad endpoint name")?;
    let conn = ClientConnection::new(Arc::clone(config), name).map_err(|_| "tls setup")?;

    let socket = TcpStream::connect_timeout(&endpoint.addr, budget.min(CONNECT_BUDGET))
        .map_err(|_| "unreachable")?;
    let _ = socket.set_read_timeout(Some(budget));
    let _ = socket.set_write_timeout(Some(budget));
    let _ = socket.set_nodelay(true);
    let mut tls = StreamOwned::new(conn, socket);

    // The body is the client's own query bytes. Sending them verbatim is what
    // makes the response usable without re-encoding: the transaction id, the
    // question, the EDNS0 size the client advertised and any options it set all
    // travel through untouched.
    let head = format!(
        "POST {} HTTP/1.1\r\n\
         Host: {}\r\n\
         Accept: application/dns-message\r\n\
         Content-Type: application/dns-message\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        endpoint.path,
        endpoint.host_header(),
        query.len()
    );
    tls.write_all(head.as_bytes()).map_err(|_| "write failed")?;
    tls.write_all(query).map_err(|_| "write failed")?;
    tls.flush().map_err(|_| "write failed")?;

    let (status, length) = read_head(&mut tls)?;
    if status != 200 {
        return Err("refused");
    }
    let body = read_body(&mut tls, length)?;

    // The answer has to be a response to the question that was asked. A gateway
    // that returns someone else's answer, or a query, is not answering this one,
    // and handing that to the client would be worse than reporting a failure.
    let message = dns::Message::parse(&body).map_err(|_| "not dns")?;
    if message.is_query() {
        return Err("not a response");
    }
    if message.id != dns::Message::parse(query).map_err(|_| "not dns")?.id {
        return Err("wrong transaction");
    }
    Ok(body)
}

/// Read one byte, mapping a socket timeout to a refusal.
///
/// A timeout is not an end of stream. `Ok(0)` would leave the header loop early
/// and produce a truncated request line, which then parses as a refusal for
/// entirely the wrong reason.
fn read_byte(stream: &mut impl Read) -> Result<u8, &'static str> {
    let mut one = [0u8; 1];
    loop {
        match stream.read(&mut one) {
            Ok(0) => return Err("closed"),
            Ok(_) => return Ok(one[0]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("timeout"),
        }
    }
}

/// Read the status line and headers, one byte at a time.
///
/// One byte at a time because the body follows the blank line immediately and
/// must not be consumed here — the same rule, and the same reason, as
/// `upstream_proxy`'s handshake. The cost is a few hundred reads on a path that
/// runs once per query.
fn read_head(stream: &mut impl Read) -> Result<(u16, Option<usize>), &'static str> {
    let mut head: Vec<u8> = Vec::with_capacity(256);
    loop {
        head.push(read_byte(stream)?);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > MAX_HEAD {
            return Err("oversized reply");
        }
    }

    let text = std::str::from_utf8(&head).map_err(|_| "not http")?;
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("no status")?;
    let length = text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    });
    Ok((status, length))
}

/// Read the body, bounded either by `Content-Length` or by the end of stream.
fn read_body(stream: &mut impl Read, length: Option<usize>) -> Result<Vec<u8>, &'static str> {
    match length {
        Some(size) => {
            if size > MAX_BODY {
                return Err("oversized answer");
            }
            let mut body = vec![0u8; size];
            let mut filled = 0;
            while filled < size {
                match stream.read(&mut body[filled..]) {
                    Ok(0) => return Err("truncated"),
                    Ok(read) => filled += read,
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return Err("timeout"),
                }
            }
            Ok(body)
        }
        // No `Content-Length`: the request asked for `Connection: close`, so the
        // body ends where the stream does.
        None => {
            let mut body = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        body.extend_from_slice(&chunk[..read]);
                        if body.len() > MAX_BODY {
                            return Err("oversized answer");
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return Err("timeout"),
                }
            }
            Ok(body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn an_address_url_is_its_own_identity() {
        // The certificate for `1.1.1.1` carries the address, so no name is
        // invented and the check is done against the address itself.
        let endpoint = DohEndpoint::parse("https://1.1.1.1/dns-query", None).expect("parses");
        assert_eq!(endpoint.addr, "1.1.1.1:443".parse().expect("valid"));
        assert_eq!(endpoint.host, "");
        assert_eq!(endpoint.path, "/dns-query");
    }

    #[test]
    fn a_named_url_needs_a_bootstrap_address() {
        // The circle this breaks: `dns.alidns.com` cannot be resolved in order to
        // be connected to, because resolving is what it is being configured for.
        assert!(DohEndpoint::parse("https://dns.alidns.com/dns-query", None).is_none());

        let endpoint =
            DohEndpoint::parse("https://dns.alidns.com/dns-query", Some(ip(223, 5, 5, 5)))
                .expect("parses");
        assert_eq!(endpoint.addr, "223.5.5.5:443".parse().expect("valid"));
        assert_eq!(endpoint.host, "dns.alidns.com");
    }

    #[test]
    fn a_name_is_lowercased_and_a_port_is_honoured() {
        let endpoint =
            DohEndpoint::parse("https://DoH.Example.COM:8443/q", Some(ip(203, 0, 113, 9)))
                .expect("parses");
        assert_eq!(endpoint.host, "doh.example.com");
        assert_eq!(endpoint.addr.port(), 8443);
        assert_eq!(endpoint.host_header(), "doh.example.com:8443");
    }

    #[test]
    fn a_url_without_a_path_gets_the_well_known_one() {
        let endpoint = DohEndpoint::parse("https://1.1.1.1", None).expect("parses");
        assert_eq!(endpoint.path, DEFAULT_PATH);
    }

    #[test]
    fn an_ipv6_literal_is_bracketed_not_split_at_its_colons() {
        // Without the bracket branch the port would be read out of the middle of
        // the address, and `2606:4700:4700::1111` would parse as a nonsense port.
        let endpoint = DohEndpoint::parse("https://[2606:4700:4700::1111]/dns-query", None)
            .expect("parses");
        assert_eq!(endpoint.addr, "[2606:4700:4700::1111]:443".parse().expect("valid"));
        assert_eq!(endpoint.host, "");
    }

    #[test]
    fn anything_that_is_not_https_is_refused_rather_than_guessed() {
        // A plaintext endpoint is reachable by exactly the interception this
        // exists to avoid, so accepting one would ship a control that cannot do
        // its job.
        assert!(DohEndpoint::parse("http://1.1.1.1/dns-query", None).is_none());
        assert!(DohEndpoint::parse("1.1.1.1/dns-query", None).is_none());
        assert!(DohEndpoint::parse("", None).is_none());
        assert!(DohEndpoint::parse("https://", None).is_none());
        assert!(DohEndpoint::parse("https://1.1.1.1:notaport/dns-query", None).is_none());
    }

    #[test]
    fn the_description_names_the_endpoint_the_way_it_was_configured() {
        let literal = DohEndpoint::parse("https://1.1.1.1/dns-query", None).expect("parses");
        assert_eq!(literal.describe(), "https://1.1.1.1:443/dns-query");

        let named =
            DohEndpoint::parse("https://dns.alidns.com/dns-query", Some(ip(223, 5, 5, 5)))
                .expect("parses");
        assert_eq!(
            named.describe(),
            "https://dns.alidns.com/dns-query via 223.5.5.5:443"
        );
    }

    #[test]
    fn the_backoff_doubles_and_stays_bounded() {
        assert_eq!(backoff_for(1), BACKOFF * 2);
        assert_eq!(backoff_for(2), BACKOFF * 4);
        // The exponent is capped, so an attempt number far past `ATTEMPTS`
        // cannot turn the pause into an overflow or an hour-long sleep.
        assert_eq!(backoff_for(9), BACKOFF * 16);
    }

    #[test]
    fn an_endpoint_that_keeps_failing_is_tried_once_per_resolution() {
        let health = Health::default();
        assert_eq!(health.attempts(), ATTEMPTS);
        assert_eq!(health.record_failure(), HealthChange::FirstFailure);
        assert_eq!(health.attempts(), ATTEMPTS, "one lost request is not an outage");
        assert_eq!(health.record_failure(), HealthChange::None);
        assert_eq!(health.record_failure(), HealthChange::EnteredCooldown);
        assert_eq!(health.attempts(), 1);
        assert!(health.remaining().is_some());

        // One answer puts it straight back to full retries. Being unhealthy is
        // not a latch: the endpoint is never disabled, only slowed down.
        assert_eq!(health.record_success(), HealthChange::Recovered);
        assert_eq!(health.attempts(), ATTEMPTS);
        assert_eq!(health.remaining(), None);
    }

    #[test]
    fn the_cooldown_widens_without_overflowing() {
        let health = Health::default();
        for _ in 0..FAILURES_BEFORE_COOLDOWN {
            health.record_failure();
        }
        let first = health.remaining().expect("cooling down");

        for _ in 0..2 {
            health.record_failure();
        }
        let later = health.remaining().expect("still cooling down");
        assert!(later > first, "each further failure widens the pause");

        for _ in 0..20 {
            health.record_failure();
        }
        let capped = health.remaining().expect("still cooling down");
        assert!(capped <= MAX_COOLDOWN);
    }

    #[test]
    fn a_question_already_in_flight_is_not_asked_twice() {
        // A client that repeats a query while the first is unresolved must not
        // start a second resolution: the retry is the resolver's job, and a
        // duplicate would double the load for one answer.
        //
        // The endpoint is a closed port on the loopback, so the attempt fails
        // immediately and the test does not depend on the network.
        let endpoint =
            DohEndpoint::parse("https://127.0.0.1:1/dns-query", None).expect("parses");
        let resolver = DohResolver::new(endpoint);
        let waiter = Waiter {
            client: "198.51.100.7:5353".parse().expect("valid"),
            from: ip(198, 18, 0, 1),
            id: 0x1234,
        };
        let query = crate::dns::Message {
            id: 0x1234,
            flags: crate::dns::FLAG_RD,
            questions: vec![crate::dns::Question {
                name: "example.com".to_string(),
                qtype: crate::dns::TYPE_A,
                qclass: crate::dns::CLASS_IN,
            }],
            answers: Vec::new(),
            authorities: Vec::new(),
            additionals: Vec::new(),
        }
        .encode()
        .expect("encodes");

        assert!(resolver.resolve(waiter, &query), "the first request starts");
        assert!(
            !resolver.resolve(waiter, &query),
            "the same question is already in flight"
        );

        // The attempt is against a closed port, so it fails; what matters is
        // that a result arrives at all rather than the answer's content.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut answers = Vec::new();
        while Instant::now() < deadline && answers.is_empty() {
            answers = resolver.take_ready();
            if answers.is_empty() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert_eq!(answers.len(), 1, "one answer for one question");
        assert_eq!(answers[0].waiter, waiter);
        assert!(answers[0].outcome.is_err(), "a closed port cannot answer");

        let counters = resolver.take_counters();
        assert_eq!(counters.queries, 1);
        assert_eq!(counters.failed, 1);
        assert_eq!(counters.answered, 0);
    }
}

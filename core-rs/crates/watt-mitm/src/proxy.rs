//! The local reverse proxy: terminate TLS, then re-originate it.
//!
//! # What it is for
//!
//! A rule can name an address that is reachable but serves a certificate for a
//! *different* name. Speed's ordinary answer is to keep looking for an address
//! whose certificate covers the name (that is what the tunnel's pre-check does),
//! and when none exists the connection simply fails — the client rejects the
//! certificate, and the kernel never sees it because in TLS 1.3 that check
//! happens inside the encrypted handshake.
//!
//! This proxy removes that constraint by taking the client's TLS connection
//! itself. The client is answered with a certificate for the name it asked for,
//! signed by the authority the device has been told to trust, and the request is
//! then carried to the rule's address over a second TLS connection that is free
//! to ignore what that address presents. The name the client asked for and the
//! certificate the origin serves stop being the same question.
//!
//! # How traffic reaches it
//!
//! Not by being pointed at a proxy setting — a client that ignores its proxy
//! setting, or speaks QUIC, would bypass that. Root mode rewrites the name to a
//! dedicated loopback address in the system hosts file, and the root helper
//! installs a NAT rule redirecting that address's port 443 to this listener.
//! Anything that resolves the name and connects to 443 is therefore served,
//! whether or not it knows a proxy exists.
//!
//! # Why the loopback address is not 127.0.0.1
//!
//! `127.0.0.1:443` is a port real local services use. Redirecting it would
//! capture them too, and there is no way to tell "this connection was meant for
//! the name we rewrote" from "this connection was meant for whatever else is
//! listening on 443". A separate address keeps the rewrite's blast radius to the
//! names the rewrite actually covers. This is also why the caller is handed the
//! address to write into hosts rather than being left to pick one.
//!
//! # Why an unmatched name is refused rather than dialled
//!
//! The hosts rewrite is system-wide, so once a name points at the loopback
//! address, *every* resolver on the device returns the loopback address for it —
//! including this process. Resolving an unmatched name here would therefore
//! dial the listener itself, and the connection would recurse until something
//! ran out of file descriptors. A name this proxy cannot find a rule address for
//! is closed, and that is the only safe answer.
//!
//! # Which name the origin is asked for
//!
//! The client's, normally. The certificate this proxy mints is for the name the
//! client asked for, and the request relayed inside carries that same name in
//! `Host` — both are the client's and neither is touched.
//!
//! Not always, though. A rule may name a *host* instead of an address, and then
//! that host is the name the origin answers to: the address it resolved to is an
//! alias for it, and the origin's certificate and virtual hosts are built around
//! the host, not the alias. Measured on the device: reaching a Steam Community
//! address with the client's name in SNI fails the handshake (`rc=35`), while the
//! rule's name completes it and returns 200 with the page body. So when the
//! address actually dialled came from a rule name, that name is what goes in SNI.
//!
//! And sometimes the origin is asked for nothing. A rule may have an address and
//! no name for it at all — its source reaches that block over a bare upstream
//! address with no server name set — and writing the address as `!<address>` says
//! so. Measured over 13 such addresses: the client's name in SNI is refused by 11
//! of them (RST 0.06–0.4 s in, so the address itself is live), while the same
//! addresses with no SNI complete the handshake on 10. A rule that flags an
//! address the client never asked about therefore reaches further by saying less.
//!
//! These three are the states a rule can be in, and they are not interchangeable:
//! the client's name is what a real-address rule wants, a substituted host is what
//! a re-routed rule wants, and nothing at all is what an address-only rule wants.
//!
//! The split is the one the rule source itself describes: `header_up Host {host}`
//! with `tls_server_name <upstream>`. Only the second half needs doing here, and
//! only because this proxy terminates TLS — a caller that relays ciphertext
//! cannot change SNI and does not have to.
//!
//! # What it deliberately does not do
//!
//! It does not verify the origin's certificate. It cannot: the entire point is
//! to reach an address whose certificate does not cover the name, so a check
//! would reject exactly the connections this exists to carry. The consequence is
//! real and worth stating plainly — inside this proxy's traffic, the origin is
//! authenticated by nothing but the rule table's say-so. Root mode is a mode
//! that trades that assurance for reachability, and it is opt-in for that
//! reason.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::ResolvesServerCert;
use rustls::sign::CertifiedKey;
use rustls::{ClientConnection, ServerConnection};
use watt_rules::{Family, Router, RuleSet, Strategy};

use crate::ca::Authority;

/// How long a side waits for its peer before looking at its own socket again.
///
/// This is the only latency the relay adds, and only when a direction is idle:
/// a write from the peer wakes the wait immediately. Long enough not to spin a
/// core per connection, short enough to be invisible next to a round trip.
const IDLE: Duration = Duration::from_millis(20);

/// How long a client has to complete its handshake.
///
/// A client that connects and then says nothing is holding a thread; the same
/// reason `watt-proxy` bounds its header read.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to spend on each candidate address before trying the next.
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);

/// Read buffer for one plaintext chunk.
const CHUNK: usize = 16 * 1024;

/// How often the rules' dial names are re-resolved.
///
/// The same value the no-root proxy uses, and for the same reason: a name is
/// re-resolved on a tick rather than per connection, because the answer changes
/// on the order of minutes and a lookup on the connection path would block a
/// thread that is holding a client mid-handshake.
const DIAL_REFRESH: Duration = Duration::from_secs(60);

/// What the proxy has done, shared with every connection thread.
///
/// Atomics rather than the plain struct `watt-stack` uses for its own counters,
/// and the difference is not a preference: that struct is owned by a single step
/// loop, while this proxy hands every connection its own thread, so each field
/// here is written concurrently by construction. `Relaxed` throughout — no
/// counter is read to make a decision, and no reader needs two of them to agree.
#[derive(Debug, Default)]
pub struct Stats {
    pub(crate) connections: AtomicU64,
    pub(crate) handshakes: AtomicU64,
    pub(crate) served: AtomicU64,
    pub(crate) refused: AtomicU64,
    pub(crate) dial_failures: AtomicU64,
    pub(crate) named_upstreams: AtomicU64,
    pub(crate) no_sni_upstreams: AtomicU64,
    pub(crate) bytes_to_upstream: AtomicU64,
    pub(crate) bytes_to_client: AtomicU64,
}

impl Stats {
    /// Every counter, read once.
    ///
    /// One pass rather than handing out a reference: the counters move under the
    /// reader, and a caller that read them one at a time would be comparing
    /// values from different moments as if they were one.
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            connections: self.connections.load(Ordering::Relaxed),
            handshakes: self.handshakes.load(Ordering::Relaxed),
            served: self.served.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            dial_failures: self.dial_failures.load(Ordering::Relaxed),
            named_upstreams: self.named_upstreams.load(Ordering::Relaxed),
            no_sni_upstreams: self.no_sni_upstreams.load(Ordering::Relaxed),
            bytes_to_upstream: self.bytes_to_upstream.load(Ordering::Relaxed),
            bytes_to_client: self.bytes_to_client.load(Ordering::Relaxed),
        }
    }

    /// Add `bytes` to one direction's total.
    fn record_bytes(&self, direction: Direction, bytes: u64) {
        let counter = match direction {
            Direction::ToUpstream => &self.bytes_to_upstream,
            Direction::ToClient => &self.bytes_to_client,
        };
        counter.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// A [`Stats`] read at one moment.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// Client TCP connections accepted.
    pub connections: u64,
    /// Connections whose client-side TLS handshake completed.
    ///
    /// The counter that proves a certificate was actually presented: a
    /// connection that fails here never reached a name, a rule or an origin.
    /// Read against `connections` — the gap is clients that connected and then
    /// could not be spoken to, which is what a client that does not trust the
    /// installed authority looks like.
    pub handshakes: u64,
    /// Connections that became fully usable: routed, dialled, and handshaken
    /// with the origin.
    ///
    /// Counted when the relay starts rather than when it ends, so a connection
    /// cut short by a stop or a hung-up client still counts — it was served.
    pub served: u64,
    /// Connections closed for having no address to dial.
    ///
    /// Two paths land here and they mean the same thing to the client. No rule
    /// owns the name (`Strategy::Direct`), or the rule matched but carries no
    /// usable address yet — the latter is the dial-name refresh not having run,
    /// and the module docs explain why it is refused rather than resolved:
    /// resolving it here would dial this very listener.
    pub refused: u64,
    /// Connections where every address the rule lists refused to connect.
    ///
    /// Separate from `refused` because the cause is different and so is the fix:
    /// `refused` is a rule-table question, this is a network one.
    pub dial_failures: u64,
    /// Connections whose origin handshake presented a rule-supplied name in SNI
    /// instead of the name the client asked for.
    ///
    /// This is the counter that says the name switch happened at all. On a rule
    /// set built entirely from names — the shape a hosts file has when its first
    /// field is a host rather than an address — it should track `served`, and a
    /// zero here means every origin is still being asked for the client's name,
    /// which is the case measured to fail the handshake.
    pub named_upstreams: u64,
    /// Connections whose origin handshake sent *no* SNI at all.
    ///
    /// The third state, and the reason the two counters are separate: `named`
    /// says a name was swapped, this says a name was dropped. Both are "the
    /// origin was not asked for the client's name", but only one of them can be
    /// undone by editing the rule's upstream, so a single number could not say
    /// which had happened.
    ///
    /// Set by a rule that wrote its address as `!<address>`: an address is all
    /// the source has, and asking such an origin for the client's name is
    /// measured to be refused. A zero here on a rule set that carries flagged
    /// addresses means the flag never reached the plan.
    pub no_sni_upstreams: u64,
    /// Plaintext bytes carried from the client towards the origin.
    pub bytes_to_upstream: u64,
    /// Plaintext bytes carried from the origin back to the client.
    ///
    /// Plaintext, not wire bytes: this is what crossed between the two TLS
    /// endpoints, so it is the size of the traffic actually carried and not of
    /// the two encrypted streams around it.
    pub bytes_to_client: u64,
}

/// Which of the two byte counters a relay direction feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Client plaintext on its way to the origin.
    ToUpstream,
    /// Origin plaintext on its way to the client.
    ToClient,
}

/// A running listener.
pub struct Proxy {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    addr: SocketAddr,
    router: Arc<Mutex<Router>>,
    stats: Arc<Stats>,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy").field("addr", &self.addr).finish()
    }
}

impl Proxy {
    /// The address the listener actually bound.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Every counter, read once.
    ///
    /// A snapshot rather than a borrow, for the reason [`Stats::snapshot`] gives:
    /// these are written by connection threads, so values read one at a time are
    /// from different moments.
    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    /// Swap the rule set of the running proxy.
    ///
    /// Same promise the tunnel and the no-root proxy make: a rule change reaches
    /// the routing decisions not yet taken, and never cuts a connection that is
    /// already being served.
    pub fn replace_rules(&self, rules: RuleSet) -> bool {
        match self.router.lock() {
            Ok(mut router) => {
                router.replace_rules(rules);
                true
            }
            Err(_) => false,
        }
    }

    /// Stop the listener and wait for the accept loop to notice.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Bind `bind`, serve `rules`, sign with `ca`.
pub fn start(bind: SocketAddr, rules: RuleSet, ca: Arc<Authority>) -> io::Result<Proxy> {
    let listener = TcpListener::bind(bind)?;
    let addr = listener.local_addr()?;
    let router = Arc::new(Mutex::new(Router::new(rules)));
    let stats = Arc::new(Stats::default());
    let stop = Arc::new(AtomicBool::new(false));

    let worker_router = Arc::clone(&router);
    let worker_stats = Arc::clone(&stats);
    let worker_stop = Arc::clone(&stop);
    let worker = thread::Builder::new()
        .name("watt-mitm".to_string())
        .spawn(move || {
            if let Err(err) = serve_until(listener, worker_router, ca, worker_stats, worker_stop) {
                log::error!("mitm: the listener stopped: {err}");
            }
        })?;

    Ok(Proxy {
        stop,
        worker: Some(worker),
        addr,
        router,
        stats,
    })
}

/// Accept loop. Split out from [`start`] so a test can drive it with its own
/// listener and shut it down without a handle.
///
/// The counters are passed down rather than kept here: the work being counted
/// happens on the connection threads, so this loop only owns the one count it
/// itself performs — a connection accepted.
pub fn serve_until(
    listener: TcpListener,
    router: Arc<Mutex<Router>>,
    ca: Arc<Authority>,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    let server = Arc::new(server_config(ca)?);
    let client = Arc::new(client_config());

    // A rule that names a host instead of an address is only dialable once this
    // has run: `plan.addresses` is filled from the last tick's results, and a
    // name that has never been resolved leaves a plan with no address, which
    // `serve_one` then refuses. Started before the accept loop so the first
    // connections are not all refused while the first tick is still out.
    spawn_dial_refresh(Arc::clone(&router), Arc::clone(&stop));

    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                stats.connections.fetch_add(1, Ordering::Relaxed);
                let server = Arc::clone(&server);
                let client = Arc::clone(&client);
                let router = Arc::clone(&router);
                let stats = Arc::clone(&stats);
                let stop = Arc::clone(&stop);
                // One thread per connection, matching what the no-root proxy
                // does: a connection here is two handshakes and a relay, and
                // serialising them would make a page load as slow as its
                // slowest subresource.
                let spawned = thread::Builder::new()
                    .name("watt-mitm-conn".to_string())
                    .spawn(move || {
                        if let Err(err) = serve_one(stream, peer, server, client, router, stats, stop) {
                            // Not an error worth surfacing: a client that hangs
                            // up mid-handshake, a name with no rule, and a
                            // browser cancelling a request all land here, and
                            // all of them are ordinary.
                            log::debug!("mitm: connection from {peer} ended: {err}");
                        }
                    });
                if spawned.is_err() {
                    log::warn!("mitm: could not spawn a connection thread");
                }
            }
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Re-resolve the rules' dial names on a tick, in the background.
///
/// A rule may name a host instead of an address; `Router::plan` reports such a
/// rule as matched but leaves `addresses` empty until `resolve_dial_names` has
/// filled them from a lookup. Without this thread those rules would be treated
/// as "no usable address" and refused — see `serve_one`.
///
/// The lookup is deliberately not on the connection path: it is blocking I/O,
/// and doing it while a client waits mid-handshake would stall that thread for
/// the length of a DNS timeout. A failure to spawn is reported and otherwise
/// ignored — every rule that lists literal addresses still works without it,
/// and refusing to serve at all would be the worse trade.
fn spawn_dial_refresh(router: Arc<Mutex<Router>>, stop: Arc<AtomicBool>) {
    let spawned = thread::Builder::new()
        .name("watt-mitm-dial".to_string())
        .spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let resolved = {
                    let mut guard = match router.lock() {
                        Ok(guard) => guard,
                        Err(_) => return,
                    };
                    guard.resolve_dial_names(resolve_dropping_loopback)
                };
                if resolved > 0 {
                    log::info!("mitm: re-resolved dial names for {resolved} entries");
                }
                // Slept in short steps rather than one long one, so a stop
                // request does not have to wait out a whole refresh interval.
                let mut waited = Duration::ZERO;
                while waited < DIAL_REFRESH && !stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(200));
                    waited += Duration::from_millis(200);
                }
            }
        });
    if let Err(err) = spawned {
        log::warn!("mitm: dial-name refresh is not running: {err}");
    }
}

/// Resolve one rule dial name, discarding loopback answers.
///
/// # Why loopback is dropped, and why this is not a micro-optimisation
///
/// Root mode rewrites the intercepted names to a loopback address in the system
/// hosts file, so *every* resolver on the device — `getaddrinfo` included —
/// answers those names with that address. If a rule's dial name is itself an
/// intercepted name, the lookup returns loopback, the proxy dials it, and the
/// connection lands back on this very listener: recursion until the process runs
/// out of file descriptors. Dropping loopback here is what stops a rule from
/// being able to point the proxy at itself. It is the same failure `serve_one`
/// avoids by refusing a name with no rule address instead of falling back to the
/// system resolver.
fn resolve_dropping_loopback(name: &str) -> Vec<IpAddr> {
    (name, 0u16)
        .to_socket_addrs()
        .map(|addrs| {
            addrs
                .map(|addr| addr.ip())
                .filter(|ip| !ip.is_loopback())
                .collect()
        })
        .unwrap_or_default()
}

/// Serve one accepted connection.
fn serve_one(
    mut client: TcpStream,
    peer: SocketAddr,
    server: Arc<rustls::ServerConfig>,
    client_config: Arc<rustls::ClientConfig>,
    router: Arc<Mutex<Router>>,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    client.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    client.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;

    let mut server_conn = ServerConnection::new(server)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    handshake(&mut server_conn, &mut client)?;
    stats.handshakes.fetch_add(1, Ordering::Relaxed);

    // The name the client asked for. Read after the handshake because rustls
    // only exposes it once the ClientHello has been processed, and it is the
    // resolver's answer to *this* connection rather than a second parse of the
    // same bytes.
    let name = server_conn
        .server_name()
        .map(|name| name.to_string())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the client sent no SNI, so there is no name to route on",
            )
        })?;

    let (addresses, port, named, no_sni) = {
        let router = router
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "the rule table is poisoned"))?;
        let plan = router.plan(Instant::now(), &name, Family::V4);
        match plan.strategy {
            // Only a rule address is usable. See the module docs: a fallback to
            // the system resolver here would dial this very listener, because
            // the hosts rewrite has already made the name point at it.
            Strategy::RuleAddresses | Strategy::PlaceholderFallback | Strategy::FamilyFallback => {
                (plan.addresses, plan.port, plan.dial_addresses, plan.no_sni)
            }
            Strategy::Direct => {
                stats.refused.fetch_add(1, Ordering::Relaxed);
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no rule owns {name}, so there is no address to dial"),
                ))
            }
        }
    };
    if addresses.is_empty() {
        stats.refused.fetch_add(1, Ordering::Relaxed);
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("the rule for {name} carries no usable address"),
        ));
    }

    let port = port.unwrap_or(443);
    let (upstream, connected) = match dial(&addresses, port) {
        Some(found) => found,
        None => {
            stats.dial_failures.fetch_add(1, Ordering::Relaxed);
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                format!("none of the {} addresses for {name} accepted a connection", addresses.len()),
            ));
        }
    };

    // Which name the origin is asked for. See the module docs; in short, the
    // client's name is only the right answer when nothing better is known.
    //
    // Three answers, in the order of who is entitled to give one:
    //
    //   * the rule flagged this address `!<address>` — it has an address and no
    //     name for it, so the origin is asked for nothing;
    //   * the address came from a host the rule wrote down — present that host,
    //     because that is the name the origin answers to;
    //   * otherwise the client's, which is the only name anyone has.
    //
    // Matched against the address that actually accepted the connection rather
    // than against the first candidate: a rule may list several, and the one that
    // answered is the one whose name applies.
    //
    // `None` is not a missing value — it is the answer. `ServerName::try_from`
    // turns an address into `ServerName::IpAddress`, and rustls emits the SNI
    // extension only for a `DnsName` (RFC 6066 permits a host name and nothing
    // else), so handing it the address is how "nothing" is expressed. Verified in
    // rustls 0.23.45 `client/hs.rs`: the `(None, true)` arm matches `DnsName` and
    // falls through to `None` for every other variant.
    let upstream_name: Option<String> = if no_sni.contains(&connected) {
        None
    } else {
        Some(
            named
                .iter()
                .find(|(_, addr)| *addr == connected)
                .map(|(host, _)| host.clone())
                .unwrap_or_else(|| name.clone()),
        )
    };

    let server_name = ServerName::try_from(
        upstream_name
            .clone()
            .unwrap_or_else(|| connected.to_string()),
    )
    .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
    let mut client_conn = ClientConnection::new(client_config, server_name)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;

    let mut upstream = upstream;
    upstream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    upstream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    handshake(&mut client_conn, &mut upstream)?;

    // Mutually exclusive, not two independent counts: an unnamed dial has no name
    // that could differ from the client's, and counting it as both would make
    // `named_upstreams` mean "the client's name was not used" rather than "a name
    // was substituted".
    if upstream_name.is_none() {
        stats.no_sni_upstreams.fetch_add(1, Ordering::Relaxed);
    } else if upstream_name.as_deref() != Some(name.as_str()) {
        stats.named_upstreams.fetch_add(1, Ordering::Relaxed);
    }

    log::debug!(
        "mitm: {peer} asked for {name}, served from {connected}:{port} as {} \
         ({} candidate(s))",
        upstream_name.as_deref().unwrap_or("<no SNI>"),
        addresses.len()
    );

    stats.served.fetch_add(1, Ordering::Relaxed);
    relay(client, server_conn, upstream, client_conn, stats, stop)
}

/// Pick the first candidate that accepts a connection, and say which one it was.
///
/// Sequential rather than raced, and short per candidate: this is the same
/// trade the no-root proxy makes, and for the same reason — a rule lists its
/// addresses in preference order, and paying the full timeout on a dead first
/// entry would make a working rule look broken.
///
/// The address comes back with the stream rather than being read off the socket
/// afterwards. The caller needs it to know which of the rule's names to present,
/// and asking the OS what it connected to would be a second source of truth for
/// something already known — and a wrong one, if anything ever rewrote the
/// destination in between.
fn dial(addresses: &[std::net::IpAddr], port: u16) -> Option<(TcpStream, std::net::IpAddr)> {
    for address in addresses {
        let target = SocketAddr::new(*address, port);
        match TcpStream::connect_timeout(&target, DIAL_TIMEOUT) {
            Ok(stream) => return Some((stream, *address)),
            Err(err) => log::debug!("mitm: {target} refused: {err}"),
        }
    }
    None
}

/// Drive a handshake to completion on a blocking socket.
///
/// Written in terms of `read_tls`/`write_tls` rather than `complete_io` so the
/// same code works for both sides through [`TlsSide`], which `complete_io`'s
/// generic parameter would not allow.
fn handshake<S: TlsSide>(conn: &mut S, sock: &mut TcpStream) -> io::Result<()> {
    while conn.is_handshaking() {
        if conn.wants_write() {
            conn.write_tls(sock)?;
        }
        if conn.wants_read() {
            match conn.read_tls(sock) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the peer closed during the handshake",
                    ))
                }
                Ok(_) => {}
                Err(err) => return Err(err),
            }
            conn.process_new_packets()
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        }
    }
    while conn.wants_write() {
        conn.write_tls(sock)?;
    }
    Ok(())
}

/// Carry bytes between the two TLS endpoints until either side is done.
///
/// Two threads, one per direction, each owning exactly one TLS connection and
/// one socket. The alternative — a single thread driving both state machines —
/// needs both sockets in one `poll`, and the split buys the same result with
/// each thread only ever touching its own file descriptor. Plaintext crosses
/// between them on a channel.
fn relay(
    client: TcpStream,
    server_conn: ServerConnection,
    upstream: TcpStream,
    client_conn: ClientConnection,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    let (to_upstream_tx, to_upstream_rx) = mpsc::channel::<Vec<u8>>();
    let (to_client_tx, to_client_rx) = mpsc::channel::<Vec<u8>>();

    let stop_a = Arc::clone(&stop);
    let stop_b = Arc::clone(&stop);
    let stats_a = Arc::clone(&stats);
    let stats_b = Arc::clone(&stats);
    let client_sock = client.try_clone()?;

    // Client -> upstream, driven by the server-side connection.
    let up = thread::Builder::new()
        .name("watt-mitm-up".to_string())
        .spawn(move || {
            pump(
                client,
                Box::new(server_conn),
                to_upstream_tx,
                to_client_rx,
                stats_a,
                Direction::ToUpstream,
                stop_a,
            )
        })?;
    // Upstream -> client, driven by the client-side connection.
    let down = thread::Builder::new()
        .name("watt-mitm-down".to_string())
        .spawn(move || {
            pump(
                upstream,
                Box::new(client_conn),
                to_client_tx,
                to_upstream_rx,
                stats_b,
                Direction::ToClient,
                stop_b,
            )
        })?;

    // Both pumps are joined, and their results are reported rather than
    // discarded. A panicking pump used to vanish here — `let _ = join()` swallows
    // it — which would leave a connection that looks like a stall and a log with
    // nothing in it.
    let up_result = up.join();
    let down_result = down.join();
    log::info!("mitm-relay: up={up_result:?} down={down_result:?}");
    let _ = client_sock.shutdown(Shutdown::Both);
    Ok(())
}

/// One direction of the relay: read plaintext out of `conn`, hand it to the
/// peer on `outbound`, and write whatever the peer sends back into `conn`.
fn pump(
    mut sock: TcpStream,
    mut conn: Box<dyn TlsSide>,
    outbound: mpsc::Sender<Vec<u8>>,
    inbound: mpsc::Receiver<Vec<u8>>,
    stats: Arc<Stats>,
    direction: Direction,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    sock.set_nonblocking(true)?;
    let mut buf = vec![0u8; CHUNK];
    // Plaintext the sink would not take yet.
    //
    // `write_plain` is a **partial** write: it hands bytes to rustls's send
    // buffer and returns how many it accepted, bounded by that buffer's limit
    // (`PlaintextSink::write` → `buffer_plaintext`). Dropping the return value —
    // what this did before — loses the tail of a chunk silently, and a truncated
    // stream is indistinguishable from a stall at the other end: the peer waits
    // for bytes that were never sent. The remainder is therefore carried here
    // and retried, and the flush below is what makes room for it.
    let mut pending: Vec<u8> = Vec::new();
    // The peer has said it is finished. The pump must not return before
    // `pending` is empty, or the last chunk is dropped on the way out — the same
    // truncation by another route.
    let mut peer_done = false;
    // A pass counter, logged periodically. A pump that is silently looping and a
    // pump that is stuck somewhere look identical in a log that only records
    // reads and writes — both are silence. This is what tells them apart.
    let mut passes: u64 = 0;
    log::info!("mitm-pump {direction:?}: start");

    loop {
        passes += 1;
        if passes % 50 == 0 {
            log::info!("mitm-pump {direction:?}: alive, {passes} passes");
        }
        if stop.load(Ordering::SeqCst) {
            log::info!("mitm-pump {direction:?}: stop flag, exiting");
            return Ok(());
        }

        // Everything the peer produced goes into this side's TLS connection.
        loop {
            match inbound.try_recv() {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        peer_done = true;
                    } else {
                        pending.extend_from_slice(&chunk);
                    }
                }
                Err(TryRecvError::Empty) => break,
                // The peer is gone; nothing more will arrive for this socket.
                Err(TryRecvError::Disconnected) => {
                    peer_done = true;
                    break;
                }
            }
        }

        // Hand over as much as the sink will take. A zero-length write means the
        // send buffer is full; the flush just below is what drains it.
        while !pending.is_empty() {
            let offered = pending.len();
            let accepted = conn.write_plain(&pending)?;
            if accepted == 0 {
                log::info!("mitm-pump {direction:?}: sink full with {offered} bytes pending");
                break;
            }
            if accepted < offered {
                log::info!("mitm-pump {direction:?}: sink took {accepted} of {offered}");
            }
            pending.drain(..accepted);
            log::info!("mitm-pump {direction:?}: wrote {accepted}, {} still pending", pending.len());
        }
        let t_flush = Instant::now();
        flush(&mut *conn, &mut sock)?;
        let flush_ms = t_flush.elapsed().as_millis();
        if flush_ms > 50 {
            log::info!("mitm-pump {direction:?}: flush blocked {flush_ms}ms");
        }
        if conn.wants_write() {
            // `flush` gives up on `WouldBlock`, so records can be left behind.
            // Nothing retries them until another chunk arrives, which for a
            // response that has already been fully read never happens.
            log::info!("mitm-pump {direction:?}: flush left records unwritten");
        }

        if peer_done && pending.is_empty() {
            // Nothing more will arrive from the peer, so this is the last chance
            // to get what has already been written out of the connection. A bare
            // `return` here is what truncated a large response.
            finish(&mut *conn, &mut sock, &mut pending, &inbound)?;
            log::info!("mitm-pump {direction:?}: peer done and nothing pending, exiting");
            return Ok(());
        }

        // Pull whatever TLS records are waiting, **then** drain whatever
        // plaintext that produced — including plaintext that was already there
        // before this pass started.
        //
        // Two properties, and both are load-bearing:
        //
        //   * The drain is **not** inside the record loop. `handshake` reads the
        //     socket to completion (`read_tls` + `process_new_packets` while
        //     `is_handshaking`), so a client that writes its request in the same
        //     segment as its Finished has that request decrypted and sitting in
        //     the connection's plaintext buffer by the time this pump starts.
        //     Draining only after a *fresh* record never looks at it: the request
        //     is forwarded nowhere while the client waits out its whole timeout.
        //   * The record loop **ends when the socket is dry**. An earlier attempt
        //     at the first property hoisted the drain but left the loop with a
        //     "keep going while a record was ever seen" flag. That flag never
        //     goes back to false, so the loop spun inside this pass forever, the
        //     pump never returned to hand the peer's bytes to the client, and the
        //     visible failure was the same stall — reached from the opposite
        //     mistake.
        let mut got_something = false;
        // The socket reaching EOF is not the end of the *records*: whatever the
        // last read put into the connection has not been decrypted or drained
        // yet. Returning there — which this once did — drops that tail, and a
        // truncated response is what the peer sees. It is why the same URL came
        // back with 403 and a body of a different length every time.
        let mut socket_eof = false;
        let mut peer_closed = false;
        let mut orphaned = false;

        loop {
            let t_read_tls = Instant::now();
            let read_result = conn.read_tls(&mut sock);
            let read_ms = t_read_tls.elapsed().as_millis();
            if read_ms > 50 {
                log::info!("mitm-pump {direction:?}: read_tls blocked {read_ms}ms");
            }
            match read_result {
                Ok(0) => {
                    log::info!("mitm-pump {direction:?}: socket EOF, draining what is left");
                    socket_eof = true;
                    break;
                }
                Ok(n) => {
                    got_something = true;
                    log::info!("mitm-pump {direction:?}: read_tls {n}");
                    conn.process_new_packets()
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
                    // Drain **here**, inside the record loop, rather than after
                    // the socket has gone dry. `drain_plain` carries the
                    // measurement; the short version is that rustls's plaintext
                    // buffer is 16 KiB and the next `read_tls` refuses to run
                    // once it is full — `Err("received plaintext buffer full")`
                    // — which killed this pump and the connection with it.
                    match drain_plain(&mut *conn, &mut buf, &outbound, &stats, direction)? {
                        Drained::Open => {}
                        Drained::PeerClosed => {
                            peer_closed = true;
                            break;
                        }
                        Drained::Orphaned => {
                            orphaned = true;
                            break;
                        }
                    }
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => return Err(err),
            }
        }

        // One more drain, for plaintext that was already buffered when this pass
        // began. `handshake` reads the socket to completion, so a client that
        // writes its request in the same segment as its Finished has that request
        // decrypted and waiting here before the loop above ever runs. A drain that
        // only follows a *fresh* record never looks at it: the request is
        // forwarded nowhere while the client waits out its whole timeout.
        if !peer_closed && !orphaned {
            match drain_plain(&mut *conn, &mut buf, &outbound, &stats, direction)? {
                Drained::Open => {}
                Drained::PeerClosed => peer_closed = true,
                Drained::Orphaned => orphaned = true,
            }
        }

        if orphaned {
            log::info!("mitm-pump {direction:?}: peer pump gone, exiting");
            return Ok(());
        }

        if peer_closed {
            // Clean close from this side; tell the peer so it can stop waiting.
            // Not counted: the empty marker carries no bytes, only the close.
            finish(&mut *conn, &mut sock, &mut pending, &inbound)?;
            log::info!("mitm-pump {direction:?}: TLS closed cleanly, exiting");
            let _ = outbound.send(Vec::new());
            return Ok(());
        }

        if socket_eof {
            // The socket is gone and its last records have been drained above.
            finish(&mut *conn, &mut sock, &mut pending, &inbound)?;
            log::info!("mitm-pump {direction:?}: socket EOF and drained, exiting");
            let _ = outbound.send(Vec::new());
            return Ok(());
        }

        if !got_something {
            // Idle on both sides. Wait for the peer's plaintext rather than
            // spinning; a write from the peer returns immediately, so this is
            // a bound on how long a direction can sit still, not added latency.
            //
            // What arrives is queued rather than written here, so there is one
            // place that writes and one place that flushes — a second write site
            // is a second place for a partial write to be dropped.
            match inbound.recv_timeout(IDLE) {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        peer_done = true;
                    } else {
                        pending.extend_from_slice(&chunk);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => peer_done = true,
            }
        }
    }
}

/// Write out every pending TLS record, stopping early if the socket is full.
fn flush(conn: &mut dyn TlsSide, sock: &mut TcpStream) -> io::Result<()> {
    while conn.wants_write() {
        match conn.write_tls(sock) {
            Ok(0) => break,
            Ok(_) => {}
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// What a plaintext drain found.
enum Drained {
    /// Nothing more to read right now; the connection is still open.
    Open,
    /// The peer is finished — a `close_notify` (which `read_plain` reports as
    /// `Ok(0)`) or a bare TCP close (`UnexpectedEof`). rustls reports the latter
    /// only after every pending record has been decrypted, so in both cases
    /// everything the peer sent is already in hand. It is a close, not a fault.
    PeerClosed,
    /// The peer pump has dropped its receiver, so what was just read has nowhere
    /// to go. Not a failure: the other direction has already finished.
    Orphaned,
}

/// Empty the connection's plaintext buffer, publishing each chunk to the peer.
///
/// **This has to run after every `process_new_packets`, not once the socket has
/// gone dry.** rustls caps its received-plaintext buffer at 16 KiB
/// (`DEFAULT_RECEIVED_PLAINTEXT_LIMIT`, `common_state.rs`) and refuses to read
/// more TLS into the connection while that buffer is full:
///
/// ```text
/// // rustls  conn.rs, read_tls()
/// if self.received_plaintext.is_full() {
///     return Err(io::Error::new(io::ErrorKind::Other, "received plaintext buffer full"));
/// }
/// ```
///
/// Its documentation states the contract outright: "in order to empty the
/// incoming plaintext data buffer, you should empty it through the reader()
/// after the call to process_new_packets()". A record loop that reads the socket
/// to dryness first breaks it — an origin that bursts a handful of records (all
/// of them do) overruns 16 KiB before the drain is ever reached, `read_tls`
/// answers with that error, and the pump dies. From the client that is a
/// connection closed before any response: `000`, or a body cut short. Measured
/// on `artstation.com`, where four of five requests failed and the relay line
/// read `down=Err(... "received plaintext buffer full")`.
fn drain_plain(
    conn: &mut dyn TlsSide,
    buf: &mut [u8],
    outbound: &mpsc::Sender<Vec<u8>>,
    stats: &Stats,
    direction: Direction,
) -> io::Result<Drained> {
    loop {
        match conn.read_plain(buf) {
            Ok(0) => return Ok(Drained::PeerClosed),
            Ok(n) => {
                log::info!("mitm-pump {direction:?}: read_plain {n}");
                // Counted before the bytes are published, not after. A reader
                // that correlates the counter with the bytes it received would
                // otherwise race the two, and see a chunk whose bytes are not
                // counted yet.
                stats.record_bytes(direction, n as u64);
                if outbound.send(buf[..n].to_vec()).is_err() {
                    return Ok(Drained::Orphaned);
                }
            }
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(Drained::Open),
            Err(ref err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                log::info!("mitm-pump {direction:?}: peer closed without close_notify");
                return Ok(Drained::PeerClosed);
            }
            Err(err) => return Err(err),
        }
    }
}

/// Write everything still pending and get it onto the wire, before returning.
///
/// Three things have to be empty before a pump may return, and they are not the
/// same thing:
///
///   * the channel — bytes the peer has already read out of its own connection
///     and handed over. Draining it at the top of the loop is not enough: the
///     peer may have produced more since, and returning without taking them
///     throws away data that has already crossed the wire once.
///   * `pending` — plaintext the sink would not take, because rustls's send
///     buffer is bounded (64 KiB by default). A response larger than that buffer
///     always has a tail left here, and a bare `return` drops it.
///   * the connection's own output — TLS records written but not yet handed to
///     the socket, which `flush` leaves behind whenever the socket is full.
///
/// Missing any one of them is a truncated response, and which one was missing is
/// invisible from outside: all three look like the peer stopping early. Measured
/// against one 88 KB URL it came back as 16915, 45669, 51145, 53884 and 70792
/// bytes on different attempts, with `curl: (18)` each time, while the origin
/// sends 88239 every time.
///
/// Bounded rather than unbounded: a peer that has stopped reading must not pin
/// the thread, and the bound is generous for a buffer drain and short for any
/// client timeout.
fn finish(
    conn: &mut dyn TlsSide,
    sock: &mut TcpStream,
    pending: &mut Vec<u8>,
    inbound: &mpsc::Receiver<Vec<u8>>,
) -> io::Result<()> {
    for _ in 0..2000 {
        loop {
            match inbound.try_recv() {
                Ok(chunk) => {
                    if !chunk.is_empty() {
                        pending.extend_from_slice(&chunk);
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        while !pending.is_empty() {
            let accepted = conn.write_plain(pending)?;
            if accepted == 0 {
                break;
            }
            pending.drain(..accepted);
        }
        flush(conn, sock)?;
        if pending.is_empty() && !conn.wants_write() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(1));
    }
    log::warn!(
        "mitm-pump: gave up finishing, {} bytes pending and wants_write={}",
        pending.len(),
        conn.wants_write()
    );

    Ok(())
}

/// The bits of a rustls connection the relay needs, so one code path can drive
/// a server side and a client side.
trait TlsSide: Send {
    fn is_handshaking(&self) -> bool;
    fn wants_read(&self) -> bool;
    fn wants_write(&self) -> bool;
    fn read_tls(&mut self, rd: &mut dyn Read) -> io::Result<usize>;
    fn write_tls(&mut self, wr: &mut dyn Write) -> io::Result<usize>;
    fn process_new_packets(&mut self) -> Result<rustls::IoState, rustls::Error>;
    fn read_plain(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_plain(&mut self, buf: &[u8]) -> io::Result<usize>;
}

impl TlsSide for ServerConnection {
    // These bodies dereference through `**self` — the `ConnectionCommon` this
    // connection derefs to — rather than naming `ServerConnection::read_tls` and
    // friends. The reason is not style: `ServerConnection` has no inherent
    // method by those names (they live on `ConnectionCommon`, reached by
    // `Deref`), and a path like `ServerConnection::read_tls` does not follow
    // `Deref`, so it resolves to *this* trait method instead. That is an
    // infinite recursion the compiler warns about but does not reject, so a
    // handshake would have hung the connection thread rather than failing.
    // Dereferencing first picks the inherent method and cannot recurse.
    fn is_handshaking(&self) -> bool {
        (**self).is_handshaking()
    }
    fn wants_read(&self) -> bool {
        (**self).wants_read()
    }
    fn wants_write(&self) -> bool {
        (**self).wants_write()
    }
    fn read_tls(&mut self, rd: &mut dyn Read) -> io::Result<usize> {
        (**self).read_tls(rd)
    }
    fn write_tls(&mut self, wr: &mut dyn Write) -> io::Result<usize> {
        (**self).write_tls(wr)
    }
    fn process_new_packets(&mut self) -> Result<rustls::IoState, rustls::Error> {
        (**self).process_new_packets()
    }
    fn read_plain(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Read::read(&mut self.reader(), buf)
    }
    fn write_plain(&mut self, buf: &[u8]) -> io::Result<usize> {
        Write::write(&mut self.writer(), buf)
    }
}

impl TlsSide for ClientConnection {
    // See the `ServerConnection` impl above: `**self` is the `ConnectionCommon`
    // these methods live on, and naming `ClientConnection::read_tls` would
    // resolve to this trait's method and recurse forever.
    fn is_handshaking(&self) -> bool {
        (**self).is_handshaking()
    }
    fn wants_read(&self) -> bool {
        (**self).wants_read()
    }
    fn wants_write(&self) -> bool {
        (**self).wants_write()
    }
    fn read_tls(&mut self, rd: &mut dyn Read) -> io::Result<usize> {
        (**self).read_tls(rd)
    }
    fn write_tls(&mut self, wr: &mut dyn Write) -> io::Result<usize> {
        (**self).write_tls(wr)
    }
    fn process_new_packets(&mut self) -> Result<rustls::IoState, rustls::Error> {
        (**self).process_new_packets()
    }
    fn read_plain(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Read::read(&mut self.reader(), buf)
    }
    fn write_plain(&mut self, buf: &[u8]) -> io::Result<usize> {
        Write::write(&mut self.writer(), buf)
    }
}

/// Answers every handshake with a leaf for the name the client asked for.
#[derive(Debug)]
struct LeafResolver {
    ca: Arc<Authority>,
}

impl ResolvesServerCert for LeafResolver {
    fn resolve(&self, hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = hello.server_name()?;
        match self.ca.leaf_for(name) {
            Ok(leaf) => Some(leaf),
            Err(err) => {
                // No certificate means no handshake. Saying which name failed
                // is the only way to tell this apart from a network problem.
                log::warn!("mitm: could not mint a certificate for {name}: {err}");
                None
            }
        }
    }
}

fn server_config(ca: Arc<Authority>) -> io::Result<rustls::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(to_io)?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(LeafResolver { ca }))
        .pipe(Ok)
}

/// A client configuration that accepts any origin certificate.
///
/// See the module docs for why this is not a defect: reaching an address whose
/// certificate does not cover the name is the entire purpose, so verification
/// here would refuse every connection the proxy exists to serve.
fn client_config() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyOrigin))
        .with_no_client_auth()
}

#[derive(Debug)]
struct AcceptAnyOrigin;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyOrigin {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn to_io<E: std::fmt::Display>(err: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

/// Tiny helper so `server_config` can stay an expression.
trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use watt_rules::RuleSource;

    /// One source of unique temp-directory names within a process.
    static SERIAL: AtomicU64 = AtomicU64::new(0);

    const DOC: &str = r#"{"meta":{"version":"t"},"groups":[{"group":"g","entries":[
        {"id":"a","name":"a","ips":["203.0.113.10"],"domains":["a.example"]}]}]}"#;

    /// A connection that cannot handshake must count as exactly one connection,
    /// and nothing else.
    ///
    /// The point is the negative half. `connections` is bumped by the accept
    /// loop and every other counter by `serve_one`, which runs on its own
    /// thread; a bump placed too early would still pass a test that only
    /// checked `connections`. Asserting the rest are zero is what pins the
    /// counting to the branches it belongs to.
    #[test]
    fn a_connection_that_cannot_handshake_counts_only_connections() {
        let rules = RuleSet::from_slice(DOC.as_bytes(), RuleSource::Provided)
            .expect("the fixture compiles");

        let dir = std::env::temp_dir().join(format!(
            "watt-mitm-stats-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("the temp CA directory is writable");
        let authority = Authority::load_or_create(&dir).expect("the authority is built");

        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port is available");
        let addr = listener.local_addr().expect("the listener reports its address");
        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));

        let worker = thread::Builder::new()
            .name("watt-mitm-test".to_string())
            .spawn({
                let stats = Arc::clone(&stats);
                let stop = Arc::clone(&stop);
                let router = Arc::new(Mutex::new(Router::new(rules)));
                let authority = Arc::new(authority);
                move || {
                    let _ = serve_until(listener, router, authority, stats, stop);
                }
            })
            .expect("the accept loop thread starts");

        // A plain connection, closed at once: no ClientHello is ever sent, so
        // the handshake cannot complete and the other counters must not move.
        let client = TcpStream::connect(addr).expect("the listener accepts");
        let _ = client.shutdown(Shutdown::Both);

        let deadline = Instant::now() + Duration::from_secs(5);
        while stats.snapshot().connections < 1 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.connections, 1, "the accept loop counted the connection");
        assert_eq!(snapshot.handshakes, 0, "a plain connection cannot handshake");
        assert_eq!(snapshot.served, 0);
        assert_eq!(snapshot.refused, 0);
        assert_eq!(snapshot.dial_failures, 0);
        assert_eq!(snapshot.named_upstreams, 0);
        assert_eq!(snapshot.no_sni_upstreams, 0);
        assert_eq!(snapshot.bytes_to_upstream, 0);
        assert_eq!(snapshot.bytes_to_client, 0);

        stop.store(true, Ordering::SeqCst);
        let _ = worker.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A request that was already decrypted before the pump started must still
    /// be forwarded.
    ///
    /// The handshake reads the socket to completion, so a client that writes its
    /// request in the same segment as its Finished has that request decrypted
    /// and buffered by the time `pump` runs. A pump that only calls `read_plain`
    /// after a *fresh* `read_tls` never looks at it, and the connection stalls
    /// with the request forwarded nowhere — measured on a device as
    /// `bytes_to_upstream = 0` on a connection that had handshaken and been
    /// counted as served.
    ///
    /// The socket here is deliberately silent — every `read_tls` returns
    /// `WouldBlock` — so the only bytes the pump can possibly find are the ones
    /// already buffered. That is the whole of the case, with nothing else to
    /// explain a pass.
    #[test]
    fn plaintext_buffered_before_the_pump_started_is_still_forwarded() {
        /// A TLS side that has one chunk already decrypted and never sees
        /// another record.
        struct Buffered {
            pending: Vec<u8>,
            taken: bool,
        }

        impl TlsSide for Buffered {
            fn is_handshaking(&self) -> bool {
                false
            }
            fn wants_read(&self) -> bool {
                true
            }
            fn wants_write(&self) -> bool {
                false
            }
            fn read_tls(&mut self, _rd: &mut dyn Read) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "no new records"))
            }
            fn write_tls(&mut self, _wr: &mut dyn Write) -> io::Result<usize> {
                Ok(0)
            }
            fn process_new_packets(&mut self) -> Result<rustls::IoState, rustls::Error> {
                // Unreachable by construction, and the `unreachable!` says so
                // rather than hiding it behind a stub: `pump` only calls this
                // after `read_tls` returned `Ok(_)`, and this fake never does.
                // `IoState`'s fields are private and it has no `Default`, so
                // there is nothing honest to return here anyway. If this ever
                // fires, the fake stopped producing the case the test is about.
                unreachable!("the fake never produces a TLS record")
            }
            fn read_plain(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.taken {
                    // A clean close, so the pump returns rather than spinning.
                    return Ok(0);
                }
                let n = self.pending.len().min(buf.len());
                buf[..n].copy_from_slice(&self.pending[..n]);
                self.taken = true;
                Ok(n)
            }
            fn write_plain(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
        }

        const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: gravatar.com\r\n\r\n";

        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port is available");
        let addr = listener.local_addr().expect("the listener reports its address");
        let sock = TcpStream::connect(addr).expect("the pair connects");
        let _accepted = listener.accept().expect("the listener accepts");

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (_to_client_tx, to_client_rx) = mpsc::channel::<Vec<u8>>();
        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));

        let worker = thread::Builder::new()
            .name("watt-mitm-pump-test".to_string())
            .spawn({
                let stats = Arc::clone(&stats);
                let stop = Arc::clone(&stop);
                move || {
                    let _ = pump(
                        sock,
                        Box::new(Buffered {
                            pending: REQUEST.to_vec(),
                            taken: false,
                        }),
                        tx,
                        to_client_rx,
                        stats,
                        Direction::ToUpstream,
                        stop,
                    );
                }
            })
            .expect("the pump thread starts");

        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the buffered request is forwarded");
        assert_eq!(
            got, REQUEST,
            "a request decrypted before the pump started must reach the peer"
        );
        assert_eq!(
            stats.snapshot().bytes_to_upstream,
            REQUEST.len() as u64,
            "and it must be counted as forwarded, not silently dropped"
        );

        let _ = worker.join();
    }

    /// Reading a record must not stop the pump from draining the peer.
    ///
    /// The failure this guards against is a record loop that keeps going once it
    /// has seen a record: the pump then spins inside that pass, never returns to
    /// the channel, and the peer's bytes sit in it while the client waits out its
    /// whole timeout. Measured on a device it looked like this — `ToUpstream`
    /// silent for 7.6 s with no read and no write in between, on exactly the
    /// connections whose request arrived as a *fresh* record rather than being
    /// decrypted during the handshake.
    ///
    /// The fake reports one record and then silence, and the peer's bytes are
    /// sent *after* the pump starts, so the only way they can be written is for
    /// the pump to come back around to the channel.
    #[test]
    fn reading_a_record_does_not_stop_the_pump_draining_the_peer() {
        /// One record, then a dry socket.
        struct OneRecordThenSilence {
            records_left: usize,
            written: Arc<Mutex<Vec<u8>>>,
            /// A real connection, held only because `IoState` cannot be built
            /// outside rustls — its fields are private and it has no `Default`.
            /// Nothing is ever written to it or read from it.
            io_state_source: ClientConnection,
        }

        impl TlsSide for OneRecordThenSilence {
            fn is_handshaking(&self) -> bool {
                false
            }
            fn wants_read(&self) -> bool {
                true
            }
            fn wants_write(&self) -> bool {
                false
            }
            fn read_tls(&mut self, _rd: &mut dyn Read) -> io::Result<usize> {
                if self.records_left == 0 {
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "dry"));
                }
                self.records_left -= 1;
                Ok(1)
            }
            fn write_tls(&mut self, _wr: &mut dyn Write) -> io::Result<usize> {
                Ok(0)
            }
            fn process_new_packets(&mut self) -> Result<rustls::IoState, rustls::Error> {
                self.io_state_source.process_new_packets()
            }
            fn read_plain(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "no plaintext"))
            }
            fn write_plain(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.written.lock().expect("the sink is not poisoned").extend_from_slice(buf);
                Ok(buf.len())
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port is available");
        let addr = listener.local_addr().expect("the listener reports its address");
        let sock = TcpStream::connect(addr).expect("the pair connects");
        let _accepted = listener.accept().expect("the listener accepts");

        let io_state_source = ClientConnection::new(
            Arc::new(client_config()),
            ServerName::try_from("watt-mitm.test").expect("the name is valid"),
        )
        .expect("a client connection is built");
        let written = Arc::new(Mutex::new(Vec::new()));

        // The pump's own reads are not what this test is about — it asserts on
        // the sink — so the receiver is deliberately unused.
        let (tx, _rx) = mpsc::channel::<Vec<u8>>();
        let (peer_tx, peer_rx) = mpsc::channel::<Vec<u8>>();
        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));

        let worker = thread::Builder::new()
            .name("watt-mitm-pump-record-test".to_string())
            .spawn({
                let stats = Arc::clone(&stats);
                let stop = Arc::clone(&stop);
                let written = Arc::clone(&written);
                move || {
                    let _ = pump(
                        sock,
                        Box::new(OneRecordThenSilence {
                            records_left: 1,
                            written,
                            io_state_source,
                        }),
                        tx,
                        peer_rx,
                        stats,
                        Direction::ToUpstream,
                        stop,
                    );
                }
            })
            .expect("the pump thread starts");

        // Sent after the pump is running: the pump has already seen its one
        // record by now, which is the state the old shape never left.
        thread::sleep(Duration::from_millis(100));
        peer_tx.send(b"GET / HTTP/1.1\r\n\r\n".to_vec()).expect("the peer is reachable");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if !written.lock().expect("the sink is not poisoned").is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the pump never drained the peer after reading a record"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            written.lock().expect("the sink is not poisoned").as_slice(),
            b"GET / HTTP/1.1\r\n\r\n",
            "the peer's bytes must reach the connection"
        );

        stop.store(true, Ordering::SeqCst);
        let _ = worker.join();
    }

    /// A socket EOF must not drop the plaintext that arrived with it.
    ///
    /// `read_tls` returning `Ok(0)` means the TCP socket reached EOF, not that
    /// the connection is finished: the records already read have not been
    /// decrypted, and the plaintext they hold has not been drained. Returning on
    /// EOF — which this did — loses that tail, and the peer sees a response that
    /// stops early. Measured against one URL, the body came back 22902, 5106,
    /// 3736, 6475 and 2367 bytes long over five attempts, where the origin sends
    /// 25171 every time.
    #[test]
    fn a_socket_eof_still_drains_the_plaintext_that_came_with_it() {
        struct OneRecordThenEof {
            written: Arc<Mutex<Vec<u8>>>,
            reads: usize,
            /// See the note in the record-loop test: `IoState` cannot be built
            /// outside rustls, so a real connection is borrowed for it.
            io_state_source: ClientConnection,
        }

        impl TlsSide for OneRecordThenEof {
            fn is_handshaking(&self) -> bool {
                false
            }
            fn wants_read(&self) -> bool {
                true
            }
            fn wants_write(&self) -> bool {
                false
            }
            fn read_tls(&mut self, _rd: &mut dyn Read) -> io::Result<usize> {
                self.reads += 1;
                // One record, then the socket is gone.
                if self.reads == 1 {
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
            fn write_tls(&mut self, _wr: &mut dyn Write) -> io::Result<usize> {
                Ok(0)
            }
            fn process_new_packets(&mut self) -> Result<rustls::IoState, rustls::Error> {
                self.io_state_source.process_new_packets()
            }
            fn read_plain(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                // The plaintext that the record above produced.
                let payload = b"the tail that must not be dropped";
                if self.reads > 1 {
                    let n = payload.len().min(buf.len());
                    buf[..n].copy_from_slice(&payload[..n]);
                    self.reads = 0; // one delivery, then a clean close
                    return Ok(n);
                }
                Err(io::Error::new(io::ErrorKind::WouldBlock, "nothing yet"))
            }
            fn write_plain(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.written.lock().expect("the sink is not poisoned").extend_from_slice(buf);
                Ok(buf.len())
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port is available");
        let addr = listener.local_addr().expect("the listener reports its address");
        let sock = TcpStream::connect(addr).expect("the pair connects");
        let _accepted = listener.accept().expect("the listener accepts");

        let io_state_source = ClientConnection::new(
            Arc::new(client_config()),
            ServerName::try_from("watt-mitm.test").expect("the name is valid"),
        )
        .expect("a client connection is built");
        let written = Arc::new(Mutex::new(Vec::new()));

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (_peer_tx, peer_rx) = mpsc::channel::<Vec<u8>>();
        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));

        let worker = thread::Builder::new()
            .name("watt-mitm-pump-eof-test".to_string())
            .spawn({
                let stats = Arc::clone(&stats);
                let stop = Arc::clone(&stop);
                let written = Arc::clone(&written);
                move || {
                    let _ = pump(
                        sock,
                        Box::new(OneRecordThenEof {
                            written,
                            reads: 0,
                            io_state_source,
                        }),
                        tx,
                        peer_rx,
                        stats,
                        Direction::ToUpstream,
                        stop,
                    );
                }
            })
            .expect("the pump thread starts");

        // The plaintext read out of the connection goes to the peer over the
        // channel; `write_plain` is the other direction.
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the plaintext that arrived with the EOF was dropped");
        assert_eq!(
            got, b"the tail that must not be dropped",
            "the whole chunk must reach the peer, not a prefix of it"
        );

        stop.store(true, Ordering::SeqCst);
        let _ = worker.join();
    }

    /// A burst of records must not overflow rustls's plaintext buffer.
    ///
    /// The received-plaintext buffer holds 16 KiB
    /// (`DEFAULT_RECEIVED_PLAINTEXT_LIMIT`, `common_state.rs`) and `read_tls`
    /// refuses to read more once it is full, answering
    /// `Err(ErrorKind::Other, "received plaintext buffer full")`. rustls states
    /// the contract in its own doc comment: the buffer is emptied "through the
    /// reader() after the call to process_new_packets()". A record loop that
    /// reads the socket to dryness first breaks that contract — a few records
    /// from any real origin overrun 16 KiB before the drain is reached — and the
    /// pump dies with that error. From the client that is a connection closed
    /// before any response: `000`, or a body cut short. Measured on
    /// `artstation.com`, where four of five requests failed and the relay line
    /// read `down=Err(... "received plaintext buffer full")`.
    ///
    /// The fake below reproduces rustls's buffering exactly — records arrive in
    /// a burst, each adding plaintext, and `read_tls` fails once the undrained
    /// total reaches the limit — so the failure mode is the real one rather than
    /// an approximation of it.
    #[test]
    fn a_burst_of_records_does_not_overflow_the_plaintext_buffer() {
        /// rustls's limit, verbatim from `common_state.rs`.
        const LIMIT: usize = 16 * 1024;
        /// What one record decrypts to.
        const RECORD: usize = 4096;
        /// Enough records that reading them all without draining would overrun.
        const RECORDS: usize = 8;

        struct Bursting {
            records_left: usize,
            /// Plaintext produced but not yet read out by the application — the
            /// quantity rustls's `is_full()` looks at.
            undrained: usize,
            /// See the note in the record-loop test: `IoState` cannot be built
            /// outside rustls, so a real connection is borrowed for it.
            io_state_source: ClientConnection,
        }

        impl TlsSide for Bursting {
            fn is_handshaking(&self) -> bool {
                false
            }
            fn wants_read(&self) -> bool {
                true
            }
            fn wants_write(&self) -> bool {
                false
            }
            fn read_tls(&mut self, _rd: &mut dyn Read) -> io::Result<usize> {
                // The check rustls makes before it reads anything.
                if self.undrained >= LIMIT {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "received plaintext buffer full",
                    ));
                }
                if self.records_left == 0 {
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "no more records"));
                }
                self.records_left -= 1;
                self.undrained += RECORD;
                Ok(RECORD)
            }
            fn write_tls(&mut self, _wr: &mut dyn Write) -> io::Result<usize> {
                Ok(0)
            }
            fn process_new_packets(&mut self) -> Result<rustls::IoState, rustls::Error> {
                self.io_state_source.process_new_packets()
            }
            fn read_plain(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.undrained > 0 {
                    let n = self.undrained.min(buf.len());
                    for byte in &mut buf[..n] {
                        *byte = b'x';
                    }
                    self.undrained -= n;
                    return Ok(n);
                }
                if self.records_left > 0 {
                    // More is coming; not a close.
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "nothing yet"));
                }
                // Everything sent has been handed over, and the peer is done.
                Ok(0)
            }
            fn write_plain(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port is available");
        let addr = listener.local_addr().expect("the listener reports its address");
        let sock = TcpStream::connect(addr).expect("the pair connects");
        let _accepted = listener.accept().expect("the listener accepts");

        let io_state_source = ClientConnection::new(
            Arc::new(client_config()),
            ServerName::try_from("watt-mitm.test").expect("the name is valid"),
        )
        .expect("a client connection is built");

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (_peer_tx, peer_rx) = mpsc::channel::<Vec<u8>>();
        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));
        let outcome = Arc::new(Mutex::new(None::<String>));

        let worker = thread::Builder::new()
            .name("watt-mitm-pump-burst-test".to_string())
            .spawn({
                let stats = Arc::clone(&stats);
                let stop = Arc::clone(&stop);
                let outcome = Arc::clone(&outcome);
                move || {
                    let result = pump(
                        sock,
                        Box::new(Bursting {
                            records_left: RECORDS,
                            undrained: 0,
                            io_state_source,
                        }),
                        tx,
                        peer_rx,
                        stats,
                        Direction::ToUpstream,
                        stop,
                    );
                    let text = match result {
                        Ok(()) => "ok".to_string(),
                        Err(err) => format!("err: {err}"),
                    };
                    *outcome.lock().expect("the outcome slot is not poisoned") = Some(text);
                }
            })
            .expect("the pump thread starts");

        // Every record's plaintext has to arrive. The pump returning early is
        // what makes this fail: the sender is dropped, `recv_timeout` reports a
        // disconnect, and the assertion below says how much was missing.
        let expected = RECORD * RECORDS;
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.len() < expected && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => got.extend_from_slice(&chunk),
                Err(_) => break,
            }
        }
        assert_eq!(
            got.len(),
            expected,
            "each record's plaintext must be drained as it arrives, not after the socket is dry"
        );

        stop.store(true, Ordering::SeqCst);
        worker.join().expect("the pump thread does not panic");
        let text = outcome
            .lock()
            .expect("the outcome slot is not poisoned")
            .clone();
        assert_eq!(
            text.as_deref(),
            Some("ok"),
            "the pump must survive the burst rather than dying on it"
        );
    }
}

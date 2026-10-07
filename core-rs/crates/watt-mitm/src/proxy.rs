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

    let (addresses, port) = {
        let router = router
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "the rule table is poisoned"))?;
        let plan = router.plan(Instant::now(), &name, Family::V4);
        match plan.strategy {
            // Only a rule address is usable. See the module docs: a fallback to
            // the system resolver here would dial this very listener, because
            // the hosts rewrite has already made the name point at it.
            Strategy::RuleAddresses | Strategy::PlaceholderFallback | Strategy::FamilyFallback => {
                (plan.addresses, plan.port)
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
    let upstream = match dial(&addresses, port) {
        Some(upstream) => upstream,
        None => {
            stats.dial_failures.fetch_add(1, Ordering::Relaxed);
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                format!("none of the {} addresses for {name} accepted a connection", addresses.len()),
            ));
        }
    };

    let server_name = ServerName::try_from(name.clone())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
    let mut client_conn = ClientConnection::new(client_config, server_name)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;

    let mut upstream = upstream;
    upstream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    upstream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    handshake(&mut client_conn, &mut upstream)?;

    log::debug!(
        "mitm: {peer} asked for {name}, served from {:?} ({} candidate(s))",
        upstream.peer_addr().ok(),
        addresses.len()
    );

    stats.served.fetch_add(1, Ordering::Relaxed);
    relay(client, server_conn, upstream, client_conn, stats, stop)
}

/// Pick the first candidate that accepts a connection.
///
/// Sequential rather than raced, and short per candidate: this is the same
/// trade the no-root proxy makes, and for the same reason — a rule lists its
/// addresses in preference order, and paying the full timeout on a dead first
/// entry would make a working rule look broken.
fn dial(addresses: &[std::net::IpAddr], port: u16) -> Option<TcpStream> {
    for address in addresses {
        let target = SocketAddr::new(*address, port);
        match TcpStream::connect_timeout(&target, DIAL_TIMEOUT) {
            Ok(stream) => return Some(stream),
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

    let _ = up.join();
    let _ = down.join();
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
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }

        // Everything the peer produced goes into this side's TLS connection.
        let mut queued = false;
        loop {
            match inbound.try_recv() {
                Ok(chunk) => {
                    if !chunk.is_empty() {
                        conn.write_plain(&chunk)?;
                        queued = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // The peer is gone; nothing more will arrive for this socket.
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if queued {
            flush(&mut *conn, &mut sock)?;
        }

        // Pull whatever TLS records are waiting.
        let mut saw_records = false;
        loop {
            match conn.read_tls(&mut sock) {
                Ok(0) => return Ok(()),
                Ok(_) => {
                    saw_records = true;
                    conn.process_new_packets()
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
                    loop {
                        match conn.read_plain(&mut buf) {
                            Ok(0) => {
                                // Clean close from this side; tell the peer so
                                // it can stop waiting.
                                // Not counted: the empty marker carries no
                                // bytes, only the close.
                                let _ = outbound.send(Vec::new());
                                return Ok(());
                            }
                            Ok(n) => {
                                if outbound.send(buf[..n].to_vec()).is_err() {
                                    return Ok(());
                                }
                                stats.record_bytes(direction, n as u64);
                            }
                            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
                            Err(err) => return Err(err),
                        }
                    }
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => return Err(err),
            }
        }

        if !saw_records {
            // Idle on both sides. Wait for the peer's plaintext rather than
            // spinning; a write from the peer returns immediately, so this is
            // a bound on how long a direction can sit still, not added latency.
            match inbound.recv_timeout(IDLE) {
                Ok(chunk) => {
                    if !chunk.is_empty() {
                        conn.write_plain(&chunk)?;
                        flush(&mut *conn, &mut sock)?;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
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
        assert_eq!(snapshot.bytes_to_upstream, 0);
        assert_eq!(snapshot.bytes_to_client, 0);

        stop.store(true, Ordering::SeqCst);
        let _ = worker.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

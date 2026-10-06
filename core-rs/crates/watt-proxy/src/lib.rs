//! No-Root proxy mode: a CONNECT tunnel and a plain-HTTP forwarder on one port.
//!
//! This is intentionally separate from the TUN engine. A TUN requires Android's
//! `VpnService` to own the descriptor and route all app traffic; a listener on
//! a loopback port needs neither root nor a synthetic local TLS server, and it
//! never terminates TLS — a CONNECT has its encrypted bytes copied without
//! inspection, and a forwarded request has its request line rewritten and the
//! rest copied.
//!
//! # Policy: a rule is a ranking of preferences, not a gate
//!
//! A domain the rule set mentions is dialled at the rule's addresses first,
//! because those are addresses the rule author already verified. A domain no
//! rule mentions is reached through the system resolver, exactly as the rest of
//! the device reaches it.
//!
//! Nothing is refused for being unlisted. An earlier revision answered `403`
//! there, on the reasoning that dialling an unlisted domain directly would
//! expose the user's address — but the tunnel does not work that way either
//! (an unlisted domain in VPN mode is `Plan::direct` and goes out untouched),
//! and the effect of the stricter rule was that a browser pointed at the proxy
//! stopped working on any site the rule set had not heard of. A proxy that
//! serves only the domains a third-party list happens to name is not a proxy.
//!
//! The asymmetry runs the other way for *how* a listed domain is reached: if
//! every rule address refuses or times out, the system resolver gets the last
//! word. A rule is a hint that address X is better for this domain — usually it
//! is, sometimes the network disagrees, and sometimes the rule is simply stale.
//! Treating the hint as the sole route turns a stale entry into an outage for a
//! domain we already decided to serve. Falling back keeps the failure honest:
//! we only report 502 when *nothing* reached the host.
//!
//! # What this cannot do
//!
//! Only traffic the client *chooses* to send here is served, so it has to be
//! told to use the proxy — and a client that ignores its proxy setting, or
//! speaks QUIC over UDP/443 instead of TCP, bypasses this entirely. That is the
//! price of needing no permission, and it is why the tunnel exists as well.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use watt_rules::{Family, Outcome, Router, Strategy};

const HEADER_LIMIT: usize = 16 * 1024;
/// Per candidate, not per request.
///
/// A rule lists its addresses in the order it prefers, and the first one is
/// often unreachable from a given network. Waiting the full budget on each would
/// make a working rule look broken, so the timeout is short and every candidate
/// gets a turn.
///
/// Three seconds rather than five: a TCP handshake that has not completed in
/// three seconds is not going to serve an interactive request anyway, and the
/// merged rule set can put dozens of candidates in front of a live one.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long after the previous candidate the next one is started.
///
/// A quarter second is long enough that a candidate which is going to answer at
/// all has usually answered, and short enough that a dead one is not waited on.
/// The head start is what keeps the rule author's ordering meaningful: with an
/// even race the fastest handshake wins, and the fastest handshake is not
/// necessarily the address that serves the right certificate.
const CONNECT_STAGGER: Duration = Duration::from_millis(250);

/// How long a certificate-verified candidate is given alone before unverified
/// ones are allowed to join the race.
///
/// Sized to cover a handshake on a slow link, which is what the verified address
/// has to complete before it can win. A confirmed address that is merely slow
/// still beats an unverified one that is fast, which is the entire point: the
/// verified one is known to serve this host and the other is not.
const VERIFIED_HEAD_START: Duration = Duration::from_millis(1200);

/// How many candidates a single request will try.
///
/// The merged rule set can list hundreds for one domain — `d2.baidupcs.com`
/// carries a hundred and fourteen — and one thread each is not a plan. The cap
/// costs little: addresses that fail sink in the ranking, so the next request
/// starts with the ones this one never reached.
const MAX_CANDIDATES: usize = 12;

/// How many times a session may replace an upstream that never answered.
///
/// Each round is a staggered batch, so the cost is bounded but not free. Two
/// rounds covers the measured cases — `www.xbox.com` needs one — and giving up
/// after that closes the connection instead of holding the client open.
const MAX_FAILOVERS: usize = 2;

/// How long an upstream may stay silent before the next batch is tried.
const FIRST_BYTE_DEADLINE: Duration = Duration::from_secs(3);

const COPY_BUFFER: usize = 32 * 1024;

/// How often the rules' dial names are re-resolved.
///
/// Resolving per request would be a DNS lookup per request to learn something
/// that changes on the order of minutes. This is the `dynamic a <name>` cadence
/// from Caddy, for the same reason.
const DIAL_REFRESH: Duration = Duration::from_secs(60);

/// Serve HTTP CONNECT requests until the process receives SIGTERM/SIGINT.
///
/// The listener itself is blocking and each accepted connection gets one thread;
/// this is the bootstrap implementation for Android, where the client count is
/// small and the important property is that no thread owns or decrypts TLS data.
pub fn serve(listener: TcpListener, router: Router) -> io::Result<()> {
    serve_until(
        listener,
        Arc::new(Mutex::new(router)),
        Arc::new(AtomicBool::new(false)),
    )
}

/// How often the accept loop looks at the stop flag while idle.
///
/// The listener is non-blocking so the flag can be seen at all: a blocking
/// `accept` has no way to be interrupted from another thread, and the only
/// alternative — connecting to our own port to wake it — is a hack that shows up
/// in the logs as a spurious failed connection.
const IDLE_POLL: Duration = Duration::from_millis(50);

/// Serve until `stop` is set.
///
/// Split from [`serve`] because the shell needs to shut the proxy down without
/// killing the process: on Android the daemon and the proxy live in the same
/// app, so "exit the process" is not available.
///
/// Takes the shared router rather than building it, so the caller keeps a handle
/// and can swap the rule set while this is running. Sharing and mutability are
/// both load-bearing: shared because the router learns (an address that just
/// refused should not be first in line next time), mutable because
/// [`Router::replace_rules`] is how a rule switch reaches a live proxy. The lock
/// is held only across a plan or a report, never across a connection.
pub fn serve_until(
    listener: TcpListener,
    router: Arc<Mutex<Router>>,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    log::info!("proxy listening on {}", listener.local_addr()?);

    // Resolve once before serving, then on a tick. Without the first pass a dial
    // name would be dead until the first tick came round, which reads as "the
    // feature does not work" rather than "the feature has not run yet".
    spawn_dial_refresh(Arc::clone(&router), Arc::clone(&stop));

    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let router = Arc::clone(&router);
                thread::Builder::new()
                    .name("watt-connect".to_string())
                    .spawn(move || {
                        if let Err(err) = handle(stream, &router) {
                            log::warn!("proxy connection failed: {err}");
                        }
                    })
                    .map_err(|err| io::Error::other(format!("spawn proxy worker: {err}")))?;
            }
            // Nothing waiting. This is the normal case, not an error.
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(IDLE_POLL);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }

    log::info!("proxy stopped");
    Ok(())
}

/// The client's request head, and anything it sent past it.
struct Head {
    /// The head verbatim, including the blank line that ends it.
    raw: Vec<u8>,
    /// Bytes read past that blank line. On a forwarded request they are the
    /// start of the body; on a CONNECT they are whatever the client pipelined
    /// before waiting for the tunnel. Either way they belong to the upstream,
    /// and dropping them is silent data loss.
    leftover: Vec<u8>,
}

/// Read one request head.
///
/// Reads bytes and stops at the blank line rather than filling a buffer,
/// because everything after the head has to survive. A `BufReader` reads ahead
/// by design, and the bytes it swallowed go with it when it is dropped — which
/// is invisible for a CONNECT, whose client waits for the `200`, and fatal for
/// a `POST`, whose body arrives alongside the head.
///
/// `Ok(None)` means the client closed without sending a head: not an error, and
/// not worth a log line. `Err` of kind [`io::ErrorKind::InvalidData`] means the
/// head is over [`HEADER_LIMIT`], so the caller can answer 431 rather than drop
/// the connection unexplained.
fn read_head(client: &mut TcpStream) -> io::Result<Option<Head>> {
    let mut pending: Vec<u8> = Vec::new();
    let mut buffer = vec![0u8; 4096];
    loop {
        if let Some(end) = head_end(&pending) {
            let leftover = pending.split_off(end);
            return Ok(Some(Head {
                raw: pending,
                leftover,
            }));
        }
        if pending.len() >= HEADER_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the request head is larger than the limit",
            ));
        }
        let read = client.read(&mut buffer)?;
        if read == 0 {
            return if pending.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the client closed in the middle of a request head",
                ))
            };
        }
        pending.extend_from_slice(&buffer[..read]);
    }
}

/// Where a request head ends: the index just past its blank line.
///
/// `\r\n\r\n` is what every real client sends. Bare `\n\n` is accepted because
/// a hand-written probe sends it, and refusing would be a mystery to whoever
/// wrote the probe. The earlier of the two wins, so a head that contains both
/// is not cut in the wrong place.
fn head_end(bytes: &[u8]) -> Option<usize> {
    let crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4);
    let lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| index + 2);
    match (crlf, lf) {
        (Some(crlf), Some(lf)) => Some(crlf.min(lf)),
        (found, None) | (None, found) => found,
    }
}

/// The first line of a head, without its terminator, as text.
///
/// Lossy on purpose: a request line is ASCII by specification, and a client
/// that sends something else is answered 400 by the caller rather than dropped
/// with an encoding error it cannot see.
fn first_line(raw: &[u8]) -> &str {
    let end = raw.iter().position(|byte| *byte == b'\n').unwrap_or(raw.len());
    let line = &raw[..end];
    let line = match line.split_last() {
        Some((&b'\r', rest)) => rest,
        _ => line,
    };
    std::str::from_utf8(line).unwrap_or("")
}

/// The bytes a CONNECT client sends once the tunnel is open.
///
/// `None` means the client closed without sending any: a session that never
/// started, which is not a failure and must not be reported as one.
fn read_opening(host: &str, client: &mut TcpStream) -> io::Result<Option<Vec<u8>>> {
    // One read is enough: a ClientHello arrives in a single segment, and
    // anything beyond it stays queued in the socket for the relay to pick up.
    client.set_read_timeout(Some(FIRST_BYTE_DEADLINE))?;
    let mut opening = vec![0u8; COPY_BUFFER];
    match client.read(&mut opening) {
        Ok(0) => {
            log::info!("proxy: {host} client closed before sending anything");
            Ok(None)
        }
        Ok(len) => Ok(Some(opening[..len].to_vec())),
        Err(err) => {
            log::info!("proxy: {host} reading the client's opening bytes: {err}");
            Err(err)
        }
    }
}

fn handle(mut client: TcpStream, router: &Mutex<Router>) -> io::Result<()> {
    client.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    client.set_write_timeout(Some(CONNECT_TIMEOUT))?;

    let head = match read_head(&mut client) {
        Ok(Some(head)) => head,
        // A connection that opens and says nothing. Port scanners, health
        // checks and a browser's speculative preconnect all look like this, so
        // it is not worth a log line.
        Ok(None) => return Ok(()),
        Err(err) if err.kind() == io::ErrorKind::InvalidData => {
            return response(&mut client, "431 Request Header Fields Too Large\r\n\r\n");
        }
        Err(err) => return Err(err),
    };

    let request_line = first_line(&head.raw);
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if method.is_empty() || target.is_empty() || version.is_empty() {
        return response(&mut client, "400 Bad Request\r\n\r\n");
    }

    // Two protocols share this port and the first line is all they agree on. A
    // CONNECT names an authority and asks for a tunnel; a request carrying an
    // absolute URI is addressed to us *as a proxy*, and is forwarded with the
    // URI rewritten into the origin-form the server expects.
    let (host, port, opening, tunnelled) = if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = parse_authority(target) else {
            return response(&mut client, "400 Bad Request\r\n\r\n");
        };
        (host, port, head.leftover, true)
    } else if let Some(after_scheme) = target.strip_prefix("http://") {
        let Some((host, port, path)) = parse_absolute(after_scheme) else {
            return response(&mut client, "400 Bad Request\r\n\r\n");
        };
        let mut opening = rewrite_head(&head.raw, method, &path, version);
        // Anything the client sent past the head is the start of the body, and
        // it belongs to the upstream. Reading the head line-by-line into a
        // `BufReader` used to swallow these bytes and drop them with the reader
        // — invisible for a CONNECT, whose client waits for the `200` before
        // sending anything, and fatal for a `POST`, whose body arrives with the
        // head.
        opening.extend_from_slice(&head.leftover);
        (host, port, opening, false)
    } else if target.starts_with("https://") {
        // An absolute `https://` URI can only be served by terminating TLS: the
        // client sent its request line in the clear and expects a plaintext
        // reply. This proxy never terminates TLS, and a browser sends CONNECT
        // for HTTPS anyway, so this shape means a client that is not a browser
        // asking for something we do not do.
        log::warn!("proxy: refusing an absolute https:// request for {target}");
        return response(&mut client, "501 Not Implemented\r\n\r\n");
    } else {
        // Origin-form: the client is talking to us as though we were the
        // server. There is no way to know what it wants, and guessing from the
        // `Host` header would make a client with a misconfigured proxy look
        // like a working one.
        log::warn!("proxy: {method} {target} is not addressed to a proxy");
        return response(&mut client, "400 Bad Request\r\n\r\n");
    };

    let now = Instant::now();
    let plan = {
        let router = router.lock().expect("the router lock is never poisoned");
        router.plan(now, &host, Family::V4)
    };
    // Every request says what the rule set decided about it. Without this the
    // three outcomes that look identical from outside — no rule matched, a rule
    // matched with no addresses, a rule matched and was dialled — are one
    // indistinguishable timeout, and the first two are answered without a single
    // further line being written.
    log::info!(
        "proxy: {host} strategy={} addresses={} port={:?}",
        plan.strategy.as_str(),
        plan.addresses.len(),
        plan.port
    );
    if matches!(plan.strategy, Strategy::Direct) {
        log::info!("proxy: {host} is not in the rule set; going out directly");
    }

    // Three stages, tried in this order, each one strictly weaker than the last:
    //
    //   1. the rule's literal addresses, ranked by the selector,
    //   2. the rule's dial names, resolved now,
    //   3. the client's own name, through the system resolver.
    //
    // Literal addresses come first because they are data the rule author already
    // verified; a name is still a lookup that may fail. The client's own name
    // comes last because if the rule knew a better way to reach this domain, that
    // way has just been disproved — but the domain is still one we promised to
    // serve, so giving up entirely is the one thing we must not do.
    let rule_port = plan.port.unwrap_or(port);

    let mut literal: Vec<SocketAddr> = Vec::new();
    for ip in plan.addresses.iter().copied() {
        push_unique(&mut literal, SocketAddr::new(ip, rule_port));
    }

    // A rule address that just failed is worth less than a fresh fallback, so
    // when every literal candidate is in cooldown the fallback goes first.
    //
    // This is the only way the health history pays off for a domain that has
    // been narrowed to a single candidate. That used to be the norm here — an
    // earlier version of this comment said "the rule source averages barely one
    // address per domain" — but `/1` re-measured 2026-09-26 (5225 domains,
    // 15952 addresses, 3.05 per domain, 5225 domains with more than one
    // address) shows the selector now has real ranking room. The single-
    // candidate branch still matters because ranking can only reorder addresses
    // that exist: a domain whose other addresses were all refused or timed out
    // collapses to one, and roughly 3 in 10 sampled domains had no usable
    // address at all (18/25 = 72% usable). With nothing to rank, the only lever
    // left is the order of the stages themselves. The cooled address is still
    // tried, just last, so a transient failure costs a reordering rather than a
    // blacklist.
    let mut order = vec![Stage::Literal, Stage::System];
    if literal.is_empty() {
        // Nothing to rank. Either no rule matched — the ordinary case for a
        // domain the list has never heard of — or a rule matched without a
        // usable address. Both leave the system resolver as the only stage, and
        // the domain is reached the way the rest of the device reaches it.
        order.retain(|stage| !matches!(stage, Stage::Literal));
    } else if literal
        .iter()
        .all(|target| router.lock().map(|r| r.is_cooled(target.ip(), now)).unwrap_or(false))
    {
        order.retain(|stage| !matches!(stage, Stage::Literal));
        order.push(Stage::Literal);
        log::info!(
            "proxy: {host} rule addresses are all in cooldown; trying the fallback first"
        );
    }

    // Each stage is prepared only when it is reached. Resolving the later stages
    // up front would charge every successful request for work only the failing
    // ones need — and the common case is that the first stage works, so that
    // would be a lookup per request for nothing.
    let mut tried: Vec<SocketAddr> = Vec::new();
    let mut upstream = None;
    // The stage that actually carried the request. A separate variable from the
    // one naming the stage *being* tried, because a single one cannot do both:
    // it is only known to be correct after the dial succeeds, so every line
    // printed before that would carry the previous stage's name. Measured
    // 2026-10-06 on `www.baidu.com` (unlisted, so `order` is `[System]` alone):
    // the dial line read `from rule-addresses` while the very next line read
    // `served via system-resolution`.
    let mut served_by: Option<&'static str> = None;
    let mut stage_candidates: Vec<SocketAddr> = Vec::new();
    for stage in order {
        if upstream.is_some() {
            break;
        }
        let targets: Vec<SocketAddr> = match stage {
            Stage::Literal => literal.clone(),
            Stage::System => fallback_targets(&host, port, plan.port).unwrap_or_else(|err| {
                log::warn!("proxy: resolver declined to look up {host}: {err}");
                Vec::new()
            }),
        };
        let fresh: Vec<SocketAddr> = targets
            .into_iter()
            .filter(|target| !tried.contains(target))
            .collect();
        tried.extend(fresh.iter().copied());
        if fresh.is_empty() {
            continue;
        }
        // Kept for the relay: an address that accepts the connection and then
        // says nothing is the normal shape of SNI blocking, and the only way to
        // recover from it is to try the next one before the client gives up.
        // Rule addresses are the only stage that needs this. The dial-name and
        // system-resolver stages produce addresses the network itself vouched
        // for; a rule's addresses are a third party's belief about the domain,
        // and the belief is sometimes out of date — or, measured here, the
        // fastest of them is sometimes serving someone else's certificate.
        let mut verified = 0usize;
        // Only the rule's own addresses carry an author's ordering, so only they
        // get a certificate probe and the head start that protects that ordering.
        // A system-resolver answer has no author and is made of addresses the
        // network already vouched for; there the head start is pure latency.
        let mut head_start = Duration::ZERO;
        let fresh: Vec<SocketAddr> = if matches!(stage, Stage::Literal) {
            let (ordered, count) = prefer_covered(fresh, &host);
            verified = count;
            head_start = VERIFIED_HEAD_START;
            ordered
        } else {
            fresh
        };
        if fresh.is_empty() {
            continue;
        }

        stage_candidates = fresh.clone();
        // The dial is the decisive step and until now it reported nothing: a
        // request that timed out looked the same whether every candidate was
        // refused, every one connected and stayed silent, or the winning address
        // served the wrong certificate.
        log::info!(
            "proxy: {host} dialling {} candidate(s) from {} ({verified} verified): {:?}",
            fresh.len().min(MAX_CANDIDATES),
            stage.label(),
            fresh
                .iter()
                .take(4)
                .map(|target| target.ip())
                .collect::<Vec<_>>()
        );
        if let Some(stream) = connect_first(&fresh, router, now, verified, head_start) {
            log::info!(
                "proxy: {host} connected upstream via {}",
                stream
                    .peer_addr()
                    .map(|addr| addr.to_string())
                    .unwrap_or_else(|_| "?".to_string())
            );
            upstream = Some(stream);
            served_by = Some(stage.label());
        } else {
            log::warn!("proxy: {host} no candidate in {} connected", stage.label());
        }
    }

    let upstream = match upstream {
        Some(stream) => stream,
        None => return response(&mut client, "502 Bad Gateway\r\n\r\n"),
    };
    match served_by {
        // Worth a line: the rule for this domain is not describing a reachable
        // address any more, which is a signal the ruleset needs refreshing.
        Some(served_by) if served_by != Stage::Literal.label() => {
            log::info!("proxy: {host} served via {served_by}");
        }
        _ => {}
    }

    // Captured before the copy: after it, the socket may already be gone.
    let upstream_addr = upstream.peer_addr().ok().map(|addr| addr.ip());

    // A CONNECT client waits for this line before it sends anything, so its
    // opening bytes can only be read here. A forwarded request has already sent
    // everything it is going to send, and answering it with a tunnel response
    // would put `200 Connection Established` in front of the server's own reply.
    let opening = if tunnelled {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        match read_opening(&host, &mut client)? {
            Some(opening) => opening,
            None => return Ok(()),
        }
    } else {
        opening
    };

    // Timeouts are the relay's business: it needs a deadline on the upstream
    // until the first byte arrives, and none afterwards.
    let copied = relay(Session {
        host: &host,
        client: &mut client,
        upstream,
        candidates: &stage_candidates,
        router,
        now,
        opening,
        // A forwarded request is one exchange: the client has sent its request
        // and is waiting, so a client that closes has given up and the upstream
        // should be told rather than left answering into a socket nobody reads.
        // A CONNECT may be half-closed by a client that has finished uploading
        // and is still waiting for the reply — the normal shape of an upload —
        // and ending the session there would cut the response off.
        client_ends_session: !tunnelled,
    });

    // The connection is over, so now there is something worth saying about it.
    // A session that opened and moved no bytes downstream is the one failure the
    // connect-time report cannot see: the TCP handshake completed, so the old
    // signal called it a success, but nothing ever came back. A middlebox that
    // blackholes a flow after accepting it looks exactly like this.
    //
    // A rejected certificate does *not* look like this — measured against a real
    // edge it is `up=469 down=3105`, because the server sends its certificate
    // before the client can refuse it. The kernel cannot see that rejection, and
    // should not pretend to.
    if let Some(addr) = upstream_addr {
        let outcome = match &copied {
            Ok(bytes) if bytes.downstream == 0 => Outcome::Silent,
            Ok(_) => Outcome::Healthy,
            Err(_) => Outcome::Silent,
        };
        if let Ok(mut router) = router.lock() {
            router.report_outcome(addr, outcome, Instant::now());
        }
        let (up, down) = copied
            .as_ref()
            .map(|bytes| (bytes.upstream, bytes.downstream))
            .unwrap_or((0, 0));
        if matches!(outcome, Outcome::Silent) {
            // Both counts, because they tell different stories: bytes up and none
            // down is a refusal, whereas nothing in either direction is a client
            // that changed its mind.
            log::info!("proxy: SESSION {host} -> {addr} up={up} down={down}");
            log::info!(
                "proxy: {host} connected to {addr} but returned no data (sent {up} bytes upstream)"
            );
        }
    }
    copied.map(|_| ())
}

/// Re-resolve the rules' dial names on a tick, in the background.
///
/// A failure to spawn is reported and otherwise ignored: the proxy still works
/// without it, it just loses the dial-name path, and refusing to serve at all
/// would be a worse trade.
fn spawn_dial_refresh(router: Arc<Mutex<Router>>, stop: Arc<AtomicBool>) {
    let spawned = thread::Builder::new()
        .name("watt-dial".to_string())
        .spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let resolved = {
                    let mut guard = match router.lock() {
                        Ok(guard) => guard,
                        Err(_) => return,
                    };
                    guard.resolve_dial_names(|name| {
                        (name, 0u16)
                            .to_socket_addrs()
                            .map(|addrs| addrs.map(|addr| addr.ip()).collect())
                            .unwrap_or_default()
                    })
                };
                if resolved > 0 {
                    log::info!("proxy: re-resolved dial names for {resolved} entries");
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
        log::warn!("proxy: dial-name refresh is not running: {err}");
    }
}

/// How many candidates get a certificate probe.
///
/// # Why this is sized to the dial, not to a guess
///
/// This used to be 8, on the reasoning that "the answer is almost always near
/// the front: the selector has already ranked them". Measured against the real
/// rule set, that reasoning was wrong for exactly the domain that mattered.
///
/// `github.com` carries thirty-nine addresses and the usable ones sit at
/// positions 9 onward:
///
/// ```text
/// [ 0] 51.142.105.107   [ 5] 20.87.245.0        [10] 20.205.243.166
/// [ 1] 20.218.253.22    [ 6] 20.203.176.211     [11] 20.27.177.113
/// [ 2] 20.12.240.255    [ 7] 20.113.161.247     [12] 20.200.245.247
/// [ 3] 20.203.59.95     [ 8] 20.248.137.48      [13] 20.207.73.82
/// [ 4] 20.201.28.151    [ 9] 20.175.192.147     ...
/// ```
///
/// With a limit of 8 the probe never examined a single address that works, so
/// `covered` was empty, so no head start was ever given and the dial fell back
/// to a plain race — which is the original bug, wearing a disguise. It showed up
/// as intermittent failure: whichever dead address happened to answer first won.
///
/// `the selector has already ranked them` is true but irrelevant here. The
/// selector ranks by *liveness*, and an address that refuses every connection is
/// trivially fast to mark down. Only the certificate check can tell a fast wrong
/// address from a slow right one, so it has to look at least as far down the
/// list as the dial will.
const PROBE_LIMIT: usize = MAX_CANDIDATES;

/// How long one probe may take before it is treated as "could not tell".
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Keep the addresses whose certificate covers `host`, best first.
///
/// Returns the reordered list and how many leading entries were confirmed good,
/// so the dial can give those an exclusive head start rather than merely listing
/// them first. Counting matters as much as ordering: `connect_first` is a race,
/// and a race does not respect a list.
///
/// # Why this is not optional
///
/// Without it the address is chosen by an RTT race, and whether a domain works
/// is luck. Measured on `github.com` against the same rule set: repeated
/// requests without this gave a TLS failure every time — the fastest of the
/// rule's thirty-nine addresses was consistently an edge serving someone else's
/// certificate, and the client rejected it on a connection that would otherwise
/// have worked.
///
/// # The rule about shortening
///
/// The list is only shortened when something in it has been **confirmed good**.
/// A verdict of "bad" on its own is not enough: a probe can fail for reasons
/// that say nothing about the address — a transient fault, a server that will
/// not speak TLS 1.2 to a probe — and acting on those alone would leave one
/// candidate where there were thirty-nine.
///
/// Returns the input unchanged when nothing could be confirmed, so the ordinary
/// race and its failure reporting still apply.
fn prefer_covered(targets: Vec<SocketAddr>, host: &str) -> (Vec<SocketAddr>, usize) {
    if targets.len() <= 1 || host.is_empty() {
        return (targets, 0);
    }

    let probed: Vec<SocketAddr> = targets.iter().copied().take(PROBE_LIMIT).collect();
    let verdicts = probe_all(&probed, host);

    let mut covered = Vec::with_capacity(targets.len());
    let mut unknown = Vec::with_capacity(targets.len());
    let mut rejected = Vec::with_capacity(targets.len());
    for target in &targets {
        match verdicts.iter().find(|(address, _)| address == target) {
            Some((_, Some(true))) => covered.push(*target),
            // Kept aside rather than discarded here: a rejection is only
            // meaningful next to a confirmation. See below.
            Some((_, Some(false))) => rejected.push(*target),
            _ => unknown.push(*target),
        }
    }

    // A verdict of "bad" is only useful when something else was confirmed good.
    //
    // This is the rule the whole function turns on, and getting it wrong is not
    // obvious. A probe can fail for reasons that say nothing about the address —
    // a transient fault, a server that will not speak TLS 1.2 to a probe, a
    // network that blocks probes but not real connections. Acting on those
    // alone, when *every* address was rejected, leaves the caller with an empty
    // candidate list and turns "one address is stale" into "this domain cannot
    // be reached at all" — which is strictly worse than not probing.
    //
    // Measured: this is exactly what happened to `raw.githubusercontent.com`,
    // which went from a working 301 to a certificate failure.
    if covered.is_empty() {
        // Nothing to prefer. Return everything, in the original order, so the
        // ordinary race and its failure reporting still apply.
        let mut all = unknown;
        all.extend(rejected);
        all.sort_by_key(|target| {
            // Preserve the caller's order: it came in ranked by the selector.
            targets.iter().position(|t| t == target).unwrap_or(usize::MAX)
        });
        return (all, 0);
    }

    // Confirmed-good first. Dropping the rejected ones is not enough on its own,
    // because the race gives the earliest candidate a head start.
    let verified = covered.len();
    log::info!(
        "proxy: {host} {verified} of {} address(es) confirmed to cover it; \
         {} rejected; those go first",
        targets.len(),
        rejected.len()
    );
    covered.extend(unknown);
    (covered, verified)
}

/// Probe several addresses at once. `None` means "could not confirm".
fn probe_all(targets: &[SocketAddr], host: &str) -> Vec<(SocketAddr, Option<bool>)> {
    let (sender, receiver) = mpsc::channel();
    let mut spawned = 0usize;

    for target in targets {
        let target = *target;
        let host = host.to_string();
        let sender = sender.clone();
        let worker = thread::Builder::new()
            .name("watt-probe".to_string())
            .spawn(move || {
                let outcome = watt_net::probe::check(target, &host);
                let verdict = match outcome {
                    watt_net::probe::Probe::Covers => Some(true),
                    // A dead address is as unusable as one with the wrong
                    // certificate: relaying to it fails either way, and the
                    // client only sees a connection that never completes.
                    watt_net::probe::Probe::DoesNotCover
                    | watt_net::probe::Probe::Unreachable => Some(false),
                    watt_net::probe::Probe::Inconclusive => None,
                };
                // Named per address, because the whole point of the check is
                // that the addresses differ: a rule whose thirty-nine entries
                // all fail needs to show *why* each one did.
                //
                // Debug level, not info: a domain with eight probed addresses
                // writes eight lines for one request, and the useful summary is
                // the one `prefer_covered` writes. Turn this on by raising the
                // logger's level when the answer is not obvious.
                log::debug!(
                    "proxy: probe {} for {host} -> {:?}",
                    target.ip(),
                    outcome
                );
                let _ = sender.send((target, verdict));
            });
        if worker.is_ok() {
            spawned += 1;
        }
    }

    let mut results = Vec::with_capacity(spawned);
    for _ in 0..spawned {
        match receiver.recv_timeout(PROBE_TIMEOUT + Duration::from_millis(500)) {
            Ok(result) => results.push(result),
            Err(_) => break,
        }
    }
    results
}

/// Dial candidates in order, stopping at the first that connects.
///
/// Every attempt is reported back to the router, success or failure. Without
/// that feedback each request pays the same dead-address timeout and a rule that
/// actually works keeps looking broken — the ranking never escapes its first
/// entry.
/// Try candidates with staggered starts, first success wins.
///
/// Two failure modes to avoid, and they pull in opposite directions:
///
/// * **Strictly sequential** does not survive the address counts a merged rule
///   set produces. `github.com` carries thirty-nine addresses; at one timeout
///   each, a client waits minutes before reaching a live one.
/// * **Racing all at once** throws away the rule author's ordering. The fastest
///   TCP handshake is not the most correct address — an edge that answers in 20ms
///   with the wrong certificate beats one that answers in 60ms with the right
///   one, and the client then fails on a connection that would have worked.
///
/// So each candidate is started `CONNECT_STAGGER` after the one before it. The
/// first candidate gets a head start and usually wins, preserving the author's
/// preference; a candidate that is merely slow rather than dead is overtaken
/// instead of blocking the request. The wait is bounded by the stagger, not by
/// the number of addresses.
///
/// Attempts that report before the winner are recorded, success or failure, so a
/// single request teaches the ranking. Attempts still in flight are abandoned and
/// say nothing — a cancelled attempt is not evidence that an address is dead.
///
/// # `verified_count`
///
/// The first `verified_count` entries have been confirmed by a certificate probe
/// to cover this host. They get an exclusive head start of [`VERIFIED_HEAD_START`]
/// before the rest are allowed to race, because a stagger alone loses to an
/// address that answers instantly: measured, a confirmed-good `github.com`
/// address was overtaken by an unverified one that completed its handshake two
/// hundred milliseconds sooner, and the request then died with `up=0 down=0`.
///
/// Unverified candidates are still dialled if the verified ones do not answer —
/// a verdict can be stale, and refusing outright would turn a rule whose one
/// covered address has gone dark into a total failure.
///
/// # `head_start`
///
/// How long the first unverified candidate is given alone. It exists to keep the
/// rule author's ordering meaningful, so it belongs to the rule-addresses stage
/// and to nothing else: a system-resolver answer is a set of addresses the
/// network already vouched for, with no author's preference in it, and there the
/// head start is a flat second and a bit added to every request for no gain.
/// The failover pass passes zero for the same reason its own comment gives —
/// the confirmed addresses already had their turn on the initial dial.
fn connect_first(
    targets: &[SocketAddr],
    router: &Mutex<Router>,
    now: Instant,
    verified_count: usize,
    head_start: Duration,
) -> Option<TcpStream> {
    let (sender, receiver) = mpsc::channel();
    let mut in_flight = 0usize;

    for (index, target) in targets.iter().take(MAX_CANDIDATES).enumerate() {
        let target = *target;
        let worker_sender = sender.clone();
        // Confirmed candidates start immediately and outrank the stagger; the
        // unverified ones wait behind the whole verified group.
        let delay = if index < verified_count {
            Duration::ZERO
        } else {
            head_start + CONNECT_STAGGER * (index - verified_count) as u32
        };
        let spawned = thread::Builder::new()
            .name("watt-connect".to_string())
            .spawn(move || {
                if !delay.is_zero() {
                    thread::sleep(delay);
                }
                let started = Instant::now();
                let outcome = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT);
                let _ = worker_sender.send((target, outcome, started.elapsed()));
            });
        if spawned.is_err() {
            // Out of threads. Try this one inline rather than dropping the
            // candidate silently — a slow request beats a missing address.
            let started = Instant::now();
            let outcome = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT);
            let _ = sender.send((target, outcome, started.elapsed()));
        }
        in_flight += 1;
    }
    // Dropped so that `recv` ends once every worker has reported.
    drop(sender);

    for _ in 0..in_flight {
        match receiver.recv() {
            Ok((target, Ok(stream), rtt)) => {
                if let Ok(mut router) = router.lock() {
                    router.report_success(target.ip(), rtt, now);
                }
                return Some(stream);
            }
            Ok((target, Err(_), _)) => {
                if let Ok(mut router) = router.lock() {
                    router.report_failure(target.ip(), now);
                }
            }
            Err(_) => break,
        }
    }
    None
}

/// Append `target` unless it is already present.
fn push_unique(targets: &mut Vec<SocketAddr>, target: SocketAddr) {
    if !targets.contains(&target) {
        targets.push(target);
    }
}

/// Addresses to try when the rule's own list did not connect.
///
/// The port comes from the rule when it named one: a rule that says "this domain
/// lives on port 8443" is describing the service, and the client's own port is
/// only what it happened to ask for. Splitting the two keeps the rule meaningful
/// without letting it override the caller.
fn fallback_targets(
    host: &str,
    client_port: u16,
    rule_port: Option<u16>,
) -> io::Result<Vec<SocketAddr>> {
    let port = rule_port.unwrap_or(client_port);
    let mut targets = Vec::new();
    for target in (host, port).to_socket_addrs()? {
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    Ok(targets)
}

fn response(client: &mut TcpStream, value: &str) -> io::Result<()> {
    client.write_all(format!("HTTP/1.1 {value}").as_bytes())
}

/// Where a plain `http://` request goes when the URI names no port.
const DEFAULT_HTTP_PORT: u16 = 80;

/// Split `host[:port]` into its parts. The port is optional here.
///
/// Shared by both request shapes. A CONNECT authority always carries a port and
/// an absolute URI usually does not, but the bracketing and case rules for the
/// host are the same for both — and two copies of them is how `[::1]` ends up
/// working on one path and not the other.
fn parse_host_port(raw: &str) -> Option<(String, Option<u16>)> {
    if raw.is_empty() || raw.contains('/') || raw.contains('@') {
        // `@` is userinfo, which has nowhere to go once the authority is
        // stripped off the request line. Refused rather than dropped: a
        // credential silently discarded turns a 401 into a mystery.
        return None;
    }
    if let Some(rest) = raw.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        if host.is_empty() {
            return None;
        }
        let port = match rest {
            "" => None,
            rest => Some(rest.strip_prefix(':')?.parse().ok()?),
        };
        return Some((host.to_ascii_lowercase(), port));
    }
    match raw.rsplit_once(':') {
        Some((host, port)) => {
            if host.is_empty() {
                return None;
            }
            Some((host.to_ascii_lowercase(), Some(port.parse().ok()?)))
        }
        None => Some((raw.to_ascii_lowercase(), None)),
    }
}

/// A CONNECT authority. Unlike an absolute URI, the port is not optional.
fn parse_authority(raw: &str) -> Option<(String, u16)> {
    let (host, port) = parse_host_port(raw)?;
    Some((host, port?))
}

/// Split the part of an absolute URI after `http://` into `(host, port, path)`.
///
/// The path comes back in origin-form — with a leading slash — because that is
/// what the request line has to carry once the authority is gone. A URI whose
/// query has no path (`http://host?q`) still needs one, and `/` is what the
/// origin server would have assumed.
fn parse_absolute(rest: &str) -> Option<(String, u16, String)> {
    let (authority, tail) = match rest.find(|c| c == '/' || c == '?' || c == '#') {
        Some(index) => rest.split_at(index),
        None => (rest, ""),
    };
    let path = if tail.is_empty() {
        "/".to_string()
    } else if tail.starts_with('/') {
        tail.to_string()
    } else {
        format!("/{tail}")
    };
    let (host, port) = parse_host_port(authority)?;
    Some((host, port.unwrap_or(DEFAULT_HTTP_PORT), path))
}

/// Rewrite a forwarded request head into the origin-form the server expects.
///
/// Three edits, and each one is load-bearing:
///
/// * the request line loses the scheme and the authority. A server reading
///   `GET http://example.com/ HTTP/1.1` answers 400, because absolute-form is
///   addressed to a proxy and it is not one.
/// * `Proxy-Connection` and `Proxy-Authorization` are dropped. They are
///   addressed to us; forwarding them tells the server a proxy is in the path,
///   and the second hands it a credential it never asked for.
/// * `Connection` is replaced with `close`. This relay copies bytes and does
///   not parse responses, so a second request on the same client connection
///   would arrive in absolute-form and be forwarded verbatim to a server that
///   cannot read it. One request per connection is the only shape this relay
///   can keep correct — and saying so in the request is what makes the upstream
///   close at the right moment instead of leaving the client waiting.
///
/// Not done, and worth knowing: `Connection` may itself name further hop-by-hop
/// headers to strip, and this does not follow that list. The two that matter in
/// practice are the `Proxy-` headers above.
fn rewrite_head(raw: &[u8], method: &str, path: &str, version: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    out.extend_from_slice(method.as_bytes());
    out.push(b' ');
    out.extend_from_slice(path.as_bytes());
    out.push(b' ');
    out.extend_from_slice(version.as_bytes());
    out.extend_from_slice(b"\r\n");

    for line in raw.split(|byte| *byte == b'\n').skip(1) {
        let line = match line.split_last() {
            Some((&b'\r', rest)) => rest,
            _ => line,
        };
        // The blank line that ended the head, and any stray blank line before
        // it. Neither is a header, so neither is forwarded.
        if line.is_empty() {
            continue;
        }
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            // Not a header at all. Dropped rather than passed through: a server
            // reading a malformed line either rejects the whole request or
            // misreads it, and neither is better than losing a line that was
            // already invalid.
            continue;
        };
        if is_hop_by_hop(&line[..colon]) {
            continue;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out
}

/// Headers that describe this hop, which must not be forwarded to the next one.
fn is_hop_by_hop(name: &[u8]) -> bool {
    const DROPPED: [&[u8]; 3] = [b"proxy-connection", b"proxy-authorization", b"connection"];
    DROPPED
        .iter()
        .any(|dropped| name.eq_ignore_ascii_case(*dropped))
}

/// Where a candidate address came from. Tried in order, weakest last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// The rule's addresses: literals, plus whatever its dial names resolved to
    /// on the last refresh tick. The two are one stage because by the time a plan
    /// exists they are the same thing — addresses the rule vouched for.
    Literal,
    /// The client's own name, through the system resolver.
    System,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Stage::Literal => "rule-addresses",
            Stage::System => "system-resolution",
        }
    }
}

/// How much moved in each direction during one relayed session.
#[derive(Debug, Clone, Copy, Default)]
struct SessionBytes {
    /// Bytes the client sent upstream.
    upstream: u64,
    /// Bytes the upstream sent back. Zero here is the interesting case.
    downstream: u64,
}

/// One relayed session: the client, the upstream already chosen for it, and
/// everything the relay needs in order to change its mind about that upstream.
///
/// A struct rather than a longer parameter list because the pieces only mean
/// anything together, and because the last two fields are the ones that tell
/// the two request shapes apart — which is the part that is easy to get wrong.
struct Session<'a> {
    host: &'a str,
    client: &'a mut TcpStream,
    upstream: TcpStream,
    candidates: &'a [SocketAddr],
    router: &'a Mutex<Router>,
    now: Instant,
    /// Bytes the client has already sent that belong to this session: the
    /// rewritten head of a forwarded request plus whatever followed it, or the
    /// bytes a CONNECT client sent once its tunnel was open.
    opening: Vec<u8>,
    /// Whether a client that stops sending ends the session.
    ///
    /// True for a forwarded request — the client sent one request and is waiting
    /// for one reply, so a client that closes has given up. False for a CONNECT,
    /// where a half-closed client is one that has finished uploading and is
    /// still waiting for the answer.
    client_ends_session: bool,
}

/// Relay, changing upstream if the chosen one never answers.
///
/// The choice of upstream cannot be verified in advance: nothing is known about
/// it until the session's first bytes reach it. An address that accepts the
/// connection and then says nothing — the normal shape of SNI blocking, and the
/// reason `Outcome::Silent` exists — would otherwise leave the client waiting
/// until its own timeout. Measured against the merged rule set, that is an
/// eighty-second hang on `github.com`, which is not a product.
///
/// The way out is that **no upstream byte has reached the client yet**. Until the
/// first byte comes back the session is still ours to change: the silent upstream
/// can be dropped and the session's opening bytes replayed against the next
/// candidate, and the client sees a slow start rather than a failure. Once a byte
/// has been forwarded the client's protocol is bound to that stream and the
/// choice is final, so the handover to the ordinary relay happens there.
///
/// A silent attempt is reported as [`Outcome::Silent`], so the address sinks in
/// the ranking and the next request does not repeat the discovery.
fn relay(session: Session<'_>) -> io::Result<SessionBytes> {
    let Session {
        host,
        client,
        mut upstream,
        candidates,
        router,
        now,
        opening,
        client_ends_session,
    } = session;
    let opening_len = opening.len();

    // Every candidate except the one already in hand, in preference order, and
    // capped like the initial attempt: a domain can carry hundreds of addresses,
    // and walking them one at a time would turn a failover into a hang. The cap
    // is not a loss — addresses that go silent are reported, so they sink in the
    // ranking and the next request starts past them.
    let in_hand = upstream.peer_addr().ok();
    let alternatives: Vec<SocketAddr> = candidates
        .iter()
        .copied()
        .filter(|candidate| Some(*candidate) != in_hand)
        .take(MAX_CANDIDATES)
        .collect();

    let mut remaining = alternatives.as_slice();
    let mut rounds = 0usize;
    loop {
        let replied = match upstream.write_all(&opening) {
            Ok(()) => {
                upstream.set_read_timeout(Some(FIRST_BYTE_DEADLINE))?;
                let mut reply = vec![0u8; COPY_BUFFER];
                match upstream.read(&mut reply) {
                    Ok(0) => None,
                    Ok(len) => Some(reply[..len].to_vec()),
                    Err(_) => None,
                }
            }
            Err(_) => None,
        };

        if let Some(reply) = replied {
            // Alive. Hand over to the ordinary relay for the rest of the session.
            upstream.set_read_timeout(None)?;
            client.set_read_timeout(None)?;
            client.write_all(&reply)?;
            let mut bytes = copy_bidirectional(client, &mut upstream, client_ends_session)?;
            bytes.upstream += opening_len as u64;
            bytes.downstream += reply.len() as u64;
            return Ok(bytes);
        }

        // It connected and then said nothing. That is a reachable address that
        // does not serve, which is exactly what `Silent` means — not a failed
        // connect, so it does not enter the failure cooldown.
        if let Ok(addr) = upstream.peer_addr() {
            if let Ok(mut router) = router.lock() {
                router.report_outcome(addr.ip(), Outcome::Silent, now);
            }
            log::warn!("proxy: {host} {} connected but never replied; trying another", addr.ip());
        }

        // The next attempt is staggered like the first, not walked one at a time.
        // Connecting to them in sequence would make each failover cost a full
        // timeout, and a domain with forty-three addresses measured eighteen
        // seconds that way.
        rounds += 1;
        if remaining.is_empty() || rounds > MAX_FAILOVERS {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "every candidate connected without replying",
            ));
        }
        // Zero verified and no head start: the confirmed addresses were already
        // given their exclusive turn by the initial dial, and the ones reached
        // here are the remainder. Re-applying either would only slow the
        // failover.
        let Some(next) = connect_first(remaining, router, now, 0, Duration::ZERO) else {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "every candidate connected without replying",
            ));
        };
        // Skip past whatever the stagger already tried, so the next round does
        // not repeat this one.
        remaining = &remaining[remaining.len().min(MAX_CANDIDATES)..];
        upstream = next;
    }
}

/// Copy in both directions for the rest of the session.
///
/// The upstream is watched on this thread rather than the client, because the
/// upstream closing is the end of the reply while a quiet client is just a
/// client with nothing to say.
///
/// # `client_ends_session`
///
/// When the client stops sending, the upstream is told so, and what "told so"
/// means depends on the shape of the session. A forwarded request is one
/// exchange: a client that closes has given up, so the upstream's read side is
/// closed too and a server still waiting for a body stops waiting instead of
/// answering into a socket nobody will read. A tunnel is not one exchange — an
/// upload over a CONNECT ends with exactly that half-close, and the client is
/// still waiting for the reply — so there the upstream is left alone.
///
/// The client's write side is closed in both cases. The upstream has already
/// stopped sending, which is what ended the loop, so there is nothing left for
/// the client to read; a FIN is how it learns that instead of waiting for a
/// close that never comes. That FIN is also what lets the forward thread
/// finish: without it the client sits there, the copy thread sits in `join`, and
/// the session outlives the connection it was relaying.
fn copy_bidirectional(
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    client_ends_session: bool,
) -> io::Result<SessionBytes> {
    let mut client_reader = client.try_clone()?;
    let mut upstream_writer = upstream.try_clone()?;
    // The forward direction runs in its own thread because a half-open
    // connection must still be pumped; `io::copy` already counts the bytes.
    let forward = thread::spawn(move || io::copy(&mut client_reader, &mut upstream_writer));

    let mut buffer = vec![0u8; COPY_BUFFER];
    let mut downstream = 0u64;
    let mut failure = None;
    loop {
        match upstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                downstream += n as u64;
                if let Err(err) = client.write_all(&buffer[..n]) {
                    failure = Some(err);
                    break;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                failure = Some(err);
                break;
            }
        }
    }

    let _ = client.shutdown(Shutdown::Write);
    if client_ends_session {
        let _ = upstream.shutdown(Shutdown::Read);
    }
    // The forward thread is allowed to fail on its own; a client that hangs up
    // mid-upload is routine and must not turn into an error for the caller. Its
    // byte count is still worth keeping.
    let upstream_bytes = forward.join().ok().and_then(Result::ok).unwrap_or(0);

    match failure {
        Some(err) => Err(err),
        None => Ok(SessionBytes {
            upstream: upstream_bytes,
            downstream,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_authority() {
        assert_eq!(parse_authority("Example.COM:443"), Some(("example.com".into(), 443)));
    }

    #[test]
    fn parses_bracketed_ipv6_authority() {
        assert_eq!(parse_authority("[2001:db8::1]:8443"), Some(("2001:db8::1".into(), 8443)));
    }

    #[test]
    fn rejects_non_authorities() {
        assert!(parse_authority("https://example.com:443").is_none());
        assert!(parse_authority("example.com").is_none());
        assert!(parse_authority("user@example.com:443").is_none());
    }

    #[test]
    fn a_rule_port_overrides_the_requested_one() {
        // A rule naming port 8443 describes where the service lives, so the
        // fallback has to aim there rather than at whatever the client asked for.
        assert_eq!(
            fallback_targets("localhost", 1234, Some(8443)).unwrap(),
            vec![SocketAddr::from(([127, 0, 0, 1], 8443))]
        );
    }

    #[test]
    fn without_a_rule_port_the_client_port_is_used() {
        assert_eq!(
            fallback_targets("localhost", 443, None).unwrap(),
            vec![SocketAddr::from(([127, 0, 0, 1], 443))]
        );
    }

    #[test]
    fn an_unresolvable_host_yields_no_fallback() {
        // The caller turns this into a 502 rather than a panic; a bogus name
        // must not take the worker thread down with it.
        assert!(fallback_targets("watt.invalid", 443, None).is_err());
    }

    #[test]
    fn a_single_candidate_is_never_probed_or_reordered() {
        // A domain narrowed to one address. `/1` (measured 2026-09-26) gives
        // every domain more than one address, so this happens only after the
        // others were rejected — but when it does, there is nothing to rank, and
        // probing it would charge every request for an answer that cannot change
        // the outcome.
        let only = SocketAddr::from(([203, 0, 113, 7], 443));
        let (ordered, verified) = prefer_covered(vec![only], "example.com");
        assert_eq!(ordered, vec![only]);
        assert_eq!(verified, 0, "nothing was confirmed, so nothing gets a head start");
    }

    #[test]
    fn an_empty_host_is_not_probed() {
        let targets = vec![
            SocketAddr::from(([203, 0, 113, 1], 443)),
            SocketAddr::from(([203, 0, 113, 2], 443)),
        ];
        let (ordered, verified) = prefer_covered(targets.clone(), "");
        assert_eq!(ordered, targets);
        assert_eq!(verified, 0);
    }

    #[test]
    fn a_list_where_every_candidate_was_rejected_is_returned_whole() {
        // The failure this guards against. Nothing is listening on these
        // addresses, so every probe concludes unreachable and every candidate is
        // rejected. Discarding the rejected ones leaves an empty list, and the
        // caller then has nothing to dial — a domain that was merely full of
        // stale addresses becomes a domain that cannot be reached at all.
        //
        // Measured on real traffic: `raw.githubusercontent.com` went from a
        // working 301 to a certificate failure this way.
        let targets = vec![
            SocketAddr::from(([127, 0, 0, 1], 1)),
            SocketAddr::from(([127, 0, 0, 1], 2)),
        ];
        let (ordered, verified) = prefer_covered(targets.clone(), "example.com");
        assert_eq!(verified, 0, "no probe can succeed against a closed port");
        assert_eq!(
            ordered, targets,
            "with nothing confirmed, the list must come back whole and in order"
        );
    }

    /// The smallest rule document the compiler accepts, naming a host none of
    /// these tests touch. A ruleset must own at least one entry to be valid, so
    /// every fixture here carries this one.
    fn one_unrelated_rule() -> watt_rules::RuleSet {
        watt_rules::RuleSet::from_str(
            r#"{"version":"test","groups":[{"group":"g","entries":[
                {"id":"1","name":"unrelated","domains":["unrelated.example"],
                 "ips":["203.0.113.10"],"port":"443","isPlaceholder":false}
            ]}]}"#,
            watt_rules::RuleSource::Provided,
        )
        .unwrap()
    }

    /// A listener on a free loopback port, plus the address it landed on.
    fn loopback() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        (listener, address)
    }

    /// Start a proxy on a free loopback port, and hand back its address plus the
    /// stop flag the test uses to shut it down.
    fn start_proxy() -> (SocketAddr, Arc<AtomicBool>, thread::JoinHandle<io::Result<()>>) {
        let (listener, address) = loopback();
        let router = Arc::new(Mutex::new(Router::new(one_unrelated_rule())));
        let stop = Arc::new(AtomicBool::new(false));
        let serving = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || serve_until(listener, router, stop))
        };
        (address, stop, serving)
    }

    /// Read a head off a socket, one byte at a time, stopping at its blank line.
    ///
    /// Byte at a time on purpose. A buffered read would swallow the first bytes
    /// of whatever follows the head, which is precisely the bug the forwarding
    /// path has to avoid — and a test that reads ahead cannot see it.
    fn read_head_from(stream: &mut TcpStream) -> Vec<u8> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => head.push(byte[0]),
                Err(err) => panic!("reading a head: {err}"),
            }
        }
        head
    }

    #[test]
    fn a_head_stops_at_its_blank_line_and_keeps_what_follows() {
        // The bug this guards is invisible without a socket: reading a head with
        // a `BufReader` reads ahead by design, and the bytes it read ahead are
        // the start of the body. The CONNECT path never noticed, because a
        // CONNECT client waits for the `200` before sending anything — a `POST`
        // sends its body with its head.
        let (listener, address) = loopback();
        let writer = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .write_all(b"POST /x HTTP/1.1\r\nHost: h\r\n\r\nBODY")
                .unwrap();
            // Held open until the assertions have run, so a close cannot be
            // mistaken for a short head.
            stream
        });

        let (mut stream, _) = listener.accept().unwrap();
        let head = read_head(&mut stream).unwrap().unwrap();
        assert_eq!(head.raw, b"POST /x HTTP/1.1\r\nHost: h\r\n\r\n");
        assert_eq!(head.leftover, b"BODY");

        drop(writer.join().unwrap());
    }

    #[test]
    fn a_forwarded_request_reaches_the_origin_server_in_origin_form() {
        // The whole forward path, end to end, and the reason it exists: the
        // client speaks absolute-form because it was told to use a proxy, and
        // the origin server must receive origin-form because it was told
        // nothing. The two are not interchangeable — a server answers 400 to
        // `GET http://…`.
        //
        // The host is `127.0.0.1` and no rule mentions it, so this also pins the
        // policy change: an unlisted domain is served, not refused. It used to
        // be a 403, and the assertion on the reply is what would fail.
        let (origin, origin_address) = loopback();
        let seen = thread::spawn(move || {
            let (mut stream, _) = origin.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let head = read_head_from(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            head
        });

        let (proxy_address, stop, serving) = start_proxy();
        let mut client = TcpStream::connect(proxy_address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

        let port = origin_address.port();
        let request = format!(
            "GET http://127.0.0.1:{port}/x?y=1 HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Proxy-Connection: keep-alive\r\n\r\n"
        );
        client.write_all(request.as_bytes()).unwrap();

        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        let reply = String::from_utf8_lossy(&reply).to_string();
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
        assert!(reply.ends_with("ok"), "{reply}");

        let head = String::from_utf8_lossy(&seen.join().unwrap()).to_string();
        assert!(head.starts_with("GET /x?y=1 HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Host: 127.0.0.1:"), "{head}");
        assert!(head.contains("Connection: close\r\n"), "{head}");
        assert!(
            !head.to_ascii_lowercase().contains("proxy-"),
            "a header addressed to the proxy must not reach the origin: {head}"
        );

        stop.store(true, Ordering::SeqCst);
        serving.join().unwrap().unwrap();
    }

    #[test]
    fn a_connect_tunnel_still_carries_bytes_it_never_inspects() {
        // The path this rewrite must not have broken. The client asks for a
        // tunnel, gets the `200`, and from then on both sides are opaque — which
        // is why nothing here resembles HTTP after the first line.
        let (origin, origin_address) = loopback();
        let echoed = thread::spawn(move || {
            let (mut stream, _) = origin.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut buffer = [0u8; 32];
            let read = stream.read(&mut buffer).unwrap();
            stream.write_all(&buffer[..read]).unwrap();
            buffer[..read].to_vec()
        });

        let (proxy_address, stop, serving) = start_proxy();
        let mut client = TcpStream::connect(proxy_address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

        let port = origin_address.port();
        client
            .write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes())
            .unwrap();

        let mut established = [0u8; 39];
        client.read_exact(&mut established).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&established),
            "HTTP/1.1 200 Connection Established\r\n\r\n"
        );

        client.write_all(b"opaque-bytes").unwrap();
        let mut echo = [0u8; 12];
        client.read_exact(&mut echo).unwrap();
        assert_eq!(&echo, b"opaque-bytes");
        assert_eq!(echoed.join().unwrap(), b"opaque-bytes");

        stop.store(true, Ordering::SeqCst);
        serving.join().unwrap().unwrap();
    }

    #[test]
    fn connecting_to_nothing_fails_rather_than_hanging() {
        // The point is the empty *candidate list*, not the rules.
        let router = Mutex::new(Router::new(one_unrelated_rule()));
        assert!(connect_first(&[], &router, Instant::now(), 0, VERIFIED_HEAD_START).is_none());
    }

    #[test]
    fn an_absolute_uri_splits_into_host_port_and_origin_form() {
        assert_eq!(
            parse_absolute("example.com/a/b?c=d"),
            Some(("example.com".into(), 80, "/a/b?c=d".into()))
        );
        assert_eq!(
            parse_absolute("example.com:8080/"),
            Some(("example.com".into(), 8080, "/".into()))
        );
    }

    #[test]
    fn an_absolute_uri_with_no_path_still_gets_one() {
        // A URI whose only trailing part is a query has no path, and origin-form
        // has nowhere to put a query that is not behind a slash. `/` is what the
        // origin server would have assumed.
        assert_eq!(
            parse_absolute("example.com"),
            Some(("example.com".into(), 80, "/".into()))
        );
        assert_eq!(
            parse_absolute("example.com?q=1"),
            Some(("example.com".into(), 80, "/?q=1".into()))
        );
    }

    #[test]
    fn a_bracketed_ipv6_absolute_uri_keeps_its_colons_off_the_port() {
        assert_eq!(
            parse_absolute("[2001:db8::1]:8080/x"),
            Some(("2001:db8::1".into(), 8080, "/x".into()))
        );
        // Without a port, the colons inside the brackets must not be read as one.
        assert_eq!(
            parse_absolute("[2001:db8::1]/x"),
            Some(("2001:db8::1".into(), 80, "/x".into()))
        );
    }

    #[test]
    fn userinfo_is_refused_rather_than_silently_dropped() {
        // `user:pass@host` has nowhere to go once the authority is stripped off
        // the request line, and a credential silently discarded turns a 401 into
        // a mystery.
        assert!(parse_absolute("user:pass@example.com/").is_none());
    }

    #[test]
    fn a_connect_authority_still_requires_a_port() {
        assert_eq!(
            parse_authority("example.com:443"),
            Some(("example.com".into(), 443))
        );
        assert!(parse_authority("example.com").is_none());
    }

    #[test]
    fn a_forwarded_head_loses_the_absolute_uri_and_the_proxy_headers() {
        let raw = b"GET http://example.com/a HTTP/1.1\r\n\
                    Host: example.com\r\n\
                    Proxy-Connection: keep-alive\r\n\
                    Proxy-Authorization: Basic Zm9v\r\n\
                    User-Agent: probe\r\n\r\n";
        let text = String::from_utf8(rewrite_head(raw, "GET", "/a", "HTTP/1.1")).unwrap();
        assert!(text.starts_with("GET /a HTTP/1.1\r\n"), "{text}");
        assert!(text.contains("Host: example.com\r\n"), "{text}");
        assert!(text.contains("User-Agent: probe\r\n"), "{text}");
        assert!(!text.to_ascii_lowercase().contains("proxy-"), "{text}");
        assert!(text.ends_with("Connection: close\r\n\r\n"), "{text}");
    }

    #[test]
    fn a_forwarded_head_replaces_the_clients_own_connection_header() {
        // The client's `Connection: keep-alive` would leave the upstream holding
        // the socket open after one reply, which this relay cannot use: a second
        // request on the same client connection would arrive in absolute-form and
        // be forwarded verbatim to a server that cannot read it.
        let raw = b"GET http://example.com/ HTTP/1.1\r\nConnection: keep-alive\r\n\r\n";
        let text = String::from_utf8(rewrite_head(raw, "GET", "/", "HTTP/1.1")).unwrap();
        assert_eq!(text.matches("Connection:").count(), 1, "{text}");
        assert!(text.contains("Connection: close\r\n"), "{text}");
        assert!(!text.contains("keep-alive"), "{text}");
    }

    #[test]
    fn hop_by_hop_headers_are_recognised_whatever_their_case() {
        assert!(is_hop_by_hop(b"Proxy-Connection"));
        assert!(is_hop_by_hop(b"proxy-authorization"));
        assert!(is_hop_by_hop(b"CONNECTION"));
        assert!(!is_hop_by_hop(b"Content-Length"));
    }

    #[test]
    fn a_head_ends_at_the_earlier_of_the_two_terminators() {
        assert_eq!(head_end(b"GET / HTTP/1.1\r\n\r\n"), Some(18));
        assert_eq!(head_end(b"GET / HTTP/1.1\n\n"), Some(16));
        assert_eq!(head_end(b"GET / HTTP/1.1\r\n"), None);
    }

    #[test]
    fn the_request_line_is_read_without_its_terminator() {
        assert_eq!(first_line(b"GET /x HTTP/1.1\r\nHost: h\r\n\r\n"), "GET /x HTTP/1.1");
        assert_eq!(first_line(b"GET /x HTTP/1.1\n\n"), "GET /x HTTP/1.1");
        // A head that is not text at all yields an empty line rather than an
        // error, so the caller can answer 400 instead of dropping the client.
        assert_eq!(first_line(b"\xff\xfe\r\n\r\n"), "");
    }
}

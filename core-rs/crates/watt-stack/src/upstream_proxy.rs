//! Asking an upstream proxy to carry a flow: SOCKS5 and HTTP CONNECT.
//!
//! # Why this exists
//!
//! Everything else in this kernel chooses **which address** a flow goes to. That
//! is the whole of what a rule can express, and it is measurably not enough. On
//! the network this was written against, the reset that kills a blocked domain
//! follows the **name** and not the address:
//!
//! ```text
//! four unrelated addresses — Akamai, Cloudflare, Microsoft, Akamai again —
//! one blocked SNI: reset on all four. Same four addresses, a name that is not
//! blocked: no reset. (memory/2026-10-02.md §5.1)
//! ```
//!
//! Address choice cannot fix that, and neither can rewriting the handshake: the
//! bytes that carry the name are the client's, and changing them means
//! terminating its TLS, which this kernel does not do and will not start doing.
//! The one lever left is the **path**: hand the bytes to a proxy that opens the
//! connection from somewhere else, and let it forward them untouched.
//!
//! # What this is not
//!
//! Not a tunnel, not a decryptor, and not a change to what the client sees. The
//! proxy is handed a destination and asked for a byte pipe; TLS still passes
//! through end to end, no certificate is involved, and nothing here can read a
//! session. That is the same posture as the rest of the kernel — see
//! `memory/NET-NOTES.md`, "架构边界：不解密 TLS".
//!
//! # Why both protocols
//!
//! They are the two every proxy on the planet speaks, and they cost about the
//! same to implement: SOCKS5 is a binary handshake in three round trips, HTTP
//! CONNECT is one request line and a status code. Implementing one and not the
//! other would rule out half the endpoints a user might have.
//!
//! # The one invariant that matters
//!
//! **A handshake must never read past the end of its own reply.** Everything a
//! proxy sends after the reply is tunnel data on its way to the client, and a
//! handshake that buffered it would silently swallow the first bytes of the
//! session it had just opened — a corruption that would show up as "the proxy
//! works for HTTP but breaks TLS", which is exactly the kind of bug that costs
//! days. So every read below asks for **at most** the number of bytes the reply
//! still needs, and the reader that does not know the length in advance (HTTP)
//! reads one byte at a time. The cost is a few dozen syscalls on a path that runs
//! once per flow; the benefit is that over-reading is impossible rather than
//! unlikely.

use std::net::SocketAddr;

use crate::upstream::{is_retryable, UpstreamSocket};

/// Largest proxy reply that will be buffered before the handshake is refused.
///
/// A SOCKS5 reply is at most 22 bytes and an HTTP CONNECT response is a status
/// line and a few headers, so this is a sanity bound rather than a policy: a peer
/// that sends more than this is not answering the question that was asked, and an
/// unbounded read is how a hostile endpoint turns a handshake into an allocation.
const MAX_REPLY: usize = 8 * 1024;

/// Which handshake to speak to the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    /// RFC 1928, optionally with RFC 1929 username/password authentication.
    Socks5,
    /// RFC 9110 §9.3.6, optionally with `Proxy-Authorization: Basic`.
    HttpConnect,
}

impl ProxyKind {
    /// Read a kind from a configuration string.
    ///
    /// Returns `None` for anything else rather than falling back to a default.
    /// A shell that misspells the protocol must not silently get SOCKS5: the
    /// user would have a proxy configured, a switch that says it is on, and no
    /// way to tell that the request is going out in a language the other end
    /// does not speak.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "socks5" | "socks" => Some(Self::Socks5),
            "http" | "connect" | "http-connect" => Some(Self::HttpConnect),
            _ => None,
        }
    }

    /// The name this kind is written as in logs and in the control plane.
    pub fn name(self) -> &'static str {
        match self {
            Self::Socks5 => "socks5",
            Self::HttpConnect => "http-connect",
        }
    }
}

/// Where the upstream exit is, and how to ask it for a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    /// The proxy itself. This is what the kernel's socket connects to, which is
    /// why it is the address that has to be reachable — not the destination.
    pub address: SocketAddr,
    /// Username for `Proxy-Authorization` / SOCKS5 method `0x02`.
    ///
    /// `None` means "do not offer authentication at all" for HTTP, and for
    /// SOCKS5 "do not offer method `0x02`" — which is different from offering it
    /// and sending an empty password. A proxy that requires authentication and
    /// is not given one is refused with a reason rather than retried.
    pub username: Option<String>,
    /// Password, paired with `username`.
    pub password: Option<String>,
}

impl ProxyConfig {
    /// The credentials, when both halves are present and non-empty.
    ///
    /// A half-configured pair is treated as no credentials: a username with no
    /// password is far more likely to be a shell bug than an account with an
    /// empty password, and sending `user:` to a proxy produces a
    /// `407`/`0x02`-then-reject that reads like a wrong password.
    pub fn credentials(&self) -> Option<(&str, &str)> {
        let user = self.username.as_deref().filter(|s| !s.is_empty())?;
        let pass = self.password.as_deref().filter(|s| !s.is_empty())?;
        Some((user, pass))
    }
}

/// How far a handshake has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// More bytes to write or read; call again on a later tick.
    Pending,
    /// The proxy accepted. The socket now carries the destination's bytes.
    Done,
    /// The proxy answered and refused, or answered something unparsable. The
    /// string is the reason, phrased for a log line.
    Refused(&'static str),
}

/// Which reply the reader is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// SOCKS5: the proxy's choice of authentication method.
    Method,
    /// SOCKS5: the verdict on the username and password.
    Auth,
    /// SOCKS5: the verdict on the connect request.
    Connect,
    /// HTTP: the status line and headers.
    Head,
}

/// A handshake in progress, driven one tick at a time.
///
/// Holds its own request buffer rather than borrowing the configuration, so the
/// relay can build one per dial while the flow map is mutably borrowed. A
/// handshake is created the moment a socket's TCP connect is confirmed and is
/// dropped the moment it finishes, so it never outlives the dial it belongs to.
#[derive(Debug)]
pub struct Handshake {
    kind: ProxyKind,
    /// The destination the client asked for. This is what goes in the request.
    target: SocketAddr,
    username: Option<String>,
    password: Option<String>,
    phase: Phase,
    /// The request for the current phase, and how much of it has gone out.
    out: Vec<u8>,
    sent: usize,
    /// Bytes of the current reply that have arrived.
    received: Vec<u8>,
}

impl Handshake {
    /// Build the opening request for `target` through `config`.
    pub fn new(config: &ProxyConfig, target: SocketAddr) -> Self {
        let (username, password) = match config.credentials() {
            Some((user, pass)) => (Some(user.to_string()), Some(pass.to_string())),
            None => (None, None),
        };
        let mut handshake = Self {
            kind: config.kind,
            target,
            username,
            password,
            phase: Phase::Method,
            out: Vec::new(),
            sent: 0,
            received: Vec::new(),
        };
        match config.kind {
            ProxyKind::Socks5 => handshake.queue_greeting(),
            ProxyKind::HttpConnect => handshake.queue_connect_request(),
        }
        handshake
    }

    /// The destination this handshake is asking for.
    pub fn target(&self) -> SocketAddr {
        self.target
    }

    /// How far along, for a log line that has to explain a stall.
    pub fn phase(&self) -> &'static str {
        match (self.kind, self.phase) {
            (_, Phase::Method) => "method selection",
            (_, Phase::Auth) => "authentication",
            (_, Phase::Connect) => "connect request",
            (_, Phase::Head) => "response header",
        }
    }

    /// Advance the handshake as far as the socket allows.
    ///
    /// Never blocks: a socket that has nothing to give yet produces `Pending`.
    /// Loops internally so a proxy that answers within the same tick (loopback
    /// in a test, a fast endpoint in practice) finishes in one call instead of
    /// one round trip per tick.
    pub fn advance(&mut self, socket: &UpstreamSocket) -> Step {
        loop {
            if let Err(step) = self.write_request(socket) {
                return step;
            }

            loop {
                match self.want() {
                    Want::Complete => break,
                    Want::Bad(reason) => return Step::Refused(reason),
                    Want::More(missing) => match self.read_reply(socket, missing) {
                        Ok(true) => {}
                        Ok(false) => return Step::Pending,
                        Err(step) => return step,
                    },
                }
            }

            // The reply for this phase is in. Either the handshake is over or
            // the next request is queued and the loop goes round again.
            match self.interpret() {
                Step::Pending => continue,
                other => return other,
            }
        }
    }

    /// Write the queued request out in full, or say why it could not be.
    ///
    /// `Ok(())` means every byte is in the socket. A short write is not a
    /// failure: the socket is non-blocking, so `Err(Step::Pending)` means "come
    /// back on a later tick", which is why the count is kept rather than the
    /// buffer being retried whole.
    fn write_request(&mut self, socket: &UpstreamSocket) -> Result<(), Step> {
        while self.sent < self.out.len() {
            match socket.write(&self.out[self.sent..]) {
                Ok(0) => return Err(Step::Pending),
                Ok(n) => self.sent += n,
                Err(err) if is_retryable(&err) => return Err(Step::Pending),
                Err(_) => {
                    return Err(Step::Refused(
                        "the proxy connection failed while sending the request",
                    ))
                }
            }
        }
        Ok(())
    }

    /// Read at most `missing` bytes of the reply.
    ///
    /// `Ok(false)` means "nothing to read yet". `Ok(true)` means the buffer grew.
    /// The bound is the whole point: asking for at most what the reply still
    /// needs is what makes reading a byte of tunnel data impossible.
    fn read_reply(&mut self, socket: &UpstreamSocket, missing: usize) -> Result<bool, Step> {
        // A small stack buffer, capped so a single read never asks for more than
        // the reply needs nor for more than fits.
        let mut buf = [0u8; 128];
        let take = missing.min(buf.len());
        match socket.read(&mut buf[..take]) {
            Ok(0) => Err(Step::Refused("the proxy closed the connection during the handshake")),
            Ok(n) => {
                self.received.extend_from_slice(&buf[..n]);
                Ok(true)
            }
            Err(err) if is_retryable(&err) => Ok(false),
            Err(_) => Err(Step::Refused("the proxy connection failed during the handshake")),
        }
    }

    /// How much more of the current reply is needed.
    fn want(&self) -> Want {
        let have = self.received.len();
        match self.kind {
            ProxyKind::Socks5 => match self.phase {
                Phase::Method | Phase::Auth => {
                    if have >= 2 {
                        Want::Complete
                    } else {
                        Want::More(2 - have)
                    }
                }
                Phase::Connect => {
                    if have < 4 {
                        return Want::More(4 - have);
                    }
                    let total = match self.received[3] {
                        // IPv4 bound address.
                        0x01 => 10,
                        // IPv6 bound address.
                        0x04 => 22,
                        // A domain the proxy resolved for itself. Allowed on the
                        // way back even though this client only ever sends
                        // address forms, because RFC 1928 lets a proxy answer
                        // with whichever form it likes.
                        0x03 => {
                            if have < 5 {
                                return Want::More(5 - have);
                            }
                            5 + self.received[4] as usize + 2
                        }
                        _ => return Want::Bad("the proxy replied with an unknown address type"),
                    };
                    if have >= total {
                        Want::Complete
                    } else {
                        Want::More(total - have)
                    }
                }
                // Unreachable: `want` is only consulted while a SOCKS5 request
                // is outstanding, and `queue_connect_request` sets `Connect`.
                Phase::Head => Want::Bad("a SOCKS5 handshake reached the HTTP reader"),
            },
            ProxyKind::HttpConnect => {
                if have >= MAX_REPLY {
                    return Want::Bad("the proxy's response header is larger than any real one");
                }
                if find_headers_end(&self.received).is_some() {
                    Want::Complete
                } else {
                    // One byte at a time: the length of an HTTP response is not
                    // knowable in advance, and reading more than the terminator
                    // would take bytes the client is owed.
                    Want::More(1)
                }
            }
        }
    }

    /// Act on a complete reply, queueing the next request when there is one.
    fn interpret(&mut self) -> Step {
        match self.kind {
            ProxyKind::Socks5 => self.interpret_socks5(),
            ProxyKind::HttpConnect => self.interpret_http(),
        }
    }

    fn interpret_socks5(&mut self) -> Step {
        match self.phase {
            Phase::Method => {
                if self.received[0] != 0x05 {
                    return Step::Refused("the proxy is not speaking SOCKS5");
                }
                match self.received[1] {
                    0x00 => {
                        self.queue_connect_request();
                        Step::Pending
                    }
                    0x02 => {
                        if self.username.is_none() || self.password.is_none() {
                            return Step::Refused(
                                "the proxy wants a username and password, and none is configured",
                            );
                        }
                        self.queue_auth();
                        Step::Pending
                    }
                    0xFF => Step::Refused("the proxy accepted none of the offered methods"),
                    _ => Step::Refused("the proxy chose a method that was not offered"),
                }
            }
            Phase::Auth => {
                if self.received[0] != 0x01 {
                    return Step::Refused("the proxy's authentication reply is malformed");
                }
                if self.received[1] != 0x00 {
                    return Step::Refused("the proxy rejected the username and password");
                }
                self.queue_connect_request();
                Step::Pending
            }
            Phase::Connect => {
                if self.received[0] != 0x05 {
                    return Step::Refused("the proxy's connect reply is malformed");
                }
                match self.received[1] {
                    0x00 => Step::Done,
                    code => Step::Refused(socks5_refusal(code)),
                }
            }
            Phase::Head => Step::Refused("a SOCKS5 handshake reached the HTTP reader"),
        }
    }

    fn interpret_http(&mut self) -> Step {
        let Some(end) = find_headers_end(&self.received) else {
            // `want` only reports `Complete` once the terminator is present.
            return Step::Refused("the proxy's response has no end of header");
        };
        match parse_status(&self.received[..end]) {
            Some(code) if (200..300).contains(&code) => Step::Done,
            Some(code) => Step::Refused(http_refusal(code)),
            None => Step::Refused("the proxy's response is not an HTTP status line"),
        }
    }

    /// `05 <n> <methods…>` — the opening SOCKS5 greeting.
    fn queue_greeting(&mut self) {
        // Both methods are offered when credentials exist, which is what every
        // other client does: a proxy that accepts unauthenticated connections
        // then picks `0x00` and the credentials are simply unused, and one that
        // requires authentication picks `0x02`. Offering only `0x02` would break
        // the first case, and only `0x00` the second.
        let methods: &[u8] = if self.username.is_some() && self.password.is_some() {
            &[0x00, 0x02]
        } else {
            &[0x00]
        };
        let mut out = Vec::with_capacity(2 + methods.len());
        out.push(0x05);
        out.push(methods.len() as u8);
        out.extend_from_slice(methods);
        self.queue(out, Phase::Method);
    }

    /// `01 <ulen> <user> <plen> <pass>` — RFC 1929.
    fn queue_auth(&mut self) {
        let user = self.username.as_deref().unwrap_or_default().as_bytes();
        let pass = self.password.as_deref().unwrap_or_default().as_bytes();
        // The lengths are single bytes in the protocol. Truncating rather than
        // refusing keeps this total; the proxy then rejects a truncated password
        // with a clear reason, which is a better outcome than a handshake that
        // panics on a long configuration string.
        let user = &user[..user.len().min(255)];
        let pass = &pass[..pass.len().min(255)];
        let mut out = Vec::with_capacity(3 + user.len() + pass.len());
        out.push(0x01);
        out.push(user.len() as u8);
        out.extend_from_slice(user);
        out.push(pass.len() as u8);
        out.extend_from_slice(pass);
        self.queue(out, Phase::Auth);
    }

    /// `05 01 00 <ATYP> <addr> <port>` — the connect request, or the HTTP
    /// equivalent when the proxy speaks HTTP.
    fn queue_connect_request(&mut self) {
        match self.kind {
            ProxyKind::Socks5 => {
                let mut out = Vec::with_capacity(22);
                out.extend_from_slice(&[0x05, 0x01, 0x00]);
                match self.target {
                    SocketAddr::V4(v4) => {
                        out.push(0x01);
                        out.extend_from_slice(&v4.ip().octets());
                    }
                    SocketAddr::V6(v6) => {
                        out.push(0x04);
                        out.extend_from_slice(&v6.ip().octets());
                    }
                }
                out.extend_from_slice(&self.target.port().to_be_bytes());
                self.queue(out, Phase::Connect);
            }
            ProxyKind::HttpConnect => {
                // The authority carries the port and, for IPv6, the brackets the
                // grammar requires; a bare `::1:443` would be read as a host
                // named `::1:443`.
                let authority = match self.target {
                    SocketAddr::V4(v4) => format!("{}:{}", v4.ip(), v4.port()),
                    SocketAddr::V6(v6) => format!("[{}]:{}", v6.ip(), v6.port()),
                };
                let mut out = Vec::new();
                out.extend_from_slice(format!("CONNECT {authority} HTTP/1.1\r\n").as_bytes());
                out.extend_from_slice(format!("Host: {authority}\r\n").as_bytes());
                if let (Some(user), Some(pass)) = (self.username.as_deref(), self.password.as_deref())
                {
                    out.extend_from_slice(b"Proxy-Authorization: Basic ");
                    out.extend_from_slice(base64(format!("{user}:{pass}").as_bytes()).as_bytes());
                    out.extend_from_slice(b"\r\n");
                }
                // A CONNECT request has no body, and the blank line is what tells
                // the proxy the request is over. Without it the proxy waits for
                // more headers and the handshake times out looking like a stall.
                out.extend_from_slice(b"\r\n");
                self.queue(out, Phase::Head);
            }
        }
    }

    /// Replace the outstanding request and move to the phase that answers it.
    fn queue(&mut self, out: Vec<u8>, phase: Phase) {
        self.out = out;
        self.sent = 0;
        self.received.clear();
        self.phase = phase;
    }
}

/// How much more of the current reply is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    /// The reply is complete.
    Complete,
    /// Exactly this many more bytes, and never more.
    More(usize),
    /// The reply cannot be right.
    Bad(&'static str),
}

/// The reason a SOCKS5 connect reply carries, as a log-ready phrase.
fn socks5_refusal(code: u8) -> &'static str {
    match code {
        0x01 => "the proxy reported a general failure",
        0x02 => "the proxy's rules do not allow that destination",
        0x03 => "the proxy reports the network is unreachable",
        0x04 => "the proxy reports the host is unreachable",
        0x05 => "the proxy reports the connection was refused",
        0x06 => "the proxy reports the TTL expired",
        0x07 => "the proxy does not support CONNECT",
        0x08 => "the proxy does not support that address type",
        _ => "the proxy refused the destination",
    }
}

/// The reason an HTTP status carries, as a log-ready phrase.
///
/// The number itself is folded into the phrase rather than formatted, because a
/// `&'static str` is what the caller logs and allocating a `String` on a failure
/// path that may run per flow is not worth the readability. `407` gets its own
/// wording: it is the one status that says the *configuration* is wrong rather
/// than the destination, and it is the one a user is most likely to meet.
fn http_refusal(code: u16) -> &'static str {
    match code {
        407 => "the proxy requires a username and password (407)",
        403 => "the proxy refused that destination (403)",
        404 => "the proxy did not recognise the connect target (404)",
        502 | 503 | 504 => "the proxy could not reach the destination (5xx)",
        c if (400..500).contains(&c) => "the proxy refused the request (4xx)",
        c if (500..600).contains(&c) => "the proxy failed to reach the destination (5xx)",
        _ => "the proxy answered with a status that is not success",
    }
}

/// Where the blank line that ends an HTTP header block starts.
///
/// Accepts a bare `\n\n` as well as `\r\n\r\n`. Strict CRLF is what the grammar
/// says, but a proxy that emits LF-only is answering a question that was asked
/// in earnest, and refusing it would be pedantry with a handshake as the cost.
fn find_headers_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| at + 4)
        .or_else(|| bytes.windows(2).position(|w| w == b"\n\n").map(|at| at + 2))
}

/// The three digit status of an HTTP response, if the block starts with one.
fn parse_status(head: &[u8]) -> Option<u16> {
    let line_end = head.iter().position(|&b| b == b'\n').unwrap_or(head.len());
    let line = &head[..line_end];
    // Split on any ASCII whitespace rather than a single space: the line as it
    // arrives still carries its CR, and `407\r` is not a three digit status.
    let mut fields = line
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());
    if !fields.next()?.starts_with(b"HTTP/") {
        return None;
    }
    let code = fields.next()?;
    if code.len() != 3 || !code.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        code.iter()
            .fold(0u16, |value, digit| value * 10 + u16::from(digit - b'0')),
    )
}

/// Standard base64, for `Proxy-Authorization: Basic`.
///
/// Written out rather than pulled in: it is twelve lines, it is used once, and a
/// dependency for it would be a dependency in the kernel's `Cargo.toml` for the
/// rest of the project's life.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = u32::from(chunk[0]);
        let b = u32::from(*chunk.get(1).unwrap_or(&0));
        let c = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (a << 16) | (b << 8) | c;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::NoProtector;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    /// A peer that answers one reply per request, and records what it was asked.
    ///
    /// One read per reply rather than "read exactly N bytes": the two protocols
    /// here are strict request/response, so a read that returns is a whole
    /// request, and the shape is the same for both. It also means the test peer
    /// cannot accidentally hide a bug in the client's framing — whatever the
    /// client sends is what is recorded, verbatim.
    struct Peer {
        address: SocketAddr,
        seen: Arc<Mutex<Vec<u8>>>,
        handle: Option<JoinHandle<()>>,
    }

    impl Peer {
        /// Accept one connection and answer each request with the next reply.
        fn start(replies: Vec<Vec<u8>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let recorder = Arc::clone(&seen);
            let handle = std::thread::spawn(move || {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                for reply in replies {
                    let mut buf = [0u8; 512];
                    let Ok(n) = stream.read(&mut buf) else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    recorder.lock().unwrap().extend_from_slice(&buf[..n]);
                    if stream.write_all(&reply).is_err() {
                        return;
                    }
                }
                // Hold the connection open so the client's post-handshake reads
                // see a live socket rather than an EOF.
                std::thread::sleep(Duration::from_millis(150));
            });
            Self {
                address,
                seen,
                handle: Some(handle),
            }
        }

        fn seen(&self) -> Vec<u8> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Drop for Peer {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// Connect a real upstream socket to `peer`, as the relay does before a dial.
    fn socket_to(address: SocketAddr) -> UpstreamSocket {
        let mut protector = NoProtector;
        let mut socket = UpstreamSocket::tcp(watt_rules::Family::V4, &mut protector).unwrap();
        socket.start_connect(address).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if socket.take_connect_error().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        socket
    }

    /// Run a handshake to completion, or fail the test.
    fn drive(handshake: &mut Handshake, socket: &UpstreamSocket) -> Step {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match handshake.advance(socket) {
                Step::Pending => {}
                other => return other,
            }
            assert!(Instant::now() < deadline, "the handshake never finished");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn socks5_config(address: SocketAddr) -> ProxyConfig {
        ProxyConfig {
            kind: ProxyKind::Socks5,
            address,
            username: None,
            password: None,
        }
    }

    fn http_config(address: SocketAddr) -> ProxyConfig {
        ProxyConfig {
            kind: ProxyKind::HttpConnect,
            address,
            username: None,
            password: None,
        }
    }

    fn target() -> SocketAddr {
        "203.0.113.10:443".parse().unwrap()
    }

    /// `05 00 00 01 <4 bytes> <2 bytes>` — a successful SOCKS5 connect reply.
    fn socks5_ok() -> Vec<u8> {
        vec![0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
    }

    #[test]
    fn a_socks5_handshake_without_credentials_completes() {
        let peer = Peer::start(vec![vec![0x05, 0x00], socks5_ok()]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&socks5_config(peer.address), target());

        assert_eq!(drive(&mut handshake, &socket), Step::Done);

        let seen = peer.seen();
        assert_eq!(
            &seen[..3],
            &[0x05, 0x01, 0x00],
            "the greeting must offer exactly the no-auth method"
        );
        let expected: Vec<u8> = vec![0x05, 0x01, 0x00, 0x01, 203, 0, 113, 10, 0x01, 0xbb];
        assert_eq!(
            &seen[3..],
            &expected[..],
            "the connect request must carry the IPv4 destination and its port"
        );
    }

    #[test]
    fn a_socks5_handshake_with_credentials_authenticates_first() {
        let mut config = socks5_config("127.0.0.1:1".parse().unwrap());
        config.username = Some("user".to_string());
        config.password = Some("pass".to_string());

        let peer = Peer::start(vec![vec![0x05, 0x02], vec![0x01, 0x00], socks5_ok()]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&config, target());

        assert_eq!(drive(&mut handshake, &socket), Step::Done);

        let seen = peer.seen();
        assert_eq!(
            &seen[..4],
            &[0x05, 0x02, 0x00, 0x02],
            "with credentials configured both methods are offered, so a proxy that \
             needs none still works"
        );
        assert_eq!(
            &seen[4..14],
            &[0x01, 4, b'u', b's', b'e', b'r', 4, b'p', b'a', b's'],
            "the RFC 1929 subnegotiation carries both halves"
        );
    }

    #[test]
    fn a_socks5_proxy_that_wants_credentials_and_has_none_is_refused() {
        // The alternative is a handshake that waits out its whole budget for a
        // reply that is never coming, which reads in the log as a slow proxy
        // rather than a missing setting.
        let peer = Peer::start(vec![vec![0x05, 0x02]]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&socks5_config(peer.address), target());

        let step = drive(&mut handshake, &socket);
        assert!(
            matches!(step, Step::Refused(reason) if reason.contains("username and password")),
            "expected a missing-credentials refusal, got {step:?}"
        );
    }

    #[test]
    fn a_socks5_refusal_code_becomes_a_reason() {
        // The reply code is the proxy telling you *why*, and it is the difference
        // between "this destination is unreachable from there" and "this proxy is
        // misconfigured" — the two call for opposite reactions.
        let peer = Peer::start(vec![vec![0x05, 0x00], vec![0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0]]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&socks5_config(peer.address), target());

        let step = drive(&mut handshake, &socket);
        assert!(
            matches!(step, Step::Refused(reason) if reason.contains("refused")),
            "expected the refusal code to reach the reason, got {step:?}"
        );
    }

    #[test]
    fn a_socks5_reply_that_is_not_socks5_is_refused() {
        let peer = Peer::start(vec![vec![0x04, 0x00]]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&socks5_config(peer.address), target());

        let step = drive(&mut handshake, &socket);
        assert!(
            matches!(step, Step::Refused(reason) if reason.contains("not speaking SOCKS5")),
            "expected a version refusal, got {step:?}"
        );
    }

    #[test]
    fn an_ipv6_target_is_sent_as_an_ipv6_address() {
        let peer = Peer::start(vec![vec![0x05, 0x00], socks5_ok()]);
        let socket = socket_to(peer.address);
        let v6: SocketAddr = "[2001:db8::1]:8443".parse().unwrap();
        let mut handshake = Handshake::new(&socks5_config(peer.address), v6);

        assert_eq!(drive(&mut handshake, &socket), Step::Done);

        let seen = peer.seen();
        assert_eq!(seen[3], 0x05);
        assert_eq!(seen[6], 0x04, "ATYP must be the IPv6 form");
        let expected = match v6 {
            SocketAddr::V6(addr) => addr.ip().octets(),
            SocketAddr::V4(_) => unreachable!("the literal above is IPv6"),
        };
        assert_eq!(&seen[7..23], &expected[..]);
        assert_eq!(&seen[23..25], &8443u16.to_be_bytes());
    }

    #[test]
    fn an_http_connect_handshake_completes() {
        let peer = Peer::start(vec![b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec()]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&http_config(peer.address), target());

        assert_eq!(drive(&mut handshake, &socket), Step::Done);

        let seen = String::from_utf8(peer.seen()).unwrap();
        assert!(
            seen.starts_with("CONNECT 203.0.113.10:443 HTTP/1.1\r\n"),
            "the request line must carry the destination, got {seen:?}"
        );
        assert!(
            seen.contains("Host: 203.0.113.10:443\r\n"),
            "a proxy is entitled to route on Host, so it has to be there: {seen:?}"
        );
        assert!(
            seen.ends_with("\r\n\r\n"),
            "the blank line is what ends the request; without it the proxy waits: {seen:?}"
        );
    }

    #[test]
    fn an_http_proxy_that_needs_authentication_is_reported_as_such() {
        let peer = Peer::start(vec![b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n".to_vec()]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&http_config(peer.address), target());

        let step = drive(&mut handshake, &socket);
        assert!(
            matches!(step, Step::Refused(reason) if reason.contains("407")),
            "407 is a configuration problem and must say so, got {step:?}"
        );
    }

    #[test]
    fn an_http_connect_handshake_carries_credentials() {
        let mut config = http_config("127.0.0.1:1".parse().unwrap());
        config.username = Some("user".to_string());
        config.password = Some("pass".to_string());

        let peer = Peer::start(vec![b"HTTP/1.1 200 OK\r\n\r\n".to_vec()]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&config, target());

        assert_eq!(drive(&mut handshake, &socket), Step::Done);

        let seen = String::from_utf8(peer.seen()).unwrap();
        assert!(
            seen.contains("Proxy-Authorization: Basic dXNlcjpwYXNz\r\n"),
            "base64(\"user:pass\") is dXNlcjpwYXNz, got {seen:?}"
        );
    }

    #[test]
    fn an_ipv6_authority_is_bracketed() {
        // `::1:443` is a host name as far as the grammar is concerned, so an
        // unbracketed IPv6 authority is a request for the wrong thing.
        let peer = Peer::start(vec![b"HTTP/1.1 200 OK\r\n\r\n".to_vec()]);
        let socket = socket_to(peer.address);
        let v6: SocketAddr = "[2001:db8::1]:443".parse().unwrap();
        let mut handshake = Handshake::new(&http_config(peer.address), v6);

        assert_eq!(drive(&mut handshake, &socket), Step::Done);

        let seen = String::from_utf8(peer.seen()).unwrap();
        assert!(
            seen.starts_with("CONNECT [2001:db8::1]:443 HTTP/1.1\r\n"),
            "got {seen:?}"
        );
    }

    #[test]
    fn a_reply_split_across_reads_is_waited_for_rather_than_mistaken_for_a_short_one() {
        // A reply arriving in pieces is normal on a real link, and treating the
        // first piece as the whole thing would read a truncated address as a
        // verdict. The peer writes the two halves with a gap between them.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            stream.read(&mut buf).unwrap();
            stream.write_all(&[0x05]).unwrap();
            std::thread::sleep(Duration::from_millis(30));
            stream.write_all(&[0x00]).unwrap();
            stream.read(&mut buf).unwrap();
            stream.write_all(&[0x05, 0x00, 0x00]).unwrap();
            std::thread::sleep(Duration::from_millis(30));
            stream.write_all(&[0x01, 0, 0, 0, 0, 0, 0]).unwrap();
            std::thread::sleep(Duration::from_millis(150));
        });

        let socket = socket_to(address);
        let mut handshake = Handshake::new(&socks5_config(address), target());
        assert_eq!(drive(&mut handshake, &socket), Step::Done);
        let _ = handle.join();
    }

    #[test]
    fn the_handshake_never_reads_past_the_end_of_its_reply() {
        // The invariant the module's header is about. Everything after the reply
        // belongs to the client, and a handshake that buffered it would eat the
        // first bytes of the session it just opened — which would present as
        // "works for plain HTTP, breaks TLS".
        let mut second = socks5_ok();
        second.extend_from_slice(b"tunnel data");
        let peer = Peer::start(vec![vec![0x05, 0x00], second]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&socks5_config(peer.address), target());

        assert_eq!(drive(&mut handshake, &socket), Step::Done);

        // Whatever the handshake left behind has to still be there, in order.
        let mut buf = [0u8; 32];
        let mut read = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while read < b"tunnel data".len() && Instant::now() < deadline {
            match socket.read(&mut buf[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        assert_eq!(
            &buf[..read],
            b"tunnel data",
            "the bytes after the reply must reach the caller untouched"
        );
    }

    #[test]
    fn an_http_response_read_one_byte_at_a_time_still_leaves_the_tunnel_intact() {
        let mut reply = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
        reply.extend_from_slice(b"tunnel data");
        let peer = Peer::start(vec![reply]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&http_config(peer.address), target());

        assert_eq!(drive(&mut handshake, &socket), Step::Done);

        let mut buf = [0u8; 32];
        let mut read = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while read < b"tunnel data".len() && Instant::now() < deadline {
            match socket.read(&mut buf[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        assert_eq!(&buf[..read], b"tunnel data");
    }

    #[test]
    fn a_proxy_that_closes_mid_handshake_is_refused_rather_than_waited_on() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf);
            // Drop the stream without answering.
        });

        let socket = socket_to(address);
        let mut handshake = Handshake::new(&socks5_config(address), target());
        let step = drive(&mut handshake, &socket);
        assert!(
            matches!(step, Step::Refused(reason) if reason.contains("closed the connection")),
            "expected an EOF refusal, got {step:?}"
        );
        let _ = handle.join();
    }

    #[test]
    fn an_oversized_http_response_is_refused_rather_than_buffered() {
        // No real response header is 8 KiB, and one that is has stopped being an
        // answer to the question that was asked.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf);
            // A plausible status line followed by headers that never end.
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n");
            let filler = vec![b'x'; 1024];
            for _ in 0..16 {
                if stream.write_all(b"X-Pad: ").is_err() || stream.write_all(&filler).is_err() {
                    return;
                }
                if stream.write_all(b"\r\n").is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        });

        let socket = socket_to(address);
        let mut handshake = Handshake::new(&http_config(address), target());
        let step = drive(&mut handshake, &socket);
        assert!(
            matches!(step, Step::Refused(reason) if reason.contains("larger than")),
            "expected the size bound to fire, got {step:?}"
        );
        let _ = handle.join();
    }

    #[test]
    fn a_proxy_that_picks_a_method_that_was_not_offered_is_refused() {
        // `0x01` (GSSAPI) is real and is not offered by this client. Treating it
        // as success would send a connect request the proxy is not ready for.
        let peer = Peer::start(vec![vec![0x05, 0x01]]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&socks5_config(peer.address), target());

        let step = drive(&mut handshake, &socket);
        assert!(
            matches!(step, Step::Refused(reason) if reason.contains("not offered")),
            "expected a method refusal, got {step:?}"
        );
    }

    #[test]
    fn an_http_response_that_is_not_a_status_line_is_refused() {
        let peer = Peer::start(vec![b"<html>not a proxy</html>\r\n\r\n".to_vec()]);
        let socket = socket_to(peer.address);
        let mut handshake = Handshake::new(&http_config(peer.address), target());

        let step = drive(&mut handshake, &socket);
        assert!(
            matches!(step, Step::Refused(reason) if reason.contains("not an HTTP status line")),
            "expected a malformed-response refusal, got {step:?}"
        );
    }

    #[test]
    fn a_kind_that_is_not_recognised_is_not_guessed_at() {
        assert_eq!(ProxyKind::parse("socks5"), Some(ProxyKind::Socks5));
        assert_eq!(ProxyKind::parse(" SOCKS "), Some(ProxyKind::Socks5));
        assert_eq!(ProxyKind::parse("http"), Some(ProxyKind::HttpConnect));
        assert_eq!(ProxyKind::parse("connect"), Some(ProxyKind::HttpConnect));
        assert_eq!(
            ProxyKind::parse("https"),
            None,
            "an https proxy means TLS to the proxy, which this does not do; \
             guessing HTTP CONNECT would send a plaintext request to a TLS port"
        );
        assert_eq!(ProxyKind::parse("socks4"), None);
    }

    #[test]
    fn half_a_credential_pair_is_no_credentials() {
        // Sending `user:` to a proxy produces a rejection that reads like a wrong
        // password, which sends the user looking in the wrong place.
        let mut config = http_config("127.0.0.1:1".parse().unwrap());
        config.username = Some("user".to_string());
        assert_eq!(config.credentials(), None);
        config.password = Some(String::new());
        assert_eq!(config.credentials(), None, "an empty password is not a password");
        config.password = Some("pass".to_string());
        assert_eq!(config.credentials(), Some(("user", "pass")));
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        // `Proxy-Authorization` is the one place a wrong encoder fails loudly but
        // unhelpfully: the proxy answers 407 and the user rechecks the password.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn the_header_terminator_is_found_for_both_line_endings() {
        assert_eq!(find_headers_end(b"HTTP/1.1 200 OK\r\n\r\n"), Some(19));
        assert_eq!(find_headers_end(b"HTTP/1.1 200 OK\n\n"), Some(17));
        assert_eq!(find_headers_end(b"HTTP/1.1 200 OK\r\n"), None);
        assert_eq!(find_headers_end(b""), None);
    }

    #[test]
    fn a_status_line_is_read_from_either_spelling() {
        assert_eq!(parse_status(b"HTTP/1.1 200 Connection established"), Some(200));
        assert_eq!(parse_status(b"HTTP/1.0 407\r\n"), Some(407));
        assert_eq!(parse_status(b"HTTP/1.1 20 OK"), None, "two digits is not a status");
        assert_eq!(parse_status(b"HTTP/1.1 abc OK"), None);
        assert_eq!(parse_status(b"200 OK"), None, "no version means it is not a response");
    }

    #[test]
    fn a_target_is_carried_verbatim() {
        // The handshake is handed a destination and must ask for exactly that
        // one: a port quietly dropped or reordered would connect the client to
        // the wrong service, which TLS would then reject as a certificate
        // mismatch — an error that points nowhere near the cause.
        let peer = Peer::start(vec![vec![0x05, 0x00], socks5_ok()]);
        let socket = socket_to(peer.address);
        let destination: SocketAddr = "198.51.100.7:8443".parse().unwrap();
        let mut handshake = Handshake::new(&socks5_config(peer.address), destination);
        assert_eq!(handshake.target(), destination);
        assert_eq!(drive(&mut handshake, &socket), Step::Done);

        let seen = peer.seen();
        assert_eq!(&seen[7..11], &[198, 51, 100, 7]);
        assert_eq!(&seen[11..13], &8443u16.to_be_bytes());
    }
}

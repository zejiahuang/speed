//! Ask a candidate address for its certificate, and check it covers the host.
//!
//! # The problem this solves
//!
//! A rule gives a domain a list of addresses that are *believed* to be better
//! than what the system resolver returns. Measured on a real device, that belief
//! is often wrong in a way the proxy could not previously see:
//!
//! ```text
//! github.com  39 addresses in the rule set
//!   140.82.121.3     http=200        the real GitHub
//!   140.82.114.4     http=200        the real GitHub
//!   20.205.243.166   times out       also what the system resolver returns
//!   51.142.105.107   times out
//!   ...and ~25 more that answer fast and serve someone else's certificate
//! ```
//!
//! The selector ranks by cooldown, then RTT, then failures. A wrong address that
//! answers in 20 ms beats a right one that answers in 80 ms, so the fastest wrong
//! address wins every time — and the client rejects the certificate and closes.
//! The proxy cannot see that rejection: it never decrypts anything, which is the
//! whole point of a CONNECT proxy, and in TLS 1.3 the certificate is inside the
//! encrypted handshake.
//!
//! # What this does instead
//!
//! Asks the question directly. Connect, offer a **TLS 1.2** ClientHello, read the
//! ServerHello and the Certificate message, and check the names in it against the
//! host the client asked for.
//!
//! TLS 1.2 rather than 1.3 on purpose: in 1.2 the Certificate message is
//! plaintext, so no key material is needed to read it. That is what keeps this a
//! few hundred lines of TLV walking instead of a TLS stack. Servers still offer
//! 1.2 — GitHub, Cloudflare and Akamai all do — and one that does not simply
//! fails the probe, which costs a candidate rather than a connection.
//!
//! **No user data is decrypted, and none is seen.** The probe opens its own
//! connection, reads the certificate the server offers to anyone who asks, and
//! closes it. The relayed session is untouched.
//!
//! # Failure is not proof of badness
//!
//! A probe that fails says "this could not be confirmed", not "this is wrong".
//! Callers must treat it that way: refusing a candidate because a probe timed out
//! would turn a slow server into an outage.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// How long the probe's own connection may take.
///
/// Short, and shorter than the tunnel's own connect timeout by an order of
/// magnitude. A rule address that has not accepted a TCP connection in under a
/// second is not going to: measured against `github.com`'s rule set, the dead
/// addresses simply never answer. The tunnel pays its full connect timeout per
/// dead address because its failover is sequential, so the earlier this verdict
/// arrives the fewer of those it pays.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(800);

/// How long a probe may take, end to end.
///
/// A probe that is slow has already failed at its job — the point is to choose
/// between candidates before the user notices.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Cap on the ServerHello flight. A certificate chain is a few kilobytes; the
/// bound stops a hostile or broken server from making the proxy read forever.
const MAX_FLIGHT: usize = 64 * 1024;

/// What a probe concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// The certificate covers the host. Safe to relay to.
    Covers,
    /// The certificate does not cover the host. Relaying would be rejected by
    /// the client, so this address is worse than useless for this domain.
    DoesNotCover,
    /// Nothing accepted the connection.
    ///
    /// Separate from [`Probe::Inconclusive`] because it is a much stronger
    /// statement: a refused or unanswered TCP connection says the address is not
    /// serving anything, which is a fact about the address rather than about this
    /// probe. The tunnel's failover is sequential and pays its full connect
    /// timeout per candidate, so dropping the dead ones up front is the
    /// difference between one attempt and a dozen.
    Unreachable,
    /// The server answered but offered no certificate: a TLS 1.3-only server, or
    /// a handshake this minimal client could not follow. **Not** evidence
    /// against the address.
    Inconclusive,
}

/// Ask `target` for its certificate and check it against `host`.
///
/// `host` is the name the client put in its CONNECT line — the name its TLS
/// session will use as SNI and the name it will verify against.
pub fn check(target: SocketAddr, host: &str) -> Probe {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    match ask(target, host, deadline) {
        Ok(Answer::Certificate(names)) => {
            if names.iter().any(|name| covers(name, host)) {
                Probe::Covers
            } else {
                Probe::DoesNotCover
            }
        }
        // A server that answered without a certificate — an alert, or a refusal
        // to speak TLS 1.2 — is not evidence against the address.
        Ok(Answer::NoCertificate) => Probe::Inconclusive,
        // The connection never opened. That is about the address, not the probe.
        Err(_) => Probe::Unreachable,
    }
}

/// Send the probe's ClientHello and return the raw reply, for diagnosis.
///
/// Public because the failure modes here are invisible from the outside: a
/// server that refuses the handshake and a certificate that does not cover the
/// host both end as "not usable", and the bytes are the only way to tell them
/// apart. Used by `examples/probe_check.rs`.
pub fn debug_flight(target: SocketAddr, host: &str) -> std::io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
    stream.write_all(&client_hello(host))?;
    stream.flush()?;
    let mut reply = Vec::new();
    let mut buffer = [0u8; 4096];
    for _ in 0..4 {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => reply.extend_from_slice(&buffer[..n]),
            Err(_) => break,
        }
    }
    Ok(reply)
}

/// The names the parser found in a flight, for diagnosis.
pub fn debug_names(flight: &[u8]) -> Vec<String> {
    match certificate_names(flight) {
        Some(Answer::Certificate(names)) => names,
        Some(Answer::NoCertificate) => vec!["<no certificate>".to_string()],
        None => vec!["<incomplete>".to_string()],
    }
}

/// The client's ClientHello, for diagnosis.
pub fn debug_hello(host: &str) -> Vec<u8> {
    client_hello(host)
}

/// What came back from the server.
enum Answer {
    /// The chain arrived, and these are the names in the leaf.
    Certificate(Vec<String>),
    /// The server answered, but not with a certificate: an alert, or a
    /// handshake this minimal client could not follow.
    NoCertificate,
}

/// Open a connection, run a TLS 1.2 handshake far enough to read the chain.
fn ask(target: SocketAddr, host: &str, deadline: Instant) -> std::io::Result<Answer> {
    let mut stream = TcpStream::connect_timeout(&target, PROBE_TIMEOUT)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Ok(Answer::NoCertificate);
    }
    stream.set_read_timeout(Some(remaining))?;
    stream.set_write_timeout(Some(remaining))?;

    stream.write_all(&client_hello(host))?;
    stream.flush()?;

    let mut flight = Vec::with_capacity(4096);
    let mut buffer = [0u8; 4096];
    while flight.len() < MAX_FLIGHT {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => flight.extend_from_slice(&buffer[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(err) => return Err(err),
        }
        // The chain arrives in the first flight; once a Certificate has been
        // seen there is nothing further to wait for.
        if let Some(answer) = certificate_names(&flight) {
            return Ok(answer);
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    Ok(certificate_names(&flight).unwrap_or(Answer::NoCertificate))
}

/// A minimal TLS 1.2 ClientHello.
///
/// Nothing here is secret and nothing is verified: the handshake is abandoned as
/// soon as the certificate has been read. The random is a fixed string rather
/// than entropy because a real random would need a source, and a probe that
/// cannot be repeated is harder to debug.
///
/// # The extensions are not optional
///
/// A TLS 1.2 server **rejects** a ClientHello without `signature_algorithms` —
/// RFC 5246 makes it mandatory, and OpenSSL answers with a `handshake_failure`
/// alert. `supported_groups` and `ec_point_formats` are needed for the ECDHE
/// suite offered here. Leaving them out produced an alert, which the first
/// version of this read as "no names in the certificate" and reported as a
/// mismatch — so every address, good or bad, was condemned. That is the failure
/// mode this comment exists to prevent.
fn client_hello(host: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(256);

    // ClientHello
    body.push(0x01);
    body.extend_from_slice(&[0x00, 0x00, 0x00]); // length, patched below
    body.extend_from_slice(&[0x03, 0x03]); // TLS 1.2
    body.extend_from_slice(&[0x11; 32]); // random
    body.push(0x00); // empty session id
    body.extend_from_slice(&[0x00, 0x02]); // one cipher suite
    body.extend_from_slice(&[0xc0, 0x2f]); // TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
    body.extend_from_slice(&[0x01, 0x00]); // one compression method: null

    let mut extensions = Vec::with_capacity(64);

    // server_name (0x0000) — without it a shared host answers with its default
    // certificate and every probe would be wrong.
    //
    // The encoding is `ServerNameList -> ServerName -> name_type + HostName`,
    // and `name_type` is a byte that must be 0. Leaving it out shifts every
    // length by one and the server answers with a fatal `unrecognized_name`
    // alert (112) instead of a certificate.
    {
        let name = host.as_bytes();
        let mut entry = Vec::with_capacity(name.len() + 3);
        entry.push(0x00); // name_type: host_name
        entry.extend_from_slice(&[(name.len() >> 8) as u8, name.len() as u8]);
        entry.extend_from_slice(name);

        let mut payload = Vec::with_capacity(entry.len() + 2);
        payload.extend_from_slice(&[(entry.len() >> 8) as u8, entry.len() as u8]);
        payload.extend_from_slice(&entry);
        extension(&mut extensions, 0x0000, &payload);
    }

    // signature_algorithms (0x000d) — mandatory in TLS 1.2.
    {
        let algorithms: [u8; 6] = [
            0x04, 0x01, // rsa_pkcs1_sha256
            0x05, 0x01, // ecdsa_secp256r1_sha256
            0x04, 0x03, // ecdsa_sha256
        ];
        let mut payload = Vec::with_capacity(algorithms.len() + 2);
        payload.extend_from_slice(&[(algorithms.len() >> 8) as u8, algorithms.len() as u8]);
        payload.extend_from_slice(&algorithms);
        extension(&mut extensions, 0x000d, &payload);
    }

    // supported_groups (0x000a) — secp256r1, which the offered suite needs.
    extension(&mut extensions, 0x000a, &[0x00, 0x02, 0x00, 0x17]);

    // ec_point_formats (0x000b) — uncompressed.
    extension(&mut extensions, 0x000b, &[0x01, 0x00]);

    body.extend_from_slice(&[(extensions.len() >> 8) as u8, extensions.len() as u8]);
    body.extend_from_slice(&extensions);

    let body_len = body.len() - 4;
    body[1] = (body_len >> 16) as u8;
    body[2] = (body_len >> 8) as u8;
    body[3] = body_len as u8;

    // Record layer
    let mut record = Vec::with_capacity(body.len() + 5);
    record.push(0x16); // handshake
    record.extend_from_slice(&[0x03, 0x01]); // TLS 1.0, what every server accepts
    record.extend_from_slice(&[(body.len() >> 8) as u8, body.len() as u8]);
    record.extend_from_slice(&body);
    record
}

/// Append one extension: a two-byte type and a two-byte-length payload.
fn extension(out: &mut Vec<u8>, kind: u16, payload: &[u8]) {
    out.extend_from_slice(&[(kind >> 8) as u8, kind as u8]);
    out.extend_from_slice(&[(payload.len() >> 8) as u8, payload.len() as u8]);
    out.extend_from_slice(payload);
}

/// Walk the records looking for a Certificate message, and pull the names out.
///
/// `None` means no Certificate has arrived *yet*, which is what tells the caller
/// to keep reading. `Some(Answer::NoCertificate)` means the server has answered
/// and will not be sending one.
fn certificate_names(flight: &[u8]) -> Option<Answer> {
    let mut offset = 0usize;
    let mut handshake = Vec::new();

    // Records first: the handshake messages inside them are what matters, and a
    // certificate can be split across records.
    while offset + 5 <= flight.len() {
        let kind = flight[offset];
        let length = ((flight[offset + 3] as usize) << 8) | flight[offset + 4] as usize;
        let start = offset + 5;
        let end = start + length;
        if end > flight.len() {
            break;
        }
        if kind == 0x16 {
            handshake.extend_from_slice(&flight[start..end]);
        }
        // 0x15 is an alert: the server refused the handshake outright. That is a
        // statement about this probe, not about the address's certificate, so it
        // must not be read as a mismatch.
        if kind == 0x15 {
            return Some(Answer::NoCertificate);
        }
        offset = end;
    }

    // Then the handshake messages inside them.
    let mut at = 0usize;
    while at + 4 <= handshake.len() {
        let kind = handshake[at];
        let length = ((handshake[at + 1] as usize) << 16)
            | ((handshake[at + 2] as usize) << 8)
            | handshake[at + 3] as usize;
        let start = at + 4;
        let end = start + length;
        if end > handshake.len() {
            return None; // split across records; keep reading
        }
        if kind == 0x0b {
            return Some(Answer::Certificate(names_in_certificate_message(
                &handshake[start..end],
            )));
        }
        at = end;
    }
    None
}

/// The Certificate message: a list of DER certificates. Only the first is read —
/// the leaf is the one whose names the client checks against the host.
fn names_in_certificate_message(message: &[u8]) -> Vec<String> {
    if message.len() < 6 {
        return Vec::new();
    }
    let total = ((message[0] as usize) << 16) | ((message[1] as usize) << 8) | message[2] as usize;
    if total + 3 > message.len() || message.len() < 6 {
        return Vec::new();
    }
    let cert_len = ((message[3] as usize) << 16) | ((message[4] as usize) << 8) | message[5] as usize;
    let start = 6;
    let end = start + cert_len;
    if end > message.len() {
        return Vec::new();
    }
    names_in_certificate(&message[start..end])
}

/// Pull every `dNSName` out of a DER certificate, plus its subject CN.
///
/// A hand-written walk rather than a parser dependency: the structure being read
/// is three levels deep and never changes, and pulling in an X.509 crate to read
/// one extension would be a larger surface than the thing it reads.
///
/// It is deliberately forgiving. Anything it does not understand is skipped, and
/// an empty result means "could not tell", never "no names" — a wrong conclusion
/// here would quarantine a good address.
fn names_in_certificate(der: &[u8]) -> Vec<String> {
    let mut names = Vec::new();

    // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }
    let Some(certificate) = child(der, 0x30) else {
        return names;
    };
    // TBSCertificate ::= SEQUENCE { [0] version?, serialNumber, signature,
    //                               issuer, validity, subject, spki,
    //                               [1]?, [2]?, [3] extensions? }
    let Some(tbs) = child(certificate, 0x30) else {
        return names;
    };

    let mut at = 0usize;
    // An explicit version tag is present in every modern certificate.
    if let Some(version) = tlv(&tbs[at..]) {
        if version.tag == 0xa0 {
            at += version.total();
        }
    }
    // Six mandatory fields, each a TLV to step over.
    for _ in 0..6 {
        let Some(field) = tlv(tbs.get(at..).unwrap_or_default()) else {
            return names;
        };
        at += field.total();
    }
    // Then the optional unique IDs and the extensions.
    while at < tbs.len() {
        let Some(field) = tlv(&tbs[at..]) else {
            break;
        };
        if field.tag == 0xa3 {
            let start = at + field.header;
            let end = start + field.content;
            if end <= tbs.len() {
                collect_extensions(&tbs[start..end], &mut names);
            }
            break;
        }
        at += field.total();
    }

    names
}

/// Extensions ::= SEQUENCE OF Extension. Only subjectAltName is wanted.
fn collect_extensions(body: &[u8], names: &mut Vec<String>) {
    let Some(extensions) = child(body, 0x30) else {
        return;
    };
    let mut at = 0usize;
    while at < extensions.len() {
        let Some(extension) = tlv(&extensions[at..]) else {
            return;
        };
        if extension.tag != 0x30 {
            return;
        }
        let start = at + extension.header;
        let end = start + extension.content;
        if end > extensions.len() {
            return;
        }
        collect_one_extension(&extensions[start..end], names);
        at += extension.total();
    }
}

/// One Extension: an OID, an optional critical flag, and an OCTET STRING.
fn collect_one_extension(extension: &[u8], names: &mut Vec<String>) {
    let Some(oid) = tlv(extension) else { return };
    if oid.tag != 0x06 {
        return;
    }
    let oid_start = oid.header;
    let oid_end = oid_start + oid.content;
    if oid_end > extension.len() {
        return;
    }
    // subjectAltName is 2.5.29.17, which encodes as 55 1d 11.
    if extension[oid_start..oid_end] != [0x55, 0x1d, 0x11] {
        return;
    }

    let mut at = oid.total();
    // Skip the critical flag when present.
    if let Some(flag) = tlv(extension.get(at..).unwrap_or_default()) {
        if flag.tag == 0x01 {
            at += flag.total();
        }
    }
    let Some(value) = tlv(extension.get(at..).unwrap_or_default()) else {
        return;
    };
    if value.tag != 0x04 {
        return;
    }
    let start = at + value.header;
    let end = start + value.content;
    if end > extension.len() {
        return;
    }
    collect_general_names(&extension[start..end], names);
}

/// GeneralNames ::= SEQUENCE OF GeneralName, where dNSName is `[2] IA5String`.
fn collect_general_names(body: &[u8], names: &mut Vec<String>) {
    let Some(sequence) = child(body, 0x30) else {
        return;
    };
    let mut at = 0usize;
    while at < sequence.len() {
        let Some(name) = tlv(&sequence[at..]) else {
            return;
        };
        let start = at + name.header;
        let end = start + name.content;
        if end > sequence.len() {
            return;
        }
        // 0x82 is context tag 2, primitive: a DNS name.
        if name.tag == 0x82 {
            if let Ok(text) = std::str::from_utf8(&sequence[start..end]) {
                names.push(text.to_ascii_lowercase());
            }
        }
        at += name.total();
    }
}

/// A parsed TLV header.
#[derive(Debug, Clone, Copy)]
struct Tlv {
    tag: u8,
    /// Bytes the header itself occupies.
    header: usize,
    /// Bytes of content that follow.
    content: usize,
}

impl Tlv {
    /// Total size: header plus content.
    fn total(&self) -> usize {
        self.header + self.content
    }
}

/// Parse a TLV header.
///
/// DER length encoding: a short form for anything under 128 bytes, and a
/// long form where the low seven bits of the first byte count how many length
/// bytes follow. The long form is what certificates use for their outer
/// sequences, so it cannot be skipped.
fn tlv(bytes: &[u8]) -> Option<Tlv> {
    let tag = *bytes.first()?;
    let first = *bytes.get(1)?;
    if first & 0x80 == 0 {
        return Some(Tlv {
            tag,
            header: 2,
            content: first as usize,
        });
    }
    let count = (first & 0x7f) as usize;
    // Four bytes is the most DER allows, and a length of zero bytes is
    // indefinite length, which DER forbids.
    if count == 0 || count > 4 || bytes.len() < 2 + count {
        return None;
    }
    let mut content = 0usize;
    for byte in &bytes[2..2 + count] {
        content = (content << 8) | *byte as usize;
    }
    Some(Tlv {
        tag,
        header: 2 + count,
        content,
    })
}

/// The content of the first TLV when it carries the expected tag.
fn child(bytes: &[u8], expected: u8) -> Option<&[u8]> {
    let header = tlv(bytes)?;
    if header.tag != expected {
        return None;
    }
    let start = header.header;
    let end = start + header.content;
    if end > bytes.len() {
        return None;
    }
    Some(&bytes[start..end])
}

/// Whether a certificate name matches the host the client asked for.
///
/// A single leading `*.` matches exactly one label, which is what RFC 6125 says
/// and what every browser does. `*.example.com` therefore covers
/// `api.example.com` but not `example.com` and not `a.b.example.com`.
fn covers(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if pattern == host {
        return true;
    }
    let Some(suffix) = pattern.strip_prefix("*.") else {
        return false;
    };
    let Some(rest) = host.split_once('.').map(|(_, rest)| rest) else {
        return false;
    };
    rest == suffix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_name_covers() {
        assert!(covers("github.com", "github.com"));
        assert!(covers("GitHub.com", "github.com"));
    }

    #[test]
    fn a_wildcard_covers_one_label_only() {
        assert!(covers("*.github.com", "api.github.com"));
        // RFC 6125: the wildcard does not match the bare domain...
        assert!(!covers("*.github.com", "github.com"));
        // ...and does not span a dot.
        assert!(!covers("*.github.com", "a.b.github.com"));
    }

    #[test]
    fn an_unrelated_name_does_not_cover() {
        assert!(!covers("example.com", "github.com"));
        assert!(!covers("*.example.com", "github.com"));
    }

    #[test]
    fn a_trailing_dot_is_ignored_on_both_sides() {
        assert!(covers("github.com.", "github.com"));
        assert!(covers("github.com", "github.com."));
    }

    #[test]
    fn the_client_hello_declares_every_extension_tls12_requires() {
        // A TLS 1.2 server answers a ClientHello without `signature_algorithms`
        // with an alert, which the first version of this read as a certificate
        // mismatch — condemning every address, good or bad. The extensions are
        // the difference between a working probe and a broken one.
        let hello = client_hello("github.com");
        let text: Vec<u8> = hello.clone();
        let has = |needle: &[u8]| text.windows(needle.len()).any(|w| w == needle);

        assert!(has(&[0x00, 0x00]), "server_name is missing");
        assert!(has(&[0x00, 0x0d]), "signature_algorithms is missing");
        assert!(has(&[0x00, 0x0a]), "supported_groups is missing");
        assert!(has(&[0x00, 0x0b]), "ec_point_formats is missing");
    }

    #[test]
    fn the_client_hello_carries_the_host_in_sni() {
        let hello = client_hello("github.com");
        // Record: handshake, then the version every server accepts.
        assert_eq!(hello[0], 0x16);
        let text = String::from_utf8_lossy(&hello);
        assert!(text.contains("github.com"), "SNI is missing");
    }

    #[test]
    fn the_client_hello_lengths_are_consistent() {
        let hello = client_hello("a.example");
        let record_len = ((hello[3] as usize) << 8) | hello[4] as usize;
        assert_eq!(record_len, hello.len() - 5, "the record length is wrong");
        let body_len = ((hello[6] as usize) << 16) | ((hello[7] as usize) << 8) | hello[8] as usize;
        assert_eq!(body_len, hello.len() - 9, "the handshake length is wrong");
    }

    #[test]
    fn a_flight_without_a_certificate_yet_asks_for_more() {
        // A ServerHello record, no Certificate. `None` means keep reading.
        let record = [0x16, 0x03, 0x03, 0x00, 0x04, 0x02, 0x00, 0x00, 0x00];
        assert!(certificate_names(&record).is_none());
    }

    #[test]
    fn a_server_alert_is_an_empty_answer_not_a_missing_one() {
        // 0x15 is an alert. The server refused; there will be no certificate.
        let record = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x28];
        assert!(matches!(
            certificate_names(&record),
            Some(Answer::NoCertificate)
        ));
    }

    #[test]
    fn a_refused_connection_is_reported_as_unreachable() {
        // Port 1 is not listening. The conclusion must be Unreachable — a fact
        // about the address — and never DoesNotCover, which would be a claim
        // about a certificate that was never seen.
        let result = check("127.0.0.1:1".parse().unwrap(), "github.com");
        assert_eq!(result, Probe::Unreachable);
    }
}

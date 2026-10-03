//! Recovering the domain from a TLS ClientHello.
//!
//! # Why this exists
//!
//! The data plane only ever sees addresses. Everything that makes the kernel
//! more than a relay hangs off knowing the *name*: rule addresses are found by
//! looking the destination up in the DNS observation cache, and a candidate can
//! only be judged by checking whether its certificate covers the host. When the
//! name is unknown the flow is relayed blind — the rule set is never consulted,
//! the certificate check is skipped, and the failure report cannot say which
//! domain failed.
//!
//! The kernel's only source of names used to be the DNS it forwards, and that
//! source has been closing for years: a client resolving over HTTPS never shows
//! the kernel a query, and DoH is on by default in current browsers. The other
//! way a flow ends up unnamed is the address itself — one that no rule claims
//! identifies nothing, so a client that resolved correctly on its own still
//! arrives as a bare address.
//!
//! Both are the same problem from here, and between them they cover the traffic
//! that most needs steering. How much of it there is is a question for the
//! counters (`flows_without_name` against `flows_named_by_sni`), not for this
//! module: nothing here can tell how often it is reached.
//!
//! # Why this is not interception
//!
//! `server_name` is sent in the clear. It has to be: the server picks its
//! certificate from it before any key is agreed. Reading it requires no key, no
//! certificate authority, and no modification of the stream — the bytes are
//! already flowing through this process, and this module only looks at them.
//!
//! Nothing here decrypts, re-signs or terminates TLS, which is what keeps
//! certificate pinning intact and is the whole reason the kernel can be used
//! without installing a CA.
//!
//! # ECH
//!
//! `encrypted_client_hello` is reported as [`Sni::Ech`], carrying whatever name
//! was in the clear. It is not treated as a reason to withhold that name, and the
//! reason is in the RFC rather than in a judgement call: RFC 9849 §10.10.4 states
//! that real ECH is *designed* to be indistinguishable from GREASE ECH for a
//! passive adversary that does not know the server's `ECHConfigList` — which is
//! exactly what this module is. "The extension is present" is therefore not
//! evidence that the visible name is a cover name, and a module that reads it as
//! evidence loses the name on every client that greases.
//!
//! That cost is not hypothetical. Measured against a current Android browser: the
//! extension is present on **every** ClientHello (7 of 7 captures, 1729–1825
//! bytes, 18 extensions), while the device's own curl never sends it (517 bytes,
//! 11 extensions). The names in those hellos were the real ones — the client had
//! been pointed at a name that does not exist and so cannot have an `ECHConfig`,
//! which left it no `public_name` to substitute and nothing to send but what it
//! actually wanted. RFC 9849 §6.2.1 describes exactly that shape: GREASE sets
//! `config_id` to a random byte, and the two captures read `0xa5` and `0x00`.
//!
//! The caller decides what the flag is worth. This module only reports.
//!
//! # What this does not handle
//!
//! A ClientHello split across more than one TLS record. TLS permits it and it
//! happens when a hello exceeds the record size, which a real browser's does not
//! — they are a few hundred bytes. Supporting it would mean reassembling records
//! before parsing, and the parser is deliberately restartable from the start of
//! the buffer instead.
//!
//! The failure mode is a missing name, not a wrong one: the first record's body
//! cannot contain the whole handshake, so the parse reports
//! [`Sni::Incomplete`] however many more bytes arrive, and the caller stops
//! watching at its own buffer cap. That is the right way round — a name this
//! module cannot vouch for is worse than no name, because the caller routes on
//! it.
//!
//! The other half of the format lives in `watt_net::probe::client_hello`, which
//! *builds* one of these to ask a server for its certificate. The two are
//! deliberately independent implementations; the test that reads a hello built
//! there is checking the wire format rather than a shared helper.

/// What a buffer of client bytes says about the domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sni {
    /// The name the client is asking for, lowercased, without a trailing dot.
    Found(String),
    /// A ClientHello that carries `encrypted_client_hello`, with the name it left
    /// in the clear.
    ///
    /// The name is *reported*, not vouched for: with real ECH it is a cover name
    /// the server chose, and this module cannot tell that case from GREASE ECH —
    /// see the module documentation. What it can say is that a client which had
    /// no `ECHConfig` for the name it wanted had nothing else to put there, which
    /// is the common case by a wide margin.
    Ech(String),
    /// A complete TLS ClientHello that carries no server_name. A client may
    /// legitimately omit it when it will send one later, so this is not a fault.
    Absent,
    /// A TLS ClientHello that this buffer does not contain in full yet. The
    /// caller should feed more bytes before concluding anything.
    Incomplete,
    /// Not a TLS handshake, so there will never be a name here. The caller should
    /// stop asking.
    NotTls,
}

/// How many bytes a ClientHello may take before it is not worth waiting for.
///
/// A real one is well under a kilobyte. The limit exists so a client that sends
/// a handshake record header claiming an implausible length cannot make the
/// caller buffer without bound.
pub const MAX_HELLO: usize = 16 * 1024;

/// The `server_name` extension.
const EXT_SERVER_NAME: u16 = 0x0000;
/// `encrypted_client_hello`. Present means the visible name *may* be a cover
/// name — it does not mean it is one, which is the whole point of GREASE ECH.
const EXT_ENCRYPTED_CLIENT_HELLO: u16 = 0xfe0d;

const RECORD_HANDSHAKE: u8 = 0x16;
const HANDSHAKE_CLIENT_HELLO: u8 = 0x01;
/// The name type for a host name; the only one defined.
const NAME_TYPE_HOST: u8 = 0x00;

/// Read the domain out of the start of a client-to-server stream.
///
/// Returns [`Sni::Incomplete`] when more bytes are needed, so the caller can
/// simply call this again on the extended buffer.
pub fn from_client_hello(bytes: &[u8]) -> Sni {
    // TLS record header: type, version, length.
    if bytes.len() < 5 {
        return Sni::Incomplete;
    }
    if bytes[0] != RECORD_HANDSHAKE {
        return Sni::NotTls;
    }
    // A handshake record whose body is longer than the buffer means the
    // ClientHello has been split across segments, which is legal and does happen.
    let record_len = u16::from_be_bytes([bytes[3], bytes[4]]) as usize;
    if record_len > MAX_HELLO {
        return Sni::NotTls;
    }
    let body = match bytes.get(5..5 + record_len) {
        Some(body) => body,
        None => return Sni::Incomplete,
    };

    // Handshake header: message type, then a three-byte length.
    if body.len() < 4 {
        return Sni::Incomplete;
    }
    if body[0] != HANDSHAKE_CLIENT_HELLO {
        return Sni::NotTls;
    }
    let hello_len = ((body[1] as usize) << 16) | ((body[2] as usize) << 8) | body[3] as usize;
    let hello = match body.get(4..4 + hello_len) {
        Some(hello) => hello,
        None => return Sni::Incomplete,
    };

    parse_client_hello(hello)
}

/// Walk the ClientHello body to the extension block, then look for the name.
fn parse_client_hello(hello: &[u8]) -> Sni {
    let mut cursor = 0usize;

    // legacy_version (2) + random (32).
    cursor += 2 + 32;
    if hello.len() < cursor {
        return Sni::Incomplete;
    }

    // session_id
    let Some(session_len) = hello.get(cursor).copied() else {
        return Sni::Incomplete;
    };
    cursor += 1 + session_len as usize;

    // cipher_suites
    let Some(ciphers_len) = read_u16(hello, cursor) else {
        return Sni::Incomplete;
    };
    cursor += 2 + ciphers_len as usize;

    // compression_methods
    let Some(compression_len) = hello.get(cursor).copied() else {
        return Sni::Incomplete;
    };
    cursor += 1 + compression_len as usize;

    // extensions. A ClientHello may legitimately stop here, with no SNI.
    let Some(extensions_len) = read_u16(hello, cursor) else {
        return Sni::Incomplete;
    };
    cursor += 2;
    let extensions = match hello.get(cursor..cursor + extensions_len as usize) {
        Some(extensions) => extensions,
        None => return Sni::Incomplete,
    };

    let mut found: Option<String> = None;
    let mut covered = false;
    let mut at = 0usize;
    while at + 4 <= extensions.len() {
        let kind = u16::from_be_bytes([extensions[at], extensions[at + 1]]);
        let len = u16::from_be_bytes([extensions[at + 2], extensions[at + 3]]) as usize;
        let data = match extensions.get(at + 4..at + 4 + len) {
            Some(data) => data,
            // A truncated extension list is a malformed or segmented hello; the
            // caller's buffer is the only thing that can fix it.
            None => return Sni::Incomplete,
        };
        match kind {
            EXT_SERVER_NAME => found = parse_server_name(data).or(found),
            EXT_ENCRYPTED_CLIENT_HELLO => covered = true,
            _ => {}
        }
        at += 4 + len;
    }

    // ECH is a flag on the answer, not a reason to withhold it: refusing here
    // drops the name for every client that greases, and measurement says that is
    // every current browser. See the module documentation.
    match (found, covered) {
        (Some(name), false) => Sni::Found(name),
        (Some(name), true) => Sni::Ech(name),
        (None, _) => Sni::Absent,
    }
}

/// `ServerNameList`: a length, then entries of (type, length, bytes).
fn parse_server_name(data: &[u8]) -> Option<String> {
    let list_len = u16::from_be_bytes([*data.first()?, *data.get(1)?]) as usize;
    let list = data.get(2..2 + list_len)?;

    let mut at = 0usize;
    while at + 3 <= list.len() {
        let name_type = list[at];
        let len = u16::from_be_bytes([list[at + 1], list[at + 2]]) as usize;
        let raw = list.get(at + 3..at + 3 + len)?;
        if name_type == NAME_TYPE_HOST {
            return normalize(raw);
        }
        at += 3 + len;
    }
    None
}

/// Accept a name only if it is something a rule set could match.
///
/// The bytes come off the wire, so this is the boundary where a hostile client
/// could hand the kernel a name of its choosing. Nothing here is a security
/// control — a client can already choose where to connect — but a name that is
/// not a plain host name is not useful for a lookup and is refused rather than
/// carried around as if it meant something.
fn normalize(raw: &[u8]) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let text = std::str::from_utf8(raw).ok()?;
    let trimmed = text.strip_suffix('.').unwrap_or(text);
    if trimmed.is_empty() || trimmed.len() > 253 {
        return None;
    }
    let ok = trimmed.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    });
    if !ok {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ClientHello built the way a browser builds one: the extensions that
    /// matter for parsing, in the order that makes the offsets non-trivial.
    fn hello(host: Option<&str>, ech: bool) -> Vec<u8> {
        let mut extensions: Vec<u8> = Vec::new();

        // supported_versions, so the extension block is not just the SNI.
        let versions = [0x02u8, 0x03, 0x04];
        extensions.extend_from_slice(&[0x00, 0x2b]);
        extensions.extend_from_slice(&(versions.len() as u16).to_be_bytes());
        extensions.extend_from_slice(&versions);

        if let Some(host) = host {
            let mut entry = vec![NAME_TYPE_HOST];
            entry.extend_from_slice(&(host.len() as u16).to_be_bytes());
            entry.extend_from_slice(host.as_bytes());
            let mut list = Vec::new();
            list.extend_from_slice(&(entry.len() as u16).to_be_bytes());
            list.extend_from_slice(&entry);
            extensions.extend_from_slice(&EXT_SERVER_NAME.to_be_bytes());
            extensions.extend_from_slice(&(list.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&list);
        }

        if ech {
            // The real layout, RFC 9849 §5: type, cipher_suite, config_id, enc,
            // payload. Nothing here reads inside the extension, so the contents
            // cannot change a verdict today — but a fixture shaped like no client
            // has ever sent one cannot catch a parser that later does look
            // inside, and this module's whole claim is about what real clients
            // send. The values are the ones two captured browser hellos used:
            // outer, HKDF-SHA256/ChaCha20Poly1305, and a `config_id` that GREASE
            // sets to a random byte (the captures read 0xa5 and 0x00).
            let mut ech_ext: Vec<u8> = vec![0x00];
            ech_ext.extend_from_slice(&[0x00, 0x01, 0x00, 0x03]);
            ech_ext.push(0xa5);
            ech_ext.extend_from_slice(&32u16.to_be_bytes());
            ech_ext.extend_from_slice(&[0x5a; 32]);
            ech_ext.extend_from_slice(&144u16.to_be_bytes());
            ech_ext.extend_from_slice(&[0x3c; 144]);
            extensions.extend_from_slice(&EXT_ENCRYPTED_CLIENT_HELLO.to_be_bytes());
            extensions.extend_from_slice(&(ech_ext.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&ech_ext);
        }

        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(&[0x03, 0x03]); // legacy_version
        body.extend_from_slice(&[0x11; 32]); // random
        body.push(0); // empty session id
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&[0x13, 0x01]); // one cipher suite
        body.push(1);
        body.push(0); // one compression method: null
        body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        body.extend_from_slice(&extensions);

        let mut handshake: Vec<u8> = vec![HANDSHAKE_CLIENT_HELLO];
        let len = body.len();
        handshake.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8]);
        handshake.extend_from_slice(&body);

        let mut record: Vec<u8> = vec![RECORD_HANDSHAKE, 0x03, 0x01];
        record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
        record.extend_from_slice(&handshake);
        record
    }

    #[test]
    fn finds_the_name_in_a_real_shaped_hello() {
        assert_eq!(
            from_client_hello(&hello(Some("github.com"), false)),
            Sni::Found("github.com".to_string())
        );
    }

    #[test]
    fn the_name_is_normalized_before_it_is_returned() {
        // A rule set is keyed by lower case without a trailing dot, and a client
        // is free to send either form. Normalizing here means every later
        // comparison is a plain string compare.
        assert_eq!(
            from_client_hello(&hello(Some("WWW.Example.COM."), false)),
            Sni::Found("www.example.com".to_string())
        );
    }

    #[test]
    fn a_hello_split_across_segments_is_reported_incomplete_until_it_is_whole() {
        // The reason this is a three-valued answer rather than an Option: the
        // caller reads one segment at a time, and "not here yet" must not be
        // confused with "not a TLS handshake", which is the signal to stop.
        let whole = hello(Some("example.com"), false);
        for cut in 1..whole.len() {
            assert_eq!(
                from_client_hello(&whole[..cut]),
                Sni::Incomplete,
                "a prefix of {cut} bytes should be incomplete, not a verdict"
            );
        }
        assert_eq!(
            from_client_hello(&whole),
            Sni::Found("example.com".to_string())
        );
    }

    #[test]
    fn a_client_that_sends_its_name_later_is_absent_not_a_failure() {
        // TLS permits a second ClientHello carrying the SNI. Reporting this as
        // `NotTls` would make the caller stop looking, and the name would be
        // missed for a client that was about to send it.
        assert_eq!(from_client_hello(&hello(None, false)), Sni::Absent);
    }

    #[test]
    fn a_hello_with_ech_still_reports_the_name_it_left_in_the_clear() {
        // The extension says "the real name may be encrypted"; it does not say
        // "this name is a lie". RFC 9849 designs the two cases to be
        // indistinguishable from where this parser stands, so withholding the
        // name costs every client that greases — measurement says that is every
        // browser — and buys nothing against the few that do not.
        assert_eq!(
            from_client_hello(&hello(Some("cover.example"), true)),
            Sni::Ech("cover.example".to_string())
        );
        // ECH with nothing in the clear is still no name.
        assert_eq!(from_client_hello(&hello(None, true)), Sni::Absent);
    }

    #[test]
    fn something_that_is_not_tls_is_reported_so_the_caller_can_stop() {
        // HTTP, SSH and every other plaintext protocol land here. The caller must
        // be able to give up rather than buffer a stream that will never parse.
        assert_eq!(from_client_hello(b"GET / HTTP/1.1\r\n"), Sni::NotTls);
        // A TLS record of a different type -- application data, say.
        assert_eq!(from_client_hello(&[0x17, 0x03, 0x03, 0x00, 0x10, 0x00]), Sni::NotTls);
    }

    #[test]
    fn a_truncated_buffer_is_not_mistaken_for_a_verdict() {
        assert_eq!(from_client_hello(&[]), Sni::Incomplete);
        assert_eq!(from_client_hello(&[0x16]), Sni::Incomplete);
        assert_eq!(from_client_hello(&[0x16, 0x03, 0x01]), Sni::Incomplete);
    }

    #[test]
    fn an_implausible_record_length_is_refused_rather_than_buffered() {
        // Without the cap a client could make the caller hold megabytes waiting
        // for a handshake that is never coming.
        let mut bytes = vec![RECORD_HANDSHAKE, 0x03, 0x01];
        bytes.extend_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(from_client_hello(&bytes), Sni::NotTls);
    }

    #[test]
    fn a_name_that_is_not_a_host_name_is_dropped() {
        // Bytes off the wire, so the parser is the boundary. A name with a space
        // or a NUL cannot match any rule and is not worth carrying.
        assert!(normalize(b"exa mple.com").is_none());
        assert!(normalize(b"exa\0mple.com").is_none());
        assert!(normalize(b"..").is_none());
        assert!(normalize(b"").is_none());
        assert_eq!(normalize(b"a-b_c.example").as_deref(), Some("a-b_c.example"));
    }
}

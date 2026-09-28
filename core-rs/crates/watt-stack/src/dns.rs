//! Minimal DNS wire codec.
//!
//! The kernel intercepts DNS for two reasons. It answers queries for domains the
//! rule set owns locally, which is how a rule's `ips` list reaches the client
//! without a real resolver involved. And it observes the answers that come back
//! from the real resolver, building the address-to-domain mapping the data plane
//! needs to recognise a flow it only knows the address of.
//!
//! Only what those two jobs need is implemented: questions, resource records, A
//! and AAAA payloads, and a pass-through for anything else. Names are decoded
//! with full compression pointer support, because real answers use it heavily,
//! and are encoded with compression for the same reason it exists: a rule can
//! list hundreds of addresses for one name, and repeating that name in every
//! record is what pushes an answer past what a single datagram can carry. With a
//! pointer each A record costs 16 bytes instead of `12 + name + 4`.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// DNS record type: host address.
pub const TYPE_A: u16 = 1;
/// DNS record type: canonical name.
pub const TYPE_CNAME: u16 = 5;
/// DNS record type: IPv6 host address.
pub const TYPE_AAAA: u16 = 28;
/// DNS record type: OPT, the EDNS0 pseudo record.
pub const TYPE_OPT: u16 = 41;

/// DNS class: internet.
pub const CLASS_IN: u16 = 1;

/// Receive size the kernel advertises in its own EDNS0 records.
///
/// 4096 is the value every resolver supports and keeps an answer inside a single
/// datagram for all but the largest zones.
pub const EDNS_UDP_SIZE: u16 = 4096;

/// Response code: no error.
pub const RCODE_NOERROR: u8 = 0;
/// Response code: the name does not exist.
pub const RCODE_NXDOMAIN: u8 = 3;
/// Response code: the server failed to process the query.
pub const RCODE_SERVFAIL: u8 = 2;
/// Response code: the server does not support the requested operation.
pub const RCODE_NOTIMP: u8 = 4;

/// Flag bit: the message is a response.
pub const FLAG_QR: u16 = 0x8000;
/// Flag bit: authoritative answer.
pub const FLAG_AA: u16 = 0x0400;
/// Flag bit: the response was truncated because it did not fit.
///
/// The relay sets this when a reply is too large for the tunnel MTU. A resolver
/// that truncates is telling the client to retry over TCP, which the relay also
/// carries, so the datagram is not lost — only delayed by one round trip.
pub const FLAG_TC: u16 = 0x0200;
/// Flag bit: recursion desired.
pub const FLAG_RD: u16 = 0x0100;
/// Flag bit: recursion available.
pub const FLAG_RA: u16 = 0x0080;

/// Why a DNS message could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsError {
    /// The message is shorter than its own header.
    TooShort,
    /// A name ran past the end of the message.
    NameOutOfBounds,
    /// A compression pointer formed a loop, or pointed forwards.
    BadPointer,
    /// A label is longer than the 63-byte limit.
    LabelTooLong,
    /// A name is longer than the 255-byte limit.
    NameTooLong,
    /// A section declared more records than the message contains.
    Truncated,
    /// A record's length field disagrees with the available bytes.
    BadRecordLength,
}

impl std::fmt::Display for DnsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            DnsError::TooShort => "message shorter than its header",
            DnsError::NameOutOfBounds => "name runs past the end of the message",
            DnsError::BadPointer => "compression pointer loops or points forwards",
            DnsError::LabelTooLong => "label longer than 63 bytes",
            DnsError::NameTooLong => "name longer than 255 bytes",
            DnsError::Truncated => "section declares more records than are present",
            DnsError::BadRecordLength => "record length disagrees with the remaining bytes",
        };
        f.write_str(text)
    }
}

impl std::error::Error for DnsError {}

/// One entry in the question section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// Lowercase name without a trailing dot.
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
}

/// The payload of a resource record the kernel understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordData {
    A(Ipv4Addr),
    Aaaa(Ipv6Addr),
    Cname(String),
    /// Anything else, kept verbatim so it can be re-emitted.
    Other(Vec<u8>),
}

/// One resource record.
///
/// The class field is kept rather than assumed. For every ordinary record it is
/// [`CLASS_IN`], but for an EDNS0 `OPT` record it carries the sender's UDP
/// payload size, which is how a client tells a resolver how large an answer it
/// can receive. Dropping it silently turns a valid OPT record into a malformed
/// one, so the field is carried through the round trip intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub rtype: u16,
    pub class: u16,
    pub ttl: u32,
    pub data: RecordData,
}

impl Record {
    /// The address a record carries, when it is an address record.
    pub fn address(&self) -> Option<IpAddr> {
        match &self.data {
            RecordData::A(addr) => Some(IpAddr::V4(*addr)),
            RecordData::Aaaa(addr) => Some(IpAddr::V6(*addr)),
            _ => None,
        }
    }
}

/// A decoded DNS message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    /// Raw flag word, so unknown bits survive a re-encode.
    pub flags: u16,
    pub questions: Vec<Question>,
    pub answers: Vec<Record>,
    pub authorities: Vec<Record>,
    pub additionals: Vec<Record>,
}

impl Message {
    /// True when the message is a query.
    pub fn is_query(&self) -> bool {
        self.flags & FLAG_QR == 0
    }

    /// The response code from the flag word.
    pub fn rcode(&self) -> u8 {
        (self.flags & 0x000f) as u8
    }

    /// The opcode from the flag word.
    pub fn opcode(&self) -> u8 {
        ((self.flags >> 11) & 0x000f) as u8
    }

    /// The first question, which is the one the kernel answers.
    pub fn first_question(&self) -> Option<&Question> {
        self.questions.first()
    }

    /// The UDP payload size advertised through EDNS0, when present.
    ///
    /// RFC 6891 puts this in the class field of the `OPT` pseudo record, not in
    /// its data. The data of an `OPT` record carries the EDNS options, each with
    /// its own code and length, so reading a size out of it would be reading an
    /// option header.
    pub fn edns_udp_size(&self) -> Option<u16> {
        self.additionals
            .iter()
            .find(|record| record.rtype == TYPE_OPT)
            .map(|record| record.class)
    }

    /// All addresses carried by the answer section, following CNAME chains by
    /// simply collecting every address record present.
    pub fn answer_addresses(&self) -> Vec<IpAddr> {
        self.answers.iter().filter_map(Record::address).collect()
    }

    /// Decode a message.
    pub fn parse(buf: &[u8]) -> Result<Self, DnsError> {
        if buf.len() < 12 {
            return Err(DnsError::TooShort);
        }
        let id = read_u16(buf, 0)?;
        let flags = read_u16(buf, 2)?;
        let qdcount = read_u16(buf, 4)? as usize;
        let ancount = read_u16(buf, 6)? as usize;
        let nscount = read_u16(buf, 8)? as usize;
        let arcount = read_u16(buf, 10)? as usize;

        let mut cursor = 12usize;
        let mut questions = Vec::with_capacity(qdcount.min(8));
        for _ in 0..qdcount {
            let (name, next) = read_name(buf, cursor)?;
            cursor = next;
            let qtype = read_u16(buf, cursor)?;
            let qclass = read_u16(buf, cursor + 2)?;
            cursor += 4;
            questions.push(Question {
                name,
                qtype,
                qclass,
            });
        }

        let answers = read_records(buf, &mut cursor, ancount)?;
        let authorities = read_records(buf, &mut cursor, nscount)?;
        let additionals = read_records(buf, &mut cursor, arcount)?;

        Ok(Message {
            id,
            flags,
            questions,
            answers,
            authorities,
            additionals,
        })
    }

    /// Encode a message.
    ///
    /// Names already present earlier in the message are written as pointers to
    /// their first occurrence (RFC 1035 §4.1.4). Every answer to a question
    /// carries that question's name, so in a response with many records this
    /// turns a repeated name into two bytes each.
    pub fn encode(&self) -> Result<Vec<u8>, DnsError> {
        let mut out = Vec::with_capacity(64 + self.answers.len() * 16);
        let mut names: HashMap<&str, usize> = HashMap::new();

        out.extend_from_slice(&self.id.to_be_bytes());
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(&(self.questions.len() as u16).to_be_bytes());
        out.extend_from_slice(&(self.answers.len() as u16).to_be_bytes());
        out.extend_from_slice(&(self.authorities.len() as u16).to_be_bytes());
        out.extend_from_slice(&(self.additionals.len() as u16).to_be_bytes());

        for question in &self.questions {
            write_compressed_name(&mut out, &question.name, &mut names)?;
            out.extend_from_slice(&question.qtype.to_be_bytes());
            out.extend_from_slice(&question.qclass.to_be_bytes());
        }
        for record in &self.answers {
            write_compressed_record(&mut out, record, &mut names)?;
        }
        for record in &self.authorities {
            write_compressed_record(&mut out, record, &mut names)?;
        }
        for record in &self.additionals {
            write_compressed_record(&mut out, record, &mut names)?;
        }
        Ok(out)
    }
}

/// Build a response to `query`.
///
/// `answers` is used verbatim, the question section is echoed, and the recursion
/// desired bit is preserved so the client does not treat the reply as unsolicited.
pub fn build_response(query: &Message, answers: Vec<Record>, rcode: u8) -> Result<Vec<u8>, DnsError> {
    let mut flags = FLAG_QR | FLAG_AA;
    if query.flags & FLAG_RD != 0 {
        flags |= FLAG_RD;
    }
    // Authoritative answers need no recursion, but advertising it keeps stub
    // resolvers from immediately retrying against another server.
    flags |= FLAG_RA;
    flags |= u16::from(rcode & 0x0f);

    let mut response = Message {
        id: query.id,
        flags,
        questions: query.questions.clone(),
        answers,
        authorities: Vec::new(),
        additionals: Vec::new(),
    };

    // Echo an EDNS0 OPT record when the client asked for one, advertising a
    // 4096-byte receive size. Without this a client that used EDNS0 may treat the
    // answer as malformed.
    if query.edns_udp_size().is_some() {
        response.additionals.push(Record {
            name: String::new(),
            rtype: TYPE_OPT,
            // RFC 6891: the class field is the advertised receive size.
            class: EDNS_UDP_SIZE,
            // Extended rcode and EDNS version, both zero.
            ttl: 0,
            // No options.
            data: RecordData::Other(Vec::new()),
        });
    }

    response.encode()
}

/// Build an address response whose answer section fits within `limit` bytes.
///
/// Returns the encoded response and the number of addresses it carries.
///
/// When every address does not fit, the answer is trimmed to the ones that do
/// rather than replaced by a truncation flag. RFC 1035 truncation asks the
/// client to retry over TCP; this kernel answers queries that never leave the
/// device, so there is no TCP retry to make, and a flag would turn a rule with
/// hundreds of addresses into an answer with none. The addresses are already in
/// the order the selector ranked them, so the ones kept are the ones worth
/// keeping.
///
/// Returns `None` when not even a single record fits.
pub fn build_response_fitting(
    query: &Message,
    name: &str,
    addresses: &[IpAddr],
    ttl: u32,
    limit: usize,
) -> Option<(Vec<u8>, usize)> {
    let build = |count: usize| {
        build_response(query, address_records(name, &addresses[..count], ttl), RCODE_NOERROR)
    };

    if addresses.is_empty() {
        return None;
    }
    let all = build(addresses.len()).ok()?;
    if all.len() <= limit {
        return Some((all, addresses.len()));
    }

    // Records share one compressed name, so they are all the same size. Measure
    // that size from a response that has none, then take as many as fit.
    let overhead = build(0).ok()?.len();
    let per_record = ((all.len() - overhead) / addresses.len()).max(1);
    let mut count = limit.saturating_sub(overhead) / per_record;
    count = count.clamp(1, addresses.len());

    // The arithmetic is an estimate — an OPT record or an unusual name can shift
    // it — so step down until it really fits rather than trusting the number.
    while count > 1 {
        if let Ok(response) = build(count) {
            if response.len() <= limit {
                return Some((response, count));
            }
        }
        count -= 1;
    }
    let one = build(1).ok()?;
    (one.len() <= limit).then_some((one, 1))
}

/// Build address records for `name`.
pub fn address_records(name: &str, addresses: &[IpAddr], ttl: u32) -> Vec<Record> {
    addresses
        .iter()
        .map(|addr| match addr {
            IpAddr::V4(v4) => Record {
                name: name.to_string(),
                rtype: TYPE_A,
                class: CLASS_IN,
                ttl,
                data: RecordData::A(*v4),
            },
            IpAddr::V6(v6) => Record {
                name: name.to_string(),
                rtype: TYPE_AAAA,
                class: CLASS_IN,
                ttl,
                data: RecordData::Aaaa(*v6),
            },
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Primitive readers and writers
// ---------------------------------------------------------------------------

fn read_u16(buf: &[u8], offset: usize) -> Result<u16, DnsError> {
    let bytes = buf.get(offset..offset + 2).ok_or(DnsError::Truncated)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(buf: &[u8], offset: usize) -> Result<u32, DnsError> {
    let bytes = buf.get(offset..offset + 4).ok_or(DnsError::Truncated)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Read a possibly compressed name starting at `offset`.
///
/// Returns the name and the offset of the first byte after it. A compression
/// pointer terminates the name at the current position; the returned offset is
/// the one just past the pointer, not past its target.
fn read_name(buf: &[u8], offset: usize) -> Result<(String, usize), DnsError> {
    let mut name = String::new();
    let mut cursor = offset;
    let mut after: Option<usize> = None;
    // A pointer may only jump backwards, so this bound is generous but finite.
    let mut budget = buf.len();

    loop {
        let length = *buf.get(cursor).ok_or(DnsError::NameOutOfBounds)?;
        match length & 0xc0 {
            0x00 => {
                if length == 0 {
                    cursor += 1;
                    break;
                }
                let label_len = length as usize;
                let label = buf
                    .get(cursor + 1..cursor + 1 + label_len)
                    .ok_or(DnsError::NameOutOfBounds)?;
                if !name.is_empty() {
                    name.push('.');
                }
                for byte in label {
                    name.push((*byte as char).to_ascii_lowercase());
                }
                cursor += 1 + label_len;
            }
            0xc0 => {
                let second = *buf.get(cursor + 1).ok_or(DnsError::NameOutOfBounds)?;
                let target = (((length & 0x3f) as usize) << 8) | second as usize;
                if target >= cursor {
                    return Err(DnsError::BadPointer);
                }
                if after.is_none() {
                    after = Some(cursor + 2);
                }
                cursor = target;
            }
            _ => return Err(DnsError::BadPointer),
        }
        if name.len() > 255 {
            return Err(DnsError::NameTooLong);
        }
        budget = budget.checked_sub(1).ok_or(DnsError::BadPointer)?;
        if budget == 0 {
            return Err(DnsError::BadPointer);
        }
    }

    Ok((name, after.unwrap_or(cursor)))
}

fn read_records(buf: &[u8], cursor: &mut usize, count: usize) -> Result<Vec<Record>, DnsError> {
    let mut records = Vec::with_capacity(count.min(32));
    for _ in 0..count {
        let (name, next) = read_name(buf, *cursor)?;
        *cursor = next;
        let rtype = read_u16(buf, *cursor)?;
        let class = read_u16(buf, *cursor + 2)?;
        let ttl = read_u32(buf, *cursor + 4)?;
        let rdlength = read_u16(buf, *cursor + 8)? as usize;
        *cursor += 10;
        let rdata = buf
            .get(*cursor..*cursor + rdlength)
            .ok_or(DnsError::BadRecordLength)?;
        *cursor += rdlength;

        let data = match (rtype, class, rdlength) {
            (TYPE_A, CLASS_IN, 4) => {
                RecordData::A(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]))
            }
            (TYPE_AAAA, CLASS_IN, 16) => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(rdata);
                RecordData::Aaaa(Ipv6Addr::from(octets))
            }
            (TYPE_CNAME, _, _) => {
                let (target, _) = read_name(buf, *cursor - rdlength)?;
                RecordData::Cname(target)
            }
            _ => RecordData::Other(rdata.to_vec()),
        };
        records.push(Record {
            name,
            rtype,
            class,
            ttl,
            data,
        });
    }
    Ok(records)
}

fn write_name(out: &mut Vec<u8>, name: &str) -> Result<(), DnsError> {
    if name.is_empty() {
        out.push(0);
        return Ok(());
    }
    let mut written = 0usize;
    for label in name.split('.') {
        if label.is_empty() {
            continue;
        }
        if label.len() > 63 {
            return Err(DnsError::LabelTooLong);
        }
        written += 1 + label.len();
        if written + 1 > 255 {
            return Err(DnsError::NameTooLong);
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    Ok(())
}

/// Write a name, or a pointer to it when the same name was written earlier.
///
/// A pointer is only usable below offset 0x4000, which is also the cap on a
/// pointer's own value, so names past that point are simply written out again.
fn write_compressed_name<'a>(
    out: &mut Vec<u8>,
    name: &'a str,
    names: &mut HashMap<&'a str, usize>,
) -> Result<(), DnsError> {
    if let Some(offset) = names.get(name).copied() {
        if offset < 0x4000 {
            out.extend_from_slice(&(0xc000u16 | (offset as u16)).to_be_bytes());
            return Ok(());
        }
    }
    let offset = out.len();
    if offset < 0x4000 {
        names.insert(name, offset);
    }
    write_name(out, name)
}

fn write_compressed_record<'a>(
    out: &mut Vec<u8>,
    record: &'a Record,
    names: &mut HashMap<&'a str, usize>,
) -> Result<(), DnsError> {
    write_compressed_name(out, &record.name, names)?;
    out.extend_from_slice(&record.rtype.to_be_bytes());
    out.extend_from_slice(&record.class.to_be_bytes());
    out.extend_from_slice(&record.ttl.to_be_bytes());
    match &record.data {
        RecordData::A(addr) => {
            out.extend_from_slice(&4u16.to_be_bytes());
            out.extend_from_slice(&addr.octets());
        }
        RecordData::Aaaa(addr) => {
            out.extend_from_slice(&16u16.to_be_bytes());
            out.extend_from_slice(&addr.octets());
        }
        RecordData::Cname(target) => {
            let mut encoded = Vec::new();
            write_name(&mut encoded, target)?;
            out.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
            out.extend_from_slice(&encoded);
        }
        RecordData::Other(raw) => {
            if raw.len() > u16::MAX as usize {
                return Err(DnsError::BadRecordLength);
            }
            out.extend_from_slice(&(raw.len() as u16).to_be_bytes());
            out.extend_from_slice(raw);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0x1234u16.to_be_bytes());
        out.extend_from_slice(&FLAG_RD.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        write_name(&mut out, name).unwrap();
        out.extend_from_slice(&qtype.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out
    }

    #[test]
    fn parses_a_query() {
        let raw = query("Raw.GitHubUserContent.com", TYPE_A);
        let message = Message::parse(&raw).unwrap();
        assert!(message.is_query());
        assert_eq!(message.id, 0x1234);
        assert_eq!(message.rcode(), RCODE_NOERROR);
        assert_eq!(message.questions.len(), 1);
        // Names are lowercased during decoding so the rule index can match them.
        assert_eq!(message.questions[0].name, "raw.githubusercontent.com");
        assert_eq!(message.questions[0].qtype, TYPE_A);
        assert_eq!(message.questions[0].qclass, CLASS_IN);
        assert!(message.answers.is_empty());
    }

    #[test]
    fn round_trips_through_encode() {
        let raw = query("example.com", TYPE_AAAA);
        let message = Message::parse(&raw).unwrap();
        let reencoded = message.encode().unwrap();
        let reparsed = Message::parse(&reencoded).unwrap();
        assert_eq!(message, reparsed);
    }

    #[test]
    fn answers_a_query_with_address_records() {
        let raw = query("cdn.example.com", TYPE_A);
        let parsed = Message::parse(&raw).unwrap();

        let addresses: Vec<IpAddr> = vec![
            "203.0.113.10".parse().unwrap(),
            "203.0.113.20".parse().unwrap(),
        ];
        let response_bytes =
            build_response(&parsed, address_records("cdn.example.com", &addresses, 60), RCODE_NOERROR).unwrap();

        let response = Message::parse(&response_bytes).unwrap();
        assert!(!response.is_query());
        assert_eq!(response.id, 0x1234);
        assert_eq!(response.rcode(), RCODE_NOERROR);
        assert_eq!(response.questions, parsed.questions, "question must be echoed");
        assert_eq!(response.answer_addresses(), addresses);
        assert_eq!(response.answers[0].ttl, 60);
        assert!(
            response.flags & FLAG_RD != 0,
            "recursion desired must be preserved"
        );
    }

    #[test]
    fn an_answer_repeats_the_name_as_a_pointer_rather_than_in_full() {
        // A rule can list hundreds of addresses for one name. Writing the name
        // out in every record is what pushes such an answer past the MTU, so the
        // whole point of compression here is size, not tidiness.
        let parsed = Message::parse(&query("example.com", TYPE_A)).unwrap();
        let addresses: Vec<IpAddr> = (0..40u8)
            .map(|i| IpAddr::V4(Ipv4Addr::new(203, 0, 113, i)))
            .collect();

        let one = build_response(&parsed, address_records("example.com", &addresses[..1], 60), 0)
            .unwrap();
        let forty =
            build_response(&parsed, address_records("example.com", &addresses, 60), 0).unwrap();

        let per_record = (forty.len() - one.len()) / (addresses.len() - 1);
        assert_eq!(
            per_record, 16,
            "a compressed A record is 2 name + 2 type + 2 class + 4 ttl + 2 length + 4 address"
        );
    }

    #[test]
    fn an_oversized_answer_is_trimmed_rather_than_emptied() {
        // Truncating would set TC and hand the client nothing, and there is no
        // TCP retry behind this answer — it never leaves the device. Real rules
        // list up to 971 addresses for a single name, so this is the common
        // case, not an edge one.
        let parsed = Message::parse(&query("large.example.com", TYPE_A)).unwrap();
        let addresses: Vec<IpAddr> = (0..200u16)
            .map(|i| IpAddr::V4(Ipv4Addr::new(10, 0, (i / 256) as u8, (i % 256) as u8)))
            .collect();

        let (bytes, delivered) =
            build_response_fitting(&parsed, "large.example.com", &addresses, 60, 1452)
                .expect("an answer must survive being too large to carry whole");

        assert!(delivered > 0, "trimming must never produce an empty answer");
        assert!(delivered < addresses.len(), "the answer did not fit whole");
        assert!(bytes.len() <= 1452, "the answer must fit the limit");

        let response = Message::parse(&bytes).unwrap();
        assert_eq!(response.answer_addresses().len(), delivered);
        assert!(
            response.flags & FLAG_TC == 0,
            "a trimmed answer is complete on its own and must not claim truncation"
        );
        // The addresses kept are the first ones, which are the ones the selector
        // ranked highest.
        assert_eq!(response.answer_addresses(), addresses[..delivered].to_vec());
    }

    #[test]
    fn an_answer_that_cannot_fit_one_record_is_refused() {
        let parsed = Message::parse(&query("tight.example.com", TYPE_A)).unwrap();
        let addresses: Vec<IpAddr> = vec!["203.0.113.5".parse().unwrap()];
        assert!(
            build_response_fitting(&parsed, "tight.example.com", &addresses, 60, 40).is_none(),
            "not even one record fits, so there is nothing to send"
        );
    }

    #[test]
    fn builds_negative_responses() {
        let raw = query("missing.example", TYPE_A);
        let parsed = Message::parse(&raw).unwrap();
        let bytes = build_response(&parsed, Vec::new(), RCODE_NXDOMAIN).unwrap();
        let response = Message::parse(&bytes).unwrap();
        assert_eq!(response.rcode(), RCODE_NXDOMAIN);
        assert!(response.answers.is_empty());
    }

    #[test]
    fn reads_compressed_names_in_answers() {
        // Hand build a response whose answer name is a pointer to the question.
        let mut raw = query("example.com", TYPE_A);
        raw[2] = 0x81; // QR + RD
        raw[3] = 0x80; // RA
        raw[6..8].copy_from_slice(&1u16.to_be_bytes()); // ANCOUNT = 1
        raw.extend_from_slice(&[0xc0, 0x0c]); // pointer to offset 12
        raw.extend_from_slice(&TYPE_A.to_be_bytes());
        raw.extend_from_slice(&CLASS_IN.to_be_bytes());
        raw.extend_from_slice(&30u32.to_be_bytes());
        raw.extend_from_slice(&4u16.to_be_bytes());
        raw.extend_from_slice(&[203, 0, 113, 5]);

        let message = Message::parse(&raw).unwrap();
        assert_eq!(message.answers.len(), 1);
        assert_eq!(message.answers[0].name, "example.com");
        assert_eq!(
            message.answers[0].data,
            RecordData::A(Ipv4Addr::new(203, 0, 113, 5))
        );
        assert_eq!(
            message.answer_addresses(),
            vec!["203.0.113.5".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn reads_cname_records() {
        let mut raw = query("www.example.com", TYPE_A);
        raw[2] = 0x81;
        raw[3] = 0x80;
        raw[6..8].copy_from_slice(&2u16.to_be_bytes());
        // CNAME answer, name compressed to the question.
        raw.extend_from_slice(&[0xc0, 0x0c]);
        raw.extend_from_slice(&TYPE_CNAME.to_be_bytes());
        raw.extend_from_slice(&CLASS_IN.to_be_bytes());
        raw.extend_from_slice(&30u32.to_be_bytes());
        let mut target = Vec::new();
        write_name(&mut target, "edge.example.net").unwrap();
        raw.extend_from_slice(&(target.len() as u16).to_be_bytes());
        raw.extend_from_slice(&target);
        // A answer for the canonical name.
        raw.extend_from_slice(&target);
        raw.extend_from_slice(&TYPE_A.to_be_bytes());
        raw.extend_from_slice(&CLASS_IN.to_be_bytes());
        raw.extend_from_slice(&30u32.to_be_bytes());
        raw.extend_from_slice(&4u16.to_be_bytes());
        raw.extend_from_slice(&[198, 51, 100, 9]);

        let message = Message::parse(&raw).unwrap();
        assert_eq!(message.answers.len(), 2);
        assert_eq!(
            message.answers[0].data,
            RecordData::Cname("edge.example.net".to_string())
        );
        assert_eq!(
            message.answer_addresses(),
            vec!["198.51.100.9".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn detects_and_echoes_edns0() {
        let mut raw = query("example.com", TYPE_A);
        raw[10..12].copy_from_slice(&1u16.to_be_bytes()); // ARCOUNT = 1
        raw.push(0); // root name
        raw.extend_from_slice(&TYPE_OPT.to_be_bytes());
        raw.extend_from_slice(&1232u16.to_be_bytes()); // advertised size in class
        raw.extend_from_slice(&0u32.to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());

        let parsed = Message::parse(&raw).unwrap();
        assert_eq!(parsed.edns_udp_size(), Some(1232));

        let bytes = build_response(&parsed, Vec::new(), RCODE_NOERROR).unwrap();
        let response = Message::parse(&bytes).unwrap();
        assert_eq!(response.additionals.len(), 1);
        assert_eq!(response.additionals[0].rtype, TYPE_OPT);
        assert_eq!(response.edns_udp_size(), Some(4096));
    }

    #[test]
    fn rejects_malformed_messages() {
        assert_eq!(Message::parse(&[]), Err(DnsError::TooShort));
        assert_eq!(Message::parse(&[0u8; 11]), Err(DnsError::TooShort));

        // A pointer that points at itself must not loop forever.
        let mut looping = vec![0u8; 12];
        looping[4..6].copy_from_slice(&1u16.to_be_bytes());
        looping.extend_from_slice(&[0xc0, 0x0c]);
        looping.extend_from_slice(&TYPE_A.to_be_bytes());
        looping.extend_from_slice(&CLASS_IN.to_be_bytes());
        assert_eq!(Message::parse(&looping), Err(DnsError::BadPointer));

        // A record whose length field exceeds the message.
        let mut truncated = query("example.com", TYPE_A);
        truncated[6..8].copy_from_slice(&1u16.to_be_bytes());
        truncated.extend_from_slice(&[0xc0, 0x0c]);
        truncated.extend_from_slice(&TYPE_A.to_be_bytes());
        truncated.extend_from_slice(&CLASS_IN.to_be_bytes());
        truncated.extend_from_slice(&30u32.to_be_bytes());
        truncated.extend_from_slice(&40u16.to_be_bytes());
        truncated.extend_from_slice(&[1, 2, 3]);
        assert_eq!(Message::parse(&truncated), Err(DnsError::BadRecordLength));
    }

    #[test]
    fn rejects_an_over_long_label() {
        let long_label = "a".repeat(64);
        let mut out = Vec::new();
        assert_eq!(write_name(&mut out, &long_label), Err(DnsError::LabelTooLong));
    }

    #[test]
    fn the_record_class_survives_a_round_trip() {
        // An OPT record's class is not `CLASS_IN`; it is the advertised receive
        // size. Re-encoding with a hard coded class would corrupt it.
        let record = Record {
            name: String::new(),
            rtype: TYPE_OPT,
            class: 1232,
            ttl: 0,
            data: RecordData::Other(Vec::new()),
        };
        let mut bytes = Vec::new();
        write_compressed_record(&mut bytes, &record, &mut HashMap::new()).unwrap();

        let message = Message {
            id: 1,
            flags: 0,
            questions: Vec::new(),
            answers: Vec::new(),
            authorities: Vec::new(),
            additionals: vec![record.clone()],
        };
        let encoded = message.encode().unwrap();
        let decoded = Message::parse(&encoded).unwrap();
        assert_eq!(decoded.additionals, vec![record]);
        assert_eq!(decoded.edns_udp_size(), Some(1232));
    }
}

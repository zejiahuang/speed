//! IPv4/IPv6, UDP and TCP wire codec.
//!
//! This module is hand written rather than delegated to a dependency for one
//! reason: the kernel needs to inspect and synthesise packets on paths where a
//! full stack is the wrong tool — recognising a SYN before the stack sees it,
//! answering DNS locally, emitting a reset for a port nobody listens on, and
//! rewriting a UDP datagram's destination. Doing it here keeps those paths
//! explicit, allocation-light and unit testable, and leaves the dependency to do
//! the one thing it is genuinely hard to replace: the TCP state machine.
//!
//! Every parser is checked: a truncated or self-contradictory packet is rejected
//! rather than being read out of bounds.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// IP protocol number for ICMP.
pub const PROTO_ICMP: u8 = 1;
/// IP protocol number for TCP.
pub const PROTO_TCP: u8 = 6;
/// IP protocol number for UDP.
pub const PROTO_UDP: u8 = 17;
/// IP protocol number for ICMPv6.
pub const PROTO_ICMPV6: u8 = 58;

/// Minimum IPv4 header length in bytes.
const IPV4_MIN_HEADER: usize = 20;
/// IPv6 header length in bytes; IPv6 has no options in the base header.
const IPV6_HEADER: usize = 40;
/// UDP header length in bytes.
const UDP_HEADER: usize = 8;
/// Minimum TCP header length in bytes.
const TCP_MIN_HEADER: usize = 20;

/// Why a packet could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketError {
    /// The buffer is shorter than the header it claims to contain.
    TooShort,
    /// The IP version nibble is neither 4 nor 6.
    UnsupportedVersion(u8),
    /// `ihl` is smaller than the mandatory IPv4 header.
    BadHeaderLength,
    /// The declared total length exceeds the buffer.
    TruncatedPayload,
    /// The declared total length is smaller than the header.
    BadTotalLength,
    /// The header is not a UDP datagram.
    NotUdp,
    /// The header is not a TCP segment.
    NotTcp,
    /// The UDP length field disagrees with the IP payload length.
    BadUdpLength,
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PacketError::TooShort => f.write_str("packet is shorter than its header"),
            PacketError::UnsupportedVersion(v) => write!(f, "unsupported IP version {v}"),
            PacketError::BadHeaderLength => f.write_str("invalid IP header length"),
            PacketError::TruncatedPayload => f.write_str("packet is shorter than its declared length"),
            PacketError::BadTotalLength => f.write_str("declared length is smaller than the header"),
            PacketError::NotUdp => f.write_str("packet does not carry UDP"),
            PacketError::NotTcp => f.write_str("packet does not carry TCP"),
            PacketError::BadUdpLength => f.write_str("UDP length disagrees with the IP payload"),
        }
    }
}

impl std::error::Error for PacketError {}

/// Which IP version a packet uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpVersion {
    V4,
    V6,
}

/// Decoded IP header, common to both versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpHeader {
    pub version: IpVersion,
    pub src: IpAddr,
    pub dst: IpAddr,
    /// Transport protocol number.
    pub protocol: u8,
    /// Header length in bytes, including IPv4 options.
    pub header_len: usize,
    /// Total packet length in bytes, clamped to the buffer length.
    pub total_len: usize,
}

impl IpHeader {
    /// Bytes of transport payload that follow the IP header.
    pub fn payload_len(&self) -> usize {
        self.total_len.saturating_sub(self.header_len)
    }

    /// The transport payload, when the packet buffer is long enough.
    pub fn payload<'a>(&self, packet: &'a [u8]) -> Option<&'a [u8]> {
        packet.get(self.header_len..self.total_len)
    }

    /// True when the packet is a fragment other than the first one.
    ///
    /// Only the first fragment of a datagram carries the transport header, so the
    /// kernel forwards later fragments untouched instead of trying to parse them.
    pub fn is_later_fragment(&self, packet: &[u8]) -> bool {
        if self.version != IpVersion::V4 || packet.len() < 8 {
            return false;
        }
        let flags_and_offset = u16::from_be_bytes([packet[6], packet[7]]);
        // Bit 13 is MF; the low 13 bits are the fragment offset in 8-byte units.
        let more_fragments = flags_and_offset & 0x2000 != 0;
        let fragment_offset = flags_and_offset & 0x1fff;
        more_fragments || fragment_offset != 0
    }
}

/// Parse the IP header of `packet`.
pub fn parse_ip_header(packet: &[u8]) -> Result<IpHeader, PacketError> {
    let first = *packet.first().ok_or(PacketError::TooShort)?;
    match first >> 4 {
        4 => parse_ipv4_header(packet),
        6 => parse_ipv6_header(packet),
        other => Err(PacketError::UnsupportedVersion(other)),
    }
}

fn parse_ipv4_header(packet: &[u8]) -> Result<IpHeader, PacketError> {
    if packet.len() < IPV4_MIN_HEADER {
        return Err(PacketError::TooShort);
    }
    let header_len = ((packet[0] & 0x0f) as usize) * 4;
    if header_len < IPV4_MIN_HEADER || header_len > packet.len() {
        return Err(PacketError::BadHeaderLength);
    }
    let declared = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if declared < header_len {
        return Err(PacketError::BadTotalLength);
    }
    // A TUN device never hands over a packet longer than it declared, but a
    // shorter buffer means the packet was truncated somewhere.
    let total_len = declared.min(packet.len());
    if total_len < header_len {
        return Err(PacketError::TruncatedPayload);
    }
    Ok(IpHeader {
        version: IpVersion::V4,
        src: IpAddr::V4(Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15])),
        dst: IpAddr::V4(Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19])),
        protocol: packet[9],
        header_len,
        total_len,
    })
}

fn parse_ipv6_header(packet: &[u8]) -> Result<IpHeader, PacketError> {
    if packet.len() < IPV6_HEADER {
        return Err(PacketError::TooShort);
    }
    let declared = u16::from_be_bytes([packet[4], packet[5]]) as usize + IPV6_HEADER;
    let total_len = declared.min(packet.len());
    if total_len < IPV6_HEADER {
        return Err(PacketError::TruncatedPayload);
    }
    let mut src = [0u8; 16];
    src.copy_from_slice(&packet[8..24]);
    let mut dst = [0u8; 16];
    dst.copy_from_slice(&packet[24..40]);
    Ok(IpHeader {
        version: IpVersion::V6,
        src: IpAddr::V6(Ipv6Addr::from(src)),
        dst: IpAddr::V6(Ipv6Addr::from(dst)),
        // Extension headers are not walked; a packet whose next-header is an
        // extension header is treated as an opaque payload and forwarded as is.
        protocol: packet[6],
        header_len: IPV6_HEADER,
        total_len,
    })
}

/// A decoded UDP datagram, borrowing its payload from the packet buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpDatagram<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: &'a [u8],
}

/// Parse the UDP datagram inside an already decoded IP packet.
pub fn parse_udp<'a>(packet: &'a [u8], header: &IpHeader) -> Result<UdpDatagram<'a>, PacketError> {
    if header.protocol != PROTO_UDP {
        return Err(PacketError::NotUdp);
    }
    let body = header.payload(packet).ok_or(PacketError::TruncatedPayload)?;
    if body.len() < UDP_HEADER {
        return Err(PacketError::TooShort);
    }
    let src_port = u16::from_be_bytes([body[0], body[1]]);
    let dst_port = u16::from_be_bytes([body[2], body[3]]);
    let declared = u16::from_be_bytes([body[4], body[5]]) as usize;
    if declared < UDP_HEADER || declared > body.len() {
        return Err(PacketError::BadUdpLength);
    }
    Ok(UdpDatagram {
        src_port,
        dst_port,
        payload: &body[UDP_HEADER..declared],
    })
}

/// TCP control flags the kernel cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TcpFlags {
    pub fin: bool,
    pub syn: bool,
    pub rst: bool,
    pub psh: bool,
    pub ack: bool,
    pub urg: bool,
}

impl TcpFlags {
    /// Encode the flags into the single control byte the TCP header carries.
    pub fn to_byte(self) -> u8 {
        let mut byte = 0u8;
        if self.fin {
            byte |= 0x01;
        }
        if self.syn {
            byte |= 0x02;
        }
        if self.rst {
            byte |= 0x04;
        }
        if self.psh {
            byte |= 0x08;
        }
        if self.ack {
            byte |= 0x10;
        }
        if self.urg {
            byte |= 0x20;
        }
        byte
    }

    /// Every flag cleared, which is the starting point for a hand built segment.
    pub fn none() -> Self {
        Self::default()
    }
}

/// A decoded TCP segment, borrowing its payload from the packet buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpSegment<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: TcpFlags,
    pub window: u16,
    /// Bytes of TCP header, including options.
    ///
    /// Read from the data offset field rather than assumed: a segment carrying
    /// timestamps or a maximum segment size has a longer header than the minimum,
    /// and anything counting header bytes has to know which it is looking at.
    pub header_len: usize,
    pub payload: &'a [u8],
}

impl TcpSegment<'_> {
    /// True for a bare connection request, which is what the kernel watches for
    /// to decide whether a listener needs to exist for a destination port.
    pub fn is_initial_syn(&self) -> bool {
        self.flags.syn && !self.flags.ack
    }
}

/// Parse the TCP segment inside an already decoded IP packet.
pub fn parse_tcp<'a>(packet: &'a [u8], header: &IpHeader) -> Result<TcpSegment<'a>, PacketError> {
    if header.protocol != PROTO_TCP {
        return Err(PacketError::NotTcp);
    }
    let body = header.payload(packet).ok_or(PacketError::TruncatedPayload)?;
    if body.len() < TCP_MIN_HEADER {
        return Err(PacketError::TooShort);
    }
    let data_offset = ((body[12] >> 4) as usize) * 4;
    if data_offset < TCP_MIN_HEADER || data_offset > body.len() {
        return Err(PacketError::BadHeaderLength);
    }
    let flags_byte = body[13];
    Ok(TcpSegment {
        src_port: u16::from_be_bytes([body[0], body[1]]),
        dst_port: u16::from_be_bytes([body[2], body[3]]),
        seq: u32::from_be_bytes([body[4], body[5], body[6], body[7]]),
        ack: u32::from_be_bytes([body[8], body[9], body[10], body[11]]),
        flags: TcpFlags {
            fin: flags_byte & 0x01 != 0,
            syn: flags_byte & 0x02 != 0,
            rst: flags_byte & 0x04 != 0,
            psh: flags_byte & 0x08 != 0,
            ack: flags_byte & 0x10 != 0,
            urg: flags_byte & 0x20 != 0,
        },
        window: u16::from_be_bytes([body[14], body[15]]),
        header_len: data_offset,
        payload: &body[data_offset..],
    })
}

// ---------------------------------------------------------------------------
// Checksums
// ---------------------------------------------------------------------------

/// Add `data` to a running one's complement sum, as 16-bit big-endian words.
fn add_words(data: &[u8], mut sum: u32) -> u32 {
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    if let [last] = chunks.remainder() {
        sum += u32::from(*last) << 8;
    }
    sum
}

/// Fold a running sum into the final one's complement checksum.
fn fold(sum: u32) -> u16 {
    let mut sum = sum;
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// IPv4 header checksum, computed with the checksum field treated as zero.
fn ipv4_header_checksum(header: &[u8]) -> u16 {
    fold(add_words(header, 0))
}

/// Transport checksum over the pseudo header plus the segment.
///
/// `zero` is a byte that participates in the pseudo header; for IPv4 it is the
/// reserved zero byte and for IPv6 it is the upper byte of the length field.
fn transport_checksum(
    src: IpAddr,
    dst: IpAddr,
    protocol: u8,
    segment: &[u8],
) -> u16 {
    let mut sum = 0u32;
    match (src, dst) {
        (IpAddr::V4(src), IpAddr::V4(dst)) => {
            sum = add_words(&src.octets(), sum);
            sum = add_words(&dst.octets(), sum);
            sum += u32::from(protocol);
            sum += segment.len() as u32;
        }
        (IpAddr::V6(src), IpAddr::V6(dst)) => {
            sum = add_words(&src.octets(), sum);
            sum = add_words(&dst.octets(), sum);
            // Upper-layer packet length, 32 bits, followed by three zero bytes
            // and the next-header value.
            sum += (segment.len() as u32) & 0xffff;
            sum += ((segment.len() as u32) >> 16) & 0xffff;
            sum += u32::from(protocol);
        }
        // A mixed pair cannot happen: the kernel never crosses address families.
        _ => return 0,
    }
    fold(add_words(segment, sum))
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Write an IP header for `payload_len` bytes of transport data into `out`.
fn write_ip_header(
    out: &mut Vec<u8>,
    src: IpAddr,
    dst: IpAddr,
    protocol: u8,
    payload_len: usize,
) -> Result<(), PacketError> {
    match (src, dst) {
        (IpAddr::V4(src), IpAddr::V4(dst)) => {
            let total = IPV4_MIN_HEADER + payload_len;
            if total > u16::MAX as usize {
                return Err(PacketError::BadTotalLength);
            }
            let start = out.len();
            out.extend_from_slice(&[0x45, 0x00]);
            out.extend_from_slice(&(total as u16).to_be_bytes());
            // Identification and flags: zero, and the "don't fragment" bit set so
            // that nothing downstream tries to split a packet the kernel built.
            out.extend_from_slice(&[0x00, 0x00, 0x40, 0x00]);
            out.push(64); // TTL
            out.push(protocol);
            out.extend_from_slice(&[0x00, 0x00]); // checksum placeholder
            out.extend_from_slice(&src.octets());
            out.extend_from_slice(&dst.octets());
            let checksum = ipv4_header_checksum(&out[start..start + IPV4_MIN_HEADER]);
            out[start + 10..start + 12].copy_from_slice(&checksum.to_be_bytes());
            Ok(())
        }
        (IpAddr::V6(src), IpAddr::V6(dst)) => {
            if payload_len > u16::MAX as usize {
                return Err(PacketError::BadTotalLength);
            }
            out.extend_from_slice(&[0x60, 0x00, 0x00, 0x00]);
            out.extend_from_slice(&(payload_len as u16).to_be_bytes());
            out.push(protocol);
            out.push(64); // hop limit
            out.extend_from_slice(&src.octets());
            out.extend_from_slice(&dst.octets());
            Ok(())
        }
        _ => Err(PacketError::UnsupportedVersion(0)),
    }
}

/// Build a complete UDP/IP packet carrying `payload`.
pub fn build_udp_packet(
    src: IpAddr,
    dst: IpAddr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Result<Vec<u8>, PacketError> {
    let udp_len = UDP_HEADER + payload.len();
    if udp_len > u16::MAX as usize {
        return Err(PacketError::BadUdpLength);
    }
    let mut out = Vec::with_capacity(IPV4_MIN_HEADER + udp_len);
    write_ip_header(&mut out, src, dst, PROTO_UDP, udp_len)?;

    let segment_start = out.len();
    out.extend_from_slice(&src_port.to_be_bytes());
    out.extend_from_slice(&dst_port.to_be_bytes());
    out.extend_from_slice(&(udp_len as u16).to_be_bytes());
    out.extend_from_slice(&[0x00, 0x00]); // checksum placeholder
    out.extend_from_slice(payload);

    let checksum = transport_checksum(src, dst, PROTO_UDP, &out[segment_start..]);
    // A zero checksum means "not computed" for IPv4, so a computed zero is sent
    // as all ones instead. IPv6 has no such escape hatch, but the same
    // substitution is valid there too.
    let checksum = if checksum == 0 { 0xffff } else { checksum };
    out[segment_start + 6..segment_start + 8].copy_from_slice(&checksum.to_be_bytes());
    Ok(out)
}

/// Build a TCP segment, optionally carrying a payload.
///
/// The kernel itself only ever emits control segments — resets, and the
/// handshake the userspace stack generates. A payload capable builder exists so
/// tests can drive that stack with hand crafted packets instead of needing a
/// second machine on the far side of the tunnel.
#[allow(clippy::too_many_arguments)]
pub fn build_tcp_segment(
    src: IpAddr,
    dst: IpAddr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: TcpFlags,
    window: u16,
    payload: &[u8],
) -> Result<Vec<u8>, PacketError> {
    let header_len = TCP_MIN_HEADER;
    let payload_len = payload.len();
    if header_len + payload_len > u16::MAX as usize {
        return Err(PacketError::BadTotalLength);
    }

    let mut out = Vec::with_capacity(IPV4_MIN_HEADER + header_len + payload_len);
    write_ip_header(&mut out, src, dst, PROTO_TCP, header_len + payload_len)?;

    let segment_start = out.len();
    out.extend_from_slice(&src_port.to_be_bytes());
    out.extend_from_slice(&dst_port.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ack.to_be_bytes());
    out.push(0x50); // data offset 5, no options
    out.push(flags.to_byte());
    out.extend_from_slice(&window.to_be_bytes());
    out.extend_from_slice(&[0x00, 0x00]); // checksum placeholder
    out.extend_from_slice(&[0x00, 0x00]); // urgent pointer
    out.extend_from_slice(payload);

    let checksum = transport_checksum(src, dst, PROTO_TCP, &out[segment_start..]);
    out[segment_start + 16..segment_start + 18].copy_from_slice(&checksum.to_be_bytes());
    Ok(out)
}

/// Build a TCP reset refusing `incoming`, sent from the address that was dialled.
///
/// The segment is taken whole rather than field by field because RFC 793 section
/// 3.4 needs the segment's length as well as its numbers, and getting the rule
/// the wrong way round is not cosmetic: a client sitting in `SYN-SENT` accepts a
/// reset only when its acknowledgement field acknowledges the SYN it sent, so a
/// reset that echoes the SYN's own sequence number instead is silently ignored and
/// the client retries until it gives up. The two cases are:
///
/// * `ACK` was set: `<SEQ=SEG.ACK><CTL=RST>`. Nothing is acknowledged, because the
///   segment being refused already was.
/// * `ACK` was clear: `<SEQ=0><ACK=SEG.SEQ+SEG.LEN><CTL=RST,ACK>`, where `LEN` is
///   the sequence space the segment consumed — one for a SYN or a FIN, plus its
///   payload.
pub fn build_tcp_reset(
    src: IpAddr,
    dst: IpAddr,
    incoming: &TcpSegment<'_>,
) -> Result<Vec<u8>, PacketError> {
    let consumed = incoming.payload.len() as u32
        + u32::from(incoming.flags.syn)
        + u32::from(incoming.flags.fin);

    if incoming.flags.ack {
        build_tcp_segment(
            src,
            dst,
            incoming.dst_port,
            incoming.src_port,
            incoming.ack,
            0,
            TcpFlags {
                rst: true,
                ..TcpFlags::default()
            },
            0,
            &[],
        )
    } else {
        build_tcp_segment(
            src,
            dst,
            incoming.dst_port,
            incoming.src_port,
            0,
            incoming.seq.wrapping_add(consumed),
            TcpFlags {
                rst: true,
                ack: true,
                ..TcpFlags::default()
            },
            0,
            &[],
        )
    }
}

/// Build an ICMP port-unreachable message for a UDP datagram nobody handled.
///
/// The reply carries the original IP header plus the first eight bytes of its
/// payload, as required by RFC 792.
pub fn build_icmp_port_unreachable(
    src: IpAddr,
    dst: IpAddr,
    original: &[u8],
) -> Result<Vec<u8>, PacketError> {
    if src.is_ipv4() {
        // RFC 792 asks for the original internet header plus the first 64 bits of
        // its payload, so 28 bytes. Echoing more is permitted but pointless: the
        // client only needs enough of the datagram to match the reply to it.
        let echoed = original.len().min(28);
        let mut out = Vec::with_capacity(IPV4_MIN_HEADER + 8 + echoed);
        // The ICMP payload is its own 8 byte header plus the echoed bytes, and the
        // IP header has to declare exactly that. Declaring more produces a packet
        // whose length field disagrees with its contents, which every receiver
        // discards — so getting this wrong makes the reply useless.
        write_ip_header(&mut out, src, dst, PROTO_ICMP, 8 + echoed)?;

        let segment_start = out.len();
        out.push(3); // type: destination unreachable
        out.push(3); // code: port unreachable
        out.extend_from_slice(&[0x00, 0x00]); // checksum placeholder
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // unused
        out.extend_from_slice(&original[..echoed]);

        // ICMPv4 uses a plain one's complement sum with no pseudo header.
        let checksum = fold(add_words(&out[segment_start..], 0));
        out[segment_start + 2..segment_start + 4].copy_from_slice(&checksum.to_be_bytes());
        Ok(out)
    } else {
        // ICMPv6 needs the full IPv6 pseudo header; the kernel simply drops the
        // datagram instead, which is a legal outcome for an unreachable port.
        Err(PacketError::UnsupportedVersion(6))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V4_A: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    const V4_B: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));
    const V6_A: IpAddr = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
    const V6_B: IpAddr = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2));

    #[test]
    fn udp_round_trips_over_ipv4() {
        let packet = build_udp_packet(V4_A, V4_B, 1234, 53, b"hello").unwrap();
        assert_eq!(packet.len(), 20 + 8 + 5);

        let header = parse_ip_header(&packet).unwrap();
        assert_eq!(header.version, IpVersion::V4);
        assert_eq!(header.src, V4_A);
        assert_eq!(header.dst, V4_B);
        assert_eq!(header.protocol, PROTO_UDP);
        assert_eq!(header.header_len, 20);
        assert_eq!(header.total_len, packet.len());

        let datagram = parse_udp(&packet, &header).unwrap();
        assert_eq!(datagram.src_port, 1234);
        assert_eq!(datagram.dst_port, 53);
        assert_eq!(datagram.payload, b"hello");
    }

    #[test]
    fn udp_round_trips_over_ipv6() {
        let packet = build_udp_packet(V6_A, V6_B, 5353, 5353, b"mdns").unwrap();
        assert_eq!(packet.len(), 40 + 8 + 4);

        let header = parse_ip_header(&packet).unwrap();
        assert_eq!(header.version, IpVersion::V6);
        assert_eq!(header.src, V6_A);
        assert_eq!(header.dst, V6_B);

        let datagram = parse_udp(&packet, &header).unwrap();
        assert_eq!(datagram.payload, b"mdns");
    }

    #[test]
    fn generated_checksums_verify() {
        // IPv4 header checksum: recomputing over the finished header must yield
        // zero, because the field is included in the sum.
        let packet = build_udp_packet(V4_A, V4_B, 1, 2, b"payload").unwrap();
        assert_eq!(fold(add_words(&packet[..20], 0)), 0);

        // UDP checksum: recomputing with the field in place must also yield zero.
        let segment = &packet[20..];
        let mut sum = 0u32;
        sum = add_words(&[10, 0, 0, 1], sum);
        sum = add_words(&[198, 51, 100, 7], sum);
        sum += u32::from(PROTO_UDP);
        sum += segment.len() as u32;
        assert_eq!(fold(add_words(segment, sum)), 0);
    }

    /// Parse a segment out of a packet the client would have sent.
    fn incoming(src: IpAddr, dst: IpAddr, sport: u16, dport: u16, seq: u32, ack: u32, flags: TcpFlags) -> Vec<u8> {
        build_tcp_segment(src, dst, sport, dport, seq, ack, flags, 65535, &[]).unwrap()
    }

    #[test]
    fn a_reset_answering_a_syn_acknowledges_it() {
        let syn = incoming(
            V4_A,
            V4_B,
            5000,
            443,
            1000,
            0,
            TcpFlags {
                syn: true,
                ..TcpFlags::default()
            },
        );
        let syn_header = parse_ip_header(&syn).unwrap();
        let syn_segment = parse_tcp(&syn, &syn_header).unwrap();

        let packet = build_tcp_reset(V4_B, V4_A, &syn_segment).unwrap();
        let header = parse_ip_header(&packet).unwrap();
        let segment = parse_tcp(&packet, &header).unwrap();

        // The reset travels back the way the SYN came.
        assert_eq!(header.src, V4_B);
        assert_eq!(header.dst, V4_A);
        assert_eq!(segment.src_port, 443);
        assert_eq!(segment.dst_port, 5000);
        assert!(segment.flags.rst);
        assert!(segment.flags.ack, "a reset in reply to a SYN must acknowledge it");
        // The SYN consumed one sequence number, so the acknowledgement is
        // `seq + 1`. A client in SYN-SENT accepts the reset only for that value.
        assert_eq!(segment.ack, 1001);
        assert_eq!(segment.seq, 0, "RFC 793 uses sequence zero when ACK was clear");
    }

    #[test]
    fn tcp_reset_for_an_acknowledgement_does_not_acknowledge_back() {
        let ack = incoming(
            V4_A,
            V4_B,
            5000,
            443,
            10,
            99,
            TcpFlags {
                ack: true,
                ..TcpFlags::default()
            },
        );
        let ack_header = parse_ip_header(&ack).unwrap();
        let ack_segment = parse_tcp(&ack, &ack_header).unwrap();

        let packet = build_tcp_reset(V4_B, V4_A, &ack_segment).unwrap();
        let header = parse_ip_header(&packet).unwrap();
        let segment = parse_tcp(&packet, &header).unwrap();
        assert!(segment.flags.rst);
        assert!(!segment.flags.ack);
        assert_eq!(segment.seq, 99, "the reset takes the incoming acknowledgement");
    }

    #[test]
    fn a_reset_accounts_for_a_payload_the_segment_carried() {
        let data = build_tcp_segment(
            V4_A,
            V4_B,
            5000,
            443,
            1000,
            0,
            TcpFlags {
                psh: true,
                ..TcpFlags::default()
            },
            65535,
            b"1234567890",
        )
        .unwrap();
        let data_header = parse_ip_header(&data).unwrap();
        let data_segment = parse_tcp(&data, &data_header).unwrap();

        let packet = build_tcp_reset(V4_B, V4_A, &data_segment).unwrap();
        let header = parse_ip_header(&packet).unwrap();
        let segment = parse_tcp(&packet, &header).unwrap();
        // Ten bytes of payload consume ten sequence numbers, so the reset has to
        // acknowledge eleven past the start.
        assert_eq!(segment.ack, 1010);
    }

    #[test]
    fn icmp_port_unreachable_carries_the_original_header() {
        let original = build_udp_packet(V4_A, V4_B, 4000, 4001, b"abc").unwrap();
        let reply = build_icmp_port_unreachable(V4_B, V4_A, &original).unwrap();

        let header = parse_ip_header(&reply).unwrap();
        assert_eq!(header.protocol, PROTO_ICMP);
        assert_eq!(header.src, V4_B);
        assert_eq!(header.dst, V4_A);

        let payload = header.payload(&reply).unwrap();
        assert_eq!(payload[0], 3, "destination unreachable");
        assert_eq!(payload[1], 3, "port unreachable");
        // Checksum over the finished ICMP message must fold to zero.
        assert_eq!(fold(add_words(payload, 0)), 0);
        // The declared length must match what was actually written, and the echo
        // is capped at the 28 bytes RFC 792 requires.
        let echoed = original.len().min(28);
        assert_eq!(payload.len(), 8 + echoed);
        assert_eq!(&payload[8..], &original[..echoed]);
    }

    #[test]
    fn a_short_original_is_echoed_in_full() {
        let original = build_udp_packet(V4_A, V4_B, 4000, 4001, b"").unwrap();
        let reply = build_icmp_port_unreachable(V4_B, V4_A, &original).unwrap();

        let header = parse_ip_header(&reply).unwrap();
        assert_eq!(header.total_len, reply.len(), "the length field must be honest");
        let payload = header.payload(&reply).unwrap();
        assert_eq!(payload.len(), 8 + original.len());
        assert_eq!(&payload[8..], &original[..]);
    }

    #[test]
    fn parses_a_hand_built_ipv4_header_with_options() {
        let mut packet = vec![0u8; 24 + 8];
        packet[0] = 0x46; // version 4, IHL 6 (24 bytes)
        packet[2..4].copy_from_slice(&32u16.to_be_bytes());
        packet[9] = PROTO_UDP;
        packet[12..16].copy_from_slice(&[10, 0, 0, 1]);
        packet[16..20].copy_from_slice(&[10, 0, 0, 2]);
        packet[20..24].copy_from_slice(&[1, 1, 1, 1]); // option bytes
        packet[24..26].copy_from_slice(&53u16.to_be_bytes());
        packet[26..28].copy_from_slice(&5353u16.to_be_bytes());
        packet[28..30].copy_from_slice(&8u16.to_be_bytes());

        let header = parse_ip_header(&packet).unwrap();
        assert_eq!(header.header_len, 24);
        let datagram = parse_udp(&packet, &header).unwrap();
        assert_eq!(datagram.src_port, 53);
        assert_eq!(datagram.dst_port, 5353);
    }

    #[test]
    fn rejects_malformed_packets() {
        assert_eq!(parse_ip_header(&[]), Err(PacketError::TooShort));
        assert_eq!(parse_ip_header(&[0x00; 20]), Err(PacketError::UnsupportedVersion(0)));
        assert_eq!(parse_ip_header(&[0x45; 10]), Err(PacketError::TooShort));

        // IHL of 4 is below the mandatory 5 words.
        let mut short_header = vec![0u8; 20];
        short_header[0] = 0x44;
        assert_eq!(parse_ip_header(&short_header), Err(PacketError::BadHeaderLength));

        // Declared total length smaller than the header.
        let mut bad_total = vec![0u8; 20];
        bad_total[0] = 0x45;
        bad_total[2..4].copy_from_slice(&10u16.to_be_bytes());
        assert_eq!(parse_ip_header(&bad_total), Err(PacketError::BadTotalLength));

        // UDP length field longer than the IP payload.
        let mut packet = build_udp_packet(V4_A, V4_B, 1, 2, b"x").unwrap();
        let segment = 20;
        packet[segment + 4..segment + 6].copy_from_slice(&99u16.to_be_bytes());
        let header = parse_ip_header(&packet).unwrap();
        assert_eq!(parse_udp(&packet, &header), Err(PacketError::BadUdpLength));
    }

    #[test]
    fn detects_non_initial_fragments() {
        let mut packet = vec![0u8; 28];
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&28u16.to_be_bytes());
        packet[9] = PROTO_UDP;

        // No fragmentation: flags and offset are zero.
        let header = parse_ip_header(&packet).unwrap();
        assert!(!header.is_later_fragment(&packet));

        // More-fragments set.
        packet[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
        assert!(header.is_later_fragment(&packet));

        // Non-zero fragment offset.
        packet[6..8].copy_from_slice(&0x0008u16.to_be_bytes());
        assert!(header.is_later_fragment(&packet));
    }

    #[test]
    fn recognises_an_initial_syn() {
        let syn = build_tcp_segment(
            V4_A,
            V4_B,
            5000,
            443,
            42,
            0,
            TcpFlags {
                syn: true,
                ..TcpFlags::default()
            },
            65535,
            &[],
        )
        .unwrap();
        let header = parse_ip_header(&syn).unwrap();
        let segment = parse_tcp(&syn, &header).unwrap();
        assert!(segment.is_initial_syn());

        let syn_ack = build_tcp_segment(
            V4_B,
            V4_A,
            443,
            5000,
            7,
            43,
            TcpFlags {
                syn: true,
                ack: true,
                ..TcpFlags::default()
            },
            65535,
            &[],
        )
        .unwrap();
        let header = parse_ip_header(&syn_ack).unwrap();
        assert!(!parse_tcp(&syn_ack, &header).unwrap().is_initial_syn());
    }

    #[test]
    fn a_tcp_header_longer_than_the_minimum_is_measured_not_assumed() {
        // A real SYN nearly always carries options — a maximum segment size at the
        // least — so a header length assumed to be twenty bytes would put the start
        // of the payload in the middle of the options.
        const OPTIONS: [u8; 4] = [0x02, 0x04, 0x05, 0xb4]; // MSS 1460
        let header_len = TCP_MIN_HEADER + OPTIONS.len();
        let payload = b"hello";

        let mut packet = Vec::new();
        write_ip_header(
            &mut packet,
            V4_A,
            V4_B,
            PROTO_TCP,
            header_len + payload.len(),
        )
        .unwrap();
        let segment_start = packet.len();
        packet.extend_from_slice(&5000u16.to_be_bytes());
        packet.extend_from_slice(&443u16.to_be_bytes());
        packet.extend_from_slice(&7u32.to_be_bytes());
        packet.extend_from_slice(&0u32.to_be_bytes());
        packet.push(((header_len / 4) as u8) << 4);
        packet.push(
            TcpFlags {
                syn: true,
                ..TcpFlags::default()
            }
            .to_byte(),
        );
        packet.extend_from_slice(&65535u16.to_be_bytes());
        packet.extend_from_slice(&[0x00, 0x00]); // checksum placeholder
        packet.extend_from_slice(&[0x00, 0x00]); // urgent pointer
        packet.extend_from_slice(&OPTIONS);
        packet.extend_from_slice(payload);

        let checksum = transport_checksum(V4_A, V4_B, PROTO_TCP, &packet[segment_start..]);
        packet[segment_start + 16..segment_start + 18].copy_from_slice(&checksum.to_be_bytes());

        let header = parse_ip_header(&packet).unwrap();
        let segment = parse_tcp(&packet, &header).unwrap();

        assert_eq!(segment.header_len, header_len);
        assert_eq!(segment.payload, payload, "the options are not payload");
        assert!(segment.is_initial_syn());
    }
}

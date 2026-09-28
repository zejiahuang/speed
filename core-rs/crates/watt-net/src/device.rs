//! The [`PacketDevice`] trait plus an in-memory implementation for tests.

use std::collections::VecDeque;
use std::io;
use std::os::unix::io::RawFd;

use crate::MAX_PACKET;

/// A bidirectional stream of whole IP packets.
///
/// Implementations are non-blocking. `read_packet` returning `Ok(None)` means
/// "nothing to read right now" and is not an error: the engine loop is expected
/// to poll, not to block.
pub trait PacketDevice: Send {
    /// Interface MTU. Packets larger than this are never produced by the device.
    fn mtu(&self) -> usize;

    /// Read one packet into `buf`, returning its length.
    ///
    /// Returns `Ok(None)` when no packet is available, and `Ok(Some(0))` never
    /// happens. A packet longer than `buf` is a programming error and reported as
    /// an error rather than silently truncated.
    fn read_packet(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>>;

    /// Write one packet. Implementations should treat a partial write as an error,
    /// because a TUN device either accepts the whole packet or none of it.
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<()>;

    /// Human readable name, used in logs and diagnostics.
    fn name(&self) -> &str;

    /// The descriptor the engine should wait on for readability.
    ///
    /// `None` means the device cannot be waited on and the engine must poll on a
    /// timer instead. The in-memory device returns `None` because it is fed
    /// directly, which is exactly why tests drive the engine with a zero timeout.
    fn raw_fd(&self) -> Option<RawFd> {
        None
    }
}

/// In-memory device used by tests.
///
/// Packets pushed with [`MemoryDevice::inject`] appear on the receive path, and
/// packets the kernel writes appear in [`MemoryDevice::drain_sent`]. This makes
/// the entire packet path testable without root privileges, a real interface, or
/// any network access.
#[derive(Debug, Default)]
pub struct MemoryDevice {
    incoming: VecDeque<Vec<u8>>,
    outgoing: Vec<Vec<u8>>,
    mtu: usize,
    name: String,
    /// When set, `read_packet` reports this error once, to exercise error paths.
    fail_next_read: Option<io::ErrorKind>,
}

impl MemoryDevice {
    /// Create a device with the default MTU.
    pub fn new() -> Self {
        Self {
            mtu: crate::DEFAULT_MTU,
            name: "memory0".to_string(),
            ..Self::default()
        }
    }

    /// Override the reported MTU.
    pub fn with_mtu(mut self, mtu: usize) -> Self {
        self.mtu = mtu;
        self
    }

    /// Queue a packet as if it had arrived from the network.
    pub fn inject(&mut self, packet: impl Into<Vec<u8>>) {
        self.incoming.push_back(packet.into());
    }

    /// Number of packets waiting to be read.
    pub fn pending(&self) -> usize {
        self.incoming.len()
    }

    /// Take everything the kernel has written since the last drain.
    pub fn drain_sent(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outgoing)
    }

    /// Look at the most recently written packet without removing it.
    pub fn last_sent(&self) -> Option<&[u8]> {
        self.outgoing.last().map(Vec::as_slice)
    }

    /// Make the next read fail, for error handling tests.
    pub fn fail_next_read(&mut self, kind: io::ErrorKind) {
        self.fail_next_read = Some(kind);
    }
}

impl PacketDevice for MemoryDevice {
    fn mtu(&self) -> usize {
        self.mtu
    }

    fn read_packet(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        if let Some(kind) = self.fail_next_read.take() {
            return Err(io::Error::new(kind, "injected read failure"));
        }
        let Some(packet) = self.incoming.pop_front() else {
            return Ok(None);
        };
        if packet.len() > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("packet of {} bytes exceeds the {}-byte buffer", packet.len(), buf.len()),
            ));
        }
        buf[..packet.len()].copy_from_slice(&packet);
        Ok(Some(packet.len()))
    }

    fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        if packet.is_empty() || packet.len() > MAX_PACKET {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to write a {}-byte packet", packet.len()),
            ));
        }
        self.outgoing.push(packet.to_vec());
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_and_drains_packets() {
        let mut device = MemoryDevice::new();
        assert_eq!(device.pending(), 0);

        device.inject(vec![0x45, 0x00, 0x00, 0x14]);
        assert_eq!(device.pending(), 1);

        let mut buf = [0u8; 64];
        let read = device.read_packet(&mut buf).unwrap();
        assert_eq!(read, Some(4));
        assert_eq!(&buf[..4], &[0x45, 0x00, 0x00, 0x14]);
        assert_eq!(device.read_packet(&mut buf).unwrap(), None);

        device.write_packet(&[1, 2, 3]).unwrap();
        assert_eq!(device.drain_sent(), vec![vec![1, 2, 3]]);
        assert!(device.drain_sent().is_empty());
    }

    #[test]
    fn rejects_packets_larger_than_the_buffer() {
        let mut device = MemoryDevice::new();
        device.inject(vec![0u8; 32]);
        let mut small = [0u8; 8];
        let err = device.read_packet(&mut small).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_empty_and_oversized_writes() {
        let mut device = MemoryDevice::new();
        assert_eq!(
            device.write_packet(&[]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let huge = vec![0u8; MAX_PACKET + 1];
        assert_eq!(
            device.write_packet(&huge).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn propagates_injected_failures() {
        let mut device = MemoryDevice::new();
        device.fail_next_read(io::ErrorKind::WouldBlock);
        assert_eq!(
            device.read_packet(&mut [0u8; 8]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        // The failure is one-shot.
        assert_eq!(device.read_packet(&mut [0u8; 8]).unwrap(), None);
    }
}

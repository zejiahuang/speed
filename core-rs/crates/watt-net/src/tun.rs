//! Linux TUN device support, plus adoption of a file descriptor that somebody
//! else already configured (the Android `VpnService` case).
//!
//! The unsafe surface is deliberately tiny and confined to this module: three
//! `ioctl` calls, `open`, `read`, `write`, `fcntl` and `close`.
//!
//! Every `ioctl` request is written as `CONST as _` rather than passed through
//! unchanged, because the two platforms disagree about the argument's type:
//! Linux declares it `c_ulong` and Android declares it `c_int`. The cast lets the
//! compiler pick whichever the target expects, and it is safe because every
//! request used here fits in 32 bits on both. Writing the type out by hand is
//! what makes the Android build fail while the Linux one keeps working.

use std::io;
use std::os::unix::io::RawFd;

use crate::device::PacketDevice;
use crate::{DEFAULT_MTU, MAX_PACKET};

/// Path of the TUN clone device on Linux.
pub const TUN_CLONE_PATH: &str = "/dev/net/tun";

/// Length of the interface name field in `struct ifreq`.
const IFNAMSIZ: usize = 16;

/// `IFF_TUN` — layer 3 packets, no Ethernet header.
const IFF_TUN: libc::c_short = 0x0001;
/// `IFF_NO_PI` — do not prepend a 4-byte packet information header.
const IFF_NO_PI: libc::c_short = 0x1000;
/// `IFF_UP` — interface administratively up.
const IFF_UP: libc::c_short = 0x0001;

/// `_IOW('T', 202, int)`, identical on every Linux architecture.
const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
/// `SIOCGIFFLAGS` — read interface flags.
const SIOCGIFFLAGS: libc::c_ulong = 0x8913;
/// `SIOCSIFFLAGS` — write interface flags.
const SIOCSIFFLAGS: libc::c_ulong = 0x8914;

/// `struct ifreq` from `<net/if.h>`.
///
/// Only the name field and the `ifr_flags` member of the union are used. The
/// explicit padding brings the struct to the kernel's 40-byte size, which matters
/// because `TUNSETIFF` copies a whole `struct ifreq` from user space.
#[repr(C)]
#[derive(Clone, Copy)]
struct IfReq {
    name: [libc::c_char; IFNAMSIZ],
    flags: libc::c_short,
    _pad: [u8; 22],
}

impl IfReq {
    fn new(name: &str) -> io::Result<Self> {
        let bytes = name.as_bytes();
        if bytes.len() >= IFNAMSIZ {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("interface name {name:?} is longer than {} bytes", IFNAMSIZ - 1),
            ));
        }
        if !bytes.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("interface name {name:?} contains unsupported characters"),
            ));
        }
        let mut req = Self {
            name: [0; IFNAMSIZ],
            flags: 0,
            _pad: [0; 22],
        };
        for (slot, byte) in req.name.iter_mut().zip(bytes) {
            *slot = *byte as libc::c_char;
        }
        Ok(req)
    }

    /// The name as reported back by the kernel.
    fn resolved_name(&self) -> String {
        let bytes: Vec<u8> = self
            .name
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Configuration for [`TunDevice::create`].
#[derive(Debug, Clone)]
pub struct TunConfig {
    /// Requested interface name. An empty string lets the kernel pick one.
    pub name: String,
    /// MTU reported to the engine. The kernel default is used when this is zero.
    pub mtu: usize,
}

impl Default for TunConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            mtu: DEFAULT_MTU,
        }
    }
}

impl TunConfig {
    /// Ask the kernel for an interface with an automatic name.
    pub fn auto() -> Self {
        Self::default()
    }

    /// Ask for a specific interface name.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mtu: DEFAULT_MTU,
        }
    }
}

/// A raw IP packet device backed by a TUN file descriptor.
///
/// On Linux the descriptor comes from `/dev/net/tun`; on Android it comes from
/// `VpnService.establish()`. Both are the same kind of object to this type, which
/// is why there is a single implementation rather than a platform split.
#[derive(Debug)]
pub struct TunDevice {
    fd: RawFd,
    name: String,
    mtu: usize,
    /// Whether `Drop` should close the descriptor.
    owns_fd: bool,
}

impl TunDevice {
    /// Open `/dev/net/tun` and create (or attach to) a TUN interface.
    ///
    /// Requires `CAP_NET_ADMIN`; on a normal system that means running as root.
    pub fn create(config: &TunConfig) -> io::Result<Self> {
        let name = if config.name.is_empty() {
            String::new()
        } else {
            config.name.clone()
        };

        // SAFETY: `open` with a valid NUL-terminated path and flags. The returned
        // descriptor is checked for an error before being used.
        let fd = unsafe {
            let path = std::ffi::CString::new(TUN_CLONE_PATH).expect("static path has no NUL");
            libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut request = IfReq::new(&name)?;
        request.flags = IFF_TUN | IFF_NO_PI;

        // SAFETY: `fd` is an open TUN clone device and `request` is a correctly
        // sized `struct ifreq` that outlives the call.
        let result = unsafe { libc::ioctl(fd, TUNSETIFF as _, &request as *const IfReq) };
        if result < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: `fd` was returned by `open` above and has not been closed.
            unsafe { libc::close(fd) };
            return Err(err);
        }

        let resolved = request.resolved_name();
        let device = Self {
            fd,
            name: resolved,
            mtu: if config.mtu == 0 { DEFAULT_MTU } else { config.mtu },
            owns_fd: true,
        };
        device.set_nonblocking()?;
        Ok(device)
    }

    /// Adopt an already configured TUN descriptor.
    ///
    /// Used by Android: the descriptor belongs to `VpnService` and the Java side
    /// closes it, so the kernel must not. Ownership is therefore `false`.
    pub fn adopt(fd: RawFd, name: impl Into<String>, mtu: usize) -> io::Result<Self> {
        if fd < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative file descriptor"));
        }
        let device = Self {
            fd,
            name: name.into(),
            mtu: if mtu == 0 { DEFAULT_MTU } else { mtu },
            owns_fd: false,
        };
        device.set_nonblocking()?;
        Ok(device)
    }

    /// The descriptor, for handing to `epoll` or back to the JVM.
    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Mark the descriptor non-blocking so reads can be polled.
    fn set_nonblocking(&self) -> io::Result<()> {
        // SAFETY: `self.fd` is an open descriptor for the lifetime of `self`.
        let current = unsafe { libc::fcntl(self.fd, libc::F_GETFL) };
        if current < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: same descriptor, and `current` came from `F_GETFL`.
        let result = unsafe { libc::fcntl(self.fd, libc::F_SETFL, current | libc::O_NONBLOCK) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Bring the interface up.
    ///
    /// This needs a separate socket because interface flags live on the network
    /// interface, not on the TUN descriptor.
    pub fn bring_up(&self) -> io::Result<()> {
        // SAFETY: a plain AF_INET SOCK_DGRAM socket, closed on every path below.
        let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if sock < 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| -> io::Result<()> {
            let mut request = IfReq::new(&self.name)?;
            // SAFETY: `sock` is an open socket and `request` is a correctly sized
            // `struct ifreq`.
            if unsafe { libc::ioctl(sock, SIOCGIFFLAGS as _, &mut request as *mut IfReq) } < 0 {
                return Err(io::Error::last_os_error());
            }
            request.flags |= IFF_UP;
            // SAFETY: same socket, same struct, now populated with the read flags.
            if unsafe { libc::ioctl(sock, SIOCSIFFLAGS as _, &request as *const IfReq) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        // SAFETY: `sock` was created above and is not used afterwards.
        unsafe { libc::close(sock) };
        result
    }

    /// Translate a raw read result into the trait's contract.
    fn read_fd(fd: RawFd, buf: &mut [u8]) -> io::Result<Option<usize>> {
        if buf.len() > MAX_PACKET {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read buffer exceeds the maximum packet size",
            ));
        }
        // SAFETY: `fd` is open for the lifetime of the caller's device and `buf`
        // is a valid writable slice of the reported length.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            return match err.kind() {
                io::ErrorKind::WouldBlock => Ok(None),
                // A TUN device reports EINTR when a signal arrives mid-read.
                io::ErrorKind::Interrupted => Ok(None),
                _ => Err(err),
            };
        }
        if n == 0 {
            // Zero-length reads happen while the interface is coming up.
            return Ok(None);
        }
        Ok(Some(n as usize))
    }

    fn write_fd(fd: RawFd, packet: &[u8]) -> io::Result<()> {
        if packet.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty packet"));
        }
        if packet.len() > MAX_PACKET {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("packet of {} bytes exceeds the maximum", packet.len()),
            ));
        }
        // SAFETY: `fd` is open and `packet` is a valid readable slice.
        let n = unsafe { libc::write(fd, packet.as_ptr().cast(), packet.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            return match err.kind() {
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(()),
                _ => Err(err),
            };
        }
        if (n as usize) != packet.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("partial TUN write: {} of {} bytes", n, packet.len()),
            ));
        }
        Ok(())
    }
}

impl PacketDevice for TunDevice {
    fn mtu(&self) -> usize {
        self.mtu
    }

    fn read_packet(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        Self::read_fd(self.fd, buf)
    }

    fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        Self::write_fd(self.fd, packet)
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn raw_fd(&self) -> Option<RawFd> {
        Some(self.fd)
    }
}

impl Drop for TunDevice {
    fn drop(&mut self) {
        if self.owns_fd {
            // SAFETY: `self.fd` was returned by `open` and is closed exactly once,
            // because `Drop` runs once and the descriptor is never cloned.
            unsafe { libc::close(self.fd) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ifreq_matches_the_kernel_layout() {
        // `struct ifreq` is 40 bytes on every Linux architecture we support.
        assert_eq!(std::mem::size_of::<IfReq>(), 40);
    }

    #[test]
    fn ifreq_rejects_bad_names() {
        assert!(IfReq::new("wt0").is_ok());
        assert!(IfReq::new("").is_ok());
        assert!(IfReq::new("a".repeat(16).as_str()).is_err());
        assert!(IfReq::new("bad name").is_err());
        assert!(IfReq::new("bad/name").is_err());
    }

    #[test]
    fn ifreq_round_trips_the_name() {
        let req = IfReq::new("watt0").unwrap();
        assert_eq!(req.resolved_name(), "watt0");
    }

    #[test]
    fn adopting_a_bad_descriptor_fails_cleanly() {
        let err = TunDevice::adopt(-1, "nope", 1500).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

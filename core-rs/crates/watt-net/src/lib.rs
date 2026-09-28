//! Raw IP packet transport for the full-traffic kernel.
//!
//! Everything above this crate works with whole IP packets: it reads a packet,
//! decides what to do with it, and writes a packet back. Nothing here knows about
//! TCP, rules or proxies.
//!
//! Three device implementations share one trait:
//!
//! * [`TunDevice`] — a real Linux TUN interface, or an adopted descriptor that
//!   somebody else already configured. The adopted case is how Android
//!   integrates: `VpnService.establish()` hands the native side a TUN file
//!   descriptor and the kernel simply reads and writes it.
//! * [`MemoryDevice`] — a pair of in-memory queues, so the whole packet path can be
//!   tested without root privileges or a real interface.
//!
//! All devices are non-blocking: a read that would block reports "nothing
//! available" rather than stalling the engine loop.

pub mod device;
pub mod probe;
pub mod tun;

pub use device::{MemoryDevice, PacketDevice};
pub use tun::{TunConfig, TunDevice, TUN_CLONE_PATH};

/// Conventional interface MTU for a TUN device carrying the whole IPv4 stack.
///
/// 1500 keeps the packet path identical to a normal Ethernet interface, which
/// avoids surprises with path MTU discovery.
pub const DEFAULT_MTU: usize = 1500;

/// Upper bound on a packet the kernel is willing to handle, including the IPv6
/// minimum MTU requirement of 1280 and the headroom a device may add.
pub const MAX_PACKET: usize = 65_536;

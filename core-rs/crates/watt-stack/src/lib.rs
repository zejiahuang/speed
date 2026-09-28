//! Full-traffic userspace kernel for the Watt Android client.
//!
//! ```text
//!                       ┌──────────────────────────────────────────┐
//!   TUN (Android fd,    │  engine                                  │
//!   Linux /dev/net/tun) │                                          │
//!        │              │  ┌────────────┐   ┌───────────────────┐  │
//!        └─────────────▶│  │  planner   │◀──│ rules + selector  │  │
//!                       │  └─────┬──────┘   └───────────────────┘  │
//!                       │        │  target                        │
//!                       │  ┌─────▼──────┐   ┌───────────────────┐  │
//!                       │  │ tcp relay  │   │ observation cache │  │
//!                       │  │ udp relay  │◀──│ (address → name)  │  │
//!                       │  └─────┬──────┘   └───────────────────┘  │
//!                       │        │                                │
//!                       │  ┌─────▼──────┐                         │
//!                       │  │  poll(2)   │  upstream sockets       │
//!                       │  └────────────┘                         │
//!                       └──────────────────────────────────────────┘
//! ```
//!
//! The engine takes every packet the tunnel delivers, decides per flow where it
//! should really go, and carries it there over ordinary sockets. Nothing is
//! decrypted: TLS passes through untouched, and a rule can only choose which
//! address a connection is made to.
//!
//! Layers, bottom up:
//!
//! * [`packet`] — IPv4/IPv6, UDP and TCP wire codec.
//! * [`dns`] — DNS message codec, used to answer rule owned names and to observe
//!   the names other answers belong to.
//! * [`flow`] — flow identity and the bounded address-to-domain cache.
//! * [`planner`] — the routing policy: rewrites, then rules, then direct.
//! * [`upstream`] — non-blocking sockets created through `libc`, so a platform
//!   protector can exempt them from the tunnel before they connect.
//! * [`poller`] — `poll(2)` over the tunnel and every upstream socket.
//! * [`tcp`] / [`udp`] — the two data planes.
//! * [`engine`] — the loop that ties them together.
//!
//! The shortest useful program is:
//!
//! ```no_run
//! use watt_rules::Router;
//! use watt_stack::{open_tun_unprotected, StackConfig};
//!
//! let router = Router::builtin()?;
//! let mut engine = open_tun_unprotected(StackConfig::default(), router)?;
//! engine.run()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! On Android the device comes from `VpnService` instead, and the protector is
//! what keeps the kernel's own sockets out of its own tunnel:
//!
//! ```no_run
//! # use watt_stack::{Engine, Protector, StackConfig};
//! # use watt_rules::Router;
//! # use std::os::unix::io::RawFd;
//! # struct VpnServiceProtector;
//! # impl Protector for VpnServiceProtector {
//! #     fn protect(&mut self, _fd: RawFd) -> bool { true }
//! # }
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let router = Router::builtin()?;
//! let engine = Engine::adopt_tun(
//!     /* descriptor from VpnService.establish() */ 3,
//!     "watt0",
//!     StackConfig::default(),
//!     router,
//!     Box::new(VpnServiceProtector),
//! )?;
//! # let _ = engine;
//! # Ok(())
//! # }
//! ```

#![warn(missing_debug_implementations)]

pub mod config;
pub mod dns;
pub mod engine;
pub mod flow;
pub mod packet;
pub mod planner;
pub mod poller;
pub mod tcp;
pub mod udp;
pub mod upstream;
pub mod verify;

pub use config::{DestinationOverride, StackConfig, Stats};
pub use engine::{open_tun_unprotected, Engine, FlowSnapshot};
pub use flow::{FlowKey, ObservationCache};
pub use planner::{Decision, Planner};
pub use poller::{PollEvent, Poller};
pub use tcp::{TcpFlowInfo, TcpRelay};
pub use udp::{UdpFlowInfo, UdpRelay, DNS_PORT};
pub use upstream::{
    ConnectState, MarkProtector, NoProtector, Protector, UpstreamSocket, DEFAULT_MARK,
};

pub use watt_net::{MemoryDevice, PacketDevice, TunConfig, TunDevice};

/// Conventional interface MTU, re-exported so callers need only one import.
pub use watt_net::DEFAULT_MTU;

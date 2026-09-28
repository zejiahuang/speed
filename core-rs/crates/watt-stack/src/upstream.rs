//! Non-blocking upstream sockets, built on raw file descriptors.
//!
//! `std::net` is not used here for one decisive reason: the descriptor must be
//! handed to the platform *before* the connection starts. On Android,
//! `VpnService.protect(fd)` is what keeps the kernel's own upstream connections
//! out of the tunnel. Without it every relayed connection is captured by the VPN
//! again and the kernel talks to itself until descriptors run out.
//!
//! Everything is non-blocking and driven by the engine's poll loop, so a slow or
//! unreachable destination stalls one flow rather than the whole kernel.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::io::RawFd;

use watt_rules::Family;

/// Outcome of starting a non-blocking connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectState {
    /// The connection is already established.
    Connected,
    /// The connection is in progress; wait for the descriptor to become writable.
    InProgress,
}

/// Exempts a descriptor from the tunnel.
///
/// Android implements this by calling `VpnService.protect`. Other platforms can
/// declare that they have nothing to do, which is different from trying and
/// failing.
pub trait Protector: Send {
    /// Called with a fresh descriptor before it is connected.
    ///
    /// Returning `false` means the descriptor could not be protected. What that
    /// costs depends on [`Protector::required`]: for a protector that has work
    /// to do, a failure is fatal and the socket is never connected.
    fn protect(&mut self, fd: RawFd) -> bool;

    /// Whether this protector was supposed to succeed.
    ///
    /// A protector that genuinely has nothing to do answers `false` here, so its
    /// `protect` result is not a failure. One that does have work to do —
    /// setting a socket mark, calling `VpnService.protect` — must succeed, and
    /// is treated as failed when it does not.
    fn required(&self) -> bool {
        true
    }
}

/// A protector that does nothing, used on plain Linux and in tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProtector;

impl Protector for NoProtector {
    fn protect(&mut self, _fd: RawFd) -> bool {
        false
    }

    // Nothing to do, so a `false` from `protect` is the expected answer rather
    // than a failure. Only a protector with real work to do can fail.
    fn required(&self) -> bool {
        false
    }
}

/// Default socket mark. Arbitrary, but it has to match the policy rule the host
/// installs, so it is published rather than hidden.
pub const DEFAULT_MARK: u32 = 0x5754;

/// Exempts a descriptor from the tunnel by tagging it with a socket mark.
///
/// This is the Linux counterpart of `VpnService.protect`, and it exists because
/// without *some* such mechanism the kernel cannot be used on a plain host at
/// all. The problem it solves is not subtle: the kernel's upstream sockets are
/// routed by the same table as everything else, so a connection to a relayed
/// address is delivered straight back into the tunnel, where the kernel accepts
/// it as a new client and relays it again. One client connection becomes
/// hundreds in under a second, and the client is told it connected, because the
/// kernel answered its own SYN.
///
/// The mark on its own does nothing. The host has to send marked traffic to a
/// table that does not contain the tunnel's routes, which is the same
/// arrangement `wg-quick` uses for WireGuard's own packets:
///
/// ```text
/// ip route add default via <gateway> dev <iface> table 5754
/// ip rule add fwmark 0x5754 lookup 5754 pref 100
/// ```
///
/// Needs `CAP_NET_ADMIN`. A failure to set the mark is fatal: the socket is
/// never connected.
///
/// Letting it proceed was tried, and it is the wrong trade. An unprotected
/// upstream socket is captured by the tunnel, and the kernel then relays its own
/// traffic — one client connection becomes hundreds in under a second, and the
/// client is told it connected because the kernel answered its own SYN. A
/// refusal is loud and points at the cause; a loop looks like the kernel
/// working, only slowly, and is far harder to diagnose.
#[derive(Debug, Clone, Copy)]
pub struct MarkProtector {
    mark: u32,
}

impl MarkProtector {
    pub fn new(mark: u32) -> Self {
        Self { mark }
    }

    pub fn mark(&self) -> u32 {
        self.mark
    }
}

impl Default for MarkProtector {
    fn default() -> Self {
        Self::new(DEFAULT_MARK)
    }
}

impl Protector for MarkProtector {
    fn protect(&mut self, fd: RawFd) -> bool {
        let value = self.mark as libc::c_int;
        // SAFETY: `fd` is an open socket owned by the caller, and `value` is a
        // correctly sized `c_int` for the option being written.
        let result = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_MARK,
                std::ptr::addr_of!(value).cast::<libc::c_void>(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        result == 0
    }
}

/// A socket owned by the kernel, closed on drop.
#[derive(Debug)]
pub struct UpstreamSocket {
    fd: RawFd,
    family: Family,
    peer: Option<SocketAddr>,
    /// Whether the descriptor was offered to the protector before being used.
    ///
    /// The ordering is the difference between working on Android and the kernel
    /// capturing its own upstream traffic, and a rule that lives only in a comment
    /// is one refactor away from being wrong. Tracked so that a future constructor
    /// which forgets to protect fails loudly instead of looping silently.
    offered_to_protector: bool,
}

impl UpstreamSocket {
    /// Create a non-blocking TCP socket, protected and not yet connected.
    pub fn tcp(family: Family, protector: &mut dyn Protector) -> io::Result<Self> {
        let fd = create_socket(libc::SOCK_STREAM, family)?;
        Self::new_protected(fd, family, protector)
    }

    /// Create a non-blocking UDP socket, protected and not yet bound.
    pub fn udp(family: Family, protector: &mut dyn Protector) -> io::Result<Self> {
        let fd = create_socket(libc::SOCK_DGRAM, family)?;
        Self::new_protected(fd, family, protector)
    }

    /// Offer `fd` to the protector, refusing it when protection was required and
    /// did not happen.
    ///
    /// The descriptor is closed on refusal: a socket that will never be allowed
    /// to connect has no further use, and leaking one per attempt is how a
    /// misconfigured host runs out of descriptors as well as looping.
    fn new_protected(
        fd: RawFd,
        family: Family,
        protector: &mut dyn Protector,
    ) -> io::Result<Self> {
        if !protector.protect(fd) && protector.required() {
            let err = io::Error::other(
                "the upstream socket could not be protected from the tunnel; \
                 refusing to connect, because an unprotected socket is captured \
                 by the tunnel and the kernel then relays its own traffic",
            );
            // SAFETY: `fd` was created here and is not used after this point.
            unsafe { libc::close(fd) };
            return Err(err);
        }
        Ok(Self {
            fd,
            family,
            peer: None,
            offered_to_protector: true,
        })
    }

    /// The descriptor, for registering with the poll loop.
    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    pub fn family(&self) -> Family {
        self.family
    }

    /// The address this socket was connected to, once known.
    pub fn peer(&self) -> Option<SocketAddr> {
        self.peer
    }

    /// Begin a non-blocking connection.
    pub fn start_connect(&mut self, target: SocketAddr) -> io::Result<ConnectState> {
        // Refusing here rather than asserting: a socket that reaches `connect`
        // without having been offered to the protector is the one failure that
        // would look, from the outside, like the kernel simply not working. A hard
        // error turns it into a connect failure the engine already knows how to
        // report and retry.
        if !self.offered_to_protector {
            return Err(io::Error::other(
                "refusing to connect a socket that was never offered to the protector; \
                 on Android this is what stops the kernel capturing its own traffic",
            ));
        }

        let (storage, len) = sockaddr_for(target);
        // SAFETY: `self.fd` is an open socket of the matching family, and
        // `storage` is a correctly initialised `sockaddr_storage` of length `len`.
        let result = unsafe {
            libc::connect(
                self.fd,
                (&storage as *const libc::sockaddr_storage).cast::<libc::sockaddr>(),
                len,
            )
        };
        self.peer = Some(target);
        if result == 0 {
            return Ok(ConnectState::Connected);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINPROGRESS) | Some(libc::EALREADY) => Ok(ConnectState::InProgress),
            // A non-blocking connect on an already connected socket is harmless.
            Some(libc::EISCONN) => Ok(ConnectState::Connected),
            _ => Err(err),
        }
    }

    /// Ask the kernel whether a completed connect actually succeeded.
    ///
    /// A writable descriptor only means the attempt finished; `SO_ERROR` carries
    /// the verdict.
    pub fn take_connect_error(&self) -> io::Result<()> {
        let mut error: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: `self.fd` is open and `error`/`len` are valid for the level and
        // option requested.
        let result = unsafe {
            libc::getsockopt(
                self.fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error as *mut libc::c_int).cast::<libc::c_void>(),
                &mut len,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if error == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(error))
        }
    }

    /// Read from a connected socket. `Ok(0)` means end of stream.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `self.fd` is open and `buf` is a valid writable slice.
        let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    /// Write to a connected socket, without raising `SIGPIPE`.
    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: `self.fd` is open and `buf` is a valid readable slice.
        // `MSG_NOSIGNAL` is what makes a peer reset an error instead of a signal
        // that would kill the process.
        let n = unsafe {
            libc::send(
                self.fd,
                buf.as_ptr().cast(),
                buf.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    /// Receive one datagram and its source address.
    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        // SAFETY: zeroed POD storage for the kernel to fill in.
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        // SAFETY: `self.fd` is open and the storage is large enough for any
        // address family the socket can produce.
        let n = unsafe {
            libc::recvfrom(
                self.fd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
                (&mut storage as *mut libc::sockaddr_storage).cast::<libc::sockaddr>(),
                &mut len,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((n as usize, sockaddr_from(&storage)?))
    }

    /// Send one datagram.
    pub fn send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        let (storage, len) = sockaddr_for(target);
        // SAFETY: `self.fd` is open, `buf` is readable and `storage` is a valid
        // address of length `len`.
        let n = unsafe {
            libc::sendto(
                self.fd,
                buf.as_ptr().cast(),
                buf.len(),
                libc::MSG_NOSIGNAL,
                (&storage as *const libc::sockaddr_storage).cast::<libc::sockaddr>(),
                len,
            )
        };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    /// Send to the address the socket was connected to.
    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.write(buf)
    }

    /// Close the write direction, leaving reads open.
    pub fn shutdown_write(&self) -> io::Result<()> {
        // SAFETY: `self.fd` is open.
        let result = unsafe { libc::shutdown(self.fd, libc::SHUT_WR) };
        if result < 0 {
            let err = io::Error::last_os_error();
            // Shutting down a socket that is already gone is not an error here.
            return match err.raw_os_error() {
                Some(libc::ENOTCONN) | Some(libc::EPIPE) => Ok(()),
                _ => Err(err),
            };
        }
        Ok(())
    }
}

impl Drop for UpstreamSocket {
    fn drop(&mut self) {
        // SAFETY: `self.fd` came from `socket` and is closed exactly once, since
        // `Drop` runs once and the descriptor is never duplicated.
        unsafe { libc::close(self.fd) };
    }
}

fn create_socket(kind: libc::c_int, family: Family) -> io::Result<RawFd> {
    let domain = match family {
        Family::V4 => libc::AF_INET,
        Family::V6 => libc::AF_INET6,
    };
    // SAFETY: a plain socket creation with no user pointers.
    let fd = unsafe { libc::socket(domain, kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Build a `sockaddr_storage` for `addr`.
fn sockaddr_for(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: `sockaddr_storage` is plain data and all-zero is a valid value.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    match addr {
        SocketAddr::V4(v4) => {
            let sin = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    // Network byte order is the in-memory order of the octets.
                    s_addr: u32::from_ne_bytes(v4.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: the destination is a zeroed `sockaddr_storage`, which is
            // larger and at least as aligned as `sockaddr_in`.
            unsafe {
                std::ptr::write(
                    (&mut storage as *mut libc::sockaddr_storage).cast::<libc::sockaddr_in>(),
                    sin,
                )
            };
            (
                storage,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        }
        SocketAddr::V6(v6) => {
            let sin6 = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: v6.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                },
                sin6_scope_id: v6.scope_id(),
            };
            // SAFETY: as above, with `sockaddr_in6`.
            unsafe {
                std::ptr::write(
                    (&mut storage as *mut libc::sockaddr_storage).cast::<libc::sockaddr_in6>(),
                    sin6,
                )
            };
            (
                storage,
                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
            )
        }
    }
}

/// Recover a [`SocketAddr`] from kernel-filled storage.
fn sockaddr_from(storage: &libc::sockaddr_storage) -> io::Result<SocketAddr> {
    match storage.ss_family as libc::c_int {
        libc::AF_INET => {
            // SAFETY: the family says this storage holds a `sockaddr_in`.
            let sin = unsafe {
                &*(storage as *const libc::sockaddr_storage).cast::<libc::sockaddr_in>()
            };
            let octets = sin.sin_addr.s_addr.to_ne_bytes();
            Ok(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::from(octets)),
                u16::from_be(sin.sin_port),
            ))
        }
        libc::AF_INET6 => {
            // SAFETY: the family says this storage holds a `sockaddr_in6`.
            let sin6 = unsafe {
                &*(storage as *const libc::sockaddr_storage).cast::<libc::sockaddr_in6>()
            };
            Ok(SocketAddr::new(
                IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)),
                u16::from_be(sin6.sin6_port),
            ))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported address family {other}"),
        )),
    }
}

/// True when an error means "try again later" rather than "this failed".
pub fn is_retryable(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records whether `protect` was called while the socket was still unconnected.
    ///
    /// `getpeername` succeeds only once `connect` has been called, so it is a
    /// direct observation of the ordering rather than an inference from it.
    #[derive(Default)]
    struct OrderingProtector {
        calls: usize,
        saw_connected_socket: bool,
    }

    impl Protector for OrderingProtector {
        fn protect(&mut self, fd: RawFd) -> bool {
            self.calls += 1;
            let mut storage = std::mem::MaybeUninit::<libc::sockaddr_storage>::uninit();
            let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            // SAFETY: `fd` was just created by the caller and is open; `storage`
            // and `len` are valid for the option being read.
            let result =
                unsafe { libc::getpeername(fd, storage.as_mut_ptr().cast(), &mut len) };
            if result == 0 {
                self.saw_connected_socket = true;
            }
            true
        }
    }

    #[test]
    fn a_socket_is_protected_before_it_is_connected() {
        // The whole point of hand-writing these sockets instead of using
        // `std::net`. On Android, protecting after connecting means the kernel's
        // own upstream connection is captured by the tunnel it is implementing,
        // so it talks to itself until descriptors run out. On Linux the protector
        // is a no-op, which is exactly why no other test in this suite can notice
        // the order being wrong.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();

        let mut protector = OrderingProtector::default();
        let mut socket = UpstreamSocket::tcp(Family::V4, &mut protector).unwrap();

        assert_eq!(
            protector.calls, 1,
            "every socket has to be offered to the protector, or Android will \
             capture its own traffic"
        );
        assert!(
            !protector.saw_connected_socket,
            "protect ran on an already connected socket; the descriptor must be \
             handed over before connect() is called"
        );

        // The ordering must not have been achieved by simply not connecting.
        let state = socket.start_connect(target).unwrap();
        if state == ConnectState::InProgress {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if socket.take_connect_error().is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        assert_eq!(
            socket.peer(),
            Some(target),
            "the socket has to end up connected, or the ordering proves nothing"
        );

        // Positive control. Everything above passes just as well if `getpeername`
        // never succeeds, which would make the detector blind rather than correct.
        // Calling it again on the now-connected socket has to flip the flag.
        protector.protect(socket.raw_fd());
        assert!(
            protector.saw_connected_socket,
            "the detector cannot see a connected socket, so it could not have \
             seen the ordering either"
        );
    }

    #[test]
    fn a_udp_socket_is_protected_before_it_is_bound() {
        // UDP never connects, so the equivalent question is whether the
        // descriptor is handed over before it is used at all. `getsockname` is
        // unspecified on an unbound socket, so the check is simply that protect
        // ran, exactly once, for the socket that was created.
        let mut protector = OrderingProtector::default();
        let socket = UpstreamSocket::udp(Family::V4, &mut protector).unwrap();

        assert_eq!(protector.calls, 1);
        assert!(
            !protector.saw_connected_socket,
            "a UDP socket is never connected, so protect cannot have seen one"
        );
        assert!(socket.peer().is_none());
    }

    #[test]
    fn connecting_a_socket_that_was_never_offered_to_the_protector_is_refused() {
        // The guard exists for constructors that do not exist yet, so reaching it
        // means building the struct by hand — which only this module can do.
        let mut socket = UpstreamSocket {
            fd: create_socket(libc::SOCK_STREAM, Family::V4).unwrap(),
            family: Family::V4,
            peer: None,
            offered_to_protector: false,
        };

        let err = socket
            .start_connect("127.0.0.1:9".parse().unwrap())
            .unwrap_err();
        assert!(
            err.to_string().contains("never offered to the protector"),
            "expected the guard to refuse, got {err}"
        );
    }

    /// A protector that is asked to do something and cannot do it.
    struct FailingProtector;

    impl Protector for FailingProtector {
        fn protect(&mut self, _fd: RawFd) -> bool {
            false
        }
    }

    #[test]
    fn a_socket_whose_protection_fails_is_refused_rather_than_connected() {
        // This is the difference between a misconfigured host reporting an error
        // and a misconfigured host relaying its own traffic. An unprotected
        // upstream socket is captured by the tunnel; the kernel accepts it as a
        // new client and relays it again, so refusing is not pedantry, it is the
        // only failure mode that can be diagnosed from the outside.
        let err = UpstreamSocket::tcp(Family::V4, &mut FailingProtector).unwrap_err();
        assert!(
            err.to_string().contains("could not be protected"),
            "expected the socket to be refused, got {err}"
        );
    }

    #[test]
    fn a_protector_with_nothing_to_do_is_not_a_failure() {
        // `NoProtector` returns false from `protect`, and that has to stay
        // usable: it is what a host with no tunnel in the path uses.
        assert!(!NoProtector.required());
        let socket = UpstreamSocket::tcp(Family::V4, &mut NoProtector);
        assert!(socket.is_ok(), "a no-op protector must not refuse: {socket:?}");
    }

    #[test]
    fn the_mark_protector_reports_failure_rather_than_claiming_success() {
        // A protector that always returned true would hide a host that was never
        // configured, and the kernel would then capture its own traffic with
        // nothing in the log to say why. An invalid descriptor is the one case
        // that fails on every host, privileged or not, so it is the case that can
        // always be asserted on.
        let mut protector = MarkProtector::default();
        assert!(
            !protector.protect(-1),
            "marking an invalid descriptor has to report failure"
        );
    }

    #[test]
    fn the_mark_protector_sets_the_mark_when_the_host_allows_it() {
        // Setting SO_MARK needs CAP_NET_ADMIN, so the write path cannot be
        // exercised by an ordinary test run. What can be asserted either way is
        // that the report and the descriptor agree: accepted means the mark is
        // readable, refused means it is not.
        let mark = 0x1234u32;
        let mut protector = MarkProtector::new(mark);
        let fd = create_socket(libc::SOCK_STREAM, Family::V4).unwrap();
        let accepted = protector.protect(fd);

        let mut value: libc::c_int = -1;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: `fd` is open and `value`/`len` are valid for the option read.
        let readable = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_MARK,
                std::ptr::addr_of_mut!(value).cast::<libc::c_void>(),
                &mut len,
            )
        } == 0;

        if accepted {
            assert!(readable, "the mark was accepted but cannot be read back");
            assert_eq!(value, mark as libc::c_int, "the mark was set to something else");
        } else {
            assert_eq!(
                value, 0,
                "the protector reported failure but the mark changed anyway"
            );
            eprintln!(
                "note: SO_MARK needs CAP_NET_ADMIN and was refused here, so the \
                 write path was not exercised"
            );
        }

        // SAFETY: `fd` came from `create_socket` and is not used again.
        unsafe { libc::close(fd) };
    }

    #[test]
    fn the_default_mark_is_the_one_the_host_rules_expect() {
        assert_eq!(MarkProtector::default().mark(), DEFAULT_MARK);
        assert_eq!(DEFAULT_MARK, 0x5754);
    }

    #[test]
    fn sockaddr_round_trips_ipv4() {
        let addr: SocketAddr = "203.0.113.7:8443".parse().unwrap();
        let (storage, len) = sockaddr_for(addr);
        assert_eq!(len, std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t);
        assert_eq!(sockaddr_from(&storage).unwrap(), addr);
    }

    #[test]
    fn sockaddr_round_trips_ipv6() {
        let addr: SocketAddr = "[2001:db8::1]:443".parse().unwrap();
        let (storage, len) = sockaddr_for(addr);
        assert_eq!(len, std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t);
        assert_eq!(sockaddr_from(&storage).unwrap(), addr);
    }

    #[test]
    fn rejects_an_unknown_address_family() {
        // SAFETY: plain data, zeroed then given an invalid family value.
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        storage.ss_family = 99;
        assert!(sockaddr_from(&storage).is_err());
    }

    #[test]
    fn tcp_socket_is_non_blocking_and_connects() {
        // A local listener stands in for a real destination.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();

        let mut protector = NoProtector;
        let mut socket = UpstreamSocket::tcp(Family::V4, &mut protector).unwrap();

        // A non-blocking connect to a listening socket on loopback either
        // completes immediately or reports that it is in progress. Both are
        // valid; what matters is that neither blocks nor errors.
        let state = socket.start_connect(target).unwrap();
        if state == ConnectState::InProgress {
            // Wait briefly for the connect to finish, then confirm via SO_ERROR.
            let mut ready = false;
            for _ in 0..200 {
                if socket.take_connect_error().is_ok() {
                    ready = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(ready, "connect did not complete");
        }

        let (mut accepted, _) = listener.accept().unwrap();
        socket.write(b"ping").unwrap();
        let mut buf = [0u8; 4];
        use std::io::Read;
        accepted.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[test]
    fn udp_socket_sends_and_receives() {
        let server = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();

        let mut protector = NoProtector;
        let socket = UpstreamSocket::udp(Family::V4, &mut protector).unwrap();
        socket.send_to(b"hello", server_addr).unwrap();

        let mut buf = [0u8; 16];
        let (len, from) = server.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"hello");
        assert_eq!(from.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));

        server.send_to(b"world", from).unwrap();
        let (len, peer) = socket.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"world");
        assert_eq!(peer, server_addr);
    }

    #[test]
    fn connecting_to_a_closed_port_reports_an_error() {
        // Bind and drop to obtain a port nothing listens on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        drop(listener);

        let mut protector = NoProtector;
        let mut socket = UpstreamSocket::tcp(Family::V4, &mut protector).unwrap();
        socket.start_connect(target).unwrap();

        let mut failed = false;
        for _ in 0..200 {
            if socket.take_connect_error().is_err() {
                failed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(failed, "connecting to a closed port must surface an error");
    }

    #[test]
    fn retryable_classification() {
        assert!(is_retryable(&io::Error::from(io::ErrorKind::WouldBlock)));
        assert!(is_retryable(&io::Error::from(io::ErrorKind::Interrupted)));
        assert!(!is_retryable(&io::Error::from(io::ErrorKind::ConnectionRefused)));
    }
}

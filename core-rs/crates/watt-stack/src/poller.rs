//! A thin wrapper over `poll(2)`.
//!
//! The engine has to wait on two very different things at once: the TUN
//! descriptor, which delivers client packets, and every upstream socket, which
//! delivers replies. `poll` handles both in one syscall and gives the loop a
//! timeout, so timer work (smoltcp's retransmit deadlines, idle flow reaping)
//! happens on schedule instead of on packet arrival.
//!
//! The descriptor set is rebuilt for every wait. That is O(n) per iteration, but
//! n is bounded by the flow limit and the work is a `memcpy` of eight bytes per
//! flow, which is far cheaper than the bookkeeping needed to maintain the set
//! incrementally and keep it correct.

use std::io;
use std::os::unix::io::RawFd;

/// The caller wants to know when the descriptor becomes readable.
pub const INTEREST_READ: u8 = 0x1;
/// The caller wants to know when the descriptor becomes writable.
pub const INTEREST_WRITE: u8 = 0x2;

/// One ready descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollEvent {
    /// Caller supplied identifier for the descriptor.
    pub key: u64,
    pub readable: bool,
    pub writable: bool,
    /// The descriptor reported an error condition.
    pub error: bool,
    /// The peer closed, or the descriptor was invalidated.
    pub hangup: bool,
}

/// Descriptor set for a single wait.
#[derive(Debug, Default)]
pub struct Poller {
    fds: Vec<libc::pollfd>,
    keys: Vec<u64>,
    events: Vec<PollEvent>,
}

impl Poller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of descriptors currently registered.
    pub fn len(&self) -> usize {
        self.fds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fds.is_empty()
    }

    /// Drop every registration. Called at the top of each loop iteration.
    pub fn clear(&mut self) {
        self.fds.clear();
        self.keys.clear();
    }

    /// Register `fd` under `key`. Re-registering the same descriptor in one
    /// iteration is a caller error; the engine builds the set from its own maps,
    /// which are keyed by descriptor, so it cannot happen.
    pub fn add(&mut self, fd: RawFd, key: u64, interests: u8) {
        let mut events: libc::c_short = 0;
        if interests & INTEREST_READ != 0 {
            events |= libc::POLLIN;
        }
        if interests & INTEREST_WRITE != 0 {
            events |= libc::POLLOUT;
        }
        self.fds.push(libc::pollfd {
            fd,
            events,
            revents: 0,
        });
        self.keys.push(key);
    }

    /// Wait for readiness, up to `timeout_ms` (negative waits forever, zero
    /// returns immediately). Returns the ready descriptors.
    pub fn wait(&mut self, timeout_ms: i32) -> io::Result<&[PollEvent]> {
        self.events.clear();
        if self.fds.is_empty() {
            // With nothing to wait on, honour the timeout by sleeping rather than
            // spinning. A zero timeout still returns immediately.
            if timeout_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(timeout_ms as u64));
            }
            return Ok(&self.events);
        }

        // SAFETY: `self.fds` is a valid slice of `pollfd` of the length passed.
        let ready = unsafe {
            libc::poll(
                self.fds.as_mut_ptr(),
                self.fds.len() as libc::nfds_t,
                timeout_ms,
            )
        };
        if ready < 0 {
            let err = io::Error::last_os_error();
            // A signal arriving mid-wait is not a failure.
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(&self.events);
            }
            return Err(err);
        }

        for (index, entry) in self.fds.iter().enumerate() {
            let revents = entry.revents;
            if revents == 0 {
                continue;
            }
            self.events.push(PollEvent {
                key: self.keys[index],
                readable: revents & (libc::POLLIN | libc::POLLPRI) != 0,
                writable: revents & libc::POLLOUT != 0,
                error: revents & libc::POLLERR != 0,
                hangup: revents & (libc::POLLHUP | libc::POLLNVAL) != 0,
            });
        }
        Ok(&self.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    #[test]
    fn reports_readiness_on_a_socket_pair() {
        let (read_end, mut write_end) = UnixStream::pair().unwrap();
        let mut poller = Poller::new();
        poller.add(read_end.as_raw_fd(), 7, INTEREST_READ);

        // Nothing written yet: the wait times out with no events.
        assert!(poller.wait(0).unwrap().is_empty());

        write_end.write_all(b"x").unwrap();

        let events = poller.wait(1000).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].key, 7);
        assert!(events[0].readable);
        assert!(!events[0].writable);
    }

    #[test]
    fn reports_writability_and_hangup() {
        let (read_end, write_end) = UnixStream::pair().unwrap();
        let mut poller = Poller::new();
        poller.add(write_end.as_raw_fd(), 1, INTEREST_WRITE);
        let events = poller.wait(1000).unwrap();
        assert!(events[0].writable, "a fresh socket must be writable");

        // Closing the peer makes the surviving end report a hangup.
        drop(read_end);
        let mut poller = Poller::new();
        poller.add(write_end.as_raw_fd(), 2, INTEREST_WRITE);
        let events = poller.wait(1000).unwrap();
        assert!(events[0].hangup || events[0].error);
    }

    #[test]
    fn reads_what_the_peer_wrote() {
        let (mut read_end, mut write_end) = UnixStream::pair().unwrap();
        write_end.write_all(b"payload").unwrap();

        let mut poller = Poller::new();
        poller.add(read_end.as_raw_fd(), 1, INTEREST_READ);
        assert_eq!(poller.wait(1000).unwrap().len(), 1);

        let mut buf = [0u8; 7];
        read_end.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"payload");
    }

    #[test]
    fn honours_the_timeout() {
        let (read_end, _write_end) = UnixStream::pair().unwrap();
        let mut poller = Poller::new();
        poller.add(read_end.as_raw_fd(), 1, INTEREST_READ);

        let start = Instant::now();
        poller.wait(60).unwrap();
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(50), "returned after {elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "waited too long: {elapsed:?}");
    }

    #[test]
    fn an_empty_set_still_honours_the_timeout() {
        let mut poller = Poller::new();
        let start = Instant::now();
        poller.wait(30).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(25));
        assert!(poller.is_empty());
    }

    #[test]
    fn clearing_removes_registrations() {
        let (read_end, _write_end) = UnixStream::pair().unwrap();
        let mut poller = Poller::new();
        poller.add(read_end.as_raw_fd(), 1, INTEREST_READ);
        assert_eq!(poller.len(), 1);
        poller.clear();
        assert_eq!(poller.len(), 0);
    }
}

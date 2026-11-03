// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor readiness without consuming bytes: indefinite reads or an absolute-deadline
//! read/write wait. Hangup also wakes the caller, which drains buffered data before EOF.

use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd};

/// Run an operation with nonblocking status set on a descriptor, then restore its flags.
/// Duplicated descriptors share these flags; the caller must own their read/write use
/// during this scope. Restoration is also attempted during unwinding.
///
/// # Errors
/// Reading, setting or restoring descriptor flags fails, or the operation fails.
pub fn with_nonblocking<T>(
    fd: BorrowedFd<'_>,
    work: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    struct Restore<'a> {
        fd: BorrowedFd<'a>,
        flags: i32,
    }
    impl Restore<'_> {
        fn apply(&self) -> io::Result<()> {
            // SAFETY: the borrowed descriptor remains open; F_SETFL takes this integer flag word.
            if unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, self.flags) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = self.apply();
        }
    }
    // SAFETY: F_GETFL reads the flags of the live borrowed descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let restore = Restore { fd, flags };
    // SAFETY: F_SETFL updates the status flags of that same open descriptor.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = work();
    let restored = restore.apply();
    result.and_then(|value| restored.map(|()| value))
}

/// What [`either`] found ready: at least one is `true`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ready {
    /// The first descriptor is readable, at its end (`POLLHUP`) or in error (`POLLERR`,
    /// `POLLNVAL`).
    pub first: bool,
    /// The second is.
    pub second: bool,
}

/// Blocks, with no timeout, until `first` or `second` is readable, at its end or in error, and
/// says which (both, when both were by the time `poll(2)` returned). Reads nothing, so a byte
/// either holds is still there for its reader. An interrupted `poll(2)` is asked again.
///
/// # Errors
/// `poll(2)`'s errno, other than `EINTR`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn either(first: BorrowedFd<'_>, second: BorrowedFd<'_>) -> io::Result<Ready> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    #[cfg(target_os = "linux")]
    type Nfds = core::ffi::c_ulong;
    #[cfg(not(target_os = "linux"))]
    type Nfds = core::ffi::c_uint;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: Nfds, timeout: i32) -> i32;
    }
    const POLLIN: i16 = 0x1;
    loop {
        let mut fds = [
            PollFd {
                fd: first.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            },
            PollFd {
                fd: second.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two initialized pollfds that outlive the call, each naming a descriptor its
        // `BorrowedFd` keeps open for it; `poll` reads and writes them and retains neither.
        let ready = unsafe { poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let found = Ready {
            first: fds[0].revents != 0,
            second: fds[1].revents != 0,
        };
        if found.first || found.second {
            return Ok(found);
        }
    }
}

/// Blocks, with no timeout, until `fd` is readable, at its end or in error: [`either`] of it and
/// itself. A listener is readable once a connection waits to be accepted.
///
/// # Errors
/// `poll(2)`'s errno, other than `EINTR`.
pub fn one(fd: BorrowedFd<'_>) -> io::Result<()> {
    either(fd, fd).map(drop)
}

/// Which nonblocking operation a deadline wait precedes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interest {
    Read,
    Write,
}

/// Wait for `interest`, hangup or error without changing socket timeout options.
/// Use a nonblocking operation after this call: readiness can be consumed by
/// another reader/writer. Interrupted waits retain the original deadline.
///
/// # Errors
/// Expiry, an invalid descriptor, or `poll(2)` errors other than interruption.
#[cfg_attr(trust_verify, trust::skip)]
pub fn until(
    fd: BorrowedFd<'_>,
    interest: Interest,
    deadline: std::time::Instant,
) -> io::Result<()> {
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|n| !n.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "readiness deadline"))?;
        let millis =
            remaining.as_millis() + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000));
        let timeout = millis.min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: match interest {
                Interest::Read => libc::POLLIN,
                Interest::Write => libc::POLLOUT,
            },
            revents: 0,
        };
        // SAFETY: the initialized pollfd remains live and its borrowed descriptor stays open.
        let result = unsafe { libc::poll(&raw mut descriptor, 1, timeout) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "readiness deadline",
            ));
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid readiness descriptor",
            ));
        }
        if descriptor.revents != 0 {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_nonblocking_restores_flags_after_an_operation_error() {
        use std::io::Read as _;
        use std::os::fd::{AsFd as _, AsRawFd as _};
        let (mut reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let observer = reader.try_clone().unwrap();
        // SAFETY: the stream retains its live descriptor throughout the test.
        let before = unsafe { libc::fcntl(observer.as_raw_fd(), libc::F_GETFL) };
        assert!(before >= 0);
        let error = super::with_nonblocking(observer.as_fd(), || reader.read(&mut [0]).map(|_| ()))
            .expect_err("an empty open stream would block");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        // SAFETY: the same observer still retains the open descriptor.
        let after = unsafe { libc::fcntl(observer.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(after, before);
    }

    use super::{Ready, either, one};
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// The one-minute hang detector on a wait that must end.
    const HANG: Duration = Duration::from_secs(60);

    #[test]
    fn deadline_readiness_keeps_buffered_bytes_and_eof_after_disconnect() {
        let (mut near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
        near.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + HANG;
        super::until(near.as_fd(), super::Interest::Write, deadline).unwrap();
        far.write_all(b"kept").unwrap();
        drop(far);
        super::until(near.as_fd(), super::Interest::Read, deadline).unwrap();
        let mut bytes = [0; 4];
        near.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"kept");
        super::until(near.as_fd(), super::Interest::Read, deadline).unwrap();
        assert_eq!(near.read(&mut bytes).unwrap(), 0);
        for interest in [super::Interest::Read, super::Interest::Write] {
            assert_eq!(
                super::until(near.as_fd(), interest, std::time::Instant::now())
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::TimedOut
            );
        }
    }

    /// EACH SIDE WAKES THE WAIT, AND NEITHER IS READ: a byte on the first is
    /// still there after the wait said so; the second's writer closing (a
    /// pipe at its end) wakes it as the second; and a wait woken while the
    /// other side is still silent names only the side that woke it.
    #[test]
    fn either_side_ends_the_wait_and_nothing_is_read() {
        let (mut a_near, a_far) = UnixStream::pair().expect("a pair");
        let (b_read, b_write) = std::io::pipe().expect("a pipe");
        let (said, heard) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let found = either(a_far.as_fd(), b_read.as_fd()).expect("the first wait");
            said.send(found).expect("the test listens");
            let mut byte = [0_u8; 1];
            (&a_far)
                .read_exact(&mut byte)
                .expect("the byte is still there");
            assert_eq!(&byte, b"x");
            let found = either(a_far.as_fd(), b_read.as_fd()).expect("the second wait");
            said.send(found).expect("the test listens");
        });
        a_near.write_all(b"x").expect("write");
        assert_eq!(
            heard.recv_timeout(HANG).expect("the first wait ended"),
            Ready {
                first: true,
                second: false
            }
        );
        drop(b_write);
        assert_eq!(
            heard.recv_timeout(HANG).expect("the second wait ended"),
            Ready {
                first: false,
                second: true
            }
        );
        waiter.join().expect("the waiter");
    }

    /// A LISTENER IS READY WITH A CONNECTION WAITING: [`one`] ends once a connect is queued, and
    /// a nonblocking accept then finds it.
    #[test]
    fn one_ends_when_a_listener_has_a_connection_to_accept() {
        let root = aterm_tempfile::tempdir().expect("a folder");
        let path = root.path().join("ready.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let (said, heard) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            one(listener.as_fd()).expect("the wait");
            said.send(listener.accept().is_ok())
                .expect("the test listens");
        });
        let _client = UnixStream::connect(&path).expect("connect");
        assert!(heard.recv_timeout(HANG).expect("the wait ended"));
        waiter.join().expect("the waiter");
    }

    /// A PEER'S CLOSE IS READY: a socket whose peer is gone ends the wait
    /// as its side, with nothing to read but its end.
    #[test]
    fn a_closed_peer_ends_the_wait() {
        let (near, far) = UnixStream::pair().expect("a pair");
        let (b_read, _b_write) = std::io::pipe().expect("a pipe");
        drop(far);
        let found = either(b_read.as_fd(), near.as_fd()).expect("the wait");
        assert_eq!(
            found,
            Ready {
                first: false,
                second: true
            }
        );
        assert_eq!((&near).read(&mut [0_u8; 1]).expect("its end"), 0);
    }
}

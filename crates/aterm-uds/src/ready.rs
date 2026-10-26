// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A wait on two descriptors at once, with no timeout: one `poll(2)` that ends when either is
//! readable, at its end or in error, and reads neither.

use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd};

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

#[cfg(test)]
mod tests {
    use super::{Ready, either};
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// The one-minute hang detector on a wait that must end.
    const HANG: Duration = Duration::from_secs(60);

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

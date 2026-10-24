// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The cordoned Apple module: hand-declared FFI for log durability, boot identity,
//! and a zero-wait socket hangup observation (no libc dependency). Raw pointers and
//! kernel errno values stay inside this module.

use std::ffi::{c_char, c_int, c_short, c_uint, c_void};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// `SIGHUP` from `<sys/signal.h>`.
const SIGHUP: c_int = 1;

/// `F_BARRIERFSYNC` from `<sys/fcntl.h>`: "fsync + issue barrier to drive".
const F_BARRIERFSYNC: c_int = 85;

/// `F_GETFD`, `F_SETFD` and the descriptor flags from `<sys/fcntl.h>`.
const F_GETFD: c_int = 1;
const F_SETFD: c_int = 2;
const FD_CLOFORK: c_int = 2;

// Darwin <sys/poll.h>: nfds_t is unsigned int; pollfd is int, short, short.
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}

const POLLIN: c_short = 0x0001;
const POLLOUT: c_short = 0x0004;
const POLLERR: c_short = 0x0008;
const POLLHUP: c_short = 0x0010;
const POLLNVAL: c_short = 0x0020;

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: c_uint, timeout: c_int) -> c_int;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    #[cfg(debug_assertions)]
    fn fsync(fd: c_int) -> c_int;
    fn signal(sig: c_int, handler: extern "C" fn(c_int)) -> usize;
    fn sysctlbyname(
        name: *const c_char,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> c_int;
}

static SIGHUP_INSTALL: OnceLock<io::Result<()>> = OnceLock::new();
static SIGHUP_GENERATION: AtomicU64 = AtomicU64::new(0);

extern "C" fn on_sighup(_sig: c_int) {
    SIGHUP_GENERATION.fetch_add(1, Ordering::Release);
}

pub(crate) fn install_sighup_handler() -> io::Result<()> {
    // Concurrent callers wait for the actual installation result. The handler never
    // touches this lock: it only increments SIGHUP_GENERATION.
    match SIGHUP_INSTALL.get_or_init(|| {
        // Darwin defines SIG_ERR as the handler pointer (void (*)(int))-1.
        if apple_ffi!(install_sighup(SIGHUP, on_sighup)) == usize::MAX {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }) {
        Ok(()) => Ok(()),
        Err(err) => Err(match err.raw_os_error() {
            Some(code) => io::Error::from_raw_os_error(code),
            None => io::Error::new(err.kind(), err.to_string()),
        }),
    }
}

pub(crate) fn sighup_generation() -> u64 {
    SIGHUP_GENERATION.load(Ordering::Acquire)
}

/// `fcntl(fd, F_BARRIERFSYNC)`, retried while it is interrupted (`EINTR`); any other
/// failure is the call's own errno.
pub(crate) fn barrier_fsync(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    loop {
        let rc = apple_ffi!(barrier_fsync(fd, F_BARRIERFSYNC));
        if rc != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// `fsync(fd)`, retried while it is interrupted (`EINTR`): the file's writes handed
/// to the drive, without the drain of its cache `F_FULLFSYNC` waits for. Only a test's
/// process in a debug build takes it ([`crate::full_sync`], [`crate::TEST_PLAIN_FSYNC_ENV`]).
#[cfg(debug_assertions)]
pub(crate) fn plain_fsync(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    loop {
        let rc = apple_ffi!(fsync(fd));
        if rc != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// The raw `kern.bootsessionuuid` answer (the UUID text and its NUL), or `None` when
/// the call fails. The buffer leaves room past a UUID's 37 bytes: a longer answer is
/// refused by the kernel (`ENOMEM`), never truncated, and reads `None`.
pub(crate) fn boot_session_uuid() -> Option<[u8; 64]> {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    let len_ptr = std::slice::from_mut(&mut len).as_mut_ptr();
    let rc = apple_ffi!(sysctlbyname(
        c"kern.bootsessionuuid".as_ptr(),
        buf.as_mut_ptr().cast(),
        len_ptr,
        std::ptr::null_mut(),
        0
    ));
    if rc != 0 || len > buf.len() {
        return None;
    }
    Some(buf)
}

/// `fcntl(fd, F_SETFD, flags | FD_CLOFORK)` ([`crate::close_on_fork`]).
pub(crate) fn close_on_fork(socket: BorrowedFd<'_>) -> io::Result<()> {
    let fd = socket.as_raw_fd();
    let flags = apple_ffi!(fd_flags(fd));
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if flags & FD_CLOFORK != 0 {
        return Ok(());
    }
    if apple_ffi!(set_fd_flags(fd, flags | FD_CLOFORK)) == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Separate read/write entries keep their EOF evidence distinct. A single entry
/// requesting both would merge either direction's HUP into the same revents flag.
pub(crate) fn socket_hung_up(socket: BorrowedFd<'_>) -> bool {
    let fd = socket.as_raw_fd();
    let mut entries = [
        PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        },
        PollFd {
            fd,
            events: POLLOUT,
            revents: 0,
        },
    ];
    let rc = apple_ffi!(socket_hangup(entries.as_mut_ptr()));
    rc == 2
        && entries
            .iter()
            .all(|entry| entry.revents & POLLHUP != 0 && entry.revents & (POLLERR | POLLNVAL) == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;

    /// `EEXIST`, `EBADF` and `ENOENT` from `<sys/errno.h>`.
    const EEXIST: c_int = 17;
    const EBADF: c_int = 9;
    const ENOENT: c_int = 2;

    /// The kernel keeps a marked socket out of a forked child and leaves an unmarked
    /// one in it. The child looks at its own table between fork and exec — the window a
    /// descriptor scan drains a parent's socket through — then execs a path that does
    /// not exist, so no program runs and its `ENOENT` is the all-clear.
    #[test]
    fn close_on_fork_keeps_a_socket_out_of_a_forked_child() {
        let (marked, _peer) = UnixStream::pair().expect("a socket pair");
        let (unmarked, _peer2) = UnixStream::pair().expect("a second socket pair");
        crate::close_on_fork(marked.as_fd()).expect("mark the socket");
        crate::close_on_fork(marked.as_fd()).expect("marking twice is a no-op");
        let (m, u) = (marked.as_raw_fd(), unmarked.as_raw_fd());
        let mut child = std::process::Command::new("/nonexistent/astream-sys-close-on-fork");
        // SAFETY: the hook only calls `fcntl(F_GETFD)`, which is async-signal-safe, and
        // allocates nothing: `from_raw_os_error` builds its error without the heap.
        unsafe {
            child.pre_exec(move || {
                if fcntl(m, F_GETFD) != -1 {
                    return Err(io::Error::from_raw_os_error(EEXIST));
                }
                if fcntl(u, F_GETFD) == -1 {
                    return Err(io::Error::from_raw_os_error(EBADF));
                }
                Ok(())
            });
        }
        let refused = child.spawn().expect_err("no program exists at that path");
        match refused.raw_os_error() {
            Some(ENOENT) => {}
            Some(EEXIST) => panic!("the marked socket reached the forked child"),
            Some(EBADF) => panic!("NEGATIVE CONTROL: the unmarked socket did not reach the child"),
            _ => panic!("the child failed some other way: {refused}"),
        }
    }
}

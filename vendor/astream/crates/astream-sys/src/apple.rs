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

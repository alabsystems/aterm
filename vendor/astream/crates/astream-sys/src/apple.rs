// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The cordoned Apple module: the workspace's `unsafe` for the durable log, two FFI
//! calls declared by hand (no libc dependency). A reviewer auditing what the log asks
//! of the kernel reads exactly this file: `fcntl(F_BARRIERFSYNC)` and
//! `sysctlbyname("kern.bootsessionuuid")`. Neither hands a raw pointer or an errno
//! back to safe code.

use std::ffi::{c_char, c_int, c_void};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// `F_BARRIERFSYNC` from `<sys/fcntl.h>`: "fsync + issue barrier to drive".
const F_BARRIERFSYNC: c_int = 85;

unsafe extern "C" {
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn sysctlbyname(
        name: *const c_char,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> c_int;
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
pub(crate) fn boot_session_uuid() -> Option<Vec<u8>> {
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
    if rc != 0 {
        return None;
    }
    buf.get(..len).map(<[u8]>::to_vec)
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `rustix::pipe`: `pipe` and `pipe2`.

use std::os::fd::{FromRawFd, OwnedFd};

use libc::{c_int, c_uint};

use crate::io::{self, check};

flags! {
    /// `O_*` flags for [`pipe_with`].
    pub struct PipeFlags: c_uint {
        /// `O_CLOEXEC`
        const CLOEXEC = libc::O_CLOEXEC as c_uint;
        /// `O_DIRECT` (packet mode)
        const DIRECT = libc::O_DIRECT as c_uint;
        /// `O_NONBLOCK`
        const NONBLOCK = libc::O_NONBLOCK as c_uint;
    }
}

/// Takes ownership of the two ends `pipe`/`pipe2` just wrote.
fn ends(fds: [c_int; 2]) -> (OwnedFd, OwnedFd) {
    // SAFETY: the call succeeded, so both are new descriptors returned to us
    // alone; each gets exactly one owner.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

/// `pipe()`: a pipe, read end first, with no flags (in particular, NOT
/// close-on-exec).
#[inline]
pub fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [c_int; 2] = [-1, -1];
    // SAFETY: `fds` has room for the two descriptors `pipe` writes.
    check(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
    Ok(ends(fds))
}

/// `pipe2(flags)`: a pipe, read end first, with `flags` on both ends.
#[inline]
pub fn pipe_with(flags: PipeFlags) -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [c_int; 2] = [-1, -1];
    // SAFETY: `fds` has room for the two descriptors `pipe2` writes.
    check(unsafe { libc::pipe2(fds.as_mut_ptr(), flags.bits() as c_int) })?;
    Ok(ends(fds))
}

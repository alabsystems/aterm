// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `rustix::thread`: the thread id.

pub use crate::process::{Pid, RawPid};

/// `gettid()`: the calling thread's id; equal to [`getpid`](crate::process::getpid)
/// on the main thread only.
///
/// Through `syscall(2)`, as upstream issues it: glibc's `gettid` wrapper exists
/// only since 2.30.
#[inline]
pub fn gettid() -> Pid {
    // SAFETY: SYS_gettid takes no arguments, cannot fail, and returns the
    // calling thread's id, which is positive and fits `pid_t`.
    unsafe { Pid::from_raw_unchecked(libc::syscall(libc::SYS_gettid) as RawPid) }
}

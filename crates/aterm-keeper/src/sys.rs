// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The three syscalls the server loop makes that `aterm-uds` has no wrapper
//! for: `poll(2)`, a non-blocking `waitpid(2)` for the one kind of child the
//! keeper has (`/usr/bin/open`), and raising `RLIMIT_NOFILE` to its hard limit
//! (the keeper holds one descriptor per terminal). Nothing here touches a PTY.

/// `struct pollfd`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// Readable.
pub const POLLIN: i16 = 0x1;
/// Hung up.
pub const POLLHUP: i16 = 0x10;
/// Error.
pub const POLLERR: i16 = 0x8;

unsafe extern "C" {
    #[link_name = "poll"]
    fn c_poll(fds: *mut PollFd, n: u32, timeout: i32) -> i32;
    #[link_name = "waitpid"]
    fn c_waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn getrlimit(resource: i32, rlim: *mut [u64; 2]) -> i32;
    fn setrlimit(resource: i32, rlim: *const [u64; 2]) -> i32;
}

/// `poll(2)` over `fds` for up to `timeout_ms`; `Interrupted` is retried by
/// the caller's loop, never here.
///
/// # Errors
/// What `poll` reports.
// Skip: bottoms out at the `poll` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn poll(fds: &mut [PollFd], timeout_ms: i32) -> std::io::Result<usize> {
    let n = u32::try_from(fds.len()).map_err(|_| std::io::Error::other("too many fds"))?;
    // SAFETY: `fds` is `n` live pollfd records.
    let rc = unsafe { c_poll(fds.as_mut_ptr(), n, timeout_ms) };
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(usize::try_from(rc).unwrap_or(0))
    }
}

/// Reap `pid` if it has exited; `true` when it has (or is no child of ours).
// Skip: bottoms out at the `waitpid` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn reap_if_done(pid: u32) -> bool {
    const WNOHANG: i32 = 1;
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    let mut status = 0;
    // SAFETY: a live out-parameter; WNOHANG never blocks.
    let rc = unsafe { c_waitpid(pid, &mut status, WNOHANG) };
    rc != 0
}

/// Raise `RLIMIT_NOFILE`'s soft limit to its hard limit (Darwin caps the
/// value at `OPEN_MAX` for an unlimited hard limit). Best-effort.
// Skip: bottoms out at the `getrlimit`/`setrlimit` FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn raise_nofile() {
    #[cfg(target_vendor = "apple")]
    const RLIMIT_NOFILE: i32 = 8;
    #[cfg(not(target_vendor = "apple"))]
    const RLIMIT_NOFILE: i32 = 7;
    const OPEN_MAX: u64 = 10_240;
    let mut lim = [0u64; 2];
    // SAFETY: a live two-word out-parameter (`struct rlimit`).
    if unsafe { getrlimit(RLIMIT_NOFILE, &mut lim) } != 0 {
        return;
    }
    let want = if cfg!(target_vendor = "apple") {
        lim[1].min(OPEN_MAX)
    } else {
        lim[1]
    };
    if want > lim[0] {
        let new = [want, lim[1]];
        // SAFETY: a live two-word record.
        let _ = unsafe { setrlimit(RLIMIT_NOFILE, &new) };
    }
}

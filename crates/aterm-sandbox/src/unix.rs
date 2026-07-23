// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX actuator for [`crate::Limits::apply`]: the best-effort per-limit
//! `setrlimit` loop, moved verbatim from the pre-split `lib.rs`. The capability
//! gate stays in `lib.rs` (platform-shared); only the OS actuation lives here.

use std::io;

use crate::Limits;

/// Apply every requested limit BEST-EFFORT: a resource the OS does not support
/// (e.g. `RLIMIT_AS` on macOS) must NOT prevent the limits that DO work
/// (`RLIMIT_NOFILE`) from being installed. Every limit is attempted; the first
/// per-limit error is returned only after all have been tried, so one
/// unsupported resource can never silently leave the child unconfined.
pub(crate) fn apply_limits(limits: &Limits) -> io::Result<()> {
    let mut first_err: Option<io::Error> = None;
    for (resource, value) in [
        (libc::RLIMIT_CPU, limits.cpu_seconds),
        (libc::RLIMIT_AS, limits.address_space),
        (libc::RLIMIT_FSIZE, limits.file_size),
        (libc::RLIMIT_NOFILE, limits.open_files),
    ] {
        if let Err(e) = set_limit(resource, value) {
            first_err.get_or_insert(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The type of a `RLIMIT_*` resource selector, which differs by platform: glibc
/// Linux types `setrlimit`'s first argument (and its `RLIMIT_*` constants) as
/// `__rlimit_resource_t` (a `u32`), while macOS/BSD and musl use `c_int`. Aliasing
/// it keeps [`set_limit`] portable — the `RLIMIT_*` constants already have this
/// per-platform type, so they pass through without a cast.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) type RlimitResource = libc::c_int;

/// Set the SOFT `resource` limit to `value` while PRESERVING the inherited hard
/// ceiling (no-op when `value` is `None`).
///
/// We deliberately do NOT lower the hard limit. The spawned `$SHELL` (User mode
/// runs it transparently) must stay able to RAISE its own soft limit from its rc
/// — e.g. a `.zshrc` line `ulimit -n 65536` — exactly as under any other
/// terminal. Clamping the hard limit to the requested value broke that: a soft
/// request above the (also-lowered) hard limit aborts shell startup with
/// `ulimit: value exceeds hard limit`. Treating the value as a soft default the
/// child can raise up to the inherited hard ceiling is the correct semantics for
/// the non-actuated User sandbox. The soft value is clamped to the hard ceiling
/// so `setrlimit` can never `EINVAL` on `rlim_cur > rlim_max`.
pub(crate) fn set_limit(resource: RlimitResource, value: Option<u64>) -> io::Result<()> {
    let Some(v) = value else {
        return Ok(());
    };
    // Read the inherited limits so the hard ceiling is preserved. This is a bare
    // `getrlimit` syscall (no allocation), so it stays async-signal-safe in the
    // post-fork child where `apply` runs. If the read fails, fall back to the old
    // behavior (set both) rather than leaving the limit unconfined.
    let mut cur = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid resource id + a valid out-param for the call's duration.
    let hard = if unsafe { libc::getrlimit(resource, &mut cur) } == 0 {
        cur.rlim_max
    } else {
        v as libc::rlim_t
    };
    let lim = libc::rlimit {
        rlim_cur: core::cmp::min(v as libc::rlim_t, hard),
        rlim_max: hard,
    };
    // SAFETY: `resource` is a valid RLIMIT_* constant and `&lim` is a valid,
    // fully-initialized `rlimit` for the duration of the call.
    let rc = unsafe { libc::setrlimit(resource, &lim) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX actuator for [`crate::Limits::apply`]: the best-effort per-limit
//! `setrlimit` loop. The capability gate is in `lib.rs`.

use std::io;

use crate::Limits;

/// Attempt every requested limit, so one the OS refuses (a 16 GiB `RLIMIT_AS`
/// on macOS) cannot keep the others from installing; return the first error
/// after all were tried.
pub(crate) fn apply_limits(limits: &Limits) -> io::Result<()> {
    let mut first_err: Option<io::Error> = None;
    for (resource, value) in [
        (libc::RLIMIT_CPU, limits.cpu_seconds),
        (libc::RLIMIT_AS, limits.address_space),
        (libc::RLIMIT_FSIZE, limits.file_size),
        (libc::RLIMIT_NOFILE, limits.open_files),
    ] {
        if let Err(e) = set_limit(resource, value, limits.binding) {
            first_err.get_or_insert(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The type of a `RLIMIT_*` selector: `__rlimit_resource_t` (`u32`) on glibc
/// Linux, `c_int` on macOS/BSD and musl. The constants already have it, so they
/// pass without a cast.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) type RlimitResource = libc::c_int;

/// Set the `resource` limit to `value` (no-op when `value` is `None`), clamped
/// to the inherited hard ceiling so `setrlimit` cannot `EINVAL` on
/// `rlim_cur > rlim_max`.
///
/// Without `binding` only the SOFT limit is set and the inherited hard ceiling
/// is kept, so the `$SHELL` can raise its soft limit from its rc (a `.zshrc`
/// `ulimit -n 65536`), which a lowered hard limit turns into `ulimit: value
/// exceeds hard limit` at startup. With `binding` the hard limit is lowered to
/// the same value, so the program cannot raise it back
/// ([`crate::Limits::contained`]).
pub(crate) fn set_limit(
    resource: RlimitResource,
    value: Option<u64>,
    binding: bool,
) -> io::Result<()> {
    let Some(v) = value else {
        return Ok(());
    };
    // A bare syscall, no allocation: async-signal-safe in the post-fork child.
    // If the read fails, set soft and hard to `v` rather than leave it unconfined.
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
    let soft = core::cmp::min(v as libc::rlim_t, hard);
    let lim = libc::rlimit {
        rlim_cur: soft,
        rlim_max: if binding { soft } else { hard },
    };
    // SAFETY: `resource` is a valid RLIMIT_* constant and `&lim` is a valid,
    // fully-initialized `rlimit` for the duration of the call.
    let rc = unsafe { libc::setrlimit(resource, &lim) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

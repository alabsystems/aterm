// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX actuator for [`crate::Limits::apply`].

use std::io;

use crate::Limits;

/// Attempt every requested limit and return the first error after all were tried.
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

/// The platform type of an `RLIMIT_*` selector.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) type RlimitResource = libc::c_int;

/// Set the `resource` limit to `value`, clamped to the inherited hard ceiling.
pub(crate) fn set_limit(
    resource: RlimitResource,
    value: Option<u64>,
    binding: bool,
) -> io::Result<()> {
    let Some(v) = value else {
        return Ok(());
    };
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
    // SAFETY: `resource` is a valid RLIMIT_* constant and `&lim` is a valid, fully-initialized
    // `rlimit` for the duration of the call.
    let rc = unsafe { libc::setrlimit(resource, &lim) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

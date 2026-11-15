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

/// Set the `resource` limit to `value`, clamped to the inherited hard ceiling ([`limit_for`]).
pub(crate) fn set_limit(
    resource: RlimitResource,
    value: Option<u64>,
    binding: bool,
) -> io::Result<()> {
    let Some(lim) = limit_for(resource, value, binding)? else {
        return Ok(());
    };
    // SAFETY: `resource` is a valid RLIMIT_* constant and `&lim` is a valid, fully-initialized
    // `rlimit` for the duration of the call.
    let rc = unsafe { libc::setrlimit(resource, &lim) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// What `resource` is set to for `value` (`None`: nothing asked): `value` as the soft limit,
/// clamped to this process's hard ceiling, and as the hard limit too when `binding`, else that
/// ceiling kept.
pub(crate) fn limit_for(
    resource: RlimitResource,
    value: Option<u64>,
    binding: bool,
) -> io::Result<Option<libc::rlimit>> {
    let Some(v) = value else {
        return Ok(None);
    };
    let mut cur = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid resource id + a valid out-param for the call's duration.
    if unsafe { libc::getrlimit(resource, &mut cur) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let hard = cur.rlim_max;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::unnecessary_cast,
        reason = "rlim_t width and signedness vary across Unix targets"
    )]
    let capped = v.min(libc::rlim_t::MAX as u64) as libc::rlim_t;
    let soft = core::cmp::min(capped, hard);
    Ok(Some(libc::rlimit {
        rlim_cur: soft,
        rlim_max: if binding { soft } else { hard },
    }))
}

/// Each limit `limits` asks for, as [`apply_limits`] would set it in this process.
pub(crate) fn plan_limits(limits: &Limits) -> io::Result<Vec<crate::Rlimit>> {
    let mut planned = Vec::new();
    for (resource, value) in [
        (libc::RLIMIT_CPU, limits.cpu_seconds),
        (libc::RLIMIT_AS, limits.address_space),
        (libc::RLIMIT_FSIZE, limits.file_size),
        (libc::RLIMIT_NOFILE, limits.open_files),
    ] {
        if let Some(lim) = limit_for(resource, value, limits.binding)? {
            #[allow(
                clippy::unnecessary_cast,
                clippy::cast_sign_loss,
                reason = "an RLIMIT_* selector is a small non-negative number, `c_uint` or `c_int` \
                          by target; `rlim_t` is 64 bits on every Unix target here"
            )]
            planned.push(crate::Rlimit {
                resource: resource as u32,
                soft: lim.rlim_cur as u64,
                hard: lim.rlim_max as u64,
            });
        }
    }
    Ok(planned)
}

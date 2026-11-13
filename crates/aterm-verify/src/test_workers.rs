// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CPU share a test run's binaries are charged from ([`crate::test_plan`]): a release's pool
//! ([`aterm_capacity::POOL_ENV`]), a held build slot's `CARGO_BUILD_JOBS`, or the host's cores
//! where neither is.

use crate::EnvSnapshot;

/// Where a test run's share of cores comes from ([`share`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Share {
    /// The release's pool ([`aterm_capacity::POOL_ENV`], [`crate::Ctx::pool`]), of this many
    /// slots: the release's own Release core claim bounds it, so the run makes no core claim of
    /// its own (one would count the release's other branches' claims and wait for their end),
    /// and each binary that runs holds its charge in the pool's tokens
    /// ([`crate::testrun::dispatch_pooled`]).
    Pool(u32),
    /// The caller's admitted share ([`admitted_share`]), or none: the run claims its cores.
    Claim(Option<u32>),
}

/// The test run's [`Share`]: the pool the run adopted, which wins over an admitted share; else
/// [`admitted_share`].
///
/// # Errors
/// A pool is named in the environment and none was adopted: it is refused, never replaced by a
/// claim of the host's cores beside the release's own; or [`admitted_share`]'s.
pub(crate) fn share(
    env: &EnvSnapshot,
    pool: Option<&aterm_capacity::Jobserver>,
) -> Result<Share, &'static str> {
    match (pool, &env.release_pool) {
        (Some(pool), _) => Ok(Share::Pool(pool.slots())),
        (None, Some(_)) => Err("ATERM_RELEASE_POOL is set but this run adopted no pool"),
        (None, None) => admitted_share(env).map(Share::Claim),
    }
}

/// The share a held build slot grants (`BUILD_SLOT_HELD=1` and its positive decimal
/// `CARGO_BUILD_JOBS`); `None` where no slot is held.
///
/// # Errors
/// A slot is held and its share is missing or malformed: the run refuses before any check.
pub(crate) fn admitted_share(env: &EnvSnapshot) -> Result<Option<u32>, &'static str> {
    let Some(held) = &env.build_slot_held else {
        return Ok(None);
    };
    if held != "1" {
        return Err("BUILD_SLOT_HELD must be 1 when set");
    }
    env.cargo_build_jobs
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|raw| !raw.is_empty() && raw.bytes().all(|c| c.is_ascii_digit()))
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|&share| share > 0)
        .map(Some)
        .ok_or("BUILD_SLOT_HELD=1 requires positive decimal CARGO_BUILD_JOBS")
}

#[cfg(test)]
#[path = "test_workers_tests.rs"]
mod tests;

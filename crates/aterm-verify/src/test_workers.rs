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
mod tests {
    use super::*;

    /// A run under the release's pool takes the pool's slots as its share and claims nothing;
    /// without one it keeps the caller's admitted share or claims host cores; a pool named but
    /// not adopted refuses. Negative control: a `share` that ignores the pool answers `Claim`.
    #[cfg(unix)]
    #[test]
    fn the_release_pool_is_the_share_and_replaces_the_core_claim() {
        use std::path::PathBuf;
        let dir = aterm_tempfile::tempdir().unwrap();
        let release = aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 5).unwrap();
        let handed = release.handed();
        let mut env = EnvSnapshot {
            release_pool: Some(handed.clone()),
            build_slot_held: Some("1".into()),
            cargo_build_jobs: Some("3".into()),
            ..EnvSnapshot::default()
        };
        let adopted = aterm_capacity::Jobserver::adopt(&handed).unwrap();
        assert_eq!(share(&env, Some(&adopted)), Ok(Share::Pool(5)));
        assert!(
            share(&env, None).is_err(),
            "a named pool is never replaced by a claim"
        );
        env.release_pool = None;
        assert_eq!(share(&env, None), Ok(Share::Claim(Some(3))));
        env.build_slot_held = None;
        assert_eq!(share(&env, None), Ok(Share::Claim(None)));

        let make = |pool| {
            crate::Ctx::new(
                PathBuf::from("/repo"),
                crate::Scope::workspace(),
                EnvSnapshot {
                    release_pool: Some(handed.clone()),
                    ..EnvSnapshot::default()
                },
                PathBuf::from("/tmp"),
            )
            .with_pool(pool)
        };
        let pooled = make(Some(adopted));
        assert_eq!((pooled.test_share(), pooled.test_threads()), (5, 5));
        assert_eq!(pooled.test_thread_override(), Some(5));
        assert!(
            pooled
                .child_env_add
                .iter()
                .any(|(k, v)| k == "RUST_TEST_THREADS" && v == "5"),
            "builders and serial fallbacks carry the pool's width"
        );
        assert_eq!(make(None).test_share(), 1, "unadopted: no unbounded share");
    }

    #[test]
    fn managed_runs_refuse_missing_or_malformed_admission_data() {
        let mut env = EnvSnapshot::default();
        assert_eq!(admitted_share(&env), Ok(None));
        env.build_slot_held = Some("1".into());
        for raw in [
            None,
            Some(""),
            Some("0"),
            Some("-1"),
            Some("+6"),
            Some(" 6"),
            Some("6 "),
            Some("4294967296"),
            Some("six"),
        ] {
            env.cargo_build_jobs = raw.map(Into::into);
            assert!(admitted_share(&env).is_err(), "{raw:?}");
        }
        env.cargo_build_jobs = Some("6".into());
        assert_eq!(admitted_share(&env), Ok(Some(6)));
        env.build_slot_held = Some("0".into());
        assert!(admitted_share(&env).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            env.build_slot_held = Some("1".into());
            env.cargo_build_jobs = Some(std::ffi::OsString::from_vec(vec![0xff]));
            assert!(admitted_share(&env).is_err());
        }
    }

    /// A held slot's share bounds every binary's width, and the caller's `RUST_TEST_THREADS` or
    /// `--test-threads` narrows it; every child carries that ceiling once, the last request
    /// winning, and `CARGO_BUILD_JOBS` stays the build's. NEGATIVE CONTROL: with no slot held and
    /// no pin, no child is pinned at all.
    #[test]
    fn context_preferences_change_test_workers_without_spending_build_jobs() {
        use std::num::NonZeroU32;
        use std::path::PathBuf;
        let make = |held, pin: Option<&str>| {
            crate::Ctx::new(
                PathBuf::from("/repo"),
                crate::Scope::workspace(),
                EnvSnapshot {
                    build_slot_held: held,
                    cargo_build_jobs: Some("6".into()),
                    rust_test_threads: pin.map(Into::into),
                    ..EnvSnapshot::default()
                },
                PathBuf::from("/tmp"),
            )
        };
        let pins = |ctx: &crate::Ctx| -> Vec<String> {
            ctx.child_env_add
                .iter()
                .filter(|(k, _)| k == "RUST_TEST_THREADS")
                .map(|(_, v)| v.to_string_lossy().into_owned())
                .collect()
        };
        let managed = make(Some("1".into()), Some("2"));
        assert_eq!((managed.test_share(), managed.test_threads()), (6, 2));
        assert_eq!(managed.build_jobs_ceiling(), Some(6));
        assert_eq!(pins(&managed), ["2"]);
        let wider = managed.with_test_threads(NonZeroU32::new(3));
        assert_eq!(
            (wider.test_threads(), pins(&wider)),
            (3, vec!["3".to_owned()])
        );
        assert_eq!(wider.build_jobs_ceiling(), Some(6));
        let huge = make(Some("1".into()), None).with_test_threads(NonZeroU32::new(u32::MAX));
        assert_eq!(
            (huge.test_threads(), pins(&huge)),
            (6, vec!["6".to_owned()])
        );
        let default = make(Some("1".into()), None);
        assert_eq!(default.test_thread_override(), Some(6));
        assert_eq!(default.test_threads_within(4), 4, "within a smaller grant");
        let unmanaged = make(None, None);
        assert_eq!(unmanaged.test_share(), unmanaged.cores());
        assert_eq!(unmanaged.test_thread_override(), None);
        assert!(pins(&unmanaged).is_empty());
    }

    #[test]
    fn unmanaged_requests_share_host_cores_without_treating_build_jobs_as_a_grant() {
        use std::{num::NonZeroU32, path::PathBuf};
        for build_jobs in [None, Some("1"), Some("4294967295")] {
            let ctx = crate::Ctx::new(
                PathBuf::from("/repo"),
                crate::Scope::workspace(),
                EnvSnapshot {
                    cargo_build_jobs: build_jobs.map(Into::into),
                    ..EnvSnapshot::default()
                },
                PathBuf::from("/tmp"),
            );
            assert_eq!(
                (ctx.test_share(), ctx.test_threads()),
                (ctx.cores(), ctx.cores())
            );
            let huge = ctx.with_test_threads(NonZeroU32::new(u32::MAX));
            assert_eq!(huge.test_threads(), huge.cores());
            assert_eq!(huge.test_thread_override(), Some(huge.cores()));
            assert_eq!(
                huge.child_env_add
                    .iter()
                    .filter(|(key, _)| key == "RUST_TEST_THREADS")
                    .map(|(_, value)| value.to_str().unwrap())
                    .collect::<Vec<_>>(),
                [huge.cores().to_string()]
            );
        }
    }
}

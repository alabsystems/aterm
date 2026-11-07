// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test binaries and their libtest threads share one CPU ceiling.

use crate::EnvSnapshot;

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

/// The most binaries at once and the widest binary, within `share`: each preference is a
/// ceiling, and an absent one is the share. What the binaries running hold together is their
/// charges, which [`crate::test_plan::dispatch`] keeps within the share.
pub(crate) fn ceilings(jobs: Option<u32>, threads: Option<u32>, share: u32) -> (u32, u32) {
    let share = share.max(1);
    (
        jobs.unwrap_or(share).clamp(1, share),
        threads.unwrap_or(share).clamp(1, share),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_ceilings_stay_within_the_share_without_overflow() {
        for share in [0, 1, 2, 6, 32, u32::MAX] {
            for jobs in [None, Some(1), Some(2), Some(8), Some(u32::MAX)] {
                for threads in [None, Some(1), Some(2), Some(6), Some(32), Some(u32::MAX)] {
                    let (j, t) = ceilings(jobs, threads, share);
                    assert!(j > 0 && t > 0);
                    assert!(j <= share.max(1) && t <= share.max(1));
                    assert!(jobs.is_none_or(|asked| j <= asked));
                    assert!(threads.is_none_or(|asked| t <= asked));
                }
            }
        }
        assert_eq!(ceilings(None, None, 6), (6, 6));
        assert_eq!(ceilings(Some(8), Some(32), 6), (6, 6));
        assert_eq!(ceilings(Some(2), Some(3), 6), (2, 3));
        assert_eq!(ceilings(Some(0), Some(0), 0), (1, 1));
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

    #[test]
    fn context_preferences_change_test_workers_without_spending_build_jobs() {
        use std::num::NonZeroU32;
        use std::path::PathBuf;
        let make = |held| {
            crate::Ctx::new(
                PathBuf::from("/repo"),
                crate::Scope::workspace(),
                EnvSnapshot {
                    build_slot_held: held,
                    cargo_build_jobs: Some("6".into()),
                    rust_test_threads: Some("2".into()),
                    ..EnvSnapshot::default()
                },
                PathBuf::from("/tmp"),
            )
        };
        let managed = make(Some("1".into()));
        assert_eq!((managed.test_jobs(), managed.test_threads()), (6, 2));
        assert_eq!(managed.build_jobs_ceiling(), Some(6));
        let forward = managed
            .with_test_jobs(NonZeroU32::new(2))
            .with_test_threads(NonZeroU32::new(3));
        let reverse = make(Some("1".into()))
            .with_test_threads(NonZeroU32::new(3))
            .with_test_jobs(NonZeroU32::new(2));
        for ctx in [forward, reverse] {
            assert_eq!((ctx.test_jobs(), ctx.test_threads()), (2, 3));
            assert_eq!(ctx.build_jobs_ceiling(), Some(6));
            let pins: Vec<_> = ctx
                .child_env_add
                .iter()
                .filter(|(k, _)| k == "RUST_TEST_THREADS")
                .collect();
            assert_eq!(pins.len(), 1);
            assert_eq!(pins[0].1, "3");
        }
        let huge = make(Some("1".into()))
            .with_test_jobs(NonZeroU32::new(u32::MAX))
            .with_test_threads(NonZeroU32::new(u32::MAX));
        assert_eq!((huge.test_jobs(), huge.test_threads()), (6, 6));
        let small = make(Some("1".into())).with_test_jobs(NonZeroU32::new(1));
        assert_eq!((small.test_jobs(), small.test_threads()), (1, 2));
        let unmanaged = make(None);
        assert_eq!(
            (unmanaged.test_jobs(), unmanaged.test_threads()),
            (
                usize::try_from(unmanaged.cores()).unwrap(),
                unmanaged.cores().min(2)
            )
        );
        let default = crate::Ctx::new(
            PathBuf::from("/repo"),
            crate::Scope::workspace(),
            EnvSnapshot {
                build_slot_held: Some("1".into()),
                cargo_build_jobs: Some("6".into()),
                ..EnvSnapshot::default()
            },
            PathBuf::from("/tmp"),
        );
        assert_eq!((default.test_jobs(), default.test_threads()), (6, 6));
        assert_eq!(default.test_thread_override(), Some(default.test_threads()));
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
            let cores = usize::try_from(ctx.cores()).unwrap();
            assert_eq!((ctx.test_jobs(), ctx.test_threads()), (cores, ctx.cores()));
            let huge = ctx
                .with_test_jobs(NonZeroU32::new(u32::MAX))
                .with_test_threads(NonZeroU32::new(u32::MAX));
            assert_eq!(
                (huge.test_jobs(), huge.test_threads()),
                (cores, huge.cores())
            );
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

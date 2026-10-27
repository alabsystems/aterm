// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test binaries and their libtest threads share one admitted CPU allowance.

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

/// Preferences are ceilings. Division avoids overflow even for u32::MAX requests.
pub(crate) fn counts(jobs: u32, threads: u32, share: Option<u32>) -> (u32, u32) {
    let (jobs, threads) = (jobs.max(1), threads.max(1));
    let Some(share) = share else {
        return (jobs, threads);
    };
    let threads = threads.min(share.max(1));
    (jobs.min(share.max(1) / threads), threads)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_test_workers_share_one_allowance_without_overflow() {
        for share in [1, 2, 6, 32, u32::MAX] {
            for jobs in [1, 2, 8, u32::MAX] {
                for threads in [1, 2, 6, 32, u32::MAX] {
                    let (actual_jobs, actual_threads) = counts(jobs, threads, Some(share));
                    assert!(actual_jobs > 0 && actual_threads > 0);
                    assert!(actual_jobs <= jobs && actual_threads <= threads);
                    assert!(u64::from(actual_jobs) * u64::from(actual_threads) <= u64::from(share));
                }
            }
        }
        assert_eq!(counts(8, 32, Some(6)), (1, 6));
        assert_eq!(counts(8, 2, Some(6)), (3, 2));
        assert_eq!(counts(2, 2, Some(6)), (2, 2));
        assert_eq!(counts(8, 32, None), (8, 32));
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
        assert_eq!((managed.test_jobs(), managed.test_threads()), (3, 2));
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
        assert_eq!((huge.test_jobs(), huge.test_threads()), (1, 6));
        let small = make(Some("1".into())).with_test_jobs(NonZeroU32::new(1));
        assert_eq!((small.test_jobs(), small.test_threads()), (1, 2));
        let unmanaged = make(None);
        assert_eq!((unmanaged.test_jobs(), unmanaged.test_threads()), (8, 2));
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
        assert_eq!(default.test_threads(), default.cores().min(6));
        assert!(
            u64::try_from(default.test_jobs()).expect("job count")
                * u64::from(default.test_threads())
                <= 6
        );
        assert_eq!(default.test_thread_override(), Some(default.test_threads()));
    }
}

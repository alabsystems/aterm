// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::time::{Duration, Instant};

#[test]
fn the_windows_required_lock_policy_waits_for_its_native_writer() {
    for age in [Duration::from_secs(6), Duration::from_secs(61)] {
        let expired = age >= Duration::from_secs(60);
        let root = crate::test_scratch_dir("required-lock-policy");
        let path = root.path().join("apply.lock");
        let mut held = Some(FileLock::acquire(&path).unwrap());
        let busy = FileLock::acquire_within(&path, Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(busy.kind(), io::ErrorKind::TimedOut);
        let mut observed = 0;
        let result = FileLock::acquire_required(&path, Instant::now() - age, |path, left| {
            let entered = Instant::now();
            let busy = FileLock::acquire_within(path, Duration::ZERO)
                .err()
                .expect("the actual native writer is held");
            assert_eq!(busy.kind(), io::ErrorKind::TimedOut);
            observed += 1;
            drop(held.take());
            FileLock::acquire_within(path, left.saturating_sub(entered.elapsed()))
        });
        assert_eq!(observed, usize::from(!expired));
        assert_eq!(held.is_some(), expired);
        assert_eq!(result.is_err(), expired);
        assert_eq!(
            FileLock::acquire_within(&path, Duration::ZERO)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::TimedOut
        );
        drop(result);
        drop(held);
        FileLock::acquire_within(&path, Duration::ZERO).unwrap();
    }
}

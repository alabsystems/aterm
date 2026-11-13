// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::FileLock;
use std::time::Duration;

/// A held lock times out `acquire_within` and is immediately free once dropped.
#[test]
fn acquire_within_times_out_while_another_holder_has_it() {
    let dir = crate::test_scratch_dir("bounded-lock");
    let path = dir.path().join("apply.lock");

    let held = FileLock::acquire(&path).expect("first holder takes the lock");
    let error = match FileLock::acquire_within(&path, Duration::from_millis(50)) {
        Ok(_) => panic!("a held lock must not be acquirable"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");

    drop(held);
    FileLock::acquire_within(&path, Duration::ZERO)
        .expect("the dropped lock must be free at the first try");
}

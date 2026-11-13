// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::bound_start_seconds;

#[test]
fn seconds_require_the_snapshots_current_kernel_start() {
    for current in [None, Some("replacement-process")] {
        assert_eq!(
            bound_start_seconds("recorded-process", current, || {
                panic!("a different process must not supply its birth")
            }),
            None
        );
    }
    assert_eq!(
        bound_start_seconds("recorded-process", Some("recorded-process"), || Some(42)),
        Some(42)
    );
    assert_eq!(
        bound_start_seconds("recorded-process", Some("recorded-process"), || None),
        None
    );
}

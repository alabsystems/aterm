// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn native_admission_never_parks_behind_an_app_handoff_or_cuts_one_in_half() {
    let boundary = Boundary::new();
    let latch = boundary.latch_handoff(Duration::from_secs(60));
    assert!(
        boundary
            .try_hold(GuardedItem::HarnessNativeMutation)
            .is_none()
    );
    drop(latch);
    let native = boundary
        .try_hold(GuardedItem::HarnessNativeMutation)
        .unwrap();
    assert_eq!(
        boundary.any_held(),
        Some(GuardedItem::HarnessNativeMutation)
    );
    let latch = boundary.latch_handoff(Duration::from_secs(60));
    assert!(boundary.is_held(GuardedItem::HarnessNativeMutation));
    drop(native);
    assert!(!boundary.is_held(GuardedItem::HarnessNativeMutation));
    drop(latch);
}

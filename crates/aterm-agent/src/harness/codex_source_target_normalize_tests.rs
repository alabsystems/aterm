// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn normalization_cleanup_counts_without_pinning_original_operation() {
    // Actual App operations and drain protocol, without any synthetic
    // Applied receipt, owner, Slot, or target admission authority.
    let app = crate::harness::account_work::Lifetime::default();
    let scope = app.enter().unwrap();
    let capture = scope.clone();
    let custody = RetirementCustody::new(&scope).unwrap();
    let pause = app.pause().unwrap();
    drop(scope);
    assert!(!custody.original.retired());
    let pause = pause
        .try_wait()
        .expect_err("actual original capture remains");
    drop(capture);
    assert!(
        custody.original.retired(),
        "cleanup must not self-pin original"
    );
    let pause = pause.try_wait().expect_err("retry custody remains counted");
    // Kernel-departure abandonment uses this same release path; it grants
    // no normalization/finalization observation.
    custody.release();
    custody.release();
    pause.try_wait().unwrap().resume().unwrap();
}

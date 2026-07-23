// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Saturation boundary and `try_extend_with`/`try_push` overflow tests (#5010).

use crate::{Rle, RleCapacityError, Run};

/// Helper: verify `total_length == sum(run.length)` invariant.
fn assert_invariant(rle: &Rle<u8>, label: &str) {
    let run_total: u32 = rle.runs().iter().map(|run| run.length).sum();
    assert_eq!(run_total, rle.len(), "invariant broken: {label}");
}

/// try_extend_with succeeds when there is sufficient capacity.
#[test]
fn try_extend_with_succeeds_within_capacity() {
    let mut rle = Rle::<u8>::new();
    assert!(rle.try_extend_with(1, 100).is_ok());
    assert_eq!(rle.len(), 100);
    assert_invariant(&rle, "after try_extend_with within capacity");
}

/// try_extend_with returns an error when count exceeds remaining capacity.
#[test]
fn try_extend_with_rejects_overflow() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX);

    let err = rle.try_extend_with(2, 1).unwrap_err();
    assert_eq!(err.requested, 1);
    assert_eq!(err.available, 0);

    // RLE must be unchanged after the rejected operation.
    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(rle.run_count(), 1);
    assert_invariant(&rle, "after rejected try_extend_with");
}

/// try_extend_with rejects partial overflow (some capacity but not enough).
#[test]
fn try_extend_with_rejects_partial_overflow() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX - 5);

    let err = rle.try_extend_with(2, 10).unwrap_err();
    assert_eq!(err.requested, 10);
    assert_eq!(err.available, 5);

    // RLE must be unchanged — no partial insertion.
    assert_eq!(rle.len(), u32::MAX - 5);
    assert_eq!(rle.run_count(), 1);
    assert_invariant(&rle, "after rejected partial try_extend_with");
}

/// try_push succeeds when there is capacity.
#[test]
fn try_push_succeeds_within_capacity() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX - 1);

    assert!(rle.try_push(2).is_ok());
    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(rle.run_count(), 2);
    assert_invariant(&rle, "after try_push within capacity");
}

/// try_push returns an error when at capacity.
#[test]
fn try_push_rejects_at_capacity() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX);

    let err = rle.try_push(2).unwrap_err();
    assert_eq!(err.requested, 1);
    assert_eq!(err.available, 0);

    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(rle.run_count(), 1);
    assert_invariant(&rle, "after rejected try_push");
}

/// remaining_capacity reports correct values.
#[test]
fn remaining_capacity_accurate() {
    let mut rle = Rle::<u8>::new();
    assert_eq!(rle.remaining_capacity(), u32::MAX);

    rle.extend_with(1, 100);
    assert_eq!(rle.remaining_capacity(), u32::MAX - 100);

    rle.extend_with(2, u32::MAX - 100);
    assert_eq!(rle.remaining_capacity(), 0);
}

/// RleCapacityError Display implementation.
#[test]
fn capacity_error_display() {
    let err = RleCapacityError {
        requested: 10,
        available: 5,
    };
    let msg = err.to_string();
    assert!(msg.contains("10"), "should show requested count");
    assert!(msg.contains("5"), "should show available count");
}

/// Issue #5010 reproduction case: invariant is preserved after saturation.
///
/// The issue claimed `runs = [(1, u32::MAX), (2, 100)]` after double extend,
/// but the clamping logic prevents this — the second extend produces 0 elements.
#[test]
fn issue_5010_reproduction_invariant_preserved() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX);
    rle.extend_with(2, 100);

    // The second extend is clamped to 0 — no second run created.
    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(rle.run_count(), 1);
    assert_eq!(
        rle.runs(),
        &[Run {
            value: 1,
            length: u32::MAX
        }]
    );
    assert_invariant(&rle, "issue #5010 reproduction");
}

/// Verify invariant holds after a sequence of saturating operations then
/// mutations (truncate, set, set_range, resize).
#[test]
fn saturation_then_full_mutation_sequence() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX - 2);
    rle.extend_with(2, 10); // Clamped to 2.
    assert_eq!(rle.len(), u32::MAX);
    assert_invariant(&rle, "after clamped extend");

    // Truncate below the second run boundary.
    rle.resize(u32::MAX - 3);
    assert_eq!(rle.len(), u32::MAX - 3);
    assert_invariant(&rle, "after truncate");

    // set at the new last index.
    assert!(rle.set(u32::MAX - 4, 5));
    assert_eq!(rle.get(u32::MAX - 4), Some(5));
    assert_invariant(&rle, "after set");

    // set_range near the boundary.
    rle.set_range(u32::MAX - 6, u32::MAX - 3, 6);
    for i in (u32::MAX - 6)..(u32::MAX - 3) {
        assert_eq!(rle.get(i), Some(6));
    }
    assert_invariant(&rle, "after set_range");

    // Grow back to near-max.
    rle.resize(u32::MAX);
    assert_eq!(rle.len(), u32::MAX);
    assert_invariant(&rle, "after resize grow to max");
}

/// Verify try_extend_with with same-value merge path at capacity boundary.
#[test]
fn try_extend_with_same_value_at_boundary() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX - 5);

    // Same-value merge: exactly fills remaining capacity.
    assert!(rle.try_extend_with(1, 5).is_ok());
    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(rle.run_count(), 1);
    assert_invariant(&rle, "same-value merge at exact boundary");

    // Now at capacity — same-value merge should also fail.
    let err = rle.try_extend_with(1, 1).unwrap_err();
    assert_eq!(err.available, 0);
    assert_invariant(&rle, "same-value merge rejected at capacity");
}

// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Index<u32> valid access via `rle[i]` syntax.
#[test]
fn rle_index_trait_valid_access() {
    let rle = Rle::from_iter([10u8, 20, 20, 30]);
    assert_eq!(rle[0], 10);
    assert_eq!(rle[1], 20);
    assert_eq!(rle[2], 20);
    assert_eq!(rle[3], 30);
}

/// Index<u32> panics on out-of-bounds access.
#[test]
#[should_panic(expected = "index out of bounds")]
fn rle_index_trait_oob_panics() {
    let rle = Rle::from_iter([1u8, 2, 3]);
    let _ = rle[3]; // OOB: valid indices are 0..3
}

/// ExactSizeIterator reports correct remaining count after partial iteration.
#[test]
fn rle_exact_size_iter_after_partial_iteration() {
    let rle = Rle::from_iter([1u8, 1, 2, 3, 3, 3]);
    let mut iter = rle.iter();
    assert_eq!(iter.len(), 6);

    iter.next(); // consume 1st
    assert_eq!(iter.len(), 5);

    iter.next(); // consume 2nd
    iter.next(); // consume 3rd
    assert_eq!(iter.len(), 3);

    iter.next(); // consume 4th
    iter.next(); // consume 5th
    iter.next(); // consume 6th (last)
    assert_eq!(iter.len(), 0);

    // Exhausted — still reports 0
    assert_eq!(iter.next(), None);
    assert_eq!(iter.len(), 0);
}

/// FromIterator with empty input produces empty RLE.
#[test]
fn rle_from_iter_empty() {
    let rle = Rle::from_iter(std::iter::empty::<u8>());
    assert!(rle.is_empty());
    assert_eq!(rle.len(), 0);
    assert_eq!(rle.run_count(), 0);
}

/// set_range with start > end is a no-op (guard at line 282).
#[test]
fn rle_set_range_start_greater_than_end() {
    let mut rle = Rle::from_iter([1u8, 2, 3]);
    rle.set_range(2, 1, 9); // start > end
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(1), Some(2));
    assert_eq!(rle.get(2), Some(3));
}

/// set_range on empty RLE is a no-op.
#[test]
fn rle_set_range_on_empty() {
    let mut rle: Rle<u8> = Rle::new();
    rle.set_range(0, 5, 9); // empty RLE — start >= total_length
    assert!(rle.is_empty());
}

/// with_value(_, 0) returns empty RLE (early return at line 129).
#[test]
fn rle_with_value_zero_length() {
    let rle = Rle::with_value(42u8, 0);
    assert!(rle.is_empty());
    assert_eq!(rle.len(), 0);
    assert_eq!(rle.run_count(), 0);
}

/// resize_with direct test — grow and shrink.
#[test]
fn rle_resize_with_grow_and_shrink() {
    let mut rle = Rle::from_iter([1u8, 2]);

    // Grow with specific value
    rle.resize_with(5, 7);
    assert_eq!(rle.len(), 5);
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(1), Some(2));
    assert_eq!(rle.get(2), Some(7));
    assert_eq!(rle.get(4), Some(7));

    // Shrink
    rle.resize_with(3, 99); // value ignored on shrink
    assert_eq!(rle.len(), 3);
    assert_eq!(rle.get(2), Some(7));
    assert_eq!(rle.get(3), None);

    // Resize to 0
    rle.resize_with(0, 1);
    assert!(rle.is_empty());

    // Same length is no-op
    let mut rle2 = Rle::with_value(5u8, 3);
    rle2.resize_with(3, 9);
    assert_eq!(rle2.len(), 3);
    assert_eq!(rle2.get(0), Some(5));
}

/// Default trait instantiation via ::default().
#[test]
fn rle_default_trait() {
    let rle: Rle<u8> = Rle::default();
    assert!(rle.is_empty());
    assert_eq!(rle.len(), 0);
    assert_eq!(rle.run_count(), 0);

    let run: Run<u8> = Run::default();
    assert_eq!(run.value, 0);
    assert_eq!(run.length, 0);
}

/// IntoIterator for &Rle via `for x in &rle` syntax.
#[test]
fn rle_into_iterator_ref() {
    let rle = Rle::from_iter([1u8, 1, 2, 3]);
    let mut collected = Vec::new();
    for val in &rle {
        collected.push(val);
    }
    assert_eq!(collected, vec![1, 1, 2, 3]);
}

/// Verify extend_with clamps to remaining capacity instead of desynchronizing.
#[test]
fn extend_with_clamps_on_overflow() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX);
    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(
        rle.runs(),
        &[Run {
            value: 1,
            length: u32::MAX
        }]
    );

    // Second extend should be clamped away once no capacity remains.
    rle.extend_with(2, 1);
    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(rle.run_count(), 1);

    let run_total: u32 = rle.runs().iter().map(|run| run.length).sum();
    assert_eq!(
        run_total,
        rle.len(),
        "run lengths must stay exact at capacity"
    );

    // Same-value merge path should clamp to the remaining slack.
    let mut rle2 = Rle::<u8>::new();
    rle2.extend_with(1, u32::MAX - 10);
    rle2.extend_with(1, 20); // Only 10 cells of capacity remain.
    assert_eq!(rle2.len(), u32::MAX);
    assert_eq!(rle2.run_count(), 1);
    assert_eq!(rle2.runs()[0].length, u32::MAX);
    assert_eq!(rle2.get(u32::MAX - 1), Some(1));
}

/// Verify push becomes a no-op once the sequence is at capacity.
#[test]
fn push_at_capacity_is_no_op() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX);
    assert_eq!(rle.len(), u32::MAX);
    rle.push(1); // Same-value merge path.
    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(rle.run_count(), 1);
    rle.push(2); // New-run path.
    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(rle.run_count(), 1);
    assert_eq!(
        rle.runs(),
        &[Run {
            value: 1,
            length: u32::MAX
        }]
    );
}

/// Regression: post-clamp mutations must preserve exact length accounting.
#[test]
fn post_clamp_mutations_preserve_invariant() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX - 1);
    rle.extend_with(2, 10); // Clamp to a single trailing cell.

    assert_eq!(
        rle.runs(),
        &[
            Run {
                value: 1,
                length: u32::MAX - 1,
            },
            Run {
                value: 2,
                length: 1,
            },
        ]
    );
    assert!(rle.set(u32::MAX - 1, 3));
    assert_eq!(rle.get(u32::MAX - 1), Some(3));

    rle.set_range(u32::MAX - 2, u32::MAX, 4);
    assert_eq!(rle.get(u32::MAX - 2), Some(4));
    assert_eq!(rle.get(u32::MAX - 1), Some(4));

    rle.resize(u32::MAX - 1);
    assert_eq!(rle.len(), u32::MAX - 1);
    assert_eq!(rle.get(u32::MAX - 1), None);

    let run_total: u32 = rle.runs().iter().map(|run| run.length).sum();
    assert_eq!(
        run_total,
        rle.len(),
        "post-clamp mutations must keep total_length exact"
    );
}

/// Regression: the linear find_run fallback must handle near-MAX offsets exactly.
#[test]
fn linear_find_run_after_clamp_handles_near_max_offsets() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX - 1);
    rle.extend_with(2, 10); // Clamp to a single trailing cell.

    // Force the linear fallback path instead of the cached prefix-sum search.
    rle.prefix_sums.clear();

    assert_eq!(rle.get(u32::MAX - 2), Some(1));
    assert_eq!(rle.get(u32::MAX - 1), Some(2));
    assert!(rle.set(u32::MAX - 1, 3));
    assert_eq!(rle.get(u32::MAX - 1), Some(3));

    let run_total: u32 = rle.runs().iter().map(|run| run.length).sum();
    assert_eq!(
        run_total,
        rle.len(),
        "linear fallback must preserve exact total_length accounting"
    );
}

/// Exhaustive small-domain test for `set` correctness (value written,
/// other values preserved, structural invariants). Covers len 1..=4,
/// all valid indices, and representative new_val values.
#[test]
fn rle_set_correctness_exhaustive() {
    for len in 1u8..=4 {
        for idx in 0..len {
            for &new_val in &[0u8, 1, 99, 255] {
                let mut rle = Rle::with_value(1u8, len as u32);
                let original_len = rle.len();
                rle.set(idx as u32, new_val);

                // Value correctness
                assert_eq!(
                    rle.get(idx as u32),
                    Some(new_val),
                    "set({idx},{new_val}) on len={len}: wrong value",
                );
                // Length preservation
                assert_eq!(
                    rle.len(),
                    original_len,
                    "set({idx},{new_val}) on len={len}: length changed",
                );
                // Other values preserved
                for other in 0..len {
                    if other != idx {
                        assert_eq!(
                            rle.get(other as u32),
                            Some(1),
                            "set({idx},{new_val}) on len={len}: value at {other} changed",
                        );
                    }
                }
                // Structural invariants
                for run in rle.runs() {
                    assert!(run.length > 0, "zero-length run");
                }
                let runs = rle.runs();
                for i in 0..runs.len().saturating_sub(1) {
                    assert_ne!(
                        runs[i].value,
                        runs[i + 1].value,
                        "adjacent duplicate at {i}",
                    );
                }
                let sum: u32 = runs.iter().map(|r| r.length).sum();
                assert_eq!(rle.len(), sum, "sum mismatch");
            }
        }
    }
}

/// Exhaustive small-domain test covering the exact input space of the
/// `rle_set_range_value_correctness` Kani proof (len 1..=4).
/// This proves algorithmically that set_range writes the correct value
/// for all valid (len, start, end, check_idx) combinations.
#[test]
fn rle_set_range_value_correctness_exhaustive() {
    for len in 1u8..=4 {
        for start in 0..len {
            for end in (start + 1)..=len {
                for check_idx in start..end {
                    let mut rle = Rle::with_value(1u8, len as u32);
                    rle.set_range(start as u32, end as u32, 42);
                    assert_eq!(
                        rle.get(check_idx as u32),
                        Some(42),
                        "set_range({start},{end},42) on len={len}: get({check_idx}) should be 42",
                    );
                }
            }
        }
    }
}

/// Exhaustive small-domain test for set_range preserving total length
/// (mirrors the Kani `rle_set_range_preserves_length` harness).
#[test]
fn rle_set_range_preserves_length_exhaustive() {
    for len in 1u8..=4 {
        for start in 0..len {
            for end in (start + 1)..=len {
                let mut rle = Rle::with_value(1u8, len as u32);
                if len > 1 {
                    rle.set(0, 10);
                }
                let original_len = rle.len();
                rle.set_range(start as u32, end as u32, 42);
                assert_eq!(
                    rle.len(),
                    original_len,
                    "set_range({start},{end},42) on len={len}: length changed",
                );
            }
        }
    }
}

/// Exhaustive small-domain test for set_range structural invariants
/// (mirrors the Kani `rle_set_range_structural_invariants` harness).
#[test]
fn rle_set_range_structural_invariants_exhaustive() {
    for len in 1u8..=4 {
        for start in 0..len {
            for end in (start + 1)..=len {
                let mut rle = Rle::with_value(1u8, len as u32);
                rle.set_range(start as u32, end as u32, 42);

                // No zero-length runs
                for run in rle.runs() {
                    assert!(
                        run.length > 0,
                        "set_range({start},{end},42) on len={len}: zero-length run",
                    );
                }

                // No adjacent duplicates
                let runs = rle.runs();
                for i in 0..runs.len().saturating_sub(1) {
                    assert_ne!(
                        runs[i].value,
                        runs[i + 1].value,
                        "set_range({start},{end},42) on len={len}: adjacent duplicate at {i}",
                    );
                }

                // Sum of run lengths == total_length
                let sum: u32 = runs.iter().map(|r| r.length).sum();
                assert_eq!(
                    rle.len(),
                    sum,
                    "set_range({start},{end},42) on len={len}: sum mismatch",
                );
            }
        }
    }
}

// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for run-length encoding primitives.

use super::*;

fn assert_rle_matches_model(rle: &Rle<u8>, model: &[u8], step: usize) {
    assert_eq!(
        rle.len() as usize,
        model.len(),
        "length mismatch at step {step}"
    );
    let run_total: u32 = rle.runs.iter().map(|run| run.length).sum();
    assert_eq!(
        run_total, rle.total_length,
        "sum of run lengths must match total_length at step {step}"
    );
    // The prefix-sum index this used to cross-check was deleted (it cost an
    // allocation per constructed Rle and no production caller read it). The
    // invariant it existed to protect — run starts accumulate exactly to
    // `total_length`, with no zero-length run — is checked directly on `runs`,
    // which is now the sole structure `find_run` walks.
    let mut expected_start = 0u32;
    for (run_idx, run) in rle.runs.iter().enumerate() {
        assert!(
            run.length > 0,
            "zero-length run at index {run_idx} (step {step})"
        );
        expected_start += run.length;
    }
    assert_eq!(
        expected_start, rle.total_length,
        "run starts must accumulate to total_length at step {step}"
    );

    for (idx, expected) in model.iter().copied().enumerate() {
        let index = idx as u32;
        assert_eq!(
            rle.get(index),
            Some(expected),
            "get({index}) mismatch at step {step}"
        );
        let (run_idx, offset_in_run) = rle
            .find_run(index)
            .expect("find_run should succeed for in-bounds index");
        assert!(
            offset_in_run < rle.runs[run_idx].length,
            "offset {} out of bounds for run {} (len {}) at step {}",
            offset_in_run,
            run_idx,
            rle.runs[run_idx].length,
            step
        );
        assert_eq!(
            rle.runs[run_idx].value, expected,
            "run value mismatch for index {index} at step {step}"
        );
    }
    assert_eq!(
        rle.get(model.len() as u32),
        None,
        "out-of-bounds get should return None at step {step}"
    );
}

#[test]
fn rle_new_empty() {
    let rle: Rle<u8> = Rle::new();
    assert!(rle.is_empty());
    assert_eq!(rle.len(), 0);
    assert_eq!(rle.run_count(), 0);
}

#[test]
fn rle_with_value() {
    let rle = Rle::with_value(42u8, 10);
    assert_eq!(rle.len(), 10);
    assert_eq!(rle.run_count(), 1);
    assert_eq!(rle.get(0), Some(42));
    assert_eq!(rle.get(9), Some(42));
    assert_eq!(rle.get(10), None);
}

#[test]
fn rle_push_same_value() {
    let mut rle: Rle<u8> = Rle::new();
    rle.push(1);
    rle.push(1);
    rle.push(1);
    assert_eq!(rle.len(), 3);
    assert_eq!(rle.run_count(), 1);
}

#[test]
fn rle_push_different_values() {
    let mut rle: Rle<u8> = Rle::new();
    rle.push(1);
    rle.push(2);
    rle.push(3);
    assert_eq!(rle.len(), 3);
    assert_eq!(rle.run_count(), 3);
}

#[test]
fn rle_from_iter() {
    let rle = Rle::from_iter([1u8, 1, 1, 2, 2, 3, 3, 3, 3]);
    assert_eq!(rle.len(), 9);
    assert_eq!(rle.run_count(), 3);
    assert_eq!(rle.runs()[0].length, 3);
    assert_eq!(rle.runs()[1].length, 2);
    assert_eq!(rle.runs()[2].length, 4);
}

#[test]
fn rle_get() {
    let rle = Rle::from_iter([1u8, 1, 2, 2, 2, 3]);
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(1), Some(1));
    assert_eq!(rle.get(2), Some(2));
    assert_eq!(rle.get(4), Some(2));
    assert_eq!(rle.get(5), Some(3));
    assert_eq!(rle.get(6), None);
}

#[test]
fn rle_set_same_value() {
    let mut rle = Rle::from_iter([1u8, 1, 1]);
    assert!(rle.set(1, 1));
    assert_eq!(rle.run_count(), 1);
}

#[test]
fn rle_set_middle() {
    let mut rle = Rle::from_iter([1u8, 1, 1, 1, 1]);
    assert!(rle.set(2, 9));
    assert_eq!(rle.get(2), Some(9));
    assert_eq!(rle.run_count(), 3);
    assert_eq!(rle.len(), 5);
}

#[test]
fn rle_set_start() {
    let mut rle = Rle::from_iter([1u8, 1, 1]);
    assert!(rle.set(0, 9));
    assert_eq!(rle.get(0), Some(9));
    assert_eq!(rle.run_count(), 2);
}

#[test]
fn rle_set_end() {
    let mut rle = Rle::from_iter([1u8, 1, 1]);
    assert!(rle.set(2, 9));
    assert_eq!(rle.get(2), Some(9));
    assert_eq!(rle.run_count(), 2);
}

#[test]
fn rle_set_range_entire() {
    let mut rle = Rle::from_iter([1u8, 2, 3, 4, 5]);
    rle.set_range(0, 5, 9);
    assert_eq!(rle.run_count(), 1);
    assert_eq!(rle.get(0), Some(9));
    assert_eq!(rle.get(4), Some(9));
}

#[test]
fn rle_set_range_partial() {
    let mut rle = Rle::from_iter([1u8, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
    rle.set_range(2, 5, 9);
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(2), Some(9));
    assert_eq!(rle.get(4), Some(9));
    assert_eq!(rle.get(5), Some(1));
}

#[test]
fn rle_set_range_across_runs() {
    let mut rle = Rle::from_iter([1u8, 1, 2, 2, 3, 3]);
    rle.set_range(1, 5, 9);
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(1), Some(9));
    assert_eq!(rle.get(4), Some(9));
    assert_eq!(rle.get(5), Some(3));
}

#[test]
fn rle_resize_grow() {
    let mut rle = Rle::from_iter([1u8, 1, 1]);
    rle.resize(5);
    assert_eq!(rle.len(), 5);
    assert_eq!(rle.get(3), Some(0)); // Default value
}

#[test]
fn rle_resize_shrink() {
    let mut rle = Rle::from_iter([1u8, 1, 1, 1, 1]);
    rle.resize(3);
    assert_eq!(rle.len(), 3);
    assert_eq!(rle.get(3), None);
}

#[test]
fn rle_iter() {
    let rle = Rle::from_iter([1u8, 1, 2, 3, 3]);
    let values: Vec<_> = rle.iter().collect();
    assert_eq!(values, vec![1, 1, 2, 3, 3]);
}

#[test]
fn rle_compact_on_set() {
    let mut rle = Rle::from_iter([1u8, 2, 1]);
    // Set middle to match adjacent
    rle.set(1, 1);
    assert_eq!(rle.run_count(), 1);
    assert_eq!(rle.len(), 3);
}

#[test]
fn rle_set_range_linear_iterations() {
    fn measure_once(run_count: u32) -> usize {
        let mut rle: Rle<u8> = Rle::new();
        for i in 0..run_count {
            rle.push((i & 1) as u8);
        }

        let len = rle.len();
        let start = len / 4;
        let end = len - start;

        // Count run-iteration steps to avoid timing-based flakiness in CI.
        reset_run_iterations();
        rle.set_range(start, end, 3);
        take_run_iterations()
    }

    let small_iters = measure_once(5_000) as u128;
    let large_iters = measure_once(50_000) as u128;

    // 10x more runs should take roughly 10x more iterations (linear).
    // If O(n^2), it would take ~100x more iterations.
    assert!(
        small_iters > 0,
        "small run-iteration count should be non-zero"
    );
    assert!(
        large_iters > 0,
        "large run-iteration count should be non-zero"
    );
    assert!(
        large_iters >= small_iters,
        "larger run set should not reduce iteration count (small: {small_iters}, large: {large_iters})"
    );
    let small_iters = small_iters.max(1);
    let ratio_x10 = large_iters
        .saturating_mul(10)
        .saturating_add(small_iters / 2)
        / small_iters;
    let max_ratio = 20u128;

    assert!(
        large_iters < small_iters.saturating_mul(max_ratio),
        "rle set_range ratio {}.{}x suggests non-linear behavior (small: {}, large: {})",
        ratio_x10 / 10,
        ratio_x10 % 10,
        small_iters,
        large_iters
    );
}

/// Verify that `Rle::get` walks the runs linearly and lands EXACTLY on the run
/// holding the index.
///
/// The prefix-sum index that made this a binary search was deleted: it cost one
/// allocation on every constructed `Rle` (paid per materialized scrollback
/// line, including the all-default lines whose `Rle` is discarded unread) and no
/// production caller ever reached a reader that consulted it. What has to stay
/// true is the walk's exactness, which is what this pins: reaching the LAST run
/// costs exactly `run_count` steps, and the value is right.
#[test]
fn rle_get_walks_runs_linearly() {
    fn measure_get_last(run_count: u32) -> usize {
        let mut rle: Rle<u8> = Rle::new();
        for i in 0..run_count {
            rle.push((i & 1) as u8);
        }
        assert_eq!(rle.run_count(), run_count as usize);

        reset_run_iterations();
        let val = rle.get(run_count - 1);
        let iters = take_run_iterations();
        assert_eq!(
            val,
            Some(((run_count - 1) & 1) as u8),
            "last element should match inserted parity pattern"
        );
        iters
    }

    let small = measure_get_last(1_000);
    let large = measure_get_last(10_000);

    // The walk visits one run per step and stops on the one that contains the
    // index, so reaching the last of R runs is exactly R steps — and scaling
    // with R is the honest signature of the linear walk.
    assert_eq!(small, 1_000, "linear walk visits every run (small)");
    assert_eq!(large, 10_000, "linear walk visits every run (large)");
}

/// Verify that a FIRST-element lookup stops immediately while a last-element
/// lookup walks to the end — the asymmetry that is inherent to the linear walk,
/// pinned so an accidental off-by-one in the accumulator shows up as a count.
#[test]
fn rle_get_first_stops_at_one_iteration() {
    let mut rle: Rle<u8> = Rle::new();
    let n = 5_000u32;
    for i in 0..n {
        rle.push((i & 1) as u8);
    }

    reset_run_iterations();
    assert_eq!(rle.get(0), Some(0));
    let first_iters = take_run_iterations();

    reset_run_iterations();
    assert_eq!(rle.get(n - 1), Some(((n - 1) & 1) as u8));
    let last_iters = take_run_iterations();

    assert_eq!(
        first_iters, 1,
        "first-element lookup must stop on the first run"
    );
    assert_eq!(
        last_iters, n as usize,
        "last-element lookup walks every run exactly once"
    );
}

/// Verify the run-structure invariants across mixed mutation sequences.
///
/// This is the `find_run` precondition: run lengths are non-zero and accumulate
/// exactly to `total_length` after every mutation, so the linear walk lands on
/// the right run for every in-bounds index.
#[test]
fn rle_run_structure_consistent_across_mutations() {
    let mut rle = Rle::from_iter([0u8, 1, 1, 2, 2, 2, 3, 3]);
    let mut model: Vec<u8> = rle.iter().collect();
    assert_rle_matches_model(&rle, &model, 0);

    let mut seed = 0xA5A5_1D3D_u32;

    for step in 1..=512usize {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let value = (seed & 0b11) as u8;

        match seed % 7 {
            0 => {
                rle.push(value);
                model.push(value);
            }
            1 => {
                let count = (seed >> 8) % 4;
                rle.extend_with(value, count);
                for _ in 0..count {
                    model.push(value);
                }
            }
            2 => {
                if !model.is_empty() {
                    let idx = ((seed >> 8) as usize) % model.len();
                    assert!(rle.set(idx as u32, value));
                    model[idx] = value;
                }
            }
            3 => {
                if !model.is_empty() {
                    let a = ((seed >> 4) as usize) % model.len();
                    let b = ((seed >> 12) as usize) % model.len();
                    let (start, end) = if a <= b { (a, b + 1) } else { (b, a + 1) };
                    rle.set_range(start as u32, end as u32, value);
                    for cell in &mut model[start..end] {
                        *cell = value;
                    }
                }
            }
            4 => {
                let new_len = ((seed >> 16) % 64) as usize;
                rle.resize(new_len as u32);
                model.resize(new_len, 0);
            }
            5 => {
                let new_len = ((seed >> 20) % 64) as usize;
                rle.resize_with(new_len as u32, value);
                model.resize(new_len, value);
            }
            _ => {
                rle.clear();
                model.clear();
            }
        }

        assert_rle_matches_model(&rle, &model, step);
    }
}

#[path = "tests_boundary.rs"]
mod boundary;

#[path = "tests_exhaustive.rs"]
mod exhaustive;

#[path = "tests_performance.rs"]
mod performance;

#[path = "tests_saturation.rs"]
mod saturation;

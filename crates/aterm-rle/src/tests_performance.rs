// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Performance scaling proofs for RLE operations.
//!
//! Uses deterministic iteration counters to verify algorithmic complexity
//! without timing-dependent assertions.

use crate::{Rle, reset_run_iterations, take_run_iterations};

/// Prove that individual `set()` operations are O(R) per call where R is run count.
///
/// When each cell has a unique attribute value (pathological case), R grows to O(N)
/// and N `set()` calls have total cost O(N²). This test documents this known bound
/// and ensures it doesn't exceed the expected quadratic growth rate.
///
/// For typical terminal workloads (few distinct styles per row), R stays small
/// and `set()` is effectively O(1) per call.
#[test]
fn rle_set_individual_cost_scales_with_run_count() {
    fn measure_individual_sets(n: u32) -> usize {
        // Start with a uniform RLE — 1 run.
        let mut rle = Rle::with_value(0u16, n);
        assert_eq!(rle.run_count(), 1);

        reset_run_iterations();
        // Set every other cell to a unique value — forces run fragmentation.
        // After this loop, R ≈ N (alternating values).
        for i in (0..n).step_by(2) {
            // Value i+1 is unique and different from 0 and all other values.
            rle.set(i, (i + 1) as u16);
        }
        take_run_iterations()
    }

    let small_iters = measure_individual_sets(200) as u128;
    let large_iters = measure_individual_sets(400) as u128;

    assert!(small_iters > 0, "should register iterations");
    assert!(large_iters > 0, "should register iterations");

    // With 2x more elements, if each set() is O(R) and R grows linearly,
    // total iterations should grow ~4x (quadratic). We allow up to 6x to
    // accommodate constant factors in compaction.
    // If iterations grow faster than 8x, there's an unexpected superquadratic bug.
    let ratio = large_iters as f64 / small_iters as f64;
    assert!(
        ratio < 8.0,
        "2x input with pathological fragmentation should grow at most ~4x (quadratic), \
         got {ratio:.1}x (small={small_iters}, large={large_iters})"
    );
    // Also verify it's at least superlinear (not accidentally O(N)):
    assert!(
        ratio > 1.5,
        "pathological set() should be superlinear, got {ratio:.1}x"
    );
}

/// Prove that `set()` on a well-compressed RLE (few runs) is O(1) per call.
///
/// When attributes form large runs (typical terminal: bold prompt + normal text),
/// individual `set()` calls are effectively O(1) because R is constant.
#[test]
fn rle_set_well_compressed_is_constant() {
    fn measure_sets_on_uniform(n: u32) -> usize {
        // Uniform RLE — 1 run of value 0.
        let mut rle = Rle::with_value(0u8, n);

        reset_run_iterations();
        // Set a single cell in the middle — splits into 3 runs max.
        rle.set(n / 2, 1);
        take_run_iterations()
    }

    let small = measure_sets_on_uniform(100);
    let large = measure_sets_on_uniform(10_000);

    // Both should use the same number of iterations (O(1) — binary search
    // finds the run, then O(1) split/compact with only 1-3 runs).
    assert_eq!(
        small, large,
        "single set() on uniform RLE should be O(1) regardless of length: \
         small={small}, large={large}"
    );
}

/// Prove that `set_range()` across the full RLE is O(R) (linear in run count).
///
/// This is a more precise test than `rle_set_range_linear_iterations` in tests.rs:
/// it measures the ratio at two specific sizes and verifies linearity.
#[test]
fn rle_set_range_linear_in_runs() {
    fn measure_set_range(run_count: u32) -> usize {
        let mut rle: Rle<u8> = Rle::new();
        for i in 0..run_count {
            rle.push((i & 1) as u8);
        }
        assert_eq!(rle.run_count(), run_count as usize);

        let start = run_count / 4;
        let end = run_count * 3 / 4;

        reset_run_iterations();
        rle.set_range(start, end, 5);
        take_run_iterations()
    }

    let small_iters = measure_set_range(1_000) as f64;
    let large_iters = measure_set_range(4_000) as f64;

    assert!(small_iters > 0.0, "should register iterations");
    let ratio = large_iters / small_iters;
    // 4x more runs should produce ~4x more iterations (linear).
    assert!(
        ratio > 2.0 && ratio < 8.0,
        "4x runs should ~4x set_range iterations (linear): \
         small={small_iters}, large={large_iters}, ratio={ratio:.2}"
    );
}

/// Prove that `find_run()` is an O(R) walk that visits each run at most once.
///
/// The prefix-sum index that made this O(log R) was deleted: it cost one heap
/// allocation on EVERY constructed `Rle` — paid by the scrollback
/// materialization path per line, including the all-default lines whose `Rle`
/// is discarded unread — and its only readers (`get`/`set`/`set_range`/`Index`)
/// have no production caller. Runs per scrollback line are 1-6, so the walk is
/// bounded by construction at the sizes that exist; what must not regress is
/// visiting a run MORE than once, which this pins as an exact count.
#[test]
fn rle_find_run_linear_visits_each_run_once() {
    fn measure_find_last(run_count: u32) -> usize {
        let mut rle: Rle<u8> = Rle::new();
        for i in 0..run_count {
            rle.push((i & 1) as u8);
        }

        reset_run_iterations();
        let result = rle.get(run_count - 1);
        assert!(result.is_some());
        take_run_iterations()
    }

    let small = measure_find_last(100);
    let large = measure_find_last(100_000);

    assert_eq!(small, 100, "one iteration per run, no more (small)");
    assert_eq!(large, 100_000, "one iteration per run, no more (large)");
}

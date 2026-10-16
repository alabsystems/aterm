// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Performance scaling proofs for RLE operations.
//!
//! Uses deterministic iteration counters to verify algorithmic complexity
//! without timing-dependent assertions.

use crate::{Rle, reset_run_iterations, take_run_iterations};

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

    // One run before the set, so the linear walk visits one run at either
    // length; split/compact touch at most 3 runs.
    assert_eq!(
        small, large,
        "single set() on uniform RLE should be O(1) regardless of length: \
         small={small}, large={large}"
    );
}

/// Prove that `set_range()` across the full RLE is O(R) (linear in run count).
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

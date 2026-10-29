// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Deterministic iteration-counter complexity tests for RLE operations.

use crate::{Rle, reset_run_iterations, take_run_iterations};

#[test]
fn rle_set_well_compressed_is_constant() {
    fn measure_sets_on_uniform(n: u32) -> usize {
        let mut rle = Rle::with_value(0u8, n);

        reset_run_iterations();
        rle.set(n / 2, 1);
        take_run_iterations()
    }

    let small = measure_sets_on_uniform(100);
    let large = measure_sets_on_uniform(10_000);

    assert_eq!(small, 1);
    assert_eq!(large, 1);
}

#[test]
fn rle_set_range_linear_in_runs() {
    fn measure_set_range(run_count: u32) -> (usize, usize) {
        let mut rle: Rle<u8> = Rle::new();
        for i in 0..run_count {
            rle.push((i & 1) as u8);
        }
        assert_eq!(rle.run_count(), run_count as usize);

        let start = run_count / 4;
        let end = run_count * 3 / 4;

        reset_run_iterations();
        rle.set_range(start, end, 5);
        let actual = take_run_iterations();
        // `find_run(start)` visits `start + 1` runs, `find_run(end - 1)` visits `end` runs,
        // and `split_range_multi_run` visits `end - start` runs (`compact_around` has no loop).
        let expected = (start + 1 + end + (end - start)) as usize;
        (actual, expected)
    }

    let (small_iters, small_expected) = measure_set_range(1_000);
    let (large_iters, large_expected) = measure_set_range(4_000);

    assert_eq!(small_iters, small_expected);
    assert_eq!(large_iters, large_expected);
}

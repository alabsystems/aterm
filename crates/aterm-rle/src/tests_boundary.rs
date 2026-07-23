// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// set_range with a single-element range at a run boundary.
#[test]
fn rle_set_range_single_element_at_run_boundary() {
    // Sequence: [1, 1, 2, 2, 3, 3]
    //           indices: 0 1 2 3 4 5
    // Set range [2, 3) to value 9 — exactly the first element of the second run
    let mut rle = Rle::from_iter([1u8, 1, 2, 2, 3, 3]);
    let mut model = vec![1u8, 1, 2, 2, 3, 3];

    rle.set_range(2, 3, 9);
    model[2] = 9;

    assert_rle_matches_model(&rle, &model, 0);
    assert_eq!(rle.get(2), Some(9));
    assert_eq!(rle.get(3), Some(2), "element after range unchanged");
}

/// set_range with a single-element range at the last element.
#[test]
fn rle_set_range_single_element_last() {
    let mut rle = Rle::from_iter([1u8, 2, 3]);
    rle.set_range(2, 3, 9);
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(1), Some(2));
    assert_eq!(rle.get(2), Some(9));
    assert_eq!(rle.get(3), None);
}

/// set_range that starts at 0 and ends before the sequence end.
#[test]
fn rle_set_range_from_start() {
    let mut rle = Rle::from_iter([1u8, 1, 1, 2, 2]);
    let mut model = vec![1u8, 1, 1, 2, 2];

    rle.set_range(0, 3, 9);
    for m in model.iter_mut().take(3) {
        *m = 9;
    }

    assert_rle_matches_model(&rle, &model, 0);
}

/// set_range where the new value matches adjacent runs, causing merge.
#[test]
fn rle_set_range_merges_with_adjacent() {
    // [1, 1, 2, 2, 1, 1] — set middle to 1 should merge into single run
    let mut rle = Rle::from_iter([1u8, 1, 2, 2, 1, 1]);
    rle.set_range(2, 4, 1);
    assert_eq!(rle.run_count(), 1, "all same value should compact to 1 run");
    assert_eq!(rle.len(), 6);
    for i in 0..6 {
        assert_eq!(rle.get(i), Some(1));
    }
}

/// set on a length-1 RLE.
#[test]
fn rle_set_on_single_element() {
    let mut rle = Rle::with_value(5u8, 1);
    assert!(rle.set(0, 9));
    assert_eq!(rle.get(0), Some(9));
    assert_eq!(rle.len(), 1);
    assert_eq!(rle.run_count(), 1);
}

/// truncate to length 1.
#[test]
fn rle_truncate_to_one() {
    let mut rle = Rle::from_iter([1u8, 2, 3, 4, 5]);
    rle.resize(1);
    assert_eq!(rle.len(), 1);
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(1), None);
    assert_eq!(rle.run_count(), 1);
}

/// truncate exactly at a run boundary.
#[test]
fn rle_truncate_at_run_boundary() {
    // [1, 1, 2, 2, 3, 3] — truncate to 4 (end of second run)
    let mut rle = Rle::from_iter([1u8, 1, 2, 2, 3, 3]);
    rle.resize(4);
    assert_eq!(rle.len(), 4);
    assert_eq!(rle.run_count(), 2);
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(3), Some(2));
    assert_eq!(rle.get(4), None);
}

/// truncate inside a run (not at boundary).
#[test]
fn rle_truncate_inside_run() {
    // [1, 1, 1, 1, 1] — truncate to 3
    let mut rle = Rle::with_value(1u8, 5);
    rle.resize(3);
    assert_eq!(rle.len(), 3);
    assert_eq!(rle.run_count(), 1);
    assert_eq!(rle.runs()[0].length, 3);
}

/// set_range with start == end (empty range) is a no-op.
#[test]
fn rle_set_range_empty_range() {
    let mut rle = Rle::from_iter([1u8, 2, 3]);
    rle.set_range(1, 1, 9); // empty range
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(1), Some(2));
    assert_eq!(rle.get(2), Some(3));
}

/// set_range with end > total_length clips to total_length.
#[test]
fn rle_set_range_clips_end() {
    let mut rle = Rle::from_iter([1u8, 2, 3]);
    rle.set_range(1, 100, 9);
    assert_eq!(rle.get(0), Some(1));
    assert_eq!(rle.get(1), Some(9));
    assert_eq!(rle.get(2), Some(9));
    assert_eq!(rle.get(3), None);
}

/// Verify iterator size_hint is exact for a multi-run RLE.
#[test]
fn rle_iter_size_hint_exact() {
    let rle = Rle::from_iter([1u8, 1, 2, 3, 3, 3]);
    let iter = rle.iter();
    let (lower, upper) = iter.size_hint();
    assert_eq!(lower, 6);
    assert_eq!(upper, Some(6));
    assert_eq!(iter.len(), 6);
}

/// find_run_binary returns None for index == total_length.
#[test]
fn rle_find_run_out_of_bounds() {
    let rle = Rle::from_iter([1u8, 2, 3]);
    assert_eq!(rle.get(3), None);
    assert_eq!(rle.get(u32::MAX), None);
}

/// Extend then access: prefix sums stay consistent after extend_with.
#[test]
fn rle_extend_with_prefix_sums_consistent() {
    let mut rle = Rle::from_iter([1u8, 2, 3]);
    rle.extend_with(4, 5);

    let mut model = vec![1u8, 2, 3, 4, 4, 4, 4, 4];
    assert_rle_matches_model(&rle, &model, 0);

    // Extend with same value as last run
    rle.extend_with(4, 3);
    model.extend_from_slice(&[4, 4, 4]);
    assert_rle_matches_model(&rle, &model, 1);
}

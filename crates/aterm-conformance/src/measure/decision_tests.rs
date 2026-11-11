// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_completion_is_inside_and_both_extremes_are_attainable() {
    for cap in 1..=8 {
        for n in 0..=cap {
            let prefix: Vec<_> = (0..n).map(|i| (i * 2 + 1) as u64).rev().collect();
            for rank in 1..=cap {
                let (lo, hi) = completion(&prefix, cap, rank).unwrap();
                for mask in 0..1_usize << (cap - n) {
                    let mut full = prefix.clone();
                    full.extend(
                        (0..cap - n).map(|i| if mask & (1 << i) == 0 { 0 } else { u64::MAX }),
                    );
                    full.sort_unstable();
                    assert!((lo..=hi).contains(&full[rank - 1]));
                    if mask == 0 {
                        assert_eq!(full[rank - 1], lo);
                    }
                    if mask == (1 << (cap - n)) - 1 {
                        assert_eq!(full[rank - 1], hi);
                    }
                }
            }
        }
    }
    // A prefix p99 pass can still be overturned by an unobserved tail.
    assert_eq!(completion(&[1; 40], 100, 99), Some((1, u64::MAX)));
    assert_eq!(completion(&[1; 98], 100, 99), Some((1, u64::MAX)));
    assert_eq!(completion(&[9; 2], 100, 99), Some((9, u64::MAX)));
    assert_eq!(completion(&[], 0, 1), None);
    assert_eq!(completion(&[1; 3], 2, 1), None);
    assert_eq!(completion(&[1], 2, 0), None);
    assert_eq!(completion(&[1], 2, 3), None);
    assert_eq!(completion(&[], usize::MAX, 1), Some((0, u64::MAX)));
}

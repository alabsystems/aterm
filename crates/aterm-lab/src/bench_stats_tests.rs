// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn percentiles_are_nearest_rank() {
    assert_eq!(percentiles(&[]), None);
    let ms: Vec<u64> = (1..=100).map(|i| i * 1_000_000).collect();
    assert_eq!(percentiles(&ms), Some((50.0, 95.0, 99.0)));
    assert_eq!(percentiles(&[7_000_000]), Some((7.0, 7.0, 7.0)));
}

#[test]
fn the_williams_square_puts_each_arm_once_in_every_position() {
    let square = williams(6).expect("six is even");
    assert_eq!(square.len(), 6);
    for row in &square {
        let mut sorted = row.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            (0..6).collect::<Vec<_>>(),
            "each order is a permutation"
        );
    }
    for pos in 0..6 {
        let mut column: Vec<usize> = square.iter().map(|row| row[pos]).collect();
        column.sort_unstable();
        assert_eq!(column, (0..6).collect::<Vec<_>>(), "position {pos}");
    }
}

#[test]
fn the_williams_square_puts_each_arm_after_every_other_exactly_once() {
    for n in [2, 4, 6, 8] {
        let square = williams(n).expect("even");
        let mut after = vec![vec![0u32; n]; n];
        for row in &square {
            for pair in row.windows(2) {
                after[pair[0]][pair[1]] += 1;
            }
        }
        for (a, followers) in after.iter().enumerate() {
            for (b, count) in followers.iter().enumerate() {
                let want = u32::from(a != b);
                assert_eq!(*count, want, "n={n}: {b} after {a}");
            }
        }
    }
    assert_eq!(williams(5), None, "an odd n has no n-row square");
    assert_eq!(williams(0), None);
}

#[test]
fn the_load_average_is_the_first_of_sysctls_three() {
    assert_eq!(parse_loadavg("{ 2.85 3.10 2.97 }\n"), Some(2.85));
    assert_eq!(parse_loadavg(""), None);
}

#[test]
fn the_median_the_span_and_the_valid_rounds() {
    assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
    assert_eq!(median(&[]), None);
    assert_eq!(span_ratio(&[]), None);
    assert_eq!(span_ratio(&[2.0, 3.0, 1.0]), Some(3.0));
    assert_eq!(span_ratio(&[0.0, 1.0]), None, "no ratio over a zero wall");
    assert_eq!(required_valid(12), 10);
    assert_eq!(required_valid(1), 1);
    assert_eq!(required_valid(6), 5);
}

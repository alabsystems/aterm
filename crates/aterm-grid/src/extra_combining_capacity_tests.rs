// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::CellExtra;

#[test]
fn batching_preserves_scalar_growth_capacity_for_following_text() {
    for capacity in [0, 1, 2, 3, 4, 5, 7, 8, 15, 17, 31, 33, 63] {
        for prefix in ["", "abc", "prefix  "] {
            for mark in ['\u{301}', '\u{20dd}', '\u{1d185}'] {
                for count in 0..=16 {
                    let marks = vec![mark; count];
                    let mut extra = CellExtra::default();
                    for &ch in &marks {
                        extra.add_combining(ch);
                    }
                    let mut actual = String::with_capacity(capacity);
                    actual.push_str(prefix);
                    actual.push('x');
                    let mut scalar = String::with_capacity(capacity);
                    scalar.push_str(prefix);
                    scalar.push('x');
                    extra.push_combining_text(&mut actual);
                    for &ch in &marks {
                        scalar.push(ch);
                    }
                    assert_eq!(actual, scalar);
                    assert_eq!(actual.capacity(), scalar.capacity());
                    actual.push('y');
                    scalar.push('y');
                    assert_eq!(actual.capacity(), scalar.capacity());
                }
            }
        }
    }
}

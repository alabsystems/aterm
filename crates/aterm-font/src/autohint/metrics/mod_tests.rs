// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/metrics/mod.rs and modified by the
// aterm project in 2026: metrics computed lazily only, the advance-only digit width dropped.
// See ../../../LICENSE-FONTATIONS for the upstream license.

use super::*;

#[test]
fn sort_widths() {
    assert_eq!(sort_widths_helper(&[1], 10), &[1]);
    assert_eq!(sort_widths_helper(&[1], 20), &[1]);
    assert_eq!(sort_widths_helper(&[60, 20, 40, 35], 10), &[20, 35, 13, 60]);
    assert_eq!(sort_widths_helper(&[60, 20, 40, 35], 20), &[31, 60]);
}

fn sort_widths_helper(widths: &[i32], threshold: i32) -> Vec<i32> {
    let mut widths2 = UnscaledWidths::new();
    for width in widths {
        widths2.push(*width);
    }
    sort_and_quantize_widths(&mut widths2, threshold);
    widths2
}

#[test]
fn pix_round_extremes_do_not_panic() {
    let _ = pix_round(i32::MIN);
    let _ = pix_round(i32::MAX);
}

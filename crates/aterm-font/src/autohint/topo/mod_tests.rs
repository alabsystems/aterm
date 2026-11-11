// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/topo/mod.rs and modified by the
// aterm project in 2026: the hint-plan accessors dropped.
// See ../../../LICENSE-FONTATIONS for the upstream license.

use super::*;

#[test]
fn segment_adjust_height_extreme_values_do_not_panic() {
    let mut segment = Segment {
        height: i16::MAX,
        ..Default::default()
    };
    segment.adjust_height(i32::MIN, i32::MAX);
    segment.adjust_height(i32::MAX, i32::MIN);
}

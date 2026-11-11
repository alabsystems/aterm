// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::visible_cell_range;

#[test]
fn a_footprint_walks_only_its_on_screen_cells() {
    assert_eq!(visible_cell_range(0, 10, 80), 0..10, "wholly on screen");
    assert_eq!(visible_cell_range(-5, 10, 80), 5..10, "off the left edge");
    assert_eq!(visible_cell_range(75, 10, 80), 0..5, "off the right edge");
    assert_eq!(visible_cell_range(-3, 10, 4), 3..7, "past both edges");
    for (origin, span, limit) in [
        (100, 10, 80),
        (80, 10, 80),
        (-10, 10, 80),
        (i32::MIN, u16::MAX, 80),
        (i32::MAX, u16::MAX, 80),
        (70_000, u16::MAX, 80),
        (0, 0, 80),
        (0, 10, 0),
    ] {
        assert!(
            visible_cell_range(origin, span, limit).is_empty(),
            "({origin}, {span}, {limit})"
        );
    }
    assert_eq!(
        visible_cell_range(-65_534, u16::MAX, 24),
        65_534..u16::MAX,
        "the last cell of the widest footprint"
    );
}

// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Boundary condition tests: a 1x1 write, history line API ordering and
//! `row_u16` saturation.

use super::super::*;
use crate::grid::test_support::write_marker_line;

// Minimum dimension / clamping

#[test]
fn grid_write_char_1x1_stays_at_origin() {
    let mut grid = Grid::new(1, 1);

    // Write to the only cell — cursor should stay at (0, 0)
    grid.write_char('X');
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(
        grid.cursor_col(),
        0,
        "1x1: cursor stays at col 0 after write"
    );
    grid.assert_invariants();
}

// History line API ordering

#[test]
fn grid_history_line_api_covers_ring_buffer_ordering() {
    let mut grid = Grid::with_scrollback(3, 4, 8);

    for marker in ['A', 'B', 'C', 'D', 'E'] {
        write_marker_line(&mut grid, marker);
    }

    assert_eq!(grid.history_line_count(), 3);

    let oldest = grid
        .get_history_line(0)
        .expect("history idx 0 should exist");
    let middle = grid
        .get_history_line(1)
        .expect("history idx 1 should exist");
    let newest = grid
        .get_history_line(2)
        .expect("history idx 2 should exist");
    let newest_rev = grid
        .history_line_rev(0)
        .expect("reverse history idx 0 should exist");
    let oldest_rev = grid
        .history_line_rev(2)
        .expect("reverse history idx 2 should exist");

    assert_eq!(oldest.to_string().chars().next(), Some('A'));
    assert_eq!(middle.to_string().chars().next(), Some('B'));
    assert_eq!(newest.to_string().chars().next(), Some('C'));
    assert!(grid.get_history_line(3).is_none());
    assert_eq!(newest_rev.to_string().chars().next(), Some('C'));
    assert_eq!(oldest_rev.to_string().chars().next(), Some('A'));
    assert!(grid.history_line_rev(3).is_none());
}

// row_u16 / clamp_u16 saturation

/// `row_u16` saturates to `u16::MAX` for values above the u16 range.
#[test]
fn row_u16_saturates_on_overflow() {
    use super::super::row_u16;

    // In-range values convert exactly.
    for val in [0_usize, 1, 24, 500, 65_535] {
        assert_eq!(usize::from(row_u16(val)), val);
    }

    // Values above u16::MAX saturate.
    assert_eq!(row_u16(u16::MAX as usize + 1), u16::MAX);
    assert_eq!(row_u16(usize::MAX), u16::MAX);
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the grid's absolute row numbering.

use crate::Grid;

#[test]
fn continue_absolute_numbering_is_bounded_both_ways() {
    let mut grid = Grid::new(4, 10);
    assert_eq!(grid.absolute_row_counter(), 4);
    grid.continue_absolute_numbering(1_000);
    assert_eq!(grid.absolute_row_counter(), 1_000);
    assert_eq!(
        grid.visible_to_absolute(0),
        996,
        "the top row is renumbered"
    );
    grid.continue_absolute_numbering(0);
    assert_eq!(
        grid.absolute_row_counter(),
        4,
        "never below the rows it holds"
    );
    grid.continue_absolute_numbering(u64::MAX);
    assert_eq!(
        grid.absolute_row_counter(),
        u64::MAX >> 16,
        "the forged value is clamped"
    );
}

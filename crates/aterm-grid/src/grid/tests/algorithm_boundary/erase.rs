// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
// Erase display modes (ED): erase_to_end_of_screen (ED 0),
// erase_from_start_of_screen (ED 1), erase_screen (ED 2)
// ========================================================================

#[test]
fn erase_to_end_of_screen_cursor_at_last_row() {
    let mut grid = Grid::new(5, 10);
    fill_grid_rows(&mut grid, 5);

    grid.set_cursor(4, 0);
    grid.erase_to_end_of_screen();

    for row in 0..4 {
        assert_eq!(
            grid.cell(row, 0).unwrap().char(),
            (b'A' + row as u8) as char,
            "row {row} should be preserved",
        );
    }
    assert!(
        grid.row(4).unwrap().is_empty(),
        "last row should be cleared"
    );
}

#[test]
fn erase_from_start_of_screen_cursor_at_origin() {
    let mut grid = Grid::new(5, 10);
    fill_grid_rows(&mut grid, 5);

    grid.set_cursor(0, 0);
    grid.erase_from_start_of_screen();

    assert_eq!(grid.cell(0, 0).unwrap().char(), ' ');
    for row in 1..5 {
        assert_eq!(
            grid.cell(row, 0).unwrap().char(),
            (b'A' + row as u8) as char,
            "row {row} should be preserved",
        );
    }
}

#[test]
fn erase_from_start_of_screen_cursor_at_last_row() {
    let mut grid = Grid::new(5, 10);
    fill_grid_rows(&mut grid, 5);

    grid.set_cursor(4, 0);
    grid.erase_from_start_of_screen();

    for row in 0..4 {
        assert!(
            grid.row(row).unwrap().is_empty(),
            "row {row} should be empty after ED 1 from last row",
        );
    }
    assert_eq!(grid.cell(4, 0).unwrap().char(), ' ');
}

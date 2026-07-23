// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
// Erase display modes (ED): erase_to_end_of_screen (ED 0),
// erase_from_start_of_screen (ED 1), erase_screen (ED 2)
// ========================================================================

#[test]
fn erase_to_end_of_screen_cursor_at_origin() {
    let mut grid = Grid::new(5, 10);
    fill_grid_rows(&mut grid, 5);

    grid.set_cursor(0, 0);
    grid.erase_to_end_of_screen();

    for row in 0..5 {
        assert!(
            grid.row(row).unwrap().is_empty(),
            "row {row} should be empty after ED 0 from origin",
        );
    }
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 0);
}

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
fn erase_to_end_of_screen_cursor_mid_line() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(1, 0);
    for c in "0123456789".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(2, 0);
    grid.write_char('Z');

    grid.set_cursor(0, 5);
    grid.erase_to_end_of_screen();

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 4).unwrap().char(), 'E');
    // Cols 5-9 on cursor row should all be cleared
    for col in 5..10 {
        assert_eq!(
            grid.cell(0, col).unwrap().char(),
            ' ',
            "col {col} should be cleared on cursor row",
        );
    }
    assert!(grid.row(1).unwrap().is_empty());
    assert!(grid.row(2).unwrap().is_empty());
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

#[test]
fn erase_from_start_of_screen_cursor_mid_line() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    grid.write_char('X');
    grid.set_cursor(1, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(2, 0);
    grid.write_char('Z');

    grid.set_cursor(1, 5);
    grid.erase_from_start_of_screen();

    assert!(grid.row(0).unwrap().is_empty());
    for col in 0..=5 {
        assert_eq!(
            grid.cell(1, col).unwrap().char(),
            ' ',
            "row 1 col {col} should be cleared",
        );
    }
    assert_eq!(grid.cell(1, 6).unwrap().char(), 'G');
    assert_eq!(grid.cell(1, 9).unwrap().char(), 'J');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'Z');
}

#[test]
fn erase_screen_clears_all() {
    let mut grid = Grid::new(5, 10);
    fill_grid_rows(&mut grid, 5);

    grid.set_cursor(2, 3);
    grid.erase_screen();

    for row in 0..5 {
        assert!(
            grid.row(row).unwrap().is_empty(),
            "row {row} should be empty after ED 2",
        );
    }
    assert_eq!(grid.cursor_row(), 2);
    assert_eq!(grid.cursor_col(), 3);
}

// ========================================================================
// Erase line modes (EL)
// ========================================================================

#[test]
fn erase_to_end_of_line_cursor_at_col_zero() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.set_cursor(0, 0);
    grid.erase_to_end_of_line();

    assert!(
        grid.row(0).unwrap().is_empty(),
        "entire line should be cleared"
    );
}

#[test]
fn erase_to_end_of_line_cursor_at_last_col() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.set_cursor(0, 9);
    grid.erase_to_end_of_line();

    assert_eq!(grid.cell(0, 8).unwrap().char(), 'I');
    assert_eq!(grid.cell(0, 9).unwrap().char(), ' ');
}

#[test]
fn erase_from_start_of_line_cursor_at_col_zero() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.set_cursor(0, 0);
    grid.erase_from_start_of_line();

    assert_eq!(grid.cell(0, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'B');
}

#[test]
fn erase_from_start_of_line_cursor_at_last_col() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.set_cursor(0, 9);
    grid.erase_from_start_of_line();

    assert!(
        grid.row(0).unwrap().is_empty(),
        "entire line should be cleared when cursor at last col"
    );
}

#[test]
fn erase_line_preserves_other_rows() {
    let mut grid = Grid::new(3, 10);
    fill_grid_rows(&mut grid, 3);

    grid.set_cursor(1, 5);
    grid.erase_line();

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert!(grid.row(1).unwrap().is_empty());
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'C');
}

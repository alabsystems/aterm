// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! DECSLRM horizontal margin scope: in-place erases (EL, DECSEL, ECH, ED) ignore horizontal
//! margins, while sideways shifts (ICH, DCH, DECIC, DECDC) stay bounded by them.

use super::super::*;

fn filled_row_with_margins() -> Grid {
    let mut grid = Grid::new(3, 20);
    for _ in 0..20 {
        grid.write_char('X');
    }
    grid.set_horizontal_margins(4, 10);
    assert!(grid.has_horizontal_margins(), "DECSLRM is armed");
    assert_eq!(grid.cell(0, 19).unwrap().char(), 'X', "row 0 is full");
    grid
}

#[track_caller]
fn assert_blank(grid: &Grid, col: u16, why: &str) {
    assert_eq!(grid.cell(0, col).unwrap().char(), ' ', "col {col}: {why}");
}

#[track_caller]
fn assert_kept(grid: &Grid, col: u16, why: &str) {
    assert_eq!(grid.cell(0, col).unwrap().char(), 'X', "col {col}: {why}");
}

#[test]
fn el0_erases_past_the_right_margin() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_to_end_of_line();
    assert_kept(&grid, 5, "EL 0 never touches the left of the cursor");
    assert_blank(&grid, 6, "EL 0 erases from the cursor, inclusive");
    assert_blank(&grid, 10, "the right margin itself is erased");
    assert_blank(&grid, 11, "EL 0 does not stop at the right margin");
    assert_blank(&grid, 19, "EL 0 runs to the end of the row");
}

#[test]
fn el1_erases_left_of_the_left_margin() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_from_start_of_line();
    assert_blank(&grid, 0, "EL 1 opens at column 0, not at the left margin");
    assert_blank(&grid, 3, "the columns left of the margin are erased");
    assert_blank(&grid, 6, "EL 1 erases through the cursor, inclusive");
    assert_kept(&grid, 7, "EL 1 never touches the right of the cursor");
}

#[test]
fn el2_erases_the_whole_row_not_the_margin_span() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_line();
    assert_blank(&grid, 0, "EL 2 reaches left of the left margin");
    assert_blank(&grid, 19, "EL 2 reaches right of the right margin");
    assert!(
        grid.row(0).unwrap().is_empty(),
        "EL 2 leaves the row completely empty"
    );
}

#[test]
fn decsel0_erases_past_the_right_margin() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.selective_erase_to_end_of_line();
    assert_kept(&grid, 5, "DECSEL 0 never touches the left of the cursor");
    assert_blank(&grid, 11, "DECSEL 0 does not stop at the right margin");
    assert_blank(&grid, 19, "DECSEL 0 runs to the end of the row");
}

#[test]
fn decsel1_erases_left_of_the_left_margin() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.selective_erase_from_start_of_line();
    assert_blank(&grid, 0, "DECSEL 1 opens at column 0");
    assert_blank(&grid, 3, "the columns left of the margin are erased");
    assert_kept(&grid, 7, "DECSEL 1 never touches the right of the cursor");
}

#[test]
fn decsel2_erases_the_whole_row_not_the_margin_span() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.selective_erase_line();
    assert_blank(&grid, 0, "DECSEL 2 reaches left of the left margin");
    assert_blank(&grid, 19, "DECSEL 2 reaches right of the right margin");
}

#[test]
fn ech_erases_past_the_right_margin() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_chars(10);
    assert_kept(&grid, 5, "ECH starts at the cursor");
    assert_blank(&grid, 6, "ECH erases from the cursor, inclusive");
    assert_blank(&grid, 10, "the right margin itself is erased");
    assert_blank(&grid, 11, "ECH does not stop at the right margin");
    assert_blank(&grid, 15, "all ten requested cells are erased");
    assert_kept(&grid, 16, "and exactly ten: the eleventh cell survives");
}

#[test]
fn ech_count_is_clipped_by_the_row_edge() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_chars(999);
    assert_blank(&grid, 19, "the row edge is the only bound");
    assert_kept(&grid, 5, "and nothing left of the cursor moves");
}

#[test]
fn ed0_erases_the_cursor_row_past_the_right_margin() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_to_end_of_screen();
    assert_kept(&grid, 5, "ED 0 starts at the cursor");
    assert_blank(&grid, 19, "ED 0's cursor row runs to the row edge");
}

#[test]
fn ed1_erases_the_cursor_row_from_column_zero() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_from_start_of_screen();
    assert_blank(&grid, 0, "ED 1's cursor row opens at column 0");
    assert_kept(&grid, 7, "and stops after the cursor");
}

#[test]
fn ich_still_shifts_only_within_the_margins() {
    let mut grid = Grid::new(3, 20);
    for c in "ABCDEFGHIJKLMNOPQRST".chars() {
        grid.write_char(c);
    }
    grid.set_horizontal_margins(4, 10);
    grid.set_cursor(0, 6);
    grid.insert_chars(2);
    assert_eq!(grid.cell(0, 6).unwrap().char(), ' ', "two blanks inserted");
    assert_eq!(grid.cell(0, 7).unwrap().char(), ' ');
    assert_eq!(grid.cell(0, 8).unwrap().char(), 'G', "'G' shifted right");
    assert_eq!(
        grid.cell(0, 11).unwrap().char(),
        'L',
        "the cell right of the right margin did not move — ICH is margin-bound"
    );
}

#[test]
fn dch_still_shifts_only_within_the_margins() {
    let mut grid = Grid::new(3, 20);
    for c in "ABCDEFGHIJKLMNOPQRST".chars() {
        grid.write_char(c);
    }
    grid.set_horizontal_margins(4, 10);
    grid.set_cursor(0, 6);
    grid.delete_chars(2);
    assert_eq!(grid.cell(0, 6).unwrap().char(), 'I', "'I' shifted left");
    assert_eq!(
        grid.cell(0, 10).unwrap().char(),
        ' ',
        "the blank fill lands at the right margin"
    );
    assert_eq!(
        grid.cell(0, 11).unwrap().char(),
        'L',
        "the cell right of the right margin did not move — DCH is margin-bound"
    );
}

#[test]
fn decic_still_shifts_only_within_the_margins() {
    let mut grid = Grid::new(3, 20);
    for c in "ABCDEFGHIJKLMNOPQRST".chars() {
        grid.write_char(c);
    }
    grid.set_horizontal_margins(4, 10);
    grid.set_cursor(0, 6);
    grid.insert_columns(2);
    assert_eq!(
        grid.cell(0, 5).unwrap().char(),
        'F',
        "left of the cursor stays"
    );
    assert_eq!(
        grid.cell(0, 6).unwrap().char(),
        ' ',
        "two blank columns inserted"
    );
    assert_eq!(grid.cell(0, 7).unwrap().char(), ' ');
    assert_eq!(grid.cell(0, 8).unwrap().char(), 'G', "'G' shifted right");
    assert_eq!(
        grid.cell(0, 10).unwrap().char(),
        'I',
        "'I' lands ON the right margin; 'J' and 'K' fell off it"
    );
    assert_eq!(
        grid.cell(0, 11).unwrap().char(),
        'L',
        "the cell right of the right margin did not move — DECIC is margin-bound"
    );
}

#[test]
fn decdc_still_shifts_only_within_the_margins() {
    let mut grid = Grid::new(3, 20);
    for c in "ABCDEFGHIJKLMNOPQRST".chars() {
        grid.write_char(c);
    }
    grid.set_horizontal_margins(4, 10);
    grid.set_cursor(0, 6);
    grid.delete_columns(2);
    assert_eq!(grid.cell(0, 6).unwrap().char(), 'I', "'I' shifted left");
    assert_eq!(grid.cell(0, 8).unwrap().char(), 'K', "'K' shifted left");
    assert_eq!(
        grid.cell(0, 9).unwrap().char(),
        ' ',
        "the blank fill opens inside the right margin"
    );
    assert_eq!(grid.cell(0, 10).unwrap().char(), ' ');
    assert_eq!(
        grid.cell(0, 11).unwrap().char(),
        'L',
        "the cell right of the right margin did not move — DECDC is margin-bound"
    );
}

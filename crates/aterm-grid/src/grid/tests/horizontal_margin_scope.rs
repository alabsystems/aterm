// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Which operations DECSLRM's horizontal margins bound, and which they do not.
//!
//! VT510 splits them: in-place erases (EL, ECH, ED) "work inside or outside the
//! scrolling margins", while sideways shifts (ICH, DCH) have "no effect outside
//! the scrolling margins". xterm, Ghostty, iTerm2 and WezTerm implement exactly
//! that split (xterm's `ClearRight`/`ClearLeft`/`ClearLine` never consult
//! `ScrnLeftMargin`; `ScrnInsertChar`/`ScrnDeleteChar` open with both margins).
//!
//! These tests pin both halves: EL, DECSEL, ECH and ED are unbounded; ICH/DCH
//! and DECIC/DECDC keep their margins. IL/DL's rectangular shift is pinned in
//! `scroll_region::region_ops`, and autowrap to the left margin in
//! aterm-conformance `vt_categories3::declrmm_autowrap_wraps_to_left_margin`.

use super::super::*;

/// Row 0 of a 3x20 grid filled with 'X' (before margins, so every column holds
/// one), then DECSLRM margins at columns 4..=10.
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

// EL: the in-place erase class, not bounded by DECSLRM.

/// EL 0 runs to the row's edge, past the right margin (xterm `ClearRight`
/// bounded by `MaxCols`; Ghostty and iTerm2 likewise).
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

/// EL 1 starts at column 0, not at the left margin (xterm `ClearLeft`).
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

/// EL 2 erases the whole row, not the margin span.
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

// DECSEL: the same spans, with protection honoured inside them.

/// DECSEL 0 shares EL 0's span: xterm reaches `ClearRight` for both, the
/// protection split happening inside `ClearInLine2`, not in the bounds.
#[test]
fn decsel0_erases_past_the_right_margin() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.selective_erase_to_end_of_line();
    assert_kept(&grid, 5, "DECSEL 0 never touches the left of the cursor");
    assert_blank(&grid, 11, "DECSEL 0 does not stop at the right margin");
    assert_blank(&grid, 19, "DECSEL 0 runs to the end of the row");
}

/// DECSEL 1 shares EL 1's span, which opens at column 0.
#[test]
fn decsel1_erases_left_of_the_left_margin() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.selective_erase_from_start_of_line();
    assert_blank(&grid, 0, "DECSEL 1 opens at column 0");
    assert_blank(&grid, 3, "the columns left of the margin are erased");
    assert_kept(&grid, 7, "DECSEL 1 never touches the right of the cursor");
}

/// DECSEL 2 shares EL 2's span: the whole row.
#[test]
fn decsel2_erases_the_whole_row_not_the_margin_span() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.selective_erase_line();
    assert_blank(&grid, 0, "DECSEL 2 reaches left of the left margin");
    assert_blank(&grid, 19, "DECSEL 2 reaches right of the right margin");
}

// ECH: the same class (xterm's `do_erase_char` calls `ClearRight`).

/// ECH erases `count` cells from the cursor regardless of the right margin
/// (VT510: "ECH works inside or outside the scrolling margins").
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

/// A count past the row's edge is clipped by the ROW, not by the margin.
#[test]
fn ech_count_is_clipped_by_the_row_edge() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_chars(999);
    assert_blank(&grid, 19, "the row edge is the only bound");
    assert_kept(&grid, 5, "and nothing left of the cursor moves");
}

// ED

/// ED 0's cursor row is erased to the row edge, margins or not.
#[test]
fn ed0_erases_the_cursor_row_past_the_right_margin() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_to_end_of_screen();
    assert_kept(&grid, 5, "ED 0 starts at the cursor");
    assert_blank(&grid, 19, "ED 0's cursor row runs to the row edge");
}

/// ED 1's cursor row is erased from column 0, margins or not.
#[test]
fn ed1_erases_the_cursor_row_from_column_zero() {
    let mut grid = filled_row_with_margins();
    grid.set_cursor(0, 6);
    grid.erase_from_start_of_screen();
    assert_blank(&grid, 0, "ED 1's cursor row opens at column 0");
    assert_kept(&grid, 7, "and stops after the cursor");
}

// ICH / DCH keep their margins.

/// ICH shifts within the margins: cells right of the right margin do not move
/// and nothing falls off the row's end.
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

/// DCH likewise: the fill lands at the right margin, not at the row edge.
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

// DECIC / DECDC keep their margins too.

/// DECIC shifts only the margin span: columns pushed past the right margin are
/// lost and the cell right of the margin does not move (DEC STD 070).
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

/// DECDC likewise: blank columns open at the right margin, and nothing right of
/// the margin is pulled in.
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

// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Which operations DECSLRM's horizontal margins bound, and which they do not.
//!
//! VT510 draws the line with one sentence per page, and the two sentences are
//! opposites:
//! - erase IN PLACE — EL, ECH and ED each carry "works inside or outside the
//!   scrolling margins";
//! - SHIFT data sideways — ICH and DCH each carry "has no effect outside the
//!   scrolling margins".
//!
//! DECSLRM's page is what makes those "scrolling margins" these margins ("sets
//! the left and right margins to define the scrolling region"). xterm, Ghostty,
//! iTerm2 and WezTerm all implement exactly that split, each showing both halves
//! in one file: xterm's `ClearRight`/`ClearLeft`/`ClearLine` (util.c) bound by
//! `MaxCols`/column 0 and never call `ScrnLeftMargin`, while `ScrnInsertChar` /
//! `ScrnDeleteChar` (screen.c) open with both margins.
//!
//! aterm used to clamp EL 0/1/2, DECSEL 0/1/2 and ECH to the margins, citing the
//! VT510 spec for a claim the VT510 ECH page contradicts word for word. Nothing
//! pinned the clamp, so these tests pin its ABSENCE — and the ICH/DCH and
//! DECIC/DECDC cases pin the other half, so removing a clamp here can never be
//! mistaken for a licence to remove those. The rest of that half is pinned
//! where it already lived: IL/DL's rectangular shift inside the margins by
//! `scroll_region::region_ops` (`grid_insert_lines_margined_rectangular_shift`,
//! `grid_delete_lines_margined_rectangular_shift`), and autowrap to the left
//! margin by aterm-conformance `vt_categories3::declrmm_autowrap_wraps_to_left_margin`.

use super::super::*;

/// Row 0 of a 3x20 grid filled with 'X', then DECSLRM margins armed at columns
/// 4..=10. The fill happens FIRST so the writes themselves are unmargined and
/// every one of the 20 columns really holds an 'X'.
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

// ===========================================================================
// EL — the in-place erase class. NOT bounded by DECSLRM.
// ===========================================================================

/// EL 0 runs to the ROW's edge, straight past the right margin.
///
/// xterm `do_erase_line` case 0 -> `ClearRight`, bounded by `MaxCols(screen)`;
/// Ghostty `eraseLine`'s right span is `{x, self.cols}`; iTerm2 uses
/// `size.width - 1`. Column 19 is nine columns right of the margin at 10.
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

/// EL 1 opens at COLUMN 0, not at the left margin.
///
/// xterm `ClearLeft` starts at column 0; Ghostty's EL 1 span is `{0, x + 1}`.
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

/// EL 2 erases the WHOLE row, not the margin span.
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

// ===========================================================================
// DECSEL — the same spans, with protection honoured inside them.
// ===========================================================================

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

// ===========================================================================
// ECH — the same class, and upstream literally the same function as EL 0.
// ===========================================================================

/// ECH erases `count` cells from the cursor regardless of the right margin.
///
/// VT510's ECH page: "ECH works inside or outside the scrolling margins."
/// xterm's `do_erase_char` is a call to `ClearRight` — the EL 0 function —
/// so an ECH bounded by the margins while EL 0 was not would be incoherent
/// upstream as well as wrong. Ghostty bounds by `self.cols - cursor.x`.
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

// ===========================================================================
// ED — already correct before this round; pinned so it stays that way.
// ===========================================================================

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

// ===========================================================================
// ICH / DCH — the OTHER half of the split. These KEEP their margins.
// ===========================================================================

/// ICH shifts within the margins: cells right of the right margin do not move,
/// and nothing falls off the row's end. VT510 ICH: "ICH has no effect outside
/// the scrolling margins."
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

/// DCH likewise: the fill lands at the RIGHT MARGIN, not at the row edge.
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

// ===========================================================================
// DECIC / DECDC — column shifts. These KEEP their margins too.
// ===========================================================================

/// DECIC shifts only the margin span: the columns pushed past the right margin
/// are lost, and the cell right of the right margin does not move. DEC STD 070
/// scopes DECIC to the scrolling margins, like ICH.
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

/// DECDC likewise: the blank columns open at the RIGHT MARGIN, not at the row
/// edge, and nothing right of the margin is pulled in.
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

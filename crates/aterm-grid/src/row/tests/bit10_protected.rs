// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! PROTECTED and WIDE_CONTINUATION share bit 10. The char-op fixups use the
//! context-aware `is_cell_wide_continuation` so they never misread a
//! DECSCA-protected cell as a spacer; `fixup_wide_boundary` keeps its ' '
//! spacer heuristic only when `clear_orphan_spacers` is set (whole-row rect
//! copies), not for the DECLRMM column shifts.

use super::*;
use crate::{Cell, CellFlags, PackedColor, StyleId};

fn normal(c: char) -> Cell {
    Cell::with_style_id(c, StyleId::DEFAULT, CellFlags::empty())
}

fn protected(c: char) -> Cell {
    Cell::with_style_id(c, StyleId::DEFAULT, CellFlags::PROTECTED)
}

fn ch(row: &Row, col: u16) -> char {
    row.get(col).unwrap().char()
}

#[test]
fn ich_preserves_left_neighbor_and_shifts_protected_cell() {
    // ICH at a DECSCA-protected cell must not blank the untouched left neighbor and
    // must shift the protected cell right (not destroy it).
    let (_p, mut row) = make_row(10);
    *row.get_mut(0).unwrap() = normal('X');
    *row.get_mut(1).unwrap() = protected('Y');

    row.insert_chars_fill(1, 1, Cell::EMPTY);

    assert_eq!(
        ch(&row, 0),
        'X',
        "cell left of the insertion point must be untouched"
    );
    assert_eq!(ch(&row, 1), ' ', "a blank is inserted at the cursor");
    assert_eq!(
        ch(&row, 2),
        'Y',
        "the protected cell shifts right, not destroyed"
    );
}

#[test]
fn dch_preserves_left_neighbor() {
    // DCH at a protected cell must not blank the cell left of the deletion range.
    let (_p, mut row) = make_row(10);
    *row.get_mut(0).unwrap() = normal('X');
    *row.get_mut(1).unwrap() = protected('Y');
    *row.get_mut(2).unwrap() = normal('Z');

    row.delete_chars_fill(1, 1, Cell::EMPTY);

    assert_eq!(
        ch(&row, 0),
        'X',
        "cell left of the deletion must be untouched"
    );
    assert_eq!(
        ch(&row, 1),
        'Z',
        "the cell right of the deletion shifts left"
    );
}

#[test]
fn dch_preserves_protected_cell_shifting_into_the_gap() {
    // A protected cell shifting into the deletion point survives.
    let (_p, mut row) = make_row(10);
    *row.get_mut(0).unwrap() = normal('X');
    *row.get_mut(1).unwrap() = normal('A');
    *row.get_mut(2).unwrap() = protected('W');

    row.delete_chars_fill(1, 1, Cell::EMPTY);

    assert_eq!(ch(&row, 0), 'X');
    assert_eq!(
        ch(&row, 1),
        'W',
        "the protected cell shifts left intact, not blanked"
    );
}

#[test]
fn ech_preserves_left_neighbor() {
    // ECH over a protected cell must not blank the cell left of the erase range.
    let (_p, mut row) = make_row(10);
    *row.get_mut(0).unwrap() = normal('B');
    *row.get_mut(1).unwrap() = protected('C');

    row.erase_chars_with(1, 1, Cell::EMPTY);

    assert_eq!(
        ch(&row, 0),
        'B',
        "cell left of the erase range must be untouched"
    );
}

#[test]
fn bounded_ich_dch_preserve_protected_cell_outside_the_margin() {
    // The DECLRMM bounded variants: a protected cell just left of the margin
    // (cells[col-1], outside [col, right_bound)) survives.
    let (_p, mut row) = make_row(10);
    *row.get_mut(1).unwrap() = normal('L'); // just left of the region
    *row.get_mut(2).unwrap() = protected('P'); // region left edge (cursor)
    row.insert_chars_bounded_fill(2, 1, 8, Cell::EMPTY);
    assert_eq!(
        ch(&row, 1),
        'L',
        "bounded ICH must not blank the cell left of the margin"
    );

    let (_p2, mut row2) = make_row(10);
    *row2.get_mut(1).unwrap() = normal('L');
    *row2.get_mut(2).unwrap() = protected('P');
    row2.delete_chars_bounded_fill(2, 1, 8, Cell::EMPTY);
    assert_eq!(
        ch(&row2, 1),
        'L',
        "bounded DCH must not blank the cell left of the margin"
    );
}

#[test]
fn fixup_wide_boundary_preserves_protected_visible_cells() {
    // The orphan-continuation branches blank only ' ' spacers, never a
    // protected visible cell.
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    let _ = (fg, bg);

    // Left branch: protected 'Z' at `left` with a non-wide left neighbor.
    let (_p, mut row) = make_row(10);
    *row.get_mut(2).unwrap() = normal('A'); // left-1, not WIDE
    *row.get_mut(3).unwrap() = protected('Z'); // bit 10 set, but visible text
    row.fixup_wide_boundary(3, 6, 10, true);
    assert_eq!(
        ch(&row, 3),
        'Z',
        "protected visible cell at the boundary must survive"
    );

    // Right branch: protected 'Q' at right+1 (outside the region), non-wide at right.
    let (_p2, mut row2) = make_row(10);
    *row2.get_mut(5).unwrap() = normal('B'); // `right`, not WIDE
    *row2.get_mut(6).unwrap() = protected('Q'); // right+1, outside [left,right]
    row2.fixup_wide_boundary(2, 5, 10, true);
    assert_eq!(
        ch(&row2, 6),
        'Q',
        "protected cell past the right margin must not be blanked"
    );
}

#[test]
fn write_paths_do_not_destroy_left_neighbor_when_overwriting_a_protected_cell() {
    // The write-path wide fixups (write_char / write_char_packed, fill_cell_run,
    // write_wide_char_packed) overwriting a DECSCA-protected cell at col leave
    // the cell at col-1 alone. A wide char elsewhere in the row sets
    // HAS_WIDE_CHARS so the fixup path runs.
    use crate::PackedColors;
    let colors = PackedColors::DEFAULT;

    // Path (b): write_char_packed → fixup_wide_char_overwrite.
    let (_p, mut row) = make_row(10);
    row.write_wide_char_packed(5, '\u{4E2D}', colors, CellFlags::empty()); // sets HAS_WIDE_CHARS
    row.write_char_packed(0, 'A', colors, CellFlags::empty());
    row.write_char_packed(1, 'B', colors, CellFlags::PROTECTED);
    row.write_char_packed(1, 'X', colors, CellFlags::empty()); // overwrite the protected cell
    assert_eq!(
        ch(&row, 0),
        'A',
        "path B: left neighbor of an overwritten protected cell destroyed"
    );
    assert_eq!(ch(&row, 1), 'X');

    // Path (c): write_char (unstyled).
    let (_p2, mut row2) = make_row(10);
    row2.write_wide_char_packed(5, '\u{4E2D}', colors, CellFlags::empty());
    row2.write_char_packed(0, 'A', colors, CellFlags::empty());
    row2.write_char_packed(1, 'B', colors, CellFlags::PROTECTED);
    row2.write_char(1, 'X');
    assert_eq!(
        ch(&row2, 0),
        'A',
        "path C: write_char destroyed the left neighbor"
    );

    // Path (a): fill_cell_run → fixup_wide_chars_in_range.
    let (_p3, mut row3) = make_row(10);
    row3.write_wide_char_packed(5, '\u{4E2D}', colors, CellFlags::empty());
    row3.write_char_packed(0, 'A', colors, CellFlags::empty());
    row3.write_char_packed(1, 'B', colors, CellFlags::PROTECTED);
    row3.fill_cell_run(1, 1, Cell::from_ascii_fast(b'X'));
    assert_eq!(
        ch(&row3, 0),
        'A',
        "path A: fill_cell_run destroyed the left neighbor"
    );

    // Path (d): write_wide_char_packed → fixup_wide_char_write, starting on a protected cell.
    let (_p4, mut row4) = make_row(10);
    row4.write_wide_char_packed(7, '\u{4E2D}', colors, CellFlags::empty());
    row4.write_char_packed(0, 'A', colors, CellFlags::empty());
    row4.write_char_packed(1, 'B', colors, CellFlags::PROTECTED);
    row4.write_wide_char_packed(1, '\u{4E00}', colors, CellFlags::empty());
    assert_eq!(
        ch(&row4, 0),
        'A',
        "path D: write_wide_char_packed destroyed the left neighbor"
    );
}

#[test]
fn write_over_a_genuine_wide_continuation_still_clears_the_orphaned_head() {
    // Overwriting a genuine wide continuation still clears its head.
    use crate::PackedColors;
    let colors = PackedColors::DEFAULT;
    let (_p, mut row) = make_row(10);
    row.write_wide_char_packed(0, '\u{4E2D}', colors, CellFlags::empty()); // col0 WIDE, col1 continuation
    assert!(row.get(0).unwrap().is_wide());
    row.write_char_packed(1, 'X', colors, CellFlags::empty()); // overwrite the continuation
    assert!(
        !row.get(0).unwrap().is_wide(),
        "orphaned wide head must be cleared"
    );
    assert_eq!(ch(&row, 0), ' ', "orphaned wide head becomes blank");
    assert_eq!(ch(&row, 1), 'X');
}

#[test]
fn fixup_wide_boundary_suppressed_preserves_protected_spaces() {
    // The DECLRMM column shifts pass clear_orphan_spacers=false, so a protected
    // space at the boundary (indistinguishable from an orphaned spacer)
    // survives, including one column past the right margin.
    let protected_space = protected(' ');

    // Left boundary: protected space at `left`, non-wide left neighbor.
    let (_p, mut row) = make_row(10);
    *row.get_mut(2).unwrap() = normal('A'); // left-1, not WIDE
    *row.get_mut(3).unwrap() = protected_space; // bit 10 set, char ' '
    row.fixup_wide_boundary(3, 6, 10, false);
    assert!(
        row.get(3).unwrap().is_protected(),
        "suppressed mode must not clear a protected space at the left boundary"
    );

    // Right boundary: protected space at right+1 (outside the operated region).
    let (_p2, mut row2) = make_row(10);
    *row2.get_mut(5).unwrap() = normal('B'); // `right`, not WIDE
    *row2.get_mut(6).unwrap() = protected_space; // right+1, outside [left,right]
    row2.fixup_wide_boundary(2, 5, 10, false);
    assert!(
        row2.get(6).unwrap().is_protected(),
        "suppressed mode must not clear a protected space past the right margin (the SL bug)"
    );
}

#[test]
fn fixup_wide_boundary_suppressed_still_clears_dangling_wide_heads() {
    // The unambiguous branches (a dangling WIDE head) run even with
    // clear_orphan_spacers=false.
    let wide = |c: char| Cell::with_style_id(c, StyleId::DEFAULT, CellFlags::WIDE);

    // Left: dangling WIDE head at left-1 (left is a normal, non-continuation cell).
    let (_p, mut row) = make_row(10);
    *row.get_mut(2).unwrap() = wide('中'); // left-1 = 2, WIDE head
    *row.get_mut(3).unwrap() = normal('N'); // left = 3, NOT a continuation
    row.fixup_wide_boundary(3, 6, 10, false);
    assert!(
        !row.get(2).unwrap().is_wide(),
        "dangling wide head at left-1 must still be cleared in suppressed mode"
    );

    // Right: dangling WIDE head at `right` (right+1 is a normal, non-continuation cell).
    let (_p2, mut row2) = make_row(10);
    *row2.get_mut(5).unwrap() = wide('中'); // right = 5, WIDE head
    *row2.get_mut(6).unwrap() = normal('N'); // right+1 = 6, NOT a continuation
    row2.fixup_wide_boundary(2, 5, 10, false);
    assert!(
        !row2.get(5).unwrap().is_wide(),
        "dangling wide head at right must still be cleared in suppressed mode"
    );
}

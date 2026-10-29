// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for bit-10 `PROTECTED` and `WIDE_CONTINUATION` disambiguation in row operations.

use super::*;
use crate::{Cell, CellFlags, PackedColor};

fn normal(c: char) -> Cell {
    Cell::with_style(
        c,
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    )
}

fn protected(c: char) -> Cell {
    Cell::with_style(
        c,
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::PROTECTED,
    )
}

fn ch(row: &Row, col: u16) -> char {
    row.get(col).unwrap().char()
}

#[test]
fn ich_preserves_left_neighbor_and_shifts_protected_cell() {
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
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    let _ = (fg, bg);

    let (_p, mut row) = make_row(10);
    *row.get_mut(2).unwrap() = normal('A'); // left-1, not WIDE
    *row.get_mut(3).unwrap() = protected('Z'); // bit 10 set, but visible text
    row.fixup_wide_boundary(3, 6, 10, true);
    assert_eq!(
        ch(&row, 3),
        'Z',
        "protected visible cell at the boundary must survive"
    );

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
    use crate::PackedColors;
    let colors = PackedColors::DEFAULT;

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
    let protected_space = protected(' ');

    let (_p, mut row) = make_row(10);
    *row.get_mut(2).unwrap() = normal('A'); // left-1, not WIDE
    *row.get_mut(3).unwrap() = protected_space; // bit 10 set, char ' '
    row.fixup_wide_boundary(3, 6, 10, false);
    assert!(
        row.get(3).unwrap().is_protected(),
        "suppressed mode must not clear a protected space at the left boundary"
    );

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
    let wide = |c: char| {
        Cell::with_style(
            c,
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::WIDE,
        )
    };

    let (_p, mut row) = make_row(10);
    *row.get_mut(2).unwrap() = wide('中'); // left-1 = 2, WIDE head
    *row.get_mut(3).unwrap() = normal('N'); // left = 3, NOT a continuation
    row.fixup_wide_boundary(3, 6, 10, false);
    assert!(
        !row.get(2).unwrap().is_wide(),
        "dangling wide head at left-1 must still be cleared in suppressed mode"
    );

    let (_p2, mut row2) = make_row(10);
    *row2.get_mut(5).unwrap() = wide('中'); // right = 5, WIDE head
    *row2.get_mut(6).unwrap() = normal('N'); // right+1 = 6, NOT a continuation
    row2.fixup_wide_boundary(2, 5, 10, false);
    assert!(
        !row2.get(5).unwrap().is_wide(),
        "dangling wide head at right must still be cleared in suppressed mode"
    );
}

#[test]
fn display_and_debug_preserve_decsca_protected_glyphs_and_skip_wide_spacers() {
    use crate::PackedColors;
    let colors = PackedColors::DEFAULT;
    let (_p, mut row) = make_row(10);
    row.write_char_packed(0, 'P', colors, CellFlags::PROTECTED);
    row.write_wide_char_packed(1, '中', colors, CellFlags::PROTECTED);
    row.write_char_packed(3, 'Q', colors, CellFlags::PROTECTED);
    row.write_char_packed(4, '!', colors, CellFlags::empty());

    assert_eq!(row.to_string(), "P中Q!");
    assert!(
        format!("{row:?}").contains("\"P中Q!\""),
        "Debug must include DECSCA-protected characters in content"
    );
}

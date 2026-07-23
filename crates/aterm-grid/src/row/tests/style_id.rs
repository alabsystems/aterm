// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// StyleId write methods tests: write_char_with_style_id and
// write_wide_char_with_style_id.

use super::make_row;

#[test]
fn row_write_char_with_style_id_basic() {
    use crate::{CellFlags, StyleId};

    let (_pages, mut row) = make_row(80);
    let style_id = StyleId::new(42);

    // Write a char with StyleId
    assert!(row.write_char_with_style_id(0, 'H', style_id, CellFlags::empty()));
    assert!(row.write_char_with_style_id(1, 'i', style_id, CellFlags::empty()));

    // Verify the cells
    let cell0 = row.get(0).unwrap();
    let cell1 = row.get(1).unwrap();

    assert_eq!(cell0.char(), 'H');
    assert!(cell0.uses_style_id());
    assert_eq!(cell0.style_id(), style_id);

    assert_eq!(cell1.char(), 'i');
    assert!(cell1.uses_style_id());
    assert_eq!(cell1.style_id(), style_id);

    assert_eq!(row.len(), 2);
    assert!(row.is_dirty());
}

#[test]
fn row_write_char_with_style_id_preserves_flags() {
    use crate::{CellFlags, StyleId};

    let (_pages, mut row) = make_row(80);
    let style_id = StyleId::new(100);

    // Write with BOLD flag (cell-level attribute)
    row.write_char_with_style_id(5, 'X', style_id, CellFlags::BOLD);

    let cell = row.get(5).unwrap();
    assert_eq!(cell.char(), 'X');
    assert!(cell.uses_style_id());
    assert_eq!(cell.style_id(), style_id);
    assert!(cell.flags().contains(CellFlags::BOLD));
}

#[test]
fn row_write_char_with_style_id_out_of_bounds() {
    use crate::{CellFlags, StyleId};

    let (_pages, mut row) = make_row(10);
    let style_id = StyleId::new(1);

    // Write at valid position
    assert!(row.write_char_with_style_id(9, 'Z', style_id, CellFlags::empty()));

    // Write at invalid position
    assert!(!row.write_char_with_style_id(10, 'X', style_id, CellFlags::empty()));
    assert!(!row.write_char_with_style_id(100, 'Y', style_id, CellFlags::empty()));
}

#[test]
fn row_write_char_with_style_id_overwrites_wide_continuation() {
    use crate::{CellFlags, PackedColor, StyleId};

    let (_pages, mut row) = make_row(80);

    // First write a wide char using inline colors
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());

    // Verify wide char setup
    assert!(row.get(0).unwrap().is_wide());
    assert!(row.get(1).unwrap().is_wide_continuation());

    // Now overwrite the continuation (col 1) with StyleId
    let style_id = StyleId::new(77);
    row.write_char_with_style_id(1, 'A', style_id, CellFlags::empty());

    // First cell should be cleared to space
    let cell0 = row.get(0).unwrap();
    assert_eq!(cell0.char(), ' ');

    // Second cell should have our new character with StyleId
    let cell1 = row.get(1).unwrap();
    assert_eq!(cell1.char(), 'A');
    assert!(cell1.uses_style_id());
    assert_eq!(cell1.style_id(), style_id);
}

#[test]
fn row_write_wide_char_with_style_id_basic() {
    use crate::{CellFlags, StyleId};

    let (_pages, mut row) = make_row(80);
    let style_id = StyleId::new(50);

    // Write a wide character
    let ok = row.write_wide_char_with_style_id(0, '\u{4E2D}', style_id, CellFlags::empty());
    assert!(ok);

    // Verify the cells
    let cell0 = row.get(0).unwrap();
    let cell1 = row.get(1).unwrap();

    // First cell: character with WIDE flag and StyleId
    assert_eq!(cell0.char(), '\u{4E2D}');
    assert!(cell0.is_wide());
    assert!(cell0.uses_style_id());
    assert_eq!(cell0.style_id(), style_id);

    // Second cell: continuation with StyleId
    assert_eq!(cell1.char(), ' ');
    assert!(cell1.is_wide_continuation());
    assert!(cell1.uses_style_id());
    assert_eq!(cell1.style_id(), style_id);

    assert_eq!(row.len(), 2);
}

#[test]
fn row_write_wide_char_with_style_id_at_edge() {
    use crate::{CellFlags, StyleId};

    let (_pages, mut row) = make_row(10);
    let style_id = StyleId::new(1);

    // Write wide char at last valid position (col 8, needs 8 and 9)
    let ok = row.write_wide_char_with_style_id(8, '\u{4E2D}', style_id, CellFlags::empty());
    assert!(ok);

    // Try to write at col 9 - no room for 2 cells
    let ok = row.write_wide_char_with_style_id(9, '\u{4E2D}', style_id, CellFlags::empty());
    assert!(!ok);
}

#[test]
fn row_write_wide_char_with_style_id_overwrites_existing_wide() {
    use crate::{CellFlags, StyleId};

    let (_pages, mut row) = make_row(80);
    let style_id1 = StyleId::new(10);
    let style_id2 = StyleId::new(20);

    // Write first wide char at col 0
    row.write_wide_char_with_style_id(0, '\u{4E00}', style_id1, CellFlags::empty());

    // Write second wide char at col 1 (overlaps continuation of first)
    row.write_wide_char_with_style_id(1, '\u{4E8C}', style_id2, CellFlags::empty());

    // First cell should be cleared
    let cell0 = row.get(0).unwrap();
    assert_eq!(cell0.char(), ' ');

    // Cells 1-2 should have the new wide char
    let cell1 = row.get(1).unwrap();
    let cell2 = row.get(2).unwrap();

    assert_eq!(cell1.char(), '\u{4E8C}');
    assert!(cell1.is_wide());
    assert!(cell1.uses_style_id());
    assert_eq!(cell1.style_id(), style_id2);

    assert!(cell2.is_wide_continuation());
    assert!(cell2.uses_style_id());
    assert_eq!(cell2.style_id(), style_id2);
}

#[test]
fn row_write_wide_char_with_style_id_preserves_flags() {
    use crate::{CellFlags, StyleId};

    let (_pages, mut row) = make_row(80);
    let style_id = StyleId::new(99);

    // Write wide char with UNDERLINE flag
    row.write_wide_char_with_style_id(0, '\u{4E2D}', style_id, CellFlags::UNDERLINE);

    let cell0 = row.get(0).unwrap();
    // WIDE flag should be added, along with our UNDERLINE
    assert!(cell0.flags().contains(CellFlags::WIDE));
    assert!(cell0.flags().contains(CellFlags::UNDERLINE));
    assert!(cell0.uses_style_id());
}

// ── Round 15: #7522 len class on the StyleId write paths ─────────────────
// These mirror write_char_packed / write_wide_char_packed exactly: a wide-orphan
// clear at the tail must tighten len rather than leave it stale-high.

/// write_char_with_style_id: single-width write over a WIDE head at the tail must
/// tighten len when the cleared continuation was the row's last content.
#[test]
fn write_char_with_style_id_shrinks_len_on_tail_wide_head_overwrite() {
    use crate::{CellFlags, StyleId};

    let (_pages, mut row) = make_row(10);
    let style_id = StyleId::new(7);
    row.write_wide_char_with_style_id(8, '\u{4E2D}', style_id, CellFlags::empty());
    assert_eq!(row.len(), 10, "wide char at 8-9 → len 10");

    assert!(row.write_char_with_style_id(8, 'X', style_id, CellFlags::empty()));
    assert!(
        row.get(9).unwrap().is_empty(),
        "orphaned continuation at col 9 must be empty"
    );
    assert_eq!(
        row.len(),
        9,
        "len must shrink to 9 (no phantom trailing space)"
    );
}

/// write_wide_char_with_style_id: a new wide char orphaning an existing wide
/// char's continuation at the tail must tighten len.
#[test]
fn write_wide_char_with_style_id_shrinks_len_on_tail_wide_orphan() {
    use crate::{CellFlags, StyleId};

    let (_pages, mut row) = make_row(4);
    let style_id = StyleId::new(11);
    row.write_wide_char_with_style_id(2, '\u{4E00}', style_id, CellFlags::empty());
    assert_eq!(row.len(), 4, "wide char at 2-3 → len 4");

    assert!(row.write_wide_char_with_style_id(1, '\u{4E8C}', style_id, CellFlags::empty()));
    assert!(row.get(1).unwrap().is_wide());
    assert!(row.get(2).unwrap().is_wide_continuation());
    assert!(
        row.get(3).unwrap().is_empty(),
        "orphaned continuation at col 3 must be empty"
    );
    assert_eq!(
        row.len(),
        3,
        "len must shrink to 3 (no phantom trailing space)"
    );
}

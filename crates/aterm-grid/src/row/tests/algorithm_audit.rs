// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::*;
use super::make_row;

#[test]
fn insert_chars_at_wide_continuation_clears_orphan() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());

    row.insert_chars(3, 1);

    let cell2 = row.get(2).unwrap();
    let cell3 = row.get(3).unwrap();

    assert_eq!(
        cell2.flags() & CellFlags::WIDE,
        CellFlags::empty(),
        "orphaned WIDE cell at col 2 should be cleared"
    );
    assert_eq!(
        cell3.flags() & CellFlags::WIDE_CONTINUATION,
        CellFlags::empty(),
        "col 3 should be blank (inserted), not continuation"
    );
}

#[test]
fn delete_chars_splits_wide_char_at_boundary() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_char_styled(0, 'A', fg, bg, CellFlags::empty());
    row.write_wide_char(1, '\u{4E2D}', fg, bg, CellFlags::empty());
    row.write_char_styled(3, 'B', fg, bg, CellFlags::empty());

    row.delete_chars(1, 1);

    let cell0 = row.get(0).unwrap();
    let cell1 = row.get(1).unwrap();

    assert_eq!(cell0.char(), 'A', "col 0 should be unchanged");

    assert_eq!(
        cell1.flags() & CellFlags::WIDE_CONTINUATION,
        CellFlags::empty(),
        "orphaned WIDE_CONTINUATION at col 1 should be cleared"
    );
    assert_eq!(
        row.get(2).unwrap().char(),
        'B',
        "'B' should shift from col 3 to col 2"
    );
}

#[test]
fn delete_chars_removes_continuation_orphans_wide() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
    row.write_char_styled(6, 'X', fg, bg, CellFlags::empty());
    row.write_char_styled(7, 'Y', fg, bg, CellFlags::empty());

    row.delete_chars(5, 1);

    let cell4 = row.get(4).unwrap();
    let cell5 = row.get(5).unwrap();

    assert_eq!(
        cell4.flags() & CellFlags::WIDE,
        CellFlags::empty(),
        "orphaned WIDE cell at col 4 should be cleared"
    );
    assert_eq!(cell5.char(), 'X', "'X' should shift from col 6 to col 5");
    assert_eq!(
        row.get(6).unwrap().char(),
        'Y',
        "'Y' should shift from col 7 to col 6"
    );
}

#[test]
fn insert_chars_count_exceeds_remaining_space() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDE".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.len(), 5);

    row.insert_chars(2, 20);

    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(1).unwrap().char(), 'B');

    for col in 2..10u16 {
        assert_eq!(
            row.get(col).unwrap().char(),
            ' ',
            "col {col} should be blank after oversized insert"
        );
    }
}

#[test]
fn selective_clear_range_len_with_protected_at_end() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_char_styled(0, 'A', fg, bg, CellFlags::empty());
    row.write_char_styled(1, 'B', fg, bg, CellFlags::empty());
    row.write_char_styled(2, 'C', fg, bg, CellFlags::empty());
    row.write_char_styled(3, 'P', fg, bg, CellFlags::PROTECTED);
    row.write_char_styled(4, 'D', fg, bg, CellFlags::empty());
    assert_eq!(row.len(), 5);

    row.selective_clear_range(0, 5, Cell::EMPTY);

    assert_eq!(
        row.get(3).unwrap().char(),
        'P',
        "protected cell should survive"
    );
    assert_eq!(row.get(4).unwrap().char(), ' ', "D should be erased");

    assert_eq!(
        row.len(),
        4,
        "len should account for protected cell at col 3"
    );
}

#[test]
fn selective_clear_all_protected_preserves_len() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(5);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    for i in 0..5u16 {
        row.write_char_styled(i, char::from(b'A' + i as u8), fg, bg, CellFlags::PROTECTED);
    }
    assert_eq!(row.len(), 5);

    row.selective_clear(Cell::EMPTY);

    assert_eq!(row.len(), 5);
    for i in 0..5u16 {
        assert_eq!(
            row.get(i).unwrap().char(),
            char::from(b'A' + i as u8),
            "protected cell at col {i} should survive"
        );
    }
}

#[test]
fn clear_range_middle_preserves_len() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDEFGHIJ".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.len(), 10);

    row.clear_range_with(3, 6, Cell::EMPTY);
    assert_eq!(
        row.len(),
        10,
        "len should remain 10 when clearing middle of row"
    );
    assert_eq!(row.get(6).unwrap().char(), 'G');
    assert_eq!(row.get(9).unwrap().char(), 'J');
}

#[test]
fn erase_chars_middle_preserves_len() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDEFGHIJ".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.len(), 10);

    row.erase_chars(2, 3);
    assert_eq!(
        row.len(),
        10,
        "len should remain 10 when erasing middle of row"
    );
    assert_eq!(row.get(5).unwrap().char(), 'F');
}

#[test]
fn erase_chars_wide_head_at_tail_recalcs_len() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_char(0, 'A');
    row.write_char(1, 'B');
    row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert_eq!(row.len(), 4);
    assert!(row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());

    row.erase_chars(2, 1);

    assert_eq!(
        row.len(),
        2,
        "len must shrink to 2 after erasing the tail wide pair"
    );
    assert_eq!(row.to_string(), "AB", "no trailing blanks should leak");
    assert!(row.get(2).unwrap().is_empty());
    assert!(row.get(3).unwrap().is_empty());
}

#[test]
fn row_set_page_max_cols_last_valid() {
    let (_pages, mut row) = make_row(8192);
    let cell = Cell::new('X');

    let last_col: u16 = 8191;
    assert!(
        row.set(last_col, cell),
        "set at col 8191 should succeed on 8192-col row"
    );
    assert_eq!(row.len(), 8192, "len should be 8192 after writing col 8191");

    assert!(
        !row.set(8192, cell),
        "set at col 8192 should fail (OOB on 8192-element row)"
    );
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! DECDWL/DECDHL reflow tests — double-width/height lines must NOT be reflowed.
//!
//! Per VT spec, DECDWL lines are logically half-width (40 usable columns on an
//! 80-column terminal). They must be resized in place during reflow, not split
//! or merged with adjacent lines like single-width content.
//!
//! Regression tests for #7524.

use crate::{Grid, LineSize};

#[test]
fn decdwl_row_preserved_on_shrink() {
    // An 80-col DECDWL row with content in cols 0-9 should NOT be split
    // when the terminal shrinks to 40 columns.
    let mut grid = Grid::new(5, 80);
    for i in 0..10u16 {
        grid.write_char((b'A' + i as u8) as char);
    }
    grid.row_mut(0)
        .unwrap()
        .set_line_size(LineSize::DoubleWidth);

    grid.resize(5, 40);
    grid.assert_invariants();

    let row0 = grid.row(0).unwrap();
    assert_eq!(
        row0.line_size(),
        LineSize::DoubleWidth,
        "DECDWL attribute must survive shrink reflow"
    );
    // Content should be truncated to fit, not wrapped to a new row.
    assert_eq!(row0.get(0).unwrap().char_data(), 'A' as u16);
    assert_eq!(row0.get(9).unwrap().char_data(), 'J' as u16);
}

#[test]
fn decdwl_row_preserved_on_grow() {
    // A 40-col DECDWL row should not merge with the next row when growing to 80.
    let mut grid = Grid::new(5, 40);
    for i in 0..10u16 {
        grid.write_char((b'A' + i as u8) as char);
    }
    grid.row_mut(0)
        .unwrap()
        .set_line_size(LineSize::DoubleWidth);

    // Put content on row 1 to verify it doesn't merge.
    grid.set_cursor(1, 0);
    grid.write_char('Z');

    grid.resize(5, 80);
    grid.assert_invariants();

    let row0 = grid.row(0).unwrap();
    assert_eq!(
        row0.line_size(),
        LineSize::DoubleWidth,
        "DECDWL attribute must survive grow reflow"
    );
    assert_eq!(row0.get(0).unwrap().char_data(), 'A' as u16);

    let row1 = grid.row(1).unwrap();
    assert_eq!(
        row1.get(0).unwrap().char_data(),
        'Z' as u16,
        "DECDWL row must not merge with next row on grow"
    );
}

#[test]
fn decdhl_top_bottom_preserved_on_shrink() {
    let mut grid = Grid::new(5, 80);
    grid.write_char('T');
    grid.row_mut(0)
        .unwrap()
        .set_line_size(LineSize::DoubleHeightTop);

    grid.set_cursor(1, 0);
    grid.write_char('B');
    grid.row_mut(1)
        .unwrap()
        .set_line_size(LineSize::DoubleHeightBottom);

    grid.resize(5, 40);
    grid.assert_invariants();

    assert_eq!(
        grid.row(0).unwrap().line_size(),
        LineSize::DoubleHeightTop,
        "DECDHL top half must survive shrink"
    );
    assert_eq!(
        grid.row(1).unwrap().line_size(),
        LineSize::DoubleHeightBottom,
        "DECDHL bottom half must survive shrink"
    );
}

#[test]
fn decdwl_row_not_split_even_when_full() {
    // A fully-filled DECDWL row shrunk to half its width should truncate,
    // not wrap. The second half of the content is lost (no new row created).
    let mut grid = Grid::new(3, 80);
    for _i in 0..80u16 {
        grid.write_char('X');
    }
    grid.row_mut(0)
        .unwrap()
        .set_line_size(LineSize::DoubleWidth);

    let rows_before = grid.rows();
    grid.resize(3, 40);
    grid.assert_invariants();

    assert_eq!(
        grid.row(0).unwrap().line_size(),
        LineSize::DoubleWidth,
        "DECDWL must be preserved"
    );
    // Row count should not increase — DECDWL rows don't split.
    assert_eq!(grid.rows(), rows_before);
}

#[test]
fn decdwl_cursor_clamped_on_shrink() {
    let mut grid = Grid::new(5, 80);
    for _i in 0..30u16 {
        grid.write_char('A');
    }
    grid.row_mut(0)
        .unwrap()
        .set_line_size(LineSize::DoubleWidth);
    grid.set_cursor(0, 35);

    grid.resize(5, 40);
    grid.assert_invariants();

    assert_eq!(grid.cursor_row(), 0, "cursor should stay on row 0");
    // Cursor clamped to new_cols - 1 = 39 (the resize clamps it).
    assert!(
        grid.cursor_col() <= 39,
        "cursor col {} should be <= 39",
        grid.cursor_col()
    );
    assert_eq!(grid.row(0).unwrap().line_size(), LineSize::DoubleWidth);
}

#[test]
fn decdwl_round_trip_shrink_then_grow() {
    // Shrink then grow should preserve DECDWL attribute and content
    // (content beyond the shrunk width is lost, as expected).
    let mut grid = Grid::new(5, 80);
    for i in 0..20u16 {
        grid.write_char((b'A' + (i % 26) as u8) as char);
    }
    grid.row_mut(0)
        .unwrap()
        .set_line_size(LineSize::DoubleWidth);

    // Shrink to 40 — content still fits (20 chars < 40 cols).
    grid.resize(5, 40);
    assert_eq!(grid.row(0).unwrap().line_size(), LineSize::DoubleWidth);
    assert_eq!(grid.row(0).unwrap().get(0).unwrap().char_data(), 'A' as u16);

    // Grow back to 80 — row should stay as a single DECDWL row, not merge.
    grid.resize(5, 80);
    grid.assert_invariants();
    assert_eq!(grid.row(0).unwrap().line_size(), LineSize::DoubleWidth);
    assert_eq!(grid.row(0).unwrap().get(0).unwrap().char_data(), 'A' as u16);
}

#[test]
fn decdhl_pair_preserved_on_grow() {
    // DECDHL top+bottom pair should not merge with adjacent rows on grow.
    let mut grid = Grid::new(5, 40);
    grid.write_char('T');
    grid.row_mut(0)
        .unwrap()
        .set_line_size(LineSize::DoubleHeightTop);

    grid.set_cursor(1, 0);
    grid.write_char('B');
    grid.row_mut(1)
        .unwrap()
        .set_line_size(LineSize::DoubleHeightBottom);

    grid.set_cursor(2, 0);
    grid.write_char('N'); // Normal row below the pair.

    grid.resize(5, 80);
    grid.assert_invariants();

    assert_eq!(grid.row(0).unwrap().line_size(), LineSize::DoubleHeightTop);
    assert_eq!(
        grid.row(1).unwrap().line_size(),
        LineSize::DoubleHeightBottom
    );
    assert_eq!(grid.row(2).unwrap().line_size(), LineSize::SingleWidth);
    assert_eq!(
        grid.row(2).unwrap().get(0).unwrap().char_data(),
        'N' as u16,
        "normal row below DECDHL pair must not be absorbed"
    );
}

#[test]
fn decdwl_mixed_with_normal_rows_on_shrink() {
    // A grid with interleaved DECDWL and normal rows: only normal rows reflow.
    let mut grid = Grid::new(5, 20);

    // Row 0: DECDWL with "ABCDEFGHIJ" (10 chars)
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.row_mut(0)
        .unwrap()
        .set_line_size(LineSize::DoubleWidth);

    // Row 1: normal with "1234567890" (10 chars)
    grid.set_cursor(1, 0);
    for c in "1234567890".chars() {
        grid.write_char(c);
    }

    // Shrink to 5 columns.
    grid.resize(5, 5);
    grid.assert_invariants();

    // Row 0 should still be DECDWL, content truncated to 5 cols.
    assert_eq!(grid.row(0).unwrap().line_size(), LineSize::DoubleWidth);
    assert_eq!(grid.row(0).unwrap().get(0).unwrap().char_data(), 'A' as u16);
    assert_eq!(grid.row(0).unwrap().get(4).unwrap().char_data(), 'E' as u16);

    // Row 1 should be the normal row's first chunk "12345".
    assert_eq!(grid.row(1).unwrap().line_size(), LineSize::SingleWidth);
    assert_eq!(grid.row(1).unwrap().get(0).unwrap().char_data(), '1' as u16);

    // Row 2 should be the normal row's second chunk "67890" (wrapped).
    assert!(grid.row(2).unwrap().is_wrapped());
    assert_eq!(grid.row(2).unwrap().get(0).unwrap().char_data(), '6' as u16);
}

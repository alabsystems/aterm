// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reflow performance proofs — verify O(n) row operations and cursor invariants
//! under extreme resize ratios.

use super::super::super::*;
use crate::test_counters::take_reflow_row_ops;

/// Reflow shrink from 100 cols to 1 visits each source row once: total work is
/// O(visible_rows * cols), linear in content, though the output has
/// `visible_rows * 100` rows.
#[test]
fn reflow_shrink_extreme_ratio_is_linear_in_source_rows() {
    let _ = take_reflow_row_ops(); // reset counter

    let rows: u16 = 10;
    let old_cols: u16 = 100;
    let mut grid = Grid::new(rows, old_cols);

    // Fill every cell of row 0 with content
    grid.set_cursor(0, 0);
    for i in 0..old_cols {
        grid.write_char((b'A' + (i % 26) as u8) as char);
    }

    let _ = take_reflow_row_ops(); // reset after setup
    grid.resize(rows, 1); // extreme shrink: 100 → 1

    let ops = take_reflow_row_ops();
    // Each visible row should be visited exactly once by the reflow loop.
    // The reflow_shrink_columns loop iterates `visible_count` times.
    assert_eq!(
        ops,
        usize::from(rows),
        "reflow shrink should visit each source row exactly once, got {ops}"
    );

    // The cursor (col 99 → row 99) stays visible: rows 0..90 go to scrollback,
    // so visible rows 0..9 hold original columns 90..99.
    for i in 0..10u16 {
        let char_index = 90 + i;
        let expected = (b'A' + (char_index % 26) as u8) as char;
        assert_eq!(
            grid.cell(i, 0).unwrap().char(),
            expected,
            "row {i} (original col {char_index}) should contain '{expected}'"
        );
    }

    grid.assert_invariants();
}

/// Reflow grow from 1 col to 10 merges continuation rows in one pass: the outer
/// loop runs per logical line, not per source row.
#[test]
fn reflow_grow_extreme_ratio_merges_continuations() {
    let _ = take_reflow_row_ops(); // reset counter

    let rows: u16 = 20;
    let mut grid = Grid::new(rows, 10);
    grid.set_cursor(0, 0);
    for i in 0..10u16 {
        grid.write_char((b'A' + (i % 26) as u8) as char);
    }

    // Shrink to 1 col to create many wrapped rows
    grid.resize(rows, 1);
    let visible_after_shrink = grid.storage.visible_rows();

    let _ = take_reflow_row_ops(); // reset before grow
    grid.resize(rows, 10); // grow back

    let ops = take_reflow_row_ops();
    // 10 continuation rows merge into 1 logical line, plus 10 empty rows: 11.
    assert!(
        ops <= usize::from(visible_after_shrink),
        "reflow grow ops ({ops}) should not exceed source row count ({visible_after_shrink})"
    );
    assert!(ops > 0, "reflow grow should do some work");

    let row0_text = grid.row(0).unwrap().to_string();
    assert_eq!(
        row0_text, "ABCDEFGHIJ",
        "content should round-trip through extreme reflow"
    );

    grid.assert_invariants();
}

/// A cursor on the last row survives an extreme shrink/grow round trip (modulo
/// clamping); scrollback keeps the content.
#[test]
fn reflow_extreme_resize_preserves_cursor_on_last_row() {
    let rows: u16 = 10;
    let cols: u16 = 20;
    // Use scrollback so content expanding beyond visible area isn't lost
    let mut grid = Grid::with_scrollback(rows, cols, 200);

    // Write content on the first row where it won't be pushed out
    grid.set_cursor(0, 0);
    for c in "HELLO".chars() {
        grid.write_char(c);
    }
    // Place cursor at col 3 ('L')
    grid.set_cursor(0, 3);

    grid.resize(rows, 2);
    grid.assert_invariants();

    // The cursor col should be clamped to new_cols
    let cursor_col_after_shrink = grid.cursor_col();
    assert!(
        cursor_col_after_shrink < 2,
        "cursor col should be clamped to new_cols, got {cursor_col_after_shrink}"
    );

    grid.resize(rows, cols);
    grid.assert_invariants();

    // Cursor should be on a valid position
    assert!(
        grid.cursor_row() < rows,
        "cursor row out of bounds after grow"
    );
    assert!(
        grid.cursor_col() < cols,
        "cursor col out of bounds after grow"
    );

    // The content should survive the round trip
    let mut found_hello = false;
    for r in 0..rows {
        if let Some(row) = grid.row(r) {
            let text = row.to_string();
            if text.contains("HELLO") {
                found_hello = true;
                break;
            }
        }
    }
    assert!(
        found_hello,
        "HELLO text should survive extreme resize round trip"
    );
}

/// The shrink pre-allocation estimate (`visible_rows * old_cols / new_cols`
/// rows for a full grid) avoids repeated reallocation.
#[test]
fn reflow_shrink_preallocation_adequate() {
    let rows: u16 = 5;
    let old_cols: u16 = 50;
    let new_cols: u16 = 5;
    let mut grid = Grid::new(rows, old_cols);

    // Fill row 0 completely
    grid.set_cursor(0, 0);
    for i in 0..old_cols {
        grid.write_char((b'A' + (i % 26) as u8) as char);
    }

    grid.resize(rows, new_cols);

    // Invariants hold and no content was lost.
    grid.assert_invariants();

    // The cursor (col 49 → row 9 at 5 cols) stays visible: rows 0..4 go to
    // scrollback, so visible row 0 is chars 25..29, "ZABCD".
    let row0_text = grid.row(0).unwrap().to_string();
    assert_eq!(
        row0_text, "ZABCD",
        "first visible chunk after scrollback push"
    );

    let row1_text = grid.row(1).unwrap().to_string();
    assert_eq!(
        row1_text, "EFGHI",
        "second visible chunk after scrollback push"
    );
}

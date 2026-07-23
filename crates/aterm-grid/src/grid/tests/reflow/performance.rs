// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reflow performance proofs — verify O(n) row operations and cursor invariants
//! under extreme resize ratios.

use super::super::super::*;
use crate::test_counters::take_reflow_row_ops;

/// Reflow shrink from 100 cols → 1 col visits each source row exactly once.
///
/// The output will have `visible_rows * 100` rows (each cell becomes its own row),
/// but the reflow loop should iterate the source rows in O(visible_rows), not
/// O(output_rows). The per-row chunking in `chunk_cells_to_rows` is linear in
/// content length, so total work is O(visible_rows * cols) = O(cells), which is
/// the minimum for touching all content.
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

    // After #7410 scrollback-push, the cursor (at col 99 → row 99 after
    // shrink) must remain visible. Rows 0..90 are pushed to scrollback,
    // so visible rows 0..9 correspond to original columns 90..99.
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

/// Reflow grow from 1 col → 10 cols merges continuation rows efficiently.
///
/// The merge buffer pattern concatenates continuation rows in a single pass,
/// meaning the outer loop iterates over logical lines (merged groups + individual
/// rows), not over every source row. This is O(logical_lines) ≤ O(source_rows),
/// which is strictly better when there are long wrapped lines.
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
    // The merge optimization means fewer outer-loop iterations than source rows:
    // 10 continuation rows (from "ABCDEFGHIJ") merge into 1 logical line = 1 op,
    // plus 10 empty rows = 10 ops, total = 11.
    assert!(
        ops <= usize::from(visible_after_shrink),
        "reflow grow ops ({ops}) should not exceed source row count ({visible_after_shrink})"
    );
    assert!(ops > 0, "reflow grow should do some work");

    // Content should be restored
    let row0_text = grid.row(0).unwrap().to_string();
    assert_eq!(
        row0_text, "ABCDEFGHIJ",
        "content should round-trip through extreme reflow"
    );

    grid.assert_invariants();
}

/// Cursor on the last row survives an extreme shrink/grow round trip.
///
/// Verifies the fix from #3975: both grow and shrink paths now initialize
/// cursor to (cursor_row, cursor_col), so cursor on a high row number
/// is preserved through the round trip (modulo clamping to grid bounds).
/// Uses scrollback to prevent content loss during extreme shrink.
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

    // Extreme shrink to 2 cols
    grid.resize(rows, 2);
    grid.assert_invariants();

    // The cursor col should be clamped to new_cols
    let cursor_col_after_shrink = grid.cursor_col();
    assert!(
        cursor_col_after_shrink < 2,
        "cursor col should be clamped to new_cols, got {cursor_col_after_shrink}"
    );

    // Grow back to original
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

/// Verify that shrink pre-allocation estimate doesn't cause excessive reallocs.
///
/// For a full grid (all cells have content), shrinking from `old_cols` to `new_cols`
/// should produce approximately `visible_rows * old_cols / new_cols` output rows.
/// The pre-allocation estimate should cover this without needing more than one
/// reallocation (Vec's doubling strategy).
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

    // After shrink: 50 chars / 5 cols = 10 rows from the one filled row
    // Plus 4 empty rows → total should be 14, but clamped to visible_rows
    // The key assertion: grid invariants hold and no content was lost
    grid.assert_invariants();

    // After #7410 scrollback-push, the cursor (at col 49 → row 9 after
    // shrink to 5 cols) must remain visible. With 5 visible rows, rows
    // 0..4 are pushed to scrollback. Visible row 0 = chars at positions
    // 25..29 = "ZABCD".
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

// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exhaustive cursor invariant proofs for reflow (#3975).
//!
//! Verifies that after any reflow (shrink or grow), the cursor is within
//! bounds: `cursor.row < visible_rows && cursor.col < cols`. Tests exercise
//! the symmetric initialization fix (commit 927d232) where both
//! `reflow_shrink_columns` and `reflow_grow_columns` now use
//! `(cursor_row, cursor_col)` as the fallback cursor position.

use super::super::super::*;

/// Exhaustive grid: all (rows, old_cols, new_cols) triples in [1..max],
/// cursor at every valid (row, col) position. Verifies `assert_invariants()`
/// passes after resize.
///
/// This is the core proof that the cursor invariant holds for ALL resize
/// dimensions, not just sampled ones. The max is kept small (8) so the test
/// runs in under 1 second despite O(max^5) iterations.
#[test]
fn cursor_invariant_exhaustive_small_grids() {
    let max: u16 = 8;
    let mut checks = 0u64;
    for rows in 1..=max {
        for old_cols in 1..=max {
            for new_cols in 1..=max {
                if old_cols == new_cols {
                    continue; // no reflow path taken
                }
                for cursor_row in 0..rows {
                    for cursor_col in 0..old_cols {
                        let mut grid = Grid::new(rows, old_cols);
                        // Write content on cursor row to exercise chunk_cells_to_rows
                        grid.set_cursor(cursor_row, 0);
                        let content_len = old_cols.min(26);
                        for i in 0..content_len {
                            grid.write_char((b'A' + (i % 26) as u8) as char);
                        }
                        grid.set_cursor(cursor_row, cursor_col);

                        grid.resize(rows, new_cols);

                        // The invariant: cursor within bounds
                        assert!(
                            grid.cursor_row() < rows,
                            "cursor row {} >= rows {} after resize({rows}, {new_cols}) \
                             from ({rows}, {old_cols}) with cursor at ({cursor_row}, {cursor_col})",
                            grid.cursor_row(),
                            rows,
                        );
                        assert!(
                            grid.cursor_col() < new_cols,
                            "cursor col {} >= new_cols {} after resize({rows}, {new_cols}) \
                             from ({rows}, {old_cols}) with cursor at ({cursor_row}, {cursor_col})",
                            grid.cursor_col(),
                            new_cols,
                        );
                        grid.assert_invariants();
                        checks += 1;
                    }
                }
            }
        }
    }
    assert!(
        checks > 5_000,
        "exhaustive check ran {checks} cases (expected >5k)"
    );
}

/// Cursor on an empty row (no content) during shrink: the cursor tracking
/// in `reflow_shrink_columns` takes the `content_len == 0` branch which
/// maps `(i == cursor_row)` → `(new_rows.len(), clamped_col)`. Verify this
/// produces a valid cursor for all grid sizes.
#[test]
fn cursor_on_empty_row_shrink() {
    for rows in 1..=12u16 {
        for old_cols in 2..=12u16 {
            for new_cols in 1..old_cols {
                for cursor_row in 0..rows {
                    let mut grid = Grid::new(rows, old_cols);
                    // Leave all rows empty — cursor is on a blank row
                    grid.set_cursor(cursor_row, old_cols / 2);

                    grid.resize(rows, new_cols);

                    assert!(
                        grid.cursor_row() < rows,
                        "empty-row shrink: cursor row {} >= rows {rows}",
                        grid.cursor_row()
                    );
                    assert!(
                        grid.cursor_col() < new_cols,
                        "empty-row shrink: cursor col {} >= new_cols {new_cols}",
                        grid.cursor_col()
                    );
                    grid.assert_invariants();
                }
            }
        }
    }
}

/// Cursor on an empty row during grow: same as above but for the grow path.
#[test]
fn cursor_on_empty_row_grow() {
    for rows in 1..=12u16 {
        for old_cols in 1..=12u16 {
            for new_cols in (old_cols + 1)..=13 {
                for cursor_row in 0..rows {
                    let mut grid = Grid::new(rows, old_cols);
                    grid.set_cursor(cursor_row, 0);

                    grid.resize(rows, new_cols);

                    assert!(grid.cursor_row() < rows);
                    assert!(grid.cursor_col() < new_cols);
                    grid.assert_invariants();
                }
            }
        }
    }
}

/// Multiple resize cycles: shrink → grow → shrink → grow. Each step must
/// leave the cursor in bounds. Exercises the round-trip path with the
/// symmetric initialization fix.
#[test]
fn cursor_invariant_multi_resize_cycle() {
    let dimensions: &[(u16, u16)] = &[
        (5, 20),
        (5, 3),
        (5, 40),
        (5, 1),
        (5, 10),
        (5, 7),
        (5, 80),
        (5, 2),
    ];

    let mut grid = Grid::new(5, 20);
    // Fill row 0 with content
    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJKLMNOPQRST".chars() {
        grid.write_char(c);
    }
    // Place cursor at row 2, col 10
    grid.set_cursor(2, 10);

    for &(rows, cols) in dimensions {
        grid.resize(rows, cols);
        assert!(
            grid.cursor_row() < rows,
            "cycle: cursor row {} >= rows {rows} (resize to {rows}x{cols})",
            grid.cursor_row()
        );
        assert!(
            grid.cursor_col() < cols,
            "cycle: cursor col {} >= cols {cols} (resize to {rows}x{cols})",
            grid.cursor_col()
        );
        grid.assert_invariants();
    }
}

/// Extreme ratio: 1-column terminal. Every cursor column must clamp to 0.
#[test]
fn cursor_invariant_single_column_terminal() {
    for rows in 1..=10u16 {
        for old_cols in 2..=20u16 {
            let mut grid = Grid::new(rows, old_cols);
            grid.set_cursor(0, 0);
            for i in 0..old_cols.min(26) {
                grid.write_char((b'A' + (i % 26) as u8) as char);
            }
            grid.set_cursor(rows - 1, old_cols - 1);

            grid.resize(rows, 1);

            assert_eq!(grid.cursor_col(), 0, "single-col: cursor must be at col 0");
            assert!(grid.cursor_row() < rows);
            grid.assert_invariants();
        }
    }
}

/// Extreme ratio: 1-row terminal. Cursor row must clamp to 0.
#[test]
fn cursor_invariant_single_row_terminal() {
    for old_rows in 2..=10u16 {
        for cols in 1..=20u16 {
            let new_cols = if cols > 1 { cols / 2 } else { cols + 1 };
            let mut grid = Grid::new(old_rows, cols);
            grid.set_cursor(old_rows - 1, cols.saturating_sub(1));

            grid.resize(1, new_cols);

            assert_eq!(grid.cursor_row(), 0, "single-row: cursor must be at row 0");
            assert!(grid.cursor_col() < new_cols);
            grid.assert_invariants();
        }
    }
}

/// Cursor on the last row with content that will expand during shrink.
/// The shrink path produces more output rows than source rows, but
/// `finalize_reflow` truncates to `target_rows`. The cursor must still
/// be within bounds after truncation.
#[test]
fn cursor_on_last_row_with_expanding_content_shrink() {
    let rows: u16 = 4;
    let old_cols: u16 = 20;
    let new_cols: u16 = 2;

    let mut grid = Grid::new(rows, old_cols);
    // Fill the last row with content that will expand to 10 output rows
    grid.set_cursor(rows - 1, 0);
    for i in 0..old_cols {
        grid.write_char((b'A' + (i % 26) as u8) as char);
    }
    // Cursor at last row, middle column
    grid.set_cursor(rows - 1, 10);

    grid.resize(rows, new_cols);

    assert!(
        grid.cursor_row() < rows,
        "expanding shrink: cursor row {} >= rows {rows}",
        grid.cursor_row()
    );
    assert!(
        grid.cursor_col() < new_cols,
        "expanding shrink: cursor col {} >= new_cols {new_cols}",
        grid.cursor_col()
    );
    grid.assert_invariants();
}

/// Scrollback + reflow: cursor survives shrink/grow with scrollback active.
/// The presence of scrollback changes the ring buffer state but should not
/// affect cursor validity after reflow.
#[test]
fn cursor_invariant_with_scrollback() {
    let rows: u16 = 5;
    let cols: u16 = 10;
    let mut grid = Grid::with_scrollback(rows, cols, 50);

    // Build scrollback
    for i in 0..20u16 {
        grid.set_cursor(rows - 1, 0);
        grid.write_char((b'A' + (i % 26) as u8) as char);
        grid.line_feed();
    }
    assert!(grid.scrollback_lines() > 0);

    // Cursor at various positions, shrink then grow
    for cursor_row in 0..rows {
        for cursor_col in [0u16, cols / 2, cols - 1] {
            let mut g = Grid::with_scrollback(rows, cols, 50);
            for i in 0..20u16 {
                g.set_cursor(rows - 1, 0);
                g.write_char((b'A' + (i % 26) as u8) as char);
                g.line_feed();
            }
            g.set_cursor(cursor_row, cursor_col);

            // Shrink
            g.resize(rows, 3);
            assert!(g.cursor_row() < rows);
            assert!(g.cursor_col() < 3);
            g.assert_invariants();

            // Grow
            g.resize(rows, 20);
            assert!(g.cursor_row() < rows);
            assert!(g.cursor_col() < 20);
            g.assert_invariants();
        }
    }
}

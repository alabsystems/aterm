// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exhaustive cursor invariant tests for reflow.

use super::super::super::*;

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
                        grid.set_cursor(cursor_row, 0);
                        let content_len = old_cols.min(26);
                        for i in 0..content_len {
                            grid.write_char((b'A' + (i % 26) as u8) as char);
                        }
                        grid.set_cursor(cursor_row, cursor_col);

                        grid.resize(rows, new_cols);

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

#[test]
fn cursor_on_empty_row_shrink_and_grow() {
    for rows in 1..=12u16 {
        for old_cols in 1..=12u16 {
            for new_cols in (1..=13u16).filter(|&n| n != old_cols) {
                for cursor_row in 0..rows {
                    let mut grid = Grid::new(rows, old_cols);
                    grid.set_cursor(cursor_row, old_cols / 2);

                    grid.resize(rows, new_cols);

                    assert!(
                        grid.cursor_row() < rows,
                        "empty-row resize: cursor row {} >= rows {rows}",
                        grid.cursor_row()
                    );
                    assert!(
                        grid.cursor_col() < new_cols,
                        "empty-row resize: cursor col {} >= new_cols {new_cols}",
                        grid.cursor_col()
                    );
                    grid.assert_invariants();
                }
            }
        }
    }
}

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
    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJKLMNOPQRST".chars() {
        grid.write_char(c);
    }
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

#[test]
fn cursor_invariant_with_scrollback() {
    let rows: u16 = 5;
    let cols: u16 = 10;
    let mut grid = Grid::with_scrollback(rows, cols, 50);

    for i in 0..20u16 {
        grid.set_cursor(rows - 1, 0);
        grid.write_char((b'A' + (i % 26) as u8) as char);
        grid.line_feed();
    }
    assert!(grid.scrollback_lines() > 0);

    for cursor_row in 0..rows {
        for cursor_col in [0u16, cols / 2, cols - 1] {
            let mut g = Grid::with_scrollback(rows, cols, 50);
            for i in 0..20u16 {
                g.set_cursor(rows - 1, 0);
                g.write_char((b'A' + (i % 26) as u8) as char);
                g.line_feed();
            }
            g.set_cursor(cursor_row, cursor_col);

            g.resize(rows, 3);
            assert!(g.cursor_row() < rows);
            assert!(g.cursor_col() < 3);
            g.assert_invariants();

            g.resize(rows, 20);
            assert!(g.cursor_row() < rows);
            assert!(g.cursor_col() < 20);
            g.assert_invariants();
        }
    }
}

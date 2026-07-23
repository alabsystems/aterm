// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Complexity and regression-focused grid tests extracted from style_perf.rs.
//!
//! Migrated from aterm-core as part of #6556 Batch 2.

use super::super::*;

// =============================================================================
// Performance Tests (O(n * cols) verification)
// =============================================================================

/// Verify scroll_up defers row_to_line conversion (lazy scrollback promotion).
///
/// With lazy promotion, `scroll_up` captures cells into `DeferredLine` via O(1)
/// memcpy instead of performing O(cols) `row_to_line` conversion. The conversion
/// happens later when scrollback is actually read. This test verifies:
/// 1. Zero `row_to_line` ops at scroll time (deferral works)
/// 2. Materialization ops scale linearly when scrollback is drained
#[test]
fn scroll_up_linear_time() {
    let rows = 100;
    let cols = 200;

    fn measure_scroll_and_drain(rows: u16, cols: u16, scroll_lines: usize) -> (usize, usize) {
        take_row_to_line_ops();

        let mut grid = Grid::with_scrollback(rows, cols, 0);

        for row in 0..rows {
            grid.set_cursor(row, 0);
            for col in 0..cols {
                grid.write_char(if (row + col) % 26 == 0 { 'A' } else { 'x' });
            }
        }

        let scrollback = Scrollback::new(100, 1000, 10_000_000);
        grid.attach_scrollback(scrollback);

        take_row_to_line_ops();

        grid.scroll_up(scroll_lines);
        let scroll_ops = take_row_to_line_ops();

        // Drain lazy buffer to trigger materialization.
        grid.drain_lazy_buffer();
        let drain_ops = take_row_to_line_ops();

        (scroll_ops, drain_ops)
    }

    let (small_scroll, small_drain) = measure_scroll_and_drain(rows, cols, 10);
    let (large_scroll, large_drain) = measure_scroll_and_drain(rows, cols, 50);

    // Lazy promotion: zero ops at scroll time.
    assert_eq!(
        small_scroll, 0,
        "scroll_up(10) should defer row_to_line (lazy promotion), got {small_scroll} ops"
    );
    assert_eq!(
        large_scroll, 0,
        "scroll_up(50) should defer row_to_line (lazy promotion), got {large_scroll} ops"
    );

    // Materialization ops scale linearly.
    assert!(
        small_drain > 0,
        "drain after scroll_up(10) should perform row_to_line ops, got 0"
    );

    let ratio = large_drain as f64 / small_drain as f64;
    assert!(
        ratio < 10.0,
        "drain ops ratio {ratio:.1}x suggests non-linear behavior (10 lines: {small_drain}, 50 lines: {large_drain})"
    );
    assert!(
        ratio > 2.0,
        "drain ops ratio {ratio:.1}x is too low - expected ~5x (10 lines: {small_drain}, 50 lines: {large_drain})"
    );
}

/// Verify reflow scales linearly with the number of rows.
#[test]
fn reflow_linear_time() {
    fn build_wrapped_grid(rows: u16, cols: u16) -> Grid {
        let mut grid = Grid::new(rows, cols);
        for row in 0..rows {
            if let Some(line) = grid.row_mut(row) {
                for col in 0..cols {
                    let ch = if (row + col) % 26 == 0 { 'A' } else { 'x' };
                    line.write_char(col, ch);
                }
                if row + 1 < rows {
                    line.set_wrapped(true);
                }
            }
        }
        grid
    }

    fn measure_reflow_ops(rows: u16, cols: u16, new_cols: u16) -> usize {
        take_reflow_row_ops();

        let mut grid = build_wrapped_grid(rows, cols);

        take_reflow_row_ops();

        grid.resize(rows, new_cols);
        take_reflow_row_ops()
    }

    let small_ops = measure_reflow_ops(80, 120, 60);
    let large_ops = measure_reflow_ops(400, 120, 60);

    assert!(
        small_ops > 0,
        "reflow(80 rows) should perform row ops, got 0"
    );

    let ratio = large_ops as f64 / small_ops as f64;
    assert!(
        ratio < 10.0,
        "reflow ops ratio {ratio:.1}x suggests non-linear behavior (80 rows: {small_ops}, 400 rows: {large_ops})"
    );
    assert!(
        ratio > 2.0,
        "reflow ops ratio {ratio:.1}x is too low - expected ~5x (80 rows: {small_ops}, 400 rows: {large_ops})"
    );
}

/// Verify reflow scales linearly with column count.
#[test]
fn reflow_linear_time_columns() {
    fn build_filled_grid(rows: u16, cols: u16) -> Grid {
        let mut grid = Grid::new(rows, cols);
        for row in 0..rows {
            if let Some(line) = grid.row_mut(row) {
                for col in 0..cols {
                    let ch = if (row + col) % 26 == 0 { 'A' } else { 'x' };
                    line.write_char(col, ch);
                }
            }
        }
        grid
    }

    fn measure_reflow_cells(rows: u16, cols: u16, new_cols: u16) -> usize {
        let mut grid = build_filled_grid(rows, cols);
        take_reflow_cell_ops();
        grid.resize(rows, new_cols);
        take_reflow_cell_ops()
    }

    let rows = 100;

    let small_ops = measure_reflow_cells(rows, 80, 40);
    let large_ops = measure_reflow_cells(rows, 320, 160);

    assert!(
        small_ops > 0,
        "expected nonzero cell ops for shrink (80→40)"
    );
    let ratio = large_ops as f64 / small_ops as f64;
    assert!(
        ratio < 6.0,
        "reflow shrink cell-ops ratio {ratio:.1}x suggests non-linear column scaling \
         (80 cols: {small_ops}, 320 cols: {large_ops})",
    );

    let grow_small = measure_reflow_cells(rows, 40, 80);
    let grow_large = measure_reflow_cells(rows, 160, 320);

    assert!(grow_small > 0, "expected nonzero cell ops for grow (40→80)");
    let ratio = grow_large as f64 / grow_small as f64;
    assert!(
        ratio < 6.0,
        "reflow grow cell-ops ratio {ratio:.1}x suggests non-linear column scaling \
         (40→80: {grow_small}, 160→320: {grow_large})",
    );
}

/// Verify many scroll_up(1) calls defer row_to_line (lazy scrollback promotion).
///
/// With lazy promotion, repeated `scroll_up(1)` calls accumulate `DeferredLine`
/// entries in the lazy buffer without performing `row_to_line` conversion.
/// Materialization ops scale linearly when the buffer is drained.
#[test]
fn scroll_up_handles_many_rows() {
    fn measure_scroll_and_drain(scrolls: usize) -> (usize, usize) {
        take_row_to_line_ops();

        let rows = 50;
        let cols = 80;
        let mut grid = Grid::with_scrollback(rows, cols, 0);

        for row in 0..rows {
            grid.set_cursor(row, 0);
            for c in "Test content for row".chars() {
                grid.write_char(c);
            }
        }

        let scrollback = Scrollback::new(200, 1000, 10_000_000);
        grid.attach_scrollback(scrollback);

        take_row_to_line_ops();

        for _ in 0..scrolls {
            grid.scroll_up(1);
        }
        let scroll_ops = take_row_to_line_ops();

        // Drain lazy buffer to trigger materialization.
        grid.drain_lazy_buffer();
        let drain_ops = take_row_to_line_ops();

        (scroll_ops, drain_ops)
    }

    let (small_scroll, small_drain) = measure_scroll_and_drain(25);
    let (large_scroll, large_drain) = measure_scroll_and_drain(125);

    // Lazy promotion: zero ops at scroll time.
    assert_eq!(
        small_scroll, 0,
        "scroll_up(1) x 25 should defer row_to_line (lazy promotion), got {small_scroll} ops"
    );
    assert_eq!(
        large_scroll, 0,
        "scroll_up(1) x 125 should defer row_to_line (lazy promotion), got {large_scroll} ops"
    );

    // Materialization ops scale linearly.
    assert!(
        small_drain > 0,
        "drain after 25 scrolls should perform row_to_line ops, got 0"
    );

    let ratio = large_drain as f64 / small_drain as f64;
    assert!(
        ratio < 10.0,
        "drain ops ratio {ratio:.1}x suggests non-linear behavior (25 scrolls: {small_drain}, 125 scrolls: {large_drain})"
    );
    assert!(
        ratio > 2.0,
        "drain ops ratio {ratio:.1}x is too low - expected ~5x (25 scrolls: {small_drain}, 125 scrolls: {large_drain})"
    );
}

/// Verify row_to_line conversion is O(cols).
#[test]
fn row_to_line_linear_in_columns() {
    fn measure_row_to_line_cells(cols: u16) -> usize {
        take_row_to_line_cells();

        let mut grid = Grid::new(1, cols);

        for col in 0..cols {
            grid.set_cursor(0, col);
            grid.write_char(if col % 2 == 0 { 'A' } else { 'B' });
        }

        take_row_to_line_cells();

        let row = grid.row(0).unwrap();

        let _line = Grid::row_to_line_static(row);
        take_row_to_line_cells()
    }

    let small_cells = measure_row_to_line_cells(40);
    let large_cells = measure_row_to_line_cells(200);

    assert!(
        small_cells >= 40,
        "row_to_line(40 cols) should process at least 40 cells, got {small_cells}"
    );
    assert!(
        large_cells >= 200,
        "row_to_line(200 cols) should process at least 200 cells, got {large_cells}"
    );

    let ratio = large_cells as f64 / small_cells as f64;
    assert!(
        ratio < 7.0,
        "row_to_line cells ratio {ratio:.1}x suggests non-linear behavior (40 cols: {small_cells}, 200 cols: {large_cells})"
    );
    assert!(
        ratio > 3.0,
        "row_to_line cells ratio {ratio:.1}x is too low - expected ~5x (40 cols: {small_cells}, 200 cols: {large_cells})"
    );
}

/// Verify single scroll_up(1) defers conversion and is O(1) with respect to grid size.
///
/// With lazy promotion, `scroll_up(1)` captures a single `DeferredLine` regardless
/// of grid size — the conversion cost is zero at scroll time. When drained, the
/// materialization cost is O(cols), independent of total grid rows.
#[test]
fn scroll_single_line_constant_time() {
    let cols = 80;

    fn measure_single_scroll_and_drain(rows: u16, cols: u16) -> (usize, usize) {
        take_row_to_line_ops();

        let mut grid = Grid::with_scrollback(rows, cols, 0);

        for row in 0..rows {
            grid.set_cursor(row, 0);
            for col in 0..cols {
                grid.write_char(if (row + col) % 26 == 0 { 'A' } else { 'x' });
            }
        }

        let scrollback = Scrollback::new(100, 1000, 10_000_000);
        grid.attach_scrollback(scrollback);

        take_row_to_line_ops();

        grid.scroll_up(1);
        let scroll_ops = take_row_to_line_ops();

        // Drain lazy buffer to trigger materialization.
        grid.drain_lazy_buffer();
        let drain_ops = take_row_to_line_ops();

        (scroll_ops, drain_ops)
    }

    let (small_scroll, small_drain) = measure_single_scroll_and_drain(100, cols);
    let (large_scroll, large_drain) = measure_single_scroll_and_drain(1000, cols);

    // Lazy promotion: zero ops at scroll time regardless of grid size.
    assert_eq!(
        small_scroll, 0,
        "scroll_up(1) on 100-row grid should defer row_to_line, got {small_scroll} ops"
    );
    assert_eq!(
        large_scroll, 0,
        "scroll_up(1) on 1000-row grid should defer row_to_line, got {large_scroll} ops"
    );

    // Materialization should happen on drain.
    assert!(
        small_drain > 0,
        "drain after scroll_up(1) on 100-row grid should perform row_to_line ops, got 0"
    );
    assert!(
        large_drain > 0,
        "drain after scroll_up(1) on 1000-row grid should perform row_to_line ops, got 0"
    );

    // Single-line materialization should be O(1) w.r.t. grid size — same cost
    // regardless of whether grid has 100 or 1000 rows.
    assert!(
        large_drain <= small_drain * 2,
        "Single-line drain should be O(1): 100-row grid {small_drain} ops, 1000-row grid {large_drain} ops (10x grid size should not increase ops)"
    );
}

/// Verify row_to_line_with_hyperlinks extracts hyperlink spans correctly.
#[test]
fn row_to_line_extracts_hyperlinks() {
    use std::sync::Arc;

    let mut grid = Grid::new(1, 20);
    grid.set_cursor(0, 0);

    for c in "Hello World Test123".chars() {
        grid.write_char(c);
    }

    let mut extras = CellExtras::new();
    let hello_url: Arc<str> = Arc::from("https://hello.com");
    let world_url: Arc<str> = Arc::from("https://world.com");

    for col in 0..5 {
        let mut extra = CellExtra::default();
        extra.set_hyperlink(Some(hello_url.clone()));
        extras.set(CellCoord::new(0, col), extra);
    }

    for col in 6..11 {
        let mut extra = CellExtra::default();
        extra.set_hyperlink(Some(world_url.clone()));
        extras.set(CellCoord::new(0, col), extra);
    }

    let row = grid.row(0).unwrap();
    let line = Grid::row_to_line_with_hyperlinks(row, &extras, 0, grid.styles());

    assert_eq!(line.to_string(), "Hello World Test123");

    assert!(line.has_hyperlinks(), "Line should have hyperlinks");
    assert_eq!(line.hyperlink_count(), 2, "Should have 2 hyperlink spans");

    assert_eq!(
        line.get_hyperlink(0).map(|h| h.as_ref()),
        Some("https://hello.com"),
        "Col 0 should link to hello.com"
    );
    assert_eq!(
        line.get_hyperlink(4).map(|h| h.as_ref()),
        Some("https://hello.com"),
        "Col 4 should link to hello.com"
    );

    assert!(
        line.get_hyperlink(5).is_none(),
        "Col 5 should not have hyperlink"
    );

    assert_eq!(
        line.get_hyperlink(6).map(|h| h.as_ref()),
        Some("https://world.com"),
        "Col 6 should link to world.com"
    );
    assert_eq!(
        line.get_hyperlink(10).map(|h| h.as_ref()),
        Some("https://world.com"),
        "Col 10 should link to world.com"
    );

    assert!(
        line.get_hyperlink(11).is_none(),
        "Col 11 should not have hyperlink"
    );
}

/// Verify row_to_line_with_hyperlinks handles empty rows without hyperlinks.
#[test]
fn row_to_line_no_hyperlinks() {
    let mut grid = Grid::new(1, 10);
    grid.set_cursor(0, 0);
    for c in "NoLinks!!!".chars() {
        grid.write_char(c);
    }

    let extras = CellExtras::new();

    let row = grid.row(0).unwrap();
    let line = Grid::row_to_line_with_hyperlinks(row, &extras, 0, grid.styles());

    assert_eq!(line.to_string(), "NoLinks!!!");

    assert!(!line.has_hyperlinks(), "Line should not have hyperlinks");
    assert_eq!(line.hyperlink_count(), 0, "Should have 0 hyperlink spans");
}

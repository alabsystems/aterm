// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Complexity tests for scroll-off and lazy scrollback promotion.

use super::super::*;

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

        grid.drain_lazy_buffer();
        let drain_ops = take_row_to_line_ops();

        (scroll_ops, drain_ops)
    }

    let (small_scroll, small_drain) = measure_scroll_and_drain(rows, cols, 10);
    let (large_scroll, large_drain) = measure_scroll_and_drain(rows, cols, 50);

    assert_eq!(
        small_scroll, 0,
        "scroll_up(10) should defer row_to_line (lazy promotion), got {small_scroll} ops"
    );
    assert_eq!(
        large_scroll, 0,
        "scroll_up(50) should defer row_to_line (lazy promotion), got {large_scroll} ops"
    );
    assert_eq!(small_drain, 10);
    assert_eq!(large_drain, 50);
}

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

    assert_eq!(measure_reflow_ops(80, 120, 60), 80);
    assert_eq!(measure_reflow_ops(400, 120, 60), 400);
}

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

    assert_eq!(measure_reflow_cells(rows, 80, 40), 8_000);
    assert_eq!(measure_reflow_cells(rows, 320, 160), 32_000);
    assert_eq!(measure_reflow_cells(rows, 40, 80), 4_000);
    assert_eq!(measure_reflow_cells(rows, 160, 320), 16_000);
}

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

        grid.drain_lazy_buffer();
        let drain_ops = take_row_to_line_ops();

        (scroll_ops, drain_ops)
    }

    let (small_scroll, small_drain) = measure_single_scroll_and_drain(100, cols);
    let (large_scroll, large_drain) = measure_single_scroll_and_drain(1000, cols);

    assert_eq!(
        small_scroll, 0,
        "scroll_up(1) on 100-row grid should defer row_to_line, got {small_scroll} ops"
    );
    assert_eq!(
        large_scroll, 0,
        "scroll_up(1) on 1000-row grid should defer row_to_line, got {large_scroll} ops"
    );
    assert_eq!(small_drain, 1);
    assert_eq!(large_drain, 1);
}

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
    let stored = Grid::extract_row_extras(row, &extras, 0);
    let line = Grid::row_to_line_with_stored_extras(row, &stored);

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

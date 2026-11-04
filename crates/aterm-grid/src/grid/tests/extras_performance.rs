// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! CellExtras performance/complexity tests.

use super::*;

#[test]
fn erase_to_end_of_screen_extras_clear_is_batch_o_e() {
    fn measure_erase_to_end_ops(extras_count: usize, cursor_row: u16) -> usize {
        let rows = 100u16;
        let cols = 80u16;
        let mut grid = Grid::with_scrollback(rows, cols, 0);

        for row in 0..rows {
            grid.set_cursor(row, 0);
            for _ in 0..cols {
                grid.write_char('x');
            }
        }

        for i in 0..extras_count {
            let row = (i % rows as usize) as u16;
            let col = (i / rows as usize) as u16;
            if col < cols {
                grid.extras_mut()
                    .get_or_create(CellCoord::new(row, col))
                    .add_combining('\u{0301}');
            }
        }

        grid.set_cursor(cursor_row.min(rows.saturating_sub(1)), 0);
        take_extras_clear_ops(); // clear
        grid.erase_to_end_of_screen();
        take_extras_clear_ops()
    }

    // `erase_to_end_of_screen` calls `clear_range` on `cursor_row` (1,000 entries, removing 10)
    // followed by one `clear_rows(cursor_row + 1 .. 100)` pass (990 remaining entries), so
    // clearing 9 rows and clearing 89 rows both execute 1,990 ops; on the bottom row (99),
    // `clear_rows(100..100)` is empty and only `clear_range` runs (1,000 ops).
    assert_eq!(measure_erase_to_end_ops(1_000, 90), 1_990);
    assert_eq!(measure_erase_to_end_ops(1_000, 10), 1_990);
    assert_eq!(measure_erase_to_end_ops(1_000, 99), 1_000);
}

#[test]
fn erase_rect_extras_clear_is_batch_o_e() {
    fn measure_erase_rect_ops(extras_count: usize, rect_height: u16) -> usize {
        let rows = 100u16;
        let cols = 80u16;
        let mut grid = Grid::with_scrollback(rows, cols, 0);

        for i in 0..extras_count {
            let row = (i % rows as usize) as u16;
            let col = (i / rows as usize) as u16;
            if col < cols {
                grid.extras_mut()
                    .get_or_create(CellCoord::new(row, col))
                    .add_combining('\u{0301}');
            }
        }

        let bottom = rect_height.saturating_sub(1).min(rows.saturating_sub(1));
        take_extras_clear_ops(); // clear
        grid.erase_rect(0, 0, bottom, cols.saturating_sub(1));
        take_extras_clear_ops()
    }

    // `erase_rect` executes one `clear_rect` pass over the 1,000-entry map regardless of height.
    assert_eq!(measure_erase_rect_ops(1_000, 5), 1_000);
    assert_eq!(measure_erase_rect_ops(1_000, 80), 1_000);
}

fn measure_shift(
    entries: usize,
    rows: usize,
    col0: u16,
    shift: impl FnOnce(&mut CellExtras),
) -> usize {
    let mut extras = CellExtras::new();
    for i in 0..entries {
        let row = (i % rows) as u16;
        let col = (i / rows) as u16 + col0;
        extras
            .get_or_create(CellCoord::new(row, col))
            .add_combining('\u{0301}');
    }
    take_extras_shift_ops(); // clear
    shift(&mut extras);
    take_extras_shift_ops()
}

fn assert_shift_ops_o_e(
    grown: &str,
    small_n: u16,
    large_n: u16,
    measure: impl Fn(usize, u16) -> usize,
) {
    assert_eq!(measure(500, small_n), 500, "{grown}: small_n={small_n}");
    assert_eq!(measure(500, large_n), 500, "{grown}: large_n={large_n}");
    assert_eq!(measure(1_000, small_n), 1_000, "{grown}: 2x entries");
}

#[test]
fn extras_shifts_are_o_e_not_o_region_or_count() {
    assert_shift_ops_o_e("up_by: 10x larger region", 10, 100, |entries, region| {
        measure_shift(entries, 200, 0, |e| {
            e.shift_region_up_by(0, region.saturating_sub(1), 1)
        })
    });
    assert_shift_ops_o_e("down_by: 10x larger region", 10, 100, |entries, region| {
        measure_shift(entries, 200, 0, |e| {
            e.shift_region_down_by(0, region.saturating_sub(1), 1)
        })
    });
    assert_shift_ops_o_e(
        "cols_right: 50x larger shift count",
        1,
        50,
        |entries, count| measure_shift(entries, 50, 10, |e| e.shift_cols_right(0, 5, count, 200)),
    );
    assert_shift_ops_o_e(
        "cols_left: 30x larger shift count",
        1,
        30,
        |entries, count| measure_shift(entries, 50, 50, |e| e.shift_cols_left(0, 10, count, 200)),
    );
}

#[test]
fn rapid_scroll_extras_cost_bounded() {
    let rows = 24u16;
    let cols = 80u16;
    let mut grid = Grid::with_scrollback(rows, cols, 0);

    for row in 0..rows {
        grid.set_cursor(row, 0);
        for _ in 0..cols {
            grid.write_char('x');
        }
    }

    for row in (0..rows).step_by(2) {
        let url: std::sync::Arc<str> = std::sync::Arc::from("https://test.com");
        grid.extras_mut()
            .get_or_create(CellCoord::new(row, 0))
            .set_hyperlink(Some(url));
    }

    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    grid.attach_scrollback(scrollback);

    take_extras_shift_ops(); // clear
    for _ in 0..100 {
        grid.scroll_up(1);
    }
    let ops_100_scrolls = take_extras_shift_ops();

    assert_eq!(
        ops_100_scrolls, 0,
        "100 scrolls should use O(1) offset amortization, got {ops_100_scrolls}"
    );
}

fn assert_reflow_row_ops_linear(old_cols: u16, new_cols: u16, ch: char) {
    let measure_reflow_rows = |num_rows: u16| {
        let mut grid = Grid::with_scrollback(num_rows, old_cols, 0);
        for row in 0..num_rows {
            grid.set_cursor(row, 0);
            for _ in 0..old_cols {
                grid.write_char(ch);
            }
        }
        take_reflow_row_ops(); // clear
        grid.resize(num_rows, new_cols);
        take_reflow_row_ops()
    };

    // Each source visible row is visited exactly once during width reflow (`reflow-cost`).
    assert_eq!(measure_reflow_rows(25), 25);
    assert_eq!(measure_reflow_rows(50), 50);
}

#[test]
fn reflow_grow_row_ops_linear_in_rows() {
    assert_reflow_row_ops_linear(40, 80, 'A');
}

#[test]
fn reflow_shrink_row_ops_linear_in_rows() {
    assert_reflow_row_ops_linear(80, 40, 'B');
}

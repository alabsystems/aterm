// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! CellExtras performance/complexity verification tests.
//!
//! Migrated from aterm-core as part of #6556 Batch 2.
//! The `take_response_preserves_capacity` test remains in aterm-core
//! because it depends on `crate::terminal::Terminal`.

use super::*;

// =============================================================================
// Performance Tests (complexity verification)
// =============================================================================

/// Verify `shift_rows_up_by` with start_row=0 is O(1) via offset (#4542).
#[test]
fn extras_shift_full_screen_is_o1_partial_is_o_e() {
    let mut extras = CellExtras::new();
    for i in 0..100 {
        let row = (i % 50) as u16 + 10;
        let col = (i / 50) as u16;
        extras
            .get_or_create(CellCoord::new(row, col))
            .add_combining('\u{0301}');
    }
    take_extras_shift_ops(); // clear

    // Full-screen shift: O(1) offset bump
    extras.shift_rows_up_by(0, 10);
    let full_screen_ops = take_extras_shift_ops();
    assert_eq!(
        full_screen_ops, 0,
        "full-screen shift should be O(1) via offset, got {full_screen_ops}"
    );

    // Region shift (start_row > 0): O(E) drain-rebuild (compacts first)
    extras.shift_rows_up_by(5, 1);
    let region_ops = take_extras_shift_ops();
    assert!(
        region_ops > 0,
        "region shift should visit entries (O(E) drain-rebuild)"
    );
}

/// Verify `shift_rows_up_by(0, n)` is O(1) regardless of E (#4542).
#[test]
fn shift_rows_up_by_o1_regardless_of_entry_count() {
    fn measure_shift_ops(entries: usize) -> usize {
        let mut extras = CellExtras::new();
        for i in 0..entries {
            let row = (i % 200) as u16;
            let col = (i / 200) as u16;
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }
        take_extras_shift_ops(); // clear
        extras.shift_rows_up_by(0, 5);
        take_extras_shift_ops()
    }

    let small = measure_shift_ops(500);
    let large = measure_shift_ops(2000);

    assert_eq!(small, 0, "full-screen shift should be O(1): got {small}");
    assert_eq!(large, 0, "full-screen shift should be O(1): got {large}");
}

/// Verify scroll_up extras shift is O(1) via row-offset amortization (#4542).
#[test]
fn scroll_up_extras_shift_is_o1() {
    let rows = 100u16;
    let cols = 80u16;
    let mut grid = Grid::with_scrollback(rows, cols, 0);

    for row in 0..rows {
        grid.set_cursor(row, 0);
        for _ in 0..cols {
            grid.write_char('x');
        }
    }

    for i in 0..50 {
        let row = (i % (rows as usize)) as u16;
        let col = (i / (rows as usize)) as u16;
        if col < cols {
            let url: std::sync::Arc<str> = std::sync::Arc::from("https://test.com");
            grid.extras_mut()
                .get_or_create(CellCoord::new(row, col))
                .set_hyperlink(Some(url));
        }
    }

    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    grid.attach_scrollback(scrollback);

    take_extras_shift_ops(); // clear
    grid.scroll_up(50);
    let ops = take_extras_shift_ops();
    assert_eq!(
        ops, 0,
        "scroll_up should use O(1) offset amortization, got {ops} shift ops"
    );
}

/// Verify `scroll_up` extras cost is O(1) regardless of E (#4542).
#[test]
fn scroll_up_extras_cost_o1_regardless_of_e() {
    fn measure_scroll_shift_ops(extras_count: usize) -> usize {
        let rows = 50u16;
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
                let url: std::sync::Arc<str> = std::sync::Arc::from("https://test.com");
                grid.extras_mut()
                    .get_or_create(CellCoord::new(row, col))
                    .set_hyperlink(Some(url));
            }
        }

        let scrollback = Scrollback::new(200, 1000, 10_000_000);
        grid.attach_scrollback(scrollback);

        take_extras_shift_ops(); // clear
        grid.scroll_up(10);
        take_extras_shift_ops()
    }

    let small = measure_scroll_shift_ops(100);
    let large = measure_scroll_shift_ops(400);

    assert_eq!(
        small, 0,
        "scroll_up with 100 extras should be O(1): got {small}"
    );
    assert_eq!(
        large, 0,
        "scroll_up with 400 extras should be O(1): got {large}"
    );
}

/// Verify batched row clears cost O(E), independent of rows cleared.
#[test]
fn extras_clear_rows_is_batch_o_e() {
    fn measure_clear_rows_ops(extras_count: usize, rows_cleared: u16) -> usize {
        let mut extras = CellExtras::new();
        for i in 0..extras_count {
            let row = (i % 100) as u16;
            let col = (i / 100) as u16;
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }

        take_extras_clear_ops(); // clear
        extras.clear_rows(0..rows_cleared);
        take_extras_clear_ops()
    }

    let base = measure_clear_rows_ops(1_000, 10);
    assert!(base > 0, "should register extras clear ops");

    let more_rows = measure_clear_rows_ops(1_000, 90);
    let rows_ratio = more_rows as f64 / base as f64;
    assert!(
        rows_ratio > 0.5 && rows_ratio < 1.5,
        "clearing more rows should not increase ops: base={base}, more_rows={more_rows}, ratio={rows_ratio:.2}"
    );

    let more_entries = measure_clear_rows_ops(2_000, 10);
    let entries_ratio = more_entries as f64 / base as f64;
    assert!(
        entries_ratio > 1.5 && entries_ratio < 3.0,
        "doubling entries should ~double ops: base={base}, more_entries={more_entries}, ratio={entries_ratio:.2}"
    );
}

/// Verify erase-to-end-of-screen extras clears are O(E), not O(rows*E).
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

    let clear_few_rows = measure_erase_to_end_ops(1_000, 90);
    let clear_many_rows = measure_erase_to_end_ops(1_000, 10);
    let ratio = clear_many_rows as f64 / clear_few_rows as f64;
    assert!(
        ratio < 3.0,
        "clearing many more rows should not multiply extras ops: few={clear_few_rows}, many={clear_many_rows}, ratio={ratio:.2}"
    );
}

/// Verify erase-rect extras clears are O(E), independent of rectangle height.
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

    let short_rect = measure_erase_rect_ops(1_000, 5);
    let tall_rect = measure_erase_rect_ops(1_000, 80);
    let ratio = tall_rect as f64 / short_rect as f64;
    assert!(
        ratio < 3.0,
        "taller rectangles should not multiply extras ops: short={short_rect}, tall={tall_rect}, ratio={ratio:.2}"
    );
}

/// `row_has_hyperlinks` returns correct results after mutations.
#[test]
fn row_has_hyperlinks_after_mutations() {
    use std::sync::Arc;
    let url: Arc<str> = Arc::from("https://test.com");

    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(5, 0))
        .set_hyperlink(Some(url.clone()));
    assert!(extras.row_has_hyperlinks(5));
    assert!(!extras.row_has_hyperlinks(0));

    extras
        .get_or_create(CellCoord::new(0, 0))
        .set_hyperlink(Some(url));
    assert!(extras.row_has_hyperlinks(0));
    assert!(extras.row_has_hyperlinks(5));
    assert!(!extras.row_has_hyperlinks(1));

    extras.clear_row(5);
    assert!(!extras.row_has_hyperlinks(5));
    assert!(extras.row_has_hyperlinks(0));
}

// =============================================================================
// Region shift complexity tests
// =============================================================================

/// Verify shift_region_up_by is O(E), independent of region size.
#[test]
fn shift_region_up_by_is_o_e_not_o_region() {
    fn measure_region_shift(entries: usize, region_size: u16) -> usize {
        let mut extras = CellExtras::new();
        for i in 0..entries {
            let row = (i % 200) as u16;
            let col = (i / 200) as u16;
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }
        take_extras_shift_ops(); // clear
        extras.shift_region_up_by(0, region_size.saturating_sub(1), 1);
        take_extras_shift_ops()
    }

    let small_region = measure_region_shift(500, 10);
    let large_region = measure_region_shift(500, 100);

    assert!(small_region > 0, "should register shift ops");
    let ratio = large_region as f64 / small_region as f64;
    assert!(
        ratio > 0.5 && ratio < 2.0,
        "10x larger region should NOT change ops (O(E)): small={small_region}, \
         large={large_region}, ratio={ratio:.2}"
    );

    let double_entries = measure_region_shift(1000, 10);
    let entry_ratio = double_entries as f64 / small_region as f64;
    assert!(
        entry_ratio > 1.5 && entry_ratio < 3.0,
        "2x entries should ~double ops: base={small_region}, double={double_entries}, \
         ratio={entry_ratio:.2}"
    );
}

/// Verify shift_region_down_by is O(E), independent of region size.
#[test]
fn shift_region_down_by_is_o_e_not_o_region() {
    fn measure_region_shift(entries: usize, region_size: u16) -> usize {
        let mut extras = CellExtras::new();
        for i in 0..entries {
            let row = (i % 200) as u16;
            let col = (i / 200) as u16;
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }
        take_extras_shift_ops(); // clear
        extras.shift_region_down_by(0, region_size.saturating_sub(1), 1);
        take_extras_shift_ops()
    }

    let small_region = measure_region_shift(500, 10);
    let large_region = measure_region_shift(500, 100);

    assert!(small_region > 0, "should register shift ops");
    let ratio = large_region as f64 / small_region as f64;
    assert!(
        ratio > 0.5 && ratio < 2.0,
        "10x larger region should NOT change ops (O(E)): small={small_region}, \
         large={large_region}, ratio={ratio:.2}"
    );

    let double_entries = measure_region_shift(1000, 10);
    let entry_ratio = double_entries as f64 / small_region as f64;
    assert!(
        entry_ratio > 1.5 && entry_ratio < 3.0,
        "2x entries should ~double ops: base={small_region}, double={double_entries}, \
         ratio={entry_ratio:.2}"
    );
}

/// Verify shift_cols_right is O(E), independent of shift count.
#[test]
fn shift_cols_right_is_o_e_not_o_count() {
    fn measure_col_shift(entries: usize, shift_count: u16) -> usize {
        let mut extras = CellExtras::new();
        for i in 0..entries {
            let row = (i % 50) as u16;
            let col = (i / 50) as u16 + 10;
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }
        take_extras_shift_ops(); // clear
        extras.shift_cols_right(0, 5, shift_count, 200);
        take_extras_shift_ops()
    }

    let small_shift = measure_col_shift(500, 1);
    let large_shift = measure_col_shift(500, 50);

    assert!(small_shift > 0, "should register shift ops");
    let ratio = large_shift as f64 / small_shift as f64;
    assert!(
        ratio > 0.5 && ratio < 2.0,
        "50x larger shift count should NOT change ops: small={small_shift}, \
         large={large_shift}, ratio={ratio:.2}"
    );

    let double_entries = measure_col_shift(1000, 1);
    let entry_ratio = double_entries as f64 / small_shift as f64;
    assert!(
        entry_ratio > 1.5 && entry_ratio < 3.0,
        "2x entries should ~double ops: base={small_shift}, double={double_entries}, \
         ratio={entry_ratio:.2}"
    );
}

/// Verify shift_cols_left is O(E), independent of shift count.
#[test]
fn shift_cols_left_is_o_e_not_o_count() {
    fn measure_col_shift(entries: usize, shift_count: u16) -> usize {
        let mut extras = CellExtras::new();
        for i in 0..entries {
            let row = (i % 50) as u16;
            let col = (i / 50) as u16 + 50;
            extras
                .get_or_create(CellCoord::new(row, col))
                .add_combining('\u{0301}');
        }
        take_extras_shift_ops(); // clear
        extras.shift_cols_left(0, 10, shift_count, 200);
        take_extras_shift_ops()
    }

    let small_shift = measure_col_shift(500, 1);
    let large_shift = measure_col_shift(500, 30);

    assert!(small_shift > 0, "should register shift ops");
    let ratio = large_shift as f64 / small_shift as f64;
    assert!(
        ratio > 0.5 && ratio < 2.0,
        "30x larger shift count should NOT change ops: small={small_shift}, \
         large={large_shift}, ratio={ratio:.2}"
    );

    let double_entries = measure_col_shift(1000, 1);
    let entry_ratio = double_entries as f64 / small_shift as f64;
    assert!(
        entry_ratio > 1.5 && entry_ratio < 3.0,
        "2x entries should ~double ops: base={small_shift}, double={double_entries}, \
         ratio={entry_ratio:.2}"
    );
}

// =============================================================================
// Rapid scroll allocation stress test
// =============================================================================

/// Verify that rapid sequential scrolls do not cause unbounded extras overhead.
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

// =============================================================================
// Reflow complexity verification
// =============================================================================

/// Verify reflow grow-columns processes O(rows) row operations.
#[test]
fn reflow_grow_row_ops_linear_in_rows() {
    fn measure_reflow_rows(num_rows: u16, old_cols: u16, new_cols: u16) -> usize {
        let mut grid = Grid::with_scrollback(num_rows, old_cols, 0);
        for row in 0..num_rows {
            grid.set_cursor(row, 0);
            for _ in 0..old_cols {
                grid.write_char('A');
            }
        }
        take_reflow_row_ops(); // clear
        grid.resize(num_rows, new_cols);
        take_reflow_row_ops()
    }

    let small = measure_reflow_rows(25, 40, 80);
    let large = measure_reflow_rows(50, 40, 80);

    assert!(small > 0, "should register reflow row ops");
    let ratio = large as f64 / small as f64;
    assert!(
        ratio > 1.5 && ratio < 3.0,
        "2x rows should ~2x reflow ops: small={small}, large={large}, ratio={ratio:.2}"
    );
}

/// Verify reflow shrink-columns processes O(rows) row operations.
#[test]
fn reflow_shrink_row_ops_linear_in_rows() {
    fn measure_reflow_rows(num_rows: u16, old_cols: u16, new_cols: u16) -> usize {
        let mut grid = Grid::with_scrollback(num_rows, old_cols, 0);
        for row in 0..num_rows {
            grid.set_cursor(row, 0);
            for _ in 0..old_cols {
                grid.write_char('B');
            }
        }
        take_reflow_row_ops(); // clear
        grid.resize(num_rows, new_cols);
        take_reflow_row_ops()
    }

    let small = measure_reflow_rows(25, 80, 40);
    let large = measure_reflow_rows(50, 80, 40);

    assert!(small > 0, "should register reflow row ops");
    let ratio = large as f64 / small as f64;
    assert!(
        ratio > 1.5 && ratio < 3.0,
        "2x rows should ~2x reflow ops: small={small}, large={large}, ratio={ratio:.2}"
    );
}

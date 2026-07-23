// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn write_line(grid: &mut Grid, text: &str) {
    grid.carriage_return();
    for ch in text.chars() {
        grid.write_char(ch);
    }
}

fn build_unscroll_fixture(rows: u16, cols: u16, line_count: usize) -> Grid {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 2, scrollback);
    for i in 0..line_count {
        write_line(&mut grid, &format!("Line{i}"));
        if i + 1 < line_count {
            grid.line_feed();
        }
    }
    grid
}

fn dirty_rows(grid: &Grid) -> Vec<u16> {
    grid.damage()
        .iter_bounds(grid.rows(), grid.cols())
        .map(|bound| bound.line)
        .collect()
}

#[test]
fn grid_unscroll_full_screen_marks_all_rows_dirty() {
    let mut grid = build_unscroll_fixture(4, 10, 8);
    assert!(
        grid.tiered_scrollback_lines() >= 1,
        "fixture should create scrollback before unscroll"
    );

    grid.clear_damage();
    let unscrolled = grid.unscroll_from_scrollback(1);

    assert_eq!(unscrolled, 1);
    assert_eq!(dirty_rows(&grid), vec![0, 1, 2, 3]);
}

#[test]
fn grid_unscroll_scroll_region_marks_only_region_rows_dirty() {
    let mut grid = build_unscroll_fixture(8, 20, 16);
    assert!(
        grid.tiered_scrollback_lines() >= 2,
        "fixture should create enough scrollback for a two-line unscroll"
    );

    grid.set_scroll_region(2, 5);
    grid.clear_damage();
    let unscrolled = grid.unscroll_from_scrollback(2);

    assert_eq!(unscrolled, 2);
    assert_eq!(dirty_rows(&grid), vec![2, 3, 4, 5]);
}

#[test]
fn grid_unscroll_clamps_region_scroll_delta_to_available_scrollback() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 10, 2, scrollback);
    write_line(&mut grid, "Line0");
    grid.line_feed();
    write_line(&mut grid, "Line1");
    grid.line_feed();
    for i in 2..6 {
        write_line(&mut grid, &format!("Line{i}"));
        grid.line_feed();
    }
    assert_eq!(
        grid.tiered_scrollback_lines(),
        1,
        "fixture should leave exactly one recoverable tiered line"
    );

    grid.clear_damage();
    let unscrolled = grid.unscroll_from_scrollback(100);

    assert_eq!(unscrolled, 1);
    assert_eq!(dirty_rows(&grid), vec![0, 1, 2, 3]);
}

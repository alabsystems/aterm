// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! 24-bit colour across a resize that does not reflow columns (same size, rows only, every
//! alternate-screen resize). The renderer reads ring-stored truecolor from the dense RGB ring,
//! which `invalidate_rings` drops; `migrate_rgb_ring_to_extras` must harvest it first, or the
//! cell keeps its RGB-overflow flag but falls to the theme default.
//!
//! Row `n` is labelled `R<n>` and painted in a colour that names `n`, so a row keeps its colour
//! only if its own colour follows it wherever the resize moved it.

use crate::test_counters::take_rgb_harvest_cells;
use crate::{Cell, CellFlags, Grid, PackedColor};

type Rgb = Option<[u8; 3]>;

fn fg_of(n: u16) -> [u8; 3] {
    [(n + 1) as u8, 20, 30]
}

fn bg_of(n: u16) -> [u8; 3] {
    [10, (n + 1) as u8, 30]
}

/// `rows` rows labelled `R0`, `R1`, ..., every cell truecolor in the dense ring.
/// `max_scrollback` 0 is the alternate screen's shape.
fn painted(rows: u16, cols: u16, cursor_row: u16, max_scrollback: usize) -> Grid {
    let mut grid = Grid::with_scrollback(rows, cols, max_scrollback);
    for r in 0..rows {
        let label = format!("R{r}");
        for col in 0..cols {
            let ch = label.chars().nth(usize::from(col)).unwrap_or(' ');
            let (fg, bg) = (fg_of(r), bg_of(r));
            let cell = Cell::with_style(
                ch,
                PackedColor::rgb(fg[0], fg[1], fg[2]),
                PackedColor::rgb(bg[0], bg[1], bg[2]),
                CellFlags::empty(),
            );
            grid.row_mut(r).unwrap().set(col, cell);
        }
        grid.extras_mut().set_rgb_ring_range(
            r,
            0,
            cols,
            Some(fg_of(r)),
            Some(bg_of(r)),
            rows,
            cols,
        );
    }
    grid.set_cursor(cursor_row, 2);
    grid
}

fn label(grid: &Grid, row: u16) -> Option<u16> {
    grid.row(row)?
        .to_string()
        .trim_end()
        .strip_prefix('R')?
        .parse()
        .ok()
}

fn colours(grid: &Grid, row: u16, cols: u16) -> Vec<(Rgb, Rgb)> {
    (0..cols)
        .map(|c| (grid.fg_rgb_at(row, c), grid.bg_rgb_at(row, c)))
        .collect()
}

/// Every labelled row on screen carries its own label's colour across `cols`.
fn assert_every_row_keeps_its_colour(grid: &Grid, cols: u16, what: &str) {
    let mut seen = 0;
    for r in 0..grid.rows() {
        let Some(n) = label(grid, r) else { continue };
        seen += 1;
        for (c, (fg, bg)) in colours(grid, r, cols).into_iter().enumerate() {
            assert_eq!(fg, Some(fg_of(n)), "{what}: row {r} (R{n}) col {c} fg");
            assert_eq!(bg, Some(bg_of(n)), "{what}: row {r} (R{n}) col {c} bg");
        }
    }
    assert!(seen > 0, "{what}: the fixture lost every labelled row");
}

#[test]
fn rows_only_resize_keeps_truecolor_fg_bg() {
    for max_scrollback in [0, 100] {
        for cursor in [0, 5] {
            for (to, what) in [(4, "shrink"), (9, "grow")] {
                let mut grid = painted(6, 10, cursor, max_scrollback);
                grid.resize_no_reflow(to, 10);
                assert_every_row_keeps_its_colour(
                    &grid,
                    10,
                    &format!("{what} to {to}, cursor {cursor}, scrollback {max_scrollback}"),
                );
            }
        }
    }
}

#[test]
fn same_size_resize_keeps_truecolor() {
    for max_scrollback in [0, 100] {
        let mut grid = painted(6, 10, 3, max_scrollback);
        grid.resize(6, 10);
        assert_every_row_keeps_its_colour(&grid, 10, "same-size resize");
    }
}

#[test]
fn alt_width_change_keeps_truecolor() {
    let mut grid = painted(6, 10, 3, 0);
    grid.resize_no_reflow(6, 7);
    assert_every_row_keeps_its_colour(&grid, 7, "alt screen narrowed");
    grid.resize_no_reflow(6, 12);
    assert_every_row_keeps_its_colour(&grid, 7, "alt screen widened again");
}

#[test]
fn two_resizes_keep_truecolor() {
    // The first resize harvests the ring into the map and drops it; the second reads the map.
    for max_scrollback in [0, 100] {
        let mut grid = painted(6, 10, 5, max_scrollback);
        grid.resize_no_reflow(5, 10);
        grid.resize_no_reflow(4, 10);
        assert_every_row_keeps_its_colour(&grid, 10, "two shrinks");
        grid.resize_no_reflow(6, 10);
        assert_every_row_keeps_its_colour(&grid, 10, "two shrinks and a grow");
    }
}

#[test]
fn main_width_change_keeps_truecolor() {
    // The control: the column-reflow path reads the ring itself.
    let mut grid = painted(6, 10, 5, 100);
    grid.resize(6, 8);
    let mut seen = 0;
    for r in 0..grid.rows() {
        if let Some(n) = label(&grid, r) {
            seen += 1;
            assert_eq!(grid.fg_rgb_at(r, 0), Some(fg_of(n)), "row {r} fg");
            assert_eq!(grid.bg_rgb_at(r, 0), Some(bg_of(n)), "row {r} bg");
        }
    }
    assert!(seen > 0);
}

#[test]
fn a_screen_with_no_truecolor_harvests_no_cell() {
    let mut grid = Grid::with_scrollback(6, 10, 0);
    for r in 0..6 {
        grid.set_cursor(r, 0);
        grid.write_char('x');
    }
    take_rgb_harvest_cells();
    grid.resize_no_reflow(5, 10);
    grid.resize_no_reflow(6, 10);
    assert_eq!(
        take_rgb_harvest_cells(),
        0,
        "no RGB ring, so no cell is read"
    );
}

#[test]
fn a_second_resize_harvests_nothing() {
    let mut grid = painted(6, 10, 3, 0);
    take_rgb_harvest_cells();
    grid.resize_no_reflow(5, 10);
    assert!(
        take_rgb_harvest_cells() > 0,
        "the first resize harvests the ring"
    );
    grid.resize_no_reflow(6, 10);
    assert_eq!(
        take_rgb_harvest_cells(),
        0,
        "the ring is gone after the first resize: the second finds the colour in the map"
    );
    assert_every_row_keeps_its_colour(&grid, 10, "after the second resize");
}

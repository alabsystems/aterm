// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded cost of the rows-only resize. It changes no line's width, so only the live/history
//! boundary moves inside the ring; evacuating the ring into the tiered store would cost
//! O(history) under the terminal lock and strand bump-allocated `PageStore` pages.
//! `count_rows_only_resize_migrated_rows` counts the rows it evacuates: bounded by the height
//! delta, independent of history depth. `cost_bound` covers the width rewrap.

use super::super::super::*;
use crate::test_counters::take_rows_only_resize_migrated_rows;
use aterm_scrollback::{Scrollback, ScrollbackStorage};

/// A tiered grid (the GUI's shape) whose ring holds `ring` history lines and whose store is empty.
fn tiered_grid_with_full_ring(rows: u16, cols: u16, ring: usize) -> Grid {
    let sb: ScrollbackStorage = Scrollback::new(1000, 10_000, 100_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, ring, sb);
    for i in 0..ring {
        grid.set_cursor(rows - 1, 0);
        for c in format!("L{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }
    assert_eq!(
        grid.ring_buffer_scrollback(),
        ring,
        "fixture: the whole history must be ring-resident before the resize"
    );
    grid
}

/// A rows-only resize evacuates at most the height delta out of the ring.
#[test]
fn rows_only_resize_migration_is_bounded_by_the_height_delta() {
    let (rows, cols) = (50u16, 200u16);
    let ring = 10_000usize;
    let mut grid = tiered_grid_with_full_ring(rows, cols, ring);

    let _ = take_rows_only_resize_migrated_rows(); // reset
    grid.resize(rows - 1, cols); // one step of a window-height drag
    let migrated = take_rows_only_resize_migrated_rows();

    // Budget: one viewport of slack over the height delta of 1, a bound on the shape.
    let budget = usize::from(rows);
    assert!(
        migrated <= budget,
        "a rows-only resize must evacuate <= {budget} rows from the ring, got \
         {migrated} (a history-sized count means the whole-ring migration is back \
         — the L0 stall on the UI thread under `term_lock`)"
    );
    grid.assert_invariants();
}

/// The migrated count is independent of ring depth: same resize, four depths
/// spanning 10x, identical counts.
#[test]
fn rows_only_resize_migration_is_independent_of_ring_depth() {
    let (rows, cols) = (50u16, 200u16);
    let mut counts = Vec::new();
    for ring in [1_000usize, 2_500, 5_000, 10_000] {
        let mut grid = tiered_grid_with_full_ring(rows, cols, ring);
        let _ = take_rows_only_resize_migrated_rows(); // reset
        grid.resize(rows - 1, cols);
        counts.push((ring, take_rows_only_resize_migrated_rows()));
        grid.assert_invariants();
    }
    let first = counts[0].1;
    assert!(
        counts.iter().all(|&(_, n)| n == first),
        "the rows-only resize migration must not scale with ring depth, got {counts:?}"
    );
}

/// A rows grow migrates nothing.
#[test]
fn rows_only_grow_migrates_nothing() {
    let (rows, cols) = (50u16, 200u16);
    let mut grid = tiered_grid_with_full_ring(rows, cols, 10_000);

    let _ = take_rows_only_resize_migrated_rows(); // reset
    grid.resize(rows + 1, cols);
    let migrated = take_rows_only_resize_migrated_rows();

    assert_eq!(
        migrated, 0,
        "a rows-GROW reveals history by reclassification — it evacuates nothing"
    );
    grid.assert_invariants();
}

/// History stays in the ring, so its rows never strand their `PageStore` pages.
#[test]
fn rows_only_resize_leaves_history_in_the_ring() {
    let (rows, cols) = (50u16, 200u16);
    let ring = 10_000usize;
    let mut grid = tiered_grid_with_full_ring(rows, cols, ring);
    let total_before = grid.scrollback_lines();
    assert_eq!(
        grid.tiered_scrollback_lines(),
        0,
        "fixture: nothing in the store or lazy buffer yet"
    );

    grid.resize(rows - 1, cols);

    assert!(
        grid.ring_buffer_scrollback() >= ring,
        "history must stay ring-resident across a rows-only resize (ring={}, was {ring})",
        grid.ring_buffer_scrollback()
    );
    assert!(
        grid.scrollback_lines() >= total_before,
        "and nothing may be lost (before={total_before}, after={})",
        grid.scrollback_lines()
    );
    grid.assert_invariants();
}

/// On a tiered grid, ring-history extras (a hyperlink on a scrolled-off line) survive a rows-only
/// resize. `rows_only_ring_retention` pins the ring-only shape.
#[test]
fn rows_only_resize_keeps_tiered_ring_history_extras() {
    use std::sync::Arc;

    let sb: ScrollbackStorage = Scrollback::new(1000, 10_000, 100_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(3, 20, 10_000, sb);
    let url: Arc<str> = Arc::from("https://example.com/tiered-rows-only");
    for c in "Hello".chars() {
        grid.write_char(c);
    }
    for col in 0..5u16 {
        grid.extras_mut()
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url.clone()));
    }
    grid.carriage_return();
    grid.line_feed();
    for i in 0..10 {
        for c in format!("N{i}").chars() {
            grid.write_char(c);
        }
        grid.carriage_return();
        grid.line_feed();
    }
    let linked = grid
        .get_history_line(0)
        .expect("precondition: the scrolled-off line is in history");
    assert!(
        linked.hyperlinks().is_some_and(|spans| !spans.is_empty()),
        "precondition: the scrolled-off line carries its hyperlink"
    );
    drop(linked);

    grid.resize(2, 20);

    let linked = grid
        .get_history_line(0)
        .expect("history survives the rows-only shrink");
    assert_eq!(linked.to_string().trim_end(), "Hello");
    assert!(
        linked
            .hyperlinks()
            .is_some_and(|spans| spans[0].url.as_ref() == "https://example.com/tiered-rows-only"),
        "a tiered grid's ring-history extras must survive a rows-only resize too"
    );
    grid.assert_invariants();
}

/// A width resize still takes the generic `adjust_row_count` path; the rows-only early return
/// does not swallow it.
#[test]
fn width_resize_still_reflows_and_preserves_history() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_full_ring(rows, cols, 500);
    let before = grid.scrollback_lines();

    grid.resize(rows, cols / 2);

    assert!(
        grid.scrollback_lines() >= before,
        "a width resize must still preserve history (before={before}, after={})",
        grid.scrollback_lines()
    );
    grid.assert_invariants();
}

// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded cost of the rows-only resize. It changes no line's width, so only the live/history
//! boundary moves inside the ring; evacuating the ring into the tiered store would cost
//! O(history) under the terminal lock and strand bump-allocated `PageStore` pages.
//! `count_rows_only_resize_migrated_rows` counts the rows it evacuates: a grow evacuates none,
//! and a shrink leaves the whole history ring-resident. `cost_bound` covers the width rewrap.

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

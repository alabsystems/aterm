// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for grid pin and generation tracking.

use crate::{GenerationTracker, Grid};

#[test]
fn generation_tracker_new() {
    let tracker = GenerationTracker::new();
    assert_eq!(tracker.current_generation(), 0);
    assert_eq!(tracker.page_generation(0), 0);
    assert_eq!(tracker.page_generation(100), 0);
}

#[test]
fn generation_tracker_evict() {
    let mut tracker = GenerationTracker::new();
    tracker.ensure_capacity(3);

    // Evict page 1
    tracker.evict_page(1);
    assert_eq!(tracker.page_generation(0), 0);
    assert_eq!(tracker.page_generation(1), 1);
    assert_eq!(tracker.page_generation(2), 0);
    assert_eq!(tracker.current_generation(), 1);

    // Evict page 1 again
    tracker.evict_page(1);
    assert_eq!(tracker.page_generation(1), 2);
    assert_eq!(tracker.current_generation(), 2);
}

/// A restore continues the source's numbering: the counter moves to the
/// carried value, bounded below by what the grid holds and above by the
/// overflow ceiling.
#[test]
fn continue_absolute_numbering_is_bounded_both_ways() {
    let mut grid = Grid::new(4, 10);
    assert_eq!(grid.absolute_row_counter(), 4);
    grid.continue_absolute_numbering(1_000);
    assert_eq!(grid.absolute_row_counter(), 1_000);
    assert_eq!(
        grid.visible_to_absolute(0),
        996,
        "the top row is renumbered"
    );
    grid.continue_absolute_numbering(0);
    assert_eq!(
        grid.absolute_row_counter(),
        4,
        "never below the rows it holds"
    );
    grid.continue_absolute_numbering(u64::MAX);
    assert_eq!(
        grid.absolute_row_counter(),
        u64::MAX >> 16,
        "the forged value is clamped"
    );
}

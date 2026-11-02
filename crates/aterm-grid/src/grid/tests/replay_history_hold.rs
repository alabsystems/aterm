// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for a replay's trimmed history held staged rather than stored (`Grid::holds_replay_history`).

use super::super::*;
use crate::grid::test_support::{feed_numbered_lines, history_trimmed};

/// The history a replay keeps between input slices, as a rekey fold trims it.
const KEEP: usize = 2048;
/// Lines one input slice scrolls into history.
const SLICE: usize = 150;

fn replay_grid(limit: Option<usize>, budget: usize) -> Grid {
    let scrollback = Scrollback::new(1000, 10_000, budget);
    let mut grid = Grid::with_tiered_scrollback(24, 80, 100, scrollback);
    grid.set_scrollback_line_limit(limit);
    grid
}

/// Feed `slices` slices of numbered lines, trimming history to [`KEEP`] after each as a rekey
/// fold does, and call `each` after every trim. Unless `held`, the hold each trim grants is
/// withdrawn at once, so every drain stores its rows: the reference a held replay must match.
fn replay(grid: &mut Grid, slices: usize, held: bool, mut each: impl FnMut(&Grid)) {
    for slice in 0..slices {
        feed_numbered_lines(grid, slice * SLICE, SLICE);
        assert!(grid.trim_replay_history(KEEP), "the trim applies");
        if !held {
            grid.storage.replay_trim = 0;
        }
        grid.storage.lazy_buffer.assert_running_sums();
        each(grid);
    }
}

#[test]
fn a_trimmed_replay_holds_its_flood_staged_and_stores_none_of_it() {
    let mut grid = replay_grid(None, aterm_scrollback::DEFAULT_MEMORY_BUDGET);
    let mut staged = 0;
    let mut fed = 0;
    replay(&mut grid, 40, true, |grid| {
        fed += SLICE;
        assert_eq!(
            grid.scrollback().map_or(0, |store| store.line_count()),
            0,
            "a drain held the rows the trims discard, so none reached the store"
        );
        assert_eq!(
            grid.scrollback_lines(),
            (fed - usize::from(grid.rows()) + 1).min(KEEP),
            "every line that left the screen is history, up to the trim's rows"
        );
        staged = staged.max(grid.lazy_backlog_len());
    });
    assert!(
        staged > 1000,
        "the held rows stayed staged past the drain threshold ({staged} staged)"
    );
}

#[test]
fn a_held_replay_keeps_the_history_a_stored_replay_keeps() {
    let big = aterm_scrollback::DEFAULT_MEMORY_BUDGET;
    for (limit, budget, case) in [
        (None, big, "no line limit"),
        (Some(KEEP), big, "a line limit at the trim"),
        (Some(KEEP + 300), big, "a line limit above the trim"),
        (
            Some(KEEP - 500),
            big,
            "a line limit below the trim, which drops stored rows first",
        ),
        (
            None,
            8 * 1024,
            "a memory budget the trim's rows outweigh, which evicts stored rows",
        ),
    ] {
        let mut stored = Vec::new();
        let mut reference = replay_grid(limit, budget);
        replay(&mut reference, 40, false, |grid| {
            stored.push((grid.scrollback_lines(), history_trimmed(grid)));
        });
        let mut slice = 0;
        let mut held = replay_grid(limit, budget);
        replay(&mut held, 40, true, |grid| {
            let (lines, history) = &stored[slice];
            assert_eq!(
                grid.scrollback_lines(),
                *lines,
                "{case}: slice {slice} keeps as many history rows held as stored"
            );
            assert!(
                history_trimmed(grid) == *history,
                "{case}: slice {slice} keeps the same history rows held as stored"
            );
            slice += 1;
        });
    }
}

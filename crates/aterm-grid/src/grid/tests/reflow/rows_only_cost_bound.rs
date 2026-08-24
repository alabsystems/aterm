// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded-cost obligation for the ROWS-ONLY resize — the second half of the
//! resize L0 budget, and the half that had no instrument at all.
//!
//! [`cost_bound`](super::cost_bound) converts "how long does a WIDTH resize
//! take" into a countable predicate (`count_scrollback_reflow_sync_lines`). That
//! counter fires at exactly one site, the width rewrap, so a rows-only resize
//! incremented nothing and the gate read 0 — which looks like proof of
//! O(viewport) and actually means NOT INSTRUMENTED.
//!
//! It was not O(viewport). `adjust_row_count` compared the FULL RING LENGTH
//! against the new VISIBLE row target (`rows.len() > target` with
//! `rows.len() = 10_050` and `target = 51`), so every rows-only resize —
//! window-height drag, pane split/close, divider drag, find-bar toggle, font
//! zoom — evacuated the ENTIRE ring into the tiered store: ~9,999 rows
//! materialized into `DeferredLine`s, synchronously, inside one `term_lock`
//! hold on the UI thread, per pane, per event. Both directions did it, because
//! a rows-GROW also has `rows.len() > target`. And because `PageStore` is
//! bump-only with no free path, the evacuated rows' pages were STRANDED for the
//! process lifetime.
//!
//! The migration bought nothing: a rows-only resize changes no line's WIDTH, so
//! no line's wrap topology moves — only where the live/history boundary falls
//! inside the same ring. `scrollback_lines() == ring + lazy + tiered` counts a
//! retained line identically in whichever tier it sits, so relocating the ring
//! into the store expressed nothing that reclassifying in place does not.
//!
//! `count_rows_only_resize_migrated_rows` is that missing instrument: the rows
//! a rows-only resize evacuates OUT of the ring. The obligation is that it stays
//! bounded by the HEIGHT DELTA and is INDEPENDENT of history depth.

use super::super::super::*;
use crate::test_counters::take_rows_only_resize_migrated_rows;
use aterm_scrollback::{Scrollback, ScrollbackStorage};

/// A tiered grid (every GUI session's shape: `Terminal::with_scrollback(rows,
/// cols, LIVE_SCROLLBACK_RING_LINES, Scrollback::with_defaults())`) whose ring
/// holds `ring` history lines and whose store is still empty — the state a live
/// session sits in for its first 10,000 lines of output, and the state in which
/// a resize decides the whole ring's fate.
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

/// THE OBLIGATION. A rows-only resize evacuates at most the HEIGHT DELTA worth
/// of rows out of the ring — the rows the shrunken retention cap can no longer
/// hold — never a history-sized number.
#[test]
fn rows_only_resize_migration_is_bounded_by_the_height_delta() {
    let (rows, cols) = (50u16, 200u16);
    let ring = 10_000usize;
    let mut grid = tiered_grid_with_full_ring(rows, cols, ring);

    let _ = take_rows_only_resize_migrated_rows(); // reset
    grid.resize(rows - 1, cols); // one step of a window-height drag
    let migrated = take_rows_only_resize_migrated_rows();

    // Budget: one viewport. The height delta is 1; a viewport of slack keeps
    // this a bound on the SHAPE, not a brittle exact-count pin.
    let budget = usize::from(rows);
    assert!(
        migrated <= budget,
        "a rows-only resize must evacuate <= {budget} rows from the ring, got \
         {migrated} (a history-sized count means the whole-ring migration is back \
         — the L0 stall on the UI thread under `term_lock`)"
    );
    grid.assert_invariants();
}

/// THE CATEGORICAL HALF: the migrated count does not depend on how deep the
/// ring is. Same geometry, same resize, four ring depths spanning 10x — if the
/// counts differ the cost is O(history) again, whatever its constant.
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

/// A rows-GROW migrates NOTHING. Pre-fix it took the same branch as a shrink
/// (`rows.len() > target` is true for a 10,050-row ring at any viewport) and
/// evacuated ~9,990 rows to reveal ONE line of history.
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

/// The mechanism, stated as state rather than as a counter: after the resize
/// history is still IN THE RING. This is what makes the bound above true, and
/// it is also the memory half — rows that never leave the ring never strand
/// their `PageStore` pages (`alloc_slice_impl` is bump-only; this path never
/// rebuilds, so evacuated rows' pages were unreclaimable for the process
/// lifetime).
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

/// The side effect the whole-ring migration was quietly paying for, on a
/// TIERED grid: it DESTROYED ring-history extras. `resize` cleared
/// `ring_extras` (the ring history's side table) and then re-extracted each
/// drained row with `row_idx = u16::MAX`, which matches nothing in the live
/// HashMap — so a hyperlink on a scrolled-off line vanished on every
/// window-height drag. Reclassifying in place keeps the side table, so the
/// entry rides its row. `rows_only_ring_retention::rows_only_resize_keeps_ring_history_extras`
/// pins the same property for the ring-only shape.
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

/// A WIDTH resize is untouched by all of the above: it still goes through the
/// generic `adjust_row_count`, whose ring-sized migration is legitimate there
/// (the rows are being rebuilt at a new width anyway) and is governed by
/// `count_scrollback_reflow_sync_lines` instead. Guards against the rows-only
/// early return swallowing width resizes if `self.storage.cols` is ever updated
/// before `adjust_row_count` runs.
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

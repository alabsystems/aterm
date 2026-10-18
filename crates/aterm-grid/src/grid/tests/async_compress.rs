// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Off-thread tier-compression offload (deferred bounded lazy-buffer drain),
//! against the real `Grid`:
//!   - while a worker owns the drain, staged lines stay readable;
//!   - a bounded drain loses no history and preserves order;
//!   - with no worker attached, the drain runs inline.

use crate::Grid;
use crate::grid::test_support::feed_numbered_lines;
use aterm_scrollback::Scrollback;

/// The oldest→newest history contents, reading every scrollback line.
fn history_oldest_first(grid: &Grid) -> Vec<String> {
    let total = grid.scrollback_lines();
    (0..total)
        .rev()
        .map(|rev| {
            grid.history_line_rev(rev)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn deferred_backlog_stays_readable_then_drains_without_loss() {
    let scrollback = Scrollback::new(100, 1000, 100_000_000);
    // Small ring so every fed line quickly evicts into the lazy buffer.
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);
    grid.set_compress_offload_active(true);

    // Feed past the 1000-line drain threshold: with the worker owning the drain,
    // these accumulate in the lazy buffer instead of being promoted inline.
    let total_fed = 1100usize;
    feed_numbered_lines(&mut grid, 0, total_fed);

    let backlog = grid.lazy_backlog_len();
    assert!(
        backlog > 1000,
        "offload-active drain must be deferred: backlog={backlog}"
    );

    // Every staged line is readable now, oldest to newest with no gaps (the
    // newest few lines are still on screen).
    let before = history_oldest_first(&grid);
    assert_eq!(before.len(), grid.scrollback_lines());
    let expected: Vec<String> = (0..before.len()).map(|i| format!("L{i}")).collect();
    assert_eq!(
        before, expected,
        "staged history is contiguous L0.. in order"
    );

    let depth_before = grid.scrollback_lines();

    // The worker drains in bounded batches; each call shrinks the backlog.
    let mut guard = 0;
    while grid.lazy_backlog_len() > 0 {
        let remaining = grid.drain_lazy_bounded(300);
        assert_eq!(remaining, grid.lazy_backlog_len());
        guard += 1;
        assert!(guard < 100, "bounded drain must make progress every call");
    }

    assert_eq!(grid.scrollback_lines(), depth_before, "no lines lost");
    assert!(
        grid.tiered_scrollback_lines() > 0,
        "backlog was promoted into the tiers"
    );
    assert_eq!(
        history_oldest_first(&grid),
        before,
        "content identical before and after the deferred drain"
    );
}

#[test]
fn bounded_drain_promotes_at_most_the_batch() {
    let scrollback = Scrollback::new(100, 1000, 100_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);
    grid.set_compress_offload_active(true);
    feed_numbered_lines(&mut grid, 0, 1100);

    let start = grid.lazy_backlog_len();
    let after = grid.drain_lazy_bounded(200);
    // One batch removes at most `max_lines` from the backlog.
    assert!(after <= start.saturating_sub(1), "batch drained something");
    assert!(
        start - after <= 200,
        "a single bounded batch promotes at most max_lines ({} drained)",
        start - after
    );
}

#[test]
fn bounded_drain_is_a_noop_while_detached_for_reflow() {
    // While the store is detached for reflow, drain_lazy_bounded is a no-op
    // returning the unchanged backlog (kept for the re-attach flush), so the
    // worker stops on no progress instead of spinning on the term lock.
    let scrollback = Scrollback::new(100, 1000, 100_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);
    grid.set_compress_offload_active(true);
    feed_numbered_lines(&mut grid, 0, 400); // build some tiered history first
    // Detach the store for a width-change reflow; the job is never re-attached
    // (this test covers only the detach window).
    let _pending = grid.resize_offloading_scrollback(3, 100);
    feed_numbered_lines(&mut grid, 400, 1200);
    let before = grid.lazy_backlog_len();
    assert!(
        before > 256,
        "a backlog accumulated while detached: {before}"
    );
    let after = grid.drain_lazy_bounded(256);
    assert_eq!(after, before, "detached drain must not change the backlog");
    assert_eq!(grid.lazy_backlog_len(), before);
}

#[test]
fn inactive_offload_drains_inline_as_before() {
    let scrollback = Scrollback::new(100, 1000, 100_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);
    // Default: no worker attached — the reader drains inline at the threshold.
    feed_numbered_lines(&mut grid, 0, 1100);
    assert!(
        grid.lazy_backlog_len() <= 1000,
        "inline drain keeps the lazy buffer at/below the threshold, got {}",
        grid.lazy_backlog_len()
    );
    assert!(grid.tiered_scrollback_lines() > 0, "promoted inline");
}

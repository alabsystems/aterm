// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THRU-5 — off-thread tier-compression offload (deferred bounded lazy-buffer
//! drain). These tests pin the invariants the design rests on, all exercised
//! against the real `Grid`:
//!   - while a worker owns the drain, staged lines stay READABLE (no scrub
//!     blackout — the property that protects ARENA-SCROLL);
//!   - a bounded drain loses NO history and preserves ORDER;
//!   - the backlog is BOUNDED under sustained overload (inline fallback);
//!   - with no worker attached, behavior is unchanged (inline drain).

use crate::Grid;
use aterm_scrollback::Scrollback;

/// Feed `n` distinguishable lines ("L{i}") through the grid, one per line-feed.
fn feed_lines(grid: &mut Grid, start: usize, n: usize) {
    for i in start..start + n {
        grid.carriage_return();
        for c in format!("L{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }
}

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
    feed_lines(&mut grid, 0, total_fed);

    // The backlog is genuinely deferred (not drained inline)...
    let backlog = grid.lazy_backlog_len();
    assert!(
        backlog > 1000,
        "offload-active drain must be deferred: backlog={backlog}"
    );

    // ...yet every staged line is READABLE right now (the anti-blackout property)
    // and in strict oldest→newest order with no gaps (the newest few fed lines
    // are still on the visible screen, not yet in scrollback).
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

    // No history lost, order preserved, total unchanged.
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
    feed_lines(&mut grid, 0, 1100);

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
fn backpressure_bounds_the_backlog_under_sustained_overload() {
    let scrollback = Scrollback::new(100, 1000, 100_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);
    grid.set_compress_offload_active(true);

    // Feed far past the backpressure cap WITHOUT the worker ever running. The
    // reader must bound the backlog WITHOUT compressing inline (inline LZ4/zstd
    // on the PTY-drain path is the cat-flood collapse): past the cap it drops
    // its OLDEST staged lines, O(1), trading history depth for ingest speed.
    let total = Grid::ASYNC_COMPRESS_BACKPRESSURE + 5_000;
    feed_lines(&mut grid, 0, total);

    assert!(
        grid.lazy_backlog_len() <= Grid::ASYNC_COMPRESS_BACKPRESSURE,
        "backlog must stay clamped at the cap via drop-oldest, got {}",
        grid.lazy_backlog_len()
    );
    // The retained history is a CONTIGUOUS newest suffix: truncated at the
    // front (the deliberate flood trade), never gapped in the middle, and it
    // still reaches (nearly) the last line fed.
    let hist = history_oldest_first(&grid);
    assert_ne!(
        hist.first().map(String::as_str),
        Some("L0"),
        "oldest lines must have been dropped under sustained overload"
    );
    let first_n: usize = hist[0].trim_start_matches('L').parse().expect("L{n} line");
    let expected: Vec<String> = (first_n..first_n + hist.len())
        .map(|n| format!("L{n}"))
        .collect();
    assert_eq!(hist, expected, "retained history must be contiguous");
    let last_n = first_n + hist.len() - 1;
    assert!(
        last_n + 4 >= total,
        "history must end at the newest scrolled lines (last retained L{last_n}, fed {total})"
    );
    assert!(
        hist.len() >= Grid::ASYNC_COMPRESS_BACKPRESSURE,
        "the cap's worth of depth stays retained, got {}",
        hist.len()
    );
}

#[test]
fn bounded_drain_is_a_noop_while_detached_for_reflow() {
    // Regression: while the tiered store is detached for an off-thread reflow,
    // drain_lazy_bounded must be a NO-OP that returns the UNCHANGED backlog (the
    // staged lines are kept for the reflow re-attach flush — audit bug B). This
    // is what lets the compression worker break on no-progress instead of
    // busy-spinning the term lock for the whole reflow window.
    let scrollback = Scrollback::new(100, 1000, 100_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);
    grid.set_compress_offload_active(true);
    feed_lines(&mut grid, 0, 400); // build some tiered history first
    // Detach the store for a width-change reflow (must_use job dropped — this
    // test only exercises the detach WINDOW, not the re-attach).
    let _pending = grid.resize_offloading_scrollback(3, 100);
    // Re-accumulate a backlog into the now-detached lazy buffer.
    feed_lines(&mut grid, 400, 1200);
    let before = grid.lazy_backlog_len();
    assert!(
        before > 256,
        "a backlog accumulated while detached: {before}"
    );
    // No-op: same backlog out, nothing promoted (store is detached).
    let after = grid.drain_lazy_bounded(256);
    assert_eq!(after, before, "detached drain must not change the backlog");
    assert_eq!(grid.lazy_backlog_len(), before);
}

#[test]
fn inactive_offload_drains_inline_as_before() {
    let scrollback = Scrollback::new(100, 1000, 100_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);
    // Default: no worker attached — the reader drains inline at the threshold.
    feed_lines(&mut grid, 0, 1100);
    assert!(
        grid.lazy_backlog_len() <= 1000,
        "inline drain keeps the lazy buffer at/below the threshold, got {}",
        grid.lazy_backlog_len()
    );
    assert!(grid.tiered_scrollback_lines() > 0, "promoted inline");
}

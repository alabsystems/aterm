// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The ROWS-ONLY RESIZE STALL: how long one window-height / pane-split /
//! find-bar-toggle resize holds the caller's lock on a TIERED grid whose ring
//! is at the production depth.
//!
//! A rows-only resize changes no line WIDTH, so nothing in history needs
//! rewrapping — but `adjust_row_count`'s shrink branch compares the FULL ring
//! length against the new VISIBLE row target, so it evacuates every
//! ring-resident history row into the tiered store on every such event. At the
//! GUI's `LIVE_SCROLLBACK_RING_LINES = 10_000` ring and a 50x200 pane that is
//! ~9,999 rows migrated per pane per event, synchronously, under one
//! `term_lock` hold on the UI thread.
//!
//! Measured as a STALL (one event = one sample), not as throughput, and swept
//! over ring depth: the shape of the curve is the claim. Depth is the ONLY
//! variable across the sweep arms — geometry, content width and store settings
//! are fixed — so a flat curve means O(viewport) and a rising one means
//! O(history).
//!
//! Each routine RETURNS its grid rather than dropping it: `iter_batched` drops
//! the returned outputs OUTSIDE the timed region, and freeing a 10,050-row ring
//! plus its 16 MB of `PageStore` pages is itself O(ring depth) — leaving that in
//! would have added ~17 us of teardown to the deepest arm and blunted the very
//! curve the sweep exists to show.
//!
//! `noop_ring*` (a resize to the SAME dimensions) is the control: it runs the
//! whole resize prologue/epilogue and returns from `adjust_row_count` without
//! reclassifying anything, so it prices the depth-dependence that does NOT
//! belong to the migration. Read the shrink/grow arms against it, not against
//! zero.

use aterm_grid::Grid;
use aterm_scrollback::Scrollback;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

/// The GUI's pane geometry class (`50x200` is the brief's stated viewport).
const ROWS: u16 = 50;
const COLS: u16 = 200;
/// `aterm_gui::spawn::LIVE_SCROLLBACK_RING_LINES`.
const PROD_RING: usize = 10_000;
/// Content columns per history line. The per-row migration copies `row.len()`
/// cells (`DeferredLine::new_boxed`), so this fixes the per-row constant; the
/// finding's term of interest is the ROW COUNT, which the depth sweep varies.
const CONTENT: usize = 60;

/// A tiered grid (the shape every GUI session is constructed with) whose ring
/// holds `ring` history lines, filled with distinct near-realistic content.
fn tiered_grid_with_full_ring(ring: usize) -> Grid {
    use std::fmt::Write as _;
    let sb = Scrollback::with_defaults();
    let mut grid = Grid::with_tiered_scrollback(ROWS, COLS, ring, sb);
    // Fill exactly the ring: `visible + max_scrollback` rows are resident
    // before `scroll_up` starts spilling to the lazy buffer, so writing
    // `ring` lines leaves the ring at capacity with nothing yet in the store.
    let mut text = String::with_capacity(CONTENT);
    for i in 0..ring {
        text.clear();
        let _ = write!(text, "L{i}-");
        while text.len() < CONTENT {
            text.push('x');
        }
        grid.set_cursor(ROWS - 1, 0);
        for c in text.chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }
    grid
}

/// REACH GUARD. Refuse to report anything unless the fixture really is the
/// state the finding describes: a TIERED grid whose ring is at capacity, with
/// the whole of history resident in the ring and NOTHING yet in the store — so
/// the resize under test is the one that decides the ring's fate. Also pins
/// that history SURVIVES the resize, so an arm can never get fast by losing
/// lines. Panics (aborting the bench) rather than silently measuring the wrong
/// thing.
fn assert_arm_reaches_the_finding() {
    let mut grid = tiered_grid_with_full_ring(PROD_RING);
    let ring_before = grid.ring_buffer_scrollback();
    let total_before = grid.scrollback_lines();
    assert_eq!(
        ring_before, PROD_RING,
        "fixture must leave the whole {PROD_RING}-line history RESIDENT IN THE RING \
         (got {ring_before}) — otherwise the arm measures a ring that is already empty"
    );
    assert_eq!(
        total_before, ring_before,
        "fixture must leave the tiered store and lazy buffer EMPTY before the resize \
         (total={total_before}, ring={ring_before})"
    );
    assert!(
        grid.scrollback().is_some(),
        "fixture must be a TIERED grid — the ring-only shape already reclassifies in place"
    );

    grid.resize(ROWS - 1, COLS);
    let total_after = grid.scrollback_lines();
    assert!(
        total_after >= total_before,
        "a rows-only resize must not LOSE history (before={total_before}, after={total_after})"
    );
}

/// One rows-only resize event: a height change by one row, in each direction.
/// This is a pane split/close, a find-bar toggle, a divider drag or one step of
/// a window-height drag — the whole user-perceived stall.
///
/// Both directions matter because the pre-fix `adjust_row_count` compared the
/// FULL ring length against the new VISIBLE row target, so a rows-GROW entered
/// the same shrink-and-migrate branch as a rows-shrink and paid the same
/// whole-ring evacuation.
fn one_event(c: &mut Criterion) {
    assert_arm_reaches_the_finding();
    let mut group = c.benchmark_group("rows_only_resize");
    // One sample = one resize event; the arm is hundreds of microseconds, so
    // keep the sample count low and let the harness pair rounds instead.
    group.sample_size(10);
    for ring in [1_000usize, 2_500, 5_000, PROD_RING] {
        group.bench_function(format!("shrink_1row_ring{ring}"), |b| {
            b.iter_batched(
                || tiered_grid_with_full_ring(ring),
                |mut grid| {
                    grid.resize(ROWS - 1, COLS);
                    grid
                },
                BatchSize::PerIteration,
            );
        });
    }
    for ring in [1_000usize, PROD_RING] {
        group.bench_function(format!("noop_ring{ring}"), |b| {
            b.iter_batched(
                || tiered_grid_with_full_ring(ring),
                |mut grid| {
                    grid.resize(ROWS, COLS);
                    grid
                },
                BatchSize::PerIteration,
            );
        });
    }
    for ring in [1_000usize, PROD_RING] {
        group.bench_function(format!("grow_1row_ring{ring}"), |b| {
            b.iter_batched(
                || tiered_grid_with_full_ring(ring),
                |mut grid| {
                    grid.resize(ROWS + 1, COLS);
                    grid
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

/// One full DRAG STEP: shrink then grow back. The second event is what pays
/// the follow-on `drain_lazy_buffer` (LZ4 promotion of everything the first
/// event evacuated), so the pair is the honest cost of one drag tick.
fn drag_step(c: &mut Criterion) {
    let mut group = c.benchmark_group("rows_only_resize");
    group.sample_size(10);
    group.bench_function(format!("drag_step_ring{PROD_RING}"), |b| {
        b.iter_batched(
            || tiered_grid_with_full_ring(PROD_RING),
            |mut grid| {
                grid.resize(ROWS - 1, COLS);
                grid.resize(ROWS, COLS);
                grid
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(benches, one_event, drag_step);
criterion_main!(benches);

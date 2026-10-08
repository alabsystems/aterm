// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Scrollback scrub harness: the headless half of the scrub dimension, an
// `xtask perf` floor. Tiered compressed scrollback pays LZ4/zstd decode on the
// interactive scrub path, so this fences the read path against regressions.
//
//   targo --unverified run --release -q -p aterm-bench --example scroll_scrub_harness
//   -> {"scrub_median_rps":...,"pageup_median_rps":...,"jump_top_median_jps":...,
//       "fill_lines":...,"depth":...,"n":7,"warmup":2}
//
// Three phases, all bigger-is-better so `xtask::perf::compare` applies (the
// jump latency is reported as jumps per second):
//   scrub     small-delta wheel scrub (3 lines/step, overlapping viewports):
//             the cache-warm scrub cost.
//   pageup    full-depth page sweep (one screen/step, non-overlapping): a real
//             warm/cold decode per step, the hold-PageUp worst case.
//   jump_top  bottom -> deep-history jumps, each to a different cold-tier page:
//             the worst-case cold decode.
//
// The unit is rows materialized through `materialize_scrollback_row_full`, the
// call the renderer makes per visible scrollback row.

use std::hint::black_box;
use std::time::Instant;

use aterm_bench::rotating_corpus;
use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

/// Small shell window: scrolling dominates, like a real terminal.
const ROWS: u16 = 24;
const COLS: u16 = 80;

/// Live ring in front of the tiers — mirrors the GUI's `LIVE_SCROLLBACK_RING_LINES`.
const RING_LINES: usize = 10_000;

/// Fill target. 120k overflows the default 100k tiered limit: truncation runs,
/// the cold (zstd) tier is deep, and 100k+ lines remain.
const FILL_LINES: usize = 120_000;

/// Wheel scrub: 3 lines per step (a mouse notch), a bounded number of steps so
/// the phase stays sub-second even at pathological decode cost. Overlapping
/// windows exercise the cache-hit scrub path.
const SCRUB_DELTA: i32 = 3;
const SCRUB_STEPS: usize = 6_000;

/// Median-of-N + warmup: identical discipline to the other perf lanes.
const N_ITERS: usize = 7;
const WARMUP: usize = 2;

/// How many jump-to-top round trips to time per iteration (each is one full
/// bottom -> top jump plus a top-screen materialize).
const JUMP_REPS: usize = 400;

/// Materialize the whole visible history window at the current display offset —
/// exactly the per-row decode the renderer performs for a scrolled-back frame.
/// Returns the number of rows materialized (so callers can total the work).
fn materialize_visible(term: &Terminal) -> usize {
    let grid = term.grid();
    let offset = grid.display_offset();
    if offset == 0 {
        return 0; // live screen, no history decode
    }
    let mut rows = 0usize;
    // Visible history rows map to reverse indices [offset-1 .. offset-ROWS].
    for r in 0..ROWS as usize {
        if r >= offset {
            break;
        }
        let rev_idx = offset - 1 - r;
        if let Some(mat) = grid.materialize_scrollback_row_full(rev_idx, COLS) {
            black_box(&mat);
            rows += 1;
        }
    }
    rows
}

/// Wheel-scrub sweep: from the bottom, step up `SCRUB_DELTA` lines at a time,
/// materializing the visible window each step. Returns rows materialized.
fn scrub_pass(term: &mut Terminal) -> usize {
    term.grid_mut().scroll_to_bottom();
    let mut rows = 0usize;
    for _ in 0..SCRUB_STEPS {
        // Positive delta scrolls UP into older history (see `scroll_display`).
        term.grid_mut().scroll_display(SCRUB_DELTA);
        rows += materialize_visible(term);
        // Clamped at the top: further steps are no-ops — stop early.
        if term.grid().display_offset() >= term.grid().scrollback_lines() {
            break;
        }
    }
    rows
}

/// Page sweep: full depth, one screen per step (non-overlapping), materializing
/// each fresh screen — the tier-cache-thrashing hold-PageUp worst case.
fn pageup_pass(term: &mut Terminal) -> usize {
    term.grid_mut().scroll_to_bottom();
    let depth = term.grid().scrollback_lines();
    let mut rows = 0usize;
    let steps = depth / ROWS as usize + 1;
    for _ in 0..steps {
        // Positive delta = one screen further UP into history.
        term.grid_mut().scroll_display(ROWS as i32);
        rows += materialize_visible(term);
        if term.grid().display_offset() >= depth {
            break;
        }
    }
    rows
}

/// Stride between successive jump targets, in lines. Larger than the tier
/// block/page size (100) and co-prime-ish so consecutive jumps land on distinct
/// cold pages — the point of the sweep (see [`jump_top_pass`]).
const JUMP_STRIDE: usize = 137;

/// Jump into deep history: `JUMP_REPS` bottom -> deep-page jumps, each landing
/// on a different cold-tier page plus a screen materialize. Returns the jump
/// count. The tiers cache only the last two decompressed pages, so the target
/// sweeps the oldest region (stride > page size) and every rep pays a real
/// cold decode rather than a cache hit.
fn jump_top_pass(term: &mut Terminal) -> usize {
    let depth = term.grid().scrollback_lines();
    // Sweep the oldest `span` lines (all in the warm/cold tiers, past the 10k live
    // ring), guarding against a shallow fill.
    let span = (JUMP_REPS * JUMP_STRIDE)
        .min(depth.saturating_sub(ROWS as usize))
        .max(1);
    for i in 0..JUMP_REPS {
        term.grid_mut().scroll_to_bottom();
        // A deep offset near the top, stepped by a page each rep (wrapping within
        // the oldest region) so every materialize is a fresh cold-page decode.
        let off = depth
            .saturating_sub((i * JUMP_STRIDE) % span)
            .max(ROWS as usize);
        term.grid_mut().scroll_display(off as i32);
        black_box(materialize_visible(term));
    }
    JUMP_REPS
}

fn median(samples: &[f64]) -> f64 {
    let mut v = samples.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Time a single closure returning a work count; report work/sec.
fn rate(mut f: impl FnMut() -> usize) -> f64 {
    let t0 = Instant::now();
    let work = f();
    let secs = t0.elapsed().as_secs_f64();
    if secs <= 0.0 {
        return f64::INFINITY;
    }
    work as f64 / secs
}

fn main() {
    // Fill once: scrub is a read-only viewport operation, so one settled tier
    // state serves every timed iteration; building it per iteration would
    // measure ingest and compression instead.
    let corpus = rotating_corpus(FILL_LINES);
    let mut term = Terminal::with_scrollback(ROWS, COLS, RING_LINES, Scrollback::with_defaults());
    term.process(black_box(&corpus));
    // Settle: force the lazy buffer to drain into the tiers so the timed reads
    // see the real compressed state, not undrained deferred lines.
    let _ = term.grid_mut().scrollback_mut();
    let depth = term.grid().scrollback_lines();
    eprintln!("scroll_scrub_harness: filled {FILL_LINES} lines -> {depth} scrollback depth");

    // The phase keys are mirrored by `xtask::perf::SCROLL_LANE`
    // (the gate reads each key) — keep the two in sync.
    let mut scrub = Vec::with_capacity(N_ITERS);
    let mut pageup = Vec::with_capacity(N_ITERS);
    let mut jump = Vec::with_capacity(N_ITERS);

    for _ in 0..WARMUP {
        let _ = scrub_pass(&mut term);
        let _ = pageup_pass(&mut term);
        let _ = jump_top_pass(&mut term);
    }
    for _ in 0..N_ITERS {
        scrub.push(rate(|| scrub_pass(&mut term)));
        pageup.push(rate(|| pageup_pass(&mut term)));
        jump.push(rate(|| jump_top_pass(&mut term)));
    }

    let scrub_med = median(&scrub);
    let pageup_med = median(&pageup);
    let jump_med = median(&jump);
    eprintln!(
        "scroll_scrub_harness: scrub {scrub_med:.0} rows/s | pageup {pageup_med:.0} rows/s | \
         jump_top {jump_med:.1} jumps/s (depth {depth}, {N_ITERS} iters)"
    );
    println!(
        "{{\"scrub_median_rps\":{scrub_med:.3},\"pageup_median_rps\":{pageup_med:.3},\
         \"jump_top_median_jps\":{jump_med:.3},\"fill_lines\":{FILL_LINES},\"depth\":{depth},\
         \"n\":{N_ITERS},\"warmup\":{WARMUP}}}"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scrollback scrub benchmark harness for the headless scrub performance lane.

use std::hint::black_box;
use std::time::Instant;

use aterm_bench::rotating_corpus;
use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

const ROWS: u16 = 24;
const COLS: u16 = 80;
const RING_LINES: usize = 10_000;
const FILL_LINES: usize = 120_000;
const SCRUB_DELTA: i32 = 3;
const SCRUB_STEPS: usize = 6_000;
const N_ITERS: usize = 7;
const WARMUP: usize = 2;
const JUMP_REPS: usize = 400;

fn materialize_visible(term: &Terminal) -> usize {
    let grid = term.grid();
    let offset = grid.display_offset();
    if offset == 0 {
        return 0; // live screen, no history decode
    }
    let mut rows = 0usize;
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

fn scrub_pass(term: &mut Terminal) -> usize {
    term.grid_mut().scroll_to_bottom();
    let mut rows = 0usize;
    for _ in 0..SCRUB_STEPS {
        term.grid_mut().scroll_display(SCRUB_DELTA);
        rows += materialize_visible(term);
        if term.grid().display_offset() >= term.grid().scrollback_lines() {
            break;
        }
    }
    rows
}

fn pageup_pass(term: &mut Terminal) -> usize {
    term.grid_mut().scroll_to_bottom();
    let depth = term.grid().scrollback_lines();
    let mut rows = 0usize;
    let steps = depth / ROWS as usize + 1;
    for _ in 0..steps {
        term.grid_mut().scroll_display(ROWS as i32);
        rows += materialize_visible(term);
        if term.grid().display_offset() >= depth {
            break;
        }
    }
    rows
}

const JUMP_STRIDE: usize = 137;

fn jump_top_pass(term: &mut Terminal) -> usize {
    let depth = term.grid().scrollback_lines();
    let span = (JUMP_REPS * JUMP_STRIDE)
        .min(depth.saturating_sub(ROWS as usize))
        .max(1);
    for i in 0..JUMP_REPS {
        term.grid_mut().scroll_to_bottom();
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
    let corpus = rotating_corpus(FILL_LINES);
    let mut term = Terminal::with_scrollback(ROWS, COLS, RING_LINES, Scrollback::with_defaults());
    term.process(black_box(&corpus));
    let _ = term.grid_mut().scrollback_mut();
    let depth = term.grid().scrollback_lines();
    eprintln!("scroll_scrub_harness: filled {FILL_LINES} lines -> {depth} scrollback depth");

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

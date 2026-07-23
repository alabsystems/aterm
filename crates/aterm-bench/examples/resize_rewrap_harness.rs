// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// RESIZE/REWRAP fence harness (E0, audit §5.3): the 42-second-freeze CLASS.
// A width change once rewrapped the ENTIRE scrollback synchronously on the
// event-loop thread (gate.rs's mainloop census recalls it); the offload seam
// fixed the tiered path, but there was NO wall-clock fence — a regression
// re-introducing a synchronous O(history) rewrap would ship silently. This
// harness measures both shipping shapes:
//
//   ring     ring-only (the wasm/daemon shipping config): `Terminal::resize`
//            rewraps the whole ring synchronously — the cost every width drag
//            pays today. Floor: resizes/s; fence: worst single resize.
//   tiered   `resize_offloading_scrollback`: the SYNCHRONOUS half must stay
//            viewport-bounded (that is the L0 fix), with the history rewrap
//            stepped via `reflow_step` (the wasm pump's budget). Floor: full
//            offload+pump+reattach cycles/s; fence: worst SYNC phase.
//
// The gate applies BOTH a baseline-relative floor (rates) and ABSOLUTE
// catastrophic caps on the worst-ms values — the caps hold even on a fresh
// checkout with no baseline, so the 42s class can never ride in on a box that
// simply never recorded one.
//
//   cargo run --release -q -p aterm-bench --example resize_rewrap_harness
//   -> {"resize_ring_median_rps":...,"resize_tiered_median_rps":...,
//       "resize_ring_worst_ms":...,"resize_tiered_sync_worst_ms":...,
//       "ring_lines":...,"tiered_fill_lines":...,"n":...,"warmup":...}

use std::hint::black_box;
use std::time::Instant;

use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::{Terminal, TerminalBuilder};
use aterm_grid::ReflowStep;

const ROWS: u16 = 24;

/// Width toggle: 80 <-> 76 forces a real rewrap of every soft-wrapped line in
/// both directions (lines below are ~100 chars, so they wrap at either width).
const COLS_A: u16 = 80;
const COLS_B: u16 = 76;

/// Ring-only depth: the product's 50k scrollback cap — the deepest ring a
/// shipping pane rewraps synchronously today.
const RING_LINES: usize = 50_000;

/// Tiered fill: overflow the 100k default limit so warm/cold tiers are real.
const TIERED_FILL_LINES: usize = 120_000;
const TIERED_RING: usize = 10_000;

/// Resizes per timed pass (each pass toggles width back and forth so every
/// resize rewraps; an even count returns the grid to its start width).
const RESIZES_PER_PASS: usize = 6;

/// Pump budget for the tiered step loop — the wasm pump's default
/// (`REFLOW_STEP_BUDGET_LINES`), so the stepped total matches production.
const PUMP_BUDGET_LINES: usize = 2_000;

const N_ITERS: usize = 5;
const WARMUP: usize = 1;

/// ~100-char wrapping-prone lines (they re-break at BOTH toggle widths), with
/// a counter so content is distinguishable. Deterministic, no RNG/clock.
fn fill_corpus(lines: usize) -> Vec<u8> {
    const GLYPHS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/-=";
    let mut out = Vec::with_capacity(lines * 108);
    for line in 0..lines {
        out.extend_from_slice(line.to_string().as_bytes());
        out.push(b' ');
        for c in 0..96usize {
            out.push(GLYPHS[(line + c) % GLYPHS.len()]);
        }
        out.extend_from_slice(b"\r\n");
    }
    out
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

/// Ring-only pass: RESIZES_PER_PASS synchronous full-ring rewraps.
/// Returns (rate: resizes/s, worst single resize seconds).
fn ring_pass(term: &mut Terminal) -> (f64, f64) {
    let mut worst = 0.0f64;
    let t0 = Instant::now();
    for i in 0..RESIZES_PER_PASS {
        let cols = if i % 2 == 0 { COLS_B } else { COLS_A };
        let r0 = Instant::now();
        term.resize(ROWS, cols);
        worst = worst.max(r0.elapsed().as_secs_f64());
        black_box(term.grid().scrollback_lines());
    }
    let secs = t0.elapsed().as_secs_f64();
    let rate = if secs > 0.0 {
        RESIZES_PER_PASS as f64 / secs
    } else {
        f64::INFINITY
    };
    (rate, worst)
}

/// Tiered pass: offloading resize + budget-stepped rewrap + reattach, the
/// production pump schedule. Returns (full-cycle rate, worst SYNC-phase secs).
fn tiered_pass(term: &mut Terminal) -> (f64, f64) {
    let mut worst_sync = 0.0f64;
    let t0 = Instant::now();
    for i in 0..RESIZES_PER_PASS {
        let cols = if i % 2 == 0 { COLS_B } else { COLS_A };
        let s0 = Instant::now();
        let pending = term.resize_offloading_scrollback(ROWS, cols);
        // The SYNC phase — everything a caller's thread cannot avoid — ends
        // here; this is the value the L0 fence caps.
        worst_sync = worst_sync.max(s0.elapsed().as_secs_f64());
        // The ASYNC CONTRACT the 42s class violated: a width change over a deep
        // tiered history MUST hand the rewrap back as a pending job (stepped by
        // the pump), never complete it synchronously. If this returns None the
        // measured "sync phase" silently included the whole history rewrap and
        // the wall-clock fence would be capping the wrong thing — fail loudly.
        let mut job = pending.expect(
            "resize_offloading_scrollback returned no pending job over a deep tiered \
             history — the offload/async contract is broken",
        );
        loop {
            match job.reflow_step(PUMP_BUDGET_LINES) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(reflowed) => {
                    term.finish_resize_offload(reflowed);
                    break;
                }
            }
        }
        black_box(term.grid().scrollback_lines());
    }
    let secs = t0.elapsed().as_secs_f64();
    let rate = if secs > 0.0 {
        RESIZES_PER_PASS as f64 / secs
    } else {
        f64::INFINITY
    };
    (rate, worst_sync)
}

fn main() {
    // -- ring-only terminal at the 50k cap --
    let ring_corpus = fill_corpus(RING_LINES);
    let mut ring_term = TerminalBuilder::new()
        .size(ROWS, COLS_A)
        .ring_buffer_size(RING_LINES)
        .build();
    ring_term.process(black_box(&ring_corpus));

    // -- tiered terminal overflowing into warm/cold --
    let tiered_corpus = fill_corpus(TIERED_FILL_LINES);
    let mut tiered_term =
        Terminal::with_scrollback(ROWS, COLS_A, TIERED_RING, Scrollback::with_defaults());
    tiered_term.process(black_box(&tiered_corpus));
    let tiered_depth = tiered_term.grid().scrollback_lines();

    for _ in 0..WARMUP {
        let _ = ring_pass(&mut ring_term);
        let _ = tiered_pass(&mut tiered_term);
    }
    let mut ring_rates = Vec::with_capacity(N_ITERS);
    let mut tiered_rates = Vec::with_capacity(N_ITERS);
    let mut ring_worst = 0.0f64;
    let mut tiered_sync_worst = 0.0f64;
    for _ in 0..N_ITERS {
        let (rate, worst) = ring_pass(&mut ring_term);
        ring_rates.push(rate);
        ring_worst = ring_worst.max(worst);
        let (rate, worst) = tiered_pass(&mut tiered_term);
        tiered_rates.push(rate);
        tiered_sync_worst = tiered_sync_worst.max(worst);
    }

    let ring_med = median(&ring_rates);
    let tiered_med = median(&tiered_rates);
    eprintln!(
        "resize_rewrap_harness: ring {RING_LINES} lines — {ring_med:.1} resizes/s, worst \
         {:.1} ms | tiered depth {tiered_depth} — {tiered_med:.1} cycles/s, worst SYNC \
         {:.2} ms",
        ring_worst * 1e3,
        tiered_sync_worst * 1e3,
    );
    println!(
        "{{\"resize_ring_median_rps\":{ring_med:.3},\"resize_tiered_median_rps\":{tiered_med:.3},\
         \"resize_ring_worst_ms\":{:.3},\"resize_tiered_sync_worst_ms\":{:.3},\
         \"ring_lines\":{RING_LINES},\"tiered_fill_lines\":{TIERED_FILL_LINES},\
         \"tiered_depth\":{tiered_depth},\"n\":{N_ITERS},\"warmup\":{WARMUP}}}",
        ring_worst * 1e3,
        tiered_sync_worst * 1e3,
    );
}

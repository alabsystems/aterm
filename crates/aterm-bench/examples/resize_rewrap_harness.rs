// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Resize/rewrap fence harness: a width change must never rewrap the whole
// scrollback synchronously on the event-loop thread. Workloads:
//
//   ring          ring-only (the wasm/daemon config): `Terminal::resize`
//                 rewraps the whole ring synchronously. Floor: resizes/s;
//                 fence: worst single resize.
//   tiered        `resize_offloading_scrollback`: the synchronous half stays
//                 viewport-bounded, with the history rewrap stepped via
//                 `reflow_step`. Floor: offload+pump+reattach cycles/s;
//                 fence: worst synchronous phase.
//   deep_mixed    a ~200k-logical-line history, ~90% short lines / ~10%
//                 wrapping: total off-thread rewrap time, worst single pump
//                 step, and the synchronous phase at depth.
//   deep_wrapall  the same depth with the all-wrapping worst-case corpus.
//   converge      a second width change lands mid-flight; reports whether
//                 the settled wrap width is stale (1) or the final width (0).
//
// `verify perf` applies a baseline-relative floor to the rates and absolute caps
// to the worst-ms values, so the caps hold even with no recorded baseline.
//
//   targo --unverified run --release -q -p aterm-bench --example resize_rewrap_harness
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

/// Ring-only depth: the product's 50k scrollback cap, the deepest ring a pane
/// rewraps synchronously.
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

/// Deep-history depth (logical lines): big enough that warm and cold tiers are
/// real and the pump takes hundreds of steps, small enough that the fence run
/// stays a few seconds.
const DEEP_FILL_LINES: usize = 200_000;
const DEEP_RING: usize = 10_000;
/// One toggle out and back per pass, so the terminal ends at its start width.
const DEEP_RESIZES: usize = 2;
const DEEP_ITERS: usize = 2;

/// ~100-char wrapping-prone lines (they re-break at both toggle widths), with
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

/// Mixed corpus: ~90% short (8..48-char, wrap-invariant at both toggle widths)
/// lines, ~10% ~100-char wrapping lines: the realistic shell-history shape.
fn fill_mixed_corpus(lines: usize) -> Vec<u8> {
    const GLYPHS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/-=";
    let mut out = Vec::with_capacity(lines * 44);
    for line in 0..lines {
        out.extend_from_slice(line.to_string().as_bytes());
        out.push(b' ');
        let n = if line % 10 == 0 { 96 } else { 8 + (line % 40) };
        for c in 0..n {
            out.push(GLYPHS[(line + c) % GLYPHS.len()]);
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// An unlimited-line-limit tiered terminal (retention bounded only by the byte
/// budget, the histories the offload exists for), filled with `corpus`.
fn deep_unlimited_term(corpus: &[u8]) -> Terminal {
    let mut sb = Scrollback::new(64, 512, 512_000_000);
    sb.set_line_limit(None);
    let mut term = Terminal::with_scrollback(ROWS, COLS_A, DEEP_RING, sb);
    term.process(black_box(corpus));
    term
}

/// Deep-pass measurements (see the header).
struct DeepPassStats {
    sync_worst: f64,
    pump_total: f64,
    step_worst: f64,
    steps: usize,
}

/// One deep pass: DEEP_RESIZES offloading width toggles, each pumped at the
/// production budget, timing every step.
fn deep_pass(term: &mut Terminal) -> DeepPassStats {
    let mut stats = DeepPassStats {
        sync_worst: 0.0,
        pump_total: 0.0,
        step_worst: 0.0,
        steps: 0,
    };
    for i in 0..DEEP_RESIZES {
        let cols = if i % 2 == 0 { COLS_B } else { COLS_A };
        let s0 = Instant::now();
        let pending = term.resize_offloading_scrollback(ROWS, cols);
        stats.sync_worst = stats.sync_worst.max(s0.elapsed().as_secs_f64());
        let mut job = pending.expect(
            "a width change over a deep unlimited tiered history must hand back a \
             pending job — a None means the offload/async contract broke at depth",
        );
        loop {
            let t = Instant::now();
            let step = job.reflow_step(PUMP_BUDGET_LINES);
            let dt = t.elapsed().as_secs_f64();
            stats.steps += 1;
            stats.pump_total += dt;
            stats.step_worst = stats.step_worst.max(dt);
            match step {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(reflowed) => {
                    // Re-attach can hand back a convergence job (the settled
                    // width disagreed with the job's); drive it through this
                    // same timed loop so its cost is priced here.
                    match term.finish_resize_offload(reflowed) {
                        Some(next) => job = next,
                        None => break,
                    }
                }
            }
        }
        black_box(term.grid().scrollback_lines());
    }
    stats
}

/// Convergence probe: a second width change lands mid-flight (throttled; the
/// one-in-flight rule detaches nothing), the job completes at the first width,
/// and the settled wrap width of the deep history is measured: 1 = stale, 0 =
/// converged at the final width. A metric, not an assert.
fn converge_probe() -> (u64, usize) {
    let mut term = deep_unlimited_term(&fill_corpus(60_000));

    let mut job = term
        .resize_offloading_scrollback(ROWS, COLS_B)
        .expect("deep tiered history detaches");
    // Reach guard (two-sided): the race must really race; the mid-flight
    // width change is throttled to a plain bounded resize.
    assert!(
        term.resize_offloading_scrollback(ROWS, 72).is_none(),
        "mid-flight width change must detach nothing while a job is in flight"
    );
    let reflowed = loop {
        match job.reflow_step(PUMP_BUDGET_LINES) {
            ReflowStep::InProgress(next) => job = next,
            ReflowStep::Done(done) => break done,
        }
    };
    // Drive every returned follow-up job (same pump loop) before measuring.
    // The loop also guards the protocol: convergence must settle, not
    // oscillate.
    let mut follow_up = term.finish_resize_offload(reflowed);
    let mut passes = 0usize;
    while let Some(mut job) = follow_up.take() {
        passes += 1;
        assert!(passes < 8, "convergence must settle, not oscillate");
        let done = loop {
            match job.reflow_step(PUMP_BUDGET_LINES) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(done) => break done,
            }
        };
        follow_up = term.finish_resize_offload(done);
    }

    let grid = term.grid();
    let depth = grid.scrollback_lines();
    assert!(
        depth > 40_000,
        "reach: deep history survived ({depth} lines)"
    );
    // Sample the oldest region, the tiered store the job rewrapped; the ring
    // was rewrapped synchronously at 72 and would mask staleness.
    let mut max_width = 0usize;
    for i in 0..depth.min(20_000) {
        if let Some(line) = grid.get_history_line(i)
            && let Some(text) = line.as_str()
        {
            max_width = max_width.max(text.chars().count());
        }
    }
    (u64::from(max_width > 72), depth)
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
/// production pump schedule. Returns (full-cycle rate, worst sync-phase secs).
fn tiered_pass(term: &mut Terminal) -> (f64, f64) {
    let mut worst_sync = 0.0f64;
    let t0 = Instant::now();
    for i in 0..RESIZES_PER_PASS {
        let cols = if i % 2 == 0 { COLS_B } else { COLS_A };
        let s0 = Instant::now();
        let pending = term.resize_offloading_scrollback(ROWS, cols);
        // The sync phase (everything a caller's thread cannot avoid) ends
        // here; this is the value the fence caps.
        worst_sync = worst_sync.max(s0.elapsed().as_secs_f64());
        // A width change over a deep tiered history must hand the rewrap back
        // as a pending job (stepped by the pump), never complete it
        // synchronously; otherwise the measured sync phase would include the
        // whole history rewrap and the fence would cap the wrong thing.
        let mut job = pending.expect(
            "resize_offloading_scrollback returned no pending job over a deep tiered \
             history — the offload/async contract is broken",
        );
        loop {
            match job.reflow_step(PUMP_BUDGET_LINES) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(reflowed) => {
                    // Widths are settled before each pump completes, so the
                    // convergence pass must not trigger here.
                    assert!(
                        term.finish_resize_offload(reflowed).is_none(),
                        "no convergence pass expected in the settled-width pump"
                    );
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

    // -- deep unlimited-history passes: mixed (realistic) + all-wrapping --
    let deep_mixed_corpus = fill_mixed_corpus(DEEP_FILL_LINES);
    let mut deep_mixed_term = deep_unlimited_term(&deep_mixed_corpus);
    let deep_mixed_depth = deep_mixed_term.grid().scrollback_lines();
    assert!(
        deep_mixed_depth > 150_000,
        "reach: the unlimited-limit mixed fill must be deep (got {deep_mixed_depth})"
    );
    let deep_wrap_corpus = fill_corpus(DEEP_FILL_LINES);
    let mut deep_wrap_term = deep_unlimited_term(&deep_wrap_corpus);
    let deep_wrap_depth = deep_wrap_term.grid().scrollback_lines();
    assert!(
        deep_wrap_depth > 300_000,
        "reach: the all-wrapping fill re-splits into >300k physical lines \
         (got {deep_wrap_depth})"
    );

    let mut mixed = DeepPassStats {
        sync_worst: 0.0,
        pump_total: f64::INFINITY,
        step_worst: 0.0,
        steps: 0,
    };
    let mut wrapall = DeepPassStats {
        sync_worst: 0.0,
        pump_total: f64::INFINITY,
        step_worst: 0.0,
        steps: 0,
    };
    for _ in 0..DEEP_ITERS {
        let s = deep_pass(&mut deep_mixed_term);
        mixed.sync_worst = mixed.sync_worst.max(s.sync_worst);
        mixed.pump_total = mixed.pump_total.min(s.pump_total);
        mixed.step_worst = mixed.step_worst.max(s.step_worst);
        mixed.steps = s.steps;
        let s = deep_pass(&mut deep_wrap_term);
        wrapall.sync_worst = wrapall.sync_worst.max(s.sync_worst);
        wrapall.pump_total = wrapall.pump_total.min(s.pump_total);
        wrapall.step_worst = wrapall.step_worst.max(s.step_worst);
        wrapall.steps = s.steps;
    }
    // Reach: the deep pump is genuinely chunked (a one-step completion would
    // mean the budget contract silently broke and the step metrics price
    // nothing).
    assert!(
        mixed.steps > 50 && wrapall.steps > 50,
        "reach: deep pumps must take many steps (mixed {}, wrapall {})",
        mixed.steps,
        wrapall.steps
    );

    let (converge_stale, converge_depth) = converge_probe();

    eprintln!(
        "resize_rewrap_harness: ring {RING_LINES} lines — {ring_med:.1} resizes/s, worst \
         {:.1} ms | tiered depth {tiered_depth} — {tiered_med:.1} cycles/s, worst SYNC \
         {:.2} ms",
        ring_worst * 1e3,
        tiered_sync_worst * 1e3,
    );
    eprintln!(
        "resize_rewrap_harness[deep]: mixed depth {deep_mixed_depth} — pump {:.0} ms \
         ({} steps, worst step {:.2} ms, sync {:.2} ms) | wrapall depth \
         {deep_wrap_depth} — pump {:.0} ms (worst step {:.2} ms) | converge stale={} \
         over {converge_depth} lines",
        mixed.pump_total * 1e3,
        mixed.steps,
        mixed.step_worst * 1e3,
        mixed.sync_worst * 1e3,
        wrapall.pump_total * 1e3,
        wrapall.step_worst * 1e3,
        converge_stale,
    );
    println!(
        "{{\"resize_ring_median_rps\":{ring_med:.3},\"resize_tiered_median_rps\":{tiered_med:.3},\
         \"resize_ring_worst_ms\":{:.3},\"resize_tiered_sync_worst_ms\":{:.3},\
         \"ring_lines\":{RING_LINES},\"tiered_fill_lines\":{TIERED_FILL_LINES},\
         \"tiered_depth\":{tiered_depth},\"n\":{N_ITERS},\"warmup\":{WARMUP},\
         \"deep_mixed_depth\":{deep_mixed_depth},\"deep_mixed_pump_total_ms\":{:.3},\
         \"deep_mixed_step_worst_ms\":{:.3},\"deep_mixed_sync_worst_ms\":{:.3},\
         \"deep_mixed_steps\":{},\"deep_wrapall_depth\":{deep_wrap_depth},\
         \"deep_wrapall_pump_total_ms\":{:.3},\"deep_wrapall_step_worst_ms\":{:.3},\
         \"converge_stale_width\":{converge_stale},\"converge_history_lines\":{converge_depth}}}",
        ring_worst * 1e3,
        tiered_sync_worst * 1e3,
        mixed.pump_total * 1e3,
        mixed.step_worst * 1e3,
        mixed.sync_worst * 1e3,
        mixed.steps,
        wrapall.pump_total * 1e3,
        wrapall.step_worst * 1e3,
    );
}

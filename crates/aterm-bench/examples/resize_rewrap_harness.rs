// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Resize/rewrap fence harness (`verify perf`).

use std::hint::black_box;
use std::time::Instant;

use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::{Terminal, TerminalBuilder};
use aterm_grid::ReflowStep;

const ROWS: u16 = 24;
const COLS_A: u16 = 80;
const COLS_B: u16 = 76;
const RING_LINES: usize = 50_000;
const TIERED_FILL_LINES: usize = 120_000;
const TIERED_RING: usize = 10_000;
const RESIZES_PER_PASS: usize = 6;
const PUMP_BUDGET_LINES: usize = 2_000;
const N_ITERS: usize = 5;
const WARMUP: usize = 1;
const DEEP_FILL_LINES: usize = 200_000;
const DEEP_RING: usize = 10_000;
const DEEP_RESIZES: usize = 2;
const DEEP_ITERS: usize = 5;

/// Deterministic ~100-char wrapping lines.
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

/// Deterministic mixed corpus (~90% short 8..48-char lines, ~10% ~100-char wrapping lines).
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

/// Unlimited-line-limit tiered terminal filled with `corpus`.
fn deep_unlimited_term(corpus: &[u8]) -> Terminal {
    let mut sb = Scrollback::new(64, 512, 512_000_000);
    sb.set_line_limit(None);
    let mut term = Terminal::with_scrollback(ROWS, COLS_A, DEEP_RING, sb);
    term.process(black_box(corpus));
    term
}

struct DeepPassStats {
    sync_worst: f64,
    pump_total: f64,
    step_worst: f64,
    steps: usize,
}

/// The deep passes as the report gives them: each latency's worst over every pass, the median of
/// the passes' worsts (a stall lands in one pass's worst, a step or resize made slower in every
/// pass's), and the fastest pump.
struct DeepSummary {
    sync_worst: f64,
    sync_worst_p50: f64,
    pump_total: f64,
    step_worst: f64,
    step_worst_p50: f64,
    steps: usize,
}

impl DeepSummary {
    fn of(passes: &[DeepPassStats]) -> Self {
        let each = |f: fn(&DeepPassStats) -> f64| passes.iter().map(f).collect::<Vec<f64>>();
        let sync = each(|p| p.sync_worst);
        let step = each(|p| p.step_worst);
        Self {
            sync_worst: worst(&sync),
            sync_worst_p50: median(&sync),
            pump_total: best(&each(|p| p.pump_total)),
            step_worst: worst(&step),
            step_worst_p50: median(&step),
            steps: passes.last().map_or(0, |p| p.steps),
        }
    }
}

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
                    // Drive any follow-up convergence job through the same timed loop.
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

/// Measure whether a mid-flight width change settles to the final width (0) or stays stale (1).
fn converge_probe() -> (u64, usize) {
    let mut term = deep_unlimited_term(&fill_corpus(60_000));

    let mut job = term
        .resize_offloading_scrollback(ROWS, COLS_B)
        .expect("deep tiered history detaches");
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

fn worst(samples: &[f64]) -> f64 {
    samples.iter().copied().fold(0.0, f64::max)
}

fn best(samples: &[f64]) -> f64 {
    samples.iter().copied().fold(f64::INFINITY, f64::min)
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

/// Ring-only pass returning `(resizes/s, worst_single_resize_secs)`.
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

/// Tiered pass returning `(full_cycle_rate, worst_sync_phase_secs)`.
fn tiered_pass(term: &mut Terminal) -> (f64, f64) {
    let mut worst_sync = 0.0f64;
    let t0 = Instant::now();
    for i in 0..RESIZES_PER_PASS {
        let cols = if i % 2 == 0 { COLS_B } else { COLS_A };
        let s0 = Instant::now();
        let pending = term.resize_offloading_scrollback(ROWS, cols);
        worst_sync = worst_sync.max(s0.elapsed().as_secs_f64());
        let mut job = pending.expect(
            "resize_offloading_scrollback returned no pending job over a deep tiered \
             history — the offload/async contract is broken",
        );
        loop {
            match job.reflow_step(PUMP_BUDGET_LINES) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(reflowed) => {
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
    let ring_corpus = fill_corpus(RING_LINES);
    let mut ring_term = TerminalBuilder::new()
        .size(ROWS, COLS_A)
        .ring_buffer_size(RING_LINES)
        .build();
    ring_term.process(black_box(&ring_corpus));

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
    let mut ring_worsts = Vec::with_capacity(N_ITERS);
    let mut tiered_sync_worsts = Vec::with_capacity(N_ITERS);
    for _ in 0..N_ITERS {
        let (rate, worst) = ring_pass(&mut ring_term);
        ring_rates.push(rate);
        ring_worsts.push(worst);
        let (rate, worst) = tiered_pass(&mut tiered_term);
        tiered_rates.push(rate);
        tiered_sync_worsts.push(worst);
    }

    let ring_med = median(&ring_rates);
    let tiered_med = median(&tiered_rates);
    let ring_worst = worst(&ring_worsts);
    let tiered_sync_worst = worst(&tiered_sync_worsts);

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

    let mut mixed_passes = Vec::with_capacity(DEEP_ITERS);
    let mut wrapall_passes = Vec::with_capacity(DEEP_ITERS);
    for _ in 0..DEEP_ITERS {
        mixed_passes.push(deep_pass(&mut deep_mixed_term));
        wrapall_passes.push(deep_pass(&mut deep_wrap_term));
    }
    let mixed = DeepSummary::of(&mixed_passes);
    let wrapall = DeepSummary::of(&wrapall_passes);
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
         \"resize_ring_worst_p50_ms\":{:.3},\"resize_tiered_sync_worst_p50_ms\":{:.3},\
         \"ring_lines\":{RING_LINES},\"tiered_fill_lines\":{TIERED_FILL_LINES},\
         \"tiered_depth\":{tiered_depth},\"n\":{N_ITERS},\"warmup\":{WARMUP},\
         \"deep_n\":{DEEP_ITERS},\
         \"deep_mixed_depth\":{deep_mixed_depth},\"deep_mixed_pump_total_ms\":{:.3},\
         \"deep_mixed_step_worst_ms\":{:.3},\"deep_mixed_sync_worst_ms\":{:.3},\
         \"deep_mixed_step_worst_p50_ms\":{:.3},\"deep_mixed_sync_worst_p50_ms\":{:.3},\
         \"deep_mixed_steps\":{},\"deep_wrapall_depth\":{deep_wrap_depth},\
         \"deep_wrapall_pump_total_ms\":{:.3},\"deep_wrapall_step_worst_ms\":{:.3},\
         \"deep_wrapall_step_worst_p50_ms\":{:.3},\
         \"converge_stale_width\":{converge_stale},\"converge_history_lines\":{converge_depth}}}",
        ring_worst * 1e3,
        tiered_sync_worst * 1e3,
        median(&ring_worsts) * 1e3,
        median(&tiered_sync_worsts) * 1e3,
        mixed.pump_total * 1e3,
        mixed.step_worst * 1e3,
        mixed.sync_worst * 1e3,
        mixed.step_worst_p50 * 1e3,
        mixed.sync_worst_p50 * 1e3,
        mixed.steps,
        wrapall.pump_total * 1e3,
        wrapall.step_worst * 1e3,
        wrapall.step_worst_p50 * 1e3,
    );
}

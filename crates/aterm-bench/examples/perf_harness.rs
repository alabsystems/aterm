// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Wall-clock throughput harness for the `verify perf` baseline: feeds a
// deterministic, representative VT workload through `Terminal::process` and
// prints a median-of-N throughput in MB/s as one JSON line. A release binary
// so timing reflects the shipped build; `verify perf` owns compare/threshold/record.
//
//   targo --unverified run --release -q -p aterm-bench --example perf_harness
//   -> {"median_mbps":3142.0,"min_mbps":...,"max_mbps":...,"workload_bytes":...,"n":...,"warmup":...}
//
// The workload knobs (WORKLOAD_BYTES, N_ITERS, WARMUP) are echoed in the JSON,
// which the `throughput` lane of `verify perf` records in
// crates/aterm-verify/perf/golden/perf-baseline.json.

use std::time::Instant;

use aterm_core::terminal::Terminal;

/// Approximate size of the synthesized workload, in bytes. "Tens of MB" so a
/// single iteration dominates timer granularity and scheduler jitter — large
/// enough that the median is dominated by steady-state engine throughput, not
/// setup/teardown. ~32 MiB.
const WORKLOAD_BYTES: usize = 32 << 20;

/// Number of timed iterations. The gate uses the median of these (not mean/min)
/// to reject outliers from a single scheduler hiccup. Odd so the median is a
/// single sample, not an average of two. N >= 5 per the lane requirement.
const N_ITERS: usize = 7;

/// Discarded warmup iterations run before timing (page faults, cache/branch-
/// predictor warmup, CPU frequency ramp). >= 1 per the lane requirement.
const WARMUP: usize = 2;

/// Grid the engine runs on. Small, fixed: scrolling is part of the workload, so a
/// short window exercises the scroll path heavily (representative of a real shell).
const ROWS: u16 = 24;
const COLS: u16 = 80;

/// The deterministic, representative VT workload (`aterm_bench::throughput_corpus`).
fn workload() -> Vec<u8> {
    aterm_bench::throughput_corpus(WORKLOAD_BYTES)
}

/// Process the whole corpus once on a fresh engine; return MB/s (decimal MB =
/// 1e6 bytes, the conventional throughput unit). The engine is rebuilt each
/// iteration so retained state never accumulates across timed runs.
fn one_iter_mbps(corpus: &[u8]) -> f64 {
    let mut term = Terminal::new(ROWS, COLS);
    let t0 = Instant::now();
    term.process(std::hint::black_box(corpus));
    let elapsed = t0.elapsed();
    // Defeat dead-code elimination of the work.
    std::hint::black_box(term.cursor().col);
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        // Timer granularity guard: an impossibly-fast read would divide by ~0.
        return f64::INFINITY;
    }
    (corpus.len() as f64 / 1.0e6) / secs
}

/// Median of a non-empty slice (sorts a copy; small N). For even len returns the
/// mean of the two middle samples — here N is odd so it is a single sample.
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

fn main() {
    let corpus = workload();

    // Warmup: discard.
    for _ in 0..WARMUP {
        let _ = one_iter_mbps(&corpus);
    }

    // Timed iterations.
    let mut samples = Vec::with_capacity(N_ITERS);
    for _ in 0..N_ITERS {
        samples.push(one_iter_mbps(&corpus));
    }

    let med = median(&samples);
    let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let max = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    // Single JSON line on stdout; everything else (human notes) to stderr so the
    // gate can parse stdout unambiguously.
    eprintln!(
        "perf_harness: {} timed iters over {:.1} MiB; median {:.1} MB/s (min {:.1}, max {:.1})",
        N_ITERS,
        corpus.len() as f64 / (1u64 << 20) as f64,
        med,
        min,
        max,
    );
    println!(
        "{{\"median_mbps\":{med:.3},\"min_mbps\":{min:.3},\"max_mbps\":{max:.3},\"workload_bytes\":{},\"n\":{N_ITERS},\"warmup\":{WARMUP}}}",
        corpus.len(),
    );
}

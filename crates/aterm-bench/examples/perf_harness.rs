// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Wall-clock THROUGHPUT harness for the `xtask gate perf` baseline (PERF-WALLCLOCK
// -BASELINE lane). Feeds a deterministic, sizeable, representative VT workload
// through the engine's parse/process hot path (`Terminal::process`) and reports a
// median-of-N throughput in MB/s as a single JSON line on stdout.
//
// This is a RELEASE binary on purpose: `xtask gate perf` spawns it with
// `cargo run --release` so timing reflects the shipped build, never the debug
// xtask interpreter. The gate owns the compare/threshold/record logic; this
// harness owns only the measurement (warmup, N timed iters, median).
//
//   cargo run --release -q -p aterm-bench --example perf_harness
//   -> {"median_mbps":3142.0,"min_mbps":...,"max_mbps":...,"workload_bytes":...,"n":...,"warmup":...}
//
// The constants below (WORKLOAD_BYTES, N_ITERS, WARMUP) are the workload knobs;
// they MUST match the metadata the gate records in tools/golden/perf-baseline.json
// (the gate reads them back from this harness's JSON, not from a duplicate).

use std::time::Instant;

use aterm_core::terminal::Terminal;

/// Approximate size of the synthesized workload, in bytes. "Tens of MB" so a
/// single iteration dominates timer granularity and scheduler jitter — large
/// enough that the median is dominated by steady-state engine throughput, not
/// setup/teardown. ~32 MiB.
const WORKLOAD_BYTES: usize = 32 << 20;

/// Number of TIMED iterations. The gate uses the MEDIAN of these (not mean/min)
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

/// Build a deterministic, representative VT workload: a repeating mix of plain
/// text, SGR colour/style runs, CSI cursor moves + erases, and newline-driven
/// scrolling. Deterministic byte-for-byte across machines (no RNG, no clock) so
/// the corpus — and therefore the work performed — is identical everywhere.
fn workload() -> Vec<u8> {
    // One "frame" of mixed traffic. Hand-built so the proportions are stable:
    //   - plain printable ASCII (the cheap path),
    //   - SGR runs (parser-heavy: params + dispatch),
    //   - CSI cursor positioning + erase-in-line (state mutation),
    //   - explicit newlines so the 24-row window scrolls constantly.
    let frame: &[&[u8]] = &[
        b"the quick brown fox jumps over the lazy dog 0123456789\r\n",
        b"\x1b[1;38;5;202mERROR\x1b[0m \x1b[4;48;5;19mwarning\x1b[0m normal text here\r\n",
        b"\x1b[2K\x1b[1Gredrawn line after erase-in-line and cursor-to-col-1\r\n",
        b"\x1b[7minverse\x1b[27m \x1b[3mitalic\x1b[23m \x1b[9mstrike\x1b[29m mixed sgr\r\n",
        b"\x1b[10;5Hpositioned via CUP \x1b[K then erased to end of line\r\n",
        b"plain throughput line with a tab\tand more ascii content padding 42\r\n",
        b"\x1b[38;2;120;200;255mtruecolor\x1b[0m run plus \x1b[48;2;10;10;10mbg\x1b[0m\r\n",
        b"\x1b[H\x1b[2Jfull clear then home, forcing a screen reset every frame\r\n",
    ];
    let unit: Vec<u8> = frame.concat();
    let mut out = Vec::with_capacity(WORKLOAD_BYTES + unit.len());
    while out.len() < WORKLOAD_BYTES {
        out.extend_from_slice(&unit);
    }
    out
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

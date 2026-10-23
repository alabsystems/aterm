// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

use aterm_core::terminal::Terminal;

const WORKLOAD_BYTES: usize = 32 << 20;
const N_ITERS: usize = 7;
const WARMUP: usize = 2;
const ROWS: u16 = 24;
const COLS: u16 = 80;

fn workload() -> Vec<u8> {
    aterm_bench::throughput_corpus(WORKLOAD_BYTES)
}

fn one_iter_mbps(corpus: &[u8]) -> f64 {
    let mut term = Terminal::new(ROWS, COLS);
    let t0 = Instant::now();
    term.process(std::hint::black_box(corpus));
    let elapsed = t0.elapsed();
    std::hint::black_box(term.cursor().col);
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return f64::INFINITY;
    }
    (corpus.len() as f64 / 1.0e6) / secs
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

fn main() {
    let corpus = workload();

    for _ in 0..WARMUP {
        let _ = one_iter_mbps(&corpus);
    }

    let mut samples = Vec::with_capacity(N_ITERS);
    for _ in 0..N_ITERS {
        samples.push(one_iter_mbps(&corpus));
    }

    let med = median(&samples);
    let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let max = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);

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

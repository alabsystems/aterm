// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// ASCII-cat throughput micro-harness (the ARENA-CAT shape): long printable
// prose lines (>64B runs) that exercise write_ascii_bulk_fast's >64 path.
// Median-of-N MB/s through Terminal::process. Scratch A/B tool.

use std::time::Instant;

use aterm_core::terminal::Terminal;

const ROWS: u16 = 24;
const COLS: u16 = 80;
const WORKLOAD_BYTES: usize = 32 << 20;
const N_ITERS: usize = 9;
const WARMUP: usize = 3;

fn workload() -> Vec<u8> {
    // Long printable prose lines (no 4+ identical runs) — the case the run-scan
    // skip targets — plus one indented line WITH a real space run so the
    // has-run path is also exercised, not just the fast reject.
    let frame: &[&[u8]] = &[
        b"the quick brown fox jumps over the lazy dog 0123456789 and then some more printable content to make a long ground-state run before the newline terminator arrives\r\n",
        b"another long line of purely printable characters abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ padding padding more content here to exceed sixty four bytes\r\n",
        b"                    indented code line with a genuine run of leading spaces then some trailing prose content to keep the line comfortably beyond the threshold\r\n",
    ];
    let unit: Vec<u8> = frame.concat();
    let mut out = Vec::with_capacity(WORKLOAD_BYTES + unit.len());
    while out.len() < WORKLOAD_BYTES {
        out.extend_from_slice(&unit);
    }
    out
}

fn one_iter_mbps(corpus: &[u8]) -> f64 {
    let mut term = Terminal::new(ROWS, COLS);
    let t0 = Instant::now();
    term.process(std::hint::black_box(corpus));
    let elapsed = t0.elapsed();
    std::hint::black_box(term.cursor().col);
    (corpus.len() as f64 / 1.0e6) / elapsed.as_secs_f64()
}

fn main() {
    let corpus = workload();
    for _ in 0..WARMUP {
        let _ = one_iter_mbps(&corpus);
    }
    let mut s: Vec<f64> = (0..N_ITERS).map(|_| one_iter_mbps(&corpus)).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = s[s.len() / 2];
    eprintln!(
        "ascii_throughput: median {:.1} MB/s (min {:.1}, max {:.1}, n={N_ITERS})",
        med,
        s[0],
        s[s.len() - 1]
    );
    println!("{{\"median_mbps\":{med:.3}}}");
}

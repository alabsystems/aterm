// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// SEARCH-CHURN harness (SA-2 / E2 terminal-side): prices the cost of a search
// AFTER content changed — the exact cost `search_harness`'s `build` lane pins
// as a FULL O(total-retained) rebuild (~459 ms at 50k lines per the module's
// own docs). With the incremental refresh in `Terminal::indexed_search`, a
// content-changing search costs O(appended rows + visible rows) — the churn —
// so this harness's per-search latency must scale with the APPEND BATCH, not
// with the corpus.
//
//   cargo run --release -q -p aterm-bench --example search_churn_harness
//   -> {"churn_search_qps":...,"full_rebuild_ms":...,"churn_speedup_x":...,
//       "churn_lines_per_batch":...,"corpus_lines":...,"n":...,"warmup":...}
//
// GUARDS (two-sided, the repo discipline — this workload proves its own reach
// or aborts, so the numbers can never quietly price the wrong path):
//   1. REACH: every churn-loop search must be served by the refresh arm
//      (`search_index_refreshes` advances by exactly one per search) and no
//      full rebuild may hide inside the loop
//      (`search_index_rebuilds - search_index_refreshes` constant across it).
//   2. IDENTITY: the final refreshed index's results are byte-identical to a
//      from-scratch rebuild over the same content (release + rebuild oracle) —
//      the same differential contract the in-crate tests pin per-step.
//
// Keys are INFORMATIONAL (not in xtask's SEARCH_LANE floor set): recording a
// floor requires a measured baseline (ATERM_PERF_RECORD), which this
// environment cannot produce honestly. The in-harness guards are the
// non-negotiable part; the owner can ratchet the keys into the lane after one
// recorded run. Same non-flake discipline as the sibling harnesses:
// deterministic corpus, median-of-N, no RNG/clock dependence.

use std::hint::black_box;
use std::time::Instant;

use aterm_bench::rotating_corpus;
use aterm_core::terminal::{Terminal, TerminalBuilder};

/// Same window/corpus shape as `search_harness` so the two lanes price the
/// same retained set.
const ROWS: u16 = 24;
const COLS: u16 = 80;
const RING_LINES: usize = 60_000;
const CORPUS_LINES: usize = 50_000;

/// Appended lines between successive searches — the churn variable. Small on
/// purpose: the pre-refresh behavior paid the SAME ~full-corpus rebuild for
/// this batch as for a million-line one, which is exactly the wrong-variable
/// scaling SA-2 closes.
const CHURN_LINES: usize = 100;

const N_ITERS: usize = 30;
const WARMUP: usize = 3;

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

fn results(term: &mut Terminal, query: &str) -> Vec<(usize, usize, usize)> {
    term.indexed_search()
        .search_results_opts(black_box(query), true, false)
        .expect("literal query is always valid")
        .matches
        .iter()
        .map(|m| (m.line, m.start_col, m.len()))
        .collect()
}

fn main() {
    let corpus = rotating_corpus(CORPUS_LINES);
    let mut term = TerminalBuilder::new()
        .size(ROWS, COLS)
        .ring_buffer_size(RING_LINES)
        .build();
    term.process(black_box(&corpus));

    // Prime the cache, then price ONE full rebuild for the speedup readout
    // (release drops the cache, so the next search pays the legacy path).
    let _ = results(&mut term, "jkl");
    term.release_search_index();
    let t0 = Instant::now();
    let _ = results(&mut term, "jkl");
    let full_rebuild_ms = t0.elapsed().as_secs_f64() * 1e3;
    assert_eq!(
        term.search_index_refreshes(),
        0,
        "REACH guard: a post-release miss must be a FULL rebuild (no cache to refresh)"
    );

    // Churn loop: append CHURN_LINES, search, repeat. Median per-search time.
    let mut churn_line = 0usize;
    let mut samples = Vec::with_capacity(N_ITERS);
    let full_rebuilds = |t: &Terminal| t.search_index_rebuilds() - t.search_index_refreshes();
    let fulls_before = full_rebuilds(&term);
    for iter in 0..(WARMUP + N_ITERS) {
        for _ in 0..CHURN_LINES {
            term.process(format!("churn {churn_line} appended jkl line\r\n").as_bytes());
            churn_line += 1;
        }
        let refreshes_before = term.search_index_refreshes();
        let t0 = Instant::now();
        let got = results(&mut term, "jkl");
        let secs = t0.elapsed().as_secs_f64();
        black_box(got.len());
        assert_eq!(
            term.search_index_refreshes(),
            refreshes_before + 1,
            "REACH guard: every churn search must be served by the incremental refresh"
        );
        if iter >= WARMUP && secs > 0.0 {
            samples.push(1.0 / secs);
        }
    }
    assert_eq!(
        full_rebuilds(&term),
        fulls_before,
        "REACH guard: no full O(total) rebuild may hide inside the churn loop"
    );

    // IDENTITY guard: the refreshed index equals a from-scratch rebuild over
    // the same final content (matches, order, coordinates).
    let refreshed = results(&mut term, "jkl");
    term.release_search_index();
    let rebuilt = results(&mut term, "jkl");
    assert_eq!(
        refreshed, rebuilt,
        "IDENTITY guard: refreshed results must be byte-identical to a rebuild"
    );

    let churn_search_qps = median(&samples);
    let churn_search_ms = if churn_search_qps > 0.0 {
        1e3 / churn_search_qps
    } else {
        f64::INFINITY
    };
    let churn_speedup_x = if churn_search_ms > 0.0 {
        full_rebuild_ms / churn_search_ms
    } else {
        f64::INFINITY
    };
    eprintln!(
        "search_churn_harness: full rebuild {full_rebuild_ms:.1} ms | churn search \
         {churn_search_ms:.2} ms ({churn_search_qps:.0} q/s at {CHURN_LINES} appended \
         lines/search) | speedup {churn_speedup_x:.0}x"
    );
    println!(
        "{{\"churn_search_qps\":{churn_search_qps:.3},\"full_rebuild_ms\":{full_rebuild_ms:.3},\
         \"churn_speedup_x\":{churn_speedup_x:.3},\"churn_lines_per_batch\":{CHURN_LINES},\
         \"corpus_lines\":{CORPUS_LINES},\"n\":{N_ITERS},\"warmup\":{WARMUP}}}"
    );
}

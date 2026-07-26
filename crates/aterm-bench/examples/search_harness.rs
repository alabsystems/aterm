// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// SEARCH-BENCH harness (E0): committed floors for the trigram search engine —
// the audit's "search had ZERO in-tree benchmarks" gap. Measures, per corpus:
//
//   build    full index build through the PRODUCT path (`Terminal::indexed_search`
//            after a content change — the all-or-nothing rebuild every live search
//            pays today), in thousand lines/s.
//   query    per-query cost on the CACHED index (unchanged content), queries/s.
//   memory   net heap RETAINED by the built index (counting global allocator),
//            reported bigger-is-better as lines-per-MiB for the gate, plus the
//            human-facing bytes/line.
//
// plus the engine's incremental primitive (`index_scrollback_line`, the E2
// destination) as `index_line_klps`, so the rework's before/after is measurable.
//
// TWO corpora, per the audit's rotating-alphabet caveat (§5.2): `rotating` is
// trigram-DIVERSE (worst case for the trigram map), `replog` is a repetitive
// service log — few templates, digits-only variation — where the trigram map
// shrinks but the dominant per-line postings/String costs do not. A regression
// that only shows on one shape cannot hide behind the other.
//
//   cargo run --release -q -p aterm-bench --example search_harness
//   -> {"rotating_build_klps":...,"rotating_query_qps":...,"rotating_lines_per_mib":...,
//       "replog_build_klps":...,"replog_query_qps":...,"replog_lines_per_mib":...,
//       "index_line_klps":...,"rotating_bytes_per_line":...,"replog_bytes_per_line":...,
//       "corpus_lines":...,"n":...,"warmup":...}
//
// Same non-flake discipline as the sibling harnesses: deterministic corpora
// (no RNG/clock), release subprocess, median-of-N, generous gate ratio.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

use aterm_core::search::TerminalSearch;
use aterm_core::terminal::{Terminal, TerminalBuilder};

/// Net-live-bytes counting allocator, the `tests/memory.rs` pattern: the
/// retained-index measurement needs REAL heap deltas, not estimates. Two relaxed
/// atomics per alloc are noise next to the index build's own allocation traffic.
static NET: AtomicI64 = AtomicI64::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        NET.fetch_add(l.size() as i64, Ordering::Relaxed);
        // SAFETY: forwarding to the System allocator with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        NET.fetch_sub(l.size() as i64, Ordering::Relaxed);
        // SAFETY: forwarding to the System allocator with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn net() -> i64 {
    NET.load(Ordering::Relaxed)
}

/// Small shell window; scrollback holds the corpus.
const ROWS: u16 = 24;
const COLS: u16 = 80;

/// Ring big enough to retain the whole corpus (ring-only, the shipping config —
/// the index covers every retained line, so retention IS the indexed set).
const RING_LINES: usize = 60_000;

/// Corpus height. 50k lines matches the product's scrollback cap and keeps a
/// full-rebuild iteration in the hundreds of milliseconds (the module's own doc
/// claims ~459 ms at this depth — this harness turns that claim into a floor).
const CORPUS_LINES: usize = 50_000;

/// Median-of-N + warmup: identical discipline to the other perf lanes. N=5 keeps
/// the whole harness (two corpora x full rebuilds) inside a few seconds.
const N_ITERS: usize = 5;
const WARMUP: usize = 1;

/// Queries per timed batch on the cached index (one search is sub-ms to tens of
/// ms depending on match count; a batch dominates timer granularity).
const QUERY_BATCH: usize = 20;

/// Shared deterministic corpora (aterm-bench lib): the floor lane and the
/// posting-container decision bench must measure the SAME bytes.
use aterm_bench::{linkheavy_corpus, replog_corpus, rotating_corpus};

/// LINK-HEAVY corpus height in LOGICAL lines (Wave-4A P7). Each logical line
/// soft-wraps to 2 grid rows at 80 columns, so 25k logical lines occupy ~50k
/// rows — the same indexed depth as the other corpora, fully retained inside
/// `RING_LINES` (no eviction: the lane measures indexing, not retention).
const LINKHEAVY_LINES: usize = 25_000;

/// Fill a fresh ring-only terminal (the SHIPPING config — no tiered store) with
/// the corpus, so the index covers scrollback + visible exactly as production.
fn filled_terminal(corpus: &[u8]) -> Terminal {
    let mut term = TerminalBuilder::new()
        .size(ROWS, COLS)
        .ring_buffer_size(RING_LINES)
        .build();
    term.process(black_box(corpus));
    term
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

/// One corpus's measured lane values.
struct CorpusReport {
    build_klps: f64,
    query_qps: f64,
    lines_per_mib: f64,
    bytes_per_line: f64,
    indexed_lines: usize,
    match_count: usize,
}

/// Measure one corpus. `query` must HIT (a no-match query would measure the
/// bloom/trigram early-out, not verification).
fn measure_corpus(corpus: &[u8], query: &str) -> CorpusReport {
    let mut term = filled_terminal(corpus);

    // -- retained index memory: net heap across the FIRST build (cache empty) --
    let before = net();
    let index = term.indexed_search();
    let indexed_lines = index.indexed_line_count();
    let retained = (net() - before).max(0) as f64;
    let bytes_per_line = retained / indexed_lines.max(1) as f64;
    let lines_per_mib = indexed_lines as f64 / (retained / (1024.0 * 1024.0)).max(1e-9);

    // -- full rebuild (the product path's per-content-change cost) --
    let mut build = Vec::with_capacity(N_ITERS);
    let rebuild_pass = |term: &mut Terminal| -> f64 {
        // Bump content_gen (one appended cell) so `indexed_search` MISSES and
        // pays the whole all-or-nothing rebuild — exactly what any live search
        // pays after any output arrives today.
        term.process(b"x");
        let t0 = Instant::now();
        let idx = term.indexed_search();
        let secs = t0.elapsed().as_secs_f64();
        black_box(idx.indexed_line_count());
        if secs <= 0.0 {
            return f64::INFINITY;
        }
        (indexed_lines as f64 / 1000.0) / secs
    };
    for _ in 0..WARMUP {
        let _ = rebuild_pass(&mut term);
    }
    for _ in 0..N_ITERS {
        build.push(rebuild_pass(&mut term));
    }

    // -- per-query cost on the cached index (content unchanged) --
    let mut match_count = 0usize;
    let mut query_pass = |term: &mut Terminal| -> f64 {
        let t0 = Instant::now();
        for _ in 0..QUERY_BATCH {
            let results = term
                .indexed_search()
                .search_results_opts(black_box(query), true, false)
                .expect("literal query is always a valid pattern");
            match_count = results.matches.len();
            black_box(&results);
        }
        let secs = t0.elapsed().as_secs_f64();
        if secs <= 0.0 {
            return f64::INFINITY;
        }
        QUERY_BATCH as f64 / secs
    };
    let mut queries = Vec::with_capacity(N_ITERS);
    for _ in 0..WARMUP {
        let _ = query_pass(&mut term);
    }
    for _ in 0..N_ITERS {
        queries.push(query_pass(&mut term));
    }

    CorpusReport {
        build_klps: median(&build),
        query_qps: median(&queries),
        lines_per_mib,
        bytes_per_line,
        indexed_lines,
        match_count,
    }
}

/// The incremental primitive (`index_scrollback_line`) in thousand lines/s —
/// the E2 destination's unit cost, measured on the rotating corpus so the
/// trigram map takes its worst-case shape.
fn measure_index_line(corpus: &[u8]) -> f64 {
    let lines: Vec<&str> = std::str::from_utf8(corpus)
        .expect("corpus is ASCII")
        .lines()
        .collect();
    let mut samples = Vec::with_capacity(N_ITERS);
    let pass = || -> f64 {
        let mut search = TerminalSearch::with_capacity(lines.len());
        let t0 = Instant::now();
        for line in &lines {
            search.index_scrollback_line(black_box(line));
        }
        let secs = t0.elapsed().as_secs_f64();
        black_box(search.indexed_line_count());
        if secs <= 0.0 {
            return f64::INFINITY;
        }
        (lines.len() as f64 / 1000.0) / secs
    };
    for _ in 0..WARMUP {
        let _ = pass();
    }
    for _ in 0..N_ITERS {
        samples.push(pass());
    }
    median(&samples)
}

fn main() {
    let rotating = rotating_corpus(CORPUS_LINES);
    let replog = replog_corpus(CORPUS_LINES);
    let linkheavy = linkheavy_corpus(LINKHEAVY_LINES);

    // "jkl" rides the rotating alphabet through most lines; "ERROR" is a real
    // log level hitting 1-in-8 replog lines. Both HIT (verification measured).
    let rot = measure_corpus(&rotating, "jkl");
    let rep = measure_corpus(&replog, "ERROR");
    // ".tgz" hits inside every logical line's visible URL (P7 lane).
    let link = measure_corpus(&linkheavy, "tgz");
    let index_line_klps = measure_index_line(&rotating);

    eprintln!(
        "search_harness: rotating — build {:.1} klines/s | {:.0} q/s ({} matches) | \
         {:.0} B/line ({} lines)",
        rot.build_klps, rot.query_qps, rot.match_count, rot.bytes_per_line, rot.indexed_lines,
    );
    eprintln!(
        "search_harness: replog   — build {:.1} klines/s | {:.0} q/s ({} matches) | \
         {:.0} B/line ({} lines)",
        rep.build_klps, rep.query_qps, rep.match_count, rep.bytes_per_line, rep.indexed_lines,
    );
    eprintln!(
        "search_harness: linkheavy — build {:.1} klines/s | {:.0} q/s ({} matches) | \
         {:.0} B/line ({} lines)",
        link.build_klps, link.query_qps, link.match_count, link.bytes_per_line, link.indexed_lines,
    );
    eprintln!("search_harness: index_scrollback_line primitive {index_line_klps:.1} klines/s");

    println!(
        "{{\"rotating_build_klps\":{:.3},\"rotating_query_qps\":{:.3},\
         \"rotating_lines_per_mib\":{:.3},\"replog_build_klps\":{:.3},\
         \"replog_query_qps\":{:.3},\"replog_lines_per_mib\":{:.3},\
         \"linkheavy_build_klps\":{:.3},\"linkheavy_query_qps\":{:.3},\
         \"linkheavy_lines_per_mib\":{:.3},\
         \"index_line_klps\":{:.3},\"rotating_bytes_per_line\":{:.1},\
         \"replog_bytes_per_line\":{:.1},\"linkheavy_bytes_per_line\":{:.1},\
         \"rotating_matches\":{},\"replog_matches\":{},\"linkheavy_matches\":{},\
         \"corpus_lines\":{CORPUS_LINES},\"n\":{N_ITERS},\"warmup\":{WARMUP}}}",
        rot.build_klps,
        rot.query_qps,
        rot.lines_per_mib,
        rep.build_klps,
        rep.query_qps,
        rep.lines_per_mib,
        link.build_klps,
        link.query_qps,
        link.lines_per_mib,
        index_line_klps,
        rot.bytes_per_line,
        rep.bytes_per_line,
        link.bytes_per_line,
        rot.match_count,
        rep.match_count,
        link.match_count,
    );
}

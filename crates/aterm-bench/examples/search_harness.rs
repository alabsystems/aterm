// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Benchmark harness for trigram search build throughput, cached query rate, and retained heap.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

use aterm_bench::{linkheavy_corpus, replog_corpus, rotating_corpus};
use aterm_core::search::TerminalSearch;
use aterm_core::terminal::{Terminal, TerminalBuilder};

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

const ROWS: u16 = 24;
const COLS: u16 = 80;
const RING_LINES: usize = 60_000;
const CORPUS_LINES: usize = 50_000;
const N_ITERS: usize = 5;
const WARMUP: usize = 1;
const QUERY_BATCH: usize = 20;
const LINKHEAVY_LINES: usize = 25_000;

fn filled_terminal(corpus: &[u8]) -> Terminal {
    let mut term = TerminalBuilder::new()
        .size(ROWS, COLS)
        .ring_buffer_size(RING_LINES)
        .build();
    term.process(black_box(corpus));
    term
}

fn filled_tiered_terminal(corpus: &[u8]) -> Terminal {
    let mut term = TerminalBuilder::new()
        .size(ROWS, COLS)
        .tiered_scrollback_defaults()
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

fn full_search_rebuilds(term: &Terminal) -> u64 {
    term.search_index_rebuilds()
        .checked_sub(term.search_index_refreshes())
        .expect("search refresh count cannot exceed total cache-miss rebuilds")
}

struct CorpusReport {
    build_klps: f64,
    query_qps: f64,
    lines_per_mib: f64,
    bytes_per_line: f64,
    indexed_lines: usize,
    match_count: usize,
}

fn measure_corpus(corpus: &[u8], query: &str) -> CorpusReport {
    measure_terminal(filled_terminal(corpus), query)
}

fn measure_terminal(mut term: Terminal, query: &str) -> CorpusReport {
    let before = net();
    let index = term.indexed_search();
    let indexed_lines = index.indexed_line_count();
    let retained = (net() - before).max(0) as f64;
    let bytes_per_line = retained / indexed_lines.max(1) as f64;
    let lines_per_mib = indexed_lines as f64 / (retained / (1024.0 * 1024.0)).max(1e-9);

    let mut build = Vec::with_capacity(N_ITERS);
    let rebuild_pass = |term: &mut Terminal| -> f64 {
        term.process(b"x");
        term.release_search_index();
        let refreshes_before = term.search_index_refreshes();
        let full_rebuilds_before = full_search_rebuilds(term);
        let t0 = Instant::now();
        let idx = term.indexed_search();
        let secs = t0.elapsed().as_secs_f64();
        let rebuilt_lines = idx.indexed_line_count();
        black_box(rebuilt_lines);
        assert_eq!(
            term.search_index_refreshes(),
            refreshes_before,
            "REACH guard: a cacheless search must not report an incremental refresh"
        );
        assert_eq!(
            full_search_rebuilds(term),
            full_rebuilds_before + 1,
            "REACH guard: every timed pass must execute exactly one full rebuild"
        );
        assert_eq!(
            rebuilt_lines, indexed_lines,
            "full rebuild must cover the same retained row set as the priming build"
        );
        if secs <= 0.0 {
            return f64::INFINITY;
        }
        (rebuilt_lines as f64 / 1000.0) / secs
    };
    for _ in 0..WARMUP {
        let _ = rebuild_pass(&mut term);
    }
    for _ in 0..N_ITERS {
        build.push(rebuild_pass(&mut term));
    }

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

    let rot = measure_corpus(&rotating, "jkl");
    let rep = measure_corpus(&replog, "ERROR");
    let link = measure_corpus(&linkheavy, "tgz");
    let index_line_klps = measure_index_line(&rotating);
    let tiered = measure_terminal(filled_tiered_terminal(&rotating), "jkl");

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
    eprintln!(
        "search_harness: tiered rotating — build {:.1} klines/s ({} lines, informational)",
        tiered.build_klps, tiered.indexed_lines,
    );

    println!(
        "{{\"rotating_build_klps\":{:.3},\"rotating_query_qps\":{:.3},\
         \"rotating_lines_per_mib\":{:.3},\"replog_build_klps\":{:.3},\
         \"replog_query_qps\":{:.3},\"replog_lines_per_mib\":{:.3},\
         \"linkheavy_build_klps\":{:.3},\"linkheavy_query_qps\":{:.3},\
         \"linkheavy_lines_per_mib\":{:.3},\
         \"index_line_klps\":{:.3},\"rotating_bytes_per_line\":{:.1},\
         \"replog_bytes_per_line\":{:.1},\"linkheavy_bytes_per_line\":{:.1},\
         \"rotating_matches\":{},\"replog_matches\":{},\"linkheavy_matches\":{},\
         \"tiered_build_klps\":{:.3},\
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
        tiered.build_klps,
    );
}

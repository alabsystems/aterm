// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// INCREMENTAL-TYPING harness (SA-1): prices the per-KEYSTROKE cost of an
// interactive find, the workload `search_harness`'s whole-query lane cannot
// see. The find bar re-runs its query on every text edit; the stateless batch
// layer paid a full range scan (1–2-char literals) or a fresh posting-list
// decode + intersection (longer ones) plus an up-to-100k match
// materialization PER KEYSTROKE. The narrowing layer
// (`TerminalSearch::search_literal_narrowed` + the GUI's `NarrowSession`
// stack) verifies only the previous keystroke's occurrence frame instead.
//
// Two deterministic corpora at 100k lines (the default retention cap — the
// depth the finding prices): trigram-diverse `rotating` typing "jklm"
// (~61k/59k/58k/56k matches per prefix — every step uncapped, so the 1-char
// full-range-scan step SEEDS and every later step narrows), and the
// repetitive `replog` typing "ERROR" (~62k capital E's, then ~12.5k "ER.."
// lines). The capped-set fallback regime is pinned by the engine battery in
// `aterm-search`; this harness prices the uncapped narrowing regime the
// finding names.
//
//   cargo run --release -q -p aterm-bench --example search_typing_harness
//   -> {"typing_batch_kps":...,"typing_narrowed_kps":...,"typing_speedup_x":...,
//       "typing_framed_steps":...,"corpus_lines":...,"n":...,"warmup":...}
//
// GUARDS (two-sided, the repo discipline — this workload proves its own reach
// or aborts, so the numbers can never quietly price the wrong path):
//   1. IDENTITY: at EVERY keystroke, the narrowed lane's results are
//      byte-identical to the batch lane's forward results — the same
//      differential contract `aterm-search`'s narrowing battery pins.
//   2. REACH: for each query the framed-step count is exactly len-1 — the
//      1-char step seeds (guarded uncapped, so the seed is not vacuous) and
//      EVERY later step runs off a frame — proving the lane prices
//      narrowing, never a silent batch fallback.
//
// Excluded honestly: the GUI-side `map_matches` remap (aterm-gui is not a
// bench dependency); it is O(matches) in both lanes, so its exclusion cannot
// flip the comparison. Keys are INFORMATIONAL (not in xtask's SEARCH_LANE
// floor set) — recording floors needs a measured baseline; the in-harness
// guards are the non-negotiable part. Same non-flake discipline as the
// sibling harnesses: deterministic corpora, median-of-N, no RNG/clock.

use std::hint::black_box;
use std::time::Instant;

use aterm_bench::{replog_corpus, rotating_corpus};
use aterm_core::search::{MAX_SEARCH_MATCHES, SearchDirection, TerminalSearch};

/// The finding's depth: the default retention cap.
const CORPUS_LINES: usize = 100_000;

const N_ITERS: usize = 9;
const WARMUP: usize = 2;

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

fn build_index(corpus: &[u8]) -> TerminalSearch {
    let mut search = TerminalSearch::with_capacity(CORPUS_LINES);
    for line in std::str::from_utf8(corpus)
        .expect("corpus is ASCII")
        .lines()
    {
        search.index_scrollback_line(black_box(line));
    }
    search
}

/// One narrowed typing pass: the GUI's frame discipline (seed on the first
/// uncapped step, narrow every extension). Returns (seconds, framed_steps);
/// with `check` set it also asserts per-keystroke identity with the batch
/// lane (run outside the timed passes so timing prices narrowing alone).
fn narrowed_pass(index: &TerminalSearch, query: &str, check: bool) -> (f64, usize) {
    let mut frame: Option<Vec<u32>> = None;
    let mut framed_steps = 0usize;
    let mut prefix = String::new();
    let t0 = Instant::now();
    for c in query.chars() {
        prefix.push(c);
        if frame.is_some() {
            framed_steps += 1;
        }
        let step = index.search_literal_narrowed(&prefix, true, frame.as_deref());
        black_box(step.results.matches.len());
        if check {
            let batch = index
                .search_results_opts_direction(&prefix, true, false, SearchDirection::Forward)
                .expect("literal query is always valid");
            assert_eq!(
                step.results, batch,
                "IDENTITY guard: narrowed keystroke {prefix:?} must equal the batch lane"
            );
        }
        frame = step.occurrence_lines;
    }
    (t0.elapsed().as_secs_f64(), framed_steps)
}

/// One batch typing pass: the pre-SA-1 per-keystroke cost.
fn batch_pass(index: &TerminalSearch, query: &str) -> f64 {
    let mut prefix = String::new();
    let t0 = Instant::now();
    for c in query.chars() {
        prefix.push(c);
        let results = index
            .search_results_opts_direction(&prefix, true, false, SearchDirection::Forward)
            .expect("literal query is always valid");
        black_box(results.matches.len());
    }
    t0.elapsed().as_secs_f64()
}

fn main() {
    let corpora: [(&str, Vec<u8>, &str); 2] = [
        ("rotating", rotating_corpus(CORPUS_LINES), "jklm"),
        ("replog", replog_corpus(CORPUS_LINES), "ERROR"),
    ];
    let indexes: Vec<(&str, TerminalSearch, &str)> = corpora
        .iter()
        .map(|(name, corpus, query)| (*name, build_index(corpus), *query))
        .collect();

    let total_keystrokes: usize = indexes.iter().map(|(_, _, q)| q.chars().count()).sum();
    let mut framed_total = 0usize;
    for (name, index, query) in &indexes {
        let (_, framed) = narrowed_pass(index, query, true);
        let expected = query.chars().count().saturating_sub(1);
        assert_eq!(
            framed, expected,
            "REACH guard ({name}): the 1-char step must seed and every later \
             step must run off a frame"
        );
        // The 1-char prefix stays UNDER the cap (deterministic corpus
        // property), so the seed frame really exists and the framed count
        // above is not vacuous.
        let one = &query[..1];
        let seed = index
            .search_results_opts_direction(one, true, false, SearchDirection::Forward)
            .expect("literal query is always valid");
        assert!(
            seed.matches.len() < MAX_SEARCH_MATCHES,
            "REACH guard ({name}): the {one:?} seed step must be uncapped \
             ({} matches)",
            seed.matches.len()
        );
        framed_total += framed;
    }

    let mut batch_samples = Vec::with_capacity(N_ITERS);
    let mut narrowed_samples = Vec::with_capacity(N_ITERS);
    for iter in 0..(WARMUP + N_ITERS) {
        let mut batch_secs = 0.0;
        let mut narrowed_secs = 0.0;
        for (_, index, query) in &indexes {
            batch_secs += batch_pass(index, query);
            narrowed_secs += narrowed_pass(index, query, false).0;
        }
        if iter >= WARMUP {
            if batch_secs > 0.0 {
                batch_samples.push(total_keystrokes as f64 / batch_secs);
            }
            if narrowed_secs > 0.0 {
                narrowed_samples.push(total_keystrokes as f64 / narrowed_secs);
            }
        }
    }
    let typing_batch_kps = median(&batch_samples);
    let typing_narrowed_kps = median(&narrowed_samples);
    let typing_speedup_x = if typing_batch_kps > 0.0 {
        typing_narrowed_kps / typing_batch_kps
    } else {
        f64::INFINITY
    };
    eprintln!(
        "search_typing_harness: batch {typing_batch_kps:.1} keystrokes/s | narrowed \
         {typing_narrowed_kps:.1} keystrokes/s | speedup {typing_speedup_x:.1}x \
         ({framed_total} framed steps over {total_keystrokes} keystrokes, {CORPUS_LINES} lines)"
    );
    println!(
        "{{\"typing_batch_kps\":{typing_batch_kps:.3},\
         \"typing_narrowed_kps\":{typing_narrowed_kps:.3},\
         \"typing_speedup_x\":{typing_speedup_x:.3},\
         \"typing_framed_steps\":{framed_total},\
         \"corpus_lines\":{CORPUS_LINES},\"n\":{N_ITERS},\"warmup\":{WARMUP}}}"
    );
}

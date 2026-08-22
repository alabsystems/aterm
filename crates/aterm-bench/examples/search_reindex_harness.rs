// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// CHANGED-ROW RE-INDEX harness (SA-3): prices the one indexing path the floor
// lane never touches — `SearchIndex::index_line` over a row that ALREADY holds
// DIFFERENT text. That is the per-search cost of every genuinely edited
// visible row (the echoed prompt line, a partial last row, a TUI repaint)
// whenever a find bar or the socket `search` verb runs over deep scrollback
// while output streams.
//
// Two lanes, deliberately:
//   edit    the REALISTIC shape — the newest 50 rows re-indexed with a small
//           tail edit (a counter suffix), so old and new text share almost
//           every trigram. The set-diff re-index touches only the delta.
//   rewrite the WORST CASE — the same 50 rows replaced with disjoint text, so
//           nearly every trigram membership genuinely changes and the
//           O(posting-list) container cost is paid in full. Keeping this lane
//           honest pins that the set-diff did not slow the case it cannot
//           help.
//
//   cargo run --release -q -p aterm-bench --example search_reindex_harness
//   -> {"reindex_edit_rows_ps":...,"reindex_rewrite_rows_ps":...,
//       "reindex_edit_vs_rewrite_x":...,"corpus_lines":...,"n":...,"warmup":...}
//
// GUARDS (two-sided): after all timed passes the mutated index must be
// RESULT-identical to a from-scratch build over the final texts (probe
// battery: shared, edit-suffix, removed and never-present content) — the same
// oracle contract the in-crate SA-3 battery pins per-step. Keys are
// INFORMATIONAL (not in xtask's SEARCH_LANE floor set): floors need a
// measured baseline; the guards are the non-negotiable part.

use std::hint::black_box;
use std::time::Instant;

use aterm_bench::rotating_corpus;
use aterm_core::search::SearchIndex;

/// Depth at the default retention cap: posting lists for common trigrams
/// approach the full corpus, which is exactly the O(list) variable the
/// changed-row path multiplies into.
const CORPUS_LINES: usize = 100_000;

/// Newest rows mutated per pass — the visible screen plus headroom.
const CHANGED_ROWS: usize = 50;

const N_ITERS: usize = 15;
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

fn corpus_lines(corpus: &[u8]) -> Vec<String> {
    std::str::from_utf8(corpus)
        .expect("corpus is ASCII")
        .lines()
        .map(str::to_string)
        .collect()
}

fn build_index(lines: &[String]) -> SearchIndex {
    let mut index = SearchIndex::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        index.index_line(i, line);
    }
    index
}

/// Tail-edited variant of a base row: shares every trigram except the suffix.
fn edited(base: &str, generation: usize) -> String {
    format!("{base} g{generation}")
}

/// Disjoint rewrite of a row: shares (almost) no trigram with the base.
fn rewritten(row: usize, generation: usize) -> String {
    format!("~~{generation:06}~~{row:06}~~ ####")
}

fn main() {
    let lines = corpus_lines(&rotating_corpus(CORPUS_LINES));
    let first_changed = CORPUS_LINES - CHANGED_ROWS;

    // --- edit lane: realistic tail edits ---
    let mut index = build_index(&lines);
    let mut generation = 0usize;
    let mut edit_samples = Vec::with_capacity(N_ITERS);
    for iter in 0..(WARMUP + N_ITERS) {
        generation += 1;
        let t0 = Instant::now();
        // Sub-slice + enumerate rather than a range loop over an index: same
        // rows, same order, O(1) to start, and no `needless_range_loop`.
        for (offset, base) in lines[first_changed..CORPUS_LINES].iter().enumerate() {
            index.index_line(first_changed + offset, &edited(base, generation));
        }
        let secs = t0.elapsed().as_secs_f64();
        black_box(index.len());
        if iter >= WARMUP && secs > 0.0 {
            edit_samples.push(CHANGED_ROWS as f64 / secs);
        }
    }
    // IDENTITY guard (edit lane): equals a from-scratch build of final texts.
    let mut final_lines = lines.clone();
    for (target, base) in final_lines[first_changed..CORPUS_LINES]
        .iter_mut()
        .zip(&lines[first_changed..CORPUS_LINES])
    {
        *target = edited(base, generation);
    }
    let fresh = build_index(&final_lines);
    let newest_edit = format!("g{generation}");
    for query in ["jkl", newest_edit.as_str(), "g1", "zzz-not-present"] {
        let a = index
            .search_results_opts(query, true, false)
            .expect("literal query is always valid");
        let b = fresh
            .search_results_opts(query, true, false)
            .expect("literal query is always valid");
        assert_eq!(
            a, b,
            "IDENTITY guard (edit lane): {query:?} must match a from-scratch build"
        );
    }
    // REACH guard: the edits really replaced text — the previous generation's
    // suffix is gone, the current one present on every changed row.
    assert_eq!(
        index
            .search_results_opts(newest_edit.as_str(), true, false)
            .expect("valid")
            .matches
            .len(),
        CHANGED_ROWS,
        "REACH guard: every changed row must carry the newest edit"
    );

    // --- rewrite lane: worst-case disjoint replacements ---
    let mut index = build_index(&lines);
    let mut generation = 0usize;
    let mut rewrite_samples = Vec::with_capacity(N_ITERS);
    for iter in 0..(WARMUP + N_ITERS) {
        generation += 1;
        let t0 = Instant::now();
        for row in first_changed..CORPUS_LINES {
            index.index_line(row, &rewritten(row, generation));
        }
        let secs = t0.elapsed().as_secs_f64();
        black_box(index.len());
        if iter >= WARMUP && secs > 0.0 {
            rewrite_samples.push(CHANGED_ROWS as f64 / secs);
        }
    }
    let mut final_lines = lines.clone();
    for (offset, target) in final_lines[first_changed..CORPUS_LINES]
        .iter_mut()
        .enumerate()
    {
        *target = rewritten(first_changed + offset, generation);
    }
    let fresh = build_index(&final_lines);
    let newest_rewrite = format!("~~{generation:06}");
    for query in ["jkl", "####", newest_rewrite.as_str(), "zzz-not-present"] {
        let a = index
            .search_results_opts(query, true, false)
            .expect("literal query is always valid");
        let b = fresh
            .search_results_opts(query, true, false)
            .expect("literal query is always valid");
        assert_eq!(
            a, b,
            "IDENTITY guard (rewrite lane): {query:?} must match a from-scratch build"
        );
    }

    let reindex_edit_rows_ps = median(&edit_samples);
    let reindex_rewrite_rows_ps = median(&rewrite_samples);
    let reindex_edit_vs_rewrite_x = if reindex_rewrite_rows_ps > 0.0 {
        reindex_edit_rows_ps / reindex_rewrite_rows_ps
    } else {
        f64::INFINITY
    };
    eprintln!(
        "search_reindex_harness: tail-edit {reindex_edit_rows_ps:.0} rows/s | disjoint \
         rewrite {reindex_rewrite_rows_ps:.0} rows/s | edit/rewrite {reindex_edit_vs_rewrite_x:.1}x \
         ({CHANGED_ROWS} newest rows over {CORPUS_LINES} lines)"
    );
    println!(
        "{{\"reindex_edit_rows_ps\":{reindex_edit_rows_ps:.3},\
         \"reindex_rewrite_rows_ps\":{reindex_rewrite_rows_ps:.3},\
         \"reindex_edit_vs_rewrite_x\":{reindex_edit_vs_rewrite_x:.3},\
         \"corpus_lines\":{CORPUS_LINES},\"changed_rows\":{CHANGED_ROWS},\
         \"n\":{N_ITERS},\"warmup\":{WARMUP}}}"
    );
}

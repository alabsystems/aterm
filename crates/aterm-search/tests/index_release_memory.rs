// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// REAL heap-delta verification for SearchIndex::release / TerminalSearch::release.
// A logical `clear()` empties the containers but a
// HashMap/Vec RETAINS its grown capacity (and the bloom keeps its bit array), so
// a cleared idle document still pins its peak footprint. `release()` must return
// that capacity to the allocator. "Logical clears insufficient" is not provable
// by asserting emptiness — it needs a REAL net-heap measurement, so this test
// installs a counting global allocator (the mem_budget / search_harness
// pattern) and asserts the live-byte delta, not a logical line count.
//
// ONE test function on purpose: the counting allocator is a single global
// counter, so a second #[test] running in parallel would pollute every net()
// reading. All scenarios run sequentially here.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use aterm_search::{BudgetedSearch, SearchIndex, TerminalSearch};

static NET: AtomicI64 = AtomicI64::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        NET.fetch_add(l.size() as i64, Ordering::Relaxed);
        ALLOCATED.fetch_add(l.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        NET.fetch_sub(l.size() as i64, Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn net() -> i64 {
    NET.load(Ordering::Relaxed)
}

/// Distinct, trigram-diverse lines so the postings, line Strings, column maps
/// and bloom all grow to a substantial footprint.
const LINES: usize = 20_000;

fn build(n: usize) -> TerminalSearch {
    let mut ts = TerminalSearch::with_capacity(n);
    for i in 0..n {
        // Rotate content so trigrams and posting lists are non-trivial.
        ts.index_scrollback_line(&format!(
            "row {i:06} svc-worker-{} path=/var/log/app-{:04}.log status={}",
            i % 37,
            i % 900,
            i % 13
        ));
    }
    ts
}

#[test]
fn release_reclaims_real_heap_that_clear_retains_without_disturbing_siblings() {
    // -------- (1) clear path: live bytes retained after a LOGICAL clear -----
    let base_clear = net();
    let mut idx_clear = build(LINES);
    let grown_clear = (net() - base_clear).max(0);
    idx_clear.clear();
    let retained_after_clear = (net() - base_clear).max(0);
    assert_eq!(idx_clear.indexed_line_count(), 0, "clear empties logically");
    drop(idx_clear);

    // -------- (2) release path: live bytes retained after a REAL release ----
    let base_rel = net();
    let mut idx_rel = build(LINES);
    let grown_rel = (net() - base_rel).max(0);
    idx_rel.release();
    let retained_after_release = (net() - base_rel).max(0);
    assert_eq!(
        idx_rel.indexed_line_count(),
        0,
        "release empties logically too"
    );
    drop(idx_rel);

    eprintln!(
        "release_memory: grown≈{grown_clear} clear-retains≈{retained_after_clear} \
         release-retains≈{retained_after_release} (bytes, N={LINES})",
    );

    // The index genuinely grew (guards against a vacuous test).
    assert!(
        grown_clear > 1_000_000 && grown_rel > 1_000_000,
        "index should grow past 1 MiB for {LINES} lines \
         (grown_clear={grown_clear}, grown_rel={grown_rel})",
    );

    // Real reclamation: after release the live heap is a small fraction of the
    // grown footprint — the allocations were RETURNED, not just emptied.
    assert!(
        retained_after_release * 5 < grown_rel,
        "release must free the bulk (>80%) of the grown heap: \
         retained {retained_after_release} of {grown_rel}",
    );

    // Logical clears are insufficient: a cleared index pins strictly — and
    // substantially — more live heap than a released one (retained bloom bit
    // array + HashMap spines). If this ever fails, `clear()` started freeing
    // capacity (fine, revisit) or `release()` stopped freeing (the regression
    // this guards).
    assert!(
        retained_after_clear > retained_after_release * 2,
        "clear must retain materially more than release \
         (clear={retained_after_clear}, release={retained_after_release}) — \
         else 'release' reclaims nothing extra over 'clear'",
    );

    // -------- (3) alt-doc residency: releasing A leaves B resident ----------
    let mut a = build(LINES);
    let b = build(LINES);
    let hits_before = b.search("svc-worker").len();
    assert!(
        hits_before > 0,
        "sibling B should have matches before release"
    );

    let before_release = net();
    a.release();
    let freed = (before_release - net()).max(0);
    assert_eq!(a.indexed_line_count(), 0);

    // Releasing A freed most of A's footprint...
    assert!(
        freed > 1_000_000,
        "releasing A should free a real chunk of heap (freed={freed})",
    );
    // ...and B is untouched: same residency, same answers.
    assert_eq!(b.indexed_line_count(), LINES, "B stays fully indexed");
    assert_eq!(
        b.search("svc-worker").len(),
        hits_before,
        "B returns identical results after A is released",
    );
    drop((a, b));

    // -------- (4) budgeted scans retain matches, not already-read rows ------
    // A no-match Unicode workload isolates text/map retention from the bounded
    // result vector. Measure with the engine still live, both mid-scan and
    // after completion, as Terminal keeps a completed scan for summary reads.
    // A batch index over the same rows is the historical-retention control.
    const SCAN_ROWS: usize = 2048;
    const SCRATCH_ALLOWANCE: i64 = 256 * 1024;
    let row = "e\u{301} 日本語 👩🏽‍💻 ".repeat(32);
    for (query, case_sensitive, is_regex) in [
        ("absent", true, false),
        ("ABSENT", false, false),
        ("absent[0-9]+", true, true),
        ("ABSENT[0-9]+", false, true),
    ] {
        let mut search = BudgetedSearch::new(query, case_sensitive, is_regex, 37, SCAN_ROWS)
            .expect("bounded search construction");
        // Compilation is deliberately outside the measurement; one-row
        // matcher scratch may persist, but a growing row cache must not.
        let base = net();
        for i in 0..SCAN_ROWS {
            search.feed_row_owned(row.clone());
            if i + 1 == SCAN_ROWS / 2 {
                let retained = net() - base;
                assert!(
                    retained < SCRATCH_ALLOWANCE,
                    "in-flight scan retained {retained} bytes for already-read rows: \
                     cs={case_sensitive} rx={is_regex}"
                );
            }
        }
        let retained = net() - base;
        assert!(search.is_complete());
        assert!(search.results().matches.is_empty());
        assert!(
            retained < SCRATCH_ALLOWANCE,
            "completed scan retained {retained} bytes beyond matcher scratch: \
             cs={case_sensitive} rx={is_regex}"
        );
    }

    let mut retained_index = SearchIndex::new();
    let base_index = net();
    for i in 0..SCAN_ROWS {
        retained_index.index_line(37 + i, &row);
    }
    let retained = net() - base_index;
    assert!(
        retained_index
            .search_results_opts("absent", true, false)
            .expect("negative-control query")
            .matches
            .is_empty()
    );
    assert!(
        retained > 4 * SCRATCH_ALLOWANCE,
        "negative control must retain enough row data to fail the scan bound: {retained} bytes"
    );
    // Text alone exceeds the allowance, independently of batch-only postings:
    // a scan that kept only rows' column maps would fail the same bound.
    assert!(row.len() * SCAN_ROWS > 4 * SCRATCH_ALLOWANCE as usize);

    // -------- (5) screen refreshes do not inflate the filter's live heap ---
    // One wide row isolates filter growth from line and posting allocations.
    // Counting duplicate trigram inserts per refresh would grow the filter by
    // hundreds of KiB.
    let wide = "compiler_worker emitted another build diagnostic ".repeat(100);
    let mut refreshed = SearchIndex::new();
    refreshed.index_line(0, &wide);
    let before_refresh = net();
    for _ in 0..1000 {
        refreshed.index_line(0, &wide);
    }
    let refresh_growth = net() - before_refresh;
    assert!(
        refresh_growth < 16 * 1024,
        "unchanged screen refreshes grew live heap by {refresh_growth} bytes"
    );
    assert_eq!(refreshed.search_with_positions("diagnostic").len(), 100);

    // -------- (6) retained row cap also bounds constructor reservations ----
    // A very large expected history must not reserve its full hash-table
    // spines when only a tiny suffix can ever be retained.
    let before_capped = net();
    let capped = SearchIndex::with_capacity_and_max(1_000_000, 32);
    let capped_bytes = net() - before_capped;
    assert!(
        capped_bytes < 32 * 1024,
        "32-row cache reserved {capped_bytes} bytes for evicted history"
    );
    drop(capped);

    // Allocation control: the full million-row tables an uncapped reservation
    // asks for. No content is allocated, and it is released before the next
    // assertion.
    let before_full_reservation = net();
    let full_reservation = SearchIndex::with_capacity_and_max(1_000_000, 1_000_000);
    let full_bytes = net() - before_full_reservation;
    drop(full_reservation);
    assert!(full_bytes > capped_bytes * 100);
    eprintln!(
        "search_cache_bounds: unchanged_refresh_growth={refresh_growth} \
         capped_32_rows={capped_bytes} uncapped_million_row_reservation={full_bytes} bytes"
    );

    // -------- (7) a full history's front eviction does not decode survivors -
    // One shared trigram isolates posting maintenance while the real index
    // drops and appends rows. Count GROSS allocation: decode scratch would be
    // freed immediately and so is invisible to net-heap readings.
    const HISTORY_ROWS: usize = 10_000;
    const ADVANCES: usize = 256;
    let mut moving = SearchIndex::with_capacity_and_max(HISTORY_ROWS, HISTORY_ROWS);
    for row in 0..HISTORY_ROWS {
        moving.index_line(row, "aaa");
    }
    let before_allocated = ALLOCATED.load(Ordering::Relaxed);
    let before_live = net();
    for step in 0..ADVANCES {
        moving.drop_history_below(step + 1);
        moving.index_line(step + HISTORY_ROWS, "aaa");
    }
    let eviction_allocated = ALLOCATED.load(Ordering::Relaxed) - before_allocated;
    let eviction_growth = net() - before_live;
    assert!(
        eviction_allocated < 256 * 1024,
        "{ADVANCES} one-row history evictions allocated {eviction_allocated} bytes"
    );
    assert!(eviction_growth < 64 * 1024);
    assert_eq!(moving.search_with_positions("aaa").len(), HISTORY_ROWS);
    assert!(!moving.results_may_be_incomplete());

    // Allocation control: decoding even ONE posting to a Vec allocates every
    // retained u32 ID per step. It omits re-encoding and text costs, so it is
    // a conservative measure of the traffic a decoding eviction would cost.
    let before_old = ALLOCATED.load(Ordering::Relaxed);
    for step in 0..ADVANCES as u32 {
        let decoded: Vec<u32> = (step..step + HISTORY_ROWS as u32).collect();
        std::hint::black_box(decoded);
    }
    let old_decode_allocated = ALLOCATED.load(Ordering::Relaxed) - before_old;
    assert_eq!(old_decode_allocated, (ADVANCES * HISTORY_ROWS * 4) as u64);
    assert!(old_decode_allocated > eviction_allocated * 20);
    eprintln!(
        "history_eviction_allocation: rows={HISTORY_ROWS} steps={ADVANCES} \
         allocated={eviction_allocated} live_growth={eviction_growth} \
         historical_decode_only={old_decode_allocated} bytes"
    );
}

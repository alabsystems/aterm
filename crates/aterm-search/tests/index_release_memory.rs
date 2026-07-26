// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// REAL heap-delta verification for SearchIndex::release / TerminalSearch::release
// (Wave-4A prescription a). A logical `clear()` empties the containers but a
// HashMap/Vec RETAINS its grown capacity (and the bloom keeps its bit array), so
// a cleared idle document still pins its peak footprint. `release()` must return
// that capacity to the allocator. "Logical clears insufficient" is not provable
// by asserting emptiness — it needs a REAL net-heap measurement, so this test
// installs a counting global allocator (the tests/memory.rs / search_harness
// pattern) and asserts the live-byte delta, not a logical line count.
//
// ONE test function on purpose: the counting allocator is a single global
// counter, so a second #[test] running in parallel would pollute every net()
// reading. All scenarios run sequentially here.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, Ordering};

use aterm_search::TerminalSearch;

static NET: AtomicI64 = AtomicI64::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        NET.fetch_add(l.size() as i64, Ordering::Relaxed);
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
    assert_eq!(idx_rel.indexed_line_count(), 0, "release empties logically too");
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
    assert!(hits_before > 0, "sibling B should have matches before release");

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
}

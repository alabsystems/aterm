// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Engine complexity guard (M2 PERF-BASELINE) — DETERMINISTIC, machine-independent.
//!
//! Wall-clock throughput is flaky on shared/throttled machines, so the regression
//! gate here measures ALLOCATION COUNT instead: a quantity that is reproducible run
//! to run and identical across machines. The engine writes into a fixed grid with a
//! capped (ring-buffer) scrollback, so the number of heap allocations to process a
//! stream must stay ~constant — NOT grow proportionally with the input. A regression
//! that allocates per input byte / per line (an accidental per-cell box, a grid
//! realloc per scroll) shows up as super-linear allocation growth and fails here.
//!
//! This complements the parser's O(n) iteration-counter guard
//! (aterm-parser performance tests) and the retained-heap ceiling (mem_budget.rs).
//! Run: `cargo test -p aterm-core --test perf_scaling -- --nocapture`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);

/// System allocator that counts allocation CALLS while `ACTIVE` (so only the
/// measured `process()` span is counted, not test/grid setup).
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarding to System with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: forwarding to System with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// `lines` rows of ASCII + a little SGR, each ending in `\r\n` so the stream
/// scrolls (exercising scrollback, the path most at risk of per-line allocation).
fn workload(lines: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..lines {
        v.extend_from_slice(b"\x1b[1;32m");
        for c in 0..72 {
            v.push(b'a' + ((i + c) % 26) as u8);
        }
        v.extend_from_slice(b"\x1b[0m\r\n");
    }
    v
}

/// Like [`workload`] but each row is multi-byte UTF-8 (CJK + emoji), so the engine
/// drives the SIMD-UTF8 bulk-decode path. Used for the CJK/emoji floor.
fn unicode_workload(lines: usize) -> Vec<u8> {
    let glyphs = ['中', '文', '日', '本', '한', '😀', 'é', '∑'];
    let mut v = Vec::new();
    let mut buf = [0u8; 4];
    for i in 0..lines {
        for c in 0..36 {
            let g = glyphs[(i + c) % glyphs.len()];
            v.extend_from_slice(g.encode_utf8(&mut buf).as_bytes());
        }
        v.extend_from_slice(b"\r\n");
    }
    v
}

/// Default scrollback is 10,000 lines; warming past it fills the ring so further
/// scrolling reuses slots (the steady state we measure).
const WARM_LINES: usize = 11_000;

/// Allocation calls to `process` `measure_input` on an engine ALREADY warmed to
/// scrollback capacity — i.e. the steady-state allocation cost of that input.
fn steady_state_alloc_calls(measure_input: &[u8]) -> u64 {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(&workload(WARM_LINES)); // fill grid + scrollback ring to capacity
    ALLOC_CALLS.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    t.process(measure_input);
    ACTIVE.store(false, Ordering::Relaxed);
    ALLOC_CALLS.load(Ordering::Relaxed)
}

// Orders of magnitude below the line count, far above the observed handful: noise
// never flaps, but a per-line/per-cell regression (≈ thousands of allocs) is caught.
const CEILING: u64 = 64;

// ONE test: the global allocation counter is process-wide, so the measured spans
// must not overlap. Running both workloads sequentially in a single test keeps them
// serialized (cargo runs separate #[test] fns on parallel threads).
#[test]
fn engine_steady_state_processing_is_allocation_free() {
    let measure = 2_000;

    // ASCII: at steady state the ring is full, so scrolling more lines reuses slots.
    let ascii = steady_state_alloc_calls(&workload(measure));
    eprintln!("PERF-BASELINE (ascii):   {measure} lines at steady-state -> {ascii} allocations");
    assert!(
        ascii <= CEILING,
        "steady-state processing of {measure} ascii lines made {ascii} allocations (ceiling \
         {CEILING}) — the engine is allocating per line/cell instead of reusing the ring"
    );

    // CJK/EMOJI floor (M2 SIMD-UTF8): multi-byte text drives the bulk UTF-8 decode
    // path. Unlike ASCII, the wide/grapheme write path DOES allocate (grapheme
    // storage), so "allocation-free" is the wrong bar. Instead assert it scales
    // LINEARLY with input — 2x the lines must not cost ~4x the allocations — which
    // catches a quadratic regression while tolerating the honest per-grapheme cost.
    // Ratio is machine-independent + deterministic (same content, same allocator).
    let u_n = steady_state_alloc_calls(&unicode_workload(measure));
    let u_2n = steady_state_alloc_calls(&unicode_workload(measure * 2));
    eprintln!(
        "PERF-BASELINE (unicode): {measure} lines -> {u_n} allocs, {} lines -> {u_2n} allocs (ratio {:.2}x)",
        measure * 2,
        u_2n as f64 / u_n.max(1) as f64
    );
    // Linear is 2.0x; a quadratic regression is ~4x. Ceiling 3x catches quadratic
    // with headroom for the fixed per-call overhead amortizing differently.
    assert!(
        u_2n <= u_n.max(1) * 3,
        "unicode allocation scaled super-linearly: {u_n} -> {u_2n} for 2x input (ceiling {}) \
         — the CJK/emoji write path has a quadratic-allocation regression",
        u_n.max(1) * 3
    );
}

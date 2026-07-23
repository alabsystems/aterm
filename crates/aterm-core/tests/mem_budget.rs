// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Engine retained-heap CEILING (M2 MEM-BUDGET) — a deterministic regression gate.
//!
//! This is the self-contained, dependency-free half of the memory story (the
//! aterm-vs-alacritty COMPARISON lives in `aterm-bench/tests/memory.rs`). It pins
//! the engine's retained heap for a 24x80 grid under a committed ceiling so a real
//! regression — accidental per-cell boxing, scrollback bloat, a leak on the process
//! path — fails the build. The number is deterministic: the grid is pre-allocated,
//! cells are inline, so a full screen of SGR text retains ZERO additional heap.
//!
//! An integration test is its own crate, so the counting global allocator here does
//! NOT affect any other test or bench. The local gate runs this via `gate perf`.
//! Run directly: `cargo test -p aterm-core --test mem_budget -- --nocapture`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, Ordering};

static NET: AtomicI64 = AtomicI64::new(0);

/// System allocator that tracks net live bytes (alloc − dealloc).
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

/// Net heap retained while `f`'s result is alive (the engine's grid + state).
fn retained<T>(f: impl FnOnce() -> T) -> (i64, T) {
    let before = net();
    let v = f();
    let held = net() - before;
    (held, v)
}

/// 24 full rows of mixed ASCII + SGR — fills the visible screen without scrolling
/// far past it, so the measurement is dominated by the GRID, not scrollback config.
fn screen_fill() -> Vec<u8> {
    let mut v = Vec::new();
    for r in 0..24 {
        v.extend_from_slice(format!("\x1b[{};1H", r + 1).as_bytes());
        v.extend_from_slice(b"\x1b[1;38;5;202m");
        for c in 0..80 {
            v.push(b'a' + ((r + c) % 26) as u8);
        }
        v.extend_from_slice(b"\x1b[0m");
    }
    v
}

/// Observed 2026-06: 74_020 bytes, and a full screen of SGR text retains ZERO
/// additional heap (`filled == fresh`). Ceiling = observed × ~1.33 so minor
/// legitimate churn does not flap while a 33%+ blowup is caught.
const MEM_BUDGET_24X80_BYTES: i64 = 96 * 1024;

#[test]
fn engine_retained_heap_within_budget() {
    let corpus = screen_fill(); // allocated before measurement; stays alive

    let (fresh, t0) = retained(|| aterm_core::terminal::Terminal::new(24, 80));
    drop(t0);
    let (filled, t1) = retained(|| {
        let mut t = aterm_core::terminal::Terminal::new(24, 80);
        t.process(&corpus);
        t
    });
    drop(t1);

    eprintln!(
        "MEM-BUDGET 24x80: fresh={fresh}B filled={filled}B ceiling={MEM_BUDGET_24X80_BYTES}B"
    );

    assert!(
        filled <= MEM_BUDGET_24X80_BYTES,
        "engine retained heap {filled}B exceeds MEM-BUDGET ceiling {MEM_BUDGET_24X80_BYTES}B \
         — investigate (per-cell heap? scrollback bloat? leak on the process path?)"
    );
    // Filling a full screen must not balloon the heap: the grid is pre-allocated and
    // cells are inline. Lock that property (8KB slack for incidental bookkeeping).
    assert!(
        filled <= fresh + 8 * 1024,
        "filling a 24x80 screen grew retained heap by {}B (fresh={fresh}, filled={filled}) \
         — a full screen of text should reuse the pre-allocated grid, not allocate per-cell",
        filled - fresh
    );
}

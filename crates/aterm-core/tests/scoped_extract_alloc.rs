// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The damage-scoped refill is ALLOCATION-FREE once warm (typing-to-pixels
//! audit, P2 "remove avoidable typing-frame work").
//!
//! `Terminal::cell_frame_damage_scoped_into` is the per-frame extraction every
//! native echo frame rides. It used to build a fresh `Vec<u64>` row mask on
//! every scoped refill (and SCR-2's scrolled arm built another), so a typing
//! burst paid one heap allocation per frame for a mask whose size never
//! changes. The mask now lives in resident terminal scratch; this file pins
//! that the extraction of 10,000 one-row echoes allocates NOTHING after the
//! warm-up frame, and that the frames really took the scoped arm (a run that
//! silently fell back to full refills would be allocation-free for a
//! different reason and prove nothing about the mask).
//!
//! SCR-2's scrolled arm shares the same resident mask, but it is not measured
//! here: its debug-build net (`render_cells.rs`, "SCR-2 DEBUG NET") clones the
//! scratch for a full-extraction oracle on every scrolled step by design, so a
//! test build cannot observe that arm allocation-free.
//!
//! The counter is THREAD-LOCAL so the measured span counts only this test
//! thread's allocations — the harness and any sibling test are invisible.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ALLOC_CALLS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every method forwards to `System` with the caller's own layout and
// pointer; the only addition is a const-initialised thread-local counter,
// which never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = ALLOC_CALLS.try_with(|n| n.set(n.get() + 1));
            }
        });
        // SAFETY: forwarding to System with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = ALLOC_CALLS.try_with(|n| n.set(n.get() + 1));
            }
        });
        // SAFETY: forwarding to System with the same ptr+layout.
        unsafe { System.realloc(p, l, new_size) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: forwarding to System with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Run `f` and return how many allocations (and reallocations) it made on
/// this thread.
fn allocs_in<R>(f: impl FnOnce() -> R) -> (R, u64) {
    ALLOC_CALLS.with(|n| n.set(0));
    ACTIVE.with(|a| a.set(true));
    let r = f();
    ACTIVE.with(|a| a.set(false));
    (r, ALLOC_CALLS.with(Cell::get))
}

/// One keystroke's echo on a single row: a printable byte, and every 70th
/// keystroke a carriage return plus erase-in-line so the caret stays on the
/// same row forever (no scroll, no wrap — exactly one damaged row per frame).
fn echo(i: usize) -> &'static [u8] {
    const LETTERS: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
    if i % 70 == 69 {
        b"\r\x1b[K"
    } else {
        let k = i % 26;
        &LETTERS[k..=k]
    }
}

#[test]
fn ten_thousand_one_row_echoes_extract_with_zero_allocations_after_warm_up() {
    use aterm_core::render::{FrameRefill, RenderInput};
    use aterm_core::terminal::Terminal;

    let (rows, cols) = (24usize, 80usize);
    let mut term = Terminal::new(24, 80);
    let mut scratch = RenderInput::empty();
    // Warm-up: the first fill is Full (unstamped scratch) and sizes every row
    // buffer; the next echo frames take the scoped arm and size the mask.
    term.process(b"$ ");
    let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
    for i in 0..140 {
        term.process(echo(i));
        let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
    }

    let mut scoped = 0usize;
    let mut extraction_allocs = 0u64;
    for i in 140..140 + 10_000 {
        term.process(echo(i));
        let (refill, n) =
            allocs_in(|| term.cell_frame_damage_scoped_into(&mut scratch, rows, cols));
        extraction_allocs += n;
        match refill {
            FrameRefill::Scoped { rows_refilled } => {
                assert!(
                    rows_refilled <= 1,
                    "a one-row echo refilled {rows_refilled} rows at keystroke {i}"
                );
                scoped += 1;
            }
            FrameRefill::Full { cause } => {
                panic!("a one-row echo fell back to a full refill ({cause:?}) at keystroke {i}")
            }
        }
    }
    assert_eq!(scoped, 10_000, "every echo frame must take the scoped arm");
    assert_eq!(
        extraction_allocs, 0,
        "10,000 one-row echo extractions made {extraction_allocs} heap allocations after \
         warm-up — the damage-scoped refill must reuse its row mask and row buffers"
    );
}

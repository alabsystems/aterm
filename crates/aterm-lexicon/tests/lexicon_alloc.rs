// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-thread allocation-counting harness for typing-path hot spots, and the live-heap peak of a
//! load.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

mod scan_scratch_alloc;
mod tricks_alloc;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    /// Bytes this thread allocated less the bytes it freed since the window opened.
    static LIVE: Cell<isize> = const { Cell::new(0) };
    /// The most `LIVE` reached in the window.
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

fn note_allocation() {
    if counting() {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
    }
}

/// Move this thread's live bytes by `grew` less `shrank`, raising the peak with them.
fn note_bytes(grew: usize, shrank: usize) {
    if counting() {
        let delta = isize::try_from(grew).unwrap_or(isize::MAX)
            - isize::try_from(shrank).unwrap_or(isize::MAX);
        let live = LIVE.try_with(|n| {
            let live = n.get().saturating_add(delta);
            n.set(live);
            live
        });
        if let Ok(live) = live {
            let _ = PEAK.try_with(|p| p.set(p.get().max(live)));
        }
    }
}

struct CountingAllocator;

// SAFETY: every method delegates unchanged to `System`, a correct global
// allocator; counting touches only `const`-initialised thread-locals, which
// never allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            note_allocation();
            note_bytes(layout.size(), 0);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            note_allocation();
            note_bytes(layout.size(), 0);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            note_allocation();
            note_bytes(new_size, layout.size());
        }
        new_ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` came from this delegating allocator.
        unsafe { System.dealloc(ptr, layout) };
        note_bytes(0, layout.size());
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations_during(f: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|n| n.set(0));
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    ALLOCATIONS.with(Cell::get)
}

/// The most heap `f` held live on this thread at once, in bytes (what it held on entry not
/// counted).
fn peak_bytes_during(f: impl FnOnce()) -> usize {
    LIVE.with(|n| n.set(0));
    PEAK.with(|p| p.set(0));
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    usize::try_from(PEAK.with(Cell::get)).unwrap_or(0)
}

#[test]
fn the_peak_is_the_most_this_thread_held_at_once() {
    let peak = peak_bytes_during(|| {
        drop(std::hint::black_box(vec![0u8; 1 << 20]));
        drop(std::hint::black_box(Vec::<u8>::with_capacity(64)));
    });
    assert_eq!(peak, 1 << 20, "one MiB held, then freed, then 64 bytes");
}

#[test]
fn the_counter_sees_this_threads_allocations_and_no_other_threads() {
    let mine = allocations_during(|| {
        std::hint::black_box(Vec::<u8>::with_capacity(64));
    });
    assert_eq!(mine, 1, "one Vec::with_capacity is one allocation");

    let theirs = allocations_during(|| {
        std::thread::scope(|s| {
            s.spawn(|| std::hint::black_box(Vec::<u8>::with_capacity(64)))
                .join()
                .expect("join");
        });
    });
    let spawn_only = allocations_during(|| {
        std::thread::scope(|s| {
            s.spawn(|| {}).join().expect("join");
        });
    });
    assert_eq!(
        theirs, spawn_only,
        "an allocation on ANOTHER thread must not be counted here"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-thread allocation-counting harness for typing-path hot spots.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

mod scan_scratch_alloc;
mod tricks_alloc;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn note_allocation() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
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
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            note_allocation();
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            note_allocation();
        }
        new_ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` came from this delegating allocator.
        unsafe { System.dealloc(ptr, layout) };
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

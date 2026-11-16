// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The counting allocator of the memory tests whose client spawns workers: while `ACTIVE` is
//! set, `GROSS` adds the size of every successful request on every thread but the fixture
//! server's, and frees are never subtracted. The counters are process-wide, so each binary
//! that includes this module by `#[path]` holds a single test, and no other test's
//! allocations reach `GROSS`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) static ACTIVE: AtomicBool = AtomicBool::new(false);
pub(crate) static GROSS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // Only the bounded fixture server sets this. Every client worker is counted.
    pub(crate) static FIXTURE_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn allocated(bytes: usize) {
    if ACTIVE.load(Ordering::SeqCst) && !FIXTURE_THREAD.try_with(Cell::get).unwrap_or(false) {
        GROSS.fetch_add(bytes, Ordering::SeqCst);
    }
}

struct Counting;

// SAFETY: every allocator call forwards the original pointer and layout to System. Atomics
// and const TLS allocate nothing. Count successful requested sizes, including the new size of
// realloc; never subtract frees, including frees of pre-window cache and memo owners.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation layout.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forward the matching live allocation and requested new size unchanged.
        let changed = unsafe { System.realloc(pointer, layout, new_size) };
        if !changed.is_null() {
            allocated(new_size);
        }
        changed
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forward the allocation and its matching original layout unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

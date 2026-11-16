// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The counting allocator of the memory tests that bound a net peak on one thread: while
//! `ACTIVE` is set on a thread, `LIVE` holds the bytes that thread has allocated and not freed
//! and `PEAK` the most `LIVE` reached. Each test binary that includes this module by `#[path]`
//! installs it as its `#[global_allocator]`; every other thread, and every thread outside a
//! measurement, is forwarded to `System` uncounted.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    pub(crate) static ACTIVE: Cell<bool> = const { Cell::new(false) };
    pub(crate) static LIVE: Cell<usize> = const { Cell::new(0) };
    pub(crate) static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every pointer and layout is forwarded unchanged to System. Constant TLS counters
// allocate nothing. The default realloc counts the old and new blocks during growth.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding the caller's valid layout unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let _ = ACTIVE.try_with(|active| {
                if active.get() {
                    let _ = LIVE.try_with(|live| {
                        let next = live.get() + layout.size();
                        live.set(next);
                        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(next)));
                    });
                }
            });
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = LIVE.try_with(|live| live.set(live.get() - layout.size()));
            }
        });
        // SAFETY: forwarding the caller's allocated pointer and matching layout unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

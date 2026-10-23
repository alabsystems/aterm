// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Combined integration test binary for `aterm-search`, with a per-thread
//! counting `#[global_allocator]` ([`heap`]) for memory tests.

mod index_column_map_memory;
mod index_release_memory;
mod search_vs_bruteforce;

/// Net live heap bytes, and bytes ever allocated, on the calling thread.
mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static NET: Cell<i64> = const { Cell::new(0) };
        static ALLOCATED: Cell<u64> = const { Cell::new(0) };
    }

    // Const-initialized and drop-free so thread-locals never allocate during teardown.
    /// Const-initialized and drop-free, so these thread-locals never allocate
    /// and stay readable while a thread is torn down.
    fn note(grow: usize, shrink: usize) {
        let _ = NET.try_with(|n| n.set(n.get() + grow as i64 - shrink as i64));
        let _ = ALLOCATED.try_with(|a| a.set(a.get() + grow as u64));
    }

    struct Counting;

    // SAFETY: every call forwards its arguments to `System` unchanged; the only
    // other work is thread-local arithmetic, which allocates nothing.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            note(l.size(), 0);
            // SAFETY: the caller's layout, forwarded unchanged.
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            note(0, l.size());
            // SAFETY: `p` and `l` came from `System` through this allocator.
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            note(n, l.size());
            // SAFETY: `p` and `l` came from `System` through this allocator.
            unsafe { System.realloc(p, l, n) }
        }
    }

    #[global_allocator]
    static ALLOC: Counting = Counting;

    /// Live heap bytes this thread allocated and has not freed.
    pub(crate) fn net() -> i64 {
        NET.with(Cell::get)
    }

    /// Heap bytes this thread ever allocated (a `realloc` counts its new size).
    pub(crate) fn allocated() -> u64 {
        ALLOCATED.with(Cell::get)
    }
}

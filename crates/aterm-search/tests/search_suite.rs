// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Combined integration test binary for `aterm-search`, with a per-thread
//! counting `#[global_allocator]` ([`heap`]) for memory tests.

mod casefold_cost;
mod column_projection_cost;
mod column_refresh_memory;
mod index_column_map_memory;
mod index_release_memory;
mod navigation_memory;
mod overlap_cost;
mod refresh_memory;
mod regex_retention;
mod retention_memory;
mod reverse_casefold_cost;
mod search_vs_bruteforce;
mod selection_projection_cost;

/// Net live heap bytes, and bytes ever allocated, on the calling thread.
mod heap {
    use aterm_bench::allocator::{Observed, Observer};
    use std::alloc::System;
    use std::cell::Cell;

    thread_local! {
        static NET: Cell<i64> = const { Cell::new(0) };
        static ALLOCATED: Cell<u64> = const { Cell::new(0) };
        static PEAK: Cell<Option<i64>> = const { Cell::new(None) };
    }

    /// Const-initialized and drop-free, so these thread-locals never allocate
    /// and stay readable while a thread is torn down.
    fn note(grow: usize, shrink: usize) {
        let _ = NET.try_with(|n| {
            let current = n
                .get()
                .wrapping_add(grow as i64)
                .wrapping_sub(shrink as i64);
            n.set(current);
            let _ = PEAK.try_with(|peak| {
                if let Some(previous) = peak.get() {
                    peak.set(Some(previous.max(current)));
                }
            });
        });
        let _ = ALLOCATED.try_with(|a| a.set(a.get().wrapping_add(grow as u64)));
    }

    struct Counting;

    // SAFETY: constant, drop-free thread-locals and wrapping arithmetic neither allocate nor unwind.
    unsafe impl Observer for Counting {
        fn retained(&self, old: usize, new: usize) {
            note(new, old);
        }
    }

    #[global_allocator]
    static ALLOC: Observed<System, Counting> = Observed::new(System, Counting);

    /// Live heap bytes this thread allocated and has not freed.
    pub(crate) fn net() -> i64 {
        NET.with(Cell::get)
    }

    /// Heap bytes this thread ever allocated (a `realloc` counts its new size).
    pub(crate) fn allocated() -> u64 {
        ALLOCATED.with(Cell::get)
    }

    /// Start tracking the calling thread's logical live-allocation high-water mark.
    pub(crate) fn start_peak() -> i64 {
        let before = net();
        PEAK.with(|peak| peak.set(Some(before)));
        before
    }

    /// Read the high-water mark and disable peak tracking.
    pub(crate) fn take_peak() -> i64 {
        PEAK.with(|peak| peak.take().expect("peak tracking was started"))
    }
}

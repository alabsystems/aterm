// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The heap counter of `aterm-gui`'s lib tests, whose allocation budgets hold its per-frame paths.
//!
//! Linking this crate makes its allocator the binary's global one, so a crate takes it as a
//! dev-dependency and calls it from `cfg(test)` code alone. The allocator stands on every
//! allocation of the whole binary and compiles at the opt-level of the crate that declares it,
//! std's `System`, `Layout` and atomics with it (each is `#[inline]`), so it lives apart from the
//! crate it counts, which runs at opt-level 0: the root `Cargo.toml` holds this one at 2, where a
//! call costs a relaxed load beside the system allocator's own work.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static FREED: Cell<usize> = const { Cell::new(0) };
}

/// Threads inside [`allocations_in`] now: while none is, an allocation skips the thread-local read.
static COUNTING_THREADS: AtomicUsize = AtomicUsize::new(0);

struct CountingAlloc<A = System>(A);

impl<A> CountingAlloc<A> {
    #[inline]
    fn note() {
        if COUNTING_THREADS.load(Ordering::Relaxed) == 0 {
            return;
        }
        let _ = COUNTING.try_with(|on| {
            if on.get() {
                let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            }
        });
    }

    /// Count `bytes` handed back to the allocator on a counting thread ([`bytes_freed_in`]).
    #[inline]
    fn note_freed(bytes: usize) {
        if COUNTING_THREADS.load(Ordering::Relaxed) == 0 {
            return;
        }
        let _ = COUNTING.try_with(|on| {
            if on.get() {
                let _ = FREED.try_with(|n| n.set(n.get().saturating_add(bytes)));
            }
        });
    }
}

// SAFETY: every method forwards to the inner allocator with the caller's layout and
// pointer unchanged; the count beside it never allocates.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAlloc<A> {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded with the caller's layout.
        unsafe { self.0.alloc(layout) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded with the caller's layout.
        unsafe { self.0.alloc_zeroed(layout) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded with the caller's pointer, layout and size.
        let next = unsafe { self.0.realloc(ptr, layout, size) };
        if !next.is_null() {
            Self::note_freed(layout.size().saturating_sub(size));
        }
        next
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::note_freed(layout.size());
        // SAFETY: forwarded with the caller's pointer and layout.
        unsafe { self.0.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc(System);

/// Run `f` and say how many heap allocations (and reallocations) it made on this thread.
pub fn allocations_in<R>(f: impl FnOnce() -> R) -> (R, usize) {
    struct Counting;
    impl Drop for Counting {
        fn drop(&mut self) {
            COUNTING.with(|on| on.set(false));
            COUNTING_THREADS.fetch_sub(1, Ordering::Relaxed);
        }
    }
    ALLOCATIONS.with(|n| n.set(0));
    COUNTING_THREADS.fetch_add(1, Ordering::Relaxed);
    let counting = Counting;
    COUNTING.with(|on| on.set(true));
    let out = f();
    drop(counting);
    (out, ALLOCATIONS.with(|n| n.replace(0)))
}

/// Run `f` and say how many heap bytes it handed back on this thread: each freed block's requested
/// size, and what a shrinking reallocation gave up (a growing one frees nothing net).
pub fn bytes_freed_in<R>(f: impl FnOnce() -> R) -> (R, usize) {
    FREED.with(|n| n.set(0));
    let (out, _) = allocations_in(f);
    (out, FREED.with(|n| n.replace(0)))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

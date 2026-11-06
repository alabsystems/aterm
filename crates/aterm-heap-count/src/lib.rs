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

struct CountingAlloc;

impl CountingAlloc {
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

// SAFETY: every method forwards to `System` with the caller's layout and
// pointer unchanged; the count beside it never allocates.
unsafe impl GlobalAlloc for CountingAlloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded with the caller's layout.
        unsafe { System.alloc(layout) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded with the caller's layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        Self::note();
        Self::note_freed(layout.size().saturating_sub(size));
        // SAFETY: forwarded with the caller's pointer, layout and size.
        unsafe { System.realloc(ptr, layout, size) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::note_freed(layout.size());
        // SAFETY: forwarded with the caller's pointer and layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

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
mod tests {
    use std::hint::black_box;

    use super::*;

    /// An allocation and a reallocation each count one, a free none, and a thread counting finds
    /// only its own.
    /// NEGATIVE CONTROL (2026-11-05): with `note` leaving the tally as it found it this fired "a box
    /// allocates once" (left 0, right 1); with one tally for the whole process, run alone, "another
    /// thread's allocations are its own: 1006".
    #[test]
    fn a_thread_counts_its_own_allocations_and_no_others() {
        let ((), one) = allocations_in(|| {
            black_box(Box::new(1_u64));
        });
        assert_eq!(one, 1, "a box allocates once");
        let mut grown = Vec::<u8>::with_capacity(8);
        let ((), one) = allocations_in(|| {
            grown.reserve_exact(4_096);
            black_box(&grown);
        });
        assert_eq!(one, 1, "a reallocation counts one");
        let (kept, none) = allocations_in(|| black_box(grown));
        assert_eq!(none, 0, "a move allocates nothing");
        let ((), none) = allocations_in(|| drop(kept));
        assert_eq!(none, 0, "a free is no allocation");

        // Starting the thread allocates a few of this thread's own; the thousand are the other's.
        let ((), mine) = allocations_in(|| {
            std::thread::scope(|threads| {
                threads.spawn(|| {
                    for at in 0..1_000_u64 {
                        black_box(Box::new(at));
                    }
                });
            });
        });
        assert!(
            mine < 1_000,
            "another thread's allocations are its own: {mine}"
        );
    }

    /// A freed block counts its requested size, a shrinking reallocation what it gave up, and a
    /// growing one nothing.
    /// NEGATIVE CONTROL (2026-11-05): with `realloc` charging its whole old block this fired "a
    /// shrinking reallocation" (left 1000, right 900).
    #[test]
    fn a_thread_counts_the_bytes_it_hands_back() {
        let block = black_box(vec![0_u8; 1_000]);
        let ((), freed) = bytes_freed_in(|| drop(block));
        assert_eq!(freed, 1_000, "a freed block");
        let mut shrunk = black_box(vec![0_u8; 1_000]);
        let ((), freed) = bytes_freed_in(|| {
            shrunk.truncate(100);
            shrunk.shrink_to_fit();
            black_box(&shrunk);
        });
        assert_eq!(freed, 900, "a shrinking reallocation");
        let ((), freed) = bytes_freed_in(|| {
            shrunk.reserve_exact(4_096);
            black_box(&shrunk);
        });
        assert_eq!(freed, 0, "a growing reallocation");
    }
}

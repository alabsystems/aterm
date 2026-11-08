// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering::Relaxed};

use aterm_bench::allocator::{Observed, Observer};

#[derive(Default)]
struct State {
    fail: AtomicBool,
    alloc: AtomicUsize,
    zeroed: AtomicUsize,
    realloc: AtomicUsize,
    dealloc: AtomicUsize,
    requested: AtomicUsize,
    calls: AtomicUsize,
    live: AtomicIsize,
    changes: AtomicUsize,
}

struct Inner<'a>(&'a State);
struct Counter<'a>(&'a State);

// SAFETY: all successful operations forward unchanged to System; null failures preserve the old
// allocation. The bookkeeping uses only nonallocating, nonpanicking atomics.
unsafe impl GlobalAlloc for Inner<'_> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0.alloc.fetch_add(1, Relaxed);
        if self.0.fail.load(Relaxed) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarding the caller's layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.0.zeroed.fetch_add(1, Relaxed);
        if self.0.fail.load(Relaxed) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarding the caller's layout and zeroing request.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        self.0.realloc.fetch_add(1, Relaxed);
        if self.0.fail.load(Relaxed) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarding the caller's pointer, layout and new size.
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.0.dealloc.fetch_add(1, Relaxed);
        // SAFETY: forwarding the caller's pointer and layout.
        unsafe { System.dealloc(ptr, layout) };
    }
}

// SAFETY: the callbacks use only nonallocating, nonpanicking atomic arithmetic.
unsafe impl Observer for Counter<'_> {
    fn requested(&self, bytes: usize) {
        self.0.calls.fetch_add(1, Relaxed);
        self.0.requested.fetch_add(bytes, Relaxed);
    }

    fn retained(&self, old: usize, new: usize) {
        self.0.changes.fetch_add(1, Relaxed);
        self.0
            .live
            .fetch_add((new as isize).wrapping_sub(old as isize), Relaxed);
    }
}

#[test]
fn native_operations_preserve_contents_and_account_for_success_and_failure() {
    let state = State::default();
    let allocator = Observed::new(Inner(&state), Counter(&state));
    let small = Layout::from_size_align(32, 32).unwrap();
    let large = Layout::from_size_align(256, 32).unwrap();
    // SAFETY: each non-null block is used within its layout, reallocated with its current layout,
    // and freed exactly once. A failed realloc leaves the original block owned by this test.
    unsafe {
        state.fail.store(true, Relaxed);
        assert!(allocator.alloc(small).is_null());
        assert!(allocator.alloc_zeroed(large).is_null());
        assert_eq!(state.live.load(Relaxed), 0);
        assert_eq!(state.changes.load(Relaxed), 0);

        state.fail.store(false, Relaxed);
        let mut ptr = allocator.alloc_zeroed(small);
        assert!(!ptr.is_null());
        assert_eq!(ptr as usize % 32, 0);
        assert!(std::slice::from_raw_parts(ptr, 32).iter().all(|b| *b == 0));
        ptr.write_bytes(0xa5, 32);
        assert_eq!(state.live.load(Relaxed), 32);

        ptr = allocator.realloc(ptr, small, 256);
        assert!(!ptr.is_null());
        assert_eq!(ptr as usize % 32, 0);
        assert!(
            std::slice::from_raw_parts(ptr, 32)
                .iter()
                .all(|b| *b == 0xa5)
        );
        assert_eq!(state.live.load(Relaxed), 256);

        state.fail.store(true, Relaxed);
        assert!(allocator.realloc(ptr, large, 32).is_null());
        assert!(allocator.realloc(ptr, large, 512).is_null());
        assert_eq!(state.live.load(Relaxed), 256);
        assert_eq!(state.changes.load(Relaxed), 2);
        assert!(
            std::slice::from_raw_parts(ptr, 32)
                .iter()
                .all(|b| *b == 0xa5)
        );

        state.fail.store(false, Relaxed);
        ptr = allocator.realloc(ptr, large, 32);
        assert!(!ptr.is_null());
        assert!(
            std::slice::from_raw_parts(ptr, 32)
                .iter()
                .all(|b| *b == 0xa5)
        );
        assert_eq!(state.live.load(Relaxed), 32);
        allocator.dealloc(ptr, small);

        let ptr = allocator.alloc(small);
        assert!(!ptr.is_null());
        allocator.dealloc(ptr, small);
    }
    assert_eq!(state.live.load(Relaxed), 0);
    assert_eq!(state.changes.load(Relaxed), 6);
    assert_eq!(state.calls.load(Relaxed), 8);
    assert_eq!(
        state.requested.load(Relaxed),
        32 + 256 + 32 + 256 + 32 + 512 + 32 + 32
    );
    assert_eq!(state.alloc.load(Relaxed), 2);
    assert_eq!(state.zeroed.load(Relaxed), 2);
    assert_eq!(state.realloc.load(Relaxed), 4);
    assert_eq!(state.dealloc.load(Relaxed), 2);
}

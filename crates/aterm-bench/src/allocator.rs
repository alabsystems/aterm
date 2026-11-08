// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Allocation observation that preserves the underlying allocator's native operations.

use std::alloc::{GlobalAlloc, Layout};

/// Observe requested traffic and successfully retained requested bytes separately.
///
/// # Safety
/// Callbacks run inside a global allocator: they must not allocate, reenter the allocator,
/// or unwind. Shared state must be synchronized.
pub unsafe trait Observer: Sync {
    /// An allocation, zeroed allocation, or reallocation was requested, even if it fails.
    fn requested(&self, _bytes: usize) {}
    /// A successful operation replaces `old` retained bytes with `new` retained bytes.
    fn retained(&self, _old: usize, _new: usize) {}
}

/// Forward every operation unchanged; observation never substitutes allocate/copy/free for realloc.
pub struct Observed<A, O> {
    inner: A,
    observer: O,
}

impl<A, O> Observed<A, O> {
    /// Wrap an allocator with an observer.
    pub const fn new(inner: A, observer: O) -> Self {
        Self { inner, observer }
    }

    /// Read the observer's counters without exposing the underlying allocator.
    pub const fn observer(&self) -> &O {
        &self.observer
    }
}

// SAFETY: all four operations preserve the inner allocator's pointer/layout contract. Observer's
// unsafe contract excludes allocation, reentry and unwinding in the callbacks.
unsafe impl<A: GlobalAlloc, O: Observer> GlobalAlloc for Observed<A, O> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.observer.requested(layout.size());
        // SAFETY: the caller's layout is forwarded unchanged.
        let ptr = unsafe { self.inner.alloc(layout) };
        if !ptr.is_null() {
            self.observer.retained(0, layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.observer.requested(layout.size());
        // SAFETY: the caller's layout is forwarded unchanged, including the zeroing operation.
        let ptr = unsafe { self.inner.alloc_zeroed(layout) };
        if !ptr.is_null() {
            self.observer.retained(0, layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        self.observer.requested(size);
        // SAFETY: the caller's pointer, old layout and new size are forwarded unchanged.
        let next = unsafe { self.inner.realloc(ptr, layout, size) };
        if !next.is_null() {
            self.observer.retained(layout.size(), size);
        }
        next
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's pointer and layout are forwarded unchanged.
        unsafe { self.inner.dealloc(ptr, layout) };
        self.observer.retained(layout.size(), 0);
    }
}

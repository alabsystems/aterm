// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-thread allocation counter (`#[global_allocator]`) for zero-allocation hot-path tests.

#![allow(dead_code, reason = "a binary that mounts this uses what it needs")]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

struct CountingAllocator;

/// How many sizes a counted window keeps.
pub const SIZE_SLOTS: usize = 16;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static ALLOCATION_SIZES: Cell<[usize; SIZE_SLOTS]> = const { Cell::new([0; SIZE_SLOTS]) };
}

fn record_allocation(size: usize) {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|count| {
            let index = count.get();
            count.set(index + 1);
            if index < SIZE_SLOTS {
                let _ = ALLOCATION_SIZES.try_with(|sizes| {
                    let mut kept = sizes.get();
                    kept[index] = size;
                    sizes.set(kept);
                });
            }
        });
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            record_allocation(new_size);
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

/// Count allocations performed by `f` on the current thread.
pub fn allocations_during<T>(f: impl FnOnce() -> T) -> (T, usize, [usize; SIZE_SLOTS]) {
    ALLOCATIONS.set(0);
    ALLOCATION_SIZES.set([0; SIZE_SLOTS]);
    COUNTING.set(true);
    let out = f();
    COUNTING.set(false);
    (out, ALLOCATIONS.get(), ALLOCATION_SIZES.get())
}

/// Verify that [`allocations_during`] counts only the calling thread's allocations.
pub fn assert_counter_is_per_thread() {
    let go = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let foreign = {
        let (go, done) = (Arc::clone(&go), Arc::clone(&done));
        std::thread::spawn(move || {
            while !go.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            std::hint::black_box(Box::new([0u8; 48]));
            done.store(true, Ordering::Release);
        })
    };
    let ((), allocs, sizes) = allocations_during(|| {
        go.store(true, Ordering::Release);
        while !done.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        std::hint::black_box(Box::new([0u8; 24]));
    });
    foreign
        .join()
        .expect("the foreign allocating thread panicked");
    assert_eq!(
        (allocs, sizes[0]),
        (1, 24),
        "the counting allocator must count this thread's one 24-byte box and \
         nothing another thread allocates meanwhile (first sizes {sizes:?})"
    );
    let ((), allocs, sizes) = std::thread::spawn(|| {
        allocations_during(|| {
            std::hint::black_box(Box::new([0u8; 40]));
        })
    })
    .join()
    .expect("the counting worker thread panicked");
    assert_eq!(
        (allocs, sizes[0]),
        (1, 40),
        "a worker thread's own counted window must see its one 40-byte box \
         (first sizes {sizes:?})"
    );
}

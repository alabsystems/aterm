// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::hint::black_box;

use super::*;

struct RefuseRealloc;

// SAFETY: allocations and deallocations forward unchanged to System. A failed reallocation
// returns null without touching the original allocation, as required by GlobalAlloc.
unsafe impl GlobalAlloc for RefuseRealloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding the caller's layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarding the caller's pointer and layout.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, _ptr: *mut u8, _layout: Layout, _size: usize) -> *mut u8 {
        std::ptr::null_mut()
    }
}

#[test]
fn failed_reallocations_count_requests_but_free_no_bytes() {
    let allocator = CountingAlloc(RefuseRealloc);
    let layout = Layout::from_size_align(1_000, 8).unwrap();
    // SAFETY: allocate a real block, keep ownership across both refused reallocations, read
    // only initialized bytes within it, then deallocate it exactly once with its old layout.
    unsafe {
        let ptr = allocator.alloc(layout);
        assert!(!ptr.is_null());
        ptr.write_bytes(0xa5, 1_000);
        for size in [100, 2_000] {
            let (next, requests) = allocations_in(|| allocator.realloc(ptr, layout, size));
            assert!(next.is_null());
            assert_eq!(requests, 1);
            let (next, freed) = bytes_freed_in(|| allocator.realloc(ptr, layout, size));
            assert!(next.is_null());
            assert_eq!(freed, 0);
            assert!(
                std::slice::from_raw_parts(ptr, 1_000)
                    .iter()
                    .all(|b| *b == 0xa5)
            );
        }
        let ((), freed) = bytes_freed_in(|| allocator.dealloc(ptr, layout));
        assert_eq!(freed, 1_000);
    }
}

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

// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::*;
use ntest::timeout;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

// ========================================================================
// Compile-time Send/Sync guards (#5697 Phase 2)
// ========================================================================
//
// These guards verify at compile time that Page and PageSlice still
// satisfy their unsafe Send/Sync bounds. A field change that breaks
// the bound causes a compile error — no runtime test needed.

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

#[test]
fn guard_page_send_sync() {
    assert_send::<Page>();
    assert_sync::<Page>();
}

#[test]
fn guard_page_slice_send_sync() {
    // PageSlice<T> is Send when T: Send, Sync when T: Sync
    assert_send::<PageSlice<u32>>();
    assert_sync::<PageSlice<u32>>();
    assert_send::<PageSlice<u8>>();
    assert_sync::<PageSlice<u8>>();
}

// ========================================================================
// Runtime thread safety tests (MIRI-exercised)
// ========================================================================

/// Verify that Page satisfies Send - it can be transferred to another thread.
///
/// This exercises the `unsafe impl Send for Page` at page.rs:48.
/// If the impl were unsound, MIRI would flag this.
#[test]
#[timeout(10_000)]
fn page_send_across_thread() {
    let page = Page::new();

    let handle = std::thread::spawn(move || {
        // Access page data on the new thread
        let ptr = page.data_ptr();
        assert!(!ptr.is_null());
        // Read a byte to verify the pointer is valid
        let val = unsafe { *ptr };
        assert_eq!(val, 0, "freshly allocated page should be zeroed");
        page // return ownership
    });

    let _page = handle.join().expect("thread panicked");
}

/// Verify that PageSlice satisfies Send — allocated slices can be
/// transferred to another thread and accessed.
///
/// This exercises the `unsafe impl<T: Send> Send for PageSlice<T>` at
/// page.rs:254.
#[test]
#[timeout(10_000)]
fn page_slice_send_across_thread() {
    let mut store = PageStore::new();
    let mut slice = store.alloc_slice::<u32>(50);

    // Write data on main thread
    for (i, val) in slice.iter_mut().enumerate() {
        *val = i as u32 * 7;
    }

    // Transfer to another thread and verify
    let handle = std::thread::spawn(move || {
        for (i, &val) in slice.as_slice().iter().enumerate() {
            assert_eq!(val, i as u32 * 7, "slice[{i}] wrong after thread transfer");
        }
        slice.len()
    });

    let len = handle.join().expect("thread panicked");
    assert_eq!(len, 50);
}

// ========================================================================
// Sync impl validation: concurrent shared access (MIRI-exercised)
// ========================================================================
//
// These tests exercise the `unsafe impl Sync for Page` and
// `unsafe impl Sync for PageSlice<T>` impls that previously had no
// test coverage. Under MIRI they validate Stacked Borrows compliance
// for concurrent reads through shared references.

/// Verify that `unsafe impl Sync for Page` is sound: multiple threads can
/// read concurrently through `&Page` via `Arc`.
///
/// Exercises page.rs:49. If the UnsafeCell interior mutability violated
/// Sync rules, MIRI would flag a data race.
#[test]
#[timeout(10_000)]
fn page_sync_concurrent_readers() {
    use std::sync::Arc;

    let page = Arc::new(Page::new());

    // Spawn two reader threads sharing the same &Page
    let p1 = Arc::clone(&page);
    let h1 = std::thread::spawn(move || {
        let ptr = p1.data_ptr();
        assert!(!ptr.is_null());
        // Read from the first quarter
        let val = unsafe { *ptr };
        assert_eq!(val, 0);
    });

    let p2 = Arc::clone(&page);
    let h2 = std::thread::spawn(move || {
        let ptr = p2.data_ptr();
        assert!(!ptr.is_null());
        // Read from further into the page
        let val = unsafe { *ptr.add(PAGE_SIZE / 2) };
        assert_eq!(val, 0);
    });

    h1.join().expect("reader 1 panicked");
    h2.join().expect("reader 2 panicked");
}

/// Verify that `unsafe impl Sync for PageSlice<T>` is sound: multiple
/// threads can read concurrently through `Arc<PageSlice<u32>>`.
///
/// Exercises page.rs:258. The `NonNull<T>` pointer must remain valid and
/// the data must be safely readable from multiple threads simultaneously.
///
/// Note: `PageSlice` does not implement Clone, so we wrap in Arc to share
/// across threads. The `as_slice()` method calls `from_raw_parts` which
/// MIRI validates under Stacked Borrows.
#[test]
#[timeout(10_000)]
fn page_slice_sync_concurrent_readers() {
    use std::sync::Arc;

    let mut store = PageStore::new();
    let mut slice = store.alloc_slice::<u32>(100);

    // Write a pattern
    for (i, val) in slice.iter_mut().enumerate() {
        *val = (i as u32) * 13 + 7;
    }

    // Transfer the slice into an Arc for shared concurrent access.
    // PageSlice<u32>: Sync because u32: Sync.
    let shared = Arc::new(slice);

    let s1 = Arc::clone(&shared);
    let h1 = std::thread::spawn(move || {
        // Read first half
        for (i, &val) in s1.as_slice()[..50].iter().enumerate() {
            assert_eq!(val, (i as u32) * 13 + 7, "reader1 s[{i}] wrong");
        }
    });

    let s2 = Arc::clone(&shared);
    let h2 = std::thread::spawn(move || {
        // Read second half
        for (i, &val) in s2.as_slice()[50..].iter().enumerate() {
            let idx = i + 50;
            assert_eq!(val, (idx as u32) * 13 + 7, "reader2 s[{idx}] wrong");
        }
    });

    h1.join().expect("reader 1 panicked");
    h2.join().expect("reader 2 panicked");
}

// ========================================================================
// Offset::get_mut aliasing invariant (MIRI-exercised)
// ========================================================================
//
// Offset::get_mut returns &mut T from &Page via UnsafeCell.
// The key invariant: two get_mut calls at the SAME offset would create
// aliasing &mut references, which is UB. The following test demonstrates
// the safe usage pattern (sequential non-overlapping access) that MIRI
// validates under Stacked Borrows.

/// Verify that sequential Offset::get_mut calls to non-overlapping offsets
/// are sound under Stacked Borrows.
///
/// Exercises page.rs:171-181. MIRI validates that the two mutable
/// references do not alias.
#[test]
fn offset_get_mut_non_overlapping_offsets() {
    let page = Page::new();

    let offset_a = Offset::<u32>::new(0);
    let offset_b = Offset::<u32>::new(64); // different cache line

    unsafe {
        // Write to offset A, then drop the reference
        *offset_a.get_mut(&page).expect("valid offset A") = 0xAAAA_AAAA;
    }
    unsafe {
        // Write to offset B — no alias since A's &mut was dropped
        *offset_b.get_mut(&page).expect("valid offset B") = 0xBBBB_BBBB;
    }

    // Verify both values via immutable get (no aliasing concern)
    unsafe {
        assert_eq!(*offset_a.get(&page).expect("read A"), 0xAAAA_AAAA);
        assert_eq!(*offset_b.get(&page).expect("read B"), 0xBBBB_BBBB);
    }
}

/// Verify that Offset::get_mut followed by get on the same offset is
/// sound when the mutable reference is dropped before the shared one.
///
/// This is the tightest safe pattern: write through get_mut, drop, then
/// read through get. MIRI validates no Stacked Borrows violation.
#[test]
fn offset_get_mut_then_get_same_offset() {
    let page = Page::new();
    let offset = Offset::<u64>::new(0);

    unsafe {
        // Mutable access — write
        *offset.get_mut(&page).expect("valid") = 0xDEAD_BEEF_CAFE_BABE;
    }
    // The &mut was dropped above, so &T is fine now
    unsafe {
        let val = *offset.get(&page).expect("valid");
        assert_eq!(val, 0xDEAD_BEEF_CAFE_BABE);
    }
}

// ========================================================================
// PageStore allocate-reset-allocate property: lazy zeroing correctness
// ========================================================================
//
// After reset(), the PageStore moves active pages to the free list. On
// re-allocation, the lazy zeroing path must zero exactly the used portion.
// This test uses proptest-style iteration over various allocation sizes.

/// Multiple allocate-reset cycles with varying sizes: every re-allocation
/// must produce zeroed memory regardless of what was previously stored.
///
/// This is the critical memory safety property for the lazy zeroing
/// optimization: if page_used_bytes tracking is wrong, stale data leaks.
#[test]
fn allocate_reset_cycle_always_zeroed() {
    let mut store = PageStore::new();

    // Run several cycles with different allocation sizes
    let sizes: &[u16] = &[1, 10, 100, 1000, 8000, 16384, 100, 1];

    for (cycle, &size) in sizes.iter().enumerate() {
        // Allocate and fill with non-zero pattern
        let mut slice = store.alloc_slice::<u32>(size);
        let pattern = 0xDEAD_0000 | (cycle as u32);
        for val in slice.iter_mut() {
            *val = pattern;
        }

        // Verify pattern was written
        for (i, &val) in slice.as_slice().iter().enumerate() {
            assert_eq!(val, pattern, "cycle {cycle}: write failed at index {i}");
        }

        // Reset: pages go to free list with used_bytes metadata
        store.reset();

        // Re-allocate: must get zeroed memory
        let fresh = store.alloc_slice::<u32>(size);
        for (i, &val) in fresh.as_slice().iter().enumerate() {
            assert_eq!(
                val, 0,
                "cycle {cycle}: stale data at index {i} (got {val:#010x})"
            );
        }

        // Reset again for next cycle
        store.reset();
    }
}

/// Allocate-reset with cross-page spills: verify zeroing when multiple
/// pages are recycled with different used_bytes values.
#[test]
fn allocate_reset_cross_page_zeroed() {
    let mut store = PageStore::new();

    // Two full-page allocations that each fill an entire page.
    // 16384 u32s = 65536 bytes = exactly PAGE_SIZE. Second alloc must go to page 1.
    let mut first = store.alloc_slice::<u32>(16384);
    for val in first.iter_mut() {
        *val = 0x1111_1111;
    }
    let mut second = store.alloc_slice::<u32>(100); // no room on page 0
    for val in second.iter_mut() {
        *val = 0x2222_2222;
    }
    assert_eq!(store.active_pages(), 2, "expected exactly 2 active pages");

    store.reset();

    // Re-allocate: both pages should be lazily zeroed with correct used_bytes
    let fresh_first = store.alloc_slice::<u32>(16384);
    for (i, &val) in fresh_first.as_slice().iter().enumerate() {
        assert_eq!(val, 0, "first re-alloc: stale data at index {i}");
    }
    let fresh_second = store.alloc_slice::<u32>(100);
    for (i, &val) in fresh_second.as_slice().iter().enumerate() {
        assert_eq!(val, 0, "second re-alloc: stale data at index {i}");
    }
}

/// Lazy zeroing correctness: write a small allocation, reset, then allocate
/// a much larger slice from the recycled page. Bytes beyond the original
/// `used_bytes` must still be zero (from the initial Page::new calloc).
///
/// This covers the gap where `allocate_reset_cycle_always_zeroed` only
/// re-allocates the same size - it never tests the region *beyond* the
/// previous `used_bytes` boundary on a reused page.
#[test]
fn lazy_zeroing_small_then_large_reallocation() {
    let mut store = PageStore::new();

    // Small allocation: 10 u32s = 40 bytes used
    let mut small = store.alloc_slice::<u32>(10);
    for val in small.iter_mut() {
        *val = 0xDEAD_BEEF;
    }

    // Reset: page goes to free list with used_bytes=40
    store.reset();

    // Large re-allocation: 8000 u32s = 32000 bytes
    // Lazy zeroing only zeros bytes [0, 40); bytes [40, 32000) must be zero
    // from the original Page::new initialization.
    let large = store.alloc_slice::<u32>(8000);
    assert_eq!(store.stats().reused, 1, "should reuse the freed page");

    for (i, &val) in large.as_slice().iter().enumerate() {
        assert_eq!(
            val,
            0,
            "stale data at u32 index {i} (byte offset {}): got {val:#010x}",
            i * 4,
        );
    }
}

// ============== Property Tests ==============

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    /// Offset::get returns None for out-of-bounds and misaligned offsets,
    /// and Some for valid offsets.
    ///
    /// Exercises the bounds/alignment checks in Offset::get (page.rs:151-153)
    /// that guard the unsafe pointer dereference.
    #[test]
    fn offset_get_bounds_and_alignment(byte_offset in 0u32..70000) {
        let page = Page::new();
        let offset: Offset<u64> = Offset::new(byte_offset);

        let align = std::mem::align_of::<u64>();
        let size = std::mem::size_of::<u64>();
        let off = byte_offset as usize;

        // SAFETY: page is freshly allocated with zeroed memory; reading
        // zeroed bytes as u64 is safe.
        let result = unsafe { offset.get(&page) };

        if off >= PAGE_SIZE || !off.is_multiple_of(align) || off + size > PAGE_SIZE {
            prop_assert!(
                result.is_none(),
                "Offset {} should be None (PAGE_SIZE={}, align={}, size={})",
                byte_offset, PAGE_SIZE, align, size
            );
        } else {
            prop_assert!(
                result.is_some(),
                "Offset {} should be Some (within bounds and aligned)",
                byte_offset
            );
            // Fresh page is zeroed
            let result = result.expect("invariant: aligned in-bounds offset should be readable");
            prop_assert_eq!(*result, 0u64);
        }
    }

    /// Offset::get_mut/get roundtrip: write via get_mut, read via get.
    ///
    /// Exercises both unsafe dereference paths for valid offsets.
    #[test]
    fn offset_get_mut_roundtrip(
        // Use aligned offsets that fit u64
        slot in 0u32..8190,
    ) {
        let page = Page::new();
        let byte_offset = slot * 8; // u64-aligned
        let offset: Offset<u64> = Offset::new(byte_offset);

        if (byte_offset as usize) + 8 <= PAGE_SIZE {
            // SAFETY: page is zeroed, offset is aligned and in bounds,
            // and we have exclusive access in this test scope.
            let val_mut = unsafe { offset.get_mut(&page) };
            prop_assert!(val_mut.is_some(), "aligned offset {} should succeed", byte_offset);

            let val_mut = val_mut.expect("invariant: aligned in-bounds offset should be writable");
            *val_mut = 0xCAFE_BABE_DEAD_BEEFu64;

            // SAFETY: same page, same offset, data was just written.
            let val_ref = unsafe { offset.get(&page) };
            let val_ref = val_ref.expect("invariant: written offset should remain readable");
            prop_assert_eq!(*val_ref, 0xCAFE_BABE_DEAD_BEEFu64);
        }
    }
}

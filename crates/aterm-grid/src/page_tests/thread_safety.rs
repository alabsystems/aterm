// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::*;

// Compile-time Send/Sync guards: a field change that breaks the unsafe
// Send/Sync impls of Page or PageSlice fails to compile.

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

// Runtime thread safety tests (exercised under Miri)

/// Page can be moved to another thread (`unsafe impl Send for Page`).
#[test]
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

/// Allocated PageSlices can be moved to another thread and read
/// (`unsafe impl<T: Send> Send for PageSlice<T>`).
#[test]
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

// Sync impls: concurrent reads through shared references, checked under
// Miri's Stacked Borrows.

/// Several threads read one `Arc<Page>` concurrently (`unsafe impl Sync for Page`).
#[test]
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

/// Several threads read one `Arc<PageSlice<u32>>` concurrently
/// (`unsafe impl Sync for PageSlice<T>`); `as_slice()` goes through
/// `from_raw_parts`.
#[test]
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

// Offset::get_mut returns &mut T from &Page via UnsafeCell; two live get_mut
// results at the same offset would alias. These tests pin the sound patterns.

/// Sequential Offset::get_mut calls at non-overlapping offsets do not alias.
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

/// get_mut, drop, then get at the same offset is sound.
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

/// Every re-allocation after a reset is zeroed, across varying sizes; wrong
/// page_used_bytes tracking would leak stale data.
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

/// A larger allocation from a recycled page is zeroed beyond the previous
/// `used_bytes` too (from the initial Page::new calloc).
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

    // 8000 u32s: lazy zeroing covers bytes [0, 40); [40, 32000) is zero from
    // Page::new.
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

// ============== Exhaustive offset tests ==============

/// Offset::get returns None for out-of-bounds and misaligned offsets, and Some
/// for valid offsets — for EVERY byte offset up to past the page end.
///
/// Exercises the bounds/alignment checks in Offset::get (page.rs:151-153)
/// that guard the unsafe pointer dereference.
#[test]
fn offset_get_bounds_and_alignment() {
    let page = Page::new();
    let align = std::mem::align_of::<u64>();
    let size = std::mem::size_of::<u64>();
    for byte_offset in 0u32..70000 {
        let offset: Offset<u64> = Offset::new(byte_offset);
        let off = byte_offset as usize;
        // SAFETY: page is freshly allocated with zeroed memory; reading
        // zeroed bytes as u64 is safe.
        let result = unsafe { offset.get(&page) };
        if off >= PAGE_SIZE || !off.is_multiple_of(align) || off + size > PAGE_SIZE {
            assert!(result.is_none(), "Offset {byte_offset} should be None");
        } else {
            assert_eq!(result.copied(), Some(0u64), "Offset {byte_offset}");
        }
    }
}

/// Offset::get_mut/get roundtrip for every aligned slot: write via get_mut,
/// read via get.
#[test]
fn offset_get_mut_roundtrip() {
    let page = Page::new();
    for slot in 0u32..8190 {
        let byte_offset = slot * 8; // u64-aligned
        if (byte_offset as usize) + 8 > PAGE_SIZE {
            continue;
        }
        let offset: Offset<u64> = Offset::new(byte_offset);
        // SAFETY: the page is live, the offset is aligned and in bounds, and
        // no other reference into it is held.
        let val_mut = unsafe { offset.get_mut(&page) }
            .unwrap_or_else(|| panic!("aligned offset {byte_offset} should succeed"));
        *val_mut = 0xCAFE_BABE_DEAD_BEEFu64 ^ u64::from(slot);
        // SAFETY: same page, same offset, the &mut above is dead.
        let val_ref = unsafe { offset.get(&page) }.expect("written offset stays readable");
        assert_eq!(*val_ref, 0xCAFE_BABE_DEAD_BEEFu64 ^ u64::from(slot));
    }
}

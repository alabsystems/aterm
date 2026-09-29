// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::*;

#[test]
fn page_is_zeroed() {
    let page = Page::new();
    // SAFETY: We have exclusive access to the page data in this test.
    // Use data_ptr() and read through the raw pointer.
    let data = unsafe { std::slice::from_raw_parts(page.data_ptr(), PAGE_SIZE) };
    assert!(data.iter().all(|&b| b == 0));
}

#[test]
fn offset_roundtrip() {
    let page = Page::new();

    // Write a value
    let value: u32 = 0xDEADBEEF;
    let offset = Offset::<u32>::new(100);

    unsafe {
        *offset.get_mut(&page).expect("valid offset") = value;
        assert_eq!(*offset.get(&page).expect("valid offset"), value);
    }
}

#[test]
fn page_store_allocates_slices() {
    let mut store = PageStore::new();
    let slice = store.alloc_slice::<u32>(16);
    assert_eq!(slice.len_u16(), 16);
    assert!(slice.offset().byte_offset() < PAGE_SIZE as u32);
}

// === Memory pooling tests ===

#[test]
fn preheat_allocates_pages() {
    let mut store = PageStore::new();
    assert_eq!(store.free_pages(), 0);

    store.preheat(4);
    assert_eq!(store.free_pages(), 4);
    assert_eq!(store.stats().pages_allocated, 4);
    assert_eq!(store.stats().pages_free, 4);
}

#[test]
fn with_capacity_preheats() {
    let store = PageStore::with_capacity(3);
    assert_eq!(store.free_pages(), 3);
    assert_eq!(store.stats().pages_allocated, 3);
}

#[test]
fn alloc_uses_free_list() {
    let mut store = PageStore::new();
    store.preheat(2);
    assert_eq!(store.free_pages(), 2);

    // First allocation should use a preheated page
    let _slice1 = store.alloc_slice::<u32>(16);
    assert_eq!(store.free_pages(), 1);
    assert_eq!(store.active_pages(), 1);
    assert_eq!(store.stats().reused, 1);

    // Second allocation on same page shouldn't use another free page
    let _slice2 = store.alloc_slice::<u32>(16);
    assert_eq!(store.free_pages(), 1);
    assert_eq!(store.active_pages(), 1);
}

#[test]
fn alloc_new_page_when_full() {
    let mut store = PageStore::new();
    store.preheat(1);

    // Fill the first page (64KB / 4 bytes = 16384 u32s max)
    // Allocate slightly less to account for alignment
    let _slice1 = store.alloc_slice::<u32>(16000);
    assert_eq!(store.active_pages(), 1);
    assert_eq!(store.free_pages(), 0);

    // This should require a new page (no preheated pages left)
    let _slice2 = store.alloc_slice::<u32>(16000);
    assert_eq!(store.active_pages(), 2);
    assert_eq!(store.stats().pages_allocated, 2); // 1 preheated + 1 new
}

#[test]
fn reset_moves_pages_to_free_list() {
    let mut store = PageStore::new();

    // Allocate some data
    let _slice1 = store.alloc_slice::<u32>(100);
    let _slice2 = store.alloc_slice::<u32>(100);
    assert_eq!(store.active_pages(), 1);

    // Reset should move active pages to free list
    store.reset();
    assert_eq!(store.active_pages(), 0);
    assert_eq!(store.free_pages(), 1);

    // Next allocation should reuse the freed page
    let _slice3 = store.alloc_slice::<u32>(100);
    assert_eq!(store.active_pages(), 1);
    assert_eq!(store.free_pages(), 0);
    assert_eq!(store.stats().reused, 1);
}

#[test]
fn shrink_to_fit_releases_free_pages() {
    let mut store = PageStore::new();
    store.preheat(5);
    assert_eq!(store.free_pages(), 5);
    assert_eq!(store.total_memory(), 5 * PAGE_SIZE);

    store.shrink_to_fit();
    assert_eq!(store.free_pages(), 0);
    assert_eq!(store.total_memory(), 0);
}

#[test]
fn total_memory_tracks_all_pages() {
    let mut store = PageStore::new();
    store.preheat(2);
    assert_eq!(store.total_memory(), 2 * PAGE_SIZE);

    // Allocate uses one preheated page
    let _slice = store.alloc_slice::<u32>(100);
    // 1 active + 1 free = 2 total
    assert_eq!(store.total_memory(), 2 * PAGE_SIZE);

    // Large allocation still fits in the active preheated page.
    let _big_slice = store.alloc_slice::<u32>(16000);
    assert_eq!(store.total_memory(), 2 * PAGE_SIZE);
}

#[test]
fn stats_tracking_accurate() {
    let mut store = PageStore::new();

    // Preheat
    store.preheat(2);
    let stats = store.stats();
    assert_eq!(stats.pages_allocated, 2);
    assert_eq!(stats.pages_free, 2);
    assert_eq!(stats.pages_in_use, 0);
    assert_eq!(stats.allocations, 0);
    assert_eq!(stats.reused, 0);

    // Allocate
    let _slice = store.alloc_slice::<u32>(100);
    let stats = store.stats();
    assert_eq!(stats.pages_in_use, 1);
    assert_eq!(stats.pages_free, 1);
    assert_eq!(stats.allocations, 1);
    assert_eq!(stats.reused, 1); // Used from free list

    // Allocate again (same page)
    let _slice2 = store.alloc_slice::<u32>(100);
    let stats = store.stats();
    assert_eq!(stats.allocations, 2);
    assert_eq!(stats.reused, 1); // Still 1, no new page needed
}

#[test]
fn reused_pages_are_zeroed() {
    let mut store = PageStore::new();

    // Allocate and write data
    let mut slice = store.alloc_slice::<u32>(10);
    for i in 0..10 {
        slice[i] = 0xDEADBEEF;
    }

    // Reset and reallocate
    store.reset();
    let slice2 = store.alloc_slice::<u32>(10);

    // New slice should be zeroed
    for &val in slice2.iter() {
        assert_eq!(val, 0);
    }
}

#[test]
fn lazy_zeroing_only_zeros_used_portion() {
    let mut store = PageStore::new();

    // Allocate a small amount (400 bytes = 100 u32s)
    let mut slice = store.alloc_slice::<u32>(100);
    for i in 0..100 {
        slice[i] = 0xDEADBEEF;
    }

    // Reset - this should track that only ~400 bytes were used
    store.reset();
    assert_eq!(store.free_pages(), 1);

    // The free page should have used_bytes tracking
    // When we allocate again, only the used portion gets zeroed

    // Allocate again - should reuse the page
    let slice2 = store.alloc_slice::<u32>(100);
    assert_eq!(store.stats().reused, 1);

    // Verify the used portion is zeroed
    for &val in slice2.iter() {
        assert_eq!(val, 0);
    }
}

#[test]
fn lazy_zeroing_handles_multiple_pages() {
    let mut store = PageStore::new();

    // Fill first page partially and force a second page
    let mut slice1 = store.alloc_slice::<u32>(1000); // 4000 bytes
    for i in 0..1000 {
        slice1[i] = 0xAAAAAAAA;
    }

    let mut slice2 = store.alloc_slice::<u32>(16000); // Forces new page
    for i in 0..16000 {
        slice2[i] = 0xBBBBBBBB;
    }

    assert_eq!(store.active_pages(), 2);

    // Reset and reallocate
    store.reset();
    assert_eq!(store.free_pages(), 2);

    // Allocate and verify zeroing
    let new_slice = store.alloc_slice::<u32>(1000);
    for &val in new_slice.iter() {
        assert_eq!(val, 0);
    }
}

#[test]
fn page_used_bytes_tracked_correctly() {
    let mut store = PageStore::new();

    // Allocate some data
    let _s1 = store.alloc_slice::<u32>(100); // 400 bytes
    let _s2 = store.alloc_slice::<u32>(100); // 400 more bytes
    // next_offset should be 800

    // Force new page - need allocation that exceeds remaining space
    // PAGE_SIZE = 65536, used = 800, remaining = 64736
    // 16384 u32s = 65536 bytes, which exceeds remaining 64736 bytes
    let _s3 = store.alloc_slice::<u32>(16384); // Forces new page
    // First page's used bytes should be tracked in page_used_bytes

    assert_eq!(store.active_pages(), 2);
    assert_eq!(store.tracked_page_count(), 1); // One completed page tracked

    // Reset
    store.reset();

    // Both pages should be in free list with their used_bytes
    assert_eq!(store.free_pages(), 2);
    assert_eq!(store.tracked_page_count(), 0); // Cleared after reset
}

#[test]
fn recycled_row_cells_are_cleared_unique_and_width_matched() {
    use crate::{Cell, Row};
    let mut pages = PageStore::new();
    // SAFETY: all rows below are dropped before their owning pages.
    let mut old = unsafe { Row::new(80, &mut pages) };
    old.set(0, Cell::new('X'));
    let old_ptr = old.as_slice().as_ptr();
    let page_bytes = pages.total_memory();
    // SAFETY: old belongs to pages and is consumed, so its range is unaliased.
    unsafe { old.recycle(&mut pages) };
    assert!(
        pages.total_memory() > page_bytes,
        "returned-token metadata is charged"
    );
    let allocated = pages.stats().allocations;
    // SAFETY: same owning arena, outliving both returned rows.
    let mut other = unsafe { Row::new(81, &mut pages) };
    assert_ne!(other.as_slice().as_ptr(), old_ptr);
    assert_eq!(
        pages.recycled_rows(),
        1,
        "a different width cannot consume it"
    );
    // SAFETY: same owning arena, outliving the returned row.
    let mut recycled = unsafe { Row::new(80, &mut pages) };
    assert_eq!(recycled.as_slice().as_ptr(), old_ptr);
    assert_eq!(pages.stats().allocations, allocated + 1);
    assert_eq!(recycled.len(), 0);
    assert!(recycled.as_slice().iter().all(|cell| *cell == Cell::EMPTY));
    recycled.set(0, Cell::new('A'));
    other.set(0, Cell::new('B'));
    assert_eq!(recycled.get(0).unwrap().char(), 'A');
    assert_eq!(other.get(0).unwrap().char(), 'B');
}

#[test]
fn resetting_pages_discards_recycled_handles_before_page_reuse() {
    use crate::{Cell, Row};
    let mut pages = PageStore::new();
    // SAFETY: the row is consumed before reset invalidates any cell ranges.
    let row = unsafe { Row::new(80, &mut pages) };
    // SAFETY: pages owns row, consumed here without retained references.
    unsafe { row.recycle(&mut pages) };
    assert_eq!(pages.recycled_rows(), 1);
    pages.reset();
    assert_eq!(pages.recycled_rows(), 0);
    // SAFETY: both new rows are dropped before pages, with fresh unique ranges.
    let mut first = unsafe { Row::new(80, &mut pages) };
    // SAFETY: same owning arena, outliving the returned row.
    let mut second = unsafe { Row::new(80, &mut pages) };
    assert_ne!(first.as_slice().as_ptr(), second.as_slice().as_ptr());
    first.set(0, Cell::new('A'));
    second.set(0, Cell::new('B'));
    assert_eq!(first.get(0).unwrap().char(), 'A');
    assert_eq!(second.get(0).unwrap().char(), 'B');
}

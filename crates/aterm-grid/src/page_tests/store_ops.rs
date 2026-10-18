// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::*;

// Memory pooling.

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

    let _slice = store.alloc_slice::<u32>(100);
    // 1 active + 1 free = 2 total
    assert_eq!(store.total_memory(), 2 * PAGE_SIZE);

    // Large allocation still fits in the active preheated page.
    let _big_slice = store.alloc_slice::<u32>(16000);
    assert_eq!(store.total_memory(), 2 * PAGE_SIZE);
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

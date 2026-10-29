// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Page-backed allocator with memory pooling; see [`PageStore`].

use std::ptr::NonNull;

use crate::cell::Cell;

#[cfg(test)]
use super::Offset;
use super::{PAGE_SIZE, Page, PageSlice};

/// Statistics for memory pool usage.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PoolStats {
    /// Total pages allocated (including freed).
    pub pages_allocated: usize,
    /// Pages currently in use.
    pub pages_in_use: usize,
    /// Pages in free list (available for reuse).
    pub pages_free: usize,
    /// Total allocations performed.
    pub allocations: usize,
    /// Allocations satisfied from free list (no new memory).
    pub reused: usize,
}

/// Page-backed allocator with memory pooling.
#[derive(Debug, Default)]
pub struct PageStore {
    recycled_cells: Vec<PageSlice<Cell>>,
    pages: Vec<Box<Page>>,
    free_pages: Vec<Box<Page>>,
    current_page: usize,
    next_offset: usize,
    stats: PoolStats,
}

impl PageStore {
    /// Create a new, empty page store.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ResizeRowReuse",
            action = "Rebuild",
            project = "grid::tests::resize_row_reuse::resize_row_reuse_conforms"
        )
    )]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a page store with the free list pre-filled with `page_count` pages.
    #[must_use]
    pub(crate) fn with_capacity(page_count: usize) -> Self {
        let mut store = Self::new();
        store.preheat(page_count);
        store
    }

    /// Pre-allocate pages into the free list (64KB each).
    pub(crate) fn preheat(&mut self, page_count: usize) {
        for _ in 0..page_count {
            let page = Page::new();
            self.free_pages.push(page);
            self.stats.pages_allocated += 1;
            self.stats.pages_free += 1;
        }
    }

    /// Get pool statistics.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) const fn stats(&self) -> PoolStats {
        self.stats
    }

    /// Number of active pages (holding allocations).
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) const fn active_pages(&self) -> usize {
        self.pages.len()
    }

    /// Number of free pages (available for reuse).
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) const fn free_pages(&self) -> usize {
        self.free_pages.len()
    }

    /// Total memory used by all pages (active + free), in bytes.
    #[must_use]
    #[inline]
    pub(crate) const fn total_memory(&self) -> usize {
        self.pages
            .len()
            .saturating_add(self.free_pages.len())
            .saturating_mul(PAGE_SIZE)
            .saturating_add(
                self.recycled_cells
                    .capacity()
                    .saturating_mul(std::mem::size_of::<PageSlice<Cell>>()),
            )
    }

    /// Release all free pages back to the system.
    pub(crate) fn shrink_to_fit(&mut self) {
        self.stats.pages_free = 0;
        self.free_pages.clear();
        self.free_pages.shrink_to_fit();
    }

    fn alloc_page(&mut self) -> Box<Page> {
        if let Some(page) = self.free_pages.pop() {
            self.stats.pages_free -= 1;
            self.stats.reused += 1;
            page
        } else {
            self.stats.pages_allocated += 1;
            Page::new()
        }
    }

    /// Allocate row cells, consuming a returned slice of the exact width first.
    pub(crate) fn alloc_cells(&mut self, cols: u16) -> PageSlice<Cell> {
        if let Some(index) = self
            .recycled_cells
            .iter()
            .rposition(|cells| cells.len() == usize::from(cols))
        {
            return self.recycled_cells.swap_remove(index);
        }
        self.alloc_slice(cols)
    }

    /// Return a removed row's unique allocation to this arena.
    /// # Safety
    /// `cells` must have been allocated by this live PageStore, and no other handle or reference
    /// may retain access to its range after this transfer.
    pub(crate) unsafe fn recycle_cells(&mut self, cells: PageSlice<Cell>) {
        self.recycled_cells.push(cells);
    }

    #[cfg(test)]
    #[inline]
    pub(crate) const fn recycled_rows(&self) -> usize {
        self.recycled_cells.len()
    }

    /// Allocate a typed slice within the page store.
    #[cfg(any(test, debug_assertions))]
    #[allow(clippy::expect_used)]
    #[inline]
    pub fn alloc_slice<T>(&mut self, len: u16) -> PageSlice<T> {
        self.alloc_slice_impl(len)
    }

    /// Allocate a typed slice within the page store (crate-internal in production).
    #[cfg(not(any(test, debug_assertions)))]
    #[allow(clippy::expect_used)]
    #[inline]
    pub(crate) fn alloc_slice<T>(&mut self, len: u16) -> PageSlice<T> {
        self.alloc_slice_impl(len)
    }

    #[allow(clippy::expect_used)] // Assertions: overflow = programmer error, NonNull = safety invariant.
    fn alloc_slice_impl<T>(&mut self, len: u16) -> PageSlice<T> {
        let len_usize = len as usize;
        let bytes = len_usize
            .checked_mul(std::mem::size_of::<T>())
            .expect("page allocation overflow");
        assert!(bytes <= PAGE_SIZE, "allocation exceeds page size");

        let align = std::mem::align_of::<T>();
        let mut offset = align_up(self.next_offset, align);

        if self.pages.is_empty() || offset + bytes > PAGE_SIZE {
            let page = self.alloc_page();
            self.pages.push(page);
            self.current_page = self.pages.len() - 1;
            self.stats.pages_in_use += 1;
            offset = 0;
        }

        let page_id = self.current_page;
        let base_ptr = self.pages[page_id].data_ptr();
        // SAFETY: offset is aligned to T and offset + size_of::<T>() * len <= PAGE_SIZE (the size
        // assert and the new-page check above); base_ptr is valid for the Page's life.
        let ptr = unsafe { base_ptr.add(offset).cast::<T>() };
        let ptr = NonNull::new(ptr).expect("page slice pointer is null");

        self.next_offset = offset + bytes;
        self.stats.allocations += 1;

        PageSlice {
            ptr,
            len,
            #[cfg(any(test, debug_assertions))]
            page_id,
            #[cfg(test)]
            offset: Offset::new(u32::try_from(offset).unwrap_or(u32::MAX)),
        }
    }
}

#[inline]
const fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

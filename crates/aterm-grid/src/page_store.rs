// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Page-backed allocator with memory pooling.
//!
//! See [`PageStore`] for pooling strategy and lazy zeroing optimization.

use std::ptr::NonNull;

#[cfg(any(test, kani, feature = "testing"))]
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
///
/// ## Pooling Strategy
///
/// - Pages that would be deallocated are instead added to a free list
/// - New allocations first check the free list before allocating
/// - Grid constructors can pre-allocate pages internally to avoid runtime allocations
/// - The pool can grow unbounded; use `shrink_to_fit()` to release unused pages
///
/// ## Lazy Zeroing Optimization
///
/// Pages are only zeroed up to the amount actually used (`next_offset`), not the
/// full 64KB. This is tracked per-page using `page_used_bytes`. When a page is
/// recycled, only the used portion is zeroed, reducing write overhead significantly
/// for typical allocations that don't fill entire pages.
#[derive(Debug, Default)]
pub struct PageStore {
    /// Active pages (currently holding allocations).
    pages: Vec<Box<Page>>,
    /// Free list of recycled pages (available for reuse).
    /// Each entry is (page, used_bytes) where used_bytes tracks how much was allocated.
    free_pages: Vec<(Box<Page>, usize)>,
    /// Current page index for allocations.
    current_page: usize,
    /// Next offset within current page.
    next_offset: usize,
    /// Pool statistics.
    stats: PoolStats,
    /// Track bytes used per active page for partial zeroing on recycle.
    page_used_bytes: Vec<usize>,
}

impl PageStore {
    /// Create a new, empty page store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a page store with pre-allocated pages.
    ///
    /// This eliminates allocation latency during normal operation by
    /// pre-heating the free list with the specified number of pages.
    #[must_use]
    pub(crate) fn with_capacity(page_count: usize) -> Self {
        let mut store = Self::new();
        store.preheat(page_count);
        store
    }

    /// Pre-allocate pages into the free list.
    ///
    /// Call this during initialization to avoid allocation during hot paths.
    /// Each page is 64KB, so `preheat(10)` allocates 640KB.
    ///
    /// Crate-internal in production builds. Exposed under `cfg(miri)` so the
    /// memory-safety integration tests can exercise free-list reuse directly.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aterm_grid::PageStore;
    ///
    /// let mut store = PageStore::new();
    /// store.preheat(4);
    /// ```
    #[cfg(miri)]
    pub fn preheat(&mut self, page_count: usize) {
        self.preheat_impl(page_count);
    }

    #[cfg(not(miri))]
    pub(crate) fn preheat(&mut self, page_count: usize) {
        self.preheat_impl(page_count);
    }

    fn preheat_impl(&mut self, page_count: usize) {
        for _ in 0..page_count {
            let page = Page::new();
            // Fresh pages are already zeroed, used_bytes = 0
            self.free_pages.push((page, 0));
            self.stats.pages_allocated += 1;
            self.stats.pages_free += 1;
        }
    }

    /// Get pool statistics.
    #[cfg(any(test, kani))]
    #[must_use]
    pub(crate) fn stats(&self) -> PoolStats {
        self.stats
    }

    /// Number of active pages (holding allocations).
    #[cfg(any(test, kani))]
    #[must_use]
    pub(crate) fn active_pages(&self) -> usize {
        self.pages.len()
    }

    /// Number of free pages (available for reuse).
    #[cfg(any(test, kani))]
    #[must_use]
    pub(crate) fn free_pages(&self) -> usize {
        self.free_pages.len()
    }

    /// Number of completed pages with tracked used-byte metadata.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn tracked_page_count(&self) -> usize {
        self.page_used_bytes.len()
    }

    /// Total memory used by all pages (active + free), in bytes.
    #[must_use]
    pub(crate) fn total_memory(&self) -> usize {
        (self.pages.len() + self.free_pages.len()) * PAGE_SIZE
    }

    /// Release all free pages back to the system.
    ///
    /// This reduces memory usage but may cause future allocations to be slower.
    /// Call after operations that reduce grid size to reclaim freed page memory.
    pub(crate) fn shrink_to_fit(&mut self) {
        self.stats.pages_free = 0;
        self.free_pages.clear();
        self.free_pages.shrink_to_fit();
    }

    /// Reset the page store, moving all active pages to the free list.
    ///
    /// This is useful when clearing the terminal without deallocating memory.
    /// All existing `PageSlice` references become invalid after this call.
    ///
    /// # Safety
    ///
    /// Caller must ensure no `PageSlice` references are used after reset.
    #[cfg(any(test, kani, feature = "testing"))]
    pub fn reset(&mut self) {
        // Build used-byte metadata in page order so it stays aligned when
        // pages are popped in reverse order below.
        let mut used_bytes_by_page = std::mem::take(&mut self.page_used_bytes);
        if !self.pages.is_empty() {
            used_bytes_by_page.push(self.next_offset);
        }
        debug_assert_eq!(used_bytes_by_page.len(), self.pages.len());

        // Move all active pages to free list with their used byte counts.
        while let Some(page) = self.pages.pop() {
            // If metadata is missing, zero the full page on reuse as a safe fallback.
            let used_bytes = used_bytes_by_page.pop().unwrap_or(PAGE_SIZE);
            self.free_pages.push((page, used_bytes));
            self.stats.pages_in_use -= 1;
            self.stats.pages_free += 1;
        }
        self.current_page = 0;
        self.next_offset = 0;
    }

    /// Allocate a fresh page from the pool.
    ///
    /// Returns a page from the free list if available, otherwise allocates new.
    /// Uses lazy zeroing: only zeros the portion that was actually used.
    fn alloc_page(&mut self) -> Box<Page> {
        if let Some((page, used_bytes)) = self.free_pages.pop() {
            // Reuse from free list - only zero the used portion (lazy zeroing)
            // This is a significant optimization: instead of writing 64KB,
            // we only write the portion that was actually used.
            if used_bytes > 0 {
                // Zero only the used portion through UnsafeCell
                // SAFETY: We have exclusive access to the page (it's from the free list)
                // and no PageSlice references exist to this page's data.
                unsafe {
                    let data = &mut *page.data.get();
                    data[..used_bytes].fill(0);
                }
            }
            self.stats.pages_free -= 1;
            self.stats.reused += 1;
            page
        } else {
            // Allocate fresh page (already zeroed by Page::new)
            self.stats.pages_allocated += 1;
            Page::new()
        }
    }

    /// Allocate a typed slice within the page store.
    ///
    /// Crate-internal in production builds (#5573). Accessible externally under
    /// `feature = "testing"` for downstream property tests and Kani scaffolding.
    #[cfg(any(test, kani, feature = "testing"))]
    #[allow(clippy::expect_used)]
    pub fn alloc_slice<T>(&mut self, len: u16) -> PageSlice<T> {
        self.alloc_slice_impl(len)
    }

    /// Allocate a typed slice within the page store (crate-internal in production).
    #[cfg(not(any(test, kani, feature = "testing")))]
    #[allow(clippy::expect_used)]
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
            // Save the used bytes for the current page before moving to a new one
            if !self.pages.is_empty() {
                self.page_used_bytes.push(self.next_offset);
            }
            // Need a new page - try free list first
            let page = self.alloc_page();
            self.pages.push(page);
            self.current_page = self.pages.len() - 1;
            self.stats.pages_in_use += 1;
            offset = 0;
        }

        let page_id = self.current_page;
        // Use data_ptr() which goes through UnsafeCell, avoiding borrow invalidation
        // under Stacked/Tree Borrows when other pages are accessed later.
        let base_ptr = self.pages[page_id].data_ptr();
        // SAFETY: offset is aligned to T and offset + size_of::<T>() * len <= PAGE_SIZE,
        // verified by the loop above. base_ptr is valid for the lifetime of the Page.
        let ptr = unsafe { base_ptr.add(offset).cast::<T>() };
        let ptr = NonNull::new(ptr).expect("page slice pointer is null");

        self.next_offset = offset + bytes;
        self.stats.allocations += 1;

        PageSlice {
            ptr,
            len,
            page_id,
            #[cfg(any(test, kani, feature = "testing"))]
            // offset bounded by PAGE_SIZE (64KB) — always fits in u32
            offset: Offset::new(u32::try_from(offset).unwrap_or(u32::MAX)),
        }
    }
}

fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

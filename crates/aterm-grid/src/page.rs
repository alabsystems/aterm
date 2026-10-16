// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Page storage: 64 KB pages that [`PageStore`] bump-allocates typed
//! [`PageSlice`]s from, with a free list of recycled pages
//! ([`PageStore::preheat`] fills it ahead of use).
//!
//! [`Page`] is crate-internal. [`PageSlice`] is reachable for downstream test
//! scaffolding under `debug_assertions` but is not re-exported at the crate
//! root or through the `aterm-core` facade; production consumers use
//! [`Row`](crate::Row) and [`PageStore`].
#![allow(
    clippy::large_stack_arrays,
    reason = "64KB page buffers are intentional for memory-mapped scrollback"
)]

use std::cell::UnsafeCell;
#[cfg(any(test, debug_assertions))]
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// Page size: 64 KB.
pub const PAGE_SIZE: usize = 64 * 1024;

/// A 64 KB page of terminal data that [`PageStore`] bump-allocates slices from.
#[repr(C, align(4096))]
pub(crate) struct Page {
    /// `UnsafeCell` so pointers derived from the data stay valid while other
    /// parts of the `PageStore` are accessed.
    data: UnsafeCell<[u8; PAGE_SIZE]>,
}

// SAFETY: `Page` is `Send`: its only field is plain bytes with no thread-affine
// resources, and `PageStore` hands each byte range to at most one `PageSlice`.
unsafe impl Send for Page {}

// SAFETY: `Page` is `Sync`: its bytes are reached only through `PageSlice`s over
// disjoint ranges (`as_slice` under `&self`, `as_mut_slice` under `&mut self`),
// `alloc_page`'s zeroing under `&mut PageStore`, and test code's raw `data_ptr`
// accesses, whose callers guarantee exclusivity; no byte range is mutated concurrently.
unsafe impl Sync for Page {}

fn _assert_send<T: Send>() {}
fn _assert_sync<T: Sync>() {}
fn _guard_page() {
    _assert_send::<Page>();
    _assert_sync::<Page>();
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("data", &format_args!("UnsafeCell<[u8; {PAGE_SIZE}]>"))
            .finish()
    }
}

impl Page {
    /// Create a new zeroed page.
    #[must_use]
    pub(crate) fn new() -> Box<Self> {
        Box::default()
    }

    /// Get a mutable pointer to the page data.
    ///
    /// Writing through it requires that no other reference to the bytes exists.
    #[inline]
    pub(crate) fn data_ptr(&self) -> *mut u8 {
        self.data.get().cast()
    }
}

impl Default for Page {
    fn default() -> Self {
        Self {
            data: UnsafeCell::new([0; PAGE_SIZE]),
        }
    }
}

/// A byte offset into a page (test scaffolding): an index, not a pointer.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Offset<T> {
    /// Byte offset into the page.
    byte_offset: u32,
    /// Phantom data for type safety.
    _marker: PhantomData<T>,
}

// Manual Copy/Clone: derive would require `T: Copy`.
#[cfg(any(test, debug_assertions))]
impl<T> Copy for Offset<T> {}
#[cfg(any(test, debug_assertions))]
impl<T> Clone for Offset<T> {
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(any(test, debug_assertions))]
impl<T> Offset<T> {
    /// Create an offset.
    #[must_use]
    pub const fn new(byte_offset: u32) -> Self {
        Self {
            byte_offset,
            _marker: PhantomData,
        }
    }

    /// Get the byte offset.
    #[cfg(test)]
    #[must_use]
    pub const fn byte_offset(self) -> u32 {
        self.byte_offset
    }
}

/// Logical page identifier.
pub type PageId = usize;

/// A typed slice allocated within a page. Production consumers use
/// [`Row`](crate::Row) as their safe boundary.
#[derive(Debug)]
pub struct PageSlice<T> {
    ptr: NonNull<T>,
    len: u16,
    page_id: PageId,
    #[cfg(test)]
    offset: Offset<T>,
}

impl<T> PageSlice<T> {
    /// Length of the slice (u16).
    #[cfg(test)]
    #[must_use]
    pub const fn len_u16(&self) -> u16 {
        self.len
    }

    /// Page ID for this slice.
    #[must_use]
    pub const fn page_id(&self) -> PageId {
        self.page_id
    }

    /// Offset within the page.
    #[cfg(test)]
    #[must_use]
    pub const fn offset(&self) -> Offset<T> {
        self.offset
    }

    /// View the slice as a shared reference.
    #[must_use]
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        let len = self.len as usize;
        // SAFETY: ptr/len are validated at allocation time.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), len) }
    }

    /// View the slice as a mutable reference.
    #[must_use]
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        let len = self.len as usize;
        // SAFETY: ptr/len are validated at allocation time and uniquely owned.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), len) }
    }
}

impl<T> Deref for PageSlice<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T> DerefMut for PageSlice<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

// SAFETY: `PageSlice<T>` is `Send` when `T: Send`: it uniquely owns its `[T]` range
// (`PageStore::alloc_slice` hands out disjoint ranges) inside a `Page`, which is `Send`
// and outlives its slices (`Row::new`'s contract). Moving it moves that ownership; `len`
// and `page_id` are plain data.
unsafe impl<T: Send> Send for PageSlice<T> {}

// SAFETY: `PageSlice<T>` is `Sync` when `T: Sync`: shared access only yields `&[T]`
// (`as_slice`), and `as_mut_slice` needs `&mut PageSlice<T>`, which the borrow checker
// keeps exclusive.
unsafe impl<T: Sync> Sync for PageSlice<T> {}

#[path = "page_store.rs"]
mod page_store;
pub use page_store::PageStore;

#[cfg(test)]
#[path = "page_tests/mod.rs"]
mod tests;

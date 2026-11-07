// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Page storage: 64 KB pages that [`PageStore`] bump-allocates typed [`PageSlice`](crate::page::PageSlice)s from.
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

// SAFETY: `Page` is `Send`: its only field is plain bytes with no thread-affine resources, and
// `PageStore` hands each byte range to at most one `PageSlice`.
unsafe impl Send for Page {}

// SAFETY: `Page` is `Sync`: its bytes are reached only through `PageSlice`s over disjoint ranges
// (`as_slice` under `&self`, `as_mut_slice` under `&mut self`) and test code's raw `data_ptr`
// accesses, whose callers guarantee exclusivity; no byte range is mutated concurrently.
unsafe impl Sync for Page {}

const fn _assert_send<T: Send>() {}
const fn _assert_sync<T: Sync>() {}
const fn _guard_page() {
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
    pub(crate) const fn data_ptr(&self) -> *mut u8 {
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
    #[inline]
    pub const fn new(byte_offset: u32) -> Self {
        Self {
            byte_offset,
            _marker: PhantomData,
        }
    }

    /// Get the byte offset.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub const fn byte_offset(self) -> u32 {
        self.byte_offset
    }
}

/// Logical page identifier.
pub type PageId = usize;

/// A typed slice allocated within a page.
#[derive(Debug)]
pub struct PageSlice<T> {
    ptr: NonNull<T>,
    len: u16,
    /// The elements that may have been written since [`Self::take_touched`] last took them,
    /// `touched_lo..touched_hi` (none when `touched_lo >= touched_hi`): every mutable access
    /// notes what it reaches, a [`DerefMut`] view the whole slice, so an element outside it
    /// holds what it held then. A new slice is touched whole.
    touched_lo: u16,
    touched_hi: u16,
    #[cfg(any(test, debug_assertions))]
    page_id: PageId,
    #[cfg(test)]
    offset: Offset<T>,
}

impl<T> PageSlice<T> {
    /// Length of the slice (u16).
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub const fn len_u16(&self) -> u16 {
        self.len
    }

    /// Page ID for this slice.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn page_id(&self) -> PageId {
        self.page_id
    }

    /// Offset within the page.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub const fn offset(&self) -> Offset<T> {
        self.offset
    }

    /// View the slice as a shared reference.
    #[must_use]
    #[inline]
    pub const fn as_slice(&self) -> &[T] {
        let len = self.len as usize;
        // SAFETY: ptr/len are validated at allocation time.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), len) }
    }

    /// View the slice as a mutable reference, noting it touched whole.
    #[must_use]
    #[inline]
    pub const fn as_mut_slice(&mut self) -> &mut [T] {
        self.touched_lo = 0;
        self.touched_hi = self.len;
        self.untracked_mut()
    }

    /// The whole slice, mutably, with nothing noted: only for a caller that notes what it
    /// writes.
    #[inline]
    const fn untracked_mut(&mut self) -> &mut [T] {
        let len = self.len as usize;
        // SAFETY: ptr/len are validated at allocation time and uniquely owned.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), len) }
    }

    /// Notes the elements `lo..hi`, which the caller keeps within the slice, as touched, with no
    /// branch: an empty range may widen what is noted, which only makes a reader look at more.
    /// Each access notes what it reached once its bounds are known good.
    #[inline]
    fn touch(&mut self, lo: usize, hi: usize) {
        #[cfg(test)]
        if FORGET_TOUCHES.get() {
            return;
        }
        debug_assert!(
            lo <= hi && hi <= usize::from(self.len),
            "a noted range is in the slice"
        );
        #[expect(
            clippy::cast_possible_truncation,
            reason = "lo <= hi <= len, which is a u16"
        )]
        let (lo, hi) = (lo as u16, hi as u16);
        self.touched_lo = self.touched_lo.min(lo);
        self.touched_hi = self.touched_hi.max(hi);
    }

    /// The elements in `range`, mutably, noting them touched once (a run writer's one note for
    /// its run). Panics as indexing does when `range` is out of bounds.
    #[inline]
    pub(crate) fn slice_mut(&mut self, range: std::ops::Range<usize>) -> &mut [T] {
        let (lo, hi) = (range.start, range.end);
        let _ = &self.as_slice()[range];
        self.touch(lo, hi);
        // SAFETY: indexing the same range above did not panic, so it is in bounds.
        unsafe { self.untracked_mut().get_unchecked_mut(lo..hi) }
    }

    /// The element at `index`, mutably, noting it touched.
    #[inline]
    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= usize::from(self.len) {
            return None;
        }
        self.touch(index, index + 1);
        // SAFETY: `index` is below the length.
        Some(unsafe { self.untracked_mut().get_unchecked_mut(index) })
    }

    /// The element at `index`, mutably and unchecked, noting it touched.
    /// # Safety
    /// `index` must be below the slice's length.
    #[inline]
    pub(crate) unsafe fn get_unchecked_mut(&mut self, index: usize) -> &mut T {
        self.touch(index, index + 1);
        // SAFETY: the caller keeps `index` below the length.
        unsafe { self.untracked_mut().get_unchecked_mut(index) }
    }

    /// Sets the element at `index` to `value`, noting it touched. Panics as indexing does when
    /// `index` is out of bounds.
    #[inline]
    pub(crate) fn set(&mut self, index: usize, value: T) {
        self.untracked_mut()[index] = value;
        self.touch(index, index + 1);
    }

    /// Copies `src` to `dest` within the slice (`<[T]>::copy_within`), noting what it writes.
    #[inline]
    pub(crate) fn copy_within(&mut self, src: std::ops::Range<usize>, dest: usize)
    where
        T: Copy,
    {
        let n = src.len();
        self.untracked_mut().copy_within(src, dest);
        self.touch(dest, dest + n);
    }

    /// The elements touched since the last take, as a range, and forgets them.
    #[inline]
    pub(crate) fn take_touched(&mut self) -> std::ops::Range<usize> {
        let touched = usize::from(self.touched_lo)..usize::from(self.touched_hi);
        self.touched_lo = u16::MAX;
        self.touched_hi = 0;
        if touched.is_empty() { 0..0 } else { touched }
    }
}

#[cfg(test)]
thread_local! {
    static FORGET_TOUCHES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While `on`, the precise writes on this thread note nothing: the planted miss the touched-span
/// test must catch.
#[cfg(test)]
pub(crate) fn forget_touches(on: bool) {
    FORGET_TOUCHES.set(on);
}

impl<T> Deref for PageSlice<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T> DerefMut for PageSlice<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

// SAFETY: `PageSlice<T>` is `Send` when `T: Send`: it uniquely owns its `[T]` range
// (`PageStore::alloc_slice` hands out disjoint ranges) inside a `Page`, which is `Send` and
// outlives its slices (`Row::new`'s contract). Moving it moves that ownership; `len` and `page_id`
// are plain data.
unsafe impl<T: Send> Send for PageSlice<T> {}

// SAFETY: `PageSlice<T>` is `Sync` when `T: Sync`: shared access only yields `&[T]` (`as_slice`),
// and `as_mut_slice` needs `&mut PageSlice<T>`, which the borrow checker keeps exclusive.
unsafe impl<T: Sync> Sync for PageSlice<T> {}

#[path = "page_store.rs"]
mod page_store;
pub use page_store::PageStore;

#[cfg(test)]
#[path = "page_tests/mod.rs"]
mod tests;

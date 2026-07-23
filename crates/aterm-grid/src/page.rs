// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Offset-based page storage with memory pooling.
//!
//! Pages use offsets instead of pointers, enabling:
//! - Direct serialization to disk
//! - Memory-mapping without fixup
//! - Network transmission for sync
//!
//! ## Memory Pooling
//!
//! The [`PageStore`] maintains a free list of recycled pages to avoid
//! allocation overhead in hot paths. When pages are freed, they're added
//! to the free list. New allocations first check the free list before
//! allocating fresh memory.
//!
//! Use [`PageStore::preheat`] to pre-allocate pages during initialization,
//! eliminating allocation latency during normal operation.
//!
//! ## API Safety (#5573)
//!
//! [`Page`] is crate-internal — external code cannot construct or hold raw
//! pages. [`PageSlice`] is accessible for downstream test scaffolding via
//! `feature = "testing"`, but is not re-exported at the crate root or
//! through the `aterm-core` facade. Production consumers use [`Row`] and
//! [`PageStore`] as their safe API boundary.
//!
#![allow(
    clippy::large_stack_arrays,
    reason = "64KB page buffers are intentional for memory-mapped scrollback"
)]

use std::cell::UnsafeCell;
#[cfg(any(test, kani, feature = "testing"))]
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// Page size: 64 KB in production, 256 bytes under Kani.
///
/// CBMC cannot model the full 64KB array symbolically — verification
/// times out at 300s+ even with preheat bounds reduced to 2. 256 bytes
/// is sufficient to verify offset arithmetic, allocation bounds, and
/// memory pooling invariants (all are PAGE_SIZE-relative, not absolute).
/// Follows the existing `KANI_MAX_ROWS`/`KANI_MAX_COLS` pattern in this crate.
#[cfg(not(kani))]
pub const PAGE_SIZE: usize = 64 * 1024;
#[cfg(kani)]
pub const PAGE_SIZE: usize = 256;

/// A page of terminal data.
///
/// All data is stored contiguously with offset-based references.
/// Uses `UnsafeCell` for interior mutability to satisfy Stacked Borrows -
/// arena-allocated data may be accessed through multiple independent borrows.
///
/// **Crate-internal (#5573):** external code cannot construct `Page` values.
#[repr(C, align(4096))]
pub(crate) struct Page {
    /// Raw page data wrapped in UnsafeCell for interior mutability.
    /// This allows arena-style allocation where pointers derived from this data
    /// remain valid even when other parts of the PageStore are accessed.
    data: UnsafeCell<[u8; PAGE_SIZE]>,
}

// SAFETY: `Page` is `Send` because its only field (`UnsafeCell<[u8; PAGE_SIZE]>`)
// contains plain byte data with no thread-affine resources (no `Rc`, no thread-local
// refs, no raw OS handles). Ownership transfer between threads is safe because the
// `PageStore` allocator guarantees that at most one `PageSlice` references any byte
// range at a time — allocation returns disjoint slices and the free-list reclaims
// entire pages only after all slices are dropped.
unsafe impl Send for Page {}

// SAFETY: `Page` is `Sync` because shared `&Page` access is mediated exclusively
// through `PageSlice`, which enforces Rust's borrowing rules: `as_slice()` requires
// `&self` (shared) and `as_mut_slice()` requires `&mut self` (exclusive). The
// `UnsafeCell` interior mutability is only exercised during allocation
// (`PageStore::alloc_page` zeroes data under `&mut` borrow), never through shared
// references. No concurrent mutation of the same byte range is possible.
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
        // Use calloc for zero-initialized, page-aligned memory
        Box::new(Self {
            data: UnsafeCell::new([0; PAGE_SIZE]),
        })
    }

    /// Get a mutable pointer to the page data.
    ///
    /// # Safety
    /// The caller must ensure no other references to this data exist.
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

/// An offset into a page.
///
/// This is NOT a pointer - it's an index that remains valid
/// after the page is copied, serialized, or memory-mapped.
#[cfg(any(test, kani, feature = "testing"))]
#[derive(Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Offset<T> {
    /// Byte offset into the page.
    byte_offset: u32,
    /// Phantom data for type safety.
    _marker: PhantomData<T>,
}

// Manual Copy/Clone impl because derive requires T: Copy, but PhantomData<T> is always Copy
#[cfg(any(test, kani, feature = "testing"))]
impl<T> Copy for Offset<T> {}
#[cfg(any(test, kani, feature = "testing"))]
impl<T> Clone for Offset<T> {
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(any(test, kani, feature = "testing"))]
impl<T> Offset<T> {
    /// Create a new offset.
    ///
    /// # Safety
    ///
    /// The offset must be:
    /// - Less than PAGE_SIZE
    /// - Properly aligned for T
    #[must_use]
    pub const fn new(byte_offset: u32) -> Self {
        Self {
            byte_offset,
            _marker: PhantomData,
        }
    }

    /// Get the byte offset.
    #[cfg(any(test, kani, feature = "testing"))]
    #[must_use]
    pub const fn byte_offset(self) -> u32 {
        self.byte_offset
    }

    /// Resolve the offset to a reference.
    ///
    /// Returns `None` if the offset is out of bounds or misaligned.
    ///
    /// # Safety
    ///
    /// - The page must contain valid, initialized data at this offset
    /// - The type `T` at the offset must have been properly constructed
    #[cfg(any(test, kani))]
    #[must_use]
    pub(crate) unsafe fn get<'a>(&self, page: &'a Page) -> Option<&'a T> {
        let offset = self.byte_offset as usize;
        let align = std::mem::align_of::<T>();
        let size = std::mem::size_of::<T>();

        // Runtime bounds and alignment check
        if offset >= PAGE_SIZE || !offset.is_multiple_of(align) || offset + size > PAGE_SIZE {
            return None;
        }

        // SAFETY: Caller guarantees valid data at offset; we verified bounds/alignment.
        // Use data_ptr() to go through UnsafeCell for Stacked Borrows compliance.
        Some(unsafe { &*page.data_ptr().add(offset).cast::<T>() })
    }

    /// Resolve the offset to a mutable reference.
    ///
    /// Returns `None` if the offset is out of bounds or misaligned.
    ///
    /// # Safety
    ///
    /// Same as `get`, plus exclusive access to the memory at this offset.
    #[cfg(any(test, kani))]
    #[must_use]
    #[allow(
        clippy::mut_from_ref,
        reason = "unsafe test helper: caller ensures exclusive page access"
    )]
    pub(crate) unsafe fn get_mut<'a>(&self, page: &'a Page) -> Option<&'a mut T> {
        let offset = self.byte_offset as usize;
        let align = std::mem::align_of::<T>();
        let size = std::mem::size_of::<T>();

        if offset >= PAGE_SIZE || !offset.is_multiple_of(align) || offset + size > PAGE_SIZE {
            return None;
        }

        // SAFETY: Caller guarantees valid data at offset and exclusive access;
        // we verified bounds/alignment above. Uses data_ptr() for Stacked Borrows compliance.
        Some(unsafe { &mut *page.data_ptr().add(offset).cast::<T>() })
    }
}

/// Logical page identifier.
pub type PageId = usize;

/// A typed slice allocated within a page.
///
/// **Crate-internal (#5573).** Production consumers use [`Row`] as their safe
/// boundary. `PageSlice` is not accessible outside `aterm-grid`.
#[derive(Debug)]
pub struct PageSlice<T> {
    ptr: NonNull<T>,
    len: u16,
    page_id: PageId,
    #[cfg(any(test, kani, feature = "testing"))]
    offset: Offset<T>,
}

impl<T> PageSlice<T> {
    /// Length of the slice (u16).
    #[cfg(any(test, feature = "testing"))]
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
    #[cfg(any(test, kani, feature = "testing"))]
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

// SAFETY: `PageSlice<T>` is `Send` when `T: Send` because:
// 1. The `NonNull<T>` pointer targets memory owned by a `Page` (which is `Send`).
// 2. `PageSlice` logically owns the [T] range — `PageStore::alloc_slice` returns
//    a unique, non-overlapping slice per allocation. No aliasing across slices.
// 3. The remaining fields (`len: u16`, `page_id: PageId`) are trivially `Send`.
// Transferring a `PageSlice` to another thread transfers ownership of the [T] range,
// which is safe because the backing `Page` outlives all its slices (pages are freed
// only after all `PageSlice` references via `Offset` are dropped or the `PageStore`
// itself is dropped).
unsafe impl<T: Send> Send for PageSlice<T> {}

// SAFETY: `PageSlice<T>` is `Sync` when `T: Sync` because shared access (`&PageSlice<T>`)
// delegates to `as_slice() -> &[T]`, which returns an immutable reference. The
// `from_raw_parts` call in `as_slice()` produces a `&[T]` valid for the slice lifetime,
// and `&[T]` is `Sync` when `T: Sync`. Mutable access (`as_mut_slice`) requires
// `&mut PageSlice<T>`, which Rust's borrow checker guarantees is exclusive.
unsafe impl<T: Sync> Sync for PageSlice<T> {}

#[cfg(kani)]
impl<T> PageSlice<T> {
    /// Create a PageSlice from a raw mutable slice.
    ///
    /// # Safety
    ///
    /// This is safe because:
    /// - Kani verification runs in a single thread
    /// - The slice lifetime is tied to the static array
    /// - Used only for Kani proofs, not production code
    ///
    /// # Arguments
    ///
    /// * `slice` - Mutable reference to a slice of T
    ///
    /// # Returns
    ///
    /// A PageSlice pointing to the provided slice with page_id 0 and offset 0.
    #[must_use]
    pub(crate) fn from_raw(slice: &mut [T]) -> Self {
        let ptr = NonNull::new(slice.as_mut_ptr()).expect("slice pointer is null");
        let len = slice.len().min(u16::MAX as usize) as u16;
        Self {
            ptr,
            len,
            page_id: 0,
            #[cfg(any(test, kani, feature = "testing"))]
            offset: Offset {
                byte_offset: 0,
                _marker: PhantomData,
            },
        }
    }
}

#[path = "page_store.rs"]
mod page_store;
pub use page_store::PageStore;

// Tests are in a separate file for better organization
#[cfg(test)]
#[path = "page_tests/mod.rs"]
mod tests;

#[cfg(kani)]
#[path = "page_kani_proofs.rs"]
mod proofs;

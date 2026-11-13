// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared-memory region traits ([`RegionView`], [`RegionView::read`],
//! [`RegionView::load_u64_acquire`], [`RegionStore::write`], [`RegionStore::store_u64_release`])
//! and wrappers ([`CountdownStore`], [`Touched`], [`Shared`], [`Shared::swap_in`]).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::layout::PAGE;

/// Why an access was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// `[at, at + len)` is not inside the region.
    OutOfBounds {
        /// Where the access started.
        at: usize,
        /// How many bytes it covered.
        len: usize,
    },
    /// A word access at an offset that is not a multiple of 8.
    Misaligned {
        /// The offset.
        at: usize,
    },
    /// A write to a mapping this process may only read.
    ReadOnly,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds { at, len } => {
                write!(f, "{len} bytes at {at} run past the region")
            }
            Self::Misaligned { at } => write!(f, "a word access at {at} is not 8-byte aligned"),
            Self::ReadOnly => f.write_str("the region is mapped read-only"),
        }
    }
}

/// A region a reader can look at.
pub trait RegionView {
    /// The region's length in bytes.
    fn len(&self) -> usize;

    /// Whether the region has no bytes (never true of a mapped region).
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copy `out.len()` bytes at `at` into `out`.
    ///
    /// # Errors
    /// [`StoreError::OutOfBounds`] when the range is not inside the region.
    fn read(&self, at: usize, out: &mut [u8]) -> Result<(), StoreError>;

    /// Load the little-endian `u64` word at `at` (8-aligned) with acquire semantics.
    ///
    /// # Errors
    /// [`StoreError::Misaligned`] or [`StoreError::OutOfBounds`].
    fn load_u64_acquire(&self, at: usize) -> Result<u64, StoreError>;
}

/// A region a writer can change.
pub trait RegionStore: RegionView {
    /// Copy `bytes` to `at`, with no ordering of its own.
    ///
    /// # Errors
    /// [`StoreError::OutOfBounds`], or [`StoreError::ReadOnly`] for a reader's mapping.
    fn write(&self, at: usize, bytes: &[u8]) -> Result<(), StoreError>;

    /// Store the little-endian `u64` word at `at` (8-aligned) with release semantics.
    ///
    /// # Errors
    /// [`StoreError::Misaligned`], [`StoreError::OutOfBounds`] or [`StoreError::ReadOnly`].
    fn store_u64_release(&self, at: usize, value: u64) -> Result<(), StoreError>;
}

/// The range `[at, at + len)` if it is inside `0..region`.
const fn span(at: usize, len: usize, region: usize) -> Result<std::ops::Range<usize>, StoreError> {
    match at.checked_add(len) {
        Some(end) if end <= region => Ok(at..end),
        _ => Err(StoreError::OutOfBounds { at, len }),
    }
}

/// Check an aligned word access at `at`.
fn word_at(at: usize, region: usize) -> Result<(), StoreError> {
    if !at.is_multiple_of(8) {
        return Err(StoreError::Misaligned { at });
    }
    span(at, 8, region).map(|_| ())
}

/// A region in this process's heap.
#[derive(Debug)]
pub struct VecStore {
    bytes: Mutex<Vec<u8>>,
    len: usize,
}

impl VecStore {
    /// A zeroed region of `len` bytes.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self {
            bytes: Mutex::new(vec![0; len]),
            len,
        }
    }

    /// The bytes, whatever a panicking holder left (every access is a whole copy or a
    /// whole word, so there is no half-made state to refuse).
    fn lock_bytes(&self) -> MutexGuard<'_, Vec<u8>> {
        self.bytes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A copy of every byte, for a test that recovers from a snapshot while the writer goes on.
    #[must_use]
    pub fn snapshot(&self) -> Self {
        Self {
            bytes: Mutex::new(self.lock_bytes().clone()),
            len: self.len,
        }
    }
}

impl RegionView for VecStore {
    fn len(&self) -> usize {
        self.len
    }

    fn read(&self, at: usize, out: &mut [u8]) -> Result<(), StoreError> {
        let range = span(at, out.len(), self.len)?;
        let bytes = self.lock_bytes();
        let src = bytes
            .get(range)
            .ok_or(StoreError::OutOfBounds { at, len: out.len() })?;
        out.copy_from_slice(src);
        Ok(())
    }

    fn load_u64_acquire(&self, at: usize) -> Result<u64, StoreError> {
        word_at(at, self.len)?;
        let bytes = self.lock_bytes();
        let word = bytes
            .get(at..)
            .and_then(<[u8]>::first_chunk::<8>)
            .ok_or(StoreError::OutOfBounds { at, len: 8 })?;
        Ok(u64::from_le_bytes(*word))
    }
}

impl RegionStore for VecStore {
    fn write(&self, at: usize, src: &[u8]) -> Result<(), StoreError> {
        let range = span(at, src.len(), self.len)?;
        let mut bytes = self.lock_bytes();
        let dst = bytes
            .get_mut(range)
            .ok_or(StoreError::OutOfBounds { at, len: src.len() })?;
        dst.copy_from_slice(src);
        Ok(())
    }

    fn store_u64_release(&self, at: usize, value: u64) -> Result<(), StoreError> {
        word_at(at, self.len)?;
        let mut bytes = self.lock_bytes();
        let word = bytes
            .get_mut(at..)
            .and_then(<[u8]>::first_chunk_mut::<8>)
            .ok_or(StoreError::OutOfBounds { at, len: 8 })?;
        *word = value.to_le_bytes();
        Ok(())
    }
}

/// One region several threads write, each its own words.
#[derive(Debug)]
pub struct Shared<S> {
    inner: std::sync::Arc<Mutex<S>>,
    len: usize,
}

impl<S> Clone for Shared<S> {
    fn clone(&self) -> Self {
        Self {
            inner: std::sync::Arc::clone(&self.inner),
            len: self.len,
        }
    }
}

impl<S: RegionView> Shared<S> {
    /// Share `store`.
    #[must_use]
    pub fn new(store: S) -> Self {
        Self {
            len: store.len(),
            inner: std::sync::Arc::new(Mutex::new(store)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, S> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `f` over the inner store under the lock.
    pub fn with<R>(&self, f: impl FnOnce(&S) -> R) -> R {
        f(&self.lock())
    }

    /// Put `store` in place of the region every clone shares.
    ///
    /// # Errors
    /// Returns `store` untouched when its length differs.
    pub fn swap_in(&self, store: S, seed: impl FnOnce(&S)) -> Result<S, S> {
        if store.len() != self.len {
            return Err(store);
        }
        let mut guard = self.lock();
        seed(&store);
        Ok(std::mem::replace(&mut *guard, store))
    }
}

impl<S: RegionView> RegionView for Shared<S> {
    fn len(&self) -> usize {
        self.len
    }

    fn read(&self, at: usize, out: &mut [u8]) -> Result<(), StoreError> {
        self.lock().read(at, out)
    }

    fn load_u64_acquire(&self, at: usize) -> Result<u64, StoreError> {
        self.lock().load_u64_acquire(at)
    }
}

impl<S: RegionStore> RegionStore for Shared<S> {
    fn write(&self, at: usize, bytes: &[u8]) -> Result<(), StoreError> {
        self.lock().write(at, bytes)
    }

    fn store_u64_release(&self, at: usize, value: u64) -> Result<(), StoreError> {
        self.lock().store_u64_release(at, value)
    }
}

/// A writer's region that counts the pages it has written.
#[derive(Debug)]
pub struct Touched<S> {
    inner: S,
    /// One bit per page, set by the first store that reaches it.
    bits: Box<[AtomicU64]>,
    /// How many bits are set.
    pages: AtomicUsize,
    /// The process-wide sum of every live region's touched bytes.
    meter: Option<Arc<AtomicU64>>,
}

impl<S: RegionView> Touched<S> {
    /// Count the pages written to `inner` (none yet), adding each to `meter`.
    #[must_use]
    pub fn new(inner: S, meter: Option<Arc<AtomicU64>>) -> Self {
        let words = inner.len().div_ceil(PAGE).div_ceil(64);
        Self {
            inner,
            bits: (0..words).map(|_| AtomicU64::new(0)).collect(),
            pages: AtomicUsize::new(0),
            meter,
        }
    }

    /// The bytes of every page written so far.
    #[must_use]
    pub fn touched_bytes(&self) -> usize {
        self.pages.load(Ordering::Relaxed).saturating_mul(PAGE)
    }

    /// The bytes of written pages intersecting `[at, at + len)`, clipped to the region.
    #[must_use]
    pub fn touched_bytes_in(&self, at: usize, len: usize) -> usize {
        let region_len = self.inner.len();
        if len == 0 || at >= region_len {
            return 0;
        }
        let end = at.saturating_add(len).min(region_len);
        let first = at / PAGE;
        let last = (end - 1) / PAGE;
        let Some(words) = self.bits.get(first / 64..=last / 64) else {
            return 0;
        };
        let mut pages = 0usize;
        for (index, word) in words.iter().enumerate() {
            let low = if index == 0 { first % 64 } else { 0 };
            let high = if index + 1 == words.len() {
                last % 64
            } else {
                63
            };
            let mask = (u64::MAX << low) & (u64::MAX >> (63 - high));
            pages =
                pages.saturating_add((word.load(Ordering::Relaxed) & mask).count_ones() as usize);
        }
        pages.saturating_mul(PAGE)
    }

    /// Whether any byte of `[at, at + len)` lies on a page this region has written.
    #[must_use]
    pub fn touches(&self, at: usize, len: usize) -> bool {
        self.touched_bytes_in(at, len) != 0
    }

    /// The byte ranges of the pages a store has reached, in address order.
    pub fn written_pages(&self) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
        let len = self.inner.len();
        (0..len.div_ceil(PAGE))
            .filter(move |&page| {
                self.bits
                    .get(page / 64)
                    .is_some_and(|word| word.load(Ordering::Relaxed) & (1 << (page % 64)) != 0)
            })
            .map(move |page| page * PAGE..((page + 1) * PAGE).min(len))
    }

    /// The region whose pages this counts.
    #[must_use]
    pub const fn get_ref(&self) -> &S {
        &self.inner
    }

    /// Count the pages `[at, at + len)` spans (a store there just succeeded).
    fn touch(&self, at: usize, len: usize) {
        if len == 0 {
            return;
        }
        let last = at.saturating_add(len - 1) / PAGE;
        for page in at / PAGE..=last {
            let Some(word) = self.bits.get(page / 64) else {
                return;
            };
            let bit = 1u64 << (page % 64);
            if word.load(Ordering::Relaxed) & bit != 0 {
                continue;
            }
            if word.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
                self.pages.fetch_add(1, Ordering::Relaxed);
                if let Some(meter) = &self.meter {
                    meter.fetch_add(PAGE as u64, Ordering::Relaxed);
                }
            }
        }
    }
}

impl<S> Drop for Touched<S> {
    fn drop(&mut self) {
        if let Some(meter) = &self.meter {
            let bytes = (self.pages.load(Ordering::Relaxed) as u64).saturating_mul(PAGE as u64);
            meter.fetch_sub(bytes, Ordering::Relaxed);
        }
    }
}

impl<S: RegionView> RegionView for Touched<S> {
    fn len(&self) -> usize {
        self.inner.len()
    }

    fn read(&self, at: usize, out: &mut [u8]) -> Result<(), StoreError> {
        self.inner.read(at, out)
    }

    fn load_u64_acquire(&self, at: usize) -> Result<u64, StoreError> {
        self.inner.load_u64_acquire(at)
    }
}

impl<S: RegionStore> RegionStore for Touched<S> {
    fn write(&self, at: usize, bytes: &[u8]) -> Result<(), StoreError> {
        self.inner.write(at, bytes)?;
        self.touch(at, bytes.len());
        Ok(())
    }

    fn store_u64_release(&self, at: usize, value: u64) -> Result<(), StoreError> {
        self.inner.store_u64_release(at, value)?;
        self.touch(at, 8);
        Ok(())
    }
}

/// The window's region: the shared mapping itself (`aterm_uds::shm`), read-write in the window
/// that journals the session and read-only in the one that restores it.
#[cfg(unix)]
impl RegionView for aterm_uds::shm::Region {
    fn len(&self) -> usize {
        aterm_uds::shm::Region::len(self)
    }

    fn read(&self, at: usize, out: &mut [u8]) -> Result<(), StoreError> {
        self.read_into(at, out).map_err(StoreError::from)
    }

    fn load_u64_acquire(&self, at: usize) -> Result<u64, StoreError> {
        aterm_uds::shm::Region::load_u64_acquire(self, at).map_err(StoreError::from)
    }
}

#[cfg(unix)]
impl RegionStore for aterm_uds::shm::Region {
    fn write(&self, at: usize, bytes: &[u8]) -> Result<(), StoreError> {
        aterm_uds::shm::Region::write(self, at, bytes).map_err(StoreError::from)
    }

    fn store_u64_release(&self, at: usize, value: u64) -> Result<(), StoreError> {
        aterm_uds::shm::Region::store_u64_release(self, at, value).map_err(StoreError::from)
    }
}

#[cfg(unix)]
impl From<aterm_uds::shm::AccessError> for StoreError {
    fn from(e: aterm_uds::shm::AccessError) -> Self {
        use aterm_uds::shm::AccessError;
        match e {
            AccessError::OutOfBounds { at, len } => Self::OutOfBounds { at, len },
            AccessError::Misaligned { at } => Self::Misaligned { at },
            AccessError::ReadOnly => Self::ReadOnly,
        }
    }
}

/// A [`VecStore`] that executes only its first `budget` primitive stores.
#[cfg(any(test, debug_assertions))]
#[derive(Debug)]
pub struct CountdownStore {
    inner: VecStore,
    budget: Option<usize>,
    stores: AtomicUsize,
}

#[cfg(any(test, debug_assertions))]
impl CountdownStore {
    /// A zeroed region of `len` bytes that executes its first `budget` stores.
    #[must_use]
    pub fn new(len: usize, budget: Option<usize>) -> Self {
        Self {
            inner: VecStore::new(len),
            budget,
            stores: AtomicUsize::new(0),
        }
    }

    /// Primitive stores attempted so far, executed or dropped.
    #[must_use]
    pub fn stores(&self) -> usize {
        self.stores.load(Ordering::Relaxed)
    }

    /// Whether a store has been dropped.
    #[must_use]
    pub fn died(&self) -> bool {
        self.budget.is_some_and(|budget| self.stores() > budget)
    }

    /// The region as the dead writer left it.
    #[must_use]
    pub fn snapshot(&self) -> VecStore {
        self.inner.snapshot()
    }

    /// Count one store, and say whether it executes.
    fn admit(&self) -> bool {
        let n = self.stores.fetch_add(1, Ordering::Relaxed);
        self.budget.is_none_or(|budget| n < budget)
    }
}

#[cfg(any(test, debug_assertions))]
impl RegionView for CountdownStore {
    fn len(&self) -> usize {
        self.inner.len()
    }

    fn read(&self, at: usize, out: &mut [u8]) -> Result<(), StoreError> {
        self.inner.read(at, out)
    }

    fn load_u64_acquire(&self, at: usize) -> Result<u64, StoreError> {
        self.inner.load_u64_acquire(at)
    }
}

#[cfg(any(test, debug_assertions))]
impl RegionStore for CountdownStore {
    fn write(&self, at: usize, bytes: &[u8]) -> Result<(), StoreError> {
        if self.admit() {
            self.inner.write(at, bytes)
        } else {
            Ok(())
        }
    }

    fn store_u64_release(&self, at: usize, value: u64) -> Result<(), StoreError> {
        if self.admit() {
            self.inner.store_u64_release(at, value)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "store_touched_range_tests.rs"]
mod touched_range_tests;

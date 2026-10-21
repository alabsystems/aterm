// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a writer and a reader reach a region through.
//!
//! The region is shared memory written by several threads of one process (the parse
//! thread, the gather thread, the journal worker, the host's resize and config
//! paths), each owning its own words, and read only after its writer is dead. So
//! the store's methods take `&self`: the memory is shared, not owned, and each word
//! has one writer by protocol (the plan's §2.2 table), not by borrow.
//!
//! Two kinds of access, as in the shared mapping:
//!
//! - plain byte copies ([`RegionStore::write`], [`RegionView::read`]) for record
//!   bodies, ordered by nothing on their own;
//! - word stores with release semantics and loads with acquire semantics
//!   ([`RegionStore::store_u64_release`], [`RegionView::load_u64_acquire`]) for the
//!   header words. A commit word stored with release after a copy makes the copy
//!   visible to whoever loads the word with acquire.
//!
//! A process that dies loses no store it already made to its memory, so the only
//! hazard a reader faces is a writer stopped between two of these calls: what
//! [`CountdownStore`] reproduces, by executing the first `k` primitive stores and
//! dropping the rest.
//!
//! A reader needs only [`RegionView`]: the recovery never writes, and a read-only
//! mapping (which the kernel refuses to map writable) implements nothing more.
//!
//! A writer's region is wrapped twice in the window: in [`Touched`], which counts the
//! pages its stores have made resident (what the region costs, which a memory shed
//! weighs), and in [`Shared`], which lets the threads that write it share it and lets the
//! shed put a fresh region in its place ([`Shared::swap_in`]).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::layout::PAGE;

/// Why an access was refused. Every access is bounds-checked; none panics.
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
    ///
    /// [`StoreError::OutOfBounds`] when the range is not inside the region.
    fn read(&self, at: usize, out: &mut [u8]) -> Result<(), StoreError>;

    /// Load the little-endian `u64` word at `at` (8-aligned) with acquire semantics.
    ///
    /// # Errors
    ///
    /// [`StoreError::Misaligned`] or [`StoreError::OutOfBounds`].
    fn load_u64_acquire(&self, at: usize) -> Result<u64, StoreError>;
}

/// A region a writer can change. Each call is one primitive store, the granularity at
/// which a writer can die.
pub trait RegionStore: RegionView {
    /// Copy `bytes` to `at`, with no ordering of its own.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfBounds`], or [`StoreError::ReadOnly`] for a reader's mapping.
    fn write(&self, at: usize, bytes: &[u8]) -> Result<(), StoreError>;

    /// Store the little-endian `u64` word at `at` (8-aligned) with release semantics:
    /// every write and store this thread made before it is visible to a thread whose
    /// acquire load sees it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Misaligned`], [`StoreError::OutOfBounds`] or
    /// [`StoreError::ReadOnly`].
    fn store_u64_release(&self, at: usize, value: u64) -> Result<(), StoreError>;
}

/// The range `[at, at + len)` if it is inside `0..region`.
fn span(at: usize, len: usize, region: usize) -> Result<std::ops::Range<usize>, StoreError> {
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

/// A region in this process's heap: the shared mapping's semantics without a mapping.
///
/// The bytes sit behind one mutex, so a copy is one `memcpy` and every access is
/// ordered as the mapping's release stores and acquire loads are (the lock's release
/// and acquire are at least that strong). A mutex rather than atomic words because the
/// tail tap copies every PTY byte once: word-at-a-time atomic copies cost the parse
/// thread a quarter of its throughput in the `crash-restore` perf lane, a memcpy well
/// under the plan's 3% (writers of different words never wait long: the gather thread's
/// one store per read is the only other writer while output flows).
///
/// The tests, the benches and a window without a keeper use it; the window's shared
/// region implements the same traits over its mapping (`aterm_uds::shm::Region`, below),
/// whose copies are a raw `memcpy` too.
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

    /// A copy of every byte, for a test that recovers from a snapshot while the writer
    /// goes on.
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

/// One region several threads write, each its own words (the plan's §2.2 table: the
/// parse thread's tail and keyframes, the gather thread's `stream_gathered`, the host's
/// resize, config and cell-size words): the store behind one lock, taken for each
/// primitive store and released after it.
///
/// The window's mapping is `Send` and not `Sync` (`aterm_uds::shm`: a plain copy two
/// threads race is undefined behaviour, so `&Region` cannot be shared), and this is the
/// one way to share it, as [`VecStore`] shares its bytes: a lock around a `memcpy`
/// measured at no cost to the parse thread's throughput where word-at-a-time atomic
/// copies cost a quarter of it. Each call is still exactly one of the inner store's
/// primitives, so a writer that dies between two calls leaves what the protocols
/// promise, and a thread that dies holding the lock (a panic mid-copy) leaves a lock
/// the next caller takes over, as `VecStore`'s does: every access is a whole copy or a
/// whole word, so there is no half-made state to refuse.
///
/// Clones share the region; the last one drops it (for the mapping: unmaps it).
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

    /// Run `f` over the inner store under the lock (a test that snapshots a heap region
    /// while its writer goes on; the window's read of how many pages its region touched).
    pub fn with<R>(&self, f: impl FnOnce(&S) -> R) -> R {
        f(&self.lock())
    }

    /// Put `store` where the region every clone shares was, run `seed` on it in the same
    /// hold of the lock, and hand back the region it replaced: what the window's memory
    /// shed does to free a session's history ring (`aterm-gui`'s `crash_restore`), since
    /// shared pages a region has touched stay resident for as long as the region lives
    /// (`madvise` cannot give them back) and only a new region drops them.
    ///
    /// Every clone reaches the new region from its next primitive on, so a thread that
    /// writes its own word through a clone (the gather thread's count) needs no new handle.
    /// A primitive store lands wholly in one region or the other, never half in each. What
    /// `seed` stores is in the new region before any other thread's next store is: the
    /// gather thread's running count goes there in the hold of the swap, so a count stored
    /// by a read the swap overtook can only be a larger one, never an older value written
    /// over the newer.
    ///
    /// # Errors
    ///
    /// `store` back, untouched, when its length is not the region's: every clone's bounds
    /// checks were taken against that length.
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

/// A writer's region that counts the pages it has written: what the region costs the
/// window that maps it (the crash-restore plan's increment 6, `metrics
/// journal_resident_bytes=` and `status journal_bytes=`).
///
/// WHY PAGES WRITTEN, AND NOT A MEASURE OF RESIDENCY. A region is anonymous shared memory:
/// a page exists from the first store that reaches it until the region dies, and nothing
/// short of its death gives it back (`madvise` cannot release shared pages, the plan's
/// D3 M-R5). The writer never reads its region back (it keeps a mirror of every word it
/// owns), so the pages it has written are exactly the pages it made resident, and they
/// only grow. Counting them here, at the one funnel every store goes through, needs no
/// system call, holds for every section (the keyframe slots, the tail, the history ring,
/// increment 7's control slot) without knowing their protocols, and reads the same in a
/// test over a heap region as in the window over the mapping. The page is the layout's
/// ([`PAGE`], Apple silicon's 16 KiB): on a host whose page is smaller, a partly written
/// page is counted whole, an over-count of less than one page per touched run, never an
/// under-count.
///
/// The count is kept beside a process-wide `meter`, when one is given, which this region
/// adds each new page to and takes all of them back from when it is dropped (unmapped):
/// the window's total over every region it maps, which a control verb reads with no
/// session's lock.
///
/// Each new page costs one atomic `fetch_or` and two adds; a page already counted costs one
/// relaxed load, so the tail tap's every-slice copy (whose pages are counted after the
/// ring's first lap) pays a load per page it spans and nothing else.
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
    /// Count the pages written to `inner` (none yet), adding each to `meter` too.
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

    /// The bytes of every page written so far: what this region keeps resident.
    #[must_use]
    pub fn touched_bytes(&self) -> usize {
        self.pages.load(Ordering::Relaxed).saturating_mul(PAGE)
    }

    /// The bytes of written pages intersecting `[at, at + len)`, clipped to the
    /// region. A partly covered page counts whole. Clearing its bytes does not
    /// clear its bit: only dropping this region releases that page's accounting.
    ///
    /// Work is bounded by this region's allocated bitmap, even when the requested
    /// range overflows or extends beyond it. This reads the counter bitmap, never
    /// the underlying mapping or an operating-system residency query.
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
        if len == 0 {
            return false;
        }
        let last = at.saturating_add(len - 1) / PAGE;
        (at / PAGE..=last).any(|page| {
            self.bits
                .get(page / 64)
                .is_some_and(|word| word.load(Ordering::Relaxed) & (1 << (page % 64)) != 0)
        })
    }

    /// The byte ranges of the pages a store has reached, in address order, each clipped to
    /// the region: what an erase zeroes ([`crate::writer::erase_region`]) without making
    /// resident a page nothing wrote.
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
    pub fn get_ref(&self) -> &S {
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
///
/// The mapping's accessors are already this trait's, each bounds- and alignment-checked; this
/// only names their refusals in the store's words. The words are atomic `u64`s with the release
/// and acquire orderings the trait promises, and the copies are raw `memcpy`s that hand out no
/// reference into the mapping (every `unsafe` stays in aterm-uds, so this crate keeps
/// `forbid(unsafe_code)`). The mapping is `Send` but not `Sync`, because a plain copy two
/// threads race is undefined behaviour: a writer several threads share goes behind one lock, as
/// [`VecStore`]'s bytes do (atomic word copies measured at a quarter of the parse thread's
/// throughput; the lock and a `memcpy` at none). A read-only mapping refuses every
/// store as [`StoreError::ReadOnly`]: a restorer that tried one is a bug, never a write into the
/// dead window's words, and the kernel would refuse the page anyway.
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

/// A [`VecStore`] that executes only its first `budget` primitive stores (each
/// [`RegionStore::write`] and each [`RegionStore::store_u64_release`] is one) and
/// silently drops the rest: the writer carries on as if nothing happened, and the
/// region is left exactly as a writer killed after its `budget`-th store leaves it.
/// That is a death at instruction granularity, which the Tier-1 sweep runs at every
/// `k` of every bounded schedule.
///
/// Test and debug builds only: the shipped binary never carries it.
#[cfg(any(test, debug_assertions))]
#[derive(Debug)]
pub struct CountdownStore {
    inner: VecStore,
    budget: Option<usize>,
    stores: AtomicUsize,
}

#[cfg(any(test, debug_assertions))]
impl CountdownStore {
    /// A zeroed region of `len` bytes that executes its first `budget` stores
    /// (`None`: every store).
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

    /// Whether a store has been dropped: the writer is dead.
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
mod tests {
    use super::*;

    /// Byte copies at every alignment land exactly, leave their neighbours alone, and
    /// read back; words round-trip; every access outside the region is refused.
    #[test]
    fn the_heap_region_copies_and_bounds_like_the_mapping() {
        let store = VecStore::new(64);
        store.write(0, &[0xAA; 64]).expect("fill");
        for at in 0..16 {
            for len in 0..20 {
                let bytes: Vec<u8> = (0..len).map(|i| (at * 31 + i) as u8).collect();
                store.write(at, &bytes).expect("inside");
                let mut back = vec![0; len];
                store.read(at, &mut back).expect("inside");
                assert_eq!(back, bytes, "at {at} len {len}");
                let mut after = [0u8; 1];
                store.read(at + len, &mut after).expect("inside");
                assert_eq!(after[0], 0xAA, "the byte after {at}+{len} is untouched");
                store.write(at, &vec![0xAA; len]).expect("restore");
            }
        }
        store
            .store_u64_release(16, 0x0102_0304_0506_0708)
            .expect("aligned");
        assert_eq!(store.load_u64_acquire(16), Ok(0x0102_0304_0506_0708));
        let mut le = [0u8; 8];
        store.read(16, &mut le).expect("inside");
        assert_eq!(le, 0x0102_0304_0506_0708_u64.to_le_bytes(), "little-endian");
        assert_eq!(
            store.load_u64_acquire(12),
            Err(StoreError::Misaligned { at: 12 })
        );
        assert_eq!(
            store.load_u64_acquire(64),
            Err(StoreError::OutOfBounds { at: 64, len: 8 })
        );
        assert_eq!(
            store.write(60, &[0; 5]),
            Err(StoreError::OutOfBounds { at: 60, len: 5 })
        );
        assert_eq!(
            store.read(usize::MAX, &mut [0; 2]),
            Err(StoreError::OutOfBounds {
                at: usize::MAX,
                len: 2
            })
        );
    }

    /// A shared region takes a writer on one thread and another thread's word at the
    /// same time, as the window's parse and gather threads write one session's region:
    /// every store lands, and the session recovers from it as from an unshared one.
    #[test]
    fn a_shared_region_takes_two_threads_words() {
        use crate::layout::{Geometry, OFF_STREAM_GATHERED};
        use crate::recover::{RecoverPolicy, Recovery, recover};
        use crate::writer::{JournalWriter, KeyframeInput, WriterIdentity, store_gathered};
        use aterm_core::terminal::Terminal;

        let geometry = Geometry::v1();
        let shared = Shared::new(VecStore::new(geometry.region_len));
        let mut writer = JournalWriter::create(
            shared.clone(),
            geometry,
            &WriterIdentity {
                pid: 1,
                birth_us: 2,
                local_id: 3,
                build: 4,
                sid: "s-shared",
            },
        )
        .expect("created");
        let mut term = Terminal::new(6, 30);
        let checkpoint = term.checkpoint_journal(0).expect("ground");
        writer
            .write_keyframe(&KeyframeInput {
                checkpoint: &checkpoint,
                fence: term.history_fence(),
                resize_lifetime: term.resize_lifetime(),
                resize_ordinal: term.resize_ordinal(),
                bridge_lines: 0,
                config_fp: 9,
                integration_on: term.shell_integration_posture()
                    == aterm_core::terminal::ShellIntegrationPosture::On,
            })
            .expect("keyframe");
        let slices: Vec<Vec<u8>> = (0..200)
            .map(|i| format!("line {i}\r\n").into_bytes())
            .collect();
        let total: u64 = slices.iter().map(|s| s.len() as u64).sum();
        // The gather thread: its count, ahead of what the parse thread processed.
        let gather = {
            let store = shared.clone();
            let counts: Vec<u64> = slices
                .iter()
                .scan(0u64, |n, s| {
                    *n += s.len() as u64;
                    Some(*n)
                })
                .collect();
            std::thread::spawn(move || {
                for n in counts {
                    store_gathered(&store, n).expect("gathered");
                }
            })
        };
        for slice in &slices {
            term.process(slice);
            writer.append_slice(slice).expect("appended");
        }
        gather.join().expect("the gather thread");
        assert_eq!(shared.load_u64_acquire(OFF_STREAM_GATHERED), Ok(total));
        let dead = shared.with(VecStore::snapshot);
        let Recovery::Restored(restored) = recover(
            &dead,
            RecoverPolicy {
                tail: true,
                config_fp: 9,
                scratch_nonce: [0xA5; 32],
            },
            Terminal::new,
        ) else {
            panic!("the shared region restores a screen");
        };
        assert!(restored.exact, "{:?}", restored.losses);
        assert_eq!(restored.replayed_records, 200);
    }

    /// A touched region counts each page once, at its first store, whatever wrote it and
    /// however often; a refused store counts nothing; the meter carries every live region's
    /// pages and takes a region's back when it is dropped.
    #[test]
    fn a_touched_region_counts_each_page_once_and_gives_them_back_when_dropped() {
        let meter = Arc::new(AtomicU64::new(0));
        let region = Touched::new(VecStore::new(4 * PAGE), Some(Arc::clone(&meter)));
        assert_eq!(
            region.touched_bytes(),
            0,
            "nothing written, nothing resident"
        );
        region.store_u64_release(0, 1).expect("a header word");
        assert_eq!(region.touched_bytes(), PAGE);
        region.write(8, &[7; 64]).expect("the same page");
        region
            .store_u64_release(16, 2)
            .expect("the same page again");
        assert_eq!(region.touched_bytes(), PAGE, "a page counts once");
        region
            .write(PAGE - 4, &[9; 8])
            .expect("a copy across the first boundary");
        assert_eq!(region.touched_bytes(), 2 * PAGE, "both pages it spans");
        assert!(region.touches(PAGE, 1) && !region.touches(2 * PAGE, PAGE));
        assert_eq!(
            region.write(4 * PAGE - 4, &[1; 8]),
            Err(StoreError::OutOfBounds {
                at: 4 * PAGE - 4,
                len: 8
            })
        );
        assert_eq!(
            region.touched_bytes(),
            2 * PAGE,
            "a refused store touches nothing"
        );
        region
            .store_u64_release(3 * PAGE + 8, 3)
            .expect("the last page");
        assert_eq!(region.touched_bytes(), 3 * PAGE);
        assert!(
            !region.touches(2 * PAGE, PAGE),
            "the page between, never written"
        );
        assert_eq!(meter.load(Ordering::Relaxed), 3 * PAGE as u64);
        let other = Touched::new(VecStore::new(2 * PAGE), Some(Arc::clone(&meter)));
        other.write(PAGE, &[1]).expect("one page");
        assert_eq!(
            meter.load(Ordering::Relaxed),
            4 * PAGE as u64,
            "every live region"
        );
        drop(region);
        assert_eq!(
            meter.load(Ordering::Relaxed),
            PAGE as u64,
            "a dropped region's pages leave the meter"
        );
        drop(other);
        assert_eq!(meter.load(Ordering::Relaxed), 0);
    }

    /// A replaced region takes every clone's next store, with what the seed stored already
    /// in it; the region it replaced comes back as it was; a region of another length is
    /// refused and the shared one left in place.
    #[test]
    fn a_replaced_region_takes_every_clone_s_next_store() {
        let shared = Shared::new(VecStore::new(64));
        let other_thread = shared.clone();
        shared
            .store_u64_release(0, 1)
            .expect("into the first region");
        let old = shared
            .swap_in(VecStore::new(64), |new| {
                new.store_u64_release(8, 2).expect("the seed");
            })
            .expect("same length");
        other_thread
            .store_u64_release(16, 3)
            .expect("a clone's next store");
        assert_eq!(shared.load_u64_acquire(0), Ok(0), "a new region");
        assert_eq!(shared.load_u64_acquire(8), Ok(2), "the seed's word");
        assert_eq!(other_thread.load_u64_acquire(16), Ok(3));
        assert_eq!(old.load_u64_acquire(0), Ok(1), "the old region as it was");
        assert_eq!(
            old.load_u64_acquire(16),
            Ok(0),
            "and nothing after the swap"
        );
        let refused = shared
            .swap_in(VecStore::new(128), |_| {
                panic!("no seed for a refused region")
            })
            .expect_err("another length");
        assert_eq!(refused.len(), 128);
        assert_eq!(
            shared.load_u64_acquire(8),
            Ok(2),
            "still the region in place"
        );
    }

    /// The countdown executes exactly its budget and drops the rest silently.
    #[test]
    fn the_countdown_executes_its_budget_and_drops_the_rest() {
        let store = CountdownStore::new(64, Some(2));
        store.store_u64_release(0, 1).expect("one");
        store.write(8, &[2; 8]).expect("two");
        assert!(!store.died());
        store.store_u64_release(16, 3).expect("dropped, silently");
        store.write(24, &[4; 8]).expect("dropped, silently");
        assert!(store.died());
        assert_eq!(store.stores(), 4);
        let dead = store.snapshot();
        assert_eq!(dead.load_u64_acquire(0), Ok(1));
        assert_eq!(dead.load_u64_acquire(8), Ok(u64::from_le_bytes([2; 8])));
        assert_eq!(dead.load_u64_acquire(16), Ok(0), "never stored");
        assert_eq!(dead.load_u64_acquire(24), Ok(0), "never copied");
    }
}

#[cfg(test)]
mod touched_range_tests {
    use super::{PAGE, RegionStore, Touched, VecStore};

    #[test]
    fn written_page_ranges_survive_zeroing_and_clip_untrusted_bounds() {
        let store = Touched::new(VecStore::new(130 * PAGE + 1), None);
        assert_eq!(store.touched_bytes_in(0, usize::MAX), 0);
        for page in [0, 63, 64, 129, 130] {
            store
                .write(page * PAGE, &[9])
                .expect("one byte in the page");
        }
        assert_eq!(store.touched_bytes(), 5 * PAGE);
        assert_eq!(store.touched_bytes_in(0, usize::MAX), 5 * PAGE);
        assert_eq!(store.touched_bytes_in(63 * PAGE, 2 * PAGE), 2 * PAGE);
        assert_eq!(store.touched_bytes_in(64 * PAGE - 1, 2), 2 * PAGE);
        assert_eq!(store.touched_bytes_in(PAGE, 62 * PAGE), 0);
        assert_eq!(store.touched_bytes_in(130 * PAGE, usize::MAX), PAGE);
        assert_eq!(store.touched_bytes_in(0, 0), 0);
        assert_eq!(store.touched_bytes_in(usize::MAX, usize::MAX), 0);
        for page in [0, 63, 64, 129, 130] {
            store.write(page * PAGE, &[0]).expect("zero the old byte");
        }
        assert_eq!(store.touched_bytes_in(0, usize::MAX), 5 * PAGE);
    }
}

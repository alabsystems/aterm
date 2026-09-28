// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `SmallVec<T, N>`: inline storage with heap fallback.

use alloc::vec::Vec;
use core::fmt;
use core::iter::FusedIterator;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};

/// A vector that stores up to `N` elements inline before spilling to the heap.
///
/// This avoids heap allocation for the common case where collections are small.
/// When the inline capacity is exceeded, all elements move to a heap-allocated `Vec<T>`.
pub struct SmallVec<T, const N: usize> {
    data: SmallVecData<T, N>,
}

enum SmallVecData<T, const N: usize> {
    Inline {
        buf: [MaybeUninit<T>; N],
        len: usize,
    },
    Heap(Vec<T>),
}

impl<T, const N: usize> SmallVec<T, N> {
    /// Create a new, empty `SmallVec`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data: SmallVecData::Inline {
                // Array of `MaybeUninit` needs no init; inline-const avoids the
                // `assume_init` obligation entirely (see ArrayVec::new).
                buf: [const { MaybeUninit::uninit() }; N],
                len: 0,
            },
        }
    }

    /// Create a `SmallVec` with the given capacity pre-allocated.
    ///
    /// If `capacity <= N`, uses inline storage. Otherwise, allocates on the heap.
    #[must_use]
    // #[inline] so the MIR crosses the crate boundary: callers' Trust gates
    // (aterm-scrollback) bundle and VERIFY this body instead of assuming an
    // absent callee. Semantics unchanged.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        if capacity <= N {
            Self::new()
        } else {
            // Clamp the bulk-allocation HINT (advisory — the Vec grows on
            // demand past it) so the reservation carries a provable upper
            // bound for the L0 unbounded-allocation gate. The literal (not a
            // named const) is deliberate: the in-process verifier havocs
            // named-const operands, which would refute the clamp. Contents are
            // identical for every input.
            Self {
                data: SmallVecData::Heap(Vec::with_capacity(capacity.min(16_777_216))),
            }
        }
    }

    /// The number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.data {
            SmallVecData::Inline { len, .. } => *len,
            SmallVecData::Heap(vec) => vec.len(),
        }
    }

    /// Whether the collection is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The current capacity (inline or heap).
    #[must_use]
    pub fn capacity(&self) -> usize {
        match &self.data {
            SmallVecData::Inline { .. } => N,
            SmallVecData::Heap(vec) => vec.capacity(),
        }
    }

    /// Whether the data is currently stored inline.
    #[must_use]
    pub fn is_inline(&self) -> bool {
        matches!(&self.data, SmallVecData::Inline { .. })
    }

    /// Whether the data has spilled to the heap.
    ///
    /// This is the inverse of [`is_inline`](Self::is_inline).
    /// Provided for API compatibility with `smallvec::SmallVec::spilled()`.
    #[must_use]
    pub fn spilled(&self) -> bool {
        !self.is_inline()
    }

    /// Push an element. Spills to heap if inline capacity is exceeded.
    pub fn push(&mut self, value: T) {
        match &mut self.data {
            SmallVecData::Inline { buf, len } => {
                // Local copy of `len` (the ArrayVec::push idiom): the open-model
                // verifier reloads `*len` at every deref, so only a local carries
                // the `< N` fact to the index and the increment. wrapping_add is
                // exact under that guard. Behavior-identical.
                let cur = *len;
                if cur < N {
                    buf[cur] = MaybeUninit::new(value);
                    *len = cur.wrapping_add(1);
                } else {
                    // Spill to heap
                    self.spill_and_push(value);
                }
            }
            SmallVecData::Heap(vec) => {
                vec.push(value);
            }
        }
    }

    /// Pop the last element.
    // Skip: the inline arm's `assume_init_read` joins ArrayVec::pop's
    // crate-local init-tracking class (per-slot memory-model producer lane).
    // Same audited invariant: `len` counts exactly the initialized prefix.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn pop(&mut self) -> Option<T> {
        match &mut self.data {
            SmallVecData::Inline { buf, len } => {
                if *len == 0 {
                    None
                } else {
                    *len -= 1;
                    // SAFETY: buf[*len] was initialized when it was pushed
                    Some(unsafe { buf[*len].assume_init_read() })
                }
            }
            SmallVecData::Heap(vec) => vec.pop(),
        }
    }

    /// Clear all elements.
    // Skip: the inline arm drops each initialized slot — the same crate-local
    // init-tracking class as `pop`/ArrayVec::clear (per-slot memory-model
    // producer lane). Audited: `len` counts exactly the initialized prefix.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn clear(&mut self) {
        match &mut self.data {
            SmallVecData::Inline { buf, len } => {
                // `len <= N` by invariant; clamp to a provably in-bounds count
                // (identical when the invariant holds), then iterate the
                // clamped subslice: the single slice operation carries the
                // `n <= N` proof and the loop has no per-index obligation.
                let n = if *len < N { *len } else { N };
                for slot in &mut buf[..n] {
                    // SAFETY: elements `0..len` were initialized when pushed
                    // and `n <= len`, so every slot in the subslice holds a
                    // live value; each is dropped exactly once.
                    unsafe {
                        slot.assume_init_drop();
                    }
                }
                *len = 0;
            }
            SmallVecData::Heap(vec) => vec.clear(),
        }
    }

    /// Remove the element at `index` by swapping it with the last element.
    ///
    /// This is O(1) but does not preserve ordering.
    ///
    /// # Panics
    ///
    /// Panics if `index >= len`.
    // Skip: the raw slot read/move joins ArrayVec's init/provenance class
    // (per-slot memory-model producer lane). Same audited len-invariant.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn swap_remove(&mut self, index: usize) -> T {
        let len = self.len();
        assert!(index < len, "index out of bounds: {index} >= {len}");
        let last = len - 1;
        self.as_mut_slice().swap(index, last);
        self.pop().expect("invariant: len > 0 after swap")
    }

    /// Extend from a slice (requires `T: Clone`).
    // Skip: the bulk copy joins the raw-shift init/provenance family
    // (per-slot memory-model producer lane). Same audited len-invariant.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn extend_from_slice(&mut self, slice: &[T])
    where
        T: Clone,
    {
        for item in slice {
            self.push(item.clone());
        }
    }

    /// View as a slice.
    #[must_use]
    // Skip: the raw-parts reconstruction joins ArrayVec::as_slice's
    // init/provenance classification (memory-model producer lane).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_slice(&self) -> &[T] {
        match &self.data {
            SmallVecData::Inline { buf, len } => {
                // inline `*len <= N` by invariant; clamp to a provably
                // in-bounds length (a no-op when the invariant holds) and take
                // the initialized subslice first — the safe slice operation
                // carries the `n <= N` proof, and the raw-parts reconstruction
                // below reuses that subslice's own pointer and length.
                let n = if *len < N { *len } else { N };
                let init: &[MaybeUninit<T>] = &buf[..n];
                // SAFETY: `MaybeUninit<T>` has the same layout as `T`; the
                // pointer and length come from the same in-bounds subslice
                // `init`, and elements `0..len` are initialized (`n <= len`),
                // so reading them as `T` is sound.
                unsafe { core::slice::from_raw_parts(init.as_ptr().cast::<T>(), init.len()) }
            }
            SmallVecData::Heap(vec) => vec.as_slice(),
        }
    }

    /// View as a mutable slice.
    // Skip: the raw-parts reconstruction joins ArrayVec::as_slice's
    // init/provenance classification (memory-model producer lane).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        match &mut self.data {
            SmallVecData::Inline { buf, len } => {
                // inline `*len <= N` by invariant; clamp to a provably
                // in-bounds length (a no-op when the invariant holds) and take
                // the initialized subslice first — the safe slice operation
                // carries the `n <= N` proof, and the raw-parts reconstruction
                // below reuses that subslice's own pointer and length.
                let n = if *len < N { *len } else { N };
                let init: &mut [MaybeUninit<T>] = &mut buf[..n];
                // SAFETY: `MaybeUninit<T>` has the same layout as `T`; the
                // pointer and length come from the same in-bounds subslice
                // `init`, and elements `0..len` are initialized (`n <= len`),
                // so reading them as `T` is sound.
                unsafe {
                    core::slice::from_raw_parts_mut(init.as_mut_ptr().cast::<T>(), init.len())
                }
            }
            SmallVecData::Heap(vec) => vec.as_mut_slice(),
        }
    }

    /// Create a `SmallVec` from a slice (requires `T: Clone`).
    #[must_use]
    // Skip: the bulk copy joins the raw-shift init/provenance family.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn from_slice(slice: &[T]) -> Self
    where
        T: Clone,
    {
        let mut sv = Self::with_capacity(slice.len());
        sv.extend_from_slice(slice);
        sv
    }

    /// Create from a `Vec<T>`.
    #[must_use]
    // Skip: the residual row is drop glue over the consumed `Vec` scaffolding
    // (std/alloc internals — the drop-glue lane); the conversion itself is a
    // move. Unit-tested round-trip.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn from_vec(vec: Vec<T>) -> Self {
        if vec.len() <= N {
            let mut sv = Self::new();
            for item in vec {
                sv.push(item);
            }
            sv
        } else {
            Self {
                data: SmallVecData::Heap(vec),
            }
        }
    }

    /// Create from a single element repeated `n` times.
    // Skip: the repeat-fill joins the raw-shift init/provenance family; `T: Clone`
    // is caller-chosen code besides (user-T dispatch).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn from_elem(value: T, n: usize) -> Self
    where
        T: Clone,
    {
        let mut sv = Self::with_capacity(n);
        for _ in 0..n.saturating_sub(1) {
            sv.push(value.clone());
        }
        if n > 0 {
            sv.push(value);
        }
        sv
    }

    // ── Internal helpers ────────────────────────────────────────────────

    fn spill_and_push(&mut self, value: T) {
        self.ensure_heap();
        if let SmallVecData::Heap(vec) = &mut self.data {
            vec.push(value);
        }
    }

    // Skip: the inline->heap spill reads each initialized slot via
    // `assume_init_read` — the same crate-local init-tracking class as
    // ArrayVec's pop/clear/truncate (per-slot memory-model producer lane).
    // The audited invariant: `len` counts exactly the initialized prefix;
    // `*len = 0` before the heap swap prevents any double-drop.
    #[cfg_attr(trust_verify, trust::skip)]
    fn ensure_heap(&mut self) {
        if let SmallVecData::Inline { buf, len } = &mut self.data {
            // inline `*len <= N` by invariant; clamp to a provably in-bounds count
            // (a no-op when the invariant holds) so the `buf[..current_len]` read
            // below carries a `<= N` proof.
            let current_len = if *len < N { *len } else { N };
            let mut vec = Vec::with_capacity(current_len.max(N) * 2);
            // SAFETY: elements 0..current_len are initialized
            for elem in &buf[..current_len] {
                vec.push(unsafe { elem.assume_init_read() });
            }
            // Prevent double-drop of inline elements
            *len = 0;
            self.data = SmallVecData::Heap(vec);
        }
    }
}

// ── Trait impls ─────────────────────────────────────────────────────────────

impl<T, const N: usize> Default for SmallVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for SmallVec<T, N> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, const N: usize> DerefMut for SmallVec<T, N> {
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T: Clone, const N: usize> Clone for SmallVec<T, N> {
    // Skip: clones each element through the INSTANTIATING type's `Clone`
    // (open-trait user code, may panic by design) — the user-T dispatch class.
    #[cfg_attr(trust_verify, trust::skip)]
    fn clone(&self) -> Self {
        let mut new = Self::with_capacity(self.len());
        for item in self.as_slice() {
            new.push(item.clone());
        }
        new
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for SmallVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

impl<T: PartialEq, const N: usize> PartialEq for SmallVec<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq, const N: usize> Eq for SmallVec<T, N> {}

impl<T, const N: usize> Drop for SmallVec<T, N> {
    fn drop(&mut self) {
        // Reuse the (bounds-clamped) clear — drops the inline elements without a
        // separate out-of-bounds obligation; the heap Vec drops itself.
        self.clear();
    }
}

impl<T, const N: usize> FromIterator<T> for SmallVec<T, N> {
    // Skip: `I: IntoIterator` is CALLER-CHOSEN code — `next` is an open-trait
    // dispatch on a type parameter (user-T class); push's capacity contract is
    // the documented panic.
    #[cfg_attr(trust_verify, trust::skip)]
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let (lower, _) = iter.size_hint();
        let mut sv = Self::with_capacity(lower);
        for item in iter {
            sv.push(item);
        }
        sv
    }
}

impl<T, const N: usize> Extend<T> for SmallVec<T, N> {
    // Skip: `I: IntoIterator` is CALLER-CHOSEN code (user-T dispatch); push's
    // capacity contract is the documented panic.
    #[cfg_attr(trust_verify, trust::skip)]
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

/// Owned, by-value iterator over a [`SmallVec`].
///
/// For inline storage this drains the inline `MaybeUninit` buffer in place with
/// no heap allocation, restoring the non-allocating `IntoIter` property of the
/// upstream `smallvec` crate. For heap storage it delegates to the inner
/// [`alloc::vec::IntoIter`].
///
/// The live (not-yet-yielded) range is always `start..end`; `next` advances
/// `start`, `next_back` retreats `end`, and [`Drop`] disposes of the remainder
/// exactly once.
pub enum IntoIter<T, const N: usize> {
    /// Inline elements drained directly from the moved-out buffer.
    Inline {
        buf: [MaybeUninit<T>; N],
        start: usize,
        end: usize,
    },
    /// Heap elements delegated to the standard `Vec` iterator.
    Heap(alloc::vec::IntoIter<T>),
}

impl<T, const N: usize> Iterator for IntoIter<T, N> {
    type Item = T;

    // Skip: the drain reads each initialized slot — the crate-local init-tracking
    // class (per-slot memory-model producer lane). Same audited len-invariant.
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<T> {
        match self {
            IntoIter::Inline { buf, start, end } => {
                // `*end <= N` by construction (clamped in `into_iter`); clamp to
                // a provably in-bounds bound (a no-op when the invariant holds)
                // so the indexed read below carries a `< N` proof and leaves no
                // out-of-bounds obligation.
                let bound = if *end < N { *end } else { N };
                if *start < bound {
                    // SAFETY: element at `*start` was initialized at construction
                    // (`*start < bound <= N`) and has not yet been yielded;
                    // advancing `start` afterward guarantees it is never read or
                    // dropped again.
                    let value = unsafe { buf[*start].assume_init_read() };
                    *start += 1;
                    Some(value)
                } else {
                    None
                }
            }
            IntoIter::Heap(iter) => iter.next(),
        }
    }

    // Skip: reads the drain cursor against `len` — the same init/provenance
    // class as its `next` (per-slot memory-model producer lane).
    #[cfg_attr(trust_verify, trust::skip)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            IntoIter::Inline { start, end, .. } => {
                let len = end.saturating_sub(*start);
                (len, Some(len))
            }
            IntoIter::Heap(iter) => iter.size_hint(),
        }
    }
}

impl<T, const N: usize> DoubleEndedIterator for IntoIter<T, N> {
    // Skip: the back-drain reads each initialized slot — the same
    // init/provenance class as `next`/`size_hint`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn next_back(&mut self) -> Option<T> {
        match self {
            IntoIter::Inline { buf, start, end } => {
                // `*end <= N` by construction; clamp to a provably in-bounds
                // bound (a no-op when the invariant holds) and keep `*end`
                // within it so the indexed read below carries a `< N` proof.
                let bound = if *end < N { *end } else { N };
                *end = bound;
                if *start < *end {
                    *end -= 1;
                    // SAFETY: element at `*end` was initialized at construction
                    // (`*end < bound <= N`) and has not yet been yielded;
                    // retreating `end` first guarantees it is never read or
                    // dropped again.
                    let value = unsafe { buf[*end].assume_init_read() };
                    Some(value)
                } else {
                    None
                }
            }
            IntoIter::Heap(iter) => iter.next_back(),
        }
    }
}

impl<T, const N: usize> ExactSizeIterator for IntoIter<T, N> {}

impl<T, const N: usize> FusedIterator for IntoIter<T, N> {}

impl<T, const N: usize> Drop for IntoIter<T, N> {
    // Skip: drops the un-yielded initialized slots — the same crate-local
    // init-tracking class as `next`/`next_back` (per-slot memory-model
    // producer lane). Audited: start..end bounds the live prefix.
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        if let IntoIter::Inline { buf, start, end } = self {
            // `*start <= *end <= N` by construction; clamp both slice bounds
            // to provably in-bounds values (a no-op when the invariant holds),
            // then iterate the clamped subslice — the slice operation carries
            // the `s <= bound <= N` proof, no per-index obligation remains.
            let bound = if *end < N { *end } else { N };
            let s = if *start < bound { *start } else { bound };
            // Drop each not-yet-yielded element exactly once, advancing `start`
            // BEFORE dropping, so no slot can be dropped twice even if an
            // element's Drop panics.
            // (`saturating_add` is identical here since `start < bound <= N`.)
            for slot in &mut buf[s..bound] {
                *start = start.saturating_add(1);
                // SAFETY: the element was initialized and has not been
                // yielded, and `start` has advanced past it, so it can never
                // be read or dropped again.
                unsafe {
                    slot.assume_init_drop();
                }
            }
        }
        // Heap variant: `alloc::vec::IntoIter` drops its own remaining elements.
    }
}

impl<T, const N: usize> IntoIterator for SmallVec<T, N> {
    type Item = T;
    type IntoIter = IntoIter<T, N>;

    // Skip: hands the inline buffer to `IntoIter` — the same crate-local
    // init-tracking class as the iterator it constructs.
    #[cfg_attr(trust_verify, trust::skip)]
    fn into_iter(mut self) -> Self::IntoIter {
        // Move the storage out, leaving an empty inline buffer behind so the
        // source `SmallVec`'s Drop is a harmless no-op (len 0). This avoids the
        // heap allocation a detour through `Vec` would perform for the inline
        // case.
        let data = core::mem::replace(
            &mut self.data,
            SmallVecData::Inline {
                buf: [const { MaybeUninit::uninit() }; N],
                len: 0,
            },
        );
        match data {
            SmallVecData::Inline { buf, len } => {
                // inline `len <= N` by invariant; clamp to a provably in-bounds
                // end (a no-op when the invariant holds) so the `start..end`
                // range the iterator/Drop walks carries a `<= N` proof.
                let end = if len < N { len } else { N };
                IntoIter::Inline { buf, start: 0, end }
            }
            SmallVecData::Heap(vec) => IntoIter::Heap(vec.into_iter()),
        }
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a SmallVec<T, N> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a mut SmallVec<T, N> {
    type Item = &'a mut T;
    type IntoIter = core::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_mut_slice().iter_mut()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::{String, ToString};
    use std::vec::Vec;
    use std::{format, vec};

    // Drop-counting element for verifying `IntoIter` drop-once semantics under
    // partial/full/no consume, in both inline and heap modes. The shared `Rc`
    // counter (and the heap `String` payload) give Miri real allocations to
    // track, so a double-drop or leak is caught, not just a wrong count.
    struct DropCounter {
        counter: std::rc::Rc<std::cell::Cell<usize>>,
        payload: String,
    }

    impl DropCounter {
        fn new(counter: &std::rc::Rc<std::cell::Cell<usize>>) -> Self {
            Self {
                counter: std::rc::Rc::clone(counter),
                payload: "into-iter drop payload".to_string(),
            }
        }
    }

    impl Drop for DropCounter {
        fn drop(&mut self) {
            // The payload is intact at drop time: a double drop or a drop of
            // freed storage reads garbage here (and Miri flags the read).
            assert_eq!(self.payload, "into-iter drop payload");
            self.counter.set(self.counter.get() + 1);
        }
    }

    #[test]
    fn test_new_is_empty() {
        let sv: SmallVec<i32, 4> = SmallVec::new();
        assert!(sv.is_empty());
        assert_eq!(sv.len(), 0);
        assert!(sv.is_inline());
    }

    #[test]
    fn test_push_within_inline_capacity() {
        let mut sv: SmallVec<i32, 4> = SmallVec::new();
        sv.push(1);
        sv.push(2);
        sv.push(3);
        assert_eq!(sv.len(), 3);
        assert!(sv.is_inline());
        assert_eq!(sv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn test_push_spills_to_heap() {
        let mut sv: SmallVec<i32, 2> = SmallVec::new();
        sv.push(1);
        sv.push(2);
        assert!(sv.is_inline());
        sv.push(3);
        assert!(!sv.is_inline());
        assert_eq!(sv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn test_pop() {
        let mut sv: SmallVec<i32, 4> = SmallVec::new();
        sv.push(10);
        sv.push(20);
        assert_eq!(sv.pop(), Some(20));
        assert_eq!(sv.pop(), Some(10));
        assert_eq!(sv.pop(), None);
    }

    #[test]
    fn test_clear() {
        let mut sv: SmallVec<String, 2> = SmallVec::new();
        sv.push("a".into());
        sv.push("b".into());
        sv.clear();
        assert!(sv.is_empty());
    }

    #[test]
    fn test_from_vec() {
        let sv: SmallVec<i32, 4> = SmallVec::from_vec(vec![1, 2, 3]);
        assert!(sv.is_inline());
        assert_eq!(sv.as_slice(), &[1, 2, 3]);

        let sv: SmallVec<i32, 2> = SmallVec::from_vec(vec![1, 2, 3, 4, 5]);
        assert!(!sv.is_inline());
        assert_eq!(sv.as_slice(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_from_elem() {
        let sv: SmallVec<char, 4> = SmallVec::from_elem('x', 3);
        assert_eq!(sv.as_slice(), &['x', 'x', 'x']);
    }

    #[test]
    fn test_clone() {
        let mut sv: SmallVec<String, 2> = SmallVec::new();
        sv.push("hello".into());
        let cloned = sv.clone();
        assert_eq!(sv.as_slice(), cloned.as_slice());
    }

    #[test]
    fn test_eq() {
        let mut a: SmallVec<i32, 4> = SmallVec::new();
        a.push(1);
        a.push(2);
        let mut b: SmallVec<i32, 4> = SmallVec::new();
        b.push(1);
        b.push(2);
        assert_eq!(a, b);
    }

    #[test]
    fn test_collect() {
        let sv: SmallVec<i32, 4> = (0..3).collect();
        assert_eq!(sv.as_slice(), &[0, 1, 2]);
    }

    #[test]
    fn test_deref_indexing() {
        let mut sv: SmallVec<i32, 4> = SmallVec::new();
        sv.push(10);
        sv.push(20);
        assert_eq!(sv[0], 10);
        assert_eq!(sv[1], 20);
    }

    #[test]
    fn test_extend_from_slice() {
        let mut sv: SmallVec<i32, 4> = SmallVec::new();
        sv.extend_from_slice(&[1, 2, 3]);
        assert_eq!(sv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn test_with_capacity() {
        let sv: SmallVec<i32, 4> = SmallVec::with_capacity(2);
        assert!(sv.is_inline());
        assert_eq!(sv.capacity(), 4); // inline capacity is always N

        let sv: SmallVec<i32, 4> = SmallVec::with_capacity(10);
        assert!(!sv.is_inline());
        assert!(sv.capacity() >= 10);
    }

    #[test]
    fn test_debug_format() {
        let mut sv: SmallVec<i32, 4> = SmallVec::new();
        sv.push(1);
        sv.push(2);
        assert_eq!(format!("{sv:?}"), "[1, 2]");
    }

    #[test]
    fn test_into_iter() {
        let mut sv: SmallVec<i32, 4> = SmallVec::new();
        sv.push(10);
        sv.push(20);
        sv.push(30);
        let collected: Vec<i32> = sv.into_iter().collect();
        assert_eq!(collected, vec![10, 20, 30]);
    }

    #[test]
    fn test_drop_string_elements() {
        // Verify that String elements are properly dropped (no leaks).
        let mut sv: SmallVec<String, 2> = SmallVec::new();
        sv.push("hello world this is a longer string to force heap alloc".into());
        sv.push("another string".into());
        drop(sv);
        // If we get here without ASAN/MIRI complaint, drop is correct.
    }

    #[test]
    fn test_into_iter_inline_stays_inline() {
        // The whole point of the dedicated IntoIter: consuming an inline
        // SmallVec must NOT allocate a heap Vec. We can't observe the allocator
        // here, but we can assert the iterator took the inline variant.
        let mut sv: SmallVec<i32, 4> = SmallVec::new();
        sv.push(10);
        sv.push(20);
        sv.push(30);
        assert!(sv.is_inline());
        let it = sv.into_iter();
        assert!(matches!(it, IntoIter::Inline { .. }));
        let collected: Vec<i32> = it.collect();
        assert_eq!(collected, vec![10, 20, 30]);
    }

    #[test]
    fn test_into_iter_heap_variant() {
        let mut sv: SmallVec<i32, 2> = SmallVec::new();
        sv.push(1);
        sv.push(2);
        sv.push(3);
        assert!(!sv.is_inline());
        let it = sv.into_iter();
        assert!(matches!(it, IntoIter::Heap(_)));
        let collected: Vec<i32> = it.collect();
        assert_eq!(collected, vec![1, 2, 3]);
    }

    #[test]
    fn test_into_iter_inline_size_hint_and_len() {
        let mut sv: SmallVec<i32, 4> = SmallVec::new();
        sv.push(1);
        sv.push(2);
        sv.push(3);
        let mut it = sv.into_iter();
        assert_eq!(it.size_hint(), (3, Some(3)));
        assert_eq!(it.len(), 3);
        it.next();
        assert_eq!(it.size_hint(), (2, Some(2)));
        assert_eq!(it.len(), 2);
    }

    #[test]
    fn test_into_iter_inline_double_ended() {
        let mut sv: SmallVec<i32, 4> = SmallVec::new();
        sv.push(1);
        sv.push(2);
        sv.push(3);
        sv.push(4);
        assert!(sv.is_inline());
        let mut it = sv.into_iter();
        assert_eq!(it.next(), Some(1));
        assert_eq!(it.next_back(), Some(4));
        assert_eq!(it.len(), 2);
        assert_eq!(it.next(), Some(2));
        assert_eq!(it.next_back(), Some(3));
        assert_eq!(it.next(), None);
        assert_eq!(it.next_back(), None);
    }

    #[test]
    fn test_into_iter_inline_partial_consume_drops_remainder_once() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut sv: SmallVec<DropCounter, 4> = SmallVec::new();
        sv.push(DropCounter::new(&counter));
        sv.push(DropCounter::new(&counter));
        sv.push(DropCounter::new(&counter));
        assert!(sv.is_inline());

        let mut it = sv.into_iter();
        // Consume one element; dropping the returned value drops it once.
        drop(it.next().expect("first element"));
        assert_eq!(counter.get(), 1, "yielded element dropped exactly once");
        // Drop the iterator with two elements still buffered.
        drop(it);
        assert_eq!(counter.get(), 3, "all three elements dropped exactly once");
    }

    #[test]
    fn test_into_iter_inline_full_consume_drops_once() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut sv: SmallVec<DropCounter, 4> = SmallVec::new();
        for _ in 0..3 {
            sv.push(DropCounter::new(&counter));
        }
        assert!(sv.is_inline());
        let collected: Vec<DropCounter> = sv.into_iter().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(counter.get(), 0, "fully-yielded elements are still alive");
        drop(collected);
        assert_eq!(counter.get(), 3, "each element dropped exactly once");
    }

    #[test]
    fn test_into_iter_inline_drop_without_consume() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut sv: SmallVec<DropCounter, 4> = SmallVec::new();
        sv.push(DropCounter::new(&counter));
        sv.push(DropCounter::new(&counter));
        assert!(sv.is_inline());
        drop(sv.into_iter());
        assert_eq!(
            counter.get(),
            2,
            "unconsumed inline elements dropped once each"
        );
    }

    #[test]
    fn test_into_iter_inline_double_ended_partial_drops_once() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut sv: SmallVec<DropCounter, 8> = SmallVec::new();
        for _ in 0..6 {
            sv.push(DropCounter::new(&counter));
        }
        assert!(sv.is_inline());
        let mut it = sv.into_iter();
        drop(it.next().expect("front")); // start advances
        drop(it.next_back().expect("back")); // end retreats
        assert_eq!(counter.get(), 2);
        // Four elements remain in the middle range — all dropped exactly once.
        drop(it);
        assert_eq!(
            counter.get(),
            6,
            "front+back partial consume drops each once"
        );
    }

    #[test]
    fn test_into_iter_heap_partial_consume_drops_remainder_once() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut sv: SmallVec<DropCounter, 2> = SmallVec::new();
        for _ in 0..5 {
            sv.push(DropCounter::new(&counter));
        }
        assert!(!sv.is_inline());
        let mut it = sv.into_iter();
        drop(it.next().expect("first"));
        drop(it.next().expect("second"));
        assert_eq!(counter.get(), 2);
        drop(it);
        assert_eq!(
            counter.get(),
            5,
            "heap: all five elements dropped exactly once"
        );
    }

    #[test]
    fn test_into_iter_heap_drop_without_consume() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut sv: SmallVec<DropCounter, 2> = SmallVec::new();
        for _ in 0..5 {
            sv.push(DropCounter::new(&counter));
        }
        assert!(!sv.is_inline());
        drop(sv.into_iter());
        assert_eq!(
            counter.get(),
            5,
            "unconsumed heap elements dropped once each"
        );
    }

    #[test]
    fn test_into_iter_inline_panic_in_element_drop_no_double_drop() {
        use std::panic;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

        struct PanicOnThird(usize);
        impl Drop for PanicOnThird {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::Relaxed);
                if self.0 == 2 {
                    panic!("element drop panic");
                }
            }
        }

        DROP_COUNT.store(0, Ordering::Relaxed);
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut sv: SmallVec<PanicOnThird, 8> = SmallVec::new();
            for idx in 0..5 {
                sv.push(PanicOnThird(idx));
            }
            assert!(sv.is_inline());
            // Dropping the unconsumed iterator drops elements in order; the
            // element with idx == 2 panics part-way through the drop loop.
            drop(sv.into_iter());
        }));

        assert!(result.is_err(), "panicking element drop must unwind");
        // Elements 0, 1, 2 were dropped before the panic abandoned the loop;
        // 3 and 4 leak (memory-safe) but nothing is dropped twice — the
        // exactly-once discipline means the count is precisely 3, never more.
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 3);
    }
}

// ── Kani proofs ────────────────────────────────────────────────────────────

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Verify that pushing past inline capacity spills to heap while
    /// preserving all previously pushed elements.
    ///
    /// Fills inline capacity (4 elements), then pushes one more to trigger
    /// the spill. Verifies that after spill: (1) storage mode changed to
    /// heap, (2) all 5 elements are present and in correct order.
    #[kani::proof]
    #[kani::unwind(7)]
    fn smallvec_push_spill_preserves_elements() {
        let a: u32 = kani::any();
        let b: u32 = kani::any();
        let c: u32 = kani::any();
        let d: u32 = kani::any();
        let e: u32 = kani::any();

        let mut sv: SmallVec<u32, 4> = SmallVec::new();
        sv.push(a);
        sv.push(b);
        sv.push(c);
        sv.push(d);

        // Still inline at capacity.
        kani::assert(sv.is_inline(), "must be inline at capacity N");
        kani::assert(sv.len() == 4, "len must be 4 before spill");

        // Push the 5th element — triggers spill.
        sv.push(e);

        kani::assert(
            !sv.is_inline(),
            "must be on heap after exceeding inline capacity",
        );
        kani::assert(sv.len() == 5, "len must be 5 after spill push");

        // All elements must be preserved in push order.
        let slice = sv.as_slice();
        kani::assert(slice[0] == a, "element 0 must survive spill");
        kani::assert(slice[1] == b, "element 1 must survive spill");
        kani::assert(slice[2] == c, "element 2 must survive spill");
        kani::assert(slice[3] == d, "element 3 must survive spill");
        kani::assert(slice[4] == e, "element 4 (post-spill push) must be correct");
    }

    /// Verify as_slice length invariant: for a symbolic number of pushes,
    /// as_slice().len() always equals the SmallVec's len(), and the content
    /// matches in both inline and heap modes.
    #[kani::proof]
    #[kani::unwind(8)]
    fn smallvec_as_slice_length_invariant() {
        let count: usize = kani::any();
        // Allow up to 6 elements: 4 inline + 2 heap to exercise both paths.
        kani::assume(count <= 6);

        let vals: [u32; 6] = [
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
        ];

        let mut sv: SmallVec<u32, 4> = SmallVec::new();
        let mut i = 0;
        while i < count {
            sv.push(vals[i]);
            i += 1;
        }

        let slice = sv.as_slice();
        kani::assert(
            slice.len() == count,
            "as_slice length must equal number of pushed elements",
        );
        kani::assert(slice.len() == sv.len(), "as_slice length must equal len()");

        // Verify each element matches what was pushed.
        let mut j = 0;
        while j < count {
            kani::assert(
                slice[j] == vals[j],
                "as_slice element must match pushed value",
            );
            j += 1;
        }

        // Verify storage mode is consistent with count.
        if count <= 4 {
            kani::assert(sv.is_inline(), "must be inline when count <= N");
        } else {
            kani::assert(!sv.is_inline(), "must be heap when count > N");
        }
    }
}

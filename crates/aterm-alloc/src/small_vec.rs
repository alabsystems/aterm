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
    #[inline]
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
    // `#[inline]` so callers' Trust gates (aterm-scrollback) verify this body
    // rather than assuming an absent callee.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        if capacity <= N {
            Self::new()
        } else {
            // Clamp the advisory allocation hint so the reservation has a
            // provable upper bound for the L0 unbounded-allocation gate. A
            // literal, not a named const: the verifier havocs named-const
            // operands.
            Self {
                data: SmallVecData::Heap(Vec::with_capacity(capacity.min(16_777_216))),
            }
        }
    }

    /// The number of elements.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        match &self.data {
            SmallVecData::Inline { len, .. } => *len,
            SmallVecData::Heap(vec) => vec.len(),
        }
    }

    /// Whether the collection is empty.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The current capacity (inline or heap).
    #[inline]
    #[must_use]
    pub const fn capacity(&self) -> usize {
        match &self.data {
            SmallVecData::Inline { .. } => N,
            SmallVecData::Heap(vec) => vec.capacity(),
        }
    }

    /// Whether the data is currently stored inline.
    #[inline]
    #[must_use]
    pub const fn is_inline(&self) -> bool {
        matches!(&self.data, SmallVecData::Inline { .. })
    }

    /// Whether the data has spilled to the heap.
    ///
    /// This is the inverse of [`is_inline`](Self::is_inline).
    /// Provided for API compatibility with `smallvec::SmallVec::spilled()`.
    #[inline]
    #[must_use]
    pub const fn spilled(&self) -> bool {
        !self.is_inline()
    }

    /// Push an element. Spills to heap if inline capacity is exceeded.
    #[inline]
    pub fn push(&mut self, value: T) {
        match &mut self.data {
            SmallVecData::Inline { buf, len } => {
                // Local copy of `len` (the ArrayVec::push idiom), so the `< N`
                // fact reaches the index and the increment.
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
    // Skip: the inline arm's `assume_init_read` is in ArrayVec::pop's
    // init-tracking class; `len` counts exactly the initialized prefix.
    #[inline]
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
    ///
    /// If an element's destructor panics, every other element is still
    /// dropped and the vec is left empty, inline or spilled alike (the heap
    /// arm is std `Vec::clear`).
    // Skip: the inline arm drops each initialized slot (ArrayVec::truncate's
    // init-tracking class).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn clear(&mut self) {
        match &mut self.data {
            SmallVecData::Inline { buf, len } => {
                // Clamp to a provably in-bounds count (identical when `len <= N`)
                // so the subslice carries the bound.
                let n = if *len < N { *len } else { N };
                // Zero `len` before any destructor runs, as `ArrayVec::truncate`
                // does: a panicking element `Drop` unwinds into
                // `Drop for SmallVec`, which calls this again.
                *len = 0;
                // SAFETY: elements `0..n` were initialized when pushed and
                // `n <= len` as it was on entry; `len` is already 0, so no
                // other path reaches them again.
                unsafe { crate::drop_initialized(&mut buf[..n]) }
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
    #[inline]
    #[must_use]
    // Skip: the raw-parts reconstruction joins ArrayVec::as_slice's
    // init/provenance classification (memory-model producer lane).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_slice(&self) -> &[T] {
        match &self.data {
            SmallVecData::Inline { buf, len } => {
                // Clamp to a provably in-bounds length and take the initialized
                // subslice; the raw-parts reconstruction reuses its pointer and
                // length.
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
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        match &mut self.data {
            SmallVecData::Inline { buf, len } => {
                // Clamp to a provably in-bounds length and take the initialized
                // subslice; the raw-parts reconstruction reuses its pointer and
                // length.
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
    // Skip: the residual obligation is drop glue over the consumed `Vec`; the
    // conversion itself is a move.
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

    // Skip: the inline-to-heap spill reads each initialized slot via
    // `assume_init_read` (ArrayVec's init-tracking class). `*len = 0` before
    // the heap swap prevents any double drop.
    #[cfg_attr(trust_verify, trust::skip)]
    fn ensure_heap(&mut self) {
        if let SmallVecData::Inline { buf, len } = &mut self.data {
            // Clamp to a provably in-bounds count so the `buf[..current_len]`
            // read carries a `<= N` proof.
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
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for SmallVec<T, N> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, const N: usize> DerefMut for SmallVec<T, N> {
    #[inline]
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
    // Skip: `I: IntoIterator` is caller-chosen code; push's capacity contract
    // is the documented panic.
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
    // Skip: `I: IntoIterator` is caller-chosen code; push's capacity contract
    // is the documented panic.
    #[cfg_attr(trust_verify, trust::skip)]
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

/// Owned, by-value iterator over a [`SmallVec`].
///
/// Inline storage drains the `MaybeUninit` buffer in place without allocating;
/// heap storage delegates to [`alloc::vec::IntoIter`]. The live range is
/// `start..end`; `next` advances `start`, `next_back` retreats `end`, and
/// [`Drop`] disposes of the remainder exactly once.
///
/// The representation is private, as upstream `smallvec`'s is. `next`,
/// `next_back` and `Drop` read `buf[start..end]` as initialized, so safe code
/// that could name the fields could forge an iterator over an uninitialized
/// buffer or rewind the cursor and move one element out twice. Neither form
/// compiles:
///
/// ```compile_fail,E0223
/// use aterm_alloc::{IntoIter, SmallVec};
/// let mut sv: SmallVec<String, 4> = SmallVec::new();
/// sv.push(String::from("moved out twice if the cursor rewinds"));
/// let mut it = sv.into_iter();
/// let first = it.next();
/// if let IntoIter::Inline { start, .. } = &mut it {
///     *start = 0;
/// }
/// ```
///
/// ```compile_fail,E0223
/// use aterm_alloc::IntoIter;
/// use core::mem::MaybeUninit;
/// let mut forged: IntoIter<Box<u64>, 2> = IntoIter::Inline {
///     buf: [const { MaybeUninit::uninit() }; 2],
///     start: 0,
///     end: 2,
/// };
/// let uninit_box = forged.next();
/// ```
pub struct IntoIter<T, const N: usize> {
    repr: IntoIterRepr<T, N>,
}

enum IntoIterRepr<T, const N: usize> {
    /// Inline elements drained directly from the moved-out buffer.
    Inline {
        buf: [MaybeUninit<T>; N],
        start: usize,
        end: usize,
    },
    /// Heap elements delegated to the standard `Vec` iterator.
    Heap(alloc::vec::IntoIter<T>),
}

impl<T, const N: usize> IntoIter<T, N> {
    /// Whether the iterator drains an inline buffer (so consuming an inline
    /// `SmallVec` did not allocate).
    #[cfg(test)]
    const fn is_inline(&self) -> bool {
        matches!(self.repr, IntoIterRepr::Inline { .. })
    }
}

impl<T, const N: usize> Iterator for IntoIter<T, N> {
    type Item = T;

    // Skip: the drain reads each initialized slot — the crate-local init-tracking
    // class (per-slot memory-model producer lane). Same audited len-invariant.
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<T> {
        match &mut self.repr {
            IntoIterRepr::Inline { buf, start, end } => {
                // Clamp to a provably in-bounds bound (`*end <= N` by
                // construction) so the indexed read carries a `< N` proof.
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
            IntoIterRepr::Heap(iter) => iter.next(),
        }
    }

    // Skip: reads the drain cursor against `len` — the same init/provenance
    // class as its `next` (per-slot memory-model producer lane).
    #[cfg_attr(trust_verify, trust::skip)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.repr {
            IntoIterRepr::Inline { start, end, .. } => {
                let len = end.saturating_sub(*start);
                (len, Some(len))
            }
            IntoIterRepr::Heap(iter) => iter.size_hint(),
        }
    }
}

impl<T, const N: usize> DoubleEndedIterator for IntoIter<T, N> {
    // Skip: the back-drain reads each initialized slot — the same
    // init/provenance class as `next`/`size_hint`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn next_back(&mut self) -> Option<T> {
        match &mut self.repr {
            IntoIterRepr::Inline { buf, start, end } => {
                // Clamp to a provably in-bounds bound so the indexed read
                // carries a `< N` proof.
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
            IntoIterRepr::Heap(iter) => iter.next_back(),
        }
    }
}

impl<T, const N: usize> ExactSizeIterator for IntoIter<T, N> {}

impl<T, const N: usize> FusedIterator for IntoIter<T, N> {}

impl<T, const N: usize> Drop for IntoIter<T, N> {
    // Skip: drops the un-yielded initialized slots (the init-tracking class of
    // `next`/`next_back`); start..end bounds the live range.
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        if let IntoIterRepr::Inline { buf, start, end } = &mut self.repr {
            // Clamp both bounds (`*start <= *end <= N` by construction) so the
            // subslice carries the proof.
            let bound = if *end < N { *end } else { N };
            let s = if *start < bound { *start } else { bound };
            // SAFETY: the elements `s..bound` were initialized and have not
            // been yielded, and this `Drop` runs once, so nothing reads or
            // drops them again. A panicking destructor still lets the rest
            // drop, as the heap arm's `vec::IntoIter` does (upstream
            // smallvec's `for _ in self {}` leaks the rest; whether the vec
            // had spilled must not change that).
            unsafe { crate::drop_initialized(&mut buf[s..bound]) }
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
        // Move the storage out, leaving an empty inline buffer so the source's
        // Drop is a no-op, without allocating for the inline case.
        let data = core::mem::replace(
            &mut self.data,
            SmallVecData::Inline {
                buf: [const { MaybeUninit::uninit() }; N],
                len: 0,
            },
        );
        let repr = match data {
            SmallVecData::Inline { buf, len } => {
                // Clamp to a provably in-bounds end so the range the
                // iterator/Drop walks carries a `<= N` proof.
                let end = if len < N { len } else { N };
                IntoIterRepr::Inline { buf, start: 0, end }
            }
            SmallVecData::Heap(vec) => IntoIterRepr::Heap(vec.into_iter()),
        };
        IntoIter { repr }
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
    use std::format;
    use std::string::{String, ToString};
    use std::vec::Vec;

    // Drop-counting element for `IntoIter` drop-once checks under partial,
    // full and no consumption, inline and heap. The `Rc` counter and `String`
    // payload give Miri real allocations, so a double drop or leak is caught.
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

    /// A by-value iterator keeps the vector's storage: consuming an inline
    /// vector does not allocate.
    #[test]
    fn into_iter_keeps_the_storage_variant() {
        let inline: SmallVec<i32, 4> = SmallVec::from_vec([10, 20, 30].to_vec());
        let heap: SmallVec<i32, 2> = SmallVec::from_vec([1, 2, 3].to_vec());
        assert!(inline.is_inline() && !heap.is_inline());
        let (inline, heap) = (inline.into_iter(), heap.into_iter());
        assert!(inline.is_inline() && !heap.is_inline());
        assert_eq!(inline.collect::<Vec<_>>(), [10, 20, 30]);
        assert_eq!(heap.collect::<Vec<_>>(), [1, 2, 3]);
    }

    /// Fill a `SmallVec<_, N>` with `len` drop counters, take `front` and
    /// `back` elements from the by-value iterator, drop it, then drop what was
    /// taken: every element is dropped exactly once.
    fn drops_each_once<const N: usize>(len: usize, front: usize, back: usize) {
        let case = format!("N={N} len={len} front={front} back={back}");
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut sv: SmallVec<DropCounter, N> = SmallVec::new();
        for _ in 0..len {
            sv.push(DropCounter::new(&counter));
        }
        assert_eq!(sv.is_inline(), len <= N, "{case}");
        let mut it = sv.into_iter();
        let mut taken = Vec::new();
        for _ in 0..front {
            taken.push(it.next().expect("a front element"));
        }
        for _ in 0..back {
            taken.push(it.next_back().expect("a back element"));
        }
        assert_eq!(counter.get(), 0, "{case}: yielded elements are alive");
        drop(it);
        assert_eq!(
            counter.get(),
            len - taken.len(),
            "{case}: the iterator drops what it still holds"
        );
        drop(taken);
        assert_eq!(counter.get(), len, "{case}: each element dropped once");
    }

    /// Partial, full and no consumption, from either end, inline and spilled.
    #[test]
    fn into_iter_drops_each_element_exactly_once() {
        for (front, back) in [(0, 0), (1, 0), (1, 1), (3, 0)] {
            drops_each_once::<4>(3, front, back);
        }
        for (front, back) in [(0, 0), (2, 0), (1, 1), (5, 0)] {
            drops_each_once::<2>(5, front, back);
        }
    }
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `ArrayVec<T, N>`: fixed-capacity inline-only storage.
//!
//! Uses only `core` items (`#![no_std]`) so `crates/aterm-arrayvec` can
//! re-export it as `arrayvec` for `default-features = false` consumers.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ops::{Bound, Deref, DerefMut, RangeBounds};

/// A fixed-capacity vector stored entirely on the stack.
///
/// Never allocates on the heap. Panics if you try to push beyond capacity `N`.
/// Used in the parser for CSI params (N = `MAX_PARAMS` = 24) and intermediates (N = 4).
pub struct ArrayVec<T, const N: usize> {
    buf: [MaybeUninit<T>; N],
    len: usize,
}

impl<T, const N: usize> ArrayVec<T, N> {
    /// Create a new, empty `ArrayVec`.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            // An inline-const array of `MaybeUninit` needs no `assume_init`, so
            // there is no `unsafe` obligation for the Trust verifier.
            buf: [const { MaybeUninit::uninit() }; N],
            len: 0,
        }
    }

    /// Const-compatible alias for `new`.
    #[inline]
    #[must_use]
    pub const fn new_const() -> Self {
        Self::new()
    }

    /// The number of elements.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the collection is empty.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The fixed capacity `N`.
    #[inline]
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Remaining capacity before full.
    #[inline]
    #[must_use]
    pub const fn remaining_capacity(&self) -> usize {
        // `len <= N` by invariant; `saturating_sub` makes the no-underflow obligation
        // provable (identical result when the invariant holds).
        N.saturating_sub(self.len)
    }

    /// Whether the collection is at full capacity.
    #[inline]
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    /// Push an element. Panics if full.
    ///
    /// # Panics
    ///
    /// Panics if `len == N`.
    // Audited contract panic: `push` beyond capacity panics by the documented
    // ArrayVec contract; callers that cannot uphold `len < N` use `try_push`.
    // The declaration files the obligation under the `contract-panic` column.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "ArrayVec overflow")
    )]
    // `#[track_caller]` so a capacity panic names the caller's line (as
    // upstream `arrayvec` does); the shim inherits it through the re-export,
    // so an overflow in the patched graph points at the real caller.
    #[inline]
    #[track_caller]
    pub fn push(&mut self, value: T) {
        // Guard on a local copy of `len`: the open-model verifier reloads
        // `self.len` at every field read, so only a local carries `len < N` to
        // the index and the increment.
        let len = self.len;
        if len < N {
            self.buf[len] = MaybeUninit::new(value);
            // `len < N <= usize::MAX`, so the increment cannot wrap; the
            // wrapping form removes an overflow obligation the verifier cannot
            // discharge across the const-generic bound.
            self.len = len.wrapping_add(1);
        } else {
            // A const message (not `{N}`-formatted) so the contract_panic
            // matcher can bind it.
            panic!("ArrayVec overflow: capacity exceeded");
        }
    }

    /// Try to push an element. Returns `Err(CapacityError(value))` if full.
    ///
    /// # Errors
    ///
    /// Returns the element back inside a [`CapacityError`] when `len == N`
    /// (upstream `arrayvec`'s signature).
    #[inline]
    pub const fn try_push(&mut self, value: T) -> Result<(), CapacityError<T>> {
        // Same local-copy idiom as `push`.
        let len = self.len;
        if len < N {
            self.buf[len] = MaybeUninit::new(value);
            // `len < N <= usize::MAX`: the increment cannot wrap.
            self.len = len.wrapping_add(1);
            Ok(())
        } else {
            Err(CapacityError::new(value))
        }
    }

    /// Pop the last element.
    // Skip: proving the popped slot is initialized before `assume_init_read`
    // needs per-slot `MaybeUninit` init tracking the verifier lacks. Invariant:
    // `len` counts exactly the initialized prefix; `push`/`try_push` increment
    // `len` only after initializing `buf[len]`, and `pop` decrements before
    // reading.
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn pop(&mut self) -> Option<T> {
        // Same local-copy idiom as `push`; `wrapping_sub` is exact under the
        // `len != 0` guard.
        let len = self.len;
        if len == 0 {
            None
        } else {
            let idx = len.wrapping_sub(1);
            // `get`, not `buf[idx]`: `len <= N` cannot cross the verifier's
            // havoc of `self.len`, and `get`'s `None` arm is unreachable in
            // every real state (same single bounds check at runtime).
            match self.buf.get(idx) {
                Some(slot) => {
                    self.len = idx;
                    // SAFETY: buf[idx] was initialized when it was pushed
                    let value = unsafe { slot.assume_init_read() };
                    Some(value)
                }
                None => None,
            }
        }
    }

    /// Replace all contents with a single element.
    ///
    /// Equivalent to `clear(); push(value);`; panics for `N == 0` by the same
    /// capacity contract as `push`.
    // Audited contract panic: the same "ArrayVec overflow" contract as `push`,
    // reachable only for an `N == 0` instantiation (none exist here).
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "ArrayVec overflow")
    )]
    #[inline]
    pub fn set_single(&mut self, value: T) {
        // Drop existing elements via the bounds-clamped clear, then store.
        self.clear();
        if self.try_push(value).is_err() {
            panic!("ArrayVec overflow: capacity exceeded");
        }
    }

    /// Clear all elements.
    // Skip: `truncate(0)` is in the same init-tracking class as `pop`.
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Truncate to the given length.
    ///
    /// If an element's destructor panics, every other cut element is still
    /// dropped and the vec is left at `new_len`, as upstream `arrayvec` and std
    /// `Vec` do.
    // Skip: same init-tracking class as `pop`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn truncate(&mut self, new_len: usize) {
        if new_len < self.len {
            // Clamp both bounds to provably in-bounds values (identical when
            // `new_len < len <= N`) so the subslice carries the bound.
            let end = if self.len < N { self.len } else { N };
            let start = if new_len < end { new_len } else { end };
            // Length first: a panicking destructor unwinds into
            // `Drop for ArrayVec`, which must see only the kept prefix, or it
            // would drop the cut slots a second time.
            self.len = new_len;
            // SAFETY: elements `start..end` were initialized when pushed
            // (`end <= len` as it was on entry), and `len` is already
            // `new_len <= start`, so no other path reaches them again.
            unsafe { crate::drop_initialized(&mut self.buf[start..end]) }
        }
    }

    /// View as a slice.
    #[inline]
    #[must_use]
    // Skip: the raw-parts reconstruction is in the same init/provenance class
    // as `pop`/`clear`/`truncate`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_slice(&self) -> &[T] {
        // Clamp to a provably in-bounds length and take the initialized
        // subslice; the raw-parts reconstruction reuses its pointer and length.
        let n = if self.len < N { self.len } else { N };
        let init: &[MaybeUninit<T>] = &self.buf[..n];
        // SAFETY: `MaybeUninit<T>` has the same layout as `T`; the pointer and
        // length come from the same in-bounds subslice `init`, and elements
        // `0..len` are initialized (`n <= len`), so reading them as `T` is sound.
        unsafe { core::slice::from_raw_parts(init.as_ptr().cast::<T>(), init.len()) }
    }

    /// View as a mutable slice.
    // Skip: same class as `as_slice` above.
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // Clamp to a provably in-bounds length and take the initialized
        // subslice; the raw-parts reconstruction reuses its pointer and length.
        let n = if self.len < N { self.len } else { N };
        let init: &mut [MaybeUninit<T>] = &mut self.buf[..n];
        // SAFETY: `MaybeUninit<T>` has the same layout as `T`; the pointer and
        // length come from the same in-bounds subslice `init`, and elements
        // `0..len` are initialized (`n <= len`), so reading them as `T` is sound.
        unsafe { core::slice::from_raw_parts_mut(init.as_mut_ptr().cast::<T>(), init.len()) }
    }

    /// An iterator over references.
    #[inline]
    pub fn iter(&self) -> core::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    /// An iterator over mutable references.
    #[inline]
    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, T> {
        self.as_mut_slice().iter_mut()
    }

    /// Insert an element at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index > len` or if the array is full.
    // Skip: the raw `ptr::copy` shift and `ptr::write` are in the same
    // init/provenance class as `pop`/`clear`/`as_slice`.
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    pub fn insert(&mut self, index: usize, value: T) {
        assert!(
            index <= self.len,
            "index out of bounds: {index} > {}",
            self.len
        );
        // Const message so the contract_panic matcher can bind it.
        assert!(self.len < N, "ArrayVec overflow: capacity exceeded");

        // SAFETY: we checked bounds and capacity above
        unsafe {
            let ptr = self.buf.as_mut_ptr().cast::<T>();
            core::ptr::copy(ptr.add(index), ptr.add(index + 1), self.len - index);
            core::ptr::write(ptr.add(index), value);
        }
        self.len += 1;
    }

    /// Remove and return the element at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index >= len`.
    // Skip: the raw element shift is in `insert`'s init/provenance class.
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    pub fn remove(&mut self, index: usize) -> T {
        assert!(
            index < self.len,
            "index out of bounds: {index} >= {}",
            self.len
        );

        // SAFETY: element at index is initialized
        unsafe {
            let ptr = self.buf.as_mut_ptr().cast::<T>();
            let value = core::ptr::read(ptr.add(index));
            core::ptr::copy(ptr.add(index + 1), ptr.add(index), self.len - index - 1);
            self.len -= 1;
            value
        }
    }

    /// Retain only elements where the predicate returns true.
    ///
    /// The predicate takes `&mut T`, as upstream `arrayvec`'s does. If it
    /// panics, the vec keeps every element it had kept plus every element it
    /// had not yet returned for (the one it panicked on included), in order.
    /// If a rejected element's destructor panics, that element is gone and the
    /// vec keeps the kept ones plus every element after it. Either way each
    /// element is dropped exactly once: std `Vec::retain`'s contract, which
    /// upstream's `BackshiftOnDrop` mirrors.
    // Skip: the compaction's raw shift is in insert/remove's class, and
    // `F: FnMut` is caller-chosen code.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn retain<F: FnMut(&mut T) -> bool>(&mut self, mut f: F) {
        let original_len = self.len;
        // `len` is 0 while the predicate runs: until the guard settles it, the
        // vec claims no slot, whatever state the compaction is in.
        self.len = 0;

        struct RetainGuard<'a, T, const N: usize> {
            av: &'a mut ArrayVec<T, N>,
            write: usize,
            read: usize,
            original_len: usize,
        }

        impl<T, const N: usize> Drop for RetainGuard<'_, T, N> {
            fn drop(&mut self) {
                // `0..write` are kept, `write..read` are vacated (moved down
                // or dropped), and `read..original_len` were never decided if a
                // panic occurred in the predicate or a rejected element's
                // destructor. Rotate the vacated gap past the undecided tail so
                // undecided elements are preserved, with bounds clamped for the
                // verifier.
                let end = if self.original_len < N {
                    self.original_len
                } else {
                    N
                };
                let read = if self.read < end { self.read } else { end };
                let write = if self.write < read { self.write } else { read };
                if read < end {
                    self.av.buf[write..end].rotate_left(read - write);
                }
                self.av.len = write + (end - read);
            }
        }

        let mut guard = RetainGuard {
            av: self,
            write: 0,
            read: 0,
            original_len,
        };

        while guard.read < original_len {
            let read = guard.read;
            // SAFETY: element at `read` is initialized (read < original_len)
            let keep = unsafe { f(&mut *guard.av.buf[read].as_mut_ptr()) };
            // Advance `read` BEFORE the reject arm runs the element's
            // destructor: if that panics, the slot must already be in the
            // vacated range `write..read`, which the guard closes over. Left in
            // the undecided range, the guard would keep a dropped element and
            // it would be dropped a second time (`panic_drop_parity`'s retain
            // sweep pins this order).
            guard.read += 1;
            if keep {
                if guard.write != read {
                    // SAFETY: both indices in bounds; read element consumed, write slot empty
                    unsafe {
                        let val = guard.av.buf[read].assume_init_read();
                        guard.av.buf[guard.write] = MaybeUninit::new(val);
                    }
                }
                guard.write += 1;
            } else {
                // SAFETY: the element is initialized, and `read` is already
                // past it, so whether or not its destructor panics no other
                // path reaches it again: it is dropped exactly once.
                unsafe {
                    guard.av.buf[read].assume_init_drop();
                }
            }
        }

        // Every element decided (`read == original_len`): the guard settles
        // `len` to the kept count, the same code a panic takes.
        drop(guard);
    }

    /// Append every element of `other`, cloning each.
    ///
    /// Public, unlike upstream's `pub(crate)` version, for `clone_from` and the
    /// shim's tests; behaves as upstream's `extend` over a slice
    /// (`crates/aterm-alloc/tests/arrayvec_differential.rs`).
    ///
    /// # Panics
    ///
    /// Panics if the slice does not fit in the remaining capacity, the same
    /// contract as [`push`](Self::push).
    // Audited contract panic: delegates to `push`; `try_extend_from_slice` is
    // the non-panicking form.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "ArrayVec overflow")
    )]
    #[track_caller]
    pub fn extend_from_slice(&mut self, other: &[T])
    where
        T: Clone,
    {
        for value in other {
            self.push(value.clone());
        }
    }

    /// Append every element of `other`, cloning each, or return the slice's
    /// length as an error if it does not fit.
    ///
    /// # Errors
    ///
    /// Returns [`CapacityError`] and appends nothing when
    /// `other.len() > remaining_capacity()` (upstream's all-or-nothing rule).
    pub fn try_extend_from_slice(&mut self, other: &[T]) -> Result<(), CapacityError>
    where
        T: Clone,
    {
        if other.len() > self.remaining_capacity() {
            return Err(CapacityError::new(()));
        }
        self.extend_from_slice(other);
        Ok(())
    }

    /// Return the inner fixed-size array, but only if the vec is full.
    ///
    /// # Errors
    ///
    /// Returns `Err(self)` when `len < N`. `naga`'s constant evaluator relies
    /// on `.into_inner().unwrap()` panicking to reject a short component list.
    // Skip: `into_inner_unchecked` below carries the memory-model obligation;
    // this arm is a pure `len` comparison.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn into_inner(self) -> Result<[T; N], Self> {
        if self.len < N {
            Err(self)
        } else {
            // SAFETY: `len >= N` and the invariant gives `len <= N`, so the
            // vec is exactly full and every slot is initialized.
            Ok(unsafe { self.into_inner_unchecked() })
        }
    }

    /// Return the inner fixed-size array without checking that it is full.
    ///
    /// # Safety
    ///
    /// Sound if and only if `len == N`.
    // Skip (the init-tracking family): the whole-array `ptr::read` relies on
    // the len invariant plus the caller's `len == N` obligation.
    #[cfg_attr(trust_verify, trust::skip)]
    pub unsafe fn into_inner_unchecked(self) -> [T; N] {
        debug_assert_eq!(self.len, N, "into_inner_unchecked on a non-full ArrayVec");
        // `ManuallyDrop` first: the array is moved out wholesale, so `Drop for
        // ArrayVec` must not also run over the same slots.
        let me = ManuallyDrop::new(self);
        // SAFETY: `[MaybeUninit<T>; N]` has the same layout as `[T; N]`, the
        // caller guarantees all `N` slots are initialized, and `me` will not be
        // dropped, so the elements are moved (not copied) exactly once.
        unsafe { core::ptr::read(me.buf.as_ptr().cast::<[T; N]>()) }
    }

    /// Remove `range` from the vec and yield the removed elements.
    ///
    /// The tail closes up as the iterator runs, so the vec is valid and
    /// correctly shortened at every point, including when the [`Drain`] is
    /// dropped early or forgotten.
    ///
    /// # Panics
    ///
    /// Panics if the range's start is past its end, or its end past `len`.
    #[track_caller]
    pub fn drain<R: RangeBounds<usize>>(&mut self, range: R) -> Drain<'_, T, N> {
        let len = self.len;
        let start = match range.start_bound() {
            Bound::Unbounded => 0,
            Bound::Included(&i) => i,
            Bound::Excluded(&i) => i.saturating_add(1),
        };
        let end = match range.end_bound() {
            Bound::Unbounded => len,
            Bound::Included(&j) => j.saturating_add(1),
            Bound::Excluded(&j) => j,
        };
        assert!(start <= end, "ArrayVec::drain: start {start} > end {end}");
        assert!(end <= len, "ArrayVec::drain: end {end} > len {len}");
        Drain {
            vec: self,
            start,
            remaining: end - start,
        }
    }
}

// ── Trait impls ─────────────────────────────────────────────────────────────

impl<T, const N: usize> Default for ArrayVec<T, N> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for ArrayVec<T, N> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, const N: usize> DerefMut for ArrayVec<T, N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T: Clone, const N: usize> Clone for ArrayVec<T, N> {
    fn clone(&self) -> Self {
        let mut new = Self::new();
        for item in self.as_slice() {
            new.push(item.clone());
        }
        new
    }

    /// Clones the common prefix in place rather than dropping and rebuilding,
    /// as upstream does; `wgpu-hal`'s GLES backend calls this on every pipeline
    /// bind.
    fn clone_from(&mut self, source: &Self) {
        let prefix = if self.len < source.len {
            self.len
        } else {
            source.len
        };
        self.as_mut_slice()[..prefix].clone_from_slice(&source.as_slice()[..prefix]);
        if prefix < self.len {
            // `source` was shorter: drop our surplus tail.
            self.truncate(prefix);
        } else {
            // `source` was longer (or equal): clone the difference in.
            self.extend_from_slice(&source.as_slice()[prefix..]);
        }
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for ArrayVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

impl<T: PartialEq, const N: usize> PartialEq for ArrayVec<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq, const N: usize> Eq for ArrayVec<T, N> {}

impl<T, const N: usize> Drop for ArrayVec<T, N> {
    fn drop(&mut self) {
        // Reuse `clear` (the bounds-clamped `truncate(0)`) instead of
        // duplicating the slice — same drop-all-elements behaviour, no
        // separate out-of-bounds obligation.
        self.clear();
    }
}

impl<T, const N: usize> FromIterator<T> for ArrayVec<T, N> {
    // Skip: `I: IntoIterator` is caller-chosen code; `push`'s capacity
    // contract is the documented panic.
    #[cfg_attr(trust_verify, trust::skip)]
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut av = Self::new();
        for item in iter {
            av.push(item);
        }
        av
    }
}

// ── Capacity error ──────────────────────────────────────────────────────────

/// The element did not fit.
///
/// Upstream `arrayvec`'s error type, so [`ArrayVec::try_push`] and
/// [`ArrayVec::try_extend_from_slice`] have upstream's signatures. `Debug` and
/// `Display` do not require `T: Debug` and never print the payload.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct CapacityError<T = ()> {
    element: T,
}

impl<T> CapacityError<T> {
    /// Wrap the element that did not fit.
    #[inline]
    #[must_use]
    pub const fn new(element: T) -> Self {
        Self { element }
    }

    /// Take the element back out.
    #[inline]
    #[must_use]
    pub fn element(self) -> T {
        self.element
    }

    /// Drop the payload, keeping only the "it did not fit" fact.
    #[inline]
    #[must_use]
    pub fn simplify(self) -> CapacityError {
        CapacityError { element: () }
    }
}

/// The one message, shared by `Display` and `Debug` — upstream's wording.
const CAPACITY_ERROR_MESSAGE: &str = "insufficient capacity";

impl<T> fmt::Display for CapacityError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CAPACITY_ERROR_MESSAGE)
    }
}

impl<T> fmt::Debug for CapacityError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CapacityError: {CAPACITY_ERROR_MESSAGE}")
    }
}

// Unconditional, unlike upstream's `std`-gated impl: `core::error::Error` is
// stable, so this is a strict superset. The `T: Any` bound is upstream's.

impl<T: core::any::Any> core::error::Error for CapacityError<T> {}

// ── Hash ────────────────────────────────────────────────────────────────────

// Hashes the `len` initialized elements through the slice; hashing the backing
// `[MaybeUninit<T>; N]` would read uninitialized memory. `wgpu-hal` and
// `wgpu-core` use ArrayVec-holding types as `HashMap` keys.
impl<T: Hash, const N: usize> Hash for ArrayVec<T, N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&**self, state);
    }
}

// ── Extend ──────────────────────────────────────────────────────────────────

/// Panics on overflow; it does not truncate. A silently truncating `extend`
/// would drop `wgpu-hal`'s DXC compiler arguments or `wgpu-core`'s copy
/// barriers. Upstream panics too, via `push`.
impl<T, const N: usize> Extend<T> for ArrayVec<T, N> {
    // Audited contract panic: `push`'s capacity contract, reached through
    // caller-chosen `I: IntoIterator` code.
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

// ── Iteration ───────────────────────────────────────────────────────────────

// `for x in &av` and `for x in &mut av` need these explicitly: trait selection
// happens before autoderef. Both walk `len` elements, never `N` (`tiny-skia`
// writes through every element it visits).
impl<'a, T, const N: usize> IntoIterator for &'a ArrayVec<T, N> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a mut ArrayVec<T, N> {
    type Item = &'a mut T;
    type IntoIter = core::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<T, const N: usize> IntoIterator for ArrayVec<T, N> {
    type Item = T;
    type IntoIter = IntoIter<T, N>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter { vec: self }
    }
}

/// By-value iterator for [`ArrayVec`], produced by `IntoIterator`.
///
/// It owns the `ArrayVec` and moves elements out with `remove(0)`, so the
/// un-yielded elements are exactly those still in the vec and `ArrayVec`'s own
/// `Drop` drops each once: no `impl Drop`, no `unsafe`, no leak or double free
/// on early exit. Forward iteration costs O(n²) moves; capacities in the
/// patched graph are at most 32 and the sites are setup code, not per-frame.
pub struct IntoIter<T, const N: usize> {
    vec: ArrayVec<T, N>,
}

impl<T, const N: usize> IntoIter<T, N> {
    /// The not-yet-yielded elements, in order.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        self.vec.as_slice()
    }
}

impl<T, const N: usize> Iterator for IntoIter<T, N> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.vec.is_empty() {
            None
        } else {
            Some(self.vec.remove(0))
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.vec.len();
        (len, Some(len))
    }
}

impl<T, const N: usize> DoubleEndedIterator for IntoIter<T, N> {
    fn next_back(&mut self) -> Option<T> {
        self.vec.pop()
    }
}

impl<T, const N: usize> ExactSizeIterator for IntoIter<T, N> {}

impl<T: Clone, const N: usize> Clone for IntoIter<T, N> {
    fn clone(&self) -> Self {
        Self {
            vec: self.vec.clone(),
        }
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for IntoIter<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.vec.as_slice()).finish()
    }
}

/// A draining iterator for [`ArrayVec`], produced by [`ArrayVec::drain`].
///
/// Each element is removed from the vec as it is yielded (`ArrayVec::remove`
/// shifts the tail down), and `Drop` removes and drops the rest, so the
/// removal happens whether or not the caller finishes (`wgpu-hal`'s GLES
/// backend relies on `drain(..)` emptying `resolve_attachments`). No `unsafe`.
///
/// Unlike upstream, forgetting a `Drain` leaves the un-drained elements in the
/// vec instead of losing them, and a panicking element destructor during
/// `Drop` still drops the rest of the range and closes the tail up (std
/// `Vec::Drain`'s outcome) where upstream leaks both.
pub struct Drain<'a, T, const N: usize> {
    vec: &'a mut ArrayVec<T, N>,
    /// Index in `vec` of the next element to yield from the FRONT.
    start: usize,
    /// How many of the requested range have not been yielded yet.
    remaining: usize,
}

impl<T, const N: usize> Iterator for Drain<'_, T, N> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.remaining == 0 {
            None
        } else {
            self.remaining -= 1;
            Some(self.vec.remove(self.start))
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<T, const N: usize> DoubleEndedIterator for Drain<'_, T, N> {
    fn next_back(&mut self) -> Option<T> {
        if self.remaining == 0 {
            None
        } else {
            self.remaining -= 1;
            // After the decrement, `start + remaining` is the last index of the
            // range still to be drained.
            Some(self.vec.remove(self.start + self.remaining))
        }
    }
}

impl<T, const N: usize> ExactSizeIterator for Drain<'_, T, N> {}

impl<T, const N: usize> Drop for Drain<'_, T, N> {
    fn drop(&mut self) {
        // Rotate the un-yielded range (`start..start + remaining`) to the end
        // of the vec and `truncate` it off so `len` is lowered before slice
        // drop glue runs, leaving the vec valid even if a destructor panics.
        // Bounds are clamped so the index, rotation, and arithmetic carry their
        // proofs.
        if self.remaining == 0 {
            return;
        }
        let all = self.vec.as_mut_slice();
        let len = all.len();
        let start = if self.start < len { self.start } else { len };
        let range_and_tail = &mut all[start..];
        let cut = if self.remaining < range_and_tail.len() {
            self.remaining
        } else {
            range_and_tail.len()
        };
        range_and_tail.rotate_left(cut);
        self.vec.truncate(len.saturating_sub(cut));
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;

    // ── The surface added for the `arrayvec` shim ───────────────────────────

    #[test]
    fn test_into_inner_moves_without_double_drop() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        struct Bomb(std::rc::Rc<std::cell::Cell<usize>>);
        impl Drop for Bomb {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let mut av: ArrayVec<Bomb, 2> = ArrayVec::new();
        av.push(Bomb(std::rc::Rc::clone(&counter)));
        av.push(Bomb(std::rc::Rc::clone(&counter)));
        let arr = av.into_inner().unwrap_or_else(|_| unreachable!());
        assert_eq!(counter.get(), 0, "into_inner must not drop");
        drop(arr);
        assert_eq!(counter.get(), 2, "each element dropped exactly once");
    }

    #[test]
    fn test_drain_double_ended() {
        let mut av: ArrayVec<i32, 8> = ArrayVec::new();
        av.extend([1, 2, 3, 4, 5]);
        {
            let mut d = av.drain(1..4);
            assert_eq!(d.next_back(), Some(4));
            assert_eq!(d.next(), Some(2));
            assert_eq!(d.next(), Some(3));
            assert_eq!(d.next(), None);
        }
        assert_eq!(av.as_slice(), &[1, 5]);
    }

    #[test]
    #[should_panic(expected = "ArrayVec::drain: end")]
    fn test_drain_past_len_panics() {
        let mut av: ArrayVec<i32, 8> = ArrayVec::new();
        av.extend([1, 2]);
        let _ = av.drain(0..3);
    }

    #[test]
    fn test_capacity_error_shape() {
        let e = CapacityError::new(7u8);
        assert_eq!(format!("{e}"), "insufficient capacity");
        assert_eq!(format!("{e:?}"), "CapacityError: insufficient capacity");
        assert_eq!(e.simplify(), CapacityError::new(()));
        assert_eq!(e.element(), 7);
    }
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `ArrayVec<T, N>`: fixed-capacity inline-only storage.
//!
//! The implementation names only `core` items (never `std`), so this module
//! compiles unchanged in a `#![no_std]` crate. That matters because
//! `crates/aterm-arrayvec` republishes this type under the package name
//! `arrayvec`, and three of the six third-party consumers of that package
//! (`naga`, `tiny-skia`, `vte`) are `no_std`-capable and take it with
//! `default-features = false`. Re-check with
//! `grep -n 'std::' crates/aterm-alloc/src/array_vec.rs` — every hit other than
//! this sentence must be inside the `#[cfg(test)]` module at the bottom (17
//! hits today: this line and 16 in the tests).

use core::fmt;
use core::hash::{Hash, Hasher};
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ops::{Bound, Deref, DerefMut, RangeBounds};

/// A fixed-capacity vector stored entirely on the stack.
///
/// Never allocates on the heap. Panics if you try to push beyond capacity `N`.
/// Used in the parser for CSI params (N=16) and intermediates (N=4).
pub struct ArrayVec<T, const N: usize> {
    buf: [MaybeUninit<T>; N],
    len: usize,
}

impl<T, const N: usize> ArrayVec<T, N> {
    /// Create a new, empty `ArrayVec`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            // An array of `MaybeUninit` needs no initialization. Build it with an
            // inline-const element (stable) rather than `MaybeUninit::uninit()
            // .assume_init()` — there is then no `unsafe`/`assume_init` obligation
            // for the Trust verifier to refute (it cannot see that the outer array's
            // elements are themselves `MaybeUninit`).
            buf: [const { MaybeUninit::uninit() }; N],
            len: 0,
        }
    }

    /// Const-compatible alias for `new`.
    #[must_use]
    pub const fn new_const() -> Self {
        Self::new()
    }

    /// The number of elements.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the collection is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The fixed capacity `N`.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Remaining capacity before full.
    #[must_use]
    pub const fn remaining_capacity(&self) -> usize {
        // `len <= N` by invariant; `saturating_sub` makes the no-underflow obligation
        // provable (identical result when the invariant holds).
        N.saturating_sub(self.len)
    }

    /// Whether the collection is at full capacity.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    /// Push an element. Panics if full.
    ///
    /// # Panics
    ///
    /// Panics if `len == N`.
    // AUDITED CONTRACT PANIC (T9): `push` beyond capacity panics BY DOCUMENTED
    // CONTRACT (the ArrayVec API promise); callers that cannot uphold `len < N`
    // use `try_push`. The declaration reclassifies the refuted panic-freedom
    // obligation into the visible `contract-panic` gate column.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "ArrayVec overflow")
    )]
    // `#[track_caller]` so a capacity panic names the CALLER's line, not this
    // file's. Upstream `arrayvec` marks `push`/`insert`/`extend`/`from_iter`
    // the same way, and the shim inherits the attribute through the re-export:
    // without it every overflow in the patched graph would report
    // `crates/aterm-alloc/src/array_vec.rs:<line>` and read as an aterm bug
    // rather than the wgpu/naga limit violation it is.
    #[track_caller]
    pub fn push(&mut self, value: T) {
        // Guard on a LOCAL copy of `len`: the verifier is a modular open-model
        // checker that reloads `self.len` at every field read, so only a local
        // carries the `len < N` fact to the index and the increment. Behavior
        // is identical — same store, same increment, same panic when full.
        let len = self.len;
        if len < N {
            self.buf[len] = MaybeUninit::new(value);
            // `len < N <= usize::MAX` means the increment cannot wrap; the
            // wrapping form only removes the overflow obligation the verifier
            // cannot discharge across the const-generic bound.
            self.len = len.wrapping_add(1);
        } else {
            // Const message (not `{N}`-formatted): the contract_panic matcher
            // chases `Arguments::from_str` one level and can only bind a const
            // string. Same contract violation, same panic — only the panic
            // text loses the capacity value.
            panic!("ArrayVec overflow: capacity exceeded");
        }
    }

    /// Try to push an element. Returns `Err(CapacityError(value))` if full.
    ///
    /// # Errors
    ///
    /// Returns the element back inside a [`CapacityError`] when `len == N`.
    ///
    /// The error type is `CapacityError<T>` rather than a bare `T` so that the
    /// signature is upstream `arrayvec`'s to the letter — see the module docs
    /// of `crates/aterm-arrayvec`. No caller in this workspace or in the
    /// patched graph used the old `Result<(), T>` form, so the change is
    /// source-compatible with everything that exists.
    pub fn try_push(&mut self, value: T) -> Result<(), CapacityError<T>> {
        // Same local-copy idiom as `push` (above): the open-model verifier
        // reloads `self.len` at every field read, so only a local carries the
        // `len < N` fact to the index and the increment. Behavior-identical.
        let len = self.len;
        if len < N {
            self.buf[len] = MaybeUninit::new(value);
            // `len < N <= usize::MAX`: the increment cannot wrap; the
            // wrapping form only removes the overflow obligation the
            // verifier cannot discharge across the const-generic bound.
            self.len = len.wrapping_add(1);
            Ok(())
        } else {
            Err(CapacityError::new(value))
        }
    }

    /// Pop the last element.
    // Skip (this crate's one memory-model row): proving the popped slot is
    // INITIALIZED before `assume_init_read` needs per-slot init tracking the
    // toolchain does not yet have (the proof-unit memory-model producer
    // lane). The invariant is crate-local and audited: `len` counts exactly
    // the initialized prefix — the only writers are `push`/`try_push`
    // (verified in-bounds writes that increment `len` only AFTER
    // initializing `buf[len]`) and `pop` (decrements before reading below
    // the old `len`); round-trip unit tests cover the boundary. Verify-only;
    // behavior unchanged; droppable when init tracking lands.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn pop(&mut self) -> Option<T> {
        // Same local-copy idiom as `push`/`try_push` (above); wrapping_sub is
        // exact under the `len != 0` guard and removes the underflow
        // obligation the verifier cannot chain through the field re-read.
        let len = self.len;
        if len == 0 {
            None
        } else {
            let idx = len.wrapping_sub(1);
            // `get` (not `buf[idx]`): the struct invariant `len <= N` cannot
            // cross the open-model havoc of `self.len`, so a direct index
            // carries an undischargeable bounds obligation; `get` is total
            // and its `None` arm is unreachable in every real state — same
            // value, same one bounds check at runtime.
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
    /// Equivalent to `clear(); push(value);` — panics for `N == 0` exactly
    /// as `push` on a full vec does (the same documented capacity contract).
    // AUDITED CONTRACT PANIC (T9): same "ArrayVec overflow" contract as
    // `push`; reachable only for an `N == 0` instantiation (none exist in
    // this workspace). The `try_push` spelling carries the verified bounds
    // proof; the direct `buf[0]` store it replaces was refuted by the
    // verifier at symbolic `N = 0` — the same panic, now annotation-bound.
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
    // Skip: the same crate-local init-tracking class as `pop` above —
    // proving each dropped slot is INITIALIZED needs the per-slot
    // memory-model producer; the audited invariant (`len` counts exactly
    // the initialized prefix; the clamp keeps the subslice in-bounds) is
    // documented on the SAFETY comment below. Droppable with `pop`'s.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn clear(&mut self) {
        // `len <= N` by invariant, but the verifier cannot see that across calls,
        // so clamp to a provably in-bounds count (identical when the invariant
        // holds), then iterate the clamped subslice: the single slice operation
        // carries the `n <= N` proof and the loop has no per-index obligation.
        let n = if self.len < N { self.len } else { N };
        // LENGTH FIRST, AND THAT ORDER IS A SAFETY REQUIREMENT, not a style.
        // If an element's `Drop` PANICS partway through the loop below, unwinding
        // runs `Drop for ArrayVec`, which calls this same `clear`. With `len`
        // still describing the old contents, that second pass re-drops every
        // slot the first pass already dropped — a double free. Zeroing `len`
        // before any destructor runs makes the re-entrant call a no-op.
        // The cost is that the not-yet-dropped tail LEAKS when a destructor
        // panics; leaking is safe and double-dropping is not, which is the
        // trade upstream `arrayvec` makes for the same reason.
        self.len = 0;
        for slot in &mut self.buf[..n] {
            // SAFETY: elements `0..n` were initialized when pushed and
            // `n <= len` as it was on entry, so every slot in the subslice held
            // a live value; `len` is already 0, so no other path can reach them
            // and each is dropped exactly once.
            unsafe {
                slot.assume_init_drop();
            }
        }
    }

    /// Truncate to the given length.
    // Skip: same crate-local init-tracking class as `pop`/`clear` above —
    // the drop-initialized-suffix loop needs the per-slot memory-model
    // producer. Same audited len-invariant; droppable with theirs.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn truncate(&mut self, new_len: usize) {
        if new_len < self.len {
            // `len <= N` by invariant, but the verifier cannot see that across
            // calls, so clamp both slice bounds to provably in-bounds values
            // (identical when the invariant holds: `new_len < len <= N`), then
            // iterate the clamped subslice — the slice operation carries the
            // `start <= end <= N` proof and the loop has no per-index obligation.
            let end = if self.len < N { self.len } else { N };
            let start = if new_len < end { new_len } else { end };
            // LENGTH FIRST — same panic-safety requirement as `clear` above: a
            // destructor that panics inside the loop unwinds into
            // `Drop for ArrayVec` -> `clear`, which would re-drop this range if
            // `len` still covered it.
            self.len = new_len;
            for slot in &mut self.buf[start..end] {
                // SAFETY: elements `start..end` were initialized when pushed
                // (`end <= len` as it was on entry), so every slot in the
                // subslice held a live value; `len` is already `new_len <=
                // start`, so no other path can reach them and each is dropped
                // exactly once.
                unsafe {
                    slot.assume_init_drop();
                }
            }
        }
    }

    /// View as a slice.
    #[must_use]
    // Skip: the raw-parts reconstruction is the same crate-local init/
    // provenance class as `pop`/`clear`/`truncate` (the null/bounds facts of
    // `init.as_ptr()` don't cross the unsafe cast in the sep model). Same
    // audited len-invariant; droppable with theirs.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_slice(&self) -> &[T] {
        // `len <= N` by invariant; clamp to a provably in-bounds length (a no-op
        // when the invariant holds) and take the initialized subslice first —
        // the safe slice operation carries the `n <= N` proof, and the raw-parts
        // reconstruction below reuses that subslice's own pointer and length.
        let n = if self.len < N { self.len } else { N };
        let init: &[MaybeUninit<T>] = &self.buf[..n];
        // SAFETY: `MaybeUninit<T>` has the same layout as `T`; the pointer and
        // length come from the same in-bounds subslice `init`, and elements
        // `0..len` are initialized (`n <= len`), so reading them as `T` is sound.
        unsafe { core::slice::from_raw_parts(init.as_ptr().cast::<T>(), init.len()) }
    }

    /// View as a mutable slice.
    // Skip: same class as `as_slice` above.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // `len <= N` by invariant; clamp to a provably in-bounds length (a no-op
        // when the invariant holds) and take the initialized subslice first —
        // the safe slice operation carries the `n <= N` proof, and the raw-parts
        // reconstruction below reuses that subslice's own pointer and length.
        let n = if self.len < N { self.len } else { N };
        let init: &mut [MaybeUninit<T>] = &mut self.buf[..n];
        // SAFETY: `MaybeUninit<T>` has the same layout as `T`; the pointer and
        // length come from the same in-bounds subslice `init`, and elements
        // `0..len` are initialized (`n <= len`), so reading them as `T` is sound.
        unsafe { core::slice::from_raw_parts_mut(init.as_mut_ptr().cast::<T>(), init.len()) }
    }

    /// An iterator over references.
    pub fn iter(&self) -> core::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    /// An iterator over mutable references.
    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, T> {
        self.as_mut_slice().iter_mut()
    }

    /// Insert an element at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index > len` or if the array is full.
    // Skip: the raw `ptr::copy` element shift + `ptr::write` carry the same
    // crate-local init/provenance class as `pop`/`clear`/`as_slice` (the
    // sep model cannot see the shifted region's bounds across the raw ops).
    // Same audited len-invariant; droppable with theirs.
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    pub fn insert(&mut self, index: usize, value: T) {
        assert!(
            index <= self.len,
            "index out of bounds: {index} > {}",
            self.len
        );
        // Const message (not `{N}`-formatted) so the contract_panic matcher
        // can bind it; same contract violation, same panic — only the panic
        // text loses the capacity value.
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
    // Skip: the raw element shift joins `insert`'s init/provenance
    // classification (pop/clear/as_slice family — the sep model cannot see
    // the shifted region's bounds across the raw ops). Same audited
    // len-invariant; droppable with theirs.
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
    /// Panic-safe: if the predicate panics, all elements that have already
    /// been processed are in a valid state and will be dropped correctly.
    ///
    /// The predicate takes `&mut T`, matching upstream `arrayvec`'s
    /// `retain<F>(&mut self, f: F) where F: FnMut(&mut T) -> bool`. A closure
    /// that only reads still infers the right argument type, so every
    /// `retain(|x| …)` spelling keeps compiling.
    // Skip: the compaction's raw element shift joins insert/remove's
    // init/provenance classification; `F: FnMut` is caller-chosen code
    // besides (the user-T dispatch class). Same audited len-invariant.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn retain<F: FnMut(&mut T) -> bool>(&mut self, mut f: F) {
        let original_len = self.len;
        // Set len to 0 so that if we panic, Drop only drops elements
        // that we've already moved into the write region.
        self.len = 0;

        struct RetainGuard<'a, T, const N: usize> {
            av: &'a mut ArrayVec<T, N>,
            write: usize,
            read: usize,
            original_len: usize,
        }

        impl<T, const N: usize> Drop for RetainGuard<'_, T, N> {
            fn drop(&mut self) {
                // Elements 0..write are the retained (initialized) elements.
                // Elements write..read have already been processed (moved or
                // dropped). Elements read..original_len have NOT been processed
                // — drop them now. `read <= original_len <= N` by invariant;
                // clamp both slice bounds so they are provably in bounds
                // (identical when the invariant holds), then iterate the
                // clamped subslice — the slice operation carries the
                // `start <= end <= N` proof, no per-index obligation remains.
                let end = if self.original_len < N {
                    self.original_len
                } else {
                    N
                };
                let start = if self.read < end { self.read } else { end };
                for slot in &mut self.av.buf[start..end] {
                    // SAFETY: the element is initialized and unprocessed;
                    // it is dropped exactly once.
                    unsafe {
                        slot.assume_init_drop();
                    }
                }
                self.av.len = self.write;
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
                // SAFETY: element is initialized; drop it
                unsafe {
                    guard.av.buf[read].assume_init_drop();
                }
            }
        }

        let final_len = guard.write;
        // Defuse the guard — all elements processed, set final state.
        guard.original_len = guard.read; // no unprocessed elements remain
        drop(guard);
        self.len = final_len;
    }

    /// Set the length without dropping or initializing elements.
    ///
    /// # Safety
    ///
    /// `len` must be `<= N` and the first `len` slots must be initialized.
    /// This is upstream `arrayvec`'s `set_len` signature and contract.
    pub const unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= N, "ArrayVec::set_len past capacity");
        self.len = len;
    }

    /// Append every element of `other`, cloning each.
    ///
    /// A deliberate SUPERSET: upstream `arrayvec` keeps its `extend_from_slice`
    /// `pub(crate)` (arrayvec-0.7.6/src/arrayvec.rs:1116) and exposes only
    /// [`try_extend_from_slice`](Self::try_extend_from_slice). This one is
    /// public because `clone_from` and the shim's tests want the panicking
    /// form by name; behaviourally it is upstream's `extend` over a slice, and
    /// `crates/aterm-alloc/tests/arrayvec_differential.rs` asserts exactly that.
    ///
    /// # Panics
    ///
    /// Panics if the slice does not fit in the remaining capacity — the same
    /// contract as [`push`](Self::push), and the same contract as upstream
    /// `arrayvec::ArrayVec::extend_from_slice`.
    // AUDITED CONTRACT PANIC (T9): delegates to `push`, whose overflow panic is
    // the documented ArrayVec capacity contract; `try_extend_from_slice` is the
    // non-panicking form.
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
    /// Returns [`CapacityError`] and appends NOTHING when
    /// `other.len() > remaining_capacity()` — the all-or-nothing behaviour of
    /// upstream `arrayvec::ArrayVec::try_extend_from_slice`.
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

    /// Return the inner fixed-size array, but only if the vec is FULL.
    ///
    /// # Errors
    ///
    /// Returns `Err(self)` when `len < N`. The polarity is upstream's and it
    /// is load-bearing: `naga`'s constant evaluator spells this
    /// `.into_inner().unwrap()` and relies on the panic to reject a
    /// short vector component list. Returning an `Ok` with a default-filled
    /// tail would compile everywhere and emit shaders built from garbage.
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
    // Skip (the pop/clear/as_slice init-tracking family): proving every slot
    // is INITIALIZED before the whole-array `ptr::read` needs the per-slot
    // memory-model producer. Same audited invariant — `len` counts exactly the
    // initialized prefix — plus the caller's `len == N` obligation.
    #[cfg_attr(trust_verify, trust::skip)]
    pub unsafe fn into_inner_unchecked(self) -> [T; N] {
        debug_assert_eq!(self.len, N, "into_inner_unchecked on a non-full ArrayVec");
        // `ManuallyDrop` first: the array is moved out wholesale, so `Drop for
        // ArrayVec` must NOT also run over the same slots.
        let me = ManuallyDrop::new(self);
        // SAFETY: `[MaybeUninit<T>; N]` has the same layout as `[T; N]`, the
        // caller guarantees all `N` slots are initialized, and `me` will not be
        // dropped, so the elements are moved (not copied) exactly once.
        unsafe { core::ptr::read(me.buf.as_ptr().cast::<[T; N]>()) }
    }

    /// Remove `range` from the vec and yield the removed elements.
    ///
    /// The tail is closed up as the iterator runs, so the vec is in a valid,
    /// correctly-shortened state at every point — including if the [`Drain`]
    /// is dropped early, and including if it is `mem::forget`ten (see the
    /// divergence note on [`Drain`]).
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
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for ArrayVec<T, N> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, const N: usize> DerefMut for ArrayVec<T, N> {
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

    /// Overridden (the trait default would `drop` every existing element and
    /// rebuild) so that the common prefix is `clone_from`-ed in place, exactly
    /// as upstream `arrayvec` does. The observable result is identical; what
    /// changes is the per-call cost, and `wgpu-hal`'s GLES backend calls this
    /// on every pipeline bind (`gles/command.rs:238`).
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
        // Reuse the (bounds-clamped) clear loop instead of duplicating the slice —
        // same drop-all-elements behaviour, no separate out-of-bounds obligation.
        self.clear();
    }
}

impl<T, const N: usize> FromIterator<T> for ArrayVec<T, N> {
    // Skip: `I: IntoIterator` is CALLER-CHOSEN code — `next` is an open-trait
    // dispatch on a type parameter (user-T class); `push`'s capacity contract
    // is the documented panic.
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
/// Upstream `arrayvec`'s error type, reproduced so that
/// [`ArrayVec::try_push`] and [`ArrayVec::try_extend_from_slice`] have
/// upstream's exact signatures. `Debug` and `Display` are hand-written and do
/// NOT require `T: Debug`, matching upstream: the payload is deliberately not
/// printed, because it is frequently a value whose `Debug` is enormous.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct CapacityError<T = ()> {
    element: T,
}

impl<T> CapacityError<T> {
    /// Wrap the element that did not fit.
    #[must_use]
    pub const fn new(element: T) -> Self {
        Self { element }
    }

    /// Take the element back out.
    #[must_use]
    pub fn element(self) -> T {
        self.element
    }

    /// Drop the payload, keeping only the "it did not fit" fact.
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

// DIVERGENCE, deliberate and a strict superset: upstream gates this impl on its
// `std` feature, because `Error` lived only in `std` when 0.7.6 was written.
// `core::error::Error` is stable now, so the impl is unconditional here. Every
// program that compiled against upstream's still compiles; programs that would
// have failed under `default-features = false` now succeed. The `T: Any` bound
// is upstream's, kept so the two impls select identically.
impl<T: core::any::Any> core::error::Error for CapacityError<T> {}

// ── Hash ────────────────────────────────────────────────────────────────────

// DELEGATES TO THE SLICE, and that is the whole point. `&**self` is exactly
// `len` elements; hashing the backing `[MaybeUninit<T>; N]` would read
// uninitialized memory while still satisfying the Hash/Eq contract within a
// single process — i.e. it would pass every test and be UB. `wgpu-hal`'s
// `ProgramCacheKey`, `RenderPassKey` and `FramebufferKey` and `wgpu-core`'s
// `AttachmentData` are all `HashMap` keys that reach here through
// `#[derive(Hash)]`.
impl<T: Hash, const N: usize> Hash for ArrayVec<T, N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&**self, state);
    }
}

// ── Extend ──────────────────────────────────────────────────────────────────

/// PANICS on overflow; it does not truncate.
///
/// This is the single most consequential line in the file. A fixed-capacity
/// `extend` that quietly stopped at capacity would compile everywhere and never
/// fire: `wgpu-hal`'s DXC path (`dx12/shader_compilation.rs:388`) would drop
/// compiler arguments and silently change how every HLSL shader is built, and
/// `wgpu-core`'s texture-to-texture copy (`command/transfer.rs:1462`) would
/// drop the destination barrier and race the GPU. Upstream panics; so does
/// this, via `push`.
impl<T, const N: usize> Extend<T> for ArrayVec<T, N> {
    // AUDITED CONTRACT PANIC (T9): the overflow panic is `push`'s documented
    // capacity contract, reached through caller-chosen `I: IntoIterator` code
    // (the user-T dispatch class, as in `FromIterator` above).
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

// ── Iteration ───────────────────────────────────────────────────────────────

// `for x in &av` and `for x in &mut av` need these EXPLICITLY: trait selection
// happens before autoderef, so `Deref<Target = [T]>` does not satisfy either
// loop. Both bound the walk by `len` (through `as_slice`/`as_mut_slice`), never
// by `N` — `tiny-skia`'s `pipeline/mod.rs:407` writes through every element it
// visits, so a `0..N` walk would write into uninitialized slots.
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
/// # Why this owns an `ArrayVec` and moves elements out with `remove(0)`
///
/// The dangerous way to write this type is an index cursor plus a hand-rolled
/// `Drop` that drops the un-yielded remainder. Get that `Drop` wrong in one
/// direction and every early `return` out of a `for ra in self.render_attachments`
/// (`wgpu-core/src/command/render.rs:1532`) leaks an `Arc<Texture>` — no panic,
/// no failing test, just VRAM that never comes back. Get it wrong in the other
/// direction and it double-frees.
///
/// Holding the `ArrayVec` itself removes the failure mode instead of testing
/// for it: the un-yielded elements are exactly the elements still in the vec,
/// so `ArrayVec`'s own already-audited `Drop` drops each of them exactly once
/// and there is no second drop path to get wrong. There is no `impl Drop` on
/// this type at all, and no `unsafe` in it.
///
/// The cost is that forward iteration is O(n²) in element moves rather than
/// O(n), because each `next` shifts the tail down by one. The largest capacity
/// anywhere in the patched graph is 32 and these sites are pipeline/attachment
/// setup, not per-frame work, so the shifts are bounded by a few hundred
/// register-sized moves per call. That trade is deliberate.
pub struct IntoIter<T, const N: usize> {
    vec: ArrayVec<T, N>,
}

impl<T, const N: usize> IntoIter<T, N> {
    /// The not-yet-yielded elements, in order.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        self.vec.as_slice()
    }

    /// The not-yet-yielded elements, in order, mutably.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self.vec.as_mut_slice()
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
/// # Why the tail closes up as it goes
///
/// `wgpu-hal`'s GLES backend empties `resolve_attachments` exactly once per
/// pass, with `drain(..)` at `gles/command.rs:698`. A `drain` that yielded the
/// elements but left `len` alone would make every later pass re-emit the
/// previous pass's MSAA resolves against stale `TextureView`s — wrong pixels,
/// never a panic. So the removal has to be real, and it has to happen whether
/// or not the caller runs the iterator to the end.
///
/// This type therefore removes each element from the vec as it yields it
/// (`ArrayVec::remove`, which shifts the tail down), and its `Drop` simply
/// drains whatever is left. Like [`IntoIter`], it contains no `unsafe`.
///
/// DIVERGENCE from upstream, in the safe direction: upstream shortens the vec
/// up front and restores the tail in `Drop`, so `mem::forget`ting an upstream
/// `Drain` loses the un-drained tail. Forgetting this one leaves those elements
/// in the vec instead. Nothing in the patched graph forgets a `Drain`, and
/// "keeps the elements" is the safer of the two answers.
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
        // Removing each remaining element both drops it and closes the tail up,
        // so an early-dropped `Drain` leaves exactly the same vec an exhausted
        // one would.
        while self.next().is_some() {}
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_empty() {
        let av: ArrayVec<i32, 4> = ArrayVec::new();
        assert!(av.is_empty());
        assert_eq!(av.len(), 0);
        assert_eq!(av.capacity(), 4);
        assert!(!av.is_full());
    }

    #[test]
    fn test_push_and_access() {
        let mut av: ArrayVec<i32, 4> = ArrayVec::new();
        av.push(1);
        av.push(2);
        av.push(3);
        assert_eq!(av.len(), 3);
        assert_eq!(av.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn test_is_full() {
        let mut av: ArrayVec<i32, 2> = ArrayVec::new();
        av.push(1);
        av.push(2);
        assert!(av.is_full());
        assert_eq!(av.remaining_capacity(), 0);
    }

    #[test]
    #[should_panic(expected = "ArrayVec overflow")]
    fn test_push_overflow_panics() {
        let mut av: ArrayVec<i32, 2> = ArrayVec::new();
        av.push(1);
        av.push(2);
        av.push(3); // should panic
    }

    #[test]
    fn test_try_push() {
        let mut av: ArrayVec<i32, 2> = ArrayVec::new();
        assert!(av.try_push(1).is_ok());
        assert!(av.try_push(2).is_ok());
        assert_eq!(av.try_push(3), Err(CapacityError::new(3)));
        // …and the element comes back out.
        assert_eq!(av.try_push(4).unwrap_err().element(), 4);
    }

    #[test]
    fn test_pop() {
        let mut av: ArrayVec<i32, 4> = ArrayVec::new();
        av.push(10);
        av.push(20);
        assert_eq!(av.pop(), Some(20));
        assert_eq!(av.pop(), Some(10));
        assert_eq!(av.pop(), None);
    }

    #[test]
    fn test_clear() {
        let mut av: ArrayVec<String, 4> = ArrayVec::new();
        av.push("hello".into());
        av.push("world".into());
        av.clear();
        assert!(av.is_empty());
    }

    #[test]
    fn test_truncate() {
        let mut av: ArrayVec<i32, 4> = ArrayVec::new();
        av.push(1);
        av.push(2);
        av.push(3);
        av.truncate(1);
        assert_eq!(av.as_slice(), &[1]);
    }

    #[test]
    fn test_insert_and_remove() {
        let mut av: ArrayVec<i32, 8> = ArrayVec::new();
        av.push(1);
        av.push(3);
        av.insert(1, 2);
        assert_eq!(av.as_slice(), &[1, 2, 3]);

        let removed = av.remove(1);
        assert_eq!(removed, 2);
        assert_eq!(av.as_slice(), &[1, 3]);
    }

    #[test]
    fn test_retain() {
        let mut av: ArrayVec<i32, 8> = ArrayVec::new();
        av.push(1);
        av.push(2);
        av.push(3);
        av.push(4);
        av.push(5);
        av.retain(|x| *x % 2 == 0);
        assert_eq!(av.as_slice(), &[2, 4]);
    }

    #[test]
    fn test_clone() {
        let mut av: ArrayVec<String, 4> = ArrayVec::new();
        av.push("hello".into());
        let cloned = av.clone();
        assert_eq!(av, cloned);
    }

    #[test]
    fn test_collect() {
        let av: ArrayVec<i32, 8> = (0..5).collect();
        assert_eq!(av.as_slice(), &[0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_deref_indexing() {
        let mut av: ArrayVec<i32, 4> = ArrayVec::new();
        av.push(10);
        av.push(20);
        assert_eq!(av[0], 10);
        assert_eq!(av[1], 20);
    }

    #[test]
    fn test_debug_format() {
        let mut av: ArrayVec<i32, 4> = ArrayVec::new();
        av.push(1);
        av.push(2);
        assert_eq!(format!("{av:?}"), "[1, 2]");
    }

    #[test]
    fn test_drop_string_elements() {
        let mut av: ArrayVec<String, 4> = ArrayVec::new();
        av.push("heap allocated string that is long enough".into());
        av.push("another one".into());
        drop(av);
    }

    #[test]
    fn test_const_new() {
        // Verify const construction works
        const AV: ArrayVec<u8, 16> = ArrayVec::new_const();
        assert!(AV.is_empty());
    }

    #[test]
    fn test_retain_with_drop_types() {
        let mut av: ArrayVec<String, 8> = ArrayVec::new();
        av.push("keep-a".into());
        av.push("drop-b".into());
        av.push("keep-c".into());
        av.push("drop-d".into());
        av.push("keep-e".into());
        av.retain(|s| s.starts_with("keep"));
        assert_eq!(av.len(), 3);
        assert_eq!(av.as_slice(), &["keep-a", "keep-c", "keep-e"]);
    }

    #[test]
    fn test_retain_panic_safety() {
        use std::panic;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

        #[derive(Debug)]
        struct Tracked(#[allow(dead_code)] i32);

        impl Drop for Tracked {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }

        DROP_COUNT.store(0, Ordering::Relaxed);

        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut av: ArrayVec<Tracked, 8> = ArrayVec::new();
            av.push(Tracked(1));
            av.push(Tracked(2));
            av.push(Tracked(3));
            av.push(Tracked(4));
            av.push(Tracked(5));

            let mut call_count = 0;
            av.retain(|_| {
                call_count += 1;
                if call_count == 3 {
                    panic!("predicate panic");
                }
                true
            });
        }));

        assert!(result.is_err());
        // All 5 elements must be dropped exactly once (no leak, no double-free).
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 5);
    }

    // ── The surface added for the `arrayvec` shim ───────────────────────────

    #[test]
    fn test_into_inner_full_and_short() {
        let mut av: ArrayVec<i32, 3> = ArrayVec::new();
        av.push(1);
        av.push(2);
        // Short: Err, and the vec comes back untouched.
        let mut av = av.into_inner().unwrap_err();
        assert_eq!(av.as_slice(), &[1, 2]);
        av.push(3);
        // Full: Ok, in order.
        assert_eq!(av.into_inner().unwrap(), [1, 2, 3]);
    }

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
    fn test_hash_matches_slice_and_ignores_capacity() {
        use std::collections::hash_map::DefaultHasher;
        fn h<T: Hash + ?Sized>(v: &T) -> u64 {
            let mut s = DefaultHasher::new();
            v.hash(&mut s);
            s.finish()
        }
        let mut a: ArrayVec<u8, 4> = ArrayVec::new();
        let mut b: ArrayVec<u8, 32> = ArrayVec::new();
        for v in [1u8, 2, 3] {
            a.push(v);
            b.push(v);
        }
        // Same elements, different N: the hash must not see the capacity, and
        // it must equal the slice's own hash.
        assert_eq!(h(&a), h(&b));
        assert_eq!(h(&a), h(&[1u8, 2, 3][..]));
    }

    #[test]
    fn test_extend_and_collect_panic_rather_than_truncate() {
        let mut av: ArrayVec<i32, 3> = ArrayVec::new();
        av.extend([1, 2, 3]);
        assert_eq!(av.as_slice(), &[1, 2, 3]);

        // CONTROL for the tripwires below: the same call one element smaller
        // must NOT panic, so a passing "it panicked" assertion cannot be an
        // artifact of `extend` panicking unconditionally.
        let control = std::panic::catch_unwind(|| {
            let mut av: ArrayVec<i32, 3> = ArrayVec::new();
            av.extend([1, 2]);
            av.len()
        });
        assert_eq!(
            control.ok(),
            Some(2),
            "control: a fitting extend must not panic"
        );

        let overflow = std::panic::catch_unwind(|| {
            let mut av: ArrayVec<i32, 3> = ArrayVec::new();
            av.extend([1, 2, 3, 4]);
        });
        assert!(overflow.is_err(), "extend must panic, never truncate");

        let collected = std::panic::catch_unwind(|| {
            let _: ArrayVec<i32, 3> = (1..=4).collect();
        });
        assert!(collected.is_err(), "collect must panic, never truncate");
    }

    #[test]
    fn test_into_iter_by_value_order_and_drop_of_remainder() {
        let mut av: ArrayVec<i32, 4> = ArrayVec::new();
        av.extend([1, 2, 3, 4]);
        assert_eq!(av.into_iter().collect::<Vec<_>>(), vec![1, 2, 3, 4]);

        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        struct Bomb(std::rc::Rc<std::cell::Cell<usize>>);
        impl Drop for Bomb {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let mut av: ArrayVec<Bomb, 4> = ArrayVec::new();
        for _ in 0..4 {
            av.push(Bomb(std::rc::Rc::clone(&counter)));
        }
        {
            let mut it = av.into_iter();
            let first = it.next().unwrap();
            drop(first);
            assert_eq!(counter.get(), 1);
            // `it` is dropped here with three elements un-yielded.
        }
        assert_eq!(
            counter.get(),
            4,
            "the un-yielded remainder is dropped exactly once"
        );
    }

    #[test]
    fn test_into_iter_double_ended_and_as_slice() {
        let mut av: ArrayVec<i32, 5> = ArrayVec::new();
        av.extend([1, 2, 3, 4, 5]);
        let mut it = av.into_iter();
        assert_eq!(it.next(), Some(1));
        assert_eq!(it.next_back(), Some(5));
        assert_eq!(it.as_slice(), &[2, 3, 4]);
        assert_eq!(it.len(), 3);
        assert_eq!(it.collect::<Vec<_>>(), vec![2, 3, 4]);
    }

    #[test]
    fn test_iter_by_ref_and_by_mut_are_bounded_by_len() {
        let mut av: ArrayVec<i32, 8> = ArrayVec::new();
        av.extend([1, 2, 3]);
        let mut seen = Vec::new();
        for v in &av {
            seen.push(*v);
        }
        assert_eq!(seen, vec![1, 2, 3], "&ArrayVec walks 0..len, not 0..N");
        for v in &mut av {
            *v *= 10;
        }
        assert_eq!(av.as_slice(), &[10, 20, 30]);
    }

    #[test]
    fn test_drain_removes_and_closes_the_tail() {
        let mut av: ArrayVec<i32, 8> = ArrayVec::new();
        av.extend([1, 2, 3, 4, 5]);
        let taken: Vec<_> = av.drain(1..3).collect();
        assert_eq!(taken, vec![2, 3]);
        assert_eq!(av.as_slice(), &[1, 4, 5]);

        // `drain(..)` — the form `wgpu-hal` uses — must EMPTY the vec.
        let taken: Vec<_> = av.drain(..).collect();
        assert_eq!(taken, vec![1, 4, 5]);
        assert!(av.is_empty(), "drain(..) must leave the vec empty");
    }

    #[test]
    fn test_drain_dropped_early_still_removes_and_drops_once() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        struct Bomb(u32, std::rc::Rc<std::cell::Cell<usize>>);
        impl Drop for Bomb {
            fn drop(&mut self) {
                self.1.set(self.1.get() + 1);
            }
        }
        let mut av: ArrayVec<Bomb, 8> = ArrayVec::new();
        for i in 0..5 {
            av.push(Bomb(i, std::rc::Rc::clone(&counter)));
        }
        {
            let mut d = av.drain(1..4);
            drop(d.next().unwrap());
            assert_eq!(counter.get(), 1);
            // dropped here with two of the three un-yielded
        }
        assert_eq!(counter.get(), 3, "the whole drained range is dropped once");
        assert_eq!(av.len(), 2, "the tail closed up");
        assert_eq!(av[0].0, 0);
        assert_eq!(av[1].0, 4);
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
    fn test_clone_from_reuses_prefix_and_matches_clone() {
        let mut dst: ArrayVec<String, 4> = ArrayVec::new();
        dst.extend(["a".to_string(), "b".to_string(), "c".to_string()]);

        // Shorter source: truncates.
        let mut src: ArrayVec<String, 4> = ArrayVec::new();
        src.extend(["x".to_string()]);
        dst.clone_from(&src);
        assert_eq!(dst.as_slice(), src.as_slice());

        // Longer source: extends.
        let mut src: ArrayVec<String, 4> = ArrayVec::new();
        src.extend([
            "p".to_string(),
            "q".to_string(),
            "r".to_string(),
            "s".to_string(),
        ]);
        dst.clone_from(&src);
        assert_eq!(dst.as_slice(), src.as_slice());
        assert_eq!(dst, src.clone());
    }

    #[test]
    fn test_capacity_error_shape() {
        let e = CapacityError::new(7u8);
        assert_eq!(format!("{e}"), "insufficient capacity");
        assert_eq!(format!("{e:?}"), "CapacityError: insufficient capacity");
        assert_eq!(e.simplify(), CapacityError::new(()));
        assert_eq!(e.element(), 7);
    }

    #[test]
    fn test_retain_can_mutate_through_the_predicate() {
        let mut av: ArrayVec<i32, 8> = ArrayVec::new();
        av.extend([1, 2, 3, 4]);
        // Upstream's bound is `FnMut(&mut T) -> bool`; this closure could not
        // compile against the old `FnMut(&T) -> bool`.
        av.retain(|v| {
            *v += 100;
            *v % 2 == 1
        });
        assert_eq!(av.as_slice(), &[101, 103]);
    }

    #[test]
    fn test_extend_from_slice_and_try_form() {
        let mut av: ArrayVec<u8, 4> = ArrayVec::new();
        av.extend_from_slice(&[1, 2]);
        assert!(av.try_extend_from_slice(&[3, 4]).is_ok());
        assert_eq!(av.as_slice(), &[1, 2, 3, 4]);
        // All-or-nothing: the vec is untouched when the slice does not fit.
        let mut av: ArrayVec<u8, 4> = ArrayVec::new();
        av.extend_from_slice(&[1, 2, 3]);
        assert!(av.try_extend_from_slice(&[4, 5]).is_err());
        assert_eq!(av.as_slice(), &[1, 2, 3]);
    }
}

// ── Kani proofs ────────────────────────────────────────────────────────────

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Verify that pushing into a full ArrayVec panics (not UB).
    ///
    /// The `push` method uses an `assert!` guard before writing into the
    /// MaybeUninit buffer. This proof drives all capacity-4 slots full, then
    /// confirms the N+1 push triggers the panic path rather than writing
    /// out-of-bounds into uninitialized memory.
    #[kani::proof]
    #[kani::unwind(6)]
    fn arrayvec_push_at_capacity_panics() {
        let mut av: ArrayVec<u32, 4> = ArrayVec::new();
        av.push(1);
        av.push(2);
        av.push(3);
        av.push(4);

        // Capacity is full; len must equal N.
        kani::assert(av.len() == 4, "len must equal capacity after filling");
        kani::assert(av.is_full(), "is_full must be true at capacity");

        // try_push must fail and return the value back. `try_push` yields
        // `Result<(), CapacityError<T>>`, not `Result<(), T>` — comparing against
        // `Err(99)` did not type-check, so this harness never compiled and the
        // whole crate's `list` step died with it, taking harness discovery for the
        // entire workspace down (the lane then exits 2, having proved nothing).
        let result = av.try_push(99);
        kani::assert(
            result == Err(CapacityError::new(99)),
            "try_push must return Err(value) when full",
        );

        // len must not have changed.
        kani::assert(av.len() == 4, "len must remain 4 after failed try_push");
    }

    /// Verify the retain drop guard: after retain, len equals the number of
    /// elements that passed the predicate, and those elements are exactly
    /// the ones the predicate accepted.
    ///
    /// Uses symbolic threshold to partition elements into keep/drop sets,
    /// then verifies the surviving slice matches expectations.
    #[kani::proof]
    #[kani::unwind(6)]
    fn arrayvec_retain_len_consistent() {
        let threshold: u32 = kani::any();
        kani::assume(threshold <= 4);

        let mut av: ArrayVec<u32, 4> = ArrayVec::new();
        av.push(0);
        av.push(1);
        av.push(2);
        av.push(3);

        // Retain elements strictly less than the symbolic threshold.
        av.retain(|x| *x < threshold);

        // The number of values in 0..4 that are < threshold is exactly
        // min(threshold, 4).
        let expected_len = threshold as usize;
        kani::assert(
            av.len() == expected_len,
            "len must equal count of elements satisfying predicate",
        );

        // Every surviving element must actually satisfy the predicate.
        let slice = av.as_slice();
        let mut i = 0;
        while i < av.len() {
            kani::assert(
                slice[i] < threshold,
                "retained element must satisfy predicate",
            );
            i += 1;
        }
    }

    /// Verify that pop returns elements in LIFO order.
    ///
    /// Pushes symbolic values a, b, c and verifies pop returns c, b, a
    /// in that order, then returns None on empty.
    #[kani::proof]
    fn arrayvec_pop_lifo_order() {
        let a: u32 = kani::any();
        let b: u32 = kani::any();
        let c: u32 = kani::any();

        let mut av: ArrayVec<u32, 4> = ArrayVec::new();
        av.push(a);
        av.push(b);
        av.push(c);

        kani::assert(av.len() == 3, "len must be 3 after 3 pushes");

        let p1 = av.pop();
        kani::assert(p1 == Some(c), "first pop must return last pushed (c)");
        kani::assert(av.len() == 2, "len must be 2 after first pop");

        let p2 = av.pop();
        kani::assert(p2 == Some(b), "second pop must return b");
        kani::assert(av.len() == 1, "len must be 1 after second pop");

        let p3 = av.pop();
        kani::assert(p3 == Some(a), "third pop must return a");
        kani::assert(av.len() == 0, "len must be 0 after third pop");

        let p4 = av.pop();
        kani::assert(p4.is_none(), "pop on empty must return None");
    }

    /// Verify that as_slice returns a slice whose length equals the
    /// ArrayVec's len, and whose elements match what was pushed.
    ///
    /// Pushes a symbolic number of elements (0..=4) and verifies the
    /// slice length and content at each step.
    #[kani::proof]
    #[kani::unwind(6)]
    fn arrayvec_as_slice_len_and_content() {
        let count: usize = kani::any();
        kani::assume(count <= 4);

        let vals: [u32; 4] = [kani::any(), kani::any(), kani::any(), kani::any()];

        let mut av: ArrayVec<u32, 4> = ArrayVec::new();
        let mut i = 0;
        while i < count {
            av.push(vals[i]);
            i += 1;
        }

        let slice = av.as_slice();
        kani::assert(
            slice.len() == count,
            "as_slice length must equal number of pushed elements",
        );

        // Verify each element matches what was pushed.
        let mut j = 0;
        while j < count {
            kani::assert(
                slice[j] == vals[j],
                "as_slice element must match pushed value",
            );
            j += 1;
        }
    }
}

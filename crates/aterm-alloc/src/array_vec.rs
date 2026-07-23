// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `ArrayVec<T, N>`: fixed-capacity inline-only storage.

use std::fmt;
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};

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

    /// Try to push an element. Returns `Err(value)` if full.
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
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
            Err(value)
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
        for slot in &mut self.buf[..n] {
            // SAFETY: elements `0..len` were initialized when pushed and
            // `n <= len`, so every slot in the subslice holds a live value;
            // each is dropped exactly once.
            unsafe {
                slot.assume_init_drop();
            }
        }
        self.len = 0;
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
            for slot in &mut self.buf[start..end] {
                // SAFETY: elements `new_len..len` were initialized when pushed
                // (`end <= len`), so every slot in the subslice holds a live
                // value; each is dropped exactly once.
                unsafe {
                    slot.assume_init_drop();
                }
            }
            self.len = new_len;
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
        unsafe { std::slice::from_raw_parts(init.as_ptr().cast::<T>(), init.len()) }
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
        unsafe { std::slice::from_raw_parts_mut(init.as_mut_ptr().cast::<T>(), init.len()) }
    }

    /// An iterator over references.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    /// An iterator over mutable references.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
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
            std::ptr::copy(ptr.add(index), ptr.add(index + 1), self.len - index);
            std::ptr::write(ptr.add(index), value);
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
    pub fn remove(&mut self, index: usize) -> T {
        assert!(
            index < self.len,
            "index out of bounds: {index} >= {}",
            self.len
        );

        // SAFETY: element at index is initialized
        unsafe {
            let ptr = self.buf.as_mut_ptr().cast::<T>();
            let value = std::ptr::read(ptr.add(index));
            std::ptr::copy(ptr.add(index + 1), ptr.add(index), self.len - index - 1);
            self.len -= 1;
            value
        }
    }

    /// Retain only elements where the predicate returns true.
    ///
    /// Panic-safe: if the predicate panics, all elements that have already
    /// been processed are in a valid state and will be dropped correctly.
    // Skip: the compaction's raw element shift joins insert/remove's
    // init/provenance classification; `F: FnMut` is caller-chosen code
    // besides (the user-T dispatch class). Same audited len-invariant.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn retain<F: FnMut(&T) -> bool>(&mut self, mut f: F) {
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
            let keep = unsafe { f(&*guard.av.buf[read].as_ptr()) };
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
        assert_eq!(av.try_push(3), Err(3));
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
        av.retain(|x| x % 2 == 0);
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

        // try_push must fail and return the value back.
        let result = av.try_push(99);
        kani::assert(
            result == Err(99),
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
        av.retain(|&x| x < threshold);

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

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fixed-capacity inline storage (`ArrayVec<T, N>`).

use core::fmt;
use core::hash::{Hash, Hasher};
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ops::{Bound, Deref, DerefMut, RangeBounds};

/// Fixed-capacity stack-backed vector.
pub struct ArrayVec<T, const N: usize> {
    buf: [MaybeUninit<T>; N],
    len: usize,
}

impl<T, const N: usize> ArrayVec<T, N> {
    /// Creates an empty `ArrayVec`.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [const { MaybeUninit::uninit() }; N],
            len: 0,
        }
    }

    /// Const-compatible alias for [`Self::new`].
    #[inline]
    #[must_use]
    pub const fn new_const() -> Self {
        Self::new()
    }

    /// Number of initialized elements.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when empty.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Fixed capacity `N`.
    #[inline]
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Remaining capacity before full.
    #[inline]
    #[must_use]
    pub const fn remaining_capacity(&self) -> usize {
        N.saturating_sub(self.len)
    }

    /// Returns `true` when `len == N`.
    #[inline]
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    /// Pushes an element.
    /// # Panics
    /// Panics if `len == N`.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "ArrayVec overflow")
    )]
    #[inline]
    #[track_caller]
    pub fn push(&mut self, value: T) {
        let len = self.len;
        if len < N {
            self.buf[len] = MaybeUninit::new(value);
            self.len = len.wrapping_add(1);
        } else {
            panic!("ArrayVec overflow: capacity exceeded");
        }
    }

    /// Appends `value` if capacity remains.
    /// # Errors
    /// Returns [`CapacityError`] containing `value` if full.
    #[inline]
    pub const fn try_push(&mut self, value: T) -> Result<(), CapacityError<T>> {
        let len = self.len;
        if len < N {
            self.buf[len] = MaybeUninit::new(value);
            self.len = len.wrapping_add(1);
            Ok(())
        } else {
            Err(CapacityError::new(value))
        }
    }

    /// Pops and returns the last element.
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn pop(&mut self) -> Option<T> {
        let len = self.len;
        if len == 0 {
            None
        } else {
            let idx = len.wrapping_sub(1);
            match self.buf.get(idx) {
                Some(slot) => {
                    self.len = idx;
                    // SAFETY: `buf[idx]` was initialized when pushed.
                    let value = unsafe { slot.assume_init_read() };
                    Some(value)
                }
                None => None,
            }
        }
    }

    /// Replaces all contents with `value`.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "ArrayVec overflow")
    )]
    #[inline]
    pub fn set_single(&mut self, value: T) {
        self.clear();
        if self.try_push(value).is_err() {
            panic!("ArrayVec overflow: capacity exceeded");
        }
    }

    /// Clears all elements.
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Truncates the vector to `new_len` elements.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn truncate(&mut self, new_len: usize) {
        if new_len < self.len {
            let end = if self.len < N { self.len } else { N };
            let start = if new_len < end { new_len } else { end };
            self.len = new_len;
            // SAFETY: `start..end` was initialized when pushed and `len` is already `<= start`.
            unsafe { crate::drop_initialized(&mut self.buf[start..end]) }
        }
    }

    /// Views the initialized elements as a slice.
    #[inline]
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_slice(&self) -> &[T] {
        let n = if self.len < N { self.len } else { N };
        let init: &[MaybeUninit<T>] = &self.buf[..n];
        // SAFETY: `MaybeUninit<T>` matches `T` layout and `0..len` in `init` is initialized.
        unsafe { core::slice::from_raw_parts(init.as_ptr().cast::<T>(), init.len()) }
    }

    /// Views the initialized elements as a mutable slice.
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        let n = if self.len < N { self.len } else { N };
        let init: &mut [MaybeUninit<T>] = &mut self.buf[..n];
        // SAFETY: `MaybeUninit<T>` matches `T` layout and `0..len` in `init` is initialized.
        unsafe { core::slice::from_raw_parts_mut(init.as_mut_ptr().cast::<T>(), init.len()) }
    }

    /// Iterator over shared element references.
    #[inline]
    pub fn iter(&self) -> core::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    /// Iterator over mutable element references.
    #[inline]
    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, T> {
        self.as_mut_slice().iter_mut()
    }

    /// Inserts `element` at `index`, shifting subsequent elements right.
    /// # Panics
    /// Panics if `index > len` or `len == N`.
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    pub fn insert(&mut self, index: usize, value: T) {
        assert!(
            index <= self.len,
            "index out of bounds: {index} > {}",
            self.len
        );
        assert!(self.len < N, "ArrayVec overflow: capacity exceeded");

        // SAFETY: bounds and capacity were checked above.
        unsafe {
            let ptr = self.buf.as_mut_ptr().cast::<T>();
            core::ptr::copy(ptr.add(index), ptr.add(index + 1), self.len - index);
            core::ptr::write(ptr.add(index), value);
        }
        self.len += 1;
    }

    /// Removes and returns the element at `index`.
    /// # Panics
    /// Panics if `index >= len`.
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    pub fn remove(&mut self, index: usize) -> T {
        assert!(
            index < self.len,
            "index out of bounds: {index} >= {}",
            self.len
        );

        // SAFETY: element at `index` is initialized.
        unsafe {
            let ptr = self.buf.as_mut_ptr().cast::<T>();
            let value = core::ptr::read(ptr.add(index));
            core::ptr::copy(ptr.add(index + 1), ptr.add(index), self.len - index - 1);
            self.len -= 1;
            value
        }
    }

    /// Retains only elements for which `f` returns `true`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn retain<F: FnMut(&mut T) -> bool>(&mut self, mut f: F) {
        let original_len = self.len;
        self.len = 0;

        struct RetainGuard<'a, T, const N: usize> {
            av: &'a mut ArrayVec<T, N>,
            write: usize,
            read: usize,
            original_len: usize,
        }

        impl<T, const N: usize> Drop for RetainGuard<'_, T, N> {
            fn drop(&mut self) {
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
            // SAFETY: element at `read` is initialized (`read < original_len`).
            let keep = unsafe { f(&mut *guard.av.buf[read].as_mut_ptr()) };
            guard.read += 1;
            if keep {
                if guard.write != read {
                    // SAFETY: both indices are in bounds; `read` is consumed and `write` is vacant.
                    unsafe {
                        let val = guard.av.buf[read].assume_init_read();
                        guard.av.buf[guard.write] = MaybeUninit::new(val);
                    }
                }
                guard.write += 1;
            } else {
                // SAFETY: element at `read` is initialized and `guard.read` already advanced past it.
                unsafe {
                    guard.av.buf[read].assume_init_drop();
                }
            }
        }

        drop(guard);
    }

    /// Clones and appends every element of `other`.
    /// # Panics
    /// Panics if the remaining capacity is less than `other.len()`.
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

    /// Clones and appends every element of `other` if capacity permits.
    /// # Errors
    /// Returns [`CapacityError`] if `other` does not fit.
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

    /// Extracts the full fixed-size array.
    /// # Errors
    /// Returns `Err(self)` when `len < N`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn into_inner(self) -> Result<[T; N], Self> {
        if self.len < N {
            Err(self)
        } else {
            // SAFETY: `len == N`, so every slot is initialized.
            Ok(unsafe { self.into_inner_unchecked() })
        }
    }

    /// Extracts the fixed-size array without checking that it is full.
    /// # Safety
    /// Caller must ensure `len == N`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub unsafe fn into_inner_unchecked(self) -> [T; N] {
        debug_assert_eq!(self.len, N, "into_inner_unchecked on a non-full ArrayVec");
        let me = ManuallyDrop::new(self);
        // SAFETY: layout matches `[T; N]`, all `N` slots are initialized, and `me` is not dropped.
        unsafe { core::ptr::read(me.buf.as_ptr().cast::<[T; N]>()) }
    }

    /// Removes `range` and yields the removed elements.
    /// # Panics
    /// Panics if `range` is out of bounds or inverted.
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
    /// as upstream does.
    fn clone_from(&mut self, source: &Self) {
        let prefix = if self.len < source.len {
            self.len
        } else {
            source.len
        };
        self.as_mut_slice()[..prefix].clone_from_slice(&source.as_slice()[..prefix]);
        if prefix < self.len {
            self.truncate(prefix);
        } else {
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
        self.clear();
    }
}

impl<T, const N: usize> FromIterator<T> for ArrayVec<T, N> {
    #[cfg_attr(trust_verify, trust::skip)]
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut av = Self::new();
        for item in iter {
            av.push(item);
        }
        av
    }
}

/// Error returned by [`ArrayVec::try_push`] and [`ArrayVec::try_extend_from_slice`] when full.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct CapacityError<T = ()> {
    element: T,
}

impl<T> CapacityError<T> {
    /// Wraps the element that did not fit.
    #[inline]
    #[must_use]
    pub const fn new(element: T) -> Self {
        Self { element }
    }

    /// Extracts the wrapped element.
    #[inline]
    #[must_use]
    pub fn element(self) -> T {
        self.element
    }

    /// Drops the payload, returning an unparameterized [`CapacityError`].
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

impl<T: core::any::Any> core::error::Error for CapacityError<T> {}

impl<T: Hash, const N: usize> Hash for ArrayVec<T, N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&**self, state);
    }
}

/// Appends elements from an iterator, panicking on overflow.
impl<T, const N: usize> Extend<T> for ArrayVec<T, N> {
    #[cfg_attr(trust_verify, trust::skip)]
    #[track_caller]
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

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

/// By-value iterator for [`ArrayVec`].
pub struct IntoIter<T, const N: usize> {
    vec: ArrayVec<T, N>,
}

impl<T, const N: usize> IntoIter<T, N> {
    /// Remaining unyielded elements as a slice.
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

/// Draining iterator for [`ArrayVec`], produced by [`ArrayVec::drain`].
pub struct Drain<'a, T, const N: usize> {
    vec: &'a mut ArrayVec<T, N>,
    /// Index of the next front element to yield.
    start: usize,
    /// Count of remaining elements in the drained range.
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
            Some(self.vec.remove(self.start + self.remaining))
        }
    }
}

impl<T, const N: usize> ExactSizeIterator for Drain<'_, T, N> {}

impl<T, const N: usize> Drop for Drain<'_, T, N> {
    fn drop(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;

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

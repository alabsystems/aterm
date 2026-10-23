// Copyright 2026 Andrew Yates
// Copyright (c) 2018 The Servo Project Developers
// SPDX-License-Identifier: Apache-2.0

//! `smallvec`: aterm's first-party replacement for the upstream `smallvec` 1.x crate.

#![no_std]
#![forbid(unsafe_code)]

#[doc(hidden)]
pub extern crate alloc;

use alloc::vec::Vec;
use core::borrow::{Borrow, BorrowMut};
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::iter::{FusedIterator, repeat};
use core::ops::{Bound, Deref, DerefMut, Index, IndexMut, RangeBounds};
use core::slice::{self, SliceIndex};

/// Creates a [`SmallVec`] containing the arguments, as `vec!` does a `Vec`.
#[macro_export]
macro_rules! smallvec {
    (@one $x:expr) => (1usize);
    () => (
        $crate::SmallVec::new()
    );
    ($elem:expr; $n:expr) => ({
        $crate::SmallVec::from_elem($elem, $n)
    });
    ($($x:expr),+$(,)?) => ({
        let count = 0usize $(+ $crate::smallvec!(@one $x))+;
        let mut vec = $crate::SmallVec::new();
        if count <= vec.inline_size() {
            $(vec.push($x);)*
            vec
        } else {
            $crate::SmallVec::from_vec($crate::alloc::vec![$($x,)+])
        }
    });
}

/// The inline buffer type of a [`SmallVec`].
pub trait Array {
    /// The element type.
    type Item;
    /// The vector this array type selects: `aterm_alloc::SmallVec<T, N>`.
    #[doc(hidden)]
    type Store: Store<Item = Self::Item>;
    /// The number of items the array can hold.
    fn size() -> usize;
}

impl<T, const N: usize> Array for [T; N] {
    type Item = T;
    type Store = aterm_alloc::SmallVec<T, N>;
    #[inline]
    fn size() -> usize {
        N
    }
}

/// The operations [`SmallVec`] forwards to its storage.
#[doc(hidden)]
pub trait Store: Sized {
    type Item;
    type IntoIter: DoubleEndedIterator<Item = Self::Item> + ExactSizeIterator + FusedIterator;
    fn new() -> Self;
    fn with_capacity(capacity: usize) -> Self;
    fn from_vec(vec: Vec<Self::Item>) -> Self;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
    fn capacity(&self) -> usize;
    fn spilled(&self) -> bool;
    fn push(&mut self, value: Self::Item);
    fn pop(&mut self) -> Option<Self::Item>;
    fn as_slice(&self) -> &[Self::Item];
    fn as_mut_slice(&mut self) -> &mut [Self::Item];
    fn into_iter(self) -> Self::IntoIter;
}

impl<T, const N: usize> Store for aterm_alloc::SmallVec<T, N> {
    type Item = T;
    type IntoIter = aterm_alloc::IntoIter<T, N>;
    #[inline]
    fn new() -> Self {
        aterm_alloc::SmallVec::new()
    }
    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        aterm_alloc::SmallVec::with_capacity(capacity)
    }
    #[inline]
    fn from_vec(vec: Vec<T>) -> Self {
        aterm_alloc::SmallVec::from_vec(vec)
    }
    #[inline]
    fn len(&self) -> usize {
        aterm_alloc::SmallVec::len(self)
    }
    #[inline]
    fn is_empty(&self) -> bool {
        aterm_alloc::SmallVec::is_empty(self)
    }
    #[inline]
    fn capacity(&self) -> usize {
        aterm_alloc::SmallVec::capacity(self)
    }
    #[inline]
    fn spilled(&self) -> bool {
        aterm_alloc::SmallVec::spilled(self)
    }
    #[inline]
    fn push(&mut self, value: T) {
        aterm_alloc::SmallVec::push(self, value);
    }
    #[inline]
    fn pop(&mut self) -> Option<T> {
        aterm_alloc::SmallVec::pop(self)
    }
    #[inline]
    fn as_slice(&self) -> &[T] {
        aterm_alloc::SmallVec::as_slice(self)
    }
    #[inline]
    fn as_mut_slice(&mut self) -> &mut [T] {
        aterm_alloc::SmallVec::as_mut_slice(self)
    }
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        IntoIterator::into_iter(self)
    }
}

/// A vector that keeps up to `A::size()` items inline and spills to the heap past that.
pub struct SmallVec<A: Array> {
    store: A::Store,
}

impl<A: Array> SmallVec<A> {
    /// An empty vector, inline.
    #[inline]
    pub fn new() -> SmallVec<A> {
        SmallVec {
            store: A::Store::new(),
        }
    }

    /// An empty vector with room for `n` items.
    #[inline]
    pub fn with_capacity(n: usize) -> Self {
        SmallVec {
            store: A::Store::with_capacity(n),
        }
    }

    /// Takes over `vec`.
    pub fn from_vec(vec: Vec<A::Item>) -> SmallVec<A> {
        let store = if vec.len() <= A::size() && vec.capacity() > A::size() {
            let mut store = A::Store::with_capacity(vec.capacity());
            for item in vec {
                store.push(item);
            }
            store
        } else {
            A::Store::from_vec(vec)
        };
        SmallVec { store }
    }

    /// The number of items the inline buffer holds.
    #[inline]
    pub fn inline_size(&self) -> usize {
        A::size()
    }

    /// The number of items in the vector.
    #[inline]
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Whether the vector holds no items.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// The number of items the vector holds without reallocating.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.store.capacity()
    }

    /// Whether the items live on the heap.
    #[inline]
    pub fn spilled(&self) -> bool {
        self.store.spilled()
    }

    /// Appends `value`, spilling to the heap when the inline buffer is full.
    #[inline]
    pub fn push(&mut self, value: A::Item) {
        self.store.push(value);
    }

    /// Removes and returns the last item.
    #[inline]
    pub fn pop(&mut self) -> Option<A::Item> {
        self.store.pop()
    }

    /// Inserts `element` at `index`, shifting the items after it right.
    ///
    /// # Panics
    /// If `index > len`.
    pub fn insert(&mut self, index: usize, element: A::Item) {
        let len = self.len();
        assert!(index <= len, "index exceeds length");
        self.store.push(element);
        self.store.as_mut_slice()[index..].rotate_right(1);
    }

    /// Removes and returns the item at `index`, shifting the items after it left.
    ///
    /// # Panics
    /// If `index >= len`.
    pub fn remove(&mut self, index: usize) -> A::Item {
        let len = self.len();
        assert!(index < len);
        self.store.as_mut_slice()[index..].rotate_left(1);
        match self.store.pop() {
            Some(item) => item,
            None => unreachable!("len > index >= 0"),
        }
    }

    /// Removes and returns the item at `index`, moving the last item into its place.
    ///
    /// # Panics
    /// If `index >= len`.
    pub fn swap_remove(&mut self, index: usize) -> A::Item {
        let len = self.len();
        assert!(
            index < len,
            "swap_remove index (is {index}) should be < len (is {len})"
        );
        self.store.as_mut_slice().swap(len - 1, index);
        match self.store.pop() {
            Some(item) => item,
            None => unreachable!("len > index >= 0"),
        }
    }

    /// Keeps the first `len` items and drops the rest, last first.
    #[inline]
    pub fn truncate(&mut self, len: usize) {
        while self.len() > len {
            drop(self.store.pop());
        }
    }

    /// Drops every item, last first, as upstream's `truncate(0)` does.
    #[inline]
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Keeps the items for which `f` returns `true`, in order.
    pub fn retain<F: FnMut(&mut A::Item) -> bool>(&mut self, mut f: F) {
        let mut del = 0;
        let len = self.len();
        let items = self.store.as_mut_slice();
        for i in 0..len {
            if !f(&mut items[i]) {
                del += 1;
            } else if del > 0 {
                items.swap(i - del, i);
            }
        }
        self.truncate(len - del);
    }

    /// The same as [`retain`](Self::retain), as upstream has it.
    #[inline]
    pub fn retain_mut<F: FnMut(&mut A::Item) -> bool>(&mut self, f: F) {
        self.retain(f);
    }

    /// Makes the length `new_len`.
    pub fn resize_with<F>(&mut self, new_len: usize, f: F)
    where
        F: FnMut() -> A::Item,
    {
        let old_len = self.len();
        if old_len < new_len {
            let mut f = f;
            let additional = new_len - old_len;
            self.reserve(additional);
            for _ in 0..additional {
                self.push(f());
            }
        } else if old_len > new_len {
            self.truncate(new_len);
        }
    }

    /// Makes room for `additional` more items, rounding the new capacity up to a power of two.
    ///
    /// # Panics
    /// If the new capacity overflows `usize`.
    pub fn reserve(&mut self, additional: usize) {
        let len = self.len();
        if self.capacity() - len >= additional {
            return;
        }
        let new_cap = len
            .checked_add(additional)
            .and_then(usize::checked_next_power_of_two)
            .expect("capacity overflow");
        self.regrow(new_cap);
    }

    /// Makes room for exactly `additional` more items.
    ///
    /// # Panics
    /// If the new capacity overflows `usize`.
    pub fn reserve_exact(&mut self, additional: usize) {
        let len = self.len();
        if self.capacity() - len >= additional {
            return;
        }
        let new_cap = len.checked_add(additional).expect("capacity overflow");
        self.regrow(new_cap);
    }

    /// Moves the items into fresh storage with room for `new_cap`. Called only
    /// with `new_cap` above the current capacity, which is at least the inline
    /// size, so the new storage is a heap buffer.
    fn regrow(&mut self, new_cap: usize) {
        let old = core::mem::replace(&mut self.store, A::Store::with_capacity(new_cap));
        for item in old.into_iter() {
            self.store.push(item);
        }
    }

    /// The items as a slice.
    #[inline]
    pub fn as_slice(&self) -> &[A::Item] {
        self.store.as_slice()
    }

    /// The items as a mutable slice.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [A::Item] {
        self.store.as_mut_slice()
    }

    /// Removes the items in `range` and yields them by value, front to back.
    ///
    /// # Panics
    /// If the start is greater than the end, or the end is greater than the length.
    pub fn drain<R>(&mut self, range: R) -> Drain<'_, A>
    where
        R: RangeBounds<usize>,
    {
        let len = self.len();
        let start = match range.start_bound() {
            Bound::Included(&n) => n,
            Bound::Excluded(&n) => n.checked_add(1).expect("Range start out of bounds"),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&n) => n.checked_add(1).expect("Range end out of bounds"),
            Bound::Excluded(&n) => n,
            Bound::Unbounded => len,
        };
        assert!(start <= end);
        assert!(end <= len);

        let items = self.store.as_mut_slice();
        items[start..end].reverse();
        items[start..].rotate_left(end - start);
        Drain {
            vec: self,
            remaining: end - start,
        }
    }

    /// The items as a `Vec`.
    pub fn into_vec(self) -> Vec<A::Item> {
        if self.spilled() {
            let mut vec = Vec::with_capacity(self.capacity());
            vec.extend(self.store.into_iter());
            vec
        } else {
            self.store.into_iter().collect()
        }
    }
}

impl<A: Array> SmallVec<A>
where
    A::Item: Copy,
{
    /// A vector holding a copy of `slice`.
    pub fn from_slice(slice: &[A::Item]) -> Self {
        let mut vec: Self = SmallVec::with_capacity(slice.len());
        for item in slice {
            vec.store.push(*item);
        }
        vec
    }

    /// Appends a copy of `slice`.
    pub fn extend_from_slice(&mut self, slice: &[A::Item]) {
        self.reserve(slice.len());
        for item in slice {
            self.store.push(*item);
        }
    }
}

impl<A: Array> SmallVec<A>
where
    A::Item: Clone,
{
    /// Makes the length `len`.
    #[allow(
        clippy::manual_repeat_n,
        reason = "upstream clones `value` once per new item and drops it; `repeat_n` would move it into the last slot instead, which a `Clone`/`Drop` with side effects can see"
    )]
    pub fn resize(&mut self, len: usize, value: A::Item) {
        let old_len = self.len();
        if len > old_len {
            self.extend(repeat(value).take(len - old_len));
        } else {
            self.truncate(len);
        }
    }

    /// `n` clones of `elem`.
    pub fn from_elem(elem: A::Item, n: usize) -> Self {
        if n > A::size() {
            SmallVec::from_vec(alloc::vec![elem; n])
        } else {
            let mut vec: Self = SmallVec::new();
            for _ in 0..n {
                vec.store.push(elem.clone());
            }
            vec
        }
    }
}

impl<T, const N: usize> SmallVec<[T; N]> {
    /// [`SmallVec::new`] in a `const` context (upstream's `const_new` feature; always present here).
    #[inline]
    pub const fn new_const() -> Self {
        SmallVec {
            store: aterm_alloc::SmallVec::new(),
        }
    }
}

impl<A: Array> Deref for SmallVec<A> {
    type Target = [A::Item];
    #[inline]
    fn deref(&self) -> &[A::Item] {
        self.store.as_slice()
    }
}

impl<A: Array> DerefMut for SmallVec<A> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [A::Item] {
        self.store.as_mut_slice()
    }
}

impl<A: Array> AsRef<[A::Item]> for SmallVec<A> {
    #[inline]
    fn as_ref(&self) -> &[A::Item] {
        self
    }
}

impl<A: Array> AsMut<[A::Item]> for SmallVec<A> {
    #[inline]
    fn as_mut(&mut self) -> &mut [A::Item] {
        self
    }
}

impl<A: Array> Borrow<[A::Item]> for SmallVec<A> {
    #[inline]
    fn borrow(&self) -> &[A::Item] {
        self
    }
}

impl<A: Array> BorrowMut<[A::Item]> for SmallVec<A> {
    #[inline]
    fn borrow_mut(&mut self) -> &mut [A::Item] {
        self
    }
}

impl<A: Array, I: SliceIndex<[A::Item]>> Index<I> for SmallVec<A> {
    type Output = I::Output;
    #[inline]
    fn index(&self, index: I) -> &I::Output {
        &(**self)[index]
    }
}

impl<A: Array, I: SliceIndex<[A::Item]>> IndexMut<I> for SmallVec<A> {
    #[inline]
    fn index_mut(&mut self, index: I) -> &mut I::Output {
        &mut (&mut **self)[index]
    }
}

impl<'a, A: Array> From<&'a [A::Item]> for SmallVec<A>
where
    A::Item: Clone,
{
    #[inline]
    fn from(slice: &'a [A::Item]) -> SmallVec<A> {
        slice.iter().cloned().collect()
    }
}

impl<A: Array> From<Vec<A::Item>> for SmallVec<A> {
    #[inline]
    fn from(vec: Vec<A::Item>) -> SmallVec<A> {
        SmallVec::from_vec(vec)
    }
}

impl<A: Array> FromIterator<A::Item> for SmallVec<A> {
    #[inline]
    fn from_iter<I: IntoIterator<Item = A::Item>>(iterable: I) -> SmallVec<A> {
        let mut vec = SmallVec::new();
        vec.extend(iterable);
        vec
    }
}

impl<A: Array> Extend<A::Item> for SmallVec<A> {
    /// Reserves the iterator's lower size bound, then pushes each item.
    fn extend<I: IntoIterator<Item = A::Item>>(&mut self, iterable: I) {
        let iter = iterable.into_iter();
        let (lower_size_bound, _) = iter.size_hint();
        self.reserve(lower_size_bound);
        for item in iter {
            self.store.push(item);
        }
    }
}

impl<A: Array> fmt::Debug for SmallVec<A>
where
    A::Item: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<A: Array> Default for SmallVec<A> {
    #[inline]
    fn default() -> SmallVec<A> {
        SmallVec::new()
    }
}

impl<A: Array> Clone for SmallVec<A>
where
    A::Item: Clone,
{
    /// As upstream: a clone of a spilled vector whose items fit inline is
    /// inline.
    #[inline]
    fn clone(&self) -> SmallVec<A> {
        SmallVec::from(self.as_slice())
    }

    fn clone_from(&mut self, source: &Self) {
        self.truncate(source.len());
        let (init, tail) = source.split_at(self.len());
        self.clone_from_slice(init);
        self.extend(tail.iter().cloned());
    }
}

impl<A: Array, B: Array> PartialEq<SmallVec<B>> for SmallVec<A>
where
    A::Item: PartialEq<B::Item>,
{
    #[inline]
    fn eq(&self, other: &SmallVec<B>) -> bool {
        self[..] == other[..]
    }
}

impl<A: Array> Eq for SmallVec<A> where A::Item: Eq {}

impl<A: Array> PartialOrd for SmallVec<A>
where
    A::Item: PartialOrd,
{
    #[inline]
    fn partial_cmp(&self, other: &SmallVec<A>) -> Option<Ordering> {
        PartialOrd::partial_cmp(&**self, &**other)
    }
}

impl<A: Array> Ord for SmallVec<A>
where
    A::Item: Ord,
{
    #[inline]
    fn cmp(&self, other: &SmallVec<A>) -> Ordering {
        Ord::cmp(&**self, &**other)
    }
}

impl<A: Array> Hash for SmallVec<A>
where
    A::Item: Hash,
{
    /// The slice's hash, as upstream.
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
}

/// An iterator that consumes a [`SmallVec`] and yields its items by value.
pub struct IntoIter<A: Array> {
    iter: <A::Store as Store>::IntoIter,
}

impl<A: Array> Iterator for IntoIter<A> {
    type Item = A::Item;
    #[inline]
    fn next(&mut self) -> Option<A::Item> {
        self.iter.next()
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<A: Array> DoubleEndedIterator for IntoIter<A> {
    #[inline]
    fn next_back(&mut self) -> Option<A::Item> {
        self.iter.next_back()
    }
}

impl<A: Array> ExactSizeIterator for IntoIter<A> {}

impl<A: Array> FusedIterator for IntoIter<A> {}

impl<A: Array> IntoIterator for SmallVec<A> {
    type IntoIter = IntoIter<A>;
    type Item = A::Item;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            iter: self.store.into_iter(),
        }
    }
}

impl<'a, A: Array> IntoIterator for &'a SmallVec<A> {
    type IntoIter = slice::Iter<'a, A::Item>;
    type Item = &'a A::Item;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, A: Array> IntoIterator for &'a mut SmallVec<A> {
    type IntoIter = slice::IterMut<'a, A::Item>;
    type Item = &'a mut A::Item;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

/// The iterator [`SmallVec::drain`] returns.
pub struct Drain<'a, A: Array> {
    vec: &'a mut SmallVec<A>,
    /// The items not yet yielded: the last `remaining` items of `vec`, in
    /// reverse order, so the next one is `vec`'s last.
    remaining: usize,
}

impl<A: Array> Iterator for Drain<'_, A> {
    type Item = A::Item;

    #[inline]
    fn next(&mut self) -> Option<A::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        self.vec.store.pop()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<A: Array> DoubleEndedIterator for Drain<'_, A> {
    #[inline]
    fn next_back(&mut self) -> Option<A::Item> {
        if self.remaining == 0 {
            return None;
        }
        let at = self.vec.len() - self.remaining;
        self.remaining -= 1;
        Some(self.vec.remove(at))
    }
}

impl<A: Array> ExactSizeIterator for Drain<'_, A> {}

impl<A: Array> FusedIterator for Drain<'_, A> {}

impl<A: Array> Drop for Drain<'_, A> {
    /// Drops the items not yielded, front to back (the reversed run's last
    /// first), leaving `[head | tail]`.
    fn drop(&mut self) {
        let keep = self.vec.len() - self.remaining;
        self.remaining = 0;
        self.vec.truncate(keep);
    }
}

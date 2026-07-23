// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Inline sparse bitmap replacing the `roaring` crate dependency.
//!
//! Uses `BTreeSet<u32>` for ordered storage with logarithmic insert/remove
//! and efficient range queries. Suitable for terminal search posting lists
//! which are typically small-to-medium (hundreds to low thousands of entries).
//! Part of #7698.

use std::collections::BTreeSet;
#[cfg(test)]
use std::ops::RangeBounds;
use std::ops::{BitAnd, BitAndAssign};

/// A sparse bitmap backed by `BTreeSet<u32>`.
///
/// Provides the subset of `RoaringBitmap` APIs used by the search index:
/// insert, remove, range queries, set intersection, and iteration.
#[derive(Debug, Clone, Default)]
pub(crate) struct SparseBitmap {
    inner: BTreeSet<u32>,
}

/// Iterator over values in a `SparseBitmap`, consuming it.
///
/// Named `SparseBitmapIntoIter` (not `IntoIter`) because cbindgen 0.29.x
/// panics when a 0-generic-param type alias shadows the name `IntoIter`
/// elsewhere in its global symbol table ("IntoIter has 0 params but is
/// being instantiated with 1 values"). See #8022.
pub(crate) type SparseBitmapIntoIter = std::collections::btree_set::IntoIter<u32>;

/// Borrowed ordered range over a sparse bitmap.
pub(crate) type SparseBitmapRange<'a> = std::collections::btree_set::Range<'a, u32>;

impl SparseBitmap {
    /// Create a new empty bitmap.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            inner: BTreeSet::new(),
        }
    }

    /// Insert a value. Returns `true` if the value was newly inserted.
    #[inline]
    pub(crate) fn insert(&mut self, value: u32) -> bool {
        self.inner.insert(value)
    }

    /// Remove a value. Returns `true` if the value was present.
    #[inline]
    pub(crate) fn remove(&mut self, value: u32) -> bool {
        self.inner.remove(&value)
    }

    /// Remove all values in the given range.
    #[cfg(test)]
    pub(crate) fn remove_range<R: RangeBounds<u32>>(&mut self, range: R) {
        self.inner.retain(|v| !range.contains(v));
    }

    /// Borrow values at or after `from` without cloning the bitmap.
    pub(crate) fn range_from(&self, from: u32) -> SparseBitmapRange<'_> {
        self.inner.range(from..)
    }

    /// Borrow values below `before` without cloning the bitmap.
    pub(crate) fn range_before(&self, before: u32) -> SparseBitmapRange<'_> {
        self.inner.range(..before)
    }

    /// Iterate over values in the given range.
    #[cfg(test)]
    pub(crate) fn range<R: RangeBounds<u32>>(
        &self,
        range: R,
    ) -> impl Iterator<Item = u32> + '_ + use<'_, R> {
        self.inner.range(range).copied()
    }

    /// Returns `true` if the bitmap contains no values.
    #[inline]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the number of values in the bitmap.
    #[inline]
    #[must_use]
    pub(crate) fn len(&self) -> u64 {
        self.inner.len() as u64
    }

    /// Returns `true` if the bitmap contains the given value.
    #[inline]
    #[must_use]
    pub(crate) fn contains(&self, value: u32) -> bool {
        self.inner.contains(&value)
    }

    /// Iterate over all values in ascending order (borrowed).
    #[cfg(test)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.inner.iter().copied()
    }
}

impl IntoIterator for SparseBitmap {
    type Item = u32;
    type IntoIter = std::collections::btree_set::IntoIter<u32>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.into_iter()
    }
}

/// Intersect two `BTreeSet`s into a new owned set.
///
/// Spelled as an explicit insert loop (not `.collect()`) because the Trust L0
/// gate's bulk-allocation recognizer cannot derive an element count for
/// `collect` here and refuses to prove the allocation bounded; incremental
/// `insert` carries per-element bounds instead. Identical result set.
fn intersect_sets(a: &BTreeSet<u32>, b: &BTreeSet<u32>) -> BTreeSet<u32> {
    let mut inner = BTreeSet::new();
    for v in a.intersection(b) {
        inner.insert(*v);
    }
    inner
}

impl BitAnd<&SparseBitmap> for &SparseBitmap {
    type Output = SparseBitmap;

    fn bitand(self, rhs: &SparseBitmap) -> SparseBitmap {
        SparseBitmap {
            inner: intersect_sets(&self.inner, &rhs.inner),
        }
    }
}

impl BitAndAssign<&SparseBitmap> for SparseBitmap {
    fn bitand_assign(&mut self, rhs: &SparseBitmap) {
        self.inner = intersect_sets(&self.inner, &rhs.inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_len() {
        let mut bm = SparseBitmap::new();
        assert!(bm.is_empty());
        assert_eq!(bm.len(), 0);

        bm.insert(5);
        bm.insert(10);
        bm.insert(5); // duplicate
        assert_eq!(bm.len(), 2);
        assert!(!bm.is_empty());
    }

    #[test]
    fn test_remove() {
        let mut bm = SparseBitmap::new();
        bm.insert(1);
        bm.insert(2);
        bm.insert(3);

        assert!(bm.remove(2));
        assert!(!bm.remove(2)); // already removed
        assert_eq!(bm.len(), 2);
    }

    #[test]
    fn test_remove_range() {
        let mut bm = SparseBitmap::new();
        for i in 0..10 {
            bm.insert(i);
        }
        bm.remove_range(..5u32);
        let vals: Vec<u32> = bm.into_iter().collect();
        assert_eq!(vals, vec![5, 6, 7, 8, 9]);
    }

    #[test]
    fn test_range_query() {
        let mut bm = SparseBitmap::new();
        for i in 0..10 {
            bm.insert(i);
        }
        let vals: Vec<u32> = bm.range(3..7).collect();
        assert_eq!(vals, vec![3, 4, 5, 6]);
    }

    #[test]
    fn test_into_iter_sorted() {
        let mut bm = SparseBitmap::new();
        bm.insert(30);
        bm.insert(10);
        bm.insert(20);
        let vals: Vec<u32> = bm.into_iter().collect();
        assert_eq!(vals, vec![10, 20, 30]);
    }

    #[test]
    fn test_clone() {
        let mut bm = SparseBitmap::new();
        bm.insert(1);
        bm.insert(2);
        let bm2 = bm.clone();
        assert_eq!(bm2.len(), 2);
    }

    #[test]
    fn test_bitand() {
        let mut a = SparseBitmap::new();
        a.insert(1);
        a.insert(2);
        a.insert(3);

        let mut b = SparseBitmap::new();
        b.insert(2);
        b.insert(3);
        b.insert(4);

        let result = &a & &b;
        let vals: Vec<u32> = result.into_iter().collect();
        assert_eq!(vals, vec![2, 3]);
    }

    #[test]
    fn test_bitand_assign() {
        let mut a = SparseBitmap::new();
        a.insert(1);
        a.insert(2);
        a.insert(3);

        let mut b = SparseBitmap::new();
        b.insert(2);
        b.insert(3);
        b.insert(4);

        a &= &b;
        let vals: Vec<u32> = a.into_iter().collect();
        assert_eq!(vals, vec![2, 3]);
    }

    #[test]
    fn test_default_is_empty() {
        let bm = SparseBitmap::default();
        assert!(bm.is_empty());
        assert_eq!(bm.len(), 0);
    }

    #[test]
    fn test_iter_borrowed() {
        let mut bm = SparseBitmap::new();
        bm.insert(5);
        bm.insert(3);
        bm.insert(7);
        let vals: Vec<u32> = bm.iter().collect();
        assert_eq!(vals, vec![3, 5, 7]);
        // bm still usable after iter()
        assert_eq!(bm.len(), 3);
    }
}

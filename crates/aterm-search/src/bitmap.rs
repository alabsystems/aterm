// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Inline sparse bitmap replacing the `roaring` crate dependency.
//!
//! Backed by a DELTA + VARINT posting container: the first doc id verbatim, then
//! LEB128 varint-encoded ascending gaps for the rest. Terminal appends arrive in
//! row order, so the hot insert is a `push_back` of one varint gap (`O(1)`
//! amortized); out-of-order inserts (re-index of an existing row) decode, splice,
//! and re-encode.
//!
//! The predecessor was the "sortedvec" container — a plain ascending
//! `Vec<u32>`. That swap (docs/measured/search-posting-containers.md, Wave-4A
//! milestone 3) already removed the `BTreeSet<u32>` overhead; the residual
//! whole-index cost was still dominated in part by 4 bytes per posting. Gap +
//! varint cuts that: a run of consecutive rows (the common case for a trigram
//! that rides a template) encodes to one byte per posting instead of four, and a
//! single-row posting list needs zero delta bytes at all. Candidate sets are
//! byte-identical to the sortedvec container — every consumer decodes to the
//! exact same ascending, deduplicated `Vec<u32>` the old container held (the E4
//! differential oracle pins this). Part of #7698.
//!
//! Random access (binary-searched `contains`, `range_*`) and set intersection
//! decode to an owned `Vec<u32>` first: a varint stream is not seekable. The
//! query paths decode each involved posting list ONCE per query (a bounded,
//! transient per-query working set) and run the identical slice logic the
//! sortedvec container used — so decode cost is paid once, not per membership
//! probe. The stored container never retains a decoded copy, so the retained
//! heap reflects the compressed form.

#[cfg(test)]
use std::ops::RangeBounds;
use std::cmp::Ordering;
use std::ops::{BitAnd, BitAndAssign};

/// A sparse bitmap backed by delta + varint-encoded ascending doc ids.
///
/// Stores the smallest value (`first`) verbatim and the ascending gaps to every
/// later value as LEB128 varints in `deltas`. `last` and `count` are cached so
/// the append fast path and `len`/`is_empty` stay `O(1)` without decoding.
///
/// Provides the subset of `RoaringBitmap` APIs the search index uses: insert,
/// remove, range queries, set intersection, and iteration. Values are kept
/// ascending and duplicate-free by every mutator; membership, range, and
/// intersection decode to an owned `Vec<u32>` and run the same slice logic the
/// former ascending-array container did.
#[derive(Debug, Clone, Default)]
pub(crate) struct SparseBitmap {
    /// Varint-encoded ascending gaps for every value after `first`. Empty when
    /// the bitmap holds 0 or 1 values.
    deltas: Vec<u8>,
    /// The smallest value (meaningless when `count == 0`).
    first: u32,
    /// The largest value (the append-fast-path pivot; meaningless when empty).
    last: u32,
    /// Number of values held. `u32` because doc ids are bounded by the
    /// scrollback cap, so the count is too.
    count: u32,
}

/// Iterator over values in a `SparseBitmap`, consuming it. Ascending order.
///
/// Named `SparseBitmapIntoIter` (not `IntoIter`) because cbindgen 0.29.x
/// panics when a 0-generic-param type alias shadows the name `IntoIter`
/// elsewhere in its global symbol table ("IntoIter has 0 params but is
/// being instantiated with 1 values"). See #8022.
pub(crate) type SparseBitmapIntoIter = std::vec::IntoIter<u32>;

/// Owned ordered range over a sparse bitmap. Materialized from a transient
/// decode, so it is double-ended (reverse navigation consumes it back-to-front)
/// without pinning a borrow on the compressed container.
///
/// Test-only: the query paths decode each posting list once (via [`to_vec`]) and
/// range-bound the decoded slice directly, so no shipping caller borrows a
/// range off the compressed container. Retained to exercise the range logic.
///
/// [`to_vec`]: SparseBitmap::to_vec
#[cfg(test)]
pub(crate) type SparseBitmapRange = std::vec::IntoIter<u32>;

/// Append `value` to `buf` as an unsigned LEB128 varint (7 data bits per byte,
/// high bit continues). Push-only, so the Trust L0 gate carries no slice-bounds
/// obligation.
fn push_varint(buf: &mut Vec<u8>, mut value: u32) {
    while value >= 0x80 {
        buf.push((value as u8) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

impl SparseBitmap {
    /// Create a new empty bitmap.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Decode the compressed container into a fresh ascending `Vec<u32>`.
    ///
    /// The single decode point every random-access / intersection path funnels
    /// through. Spelled over a byte ITERATOR (`next`, not indexing) so the Trust
    /// L0 gate carries no slice-bounds obligation, and with `wrapping_add` so no
    /// overflow obligation — gaps sum back to the original ascending ids by
    /// construction.
    pub(crate) fn to_vec(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.count as usize);
        if self.count == 0 {
            return out;
        }
        let mut cur = self.first;
        out.push(cur);
        let mut bytes = self.deltas.iter().copied();
        'values: loop {
            let mut gap = 0u32;
            let mut shift = 0u32;
            loop {
                let Some(byte) = bytes.next() else {
                    break 'values;
                };
                gap |= u32::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    break;
                }
                shift = shift.saturating_add(7);
            }
            cur = cur.wrapping_add(gap);
            out.push(cur);
        }
        out
    }

    /// Re-encode from an ascending, deduplicated slice (the canonical form every
    /// decode-modify mutator writes back). Gaps use `wrapping_sub`: the slice is
    /// strictly ascending, so each `v - prev` is exact and non-negative.
    fn rebuild_from_sorted(&mut self, sorted: &[u32]) {
        self.deltas.clear();
        match sorted.split_first() {
            None => {
                self.first = 0;
                self.last = 0;
                self.count = 0;
            }
            Some((&first, rest)) => {
                self.first = first;
                let mut prev = first;
                for &v in rest {
                    push_varint(&mut self.deltas, v.wrapping_sub(prev));
                    prev = v;
                }
                self.last = prev;
                self.count = sorted.len() as u32;
            }
        }
    }

    /// Build a compressed bitmap from an ascending, deduplicated slice.
    fn from_sorted(sorted: &[u32]) -> Self {
        let mut bm = Self::default();
        bm.rebuild_from_sorted(sorted);
        bm
    }

    /// Insert a value. Returns `true` if the value was newly inserted.
    ///
    /// Fast path for the live-append shape: a value strictly greater than the
    /// current maximum encodes to one appended varint gap (`O(1)` amortized).
    /// Any other value decodes, binary-searches for its ordered position, and
    /// re-encodes; an already-present value is a no-op returning `false`.
    #[inline]
    pub(crate) fn insert(&mut self, value: u32) -> bool {
        if self.count == 0 {
            self.first = value;
            self.last = value;
            self.count = 1;
            return true;
        }
        if value > self.last {
            push_varint(&mut self.deltas, value.wrapping_sub(self.last));
            self.last = value;
            self.count = self.count.saturating_add(1);
            return true;
        }
        if value == self.last {
            return false;
        }
        let mut values = self.to_vec();
        match values.binary_search(&value) {
            Ok(_) => false,
            Err(pos) => {
                values.insert(pos, value);
                self.rebuild_from_sorted(&values);
                true
            }
        }
    }

    /// Remove a value. Returns `true` if the value was present.
    ///
    /// Decode-modify-encode: re-index (the only non-eviction caller) removes
    /// recently-appended rows, and bulk prefix eviction uses [`drop_below`]
    /// instead of one-at-a-time front removals.
    ///
    /// [`drop_below`]: Self::drop_below
    #[inline]
    pub(crate) fn remove(&mut self, value: u32) -> bool {
        if self.count == 0 || value < self.first || value > self.last {
            return false;
        }
        let mut values = self.to_vec();
        match values.binary_search(&value) {
            Ok(pos) => {
                values.remove(pos);
                self.rebuild_from_sorted(&values);
                true
            }
            Err(_) => false,
        }
    }

    /// Drop every value strictly below `watermark` in a single front trim.
    ///
    /// Prefix eviction (the cache cap and history-retain paths) removes ALL
    /// rows below a watermark at once. Produces the identical remaining list a
    /// one-at-a-time front removal would.
    pub(crate) fn drop_below(&mut self, watermark: u32) {
        if self.count == 0 || self.first >= watermark {
            return;
        }
        let values = self.to_vec();
        let cut = values.partition_point(|&v| v < watermark);
        self.rebuild_from_sorted(values.get(cut..).unwrap_or(&[]));
    }

    /// Remove all values in the given range.
    #[cfg(test)]
    pub(crate) fn remove_range<R: RangeBounds<u32>>(&mut self, range: R) {
        let mut values = self.to_vec();
        values.retain(|v| !range.contains(v));
        self.rebuild_from_sorted(&values);
    }

    /// Values at or after `from`, as an owned ascending iterator.
    #[cfg(test)]
    pub(crate) fn range_from(&self, from: u32) -> SparseBitmapRange {
        let mut values = self.to_vec();
        let start = values.partition_point(|&v| v < from);
        values.drain(..start);
        values.into_iter()
    }

    /// Values below `before`, as an owned ascending iterator.
    #[cfg(test)]
    pub(crate) fn range_before(&self, before: u32) -> SparseBitmapRange {
        let mut values = self.to_vec();
        let end = values.partition_point(|&v| v < before);
        values.truncate(end);
        values.into_iter()
    }

    /// Iterate over values in the given range.
    #[cfg(test)]
    pub(crate) fn range<R: RangeBounds<u32>>(&self, range: R) -> impl Iterator<Item = u32> {
        use std::ops::Bound;
        let values = self.to_vec();
        let start = match range.start_bound() {
            Bound::Included(&v) => values.partition_point(|&x| x < v),
            Bound::Excluded(&v) => values.partition_point(|&x| x <= v),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&v) => values.partition_point(|&x| x <= v),
            Bound::Excluded(&v) => values.partition_point(|&x| x < v),
            Bound::Unbounded => values.len(),
        };
        let mut values = values;
        values.truncate(end.max(start));
        values.drain(..start);
        values.into_iter()
    }

    /// Returns `true` if the bitmap contains no values.
    #[inline]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the number of values in the bitmap.
    #[inline]
    #[must_use]
    pub(crate) fn len(&self) -> u64 {
        u64::from(self.count)
    }

    /// Returns `true` if the bitmap contains the given value.
    ///
    /// Test-only: the filtered navigation path decodes each posting list once
    /// and binary-searches the decoded slice, so no shipping caller probes
    /// membership on the compressed container one value at a time.
    #[cfg(test)]
    #[inline]
    #[must_use]
    pub(crate) fn contains(&self, value: u32) -> bool {
        if self.count == 0 || value < self.first || value > self.last {
            return false;
        }
        self.to_vec().binary_search(&value).is_ok()
    }

    /// Iterate over all values in ascending order.
    #[cfg(test)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = u32> {
        self.to_vec().into_iter()
    }
}

impl IntoIterator for SparseBitmap {
    type Item = u32;
    type IntoIter = std::vec::IntoIter<u32>;

    fn into_iter(self) -> Self::IntoIter {
        self.to_vec().into_iter()
    }
}

/// Intersect two ascending, deduplicated slices into a new owned vector.
///
/// Linear two-pointer merge (both inputs sorted): advance the smaller cursor,
/// emit on equality. Spelled with `get` + a `while let` on the paired lookups
/// (not indexing under a proven loop bound) so the Trust L0 gate carries no
/// slice-bounds obligation. Incremental `push` keeps the allocation
/// per-element-bounded, the same reason `bitand` here does not `collect`.
fn intersect_sorted(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while let (Some(&av), Some(&bv)) = (a.get(i), b.get(j)) {
        match av.cmp(&bv) {
            Ordering::Less => i = i.saturating_add(1),
            Ordering::Greater => j = j.saturating_add(1),
            Ordering::Equal => {
                out.push(av);
                i = i.saturating_add(1);
                j = j.saturating_add(1);
            }
        }
    }
    out
}

impl BitAnd<&SparseBitmap> for &SparseBitmap {
    type Output = SparseBitmap;

    fn bitand(self, rhs: &SparseBitmap) -> SparseBitmap {
        // Decode both once; intersect on the ascending slices; re-encode the
        // (already sorted, deduplicated) result.
        SparseBitmap::from_sorted(&intersect_sorted(&self.to_vec(), &rhs.to_vec()))
    }
}

impl BitAndAssign<&SparseBitmap> for SparseBitmap {
    fn bitand_assign(&mut self, rhs: &SparseBitmap) {
        let intersection = intersect_sorted(&self.to_vec(), &rhs.to_vec());
        self.rebuild_from_sorted(&intersection);
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
    fn test_insert_out_of_order_stays_sorted() {
        let mut bm = SparseBitmap::new();
        // Not an append stream: values arrive unordered and with duplicates.
        for v in [30u32, 10, 20, 10, 25, 5] {
            bm.insert(v);
        }
        let vals: Vec<u32> = bm.iter().collect();
        assert_eq!(vals, vec![5, 10, 20, 25, 30]);
    }

    #[test]
    fn test_insert_return_value() {
        let mut bm = SparseBitmap::new();
        assert!(bm.insert(5)); // new
        assert!(bm.insert(3)); // new (out of order)
        assert!(!bm.insert(5)); // duplicate
        assert!(!bm.insert(3)); // duplicate
        assert!(bm.insert(9)); // new (append)
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
        let vals: Vec<u32> = bm.iter().collect();
        assert_eq!(vals, vec![1, 3]);
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
    fn test_drop_below() {
        let mut bm = SparseBitmap::new();
        for i in 0..10 {
            bm.insert(i);
        }
        bm.drop_below(4);
        let vals: Vec<u32> = bm.iter().collect();
        assert_eq!(vals, vec![4, 5, 6, 7, 8, 9]);
        // Below the minimum: no-op. Above the maximum: empties the list.
        bm.drop_below(0);
        assert_eq!(bm.len(), 6);
        bm.drop_below(100);
        assert!(bm.is_empty());
    }

    #[test]
    fn test_drop_below_equals_repeated_front_remove() {
        // The batched trim must match one-at-a-time removal of every value
        // below the watermark (the equivalence prefix eviction relies on).
        let seed = [2u32, 3, 5, 8, 13, 21, 34, 55];
        let mut batched = SparseBitmap::new();
        let mut incremental = SparseBitmap::new();
        for &v in &seed {
            batched.insert(v);
            incremental.insert(v);
        }
        batched.drop_below(13);
        for &v in seed.iter().filter(|&&v| v < 13) {
            incremental.remove(v);
        }
        let a: Vec<u32> = batched.iter().collect();
        let b: Vec<u32> = incremental.iter().collect();
        assert_eq!(a, b);
        assert_eq!(a, vec![13, 21, 34, 55]);
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
    fn test_range_from_and_before() {
        let mut bm = SparseBitmap::new();
        for v in [1u32, 4, 9, 16, 25] {
            bm.insert(v);
        }
        let from: Vec<u32> = bm.range_from(9).collect();
        assert_eq!(from, vec![9, 16, 25]);
        // `from` need not be a member: starts at the first value >= it.
        let from_gap: Vec<u32> = bm.range_from(10).collect();
        assert_eq!(from_gap, vec![16, 25]);
        let before: Vec<u32> = bm.range_before(16).collect();
        assert_eq!(before, vec![1, 4, 9]);
        // Owned range is double-ended (reverse navigation).
        let rev: Vec<u32> = bm.range_from(0).rev().collect();
        assert_eq!(rev, vec![25, 16, 9, 4, 1]);
    }

    #[test]
    fn test_contains() {
        let mut bm = SparseBitmap::new();
        for v in [2u32, 4, 6, 8] {
            bm.insert(v);
        }
        assert!(bm.contains(2));
        assert!(bm.contains(8));
        assert!(!bm.contains(0));
        assert!(!bm.contains(5));
        assert!(!bm.contains(9));
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
    fn test_bitand_disjoint_and_empty() {
        let mut a = SparseBitmap::new();
        a.insert(1);
        a.insert(3);
        let mut b = SparseBitmap::new();
        b.insert(2);
        b.insert(4);
        assert!((&a & &b).is_empty());
        let empty = SparseBitmap::new();
        assert!((&a & &empty).is_empty());
        assert!((&empty & &a).is_empty());
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

    #[test]
    fn test_large_gap_varint_roundtrip() {
        // Gaps spanning multiple varint bytes (>= 128, >= 16384) must decode
        // back to the exact ascending ids.
        let mut bm = SparseBitmap::new();
        let seed = [0u32, 1, 200, 500, 70_000, 70_001, 5_000_000];
        for &v in &seed {
            bm.insert(v);
        }
        let vals: Vec<u32> = bm.iter().collect();
        assert_eq!(vals, seed.to_vec());
        assert_eq!(bm.len(), seed.len() as u64);
        assert!(bm.contains(70_000));
        assert!(bm.contains(5_000_000));
        assert!(!bm.contains(70_002));
    }

    #[test]
    fn test_single_value_has_no_delta_bytes() {
        // A one-posting list is the common short-trigram case: it must encode
        // to zero delta bytes (only `first`), the memory win over 4 bytes.
        let mut bm = SparseBitmap::new();
        bm.insert(42);
        assert_eq!(bm.deltas.len(), 0);
        assert_eq!(bm.into_iter().collect::<Vec<_>>(), vec![42]);
    }
}

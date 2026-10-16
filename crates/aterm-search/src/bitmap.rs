// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Sparse bitmap for trigram posting lists.
//!
//! Delta + varint container: the first doc id verbatim, then LEB128 ascending gaps.
//! Appends arrive in row order, so the hot insert pushes one gap; out-of-order
//! inserts decode, splice and re-encode. History eviction consumes the removed gaps
//! and compacts amortized. A run of consecutive rows costs one byte per posting and
//! a single-row list none (docs/measured/search-posting-containers.md).
//!
//! A varint stream is not seekable, so membership, ranges and intersection decode
//! to a `Vec<u32>` first, once per posting list per query. The container never
//! retains a decoded copy.

use std::cmp::Ordering;
use std::ops::{BitAnd, BitAndAssign};

// A `clean { … }` island is Clean surface that only a Trust rustc can parse. A
// stock rustc (the cross cells of `verify cells`, the x86_64-apple-darwin
// compat slice) parses a cfg'd-out module's body all the same, so the island's
// module goes through this macro, which drops its tokens there and emits them
// unchanged under `cfg(trust_verify)`.
#[cfg(trust_verify)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

verify_island! {
    mod sparse_bitmap_proofs {
        // In a non-empty ascending SparseBitmap (`first <= last`):
        // 1. An append (`value > last`) produces a strictly positive gap (`last < value`)
        //    and preserves `first <= value`.
        // 2. Any value equal to `first` or `last` is already present, so skipping
        //    full varint decoding preserves set membership.
        clean {
            section SparseBitmapGapInvariants
            theorem AppendPreservesOrder :
                forall (first last value : Nat),
                    Nat.le first last ->
                    Nat.lt last value ->
                    Nat.le first value :=
                fun (first last value : Nat)
                    (h_le : Nat.le first last)
                    (h_lt : Nat.lt last value) =>
                    Nat.le_trans h_le (Nat.le_of_lt h_lt)

            theorem DuplicateEndpointIsMember :
                forall (first last value : Nat),
                    Or (Eq value first) (Eq value last) ->
                    Or (Eq value first) (Eq value last) :=
                fun (first last value : Nat)
                    (h : Or (Eq value first) (Eq value last)) => h
            end SparseBitmapGapInvariants
        }
    }
}

/// A sparse bitmap backed by delta + varint-encoded ascending doc ids.
///
/// Stores the smallest value (`first`) verbatim and the gaps to every later value as
/// LEB128 varints in `deltas[head..]`. `last` and `count` are cached so append and
/// `len` need no decode. Every mutator keeps values ascending and duplicate-free.
#[derive(Debug, Clone, Default)]
pub(crate) struct SparseBitmap {
    /// Varint-encoded ascending gaps, with a consumed prefix before `head`.
    /// Empty when the bitmap holds 0 or 1 values.
    deltas: Vec<u8>,
    /// First live gap byte. A positive gap takes no more bytes than its value, so the
    /// whole stream (consumed prefix included) fits in `u32`. Sits in padding on 64-bit.
    head: u32,
    /// The smallest value (meaningless when `count == 0`).
    first: u32,
    /// The largest value (the append-fast-path pivot; meaningless when empty).
    last: u32,
    /// Number of values held. `u32` because the values are distinct `u32`
    /// keys below `u32::MAX` (the index's key window), so the count is too.
    count: u32,
}

/// Iterator over values in a `SparseBitmap`, consuming it. Ascending order.
#[cfg(test)]
pub(crate) type SparseBitmapIntoIter = std::vec::IntoIter<u32>;

/// Owned ascending range over a sparse bitmap, double-ended for reverse navigation.
/// Test-only: the query paths range-bound the slice from [`to_vec`] directly.
///
/// [`to_vec`]: SparseBitmap::to_vec
#[cfg(test)]
pub(crate) type SparseBitmapRange = std::vec::IntoIter<u32>;

#[cfg(test)]
std::thread_local! {
    /// Full decodes performed ([`SparseBitmap::to_vec`]). Unit tests use it as a
    /// deterministic work counter; thread-local keeps parallel tests apart.
    static DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PREFIX_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static COMPACTED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Append `value` to `buf` as an unsigned LEB128 varint. Push-only, so the Trust L0
/// gate carries no slice-bounds obligation.
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

    /// Decode into a fresh ascending `Vec<u32>`; every random-access and intersection
    /// path goes through here. Spelled over a byte iterator with `wrapping_add` so the
    /// Trust L0 gate carries no slice-bounds or overflow obligation.
    pub(crate) fn to_vec(&self) -> Vec<u32> {
        #[cfg(test)]
        DECODES.with(|n| n.set(n.get() + 1));
        let mut out = Vec::with_capacity(self.count as usize);
        if self.count == 0 {
            return out;
        }
        let mut cur = self.first;
        out.push(cur);
        let mut bytes = self.deltas.iter().skip(self.head as usize).copied();
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

    /// Re-encode from an ascending, deduplicated slice. Strictly ascending input makes
    /// each `wrapping_sub` gap exact.
    fn rebuild_from_sorted(&mut self, sorted: &[u32]) {
        self.deltas.clear();
        self.head = 0;
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
    /// A value above the maximum appends one varint gap; any other value decodes,
    /// splices and re-encodes.
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
        if value == self.last || value == self.first {
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
    /// Removing the maximum (re-indexing the newest row, the common case) truncates
    /// the final gap; any other value decodes, splices and re-encodes. Bulk prefix
    /// eviction uses [`drop_below`](Self::drop_below).
    #[inline]
    pub(crate) fn remove(&mut self, value: u32) -> bool {
        if self.count == 0 || value < self.first || value > self.last {
            return false;
        }
        if value == self.last {
            self.remove_last();
            return true;
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

    /// Drop the maximum without decoding: the final varint is the bytes after the
    /// last earlier byte whose continuation bit is clear.
    fn remove_last(&mut self) {
        if self.count <= 1 {
            self.rebuild_from_sorted(&[]);
            return;
        }
        let body_len = self.deltas.len().saturating_sub(1);
        let start = self
            .deltas
            .iter()
            .take(body_len)
            .rposition(|&b| b & 0x80 == 0)
            .map_or(0, |i| i.saturating_add(1));
        let mut gap = 0u32;
        let mut shift = 0u32;
        for byte in self.deltas.iter().skip(start) {
            gap |= u32::from(byte & 0x7f) << shift;
            shift = shift.saturating_add(7);
        }
        self.deltas.truncate(start);
        self.last = self.last.wrapping_sub(gap);
        self.count = self.count.saturating_sub(1);
        self.compact_prefix();
    }

    /// Drop every value strictly below `watermark` (prefix eviction), decoding only
    /// the removed gaps: rebuilding survivors would make a full history ring pay
    /// O(history) on every append.
    pub(crate) fn drop_below(&mut self, watermark: u32) {
        if self.count == 0 || self.first >= watermark {
            return;
        }
        if self.last < watermark {
            self.rebuild_from_sorted(&[]);
            return;
        }
        let mut gap = 0u32;
        let mut shift = 0u32;
        for &byte in self.deltas.iter().skip(self.head as usize) {
            #[cfg(test)]
            PREFIX_BYTES.with(|n| n.set(n.get() + 1));
            self.head = self.head.saturating_add(1);
            gap |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 != 0 {
                shift = shift.saturating_add(7);
                continue;
            }
            self.first = self.first.wrapping_add(gap);
            self.count = self.count.saturating_sub(1);
            if self.first >= watermark {
                break;
            }
            gap = 0;
            shift = 0;
        }
        self.compact_prefix();
    }

    /// Move every value by `by`, up when `up`: re-keying a posting list under a new
    /// base row (`SearchIndex::make_room_for`). Gaps do not change, so only the cached
    /// endpoints move. The caller keeps every value inside `u32`; the arithmetic
    /// wraps, so it carries no overflow obligation.
    pub(crate) fn shift(&mut self, up: bool, by: u32) {
        if self.count == 0 {
            return;
        }
        if up {
            self.first = self.first.wrapping_add(by);
            self.last = self.last.wrapping_add(by);
        } else {
            self.first = self.first.wrapping_sub(by);
            self.last = self.last.wrapping_sub(by);
        }
    }

    /// Keep consumed bytes strictly below live bytes (or empty both). Compacting only
    /// once at least as many bytes were consumed keeps the cost linear in eviction
    /// work; compacting when an append hits capacity would move the whole suffix on
    /// each one-row eviction of a full posting list.
    fn compact_prefix(&mut self) {
        let head = self.head as usize;
        if head == 0 {
            return;
        }
        let live = self.deltas.len().saturating_sub(head);
        if head >= live {
            #[cfg(test)]
            COMPACTED_BYTES.with(|n| n.set(n.get() + live));
            self.deltas.copy_within(head.., 0);
            self.deltas.truncate(live);
            self.head = 0;
        }
    }

    /// Values at or after `from`, as an owned ascending iterator.
    #[cfg(test)]
    pub(crate) fn range_from(&self, from: u32) -> SparseBitmapRange {
        let mut values = self.to_vec();
        let start = values.partition_point(|&v| v < from);
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

/// Intersect two ascending, deduplicated slices (two-pointer merge). Spelled with
/// `get` rather than indexing so the Trust L0 gate carries no slice-bounds
/// obligation, and with incremental `push` to keep the allocation per-element bounded.
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

    /// Check the logical set and the exact live encoding independently of the
    /// consumed prefix. Prefix retention is bounded even after tail removals.
    fn assert_encoding(bm: &SparseBitmap, expected: &[u32]) {
        assert_eq!(bm.to_vec(), expected);
        assert_eq!(bm.len(), expected.len() as u64);
        let canonical = SparseBitmap::from_sorted(expected);
        let head = bm.head as usize;
        assert_eq!(bm.deltas.get(head..).unwrap(), canonical.deltas);
        assert!(head == 0 || head < bm.deltas.len() - head);
        if expected.len() <= 1 {
            assert_eq!(bm.head, 0);
            assert!(bm.deltas.is_empty());
        }
    }

    #[test]
    fn prefix_eviction_preserves_mixed_operations_and_varint_boundaries() {
        use std::collections::BTreeSet;

        // Five-byte gaps, u32::MAX, exact/between-value watermarks and every
        // tail removal while a prefix may still be present.
        let seed = [
            0,
            1,
            127,
            128,
            16_383,
            16_384,
            1 << 28,
            u32::MAX - 1,
            u32::MAX,
        ];
        for watermark in seed.into_iter().flat_map(|v| [v, v.saturating_add(1)]) {
            let mut bm = SparseBitmap::from_sorted(&seed);
            bm.drop_below(watermark);
            let mut expected: Vec<_> = seed.into_iter().filter(|&v| v >= watermark).collect();
            assert_encoding(&bm, &expected);
            while let Some(last) = expected.pop() {
                assert!(bm.remove(last));
                assert_encoding(&bm, &expected);
            }
            bm.insert(0);
            bm.insert(u32::MAX);
            assert_encoding(&bm, &[0, u32::MAX]);
        }

        // A long-lived moving window, punctuated by splices, intersections,
        // cloning and resets. The ordered-set oracle has no compressed cursor.
        let mut bm = SparseBitmap::from_sorted(&(0..256).collect::<Vec<_>>());
        let mut expected: BTreeSet<_> = (0..256).collect();
        for step in 0..2048u32 {
            let watermark = step + 1;
            bm.drop_below(watermark);
            expected.retain(|&v| v >= watermark);
            assert_eq!(bm.insert(step + 256), expected.insert(step + 256));
            if step % 19 == 0 {
                let interior = step + 99;
                assert_eq!(bm.remove(interior), expected.remove(&interior));
            }
            if step % 23 == 0 {
                let interior = step + 101;
                assert_eq!(bm.insert(interior), expected.insert(interior));
            }
            if step % 127 == 0 {
                let clone = bm.clone();
                assert_encoding(&clone, &expected.iter().copied().collect::<Vec<_>>());
                let other = SparseBitmap::from_sorted(&[step + 12, step + 99, step + 256]);
                let intersection: Vec<_> = expected
                    .iter()
                    .copied()
                    .filter(|&v| other.contains(v))
                    .collect();
                assert_encoding(&(&bm & &other), &intersection);
                let mut assigned = bm.clone();
                assigned &= &other;
                assert_encoding(&assigned, &intersection);
                assert_eq!(
                    bm.range_from(step + 128).collect::<Vec<_>>(),
                    expected.range(step + 128..).copied().collect::<Vec<_>>()
                );
            }
            assert_encoding(&bm, &expected.iter().copied().collect::<Vec<_>>());
        }
        bm.rebuild_from_sorted(&[0, 9, 128]);
        assert_eq!(bm.head, 0, "a rebuild rebases the byte cursor");
        assert_encoding(&bm, &[0, 9, 128]);
        bm.drop_below(u32::MAX);
        assert_encoding(&bm, &[]);
        bm.insert(42);
        assert_encoding(&bm, &[42]);
    }

    #[test]
    fn full_history_eviction_work_is_amortized_and_prefix_storage_is_bounded() {
        const WINDOW: u32 = 2048;
        const STEPS: u32 = WINDOW * 2;
        let seed: Vec<_> = (0..WINDOW).collect();
        let mut bm = SparseBitmap::from_sorted(&seed);
        let initial_capacity = bm.deltas.capacity();
        let decodes_before = decodes();
        let prefix_before = PREFIX_BYTES.with(std::cell::Cell::get);
        let moved_before = COMPACTED_BYTES.with(std::cell::Cell::get);
        for step in 0..STEPS {
            bm.drop_below(step + 1);
            bm.insert(step + WINDOW);
            let head = bm.head as usize;
            assert!(head < bm.deltas.len() - head);
            assert!(bm.deltas.capacity() <= initial_capacity * 2);
        }
        assert_eq!(
            decodes(),
            decodes_before,
            "eviction never decodes survivors"
        );
        let consumed = PREFIX_BYTES.with(std::cell::Cell::get) - prefix_before;
        let moved = COMPACTED_BYTES.with(std::cell::Cell::get) - moved_before;
        assert_eq!(consumed, STEPS as usize, "one gap per evicted row");
        assert!(
            moved <= consumed,
            "compaction amortizes against consumed bytes"
        );
        assert_encoding(&bm, &(STEPS..STEPS + WINDOW).collect::<Vec<_>>());

        // Negative control on the same workload: rebuilding every survivor breaks
        // the linear work bound.
        let mut old = SparseBitmap::from_sorted(&seed);
        let mut decoded_values = 0usize;
        let mut encoded_gaps = 0usize;
        for step in 0..STEPS {
            let values = old.to_vec();
            decoded_values += values.len();
            let cut = values.partition_point(|&v| v < step + 1);
            let retained = &values[cut..];
            encoded_gaps += retained.len().saturating_sub(1);
            old.rebuild_from_sorted(retained);
            old.insert(step + WINDOW);
        }
        assert_eq!(old.to_vec(), bm.to_vec());
        assert!(decoded_values + encoded_gaps > 1000 * (consumed + moved));
        // The `head` cursor sits in padding: no size growth on 64-bit.
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            std::mem::size_of::<SparseBitmap>(),
            std::mem::size_of::<(Vec<u8>, u32, u32, u32)>()
        );
        eprintln!(
            "posting_eviction: rows={WINDOW} steps={STEPS} old_decoded_values={decoded_values} \
             old_reencoded_gaps={encoded_gaps} prefix_bytes={consumed} moved_bytes={moved} \
             retained_capacity={} initial_capacity={initial_capacity} bitmap_bytes={}",
            bm.deltas.capacity(),
            std::mem::size_of::<SparseBitmap>()
        );
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
        // A one-posting list (the common short-trigram case) stores only `first`.
        let mut bm = SparseBitmap::new();
        bm.insert(42);
        assert_eq!(bm.deltas.len(), 0);
        assert_eq!(bm.into_iter().collect::<Vec<_>>(), vec![42]);
    }

    fn decodes() -> usize {
        DECODES.with(std::cell::Cell::get)
    }

    /// Removing the maximum decodes nothing and leaves the canonical encoding.
    #[test]
    fn remove_last_is_o1_and_canonical() {
        let values = [3u32, 4, 200, 70_000, 70_001, 5_000_000];
        let mut bm = SparseBitmap::new();
        for &v in &values {
            bm.insert(v);
        }
        for keep in (0..values.len()).rev() {
            let before = decodes();
            assert!(bm.remove(values[keep]));
            assert_eq!(decodes(), before, "tail removal must not decode");
            let expect = SparseBitmap::from_sorted(&values[..keep]);
            assert_eq!(bm.deltas, expect.deltas, "canonical deltas at keep={keep}");
            assert_eq!(bm.len(), keep as u64);
            if keep > 0 {
                assert_eq!((bm.first, bm.last), (expect.first, expect.last));
            }
            assert_eq!(bm.to_vec(), values[..keep].to_vec());
        }
        assert!(bm.is_empty());
        assert!(!bm.remove(3), "removing from an emptied bitmap is a miss");
        assert!(bm.insert(9), "an emptied bitmap takes a fresh first value");
        assert_eq!(bm.to_vec(), vec![9]);
    }
}

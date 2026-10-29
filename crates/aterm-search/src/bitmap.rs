// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Delta and LEB128 varint-encoded sparse bitmap for trigram posting lists.

use std::cmp::Ordering;
use std::ops::{BitAnd, BitAndAssign};

#[cfg(clean_islands)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

verify_island! {
    mod sparse_bitmap_proofs {
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

/// Sparse bitmap backed by delta and LEB128 varint-encoded ascending doc IDs.
#[derive(Debug, Clone, Default)]
pub(crate) struct SparseBitmap {
    deltas: Vec<u8>,
    head: u32,
    first: u32,
    last: u32,
    count: u32,
}

/// Iterator over values in a `SparseBitmap`, consuming it. Ascending order.
#[cfg(test)]
pub(crate) type SparseBitmapIntoIter = std::vec::IntoIter<u32>;

#[cfg(test)]
std::thread_local! {
    static DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PREFIX_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static COMPACTED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[inline]
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

    /// Decodes the bitmap into an ascending `Vec<u32>`.
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

    fn from_sorted(sorted: &[u32]) -> Self {
        let mut bm = Self::default();
        bm.rebuild_from_sorted(sorted);
        bm
    }

    /// Inserts `value`, returning `true` if newly inserted.
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

    /// Removes `value`, returning `true` if present.
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

    /// Drops every value strictly below `watermark` by advancing past evicted prefix gaps.
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

    /// Shifts every value up or down by `by` without decoding gaps.
    #[inline]
    pub(crate) const fn shift(&mut self, up: bool, by: u32) {
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

    /// Returns `true` if the bitmap contains no values.
    #[inline]
    #[must_use]
    pub(crate) const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the number of values in the bitmap.
    #[inline]
    #[must_use]
    pub(crate) const fn len(&self) -> u64 {
        self.count as u64
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
}

impl IntoIterator for SparseBitmap {
    type Item = u32;
    type IntoIter = std::vec::IntoIter<u32>;

    fn into_iter(self) -> Self::IntoIter {
        self.to_vec().into_iter()
    }
}

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
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            std::mem::size_of::<SparseBitmap>(),
            std::mem::size_of::<(Vec<u8>, u32, u32, u32)>()
        );
        eprintln!(
            "posting_eviction: rows={WINDOW} steps={STEPS} prefix_bytes={consumed} moved_bytes={moved} \
             retained_capacity={} initial_capacity={initial_capacity} bitmap_bytes={}",
            bm.deltas.capacity(),
            std::mem::size_of::<SparseBitmap>()
        );
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

    fn decodes() -> usize {
        DECODES.with(std::cell::Cell::get)
    }

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

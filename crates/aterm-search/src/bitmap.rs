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

/// Borrowed double-ended cursor over compressed posting values.
pub(crate) struct PostingCursor<'a> {
    bytes: &'a [u8],
    first: u32,
    last: u32,
    remaining: u32,
}

impl PostingCursor<'_> {
    /// Read the next endpoint without decoding a gap.
    pub(crate) fn peek(&self, backward: bool) -> Option<u32> {
        (self.remaining != 0).then_some(if backward { self.last } else { self.first })
    }

    /// Skip an encoded prefix in one pass, without constructing one iterator item per row.
    pub(crate) fn seek_forward(&mut self, boundary: u32) {
        if self.remaining == 0 || self.first >= boundary {
            return;
        }
        if self.last < boundary {
            self.remaining = 0;
            return;
        }
        let mut gap = 0u32;
        let mut shift = 0;
        for (i, &byte) in self.bytes.iter().enumerate() {
            gap |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 != 0 {
                shift += 7;
                continue;
            }
            self.first = self.first.wrapping_add(gap);
            self.remaining -= 1;
            if self.first >= boundary {
                self.bytes = self.bytes.get(i + 1..).unwrap_or_default();
                return;
            }
            gap = 0;
            shift = 0;
        }
    }

    /// Skip values at or above an exclusive upper bound in one reverse byte walk.
    pub(crate) fn seek_backward(&mut self, boundary: u32) {
        if self.remaining == 0 || self.last < boundary {
            return;
        }
        if self.first >= boundary {
            self.remaining = 0;
            return;
        }
        let mut gap = 0u32;
        for (i, &byte) in self.bytes.iter().enumerate().rev() {
            gap = (gap << 7) | u32::from(byte & 0x7f);
            if i != 0 && self.bytes.get(i - 1).is_some_and(|b| b & 0x80 != 0) {
                continue;
            }
            self.last = self.last.wrapping_sub(gap);
            self.remaining -= 1;
            if self.last < boundary {
                self.bytes = self.bytes.get(..i).unwrap_or_default();
                return;
            }
            gap = 0;
        }
    }
}

impl Iterator for PostingCursor<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        if self.remaining == 0 {
            return None;
        }
        let value = self.first;
        self.remaining -= 1;
        if self.remaining != 0 {
            let mut gap = 0u32;
            let mut shift = 0;
            loop {
                let (&byte, rest) = self.bytes.split_first()?;
                self.bytes = rest;
                gap |= u32::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            self.first = self.first.wrapping_add(gap);
        }
        Some(value)
    }
}

impl DoubleEndedIterator for PostingCursor<'_> {
    fn next_back(&mut self) -> Option<u32> {
        if self.remaining == 0 {
            return None;
        }
        let value = self.last;
        self.remaining -= 1;
        if self.remaining != 0 {
            let end = self.bytes.len().checked_sub(1)?;
            let start = self
                .bytes
                .get(..end)?
                .iter()
                .rposition(|b| b & 0x80 == 0)
                .map_or(0, |i| i + 1);
            let mut gap = 0u32;
            for (i, &byte) in self.bytes.get(start..)?.iter().enumerate() {
                gap |= u32::from(byte & 0x7f) << (i * 7);
            }
            self.bytes = self.bytes.get(..start)?;
            self.last = self.last.wrapping_sub(gap);
        }
        Some(value)
    }
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

fn read_varint(bytes: &[u8]) -> Option<(u32, usize)> {
    let mut value = 0u32;
    for (i, &byte) in bytes.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7f) << (i * 7);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// The encoded gap bracketing a key (`previous < key <= next`).
struct Gap {
    previous: u32,
    next: u32,
    start: usize,
    end: usize,
}

impl SparseBitmap {
    /// Locate the gap for a key strictly between the endpoints, starting at the nearer numeric
    /// endpoint and retaining byte positions instead of expanded IDs.
    fn gap_at(&self, value: u32) -> Option<Gap> {
        let mut cursor = self.cursor();
        if value.wrapping_sub(self.first) <= self.last.wrapping_sub(value) {
            cursor.seek_forward(value);
            let next = cursor.peek(false)?;
            let end = self.deltas.len().checked_sub(cursor.bytes.len())?;
            let start = self
                .deltas
                .get(..end.checked_sub(1)?)?
                .iter()
                .rposition(|b| b & 0x80 == 0)
                .map_or(0, |i| i + 1);
            let (gap, _) = read_varint(self.deltas.get(start..end)?)?;
            Some(Gap {
                previous: next.wrapping_sub(gap),
                next,
                start,
                end,
            })
        } else {
            cursor.seek_backward(value);
            let previous = cursor.peek(true)?;
            let start = (self.head as usize).checked_add(cursor.bytes.len())?;
            let (gap, len) = read_varint(self.deltas.get(start..)?)?;
            Some(Gap {
                previous,
                next: previous.wrapping_add(gap),
                start,
                end: start.checked_add(len)?,
            })
        }
    }

    /// Replace one or two adjacent gaps using bounded stack scratch.
    fn replace_gaps(&mut self, start: usize, end: usize, gaps: &[u32]) {
        let mut encoded = [0u8; 10];
        let mut len = 0;
        for &gap in gaps {
            let mut value = gap;
            while value >= 0x80 {
                encoded[len] = (value as u8) | 0x80;
                len += 1;
                value >>= 7;
            }
            encoded[len] = value as u8;
            len += 1;
        }
        self.deltas
            .splice(start..end, encoded[..len].iter().copied());
    }

    /// Walk live postings without expanding their encoded storage.
    pub(crate) fn cursor(&self) -> PostingCursor<'_> {
        PostingCursor {
            bytes: self.deltas.get(self.head as usize..).unwrap_or_default(),
            first: self.first,
            last: self.last,
            remaining: self.count,
        }
    }
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
        if value < self.first {
            let head = self.head as usize;
            self.replace_gaps(head, head, &[self.first - value]);
            self.first = value;
        } else {
            let Some(gap) = self.gap_at(value) else {
                return false;
            };
            if gap.next == value {
                return false;
            }
            self.replace_gaps(
                gap.start,
                gap.end,
                &[value - gap.previous, gap.next - value],
            );
        }
        self.count = self.count.saturating_add(1);
        true
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
        if value == self.first {
            self.drop_below(value.saturating_add(1));
            return true;
        }
        let Some(gap) = self.gap_at(value) else {
            return false;
        };
        if gap.next != value {
            return false;
        }
        let Some((next_gap, len)) = self.deltas.get(gap.end..).and_then(read_varint) else {
            return false;
        };
        let successor = value.wrapping_add(next_gap);
        self.replace_gaps(gap.start, gap.end + len, &[successor - gap.previous]);
        self.count = self.count.saturating_sub(1);
        self.compact_prefix();
        true
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
#[path = "bitmap_tests.rs"]
mod tests;

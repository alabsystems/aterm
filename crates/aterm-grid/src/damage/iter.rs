// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Iterators for efficient damage traversal.

use super::Damage;
use super::rect::LineDamageBounds;
use crate::row_u16;

/// Iterator over damaged rows: a counter for `Full`, a `trailing_zeros()`
/// bitset scan for `Partial`.
#[non_exhaustive]
pub enum DamagedRowIterator<'a> {
    /// Full damage - iterate all rows.
    Full {
        /// Current row.
        current: u16,
        /// Maximum row (exclusive).
        max: u16,
    },
    /// Partial damage - use bitset iteration.
    Partial(BitsetRowIterator<'a>),
}

impl Iterator for DamagedRowIterator<'_> {
    type Item = u16;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            DamagedRowIterator::Full { current, max } => {
                if *current < *max {
                    let row = *current;
                    *current += 1;
                    Some(row)
                } else {
                    None
                }
            }
            DamagedRowIterator::Partial(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            DamagedRowIterator::Full { current, max } => {
                let remaining = (*max - *current) as usize;
                (remaining, Some(remaining))
            }
            DamagedRowIterator::Partial(iter) => iter.size_hint(),
        }
    }
}

/// Iterator over the set bits of a bitset, skipping clean rows via
/// `trailing_zeros()`.
pub struct BitsetRowIterator<'a> {
    /// Reference to the bitset words.
    bits: &'a [u64],
    /// Current word index.
    word_idx: usize,
    /// Current word with consumed bits cleared.
    current_word: u64,
    /// Maximum row to yield (exclusive).
    max_row: u16,
}

impl<'a> BitsetRowIterator<'a> {
    /// Create a new bitset iterator.
    #[inline]
    pub(crate) fn new(bits: &'a [u64], max_row: u16) -> Self {
        let current_word = bits.first().copied().unwrap_or(0);
        Self {
            bits,
            word_idx: 0,
            current_word,
            max_row,
        }
    }
}

impl Iterator for BitsetRowIterator<'_> {
    type Item = u16;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.current_word != 0 {
                let bit_pos = self.current_word.trailing_zeros() as usize;
                // A tracker has at most u16::MAX rows (1024 words), so this <= 65535.
                let row = row_u16(self.word_idx * 64 + bit_pos);

                self.current_word &= !(1u64 << bit_pos);

                if row < self.max_row {
                    return Some(row);
                }
                return None;
            }

            self.word_idx += 1;
            if self.word_idx >= self.bits.len() {
                return None;
            }
            self.current_word = self.bits[self.word_idx];
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // Upper bound: the set bits left; `.get()` because an exhausted iterator's
        // word_idx is past the end.
        let remaining: usize = self.current_word.count_ones() as usize
            + self
                .bits
                .get(self.word_idx.saturating_add(1)..)
                .unwrap_or(&[])
                .iter()
                .map(|w| w.count_ones() as usize)
                .sum::<usize>();
        (0, Some(remaining))
    }
}

/// Iterator over damaged rows with their damaged column range (the renderer's
/// iterator).
pub struct DamageBoundsIterator<'a> {
    damage: &'a Damage,
    row_iter: DamagedRowIterator<'a>,
    cols: u16,
}

impl<'a> DamageBoundsIterator<'a> {
    /// Create a new bounds iterator.
    pub(crate) fn new(damage: &'a Damage, row_iter: DamagedRowIterator<'a>, cols: u16) -> Self {
        Self {
            damage,
            row_iter,
            cols,
        }
    }
}

impl Iterator for DamageBoundsIterator<'_> {
    type Item = LineDamageBounds;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let row = self.row_iter.next()?;
            if let Some((left, right)) = self.damage.row_damage_bounds(row, self.cols) {
                return Some(LineDamageBounds {
                    line: row,
                    left,
                    right,
                });
            }
            // Row's column bounds are entirely out of visible range — skip it.
        }
    }
}

/// Iterator merging consecutive damaged rows with overlapping column ranges
/// into rectangles (fewer draw calls).
#[cfg(any(test, debug_assertions))]
pub struct MergedDamageIterator<'a> {
    inner: DamageBoundsIterator<'a>,
    pending: Option<super::rect::DamageRect>,
}

#[cfg(any(test, debug_assertions))]
impl<'a> MergedDamageIterator<'a> {
    /// Create a new merged damage iterator.
    #[cfg(test)]
    pub fn new(damage: &'a Damage, rows: u16, cols: u16) -> Self {
        Self {
            inner: damage.iter_bounds(rows, cols),
            pending: None,
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl Iterator for MergedDamageIterator<'_> {
    type Item = super::rect::DamageRect;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Some(bounds) => match &mut self.pending {
                    Some(rect) if rect.can_extend_with(bounds) => {
                        rect.extend_with(bounds);
                    }
                    Some(_) => {
                        let result = self.pending.take();
                        self.pending = Some(super::rect::DamageRect::from_line(bounds));
                        return result;
                    }
                    None => {
                        self.pending = Some(super::rect::DamageRect::from_line(bounds));
                    }
                },
                None => {
                    return self.pending.take();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- BitsetRowIterator tests ----

    #[test]
    fn bitset_iter_empty_bits_yields_nothing() {
        let bits = vec![0u64; 4];
        let iter = BitsetRowIterator::new(&bits, 256);
        let rows: Vec<_> = iter.collect();
        assert!(rows.is_empty());
    }

    #[test]
    fn bitset_iter_empty_slice_yields_nothing() {
        let bits: Vec<u64> = vec![];
        let iter = BitsetRowIterator::new(&bits, 100);
        let rows: Vec<_> = iter.collect();
        assert!(rows.is_empty());
    }

    #[test]
    fn bitset_iter_single_bit_zero() {
        let bits = vec![1u64]; // bit 0
        let iter = BitsetRowIterator::new(&bits, 64);
        let rows: Vec<_> = iter.collect();
        assert_eq!(rows, vec![0]);
    }

    #[test]
    fn bitset_iter_single_bit_last_in_word() {
        let bits = vec![1u64 << 63]; // bit 63
        let iter = BitsetRowIterator::new(&bits, 64);
        let rows: Vec<_> = iter.collect();
        assert_eq!(rows, vec![63]);
    }

    #[test]
    fn bitset_iter_all_bits_set_in_word() {
        let bits = vec![u64::MAX];
        let iter = BitsetRowIterator::new(&bits, 64);
        let rows: Vec<_> = iter.collect();
        assert_eq!(rows.len(), 64);
        assert_eq!(rows[0], 0);
        assert_eq!(rows[63], 63);
    }

    #[test]
    fn bitset_iter_max_row_truncates_results() {
        let bits = vec![u64::MAX]; // all 64 bits set
        let iter = BitsetRowIterator::new(&bits, 5);
        let rows: Vec<_> = iter.collect();
        assert_eq!(rows, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn bitset_iter_max_row_zero_yields_nothing() {
        let bits = vec![u64::MAX];
        let iter = BitsetRowIterator::new(&bits, 0);
        let rows: Vec<_> = iter.collect();
        assert!(rows.is_empty());
    }

    #[test]
    fn bitset_iter_cross_word_boundary() {
        let mut bits = vec![0u64; 2];
        bits[0] = 1 << 63; // row 63
        bits[1] = 1; // row 64
        let iter = BitsetRowIterator::new(&bits, 128);
        let rows: Vec<_> = iter.collect();
        assert_eq!(rows, vec![63, 64]);
    }

    #[test]
    fn bitset_iter_size_hint_upper_bound() {
        let mut bits = vec![0u64; 2];
        bits[0] = 0b111; // 3 bits set
        bits[1] = 0b11; // 2 bits set
        let iter = BitsetRowIterator::new(&bits, 128);
        let (lower, upper) = iter.size_hint();
        assert_eq!(lower, 0);
        assert_eq!(upper, Some(5));
    }

    #[test]
    fn bitset_iter_size_hint_decreases_as_consumed() {
        let bits = vec![0b1111u64]; // 4 bits set
        let mut iter = BitsetRowIterator::new(&bits, 64);

        let (_, upper_before) = iter.size_hint();
        assert_eq!(upper_before, Some(4));

        iter.next(); // consume one
        let (_, upper_after) = iter.size_hint();
        assert_eq!(upper_after, Some(3));
    }

    // ---- DamagedRowIterator tests ----

    #[test]
    fn full_iter_yields_all_rows() {
        let iter = DamagedRowIterator::Full { current: 0, max: 5 };
        let rows: Vec<_> = iter.collect();
        assert_eq!(rows, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn full_iter_empty_range() {
        let iter = DamagedRowIterator::Full { current: 0, max: 0 };
        let rows: Vec<_> = iter.collect();
        assert!(rows.is_empty());
    }

    #[test]
    fn full_iter_size_hint_exact() {
        let iter = DamagedRowIterator::Full {
            current: 2,
            max: 10,
        };
        let (lower, upper) = iter.size_hint();
        assert_eq!(lower, 8);
        assert_eq!(upper, Some(8));
    }

    #[test]
    fn partial_iter_delegates_to_bitset() {
        let mut bits = vec![0u64];
        bits[0] = (1 << 3) | (1 << 7); // rows 3, 7
        let bitset = BitsetRowIterator::new(&bits, 24);
        let iter = DamagedRowIterator::Partial(bitset);
        let rows: Vec<_> = iter.collect();
        assert_eq!(rows, vec![3, 7]);
    }

    // ---- DamageBoundsIterator tests ----

    #[test]
    fn bounds_iter_empty_damage_yields_nothing() {
        let damage = Damage::new(24);
        let bounds: Vec<_> = damage.iter_bounds(24, 80).collect();
        assert!(bounds.is_empty());
    }

    #[test]
    fn bounds_iter_single_cell_damage() {
        let mut damage = Damage::new(24);
        damage.mark_cell(5, 10);
        let bounds: Vec<_> = damage.iter_bounds(24, 80).collect();
        assert_eq!(bounds.len(), 1);
        assert_eq!(bounds[0].line, 5);
        assert_eq!(bounds[0].left, 10);
        assert_eq!(bounds[0].right, 11);
    }

    #[test]
    fn bounds_iter_full_damage_covers_all_cols() {
        let damage = Damage::Full;
        let bounds: Vec<_> = damage.iter_bounds(3, 80).collect();
        assert_eq!(bounds.len(), 3);
        for (i, b) in bounds.iter().enumerate() {
            assert_eq!(b.line, i as u16);
            assert_eq!(b.left, 0);
            assert_eq!(b.right, 80);
        }
    }
}

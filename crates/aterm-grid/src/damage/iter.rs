// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Iterators for efficient damage traversal.

use super::Damage;
use super::rect::LineDamageBounds;
use crate::row_u16;

/// Iterator over damaged rows.
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

    #[inline]
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

/// Iterator over the set bits of a bitset, skipping clean rows via `trailing_zeros()`.
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

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
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

/// Iterator over damaged rows with their damaged column range (the renderer's iterator).
pub struct DamageBoundsIterator<'a> {
    damage: &'a Damage,
    row_iter: DamagedRowIterator<'a>,
    cols: u16,
}

impl<'a> DamageBoundsIterator<'a> {
    /// Create a new bounds iterator.
    #[inline]
    pub(crate) const fn new(
        damage: &'a Damage,
        row_iter: DamagedRowIterator<'a>,
        cols: u16,
    ) -> Self {
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
        }
    }
}

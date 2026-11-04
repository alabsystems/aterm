// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact quorum relations for the active candidates of one evaluation.

use crate::membership_log::MAX_WEIGHED_RECORDS;

const MAX_BYTES: usize = 1024 * 1024;
/// The most quorum candidates a log holds: each is a group of at least two `Remove`s, and a log
/// holds at most [`MAX_WEIGHED_RECORDS`] weighed records, its genesis one of them.
pub(super) const MAX_CANDIDATES: usize = (MAX_WEIGHED_RECORDS - 1) / 2;
// Both relations over that many candidates fit the payload budget, so `Relations::new` never
// refuses a held log's candidates (the weighing `expect`s it).
const _: () =
    assert!(2 * MAX_CANDIDATES * MAX_CANDIDATES.div_ceil(64) * size_of::<u64>() <= MAX_BYTES);
const ABSENT: usize = usize::MAX;
type Plane = Vec<Option<Box<[u64]>>>;

/// Two independent relations over explicitly named, sorted active candidate indices.
/// The fixed payload budget fits every candidate set under the weighed record budget.
/// Plane slots and fixed-width rows are allocated only when they first receive a bit.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::membership_log::eval) struct Relations {
    active: Vec<usize>,
    positions: Vec<usize>,
    words_per_row: usize,
    planes: [Option<Plane>; 2],
}

impl Relations {
    /// A layout of sorted, unique active indices within the current candidate bound.
    ///
    /// # Errors
    /// The layout is invalid or its checked packed payload exceeds the fixed budget.
    pub(super) fn new(active: &[usize]) -> Result<Self, &'static str> {
        if active.len() > MAX_CANDIDATES {
            return Err("quorum candidate count exceeds the current record bound");
        }
        if active.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("active quorum candidates are not sorted and unique");
        }
        if active.last().is_some_and(|&last| last >= MAX_CANDIDATES) {
            return Err("quorum candidate index exceeds the current record bound");
        }
        let words_per_row = active.len().div_ceil(64);
        let words_per_plane = active
            .len()
            .checked_mul(words_per_row)
            .ok_or("quorum relation size overflow")?;
        let word_count = words_per_plane
            .checked_mul(2)
            .ok_or("quorum relation size overflow")?;
        let bytes = word_count
            .checked_mul(std::mem::size_of::<u64>())
            .ok_or("quorum relation size overflow")?;
        if bytes > MAX_BYTES {
            return Err("quorum relations exceed the fixed payload budget");
        }
        let mut positions = vec![ABSENT; active.last().map_or(0, |last| last + 1)];
        for (slot, &candidate) in active.iter().enumerate() {
            positions[candidate] = slot;
        }
        Ok(Self {
            active: active.to_vec(),
            positions,
            words_per_row,
            planes: [None, None],
        })
    }

    pub(super) fn active(&self) -> &[usize] {
        &self.active
    }

    pub(super) fn position(&self, candidate: usize) -> Option<usize> {
        self.positions
            .get(candidate)
            .copied()
            .filter(|&at| at != ABSENT)
    }

    /// # Panics
    /// Either endpoint is outside this active layout.
    pub(super) fn set_undoes(&mut self, one: usize, other: usize) -> bool {
        self.set(0, one, other)
    }

    /// # Panics
    /// Either endpoint is outside this active layout.
    pub(super) fn set_strips(&mut self, one: usize, other: usize) -> bool {
        self.set(1, one, other)
    }

    pub(super) fn undoes(&self, one: usize, other: usize) -> bool {
        self.contains(0, one, other)
    }

    pub(super) fn strips(&self, one: usize, other: usize) -> bool {
        self.contains(1, one, other)
    }

    pub(super) fn undoes_from(&self, one: usize) -> Row<'_> {
        self.row(0, one)
    }

    #[cfg(test)]
    pub(super) fn strips_from(&self, one: usize) -> Row<'_> {
        self.row(1, one)
    }

    #[cfg(test)]
    pub(in crate::membership_log::eval) fn undoes_pairs(
        &self,
    ) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.active
            .iter()
            .copied()
            .flat_map(|one| self.undoes_from(one).map(move |other| (one, other)))
    }

    #[cfg(test)]
    pub(in crate::membership_log::eval) fn strips_pairs(
        &self,
    ) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.active
            .iter()
            .copied()
            .flat_map(|one| self.strips_from(one).map(move |other| (one, other)))
    }

    pub(super) fn defeats(&self, one: usize, other: usize) -> bool {
        self.undoes(one, other)
            && (!self.undoes(other, one) || self.strips(one, other) && !self.strips(other, one))
    }

    pub(super) fn defeated_by(&self, one: usize) -> impl Iterator<Item = usize> + '_ {
        self.undoes_from(one)
            .filter(move |&other| self.defeats(one, other))
    }

    fn set(&mut self, plane: usize, one: usize, other: usize) -> bool {
        let from = self
            .position(one)
            .expect("relation source is an active candidate");
        let to = self
            .position(other)
            .expect("relation target is an active candidate");
        let count = self.active.len();
        let width = self.words_per_row;
        let rows = self.planes[plane].get_or_insert_with(|| vec![None; count]);
        let row = rows[from].get_or_insert_with(|| vec![0; width].into_boxed_slice());
        let word = &mut row[to / 64];
        let mask = 1u64 << (to % 64);
        let added = *word & mask == 0;
        *word |= mask;
        added
    }

    fn contains(&self, plane: usize, one: usize, other: usize) -> bool {
        let (Some(from), Some(to)) = (self.position(one), self.position(other)) else {
            return false;
        };
        self.planes[plane]
            .as_ref()
            .and_then(|rows| rows[from].as_ref())
            .is_some_and(|row| row[to / 64] & (1u64 << (to % 64)) != 0)
    }

    fn row(&self, plane: usize, one: usize) -> Row<'_> {
        let words = self
            .position(one)
            .and_then(|from| {
                self.planes[plane]
                    .as_ref()
                    .and_then(|rows| rows[from].as_deref())
            })
            .unwrap_or(&[]);
        Row {
            words,
            active: &self.active,
            word: 0,
            bits: 0,
            base: 0,
        }
    }

    #[cfg(test)]
    pub(in crate::membership_log::eval) fn payload_bytes(&self) -> usize {
        self.planes
            .iter()
            .flatten()
            .flat_map(|rows| rows.iter().flatten())
            .map(|row| row.len() * std::mem::size_of::<u64>())
            .sum()
    }

    #[cfg(test)]
    pub(in crate::membership_log::eval) fn metadata_bytes(&self) -> usize {
        (self.active.capacity() + self.positions.capacity()) * std::mem::size_of::<usize>()
            + self
                .planes
                .iter()
                .flatten()
                .map(|rows| rows.capacity() * std::mem::size_of::<Option<Box<[u64]>>>())
                .sum::<usize>()
    }

    #[cfg(test)]
    pub(in crate::membership_log::eval) fn allocated_rows(&self) -> usize {
        self.planes
            .iter()
            .flatten()
            .flat_map(|rows| rows.iter())
            .filter(|row| row.is_some())
            .count()
    }
}

/// Ascending original candidate IDs, borrowed without copying a row or its edges.
pub(super) struct Row<'a> {
    words: &'a [u64],
    active: &'a [usize],
    word: usize,
    bits: u64,
    base: usize,
}

impl Iterator for Row<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        loop {
            if self.bits != 0 {
                let bit = self.bits.trailing_zeros() as usize;
                self.bits &= self.bits - 1;
                return Some(self.active[self.base + bit]);
            }
            self.bits = *self.words.get(self.word)?;
            self.base = self.word * 64;
            self.word += 1;
        }
    }
}

#[cfg(test)]
mod tests;

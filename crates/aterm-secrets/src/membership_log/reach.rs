// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact reachability over a [`super::Log`]'s held records: the log owns it and extends it as it
//! holds each record, so every ancestry question (the evaluator's, a new record's checks, a
//! peer's missing records, a causal closure) is a bit test or a row union, never a walk.
//!
//! Record `i`'s row is its strict causal past as bits over the indices below it, `ceil(i / 64)`
//! words. Holding a record ORs in each parent's row and sets the parent's own bit; the log only
//! grows, so a row never changes once written.
//!
//! - Memory: the rows of `n` records take `bytes_for(n)` bytes, the sum over `i < n` of
//!   `ceil(i / 64) * 8` (16 MiB at 16,384 records).
//! - Work: holding a record whose parents sit at `j` costs the sum of `ceil(j / 64)` word ORs,
//!   so a reload costs that sum over every record and nothing else.
//! - Sharing: rows live in chunks of 64 behind an `Arc`. A log's clones share every chunk, and
//!   the first record a clone holds into a shared partial chunk copies that chunk alone.

use std::sync::Arc;

/// Rows per chunk.
const ROWS: usize = 64;

/// The strict causal past of each held record, by index.
#[derive(Clone, Default)]
pub(super) struct Reach {
    chunks: Vec<Arc<Chunk>>,
    len: usize,
    /// What extending this index cost, counted (never timed).
    #[cfg(test)]
    pub(super) cost: Cost,
}

/// What [`Reach::push`] did, for the pins in the tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Cost {
    /// Parent-row words ORed into new rows.
    pub(super) word_ors: usize,
    /// Shared chunks a push copied before writing into them.
    pub(super) chunk_copies: usize,
    /// The words those copies took.
    pub(super) copied_words: usize,
}

/// A broken way of building rows, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mutant {
    /// Each record's first parent's row is left out of its own.
    OmitParentRow,
    /// Each record's first parent's own bit is left unset.
    OmitParentBit,
}

#[cfg(test)]
thread_local! {
    /// The mutant [`Reach::push`] builds this thread's rows with, if any.
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
}

/// Rows `64 * index ..` packed in order: the first holds `index` words, each later one
/// `index + 1`.
struct Chunk {
    index: usize,
    words: Vec<u64>,
}

impl Chunk {
    fn new(index: usize) -> Self {
        Self {
            index,
            words: Vec::with_capacity(full_words(index)),
        }
    }

    fn row(&self, r: usize) -> &[u64] {
        let (at, width) = place(self.index, r);
        &self.words[at..at + width]
    }
}

impl Clone for Chunk {
    /// A copy reserves the whole chunk, so the rows written into it later never reallocate it.
    fn clone(&self) -> Self {
        let mut words = Vec::with_capacity(full_words(self.index));
        words.extend_from_slice(&self.words);
        Self {
            index: self.index,
            words,
        }
    }
}

/// Where row `r` of chunk `c` starts among the chunk's words, and how many words it holds.
const fn place(c: usize, r: usize) -> (usize, usize) {
    if r == 0 {
        (0, c)
    } else {
        (c + (r - 1) * (c + 1), c + 1)
    }
}

/// The words every row of chunk `c` takes together.
const fn full_words(c: usize) -> usize {
    ROWS * c + ROWS - 1
}

/// The bytes the rows of `n` records take: the sum over `i < n` of `ceil(i / 64) * 8`.
#[cfg(any(unix, test))]
pub(super) const fn bytes_for(n: usize) -> usize {
    let (full, rest) = (n / ROWS, n % ROWS);
    let mut words = ROWS * (full * full.saturating_sub(1) / 2) + (ROWS - 1) * full;
    if rest > 0 {
        words += full + (rest - 1) * (full + 1);
    }
    words * size_of::<u64>()
}

/// The most bytes the rows of a log of at most `n` records allocate: each chunk reserves all 64
/// of its rows when it starts, so a partial last chunk takes what a full one does.
#[cfg(any(unix, test))]
pub(super) const fn reserved_for(n: usize) -> usize {
    bytes_for(n.div_ceil(ROWS) * ROWS)
}

impl Reach {
    /// How many records it indexes.
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// Index the next record, whose parents are the indexed records at `parents`.
    ///
    /// # Panics
    /// A parent is not indexed yet.
    pub(super) fn push(&mut self, parents: impl IntoIterator<Item = usize>) {
        let i = self.len;
        let (c, r) = (i / ROWS, i % ROWS);
        if r == 0 {
            self.chunks.push(Arc::new(Chunk::new(c)));
        }
        let (earlier, current) = self.chunks.split_at_mut(c);
        let current = &mut current[0];
        #[cfg(test)]
        if Arc::strong_count(current) > 1 {
            self.cost.chunk_copies += 1;
            self.cost.copied_words += current.words.len();
        }
        let chunk = Arc::make_mut(current);
        let start = chunk.words.len();
        let width = place(c, r).1;
        chunk.words.resize(start + width, 0);
        let (prior, row) = chunk.words.split_at_mut(start);
        #[cfg(test)]
        let mut first = MUTANT.get();
        for p in parents {
            assert!(p < i, "a record's parents are indexed before it");
            let (pc, pr) = (p / ROWS, p % ROWS);
            let past = if pc == c {
                let (at, w) = place(c, pr);
                &prior[at..at + w]
            } else {
                earlier[pc].row(pr)
            };
            #[cfg(test)]
            let (past, own) = match first.take() {
                Some(Mutant::OmitParentRow) => (&[][..], true),
                Some(Mutant::OmitParentBit) => (past, false),
                None => (past, true),
            };
            #[cfg(not(test))]
            let own = true;
            for (word, more) in row.iter_mut().zip(past) {
                *word |= more;
            }
            #[cfg(test)]
            {
                self.cost.word_ors += past.len();
            }
            if own {
                row[p / 64] |= 1 << (p % 64);
            }
        }
        self.len += 1;
    }

    fn row(&self, b: usize) -> &[u64] {
        self.chunks[b / ROWS].row(b % ROWS)
    }

    /// Whether record `a` lies in record `b`'s strict causal past.
    pub(super) fn before(&self, a: usize, b: usize) -> bool {
        a < b && b < self.len && (self.row(b)[a / 64] >> (a % 64)) & 1 == 1
    }

    /// Whether record `a` is record `b` or lies in its causal past.
    pub(super) fn upto(&self, a: usize, b: usize) -> bool {
        (a == b && b < self.len) || self.before(a, b)
    }

    /// Whether record `a` lies in the strict causal past of a record whose parents are the
    /// indexed records at `parents`: it is one of them or lies in one's past.
    pub(super) fn in_past_of(&self, a: usize, parents: &[usize]) -> bool {
        parents.iter().any(|&p| self.upto(a, p))
    }

    /// OR record `b`'s strict past into `into`, a set over the indexed records.
    pub(super) fn union_past_into(&self, b: usize, into: &mut [u64]) {
        for (word, more) in into.iter_mut().zip(self.row(b)) {
            *word |= more;
        }
    }

    /// Make `into`, a set over the indexed records, record `b`'s strict past.
    pub(super) fn copy_past_into(&self, b: usize, into: &mut [u64]) {
        let row = self.row(b);
        into[..row.len()].copy_from_slice(row);
        into[row.len()..].fill(0);
    }

    /// Whether record `b`'s strict past meets `set`, a set over the indexed records.
    pub(super) fn past_intersects(&self, b: usize, set: &[u64]) -> bool {
        self.row(b).iter().zip(set).any(|(a, b)| a & b != 0)
    }

    /// The indexed records at `roots` and every record in their pasts, as a set over the indexed
    /// records.
    pub(super) fn closure(&self, roots: impl IntoIterator<Item = usize>) -> Vec<u64> {
        let mut set = vec![0; self.len.div_ceil(64)];
        for root in roots {
            self.union_past_into(root, &mut set);
            set[root / 64] |= 1 << (root % 64);
        }
        set
    }

    /// The bytes its rows hold allocated, every chunk's whole reservation included.
    pub(super) fn bytes(&self) -> usize {
        self.chunks
            .iter()
            .map(|chunk| chunk.words.capacity() * size_of::<u64>())
            .sum()
    }

    /// Whether this index and `other` hold chunk `c` in one allocation.
    #[cfg(test)]
    pub(super) fn shares_chunk(&self, other: &Self, c: usize) -> bool {
        match (self.chunks.get(c), other.chunks.get(c)) {
            (Some(one), Some(two)) => Arc::ptr_eq(one, two),
            _ => false,
        }
    }

    /// How many chunks it holds.
    #[cfg(test)]
    pub(super) fn chunks(&self) -> usize {
        self.chunks.len()
    }
}

impl std::fmt::Debug for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reach")
            .field("len", &self.len)
            .field("bytes", &self.bytes())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
pub(super) mod tests;

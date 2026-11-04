// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The registers of a log's entries: each `(audience, name)`'s ops in causal order, which of
//! them are carries, and its siblings, read from the log's reachability index.
//!
//! A carry is a `Put` of the value the register already held in that `Put`'s own causal past, as
//! an epoch's reseal writes when a Mac departs (the writer re-puts every entry under a fresh
//! epoch, changing no value), or a `Delete` where every live op of the register in its own causal
//! past is a `Delete`, as a reseal re-issues one (F21 rule 6). A `Delete` that settles a
//! `Put`/`Delete` conflict is no carry: it writes. A carry stands for the op whose value it
//! carries (its rep); every other op for itself.
//!
//! Every causal question is a row of the index or a frontier its one pass hands out
//! ([`Reach::frontiers`]), never a test of each pair of ops:
//!
//! - What an op's writer held is its frontier in the register, the ops newest in its past, less
//!   the carries another of them replaced ([`Register::live`]). The pass costs about the
//!   index's own build plus three rows per op. What a writer held is read once for each
//!   distinct frontier and kept by the frontier's words, so every op that reads a frontier read
//!   before (forked from one view, or alternating between merges) pays a lookup of its words,
//!   and the frontiers kept hold no more words than their ops' rows. Reading a frontier costs a
//!   step per op it holds and, only where it holds a carry beside an op of another class, a row
//!   per op over just the words holding the carries' reps.
//! - The siblings are the ops no other replaces: one union of rows for the ops that are not
//!   carries and one per class for the carries, each skipping an op already in it.
//!
//! So a register of `k` ops in a log of about `k` records costs `O(k^2 / 64)` row words for the
//! pass, the lookups and the siblings, where scanning every earlier op against the newest found
//! so far cost `O(k^3)` tests, plus what reading its distinct frontiers costs. That stays within
//! the first term where a register reads few large frontiers, as a chain, a fan, merges of
//! reseals and edits and ops alternating between merges do. It is not bounded so in general:
//! which carries a frontier replaced turns on the frontier as a whole, not on what its parents'
//! frontiers replaced (an op of another value above a rep can itself lie below one writing the
//! rep's value), so each distinct one is read afresh. A seated key that puts each of its ops on
//! a merge of a different set of many concurrent ops reads `O(k)` distinct frontiers of `O(k)`
//! ops: `O(k^2)` steps, and, where those ops are carries of distinct reps beside edits,
//! `O(k^3 / 64)` words. That is the resolver's bound; a register of 12,288 records whose every
//! edit lies on a merge of each carry so far costs about 6e8 words.

use super::{CandidateOp, DecodedOp};
use crate::membership_log::reach::{Frontier, Reach, holds, tally};
use std::collections::{BTreeMap, HashMap};

#[cfg(test)]
pub(super) mod oracle;
#[cfg(test)]
mod tests;

/// A broken way of classifying ops, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mutant {
    /// What a writer held keeps every carry, replaced or not.
    KeepEveryCarry,
    /// An op takes what the writer held at the frontier the last op read, whatever its own is.
    ReuseFrontier,
    /// What a writer held is read over the whole rows of its frontier's ops, not just the words
    /// holding the carries' reps.
    FullRows,
    /// What a writer held is kept for the last frontier read alone, as for ops forked from one
    /// view, so ops alternating between two frontiers read each again.
    RememberLast,
}

#[cfg(test)]
thread_local! {
    /// The mutant this thread's registers are classified with, if any.
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn mutant(which: Mutant) -> bool {
    MUTANT.get() == Some(which)
}

/// What an op writes: two ops of one class write the same thing (`same_op`).
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Writes<'a> {
    Delete,
    Put {
        if_absent: bool,
        anchor: Option<&'a str>,
        value: usize,
    },
}

impl<'a> Writes<'a> {
    fn of(op: &'a DecodedOp) -> Self {
        match op {
            DecodedOp::Delete => Self::Delete,
            DecodedOp::Put(entry) => Self::Put {
                if_absent: entry.if_absent,
                anchor: entry.anchor.as_deref(),
                value: entry.value_class,
            },
        }
    }
}

/// What a writer held at one frontier: the winner among its live ops, and whether every one of
/// them is a `Delete`.
#[derive(Clone, Copy)]
struct Held {
    winner: Option<usize>,
    deletes: bool,
}

/// One register's ops (in causal order, as `CandidateOp`s), which of them are carries, and what
/// each stands for.
pub(super) struct Register<'a> {
    cands: &'a [CandidateOp],
    /// Each op's class: ops of one class write the same thing.
    class: Vec<usize>,
    pub(super) carries: Vec<bool>,
    /// The op each stands for: a carry, the op whose value it carries; every other op, itself.
    pub(super) reps: Vec<usize>,
    /// What a writer held at each distinct frontier read so far, by its first word's place in
    /// the index and then its words.
    memo: HashMap<usize, HashMap<Box<[u64]>, Held>>,
    /// What a writer held at the frontier the last op read, for [`Mutant::ReuseFrontier`].
    #[cfg(test)]
    last: Option<Held>,
}

impl<'a> Register<'a> {
    fn new(cands: &'a [CandidateOp]) -> Self {
        let mut classes: BTreeMap<Writes<'a>, usize> = BTreeMap::new();
        let class = cands
            .iter()
            .map(|op| {
                let next = classes.len();
                *classes.entry(Writes::of(&op.op)).or_insert(next)
            })
            .collect();
        Self {
            cands,
            class,
            carries: vec![false; cands.len()],
            reps: (0..cands.len()).collect(),
            memo: HashMap::new(),
            #[cfg(test)]
            last: None,
        }
    }

    /// Classify the ops of each of `registers` (each register's ops in record order, no op in
    /// two) in one pass over `reach`, whose records' parents `parents` gives.
    pub(super) fn classify_all<P, I>(
        registers: &[&'a [CandidateOp]],
        reach: &Reach,
        parents: P,
    ) -> Vec<Self>
    where
        P: Fn(usize) -> I,
        I: IntoIterator<Item = usize>,
    {
        let mut all: Vec<Self> = registers.iter().map(|&cands| Self::new(cands)).collect();
        let groups: Vec<Vec<usize>> = registers
            .iter()
            .map(|cands| cands.iter().map(|op| op.index).collect())
            .collect();
        reach.frontiers(&groups, parents, |g, k, frontier| {
            all[g].classify(k, frontier, reach);
        });
        all
    }

    /// Classify op `k`, whose frontier in this register is `frontier`: what its writer held is
    /// that frontier's live ops, a `Put` is a carry when the winner among them writes what it
    /// writes, and a `Delete` when every one of them is a `Delete`. What the writer held depends
    /// on the frontier alone, since its ops were classified before it, so an op that reads a
    /// frontier an earlier op read takes what that one held. Looking a frontier up hashes its
    /// words and compares them once.
    fn classify(&mut self, k: usize, frontier: &Frontier<'_>, reach: &Reach) {
        let (start, words) = frontier.words();
        tally(2 * words.len(), 0, 0);
        let known = self
            .memo
            .get(&start)
            .and_then(|by_words| by_words.get(words))
            .copied();
        #[cfg(test)]
        let known = if mutant(Mutant::ReuseFrontier) {
            self.last.or(known)
        } else {
            known
        };
        let seen = known.unwrap_or_else(|| {
            let held = self.live(&frontier.positions(), reach);
            let deletes = !held.is_empty()
                && held
                    .iter()
                    .all(|&h| matches!(self.cands[h].op, DecodedOp::Delete));
            tally(words.len(), 0, held.len());
            let seen = Held {
                winner: self.winner(&held),
                deletes,
            };
            #[cfg(test)]
            if mutant(Mutant::RememberLast) {
                self.memo.clear();
            }
            self.memo
                .entry(start)
                .or_default()
                .insert(words.into(), seen);
            seen
        });
        #[cfg(test)]
        {
            self.last = Some(seen);
        }
        let carried = match self.cands[k].op {
            DecodedOp::Put(_) => seen.winner.filter(|&w| self.class[w] == self.class[k]),
            DecodedOp::Delete => seen.winner.filter(|_| seen.deletes),
        };
        if let Some(w) = carried {
            self.carries[k] = true;
            self.reps[k] = self.reps[w];
        }
    }

    /// `ops` less each carry whose value another of them, writing something else, already
    /// replaced: a reseal on one side of a split loses to an edit or `Delete` on the other.
    ///
    /// A carry's rep lies in the carry's past and writes what the carry does, so it lies in the
    /// past of an op of `ops` of the carry's class; it is replaced exactly when it also lies in
    /// the past of one of another class. So the ops are taken class by class, ORing their rows
    /// over just the words that hold a carry's rep, and a rep found under two classes drops its
    /// carries.
    fn live(&self, ops: &[usize], reach: &Reach) -> Vec<usize> {
        let cands = self.cands;
        tally(0, 0, ops.len());
        let reps: Vec<usize> = ops
            .iter()
            .filter(|&&c| self.carries[c])
            .map(|&c| cands[self.reps[c]].index)
            .collect();
        #[cfg(test)]
        if mutant(Mutant::KeepEveryCarry) {
            return ops.to_vec();
        }
        let Some(&first) = ops.first() else {
            return Vec::new();
        };
        if reps.is_empty() || ops.iter().all(|&o| self.class[o] == self.class[first]) {
            return ops.to_vec();
        }
        let mut words: Vec<usize> = reps.iter().map(|r| r / 64).collect();
        #[cfg(test)]
        if mutant(Mutant::FullRows) {
            let top = ops.iter().map(|&o| cands[o].index / 64).max().unwrap_or(0);
            words = (0..=top).collect();
        }
        words.sort_unstable();
        words.dedup();
        let mut by_class = ops.to_vec();
        by_class.sort_by_key(|&o| self.class[o]);
        let mut once = vec![0u64; words.len()];
        let mut twice = vec![0u64; words.len()];
        for same in by_class.chunk_by(|&a, &b| self.class[a] == self.class[b]) {
            let mut mine = vec![0u64; words.len()];
            for &s in same {
                let row = reach.row(cands[s].index);
                tally(words.len(), 0, 0);
                for (word, &w) in mine.iter_mut().zip(&words) {
                    if let Some(more) = row.get(w) {
                        *word |= more;
                    }
                }
            }
            tally(words.len(), 0, 0);
            for ((two, one), more) in twice.iter_mut().zip(once.iter_mut()).zip(&mine) {
                *two |= *one & more;
                *one |= more;
            }
        }
        ops.iter()
            .copied()
            .filter(|&c| {
                !self.carries[c] || {
                    let rep = cands[self.reps[c]].index;
                    let slot = words
                        .binary_search(&(rep / 64))
                        .expect("a carry's rep has its word");
                    (twice[slot] >> (rep % 64)) & 1 == 0
                }
            })
            .collect()
    }

    /// The op every Mac writes among `siblings`: the lowest id of what each stands for.
    pub(super) fn winner(&self, siblings: &[usize]) -> Option<usize> {
        siblings
            .iter()
            .copied()
            .min_by_key(|&i| (self.cands[self.reps[i]].record_id, self.cands[i].record_id))
    }

    /// The register's siblings. An op replaces what lies in its causal past, except that a carry
    /// replaces only ops writing the value it carries: a reseal after two concurrent edits keeps
    /// both as candidates instead of settling on one.
    ///
    /// What the ops that are not carries replace is the union of their rows, and what the
    /// carries of one class replace, among that class's ops, is the union of theirs.
    pub(super) fn siblings(&self, reach: &Reach) -> Vec<usize> {
        let cands = self.cands;
        let (Some(first), Some(last)) = (cands.first(), cands.last()) else {
            return Vec::new();
        };
        let n = cands.len();
        let base = first.index / 64;
        let writers = pasts(
            cands,
            (0..n).rev().filter(|&b| !self.carries[b]),
            reach,
            base,
            last.index / 64 + 1 - base,
        );
        tally(0, n, 0);
        let mut replaced: Vec<bool> = cands
            .iter()
            .map(|op| holds(&writers, base, op.index))
            .collect();
        let mut by_class: Vec<usize> = (0..n).collect();
        by_class.sort_by_key(|&o| (self.class[o], o));
        for same in by_class.chunk_by(|&a, &b| self.class[a] == self.class[b]) {
            if !same.iter().any(|&o| self.carries[o]) {
                continue;
            }
            let lo = cands[same[0]].index / 64;
            let hi = cands[same[same.len() - 1]].index / 64;
            let carried = pasts(
                cands,
                same.iter().rev().copied().filter(|&o| self.carries[o]),
                reach,
                lo,
                hi + 1 - lo,
            );
            tally(0, same.len(), 0);
            for &a in same {
                replaced[a] |= holds(&carried, lo, cands[a].index);
            }
        }
        let maximal: Vec<usize> = (0..n).filter(|&a| !replaced[a]).collect();
        self.live(&maximal, reach)
    }
}

/// The union of the strict pasts of the ops `of` (positions, newest first), as `len` words from
/// the index's word `base` on: an op already in it lies in a newer one's past, so its row adds
/// nothing and is not read.
fn pasts(
    cands: &[CandidateOp],
    of: impl Iterator<Item = usize>,
    reach: &Reach,
    base: usize,
    len: usize,
) -> Vec<u64> {
    let mut set = vec![0; len];
    for b in of {
        let at = cands[b].index;
        tally(0, 1, 0);
        if holds(&set, base, at) {
            continue;
        }
        if let Some(past) = reach.row(at).get(base..) {
            tally(past.len(), 0, 0);
            for (word, more) in set.iter_mut().zip(past) {
                *word |= more;
            }
        }
    }
    set
}

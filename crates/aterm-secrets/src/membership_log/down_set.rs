// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A down-set of a log, found by the digest of its heads ([`Log::down_set_named`]).
//!
//! A Mac keeps the heads digest of the log it last applied ([`Log::heads_digest`]) and nothing
//! else of it. Whether the log it holds now still holds that one whole is then a question about
//! a digest: is there a set of held records, closed under parents, whose heads hash to it? The
//! search here answers yes only by exhibiting those heads.
//!
//! The order a log holds its records in is no help: it is one topological order of many, a
//! reload sorts concurrent records by id ([`super::store::causal`]), and so the records a Mac
//! held when it applied are almost never the first so many of the records it holds later. The
//! candidates are therefore built from the graph alone, cheapest and likeliest first:
//!
//! 1. the log's own heads;
//! 2. the newest of the records a caller names (an envelope's stamp);
//! 3. every single held record, newest first: a log one Mac was writing has one head;
//! 4. the newest of each held record's parents ([`Search::parents`]): a record follows the heads
//!    its signer held ([`Log::draft`]), so these are the heads of a log some Mac held, however
//!    many they were and however long ago;
//! 5. the log less the records that came after, fewest first ([`Search::later`]): what this
//!    Mac's intake left when a few records landed after its last apply, however many heads the
//!    log had then;
//! 6. antichains of two records and more, smallest first ([`Search::antichains`]): an old log
//!    with few heads, under any number of later records.
//!
//! **A rooted log** (one pruned at a final checkpoint, [`super::rooted`]) knows records it does
//! not hold: those a root dropped, of which it keeps a stub ([`Log::knows`]). A log some Mac
//! held before the prune may have one as a head, so the hint names a dropped record where the
//! log knows each it names (2), and after the parents (4) come each dropped record alone and
//! the newest of each record's parents where one of them is dropped ([`Search::dropped`]). The
//! walks of (5) and (6) take held records alone, so a search of a rooted log is never complete:
//! one that names nothing says nothing of whether the log holds the down-set.
//!
//! Work: the search stops at its budget, which counts what it does and not how many candidates
//! it hashes, since a candidate of a wide log is long and the walk between two of them is not
//! free. One unit is 64 bytes of it:
//!
//! - one for each 64-byte block SHA-256 hashes of a candidate ([`blocks`]: one for a single
//!   head, `ceil((14 + 32 * k) / 64)` for `k` heads);
//! - one, and one more for each 8 words, for each read or union of reach-index rows
//!   ([`rows`]): the scan and the row union of each step of [`Search::antichains`], and the
//!   rows the newest of a record's parents or of the hint take;
//! - two for each record [`Search::later`] takes and puts back, two more for each parent it
//!   had, and [`rows`] of a word for each head there was and each of those parents
//!   ([`Peel::step`]), whether the heads left are hashed or not.
//!
//! Each step is charged before it is made, and a step the budget does not hold ends the
//! search, so a search of budget `b` over `n` records does at most `b` units of that work,
//! however many heads the log has or had. Outside the budget it does work linear in the log
//! alone: one pass over the records and their parents ([`Peel::of`], and
//! [`Search::parents`]' skip of records with one parent), and one causal closure where
//! [`Search::later`] names a down-set.

use super::{HEADS_DIGEST_MAGIC, Log, RecordId, heads_digest_into};
use std::collections::BTreeSet;
use std::ops::ControlFlow;

/// Which candidate of [`Log::down_set_search`] named the down-set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedBy {
    /// The log's own heads: the down-set is the whole log.
    Heads,
    /// The newest of the records the caller named.
    Hint,
    /// One held record.
    Record,
    /// The newest of one held record's parents: the heads its signer held.
    Parents,
    /// A record a root of the log dropped, or the newest of a record's parents, one of them
    /// dropped ([`Search::dropped`]).
    Dropped,
    /// The log less this many records that came after the down-set.
    Later(usize),
    /// An antichain of this many held records.
    Antichain(usize),
}

/// What [`Log::down_set_search`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownSetSearch {
    /// The sorted heads of the down-set the digest names, and the candidate that named them.
    pub named: Option<(Vec<RecordId>, NamedBy)>,
    /// How many candidates were hashed.
    pub tried: usize,
    /// The units of work done, which the budget bounds (the module's list).
    pub work: usize,
    /// Whether the search hashed every down-set of the log: with nothing named, the log holds
    /// no down-set of that digest. `false` where it stopped at its budget, and for a rooted log,
    /// whose down-sets with a dropped head the walks do not take (the module's list).
    pub complete: bool,
}

/// The 64-byte blocks SHA-256 hashes of a candidate of `heads` heads: the magic, the ids, and
/// the padding's one byte and eight of length.
const fn blocks(heads: usize) -> usize {
    (HEADS_DIGEST_MAGIC.len() + 32 * heads + 9).div_ceil(64)
}

/// The units one read or union of `words` reach-index words costs: one, and one more for each
/// 8 words (64 bytes).
const fn rows(words: usize) -> usize {
    1 + words / 8
}

/// How a step of the search ended.
enum End {
    /// A candidate's digest matched: its sorted heads.
    Named(Vec<RecordId>),
    /// The budget does not hold the next step.
    Spent,
}

struct Search<'a> {
    log: &'a Log,
    digest: &'a [u8; 32],
    /// The most units of work this search does.
    budget: usize,
    /// The units charged so far.
    work: usize,
    tried: usize,
    /// The bytes of the candidate being hashed, kept between candidates.
    bytes: Vec<u8>,
}

impl Search<'_> {
    /// Charge `units` of work before it is done: `Break` where the budget does not hold them.
    fn spend(&mut self, units: usize) -> ControlFlow<End> {
        if units > self.budget.saturating_sub(self.work) {
            return ControlFlow::Break(End::Spent);
        }
        self.work += units;
        ControlFlow::Continue(())
    }

    /// The newest of the held records at `at` (any order, repeats allowed), by id, charged as
    /// one row of the newest of them for each ([`super::reach::Reach::maximal`] unions at most
    /// that).
    fn newest(&mut self, mut at: Vec<usize>) -> ControlFlow<End, BTreeSet<RecordId>> {
        let log = self.log;
        at.sort_unstable();
        at.dedup();
        let words = at.last().map_or(0, |hi| hi / 64 + 1);
        self.spend(at.len() * rows(words))?;
        ControlFlow::Continue(
            log.reach
                .maximal(&at)
                .into_iter()
                .map(|at| *log.order[at].id())
                .collect(),
        )
    }

    /// Hash `heads`, which are sorted: `Break` where they are named or the budget does not
    /// hold their blocks.
    fn hash<'h, I>(&mut self, heads: I) -> ControlFlow<End>
    where
        I: IntoIterator<Item = &'h RecordId>,
        I::IntoIter: ExactSizeIterator,
    {
        let heads = heads.into_iter();
        self.spend(blocks(heads.len()))?;
        self.tried += 1;
        if heads_digest_into(&mut self.bytes, heads) == *self.digest {
            let named = self.bytes[HEADS_DIGEST_MAGIC.len()..]
                .as_chunks::<32>()
                .0
                .to_vec();
            return ControlFlow::Break(End::Named(named));
        }
        ControlFlow::Continue(())
    }

    /// The newest of `ids`, records the log holds or a root of it dropped ([`Log::knows`]):
    /// those in no other's past, by id, charged one unit for each pair compared.
    fn newest_known(&mut self, ids: &[RecordId]) -> ControlFlow<End, BTreeSet<RecordId>> {
        let log = self.log;
        let ids: BTreeSet<RecordId> = ids.iter().copied().collect();
        self.spend(ids.len().saturating_mul(ids.len()))?;
        ControlFlow::Continue(
            ids.iter()
                .filter(|a| !ids.iter().any(|b| log.lies_before(a, b)))
                .copied()
                .collect(),
        )
    }

    /// A rooted log's candidates of the records a root of it dropped: each one alone, the last
    /// the evaluators place first ([`Log::position`]), and then the newest of each record's
    /// parents, held or dropped, where they are two or more and one of them is dropped (a
    /// record follows the heads its signer held, which a prune may since have dropped).
    /// Nothing for a whole log.
    fn dropped(&mut self) -> ControlFlow<End> {
        let log = self.log;
        let Some(rooting) = log.rooting() else {
            return ControlFlow::Continue(());
        };
        let mut stubs: Vec<(usize, RecordId)> = rooting
            .stubs
            .keys()
            .filter_map(|id| Some((log.position(id)?, *id)))
            .collect();
        stubs.sort_unstable_by(|a, b| b.cmp(a));
        for (_, id) in &stubs {
            self.hash(std::slice::from_ref(id))?;
        }
        let held = log.order.iter().rev().map(|record| record.parents());
        let dropped = stubs
            .iter()
            .filter_map(|(_, id)| rooting.stubs.get(id))
            .map(|stub| stub.parents.as_slice());
        for parents in held.chain(dropped) {
            if parents.len() < 2 || !parents.iter().any(|parent| log.is_dropped(parent)) {
                continue;
            }
            let newest = self.newest_known(parents)?;
            if newest.len() >= 2 {
                self.hash(newest.iter())?;
            }
        }
        ControlFlow::Continue(())
    }

    /// Hash the newest of each held record's parents where they are two records or more (one is
    /// a single record, hashed before), those of the newest records first.
    fn parents(&mut self) -> ControlFlow<End> {
        let log = self.log;
        for (i, record) in log.order.iter().enumerate().rev() {
            if record.parents().len() < 2 {
                continue;
            }
            let newest = self.newest(log.placed_parents(i).collect())?;
            if newest.len() >= 2 {
                self.hash(newest.iter())?;
            }
        }
        ControlFlow::Continue(())
    }

    /// Hash every antichain of `size` held records, those of the newest records first; `Continue`
    /// says whether the log holds one of that size.
    ///
    /// An antichain is built from its newest record back: the records that may join it are the
    /// older ones in the past of none chosen so far, which is the complement of the union of
    /// their reach rows. (An older record is never in a newer one's future, so no second test
    /// is needed.)
    fn antichains(&mut self, size: usize) -> ControlFlow<End, bool> {
        let n = self.log.order.len();
        let mut any = false;
        let mut ids = Vec::with_capacity(size);
        let mut pasts = vec![Vec::new(); size + 1];
        self.extend(n, size, &mut ids, &mut pasts, &mut any)?;
        ControlFlow::Continue(any)
    }

    /// [`Search::antichains`]' step: `ids` holds the chosen records' ids, sorted; `pasts[more]`
    /// the union of their pasts over the records below `limit` (empty for none chosen); `more`
    /// records are still to choose, each below `limit`. Its scan of the records below `limit`
    /// and each union it makes for the next step are charged ([`rows`]), so the antichains
    /// walked on the way to those of `more` records, which are hashed in an earlier call and
    /// not here, are counted too.
    fn extend(
        &mut self,
        limit: usize,
        more: usize,
        ids: &mut Vec<RecordId>,
        pasts: &mut [Vec<u64>],
        any: &mut bool,
    ) -> ControlFlow<End> {
        self.spend(rows(limit.div_ceil(64)))?;
        let (below, here) = pasts.split_at_mut(more);
        let past = &here[0];
        for word in (0..limit.div_ceil(64)).rev() {
            let mut free = !past.get(word).copied().unwrap_or(0);
            if word == limit / 64 {
                free &= (1u64 << (limit % 64)) - 1;
            }
            while free != 0 {
                let bit = 63 - free.leading_zeros() as usize;
                free &= !(1u64 << bit);
                let at = word * 64 + bit;
                let id = *self.log.order[at].id();
                let place = ids.partition_point(|held| *held < id);
                ids.insert(place, id);
                if more == 1 {
                    *any = true;
                    self.hash(ids.iter())?;
                } else if at >= more - 1 {
                    // The records below `at` in no chosen record's past, `at`'s included.
                    let row = self.log.reach.row(at);
                    self.spend(rows(row.len()))?;
                    let next = &mut below[more - 1];
                    next.clear();
                    next.extend(
                        row.iter()
                            .enumerate()
                            .map(|(w, own)| own | past.get(w).copied().unwrap_or(0)),
                    );
                    self.extend(at, more - 1, ids, below, any)?;
                }
                ids.remove(place);
            }
        }
        ControlFlow::Continue(())
    }

    /// Hash the heads of the log less each set of `from..=to` records that could have come after
    /// the rest: a set closed under children. `Continue` says whether the log holds such a set
    /// of `to` records short of its genesis; with none, it holds none larger either.
    ///
    /// Each set is built once, its records taken newest first: every record's children are
    /// newer than it, so a set closed under children stays closed with its oldest records left
    /// out, and the record taken next is always a head of what is left. The walk keeps its own
    /// stack: a log of one writer is a chain as deep as it is long. Each record taken is
    /// charged for its taking and putting back and for the heads left then ([`Peel::step`]),
    /// whether or not they are hashed: a set smaller than `from` was hashed by an earlier call
    /// and is walked again here.
    fn later(&mut self, from: usize, to: usize) -> ControlFlow<End, bool> {
        let log = self.log;
        let mut peel = Peel::of(log);
        let mut any = false;
        // Each level: the heads still to take there, oldest first, and the record taken to
        // reach it.
        let mut levels = vec![(peel.heads_below(log.order.len()), None)];
        loop {
            let taken = levels.len();
            let Some((heads, _)) = levels.last_mut() else {
                break;
            };
            let Some(at) = heads.pop() else {
                if let Some((_, Some(back))) = levels.pop() {
                    peel.put_back(log, back);
                }
                continue;
            };
            self.spend(peel.step(at))?;
            peel.take(log, at);
            if taken >= from {
                self.hash(peel.ids.iter())?;
            }
            if taken == to {
                any = true;
                peel.put_back(log, at);
            } else {
                levels.push((peel.heads_below(at), Some(at)));
            }
        }
        ControlFlow::Continue(any)
    }
}

/// A log less the records [`Search::later`] took from it: the heads of what is left.
struct Peel {
    /// Each record's parents, by index.
    parents: Vec<Vec<u32>>,
    /// How many of each record's children are left.
    children: Vec<u32>,
    /// The heads of what is left, by index and by id.
    heads: BTreeSet<usize>,
    ids: BTreeSet<RecordId>,
}

impl Peel {
    fn of(log: &Log) -> Self {
        let mut children = vec![0u32; log.order.len()];
        let parents: Vec<Vec<u32>> = log
            .order
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let mut at: Vec<u32> = log.placed_parents(i).map(|at| at as u32).collect();
                at.sort_unstable();
                at.dedup();
                for parent in &at {
                    children[*parent as usize] += 1;
                }
                at
            })
            .collect();
        Self {
            parents,
            children,
            heads: log.heads.iter().map(|id| log.held[id]).collect(),
            ids: log.heads.clone(),
        }
    }

    /// The units taking the head at `at` and putting it back cost ([`Search::later`]): two for
    /// the record, two for each parent it had (a head made and unmade), and [`rows`] of a word
    /// for each head there is now and each of those parents, which bounds the heads the level
    /// under it lists.
    fn step(&self, at: usize) -> usize {
        2 + 2 * self.parents[at].len() + rows(self.heads.len() + self.parents[at].len())
    }

    /// The heads of what is left below `limit`, oldest first, the genesis left out: every
    /// down-set holds it.
    fn heads_below(&self, limit: usize) -> Vec<usize> {
        self.heads.range(1..limit.max(1)).copied().collect()
    }

    /// Take the head at `at`: each parent with no child left becomes a head.
    fn take(&mut self, log: &Log, at: usize) {
        self.heads.remove(&at);
        self.ids.remove(log.order[at].id());
        for parent in &self.parents[at] {
            let parent = *parent as usize;
            self.children[parent] -= 1;
            if self.children[parent] == 0 {
                self.heads.insert(parent);
                self.ids.insert(*log.order[parent].id());
            }
        }
    }

    /// Undo [`Peel::take`] of the record at `at`.
    fn put_back(&mut self, log: &Log, at: usize) {
        for parent in &self.parents[at] {
            let parent = *parent as usize;
            if self.children[parent] == 0 {
                self.heads.remove(&parent);
                self.ids.remove(log.order[parent].id());
            }
            self.children[parent] += 1;
        }
        self.heads.insert(at);
        self.ids.insert(*log.order[at].id());
    }
}

impl Log {
    /// The sorted heads of a down-set of this log (a set of records it holds, or of a rooted
    /// log's records and those a root of it dropped, that holds each one's parents) whose heads
    /// digest is `digest`: SHA-256 of `ALHD1` and those heads, as
    /// [`Log::heads_digest`] hashes its own. `hint` names records the down-set is thought to
    /// hold (an envelope's stamp); it orders the search and proves nothing. `None` where no
    /// candidate matched within `budget` units of work (the module's list) past the log's own
    /// heads, which cost none.
    ///
    /// **What a match proves.** An equal digest is a SHA-256 preimage, so the heads returned
    /// are the heads the digest was taken over: those of the log some Mac held. Each is held
    /// here, or was dropped by a root of this log, which holds the final checkpoint whose cut
    /// held it; and a log takes a record only after its parents, so this log holds or dropped
    /// every record in their pasts: the whole of that log. This log is that one or a descendant
    /// of it, never a rollback, and nothing but the preimage is taken as proof of it.
    ///
    /// **Why this is no scan of the held order.** The order is one topological order of the
    /// records, kept only while one `Log` lives: every load sorts the records again, concurrent
    /// ones by id, and a whole rewrite of the file writes that order. The records a Mac held at
    /// some earlier time are a down-set of the graph and almost never the first so many records
    /// of a later order, so the candidates are built from the graph (the module's list).
    #[must_use]
    pub fn down_set_named(
        &self,
        digest: &[u8; 32],
        hint: &[RecordId],
        budget: usize,
    ) -> Option<Vec<RecordId>> {
        self.down_set_search(digest, hint, budget)
            .named
            .map(|(heads, _)| heads)
    }

    /// [`Log::down_set_named`], with which candidate named the down-set, how many were hashed,
    /// the work that took and whether every down-set of the log was hashed.
    #[must_use]
    pub fn down_set_search(
        &self,
        digest: &[u8; 32],
        hint: &[RecordId],
        budget: usize,
    ) -> DownSetSearch {
        if self.heads_digest() == *digest {
            return DownSetSearch {
                named: Some((self.heads(), NamedBy::Heads)),
                tried: 0,
                work: 0,
                complete: false,
            };
        }
        let mut search = Search {
            log: self,
            digest,
            budget,
            work: 0,
            tried: 0,
            bytes: Vec::new(),
        };
        let (named, complete) = match search.candidates(hint) {
            ControlFlow::Break((End::Named(heads), by)) => (Some((heads, by)), false),
            ControlFlow::Break((End::Spent, _)) => (None, false),
            // A rooted log's down-sets with a dropped head are not all walked.
            ControlFlow::Continue(()) => (None, self.rooting().is_none()),
        };
        DownSetSearch {
            named,
            tried: search.tried,
            work: search.work,
            complete,
        }
    }
}

impl Search<'_> {
    /// Every candidate past the log's own heads, in the module's order: `Continue` once every
    /// down-set of the log was hashed.
    fn candidates(&mut self, hint: &[RecordId]) -> ControlFlow<(End, NamedBy)> {
        let by = |by: NamedBy| move |end: End| (end, by);
        let log = self.log;
        if !hint.is_empty() && hint.iter().all(|id| log.held.contains_key(id)) {
            let newest = self
                .newest(log.indexes(hint))
                .map_break(by(NamedBy::Hint))?;
            self.hash(newest.iter()).map_break(by(NamedBy::Hint))?;
        } else if !hint.is_empty() && hint.iter().all(|id| log.knows(id)) {
            // A rooted log's hint naming a record a root of it dropped.
            let newest = self.newest_known(hint).map_break(by(NamedBy::Hint))?;
            self.hash(newest.iter()).map_break(by(NamedBy::Hint))?;
        }
        self.antichains(1).map_break(by(NamedBy::Record))?;
        self.parents().map_break(by(NamedBy::Parents))?;
        self.dropped().map_break(by(NamedBy::Dropped))?;
        // Half of what is left for the records that came after, so a wide log's many ways of
        // taking a few records leave the small antichains their share.
        let whole = self.budget;
        self.budget = self.work + (whole - self.work.min(whole)) / 2;
        let (mut from, mut to) = (1, 1);
        let every_later = loop {
            match self.later(from, to) {
                ControlFlow::Break(End::Spent) => break false,
                ControlFlow::Break(end) => {
                    let named = match &end {
                        End::Named(heads) => {
                            let held: u32 = log
                                .reach
                                .closure(log.indexes(heads))
                                .iter()
                                .map(|word| word.count_ones())
                                .sum();
                            log.order.len() - held as usize
                        }
                        End::Spent => to,
                    };
                    return ControlFlow::Break((end, NamedBy::Later(named)));
                }
                ControlFlow::Continue(false) => break true,
                ControlFlow::Continue(true) => (from, to) = (to + 1, to * 2),
            }
        };
        if every_later {
            // Every set of records closed under children was taken: every down-set was hashed.
            return ControlFlow::Continue(());
        }
        self.budget = whole;
        let mut size = 2;
        loop {
            if !self
                .antichains(size)
                .map_break(by(NamedBy::Antichain(size)))?
            {
                return ControlFlow::Continue(());
            }
            size += 1;
        }
    }
}

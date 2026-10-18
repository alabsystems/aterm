// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE READER'S VIEW OF THE BUS (design §5.5, phase 4 ruling Q0): what a
//! consumer of a session's mirrored records holds, and the one place its
//! laws are stated. The bus itself may hold a body twice (a republish at a
//! new producer seq after a crash, an update or a collision): the view never
//! shows one twice.
//!
//! - **Records** are keyed by `(ep, seq)` within a session (the caller keys
//!   sessions by `(node, sid)`, and a space by its `ep`): a second body of a
//!   key is counted `dup` and dropped (ReaderViewAtMostOnce).
//! - **Covers** (`op=gap … n=<k>`) are coverage: the `ms` they cover are said
//!   lost, and two covers of one hole are one statement, so they are unioned,
//!   never deduped by key.
//! - **Snapshots** are seeds: the newest one is kept, and a fold starts from
//!   it when the view does not hold its space's first record, or holds a
//!   cover (records it will never see), so long as every cover lies at or
//!   below the snapshot's `upto=` ([`BusSpace::fold_seeded`]). A view that
//!   lost records no snapshot folds past is [`Seed::Lost`], never whole.
//! - **`ms` dense** (MsDense): every `ms` from 1 to the highest the view holds
//!   is a record's or inside a cover; [`BusSpace::holes`] lists any that is
//!   neither, which is loss nobody said (the broker's clamp, a retention).
//! - **Order** is `(ep, seq)`, never the bus's offsets: a slot re-pinned after
//!   a collision lands after later ones of its session (ruling Q3), and the
//!   fold applies it in its place. A body that arrived after a later seq of
//!   its space is counted `reordered`.
//! - **A body is the bus's, not the mirror's** until it reads as one: a
//!   record outside THIS BUILD'S bus class ([`Rec::is_bus_class`]: a label,
//!   a bell, a kind it does not know) is counted `foreign` and never folded,
//!   a key above the bus's tier (`text=`, `cwd=`, `title=`) is stripped, and
//!   a cover or an `ms` past any seq a log numbers ([`crate::SEQ_LIMIT`]) is
//!   foreign and dropped whole. Whoever can write a session's `ev` face can
//!   say what the bus class says, and no more.
//! - **Unknown is not hostile.** A fleet runs mixed builds, and a newer
//!   writer's bus class may be wider than this reader's (a carry is judged
//!   the same way, fixer F9), and its words newer (§3.4: a new word never
//!   changes `v`). A well-formed record this build calls local with an `ms`
//!   in range keeps that `ms` HELD — the view stays dense, so
//!   [`BusSpace::holes`] names only an `ms` nobody said — while nothing of it
//!   is folded. That holds for a kind, an operation or a node level this
//!   build does not know: [`crate::decode_bus`] reads such a body as a record
//!   it cannot place, which no bus class holds. Holding an `ms` says no more
//!   than a cover would, which any writer of the face can publish.

use std::collections::BTreeMap;

use crate::field::{Key, Tier};
use crate::fold::Fold;
use crate::line::BusRec;
use crate::rec::Rec;
use crate::vocab::Op;

/// Where a view's fold starts ([`BusSpace::fold_seeded`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seed {
    /// From the space's first record: the view holds it and lost nothing.
    Start,
    /// From the newest snapshot, every cover at or below its `upto=`.
    Snap,
    /// Partial: the view holds neither its start nor a snapshot.
    NoStart,
    /// Partial: the view lost records (a cover) that no snapshot it holds
    /// folds past.
    Lost,
}

impl Seed {
    /// Whether the fold is partial.
    #[must_use]
    pub const fn partial(self) -> bool {
        matches!(self, Seed::NoStart | Seed::Lost)
    }

    /// The `why=` word of a partial fold.
    #[must_use]
    pub const fn why(self) -> Option<&'static str> {
        match self {
            Seed::Start | Seed::Snap => None,
            Seed::NoStart => Some("no-start"),
            Seed::Lost => Some("lost"),
        }
    }
}

/// What a body offered to a [`BusSpace`] was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offered {
    /// A record the view did not hold.
    Record,
    /// A record of a key the view holds: dropped.
    Duplicate,
    /// A cover, unioned into the view's coverage.
    Cover,
    /// A snapshot.
    Snap,
    /// A body of another space: the caller's to route.
    OtherSpace,
    /// Not what this build's mirror publishes: a record outside its bus
    /// class (its `ms` held when in range, nothing of it folded: the module
    /// doc), or an `ms` or a cover past any seq a log numbers (dropped
    /// whole). Counted.
    Foreign,
}

/// One seq space of one session, as a reader holds it.
#[derive(Clone, Debug, Default)]
pub struct BusSpace {
    ep: u32,
    records: BTreeMap<u64, (u64, Rec)>,
    /// Records outside this build's bus class, by seq: each one's `ms`, held
    /// and never folded (the module doc: unknown is not hostile).
    unknown: BTreeMap<u64, u64>,
    covers: Vec<(u64, u64, &'static str)>,
    snap: Option<Rec>,
    /// Bodies of a key already held, dropped.
    pub dup: u64,
    /// Records that arrived after a later seq of their space.
    pub reordered: u64,
    /// Bodies that are not what a mirror publishes ([`Offered::Foreign`]).
    pub foreign: u64,
    newest_seq: u64,
}

impl BusSpace {
    /// An empty view of space `ep`.
    #[must_use]
    pub fn new(ep: u32) -> BusSpace {
        BusSpace {
            ep,
            ..BusSpace::default()
        }
    }

    /// The space.
    #[must_use]
    pub const fn ep(&self) -> u32 {
        self.ep
    }

    /// Take one decoded body into the view.
    pub fn offer(&mut self, b: BusRec) -> Offered {
        if b.ep != self.ep {
            return Offered::OtherSpace;
        }
        let mut r = b.rec;
        // What a mirror publishes, and no more (the module doc).
        let ms = b.ms.unwrap_or(0);
        if ms > crate::SEQ_LIMIT || r.seq > crate::SEQ_LIMIT {
            self.foreign += 1;
            return Offered::Foreign;
        }
        if !r.is_bus_class() {
            // A newer writer's bus class, or a forger's: its `ms` held (no
            // false hole), nothing of it folded.
            if ms == 0 {
                self.foreign += 1;
                return Offered::Foreign;
            }
            if self.holds_seq(r.seq) {
                self.dup += 1;
                return Offered::Duplicate;
            }
            self.foreign += 1;
            self.unknown.insert(r.seq, ms);
            return Offered::Foreign;
        }
        let above: Vec<Key> =
            r.f.iter()
                .map(|(k, _)| *k)
                .filter(|k| k.tier() > Tier::WireMeta)
                .collect();
        for k in above {
            r.f.remove(k);
        }
        if r.op == Op::Snap {
            if self.snap.as_ref().is_none_or(|s| s.seq <= r.seq) {
                self.snap = Some(r);
            }
            return Offered::Snap;
        }
        if r.op == Op::Gap && r.f.get_u(Key::N).is_some() {
            let n = r.f.get_u(Key::N).unwrap_or(0);
            if ms > 0 && n > 0 {
                if n > crate::SEQ_LIMIT - ms + 1 {
                    self.foreign += 1;
                    return Offered::Foreign;
                }
                self.covers
                    .push((ms, n, r.f.get_word(Key::Why).unwrap_or("unknown")));
            }
            return Offered::Cover;
        }
        if self.holds_seq(r.seq) {
            self.dup += 1;
            return Offered::Duplicate;
        }
        if r.seq < self.newest_seq {
            self.reordered += 1;
        }
        self.newest_seq = self.newest_seq.max(r.seq);
        self.records.insert(r.seq, (ms, r));
        Offered::Record
    }

    /// Whether a body of key `seq` is held (a record, or one outside this
    /// build's bus class).
    fn holds_seq(&self, seq: u64) -> bool {
        self.records.contains_key(&seq) || self.unknown.contains_key(&seq)
    }

    /// Every `ms` a body holds: the records' and those outside this build's
    /// bus class (never folded, still said).
    fn held_ms(&self) -> impl Iterator<Item = u64> + '_ {
        self.records
            .values()
            .map(|(ms, _)| *ms)
            .chain(self.unknown.values().copied())
            .filter(|ms| *ms > 0)
    }

    /// The records, in seq order.
    pub fn records(&self) -> impl Iterator<Item = &Rec> {
        self.records.values().map(|(_, r)| r)
    }

    /// The records with their `ms`, in seq order.
    pub fn records_ms(&self) -> impl Iterator<Item = (u64, &Rec)> {
        self.records.values().map(|(ms, r)| (*ms, r))
    }

    /// How many records it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The covers, as they arrived: first `ms`, count, why.
    #[must_use]
    pub fn covers(&self) -> &[(u64, u64, &'static str)] {
        &self.covers
    }

    /// The newest snapshot held.
    #[must_use]
    pub const fn snap(&self) -> Option<&Rec> {
        self.snap.as_ref()
    }

    /// Each cover's `[lo, hi]` (an offered cover's `hi` is within
    /// [`crate::SEQ_LIMIT`]; saturating all the same).
    fn spans(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.covers
            .iter()
            .map(|(lo, n, _)| (*lo, lo.saturating_add(n.saturating_sub(1))))
    }

    /// The highest `ms` a body or a cover of the view holds.
    #[must_use]
    pub fn max_ms(&self) -> u64 {
        let rec = self.held_ms().max().unwrap_or(0);
        let cov = self.spans().map(|(_, hi)| hi).max().unwrap_or(0);
        rec.max(cov)
    }

    /// The lowest `ms` a body or a cover of the view holds (0: none): where
    /// a view that does not hold its space's start begins.
    #[must_use]
    pub fn min_ms(&self) -> u64 {
        let rec = self.held_ms().min();
        let cov = self.spans().map(|(lo, _)| lo).min();
        match (rec, cov) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => a.or(b).unwrap_or(0),
        }
    }

    /// The `ms` covers say lost, unioned (each counted once).
    #[must_use]
    pub fn covered(&self) -> u64 {
        let mut spans: Vec<(u64, u64)> = self.spans().collect();
        spans.sort_unstable();
        let mut total = 0;
        let mut upto: u64 = 0;
        for (a, b) in spans {
            let a = a.max(upto.saturating_add(1));
            if b >= a {
                total += b - a + 1;
                upto = b;
            }
            if upto == u64::MAX {
                break;
            }
        }
        total
    }

    /// THE HOLES (MsDense): the `ms` ranges from `from` up to [`Self::max_ms`]
    /// that neither a body nor a cover holds — loss nobody said. Empty for a
    /// view that holds its space whole from `from`.
    #[must_use]
    pub fn holes_from(&self, from: u64) -> Vec<(u64, u64)> {
        let mut held: Vec<(u64, u64)> = self
            .spans()
            .chain(self.held_ms().map(|ms| (ms, ms)))
            .collect();
        held.sort_unstable();
        let mut out = Vec::new();
        let mut next = from.max(1);
        for (a, b) in held {
            if b < next {
                continue;
            }
            if a > next {
                out.push((next, a - 1));
            }
            next = next.max(b.saturating_add(1));
        }
        out
    }

    /// [`Self::holes_from`] 1: the space whole from its first `ms`.
    #[must_use]
    pub fn holes(&self) -> Vec<(u64, u64)> {
        self.holes_from(1)
    }

    /// Whether the view holds its space's first bus-class record (or a cover
    /// of it): a fold of it starts where the space did.
    #[must_use]
    pub fn holds_start(&self) -> bool {
        self.held_ms().any(|ms| ms == 1) || self.covers.iter().any(|(lo, ..)| *lo == 1)
    }

    /// [`Self::fold_seeded`], and whether the fold is partial.
    #[must_use]
    pub fn fold(&self) -> (Fold, bool) {
        let (fold, seed) = self.fold_seeded();
        (fold, seed.partial())
    }

    /// THE FOLD a reader shows, and where it started. A view that lost
    /// nothing (no cover) folds from the space's first record when it holds
    /// it, else from the newest snapshot (applying only the records after
    /// it). A view that lost records — a cover: a claim or a failed run's
    /// close among them perhaps — is whole only past a snapshot whose
    /// `upto=` reaches every cover: the snapshot's open needs stand for what
    /// the lost records raised and lifted ([`Fold::reseed_needs`], over the
    /// held records' totals and close when the view holds its start). Short
    /// of one it folds what it holds, which the fold tolerates as a late
    /// join, and is partial: [`Seed::Lost`]; [`Seed::NoStart`] for a view
    /// that merely began late. Records apply in seq order.
    #[must_use]
    pub fn fold_seeded(&self) -> (Fold, Seed) {
        let mut fold = Fold::new(self.ep);
        let lost_to = self.spans().map(|(_, hi)| hi).max();
        let seeds = self.snap.as_ref().filter(|snap| {
            lost_to.is_none_or(|hi| snap.f.get_u(Key::Upto).is_some_and(|upto| upto >= hi))
        });
        let apply = |fold: &mut Fold, from: u64, to: u64| {
            for (_, r) in self.records.range(from..=to).map(|(_, v)| v) {
                fold.apply(r);
            }
        };
        let seed = match (lost_to, seeds) {
            (None, _) if self.holds_start() => {
                apply(&mut fold, 0, u64::MAX);
                Seed::Start
            }
            (Some(_), Some(snap)) if self.holds_start() => {
                apply(&mut fold, 0, snap.seq);
                fold.reseed_needs(snap);
                apply(&mut fold, snap.seq.saturating_add(1), u64::MAX);
                Seed::Snap
            }
            (_, Some(snap)) => {
                fold.seed_snap(snap);
                apply(&mut fold, snap.seq.saturating_add(1), u64::MAX);
                Seed::Snap
            }
            (None, None) => {
                apply(&mut fold, 0, u64::MAX);
                Seed::NoStart
            }
            (Some(_), None) => {
                apply(&mut fold, 0, u64::MAX);
                Seed::Lost
            }
        };
        (fold, seed)
    }
}

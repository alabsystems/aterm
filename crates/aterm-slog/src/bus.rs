// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE READER'S VIEW OF THE BUS (design §5.5).

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
    /// Seeded from a snapshot whose original need count is absent or differs from the needs.
    SnapPartial,
    /// Partial: the view holds neither its start nor a snapshot.
    NoStart,
    /// Partial: the view lost records (a cover) that no snapshot it holds folds past.
    Lost,
}

impl Seed {
    /// Whether the fold is partial.
    #[must_use]
    pub const fn partial(self) -> bool {
        matches!(self, Seed::SnapPartial | Seed::NoStart | Seed::Lost)
    }

    /// The `why=` word of a partial fold.
    #[must_use]
    pub const fn why(self) -> Option<&'static str> {
        match self {
            Seed::Start | Seed::Snap => None,
            Seed::SnapPartial => Some("snapshot-needs"),
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
    /// Not what this build's mirror publishes.
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
        let ms = b.ms.unwrap_or(0);
        if ms > crate::SEQ_LIMIT || r.seq > crate::SEQ_LIMIT {
            self.foreign += 1;
            return Offered::Foreign;
        }
        if !r.is_bus_class() {
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

    /// THE HOLES (MsDense).
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

    /// Whether the view holds its space's first bus-class record (or a cover of it).
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

    /// THE FOLD a reader shows, and where it started.
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
        let snap_seed = |fold: &Fold, snap: &Rec| {
            let held = fold.state().needs.iter().flatten().count() as u64;
            if snap.f.get_u(Key::N) == Some(held) {
                Seed::Snap
            } else {
                Seed::SnapPartial
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
                let seed = snap_seed(&fold, snap);
                apply(&mut fold, snap.seq.saturating_add(1), u64::MAX);
                seed
            }
            (_, Some(snap)) => {
                fold.seed_snap(snap);
                let seed = snap_seed(&fold, snap);
                apply(&mut fold, snap.seq.saturating_add(1), u64::MAX);
                seed
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

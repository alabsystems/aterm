// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Journal snapshot, clean-quit close generation, and read-only log reconstruction.

use super::{CarryError, GapWhy, Limits, Log, MAX_HOLES, MAX_OPEN, OpenEntry, Rollup, level_slot};
use crate::Close;
use crate::field::{Fields, Key, Val};
use crate::rec::{Rec, Stamp};
use crate::vocab::{Actor, Conf, Kind, Lvl, Node, Op, Src};
use crate::words::{Out, SessionOut};

/// Maximum records a [`JournalImage`] may pass to [`Log::from_journal`].
pub const JOURNAL_IMAGE_MAX_RECORDS: usize = 1 << 20;

/// Gathered records and gap metadata for one session's seq space (`Log::from_journal`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalImage {
    /// The newest epoch the journal names for the space.
    pub ep: u32,
    /// The space's base epoch (`Log::base_ep`): with the sid, its key.
    pub base: u32,
    /// The first seq the space accounts for (`Log::first_seq`).
    pub first: u64,
    /// Newest seq accounted for by records, gap frames, and overlaps.
    pub head: u64,
    /// The records, each seq once, in seq order.
    pub recs: Vec<Rec>,
    /// Disjoint inclusive seq ranges not held by the image, in order, with their [`GapWhy`].
    pub holes: Vec<(u64, u64, GapWhy)>,
}

impl Log {
    /// Returns the session's open record and each open node's open record, outermost first.
    #[must_use]
    pub fn open_path_records(&self) -> Vec<Rec> {
        let mut out: Vec<Rec> = self
            .ring
            .iter()
            .find(|e| e.rec.node == Node::SESSION && e.rec.op == Op::Open)
            .map(|e| Rec::clone(&e.rec))
            .into_iter()
            .collect();
        if self.session_closed {
            return out;
        }
        for o in &self.open {
            if let Some(e) = self
                .ring
                .iter()
                .rev()
                .find(|e| e.rec.node == o.node && e.rec.op == Op::Open)
            {
                out.push(Rec::clone(&e.rec));
            }
        }
        out
    }

    /// Returns the close records a clean quit at `at` would emit, without mutating `self`.
    #[must_use]
    pub fn quit_closes(&self, at: Stamp) -> Vec<Rec> {
        if self.session_closed {
            return Vec::new();
        }
        let mut copy = self.clone();
        copy.notify = None;
        copy.fence = None;
        let head = copy.last_seq;
        let close =
            Close::new(Out::Session(SessionOut::AppQuit), Src::Aterm, Conf::Exact).by(Actor::Aterm);
        if copy.close_session(close, at).is_err() {
            return Vec::new();
        }
        copy.ring
            .iter()
            .filter(|e| e.rec.seq > head)
            .map(|e| Rec::clone(&e.rec))
            .collect()
    }

    /// Appends a `gap` record whose last known seq is `after` (`why=<why> after=<seq>`), returning its seq.
    pub fn gap_after(&mut self, why: GapWhy, after: u64, at: Stamp) -> u64 {
        let mut f = Fields::new();
        let _ = f.set(Key::Why, Val::Word(why.word()));
        let _ = f.set(Key::After, Val::U(after));
        self.push(
            Rec {
                seq: 0,
                t: at.wall_ms,
                op: Op::Gap,
                node: Node::SESSION,
                par: None,
                k: Kind::Gap,
                src: Src::Aterm,
                conf: Conf::Exact,
                by: Actor::Aterm,
                f,
            },
            at.mono_ms,
        )
    }

    /// Rebuilds a read-only [`Log`] by replaying `img`'s records and recording its gaps and losses.
    ///
    /// # Errors
    ///
    /// A [`CarryError`] for an image that does not hold together: epochs out
    /// of order, a seq past [`super::SEQ_LIMIT`] or the head, records out of
    /// order or in a range never held, overlaps out of order, or more records
    /// than [`JOURNAL_IMAGE_MAX_RECORDS`].
    pub fn from_journal(img: &JournalImage) -> Result<Log, CarryError> {
        if img.recs.len() > JOURNAL_IMAGE_MAX_RECORDS {
            return Err(CarryError::Bound("more journal records than a read holds"));
        }
        if img.base > img.ep || img.first == 0 || img.head > super::SEQ_LIMIT {
            return Err(CarryError::Shape("a journal image's epochs or seqs"));
        }
        let mut last = 0u64;
        for r in &img.recs {
            if r.seq == 0 || r.seq <= last || r.seq > img.head {
                return Err(CarryError::Record("journal records out of seq order"));
            }
            last = r.seq;
        }
        let mut prev_hi = 0u64;
        for &(lo, hi, _) in &img.holes {
            if lo == 0 || lo > hi || hi > img.head || (prev_hi > 0 && lo <= prev_hi) {
                return Err(CarryError::Shape("a journal image's holes out of order"));
            }
            prev_hi = hi;
        }
        if img.recs.iter().any(|r| {
            img.holes
                .iter()
                .any(|&(lo, hi, _)| lo <= r.seq && r.seq <= hi)
        }) {
            return Err(CarryError::Record("a journal record in a range never held"));
        }
        let room = img.recs.len().saturating_add(MAX_OPEN + 8);
        let limits = Limits::new(
            room.max(Limits::SESSION.records),
            room.saturating_mul(crate::line::MAX_RECORD_BYTES)
                .max(Limits::SESSION.bytes),
        )
        .unwrap_or(Limits::SESSION);
        let mut log = Log::with_limits(img.ep, limits);
        log.base_ep = img.base;
        log.first_seq = img.first;
        log.holes = img.holes.iter().copied().collect();
        while log.holes.len() > MAX_HOLES {
            if let Some((_, hi, _)) = log.holes.pop_front() {
                log.first_seq = log.first_seq.max(hi.saturating_add(1));
            }
        }
        let mut prev = 0u64;
        for rec in &img.recs {
            log.count_missing(prev, rec.seq - 1);
            log.replay(rec.clone());
            prev = rec.seq;
        }
        log.count_missing(prev, img.head);
        log.last_seq = log.last_seq.max(img.head);
        Ok(log)
    }

    fn count_missing(&mut self, lo: u64, hi: u64) {
        let missing = self.held_between(lo, hi);
        if missing > 0 {
            self.lost = self.lost.saturating_add(missing);
            self.newest_lost = self.newest_lost.max(hi);
        }
    }

    fn replay(&mut self, rec: Rec) {
        for n in [Some(rec.node), rec.par].into_iter().flatten() {
            if let Some(i) = level_slot(n.lvl) {
                self.issued[i] = self.issued[i].max(n.n);
            }
        }
        match (rec.op, rec.node == Node::SESSION) {
            (Op::Open, true) => {
                if !self.session_opened && !self.session_closed {
                    self.session_opened = true;
                    self.session_kind = rec.k;
                    self.session_t = rec.t;
                    self.session_mono = 0;
                    self.session_wall_clock = true;
                }
            }
            (Op::Open, false) => {
                let lvl_ok = matches!(rec.node.lvl, Lvl::Run | Lvl::Turn | Lvl::Step);
                if lvl_ok
                    && !self.session_closed
                    && !self.is_open(rec.node)
                    && self.open.len() < MAX_OPEN
                {
                    let parent = rec
                        .par
                        .filter(|p| *p == Node::SESSION || self.is_open(*p))
                        .unwrap_or(Node::SESSION);
                    self.open.push(OpenEntry {
                        node: rec.node,
                        kind: rec.k,
                        parent,
                        t: rec.t,
                        mono: 0,
                        orphan: rec.f.get(Key::Orphan).is_some(),
                        roll: Rollup::default(),
                        wall_clock: true,
                    });
                }
            }
            (Op::Close, true) => {
                let open: Vec<Node> = self.open.iter().rev().map(|e| e.node).collect();
                for n in open {
                    self.forget_open(n, rec.seq);
                }
                self.session_closed = true;
                self.index_close(Node::SESSION, rec.seq);
            }
            (Op::Close, false) => {
                let under: Vec<Node> = self
                    .open
                    .iter()
                    .rev()
                    .filter(|e| self.descends_from(e.node, rec.node))
                    .map(|e| e.node)
                    .collect();
                for n in under {
                    self.forget_open(n, rec.seq);
                }
                self.forget_open(rec.node, rec.seq);
            }
            _ => {}
        }
        self.last_seq = rec.seq.saturating_sub(1);
        self.push(rec, 0);
    }

    fn forget_open(&mut self, n: Node, seq: u64) {
        let Some(i) = self.open.iter().position(|e| e.node == n) else {
            return;
        };
        let e = self.open.remove(i);
        self.closed_parents.push_back((n, e.parent));
        if self.closed_parents.len() > super::CLOSED_PARENTS {
            self.closed_parents.pop_front();
        }
        self.index_close(n, seq);
    }
}

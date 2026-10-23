// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The instance's node log (design §2.1 "node", §2.6 "Per instance"; phase 5 sub-phase 5J).

use std::collections::VecDeque;

use crate::field::{Fields, Tier};
use crate::line::encoded_size;
use crate::log::{Cursor, Gap, GapWhy, Mark, Read, Refused, log_owned};
use crate::rec::{Rec, Stamp};
use crate::vocab::{Actor, Conf, Kind, Node, Op, Src};

/// The node log's bound: records, oldest evicted first (DESIGN §2.6).
pub const NODE_LOG_RECORDS: usize = 1024;

/// The mark kinds the node log takes: the fabric link's state and a config key that changed.
pub const NODE_MARKS: &[Kind] = &[Kind::Link, Kind::Config];

/// The instance's node log: a bounded FIFO ring of node `n`'s records with exact gap counts.
#[derive(Clone, Debug)]
pub struct NodeLog {
    ep: u32,
    cap: usize,
    ring: VecDeque<Rec>,
    last_seq: u64,
    lost: u64,
}

impl NodeLog {
    /// An empty node log of epoch `ep`, bounded by [`NODE_LOG_RECORDS`].
    #[must_use]
    pub fn new(ep: u32) -> NodeLog {
        NodeLog::bounded(ep, NODE_LOG_RECORDS)
    }

    /// An empty node log of epoch `ep` bounded by `records`: the models' and tests' small rings.
    #[must_use]
    pub fn bounded(ep: u32, records: usize) -> NodeLog {
        NodeLog {
            ep,
            cap: records.max(1),
            ring: VecDeque::new(),
            last_seq: 0,
            lost: 0,
        }
    }

    /// The epoch every read's `next` names.
    #[must_use]
    pub const fn ep(&self) -> u32 {
        self.ep
    }

    /// The bound, in records.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.cap
    }

    /// The last seq assigned (0: none).
    #[must_use]
    pub const fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// The records evicted so far.
    #[must_use]
    pub const fn lost(&self) -> u64 {
        self.lost
    }

    /// The oldest live seq (`last_seq + 1` when the ring is empty).
    #[must_use]
    pub fn oldest_seq(&self) -> u64 {
        self.ring.front().map_or(self.last_seq + 1, |r| r.seq)
    }

    /// The live records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether no record is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// The live records, oldest first.
    pub fn window(&self) -> impl DoubleEndedIterator<Item = &Rec> + ExactSizeIterator {
        self.ring.iter()
    }

    /// Whether the start record was written.
    #[must_use]
    pub const fn started(&self) -> bool {
        self.last_seq > 0
    }

    /// Records after `cursor`, at most `max` of them, and the gap the read crossed.
    #[must_use]
    pub fn read(&self, cursor: Cursor, max: usize) -> Read<'_> {
        let head = self.last_seq;
        let reset = cursor.seq > head || (cursor.ep != self.ep && cursor.seq > 0);
        let from = if reset { 0 } else { cursor.seq };
        let start = self.ring.partition_point(|r| r.seq <= from);
        let recs: Vec<&Rec> = self.ring.range(start..).take(max).collect();
        let next = match (max, recs.last()) {
            (0, _) => self
                .ring
                .get(start)
                .map_or(head, |r| r.seq.saturating_sub(1))
                .max(from),
            (_, Some(last)) if recs.len() == max => last.seq,
            _ => head,
        };
        let evicted_to = self.oldest_seq().saturating_sub(1).min(next);
        let lost = evicted_to.saturating_sub(from);
        let gap = (reset || lost > 0).then(|| Gap {
            why: if reset {
                GapWhy::Reset
            } else {
                GapWhy::Evicted
            },
            lost,
            resync_to: recs.first().map_or(next, |r| r.seq.saturating_sub(1)),
        });
        Read {
            gap,
            recs,
            next: Cursor::new(self.ep, next),
        }
    }

    /// The instance's start: `open n start` with `f`.
    /// # Errors
    /// [`Refused::AlreadyOpen`] once any record was written, or a field or encoding refusal.
    pub fn start(&mut self, f: Fields, at: Stamp) -> Result<u64, Refused> {
        if self.started() {
            return Err(Refused::AlreadyOpen);
        }
        self.append(Op::Open, own(f), at)
    }

    /// Refine the start with `f` (`prev_end=` once the recovery ledger was read): `amend n start`.
    /// # Errors
    /// [`Refused::NotIssued`] before [`NodeLog::start`], or a field or encoding refusal.
    pub fn amend_start(&mut self, f: Fields, at: Stamp) -> Result<u64, Refused> {
        if !self.started() {
            return Err(Refused::NotIssued(Node::INSTANCE));
        }
        self.append(Op::Amend, own(f), at)
    }

    /// A point fact on node `n`: a [`NODE_MARKS`] kind.
    /// # Errors
    /// [`Refused::KindDoesNotFit`] for any other kind, [`Refused::NotIssued`] for a mark aimed at
    /// another node, [`Refused::NotOpen`] before [`NodeLog::start`], or a field or encoding
    /// refusal.
    pub fn mark(&mut self, m: Mark, at: Stamp) -> Result<u64, Refused> {
        if !NODE_MARKS.contains(&m.kind) {
            return Err(Refused::KindDoesNotFit(m.kind));
        }
        if let Some(n) = m.at.filter(|n| *n != Node::INSTANCE) {
            return Err(Refused::NotIssued(n));
        }
        if !self.started() {
            return Err(Refused::NotOpen(Node::INSTANCE));
        }
        self.append(Op::Mark, m, at)
    }

    /// Append `m`'s record as `op` on node `n`, evicting the oldest past the
    /// bound (counted).
    fn append(&mut self, op: Op, m: Mark, at: Stamp) -> Result<u64, Refused> {
        if let Some((key, _)) = m.f.iter().find(|(key, _)| log_owned(op, *key)) {
            return Err(Refused::Field(crate::field::FieldError::Shape(*key)));
        }
        let mut rec = Rec {
            seq: u64::MAX,
            t: at.wall_ms,
            op,
            node: Node::INSTANCE,
            par: None,
            k: m.kind,
            src: m.src,
            conf: m.conf,
            by: m.by,
            f: m.f,
        };
        encoded_size(&rec, Tier::Content).map_err(Refused::Encode)?;
        self.last_seq += 1;
        rec.seq = self.last_seq;
        if self.ring.len() >= self.cap {
            self.ring.pop_front();
            self.lost += 1;
        }
        self.ring.push_back(rec);
        Ok(self.last_seq)
    }
}

/// The start record's own provenance: aterm, read off what it holds.
fn own(f: Fields) -> Mark {
    Mark::new(Kind::Start, Src::Aterm, Conf::Exact)
        .by(Actor::Aterm)
        .fields(f)
}

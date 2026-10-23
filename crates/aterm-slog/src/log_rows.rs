// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TIMELINE'S ROWS: the records a host's
//! timeline prints as rows, numbered densely, and the floor that keeps the
//! newest of them.
//!
//! - **The rule.** A host that prints some records as timeline rows installs
//!   its grammar's count ([`RowRule`]: how many rows a record stands for, 0
//!   for none) with [`Log::keep_rows`]. From then on every record the log
//!   appends ITSELF is flagged by it as it is appended: the writer's record
//!   is its row, so no writer can write a row the view does not hold, nor
//!   flag a record its grammar does not print. A record the log did not
//!   append (a carried one, a journal's) is never a row: the rows are this
//!   epoch's, as a host's ring, which no update carries, always was.
//! - **Dense ids.** A flagged record takes the next ids from the log's row
//!   counter, one per row it stands for (a session close that prints two
//!   rows takes two), in append order, never reused while the counter lives.
//!   The counter is the host's to carry across an update (its timeline id,
//!   with its overlap reserve: [`Log::continue_rows`]); a log whose host
//!   carries none (a crash adoption) counts from 1 in its new epoch, so a
//!   row is named by its epoch and id, never by a start second. A row id the
//!   log no longer holds is a hole a reader is told of
//!   ([`Log::rows_lost_between`]).
//! - **The row floor.** The newest flagged records, up to a quarter of the
//!   ring's records and of its byte budget ([`row_floor`]), are taken by an
//!   eviction batch only after every other record the batch may take: every
//!   unflagged closed detail first (U14's rule), then the rest of the
//!   policy's order. So a flood of commands, turns and verdict marks keeps
//!   the newest rows the timeline promises. The mark quota and the program
//!   share keep their own rules (a claim's quarter, a node's 64 program
//!   marks): they take only their own records, flagged or not. The floor
//!   yields to the journal (phase 2B, [`Log::note_journaled`]): while a
//!   writer follows the log, the floor holds within the records it has
//!   taken and within those it has not, so a burst between two of its
//!   passes takes a floor row the journal holds before a record it has not
//!   reached, and loses nothing the writer could still write (the row's id
//!   is then a hole a reader is told of).
//!
//! RAM ONLY: a record's row id is no field, so no tier encodes it, and the
//! carry and the journal write none (a successor's rows are its own).

use super::{Limits, Log, MAX_RECORDS, SEQ_LIMIT};
use crate::rec::Rec;

/// How many timeline rows a record stands for (0: none), as a host's
/// grammar prints it: what [`Log::keep_rows`] installs.
pub type RowRule = fn(&Rec) -> u8;

/// The most rows one record stands for (a session close: its reason's row
/// and its last state's).
pub const ROW_SPAN_MAX: u8 = 2;

/// The row floor at bounds `l`: a quarter of its records and of its bytes
/// (a session's ring: 512 records, the timeline's own cap, and 64 KiB).
#[must_use]
pub const fn row_floor(l: Limits) -> Limits {
    Limits {
        records: l.records / 4,
        bytes: l.bytes / 4,
    }
}

/// A record's place among the rows: the first row id it stands for in the
/// low 62 bits, how many in the top two (0: not a row).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RowTag(u64);

impl RowTag {
    /// No row.
    pub(super) const NONE: RowTag = RowTag(0);
    const ID_BITS: u32 = 62;
    const ID_MASK: u64 = (1 << Self::ID_BITS) - 1;

    /// Rows `id ..= id + span - 1` (a span past [`ROW_SPAN_MAX`] is that).
    const fn new(id: u64, span: u8) -> RowTag {
        let span = if span > ROW_SPAN_MAX {
            ROW_SPAN_MAX
        } else {
            span
        };
        RowTag((id & Self::ID_MASK) | ((span as u64) << Self::ID_BITS))
    }

    /// The first row id (0: not a row).
    pub(super) const fn id(self) -> u64 {
        if self.span() == 0 {
            0
        } else {
            self.0 & Self::ID_MASK
        }
    }

    /// How many rows.
    pub(super) const fn span(self) -> u8 {
        (self.0 >> Self::ID_BITS) as u8
    }
}

/// One live row record: the ids it stands for, its process-clock reading
/// (6F-1) and the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row<'a> {
    /// The first row id it stands for.
    pub id: u64,
    /// How many rows (1 ..= [`ROW_SPAN_MAX`]).
    pub span: u8,
    /// Its process-clock reading ([`Log::mono_of`]'s).
    pub mono: u64,
    /// The record.
    pub rec: &'a Rec,
}

/// The rows a log keeps: its host's rule, its counter and where it began.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Rows {
    /// The host's grammar, once installed.
    pub(super) rule: Option<RowRule>,
    /// The last row id issued (or carried in: the host's count).
    pub(super) head: u64,
    /// The first id this log issued or will issue: one past the count it
    /// was continued at.
    pub(super) first: u64,
    /// The first seq this log appended itself (0: none yet): a record
    /// before it is carried, never a row.
    pub(super) own_from: u64,
}

impl Rows {
    pub(super) const NEW: Rows = Rows {
        rule: None,
        head: 0,
        first: 1,
        own_from: 0,
    };
}

impl Log {
    /// KEEP ROWS: install the host's grammar `rule` and continue the row
    /// counter at `carried` ([`Log::continue_rows`]). The records this log
    /// appended itself before (a successor's first record, which its
    /// constructor writes: `gap why=handoff`, a carry's `mark - handoff`)
    /// are flagged now, in seq order, as if the rule had stood at their
    /// append. Installing it again replaces the rule and flags nothing twice.
    pub fn keep_rows(&mut self, rule: RowRule, carried: u64) {
        self.rows.rule = Some(rule);
        self.continue_rows(carried);
        let own_from = self.rows.own_from;
        if own_from == 0 {
            return;
        }
        let mut tags = Vec::new();
        for e in &self.ring {
            if e.rec.seq >= own_from && e.row.span() == 0 {
                tags.push(rule(&e.rec));
            } else {
                tags.push(0);
            }
        }
        for (i, span) in tags.into_iter().enumerate() {
            if span > 0
                && let Some(tag) = self.next_rows(span)
            {
                self.ring[i].row = tag;
            }
        }
    }

    /// Continue the row counter at `carried`: the next row this log issues
    /// is above it. Never lowers the count; a count past [`SEQ_LIMIT`] (no
    /// host's: a corrupt or foreign manifest) is none. Continued past its
    /// head after this log issued ids, the ids between are never issued:
    /// holes [`Log::rows_lost_between`] counts. Continued before it issued
    /// any (a successor's, as a host continues it at its adoption), the ids
    /// up to the count are below [`Log::row_first`]: no loss of this log's,
    /// so a host whose reader holds the predecessor's id tells that jump
    /// itself (aterm-gui's timeline view, as its ring's digest tells the
    /// update's).
    pub const fn continue_rows(&mut self, carried: u64) {
        if carried > SEQ_LIMIT || carried <= self.rows.head {
            return;
        }
        if self.rows_issued() == 0 {
            self.rows.first = carried.saturating_add(1);
        }
        self.rows.head = carried;
    }

    /// Whether a host's row rule is installed.
    #[must_use]
    pub const fn keeps_rows(&self) -> bool {
        self.rows.rule.is_some()
    }

    /// The last row id issued, or the count the log was continued at.
    #[must_use]
    pub const fn row_head(&self) -> u64 {
        self.rows.head
    }

    /// The first row id this log issues (one past the count it was
    /// continued at): a reader's id below it is another process's.
    #[must_use]
    pub const fn row_first(&self) -> u64 {
        self.rows.first
    }

    /// How many row ids this log has passed: issued, or skipped by a count
    /// continued past its head ([`Log::continue_rows`]).
    #[must_use]
    pub const fn rows_issued(&self) -> u64 {
        if self.rows.head >= self.rows.first {
            self.rows.head - self.rows.first + 1
        } else {
            0
        }
    }

    /// The live row records, oldest first (in seq order, which is id order).
    pub fn rows(&self) -> impl DoubleEndedIterator<Item = Row<'_>> {
        self.ring.iter().filter(|e| e.row.span() > 0).map(|e| Row {
            id: e.row.id(),
            span: e.row.span(),
            mono: e.mono,
            rec: &e.rec,
        })
    }

    /// The row ids in `(lo, hi]` this log issued and no longer holds
    /// (evicted, or taken by the mark quota or the program share): the holes
    /// a reader past `lo` crosses on its way to `hi`. An id this log never
    /// issued (below [`Log::row_first`], or past its head) is no loss of its.
    #[must_use]
    pub fn rows_lost_between(&self, lo: u64, hi: u64) -> u64 {
        let lo = lo.max(self.rows.first.saturating_sub(1));
        let hi = hi.min(self.rows.head);
        if hi <= lo {
            return 0;
        }
        let held: u64 = self
            .rows()
            .map(|r| {
                let (a, b) = (r.id, r.id + u64::from(r.span) - 1);
                let (a, b) = (a.max(lo + 1), b.min(hi));
                if b >= a { b - a + 1 } else { 0 }
            })
            .sum();
        (hi - lo).saturating_sub(held)
    }

    /// Note that `seq` is this log's own append, and flag it by the rule:
    /// the row tag its entry takes.
    pub(super) fn tag_own(&mut self, rec: &Rec) -> RowTag {
        if self.rows.own_from == 0 {
            self.rows.own_from = rec.seq;
        }
        match self.rows.rule.map(|rule| rule(rec)) {
            Some(span @ 1..) => self.next_rows(span).unwrap_or(RowTag::NONE),
            _ => RowTag::NONE,
        }
    }

    /// The next `span` row ids, or `None` when any would exceed the row tag's
    /// 62-bit id: refuse the whole span before the counter advances.
    fn next_rows(&mut self, span: u8) -> Option<RowTag> {
        let span = span.min(ROW_SPAN_MAX);
        let id = self.rows.head.checked_add(1)?;
        let head = self.rows.head.checked_add(u64::from(span))?;
        if head > RowTag::ID_MASK {
            return None;
        }
        self.rows.head = head;
        Some(RowTag::new(id, span))
    }

    /// THE ROW FLOOR's members, by ring position: the newest flagged
    /// records, newest first, while they fit [`row_floor`] of the bounds
    /// evictions keep to (a lowered byte budget lowers the floor with it).
    pub(super) fn row_floor_members(&self) -> Vec<bool> {
        let mut floor = vec![false; self.ring.len()];
        if self.rows.rule.is_none() {
            return floor;
        }
        let q = row_floor(self.effective_limits());
        let (mut count, mut bytes) = (0usize, 0usize);
        for (i, e) in self.ring.iter().enumerate().rev() {
            if e.row.span() == 0 {
                continue;
            }
            if count + 1 > q.records || bytes.saturating_add(e.bytes) > q.bytes {
                break;
            }
            count += 1;
            bytes = bytes.saturating_add(e.bytes);
            floor[i] = true;
        }
        floor
    }
}

// The floor of a session's ring is the timeline's own cap (512 rows).
const _: () = assert!(row_floor(Limits::SESSION).records == MAX_RECORDS / 4);

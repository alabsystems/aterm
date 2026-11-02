// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The retained records' RESIDENCY TIER: which committed records a [`BrokerLog`] keeps
//! decoded in memory, and where on the log file every one of them starts.
//!
//! Before this module the log held EVERY retained record decoded in RAM
//! (`records: Vec<Arc<BrokerRecord>>`) for its whole open, so a broker's resident
//! memory grew with everything it had ever been sent and not yet compacted away: a
//! fleet left running for days paid for its whole history in RAM, though readers
//! almost always want the newest records (a live tail, a resume a little behind it).
//!
//! The tier keeps, for every retained record, only what is cheap — the byte at which
//! its frame starts in the log file and its broker time (`bt=`) — and keeps the
//! DECODED record only for a hot window at the head, bounded by a byte budget
//! ([`Tier::with_budget`]). A record that falls out of the window is read back from
//! its frame on demand ([`BrokerLog::record_arc`](crate::BrokerLog)), through the same
//! frame decode, at-rest open and record parse the open's recovery scan applies, so a
//! cold read returns exactly the record the open would have recovered.
//!
//! WHAT IS NEVER EVICTED: a record nothing has made durable is never written cold,
//! because every record the tier holds was promoted by a write that put its frame at
//! its recorded position first (`commit_end`, `append_frame`, the open's scan, the
//! retention rewrite). Positions are therefore always valid for the file the log
//! currently holds. A retention rewrite replaces the file and rebuilds the tier with
//! the new positions in the same step.
//!
//! The default budget is unbounded ([`Tier::RESIDENT_ALL`]), which is exactly the old
//! behaviour; an embedder opts into a bound through `OpenOptions`.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use crate::brecord::{plaintext_frame_bytes, BrokerRecord};
use crate::store::is_hidden_record;

/// Cheap per-record metadata kept in memory for every retained record so retention
/// planning, filtered reads (`Fetch`, `Subscribe`), wake planning, and `Stats` walk
/// cold history without decoding non-matching frames from the log file: its offset,
/// its interned subject, its plaintext-frame bytes, and whether it is broker-internal.
pub(crate) struct Summary {
    off: u64,
    subject: Arc<str>,
    bytes: u64,
    hidden: bool,
}

impl Summary {
    /// The record's absolute offset.
    pub(crate) fn offset(&self) -> u64 {
        self.off
    }

    /// The record's subject.
    pub(crate) fn subject(&self) -> &str {
        &self.subject
    }

    /// Plaintext-frame byte count ([`plaintext_frame_bytes`]).
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Whether [`is_hidden_record`] holds for this record.
    pub(crate) fn is_hidden(&self) -> bool {
        self.hidden
    }
}

#[cfg(feature = "retention")]
impl crate::retention::Planned for Summary {
    fn offset(&self) -> u64 {
        self.off
    }
    fn subject(&self) -> &str {
        &self.subject
    }
    fn bytes(&self) -> u64 {
        self.bytes
    }
    fn whole(&self) -> Option<Arc<BrokerRecord>> {
        None
    }
}

/// The bytes a decoded record is charged against the hot budget: its subject and body
/// plus a fixed overhead for the record's own fields and its `Arc`.
pub(crate) fn resident_cost(rec: &BrokerRecord) -> usize {
    const FIXED: usize = 96;
    FIXED
        .saturating_add(rec.subject.len())
        .saturating_add(rec.body.len())
}

/// The retained records of one log: positions, broker times and summaries for all of
/// them, and the decoded records for the hot window at the head.
pub(crate) struct Tier {
    /// Byte at which record `base + i`'s frame starts in the log file.
    at: Vec<u64>,
    /// Broker time (`bt=`) of record `base + i`.
    bts: Vec<Option<u64>>,
    /// The decoded hot window: `hot[j]` is record index `at.len() - hot.len() + j`.
    hot: VecDeque<Arc<BrokerRecord>>,
    /// Sum of [`resident_cost`] over `hot`.
    hot_bytes: usize,
    /// The hot window's byte budget; the newest record is always kept resident.
    budget: usize,
    /// Record `base + i`'s summary, aligned 1:1 with `at`.
    sums: Vec<Summary>,
    /// One shared copy of each subject the summaries name.
    subjects: HashSet<Arc<str>>,
}

impl Tier {
    /// The budget under which every retained record stays resident (the behaviour
    /// before the tier existed).
    pub(crate) const RESIDENT_ALL: usize = usize::MAX;

    /// An empty tier keeping at most `budget` bytes of decoded records resident.
    pub(crate) fn with_budget(budget: usize) -> Tier {
        Tier {
            at: Vec::new(),
            bts: Vec::new(),
            hot: VecDeque::new(),
            hot_bytes: 0,
            budget,
            sums: Vec::new(),
            subjects: HashSet::new(),
        }
    }

    /// The summary of `rec`, its subject shared with the summaries before it.
    fn summarize(&mut self, rec: &BrokerRecord) -> Summary {
        let subject = match self.subjects.get(rec.subject.as_str()) {
            Some(s) => Arc::clone(s),
            None => {
                let s: Arc<str> = Arc::from(rec.subject.as_str());
                self.subjects.insert(Arc::clone(&s));
                s
            }
        };
        Summary {
            off: rec.seq.0,
            subject,
            bytes: plaintext_frame_bytes(rec),
            hidden: is_hidden_record(rec),
        }
    }

    /// Record `i`'s summary, if `i` is in range.
    pub(crate) fn summary(&self, i: usize) -> Option<&Summary> {
        self.sums.get(i)
    }

    /// Every retained record's summary, in index order.
    pub(crate) fn summaries(&self) -> &[Summary] {
        &self.sums
    }

    /// Forget shared subjects no summary names any more (after a cut drops records).
    #[cfg(any(test, feature = "retention"))]
    fn prune_subjects(&mut self) {
        self.subjects.retain(|s| Arc::strong_count(s) > 1);
    }

    /// How many records are retained (resident or not).
    pub(crate) fn len(&self) -> usize {
        self.at.len()
    }

    /// How many records are resident.
    pub(crate) fn resident(&self) -> usize {
        self.hot.len()
    }

    /// Bytes charged to the resident records.
    pub(crate) fn resident_bytes(&self) -> usize {
        self.hot_bytes
    }

    /// The configured budget.
    pub(crate) fn budget(&self) -> usize {
        self.budget
    }

    /// Change the budget, evicting at once if it shrank.
    pub(crate) fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.evict();
    }

    /// Index of the first resident record.
    fn first_hot(&self) -> usize {
        self.at.len().saturating_sub(self.hot.len())
    }

    /// Append a promoted record whose frame starts at byte `at`.
    pub(crate) fn push(&mut self, rec: Arc<BrokerRecord>, bt: Option<u64>, at: u64) {
        let s = self.summarize(&rec);
        self.sums.push(s);
        self.hot_bytes = self.hot_bytes.saturating_add(resident_cost(&rec));
        self.hot.push_back(rec);
        self.at.push(at);
        self.bts.push(bt);
        self.evict();
    }

    /// Drop the oldest resident records until the window fits the budget, always
    /// keeping the newest one.
    fn evict(&mut self) {
        while self.hot_bytes > self.budget && self.hot.len() > 1 {
            let Some(old) = self.hot.pop_front() else {
                break;
            };
            self.hot_bytes = self.hot_bytes.saturating_sub(resident_cost(&old));
        }
    }

    /// The resident record at index `i`, if it is resident.
    pub(crate) fn hot(&self, i: usize) -> Option<&Arc<BrokerRecord>> {
        let first = self.first_hot();
        let j = i.checked_sub(first)?;
        self.hot.get(j)
    }

    /// Where record `i`'s frame starts.
    pub(crate) fn at(&self, i: usize) -> Option<u64> {
        self.at.get(i).copied()
    }

    /// Record `i`'s broker time.
    pub(crate) fn bt(&self, i: usize) -> Option<u64> {
        self.bts.get(i).copied().flatten()
    }

    /// Drop every record at index `keep` and above (a rolled-back tail).
    #[cfg(test)]
    pub(crate) fn truncate(&mut self, keep: usize) {
        if keep >= self.at.len() {
            return;
        }
        let first = self.first_hot();
        if keep <= first {
            self.hot.clear();
            self.hot_bytes = 0;
        } else {
            let keep_hot = keep - first;
            while self.hot.len() > keep_hot {
                if let Some(r) = self.hot.pop_back() {
                    self.hot_bytes = self.hot_bytes.saturating_sub(resident_cost(&r));
                }
            }
        }
        self.at.truncate(keep);
        self.bts.truncate(keep);
        self.sums.truncate(keep);
        self.prune_subjects();
    }

    /// Iterate the resident records (test and accounting helper).
    #[cfg(test)]
    pub(crate) fn hot_iter(&self) -> impl Iterator<Item = &Arc<BrokerRecord>> {
        self.hot.iter()
    }

    /// Every retained record's broker time, in index order.
    #[cfg(any(feature = "retention", all(feature = "at-rest", unix)))]
    pub(crate) fn bts(&self) -> &[Option<u64>] {
        &self.bts
    }

    /// A tier over records an open (or a rebuild) recovered, with their broker times and
    /// frame starts; the oldest are evicted at once if they do not fit `budget`.
    pub(crate) fn from_parts(
        records: Vec<Arc<BrokerRecord>>,
        bts: Vec<Option<u64>>,
        at: Vec<u64>,
        budget: usize,
    ) -> Tier {
        let mut tier = Tier::with_budget(budget);
        let n = records.len().min(at.len());
        tier.at = at;
        tier.at.truncate(n);
        tier.bts = bts;
        tier.bts.truncate(n);
        for rec in records.into_iter().take(n) {
            let s = tier.summarize(&rec);
            tier.sums.push(s);
            if tier.bts.len() < tier.sums.len() {
                tier.bts.push(None);
            }
            tier.hot_bytes = tier.hot_bytes.saturating_add(resident_cost(&rec));
            tier.hot.push_back(rec);
        }
        tier.evict();
        tier
    }

    /// Drop the oldest `n` records (a retention cut) and move every kept frame start
    /// down by `shift` bytes (the bytes the cut removed from the front of the file).
    #[cfg(any(test, feature = "retention"))]
    pub(crate) fn drain_front(&mut self, n: usize, shift: u64) {
        let n = n.min(self.at.len());
        let first = self.first_hot();
        for _ in 0..n.saturating_sub(first) {
            if let Some(r) = self.hot.pop_front() {
                self.hot_bytes = self.hot_bytes.saturating_sub(resident_cost(&r));
            }
        }
        let old_at = std::mem::take(&mut self.at);
        for (i, a) in old_at.into_iter().enumerate() {
            if i >= n {
                self.at.push(a.saturating_sub(shift));
            }
        }
        let old_bts = std::mem::take(&mut self.bts);
        for (i, bt) in old_bts.into_iter().enumerate() {
            if i >= n {
                self.bts.push(bt);
            }
        }
        let old_sums = std::mem::take(&mut self.sums);
        for (i, s) in old_sums.into_iter().enumerate() {
            if i >= n {
                self.sums.push(s);
            }
        }
        self.prune_subjects();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astream_wire::Offset;

    fn rec(seq: u64, body: usize) -> Arc<BrokerRecord> {
        Arc::new(BrokerRecord {
            seq: Offset(seq),
            producer_id: 1,
            producer_seq: seq,
            subject: "/f/t".to_string(),
            body: vec![7u8; body],
            commit: None,
        })
    }

    #[test]
    fn unbounded_keeps_everything_resident() {
        let mut t = Tier::with_budget(Tier::RESIDENT_ALL);
        for i in 0..100 {
            t.push(rec(i, 1000), Some(i), i * 10);
        }
        assert_eq!((t.len(), t.resident()), (100, 100));
        assert!(t.hot(0).is_some());
    }

    #[test]
    fn budget_evicts_oldest_and_always_keeps_newest() {
        let cost = resident_cost(&rec(0, 1000));
        let mut t = Tier::with_budget(cost * 3);
        for i in 0..10 {
            t.push(rec(i, 1000), Some(i), i * 10);
            assert!(t.resident_bytes() <= cost * 3);
        }
        assert_eq!((t.len(), t.resident()), (10, 3));
        assert!(t.hot(6).is_none());
        assert_eq!(t.hot(7).map(|r| r.seq.0), Some(7));
        assert_eq!(t.at(2), Some(20));
        assert_eq!(t.bt(9), Some(9));
        // A record bigger than the whole budget still stays resident while newest.
        t.push(rec(10, 1 << 20), None, 100);
        assert_eq!(t.resident(), 1);
        assert_eq!(t.hot(10).map(|r| r.seq.0), Some(10));
    }

    #[test]
    fn truncate_and_drain_keep_indices_aligned() {
        let cost = resident_cost(&rec(0, 10));
        let mut t = Tier::with_budget(cost * 4);
        for i in 0..8 {
            t.push(rec(i, 10), Some(i), i * 50);
        }
        t.truncate(6);
        assert_eq!((t.len(), t.resident()), (6, 2));
        assert_eq!(t.hot(5).map(|r| r.seq.0), Some(5));
        t.drain_front(5, 250);
        assert_eq!(t.len(), 1);
        assert_eq!(t.at(0), Some(0));
        assert_eq!(t.hot(0).map(|r| r.seq.0), Some(5));
        assert_eq!(t.bt(0), Some(5));
    }

    #[test]
    fn shrinking_the_budget_evicts_at_once() {
        let mut t = Tier::with_budget(Tier::RESIDENT_ALL);
        for i in 0..5 {
            t.push(rec(i, 100), None, i);
        }
        t.set_budget(0);
        assert_eq!(t.resident(), 1);
        assert_eq!(t.resident_bytes(), resident_cost(&rec(4, 100)));
    }

    #[test]
    fn summaries_stay_aligned_and_subjects_prune_on_truncate_and_drain() {
        fn named(seq: u64, subject: &str, body: usize) -> Arc<BrokerRecord> {
            Arc::new(BrokerRecord {
                seq: Offset(seq),
                producer_id: 1,
                producer_seq: seq,
                subject: subject.to_string(),
                body: vec![3u8; body],
                commit: None,
            })
        }

        let r0 = named(10, "/f/a", 12);
        let r1 = named(11, "/f/a", 24);
        let r2 = named(12, "/a/commit", 0);
        let r3 = named(13, "/f/tail", 8);
        let mut t = Tier::from_parts(
            vec![Arc::clone(&r0), Arc::clone(&r1), Arc::clone(&r2)],
            vec![Some(100), Some(101), Some(102)],
            vec![0, 64, 128],
            0,
        );
        t.push(Arc::clone(&r3), Some(103), 192);
        assert_eq!((t.len(), t.resident()), (4, 1));
        assert_eq!(t.summaries().len(), 4);
        assert!(Arc::ptr_eq(&t.sums[0].subject, &t.sums[1].subject));
        assert_eq!(t.subjects.len(), 3);
        assert_eq!(
            (
                t.summary(0).map(Summary::offset),
                t.summary(0).map(Summary::subject),
                t.summary(0).map(Summary::bytes),
                t.summary(0).map(Summary::is_hidden),
            ),
            (
                Some(10),
                Some("/f/a"),
                Some(plaintext_frame_bytes(&r0)),
                Some(false)
            ),
        );
        assert_eq!(t.summary(2).map(Summary::is_hidden), Some(true));

        t.truncate(3);
        assert_eq!(t.summaries().len(), 3);
        assert_eq!(t.subjects.len(), 2, "/f/tail pruned on truncate");

        t.drain_front(2, 128);
        assert_eq!(t.summaries().len(), 1);
        assert_eq!(t.summary(0).map(Summary::offset), Some(12));
        assert_eq!(t.summary(0).map(Summary::subject), Some("/a/commit"));
        assert_eq!(t.subjects.len(), 1, "/f/a pruned on drain_front");
    }
}

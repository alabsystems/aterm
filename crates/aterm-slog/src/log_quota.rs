// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MARK QUOTA (design §7.4, phase 3A): the claims a consumer writes back
//! (`mark`, `src=mark`) hold at most a quarter of a ring's capacity — a
//! quarter of its record bound and of its byte budget, so a near-empty ring
//! takes marks as freely as a full one and a lowered budget lowers the share.
//!
//! [`Log::enforce_mark_quota`] is the policy, and the one function that
//! holds it: an append of a mark and a change of the budget or of the
//! readers' fence call it. It takes only marks, the oldest first, never an
//! observation, never the record just appended, and never a record the
//! readers' fence holds (ruling R4: what a reader is shown during an
//! update's overlap is what the carry holds, as [`Log::fence_reads`] keeps
//! it for the eviction batch). While a journal writer follows the log it
//! takes the marks the writer has already taken first. Each mark it takes is
//! counted lost, into the rollups, exactly as a batch's victim is. It keeps
//! no counter of its own: one pass over the ring, so it holds whichever way
//! the ring was built (a carry, a closed copy, a journal read).
//!
//! THE PROGRAM SHARE (phase 6, 6F-3; T6, ruling U17): the program marks
//! (`mark <node> program`, OSC 7501's reports) hanging from one node hold at
//! most [`program_share`] of the ring: 64 per run at a session's bound, or
//! per session while no run is open. [`Log::enforce_program_share`] keeps it
//! on the same terms as the quota: an append of a program mark and the fence
//! lifting run it, it takes only that node's program marks, the oldest
//! first, never the record just appended and never a fenced one, and each it
//! takes is counted lost. So a flapping program spends its own share and
//! never the facts beside it (a mail mark written before 300 flaps is still
//! in the log).

use std::collections::BTreeMap;

use super::{Entry, Limits, Log};
use crate::vocab::{Kind, Node, Op, Src};

/// The program share at a session ring's bound ([`super::MAX_RECORDS`]).
pub const PROGRAM_SHARE_MAX: usize = 64;

/// What one node's program marks may hold of a ring bounded by `l`: a 32nd
/// of its records, at least one (the mark just appended always stays) and at
/// most [`PROGRAM_SHARE_MAX`] (a session's ring: 64).
#[must_use]
pub const fn program_share(l: Limits) -> usize {
    let n = l.records / 32;
    if n < 1 {
        1
    } else if n > PROGRAM_SHARE_MAX {
        PROGRAM_SHARE_MAX
    } else {
        n
    }
}

/// Whether `e` is a program mark.
fn is_program(e: &Entry) -> bool {
    e.rec.op == Op::Mark && e.rec.k == Kind::Program
}

/// What marks may hold of a ring bounded by `l`: a quarter of its records
/// (at least one: the mark just appended always stays) and a quarter of its
/// bytes.
#[must_use]
pub const fn mark_quota(l: Limits) -> Limits {
    Limits {
        records: if l.records / 4 > 1 { l.records / 4 } else { 1 },
        bytes: l.bytes / 4,
    }
}

impl Log {
    /// The live marks (`src=mark` records): how many, and their encoded
    /// bytes.
    #[must_use]
    pub fn marks_held(&self) -> (usize, usize) {
        self.ring
            .iter()
            .filter(|e| e.rec.src == Src::Mark)
            .fold((0, 0), |(n, b), e| (n + 1, b + e.bytes))
    }

    /// THE MARK QUOTA: take the oldest marks until the live ones are within
    /// [`mark_quota`] of the ring's bounds (its record bound and its current
    /// byte budget), never the newest record and never one at or below the
    /// readers' fence; the ones a journal writer has taken go first. Each is
    /// counted lost as an eviction's is. Returns how many it took.
    pub fn enforce_mark_quota(&mut self) -> u64 {
        let q = mark_quota(self.effective_limits());
        let n = self.ring.len();
        let frozen = self.read_fence().unwrap_or(0);
        let (mut count, mut bytes) = (0usize, 0usize);
        let (mut first, mut rest) = (Vec::new(), Vec::new());
        for (i, e) in self.ring.iter().enumerate() {
            if e.rec.src != Src::Mark {
                continue;
            }
            count += 1;
            bytes = bytes.saturating_add(e.bytes);
            if i + 1 == n || e.rec.seq <= frozen {
                continue;
            }
            match self.journaled {
                Some(j) if e.rec.seq > j => rest.push(i),
                _ => first.push(i),
            }
        }
        let within = |count: usize, bytes: usize| count <= q.records && bytes <= q.bytes;
        if within(count, bytes) {
            return 0;
        }
        let mut victim = vec![false; n];
        let mut taken = 0u64;
        for i in first.into_iter().chain(rest) {
            if within(count, bytes) {
                break;
            }
            victim[i] = true;
            count -= 1;
            bytes = bytes.saturating_sub(self.ring[i].bytes);
            taken += 1;
        }
        if taken == 0 {
            return 0;
        }
        self.take_victims(victim);
        taken
    }

    /// The live program marks hanging from `node`.
    #[must_use]
    pub fn programs_held(&self, node: Node) -> usize {
        self.ring
            .iter()
            .filter(|e| is_program(e) && e.rec.node == node)
            .count()
    }

    /// THE PROGRAM SHARE: take the oldest program marks of each node (of
    /// `only`, when it names one) whose live program marks pass
    /// [`program_share`] of the ring's record bound, until it is within it,
    /// never the newest record and never one at or below the readers' fence.
    /// Each is counted lost as an eviction's is. Returns how many it took.
    pub fn enforce_program_share(&mut self, only: Option<Node>) -> u64 {
        let share = program_share(self.limits);
        let mut held: BTreeMap<Node, usize> = BTreeMap::new();
        for e in &self.ring {
            if is_program(e) && only.is_none_or(|n| n == e.rec.node) {
                *held.entry(e.rec.node).or_insert(0) += 1;
            }
        }
        if held.values().all(|&c| c <= share) {
            return 0;
        }
        let n = self.ring.len();
        let frozen = self.read_fence().unwrap_or(0);
        let mut victim = vec![false; n];
        let mut taken = 0u64;
        for (i, e) in self.ring.iter().enumerate() {
            if !is_program(e) || i + 1 == n || e.rec.seq <= frozen {
                continue;
            }
            if let Some(c) = held.get_mut(&e.rec.node)
                && *c > share
            {
                victim[i] = true;
                *c -= 1;
                taken += 1;
            }
        }
        if taken > 0 {
            self.take_victims(victim);
        }
        taken
    }

    /// Take the ring's records `victim` names, each counted lost.
    fn take_victims(&mut self, victim: Vec<bool>) {
        let mut gone = Vec::new();
        let old = std::mem::take(&mut self.ring);
        for (e, v) in old.into_iter().zip(victim) {
            if v {
                gone.push(e);
            } else {
                self.ring.push_back(e);
            }
        }
        for e in &gone {
            self.evicted(e);
        }
    }
}

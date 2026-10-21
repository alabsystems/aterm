// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MARK LIMITER (design §7.4, phase 3A): `mark` is rate-limited per
//! (session, author) at one claim a second with bursts of [`MARK_BURST`],
//! and over the limit the verb answers `ERR busy retry_ms=`.
//!
//! Pure and clockless: the host passes its monotonic milliseconds. A bucket
//! holds thousandths of a token and refills one token per
//! [`MARK_REFILL_MS`], up to the burst.
//!
//! THE AUTHORS (ruling R11). The harness, the bridge, the Owner's clients
//! (`ctl`) and the person each have a bucket of their own, outside every
//! table, so no flood of other authors delays them. Sessions (edge sources)
//! share [`MARK_ACTORS`] named buckets; a slot is reused only when its
//! bucket is full and idle (never while it is in debt, which would refill
//! it), and every session past them shares one overflow bucket, as does an
//! author no connection names. An Owner can mint any number of edges, so the
//! table is bounded and a flood of minted sources marks at most as fast as
//! the named slots and the overflow together allow, whatever their number.
//!
//! THE MARKS A BUCKET WAS SHORT OF (phase 5, ruling T1): a host that counts
//! a mark instead of making it, its author's bucket short
//! ([`MarkLimiter::note_short`]), keeps the count, and the next mark the
//! same author makes says it ([`MarkLimiter::owed`], [`MarkLimiter::said`]):
//! a trusted actor's in its own bucket, a session's in its named slot while
//! it holds one. A named slot owing a count is given to another session only
//! when no free slot or idle slot owing nothing is left, and its count then
//! goes with its last holder, not with the slot.
//!
//! THE OVERFLOWED AUTHORS' OWN COUNTS (Phase 6 step 6C-6, ruling U18): the
//! authors past the named slots (the sessions past the table, and `-`,
//! `aterm` and a vendor) share the overflow bucket's tokens, but not their
//! counts: [`MARK_OWED_AUTHORS`] owed-only entries, one per such author,
//! each a count and no tokens, keep what each was short of, so each one's
//! next mark says its own `dropped=`. A session that takes a named slot
//! takes its entry's count into it. The table is bounded (an Owner can mint
//! any number of edges): past it, the entry noted least recently spills its
//! count into the SHARED POOL (the overflow bucket's own count), which an
//! overflowed author with no entry of its own says, marked as the pool's
//! ([`Owed::Shared`], `pool=shared` on the host's mark), never as its own.
//! So no author's count rides another author's mark as that author's.

use std::collections::VecDeque;

use crate::vocab::{Actor, ShortId};

/// The most marks one author makes at once.
pub const MARK_BURST: u32 = 20;
/// How long one token takes to come back (1 per second).
pub const MARK_REFILL_MS: u64 = 1000;
/// The named buckets sessions share before the overflow.
pub const MARK_ACTORS: usize = 16;
/// The overflowed authors whose short counts are kept apart (ruling U18):
/// past them, the shared pool.
pub const MARK_OWED_AUTHORS: usize = 32;

/// A bucket's full level, in thousandths of a token.
const FULL: u32 = MARK_BURST * 1000;

/// One author's tokens, in thousandths, as of a monotonic instant, and the
/// marks it was short of since one last said so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bucket {
    milli: u32,
    at_mono_ms: u64,
    short: u64,
}

impl Bucket {
    const fn full(now: u64) -> Bucket {
        Bucket {
            milli: FULL,
            at_mono_ms: now,
            short: 0,
        }
    }

    /// Refill to `now` (a clock that went back refills nothing).
    fn refill(&mut self, now: u64) {
        let elapsed = now.saturating_sub(self.at_mono_ms);
        let gained = elapsed.saturating_mul(1000) / MARK_REFILL_MS;
        let level = u64::from(self.milli).saturating_add(gained);
        self.milli = u32::try_from(level.min(u64::from(FULL))).unwrap_or(FULL);
        self.at_mono_ms = self.at_mono_ms.max(now);
    }

    /// `Err(retry_ms)` while short of one token.
    fn peek(&mut self, now: u64) -> Result<(), u64> {
        self.refill(now);
        if self.milli >= 1000 {
            Ok(())
        } else {
            let short = u64::from(1000 - self.milli);
            Err((short * MARK_REFILL_MS).div_ceil(1000).max(1))
        }
    }

    fn spend(&mut self, now: u64) {
        self.refill(now);
        self.milli = self.milli.saturating_sub(1000);
    }

    /// Full and so free to give to another session.
    fn idle(&mut self, now: u64) -> bool {
        self.refill(now);
        self.milli >= FULL
    }
}

/// Which bucket an author spends from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Harness,
    Bridge,
    Ctl,
    Human,
    Named(usize),
    Overflow,
}

/// What an author's next mark says ([`MarkLimiter::owed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owed {
    /// The author's own count (`0`: nothing to say): its trusted bucket's,
    /// its named slot's, or its owed-only entry's.
    Own(u64),
    /// The shared pool's count, which an overflowed author with no count of
    /// its own says for the authors the owed-only table spilled: never as
    /// its own (`pool=shared`).
    Shared(u64),
}

impl Owed {
    /// How many marks it says.
    #[must_use]
    pub const fn count(self) -> u64 {
        match self {
            Owed::Own(n) | Owed::Shared(n) => n,
        }
    }

    /// Whether it is the shared pool's.
    #[must_use]
    pub const fn is_shared(self) -> bool {
        matches!(self, Owed::Shared(_))
    }
}

/// One session's mark limiter (see the module doc).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkLimiter {
    harness: Bucket,
    bridge: Bucket,
    ctl: Bucket,
    human: Bucket,
    named: [Option<(ShortId, Bucket)>; MARK_ACTORS],
    /// The overflowed authors' tokens, and in `short` the shared pool.
    overflow: Bucket,
    /// The overflowed authors' own counts, least recently noted first, at
    /// most [`MARK_OWED_AUTHORS`], none of them zero.
    owed_only: VecDeque<(Actor, u64)>,
}

impl Default for MarkLimiter {
    fn default() -> MarkLimiter {
        MarkLimiter::new(0)
    }
}

impl MarkLimiter {
    /// A limiter with every bucket full at `now_mono_ms`.
    #[must_use]
    pub const fn new(now_mono_ms: u64) -> MarkLimiter {
        let full = Bucket::full(now_mono_ms);
        MarkLimiter {
            harness: full,
            bridge: full,
            ctl: full,
            human: full,
            named: [None; MARK_ACTORS],
            overflow: full,
            owed_only: VecDeque::new(),
        }
    }

    /// The bucket `by` spends from at `now`: a trusted actor's own; a
    /// session's named slot (its own, else a free or full-and-idle one, which
    /// it takes), else the overflow.
    fn slot(&mut self, by: &Actor, now: u64, take: bool) -> Slot {
        let id = match by {
            Actor::Harness => return Slot::Harness,
            Actor::Bridge => return Slot::Bridge,
            Actor::Ctl => return Slot::Ctl,
            Actor::Human => return Slot::Human,
            Actor::Sid(id) => *id,
            Actor::Aterm | Actor::Vendor | Actor::Nobody => return Slot::Overflow,
        };
        if let Some(i) = self
            .named
            .iter()
            .position(|s| s.as_ref().is_some_and(|(sid, _)| *sid == id))
        {
            return Slot::Named(i);
        }
        // A free slot, or an idle one owing nothing; else an idle one whose
        // count goes unsaid (module doc).
        let free = self
            .named
            .iter_mut()
            .position(|s| match s {
                None => true,
                Some((_, b)) => b.idle(now) && b.short == 0,
            })
            .or_else(|| {
                self.named
                    .iter_mut()
                    .position(|s| s.as_mut().is_some_and(|(_, b)| b.idle(now)))
            });
        match free {
            Some(i) => {
                if take {
                    // The new holder brings its entry's count, and leaves
                    // the table first: its room is the last holder's, whose
                    // count goes with it, to its own entry. Owed first, a
                    // full table would spill the new holder's own count
                    // into the pool when it was the least recently noted.
                    let brought = self.take_entry(by).unwrap_or(0);
                    if let Some((last, b)) = self.named[i]
                        && b.short > 0
                    {
                        self.owe(Actor::Sid(last), b.short);
                    }
                    let mut b = Bucket::full(now);
                    b.short = brought;
                    self.named[i] = Some((id, b));
                }
                Slot::Named(i)
            }
            None => Slot::Overflow,
        }
    }

    /// `by`'s owed-only entry's count, if it has one.
    fn entry(&self, by: &Actor) -> Option<u64> {
        self.owed_only
            .iter()
            .find(|(a, _)| a == by)
            .map(|&(_, n)| n)
    }

    /// Remove `by`'s owed-only entry, returning its count.
    fn take_entry(&mut self, by: &Actor) -> Option<u64> {
        let i = self.owed_only.iter().position(|(a, _)| a == by)?;
        self.owed_only.remove(i).map(|(_, n)| n)
    }

    /// `n` more marks `by`, an author with no named slot, was short of: its
    /// entry, now the most recently noted. A new entry past
    /// [`MARK_OWED_AUTHORS`] spills the least recently noted one's count
    /// into the shared pool.
    fn owe(&mut self, by: Actor, n: u64) {
        if n == 0 {
            return;
        }
        let count = self.take_entry(&by).unwrap_or(0).saturating_add(n);
        if self.owed_only.len() >= MARK_OWED_AUTHORS
            && let Some((_, spilled)) = self.owed_only.pop_front()
        {
            self.overflow.short = self.overflow.short.saturating_add(spilled);
        }
        self.owed_only.push_back((by, count));
    }

    /// The bucket of `slot`. A named slot is filled before it is spent
    /// from ([`Self::slot`] with `take`); an empty one reads as the overflow.
    fn bucket(&mut self, slot: Slot) -> &mut Bucket {
        match slot {
            Slot::Harness => &mut self.harness,
            Slot::Bridge => &mut self.bridge,
            Slot::Ctl => &mut self.ctl,
            Slot::Human => &mut self.human,
            Slot::Overflow => &mut self.overflow,
            Slot::Named(i) => match &mut self.named[i] {
                Some((_, b)) => b,
                None => &mut self.overflow,
            },
        }
    }

    /// Whether `by` may mark at `now_mono_ms`: `Err(retry_ms)`, how long until
    /// it may, when its bucket is short of a token. Spends nothing.
    ///
    /// # Errors
    ///
    /// `retry_ms` while `by`'s bucket holds less than one token.
    pub fn peek(&mut self, by: &Actor, now_mono_ms: u64) -> Result<(), u64> {
        let slot = self.slot(by, now_mono_ms, false);
        match slot {
            // A slot not yet taken is a full bucket.
            Slot::Named(i) if !self.holds(i, by) => Ok(()),
            _ => self.bucket(slot).peek(now_mono_ms),
        }
    }

    /// Spend one of `by`'s tokens at `now_mono_ms` (after its mark was
    /// appended: a token is spent only by a mark that was).
    pub fn spend(&mut self, by: &Actor, now_mono_ms: u64) {
        let slot = self.slot(by, now_mono_ms, true);
        self.bucket(slot).spend(now_mono_ms);
    }

    /// `by` was short of a token at `now_mono_ms` and its mark was counted,
    /// not made: one more in its own count, which its next mark says
    /// ([`Self::owed`]). A session that holds no slot yet takes one as
    /// [`Self::spend`] would; an author past the named slots counts in its
    /// owed-only entry (the module doc).
    pub fn note_short(&mut self, by: &Actor, now_mono_ms: u64) {
        match self.slot(by, now_mono_ms, true) {
            Slot::Overflow => self.owe(*by, 1),
            slot => {
                let b = self.bucket(slot);
                b.short = b.short.saturating_add(1);
            }
        }
    }

    /// What `by`'s next mark says ([`Self::note_short`], [`Self::said`]): its
    /// own count (a trusted actor's bucket's, the named slot's it holds, or
    /// its owed-only entry's), else, for an author past the named slots with
    /// none, the shared pool's. A session that holds no slot while one is
    /// there to take (free, or idle) says its entry's count or nothing,
    /// never that slot's last holder's or the pool's.
    pub fn owed(&mut self, by: &Actor, now_mono_ms: u64) -> Owed {
        match self.slot(by, now_mono_ms, false) {
            Slot::Named(i) if !self.holds(i, by) => Owed::Own(self.entry(by).unwrap_or(0)),
            Slot::Overflow => match self.entry(by) {
                Some(n) => Owed::Own(n),
                None => Owed::Shared(self.overflow.short),
            },
            slot => Owed::Own(self.bucket(slot).short),
        }
    }

    /// A mark from `by` said what [`Self::owed`] gave it: that count alone
    /// starts again from zero.
    pub fn said(&mut self, by: &Actor, now_mono_ms: u64) {
        match self.slot(by, now_mono_ms, false) {
            Slot::Named(i) if !self.holds(i, by) => {
                let _ = self.take_entry(by);
            }
            Slot::Overflow => {
                if self.take_entry(by).is_none() {
                    self.overflow.short = 0;
                }
            }
            slot => self.bucket(slot).short = 0,
        }
    }

    /// Whether named slot `i` is `by`'s own.
    fn holds(&self, i: usize, by: &Actor) -> bool {
        match (by, &self.named[i]) {
            (Actor::Sid(id), Some((sid, _))) => sid == id,
            _ => false,
        }
    }
}

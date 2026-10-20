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
//! ([`MarkLimiter::note_short`]), keeps the count IN THAT BUCKET, and the
//! next mark made from the same bucket says it ([`MarkLimiter::owed`],
//! [`MarkLimiter::said`]): a trusted actor's own, a session's while it
//! holds its named slot, and the overflow's for every author that shares
//! it. So a trusted actor's count, and a session's while it holds a named
//! slot, never rides another author's mark; a session that takes a slot
//! says nothing its last holder owed, and a named slot owing a count is
//! given to another session only when no free slot or idle slot owing
//! nothing is left, its count going with it, unsaid. The overflow's
//! authors (the sessions past the table, and `-`, `aterm` and a vendor)
//! share ONE count as they share one bucket: any of them may say what
//! another was short of, its `dropped=` naming the bucket, not the author.
//! A count per overflowed author is not kept: an Owner can mint any number
//! of edges, so a table of them would be one more bounded table with an
//! overflow of its own.

use crate::vocab::{Actor, ShortId};

/// The most marks one author makes at once.
pub const MARK_BURST: u32 = 20;
/// How long one token takes to come back (1 per second).
pub const MARK_REFILL_MS: u64 = 1000;
/// The named buckets sessions share before the overflow.
pub const MARK_ACTORS: usize = 16;

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

/// One session's mark limiter (see the module doc).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkLimiter {
    harness: Bucket,
    bridge: Bucket,
    ctl: Bucket,
    human: Bucket,
    named: [Option<(ShortId, Bucket)>; MARK_ACTORS],
    overflow: Bucket,
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
                    self.named[i] = Some((id, Bucket::full(now)));
                }
                Slot::Named(i)
            }
            None => Slot::Overflow,
        }
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
    /// not made: one more in its bucket's count, which the next mark from
    /// that bucket says ([`Self::owed`]). A session that holds no slot yet
    /// takes one as [`Self::spend`] would, else counts in the overflow.
    pub fn note_short(&mut self, by: &Actor, now_mono_ms: u64) {
        let slot = self.slot(by, now_mono_ms, true);
        let b = self.bucket(slot);
        b.short = b.short.saturating_add(1);
    }

    /// How many marks `by`'s bucket was short of since one last said so
    /// ([`Self::note_short`], [`Self::said`]): its own, the named slot it
    /// holds, or the overflow it shares. `0` for a session that holds no
    /// slot while one is there to take (free, or idle), whatever that slot's
    /// last holder or the overflow owes.
    pub fn owed(&mut self, by: &Actor, now_mono_ms: u64) -> u64 {
        match self.slot(by, now_mono_ms, false) {
            Slot::Named(i) if !self.holds(i, by) => 0,
            slot => self.bucket(slot).short,
        }
    }

    /// A mark from `by` said its bucket's count ([`Self::owed`]): that
    /// bucket's alone starts again from zero.
    pub fn said(&mut self, by: &Actor, now_mono_ms: u64) {
        match self.slot(by, now_mono_ms, false) {
            Slot::Named(i) if !self.holds(i, by) => {}
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

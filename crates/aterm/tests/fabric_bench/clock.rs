// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The run's one clock, and the broker's side of the story told on it.
//!
//! EVERY TIME HERE IS MICROSECONDS ON ONE MONOTONIC ORIGIN ([`Clock`]), taken
//! from OUTSIDE the stack: the bench stamps a `post` before it writes the
//! request and after it reads the reply, and stamps a row VISIBLE when the
//! recipient's `await inbox` answers. Nothing reads a clock inside aterm or
//! the bridge, so no number depends on a component's own idea of time — and
//! everything downstream takes the stamps as plain integers, so the analysis is
//! unit-tested on synthetic timelines.
//!
//! WHEN A RECORD LANDED is the one moment the outside cannot see, and the
//! broker serving every world here is IN THIS PROCESS. It has two heads, each
//! one atomic load — no request, no lock, nothing that competes with the
//! bridge — and [`sample_heads`] reads both every [`SAMPLE_GAP`] and keeps
//! each change, so after the run "the record at offset `s` had passed that
//! head by `t`" is a lookup ([`Timeline::landed`]): the first change seen past
//! `s`, at most one gap (plus scheduling) after the fact.
//!
//! * The ACKED head ([`astream_broker::Broker::visible_head`]): a record below
//!   it is visible to subscribers and its publish is acked. On a strict broker
//!   it moves after the batch's full flush; on an ordered one after its
//!   barrier (an escalated record's after its full flush); on a relaxed one
//!   after the page-cache write. The bench's "acked" is this head. (Before the
//!   ordered arm existed every entry called it "durable": on the strict broker
//!   every entry measured, the two heads are one.)
//! * The DURABLE head ([`astream_broker::Broker::durable_head`]): a record
//!   below it is on stable media by a full flush the broker saw succeed. On a
//!   strict broker it moves with the acked head; on an ordered one at each
//!   full flush (an escalated record's, the horizon's); on a relaxed one only
//!   at the open and a clean close, so no record of a run is ever seen durable
//!   there, and the tables say "not seen".
//!
//! Which offset a given message took is read off the log once the broker
//! has stopped (`census.rs`), so no timed window spends a request on the
//! broker. Two things touch it inside one, both named where they happen: the
//! messaging phases' own posts, which are the traffic being measured, and the
//! one subscription an attach round holds to know when to move on (`run.rs`,
//! `await_attach`) — served as each of the bridge's own subscribers is.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The run's origin. `Copy`: every thread stamps on the same one.
#[derive(Debug, Clone, Copy)]
pub struct Clock(Instant);

impl Clock {
    /// Start the run's clock now.
    #[must_use]
    pub fn start() -> Self {
        Self(Instant::now())
    }

    /// Microseconds since the origin.
    #[must_use]
    pub fn us(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

/// `b - a` in milliseconds, for two stamps on one [`Clock`].
#[must_use]
pub fn ms(a: u64, b: u64) -> f64 {
    (b as f64 - a as f64) / 1000.0
}

/// How often [`sample_heads`] looks. NOT A SYNCHRONISATION SLEEP: nothing waits
/// on the sampler — it is a stopwatch beside the run, and the gap only bounds
/// how late it can stamp a change (one Strict fsync on this hardware is
/// milliseconds, and an ordered barrier a few tenths of one, so a tenth of a
/// millisecond is the resolution both need) while keeping it off a core.
pub const SAMPLE_GAP: Duration = Duration::from_micros(100);

/// A head over time: `(us, head)`, both ascending, one entry per change
/// [`sample_heads`] saw (the first entry is the head when it started).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline(Vec<(u64, u64)>);

impl Timeline {
    /// When the record at offset `seq` was first seen below the head: the
    /// first entry whose head is past it. `None` when the sampler never saw it
    /// pass (it stopped first, or — the durable head of a relaxed broker — the
    /// head never moved) — or when it had already passed before the sampler
    /// started, which no measured record here has.
    #[must_use]
    pub fn landed(&self, seq: u64) -> Option<u64> {
        if self.0.first().is_some_and(|&(_, h)| h > seq) {
            return None;
        }
        let i = self.0.partition_point(|&(_, h)| h <= seq);
        self.0.get(i).map(|&(t, _)| t)
    }

    /// How many changes it saw.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// The two heads over one run: the acked (visible) and the durable one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heads {
    /// [`astream_broker::Broker::visible_head`]'s changes.
    pub acked: Timeline,
    /// [`astream_broker::Broker::durable_head`]'s changes.
    pub durable: Timeline,
}

/// Watch both heads until `stop` is set, keeping each one's changes, stamped
/// on `clock`. `heads` answers `(acked, durable)`: in the run, two atomic
/// loads of the in-process broker ([`astream_broker::Broker::visible_head`]
/// and [`astream_broker::Broker::durable_head`]), each monotone because the
/// log only grows (no retention runs here). Read in one call, so the two
/// timelines share their stamps.
pub fn sample_heads(clock: Clock, heads: impl Fn() -> (u64, u64), stop: &AtomicBool) -> Heads {
    let mut acked: Vec<(u64, u64)> = Vec::with_capacity(4096);
    let mut durable: Vec<(u64, u64)> = Vec::with_capacity(4096);
    loop {
        let (a, d) = heads();
        let now = clock.us();
        if acked.last().is_none_or(|&(_, last)| last != a) {
            acked.push((now, a));
        }
        if durable.last().is_none_or(|&(_, last)| last != d) {
            durable.push((now, d));
        }
        if stop.load(Ordering::Relaxed) {
            return Heads {
                acked: Timeline(acked),
                durable: Timeline(durable),
            };
        }
        std::thread::sleep(SAMPLE_GAP);
    }
}

/// Sets its flag when dropped — on the unwind of a failing phase too, so a
/// scoped sampler always stops and the scope that joins it never hangs.
pub struct StopOnDrop<'a>(pub &'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_record_lands_at_the_first_head_seen_past_it() {
        use super::*;

        // Head 10 at the start; 12 by t=150; 13 by t=400.
        let t = Timeline(vec![(0, 10), (150, 12), (400, 13)]);
        assert_eq!(t.landed(10), Some(150), "offset 10 is the 11th record");
        assert_eq!(t.landed(11), Some(150));
        assert_eq!(t.landed(12), Some(400));
        assert_eq!(t.landed(13), None, "not yet seen");
        assert_eq!(t.landed(9), None, "landed before the sampler started");
        assert_eq!(Timeline::default().landed(0), None);
        // A head that never moved (a relaxed broker's durable head): nothing
        // is ever seen past it.
        let still = Timeline(vec![(0, 10)]);
        assert_eq!(still.landed(10), None);
    }

    #[test]
    fn the_sampler_keeps_changes_only_and_stops_on_its_flag() {
        use super::*;

        use std::sync::atomic::AtomicU64;
        let acked = AtomicU64::new(5);
        let durable = AtomicU64::new(3);
        let stop = AtomicBool::new(false);
        let clock = Clock::start();
        let heads = std::thread::scope(|s| {
            let sampler = s.spawn(|| {
                sample_heads(
                    clock,
                    || {
                        (
                            acked.load(Ordering::Relaxed),
                            durable.load(Ordering::Relaxed),
                        )
                    },
                    &stop,
                )
            });
            let _stop = StopOnDrop(&stop);
            acked.store(6, Ordering::Relaxed);
            acked.store(8, Ordering::Relaxed);
            durable.store(8, Ordering::Relaxed);
            drop(_stop);
            sampler.join().expect("the sampler returns")
        });
        // Whatever interleaving the scheduler chose, each timeline is changes
        // only, ascending in both fields, starting at a value its head held.
        for (timeline, held) in [
            (&heads.acked, &[5, 6, 8][..]),
            (&heads.durable, &[3, 8][..]),
        ] {
            let entries = &timeline.0;
            assert!(!entries.is_empty());
            assert!(held.contains(&entries[0].1));
            assert!(
                entries
                    .windows(2)
                    .all(|w| w[0].0 <= w[1].0 && w[0].1 < w[1].1)
            );
            assert!(timeline.len() <= held.len());
        }
    }
}

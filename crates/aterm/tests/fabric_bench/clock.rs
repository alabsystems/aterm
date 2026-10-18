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
//! broker serving every world here is IN THIS PROCESS. Its subscriber-visible
//! head ([`astream_broker::Broker::visible_head`]) is one atomic load — no
//! request, no lock, nothing that competes with the bridge — and it moves only
//! once a batch is durable (a guarded broker is Strict: the fsync comes first,
//! then the head, then the acks). [`sample_heads`] reads it every
//! [`SAMPLE_GAP`] and keeps each change, so after the run "the record at offset
//! `s` was durable and visible by `t`" is a lookup ([`Timeline::landed`]):
//! the first change seen past `s`, at most one gap (plus scheduling) after the
//! fact. Which offset a given message took is read off the log once the broker
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
/// milliseconds, so a tenth of one is resolution to spare) while keeping it off
/// a core.
pub const SAMPLE_GAP: Duration = Duration::from_micros(100);

/// The head over time: `(us, head)`, both ascending, one entry per change
/// [`sample_heads`] saw (the first entry is the head when it started).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline(Vec<(u64, u64)>);

impl Timeline {
    /// A timeline from its entries. Pure (for tests and the sampler).
    #[must_use]
    pub fn from_entries(entries: Vec<(u64, u64)>) -> Self {
        Self(entries)
    }

    /// When the record at offset `seq` was first seen durable: the first entry
    /// whose head is past it. `None` when the sampler never saw it land (it
    /// stopped first) — or when it had already landed before the sampler
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

/// Watch `head` until `stop` is set, keeping each change, stamped on `clock`.
///
/// `head` is [`astream_broker::Broker::visible_head`] in the run: an atomic
/// load, monotone because the log only grows (no retention runs here).
pub fn sample_heads(clock: Clock, head: impl Fn() -> u64, stop: &AtomicBool) -> Timeline {
    let mut entries: Vec<(u64, u64)> = Vec::with_capacity(4096);
    loop {
        let h = head();
        if entries.last().is_none_or(|&(_, last)| last != h) {
            entries.push((clock.us(), h));
        }
        if stop.load(Ordering::Relaxed) {
            return Timeline(entries);
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
    use super::*;

    #[test]
    fn a_record_lands_at_the_first_head_seen_past_it() {
        // Head 10 at the start; 12 by t=150; 13 by t=400.
        let t = Timeline::from_entries(vec![(0, 10), (150, 12), (400, 13)]);
        assert_eq!(t.landed(10), Some(150), "offset 10 is the 11th record");
        assert_eq!(t.landed(11), Some(150));
        assert_eq!(t.landed(12), Some(400));
        assert_eq!(t.landed(13), None, "not yet seen");
        assert_eq!(t.landed(9), None, "landed before the sampler started");
        assert_eq!(Timeline::default().landed(0), None);
    }

    #[test]
    fn the_sampler_keeps_changes_only_and_stops_on_its_flag() {
        use std::sync::atomic::AtomicU64;
        let head = AtomicU64::new(5);
        let stop = AtomicBool::new(false);
        let clock = Clock::start();
        let timeline = std::thread::scope(|s| {
            let sampler = s.spawn(|| sample_heads(clock, || head.load(Ordering::Relaxed), &stop));
            let _stop = StopOnDrop(&stop);
            head.store(6, Ordering::Relaxed);
            head.store(8, Ordering::Relaxed);
            drop(_stop);
            sampler.join().expect("the sampler returns")
        });
        // Whatever interleaving the scheduler chose, the entries are changes
        // only, ascending in both fields, starting at a value the head held.
        let entries = &timeline.0;
        assert!(!entries.is_empty());
        assert!([5, 6, 8].contains(&entries[0].1));
        assert!(
            entries
                .windows(2)
                .all(|w| w[0].0 <= w[1].0 && w[0].1 < w[1].1)
        );
        assert!(timeline.len() <= 3);
    }
}

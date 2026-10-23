// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Monotonic benchmark clock and broker head sampling ([`Clock`], [`sample_heads`],
//! [`SAMPLE_GAP`], [`Timeline::landed`], [`astream_broker::Broker::visible_head`],
//! [`astream_broker::Broker::durable_head`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The run's origin.
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

/// How often [`sample_heads`] looks.
pub const SAMPLE_GAP: Duration = Duration::from_micros(100);

/// A head over time ([`sample_heads`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline(Vec<(u64, u64)>);

impl Timeline {
    /// When the record at offset `seq` was first seen below the head.
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

/// Watch both heads until `stop` is set, keeping each one's changes, stamped on `clock`.
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

/// Sets its flag when dropped.
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

        let t = Timeline(vec![(0, 10), (150, 12), (400, 13)]);
        assert_eq!(t.landed(10), Some(150), "offset 10 is the 11th record");
        assert_eq!(t.landed(11), Some(150));
        assert_eq!(t.landed(12), Some(400));
        assert_eq!(t.landed(13), None, "not yet seen");
        assert_eq!(t.landed(9), None, "landed before the sampler started");
        assert_eq!(Timeline::default().landed(0), None);
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

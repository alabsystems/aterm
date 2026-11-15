// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Monotonic benchmark clock and head sampling ([`Clock`], [`sample_heads`],
//! [`SAMPLE_GAP`], [`Timeline::landed`]). The sampler accepts a reader closure;
//! its clock and observation arithmetic do not depend on a broker or a GUI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The run's origin.
#[derive(Debug, Clone, Copy)]
pub struct Clock(Instant);

impl Clock {
    /// Start the run's clock now.
    #[must_use]
    #[inline]
    pub fn start() -> Self {
        Self(Instant::now())
    }

    /// Microseconds since the origin.
    #[must_use]
    #[inline]
    pub fn us(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

/// `b - a` in milliseconds, for two stamps on one [`Clock`].
#[must_use]
#[inline]
pub fn ms(a: u64, b: u64) -> f64 {
    (b as f64 - a as f64) / 1000.0
}

/// The requested sleep between head observations; scheduling can make it longer.
pub const SAMPLE_GAP: Duration = Duration::from_micros(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeadChange {
    previous: u64,
    observed: u64,
    head: u64,
}

/// A sampled head over time. A rewind or overlapping clock brackets makes its
/// timestamps unavailable, since offsets cannot then identify one progression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline {
    changes: Vec<HeadChange>,
    last_read: Option<(u64, u64)>,
    valid: bool,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            changes: Vec::new(),
            last_read: None,
            valid: true,
        }
    }
}

impl Timeline {
    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        Self {
            changes: Vec::with_capacity(capacity),
            ..Self::default()
        }
    }

    #[inline]
    fn observe(&mut self, started: u64, observed: u64, head: u64) {
        self.valid &= started <= observed
            && self.last_read.is_none_or(|(_, end)| end <= started)
            && self.changes.last().is_none_or(|last| last.head <= head);
        if self.changes.last().is_none_or(|last| last.head != head) {
            self.changes.push(HeadChange {
                previous: self.last_read.map_or(started, |(start, _)| start),
                observed,
                head,
            });
        }
        self.last_read = Some((started, observed));
    }

    fn change_past(&self, seq: u64) -> Option<&HeadChange> {
        if !self.valid || self.changes.first().is_some_and(|first| first.head > seq) {
            return None;
        }
        let i = self.changes.partition_point(|change| change.head <= seq);
        self.changes.get(i)
    }

    /// When the record at offset `seq` was first seen below the head.
    #[must_use]
    pub fn landed(&self, seq: u64) -> Option<u64> {
        self.change_past(seq).map(|change| change.observed)
    }

    /// The latency interval consistent with bracketing head observations, in
    /// milliseconds from `start`. Both bounds allow one microsecond for clock
    /// quantization. A record already present at the first observation is unknown.
    #[must_use]
    pub fn latency_bounds_ms(&self, start: u64, seq: u64) -> Option<(f64, f64)> {
        let change = self.change_past(seq)?;
        let lower = i128::from(change.previous) - i128::from(start) - 1;
        let upper = i128::from(change.observed) - i128::from(start) + 1;
        Some((lower as f64 / 1000.0, upper as f64 / 1000.0))
    }

    /// Whether sampled heads and their read brackets stayed ordered.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// Whether it has recorded no head observations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// How many changes it saw.
    #[must_use]
    pub fn len(&self) -> usize {
        self.changes.len()
    }
}

/// The two heads over one run: the acked (visible) and the durable one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heads {
    /// The reader's subscriber-visible head changes.
    pub acked: Timeline,
    /// The reader's durable head changes.
    pub durable: Timeline,
}

/// Watch both heads until `stop` is set, keeping each one's changes, stamped on `clock`.
pub fn sample_heads(clock: Clock, heads: impl Fn() -> (u64, u64), stop: &AtomicBool) -> Heads {
    let mut out = Heads {
        acked: Timeline::with_capacity(4096),
        durable: Timeline::with_capacity(4096),
    };
    loop {
        let started = clock.us();
        let (a, d) = heads();
        let observed = clock.us();
        out.acked.observe(started, observed, a);
        out.durable.observe(started, observed, d);
        if stop.load(Ordering::Relaxed) {
            return out;
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
#[path = "head_sampling_tests.rs"]
mod tests;

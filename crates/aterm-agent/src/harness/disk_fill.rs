// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROJECTION (`docs/DESIGN-disk-free-ask-2026-10-24.md` §8.1): where a volume's fill takes
//! its free space. The window's disk watch feeds each look's reading to the volume's [`Fill`],
//! which reads the slope with System State's own arithmetic (`aterm_vitals::history`) and says,
//! once the fill has gone on for [`STAND_MS`], when the volume crosses its floor: to the pass
//! while it stands ([`Fill::pressing`]), and to the ask while the last look still crossed
//! ([`Fill::crossing`]).

use aterm_vitals::history::{FILLING_FOR_MS, Rates, rates_within, time_to_line};
use aterm_vitals::ring::{BucketRing, RING_BUCKETS};

/// How far ahead a crossing counts: two hours (§8.1, "Why two hours").
pub const HORIZON_MS: i64 = 2 * 3_600_000;

/// How long every look must project a crossing before the projection stands, and must not before
/// it lapses: System State's own wait before it says "full in" ([`FILLING_FOR_MS`]).
pub const STAND_MS: i64 = FILLING_FOR_MS;

/// How many 10 s buckets late a span's first reading may start: two looks.
pub const SLACK_BUCKETS: i64 = 12;

/// The ring's buckets: the hour's span, whole, with its slack.
pub const RING: usize = RING_BUCKETS + 2 * SLACK_BUCKETS as usize;

/// A crossing a fill projects: the fill rate and where it takes the volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Projection {
    /// The fill rate, bytes a second.
    pub rate_bps: u64,
    /// Milliseconds until free space falls to [`Self::floor`] at that rate (`0` at or below it).
    pub eta_ms: i64,
    /// What the fill takes over [`HORIZON_MS`], at most the volume's size.
    pub need: u64,
    /// The floor it was read against.
    pub floor: u64,
}

impl Projection {
    /// The projection at `rate_bps` from `free` bytes against `floor` on a volume of `total`.
    #[must_use]
    pub fn of(rate_bps: u64, free: u64, floor: u64, total: Option<u64>) -> Projection {
        let rate = rate_bps.max(1);
        let need = rate.saturating_mul(u64::try_from(HORIZON_MS / 1_000).unwrap_or(0));
        Projection {
            rate_bps: rate,
            eta_ms: i64::try_from(free.saturating_sub(floor) / rate)
                .unwrap_or(i64::MAX / 1_000)
                .saturating_mul(1_000),
            need: total.map_or(need, |t| need.min(t)),
            floor,
        }
    }

    /// The same rate read from `free` bytes against `floor`.
    #[must_use]
    pub fn at(&self, free: u64, floor: u64, total: Option<u64>) -> Projection {
        Projection::of(self.rate_bps, free, floor, total)
    }

    /// The fill rate an hour, as the line says it (`130.8 GiB/h`).
    #[must_use]
    pub fn per_hour(&self) -> String {
        format!(
            "{}/h",
            super::human_bytes(self.rate_bps.saturating_mul(3_600))
        )
    }

    /// When it crosses, as System State says a time to full (`~13 min`).
    #[must_use]
    pub fn eta_words(&self) -> String {
        aterm_vitals::words::duration_approx(self.eta_ms as f64 / 1_000.0)
    }

    /// One clause for a pass's row and the log: `projected under the 47.0 GiB floor in ~13 min,
    /// falling 130.8 GiB/h`.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "projected under the {} floor in {}, falling {}",
            super::human_bytes(self.floor),
            self.eta_words(),
            self.per_hour()
        )
    }
}

/// The least a 5- and a 10-minute rate must read for the volume to fill now: 1 MiB/s, 3.5 GiB an
/// hour. A volume's background writes (logs, Spotlight, swap, caches) move a two-point slope by tens
/// of KB/s either way, so a sign test would hold a fill that stopped on that noise.
pub const NOISE_BPS: f64 = 1_048_576.0;

/// A look later than this after the last one ends a streak that has not stood (§8.1): a pass that
/// ran long, not a sleep.
pub const GAP_MS: i64 = 5 * 60_000;

/// What one look's rates say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// A crossing within the horizon.
    Crossing(Projection),
    /// No crossing within it, or the volume is not filling now.
    Clear,
    /// The ring does not yet span ten minutes.
    Unknown,
}

/// One volume's fill: its readings, net of what this process freed, and whether a projection
/// stands.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    ring: BucketRing,
    /// What this process's passes and reserve freed since the ring began, as free space measured
    /// it ([`Fill::credit`]).
    credit: u64,
    /// The projection that stands, as its last crossing read it.
    standing: Option<Projection>,
    /// When the streak began that stands a projection (none standing) or lapses it (one standing).
    since_ms: Option<i64>,
    /// When the last look read.
    last_ms: Option<i64>,
    /// The last look projected a crossing.
    crossed: bool,
    /// The fill rate the last look read, bytes a second; `0.0` where it read none.
    rate_bps: f64,
    /// A look has read a 10-minute rate.
    warm: bool,
}

impl Default for Fill {
    fn default() -> Self {
        Fill {
            ring: BucketRing::with_capacity(RING),
            credit: 0,
            standing: None,
            since_ms: None,
            last_ms: None,
            crossed: false,
            rate_bps: 0.0,
            warm: false,
        }
    }
}

/// What a pass freed, as free space measured it: the least of what its removers counted and what
/// free space rose across it plus what the fill took meanwhile at `rate_bps`. Counted bytes alone
/// would read blocks a local snapshot or a clone still holds as a fill the size of the pass.
#[must_use]
pub fn credited(
    counted: u64,
    free_before: u64,
    free_after: u64,
    rate_bps: f64,
    pass_ms: i64,
) -> u64 {
    let took = (rate_bps.max(0.0) * pass_ms.max(0) as f64 / 1_000.0) as u64;
    counted.min(free_after.saturating_sub(free_before).saturating_add(took))
}

impl Fill {
    /// A fill that has read nothing.
    #[must_use]
    pub fn new() -> Fill {
        Fill::default()
    }

    /// Records the look at `now_ms` that read `free` bytes against `floor` on a volume of `total`,
    /// and returns the projection standing after it ([`Self::pressing`]).
    pub fn observe(
        &mut self,
        now_ms: i64,
        free: u64,
        total: Option<u64>,
        floor: u64,
    ) -> Option<Projection> {
        let used = self.credit as f64 - free as f64;
        let _ = self.ring.push(now_ms, Some(used), false);
        let gap = self
            .last_ms
            .is_some_and(|last| now_ms.saturating_sub(last) > GAP_MS);
        self.last_ms = Some(now_ms);
        let read = self.read(free, total, floor);
        self.crossed = matches!(read, Read::Crossing(_));
        self.warm |= read != Read::Unknown;
        match (self.standing, read) {
            (None, Read::Crossing(p)) => {
                if gap {
                    self.since_ms = None;
                }
                let since = *self.since_ms.get_or_insert(now_ms);
                if now_ms.saturating_sub(since) >= STAND_MS {
                    self.standing = Some(p);
                    self.since_ms = None;
                }
            }
            (None, Read::Clear | Read::Unknown) => self.since_ms = None,
            (Some(_), Read::Crossing(p)) => {
                self.standing = Some(p);
                self.since_ms = None;
            }
            (Some(_), Read::Clear | Read::Unknown) => {
                let since = *self.since_ms.get_or_insert(now_ms);
                if now_ms.saturating_sub(since) >= STAND_MS {
                    self.standing = None;
                    self.since_ms = None;
                }
            }
        }
        self.pressing()
    }

    /// Credits a pass that ran `pass_ms` and freed `counted` bytes as its removers counted them,
    /// free space reading `free_before` before it and `free_after` after ([`credited`]): what
    /// later readings add to the space used.
    pub fn credit(&mut self, counted: u64, free_before: u64, free_after: u64, pass_ms: i64) {
        self.credit = self.credit.saturating_add(credited(
            counted,
            free_before,
            free_after,
            self.rate_bps,
            pass_ms,
        ));
    }

    /// The standing projection, lapsing or not: what presses the pass (§8.2).
    #[must_use]
    pub fn pressing(&self) -> Option<Projection> {
        self.standing
    }

    /// The standing projection read from `free` bytes against `floor`, while the last look projected
    /// a crossing and the crossing from `free` is within the horizon: what presses the ask (§8.3).
    /// A pass that freed the horizon's fill presses nothing more of it.
    #[must_use]
    pub fn crossing(&self, free: u64, floor: u64, total: Option<u64>) -> Option<Projection> {
        self.standing
            .filter(|_| self.crossed)
            .map(|p| p.at(free, floor, total))
            .filter(|p| p.eta_ms <= HORIZON_MS)
    }

    /// No look has read a 10-minute rate yet: a process started under ten minutes ago (§8.3).
    #[must_use]
    pub fn cold(&self) -> bool {
        !self.warm
    }

    /// The rates the ring reads now.
    #[must_use]
    pub fn rates(&self) -> Rates {
        rates_within(&self.ring, SLACK_BUCKETS)
    }

    /// The fill rate: the 10-minute rate, raised to the hour's while the 10-minute rate is at least
    /// half of it, where the 5- and the 10-minute rates both read at least [`NOISE_BPS`];
    /// `Ok(None)` where either reads less, and `Err(())` while the ring does not span ten minutes.
    /// The 5-minute rate is read for that only: one file written once leaves it five minutes later.
    fn rate(&self) -> Result<Option<f64>, ()> {
        let r = self.rates();
        let (five, ten) = r.rate5.zip(r.rate10).ok_or(())?;
        if five < NOISE_BPS || ten < NOISE_BPS {
            return Ok(None);
        }
        Ok(Some(match r.rate60 {
            Some(hour) if ten >= hour / 2.0 => ten.max(hour),
            _ => ten,
        }))
    }

    fn read(&mut self, free: u64, total: Option<u64>, floor: u64) -> Read {
        let rate = self.rate();
        self.rate_bps = rate.ok().flatten().unwrap_or(0.0);
        let Ok(rate) = rate else {
            return Read::Unknown;
        };
        match time_to_line(free, floor, rate) {
            Some(secs) if secs * 1_000.0 <= HORIZON_MS as f64 => Read::Crossing(Projection::of(
                rate.map_or(1, |r| r.ceil() as u64),
                free,
                floor,
                total,
            )),
            _ => Read::Clear,
        }
    }
}

/// Planted defects the pass's conformance test arms to show it rejects them
/// (`crates/aterm-agent/tests/conformance_disk_pass.rs`).
#[cfg(any(test, debug_assertions))]
pub mod fault {
    use std::cell::Cell;

    /// One planted defect.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Fault {
        /// A gone owner's build cache keeps the day's quiet window under a projection, as before
        /// revision 2: the night's 118.6 GiB of removed worktrees' caches stayed.
        GoneQuietUnderProjection,
        /// Tier 4 under a projection above the floor takes profiles unwritten for ten minutes, not
        /// an hour: a session paused between builds loses its incremental cache.
        TenMinutesOnProjection,
    }

    thread_local! {
        static ARMED: Cell<Option<Fault>> = const { Cell::new(None) };
    }

    /// Run `f` with `fault` armed on this thread, disarmed after it even on unwind.
    pub fn with<T>(fault: Fault, f: impl FnOnce() -> T) -> T {
        struct Disarm;
        impl Drop for Disarm {
            fn drop(&mut self) {
                ARMED.with(|a| a.set(None));
            }
        }
        ARMED.with(|a| a.set(Some(fault)));
        let _disarm = Disarm;
        f()
    }

    /// Whether `fault` is armed on this thread.
    #[must_use]
    pub fn armed(fault: Fault) -> bool {
        ARMED.with(|a| a.get() == Some(fault))
    }
}

#[cfg(test)]
#[path = "disk_fill_tests.rs"]
mod tests;

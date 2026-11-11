// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window-secant ETA estimator and quantized remaining/elapsed time formatting.

use std::collections::VecDeque;

use crate::model::{Amount, Unit};
use crate::{
    COUNT_STALL_GAPS, Duration, ELAPSED_AFTER, ETA_OVERDUE_MIN, Instant, PROJECTIONS_CAP,
    RATE_MIN_SPAN, RATE_WINDOW, SAMPLE_MIN_GAP, SAMPLES_CAP, STABLE_MIN, STABLE_SPAN, STALL_AFTER,
};

/// Past this a projection is not a projection (a stalled trickle, a
/// mis-sized total): it is discarded rather than voted.
const PROJECTION_MAX: Duration = Duration::from_hours(48);

/// Milliseconds of `d`, saturating.
#[inline]
fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `a − b` in milliseconds, 0 when `b` is later.
#[inline]
fn ms_between(a: Instant, b: Instant) -> u64 {
    ms(a.saturating_duration_since(b))
}

/// One observation kept in the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    at: Instant,
    done: u64,
}

/// Estimated remaining time state for a determinate progress row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eta {
    /// No estimate shown.
    Hidden,
    /// Estimated remaining duration.
    Remaining(Duration),
    /// Progress has stalled (`STALL_AFTER` elapsed without advance).
    Stalled,
}

/// Bounded window-secant ETA estimator for a live determinate row.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ProgressTrack {
    series: Option<u64>,
    unit: Option<Unit>,
    total: u64,
    samples: VecDeque<Sample>,
    newest: Option<Sample>,
    first_done: Option<u64>,
    advanced: bool,
    last_advance: Option<Instant>,
    /// When the series began (its first reading), and how many readings
    /// since moved `done` forward: a count's own pace ([`COUNT_STALL_GAPS`]).
    began: Option<Instant>,
    advances: u32,
    /// When the held phase now running began (its first held reading).
    held_from: Option<Instant>,
    projections: VecDeque<(Instant, Instant)>,
    anchor: Option<Instant>,
    /// How long past `anchor` the latch may stand with nothing new: the
    /// larger of [`ETA_OVERDUE_MIN`] and a fifth of the estimate it latched.
    overdue_grace: Duration,
}

impl ProgressTrack {
    /// Record an [`Amount`] sample at `now`, resetting if the series, unit, or progress regresses.
    pub fn observe(&mut self, now: Instant, a: Amount) {
        let regressed = self.newest.is_some_and(|n| a.done < n.done);
        let same_count = self.unit.is_some_and(|u| u.count() == a.unit.count());
        if self.series != Some(a.series) || !same_count || regressed {
            *self = Self {
                series: Some(a.series),
                unit: Some(a.unit),
                total: a.total,
                ..Self::default()
            };
        } else if self.total != a.total {
            self.total = a.total;
            self.projections.clear();
            self.anchor = None;
        }
        if self.unit == Some(Unit::HeldSteps) && a.unit != Unit::HeldSteps {
            self.last_advance = Some(now);
            if let Some(from) = self.held_from.take() {
                let held = now.saturating_duration_since(from);
                self.began = self.began.and_then(|b| b.checked_add(held));
            }
        }
        if a.unit == Unit::HeldSteps && self.held_from.is_none() {
            self.held_from = Some(now);
        }
        self.unit = Some(a.unit);
        if let Some(n) = self.newest
            && a.done > n.done
        {
            self.last_advance = Some(now);
            self.advances = self.advances.saturating_add(1);
        }
        self.began.get_or_insert(now);
        let first = *self.first_done.get_or_insert(a.done);
        if a.done > first {
            self.advanced = true;
        }
        if self.last_advance.is_none() {
            self.last_advance = Some(now);
        }
        let sample = Sample {
            at: now,
            done: a.done,
        };
        self.newest = Some(sample);
        let keep = self
            .samples
            .back()
            .is_none_or(|b| now.saturating_duration_since(b.at) >= SAMPLE_MIN_GAP);
        if !keep {
            return;
        }
        self.samples.push_back(sample);
        while self.samples.len() > SAMPLES_CAP {
            self.samples.pop_front();
        }
        if let Some(t) = self.project(now) {
            self.vote(now, t);
        }
    }

    /// The window secant's projected completion instant at `now`, or `None`
    /// when the rate is not yet valid (under [`RATE_MIN_SPAN`], fewer than
    /// three samples in the window, nothing moved) or the projection is past
    /// [`PROJECTION_MAX`].
    fn project(&self, now: Instant) -> Option<Instant> {
        let newest = self.newest?;
        let (oldest, count) = self.window(now)?;
        let span = ms_between(now, oldest.at);
        if span < ms(RATE_MIN_SPAN) || count < 3 || newest.done <= oldest.done {
            return None;
        }
        let left = u128::from(self.total.saturating_sub(newest.done));
        let moved = u128::from(newest.done - oldest.done);
        let eta_ms = left * u128::from(span) / moved;
        if eta_ms > u128::from(ms(PROJECTION_MAX)) {
            return None;
        }
        now.checked_add(Duration::from_millis(u64::try_from(eta_ms).ok()?))
    }

    /// The oldest sample inside [`RATE_WINDOW`] of `now` and how many samples
    /// the window holds.
    fn window(&self, now: Instant) -> Option<(Sample, usize)> {
        let inside: Vec<&Sample> = self
            .samples
            .iter()
            .filter(|s| now.saturating_duration_since(s.at) <= RATE_WINDOW)
            .collect();
        let oldest = **inside.first()?;
        Some((oldest, inside.len()))
    }

    /// One projection votes: it joins the ring, the ring forgets what is past
    /// [`STABLE_SPAN`], and the latch is taken, kept, moved or dropped.
    fn vote(&mut self, now: Instant, t: Instant) {
        self.projections.push_back((now, t));
        while self.projections.len() > PROJECTIONS_CAP
            || self
                .projections
                .front()
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) > STABLE_SPAN)
        {
            self.projections.pop_front();
        }
        let mut votes: Vec<Instant> = self.projections.iter().map(|(_, t)| *t).collect();
        votes.sort();
        let (Some(lo), Some(hi)) = (votes.first(), votes.last()) else {
            return;
        };
        let spread = ms_between(*hi, *lo);
        let median = votes[votes.len() / 2];
        let tol = |at: Instant| ms_between(at, now).div_euclid(5).max(2000);
        let span = match (self.projections.front(), self.projections.back()) {
            (Some((a, _)), Some((b, _))) => ms_between(*b, *a),
            _ => 0,
        };
        match self.anchor {
            None => {
                if votes.len() >= 3 && span >= ms(STABLE_MIN) && spread <= tol(median) {
                    self.latch(now, median);
                }
            }
            Some(anchor) => {
                let off = if t > anchor {
                    ms_between(t, anchor)
                } else {
                    ms_between(anchor, t)
                };
                let left = ms_between(anchor, now);
                if off > (left / 4).max(3000) {
                    if spread <= tol(median) {
                        self.latch(now, median);
                    } else if spread > left / 2 {
                        self.anchor = None;
                    }
                }
            }
        }
    }

    /// Take (or move) the latch: completion at `at`, estimated at `now`.
    fn latch(&mut self, now: Instant, at: Instant) {
        self.anchor = Some(at);
        self.overdue_grace = ETA_OVERDUE_MIN.max(at.saturating_duration_since(now) / 5);
    }

    /// When the latch runs out: its completion instant plus the grace. Past
    /// it, with nothing new, the estimate is no longer an estimate.
    fn overdue_at(&self) -> Option<Instant> {
        self.anchor?.checked_add(self.overdue_grace)
    }

    /// Current [`Eta`] state at `now`.
    #[must_use]
    pub fn eta(&self, now: Instant) -> Eta {
        let Some(newest) = self.newest else {
            return Eta::Hidden;
        };
        if self.total == 0 || newest.done >= self.total {
            return Eta::Hidden;
        }
        if self.stall_at().is_some_and(|at| now >= at) {
            return Eta::Stalled;
        }
        match (self.anchor, self.overdue_at()) {
            (Some(anchor), Some(overdue)) if now < overdue => {
                Eta::Remaining(anchor.saturating_duration_since(now))
            }
            _ => Eta::Hidden,
        }
    }

    /// When the stream reads "stalled" if nothing arrives, once it has moved
    /// at all: bytes [`STALL_AFTER`] after the last advance; a count (steps,
    /// items) after [`COUNT_STALL_GAPS`] of its own mean gaps between
    /// advances, never sooner than bytes, so a stopped count says it stalled
    /// instead of counting down to `<5 s left` and going blank.
    fn stall_at(&self) -> Option<Instant> {
        if !self.advanced {
            return None;
        }
        let last = self.last_advance?;
        let after = match self.unit? {
            Unit::HeldSteps => return None,
            Unit::Bytes => STALL_AFTER,
            Unit::Items | Unit::Steps => {
                let pace = last.saturating_duration_since(self.began?) / self.advances.max(1);
                STALL_AFTER.max(pace.saturating_mul(COUNT_STALL_GAPS))
            }
        };
        last.checked_add(after)
    }

    /// Quick secant estimate of remaining duration from the oldest to newest sample.
    #[must_use]
    pub(crate) fn quick_left(&self, now: Instant) -> Option<Duration> {
        let newest = self.newest?;
        let first = *self.samples.front()?;
        if self.total == 0 || newest.done <= first.done {
            return None;
        }
        let span = u128::from(ms_between(newest.at, first.at));
        let left = u128::from(self.total.saturating_sub(newest.done)) * span
            / u128::from(newest.done - first.done);
        let left = Duration::from_millis(u64::try_from(left).ok()?);
        Some(left.saturating_sub(now.saturating_duration_since(newest.at)))
    }

    /// Next [`Instant`] at which the displayed ETA text or state changes without new samples.
    #[must_use]
    pub(crate) fn next_change(&self, now: Instant) -> Option<Instant> {
        let newest = self.newest?;
        if self.total == 0 || newest.done >= self.total {
            return None;
        }
        if self.stall_at().is_some_and(|at| now >= at) {
            return None;
        }
        let word = match self.eta(now) {
            Eta::Remaining(r) => {
                let boundary = eta_word_change(r).and_then(|d| now.checked_add(d));
                let overdue = self.overdue_at();
                boundary.into_iter().chain(overdue).min()
            }
            Eta::Hidden | Eta::Stalled => None,
        };
        self.stall_onset(now).into_iter().chain(word).min()
    }

    /// [`Instant`] after `now` when the stream will transition to stalled if no new advance arrives.
    #[must_use]
    pub(crate) fn stall_onset(&self, now: Instant) -> Option<Instant> {
        let newest = self.newest?;
        if self.total == 0 || newest.done >= self.total {
            return None;
        }
        self.stall_at().filter(|at| *at > now)
    }

    /// Series identifier currently tracked.
    #[inline]
    #[must_use]
    pub(crate) const fn series(&self) -> Option<u64> {
        self.series
    }

    /// Number of samples currently buffered.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn samples_len(&self) -> usize {
        self.samples.len()
    }

    /// Number of projections currently buffered.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn projections_len(&self) -> usize {
        self.projections.len()
    }
}

/// Whole seconds of `d`, rounded up.
#[inline]
fn secs_up(d: Duration) -> u64 {
    ms(d).div_ceil(1000)
}

/// The words for `s` whole seconds left; `None` past a day. Under five
/// seconds the honest word is `<5 s left` (`5 s` beside 0.5 s left reads as a
/// promise of five more seconds).
fn eta_words_secs(s: u64, short: bool) -> Option<String> {
    let (n, long_unit, short_unit) = if s < 5 {
        return Some(if short { "<5s left" } else { "<5 s left" }.to_string());
    } else if s <= 55 {
        (5 * s.max(1).div_ceil(5), " s", "s")
    } else if s < 3570 {
        (((s + 30) / 60).max(1), " min", "m")
    } else if s <= 86_400 {
        ((s + 1800) / 3600, " h", "h")
    } else {
        return None;
    };
    Some(if short {
        format!("{n}{short_unit} left")
    } else {
        format!("{n}{long_unit} left")
    })
}

/// Format `remaining` as a quantized ETA label (`<5 s left`, `15 s left`, `3 min left`, etc.).
#[must_use]
pub fn eta_words(remaining: Duration) -> Option<String> {
    eta_words_secs(secs_up(remaining), false)
}

/// Format `remaining` in compact form (`<5s left`, `15s left`, `3m left`, etc.).
#[must_use]
pub(crate) fn eta_words_short(remaining: Duration) -> Option<String> {
    eta_words_secs(secs_up(remaining), true)
}

/// The largest whole-second count below `s` whose words differ from `s`'s,
/// or `None` when the words never change as the count falls.
fn eta_change_below(s: u64) -> Option<u64> {
    if s < 5 {
        None
    } else if s == 5 {
        Some(4)
    } else if s <= 55 {
        let bucket = s.max(1).div_ceil(5);
        (bucket >= 2).then(|| 5 * (bucket - 1))
    } else if s < 3570 {
        let m = (s + 30) / 60;
        Some((60 * m).saturating_sub(31).max(55))
    } else if s <= 86_400 {
        let h = (s + 1800) / 3600;
        Some((3600 * h).saturating_sub(1801).max(3569))
    } else {
        Some(86_400)
    }
}

/// Duration until [`eta_words`] changes as `remaining` counts down.
#[must_use]
pub(crate) fn eta_word_change(remaining: Duration) -> Option<Duration> {
    let r = ms(remaining);
    let s = r.div_ceil(1000);
    let below = eta_change_below(s)?;
    Some(Duration::from_millis(r - below * 1000))
}

/// Format elapsed busy time `e` once it reaches [`ELAPSED_AFTER`].
#[must_use]
pub(crate) fn elapsed_words(e: Duration, short: bool) -> Option<String> {
    (e >= ELAPSED_AFTER).then(|| ran_words(e, short))
}

/// [`elapsed_words`] with no floor.
fn ran_words(e: Duration, short: bool) -> String {
    let s = e.as_secs();
    let (n, long_unit, short_unit) = if s < 60 {
        (s, " s", "s")
    } else if s < 3600 {
        (s / 60, " min", "m")
    } else {
        ((s / 3600).min(999), " h", "h")
    };
    if short {
        format!("for {n}{short_unit}")
    } else {
        format!("for {n}{long_unit}")
    }
}

/// Duration until [`elapsed_words`] next changes as `e` grows, or `None` once capped at `999 h`.
#[must_use]
pub(crate) fn elapsed_word_change(e: Duration) -> Option<Duration> {
    if e < ELAPSED_AFTER {
        return Some(ELAPSED_AFTER.saturating_sub(e));
    }
    if e.as_secs() >= 999 * 3600 {
        return None;
    }
    let e_ms = ms(e);
    let step = if e_ms < 60_000 {
        1000
    } else if e_ms < 3_600_000 {
        60_000
    } else {
        3_600_000
    };
    Some(Duration::from_millis(step - e_ms % step))
}

/// Whether a person is actively waiting on an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Waiter {
    /// Initiated by and waited on by a person.
    Person,
    /// Background work not initiated by a person.
    Nobody,
}

impl Waiter {
    /// Whether work with this waiter and weight claims an animated progress row.
    #[inline]
    #[must_use]
    pub const fn takes_row(self, very_heavy: bool) -> bool {
        matches!(self, Self::Person) || very_heavy
    }

    /// Return [`Waiter::Person`] when `person` is `true`, else [`Waiter::Nobody`].
    #[inline]
    #[must_use]
    pub const fn of(person: bool) -> Self {
        if person { Self::Person } else { Self::Nobody }
    }
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ETA estimator and the time words (design §10.4.4). A live determinate
//! row whose reporter supplies an [`Amount`] feeds a [`ProgressTrack`]; the
//! band reads back an honest [`Eta`] — the secant across a window, shown only
//! once several projections agree, counting down between reads, re-anchored
//! only on a real change, "stalled" after ten seconds without bytes, and
//! blank once it has run out with nothing new ([`ETA_OVERDUE_MIN`]). All of
//! the math is integer (milliseconds, u64/u128): no floats, so native and wasm
//! agree to the millisecond, and the crate's clock fence holds — every method
//! takes `now`.

use std::collections::VecDeque;

use crate::model::{Amount, Unit};
use crate::{
    Duration, ELAPSED_AFTER, ETA_OVERDUE_MIN, Instant, PROJECTIONS_CAP, RATE_MIN_SPAN, RATE_WINDOW,
    SAMPLE_MIN_GAP, SAMPLES_CAP, STABLE_MIN, STABLE_SPAN, STALL_AFTER,
};

/// Past this a projection is not a projection (a stalled trickle, a
/// mis-sized total): it is discarded rather than voted.
const PROJECTION_MAX: Duration = Duration::from_hours(48);

/// Milliseconds of `d`, saturating.
fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `a − b` in milliseconds, 0 when `b` is later.
fn ms_between(a: Instant, b: Instant) -> u64 {
    ms(a.saturating_duration_since(b))
}

/// One observation kept in the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    at: Instant,
    done: u64,
}

/// What the band says about how long is left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eta {
    /// Nothing honest to say yet (or any more): the slot stays blank.
    Hidden,
    /// About this much is left.
    Remaining(Duration),
    /// Bytes stopped arriving ([`STALL_AFTER`] without an advance).
    Stalled,
}

/// The estimator behind one live row's ETA. Bounded: at most
/// [`SAMPLES_CAP`] samples and [`PROJECTIONS_CAP`] projections, whatever the
/// reporter's rate.
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
    projections: VecDeque<(Instant, Instant)>,
    anchor: Option<Instant>,
    /// How long past `anchor` the latch may stand with nothing new: the
    /// larger of [`ETA_OVERDUE_MIN`] and a fifth of the estimate it latched.
    overdue_grace: Duration,
}

impl ProgressTrack {
    /// Feed one reading. A different series or unit, or a `done` that went
    /// BACKWARDS, starts over; a different `total` keeps the samples but drops
    /// the projections and the latch (the deliverable changed size). A sample
    /// is kept at most every [`SAMPLE_MIN_GAP`], so a 10 Hz stream keeps its
    /// time base; the projection is voted only when a sample is kept.
    pub fn observe(&mut self, now: Instant, a: Amount) {
        let regressed = self.newest.is_some_and(|n| a.done < n.done);
        if self.series != Some(a.series) || self.unit != Some(a.unit) || regressed {
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
        if let Some(n) = self.newest
            && a.done > n.done
        {
            self.last_advance = Some(now);
        }
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

    /// What the band says at `now`: hidden with no total or once done;
    /// stalled once bytes stopped for [`STALL_AFTER`]; the latched completion
    /// counting down, and `<5 s left` past it only for the overdue grace (the
    /// estimate ran out with nothing new: steps and items have no stall to
    /// rescue them, review 2026-09-24); hidden otherwise.
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

    /// When the stream reads "stalled" if nothing arrives: bytes only, and
    /// only once they have moved at all.
    fn stall_at(&self) -> Option<Instant> {
        if self.unit != Some(Unit::Bytes) || !self.advanced {
            return None;
        }
        self.last_advance?.checked_add(STALL_AFTER)
    }

    /// The next instant the ETA's WORDS change with no new reading: the
    /// countdown's next boundary, the latch running out, or the stall.
    /// `None` when nothing will change on its own.
    #[must_use]
    pub fn next_change(&self, now: Instant) -> Option<Instant> {
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

    /// When a byte stream that has moved turns "stalled" if nothing arrives —
    /// the one change a bar with no ETA slot still DRAWS (its dim
    /// [`crate::Tone::STALLED`] ink); `None` for steps and items, before the
    /// first advance, and once the stall has begun.
    #[must_use]
    pub fn stall_onset(&self, now: Instant) -> Option<Instant> {
        let newest = self.newest?;
        if self.total == 0 || newest.done >= self.total {
            return None;
        }
        self.stall_at().filter(|at| *at > now)
    }

    /// Units per minute over the window ending at the newest reading, or
    /// `None` while the rate is not valid.
    #[must_use]
    pub fn rate_per_min(&self) -> Option<u64> {
        let newest = self.newest?;
        let (oldest, count) = self.window(newest.at)?;
        let span = ms_between(newest.at, oldest.at);
        if span < ms(RATE_MIN_SPAN) || count < 3 || newest.done <= oldest.done {
            return None;
        }
        let per_min = u128::from(newest.done - oldest.done) * 60_000 / u128::from(span);
        u64::try_from(per_min).ok()
    }

    /// `true` once a reading has been seen.
    #[must_use]
    pub fn is_fed(&self) -> bool {
        self.newest.is_some()
    }

    /// The series this track follows.
    #[must_use]
    pub fn series(&self) -> Option<u64> {
        self.series
    }

    /// Samples in the ring (a bound the tests read).
    #[must_use]
    pub fn samples_len(&self) -> usize {
        self.samples.len()
    }

    /// Projections in the ring (a bound the tests read).
    #[must_use]
    pub fn projections_len(&self) -> usize {
        self.projections.len()
    }
}

/// Whole seconds of `d`, rounded up.
fn secs_up(d: Duration) -> u64 {
    ms(d).div_ceil(1000)
}

/// The words for `s` whole seconds left; `None` past a day. Under five
/// seconds the honest word is `<5 s left` (`~5 s` beside 0.5 s left read as
/// a promise of five more seconds, review 2026-09-23). Every form ends in
/// `left`: a remaining time is never mistakable for the elapsed CLOCK
/// (`0:28`) an indeterminate row counts up with (review 2026-09-24).
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
        format!("~{n}{long_unit} left")
    })
}

/// The ETA in words: `<5 s left`, `~5 s left` … `~55 s left`, `~1 min left`
/// … `~59 min left`, `~1 h left` … `~24 h left`; `None` past a day. Never
/// says zero, and never reads as a clock.
#[must_use]
pub fn eta_words(remaining: Duration) -> Option<String> {
    eta_words_secs(secs_up(remaining), false)
}

/// [`eta_words`] in the slot's SHORT form ([`crate::ETA_SHORT_W`] cells):
/// `<5s left`, `5s left` … `55s left`, `1m left` … `59m left`, `1h left` …
/// `24h left` — the same numbers, so the words change at exactly the
/// instants [`eta_word_change`] names for the long form.
#[must_use]
pub fn eta_words_short(remaining: Duration) -> Option<String> {
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

/// How long until [`eta_words`] changes as `remaining` falls: the words hold
/// for exactly this long and differ at its end. `None` when they never change
/// (`<5 s left` stays `<5 s left` down to nothing).
#[must_use]
pub fn eta_word_change(remaining: Duration) -> Option<Duration> {
    let r = ms(remaining);
    let s = r.div_ceil(1000);
    let below = eta_change_below(s)?;
    Some(Duration::from_millis(r - below * 1000))
}

/// The ETA as a screen reader says it — the painted words said whole:
/// `less than 5 seconds left`, `about 40 seconds left`, `about 3 minutes
/// left`, `about 2 hours left`, `stalled`; `None` when hidden.
#[must_use]
pub fn eta_spoken(eta: Eta) -> Option<String> {
    match eta {
        Eta::Hidden => None,
        Eta::Stalled => Some("stalled".to_string()),
        Eta::Remaining(r) => {
            let s = secs_up(r);
            if s < 5 {
                // The paint's `<5 s left`, never "about 5 seconds".
                return Some("less than 5 seconds left".to_string());
            }
            let (n, unit) = if s <= 55 {
                (5 * s.max(1).div_ceil(5), "second")
            } else if s < 3570 {
                (((s + 30) / 60).max(1), "minute")
            } else if s <= 86_400 {
                ((s + 1800) / 3600, "hour")
            } else {
                return None;
            };
            let plural = if n == 1 { "" } else { "s" };
            Some(format!("about {n} {unit}{plural} left"))
        }
    }
}

/// How long indeterminate work has run, as a CLOCK: nothing under
/// [`ELAPSED_AFTER`], then `0:12` … `59:59`, then `1:02 h` … `9:59 h`, then
/// `10 h` — floored. A clock counting up never reads like the `~25 s left`
/// counting down (review 2026-09-23: `28 s` and `~25 s` differed by a
/// tilde; review 2026-09-24: the remaining time now says `left` as well).
#[must_use]
pub fn elapsed_words(e: Duration) -> Option<String> {
    (e >= ELAPSED_AFTER).then(|| clock_words(e))
}

/// `e` as the elapsed CLOCK with no floor: `0:00` … `59:59`, `1:02 h` …
/// `9:59 h`, then `10 h` — floored, at most [`crate::ELAPSED_W`] cells. It
/// is what an indeterminate row's elapsed slot says from [`ELAPSED_AFTER`]
/// on, and what a determinate row's ETA slot says while its estimate is
/// hidden, so that slot always answers "how long" (review round 3,
/// 2026-09-24: `35%` then ten blank cells until the ETA latched).
#[must_use]
pub fn clock_words(e: Duration) -> String {
    let s = e.as_secs();
    if s < 3600 {
        format!("{}:{:02}", s / 60, s % 60)
    } else if s < 36_000 {
        format!("{}:{:02} h", s / 3600, s % 3600 / 60)
    } else {
        format!("{} h", s / 3600)
    }
}

/// How long until [`elapsed_words`] changes as `e` grows: the first words at
/// [`ELAPSED_AFTER`], then as [`clock_word_change`].
#[must_use]
pub fn elapsed_word_change(e: Duration) -> Duration {
    if e < ELAPSED_AFTER {
        return ELAPSED_AFTER.saturating_sub(e);
    }
    clock_word_change(e)
}

/// How long until [`clock_words`] changes as `e` grows: the next whole
/// second (under an hour), minute (under ten) or hour.
#[must_use]
pub fn clock_word_change(e: Duration) -> Duration {
    let e_ms = ms(e);
    let step = if e_ms < 3_600_000 {
        1000
    } else if e_ms < 36_000_000 {
        60_000
    } else {
        3_600_000
    };
    Duration::from_millis(step - e_ms % step)
}

/// The elapsed time as a screen reader says it: `running for 12 seconds`,
/// `running for 3 minutes`; `None` under [`ELAPSED_AFTER`].
#[must_use]
pub fn elapsed_spoken(e: Duration) -> Option<String> {
    if e < ELAPSED_AFTER {
        return None;
    }
    clock_spoken(e)
}

/// [`clock_words`] as a screen reader says it: `running for 4 seconds` —
/// `None` in the first second, where the clock has not yet moved.
#[must_use]
pub fn clock_spoken(e: Duration) -> Option<String> {
    let s = e.as_secs();
    if s == 0 {
        return None;
    }
    let (n, unit) = if s < 60 {
        (s, "second")
    } else if s < 3600 {
        (s / 60, "minute")
    } else {
        (s / 3600, "hour")
    };
    let plural = if n == 1 { "" } else { "s" };
    Some(format!("running for {n} {unit}{plural}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn at(base: Instant, ms_: u64) -> Instant {
        base + Duration::from_millis(ms_)
    }

    fn bytes(done: u64, total: u64) -> Amount {
        Amount {
            series: 7,
            done,
            total,
            unit: Unit::Bytes,
        }
    }

    /// 10 MB/s observed at 10 Hz: the rate is the secant across the window —
    /// 600 MB a minute — and there is none before three seconds of samples.
    #[test]
    fn rate_is_the_window_secant_and_needs_three_seconds() {
        let base = t0();
        let mut t = ProgressTrack::default();
        for k in 0..=100u64 {
            t.observe(at(base, k * 100), bytes(k * 1_000_000, 10_000_000_000));
            if k * 100 < 3000 {
                assert_eq!(t.rate_per_min(), None, "{k}: under three seconds");
            }
        }
        assert_eq!(t.rate_per_min(), Some(600_000_000));
    }

    /// A steady rate is latched at 5 s — the first valid rate is at 3 s,
    /// and three projections spanning STABLE_MIN agree by 5 s. A stream that
    /// arrives in two-second BURSTS (its rate swinging between nothing and
    /// twice its mean) does not latch while its projections disagree past the
    /// tolerance: nothing shows until the window has averaged the bursts, and
    /// what then shows is within 10 % of the truth. (A swing faster than the
    /// 20 s window is averaged by the secant — which is the point of the
    /// secant — so "never" holds only while the window still sees the swing.)
    #[test]
    fn eta_latches_only_when_three_projections_agree_over_two_seconds() {
        let base = t0();
        let total = 1_000_000_000;
        let mut t = ProgressTrack::default();
        let mut latched_at = None;
        for k in 0..=80u64 {
            let now = at(base, k * 100);
            t.observe(now, bytes(k * 1_000_000, total));
            if latched_at.is_none() && matches!(t.eta(now), Eta::Remaining(_)) {
                latched_at = Some(k * 100);
            }
        }
        assert_eq!(latched_at, Some(5000), "a steady rate latches at 5 s");
        // The bursts: 1 MB lands every two seconds, nothing in between.
        let chunk = 1_000_000;
        let mut burst = ProgressTrack::default();
        let mut shown = None;
        for k in 0..=300u64 {
            let now = at(base, k * 100);
            burst.observe(now, bytes(chunk * (1 + k / 20), 100 * chunk));
            if let Eta::Remaining(r) = burst.eta(now) {
                shown.get_or_insert((k * 100, now + r));
            }
        }
        let (first, anchor) = shown.expect("the averaged stream latches");
        assert!(
            first >= 10_000,
            "no ETA while the bursts still swing the projections: {first} ms"
        );
        // The truth: 99 more chunks, one every two seconds, from t = 0.
        let truth = at(base, 99 * 2000);
        let err = if anchor > truth {
            anchor - truth
        } else {
            truth - anchor
        };
        assert!(
            err <= Duration::from_millis(99 * 2000 / 10),
            "latched {err:?} off the truth"
        );
    }

    /// Between reads a latched ETA COUNTS DOWN, one for one; it never jumps
    /// back up unless it re-anchors; and a real change of rate moves the
    /// anchor only in the change's direction — never flapping — until it
    /// settles on the new pace once the window holds only it.
    #[test]
    fn eta_counts_down_between_reads_and_reanchors_once_on_a_real_change() {
        let base = t0();
        let total = 400_000_000;
        let mut t = ProgressTrack::default();
        let mut done = 0;
        for k in 0..=60u64 {
            done = (k + 1) * 1_000_000;
            t.observe(at(base, k * 100), bytes(done, total));
        }
        let now = at(base, 6000);
        let Eta::Remaining(r0) = t.eta(now) else {
            panic!("latched: {:?}", t.eta(now));
        };
        // 339 MB left at 10 MB/s: about 34 s.
        assert!(
            (33_000..=35_000).contains(&ms(r0)),
            "{r0:?} left at a steady 10 MB/s"
        );
        let Eta::Remaining(r1) = t.eta(at(base, 7500)) else {
            panic!()
        };
        assert_eq!(r0 - r1, Duration::from_millis(1500), "a countdown, 1:1");
        assert_eq!(t.next_change(now), Some(now + eta_word_change(r0).unwrap()));
        // The rate halves: the anchor moves LATER each time it moves, never
        // back, and lands on the new pace.
        let mut anchors: Vec<Instant> = Vec::new();
        for k in 61..=400u64 {
            done += 500_000;
            let now = at(base, k * 100);
            t.observe(now, bytes(done, total));
            if let Eta::Remaining(r) = t.eta(now) {
                let anchor = now + r;
                if anchors.last() != Some(&anchor) {
                    anchors.push(anchor);
                }
            }
        }
        assert!(anchors.len() >= 2, "the change moved the anchor");
        for pair in anchors.windows(2) {
            assert!(
                pair[1] > pair[0] + Duration::from_secs(3),
                "a re-anchor is a real step, in the change's direction: {pair:?}"
            );
        }
        // 339 MB at 5 MB/s from 6.1 s: done at about 73.9 s.
        let truth = at(base, 6100 + 67_800);
        let last = *anchors.last().unwrap();
        let err = if last > truth {
            last - truth
        } else {
            truth - last
        };
        assert!(
            err <= Duration::from_millis(67_800 / 10),
            "settled {err:?} off the new pace (within a tenth of the new remainder)"
        );
    }

    /// The words: quantized, never zero, and the table the design pins.
    #[test]
    fn eta_words_quantize_and_never_say_zero() {
        let w = |s: u64| eta_words(Duration::from_secs(s));
        assert_eq!(eta_words(Duration::ZERO).as_deref(), Some("<5 s left"));
        for (s, want) in [
            (1, "<5 s left"),
            (4, "<5 s left"),
            (5, "~5 s left"),
            (6, "~10 s left"),
            (35, "~35 s left"),
            (55, "~55 s left"),
            (56, "~1 min left"),
            (89, "~1 min left"),
            (90, "~2 min left"),
            (170, "~3 min left"),
            (3569, "~59 min left"),
            (3570, "~1 h left"),
            (86_400, "~24 h left"),
        ] {
            assert_eq!(w(s).as_deref(), Some(want), "{s} s");
        }
        assert_eq!(w(86_401), None, "past a day");
        assert_eq!(
            eta_words(Duration::from_millis(5_001)).as_deref(),
            Some("~10 s left"),
            "whole seconds rounded up"
        );
        for s in 0..90_000u64 {
            let words = w(s);
            assert!(
                words.as_deref().is_none_or(|x| !x.starts_with("~0")),
                "{s}: {words:?}"
            );
            if let Some(x) = words {
                assert!(x.chars().count() <= crate::ETA_W, "{x:?}");
                // A remaining time never reads as the elapsed clock.
                assert!(x.ends_with(" left") && !x.contains(':'), "{x:?}");
            }
            let short = eta_words_short(Duration::from_secs(s));
            assert_eq!(short.is_some(), w(s).is_some(), "{s}");
            if let Some(x) = short {
                assert!(x.chars().count() <= crate::ETA_SHORT_W, "{x:?}");
                assert!(x.ends_with(" left") && !x.contains(':'), "{x:?}");
            }
        }
        for (s, want) in [
            (0, "<5s left"),
            (4, "<5s left"),
            (35, "35s left"),
            (170, "3m left"),
            (3569, "59m left"),
            (86_400, "24h left"),
        ] {
            assert_eq!(
                eta_words_short(Duration::from_secs(s)).as_deref(),
                Some(want),
                "{s} s"
            );
        }
        for e in [10u64, 28, 59, 60, 599, 3599, 3600, 36_000] {
            let clock = elapsed_words(Duration::from_secs(e)).unwrap();
            assert!(
                !clock.contains("left") && !clock.starts_with('~'),
                "the elapsed clock is not an ETA: {clock:?}"
            );
        }
        assert_eq!(
            eta_spoken(Eta::Remaining(Duration::from_millis(2_500))).as_deref(),
            Some("less than 5 seconds left"),
            "the paint's `<5 s left`, said whole"
        );
        assert_eq!(
            eta_spoken(Eta::Remaining(Duration::from_secs(40))).as_deref(),
            Some("about 40 seconds left")
        );
        assert_eq!(
            eta_spoken(Eta::Remaining(Duration::from_secs(170))).as_deref(),
            Some("about 3 minutes left")
        );
        assert_eq!(
            eta_spoken(Eta::Remaining(Duration::from_secs(60))).as_deref(),
            Some("about 1 minute left")
        );
        assert_eq!(eta_spoken(Eta::Stalled).as_deref(), Some("stalled"));
        assert_eq!(eta_spoken(Eta::Hidden), None);
    }

    /// Every 10 ms over two hours, the words hold until the change and
    /// differ at it.
    #[test]
    fn eta_word_change_is_exact() {
        for r_ms in (0..=7_200_000u64).step_by(10) {
            let r = Duration::from_millis(r_ms);
            let now = eta_words(r);
            match eta_word_change(r) {
                None => {
                    assert_eq!(now.as_deref(), Some("<5 s left"), "{r_ms}");
                    assert_eq!(eta_words(Duration::ZERO), now);
                }
                Some(d) => {
                    assert!(d > Duration::ZERO && d <= r, "{r_ms}: {d:?}");
                    assert_ne!(eta_words(r - d), now, "{r_ms}: differs at the change");
                    let just_before = r - d + Duration::from_millis(1);
                    assert_eq!(eta_words(just_before), now, "{r_ms}: holds until it");
                    // The short form changes at the very same instants.
                    let short = eta_words_short(r);
                    assert_ne!(eta_words_short(r - d), short, "{r_ms}: short too");
                    assert_eq!(eta_words_short(just_before), short, "{r_ms}: short holds");
                }
            }
        }
    }

    /// Bytes that stop for STALL_AFTER read "stalled" — only once they have
    /// moved at all, and never for a count of items.
    #[test]
    fn stalled_after_ten_seconds_of_no_bytes_and_never_for_items() {
        let base = t0();
        let mut t = ProgressTrack::default();
        t.observe(base, bytes(0, 1000));
        assert_eq!(
            t.eta(at(base, 60_000)),
            Eta::Hidden,
            "nothing moved yet: not a stall"
        );
        t.observe(at(base, 1000), bytes(10, 1000));
        assert_eq!(t.next_change(at(base, 1000)), Some(at(base, 11_000)));
        assert_eq!(t.eta(at(base, 10_999)), Eta::Hidden);
        assert_eq!(t.eta(at(base, 11_000)), Eta::Stalled);
        t.observe(at(base, 12_000), bytes(20, 1000));
        assert_eq!(t.eta(at(base, 12_000)), Eta::Hidden, "bytes again");
        let mut items = ProgressTrack::default();
        for k in 0..5u64 {
            items.observe(
                at(base, k * 1000),
                Amount {
                    unit: Unit::Items,
                    ..bytes(k, 100)
                },
            );
        }
        assert_eq!(
            items.eta(at(base, 3_600_000)),
            Eta::Hidden,
            "items may sit still"
        );
        // Done is done.
        let mut done = ProgressTrack::default();
        done.observe(base, bytes(1000, 1000));
        assert_eq!(done.eta(at(base, 60_000)), Eta::Hidden);
        assert_eq!(done.next_change(base), None);
    }

    /// A new series, a new unit or a regression starts over; a new total
    /// keeps the samples and drops the latch.
    #[test]
    fn a_series_change_or_a_regression_resets_the_estimator() {
        let base = t0();
        let feed = |t: &mut ProgressTrack, series: u64| {
            for k in 0..=60u64 {
                t.observe(
                    at(base, k * 100),
                    Amount {
                        series,
                        ..bytes(k * 1_000_000, 1_000_000_000)
                    },
                );
            }
        };
        let mut t = ProgressTrack::default();
        feed(&mut t, 1);
        assert!(matches!(t.eta(at(base, 6000)), Eta::Remaining(_)));
        assert!(t.samples_len() > 3);
        t.observe(
            at(base, 6100),
            Amount {
                series: 2,
                ..bytes(61_000_000, 1_000_000_000)
            },
        );
        assert_eq!(t.samples_len(), 1, "a new series starts over");
        assert_eq!(t.eta(at(base, 6100)), Eta::Hidden);
        let mut t = ProgressTrack::default();
        feed(&mut t, 1);
        t.observe(at(base, 6100), bytes(1_000, 1_000_000_000));
        assert_eq!(t.samples_len(), 1, "a regression starts over");
        let mut t = ProgressTrack::default();
        feed(&mut t, 1);
        let kept = t.samples_len();
        t.observe(
            at(base, 6100),
            Amount {
                series: 1,
                ..bytes(61_000_000, 2_000_000_000)
            },
        );
        assert_eq!(t.samples_len(), kept, "same sample slot: nothing dropped");
        assert_eq!(t.projections_len(), 0, "a new total drops the votes");
        assert_eq!(t.eta(at(base, 6100)), Eta::Hidden, "…and the latch");
    }

    /// Ten thousand readings at 10 Hz keep both rings inside their caps.
    #[test]
    fn the_sample_ring_and_projections_are_bounded() {
        let base = t0();
        let mut t = ProgressTrack::default();
        for k in 0..10_000u64 {
            t.observe(at(base, k * 100), bytes(k * 1000, u64::MAX / 2));
            assert!(t.samples_len() <= SAMPLES_CAP);
            assert!(t.projections_len() <= PROJECTIONS_CAP);
        }
        let mut burst = ProgressTrack::default();
        for k in 0..10_000u64 {
            burst.observe(at(base, k), bytes(k, 20_000));
        }
        assert!(burst.samples_len() <= SAMPLES_CAP);
    }

    /// Elapsed words: nothing under ten seconds, then a clock to the second
    /// for an hour, to the minute for nine more, then hours — changing
    /// exactly at each boundary.
    #[test]
    fn elapsed_words_follow_the_presence_cadence() {
        let w = |s: u64| elapsed_words(Duration::from_secs(s));
        assert_eq!(w(9), None);
        assert_eq!(w(10).as_deref(), Some("0:10"));
        assert_eq!(w(28).as_deref(), Some("0:28"));
        assert_eq!(w(59).as_deref(), Some("0:59"));
        assert_eq!(w(60).as_deref(), Some("1:00"));
        assert_eq!(w(754).as_deref(), Some("12:34"));
        assert_eq!(w(3599).as_deref(), Some("59:59"));
        assert_eq!(w(3600).as_deref(), Some("1:00 h"));
        assert_eq!(w(3720).as_deref(), Some("1:02 h"));
        assert_eq!(w(35_999).as_deref(), Some("9:59 h"));
        assert_eq!(w(36_000).as_deref(), Some("10 h"));
        for e_ms in (0..=7_300_000u64)
            .step_by(250)
            .chain((35_000_000..=37_000_000u64).step_by(5_000))
        {
            let e = Duration::from_millis(e_ms);
            let d = elapsed_word_change(e);
            assert!(d > Duration::ZERO, "{e_ms}");
            assert_ne!(elapsed_words(e + d), elapsed_words(e), "{e_ms}: changes at");
            assert_eq!(
                elapsed_words(e + d - Duration::from_millis(1)),
                elapsed_words(e),
                "{e_ms}: holds until"
            );
            if let Some(words) = elapsed_words(e) {
                assert!(words.chars().count() <= crate::ELAPSED_W, "{words}");
            }
        }
        assert_eq!(
            elapsed_spoken(Duration::from_secs(180)).as_deref(),
            Some("running for 3 minutes")
        );
        assert_eq!(elapsed_spoken(Duration::from_secs(3)), None);
        // The floorless clock a hidden estimate leaves in the ETA slot: from
        // `0:00`, changing exactly where its change says, never wider than
        // the SHORT ETA slot, and never a remaining time's `left`.
        assert_eq!(clock_words(Duration::ZERO), "0:00");
        assert_eq!(clock_words(Duration::from_millis(12_400)), "0:12");
        for e_ms in (0..=7_300_000u64).step_by(250) {
            let e = Duration::from_millis(e_ms);
            let d = clock_word_change(e);
            assert_ne!(clock_words(e + d), clock_words(e), "{e_ms}: changes at");
            assert_eq!(
                clock_words(e + d - Duration::from_millis(1)),
                clock_words(e),
                "{e_ms}: holds until"
            );
            let words = clock_words(e);
            assert!(words.chars().count() <= crate::ETA_SHORT_W, "{words}");
            assert!(!words.ends_with("left"), "{words}");
        }
        assert_eq!(clock_spoken(Duration::from_millis(900)), None);
        assert_eq!(
            clock_spoken(Duration::from_secs(4)).as_deref(),
            Some("running for 4 seconds")
        );
    }
}

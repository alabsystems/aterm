// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ETA estimator and the time words (design §10.4.4). A live determinate
//! row whose reporter supplies an [`Amount`] feeds a [`ProgressTrack`]; the
//! band reads back an honest [`Eta`] — the secant across a window, shown only
//! once several projections agree, counting down between reads, re-anchored
//! only on a real change, "stalled" ten seconds after bytes stop (a count
//! after several of its own gaps between advances), and
//! blank once it has run out with nothing new ([`ETA_OVERDUE_MIN`]). All of
//! the math is integer (milliseconds, u64/u128): no floats, so native and wasm
//! agree to the millisecond, and the crate's clock fence holds — every method
//! takes `now`.

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
    /// When the series began (its first reading), and how many readings
    /// since moved `done` forward: a count's own pace ([`COUNT_STALL_GAPS`]).
    began: Option<Instant>,
    advances: u32,
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
        // A held phase ended (ruling 266): the count's patience restarts
        // here, so the plateau it sat through is not read as a stall.
        if self.unit == Some(Unit::HeldSteps) && a.unit != Unit::HeldSteps {
            self.last_advance = Some(now);
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

    /// When the stream reads "stalled" if nothing arrives, once it has moved
    /// at all: bytes [`STALL_AFTER`] after the last advance; a COUNT (steps,
    /// items) after [`COUNT_STALL_GAPS`] of its own mean gaps between
    /// advances, never sooner than bytes (design ruling 265) — a count that
    /// stopped used to count its estimate down to `<5 s left` at a frozen
    /// fill and then go blank, never saying it had stalled.
    fn stall_at(&self) -> Option<Instant> {
        if !self.advanced {
            return None;
        }
        let last = self.last_advance?;
        let after = match self.unit? {
            // A phase that holds the fill by plan never stalls (ruling 266).
            Unit::HeldSteps => return None,
            Unit::Bytes => STALL_AFTER,
            Unit::Items | Unit::Steps => {
                let pace = last.saturating_duration_since(self.began?) / self.advances.max(1);
                STALL_AFTER.max(pace.saturating_mul(COUNT_STALL_GAPS))
            }
        };
        last.checked_add(after)
    }

    /// A QUICK projection of the time left, for the reveal (design ruling
    /// 265): the secant from the oldest kept reading to the newest, less the
    /// time since the newest — `None` until `done` has moved. Coarser than
    /// the ETA (no votes, no latch); it only decides whether a row about to
    /// be revealed would be on the glass long enough to read.
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

    /// The next instant the ETA's WORDS change with no new reading: the
    /// countdown's next boundary, the latch running out, or the stall.
    /// `None` when nothing will change on its own.
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

    /// When a stream that has moved turns "stalled" if nothing arrives —
    /// the one change a bar with no ETA slot still DRAWS (its dim
    /// [`crate::Tone::STALLED`] ink); `None` before the first advance and
    /// once the stall has begun.
    #[must_use]
    pub(crate) fn stall_onset(&self, now: Instant) -> Option<Instant> {
        let newest = self.newest?;
        if self.total == 0 || newest.done >= self.total {
            return None;
        }
        self.stall_at().filter(|at| *at > now)
    }

    /// Units per minute over the window ending at the newest reading, or
    /// `None` while the rate is not valid.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn rate_per_min(&self) -> Option<u64> {
        let newest = self.newest?;
        let (oldest, count) = self.window(newest.at)?;
        let span = ms_between(newest.at, oldest.at);
        if span < ms(RATE_MIN_SPAN) || count < 3 || newest.done <= oldest.done {
            return None;
        }
        let per_min = u128::from(newest.done - oldest.done) * 60_000 / u128::from(span);
        u64::try_from(per_min).ok()
    }

    /// The series this track follows.
    #[must_use]
    pub(crate) fn series(&self) -> Option<u64> {
        self.series
    }

    /// Samples in the ring (a bound the tests read).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn samples_len(&self) -> usize {
        self.samples.len()
    }

    /// Projections in the ring (a bound the tests read).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn projections_len(&self) -> usize {
        self.projections.len()
    }
}

/// Whole seconds of `d`, rounded up.
fn secs_up(d: Duration) -> u64 {
    ms(d).div_ceil(1000)
}

/// The words for `s` whole seconds left; `None` past a day. Under five
/// seconds the honest word is `<5 s left` (`5 s` beside 0.5 s left read as a
/// promise of five more seconds, review 2026-09-23). Every form ends in
/// `left`, which already says it is an estimate: the `~` the long form wore
/// until round 12 said it twice and made the one number the owner reads for
/// ("how long to wait") the widest thing in its slot (design ruling 241).
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

/// The ETA in words: `<5 s left`, `5 s left` … `55 s left`, `1 min left` …
/// `59 min left`, `1 h left` … `24 h left`; `None` past a day. Never says
/// zero, and never reads as a clock.
#[must_use]
pub fn eta_words(remaining: Duration) -> Option<String> {
    eta_words_secs(secs_up(remaining), false)
}

/// [`eta_words`] in the slot's SHORT form ([`crate::ETA_SHORT_W`] cells):
/// `<5s left`, `5s left` … `55s left`, `1m left` … `59m left`, `1h left` …
/// `24h left` — the same numbers, so the words change at exactly the
/// instants [`eta_word_change`] names for the long form.
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

/// How long until [`eta_words`] changes as `remaining` falls: the words hold
/// for exactly this long and differ at its end. `None` when they never change
/// (`<5 s left` stays `<5 s left` down to nothing).
#[must_use]
pub(crate) fn eta_word_change(remaining: Duration) -> Option<Duration> {
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

/// How long busy work has run, LABELLED (design ruling 241): nothing under
/// [`ELAPSED_AFTER`], then `for 12 s` … `for 59 s`, `for 1 min` … `for 59
/// min`, `for 1 h` … `for 999 h` — floored. Never a bare clock: `0:41` next
/// to a time slot that later says `30 s left` read as seven seconds LEFT
/// where it meant seven seconds gone (round-12 critics). `short` is the
/// slot's sacrifice form ([`crate::ELAPSED_SHORT_W`] cells): `for 12s`,
/// `for 3m`, `for 2h` — the same numbers, changing at the same instants.
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

/// How long until [`elapsed_words`] changes as `e` grows: the first words at
/// [`ELAPSED_AFTER`], then the next whole second (under a minute), minute
/// (under an hour) or hour.
#[must_use]
pub(crate) fn elapsed_word_change(e: Duration) -> Duration {
    if e < ELAPSED_AFTER {
        return ELAPSED_AFTER.saturating_sub(e);
    }
    let e_ms = ms(e);
    let step = if e_ms < 60_000 {
        1000
    } else if e_ms < 3_600_000 {
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

/// How long work has run as a screen reader says it, with no floor:
/// `running for 4 seconds` — `None` in the first second.
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

/// WHO WAITS on a piece of work — the one question that decides whether it
/// takes an animated row (design rulings 220 and 224). A host answers it
/// from what started the work (a press, a menu item, a typed verb, or a
/// background loop) and passes the answer; the rule itself is here, once,
/// for every lane and every platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Waiter {
    /// A person started it and is waiting for what it delivers.
    Person,
    /// Nobody: work the person did not ask for (a background check, a
    /// scheduled pass). R3: "seamless, non-interrupting (ideally silent)".
    Nobody,
}

impl Waiter {
    /// Whether the work takes its animated row (after the progress grace,
    /// [`crate::PROGRESS_GRACE`], so work that ends inside it never touches
    /// the glass): a person's always — they asked, and the row says how long
    /// and for what; nobody's only while it is very heavy system use that
    /// needs explaining (design §10.6), and otherwise it is silent and its end
    /// a record.
    #[must_use]
    pub const fn takes_row(self, very_heavy: bool) -> bool {
        matches!(self, Self::Person) || very_heavy
    }

    /// [`Waiter::Person`] when `person` is true.
    #[must_use]
    pub const fn of(person: bool) -> Self {
        if person { Self::Person } else { Self::Nobody }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WHO WAITS DECIDES THE ROW (rulings 220, 224): a person's work always
    /// takes its row; nobody's only while it is very heavy.
    #[test]
    fn who_waits_decides_the_row() {
        assert!(Waiter::Person.takes_row(false));
        assert!(Waiter::Person.takes_row(true));
        assert!(
            !Waiter::Nobody.takes_row(false),
            "silent, a record at its end"
        );
        assert!(
            Waiter::Nobody.takes_row(true),
            "very heavy use is explained"
        );
        assert_eq!(Waiter::of(true), Waiter::Person);
        assert_eq!(Waiter::of(false), Waiter::Nobody);
    }

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
            (5, "5 s left"),
            (6, "10 s left"),
            (35, "35 s left"),
            (55, "55 s left"),
            (56, "1 min left"),
            (89, "1 min left"),
            (90, "2 min left"),
            (170, "3 min left"),
            (3569, "59 min left"),
            (3570, "1 h left"),
            (86_400, "24 h left"),
        ] {
            assert_eq!(w(s).as_deref(), Some(want), "{s} s");
        }
        assert_eq!(w(86_401), None, "past a day");
        assert_eq!(
            eta_words(Duration::from_millis(5_001)).as_deref(),
            Some("10 s left"),
            "whole seconds rounded up"
        );
        for s in 0..90_000u64 {
            let words = w(s);
            assert!(
                words
                    .as_deref()
                    .is_none_or(|x| !x.starts_with('0') && !x.contains('~')),
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
            for short in [false, true] {
                let ran = elapsed_words(Duration::from_secs(e), short).unwrap();
                assert!(
                    !ran.contains("left") && ran.starts_with("for "),
                    "the elapsed time is not an ETA: {ran:?}"
                );
            }
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
    fn stalled_after_ten_seconds_of_no_bytes_and_a_counts_own_patience() {
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
        // A COUNT may sit still for its own patience (ruling 265): four of
        // its mean gaps (here 1 s each), never under the bytes' ten
        // seconds — and then it says so.
        assert_eq!(
            items.eta(at(base, 4_000 + 9_999)),
            Eta::Hidden,
            "items may sit still"
        );
        assert_eq!(items.eta(at(base, 4_000 + 10_000)), Eta::Stalled);
        assert_eq!(
            items.stall_onset(at(base, 4_000)),
            Some(at(base, 14_000)),
            "the onset is a deadline"
        );
        // A slow count's patience is its own pace: one item every 5 s
        // stalls 20 s after the last.
        let mut slow = ProgressTrack::default();
        for k in 0..5u64 {
            slow.observe(
                at(base, k * 5000),
                Amount {
                    unit: Unit::Steps,
                    ..bytes(k, 100)
                },
            );
        }
        assert_eq!(slow.eta(at(base, 20_000 + 19_999)), Eta::Hidden);
        assert_eq!(slow.eta(at(base, 20_000 + 20_000)), Eta::Stalled);
        // Done is done.
        let mut done = ProgressTrack::default();
        done.observe(base, bytes(1000, 1000));
        assert_eq!(done.eta(at(base, 60_000)), Eta::Hidden);
        assert_eq!(done.next_change(base), None);
    }

    /// A PHASE THAT HOLDS THE FILL IS NOT A STALL (ruling 266): the ALab
    /// toolchain's fill advanced at 10 Hz through a download, then holds at
    /// its program's 500‰ share through a minute of verify. Declared
    /// [`Unit::HeldSteps`] it never reads stalled and arms no deadline; the
    /// estimator keeps its readings across the switch; and when the phase
    /// ends the count's patience starts over — a count that then really
    /// stops still says so.
    #[test]
    fn a_held_phase_never_reads_stalled_and_its_end_restarts_the_patience() {
        let base = t0();
        let steps = |done: u64, unit: Unit| Amount {
            unit,
            ..bytes(done, 1000)
        };
        let mut t = ProgressTrack::default();
        for k in 0..=50u64 {
            t.observe(at(base, k * 100), steps(k * 10, Unit::Steps));
        }
        let samples = t.samples.len();
        // Verify: the fill holds at 500‰ for a minute, atpkg's heartbeat
        // re-feeding it every 2 s.
        for k in 0..=30u64 {
            let now = at(base, 5_000 + k * 2_000);
            t.observe(now, steps(500, Unit::HeldSteps));
            assert_ne!(t.eta(now), Eta::Stalled, "held at {k}");
            assert_eq!(t.stall_onset(now), None, "no deadline while held");
        }
        assert!(t.samples.len() > samples, "the same series: readings kept");
        // Extract begins at the same share: not stalled on its first frame…
        let resumed = at(base, 66_000);
        t.observe(resumed, steps(500, Unit::Steps));
        assert_ne!(t.eta(resumed), Eta::Stalled);
        assert!(
            t.stall_onset(resumed)
                .is_some_and(|onset| onset >= at(base, 66_000 + 10_000)),
            "the patience starts at the phase's end"
        );
        // …and a count that then really stops still says so.
        assert_eq!(t.eta(at(base, 66_000 + 60_000)), Eta::Stalled);
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

    /// Elapsed words (design ruling 241): nothing under ten seconds, then
    /// `for N s`, `for N min`, `for N h` — never a bare clock — changing
    /// exactly at each boundary, the short form at the same instants.
    #[test]
    fn elapsed_words_are_labelled_and_follow_their_cadence() {
        let w = |s: u64| elapsed_words(Duration::from_secs(s), false);
        let short = |s: u64| elapsed_words(Duration::from_secs(s), true);
        assert_eq!(w(9), None);
        assert_eq!(short(9), None);
        for (s, long, sh) in [
            (10, "for 10 s", "for 10s"),
            (41, "for 41 s", "for 41s"),
            (59, "for 59 s", "for 59s"),
            (60, "for 1 min", "for 1m"),
            (199, "for 3 min", "for 3m"),
            (3599, "for 59 min", "for 59m"),
            (3600, "for 1 h", "for 1h"),
            (35_999, "for 9 h", "for 9h"),
            (36_000, "for 10 h", "for 10h"),
            (99_999_999, "for 999 h", "for 999h"),
        ] {
            assert_eq!(w(s).as_deref(), Some(long), "{s}");
            assert_eq!(short(s).as_deref(), Some(sh), "{s}");
        }
        let clock = |x: &str| {
            let mut parts = x.split(':');
            matches!((parts.next(), parts.next(), parts.next()),
                (Some(a), Some(b), None) if !a.is_empty() && a.chars().all(|c| c.is_ascii_digit())
                    && b.len() == 2 && b.chars().all(|c| c.is_ascii_digit()))
        };
        for e_ms in (0..=7_300_000u64)
            .step_by(250)
            .chain((35_000_000..=37_000_000u64).step_by(5_000))
        {
            let e = Duration::from_millis(e_ms);
            let d = elapsed_word_change(e);
            assert!(d > Duration::ZERO, "{e_ms}");
            for sh in [false, true] {
                assert_ne!(
                    elapsed_words(e + d, sh),
                    elapsed_words(e, sh),
                    "{e_ms}: changes at"
                );
                assert_eq!(
                    elapsed_words(e + d - Duration::from_millis(1), sh),
                    elapsed_words(e, sh),
                    "{e_ms}: holds until"
                );
            }
            if let Some(words) = elapsed_words(e, false) {
                assert!(words.chars().count() <= crate::ELAPSED_W, "{words}");
                assert!(!clock(&words), "{words}");
            }
            if let Some(words) = elapsed_words(e, true) {
                assert!(words.chars().count() <= crate::ELAPSED_SHORT_W, "{words}");
                assert!(!clock(&words), "{words}");
            }
        }
        assert_eq!(
            elapsed_spoken(Duration::from_secs(180)).as_deref(),
            Some("running for 3 minutes")
        );
        assert_eq!(elapsed_spoken(Duration::from_secs(3)), None);
        assert_eq!(clock_spoken(Duration::from_millis(900)), None);
        assert_eq!(
            clock_spoken(Duration::from_secs(4)).as_deref(),
            Some("running for 4 seconds")
        );
    }
}

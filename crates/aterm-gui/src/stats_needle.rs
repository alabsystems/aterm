// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stats deck's dial in motion (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §4): the
//! needle sweeps to each reading on a critically damped spring in angle space, and the bezel's
//! soak eases on a second channel of the same spring, so the dial moves like a needle rather
//! than jumping at each reading or stepping on the live edge's 250 ms grid.
//!
//! - **The law.** With the target `T`, the angle `x0` and the angular velocity `v0` at the
//!   retarget `t0`, and `u = t − t0`: `x(u) = T + (x0 − T + (v0 + ω(x0 − T))·u)·e^(−ωu)` at
//!   [`OMEGA`] (12 rad/s). From rest a step is 80 % of the way after 250 ms and within 1 % by
//!   0.56 s, and never overshoots.
//! - **Retargeting.** Each landed reading samples the spring where it stands and retargets from
//!   that angle and velocity ([`Needle::retarget`]), never from the previous reading's value, so
//!   readings 500 ms apart never make it jump. The angle stays inside the dial's stops
//!   (`START..=END`); a needle that reaches a stop rests there.
//! - **Snapping.** The first reading, a reading whose whole move is under a device pixel at the
//!   tip (the dead-band), and a reading no window animates rest the needle on the target at once,
//!   its tip and its soak both; so does a layout that adds, drops or resizes a dial
//!   ([`Needle::snapped`]).
//! - **Frames.** A window draws the spring rounded to whole device pixels of tip travel from the
//!   target (and the soak to whole [`SOAK_STEP`]s), as of its last frame. Its next frame is the
//!   first moment that rounding changes, never sooner than the window's effect present interval
//!   after its last ([`Needle::next_after`]), and there is none once it rests on the target. So
//!   every frame moves the tip a whole pixel at least (or the bezel a whole soak step), a large
//!   swing runs at the effect present interval (two display periods on an Apple-silicon Mac), a
//!   small one takes a few frames, and a settled needle asks for nothing.
//! - **Never back.** Each window walks its frames on from the one it last drew ([`Drawn`],
//!   [`Needle::follow`]), so an interval that changes mid-sweep (a display of another rate, a
//!   parked effect lane) moves the needle on from where it stands, never back to a frame of the
//!   new interval's it already passed. A window that drew a sweep at rest on its reading (it had
//!   no motion) keeps it there until the next reading, so motion that comes back mid-sweep never
//!   draws the needle back where it already stood at the reading. A sweep's first frame is moved
//!   a step on where its rounding, counted from the new target, would draw a channel behind what
//!   the window last drew of the sweep before, and no frame draws a channel against the way the
//!   spring moves it: a reading that lands mid-sweep never steps the needle back.

use std::time::{Duration, Instant};

use crate::stats_gauge::{END, START};

/// The spring's rate, rad/s: critically damped, a step from rest is 80 % of the way after
/// 250 ms and within 1 % by 0.56 s, inside the deck's 1 s cadence.
pub(crate) const OMEGA: f64 = 12.0;

/// The soak channel's step: what the bezel moves per frame and the dead-band of its move.
pub(crate) const SOAK_STEP: f64 = 1.0 / 64.0;

/// What the live edge carries of a drawn needle ([`crate::stats_live::DeckLive::needle`]): its
/// angle in eighths of a degree (under a sixth of a device pixel at the tip of Full's dial at 2×)
/// and its soak in 1024ths.
pub(crate) const ANGLE_PER_DEG: f64 = 8.0;
pub(crate) const SOAK_PER_ONE: f64 = 1024.0;

/// The shortest wait between two frames, whatever a window's present interval says.
const INTERVAL_MIN: Duration = Duration::from_millis(1);

/// How finely a frame's moment is found, s.
const RESOLVE_S: f64 = 1e-5;

/// The most frames one episode is walked through (a guard: a settled spring asks none).
const FRAMES_MAX: u32 = 4096;

/// One quantity on the spring: where it stood and how fast it moved at the retarget, where it
/// goes, and the stops it stays between.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Channel {
    pub(crate) x0: f64,
    pub(crate) v0: f64,
    pub(crate) target: f64,
    lo: f64,
    hi: f64,
}

impl Channel {
    /// At rest on `at`, between `lo` and `hi`.
    fn rest(at: f64, lo: f64, hi: f64) -> Self {
        let at = at.clamp(lo, hi);
        Self {
            x0: at,
            v0: 0.0,
            target: at,
            lo,
            hi,
        }
    }

    /// The closed form's `v0 + ω(x0 − T)`.
    fn c(&self) -> f64 {
        self.v0 + OMEGA * (self.x0 - self.target)
    }

    /// Where the spring has it `u` s after the retarget, unclamped.
    fn raw(&self, u: f64) -> f64 {
        self.target + (self.x0 - self.target + self.c() * u) * (-OMEGA * u).exp()
    }

    /// Where it stands `u` s after the retarget, held between its stops.
    pub(crate) fn at(&self, u: f64) -> f64 {
        self.raw(u).clamp(self.lo, self.hi)
    }

    /// How fast it moves `u` s after the retarget, units a second; 0 at a stop.
    pub(crate) fn velocity(&self, u: f64) -> f64 {
        let raw = self.raw(u);
        if raw <= self.lo || raw >= self.hi {
            return 0.0;
        }
        (self.v0 - OMEGA * self.c() * u) * (-OMEGA * u).exp()
    }

    /// The one moment after the retarget at which it turns back (a critically damped spring
    /// turns at most once): before it the channel moves one way, after it straight to its target
    /// without crossing it.
    fn turn(&self) -> Option<f64> {
        let u = self.v0 / (OMEGA * self.c());
        (u.is_finite() && u > 0.0).then_some(u)
    }

    /// The span it sweeps from the retarget on: from where it stands to the target, and to where
    /// it turns if it turns past either.
    fn travel(&self) -> f64 {
        let (mut lo, mut hi) = (self.x0.min(self.target), self.x0.max(self.target));
        if let Some(u) = self.turn() {
            let p = self.at(u);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        hi - lo
    }

    /// The whole steps of `step` it stands from its target `u` s after the retarget, rounded.
    fn steps(&self, u: f64, step: f64) -> i64 {
        ((self.at(u) - self.target) / step).round() as i64
    }

    /// The first moment after `a` (s) at which its rounding to `step` leaves `k`, where it
    /// stands at `a`; `None` when it never does (it rests within half a step of its target).
    fn leaves(&self, a: f64, k: i64, step: f64) -> Option<f64> {
        let left = |u: f64| self.steps(u, step) != k;
        let mut a = a;
        // Up to the turn the channel moves one way, so once it has left `k` it stays out.
        if let Some(t) = self.turn().filter(|&t| t > a) {
            if left(t) {
                return Some(bisect(a, t, left));
            }
            a = t;
        }
        // From the turn on it runs straight to the target: it leaves only a step that is not
        // the target's.
        if k == 0 {
            return None;
        }
        let mut span = 1.0 / OMEGA;
        for _ in 0..64 {
            if left(a + span) {
                return Some(bisect(a, a + span, left));
            }
            span *= 2.0;
        }
        None
    }

    /// The first moment after `u` (s) at which a frame may draw it elsewhere than `drawn` (whole
    /// steps of `step` from its target), where its rounding stands at `r`: the rounding leaves
    /// `r`, or, held at `drawn` against the way it moves ([`hold`]), it turns and comes on.
    fn moves(&self, u: f64, r: i64, drawn: i64, step: f64) -> Option<f64> {
        let leaves = self.leaves(u, r, step);
        let turns = self
            .turn()
            .filter(|&t| r != drawn && t > u)
            .map(|t| t + RESOLVE_S);
        match (leaves, turns) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// `k`, the whole steps of `step` its first frame stands from its target, moved one step the
    /// way it moves where that draws it behind `drawn` (what the window last drew of the episode
    /// before) by less than a step; kept where the step on would draw it somewhere the spring
    /// never comes. `way` is the way it moves over the window's first interval (−1, 0 or 1).
    fn not_behind(&self, k: i64, drawn: f64, step: f64, way: f64) -> i64 {
        if way == 0.0 {
            return k;
        }
        let at = |k: i64| self.target + k as f64 * step;
        let behind = |k: i64| (drawn - at(k)) * way > step * 1e-6;
        let on = k + way as i64;
        // Toward the target the spring passes every step up to it before it turns; past it (or
        // away from it) only a spring that turns comes back, and the hold lets go as it turns.
        let reached = (k != 0 && way == -(k.signum() as f64)) || self.turn().is_some();
        if behind(k) && !behind(on) && reached {
            on
        } else {
            k
        }
    }
}

/// The whole steps a frame draws a channel at, where the spring's rounding stands at `r` and the
/// window's last frame drew it at `was`, the channel moving at `v` a second: the rounding, or
/// `was` where the rounding would step against the way it moves (behind a first frame moved on
/// by [`Channel::not_behind`], or past the turn of a spring that has since come back).
fn hold(r: i64, was: i64, v: f64) -> i64 {
    if (r - was) as f64 * v < 0.0 { was } else { r }
}

/// The first moment in `lo..=hi` at which `left` holds, to [`RESOLVE_S`], where `left` is false
/// at `lo`, true at `hi`, and stays true once it holds.
fn bisect(mut lo: f64, mut hi: f64, left: impl Fn(f64) -> bool) -> f64 {
    while hi - lo > RESOLVE_S {
        let mid = f64::midpoint(lo, hi);
        if left(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// How one window draws the needle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct View {
    /// Device px the tip travels per degree in this window
    /// ([`crate::stats_gauge::tip_px_per_deg`]).
    pub(crate) px_per_deg: f64,
    /// The shortest time between two of its needle frames: the window's effect present interval.
    pub(crate) interval: Duration,
}

impl View {
    /// One device pixel of tip travel, degrees.
    fn step(&self) -> f64 {
        1.0 / self.px_per_deg.max(f64::MIN_POSITIVE)
    }

    fn interval_ns(&self) -> u64 {
        u64::try_from(self.interval.max(INTERVAL_MIN).as_nanos()).unwrap_or(u64::MAX)
    }
}

/// One of a window's needle frames in an episode: its moment, ns after the retarget, and the
/// whole steps the tip and the soak stand from their targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) at_ns: u64,
    pub(crate) k: (i64, i64),
}

/// What one window last drew of the needle (`WindowState::deck_needle_drawn`), which its next
/// frames walk on from ([`Needle::follow`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Drawn {
    /// The episode it drew: its retarget's moment ([`Needle::t0`]).
    pub(crate) t0: Instant,
    /// The frame it drew; `None` when it drew the episode at rest on its target (it had no
    /// motion, or readings had stopped), which it then keeps to the episode's end.
    pub(crate) frame: Option<Frame>,
    /// The tip's angle (degrees) and the soak that frame drew.
    pub(crate) at: (f64, f64),
}

/// The dial's needle in motion: one per app, since the strain is the whole machine's
/// (`VitalsHost::needle`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Needle {
    /// When the episode began: the last retarget.
    pub(crate) t0: Instant,
    /// The tip's angle, degrees clockwise from 12 o'clock.
    pub(crate) tip: Channel,
    /// The bezel's soak, `0..=1`.
    pub(crate) soak: Channel,
}

impl Needle {
    /// At rest on `target` (the tip's angle, degrees, and the soak) from `now`.
    pub(crate) fn at_rest(now: Instant, (tip, soak): (f64, f64)) -> Self {
        Self {
            t0: now,
            tip: Channel::rest(tip, START, END),
            soak: Channel::rest(soak, 0.0, 1.0),
        }
    }

    /// Where it goes: the tip's angle and the soak.
    pub(crate) fn target(&self) -> (f64, f64) {
        (self.tip.target, self.soak.target)
    }

    fn u(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.t0).as_secs_f64()
    }

    /// The spring at `now`: the tip's angle and its velocity (degrees, degrees a second), and the
    /// soak and its velocity.
    pub(crate) fn sample(&self, now: Instant) -> [f64; 4] {
        let u = self.u(now);
        [
            self.tip.at(u),
            self.tip.velocity(u),
            self.soak.at(u),
            self.soak.velocity(u),
        ]
    }

    /// The needle after a reading lands at `now` with `target` (`None` reads nothing: no needle),
    /// in windows whose tips travel at most `px_per_deg` device px a degree (0 when no window
    /// animates a dial). It retargets from where `was` stands and how fast it moves at `now`; the
    /// first reading, a channel whose whole move is under a device pixel at the tip (or a
    /// [`SOAK_STEP`]), and both channels when no window animates a dial, rest on the target at
    /// once.
    pub(crate) fn retarget(
        was: Option<Self>,
        now: Instant,
        target: Option<(f64, f64)>,
        px_per_deg: f64,
    ) -> Option<Self> {
        let (tip, soak) = target?;
        let Some(was) = was else {
            return Some(Self::at_rest(now, (tip, soak)));
        };
        let [x, v, s, vs] = was.sample(now);
        let mut next = Self {
            t0: now,
            tip: Channel {
                x0: x,
                v0: v,
                ..Channel::rest(tip, START, END)
            },
            soak: Channel {
                x0: s,
                v0: vs,
                ..Channel::rest(soak, 0.0, 1.0)
            },
        };
        let px = if px_per_deg.is_finite() {
            px_per_deg.max(0.0)
        } else {
            0.0
        };
        if next.tip.travel() * px < 1.0 {
            next.tip = Channel::rest(next.tip.target, START, END);
        }
        if px == 0.0 || next.soak.travel() < SOAK_STEP {
            next.soak = Channel::rest(next.soak.target, 0.0, 1.0);
        }
        Some(next)
    }

    /// This needle at rest on its target from `now`: what a layout that adds, drops or resizes a
    /// dial draws.
    pub(crate) fn snapped(&self, now: Instant) -> Self {
        Self::at_rest(now, self.target())
    }

    /// The steps the tip and the soak stand from their targets `ns` after the retarget, in
    /// `view`.
    fn steps(&self, view: View, ns: u64) -> (i64, i64) {
        let u = ns as f64 / 1e9;
        (
            self.tip.steps(u, view.step()),
            self.soak.steps(u, SOAK_STEP),
        )
    }

    /// The steps a frame `ns` after the retarget draws in `view` after one that drew `was`: where
    /// the spring stands, rounded, each channel held at `was` where its rounding would step
    /// against the way it moves ([`hold`]).
    fn k_at(&self, view: View, ns: u64, was: (i64, i64)) -> (i64, i64) {
        let u = ns as f64 / 1e9;
        let r = self.steps(view, ns);
        (
            hold(r.0, was.0, self.tip.velocity(u)),
            hold(r.1, was.1, self.soak.velocity(u)),
        )
    }

    /// The moment, ns after the retarget, of the frame after `f` in `view`: the first moment no
    /// sooner than the window's interval after `f` at which the tip or the soak stands a whole
    /// step elsewhere; `None` once both rest on their targets (or where no moment within a few
    /// nanoseconds of the one found draws a step elsewhere, so no frame draws nothing new).
    fn next_ns(&self, view: View, f: Frame) -> Option<u64> {
        let mut a = f.at_ns.saturating_add(view.interval_ns());
        for _ in 0..16 {
            if self.k_at(view, a, f.k) != f.k {
                return Some(a);
            }
            let u = a as f64 / 1e9;
            let r = self.steps(view, a);
            let tip = self.tip.moves(u, r.0, f.k.0, view.step());
            let soak = self.soak.moves(u, r.1, f.k.1, SOAK_STEP);
            let t = match (tip, soak) {
                (Some(a), Some(b)) => a.min(b),
                (t, s) => t.or(s)?,
            };
            a = ((t * 1e9).ceil() as u64).max(a.saturating_add(1));
        }
        None
    }

    /// The episode's first frame in `view`: the spring at the retarget rounded to whole steps,
    /// each channel moved a step on where that would draw it behind `last` (the tip's angle and
    /// the soak the window last drew of the episode before) against the way it moves over the
    /// window's first interval ([`Channel::not_behind`]).
    fn first(&self, view: View, last: Option<(f64, f64)>) -> Frame {
        let k = self.steps(view, 0);
        let Some((tip, soak)) = last else {
            return Frame { at_ns: 0, k };
        };
        let i = view.interval.max(INTERVAL_MIN).as_secs_f64();
        let way = |c: &Channel| {
            let d = c.at(i) - c.at(0.0);
            if d == 0.0 { 0.0 } else { d.signum() }
        };
        Frame {
            at_ns: 0,
            k: (
                self.tip.not_behind(k.0, tip, view.step(), way(&self.tip)),
                self.soak.not_behind(k.1, soak, SOAK_STEP, way(&self.soak)),
            ),
        }
    }

    /// `view`'s last frame at or before `now`, walked on from `from`.
    fn walk(&self, view: View, from: Frame, now: Instant) -> Frame {
        let end =
            u64::try_from(now.saturating_duration_since(self.t0).as_nanos()).unwrap_or(u64::MAX);
        let mut f = from;
        for _ in 0..FRAMES_MAX {
            match self.next_ns(view, f) {
                Some(at_ns) if at_ns <= end => {
                    f = Frame {
                        at_ns,
                        k: self.k_at(view, at_ns, f.k),
                    };
                }
                _ => break,
            }
        }
        f
    }

    /// `view`'s last frame at or before `now`, walked from the retarget's own, in a window that
    /// drew nothing of the needle before (the tests' window; the app's follow what each drew).
    #[cfg(test)]
    pub(crate) fn frame_at(&self, view: View, now: Instant) -> Frame {
        self.walk(view, self.first(view, None), now)
    }

    /// What a window that last drew `last` draws in `view` at `now`: the frame walked on from
    /// the one it last drew of this episode, so a change of interval never takes the needle back;
    /// the episode at rest on its target to its end where it drew it so; or, new to the episode,
    /// the frame walked from its first, which is never behind what it last drew of the episode
    /// before ([`Self::first`]).
    pub(crate) fn follow(&self, view: View, last: Option<Drawn>, now: Instant) -> Drawn {
        let from = match last {
            Some(d) if d.t0 == self.t0 => match d.frame {
                Some(f) => f,
                None => return self.rested(),
            },
            _ => self.first(view, last.map(|d| d.at)),
        };
        let f = self.walk(view, from, now);
        Drawn {
            t0: self.t0,
            frame: Some(f),
            at: self.angles(view, f),
        }
    }

    /// What a window without motion draws of this episode: its target, which it keeps to the
    /// episode's end ([`Self::follow`]).
    pub(crate) fn rested(&self) -> Drawn {
        Drawn {
            t0: self.t0,
            frame: None,
            at: self.target(),
        }
    }

    /// The tip's angle (degrees) and the soak frame `f` draws in `view`, each its target's plus
    /// its whole steps.
    fn angles(&self, view: View, f: Frame) -> (f64, f64) {
        (
            (self.tip.target + f.k.0 as f64 * view.step()).clamp(START, END),
            (self.soak.target + f.k.1 as f64 * SOAK_STEP).clamp(0.0, 1.0),
        )
    }

    /// When `view` draws the frame after `f`, or `None` once the needle rests on its target.
    pub(crate) fn next_after(&self, view: View, f: Frame) -> Option<Instant> {
        self.next_ns(view, f)
            .and_then(|ns| self.t0.checked_add(Duration::from_nanos(ns)))
    }

    /// What frame `f` draws in `view`: the tip's angle in eighths of a degree and the soak in
    /// 1024ths ([`ANGLE_PER_DEG`], [`SOAK_PER_ONE`]), each its target's plus its whole steps;
    /// `None` when both stand on their targets, which the dial draws from the reading itself.
    pub(crate) fn drawn(&self, view: View, f: Frame) -> Option<(i32, i32)> {
        if f.k == (0, 0) {
            return None;
        }
        let (tip, soak) = self.angles(view, f);
        Some((
            (tip * ANGLE_PER_DEG).round() as i32,
            (soak * SOAK_PER_ONE).round() as i32,
        ))
    }
}

#[cfg(test)]
#[path = "stats_needle_tests.rs"]
mod tests;

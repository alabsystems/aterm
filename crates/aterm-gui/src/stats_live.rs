// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stats deck's live edge (`docs/DESIGN-stats-deck-live-2026-10-24.md`): the right end of
//! each heat strip is now, and the deck says so four ways. A marker stands at that end: a line
//! down the strip's rows to the % column (the value now) and a dot on the header line. While
//! readings keep coming the dot beats once at each reading, a ring that widens and fades; when
//! they stop for [`STALE_PERIODS`] cadences the line and the dot turn grey and hollow and nothing
//! moves, so a frozen deck never looks live. The strips move left with time, so it visibly flows
//! right to left: a Folded strip (the form a window draws unless Full is chosen) holds a
//! reading's second in each cell (`stats_plan::FOLD_SLOT_S`, on the snapshot's [`SlotGrid`]), and
//! each reading that opens a new slot slides it one whole cell left over the frames after it
//! ([`Flow::Slide`]); a Full strip's ten minutes glide with the wall clock, the open column
//! growing from the right end and the oldest sliding out at the left ([`Flow::Glide`]). And the
//! header over the strip carries a time scale (the planner's [`crate::stats_plan::RulerTick`]s).
//!
//! The slide, the glide and the beat are [`MotionEffect::DeckLive`]: they run only in a focused, unoccluded
//! window whose motion policy animates (never under Reduce Motion or the load-shed latch), and
//! the beat ([`SeriousEffect::DeckBeat`]) only outside serious mode and while the deck card is
//! presented alone (no lens or card folded over it). Without motion the columns stand in their
//! slots and the marker stands still; its grey still comes, as information.
//!
//! Cost: every live frame falls on one grid, [`FRAME`] apart from the moment a reading lands,
//! so a window presents at most four a second: the beat draws [`BEAT_FRAMES`] of them after each
//! reading, the slide at most [`SLIDE_FRAMES`] (the same frames), and the glide moves on the
//! grid's frames only when it has run a [`GLIDE_QUANTUM_PX`] and its open column has not ended.
//! Every frame draws each cell edge on a whole device pixel. Such a frame re-rasters only the deck
//! card's [`LiveLayer`] rects that changed (`App::splice_stats_deck`), never the whole card, from
//! one layout that reads no words and plans no header line (`stats_deck::DeckStill`).

use std::time::{Duration, Instant};

use crate::motion::{MotionEffect, SeriousEffect};
use aterm_vitals::SlotGrid;

/// Cadences a reading may be missing for before the marker says the strips stopped (the gap
/// rule's three, Stats Deck v2 §5.4).
pub(crate) const STALE_PERIODS: u32 = 3;

/// The least wait before the marker says the strips stopped, whatever the cadence.
pub(crate) const STALE_FLOOR: Duration = Duration::from_secs(2);

/// The live edge's frame grid, laid from the moment each reading lands: every frame the edge
/// asks for falls on it, so a window presents at most four live frames a second, whatever
/// glides and beats (the cost bound of `docs/DESIGN-stats-deck-live-2026-10-24.md` §6).
pub(crate) const FRAME: Duration = Duration::from_millis(250);

/// The frames one beat draws after its reading lands; it rings for that many frames of the
/// grid (750 ms).
pub(crate) const BEAT_FRAMES: u32 = 3;

/// How far the strips glide between two frames, device px: a whole pixel, so each move is one
/// crisp step left (a quarter pixel, as first built, moved too little to be seen; §11 of the
/// design). The painter draws every shift on a whole pixel ([`column_xs`]).
pub(crate) const GLIDE_QUANTUM_PX: f64 = 1.0;

/// The glide's shortest frame period: one frame of the grid.
pub(crate) const GLIDE_STEP_MIN: Duration = FRAME;

/// The glide's longest frame period: past the slowest strip's time to run a pixel (a Full slim
/// strip at 1x, 7.5 s), so every step moves a whole pixel and none draws nothing new (at 2 s, a
/// Full strip's steps were two thirds of a pixel; §12 of the design).
pub(crate) const GLIDE_STEP_MAX: Duration = Duration::from_secs(10);

/// The most frames of the grid one slide takes ([`slide_frames`]): the reading's own and the
/// three after it, so it has come in 750 ms after the reading, on the beat's frames.
pub(crate) const SLIDE_FRAMES: u32 = 4;

/// The now marker's line sits this far right of the strip's last cell, pt (leaving breathing room
/// for the real-time `now` box's thermal glow halo).
pub(crate) const LINE_DX: f64 = 2.5;

/// The dot's radius, pt.
pub(crate) const DOT_R: f64 = 2.4;

/// The dot's centre above the header's baseline, pt.
pub(crate) const DOT_RISE: f64 = 3.3;

/// How far past the dot a beat's ring reaches, pt.
pub(crate) const BEAT_REACH: f64 = 4.0;

/// The ring's opacity as the beat starts.
const BEAT_ALPHA: f32 = 0.7;

/// A stalled marker's line is dashed: dashes and gaps this long, pt.
pub(crate) const STALE_DASH: f64 = 1.5;

/// Exponential smoothing time constant for gauge, bar, core and `now`-box transitions.
pub(crate) const TWEEN_TAU: Duration = Duration::from_millis(180);

/// Settle threshold above which [`exp_blend`] snaps to `None` so idle CPU drops to zero frames.
pub(crate) const TWEEN_SETTLE: f32 = 0.985;

/// What the deck's live edge shows in one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DeckLive {
    /// The frame's wall clock, unix ms (quantized to the glide's step), while Full's strips glide
    /// ([`Flow::Glide`]); `None` stands every column in its slot.
    pub(crate) glide_ms: Option<i64>,
    /// How far a Folded strip's slide has brought its newest slot in, `0..1` of a pitch
    /// ([`Flow::Slide`]); `None` once it has come in, or with no slide.
    pub(crate) slide: Option<f32>,
    /// How far through the newest reading's beat, `0..1` ([`beat_phase`] of its frame); `None`
    /// rings nothing.
    pub(crate) beat: Option<f32>,
    /// Smooth exponential interpolation factor (`0..1`) from the previous snapshot's values toward
    /// the current snapshot's values; `None` once settled (`>= TWEEN_SETTLE`) or without motion.
    pub(crate) blend: Option<f32>,
    /// Whether readings keep coming: `false` draws the marker grey and hollow.
    pub(crate) fresh: bool,
}

impl DeckLive {
    /// Fresh readings, nothing moving: what a window without motion draws (and the tests'
    /// default).
    pub(crate) const STILL: Self = Self {
        glide_ms: None,
        slide: None,
        beat: None,
        blend: None,
        fresh: true,
    };

    /// Readings stopped (or never came).
    pub(crate) const STALE: Self = Self {
        glide_ms: None,
        slide: None,
        beat: None,
        blend: None,
        fresh: false,
    };

    /// Its part of the deck card's key; never `0`.
    pub(crate) fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.glide_ms.hash(&mut h);
        self.slide.map(f32::to_bits).hash(&mut h);
        self.beat.map(f32::to_bits).hash(&mut h);
        self.blend.map(f32::to_bits).hash(&mut h);
        self.fresh.hash(&mut h);
        h.finish() | 1
    }
}

/// Which part of the deck card a live rect holds: what moves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveLayer {
    /// A strip's columns and gridlines: they move with [`DeckLive::glide_ms`] and
    /// [`DeckLive::slide`].
    Glide,
    /// The now marker and its beat: they move with [`DeckLive::beat`] and
    /// [`DeckLive::fresh`].
    Marker,
    /// The glowing far-right `now` box on each strip: moves when [`DeckLive::fresh`] toggles or
    /// [`DeckLive::blend`] interpolates live heat, never on a lone dot beat.
    NowBox,
    /// Gauges, overlapping scope bars and core heat cells: they move only while
    /// [`DeckLive::blend`] interpolates between consecutive readings.
    Values,
}

impl LiveLayer {
    /// Whether going from `was` to `now` changes what this layer draws.
    pub(crate) fn moved(self, was: &DeckLive, now: &DeckLive) -> bool {
        match self {
            Self::Glide => was.glide_ms != now.glide_ms || was.slide != now.slide,
            Self::Marker => was.beat != now.beat || was.fresh != now.fresh,
            Self::NowBox => was.fresh != now.fresh || was.blend != now.blend,
            Self::Values => was.blend != now.blend,
        }
    }
}

/// The open column's start, unix ms, for a snapshot read at `at_ms` and columns of `period_s`
/// aligned to the wall clock (`aterm_vitals::ring::columns_of`).
pub(crate) fn open_start_ms(at_ms: i64, period_s: u32) -> i64 {
    let p = i64::from(period_s.max(1)) * 1000;
    at_ms.div_euclid(p) * p
}

/// How far the open column of a snapshot read at `at_ms` has run at `now_ms`: 0 as it opens, 1
/// at its end, and 1 after, until a reading opens the next. A clock behind the reading counts as
/// the reading's.
pub(crate) fn open_frac(at_ms: i64, now_ms: i64, period_s: u32) -> f64 {
    let p = i64::from(period_s.max(1)) * 1000;
    let ran = now_ms.max(at_ms) - open_start_ms(at_ms, period_s);
    (ran as f64 / p as f64).clamp(0.0, 1.0)
}

/// Each of a strip's `n + 1` columns at glide `f` (the one before the oldest first, the open one
/// last): its left edge in device px, or `None` when it lies wholly outside the strip. The strip
/// starts at `x0`, its cells are `cell` wide on a `pitch`, and it ends at the open column's slot's
/// right edge, which is now. At `f = 1` every column stands in its slot and the first is outside;
/// below 1 each sits `(1 − f)` pitches right of its slot, rounded to a whole device pixel so every
/// cell edge is drawn sharp, so the open column shows only the part of its period that has run,
/// and the strip's clip cuts what passes its ends.
#[cfg(test)]
pub(crate) fn column_xs(x0: f32, pitch: f32, cell: f32, n: usize, f: f64) -> Vec<Option<f32>> {
    let right = x0 + n.saturating_sub(1) as f32 * pitch + cell;
    let shift = ((1.0 - f.clamp(0.0, 1.0)) as f32 * pitch).round();
    (0..=n)
        .map(|k| {
            let x = x0 + (k as f32 - 1.0) * pitch + shift;
            (x + cell > x0 && x < right).then_some(x)
        })
        .collect()
}

/// The columns (counted as [`column_xs`] counts them) that open a wall-clock minute, when the
/// open column of `period_s` opened at `open_ms`.
#[cfg(test)]
pub(crate) fn minute_columns(open_ms: i64, period_s: u32, n: usize) -> Vec<usize> {
    let p = i64::from(period_s.max(1)) * 1000;
    (0..=n)
        .filter(|&k| (open_ms - (n - k) as i64 * p).rem_euclid(60_000) == 0)
        .collect()
}

/// The beat's ring at `phase` (`0..1`): its radius past the dot's, pt, and its opacity. It
/// widens fast then slows, and fades to nothing as it ends.
pub(crate) fn beat_ring(phase: f32) -> (f64, f32) {
    let t = phase.clamp(0.0, 1.0);
    let rest = 1.0 - t;
    let widen = 1.0 - rest * rest;
    (BEAT_REACH * f64::from(widen), BEAT_ALPHA * rest)
}

/// Exponential smoothing factor `1 - exp(-elapsed / tau)` quantized to `1/64` steps, returning
/// `None` once settled (`>= TWEEN_SETTLE`) so idle CPU schedules zero frames.
pub(crate) fn exp_blend(elapsed: Duration, tau: Duration) -> Option<f32> {
    let dt = elapsed.as_secs_f32();
    let tau_s = tau.as_secs_f32().max(0.001);
    if !dt.is_finite() || dt < 0.0 {
        return None;
    }
    let raw = 1.0 - (-dt / tau_s).exp();
    if raw >= TWEEN_SETTLE {
        return None;
    }
    let q = ((raw * 64.0).round() / 64.0).clamp(0.0, 1.0);
    (q < TWEEN_SETTLE).then_some(q)
}

/// Smooth interpolation factor on frame `k` of the live grid (`0`, the reading's own frame), or
/// `None` once settled after [`BEAT_FRAMES`] frames.
pub(crate) fn tween_phase(k: u32) -> Option<f32> {
    (k < BEAT_FRAMES).then(|| {
        exp_blend(
            Duration::from_millis(140).saturating_add(FRAME.saturating_mul(k)),
            TWEEN_TAU,
        )
    })?
}

/// Linearly interpolates between `prev` and `curr` by `blend` (`None` returns `curr`).
pub(crate) fn lerp_opt(prev: Option<f32>, curr: Option<f32>, blend: Option<f32>) -> Option<f32> {
    match (prev, curr, blend) {
        (Some(a), Some(b), Some(t)) if a.is_finite() && b.is_finite() => {
            let t = t.clamp(0.0, 1.0);
            Some(a + (b - a) * t)
        }
        _ => curr,
    }
}

/// The phase the beat draws in frame `k` of its reading's grid (`k` from 0, the reading's own
/// frame), or `None` once it has rung: frames sample the beat at `(k + 1) / (BEAT_FRAMES + 1)`,
/// so each of its frames shows a ring, wider and fainter than the last.
pub(crate) fn beat_phase(k: u32) -> Option<f32> {
    (k < BEAT_FRAMES).then(|| (k + 1) as f32 / (BEAT_FRAMES + 1) as f32)
}

/// How long a reading speaks for: [`STALE_PERIODS`] of the slower of the cadence now and the one
/// it was read at (so a window coming into focus does not grey before its first quick reading),
/// and never under [`STALE_FLOOR`].
pub(crate) fn stale_after(period_now: Duration, period_then: Option<Duration>) -> Duration {
    let period = period_then.map_or(period_now, |then| then.max(period_now));
    (period * STALE_PERIODS).max(STALE_FLOOR)
}

/// The frames a slide of `pitch_px` device px takes, at most [`SLIDE_FRAMES`]: no more than it
/// has whole pixels, so each frame moves the strip at least one, and as many as that allows up to
/// four, so each frame moves a quarter to a third of a pitch before rounding to whole pixels and
/// always under half: the eye reads every move as leftward even over a strip of one heat step
/// (a move of half a pitch or more reads either way, or backward).
pub(crate) fn slide_frames(pitch_px: f64) -> u32 {
    if !(pitch_px.is_finite() && pitch_px >= 1.0) {
        return 1;
    }
    (pitch_px.floor() as u32).clamp(1, SLIDE_FRAMES)
}

/// How far a slide of `frames` frames has brought the newest slot in at frame `k` of its
/// reading's grid (0, the reading's own): `(k + 1) / frames`, or `None` once it has come in.
pub(crate) fn slide_phase(k: u32, frames: u32) -> Option<f32> {
    (k.saturating_add(1) < frames).then(|| (k + 1) as f32 / frames as f32)
}

/// The glide's frame period for strips moving `px_per_s` device px a second: the time to move
/// [`GLIDE_QUANTUM_PX`], held to [`GLIDE_STEP_MIN`]..=[`GLIDE_STEP_MAX`].
pub(crate) fn glide_step(px_per_s: f64) -> Duration {
    if !(px_per_s.is_finite() && px_per_s > 0.0) {
        return GLIDE_STEP_MIN;
    }
    Duration::from_secs_f64(GLIDE_QUANTUM_PX / px_per_s).clamp(GLIDE_STEP_MIN, GLIDE_STEP_MAX)
}

/// How a window's strips move with time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Flow {
    /// Nothing moves: the card draws no strip, or has not been laid out yet.
    None,
    /// Full's strips glide with the wall clock, a whole device pixel each `step` ([`glide_step`]),
    /// and hold at `end_ms`, when the held snapshot's open column ends on the wall clock: past it
    /// every column stands in its slot until a reading opens the next ([`open_frac`] is 1), so
    /// the glide asks no frame that would draw nothing new. `None`: no bound.
    Glide { step: Duration, end_ms: Option<i64> },
    /// The held snapshot's reading opened a new second of a Folded strip, which slides one pitch
    /// left over `frames` frames of the grid ([`slide_frames`]); 0, it opened none and nothing
    /// moves until the next reading.
    Slide { frames: u32 },
}

/// What decides one window's live edge at a moment.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LiveFacts {
    /// When the held snapshot landed; `None` before the first.
    pub(crate) landed: Option<Instant>,
    /// How long it speaks for ([`stale_after`]).
    pub(crate) stale_after: Duration,
    /// The strips may move and the dot may beat ([`MotionEffect::DeckLive`]).
    pub(crate) motion: bool,
    /// The dot may beat ([`SeriousEffect::DeckBeat`], and the card presented alone).
    pub(crate) beat: bool,
    /// Consecutive readings are available to interpolate gauges, bars, cores and the `now` box.
    pub(crate) tween: bool,
    /// How the strips move.
    pub(crate) flow: Flow,
}

impl LiveFacts {
    fn fresh(&self, now: Instant) -> bool {
        self.landed
            .is_some_and(|at| now.saturating_duration_since(at) < self.stale_after)
    }

    /// The frame of the grid `now` falls in, counted from the reading's own (0), and how long
    /// after that frame's moment `now` is.
    fn frame_of(landed: Instant, now: Instant) -> (u32, Duration) {
        let ran = now.saturating_duration_since(landed);
        let k = ran.as_nanos() / FRAME.as_nanos();
        let k = u32::try_from(k).unwrap_or(u32::MAX);
        (k, ran.saturating_sub(FRAME.saturating_mul(k)))
    }

    /// What the deck draws at `now` (`now_ms` on the wall clock): the live edge as it stood at
    /// the last frame of the grid, so it changes only on the grid.
    pub(crate) fn live(&self, now: Instant, now_ms: i64) -> DeckLive {
        let Some(landed) = self.landed.filter(|_| self.fresh(now)) else {
            return DeckLive::STALE;
        };
        if !self.motion {
            return DeckLive::STILL;
        }
        let (k, into) = Self::frame_of(landed, now);
        let (glide_ms, slide) = match self.flow {
            Flow::None => (None, None),
            Flow::Glide { step, end_ms } => {
                let frame_ms = now_ms - i64::try_from(into.as_millis()).unwrap_or(i64::MAX);
                let step = i64::try_from(step.as_millis()).unwrap_or(i64::MAX).max(1);
                let glide = frame_ms.div_euclid(step) * step;
                (Some(end_ms.map_or(glide, |end| glide.min(end))), None)
            }
            Flow::Slide { frames } => (None, slide_phase(k, frames)),
        };
        DeckLive {
            glide_ms,
            slide,
            beat: beat_phase(k).filter(|_| self.beat),
            blend: tween_phase(k).filter(|_| self.tween),
            fresh: true,
        }
    }

    /// The first moment after `now` at which [`Self::live`] changes, or `None` when it never
    /// will without a new reading or a change of window: the grey's moment, or the first frame
    /// of the grid that draws something new.
    pub(crate) fn next_change(&self, now: Instant, now_ms: i64) -> Option<Instant> {
        let landed = self.landed?;
        let stale = landed + self.stale_after;
        if stale <= now {
            return None;
        }
        let mut next = stale;
        if self.motion {
            let was = self.live(now, now_ms);
            let (k, _) = Self::frame_of(landed, now);
            // The grid up to the grey: at most stale_after / FRAME frames (12 at a 1 s cadence).
            for j in (k.saturating_add(1)..).take(1 << 12) {
                let at = landed + FRAME.saturating_mul(j);
                if at >= next {
                    break;
                }
                let ahead = i64::try_from(at.saturating_duration_since(now).as_millis())
                    .unwrap_or(i64::MAX);
                if self.live(at, now_ms.saturating_add(ahead)) != was {
                    next = at;
                    break;
                }
            }
        }
        Some(next.max(now + Duration::from_millis(1)))
    }
}

/// Whether a reading at `at_ms` opened a slot of `grid` (the snapshot's, [`SlotGrid`]) that the
/// reading before it, at `prev_ms`, had not: a Folded strip then slides a pitch left. The first
/// reading opens none (nothing was drawn before it to slide from).
pub(crate) fn opened_slot(prev_ms: Option<i64>, at_ms: i64, grid: SlotGrid) -> bool {
    prev_ms.is_some_and(|prev| grid.opened(prev, at_ms) > 0)
}

/// The wall clock, unix ms; 0 where unreadable (the glide then stands at the reading's own
/// moment: [`open_frac`] counts a clock behind the reading as the reading's).
fn now_unix_ms() -> i64 {
    aterm_time::unix_millis()
        .and_then(|ms| i64::try_from(ms).ok())
        .unwrap_or(0)
}

impl crate::App {
    /// What decides window `wid`'s live edge now, while it draws the v2 deck on screen.
    fn live_facts(&self, wid: crate::WindowId) -> Option<LiveFacts> {
        let ws = self.windows.get(&wid)?;
        if self.stats_bars_form() != crate::stats_deck::StatsBars::Deck
            || !crate::stats_deck::stats_drawn_on(ws)
            || ws.occluded
        {
            return None;
        }
        let lens = self.any_stats_lens_open();
        let period = self
            .vitals_audience()
            .cadence(lens)
            .map_or(crate::vitals_host::FOCUSED_PERIOD, |(period, _)| period);
        let effect = MotionEffect::DeckLive;
        let motion = self
            .effect_policy(effect, self.motion_focus(wid, ws.focused))
            .animate(effect);
        let alone = ws.lens_card.is_none()
            && ws.route_card.is_none()
            && ws.settings_card.is_none()
            && ws.badge_card.is_none();
        let beat = alone && self.serious_mode_policy().allows(SeriousEffect::DeckBeat);
        let (px_per_s, glide_period_s, slide_pitch_px) = ws.deck_glide.get();
        let snap = self.vitals.snapshot.as_ref();
        let flow = if slide_pitch_px > 0.0 {
            let opened = snap.is_some_and(|snap| {
                opened_slot(
                    self.vitals.landed_prev_at_ms,
                    snap.at_unix_ms,
                    snap.slot_grid,
                )
            });
            Flow::Slide {
                frames: if opened {
                    slide_frames(slide_pitch_px)
                } else {
                    0
                },
            }
        } else if px_per_s > 0.0 {
            Flow::Glide {
                step: glide_step(px_per_s),
                end_ms: snap.filter(|_| glide_period_s > 0).map(|snap| {
                    open_start_ms(snap.at_unix_ms, glide_period_s)
                        + i64::from(glide_period_s) * 1000
                }),
            }
        } else {
            Flow::None
        };
        Some(LiveFacts {
            landed: snap.and(self.vitals.landed_at),
            stale_after: stale_after(period, self.vitals.landed_period),
            motion,
            beat,
            tween: snap.is_some() && self.vitals.prev_snapshot.is_some(),
            flow,
        })
    }

    /// What window `wid`'s deck draws of its live edge now: [`DeckLive::STILL`] for a window
    /// that draws no v2 deck on screen (its card is not presented, or another painter draws it).
    pub(crate) fn deck_live(&self, wid: crate::WindowId) -> DeckLive {
        let now = Instant::now();
        self.live_facts(wid)
            .map_or(DeckLive::STILL, |facts| facts.live(now, now_unix_ms()))
    }

    /// The loop's deadline for the next change of any window's live edge.
    pub(crate) fn stats_live_deadline(&self, now: Instant) -> Option<Instant> {
        let now_ms = now_unix_ms();
        self.windows
            .keys()
            .filter_map(|&wid| self.live_facts(wid)?.next_change(now, now_ms))
            .min()
    }

    /// Asks a frame of each window whose live edge no longer matches what its card last drew.
    pub(crate) fn service_stats_live(&self, now: Instant) {
        let now_ms = now_unix_ms();
        for (&wid, ws) in &self.windows {
            let Some(facts) = self.live_facts(wid) else {
                continue;
            };
            if ws.deck_live_drawn.get() != Some(facts.live(now, now_ms).key())
                && let Some(window) = ws.os_window.as_ref()
            {
                window.request_redraw();
            }
        }
    }
}

#[cfg(test)]
#[path = "stats_live_tests.rs"]
mod tests;

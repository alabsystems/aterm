// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stats deck's live edge (`docs/DESIGN-stats-deck-live-2026-10-24.md`): the right end of
//! each heat strip is now, and the deck says so four ways. A marker stands at that end: a line
//! down the strip's rows to the % column (the value now) and a dot on the header line. While
//! readings keep coming the dot beats once at each reading, a ring that widens and fades; when
//! they stop for [`STALE_PERIODS`] cadences the line and the dot turn grey and hollow and nothing
//! moves, so a frozen deck never looks live. Between readings the strips glide left with the wall
//! clock: the open column grows from the right end and the oldest slides out at the left, so time
//! visibly flows right to left. And the header over the strip carries a time scale (the planner's
//! [`crate::stats_plan::RulerTick`]s).
//!
//! The glide and the beat are [`MotionEffect::DeckLive`]: they run only in a focused, unoccluded
//! window whose motion policy animates (never under Reduce Motion or the load-shed latch), and
//! the beat ([`SeriousEffect::DeckBeat`]) only outside serious mode and while the deck card is
//! presented alone (no lens or card folded over it). Without motion the columns stand in their
//! slots and the marker stands still; its grey still comes, as information.
//!
//! Cost: the frames are scheduled at the glide's quantum ([`GLIDE_QUANTUM_PX`], at most
//! [`GLIDE_STEP_MIN`] apart) and the beat's ([`BEAT_FRAME`] for [`BEAT`] after each reading), and
//! such a frame re-rasters only the deck card's [`LiveLayer`] rects that changed
//! (`App::splice_stats_deck`), never the whole card.

use std::time::{Duration, Instant};

use crate::motion::{MotionEffect, SeriousEffect};

/// Cadences a reading may be missing for before the marker says the strips stopped (the gap
/// rule's three, Stats Deck v2 §5.4).
pub(crate) const STALE_PERIODS: u32 = 3;

/// The least wait before the marker says the strips stopped, whatever the cadence.
pub(crate) const STALE_FLOOR: Duration = Duration::from_secs(2);

/// How long one beat rings after its reading lands.
pub(crate) const BEAT: Duration = Duration::from_millis(840);

/// The beat's frame period: about 17 frames a second, while it rings.
pub(crate) const BEAT_FRAME: Duration = Duration::from_millis(60);

/// The most the strips glide between two frames, device px.
pub(crate) const GLIDE_QUANTUM_PX: f64 = 0.25;

/// The glide's shortest frame period: at most eight frames a second.
pub(crate) const GLIDE_STEP_MIN: Duration = Duration::from_millis(125);

/// The glide's longest frame period.
pub(crate) const GLIDE_STEP_MAX: Duration = Duration::from_secs(2);

/// The now marker's line sits this far right of the strip's last cell, pt.
pub(crate) const LINE_DX: f64 = 1.5;

/// The dot's radius, pt.
pub(crate) const DOT_R: f64 = 2.4;

/// The dot's centre above the header's baseline, pt.
pub(crate) const DOT_RISE: f64 = 3.3;

/// How far past the dot a beat's ring reaches, pt.
pub(crate) const BEAT_REACH: f64 = 4.0;

/// The ring's opacity as the beat starts.
const BEAT_ALPHA: f32 = 0.7;

/// The line's opacity while live: quieter than the dot, which carries the beat.
pub(crate) const LINE_ALPHA: u8 = 0x9C;

/// What the deck's live edge shows in one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DeckLive {
    /// The frame's wall clock, unix ms (quantized to the glide's step), while the strips glide;
    /// `None` stands every column in its slot.
    pub(crate) glide_ms: Option<i64>,
    /// How far through the newest reading's beat, `0..1` (quantized to [`BEAT_FRAME`]); `None`
    /// rings nothing.
    pub(crate) beat: Option<f32>,
    /// Whether readings keep coming: `false` draws the marker grey and hollow.
    pub(crate) fresh: bool,
}

impl DeckLive {
    /// Fresh readings, nothing moving: what a window without motion draws (and the tests'
    /// default).
    pub(crate) const STILL: Self = Self {
        glide_ms: None,
        beat: None,
        fresh: true,
    };

    /// Readings stopped (or never came).
    pub(crate) const STALE: Self = Self {
        glide_ms: None,
        beat: None,
        fresh: false,
    };

    /// Its part of the deck card's key; never `0`.
    pub(crate) fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.glide_ms.hash(&mut h);
        self.beat.map(f32::to_bits).hash(&mut h);
        self.fresh.hash(&mut h);
        h.finish() | 1
    }
}

/// Which part of the deck card a live rect holds: what moves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveLayer {
    /// A strip's columns and gridlines: they move with [`DeckLive::glide_ms`].
    Glide,
    /// The now marker and its beat: they move with [`DeckLive::beat`] and
    /// [`DeckLive::fresh`].
    Marker,
}

impl LiveLayer {
    /// Whether going from `was` to `now` changes what this layer draws.
    pub(crate) fn moved(self, was: &DeckLive, now: &DeckLive) -> bool {
        match self {
            Self::Glide => was.glide_ms != now.glide_ms,
            Self::Marker => was.beat != now.beat || was.fresh != now.fresh,
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
/// below 1 each sits `(1 − f)` pitches right of its slot, so the open column shows only the part
/// of its period that has run, and the strip's clip cuts what passes its ends.
pub(crate) fn column_xs(x0: f32, pitch: f32, cell: f32, n: usize, f: f64) -> Vec<Option<f32>> {
    let right = x0 + n.saturating_sub(1) as f32 * pitch + cell;
    let shift = (1.0 - f.clamp(0.0, 1.0)) as f32 * pitch;
    (0..=n)
        .map(|k| {
            let x = x0 + (k as f32 - 1.0) * pitch + shift;
            (x + cell > x0 && x < right).then_some(x)
        })
        .collect()
}

/// The columns (counted as [`column_xs`] counts them) that open a wall-clock minute, when the
/// open column of `period_s` opened at `open_ms`.
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
    let widen = 1.0 - rest * rest * rest;
    (BEAT_REACH * f64::from(widen), BEAT_ALPHA * rest * rest)
}

/// How long a reading speaks for: [`STALE_PERIODS`] of the slower of the cadence now and the one
/// it was read at (so a window coming into focus does not grey before its first quick reading),
/// and never under [`STALE_FLOOR`].
pub(crate) fn stale_after(period_now: Duration, period_then: Option<Duration>) -> Duration {
    let period = period_then.map_or(period_now, |then| then.max(period_now));
    (period * STALE_PERIODS).max(STALE_FLOOR)
}

/// The glide's frame period for strips moving `px_per_s` device px a second: the time to move
/// [`GLIDE_QUANTUM_PX`], held to [`GLIDE_STEP_MIN`]..=[`GLIDE_STEP_MAX`].
pub(crate) fn glide_step(px_per_s: f64) -> Duration {
    if !(px_per_s.is_finite() && px_per_s > 0.0) {
        return GLIDE_STEP_MIN;
    }
    Duration::from_secs_f64(GLIDE_QUANTUM_PX / px_per_s).clamp(GLIDE_STEP_MIN, GLIDE_STEP_MAX)
}

/// What decides one window's live edge at a moment.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LiveFacts {
    /// When the held snapshot landed; `None` before the first.
    pub(crate) landed: Option<Instant>,
    /// How long it speaks for ([`stale_after`]).
    pub(crate) stale_after: Duration,
    /// The strips may glide and the dot may beat ([`MotionEffect::DeckLive`]).
    pub(crate) motion: bool,
    /// The dot may beat ([`SeriousEffect::DeckBeat`], and the card presented alone).
    pub(crate) beat: bool,
    /// The glide's frame period ([`glide_step`]).
    pub(crate) step: Duration,
}

impl LiveFacts {
    fn fresh(&self, now: Instant) -> bool {
        self.landed
            .is_some_and(|at| now.saturating_duration_since(at) < self.stale_after)
    }

    /// What the deck draws at `now` (`now_ms` on the wall clock).
    pub(crate) fn live(&self, now: Instant, now_ms: i64) -> DeckLive {
        if !self.fresh(now) {
            return DeckLive::STALE;
        }
        if !self.motion {
            return DeckLive::STILL;
        }
        let step = i64::try_from(self.step.as_millis())
            .unwrap_or(i64::MAX)
            .max(1);
        let beat = self.landed.filter(|_| self.beat).and_then(|at| {
            let ran = now.saturating_duration_since(at);
            (ran < BEAT).then(|| {
                let frames = ran.as_millis() / BEAT_FRAME.as_millis();
                let at_frame = BEAT_FRAME.as_millis() * frames;
                at_frame as f32 / BEAT.as_millis() as f32
            })
        });
        DeckLive {
            glide_ms: Some(now_ms.div_euclid(step) * step),
            beat,
            fresh: true,
        }
    }

    /// The first moment after `now` at which [`Self::live`] changes, or `None` when it never
    /// will without a new reading or a change of window.
    pub(crate) fn next_change(&self, now: Instant, now_ms: i64) -> Option<Instant> {
        let landed = self.landed?;
        let stale = landed + self.stale_after;
        if stale <= now {
            return None;
        }
        let mut next = stale;
        if self.motion {
            let step = i64::try_from(self.step.as_millis())
                .unwrap_or(i64::MAX)
                .max(1);
            let to_step = step - now_ms.rem_euclid(step);
            next = next.min(now + Duration::from_millis(to_step.unsigned_abs()));
            if self.beat {
                let ran = now.saturating_duration_since(landed);
                if ran < BEAT {
                    let frames = ran.as_millis() / BEAT_FRAME.as_millis() + 1;
                    let at = BEAT_FRAME
                        .saturating_mul(u32::try_from(frames).unwrap_or(u32::MAX))
                        .min(BEAT);
                    next = next.min(landed + at);
                }
            }
        }
        Some(next.max(now + Duration::from_millis(1)))
    }
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
            || self.deck_style() != crate::stats_deck::DeckStyle::V2
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
        Some(LiveFacts {
            landed: self.vitals.snapshot.as_ref().and(self.vitals.landed_at),
            stale_after: stale_after(period, self.vitals.landed_period),
            motion,
            beat,
            step: glide_step(ws.deck_glide.get()),
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

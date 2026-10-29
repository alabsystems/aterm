// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stats deck's live edge (`docs/DESIGN-stats-deck-live-2026-10-24.md`, now at the left since
//! `docs/DESIGN-stats-deck-now-left-2026-10-26.md`): the left end of each heat strip is now, and
//! the deck says so four ways. A marker stands at that end: a line down the strip's rows, between
//! the % column (the value now) and the live box, and a dot on the header line. While
//! readings keep coming the dot beats once at each reading, a ring that widens and fades; when
//! they stop for [`STALE_PERIODS`] cadences the line and the dot turn grey and hollow and nothing
//! moves, so a frozen deck never looks live. The strips' fine band (the boxes of their finest
//! period, next to the live box, [`FineBand`]) moves right with time, so the deck visibly flows
//! away from now: each reading that opens a slot of the snapshot's [`SlotGrid`] moves it by the
//! time it opened ([`FineBand::step_px`]: a 1 s reading on 1 s boxes moves a whole box), and a
//! reading that opens none leaves it to the one that did ([`Opened`]). A Folded strip (the form a
//! window draws unless Full is chosen) slides it in over the frames after the reading
//! ([`Flow::Slide`]); a Full strip glides it with the wall clock over a slot's length from the
//! reading and holds there ([`Flow::Glide`]). The box entering at the band's old end shows the
//! band's own oldest box, and the live box, the coarser boxes and the ticks stand still. And the
//! header over the strip carries a time scale (the planner's [`crate::stats_plan::RulerTick`]s).
//!
//! The slide, the glide and the beat are [`MotionEffect::DeckLive`]: they run only in a focused, unoccluded
//! window whose motion policy animates (never under Reduce Motion or the load-shed latch), and
//! the beat ([`SeriousEffect::DeckBeat`]) only outside serious mode and while the deck card is
//! presented alone (no lens or card folded over it). Without motion the boxes stand in their
//! slots and the marker stands still; its grey still comes, as information.
//!
//! Cost: every live frame but the needle's falls on one grid, [`FRAME`] apart from the moment a
//! reading lands, so a window presents at most four of them a second: the beat draws
//! [`BEAT_FRAMES`] of them after each reading, the slide at most [`SLIDE_FRAMES`] (the same
//! frames), and the glide moves on the grid's frames only when it has run a [`GLIDE_QUANTUM_PX`]
//! and has not reached its end.
//! Every frame draws each cell edge on a whole device pixel. Such a frame re-rasters only the deck
//! card's [`LiveLayer`] rects that changed (`App::splice_stats_deck`), never the whole card, from
//! one layout that reads no words and plans no header line (`stats_deck::DeckStill`).
//!
//! The dial's needle keeps its own clock off the grid ([`crate::stats_needle`]): while it sweeps
//! to a reading, a window whose motion animates draws a frame each time its tip has moved a whole
//! device pixel, never sooner than the window's effect present interval, and none once it rests
//! ([`LiveFacts::next_needle`], the loop's `stats_needle` deadline). Such a frame re-rasters only
//! the dial ([`LiveLayer::Gauge`]) and is not counted against the grid's four a second.
//!
//! The dial steams while the Mac strains (`crate::stats_steam`,
//! `docs/DESIGN-stats-deck-owner-2026-10-26.md` §5): its wisps rise on the grid's own frames, the
//! beat's ([`DeckLive::steam`]), only while the deck's last layout drew a dial hot enough
//! ([`LiveLayer::Steam`]), outside serious mode ([`SeriousEffect::DeckSteam`]) and with the card
//! presented alone. So it asks no frame the beat does not, and none while the dial is cool,
//! without motion or once readings stop.

use std::time::{Duration, Instant};

use crate::motion::{MotionEffect, SeriousEffect};
use crate::stats_needle::{Drawn, Needle, View};
use aterm_vitals::SlotGrid;

/// Cadences a reading may be missing for before the marker says the strips stopped (the gap
/// rule's three, Stats Deck v2 §5.4).
pub(crate) const STALE_PERIODS: u32 = 3;

/// The least wait before the marker says the strips stopped, whatever the cadence.
pub(crate) const STALE_FLOOR: Duration = Duration::from_secs(2);

/// The live edge's frame grid, laid from the moment each reading lands: every frame the edge
/// asks for but the needle's falls on it, so a window presents at most four grid frames a second,
/// whatever glides and beats (the cost bound of `docs/DESIGN-stats-deck-live-2026-10-24.md` §6;
/// the needle's own is [`LiveFacts::next_needle`]'s).
pub(crate) const FRAME: Duration = Duration::from_millis(250);

/// The frames one beat draws after its reading lands; it rings for that many frames of the
/// grid (750 ms).
pub(crate) const BEAT_FRAMES: u32 = 3;

/// How far the strips glide at least in one step of the glide's clock, device px: a whole
/// pixel, so each step is one crisp move (a quarter pixel, as first built, moved too little to be
/// seen; §11 of the design). Its steps are whole milliseconds rounded up ([`glide_step`]), so no
/// step runs under this, and the painter draws every shift on a whole pixel ([`lag_px`]). A frame
/// of the grid moves the band what the glide ran since the last: since the glide runs a slot's
/// length ([`glide_end_ms`]) and frames come at most four a second, a quarter of the reading's
/// step or one step of the glide's clock (about a pixel), whichever is more, rounded up to whole
/// pixels; under half a box on boxes of 3 px or more.
pub(crate) const GLIDE_QUANTUM_PX: f64 = 1.0;

/// The glide's shortest frame period: one frame of the grid.
pub(crate) const GLIDE_STEP_MIN: Duration = FRAME;

/// The glide's longest frame period. A glide that would move its fine band under a whole pixel
/// over its run asks no frame at all ([`Flow::None`]), so its steps come well inside this bound
/// and every step moves a whole pixel.
pub(crate) const GLIDE_STEP_MAX: Duration = Duration::from_secs(10);

/// The most frames of the grid one slide takes ([`slide_frames`]): the reading's own and the
/// three after it, so it has come in 750 ms after the reading, on the beat's frames.
pub(crate) const SLIDE_FRAMES: u32 = 4;

/// The now marker's line sits this far left of the strip's first cell, the live `now` box, pt
/// (leaving breathing room between the line and the box).
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
    /// How far a reading has carried the strips' fine band right in this frame ([`Flow`]): a
    /// Folded strip's slide or a Full one's glide; `None` stands every box in its slot (the move
    /// has run, or nothing moves).
    pub(crate) flow: Option<FlowAt>,
    /// How far through the newest reading's beat, `0..1` ([`beat_phase`] of its frame); `None`
    /// rings nothing.
    pub(crate) beat: Option<f32>,
    /// Smooth exponential interpolation factor (`0..1`) from the previous snapshot's values toward
    /// the current snapshot's values; `None` once settled (`>= TWEEN_SETTLE`) or without motion.
    pub(crate) blend: Option<f32>,
    /// The dial's needle in motion as this frame draws it ([`Needle::drawn`]): its angle in
    /// eighths of a degree and the bezel's soak in 1024ths; `None` draws the reading itself (at
    /// rest, without motion, or with no dial).
    pub(crate) needle: Option<(i32, i32)>,
    /// The dial's steam clock at this frame, ms ([`steam_ms`] of the frame's moment, which runs
    /// on over the beat's frames and holds after them); `None` draws no steam (a cool dial, no
    /// motion, serious mode, a card over the deck, or readings stopped).
    pub(crate) steam: Option<i64>,
    /// Whether readings keep coming: `false` draws the marker grey and hollow.
    pub(crate) fresh: bool,
}

impl DeckLive {
    /// Fresh readings, nothing moving: what a window without motion draws (and the tests'
    /// default).
    pub(crate) const STILL: Self = Self {
        flow: None,
        beat: None,
        blend: None,
        needle: None,
        steam: None,
        fresh: true,
    };

    /// Readings stopped (or never came).
    pub(crate) const STALE: Self = Self {
        flow: None,
        beat: None,
        blend: None,
        needle: None,
        steam: None,
        fresh: false,
    };

    /// Its part of the deck card's key; never `0`.
    pub(crate) fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.flow
            .map(|f| (f.opened_ms, f.ran.to_bits()))
            .hash(&mut h);
        self.beat.map(f32::to_bits).hash(&mut h);
        self.blend.map(f32::to_bits).hash(&mut h);
        self.needle.hash(&mut h);
        self.steam.hash(&mut h);
        self.fresh.hash(&mut h);
        h.finish() | 1
    }
}

/// Which part of the deck card a live rect holds: what moves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveLayer {
    /// A strip's boxes: their fine band moves with [`DeckLive::flow`].
    Glide,
    /// The now marker and its beat: they move with [`DeckLive::beat`] and
    /// [`DeckLive::fresh`].
    Marker,
    /// The `now` box at the left end of each strip: moves when [`DeckLive::fresh`]
    /// toggles or [`DeckLive::blend`] interpolates live heat, never on a lone dot beat.
    NowBox,
    /// The scope bars and core heat cells: they move only while [`DeckLive::blend`]
    /// interpolates between consecutive readings.
    Values,
    /// The dial's slot, its bloom included: it moves with [`DeckLive::needle`].
    Gauge,
    /// The dial's slot again, for its steam: it moves with [`DeckLive::steam`], and the layout
    /// lays it only while the dial reads hot enough to steam.
    Steam,
}

impl LiveLayer {
    /// Whether going from `was` to `now` changes what this layer draws.
    pub(crate) fn moved(self, was: &DeckLive, now: &DeckLive) -> bool {
        match self {
            Self::Glide => was.flow != now.flow,
            Self::Marker => was.beat != now.beat || was.fresh != now.fresh,
            Self::NowBox => was.fresh != now.fresh || was.blend != now.blend,
            Self::Values => was.blend != now.blend,
            Self::Gauge => was.needle != now.needle,
            Self::Steam => was.steam != now.steam,
        }
    }
}

/// A strip's fine band (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §3.4): the boxes of its
/// finest history period, right of the live box, which a reading's slide or glide moves right.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FineBand {
    /// One box's pitch, device px: its width and the gap after it (0 with no band).
    pub(crate) box_px: f64,
    /// One box's period, ms.
    pub(crate) period_ms: i64,
}

impl FineBand {
    /// How far a reading that opened `opened_ms` of the snapshot's slots moves the band right,
    /// whole device px: the box's pitch times the time opened over the box's period, at most one
    /// box, so it is time-true (a 1 s reading on 1 s boxes moves a whole box); 0 when it opened
    /// none.
    pub(crate) fn step_px(self, opened_ms: i64) -> f64 {
        if self.period_ms <= 0 || opened_ms <= 0 || !(self.box_px.is_finite() && self.box_px > 0.0)
        {
            return 0.0;
        }
        let part = opened_ms.min(self.period_ms) as f64 / self.period_ms as f64;
        (self.box_px * part).round()
    }

    /// The faster of two bands: the one that moves more pixels a second.
    pub(crate) fn faster(self, other: Self) -> Self {
        let speed = |b: Self| {
            if b.period_ms > 0 {
                b.box_px / b.period_ms as f64
            } else {
                0.0
            }
        };
        if speed(other) > speed(self) {
            other
        } else {
            self
        }
    }
}

/// How far a reading has carried the strips' fine band in one frame ([`DeckLive::flow`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FlowAt {
    /// How much of the snapshot's slot grid the reading that moves the band opened, ms
    /// ([`Opened::ms`]): it sets the band's step ([`FineBand::step_px`]).
    pub(crate) opened_ms: i64,
    /// How much of the step has run, `0..1`: a Folded slide's phase ([`slide_phase`]), a Full
    /// glide's share of its run on the glide's steps ([`glide_frac`]).
    pub(crate) ran: f64,
}

/// The reading a strip's fine band moves for: the newest that opened a slot of its snapshot's
/// grid ([`opened_ms`]). A reading that opens none (the lens's second in a second) leaves the
/// band to the one before it, whose slide or glide runs on to its end rather than stopping short
/// with the rest of its step undrawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Opened {
    /// Its wall clock, unix ms.
    pub(crate) at_ms: i64,
    /// How much of the grid it opened past the reading before it, ms (0 for the first).
    pub(crate) ms: i64,
    /// When it landed: a slide's frames count from here.
    pub(crate) landed: Instant,
}

impl Opened {
    /// What the band moves for once a reading at `at_ms` on `grid` lands at `landed`, after a
    /// reading at `prev_ms` while it moved for `was`: the new reading where it opened a slot or
    /// nothing moved for one before it, else `was`.
    pub(crate) fn after(
        was: Option<Self>,
        prev_ms: Option<i64>,
        at_ms: i64,
        grid: SlotGrid,
        landed: Instant,
    ) -> Self {
        let ms = opened_ms(prev_ms, at_ms, grid);
        match was {
            Some(was) if ms == 0 => was,
            _ => Self { at_ms, ms, landed },
        }
    }
}

/// How much of the snapshot's slot grid a reading at `at_ms` opened past the one before it at
/// `prev_ms`, ms: whole slots, 0 for one in the same slot, a clock set back, or the first reading
/// (nothing was drawn before it to move from).
pub(crate) fn opened_ms(prev_ms: Option<i64>, at_ms: i64, grid: SlotGrid) -> i64 {
    prev_ms.map_or(0, |prev| {
        grid.opened(prev, at_ms)
            .max(0)
            .saturating_mul(grid.slot_ms.max(1))
    })
}

/// Where Full's glide for a reading at `at_ms` on `grid` ends and holds, unix ms: a slot's
/// length after the reading, so the band moves at the axis's own speed for a reading a slot after
/// the last (its step over the slot) and each frame of the grid carries about a quarter of the
/// step (see [`GLIDE_QUANTUM_PX`]), whatever the reading's place in its slot. Gliding to the
/// slot's end instead packed the
/// step into what was left of the slot: half a box in a frame for a reading late in it (the
/// review of 2026-10-26). The next reading lands about then and moves the band anew.
pub(crate) fn glide_end_ms(at_ms: i64, grid: SlotGrid) -> i64 {
    at_ms.saturating_add(grid.slot_ms.max(1))
}

/// How far Full's glide has run at `now_ms` for a reading at `at_ms` whose glide ends at `end_ms`
/// ([`glide_end_ms`]): 0 at the reading, 1 at its end and after, until a reading opens the next
/// slot. A clock behind the reading counts as the reading's, so the band never glides backward.
pub(crate) fn glide_frac(at_ms: i64, now_ms: i64, end_ms: i64) -> f64 {
    let run = end_ms.saturating_sub(at_ms);
    if run <= 0 {
        return 1.0;
    }
    (now_ms.max(at_ms).saturating_sub(at_ms) as f64 / run as f64).clamp(0.0, 1.0)
}

/// How far a fine band moving `step_px` still lags its slots once `ran` (`0..=1`) of the move has
/// run, whole device px toward now: the part not yet run, rounded up, so it is `step_px` as the
/// move starts, 0 only once it has run, and every frame that runs at least a pixel's worth moves
/// it at least one.
pub(crate) fn lag_px(step_px: f64, ran: f64) -> f64 {
    if !(step_px.is_finite() && step_px > 0.0) {
        return 0.0;
    }
    let ran = if ran.is_nan() {
        1.0
    } else {
        ran.clamp(0.0, 1.0)
    };
    ((1.0 - ran) * step_px - 1e-6)
        .ceil()
        .clamp(0.0, step_px.ceil())
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

/// The frames a slide of `step_px` device px takes ([`FineBand::step_px`]), at most
/// [`SLIDE_FRAMES`]: no more than it has whole pixels, so each frame moves the band at least one
/// ([`lag_px`]), and as many as that allows up to four, so on boxes of 3 px or more each frame
/// moves under half a box: the eye reads every move as rightward even over a strip of one heat
/// step (a move of half a box or more reads either way, or backward).
pub(crate) fn slide_frames(step_px: f64) -> u32 {
    if !(step_px.is_finite() && step_px >= 1.0) {
        return 1;
    }
    (step_px.floor() as u32).clamp(1, SLIDE_FRAMES)
}

/// How far a slide of `frames` frames has carried the fine band at frame `k` of its reading's
/// grid (0, the reading's own): `(k + 1) / frames` of its step, or `None` once it has come in.
pub(crate) fn slide_phase(k: u32, frames: u32) -> Option<f32> {
    (k.saturating_add(1) < frames).then(|| (k + 1) as f32 / frames as f32)
}

/// The glide's frame period for strips moving `px_per_s` device px a second: the time to move
/// [`GLIDE_QUANTUM_PX`] in whole milliseconds rounded up, held to
/// [`GLIDE_STEP_MIN`]..=[`GLIDE_STEP_MAX`]. The glide's clock counts whole milliseconds
/// ([`LiveFacts::live`]), and a step cut short of the quantum could leave the band's lag where it
/// was while the frame's key changed: a frame that drew nothing new (the review of 2026-10-26).
pub(crate) fn glide_step(px_per_s: f64) -> Duration {
    if !(px_per_s.is_finite() && px_per_s > 0.0) {
        return GLIDE_STEP_MIN;
    }
    let (lo, hi) = (
        GLIDE_STEP_MIN.as_secs_f64() * 1000.0,
        GLIDE_STEP_MAX.as_secs_f64() * 1000.0,
    );
    let ms = (1000.0 * GLIDE_QUANTUM_PX / px_per_s).ceil().clamp(lo, hi);
    Duration::from_millis(ms as u64)
}

/// How a window's strips move with time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Flow {
    /// Nothing moves: the card draws no strip, has not been laid out yet, or the reading its band
    /// moves for opened too little to move it a pixel.
    None,
    /// Full's fine bands glide right with the wall clock, the step of a reading that opened
    /// `opened_ms`, from the reading at `from_ms`, a whole device pixel or more each `step`
    /// ([`glide_step`]), and hold at `end_ms` ([`glide_end_ms`]): past it every box stands in its
    /// slot until a reading opens the next slot ([`glide_frac`] is 1), so the glide asks no frame
    /// that would draw nothing new.
    Glide {
        opened_ms: i64,
        from_ms: i64,
        end_ms: i64,
        step: Duration,
    },
    /// A Folded strip's fine band slides the step of a reading that opened `opened_ms` right over
    /// `frames` frames of the grid ([`slide_frames`]) from when it `landed`.
    Slide {
        opened_ms: i64,
        landed: Instant,
        frames: u32,
    },
}

impl Flow {
    /// How a card whose Full strips glide the fine band `glide` and whose Folded strips slide
    /// `slide` (its layout's, [`crate::stats_deck::LiveOut`]; the default for none) moves for the
    /// reading `opened` on `grid`: a Folded card slides its step ([`FineBand::step_px`]) over the
    /// frames after it lands, a Full one glides it a slot's length from the reading, and one whose
    /// reading opened under a pixel's step moves nothing.
    pub(crate) fn of((glide, slide): (FineBand, FineBand), opened: Opened, grid: SlotGrid) -> Self {
        if slide.box_px > 0.0 {
            let step = slide.step_px(opened.ms);
            return if step >= 1.0 {
                Self::Slide {
                    opened_ms: opened.ms,
                    landed: opened.landed,
                    frames: slide_frames(step),
                }
            } else {
                Self::None
            };
        }
        let end = glide_end_ms(opened.at_ms, grid);
        let step = glide.step_px(opened.ms);
        let run_s = (end - opened.at_ms) as f64 / 1000.0;
        if step >= 1.0 && run_s > 0.0 {
            Self::Glide {
                opened_ms: opened.ms,
                from_ms: opened.at_ms,
                end_ms: end,
                step: glide_step(step / run_s),
            }
        } else {
            Self::None
        }
    }
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
    /// Consecutive readings are available to interpolate bars, cores and the `now` box.
    pub(crate) tween: bool,
    /// How the strips move.
    pub(crate) flow: Flow,
    /// The dial's needle in motion and how this window draws it; `None` when the window draws
    /// no dial or nothing was read.
    pub(crate) needle: Option<(Needle, View)>,
    /// What this window last drew of the needle, which its frames walk on from
    /// ([`Needle::follow`]); `None` before it drew any.
    pub(crate) drawn: Option<Drawn>,
    /// The dial steams: the deck's last layout drew a dial hot enough ([`LiveLayer::Steam`]),
    /// [`SeriousEffect::DeckSteam`] is allowed and the card is presented alone.
    pub(crate) steam: bool,
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
    /// the last frame of the grid, so it changes only on the grid, and the needle as it stood at
    /// its own last frame ([`Self::next_needle`]).
    pub(crate) fn live(&self, now: Instant, now_ms: i64) -> DeckLive {
        let grid = self.grid(now, now_ms);
        if !grid.fresh || !self.motion {
            return grid;
        }
        DeckLive {
            needle: self
                .needle
                .zip(self.needle_at(now))
                .and_then(|((n, view), d)| d.frame.and_then(|f| n.drawn(view, f))),
            ..grid
        }
    }

    /// What this window draws of the needle at `now`, walked on from what it last drew
    /// ([`Needle::follow`]): at rest on the reading without motion or once readings stop; `None`
    /// with no needle. The splice keeps it as what the window drew
    /// (`WindowState::deck_needle_drawn`), so a window that drew a sweep at rest keeps it there
    /// when its motion comes back, and one whose effect interval changes mid-sweep never draws
    /// the needle back.
    pub(crate) fn needle_at(&self, now: Instant) -> Option<Drawn> {
        let (n, view) = self.needle?;
        if !self.motion || !self.fresh(now) {
            return Some(n.rested());
        }
        Some(n.follow(view, self.drawn, now))
    }

    /// [`Self::live`] without the needle: what the grid draws.
    fn grid(&self, now: Instant, now_ms: i64) -> DeckLive {
        let Some(landed) = self.landed.filter(|_| self.fresh(now)) else {
            return DeckLive::STALE;
        };
        if !self.motion {
            return DeckLive::STILL;
        }
        let (k, into) = Self::frame_of(landed, now);
        let flow = match self.flow {
            Flow::None => None,
            Flow::Glide {
                opened_ms,
                from_ms,
                end_ms,
                step,
            } => {
                let frame_ms = now_ms - i64::try_from(into.as_millis()).unwrap_or(i64::MAX);
                let step = i64::try_from(step.as_millis()).unwrap_or(i64::MAX).max(1);
                let ran = frame_ms.saturating_sub(from_ms).max(0);
                let glide = from_ms.saturating_add(ran.div_euclid(step) * step);
                let ran = glide_frac(from_ms, glide.min(end_ms), end_ms);
                (ran < 1.0).then_some(FlowAt { opened_ms, ran })
            }
            Flow::Slide {
                opened_ms,
                landed: from,
                frames,
            } => {
                // The slide's frame at this frame of the grid's moment: the grid's own, unless a
                // reading that opened nothing landed since the one the band moves for.
                let (j, _) = Self::frame_of(from, now.checked_sub(into).unwrap_or(now));
                slide_phase(j, frames).map(|phase| FlowAt {
                    opened_ms,
                    ran: f64::from(phase),
                })
            }
        };
        DeckLive {
            flow,
            beat: beat_phase(k).filter(|_| self.beat),
            blend: tween_phase(k).filter(|_| self.tween),
            needle: None,
            // The steam moves on the beat's frames, the reading's and the next BEAT_FRAMES, and
            // holds after them: it asks no frame of its own.
            steam: self
                .steam
                .then(|| steam_ms(landed + FRAME.saturating_mul(k.min(BEAT_FRAMES)))),
            fresh: true,
        }
    }

    /// The first moment after `now` at which the grid's part of [`Self::live`] changes, or
    /// `None` when it never will without a new reading or a change of window: the grey's moment,
    /// or the first frame of the grid that draws something new. The needle's frames are
    /// [`Self::next_needle`]'s.
    pub(crate) fn next_change(&self, now: Instant, now_ms: i64) -> Option<Instant> {
        let landed = self.landed?;
        let stale = landed + self.stale_after;
        if stale <= now {
            return None;
        }
        let mut next = stale;
        if self.motion {
            let was = self.grid(now, now_ms);
            let (k, _) = Self::frame_of(landed, now);
            // The grid up to the grey: at most stale_after / FRAME frames (12 at a 1 s cadence).
            for j in (k.saturating_add(1)..).take(1 << 12) {
                let at = landed + FRAME.saturating_mul(j);
                if at >= next {
                    break;
                }
                let ahead = i64::try_from(at.saturating_duration_since(now).as_millis())
                    .unwrap_or(i64::MAX);
                if self.grid(at, now_ms.saturating_add(ahead)) != was {
                    next = at;
                    break;
                }
            }
        }
        Some(next.max(now + Duration::from_millis(1)))
    }

    /// The needle's next frame after `now` ([`Needle::next_after`]): the first moment its tip
    /// stands a whole device pixel (or its soak a whole step) from where this window last drew
    /// it, never sooner than the window's effect present interval after that frame; `None` once
    /// it rests on the reading, without motion, once readings stop (the grey draws it at rest),
    /// or for the rest of a sweep this window drew at rest ([`Self::needle_at`]).
    pub(crate) fn next_needle(&self, now: Instant) -> Option<Instant> {
        let stale = self.landed? + self.stale_after;
        if stale <= now || !self.motion {
            return None;
        }
        let (n, view) = self.needle?;
        n.next_after(view, self.needle_at(now)?.frame?)
            .filter(|&at| at < stale)
            .map(|at| at.max(now + Duration::from_millis(1)))
    }
}

/// The steam's clock at `at`, whole ms floored: one monotonic count for every window, from a
/// moment fixed when the process first asks (negative before it), so a puff's rise runs on across
/// readings and two moments a whole number of ms apart read exactly that far apart.
pub(crate) fn steam_ms(at: Instant) -> i64 {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let origin = *ORIGIN.get_or_init(Instant::now);
    let ns = |d: Duration| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX);
    let signed = at
        .checked_duration_since(origin)
        .map_or_else(|| -ns(origin.saturating_duration_since(at)), ns);
    let ms = signed.div_euclid(1_000_000);
    i64::try_from(ms).unwrap_or(if ms < 0 { i64::MIN } else { i64::MAX })
}

/// The wall clock, unix ms; 0 where unreadable (the glide then stands at the reading's own
/// moment: [`glide_frac`] counts a clock behind the reading as the reading's).
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
        let steam = alone
            && self.serious_mode_policy().allows(SeriousEffect::DeckSteam)
            && ws
                .deck_live_rects
                .try_borrow()
                .is_ok_and(|rects| rects.iter().any(|&(layer, _)| layer == LiveLayer::Steam));
        let bands = ws.deck_glide.get();
        let snap = self.vitals.snapshot.as_ref();
        let flow = snap
            .zip(self.vitals.landed_open)
            .map_or(Flow::None, |(snap, opened)| {
                Flow::of(bands, opened, snap.slot_grid)
            });
        let px_per_deg = ws.deck_needle_px.get();
        let needle = self
            .vitals
            .needle
            .get()
            .filter(|_| px_per_deg > 0.0)
            .map(|n| {
                let view = View {
                    px_per_deg,
                    interval: ws.effect_present_interval(),
                };
                (n, view)
            });
        Some(LiveFacts {
            landed: snap.and(self.vitals.landed_at),
            stale_after: stale_after(period, self.vitals.landed_period),
            motion,
            beat,
            tween: snap.is_some() && self.vitals.prev_snapshot.is_some(),
            flow,
            needle,
            drawn: ws.deck_needle_drawn.get(),
            steam,
        })
    }

    /// [`Self::deck_live_now`]'s live edge alone, as the tests read it.
    #[cfg(all(test, unix))]
    pub(crate) fn deck_live(&self, wid: crate::WindowId) -> DeckLive {
        self.deck_live_now(wid).0
    }

    /// What window `wid`'s deck draws of its live edge now ([`DeckLive::STILL`] for a window
    /// that draws no v2 deck on screen: its card is not presented, or another painter draws it),
    /// and what it draws of the needle ([`LiveFacts::needle_at`]), which the splice keeps as what
    /// the window drew: at rest on the reading where the deck draws no live edge (an occluded
    /// window's draws the reading), `None` with no needle or no dial.
    pub(crate) fn deck_live_now(&self, wid: crate::WindowId) -> (DeckLive, Option<Drawn>) {
        let now = Instant::now();
        self.live_facts(wid).map_or_else(
            || {
                (
                    DeckLive::STILL,
                    self.vitals.needle.get().map(|n| n.rested()),
                )
            },
            |facts| (facts.live(now, now_unix_ms()), facts.needle_at(now)),
        )
    }

    /// The loop's deadline for the next change of any window's live edge on the grid.
    pub(crate) fn stats_live_deadline(&self, now: Instant) -> Option<Instant> {
        let now_ms = now_unix_ms();
        self.windows
            .keys()
            .filter_map(|&wid| self.live_facts(wid)?.next_change(now, now_ms))
            .min()
    }

    /// The loop's deadline for any window's next needle frame ([`LiveFacts::next_needle`]), kept
    /// apart from the grid's (`DeadlineOwner::StatsNeedle`).
    pub(crate) fn stats_needle_deadline(&self, now: Instant) -> Option<Instant> {
        self.windows
            .keys()
            .filter_map(|&wid| self.live_facts(wid)?.next_needle(now))
            .min()
    }

    /// Retargets the dial's needle at the snapshot just adopted at `now`
    /// ([`Needle::retarget`]), against the finest tip of the windows that animate a dial: with
    /// none it rests on the reading at once. Returns the frames the new episode owes those
    /// windows ([`Needle::frames_owed`], each from what it last drew, on the shortest effect
    /// interval its display gives it, so a lane that unparks mid-episode is owed its frames),
    /// which `ctl metrics` counts (`needle_sweeps`, `needle_frames_owed`, `needle_episode_left`).
    pub(crate) fn retarget_needle(&self, now: Instant) -> u64 {
        let views: Vec<(View, Option<Drawn>)> = self
            .windows
            .iter()
            .filter_map(|(&wid, ws)| {
                let facts = self.live_facts(wid).filter(|f| f.motion)?;
                let view = View {
                    interval: ws.effect_present_interval_fastest(),
                    ..facts.needle?.1
                };
                Some((view, facts.drawn))
            })
            .collect();
        let px_per_deg = views
            .iter()
            .map(|(view, _)| view.px_per_deg)
            .fold(0.0, f64::max);
        let target = self.vitals.snapshot.as_deref().and_then(|snap| {
            crate::stats_gauge::Gauge::of(&aterm_vitals::Strain::of(snap)).target()
        });
        let was = self.vitals.needle.get();
        let next = Needle::retarget(was, now, target, px_per_deg);
        let owed = next.map_or(0, |n| {
            views
                .iter()
                .map(|(view, drawn)| n.frames_owed(*view, drawn.map(|d| d.at)))
                .sum()
        });
        crate::metrics::note_needle_episode(owed);
        self.vitals.needle.set(next);
        owed
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

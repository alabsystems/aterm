// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow kitty ribbon geometry, physics, and instanced quad generation.
use std::mem;

use aterm_time::Instant;

use aterm_render::{
    BeamClip, BeamVertex, GlowBlend, GlowQuad, RibbonVertex, comet_beam, over_premul, premul_rgb,
    ribbon_beam,
};

use crate::color_math::{hsv2rgb, oklab, relative_luminance, rgb2hsv};
use crate::cursor_glow::{InkRole, band_pos, band_row};
use crate::effect_util::lerp_rgb;
use crate::spectrum::{spectrum, spectrum_with_min_saturation};

use super::companion::KeepOut;
use super::meteor::tri;
use super::spine::{DISP_RELEASE_TAU, PHASE_RATE};
use super::stardust::GlyphProbe;
use super::timing::{
    EDGE_IN_S, FLIGHT_MIN_MS, IOI_DEFAULT_MS, JUMP_MIN_CELLS, REDUCED_MOTION_FADE_MS, clamp01,
    edge_in, flight_ms, ioi_next, kill_span_s, phrase_rest_ms, smoothstep01, spend, suck_in,
};
use super::{Cadence, Config, Ctx, Event, Frame, Licence, TypedClass, level_step_spend};

/// Quads the ribbon may spend on one frame (RAINBOW-KITTY-V2.md §18: a 76-cell hot line at retina).
pub const RIBBON_QUAD_BUDGET: usize = 10_240;

/// Cells the hot edge reaches back from the head (§4.1).
pub const HOT_EDGE_CELLS: f32 = 3.0;

/// The hot edge's line thickness, px.
pub const HOT_EDGE_THICKNESS: f32 = 1.0;

/// How far back from its head end a run the hand has left feathers in, in
/// cells ([`Ribbon::head_feather`]): the body, its rail and its hot edge thin
/// to nothing at the end the caret jumped off instead of standing square-cut
/// there, at the end and along the top edge beside it. Wide enough that its
/// pixel-fine boundaries step by no more than a fifth of the body a column,
/// narrow enough that the band an Enter leaves goes dark with a submit's.
pub const HEAD_FEATHER_CELLS: f32 = 1.2;

/// The furthest back, in cells, the crisp edge may reach when a key is born on a change of speed or under an open theme ([`edge_cells`]): the same alpha law over a longer `d`, so a longer streak, never a brighter one.
pub const HOT_EDGE_CELLS_MAX: f32 = 5.0;

/// The hot edge's falloff shape, normalised at the head cell.
pub const HOT_EDGE_ALPHA: f32 = 0.38;

/// The hot edge's coverage request ceiling.
pub const HOT_EDGE_COV_MAX: f32 = 118.0;

/// The relative luminance the hot edge's ink is lifted to when its stop is darker.
pub const HOT_EDGE_LUMA_FLOOR: f32 = 2.0 * BED_LUMA_MAX;

/// The spine value from which the hot edge's momentum gain starts rising (over [`HOT_EDGE_DISP_SPAN`]).
pub const HOT_EDGE_DISP_MIN: f32 = 0.15;

/// Width of the smoothstep that fades the hot edge in over [`HOT_EDGE_DISP_MIN`].
pub const HOT_EDGE_DISP_SPAN: f32 = 0.30;

/// The share of the hot edge's gain a cold hand gets (a word's first key, momentum zero).
pub const HOT_EDGE_GAIN_FLOOR: f32 = 0.5;

/// How far, in `ch`, the hot edge's hairline may sit from the caret's cell band before it becomes a highlighter on another row.
#[cfg(test)]
pub const HOT_EDGE_CARET_REACH_CH: f32 = 0.55;

/// Cells of the classic walk's first phase, which advances `d/16` per cell so a short word already shows several stops.
pub const WALK_FAST_CELLS: f32 = 16.0;

/// The classic walk's steady lay rate, `t`-units per cell, after [`WALK_FAST_CELLS`].
pub const WALK_LAY_RATE: f32 = 1.0 / 36.0;

/// The birth floor.
pub const BIRTH_EDGE_FLOOR: f32 = 0.35;

/// The tall body's reach above the spine, in `ch`.
pub const TALL_UP_CH: f32 = 1.10;

/// The tall body's full-strength plateau above the spine, in `ch`.
pub const TALL_CORE_CH: f32 = 0.40;

/// The downward melt's floor, in `ch`.
pub const DN_FLOOR_CH: f32 = 0.25;

/// The downward reach at full momentum, in `ch`.
pub const DN_TOP_CH: f32 = DN_FLOOR_CH + WAVE_AMP_CELLS;

/// The underline spelling's reach above the spine, in `ch`.
pub const UNDERLINE_UP_CH: f32 = 1.0 - DN_TOP_CH;

/// The leading's half-thickness in `ch`.
pub const LEAD_CH: f32 = 0.11;

/// The tall highlighter's shoulder.
pub const SHOULDER_TALL: f32 = 1.0;

/// The underline spelling's quieter glyph-band wash.
pub const SHOULDER_UNDERLINE: f32 = 0.55;

/// The 0.75-cycle wave's amplitude in cells at full spine (about 2 px at `ch 18`).
pub const WAVE_AMP_CELLS: f32 = 0.110;

/// Cycles of the wave across the whole mark, whatever its length.
pub const WAVE_CYCLES: f32 = 0.75;

/// The share of the mark over which the wave's amplitude smoothsteps up from the head.
pub const WAVE_HEAD_PIN: f32 = 0.35;

/// Cells over which the body's downward wedge closes behind the head.
pub const BLOOM_REACH_CELLS: f32 = 8.0;

/// Vertices per cell along the major axis, at most.
pub const SLABS_PER_CELL: usize = 3;

const STRIP_LIFT_GAIN: f32 = 0.35;

/// The comet's reach below the spine at the hand, in `ch`, at full momentum.
pub const COMET_DN_HAND_CH: f32 = 0.55;

/// The tall body's reach above the spine at the comet's tail, in `ch`.
pub const COMET_UP_TAIL_CH: f32 = 0.80;

/// Cells behind the head over which the comet's `up` tapers from [`TALL_UP_CH`] to [`COMET_UP_TAIL_CH`] (smoothstep).
pub const COMET_TAPER_CELLS: f32 = 12.0;

/// The flat ribbon's reach above its spine on paper, as a share of the pet's body's.
pub const FLAT_PAPER_UP_SHARE: f32 = 0.72;

/// The flat ribbon's full-strength plateau on paper, as a share of its reach.
pub const FLAT_PAPER_CORE_SHARE: f32 = 0.85;

/// The rail's ink luminance floor.
pub const RAIL_LUMA_FLOOR: f32 = 0.20;

/// The rail's luminance ceiling.
pub const RAIL_LUMA_CEIL: f32 = 0.54;

const _: () = assert!(
    RAIL_LUMA_FLOOR < RAIL_LUMA_CEIL,
    "the rail's floor must sit under its ceiling"
);

/// The rail's coverage as a share of the body's at the same slab.
pub const RAIL_GAIN: f32 = 1.0;

/// The rail's full-strength plateau as a share of its reach.
pub const RAIL_CORE_SHARE: f32 = aterm_render::RIBBON_CORE_SHARE;

/// The from-the-hand attack's duration, seconds.
pub const ATTACK_WIPE_S: f32 = 0.040;

/// The wipe front's softness, in cells.
pub const ATTACK_WIPE_FRONT: f32 = 0.35;

/// How far below the row bottom the vivid rail may reach, in cells.
pub const RAIL_REACH_MAX_CH: f32 = DN_TOP_CH;

/// The hairline of body the vivid rail leaves under the row, in cells
/// ([`rail_gap_px`]): a descender's foot ends on the row's last pixel row,
/// and a rail laid at the row's bottom would meet it and leave the foot
/// little contrast against the rail's vivid ink.
pub const RAIL_GAP_CH: f32 = 0.06;

/// [`RAIL_GAP_CH`] in whole device pixels at cell height `ch`, at least one.
#[inline]
#[must_use]
pub const fn rail_gap_px(ch: f32) -> f32 {
    (RAIL_GAP_CH * ch).round().max(1.0)
}

const _: () = assert!(RAIL_REACH_MAX_CH < aterm_render::SUB_CEILING);

/// The bed's byte ceiling.
pub const UNDER_COV_CAP: f32 = 236.0;

/// The coverage at which a planned boundary counts as on the glass for `trail status` ([`Ribbon::lit_segments`]).
pub const STATUS_LIT_COV: u8 = (UNDER_COV_CAP / 2.0) as u8;

/// The paint scanner's colour floor.
pub const SCAN_INK_MAX: u32 = 60;

/// …and the spread half of it — see [`SCAN_INK_MAX`].
pub const SCAN_INK_SPREAD: u32 = 40;

/// Whether a composited `0x00RRGGBB` pixel reads as band ink to the glass census ([`SCAN_INK_MAX`] / [`SCAN_INK_SPREAD`]).
#[inline]
#[must_use]
pub const fn reads_as_ink(rgb: u32) -> bool {
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let mx = if r > g { r } else { g };
    let mx = if mx > b { mx } else { b };
    let mn = if r < g { r } else { g };
    let mn = if mn < b { mn } else { b };
    mx >= SCAN_INK_MAX && mx - mn >= SCAN_INK_SPREAD
}

const BODY_FRAME_TOP: f32 = 251.0;

use crate::cursor_glow::{BODY_CONTRAST_BAR, BODY_CONTRAST_GUARD};

const BED_LUMA_MIN: f32 = 0.020;

const BED_LUMA_MAX: f32 = 0.150;

/// The bed's chroma floor.
pub const BED_SAT_FLOOR: f32 = 1.0;

/// A dark theme whose text reads under this on its page is dim: its bed is
/// held at its edge too ([`dim_edge_held`]).
pub const DIM_THEME_BARE: f32 = 7.0;

/// Where the bed's top crosses a letter, the letter's anti-aliased pixels sit
/// on page above and bed below, and a reader of that edge sees each channel's
/// larger of the two. On a dim theme the bed is held so that this mix keeps a
/// letter's edge pixel ([`DIM_EDGE_SOLID`] of the text's reading) at
/// [`PAPER_BAR_THEME_SHARE`] of the theme's own, with this margin.
pub const DIM_EDGE_GUARD: f32 = 0.03;

/// The share of the text's reading a letter's weakest solid pixel keeps.
pub const DIM_EDGE_SOLID: f32 = 0.85;

/// The light bed's one weight.
pub const PAPER_WEIGHT: f32 = 0.24;

/// The light bed's bar on a theme whose own text sits under the body's (Solarized Light's `#586e75` on `#fdf6e3` is 4.97:1).
pub const PAPER_BAR_THEME_SHARE: f32 = 0.8;

/// The leading-only body's weight (the underline spelling, [`InkRole::Leading`]).
pub const PAPER_WEIGHT_LEADING: f32 = 0.30;

/// The least contrast the caret block keeps against a light page
/// ([`paper_caret_ink`]): the caret law's 4.5:1, as every effect-owned
/// caret's ([`crate::cursor_fx::CARET_PAGE_MIN_CONTRAST`]). Held to a
/// graphic's 3:1, a hot burst's green and orange stops would print their
/// carets under 4.5:1 on white.
pub const PAPER_CARET_CONTRAST: f32 = crate::cursor_fx::CARET_PAGE_MIN_CONTRAST;

/// The least contrast a point mark keeps against a light page ([`paper_mark_ink`]).
pub const PAPER_MARK_CONTRAST: f32 = 4.5;

const PAPER_HUE_BEND: [(f32, f32); 4] = [(30.0, 30.0), (72.0, 40.0), (90.0, 85.0), (110.0, 110.0)];

/// Entries in the bed-ink lookup table.
pub const BED_INK_LUT_LEN: usize = 129;

/// Share of the ceiling a cold key's cell takes (momentum buys the rest, [`Cell::cov0`]), so a single keystroke lays a readable band.
pub const BODY_COLD_SHARE: f32 = 0.88;

/// The host's duration knob, as a multiplier on the base cell life.
pub const LIFE_DURATION_GAIN: f32 = 1.2;

/// Floor of the base cell life, seconds.
pub const LIFE_BASE_MIN: f32 = 0.26;

/// Ceiling of the base cell life, seconds.
pub const LIFE_BASE_MAX: f32 = 0.55;

/// Linear momentum term of the cell life.
pub const LIFE_SPINE_LINEAR: f32 = 2.6;

/// …and the superlinear term, so an earned hot run drags a rainbow across many cells and wrapped lines instead of merely a longer wake.
pub const LIFE_SPINE_SQUARE: f32 = 11.0;

/// Four-letter guarantee.
pub const CHAIN_KEYS: f32 = 4.5;

/// The ribbon chains across any plausible rhythm, even one key every five seconds.
pub const CHAIN_GAP_MAX: f32 = 5.0;

/// Fade margin added to the chained life, seconds.
pub const CHAIN_MARGIN: f32 = 0.10;

/// …capped just above `CHAIN_KEYS × CHAIN_GAP_MAX` so the slowest chained rhythm still earns its full four-letter span.
pub const CHAIN_LIFE_MAX: f32 = 23.0;

/// New light a jump's corridor may lay, in cells back from the landing, so a Ctrl-A across 200 columns does not paint the whole row.
pub const WAKE_MAX_CELLS: u16 = 32;

/// A wake's life.
pub const WAKE_LIFE_S: f32 = 1.10;

/// A same-row hop under [`JUMP_MIN_CELLS`] (an arrow key) lays its wake at this share of [`WAKE_LIFE_S`].
pub const WAKE_HOP_LIFE_SHARE: f32 = 0.5;

/// Cells the four-letter guarantee reaches for.
pub const FOUR_LETTER_CELLS: u16 = 4;

/// The phrase rest's floor, seconds.
pub const PHRASE_REST_MIN_S: f32 = super::timing::PHRASE_PAUSE_MS as f32 / 1000.0;

/// One extension cell lights per this step while the swoosh finishes reaching four letters.
pub const REACH_STEP_S: f32 = 0.05;

/// Beats of [`REACH_STEP_S`] the reach spends.
pub const REACH_BEATS: u16 = 3;

/// Cells the reach may stage in one tick, across every live cohort.
pub const REACH_STAGE: usize = 8;

/// The retract drains the ribbon tail->head over this long, whatever its length.
pub const RETRACT_DUR_S: f32 = 0.40;

/// How long a retracted cell takes to go out once the drain selects it.
pub const RETRACT_FADE_S: f32 = 0.24;

/// The fold flow's whole life.
pub const FLOW_TOTAL_S: f32 = 1.50;
const _: () = assert!(FLOW_TOTAL_S < SWOOSH_TOTAL_S);

/// The flow's far-first lead.
pub const FLOW_LEAD_S: f32 = RETRACT_DUR_S;

/// The flow's last breath.
pub const FLOW_TAIL_S: f32 = RETRACT_FADE_S;

/// The drift of the coldest hand, cells.
pub const FLOW_DRIFT_MIN_CELLS: f32 = 2.0;

/// The drift of the hottest hand, cells.
pub const FLOW_DRIFT_MAX_CELLS: f32 = FLOW_TOTAL_S / (4.0 * REACH_STEP_S);

/// The drift's cap as a share of the band's kept length.
pub const FLOW_DRIFT_MAX_SHARE: f32 = 0.40;

/// The settle.
pub const FLOW_SETTLE_SHARE: f32 = 0.40;

/// The seam's level.
pub const SEAM_LEVEL: f32 = (1.0 - 1.0 / HOT_EDGE_CELLS) * (1.0 - 1.0 / HOT_EDGE_CELLS);
/// The re-anchor's floor, cells, matching the `raw_dist > 2` floor of
/// `cursor_glow`'s `classify_move`; a shorter backward hop re-anchors only on
/// a proof (`short_reanchor`, `rewrap_start`).
pub const RE_ANCHOR_MIN_CELLS: u16 = 3;

/// The exit swoosh past the grace.
pub const SWOOSH_AFTER_GRACE_S: f32 =
    REACH_STEP_S * REACH_BEATS as f32 + RETRACT_DUR_S + RETRACT_FADE_S;

/// The whole exit swoosh at the floor grace.
pub const SWOOSH_TOTAL_S: f32 = PHRASE_REST_MIN_S + SWOOSH_AFTER_GRACE_S;

/// The margin a cell's life keeps past its swoosh, so the swoosh always ends the ribbon before the natural melt can.
pub const SWOOSH_LIFE_MARGIN_S: f32 = 0.01;

/// The life floor at the floor grace, 1.70 s.
pub const SWOOSH_LIFE_S: f32 = SWOOSH_TOTAL_S + SWOOSH_LIFE_MARGIN_S;

/// The swoosh's retract offset at the floor grace.
pub const RETRACT_START_S: f32 = retract_start_s(PHRASE_REST_MIN_S);

/// The idle at which a cohort of grace `grace_s` starts its retract.
#[inline]
#[must_use]
pub const fn retract_start_s(grace_s: f32) -> f32 {
    grace_s + REACH_STEP_S * REACH_BEATS as f32
}

/// The whole swoosh of a cohort of grace `grace_s`.
#[inline]
#[must_use]
pub const fn swoosh_total_s(grace_s: f32) -> f32 {
    grace_s + SWOOSH_AFTER_GRACE_S
}

/// The life floor of a cell whose phrase rests `grace_s`.
#[inline]
#[must_use]
pub const fn swoosh_life_s(grace_s: f32) -> f32 {
    swoosh_total_s(grace_s) + SWOOSH_LIFE_MARGIN_S
}

/// Focus loss embers the ribbon out over this long on `spend` (§8.2).
pub const FOCUS_EMBER_S: f32 = 0.30;

/// A stop is on the glass while its composite is a colour the eye can pick from the ground.
pub const SEEN_SPREAD: f32 = 40.0;

/// The level half of [`SEEN_SPREAD`].
pub const SEEN_CONTRAST: f32 = 1.90;

/// The ember's per-stop floor, in that stop's extinction units.
pub const EMBER_FLOOR_MARGIN: f32 = 1.05;

/// The curtain.
pub const CURTAIN_S: f32 = RETRACT_FADE_S;

/// The curtain's stagger.
pub const CURTAIN_STAGGER_S: f32 = CURTAIN_S * (RETRACT_DUR_S / (RETRACT_DUR_S + RETRACT_FADE_S));

/// What each cell of a falling curtain spends over once its [`CURTAIN_STAGGER_S`] turn has come.
pub const CURTAIN_SPEND_S: f32 = CURTAIN_S - CURTAIN_STAGGER_S;

/// The curtain's cliff bound.
#[cfg(test)]
pub const CURTAIN_MAX_STEP_FRAC: f32 = 0.34;

/// The other half of the law.
#[cfg(test)]
pub const CURTAIN_MIN_FRAMES: usize = 5;

/// The content retirement's melt.
pub const RETIRE_MELT_S: f32 = 0.12;

/// The share of its light a line keeps once a Return has left it printed ([`Cohort::printed`]).
pub const RETURN_PRINTED_SHARE: f32 = 0.3;

/// How long the dim to [`RETURN_PRINTED_SHARE`] takes after the Return.
pub const RETURN_DIM_S: f32 = 0.06;

/// The share of a cell's life over which the expiry melt runs; identity over the first 70 %.
pub const EXPIRY_MELT_SHARE: f32 = 0.30;

/// The expiry melt's exponent.
pub const EXPIRY_MELT_GAMMA: f32 = 1.6;

/// The coverage share the run's outer tail boundary keeps, ramping to full across the tail cell, so the oldest edge feathers out.
pub const RUN_TAIL_EASE: f32 = 0.10;

/// The instant a cell's expiry is measured from.
#[inline]
#[must_use]
pub fn live_since(cohorts: &[Cohort], cell: &Cell) -> Instant {
    cohorts
        .iter()
        .find(|c| c.id == cell.cohort)
        .map_or(cell.born, |c| cell.born.max(c.alive_at))
}

#[inline]
fn freshest<'a>(cohorts: impl Iterator<Item = &'a Cohort>) -> Option<&'a Cohort> {
    cohorts.max_by(|a, b| {
        a.alive_at
            .cmp(&b.alive_at)
            .then_with(|| a.walk_d(a.col1).total_cmp(&b.walk_d(b.col1)))
    })
}

/// Which layer a cell is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    #[default]
    Base,
    Over,
}

/// One laid ribbon cell, the atom of the field.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub row: u16,
    pub col: u16,
    pub cohort: u32,
    pub t: f32,
    pub born: Instant,
    pub attack_at: Instant,
    pub life_s: f32,
    pub cov0: f32,
    pub typing: bool,
    pub retract_at: Option<Instant>,
    pub retire_at: Option<Instant>,
    pub birth_disp: f32,
    pub edge_cells: f32,
    pub layer: Layer,
    pub rearm: Option<(Instant, f32)>,
    pub released_at: Option<Instant>,
}

impl Cell {
    /// True once the cell is on its way out (retracting after a Backspace or kill, or retired by content) and so no longer owns its column.
    #[inline]
    #[must_use]
    pub const fn leaving(&self) -> bool {
        self.retract_at.is_some() || self.retire_at.is_some()
    }
}

/// A cohort: one contiguous run of cells laid by one typing burst on one row, sharing a hue walk.
#[derive(Clone, Copy, Debug)]
pub struct Cohort {
    pub id: u32,
    pub row: u16,
    pub col0: u16,
    pub col1: u16,
    pub anchor_col: u16,
    pub t0: f32,
    pub d0: f32,
    pub born: Instant,
    pub alive_at: Instant,
    pub abandoned: bool,
    pub wake: bool,
    pub retract_col: Option<u16>,
    pub drain_right_first: bool,
    pub phase: Phase,
    pub rejoinable: bool,
    pub phrase: u32,
    pub grace_s: f32,
    pub flow: Option<Instant>,
    pub flow_drift: f32,
    release_clock: Option<ReleaseClock>,
    release_parts: [Option<ReleasePart>; RELEASE_PARTS],
    parts_revoked_at: Option<Instant>,
    line_killed: bool,
    printed: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct ReleaseClock {
    alive_at: Instant,
    retract_col: Option<u16>,
    phase: Phase,
    rejoinable: bool,
    cells: usize,
}

/// Partial releases one cohort holds in custody at once (`Cohort::release_parts`).
pub const RELEASE_PARTS: usize = 4;

#[derive(Clone, Copy, Debug)]
struct ReleasePart {
    stamp: Instant,
    cells: usize,
}

impl Cohort {
    #[inline]
    fn revoke_releases(&mut self, at: Instant) {
        self.release_clock = None;
        self.parts_revoked_at = Some(self.parts_revoked_at.map_or(at, |t| t.max(at)));
    }

    #[inline]
    fn hold_part(&mut self, part: ReleasePart) {
        let mut oldest: Option<(usize, Instant)> = None;
        for (i, slot) in self.release_parts.iter_mut().enumerate() {
            match slot {
                None => {
                    *slot = Some(part);
                    return;
                }
                Some(p) => {
                    if oldest.is_none_or(|(_, t)| p.stamp < t) {
                        oldest = Some((i, p.stamp));
                    }
                }
            }
        }
        if let Some((i, _)) = oldest {
            self.release_parts[i] = Some(part);
        }
    }

    #[inline]
    fn part_mut(&mut self, stamp: Instant) -> Option<&mut ReleasePart> {
        self.release_parts
            .iter_mut()
            .flatten()
            .find(|p| p.stamp == stamp)
    }

    #[inline]
    fn holds_partial(&self, cell: &Cell) -> bool {
        !self.abandoned
            && cell.released_at.is_some_and(|stamp| {
                self.release_parts
                    .iter()
                    .flatten()
                    .any(|part| part.stamp == stamp)
            })
    }

    /// The band is leaving at `at`.
    #[inline]
    #[must_use]
    pub fn leaving_at(&self, at: Instant) -> bool {
        self.abandoned
            || self
                .phase_at(self.idle_s(at))
                .is_none_or(Phase::is_retracting)
    }

    /// The idle at which this cohort's retract begins.
    #[inline]
    #[must_use]
    pub const fn retract_start(&self) -> f32 {
        if self.flow.is_some() {
            0.0
        } else {
            retract_start_s(self.grace_s)
        }
    }

    /// How long this cohort's retract's farthest-first stagger spans.
    #[inline]
    #[must_use]
    pub const fn retract_span(&self) -> f32 {
        if self.flow.is_some() {
            FLOW_LEAD_S
        } else {
            RETRACT_DUR_S
        }
    }

    /// The drift's progress, `0..1` at `idle` seconds into the flow.
    #[inline]
    #[must_use]
    pub const fn flow_progress(&self, idle: f32) -> f32 {
        1.0 - spend(clamp01(idle / FLOW_TOTAL_S))
    }

    /// The flow's fade at a column, `1 -> 0`.
    #[inline]
    #[must_use]
    pub const fn flow_drain(&self, col: u16, idle: f32) -> f32 {
        let s = FLOW_LEAD_S * (1.0 - self.drain_rank(col));
        spend(clamp01((idle - s) / (FLOW_TOTAL_S - s)))
    }

    /// The idle at which this cohort's swoosh — or its fold flow — is over.
    #[inline]
    #[must_use]
    pub const fn swoosh_total(&self) -> f32 {
        if self.flow.is_some() {
            FLOW_TOTAL_S
        } else {
            swoosh_total_s(self.grace_s)
        }
    }

    /// The phase this cohort is in `idle_s` after its last live key.
    #[inline]
    #[must_use]
    pub const fn phase_at(&self, idle_s: f32) -> Option<Phase> {
        if self.flow.is_none() {
            return Phase::at(idle_s, self.grace_s);
        }
        if idle_s.is_nan() || idle_s <= 0.0 {
            Some(Phase::Laying)
        } else if idle_s < FLOW_TOTAL_S {
            Some(Phase::Retracting)
        } else {
            None
        }
    }

    /// The clock the exit runs from.
    #[inline]
    #[must_use]
    pub fn clock(&self) -> Instant {
        self.flow.map_or(self.alive_at, |f| self.alive_at.max(f))
    }

    /// Seconds of idle at `now` on [`Cohort::clock`].
    #[inline]
    #[must_use]
    pub fn idle_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.clock()).as_secs_f32()
    }

    /// The drain rank of a column, `0..=1`.
    #[inline]
    #[must_use]
    pub const fn drain_rank(&self, col: u16) -> f32 {
        let span = (self.col1.saturating_sub(self.col0) as f32).max(1.0);
        if self.drain_right_first {
            col.saturating_sub(self.col0) as f32 / span
        } else {
            self.col1.saturating_sub(1).saturating_sub(col) as f32 / span
        }
    }

    /// The walk's distance at a column.
    #[inline]
    #[must_use]
    pub const fn walk_d(&self, col: u16) -> f32 {
        self.d0 + (col as f32 - self.anchor_col as f32)
    }

    /// The walk's zero.
    #[inline]
    #[must_use]
    pub const fn walk_zero(&self) -> f32 {
        self.t0 - walk_t(self.d0)
    }

    /// The field at a column of this cohort.
    #[inline]
    #[must_use]
    pub const fn t_at(&self, col: u16) -> f32 {
        self.walk_zero() + walk_t(self.walk_d(col))
    }

    #[inline]
    #[must_use]
    const fn walk_at(&self, col: u16) -> WalkPos {
        WalkPos {
            t: self.t_at(col),
            d: self.walk_d(col),
        }
    }

    #[inline]
    #[must_use]
    const fn origin_walk(&self) -> WalkPos {
        WalkPos {
            t: self.t0,
            d: self.d0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WalkPos {
    t: f32,
    d: f32,
}

impl Default for WalkPos {
    #[inline]
    fn default() -> Self {
        Self::COLD
    }
}

impl WalkPos {
    const COLD: Self = Self { t: 0.0, d: 0.0 };

    #[inline]
    const fn restarted(self) -> Self {
        Self { t: self.t, d: 0.0 }
    }
}

/// A cohort's place in the exit swoosh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Laying,
    Grace,
    Reaching,
    Retracting,
    Fading,
}

impl Phase {
    /// The phase a cohort of grace `grace_s` ([`Cohort::grace_s`]) is in `idle_s` seconds after its last live key.
    #[inline]
    #[must_use]
    pub const fn at(idle_s: f32, grace_s: f32) -> Option<Self> {
        let grace = if grace_s.is_finite() {
            grace_s.max(PHRASE_REST_MIN_S)
        } else {
            PHRASE_REST_MIN_S
        };
        let reach_end = retract_start_s(grace);
        let retract_end = reach_end + RETRACT_DUR_S;
        if idle_s.is_nan() || idle_s <= 0.0 {
            // NaN-refusing spelling: a NaN idle takes the laying arm too, which draws the mark rather than dropping it.
            return Some(Self::Laying);
        }
        if idle_s < grace {
            Some(Self::Grace)
        } else if idle_s < reach_end {
            Some(Self::Reaching)
        } else if idle_s < retract_end {
            Some(Self::Retracting)
        } else if idle_s < swoosh_total_s(grace) {
            Some(Self::Fading)
        } else {
            None
        }
    }

    /// True once the swoosh owns the mark and the retract is moving it.
    #[inline]
    #[must_use]
    pub const fn is_retracting(self) -> bool {
        matches!(self, Self::Retracting | Self::Fading)
    }
}

/// One planned sample of a cohort's spine at a cell boundary, what `aterm_render::RibbonVertex` is built from.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub x: f32,
    pub spine: f32,
    pub up: f32,
    pub dn: f32,
    pub t: f32,
    pub cov: u8,
}

/// The vertical extent of the ribbon's body at one cell, `[spine - up, spine + dn]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub top: f32,
    pub spine: f32,
    pub bottom: f32,
}

impl Band {
    /// True when `y` lies inside the body.
    #[must_use]
    #[cfg(test)]
    pub const fn contains(&self, y: f32) -> bool {
        self.top <= y && y <= self.bottom
    }
}

/// The two profiles, one emitter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyProfile {
    pub up_ch: f32,
    pub dn_ch: f32,
    pub core_up_ch: f32,
    pub shoulder: f32,
}

#[derive(Clone, Debug)]
struct FieldLane {
    row: u16,
    t: Vec<f32>,
    touched: Vec<u16>,
}

impl Default for FieldLane {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl FieldLane {
    const EMPTY: Self = Self {
        row: 0,
        t: Vec::new(),
        touched: Vec::new(),
    };

    #[inline]
    fn clear(&mut self) {
        for &col in &self.touched {
            if let Some(slot) = self.t.get_mut(usize::from(col)) {
                *slot = f32::NAN;
            }
        }
        self.touched.clear();
    }

    #[inline]
    fn resize(&mut self, cols: u16) {
        self.t.clear();
        self.t.resize(usize::from(cols), f32::NAN);
        self.touched.clear();
    }
}

/// The field index.
#[derive(Clone, Debug)]
pub struct FieldIndex {
    lanes: Vec<FieldLane>,
    spare: Vec<FieldLane>,
    cols: u16,
}

impl Default for FieldIndex {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl FieldIndex {
    /// Empty field index.
    pub const EMPTY: Self = Self {
        lanes: Vec::new(),
        spare: Vec::new(),
        cols: 0,
    };

    /// The field at a cell, or `None` where no ribbon light is laid.
    #[inline]
    #[must_use]
    pub const fn at(&self, row: u16, col: u16) -> Option<f32> {
        let lanes = self.lanes.as_slice();
        let mut i = lanes.len();
        let mut found = None;
        while i > 0 {
            i -= 1;
            if lanes[i].row == row {
                found = Some(&lanes[i]);
                break;
            }
        }
        let Some(lane) = found else {
            return None;
        };
        let ts = lane.t.as_slice();
        let idx = col as usize;
        if idx >= ts.len() {
            return None;
        }
        let t = ts[idx];
        if t.is_nan() { None } else { Some(t) }
    }

    /// Rows carrying field light this frame.
    #[must_use]
    #[cfg(test)]
    pub const fn live_rows(&self) -> usize {
        self.lanes.len()
    }

    #[inline]
    fn begin(&mut self, cols: u16) {
        if self.cols != cols {
            self.cols = cols;
            self.lanes.clear();
            self.spare.clear();
            return;
        }
        let mut i = 0;
        while i < self.lanes.len() {
            if self.lanes[i].touched.is_empty() {
                let dark = self.lanes.swap_remove(i);
                self.spare.push(dark);
            } else {
                i += 1;
            }
        }
        for lane in &mut self.lanes {
            lane.clear();
        }
    }

    #[inline]
    fn lane_mut(&mut self, row: u16) -> &mut FieldLane {
        if self.lanes.last().is_some_and(|l| l.row == row) {
            return self.lanes.last_mut().expect("just checked");
        }
        if let Some(i) = self.lanes.iter().position(|l| l.row == row) {
            let last = self.lanes.len() - 1;
            self.lanes.swap(i, last);
            return &mut self.lanes[last];
        }
        let mut lane = self.spare.pop().unwrap_or(FieldLane::EMPTY);
        lane.row = row;
        if lane.t.len() != usize::from(self.cols) {
            lane.resize(self.cols);
        }
        self.lanes.push(lane);
        self.lanes.last_mut().expect("just pushed")
    }

    #[inline]
    fn reset(&mut self) {
        self.lanes.clear();
        self.spare.clear();
    }

    #[inline]
    fn translate(&mut self, rows: u16) {
        let mut i = 0;
        while i < self.lanes.len() {
            if let Some(row) = self.lanes[i].row.checked_sub(rows) {
                self.lanes[i].row = row;
                i += 1;
            } else {
                let mut dark = self.lanes.swap_remove(i);
                dark.clear();
                self.spare.push(dark);
            }
        }
    }

    #[inline]
    fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        let mut i = 0;
        while i < self.lanes.len() {
            if let Some(row) = band_row(self.lanes[i].row, top, bottom, delta) {
                self.lanes[i].row = row;
                i += 1;
            } else {
                let mut dark = self.lanes.swap_remove(i);
                dark.clear();
                self.spare.push(dark);
            }
        }
    }

    #[inline]
    fn set(&mut self, row: u16, col: u16, t: f32) {
        let lane = self.lane_mut(row);
        let Some(slot) = lane.t.get_mut(usize::from(col)) else {
            return;
        };
        if !slot.is_nan() {
            return;
        }
        lane.touched.push(col);
        *slot = t;
    }
}

/// The bed's one ceiling, in relative luminance.
#[inline]
#[must_use]
pub fn bed_luma_budget(theme_fg: u32) -> f32 {
    let bar =
        (relative_luminance(theme_fg) + 0.05) / (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD) - 0.05;
    bar.clamp(BED_LUMA_MIN, BED_LUMA_MAX)
}

#[inline]
const fn scale_rgb(rgb: u32, k: f32) -> u32 {
    let r = ((((rgb >> 16) & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    let g = ((((rgb >> 8) & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    let b = (((rgb & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    (r << 16) | (g << 8) | b
}

#[inline]
const fn toward_white(rgb: u32, w: f32) -> u32 {
    let r0 = ((rgb >> 16) & 0xff) as f32;
    let g0 = ((rgb >> 8) & 0xff) as f32;
    let b0 = (rgb & 0xff) as f32;
    let r = ((r0 + w * (255.0 - r0)) + 0.5).clamp(0.0, 255.0) as u32;
    let g = ((g0 + w * (255.0 - g0)) + 0.5).clamp(0.0, 255.0) as u32;
    let b = ((b0 + w * (255.0 - b0)) + 0.5).clamp(0.0, 255.0) as u32;
    (r << 16) | (g << 8) | b
}

#[inline]
fn solve_for_luma(family: impl Fn(f32) -> u32, target: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let hi0 = family(1.0);
    if crate::color_math::relative_luminance_with(table, hi0) < target {
        return hi0;
    }
    let lo0 = family(0.0);
    if crate::color_math::relative_luminance_with(table, lo0) >= target {
        return lo0;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (Some(lo0), Some(hi0));
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = family(mid);
        if lo_rgb == Some(mid_rgb) {
            lo = mid;
        } else if hi_rgb == Some(mid_rgb) {
            hi = mid;
        } else if crate::color_math::relative_luminance_with(table, mid_rgb) < target {
            lo = mid;
            lo_rgb = Some(mid_rgb);
        } else {
            hi = mid;
            hi_rgb = Some(mid_rgb);
        }
    }
    lo_rgb.unwrap_or(lo0)
}

#[inline]
fn onto_luma(rgb: u32, target: f32) -> u32 {
    let mx = ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff);
    if mx == 0 {
        return solve_for_luma(|w| toward_white(rgb, w), target);
    }
    let k_full = 255.0 / mx as f32;
    let full = scale_rgb(rgb, k_full);
    if relative_luminance(full) >= target {
        solve_for_luma(|k| scale_rgb(rgb, k * k_full), target)
    } else {
        solve_for_luma(|w| toward_white(full, w), target)
    }
}

/// The dim theme's bed `bed`, scaled down until each channel's larger of it
/// and the page `page` keeps a letter's edge at the bar ([`DIM_EDGE_GUARD`]);
/// `bed` unchanged on a theme whose text `fg` reads past [`DIM_THEME_BARE`].
#[must_use]
pub fn dim_edge_held(bed: u32, fg: u32, page: u32) -> u32 {
    let (fg, page) = (fg & 0x00FF_FFFF, page & 0x00FF_FFFF);
    let table = crate::color_math::relative_luminance_table();
    let (lf, lp) = (
        crate::color_math::relative_luminance_with(table, fg),
        crate::color_math::relative_luminance_with(table, page),
    );
    let bare = (lf.max(lp) + 0.05) / (lf.min(lp) + 0.05);
    if bare >= DIM_THEME_BARE || lf <= lp {
        return bed;
    }
    let edge = (lp + 0.05) * DIM_EDGE_SOLID / (PAPER_BAR_THEME_SHARE + DIM_EDGE_GUARD) - 0.05;
    let chmax = |c: u32| {
        let ch = |sh: u32| ((c >> sh) & 0xff).max((page >> sh) & 0xff) << sh;
        ch(16) | ch(8) | ch(0)
    };
    let hi0 = bed & 0x00FF_FFFF;
    if crate::color_math::relative_luminance_with(table, chmax(hi0)) <= edge {
        return bed;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (None, Some(hi0));
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = scale_rgb(bed, mid);
        if lo_rgb == Some(mid_rgb) {
            lo = mid;
        } else if hi_rgb == Some(mid_rgb) {
            hi = mid;
        } else if crate::color_math::relative_luminance_with(table, chmax(mid_rgb)) <= edge {
            lo = mid;
            lo_rgb = Some(mid_rgb);
        } else {
            hi = mid;
            hi_rgb = Some(mid_rgb);
        }
    }
    lo_rgb.unwrap_or_else(|| scale_rgb(bed, lo))
}

/// The bed's on-glass ink: the stop, its hue bent off the dark bed's mud
/// ([`DARK_HUE_BEND`]), held to the letters' luminance budget.
#[inline]
#[must_use]
pub fn bed_ink(rgb: u32, budget: f32) -> u32 {
    onto_luma(
        dark_hue_bent(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR)),
        budget,
    )
}

/// [`bed_ink`] without the dark hue bend (a test's control).
#[cfg(test)]
pub(crate) fn bed_ink_unbent(rgb: u32, budget: f32) -> u32 {
    onto_luma(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR), budget)
}

/// Held to the letters' luminance budget, every hue from orange to
/// yellow-green has no vivid colour left in sRGB, and unbent, a third of the
/// walk draws the dark bed in brown and olive mud. The bend draws orange as a
/// red-orange and yellow as a deep orange, both as vivid as the budget
/// allows, and crosses the basin to green inside the yellow→green interval,
/// as the light bed's [`PAPER_HUE_BEND`] bends yellow off the paper. Degrees
/// in → out.
const DARK_HUE_BEND: [(f32, f32); 4] = [(0.0, 0.0), (30.0, 14.0), (60.0, 24.0), (90.0, 92.0)];

#[inline]
#[must_use]
const fn dark_hue_bent(rgb: u32) -> u32 {
    let (h, s, v) = rgb2hsv(rgb);
    if v <= 0.0 || s <= 0.0 || !(h >= 0.0 && h < 90.0) {
        return rgb;
    }
    let mut bent = h;
    let mut i = 0;
    while i + 1 < DARK_HUE_BEND.len() {
        let (x0, y0) = DARK_HUE_BEND[i];
        let (x1, y1) = DARK_HUE_BEND[i + 1];
        if h >= x0 && h < x1 {
            bent = y0 + (y1 - y0) * (h - x0) / (x1 - x0);
            break;
        }
        i += 1;
    }
    if bent == h { rgb } else { hsv2rgb(bent, s, v) }
}

/// The bed's light twin, its ink on paper.
#[must_use]
pub(crate) fn paper_ink(rgb: u32, fg: u32, bg: u32, role: InkRole) -> u32 {
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    let mx = ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff);
    if mx == 0 {
        return bg;
    }
    let full = scale_rgb(rgb, 255.0 / mx as f32);
    let bg = bg & 0x00FF_FFFF;
    let cap = (role.alpha_cap() / 255.0).clamp(0.0, 1.0);
    let table = crate::color_math::relative_luminance_table();
    let y_fg = crate::color_math::relative_luminance_with(table, fg);
    let ratio = |y: f32| (y.max(y_fg) + 0.05) / (y.min(y_fg) + 0.05);
    let (weight, bar) = match role {
        InkRole::OverText => (
            PAPER_WEIGHT,
            Some((BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD).min(
                PAPER_BAR_THEME_SHARE
                    * ratio(crate::color_math::relative_luminance_with(table, bg)),
            )),
        ),
        InkRole::Leading => (PAPER_WEIGHT_LEADING, None),
    };
    let worst_lum = |ink: u32| {
        let ch = |sh: u32| {
            let (i, b) = (((ink >> sh) & 0xff) as f32, ((bg >> sh) & 0xff) as f32);
            (b + (i - b) * cap - 1.0).max(0.0) as usize
        };
        crate::color_math::relative_luminance_rgb(table, ch(16), ch(8), ch(0))
    };
    let bg_lab = oklab(bg);
    let eval = |(on_glass, ink): (u32, u32)| {
        let lab = oklab(on_glass);
        let (dl, da, db) = (lab[0] - bg_lab[0], lab[1] - bg_lab[1], lab[2] - bg_lab[2]);
        (dl * dl + da * da + db * db).sqrt() <= weight
            && bar.is_none_or(|bar| ratio(worst_lum(ink)) >= bar)
    };
    let hi0 = (lerp_rgb(bg, full, cap), lerp_rgb(bg, full, 1.0));
    if eval(hi0) {
        return hi0.1;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_pair, mut hi_pair) = (None, Some(hi0));
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        let pair = (lerp_rgb(bg, full, mid * cap), lerp_rgb(bg, full, mid));
        if lo_pair == Some(pair) {
            lo = mid;
        } else if hi_pair == Some(pair) {
            hi = mid;
        } else if eval(pair) {
            lo = mid;
            lo_pair = Some(pair);
        } else {
            hi = mid;
            hi_pair = Some(pair);
        }
    }
    lo_pair.map_or_else(|| lerp_rgb(bg, full, lo), |(_, ink)| ink)
}

/// The caret's ink on paper.
#[inline]
#[must_use]
pub fn paper_caret_ink(rgb: u32, bg: u32) -> u32 {
    paper_vivid_ink(rgb, bg, PAPER_CARET_CONTRAST)
}

/// The caret's colour for the walk's stop `arc`.
#[inline]
#[must_use]
pub(crate) fn caret_ink(arc: u32, cfg: &Config) -> u32 {
    if cfg.dark_theme {
        arc
    } else {
        paper_caret_ink(arc, cfg.theme_bg)
    }
}

/// A point mark's ink on paper.
#[inline]
#[must_use]
pub fn paper_mark_ink(rgb: u32, bg: u32) -> u32 {
    paper_vivid_ink(rgb, bg, PAPER_MARK_CONTRAST)
}

fn paper_vivid_ink(rgb: u32, bg: u32, bar: f32) -> u32 {
    let (h, s, v) = rgb2hsv(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR));
    if v <= 0.0 {
        return 0;
    }
    let h = if (30.0..110.0).contains(&h) {
        PAPER_HUE_BEND
            .windows(2)
            .find(|k| (k[0].0..k[1].0).contains(&h))
            .map_or(h, |k| {
                k[0].1 + (k[1].1 - k[0].1) * (h - k[0].0) / (k[1].0 - k[0].0)
            })
    } else {
        h
    };
    let full = hsv2rgb(h, s, 1.0);
    let table = crate::color_math::relative_luminance_table();
    let y_bg = crate::color_math::relative_luminance_with(table, bg & 0x00FF_FFFF);
    let holds = |cand: u32| {
        let y = crate::color_math::relative_luminance_with(table, cand);
        (y.max(y_bg) + 0.05) / (y.min(y_bg) + 0.05) >= bar
    };
    if holds(full) {
        return full;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (None, full);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = scale_rgb(full, mid);
        if mid_rgb == hi_rgb {
            hi = mid;
        } else if lo_rgb == Some(mid_rgb) || holds(mid_rgb) {
            lo = mid;
            lo_rgb = Some(mid_rgb);
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
    }
    lo_rgb.unwrap_or(0)
}

#[inline]
const fn channel_contrast(linear: &[f32; 256], a: u8, b: u8) -> f32 {
    let (a, b) = (linear[a as usize], linear[b as usize]);
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether a composited pixel is still a stop.
#[cfg(test)]
#[inline]
#[must_use]
pub(crate) fn seen(rgb: u32, bg: u32) -> bool {
    seen_with(
        &crate::color_math::relative_luminance_table().linear,
        rgb,
        bg,
    )
}

#[inline]
#[must_use]
pub(crate) const fn seen_with(linear: &[f32; 256], rgb: u32, bg: u32) -> bool {
    let (r, g, b) = (
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    );
    let mx = if r > g { r } else { g };
    let mx = if mx > b { mx } else { b };
    let mn = if r < g { r } else { g };
    let mn = if mn < b { mn } else { b };
    let spread = (mx - mn) as f32;
    if spread < SEEN_SPREAD {
        return false;
    }
    let stand = channel_contrast(linear, r, ((bg >> 16) & 0xff) as u8)
        .max(channel_contrast(linear, g, ((bg >> 8) & 0xff) as u8))
        .max(channel_contrast(linear, b, (bg & 0xff) as u8));
    stand >= SEEN_CONTRAST
}

#[inline]
#[must_use]
fn seen_floor(ink: u32, bg: u32) -> f32 {
    let linear = &crate::color_math::relative_luminance_table().linear;
    (1..=255u16)
        .rev()
        .map(|a| u8::try_from(a).unwrap_or(255))
        .find(|&a| !seen_with(linear, over_premul(bg, premul_rgb(ink, a), a), bg))
        .map_or(1.0, |a| f32::from(a) + 1.0)
}

/// The crisp edge's reach, priced at birth.
#[inline]
#[must_use]
pub const fn edge_cells(surge: f32, heat: f32) -> f32 {
    let stretch = clamp01(surge).max(clamp01(heat));
    HOT_EDGE_CELLS + (HOT_EDGE_CELLS_MAX - HOT_EDGE_CELLS) * stretch
}

/// The hot edge's ink.
#[inline]
#[must_use]
pub fn hot_edge_ink(rgb: u32) -> u32 {
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    if relative_luminance(rgb) >= HOT_EDGE_LUMA_FLOOR {
        rgb
    } else {
        onto_luma(rgb, HOT_EDGE_LUMA_FLOOR)
    }
}

/// The vivid rail's ink.
#[inline]
#[must_use]
pub fn rail_ink(rgb: u32) -> u32 {
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    let mx = ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff);
    if mx == 0 {
        return onto_luma(rgb, RAIL_LUMA_FLOOR);
    }
    let full = scale_rgb(rgb, 255.0 / mx as f32);
    let y = relative_luminance(full);
    if y < RAIL_LUMA_FLOOR {
        onto_luma(full, RAIL_LUMA_FLOOR)
    } else if y > RAIL_LUMA_CEIL {
        onto_luma(full, RAIL_LUMA_CEIL)
    } else {
        full
    }
}

/// The light theme's compositing role for the body (§3.3, L6).
#[inline]
#[must_use]
pub(crate) const fn light_role(cfg: &Config) -> InkRole {
    if cfg.ribbon_tall {
        InkRole::OverText
    } else {
        InkRole::Leading
    }
}

#[inline]
#[must_use]
const fn cov_cap(cfg: &Config) -> f32 {
    if cfg.dark_theme {
        UNDER_COV_CAP
    } else {
        light_role(cfg).alpha_cap()
    }
}

#[inline]
const fn walk_arc_at(i: usize) -> u32 {
    spectrum(walk_pace(i as f32 / (BED_INK_LUT_LEN - 1) as f32))
}

/// Shared 129-stop hot-edge ink LUT across the ribbon and meteor landing marks.
#[inline]
#[must_use]
pub(crate) fn hot_edge_ink_lut() -> &'static [u32; BED_INK_LUT_LEN] {
    static HOT_EDGE_INK_LUT: std::sync::OnceLock<[u32; BED_INK_LUT_LEN]> =
        std::sync::OnceLock::new();
    HOT_EDGE_INK_LUT.get_or_init(|| std::array::from_fn(|i| hot_edge_ink(walk_arc_at(i))))
}

#[inline]
fn rail_ink_lut() -> &'static [u32; BED_INK_LUT_LEN] {
    static RAIL_INK_LUT: std::sync::OnceLock<[u32; BED_INK_LUT_LEN]> = std::sync::OnceLock::new();
    RAIL_INK_LUT.get_or_init(|| std::array::from_fn(|i| rail_ink(walk_arc_at(i))))
}

#[inline]
fn dark_bed_max_lut() -> &'static [u32; BED_INK_LUT_LEN] {
    static DARK_BED_MAX_LUT: std::sync::OnceLock<[u32; BED_INK_LUT_LEN]> =
        std::sync::OnceLock::new();
    DARK_BED_MAX_LUT.get_or_init(|| std::array::from_fn(|i| bed_ink(walk_arc_at(i), BED_LUMA_MAX)))
}

#[inline]
fn dark_seen_common_lut(bg: u32) -> Option<&'static [f32; BED_INK_LUT_LEN]> {
    static DARK_SEEN_COMMON_LUT: std::sync::OnceLock<[(u32, [f32; BED_INK_LUT_LEN]); 5]> =
        std::sync::OnceLock::new();
    let bg = bg & 0x00FF_FFFF;
    if !matches!(
        bg,
        0x0000_0000 | 0x0010_1016 | 0x0011_1318 | 0x0016_161C | 0x001A_1B26
    ) {
        return None;
    }
    let table = DARK_SEEN_COMMON_LUT.get_or_init(|| {
        let beds = dark_bed_max_lut();
        [
            0x0000_0000,
            0x0010_1016,
            0x0011_1318,
            0x0016_161C,
            0x001A_1B26,
        ]
        .map(|g| (g, std::array::from_fn(|i| seen_floor(beds[i], g))))
    });
    table.iter().find(|&&(g, _)| g == bg).map(|(_, lut)| lut)
}

/// The bed's ink table for one theme, rebuilt only when the theme or spelling changes.
#[derive(Clone, Copy, Debug)]
pub struct BedInkLut {
    key: Option<(u32, u32, bool, bool)>,
    lut: [u32; BED_INK_LUT_LEN],
    seen: [f32; BED_INK_LUT_LEN],
}

impl Default for BedInkLut {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl BedInkLut {
    /// Unsynced bed ink lookup table.
    pub const EMPTY: Self = Self {
        key: None,
        lut: [0; BED_INK_LUT_LEN],
        seen: [0.0; BED_INK_LUT_LEN],
    };

    fn sync(&mut self, cfg: &Config) {
        let key = (cfg.theme_fg, cfg.theme_bg, cfg.dark_theme, cfg.ribbon_tall);
        if self.key == Some(key) {
            return;
        }
        let budget = bed_luma_budget(cfg.theme_fg);
        let role = light_role(cfg);
        let dark_max =
            (cfg.dark_theme && (budget - BED_LUMA_MAX).abs() == 0.0).then(dark_bed_max_lut);
        if let Some(max_lut) = dark_max
            && let Some(seen_lut) = dark_seen_common_lut(cfg.theme_bg)
        {
            self.lut = *max_lut;
            self.seen = *seen_lut;
            self.key = Some(key);
            return;
        }
        let light_a = (cov_cap(cfg) + 16.0).min(255.0) as u8;
        let linear = &crate::color_math::relative_luminance_table().linear;
        for i in 0..BED_INK_LUT_LEN {
            let bed = if let Some(max_lut) = dark_max {
                max_lut[i]
            } else {
                let arc = walk_arc_at(i);
                if cfg.dark_theme {
                    dim_edge_held(bed_ink(arc, budget), cfg.theme_fg, cfg.theme_bg)
                } else {
                    let paper = paper_ink(arc, cfg.theme_fg, cfg.theme_bg, role);
                    let shows = seen_with(
                        linear,
                        over_premul(cfg.theme_bg, premul_rgb(paper, light_a), light_a)
                            & 0x00FF_FFFF,
                        cfg.theme_bg,
                    );
                    if cfg.ribbon_tall && !shows {
                        crate::cursor_glow::light_body_ink(arc, cfg.theme_fg, cfg.theme_bg, light_a)
                    } else {
                        paper
                    }
                }
            };
            self.lut[i] = bed;
            self.seen[i] = seen_floor(bed, cfg.theme_bg);
        }
        self.key = Some(key);
    }

    /// The ink at walk position `t`, folded through `meteor::tri`, so the station under the caret is the caret's stop by construction.
    #[inline]
    #[must_use]
    pub const fn at(&self, t: f32) -> u32 {
        if self.key.is_none() {
            return 0;
        }
        Self::sample(&self.lut, t)
    }

    /// The hot edge's ink at walk position `t`.
    #[inline]
    #[must_use]
    pub fn hot_at(&self, t: f32) -> u32 {
        if self.key.is_none() {
            return 0;
        }
        Self::sample(hot_edge_ink_lut(), t)
    }

    /// The vivid rail's ink at walk position `t`.
    #[inline]
    #[must_use]
    pub fn rail_at(&self, t: f32) -> u32 {
        if self.key.is_none() {
            return 0;
        }
        Self::sample(rail_ink_lut(), t)
    }

    /// The colour floor at walk position `t`.
    #[inline]
    #[must_use]
    pub const fn seen_floor_at(&self, t: f32) -> f32 {
        if self.key.is_none() {
            return 0.0;
        }
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let idx = x as usize;
        let i = if idx < BED_INK_LUT_LEN - 1 {
            idx
        } else {
            BED_INK_LUT_LEN - 1
        };
        let j = if i + 1 < BED_INK_LUT_LEN - 1 {
            i + 1
        } else {
            BED_INK_LUT_LEN - 1
        };
        self.seen[i].max(self.seen[j])
    }

    #[inline]
    #[must_use]
    const fn sample(table: &[u32; BED_INK_LUT_LEN], t: f32) -> u32 {
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let idx = x as usize;
        let i = if idx < BED_INK_LUT_LEN - 1 {
            idx
        } else {
            BED_INK_LUT_LEN - 1
        };
        let j = if i + 1 < BED_INK_LUT_LEN - 1 {
            i + 1
        } else {
            BED_INK_LUT_LEN - 1
        };
        lerp_rgb(table[i], table[j], x - i as f32)
    }
}

const CROSS_LUT0: usize = crate::spectrum::SPECTRUM_ANCHOR_AT[3] + CROSS_GUARD;
const CROSS_LUT1: usize = crate::spectrum::SPECTRUM_ANCHOR_AT[4] - CROSS_GUARD;

const CROSS_GUARD: usize = 24;
const CROSS_ARC0: f32 = CROSS_LUT0 as f32 / (crate::spectrum::SPECTRUM_LUT_LEN - 1) as f32;
const CROSS_ARC1: f32 = CROSS_LUT1 as f32 / (crate::spectrum::SPECTRUM_LUT_LEN - 1) as f32;

const CROSS_PACE: [u16; CROSS_PACE_KNOTS] = [
    33539, 33818, 34076, 34375, 34679, 35026, 35233, 35530, 35828, 36087, 36381, 36642, 36900,
    37159, 37456, 37760, 38011, 38231, 38537, 38825, 39085, 39343, 39598, 39897, 40189, 40573,
    40831, 41141, 41399, 41696, 41916, 42169, 42470, 42683, 42939, 43158, 43336, 43542, 43795,
    43969, 44174, 44364, 44569, 44782, 44996, 45199, 45337, 45554, 45715, 45896, 46035, 46197,
    46409, 46552, 46712, 46840, 47051, 47224, 47523, 47738, 47952, 48208, 48423, 48553, 48830,
];

/// Knots in [`CROSS_PACE`].
pub const CROSS_PACE_KNOTS: usize = 65;

/// The arc position of a walk position.
#[inline]
#[must_use]
pub const fn walk_arc(t: f32) -> f32 {
    walk_pace(clamp01(tri(t)))
}

/// Where a walk position lands on the arc.
#[inline]
#[must_use]
pub const fn walk_pace(x: f32) -> f32 {
    let x = clamp01(x);
    if x <= CROSS_ARC0 || x >= CROSS_ARC1 {
        return x;
    }
    let f = (x - CROSS_ARC0) / (CROSS_ARC1 - CROSS_ARC0) * (CROSS_PACE_KNOTS - 1) as f32;
    let u = f as usize;
    let i = if u < CROSS_PACE_KNOTS - 2 {
        u
    } else {
        CROSS_PACE_KNOTS - 2
    };
    let (a, b) = (CROSS_PACE[i] as f32, CROSS_PACE[i + 1] as f32);
    (a + (b - a) * (f - i as f32)) / 65535.0
}

/// The classic walk.
#[inline]
#[must_use]
pub const fn walk_t(d: f32) -> f32 {
    if !d.is_finite() {
        return 0.0;
    }
    if d <= WALK_FAST_CELLS {
        d / WALK_FAST_CELLS
    } else {
        1.0 + (d - WALK_FAST_CELLS) * WALK_LAY_RATE
    }
}

/// A cell's terminal approach to zero, read on its own clock.
#[inline]
#[must_use]
pub fn expiry_melt(u: f32) -> f32 {
    let rem = 1.0 - clamp01(u);
    if rem >= EXPIRY_MELT_SHARE {
        return 1.0;
    }
    let taper = smoothstep01(rem / EXPIRY_MELT_SHARE);
    if taper <= 0.0 {
        0.0
    } else if taper >= 1.0 {
        1.0
    } else {
        taper.powf(EXPIRY_MELT_GAMMA)
    }
}

/// The focus ember's level, per stop.
#[inline]
#[must_use]
pub const fn ember_level(floor: f32, f: f32) -> f32 {
    let floor = floor.max(0.0);
    clamp01(floor + (1.0 - floor) * clamp01(f))
}

/// The one linear fade of the theme (§2.5, §6.11).
#[inline]
#[must_use]
pub const fn reduced_fade(remaining_s: f32) -> f32 {
    clamp01(remaining_s / (REDUCED_MOTION_FADE_MS / 1000.0))
}

/// Seconds until [`expiry_melt`] next takes a mark of `peak` levels down one u8 level, `u` of the way through its `life_s`.
#[must_use]
pub(crate) fn level_step_melt(peak: f32, u: f32, life_s: f32) -> Option<f32> {
    if !(peak.is_finite() && life_s.is_finite() && u.is_finite()) || peak <= 0.0 || life_s <= 0.0 {
        return None;
    }
    let u = clamp01(u);
    let level = (peak * expiry_melt(u)).round();
    if level < 1.0 {
        return None;
    }
    let taper = ((level - 0.5) / peak)
        .clamp(0.0, 1.0)
        .powf(1.0 / EXPIRY_MELT_GAMMA);
    let x = 0.5 - ((1.0 - 2.0 * taper).clamp(-1.0, 1.0).asin() / 3.0).sin();
    let u_next = 1.0 - EXPIRY_MELT_SHARE * x;
    Some(((u_next - u) * life_s).max(0.0))
}

type Sample = (f32, f32, f32, f32, f32);

#[derive(Clone, Copy, Debug)]
struct Run {
    row: u16,
    col0: u16,
    col1: u16,
    lo: usize,
    hi: usize,
    head_col: u16,
    head: usize,
    wet: bool,
    at_caret: bool,
    born: Instant,
    stream_dir: i8,
    /// How far the run is held inside its row ([`Ribbon::plan_over`]), `0`
    /// to `1`: at `1` the body is laid inside the typed row, as on a pane's
    /// last row, and the rail is dark, so no light lies on the next line's
    /// ascenders; between, both shapes blend and the rail dims.
    hold: f32,
}

/// How long a line's light takes to settle into its row when the next row
/// inks under it (and to reach below again when it clears), seconds.
pub const HOLD_EASE_S: f32 = 0.15;

/// How long a held row takes to let go of its row, seconds: when the next
/// row clears, and when a line on it flows into the fold.
pub const HOLD_RELEASE_S: f32 = 0.60;

/// The feather a run the hand has left wears at its head end ([`Ribbon::head_feather`]): `1` everywhere for a run at the caret.
#[derive(Clone, Copy, Debug)]
struct HeadFeather {
    /// The head boundary's window x.
    head_x: f32,
    /// The feather's reach back from it, px; `0` for none.
    width: f32,
}

impl HeadFeather {
    /// The share of its light a boundary at window x `x` keeps: nothing at the head end, easing to all of it [`HEAD_FEATHER_CELLS`] back.
    #[inline]
    const fn at(self, x: f32) -> f32 {
        if self.width <= 0.0 {
            return 1.0;
        }
        smoothstep01((x - self.head_x).abs() / self.width)
    }
}

/// A boundary as the emit lays it: a planned [`Segment`]'s fields with its coverage in `f32`, or one laid between two of them.
#[derive(Clone, Copy, Debug)]
struct LaidSegment {
    x: f32,
    spine: f32,
    up: f32,
    dn: f32,
    t: f32,
    cov: f32,
}

const FEATHER_FINE_PX: f32 = 1.0;

/// How deep from a run's tail its boundaries are laid finely, in cells
/// ([`for_each_feathered`]).
const RUN_END_FINE_CELLS: f32 = 1.0;

/// The strides a run's end is laid finely into: a pixel apart on the
/// gallery's 14 px cells, two on a Retina's 30.
const RUN_END_FINE_SPLIT: usize = 5;

/// The share of the quad budget a frame's estimate, its tails laid finely
/// with it, must stay under: below [`FEATHER_ROOM_SHARE`], since the
/// estimate leaves out the head feathers' own fine windows.
const RUN_END_ROOM_SHARE: f32 = 0.5;

const FEATHER_ROOM_ROWS: f32 = 1.6;
const FEATHER_ROOM_SHARE: f32 = 0.6;

/// Lays each of `segs` (in reverse) through `each`, and between two of
/// them a boundary a pixel where the head's `feather` reaches, or every
/// `end_px` px within `end_reach` px of the left end of `segs` (where the
/// plan eases a run's tail in) unless the head is there. The rasterizer
/// holds a window's light flat across each stride, so a run's tail easing
/// in from [`RUN_TAIL_EASE`] over its cell's three slabs would stand as a
/// stair of flat blocks. The reach is a place, not a coverage step, so a
/// window keeps its layout while its light changes.
fn for_each_feathered(
    segs: &[Segment],
    feather: HeadFeather,
    (end_reach, end_px): (f32, f32),
    mut each: impl FnMut(Option<usize>, LaidSegment),
) {
    let tail = segs.first().map_or(0.0, |s| s.x);
    let feather_active = feather.width > 0.0;
    let end_active = end_reach > 0.0 && end_px > 0.0 && (tail - feather.head_x).abs() >= 1.0;
    let laid = |s: &Segment| LaidSegment {
        x: s.x,
        spine: s.spine,
        up: s.up,
        dn: s.dn,
        t: s.t,
        cov: f32::from(s.cov),
    };
    if !feather_active && !end_active {
        for (i, seg) in segs.iter().enumerate().rev() {
            each(Some(i), laid(seg));
        }
        return;
    }
    let mut prev: Option<LaidSegment> = None;
    for (i, seg) in segs.iter().enumerate().rev() {
        let here = laid(seg);
        let feathered = |p: &LaidSegment| {
            feather_active
                && (p.x - feather.head_x)
                    .abs()
                    .min((here.x - feather.head_x).abs())
                    < feather.width
        };
        let at_end = |p: &LaidSegment| {
            end_active && (p.x - tail).abs().min((here.x - tail).abs()) < end_reach
        };
        let fine = prev.map(|p| {
            if feathered(&p) {
                FEATHER_FINE_PX
            } else if at_end(&p) {
                end_px
            } else {
                0.0
            }
        });
        if let Some(p) = prev
            && let Some(px) = fine.filter(|&px| px > 0.0)
        {
            let n = ((here.x - p.x).abs() / px).floor() as usize;
            if n > 1 {
                let inv_nf = 1.0 / (n as f32);
                let (dx, d_spine, d_up, d_dn, dt, d_cov) = (
                    here.x - p.x,
                    here.spine - p.spine,
                    here.up - p.up,
                    here.dn - p.dn,
                    here.t - p.t,
                    here.cov - p.cov,
                );
                for k in 1..n {
                    let u = k as f32 * inv_nf;
                    each(
                        None,
                        LaidSegment {
                            x: p.x + dx * u,
                            spine: p.spine + d_spine * u,
                            up: p.up + d_up * u,
                            dn: p.dn + d_dn * u,
                            t: p.t + dt * u,
                            cov: p.cov + d_cov * u,
                        },
                    );
                }
            }
        }
        each(Some(i), here);
        prev = Some(here);
    }
}

/// The attach's level this frame, set by [`Ribbon::plan_run`] at the head boundary of the run under the caret: the head boundary's coverage (`base`), the head cell's uniform attack (`uniform`, [`BIRTH_EDGE_FLOOR`] to 1 over the edge-in) and the wipe's probe ([`Ribbon::wipe_of`]).
#[derive(Clone, Copy, Debug)]
struct AttachFloor {
    row: u16,
    col: u16,
    base: f32,
    uniform: f32,
    probe: Cell,
}

impl AttachFloor {
    #[inline]
    fn level(&self, rib: &Ribbon, ctx: &Ctx<'_>, dist: f32) -> f32 {
        self.base * self.uniform * rib.wipe_of(ctx, &self.probe, dist)
    }
}

#[derive(Clone, Copy, Debug)]
struct ShortReanchorProof {
    from: (u16, u16),
    to: (u16, u16),
    at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct RewrapStart {
    from: (u16, u16),
    to: (u16, u16),
    start: u16,
    width: Option<u16>,
    space: Option<u16>,
}

/// The ribbon producer.
#[derive(Clone, Debug)]
pub struct Ribbon {
    /// Test-only: leave a run the hand has left square-cut at its head end (no [`Ribbon::head_feather`]), the negative control of the feather's test.
    #[cfg(test)]
    head_square: bool,
    /// Test-only: keep the hot edge on a run the hand has left, the negative
    /// control of the hot edge's hand test.
    #[cfg(test)]
    hot_edge_left_lit: bool,
    /// Test-only: lay the boundaries inside a head feather's reach at the
    /// stride ([`for_each_feathered`]), the negative control of the smooth
    /// feather's test.
    #[cfg(test)]
    feather_coarse: bool,
    /// Test-only: lay a run's tail at the stride, the negative control of the
    /// tail's fine lay ([`RUN_END_FINE_CELLS`]).
    #[cfg(test)]
    tail_stair: bool,
    /// Test-only: centre the hot edge on the row's top under a kept box, the
    /// negative control of its inset.
    #[cfg(test)]
    hot_edge_on_bar: bool,
    /// Test-only: step a line's hold to its target in one frame, the negative
    /// control of the hold's ease.
    #[cfg(test)]
    hold_steps: bool,
    /// Test-only: lay the rail at the row's bottom ([`RAIL_GAP_CH`] off), the
    /// negative control of the descenders' hairline.
    #[cfg(test)]
    rail_flush: bool,
    /// Test-only: draw the pet's and the flat ribbon alike on paper (no
    /// rail, one body), the negative control of their reading apart there.
    #[cfg(test)]
    paper_alike: bool,
    /// Test-only: leave each row's hold behind when a scroll or a band move
    /// carries its line away, the negative control of the carried hold.
    #[cfg(test)]
    holds_left_behind: bool,
    /// Each row's hold ([`Run::hold`]), with when it was last eased, and its
    /// next frame's list (resident and reused). By row: the runs of a row
    /// share seams, so they share a floor.
    holds: Vec<(u16, f32, Instant)>,
    holds_next: Vec<(u16, f32, Instant)>,
    /// Prior shape of each moved or newborn cohort until the next plan.
    /// Kept apart from row holds so an earlier move cannot rewrite a later
    /// run's source. A fresh identity explicitly has no previous shape.
    carried_holds: Vec<(u32, Option<(f32, Instant)>)>,
    /// The live cells, resident and reused (§18: no allocation per frame).
    cells: Vec<Cell>,
    cohorts: Vec<Cohort>,
    plan: Vec<Segment>,
    index: FieldIndex,
    slabs: usize,
    next_cohort: u32,
    caret: Option<(u16, u16)>,
    runs: Vec<Run>,
    sorted: Vec<(u16, u16, u32)>,
    verts: Vec<RibbonVertex>,
    released_cohorts: Vec<u32>,
    ink: BedInkLut,
    ground: u32,
    ink_quads: usize,
    ember_at: Option<Instant>,
    rearm: Option<(Instant, f32)>,
    translated_at: Option<Instant>,
    reduced: bool,
    pane: Option<(u16, u16)>,
    pane_rows: Option<(u16, u16)>,
    attach: Option<(u16, u16)>,
    attach_floor: Option<AttachFloor>,
    cell_h: f32,
    wedge_px: f32,
    scroll_followed: bool,
    followed_now: Vec<(u16, u16, Instant)>,
    short_reanchor: Option<ShortReanchorProof>,
    shifted_now: Vec<(u16, u16, u16, i16)>,
    pending_erase: u16,
    pen: Option<(u16, u16)>,
    last_space: Option<(u16, u16)>,
    arrived: Option<(u16, u16)>,
    prev_space: Option<(u16, u16)>,
    relocate: Option<Relocate>,
    soft_wrap: Option<SoftWrap>,
    carry: Option<(SoftWrap, Instant)>,
    reflow: Option<SoftWrap>,
    lift: Option<Lift>,
    rewrap_start: Option<RewrapStart>,
    revive: Option<Revive>,
    split_off: Option<SplitOff>,
    last_walk: Option<LastWalk>,
    fresh_line: Option<FreshLine>,
    seam: Option<Seam>,
    ioi_ms: f32,
    last_key_at: Option<Instant>,
    phrase_open: Option<u32>,
    next_phrase: u32,
    curtain: Option<Instant>,
    hand_floor: Option<(u16, u16)>,
    /// What the body's top never feathers up into ([`KeepOut`]): a word
    /// icon or a lettered or filled cell standing on the run's row.
    pub keep_out: KeepOut,
}

/// How far beside a [`KeepOut`] box the body's top eases back to its own
/// reach, in cells.
const KEEP_OUT_EASE_CELLS: f32 = 0.5;

#[derive(Clone, Copy, Debug)]
struct Relocate {
    row: u16,
    w: u16,
    at: Instant,
    walk: Option<WalkPos>,
}

/// How long a [`Relocate`] waits for the caret to reveal the moved word.
pub const RELOCATE_PATIENCE_S: f32 = 2.0;

/// The soft-wrapped caret (`CursorGlow::soft_wrapped_caret`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftWrap {
    pub origin: (u16, u16),
    pub landing: (u16, u16),
    pub word_col0: u16,
}

/// The lifted word (`CursorGlow::lifted_word`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lift {
    pub origin: (u16, u16),
    pub landing: (u16, u16),
    pub dst: (u16, u16),
}

/// The revive lays a line back on its own walk (`RAINBOW-KITTY-V2.md` §43).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Revive {
    pub row: u16,
    pub col0: u16,
    pub col1: u16,
    pub origin: RunOrigin,
}

impl Revive {
    #[inline]
    const fn covers(&self, row: u16, col: u16) -> bool {
        row == self.row && self.col0 <= col && col < self.col1
    }
}

/// A typed run's walk origin, kept outside the ribbon ([`Revive`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunOrigin {
    pub anchor_col: u16,
    pub t0: f32,
    pub d0: f32,
}

impl RunOrigin {
    /// `coh`'s walk.
    #[inline]
    #[must_use]
    pub const fn of(coh: &Cohort) -> Self {
        Self {
            anchor_col: coh.anchor_col,
            t0: coh.t0,
            d0: coh.d0,
        }
    }

    #[inline]
    const fn walk(self) -> WalkPos {
        WalkPos {
            t: self.t0,
            d: self.d0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SplitOff {
    copy: u32,
    dr: i16,
    col0: u16,
    col1: u16,
}

#[derive(Clone, Copy, Debug)]
struct LastWalk {
    end: WalkPos,
    alive_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seam {
    row: u16,
    at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FreshLine {
    since: Instant,
    chord_at: Option<Instant>,
    awaiting_move: bool,
    left_row: Option<u16>,
}

impl FreshLine {
    #[inline]
    fn takes(self, row: u16, at: Instant) -> bool {
        !self.awaiting_move
            && self.chord_at.is_none_or(|chord| at != chord)
            && (at >= self.since || self.left_row.is_some_and(|left| left != row))
    }
}

#[derive(Clone, Copy)]
enum RetractX {
    Identity,
    SuckIn { caret_x: f32, keep: f32 },
    FlowRightFirst { start: f32, s: f32 },
    FlowLeftFirst { end: f32, s: f32 },
}

impl RetractX {
    #[inline]
    const fn at(self, x: f32) -> f32 {
        match self {
            Self::Identity => x,
            Self::SuckIn { caret_x, keep } => caret_x + (x - caret_x) * keep,
            Self::FlowRightFirst { start, s } => {
                if x < start - 0.5 {
                    x
                } else {
                    (x - s).max(start)
                }
            }
            Self::FlowLeftFirst { end, s } => {
                if x > end + 0.5 {
                    x
                } else {
                    (x + s).min(end)
                }
            }
        }
    }
}

impl Default for Ribbon {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Ribbon {
    /// A ribbon with nothing laid.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            #[cfg(test)]
            head_square: false,
            #[cfg(test)]
            hot_edge_left_lit: false,
            #[cfg(test)]
            feather_coarse: false,
            #[cfg(test)]
            tail_stair: false,
            #[cfg(test)]
            hot_edge_on_bar: false,
            #[cfg(test)]
            hold_steps: false,
            #[cfg(test)]
            rail_flush: false,
            #[cfg(test)]
            paper_alike: false,
            #[cfg(test)]
            holds_left_behind: false,
            holds: Vec::new(),
            holds_next: Vec::new(),
            carried_holds: Vec::new(),
            cells: Vec::new(),
            cohorts: Vec::new(),
            plan: Vec::new(),
            index: FieldIndex::EMPTY,
            slabs: 0,
            next_cohort: 0,
            caret: None,
            runs: Vec::new(),
            sorted: Vec::new(),
            verts: Vec::new(),
            released_cohorts: Vec::new(),
            ink: BedInkLut::EMPTY,
            ground: 0,
            ink_quads: 0,
            ember_at: None,
            rearm: None,
            translated_at: None,
            reduced: false,
            pane: None,
            pane_rows: None,
            attach: None,
            attach_floor: None,
            cell_h: 0.0,
            wedge_px: 0.0,
            scroll_followed: false,
            followed_now: Vec::new(),
            short_reanchor: None,
            shifted_now: Vec::new(),
            pending_erase: 0,
            pen: None,
            last_space: None,
            arrived: None,
            prev_space: None,
            relocate: None,
            soft_wrap: None,
            carry: None,
            reflow: None,
            lift: None,
            rewrap_start: None,
            revive: None,
            split_off: None,
            last_walk: None,
            fresh_line: None,
            seam: None,
            ioi_ms: 0.0,
            last_key_at: None,
            phrase_open: None,
            next_phrase: 0,
            curtain: None,
            hand_floor: None,
            keep_out: KeepOut::EMPTY,
        }
    }

    /// The host has queued the licensed move of the exact held park whose source row's content followed.
    #[inline]
    pub(crate) const fn prove_short_reanchor(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        at: Instant,
    ) {
        debug_assert!(
            from.0 == to.0 && {
                let d = from.1.saturating_sub(to.1);
                d >= 1 && d <= 2
            }
        );
        self.short_reanchor = Some(ShortReanchorProof { from, to, at });
    }

    /// The live cells, in pool (append) order. Read-only.
    #[inline]
    #[must_use]
    pub const fn cells(&self) -> &[Cell] {
        self.cells.as_slice()
    }

    /// The live cohorts. Read-only.
    #[inline]
    #[must_use]
    pub const fn cohorts(&self) -> &[Cohort] {
        self.cohorts.as_slice()
    }

    /// The instant `cell`'s expiry is measured from — see [`live_since`].
    #[must_use]
    #[cfg(test)]
    pub fn live_since(&self, cell: &Cell) -> Instant {
        live_since(&self.cohorts, cell)
    }

    /// The callback-relative swoosh as a fault.
    #[cfg(test)]
    pub fn restart_unobserved_swooshes(&mut self, now: Instant) {
        for coh in &mut self.cohorts {
            let unobserved = matches!(coh.phase, Phase::Laying | Phase::Grace);
            if !unobserved || coh.phase_at(coh.idle_s(now)).is_some() {
                continue;
            }
            let lead_s = if coh.flow.is_some() {
                1e-3
            } else if coh.grace_s.is_finite() {
                coh.grace_s.max(PHRASE_REST_MIN_S)
            } else {
                PHRASE_REST_MIN_S
            };
            let restart = now
                .checked_sub(std::time::Duration::from_secs_f32(lead_s))
                .unwrap_or(now);
            coh.alive_at = restart;
            if coh.flow.is_some() {
                coh.flow = Some(restart);
            }
        }
    }

    /// This frame's planned boundary samples, valid after [`Ribbon::plan`].
    #[inline]
    #[must_use]
    pub const fn plan_segments(&self) -> &[Segment] {
        self.plan.as_slice()
    }

    /// The frame's vertex density.
    #[inline]
    #[must_use]
    pub const fn slabs_per_cell(&self) -> usize {
        if self.slabs < 1 {
            1
        } else if self.slabs > SLABS_PER_CELL {
            SLABS_PER_CELL
        } else {
            self.slabs
        }
    }

    /// The caret cell as last observed, or `None` before any.
    #[must_use]
    #[cfg(test)]
    pub const fn caret(&self) -> Option<(u16, u16)> {
        self.caret
    }

    /// The two profiles (§4).
    #[inline]
    #[must_use]
    pub const fn body_profile(cfg: &Config) -> BodyProfile {
        let dn_ch = if Self::comet(cfg) {
            COMET_DN_HAND_CH
        } else {
            DN_TOP_CH
        };
        if cfg.ribbon_tall {
            BodyProfile {
                up_ch: TALL_UP_CH,
                dn_ch,
                core_up_ch: TALL_CORE_CH,
                shoulder: SHOULDER_TALL,
            }
        } else {
            BodyProfile {
                up_ch: UNDERLINE_UP_CH,
                dn_ch,
                core_up_ch: LEAD_CH,
                shoulder: SHOULDER_UNDERLINE,
            }
        }
    }

    /// Whether the comet body and the from-the-hand attack apply (`RAINBOW-KITTY-V2.md` §30).
    #[inline]
    #[must_use]
    pub const fn comet(cfg: &Config) -> bool {
        !cfg.ribbon_flat
    }

    /// Whether the vivid rail is drawn: under the comet body on either
    /// page, so the pet's ribbon keeps its under-edge on paper too, where
    /// without it the pet's and the flat ribbon read alike.
    #[inline]
    #[must_use]
    pub const fn rail_lit(cfg: &Config) -> bool {
        Self::comet(cfg)
    }

    /// Ingest one engine event at its own edge, before this frame's plan.
    pub fn on_event(&mut self, ev: &super::Event, at: Instant, ctx: &Ctx<'_>) {
        // Under a curtain nothing is laid (§2.8): the cells on glass belong to an ended coordinate space.
        if self.curtain.is_some() {
            if let Event::Move { to, .. } = *ev {
                self.caret = Some(to);
            }
            return;
        }
        if matches!(
            *ev,
            Event::CancelComposerNewline
                | Event::Erase
                | Event::Kill { .. }
                | Event::Return
                | Event::Move {
                    licence: Licence::Nav | Licence::Synthetic,
                    ..
                }
        ) && self.fresh_line.is_some_and(|gate| gate.awaiting_move)
        {
            self.fresh_line = None;
        }
        let revoke = match *ev {
            Event::Erase | Event::Kill { .. } | Event::Return | Event::ComposerNewline => {
                Some(ctx.caret.0)
            }
            Event::Rewrite { row, .. } => Some(row),
            Event::Move {
                from,
                licence: Licence::Nav | Licence::Return | Licence::Synthetic,
                ..
            } => Some(from.0),
            _ => None,
        };
        if let Some(row) = revoke {
            for cohort in &mut self.cohorts {
                if cohort.row == row {
                    cohort.revoke_releases(at);
                }
            }
        }
        match *ev {
            Event::Typed { cells, class, .. } => {
                // A key typed while the focus ember burns clears it, as a focus regain does.
                if self.ember_at.is_some() {
                    self.regain_focus(at);
                }
                self.note_key(at);
                self.pending_erase = 0;
                let landing = self.typed_landing(
                    ctx.caret,
                    cells,
                    u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX),
                );
                let landing = self.insert_landing(landing, cells);
                self.pen = Some(landing);
                let ctx = &Ctx {
                    caret: landing,
                    ..*ctx
                };
                if class == TypedClass::Space {
                    let (row, col_end) = landing;
                    if let Some(c) = col_end.checked_sub(1) {
                        self.note_space((row, c));
                    }
                }
                self.lay(cells, at, ctx);
            }
            Event::Erase => {
                self.pen = Some(ctx.caret);
                self.touch_hand(ctx.caret.0, ctx.caret.1);
                self.erase(ctx.caret, 1, RETRACT_FADE_S, at);
            }
            Event::Kill { cells, scope } => {
                self.pen = Some(ctx.caret);
                self.erase(ctx.caret, cells.max(1), kill_span_s(cells), at);
                if scope == super::KillScope::Line {
                    self.phrase_open = None;
                    self.arm_fresh_line(at, None);
                    self.mark_line_killed(ctx.caret.0);
                }
            }
            Event::Move {
                from, to, licence, ..
            } => {
                self.caret = Some(to);
                if !licence.is_echo() {
                    self.pen = Some(to);
                    self.arrived = Some(to);
                    self.touch_hand(to.0, to.1);
                }
                let same_row = to.0 == from.0;
                if licence == Licence::Return || (!same_row && licence == Licence::Nav) {
                    self.arm_fresh_line(at, (!same_row).then_some(from.0));
                    if licence == Licence::Return
                        && let Some(gate) = &mut self.fresh_line
                    {
                        gate.awaiting_move = false;
                    }
                }
                if licence.is_echo() {
                    // An echo's motion is inert (its key or sweep laid the cells) except the hand leaving the row, an erase's retreat, and a re-anchor.
                    if !same_row {
                        // A repaint's home is not a gesture: only a row change the hand could have made ([`Ribbon::hand_changed_row`]) leaves the row; anything else under an echo licence moves only the caret mirror.
                        if Self::hand_changed_row(from, to) {
                            self.leave_row(from, to, at, licence);
                        }
                    } else if to.1 < from.1 {
                        if let Some(lift) =
                            self.lift.filter(|l| l.origin == from && l.landing == to)
                        {
                            self.lift_word(lift, at);
                        } else if self.pending_erase > 0 {
                            let erased = from.1 - to.1;
                            self.retract_suffix(to.0, to.1, erased, at, RETRACT_FADE_S);
                            self.pending_erase = self.pending_erase.saturating_sub(erased);
                        } else if licence == Licence::Typed
                            && !self.scroll_followed
                            && (from.1 - to.1 >= RE_ANCHOR_MIN_CELLS
                                || self.short_reanchor.is_some_and(|proof| {
                                    proof.from == from && proof.to == to && proof.at == at
                                })
                                || self.rewrap_start.is_some_and(|proof| {
                                    proof.from == from && proof.to == to && proof.width.is_some()
                                }))
                        {
                            self.re_anchor(from, to, at);
                            self.short_reanchor = None;
                        }
                    } else if to.1 > from.1 {
                        self.settle_carry(to.0, from.1, to.1, at);
                        self.settle_relocate(to.0, from.1, at, ctx);
                    }
                } else {
                    let scrub = same_row
                        && licence.is_keyed_nav()
                        && self.scrub(to.0, from.1, to.1, at, ctx);
                    let jump = !same_row || to.1.abs_diff(from.1) >= JUMP_MIN_CELLS;
                    if scrub {
                    } else if jump {
                        let wakes = same_row && licence.is_keyed_nav();
                        let keep = if wakes {
                            self.wake(to.0, from.1, to.1, at, ctx, WAKE_LIFE_S, true)
                        } else {
                            None
                        };
                        let program = licence == Licence::Pty;
                        let toward = if program {
                            Some((None, from.1, false))
                        } else {
                            same_row.then_some((Some(to.0), to.1, to.1 < from.1))
                        };
                        if to.0 > from.0 && licence == Licence::Return {
                            self.print_row(from.0, at);
                        }
                        self.abandon(at, keep, toward, program);
                    } else if same_row && licence == Licence::Nav && from.1 != to.1 {
                        self.wake(
                            to.0,
                            from.1,
                            to.1,
                            at,
                            ctx,
                            WAKE_LIFE_S * WAKE_HOP_LIFE_SHARE,
                            false,
                        );
                    }
                }
            }
            Event::Focus(on) => {
                if on {
                    self.regain_focus(at);
                } else {
                    self.ember_at = Some(at);
                    self.rearm = None;
                    self.seam = None;
                }
            }
            Event::Sweep { row, col0, col1 } => {
                let (row, end) = self.insert_landing((row, col1), col1.saturating_sub(col0));
                let col0 = end.saturating_sub(col1.saturating_sub(col0));
                self.sweep(row, col0, end, at, ctx);
            }
            Event::Rewrite { row, col, cells } => {
                let span_s = kill_span_s(cells);
                self.retract_suffix(row, col, cells.max(1), at, span_s);
                self.caret = Some((row, col));
            }
            Event::Return => {
                self.phrase_open = None;
                self.arm_fresh_line(at, None);
            }
            Event::ComposerNewline => {
                self.phrase_open = None;
                self.arm_fresh_line(at, None);
                if let Some(gate) = &mut self.fresh_line {
                    gate.chord_at = Some(at);
                    gate.awaiting_move = true;
                }
            }
            Event::CancelComposerNewline => {}
            Event::ReducedMotion(_) => {}
        }
    }

    #[inline]
    fn arm_fresh_line(&mut self, at: Instant, left_row: Option<u16>) {
        self.seam = None;
        self.fresh_line = Some(match self.fresh_line {
            Some(gate) => FreshLine {
                since: gate.since.min(at),
                chord_at: gate.chord_at,
                awaiting_move: gate.awaiting_move,
                left_row: gate.left_row.or(left_row),
            },
            None => FreshLine {
                since: at,
                chord_at: None,
                awaiting_move: false,
                left_row,
            },
        });
    }

    /// Arm the seam on `row` at `at` ([`Ribbon::seam`]).
    #[inline]
    pub const fn arm_seam(&mut self, row: u16, at: Instant) {
        self.seam = Some(Seam { row, at });
    }

    /// The focused pane's `(first column, width)`; `None` is the whole grid.
    #[inline]
    pub const fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// The focused pane's `(first row, rows)`.
    #[inline]
    pub const fn set_pane_rows(&mut self, pane_rows: Option<(u16, u16)>) {
        self.pane_rows = pane_rows;
    }

    #[inline]
    const fn pane_bot(&self, ctx: &Ctx<'_>, chf: f32) -> f32 {
        let grid = ctx.geom.fx_bot() as f32;
        match self.pane_rows {
            Some((row0, rows)) => {
                (ctx.geom.origin_y as f32 + row0.saturating_add(rows) as f32 * chf).min(grid)
            }
            None => grid,
        }
    }

    /// Drop the curtain ([`CURTAIN_S`]).
    pub fn curtain(&mut self, at: Instant) {
        self.phrase_open = None;
        self.arm_fresh_line(at, None);
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = None;
            gate.chord_at = None;
            gate.awaiting_move = false;
        }
        self.clear_space_transient_state();
        if self.cells.is_empty() || self.curtain.is_some() {
            return;
        }
        let hand = self.curtain_hand();
        for i in 0..self.cohorts.len() {
            // A cohort already retracting froze its target when the hand let go of it; that is the same fact, measured earlier, and it stays.
            if self.cohorts[i].retract_col.is_some() {
                continue;
            }
            let coh = self.cohorts[i];
            let (target, right_first) = Self::target_toward(&coh, hand);
            self.cohorts[i].retract_col = target;
            self.cohorts[i].drain_right_first = right_first;
        }
        self.curtain = Some(at);
    }

    #[inline]
    fn curtain_hand(&self) -> Option<(u16, u16)> {
        let newest = |wake_ok: bool| {
            freshest(self.cohorts.iter().filter(|c| wake_ok || !c.wake)).map(|c| (c.row, c.col1))
        };
        newest(false).or_else(|| newest(true))
    }

    /// Whether a curtain is falling.
    #[inline]
    #[must_use]
    pub const fn curtained(&self) -> bool {
        self.curtain.is_some()
    }

    /// The phrase rest in force, seconds.
    #[inline]
    #[must_use]
    pub const fn rest_s(&self) -> f32 {
        let ioi = if self.ioi_ms > 0.0 {
            self.ioi_ms
        } else {
            IOI_DEFAULT_MS
        };
        phrase_rest_ms(ioi) / 1000.0
    }

    /// The open phrase's id — read-only, for the twins.
    #[must_use]
    #[cfg(test)]
    pub const fn phrase_open(&self) -> Option<u32> {
        self.phrase_open
    }

    #[inline]
    fn note_key(&mut self, at: Instant) {
        let gap_ms = self
            .last_key_at
            .map(|t| (at.saturating_duration_since(t).as_secs_f32() * 1000.0).round());
        let first = gap_ms.is_none();
        let rest = gap_ms.is_none_or(|g| g >= self.rest_s() * 1000.0);
        if rest || self.phrase_open.is_none() {
            self.open_phrase();
        }
        let gap = gap_ms.map_or(0, |g| g.clamp(0.0, f32::from(u16::MAX) * 64.0) as u32);
        let ioi = if self.ioi_ms > 0.0 {
            self.ioi_ms
        } else {
            IOI_DEFAULT_MS
        };
        self.ioi_ms = ioi_next(ioi, gap, first, rest);
        self.last_key_at = Some(at);
    }

    #[inline]
    const fn open_phrase(&mut self) {
        let id = if self.next_phrase > 1 {
            self.next_phrase
        } else {
            1
        };
        let next = id.wrapping_add(1);
        self.next_phrase = if next > 1 { next } else { 1 };
        self.phrase_open = Some(id);
    }

    #[inline]
    const fn phrase_for_lay(&mut self) -> u32 {
        if self.phrase_open.is_none() {
            self.open_phrase();
        }
        match self.phrase_open {
            Some(id) => id,
            None => 1,
        }
    }

    fn hold_phrase(&mut self, at: Instant, row: u16) {
        let rested = self
            .last_key_at
            .is_some_and(|t| at.saturating_duration_since(t).as_secs_f32() >= self.rest_s());
        let open = match self.phrase_open {
            Some(open) if !rested => open,
            _ => {
                self.open_phrase();
                let id = self.phrase_open.unwrap_or(1);
                for coh in &mut self.cohorts {
                    if !coh.abandoned && coh.row == row {
                        coh.phrase = id;
                    }
                }
                id
            }
        };
        self.last_key_at = Some(at);
        self.hold(open, row, None, at);
    }

    fn hold(&mut self, open: u32, hand_row: u16, laid_into: Option<usize>, at: Instant) {
        let grace = self.rest_s();
        let floor = swoosh_life_s(grace);
        let held_by = |i: usize, c: &Cohort| {
            !c.abandoned && c.phrase == open && (c.row == hand_row || laid_into == Some(i))
        };
        let mut held_count = 0usize;
        let mut single_held_id = 0u32;
        for (i, coh) in self.cohorts.iter_mut().enumerate() {
            if held_by(i, coh) {
                held_count += 1;
                single_held_id = coh.id;
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
                coh.grace_s = grace;
                if coh.flow.take().is_some() {
                    coh.flow_drift = 0.0;
                }
            }
        }
        if held_count == 0 {
            return;
        }
        if held_count == 1 {
            for cell in &mut self.cells {
                if cell.cohort == single_held_id && cell.life_s < floor {
                    cell.life_s = floor;
                }
            }
            return;
        }
        let cohorts = &self.cohorts;
        let mut cached_coh: Option<(u32, bool)> = None;
        for cell in &mut self.cells {
            if cell.life_s >= floor {
                continue;
            }
            let held = match cached_coh {
                Some((id, h)) if id == cell.cohort => h,
                _ => {
                    let h = cohorts
                        .iter()
                        .enumerate()
                        .any(|(i, c)| c.id == cell.cohort && held_by(i, c));
                    cached_coh = Some((cell.cohort, h));
                    h
                }
            };
            if held {
                cell.life_s = floor;
            }
        }
    }

    #[inline]
    fn take_row_back(&mut self, open: u32, row: u16, at: Instant) {
        for coh in &mut self.cohorts {
            if !coh.abandoned && coh.row == row && coh.flow.is_some() {
                coh.phrase = open;
            }
        }
        self.hold(open, row, None, at);
    }

    #[inline]
    const fn retract_target(&self, coh: &Cohort) -> (Option<u16>, bool) {
        Self::target_toward(coh, self.caret)
    }

    fn flow_target(&self, coh: &Cohort) -> (Option<u16>, bool, f32) {
        let below = self.caret.is_some_and(|(row, _)| coh.row > row);
        let (lo, hi) = self
            .cells
            .iter()
            .filter(|c| c.row == coh.row && c.cohort == coh.id && !c.leaving())
            .fold((u16::MAX, 0u16), |(lo, hi), c| {
                (lo.min(c.col), hi.max(c.col))
            });
        let (lo, hi) = if lo > hi {
            (coh.col0, coh.col1.saturating_sub(1).max(coh.col0))
        } else {
            (lo, hi)
        };
        let edge = if below { lo } else { hi };
        let disp = self
            .cells
            .iter()
            .filter(|c| c.row == coh.row && c.cohort == coh.id && c.col == edge)
            .max_by(|a, b| a.born.cmp(&b.born))
            .map_or(0.0, |c| c.birth_disp);
        let heat = smoothstep01((disp - HOT_EDGE_DISP_MIN) / HOT_EDGE_DISP_SPAN);
        let kept = f32::from(hi - lo) + 1.0;
        let drift = (FLOW_DRIFT_MIN_CELLS + (FLOW_DRIFT_MAX_CELLS - FLOW_DRIFT_MIN_CELLS) * heat)
            .min(FLOW_DRIFT_MAX_SHARE * kept);
        (Some(edge), below, drift)
    }

    #[inline]
    const fn target_toward(coh: &Cohort, hand: Option<(u16, u16)>) -> (Option<u16>, bool) {
        match hand {
            None => (None, false),
            Some((hrow, hcol)) if coh.row == hrow => (Some(hcol), false),
            Some((hrow, _)) if coh.row < hrow => {
                let sub = coh.col1.saturating_sub(1);
                (Some(if sub > coh.col0 { sub } else { coh.col0 }), false)
            }
            Some(_) => (Some(coh.col0), true),
        }
    }

    #[inline]
    fn erase(&mut self, caret: (u16, u16), cells: u16, span_s: f32, at: Instant) {
        self.retract_suffix(caret.0, caret.1, cells, at, span_s);
        self.pending_erase = self.pending_erase.saturating_add(cells);
        self.hold_phrase(at, caret.0);
    }

    #[inline]
    fn alive_at(&self, cell: &Cell, at: Instant) -> bool {
        !cell.leaving()
            && at
                .saturating_duration_since(live_since(&self.cohorts, cell))
                .as_secs_f32()
                < cell.life_s
    }

    #[inline]
    fn owned(&self, row: u16, col: u16, at: Instant) -> bool {
        self.cells
            .iter()
            .any(|c| c.row == row && c.col == col && self.alive_at(c, at))
    }

    /// A key whose echo has not landed is held, not laid.
    #[inline]
    pub(super) fn hold_typed(
        &mut self,
        ev: &super::Event,
        at: Instant,
        ctx: &Ctx<'_>,
        cell: Option<(u16, u16)>,
    ) {
        if let Event::Typed { class, .. } = *ev {
            self.pending_erase = 0;
            if class == TypedClass::Space
                && let Some(cell) = cell
            {
                self.note_space(cell);
            }
            self.hold_phrase(at, ctx.caret.0);
        }
    }

    fn sweep(&mut self, row: u16, col0: u16, col1: u16, at: Instant, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        for col in col0..col1.min(cols) {
            let own = self
                .cells
                .iter()
                .any(|c| c.row == row && c.col == col && c.born >= at && self.alive_at(c, at));
            if !own {
                self.lay_cell(row, col, at, ctx, true);
            }
        }
    }

    #[inline]
    fn regain_focus(&mut self, at: Instant) {
        if let Some(ember) = self.ember_at.take() {
            let r = at.saturating_duration_since(ember).as_secs_f32() / FOCUS_EMBER_S;
            if r < 1.0 {
                self.rearm = Some((at, spend(clamp01(r))));
            }
        }
    }

    fn lay(&mut self, n: u16, at: Instant, ctx: &Ctx<'_>) {
        if n == 0 || ctx.geom.cols == 0 {
            return;
        }
        let cols = i32::try_from(ctx.geom.cols).unwrap_or(i32::MAX);
        let (pane_col0, pane_cols) = self
            .pane
            .map_or((0, cols), |(c0, w)| (i32::from(c0), i32::from(w).max(1)));
        let pane_col1 = pane_col0.saturating_add(pane_cols).min(cols);
        let (row, col_end) = ctx.caret;
        for back in (1..=i32::from(n)).rev() {
            let mut c = i32::from(col_end) - back;
            let mut r = i32::from(row);
            while c < pane_col0 {
                r -= 1;
                c += pane_cols;
            }
            if r < 0 || c >= pane_col1 {
                continue;
            }
            let (Ok(r), Ok(c)) = (u16::try_from(r), u16::try_from(c)) else {
                continue;
            };
            self.lay_cell(r, c, at, ctx, true);
        }
    }

    #[inline]
    const fn birth_price(ctx: &Ctx<'_>) -> f32 {
        clamp01(match ctx.mend {
            Some(m) => ctx.birth_disp.max(m.disp),
            None => ctx.birth_disp,
        })
    }

    #[inline]
    fn cell_life(&self, at: Instant, ctx: &Ctx<'_>) -> f32 {
        let full = ctx.cfg.duration.as_secs_f32().max(0.001);
        let base = (full * LIFE_DURATION_GAIN).clamp(LIFE_BASE_MIN, LIFE_BASE_MAX);
        let d = Self::birth_price(ctx);
        let life = base * (1.0 + LIFE_SPINE_LINEAR * d + LIFE_SPINE_SQUARE * d * d);
        let life = if let Some(newest) = self.cohorts.iter().map(|c| c.alive_at).max() {
            let gap = at.saturating_duration_since(newest).as_secs_f32();
            if gap <= CHAIN_GAP_MAX {
                life.max((CHAIN_KEYS * gap + CHAIN_MARGIN).min(CHAIN_LIFE_MAX))
            } else {
                life
            }
        } else {
            life
        };
        life.max(swoosh_life_s(self.rest_s()))
    }

    #[inline]
    #[must_use]
    const fn hand_changed_row(from: (u16, u16), to: (u16, u16)) -> bool {
        if to.0 == from.0.saturating_add(1) {
            return true;
        }
        from.0 == to.0.saturating_add(1) && to.1 > from.1
    }

    #[inline]
    #[must_use]
    fn insert_landing(&self, landing: (u16, u16), cells: u16) -> (u16, u16) {
        if cells == 0 {
            return landing;
        }
        let (row, end) = landing;
        let first = end.saturating_sub(cells);
        for &(r, lo, hi, dc) in &self.shifted_now {
            let Ok(dc) = u16::try_from(dc) else {
                continue;
            };
            if r != row || cells > dc {
                continue;
            }
            let (moved0, moved1) = (lo.saturating_add(dc), hi.saturating_add(dc));
            if first >= moved0 && end.saturating_sub(1) <= moved1 {
                return (row, lo.saturating_add(dc));
            }
        }
        landing
    }

    #[must_use]
    fn typed_landing(&self, caret: (u16, u16), cells: u16, cols: u16) -> (u16, u16) {
        let Some(pen) = self.pen else {
            return caret;
        };
        if cells == 0 {
            return caret;
        }
        if caret.0 >= pen.0 {
            let top = self.fold_top_row(caret, cells, cols);
            if top == i32::from(caret.0) || top == i32::from(pen.0) {
                return caret;
            }
        } else if Self::hand_changed_row(pen, caret) {
            return caret;
        }
        if !self.cells.iter().any(|c| c.row == pen.0) {
            return caret;
        }
        (pen.0, pen.1.saturating_add(cells.max(1)))
    }

    #[inline]
    #[must_use]
    const fn fold_pane(&self, cols: u16) -> (i32, i32) {
        let (c0, w) = match self.pane {
            Some(p) => p,
            None => (0, cols),
        };
        (c0 as i32, if w > 1 { w as i32 } else { 1 })
    }

    #[inline]
    #[must_use]
    const fn fold_top_row(&self, caret: (u16, u16), cells: u16, cols: u16) -> i32 {
        let (pane_col0, pane_cols) = self.fold_pane(cols);
        let left = caret.1 as i32 - cells as i32 - pane_col0;
        let d = left.div_euclid(pane_cols);
        caret.0 as i32 + if d < 0 { d } else { 0 }
    }

    #[inline]
    #[must_use]
    const fn folded_down(from: (u16, u16), to: (u16, u16), licence: Licence) -> bool {
        licence.is_echo() && to.0 == from.0.saturating_add(1)
    }

    fn leave_row(&mut self, from: (u16, u16), to: (u16, u16), at: Instant, licence: Licence) {
        let soft_wrap = self
            .soft_wrap
            .filter(|sw| sw.origin == from && sw.landing == to);
        self.carry = None;
        if let Some(sw) = soft_wrap
            && Self::folded_down(from, to, licence)
        {
            self.arm_seam(to.0, at);
            self.flow_row(from.0, at);
            self.pending_erase = 0;
            self.last_space = None;
            self.prev_space = None;
            self.arrived = None;
            self.relocate = None;
            self.carry = Some((sw, at));
            return;
        }
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let rewrap = self
            .rewrap_start
            .filter(|rewrap| rewrap.from == from && rewrap.to == to);
        let w = rewrap
            .and_then(|rewrap| rewrap.width)
            .unwrap_or_else(|| self.moved_word_len(from, at));
        let held_after = self.space_held_after_the_wrap(from);
        let re_wrapped = licence == Licence::Typed && to.1 > pane_col0.saturating_add(1);
        let origin = if re_wrapped {
            self.relay_origin(from, w)
        } else {
            None
        };
        if re_wrapped {
            self.split_off = None;
        }
        if Self::folded_down(from, to, licence) {
            self.arm_seam(to.0, at);
            if re_wrapped && w > 0 {
                let moved = (w + 1).min(from.1.saturating_sub(pane_col0));
                self.retract_span(from.0, from.1 - moved, moved, at, RETRACT_FADE_S);
            }
            self.flow_row(from.0, at);
        } else {
            // A line the hand left downward by anything but a navigation (a Return, the line
            // feed after it) is printed text now: its light dims to [`RETURN_PRINTED_SHARE`]
            // over [`RETURN_DIM_S`], clear of its letters by the time the output lands (at full
            // light the band under its last word holds it under 4.5:1 on paper), and goes on
            // out through the swoosh's retract.
            if to.0 > from.0 && licence != Licence::Nav {
                self.print_row(from.0, at);
            }
            self.abandon_row(from.0, at, Some(from.1));
            if licence == Licence::Typed
                && self.pending_erase > 0
                && to.0.checked_add(1) == Some(from.0)
                && let Some(open) = self.phrase_open
            {
                self.take_row_back(open, to.0, at);
            }
        }
        self.pending_erase = 0;
        self.reseat_spaces(held_after, to);
        self.arrived = None;
        self.relocate = None;
        if re_wrapped {
            let rewrap = self.take_rewrap_start(from, to);
            if let Some(rewrap) = rewrap
                && rewrap.start.checked_add(w).is_some_and(|end| end < to.1)
            {
                self.relay_word(to.0, rewrap.start, rewrap.start + w, at, origin);
                if let Some(space) = rewrap.space {
                    self.note_space((to.0, space));
                }
            } else {
                self.begin_relocate(to, w, at, origin);
            }
        }
    }

    #[inline]
    fn flow_row(&mut self, row: u16, at: Instant) {
        for coh in &mut self.cohorts {
            if coh.row == row {
                Self::flow_cohort(coh, at);
            }
        }
    }

    #[inline]
    fn flow_cohort(coh: &mut Cohort, at: Instant) {
        if coh.abandoned || coh.wake || coh.flow.is_some() {
            return;
        }
        if !matches!(
            Phase::at(
                at.saturating_duration_since(coh.alive_at).as_secs_f32(),
                coh.grace_s
            ),
            Some(Phase::Laying | Phase::Grace | Phase::Reaching)
        ) {
            return;
        }
        coh.flow = Some(at);
        coh.release_clock = None;
    }

    #[inline]
    #[must_use]
    fn relay_origin(&self, from: (u16, u16), w: u16) -> Option<WalkPos> {
        if w == 0 {
            return None;
        }
        let last = from.1.checked_sub(1)?;
        let first = from.1.checked_sub(w)?;
        if let Some(owner) = self
            .cells
            .iter()
            .rev()
            .find(|c| c.row == from.0 && c.col == last)
        {
            let coh = self.cohorts.iter().find(|c| c.id == owner.cohort)?;
            return Some(coh.walk_at(first));
        }
        let split = self.split_off?;
        if !(split.col0..split.col1).contains(&last) {
            return None;
        }
        let copy = self.cohorts.iter().find(|c| c.id == split.copy)?;
        (copy.row.checked_add_signed(split.dr.checked_neg()?) == Some(from.0))
            .then(|| copy.walk_at(first))
    }

    fn re_anchor(&mut self, from: (u16, u16), to: (u16, u16), at: Instant) {
        let n = from.1 - to.1;
        let rewrap = self.take_rewrap_start(from, to);
        let w = rewrap
            .and_then(|rewrap| rewrap.width)
            .unwrap_or_else(|| self.moved_word_len(from, at));
        let held_after = self.space_held_after_the_wrap(from);
        let origin = self.relay_origin(from, w);
        let start = rewrap.map(|rewrap| rewrap.start);
        self.split_off = None;
        self.retract_suffix(to.0, to.1, n, at, RETRACT_DUR_S);
        self.reseat_spaces(held_after, to);
        if let Some(space) = rewrap.and_then(|rewrap| rewrap.space) {
            self.note_space((to.0, space));
        }
        self.arrived = None;
        let hand_floor = self
            .hand_floor_on(to.0)
            .map(|floor| start.map_or(floor, |s| floor.min(s)));
        let relays_off_the_hand_s_ink =
            hand_floor.is_some_and(|floor| to.1.saturating_sub(1).saturating_sub(w) < floor);
        if relays_off_the_hand_s_ink {
            return;
        }
        if let (Some(start), Some(glyph)) = (start, to.1.checked_sub(1))
            && start <= glyph.saturating_sub(w)
            && start < glyph
            && hand_floor.is_none_or(|floor| start >= floor)
        {
            self.relocate = None;
            self.relay_word(to.0, start, to.1, at, origin);
            return;
        }
        self.begin_relocate(to, w, at, origin);
    }

    /// The content witness says the text this typed rewrap relays starts at `start` on its landing row.
    #[inline]
    pub(super) const fn note_rewrap_start(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        start: u16,
        width: Option<u16>,
        space: Option<u16>,
    ) {
        // The follow pass may also sample an earlier buffered move.
        if width.is_none() && matches!(self.rewrap_start, Some(RewrapStart { width: Some(_), .. }))
        {
            return;
        }
        self.rewrap_start = Some(RewrapStart {
            from,
            to,
            start,
            width,
            space,
        });
    }

    #[inline]
    const fn take_rewrap_start(&mut self, from: (u16, u16), to: (u16, u16)) -> Option<RewrapStart> {
        let Some(rewrap) = self.rewrap_start else {
            return None;
        };
        if rewrap.from.0 != from.0
            || rewrap.from.1 != from.1
            || rewrap.to.0 != to.0
            || rewrap.to.1 != to.1
        {
            return None;
        }
        self.rewrap_start = None;
        Some(rewrap)
    }

    /// Correct a Space's cell from an exact ordered key batch and its licensed same-row sweep.
    #[inline]
    pub(super) const fn observe_space_cell(&mut self, row: u16, col: u16) {
        self.note_space((row, col));
    }

    #[inline]
    const fn note_space(&mut self, cell: (u16, u16)) {
        if let Some((r, c)) = self.last_space {
            if r != cell.0 || c != cell.1 {
                self.prev_space = self.last_space;
            }
        } else {
            self.prev_space = None;
        }
        self.last_space = Some(cell);
    }

    #[inline]
    const fn word_space(&self, from: (u16, u16)) -> Option<(u16, u16)> {
        match self.last_space {
            Some(cell) if cell.0 == from.0 && cell.1 == from.1 => self.prev_space,
            other => other,
        }
    }

    #[inline]
    const fn space_held_after_the_wrap(&self, from: (u16, u16)) -> bool {
        matches!(self.last_space, Some((r, c)) if r == from.0 && c == from.1)
    }

    #[inline]
    const fn reseat_spaces(&mut self, held_after: bool, to: (u16, u16)) {
        self.prev_space = None;
        self.last_space = if held_after { Some(to) } else { None };
    }

    /// Replay the `Move` the seam judged a soft-wrapped caret ([`SoftWrap`]).
    #[inline]
    pub(super) fn soft_wrap_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, sw: SoftWrap) {
        self.soft_wrap = Some(sw);
        self.on_event(ev, at, ctx);
        self.soft_wrap = None;
    }

    /// Replay the `Move` the seam judged a lifted word ([`Lift`]).
    #[inline]
    pub(super) fn lift_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, lift: Lift) {
        self.lift = Some(lift);
        self.on_event(ev, at, ctx);
        self.lift = None;
    }

    /// Replay the `Move` whose echo the seam saw carry the soft-wrapped word `sw` off its row ([`Ribbon::settle_carry`]).
    #[inline]
    pub(super) fn reflow_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, sw: SoftWrap) {
        self.reflow = Some(sw);
        self.on_event(ev, at, ctx);
        self.reflow = None;
    }

    /// Replay the `Sweep` the seam named the revive of a retired typed run ([`Revive`]).
    #[inline]
    pub(super) fn revive_sweep(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, rv: Revive) {
        self.revive = Some(rv);
        self.on_event(ev, at, ctx);
        self.revive = None;
    }

    /// The soft-wrapped word waiting for the key that reflows it, if any.
    #[inline]
    #[must_use]
    pub(super) const fn carry(&self) -> Option<SoftWrap> {
        match self.carry {
            Some((sw, _)) => Some(sw),
            None => None,
        }
    }

    fn lift_word(&mut self, lift: Lift, at: Instant) {
        let (row, end) = lift.origin;
        let col0 = lift.landing.1;
        self.last_space = None;
        self.prev_space = None;
        self.arrived = None;
        self.relocate = None;
        let Some(lit0) = (col0..end)
            .rev()
            .take_while(|&col| self.owned(row, col, at))
            .last()
        else {
            return;
        };
        let lit = end - lit0;
        let t0 = self.relay_origin((row, end), lit);
        self.retract_span(row, lit0, lit, at, RETRACT_FADE_S);
        let (dst_row, dst0) = lift.dst;
        self.relay_word(
            dst_row,
            dst0.saturating_add(lit0 - col0),
            dst0.saturating_add(end - col0),
            at,
            t0,
        );
        self.flow_row(dst_row, at);
    }

    fn settle_carry(&mut self, row: u16, from_col: u16, to_col: u16, at: Instant) {
        let Some((sw, wrapped_at)) = self.carry else {
            return;
        };
        let (landing_row, landing) = sw.landing;
        if landing_row != row || from_col != landing {
            return;
        }
        self.carry = None;
        let (src_row, src_end) = (sw.origin.0, sw.origin.1.saturating_add(1));
        let n = src_end.saturating_sub(sw.word_col0);
        if n == 0
            || self.reflow != Some(sw)
            || at.saturating_duration_since(wrapped_at).as_secs_f32() > RELOCATE_PATIENCE_S
            || to_col <= landing.saturating_add(n)
        {
            return;
        }
        let Some(lit0) = (sw.word_col0..src_end)
            .rev()
            .take_while(|&col| self.owned(src_row, col, at))
            .last()
        else {
            return;
        };
        let lit = src_end - lit0;
        let t0 = self.relay_origin((src_row, src_end), lit);
        self.retract_span(src_row, lit0, lit, at, RETRACT_FADE_S);
        let dst0 = landing + (lit0 - sw.word_col0);
        self.relay_word(row, dst0, to_col.min(landing + n + 1), at, t0);
    }

    /// Cells of the word the app moved off `from`'s row.
    #[inline]
    pub(crate) fn moved_word_len(&self, from: (u16, u16), at: Instant) -> u16 {
        let after_space = match self.word_space(from) {
            Some((row, s)) if row == from.0 && s < from.1 => Some(s + 1),
            _ => None,
        };
        let arrived = self
            .arrived
            .filter(|&(row, col)| row == from.0 && col <= from.1)
            .map(|(row, col)| {
                (0..col)
                    .rev()
                    .take_while(|&c| self.typed_live(row, c, at))
                    .last()
                    .unwrap_or(col)
            });
        after_space.max(arrived).map_or(0, |start| from.1 - start)
    }

    #[inline]
    fn typed_live(&self, row: u16, col: u16, at: Instant) -> bool {
        self.cells
            .iter()
            .any(|c| c.row == row && c.col == col && c.typing && self.alive_at(c, at))
    }

    /// Resident typed suffix immediately before `from`, for a content proof whose preceding Space was program text rather than a typed event.
    #[inline]
    pub(crate) fn typed_suffix_len(&self, from: (u16, u16), at: Instant) -> u16 {
        (0..from.1)
            .rev()
            .take_while(|&col| self.typed_live(from.0, col, at))
            .last()
            .map_or(0, |start| from.1 - start)
    }

    fn begin_relocate(&mut self, to: (u16, u16), w: u16, at: Instant, walk: Option<WalkPos>) {
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let (row, landing) = to;
        let Some(glyph) = landing.checked_sub(1) else {
            return;
        };
        if w == 0 {
            return;
        }
        if glyph >= w && glyph - w >= pane_col0 {
            self.relay_word(row, glyph - w, glyph, at, walk);
            return;
        }
        self.cells
            .retain(|c| !(c.row == row && c.col < landing && c.born >= at && c.typing));
        self.relocate = Some(Relocate { row, w, at, walk });
    }

    fn settle_relocate(&mut self, row: u16, from_col: u16, at: Instant, ctx: &Ctx<'_>) {
        let Some(rel) = self.relocate else {
            return;
        };
        if rel.row != row {
            return;
        }
        if at.saturating_duration_since(rel.at).as_secs_f32() > RELOCATE_PATIENCE_S {
            self.relocate = None;
            return;
        }
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let Some(glyph) = from_col.checked_sub(1) else {
            return;
        };
        if glyph < rel.w || glyph - rel.w < pane_col0 {
            return;
        }
        let _ = ctx;
        self.relay_word(row, glyph - rel.w, from_col, at, rel.walk);
        self.relocate = None;
    }

    fn relay_word(&mut self, row: u16, col0: u16, col1: u16, at: Instant, walk: Option<WalkPos>) {
        let mut first = true;
        let mut minted: Option<u32> = None;
        let mut cached_like: Option<(u32, (f32, f32, f32, f32))> = None;
        for col in col0..col1 {
            if self.owned(row, col, at) {
                continue;
            }
            let own = minted.and_then(|id| self.cohorts.iter().position(|c| c.id == id));
            let idx = match (walk, own) {
                (_, Some(i)) => i,
                (Some(walk), None) if first => {
                    let i = self.mint_cohort(row, col, col0, walk, at, false);
                    minted = Some(self.cohorts[i].id);
                    i
                }
                _ => self.join_cohort(row, col, at, false),
            };
            first = false;
            let cohort = self.cohorts[idx];
            let (life_s, cov0, birth_disp, edge_cells) = match cached_like {
                Some((id, pricing)) if id == cohort.id => pricing,
                _ => {
                    let pricing = self
                        .cells
                        .iter()
                        .filter(|c| c.row == row && c.cohort == cohort.id && c.typing)
                        .max_by(|a, b| a.born.cmp(&b.born))
                        .map_or((SWOOSH_LIFE_S, BODY_COLD_SHARE, 0.0, HOT_EDGE_CELLS), |c| {
                            (c.life_s, c.cov0, c.birth_disp, c.edge_cells)
                        });
                    cached_like = Some((cohort.id, pricing));
                    pricing
                }
            };
            let cell = Cell {
                row,
                col,
                cohort: cohort.id,
                t: cohort.t_at(col),
                born: at,
                attack_at: at,
                life_s,
                cov0,
                typing: true,
                retract_at: None,
                retire_at: None,
                birth_disp,
                edge_cells,
                layer: Layer::Base,
                rearm: None,
                released_at: None,
            };
            self.place(idx, cell, at, row);
        }
    }

    fn scrub(&mut self, row: u16, from: u16, to: u16, at: Instant, ctx: &Ctx<'_>) -> bool {
        let (lo, hi) = (from.min(to), from.max(to));
        if lo == hi {
            return false;
        }
        let owner = |cells: &[Cell], col: u16| {
            cells
                .iter()
                .rposition(|c| c.row == row && c.col == col && !c.leaving())
        };
        let reduced = ctx.cfg.reduced_motion;
        let mut last_coh: Option<(u32, Instant)> = None;
        for col in lo..hi {
            let Some(i) = owner(&self.cells, col) else {
                return false;
            };
            let cell = self.cells[i];
            if self.env_of(ctx, &cell) <= 0.0 {
                return false;
            }
            let alive_at = match last_coh {
                Some((id, alive_at)) if id == cell.cohort => alive_at,
                _ => {
                    let Some(coh) = self.cohorts.iter().find(|k| k.id == cell.cohort) else {
                        return false;
                    };
                    if coh.leaving_at(at) {
                        return false;
                    }
                    last_coh = Some((coh.id, coh.alive_at));
                    coh.alive_at
                }
            };
            let spent = at
                .saturating_duration_since(cell.born.max(alive_at))
                .as_secs_f32();
            let melt_opens = if reduced {
                (cell.life_s - REDUCED_MOTION_FADE_MS / 1000.0).max(0.0)
            } else {
                cell.life_s * (1.0 - EXPIRY_MELT_SHARE)
            };
            if spent > melt_opens {
                return false;
            }
        }
        let mut last_held: Option<u32> = None;
        for col in lo..hi {
            let Some(i) = owner(&self.cells, col) else {
                continue;
            };
            let id = self.cells[i].cohort;
            if last_held == Some(id) {
                continue;
            }
            if let Some(coh) = self.cohorts.iter_mut().find(|k| k.id == id) {
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
                last_held = Some(id);
            }
        }
        true
    }

    fn abandon_row(&mut self, row: u16, at: Instant, toward: Option<u16>) {
        if self.seam.is_some_and(|s| s.row == row) {
            self.seam = None;
        }
        for coh in &mut self.cohorts {
            if coh.row != row {
                continue;
            }
            coh.revoke_releases(at);
            if coh.abandoned {
                continue;
            }
            let Some(rewound) =
                at.checked_sub(std::time::Duration::from_secs_f32(coh.retract_start()))
            else {
                continue;
            };
            if coh.retract_col.is_none() {
                coh.retract_col = toward;
            }
            coh.abandoned = true;
            coh.rejoinable = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
        }
    }

    #[inline]
    fn print_row(&mut self, row: u16, at: Instant) {
        for coh in &mut self.cohorts {
            if coh.row == row && coh.printed.is_none() {
                coh.printed = Some(at);
            }
        }
    }

    #[inline]
    const fn touch_hand(&mut self, row: u16, col: u16) {
        self.hand_floor = Some(match self.hand_floor {
            Some((r, c)) if r == row => (row, if c < col { c } else { col }),
            _ => (row, col),
        });
    }

    #[inline]
    const fn hand_floor_on(&self, row: u16) -> Option<u16> {
        match self.hand_floor {
            Some((r, c)) if r == row => Some(c),
            _ => None,
        }
    }

    /// The hand's floor ([`Ribbon::hand_floor`]) as `(row, col)`, or `None` when no row has seen the hand yet.
    #[inline]
    pub(super) const fn hand_floor_cell(&self) -> Option<(u16, u16)> {
        self.hand_floor
    }

    fn lay_cell(&mut self, row: u16, col: u16, at: Instant, ctx: &Ctx<'_>, typing: bool) {
        // The hand's own ink is the floor ([`Ribbon::hand_floor`]).
        if typing {
            self.touch_hand(row, col);
        }
        let life_s = self.cell_life(at, ctx);
        let birth_disp = Self::birth_price(ctx);
        let cov0 = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        let reviving = self.revive.is_some_and(|rv| rv.covers(row, col));
        let gate = self
            .fresh_line
            .filter(|gate| !reviving && gate.takes(row, at));
        let idx = self.join_cohort(row, col, at, gate.is_some());
        if let Some(gate) = gate {
            let joined = self.cohorts[idx];
            let re_laid = joined.born < gate.since
                && self
                    .cells
                    .iter()
                    .any(|c| c.row == row && c.col == col && c.cohort == joined.id);
            if !re_laid {
                self.fresh_line = None;
            }
        }
        let idx = if typing {
            self.fold_wakes_into_band(idx, row, col, at)
        } else {
            idx
        };
        let cohort = self.cohorts[idx];
        let inherited = typing && self.attach == Some((row, col));
        let attack_at = if inherited {
            at.checked_sub(std::time::Duration::from_secs_f32(
                EDGE_IN_S + ATTACK_WIPE_S,
            ))
            .unwrap_or(at)
        } else {
            at
        };
        let cell = Cell {
            row,
            col,
            cohort: cohort.id,
            t: cohort.t_at(col),
            born: at,
            attack_at,
            life_s,
            cov0,
            typing,
            retract_at: None,
            retire_at: None,
            birth_disp,
            edge_cells: edge_cells(ctx.surge, ctx.flow.heat),
            layer: Layer::Base,
            rearm: None,
            released_at: None,
        };
        self.place(idx, cell, at, ctx.caret.0);
        self.seat_fold_successor(idx, col, ctx);
    }

    fn seat_fold_successor(&mut self, idx: usize, col: u16, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        let (pane_col0, pane_col1) = self
            .pane
            .map_or((0, cols), |(c0, w)| (c0, c0.saturating_add(w.max(1))));
        if col.saturating_add(1) != pane_col1 {
            return;
        }
        let a = self.cohorts[idx];
        let Some(below) = a.row.checked_add(1) else {
            return;
        };
        let stale = a.walk_at(col);
        for bi in 0..self.cohorts.len() {
            let b = self.cohorts[bi];
            if b.id == a.id
                || b.row != below
                || b.anchor_col < pane_col0
                || b.anchor_col >= pane_col1
                || b.origin_walk() != stale
            {
                continue;
            }
            let seat = a.walk_at(pane_col1.saturating_add(b.anchor_col - pane_col0));
            let coh = &mut self.cohorts[bi];
            coh.t0 = seat.t;
            coh.d0 = seat.d;
            let origin = *coh;
            for c in &mut self.cells {
                if c.cohort == origin.id {
                    c.t = origin.t_at(c.col);
                }
            }
        }
    }

    fn place(&mut self, idx: usize, mut cell: Cell, at: Instant, hand_row: u16) {
        let (row, col, typing) = (cell.row, cell.col, cell.typing);
        if (self.cohorts[idx].col0..self.cohorts[idx].col1).contains(&col)
            && let Some(slot) = self
                .cells
                .iter()
                .position(|c| c.row == row && c.col == col && c.cohort == cell.cohort)
        {
            let old = self.cells.remove(slot);
            self.custody_replaced(idx, old.released_at, 1);
            let alive = self.alive_at(&old, at);
            if alive && old.typing == typing {
                cell.attack_at = old.attack_at.min(cell.attack_at);
                cell.cov0 = cell.cov0.max(old.cov0);
                cell.life_s = cell.life_s.max(old.life_s);
            }
        }
        self.cells.push(cell);
        let coh = &mut self.cohorts[idx];
        coh.col0 = coh.col0.min(col);
        coh.col1 = coh.col1.max(col + 1);
        if typing {
            coh.wake = false;
            if coh.flow.take().is_some() {
                coh.retract_col = None;
                coh.flow_drift = 0.0;
            }
            let open = self.phrase_for_lay();
            self.cohorts[idx].phrase = open;
            self.hold(open, hand_row, Some(idx), at);
        } else {
            coh.alive_at = coh.alive_at.max(at);
            coh.phase = Phase::Laying;
            coh.retract_col = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn wake(
        &mut self,
        row: u16,
        from: u16,
        to: u16,
        at: Instant,
        ctx: &Ctx<'_>,
        life_s: f32,
        takeover: bool,
    ) -> Option<u32> {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        if cols == 0 || from == to || to >= cols {
            return None;
        }
        let leftward = to < from;
        let far = from.min(cols - 1);
        let span = far.abs_diff(to);
        let flight_s = if takeover {
            flight_ms(f32::from(span))
        } else {
            FLIGHT_MIN_MS
        } / 1000.0;
        let n = f32::from(span).max(1.0);
        let birth_disp = clamp01(ctx.birth_disp);
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        let (anchor_col, walk) = self.wake_origin(row, to, at);
        let minted = takeover.then(|| self.own_wake_cohort(row, to, from, anchor_col, walk, at));
        let reach = (WAKE_MAX_CELLS..=span)
            .rev()
            .find(|&k| {
                let col = if leftward { to + k } else { to - k };
                self.corridor_keeps_glass_light(ctx, row, col, at)
            })
            .unwrap_or(0);

        for k in (0..=span).rev() {
            let col = if leftward { to + k } else { to - k };
            let j = span - k;
            let born_new = at
                .checked_add(std::time::Duration::from_secs_f32(
                    flight_s * f32::from(j) / n,
                ))
                .unwrap_or(at);
            let (over, base_cov, base_leaving) = self.corridor_column(ctx, row, col, at);
            let over_live = over.filter(|&i| {
                let c = self.cells[i];
                !c.leaving() && self.env_of(ctx, &c) > 0.0
            });
            let base_live = base_cov > 0.0 && !base_leaving;
            if !takeover && (base_live || over_live.is_some()) {
                continue;
            }
            let (born, attack_at, cov0, rearm, adopted) = match over {
                Some(i) => {
                    let old = self.cells[i];
                    let level = clamp01(self.env_of(ctx, &old));
                    self.cells.remove(i);
                    (
                        old.born,
                        old.attack_at,
                        old.cov0,
                        Some((at, level)),
                        Some(old.cohort),
                    )
                }
                None => {
                    if k >= WAKE_MAX_CELLS && k > reach && !Self::reads_as_glass_light(base_cov) {
                        continue;
                    }
                    if base_cov > 0.0 {
                        let cov0 = cov0_new.max(base_cov);
                        (
                            born_new,
                            born_new,
                            cov0,
                            Some((born_new, clamp01(base_cov / cov0))),
                            None,
                        )
                    } else {
                        (born_new, born_new, cov0_new, None, None)
                    }
                }
            };
            let idx = match minted {
                Some(idx) => idx,
                None => self.join_wake_cohort(row, col, anchor_col, walk, at),
            };
            let cohort = self.cohorts[idx];
            if let Some(old) = adopted {
                self.copy_hold(old, cohort.id, row);
            }
            let cell = Cell {
                row,
                col,
                cohort: cohort.id,
                t: cohort.t_at(col),
                born,
                attack_at,
                life_s,
                cov0,
                typing: false,
                retract_at: None,
                retire_at: None,
                birth_disp,
                edge_cells: HOT_EDGE_CELLS,
                layer: Layer::Over,
                rearm,
                released_at: None,
            };
            self.place(idx, cell, at, row);
        }
        minted.map(|idx| self.cohorts[idx].id)
    }

    #[inline]
    fn corridor_column(
        &self,
        ctx: &Ctx<'_>,
        row: u16,
        col: u16,
        at: Instant,
    ) -> (Option<usize>, f32, bool) {
        let mut over = None;
        let mut base = None;
        for (i, c) in self.cells.iter().enumerate().rev() {
            if c.row != row || c.col != col {
                continue;
            }
            match c.layer {
                Layer::Over if over.is_none() => {
                    over = Some(i);
                    if base.is_some() {
                        break;
                    }
                }
                Layer::Base if base.is_none() => {
                    base = Some(c);
                    if over.is_some() {
                        break;
                    }
                }
                _ => {}
            }
        }
        let (base_cov, base_leaving) = base.map_or((0.0, false), |c| {
            let env = if c.leaving() {
                0.0
            } else {
                self.env_of(ctx, c)
            };
            let leaving = self
                .cohorts
                .iter()
                .find(|k| k.id == c.cohort)
                .is_none_or(|k| k.leaving_at(at));
            (c.cov0 * env, leaving)
        });
        (over, base_cov, base_leaving)
    }

    #[inline]
    fn corridor_keeps_glass_light(&self, ctx: &Ctx<'_>, row: u16, col: u16, at: Instant) -> bool {
        let (over, base_cov, _) = self.corridor_column(ctx, row, col, at);
        over.is_some() || Self::reads_as_glass_light(base_cov)
    }

    #[inline]
    const fn reads_as_glass_light(share: f32) -> bool {
        UNDER_COV_CAP * share >= STATUS_LIT_COV as f32
    }

    fn own_wake_cohort(
        &mut self,
        row: u16,
        to: u16,
        from: u16,
        anchor_col: u16,
        walk: WalkPos,
        at: Instant,
    ) -> usize {
        let (lo, hi) = (to.min(from), to.max(from));
        let live = self.cohorts.iter().enumerate().find(|(_, c)| {
            c.row == row
                && c.wake
                && !c.abandoned
                && c.col0 <= hi
                && lo < c.col1
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= WAKE_LIFE_S
        });
        if let Some((i, _)) = live {
            let coh = &mut self.cohorts[i];
            coh.alive_at = at;
            coh.phase = Phase::Laying;
            coh.retract_col = None;
            return i;
        }
        self.mint_cohort(row, to, anchor_col, walk, at, true)
    }

    #[inline]
    fn wake_origin(&self, row: u16, landing: u16, at: Instant) -> (u16, WalkPos) {
        let fresh =
            |c: &&Cohort| at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX;
        if let Some(c) = freshest(self.cohorts.iter().filter(fresh).filter(|c| c.row == row)) {
            return (c.anchor_col, c.origin_walk());
        }
        let walk = freshest(self.cohorts.iter().filter(fresh))
            .map_or(WalkPos::COLD, |c| c.walk_at(c.col1));
        (landing, walk.restarted())
    }

    #[inline]
    fn join_wake_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        walk: WalkPos,
        at: Instant,
    ) -> usize {
        if let Some(i) = self.cohorts.iter().position(|c| {
            c.row == row
                && c.wake
                && !c.abandoned
                && col + 1 >= c.col0
                && col <= c.col1
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX
        }) {
            return i;
        }
        self.mint_cohort(row, col, anchor_col, walk, at, true)
    }

    #[inline]
    fn mint_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        walk: WalkPos,
        at: Instant,
        wake: bool,
    ) -> usize {
        let id = self.next_cohort;
        self.next_cohort = self.next_cohort.wrapping_add(1);
        let grace_s = if wake {
            PHRASE_REST_MIN_S
        } else {
            self.rest_s()
        };
        self.cohorts.push(Cohort {
            id,
            row,
            col0: col,
            col1: col + 1,
            anchor_col,
            t0: walk.t,
            d0: walk.d,
            born: at,
            alive_at: at,
            abandoned: false,
            wake,
            retract_col: None,
            drain_right_first: false,
            phase: Phase::Laying,
            rejoinable: false,
            phrase: 0,
            grace_s,
            flow: None,
            flow_drift: 0.0,
            release_clock: None,
            release_parts: [None; RELEASE_PARTS],
            parts_revoked_at: None,
            line_killed: false,
            printed: None,
        });
        // A fresh identity cannot borrow the row's previous occupant, even
        // when it is minted before the first content move in this batch.
        self.stage_hold(id, None);
        self.cohorts.len() - 1
    }

    fn join_cohort(&mut self, row: u16, col: u16, at: Instant, new_line: bool) -> usize {
        let mut rejoin = None;
        for (i, c) in self.cohorts.iter().enumerate() {
            if c.row == row
                && col + 1 >= c.col0
                && col <= c.col1
                && (!c.abandoned || (rejoin.is_none() && c.rejoinable))
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX
                && self.stands(c)
            {
                if !c.abandoned {
                    return i;
                }
                rejoin = Some(i);
            }
        }
        if let Some(i) = rejoin {
            let coh = &mut self.cohorts[i];
            coh.abandoned = false;
            coh.retract_col = None;
            coh.printed = None;
            return i;
        }
        if let Some(rv) = self.revive.filter(|rv| rv.covers(row, col)) {
            return self.mint_cohort(row, col, rv.origin.anchor_col, rv.origin.walk(), at, false);
        }
        let fresh = |t: Instant| at.saturating_duration_since(t).as_secs_f32() <= CHAIN_GAP_MAX;
        let continued = freshest(self.cohorts.iter().filter(|c| fresh(c.alive_at)));
        let walk = continued
            .map(|c| {
                if c.row == row && !new_line && self.stands(c) {
                    c.walk_at(col)
                } else {
                    c.walk_at(c.col1)
                }
            })
            .or_else(|| {
                // A finished swoosh's walk continues too, inside the chain window (`retire` drops the cohort with its cells).
                self.last_walk.filter(|w| fresh(w.alive_at)).map(|w| w.end)
            })
            .unwrap_or(WalkPos::COLD);
        let walk = if new_line { walk.restarted() } else { walk };
        self.mint_cohort(row, col, col, walk, at, false)
    }

    #[inline]
    const fn stands(&self, coh: &Cohort) -> bool {
        let cells = self.cells.as_slice();
        let mut i = 0;
        while i < cells.len() {
            let c = &cells[i];
            if c.cohort == coh.id
                && c.row == coh.row
                && c.retire_at.is_none()
                && (c.retract_at.is_none() || !coh.line_killed)
            {
                return true;
            }
            i += 1;
        }
        false
    }

    #[inline]
    fn mark_line_killed(&mut self, row: u16) {
        for k in 0..self.cohorts.len() {
            let coh = self.cohorts[k];
            if coh.row == row
                && !self
                    .cells
                    .iter()
                    .any(|c| c.cohort == coh.id && c.row == row && !c.leaving())
            {
                self.cohorts[k].line_killed = true;
            }
        }
    }

    fn fold_wakes_into_band(&mut self, idx: usize, row: u16, col: u16, at: Instant) -> usize {
        let fresh = |t: Instant| at.saturating_duration_since(t).as_secs_f32() <= CHAIN_GAP_MAX;
        let joined = self.cohorts[idx];
        let band_id = if joined.wake {
            let Some(j) = self.cohorts.iter().position(|c| {
                c.id != joined.id
                    && c.row == row
                    && !c.wake
                    && !c.abandoned
                    && c.retract_col.is_none()
                    && !matches!(c.phase, Phase::Reaching | Phase::Retracting | Phase::Fading)
                    && fresh(c.alive_at)
                    && c.col1 >= joined.col0
                    && joined.col1 >= c.col0
            }) else {
                return idx;
            };
            self.cohorts[j].id
        } else {
            joined.id
        };
        while let Some(w) = self.cohorts.iter().position(|c| {
            c.wake
                && c.id != band_id
                && c.row == row
                && !c.abandoned
                && fresh(c.alive_at)
                && col.saturating_add(1) >= c.col0
                && col <= c.col1
        }) {
            let wake = self.cohorts[w];
            let Some(b) = self.cohorts.iter().position(|c| c.id == band_id) else {
                break;
            };
            let band = self.cohorts[b];
            let like = self
                .cells
                .iter()
                .filter(|c| c.row == band.row && c.cohort == band.id && c.typing)
                .max_by(|a, b| a.born.cmp(&b.born))
                .copied();
            let mut ci = 0usize;
            while ci < self.cells.len() {
                let c = self.cells[ci];
                if c.cohort == band.id
                    && c.row == row
                    && (wake.col0..wake.col1).contains(&c.col)
                    && self
                        .cells
                        .iter()
                        .any(|w| w.cohort == wake.id && w.col == c.col)
                {
                    let gone = self.cells.remove(ci);
                    self.custody_replaced(b, gone.released_at, 1);
                } else {
                    ci += 1;
                }
            }
            self.carry_release(wake.id, band.id);
            self.merge_hold(wake.id, band.id, row);
            for cell in &mut self.cells {
                if cell.cohort == wake.id {
                    cell.cohort = band.id;
                    cell.t = band.t_at(cell.col);
                    if let Some(like) = like {
                        cell.life_s = cell.life_s.max(like.life_s);
                        cell.cov0 = cell.cov0.max(like.cov0);
                        cell.birth_disp = cell.birth_disp.max(like.birth_disp);
                        cell.edge_cells = like.edge_cells;
                    }
                }
            }
            self.cohorts[b].col0 = band.col0.min(wake.col0);
            self.cohorts[b].col1 = band.col1.max(wake.col1);
            self.cohorts.remove(w);
        }
        self.cohorts
            .iter()
            .position(|c| c.id == band_id)
            .unwrap_or(idx.min(self.cohorts.len().saturating_sub(1)))
    }

    #[inline]
    fn retract_span(&mut self, row: u16, col: u16, min_cells: u16, at: Instant, span_s: f32) {
        let last = col.saturating_add(min_cells.max(1) - 1);
        let n = f32::from(last - col + 1);
        let step = span_s / n;
        for cell in &mut self.cells {
            if cell.row != row || cell.col < col || cell.col > last || cell.leaving() {
                continue;
            }
            let rank = f32::from(last - cell.col);
            cell.retract_at = at.checked_add(std::time::Duration::from_secs_f32(rank * step));
        }
    }

    fn retract_suffix(&mut self, row: u16, col: u16, min_cells: u16, at: Instant, span_s: f32) {
        let followed = &self.followed_now;
        let arrived = |c: &Cell| followed.contains(&(c.row, c.col, c.born));
        let far = self
            .cells
            .iter()
            .filter(|c| c.row == row && c.col >= col && !c.leaving() && !arrived(c))
            .map(|c| c.col)
            .max();
        let last = far
            .unwrap_or(col)
            .max(col.saturating_add(min_cells.saturating_sub(1)));
        let mut right_latest: Option<Instant> = None;
        for cell in &mut self.cells {
            if cell.row != row
                || cell.col <= last
                || followed.contains(&(cell.row, cell.col, cell.born))
            {
                continue;
            }
            if let Some(prev) = cell.retract_at {
                let capped = if prev > at {
                    cell.retract_at = Some(at);
                    at
                } else {
                    prev
                };
                if at.saturating_duration_since(capped).as_secs_f32() < RETRACT_FADE_S {
                    right_latest = Some(right_latest.map_or(capped, |m| m.max(capped)));
                }
            }
        }
        let base_at = right_latest
            .and_then(|t| t.checked_add(std::time::Duration::from_millis(16)))
            .map_or(at, |t| at.max(t));
        let n = f32::from(last - col + 1);
        let step = span_s / n;
        for cell in &mut self.cells {
            if cell.row != row
                || cell.col < col
                || cell.col > last
                || cell.leaving()
                || followed.contains(&(cell.row, cell.col, cell.born))
            {
                continue;
            }
            let rank = f32::from(last - cell.col);
            cell.retract_at = at
                .checked_add(std::time::Duration::from_secs_f32(rank * step))
                .map(|t| t.max(base_at));
        }
    }

    fn abandon(
        &mut self,
        at: Instant,
        keep: Option<u32>,
        toward: Option<(Option<u16>, u16, bool)>,
        rejoinable: bool,
    ) {
        for coh in &mut self.cohorts {
            if keep == Some(coh.id) {
                continue;
            }
            coh.revoke_releases(at);
            if !coh.abandoned {
                coh.rejoinable = rejoinable;
            }
            let Some(rewound) =
                at.checked_sub(std::time::Duration::from_secs_f32(coh.retract_start()))
            else {
                continue;
            };
            coh.abandoned = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
            if let Some((row, col, leftward)) = toward
                && row.is_none_or(|row| coh.row == row)
                && coh.retract_col.is_none()
            {
                coh.retract_col = Some(col);
                coh.drain_right_first = leftward;
            }
        }
    }

    /// Retire specific live cells on the fast melt.
    pub fn retire_cells(&mut self, cells: &[(u16, u16, Instant)], at: Instant) -> usize {
        let mut n = 0;
        for cell in &mut self.cells {
            if !cells.contains(&(cell.row, cell.col, cell.born)) {
                continue;
            }
            if cell.leaving() {
                // Replaced content cannot restore a prior partial release, even when its visual retract has already finished.
                if cell.released_at.take().is_some()
                    && let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == cell.cohort)
                {
                    coh.revoke_releases(at);
                }
                continue;
            }
            cell.retire_at = Some(at);
            if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == cell.cohort) {
                coh.revoke_releases(at);
            }
            n += 1;
        }
        n
    }

    /// Release specific live cells to the swoosh.
    pub fn release_cells(&mut self, cells: &[(u16, u16, Instant)], at: Instant) -> usize {
        let Some(rewound) = at.checked_sub(std::time::Duration::from_secs_f32(RETRACT_START_S))
        else {
            return 0;
        };
        let mut n = 0;
        let mut touched = std::mem::take(&mut self.released_cohorts);
        touched.clear();
        for cell in &self.cells {
            if !cell.leaving()
                && cells.contains(&(cell.row, cell.col, cell.born))
                && !touched.contains(&cell.cohort)
            {
                touched.push(cell.cohort);
            }
        }
        for &id in &touched {
            let mut whole = true;
            let mut count = 0usize;
            let mut live_count = 0usize;
            for c in &self.cells {
                if c.cohort != id {
                    continue;
                }
                count += 1;
                if !c.leaving() {
                    live_count += 1;
                    if whole && !cells.contains(&(c.row, c.col, c.born)) {
                        whole = false;
                    }
                }
            }
            if !whole {
                let mut stamped = 0usize;
                for cell in &mut self.cells {
                    if cell.cohort == id
                        && !cell.leaving()
                        && cells.contains(&(cell.row, cell.col, cell.born))
                    {
                        cell.retract_at = Some(at);
                        cell.released_at = Some(at);
                        n += 1;
                        stamped += 1;
                    }
                }
                if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == id)
                    && stamped > 0
                {
                    coh.hold_part(ReleasePart {
                        stamp: at,
                        cells: stamped,
                    });
                }
                continue;
            }
            n += live_count;
            let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == id) else {
                continue;
            };
            if !coh.abandoned {
                coh.release_clock = Some(ReleaseClock {
                    alive_at: coh.alive_at,
                    retract_col: coh.retract_col,
                    phase: coh.phase,
                    rejoinable: coh.rejoinable,
                    cells: count,
                });
            }
            coh.rejoinable = false;
            if coh.abandoned {
                continue;
            }
            coh.abandoned = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
        }
        self.released_cohorts = touched;
        n
    }

    /// Lift the releases whose text the witness found back.
    pub(super) fn restore_releases(
        &mut self,
        candidates: &[(u32, Option<Instant>)],
        accepted: &mut Vec<u32>,
    ) {
        accepted.clear();
        for &(id, stamp) in candidates {
            let Some(k) = self.cohorts.iter().position(|c| c.id == id) else {
                continue;
            };
            let Some(stamp) = stamp else {
                let Some(saved) = self.cohorts[k].release_clock else {
                    continue;
                };
                if self.cells.iter().filter(|c| c.cohort == id).count() != saved.cells {
                    continue;
                }
                let coh = &mut self.cohorts[k];
                coh.release_clock = None;
                coh.alive_at = saved.alive_at;
                coh.retract_col = saved.retract_col;
                coh.phase = saved.phase;
                coh.rejoinable = saved.rejoinable;
                coh.abandoned = false;
                accepted.push(id);
                continue;
            };
            let Some(part) = self.cohorts[k].part_mut(stamp).copied() else {
                continue;
            };
            let resident = self
                .cells
                .iter()
                .filter(|c| c.cohort == id && c.released_at == Some(stamp))
                .count();
            if resident != part.cells {
                continue;
            }
            for cell in &mut self.cells {
                if cell.cohort == id && cell.released_at == Some(stamp) {
                    cell.retract_at = None;
                    cell.released_at = None;
                }
            }
            for slot in &mut self.cohorts[k].release_parts {
                if slot.is_some_and(|p| p.stamp == stamp) {
                    *slot = None;
                }
            }
            accepted.push(id);
        }
        accepted.sort_unstable();
        accepted.dedup();
    }

    #[inline]
    fn custody_replaced(&mut self, idx: usize, stamp: Option<Instant>, n: usize) {
        let Some(stamp) = stamp else {
            return;
        };
        if n == 0 {
            return;
        }
        if let Some(part) = self.cohorts.get_mut(idx).and_then(|c| c.part_mut(stamp)) {
            part.cells = part.cells.saturating_sub(n);
        }
    }

    #[inline]
    fn carry_release(&mut self, from: u32, into: u32) {
        let Some((parts, revoked)) = self
            .cohorts
            .iter()
            .find(|c| c.id == from)
            .map(|c| (c.release_parts, c.parts_revoked_at))
        else {
            return;
        };
        let Some(target) = self.cohorts.iter_mut().find(|c| c.id == into) else {
            return;
        };
        for part in parts.into_iter().flatten() {
            target.hold_part(part);
        }
        if let Some(t) = revoked {
            target.parts_revoked_at = Some(target.parts_revoked_at.map_or(t, |u| u.max(t)));
        }
    }

    /// Let go of the partial releases revoked since the last tick.
    #[inline]
    pub(super) fn settle_revoked_parts(&mut self) {
        for coh in &mut self.cohorts {
            let Some(t) = coh.parts_revoked_at.take() else {
                continue;
            };
            for slot in &mut coh.release_parts {
                if slot.is_some_and(|p| p.stamp < t) {
                    *slot = None;
                }
            }
        }
    }

    fn heal_seams(&mut self) {
        if self.cohorts.len() < 2 {
            return;
        }
        let mut i = 0;
        while i < self.cohorts.len() {
            let a = self.cohorts[i];
            let hot =
                |c: &Cohort| !c.wake && !c.abandoned && c.retract_col.is_none() && c.flow.is_none();
            if !hot(&a) {
                i += 1;
                continue;
            }
            let next = self.cohorts.iter().position(|b| {
                b.id != a.id && b.row == a.row && b.col0 == a.col1 && hot(b) && self.stands(b)
            });
            let Some(j) = next.filter(|_| self.stands(&a)) else {
                i += 1;
                continue;
            };
            let b = self.cohorts[j];
            let (into, from) = if a.born <= b.born { (i, j) } else { (j, i) };
            let (into_id, from_id) = (self.cohorts[into].id, self.cohorts[from].id);
            let mut idx = 0;
            while idx < self.cells.len() {
                let c = self.cells[idx];
                let shadowed = c.cohort == into_id
                    && self.cells.iter().any(|d| {
                        d.cohort == from_id && d.row == c.row && d.col == c.col && d.born >= c.born
                    });
                if shadowed {
                    let gone = self.cells.remove(idx);
                    self.custody_replaced(into, gone.released_at, 1);
                } else {
                    idx += 1;
                }
            }
            self.carry_release(from_id, into_id);
            self.merge_hold(from_id, into_id, a.row);
            for c in &mut self.cells {
                if c.cohort == from_id {
                    c.cohort = into_id;
                }
            }
            let merged = self.cohorts[from];
            let coh = &mut self.cohorts[into];
            coh.col0 = coh.col0.min(merged.col0);
            coh.col1 = coh.col1.max(merged.col1);
            coh.alive_at = coh.alive_at.max(merged.alive_at);
            coh.phase = Phase::Laying;
            if merged.anchor_col < coh.anchor_col {
                coh.anchor_col = merged.anchor_col;
                coh.t0 = merged.t0;
                coh.d0 = merged.d0;
            }
            let origin = *coh;
            for c in &mut self.cells {
                if c.cohort == into_id {
                    c.t = origin.t_at(c.col);
                }
            }
            self.cohorts.remove(from);
            i = 0;
        }
    }

    /// Build this frame's [`Ribbon::plan`] and [`FieldIndex`].
    pub fn plan(&mut self, ctx: &Ctx<'_>) {
        self.plan_over(ctx, None);
    }

    /// [`Self::plan`] told what the glyphs below the light hold (`below`, the
    /// frame's [`GlyphProbe`]). A row of light whose next row is probed with
    /// ink under it (a column either side of its span included) is held
    /// inside its own row: its body takes the pane's last-row shape (spine
    /// lifted by the lower reach, so nothing reaches past the row's bottom)
    /// and its rail stays dark, so no light lies on the next line's
    /// ascenders. The pane's last row is held too (no row is under it). A row
    /// first lit over ink is born held, one first lit over a blank or unknown
    /// row is born free; one whose next row inks while it is lit eases into
    /// its row over [`HOLD_EASE_S`], and one whose next row clears, or whose
    /// line flows into the fold, lets go over [`HOLD_RELEASE_S`]; while the
    /// probe does not know the row below (a scroll's frame clears it) the row
    /// keeps its level. A scroll or a band move carries each row's hold with
    /// its line.
    pub fn plan_over(&mut self, ctx: &Ctx<'_>, below: Option<&GlyphProbe>) {
        self.ink.sync(ctx.cfg);
        self.ground = ctx.cfg.theme_bg;
        self.caret = Some(ctx.caret);
        self.reduced = ctx.cfg.reduced_motion;
        self.cell_h = ctx.geom.ch as f32;
        self.wedge_px = (Self::body_profile(ctx.cfg).dn_ch - DN_FLOOR_CH).max(0.0) * self.cell_h;
        self.advance_swoosh(ctx);
        self.retire(ctx);
        if self
            .seam
            .is_some_and(|s| ctx.now.saturating_duration_since(s.at).as_secs_f32() >= FLOW_TOTAL_S)
        {
            self.seam = None;
        }
        self.heal_seams();
        self.build(ctx, below);
        self.carried_holds.clear();
        self.scroll_followed = false;
        self.followed_now.clear();
        self.short_reanchor = None;
        if self
            .rewrap_start
            .is_some_and(|rewrap| rewrap.width.is_some())
        {
            self.rewrap_start = None;
        }
        self.shifted_now.clear();
    }

    fn advance_swoosh(&mut self, ctx: &Ctx<'_>) {
        if self.cohorts.is_empty() {
            return;
        }
        let now = ctx.now;
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let mut reach = [(0u16, 0u16, 0u32); REACH_STAGE];
        let mut n_reach = 0usize;
        for i in 0..self.cohorts.len() {
            let coh = self.cohorts[i];
            let idle = coh.idle_s(now);
            let Some(phase) = coh.phase_at(idle) else {
                self.cohorts[i].phase = Phase::Fading;
                continue;
            };
            self.cohorts[i].phase = phase;
            if phase.is_retracting() && self.cohorts[i].retract_col.is_none() {
                let coh = self.cohorts[i];
                if coh.flow.is_some() {
                    let (target, right_first, drift) = self.flow_target(&coh);
                    self.cohorts[i].retract_col = target;
                    self.cohorts[i].drain_right_first = right_first;
                    self.cohorts[i].flow_drift = drift;
                } else {
                    let (target, right_first) = self.retract_target(&coh);
                    self.cohorts[i].retract_col = target;
                    self.cohorts[i].drain_right_first = right_first;
                }
            }
            if phase != Phase::Reaching || ctx.cfg.reduced_motion || self.curtain.is_some() {
                continue;
            }
            let beats = (((idle - coh.grace_s) / REACH_STEP_S).floor() as i32 + 1)
                .clamp(0, i32::from(REACH_BEATS)) as u16;
            let span = coh.col1.saturating_sub(coh.col0);
            let want = FOUR_LETTER_CELLS.saturating_sub(span).min(beats);
            let reach_floor = self.hand_floor_on(coh.row).unwrap_or(0).max(pane_col0);
            for k in 0..want {
                let Some(c) = coh.col0.checked_sub(k + 1) else {
                    break;
                };
                if c < reach_floor {
                    break;
                }
                if n_reach == REACH_STAGE {
                    break;
                }
                reach[n_reach] = (coh.row, c, coh.id);
                n_reach += 1;
            }
        }
        for &(row, col, cohort) in &reach[..n_reach] {
            if self.cells.iter().any(|c| c.row == row && c.col == col) {
                continue;
            }
            self.lay_reach_cell(row, col, cohort);
        }
    }

    fn lay_reach_cell(&mut self, row: u16, col: u16, cohort: u32) {
        let Some(coh) = self.cohorts.iter().find(|c| c.id == cohort).copied() else {
            return;
        };
        let Some(&tail) = self
            .cells
            .iter()
            .filter(|c| c.row == row && c.cohort == cohort)
            .min_by_key(|c| c.col)
        else {
            return;
        };
        self.cells.push(Cell {
            row,
            col,
            cohort,
            t: coh.t_at(col),
            born: tail.born,
            attack_at: tail.attack_at,
            life_s: tail.life_s,
            cov0: tail.cov0,
            typing: false,
            retract_at: tail.retract_at,
            retire_at: tail.retire_at,
            birth_disp: tail.birth_disp,
            edge_cells: tail.edge_cells,
            layer: tail.layer,
            rearm: tail.rearm,
            released_at: None,
        });
        if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == cohort) {
            coh.col0 = coh.col0.min(col);
        }
    }

    fn retire(&mut self, ctx: &Ctx<'_>) {
        if self.cells.is_empty()
            && self.cohorts.is_empty()
            && self.ember_at.is_none()
            && self.curtain.is_none()
        {
            return;
        }
        let now = ctx.now;
        let ember_done = self
            .ember_at
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() >= FOCUS_EMBER_S);
        let curtain_done = self
            .curtain
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() >= CURTAIN_S);
        if ember_done || curtain_done {
            self.cells.clear();
            self.cohorts.clear();
            self.ember_at = None;
            self.curtain = None;
            return;
        }
        let cohorts = &self.cohorts;
        let mut cached_coh: Option<(&Cohort, bool)> = None;
        self.cells.retain(|cell| {
            let (coh, phase_live) = match cached_coh {
                Some((c, live)) if c.id == cell.cohort => (c, live),
                _ => {
                    let Some(c) = cohorts.iter().find(|c| c.id == cell.cohort) else {
                        return false;
                    };
                    let live = c.phase_at(c.idle_s(now)).is_some();
                    cached_coh = Some((c, live));
                    (c, live)
                }
            };
            if coh.flow.is_none()
                && now
                    .saturating_duration_since(cell.born.max(coh.alive_at))
                    .as_secs_f32()
                    >= cell.life_s
            {
                return false;
            }
            if let Some(at) = cell.retract_at
                && now.saturating_duration_since(at).as_secs_f32() >= RETRACT_FADE_S
            {
                let held = coh.holds_partial(cell);
                if !held {
                    return false;
                }
            }
            if let Some(at) = cell.retire_at
                && now.saturating_duration_since(at).as_secs_f32() >= RETIRE_MELT_S
            {
                return false;
            }
            phase_live
        });
        let cells = &self.cells;
        let mut gone_freshest: Option<Cohort> = None;
        self.cohorts.retain_mut(|coh| {
            let resident = if coh.flow.is_none() {
                cells.iter().any(|cell| cell.cohort == coh.id)
            } else {
                let (resident, lo, hi) = cells.iter().filter(|cell| cell.cohort == coh.id).fold(
                    (false, u16::MAX, 0u16),
                    |(_, lo, hi), cell| {
                        if cell.leaving() {
                            (true, lo, hi)
                        } else {
                            (true, lo.min(cell.col), hi.max(cell.col))
                        }
                    },
                );
                if lo <= hi
                    && let Some(edge) = &mut coh.retract_col
                {
                    *edge = (*edge).clamp(lo, hi);
                    coh.flow_drift = coh
                        .flow_drift
                        .min(FLOW_DRIFT_MAX_SHARE * (f32::from(hi - lo) + 1.0));
                }
                resident
            };
            if !resident {
                let fresher = gone_freshest.is_none_or(|a| {
                    coh.alive_at
                        .cmp(&a.alive_at)
                        .then_with(|| coh.walk_d(coh.col1).total_cmp(&a.walk_d(a.col1)))
                        .is_ge()
                });
                if fresher {
                    gone_freshest = Some(*coh);
                }
            }
            resident
        });
        if let Some(gone) = gone_freshest {
            self.last_walk = Some(LastWalk {
                end: gone.walk_at(gone.col1),
                alive_at: gone.alive_at,
            });
        }
    }

    fn build(&mut self, ctx: &Ctx<'_>, below: Option<&GlyphProbe>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        self.plan.clear();
        self.runs.clear();
        if let Some((row, col)) = self.attach
            && ctx.caret != (row, col)
            && !self
                .cells
                .iter()
                .any(|c| c.row == row && c.col == col && !c.leaving())
        {
            for c in &mut self.cells {
                if c.row == row
                    && c.col == col
                    && let Some(at) = c.retract_at
                    && at < ctx.now
                {
                    c.retract_at = Some(ctx.now);
                }
            }
        }
        self.attach = None;
        self.attach_floor = None;
        self.index.begin(cols);
        if self.cells.is_empty() {
            self.holds.clear();
            self.holds_next.clear();
            return;
        }
        let mut sorted = mem::take(&mut self.sorted);
        sorted.clear();
        sorted.reserve(self.cells.len());
        for (i, c) in self.cells.iter().enumerate() {
            // Expired partial releases may remain in content custody, but own neither field nor geometry until their glyphs return.
            if c.retract_at.is_some_and(|at| {
                ctx.now.saturating_duration_since(at).as_secs_f32() >= RETRACT_FADE_S
            }) {
                continue;
            }
            sorted.push((c.row, c.col, u32::try_from(i).unwrap_or(u32::MAX)));
        }
        let now = ctx.now;
        let cells = &self.cells;
        let rank = |i: u32| -> u8 {
            match cells.get(i as usize) {
                Some(c) if c.layer == Layer::Over => u8::from(now < c.born) * 2,
                _ => 1,
            }
        };
        sorted.sort_unstable_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then_with(|| rank(a.2).cmp(&rank(b.2)))
                .then_with(|| b.2.cmp(&a.2))
        });
        sorted.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        for &(row, col, idx) in &sorted {
            let Some(cell) = self.cells.get(idx as usize) else {
                continue;
            };
            self.index.set(row, col, cell.t);
        }
        self.slabs = Self::slabs_for(ctx, sorted.len());
        self.build_runs(ctx, &sorted, below);
        self.sorted = sorted;
        self.runs
            .sort_unstable_by(|a, b| b.at_caret.cmp(&a.at_caret).then(b.born.cmp(&a.born)));
    }

    #[inline]
    const fn slabs_for(ctx: &Ctx<'_>, cells: usize) -> usize {
        let prof = Self::body_profile(ctx.cfg);
        let ch = ctx.geom.ch as f32;
        let body = ((prof.up_ch + DN_TOP_CH) * ch).ceil().max(1.0) + 2.0;
        let rail = if Self::rail_lit(ctx.cfg) {
            (DN_TOP_CH * ch).ceil() + 1.0
        } else {
            0.0
        };
        let rows = body + rail;
        let cells = cells + 1;
        let per_slab = (cells as f32 * rows).max(1.0);
        let fit = (RIBBON_QUAD_BUDGET as f32 / per_slab).floor();
        let n = fit.max(1.0) as usize;
        if n < 1 {
            1
        } else if n > SLABS_PER_CELL {
            SLABS_PER_CELL
        } else {
            n
        }
    }

    fn build_runs(
        &mut self,
        ctx: &Ctx<'_>,
        sorted: &[(u16, u16, u32)],
        below: Option<&GlyphProbe>,
    ) {
        const fn cohort_of(cells: &[Cell], e: (u16, u16, u32)) -> Option<u32> {
            let idx = e.2 as usize;
            if idx < cells.len() {
                Some(cells[idx].cohort)
            } else {
                None
            }
        }
        // One answer per row (its runs share seams, so they share a floor).
        let mut held_row: Option<(u16, f32)> = None;
        let chf = ctx.geom.ch as f32;
        let pane_bot = self.pane_bot(ctx, chf);
        let mut s = 0usize;
        while s < sorted.len() {
            let row = sorted[s].0;
            let hold = match held_row {
                Some((r, hold)) if r == row => hold,
                _ => {
                    let end = s + sorted[s..].partition_point(|e| e.0 == row);
                    let row_cells = &sorted[s..end];
                    let last = row_cells.last().map_or(sorted[s].1, |e| e.1);
                    let floor = f32::from(ctx.geom.origin_y) + (f32::from(row) + 1.0) * chf;
                    let held = if floor >= pane_bot {
                        Some(true)
                    } else {
                        below.and_then(|p| Self::inked_below(p, ctx, row, sorted[s].1, last))
                    };
                    let held = held.map(|h| {
                        if !h {
                            return false;
                        }
                        let mut seen_coh = None;
                        let flowing = row_cells
                            .iter()
                            .filter_map(|e| cohort_of(&self.cells, *e))
                            .any(|id| {
                                if seen_coh == Some(id) {
                                    return false;
                                }
                                seen_coh = Some(id);
                                self.cohorts.iter().any(|c| {
                                    c.id == id && c.flow.is_some() && c.phase.is_retracting()
                                })
                            });
                        !flowing
                    });
                    let previous = self.previous_hold(row, row_cells);
                    let hold = self.ease_hold(ctx, row, held, previous);
                    held_row = Some((row, hold));
                    hold
                }
            };
            let mut e = s;
            while e + 1 < sorted.len()
                && sorted[e + 1].0 == sorted[s].0
                && sorted[e + 1].1 == sorted[e].1 + 1
                && cohort_of(&self.cells, sorted[e + 1]) == cohort_of(&self.cells, sorted[e])
            {
                e += 1;
            }
            let row = sorted[s].0;
            let nb = |i: Option<usize>, col: Option<u16>| -> Option<Cell> {
                let (i, col) = (i?, col?);
                let e = sorted.get(i)?;
                (e.0 == row && e.1 == col).then(|| self.cells.get(e.2 as usize).copied())?
            };
            let abuts_left = nb(s.checked_sub(1), sorted[s].1.checked_sub(1)).is_some();
            let seam = if abuts_left {
                self.runs
                    .last()
                    .filter(|r| r.row == row && r.col1.checked_add(1) == Some(sorted[s].1))
                    .and_then(|r| self.plan.get(r.hi.checked_sub(1)?).copied())
            } else {
                None
            };
            let right = nb(Some(e + 1), sorted[e].1.checked_add(1));
            self.plan_run(ctx, &sorted[s..=e], seam, right, hold);
            s = e + 1;
        }
        mem::swap(&mut self.holds, &mut self.holds_next);
        self.holds_next.clear();
    }

    /// The strongest prior shape among the cohorts actually drawing this row.
    /// A moved-out run contributes nothing; a partial move leaves its original
    /// source alongside the copy's carried shape. A no-move, no-birth frame
    /// reads its row directly, and a move scans each contiguous cohort once.
    fn previous_hold(&self, row: u16, row_cells: &[(u16, u16, u32)]) -> Option<(f32, Instant)> {
        let original = self.holds.iter().find(|h| h.0 == row).map(|h| (h.1, h.2));
        if self.carried_holds.is_empty() {
            return original;
        }
        let mut seen = None;
        let mut strongest: Option<(f32, Instant)> = None;
        for &(_, _, idx) in row_cells {
            let Some(cell) = self.cells.get(idx as usize) else {
                continue;
            };
            if seen == Some(cell.cohort) {
                continue;
            }
            seen = Some(cell.cohort);
            let previous = self
                .carried_holds
                .iter()
                .find(|h| h.0 == cell.cohort)
                .map_or(original, |h| h.1);
            strongest = Self::strongest_hold(strongest, previous);
        }
        strongest
    }

    #[inline]
    fn strongest_hold(
        a: Option<(f32, Instant)>,
        b: Option<(f32, Instant)>,
    ) -> Option<(f32, Instant)> {
        match (a, b) {
            (Some(a), Some(b)) => Some(if a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).is_gt() {
                a
            } else {
                b
            }),
            (Some(a), None) => Some(a),
            (None, b) => b,
        }
    }

    #[inline]
    const fn cohort_hold(&self, cohort: u32, row: u16) -> Option<(f32, Instant)> {
        let carried = self.carried_holds.as_slice();
        let mut i = 0;
        while i < carried.len() {
            if carried[i].0 == cohort {
                return carried[i].1;
            }
            i += 1;
        }
        let holds = self.holds.as_slice();
        let mut j = 0;
        while j < holds.len() {
            if holds[j].0 == row {
                return Some((holds[j].1, holds[j].2));
            }
            j += 1;
        }
        None
    }

    fn stage_hold(&mut self, cohort: u32, previous: Option<(f32, Instant)>) {
        if let Some(carried) = self.carried_holds.iter_mut().find(|h| h.0 == cohort) {
            carried.1 = previous;
        } else {
            self.carried_holds.push((cohort, previous));
        }
    }

    /// Adopting old visual cells keeps their shape while any old cells left
    /// behind retain their own custody. Fresh identities explicitly carry no
    /// prior shape, so they cannot revive a departed occupant's row hold.
    fn copy_hold(&mut self, from: u32, into: u32, row: u16) {
        if from == into || self.carried_holds.is_empty() {
            return;
        }
        let previous =
            Self::strongest_hold(self.cohort_hold(from, row), self.cohort_hold(into, row));
        self.stage_hold(into, previous);
    }

    /// A whole-cohort merge retires the removed identity after transferring
    /// its strongest shape to the survivor.
    fn merge_hold(&mut self, from: u32, into: u32, row: u16) {
        if from == into {
            return;
        }
        self.copy_hold(from, into, row);
        self.carried_holds.retain(|h| h.0 != from);
    }

    /// Row `row`'s hold this frame, eased toward `held` from `last`
    /// ([`HOLD_EASE_S`] in, [`HOLD_RELEASE_S`] out), or born at it; recorded
    /// for the next frame. `None` (the row below not known: the frame of a
    /// scroll, which clears the probe, or a row away from the caret's) keeps
    /// the level the row had, and a row first lit so is born free.
    fn ease_hold(
        &mut self,
        ctx: &Ctx<'_>,
        row: u16,
        held: Option<bool>,
        last: Option<(f32, Instant)>,
    ) -> f32 {
        #[cfg(test)]
        let steps = self.hold_steps;
        #[cfg(not(test))]
        let steps = false;
        let level = match (held.map(|h| f32::from(u8::from(h))), last) {
            (None, Some((level, _))) => level,
            (None, None) => 0.0,
            (Some(target), Some((level, at))) if !steps && !ctx.cfg.reduced_motion => {
                let dt = ctx.now.saturating_duration_since(at).as_secs_f32();
                if target > level {
                    (level + dt / HOLD_EASE_S).min(target)
                } else {
                    (level - dt / HOLD_RELEASE_S).max(target)
                }
            }
            (Some(target), _) => target,
        };
        self.holds_next.push((row, level, ctx.now));
        level
    }

    /// Does `probe` know ink on the row below `row`, under `col0..=col1` or a
    /// column either side? `Some(false)` when it knows every one of them
    /// blank, `None` when it knows no ink there and not all of them.
    #[inline]
    const fn inked_below(
        probe: &GlyphProbe,
        ctx: &Ctx<'_>,
        row: u16,
        col0: u16,
        col1: u16,
    ) -> Option<bool> {
        let below = row as i32 + 1;
        let mut known = true;
        let mut col = col0.saturating_sub(1);
        let end = col1.saturating_add(1);
        loop {
            match probe.at(below, col, ctx.geom.rows) {
                Some(true) => return Some(true),
                Some(false) => {}
                None => known = false,
            }
            if col == end {
                break;
            }
            col += 1;
        }
        if known { Some(false) } else { None }
    }
}

#[derive(Clone, Copy)]
struct RunSample {
    chf: f32,
    prof: BodyProfile,
    reduced: bool,
    disp: f32,
    room: f32,
    span: f32,
    row_bot: f32,
    dn_floor: f32,
    settle: f32,
    settle_scale: f32,
    breath_scale: f32,
    comet: bool,
    fx_top: f32,
    pane_bot: f32,
    hold: f32,
}

impl Ribbon {
    #[inline]
    const fn run_sample(
        &self,
        ctx: &Ctx<'_>,
        row: u16,
        coh: Option<&Cohort>,
        hold: f32,
        (settle, breath): (f32, f32),
    ) -> RunSample {
        let chf = ctx.geom.ch as f32;
        let prof = Self::body_profile(ctx.cfg);
        let reduced = ctx.cfg.reduced_motion;
        let disp = clamp01(ctx.disp);
        let room = (DN_TOP_CH - DN_FLOOR_CH) * chf;
        let span = match coh {
            Some(c) => (c.col1.saturating_sub(c.col0) as f32).max(1.0),
            None => 1.0,
        };
        let row_bot = ctx.geom.origin_y as f32 + (row as f32 + 1.0) * chf;
        let dn_floor = DN_FLOOR_CH * chf;
        let settle_scale = 1.0 - FLOW_SETTLE_SHARE * settle;
        let breath_scale = 1.0 - breath;
        let comet = Self::comet(ctx.cfg);
        let fx_top = ctx.geom.fx_top() as f32;
        let pane_bot = self.pane_bot(ctx, chf);
        RunSample {
            chf,
            prof,
            reduced,
            disp,
            room,
            span,
            row_bot,
            dn_floor,
            settle,
            settle_scale,
            breath_scale,
            comet,
            fx_top,
            pane_bot,
            hold,
        }
    }

    fn sample_with(
        &self,
        ctx: &Ctx<'_>,
        cell: &Cell,
        coh: Option<&Cohort>,
        sp: f32,
        rs: &RunSample,
    ) -> (f32, f32, f32, f32) {
        let chf = rs.chf;
        let prof = rs.prof;
        let disp = rs.disp;
        let amp = if rs.reduced || disp <= 0.0 || rs.settle >= 1.0 || sp <= 0.0 {
            0.0
        } else {
            (WAVE_AMP_CELLS * chf * disp).min(rs.room)
                * smoothstep01(sp / WAVE_HEAD_PIN)
                * (1.0 - rs.settle)
        };
        let wave = if amp == 0.0 {
            0.0
        } else {
            ((sp * std::f32::consts::TAU * WAVE_CYCLES) - ctx.phase * std::f32::consts::TAU).sin()
                * amp
        };
        let behind = sp * rs.span;
        let bloom = if disp <= 0.0 || behind >= BLOOM_REACH_CELLS {
            0.0
        } else {
            clamp01(disp * (1.0 - smoothstep01(behind / BLOOM_REACH_CELLS)))
        };
        let row_bot = rs.row_bot;
        let dn_floor = rs.dn_floor;
        let settle_scale = rs.settle_scale;
        let breath_scale = rs.breath_scale;
        let fx_top = rs.fx_top;
        let wave_pos = wave.max(0.0);
        let wave_neg = wave.min(0.0);
        let spine_raw = row_bot + wave;
        let comet_up_ch = if rs.comet {
            prof.up_ch
                * (1.0
                    - (1.0 - COMET_UP_TAIL_CH / TALL_UP_CH)
                        * smoothstep01(behind / COMET_TAPER_CELLS))
        } else {
            prof.up_ch
        };
        // The body over a floor at `pane_bot`: a held row's floor is its own
        // bottom, where it is laid as a last row is.
        let shape = |pane_bot: f32| -> (f32, f32, f32) {
            let room_below = pane_bot - row_bot;
            let last_row = room_below < prof.dn_ch * chf;
            let dn_ch = if last_row { DN_TOP_CH } else { prof.dn_ch };
            let dn_open = chf * (DN_FLOOR_CH + (dn_ch - DN_FLOOR_CH) * bloom);
            let dn = (dn_open - wave_pos).max(dn_floor);
            let dn = (dn_floor + (dn - dn_floor) * settle_scale) * breath_scale;
            let up_ch = if last_row { prof.up_ch } else { comet_up_ch };
            let up = (up_ch * chf + wave_neg).max(0.0) * settle_scale * breath_scale;
            let spine = spine_raw.min(pane_bot - dn).max(fx_top);
            (spine, up, dn)
        };
        let pane_bot = rs.pane_bot;
        let hold = rs.hold;
        let (spine, up, dn) = if hold <= 0.0 || pane_bot <= row_bot {
            shape(pane_bot)
        } else if hold >= 1.0 {
            shape(row_bot)
        } else {
            let (free, held) = (shape(pane_bot), shape(row_bot));
            let mix = |a: f32, b: f32| a + (b - a) * hold;
            (
                mix(free.0, held.0),
                mix(free.1, held.1),
                mix(free.2, held.2),
            )
        };
        (spine, up, dn, self.cov_of(ctx, cell, coh))
    }

    #[inline]
    fn env_of(&self, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        self.env_with(ctx, cell, self.cohorts.iter().find(|c| c.id == cell.cohort))
    }

    #[inline]
    fn env_with(&self, ctx: &Ctx<'_>, cell: &Cell, coh: Option<&Cohort>) -> f32 {
        let now = ctx.now;
        if now < cell.born {
            return 0.0;
        }
        let reduced = ctx.cfg.reduced_motion;
        let attack_now = cell.retire_at.map_or(now, |at| now.min(at));
        let age = attack_now
            .saturating_duration_since(cell.attack_at)
            .as_secs_f32();
        let spent_to = coh.and_then(|c| c.flow).map_or(now, |f| now.min(f));
        let spent = spent_to
            .saturating_duration_since(coh.map_or(cell.born, |c| cell.born.max(c.alive_at)))
            .as_secs_f32();
        let u = (spent / cell.life_s.max(1e-3)).clamp(0.0, 1.0);
        let attack = match cell.rearm {
            Some((re_at, level)) => {
                let r = attack_now.saturating_duration_since(re_at).as_secs_f32();
                level + (1.0 - level) * edge_in(r)
            }
            None => BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age),
        };
        let mut env = attack
            * if reduced {
                reduced_fade(cell.life_s - spent)
            } else {
                expiry_melt(u)
            };
        if let Some(at) = cell.retract_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(RETRACT_FADE_S - r)
            } else {
                spend(clamp01(r / RETRACT_FADE_S))
            };
        }
        if let Some(at) = cell.retire_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(RETIRE_MELT_S - r)
            } else {
                spend(clamp01(r / RETIRE_MELT_S))
            };
        }
        if let Some(at) = self.curtain {
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(CURTAIN_S - r)
            } else {
                let rank = coh.map_or(1.0, |c| c.drain_rank(cell.col));
                let t0 = CURTAIN_STAGGER_S * (1.0 - rank);
                spend(clamp01((r - t0) / CURTAIN_SPEND_S))
            };
        }
        if let Some(at) = coh.and_then(|c| c.printed) {
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= 1.0 - (1.0 - RETURN_PRINTED_SHARE) * smoothstep01(r / RETURN_DIM_S);
        }
        if let Some(coh) = coh
            && coh.phase.is_retracting()
        {
            let idle = coh.idle_s(now);
            if reduced {
                env *= reduced_fade(coh.swoosh_total() - idle);
            } else {
                // Farthest from the hand first ([`Cohort::drain_rank`]), each cell taking `RETRACT_FADE_S`, so the mark is drawn back into the hand.
                if coh.flow.is_some() {
                    env *= ember_level(
                        self.ember_floor(ctx, cell, env),
                        coh.flow_drain(cell.col, idle),
                    );
                } else {
                    let t0 =
                        coh.retract_start() + coh.retract_span() * (1.0 - coh.drain_rank(cell.col));
                    env *= spend(clamp01((idle - t0) / RETRACT_FADE_S));
                }
            }
        }
        if let Some(at) = self.ember_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            let f = if reduced {
                reduced_fade(FOCUS_EMBER_S - r)
            } else {
                spend(clamp01(r / FOCUS_EMBER_S))
            };
            env *= ember_level(self.ember_floor(ctx, cell, env), f);
        }
        if let Some((at, fade)) = self.rearm {
            let level = ember_level(self.ember_floor(ctx, cell, env), fade);
            let r = attack_now.saturating_duration_since(at).as_secs_f32();
            env *= level + (1.0 - level) * edge_in(r);
        }
        env
    }

    #[inline]
    fn cov_of(&self, ctx: &Ctx<'_>, cell: &Cell, coh: Option<&Cohort>) -> f32 {
        let cap = cov_cap(ctx.cfg);
        (cap * cell.cov0 * self.env_with(ctx, cell, coh) * clamp01(ctx.cfg.intensity))
            .clamp(0.0, cap)
    }

    #[inline]
    const fn ember_floor(&self, ctx: &Ctx<'_>, cell: &Cell, carried: f32) -> f32 {
        self.ember_floor_of(
            cov_cap(ctx.cfg) * cell.cov0 * clamp01(ctx.cfg.intensity) * carried,
            cell.t,
        )
    }

    #[inline]
    const fn ember_floor_of(&self, full_cov: f32, t: f32) -> f32 {
        if !full_cov.is_finite() || full_cov <= 0.0 {
            return 0.0;
        }
        (self.ink.seen_floor_at(t) * EMBER_FLOOR_MARGIN / full_cov).max(0.0)
    }

    #[inline]
    fn wipe_of(&self, ctx: &Ctx<'_>, cell: &Cell, dist: f32) -> f32 {
        // Typed cells only: a wake's cells share one birth instant, so wiping each from its own edge would print a comb; its lay sweep is its attack.
        if !cell.typing
            || !Self::comet(ctx.cfg)
            || ctx.cfg.reduced_motion
            || ctx.now < cell.attack_at
        {
            return 1.0;
        }
        let age = ctx
            .now
            .saturating_duration_since(cell.attack_at)
            .as_secs_f32();
        if age >= ATTACK_WIPE_S {
            return 1.0;
        }
        let ramp = edge_in(age);
        if ramp <= 0.0 {
            return 1.0;
        }
        let front = clamp01(age / ATTACK_WIPE_S) * (1.0 + ATTACK_WIPE_FRONT);
        let gate = smoothstep01((front - clamp01(dist)) / ATTACK_WIPE_FRONT);
        let uniform = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * ramp;
        let wiped = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * ramp * gate;
        wiped / uniform
    }

    #[inline]
    fn retract_x(&self, ctx: &Ctx<'_>, coh: Option<&Cohort>) -> RetractX {
        if ctx.cfg.reduced_motion {
            return RetractX::Identity;
        }
        if let Some(at) = self.curtain {
            let u = clamp01(ctx.now.saturating_duration_since(at).as_secs_f32() / CURTAIN_S);
            let Some(col) = coh.and_then(|c| c.retract_col) else {
                return RetractX::Identity;
            };
            let caret_x = f32::from(ctx.geom.origin_x) + f32::from(col) * ctx.geom.cw as f32;
            return RetractX::SuckIn {
                caret_x,
                keep: 1.0 - suck_in(u),
            };
        }
        let Some(coh) = coh.filter(|c| c.phase.is_retracting()) else {
            return RetractX::Identity;
        };
        let idle = coh.idle_s(ctx.now);
        if coh.flow.is_some() {
            let Some(edge) = coh.retract_col else {
                return RetractX::Identity;
            };
            let cw = ctx.geom.cw as f32;
            let ox = f32::from(ctx.geom.origin_x);
            let s = coh.flow_drift * cw * coh.flow_progress(idle);
            return if coh.drain_right_first {
                RetractX::FlowRightFirst {
                    start: ox + f32::from(edge) * cw,
                    s,
                }
            } else {
                RetractX::FlowLeftFirst {
                    end: ox + (f32::from(edge) + 1.0) * cw,
                    s,
                }
            };
        }
        let Some(col) = coh.retract_col.or_else(|| self.caret.map(|(_, col)| col)) else {
            return RetractX::Identity;
        };
        let u = clamp01((idle - coh.retract_start()) / (coh.retract_span() + RETRACT_FADE_S));
        let caret_x = f32::from(ctx.geom.origin_x) + f32::from(col) * ctx.geom.cw as f32;
        RetractX::SuckIn {
            caret_x,
            keep: 1.0 - suck_in(u),
        }
    }

    #[inline]
    fn head_col(
        &self,
        ctx: &Ctx<'_>,
        run: &[(u16, u16, u32)],
        coh: Option<&Cohort>,
    ) -> (u16, bool, bool, i8) {
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let (crow, ccol) = ctx.caret;
        let at_caret = crow == row && (col0..=col1 + 1).contains(&ccol);
        let live = |col: u16| -> bool {
            run.get(usize::from(col - col0))
                .and_then(|e| self.cells.get(e.2 as usize))
                .is_some_and(|c| c.col == col && !c.leaving())
        };
        if at_caret
            && let Some(own) = ccol.checked_sub(1)
            && (col0..=col1).contains(&own)
            && live(own)
        {
            return (own, true, true, -1);
        }
        if at_caret && ccol == col0 && live(col0) && coh.is_some_and(|k| k.wake) {
            return (col0, true, true, 1);
        }
        let newest = run
            .iter()
            .filter_map(|e| self.cells.get(e.2 as usize))
            .filter(|c| !c.leaving())
            .max_by(|a, b| a.born.cmp(&b.born))
            .map(|c| c.col);
        match newest {
            Some(col) => (col, at_caret, true, -1),
            None => (col1, at_caret, false, -1),
        }
    }

    fn plan_run(
        &mut self,
        ctx: &Ctx<'_>,
        run: &[(u16, u16, u32)],
        seam: Option<Segment>,
        right_nb: Option<Cell>,
        hold: f32,
    ) {
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let Some(cohort) = self.cells.get(run[0].2 as usize).map(|c| c.cohort) else {
            return;
        };
        let coh = self.cohorts.iter().find(|c| c.id == cohort);
        let slabs = self.slabs_per_cell();
        let (head_col, at_caret, wet, stream_dir) = self.head_col(ctx, run, coh);
        let born = run
            .iter()
            .filter_map(|e| self.cells.get(e.2 as usize))
            .filter(|c| c.typing)
            .map(|c| c.born)
            .max()
            .unwrap_or(ctx.now);
        let span = f32::from(col1 - col0).max(1.0);
        let cell_at = |col: u16| -> Option<&Cell> {
            let i = usize::from(col.checked_sub(col0)?);
            let e = run.get(i)?;
            (e.1 == col).then(|| self.cells.get(e.2 as usize))?
        };
        let cw = ctx.geom.cw as f32;
        let pane_end = self.pane.map_or(
            u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX),
            |(c0, w)| c0.saturating_add(w),
        );
        let caret_cell = head_col.wrapping_add(1);
        let under_caret_leaving = match cell_at(caret_cell) {
            Some(c) => c.leaving(),
            None => right_nb.as_ref().is_some_and(Cell::leaving),
        };
        let caret_free = cell_at(caret_cell).is_none() && right_nb.is_none() || under_caret_leaving;
        let attach = stream_dir < 0
            && wet
            && at_caret
            && ctx.caret.1 == caret_cell
            && ctx.caret.1 < pane_end
            && caret_free;
        let attack = attach
            .then(|| cell_at(head_col).copied())
            .flatten()
            .map(|head| {
                let probe = Cell {
                    attack_at: head.born,
                    ..head
                };
                let age = ctx.now.saturating_duration_since(head.born).as_secs_f32();
                (
                    BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age),
                    probe,
                )
            });
        if attack.is_some() {
            self.attach = Some((row, caret_cell));
        }
        let lo = self.plan.len();
        let mut prev: Option<(f32, Sample)> = None;
        let mut left_sample = None;
        let mut doubled: Option<usize> = None;
        let caret_side_right = stream_dir < 0;
        let ox = f32::from(ctx.geom.origin_x);
        let slabs_f = slabs as f32;
        let retract = self.retract_x(ctx, coh);
        let flow_settle = if ctx.cfg.reduced_motion {
            (0.0, 0.0)
        } else {
            coh.filter(|c| c.flow.is_some() && c.phase.is_retracting())
                .map_or((0.0, 0.0), |c| {
                    let idle = c.idle_s(ctx.now);
                    (
                        c.flow_progress(idle),
                        smoothstep01((idle - (FLOW_TOTAL_S - FLOW_TAIL_S)) / FLOW_TAIL_S),
                    )
                })
        };
        let rs = self.run_sample(ctx, row, coh, hold, flow_settle);
        for boundary in u32::from(col0)..=u32::from(col1) + 1 {
            let left = boundary
                .checked_sub(1)
                .and_then(|c| u16::try_from(c).ok())
                .and_then(cell_at);
            let right = u16::try_from(boundary).ok().and_then(cell_at);
            let (a, b) = match (left, right) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (a, a),
                (None, Some(b)) => (b, b),
                (None, None) => {
                    left_sample = None;
                    continue;
                }
            };
            let sp = |c: &Cell| f32::from(head_col.abs_diff(c.col)) / span;
            debug_assert_eq!(left_sample.is_some(), left.is_some());
            let sa = left_sample.unwrap_or_else(|| self.sample_with(ctx, a, coh, sp(a), &rs));
            let sb = if a.col == b.col {
                sa
            } else {
                self.sample_with(ctx, b, coh, sp(b), &rs)
            };
            left_sample = right.map(|_| sb);
            let mid = |u: f32, v: f32| (u + v) * 0.5;
            let mut la = left.map_or(0.0, |_| {
                sa.3 * self.wipe_of(ctx, a, if caret_side_right { 0.0 } else { 1.0 })
            });
            let mut lb = right.map_or(0.0, |_| {
                sb.3 * self.wipe_of(ctx, b, if caret_side_right { 1.0 } else { 0.0 })
            });
            let mut shape = (
                mid(sa.0, sb.0),
                mid(sa.1, sb.1),
                mid(sa.2, sb.2),
                mid(a.t, b.t),
            );
            if left.is_none()
                && let Some(seam) = seam
            {
                la = f32::from(seam.cov);
                shape = (seam.spine, seam.up, seam.dn, seam.t);
            }
            if right.is_none()
                && let Some(nb) = right_nb.as_ref()
            {
                let nb_coh = self.cohorts.iter().find(|c| c.id == nb.cohort);
                lb = self.cov_of(ctx, nb, nb_coh) * self.wipe_of(ctx, nb, 1.0);
            }
            let natural = la.max(lb * RUN_TAIL_EASE);
            let floored = left
                .and_then(|l| self.attach_floor_at(ctx, row, l, 1.0))
                .map(|floor| natural.max(floor));
            let cov = floored.unwrap_or(natural);
            let here = (shape.0, shape.1, shape.2, shape.3, cov);
            if let Some((uniform, probe)) = attack
                && boundary == u32::from(caret_cell)
            {
                self.attach_floor = Some(AttachFloor {
                    row,
                    col: caret_cell,
                    base: cov,
                    uniform,
                    probe,
                });
            }
            let x = ox + boundary as f32 * cw;
            if let Some((px, p)) = prev {
                for j in 1..slabs {
                    let f = j as f32 / slabs_f;
                    let l = |u: f32, v: f32| u + (v - u) * f;
                    let own =
                        sa.3 * self.wipe_of(ctx, a, if caret_side_right { 1.0 - f } else { f });
                    let mut cov = l(p.4, here.4).min(own);
                    if let Some(floor) = self.attach_floor_at(ctx, row, a, f) {
                        cov = cov.max(floor);
                    }
                    self.plan.push(Segment {
                        x: retract.at(l(px, x)).round(),
                        spine: l(p.0, here.0),
                        up: l(p.1, here.1),
                        dn: l(p.2, here.2),
                        t: l(p.3, here.3),
                        cov: cov.clamp(0.0, 255.0) as u8,
                    });
                }
            }
            let rx = retract.at(x).round();
            self.plan.push(Segment {
                x: rx,
                spine: here.0,
                up: here.1,
                dn: here.2,
                t: here.3,
                cov: here.4.clamp(0.0, 255.0) as u8,
            });
            let continues =
                right.is_some() || (boundary == u32::from(col1) + 1 && right_nb.is_some());
            let onward = if floored.is_some() && continues {
                doubled = Some((boundary - u32::from(col0)) as usize);
                let seam = (shape.0, shape.1, shape.2, shape.3, natural);
                self.plan.push(Segment {
                    x: rx,
                    spine: seam.0,
                    up: seam.1,
                    dn: seam.2,
                    t: seam.3,
                    cov: seam.4.clamp(0.0, 255.0) as u8,
                });
                seam
            } else {
                here
            };
            prev = Some((x, onward));
        }
        if attach
            && !under_caret_leaving
            && let Some((px, p)) = prev
            && let Some(af) = self.attach_floor
        {
            let x = px + cw;
            for j in 1..=slabs {
                let f = j as f32 / slabs_f;
                let cov = af.level(self, ctx, f);
                self.plan.push(Segment {
                    x: retract.at(px + (x - px) * f).round(),
                    spine: p.0,
                    up: p.1,
                    dn: p.2,
                    t: p.3,
                    cov: cov.clamp(0.0, 255.0) as u8,
                });
            }
        }
        let hi = self.plan.len();
        if hi > lo {
            let head_boundary = usize::from(head_col - col0) + 1;
            let shift = usize::from(doubled.is_some_and(|d| d < head_boundary));
            let head = if stream_dir > 0 {
                lo
            } else {
                (lo + head_boundary * slabs + shift).min(hi - 1)
            };
            self.runs.push(Run {
                row,
                col0,
                col1,
                lo,
                hi,
                head_col,
                head,
                wet,
                at_caret,
                born,
                stream_dir,
                hold,
            });
        }
    }

    #[inline]
    fn attach_floor_at(&self, ctx: &Ctx<'_>, row: u16, cell: &Cell, dist: f32) -> Option<f32> {
        let af = self.attach_floor?;
        (af.row == row && af.col == cell.col && cell.leaving()).then(|| af.level(self, ctx, dist))
    }

    /// Draw the body into `frame.under` and the hot edge into `frame.out`.
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.ink_quads = 0;
        if self.plan.is_empty() || self.runs.is_empty() {
            return;
        }
        let ink_from = frame.under.len();
        let prof = Self::body_profile(ctx.cfg);
        let shoulder = prof.shoulder;
        #[cfg(test)]
        let alike = self.paper_alike && !ctx.cfg.dark_theme;
        #[cfg(not(test))]
        let alike = false;
        let rail = Self::rail_lit(ctx.cfg) && !alike;
        let chf = ctx.geom.ch as f32;
        let ceiling = if ctx.cfg.dark_theme {
            BODY_FRAME_TOP
        } else {
            light_role(ctx.cfg).alpha_cap()
        };
        let clip = ctx.geom.beam_clip();
        let budget = frame.under.len() + RIBBON_QUAD_BUDGET;
        let core_up = prof.core_up_ch * chf;
        let lift_span = LEAD_CH * chf;
        let mut verts = mem::take(&mut self.verts);
        let runs = mem::take(&mut self.runs);
        let stride = ctx.geom.cw.max(1).div_ceil(self.slabs_per_cell());
        let focused_multi_run_row = runs
            .iter()
            .find(|run| run.at_caret)
            .map(|run| run.row)
            .filter(|&row| runs.iter().filter(|run| run.row == row).take(2).count() > 1);
        let pane_bot = self.pane_bot(ctx, chf);
        let dn_floor = DN_FLOOR_CH * chf;
        let rail_reach_max = RAIL_REACH_MAX_CH * chf;
        #[cfg(test)]
        let rail_gap = if self.rail_flush {
            0.0
        } else {
            rail_gap_px(chf)
        };
        #[cfg(not(test))]
        let rail_gap = rail_gap_px(chf);
        let plan = &self.plan;
        let ink = &self.ink;
        let keep_out_ease = KEEP_OUT_EASE_CELLS * ctx.geom.cw as f32;
        let head_feather = |run: &Run| self.head_feather(ctx, run);
        // The flat ribbon on paper lies flatter than the pet's comet body:
        // lower, its plateau carried near its top.
        let flat_paper = !ctx.cfg.dark_theme && !Self::comet(ctx.cfg) && !alike;
        #[cfg(test)]
        let coarse = self.feather_coarse;
        let roomy = (self.plan.len() as f32) * FEATHER_ROOM_ROWS * chf
            < RIBBON_QUAD_BUDGET as f32 * FEATHER_ROOM_SHARE;
        // Each run's tail laid finely adds its fine windows to its plan's;
        // only a frame with room for a tail a row lays them (counted by
        // rows, so a row's runs lay alike together and apart).
        let roomy_ends = roomy && {
            let fine = stride.div_ceil(RUN_END_FINE_SPLIT).max(1);
            let end_windows = RUN_END_FINE_CELLS
                * (ctx.geom.cw as f32 / fine as f32 - self.slabs_per_cell() as f32);
            let tail_rows = runs
                .iter()
                .enumerate()
                .filter(|(i, run)| !runs[..*i].iter().any(|r| r.row == run.row))
                .count();
            (self.plan.len() as f32 + tail_rows as f32 * end_windows.max(0.0))
                * FEATHER_ROOM_ROWS
                * chf
                < RIBBON_QUAD_BUDGET as f32 * RUN_END_ROOM_SHARE
        };
        {
            let mut paint = |out: &mut Vec<GlowQuad>, run: &Run, rail_layer: bool| -> bool {
                let row_bottom = f32::from(ctx.geom.origin_y) + (f32::from(run.row) + 1.0) * chf;
                if rail_layer && (!rail || run.hold >= 1.0 || row_bottom >= pane_bot) {
                    return true;
                }
                let head_feather_of = head_feather(run);
                let feather = |x: f32| head_feather_of.at(x);
                #[cfg(test)]
                let roomy = roomy && !coarse;
                let fine_feather = if roomy {
                    head_feather_of
                } else {
                    HeadFeather {
                        width: 0.0,
                        ..head_feather_of
                    }
                };
                verts.clear();
                verts.reserve(run.hi - run.lo);
                let mut head_rev = 0usize;
                let head_rel = Some(run.head - run.lo);
                #[cfg(test)]
                let stair = self.tail_stair;
                #[cfg(not(test))]
                let stair = false;
                let end_fine = if roomy_ends && !stair {
                    (
                        RUN_END_FINE_CELLS * ctx.geom.cw as f32,
                        stride.div_ceil(RUN_END_FINE_SPLIT) as f32,
                    )
                } else {
                    (0.0, 0.0)
                };
                if rail_layer {
                    let rail_scale = RAIL_GAIN * (1.0 - run.hold);
                    for_each_feathered(&plan[run.lo..run.hi], fine_feather, end_fine, |i, seg| {
                        if i == head_rel {
                            head_rev = verts.len();
                        }
                        // The vivid rail lies a hairline below the row's bottom.
                        let top = (row_bottom + rail_gap).max(seg.spine);
                        let dn = (seg.spine + seg.dn - top).max(dn_floor).min(rail_reach_max);
                        verts.push(RibbonVertex {
                            x: seg.x,
                            spine: top,
                            up: 0.0,
                            dn,
                            core_up: 0.0,
                            core_dn: dn * RAIL_CORE_SHARE,
                            color: ink.rail_at(seg.t),
                            cov: seg.cov * rail_scale * feather(seg.x),
                            lift: 0.0,
                            lift_span: 0.0,
                        });
                    });
                } else {
                    let row_top = f32::from(ctx.geom.origin_y) + f32::from(run.row) * chf;
                    let has_ceiling = self.keep_out.has_ceiling_at(row_top);
                    for_each_feathered(&plan[run.lo..run.hi], fine_feather, end_fine, |i, seg| {
                        if i == head_rel {
                            head_rev = verts.len();
                        }
                        let cov = seg.cov * feather(seg.x);
                        let strip = (cov * STRIP_LIFT_GAIN).min(ceiling - cov).max(0.0);
                        let up = self.kept_up(
                            (seg.x, seg.spine, seg.up),
                            row_top,
                            keep_out_ease,
                            has_ceiling,
                        );
                        let (up, core) = if flat_paper {
                            let up = up * FLAT_PAPER_UP_SHARE;
                            (up, up * FLAT_PAPER_CORE_SHARE)
                        } else {
                            (up, core_up.min(up))
                        };
                        verts.push(RibbonVertex {
                            x: seg.x,
                            spine: seg.spine,
                            up,
                            dn: seg.dn,
                            core_up: core,
                            core_dn: 0.0,
                            color: ink.at(seg.t),
                            cov,
                            lift: strip,
                            lift_span,
                        });
                    });
                }
                let start = out.len();
                let shoulder = if rail_layer { 1.0 } else { shoulder };
                let mut complete =
                    ribbon_beam(out, clip, &verts, shoulder, stride, budget, GlowBlend::Over);
                let head = verts.len().saturating_sub(1 + head_rev);
                if !complete && head + 1 < verts.len() {
                    out.truncate(start);
                    verts.reverse();
                    complete = Self::repaint_capped_run_from_head(
                        out, clip, &mut verts, head, shoulder, stride, budget,
                    );
                }
                complete
            };
            let mut retry_body_first = false;
            let mut prior_rail_drawn = false;
            for (i, run) in runs.iter().enumerate() {
                if !paint(frame.under, run, false) {
                    retry_body_first =
                        prior_rail_drawn && focused_multi_run_row.is_some_and(|row| run.row == row);
                    break;
                }
                let rail_from = frame.under.len();
                if !paint(frame.under, run, true) {
                    retry_body_first = focused_multi_run_row
                        .is_some_and(|row| runs[i + 1..].iter().any(|later| later.row == row));
                    break;
                }
                prior_rail_drawn |= frame.under.len() > rail_from;
            }
            if retry_body_first {
                let row = focused_multi_run_row.expect("retry requires a multi-run focused row");
                frame.under.truncate(ink_from);
                let bodies_complete = runs
                    .iter()
                    .filter(|run| run.row == row)
                    .all(|run| paint(frame.under, run, false));
                if bodies_complete {
                    let rails_complete = runs
                        .iter()
                        .filter(|run| run.row == row)
                        .all(|run| paint(frame.under, run, true));
                    if rails_complete {
                        for run in runs.iter().filter(|run| run.row != row) {
                            if !paint(frame.under, run, false) || !paint(frame.under, run, true) {
                                break;
                            }
                        }
                    }
                }
            }
        }
        self.runs = runs;
        self.verts = verts;
        self.emit_hot_edge(ctx, frame);
        let bg = ctx.cfg.theme_bg;
        let mut last_ink: Option<(u32, u8, bool)> = None;
        self.ink_quads = frame.under[ink_from..]
            .iter()
            .filter(|q| {
                if let Some((prev_c, prev_a, res)) = last_ink
                    && prev_c == q.color
                    && prev_a == q.alpha
                {
                    return res;
                }
                let res = reads_as_ink(over_premul(bg, q.color, q.alpha));
                last_ink = Some((q.color, q.alpha, res));
                res
            })
            .count();
    }

    /// The reach `up` of a body boundary at window x `x` standing on
    /// `spine` in `row`, cut so its top (and the feather it wears there)
    /// stays in its own row under a [`KeepOut`] box standing on the row.
    ///
    /// [`Self::kept_up`] with the top held `inset` px inside the row under
    /// a box: a line drawn centred on the top (the hot edge) keeps its
    /// whole thickness off the box.
    #[inline]
    const fn kept_up(
        &self,
        (x, spine, up): (f32, f32, f32),
        row_top: f32,
        ease: f32,
        has_ceiling: bool,
    ) -> f32 {
        if !has_ceiling {
            return up;
        }
        let top = spine - up;
        if top >= row_top {
            return up;
        }
        let ceiling = self.keep_out.ceiling(x, row_top, top, ease);
        (spine - ceiling.max(top)).max(0.0)
    }

    #[inline]
    const fn head_feather(&self, ctx: &Ctx<'_>, run: &Run) -> HeadFeather {
        let plan = self.plan.as_slice();
        let head_x = if run.head < plan.len() {
            plan[run.head].x
        } else {
            0.0
        };
        let past = if run.stream_dir < 0 {
            run.head_col.checked_add(1)
        } else {
            run.head_col.checked_sub(1)
        };
        let meets = match past {
            Some(col) => self.index.at(run.row, col).is_some(),
            None => false,
        };
        let at_end = (run.head == run.lo || run.head + 1 == run.hi) && !meets;
        let at_end = at_end && self.curtain.is_none();
        #[cfg(test)]
        if self.head_square {
            return HeadFeather { head_x, width: 0.0 };
        }
        let width = if !run.at_caret && at_end {
            HEAD_FEATHER_CELLS * ctx.geom.cw as f32
        } else {
            0.0
        };
        HeadFeather { head_x, width }
    }

    fn repaint_capped_run_from_head(
        out: &mut Vec<GlowQuad>,
        clip: BeamClip,
        verts: &mut [RibbonVertex],
        head: usize,
        shoulder: f32,
        stride: usize,
        budget: usize,
    ) -> bool {
        debug_assert!(head < verts.len());
        verts[..=head].reverse();
        let left_complete = ribbon_beam(
            out,
            clip,
            &verts[..=head],
            shoulder,
            stride,
            budget,
            GlowBlend::Over,
        );
        verts[..=head].reverse();
        left_complete
            && ribbon_beam(
                out,
                clip,
                &verts[head..],
                shoulder,
                stride,
                budget,
                GlowBlend::Over,
            )
    }

    /// How many of the last frame's ribbon quads read as band ink on the glass.
    #[inline]
    #[must_use]
    pub const fn ink_quad_count(&self) -> usize {
        self.ink_quads
    }

    /// The hot edge's gain.
    #[inline]
    #[must_use]
    pub const fn hot_edge_gain(cfg: &Config, disp: f32, typed: bool) -> f32 {
        if !cfg.dark_theme || cfg.reduced_motion {
            return 0.0;
        }
        let floor = if typed { HOT_EDGE_GAIN_FLOOR } else { 0.0 };
        floor
            + (1.0 - floor) * smoothstep01((clamp01(disp) - HOT_EDGE_DISP_MIN) / HOT_EDGE_DISP_SPAN)
    }

    fn emit_hot_edge(&self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        if !ctx.cfg.dark_theme || ctx.cfg.reduced_motion {
            return;
        }
        let Some(run) = self.runs.first() else {
            return;
        };
        #[cfg(not(test))]
        let left_lit = false;
        #[cfg(test)]
        let left_lit = self.hot_edge_left_lit;
        if !run.wet || !(run.at_caret || left_lit) {
            return;
        }
        let Some(head_cell) = self
            .cells
            .iter()
            .filter(|c| c.row == run.row && c.col == run.head_col)
            .max_by(|a, b| a.born.cmp(&b.born))
        else {
            return;
        };
        let base_gain = Self::hot_edge_gain(ctx.cfg, head_cell.birth_disp, head_cell.typing);
        if base_gain <= 0.0 {
            return;
        }
        let gain = base_gain * self.env_of(ctx, head_cell);
        if gain <= 0.0 {
            return;
        }
        let mouth_x = self.plan[run.head].x;
        let dir = f32::from(run.stream_dir);
        let cells = head_cell
            .edge_cells
            .clamp(HOT_EDGE_CELLS, HOT_EDGE_CELLS_MAX);
        let cw = ctx.geom.cw as f32;
        let seam = self
            .seam
            .filter(|s| s.row == run.row && run.at_caret && dir < 0.0)
            .and_then(|s| {
                let floor_x =
                    f32::from(ctx.geom.origin_x) + f32::from(self.hand_floor_on(run.row)?) * cw;
                let lvl = SEAM_LEVEL
                    * spend(clamp01(
                        ctx.now.saturating_duration_since(s.at).as_secs_f32() / FLOW_TOTAL_S,
                    ));
                (lvl > 0.0).then_some((floor_x, lvl))
            });
        let reach = cells * cw;
        let seam_lvl = seam.map_or(0.0, |(_, l)| l);
        let intensity = clamp01(ctx.cfg.intensity);
        let gain_intensity = gain * intensity;
        let feather = self.head_feather(ctx, run);
        // The edge is a line a pixel thick centred on the body's top: held at
        // the row's top under a lettered or filled cell above, its upper half
        // would lie on the cell's foot, so it is held half its thickness in.
        #[cfg(test)]
        let inset = if self.hot_edge_on_bar {
            0.0
        } else {
            0.5 * HOT_EDGE_THICKNESS
        };
        #[cfg(not(test))]
        let inset = 0.5 * HOT_EDGE_THICKNESS;
        let inv_reach = 1.0 / reach;
        let neg_dir = -dir;
        let row_top =
            f32::from(ctx.geom.origin_y) + f32::from(run.row) * (ctx.geom.ch as f32) + inset;
        let keep_out_ease = KEEP_OUT_EASE_CELLS * cw;
        let has_ceiling = self.keep_out.has_ceiling_at(row_top);
        frame.beams.clear();
        for seg in self.plan[run.lo..run.hi]
            .iter()
            .step_by(self.slabs_per_cell())
        {
            let behind = (mouth_x - seg.x) * neg_dir;
            let in_seam = seam.is_some_and(|(floor_x, _)| behind >= 0.0 && seg.x >= floor_x);
            if !(0.0..=reach).contains(&behind) && !in_seam {
                continue;
            }
            let a_rel = (1.0 - behind * inv_reach).max(0.0).powi(2).max(if in_seam {
                seam_lvl
            } else {
                0.0
            });
            let cov = (HOT_EDGE_COV_MAX * a_rel * gain_intensity * feather.at(seg.x))
                .clamp(0.0, HOT_EDGE_COV_MAX);
            frame.beams.push(BeamVertex {
                x: seg.x,
                y: seg.spine
                    - self.kept_up(
                        (seg.x, seg.spine, seg.up),
                        row_top,
                        keep_out_ease,
                        has_ceiling,
                    ),
                color: self.ink.hot_at(seg.t),
                cov: cov as u8,
            });
        }
        if frame.beams.len() < 2 {
            frame.beams.clear();
            return;
        }
        comet_beam(
            frame.out,
            ctx.geom.beam_clip(),
            frame.beams,
            HOT_EDGE_THICKNESS,
            1,
            0.0,
        );
        frame.beams.clear();
    }

    /// The field at a cell, or `None`.
    #[inline]
    #[must_use]
    pub const fn field_at(&self, row: u16, col: u16) -> Option<f32> {
        self.index.at(row, col)
    }

    /// The walk at a cell.
    #[inline]
    #[must_use]
    pub const fn walk_origin_at(&self, row: u16, col: u16) -> Option<(f32, f32)> {
        let cells = self.cells.as_slice();
        let mut i = cells.len();
        let mut owner_cohort = None;
        while i > 0 {
            i -= 1;
            let c = &cells[i];
            if c.row == row && c.col == col {
                owner_cohort = Some(c.cohort);
                break;
            }
        }
        let Some(cohort_id) = owner_cohort else {
            return None;
        };
        let cohorts = self.cohorts.as_slice();
        let mut j = 0;
        while j < cohorts.len() {
            let c = &cohorts[j];
            if c.id == cohort_id {
                return Some((c.walk_d(col), c.walk_zero()));
            }
            j += 1;
        }
        None
    }

    /// The caret's position in the field (seam point 4).
    #[inline]
    #[must_use]
    pub fn field_at_caret(&self) -> f32 {
        if self.cells.is_empty() {
            return 0.0;
        }
        if let Some(t) = self.caret.and_then(|(row, col)| self.field_at(row, col)) {
            return t;
        }
        let caret_row = self.caret.map(|(row, _)| row);
        let mut best_on_row: Option<(Instant, f32)> = None;
        let mut best_any: Option<(Instant, f32)> = None;
        for c in &self.cells {
            if !c.typing || c.leaving() {
                continue;
            }
            if caret_row == Some(c.row) && best_on_row.is_none_or(|(b, _)| c.born >= b) {
                best_on_row = Some((c.born, c.t));
            }
            if best_any.is_none_or(|(b, _)| c.born >= b) {
                best_any = Some((c.born, c.t));
            }
        }
        best_on_row.or(best_any).map_or(0.0, |(_, t)| t)
    }

    /// The source RGB a companion cursor inherits to meet the laid ribbon with no palette seam (seam point 5).
    #[inline]
    #[must_use]
    pub fn head_rgb(&self, cfg: &Config) -> Option<u32> {
        self.caret?;
        Some(caret_ink(spectrum(walk_arc(self.field_at_caret())), cfg))
    }

    /// The body's vertical extent at a cell, or `None` where no cell is laid (§4.2, §5.4, D16).
    #[inline]
    #[must_use]
    pub const fn band(&self, row: u16, col: u16) -> Option<Band> {
        let runs = self.runs.as_slice();
        let mut i = 0;
        let mut found = None;
        while i < runs.len() {
            let r = &runs[i];
            if r.row == row && r.col0 <= col && col <= r.col1 {
                found = Some(r);
                break;
            }
            i += 1;
        }
        let Some(run) = found else {
            return None;
        };
        let slabs = self.slabs_per_cell();
        let idx = run.lo + (col - run.col0) as usize * slabs + slabs / 2;
        let cap = run.hi.saturating_sub(1);
        let target = if idx < cap { idx } else { cap };
        let plan = self.plan.as_slice();
        if target >= plan.len() {
            return None;
        }
        let seg = &plan[target];
        Some(Band {
            top: seg.spine - seg.up,
            spine: seg.spine,
            bottom: seg.spine + seg.dn,
        })
    }

    /// Live cells — `Status::cells` and one term of the idle test.
    #[inline]
    #[must_use]
    pub const fn live_cells(&self) -> usize {
        self.cells.len()
    }

    /// The planned boundaries lit on the last frame, at or over [`STATUS_LIT_COV`].
    #[inline]
    pub fn lit_segments(&self) -> impl Iterator<Item = &Segment> {
        self.plan.iter().filter(|s| s.cov >= STATUS_LIT_COV)
    }

    /// Count of planned boundaries lit on the last frame, at or over [`STATUS_LIT_COV`].
    #[inline]
    #[must_use]
    pub const fn lit_segment_count(&self) -> usize {
        let plan = self.plan.as_slice();
        let mut count = 0;
        let mut i = 0;
        while i < plan.len() {
            if plan[i].cov >= STATUS_LIT_COV {
                count += 1;
            }
            i += 1;
        }
        count
    }

    /// What is on the glass, by plan.
    #[cfg(test)]
    pub fn ink_segments(&self) -> impl Iterator<Item = &Segment> {
        let bg = self.ground;
        let ink = &self.ink;
        self.plan.iter().filter(move |s| {
            if s.cov == 0 {
                return false;
            }
            let a = (f32::from(s.cov) * (1.0 + STRIP_LIFT_GAIN)).min(BODY_FRAME_TOP) as u8;
            reads_as_ink(over_premul(bg, premul_rgb(ink.at(s.t), a), a))
        })
    }

    /// Milliseconds left in a falling curtain, or `None` when none is falling.
    #[inline]
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        let at = self.curtain?;
        let r = now.saturating_duration_since(at).as_secs_f32();
        Some(((CURTAIN_S - r).max(0.0) * 1000.0).round() as u32)
    }

    /// True when nothing is laid.
    #[inline]
    #[must_use]
    pub const fn at_rest(&self) -> bool {
        self.cells.is_empty() && self.cohorts.is_empty()
    }

    /// The cadence law, brisk half.
    #[inline]
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        let ramping = |at: Instant| now.saturating_duration_since(at).as_secs_f32() < EDGE_IN_S;
        (!self.reduced && self.curtain.is_some() && !self.cells.is_empty())
            || (!self.reduced && self.seam.is_some() && !self.cells.is_empty())
            || self.rearm.is_some_and(|(at, _)| ramping(at))
            || self.translated_at.is_some_and(|at| {
                now.saturating_duration_since(at)
                    < super::witness::TWIN_MIN + std::time::Duration::from_millis(16)
            })
            || self.cells.iter().rev().any(|c| ramping(c.attack_at))
            || (!self.reduced
                && self
                    .cohorts
                    .iter()
                    .any(|c| c.phase_at(c.idle_s(now)) == Some(Phase::Retracting)))
    }

    /// Seconds until the mark's settle under a falling spine moves a pixel.
    #[inline]
    #[must_use]
    pub const fn settle_step_s(&self, disp: f32) -> Option<f32> {
        if self.cells.is_empty() || !disp.is_finite() || disp <= 0.0 {
            return None;
        }
        let wedge = self.wedge_px * disp / DISP_RELEASE_TAU;
        let wave = if self.reduced {
            0.0
        } else {
            WAVE_AMP_CELLS * self.cell_h * disp * std::f32::consts::TAU * PHASE_RATE * disp
        };
        let rate = wedge + wave;
        if rate > 0.0 { Some(1.0 / rate) } else { None }
    }

    /// The cadence law.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        if self.cells.is_empty()
            && self.cohorts.is_empty()
            && self.ember_at.is_none()
            && self.curtain.is_none()
        {
            return None;
        }
        let mut cad = Cadence::at(now);
        if self.brisk(now) {
            cad.brisk();
            return cad.take();
        }
        let since = |at: Instant| now.saturating_duration_since(at).as_secs_f32();
        let dur = std::time::Duration::from_secs_f32;
        let reduced_fade_s = REDUCED_MOTION_FADE_MS / 1000.0;
        let ember_left = self.ember_at.map(|at| FOCUS_EMBER_S - since(at));
        let ember_u = self.ember_at.map(|at| clamp01(since(at) / FOCUS_EMBER_S));
        let ember_spend = ember_u.map(spend);
        let mut cached_coh: Option<(u32, Option<&Cohort>)> = None;
        for cell in &self.cells {
            if cad.at_tail_floor() {
                break;
            }
            let coh = match cached_coh {
                Some((id, c)) if id == cell.cohort => c,
                _ => {
                    let c = self.cohorts.iter().find(|c| c.id == cell.cohort);
                    cached_coh = Some((cell.cohort, c));
                    c
                }
            };
            let age = since(coh.map_or(cell.born, |c| cell.born.max(c.alive_at)));
            let life = cell.life_s.max(1e-3);
            let flowing = coh.is_some_and(|c| c.flow.is_some());
            if !flowing {
                cad.tail(cell.life_s - age);
            }
            if cell
                .retract_at
                .is_some_and(|at| since(at) >= RETRACT_FADE_S)
            {
                continue;
            }
            if self.reduced {
                let ends = [
                    (!flowing).then_some(cell.life_s - age),
                    cell.retract_at.map(|at| RETRACT_FADE_S - since(at)),
                    cell.retire_at.map(|at| RETIRE_MELT_S - since(at)),
                    ember_left,
                    coh.map(|c| c.swoosh_total() - c.idle_s(now)),
                ];
                for left in ends.into_iter().flatten() {
                    cad.tail((left - reduced_fade_s).max(0.0));
                }
                continue;
            }
            let base = UNDER_COV_CAP * cell.cov0;
            let u = clamp01(age / life);
            let melt = expiry_melt(u);
            let retract_u = cell
                .retract_at
                .map(|at| clamp01(since(at) / RETRACT_FADE_S));
            let swoosh_u = coh.and_then(|c| {
                let idle = c.idle_s(now);
                if c.phase_at(idle) != Some(Phase::Fading) {
                    return None;
                }
                let t0 = c.retract_start() + c.retract_span() * (1.0 - c.drain_rank(cell.col));
                Some(clamp01((idle - t0) / RETRACT_FADE_S))
            });
            let retire_u = cell.retire_at.map(|at| clamp01(since(at) / RETIRE_MELT_S));
            let f_r = retract_u.map_or(1.0, spend);
            let f_s = swoosh_u.map_or(1.0, spend);
            let f_m = retire_u.map_or(1.0, spend);
            let carried = base * melt * f_r * f_s * f_m;
            let (ember_floor, f_e) = match ember_spend {
                Some(s) => {
                    let floor = self.ember_floor_of(carried, cell.t);
                    (floor, ember_level(floor, s))
                }
                None => (0.0, 1.0),
            };
            if !flowing && let Some(step) = level_step_melt(base * f_r * f_e * f_s * f_m, u, life) {
                cad.tail(step);
            }
            if let Some(ur) = retract_u
                && let Some(step) =
                    level_step_spend(base * melt * f_e * f_s * f_m, ur, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(ue) = ember_u
                && let Some(step) =
                    level_step_spend(carried * (1.0 - ember_floor).max(0.0), ue, FOCUS_EMBER_S)
            {
                cad.tail(step);
            }
            if let Some(us) = swoosh_u
                && let Some(step) =
                    level_step_spend(base * melt * f_r * f_e * f_m, us, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(um) = retire_u
                && let Some(step) =
                    level_step_spend(base * melt * f_r * f_e * f_s, um, RETIRE_MELT_S)
            {
                cad.tail(step);
            }
            if let Some(at) = cell.retract_at {
                cad.tail_at(at);
                cad.tail_at(at + dur(RETRACT_FADE_S));
            }
            if let Some(at) = cell.retire_at {
                cad.tail_at(at + dur(RETIRE_MELT_S));
            }
        }
        for coh in &self.cohorts {
            // Edges: the grace's end, the two beats after it, the retract's first frame and the swoosh's end; the retract's end is a tail.
            if coh.flow.is_some() {
                cad.edge(coh.clock() + dur(coh.swoosh_total()));
                if !self.reduced {
                    cad.tail_at(coh.clock() + dur(coh.retract_span()));
                }
                continue;
            }
            let grace = coh.grace_s.max(PHRASE_REST_MIN_S);
            for beat in [
                grace,
                grace + REACH_STEP_S,
                grace + 2.0 * REACH_STEP_S,
                coh.retract_start(),
                coh.swoosh_total(),
            ] {
                cad.edge(coh.alive_at + dur(beat));
            }
            cad.tail_at(coh.alive_at + dur(coh.retract_start() + coh.retract_span()));
        }
        if let Some(at) = self.ember_at {
            cad.tail_at(at + dur(FOCUS_EMBER_S));
        }
        if let Some(at) = self.curtain {
            cad.tail_at(at + dur(CURTAIN_S));
        }
        cad.take()
    }

    /// The band follows its text along the row.
    pub fn shift_run(
        &mut self,
        run: super::witness::ShiftRun,
        moved: &mut Vec<(u16, u16, Instant)>,
    ) -> usize {
        let super::witness::ShiftRun {
            row,
            cohort,
            lo,
            hi,
            dc,
        } = run;
        if dc == 0 || lo > hi {
            return 0;
        }
        let Some(ci) = self
            .cohorts
            .iter()
            .position(|c| c.id == cohort && c.row == row)
        else {
            return 0;
        };
        let carry_partial = self.partial_custody_follows(ci, lo, hi, true);
        let mut i = 0usize;
        while i < self.cells.len() {
            let c = self.cells[i];
            let stale = c.row == row
                && c.cohort == cohort
                && !c.leaving()
                && !(lo..=hi).contains(&c.col)
                && c.col.checked_add_signed(-dc).is_some_and(|src| {
                    (lo..=hi).contains(&src)
                        && self.cells.iter().any(|d| {
                            d.row == row && d.cohort == cohort && !d.leaving() && d.col == src
                        })
                });
            if stale {
                self.cells.remove(i);
            } else {
                i += 1;
            }
        }
        let walk = self.cohorts[ci];
        let mut n = 0usize;
        let (mut c0, mut c1) = (u16::MAX, 0u16);
        for c in &mut self.cells {
            if c.row != row || c.cohort != cohort {
                continue;
            }
            if ((!c.leaving() && (lo..=hi).contains(&c.col))
                || (carry_partial && c.col >= lo && walk.holds_partial(c)))
                && let Some(col) = c.col.checked_add_signed(dc)
            {
                moved.push((c.row, c.col, c.born));
                c.col = col;
                c.t = walk.t_at(col);
                self.followed_now.push((row, col, c.born));
                n += 1;
            }
            c0 = c0.min(c.col);
            c1 = c1.max(c.col.saturating_add(1));
        }
        if n == 0 {
            return 0;
        }
        self.shifted_now.push((row, lo, hi, dc));
        let coh = &mut self.cohorts[ci];
        coh.col0 = c0;
        coh.col1 = c1;
        if coh.flow.is_some()
            && let Some(edge) = coh.retract_col
            && (lo..=hi).contains(&edge)
            && let Some(edge) = edge.checked_add_signed(dc)
        {
            coh.retract_col = Some(edge);
        }
        self.index.reset();
        self.plan.clear();
        self.runs.clear();
        n
    }

    fn partial_custody_follows(&self, ci: usize, lo: u16, hi: u16, suffix: bool) -> bool {
        let Some(cohort) = self.cohorts.get(ci) else {
            return false;
        };
        !cohort.abandoned
            && self
                .cells
                .iter()
                .filter(|cell| cell.cohort == cohort.id && !cell.leaving())
                .all(|cell| (suffix && cell.col < lo) || (lo..=hi).contains(&cell.col))
    }

    /// The band follows its text across rows.
    pub fn translate_run(
        &mut self,
        run: super::witness::FollowRun,
        at: Instant,
        away: bool,
        moved: &mut Vec<(u16, u16, Instant)>,
    ) -> usize {
        let super::witness::FollowRun {
            row,
            cohort,
            lo,
            hi,
            dr,
        } = run;
        let Some(ci) = self
            .cohorts
            .iter()
            .position(|c| c.id == cohort && c.row == row)
        else {
            return 0;
        };
        let Some(new_row) = row.checked_add_signed(dr) else {
            return 0;
        };
        if dr == 0 || lo > hi {
            return 0;
        }
        let source = self.cohorts[ci];
        let carry_partial = self.partial_custody_follows(ci, lo, hi, false);
        let carried = |cell: &Cell| {
            (lo..=hi).contains(&cell.col) || (carry_partial && source.holds_partial(cell))
        };
        let mut n = 0usize;
        let mut all = 0usize;
        let (mut c0, mut c1) = (u16::MAX, 0u16);
        let (mut k0, mut k1) = (u16::MAX, 0u16);
        for c in self
            .cells
            .iter()
            .filter(|c| c.row == row && c.cohort == cohort)
        {
            all += 1;
            if carried(c) {
                n += 1;
                c0 = c0.min(c.col);
                c1 = c1.max(c.col + 1);
            } else {
                k0 = k0.min(c.col);
                k1 = k1.max(c.col + 1);
            }
        }
        if n == 0 {
            return 0;
        }
        let id = if all == n {
            self.cohorts[ci].row = new_row;
            cohort
        } else {
            let mut copy = self.cohorts[ci];
            copy.id = self.next_cohort;
            self.next_cohort = self.next_cohort.wrapping_add(1);
            copy.row = new_row;
            copy.release_clock = None;
            copy.col0 = c0;
            copy.col1 = c1;
            if copy.flow.is_some() && c0 < c1 {
                copy.retract_col = copy.retract_col.map(|e| e.clamp(c0, c1 - 1));
                copy.flow_drift = copy
                    .flow_drift
                    .min(FLOW_DRIFT_MAX_SHARE * f32::from(c1 - c0));
            }
            let old = &mut self.cohorts[ci];
            old.col0 = k0;
            old.col1 = k1;
            old.release_clock = None;
            self.cohorts.push(copy);
            if away {
                self.split_off = Some(SplitOff {
                    copy: copy.id,
                    dr,
                    col0: k0,
                    col1: k1,
                });
            }
            copy.id
        };
        for c in &mut self.cells {
            if c.row == row && c.cohort == cohort && carried(c) {
                moved.push((c.row, c.col, c.born));
                c.row = new_row;
                c.cohort = id;
                self.followed_now.push((new_row, c.col, c.born));
            }
        }
        if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == id) {
            if away {
                Self::flow_cohort(coh, at);
            } else if coh.flow.take().is_some() {
                coh.retract_col = None;
                coh.flow_drift = 0.0;
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
            }
        }
        if !away {
            self.translated_at = Some(at);
            if self.seam.is_some_and(|s| s.row == new_row) {
                self.seam = None;
            }
        }
        // A run carries its own prior shape, including across multiple hops
        // in this batch. Never mutate another run's prior row state here.
        let previous = self.cohort_hold(cohort, row);
        self.stage_hold(id, previous);
        self.index.reset();
        self.plan.clear();
        self.runs.clear();
        n
    }

    /// A scroll carries the ribbon with its text (seam point 12).
    pub fn translate_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.cells.retain_mut(|c| {
            if c.row < rows {
                return false;
            }
            c.row -= rows;
            true
        });
        self.cohorts.retain_mut(|c| {
            if c.row < rows {
                return false;
            }
            c.row -= rows;
            true
        });
        let cells = &self.cells;
        self.cohorts
            .retain(|coh| cells.iter().any(|c| c.cohort == coh.id));
        let mut last_coh: Option<(u32, bool)> = None;
        self.cells.retain(|c| match last_coh {
            Some((id, keep)) if id == c.cohort => keep,
            _ => {
                let keep = self.cohorts.iter().any(|coh| coh.id == c.cohort);
                last_coh = Some((c.cohort, keep));
                keep
            }
        });
        self.caret = self.caret.map(|(row, col)| (row.saturating_sub(rows), col));
        self.pen = self
            .pen
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.arrived = self
            .arrived
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.hand_floor = self
            .hand_floor
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.last_space = self
            .last_space
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.prev_space = self
            .prev_space
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.attach = self
            .attach
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.attach_floor = None;
        self.seam = self.seam.filter(|s| s.row >= rows).map(|s| Seam {
            row: s.row - rows,
            ..s
        });
        self.relocate = self.relocate.filter(|r| r.row >= rows).map(|r| Relocate {
            row: r.row - rows,
            ..r
        });
        self.carry = None;
        if let Some(proof) = &mut self.short_reanchor {
            proof.from.0 = proof.from.0.saturating_sub(rows);
            proof.to.0 = proof.to.0.saturating_sub(rows);
        }
        if let Some(proof) = &mut self.rewrap_start {
            proof.from.0 = proof.from.0.saturating_sub(rows);
            proof.to.0 = proof.to.0.saturating_sub(rows);
        }
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = gate.left_row.and_then(|row| row.checked_sub(rows));
        }
        self.holds.retain_mut(|h| match h.0.checked_sub(rows) {
            Some(row) => {
                h.0 = row;
                true
            }
            None => false,
        });
        #[cfg(test)]
        if self.holds_left_behind {
            self.holds.clear();
        }
        self.index.translate(rows);
        self.plan.clear();
        self.runs.clear();
        self.scroll_followed = true;
    }

    /// A row band carries its ribbon with its text (seam point 12).
    pub fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.cells
            .retain_mut(|c| match band_row(c.row, top, bottom, delta) {
                Some(row) => {
                    c.row = row;
                    true
                }
                None => false,
            });
        self.cohorts
            .retain_mut(|c| match band_row(c.row, top, bottom, delta) {
                Some(row) => {
                    c.row = row;
                    true
                }
                None => false,
            });
        let cells = &self.cells;
        self.cohorts
            .retain(|coh| cells.iter().any(|c| c.cohort == coh.id));
        let mut last_coh: Option<(u32, bool)> = None;
        self.cells.retain(|c| match last_coh {
            Some((id, keep)) if id == c.cohort => keep,
            _ => {
                let keep = self.cohorts.iter().any(|coh| coh.id == c.cohort);
                last_coh = Some((c.cohort, keep));
                keep
            }
        });
        self.caret = self
            .caret
            .map(|(row, col)| (band_pos(row, top, bottom, delta), col));
        self.pen = self
            .pen
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.arrived = self
            .arrived
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.hand_floor = self
            .hand_floor
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.last_space = self
            .last_space
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.prev_space = self
            .prev_space
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.attach = self
            .attach
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.attach_floor = None;
        self.seam = self
            .seam
            .and_then(|s| band_row(s.row, top, bottom, delta).map(|row| Seam { row, ..s }));
        self.relocate = self
            .relocate
            .and_then(|r| band_row(r.row, top, bottom, delta).map(|row| Relocate { row, ..r }));
        self.carry = None;
        if let Some(proof) = &mut self.short_reanchor {
            proof.from.0 = band_pos(proof.from.0, top, bottom, delta);
            proof.to.0 = band_pos(proof.to.0, top, bottom, delta);
        }
        if let Some(proof) = &mut self.rewrap_start {
            proof.from.0 = band_pos(proof.from.0, top, bottom, delta);
            proof.to.0 = band_pos(proof.to.0, top, bottom, delta);
        }
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = gate
                .left_row
                .and_then(|row| band_row(row, top, bottom, delta));
        }
        self.holds
            .retain_mut(|h| match band_row(h.0, top, bottom, delta) {
                Some(row) => {
                    h.0 = row;
                    true
                }
                None => false,
            });
        #[cfg(test)]
        if self.holds_left_behind {
            self.holds.clear();
        }
        self.index.translate_band(top, bottom, delta);
        self.plan.clear();
        self.runs.clear();
        self.scroll_followed = true;
    }

    /// [`super::Engine::lift_hand`]'s ribbon half.
    pub(super) fn lift_hand(&mut self, row: u16, to: u16) {
        let map = |r: u16| if r == row { to } else { r };
        self.caret = self.caret.map(|(r, c)| (map(r), c));
        self.pen = self.pen.map(|(r, c)| (map(r), c));
        self.arrived = self.arrived.map(|(r, c)| (map(r), c));
        self.hand_floor = self.hand_floor.map(|(r, c)| (map(r), c));
        self.last_space = self.last_space.map(|(r, c)| (map(r), c));
        self.prev_space = self.prev_space.map(|(r, c)| (map(r), c));
        self.attach = self.attach.map(|(r, c)| (map(r), c));
        self.attach_floor = None;
        self.relocate = self.relocate.map(|r| Relocate {
            row: map(r.row),
            ..r
        });
        if let Some((sw, _)) = &mut self.carry {
            sw.origin.0 = map(sw.origin.0);
            sw.landing.0 = map(sw.landing.0);
        }
        if let Some(proof) = &mut self.short_reanchor {
            proof.from.0 = map(proof.from.0);
            proof.to.0 = map(proof.to.0);
        }
        if let Some(proof) = &mut self.rewrap_start {
            proof.from.0 = map(proof.from.0);
            proof.to.0 = map(proof.to.0);
        }
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = gate.left_row.map(map);
        }
    }

    /// The seam on the hand's lifted line ([`super::Engine::lift_hand`]) rides from `row` to `to` with it.
    #[inline]
    pub(super) const fn lift_seam(&mut self, row: u16, to: u16) {
        if let Some(seam) = &mut self.seam
            && seam.row == row
        {
            seam.row = to;
        }
    }

    /// The seam's row ([`Ribbon::seam`]), for the engine's tests.
    #[cfg(test)]
    pub(super) const fn seam_row(&self) -> Option<u16> {
        match self.seam {
            Some(s) => Some(s.row),
            None => None,
        }
    }

    /// Drop everything — style switch, layout change, `Engine::reset`.
    #[inline]
    pub fn reset(&mut self) {
        self.cells.clear();
        self.cohorts.clear();
        self.holds.clear();
        self.holds_next.clear();
        self.carried_holds.clear();
        self.plan.clear();
        self.runs.clear();
        self.sorted.clear();
        self.verts.clear();
        self.index.reset();
        self.caret = None;
        self.ember_at = None;
        self.rearm = None;
        self.clear_space_transient_state();
        self.ink_quads = 0;
        self.last_walk = None;
        self.fresh_line = None;
        self.seam = None;
        self.phrase_open = None;
        self.curtain = None;
    }

    #[inline]
    fn clear_space_transient_state(&mut self) {
        self.pending_erase = 0;
        self.pen = None;
        self.arrived = None;
        self.hand_floor = None;
        self.last_space = None;
        self.prev_space = None;
        self.relocate = None;
        self.carry = None;
        self.split_off = None;
        self.short_reanchor = None;
        self.rewrap_start = None;
        self.followed_now.clear();
        self.shifted_now.clear();
        self.scroll_followed = false;
        self.attach = None;
        self.attach_floor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::{Geom, SoundCue};

    #[test]
    fn the_dark_bed_keeps_its_stops_chroma() {
        // Under `#d0d0d0` text the dark bed is held to some 0.07 of
        // luminance. Every named stop keeps a vivid colour there (OKLab
        // chroma), the walk's warm half (red to green) draws little of its
        // length in the olive and brown basin, and the letters' budget
        // holds. (The green-to-blue crossing's teal is the same physics but
        // reads as no mud, so the bend leaves hues past 90° as they are.)
        let budget = bed_luma_budget(0x00D0_D0D0);
        let chroma = |rgb: u32| {
            let [_, a, b] = crate::color_math::oklab(rgb);
            a.hypot(b)
        };
        let warm = (0..BED_INK_LUT_LEN)
            .take_while(|&i| {
                let (h, ..) = rgb2hsv(walk_arc_at(i));
                h <= 120.0
            })
            .count();
        let muddy = |ink: fn(u32, f32) -> u32| {
            (0..warm)
                .filter(|&i| chroma(ink(walk_arc_at(i), budget)) < 0.105)
                .count()
        };
        for (i, &stop) in crate::spectrum::SPECTRUM_ANCHORS.iter().enumerate() {
            let ink = bed_ink(stop, budget);
            assert!(
                chroma(ink) >= 0.11,
                "stop {i} holds to #{ink:06x}, chroma {:.3}",
                chroma(ink)
            );
            assert!(
                relative_luminance(ink) <= budget + 0.002,
                "stop {i} #{ink:06x} passes the letters' budget"
            );
        }
        let (bent, unbent) = (muddy(bed_ink), muddy(bed_ink_unbent));
        assert!(warm > 40, "fixture: the warm half spans {warm} stations");
        assert!(
            5 * bent <= warm,
            "{bent} of the warm half's {warm} stations hold to mud"
        );
        assert!(
            3 * unbent > warm,
            "control: unbent, {unbent} of {warm} warm stations hold to mud"
        );
        let yellow = bed_ink_unbent(0x00FF_FF00, budget);
        assert!(
            chroma(yellow) < 0.1,
            "control: unbent, yellow holds to #{yellow:06x}, chroma {:.3}",
            chroma(yellow)
        );
    }
    use aterm_render::{GlowQuad, RainHalo, over_premul, premul_rgb};
    use std::time::Duration;

    use super::super::{CaretSeam, Dir, TypedClass};

    const DEFAULT_BG: u32 = 0x001A_1B26;
    const DEFAULT_FG: u32 = 0x00C8_D3F5;
    const NORD_BG: u32 = 0x002E_3440;
    const NORD_FG: u32 = 0x00D8_DEE9;

    const SHIPPED_BG: u32 = 0x0011_1318;
    const SHIPPED_FG: u32 = 0x00D0_D0D0;

    const fn geom() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 1080,
            win_h: 720,
            head: 0,
        }
    }

    const fn geom2x() -> Geom {
        Geom {
            cw: 18,
            ch: 36,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 2160,
            win_h: 1440,
            head: 0,
        }
    }

    const fn cfg(dark: bool, tall: bool) -> Config {
        Config {
            dark_theme: dark,
            intensity: 1.0,
            duration: Duration::from_millis(400),
            ribbon_tall: tall,
            ribbon_flat: false,
            theme_fg: if dark { DEFAULT_FG } else { 0x0016_161C },
            theme_bg: if dark { DEFAULT_BG } else { 0x00FF_FFFF },
            reduced_motion: false,
        }
    }

    const fn flat(dark: bool, tall: bool) -> Config {
        let mut c = cfg(dark, tall);
        c.ribbon_flat = true;
        c
    }

    const fn ctx_in<'a>(
        now: Instant,
        cfg: &'a Config,
        caret: (u16, u16),
        disp: f32,
        g: Geom,
    ) -> Ctx<'a> {
        Ctx {
            now,
            geom: g,
            cfg,
            disp,
            birth_disp: disp,
            phase: 0.0,
            caret,
            caret_t: 0.0,
            caret_walk: None,
            mend: None,
            surge: 0.0,
            flow: crate::rainbow_kitty::spine::Flow::EMPTY,
        }
    }

    const fn ctx<'a>(now: Instant, cfg: &'a Config, caret: (u16, u16), disp: f32) -> Ctx<'a> {
        ctx_in(now, cfg, caret, disp, geom())
    }

    #[derive(Default)]
    struct Sink {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<BeamVertex>,
        cues: Vec<SoundCue>,
    }

    impl Sink {
        fn frame(&mut self) -> Frame<'_> {
            self.under.clear();
            self.out.clear();
            self.halos.clear();
            self.cues.clear();
            Frame {
                under: &mut self.under,
                out: &mut self.out,
                halos: &mut self.halos,
                beams: &mut self.beams,
                cues: &mut self.cues,
                caret: CaretSeam::default(),
                companion: None,
                fp: 0,
            }
        }
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0.checked_add(Duration::from_millis(ms)).expect("clock")
    }

    const fn typed() -> Event {
        Event::Typed {
            cells: 1,
            shifted: false,
            class: TypedClass::Glyph,
        }
    }

    const fn nav(from: (u16, u16), to: (u16, u16)) -> Event {
        Event::Move {
            from,
            to,
            licence: Licence::Nav,
            dir: Dir::Right,
        }
    }

    const fn pty(from: (u16, u16), to: (u16, u16)) -> Event {
        Event::Move {
            from,
            to,
            licence: Licence::Pty,
            dir: Dir::Right,
        }
    }

    #[derive(Clone, Copy)]
    struct Keys {
        g: Geom,
        row: u16,
        col0: u16,
        n: u16,
        period_ms: u64,
        disp: f32,
    }

    fn type_keys(rib: &mut Ribbon, t0: Instant, keys: Keys, c: &Config) {
        for i in 0..keys.n {
            let now = at(t0, u64::from(i) * keys.period_ms);
            let caret = (keys.row, keys.col0 + i + 1);
            let cx = ctx_in(now, c, caret, keys.disp, keys.g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
        }
    }

    fn type_run(rib: &mut Ribbon, t0: Instant, col0: u16, n: u16, c: &Config, disp: f32) {
        let keys = Keys {
            g: geom(),
            row: 2,
            col0,
            n,
            period_ms: 60,
            disp,
        };
        type_keys(rib, t0, keys, c);
    }

    fn erase_at(rib: &mut Ribbon, now: Instant, caret: (u16, u16), c: &Config) {
        let cx = ctx(now, c, caret, 0.7);
        rib.on_event(&Event::Erase, now, &cx);
        rib.plan(&cx);
    }

    fn cell_lit(under: &[GlowQuad], g: Geom, row: u16, col: u16) -> bool {
        let x0 = f32::from(g.origin_x) + f32::from(col) * g.cw as f32;
        let x1 = x0 + g.cw as f32;
        let y0 = f32::from(g.origin_y) + f32::from(row) * g.ch as f32;
        let y1 = y0 + g.ch as f32 * 0.5;
        under.iter().any(|q| {
            let qx0 = f32::from(q.x);
            let qx1 = qx0 + f32::from(q.w);
            let qy = f32::from(q.y);
            qx0 < x1 && qx1 > x0 && (y0..y1).contains(&qy)
        })
    }

    fn column_census(
        quads: &[GlowQuad],
        xs: std::ops::Range<i32>,
        ys: std::ops::Range<i32>,
        floor: u8,
    ) -> Vec<Option<(i32, i32, u8)>> {
        let level = |q: &GlowQuad, x: i32| -> u8 {
            let i = (x - i32::from(q.x)) as u32;
            let w = u32::from(q.w);
            if q.alpha > 0 {
                aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, w) as u8
            } else {
                let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, w);
                ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff) as u8
            }
        };
        xs.map(|x| {
            let mut top = i32::MAX;
            let mut bot = i32::MIN;
            let mut peak = 0u8;
            for q in quads {
                let qx0 = i32::from(q.x);
                if q.w == 0 || x < qx0 || x >= qx0 + i32::from(q.w) {
                    continue;
                }
                let lv = level(q, x);
                if lv < floor {
                    continue;
                }
                let y = i32::from(q.y);
                if !ys.contains(&y) {
                    continue;
                }
                top = top.min(y);
                bot = bot.max(y + i32::from(q.h) - 1);
                peak = peak.max(lv);
            }
            (top != i32::MAX).then_some((top, bot, peak))
        })
        .collect()
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn peak_channel(quads: &[GlowQuad]) -> u32 {
        quads
            .iter()
            .map(|q| {
                ((q.color >> 16) & 0xff)
                    .max((q.color >> 8) & 0xff)
                    .max(q.color & 0xff)
            })
            .max()
            .unwrap_or(0)
    }

    fn plan_peak(rib: &Ribbon) -> u8 {
        rib.plan_segments().iter().map(|s| s.cov).max().unwrap_or(0)
    }

    fn plan_sum(rib: &Ribbon) -> u32 {
        rib.plan_segments().iter().map(|s| u32::from(s.cov)).sum()
    }

    fn band_sum(rib: &Ribbon, col: u16) -> u32 {
        let x = f32::from(col) * geom().cw as f32;
        rib.plan_segments()
            .iter()
            .filter(|s| s.x <= x + 0.5)
            .map(|s| u32::from(s.cov))
            .sum()
    }

    #[test]
    fn letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor() {
        let mut fgs: Vec<u32> = (0x90u32..=0xFF)
            .step_by(3)
            .map(|g| (g << 16) | (g << 8) | g)
            .collect();
        for r in [0xA0u32, 0xD0, 0xFF] {
            for g in [0xA0u32, 0xD0, 0xFF] {
                for b in [0xA0u32, 0xD0, 0xFF] {
                    fgs.push((r << 16) | (g << 8) | b);
                }
            }
        }
        fgs.extend([
            DEFAULT_FG,
            0x00E8_E8F0,
            0x00F8_F8F2,
            0x00AB_B2BF,
            0x00CD_D6F4,
        ]);
        fgs.push(NORD_FG);
        let grounds = [0x0000_0000u32, DEFAULT_BG, 0x0028_2C34, NORD_BG];
        let mut worst = f32::INFINITY;
        let mut at = String::new();
        let mut themes = 0;
        for fg in fgs {
            let budget = bed_luma_budget(fg);
            if budget <= BED_LUMA_MIN {
                continue;
            }
            themes += 1;
            for bg in grounds {
                if relative_luminance(bg) >= budget {
                    continue;
                }
                for i in 0..=128u32 {
                    let ink = bed_ink(spectrum(i as f32 / 128.0), budget);
                    for cov in 1..=(BODY_FRAME_TOP as u32 + 1) {
                        let cov = cov as u8;
                        let lit = over_premul(bg, premul_rgb(ink, cov), cov);
                        let c = contrast(fg, lit);
                        if c < worst {
                            worst = c;
                            at = format!(
                                "fg #{fg:06X} over #{bg:06X}, t {:.3}, cov {cov}, #{lit:06X}",
                                i as f32 / 128.0
                            );
                        }
                    }
                }
            }
        }
        assert!(
            themes >= 60,
            "the sweep must cover the themes it claims ({themes})"
        );
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the bed composited to {worst:.4}:1 at {at}; the bar is {BODY_CONTRAST_BAR}:1 on every dark theme above the floor"
        );
    }

    const PAPERS: [(u32, u32); 6] = [
        (0x001F_2328, 0x00FF_FFFF),
        (0x0020_2733, 0x00F8_FAFD),
        (0x002B_2D33, 0x00F6_F7F9),
        (0x003C_3836, 0x00FB_F1C7),
        (0x004C_4F69, 0x00EF_F1F5),
        (0x0058_6E75, 0x00FD_F6E3),
    ];

    #[test]
    fn the_paper_bed_keeps_every_stop_its_hue_and_every_letter_its_bar() {
        let cap = InkRole::OverText.alpha_cap() as u32;
        for (fg, bg) in PAPERS {
            let bar = (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD)
                .min(PAPER_BAR_THEME_SHARE * contrast(fg, bg))
                - BODY_CONTRAST_GUARD;
            for i in 0..=128u32 {
                let stop = spectrum(i as f32 / 128.0);
                let ink = paper_ink(stop, fg, bg, InkRole::OverText);
                for cov in 1..=cap {
                    let cov = cov as u8;
                    let lit = over_premul(bg, premul_rgb(ink, cov), cov);
                    let c = contrast(fg, lit);
                    assert!(
                        c >= bar,
                        "#{fg:06X} on #{bg:06X}: stop #{stop:06X} at cov {cov} composites #{lit:06X}, {c:.3}:1 under {bar:.3}:1"
                    );
                }
                let lit = over_premul(bg, premul_rgb(ink, cap as u8), cap as u8);
                assert_ne!(lit, bg, "#{bg:06X}: stop #{stop:06X} went out");
                if bg == 0x00FF_FFFF {
                    let (h, ..) = rgb2hsv(lit);
                    let (h0, ..) = rgb2hsv(stop);
                    let off = (h - h0).abs().min(360.0 - (h - h0).abs());
                    assert!(
                        off <= 4.0,
                        "on white stop #{stop:06X} (hue {h0:.1}) composites #{lit:06X} (hue {h:.1})"
                    );
                }
            }
        }
        let yellow = paper_ink(0x00FF_FF00, PAPERS[0].0, PAPERS[0].1, InkRole::OverText);
        let lit = over_premul(0x00FF_FFFF, premul_rgb(yellow, cap as u8), cap as u8);
        assert!(
            lit & 0x00FF_FF00 == 0x00FF_FF00,
            "yellow on white composites #{lit:06X}, not a yellow"
        );
    }

    #[test]
    fn the_paper_caret_is_its_stop_deepened_and_never_olive() {
        for (_, bg) in PAPERS {
            for i in 0..=128u32 {
                let stop = spectrum(i as f32 / 128.0);
                let caret = paper_caret_ink(stop, bg);
                let c = contrast(caret, bg);
                assert!(
                    c >= PAPER_CARET_CONTRAST - 0.01,
                    "on #{bg:06X} stop #{stop:06X} carets #{caret:06X} at {c:.3}:1"
                );
                let (_, s, _) = rgb2hsv(caret);
                assert!(
                    s >= 0.99,
                    "stop #{stop:06X} carets #{caret:06X}, off its hue ray"
                );
            }
        }
        // Blue stands 8.6:1 on white and stays pure; red, at 4.0:1, is
        // deepened along its own ray to the caret law's 4.5:1.
        assert_eq!(paper_caret_ink(0x0000_00FF, 0x00FF_FFFF), 0x0000_00FF);
        let red = paper_caret_ink(0x00FF_0000, 0x00FF_FFFF);
        assert!(
            red & 0x0000_FFFF == 0 && contrast(red, 0x00FF_FFFF) >= 4.5,
            "red carets #{red:06X}"
        );
        // Control: held to a graphic's 3:1, a stop's caret on white falls
        // under the caret law.
        let low = (0..=128u32)
            .map(|i| {
                let stop = spectrum(i as f32 / 128.0);
                contrast(paper_vivid_ink(stop, 0x00FF_FFFF, 3.0), 0x00FF_FFFF)
            })
            .fold(f32::INFINITY, f32::min);
        assert!(low < 4.5, "control: at 3:1 every caret stands {low:.2}:1");
        let (h, ..) = rgb2hsv(paper_caret_ink(0x00FF_FF00, 0x00FF_FFFF));
        assert!(
            (35.0..=50.0).contains(&h),
            "yellow's caret sits at hue {h:.1}, not amber"
        );
    }

    #[test]
    fn the_field_index_answers_what_a_newest_first_scan_would() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let scan_matches = |rib: &Ribbon, ms: u64| {
            for row in 0..6u16 {
                for col in 0..30u16 {
                    let scan = rib
                        .cells()
                        .iter()
                        .rev()
                        .find(|l| l.row == row && l.col == col)
                        .map(|l| l.t);
                    let idx = rib.field_at(row, col);
                    match (scan, idx) {
                        (Some(a), Some(b)) => assert!(
                            (a - b).abs() < 1e-6,
                            "+{ms} ms: index says {b} at ({row},{col}); a newest-first scan says {a}"
                        ),
                        (None, None) => {}
                        (a, b) => {
                            panic!(
                                "+{ms} ms: index and scan disagree at ({row},{col}): {a:?} vs {b:?}"
                            )
                        }
                    }
                }
            }
        };
        let mut script: Vec<(u64, (u16, u16), Event)> = Vec::new();
        for i in 0..8u16 {
            script.push((u64::from(i) * 60, (2, 11 + i), typed()));
        }
        script.push((500, (2, 40), nav((2, 18), (2, 40))));
        script.push((500, (2, 14), nav((2, 40), (2, 14))));
        script.push((500, (2, 14), typed()));
        script.push((560, (2, 15), typed()));
        script.push((900, (3, 10), nav((2, 15), (3, 10))));
        for i in 0..6u16 {
            script.push((1000 + u64::from(i) * 60, (3, 11 + i), typed()));
        }
        script.push((1600, (2, 21), nav((3, 16), (2, 21))));
        for i in 0..3u16 {
            script.push((1600 + u64::from(i) * 60, (2, 21 + i), typed()));
        }
        let mut caret = (2u16, 10u16);
        let (mut saw_shadow, mut saw_dark_row_2, mut saw_relit_row_2) = (false, false, false);
        for ms in (0..=2400u64).step_by(20) {
            let now = at(t0, ms);
            for (_, c_at, ev) in script.iter().filter(|(t, _, _)| *t == ms) {
                caret = *c_at;
                let cx = ctx(now, &c, caret, 0.6);
                rib.on_event(ev, now, &cx);
            }
            let cx = ctx(now, &c, caret, 0.6);
            rib.plan(&cx);
            scan_matches(&rib, ms);
            let cells = rib.cells();
            saw_shadow |= cells.iter().filter(|l| (l.row, l.col) == (2, 13)).count() >= 2;
            let row_2_dark = !cells.is_empty() && cells.iter().all(|l| l.row != 2);
            saw_dark_row_2 |= row_2_dark;
            saw_relit_row_2 |= saw_dark_row_2 && cells.iter().any(|l| l.row == 2);
        }
        assert!(
            saw_shadow,
            "the script must put a genuinely shadowed cell in the pool"
        );
        assert!(saw_dark_row_2, "the script must take row 2 dark");
        assert!(saw_relit_row_2, "…and relight it");
    }

    fn lab_de(a: u32, b: u32) -> f32 {
        let lab = |c: u32| {
            let ch = |sh: u32| ((c >> sh) & 0xff) as f32 / 255.0;
            let lin = |v: f32| {
                if v <= 0.040_45 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            let (r, g, bl) = (lin(ch(16)), lin(ch(8)), lin(ch(0)));
            let (x, y, z) = (
                0.4124 * r + 0.3576 * g + 0.1805 * bl,
                0.2126 * r + 0.7152 * g + 0.0722 * bl,
                0.0193 * r + 0.1192 * g + 0.9505 * bl,
            );
            let f = |t: f32| {
                if t > 0.008_856 {
                    t.cbrt()
                } else {
                    7.787 * t + 16.0 / 116.0
                }
            };
            let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
            (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
        };
        let (l1, a1, b1) = lab(a);
        let (l2, a2, b2) = lab(b);
        ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
    }

    fn arc_step(a: f32, b: f32, budget: f32) -> (f32, f32) {
        let bed = |t: f32| bed_ink(spectrum(t), budget);
        (
            lab_de(bed(a), bed(b)),
            lab_de(rail_ink(spectrum(a)), rail_ink(spectrum(b))),
        )
    }

    #[test]
    fn the_walk_s_pace_is_the_arc_s_own_perceptual_step() {
        let budget = bed_luma_budget(NORD_FG);
        const N: usize = 4096;
        let (w0, w1) = (CROSS_ARC0, CROSS_ARC1);
        let mut cum = [0.0f32; N + 1];
        for i in 1..=N {
            let a = w0 + (w1 - w0) * (i - 1) as f32 / N as f32;
            let b = w0 + (w1 - w0) * i as f32 / N as f32;
            let (db, dr) = arc_step(a, b, budget);
            cum[i] = cum[i - 1] + (db + dr).max(0.005);
        }
        let total = cum[N];
        let mut j = 0usize;
        let mut worst = 0.0f32;
        for (k, &baked) in CROSS_PACE.iter().enumerate() {
            let want = k as f32 / (CROSS_PACE_KNOTS - 1) as f32 * total;
            while j < N && cum[j + 1] < want {
                j += 1;
            }
            let span = (cum[j + 1] - cum[j]).max(1e-9);
            let f = ((want - cum[j]) / span).clamp(0.0, 1.0);
            let a = w0 + (w1 - w0) * (j as f32 + f) / N as f32;
            let got = f32::from(baked) / 65535.0;
            worst = worst.max((a - got).abs());
        }
        assert!(
            worst <= 2.0 * (CROSS_ARC1 - CROSS_ARC0) / (CROSS_PACE_KNOTS - 1) as f32,
            "the baked pace is {worst:.5} of the arc off the ink's own step — re-generate it"
        );
    }

    #[test]
    fn the_walk_is_a_function_of_position_so_a_retyped_cell_takes_back_its_stop() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.7);
        let mut ms = 40 * 60;
        for caret in [41u16, 40, 39] {
            erase_at(&mut rib, at(t0, ms), (2, caret), &c);
            ms += 60;
        }
        for cell in rib.cells() {
            assert_eq!(
                cell.retract_at.is_some(),
                cell.col >= 39,
                "col {} is on the wrong side of the backspace",
                cell.col
            );
        }
        assert!(
            (2..39u16).all(|col| rib.field_at(2, col).is_some()),
            "the lit run must stay contiguous from the caret backward"
        );
        for caret in [40u16, 41, 42, 21] {
            let now = at(t0, ms);
            let cx = ctx(now, &c, (2, caret), 0.7);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            ms += 60;
        }
        for cell in rib.cells().iter().filter(|l| l.retract_at.is_none()) {
            let want = walk_t(f32::from(cell.col) - 2.0);
            assert!(
                (cell.t - want).abs() < 1e-5,
                "col {} carries t = {} but the walk at its column is {want}",
                cell.col,
                cell.t
            );
        }
        for col in [20u16, 39, 40, 41] {
            let owners: Vec<&Cell> = rib.cells().iter().filter(|l| l.col == col).collect();
            assert_eq!(
                owners.len(),
                1,
                "col {col} must have exactly one owner after a retype"
            );
            assert!(owners[0].retract_at.is_none(), "…and it is live again");
        }
    }

    #[test]
    fn the_reach_stops_at_the_hand_s_own_first_column() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 28,
            n: 2,
            period_ms: 60,
            disp: 0.8,
        };
        type_keys(&mut rib, t0, keys, &c);
        let now = at(t0, 60 + (PHRASE_REST_MIN_S * 1000.0) as u64 + 100);
        let cx = ctx(now, &c, (2, 30), 0.0);
        rib.plan(&cx);
        assert!(
            rib.field_at(2, 28).is_some(),
            "the hand's own first cell must still be lit, or this pins nothing"
        );
        for col in 0..28u16 {
            assert!(
                rib.field_at(2, col).is_none(),
                "the reach lit column {col}, left of the first column the hand \
                 ever held on the row — at a shell prompt that is the prompt"
            );
        }
    }

    #[test]
    fn a_jump_s_landing_column_is_stamped_on_its_own_row_alone() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 28,
                n: 32,
                period_ms: 12,
                disp: 0.8,
            },
            &c,
        );
        type_keys(
            &mut rib,
            at(t0, 400),
            Keys {
                g: geom(),
                row: 3,
                col0: 0,
                n: 16,
                period_ms: 12,
                disp: 0.8,
            },
            &c,
        );
        assert!(
            rib.cohorts().iter().any(|coh| coh.row == 2),
            "row 2 must still carry a cohort, or this pins nothing"
        );
        let now = at(t0, 700);
        let cx = ctx(now, &c, (3, 40), 0.0);
        rib.on_event(
            &Event::Move {
                from: (3, 16),
                to: (3, 40),
                licence: Licence::Nav,
                dir: Dir::Right,
            },
            now,
            &cx,
        );
        for coh in rib.cohorts().iter().filter(|coh| coh.row != 3) {
            assert!(
                coh.retract_col != Some(40),
                "a cohort on row {} was handed column 40, which is a place on \
                 row 3 — it must reach the per-row law instead",
                coh.row
            );
        }
    }

    #[test]
    fn the_wave_breathes_a_tenth_of_a_cell_at_full_momentum_inside_the_body_s_own_rows() {
        for (c, dn_ch) in [
            (cfg(true, true), COMET_DN_HAND_CH),
            (flat(true, true), DN_TOP_CH),
        ] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 24, &c, 1.0);
            let g = geom();
            let ch = g.ch as f32;
            let rest = f32::from(g.origin_y) + 3.0 * ch;
            let mut deepest = 0.0f32;
            for k in 0..16u32 {
                let mut cx = ctx(at(t0, 23 * 60 + 8), &c, (2, 34), 1.0);
                cx.phase = k as f32 / 16.0;
                rib.plan(&cx);
                for col in 10..34u16 {
                    let band = rib.band(2, col).expect("a laid cell has a band");
                    deepest = deepest.max((band.spine - rest).abs());
                    assert!(
                        band.top >= rest - TALL_UP_CH * ch - 0.5,
                        "the wave lifted the body's top past its own reach at col {col}, phase {k}/16 ({})",
                        band.top
                    );
                    assert!(
                        band.bottom <= rest + dn_ch * ch + 0.5,
                        "the wave dropped the body's bottom past its own reach at col {col}, phase {k}/16 ({}; flat {})",
                        band.bottom,
                        c.ribbon_flat
                    );
                }
            }
            assert!(
                deepest >= 0.10 * ch,
                "the wave's deepest swing at full momentum is {deepest:.2} px ({:.3} ch); the owner asked for a ribbon that visibly breathes with speed — `WAVE_AMP_CELLS` 0.110",
                deepest / ch
            );
        }
    }

    #[test]
    fn the_hot_edge_lives_only_at_the_head_on_a_dark_ground() {
        let dark = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &dark, 0.9);
        let g = geom();
        let caret_top = f32::from(g.origin_y) + 2.0 * g.ch as f32;
        let caret_bot = caret_top + g.ch as f32;
        let reach_y = HOT_EDGE_CARET_REACH_CH * g.ch as f32;
        let check = |out: &[GlowQuad], head_col: u16, label: &str| {
            assert!(!out.is_empty(), "{label}: a hot hand must leave a hot edge");
            let head_x = f32::from(g.origin_x) + f32::from(head_col) * g.cw as f32;
            for q in out {
                assert!(
                    u32::from(q.alpha) == 0,
                    "{label}: the hot edge is ADDITIVE light; it may not carry a source-over alpha"
                );
                let x = f32::from(q.x);
                assert!(
                    x <= head_x + 1.0,
                    "{label}: the hot edge ran PAST the head cell (x = {x}, head = {head_x})"
                );
                assert!(
                    head_x - x <= HOT_EDGE_CELLS_MAX * g.cw as f32 + f32::from(q.w) + 1.0,
                    "{label}: the hot edge reached further than {HOT_EDGE_CELLS_MAX} cells behind the head"
                );
                let y = f32::from(q.y);
                assert!(
                    (caret_top - reach_y..=caret_bot + reach_y).contains(&y),
                    "{label}: the hot edge left the caret's own row band (y = {y})"
                );
            }
            assert!(
                peak_channel(out) as f32 <= HOT_EDGE_COV_MAX + 1.0,
                "{label}: hot-edge coverage is over the {HOT_EDGE_COV_MAX} request ceiling"
            );
        };
        let mut sink = Sink::default();
        let cx = ctx(at(t0, 8 * 60), &dark, (2, 18), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        check(&sink.out, 18, "typing");
        let mut ms = 8 * 60;
        for caret in [17u16, 16, 15] {
            erase_at(&mut rib, at(t0, ms), (2, caret), &dark);
            ms += 60;
        }
        let cx = ctx(at(t0, ms), &dark, (2, 15), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            rib.cells().iter().any(|l| l.col >= 15),
            "the erased cells must still be draining for this frame to test anything"
        );
        check(&sink.out, 15, "backspace");
        for caret in [14u16, 13, 12, 11, 10] {
            erase_at(&mut rib, at(t0, ms), (2, caret), &dark);
            ms += 60;
        }
        let cx = ctx(at(t0, ms), &dark, (2, 10), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !rib.cells().is_empty() && !sink.under.is_empty(),
            "the draining cells must still be on glass for this frame to test anything"
        );
        assert!(
            sink.out.is_empty(),
            "a word that is entirely draining has no wet head; the hot edge sat on erased cells"
        );
    }

    #[test]
    fn the_body_never_brightens_after_the_last_keystroke() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 10, &c, 0.9);
        let last = at(t0, 9 * 60);
        let mut sink = Sink::default();
        let mut peak_prev = 255u8;
        let mut edge_first: Option<u32> = None;
        for ms in (20..1400).step_by(8) {
            let now = at(last, ms);
            let disp = (0.4 + 0.6 * (ms as f32 / 700.0)).min(1.0);
            let cx = ctx(now, &c, (2, 20), disp);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let peak = plan_peak(&rib);
            assert!(
                peak <= peak_prev,
                "the body brightened at +{ms} ms with no keystroke behind it ({peak_prev} → {peak})"
            );
            peak_prev = peak;
            let lit = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
            assert!(
                f32::from(lit) <= BODY_FRAME_TOP,
                "the composited body passed the ledger's frame top at +{ms} ms ({lit})"
            );
            let edge = peak_channel(&sink.out);
            let first = *edge_first.get_or_insert(edge);
            assert!(
                edge <= first + 4,
                "the hot edge brightened at +{ms} ms with no keystroke behind it ({first} → {edge})"
            );
        }
        assert!(
            edge_first.is_some_and(|e| e > 0),
            "the hot edge must have been lit for its law to have been tested"
        );
    }

    #[test]
    fn the_exit_swoosh_retracts_toward_the_caret_and_reaches_exactly_zero() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let last = at(t0, 7 * 60);
        let caret = (2u16, 18u16);
        let mut sink = Sink::default();
        let mut left_prev = f32::NEG_INFINITY;
        let mut edge_prev = u32::MAX;
        let mut edge_first: Option<u32> = None;
        let block_right = 19.0 * geom().cw as f32;
        let mut reach_prev = f32::INFINITY;
        let retract_start = RETRACT_START_S * 1000.0;
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        for ms in ((retract_start as u64)..total + 20).step_by(10) {
            let now = at(last, ms);
            let cx = ctx(now, &c, caret, 0.0);
            rib.plan(&cx);
            let reach = rib
                .plan_segments()
                .iter()
                .map(|s| s.x)
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                reach <= block_right + 0.5 && reach <= reach_prev + 0.5,
                "+{ms} ms: a slab at x {reach} (was {reach_prev}) is past the caret block's right \
                 edge {block_right} or grew"
            );
            reach_prev = reach;
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let edge = peak_channel(&sink.out);
            edge_first.get_or_insert(edge);
            assert!(
                edge <= edge_prev.saturating_add(2),
                "the hot edge brightened inside the swoosh at +{ms} ms ({edge_prev} → {edge})"
            );
            edge_prev = edge;
            if ms == total - 90 {
                let first = edge_first.unwrap_or(0);
                assert!(
                    !sink.under.is_empty() && edge * 4 < first,
                    "mid-fade the hot edge must be well under way with its body ({first} → {edge})"
                );
            }
            if ms == total - 10 {
                assert!(
                    !rib.cells().is_empty(),
                    "the cells must still be in the pool for the pin to mean anything"
                );
                assert_eq!(
                    edge, 0,
                    "10 ms before the pool clears the hot edge must already be at exactly zero"
                );
            }
            let Some(left) = sink.under.iter().map(|q| f32::from(q.x)).reduce(f32::min) else {
                continue;
            };
            assert!(
                left + 1.0 >= left_prev,
                "the retract moved AWAY from the caret at +{ms} ms ({left_prev} → {left})"
            );
            left_prev = left;
        }
        assert!(
            edge_first.is_some_and(|e| e > 0),
            "the hot edge must have been lit at the retract's start for its law to have been tested"
        );
        let now = at(last, (SWOOSH_TOTAL_S * 1000.0) as u64 + 40);
        let cx = ctx(now, &c, caret, 0.0);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        assert!(
            sink.under.is_empty() && sink.out.is_empty(),
            "the swoosh must reach EXACTLY zero, not a residue"
        );
        assert!(rib.at_rest(), "and the pools must be empty with it");
    }

    #[test]
    fn a_cohort_still_being_typed_keeps_its_boundaries_while_its_row_mate_retracts() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let now = at(t0, 1000);
        let cx = ctx(now, &c, (2, 40), 0.8);
        rib.on_event(&pty((2, 18), (2, 40)), now, &cx);
        rib.plan(&cx);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 40,
            n: 6,
            period_ms: 60,
            disp: 0.8,
        };
        type_keys(&mut rib, at(t0, 1000), keys, &c);
        let now = at(t0, 1320);
        let cx = ctx(now, &c, (2, 46), 0.8);
        rib.plan(&cx);
        let cw = geom().cw as f32;
        let (a, b): (Vec<&Segment>, Vec<&Segment>) =
            rib.plan_segments().iter().partition(|s| s.x < 30.0 * cw);
        assert!(
            !a.is_empty() && !b.is_empty(),
            "both cohorts must be planned"
        );
        let a_left = a.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
        assert!(
            a_left > 10.0 * cw + 0.5,
            "the abandoned cohort must be moving toward the caret (left edge {a_left})"
        );
        let b_left = b.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
        let b_right = b.iter().map(|s| s.x).fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (b_left - 40.0 * cw).abs() < 0.5 && (b_right - 47.0 * cw).abs() < 0.5,
            "the cohort under the hand was squashed with its row-mate: [{b_left}, {b_right}]"
        );
    }

    #[test]
    fn a_rewrite_retracts_the_suffix_from_the_given_column_not_the_mirror() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 20, &c, 0.8);
        let now = at(t0, 1300);
        let cx = ctx(now, &c, (2, 30), 0.8);
        rib.on_event(
            &Event::Rewrite {
                row: 2,
                col: 18,
                cells: 12,
            },
            now,
            &cx,
        );
        let retract_at = |col: u16| {
            rib.cells()
                .iter()
                .find(|cell| cell.row == 2 && cell.col == col)
                .unwrap_or_else(|| panic!("no cell at (2, {col})"))
                .retract_at
        };
        assert!(
            (18..30u16).all(|col| retract_at(col).is_some()),
            "every cell at or right of the rewrite's caret retracts"
        );
        assert!(
            (10..18u16).all(|col| retract_at(col).is_none()),
            "the cells left of it are untouched"
        );
        let span = kill_span_s(12);
        let far = retract_at(29).unwrap();
        let near = retract_at(18).unwrap();
        assert!(far < near, "farthest from the caret goes first");
        let near_s = near.saturating_duration_since(now).as_secs_f32();
        assert!(
            (near_s - span * 11.0 / 12.0).abs() < 1e-3,
            "the near cell goes last, at the kill's 12·n + 240 stagger: {near_s}"
        );
        assert_eq!(
            rib.caret(),
            Some((2, 18)),
            "the ribbon's caret moves to the rewrite"
        );
    }

    #[test]
    fn under_reduced_motion_the_exit_swoosh_moves_nothing_and_only_fades() {
        let mut c = cfg(true, true);
        c.reduced_motion = true;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let last = at(t0, 7 * 60);
        let cw = geom().cw as f32;
        let mut sink = Sink::default();
        let mut frames = 0u32;
        let mut peak_prev = u8::MAX;
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        for ms in ((PHRASE_REST_MIN_S * 1000.0) as u64..total + 20).step_by(10) {
            let now = at(last, ms);
            let cx = ctx(now, &c, (2, 18), 0.0);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            if sink.under.is_empty() {
                continue;
            }
            frames += 1;
            let left = sink
                .under
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min);
            let right = sink
                .under
                .iter()
                .map(|q| f32::from(q.x) + f32::from(q.w))
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                (left - 10.0 * cw).abs() < 0.5 && (right - 19.0 * cw).abs() < 0.5,
                "a static mark moved at +{ms} ms: [{left}, {right}]"
            );
            let peak = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
            assert!(
                peak <= peak_prev,
                "the one linear fade may only fall (+{ms} ms: {peak_prev} → {peak})"
            );
            peak_prev = peak;
        }
        assert!(
            frames > 40,
            "the mark must be visible through most of the swoosh window ({frames})"
        );
        assert!(rib.at_rest(), "…and be gone at its end");
    }

    #[test]
    fn a_jump_takes_the_band_out_from_the_light_it_has_never_by_a_step() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let last = at(t0, 39 * 60);
        let cx = ctx(at(last, 8), &c, (2, 42), 0.9);
        rib.plan(&cx);
        let before = band_sum(&rib, 42);
        let segs = rib.plan_segments().len() as u32;
        assert!(
            plan_peak(&rib) > 200,
            "the mark must be hot for a step to be measurable"
        );
        let now = at(last, 16);
        let cx = ctx(now, &c, (2, 80), 0.9);
        rib.on_event(&pty((2, 42), (2, 80)), now, &cx);
        rib.plan(&cx);
        let after = band_sum(&rib, 42);
        assert!(
            after + 2 * segs >= before,
            "the jump stepped the band down ({before} → {after} over {segs} boundaries) instead of retracting it from where it was"
        );
        let cx = ctx(at(last, 16 + 340), &c, (2, 80), 0.9);
        rib.plan(&cx);
        let mid = plan_sum(&rib);
        assert!(
            mid * 10 < before * 8,
            "the abandoned band must be well into its drain by mid-retract ({before} → {mid})"
        );
        let cx = ctx(at(last, 16 + 660), &c, (2, 80), 0.9);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the abandoned band must be out {RETRACT_DUR_S} + {RETRACT_FADE_S} s after the jump"
        );
    }

    #[test]
    fn a_run_the_hand_leaves_is_feathered_at_its_head_end() {
        let level = |q: &GlowQuad| {
            if q.alpha > 0 {
                f32::from(q.alpha)
            } else {
                [16u32, 8, 0]
                    .iter()
                    .map(|&sh| ((q.color >> sh) & 0xff) as f32)
                    .fold(0.0, f32::max)
            }
        };
        let peak = |qs: &[GlowQuad], x0: f32, x1: f32| {
            qs.iter()
                .filter(|q| f32::from(q.x) < x1 && f32::from(q.x + q.w) > x0)
                .map(level)
                .fold(0.0f32, f32::max)
        };
        let cw = geom().cw as f32;
        let head_x = 22.0 * cw;
        for (name, c) in [
            ("tall dark", cfg(true, true)),
            ("underline dark", cfg(true, false)),
            ("flat dark", flat(true, true)),
            ("tall light", cfg(false, true)),
        ] {
            let jump = |square: bool| {
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                rib.head_square = square;
                type_run(&mut rib, t0, 2, 20, &c, 0.9);
                let now = at(t0, 19 * 60 + 16);
                let cx = ctx(now, &c, (10, 2), 0.9);
                rib.on_event(&pty((2, 22), (10, 2)), now, &cx);
                rib.plan(&cx);
                let mut sink = Sink::default();
                {
                    let mut f = sink.frame();
                    rib.emit(&cx, &mut f);
                }
                let end = (head_x - 0.25 * cw, head_x);
                (
                    peak(&sink.under, end.0, end.1),
                    peak(&sink.under, head_x - 4.0 * cw, head_x - 3.0 * cw),
                    peak(&sink.out, end.0, end.1),
                )
            };
            let (end, body, edge) = jump(false);
            let (square_end, square_body, square_edge) = jump(true);
            assert!(
                body > 0.0 && square_body > 0.0,
                "{name}: fixture: the band is on glass on the jump frame"
            );
            assert!(
                end <= 0.4 * body,
                "{name}: the end the caret left stands at {end} against the body's {body}"
            );
            assert!(
                square_end >= 0.75 * square_body,
                "{name}: control: square-cut, the end stands at {square_end} against {square_body}"
            );
            if square_edge > 0.0 {
                assert!(
                    edge <= 0.4 * square_edge,
                    "{name}: the hot edge stands at {edge} at the end the caret left \
                     (square-cut {square_edge})"
                );
            }
        }
    }

    /// The trail gallery's glass: 72x14 cells of 14x26 px from (12, 2).
    const fn gallery_geom() -> Geom {
        Geom {
            cw: 14,
            ch: 26,
            rows: 14,
            cols: 72,
            origin_x: 12,
            origin_y: 2,
            win_w: 1032,
            win_h: 378,
            head: 0,
        }
    }

    /// Under a filled cell on the row above the caret's (the gallery's yellow
    /// `main +2` powerline segment), the hot edge held half its thickness
    /// inside the row lights nothing above the row's top; centred on the top
    /// (the negative control) it does.
    #[test]
    fn the_hot_edge_keeps_off_a_filled_cell_above() {
        let g = gallery_geom();
        let c = cfg(true, true);
        let (ox, oy, cw, ch) = (
            i32::from(g.origin_x),
            i32::from(g.origin_y),
            g.cw as i32,
            g.ch as i32,
        );
        let row_top = oy + 4 * ch;
        let bar = (ox + 13 * cw, ox + 22 * cw, row_top - ch, row_top);
        let above = |on_bar: bool| -> (usize, u32) {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.hot_edge_on_bar = on_bar;
            rib.keep_out.set([bar]);
            let keys = Keys {
                g,
                row: 4,
                col0: 2,
                n: 19,
                period_ms: 85,
                disp: 0.9,
            };
            type_keys(&mut rib, t0, keys, &c);
            let cx = ctx_in(at(t0, 18 * 85 + 20), &c, (4, 21), 0.9, g);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let lit = |q: &&GlowQuad| q.color & 0x00FF_FFFF != 0 || q.color2 & 0x00FF_FFFF != 0;
            let n = sink.out.iter().filter(lit).count();
            let over = sink
                .out
                .iter()
                .filter(lit)
                .filter(|q| {
                    let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
                    i32::from(q.y) < row_top && x0 < bar.1 && bar.0 < x1
                })
                .map(|q| peak_channel(std::slice::from_ref(q)))
                .max()
                .unwrap_or(0);
            (n, over)
        };
        let (n, over) = above(false);
        assert!(n > 0, "fixture: the hand leaves a hot edge");
        assert_eq!(
            over, 0,
            "the hot edge lights the segment's foot above the row by {over}"
        );
        let (n, over) = above(true);
        assert!(n > 0, "control fixture: the hand leaves a hot edge");
        assert!(
            over >= 40,
            "control: centred on the row's top the edge lights the segment's foot by only {over}"
        );
    }

    /// On the gallery's 14 px cells a run's tail light steps by no more than
    /// an eighth of the body's from one lit column to the next, on dark and
    /// on paper; laid at the stride (the negative control) it steps by a fifth
    /// or more.
    #[test]
    fn the_run_s_tail_eases_in_without_a_stair() {
        let g = gallery_geom();
        let col0 = 3u16;
        let profile = |dark: bool, stair: bool| -> (f32, f32) {
            let c = cfg(dark, true);
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.tail_stair = stair;
            let keys = Keys {
                g,
                row: 4,
                col0,
                n: 16,
                period_ms: 60,
                disp: 0.6,
            };
            type_keys(&mut rib, t0, keys, &c);
            let now = at(t0, 15 * 60 + 16);
            let cx = ctx_in(now, &c, (4, col0 + 16), 0.6, g);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let mut col = vec![0.0f32; usize::from(g.win_w)];
            for q in sink.under.iter().filter(|q| q.alpha > 0 || q.alpha2 > 0) {
                let w = f32::from(q.w.max(1));
                for i in 0..q.w {
                    let t = if q.w > 1 {
                        f32::from(i) / (w - 1.0)
                    } else {
                        0.0
                    };
                    let a = f32::from(q.alpha) + (f32::from(q.alpha2) - f32::from(q.alpha)) * t;
                    let x = usize::from(q.x + i);
                    col[x] = col[x].max(a);
                }
            }
            let ox = usize::from(g.origin_x);
            let tail = ox + usize::from(col0) * g.cw;
            let body = col[tail + 4 * g.cw];
            // From the run's first lit column (its outer boundary keeps a
            // tenth of the light, a soft start) across the tail cell.
            let first = (tail.saturating_sub(2)..tail + g.cw)
                .find(|&x| col[x] > 0.0)
                .unwrap_or(tail);
            let step = col[first..first + g.cw + 2]
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f32::max);
            (step, body)
        };
        for dark in [true, false] {
            let (step, body) = profile(dark, false);
            assert!(
                body > 40.0,
                "fixture (dark {dark}): the body is lit ({body})"
            );
            assert!(
                step <= body / 8.0,
                "dark {dark}: the tail steps by {step} in a column against the body's {body}"
            );
            let (stair, body) = profile(dark, true);
            assert!(
                stair >= body / 5.0,
                "control (dark {dark}): at the stride the tail steps only {stair} against {body}"
            );
        }
    }

    #[test]
    fn the_left_run_s_feather_is_smooth_and_sheds_the_hot_edge() {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let head_x = 22.0 * cw;
        let jump = |coarse: bool, left_lit: bool| -> (f32, f32, usize) {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.feather_coarse = coarse;
            rib.hot_edge_left_lit = left_lit;
            type_run(&mut rib, t0, 2, 20, &c, 0.9);
            let now = at(t0, 19 * 60 + 16);
            let cx = ctx(now, &c, (10, 2), 0.9);
            rib.on_event(&pty((2, 22), (10, 2)), now, &cx);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let mut col = vec![0.0f32; usize::from(g.win_w)];
            for q in sink.under.iter().filter(|q| q.row == 2 && q.alpha > 0) {
                for x in q.x..q.x + q.w {
                    let a = f32::from(q.alpha.max(q.alpha2));
                    col[usize::from(x)] = col[usize::from(x)].max(a);
                }
            }
            let body = col[(head_x - 4.0 * cw) as usize];
            let lo = (head_x - 1.5 * cw) as usize;
            let hi = (head_x + 0.5 * cw) as usize;
            let step = col[lo..hi]
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f32::max);
            let edge = sink.out.iter().filter(|q| q.row <= 2).count();
            (step, body, edge)
        };
        let (step, body, edge) = jump(false, false);
        assert!(
            body > 0.0,
            "fixture: the band is on glass on the jump frame"
        );
        assert!(
            step <= 0.2 * body,
            "the feather steps by {step} in a column against the body's {body}"
        );
        assert_eq!(
            edge, 0,
            "{edge} hot-edge quads stand over the run the hand left"
        );
        let (coarse, coarse_body, _) = jump(true, false);
        assert!(
            coarse > 0.2 * coarse_body,
            "control: laid at the stride the feather steps by only {coarse} against {coarse_body}"
        );
        let (_, _, kept) = jump(false, true);
        assert!(
            kept > 0,
            "control: kept on the left run, the hairline lays nothing"
        );
    }

    fn row_cells(rib: &Ribbon, row: u16) -> Vec<(u16, bool, f32, Instant)> {
        let mut v: Vec<_> = rib
            .cells()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| (c.col, c.typing, c.life_s, c.born))
            .collect();
        v.sort_by_key(|c| c.0);
        v
    }

    #[test]
    fn a_jump_lays_its_wake_over_the_live_band_and_leaves_the_band_s_cells_untouched() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 12,
                n: 40,
                period_ms: 60,
                disp: 0.9,
            },
            &c,
        );
        let last = at(t0, 39 * 60);
        let cx = ctx(at(last, 8), &c, (2, 52), 0.9);
        rib.plan(&cx);
        let band_light = |rib: &Ribbon| band_sum(rib, 51);
        let before = band_light(&rib);
        let before_cells: Vec<Cell> = rib.cells().to_vec();
        let segs = rib.plan_segments().len() as u32;
        let stops: Vec<Option<f32>> = (12..52u16).map(|col| rib.field_at(2, col)).collect();
        assert!(
            stops.iter().all(Option::is_some),
            "the band is laid under cols 12..52"
        );
        let now = at(last, 16);
        let cx = ctx(now, &c, (2, 2), 0.9);
        let envs_before: Vec<(u16, f32)> = before_cells
            .iter()
            .map(|l| (l.col, rib.env_of(&cx, l)))
            .collect();
        rib.on_event(&nav((2, 52), (2, 2)), now, &cx);
        rib.plan(&cx);
        for (col, want) in (12..52u16).zip(&stops) {
            let got = rib.field_at(2, col);
            assert!(
                matches!((got, want), (Some(g), Some(w)) if (g - w).abs() < 1e-5),
                "col {col}: the wake repainted a stop the eye had read ({want:?} → {got:?})"
            );
        }
        let wake = rib
            .cohorts()
            .iter()
            .map(|c| c.id)
            .max()
            .expect("the wake cohort");
        let band = rib.cohorts()[0];
        assert_ne!(band.id, wake);
        assert!(band.abandoned, "the band is abandoned beneath the corridor");
        assert_eq!(
            (band.retract_col, band.drain_right_first),
            (Some(2), true),
            "…toward the landing, draining from the origin end first (the drain follows the meteor)"
        );
        let flight = flight_ms(50.0) / 1000.0;
        for col in 2..12u16 {
            assert!(
                !rib.cells()
                    .iter()
                    .any(|c| c.row == 2 && c.col == col && c.layer == Layer::Base),
                "col {col}: dark ground carries no band cell"
            );
            let over: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Over)
                .collect();
            assert_eq!(over.len(), 1, "col {col}: the corridor lit the dark ground");
            assert!(over[0].rearm.is_none(), "col {col}: nothing to re-arm from");
        }
        for col in 12..52u16 {
            let base: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Base)
                .collect();
            let over: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Over)
                .collect();
            assert_eq!(
                base.len(),
                1,
                "col {col}: the band's own cell is still there"
            );
            assert_eq!(over.len(), 1, "col {col}: one wake cell over it");
            let (b, o) = (base[0], over[0]);
            let was = before_cells
                .iter()
                .find(|c| c.col == col)
                .expect("was laid");
            assert!(
                b.cohort == was.cohort
                    && b.born == was.born
                    && b.cov0 == was.cov0
                    && b.life_s == was.life_s,
                "col {col}: the wake touched the band's cell (K1)"
            );
            assert_eq!(o.cohort, wake, "col {col}: …in the wake cohort");
            assert!(!o.typing, "col {col}: a wake cell is never typing");
            assert!(
                o.cov0 >= b.cov0 - 1e-6,
                "col {col}: the corridor is never dimmer than the band it covers"
            );
            let (re_at, level) = o.rearm.expect("born at the band's level");
            assert_eq!(re_at, o.born);
            let env_before = envs_before
                .iter()
                .find(|&&(c, _)| c == col)
                .map_or(0.0, |&(_, e)| e);
            assert!(
                env_before > 0.9 && (level * o.cov0 - b.cov0 * env_before).abs() < 1e-5,
                "col {col}: the level is the band's own coverage share (level {level}, env {env_before})"
            );
            let j = f32::from(52 - col);
            let want = now + Duration::from_secs_f32(flight * j / 50.0);
            assert!(
                o.born.saturating_duration_since(want) <= Duration::from_millis(1)
                    && want.saturating_duration_since(o.born) <= Duration::from_millis(1),
                "col {col}: born on the flight clock"
            );
        }
        assert!(rib.field_at(2, 52).is_none());
        assert_eq!(
            rib.cells().len(),
            90,
            "the band's 40 cells and the corridor's 50 over them"
        );
        let after = band_light(&rib);
        assert!(
            after + 2 * segs >= before,
            "the jump frame stepped the band down ({before} → {after} over {segs} boundaries)"
        );
        let cx = ctx(at(now, (flight * 1000.0) as u64 + 20), &c, (2, 2), 0.9);
        rib.plan(&cx);
        let landed = band_light(&rib);
        assert!(
            landed + 2 * segs >= before,
            "the landed corridor is dimmer than the band it covers ({before} → {landed})"
        );
        let cx = ctx(at(last, 16 + 660), &c, (2, 80), 0.9);
        rib.plan(&cx);
        let held = band_light(&rib);
        assert!(
            held * 2 > before,
            "0.66 s after the jump the wake must still hold the corridor ({before} → {held})"
        );
        let cx = ctx(at(last, 16 + 1250), &c, (2, 80), 0.9);
        rib.plan(&cx);
        assert!(rib.at_rest(), "the wake is out by +1.25 s");
    }

    #[test]
    fn a_ping_pong_at_speed_does_not_accumulate_and_empties_one_wake_life_after_its_last_key() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let mut caret = (2u16, 2u16);
        let mut peak_by_cycle = Vec::new();
        let mut peak = 0usize;
        let landed = 200 + flight_ms(58.0) as u64 + (EDGE_IN_S * 1000.0) as u64 + 8;
        let mut interval_frames = 0u32;
        for ms in (0..=3000u64).step_by(8) {
            let now = at(t0, ms);
            if ms % 200 == 0 {
                let to = if caret.1 == 2 { (2, 60) } else { (2, 2) };
                let cx = ctx(now, &c, to, 0.0);
                rib.on_event(&nav(caret, to), now, &cx);
                caret = to;
                if ms > 0 {
                    peak_by_cycle.push(peak);
                    peak = 0;
                }
            }
            let cx = ctx(now, &c, caret, 0.0);
            rib.plan(&cx);
            peak = peak.max(rib.cells().len());
            if ms % 200 == 0 {
                let wakes = rib
                    .cohorts()
                    .iter()
                    .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
                    .count();
                assert_eq!(wakes, 1, "+{ms} ms: one live wake cohort on the row (K4)");
            }
            if ms >= landed {
                let lit: Vec<u16> = (2..=60u16)
                    .filter(|&col| {
                        rib.cells().iter().any(|l| {
                            l.row == 2
                                && l.col == col
                                && UNDER_COV_CAP * l.cov0 * rib.env_of(&cx, l)
                                    >= f32::from(STATUS_LIT_COV)
                        })
                    })
                    .collect();
                assert!(
                    lit.first() == Some(&2) && lit.last() == Some(&60),
                    "+{ms} ms: the lit set holds both endpoints ({lit:?})"
                );
                assert!(
                    lit.windows(2).all(|w| w[1] - w[0] <= 1),
                    "+{ms} ms: the lit set is one interval, no hole (K2): {lit:?}"
                );
                interval_frames += 1;
            }
        }
        assert!(
            interval_frames > 300,
            "the interval law was checked on {interval_frames} frames"
        );
        let bound = usize::from(WAKE_MAX_CELLS + 1) * (WAKE_LIFE_S / 0.2).ceil() as usize
            + 59 * (RETRACT_START_S / 0.2).ceil() as usize;
        let worst = *peak_by_cycle.iter().max().expect("cycles");
        assert!(
            worst <= bound,
            "the pool grew past its bound: {worst} > {bound} ({peak_by_cycle:?})"
        );
        let (early, late) = peak_by_cycle.split_at(peak_by_cycle.len() / 2);
        assert!(
            late.iter().max() <= early.iter().max(),
            "the second half of the ping-pong holds more than the first: {peak_by_cycle:?}"
        );
        let cx = ctx(at(t0, 3000 + 1250), &c, caret, 0.0);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "everything is out one wake life after the last jump"
        );
    }

    #[test]
    fn a_partial_reversal_keeps_the_whole_corridor_on_one_clock() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 43, &c, 0.9);
        let cold = at(t0, 6000);
        rib.plan(&ctx(cold, &c, (2, 43), 0.0));
        assert!(
            rib.at_rest(),
            "the typed band is gone before the first jump"
        );
        let a = at(cold, 100);
        rib.on_event(&nav((2, 43), (2, 0)), a, &ctx(a, &c, (2, 0), 0.0));
        rib.plan(&ctx(at(a, 130), &c, (2, 0), 0.0));
        let first = rib
            .cohorts()
            .iter()
            .find(|k| k.wake)
            .map(|k| k.id)
            .expect("the first corridor");
        let b = at(a, 300);
        rib.on_event(&nav((2, 0), (2, 20)), b, &ctx(b, &c, (2, 20), 0.0));
        rib.plan(&ctx(at(b, 130), &c, (2, 20), 0.0));
        let live: Vec<(u32, bool)> = rib
            .cohorts()
            .iter()
            .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
            .map(|k| (k.id, k.abandoned))
            .collect();
        assert_eq!(
            live,
            vec![(first, false)],
            "the reversal re-owns the corridor it overlaps: one live wake cohort, \
             not abandoned ({live:?})"
        );
        let cx = ctx(at(b, 130), &c, (2, 20), 0.0);
        let clocks: Vec<Instant> = rib
            .cells()
            .iter()
            .filter(|l| l.layer == Layer::Over)
            .map(|l| rib.live_since(l))
            .collect();
        assert!(clocks.len() > 30, "the corridor is laid ({})", clocks.len());
        assert_eq!(
            clocks.iter().min(),
            clocks.iter().max(),
            "every corridor cell reads one clock, covered or not"
        );
        let mut frames = 0u32;
        for ms in (0..1000u64).step_by(8) {
            let now = at(b, 130 + ms);
            let cx = ctx(now, &c, (2, 20), 0.0);
            rib.plan(&cx);
            let live: Vec<u16> = rib
                .cells()
                .iter()
                .filter(|l| l.layer == Layer::Over && rib.env_of(&cx, l) > 0.0)
                .map(|l| l.col)
                .collect();
            if live.is_empty() {
                break;
            }
            let (lo, hi) = (
                *live.iter().min().expect("min"),
                *live.iter().max().expect("max"),
            );
            assert_eq!(
                (lo..=hi).filter(|col| live.contains(col)).count(),
                usize::from(hi - lo + 1),
                "+{ms} ms: the corridor is one interval ({lo}..={hi})"
            );
            frames += 1;
        }
        let _ = cx;
        assert!(
            frames > 60,
            "the interval law was checked on {frames} frames"
        );
    }

    #[test]
    fn a_row_change_a_pty_jump_and_a_typed_move_lay_no_wake() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let cx = ctx(t0, &c, (3, 5), 0.0);
        rib.on_event(&nav((2, 5), (3, 5)), t0, &cx);
        assert!(rib.at_rest(), "an Up-arrow recall paints no line");
        let cx = ctx(t0, &c, (2, 2), 0.0);
        rib.on_event(&pty((2, 60), (2, 2)), t0, &cx);
        assert!(rib.at_rest(), "a PTY cascade earns no wake");
        rib.on_event(
            &Event::Move {
                from: (2, 60),
                to: (2, 2),
                licence: Licence::Typed,
                dir: Dir::Right,
            },
            t0,
            &cx,
        );
        assert!(rib.at_rest(), "a typed echo's own motion lays nothing");
        rib.on_event(
            &Event::Move {
                from: (2, 60),
                to: (2, 2),
                licence: Licence::Return,
                dir: Dir::Right,
            },
            t0,
            &cx,
        );
        assert!(rib.at_rest(), "a return-licensed move lays nothing");
    }

    #[test]
    fn an_arrow_hop_lays_a_short_wake_behind_the_caret_and_leaves_live_cells_alone() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let word = rib.cohorts()[0];
        assert!(!word.wake);
        let now = at(t0, 600);
        let cx = ctx(now, &c, (2, 19), 0.8);
        let t17 = rib.field_at(2, 17);
        rib.on_event(&nav((2, 18), (2, 19)), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.field_at(2, 17),
            t17,
            "the live cell beside the hop is untouched"
        );
        let laid: Vec<&Cell> = rib
            .cells()
            .iter()
            .filter(|c| c.row == 2 && c.col == 18)
            .collect();
        assert_eq!(laid.len(), 1);
        assert_ne!(
            laid[0].cohort, word.id,
            "the hop's cell is in a cohort of its own, not the word's"
        );
        let hop = rib
            .cohorts()
            .iter()
            .find(|k| k.id == laid[0].cohort)
            .expect("the hop's cohort");
        assert!(hop.wake, "…a wake cohort");
        assert!(
            (laid[0].t - word.t_at(18)).abs() < 1e-6,
            "…on the word's own walk ({} vs {})",
            laid[0].t,
            word.t_at(18)
        );
        assert!(!laid[0].typing);
        assert!((laid[0].life_s - WAKE_LIFE_S * WAKE_HOP_LIFE_SHARE).abs() < 1e-6);
        assert!(
            rib.cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == 17)
                .count()
                == 1
        );
        let word_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == word.id)
            .expect("the word");
        assert_eq!(
            (word_now.alive_at, word_now.col0, word_now.col1),
            (word.alive_at, word.col0, word.col1),
            "the hop touched the word's clock or bounds"
        );
        assert_eq!(
            word_now.phase,
            Phase::Grace,
            "180 ms after its last key the word is in its grace — a hop that refreshed it would read Laying"
        );
        let cx = ctx(
            at(t0, 420 + (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            (2, 19),
            0.0,
        );
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|c| c.cohort != word.id),
            "the word must be out {SWOOSH_TOTAL_S} s after its own last key; the arrow lifted its finger"
        );
        let mut rib = Ribbon::new();
        for (k, col) in (40u16..46).enumerate() {
            let now = at(t0, 40 * k as u64);
            let cx = ctx(now, &c, (2, col + 1), 0.0);
            rib.on_event(&nav((2, col), (2, col + 1)), now, &cx);
            rib.plan(&cx);
        }
        let cols: Vec<u16> = row_cells(&rib, 2).iter().map(|c| c.0).collect();
        assert_eq!(
            cols,
            (40..=46).collect::<Vec<_>>(),
            "the arrow painted behind itself"
        );
        assert_eq!(rib.cohorts().len(), 1, "one walk under the held arrow");
        let cx = ctx(at(t0, 200 + 100 + 560), &c, (2, 46), 0.0);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the arrow's trail is out half a wake life after its last step"
        );
    }

    fn over_cols(rib: &Ribbon, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == row && l.layer == Layer::Over)
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    fn lit_cols(rib: &Ribbon, cx: &Ctx<'_>, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| {
                l.row == row
                    && UNDER_COV_CAP * l.cov0 * rib.env_of(cx, l) >= f32::from(STATUS_LIT_COV)
            })
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn a_jump_s_cap_binds_at_the_corridor_s_far_edge_never_inside_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 100, &c, 0.9);
        let last = at(t0, 99 * 60);
        let idle_ms = ((RETRACT_START_S + RETRACT_FADE_S + 0.2) * 1000.0).round() as u64;
        let now = at(last, idle_ms);
        let cx = ctx(now, &c, (2, 102), 0.9);
        rib.plan(&cx);
        let lit_before = lit_cols(&rib, &cx, 2);
        assert!(
            lit_before.first().is_some_and(|&l| l > 2 + WAKE_MAX_CELLS)
                && lit_before.last() == Some(&101),
            "the setup: the drain frontier lies past the cap, the remnant by the origin \
             ({lit_before:?})"
        );
        let cx = ctx(now, &c, (2, 2), 0.9);
        rib.on_event(&nav((2, 102), (2, 2)), now, &cx);
        assert_eq!(
            over_cols(&rib, 2),
            (2..=101).collect::<Vec<_>>(),
            "the corridor runs from the landing out to the farthest light it keeps"
        );
        let landed = at(now, flight_ms(100.0) as u64 + 30);
        let cx = ctx(landed, &c, (2, 2), 0.9);
        rib.plan(&cx);
        let lit = lit_cols(&rib, &cx, 2);
        assert!(
            lit.first() == Some(&2)
                && lit.last() == Some(&101)
                && lit.windows(2).all(|w| w[1] - w[0] <= 1),
            "(a) the lit set is one interval holding both ends: {lit:?}"
        );
        let mut rib = Ribbon::new();
        let a = at(t0, 0);
        rib.on_event(&nav((2, 110), (2, 2)), a, &ctx(a, &c, (2, 2), 0.0));
        rib.plan(&ctx(at(a, 130), &c, (2, 2), 0.0));
        assert_eq!(
            over_cols(&rib, 2),
            (2..2 + WAKE_MAX_CELLS).collect::<Vec<_>>(),
            "the setup: the cold Ctrl-A lays the cap"
        );
        let e = at(a, 300);
        rib.on_event(&nav((2, 2), (2, 110)), e, &ctx(e, &c, (2, 110), 0.0));
        assert_eq!(
            over_cols(&rib, 2),
            (2..=110).collect::<Vec<_>>(),
            "the End's corridor reaches the island it re-owns"
        );
        let wakes = rib
            .cohorts()
            .iter()
            .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
            .count();
        assert_eq!(wakes, 1, "one wake cohort on the row (K4)");
        for ms in (160..900u64).step_by(8) {
            let cx = ctx(at(e, ms), &c, (2, 110), 0.0);
            rib.plan(&cx);
            let lit = lit_cols(&rib, &cx, 2);
            assert!(
                lit.windows(2).all(|w| w[1] - w[0] <= 1),
                "(b) +{ms} ms: the lit set is one interval (K2): {lit:?}"
            );
            if ms < 500 {
                assert!(
                    lit.first() == Some(&2) && lit.last() == Some(&110),
                    "(b) +{ms} ms: the lit set holds both ends: {lit:?}"
                );
            }
        }
    }

    #[test]
    fn a_hop_covers_a_leaving_band_from_its_level_and_leaves_a_live_one_as_it_is() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * 0.9;
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 9, &c, 0.9);
        let last = at(t0, 8 * 60);
        let band = rib.cohorts()[0];
        let now = at(last, (RETRACT_START_S * 1000.0).round() as u64 + 100);
        let cx = ctx(now, &c, (2, 11), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cohorts()[0].leaving_at(now),
            "the setup: the band is inside its swoosh"
        );
        let before: Vec<Cell> = rib.cells().to_vec();
        let level: Vec<(u16, f32)> = before
            .iter()
            .map(|l| (l.col, l.cov0 * rib.env_of(&cx, l)))
            .collect();
        assert!(
            level
                .iter()
                .filter(|&&(col, _)| col >= 6)
                .all(|&(_, v)| v > 0.1),
            "the setup: the band is still lit under the hop ({level:?})"
        );
        let cx = ctx(now, &c, (2, 6), 0.9);
        rib.on_event(&nav((2, 11), (2, 6)), now, &cx);
        assert_eq!(
            over_cols(&rib, 2),
            (6..=11).collect::<Vec<_>>(),
            "the hop lays its whole corridor over the leaving band"
        );
        for w in rib.cells().iter().filter(|l| l.layer == Layer::Over) {
            match level.iter().find(|&&(col, _)| col == w.col) {
                Some(&(col, start)) => {
                    let (re_at, lv) = w
                        .rearm
                        .unwrap_or_else(|| panic!("col {col}: over band light is re-armed"));
                    assert_eq!(re_at, w.born, "col {col}: re-armed at its birth");
                    assert!(
                        (w.cov0 * lv - start).abs() < 1e-5,
                        "col {col}: born at the band's own coverage {start} (cov0 {} level {lv})",
                        w.cov0
                    );
                }
                None => assert!(
                    w.rearm.is_none() && (w.cov0 - cov0_new).abs() < 1e-6,
                    "col {}: over nothing, a cold-priced cell from dark",
                    w.col
                ),
            }
        }
        for was in &before {
            let is = rib
                .cells()
                .iter()
                .find(|l| l.layer == Layer::Base && l.col == was.col)
                .expect("the band's cells stay");
            assert!(
                is.cohort == was.cohort
                    && is.born == was.born
                    && is.cov0 == was.cov0
                    && is.retract_at == was.retract_at,
                "the hop touched the band's cell at col {} (K1)",
                was.col
            );
        }
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.id == band.id)
                .map(|k| k.alive_at),
            Some(band.alive_at),
            "the leaving band keeps its clock"
        );
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 9, &c, 0.9);
        let now = at(last, 300);
        let cx = ctx(now, &c, (2, 11), 0.9);
        rib.plan(&cx);
        assert!(
            !rib.cohorts()[0].leaving_at(now),
            "the setup: the band is live"
        );
        let before: Vec<Cell> = rib.cells().to_vec();
        let cx = ctx(now, &c, (2, 8), 0.9);
        rib.on_event(&nav((2, 14), (2, 8)), now, &cx);
        assert_eq!(
            over_cols(&rib, 2),
            (11..=14).collect::<Vec<_>>(),
            "the hop lays only the dark ground; the live band's cells are left as they are"
        );
        for was in &before {
            assert!(
                rib.cells().iter().any(|l| l.layer == Layer::Base
                    && l.col == was.col
                    && l.born == was.born
                    && l.cohort == was.cohort),
                "the live band's cell at col {} is untouched",
                was.col
            );
        }
    }

    type CensusColumn = (u16, Option<(u32, Instant, bool, u32)>, u8);

    fn census(rib: &Ribbon, row: u16, col0: u16, col1: u16) -> Vec<CensusColumn> {
        let cw = geom().cw as f32;
        (col0..col1)
            .map(|col| {
                let owner = rib
                    .cells()
                    .iter()
                    .rposition(|c| c.row == row && c.col == col && !c.leaving())
                    .map(|i| {
                        let c = rib.cells()[i];
                        (c.cohort, c.born, c.typing, c.cov0.to_bits())
                    });
                let cov = rib
                    .plan_segments()
                    .iter()
                    .filter(|s| (s.x / cw).floor() as i64 == i64::from(col))
                    .map(|s| s.cov)
                    .max()
                    .unwrap_or(0);
                (col, owner, cov)
            })
            .collect()
    }

    fn gaps(before: &[CensusColumn], now: &[CensusColumn]) -> Vec<(u16, u8)> {
        before
            .iter()
            .zip(now)
            .filter(|(b, n)| b != n)
            .map(|(_, n)| (n.0, n.2))
            .collect()
    }

    fn type_the_owner_s_line(rib: &mut Ribbon, t0: Instant, c: &Config) -> Instant {
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 0,
            n: 44,
            period_ms: 83,
            disp: 0.8,
        };
        type_keys(rib, t0, keys, c);
        at(t0, 43 * 83)
    }

    #[test]
    fn a_scrub_inside_the_band_keeps_every_cell_s_light_and_holds_the_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let last = type_the_owner_s_line(&mut rib, t0, &c);
        let band = rib.cohorts()[0];
        let cx = ctx(at(last, 150), &c, (2, 44), 0.8);
        rib.plan(&cx);
        let before = census(&rib, 2, 0, 44);
        assert!(
            before
                .iter()
                .all(|(_, owner, cov)| owner.is_some() && *cov > 100),
            "the band is whole and hot under the line before the scrub"
        );
        let stops = [40u16, 35, 31, 26, 31, 35, 40, 44, 40, 35];
        let mut caret = (2u16, 44u16);
        let mut last_hop = last;
        for (k, &col) in stops.iter().enumerate() {
            let now = at(last, 150 * (k as u64 + 1));
            let to = (2, col);
            let cx = ctx(now, &c, to, 0.8);
            rib.on_event(&nav(caret, to), now, &cx);
            rib.plan(&cx);
            caret = to;
            last_hop = now;
            let now_census = census(&rib, 2, 0, 44);
            let g = gaps(&before, &now_census);
            assert!(
                g.is_empty(),
                "hop {} ({}→{}, +{} ms): the scrub damaged the band at {} of 44 cells: {:?}",
                k + 1,
                if k == 0 { 44 } else { stops[k - 1] },
                col,
                150 * (k + 1),
                g.len(),
                g
            );
            assert!(
                rib.cohorts().iter().all(|k| !k.abandoned),
                "hop {}: a scrub abandons nothing",
                k + 1
            );
            assert!(
                rib.cells().iter().all(|c| !c.leaving()),
                "hop {}: a scrub sends no cell on its way out",
                k + 1
            );
        }
        let held = rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("the typed band is still the band");
        assert_eq!(
            held.alive_at, last_hop,
            "the scrubbing hand holds the row — the band's clock is the hop's"
        );
        assert_eq!(held.phase, Phase::Laying);
        let cx = ctx(at(last_hop, 700), &c, caret, 0.8);
        rib.plan(&cx);
        assert!(
            gaps(&before, &census(&rib, 2, 0, 44)).is_empty(),
            "0.7 s after the last hop the band is still whole: the hops renewed its clock"
        );
        let cx = ctx(at(last_hop, 1400), &c, caret, 0.8);
        rib.plan(&cx);
        assert!(
            !rib.at_rest() && plan_sum(&rib) > 0,
            "1.4 s after the last hop the band is leaving on the hop's clock"
        );
        let cx = ctx(
            at(last_hop, (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            caret,
            0.8,
        );
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the band leaves through its swoosh {SWOOSH_TOTAL_S} s after the last hop"
        );
    }

    #[test]
    fn a_scrub_into_a_leftward_corridor_does_not_move_its_soft_end() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 0, 11, &c, 0.9);
        let last = at(t0, 10 * 60);
        for (from, to, ms) in [((2u16, 11u16), (2, 31), 200u64), ((2, 31), (2, 0), 550)] {
            let now = at(last, ms);
            let cx = ctx(now, &c, to, 0.9);
            rib.on_event(&nav(from, to), now, &cx);
            rib.plan(&cx);
        }
        let cx = ctx(at(last, 734), &c, (2, 0), 0.9);
        rib.plan(&cx);
        let corridor = |r: &Ribbon| {
            r.runs
                .iter()
                .find(|u| u.row == 2 && u.col0 == 0)
                .map(|u| (u.col0, u.col1, u.stream_dir))
                .expect("the corridor's own run")
        };
        let (c0, c1, dir_before) = corridor(&rib);
        assert!(
            c1 >= c0 + 4,
            "the setup: the corridor must be long enough to have a far end away from its landing (cols {c0}..={c1})"
        );
        let before = census(&rib, 2, c0, c1 + 2);
        let cohorts_before = rib.cohorts().len();
        let scrub_at = at(last, 750);
        let cx = ctx(scrub_at, &c, (2, 2), 0.9);
        rib.on_event(&nav((2, 0), (2, 2)), scrub_at, &cx);
        rib.plan(&cx);
        let after = census(&rib, 2, c0, c1 + 2);
        let (a0, a1, dir_after) = corridor(&rib);
        let show = |cen: &[CensusColumn]| cen.iter().map(|k| (k.0, k.2)).collect::<Vec<_>>();
        assert_eq!(
            cohorts_before,
            rib.cohorts().len(),
            "the setup: the two Rights are a scrub over lit cells and mint nothing"
        );
        assert_eq!(
            (a0, a1),
            (c0, c1),
            "the scrub moved the corridor's own bounds, so the two censuses read different columns"
        );
        assert_ne!(
            dir_before, dir_after,
            "the setup: the caret move still flips the corridor run's stream, which is what the soft end must no longer follow"
        );
        for (name, cen) in [("before the scrub", &before), ("after it", &after)] {
            let whole = cen[1].2;
            assert!(
                cen[0].2 < whole && whole > 100 && cen[1..].iter().all(|k| k.2 == whole),
                "{name}: the corridor opens soft at its left and is whole to its right edge: {:?}",
                show(cen)
            );
        }
        for (b, a) in before.iter().zip(after.iter()) {
            assert!(
                i32::from(a.2) - i32::from(b.2) <= 2,
                "col {}: the scrub raised the corridor's light {} -> {} in one frame ({:?} -> {:?})",
                b.0,
                b.2,
                a.2,
                show(&before),
                show(&after)
            );
        }
    }

    #[test]
    fn a_new_phrase_holds_none_of_a_closed_one_s_cohorts_until_the_hand_resumes_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = |col0: u16, n: u16| Keys {
            g: geom(),
            row: 2,
            col0,
            n,
            period_ms: 100,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys(2, 3), &c);
        let first = rib.phrase_open().expect("open");
        let abc = rib.cohorts()[0];
        assert_eq!(abc.phrase, first);
        type_keys(&mut rib, at(t0, 1400), keys(20, 3), &c);
        let second = rib.phrase_open().expect("open");
        assert_ne!(second, first, "a rest closed the first phrase");
        let abc_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == abc.id)
            .expect("abc is still resident inside its swoosh");
        assert_eq!(
            abc_now.phrase, first,
            "the closed phrase's cohort is not re-assigned"
        );
        assert_eq!(
            abc_now.alive_at,
            at(t0, 200),
            "…and not renewed by the new phrase's keys"
        );
        assert_ne!(abc_now.phase, Phase::Laying, "it is on its way out");
        let now = at(t0, 1700);
        let cx = ctx(now, &c, (2, 6), 0.9);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        let abc_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == abc.id)
            .expect("the resumed cohort");
        assert_eq!(
            abc_now.phrase, second,
            "resuming a line re-joins the open phrase"
        );
        assert_eq!(abc_now.alive_at, now);
        assert_eq!(abc_now.phase, Phase::Laying);
    }

    #[test]
    fn a_backspace_run_holds_the_phrase_so_the_left_end_stays_lit_through_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        let coh = rib.cohorts()[0];
        let mut caret = 22u16;
        for i in 0..8u64 {
            caret -= 1;
            let now = at(last, 100 + i * 120);
            erase_at(&mut rib, now, (2, caret), &c);
            let k = rib
                .cohorts()
                .iter()
                .find(|k| k.id == coh.id)
                .expect("the band is held");
            assert_eq!(
                k.alive_at, now,
                "erase {i}: the phrase's clock follows the hand"
            );
            assert_eq!(k.phase, Phase::Laying);
            let cx = ctx(now, &c, (2, caret), 0.9);
            let left = rib
                .cells()
                .iter()
                .find(|l| l.row == 2 && l.col == 2)
                .expect("the left end is in the pool");
            assert!(
                rib.env_of(&cx, left) > 0.99,
                "erase {i} (+{} ms after the last key): the left end is at full ({})",
                100 + i * 120,
                rib.env_of(&cx, left)
            );
        }
        let last_erase = at(last, 100 + 7 * 120);
        let cx = ctx(at(last_erase, 800), &c, (2, 14), 0.0);
        rib.plan(&cx);
        let k = rib.cohorts().iter().find(|k| k.id == coh.id).expect("held");
        assert_eq!(k.phase, Phase::Grace);
        let cx = ctx(at(last_erase, 950), &c, (2, 14), 0.0);
        rib.plan(&cx);
        let k = rib.cohorts().iter().find(|k| k.id == coh.id).expect("held");
        assert_eq!(
            k.phase,
            Phase::Reaching,
            "the rest is measured from the last erase"
        );
    }

    #[test]
    fn a_backspace_run_begun_after_the_rest_snaps_the_band_back_and_runs_the_tail_from_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 2,
                n: 20,
                period_ms: 100,
                disp: 0.9,
            },
            &c,
        );
        let id = rib.cohorts()[0].id;
        type_keys(
            &mut rib,
            at(t0, 2000),
            Keys {
                g: geom(),
                row: 5,
                col0: 2,
                n: 1,
                period_ms: 100,
                disp: 0.9,
            },
            &c,
        );
        let last = at(t0, 2000);
        let other = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 5)
            .expect("row 5")
            .alive_at;
        let glow = at(last, 1400);
        let cx = ctx(glow, &c, (2, 22), 0.0);
        rib.plan(&cx);
        let leaving = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .expect("row 2 is still resident");
        assert!(
            leaving.phase != Phase::Laying && leaving.phase != Phase::Grace,
            "the fixture must erase into a band that is already leaving ({:?})",
            leaving.phase
        );
        let dim = lit_row(&rib, &cx, 2, 1.0).len();
        let full_env = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && rib.env_of(&cx, l) > 0.99)
            .count();
        let bs0 = at(glow, 0);
        erase_at(&mut rib, bs0, (2, 21), &c);
        let cx = ctx(bs0, &c, (2, 21), 0.0);
        let k = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .expect("the band is taken back");
        assert_eq!(k.alive_at, bs0, "the clock follows the hand that came back");
        assert_eq!(k.phase, Phase::Laying);
        let back = lit_row(&rib, &cx, 2, 1.0).len();
        let back_env = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && rib.env_of(&cx, l) > 0.99)
            .count();
        assert!(
            back > dim && back_env > full_env,
            "the erase must SNAP the band back ({dim} -> {back} lit, {full_env} -> {back_env} at full)"
        );
        let left = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 2)
            .expect("the left end is in the pool");
        assert!(
            rib.env_of(&cx, left) > 0.99,
            "…and the left end is back at full ({})",
            rib.env_of(&cx, left)
        );
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 5)
                .expect("row 5 is resident")
                .alive_at,
            other,
            "an erase on row 2 must not renew row 5"
        );
        let mut caret = 21u16;
        for i in 1..8u64 {
            caret -= 1;
            erase_at(&mut rib, at(glow, i * 120), (2, caret), &c);
        }
        let last_erase = at(glow, 7 * 120);
        let mut tail_ms = 0u64;
        for ms in (0..=2400).step_by(20) {
            let cx = ctx(at(last_erase, ms), &c, (2, caret), 0.0);
            rib.plan(&cx);
            if rib.cells().iter().any(|l| l.row == 2) {
                tail_ms = ms;
            } else {
                break;
            }
        }
        assert!(
            tail_ms >= 1176,
            "the band must outlive the last erase by at least the 1175.8 ms v0.85.0 measured ({tail_ms} ms)"
        );
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        assert!(
            tail_ms <= total + 60,
            "…and by no more than its own swoosh ({tail_ms} ms against {total} ms)"
        );
    }

    #[test]
    fn a_row_the_caret_is_not_on_retracts_into_its_own_nearest_end_in_logical_order() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let below = |to: (u16, u16)| {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 4, 20, &c, 0.9);
            let last = at(t0, 19 * 60);
            let now = at(last, 100);
            let cx = ctx(now, &c, to, 0.9);
            rib.on_event(&nav((2, 24), to), now, &cx);
            rib.plan(&cx);
            let k = rib.cohorts()[0];
            let env = |col: u16, ms: u64| {
                let cx = ctx(at(now, ms), &c, to, 0.0);
                let cell = rib
                    .cells()
                    .iter()
                    .find(|l| l.row == 2 && l.col == col)
                    .expect("cell");
                rib.env_of(&cx, cell)
            };
            (k, env(4, 200), env(23, 200))
        };
        let (k, left, right) = below((1, 24));
        assert!(k.abandoned && k.phase.is_retracting());
        assert_eq!(
            k.retract_col,
            Some(4),
            "a row below the caret pulls into its left end"
        );
        assert!(k.drain_right_first, "…and drains from its right end");
        assert!(
            right < left,
            "200 ms into the retract the right end has spent more than the left ({right} vs {left})"
        );
        let (k, left, right) = below((3, 0));
        assert_eq!(
            k.retract_col,
            Some(23),
            "a row above the caret pulls into its right end — the fold — not toward col 0 on another row"
        );
        assert!(!k.drain_right_first);
        assert!(
            left < right,
            "…and drains from its left end ({left} vs {right})"
        );
    }

    fn cell_cov(rib: &Ribbon, g: Geom, col: u16) -> u8 {
        let x0 = f32::from(g.origin_x) + f32::from(col) * g.cw as f32;
        let x1 = x0 + g.cw as f32;
        rib.plan_segments()
            .iter()
            .filter(|s| s.x > x0 + 0.5 && s.x < x1 - 0.5)
            .map(|s| s.cov)
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn a_return_dims_the_line_it_leaves() {
        let c = cfg(false, true);
        let t0 = Instant::now();
        let lit_after = |licence: Licence| -> (u8, bool) {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 2, 8, &c, 0.9);
            let leave = at(t0, 8 * 60 + 40);
            let cx = ctx(leave, &c, (3, 0), 0.9);
            rib.on_event(&row_change((2, 10), (3, 0), licence), leave, &cx);
            rib.plan(&cx);
            let late = at(leave, 100);
            let cx = ctx(late, &c, (3, 0), 0.9);
            rib.plan(&cx);
            let cov = rib.plan_segments().iter().map(|s| s.cov).max().unwrap_or(0);
            (cov, rib.cells().iter().any(|c| c.row == 2 && c.leaving()))
        };
        let (ret, ret_leaving) = lit_after(Licence::Return);
        let (nav, _) = lit_after(Licence::Nav);
        assert!(nav > 0, "control: an arrow down leaves the band lit");
        assert!(
            f32::from(ret) <= RETURN_PRINTED_SHARE * f32::from(nav) + 2.0,
            "the line a Return left still stands at {ret} of the arrow's {nav} at 100 ms"
        );
        assert!(
            ret > 0 && !ret_leaving,
            "…but dimmed, not cut: it goes out through its own swoosh"
        );
    }

    #[test]
    fn the_paper_ribbon_keeps_its_yellow_clean() {
        for (fg, bg) in [(0x001F_2328, 0x00FF_FFFF), (0x002B_2D33, 0x00F6_F7F9)] {
            let mut c = cfg(false, true);
            (c.theme_fg, c.theme_bg) = (fg, bg);
            let mut lut = BedInkLut::default();
            lut.sync(&c);
            let a = (cov_cap(&c) + 16.0).min(255.0) as u8;
            let lay = |ink: u32| over_premul(bg, premul_rgb(ink, a), a) & 0x00FF_FFFF;
            let yellow = (0..BED_INK_LUT_LEN)
                .map(|i| i as f32 / (BED_INK_LUT_LEN - 1) as f32)
                .min_by(|x, y| {
                    let d = |t: f32| {
                        let (h, ..) = rgb2hsv(spectrum(walk_pace(t)));
                        (h - 60.0).abs()
                    };
                    d(*x).total_cmp(&d(*y))
                })
                .expect("a walk");
            let (h, s, v) = rgb2hsv(lay(lut.at(yellow)));
            assert!(
                (48.0..=68.0).contains(&h) && v > 0.85 && s > 0.2,
                "on #{bg:06X} the yellow lays ({h:.0}°, {s:.2}, {v:.2})"
            );
            let law = crate::cursor_glow::light_body_ink(spectrum(walk_pace(yellow)), fg, bg, a);
            let (_, _, lv) = rgb2hsv(lay(law));
            assert!(
                lv < 0.8,
                "control: the body law's yellow is not olive ({lv:.2})"
            );
        }
        let (h, ..) = rgb2hsv(paper_caret_ink(
            crate::color_math::hsv2rgb(68.0, 1.0, 1.0),
            0x00FF_FFFF,
        ));
        assert!(
            h <= 45.0,
            "a greenish yellow's caret sits at hue {h:.1}, not amber"
        );
    }

    #[test]
    fn a_retired_cell_melts_to_exactly_nothing_inside_150_ms_and_never_brightens() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        let fired = at(last, 40);
        let cx = ctx(fired, &c, (2, 18), 0.9);
        rib.plan(&cx);
        assert_eq!(
            rib.slabs_per_cell(),
            SLABS_PER_CELL,
            "interior slabs exist to read"
        );
        let before = (cell_cov(&rib, g, 16), cell_cov(&rib, g, 17));
        assert!(
            before.0 > 0 && before.1 > 0,
            "the head cells are lit: {before:?}"
        );
        let mut control = rib.clone();
        let born_of = |rib: &Ribbon, col: u16| {
            rib.cells()
                .iter()
                .find(|l| l.row == 2 && l.col == col)
                .map(|l| l.born)
                .expect("laid")
        };
        let (b16, b17) = (born_of(&rib, 16), born_of(&rib, 17));
        assert_eq!(
            rib.retire_cells(&[(2, 15, fired)], fired),
            0,
            "a cell at a laid position with ANOTHER birth is not it (identity, not position)"
        );
        assert_eq!(
            rib.retire_cells(
                &[(2, 16, b16), (2, 17, b17), (2, 99, fired), (5, 16, b16)],
                fired
            ),
            2,
            "two live cells stamped; a cell that is not laid is not counted"
        );
        assert_eq!(
            rib.retire_cells(&[(2, 16, b16)], at(fired, 8)),
            0,
            "a retired cell is not restamped"
        );
        let mut prev = before;
        let mut gone_by: Option<u64> = None;
        for k in 1..=20u64 {
            let ms = 8 * k;
            let now = at(fired, ms);
            let cx = ctx(now, &c, (2, 18), 0.9);
            rib.plan(&cx);
            control.plan(&cx);
            let here = (cell_cov(&rib, g, 16), cell_cov(&rib, g, 17));
            assert!(
                here.0 <= prev.0 && here.1 <= prev.1,
                "+{ms} ms: the melt is monotone, got {here:?} after {prev:?}"
            );
            assert!(
                here.0 <= before.0 && here.1 <= before.1,
                "+{ms} ms: never brighter than the cell was ({before:?}), got {here:?}"
            );
            prev = here;
            let resident = rib
                .cells()
                .iter()
                .any(|l| l.row == 2 && (l.col == 16 || l.col == 17));
            if here == (0, 0) && !resident && gone_by.is_none() {
                gone_by = Some(ms);
            }
            for col in [10u16, 11, 12, 13] {
                assert_eq!(
                    cell_cov(&rib, g, col),
                    cell_cov(&control, g, col),
                    "+{ms} ms: cell {col} is untouched by the retirement"
                );
            }
        }
        assert!(
            gone_by.is_some_and(|ms| ms <= 150),
            "off the glass and out of the pool inside 150 ms, got {gone_by:?}"
        );
        assert!(
            rib.cells().iter().any(|l| l.row == 2 && l.col == 15),
            "the un-retired band is still resident"
        );
    }

    #[test]
    fn a_released_run_is_abandoned_into_its_retract_and_a_cell_already_leaving_keeps_its_clock() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        let last = at(t0, 5 * 60);
        erase_at(&mut rib, at(last, 50), (2, 15), &c);
        let stamp_15 = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 15)
            .and_then(|l| l.retract_at);
        assert!(stamp_15.is_some(), "the erased cell is retracting");
        let named: Vec<(u16, u16, Instant)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2)
            .map(|l| (l.row, l.col, l.born))
            .collect();
        let released = rib.release_cells(&named, at(last, 60));
        assert_eq!(
            released, 5,
            "the five live cells; the retracting one keeps its clock"
        );
        assert!(
            rib.cells()
                .iter()
                .all(|l| l.row != 2 || l.retire_at.is_none()),
            "not one cell is stamped on the melt"
        );
        assert_eq!(
            rib.cells()
                .iter()
                .find(|l| l.row == 2 && l.col == 15)
                .and_then(|l| l.retract_at),
            stamp_15,
            "the retracting cell keeps its own clock"
        );
        let coh = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .copied()
            .expect("the row's cohort");
        assert!(coh.abandoned, "the cohort is abandoned");
        assert!(!coh.rejoinable, "…and not rejoinable: the text is gone");
        let cx = ctx(at(last, 76), &c, (2, 15), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 2)
                .is_some_and(|k| k.phase.is_retracting()),
            "on the next plan the cohort is in its retract"
        );
        assert!(
            rib.cells().iter().filter(|l| l.row == 2).count() >= 5,
            "…and the five cells are still resident, drawn in, not cut"
        );
        let alive_at = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .map(|k| k.alive_at);
        rib.release_cells(&named, at(last, 90));
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 2)
                .map(|k| k.alive_at),
            alive_at,
            "a repeat release does not rewind the clock again"
        );
        let cx = ctx(
            at(
                last,
                60 + ((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64 + 20,
            ),
            &c,
            (2, 15),
            0.9,
        );
        rib.plan(&cx);
        assert!(
            !rib.cells().iter().any(|l| l.row == 2),
            "the released run is out of the pool the retract's own span later, \
             not {RETIRE_MELT_S} s later"
        );
    }

    #[test]
    fn a_partial_release_is_restored_by_its_stamped_cells_when_a_key_joined_the_cohort() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        let last = at(t0, 5 * 60);
        let id = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .map(|k| k.id)
            .expect("the row's cohort");
        let born_15 = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 15)
            .map(|l| l.born)
            .expect("cell 15");
        let cell_15 = |rib: &Ribbon| {
            rib.cells()
                .iter()
                .find(|l| l.row == 2 && l.col == 15 && l.born == born_15)
                .copied()
                .expect("cell 15 is resident")
        };
        let stamp = at(last, 60);
        assert_eq!(
            rib.release_cells(&[(2, 15, born_15)], stamp),
            1,
            "one cell stamped"
        );
        assert_eq!(cell_15(&rib).released_at, Some(stamp));
        assert!(cell_15(&rib).leaving(), "…and leaving");
        for reduced_motion in [false, true] {
            let mut c = cfg(true, true);
            c.reduced_motion = reduced_motion;
            let mut held = rib.clone();
            let mut absent = rib.clone();
            absent.cells.retain(|cell| cell.released_at != Some(stamp));
            for ms in [300, 400, 500] {
                let now = at(stamp, ms);
                let cx = ctx(now, &c, (2, 16), 0.9);
                held.plan(&cx);
                absent.plan(&cx);
                assert_eq!(cell_15(&held).released_at, Some(stamp));
                assert!(held.field_at(2, 15).is_none(), "custody is dark");
                assert_eq!(held.cells.len(), absent.cells.len() + 1);
                let expected = absent.next_change_deadline(now).expect("standing prefix");
                assert!(
                    expected > now + super::super::TAIL_FLOOR,
                    "reduced={reduced_motion}, +{ms}ms: the prefix has no immediate fade"
                );
                assert_eq!(
                    held.next_change_deadline(now),
                    Some(expected),
                    "reduced={reduced_motion}, +{ms}ms: dark custody adds no visual wake"
                );
            }
        }
        let key = at(last, 80);
        let cx = ctx(key, &c, (2, 17), 0.9);
        rib.on_event(&typed(), key, &cx);
        rib.plan(&cx);
        let on_row: Vec<(u16, u32)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2)
            .map(|l| (l.col, l.cohort))
            .collect();
        assert_eq!(
            rib.cohorts().iter().filter(|k| k.row == 2).count(),
            1,
            "one cohort on the row: {on_row:?}"
        );
        let count = rib.cells().iter().filter(|l| l.cohort == id).count();
        assert_eq!(count, 7, "the cohort grew by one: {on_row:?}");
        let alive_at = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .map(|k| k.alive_at);
        let mut accepted = Vec::new();
        rib.restore_releases(&[(id, Some(stamp))], &mut accepted);
        assert_eq!(
            accepted,
            vec![id],
            "the restore is accepted by the stamp's own cells, the cohort's count notwithstanding"
        );
        assert!(!cell_15(&rib).leaving(), "the stamp is lifted");
        assert_eq!(cell_15(&rib).released_at, None);
        let coh = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .copied()
            .expect("the cohort");
        assert!(coh.release_clock.is_none(), "the clock is handed back");
        assert_eq!(Some(coh.alive_at), alive_at, "…and was never touched");
        assert!(!coh.abandoned);
        rib.restore_releases(&[(id, Some(stamp))], &mut accepted);
        assert!(accepted.is_empty(), "nothing is released any more");
    }

    #[test]
    fn a_relay_over_a_content_retired_cell_has_a_fresh_witness_identity() {
        use super::super::witness::{RowSample, Witness};

        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 2, &c, 0.9);
        let born = |rib: &Ribbon, col| {
            rib.cells()
                .iter()
                .find(|cell| cell.row == 2 && cell.col == col)
                .expect("typed cell")
                .born
        };
        let old = born(&rib, 10);
        let neighbor = born(&rib, 11);
        let mut witness = Witness::new();
        let mut retired = Vec::new();
        let mut released = Vec::new();
        let original: Vec<char> = "          ab".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &original,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert!(retired.is_empty());
        let fired = at(t0, 100);
        assert_eq!(rib.retire_cells(&[(2, 10, old)], fired), 1);
        let fresh = at(t0, 116);
        rib.on_event(&typed(), fresh, &ctx(fresh, &c, (2, 11), 0.9));
        assert_eq!(born(&rib, 10), fresh, "a retired identity cannot be re-wet");
        assert_eq!(born(&rib, 11), neighbor, "the neighboring key is untouched");
        assert_eq!(rib.retire_cells(&[(2, 10, old)], fresh), 0);

        let replacement: Vec<char> = "          Xb".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &replacement,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert!(retired.is_empty(), "the fresh key arms its own record");
        let changed: Vec<char> = "          Yb".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &changed,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert_eq!(
            retired,
            vec![(2, 10, fresh)],
            "later changes retire the new identity"
        );
    }

    #[test]
    fn a_relocation_pays_a_content_retired_destination_and_preserves_live_neighbors() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 3, &c, 0.9);
        let before: Vec<_> = rib
            .cells()
            .iter()
            .map(|cell| (cell.col, cell.born))
            .collect();
        let old = before
            .iter()
            .find(|&&(col, _)| col == 11)
            .expect("middle")
            .1;
        let fired = at(t0, 160);
        assert_eq!(rib.retire_cells(&[(2, 11, old)], fired), 1);
        let fresh = at(t0, 176);
        rib.relay_word(2, 10, 13, fresh, None);
        for col in 10..13 {
            let cell = rib
                .cells()
                .iter()
                .find(|cell| cell.row == 2 && cell.col == col)
                .expect("relocated word");
            assert!(
                !cell.leaving(),
                "column {col} belongs to the relocated word"
            );
            let expected = if col == 11 {
                fresh
            } else {
                before.iter().find(|&&(c, _)| c == col).expect("neighbor").1
            };
            assert_eq!(
                cell.born, expected,
                "only the retired destination is paid again"
            );
        }
        assert_eq!(
            rib.retire_cells(&[(2, 11, old)], fresh),
            0,
            "stale identity has no authority over the relocation"
        );
    }

    #[test]
    fn a_live_retype_with_a_changed_glyph_does_not_inherit_the_old_witness() {
        use super::super::witness::{RowSample, Witness};

        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 1, &c, 0.9);
        let mut witness = Witness::new();
        let mut retired = Vec::new();
        let mut released = Vec::new();
        let original: Vec<char> = "          a".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &original,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert!(retired.is_empty());
        let before_ctx = ctx(at(t0, 99), &c, (2, 11), 0.9);
        rib.plan(&before_ctx);
        let before = cell_cov(&rib, geom(), 10);
        assert!(before > 0, "the old glyph is visibly lit");
        let old_attack = rib.cells()[0].attack_at;
        let fresh = at(t0, 100);
        rib.on_event(&typed(), fresh, &ctx(fresh, &c, (2, 11), 0.9));
        let after_ctx = ctx(at(t0, 101), &c, (2, 11), 0.9);
        rib.plan(&after_ctx);
        assert_eq!(rib.cells()[0].born, fresh, "fresh key, fresh identity");
        assert_eq!(rib.cells()[0].attack_at, old_attack);
        assert!(
            cell_cov(&rib, geom(), 10) >= before,
            "re-wetting cannot dim the old light"
        );
        assert!(
            !rib.brisk(after_ctx.now),
            "a settled attack is not restarted for scheduling either"
        );
        let replacement: Vec<char> = "          X".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &replacement,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert!(
            retired.is_empty(),
            "a genuine retyped glyph must not be mistaken for an unlicensed overwrite: {retired:?}"
        );
    }

    #[test]
    fn focus_regained_inside_the_ember_comes_back_up_through_edge_in_not_a_snap() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.8);
        let last = at(t0, 5 * 60);
        let cx = ctx(at(last, 100), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let full = plan_peak(&rib);
        let full_sum = plan_sum(&rib);
        rib.on_event(&Event::Focus(false), at(last, 100), &cx);
        let cx = ctx(at(last, 250), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let dimmed = plan_peak(&rib);
        let dim_sum = plan_sum(&rib);
        let mut twin = Ribbon::new();
        type_run(&mut twin, t0, 10, 6, &c, 0.8);
        let blur_cx = ctx(at(last, 100), &c, (2, 16), 0.8);
        twin.on_event(&Event::Focus(false), at(last, 100), &blur_cx);
        let end_cx = ctx(at(last, 399), &c, (2, 16), 0.8);
        twin.plan(&end_cx);
        let floor_sum = plan_sum(&twin);
        let range = full_sum.saturating_sub(floor_sum);
        let burned = full_sum.saturating_sub(dim_sum);
        assert!(
            dimmed < full,
            "the ember must be burning ({full} → {dimmed})"
        );
        assert!(
            range > 0 && floor_sum > 0,
            "the fixture must leave the band SOME range to spend and some light at the floor \
             ({full_sum} → {floor_sum})"
        );
        assert!(
            burned as f32 >= range as f32 * (1.0 - spend(0.5) - 0.05),
            "the ember must be well under way: {burned} of {range} spendable \
             ({full_sum} → {dim_sum}, floor {floor_sum})"
        );
        rib.on_event(&Event::Focus(true), at(last, 250), &cx);
        let cx = ctx(at(last, 251), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let back = plan_peak(&rib);
        assert!(
            back <= dimmed + 3,
            "focus regain snapped the ribbon from {dimmed} to {back} in one frame"
        );
        let cx = ctx(at(last, 290), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let rearmed = plan_peak(&rib);
        assert!(
            rearmed + 3 >= full,
            "after the edge-in the ribbon must be back at its level ({full} vs {rearmed})"
        );
    }

    const fn census_paint(px: u32, light: bool) -> bool {
        let r = ((px >> 16) & 0xff) as u8 as f32;
        let g = ((px >> 8) & 0xff) as u8 as f32;
        let b = (px & 0xff) as u8 as f32;
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        let level = if light {
            mn <= 255.0 - 60.0
        } else {
            mx >= 60.0
        };
        level && mx - mn >= 40.0
    }

    fn lit_row(rib: &Ribbon, cx: &Ctx<'_>, row: u16, scale: f32) -> Vec<u16> {
        let bg = cx.cfg.theme_bg;
        let light = !cx.cfg.dark_theme;
        let mut out: Vec<u16> = Vec::new();
        for cell in rib.cells().iter().filter(|c| c.row == row) {
            let coh = rib.cohorts.iter().find(|c| c.id == cell.cohort);
            let a = (rib.cov_of(cx, cell, coh) * scale)
                .round()
                .clamp(0.0, 255.0) as u8;
            if a > 0 && census_paint(over_premul(bg, premul_rgb(rib.ink.at(cell.t), a), a), light) {
                out.push(cell.col);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn dark_run(lit: &[u16]) -> u16 {
        let (mut worst, mut run) = (0u16, 0u16);
        let (Some(&first), Some(&last)) = (lit.first(), lit.last()) else {
            return 0;
        };
        for col in first..=last {
            if lit.contains(&col) {
                run = 0;
            } else {
                run += 1;
                worst = worst.max(run);
            }
        }
        worst
    }

    #[test]
    fn the_focus_ember_never_opens_a_dark_run_inside_the_band() {
        for (theme, fg, bg) in [
            ("default #111318", SHIPPED_FG, SHIPPED_BG),
            ("Nord", NORD_FG, NORD_BG),
        ] {
            let mut c = cfg(true, true);
            c.theme_fg = fg;
            c.theme_bg = bg;
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_keys(
                &mut rib,
                t0,
                Keys {
                    g: geom(),
                    row: 2,
                    col0: 2,
                    n: 60,
                    period_ms: 40,
                    disp: 0.9,
                },
                &c,
            );
            let last = at(t0, 59 * 40);
            let caret = (2u16, 62u16);
            let cx = ctx(at(last, 60), &c, caret, 0.9);
            rib.plan(&cx);
            let full = lit_row(&rib, &cx, 2, 1.0);
            assert!(
                full.len() >= 40,
                "{theme}: the fixture's band is lit before the blur ({} cells)",
                full.len()
            );
            assert_eq!(dark_run(&full), 0, "{theme}: the fixture starts whole");
            let blur = at(last, 60);
            rib.on_event(&Event::Focus(false), blur, &cx);
            let (mut worst_new, mut worst_bare, mut bare_split_frames) = (0u16, 0u16, 0u32);
            let mut shrunk_while_lit = Vec::new();
            for ms in (0..=300).step_by(4) {
                let now = at(blur, ms);
                let cx = ctx(now, &c, caret, 0.0);
                rib.plan(&cx);
                let burned = ms as f32 / 1000.0 / FOCUS_EMBER_S;
                let now_lit = lit_row(&rib, &cx, 2, 1.0);
                let burning = rib.ember_at.take();
                let bare_lit = lit_row(&rib, &cx, 2, spend(clamp01(burned)));
                rib.ember_at = burning;
                worst_new = worst_new.max(dark_run(&now_lit));
                let bd = dark_run(&bare_lit);
                worst_bare = worst_bare.max(bd);
                if bd >= 2 {
                    bare_split_frames += 1;
                }
                if !now_lit.is_empty() && now_lit != full {
                    shrunk_while_lit.push((ms, now_lit.len()));
                }
            }
            assert_eq!(
                worst_new, 0,
                "{theme}: the ember opened a dark run of {worst_new} cells inside the band"
            );
            assert!(
                shrunk_while_lit.is_empty(),
                "{theme}: cells left the band before the end of the ember: {shrunk_while_lit:?}"
            );
            let gone = ctx(at(blur, 301), &c, (2, 62), 0.0);
            rib.plan(&gone);
            assert!(
                lit_row(&rib, &gone, 2, 1.0).is_empty(),
                "{theme}: the ember's end takes every cell"
            );
            assert!(
                worst_bare >= 2 && bare_split_frames >= 2,
                "{theme}: the fixture cannot show the defect it pins \
                 (worst bare-spend dark run {worst_bare} cells over {bare_split_frames} frames)"
            );
        }
    }

    #[test]
    fn the_hot_edge_embers_out_with_its_body_on_focus_loss() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let lost = at(t0, 7 * 60 + 100);
        let mut sink = Sink::default();
        let frame = |rib: &mut Ribbon, sink: &mut Sink, now: Instant| {
            let cx = ctx(now, &c, (2, 18), 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        };
        frame(&mut rib, &mut sink, lost);
        let full = peak_channel(&sink.out);
        assert!(
            full > 0,
            "a hot hand must leave a hot edge for the law to have been tested"
        );
        rib.on_event(&Event::Focus(false), lost, &ctx(lost, &c, (2, 18), 0.9));
        frame(&mut rib, &mut sink, at(lost, 150));
        let mid = peak_channel(&sink.out);
        assert!(
            mid < full,
            "halfway through the ember the hot edge must already be spending ({full} → {mid})"
        );
        frame(&mut rib, &mut sink, at(lost, 250));
        assert!(
            !sink.under.is_empty(),
            "the body must still be on glass for the pin to mean anything"
        );
        let low = peak_channel(&sink.out);
        let cx = ctx(at(lost, 250), &c, (2, 18), 0.9);
        let head = rib
            .cells()
            .iter()
            .max_by_key(|l| l.col)
            .copied()
            .expect("the head is in the pool");
        let env = rib.env_of(&cx, &head);
        let share = low as f32 / full as f32;
        assert!(
            (share - env).abs() < 0.06,
            "+250 ms into the ember the hot edge is at {low} of {full} (share {share:.3}); \
             it must ride the head cell's own envelope ({env:.3}), not a clock of its own"
        );
        let end_cx = ctx(at(lost, 299), &c, (2, 18), 0.9);
        let env_end = rib.env_of(&end_cx, &head);
        let law = env_end + (1.0 - env_end) * spend(250.0 / 300.0);
        assert!(
            low <= mid && (env - law).abs() < 0.03 && env_end < 1.0,
            "…and it must be going out with its body on the ember's own clock, not holding \
             until the pool clears ({full} → {mid} → {low}, envelope {env:.3}, the ember's \
             law at this instant {law:.3}, its floor {env_end:.3})"
        );
        frame(&mut rib, &mut sink, at(lost, 300));
        assert!(
            sink.out.is_empty()
                && rib.at_rest()
                && rib.next_change_deadline(at(lost, 300)).is_none(),
            "…and at the ember's end there is nothing, hairline included, and no wake asked for"
        );
    }

    #[test]
    fn a_typed_key_under_a_burning_ember_clears_it_and_relights_from_the_level() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let lost = at(t0, 7 * 60 + 100);
        rib.plan(&ctx(lost, &c, (2, 18), 0.9));
        let pre = plan_peak(&rib);
        rib.on_event(&Event::Focus(false), lost, &ctx(lost, &c, (2, 18), 0.9));
        let cx = ctx(at(lost, 150), &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mid = plan_peak(&rib);
        let full = (UNDER_COV_CAP * BODY_COLD_SHARE) as u8;
        assert!(
            mid > 0 && mid < pre,
            "halfway through the ember the body is spending ({pre} → {mid}, cold ceiling {full})"
        );
        let key = at(lost, 150);
        let cx = ctx(key, &c, (2, 19), 0.9);
        rib.on_event(&typed(), key, &cx);
        rib.plan(&cx);
        assert!(
            plan_peak(&rib) >= mid,
            "the key re-arms the run from the ember's level, never below it ({} < {mid})",
            plan_peak(&rib)
        );
        let cx = ctx(at(key, 30), &c, (2, 19), 0.9);
        rib.plan(&cx);
        assert!(
            plan_peak(&rib) >= full - 2,
            "one edge-in after the key the run is back at its ceiling ({})",
            plan_peak(&rib)
        );
        let cx = ctx(at(lost, 320), &c, (2, 19), 0.9);
        rib.plan(&cx);
        assert!(
            !rib.at_rest() && plan_peak(&rib) >= full - 2,
            "past the ember's end the ribbon is still on the glass: the key cleared the ember"
        );
    }

    #[test]
    fn a_curtain_gathers_the_band_into_its_hand_over_a_quarter_second_with_no_cliff_and_lays_nothing()
     {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        let caret = (3u16, 0u16);
        let mut sink = Sink::default();
        #[derive(Clone, Copy)]
        struct Sample {
            peak: u8,
            sum: u32,
            left: f32,
            cells: usize,
            segs: usize,
            ink: usize,
        }
        let frame = |rib: &mut Ribbon, sink: &mut Sink, now: Instant| -> Sample {
            let cx = ctx(now, &c, caret, 0.9);
            rib.plan(&cx);
            let segs = rib.lit_segments().count();
            let ink = rib.ink_segments().count();
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            Sample {
                peak: sink.under.iter().map(|q| q.alpha).max().unwrap_or(0),
                sum: sink.under.iter().map(|q| u32::from(q.alpha)).sum(),
                left: sink
                    .under
                    .iter()
                    .map(|q| f32::from(q.x))
                    .fold(f32::INFINITY, f32::min),
                cells: (0..40u16)
                    .filter(|&col| cell_lit(&sink.under, g, 2, col))
                    .count(),
                segs,
                ink,
            }
        };
        let before = frame(&mut rib, &mut sink, at(last, 8));
        assert!(
            before.peak > 200 && before.cells == 20 && before.segs >= 20,
            "the band is whole before the seam (peak {}, {} cells, {} segments)",
            before.peak,
            before.cells,
            before.segs
        );
        let fell = at(last, 16);
        rib.curtain(fell);
        assert!(rib.curtained());
        assert!(rib.brisk(fell), "a falling curtain asks for every frame");
        assert!(
            rib.next_change_deadline(fell)
                .is_some_and(|d| d <= fell + Duration::from_secs_f32(CURTAIN_S)),
            "…and books its end"
        );
        let cells = rib.cells().len();
        let cx = ctx(at(fell, 40), &c, (3, 1), 0.9);
        rib.on_event(&typed(), at(fell, 40), &cx);
        rib.on_event(
            &Event::Sweep {
                row: 3,
                col0: 0,
                col1: 1,
            },
            at(fell, 40),
            &cx,
        );
        rib.on_event(&nav((3, 1), (3, 40)), at(fell, 40), &cx);
        assert_eq!(rib.cells().len(), cells, "a curtain lays nothing");
        let mut prev = before;
        let mut worst_step = 0usize;
        let mut worst_ink_step = 0usize;
        let mut ink_frames = 0usize;
        let mut dark_at = None;
        for ms in [33u64, 66, 99, 132, 165, 198, 231, 264, 297, 330] {
            let now = frame(&mut rib, &mut sink, at(fell, ms));
            assert!(
                now.peak <= prev.peak,
                "+{ms} ms: the curtain never brightens ({} → {})",
                prev.peak,
                now.peak
            );
            assert!(
                now.sum <= prev.sum + prev.sum / 500,
                "+{ms} ms: the band's total light rose ({} → {})",
                prev.sum,
                now.sum
            );
            if now.sum > 0 {
                assert!(
                    now.left >= prev.left - 0.5,
                    "+{ms} ms: the band is drawn toward its head, never away ({} → {})",
                    prev.left,
                    now.left
                );
            }
            let step = prev.segs.saturating_sub(now.segs);
            let ink_step = prev.ink.saturating_sub(now.ink);
            worst_step = worst_step.max(step);
            worst_ink_step = worst_ink_step.max(ink_step);
            let bar = CURTAIN_MAX_STEP_FRAC * before.segs.max(before.ink) as f32;
            assert!(
                step as f32 <= bar && ink_step as f32 <= bar,
                "+{ms} ms: the curtain stepped {step} claimed / {ink_step} inked segments in one \
                 frame ({} → {} / {} → {}); the bound is {bar:.1} \
                 ({CURTAIN_MAX_STEP_FRAC} of the {} it fell on)",
                prev.segs,
                now.segs,
                prev.ink,
                now.ink,
                before.segs.max(before.ink)
            );
            if now.ink > 0 {
                ink_frames += 1;
            }
            if now.ink == 0 && dark_at.is_none() {
                dark_at = Some(ms);
            }
            prev = now;
        }
        assert!(
            dark_at.is_some_and(|ms| ms >= 132),
            "the band went dark at {dark_at:?} ms — a curtain lasts CURTAIN_S"
        );
        assert!(
            worst_ink_step > 0,
            "the sweep never saw the band leave: it was already dark"
        );
        assert!(
            prev.sum * 8 < before.sum,
            "the fall spent nothing: {} → {} over the whole sweep",
            before.sum,
            prev.sum
        );
        assert!(
            ink_frames >= CURTAIN_MIN_FRAMES,
            "the curtain drew ink on {ink_frames} of the swept frames; the law is \
             {CURTAIN_MIN_FRAMES} (a cut draws one, a one-clock curtain two)"
        );
        let end = frame(&mut rib, &mut sink, at(fell, 250));
        assert_eq!((end.peak, end.segs, end.ink), (0, 0, 0));
        assert!(rib.at_rest() && !rib.curtained());
        let after = at(fell, 300);
        let cx = ctx(after, &c, (4, 6), 0.9);
        rib.on_event(&typed(), after, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.cells().len(),
            1,
            "after the curtain a key lays its cell"
        );
        for seam_caret in [(3u16, 0u16), (2, 0)] {
            let mut rib2 = Ribbon::new();
            type_run(&mut rib2, t0, 10, 20, &c, 0.9);
            let cx = ctx(at(last, 8), &c, seam_caret, 0.9);
            rib2.plan(&cx);
            rib2.curtain(fell);
            let near_end = at(fell, 216);
            let cx = ctx(near_end, &c, seam_caret, 0.9);
            rib2.plan(&cx);
            {
                let mut f2 = sink.frame();
                rib2.emit(&cx, &mut f2);
            }
            let left_x = sink
                .under
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min);
            let head_x = f32::from(g.origin_x) + 30.0 * cw;
            assert!(
                !sink.under.is_empty(),
                "seam caret {seam_caret:?}: the band was gone before the gather could be read"
            );
            assert!(
                (left_x - head_x).abs() <= 3.0 * cw,
                "seam caret {seam_caret:?}: the band gathered to x {left_x:.1}, not to its own \
                 head at {head_x:.1} (column 0 is x {:.1}, {:.0} cells away)",
                f32::from(g.origin_x),
                (head_x - f32::from(g.origin_x)) / cw
            );
        }
    }

    #[test]
    fn tall_and_underline_price_light_alike_and_differ_only_in_reach_above_the_spine() {
        let t0 = Instant::now();
        let mut peaks = Vec::new();
        let mut tops = Vec::new();
        for c in [cfg(true, true), cfg(true, false)] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 6, &c, 0.7);
            let cx = ctx(at(t0, 5 * 60), &c, (2, 16), 0.7);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            peaks.push(sink.under.iter().map(|q| q.alpha).max().unwrap_or(0));
            tops.push(sink.under.iter().map(|q| q.y).min().unwrap_or(0));
        }
        assert_eq!(
            peaks[0], peaks[1],
            "the tall and underline spellings must price light identically"
        );
        assert!(
            tops[0] < tops[1],
            "the tall body's top edge must sit above the underline's ({tops:?})"
        );
    }

    #[test]
    fn the_light_bed_keeps_theme_ink_over_it_at_the_bar() {
        fn contrast(a: u32, b: u32) -> f32 {
            let (x, y) = (relative_luminance(a), relative_luminance(b));
            let (hi, lo) = if x > y { (x, y) } else { (y, x) };
            (hi + 0.05) / (lo + 0.05)
        }
        let blend_at = |c: &Config, ink: u32| {
            let a = (cov_cap(c) + 16.0).min(255.0) as u8;
            over_premul(c.theme_bg, premul_rgb(ink, a), a) & 0x00FF_FFFF
        };
        let mut themes = vec![(0x001F_2328, 0x00FF_FFFF)];
        for bg in [0x00FF_FFFF, 0x00FD_F6E3] {
            for grey in (0..=0x33u32).step_by(0x11) {
                themes.push(((grey << 16) | (grey << 8) | grey, bg));
            }
        }
        for (fg, bg) in themes {
            let mut c = cfg(false, true);
            c.theme_fg = fg;
            c.theme_bg = bg;
            let mut lut = BedInkLut::default();
            lut.sync(&c);
            for &ink in &lut.lut {
                let blend = blend_at(&c, ink);
                assert!(
                    contrast(fg, blend) >= BODY_CONTRAST_BAR,
                    "{ink:06x} under {fg:06x} on {bg:06x}: {:.2}:1",
                    contrast(fg, blend)
                );
            }
        }
        let mut sol = cfg(false, true);
        sol.theme_fg = 0x0065_7B83;
        sol.theme_bg = 0x00FD_F6E3;
        let mut lut = BedInkLut::default();
        lut.sync(&sol);
        for &ink in &lut.lut {
            assert!(seen(blend_at(&sol, ink), sol.theme_bg), "{ink:06x} reads");
        }
        let old = blend_at(&cfg(false, true), InkRole::OverText.ink(0x0000_00FF));
        assert!(contrast(0x001F_2328, old) < 3.0, "the old table's blue");
    }

    #[test]
    fn a_light_theme_ribbon_is_source_over_ink_and_never_additive() {
        let c = cfg(false, false);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        let now = at(t0, 5 * 60);
        let cx = ctx(now, &c, (2, 16), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(!sink.under.is_empty(), "the light rail still draws");
        assert!(
            sink.out.is_empty(),
            "no additive light on white — L6, and the hot edge is dark-only"
        );
        let cap = InkRole::Leading.alpha_cap() as u8;
        for q in &sink.under {
            assert!(q.alpha > 0, "every light-theme quad composites SOURCE-OVER");
            assert!(q.alpha <= cap, "light alpha {} over the {cap} cap", q.alpha);
        }
    }

    fn composite_at(under: &[GlowQuad], x: f32, y: f32) -> u32 {
        use aterm_render::add_sat;
        let (px, py) = (x.floor(), y.floor());
        under
            .iter()
            .filter(|q| {
                let qx = f32::from(q.x);
                (qx..qx + f32::from(q.w)).contains(&px) && f32::from(q.y) == py
            })
            .fold(DEFAULT_BG, |acc, q| {
                if q.alpha == 0 {
                    add_sat(acc, q.color)
                } else {
                    over_premul(acc, q.color, q.alpha)
                }
            })
    }

    #[test]
    fn the_glyph_box_stays_at_the_bar_over_the_emitted_rail() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let g = geom();
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        let row_top = f32::from(g.origin_y) + 2.0 * ch;
        let row_bottom = row_top + ch;
        let mut worst = f32::INFINITY;
        let mut worst_at = (0.0, 0.0);
        let mut x = f32::from(g.origin_x) + 10.0 * cw;
        while x < f32::from(g.origin_x) + 18.0 * cw {
            let mut y = row_top - 0.2 * ch;
            while y < row_bottom {
                let lit = composite_at(&sink.under, x, y);
                let k = contrast(DEFAULT_FG, lit);
                if k < worst {
                    worst = k;
                    worst_at = (x, y - row_bottom);
                }
                y += 1.0;
            }
            x += 1.0;
        }
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the text composites at {worst:.3}:1 at (x {}, {:.0} px above the row bottom); the bar is {BODY_CONTRAST_BAR}:1 over every row a letter can touch",
            worst_at.0,
            -worst_at.1
        );
    }

    /// The vivid rail stands a hairline below the row ([`RAIL_GAP_CH`]):
    /// the first pixels under the row's bottom, where a descender's foot
    /// meets the ground next to it, keep the text at the body's bar at every
    /// phase of the wave, and the rail is still lit below them. Negative
    /// control: the rail laid at the row's bottom (`rail_flush`) takes those
    /// pixels under the bar.
    #[test]
    fn the_rail_leaves_the_descenders_a_hairline_of_body() {
        let c = cfg(true, true);
        let g = geom();
        let ch = g.ch as f32;
        let row_bottom = f32::from(g.origin_y) + 3.0 * ch;
        let gap = rail_gap_px(ch);
        assert!(gap >= 1.0, "premise: a hairline of at least a pixel");
        for flush in [false, true] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.rail_flush = flush;
            type_run(&mut rib, t0, 10, 8, &c, 0.9);
            let (mut worst, mut rail_lit) = (f64::INFINITY, false);
            for ph in 0..8u16 {
                let mut cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
                cx.phase = f32::from(ph) / 8.0;
                rib.plan(&cx);
                let mut sink = Sink::default();
                {
                    let mut f = sink.frame();
                    rib.emit(&cx, &mut f);
                }
                let mut x = f32::from(g.origin_x) + 10.0 * g.cw as f32;
                while x < f32::from(g.origin_x) + 18.0 * g.cw as f32 {
                    for d in 0..gap as i32 {
                        let lit = composite_at(&sink.under, x, row_bottom + d as f32);
                        worst = worst.min(f64::from(contrast(DEFAULT_FG, lit)));
                    }
                    for d in gap as i32..(gap as i32 + 4) {
                        let lit = composite_at(&sink.under, x, row_bottom + d as f32);
                        rail_lit |= contrast(DEFAULT_FG, lit) < 3.0;
                    }
                    x += 1.0;
                }
            }
            if flush {
                assert!(
                    worst < f64::from(BODY_CONTRAST_BAR),
                    "negative control: the flush rail meets the row's bottom ({worst:.2}:1)"
                );
            } else {
                assert!(
                    worst >= f64::from(BODY_CONTRAST_BAR),
                    "the hairline under the row reads the text at {worst:.2}:1, under the \
                     body's {BODY_CONTRAST_BAR}:1"
                );
                assert!(rail_lit, "the rail is still lit below the hairline");
            }
        }
    }

    /// Every stop of the walk on a dim dark theme keeps a letter's edge pixel
    /// (the judge's solid share of the text's reading) at four fifths of the
    /// theme's own contrast over each channel's larger of page and bed, and a
    /// theme past the dim bar keeps its bed byte for byte. Negative control:
    /// the bed unheld fails that edge on Solarized Dark.
    #[test]
    fn a_dim_theme_s_bed_keeps_the_letters_at_its_edge() {
        let chmax = |a: u32, b: u32| {
            let ch = |sh: u32| ((a >> sh) & 0xff).max((b >> sh) & 0xff) << sh;
            ch(16) | ch(8) | ch(0)
        };
        // The text's weakest solid pixel: on the way from the ink to the
        // page, reading the judge's solid share of the text's own.
        let edge_ink = |fg: u32, page: u32| {
            let want = DIM_EDGE_SOLID * contrast(fg, page);
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..24 {
                let mid = 0.5 * (lo + hi);
                if contrast(lerp_rgb(fg, page, mid), page) >= want {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lerp_rgb(fg, page, lo)
        };
        let walk = |fg: u32, held: bool| -> Vec<u32> {
            (0..64)
                .map(|i| {
                    let arc = walk_arc_at(i * BED_INK_LUT_LEN / 64);
                    let bed = bed_ink(arc, bed_luma_budget(fg));
                    if held {
                        dim_edge_held(bed, fg, SOLARIZED_PAGE)
                    } else {
                        bed
                    }
                })
                .collect()
        };
        const SOLARIZED_PAGE: u32 = 0x0000_2B36;
        for (fg, page) in [(0x0083_9496, SOLARIZED_PAGE), (0x0085_8585, 0x0011_1318)] {
            let bar = 0.8 * contrast(fg, page);
            let ink = edge_ink(fg, page);
            for i in 0..64 {
                let arc = walk_arc_at(i * BED_INK_LUT_LEN / 64);
                let bed = dim_edge_held(bed_ink(arc, bed_luma_budget(fg)), fg, page);
                let read = contrast(ink, chmax(bed, page));
                assert!(
                    read >= bar,
                    "fg {fg:06x} on {page:06x}: stop {i} bed {bed:06x} reads the edge at {read:.2}:1, under {bar:.2}"
                );
            }
        }
        let fg = 0x00D0_D0D0;
        for i in 0..64 {
            let arc = walk_arc_at(i * BED_INK_LUT_LEN / 64);
            let bed = bed_ink(arc, bed_luma_budget(fg));
            assert_eq!(
                dim_edge_held(bed, fg, 0x001E_1E1E),
                bed,
                "a bright theme's bed is its own"
            );
        }
        let (fg, page) = (0x0083_9496, SOLARIZED_PAGE);
        let ink = edge_ink(fg, page);
        let unheld = walk(fg, false)
            .into_iter()
            .map(|bed| contrast(ink, chmax(bed, page)))
            .fold(f32::INFINITY, f32::min);
        assert!(
            unheld < 0.8 * contrast(fg, page),
            "control: the unheld bed keeps the edge ({unheld:.2}:1), so the hold proves nothing"
        );
    }

    #[test]
    fn on_paper_the_pet_s_ribbon_and_the_flat_one_read_apart() {
        // On paper the pet's ribbon keeps its under-edge (ink below the row's
        // bottom past the hairline) and the flat one has none and a lower
        // reach; drawn alike (the negative control) the two columns match.
        let g = geom();
        let ch = g.ch as f32;
        let row_top = f32::from(g.origin_y) + 2.0 * ch;
        let row_bottom = row_top + ch;
        let gap = rail_gap_px(ch);
        let profile = |c: &Config, alike: bool| -> (u32, f32) {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.paper_alike = alike;
            type_run(&mut rib, t0, 10, 8, c, 0.9);
            let cx = ctx(at(t0, 8 * 60), c, (2, 18), 0.9);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            // The ink laid on the white page at `(x, y)`: its deepest
            // channel's fall from the page.
            let ink = |x: f32, y: f32| {
                let (px, py) = (x.floor(), y.floor());
                let p = sink
                    .under
                    .iter()
                    .filter(|q| {
                        let qx = f32::from(q.x);
                        (qx..qx + f32::from(q.w)).contains(&px) && f32::from(q.y) == py
                    })
                    .fold(0x00FF_FFFF, |acc, q| over_premul(acc, q.color, q.alpha));
                [16u32, 8, 0]
                    .iter()
                    .map(|&sh| 255 - ((p >> sh) & 0xff))
                    .max()
                    .unwrap_or(0)
            };
            let (mut under, mut top) = (0u32, row_bottom);
            let mut x = f32::from(g.origin_x) + 12.0 * g.cw as f32;
            while x < f32::from(g.origin_x) + 17.0 * g.cw as f32 {
                for d in gap as i32..(gap as i32 + 4) {
                    under += ink(x, row_bottom + d as f32);
                }
                if let Some(y) = (0..(2.0 * ch) as i32)
                    .map(|k| row_bottom - 2.0 * ch + k as f32)
                    .find(|&y| ink(x, y) > 24)
                {
                    top = top.min(y);
                }
                x += 1.0;
            }
            (under, top)
        };
        let (pet_under, pet_top) = profile(&cfg(false, true), false);
        let (flat_under, flat_top) = profile(&flat(false, true), false);
        assert!(
            5 * pet_under >= 8 * flat_under.max(1),
            "the pet's under-edge on paper ({pet_under}) against the flat one's ({flat_under})"
        );
        assert!(
            flat_top >= pet_top + 3.0,
            "the flat ribbon reaches as high as the pet's ({flat_top} vs {pet_top})"
        );
        let (alike_pet, alike_pet_top) = profile(&cfg(false, true), true);
        let (alike_flat, alike_flat_top) = profile(&flat(false, true), true);
        assert!(
            5 * alike_pet < 8 * alike_flat.max(1) && (alike_flat_top - alike_pet_top).abs() < 3.0,
            "control: drawn alike, the two still read apart ({alike_pet} vs {alike_flat}, \
             tops {alike_pet_top} vs {alike_flat_top})"
        );
    }

    #[test]
    fn the_flat_spelling_draws_no_rail() {
        let c = flat(true, true);
        assert!(!Ribbon::comet(&c) && !Ribbon::rail_lit(&c));
        assert!(Ribbon::comet(&cfg(true, true)) && Ribbon::rail_lit(&cfg(true, true)));
        // The pet's rail is lit on paper too, so L6's bed alone below the
        // baseline holds for the flat spelling only.
        assert!(
            Ribbon::rail_lit(&cfg(false, true)) && !Ribbon::rail_lit(&flat(false, true)),
            "on paper the pet keeps its under-edge and the flat spelling none"
        );
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let g = geom();
        let ch = g.ch as f32;
        let row_bottom = f32::from(g.origin_y) + 3.0 * ch;
        for q in &sink.under {
            assert!(
                f32::from(q.y) < row_bottom + DN_TOP_CH * ch + 1.0,
                "the flat body reaches no deeper than {DN_TOP_CH} ch (y {} vs row bottom {row_bottom})",
                q.y
            );
            let lit = over_premul(DEFAULT_BG, q.color, q.alpha);
            assert!(
                contrast(DEFAULT_FG, lit) >= BODY_CONTRAST_BAR,
                "the flat body is the bed everywhere: #{lit:06X} at y {}",
                q.y
            );
        }
    }

    /// A row of light whose next line holds ink stays inside its own row: no
    /// body, core, wave crest or rail lies on the next line's ascenders, at any
    /// phase of the wave, dark or light, tall, short or flat. The band is still
    /// drawn in its row. Negative controls: the same typing over a row probed
    /// blank, and over no probe at all, reaches below the row's bottom.
    #[test]
    fn a_row_with_ink_below_keeps_its_light_inside_the_row() {
        let g = geom();
        let ch = g.ch as f32;
        let row_top = f32::from(g.origin_y) + 2.0 * ch;
        let row_bottom = row_top + ch;
        let mut inked = GlyphProbe::new();
        inked.probe_row(3, &[[true; 30], [false; 30]].concat());
        let mut blank = GlyphProbe::new();
        blank.probe_row(3, &[false; 120]);
        // Ink only far from the run (cols 40..) holds nothing either.
        let mut aside = GlyphProbe::new();
        aside.probe_row(3, &[[false; 40], [true; 40]].concat());
        for (c, name) in [
            (cfg(true, true), "tall dark"),
            (cfg(true, false), "short dark"),
            (cfg(false, true), "tall light"),
            (flat(true, true), "flat dark"),
        ] {
            for (probe, held, what) in [
                (Some(&inked), true, "ink below"),
                (Some(&blank), false, "blank below"),
                (Some(&aside), false, "ink below, away from the run"),
                (None, false, "no probe"),
            ] {
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                type_run(&mut rib, t0, 10, 8, &c, 0.9);
                // The line was typed with no probe: told of the row below, it
                // settles over the ease, judged once that is spent.
                rib.plan_over(&ctx(at(t0, 8 * 60), &c, (2, 18), 0.9), probe);
                let judged_at = at(t0, 8 * 60 + (HOLD_EASE_S * 1000.0) as u64 + 16);
                let mut deepest = f32::NEG_INFINITY;
                let mut in_row = 0usize;
                for step in 0..8u16 {
                    let mut cx = ctx(judged_at, &c, (2, 18), 0.9);
                    cx.phase = f32::from(step) / 8.0;
                    rib.plan_over(&cx, probe);
                    let mut sink = Sink::default();
                    {
                        let mut f = sink.frame();
                        rib.emit(&cx, &mut f);
                    }
                    for q in sink.under.iter().filter(|q| q.color != 0) {
                        let bottom = f32::from(q.y) + f32::from(q.h);
                        deepest = deepest.max(bottom);
                        in_row += usize::from(f32::from(q.y) >= row_top && bottom <= row_bottom);
                    }
                    // Lifted into its row, the body keeps the typed letters at the bar.
                    if held && c.dark_theme {
                        let cw = g.cw as f32;
                        let mut x = f32::from(g.origin_x) + 10.0 * cw;
                        while x < f32::from(g.origin_x) + 18.0 * cw {
                            let mut y = row_top;
                            while y < row_bottom {
                                let lit = composite_at(&sink.under, x, y);
                                let k = contrast(DEFAULT_FG, lit);
                                assert!(
                                    k >= BODY_CONTRAST_BAR,
                                    "{name}, {what}: the held body takes the typed letters to \
                                     {k:.2}:1 at x {x}, {:.0} px above the row bottom",
                                    row_bottom - y
                                );
                                y += 1.0;
                            }
                            x += 1.0;
                        }
                    }
                }
                assert!(in_row > 0, "{name}, {what}: the band must still be drawn");
                if held {
                    assert!(
                        deepest <= row_bottom,
                        "{name}, {what}: light reaches {:.1} px past the row's bottom onto the \
                         next line's ascenders",
                        deepest - row_bottom
                    );
                } else {
                    assert!(
                        deepest > row_bottom + 1.0,
                        "negative control, {name}, {what}: the body's lower reach must lie \
                         below the row (deepest {:.1} px past it)",
                        deepest - row_bottom
                    );
                }
            }
        }
    }

    /// A lit line whose next row inks under it settles into its row over
    /// [`HOLD_EASE_S`], never in one frame: the frame after the ink lands it
    /// still reaches below the row by most of its reach, and once the ease is
    /// spent nothing does. Negative control: the hold stepped to its target
    /// (`hold_steps`) is inside the row in the first frame.
    #[test]
    fn a_line_whose_next_row_inks_settles_into_its_row_over_the_ease() {
        let g = geom();
        let row_bottom = f32::from(g.origin_y) + 3.0 * g.ch as f32;
        let mut inked = GlyphProbe::new();
        inked.probe_row(3, &[[true; 30], [false; 30]].concat());
        let mut blank = GlyphProbe::new();
        blank.probe_row(3, &[false; 120]);
        let c = cfg(true, true);
        let deepest = |rib: &mut Ribbon, now: Instant, probe: &GlyphProbe| {
            let cx = ctx(now, &c, (2, 18), 0.9);
            rib.plan_over(&cx, Some(probe));
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            sink.under
                .iter()
                .filter(|q| q.color != 0)
                .map(|q| f32::from(q.y) + f32::from(q.h))
                .fold(f32::NEG_INFINITY, f32::max)
                - row_bottom
        };
        for steps in [false, true] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.hold_steps = steps;
            type_run(&mut rib, t0, 10, 8, &c, 0.9);
            let lit = at(t0, 8 * 60);
            let free = deepest(&mut rib, lit, &blank);
            assert!(
                free > 2.0,
                "premise: a free line reaches {free:.1} px below"
            );
            let first = deepest(&mut rib, at(t0, 8 * 60 + 16), &inked);
            let settled = deepest(
                &mut rib,
                at(t0, 8 * 60 + 16 + (HOLD_EASE_S * 1000.0) as u64 + 16),
                &inked,
            );
            assert!(
                settled.is_finite(),
                "steps {steps}: the line is still lit once the ease is spent"
            );
            assert!(
                settled <= 0.0,
                "steps {steps}: once the ease is spent nothing reaches below the row \
                 ({settled:.1} px)"
            );
            if steps {
                assert!(
                    first <= 0.0,
                    "negative control: a stepped hold is in its row in one frame ({first:.1} px)"
                );
            } else {
                assert!(
                    first > 0.5 * free,
                    "the frame after the ink lands the line still reaches {first:.1} of its \
                     {free:.1} px below: it eases in, not in one frame"
                );
            }
        }
        // A full reset ends the old row's custody. A new line at that row
        // starts free, whether the row below is blank or not yet known.
        for probe in [Some(&blank), None] {
            let t0 = Instant::now();
            let mut reset = Ribbon::new();
            type_run(&mut reset, t0, 10, 8, &c, 0.9);
            let held_at = at(t0, 8 * 60 + 16);
            deepest(&mut reset, held_at, &inked);
            let settled_at = at(held_at, (HOLD_EASE_S * 1000.0) as u64 + 16);
            assert!(deepest(&mut reset, settled_at, &inked) <= 0.0);
            reset.reset();

            let next = at(settled_at, 16);
            let mut fresh = Ribbon::new();
            for rib in [&mut reset, &mut fresh] {
                type_run(rib, next, 10, 8, &c, 0.9);
            }
            let cx = ctx(at(next, 8 * 60), &c, (2, 18), 0.9);
            let draw = |rib: &mut Ribbon| {
                rib.plan_over(&cx, probe);
                let mut sink = Sink::default();
                rib.emit(&cx, &mut sink.frame());
                sink.under
            };
            let (got, want) = (draw(&mut reset), draw(&mut fresh));
            assert!(
                want.iter()
                    .any(|q| f32::from(q.y) + f32::from(q.h) > row_bottom + 2.0),
                "the fresh line still reaches below its row"
            );
            assert_eq!(
                got, want,
                "a reset must not lend the new line an old row's hold"
            );
        }
    }

    /// Typed on the pane's last row and carried up two rows by a scroll, a
    /// line lays no light past its row's bottom, on the scroll's frame (the
    /// probe cleared, knowing nothing below) and on every frame after (the
    /// probe knowing the printed line). Negative control: the hold left
    /// behind by the scroll (`holds_left_behind`) lays the body's lower reach
    /// on the printed line in the scroll's frame.
    #[test]
    fn a_line_a_scroll_carries_up_keeps_its_rows_shape() {
        let g = Geom {
            rows: 14,
            win_h: 14 * 18,
            ..geom()
        };
        let ch = g.ch as f32;
        let c = cfg(true, true);
        let bottom = 13u16;
        let echo = bottom - 2;
        let echo_bottom = f32::from(g.origin_y) + f32::from(echo + 1) * ch;
        let mut printed = GlyphProbe::new();
        printed.probe_row(i32::from(echo) + 1, &[[true; 30], [false; 30]].concat());
        let deepest = |rib: &mut Ribbon, now: Instant, probe: Option<&GlyphProbe>| {
            let cx = ctx_in(now, &c, (bottom, 2), 0.9, g);
            rib.plan_over(&cx, probe);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            sink.under
                .iter()
                .filter(|q| q.color != 0)
                .map(|q| f32::from(q.y) + f32::from(q.h))
                .fold(f32::NEG_INFINITY, f32::max)
                - echo_bottom
        };
        for left_behind in [false, true] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.holds_left_behind = left_behind;
            let keys = Keys {
                g,
                row: bottom,
                col0: 2,
                n: 12,
                period_ms: 60,
                disp: 0.9,
            };
            type_keys(&mut rib, t0, keys, &c);
            rib.translate_scroll(2);
            let scrolled = deepest(&mut rib, at(t0, 12 * 60), None);
            assert!(
                scrolled.is_finite(),
                "left behind {left_behind}: the carried line is still lit"
            );
            if left_behind {
                assert!(
                    scrolled > 2.0,
                    "negative control: with the hold left behind the scroll's frame reaches \
                     only {scrolled:.1} px onto the printed line"
                );
                continue;
            }
            assert!(
                scrolled <= 0.0,
                "the scroll's frame lays light {scrolled:.1} px onto the printed line"
            );
            for ms in (16..=320).step_by(16) {
                let later = deepest(&mut rib, at(t0, 12 * 60 + ms), Some(&printed));
                assert!(
                    later <= 0.0,
                    "+{ms} ms: light reaches {later:.1} px onto the printed line"
                );
            }
        }
    }

    /// A content-proven repaint carries the line's held shape just as a VT scroll does.
    /// The destination is outside the caret probe, so unknown glyphs must retain its
    /// previous hold rather than make the carried line free in the repaint's first frame.
    #[test]
    fn a_proven_repaint_carries_a_held_rows_shape_through_unknown_glyphs() {
        let g = Geom {
            rows: 14,
            win_h: 14 * 18,
            ..geom()
        };
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 12,
                period_ms: 60,
                disp: 0.9,
            },
            &c,
        );
        let cohort = rib
            .cohorts()
            .iter()
            .find(|coh| coh.row == 13)
            .expect("the typed line has a cohort")
            .id;
        let carry = at(t0, 12 * 60);
        let mut moved = Vec::new();
        assert_eq!(
            rib.translate_run(
                super::super::witness::FollowRun {
                    row: 13,
                    cohort,
                    lo: 2,
                    hi: 13,
                    dr: -2,
                },
                carry,
                true,
                &mut moved,
            ),
            12,
            "the production witness carry moved the complete typed line"
        );
        let row_bottom = f32::from(g.origin_y) + 12.0 * g.ch as f32;
        let cx = ctx_in(carry, &c, (13, 2), 0.9, g);
        rib.plan_over(&cx, None);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        let deepest = sink
            .under
            .iter()
            .filter(|q| q.color != 0)
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(deepest.is_finite(), "the carried line is still lit");
        assert!(
            deepest <= row_bottom,
            "the proven repaint loses the held shape: light jumps {:.1} px below its row",
            deepest - row_bottom
        );
    }

    /// A partial repaint leaves both halves held, and a free run joining the held
    /// destination does not lower that row's floor. The geometry at each run's own
    /// columns distinguishes both errors without reading the hold bookkeeping.
    #[test]
    fn a_partial_repaint_keeps_both_halves_and_a_stronger_destination_held() {
        let g = Geom {
            rows: 14,
            win_h: 14 * 18,
            ..geom()
        };
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 11,
                col0: 2,
                n: 12,
                period_ms: 60,
                disp: 0.9,
            },
            &c,
        );
        let mut inked = GlyphProbe::new();
        inked.probe_row(12, &[true; 120]);
        let start = at(t0, 12 * 60);
        rib.plan_over(&ctx_in(start, &c, (11, 14), 0.9, g), Some(&inked));
        let carry = at(start, (HOLD_EASE_S * 1000.0) as u64 + 16);
        rib.plan_over(&ctx_in(carry, &c, (11, 14), 0.9, g), Some(&inked));
        let depth = |rib: &mut Ribbon, now: Instant, row: u16, col: u16| {
            let cx = ctx_in(now, &c, (13, 2), 0.9, g);
            rib.plan_over(&cx, None);
            let mut sink = Sink::default();
            rib.emit(&cx, &mut sink.frame());
            let x = f32::from(g.origin_x) + (f32::from(col) + 0.5) * g.cw as f32;
            sink.under
                .iter()
                .filter(|q| {
                    q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w)
                })
                .map(|q| f32::from(q.y) + f32::from(q.h))
                .fold(f32::NEG_INFINITY, f32::max)
                - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
        };
        let cohort = rib
            .cohorts()
            .iter()
            .find(|coh| coh.row == 11)
            .expect("the held line")
            .id;
        let mut moved = Vec::new();
        assert_eq!(
            rib.translate_run(
                super::super::witness::FollowRun {
                    row: 11,
                    cohort,
                    lo: 8,
                    hi: 13,
                    dr: -2,
                },
                carry,
                true,
                &mut moved,
            ),
            6
        );
        for (row, col) in [(11, 4), (9, 10)] {
            let below = depth(&mut rib, carry, row, col);
            assert!(below.is_finite(), "the partial run on row {row} stays lit");
            assert!(
                below <= 0.0,
                "row {row}'s partial run lost its held shape: {below:.1} px"
            );
        }

        rib.on_event(
            &nav((11, 14), (7, 20)),
            carry,
            &ctx_in(carry, &c, (7, 20), 0.9, g),
        );
        type_keys(
            &mut rib,
            carry,
            Keys {
                g,
                row: 7,
                col0: 20,
                n: 4,
                period_ms: 60,
                disp: 0.9,
            },
            &c,
        );
        let join = at(carry, 4 * 60);
        let free_depth = depth(&mut rib, join, 7, 22);
        assert!(
            free_depth > 2.0,
            "control: the joining run is free ({free_depth:.1} px)"
        );
        let cohort = rib
            .cohorts()
            .iter()
            .find(|coh| coh.row == 7)
            .expect("the free joining run")
            .id;
        assert_eq!(
            rib.translate_run(
                super::super::witness::FollowRun {
                    row: 7,
                    cohort,
                    lo: 20,
                    hi: 23,
                    dr: 2,
                },
                join,
                true,
                &mut moved,
            ),
            4
        );
        let below = depth(&mut rib, join, 9, 22);
        assert!(below.is_finite(), "the joining run stays lit");
        assert!(
            below <= 0.0,
            "the joining run weakens the destination's hold: {below:.1} px"
        );
    }

    /// A repaint batch may move a held line onto a free one's old row before
    /// moving that free line away. Each run carries its own previous shape,
    /// including another hop of the first run in the same batch.
    #[test]
    fn a_repaint_batch_keeps_each_runs_previous_shape_across_multiple_hops() {
        let g = Geom {
            rows: 14,
            win_h: 14 * 18,
            ..geom()
        };
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 7,
                col0: 2,
                n: 8,
                period_ms: 20,
                disp: 0.9,
            },
            &c,
        );
        let mut inked = GlyphProbe::new();
        inked.probe_row(8, &[true; 120]);
        let start = at(t0, 8 * 20);
        rib.plan_over(&ctx_in(start, &c, (7, 10), 0.9, g), Some(&inked));
        let next = at(start, (HOLD_EASE_S * 1000.0) as u64 + 16);
        rib.plan_over(&ctx_in(next, &c, (7, 10), 0.9, g), Some(&inked));
        // A pasted line leaves the first flowing; a navigation jump would
        // abandon it before the second line finished typing.
        rib.on_event(
            &Event::Move {
                from: (7, 10),
                to: (8, 20),
                licence: Licence::Insert,
                dir: Dir::Right,
            },
            next,
            &ctx_in(next, &c, (8, 20), 0.9, g),
        );
        type_keys(
            &mut rib,
            next,
            Keys {
                g,
                row: 8,
                col0: 20,
                n: 8,
                period_ms: 20,
                disp: 0.9,
            },
            &c,
        );
        let carry = at(next, 8 * 20);
        let depth = |rib: &mut Ribbon, row: u16, col: u16| {
            let cx = ctx_in(carry, &c, (13, 2), 0.9, g);
            rib.plan_over(&cx, None);
            let mut sink = Sink::default();
            rib.emit(&cx, &mut sink.frame());
            let x = f32::from(g.origin_x) + (f32::from(col) + 0.5) * g.cw as f32;
            sink.under
                .iter()
                .filter(|q| {
                    q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w)
                })
                .map(|q| f32::from(q.y) + f32::from(q.h))
                .fold(f32::NEG_INFINITY, f32::max)
                - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
        };
        let held_depth = depth(&mut rib, 7, 5);
        let free_depth = depth(&mut rib, 8, 24);
        assert!(
            held_depth.is_finite() && held_depth <= 0.0,
            "the first run is held ({held_depth:.1} px)"
        );
        assert!(
            free_depth > 2.0,
            "the other run is free ({free_depth:.1} px)"
        );
        let held = rib
            .cohorts()
            .iter()
            .find(|coh| coh.row == 7)
            .expect("held run")
            .id;
        let free = rib
            .cohorts()
            .iter()
            .find(|coh| coh.row == 8)
            .expect("free run")
            .id;
        let mut moved = Vec::new();
        for (row, cohort, lo, hi, dr) in
            [(7, held, 2, 9, 1), (8, held, 2, 9, 2), (8, free, 20, 27, 3)]
        {
            assert_eq!(
                rib.translate_run(
                    super::super::witness::FollowRun {
                        row,
                        cohort,
                        lo,
                        hi,
                        dr
                    },
                    carry,
                    true,
                    &mut moved
                ),
                8
            );
        }
        let held_depth = depth(&mut rib, 10, 5);
        let free_depth = depth(&mut rib, 11, 24);
        assert!(
            held_depth.is_finite() && held_depth <= 0.0,
            "a second hop lost the held run's shape ({held_depth:.1} px)"
        );
        assert!(
            free_depth > 2.0,
            "the later run inherited another run's hold ({free_depth:.1} px)"
        );
    }

    fn repaint_column_depth(
        rib: &mut Ribbon,
        c: &Config,
        g: Geom,
        now: Instant,
        row: u16,
        col: u16,
    ) -> f32 {
        let cx = ctx_in(now, c, (13, 2), 0.9, g);
        rib.plan_over(&cx, None);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        let x = f32::from(g.origin_x) + (f32::from(col) + 0.5) * g.cw as f32;
        sink.under
            .iter()
            .filter(|q| q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w))
            .map(|q| f32::from(q.y) + f32::from(q.h))
            .fold(f32::NEG_INFINITY, f32::max)
            - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
    }

    /// Adjacent, content-proven runs can heal into the older cohort before
    /// painting. The removed younger identity's held shape must survive.
    #[test]
    fn a_repaint_join_keeps_the_held_shape_when_the_older_free_cohort_survives() {
        let g = Geom {
            rows: 14,
            win_h: 14 * 18,
            ..geom()
        };
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 7,
                col0: 2,
                n: 8,
                period_ms: 20,
                disp: 0.9,
            },
            &c,
        );
        let next = at(t0, 8 * 20);
        rib.on_event(
            &Event::Move {
                from: (7, 10),
                to: (8, 10),
                licence: Licence::Insert,
                dir: Dir::Right,
            },
            next,
            &ctx_in(next, &c, (8, 10), 0.9, g),
        );
        type_keys(
            &mut rib,
            next,
            Keys {
                g,
                row: 8,
                col0: 10,
                n: 8,
                period_ms: 20,
                disp: 0.9,
            },
            &c,
        );
        let mut inked = GlyphProbe::new();
        inked.probe_row(9, &[true; 120]);
        let start = at(next, 8 * 20);
        rib.plan_over(&ctx_in(start, &c, (8, 18), 0.9, g), Some(&inked));
        let carry = at(start, (HOLD_EASE_S * 1000.0) as u64 + 16);
        rib.plan_over(&ctx_in(carry, &c, (8, 18), 0.9, g), Some(&inked));
        let free_depth = repaint_column_depth(&mut rib, &c, g, carry, 7, 4);
        let held_depth = repaint_column_depth(&mut rib, &c, g, carry, 8, 14);
        assert!(
            free_depth > 2.0,
            "the older run is free ({free_depth:.1} px)"
        );
        assert!(
            held_depth.is_finite() && held_depth <= 0.0,
            "the newer run is held ({held_depth:.1} px)"
        );
        let free = rib
            .cohorts()
            .iter()
            .find(|coh| coh.row == 7)
            .expect("free run")
            .id;
        let held = rib
            .cohorts()
            .iter()
            .find(|coh| coh.row == 8)
            .expect("held run")
            .id;
        let mut moved = Vec::new();
        for (row, cohort, lo, hi, dr) in [(7, free, 2, 9, 2), (8, held, 10, 17, 1)] {
            assert_eq!(
                rib.translate_run(
                    super::super::witness::FollowRun {
                        row,
                        cohort,
                        lo,
                        hi,
                        dr
                    },
                    carry,
                    false,
                    &mut moved
                ),
                8
            );
        }
        let depth = repaint_column_depth(&mut rib, &c, g, carry, 9, 14);
        assert_eq!(
            rib.cohorts().iter().filter(|coh| coh.row == 9).count(),
            1,
            "the real seam healer merged both runs"
        );
        assert!(
            depth.is_finite() && depth <= 0.0,
            "the seam merge discarded the held shape ({depth:.1} px)"
        );
    }

    /// An empty natural-retirement frame ends the previous row's custody.
    /// A later cold run over unknown glyphs is born free, as a fresh ribbon is.
    #[test]
    fn a_repaint_rows_shape_does_not_survive_its_natural_empty_frame() {
        let g = Geom {
            rows: 14,
            win_h: 14 * 18,
            ..geom()
        };
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 11,
            col0: 2,
            n: 12,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let mut inked = GlyphProbe::new();
        inked.probe_row(12, &[true; 120]);
        let start = at(t0, 12 * 60);
        rib.plan_over(&ctx_in(start, &c, (11, 14), 0.9, g), Some(&inked));
        let held = at(start, (HOLD_EASE_S * 1000.0) as u64 + 16);
        rib.plan_over(&ctx_in(held, &c, (11, 14), 0.9, g), Some(&inked));
        let before = repaint_column_depth(&mut rib, &c, g, held, 11, 8);
        assert!(
            before.is_finite() && before <= 0.0,
            "the previous line is held ({before:.1} px)"
        );
        let empty = at(t0, 20_000);
        rib.plan_over(&ctx_in(empty, &c, (11, 14), 0.9, g), None);
        assert!(rib.cells().is_empty(), "the old line naturally retired");
        let cold = at(empty, 16);
        type_keys(&mut rib, cold, keys, &c);
        let now = at(cold, 12 * 60);
        let depth = repaint_column_depth(&mut rib, &c, g, now, 11, 8);
        let mut fresh = Ribbon::new();
        type_keys(&mut fresh, cold, keys, &c);
        let control = repaint_column_depth(&mut fresh, &c, g, now, 11, 8);
        assert!(control > 2.0, "the fresh twin is free ({control:.1} px)");
        assert!(
            (depth - control).abs() < 0.5,
            "the cold run inherited retired row state ({depth:.1} vs {control:.1} px)"
        );
    }

    /// A navigation wake can still be visible after abandonment. If its text is
    /// repainted and another navigation takes over its cells before planning,
    /// preserving their light also preserves the shape carried with that light.
    #[test]
    fn a_repainted_wake_keeps_its_shape_when_navigation_remints_its_live_cells() {
        let g = Geom {
            rows: 14,
            win_h: 14 * 18,
            ..geom()
        };
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let first = ctx_in(t0, &c, (9, 4), 0.9, g);
        let old = rib
            .wake(9, 20, 4, t0, &first, WAKE_LIFE_S, true)
            .expect("navigation lays a wake");
        let carry = at(t0, 100);
        let mut inked = GlyphProbe::new();
        inked.probe_row(10, &[true; 120]);
        let source = ctx_in(carry, &c, (9, 4), 0.9, g);
        rib.plan_over(&source, Some(&inked));
        let depth = |rib: &mut Ribbon, cx: &Ctx<'_>, row: u16| {
            let mut sink = Sink::default();
            rib.emit(cx, &mut sink.frame());
            let x = f32::from(g.origin_x) + 12.5 * g.cw as f32;
            sink.under
                .iter()
                .filter(|q| {
                    q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w)
                })
                .map(|q| f32::from(q.y) + f32::from(q.h))
                .fold(f32::NEG_INFINITY, f32::max)
                - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
        };
        let held = depth(&mut rib, &source, 9);
        assert!(
            held.is_finite() && held <= 0.0,
            "the source wake is held ({held:.1} px)"
        );
        let old_cells: Vec<_> = rib
            .cells()
            .iter()
            .filter(|cell| cell.cohort == old)
            .map(|cell| (cell.col, cell.born))
            .collect();
        assert_eq!(old_cells.len(), 17);

        rib.on_event(
            &nav((9, 4), (5, 0)),
            carry,
            &ctx_in(carry, &c, (5, 0), 0.9, g),
        );
        assert!(
            rib.cohorts()
                .iter()
                .any(|coh| coh.id == old && coh.abandoned)
        );
        assert!(
            rib.cells()
                .iter()
                .filter(|cell| cell.cohort == old)
                .all(|cell| !cell.leaving())
        );
        let mut moved = Vec::new();
        assert_eq!(
            rib.translate_run(
                super::super::witness::FollowRun {
                    row: 9,
                    cohort: old,
                    lo: 4,
                    hi: 20,
                    dr: -2,
                },
                carry,
                true,
                &mut moved,
            ),
            17
        );

        let target = ctx_in(carry, &c, (7, 4), 0.9, g);
        rib.on_event(&nav((7, 20), (7, 4)), carry, &target);
        let adopted: Vec<_> = rib.cells().iter().filter(|cell| cell.row == 7).collect();
        assert_eq!(adopted.len(), 17);
        assert!(
            adopted
                .iter()
                .all(|cell| { cell.cohort != old && old_cells.contains(&(cell.col, cell.born)) }),
            "the fresh wake must actually adopt the old visual cells"
        );
        rib.plan_over(&target, None);
        let after = depth(&mut rib, &target, 7);
        assert!(after.is_finite(), "the adopted wake is still visible");
        assert!(
            after <= 0.0,
            "navigation remint lost the repainted wake's shape ({after:.1} px)"
        );
    }

    #[test]
    fn a_reminted_free_wake_does_not_borrow_the_held_cohort_that_moved_out() {
        let g = Geom {
            rows: 14,
            win_h: 14 * 18,
            ..geom()
        };
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let first = ctx_in(t0, &c, (7, 4), 0.9, g);
        let free = rib
            .wake(7, 20, 4, t0, &first, WAKE_LIFE_S, true)
            .expect("the free source wake");
        let lay = at(t0, 100);
        rib.plan_over(&ctx_in(lay, &c, (7, 4), 0.9, g), None);
        rib.relay_word(9, 40, 46, lay, None);
        let held = rib
            .cohorts()
            .iter()
            .find(|coh| coh.row == 9 && !coh.wake)
            .expect("the held destination's original cohort")
            .id;
        let carry = at(t0, 160);
        let mut inked = GlyphProbe::new();
        inked.probe_row(10, &[true; 120]);
        let source = ctx_in(carry, &c, (9, 46), 0.9, g);
        rib.plan_over(&source, Some(&inked));
        let depth = |rib: &mut Ribbon, cx: &Ctx<'_>, row: u16, col: u16| {
            let mut sink = Sink::default();
            rib.emit(cx, &mut sink.frame());
            let x = f32::from(g.origin_x) + (f32::from(col) + 0.5) * g.cw as f32;
            sink.under
                .iter()
                .filter(|q| {
                    q.color != 0 && f32::from(q.x) <= x && x < f32::from(q.x) + f32::from(q.w)
                })
                .map(|q| f32::from(q.y) + f32::from(q.h))
                .fold(f32::NEG_INFINITY, f32::max)
                - (f32::from(g.origin_y) + f32::from(row + 1) * g.ch as f32)
        };
        let free_before = depth(&mut rib, &source, 7, 12);
        let held_before = depth(&mut rib, &source, 9, 42);
        assert!(
            free_before > 2.0,
            "the source wake is free ({free_before:.1} px)"
        );
        assert!(
            held_before.is_finite() && held_before <= 0.0,
            "the destination's original cohort is held ({held_before:.1} px)"
        );
        let old_cells: Vec<_> = rib
            .cells()
            .iter()
            .filter(|cell| cell.cohort == free)
            .map(|cell| (cell.col, cell.born))
            .collect();
        assert_eq!(old_cells.len(), 17);
        rib.on_event(
            &nav((7, 4), (5, 0)),
            carry,
            &ctx_in(carry, &c, (5, 0), 0.9, g),
        );
        assert!(
            rib.cohorts()
                .iter()
                .any(|coh| coh.id == free && coh.abandoned)
        );
        let mut moved = Vec::new();
        for (row, cohort, lo, hi, dr, expected) in
            [(9, held, 40, 45, 2, 6), (7, free, 4, 20, 2, 17)]
        {
            assert_eq!(
                rib.translate_run(
                    super::super::witness::FollowRun {
                        row,
                        cohort,
                        lo,
                        hi,
                        dr,
                    },
                    carry,
                    true,
                    &mut moved,
                ),
                expected
            );
        }
        assert!(
            rib.cells()
                .iter()
                .all(|cell| cell.cohort != held || cell.row == 11)
        );
        let target = ctx_in(carry, &c, (9, 4), 0.9, g);
        rib.on_event(&nav((9, 20), (9, 4)), carry, &target);
        let adopted: Vec<_> = rib.cells().iter().filter(|cell| cell.row == 9).collect();
        assert_eq!(adopted.len(), 17);
        assert!(
            adopted
                .iter()
                .all(|cell| { cell.cohort != free && old_cells.contains(&(cell.col, cell.born)) }),
            "the fresh wake actually adopts the free source's visual cells"
        );
        rib.plan_over(&target, None);
        let free_after = depth(&mut rib, &target, 9, 12);
        let held_after = depth(&mut rib, &target, 11, 42);
        assert!(
            held_after.is_finite() && held_after <= 0.0,
            "the moved-out original remains held ({held_after:.1} px)"
        );
        assert!(
            free_after > 2.0,
            "the reminted free wake borrowed the departed cohort's hold ({free_after:.1} px)"
        );
    }

    #[test]
    fn the_band_stops_at_the_focused_pane_s_floor_not_the_grid_s() {
        let c = cfg(true, true);
        let g = geom();
        let ch = g.ch as f32;
        let pane_last_row = 3u16;
        let deepest = |pane_rows: Option<(u16, u16)>| -> f32 {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.set_pane_rows(pane_rows);
            type_keys(
                &mut rib,
                t0,
                Keys {
                    g,
                    row: pane_last_row,
                    col0: 10,
                    n: 8,
                    period_ms: 60,
                    disp: 0.9,
                },
                &c,
            );
            let cx = ctx(at(t0, 8 * 60), &c, (pane_last_row, 18), 0.9);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            sink.under
                .iter()
                .map(|q| f32::from(q.y) + f32::from(q.h))
                .fold(0.0f32, f32::max)
        };
        let pane_floor = f32::from(g.origin_y) + f32::from(pane_last_row.saturating_add(1)) * ch;
        let bounded = deepest(Some((0, pane_last_row + 1)));
        let unbounded = deepest(None);
        assert!(
            unbounded > pane_floor + 0.5,
            "fixture: with no pane the band reaches past {pane_floor} \
             (deepest {unbounded}) — otherwise this pins nothing"
        );
        assert!(
            bounded <= pane_floor + 0.5,
            "the band reached {bounded}, past the focused pane's floor \
             {pane_floor} — that is the divider and the pane below"
        );
    }

    #[test]
    fn a_hot_paragraph_at_retina_keeps_its_head_row_whole_and_sheds_only_the_oldest_tail() {
        let c = cfg(true, true);
        let g = geom2x();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let mut sink = Sink::default();
        let mut ms = 0u64;
        for row in 2..5u16 {
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 76,
                period_ms: 5,
                disp: 1.0,
            };
            type_keys(&mut rib, at(t0, ms), keys, &c);
            ms += 76 * 5;
            let cx = ctx_in(at(t0, ms), &c, (row, 78), 1.0, g);
            rib.plan(&cx);
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let live_rows = row - 1;
            assert!(
                sink.under.len() <= RIBBON_QUAD_BUDGET,
                "{live_rows} retina rows spent {} quads against a {RIBBON_QUAD_BUDGET} budget",
                sink.under.len()
            );
            for col in 2..78u16 {
                assert!(
                    cell_lit(&sink.under, g, row, col),
                    "row {row} col {col} is dark on its own frame with {live_rows} rows live"
                );
            }
            match live_rows {
                1 => assert!(
                    rib.slabs_per_cell() < SLABS_PER_CELL,
                    "a 76-cell retina line does not fit at three slabs per cell; the density must give"
                ),
                2 => {
                    for col in 2..78u16 {
                        assert!(
                            cell_lit(&sink.under, g, 2, col),
                            "two retina rows fit the budget; row 2 col {col} was shed anyway"
                        );
                    }
                }
                _ => {
                    for col in 2..78u16 {
                        assert!(
                            cell_lit(&sink.under, g, 3, col),
                            "row 3 col {col} was shed before row 2"
                        );
                    }
                    let lit: Vec<bool> = (2..78u16)
                        .map(|col| cell_lit(&sink.under, g, 2, col))
                        .collect();
                    assert!(
                        lit.iter().any(|&l| !l) || rib.slabs_per_cell() == 1,
                        "three retina rows exceed the budget: the density must give or the oldest must lose its tail"
                    );
                    let first = lit.iter().position(|&l| l).unwrap_or(lit.len());
                    assert!(
                        lit[first..].iter().all(|&l| l),
                        "the shed must come off the oldest row's tail, not out of its middle"
                    );
                }
            }
        }
    }

    const fn geom_codex() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 57,
            cols: 151,
            origin_x: 0,
            origin_y: 0,
            win_w: 1359,
            win_h: 1026,
            head: 0,
        }
    }

    fn type_row(rib: &mut Ribbon, t0: Instant, row: u16, col0: u16, n: u16, c: &Config) {
        type_keys(
            rib,
            t0,
            Keys {
                g: geom_codex(),
                row,
                col0,
                n,
                period_ms: 60,
                disp: 0.5,
            },
            c,
        );
    }

    #[test]
    fn a_band_move_carries_the_composer_band_down_and_leaves_the_transcript_alone() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_row(&mut rib, t0, 5, 10, 3, &c);
        type_row(&mut rib, at(t0, 500), 20, 10, 6, &c);
        let before: Vec<Cell> = rib.cells().to_vec();
        assert_eq!(before.iter().filter(|l| l.row == 20).count(), 6);
        assert_eq!(before.iter().filter(|l| l.row == 5).count(), 3);
        let composer: Vec<Option<f32>> = (10..16).map(|col| rib.field_at(20, col)).collect();
        assert!(
            composer.iter().all(Option::is_some),
            "fixture: the composer is lit"
        );
        let transcript: Vec<Option<f32>> = (10..13).map(|col| rib.field_at(5, col)).collect();
        assert!(
            transcript.iter().all(Option::is_some),
            "fixture: the transcript is lit"
        );
        assert_eq!(rib.caret(), Some((20, 16)));

        rib.translate_band(19, 56, 1);

        let after = rib.cells();
        assert_eq!(
            after.len(),
            before.len(),
            "a band move loses no cell inside its band"
        );
        for b in &before {
            let want = if b.row == 20 { 21 } else { b.row };
            assert!(
                after.iter().any(|a| a.row == want
                    && a.col == b.col
                    && a.t == b.t
                    && a.born == b.born
                    && a.cohort == b.cohort
                    && a.retract_at == b.retract_at),
                "cell ({}, {}) did not arrive on row {want} with its clocks and prices",
                b.row,
                b.col
            );
        }
        assert!(
            after.iter().all(|a| a.row != 20),
            "the vacated row keeps no light"
        );
        for (i, col) in (10..16).enumerate() {
            assert_eq!(
                rib.field_at(21, col),
                composer[i],
                "the field moved with its cell"
            );
            assert_eq!(
                rib.field_at(20, col),
                None,
                "the vacated cell answers nothing"
            );
        }
        for (i, col) in (10..13).enumerate() {
            assert_eq!(
                rib.field_at(5, col),
                transcript[i],
                "the transcript stood still"
            );
        }
        assert_eq!(rib.caret(), Some((21, 16)), "the caret rides its band");
    }

    #[test]
    fn a_top_anchored_band_moves_the_transcript_up_and_pins_the_footer() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_row(&mut rib, t0, 0, 4, 2, &c);
        type_row(&mut rib, at(t0, 300), 3, 10, 4, &c);
        type_row(&mut rib, at(t0, 900), 54, 13, 5, &c);
        let footer: Vec<Option<f32>> = (13..18).map(|col| rib.field_at(54, col)).collect();
        let t3 = rib.field_at(3, 11).expect("fixture: row 3 is lit");
        assert_eq!(rib.caret(), Some((54, 18)));

        rib.translate_band(0, 51, -1);

        assert!(rib.cells().iter().all(|l| l.row != 0 && l.row != 3));
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 2).count(),
            4,
            "row 3 rode up to row 2"
        );
        assert_eq!(rib.field_at(2, 11), Some(t3));
        assert!(
            rib.cells().iter().all(|l| l.row != u16::MAX),
            "nothing wrapped around the top"
        );
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 54).count(),
            5,
            "the pinned composer is outside the band and untouched"
        );
        for (i, col) in (13..18).enumerate() {
            assert_eq!(rib.field_at(54, col), footer[i]);
        }
        assert_eq!(
            rib.caret(),
            Some((54, 18)),
            "a caret outside the band stays put"
        );
        assert_eq!(
            rib.cells().len(),
            9,
            "row 0's two cells left through the top; nothing else was lost"
        );
    }

    #[test]
    fn a_band_move_drops_what_leaves_the_band_and_retires_its_cohort() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_row(&mut rib, t0, 56, 10, 6, &c);
        assert!(rib.live_cells() == 6 && !rib.cohorts.is_empty());
        assert_eq!(rib.index.live_rows(), 1);

        rib.translate_band(50, 56, 1);

        assert!(
            rib.at_rest(),
            "light carried off the band's edge is not kept"
        );
        assert!(
            rib.cohorts.is_empty(),
            "a cohort with no cells retires with them"
        );
        for col in 10..16 {
            assert_eq!(rib.index.at(56, col), None);
            assert_eq!(rib.index.at(57, col), None);
        }
        assert_eq!(
            rib.index.live_rows(),
            0,
            "the lane retired into the spare pool"
        );
        assert_eq!(
            rib.index.spare.len(),
            1,
            "…and is there for the next row that lights"
        );
        assert_eq!(
            rib.caret(),
            Some((56, 16)),
            "the caret saturates at the edge"
        );
    }

    #[test]
    fn a_scroll_carries_the_ribbon_with_its_text() {
        let c = cfg(true, true);
        let g = geom2x();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 4, &c, 0.5);
        let now = at(t0, 4 * 60);
        erase_at(&mut rib, now, (2, 13), &c);
        let t13 = rib
            .field_at(2, 13)
            .expect("the retracting cell keeps its stop");
        let t12 = rib.field_at(2, 12).expect("col 12 is lit");
        assert_ne!(
            t13, t12,
            "the fixture needs a caret stop the newest key does not share"
        );
        assert_eq!(rib.caret(), Some((2, 13)));
        assert_eq!(rib.field_at_caret(), t13);
        assert!(
            rib.cells().iter().any(|l| l.retract_at.is_some()),
            "the fixture needs a cell on its retract clock"
        );
        let cx = ctx_in(now, &c, (2, 13), 0.7, g);
        rib.plan(&cx);
        let mut before = Sink::default();
        {
            let mut f = before.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !before.under.is_empty(),
            "the frame must draw for the law to bite"
        );
        let cells_before: Vec<Cell> = rib.cells().to_vec();

        rib.translate_scroll(1);

        assert_eq!(rib.caret(), Some((1, 13)), "the caret rides the scroll");
        assert_eq!(
            rib.field_at(1, 13),
            Some(t13),
            "the field moved with its cell"
        );
        assert_eq!(rib.field_at(2, 13), None, "nothing is left on the old row");
        assert_eq!(
            rib.field_at_caret(),
            t13,
            "the caret's stop is its own cell's, not the newest key's"
        );
        let cells_after = rib.cells();
        assert_eq!(
            cells_after.len(),
            cells_before.len(),
            "a scroll drops nothing on the grid"
        );
        for (a, b) in cells_before.iter().zip(cells_after) {
            assert_eq!(b.row + 1, a.row);
            assert_eq!(
                (b.col, b.cohort, b.born, b.retract_at, b.typing),
                (a.col, a.cohort, a.born, a.retract_at, a.typing)
            );
            assert_eq!(
                (b.t, b.life_s, b.cov0, b.birth_disp),
                (a.t, a.life_s, a.cov0, a.birth_disp)
            );
        }
        let cx1 = ctx_in(now, &c, (1, 13), 0.7, g);
        rib.plan(&cx1);
        let mut after = Sink::default();
        {
            let mut f = after.frame();
            rib.emit(&cx1, &mut f);
        }
        let ch = u16::try_from(g.ch).expect("ch");
        assert_eq!(
            after.under.len(),
            before.under.len(),
            "the body kept every quad"
        );
        for (a, b) in before.under.iter().zip(&after.under) {
            assert_eq!(
                (b.x, b.w, b.h, b.color, b.alpha),
                (a.x, a.w, a.h, a.color, a.alpha)
            );
            assert_eq!(
                b.y + ch,
                a.y,
                "a body quad moved by {} px, not one cell height ({ch})",
                i32::from(a.y) - i32::from(b.y)
            );
            assert_eq!(b.row + 1, a.row, "the damage hint rides with the quad");
        }
        rib.translate_scroll(2);
        assert!(rib.at_rest());
        assert_eq!(rib.field_at(0, 13), None);
    }

    #[test]
    fn a_fades_next_level_is_solved_not_polled() {
        let poll = |level_now: f32, curve: &dyn Fn(f32) -> f32, u: f32, span: f32| -> f32 {
            let mut step = 0u64;
            loop {
                step += 1;
                let t = (step as f64 * 1e-5) as f32;
                if (curve(u + t / span)).round() < level_now {
                    return t;
                }
                assert!(t < 5.0, "the polled level never dropped");
            }
        };
        let mut checked = 0usize;
        for peak in [UNDER_COV_CAP * 0.6, 30.0, 4.0] {
            for u in [0.0, 0.3, 0.7, 0.9, 0.97] {
                let span = RETRACT_FADE_S;
                let level = (peak * spend(u)).round();
                match level_step_spend(peak, u, span) {
                    None => assert!(
                        level < 1.0,
                        "spend peak {peak} u {u}: None while at {level}"
                    ),
                    Some(dt) => {
                        let got = poll(level, &|v| peak * spend(v), u, span);
                        assert!(
                            (dt - got).abs() < 2e-4,
                            "spend peak {peak} u {u}: solved {dt} s, polled {got} s"
                        );
                        checked += 1;
                    }
                }
                let life = 1.7;
                let level = (peak * expiry_melt(u)).round();
                match level_step_melt(peak, u, life) {
                    None => assert!(level < 1.0, "melt peak {peak} u {u}: None while at {level}"),
                    Some(dt) => {
                        let got = poll(level, &|v| peak * expiry_melt(v), u, life);
                        assert!(
                            (dt - got).abs() < 2e-4,
                            "melt peak {peak} u {u}: solved {dt} s, polled {got} s"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked >= 20, "only {checked} steps checked");
    }

    #[test]
    fn a_line_still_being_typed_keeps_every_cell_from_its_first_key_to_the_caret() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let row = 4u16;
        let col0 = 2u16;
        let words: [(&str, u64, u64, f32); 4] = [
            ("rainbow ", 110, 0, 0.9),
            ("theme", 250, 700, 0.45),
            (" truly ", 125, 0, 0.9),
            ("magical", 125, 0, 0.9),
        ];
        let mut key_ms = 0u64;
        let mut caret = col0;
        let mut now_ms = 0u64;
        let mut dark: Vec<String> = Vec::new();
        let mut keys = 0;
        for (word, per_key, pause, disp) in words {
            key_ms += pause;
            for _ in word.chars() {
                key_ms += per_key;
                while now_ms + 16 <= key_ms {
                    now_ms += 16;
                    let cx = ctx_in(at(t0, now_ms), &c, (row, caret), disp, g);
                    rib.plan(&cx);
                    let missing: Vec<u16> = (col0..caret)
                        .filter(|&col| rib.field_at(row, col).is_none())
                        .collect();
                    if !missing.is_empty() && dark.len() < 6 {
                        dark.push(format!(
                            "+{:.3}s keys={keys} caret={caret} dark={missing:?}",
                            now_ms as f32 / 1000.0
                        ));
                    }
                }
                caret += 1;
                keys += 1;
                let cx = ctx_in(at(t0, key_ms), &c, (row, caret), disp, g);
                rib.on_event(&typed(), at(t0, key_ms), &cx);
                rib.plan(&cx);
            }
        }
        assert_eq!(keys, 27);
        assert!(
            dark.is_empty(),
            "a cell went dark under a hand still typing:\n{}",
            dark.join("\n")
        );
        let last = key_ms;
        let grace_ms = (PHRASE_REST_MIN_S * 1000.0) as u64;
        let cx = ctx_in(at(t0, last + grace_ms), &c, (row, caret), 0.9, g);
        rib.plan(&cx);
        assert_eq!(
            (col0..caret)
                .filter(|&col| rib.field_at(row, col).is_some())
                .count(),
            usize::from(caret - col0),
            "every cell stands through the {PHRASE_REST_MIN_S} s grace"
        );
        let gone = last + (SWOOSH_TOTAL_S * 1000.0) as u64 + 20;
        let cx = ctx_in(at(t0, gone), &c, (row, caret), 0.9, g);
        rib.plan(&cx);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        assert!(
            rib.at_rest() && sink.under.is_empty() && sink.out.is_empty(),
            "off the glass {SWOOSH_TOTAL_S} s after the last key"
        );
    }

    fn landing_lit(rib: &Ribbon, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == row && !l.leaving())
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    fn landing_any(rib: &Ribbon, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == row)
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn a_parking_pass_after_the_row_was_backspaced_empty_still_finds_the_hand() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        let mut ms = 16 * 60;
        for i in 0..16u16 {
            ms += 40;
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, (13, 17 - i), 0.7, g);
            rib.on_event(&Event::Erase, now, &cx);
            rib.plan(&cx);
        }
        assert_eq!(rib.caret(), Some((13, 2)));
        assert!(
            landing_lit(&rib, 13).is_empty() && !landing_any(&rib, 13).is_empty(),
            "fixture: the held Backspace left row 13 entirely in retract, \
             live {:?} / all {:?}",
            landing_lit(&rib, 13),
            landing_any(&rib, 13)
        );
        ms += 40;
        let now = at(t0, ms);
        let cx = ctx_in(now, &c, (12, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 13),
            vec![2],
            "the key after the Backspace run did not land on the hand's own row"
        );
        assert!(
            landing_any(&rib, 11).is_empty(),
            "a stray was laid on the row above the box home: {:?}",
            landing_any(&rib, 11)
        );
    }

    #[test]
    fn a_parking_pass_is_caught_at_every_column_it_can_be_sampled_at() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 18)));
        let lit = |rib: &Ribbon, row: u16| {
            let mut v: Vec<u16> = rib
                .cells()
                .iter()
                .filter(|l| l.row == row && !l.leaving())
                .map(|l| l.col)
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        assert_eq!(lit(&rib, 13), (2..=17).collect::<Vec<_>>());
        for park in [0u16, 1, 5, 9, 17] {
            let mut rib = rib.clone();
            let now = at(t0, 16 * 60);
            let cx = ctx_in(now, &c, (12, park), 0.7, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            assert_eq!(
                lit(&rib, 13),
                (2..=18).collect::<Vec<_>>(),
                "a parking pass sampled at column {park} took the key off the hand's row"
            );
            assert!(
                lit(&rib, 12).is_empty() && lit(&rib, 11).is_empty(),
                "a stray was laid off the hand's row for a pass sampled at column {park}: \
                 row 12 {:?}, row 11 {:?}",
                lit(&rib, 12),
                lit(&rib, 11)
            );
        }
    }

    #[test]
    fn a_repaints_home_pass_poisons_the_mirror_and_the_pen_still_holds_the_hand() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 18)));
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (12, 0), 0.7, g);
        rib.on_event(&typed_move((13, 18), (12, 0)), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.caret(),
            Some((12, 0)),
            "the fixture's whole point: the MIRROR took the park"
        );
        assert!(
            rib.cohorts().iter().all(|k| !k.abandoned),
            "fixture: and the sibling law kept the hand's row"
        );
        let now = at(t0, 17 * 60);
        let cx = ctx_in(now, &c, (12, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 13),
            (2..=18).collect::<Vec<_>>(),
            "the key did not land on the hand's own row"
        );
        assert!(
            landing_any(&rib, 12).is_empty() && landing_any(&rib, 11).is_empty(),
            "a stray was laid off the hand's row: 11={:?} 12={:?}",
            landing_any(&rib, 11),
            landing_any(&rib, 12)
        );
    }

    #[test]
    fn a_nav_moves_the_pen_so_a_fold_after_it_still_lays_where_it_folded() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        let last = u16::try_from(g.cols).expect("cols") - 1;
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (20, last), 0.7, g);
        rib.on_event(
            &Event::Move {
                from: (13, 18),
                to: (20, last),
                licence: Licence::Nav,
                dir: Dir::Right,
            },
            now,
            &cx,
        );
        rib.plan(&cx);
        let now = at(t0, 17 * 60);
        let cx = ctx_in(now, &c, (21, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            landing_any(&rib, 20),
            vec![last],
            "the hand's own fold after a nav did not lay on the row it folded off"
        );
        assert!(
            !landing_any(&rib, 13).contains(&18),
            "the wrap key was dragged back onto the row the nav left: {:?}",
            landing_any(&rib, 13)
        );
    }

    #[test]
    fn a_downward_landing_is_questioned_only_where_lay_would_write_above_it() {
        let c = cfg(true, true);
        let g = geom();
        let seat = |t0: Instant| {
            let mut rib = Ribbon::new();
            type_keys(
                &mut rib,
                t0,
                Keys {
                    g,
                    row: 13,
                    col0: 2,
                    n: 16,
                    period_ms: 60,
                    disp: 0.7,
                },
                &c,
            );
            assert_eq!(rib.caret(), Some((13, 18)));
            rib
        };
        let t0 = Instant::now();
        let mut rib = seat(t0);
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (15, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(landing_lit(&rib, 13), (2..=18).collect::<Vec<_>>());
        assert!(landing_any(&rib, 14).is_empty() && landing_any(&rib, 15).is_empty());
        let t0 = Instant::now();
        let mut rib = seat(t0);
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (15, 3), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 15),
            vec![2],
            "a downward caret that does not fold is the hand's own, however far"
        );
        assert_eq!(landing_lit(&rib, 13), (2..=17).collect::<Vec<_>>());
    }

    #[test]
    fn a_delivered_burst_that_wraps_keeps_the_cells_it_left_on_the_row_above() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 112,
                n: 5,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(
            rib.caret(),
            Some((13, 117)),
            "fixture: the hand is five from the margin"
        );
        let now = at(t0, 5 * 60);
        let cx = ctx_in(now, &c, (14, 2), 0.7, g);
        rib.on_event(
            &Event::Typed {
                cells: 5,
                shifted: false,
                class: TypedClass::Glyph,
            },
            now,
            &cx,
        );
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 13),
            (112..=119).collect::<Vec<_>>(),
            "the burst's first three cells finished the hand's own row"
        );
        assert_eq!(
            landing_lit(&rib, 14),
            vec![0, 1],
            "…and the other two went on over the wrap"
        );
    }

    #[test]
    fn a_cold_caret_mirror_is_not_a_hand_a_key_can_be_brought_back_to() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let cold = ctx_in(t0, &c, (0, 0), 0.7, g);
        rib.plan(&cold);
        assert_eq!(rib.caret(), Some((0, 0)));
        assert!(
            rib.cells().is_empty(),
            "a cold ribbon holds no cell — there is no hand's row yet"
        );
        let now = at(t0, 80);
        let cx = ctx_in(now, &c, (2, 5), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        let lit = |rib: &Ribbon, row: u16| {
            let mut v: Vec<u16> = rib
                .cells()
                .iter()
                .filter(|l| l.row == row)
                .map(|l| l.col)
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        assert_eq!(
            lit(&rib, 2),
            vec![4],
            "the first key of the session did not land on the row the host reported"
        );
        assert!(
            lit(&rib, 0).is_empty(),
            "the cold mirror was read as a hand and took the key to row 0: {:?}",
            lit(&rib, 0)
        );
    }

    #[test]
    fn a_key_sampled_at_readlines_cr_park_stays_on_the_hands_own_row() {
        let c = cfg(true, true);
        let g = geom();

        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 18)), "fixture: the hand's caret");
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (13, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert!(
            landing_any(&rib, 12).is_empty(),
            "the key folded off the pane and laid on the row above — the \
             previous command's output: {:?}",
            landing_any(&rib, 12)
        );
        assert_eq!(
            landing_lit(&rib, 13),
            (2..=18).collect::<Vec<_>>(),
            "the key did not land on the hand's own row, next to its last cell"
        );

        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.set_pane(Some((40, 40)));
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 42,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 58)), "fixture: the hand's caret");
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (13, 40), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert!(
            landing_any(&rib, 12).is_empty(),
            "the key folded off the PANE's first column onto the row above: {:?}",
            landing_any(&rib, 12)
        );
        assert_eq!(
            landing_lit(&rib, 13),
            (42..=58).collect::<Vec<_>>(),
            "the key did not land on the hand's own row inside the pane"
        );

        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 20,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 36)), "fixture: the hand's caret");
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (13, 5), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert!(
            landing_lit(&rib, 13).contains(&4),
            "a same-row caret that folds nothing was re-homed to the pen: {:?}",
            landing_lit(&rib, 13)
        );
        assert!(
            landing_any(&rib, 12).is_empty(),
            "a caret that folds nothing reached the row above: {:?}",
            landing_any(&rib, 12)
        );
    }

    #[test]
    fn a_repaints_home_pass_does_not_abandon_the_row_the_hand_is_typing_on() {
        let c = cfg(true, true);
        let g = geom();
        for (excursion, what) in [
            ((12u16, 0u16), "the row above, column 0"),
            ((11, 0), "two rows above, column 0"),
        ] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_keys(
                &mut rib,
                t0,
                Keys {
                    g,
                    row: 13,
                    col0: 2,
                    n: 20,
                    period_ms: 60,
                    disp: 0.7,
                },
                &c,
            );
            let ms = 19 * 60 + 60;
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, excursion, 0.7, g);
            rib.on_event(&typed_move((13, 22), excursion), now, &cx);
            rib.plan(&cx);
            assert!(
                rib.cohorts().iter().all(|k| !k.abandoned),
                "{what}: the repaint's home abandoned the band"
            );
            assert_eq!(
                rib.cells()
                    .iter()
                    .filter(|l| l.row == 13 && !l.leaving())
                    .count(),
                20,
                "{what}: the band lost cells to a caret the hand never put there"
            );
            let now = at(t0, ms + 60);
            let cx = ctx_in(now, &c, (12, 79), 0.7, g);
            rib.on_event(&typed_move((13, 2), (12, 79)), now, &cx);
            rib.plan(&cx);
            assert!(
                rib.cohorts().iter().any(|k| k.row == 13 && k.abandoned),
                "{what}: a backspace back through the fold stopped being a row change"
            );
        }
    }

    #[test]
    fn a_re_anchor_inside_the_scroll_that_moved_the_band_drains_nothing() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 20,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        rib.translate_scroll(1);
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 12).count(),
            20,
            "the band followed its text up"
        );
        let now = at(t0, 19 * 60 + 60);
        let cx = ctx_in(now, &c, (13, 3), 0.7, g);
        rib.on_event(&typed_move((12, 22), (12, 2)), now, &cx);
        assert!(
            rib.cells().iter().all(|l| l.retract_at.is_none()),
            "the re-anchor drained the row the scroll had just carried: {:?}",
            rib.cells()
                .iter()
                .filter(|l| l.retract_at.is_some())
                .map(|l| l.col)
                .collect::<Vec<_>>()
        );
        rib.plan(&cx);
        let now = at(t0, 19 * 60 + 120);
        let cx = ctx_in(now, &c, (12, 2), 0.7, g);
        rib.on_event(&typed_move((12, 22), (12, 2)), now, &cx);
        assert!(
            rib.cells().iter().any(|l| l.retract_at.is_some()),
            "the scroll guard outlived its own tick"
        );
    }

    #[test]
    fn a_backward_typed_echo_under_the_re_anchor_floor_stamps_nothing() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let census = |rib: &Ribbon| {
            let mut v: Vec<(u16, u16, Instant, bool)> = rib
                .cells()
                .iter()
                .map(|l| (l.row, l.col, l.born, l.typing))
                .collect();
            v.sort_unstable();
            v
        };
        let stamped = |rib: &Ribbon| {
            rib.cells()
                .iter()
                .filter(|l| l.leaving())
                .map(|l| l.col)
                .collect::<Vec<_>>()
        };
        for back in 1..RE_ANCHOR_MIN_CELLS {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 6, &c, 0.8);
            let before = census(&rib);
            let now = at(t0, 6 * 60 + 200);
            let landing = (2, 16 - back);
            let cx = ctx(now, &c, landing, 0.8);
            rib.on_event(&typed_move((2, 16), landing), now, &cx);
            rib.plan(&cx);
            assert!(
                stamped(&rib).is_empty(),
                "a {back}-cell backward typed echo drained the row: stamped={:?}",
                stamped(&rib)
            );
            assert_eq!(census(&rib), before, "{back} back: the census is unchanged");
            assert_eq!(rib.caret(), Some(landing), "{back} back: the mirror moved");
        }
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.8);
        let now = at(t0, 6 * 60 + 200);
        let landing = (2, 16 - RE_ANCHOR_MIN_CELLS);
        let cx = ctx(now, &c, landing, 0.8);
        rib.on_event(&typed_move((2, 16), landing), now, &cx);
        assert_eq!(
            stamped(&rib),
            vec![13, 14, 15],
            "at the floor the re-anchor drains the cells at and past the landing"
        );
    }

    #[test]
    fn a_shell_home_relays_no_word_onto_the_prompt() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let mut now = t0;
        for (i, ch) in "hello there now".chars().enumerate() {
            now = at(t0, i as u64 * 60);
            let col0 = 28 + i as u16;
            let cx = ctx_in(now, &c, (5, col0 + 1), 0.9, g);
            let ev = if ch == ' ' { typed_space() } else { typed() };
            rib.on_event(&ev, now, &cx);
            rib.plan(&cx);
        }
        let home = at(now, 60);
        let cx = ctx_in(home, &c, (5, 28), 0.9, g);
        rib.on_event(&typed_move((5, 43), (5, 28)), home, &cx);
        rib.plan(&cx);
        let inside_prompt: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 5 && l.col < 28)
            .map(|l| l.col)
            .collect();
        assert!(
            inside_prompt.is_empty(),
            "the home relaid a word onto the prompt: columns {inside_prompt:?} are lit left of \
             the 28-column PS1"
        );
        let k = at(home, 60);
        let cx = ctx_in(k, &c, (5, 29), 0.9, g);
        rib.on_event(&typed(), k, &cx);
        rib.on_event(&typed_move((5, 28), (5, 29)), k, &cx);
        rib.plan(&cx);
        let inside_prompt: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 5 && l.col < 28)
            .map(|l| l.col)
            .collect();
        assert!(
            inside_prompt.is_empty(),
            "a pending relocation settled onto the prompt: columns {inside_prompt:?}"
        );
    }

    const fn typed_move(from: (u16, u16), to: (u16, u16)) -> Event {
        Event::Move {
            from,
            to,
            licence: Licence::Typed,
            dir: Dir::of(to.1 as i32 - from.1 as i32, to.0 as i32 - from.0 as i32),
        }
    }

    const fn typed_space() -> Event {
        Event::Typed {
            cells: 1,
            shifted: false,
            class: TypedClass::Space,
        }
    }

    fn interior_covs(rib: &Ribbon, col: u16) -> Vec<u8> {
        let cw = geom().cw as f32;
        let (x0, x1) = (f32::from(col) * cw, f32::from(col + 1) * cw);
        rib.plan_segments()
            .iter()
            .filter(|s| s.x > x0 + 0.5 && s.x < x1 - 0.5)
            .map(|s| s.cov)
            .collect()
    }

    fn boundary_cov(rib: &Ribbon, col: u16) -> Option<u8> {
        let x = f32::from(col) * geom().cw as f32;
        rib.plan_segments()
            .iter()
            .find(|s| (s.x - x).abs() < 0.5)
            .map(|s| s.cov)
    }

    #[test]
    fn a_coalesced_echo_leaves_one_cohort_and_no_feather_inside_the_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 6, &c, 0.9);
        let k1 = at(t0, 360);
        let k2 = at(t0, 370);
        let now = at(t0, 380);
        let cx = ctx(now, &c, (2, 10), 0.9);
        rib.on_event(&typed(), k1, &cx);
        rib.on_event(&typed(), k2, &cx);
        rib.on_event(
            &Event::Sweep {
                row: 2,
                col0: 8,
                col1: 10,
            },
            k1,
            &cx,
        );
        rib.on_event(&typed_move((2, 8), (2, 10)), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.cohorts().iter().filter(|k| k.row == 2).count(),
            1,
            "the batch's last cell joined the row's one cohort: {:?}",
            rib.cohorts()
        );
        for col in 2..10u16 {
            assert!(
                rib.cells().iter().any(|l| l.row == 2 && l.col == col),
                "col {col} is laid"
            );
        }
        let cx = ctx(at(t0, 500), &c, (2, 10), 0.9);
        rib.plan(&cx);
        let body = boundary_cov(&rib, 6).expect("a boundary inside the band");
        for col in 3..10u16 {
            let b = boundary_cov(&rib, col).expect("boundary {col}");
            assert!(
                u32::from(b) * 10 >= u32::from(body) * 9,
                "boundary {col} is feathered: {b} against the body's {body}"
            );
        }
    }

    #[test]
    fn a_boundary_takes_the_brighter_cell_s_coverage() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 4, &c, 0.9);
        let cx = ctx(at(t0, 400), &c, (2, 6), 0.9);
        rib.plan(&cx);
        let settled = interior_covs(&rib, 5);
        let edge = boundary_cov(&rib, 6).expect("the head's right edge");
        let k = at(t0, 420);
        let cx = ctx(at(t0, 422), &c, (2, 7), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        assert_eq!(
            interior_covs(&rib, 5),
            settled,
            "cell 5 dipped under cell 6's attack"
        );
        assert!(
            boundary_cov(&rib, 6).expect("the shared boundary") >= edge - 1,
            "the shared boundary sagged toward the attacking cell"
        );
        let laid: Vec<u8> = interior_covs(&rib, 6);
        assert!(
            laid.iter().all(|&v| v + 1 >= edge),
            "the cell laid where the attach stood keeps the attach's level: {laid:?} under {edge}"
        );
        let young: Vec<u8> = interior_covs(&rib, 7);
        assert!(
            !young.is_empty() && young.iter().all(|&v| v < edge),
            "the attack lives in the attach under the caret cell: {young:?} under {edge}"
        );
        let cx = ctx(at(t0, 700), &c, (2, 7), 0.9);
        rib.plan(&cx);
        let hold = interior_covs(&rib, 5);
        let hold_edge = boundary_cov(&rib, 6).expect("edge");
        erase_at(&mut rib, at(t0, 720), (2, 6), &c);
        for ms in [730u64, 800, 900, 940, 970, 1000] {
            let cx = ctx(at(t0, ms), &c, (2, 6), 0.9);
            rib.plan(&cx);
            assert_eq!(
                interior_covs(&rib, 5),
                hold,
                "+{ms} ms: the survivor sagged"
            );
            assert!(
                boundary_cov(&rib, 6).expect("edge") + 1 >= hold_edge,
                "+{ms} ms: the survivor's edge sagged"
            );
        }
        assert!(
            !rib.cells().iter().any(|l| l.col == 6),
            "the erased cell retired inside {RETRACT_FADE_S} s"
        );
    }

    #[test]
    fn an_erase_whose_echo_lands_a_tick_late_still_retracts_the_erased_cell() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        let k = at(last, 300);
        let cx = ctx(k, &c, (2, 10), 0.9);
        rib.on_event(&Event::Erase, k, &cx);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|l| l.retract_at.is_none()),
            "nothing right of the pre-retreat caret to retract"
        );
        let now = at(last, 316);
        let cx = ctx(now, &c, (2, 9), 0.9);
        rib.on_event(&typed_move((2, 10), (2, 9)), now, &cx);
        rib.plan(&cx);
        let erased = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 9)
            .expect("the erased glyph's cell is still spending");
        assert!(
            erased.retract_at.is_some(),
            "the erased cell stayed lit under the blank"
        );
        assert!(
            rib.cells()
                .iter()
                .filter(|l| l.col < 9)
                .all(|l| l.retract_at.is_none()),
            "only the erased cell retracts"
        );
        let now = at(last, 400);
        let cx = ctx(now, &c, (2, 10), 0.9);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(rib.pending_erase, 0);
    }

    #[test]
    fn a_typed_row_change_lets_the_old_row_leave_on_its_own_clock() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 100,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let last = at(t0, 19 * 60);
        let now = at(last, 60);
        let cx = ctx(now, &c, (3, 0), 0.9);
        rib.on_event(&typed_move((2, 120), (3, 0)), now, &cx);
        rib.plan(&cx);
        let old = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the old row");
        let last_key_on_the_old_row = old.alive_at;
        assert!(
            !old.abandoned,
            "a fold abandoned the row the wrap came off: the band is killed at every wrap"
        );
        assert_eq!(
            old.retract_col, None,
            "nothing froze the old row's retract target: it has not entered one"
        );
        assert!(
            last_key_on_the_old_row >= at(t0, 19 * 60),
            "the old row keeps the life its last key gave it"
        );
        let keys = Keys {
            g: geom(),
            row: 3,
            col0: 0,
            n: 12,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(now, 60), keys, &c);
        let mut sink = Sink::default();
        let cx = ctx(at(now, 500), &c, (3, 12), 0.9);
        rib.plan(&cx);
        let old = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the old row at +0.5 s");
        assert!(old.flow.is_some(), "the fold set the old row flowing");
        assert_eq!(old.phase, Phase::Retracting, "…and it is moving at +0.5 s");
        assert_eq!(
            old.retract_col,
            Some(119),
            "toward the fold point: the row's own right end"
        );
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 2).count(),
            20,
            "the row the wrap came off went dark under the hand: the band does not span the fold"
        );
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let (cov, _, left) = row_light(&sink, geom(), 2).expect("row 2 is lit at +0.5 s");
        assert!(cov > 0.0);
        assert!(
            left > 100.0 * geom().cw as f32 + 0.5,
            "the far (left) end drained first: the left edge is at {left} px"
        );
        let gone_ms = (FLOW_TOTAL_S * 1000.0) as u64 + 60;
        let cx = ctx(at(now, gone_ms), &c, (3, 12), 0.9);
        rib.plan(&cx);
        assert!(
            !rib.cells().iter().any(|l| l.row == 2),
            "the old row is still lit {FLOW_TOTAL_S} s + 60 ms after the fold"
        );
        assert!(rib.cells().iter().any(|l| l.row == 3), "the new row is lit");
    }

    fn a_line_of_thirty(rib: &mut Ribbon, t0: Instant, c: &Config) -> f32 {
        type_run(rib, t0, 2, 30, c, 0.9);
        let band = rib.cohorts()[0];
        let t_next = band.t_at(32);
        assert_eq!(
            t_next.to_bits(),
            (band.t0 + walk_t(f32::from(32 - band.anchor_col))).to_bits(),
            "the premise: a cold walk, read as it was before the odometer"
        );
        t_next
    }

    const fn row_change(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
        Event::Move {
            from,
            to,
            licence,
            dir: Dir::of(to.1 as i32 - from.1 as i32, to.0 as i32 - from.0 as i32),
        }
    }

    #[test]
    fn a_composer_gate_waits_for_its_home_and_only_its_first_real_echo_spends_it() {
        let model = aterm_models_effects::rainbow_composer_newline_gate_model();
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let t_next = a_line_of_thirty(&mut rib, t0, &c);
        let chord = at(t0, 29 * 60 + 100);
        let old = ctx(chord, &c, (2, 32), 0.9);

        rib.on_event(&Event::ComposerNewline, chord, &old);
        let armed = model.successors("ComposerKey", &model.init_state())[0].clone();
        assert_eq!(armed.get("moved"), Some(&0));
        assert_eq!(
            rib.fresh_line,
            Some(FreshLine {
                since: chord,
                chord_at: Some(chord),
                awaiting_move: true,
                left_row: None,
            })
        );

        rib.on_event(&typed(), chord, &old);
        let stamped = model.successors("ChordStamp", &armed)[0].clone();
        assert_eq!(stamped.get("spent"), Some(&0));
        assert!(rib.fresh_line.is_some_and(|gate| gate.awaiting_move));

        let key = at(chord, 10);
        rib.on_event(&typed(), key, &ctx(key, &c, (2, 32), 0.9));
        let raced = model.successors("TypeAheadBeforeHome", &stamped)[0].clone();
        assert_eq!(raced.get("spent"), Some(&0));
        assert!(rib.fresh_line.is_some_and(|gate| gate.awaiting_move));

        let seen = at(chord, 30);
        rib.on_event(
            &row_change((2, 32), (2, 2), Licence::Return),
            seen,
            &ctx(seen, &c, (2, 2), 0.9),
        );
        let homed = model.successors("ObserveHome", &raced)[0].clone();
        assert_eq!(homed.get("moved"), Some(&1));
        assert_eq!(
            rib.fresh_line.map(|gate| (gate.since, gate.awaiting_move)),
            Some((chord, false))
        );

        let echo = at(seen, 20);
        rib.on_event(
            &Event::Sweep {
                row: 2,
                col0: 2,
                col1: 3,
            },
            key,
            &ctx(echo, &c, (2, 3), 0.9),
        );
        let echoed = model.successors("FirstEcho", &homed)[0].clone();
        assert_eq!(echoed.get("spent"), Some(&1));
        assert!(rib.fresh_line.is_none());
        let line = rib
            .cohorts()
            .iter()
            .find(|q| q.row == 2 && q.anchor_col == 2 && q.born > chord)
            .expect("the first echo made the new line's cohort");
        assert_eq!((line.t0.to_bits(), line.d0), (t_next.to_bits(), 0.0));

        let mut plain = Ribbon::new();
        a_line_of_thirty(&mut plain, t0, &c);
        let plain_state = model.successors("PlainModifiedKey", &model.init_state())[0].clone();
        plain.on_event(&typed(), chord, &old);
        plain.on_event(
            &row_change((2, 32), (2, 2), Licence::Typed),
            seen,
            &ctx(seen, &c, (2, 2), 0.9),
        );
        assert!(!model.action_enabled("ObserveHome", &plain_state));
        assert!(
            plain.fresh_line.is_none(),
            "a non-composer park arms no line gate"
        );

        let mut interrupted = Ribbon::new();
        a_line_of_thirty(&mut interrupted, t0, &c);
        interrupted.on_event(&Event::ComposerNewline, chord, &old);
        assert!(
            interrupted
                .fresh_line
                .is_some_and(|gate| gate.awaiting_move)
        );
        interrupted.curtain(at(chord, 1));
        assert!(
            interrupted.fresh_line.is_some_and(|gate| {
                !gate.awaiting_move && gate.chord_at.is_none() && gate.left_row.is_none()
            }),
            "a new coordinate space cannot wait for the old composer's home"
        );

        let pending = model.successors("ComposerKey", &model.init_state())[0].clone();
        let cancelled = model.successors("CancelBeforeHome", &pending)[0].clone();
        assert_eq!(cancelled.get("composer"), Some(&0));
        for superseding in [
            Event::CancelComposerNewline,
            Event::Erase,
            Event::Kill {
                cells: 1,
                scope: super::super::KillScope::Word,
            },
            row_change((2, 32), (2, 31), Licence::Nav),
        ] {
            let mut interrupted = Ribbon::new();
            a_line_of_thirty(&mut interrupted, t0, &c);
            interrupted.on_event(&Event::ComposerNewline, chord, &old);
            interrupted.on_event(&superseding, at(chord, 1), &old);
            assert!(
                interrupted.fresh_line.is_none(),
                "{superseding:?} left the cancelled composer gate pending"
            );
        }
    }

    fn row_light(sink: &Sink, g: Geom, row: u16) -> Option<(f32, f32, f32)> {
        let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
        let (mut cov, mut num, mut left) = (0.0f32, 0.0f32, f32::INFINITY);
        for q in &sink.under {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
                continue;
            }
            let w = f32::from(q.w) * f32::from(q.alpha) / 255.0;
            cov += w;
            num += (f32::from(q.x) + f32::from(q.w) * 0.5) * w;
            left = left.min(f32::from(q.x));
        }
        (cov > 0.0).then(|| (cov / (g.cols * g.cw) as f32, num / cov, left))
    }

    fn drift_census(sink: &Sink, g: Geom, row: u16) -> Option<DriftCensus> {
        let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
        let mut cov = 0.0f32;
        let (mut left, mut right) = (f32::INFINITY, f32::NEG_INFINITY);
        for q in &sink.under {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
                continue;
            }
            cov += f32::from(q.w) * f32::from(q.alpha) / 255.0;
            left = left.min(f32::from(q.x));
            right = right.max(f32::from(q.x) + f32::from(q.w));
        }
        (cov > 0.0).then(|| DriftCensus {
            cov: cov / (g.cols * g.cw) as f32,
            left,
            right,
        })
    }

    struct DriftCensus {
        cov: f32,
        left: f32,
        right: f32,
    }

    fn seam_far_tail(sink: &Sink, g: Geom, row: u16, head_col: u16) -> (f32, f32) {
        let cw = g.cw as f32;
        let head_x = f32::from(g.origin_x) + f32::from(head_col) * cw;
        let (y_lo, y_hi) = (
            f32::from(g.origin_y) + (f32::from(row) - 0.5) * g.ch as f32,
            f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32,
        );
        let (mut far, mut left) = (0.0f32, f32::INFINITY);
        for q in &sink.out {
            let (x, y) = (f32::from(q.x), f32::from(q.y));
            if q.w == 0 || y < y_lo || y >= y_hi || x >= head_x - (HOT_EDGE_CELLS + 1.0) * cw {
                continue;
            }
            let lum = (q.color >> 16 & 0xFF) + (q.color >> 8 & 0xFF) + (q.color & 0xFF);
            far += f32::from(q.w) * lum as f32;
            if lum > 0 {
                left = left.min(x);
            }
        }
        (far, left)
    }

    fn light_between(sink: &Sink, g: Geom, row: u16, x0: f32, x1: f32) -> f32 {
        let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
        let mut cov = 0.0f32;
        for q in &sink.under {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
                continue;
            }
            let (qx0, qx1) = (f32::from(q.x), f32::from(q.x) + f32::from(q.w));
            let w = (qx1.min(x1) - qx0.max(x0)).max(0.0);
            cov += w * f32::from(q.alpha) / 255.0;
        }
        cov / (g.cols * g.cw) as f32
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum BsEcho {
        SameTick,
        Late(u64),
    }

    type BsFrame = (u64, Option<(f32, f32, f32)>, bool, Vec<Phase>);

    fn bs_back_through_a_fold(folded: bool, echo: BsEcho, bs_ms: &[u64]) -> (Vec<BsFrame>, f32) {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 100,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let rest = rib.rest_s();
        let fold = at(t0, 20 * 60);
        let mut caret = (2u16, 120u16);
        if folded {
            caret = (3, 0);
            let cx = ctx(fold, &c, caret, 0.9);
            rib.on_event(&typed_move((2, 120), caret), fold, &cx);
            rib.plan(&cx);
        }
        let mut sink = Sink::default();
        let mut out = Vec::new();
        let mut k = 0usize;
        let mut due: Option<(u64, (u16, u16))> = None;
        let mut ms = 16u64;
        while ms <= 1520 {
            if k < bs_ms.len() && ms >= bs_ms[k] && due.is_none() {
                let key = at(fold, bs_ms[k]);
                let to = if caret.0 == 3 {
                    (2, 119)
                } else {
                    (2, caret.1 - 1)
                };
                match echo {
                    BsEcho::SameTick => {
                        let cx = ctx(key, &c, to, 0.9);
                        rib.on_event(&Event::Erase, key, &cx);
                        rib.on_event(&typed_move(caret, to), key, &cx);
                        caret = to;
                    }
                    BsEcho::Late(lag) => {
                        let cx = ctx(key, &c, caret, 0.9);
                        rib.on_event(&Event::Erase, key, &cx);
                        due = Some((bs_ms[k] + lag, to));
                    }
                }
                k += 1;
            }
            if let Some((due_ms, to)) = due
                && ms >= due_ms
            {
                let echo_at = at(fold, due_ms);
                let cx = ctx(echo_at, &c, to, 0.9);
                rib.on_event(&typed_move(caret, to), echo_at, &cx);
                caret = to;
                due = None;
            }
            let now = at(fold, ms);
            let cx = ctx(now, &c, caret, 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let row2: Vec<Cohort> = rib
                .cohorts()
                .iter()
                .filter(|q| q.row == 2)
                .copied()
                .collect();
            out.push((
                ms,
                row_light(&sink, g, 2),
                row2.iter().any(|q| q.flow.is_some()),
                row2.iter().map(|q| q.phase).collect::<Vec<_>>(),
            ));
            ms += 16;
        }
        (out, rest)
    }

    fn assert_the_row_holds_under_the_hand(
        what: &str,
        folded: &[BsFrame],
        control: &[BsFrame],
        from_ms: u64,
        until_ms: u64,
    ) {
        let settled = folded
            .iter()
            .find(|s| s.0 >= from_ms)
            .and_then(|s| s.1)
            .unwrap_or_else(|| panic!("{what}: row 2 is lit at +{from_ms} ms"))
            .2;
        let mut read = 0u32;
        for (f, k) in folded.iter().zip(control) {
            let (ms, light, flowing, phases) = f;
            if *ms < from_ms || *ms >= until_ms {
                continue;
            }
            read += 1;
            assert!(
                !flowing,
                "{what}: +{ms} ms: row 2 still flows under the hand that came back to it"
            );
            assert!(
                phases
                    .iter()
                    .all(|p| matches!(p, Phase::Laying | Phase::Grace)),
                "{what}: +{ms} ms: the row left its hold under the hand: {phases:?}"
            );
            let (cov, _, left) =
                light.unwrap_or_else(|| panic!("{what}: +{ms} ms: row 2 is lit under the hand"));
            assert!(
                (left - settled).abs() <= 0.5,
                "{what}: +{ms} ms: the row's left edge moved from {settled} px to {left} px: the \
                 drift restarted"
            );
            let (want, _, _) = k.1.expect("the control is lit");
            assert!(
                cov >= 0.85 * want,
                "{what}: +{ms} ms: row 2 drained under the hand: {cov} against the never-folded \
                 control's {want}"
            );
        }
        assert!(
            read >= 10,
            "{what}: the window +{from_ms}..+{until_ms} ms read {read} frames"
        );
    }

    #[test]
    fn the_hand_arriving_back_through_a_shell_fold_takes_the_row_back_after_the_rest_and_alone() {
        let runs: [(&str, &[u64]); 3] = [
            ("a run inside the rest", &[416, 480, 540, 600, 660, 720]),
            ("a run after the rest", &[900, 960, 1020, 1080, 1140]),
            ("a lone Backspace", &[416]),
        ];
        for (name, bs_ms) in runs {
            for echo in [BsEcho::SameTick, BsEcho::Late(0), BsEcho::Late(16)] {
                let what = format!("{name}, {echo:?}");
                let (control, rest) = bs_back_through_a_fold(false, echo, bs_ms);
                let (folded, _) = bs_back_through_a_fold(true, echo, bs_ms);
                let lag = match echo {
                    BsEcho::SameTick => 0,
                    BsEcho::Late(lag) => lag,
                };
                let rest_ms = (rest * 1000.0) as u64;
                if name == "a run after the rest" {
                    assert!(
                        60 + bs_ms[0] >= rest_ms,
                        "{what}: the run starts {} ms after the last key, inside the {rest_ms} \
                         ms rest",
                        60 + bs_ms[0]
                    );
                }
                let from = bs_ms[0] + lag;
                let until = bs_ms[bs_ms.len() - 1] + lag + rest_ms;
                assert_the_row_holds_under_the_hand(&what, &folded, &control, from, until);
            }
        }
    }

    #[test]
    fn a_composer_re_wrap_s_moved_word_fades_in_place_while_the_row_it_left_drifts() {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 40,
            n: 30,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        for i in 0..9u16 {
            let now = at(t0, u64::from(30 + i) * 60);
            let ev = if i == 0 { typed_space() } else { typed() };
            let cx = ctx(now, &c, (2, 71 + i), 0.9);
            rib.on_event(&ev, now, &cx);
            rib.plan(&cx);
        }
        let wrap = at(t0, 39 * 60);
        let cx = ctx(wrap, &c, (3, 11), 0.9);
        rib.on_event(&typed(), wrap, &cx);
        rib.on_event(&typed_move((2, 79), (3, 11)), wrap, &cx);
        rib.plan(&cx);
        let (word_x0, word_x1) = (70.0 * cw, 79.0 * cw);
        let kept_end = 70.0 * cw;
        let mut sink = Sink::default();
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let first = drift_census(&sink, g, 2).expect("row 2 is lit at the wrap");
        let word0 = light_between(&sink, g, 2, word_x0, word_x1);
        assert!(
            word0 > 0.0 && (first.right - word_x1).abs() <= 0.5,
            "the fixture's point: the moved word is lit on row 2 at the wrap ({word0}, right \
             edge {} px)",
            first.right
        );
        let mut prev = first.cov;
        let mut worst = 0.0f32;
        let mut clipped = 0u32;
        let mut key_col = 11u16;
        let mut ms = 16u64;
        while ms <= 1600 {
            let now = at(wrap, ms);
            if ms / 60 != (ms - 16) / 60 {
                key_col += 1;
                let kx = ctx(now, &c, (3, key_col), 0.9);
                rib.on_event(&typed(), now, &kx);
            }
            let cx = ctx(now, &c, (3, key_col), 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let Some(r) = drift_census(&sink, g, 2) else {
                worst = worst.max(prev / first.cov);
                prev = 0.0;
                ms += 16;
                continue;
            };
            if ms == 16 {
                let word = light_between(&sink, g, 2, word_x0, word_x1);
                assert!(
                    word >= 0.8 * word0,
                    "the moved word was cut on the flow's first frame: {word} of its {word0}"
                );
            }
            worst = worst.max((prev - r.cov) / first.cov);
            assert!(
                r.right <= word_x1 + 0.5,
                "light at {} px, past the moved word's old end at {word_x1} px (+{ms} ms)",
                r.right
            );
            if !rib.cells().iter().any(|l| l.row == 2 && l.col >= 70) {
                clipped += 1;
                assert!(
                    r.right <= kept_end + 0.5,
                    "CLIP: light at {} px, past the row's kept end at {kept_end} px (+{ms} ms)",
                    r.right
                );
            }
            prev = r.cov;
            ms += 16;
        }
        assert!(
            worst <= 0.04,
            "NO CLIFF: a frame took {:.1} % of the wrap frame's light",
            100.0 * worst
        );
        assert!(
            clipped >= 20,
            "CLIP was read on the drifting band after the word had gone: {clipped} frames"
        );
    }

    #[test]
    fn a_band_carried_down_off_the_caret_s_row_drifts_into_its_kept_start_and_never_past_it() {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 5,
            col0: 20,
            n: 30,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let carry = at(t0, 30 * 60);
        let id = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 5)
            .map(|k| k.id)
            .expect("the band");
        let run = super::super::witness::FollowRun {
            row: 5,
            cohort: id,
            lo: 20,
            hi: 49,
            dr: 1,
        };
        let mut moved = Vec::new();
        assert_eq!(rib.translate_run(run, carry, true, &mut moved), 30);
        let cx = ctx(carry, &c, (5, 50), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let first = drift_census(&sink, g, 6).expect("row 6 is lit at the carry");
        let start = f32::from(g.origin_x) + 20.0 * cw;
        assert!(
            (first.left - start).abs() <= 0.5,
            "the fixture's point: the band starts on the clip line: {} px",
            first.left
        );
        let mut drifted = false;
        let mut ms = 16u64;
        while ms <= 1600 {
            let now = at(carry, ms);
            let cx = ctx(now, &c, (5, 50), 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            if ms == 16 {
                let coh = rib
                    .cohorts()
                    .iter()
                    .find(|k| k.id == id)
                    .copied()
                    .expect("the carried band");
                assert!(coh.flow.is_some(), "the carry set the band flowing");
                assert!(
                    coh.drain_right_first && coh.retract_col == Some(20),
                    "a row below the caret flows into its own left end: {:?}, right first {}",
                    coh.retract_col,
                    coh.drain_right_first
                );
            }
            if let Some(r) = drift_census(&sink, g, 6) {
                assert!(
                    r.left >= start - 0.5,
                    "CLIP: light at {} px, past the band's kept start at {start} px (+{ms} ms)",
                    r.left
                );
                drifted |= r.right < first.right - cw;
            }
            ms += 16;
        }
        assert!(drifted, "the band drifted toward its start");

        for below in [false, true] {
            for edge_disp in [0.0, 1.0] {
                let mut rib = Ribbon::new();
                type_run(&mut rib, t0, 0, 1, &c, 0.0);
                rib.plan(&ctx(at(t0, 5000), &c, (2, 1), 0.0));
                assert!(rib.at_rest());
                let born = at(t0, 6000);
                type_run(&mut rib, born, 10, 2, &c, 0.0);
                let edge_at = at(born, 120);
                let cx = ctx(edge_at, &c, (2, 13), edge_disp);
                rib.on_event(&typed(), edge_at, &cx);
                rib.plan(&cx);
                let edge = *rib
                    .cells()
                    .iter()
                    .find(|cell| cell.col == 12)
                    .expect("edge");
                let coh = *rib
                    .cohorts()
                    .iter()
                    .find(|coh| coh.id == edge.cohort)
                    .expect("band");
                let reach_at = edge_at + Duration::from_secs_f32(coh.grace_s + 0.001);
                assert_eq!(rib.retire_cells(&[(2, 12, edge.born)], reach_at), 1);
                rib.plan(&ctx(reach_at, &c, (2, 13), 0.0));
                let reach = *rib
                    .cells()
                    .iter()
                    .find(|cell| cell.col == 9)
                    .expect("reach");
                assert!(!reach.leaving() && reach.born < edge.born);
                let mut moved = Vec::new();
                assert_eq!(
                    rib.shift_run(
                        super::super::witness::ShiftRun {
                            row: 2,
                            cohort: coh.id,
                            lo: 9,
                            hi: 9,
                            dc: 3,
                        },
                        &mut moved
                    ),
                    1
                );
                assert_eq!(
                    rib.shift_run(
                        super::super::witness::ShiftRun {
                            row: 2,
                            cohort: coh.id,
                            lo: 10,
                            hi: 11,
                            dc: -8,
                        },
                        &mut moved
                    ),
                    2
                );
                let (edge_col, kept) = if below {
                    assert_eq!(
                        rib.shift_run(
                            super::super::witness::ShiftRun {
                                row: 2,
                                cohort: coh.id,
                                lo: 2,
                                hi: 3,
                                dc: 18,
                            },
                            &mut moved
                        ),
                        2
                    );
                    (12, 10.0)
                } else {
                    (12, 11.0)
                };
                let residents: Vec<_> = rib
                    .cells()
                    .iter()
                    .filter(|cell| cell.col == edge_col)
                    .collect();
                assert_eq!(residents.len(), 2);
                assert!(residents[0].leaving() && residents[0].born == edge.born);
                assert!(!residents[1].leaving() && residents[1].born == reach.born);
                assert_eq!(residents[0].birth_disp, edge_disp);
                assert_eq!(residents[1].birth_disp, 0.0);
                assert_eq!(
                    rib.translate_run(
                        super::super::witness::FollowRun {
                            row: 2,
                            cohort: coh.id,
                            lo: 0,
                            hi: u16::MAX,
                            dr: 1,
                        },
                        reach_at,
                        true,
                        &mut moved
                    ),
                    4
                );
                let caret = (if below { 1 } else { 4 }, 13);
                rib.plan(&ctx(at(reach_at, 1), &c, caret, 0.0));
                let flowed = rib
                    .cohorts()
                    .iter()
                    .find(|coh| coh.id == edge.cohort)
                    .expect("flow");
                assert_eq!(flowed.retract_col, Some(edge_col));
                assert_eq!(flowed.drain_right_first, below);
                let priced = if edge_disp == 0.0 {
                    FLOW_DRIFT_MIN_CELLS
                } else {
                    FLOW_DRIFT_MAX_CELLS
                };
                assert_eq!(flowed.flow_drift, priced.min(FLOW_DRIFT_MAX_SHARE * kept));
            }
        }
    }

    #[test]
    fn a_band_move_under_the_hand_carries_the_seam_and_its_floor_together() {
        let c = cfg(true, true);
        let g = geom();
        let read = |moved: bool| {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row: 2,
                col0: 100,
                n: 20,
                period_ms: 60,
                disp: 0.9,
            };
            type_keys(&mut rib, t0, keys, &c);
            let fold = at(t0, 20 * 60);
            let cx = ctx(fold, &c, (3, 0), 0.9);
            rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
            rib.plan(&cx);
            let keys = Keys {
                g,
                row: 3,
                col0: 0,
                n: 12,
                period_ms: 20,
                disp: 0.9,
            };
            type_keys(&mut rib, at(fold, 20), keys, &c);
            let row = if moved {
                rib.translate_band(0, 39, 1);
                4
            } else {
                3
            };
            let now = at(fold, 300);
            let cx = ctx(now, &c, (row, 12), 0.9);
            rib.plan(&cx);
            let mut sink = Sink::default();
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            (seam_far_tail(&sink, g, row, 12).0, rib.hand_floor_cell())
        };
        let (still, floor_still) = read(false);
        let (moved, floor_moved) = read(true);
        assert!(still > 0.0, "the seam holds the new line's far tail");
        assert_eq!(floor_still, Some((3, 0)));
        assert_eq!(
            floor_moved,
            Some((4, 0)),
            "the hand's floor rides the band move with its text"
        );
        assert!(
            (moved - still).abs() <= 0.01 * still,
            "the seam went with its row: {moved} on row 4 after the move, {still} on row 3 \
             without it"
        );
    }

    #[test]
    fn under_reduced_motion_a_flowing_band_asks_for_no_wake_before_its_fade() {
        let mut c = cfg(true, true);
        c.reduced_motion = true;
        let g = geom();
        let opens_ms = (FLOW_TOTAL_S * 1000.0 - REDUCED_MOTION_FADE_MS).round() as u64;
        for pause_ms in [60u64, 960] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row: 2,
                col0: 100,
                n: 20,
                period_ms: 60,
                disp: 0.2,
            };
            type_keys(&mut rib, t0, keys, &c);
            let fold = at(t0, 19 * 60 + pause_ms);
            let cx = ctx(fold, &c, (3, 0), 0.2);
            rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
            rib.plan(&cx);
            assert!(
                rib.cohorts().iter().any(|k| k.row == 2 && k.flow.is_some()),
                "pause {pause_ms} ms: the fold set the row flowing"
            );
            let earliest = at(fold, opens_ms) - super::super::TAIL_FLOOR;
            let mut ms = 100u64;
            while ms <= 1300 {
                let now = at(fold, ms);
                let cx = ctx(now, &c, (3, 0), 0.2);
                rib.plan(&cx);
                let d = rib.next_change_deadline(now).expect("the band is live");
                assert!(
                    d >= earliest,
                    "pause {pause_ms} ms: the read at +{ms} ms asks for a wake at +{} ms, before \
                     the fade opens at +{opens_ms} ms — nothing on the glass changes",
                    d.saturating_duration_since(fold).as_millis()
                );
                ms += 25;
            }
        }
    }

    #[test]
    fn a_shift_of_a_flowing_band_carries_its_frozen_fold_edge() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 60,
            n: 40,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let fold = at(t0, 40 * 60);
        let cx = ctx(fold, &c, (3, 0), 0.9);
        rib.on_event(&typed_move((2, 100), (3, 0)), fold, &cx);
        rib.plan(&cx);
        let cx = ctx(at(fold, 16), &c, (3, 0), 0.9);
        rib.plan(&cx);
        let flow = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2 && k.flow.is_some())
            .copied()
            .expect("the folded row flows");
        assert_eq!(flow.retract_col, Some(99), "frozen at the row's kept end");
        let back = at(fold, 64);
        let cx = ctx(back, &c, (2, 80), 0.9);
        rib.on_event(&nav((3, 0), (2, 80)), back, &cx);
        rib.plan(&cx);
        let key = at(fold, 80);
        let cx = ctx(key, &c, (2, 81), 0.9);
        rib.on_event(&typed(), key, &cx);
        let shift = super::super::witness::ShiftRun {
            row: 2,
            cohort: flow.id,
            lo: 80,
            hi: 99,
            dc: 1,
        };
        let mut moved = Vec::new();
        assert_eq!(rib.shift_run(shift, &mut moved), 20, "the tail moved");
        rib.plan(&cx);
        let after = rib
            .cohorts()
            .iter()
            .find(|k| k.id == flow.id)
            .copied()
            .expect("the band");
        assert!(after.flow.is_some(), "the row is still flowing");
        assert_eq!(
            after.retract_col,
            Some(100),
            "the frozen fold edge rides the shift with the glyph it bounds"
        );
        let mut sink = Sink::default();
        for ms in [96u64, 160, 320, 640] {
            let now = at(fold, ms);
            let cx = ctx(now, &c, (2, 81), 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            assert!(
                cell_lit(&sink.under, g, 2, 100),
                "+{ms} ms: the pushed glyph's column 100 is dark"
            );
        }
    }

    #[test]
    fn a_slow_hand_s_folded_row_flows_whole_whatever_it_paused_before_the_wrap() {
        let c = cfg(true, true);
        let g = geom();
        let flow = |pause_ms: u64| {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row: 2,
                col0: 90,
                n: 30,
                period_ms: 150,
                disp: 0.2,
            };
            type_keys(&mut rib, t0, keys, &c);
            let fold = at(t0, 29 * 150 + pause_ms);
            let cx = ctx(fold, &c, (3, 0), 0.2);
            rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
            rib.plan(&cx);
            assert!(
                rib.cohorts().iter().any(|k| k.row == 2 && k.flow.is_some()),
                "pause {pause_ms} ms: the fold set the row flowing"
            );
            let mut sink = Sink::default();
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let first = row_light(&sink, g, 2).expect("lit at the fold").0;
            let (mut prev, mut worst, mut share, mut dark) = (first, 0.0f32, None, None);
            let mut key_col = 0u16;
            let mut ms = 16u64;
            while ms <= 1700 {
                let now = at(fold, ms);
                if ms / 150 != (ms - 16) / 150 {
                    key_col += 1;
                    let kx = ctx(now, &c, (3, key_col), 0.2);
                    rib.on_event(&typed(), now, &kx);
                }
                let cx = ctx(now, &c, (3, key_col), 0.2);
                rib.plan(&cx);
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
                let cov = row_light(&sink, g, 2).map_or(0.0, |l| l.0);
                assert!(
                    dark.is_none() || cov == 0.0,
                    "pause {pause_ms} ms: lit again at +{ms} ms after going dark"
                );
                if cov == 0.0 && dark.is_none() {
                    dark = Some(ms as f32 / 1000.0);
                }
                worst = worst.max((prev - cov) / first);
                if ms == 1008 {
                    share = Some(cov / first);
                }
                prev = cov;
                ms += 16;
            }
            (
                dark.expect("the folded row goes dark"),
                share.expect("read at +1.0 s"),
                worst,
            )
        };
        let base = flow(60);
        let (dark0, share0, _) = base;
        for (pause_ms, (dark, share, worst)) in [(60u64, base), (600, flow(600)), (900, flow(900))]
        {
            assert!(
                (0.8 * FLOW_TOTAL_S..=FLOW_TOTAL_S + 0.017).contains(&dark),
                "pause {pause_ms} ms: LIFE: dark at +{dark} s"
            );
            assert!(
                worst <= 0.04,
                "pause {pause_ms} ms: NO CLIFF: a frame took {:.1} % of the fold frame's light",
                100.0 * worst
            );
            assert!(
                (dark - dark0).abs() <= 0.017 && (share - share0).abs() <= 0.01,
                "pause {pause_ms} ms: the flow's timeline depends on the pause: dark at +{dark} s \
                 and {share} of the light at +1.0 s, where the 60 ms pause reads +{dark0} s and \
                 {share0}"
            );
        }
    }

    #[test]
    fn a_program_round_trip_does_not_strand_the_band_under_a_typing_hand() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        let cx = ctx(at(last, 100), &c, (3, 0), 0.9);
        rib.on_event(&pty((2, 10), (3, 0)), at(last, 100), &cx);
        rib.plan(&cx);
        let coh = rib.cohorts().iter().find(|k| k.row == 2).expect("the band");
        assert!(coh.abandoned && coh.rejoinable);
        assert_eq!(
            coh.retract_col,
            Some(10),
            "retracts toward the hand, not the parked cursor"
        );
        let cx = ctx(at(last, 116), &c, (2, 10), 0.9);
        rib.on_event(&pty((3, 0), (2, 10)), at(last, 116), &cx);
        rib.plan(&cx);
        let k = at(last, 200);
        let cx = ctx(k, &c, (2, 11), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        let cohorts: Vec<_> = rib.cohorts().iter().filter(|k| k.row == 2).collect();
        assert_eq!(cohorts.len(), 1, "one band, taken back: {cohorts:?}");
        assert!(!cohorts[0].abandoned);
        assert_eq!((cohorts[0].col0, cohorts[0].col1), (2, 11));
        assert!(
            rib.cells()
                .iter()
                .filter(|l| l.row == 2)
                .all(|l| l.retract_at.is_none()),
            "no cell is left draining under the hand"
        );
    }

    const fn geom_owner() -> Geom {
        Geom {
            cw: 30,
            ch: 56,
            rows: 60,
            cols: 144,
            origin_x: 0,
            origin_y: 0,
            win_w: 4320,
            win_h: 3360,
            head: 0,
        }
    }

    const fn cfg_owner() -> Config {
        let mut c = cfg(true, true);
        c.intensity = 0.70;
        c
    }

    const OWNER_ROW: u16 = 56;

    fn type_owner_line(
        rib: &mut Ribbon,
        t0: Instant,
        c: &Config,
        disp: f32,
        period_ms: u64,
        pause_ms: u64,
    ) -> (u64, u16) {
        let g = geom_owner();
        let mut ms = 0u64;
        let mut col = 2u16;
        let mut last = 0u64;
        for (i, _) in "and I also seee an ugly gap".chars().enumerate() {
            let now = at(t0, ms);
            let cx = ctx_in(now, c, (OWNER_ROW, col + 1), disp, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            col += 1;
            last = ms;
            ms += period_ms;
            if i == 10 {
                ms += pause_ms;
            }
        }
        (last, col)
    }

    fn owner_frame(
        rib: &mut Ribbon,
        sink: &mut Sink,
        c: &Config,
        now: Instant,
        caret: u16,
        disp: f32,
    ) {
        let cx = ctx_in(now, c, (OWNER_ROW, caret), disp, geom_owner());
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    }

    fn owner_rows() -> std::ops::Range<i32> {
        let g = geom_owner();
        let top = i32::from(OWNER_ROW) * g.ch as i32;
        top - g.ch as i32 / 2..top + 2 * g.ch as i32
    }

    #[test]
    fn the_caret_block_stands_on_the_band_under_a_cold_hand_and_a_hot_one() {
        for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
            let c = cfg_owner();
            let g = geom_owner();
            let cw = g.cw as i32;
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let (last, caret) = type_owner_line(&mut rib, t0, &c, disp, 83, 700);
            let mut sink = Sink::default();
            owner_frame(&mut rib, &mut sink, &c, at(t0, last + 100), caret, disp);
            assert_eq!(
                rib.slabs_per_cell(),
                3,
                "{hand}: the composer line alone plans at three slabs"
            );
            let head_x0 = i32::from(caret - 1) * cw;
            let block_x1 = i32::from(caret + 1) * cw;
            let x0 = head_x0 - 3 * cw;
            let cens = column_census(&sink.under, x0..block_x1, owner_rows(), 8);
            let plateau = cens[..3 * cw as usize]
                .iter()
                .filter_map(|e| e.map(|(t, _, _)| t))
                .min()
                .expect("the cells before the head are lit");
            let head = &cens[3 * cw as usize..4 * cw as usize];
            let head_bot = head
                .iter()
                .filter_map(|e| e.map(|(_, b, _)| b))
                .max()
                .unwrap_or(0);
            let head_peak = head
                .iter()
                .filter_map(|e| e.map(|(_, _, p)| p))
                .max()
                .unwrap_or(0);
            let mut dark = Vec::new();
            for (k, e) in cens.iter().enumerate().skip(3 * cw as usize) {
                let x = x0 + k as i32;
                let Some((top, bot, peak)) = *e else {
                    dark.push(x);
                    continue;
                };
                assert!(
                    top <= plateau + 1,
                    "{hand}: column {x} tops out at {top}, under the plateau's {plateau}"
                );
                if x >= head_x0 + cw {
                    assert!(
                        bot >= head_bot - 1,
                        "{hand}: the foot under the block at column {x} ends at {bot}, the head's rail at {head_bot}"
                    );
                    assert!(
                        peak + 2 >= head_peak,
                        "{hand}: the band under the block at column {x} peaks at {peak}, the head at {head_peak}"
                    );
                }
            }
            assert!(
                dark.is_empty(),
                "{hand}: ground-dark columns between the head cell and the block's right edge: {} of them, from x {}",
                dark.len(),
                dark[0]
            );
            let hot = column_census(&sink.out, head_x0..head_x0 + cw, owner_rows(), 1);
            let unlit = hot.iter().filter(|e| e.is_none()).count();
            assert_eq!(
                unlit, 0,
                "{hand}: {unlit} of the head cell's columns carry no hot edge"
            );
        }
    }

    type FrameCensus = (u64, u16, Vec<Option<(i32, i32, u8)>>);

    fn owner_frames(
        rib: &mut Ribbon,
        t0: Instant,
        c: &Config,
        disp: f32,
        ms: std::ops::RangeInclusive<u64>,
        xs: std::ops::Range<i32>,
        mut step: impl FnMut(&mut Ribbon, u64) -> u16,
    ) -> Vec<FrameCensus> {
        let mut sink = Sink::default();
        let mut frames = Vec::new();
        let (mut ms, to_ms) = (*ms.start(), *ms.end());
        while ms <= to_ms {
            let caret = step(rib, ms);
            owner_frame(rib, &mut sink, c, at(t0, ms), caret, disp);
            frames.push((
                ms,
                caret,
                column_census(&sink.under, xs.clone(), owner_rows(), 8),
            ));
            ms += 16;
        }
        frames
    }

    const FADE_STEP_MAX: i32 = 36;

    #[test]
    fn the_block_stands_on_whole_light_after_a_jump_and_the_keys_that_follow_it() {
        let g = geom_owner();
        let cw = g.cw as i32;
        let row = OWNER_ROW;
        for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
            let c = cfg_owner();
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 10,
                period_ms: 83,
                disp,
            };
            type_keys(&mut rib, t0, keys, &c);
            let mut ms = 9 * 83 + 120;
            for (from, to) in [((row, 12u16), (row, 0u16)), ((row, 0), (row, 12))] {
                let now = at(t0, ms);
                let cx = ctx_in(now, &c, to, disp, g);
                rib.on_event(&nav(from, to), now, &cx);
                rib.plan(&cx);
                ms += 120;
            }
            let mut caret = 12u16;
            for _ in 0..3 {
                ms += 83;
                caret += 1;
                let now = at(t0, ms);
                let cx = ctx_in(now, &c, (row, caret), disp, g);
                rib.on_event(&typed(), now, &cx);
                rib.plan(&cx);
            }
            ms += 300;
            let mut sink = Sink::default();
            owner_frame(&mut rib, &mut sink, &c, at(t0, ms), caret, disp);
            let x0 = i32::from(caret - 5) * cw;
            let x1 = i32::from(caret + 1) * cw;
            let cens = column_census(&sink.under, x0..x1, owner_rows(), 8);
            let plateau = cens[..4 * cw as usize]
                .iter()
                .filter_map(|e| e.map(|(_, _, p)| p))
                .max()
                .expect("the band before the head is lit");
            let show = |lo: usize| {
                (0..3)
                    .map(|j| cens[lo + j * (cw as usize / 3)].map_or(0, |(_, _, p)| i32::from(p)))
                    .collect::<Vec<_>>()
            };
            for (k, e) in cens.iter().enumerate().skip(4 * cw as usize) {
                let x = x0 + k as i32;
                let peak = e.map_or(0, |(_, _, p)| p);
                assert!(
                    i32::from(plateau) - i32::from(peak) <= 2,
                    "{hand}: device column {x} reads {peak} against the band's {plateau} — the head cell is {:?} and the block's own {:?}",
                    show(4 * cw as usize),
                    show(5 * cw as usize)
                );
            }
        }
    }

    fn assert_block_on_band(frames: &[FrameCensus], x0: i32, cw: i32, hold_x1: i32, leg: &str) {
        for (i, (ms, caret, cens)) in frames.iter().enumerate() {
            let cx0 = i32::from(*caret) * cw;
            let dark: Vec<i32> = (cx0..cx0 + cw)
                .filter(|x| cens[(x - x0) as usize].is_none())
                .collect();
            assert!(
                dark.is_empty(),
                "{leg}, +{ms} ms, caret {caret}: {} ground-dark columns under the block, from x {}",
                dark.len(),
                dark[0]
            );
            if i == 0 {
                continue;
            }
            let prev = &frames[i - 1].2;
            let level = |e: &Option<(i32, i32, u8)>| e.map_or(0, |(_, _, p)| i32::from(p));
            for (k, (now, was)) in cens.iter().zip(prev.iter()).enumerate() {
                let x = x0 + k as i32;
                let (now, was) = (level(now), level(was));
                assert!(
                    now - was <= 2,
                    "{leg}, +{ms} ms: column {x} rose {was} -> {now} in one frame"
                );
                if (cx0..hold_x1).contains(&x) {
                    assert!(
                        was - now <= FADE_STEP_MAX,
                        "{leg}, +{ms} ms: column {x} fell {was} -> {now} in one frame, faster than the fade"
                    );
                }
            }
        }
    }

    #[test]
    fn the_block_stays_on_the_band_while_the_cell_under_it_leaves_and_when_it_is_gone() {
        let g = geom_owner();
        let cw = g.cw as i32;
        let row = OWNER_ROW;
        let fade_ms = (RETRACT_FADE_S * 1000.0) as u64;
        let melt_ms = (RETIRE_MELT_S * 1000.0) as u64;
        for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
            for (period, erases, leg) in [
                (0u64, 1u16, "one Backspace"),
                (33, 4, "Backspace held at 30/s"),
            ] {
                let leg = format!("{hand} hand, {leg}");
                let c = cfg_owner();
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                let keys = Keys {
                    g,
                    row,
                    col0: 2,
                    n: 8,
                    period_ms: 83,
                    disp,
                };
                type_keys(&mut rib, t0, keys, &c);
                let ms0 = 7 * 83 + 300;
                let last_erase = ms0 + u64::from(erases - 1) * period;
                let mut done = 0u16;
                let mut caret = 10u16;
                let x0 = i32::from(10 - erases) * cw;
                let frames = owner_frames(
                    &mut rib,
                    t0,
                    &c,
                    disp,
                    ms0 - 16..=last_erase + fade_ms + 200,
                    x0..11 * cw,
                    |rib, ms| {
                        while done < erases && ms >= ms0 + u64::from(done) * period {
                            caret -= 1;
                            let now = at(t0, ms);
                            let cx = ctx_in(now, &c, (row, caret), disp, g);
                            rib.on_event(&Event::Erase, now, &cx);
                            done += 1;
                        }
                        caret
                    },
                );
                assert_block_on_band(&frames, x0, cw, 10 * cw, &leg);
                assert!(
                    !rib.cells().iter().any(|l| l.row == row && l.col >= caret),
                    "{leg}: the erased cells retired inside the window"
                );
                assert_eq!(caret, 10 - erases, "{leg}: every erase landed");
            }
            {
                let leg = format!("{hand} hand, own cells retired under the caret");
                let c = cfg_owner();
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                let keys = Keys {
                    g,
                    row,
                    col0: 2,
                    n: 8,
                    period_ms: 83,
                    disp,
                };
                type_keys(&mut rib, t0, keys, &c);
                let mut ms = 7 * 83 + 300;
                let cx = ctx_in(at(t0, ms), &c, (row, 7), disp, g);
                rib.on_event(&nav((row, 10), (row, 7)), at(t0, ms), &cx);
                rib.plan(&cx);
                ms += 300;
                let cx = ctx_in(at(t0, ms), &c, (row, 8), disp, g);
                rib.on_event(&typed(), at(t0, ms), &cx);
                rib.plan(&cx);
                let ms0 = ms + 60;
                let ids: Vec<(u16, u16, Instant)> = rib
                    .cells()
                    .iter()
                    .filter(|l| l.row == row && l.col >= 8)
                    .map(|l| (l.row, l.col, l.born))
                    .collect();
                let mut retired = false;
                let x0 = 7 * cw;
                let frames = owner_frames(
                    &mut rib,
                    t0,
                    &c,
                    disp,
                    ms0 - 16..=ms0 + melt_ms + 200,
                    x0..11 * cw,
                    |rib, ms| {
                        if !retired && ms >= ms0 {
                            assert_eq!(
                                rib.retire_cells(&ids, at(t0, ms)),
                                2,
                                "{leg}: the shifted cells retire"
                            );
                            retired = true;
                        }
                        8
                    },
                );
                assert_block_on_band(&frames, x0, cw, 9 * cw, &leg);
                assert!(
                    !rib.cells().iter().any(|l| l.row == row && l.col >= 8),
                    "{leg}: the retired cells left the pool inside the window"
                );
            }
            {
                let leg = format!("{hand} hand, a wake's cell retired under the caret");
                let c = cfg_owner();
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                let keys = Keys {
                    g,
                    row,
                    col0: 2,
                    n: 3,
                    period_ms: 83,
                    disp,
                };
                type_keys(&mut rib, t0, keys, &c);
                let mut ms = 2 * 83 + 200;
                let cx = ctx_in(at(t0, ms), &c, (row, 9), disp, g);
                rib.on_event(&nav((row, 5), (row, 9)), at(t0, ms), &cx);
                rib.plan(&cx);
                ms += 120;
                let cx = ctx_in(at(t0, ms), &c, (row, 5), disp, g);
                rib.on_event(&nav((row, 9), (row, 5)), at(t0, ms), &cx);
                rib.plan(&cx);
                let typed_cohort = rib
                    .cells()
                    .iter()
                    .find(|l| l.row == row && l.col == 4)
                    .expect("the word's last cell")
                    .cohort;
                let ids: Vec<(u16, u16, Instant)> = rib
                    .cells()
                    .iter()
                    .filter(|l| l.row == row && l.col >= 5)
                    .map(|l| (l.row, l.col, l.born))
                    .collect();
                assert!(
                    rib.cells()
                        .iter()
                        .any(|l| l.row == row && l.col == 5 && l.cohort != typed_cohort),
                    "{leg}: the wake is a cohort of its own"
                );
                let ms0 = ms + 80;
                let mut retired = false;
                let x0 = 4 * cw;
                let frames = owner_frames(
                    &mut rib,
                    t0,
                    &c,
                    disp,
                    ms0 - 16..=ms0 + melt_ms + 200,
                    x0..7 * cw,
                    |rib, ms| {
                        if !retired && ms >= ms0 {
                            assert!(
                                rib.retire_cells(&ids, at(t0, ms)) >= 5,
                                "{leg}: the wake retires"
                            );
                            retired = true;
                        }
                        5
                    },
                );
                assert_block_on_band(&frames, x0, cw, 6 * cw, &leg);
                assert!(
                    !rib.cells().iter().any(|l| l.row == row && l.col >= 5),
                    "{leg}: the retired wake left the pool inside the window"
                );
            }
        }
    }

    fn column_peaks(under: &[GlowQuad], xs: std::ops::Range<i32>) -> Vec<i32> {
        column_census(under, xs, owner_rows(), 1)
            .iter()
            .map(|e| e.map_or(0, |(_, _, p)| i32::from(p)))
            .collect()
    }

    fn sliver_columns(peaks: &[i32], cw: usize, skip: usize, tol: i32) -> Vec<usize> {
        (skip..peaks.len())
            .filter(|&i| {
                let left = peaks[i.saturating_sub(cw)..i]
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0);
                let right = peaks[i + 1..(i + 1 + cw).min(peaks.len())]
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0);
                peaks[i] + tol < left && peaks[i] + tol < right
            })
            .collect()
    }

    #[test]
    fn a_boundary_two_live_runs_share_carries_the_older_side_s_light() {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: OWNER_ROW,
                col0: 2,
                n: 6,
                period_ms: 83,
                disp: 0.7,
            },
            &c,
        );
        let hop = at(t0, 5 * 83 + 120);
        let cx = ctx_in(hop, &c, (OWNER_ROW, 11), 0.7, g);
        rib.on_event(&nav((OWNER_ROW, 8), (OWNER_ROW, 11)), hop, &cx);
        rib.plan(&cx);
        let mut sink = Sink::default();
        owner_frame(&mut rib, &mut sink, &c, at(hop, 300), 11, 0.7);
        assert_eq!(
            rib.cohorts().iter().filter(|k| k.row == OWNER_ROW).count(),
            2,
            "the word and its wake are two cohorts: {:?}",
            rib.cohorts()
        );
        assert_eq!(rib.runs.len(), 2, "…and two runs abutting at column 8");
        let x0 = 2 * cw;
        let x1 = 12 * cw;
        let peaks = column_peaks(&sink.under, x0..x1);
        let dark: Vec<i32> = peaks
            .iter()
            .enumerate()
            .filter(|(_, p)| **p == 0)
            .map(|(i, _)| x0 + i as i32)
            .collect();
        assert!(dark.is_empty(), "dark columns inside the band: {dark:?}");
        let slivers: Vec<i32> = sliver_columns(&peaks, cw as usize, cw as usize, 2)
            .into_iter()
            .map(|i| x0 + i as i32)
            .collect();
        assert!(
            slivers.is_empty(),
            "columns darker than both sides by more than a dither step: {slivers:?} (peaks {:?})",
            &peaks[5 * cw as usize..7 * cw as usize]
        );
        let tops = column_census(&sink.under, x0 + cw..x1, owner_rows(), 8);
        for w in tops.windows(2) {
            let (Some(a), Some(b)) = (w[0], w[1]) else {
                continue;
            };
            assert!(
                (a.0 - b.0).abs() <= 1,
                "the top edge steps {} rows between adjacent columns ({} → {})",
                (a.0 - b.0).abs(),
                a.0,
                b.0
            );
        }
    }

    #[test]
    fn the_feather_grows_in_as_a_drained_neighbour_goes_never_in_one_frame() {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let first = "and I also ";
        let mut ms = 0u64;
        let mut col = 2u16;
        for _ in first.chars() {
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, (OWNER_ROW, col + 1), 0.7, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            col += 1;
            ms += 83;
        }
        let park = at(t0, ms + 20);
        let cx = ctx_in(park, &c, (OWNER_ROW, col), 0.7, g);
        rib.on_event(&typed_move((OWNER_ROW, col), (OWNER_ROW, 2)), park, &cx);
        rib.on_event(&typed_move((OWNER_ROW, 2), (OWNER_ROW, col)), park, &cx);
        rib.plan(&cx);
        let mut keys: Vec<(u64, u16)> = Vec::new();
        let mut k = ms + 70;
        for _ in "seee an ugly gap".chars() {
            keys.push((k, col + 1));
            col += 1;
            k += 83;
        }
        let last_key = keys.last().map_or(k, |e| e.0);
        let mut sink = Sink::default();
        let x0 = 2 * cw;
        let x1 = 30 * cw;
        let mut prev: Option<Vec<i32>> = None;
        let mut next_key = 0usize;
        let mut caret = col - 16;
        let mut worst_drop = (0i32, 0i32, 0u64);
        let mut slivers_seen = Vec::new();
        let mut t = ms + 20;
        let mut last_key_at = 0u64;
        let until = last_key + (PHRASE_REST_MIN_S * 1000.0) as u64;
        while t <= until {
            while next_key < keys.len() && keys[next_key].0 <= t {
                let (kt, kc) = keys[next_key];
                let now = at(t0, kt);
                let cx = ctx_in(now, &c, (OWNER_ROW, kc), 0.7, g);
                rib.on_event(&typed(), now, &cx);
                caret = kc;
                last_key_at = kt;
                next_key += 1;
            }
            owner_frame(&mut rib, &mut sink, &c, at(t0, t), caret, 0.7);
            let peaks = column_peaks(&sink.under, x0..x1);
            if let Some(p) = &prev {
                for (i, (&a, &b)) in p.iter().zip(&peaks).enumerate() {
                    if a - b > worst_drop.0 {
                        worst_drop = (a - b, x0 + i as i32, t);
                    }
                }
            }
            let settled = t.saturating_sub(last_key_at) >= 60;
            if settled {
                let live_col = rib
                    .cells()
                    .iter()
                    .filter(|l| !l.leaving())
                    .map(|l| l.col)
                    .min()
                    .unwrap_or(2);
                let first = (usize::from(live_col) - 2) * cw as usize;
                let live = &peaks[first.min(peaks.len())..];
                for i in sliver_columns(live, cw as usize, cw as usize, 2) {
                    slivers_seen.push((t, x0 + (first + i) as i32, live[i]));
                }
            }
            prev = Some(peaks);
            t += 16;
        }
        assert!(
            rib.cells().iter().all(|l| l.col >= 13),
            "by the end of the grace the drained cells have retired"
        );
        assert!(
            worst_drop.0 <= 36,
            "a column's light fell {} levels in one 16 ms frame (x {}, +{} ms): the feather popped in",
            worst_drop.0,
            worst_drop.1,
            worst_drop.2
        );
        assert!(
            slivers_seen.is_empty(),
            "columns darker than both sides inside the live band: {:?}",
            &slivers_seen[..slivers_seen.len().min(8)]
        );
    }

    #[test]
    fn the_attach_and_the_seam_law_hold_at_one_slab_per_cell() {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let mut ms = 0u64;
        for row in 50..53u16 {
            type_keys(
                &mut rib,
                at(t0, ms),
                Keys {
                    g,
                    row,
                    col0: 1,
                    n: 100,
                    period_ms: 1,
                    disp: 0.7,
                },
                &c,
            );
            ms += 100;
        }
        let mut col = 2u16;
        for _ in "and I also seee an ugly gap".chars() {
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, (OWNER_ROW, col + 1), 0.7, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            col += 1;
            ms += 20;
        }
        let mut sink = Sink::default();
        owner_frame(&mut rib, &mut sink, &c, at(t0, ms - 20 + 100), col, 0.7);
        assert_eq!(rib.slabs_per_cell(), 1, "327 retina cells plan at one slab");
        assert!(
            sink.under.len() + 2 > RIBBON_QUAD_BUDGET - 200,
            "the frame is at the budget's edge: {} quads",
            sink.under.len()
        );
        let head_x0 = i32::from(col - 1) * cw;
        let block_x1 = i32::from(col + 1) * cw;
        let cens = column_census(&sink.under, head_x0 - 3 * cw..block_x1, owner_rows(), 8);
        let plateau = cens[..3 * cw as usize]
            .iter()
            .filter_map(|e| e.map(|(t, _, _)| t))
            .min()
            .expect("lit");
        for (k, e) in cens.iter().enumerate().skip(3 * cw as usize) {
            let x = head_x0 - 3 * cw + k as i32;
            let (top, _, _) = e.unwrap_or_else(|| panic!("column {x} is ground-dark at one slab"));
            assert!(
                top <= plateau + 1,
                "column {x} tops out at {top} under the plateau {plateau}"
            );
        }
        let x0 = 2 * cw;
        let peaks = column_peaks(&sink.under, x0..block_x1);
        let slivers = sliver_columns(&peaks, cw as usize, cw as usize, 2);
        assert!(slivers.is_empty(), "slivers at one slab: {slivers:?}");
        let tops = column_census(&sink.under, x0 + cw..block_x1, owner_rows(), 8);
        let worst = tops
            .windows(2)
            .filter_map(|w| Some((w[0]?.0 - w[1]?.0).abs()))
            .max()
            .unwrap_or(0);
        assert!(
            worst <= 3,
            "the top edge steps {worst} rows at one slab per cell"
        );
    }

    fn capped_wide_run(head_col: u16, head: usize, stream_dir: i8) -> (Vec<GlowQuad>, u16) {
        let c = cfg_owner();
        let mut g = geom_owner();
        g.cols = 180;
        g.win_w = 5400;
        let now = Instant::now();
        let cx = ctx_in(now, &c, (OWNER_ROW, head_col), 0.7, g);
        let mut rib = Ribbon::new();
        rib.plan(&cx); // Sync the real theme ink table.
        rib.slabs = 1;
        rib.plan = (0..=179)
            .map(|col| Segment {
                x: col as f32 * g.cw as f32,
                spine: (f32::from(OWNER_ROW) + 1.0) * g.ch as f32,
                up: g.ch as f32,
                dn: 0.30 * g.ch as f32,
                t: 0.4,
                cov: 220,
            })
            .collect();
        rib.runs.push(Run {
            row: OWNER_ROW,
            col0: 0,
            col1: 178,
            lo: 0,
            hi: rib.plan.len(),
            head_col,
            head,
            wet: true,
            at_caret: true,
            born: now,
            stream_dir,
            hold: 0.0,
        });

        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        assert!(
            sink.under.len() + 2 >= RIBBON_QUAD_BUDGET,
            "precondition: the real quad ceiling was reached ({})",
            sink.under.len()
        );
        (sink.under, g.cw as u16)
    }

    #[test]
    fn a_capped_rail_does_not_cut_the_body_of_an_adjacent_run() {
        let c = cfg_owner();
        let g = geom_owner();
        let now = Instant::now();
        let cx = ctx_in(now, &c, (OWNER_ROW, 130), 0.7, g);
        let mut rib = Ribbon::new();
        rib.plan(&cx); // Sync the real theme ink table.
        rib.slabs = 1;
        rib.plan = (0..=130)
            .map(|col| Segment {
                x: col as f32 * g.cw as f32,
                spine: (f32::from(OWNER_ROW) + 1.0) * g.ch as f32,
                up: g.ch as f32,
                dn: 0.30 * g.ch as f32,
                t: 0.4,
                cov: 220,
            })
            .collect();
        rib.runs.extend([
            Run {
                row: OWNER_ROW,
                col0: 20,
                col1: 129,
                lo: 20,
                hi: 131,
                head_col: 129,
                head: 130,
                wet: true,
                at_caret: true,
                born: now,
                stream_dir: -1,
                hold: 0.0,
            },
            Run {
                row: OWNER_ROW,
                col0: 0,
                col1: 19,
                lo: 0,
                hi: 21,
                head_col: 19,
                head: 20,
                wet: true,
                at_caret: false,
                born: now,
                stream_dir: -1,
                hold: 0.0,
            },
        ]);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        assert!(
            sink.under.len() + 2 >= RIBBON_QUAD_BUDGET,
            "precondition: body plus rail must fill the budget ({})",
            sink.under.len()
        );
        assert!(sink.under.len() <= RIBBON_QUAD_BUDGET);
        assert!(cell_lit(&sink.under, g, OWNER_ROW, 129));
        assert!(
            (0..130).all(|col| cell_lit(&sink.under, g, OWNER_ROW, col)),
            "the vivid rail consumed body quads and opened a dark gap inside the typed row"
        );

        let mut uncapped = rib.clone();
        uncapped.plan.truncate(61);
        uncapped.runs[0].col1 = 59;
        uncapped.runs[0].hi = 61;
        uncapped.runs[0].head_col = 59;
        uncapped.runs[0].head = 60;
        let short_cx = ctx_in(now, &c, (OWNER_ROW, 60), 0.7, g);
        let mut joint = Sink::default();
        uncapped.emit(&short_cx, &mut joint.frame());
        let mut current = uncapped.clone();
        current.runs.truncate(1);
        let mut current_sink = Sink::default();
        current.emit(&short_cx, &mut current_sink.frame());
        let mut older = uncapped;
        older.runs.remove(0);
        let mut older_sink = Sink::default();
        older.emit(&short_cx, &mut older_sink.frame());
        let seam_x = (20 * g.cw) as u16;
        assert!(current_sink.under.iter().all(|q| q.x >= seam_x));
        assert!(older_sink.under.iter().all(|q| q.x + q.w <= seam_x));
        let mut per_run = current_sink.under;
        per_run.extend(older_sink.under);
        assert_eq!(joint.under, per_run, "uncapped emission changed");
    }

    #[test]
    fn a_capped_run_keeps_the_light_at_its_head_and_sheds_the_distant_tail() {
        for (what, head_col, head, stream_dir) in [
            ("a leftward wake", 0u16, 0usize, 1i8),
            ("a midline edit", 30, 31, -1),
        ] {
            let (quads, cw) = capped_wide_run(head_col, head, stream_dir);
            assert!(
                quads
                    .iter()
                    .any(|q| q.x >= head_col * cw && q.x < (head_col + 1) * cw),
                "{what}: the cell at the caret went dark under the cap"
            );
            assert!(
                quads.iter().any(|q| q.x < cw),
                "{what}: the run's left side must connect to the head"
            );
            assert!(
                !quads.iter().any(|q| q.x >= 178 * cw),
                "{what}: the distant right tail should pay the budget, not the head"
            );
        }
    }

    #[test]
    fn attach_cell_follows_scroll_band_and_hand_lift() {
        let now = Instant::now();
        let c = cfg(true, true);
        let g = geom();
        let mut rib = Ribbon::new();

        let cx5 = ctx_in(now, &c, (5, 5), 0.7, g);
        rib.on_event(&typed(), now, &cx5);
        rib.plan(&cx5);
        assert_eq!(rib.attach, Some((5, 5)));

        rib.translate_scroll(2);
        assert_eq!(rib.attach, Some((3, 5)));
        assert!(rib.attach_floor.is_none());

        rib.translate_band(2, 6, 1);
        assert_eq!(rib.attach, Some((4, 5)));

        rib.lift_hand(4, 3);
        assert_eq!(rib.attach, Some((3, 5)));
        rib.carry = Some((
            SoftWrap {
                origin: (3, 79),
                landing: (4, 2),
                word_col0: 74,
            },
            now,
        ));
        rib.lift_hand(4, 5);
        assert_eq!(
            rib.carry.map(|(sw, _)| (sw.origin, sw.landing)),
            Some(((3, 79), (5, 2)))
        );

        let t_next = now + Duration::from_millis(20);
        let cx3 = ctx_in(t_next, &c, (3, 6), 0.7, g);
        rib.on_event(&typed(), t_next, &cx3);
        let laid = rib
            .cells()
            .iter()
            .find(|cell| cell.row == 3 && cell.col == 5)
            .expect("cell laid at lifted caret");
        assert!(
            laid.attack_at < laid.born,
            "typed cell at translated attach must inherit spent attack"
        );
    }

    #[test]
    fn chained_word_kills_retract_right_to_left_without_a_gap() {
        let t0 = Instant::now();
        let c = cfg(true, true);
        let g = geom();
        let mut rib = Ribbon::new();
        let row = 5u16;
        let text = "so that when you fix ";
        let mut col = 2u16;
        let mut t = t0;
        for ch in text.chars() {
            t += Duration::from_millis(40);
            col += 1;
            let cx = ctx_in(t, &c, (row, col), 0.7, g);
            let ev = if ch == ' ' { typed_space() } else { typed() };
            rib.on_event(&ev, t, &cx);
            rib.plan(&cx);
        }
        let kills = [4u16, 4, 5, 5, 3];
        for cells in kills {
            for step_ms in (16..=160).step_by(16) {
                let frame_t = t + Duration::from_millis(step_ms);
                let cx = ctx_in(frame_t, &c, (row, col), 0.7, g);
                rib.plan(&cx);
                let mut lit_cols: Vec<u16> = rib
                    .lit_segments()
                    .map(|s| (s.x / g.cw as f32).floor() as u16)
                    .collect();
                lit_cols.sort_unstable();
                lit_cols.dedup();
                if let (Some(&lo), Some(&hi)) = (lit_cols.first(), lit_cols.last()) {
                    for check_col in lo..=hi {
                        assert!(
                            lit_cols.binary_search(&check_col).is_ok(),
                            "chained word-kill left an unlit gap at col {check_col} inside {lo}..={hi} ({lit_cols:?})"
                        );
                    }
                }
            }
            t += Duration::from_millis(165);
            let from = (row, col);
            col -= cells;
            let to = (row, col);
            let cx_before = ctx_in(t, &c, from, 0.7, g);
            rib.on_event(
                &Event::Kill {
                    cells,
                    scope: super::super::KillScope::Word,
                },
                t,
                &cx_before,
            );
            let cx_after = ctx_in(t, &c, to, 0.7, g);
            rib.on_event(
                &Event::Move {
                    from,
                    to,
                    licence: Licence::Typed,
                    dir: Dir::of(-i32::from(cells), 0),
                },
                t,
                &cx_after,
            );
        }
    }
}

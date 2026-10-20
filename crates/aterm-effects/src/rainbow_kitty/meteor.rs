// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The meteor: the one mark in the rainbow kitty that streaks. On the frame the
//! caret lands, a white-hot head is already [`super::timing::FLIGHT_P0`] of the
//! way across, wearing a rainbow corona; its train carries the whole spectrum
//! on every frame, laid onto the row as the head passes so the colours stream
//! backwards, and dies white-first. The arrival is a terminal flash and an
//! impact graded by distance ([`impact`]); everything is off glass by
//! `T + 600`.
//!
//! * **Train** ([`arc_gain`], [`Meteor::arc`]): a whole sweep fits inside the
//!   frame-0 span whatever the distance, fixed to the row rather than the
//!   head; wider at the head ([`W_BASE_CH`]), feathered at the tail
//!   ([`WIDTH_FLOOR_SHARE`]), saturated over its length
//!   ([`COLOUR_FALLOFF_SHARE`]).
//! * **Corona** ([`COMA_PETALS`]): a white-hot nucleus orbited by seven
//!   petals of the named stops.
//! * **Flash** ([`draw_flash`]): the caret cell and its neighbours burst white
//!   and die into the spectrum; over a glyph the coverage stays at the
//!   transient cap so the text stays legible.
//! * **Starburst** ([`draw_burst`]): isotropic tapered lances at true screen
//!   angles round the caret, rooted clear of its disc ([`BURST_ROOT_CH`]),
//!   plus jets along the flight line ([`JET_N_PER_SIDE`]). What reads as
//!   radial is uniform angular coverage in screen space, so no direction is
//!   an ellipse parameter.
//! * **Sparks** ([`draw_sparks`]): small haloed four-point stars that climb,
//!   hang and fall; a spark never lands left of the leftmost column the hand
//!   has held on its row (`spark_room`).
//! * **Palette**: every landing mark reads one walk, the band's own
//!   ([`LandingWalk`], [`landing_ink`]), so the star is the colour of the
//!   light it lands on and nothing slides.
//! * Under reduced motion the landing is the flash and the colours, static,
//!   on the theme's one linear fade.
//!
//! The impact magnitude [`impact`] scales the burst's radius and life, the
//! fan's count and reach; a bigger landing is wider and longer, never taller
//! or fatter. Design of record: `docs/design/RAINBOW-KITTY-V2.md` §6.
//!
//! ## Laws
//!
//! * **Frame 0 (T2).** `t = 0` is the frame the caret is first observed at
//!   its landing: the head is at `FLIGHT_P0` of the path, the train is lit
//!   back to the launch cell, the caret flares and every shed fragment with
//!   `s < 0.28·L` is born. Light exists at both ends on that frame.
//! * **The meteor does not read the field (C5, D4).** It walks [`Meteor::arc`], the
//!   classic lay rate continued backwards from the caret's own stop,
//!   reflected ([`tri`]) and never clamped.
//! * [`Meteors::emit`] refuses a path shorter than one device pixel; that
//!   tests the static path length `L`, which is at least `cw` for any flight
//!   the trigger admits, so it never blanks a frame of a flight.
//!
//! ## Drawn here vs minted here
//!
//! This file draws the train, shoulder, corona, nucleus, flash, pin, burst
//! and sparks. The shed fragments, the landing fan and the hop's
//! mini-fan are stars, built only by [`super::stardust`]; this file decides
//! their stations, timing, reach and count and hands them over through
//! [`Meteors::sow_into`] ([`Stardust::sow_shed`], [`Stardust::sow_fan`],
//! [`Stardust::sow_mini_fan`]).

use std::mem;
use std::sync::OnceLock;

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::{
    BeamClip, BeamVertex, GlowBlend, GlowQuad, HaloMode, RainHalo, RibbonVertex, comet_beam,
    premul_rgb, ribbon_beam, ribbon_beam_v,
};

use crate::cursor_glow::{BandPx, Geom, InkRole};
use crate::effect_util::{lerp_rgb, push_fx_quad, push_fx_rect, push_twinkle_star};
use crate::spectrum::{
    SPECTRUM_STOPS, spectrum, spectrum_snap, spectrum_snap_index, spectrum_stop,
};

use super::ribbon::{
    BED_INK_LUT_LEN, SLABS_PER_CELL, WALK_FAST_CELLS, WALK_LAY_RATE, hot_edge_ink, paper_caret_ink,
    paper_ink, paper_mark_ink, walk_pace, walk_t,
};
// `mix32`/`hash01`: the family's one integer mixer (`lowbias32`) and its
// `[0, 1)` draw. The meteor hashes only what it decides itself — the spawn
// seed and the shed stations; every per-star draw is the sky's.
use super::stardust::{
    FAN_RISE_MAX_CH, FanSow, GlyphProbe, ShedSow, Stardust, hash01, mix32, push_halo_quads,
};
use super::timing::{
    CHROMA_CULL_ALPHA, FLIGHT_ENTER_EXP, FLIGHT_MAX_LIVE, FLIGHT_OFF_GLASS_MS, FLIGHT_P0,
    FLIGHT_RETIRE_MS, JUMP_COV_CEIL, JUMP_MIN_CELLS, MINI_FAN_MAX_CELLS, MINI_FAN_MIN_CELLS,
    REDUCED_MOTION_FADE_MS, SHED_MAX, STAR_CULL_ALPHA, TRANSIENT_STAR_COV_CEIL, clamp01,
    enter_at_speed, flight, impact, shed_n, smoothstep01, spend, suck_in,
};
use super::{Cadence, Ctx, Dir, Event, Frame, Licence};

// ---- §6.3 width -----------------------------------------------------------

/// Constant term of `w = ch·(0.44 + 0.34·mom)·(1 + 0.5·grade)` (§6.3): a cold
/// short meteor is 7.9 px at `ch = 18`; the tail is feathered to match
/// ([`WIDTH_FLOOR_SHARE`]).
pub const W_BASE_CH: f32 = 0.44;

/// Momentum term of the width law (§6.3): hot is 12.6 px at `ch = 18`.
pub const W_MOM_CH: f32 = 0.34;

/// Distance buys length and speed, not width (§6.3): the grade is capped at
/// ×1.5, so a hot 34-cell meteor is 19 px (1.05 `ch`).
pub const W_GRADE_GAIN: f32 = 0.5;

/// Cells at which `grade` saturates: `grade = clamp(cells/34, 0, 1)` (§6.3).
pub const GRADE_CELLS: f32 = 34.0;

/// Base head coverage before the brightness grade (§6.3); the graded value is
/// `min(118·(1 + 0.5·grade), 160)` and 160 is
/// [`super::timing::JUMP_COV_CEIL`] (L3). This is the white heat — the
/// shoulder, the white train layer and the nucleus (§6.5 layers 2, 3, 7).
pub const HEAD_COV_BASE: f32 = TRANSIENT_STAR_COV_CEIL;

/// Brightness grade gain (§6.3): brightness is graded by distance, width is
/// not. Dark theme only; §6.10 has no brightness grade on light (the arm is
/// legibility-bounded).
pub const BRIGHT_GRADE_GAIN: f32 = 0.5;

/// Coverage of the colour train's head (§6.5 layer 1): the transient ceiling,
/// not graded. The colour layer starts here and decays
/// (`cov(0.61·L)/cov(0.06·L) = e⁻¹`); the grade is spent on the white heat and
/// nucleus ([`HEAD_COV_BASE`] × `bright`), so white wins where the eye reads
/// the head at every `s`, and the train never brightens behind the shoulder.
pub const COLOUR_TRAIN_COV: f32 = TRANSIENT_STAR_COV_CEIL;

// ---- §6.4 the arc ---------------------------------------------------------

/// `tri(x) = 1 − |1 − (x mod 2)|` (§6.4): the triangle wave that reflects the
/// continued walk instead of clamping it, so an 80-cell meteor (2.2 sweeps)
/// reads as a rainbow over a fresh run or a dead prompt, in both directions.
#[inline]
#[must_use]
pub fn tri(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    1.0 - (1.0 - x.rem_euclid(2.0)).abs()
}

/// How many `t`-units of the reflected walk the train carries inside its
/// frame-0 span (`FLIGHT_P0·L`), whatever the distance. 1.5 rather than 1.0
/// because `tri` reflects: `x → 1 → 0` spans the whole spectrum within 1.5
/// units from any phase, so every frame of every flight is a rainbow
/// (owner, 2026-09-08).
pub const ARC_FRAME0_SWEEPS: f32 = 1.5;

/// The multiplier on the classic lay rate that puts [`ARC_FRAME0_SWEEPS`]
/// inside the frame-0 span: `max(1, sweeps / (p₀·cells/36))`. Never below
/// one — a flight long enough to carry the sweep at the walk's own rate
/// (≥ 129 cells) keeps the walk's rate, so the ribbon's pacing and the
/// meteor's agree wherever they can.
#[inline]
#[must_use]
pub fn arc_gain(cells: f32) -> f32 {
    if !cells.is_finite() || cells <= 0.0 {
        return 1.0;
    }
    (ARC_FRAME0_SWEEPS / (FLIGHT_P0 * cells * WALK_LAY_RATE)).max(1.0)
}

// ---- the landing's palette ------------------------------------------------

/// The landing's walk off any band (a landing on a dark row, a party at the
/// caret): `tri(t_land + dx/16)`, the folded arc position `dx_cells` right of
/// the landing, which is what a wake laid there walks. A landing on a band
/// reads the band's own walk instead ([`LandingWalk`]), so the splash is
/// aligned with the rainbow (owner, 2026-09-14).
///
/// * `t_land` is latched raw at the spawn edge ([`Meteor::t_land`]) and
///   folded once here, so at `dx = 0` it is the colour the ribbon draws for
///   that cell;
/// * the walk runs with the column, as the band's does, never with the
///   flight;
/// * it reflects through [`tri`], so it never wraps violet into red.
#[inline]
#[must_use]
pub fn landing_field(t_land: f32, dx_cells: f32) -> f32 {
    if !dx_cells.is_finite() {
        return tri(t_land);
    }
    tri(t_land + dx_cells / WALK_FAST_CELLS)
}

/// The landing's walk: the one colour law every mark of a landing reads, the
/// band's own walk at a column relative to the landing cell.
///
/// On a laid band, [`LandingWalk::at`] is `tri(t0 + walk_t(d_land + dx))`,
/// where `(d_land, t0)` are the band's walk distance at the landing column and
/// the walk's `t` at distance zero (`Cohort::walk_d`, `Cohort::walk_zero`,
/// carried on [`Ctx::caret_walk`] and latched at the spawn edge beside
/// `t_land`). At `dx = 0` it is `tri(t_land)` exactly, and elsewhere it is the
/// band's own number at that column, bit for bit, at the band's own pace
/// (`d/16` within the first sixteen cells of the walk, `1/36` a cell after).
/// A band that continues a walk (a wrapped row, a relaid word) is already `d0`
/// cells into it at its first column.
///
/// Off any band ([`LandingWalk::band`] `None`) it is [`landing_field`],
/// `d/16` from the landing, which is what a wake laid there would walk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LandingWalk {
    /// The field at the landing cell, raw (D4): folded once, at the read.
    pub t_land: f32,
    /// `Some((d_land, t0))` when the landing cell is on a laid band —
    /// `d_land` the band's walk's distance at the landing column
    /// (`Cohort::walk_d`, cells, signed) and `t0` the walk's `t` at
    /// distance zero (`Cohort::walk_zero`) — so `t0 + walk_t(d_land) ==
    /// t_land`, the band's own equation for the landing cell. `None` off
    /// any band.
    pub band: Option<(f32, f32)>,
}

impl LandingWalk {
    /// A walk off any band: `d/16` from the landing ([`landing_field`]).
    #[must_use]
    pub const fn free(t_land: f32) -> Self {
        Self { t_land, band: None }
    }

    /// The folded arc position `dx_cells` to the right of the landing cell;
    /// `0.0` is the landing cell's own stop, the pin's colour.
    #[inline]
    #[must_use]
    pub fn at(&self, dx_cells: f32) -> f32 {
        if !dx_cells.is_finite() {
            return tri(self.t_land);
        }
        match self.band {
            Some((d_land, t0)) => tri(t0 + walk_t(d_land + dx_cells)),
            None => landing_field(self.t_land, dx_cells),
        }
    }
}

/// The landing's ink: the band's hot ink ([`hot_edge_ink`]) at folded arc
/// position `t` at full chroma (the arc is S 1.00, so the green→blue seam is
/// the band's teal; [`super::ribbon::BED_SAT_FLOOR`] guards an off-arc read) and lifted
/// through its own hue to one heat.
///
/// The burst is additive light over the text, the lane the band's hot edge
/// lives in, so it takes the hot edge's table rather than the bed's (which is
/// solved down onto the legibility bar because a letter sits on it). The hot
/// table lifts blue, indigo and violet to one heat and carries yellow, orange
/// and green pure.
///
/// Solved once per process into [`BED_INK_LUT_LEN`] entries (the same entries
/// `BedInkLut::hot` holds) and read with one lerp, since `hot_edge_ink` is a
/// bisection over `powf`.
#[must_use]
pub fn landing_ink(t: f32) -> u32 {
    static LUT: OnceLock<[u32; BED_INK_LUT_LEN]> = OnceLock::new();
    let lut = LUT.get_or_init(|| {
        let mut lut = [0_u32; BED_INK_LUT_LEN];
        for (i, e) in lut.iter_mut().enumerate() {
            // Indexed by the walk, filled from the arc
            // ([`super::ribbon::WALK_PACE`]), exactly as the band's own ink
            // table is — the landing's ink is the band's hot ink entry for
            // entry, and that stays true only if both read the same pace.
            *e = hot_edge_ink(spectrum(walk_pace(i as f32 / (BED_INK_LUT_LEN - 1) as f32)));
        }
        lut
    });
    let x = clamp01(t) * (BED_INK_LUT_LEN - 1) as f32;
    let i = (x as usize).min(BED_INK_LUT_LEN - 1);
    let j = (i + 1).min(BED_INK_LUT_LEN - 1);
    lerp_rgb(lut[i], lut[j], x - i as f32)
}

/// The landing's ink on a light theme (section 6.10): the same stop as a point
/// mark's ink on the page `bg` ([`paper_mark_ink`]), hue intact. The light arm
/// buys "bigger" as area, never as brightness, so there is no heat to equalize
/// onto.
#[must_use]
fn landing_ink_light(t: f32, bg: u32) -> u32 {
    paper_mark_ink(spectrum(walk_pace(clamp01(t))), bg)
}

/// The landing's ink at a named stop: [`hot_edge_ink`] of the anchor itself
/// ([`spectrum_stop`]), for the landing's point marks (the sparks; C1: a point
/// mark snaps). Seven entries, solved once per process; out-of-range indices
/// clamp to violet.
#[must_use]
pub fn landing_stop_ink(i: usize) -> u32 {
    static LUT: OnceLock<[u32; SPECTRUM_STOPS]> = OnceLock::new();
    LUT.get_or_init(|| std::array::from_fn(|i| hot_edge_ink(spectrum_stop(i))))
        [i.min(SPECTRUM_STOPS - 1)]
}

// ---- §6.5 the train -------------------------------------------------------

/// Stations per train layer (§6.5, §18). The stride is
/// `max(STATION_STRIDE_MIN_PX, L/STATIONS_MAX)`.
pub const STATIONS_MAX: usize = 96;

/// Minimum station stride in px: an 8-cell hop's frame-0 span is 20 px at
/// `cw = 9`, and a whole sweep across it needs more than three slabs
/// (`the_train_carries_the_whole_spectrum_on_every_frame_of_the_flight`).
/// Only a flight shorter than 192 px uses it; a long one strides `L/96`.
pub const STATION_STRIDE_MIN_PX: f32 = 2.0;

/// Hard cap on one meteor's `under` share (§6.5, §18): the colour layer sheds
/// stations before exceeding it, so a two-meteor ping-pong never sheds the
/// ribbon. `2 × 2304 + 2 × 768 + 10240 = 16384`, exactly `MAX_QUADS`; an
/// 80-cell train at the retina cell uses about 1 900.
pub const UNDER_QUAD_CAP: usize = 2_304;

/// Cap on one meteor's white (`out`) layer (§18).
pub const WHITE_QUAD_CAP: usize = 1_152;

/// Width falloff length as a share of `L`: `w(s) = w·(0.30 + 0.70·exp(−s/(0.5·L)))`
/// (§6.5 layer 1).
pub const WIDTH_FALLOFF_SHARE: f32 = 0.5;

/// The width's floor share at the tail: wider at the head, feathered at the
/// tail.
pub const WIDTH_FLOOR_SHARE: f32 = 0.22;

/// The width's exponentially-decaying share (§6.5 layer 1); floor + span = 1.
pub const WIDTH_SPAN_SHARE: f32 = 0.78;

/// The transverse profile's core share (§6.5 layer 1): the plateau is half the
/// reach, which is also `aterm_render::RIBBON_CORE_SHARE`'s ceiling, so the
/// rasterizer's clamp is a no-op.
pub const TRAIN_CORE_SHARE: f32 = 0.5;

/// Colour-layer coverage falloff length as a share of `L`:
/// `cov(s) = head_cov·exp(−s/(0.55·L))`. At `0.61·L` it is `e⁻¹` of its value
/// at the shoulder; the whole
/// length stays saturated (an 80-cell launch cell sits at 19, above the chroma
/// cull) while the head stays the brightest station.
pub const COLOUR_FALLOFF_SHARE: f32 = 0.55;

/// White-layer coverage falloff length as a share of `L` (§6.5 layer 2), much
/// shorter, so the white dies fast behind the shoulder. After the arrival edge
/// the length itself closes into the landing on the pin's clock
/// ([`WHITE_CLOSE_MS`]), which makes the white → spectrum turn (§6.8) visible.
pub const WHITE_FALLOFF_SHARE: f32 = 0.12;

/// The white layer's width as a share of `w(s)` (§6.5 layer 2), once it is
/// past the shoulder.
pub const WHITE_WIDTH_SHARE: f32 = 0.45;

/// How many e-folds of [`WHITE_FALLOFF_SHARE`] the white polyline is built
/// over before it stops spending stations. `e⁻⁵ = 0.0067`, i.e. under one
/// level of a 118-cov head: everything past this point would truncate to zero
/// anyway, and stations that emit nothing are the "no dead work" rule of §18.
pub const WHITE_TAIL_EFOLDS: f32 = 5.0;

/// The shoulder's length as a share of `L` (§6.5 layer 3): the ionisation
/// peak, brightest just behind the head. The true length is
/// `max(0.06·L, |Δ_frame| + 2 px)` with the cell clamp
/// `[SHOULDER_MIN_CELLS, SHOULDER_MAX_CELLS]` applied only to the frame-travel
/// term: `0.06·L` is the design length (an 80-column flight's shoulder is
/// 4.8 cells, §6.6), and `|Δ_frame| + 2 px` is a continuity floor that keeps
/// the head spatially continuous on a 60 Hz panel (T7) and must not run away.
pub const SHOULDER_SHARE: f32 = 0.06;

/// The shoulder's width as a share of `w` (§6.5 layer 3).
pub const SHOULDER_WIDTH_SHARE: f32 = 0.55;

/// Shoulder length floor, in cells (§6.5 layer 3).
pub const SHOULDER_MIN_CELLS: f32 = 1.0;

/// Shoulder length ceiling, in cells (§6.5 layer 3).
pub const SHOULDER_MAX_CELLS: f32 = 4.0;

/// The effect frame the continuity floor is sized to, in ms: a 60 Hz panel
/// under `EFFECT_PRESENT_PANEL_PERIODS = 2` (T7, §18). A lower bound on
/// continuity, not the slowest frame the lane can produce; a shorter floor
/// could only shrink the shoulder. Sizing the floor to the head's travel over
/// this span keeps the head continuous without the emit path remembering a
/// previous-frame position, so a frame stays a function of `now`.
pub const SHOULDER_FRAME_MS: f32 = 33.4;

/// The corona's reach as a multiple of the nucleus radius (§6.5 layer 6's
/// coma radius, kept as the corona's outer edge).
pub const COMA_R_SCALE: f32 = 2.4;

/// Each petal's coverage as a share of `head_cov` (§6.5 layer 6's coma share,
/// now spent seven times over — the petals overlap pairwise, so no pixel of
/// the corona asks more than two shares, under the nucleus's own request).
pub const COMA_COV_SHARE: f32 = 0.34;

/// The corona: seven petals, one per named stop (`spectrum_stop(0..7)`, C1
/// (owner, 2026-09-08):
/// point marks snap), orbiting the white nucleus at [`COMA_PETAL_ORBIT`] of
/// the corona's reach with [`COMA_PETAL_R`] of it as their radius, turning
/// once per [`COMA_SPIN_MS`]. The nucleus stays `#FFFFFF`, which is what makes
/// the colours read as fire (§3.2).
pub const COMA_PETALS: usize = 7;

/// A petal's radius as a share of the corona's reach.
pub const COMA_PETAL_R: f32 = 0.62;

/// A petal's orbit (centre distance from the nucleus) as a share of the
/// corona's reach. `0.72 − 0.62 = 0.10` of the reach short of the outer
/// edge, and `0.72 + 0.62 > 1`: the petals overlap the nucleus's edge, so the
/// corona reads as one glow with a white heart and a coloured rim.
pub const COMA_PETAL_ORBIT: f32 = 0.72;

/// One turn of the corona, ms — static under reduced motion (§6.11).
pub const COMA_SPIN_MS: f32 = 240.0;

/// Nucleus core radius floor in px (§6.5 layer 7).
pub const NUCLEUS_MIN_PX: f32 = 2.5;

/// Nucleus core radius as a share of `w` (§6.5 layer 7).
pub const NUCLEUS_W_SHARE: f32 = 0.22;

/// Nucleus stretch gain: `stretch = 1 + 1.2·v/v_max`, ×2.2 at launch → ×1.0 at
/// rest (§6.5 layer 7). Axis-aligned ellipse; on a diagonal, stretch the
/// dominant axis only — `RainHalo` has no rotation.
pub const NUCLEUS_STRETCH_GAIN: f32 = 1.2;

/// How far ahead of the head the nucleus centre sits, in px (§6.5 layer 7).
pub const NUCLEUS_LEAD_PX: f32 = 2.0;

// ---- §6.2 / §6.5 the arrival edge and its marks ---------------------------

/// Terminal-flash duration in ms: nucleus radius ×(1.0 → 1.6) on ease-out-back
/// (§6.5 layer 8). Area, not coverage: the cap is a cap.
pub const FLASH_MS: f32 = 40.0;

/// Terminal-flash peak radius multiplier (§6.5 layer 8) — the value the ease
/// is aimed at, and the one it passes through on the way up.
pub const FLASH_PEAK: f32 = 1.6;

/// How far past [`FLASH_PEAK`] the back ease carries the radius before it
/// relaxes (§6.5 layer 8: "overshoot 1.7 at 40 ms").
pub const FLASH_BACK: f32 = 0.1;

/// Terminal-flash overshoot at [`FLASH_MS`] (§6.5 layer 8) — where the
/// ease-out actually stands when the 40 ms are spent: the target plus the
/// back. Spelled as the sum so the published ×1.6 is the number the curve is
/// built from, not a constant nothing reads.
pub const FLASH_OVERSHOOT: f32 = FLASH_PEAK + FLASH_BACK;

/// Milliseconds by which the flash has relaxed back to ×1.0 (§6.5 layer 8).
pub const FLASH_RELAX_MS: f32 = 120.0;

/// Pin life in ms, `u = t/150` (§6.5 layer 9, §6.2). The pin closes into the
/// caret: the last pixel on glass is its nucleus, which is the caret.
pub const PIN_MS: f32 = 150.0;

/// Pin arm half-length at `u = 0`, in `ch`:
/// `A(u) = 0.6 ch·(1 − smoothstep(0.15, 1, u))^1.2` (§6.5 layer 9).
pub const PIN_ARM_CH: f32 = 0.6;

/// The `u` at which the pin's arm law starts to close (§6.5 layer 9's
/// `smoothstep(0.15, 1, u)`): the arms hold full length for the first 15 % of
/// the pin's life, so the mark is a pin on the frame it is born and not a
/// smear already in retreat.
pub const PIN_ARM_HOLD_U: f32 = 0.15;

/// The exponent of the pin's arm law (§6.5 layer 9) — `^1.2`, so the arms whip
/// in slightly faster than the smoothstep alone.
pub const PIN_ARM_EXP: f32 = 1.2;

/// The pin's waist, in px — **exactly 1** (§6.5 layer 9). Drawn as explicit
/// `push_fx_rect` arms plus a 2×2 nucleus, not via `push_twinkle_star`, whose
/// `star_waist_px` would give a 4-px waist at this arm.
pub const PIN_WAIST_PX: i32 = 1;

/// The pin's nucleus, in px — 2×2, and it *is* the caret (§6.5 layer 9).
pub const PIN_NUCLEUS_PX: i32 = 2;

/// The `u` up to which the pin holds α = 1 before spending it (§6.5 layer 9).
pub const PIN_ALPHA_HOLD_U: f32 = 0.6;

// ---- the arrival flash ----------------------------------------------------

/// How long the arrival's white burst lives, ms, from the pin's edge. The
/// caret cell and its neighbours ([`FLASH_CELLS`]) go white on the arrival
/// frame; the white leaves over this span (`1 − smoothstep`) while the colour
/// arrives over the same span, one way, never back ([`draw_flash`]). It sits
/// inside the pin's 150 ms, so the cadence law is unchanged.
pub const FLASH_BURST_MS: f32 = 80.0;

/// When the flash's colour layer is spent, ms from the pin. It peaks where
/// the white is gone ([`FLASH_BURST_MS`]) and spends on `spend` over the
/// same span again — dying into the shockwave, which is still at its hold
/// ([`RING_COV_HOLD_U`]) when the last of it goes.
pub const FLASH_COLOUR_MS: f32 = 2.0 * FLASH_BURST_MS;

/// The flash's coverage over a cell the sky's probe proved blank. Not 255: the
/// caret beside it is `#FFFFFF` on the flare frame with the pin's nucleus on
/// top, and must stay the brightest pixel by a visible margin. Over a glyph
/// cell, or one the probe has not seen, the burst asks the transient cap
/// ([`super::timing::TRANSIENT_STAR_COV_CEIL`], L3).
pub const FLASH_FULL_COV: f32 = 224.0;

/// How many cells either side of the caret the flash covers: one, so three
/// cells wide.
pub const FLASH_CELLS: i32 = 1;

/// The reduced-motion flash's white share of its cell's coverage: the
/// static form is a white heart over the colour, half and half, held for the
/// landing's life and taken off on the theme's one linear fade (§6.11).
pub const FLASH_STATIC_WHITE_SHARE: f32 = 0.5;

/// Impact life in ms at the floor, `u = t/480` (§6.5 layer 10). The name is
/// the ring's; the starburst runs on this clock ([`ring_ms`]): most of the way
/// out inside 110 ms on its quartic, then hovering and spending, off glass at
/// `u ≈ 0.93`, inside [`super::timing::FLIGHT_OFF_GLASS_MS`]
/// (owner, 2026-09-08).
pub const RING_MS: f32 = 480.0;

// ---- the landing's grade --------------------------------------------------
//
// One magnitude, [`impact`] (`timing.rs`: `clamp((cells/8)^0.7, 1, 3.5)`), is
// read by every distance-graded landing law here (the impact's radius and
// life, the fan's count and reach), so they cannot come apart. The 8-cell
// floor is impact 1.0; distance buys on top of it monotonically and saturates
// at the 3.5 cap (about 48 cells, a full line).

/// Extra ring life per unit of impact above the floor, in ms. `480 + 48·2.5
/// = 600` at the cap — exactly the pool's horizon
/// ([`super::timing::FLIGHT_OFF_GLASS_MS`]), the ceiling [`ring_ms`] holds
/// it under: the ring is off glass at `u ≈ 0.93` (558 ms) even at the cap.
pub const RING_MS_PER_IMPACT: f32 = 48.0;

/// Ring life ceiling in ms — the pool's own horizon, so the const assert
/// "every impact mark must be off glass by FLIGHT_OFF_GLASS_MS" holds at
/// every grade, and the shockwave's hold (`0.25 · 600 = 150`) is still
/// inside the pin's attack ([`PIN_MS`]).
pub const RING_MS_MAX: f32 = FLIGHT_OFF_GLASS_MS;

/// Extra ring radius per unit of impact above the floor, in `ch`: a
/// full-line landing rings at `3.0 + 1.2·2.5 = 6.0 ch`, twice the floor's
/// reach — the same ×2 the classic landing ring takes at the cap
/// (`cursor_glow::classic_ring_radius_factor`).
pub const RING_R_PER_IMPACT_CH: f32 = 1.2;

/// The ring's life for a landing of `cells` — [`RING_MS`] at the floor,
/// [`RING_MS_PER_IMPACT`] more per unit of [`impact`], capped at
/// [`RING_MS_MAX`]: 480 / 510 / 536 / 580 / 600 ms at 8 / 16 / 24 / 40 /
/// ≥ 48 cells.
#[inline]
#[must_use]
pub fn ring_ms(cells: f32) -> f32 {
    (RING_MS + RING_MS_PER_IMPACT * (impact(cells) - 1.0)).min(RING_MS_MAX)
}

/// The ring's full radius (at `u = 1`) for a landing of impact `scale`, in
/// px — [`RING_R_CH`] at the floor, [`RING_R_PER_IMPACT_CH`] more per unit of
/// impact. One function, so the emit path and the pins share it.
#[inline]
#[must_use]
pub fn ring_full_radius(scale: f32, ch: f32) -> f32 {
    let scale = if scale.is_finite() {
        scale.clamp(1.0, super::timing::IMPACT_MAX)
    } else {
        1.0
    };
    (RING_R_CH + RING_R_PER_IMPACT_CH * (scale - 1.0)) * ch
}

const _: () = assert!(
    RING_MS + RING_MS_PER_IMPACT * (super::timing::IMPACT_MAX - 1.0) <= RING_MS_MAX + 1e-3,
    "the graded ring life reaches the horizon exactly at the cap and never past it"
);

/// The fan's star count for a big landing of `cells` (§6.5 layer 11):
/// `min(19 + 3.6·(impact − 1), 28) + party·8`, held under
/// [`super::timing::FAN_MAX_N`]: 19 at the 8-cell floor, 21 / 23 / 27 at
/// 16 / 24 / 40 cells, 28 from the cap.
#[inline]
#[must_use]
pub fn fan_count(cells: f32, party: bool) -> usize {
    let base = (FAN_N_BASE + FAN_N_PER_IMPACT * (impact(cells) - 1.0)).min(FAN_N_CEIL);
    ((base.round() as usize) + usize::from(party) * FAN_PARTY_ADD).min(super::timing::FAN_MAX_N)
}

/// The fan's throw reach for a big landing of `cells`, in `ch` (§6.5 layer
/// 11): `(2.8 + 1.6·(impact − 1)).clamp(1.6, 6.8 + 1.2·grade)` — exactly
/// [`FAN_REACH_BASE_CH`] at the 8-cell floor (stardust's m2 clearance law is
/// derived there), 3.8 / 4.7 / 6.1 at 16 / 24 / 40 cells, 6.8 from the cap.
/// Shares its `(impact − 1)` form with [`fan_count`] and [`ring_full_radius`].
#[inline]
#[must_use]
pub fn fan_reach_ch(cells: f32, grade: f32) -> f32 {
    let ceil = FAN_REACH_MAX_CH + FAN_REACH_GRADE_CH * grade;
    (FAN_REACH_BASE_CH + FAN_REACH_PER_IMPACT_CH * (impact(cells) - 1.0))
        .clamp(FAN_REACH_MIN_CH, ceil)
}

/// Impact radius at `u = 1` at the floor, in `ch`: `r(u) = 3.0 ch·(1 − (1 −
/// u)^4)`.
pub const RING_R_CH: f32 = 3.0;

/// The exponent of the ring's radius law (§6.5 layer 10) — `(1 − (1 − u)^4)`,
/// so the ring is already most of the way out on the frame it is born (the
/// arrival edge owes the eye an event, not a slow bloom).
pub const RING_R_EXP: i32 = 4;

// The rise law is stated on the painted envelope, not a centreline:
// `BURST_CORE_CH`'s assert covers tip + half the root stroke + the
// rasterizer's fringe, `burst_rise_limit` clamps the same envelope in device
// px at emit, and `the_burst_paints_inside_the_fan_s_rise_at_every_cell`
// measures the emitted quads at four cell heights.

/// Impact coverage as a share of [`TRANSIENT_STAR_COV_CEIL`], held for
/// [`RING_COV_HOLD_U`] and then spent on `spend`: the full share for
/// `u ≤ 0.25`, then `(1 − u')²` with `u' = (u − 0.25)/0.75`.
///
/// The hold spends coverage while the mark is big enough to read (it has no
/// radius at `u = 0`). The share is the whole transient cap (118), the ceiling
/// the pin, the sparks and every transient star are held to over a glyph cell
/// (L3).
pub const RING_COV_SHARE: f32 = 1.0;

/// The `u` up to which the impact holds its full coverage before spending it:
/// `0.25 · 480 ms = 120 ms`, the flash's whole life and forty milliseconds
/// past it, then spent over the remaining 360 ms ([`RING_COV_SHARE`]). The
/// pin's own alpha hold is [`PIN_ALPHA_HOLD_U`].
pub const RING_COV_HOLD_U: f32 = 0.25;

/// The stroke's hold in ms at the floor, [`RING_COV_HOLD_U`] of [`RING_MS`]
/// — the shockwave's attack (inside the pin's 150 ms, so the cadence law's
/// attack window is the pin's as before). A graded ring holds the same
/// share of its longer life: [`RING_HOLD_MAX_MS`] is the cap's.
pub const RING_HOLD_MS: f32 = RING_COV_HOLD_U * RING_MS;

/// The stroke's hold at the cap, ms — `0.25 · 600 = 150`, the pin's own
/// window exactly; the const assert below holds it there.
pub const RING_HOLD_MAX_MS: f32 = RING_COV_HOLD_U * RING_MS_MAX;

/// Fan reach at the floor landing, in `ch`: `1.6 + 0.15·8 = 2.8`. Graded:
/// `reach = 2.8 + 1.6·(impact − 1)`, clamped.
pub const FAN_REACH_BASE_CH: f32 = 2.8;

/// Fan reach per unit of [`impact`] above the floor, in `ch` (§6.5 layer 11).
/// The floor landing throws exactly [`FAN_REACH_BASE_CH`] (the const assert
/// below holds it), because stardust's m2 clearance law
/// (`a_fan_star_is_born_at_full_and_clears_its_cell_inside_its_hold`) is
/// derived at that reach; distance buys 3.8 / 4.7 / 6.1 / 6.8 ch at
/// 16 / 24 / 40 / ≥ 48 cells.
pub const FAN_REACH_PER_IMPACT_CH: f32 = 1.6;

const _: () = assert!(
    FAN_REACH_BASE_CH >= 2.8 - 1e-6 && FAN_REACH_BASE_CH <= 2.8 + 1e-6,
    "the floor landing's reach is 2.8 ch — stardust's m2 clearance law is derived at the floor"
);
const _: () = assert!(
    FAN_REACH_BASE_CH + FAN_REACH_PER_IMPACT_CH * (super::timing::IMPACT_MAX - 1.0)
        <= FAN_REACH_MAX_CH + 1e-6,
    "the capped landing's reach lands exactly on the un-graded ceiling"
);

/// Fan reach floor, in `ch` (§6.5 layer 11) — and the Enter landing's fixed
/// reach (D8).
pub const FAN_REACH_MIN_CH: f32 = 1.6;

/// Fan reach ceiling before the grade term, in `ch`: the capped landing's own
/// `2.8 + 1.6·2.5`, so it binds only through `grade`. Along the line only; the
/// rise is the sky's [`FAN_RISE_MAX_CH`].
pub const FAN_REACH_MAX_CH: f32 = 6.8;

/// How much `grade` lifts the fan's reach ceiling (§6.5 layer 11).
pub const FAN_REACH_GRADE_CH: f32 = 1.2;

/// Fan count at the floor landing: `n = min(19 + 3.6·(impact − 1), 28) +
/// party·8` (§6.5 layer 11): 19 / 21 / 23 / 27 / 28 at 8 / 16 / 24 / 40 /
/// ≥ 48 cells. The census is 1 gold m1 + 4 m2 (D6) and the rest grains; the
/// hold is the sky's (`stardust::HOLD_*_TRANSIENT_MS`).
pub const FAN_N_BASE: f32 = 19.0;

/// Extra fan stars per unit of [`impact`] above the floor (§6.5 layer 11):
/// `19 + 3.6·2.5 = 28`, the ceiling, exactly at the cap.
pub const FAN_N_PER_IMPACT: f32 = 3.6;

/// Fan count ceiling before the party bonus (§6.5 layer 11). With the shed's
/// six and the sky's typical twenty this sits under `stardust::STAR_CAP`; a
/// party lands on the eviction finish, a 40 ms fade, never a pop.
pub const FAN_N_CEIL: f32 = 28.0;

const _: () = assert!(
    FAN_N_BASE + FAN_N_PER_IMPACT * (super::timing::IMPACT_MAX - 1.0) <= FAN_N_CEIL + 1e-3,
    "the capped landing's fan count lands exactly on the ceiling"
);

/// What a party adds to the fan's count (§6.5 layer 11).
pub const FAN_PARTY_ADD: usize = 8;

/// The eased spine at or above which a landing is a party (§6.5 layer 11,
/// §7.2). This is the celebration arm's own signature: `Engine::celebrate`
/// pins the spine to a floor while a sing-along is armed, and that pinned
/// spine is the one thing the glow can see that says "this is a celebration"
/// without reaching across the seam into the synth.
pub const FAN_PARTY_DISP: f32 = 0.95;

/// Cells of landing size per row of a vertical / Enter flight (D8):
/// `cells_landing = 4·|dr|` → 5-7 m3 + 1 m2, no ring, no hero, no rain.
pub const ENTER_LANDING_CELLS_PER_ROW: f32 = 4.0;

/// Floor on an Enter / vertical landing's grain count (D8, §6.11).
pub const ENTER_LANDING_M3_MIN: usize = 5;

/// Ceiling on an Enter / vertical landing's grain count (D8, §6.11).
pub const ENTER_LANDING_M3_MAX: usize = 7;

/// Nearest station of a shed fragment, as a share of `L` (§6.5 layer 5).
pub const SHED_S_MIN: f32 = 0.10;

/// Farthest station of a shed fragment, as a share of `L` (§6.5 layer 5).
pub const SHED_S_MAX: f32 = 0.70;

// ---- §6.8 the fade --------------------------------------------------------

/// White layer's post-arrival τ, ms: `α_w = exp(−(t−T)/90)`, culled at
/// α < 0.12 → `T + 191` (§6.8).
pub const TRAIN_WHITE_TAU_MS: f32 = 90.0;

/// Colour layer's post-arrival τ, ms: `α_c = exp(−(t−T)/220)`, culled at
/// α < 0.12 → `T + 466`, so the rainbow stays on the row while the impact
/// expands and leaves inside [`super::timing::FLIGHT_OFF_GLASS_MS`].
pub const TRAIN_COLOUR_TAU_MS: f32 = 220.0;

/// The colour root's retract span, ms: `root(t) = x₀ + (x₁−x₀)·((t−T)/460)^1.8`
/// on `suck-in`. The root reaches the landing on the frame the colour reaches
/// its chroma cull, so the last train pixel and the corona leave together. No
/// wind bend.
pub const ROOT_SUCK_MS: f32 = 460.0;

/// The thickness the train thins to over the fade, as a share of `w` (§6.8).
pub const THICKNESS_END_SHARE: f32 = 0.4;

/// The span, ms, over which the white layer's extent (its shoulder and falloff
/// length) retracts to the landing on `suck-in` after the arrival edge: the
/// pin's own clock ([`PIN_MS`]). The white closes with the pin's arms, so
/// everything white at the landing leaves on one edge and the last mark on
/// glass is the nucleus, the caret (§6.5 layer 9, T5). The §6.8 alpha taus are
/// untouched; this moves where the white is, not how bright.
///
/// An alpha-only fade leaves the train's chromaticity flat, so the white
/// → spectrum turn (§6.8) never reads; with the extent closing toward the
/// caret (T5: leaving marks move toward the caret) the mean saturation beyond
/// the landing rises to 1.0 by `T + 150`.
pub const WHITE_CLOSE_MS: f32 = PIN_MS;

/// The retire decay (§6.9): `ln(1/CHROMA_CULL_ALPHA) = ln(1/0.12)`. A retired
/// train's colour runs `exp(−u·RETIRE_DECAY)` over
/// `u = (t − retired_at)/FLIGHT_RETIRE_MS`, reaching the chroma cull exactly
/// at 60 ms. A literal because `f32::ln` is not `const`;
/// `the_retire_decay_lands_on_the_chroma_cull` pins it.
pub const RETIRE_DECAY: f32 = 2.1203;

/// Reduced motion's static train alpha (§6.11): head at the landing, train at
/// the exponential profile at this α, no flight, no flash, no pin, no ring, no
/// twinkle, static fan — then the theme's one linear fade.
pub const REDUCED_MOTION_ALPHA: f32 = 0.85;

// ---- §6.9 retire ----------------------------------------------------------

/// How close two corridors must come for the newer to retire the older, in
/// `ch` (§6.9).
pub const CORRIDOR_CH: f32 = 1.0;

/// How close a new launch must be to a live head, in cells, for the flight to
/// chain rather than merely cross (§6.9).
pub const CHAIN_CELLS: f32 = 1.0;

/// The meteor pool (§18: fixed pools): [`FLIGHT_MAX_LIVE`] heads in the air
/// plus as many trains finishing on §6.9's 60 ms retire. `Meteors::live` is
/// reserved to this depth and never grows: a spawn into a full pool drops the
/// finishing train nearest its end, which at any human cadence is already at
/// the chroma cull. This keeps the spawn path allocation-free under a mash.
pub const METEOR_POOL: usize = 2 * FLIGHT_MAX_LIVE;

/// The landing pool (§18). A landing lives [`Landing::end`]'s 560 ms, so an
/// 80-cell ping-pong at a 200 ms cadence has three impacts finishing at once;
/// a fourth drops the oldest, which is already dark.
pub const LANDING_POOL: usize = FLIGHT_MAX_LIVE + 1;

/// Depth of the per-tick hand-off scratch ([`Meteors::sow_into`]): every live
/// meteor's whole shed plus a fan each, plus one mini-fan, plus the sing-along
/// bar's party fan ([`Meteors::party`]), which can land on the same frame.
/// Reserved once so staging never allocates (§18). `sown` is drained every
/// tick, so this capacity is the per-frame budget. The worst case is
/// enumerated here, not reached by `tests/rainbow_kitty_v2_frame_cost.rs`.
pub const SOW_SCRATCH: usize = METEOR_POOL * (SHED_MAX as usize + 1) + 2;

// ---- the starburst (owner, 2026-09-10) ------------------------------------
//
// Dark angular gaps are necessary but not sufficient: what reads as radial is
// uniform angular coverage in screen space. A spike placed at an ellipse
// parameter angle leaves the caret at a much flatter screen angle, so every
// direction below is a true screen direction on an isotropic circle, and the
// distance grade is bought along the row, in the jets, where the rise cap
// does not charge for it.

/// The core spike's tip radius at full expansion, in `ch`: the star's own
/// reach, isotropic in screen space. Sized by the rise cap: the painted
/// envelope `BURST_CORE_CH + BURST_SEC_THICK_CH[0]/2 + BURST_AA_SUPPORT_CH =
/// 1.57 ch` stays inside [`FAN_RISE_MAX_CH`] (const assert below).
pub const BURST_CORE_CH: f32 = 1.42;

/// Every spike's root radius, in `ch`: the larger of two laws.
///
/// * The caret keeps a disc of its own half-height (`0.5 ch`), so the pin's
///   nucleus and the flash's white are never painted over. It is also why the
///   star emerges as the flash's white dies: on the quartic nothing paints
///   until the tips pass the root.
/// * The wedges stay dark all the way in: adjacent spikes of thickness `w` at
///   spacing `2pi/n` separate at radius `w / (2·sin(pi/n))`; inside it the
///   beams pile onto the flash's white. At `n = 12`, `w = 0.22 ch` that is
///   `0.425 ch` ([`BURST_ROOT_SEP_AT_MAX_N`] holds it under this at compile
///   time).
pub const BURST_ROOT_CH: f32 = 0.50;

/// `2·sin(pi/BURST_N_MAX)` — the separation coefficient of the root law
/// above, spelled as a literal because `sin` is not `const`.
/// `a_spike_root_is_derived_from_the_spacing_it_must_keep_open` proves the
/// literal against the real trigonometry at run time, so the two can never
/// drift.
pub const BURST_ROOT_SEP_AT_MAX_N: f32 = 0.517_638_1;

const _: () = assert!(
    BURST_ROOT_CH * BURST_ROOT_SEP_AT_MAX_N >= BURST_SEC_THICK_CH[0],
    "at the spike ceiling two adjacent spikes must already be separated at their roots — \
     otherwise the star has a merged hub and no dark wedges"
);
const _: () = assert!(
    BURST_ROOT_CH >= 0.5,
    "a spike root inside the landing cell's own rows is light on the caret, not from it"
);

/// The AA fringe [`comet_beam`] adds outside the nominal stroke, in `ch` — one
/// device pixel at a `25 px` cell, which is the smallest cell the family is
/// tuned for. The const rise assert keeps this much headroom; the emitter
/// also clamps in device px ([`burst_rise_limit`]), because a normalized
/// constant cannot account for a device-pixel fringe at every font size.
pub const BURST_AA_SUPPORT_CH: f32 = 0.04;

/// The painted-extent rise law: the burst's painted envelope (tip, plus half
/// the root section's stroke, plus the rasterizer's fringe) stays inside
/// [`FAN_RISE_MAX_CH`]. A centreline semi-axis would let the half-stroke
/// escape the bound.
const _: () = assert!(
    BURST_CORE_CH + 0.5 * BURST_SEC_THICK_CH[0] + BURST_AA_SUPPORT_CH <= FAN_RISE_MAX_CH,
    "the burst's PAINTED rise must stay inside the fan's — the centreline is not the mark"
);

/// Spikes at the 8-cell floor. Nine is the smallest count whose wedges read as
/// a ring of gaps rather than a cross; twelve ([`BURST_N_CEIL`]) is where the
/// `30 deg` spacing stops being legible against a `0.22 ch` spike.
pub const BURST_N_BASE: f32 = 9.0;

/// Spikes gained per unit of [`impact`] above the floor:
/// `n = min(9 + 1.2·(impact − 1), 12)`. The count is the part of the distance
/// grade that survives dense text, where the jets are gated away.
pub const BURST_N_PER_IMPACT: f32 = 1.2;

/// Spike-count ceiling — reached at [`super::timing::IMPACT_MAX`].
pub const BURST_N_CEIL: f32 = 12.0;

/// The `Burst::dir` / `Burst::len` array length. Equal to [`BURST_N_CEIL`];
/// the const assert below keeps them one number.
pub const BURST_N_MAX: usize = 12;

const _: () = assert!(
    BURST_N_BASE + BURST_N_PER_IMPACT * (super::timing::IMPACT_MAX - 1.0) <= BURST_N_CEIL + 1e-3,
    "the graded spike count reaches the ceiling exactly at the impact cap and never past it"
);
const _: () = assert!(
    BURST_N_CEIL as usize == BURST_N_MAX,
    "the spike ceiling and the array that holds the spikes are ONE number"
);

/// The shortest spike, as a share of [`BURST_CORE_CH`]:
/// `len_i = 0.78 + 0.22·unit01(seed, i)`. Narrow on purpose: under the rise
/// cap the star spans only `0.92 ch`, so a wide spread reads as missing spikes
/// rather than variety.
pub const BURST_LEN_MIN: f32 = 0.78;

/// Every third spike is a hero — full length, so the star has a rhythm rather
/// than a hash's noise.
pub const BURST_HERO_EVERY: usize = 3;

/// A hero's length gain, clamped to 1.0 — a hero is always at
/// [`BURST_CORE_CH`], which is also why sorting the spikes by descending
/// length puts the heroes first and makes [`BURST_QUAD_CAP`]'s shed order
/// hero-last for free.
pub const BURST_HERO_GAIN: f32 = 1.28;

/// Angular jitter, as a share of one spacing (`2pi/n`) — `+/- 0.15` of a
/// spacing, which breaks the asterisk without ever letting two spikes close
/// their wedge. A jitter at or above 1.0 would let neighbours cross.
pub const BURST_JITTER: f32 = 0.30;

const _: () = assert!(
    BURST_JITTER < 1.0,
    "an angular jitter of a whole spacing lets two spikes swap places and close their wedge"
);

/// The collinear sections of one spike, as shares of its span. [`comet_beam`]
/// takes one thickness per call, so the lance's taper is geometric: sections
/// of decreasing thickness sharing vertices. Three sections, shorter outward,
/// crowd the steps at the tip where they read as a taper rather than as a
/// joint.
pub const BURST_SEC_SHARE: [f32; 3] = [0.46, 0.32, 0.22];

/// Vertices per lance section. Three, not two, because [`comet_beam`]
/// interpolates colour linearly in RGB between the vertices it is handed and a
/// straight chord across a quarter of the spectrum's walk cuts the corner off
/// its curve — the mark then carries five named stops where the law wants
/// seven.
pub const BURST_SEC_STATIONS: usize = 3;

/// The sections' stroke thickness, in `ch` (`0.22 -> 0.15 -> 0.08`): each
/// about two thirds of the last, so no step reads as a joint.
pub const BURST_SEC_THICK_CH: [f32; 3] = [0.22, 0.15, 0.08];

/// How much of the root's coverage the tip keeps: a lance is brightest where
/// it leaves the impact. `1 − 0.45 = 0.55` at the tip.
pub const BURST_TIP_FADE: f32 = 0.45;

/// The floor of [`comet_beam`]'s major-axis stride for a core spike, px. The
/// core is short and is the mark the eye lands on, so it is tiled finely.
pub const BURST_STEP_PX: usize = 2;

/// The core's stride as a share of the cell, floored at [`BURST_STEP_PX`].
/// Tying the stride to the cell makes the slab count a function of the shape,
/// the same at every font size, so [`BURST_QUAD_CAP`] caps a composition
/// rather than a font size. `0.055` is `2 px` at a `38 px` cell.
pub const BURST_STEP_CH_SHARE: f32 = 0.055;

// ---- the jets: the distance grade, along the line, at no rise cost --------

/// Jets per side, along the flight axis. The jets are the star's extensions:
/// they root where the core's envelope ends, ride the same quartic and clock,
/// and carry the spectrum outward. Kept few so the round star stays dominant.
pub const JET_N_PER_SIDE: usize = 2;

/// Each jet's tilt off the flight axis, radians. The long jet is flat; the
/// short one is tilted, and its sign is hashed per side at the mint so the
/// pair reads as a skid rather than as a symmetric "V".
pub const JET_TILT_RAD: [f32; JET_N_PER_SIDE] = [0.00, 0.13];

/// Each jet's tip, as a share of the ring's own unchanged
/// [`ring_full_radius`] — this is where the distance grade lives:
/// `3.0 ch` at the floor to `6.0 ch` at the cap, exactly the reach the
/// shockwave had.
pub const JET_LEN_SHARE: [f32; JET_N_PER_SIDE] = [1.00, 0.66];

/// The tilted jet's vertical component, as a share of `sin(tilt)`: the jets
/// buy length along the line, never height. At the cap the tilted jet's
/// painted rise is `0.66·6.0·sin(0.13)·0.55 + 0.09 = 0.37 ch`, a quarter of
/// the cap.
pub const JET_RISE_SQUASH: f32 = 0.55;

/// The jets' two collinear sections, as shares of their span — the same
/// taper law as the core's, tuned longer at the root because a jet is read
/// end-on.
pub const JET_SEC_SHARE: [f32; 2] = [0.55, 0.45];

/// The jets' stroke, in `ch`. **Below the core's** [`BURST_SEC_THICK_CH`] at
/// both sections, by the ruling above: the star is always the thicker,
/// brighter mark and the jets are its extensions.
pub const JET_SEC_THICK_CH: [f32; 2] = [0.13, 0.07];

const _: () = assert!(
    JET_SEC_THICK_CH[0] < BURST_SEC_THICK_CH[0] && JET_SEC_THICK_CH[1] < BURST_SEC_THICK_CH[1],
    "the round star stays dominant — a jet thicker than a spike is an arrow, not a starburst"
);
const _: () = assert!(
    BURST_SEC_SHARE[0] + BURST_SEC_SHARE[1] + BURST_SEC_SHARE[2] > 0.999
        && BURST_SEC_SHARE[0] + BURST_SEC_SHARE[1] + BURST_SEC_SHARE[2] < 1.001,
    "a spike's sections must tile its span exactly once"
);
const _: () = assert!(
    BURST_SEC_THICK_CH[1] < BURST_SEC_THICK_CH[0] && BURST_SEC_THICK_CH[2] < BURST_SEC_THICK_CH[1],
    "a lance narrows toward its point"
);

/// The share of a jet's reach held at full coverage; past it the coverage
/// falls to nothing, so the far end is a fade, never a bar end. This keeps
/// the graded reach (what reads as a big jump) without the graded horizontal
/// area.
pub const JET_FEATHER_SHARE: f32 = 0.30;

/// The jets' share of the core's coverage, below 1.0 so the round star stays
/// dominant and the mark never reads as an arrow.
pub const JET_COV_SHARE: f32 = 0.62;

// The burst has no colour walk of its own: every station reads
// [`landing_field`] (the band's walk, at the band's pace, from the band's
// stop, folded the band's way) through the band's hot ink ([`landing_ink`]).

/// Stations per cell along a jet: the gating grain (each station asks the
/// sky's probe about its cell) and the colour grain (C1: a linear mark samples
/// the arc continuously). It is the band's own slab grain ([`SLABS_PER_CELL`])
/// on purpose: `comet_beam` lerps colour in RGB between stations, and a jet is
/// the band's gradient continued, so it must not chord the crossing the band
/// draws with three vertices a cell. Changing the ribbon's slab grain moves
/// the jets' quad count and light-theme rect width with it. Costs no slab:
/// the beam tiles its major axis by `step`, not by vertex.
pub const JET_STATIONS_PER_CELL: usize = SLABS_PER_CELL;

/// How many cells each side of the caret the landing row is probed over: the
/// jets' gate window and the width of [`SkyMask::row`]. The jets reach
/// `6.0 ch` at the cap, `6.0·(ch/cw)` cells; fourteen covers every cell aspect
/// up to 2.33, past which a jet's far tip has no licence and is shed
/// (`a_jet_is_probed_over_its_whole_capped_reach`).
pub const JET_PROBE_CELLS: i32 = 14;

/// The floor of [`comet_beam`]'s major-axis stride for a jet, px — coarser
/// than the core's because a jet is long, thin and peripheral.
pub const JET_STEP_PX: usize = 4;

/// The jets' stride as a share of the cell, floored at [`JET_STEP_PX`] — the
/// same density-independence law as [`BURST_STEP_CH_SHARE`].
pub const JET_STEP_CH_SHARE: f32 = 0.11;

/// A light-arm lance's step between ink squares, as a share of the section's
/// own thickness: overlapping squares, so the ink lance is continuous and not
/// a dotted line.
pub const BURST_LIGHT_STEP_SHARE: f32 = 0.6;

// A jet is gated cell by cell on the landing row's own probe
// ([`SkyMask::row_blank`]), so every station of every jet must actually be on
// that row. `sin(x) <= x` bounds the tilted jet's painted rise at the capped
// reach; the flat jet has no rise at all.
const _: () = assert!(
    JET_LEN_SHARE[1]
        * (RING_R_CH + RING_R_PER_IMPACT_CH * (super::timing::IMPACT_MAX - 1.0))
        * JET_TILT_RAD[1]
        * JET_RISE_SQUASH
        + 0.5 * JET_SEC_THICK_CH[0]
        < 0.5,
    "a jet must stay inside the landing row it is gated on"
);

// No rotation and no colour spin: a rotating spike set fills its own wedges
// inside the eye's integration window. A station's colour is a function of
// its column ([`landing_field`]).

/// Enforced cap on one landing's burst (`out`). It sheds whole elements in a
/// fixed order (spikes by descending length, so the heroes go last, then the
/// long jets, then the tilted jets), so a shed element is a missing short
/// spike, never a broken contour. The order is fixed at the mint and the
/// geometry only grows, so a shed element stays shed and nothing flickers.
/// `a_burst_stays_inside_its_quad_cap_at_every_grade` measures the real
/// emitter at every grade and tuned cell.
pub const BURST_QUAD_CAP: usize = 1_280;

/// A beam stride in device px for a mark whose stride is `share` of the cell,
/// floored at `floor` — see [`BURST_STEP_CH_SHARE`].
#[inline]
#[must_use]
fn burst_step(ch: f32, share: f32, floor: usize) -> usize {
    ((ch * share).round() as usize).max(floor)
}

/// The largest spike count for a landing of `cells` (the grade that survives
/// dense text): `min(9 + 1.2·(impact − 1), 12)`.
#[must_use]
pub fn burst_n(cells: f32) -> usize {
    let n = (BURST_N_BASE + BURST_N_PER_IMPACT * (impact(cells) - 1.0)).min(BURST_N_CEIL);
    (n.round() as usize).clamp(1, BURST_N_MAX)
}

// ---- the sparks -----------------------------------------------------------

/// Spark count constant term: `n = clamp(36 + cells/2, 36, 64)`. Each spark is
/// a small haloed star of one named stop (C1: point marks snap), thrown up and
/// out and falling under [`SPARK_G_CH_PER_S2`]. Drawn here rather than sown
/// into the sky: a spark is ballistic, with no twinkle or class ladder, a pure
/// function of the landing's seed and age (nothing to pool or allocate), and
/// sixty would empty the sky's pool. Its shape is the family's one star
/// (`push_twinkle_star`, D2).
pub const SPARK_N_BASE: f32 = 36.0;

/// Cells of path per extra spark.
pub const SPARK_CELLS_PER: f32 = 2.0;

/// Spark count ceiling.
pub const SPARK_N_MAX: usize = 64;

/// The smallest spark's arm half-length, px — a 5 px star.
pub const SPARK_ARM_MIN_PX: i32 = 2;

/// The largest spark's arm half-length, px — a 9 px star. Hashed per spark
/// at the mint ([`mint_spark`]), so a shower is a mix of sizes.
pub const SPARK_ARM_MAX_PX: i32 = 4;

/// How far a spark's core is tinted from white toward its stop: a coloured
/// hot centre inside a saturated halo is what makes a point read as a star.
pub const SPARK_CORE_TINT: f32 = 0.75;

/// A spark's halo radius, px, over its arm — `arm + this`: 4 px around the
/// smallest star, 6 around the largest.
pub const SPARK_HALO_R_ADD_PX: f32 = 2.0;

/// A spark's halo peak as a share of its core coverage — the halo is the
/// saturated stop itself, and it may not outshine the core (the sky's own
/// halo law).
pub const SPARK_HALO_SHARE: f32 = 0.55;

/// How many spark halos one frame may carry across every live landing —
/// the aurora's own pool depth, budgeted beside it (§18): a spark past the
/// budget keeps its star and loses its halo, never the other way round.
pub const SPARK_HALO_CAP: usize = 64;

/// Shortest spark life, ms.
pub const SPARK_LIFE_MIN_MS: f32 = 460.0;

/// Longest spark life, ms — the impact's last mark, ON the pool's horizon
/// [`super::timing::FLIGHT_OFF_GLASS_MS`] (asserted below); its alpha is
/// under the chroma cull from `u ≈ 0.86`, 516 ms.
pub const SPARK_LIFE_MAX_MS: f32 = 600.0;

/// Slowest launch speed, in `ch` per second.
pub const SPARK_V_MIN_CH_PER_S: f32 = 5.0;

/// Fastest launch speed, in `ch` per second. With [`SPARK_G_CH_PER_S2`] the
/// highest spark peaks `9²/(2·24) = 1.7 ch` above the row — the fan's own
/// rise, not the tab strip — and is back on the row by 0.75 s.
pub const SPARK_V_MAX_CH_PER_S: f32 = 9.0;

/// Gravity, in `ch` per second squared.
pub const SPARK_G_CH_PER_S2: f32 = 24.0;

/// The launch cone: angles from `π·this` to `π·(1 − this)`, measured from
/// the row toward straight up — every spark leaves upward, none along the
/// row.
pub const SPARK_CONE_SHARE: f32 = 0.08;

/// How much wider the shower is along the line than up it.
pub const SPARK_SPREAD: f32 = 1.4;

/// The `u` of a spark's life it holds full coverage for before spending it
/// — three fifths: a spark is a point of light that burns, then dies (on
/// `spend`, under the chroma cull from `u ≈ 0.86`).
pub const SPARK_HOLD_U: f32 = 0.6;

/// The widest a spark's core gets, px — the family's star core at the
/// largest arm ([`SPARK_ARM_MAX_PX`]). What a census reads as "a spark" is
/// a chromatic `out` quad no bigger than this on either axis.
#[cfg(test)]
pub const SPARK_PX: i32 = 3;

/// The alpha under which a spark loses its halo and a pixel of arm; under
/// half of it the star is a 3 px cross. A dying star dims halo-first.
pub const SPARK_SHRINK_ALPHA: f32 = 0.6;

// ---- §6.10 the light fork -------------------------------------------------

/// `RAINBOW_METEOR_LIGHT_SQUASH` (§6.10) — the nucleus and coma's vertical
/// squash on the light arm. L6: light buys "bigger" as area, never as
/// brightness, and a squashed ellipse is area spent along the path.
pub const LIGHT_SQUASH: f32 = 0.30;

/// Where the light-theme train's rail line sits below the row's centre, in
/// `ch` (§6.10). The rail is inter-line leading: nothing it darkens is ever a
/// glyph, which licenses the vivid ceiling below.
pub const LIGHT_RAIL_CH: f32 = 0.62;

/// The light-theme train's squash on the rail (§6.10).
pub const LIGHT_RAIL_SQUASH: f32 = 0.34;

/// Coverage → ink-alpha gain on the light arm (§6.10's `min(head_cov·2.1,
/// 236)`): additive coverage and source-over opacity are not the same
/// quantity, and this is the one number that converts between them.
pub const LIGHT_INK_GAIN: f32 = 2.1;

/// `LIGHT_INK_ALPHA_CAP` (L6, §3.3) — the centre over-alpha ceiling for a mark
/// that can composite over a letterform at any moment.
pub const LIGHT_ALPHA_CAP: f32 = 190.0;

/// `RAINBOW_LIGHT_RAIL_ALPHA_CAP` (L6, §3.3) — the ceiling for a mark that is
/// provably confined to the leading, which the rail line is.
pub const LIGHT_RAIL_ALPHA_CAP: f32 = 236.0;

/// The axis rule (§6.1, D15). `ribbon_beam` is x-major and would emit no train
/// quads for a vertical recall, so `|dx| ≥ |dy|` takes `ribbon_beam` and
/// `|dy| > |dx|` takes `ribbon_beam_v`, its transposed twin (same profile,
/// Bayer and `GlowBlend`, pinned by `glow_under_parity`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// `|dx| ≥ |dy|` — `aterm_render::ribbon_beam`.
    Horizontal,
    /// `|dy| > |dx|` — `aterm_render::ribbon_beam_v`.
    Vertical,
}

impl Axis {
    /// The axis of a pixel-space delta, per D15's rule exactly.
    #[must_use]
    pub fn of(dx: f32, dy: f32) -> Self {
        if dx.abs() >= dy.abs() {
            Self::Horizontal
        } else {
            Self::Vertical
        }
    }
}

/// One meteor (§17.1's `Meteor{x0,y0,x1,y1,t0,T,w,grade,mom,retiring}`).
/// `Copy`: the pool is a fixed [`FLIGHT_MAX_LIVE`]-slot array (§18) and a
/// style-crossfade ghost snapshots it whole.
#[derive(Clone, Copy, Debug)]
pub struct Meteor {
    /// Window-absolute X of the launch, px.
    pub x0: f32,
    /// Window-absolute Y of the launch, px.
    pub y0: f32,
    /// Window-absolute X of the landing, px: where the caret was observed.
    pub x1: f32,
    /// Window-absolute Y of the landing, px.
    pub y1: f32,
    /// The spawn edge: the frame the caret was first observed at its landing
    /// (T2). Everything the meteor owns is measured from here.
    pub t0: Instant,
    /// §6.2's `T`, `timing::flight(cells)`. The arrival edge is
    /// `t0 + t_flight`, read by the pin, the burst, the fan, the terminal
    /// flash, the kitty's `land_at` and the audio bell (§6.7): one `Instant`,
    /// so nothing at the landing waits for a second observation or fires early.
    pub t_flight: Duration,
    /// Body thickness in px, `ch·(0.36 + 0.34·mom)·(1 + 0.5·grade)` (§6.3).
    pub w: f32,
    /// `clamp(cells/34, 0, 1)` (§6.3) — buys brightness and the fan's reach
    /// ceiling; its width share is capped at ×1.5.
    pub grade: f32,
    /// When a crossing meteor retired this one (§6.9). Both train layers run
    /// [`RETIRE_DECAY`] from this edge and are culled at exactly
    /// [`FLIGHT_RETIRE_MS`]; the head freezes and the train's root retracts
    /// toward it over the same 60 ms on `suck-in`. The decay starts from the
    /// alpha it had, and the light leaves toward the caret (T5). It sheds
    /// nothing more.
    ///
    /// Retired mid-flight ([`Meteor::handed_over`]), the nucleus and coma stop
    /// on the edge, since only one head is in the air per row. Retired at or
    /// after the arrival edge, the head was at rest and the arrival's
    /// afterglow decays with the train over the same 60 ms, so it never pops.
    pub retired_at: Option<Instant>,
    /// True once a same-direction continuation chained off this head (§6.9).
    /// This train is cut at the chain point and finishes on its post-arrival
    /// law (§6.8): no nucleus, no further shedding, no landing of its own.
    /// Never set on a train whose head is already at rest; the landing mint
    /// refuses a chained train.
    pub chained: bool,
    /// §6.4's `t_land`: the caret's own field `t` at the landing cell, latched
    /// at the spawn edge so the arc stays phase-locked while the ribbon walks
    /// on underneath it.
    ///
    /// The raw walk, never clamped (D4): it runs past `1.0` on a long line and
    /// every read folds it once through [`tri`], so `tri(t_land)` is the
    /// ribbon's stop under the caret.
    pub t_land: f32,
    /// The band's walk at the landing, latched beside [`Meteor::t_land`] from
    /// [`Ctx::caret_walk`]: `(d_land, t0)`, the walk distance at the landing
    /// column and the walk's `t` at distance zero, or `None` off a band. Seeds
    /// the landing's [`LandingWalk`], so marks minted at the arrival edge
    /// continue the band at its own pace. The train's arc does not read it.
    pub band: Option<(f32, f32)>,
    /// Path length in cells, `L / cw` (§6.2) — the input to `flight_ms`,
    /// `shed_n` and the fan's count and reach.
    pub cells: f32,
    /// Travel direction, dominant axis — the kitty's facing and the cue's
    /// `dir`, and the chain test of §6.9.
    pub dir: Dir,
    /// The deterministic seed every hashed choice this meteor makes is drawn
    /// from (§18): the shed stations, the fan's phase, size and throw.
    pub seed: u32,
    /// Which shed fragments have already been born — bit `i` for station `i`
    /// (§6.5 layer 5). A fragment is born on the frame the head passes its
    /// station and never twice, which is what makes the sowing a pure function
    /// of the frame sequence rather than of the frame rate.
    pub shed: u8,
    /// Whether the arrival edge's marks have been minted (§6.7). One `Instant`,
    /// one minting: nothing at the landing fires early and nothing waits for a
    /// second observation.
    pub landed: bool,
    /// Whether this landing rings — false for an Enter and for a PTY line
    /// feed (D8, §6.11): the ring is reserved for same-row nav and for
    /// history recall, whichever axis the recall flies on.
    pub ring: bool,
}

impl Meteor {
    /// The arrival edge (§6.7, §8.1 no. 3): the single `Instant` every landing
    /// mark and the audio bell share.
    #[must_use]
    pub fn arrival(&self) -> Instant {
        self.t0 + self.t_flight
    }

    /// The path vector in px.
    #[must_use]
    pub fn delta(&self) -> (f32, f32) {
        (self.x1 - self.x0, self.y1 - self.y0)
    }

    /// Path length `L` in px.
    #[must_use]
    pub fn length(&self) -> f32 {
        let (dx, dy) = self.delta();
        dx.hypot(dy)
    }

    /// The train's spectrum position `cells_behind_landing` cells short of the
    /// landing: `tri(t_land − cells·(1/36)·gain)` at [`arc_gain`]'s rate.
    ///
    /// Measured from the landing, not the head: a station's colour depends on
    /// where on the row it is, so relative to the flying head the colours
    /// stream backwards. At the landing cell it is `tri(t_land)`, the caret's
    /// own reflected stop (D4) (owner, 2026-09-08).
    #[inline]
    #[must_use]
    pub fn arc(&self, cells_behind_landing: f32) -> f32 {
        tri(self.t_land - cells_behind_landing * WALK_LAY_RATE * arc_gain(self.cells))
    }

    /// Which rasterizer this flight takes (D15).
    #[must_use]
    pub fn axis(&self) -> Axis {
        let (dx, dy) = self.delta();
        Axis::of(dx, dy)
    }

    /// True once a crossing meteor has retired this one (§6.9); the edge
    /// itself is [`Meteor::retired_at`].
    #[must_use]
    #[cfg(test)]
    pub fn retiring(&self) -> bool {
        self.retired_at.is_some()
    }

    /// Whether the head left the glass (§6.9): true when it was handed over in
    /// flight, to a same-direction continuation ([`Meteor::chained`]) or to a
    /// crossing flight's 60 ms finish ([`Meteor::retired_at`] before the
    /// arrival edge). Such a train draws no nucleus or coma.
    ///
    /// False for a train retired at or after its arrival edge: its afterglow
    /// decays with the train instead of popping off. A minted landing
    /// ([`Meteor::landed`]) settles it without a clock comparison; the instant
    /// comparison covers a train retired on its arrival frame.
    #[must_use]
    pub fn handed_over(&self) -> bool {
        self.chained
            || self
                .retired_at
                .is_some_and(|at| !self.landed && at < self.arrival())
    }

    /// Whether this train has a head in the air at `at`: un-retired, not
    /// chained, not landed, and short of its arrival edge. §6.9's live cap
    /// evicts trains that answer false first, so a finishing remnant never
    /// costs another row's flight its head.
    #[must_use]
    pub fn in_flight_at(&self, at: Instant) -> bool {
        self.retired_at.is_none() && !self.chained && !self.landed && at < self.arrival()
    }

    /// The colour layer's alpha at `now` (§6.8, §6.9): the two-tau fade times
    /// the retire decay. §6.9's law is stated in this quantity.
    #[must_use]
    #[cfg(test)]
    pub fn colour_alpha(&self, now: Instant) -> f32 {
        let age = ms_since(self.t0, now);
        let t = self.t_flight.as_secs_f32() * 1000.0;
        let base = if age <= t {
            1.0
        } else {
            (-(age - t) / TRAIN_COLOUR_TAU_MS).exp()
        };
        base * self.retire_alpha(now)
    }

    /// The retire decay alone (§6.9) — `1` while the meteor is live.
    fn retire_alpha(&self, now: Instant) -> f32 {
        match self.retired_at {
            None => 1.0,
            Some(at) => {
                let u = ms_since(at, now) / FLIGHT_RETIRE_MS;
                (-u.max(0.0) * RETIRE_DECAY).exp()
            }
        }
    }

    /// The instant this meteor's last pixel leaves the glass: `T + 600`, or
    /// §6.9's `R = 60 ms` for a retired one.
    #[must_use]
    pub fn end(&self) -> Instant {
        match self.retired_at {
            Some(at) => at + Duration::from_secs_f32(FLIGHT_RETIRE_MS / 1000.0),
            None => self.arrival() + Duration::from_secs_f32(FLIGHT_OFF_GLASS_MS / 1000.0),
        }
    }
}

/// The landing pin (§6.5 layer 9): explicit arms plus a 2×2 nucleus, whipping
/// inward until the only pixel left is the caret.
#[derive(Clone, Copy, Debug)]
pub struct Pin {
    /// The arrival edge this pin was born on.
    pub at: Instant,
    /// The arc position its arms take, `tri(t_land)` — snapped through
    /// `spectrum_snap` (C1: point marks snap).
    pub t: f32,
}

/// The landing starburst: an isotropic set of tapered lances at true screen
/// angles around the caret, plus [`JET_N_PER_SIDE`] jets per side along the
/// flight axis carrying the distance grade. It runs on the quartic expansion
/// ([`RING_R_EXP`]), the graded life ([`ring_ms`]), the graded reach
/// ([`ring_full_radius`], which the jets ride) and the hold-then-spend
/// coverage ([`RING_COV_HOLD_U`]). Colour is the band's field at each
/// station's column ([`landing_field`], [`Landing::field`]).
///
/// Directions, lengths and the jets' tilt signs are hashed once at the mint
/// ([`mint_burst`]), as [`Landing::spark`] precomputes its throws, so the
/// frame path does no trigonometry or hashing (§18).
#[derive(Clone, Copy, Debug)]
pub struct Burst {
    /// The arrival edge.
    pub at: Instant,
    /// The landing's [`impact`] — 1.0 at the 8-cell floor. The jets read it
    /// through [`ring_full_radius`].
    pub scale: f32,
    /// This burst's life in ms — [`ring_ms`], unchanged, so the const assert
    /// that the graded life reaches [`FLIGHT_OFF_GLASS_MS`] exactly at the cap
    /// still holds the whole mark off glass by 600 ms.
    pub ms: f32,
    /// Live spikes, `<= BURST_N_MAX` ([`burst_n`]).
    pub n: u8,
    /// Each spike's unit screen direction; no ellipse or squash is involved.
    pub dir: [(f32, f32); BURST_N_MAX],
    /// Each spike's tip radius as a share of [`BURST_CORE_CH`],
    /// [`BURST_LEN_MIN`]`..=1.0`.
    pub len: [f32; BURST_N_MAX],
    /// The spikes' emit order, by descending length: [`BURST_QUAD_CAP`]'s shed
    /// order, fixed at the mint so an emitted spike stays in for the mark's
    /// life.
    pub order: [u8; BURST_N_MAX],
    /// The jets' unit screen directions, `k * 2 + side` (side 0 is leftward),
    /// resolved at the mint so the frame path does no trigonometry. The tilted
    /// jet's vertical sign is hashed per side, so the pair reads as a skid
    /// rather than a symmetric "V".
    pub jet_dir: [(f32, f32); JET_N_PER_SIDE * 2],
}

/// Hash one landing's whole starburst from its seed: [`burst_n`] directions
/// jittered inside their spacing, their lengths, the hero rhythm, the emit
/// order and the jets' tilt signs. Called once, at the arrival edge. The
/// directions lie on a true screen circle rotated by a hashed phase, so two
/// landings on one cell differ.
#[must_use]
fn mint_burst(seed: u32, at: Instant, cells: f32) -> Burst {
    let n = burst_n(cells);
    let step = std::f32::consts::TAU / n as f32;
    let phase = unit01(seed, 0x51) * step;
    let mut dir = [(0.0_f32, 0.0_f32); BURST_N_MAX];
    let mut len = [0.0_f32; BURST_N_MAX];
    for i in 0..n {
        let salt = (i as u32).wrapping_mul(0x9E37_79B9) ^ 0x00B5;
        let a = phase + (i as f32 + BURST_JITTER * (unit01(seed, salt) - 0.5)) * step;
        dir[i] = (a.cos(), a.sin());
        let raw = BURST_LEN_MIN + (1.0 - BURST_LEN_MIN) * unit01(seed, salt ^ 0x1D);
        len[i] = if i % BURST_HERO_EVERY == 0 {
            (raw * BURST_HERO_GAIN).min(1.0)
        } else {
            raw
        };
    }
    // Descending length: an insertion sort over at most twelve slots, so the
    // shed order costs no allocation and no comparator.
    let mut order = [0_u8; BURST_N_MAX];
    for (i, slot) in order.iter_mut().enumerate() {
        *slot = i as u8;
    }
    for i in 1..n {
        let mut j = i;
        while j > 0 && len[usize::from(order[j - 1])] < len[usize::from(order[j])] {
            order.swap(j - 1, j);
            j -= 1;
        }
    }
    // The jets' directions, resolved once: `cos`/`sin` of a constant tilt, the
    // vertical component squashed by [`JET_RISE_SQUASH`] and signed per side.
    let mut jet_dir = [(0.0_f32, 0.0_f32); JET_N_PER_SIDE * 2];
    for k in 0..JET_N_PER_SIDE {
        let (ct, st) = (JET_TILT_RAD[k].cos(), JET_TILT_RAD[k].sin());
        for si in 0..2 {
            let side = if si == 0 { -1.0_f32 } else { 1.0 };
            let sign = if unit01(seed, 0x7A ^ si as u32) < 0.5 {
                -1.0_f32
            } else {
                1.0
            };
            let (rx, ry) = (ct * side, st * JET_RISE_SQUASH * sign);
            let inv = (rx * rx + ry * ry).sqrt().max(1e-6).recip();
            jet_dir[k * 2 + si] = (rx * inv, ry * inv);
        }
    }
    Burst {
        at,
        scale: impact(cells),
        ms: ring_ms(cells),
        n: n as u8,
        dir,
        len,
        order,
        jet_dir,
    }
}

/// The landing fan (§6.5 layer 11) — "every landing is its own party". What
/// the meteor decides about the fan; the throw itself is the sky's
/// ([`Stardust::sow_fan`]).
#[derive(Clone, Copy, Debug)]
pub struct Fan {
    /// The arrival edge.
    pub at: Instant,
    /// Star count, `min(19 + 3.6·(impact − 1), 28) + party·8`,
    /// ≤ [`super::timing::FAN_MAX_N`].
    pub n: u8,
    /// Throw reach in px: how far along the line the fan is thrown over the
    /// sky's throw window. The sky jitters each star's share and caps the rise
    /// (`stardust::FAN_RISE_MAX_CH`): distance buys length, not height.
    pub reach: f32,
    /// The seed that varies count, phase, size and throw — the whole of the
    /// party's variance, and deterministic (§18).
    pub seed: u32,
    /// Whether this landing carries its gold m1 hero. False for an Enter (D8).
    pub hero: bool,
}

/// One spark's throw, resolved at the arrival edge from the landing's seed so
/// the frame path does no hashing or trigonometry: birth velocity in px/ms and
/// life in ms. Position at `age` is `v·age + ½g·age²` ([`spark_at`]), a pure
/// function of age (§18).
#[derive(Clone, Copy, Debug, Default)]
pub struct Spark {
    /// Birth velocity along the row, px/ms.
    pub vx: f32,
    /// Birth velocity down the glass, px/ms (negative is UP).
    pub vy: f32,
    /// Life, ms.
    pub life: f32,
    /// The star's arm half-length while bright, px
    /// ([`SPARK_ARM_MIN_PX`]..=[`SPARK_ARM_MAX_PX`]).
    pub arm: u8,
}

/// What the sky's probe said about the landing-row cells the flash and jets
/// may light, latched once per landing on the frame after the mint
/// ([`Meteors::sow_into`], the meteor's one sight of the probe) and read for
/// its life. Bit `k` of [`SkyMask::row`] is the cell `k − JET_PROBE_CELLS`
/// columns from the caret, set where that cell is provably blank
/// (`Some(false)`, §5.4 / L4; unknown is not blank). The starburst's core
/// crosses the rows above and below at the transient ceiling without a
/// licence. Latched because a frame is a function of `now` and the landing
/// (§18), and the probe belongs to the sky.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkyMask {
    /// Blank cells of the landing row, `2·JET_PROBE_CELLS + 1` bits centred on
    /// the caret's column. The flash reads its three ([`SkyMask::flash_blank`])
    /// and the jets the whole span ([`SkyMask::row_blank`]): one probe, two
    /// readers.
    pub row: u32,
}

impl SkyMask {
    /// Ask the probe about every cell a landing at `(x, y)` may light.
    #[must_use]
    fn probe(x: f32, y: f32, probe: &GlyphProbe, geom: Geom) -> Self {
        let (cw, ch) = ((geom.cw as f32).max(1.0), geom.ch as f32);
        let blank = |dx: i32, dy: i32| {
            let px = (x + dx as f32 * cw).round() as i32;
            let py = (y + dy as f32 * ch).round() as i32;
            probe.at_px(px, py, geom) == Some(false)
        };
        let mut mask = Self::default();
        for k in -JET_PROBE_CELLS..=JET_PROBE_CELLS {
            if blank(k, 0) {
                mask.row |= 1_u32 << (k + JET_PROBE_CELLS);
            }
        }
        mask
    }

    /// Is the landing row's cell `k` columns from the caret proven blank?
    /// The jets' gate — a station over a cell this says no about breaks the
    /// polyline, so the mark degrades to the bare core star over dense text.
    #[must_use]
    fn row_blank(self, k: i32) -> bool {
        if !(-JET_PROBE_CELLS..=JET_PROBE_CELLS).contains(&k) {
            return false;
        }
        self.row & (1_u32 << (k + JET_PROBE_CELLS)) != 0
    }

    /// The flash's own three cells of the same row.
    #[must_use]
    fn flash_blank(self, k: i32) -> bool {
        self.row_blank(k)
    }
}

/// The marks the arrival edge mints together (§17.1's `Landing{pin, ring,
/// fan}`, plus the starburst, flash and sparks). They share one `Instant`;
/// nothing fires early or waits for a second observation (§6.7).
#[derive(Clone, Copy, Debug)]
pub struct Landing {
    /// Always present — the pin IS the caret (§6.5 layer 9, D8).
    pub pin: Pin,
    /// The starburst, the impact's one big mark (§6.5 layer 10). Absent on an
    /// Enter / vertical landing (D8: an Enter lands small). A celebration's
    /// fourth bar carries a burst but no impact flash.
    pub burst: Option<Burst>,
    /// A meteor impact licenses the flash. A celebration bar does not.
    pub flash: bool,
    /// Always present; its composition varies (D8).
    pub fan: Fan,
    /// Window-absolute X of the landing cell's centre, px. The pin closes onto
    /// this point, which is the caret's own; carried on the landing because a
    /// landing outlives the meteor that minted it.
    pub x: f32,
    /// Window-absolute Y of the landing cell's centre, px.
    pub y: f32,
    /// The landing's walk ([`LandingWalk`]): the caret's field at the landing
    /// cell, latched raw at the spawn edge ([`Meteor::t_land`]), with the
    /// band's walk origin ([`Meteor::band`]). Every landing mark (burst, jets,
    /// flash, sparks, and the sky's fan) reads its colour through
    /// [`Landing::field`]. Folded once at the read, as the ribbon's own is, so
    /// the station under the caret is the caret's stop.
    pub walk: LandingWalk,
    /// How many of [`Landing::spark`] this impact throws ([`SPARK_N_BASE`]);
    /// zero for the small landing (D8: an Enter lands small).
    pub sparks: u8,
    /// The sparks' throws, the first [`Landing::sparks`] of them live —
    /// hashed once at the mint from the fan's own seed, so one landing's
    /// whole party is one number (§18) and the frame path only adds.
    pub spark: [Spark; SPARK_N_MAX],
    /// What the sky's probe said about the cells of the landing row the
    /// flash and the jets may light — `None` until the hand-off after the
    /// mint latches it ([`Meteors::sow_into`]). On the mint frame itself the
    /// flash draws at the transient cap everywhere and the jets draw nothing
    /// (the star has not cleared the caret's disc on that frame anyway).
    pub sky: Option<SkyMask>,
}

impl Landing {
    /// The band's walk through this landing ([`LandingWalk::at`]) `dx_cells`
    /// right of the landing cell. `0.0` is the landing cell's own stop, the
    /// pin's colour.
    #[inline]
    #[must_use]
    pub fn field(&self, dx_cells: f32) -> f32 {
        self.walk.at(dx_cells)
    }

    /// When the last landing pixel leaves the glass: the pin closes at
    /// [`PIN_MS`], the flash's colour at [`FLASH_COLOUR_MS`], the starburst at
    /// its graded life ([`Burst::ms`], [`RING_MS`] at the floor), the last
    /// spark at [`SPARK_LIFE_MAX_MS`]. The fan is stars, the sky's pool, so it
    /// is not counted. Under reduced motion the static form lives the pool's
    /// whole horizon ([`super::timing::FLIGHT_OFF_GLASS_MS`]) and leaves on the
    /// theme's one linear fade.
    #[must_use]
    pub fn end(&self) -> Instant {
        let burst_ms = self.burst.map_or(0.0, |b| b.ms);
        let life = PIN_MS
            .max(FLASH_COLOUR_MS)
            .max(burst_ms)
            .max(SPARK_LIFE_MAX_MS);
        self.pin.at + Duration::from_secs_f32(life / 1000.0)
    }

    /// A party landing (`RAINBOW-KITTY-V2.md` §27): the celebration's bar fan,
    /// minted at the caret with no meteor behind it. `n` stars thrown ROYGBIV
    /// from the caret's stop (`tint_t`), the pin closing onto the caret, the
    /// starburst only when `ring` (every fourth bar), and no flash, sparks or
    /// gold hero: a bar line is a beat, not an impact. Reach is the fan's floor
    /// ([`FAN_REACH_BASE_CH`]), so it clears the caret cell inside the m2 hold
    /// and leaves with the sky's transient lives (≤ 315 ms). Seeded by `seed`
    /// (§18).
    #[must_use]
    pub fn party(
        at: Instant,
        at_px: (f32, f32),
        n: u8,
        ring: bool,
        seed: u32,
        ch: f32,
        tint_t: f32,
    ) -> Self {
        let n = usize::from(n).clamp(1, super::timing::FAN_MAX_N) as u8;
        Self {
            // D4 at the party too: the caret's field is the raw walk and
            // the pin is its folded stop — `tri` once, at the read.
            pin: Pin { at, t: tri(tint_t) },
            // Off any band: a party has no landing cell on a laid run to
            // read an origin from, so its walk is `d/16` from the caret.
            walk: LandingWalk::free(tint_t),
            burst: ring.then(|| mint_burst(mix32(seed ^ 0xB0B5), at, f32::from(JUMP_MIN_CELLS))),
            flash: false,
            fan: Fan {
                at,
                n,
                reach: FAN_REACH_BASE_CH * ch,
                seed,
                hero: false,
            },
            x: at_px.0,
            y: at_px.1,
            sparks: 0,
            spark: [Spark::default(); SPARK_N_MAX],
            sky: None,
        }
    }

    /// The landing's end under reduced motion: the pool's own horizon, so
    /// the static form is on glass exactly as long as the static train is.
    #[must_use]
    fn static_end(&self) -> Instant {
        self.pin.at + Duration::from_secs_f32(FLIGHT_OFF_GLASS_MS / 1000.0)
    }

    /// The reduced-motion form's alpha at `now`: `1`, then the theme's one
    /// linear fade over [`REDUCED_MOTION_FADE_MS`] to [`Landing::static_end`]
    /// (§6.11, §2.5's single exception).
    #[must_use]
    fn static_alpha(&self, now: Instant) -> f32 {
        let left = self
            .static_end()
            .saturating_duration_since(now)
            .as_secs_f32()
            * 1000.0;
        clamp01(left / REDUCED_MOTION_FADE_MS.max(1.0))
    }

    /// The last instant any landing mark is in motion: the pin closes through
    /// [`PIN_MS`], the flash turns white → spectrum through
    /// [`FLASH_COLOUR_MS`], the starburst expands through its graded life,
    /// and every spark is ballistic until its chroma cull ([`spark_cull_u`] of
    /// its life). Past this nothing of the landing draws and the pool only
    /// waits for [`Landing::end`]. The cadence law's motion bound
    /// ([`Meteors::brisk`]): the big landing is still by `T + 516`, an Enter's
    /// small landing (pin only) by `T + 150`.
    #[must_use]
    pub fn moving_until(&self) -> Instant {
        let mut ms = PIN_MS;
        if let Some(b) = self.burst {
            ms = ms.max(b.ms).max(FLASH_COLOUR_MS);
        }
        let cull = spark_cull_u();
        for s in self.spark.iter().take(usize::from(self.sparks)) {
            ms = ms.max(s.life * cull);
        }
        self.pin.at + Duration::from_secs_f32(ms / 1000.0)
    }
}

/// The share of a spark's life at which it falls under the chroma cull and
/// [`spark_at`] stops drawing it: alpha is `spend((u − SPARK_HOLD_U) /
/// (1 − SPARK_HOLD_U))`, under [`CHROMA_CULL_ALPHA`] from
/// `u = SPARK_HOLD_U + (1 − SPARK_HOLD_U)·(1 − √CHROMA_CULL_ALPHA)` ≈ 0.861.
/// Solved rather than spelled so the cadence law's cull and the frame path's
/// agree (`a_spark_is_culled_exactly_where_the_cadence_law_says_it_is`).
#[must_use]
fn spark_cull_u() -> f32 {
    SPARK_HOLD_U + (1.0 - SPARK_HOLD_U) * (1.0 - CHROMA_CULL_ALPHA.sqrt())
}

/// What a spawn minted, handed back to `Engine` so the seam wiring lives in
/// one place: the companion impulse (§7.2), the audio gesture (§12) and the
/// glint bucket's emptying (§13) are engine-level effects, not meteor-level
/// ones.
#[derive(Clone, Copy, Debug)]
pub struct Spawn {
    /// The spawn edge.
    pub t0: Instant,
    /// §6.2's `T`, so the bell's `Voice::delay` and the kitty's `land_at` read
    /// the same number the pixels do.
    pub t_flight: Duration,
    /// Travel direction — the kitty's facing and the cue's `dir`.
    pub dir: Dir,
    /// Path length in cells (§6.2), as the integer the audio receives
    /// (`SoundKind::Meteor { cells: u16 }`). `T` is computed from this number,
    /// so `timing::flight_ms(cells)` at the synth resolves to the same
    /// `Instant` the pin and the squash read (§8.1 no. 3).
    pub cells: u16,
    /// The landing cell.
    pub landing: (u16, u16),
    /// Whether this landing rings (false for an Enter or a PTY line feed,
    /// D8).
    pub ring: bool,
    /// Whether this landing cues the five rain glints (false for an Enter or
    /// a PTY line feed, D8).
    pub rain: bool,
}

/// One star birth the meteor decided, waiting for the sky (§6.5 layers 5 and
/// 11, §6.12): everything a stardust entry point needs. The sky adds
/// clearance, velocity, envelope and the star itself.
#[derive(Clone, Copy, Debug)]
enum Sow {
    /// One shed fragment at its station, born the frame the head passed it.
    Shed {
        /// The birth edge — the frame the head passed the station.
        at: Instant,
        /// The station, the head's velocity there, the arc colour there.
        spec: ShedSow,
    },
    /// The landing fan, on the arrival edge.
    Fan {
        /// The arrival edge.
        at: Instant,
        /// Reach, count, hero, seed, landing pixel, landing colour.
        spec: FanSow,
    },
    /// A sub-floor hop's mini-fan (§6.12, D17).
    MiniFan {
        /// The hop's observed edge.
        at: Instant,
        /// The landing cell's centre, px.
        at_px: (f32, f32),
        /// The hop's seed.
        seed: u32,
    },
}

/// The meteor producer: the live pool, its landings, and the station scratch.
#[derive(Clone, Debug)]
pub struct Meteors {
    /// The meteor pool (§18): at most [`FLIGHT_MAX_LIVE`] heads in the air
    /// (§6.9; a third evicts the oldest through the same 60 ms finish), and at
    /// most [`METEOR_POOL`] entries in all, finishing trains included.
    /// Reserved once, never grown.
    pub live: Vec<Meteor>,
    /// The landing pool: one per arrived meteor, at most [`LANDING_POOL`].
    pub landings: Vec<Landing>,
    /// Star births this producer decides but does not build: shed fragments
    /// (§6.5 layer 5), the landing fan (layer 11) and the sub-floor mini-fan
    /// (§6.12), staged for the sky. Resident and drained, never rebuilt (§18).
    /// See [`Meteors::sow_into`].
    sown: Vec<Sow>,
    /// Reused station scratch for `ribbon_beam` / `ribbon_beam_v` (§18's
    /// "a reused 96-station vertex scratch"). Taken with `mem::take` for the
    /// duration of an emit and put back, so the steady-state frame path
    /// allocates nothing.
    verts: Vec<RibbonVertex>,
    /// The spawn ordinal, the deterministic stand-in for `born` in §18's
    /// "hashed from `(row, col, born)`". `aterm_time::Instant` exposes no bits
    /// to hash, so the seed counts spawns since the last [`Meteors::reset`]: a
    /// pure function of the event sequence, which keeps repeated identical
    /// round-trips distinct (`every_landing_is_its_own_party`).
    ord: u32,
    /// The reduced-motion posture of the last frame drawn, cached for the
    /// cadence law: under it nothing flies and nothing moves (§6.11), so
    /// the pool is never brisk and its one change is the theme's linear
    /// fade at the end of the life.
    reduced: bool,
    /// The hand's floor (`ribbon::Ribbon::hand_floor`), `(row, col)`, refreshed
    /// by `Engine::tick` before the frame's events reach this pool and read
    /// only by the shower's wall at the mint ([`spark_room`]). `None` bounds
    /// nothing.
    hand_floor: Option<(u16, u16)>,
}

impl Default for Meteors {
    /// The same pool [`Meteors::new`] builds — `Engine::new` reaches this
    /// through `Default`, and a derived `Default` would hand it four empty
    /// `Vec`s whose first spawn and first emit allocate (§18).
    fn default() -> Self {
        Self::new()
    }
}

impl Meteors {
    /// An empty pool, with every resident buffer reserved to its published
    /// depth so neither the spawn path nor the frame path allocates (§18).
    #[must_use]
    pub fn new() -> Self {
        Self {
            live: Vec::with_capacity(METEOR_POOL),
            landings: Vec::with_capacity(LANDING_POOL),
            sown: Vec::with_capacity(SOW_SCRATCH),
            verts: Vec::with_capacity(STATIONS_MAX),
            ord: 0,
            reduced: false,
            hand_floor: None,
        }
    }

    /// Set the hand's floor: `Engine::tick` passes the ribbon's
    /// `Ribbon::hand_floor` before dealing the frame's events, so a landing
    /// minted this frame is bounded by the floor as it stands now, including
    /// the jump's own relocation. `None` means unknown and bounds nothing.
    pub fn set_hand_floor(&mut self, floor: Option<(u16, u16)>) {
        self.hand_floor = floor;
    }

    /// The floor this pool holds, as last set by [`Meteors::set_hand_floor`].
    /// The seam reads it to pin the engine's wiring; the shower's law itself
    /// is measured in columns of ink (`the_shower_stops_where_the_hand_stopped`).
    /// Named `_cell` so the field's doc link stays unambiguous.
    #[must_use]
    #[cfg(test)]
    pub fn hand_floor_cell(&self) -> Option<(u16, u16)> {
        self.hand_floor
    }

    /// Ingest one engine event; mint a meteor if it earns one. Returns
    /// [`Spawn`] exactly when a flight was born, so the engine mints the
    /// companion impulse, starts the audio gesture and empties the glint
    /// bucket on the same edge.
    ///
    /// §6.1's trigger: a licensed same-row `|dc| ≥` [`JUMP_MIN_CELLS`], or any
    /// row change the arms license (nav-hinted `|dr| ≥ 1`, a
    /// `return_licensed` cold Enter, a synthetic preview, a PTY line feed).
    /// Typed wraps never fly and reflow licenses nothing: a [`Licence::Typed`]
    /// move mints nothing (`ink_wrap_rainbow_keeps_the_ribbon_no_zoom`). A
    /// 2-7-cell same-row hop is a mini-fan, not a flight (§6.12, D17). A meteor
    /// is not gated on a live ribbon: a cold `Home`/`End` on a dead prompt
    /// flies the full anatomy.
    pub fn on_event(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>) -> Option<Spawn> {
        match *ev {
            Event::Move {
                from,
                to,
                licence,
                dir,
            } => self.on_move(from, to, licence, dir, at, ctx),
            // §8.2: focus loss embers everything out. The meteors take the
            // same 60 ms finish a crossing flight would have given them —
            // one retire law, not two.
            Event::Focus(false) => {
                for m in &mut self.live {
                    if m.retired_at.is_none() {
                        m.retired_at = Some(at);
                    }
                }
                None
            }
            // T1, restated where it would be easiest to break: a keypress is
            // not a move. `Event::Return` is seam point 2's `note_return` —
            // the key — and the caret's own observed motion arrives as the
            // `Event::Move` that carries `Licence::Return`. Minting a flight
            // here would draw a meteor for an Enter the shell swallowed.
            _ => None,
        }
    }

    /// Draw the meteors and their landings.
    ///
    /// Called first in the emit order (§6.5): truncation sheds from the end,
    /// and on a monster jump the train alone can approach the quad budget.
    ///
    /// Layer 1 (colour train) goes into `frame.under` through `ribbon_beam` /
    /// `ribbon_beam_v` by [`Axis`], self-capped at [`UNDER_QUAD_CAP`] by
    /// shedding stations from the tail; layers 2 and 3 (white train, shoulder)
    /// into `frame.out` under [`WHITE_QUAD_CAP`]; layers 6 and 7 (coma,
    /// nucleus) into `frame.halos`; the impact (starburst, flash, pin, sparks)
    /// into `frame.out`. Layers 5 and 11 (shed fragments, fan) are minted onto
    /// [`Meteors::sown`] rather than drawn (see [`Meteors::sow_into`]).
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.reduced = ctx.cfg.reduced_motion;
        let now = ctx.now;
        self.live.retain(|m| now < m.end());
        self.landings.retain(|l| now < l.end());
        if self.live.is_empty() && self.landings.is_empty() {
            return;
        }

        // §18: the station scratch is resident. Taking it lends the vector out
        // for the emit and hands it straight back, so nothing allocates and
        // nothing borrows `self` twice.
        let mut verts = mem::take(&mut self.verts);
        // Copied out before the pools are lent: the floor is a `Copy` pair,
        // and the mint below needs it while `live` is borrowed.
        let hand_floor = self.hand_floor;
        let (live, landings, sown) = (&mut self.live, &mut self.landings, &mut self.sown);

        for m in live.iter_mut() {
            if let Some(f) = Flight::of(m, ctx) {
                draw_train(&mut verts, m, &f, ctx, frame);
                draw_head(m, &f, ctx, frame);
                // §6.9: a head handed over (to a crossing flight's 60 ms
                // finish or a chained continuation) is off the glass and sheds
                // nothing (§19.1).
                if m.retired_at.is_none() && !m.chained {
                    shed(m, &f, ctx, sown);
                }
            }
            // §6.7: the arrival edge is one instant, spent only here. A meteor
            // retired before it arrived mints nothing (the caret has left),
            // nor does a chained train (the continuation's landing celebrates,
            // §6.9).
            if !m.landed && !m.chained && now >= m.arrival() && m.retired_at.is_none() {
                m.landed = true;
                // Seam point 12: a scroll that carried the endpoint off the
                // glass took the landing with it. Nothing is minted — a pin
                // clamped onto the top row would be light on a line nobody
                // flew to, and a fan sown above the glass would sit in the
                // sky's pool for 460 ms drawing nothing.
                if !on_glass(ctx.geom, m.x1, m.y1) {
                    continue;
                }
                let landing = mint_landing(m, ctx, hand_floor);
                // §6.5 layer 11: the fan wears the landing's walk, the band's
                // colour at each star's rest column, so fan, pin and band read
                // as one landing.
                sown.push(Sow::Fan {
                    at: landing.fan.at,
                    spec: FanSow {
                        at_px: (landing.x, landing.y),
                        reach: landing.fan.reach,
                        walk: landing.walk,
                        seed: landing.fan.seed,
                        n: landing.fan.n,
                        hero: landing.fan.hero,
                    },
                });
                // §6.11: under reduced motion the landing is minted too —
                // its static form (the flash and the colours, no expansion,
                // no pin, no sparks) is what each `draw_*` below draws when
                // the config says so.
                if landings.len() >= LANDING_POOL {
                    landings.remove(0);
                }
                landings.push(landing);
            }
        }
        // The impact, bottom → top, all `out`: the starburst, the flash, the
        // pin and the sparks. The fan is the sky's. Spark halos share one
        // budget across the landings.
        let halo_cap = frame.halos.len() + SPARK_HALO_CAP;
        for l in landings.iter() {
            draw_burst(l, ctx, frame);
            draw_flash(l, ctx, frame);
            draw_pin(l, ctx, frame);
            draw_sparks(l, ctx, frame, halo_cap);
        }

        self.verts = verts;
    }

    /// The party (§27): stage one bar fan at the caret for the sky and, on a
    /// ring bar, pool the [`Landing::party`] that draws the pin and the
    /// starburst. Called from `Engine::tick` on the frame the host first saw
    /// the bar, with that frame's `ctx`. A caret off the glass mints nothing.
    /// Seeded by the spawn ordinal (§18); the pools are resident, so nothing
    /// allocates.
    pub fn party(&mut self, at: Instant, n: u8, ring: bool, ctx: &Ctx<'_>) {
        let (x, y) = ctx.geom.cell_center(ctx.caret.0, ctx.caret.1);
        if !on_glass(ctx.geom, x, y) {
            return;
        }
        self.ord = self.ord.wrapping_add(1);
        let seed = seed_of(ctx.caret.0, ctx.caret.1, self.ord);
        let landing = Landing::party(at, (x, y), n, ring, seed, ctx.geom.ch as f32, ctx.caret_t);
        self.sown.push(Sow::Fan {
            at,
            spec: FanSow {
                at_px: (x, y),
                reach: landing.fan.reach,
                walk: landing.walk,
                seed,
                n: landing.fan.n,
                hero: false,
            },
        });
        if ring {
            if self.landings.len() >= LANDING_POOL {
                self.landings.remove(0);
            }
            self.landings.push(landing);
        }
    }

    /// Hand the decided births to the sky: the shed fragments (§6.5 layer 5),
    /// the landing fan (layer 11) and the sub-floor mini-fan (§6.12).
    ///
    /// Every star is built and drawn by [`Stardust`] from its one recipe
    /// (§5.7, D2). These are meteor-lane stars (transient pricing, exempt from
    /// the typing glint bucket, silent), and the sky's transient-lane entry
    /// points are their only way in: [`Stardust::sow_shed`] (§5.6's
    /// along-plus-sputter velocity), [`Stardust::sow_fan`] (the radial throw)
    /// and [`Stardust::sow_mini_fan`] (the 1-2 px hop), each applying §5.4's
    /// glyph clearance before a star exists. The meteor never builds a `Star`.
    ///
    /// `ctx` is the frame [`Meteors::emit`] just drew with, so the stars are
    /// cleared and tinted against the geometry, config and clock that minted
    /// them. `Engine::tick` calls it right after `self.meteor.emit(&ctx, out)`:
    ///
    /// ```text
    /// self.meteor.sow_into(&mut self.stardust, &ctx);
    /// ```
    pub fn sow_into(&mut self, dust: &mut Stardust, ctx: &Ctx<'_>) {
        // The one sight of the probe: a landing minted this frame asks the sky
        // once which cells its flash and jets may light are provably blank,
        // and keeps the answer (`SkyMask`).
        for l in &mut self.landings {
            if l.sky.is_none() {
                l.sky = Some(SkyMask::probe(l.x, l.y, dust.probe(), ctx.geom));
            }
        }
        for sow in self.sown.drain(..) {
            match sow {
                Sow::Shed { at, spec } => dust.sow_shed(at, spec, ctx),
                Sow::Fan { at, spec } => dust.sow_fan(at, spec, ctx),
                Sow::MiniFan { at, at_px, seed } => dust.sow_mini_fan(at, at_px, seed, ctx),
            }
        }
    }

    /// Meteors with a head in the air or a train on its natural finish:
    /// `trail status`'s `meteors=`, ≤ [`FLIGHT_MAX_LIVE`] by §6.9's cap. A
    /// train on the 60 ms retire is a finish and is not counted
    /// (`Meteors::at_rest` still waits for it). Landed trains and chained stubs
    /// count; the cap in [`Meteors::on_event`] retires them before a head in
    /// flight.
    #[must_use]
    pub fn live(&self) -> usize {
        self.live.iter().filter(|m| m.retired_at.is_none()).count()
    }

    /// True when nothing is in the air and no landing is finishing. Every
    /// meteor pixel is gone by `T + 600` (§6.2), so this latches within 720 ms
    /// of the last flight.
    #[must_use]
    #[cfg(test)]
    pub fn at_rest(&self) -> bool {
        self.live.is_empty() && self.landings.is_empty()
    }

    /// The cadence law's brisk half: whether per-frame motion is on glass. A
    /// head in flight, a colour root still retracting toward the landing
    /// (§6.8's `R = 460`), a retiring train slurping into its frozen head
    /// (§6.9's `R = 60`), or a landing still moving ([`Landing::moving_until`]).
    /// This is seam point 8's word: the host's phase-locked train draws every
    /// frame of it, one wake per lane tick.
    ///
    /// Not "the pool is non-empty": the landing is still by `T + 516` (an
    /// Enter's by `T + 150`) while the pool's horizon is `T + 600`. Under
    /// reduced motion nothing moves (§6.11), so this is `false`.
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        if self.reduced {
            return false;
        }
        let root_suck = Duration::from_secs_f32(ROOT_SUCK_MS / 1000.0);
        self.live.iter().any(|m| match m.retired_at {
            Some(_) => now < m.end(),
            None => now < m.arrival() + root_suck,
        }) || self.landings.iter().any(|l| now < l.moving_until())
    }

    /// The attack: the part of a meteor's life the engine asks for every frame
    /// at [`super::FRAME_CADENCE`] (`a_flight_asks_for_every_frame`). The
    /// flight, then the pin's arms whipping into the caret with the white
    /// layer closing on them (`T .. T + 150`, [`PIN_MS`] = [`WHITE_CLOSE_MS`]),
    /// and a retiring train's 60 ms finish (§6.9). What moves after it is the
    /// release, which [`Meteors::brisk`] owns but the fold does not (see
    /// [`Meteors::next_change_deadline`]).
    fn attack(&self, now: Instant) -> bool {
        if self.reduced {
            return false;
        }
        let close = Duration::from_secs_f32(PIN_MS / 1000.0);
        self.live.iter().any(|m| match m.retired_at {
            Some(_) => now < m.end(),
            None => now < m.arrival() + close,
        }) || self.landings.iter().any(|l| now < l.pin.at + close)
    }

    /// The cadence law: the next instant a meteor changes what is on glass,
    /// `None` at rest (T6). Three phases, never two at once:
    ///
    /// * the attack ([`Meteors::attack`], `t₀ .. T + 150`): the fold's brisk,
    ///   the next frame;
    /// * the release (past it, while [`Meteors::brisk`]): brisk to seam point
    ///   8 alone. The fold names only [`super::TAIL_FLOOR`]; the host's train
    ///   draws every frame of the motion, and every u8 step of the release's
    ///   fades is under the floor, so the floor is its next visible step;
    /// * the horizon (nothing moves; the pool waits for `T + 600`): a
    ///   disappearance, offered as a tail on the floor grid.
    ///
    /// The release is not the fold's brisk because the host's seam takes the
    /// engine's deadline under its own lane tick
    /// (`CursorGlow::next_change_deadline` is the `min`), so a brisk producer
    /// would wake the seam twice per lane tick; the law is one wake per lane
    /// tick.
    ///
    /// Under reduced motion the mark is static until the theme's linear fade
    /// opens ([`REDUCED_MOTION_FADE_MS`] before the end), then a tail at the
    /// floor.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        let mut cad = Cadence::at(now);
        if self.attack(now) {
            cad.brisk();
        } else if self.brisk(now) {
            cad.tail(0.0);
        }
        for m in &self.live {
            if self.reduced {
                let fade_from = m.end() - Duration::from_secs_f32(REDUCED_MOTION_FADE_MS / 1000.0);
                if m.retired_at.is_none() && now < fade_from {
                    cad.tail_at(fade_from);
                } else {
                    cad.tail(0.0);
                }
            }
            cad.tail_at(m.end());
        }
        for l in &self.landings {
            if self.reduced {
                // §6.11: the static landing changes once — when its linear
                // fade opens — and then steps at the floor to its end.
                let fade_from =
                    l.static_end() - Duration::from_secs_f32(REDUCED_MOTION_FADE_MS / 1000.0);
                if now < fade_from {
                    cad.tail_at(fade_from);
                } else {
                    cad.tail(0.0);
                }
                cad.tail_at(l.static_end());
            } else {
                cad.tail_at(l.end());
            }
        }
        cad.take()
    }

    /// Translate live geometry by a scroll (seam point 12). Marks that leave
    /// the grid are dropped rather than clamped. The arrival mints at the
    /// flight's moved pixel endpoint, so an endpoint scrolled off the glass
    /// mints nothing ([`Meteors::emit`]).
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        let dy = f32::from(rows) * f32::from(cell_h);
        if dy == 0.0 {
            return;
        }
        for m in &mut self.live {
            m.y0 -= dy;
            m.y1 -= dy;
        }
        for l in &mut self.landings {
            l.y -= dy;
        }
        for s in &mut self.sown {
            match s {
                Sow::Shed { spec, .. } => spec.at_px.1 -= dy,
                Sow::Fan { spec, .. } => spec.at_px.1 -= dy,
                Sow::MiniFan { at_px, .. } => at_px.1 -= dy,
            }
        }
        self.live.retain(|m| m.y0.max(m.y1) >= 0.0);
        self.landings.retain(|l| l.y >= 0.0);
        self.sown.retain(|s| {
            let y = match s {
                Sow::Shed { spec, .. } => spec.at_px.1,
                Sow::Fan { spec, .. } => spec.at_px.1,
                Sow::MiniFan { at_px, .. } => at_px.1,
            };
            y >= 0.0
        });
    }

    /// [`Meteors::translate_scroll`]'s row-band twin (seam point 12): screen
    /// rows `top..=bottom` moved by `delta` and every other row stood still;
    /// flights, landings and sown hand-offs (window-absolute px) move under
    /// [`BandPx`]'s law. Landings and sows are points: inside the band they
    /// move and are kept iff still inside; outside they are untouched. A
    /// flight is a span: both endpoints inside moves it, neither leaves it,
    /// one of each drops it (and it mints no landing).
    ///
    /// With no cell height yet (`cell_h == 0`) every pixel mark is dropped,
    /// failing closed as `CursorGlow::translate_band_state` does.
    pub fn translate_band(
        &mut self,
        top: u16,
        bottom: u16,
        delta: i16,
        cell_h: u16,
        origin_y: u16,
    ) {
        if delta == 0 || top > bottom {
            return;
        }
        if cell_h == 0 {
            self.live.clear();
            self.landings.clear();
            self.sown.clear();
            return;
        }
        let px = BandPx::new(top, bottom, delta, cell_h, origin_y);
        self.live.retain_mut(|m| px.span(&mut m.y0, &mut m.y1));
        self.landings.retain_mut(|l| px.point(&mut l.y));
        self.sown.retain_mut(|s| match s {
            Sow::Shed { spec, .. } => px.point(&mut spec.at_px.1),
            Sow::Fan { spec, .. } => px.point(&mut spec.at_px.1),
            Sow::MiniFan { at_px, .. } => px.point(&mut at_px.1),
        });
    }

    /// Drop everything in the air.
    pub fn reset(&mut self) {
        self.live.clear();
        self.landings.clear();
        self.sown.clear();
        self.verts.clear();
        self.ord = 0;
    }

    // -- §6.1 the trigger, §6.9 the retire law -----------------------------

    fn on_move(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        licence: Licence,
        dir: Dir,
        at: Instant,
        ctx: &Ctx<'_>,
    ) -> Option<Spawn> {
        // §6.1: typed wraps never fly, and reflow licenses nothing. A typed
        // echo lays ribbon; it does not streak — and neither does a delivered
        // insert, whose span its own sweep laid.
        if licence.is_echo() {
            return None;
        }
        let drow = i32::from(to.0) - i32::from(from.0);
        let dcol = i32::from(to.1) - i32::from(from.1);
        if drow == 0 {
            let n = dcol.unsigned_abs();
            if n < u32::from(JUMP_MIN_CELLS) {
                // §6.12 / D17: a 2-7-cell hop is a mini-fan and not a flight.
                // Below two cells it is scrubbing, and scrubbing stays calm.
                if (u32::from(MINI_FAN_MIN_CELLS)..=u32::from(MINI_FAN_MAX_CELLS)).contains(&n) {
                    self.ord = self.ord.wrapping_add(1);
                    self.sown.push(Sow::MiniFan {
                        at,
                        at_px: ctx.geom.cell_center(to.0, to.1),
                        seed: seed_of(to.0, to.1, self.ord),
                    });
                }
                return None;
            }
        }

        let geom = ctx.geom;
        let (x0, y0) = geom.cell_center(from.0, from.1);
        let (x1, y1) = geom.cell_center(to.0, to.1);
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let len = (x1 - x0).hypot(y1 - y0);
        if len < 1.0 {
            return None;
        }
        // §6.2's `cells = L / cw`, resolved to the one integer the audio
        // receives (`SoundKind::Meteor { cells: u16 }`), so the flight clock
        // and the bell's delay are `flight_ms` of the same number (§8.1).
        let cells_n = (len / cw.max(1.0)).round().clamp(1.0, f32::from(u16::MAX)) as u16;
        let cells = f32::from(cells_n);
        let grade = (cells / GRADE_CELLS).clamp(0.0, 1.0);
        let mom = clamp01(ctx.birth_disp);
        let w = ch * (W_BASE_CH + W_MOM_CH * mom) * (1.0 + W_GRADE_GAIN * grade);
        self.ord = self.ord.wrapping_add(1);
        let seed = seed_of(to.0, to.1, self.ord);

        // D8 / §6.11: an Enter lands small (no burst, no hero, no rain), and
        // so does a PTY line feed (program output, whose sound carries no
        // rain either). A nav-licensed row change (history recall) is a full
        // meteor: D8 reserves the burst for same-row nav and history recall,
        // and D6 pairs recall's five rain glints with the fan's gold m1 + 4 m2.
        // The licence decides the landing; the axis decides the rasterizer
        // (D15).
        let small = matches!(licence, Licence::Return | Licence::Pty);

        let m = Meteor {
            x0,
            y0,
            x1,
            y1,
            t0: at,
            t_flight: flight(cells),
            w,
            grade,
            retired_at: None,
            chained: false,
            // D4: the caret's field `t` is the classic walk, unbounded on a
            // long line, and is latched raw. [`Meteor::arc`] folds it through `tri`
            // once at every read, so the station under the caret is the number
            // the ribbon draws for that cell.
            t_land: if ctx.caret_t.is_finite() {
                ctx.caret_t
            } else {
                0.0
            },
            // The band's walk at the landing, latched with `t_land`, so the
            // landing's marks continue the band at its own pace. `None` off
            // any band.
            band: ctx.caret_walk,
            cells,
            dir,
            seed,
            shed: 0,
            landed: false,
            ring: !small,
        };

        // §6.9: a same-direction continuation from within a cell of a live
        // head chains, and the old flight hands its head over. Its train is
        // cut at the head's position and finishes on the post-arrival law
        // (§6.8) with no nucleus, shedding or landing of its own, while the
        // new flight carries the head on from its own launch. The new flight's
        // clock, width, shed and fan are its own segment's.
        //
        // Only a head in the air can be handed over. A train whose head is at
        // rest (landed, or arrival passed unminted) is left to the corridor
        // rule below, which retires it with its afterglow decaying
        // (`Meteor::handed_over`). The arrival frame itself still chains:
        // `on_event` runs before `emit` could mint it.
        let mut chained: Option<usize> = None;
        for (i, old) in self.live.iter_mut().enumerate() {
            if old.retired_at.is_some()
                || old.chained
                || old.landed
                || old.dir != dir
                || at > old.arrival()
            {
                continue;
            }
            let (hx, hy) = head_at(old, at);
            if (x0 - hx).hypot(y0 - hy) <= CHAIN_CELLS * cw {
                if at < old.arrival() {
                    // Mid-flight: the train ends where the head is now, and
                    // its arrival edge is this edge. The arc is fixed to the
                    // row and measured from the landing (`Meteor::arc`), so
                    // the lock moves by the unflown path and every station
                    // keeps its colour: no hue pop on the chain frame.
                    let t_ms = (old.t_flight.as_secs_f32() * 1000.0).max(1.0);
                    let flown = enter_at_speed(ms_since(old.t0, at) / t_ms);
                    old.t_land -=
                        (1.0 - flown) * old.length() / cw * WALK_LAY_RATE * arc_gain(old.cells);
                    old.x1 = hx;
                    old.y1 = hy;
                    old.t_flight = at.saturating_duration_since(old.t0);
                }
                old.chained = true;
                chained = Some(i);
                break;
            }
        }

        // A new meteor whose corridor comes within 1 `ch` of a live one
        // retires it at this edge, so the older train decays from the alpha it
        // had. The train that just chained finishes on its own law.
        //
        // §6.9: only one head is ever in the air per row, so two same-row
        // flights that never come near each other (synthetic or PTY moves)
        // still retire the older head if it is flying; a train already on its
        // post-arrival fade is left alone.
        let corridor = CORRIDOR_CH * ch;
        let same_row =
            |old: &Meteor| m.y0 == m.y1 && old.y0 == old.y1 && m.y0 == old.y0 && at < old.arrival();
        for (i, old) in self.live.iter_mut().enumerate() {
            if Some(i) == chained || old.retired_at.is_some() {
                continue;
            }
            let near = seg_gap(
                (m.x0, m.y0),
                (m.x1, m.y1),
                (old.x0, old.y0),
                (old.x1, old.y1),
            ) <= corridor;
            if near || same_row(old) {
                old.retired_at = Some(at);
            }
        }

        // §18: the pool is fixed. A full pool drops the finishing train
        // nearest its own end before the push — at a human cadence that
        // train is already at the chroma cull (see `METEOR_POOL`).
        if self.live.len() >= METEOR_POOL {
            let victim = self
                .live
                .iter()
                .enumerate()
                .filter(|(_, x)| x.retired_at.is_some())
                .min_by_key(|(_, x)| x.end())
                .map_or(0, |(i, _)| i);
            self.live.remove(victim);
        }
        self.live.push(m);

        // Cap 2: a third evicts a train through the same 60 ms finish, so an
        // eviction looks like a crossing. Headless trains (chained stubs,
        // landed trains) go first; only when every train has a head does the
        // oldest head go. Counting every un-retired train keeps the pool's
        // arithmetic (`METEOR_POOL` = heads + trains on the 60 ms finish).
        while self.live.iter().filter(|x| x.retired_at.is_none()).count() > FLIGHT_MAX_LIVE {
            let victim = self
                .live
                .iter()
                .position(|x| x.retired_at.is_none() && !x.in_flight_at(at))
                .or_else(|| self.live.iter().position(|x| x.retired_at.is_none()));
            match victim {
                Some(i) => self.live[i].retired_at = Some(at),
                None => break,
            }
        }

        Some(Spawn {
            t0: at,
            t_flight: m.t_flight,
            dir,
            cells: cells_n,
            landing: to,
            ring: !small,
            // The same verdict as the ring (D6, D8): the rain pairs with the
            // hero and four m2 that only a full landing carries.
            rain: !small,
        })
    }
}

// ===========================================================================
// The per-frame flight solution (§6.2, §6.3, §6.8, §6.11)
// ===========================================================================

/// Everything about one meteor on one frame, solved once so no two layers can
/// read the flight at two different instants (the §17.1 rule that made the
/// spine one integrator applies just as hard here).
struct Flight {
    /// Path length `L`, px.
    l: f32,
    /// Unit vector along the path.
    unit: (f32, f32),
    /// The head's window-absolute position, px.
    head: (f32, f32),
    /// How far behind the head the train's root is, px — the whole visible
    /// span. During flight this is the head's own displacement; after arrival
    /// the root retracts toward the landing on `suck-in` (§6.8).
    span: f32,
    /// How far the head still has to fly to the landing, px: `(1 − p)·L`, zero
    /// from the arrival edge and frozen with a retired head. A station `s`
    /// behind the head is `(to_go + s)/cw` cells short of the landing
    /// ([`Meteor::arc`]).
    to_go: f32,
    /// `u = t/T`, clamped.
    u: f32,
    /// Body thickness now, px — `w` thinning to `0.4 w` across the fade (§6.8).
    w: f32,
    /// The white heat: `min(118·(1 + 0.5·grade)·intensity, 160)` (§6.3, L3)
    /// on dark; ungraded on light (§6.10).
    head_cov: f32,
    /// The colour train's head coverage, `118·intensity` — see
    /// [`COLOUR_TRAIN_COV`].
    colour_cov: f32,
    /// The white layer's alpha (§6.8's τ 90 × §6.9's retire).
    fade_w: f32,
    /// The colour layer's alpha (§6.8's τ 150 × §6.9's retire).
    fade_c: f32,
    /// The shoulder's design length behind the head, px (§6.5 layer 3) —
    /// `max(0.06·L, |Δ_frame| + 2 px)`. The light arm's rail chain holds
    /// full ink-bold alpha over it for the train's whole life.
    shoulder: f32,
    /// The white layer's shoulder now, px: [`Flight::shoulder`] during the
    /// flight, closing into the landing over [`WHITE_CLOSE_MS`] after it.
    white_shoulder: f32,
    /// The white layer's falloff length now, px: `0.12·L` during the flight,
    /// closing with the shoulder after it. Floored at 1 px so the exponential
    /// is always well-formed.
    white_fall: f32,
    /// True once the white layer has closed all the way into the landing
    /// (`t ≥ T + WHITE_CLOSE_MS`): nothing white is built on the train any
    /// more, whatever its alpha still says.
    white_closed: bool,
    /// The nucleus's velocity stretch, ×2.2 at launch → ×1.0 at rest.
    stretch: f32,
    /// The terminal flash's radius multiplier (§6.5 layer 8).
    flash: f32,
    /// Station stride, px, and the rasterizer's slab step (§18).
    stride: f32,
    /// Which rasterizer (D15).
    axis: Axis,
    /// True once the head was handed over in flight (§6.9,
    /// [`Meteor::handed_over`]): the nucleus and coma stop. The train layers
    /// finish on the retire decay (or, chained, on §6.8's taus from the chain
    /// edge) so nothing pops. A head retired at rest is not headless; its
    /// afterglow rides `fade_w` / `fade_c`, which carry the retire decay.
    headless: bool,
}

impl Flight {
    /// Solve one meteor on one frame, or `None` when there is nothing to draw.
    ///
    /// The `l < 1.0` guard is on the static path length, not on the animated
    /// one — see this module's header for why that distinction is the whole of
    /// T2.
    fn of(m: &Meteor, ctx: &Ctx<'_>) -> Option<Self> {
        let l = m.length();
        if !l.is_finite() || l < 1.0 {
            return None;
        }
        let (dx, dy) = m.delta();
        let unit = (dx / l, dy / l);
        let t_ms = (m.t_flight.as_secs_f32() * 1000.0).max(1.0);
        let age = ms_since(m.t0, ctx.now);
        let after = (age - t_ms).max(0.0);
        let reduced = ctx.cfg.reduced_motion;

        // §6.11: reduced motion is static — the head is at the landing from
        // the first frame, there is no flight, and the theme's one linear fade
        // (the single exception in §2.5's vocabulary) takes it off at the end
        // of the normal life.
        let (u, alpha_w, alpha_c) = if reduced {
            let life = t_ms + FLIGHT_OFF_GLASS_MS;
            let a = REDUCED_MOTION_ALPHA * clamp01((life - age) / REDUCED_MOTION_FADE_MS.max(1.0));
            (1.0, a, a)
        } else {
            (
                clamp01(age / t_ms),
                (-after / TRAIN_WHITE_TAU_MS).exp(),
                (-after / TRAIN_COLOUR_TAU_MS).exp(),
            )
        };
        let retire = m.retire_alpha(ctx.now);
        // Where the head and the root are, as shares of the path.
        //
        // Live: the head flies `enter-at-speed`; after arrival the root
        // retracts from the launch toward the landing (§6.8).
        //
        // Retired (§6.9): the head froze on the retire edge and the root
        // retracts from wherever it was toward it on `suck-in`, reaching it on
        // the same 60 ms the alpha reaches the chroma cull.
        let (u, p, root) = if reduced {
            (1.0, 1.0, 0.0)
        } else if let Some(at) = m.retired_at {
            let age_at = ms_since(m.t0, at);
            let u_at = clamp01(age_at / t_ms);
            let p_at = enter_at_speed(u_at);
            let after_at = (age_at - t_ms).max(0.0);
            let root_at = if after_at <= 0.0 {
                0.0
            } else {
                suck_in(after_at / ROOT_SUCK_MS)
            };
            let r = suck_in(ms_since(at, ctx.now) / FLIGHT_RETIRE_MS);
            (u_at, p_at, root_at + (p_at - root_at) * r)
        } else {
            let root = if after <= 0.0 {
                0.0
            } else {
                suck_in(after / ROOT_SUCK_MS)
            };
            (u, enter_at_speed(u), root)
        };
        let span = ((p - root) * l).max(0.0);
        if span < 1.0 {
            return None;
        }
        let to_go = ((1.0 - p) * l).max(0.0);

        let head = (m.x0 + dx * p, m.y0 + dy * p);
        let thin = 1.0 - (1.0 - THICKNESS_END_SHARE) * clamp01(after / ROOT_SUCK_MS);
        let cw = (ctx.geom.cw as f32).max(1.0);

        // §6.5 layer 3 / T7: the shoulder is the design length or the head's
        // travel across the slowest effect frame, whichever is longer; the
        // 1..4-cell clamp binds only that continuity floor.
        let travel = head_speed(l, t_ms, u) * SHOULDER_FRAME_MS + 2.0;
        let shoulder = (SHOULDER_SHARE * l)
            .max(travel.clamp(SHOULDER_MIN_CELLS * cw, SHOULDER_MAX_CELLS * cw));

        // §6.8: after the arrival edge the white heat closes into the caret on
        // the pin's clock (its shoulder and falloff length retract on
        // `suck-in`, `WHITE_CLOSE_MS`); its alpha still runs τ 90. Static
        // under reduced motion.
        let close = if reduced {
            0.0
        } else {
            suck_in(after / WHITE_CLOSE_MS)
        };
        let open = 1.0 - close;
        let white_shoulder = shoulder * open;
        let white_fall = (WHITE_FALLOFF_SHARE * l * open).max(1.0);

        // §6.3's brightness grade, on the dark arm only: §6.10 spends the
        // whole distance budget on length and speed where the ink is
        // legibility-bounded.
        let bright = if ctx.cfg.dark_theme {
            1.0 + BRIGHT_GRADE_GAIN * m.grade
        } else {
            1.0
        };
        let intensity = clamp01(ctx.cfg.intensity);
        let head_cov = (HEAD_COV_BASE * bright * intensity).min(JUMP_COV_CEIL);
        let colour_cov = COLOUR_TRAIN_COV * intensity;

        Some(Self {
            l,
            unit,
            head,
            span,
            to_go,
            u,
            w: m.w * thin,
            head_cov,
            colour_cov,
            fade_w: alpha_w * retire,
            fade_c: alpha_c * retire,
            shoulder,
            white_shoulder,
            white_fall,
            white_closed: close >= 1.0,
            stretch: 1.0 + NUCLEUS_STRETCH_GAIN * (1.0 - u).powf(FLIGHT_ENTER_EXP - 1.0),
            flash: if reduced { 1.0 } else { flash_scale(after) },
            stride: (l / STATIONS_MAX as f32).max(STATION_STRIDE_MIN_PX),
            axis: m.axis(),
            headless: m.handed_over(),
        })
    }

    /// The station stride as the rasterizer's own slab step (§18).
    fn step(&self) -> usize {
        (self.stride.round().max(1.0) as usize).max(1)
    }

    /// How many stations this span earns, `≤ STATIONS_MAX` (§18).
    fn stations(&self, span: f32) -> usize {
        (((span / self.stride).ceil() as usize) + 1).clamp(2, STATIONS_MAX)
    }

    /// How far along the path the head has travelled, as a share of `L` — the
    /// quantity a shed station is compared against (§6.5 layer 5).
    fn head_travel_share(&self) -> f32 {
        clamp01(enter_at_speed(self.u))
    }

    /// §6.5 layer 1's `w(s) = w·(0.30 + 0.70·exp(−s/(0.5·L)))`.
    fn width_at(&self, s: f32) -> f32 {
        self.w
            * (WIDTH_FLOOR_SHARE + WIDTH_SPAN_SHARE * (-s / (WIDTH_FALLOFF_SHARE * self.l)).exp())
    }
}

// ===========================================================================
// §6.5 layers 1-3 — the two train layers and the shoulder
// ===========================================================================

/// Which of the two train layers is being built (§6.5 layers 1-3). They share
/// one station walk and differ in exactly three things: the stream they land
/// in, the falloff length, and whether the colour comes from the arc or is
/// simply white.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// Layer 1 — `spectrum(t_m(s))` into `under`, below ink, never blooms.
    /// "Where I came from" reads beneath the letters without touching them.
    Colour,
    /// Layers 2 and 3 — `#FFFFFF` into `out`, above ink, blooms on GPU. This
    /// is the ion emission, and it dies fast behind the shoulder.
    White,
}

/// Emit §6.5's layers 1, 2 and 3.
fn draw_train(
    verts: &mut Vec<RibbonVertex>,
    m: &Meteor,
    f: &Flight,
    ctx: &Ctx<'_>,
    frame: &mut Frame<'_>,
) {
    let clip = ctx.geom.beam_clip();
    let step = f.step();
    let light = !ctx.cfg.dark_theme;

    build_stations(verts, m, f, ctx, Layer::Colour);
    let cap = frame.under.len() + UNDER_QUAD_CAP;
    let blend = if light {
        GlowBlend::Over
    } else {
        GlowBlend::Add
    };
    rasterize(frame.under, clip, verts, f.axis, step, cap, blend);

    // §6.10: the light arm drops the white layer and the shoulder entirely —
    // there is no such thing as "whiter than the page". Their job (the head
    // reading hotter than the trail) is done there by the rail chain's own
    // ink-bold alpha over the shoulder span, inside `build_stations`.
    if light {
        return;
    }
    build_stations(verts, m, f, ctx, Layer::White);
    let cap = frame.out.len() + WHITE_QUAD_CAP;
    rasterize(frame.out, clip, verts, f.axis, step, cap, GlowBlend::Add);
}

/// D15's axis rule, spent. Neither rasterizer is a fallback for the other:
/// each is degenerate on the axis its twin owns, and a producer that guesses
/// wrong draws nothing rather than drawing badly.
fn rasterize(
    out: &mut Vec<GlowQuad>,
    clip: BeamClip,
    verts: &[RibbonVertex],
    axis: Axis,
    step: usize,
    cap: usize,
    blend: GlowBlend,
) {
    match axis {
        Axis::Horizontal => ribbon_beam(out, clip, verts, 1.0, step, cap, blend),
        Axis::Vertical => ribbon_beam_v(out, clip, verts, 1.0, step, cap, blend),
    };
}

/// Walk the stations head first, so a saturated budget sheds the tail
/// (`ribbon_beam` returns early) rather than punching a hole in the middle,
/// and [`UNDER_QUAD_CAP`] sheds stations rather than starving the ribbon
/// (§6.5, §18).
fn build_stations(
    verts: &mut Vec<RibbonVertex>,
    m: &Meteor,
    f: &Flight,
    ctx: &Ctx<'_>,
    layer: Layer,
) {
    verts.clear();
    let light = !ctx.cfg.dark_theme;
    let cw = (ctx.geom.cw as f32).max(1.0);
    let ch = ctx.geom.ch as f32;

    // The white layer stops where its own exponential has nothing left to say
    // (§18: no dead work) — and is not built at all once it has closed into
    // the caret (`WHITE_CLOSE_MS`). The colour layer runs the whole visible
    // span.
    let span = match layer {
        Layer::Colour => f.span,
        Layer::White => {
            if f.white_closed {
                return;
            }
            f.span
                .min(f.white_shoulder + f.white_fall * WHITE_TAIL_EFOLDS)
        }
    };
    let n = f.stations(span);
    let alpha = match layer {
        Layer::Colour => f.fade_c,
        Layer::White => f.fade_w,
    };
    // C3 / §6.2: fade moves alpha, never chroma: a coloured pixel below the
    // chroma cull is dropped rather than drifting toward grey. This puts the
    // white layer off glass at `T + 191` and the colour layer at `T + 466`
    // from the two taus alone.
    if alpha < CHROMA_CULL_ALPHA {
        return;
    }
    // §6.10's rail: the light train leaves the body line and lies in the
    // inter-line leading, where nothing it darkens is ever a glyph. A vertical
    // flight crosses rows, so it has no one leading to lie in and stays on its
    // own path — the rail is a same-row idea, and offsetting a vertical train
    // by 0.62 `ch` would only move it onto the next row's glyphs.
    let rail = if light && f.axis == Axis::Horizontal {
        LIGHT_RAIL_CH * ch
    } else {
        0.0
    };

    for k in 0..n {
        let s = span * (k as f32) / ((n - 1) as f32);
        let px = f.head.0 - f.unit.0 * s;
        let py = f.head.1 - f.unit.1 * s + rail;

        let (color, cov, half) = match layer {
            Layer::Colour => {
                // The station's colour is its place on the row, not its
                // distance from the head (`Meteor::arc`): `to_go + s` cells
                // short of the landing.
                let stop = spectrum(walk_pace(m.arc((f.to_go + s) / cw)));
                let mut half = f.width_at(s) * 0.5;
                if light {
                    // §6.10 / L6: the light twin buys area, never brightness.
                    // The rail is squashed and coverage becomes ink opacity
                    // `min(head_cov·2.1, cap)`: 236 on the rail (leading, no
                    // glyph), 190 over text for a vertical train. The shoulder
                    // at full ink-bold alpha makes the head read hotter, joined
                    // by the exponential re-based at its end (no step).
                    half *= LIGHT_RAIL_SQUASH;
                    let cap = if rail > 0.0 {
                        LIGHT_RAIL_ALPHA_CAP
                    } else {
                        LIGHT_ALPHA_CAP
                    };
                    let peak = (f.colour_cov * LIGHT_INK_GAIN).min(cap);
                    let ink = if s <= f.shoulder {
                        peak
                    } else {
                        peak * (-(s - f.shoulder) / (COLOUR_FALLOFF_SHARE * f.l)).exp()
                    };
                    // Under the glyphs either way: on the rail, where no letter
                    // is, the caret's own vivid ink; across rows, the body's
                    // tint, which carries the legibility bar.
                    let rgb = if rail > 0.0 {
                        paper_caret_ink(stop, ctx.cfg.theme_bg)
                    } else {
                        paper_ink(stop, ctx.cfg.theme_fg, ctx.cfg.theme_bg, InkRole::OverText)
                    };
                    (rgb, ink * alpha, half)
                } else {
                    // §6.5 layer 1 / §3.4 / §6.6: `118·exp(−s/(0.35·L))` —
                    // the transient ceiling at the head, so the white heat
                    // wins over it wherever the two share a cell, and the
                    // e⁻¹ law between the shoulder and 0.61·L holds at every
                    // grade (see `COLOUR_TRAIN_COV`).
                    let c = f.colour_cov * (-s / (COLOUR_FALLOFF_SHARE * f.l)).exp();
                    (stop, c * alpha, half)
                }
            }
            Layer::White => {
                // The shoulder is flat at `head_cov` and the exponential is
                // re-based at its end, so they join with no step (§6.5 layer
                // 3). Both lengths are the white layer's own, closing into the
                // landing after arrival (`WHITE_CLOSE_MS`).
                let tail = (s - f.white_shoulder).max(0.0);
                let c = f.head_cov * (-tail / f.white_fall).exp();
                let blend = smoothstep01((s - f.white_shoulder) / f.white_shoulder.max(1.0));
                let share =
                    SHOULDER_WIDTH_SHARE + (WHITE_WIDTH_SHARE - SHOULDER_WIDTH_SHARE) * blend;
                (0x00FF_FFFF, c * alpha, f.width_at(s) * 0.5 * share)
            }
        };
        if cov < 1.0 {
            continue;
        }
        let half = half.max(0.5);
        // `ribbon_beam`'s vertex fields are major/minor, not x/y: on the
        // vertical twin `x` is the sample's window Y and `spine` its window X
        // (D15).
        let (major, spine) = match f.axis {
            Axis::Horizontal => (px, py),
            Axis::Vertical => (py, px),
        };
        verts.push(RibbonVertex {
            x: major,
            spine,
            up: half,
            dn: half,
            core_up: half * TRAIN_CORE_SHARE,
            core_dn: half * TRAIN_CORE_SHARE,
            color,
            cov,
            lift: 0.0,
            lift_span: 0.0,
        });
    }
}

// ===========================================================================
// §6.5 layers 6-8 — the coma, the nucleus and the terminal flash
// ===========================================================================

/// Emit the coma, the nucleus and the terminal flash's radius pop.
///
/// The nucleus is the top of §3.2's luminance hierarchy: the brightest pixel
/// is the smallest thing, leading the train by [`NUCLEUS_LEAD_PX`]. On a
/// diagonal only the dominant axis is stretched, because `RainHalo` is an
/// axis-aligned ellipse and a stretch on the wrong axis reads as a smear.
fn draw_head(m: &Meteor, f: &Flight, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    if f.headless {
        return;
    }
    let geom = ctx.geom;
    let light = !ctx.cfg.dark_theme;
    let r_c = (NUCLEUS_MIN_PX).max(NUCLEUS_W_SHARE * f.w);
    let squash = if light { LIGHT_SQUASH } else { 1.0 };
    // The stretch is along the path; the flash (§6.5 layer 8: "nucleus radius
    // ×(1.0 → 1.6)") pops the nucleus and only the nucleus — the coma is the
    // steady blended glow the flash happens inside of.
    let (rx, ry) = match f.axis {
        Axis::Horizontal => (r_c * f.stretch, r_c * squash),
        Axis::Vertical => (r_c * squash, r_c * f.stretch),
    };
    let (nx, ny) = (rx * f.flash, ry * f.flash);
    let cx = f.head.0 + f.unit.0 * NUCLEUS_LEAD_PX;
    let cy = f.head.1 + f.unit.1 * NUCLEUS_LEAD_PX;

    // §3.2, D3: "nothing coloured is drawn below α 0.12; a white core below
    // α 0.20 is culled, never left as a grey speck". The two halos are the
    // two cases, and each is culled on its own floor rather than allowed to
    // linger as a 3/255 dot until the pool's end.
    let coma_on = f.fade_c >= CHROMA_CULL_ALPHA;
    let core_on = f.fade_w >= STAR_CULL_ALPHA;

    // Layer 6 — the corona: seven petals of the seven named stops orbiting
    // the nucleus (`COMA_PETALS`), turning once per `COMA_SPIN_MS` — static
    // under reduced motion, like every other motion of the theme. Halos,
    // not marks (§3.1), and the flash pops the nucleus only: the petals ride
    // the head's stretch and nothing else.
    let coma_cov = (COMA_COV_SHARE * f.head_cov * f.fade_c).clamp(0.0, 255.0);
    let core_cov = (f.head_cov * f.fade_w).clamp(0.0, 255.0);
    let spin = if ctx.cfg.reduced_motion {
        0.0
    } else {
        std::f32::consts::TAU * ms_since(m.t0, ctx.now) / COMA_SPIN_MS
    };
    let (orbit_x, orbit_y) = (
        COMA_R_SCALE * COMA_PETAL_ORBIT * rx,
        COMA_R_SCALE * COMA_PETAL_ORBIT * ry,
    );
    let (petal_x, petal_y) = (
        COMA_R_SCALE * COMA_PETAL_R * rx,
        COMA_R_SCALE * COMA_PETAL_R * ry,
    );
    let petal_at = |i: usize| {
        let a = spin + std::f32::consts::TAU * (i as f32) / (COMA_PETALS as f32);
        (cx + orbit_x * a.cos(), cy + orbit_y * a.sin())
    };

    if light {
        if coma_on {
            let ink = (coma_cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
            for i in 0..COMA_PETALS {
                push_halo_veil(
                    frame.halos,
                    geom,
                    petal_at(i),
                    (petal_x, petal_y),
                    paper_mark_ink(spectrum_stop(i), ctx.cfg.theme_bg),
                    ink,
                );
            }
        }
        if core_on {
            push_halo_veil(
                frame.halos,
                geom,
                (cx, cy),
                (nx, ny),
                ctx.cfg.theme_fg & 0x00FF_FFFF,
                (core_cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8,
            );
        }
        return;
    }
    if coma_on {
        for i in 0..COMA_PETALS {
            let (px, py) = petal_at(i);
            push_halo_add(
                frame.halos,
                geom,
                px,
                py,
                petal_x,
                petal_y,
                premul_rgb(spectrum_stop(i), coma_cov as u8),
            );
        }
    }
    if core_on {
        push_halo_add(
            frame.halos,
            geom,
            cx,
            cy,
            nx,
            ny,
            premul_rgb(0x00FF_FFFF, core_cov as u8),
        );
    }
}

/// §6.5 layer 8: the terminal flash, as area and never as coverage.
///
/// The nucleus radius eases out to [`FLASH_OVERSHOOT`] over [`FLASH_MS`],
/// passing its nominal [`FLASH_PEAK`], then relaxes to ×1.0 by
/// [`FLASH_RELAX_MS`]. The rise uses the theme's own deceleration exponent
/// ([`FLIGHT_ENTER_EXP`]); §2.5's easing vocabulary is closed.
#[must_use]
fn flash_scale(after_ms: f32) -> f32 {
    if after_ms <= 0.0 {
        return 1.0;
    }
    let over = FLASH_OVERSHOOT - 1.0;
    if after_ms <= FLASH_MS {
        let u = clamp01(after_ms / FLASH_MS);
        return 1.0 + over * (1.0 - (1.0 - u).powf(FLIGHT_ENTER_EXP));
    }
    let v = clamp01((after_ms - FLASH_MS) / (FLASH_RELAX_MS - FLASH_MS));
    1.0 + over * (1.0 - smoothstep01(v))
}

// ===========================================================================
// §6.5 layers 9 and 10 — the pin and the ring
// ===========================================================================

/// §6.5 layer 9: the landing pin. Explicit arms plus a 2×2 nucleus, not
/// `push_twinkle_star`, whose waist at this arm length would be 4 px rather
/// than [`PIN_WAIST_PX`]. The arms whip inward; the last pixel on glass is the
/// nucleus, which is the caret.
fn draw_pin(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    // §6.11: the pin exists only as motion (its arms whip into the caret),
    // and under reduced motion nothing moves.
    if ctx.cfg.reduced_motion {
        return;
    }
    let u = clamp01(ms_since(l.pin.at, ctx.now) / PIN_MS);
    let ch = ctx.geom.ch as f32;
    let close = smoothstep01((u - PIN_ARM_HOLD_U) / (1.0 - PIN_ARM_HOLD_U));
    let arm = PIN_ARM_CH * ch * (1.0 - close).powf(PIN_ARM_EXP);
    let alpha = if u <= PIN_ALPHA_HOLD_U {
        1.0
    } else {
        1.0 - smoothstep01((u - PIN_ALPHA_HOLD_U) / (1.0 - PIN_ALPHA_HOLD_U))
    };
    let cov = TRANSIENT_STAR_COV_CEIL * alpha * clamp01(ctx.cfg.intensity);
    if cov < 1.0 {
        return;
    }
    let light = !ctx.cfg.dark_theme;
    let arms_rgb = spectrum_snap(l.pin.t);
    let (cx, cy) = (l.x.round() as i32, l.y.round() as i32);
    let a = arm.round() as i32;

    // The 2×2 nucleus sits on `(cx − 1 ..= cx, cy − 1 ..= cy)`; the four 1-px
    // arms run outward from its edges, never through it. D1: the caret cell
    // is ledger-exempt, so arms crossing under the nucleus would stack 3 × 118
    // on one pixel and read as a blob. Each arm is `a − 1` long past the
    // nucleus, so the full extent is `2·a`, centred half a pixel off
    // `(cx, cy)` as any 2×2 is.
    let reach = a - 1;
    let arms = [
        (cx - a, cy, reach, PIN_WAIST_PX),
        (cx + 1, cy, reach, PIN_WAIST_PX),
        (cx, cy - a, PIN_WAIST_PX, reach),
        (cx, cy + 1, PIN_WAIST_PX, reach),
    ];
    let nuc = (
        cx - PIN_NUCLEUS_PX / 2,
        cy - PIN_NUCLEUS_PX / 2,
        PIN_NUCLEUS_PX,
        PIN_NUCLEUS_PX,
    );
    if light {
        let ink = (cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
        if reach > 0 {
            let arms_ink = paper_mark_ink(arms_rgb, ctx.cfg.theme_bg);
            for b in arms {
                push_ink_rect(frame.out, ctx.geom, b, arms_ink, ink);
            }
        }
        push_ink_rect(
            frame.out,
            ctx.geom,
            nuc,
            ctx.cfg.theme_fg & 0x00FF_FFFF,
            ink,
        );
        return;
    }
    let arm_premul = premul_rgb(arms_rgb, cov as u8);
    // The arms whip all the way in before the nucleus goes: past `u = 1` the
    // pin IS the caret, and a zero-length arm is not drawn at all.
    if reach > 0 {
        for (x, y, w, h) in arms {
            push_fx_rect(frame.out, ctx.geom, x, y, w, h, arm_premul);
        }
    }
    push_fx_rect(
        frame.out,
        ctx.geom,
        nuc.0,
        nuc.1,
        nuc.2,
        nuc.3,
        premul_rgb(0x00FF_FFFF, cov as u8),
    );
}

/// The white-hot flash. On the arrival edge the caret cell and its neighbours
/// ([`FLASH_CELLS`]) burst white, and the white dies into the rainbow: over
/// each cell a `#FFFFFF` layer leaves on `1 − smoothstep(t/80)` while a
/// coloured layer arrives on the same curve, peaks where the white is gone and
/// spends over the next 80 ms ([`FLASH_BURST_MS`], [`FLASH_COLOUR_MS`]). Each
/// cell wears the band's snapped stop at its column, so the flash hands the
/// eye straight to the pin, fan and burst in one spectrum.
///
/// Over a cell the sky's probe proved blank ([`SkyMask::flash_blank`]) the
/// burst asks [`FLASH_FULL_COV`]; over a glyph cell, or before the probe has
/// been asked, the transient cap (L3). The caret cell is ledger-exempt and
/// white on the flare frame anyway.
///
/// Light: there is no whiter than the page, so the white layer is dropped and
/// the colour layer is source-over ink at the over-text cap (§6.10). Reduced
/// motion: white and colour at half each, held, then the theme's linear fade
/// (§6.11).
fn draw_flash(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    if !l.flash {
        return;
    }
    let age = ms_since(l.pin.at, ctx.now);
    let reduced = ctx.cfg.reduced_motion;
    let (white_share, colour_share) = if reduced {
        let a = l.static_alpha(ctx.now);
        (
            FLASH_STATIC_WHITE_SHARE * a,
            (1.0 - FLASH_STATIC_WHITE_SHARE) * a,
        )
    } else {
        if age >= FLASH_COLOUR_MS {
            return;
        }
        let turn = smoothstep01(age / FLASH_BURST_MS);
        let spent = spend(clamp01(
            (age - FLASH_BURST_MS) / (FLASH_COLOUR_MS - FLASH_BURST_MS),
        ));
        (1.0 - turn, turn * spent)
    };
    let intensity = clamp01(ctx.cfg.intensity);
    let light = !ctx.cfg.dark_theme;
    let geom = ctx.geom;
    let (cw, ch) = (geom.cw as i32, geom.ch as i32);
    // The landing cell's own box, from the caret's centre.
    let x0 = (l.x - geom.cw as f32 * 0.5).round() as i32;
    let y0 = (l.y - geom.ch as f32 * 0.5).round() as i32;
    for k in -FLASH_CELLS..=FLASH_CELLS {
        let blank = l.sky.is_some_and(|s| s.flash_blank(k));
        let cap = if blank {
            FLASH_FULL_COV
        } else {
            TRANSIENT_STAR_COV_CEIL
        };
        let x = x0 + k * cw;
        // A point mark snaps (C1) to the band's own stop at its column
        // ([`Landing::field`]).
        let stop = spectrum_snap(l.field(k as f32));
        let colour_cov = cap * colour_share * intensity;
        if light {
            let ink = (colour_cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
            push_ink_rect(
                frame.out,
                geom,
                (x, y0, cw, ch),
                paper_mark_ink(stop, ctx.cfg.theme_bg),
                ink,
            );
            continue;
        }
        let white_cov = cap * white_share * intensity;
        if white_cov >= 1.0 {
            push_fx_rect(
                frame.out,
                geom,
                x,
                y0,
                cw,
                ch,
                premul_rgb(0x00FF_FFFF, white_cov as u8),
            );
        }
        if colour_cov >= 1.0 {
            push_fx_rect(
                frame.out,
                geom,
                x,
                y0,
                cw,
                ch,
                premul_rgb(stop, colour_cov as u8),
            );
        }
    }
}

/// The largest radius a mark may reach along a unit direction with vertical
/// component `dy` before its painted edge (centreline plus half the stroke)
/// crosses `rise_px`. The const assert on [`BURST_CORE_CH`] holds the law in
/// `ch`; this holds it in device px, including the AA fringe at every font
/// size. A no-op at the tuned cells; at a tiny cell it shortens only the
/// near-vertical spikes.
#[inline]
#[must_use]
fn burst_rise_limit(dy: f32, rise_px: f32, half_thick_px: f32) -> f32 {
    let room = rise_px - half_thick_px;
    if room <= 0.0 {
        return 0.0;
    }
    let ady = dy.abs();
    if ady < 1e-3 {
        f32::INFINITY
    } else {
        room / ady
    }
}

/// The landing's ink at window `x`: [`landing_ink`] (or its light-theme ink)
/// at the band's walk through this landing ([`Landing::field`]). Every burst
/// and jet station and the flash's snapped cells read colour here, so a point
/// of the mark wears the colour of its column, independent of spike and age.
#[inline]
#[must_use]
fn landing_ink_at(l: &Landing, x: f32, cw: f32, page: Option<u32>) -> u32 {
    let t = l.field((x - l.x) / cw);
    match page {
        Some(bg) => landing_ink_light(t, bg),
        None => landing_ink(t),
    }
}

/// One tapered lance: three collinear [`comet_beam`] sections of decreasing
/// thickness from `root` to `tip` along `dir`, coloured by the band at each
/// station's column ([`landing_ink_at`]), coverage falling to
/// `1 − `[`BURST_TIP_FADE`] at the tip.
///
/// Light (§6.10): the same silhouette in source-over ink (overlapping ink
/// squares of each section's thickness), since additive light is invisible on
/// a page; it buys area, never brightness (L6).
#[allow(clippy::too_many_arguments)]
fn burst_lance(
    frame: &mut Frame<'_>,
    ctx: &Ctx<'_>,
    l: &Landing,
    dir: (f32, f32),
    root: f32,
    tip: f32,
    sec_thick_px: [f32; 3],
    cov: f32,
    step: usize,
    light: bool,
) {
    let (ox, oy) = (l.x, l.y);
    let span = tip - root;
    if span < 1.0 {
        return;
    }
    let clip = ctx.geom.beam_clip();
    let cw = (ctx.geom.cw as f32).max(1.0);
    // The colour `s` px along the lance: the band's walk at that station's
    // column, through the band's own table — never a chord between two
    // distant stops (a chord cuts the corner off the arc and skips names),
    // and never the mark's own walk. A near-vertical spike is one colour,
    // the landing's; a near-horizontal one is the band's own gradient.
    let page = light.then_some(ctx.cfg.theme_bg);
    let hue = |s: f32| landing_ink_at(l, ox + dir.0 * s, cw, page);
    let mut s0 = root;
    let mut done = 0.0_f32;
    for (k, &thick_ch) in sec_thick_px.iter().enumerate() {
        done += BURST_SEC_SHARE[k];
        let s1 = if k + 1 == sec_thick_px.len() {
            tip
        } else {
            root + span * done
        };
        if s1 - s0 < 0.5 {
            s0 = s1;
            continue;
        }
        let thick = thick_ch.max(1.0);
        if light {
            let ink = (cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP);
            let side = (thick.round() as i32).max(1);
            let stride = (thick * BURST_LIGHT_STEP_SHARE).max(1.0);
            let mut s = s0;
            while s <= s1 {
                let t = (s - root) / span;
                let a = (ink * (1.0 - BURST_TIP_FADE * t)) as u8;
                let rect = (
                    (ox + dir.0 * s).round() as i32 - side / 2,
                    (oy + dir.1 * s).round() as i32 - side / 2,
                    side,
                    side,
                );
                push_ink_rect(frame.out, ctx.geom, rect, hue(s), a);
                s += stride;
            }
        } else {
            frame.beams.clear();
            for j in 0..BURST_SEC_STATIONS {
                let f = j as f32 / (BURST_SEC_STATIONS - 1) as f32;
                let s = s0 + (s1 - s0) * f;
                let t = (s - root) / span;
                frame.beams.push(BeamVertex {
                    x: ox + dir.0 * s,
                    y: oy + dir.1 * s,
                    color: hue(s),
                    cov: (cov * (1.0 - BURST_TIP_FADE * t)).clamp(0.0, 255.0) as u8,
                });
            }
            comet_beam(frame.out, clip, frame.beams, thick, step, 0.0);
        }
        s0 = s1;
    }
}

/// One jet: the distance grade, along the flight line, at no rise cost.
///
/// A jet roots where the core's envelope ends and runs to [`JET_LEN_SHARE`]
/// of [`ring_full_radius`] on the same quartic, sharing the star's origin,
/// onset, clock and colour law ([`landing_ink_at`]): each station wears the
/// band's walk at its column for the whole life.
///
/// A jet crosses the landing row's text, so every station asks the sky's
/// probe about its cell ([`SkyMask::row_blank`]) and a station not proved
/// blank breaks the polyline. Over dense text the jets vanish and the mark
/// degrades to the core star; the graded fan still carries the distance cue.
#[allow(clippy::too_many_arguments)]
fn burst_jet(
    frame: &mut Frame<'_>,
    ctx: &Ctx<'_>,
    l: &Landing,
    dir: (f32, f32),
    root: f32,
    tip: f32,
    cov: f32,
    light: bool,
) {
    let Some(sky) = l.sky else {
        // Before the probe has been asked there is no licence to cross the
        // row, so the mark is the bare core star on that one frame.
        return;
    };
    let span = tip - root;
    let cw = (ctx.geom.cw as f32).max(1.0);
    let ch = ctx.geom.ch as f32;
    if span < 1.0 {
        return;
    }
    let clip = ctx.geom.beam_clip();
    let ink_gain = if light { LIGHT_INK_GAIN } else { 1.0 };
    let sec_end = [root + span * JET_SEC_SHARE[0], tip];
    let mut s0 = root;
    for k in 0..2 {
        let s1 = sec_end[k];
        let seg = s1 - s0;
        if seg < 1.0 {
            s0 = s1;
            continue;
        }
        let thick = (JET_SEC_THICK_CH[k] * ch).max(1.0);
        let step = burst_step(ch, JET_STEP_CH_SHARE, JET_STEP_PX);
        let stations = ((seg / cw) * JET_STATIONS_PER_CELL as f32).ceil().max(1.0) as usize + 1;
        frame.beams.clear();
        for j in 0..stations {
            let share = j as f32 / (stations - 1) as f32;
            let d = s0 + seg * share;
            let cell = ((dir.0 * d) / cw).round() as i32;
            // The feather is measured over the whole jet, not the section, so
            // the sections meet at one coverage.
            let t_all = ((d - root) / span).clamp(0.0, 1.0);
            let fade = 1.0 - smoothstep01((t_all - JET_FEATHER_SHARE) / (1.0 - JET_FEATHER_SHARE));
            let c = (cov * fade * ink_gain).min(if light { LIGHT_ALPHA_CAP } else { 255.0 });
            if !sky.row_blank(cell) || c < 1.0 {
                if !light {
                    comet_beam(frame.out, clip, frame.beams, thick, step, 0.0);
                    frame.beams.clear();
                }
                continue;
            }
            // The band's walk at this station's column (signed: a jet to the
            // left of the landing reads the band to the left of it).
            let stop = landing_ink_at(l, l.x + dir.0 * d, cw, light.then_some(ctx.cfg.theme_bg));
            if light {
                let w = ((seg / (stations - 1) as f32).round() as i32).max(1) + 1;
                let h = (thick.round() as i32).max(1);
                let rect = (
                    (l.x + dir.0 * d).round() as i32 - w / 2,
                    (l.y + dir.1 * d).round() as i32 - h / 2,
                    w,
                    h,
                );
                push_ink_rect(frame.out, ctx.geom, rect, stop, c as u8);
            } else {
                frame.beams.push(BeamVertex {
                    x: l.x + dir.0 * d,
                    y: l.y + dir.1 * d,
                    color: stop,
                    cov: c as u8,
                });
            }
        }
        if !light {
            comet_beam(frame.out, clip, frame.beams, thick, step, 0.0);
        }
        s0 = s1;
    }
}

/// The landing starburst (§6.5 layer 10): [`Burst::n`] tapered lances at true
/// screen angles out to [`BURST_CORE_CH`], rooted at [`BURST_ROOT_CH`] so the
/// caret keeps its own cell and the wedges stay dark all the way in, plus
/// [`JET_N_PER_SIDE`] jets per side carrying the distance grade along the
/// line.
///
/// The palette is the band's: every station reads [`landing_ink_at`], the
/// band's walk through the landing cell at the station's own column, through
/// the band's hot ink. The light over the landing cell is its stop, the star
/// is the band's gradient at the band's pace ([`LandingWalk`]), the crossing
/// is the band's teal, and a column wears one hue for the whole life. A linear
/// mark samples the arc continuously (C1); the flash's cells snap.
///
/// It runs on the quartic ([`RING_R_EXP`]), the graded life ([`ring_ms`]),
/// the graded reach ([`ring_full_radius`]), the hold-then-spend coverage
/// ([`RING_COV_HOLD_U`], [`RING_COV_SHARE`]) and the transient ceiling, so the
/// 600 ms budget is met by construction.
///
/// The frame path does no trigonometry or hashing (resolved at
/// [`mint_burst`]) and allocates nothing: `frame.beams` is the host's scratch,
/// cleared per section; colour is one table lerp per station.
///
/// Reduced motion (§6.11): the full star and jets drawn once at full reach,
/// held, then the theme's linear fade.
fn draw_burst(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    let Some(b) = l.burst else {
        return;
    };
    let ch = ctx.geom.ch as f32;
    let reduced = ctx.cfg.reduced_motion;
    let (grow, alpha) = if reduced {
        (1.0, l.static_alpha(ctx.now))
    } else {
        let u = clamp01(ms_since(b.at, ctx.now) / b.ms);
        // Hold-then-fall, the ring's own: full while the star is small, spent
        // over the rest of its life as it opens.
        let u_spend = clamp01((u - RING_COV_HOLD_U) / (1.0 - RING_COV_HOLD_U));
        (1.0 - (1.0 - u).powi(RING_R_EXP), spend(u_spend))
    };
    let cov = RING_COV_SHARE * TRANSIENT_STAR_COV_CEIL * alpha * clamp01(ctx.cfg.intensity);
    if cov < 1.0 || grow <= 0.0 {
        return;
    }
    let light = !ctx.cfg.dark_theme;
    // The cap is enforced, by shedding whole elements in the mint's own fixed
    // order. See [`BURST_QUAD_CAP`].
    let cap = frame.out.len() + BURST_QUAD_CAP;
    // The painted-extent rise law, in device px (see [`burst_rise_limit`]).
    let rise_px = (FAN_RISE_MAX_CH * ch - 1.0).max(1.0);
    let n = usize::from(b.n).min(BURST_N_MAX);
    let root = BURST_ROOT_CH * ch;
    let core_full = BURST_CORE_CH * ch;
    let sec_thick = [
        BURST_SEC_THICK_CH[0] * ch,
        BURST_SEC_THICK_CH[1] * ch,
        BURST_SEC_THICK_CH[2] * ch,
    ];
    let half_root_thick = sec_thick[0] * 0.5;
    for slot in 0..n {
        if frame.out.len() >= cap {
            break;
        }
        let i = usize::from(b.order[slot]);
        let dir = b.dir[i];
        let tip =
            (core_full * b.len[i] * grow).min(burst_rise_limit(dir.1, rise_px, half_root_thick));
        if tip <= root + 1.0 {
            continue;
        }
        burst_lance(
            frame,
            ctx,
            l,
            dir,
            root,
            tip,
            sec_thick,
            cov,
            burst_step(ch, BURST_STEP_CH_SHARE, BURST_STEP_PX),
            light,
        );
    }

    // The jets, longest first — the shed order's tail.
    let r_full = ring_full_radius(b.scale, ch);
    let jet_half_thick = JET_SEC_THICK_CH[0] * ch * 0.5;
    // The jets are the star's extensions: they root where its envelope is and
    // do not exist before it does, so the first frames are never a
    // horizontal nub without a star.
    let jet_root = core_full * grow;
    if jet_root <= root {
        return;
    }
    for (k, &len_share) in JET_LEN_SHARE.iter().enumerate() {
        for si in 0..2 {
            if frame.out.len() >= cap {
                return;
            }
            let dir = b.jet_dir[k * 2 + si];
            let tip =
                (len_share * r_full * grow).min(burst_rise_limit(dir.1, rise_px, jet_half_thick));
            burst_jet(
                frame,
                ctx,
                l,
                dir,
                jet_root,
                tip,
                cov * JET_COV_SHARE,
                light,
            );
        }
    }
}

/// One spark's throw, hashed from the landing's seed and the spark's index
/// at the mint ([`Spark`]): thrown upward inside the [`SPARK_CONE_SHARE`]
/// cone at [`SPARK_V_MIN_CH_PER_S`]..[`SPARK_V_MAX_CH_PER_S`], spread
/// [`SPARK_SPREAD`] wider along the line, for
/// [`SPARK_LIFE_MIN_MS`]..[`SPARK_LIFE_MAX_MS`].
#[must_use]
fn mint_spark(seed: u32, k: usize, ch: f32) -> Spark {
    let s = mix32(seed ^ (k as u32).wrapping_mul(0x27D4_EB2F) ^ 0x5AA5_5AA5);
    let life = SPARK_LIFE_MIN_MS + (SPARK_LIFE_MAX_MS - SPARK_LIFE_MIN_MS) * unit01(s, 1);
    let a =
        std::f32::consts::PI * (SPARK_CONE_SHARE + (1.0 - 2.0 * SPARK_CONE_SHARE) * unit01(s, 2));
    // ch per second → px per ms.
    let v = (SPARK_V_MIN_CH_PER_S + (SPARK_V_MAX_CH_PER_S - SPARK_V_MIN_CH_PER_S) * unit01(s, 3))
        * ch
        / 1000.0;
    let sizes = SPARK_ARM_MAX_PX - SPARK_ARM_MIN_PX + 1;
    let arm = SPARK_ARM_MIN_PX + ((sizes as f32 * unit01(s, 4)) as i32).min(sizes - 1);
    Spark {
        vx: v * a.cos() * SPARK_SPREAD,
        vy: -v * a.sin(),
        life,
        arm: arm as u8,
    }
}

/// One spark's displacement and alpha at `age_ms` — a pure function of its
/// throw and its age (§18: no per-frame integration, no drift, and no
/// trigonometry on the frame path). Falls under [`SPARK_G_CH_PER_S2`]; full
/// for [`SPARK_HOLD_U`] of its life, then spent. `None` once the spark is
/// spent or under the chroma cull.
#[must_use]
fn spark_at(spark: Spark, age_ms: f32, ch: f32) -> Option<((f32, f32), f32)> {
    let u = age_ms / spark.life.max(1.0);
    if !(0.0..1.0).contains(&u) {
        return None;
    }
    let alpha = if u <= SPARK_HOLD_U {
        1.0
    } else {
        spend((u - SPARK_HOLD_U) / (1.0 - SPARK_HOLD_U))
    };
    if alpha < CHROMA_CULL_ALPHA {
        return None;
    }
    let g = SPARK_G_CH_PER_S2 * ch / 1_000_000.0;
    let dx = spark.vx * age_ms;
    let dy = spark.vy * age_ms + 0.5 * g * age_ms * age_ms;
    Some(((dx, dy), alpha))
}

/// The most quads one spark's star may spend — the family star's two bars,
/// its core and its taper spans; a spark past it loses its faintest tips.
pub const SPARK_STAR_QUAD_CAP: usize = 24;

/// The sparks: a shower of small stars thrown from the landing, climbing,
/// hanging and falling, the impact's last light. Each is the family's
/// four-point star (`push_twinkle_star`, D2) at 5–9 px ([`Spark::arm`]), its
/// core the band's snapped stop (C1) tinted [`SPARK_CORE_TINT`] toward white,
/// inside a saturated halo at [`SPARK_HALO_SHARE`] of the core. A dying star
/// dims halo-first ([`SPARK_SHRINK_ALPHA`]), loses a pixel of arm, then is a
/// 3 px cross. At the transient cap (L3). Drawn from the frame the flash's
/// white has left ([`FLASH_BURST_MS`]). `halo_cap` is the frame's spark-halo
/// budget ([`SPARK_HALO_CAP`]); a spark past it keeps its star.
/// Light: ink crosses, no halos (§6.10's `peak < 96` cull). Reduced motion:
/// no sparks (§6.11).
fn draw_sparks(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>, halo_cap: usize) {
    if l.sparks == 0 || ctx.cfg.reduced_motion {
        return;
    }
    let age = ms_since(l.pin.at, ctx.now);
    // The sparks are what the flash throws: under the white burst they are
    // invisible on additive glass and only cost quads, so a spark is first
    // drawn on the frame the white has left ([`FLASH_BURST_MS`]), 0.4–0.7 `ch`
    // out and climbing.
    if age < FLASH_BURST_MS {
        return;
    }
    let geom = ctx.geom;
    let ch = geom.ch as f32;
    let cw = (geom.cw as f32).max(1.0);
    let intensity = clamp01(ctx.cfg.intensity);
    let light = !ctx.cfg.dark_theme;
    for spark in l.spark.iter().take(usize::from(l.sparks)) {
        let Some(((dx, dy), alpha)) = spark_at(*spark, age, ch) else {
            continue;
        };
        let cov = TRANSIENT_STAR_COV_CEIL * alpha * intensity;
        if cov < 1.0 {
            continue;
        }
        // A point mark snaps (C1) to the band's own stop at its column
        // ([`Landing::field`]), in the band's hot ink ([`landing_stop_ink`]),
        // so the shower wears the band's colours where it flies.
        let stop = spectrum_snap_index(walk_pace(l.field(dx / cw)));
        let rgb = landing_stop_ink(stop);
        let arm0 = i32::from(spark.arm).max(1);
        let arm = if alpha >= SPARK_SHRINK_ALPHA {
            arm0
        } else if alpha >= SPARK_SHRINK_ALPHA * 0.5 {
            (arm0 - 1).max(1)
        } else {
            1
        };
        let (x, y) = ((l.x + dx).round() as i32, (l.y + dy).round() as i32);
        if light {
            let ink = (cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
            let c = paper_mark_ink(spectrum_stop(stop), ctx.cfg.theme_bg);
            push_ink_rect(frame.out, geom, (x - arm, y, 2 * arm + 1, 1), c, ink);
            push_ink_rect(frame.out, geom, (x, y - arm, 1, 2 * arm + 1), c, ink);
            continue;
        }
        // The halo first, so a star truncated by the quad budget keeps its
        // centre over it; the halo goes before the arm does.
        if alpha >= SPARK_SHRINK_ALPHA && frame.halos.len() < halo_cap {
            let r = arm as f32 + SPARK_HALO_R_ADD_PX;
            push_halo_add(
                frame.halos,
                geom,
                x as f32,
                y as f32,
                r,
                r,
                premul_rgb(rgb, (cov * SPARK_HALO_SHARE) as u8),
            );
        }
        let core = lerp_rgb(0x00FF_FFFF, rgb, SPARK_CORE_TINT);
        let cap = frame.out.len() + SPARK_STAR_QUAD_CAP;
        push_twinkle_star(frame.out, geom, x, y, arm, cov as u8, false, core, cap);
    }
}

// ===========================================================================
// §6.5 layers 5 and 11, §6.12 — the stars this producer mints
// ===========================================================================

/// §6.5 layer 5, the shed fragments: the part the meteor decides.
///
/// `n = SHED_N(cells)` m3 grains at hashed stations `s_i ∈ [0.10, 0.70]·L`,
/// each born on the frame the head passes its station, so on frame 0 every
/// station inside `p₀ = 0.28` is already alight (T2). Each goes to
/// [`Stardust::sow_shed`] with the station, the head's observed velocity there
/// in px/ms (T4: inherited, never smoothed) and the arc colour at the station;
/// the sky prices, clears and builds it. Silent (D5). A meteor always sheds,
/// with or without a live ribbon (§19.1).
fn shed(m: &mut Meteor, f: &Flight, ctx: &Ctx<'_>, sown: &mut Vec<Sow>) {
    let n = usize::from(shed_n(m.cells)).min(8);
    let cw = (ctx.geom.cw as f32).max(1.0);
    let passed = f.head_travel_share();
    let t_ms = (m.t_flight.as_secs_f32() * 1000.0).max(1.0);
    let speed = head_speed(f.l, t_ms, f.u);
    for i in 0..n {
        let bit = 1_u8 << i;
        if m.shed & bit != 0 {
            continue;
        }
        let share = SHED_S_MIN + (SHED_S_MAX - SHED_S_MIN) * unit01(m.seed, 0x5ED0 ^ i as u32);
        if passed < share {
            continue;
        }
        m.shed |= bit;
        let s = share * f.l;
        let at_px = (m.x0 + f.unit.0 * s, m.y0 + f.unit.1 * s);
        // Seam point 12: a station a scroll has carried off the glass is
        // spent, not sown — a fragment born above the window would hold a
        // pool slot for its whole life and light nothing (dropped, never
        // clamped).
        if !on_glass(ctx.geom, at_px.0, at_px.1) {
            continue;
        }
        sown.push(Sow::Shed {
            at: ctx.now,
            spec: ShedSow {
                at_px,
                v_head: (f.unit.0 * speed, f.unit.1 * speed),
                // §6.5 layer 5: `t_m(s_i)`. The station is `s` from the
                // launch, so `L − s` behind the head at the landing: the
                // fragment wears the train's colour where it was shed and
                // reads as a piece of the meteor.
                tint_t: m.arc((f.l - s) / cw),
                seed: mix32(m.seed ^ (i as u32).wrapping_mul(0x9E37_79B9)),
                n: 1,
            },
        });
    }
}

/// The shower's room to the left: how far left of the landing a spark may
/// travel, px, or `None` when nothing bounds it.
///
/// The ribbon's law (`ribbon::Ribbon::hand_floor`): nothing the hand did not
/// put there goes left of where the hand has been on that row. The room is
/// measured from the landing to the centre of the hand's floor cell. Only the
/// sparks travel ([`draw_sparks`]: a spark's station is `vx · age`), so only
/// they are bounded; the flash, pin and burst are marks of the held cell and
/// keep their isotropy (`the_shower_stops_where_the_hand_stopped`).
///
/// The bound is applied at the mint by mirroring a leftward throw that would
/// cross the wall ([`mint_landing`]), keeping each spark's speed, life and
/// spread. A per-frame clamp would stop sparks dead mid-flight (§19.1), and a
/// proportional squeeze turns every leftward spark vertical when the landing
/// is the floor cell. The fan is the sky's and already respects glyph
/// clearance (§5.4).
///
/// `None` (no floor, or a floor on another row) bounds nothing. The landing's
/// row comes from the pixel endpoint, never [`Spawn::landing`], because a
/// scroll moves the endpoint and leaves the cell behind (seam point 12).
#[must_use]
fn spark_room(m: &Meteor, ctx: &Ctx<'_>, hand_floor: Option<(u16, u16)>) -> Option<f32> {
    let (row, col) = hand_floor?;
    let geom = ctx.geom;
    let ch = (geom.ch as f32).max(1.0);
    let land_row = ((m.y1 - geom.origin_y as f32) / ch).floor();
    if !land_row.is_finite() || land_row < 0.0 || land_row != f32::from(row) {
        return None;
    }
    Some((m.x1 - geom.cell_center(row, col).0).max(0.0))
}

/// Mint the marks the arrival edge owns (§6.7), on one `Instant`. The fan's
/// composition (1 gold m1 + 4 m2 + the rest m3, D6; or D8's 5-7 m3 + one m2
/// with no hero for an Enter's small landing) is the sky's class ladder
/// ([`Stardust::sow_fan`]); this decides the count, reach, hero flag and seed.
/// `hand_floor` is the ribbon's `(row, col)` floor, the shower's wall
/// ([`spark_room`]).
fn mint_landing(m: &Meteor, ctx: &Ctx<'_>, hand_floor: Option<(u16, u16)>) -> Landing {
    let at = m.arrival();
    let ch = ctx.geom.ch as f32;
    // The landing is the train's end — the endpoint a scroll moves with the
    // rest of the flight (seam point 12), never the spawn-time cell
    // re-derived: that put the pin, the ring and the fan `rows` rows below a
    // train's end on every PTY line-feed cascade (D18).
    let (x, y) = (m.x1, m.y1);

    // D8: an Enter's landing is sized by rows, not by the path. The flag is
    // `!ring` rather than a second licence test on purpose — the ring and the
    // small landing are the same decision, taken once at the spawn edge
    // (§6.11), so they cannot come apart later.
    let small = !m.ring;
    let cells_landing = if small {
        let drow = (m.y1 - m.y0).abs() / ch.max(1.0);
        ENTER_LANDING_CELLS_PER_ROW * drow
    } else {
        m.cells
    };

    let n = if small {
        // 5-7 m3 + 1 m2, and nothing else (§6.11).
        let extra = (cells_landing / ENTER_LANDING_CELLS_PER_ROW).round() as usize;
        (ENTER_LANDING_M3_MIN + extra.min(ENTER_LANDING_M3_MAX - ENTER_LANDING_M3_MIN) + 1)
            .min(super::timing::FAN_MAX_N)
    } else {
        // The graded count and reach — `(impact − 1)` above the floor, the
        // same form as the ring's — through the one function each that the
        // pins read (`fan_count`, `fan_reach_ch`).
        fan_count(m.cells, ctx.disp >= FAN_PARTY_DISP)
    };

    let reach = if small {
        FAN_REACH_MIN_CH * ch
    } else {
        fan_reach_ch(m.cells, m.grade) * ch
    };

    // The shower: sized by the path, and the big landing's only (D8). The
    // throws are hashed here, once, so the frame path does no trigonometry.
    let sparks = if small {
        0
    } else {
        ((SPARK_N_BASE + m.cells / SPARK_CELLS_PER).round() as usize).min(SPARK_N_MAX)
    };
    let seed = mix32(m.seed ^ 0xFA5E);
    let mut spark = [Spark::default(); SPARK_N_MAX];
    for (k, sp) in spark.iter_mut().enumerate().take(sparks) {
        *sp = mint_spark(seed, k, ch);
    }
    // The shower stops where the hand stopped ([`spark_room`]): the throw is
    // bounded at the mint, so the frame path is untouched and no spark changes
    // its mind in the air.
    if let Some(room) = spark_room(m, ctx, hand_floor) {
        for sp in spark.iter_mut().take(sparks) {
            if sp.vx < 0.0 && -sp.vx * sp.life > room {
                sp.vx = -sp.vx;
            }
        }
    }

    Landing {
        pin: Pin { at, t: m.arc(0.0) },
        walk: LandingWalk {
            t_land: m.t_land,
            band: m.band,
        },
        // The star is hashed off its own salt of the landing seed, so the
        // shower and the starburst are not correlated shapes. `m.ring` is
        // still the flag's name at the spawn edge — the one decision that
        // says "this landing is the big one" (D8) — and the mark it now
        // licenses is the burst.
        burst: m
            .ring
            .then(|| mint_burst(mix32(seed ^ 0xB0B5), at, m.cells)),
        flash: m.ring,
        fan: Fan {
            at,
            n: n as u8,
            reach,
            seed,
            hero: !small,
        },
        x,
        y,
        sparks: sparks as u8,
        spark,
        sky: None,
    }
}

// ===========================================================================
// Small shared arithmetic
// ===========================================================================

/// Milliseconds from `a` to `b`, saturating at zero — every age in this module
/// runs through it, so a clock that moved backwards can never produce a
/// negative age and a negative age can never reach a coverage byte.
#[inline]
fn ms_since(a: Instant, b: Instant) -> f32 {
    b.saturating_duration_since(a).as_secs_f32() * 1000.0
}

/// Is a window pixel inside the effects box (`Geom::fx_*`)? The one test every
/// mark this file mints at a pixel a scroll may have moved runs first: a
/// landing or a station outside it is dropped, never clamped (seam point 12).
/// The sky's own glass test, on the rounded pixel.
fn on_glass(geom: Geom, x: f32, y: f32) -> bool {
    super::stardust::on_glass((x.round() as i32, y.round() as i32), geom)
}

/// The head's position at an arbitrary instant — §6.9's chain test needs it,
/// and it must be the same curve the emit draws.
fn head_at(m: &Meteor, now: Instant) -> (f32, f32) {
    let t = (m.t_flight.as_secs_f32() * 1000.0).max(1.0);
    let p = enter_at_speed(ms_since(m.t0, now) / t);
    let (dx, dy) = m.delta();
    (m.x0 + dx * p, m.y0 + dy * p)
}

/// The head's speed in px/ms at `u`, from `enter-at-speed`'s derivative:
/// `p(u) = 1 − (1 − p₀)(1 − u)^2.2` ⇒ `dp/dt = (1 − p₀)·2.2·(1 − u)^1.2 / T`.
/// Analytic, so a frame stays a pure function of `now` (§18) while the
/// shoulder still spans the true per-frame travel (T7).
fn head_speed(l: f32, t_ms: f32, u: f32) -> f32 {
    if t_ms <= 0.0 {
        return 0.0;
    }
    l * (1.0 - FLIGHT_P0) * FLIGHT_ENTER_EXP * (1.0 - clamp01(u)).powf(FLIGHT_ENTER_EXP - 1.0)
        / t_ms
}

/// The shortest distance between two segments (§6.9's corridor test).
///
/// Crossing segments are zero by definition; otherwise the minimum is attained
/// at one of the four endpoint-to-segment distances, which is exact and needs
/// no parallel-case special pleading.
fn seg_gap(a0: (f32, f32), a1: (f32, f32), b0: (f32, f32), b1: (f32, f32)) -> f32 {
    if segments_cross(a0, a1, b0, b1) {
        return 0.0;
    }
    point_seg(a0, b0, b1)
        .min(point_seg(a1, b0, b1))
        .min(point_seg(b0, a0, a1))
        .min(point_seg(b1, a0, a1))
}

/// Distance from a point to a segment.
fn point_seg(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / len2).clamp(0.0, 1.0)
    };
    (p.0 - (a.0 + vx * t)).hypot(p.1 - (a.1 + vy * t))
}

/// Do two segments properly cross? Four orientation signs, no division.
fn segments_cross(a0: (f32, f32), a1: (f32, f32), b0: (f32, f32), b1: (f32, f32)) -> bool {
    let cross = |o: (f32, f32), p: (f32, f32), q: (f32, f32)| {
        (p.0 - o.0) * (q.1 - o.1) - (p.1 - o.1) * (q.0 - o.0)
    };
    let (d1, d2) = (cross(b0, b1, a0), cross(b0, b1, a1));
    let (d3, d4) = (cross(a0, a1, b0), cross(a0, a1, b1));
    (d1 * d2 < 0.0) && (d3 * d4 < 0.0)
}

/// The spawn seed — §18's `(row, col, born)`, with the spawn ordinal standing
/// in for `born` (see [`Meteors::ord`]).
#[inline]
fn seed_of(row: u16, col: u16, ord: u32) -> u32 {
    mix32(u32::from(row) << 20 ^ u32::from(col) << 8 ^ ord.wrapping_mul(0x9E37_79B9))
}

/// A seeded value in `[0, 1)` — the one place in this file a hash becomes a
/// number (the shed stations), so no draw rolls its own bias.
#[inline]
fn unit01(seed: u32, salt: u32) -> f32 {
    hash01(mix32(seed ^ salt))
}

/// One radial halo of premultiplied additive light: the dark arm's coma and
/// nucleus (§6.5 layers 6 and 7).
///
/// The sky's [`push_halo_quads`] on the rounded centre and radii: the bounding
/// rect is clamped to the effects box and split into per-cell-row quads that
/// share one centre and pair of falloff radii. The centre is never clamped
/// into the box, which would render an off-grid sliver at a fraction of peak
/// instead of its true ~0. Radii are floored to 1 (divide-by-zero guard); a
/// zero `peak` draws nothing.
fn push_halo_add(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    peak: u32,
) {
    let centre = (cx.round() as i32, cy.round() as i32);
    let radii = (rx.round() as i32, ry.round() as i32);
    push_halo_quads(out, geom, usize::MAX, centre, radii, peak, HaloMode::Add);
}

/// One radial halo as a source-over ink veil, the light arm's twin (§6.10).
///
/// `peak` is the centre opacity: it rides the colour's high byte as
/// `aterm_render::halo_over_cap`'s ceiling and sizes the veil. The colour is
/// straight (unpremultiplied), because under `HaloMode::Over` the radial
/// falloff scales opacity rather than light. Below the light arm's visibility
/// floor (§3.3's `peak < 96` cull) the veil is dropped.
fn push_halo_veil(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    c: (f32, f32),
    r: (f32, f32),
    color: u32,
    peak: u8,
) {
    if peak < LIGHT_HALO_MIN_PEAK {
        return;
    }
    // The light rasterizer sizes a veil by its own opacity, so a dim veil is
    // also a small one; mirroring that here keeps the two arms the same shape.
    let scale = (f32::from(peak) / 255.0).max(0.55);
    let centre = (c.0.round() as i32, c.1.round() as i32);
    let radii = ((r.0 * scale).round() as i32, (r.1 * scale).round() as i32);
    let ink = (u32::from(peak) << 24) | (color & 0x00FF_FFFF);
    push_halo_quads(out, geom, usize::MAX, centre, radii, ink, HaloMode::Over);
}

/// The light arm's halo visibility floor (§3.3, §6.10) — a veil dimmer than
/// this is dropped, not drawn.
const LIGHT_HALO_MIN_PEAK: u8 = 96;

/// A rect of source-over ink, clamped to the effects box and split into
/// per-cell-row quads with origin-anchored row damage tags. `push_fx_rect`'s
/// light twin: it stamps the coverage as the quad's opacity, so the
/// destination is attenuated instead of added to. At `alpha == 0` the two
/// emit the same.
fn push_ink_rect(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    rect: (i32, i32, i32, i32),
    color: u32,
    alpha: u8,
) {
    if alpha == 0 {
        return;
    }
    let (x, y, w, h) = rect;
    push_fx_quad(out, geom, x, y, w, h, premul_rgb(color, alpha), alpha);
}

/// Compile-time restatements of the budget arithmetic §18 depends on, so a
/// later retune cannot silently break the "a ping-pong never blanks the
/// ribbon" law by editing one number.
const _: () = {
    // The `under` stream: two meteors' colour trains and the ribbon. The
    // starburst emits nothing under the ink, so the rest up to MAX_QUADS is
    // slack.
    assert!(
        FLIGHT_MAX_LIVE * UNDER_QUAD_CAP + super::ribbon::RIBBON_QUAD_BUDGET <= 16_384,
        "§18: two meteors' trains and the ribbon must fit inside MAX_QUADS"
    );
    // The landing's own `out` share: every live landing's burst plus two
    // meteors' white layers, which is what took the splash's place.
    assert!(
        LANDING_POOL * BURST_QUAD_CAP + FLIGHT_MAX_LIVE * WHITE_QUAD_CAP <= 16_384,
        "§18: three bursts and two white layers must fit inside MAX_QUADS"
    );
    // The impact's last light (the longest spark, the burst, the colour
    // train's chroma cull `τ·ln(1/0.12)` and its root's retract) is inside the
    // pool's own horizon (§6.2).
    assert!(
        SPARK_LIFE_MAX_MS <= FLIGHT_OFF_GLASS_MS
            && RING_MS_MAX <= FLIGHT_OFF_GLASS_MS
            && TRAIN_COLOUR_TAU_MS * RETIRE_DECAY <= FLIGHT_OFF_GLASS_MS
            && ROOT_SUCK_MS <= FLIGHT_OFF_GLASS_MS,
        "every impact mark must be off glass by FLIGHT_OFF_GLASS_MS"
    );
    assert!(
        SPARK_N_MAX <= 255 && SPARK_CONE_SHARE > 0.0 && SPARK_CONE_SHARE < 0.5,
        "the spark count is a u8 and the cone opens upward"
    );
    assert!(
        SPARK_ARM_MIN_PX >= 1 && SPARK_ARM_MAX_PX >= SPARK_ARM_MIN_PX && SPARK_ARM_MAX_PX <= 255,
        "a spark's arm is a u8 of at least one pixel"
    );
    // The impact's clocks: the flash's colour is inside the pool's horizon;
    // the flash and the burst's hold are attack inside the pin's own window,
    // so the cadence law's attack is the pin's as before; and the sky mask
    // holds every cell it is asked about.
    assert!(
        FLASH_COLOUR_MS <= FLIGHT_OFF_GLASS_MS
            && FLASH_BURST_MS <= PIN_MS
            && RING_HOLD_MS <= PIN_MS
            && RING_HOLD_MAX_MS <= PIN_MS
            && FLASH_COLOUR_MS <= RING_MS,
        "the flash and the shockwave's hold must sit inside the pin's attack"
    );
    assert!(
        2 * (JET_PROBE_CELLS as usize) < u32::BITS as usize,
        "the sky mask must hold every cell of the landing row a jet can reach"
    );
    assert!(
        FLASH_FULL_COV > TRANSIENT_STAR_COV_CEIL && FLASH_FULL_COV < 255.0,
        "the flash over a blank cell is louder than the cap and under the caret"
    );
    assert!(
        WHITE_QUAD_CAP < UNDER_QUAD_CAP,
        "§6.5: the white layer is the short one"
    );
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::cursor_glow::SoundCue;
    use crate::rainbow_kitty::stardust::{FAN_RISE_MAX_CH, Star, StarClass, StarLane};
    use crate::rainbow_kitty::timing::FAN_HERO_N;
    use crate::rainbow_kitty::{CaretSeam, Config};
    use crate::spectrum::SPECTRUM_ANCHORS;

    fn geom() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 1200,
            win_h: 800,
            head: 0,
        }
    }

    fn config() -> Config {
        Config {
            dark_theme: true,
            intensity: 1.0,
            duration: Duration::from_millis(900),
            ribbon_tall: true,
            ribbon_flat: false,
            theme_fg: 0x00E8_E8F0,
            theme_bg: 0x0016_161C,
            reduced_motion: false,
        }
    }

    /// A frame's worth of host scratch, reused exactly as the host reuses
    /// `CursorGlow`'s.
    #[derive(Default)]
    struct Scratch {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<BeamVertex>,
        cues: Vec<SoundCue>,
    }

    impl Scratch {
        fn frame(&mut self) -> Frame<'_> {
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
        fn clear(&mut self) {
            self.under.clear();
            self.out.clear();
            self.halos.clear();
            self.beams.clear();
            self.cues.clear();
        }
        /// One emit into a cleared scratch.
        fn emit(&mut self, m: &mut Meteors, ctx: &Ctx<'_>) {
            self.clear();
            let mut fr = self.frame();
            m.emit(ctx, &mut fr);
        }
    }

    fn ctx_with(
        now: Instant,
        cfg: &Config,
        caret: (u16, u16),
        caret_t: f32,
        disp: f32,
        birth_disp: f32,
    ) -> Ctx<'_> {
        Ctx {
            now,
            geom: geom(),
            cfg,
            disp,
            birth_disp,
            phase: 0.0,
            caret,
            caret_t,
            caret_walk: None,
            mend: None,
            surge: 0.0,
            flow: Default::default(),
        }
    }

    fn ctx_at(now: Instant, cfg: &Config, caret: (u16, u16), caret_t: f32) -> Ctx<'_> {
        ctx_with(now, cfg, caret, caret_t, 0.5, 0.5)
    }

    fn mv_as(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
        Event::Move {
            from,
            to,
            licence,
            dir: Dir::of(
                i32::from(to.1) - i32::from(from.1),
                i32::from(to.0) - i32::from(from.0),
            ),
        }
    }

    fn mv(from: (u16, u16), to: (u16, u16)) -> Event {
        mv_as(from, to, Licence::Nav)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A sky whose glyph probe has looked at rows 3-10 and found them blank
    /// — §5.4's gate, fed explicitly: a test that left the probe empty would
    /// be relying on the free-transient asymmetry rather than stating it.
    fn sky() -> Stardust {
        let mut dust = Stardust::new();
        for row in 3..=10 {
            dust.probe_mut().probe_row(row, &[false; 120]);
        }
        dust
    }

    /// The sky with the landing row and its two neighbours proved blank — the
    /// three rows the host's probe holds (`GlyphProbe::ROWS`), which is what
    /// the flash and the splash are priced on. `sky()` probes eight rows into
    /// a three-row probe and keeps the last three (8..=10), leaving the
    /// landing row's neighbours unknown — and unknown is not blank (L4).
    fn sky_around(row: i32) -> Stardust {
        let mut dust = Stardust::new();
        for r in row - 1..=row + 1 {
            dust.probe_mut().probe_row(r, &[false; 120]);
        }
        dust
    }

    fn lane(dust: &Stardust, lane: StarLane) -> Vec<&Star> {
        dust.live_iter().filter(|s| s.lane == lane).collect()
    }

    /// The premultiplied luminance of a quad, in levels — the only thing the
    /// compositor will do with it, so the only thing an oracle should read.
    fn lum(c: u32) -> f32 {
        0.2126 * ((c >> 16) & 0xFF) as f32
            + 0.7152 * ((c >> 8) & 0xFF) as f32
            + 0.0722 * (c & 0xFF) as f32
    }

    fn chan(c: u32) -> (u32, u32, u32) {
        ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF)
    }

    /// Achromatic and lit — the white heat, the nucleus, the pin's heart.
    fn is_white(c: u32) -> bool {
        let (r, g, b) = chan(c);
        r == g && g == b && r > 0
    }

    /// Lit and not achromatic — a spectrum mark.
    fn is_chromatic(c: u32) -> bool {
        let (r, g, b) = chan(c);
        r.max(g).max(b) > 0 && !(r == g && g == b)
    }

    /// Which of the seven named stops a premultiplied quad colour is nearest,
    /// after normalising away the coverage it was multiplied by. C1: the seven
    /// stops are the only colour law, so "which stop is this" is a total
    /// question with a total answer.
    fn stop_of(c: u32) -> usize {
        let (r, g, b) = chan(c);
        let hi = r.max(g).max(b).max(1) as f32;
        let n = |v: u32| v as f32 * 255.0 / hi;
        let (nr, ng, nb) = (n(r), n(g), n(b));
        let mut best = 0;
        let mut near = f32::INFINITY;
        for (i, &a) in SPECTRUM_ANCHORS.iter().enumerate() {
            let (ar, ag, ab) = chan(a);
            let d = (nr - ar as f32).powi(2) + (ng - ag as f32).powi(2) + (nb - ab as f32).powi(2);
            if d < near {
                near = d;
                best = i;
            }
        }
        best
    }

    /// The white nucleus's centre: the achromatic halo with the highest
    /// premultiplied peak (§3.2 rank 1 — the brightest pixel is the smallest
    /// thing).
    fn nucleus(halos: &[RainHalo]) -> Option<&RainHalo> {
        halos
            .iter()
            .filter(|h| is_white(h.color))
            .max_by(|a, b| lum(a.color).total_cmp(&lum(b.color)))
    }

    fn nucleus_cx(halos: &[RainHalo]) -> Option<(f32, f32, f32)> {
        nucleus(halos).map(|h| (f32::from(h.cx), f32::from(h.cy), lum(h.color)))
    }

    // -- the arc (kept from the frozen contract) ---------------------------

    // -- T2, the frame-0 law ------------------------------------------------

    /// T2 / §6.6, the acceptance image: on the frame the caret is first
    /// observed at its landing, the head is already at `p₀ = 0.28` of the
    /// path, moving, with the train lit behind it back to the launch cell.
    #[test]
    fn the_head_is_already_moving_and_lit_on_frame_zero() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m
            .on_event(&mv((5, 0), (5, 80)), t0, &ctx)
            .expect("an 80-cell Home/End must fly");
        assert_eq!(spawn.t0, t0, "§6.7: the spawn edge IS the observed frame");

        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        assert!(
            !sc.under.is_empty(),
            "T2: the colour train must be lit on frame zero"
        );
        assert!(
            !sc.out.is_empty(),
            "T2: the white heat must be lit on frame zero"
        );

        let (cx, _, peak) = nucleus_cx(&sc.halos).expect("T2: the nucleus must exist at age 0");
        let (x0, _) = geom().cell_center(5, 0);
        let (x1, _) = geom().cell_center(5, 80);
        let l = x1 - x0;
        let want = x0 + FLIGHT_P0 * l;
        assert!(
            (cx - want).abs() <= 0.02 * l + NUCLEUS_LEAD_PX + 1.0,
            "the head is at {cx}, not at p0 = {want} (0.28 of the path)"
        );
        assert!(peak >= 100.0, "the nucleus is dim on frame zero: {peak}");

        // …and the shoulder is lit behind it, within 0.06·L.
        let hot = sc
            .out
            .iter()
            .filter(|q| {
                let x = f32::from(q.x);
                (cx - SHOULDER_SHARE * l..=cx).contains(&x) && lum(q.color) >= 100.0
            })
            .count();
        assert!(
            hot > 0,
            "T2: no white ≥ 100 within 0.06·L behind the head on frame zero"
        );

        // T2's second half: it is already moving. The head's displacement over
        // the first 8 ms is at least a tenth of the path at every published T.
        for cells in [8.0_f32, 40.0, 200.0] {
            let t = super::super::timing::flight_ms(cells);
            let d = enter_at_speed(8.0 / t) - enter_at_speed(0.0);
            assert!(
                d >= 0.10,
                "T = {t}: the head moved {d} of the path in 8 ms, not 0.10"
            );
        }
    }

    // -- §6.4 / D4, the phase lock -----------------------------------------

    // -- §3.2, the luminance hierarchy --------------------------------------

    // -- §6.2: off the glass --------------------------------------------------

    /// The impact's horizon: the impact stays on glass past `T + 400` (the
    /// burst, the falling sparks and the rainbow's root still leaving) and is
    /// off, every stream, by `T + 600` at every published distance, with the
    /// pool at rest so the host may idle (T6) (owner, 2026-09-08).
    #[test]
    fn the_impact_is_off_glass_by_t_plus_600() {
        let cfg = config();
        for cols in [8_u16, 34, 80] {
            let t0 = Instant::now();
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, (5, cols), 0.25);
            let spawn = m.on_event(&mv((5, 0), (5, cols)), t0, &ctx).expect("fly");
            let arrival = t0 + spawn.t_flight;
            let mut sc = Scratch::default();
            // Drive the whole life so every edge is spent on a real frame.
            let mut t = t0;
            while t <= arrival + ms(400) {
                let at = ctx_at(t, &cfg, (5, cols), 0.25);
                sc.emit(&mut m, &at);
                t += ms(8);
            }
            // Still an impact at T + 400: coloured points above the row (the
            // sparks) and the train's rainbow still on it.
            let at = ctx_at(arrival + ms(400), &cfg, (5, cols), 0.25);
            sc.emit(&mut m, &at);
            let (_, ly) = geom().cell_center(5, cols);
            let small = |q: &GlowQuad| i32::from(q.w) <= SPARK_PX && i32::from(q.h) <= SPARK_PX;
            let sparks = sc
                .out
                .iter()
                .filter(|q| is_chromatic(q.color) && small(q))
                .count();
            assert!(
                sparks > 0 && !sc.under.is_empty(),
                "{cols} cells: the impact is already gone at T + 400 ({sparks} sparks, {} under)",
                sc.under.len()
            );
            assert!(
                sc.out.iter().any(|q| is_chromatic(q.color)
                    && small(q)
                    && (f32::from(q.y) - ly).abs() >= 4.0),
                "{cols} cells: no spark has left the row by T + 400"
            );
            while t <= arrival + ms(600) {
                let at = ctx_at(t, &cfg, (5, cols), 0.25);
                sc.emit(&mut m, &at);
                t += ms(8);
            }
            for after in [600_u64, 650] {
                let at = ctx_at(arrival + ms(after), &cfg, (5, cols), 0.25);
                sc.emit(&mut m, &at);
                assert!(
                    sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty(),
                    "{cols} cells: meteor light survived to T + {after} ms"
                );
            }
            assert!(m.at_rest(), "{cols} cells: the pool never came to rest");
            assert!(
                m.next_change_deadline(arrival + ms(650)).is_none(),
                "{cols} cells: a resting pool still named a deadline (T6)"
            );
        }
    }

    /// The train is the rainbow: on every frame after the first, the colour
    /// train runs from red to violet (within 8 % of both ends of the spectrum
    /// table, at least five named stops), and on frame 0 at least four stops;
    /// at 8, 40 and 80 cells, both directions, over a dead prompt, behind a
    /// fresh run and at a late phase. The law is spelled on the table's span
    /// because the nearest-anchor classifier folds indigo into violet and
    /// misses red unless a slab lands near the reflection. Also pins D4: an
    /// unclamped arc never reads as one red bar (owner, 2026-09-08).
    #[test]
    fn the_train_carries_the_whole_spectrum_on_every_frame_of_the_flight() {
        use crate::spectrum::{SPECTRUM_LUT, SPECTRUM_LUT_LEN};
        let cfg = config();
        // The nearest table entry to a premultiplied quad colour.
        let lut_index = |c: u32| -> usize {
            let (r, g, b) = chan(c);
            let hi = r.max(g).max(b).max(1) as f32;
            let n = |v: u32| v as f32 * 255.0 / hi;
            let (nr, ng, nb) = (n(r), n(g), n(b));
            let mut best = 0;
            let mut near = f32::INFINITY;
            for (i, &a) in SPECTRUM_LUT.iter().enumerate() {
                let (ar, ag, ab) = chan(a);
                let d =
                    (nr - ar as f32).powi(2) + (ng - ag as f32).powi(2) + (nb - ab as f32).powi(2);
                if d < near {
                    near = d;
                    best = i;
                }
            }
            best
        };
        let (red_end, violet_end) = (
            (0.08 * (SPECTRUM_LUT_LEN - 1) as f32) as usize,
            (0.92 * (SPECTRUM_LUT_LEN - 1) as f32) as usize,
        );
        for t_land in [0.0_f32, 9.0 / 36.0, 0.6] {
            for cols in [8_u16, 40, 80] {
                for (from, to) in [((5_u16, 0_u16), (5_u16, cols)), ((5, cols), (5, 0))] {
                    let t0 = Instant::now();
                    let mut m = Meteors::new();
                    let ctx = ctx_at(t0, &cfg, to, t_land);
                    let spawn = m.on_event(&mv(from, to), t0, &ctx).expect("fly");
                    let mut sc = Scratch::default();
                    let mut t = t0;
                    let mut frame = 0;
                    while t <= t0 + spawn.t_flight {
                        sc.emit(&mut m, &ctx_at(t, &cfg, to, t_land));
                        let mut seen = [false; 7];
                        let (mut lo, mut hi) = (usize::MAX, 0_usize);
                        for q in &sc.under {
                            let (r, g, b) = chan(q.color);
                            if r.max(g).max(b) >= 16 && is_chromatic(q.color) {
                                seen[stop_of(q.color)] = true;
                                let i = lut_index(q.color);
                                lo = lo.min(i);
                                hi = hi.max(i);
                            }
                        }
                        let n = seen.iter().filter(|s| **s).count();
                        // Frame 0 of an 8-cell hop is 20 px of train, ten
                        // slabs over a unit and a half, which from a late
                        // phase folds on the reflection: four stops on the
                        // worst frame 0, five and the whole span from frame 1.
                        let want = if frame == 0 { 4 } else { 5 };
                        assert!(
                            n >= want,
                            "t_land {t_land}, {from:?}->{to:?}, frame {frame}: the train showed \
                             {n} stops, not the rainbow: {seen:?}"
                        );
                        if frame > 0 {
                            assert!(
                                lo <= red_end && hi >= violet_end,
                                "t_land {t_land}, {from:?}->{to:?}, frame {frame}: the train \
                                 spans table entries {lo}..{hi}, not red to violet"
                            );
                        }
                        t += ms(8);
                        frame += 1;
                    }
                    assert!(frame >= 8, "{cols} cells: only {frame} frames were checked");
                }
            }
        }
    }

    // -- D15, the vertical arm ----------------------------------------------

    /// D15: `ribbon_beam` is x-major and skips any segment with `|Δx| < 1e-3`,
    /// so a vertical recall needs the transposed twin to draw at all. A
    /// nav-licensed recall is a full meteor (D8, §12.3).
    #[test]
    fn a_vertical_recall_meteor_draws_its_train() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (8, 40), 0.25);
        let spawn = m
            .on_event(&mv((5, 40), (8, 40)), t0, &ctx)
            .expect("a 3-row recall must fly");
        assert!(spawn.ring, "D8: a history recall rings");
        assert!(spawn.rain, "§12.3 / D6: a history recall rains on its hero");

        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        assert!(
            !sc.under.is_empty(),
            "D15: the vertical train drew NOTHING — the x-major arm was taken"
        );

        // By the arrival edge every crossed row carries train light, and the
        // landing rings.
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (8, 40), 0.25);
        sc.emit(&mut m, &land);
        for row in 5_u16..=8 {
            assert!(
                sc.under.iter().any(|q| q.row == row),
                "row {row} was crossed but carries no train quad"
            );
        }
        assert!(
            m.landings.iter().any(|l| l.burst.is_some()),
            "a recall's landing minted no ring"
        );
    }

    // -- §6.9, retire-previous ----------------------------------------------

    /// §6.9: a new meteor whose corridor comes within 1 `ch` of a live one
    /// retires it; the older train's colour fades with `R = 60 ms` from the
    /// alpha it had and reaches the chroma cull exactly there. The pool is
    /// capped at 2 heads under any mash, on a fixed pool (§18).
    #[test]
    fn a_second_meteor_retires_the_first_without_a_pop() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 80), 0.25);
        m.on_event(&mv((5, 0), (5, 80)), t0, &a).expect("E flies");
        let before = m.live[0].colour_alpha(t0 + ms(30));

        let b = ctx_at(t0 + ms(30), &cfg, (5, 0), 0.25);
        m.on_event(&mv((5, 80), (5, 0)), t0 + ms(30), &b)
            .expect("A flies");
        let first = m.live[0];
        assert!(
            first.retiring(),
            "the crossing meteor did not retire the first"
        );
        assert!(
            (first.colour_alpha(t0 + ms(30)) - before).abs() < 1e-3,
            "the retire POPPED: alpha jumped at the edge"
        );

        let mut last = f32::INFINITY;
        for at in [30_u64, 45, 60, 75, 90] {
            let v = first.colour_alpha(t0 + ms(at));
            assert!(v <= last + 1e-4, "the retired train brightened at +{at} ms");
            last = v;
        }
        assert!(
            first.colour_alpha(t0 + ms(90)) <= CHROMA_CULL_ALPHA,
            "the retired train is still above the cull at +90 ms"
        );

        // Cap 2 under a mash of ten, on a pool reserved once and never
        // reallocated.
        let cap = m.live.capacity();
        for i in 0..10_u64 {
            let now = t0 + ms(100 + i * 5);
            let (from, to) = if i % 2 == 0 {
                ((5_u16, 0_u16), (5_u16, 80_u16))
            } else {
                ((5, 80), (5, 0))
            };
            let c = ctx_at(now, &cfg, to, 0.25);
            let _ = m.on_event(&mv(from, to), now, &c);
            let flying = m.live.iter().filter(|x| !x.retiring()).count();
            assert!(
                flying <= FLIGHT_MAX_LIVE,
                "{flying} heads in the air at once"
            );
            assert_eq!(m.live(), flying, "`live()` counts finishing trains");
            assert!(
                m.live.len() <= METEOR_POOL,
                "the pool grew to {} under a mash",
                m.live.len()
            );
        }
        assert_eq!(
            m.live.capacity(),
            cap,
            "the meteor pool reallocated under a mash"
        );
    }

    /// §6.9 / §19.1: a head that was handed over sheds nothing more; no
    /// fragment is born behind a phantom head.
    #[test]
    fn a_retired_head_sheds_nothing_more() {
        let cfg = config();
        let t0 = Instant::now();
        let run = |retire: bool| -> (u8, u8) {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
            m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("E flies");
            let mut sc = Scratch::default();
            let edge = t0 + ms(8);
            sc.emit(&mut m, &ctx_at(edge, &cfg, (5, 80), 0.25));
            let early = m.live[0].shed;
            if retire {
                let b = ctx_at(edge, &cfg, (5, 70), 0.25);
                m.on_event(&mv((5, 80), (5, 70)), edge, &b)
                    .expect("the crossing flies");
                assert!(m.live[0].retiring());
            }
            for after in [12_u64, 30, 45, 59] {
                sc.emit(&mut m, &ctx_at(edge + ms(after), &cfg, (5, 70), 0.25));
            }
            (early, m.live[0].shed)
        };
        let all = (1_u8 << shed_n(80.0)) - 1;
        let (early, late) = run(false);
        assert_ne!(
            early, all,
            "every station was already born at +8 ms — nothing to test"
        );
        assert_eq!(late, all, "a live head passes every station by +67 ms");
        let (early_r, late_r) = run(true);
        assert_eq!(early_r, early, "the two runs diverged before the retire");
        assert_eq!(
            late_r, early_r,
            "a retired head shed {:#b} → {:#b}",
            early_r, late_r
        );
    }

    /// §6.9, chaining: a same-direction continuation from within a cell of a
    /// live head chains; the old train is cut at its head and finishes on its
    /// own law, and the continuation flies its own segment, so the one head on
    /// glass never jumps backwards.
    #[test]
    fn a_chained_continuation_never_puts_the_head_behind_where_it_was() {
        let cfg = config();
        let t0 = Instant::now();
        let (x0, _) = geom().cell_center(5, 0);
        let (x40, _) = geom().cell_center(5, 40);

        // Chain on the arrival edge.
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 40), 0.25);
        let s1 = m.on_event(&mv((5, 0), (5, 40)), t0, &a).expect("flies");
        let at = t0 + s1.t_flight;
        let b = ctx_at(at, &cfg, (5, 80), 0.25);
        m.on_event(&mv((5, 40), (5, 80)), at, &b)
            .expect("the continuation flies");
        assert_eq!(m.live.len(), 2);
        assert!(m.live[0].chained && m.live[0].retired_at.is_none());
        assert!(!m.live[1].chained);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &b);
        let (cx, _, _) = nucleus_cx(&sc.halos).expect("one head");
        assert!(
            cx >= x40 - 1.0,
            "the chained head appeared at {cx}, behind the old head at {x40}"
        );
        let heads: std::collections::BTreeSet<u16> = sc
            .halos
            .iter()
            .filter(|h| is_white(h.color))
            .map(|h| h.cx)
            .collect();
        assert_eq!(
            heads.len(),
            1,
            "two heads in the air after a chain: {heads:?}"
        );
        assert!(
            m.landings.iter().all(|l| (l.x - x40).abs() > 1.0),
            "the handed-over head celebrated a landing the caret left"
        );

        // Chain mid-flight: the old train is cut at the head's position on
        // the chain edge, and its arrival edge becomes that edge.
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 40), 0.25);
        let s1 = m.on_event(&mv((5, 0), (5, 40)), t0, &a).expect("flies");
        let at = t0 + ms(30);
        let t_ms = s1.t_flight.as_secs_f32() * 1000.0;
        let hx = x0 + enter_at_speed(30.0 / t_ms) * (x40 - x0);
        let from = (5_u16, ((hx - x0) / geom().cw as f32).round() as u16);
        let b = ctx_at(at, &cfg, (5, 70), 0.25);
        m.on_event(&mv(from, (5, 70)), at, &b)
            .expect("the continuation flies");
        let old = m.live[0];
        assert!(
            old.chained,
            "a continuation within a cell of the head must chain"
        );
        assert!(
            (old.x1 - hx).abs() <= 1.0,
            "the old train was not cut at its head"
        );
        assert_eq!(
            old.t_flight,
            ms(30),
            "the old arrival edge is the chain edge"
        );
        sc.emit(&mut m, &b);
        let (cx, _, _) = nucleus_cx(&sc.halos).expect("one head");
        assert!(
            cx >= hx - 1.0,
            "the head jumped backwards on the chain frame"
        );
    }

    /// §6.9's cap, with a chain: when a continuation makes three un-retired
    /// trains, the chained stub takes the 60 ms finish, never the head in
    /// flight on another row.
    #[test]
    fn a_chained_stub_never_costs_another_row_its_head() {
        let cfg = config();
        let t0 = Instant::now();
        let (x0, _) = geom().cell_center(5, 0);
        let (x40, _) = geom().cell_center(5, 40);
        let mut m = Meteors::new();
        // B, the older flight, on row 7.
        let b = ctx_at(t0, &cfg, (7, 40), 0.25);
        m.on_event(&mv((7, 0), (7, 40)), t0, &b).expect("B flies");
        // A on row 5, 5 ms later …
        let a_at = t0 + ms(5);
        let a = ctx_at(a_at, &cfg, (5, 40), 0.25);
        let s_a = m.on_event(&mv((5, 0), (5, 40)), a_at, &a).expect("A flies");
        assert_eq!(m.live(), FLIGHT_MAX_LIVE);
        // … and A′ chains A from within a cell of its head, 30 ms into its
        // flight.
        let at = a_at + ms(30);
        let t_ms = s_a.t_flight.as_secs_f32() * 1000.0;
        let hx = x0 + enter_at_speed(30.0 / t_ms) * (x40 - x0);
        let from = (5_u16, ((hx - x0) / geom().cw as f32).round() as u16);
        let c = ctx_at(at, &cfg, (5, 70), 0.25);
        m.on_event(&mv(from, (5, 70)), at, &c).expect("A′ flies");
        let (old_b, old_a, new_a) = (m.live[0], m.live[1], m.live[2]);
        assert!(old_a.chained, "A′ did not chain A");
        assert!(
            old_a.retiring(),
            "the chained stub is the third train on glass and takes the 60 ms finish"
        );
        assert!(
            !old_b.retiring(),
            "B — a head in flight on another row — was evicted by A's own continuation"
        );
        assert!(!new_a.retiring(), "the continuation itself was retired");
        assert_eq!(
            m.live(),
            FLIGHT_MAX_LIVE,
            "two live meteors after the chain"
        );
        // And B's head is still drawn on that frame, on its own row.
        let mut sc = Scratch::default();
        sc.emit(&mut m, &c);
        let (_, y7) = geom().cell_center(7, 40);
        assert!(
            sc.halos
                .iter()
                .any(|h| is_white(h.color) && (f32::from(h.cy) - y7).abs() <= 1.0),
            "B's nucleus is gone from row 7"
        );
    }

    /// §6.9: only one head is ever in the air per row. Two same-row flights
    /// that never come near each other still retire the older head, unless it
    /// has landed, in which case its train finishes on its own law.
    #[test]
    fn only_one_head_is_in_the_air_per_row() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 10), 0.25);
        m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
        let b = ctx_at(t0 + ms(10), &cfg, (5, 70), 0.25);
        m.on_event(&mv((5, 60), (5, 70)), t0 + ms(10), &b)
            .expect("flies");
        assert!(m.live[0].retiring(), "two heads in the air on one row");
        assert_eq!(m.live(), 1);

        // Two rows apart is two heads: the corridor rule alone decides.
        let mut m = Meteors::new();
        m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
        let c = ctx_at(t0 + ms(10), &cfg, (7, 70), 0.25);
        m.on_event(&mv((7, 60), (7, 70)), t0 + ms(10), &c)
            .expect("flies");
        assert_eq!(m.live(), 2, "flights on different rows retired each other");

        // A train already landed has no head in the air.
        let mut m = Meteors::new();
        let s = m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
        let later = t0 + s.t_flight + ms(10);
        let d = ctx_at(later, &cfg, (5, 70), 0.25);
        m.on_event(&mv((5, 60), (5, 70)), later, &d).expect("flies");
        assert!(
            !m.live[0].retiring(),
            "a landed train was retired by a distant newcomer"
        );
    }

    /// **§18 / §20.1's `a_ping_pong_never_blanks_the_ribbon`.** Each meteor's
    /// `under` share is hard-capped at [`UNDER_QUAD_CAP`], so two live
    /// meteors plus a full ribbon land exactly on `MAX_QUADS` and the ribbon
    /// is never the thing that sheds.
    #[test]
    fn a_ping_pong_never_spends_more_than_its_under_share() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 119), 1.0);
        m.on_event(&mv((5, 0), (5, 119)), t0, &a).expect("E flies");
        let b = ctx_at(t0 + ms(20), &cfg, (5, 0), 1.0);
        m.on_event(&mv((5, 119), (5, 0)), t0 + ms(20), &b)
            .expect("A flies");

        let mut sc = Scratch::default();
        let mut t = t0 + ms(20);
        while t <= t0 + ms(200) {
            let at = ctx_at(t, &cfg, (5, 0), 1.0);
            sc.emit(&mut m, &at);
            // Two trains, each under its own cap; the landing puts nothing
            // under the ink, so the trains have the whole share.
            let share = FLIGHT_MAX_LIVE * UNDER_QUAD_CAP;
            assert!(
                sc.under.len() <= share,
                "two meteors spent {} under quads — over their {share} share",
                sc.under.len()
            );
            t += ms(8);
        }
    }

    // -- §6.11, reduced motion; §6.12, the sub-floor hop ---------------------

    /// §6.11: reduced motion is static. The head is at the landing from the
    /// first frame, the train is the exponential profile at α 0.85, and a
    /// landing is minted whose drawing at `T + 100` and `T + 300` is
    /// byte-identical: no pin arms, no sparks, the flash and colours at full
    /// geometry from the first frame. The fan is minted and stays where it was
    /// born.
    #[test]
    fn reduced_motion_draws_the_static_form() {
        let mut cfg = config();
        cfg.reduced_motion = true;
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("fly");

        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        assert!(!sc.under.is_empty(), "the static train must still be drawn");
        let (cx, _, _) = nucleus_cx(&sc.halos).expect("the static head must be drawn");
        let (x1, ly) = geom().cell_center(5, 80);
        assert!(
            (cx - x1).abs() <= NUCLEUS_LEAD_PX + 2.0,
            "reduced motion put the head at {cx}, not at the landing {x1} — it flew"
        );

        // Past the arrival edge: the static landing — flash and colours, no
        // motion — and the fan is static.
        let land = ctx_at(t0 + spawn.t_flight + ms(4), &cfg, (5, 80), 0.25);
        sc.emit(&mut m, &land);
        assert_eq!(
            m.landings.len(),
            1,
            "reduced motion did not mint the static landing (the flash and the colours)"
        );
        let mut dust = sky_around(5);
        m.sow_into(&mut dust, &land);
        // The landing's own layers, drawn alone: the static train and head
        // beside them keep their own linear fades under reduced motion, so a
        // whole-frame comparison would read the train's spend as the
        // landing's motion.
        let l = m.landings[0];
        let snapshot = |sc: &mut Scratch, after: u64| {
            sc.clear();
            let at = ctx_at(t0 + spawn.t_flight + ms(after), &cfg, (5, 80), 0.25);
            let mut fr = sc.frame();
            draw_burst(&l, &at, &mut fr);
            draw_flash(&l, &at, &mut fr);
            draw_sparks(&l, &at, &mut fr, SPARK_HALO_CAP);
            (sc.under.clone(), sc.out.clone(), sc.halos.clone())
        };
        let a = snapshot(&mut sc, 100);
        let b = snapshot(&mut sc, 300);
        assert!(!a.1.is_empty(), "the static landing drew nothing");
        assert!(
            a.0 == b.0 && a.1 == b.1 && a.2 == b.2,
            "the reduced-motion landing MOVED between T + 100 and T + 300"
        );
        assert!(
            a.2.iter().all(|h| !is_chromatic(h.color)),
            "a spark's halo (motion) was drawn under reduced motion"
        );
        sc.clear();
        {
            let at = ctx_at(t0 + spawn.t_flight + ms(100), &cfg, (5, 80), 0.25);
            let mut fr = sc.frame();
            draw_pin(&l, &at, &mut fr);
        }
        assert!(
            sc.out.is_empty() && sc.halos.is_empty(),
            "the pin (motion) was drawn under reduced motion"
        );
        // The flash: cell-sized white and coloured quads on the landing
        // row, held; the bands in the sky, at full reach from the first.
        let g = geom();
        let cell = |q: &GlowQuad| usize::from(q.w) == g.cw && usize::from(q.h) == g.ch;
        assert!(
            a.1.iter().any(|q| cell(q) && is_white(q.color))
                && a.1.iter().any(|q| cell(q) && is_chromatic(q.color)),
            "the static flash is not white and colour"
        );
        let ch = g.ch as f32;
        assert!(
            a.1.iter()
                .any(|q| f32::from(q.y) + f32::from(q.h) <= ly - 0.5 * ch)
                && a.1.iter().any(|q| f32::from(q.y) >= ly + 0.5 * ch),
            "the static splash is not in the sky above and below the row"
        );
        let fan = lane(&dust, StarLane::Fan);
        assert!(!fan.is_empty(), "reduced motion dropped the (static) fan");
        let later = land.now + ms(400);
        for s in &fan {
            let birth = (s.x.round() as i32, s.y.round() as i32);
            assert_eq!(
                s.pos(later, land.cfg.reduced_motion),
                birth,
                "the reduced-motion fan is thrown, not static"
            );
        }
        assert!(
            fan.iter()
                .any(|s| s.pos(later, false) != (s.x.round() as i32, s.y.round() as i32)),
            "the oracle is vacuous: the same stars would not move under motion either"
        );
    }

    /// §6.12 / D17: a 2-7-cell same-row hop is a mini-fan, not a flight: one
    /// m2 and two m3 in the mini-fan lane at the landing, no train, no burst.
    #[test]
    fn a_word_hop_is_a_mini_fan_not_a_flight() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 25), 0.25);
        assert!(
            m.on_event(&mv((5, 20), (5, 25)), t0, &ctx).is_none(),
            "a 5-cell hop must not mint a flight"
        );
        assert_eq!(m.live(), 0, "a 5-cell hop put a meteor in the air");

        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        assert!(sc.under.is_empty(), "a mini-fan drew a train");
        assert!(m.landings.is_empty(), "a mini-fan rang");

        let mut dust = sky();
        m.sow_into(&mut dust, &ctx);
        let stars = lane(&dust, StarLane::MiniFan);
        let m2 = stars.iter().filter(|s| s.class == StarClass::M2).count();
        let m3 = stars.iter().filter(|s| s.class == StarClass::M3).count();
        assert_eq!((m2, m3), (1, 2), "the mini-fan is 1 m2 + 2 m3 (D17)");
        assert_eq!(dust.live(), 3, "a mini-fan star escaped its lane");
        let (lx, ly) = geom().cell_center(5, 25);
        for s in &stars {
            assert!(
                (s.x - lx).abs() <= 2.0 && (s.y - ly).abs() <= 2.0,
                "a mini-fan star was born away from the landing cell"
            );
        }

        // Scrubbing stays calm: one cell mints nothing at all.
        let mut dust = sky();
        assert!(m.on_event(&mv((5, 25), (5, 26)), t0, &ctx).is_none());
        sc.emit(&mut m, &ctx);
        m.sow_into(&mut dust, &ctx);
        assert_eq!(dust.live(), 0, "a one-cell scrub minted light");
    }

    /// §5.4 / the vertical reach law on row 0: row 0's sky is the `Geom.head`
    /// band the host opened for effects. A fan landing on row 0 may rise into
    /// the band, but no star rests above `fx_top()` (the tab strip) or above
    /// the 1.6 ch ceiling. Under a 40 px band a 40-cell landing puts at least
    /// one star in the band; with no band every star rests inside the grid,
    /// re-aimed rather than dropped, so the census holds.
    #[test]
    fn a_row_zero_fan_may_use_the_head_band_but_never_the_tab_strip_beyond_it() {
        let cfg = config();
        let t0 = Instant::now();
        let landing = (0_u16, 40_u16);
        for head in [40_u16, 0] {
            let at = |now: Instant| {
                let mut c = ctx_at(now, &cfg, landing, 0.25);
                c.geom.head = head;
                c.geom.origin_y = head;
                c
            };
            let mut m = Meteors::new();
            let spawn = m.on_event(&mv((0, 0), landing), t0, &at(t0)).expect("fly");
            let land = at(t0 + spawn.t_flight);
            let mut sc = Scratch::default();
            sc.emit(&mut m, &land);
            let mut dust = sky();
            m.sow_into(&mut dust, &land);
            let fan = lane(&dust, StarLane::Fan);
            assert_eq!(
                fan.iter()
                    .filter(|s| s.class == StarClass::M1 && s.gold)
                    .count(),
                1,
                "head {head}: the gold hero"
            );
            assert_eq!(
                fan.iter().filter(|s| s.class == StarClass::M2).count(),
                4,
                "head {head}: its four m2"
            );
            let g = land.geom;
            let (top, grid_top) = (g.fx_top(), i32::from(g.origin_y));
            let cap = (FAN_RISE_MAX_CH * g.ch as f32).round() as i32;
            let (_, cy) = g.cell_center(0, 40);
            let cy = cy.round() as i32;
            let mut in_band = 0;
            for s in &fan {
                let (rx, ry) = s.rest();
                assert!(
                    ry >= top,
                    "head {head}: a {:?} fan star rests at ({rx}, {ry}), above the effects \
                     box's top {top} — in the tab strip",
                    s.class
                );
                let rise = (ry - cy).abs();
                assert!(
                    rise <= cap,
                    "head {head}: a {:?} fan star rests {rise} px off row 0's line at \
                     ({rx}, {ry}) — the cap is {cap} px (1.6 ch) on row 0 too",
                    s.class
                );
                in_band += usize::from(ry < grid_top);
            }
            if head > 0 {
                assert!(in_band >= 1, "head {head}: no fan star used the head band");
            } else {
                assert_eq!(in_band, 0, "head 0: a fan star rests above the grid");
            }
        }
    }

    /// §5.4 / L4 at the seam: every star the meteor decides is built by the
    /// sky through its clearance, which applies where a core comes to rest,
    /// not where it is born. A fan is born on the ledger-exempt caret cell, so
    /// a Home/End onto a glyph still throws its hero and four m2 (D6), and a
    /// core whose rest would be on a probed glyph is nudged ≤ 2 px or dropped.
    /// Fed a glyph at the landing, a glyph beside it, a row of ink, and no
    /// probe at all (free transients fly over unprobed rows); the mini-fan
    /// follows the same law, so Alt-B onto a word's first glyph still winks.
    #[test]
    fn a_fan_star_core_never_lands_on_a_probed_glyph() {
        let cfg = config();
        let t0 = Instant::now();
        let landing = (5_u16, 40_u16);
        let g = geom();
        let window = StarLane::Fan.throw_ms().expect("the fan is a throw");
        // One landing into `dust`: every fan core's rest cell is asserted
        // clear, and the census is returned.
        let census = |dust: &mut Stardust| -> usize {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, landing, 0.25);
            let spawn = m.on_event(&mv((5, 0), landing), t0, &ctx).expect("fly");
            let arrival = t0 + spawn.t_flight;
            let land = ctx_at(arrival, &cfg, landing, 0.25);
            let mut sc = Scratch::default();
            sc.emit(&mut m, &land);
            m.sow_into(dust, &land);
            let done = arrival + Duration::from_secs_f32(window / 1000.0) + ms(1);
            for s in lane(dust, StarLane::Fan) {
                let (rx, ry) = s.pos(done, false);
                assert_ne!(
                    dust.probe().at_px(rx, ry, g),
                    Some(true),
                    "a {:?} fan core came to rest on a probed glyph at ({rx}, {ry})",
                    s.class
                );
            }
            assert!(!m.landings.is_empty(), "the pin and ring need no clearance");
            lane(dust, StarLane::Fan).len()
        };
        let probed = |ink: &[bool; 120]| {
            let mut dust = Stardust::new();
            dust.probe_mut().probe_row(i32::from(landing.0), ink);
            dust
        };

        // A glyph AT the landing cell: the birth pixel is the caret's own.
        let mut inked = [false; 120];
        inked[usize::from(landing.1)] = true;
        let on_ink = census(&mut probed(&inked));
        assert!(
            on_ink >= FAN_HERO_N,
            "a glyph AT the landing cut the fan to {on_ink} stars — the hero and its four m2 \
             are gone, and the five rain glints have nothing to pair with (D6)"
        );

        // A glyph beside it.
        let mut beside = [false; 120];
        beside[usize::from(landing.1) + 1] = true;
        let full = census(&mut probed(&beside));
        assert!(
            full >= FAN_HERO_N,
            "a glyph BESIDE the landing thinned the fan"
        );
        assert_eq!(on_ink, full, "ink under the caret thinned the fan");

        // A row of ink around the landing: every core that would come to rest
        // on it is nudged or dropped, and the rest of the fan is still thrown.
        let mut row = [true; 120];
        row[usize::from(landing.1)] = false;
        let boxed = census(&mut probed(&row));
        assert!(
            boxed >= 1 && boxed <= full,
            "a row of ink around the landing left {boxed} of {full} stars"
        );

        // No probe at all: open air.
        let mut dust = Stardust::new();
        assert!(dust.probe().is_empty());
        assert_eq!(
            census(&mut dust),
            full,
            "an unprobed row refused a free transient"
        );

        // The mini-fan obeys the same law: a hop onto a glyph still winks.
        let mut dust = probed(&inked);
        let mut m = Meteors::new();
        let hop = ctx_at(t0, &cfg, landing, 0.25);
        assert!(m.on_event(&mv((5, 45), landing), t0, &hop).is_none());
        let mut sc = Scratch::default();
        sc.emit(&mut m, &hop);
        m.sow_into(&mut dust, &hop);
        assert_eq!(
            lane(&dust, StarLane::MiniFan).len(),
            3,
            "Alt-B onto a word's first glyph minted no mini-fan (§6.12)"
        );
    }

    /// Seam point 12, the band path: a flight is a span and a landing a point
    /// under the band's pixel law. Inside: a same-row flight on row 5 under a
    /// band `[3..10]` sliding down one row rides to row 6 whole, and its
    /// landing is minted at the moved endpoint; the band `[6..10]` then
    /// archiving up carries the landing out through the top edge. Outside: a
    /// band below the flight leaves it untouched. Straddle: a vertical recall
    /// from row 9 to row 3 under a band `[5..20]` has its tail inside and head
    /// outside, so the train is dropped and mints no landing.
    #[test]
    fn a_band_move_carries_a_flight_inside_the_band_and_retires_one_that_straddles_it() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        let ch = g.ch as u16;

        // Inside.
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("flies");
        let (_, y5) = g.cell_center(5, 40);
        assert!(
            (m.live[0].y1 - y5).abs() < 0.5,
            "fixture: the head is on row 5"
        );
        m.translate_band(3, 10, 1, ch, 0);
        assert_eq!(m.live.len(), 1, "a train wholly inside the band is kept");
        let (wx, wy) = g.cell_center(6, 40);
        assert!(
            (m.live[0].y0 - wy).abs() < 0.5 && (m.live[0].y1 - wy).abs() < 0.5,
            "the train rode one row down with its text: y0 {} y1 {} want {wy}",
            m.live[0].y0,
            m.live[0].y1
        );
        let mut sc = Scratch::default();
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (6, 40), 0.25);
        sc.emit(&mut m, &land);
        let l = m
            .landings
            .first()
            .copied()
            .expect("the landing is minted at T");
        assert!(
            (l.x - wx).abs() < 0.5 && (l.y - wy).abs() < 0.5,
            "the landing was minted at ({}, {}), not at the moved endpoint ({wx}, {wy})",
            l.x,
            l.y
        );
        m.translate_band(6, 10, -1, ch, 0);
        assert!(
            m.landings.is_empty(),
            "a landing carried past the band's edge is gone, not parked on it"
        );

        // Outside.
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("flies");
        let (y0, y1) = (m.live[0].y0, m.live[0].y1);
        m.translate_band(8, 20, 1, ch, 0);
        assert_eq!(m.live.len(), 1);
        assert_eq!(
            (m.live[0].y0, m.live[0].y1),
            (y0, y1),
            "a band below the flight does not touch it"
        );

        // Straddle.
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (3, 40), 0.25);
        let spawn = m.on_event(&mv((9, 40), (3, 40)), t0, &ctx).expect("flies");
        assert_eq!(m.live.len(), 1);
        m.translate_band(5, 20, 1, ch, 0);
        assert!(
            m.live.is_empty(),
            "a train straddling the band's edge is dropped, never bent to fit"
        );
        let mut sc = Scratch::default();
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (3, 40), 0.25);
        sc.emit(&mut m, &land);
        assert!(m.landings.is_empty(), "a dropped flight mints no landing");

        // No geometry yet: nothing honest can be kept.
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("flies");
        m.translate_band(3, 10, 1, 0, 0);
        assert!(
            m.live.is_empty(),
            "with no cell height the pools fail closed"
        );
    }

    #[test]
    fn a_scroll_between_spawn_and_arrival_moves_the_landing_with_the_train() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        let rows = 2_u16;
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("flies");
        m.translate_scroll(rows, g.ch as u16);
        let (wx, wy) = g.cell_center(5 - rows, 40);
        let arrival = t0 + spawn.t_flight;
        let land = ctx_at(arrival, &cfg, (5 - rows, 40), 0.25);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &land);
        let l = m
            .landings
            .first()
            .copied()
            .expect("the landing is minted at T");
        assert!(
            (l.x - wx).abs() < 0.5 && (l.y - wy).abs() < 0.5,
            "the landing was minted at ({}, {}), not at the scrolled endpoint ({wx}, {wy})",
            l.x,
            l.y
        );
        assert_eq!(
            (l.x, l.y),
            (m.live[0].x1, m.live[0].y1),
            "the landing is the train's end"
        );
        // The pin's 2×2 heart is on the scrolled row …
        let (cx, cy) = (wx.round() as i32, wy.round() as i32);
        assert!(
            sc.out.iter().any(|q| is_white(q.color)
                && q.w == 2
                && q.h == 2
                && i32::from(q.x) == cx - 1
                && i32::from(q.y) == cy - 1),
            "the pin's heart is not on the scrolled landing"
        );
        // … and so is every fan star's birth.
        let mut dust = sky();
        m.sow_into(&mut dust, &land);
        let fan = lane(&dust, StarLane::Fan);
        assert!(fan.len() >= FAN_HERO_N);
        for s in &fan {
            assert!(
                (s.x - wx).abs() <= 2.0 && (s.y - wy).abs() <= 2.0,
                "a fan star was born at ({}, {}), not on the scrolled landing ({wx}, {wy})",
                s.x,
                s.y
            );
        }

        // Off the glass: an upward recall whose landing — and farthest
        // stations — the scroll carries above the window, launch still on it.
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (3, 40), 0.25);
        let spawn = m.on_event(&mv((9, 40), (3, 40)), t0, &ctx).expect("flies");
        m.translate_scroll(7, g.ch as u16);
        assert_eq!(
            m.live.len(),
            1,
            "a train whose launch is still on glass is kept"
        );
        assert!(m.live[0].y1 < 0.0 && m.live[0].y0 >= 0.0);
        let mut dust = sky();
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (0, 40), 0.25);
        sc.emit(&mut m, &land);
        m.sow_into(&mut dust, &land);
        assert!(m.live[0].landed, "the arrival edge is still spent");
        assert!(
            m.landings.is_empty(),
            "a landing above the glass was minted"
        );
        assert!(
            lane(&dust, StarLane::Fan).is_empty(),
            "a fan was sown above the glass"
        );
        assert!(
            !lane(&dust, StarLane::Shed).is_empty(),
            "the stations still on glass were not sown"
        );
        assert!(
            dust.live_iter().all(|s| s.y >= 0.0),
            "a fragment was sown above the glass"
        );
    }

    // -- the starburst -------------------------------------------------------

    /// A large retina cell in device px (`font_px 32`: `cw 20`, `ch 38`). The
    /// rest of this module runs at `cw 9`, `ch 18`; the burst is measured at
    /// both because its content is a screen-space shape.
    fn owner_geom() -> Geom {
        Geom {
            cw: 20,
            ch: 38,
            rows: 24,
            cols: 80,
            origin_x: 0,
            origin_y: 0,
            win_w: 1600,
            win_h: 912,
            head: 0,
        }
    }

    fn ctx_on(now: Instant, cfg: &Config, caret: (u16, u16), g: Geom) -> Ctx<'_> {
        let mut c = ctx_at(now, cfg, caret, 0.25);
        c.geom = g;
        c
    }

    /// A landing minted from a `cells`-cell same-row jump, with the sky's
    /// probe latched over `blank` rows, drawn by whichever mark the caller
    /// hands back.
    fn landed(cells: u16, cfg: &Config, g: Geom, blank: bool) -> (Meteors, Landing, Instant) {
        landed_with(cells, cfg, g, blank, 0.25, false)
    }

    /// [`landed`], with the field the caret latched at the spawn edge (D4's
    /// raw `t_land`) and the flight's direction chosen: rightward flies from
    /// column 0 and lands on column `cells`; leftward (a Home-ward hop)
    /// flies from column `cells + 20` and lands on column 20, so the glass
    /// has room for a jet on both sides.
    fn landed_with(
        cells: u16,
        cfg: &Config,
        g: Geom,
        blank: bool,
        t_land: f32,
        leftward: bool,
    ) -> (Meteors, Landing, Instant) {
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let (from, to) = if leftward {
            ((5_u16, cells + 20), (5_u16, 20))
        } else {
            ((5_u16, 0), (5_u16, cells))
        };
        let mut ctx = ctx_on(t0, cfg, to, g);
        ctx.caret_t = t_land;
        let spawn = m.on_event(&mv(from, to), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        let at = ctx_on(arrival, cfg, to, g);
        {
            let mut fr = sc.frame();
            m.emit(&at, &mut fr);
        }
        let mut dust = Stardust::new();
        for r in 4..=6 {
            dust.probe_mut().probe_row(r, &[!blank; 120]);
        }
        m.sow_into(&mut dust, &at);
        let l = m.landings[0];
        (m, l, arrival)
    }

    /// Draw one landing's burst, by itself, at `after` ms.
    fn burst_only(
        sc: &mut Scratch,
        l: &Landing,
        cfg: &Config,
        g: Geom,
        arrival: Instant,
        after: u64,
    ) {
        sc.clear();
        let at = ctx_on(arrival + ms(after), cfg, (5, 40), g);
        let mut fr = sc.frame();
        draw_burst(l, &at, &mut fr);
    }

    /// §6.9: a retired train's colour reaches the chroma cull at exactly
    /// `R = 60 ms`; pins the [`RETIRE_DECAY`] literal (`ln` is not `const`)
    /// to [`CHROMA_CULL_ALPHA`].
    #[test]
    fn the_retire_decay_lands_on_the_chroma_cull() {
        let at_r = (-RETIRE_DECAY).exp();
        assert!(
            (at_r - CHROMA_CULL_ALPHA).abs() < 1e-3,
            "a retired train is at {at_r} after R, not the cull {CHROMA_CULL_ALPHA}"
        );
    }

    /// [`BURST_ROOT_SEP_AT_MAX_N`] is a literal because `sin` is not `const`;
    /// this checks it against the real trigonometry, which the const asserts
    /// on the root law rely on.
    #[test]
    fn a_spike_root_is_derived_from_the_spacing_it_must_keep_open() {
        let want = 2.0 * (std::f32::consts::PI / BURST_N_MAX as f32).sin();
        assert!(
            (BURST_ROOT_SEP_AT_MAX_N - want).abs() < 1e-6,
            "the separation coefficient is {BURST_ROOT_SEP_AT_MAX_N}, not 2 sin(pi/12) = {want}"
        );
    }

    // ---- the landing's palette ---------------------------------------------

    /// A jet breaks over a glyph and the star survives: over a full row of
    /// text the mark degrades to the bare core star, and nothing the burst
    /// emits over an inked cell exceeds the transient ceiling (L3).
    #[test]
    fn a_jet_breaks_over_a_glyph_and_the_star_survives() {
        let cfg = config();
        let g = owner_geom();
        let mut span = [0.0_f32; 2];
        let mut core = [0_usize; 2];
        for (i, blank) in [true, false].into_iter().enumerate() {
            let (_m, l, arrival) = landed(40, &cfg, g, blank);
            let b = l.burst.expect("a 40-cell nav landing bursts");
            let mut sc = Scratch::default();
            burst_only(&mut sc, &l, &cfg, g, arrival, (b.ms * 0.9) as u64);
            for q in &sc.out {
                let x = f32::from(q.x) + f32::from(q.w) * 0.5;
                span[i] = span[i].max((x - l.x).abs());
                let (r, gg, bb) = chan(q.color);
                assert!(
                    f32::from(r.max(gg).max(bb) as u8) <= TRANSIENT_STAR_COV_CEIL + 1.0,
                    "a burst quad asks {} — over the transient ceiling",
                    r.max(gg).max(bb)
                );
                if (x - l.x).abs() <= BURST_CORE_CH * g.ch as f32 {
                    core[i] += 1;
                }
            }
        }
        assert!(
            span[1] < BURST_CORE_CH * g.ch as f32 + 2.0,
            "over a full row of text the burst still reaches {} px — the jets did not break",
            span[1]
        );
        assert!(
            span[0] > 2.0 * BURST_CORE_CH * g.ch as f32,
            "over blank cells the jets only reached {} px",
            span[0]
        );
        assert!(
            core[1] * 4 >= core[0] * 3,
            "the core star lost {} of its {} quads to the gate — it must not be gated at all",
            core[0] - core[1],
            core[0]
        );
    }

    /// [`BURST_QUAD_CAP`] is enforced and not reached in normal service:
    /// measured on the real emitter at every grade and both cells, over the
    /// whole life, with the jets ungated (their worst case).
    #[test]
    fn a_burst_stays_inside_its_quad_cap_at_every_grade() {
        let cfg = config();
        // `9x18`, `20x38` and `40x76`: the cost must be a function of the
        // shape, not of pixel density ([`BURST_STEP_CH_SHARE`]).
        let big = Geom {
            cw: 40,
            ch: 76,
            ..owner_geom()
        };
        for g in [owner_geom(), geom(), big] {
            for cells in [8_u16, 19, 40, 60] {
                let (_m, l, arrival) = landed(cells, &cfg, g, true);
                let b = l.burst.expect("a nav landing bursts");
                let mut sc = Scratch::default();
                let mut peak = 0_usize;
                let mut after = 0_u64;
                while after <= b.ms as u64 {
                    burst_only(&mut sc, &l, &cfg, g, arrival, after);
                    peak = peak.max(sc.out.len());
                    assert!(
                        sc.out.len() <= BURST_QUAD_CAP,
                        "cw{} ch{} {cells} cells at T+{after}: {} quads, over the cap",
                        g.cw,
                        g.ch,
                        sc.out.len()
                    );
                    after += 8;
                }
                assert!(peak > 0, "the burst never drew");
                assert!(sc.under.is_empty(), "the burst moved light UNDER the ink");
                assert!(sc.halos.is_empty(), "the burst added a halo");
            }
        }
    }

    /// Everything is off glass by 600 ms: the pixels agree with the const
    /// assert that [`ring_ms`] reaches [`FLIGHT_OFF_GLASS_MS`] exactly at the
    /// cap.
    #[test]
    fn the_burst_is_off_glass_by_the_flight_horizon() {
        let cfg = config();
        let g = owner_geom();
        for cells in [8_u16, 40, 60] {
            let (_m, l, arrival) = landed(cells, &cfg, g, true);
            let mut sc = Scratch::default();
            burst_only(&mut sc, &l, &cfg, g, arrival, FLIGHT_OFF_GLASS_MS as u64);
            assert!(
                sc.out.is_empty(),
                "{cells} cells: {} burst quads still on glass at the horizon",
                sc.out.len()
            );
        }
    }

    /// §6.10: on a light page the burst is source-over ink, never additive
    /// light (which would be invisible there), under the light alpha cap.
    #[test]
    fn the_light_burst_is_ink_not_additive_light() {
        let mut cfg = config();
        cfg.dark_theme = false;
        let g = owner_geom();
        let (_m, l, arrival) = landed(40, &cfg, g, true);
        let b = l.burst.expect("a 40-cell nav landing bursts");
        let mut sc = Scratch::default();
        burst_only(&mut sc, &l, &cfg, g, arrival, (b.ms * 0.59) as u64);
        assert!(!sc.out.is_empty(), "the light burst drew nothing");
        for q in &sc.out {
            assert!(q.alpha > 0, "an additive quad on a light page");
            assert!(
                f32::from(q.alpha) <= LIGHT_ALPHA_CAP + 1.0,
                "a light burst quad at alpha {}",
                q.alpha
            );
        }
    }

    /// Reduced motion keeps a static star (§6.11): the full form at full reach,
    /// held, then the theme's one linear fade.
    #[test]
    fn the_reduced_motion_burst_is_a_still_star() {
        let mut cfg = config();
        cfg.reduced_motion = true;
        let g = owner_geom();
        let (_m, l, arrival) = landed(40, &cfg, g, true);
        let mut sc = Scratch::default();
        let mut spans = Vec::new();
        for after in [0_u64, 120, 300] {
            burst_only(&mut sc, &l, &cfg, g, arrival, after);
            let mut r = 0.0_f32;
            for q in &sc.out {
                let dx = f32::from(q.x) + f32::from(q.w) * 0.5 - l.x;
                let dy = f32::from(q.y) + f32::from(q.h) * 0.5 - l.y;
                r = r.max((dx * dx + dy * dy).sqrt());
            }
            spans.push(r);
        }
        assert!(
            (spans[0] - spans[2]).abs() < 2.0,
            "the static star changed size: {spans:?}"
        );
        burst_only(&mut sc, &l, &cfg, g, arrival, FLIGHT_OFF_GLASS_MS as u64);
        assert!(sc.out.is_empty(), "the static star outlived the horizon");
    }

    /// A jet is probed over its whole capped reach: [`SkyMask::row`] spans
    /// [`JET_PROBE_CELLS`] each side, and the jets' `6.0 ch` covers a number
    /// of cells that depends on the aspect; at the tuned aspects the gate
    /// never clips its own jet.
    #[test]
    fn a_jet_is_probed_over_its_whole_capped_reach() {
        for g in [owner_geom(), geom()] {
            let cells = JET_LEN_SHARE[0]
                * (RING_R_CH + RING_R_PER_IMPACT_CH * (super::super::timing::IMPACT_MAX - 1.0))
                * (g.ch as f32 / g.cw as f32);
            assert!(
                cells <= JET_PROBE_CELLS as f32,
                "cw{} ch{}: the capped jet reaches {cells:.1} cells, past the {} the probe covers",
                g.cw,
                g.ch,
                JET_PROBE_CELLS
            );
        }
    }

    /// The rise law holds for the painted mark, not its centreline:
    /// `comet_beam` treats its polyline as a centreline and adds
    /// `thickness · 0.5` either side. The burst asserts the painted envelope
    /// at compile time (`BURST_CORE_CH + 0.5·BURST_SEC_THICK_CH[0] +
    /// BURST_AA_SUPPORT_CH`), clamps it in device px at emit
    /// ([`burst_rise_limit`]), and this reads every quad of the burst over its
    /// whole life at four cell heights, one small enough for the px clamp to
    /// bite.
    #[test]
    fn the_burst_paints_inside_the_fan_s_rise_at_every_cell() {
        let cfg = config();
        let mut worst = 0.0_f32;
        for ch in [12_usize, 18, 38, 76] {
            let g = Geom {
                cw: (ch as f32 * 0.52).round() as usize,
                ch,
                rows: 24,
                cols: 100,
                origin_x: 0,
                origin_y: 0,
                win_w: 2000,
                win_h: 2000,
                head: 0,
            };
            for cells in [8_u16, 40, 60] {
                let (_m, l, arrival) = landed(cells, &cfg, g, true);
                let b = l.burst.expect("a nav landing bursts");
                let mut sc = Scratch::default();
                let mut rise = 0.0_f32;
                let mut after = 0_u64;
                while after <= b.ms as u64 + 8 {
                    burst_only(&mut sc, &l, &cfg, g, arrival, after);
                    for q in &sc.out {
                        let top = f32::from(q.y);
                        let bot = f32::from(q.y) + f32::from(q.h);
                        rise = rise.max((l.y - top).max(bot - l.y));
                    }
                    after += 4;
                }
                let cap = FAN_RISE_MAX_CH * ch as f32;
                let in_ch = rise / ch as f32;
                worst = worst.max(in_ch);
                assert!(
                    rise <= cap,
                    "ch {ch}, {cells} cells: the burst PAINTS to {in_ch:.3} ch, past the fan's \
                     rise of {FAN_RISE_MAX_CH} ch — the shipped ring painted to 2.10 ch under an \
                     assert that said 1.5"
                );
            }
        }
        // Not merely under the cap: tighter than the mark it replaced, which
        // is the design's own claim about it.
        assert!(
            worst < 2.10,
            "the burst paints to {worst:.3} ch — no tighter than the ring's own 2.10 ch"
        );
    }

    // -- T6, idle → zero ----------------------------------------------------

    /// **T6.** An idle pool writes nothing, is at rest, and names no
    /// deadline — the first half of "idle → zero work", and the reason the
    /// host can return to 0 % CPU.
    #[test]
    fn an_idle_meteor_pool_draws_nothing_and_names_no_deadline() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let mut sc = Scratch::default();
        for k in 0..8_u64 {
            let ctx = ctx_at(t0 + ms(k * 16), &cfg, (5, 10), 0.25);
            sc.emit(&mut m, &ctx);
            assert!(sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty());
        }
        assert!(m.at_rest());
        assert_eq!(m.live(), 0);
        assert!(m.next_change_deadline(t0).is_none());

        // And a typed echo is not a flight: T1 lives at the seam, and the one
        // move class it refuses here is the ink wrap.
        let ctx = ctx_at(t0, &cfg, (5, 0), 0.25);
        let wrap = Event::Move {
            from: (5, 119),
            to: (6, 0),
            licence: Licence::Typed,
            dir: Dir::Down,
        };
        assert!(
            m.on_event(&wrap, t0, &ctx).is_none(),
            "a typed wrap flew — `ink_wrap_rainbow_keeps_the_ribbon_no_zoom`"
        );
        assert!(m.at_rest(), "a typed wrap left something in the air");
    }

    // -- §18, the cadence law over a whole life ------------------------------

    /// The attack asks for every frame; the release rides the host's train.
    /// Over a whole flight and impact (8, 40 and 80 cells, both directions,
    /// and an Enter's small landing), emitted every 8 ms:
    ///
    /// * through the pin's close (`T + 150`) the pool is brisk and names the
    ///   next frame exactly (`FRAME_CADENCE`);
    /// * while the impact still moves (burst, sparks, the train's root) it is
    ///   still brisk but names nothing sooner than `TAIL_FLOOR`, so a host
    ///   folding the deadline under its lane tick wakes once per tick;
    /// * once the last spark is under its cull it is not brisk and names only
    ///   the pool's horizon, at the floor or later;
    /// * at `T + 600` it is at rest and names nothing (T6).
    #[test]
    fn the_attack_asks_for_every_frame_and_the_release_rides_the_host_s_train() {
        use crate::rainbow_kitty::{FRAME_CADENCE, TAIL_FLOOR};
        let cfg = config();
        let t0 = Instant::now();
        let close = Duration::from_secs_f32(PIN_MS / 1000.0);
        let root_suck = Duration::from_secs_f32(ROOT_SUCK_MS / 1000.0);
        let flights = [
            ((5, 0), (5, 8), Licence::Nav),
            ((5, 0), (5, 40), Licence::Nav),
            ((5, 80), (5, 0), Licence::Nav),
            ((5, 40), (5, 80), Licence::Nav),
            ((5, 20), (6, 0), Licence::Return),
        ];
        for (from, to, licence) in flights {
            let name = format!("{from:?} → {to:?} {licence:?}");
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            let spawn = m
                .on_event(&mv_as(from, to, licence), t0, &ctx)
                .expect("fly");
            let arrival = t0 + spawn.t_flight;
            let end = m.live[0].end();
            let mut sc = Scratch::default();
            let mut moving_until: Option<Instant> = None;
            let mut sub_floor = 0_u32;
            let mut release_wakes = 0_u32;
            let mut t = t0;
            while t < end + ms(50) {
                sc.emit(&mut m, &ctx_at(t, &cfg, to, 0.25));
                if moving_until.is_none()
                    && let Some(l) = m.landings.first()
                {
                    moving_until = Some(l.moving_until().max(arrival + root_suck));
                }
                let brisk = m.brisk(t);
                let due = m.next_change_deadline(t);
                let after = t.saturating_duration_since(arrival).as_millis();
                if let Some(d) = due
                    && d < t + TAIL_FLOOR
                {
                    sub_floor += 1;
                }
                if t < arrival + close {
                    assert!(brisk, "{name}: not brisk at T + {after} ms, in the attack");
                    assert_eq!(
                        due,
                        Some(t + FRAME_CADENCE),
                        "{name}: the attack did not ask for the next frame at T + {after} ms"
                    );
                } else if moving_until.is_some_and(|mu| t < mu) {
                    release_wakes += 1;
                    assert!(brisk, "{name}: the release is not brisk at T + {after} ms");
                    assert!(
                        due.is_some_and(|d| d >= t + TAIL_FLOOR),
                        "{name}: the release named {:?} at T + {after} ms — under the floor",
                        due.map(|d| d.saturating_duration_since(t))
                    );
                } else if t < end {
                    assert!(
                        !brisk,
                        "{name}: brisk at T + {after} ms with nothing moving"
                    );
                    assert!(
                        sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty(),
                        "{name}: the meteor drew at T + {after} ms, past its last motion"
                    );
                    assert!(
                        due.is_some_and(|d| d >= t + TAIL_FLOOR),
                        "{name}: the horizon named {:?} at T + {after} ms",
                        due.map(|d| d.saturating_duration_since(t))
                    );
                } else {
                    assert!(m.at_rest(), "{name}: not at rest at T + {after} ms");
                    assert!(due.is_none(), "{name}: a resting pool named a deadline");
                }
                t += ms(8);
            }
            let mu = moving_until.expect("the landing was minted");
            let release = mu.saturating_duration_since(arrival + close);
            let floor = if licence == Licence::Return {
                root_suck - close
            } else {
                Duration::from_secs_f32(RING_MS / 1000.0) - close
            };
            assert!(
                release >= floor && release_wakes > 0,
                "{name}: the release is {release:?} ({release_wakes} wakes), less than {floor:?}"
            );
            let attack = (spawn.t_flight + close).as_secs_f32() * 1000.0 / 8.0;
            assert!(
                (sub_floor as f32 - attack).abs() <= 1.0,
                "{name}: {sub_floor} sub-floor deadlines over the life; the attack is {attack:.0} \
                 frames"
            );
        }
    }

    /// A spark is culled exactly where the cadence law says: a millisecond
    /// before [`spark_cull_u`] [`spark_at`] still draws it, a millisecond after
    /// it does not, for every spark of a 40-cell landing. So
    /// [`Landing::moving_until`] is never early and never late beyond rounding.
    #[test]
    fn a_spark_is_culled_exactly_where_the_cadence_law_says_it_is() {
        let ch = geom().ch as f32;
        let cull = spark_cull_u();
        let seed = mix32(0xBEEF ^ 0xFA5E);
        for k in 0..SPARK_N_MAX {
            let s = mint_spark(seed, k, ch);
            let at = s.life * cull;
            assert!(
                spark_at(s, at - 1.0, ch).is_some(),
                "spark {k}: already gone 1 ms before its cull at {at:.1} ms"
            );
            assert!(
                spark_at(s, at + 1.0, ch).is_none(),
                "spark {k}: still drawn 1 ms after its cull at {at:.1} ms"
            );
        }
    }

    // -- the impact: flash and sparks (owner, 2026-09-08) ----------------------

    // -- the landing, measured on composited light --------------------------

    /// §27: a party landing is a fan and nothing else (`n` stars ROYGBIV from
    /// the caret, no sparks, no gold hero, the burst only when asked), born on
    /// the frame it is staged and off the glass within 315 ms, so one fan per
    /// 1.6 s bar never stacks and `LANDING_POOL` holds every ring bar. A party
    /// at a caret off the glass mints nothing.
    #[test]
    fn a_party_landing_is_a_fan_off_the_glass_inside_315_ms() {
        let t0 = Instant::now();
        let cfg = config();
        let mut m = Meteors::new();
        let mut sc = Scratch::default();
        let at = ctx_at(t0, &cfg, (5, 40), 0.3);

        // A plain bar: the fan is staged for the sky, no landing is pooled.
        m.party(t0, 5, false, &at);
        assert_eq!(m.sown.len(), 1, "the fan is staged for the sky");
        assert!(m.landings.is_empty(), "no ring bar, no landing in the pool");
        // The fourth bar: the pin and starburst, without an impact flash.
        m.party(t0, 11, true, &at);
        assert_eq!(m.landings.len(), 1);
        let l = m.landings[0];
        assert!(l.burst.is_some(), "the fourth bar carries the starburst");
        assert!(
            !l.flash && l.sparks == 0,
            "a bar line is a beat, not an impact"
        );
        let mut flash_only = Scratch::default();
        draw_flash(&l, &at, &mut flash_only.frame());
        assert!(
            flash_only.out.is_empty(),
            "the celebration emits no flash quads"
        );
        assert!(!l.fan.hero, "no gold hero: the party is the spectrum");
        assert_eq!(l.fan.n, 11);
        let (cx, cy) = at.geom.cell_center(5, 40);
        assert!(
            (l.x - cx).abs() < 1e-3 && (l.y - cy).abs() < 1e-3,
            "minted at the caret"
        );

        // Born on this frame: emit, hand to the sky, count the stars.
        sc.emit(&mut m, &at);
        let mut dust = sky_around(5);
        m.sow_into(&mut dust, &at);
        assert_eq!(
            dust.live(),
            16,
            "5 + 11 stars, all admitted on a dark theme"
        );
        let fan = lane(&dust, StarLane::Fan);
        assert_eq!(
            fan.len(),
            16,
            "every party star is a fan-lane (silent, transient) star"
        );
        assert!(m.sown.is_empty(), "the stage is drained");

        // Off the glass inside 315 ms; the ring landing leaves with its pool.
        let mut t = t0;
        let mut gone_at = None;
        while t <= t0 + ms(700) {
            let ctx = ctx_at(t, &cfg, (5, 40), 0.3);
            sc.clear();
            let mut fr = sc.frame();
            dust.emit(&ctx, &mut fr);
            if gone_at.is_none() && dust.at_rest() {
                gone_at = Some(t);
            }
            t += ms(16);
        }
        let gone = gone_at.expect("the fan leaves the glass");
        assert!(
            gone <= t0 + ms(315),
            "the party's last star must be gone by T + 315 ms, was {:?}",
            gone.saturating_duration_since(t0)
        );
        let late = ctx_at(t0 + ms(700), &cfg, (5, 40), 0.3);
        sc.emit(&mut m, &late);
        assert!(
            m.at_rest(),
            "the ring landing is out of the pool with the meteor horizon"
        );

        // Off the glass: nothing.
        let mut off = Meteors::new();
        let far = ctx_at(t0, &cfg, (200, 40), 0.3);
        off.party(t0, 9, true, &far);
        assert!(
            off.sown.is_empty() && off.landings.is_empty(),
            "a caret off the glass mints nothing"
        );
    }

    // ---- the landing's pace, on a real band ---------------------------------

    // -- the hand's floor, on the impact's one travelling mark --------------

    /// The shower stops where the hand stopped ([`spark_room`]): nothing the
    /// hand did not put there goes left of where the hand has been on that
    /// row. Modelled on a bash prompt: a 28-column `PS1` on row 6, a caret that
    /// held only columns 28..43, then `^A` from `6,42` to `6,28`.
    ///
    /// Read in columns, from the quads and halos the shower paints over its
    /// whole life. The floor is column 28, and a star centred on it spills its
    /// arm into 27 as the pin and flash do, so 27 is the bound; no spark ever
    /// stands left of the floor cell. The shower keeps every spark and still
    /// spreads across the row, and a landing with no known floor is unbounded.
    #[test]
    fn the_shower_stops_where_the_hand_stopped() {
        let cfg = config();
        let g = geom();
        let cw = g.cw as f32;
        let ch = g.ch as f32;
        let col_of = |x: f32| (x / cw).floor() as i32;

        // The hand has held columns 28..43 of row 6 and nothing left of 28.
        let floor = (6_u16, 28_u16);
        let (from, to) = ((6_u16, 42_u16), (6_u16, 28_u16));

        let run = |bounded: bool| {
            let t0 = Instant::now();
            let mut m = Meteors::new();
            if bounded {
                m.set_hand_floor(Some(floor));
            }
            let ctx = ctx_at(t0, &cfg, to, 0.30);
            m.on_event(&mv_as(from, to, Licence::Nav), t0, &ctx)
                .expect("a 14-cell keyed jump must fly");
            let mut sc = Scratch::default();
            let mut ink_min = i32::MAX;
            let mut ink_max = i32::MIN;
            let mut station_min = i32::MAX;
            let mut seen = 0_usize;
            for f in 0..45_u64 {
                let at = t0 + ms(f * 16);
                let ctx = ctx_at(at, &cfg, to, 0.30);
                // The pool advances exactly as it does on glass…
                sc.emit(&mut m, &ctx);
                // …and the shower alone is read back off it, so the burst's
                // rooted lances and the flash's own cell are not mistaken
                // for the travelling mark (they do not travel).
                let mut sp = Scratch::default();
                {
                    let mut fr = sp.frame();
                    for l in &m.landings {
                        draw_sparks(l, &ctx, &mut fr, SPARK_HALO_CAP);
                    }
                }
                for x in sp
                    .out
                    .iter()
                    .map(|q| f32::from(q.x))
                    .chain(sp.halos.iter().map(|q| f32::from(q.x)))
                {
                    seen += 1;
                    ink_min = ink_min.min(col_of(x));
                }
                for q in &sp.out {
                    ink_max = ink_max.max(col_of(f32::from(q.x) + f32::from(q.w)));
                }
                for l in &m.landings {
                    let age = ms_since(l.pin.at, ctx.now);
                    for s in l.spark.iter().take(usize::from(l.sparks)) {
                        if let Some(((dx, _), _)) = spark_at(*s, age, ch) {
                            station_min = station_min.min(col_of(l.x + dx));
                        }
                    }
                }
            }
            (ink_min, ink_max, station_min, seen)
        };

        let (ink_min, ink_max, station_min, seen) = run(true);
        assert!(
            seen > 0,
            "the shower drew nothing at all — nothing is pinned"
        );

        // The law, in the terms glass measures it.
        assert!(
            ink_min >= 27,
            "the shower painted column {ink_min}: the hand's floor is column 28 and a star \
             centred on it spills no further than 27"
        );
        // …and on the spark's own station: no spark stands in the prompt.
        assert!(
            station_min >= 28,
            "a spark came to rest in column {station_min}, left of the hand's floor (28)"
        );

        // Not A mute: the shower still crosses the row, into the room the
        // hand owns. (The landing is column 28; this is four columns of
        // travel and more.)
        assert!(
            ink_max >= 32,
            "the bounded shower reached only column {ink_max} — it is a wall, not a shower"
        );

        // An unknown floor bounds nothing: without it the same jump throws its
        // sparks deep to the left, which shows the bound above is the floor's.
        let (free_min, _, free_station, free_seen) = run(false);
        assert_eq!(free_seen, seen, "the wall must not cost the shower a spark");
        assert!(
            free_min <= 20 && free_station <= 21,
            "with no floor the shower reached column {free_min} (stations {free_station}); \
             it used to reach 18"
        );
    }

    // ── Tier-1 conformance: `RainbowLandingPool` ─────────────────────────────

    /// One real step on the landing pool: an arrival through either admission
    /// site, the pool's own expiry, or a reset.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PoolStep {
        /// A meteor flies and lands through `emit` — admission site 1.
        Arrive,
        /// A ringing party bar through `party` — admission site 2.
        Ring,
        /// `emit` at the oldest landing's own end: its `retain` expires it.
        Expire,
        /// `Meteors::reset`.
        Reset,
    }

    impl PoolStep {
        const ALL: [Self; 4] = [Self::Arrive, Self::Ring, Self::Expire, Self::Reset];

        const fn action(self) -> &'static str {
            match self {
                Self::Arrive | Self::Ring => "Land",
                Self::Expire => "ExpireOne",
                Self::Reset => "Reset",
            }
        }
    }

    /// A real pool on a harness clock, with the arrival edge of every landing
    /// it was handed — the identities the projection reads ordinals by.
    struct Pool {
        m: Meteors,
        now: Instant,
        arrivals: Vec<Instant>,
        /// Consecutive flights cross the same 20 cells in alternating
        /// directions, so every one is a same-row jump of one length.
        at_right: bool,
    }

    /// The clock step between two arrivals the harness makes. Every landing
    /// lives exactly 600 ms, so arrivals a microsecond apart expire a
    /// microsecond apart — in arrival order, one per `Expire`.
    const POOL_TICK: Duration = Duration::from_micros(1);

    /// A fault injected after a real admission, given the pool as it stood
    /// before the step.
    type PoolPatch = fn(&mut Meteors, PoolStep, &[Landing]);

    fn no_pool_patch(_: &mut Meteors, _: PoolStep, _: &[Landing]) {}

    /// The model's `Buggy = 1` at one admission site: a saturated pool drops the
    /// arrival (`if len < LANDING_POOL { push }`) instead of the oldest.
    fn drop_the_arrival_at(site: PoolStep, m: &mut Meteors, step: PoolStep, before: &[Landing]) {
        if step == site && before.len() == LANDING_POOL {
            m.landings = before.to_vec();
        }
    }

    fn drop_the_arrival_in_emit(m: &mut Meteors, step: PoolStep, before: &[Landing]) {
        drop_the_arrival_at(PoolStep::Arrive, m, step, before);
    }

    fn drop_the_arrival_in_party(m: &mut Meteors, step: PoolStep, before: &[Landing]) {
        drop_the_arrival_at(PoolStep::Ring, m, step, before);
    }

    impl Pool {
        fn new() -> Self {
            Self {
                m: Meteors::new(),
                now: Instant::now(),
                arrivals: Vec::new(),
                at_right: false,
            }
        }

        /// Drive `step` on the real pool. `Ok(false)` when the real clock
        /// cannot realise it from here: a flight spends ~70 ms in the air, and
        /// an arrival must not outlive a resident while it flies (the model has
        /// no clock — its `Land` expires nothing). `Err` when the real code
        /// did something the step cannot be read as.
        fn drive(&mut self, step: PoolStep, patch: PoolPatch) -> Result<bool, String> {
            let cfg = config();
            let before = self.m.landings.clone();
            match step {
                PoolStep::Arrive => {
                    let (from, to) = if self.at_right {
                        ((5, 30), (5, 10))
                    } else {
                        ((5, 10), (5, 30))
                    };
                    let t0 = self.now + POOL_TICK;
                    let arrival = t0 + flight(20.0);
                    if self.m.landings.iter().any(|l| l.end() <= arrival) {
                        return Ok(false);
                    }
                    let spawn = self
                        .m
                        .on_event(&mv(from, to), t0, &ctx_at(t0, &cfg, to, 0.3))
                        .ok_or("a 20-cell same-row jump must fly")?;
                    if spawn.t0 + spawn.t_flight != arrival {
                        return Err(format!("the flight lands at {:?}", spawn.t_flight));
                    }
                    Scratch::default().emit(&mut self.m, &ctx_at(arrival, &cfg, to, 0.3));
                    self.at_right = !self.at_right;
                    self.now = arrival;
                    self.arrivals.push(arrival);
                }
                PoolStep::Ring => {
                    self.now += POOL_TICK;
                    let at = self.now;
                    self.m.party(at, 11, true, &ctx_at(at, &cfg, (5, 40), 0.3));
                    self.arrivals.push(at);
                }
                PoolStep::Expire => {
                    let oldest = self.m.landings.first().ok_or("nothing to expire")?;
                    self.now = self.now.max(oldest.end());
                    Scratch::default().emit(&mut self.m, &ctx_at(self.now, &cfg, (5, 40), 0.3));
                }
                PoolStep::Reset => self.m.reset(),
            }
            patch(&mut self.m, step, &before);
            Ok(true)
        }

        fn project(&self) -> aterm_spec::interp::State {
            let ordinal = |l: &Landing| {
                self.arrivals
                    .iter()
                    .position(|&t| t == l.pin.at)
                    .map_or(-1, |i| i64::try_from(i).unwrap_or(i64::MAX) + 1)
            };
            BTreeMap::from([
                (
                    "resident",
                    i64::try_from(self.m.landings.len()).unwrap_or(i64::MAX),
                ),
                ("newest", self.m.landings.last().map_or(0, ordinal)),
                ("oldest", self.m.landings.first().map_or(0, ordinal)),
                (
                    "issued",
                    i64::try_from(self.arrivals.len()).unwrap_or(i64::MAX),
                ),
            ])
        }
    }

    /// What the walk covered, so a pass can be checked for non-vacuity.
    #[derive(Default)]
    struct PoolCoverage {
        states: usize,
        actions: std::collections::BTreeSet<&'static str>,
        /// Admission sites driven into a full pool — the one state the
        /// drop-the-arrival mutant shows in.
        saturated_sites: std::collections::BTreeSet<&'static str>,
    }

    /// Walk `model`'s whole reachable space on a real `Meteors` (with `patch`
    /// injected after every step). At every model state a real trace that
    /// reached it is replayed, every step the model enables and the real clock
    /// can realise is driven, and the projection must be exactly the model's
    /// one successor. `Err` names the first real transition the model does not
    /// admit.
    fn pool_walk(
        model: &aterm_spec::derive::Model,
        patch: PoolPatch,
    ) -> Result<PoolCoverage, String> {
        let init = model.init_state();
        if Pool::new().project() != init {
            return Err(format!("an empty pool is not the model's Init {init:?}"));
        }
        let mut seen = std::collections::BTreeSet::from([init.clone()]);
        let mut queue = std::collections::VecDeque::from([(init, Vec::<PoolStep>::new())]);
        let mut coverage = PoolCoverage::default();
        while let Some((state, path)) = queue.pop_front() {
            coverage.states += 1;
            for step in PoolStep::ALL {
                let action = step.action();
                let expected = model.successors(action, &state);
                if expected.is_empty() {
                    continue;
                }
                let mut pool = Pool::new();
                for &earlier in &path {
                    if !pool.drive(earlier, patch)? {
                        return Err(format!("{path:?} no longer replays at {earlier:?}"));
                    }
                }
                if !pool.drive(step, patch)? {
                    continue;
                }
                let after = pool.project();
                if expected != [after.clone()] {
                    return Err(format!(
                        "{}: real {step:?} after {path:?} took {state:?} to {after:?}, \
                         the model's {action} to {expected:?}",
                        model.name
                    ));
                }
                coverage.actions.insert(action);
                if action == "Land" && state["resident"] == 3 {
                    coverage.saturated_sites.insert(match step {
                        PoolStep::Arrive => "emit",
                        _ => "party",
                    });
                }
                if seen.insert(after.clone()) {
                    let mut next = path.clone();
                    next.push(step);
                    queue.push_back((after, next));
                }
            }
        }
        Ok(coverage)
    }

    /// Tier-1 conformance for `RainbowLandingPool`
    /// (`aterm_models_effects::rainbow_landing_pool_model`): the landing pool is
    /// a bounded FIFO that always holds the latest arrivals, over the model's
    /// whole reachable space, on a real `Meteors`, through both admission
    /// sites (a flight's arrival in `emit`, a ringing bar in `party`), the real
    /// `retain` expiry and `reset`. The model's `Cap` is `LANDING_POOL`.
    ///
    /// Negative control: the model's `Buggy = 1` (a saturated pool drops the
    /// arrival instead of the oldest), injected at each admission site, is
    /// refused by the healthy model at `Land`; the shipped pool is refused by
    /// the buggy model.
    #[test]
    fn the_landing_pool_conforms_to_the_model() {
        let model = aterm_models_effects::rainbow_landing_pool_model();
        assert_eq!(
            model
                .consts
                .iter()
                .find(|(name, _)| *name == "Cap")
                .map(|(_, cap)| *cap),
            Some(i64::try_from(LANDING_POOL).unwrap()),
            "the model's capacity is the shipped pool's"
        );
        let coverage = pool_walk(&model, no_pool_patch).unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(
            coverage.states,
            aterm_spec::interp::bmc(&model).expect("the healthy model holds"),
            "the real pool must reach every state the model can"
        );
        assert_eq!(
            coverage.actions,
            model.actions.iter().map(|a| a.name).collect(),
            "every model action must be driven on the real pool"
        );
        assert_eq!(
            coverage.saturated_sites,
            ["emit", "party"].into_iter().collect(),
            "both admission sites must be driven into a full pool"
        );

        for (site, patch) in [
            ("emit", drop_the_arrival_in_emit as PoolPatch),
            ("party", drop_the_arrival_in_party),
        ] {
            let refused = pool_walk(&model, patch).err().unwrap_or_else(|| {
                panic!("the healthy model must refuse a dropped arrival in {site}")
            });
            assert!(refused.contains("the model's Land"), "{site}: {refused}");
        }
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let shipped = pool_walk(&buggy, no_pool_patch)
            .err()
            .expect("the shipped pool must not conform to the drop-the-arrival model");
        assert!(shipped.contains("the model's Land"), "{shipped}");
    }
}

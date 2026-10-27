// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow kitty streaking meteor, corona, flash, starburst, and sparks.

use std::mem;

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::{
    BeamClip, BeamRows, BeamVertex, GlowBlend, GlowEnd, GlowQuad, GlowRamps, HaloMode, RainHalo,
    RampMark, RampSink, RibbonVertex, end_alpha, end_color, glow_end, premul_rgb, ribbon_beam_into,
    ribbon_beam_v,
};

use crate::cursor_glow::{BandPx, Geom, InkRole, band_row, night_rgb};
use crate::effect_util::{lerp_rgb, push_fx_quad, push_fx_rect, push_twinkle_star};
use crate::spectrum::{
    SPECTRUM_STOPS, spectrum, spectrum_snap, spectrum_snap_index, spectrum_stop,
};

use super::companion::KeepOut;
use super::ribbon::{
    BED_INK_LUT_LEN, PAPER_TRAIN_SKIP, SLABS_PER_CELL, WALK_FAST_CELLS, WALK_LAY_RATE,
    hot_edge_ink, paper_caret_ink, paper_ink, paper_mark_ink, paper_train_ink, walk_pace, walk_t,
};
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

/// Constant term of the meteor width law.
pub const W_BASE_CH: f32 = 0.44;

/// Momentum term of the width law.
pub const W_MOM_CH: f32 = 0.34;

/// Distance grade gain on width.
pub const W_GRADE_GAIN: f32 = 0.5;

/// Cells at which `grade` saturates.
pub const GRADE_CELLS: f32 = 34.0;

/// Base coverage of the white heat.
pub const HEAD_COV_BASE: f32 = TRANSIENT_STAR_COV_CEIL;

/// Brightness grade gain.
pub const BRIGHT_GRADE_GAIN: f32 = 0.5;

/// Coverage of the colour train's head.
pub const COLOUR_TRAIN_COV: f32 = TRANSIENT_STAR_COV_CEIL;

/// Reflected triangle wave on `[0, 1]`.
#[inline]
#[must_use]
pub const fn tri(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    let r = if x >= 0.0 && x < 2.0 {
        x
    } else {
        let rem = x % 2.0;
        if rem < 0.0 { rem + 2.0 } else { rem }
    };
    1.0 - (1.0 - r).abs()
}

/// How many `t`-units of the reflected walk the train carries inside its frame-0 span.
pub const ARC_FRAME0_SWEEPS: f32 = 1.5;

/// Multiplier on the classic lay rate that puts [`ARC_FRAME0_SWEEPS`] inside the frame-0 span.
#[inline]
#[must_use]
pub const fn arc_gain(cells: f32) -> f32 {
    if !cells.is_finite() || cells <= 0.0 {
        return 1.0;
    }
    (ARC_FRAME0_SWEEPS / (FLIGHT_P0 * cells * WALK_LAY_RATE)).max(1.0)
}

/// The landing's walk off any band.
#[inline]
#[must_use]
pub const fn landing_field(t_land: f32, dx_cells: f32) -> f32 {
    if !dx_cells.is_finite() {
        return tri(t_land);
    }
    tri(t_land + dx_cells * (1.0 / WALK_FAST_CELLS))
}

/// The landing's walk: the one colour law every mark of a landing reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LandingWalk {
    /// The field at the landing cell, raw.
    pub t_land: f32,
    /// `Some((d_land, t0))` on a laid band (`d_land` in cells, signed).
    pub band: Option<(f32, f32)>,
}

impl LandingWalk {
    /// A walk off any band ([`landing_field`]).
    #[inline]
    #[must_use]
    pub const fn free(t_land: f32) -> Self {
        Self { t_land, band: None }
    }

    /// The folded arc position `dx_cells` to the right of the landing cell.
    #[inline]
    #[must_use]
    pub const fn at(&self, dx_cells: f32) -> f32 {
        if !dx_cells.is_finite() {
            return tri(self.t_land);
        }
        match self.band {
            Some((d_land, t0)) => tri(t0 + walk_t(d_land + dx_cells)),
            None => landing_field(self.t_land, dx_cells),
        }
    }
}

/// The landing's ink at folded arc position `t`.
#[inline]
#[must_use]
pub const fn landing_ink(t: f32) -> u32 {
    let lut = super::ribbon::hot_edge_ink_lut();
    let x = clamp01(t) * (BED_INK_LUT_LEN - 1) as f32;
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
    lerp_rgb(lut[i], lut[j], x - i as f32)
}

const LANDING_STOP_LUT: [u32; SPECTRUM_STOPS] = {
    let mut lut = [0u32; SPECTRUM_STOPS];
    let mut i = 0;
    while i < SPECTRUM_STOPS {
        lut[i] = hot_edge_ink(spectrum_stop(i));
        i += 1;
    }
    lut
};

/// The landing's ink at a named stop.
#[inline]
#[must_use]
pub const fn landing_stop_ink(i: usize) -> u32 {
    let idx = if i < SPECTRUM_STOPS - 1 {
        i
    } else {
        SPECTRUM_STOPS - 1
    };
    LANDING_STOP_LUT[idx]
}

const SPARK_CORE_LUT: [u32; SPECTRUM_STOPS] = {
    let mut lut = [0u32; SPECTRUM_STOPS];
    let mut i = 0;
    while i < SPECTRUM_STOPS {
        lut[i] = lerp_rgb(0x00FF_FFFF, LANDING_STOP_LUT[i], SPARK_CORE_TINT);
        i += 1;
    }
    lut
};

#[inline]
#[must_use]
const fn landing_spark_core_ink(i: usize) -> u32 {
    let idx = if i < SPECTRUM_STOPS - 1 {
        i
    } else {
        SPECTRUM_STOPS - 1
    };
    SPARK_CORE_LUT[idx]
}

/// Stations per train layer.
pub const STATIONS_MAX: usize = 96;

/// How far the head end of a train layer eases in from nothing, in cells.
pub const END_EASE_HEAD_CELLS: f32 = 0.9;
/// How far the tail end of a train layer eases in from nothing, in cells.
pub const END_EASE_TAIL_CELLS: f32 = 1.0;
/// Maximum share of a short train that either end's ease may take.
pub const END_EASE_SPAN_SHARE: f32 = 0.2;
/// Stations each eased end adds.
pub const END_EASE_STATIONS: usize = 8;

/// Minimum station stride in px.
pub const STATION_STRIDE_MIN_PX: f32 = 2.0;

/// Hard cap on one meteor's `under` share.
pub const UNDER_QUAD_CAP: usize = 2_304;

/// Cap on one meteor's white (`out`) layer.
pub const WHITE_QUAD_CAP: usize = 1_152;

/// Width falloff length as a share of `L`.
pub const WIDTH_FALLOFF_SHARE: f32 = 0.5;

/// The width's floor share, reached at the tail.
pub const WIDTH_FLOOR_SHARE: f32 = 0.22;

/// The width's exponentially-decaying share.
pub const WIDTH_SPAN_SHARE: f32 = 0.78;

/// The transverse profile's core share.
pub const TRAIN_CORE_SHARE: f32 = 0.5;

/// Colour-layer coverage falloff length as a share of `L`.
pub const COLOUR_FALLOFF_SHARE: f32 = 0.55;

/// White-layer coverage falloff length as a share of `L`.
pub const WHITE_FALLOFF_SHARE: f32 = 0.12;

/// The white layer's width as a share of `w(s)` past the shoulder.
pub const WHITE_WIDTH_SHARE: f32 = 0.45;

/// How many e-folds of [`WHITE_FALLOFF_SHARE`] the white polyline spans.
pub const WHITE_TAIL_EFOLDS: f32 = 5.0;

/// The shoulder's design length as a share of `L`.
pub const SHOULDER_SHARE: f32 = 0.06;

/// The shoulder's width as a share of `w`.
pub const SHOULDER_WIDTH_SHARE: f32 = 0.55;

/// Shoulder length floor, in cells.
pub const SHOULDER_MIN_CELLS: f32 = 1.0;

/// Shoulder length ceiling, in cells.
pub const SHOULDER_MAX_CELLS: f32 = 4.0;

/// The effect frame the shoulder's continuity floor is sized to, ms.
pub const SHOULDER_FRAME_MS: f32 = 33.4;

/// The corona's reach as a multiple of the nucleus radius.
pub const COMA_R_SCALE: f32 = 2.4;

/// Each petal's coverage as a share of `head_cov`.
pub const COMA_COV_SHARE: f32 = 0.34;

/// Seven corona petals, one per named stop.
pub const COMA_PETALS: usize = 7;

/// A petal's radius as a share of the corona's reach.
pub const COMA_PETAL_R: f32 = 0.62;

/// A petal's orbit as a share of the corona's reach.
pub const COMA_PETAL_ORBIT: f32 = 0.72;

/// One turn of the corona, ms.
pub const COMA_SPIN_MS: f32 = 240.0;

/// The smallest radius a dark head's light keeps as it shrinks, px: under
/// it the light is culled rather than drawn as a dim speck.
pub const HEAD_LIGHT_MIN_R_PX: f32 = 1.0;

/// Nucleus core radius floor in px.
pub const NUCLEUS_MIN_PX: f32 = 2.5;

/// Nucleus core radius as a share of `w`.
pub const NUCLEUS_W_SHARE: f32 = 0.22;

/// Nucleus stretch gain.
pub const NUCLEUS_STRETCH_GAIN: f32 = 1.2;

/// How far ahead of the head the nucleus centre sits, in px.
pub const NUCLEUS_LEAD_PX: f32 = 2.0;

/// Terminal-flash duration in ms.
pub const FLASH_MS: f32 = 40.0;

/// Terminal-flash peak radius multiplier.
pub const FLASH_PEAK: f32 = 1.6;

/// How far past [`FLASH_PEAK`] the back ease carries the radius before it relaxes.
pub const FLASH_BACK: f32 = 0.1;

/// Terminal-flash overshoot at [`FLASH_MS`].
pub const FLASH_OVERSHOOT: f32 = FLASH_PEAK + FLASH_BACK;

/// Milliseconds by which the flash has relaxed back to ×1.0.
pub const FLASH_RELAX_MS: f32 = 120.0;

/// Pin life in ms.
pub const PIN_MS: f32 = 150.0;

/// Pin arm half-length at `u = 0`, in `ch`.
pub const PIN_ARM_CH: f32 = 0.6;

/// The `u` at which the pin's arms start to close.
pub const PIN_ARM_HOLD_U: f32 = 0.15;

/// The exponent of the pin's arm law.
pub const PIN_ARM_EXP: f32 = 1.2;

/// The pin's waist, in px.
pub const PIN_WAIST_PX: i32 = 1;

/// The pin's nucleus, in px.
pub const PIN_NUCLEUS_PX: i32 = 2;

/// The `u` up to which the pin holds α = 1 before spending it.
pub const PIN_ALPHA_HOLD_U: f32 = 0.6;

/// How long the arrival's white burst lives, ms from the pin's edge.
pub const FLASH_BURST_MS: f32 = 80.0;

/// When the flash's colour layer is spent, ms from the pin.
pub const FLASH_COLOUR_MS: f32 = 2.0 * FLASH_BURST_MS;

/// The flash's coverage over a cell the sky's probe proved blank.
pub const FLASH_FULL_COV: f32 = 224.0;

/// How many cells either side of the caret the flash covers.
pub const FLASH_CELLS: i32 = 1;

/// The reduced-motion flash's white share of its cell's coverage.
pub const FLASH_STATIC_WHITE_SHARE: f32 = 0.5;

/// Impact life in ms at the floor.
pub const RING_MS: f32 = 480.0;

/// Extra impact life per unit of impact above the floor, in ms.
pub const RING_MS_PER_IMPACT: f32 = 48.0;

/// Ring life ceiling in ms.
pub const RING_MS_MAX: f32 = FLIGHT_OFF_GLASS_MS;

/// Extra impact radius per unit of impact above the floor, in `ch`.
pub const RING_R_PER_IMPACT_CH: f32 = 1.2;

/// The impact's life for a landing of `cells`.
#[inline]
#[must_use]
pub const fn ring_ms(cells: f32) -> f32 {
    (RING_MS + RING_MS_PER_IMPACT * (impact(cells) - 1.0)).min(RING_MS_MAX)
}

/// The impact's full radius (at `u = 1`) for a landing of impact `scale`, in px.
#[inline]
#[must_use]
pub const fn ring_full_radius(scale: f32, ch: f32) -> f32 {
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

/// The fan's star count for a big landing of `cells`.
#[inline]
#[must_use]
pub const fn fan_count(cells: f32, party: bool) -> usize {
    let base = (FAN_N_BASE + FAN_N_PER_IMPACT * (impact(cells) - 1.0)).min(FAN_N_CEIL);
    let party_add = if party { FAN_PARTY_ADD } else { 0 };
    let total = (base.round() as usize) + party_add;
    if total < super::timing::FAN_MAX_N {
        total
    } else {
        super::timing::FAN_MAX_N
    }
}

/// The fan's throw reach for a big landing of `cells`, in `ch`.
#[inline]
#[must_use]
pub const fn fan_reach_ch(cells: f32, grade: f32) -> f32 {
    let ceil = FAN_REACH_MAX_CH + FAN_REACH_GRADE_CH * grade;
    (FAN_REACH_BASE_CH + FAN_REACH_PER_IMPACT_CH * (impact(cells) - 1.0))
        .clamp(FAN_REACH_MIN_CH, ceil)
}

/// Impact radius at `u = 1` at the floor, in `ch`.
pub const RING_R_CH: f32 = 3.0;

/// The exponent of the impact's radius law.
pub const RING_R_EXP: i32 = 4;

/// Impact coverage as a share of [`TRANSIENT_STAR_COV_CEIL`].
pub const RING_COV_SHARE: f32 = 1.0;

/// The `u` up to which the impact holds its full coverage before spending it.
pub const RING_COV_HOLD_U: f32 = 0.25;

/// The stroke's hold in ms at the floor, [`RING_COV_HOLD_U`] of [`RING_MS`].
pub const RING_HOLD_MS: f32 = RING_COV_HOLD_U * RING_MS;

/// The stroke's hold at the cap, ms.
pub const RING_HOLD_MAX_MS: f32 = RING_COV_HOLD_U * RING_MS_MAX;

/// Fan reach at the floor landing, in `ch`.
pub const FAN_REACH_BASE_CH: f32 = 2.8;

/// Fan reach per unit of [`impact`] above the floor, in `ch`.
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

/// Fan reach floor, in `ch`.
pub const FAN_REACH_MIN_CH: f32 = 1.6;

/// Fan reach ceiling before the grade term, in `ch`.
pub const FAN_REACH_MAX_CH: f32 = 6.8;

/// How much `grade` lifts the fan's reach ceiling.
pub const FAN_REACH_GRADE_CH: f32 = 1.2;

/// Fan count at the floor landing ([`fan_count`]).
pub const FAN_N_BASE: f32 = 19.0;

/// Extra fan stars per unit of [`impact`] above the floor.
pub const FAN_N_PER_IMPACT: f32 = 3.6;

/// Fan count ceiling before the party bonus.
pub const FAN_N_CEIL: f32 = 28.0;

const _: () = assert!(
    FAN_N_BASE + FAN_N_PER_IMPACT * (super::timing::IMPACT_MAX - 1.0) <= FAN_N_CEIL + 1e-3,
    "the capped landing's fan count lands exactly on the ceiling"
);

/// What a party adds to the fan's count.
pub const FAN_PARTY_ADD: usize = 8;

/// The eased spine at or above which a landing is a party.
pub const FAN_PARTY_DISP: f32 = 0.95;

/// Cells of landing size per row of a vertical / Enter flight.
pub const ENTER_LANDING_CELLS_PER_ROW: f32 = 4.0;

/// Floor on an Enter / vertical landing's grain count.
pub const ENTER_LANDING_M3_MIN: usize = 5;

/// Ceiling on an Enter / vertical landing's grain count.
pub const ENTER_LANDING_M3_MAX: usize = 7;

/// Nearest station of a shed fragment, as a share of `L`.
pub const SHED_S_MIN: f32 = 0.10;

/// Farthest station of a shed fragment, as a share of `L`.
pub const SHED_S_MAX: f32 = 0.70;

/// White layer's post-arrival τ, ms.
pub const TRAIN_WHITE_TAU_MS: f32 = 90.0;

/// Colour layer's post-arrival τ, ms.
pub const TRAIN_COLOUR_TAU_MS: f32 = 220.0;

/// The colour root's retract span, ms.
pub const ROOT_SUCK_MS: f32 = 460.0;

/// The thickness the train thins to over the fade, as a share of `w`.
pub const THICKNESS_END_SHARE: f32 = 0.4;

/// The span, ms, over which the white layer's extent retracts.
pub const WHITE_CLOSE_MS: f32 = PIN_MS;

/// The retire decay, `ln(1/CHROMA_CULL_ALPHA)`.
pub const RETIRE_DECAY: f32 = 2.1203;

/// Reduced motion's static train alpha.
pub const REDUCED_MOTION_ALPHA: f32 = 0.85;

/// How close two corridors must come for the newer to retire the older, in `ch`.
pub const CORRIDOR_CH: f32 = 1.0;

/// How close a new launch must be to a live head, in cells.
pub const CHAIN_CELLS: f32 = 1.0;

/// The meteor pool depth.
pub const METEOR_POOL: usize = 2 * FLIGHT_MAX_LIVE;

/// The landing pool depth.
pub const LANDING_POOL: usize = FLIGHT_MAX_LIVE + 1;

/// Depth of the per-tick hand-off scratch ([`Meteors::sow_into`]).
pub const SOW_SCRATCH: usize = METEOR_POOL * (SHED_MAX as usize + 1) + 2;

/// The core spike's tip radius at full expansion, in `ch`, sized by the rise cap.
pub const BURST_CORE_CH: f32 = 1.42;

/// Every spike's root radius, in `ch`.
pub const BURST_ROOT_CH: f32 = 0.50;

/// `2·sin(pi/BURST_N_MAX)`, the separation coefficient of the root law above.
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

/// The aa fringe [`comet_beam`](aterm_render::comet_beam) adds outside the nominal stroke, in `ch`.
pub const BURST_AA_SUPPORT_CH: f32 = 0.04;

const _: () = assert!(
    BURST_CORE_CH + 0.5 * BURST_SEC_THICK_CH[0] + BURST_AA_SUPPORT_CH <= FAN_RISE_MAX_CH,
    "the burst's PAINTED rise must stay inside the fan's — the centreline is not the mark"
);

/// Spikes at the 8-cell floor.
pub const BURST_N_BASE: f32 = 9.0;

/// Spikes gained per unit of [`impact`] above the floor.
pub const BURST_N_PER_IMPACT: f32 = 1.2;

/// Spike-count ceiling.
pub const BURST_N_CEIL: f32 = 12.0;

/// The `Burst::dir` / `Burst::len` array length.
pub const BURST_N_MAX: usize = 12;

const _: () = assert!(
    BURST_N_BASE + BURST_N_PER_IMPACT * (super::timing::IMPACT_MAX - 1.0) <= BURST_N_CEIL + 1e-3,
    "the graded spike count reaches the ceiling exactly at the impact cap and never past it"
);
const _: () = assert!(
    BURST_N_CEIL as usize == BURST_N_MAX,
    "the spike ceiling and the array that holds the spikes are ONE number"
);

/// The shortest spike, as a share of [`BURST_CORE_CH`].
pub const BURST_LEN_MIN: f32 = 0.78;

/// Every third spike is a hero.
pub const BURST_HERO_EVERY: usize = 3;

/// A hero's length gain, clamped to 1.0.
pub const BURST_HERO_GAIN: f32 = 1.28;

/// Angular jitter, as a share of one spacing (`2pi/n`).
pub const BURST_JITTER: f32 = 0.30;

const _: () = assert!(
    BURST_JITTER < 1.0,
    "an angular jitter of a whole spacing lets two spikes swap places and close their wedge"
);

/// The collinear sections of one spike, as shares of its span.
pub const BURST_SEC_SHARE: [f32; 3] = [0.46, 0.32, 0.22];

/// Vertices per lance section.
pub const BURST_SEC_STATIONS: usize = 3;

/// The sections' stroke thickness, in `ch` (`0.22 -> 0.15 -> 0.08`).
pub const BURST_SEC_THICK_CH: [f32; 3] = [0.22, 0.15, 0.08];

/// How much of the root's coverage the tip keeps.
pub const BURST_TIP_FADE: f32 = 0.45;

/// The floor of [`comet_beam`](aterm_render::comet_beam)'s major-axis stride for a core spike, px.
pub const BURST_STEP_PX: usize = 2;

/// The core's stride as a share of the cell, floored at [`BURST_STEP_PX`].
pub const BURST_STEP_CH_SHARE: f32 = 0.055;

/// Jets per side, along the flight axis.
pub const JET_N_PER_SIDE: usize = 2;

/// Each jet's tilt off the flight axis, radians.
pub const JET_TILT_RAD: [f32; JET_N_PER_SIDE] = [0.00, 0.13];

/// Each jet's tip, as a share of [`ring_full_radius`].
pub const JET_LEN_SHARE: [f32; JET_N_PER_SIDE] = [1.00, 0.66];

/// The tilted jet's vertical component, as a share of `sin(tilt)`.
pub const JET_RISE_SQUASH: f32 = 0.55;

/// The jets' two collinear sections, as shares of their span.
pub const JET_SEC_SHARE: [f32; 2] = [0.55, 0.45];

/// The jets' stroke, in `ch`.
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

/// The share of a jet's reach held at full coverage.
pub const JET_FEATHER_SHARE: f32 = 0.30;

/// The jets' share of the core's coverage.
pub const JET_COV_SHARE: f32 = 0.62;

/// Stations per cell along a jet.
pub const JET_STATIONS_PER_CELL: usize = SLABS_PER_CELL;

/// How many cells each side of the caret the landing row is probed.
pub const JET_PROBE_CELLS: i32 = 14;

/// The floor of [`comet_beam`](aterm_render::comet_beam)'s major-axis stride for a jet, px.
pub const JET_STEP_PX: usize = 4;

/// The jets' stride as a share of the cell, floored at [`JET_STEP_PX`].
pub const JET_STEP_CH_SHARE: f32 = 0.11;

/// A light-arm lance's step between ink squares, as a share of the section's own thickness.
pub const BURST_LIGHT_STEP_SHARE: f32 = 0.6;

const _: () = assert!(
    JET_LEN_SHARE[1]
        * (RING_R_CH + RING_R_PER_IMPACT_CH * (super::timing::IMPACT_MAX - 1.0))
        * JET_TILT_RAD[1]
        * JET_RISE_SQUASH
        + 0.5 * JET_SEC_THICK_CH[0]
        < 0.5,
    "a jet must stay inside the landing row it is gated on"
);

/// Enforced cap on one landing's burst (`out`).
pub const BURST_QUAD_CAP: usize = 1_280;

#[inline]
#[must_use]
const fn burst_step(ch: f32, share: f32, floor: usize) -> usize {
    let s = (ch * share).round() as usize;
    if s > floor { s } else { floor }
}

/// The largest spike count for a landing of `cells`.
#[inline]
#[must_use]
pub const fn burst_n(cells: f32) -> usize {
    let n = (BURST_N_BASE + BURST_N_PER_IMPACT * (impact(cells) - 1.0)).min(BURST_N_CEIL);
    let r = n.round() as usize;
    if r < 1 {
        1
    } else if r > BURST_N_MAX {
        BURST_N_MAX
    } else {
        r
    }
}

/// Spark count constant term.
pub const SPARK_N_BASE: f32 = 36.0;

/// Cells of path per extra spark.
pub const SPARK_CELLS_PER: f32 = 2.0;

/// Spark count ceiling.
pub const SPARK_N_MAX: usize = 64;

/// The smallest spark's arm half-length, px.
pub const SPARK_ARM_MIN_PX: i32 = 2;

/// The largest spark's arm half-length, px.
pub const SPARK_ARM_MAX_PX: i32 = 4;

/// How far a spark's core is tinted from white toward its stop.
pub const SPARK_CORE_TINT: f32 = 0.75;

/// A spark's halo radius, px, over its arm.
pub const SPARK_HALO_R_ADD_PX: f32 = 2.0;

/// A spark's halo peak as a share of its core coverage.
pub const SPARK_HALO_SHARE: f32 = 0.55;

/// How many spark halos one frame may carry across every live landing.
pub const SPARK_HALO_CAP: usize = 64;

/// Shortest spark life, ms.
pub const SPARK_LIFE_MIN_MS: f32 = 460.0;

/// Longest spark life, ms.
pub const SPARK_LIFE_MAX_MS: f32 = 600.0;

/// Slowest launch speed, in `ch` per second.
pub const SPARK_V_MIN_CH_PER_S: f32 = 5.0;

/// Fastest launch speed, in `ch` per second.
pub const SPARK_V_MAX_CH_PER_S: f32 = 9.0;

/// Gravity, in `ch` per second squared.
pub const SPARK_G_CH_PER_S2: f32 = 24.0;

/// The launch cone: angles from `π·this` to `π·(1 − this)`.
pub const SPARK_CONE_SHARE: f32 = 0.08;

/// How much wider the shower is along the line than up it.
pub const SPARK_SPREAD: f32 = 1.4;

/// The `u` of a spark's life it holds full coverage for before spending it.
pub const SPARK_HOLD_U: f32 = 0.6;

/// The widest a spark's core gets, px.
#[cfg(test)]
pub const SPARK_PX: i32 = 3;

/// The alpha under which a spark loses its halo and a pixel of arm.
pub const SPARK_SHRINK_ALPHA: f32 = 0.6;

/// Where the light-theme train's rail sits below the row's centre, in `ch`.
pub const LIGHT_RAIL_CH: f32 = 0.62;

/// The light-theme train's squash on the rail.
pub const LIGHT_RAIL_SQUASH: f32 = 0.34;

/// Coverage-to-ink-alpha gain on the light arm.
pub const LIGHT_INK_GAIN: f32 = 2.1;

/// Light-theme ink alpha cap.
pub const LIGHT_ALPHA_CAP: f32 = 190.0;

/// Light-theme rail ink alpha cap.
pub const LIGHT_RAIL_ALPHA_CAP: f32 = 236.0;

/// Dominant flight axis for ribbon rasterization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// `|dx| ≥ |dy|` — `aterm_render::ribbon_beam`.
    Horizontal,
    /// `|dy| > |dx|` — `aterm_render::ribbon_beam_v`.
    Vertical,
}

impl Axis {
    /// The axis of a pixel-space delta.
    #[inline]
    #[must_use]
    pub const fn of(dx: f32, dy: f32) -> Self {
        if dx.abs() >= dy.abs() {
            Self::Horizontal
        } else {
            Self::Vertical
        }
    }
}

/// One meteor in the fixed-depth pool ([`METEOR_POOL`]).
#[derive(Clone, Copy, Debug)]
pub struct Meteor {
    /// Window-absolute X of the launch, px.
    pub x0: f32,
    /// Window-absolute Y of the launch, px.
    pub y0: f32,
    /// Window-absolute X of the landing, px.
    pub x1: f32,
    /// Window-absolute Y of the landing, px.
    pub y1: f32,
    /// The spawn edge: the frame the caret was first observed at its landing.
    pub t0: Instant,
    /// Flight duration `timing::flight(cells)`.
    pub t_flight: Duration,
    /// Body thickness in px.
    pub w: f32,
    /// Distance grade `clamp(cells/34, 0, 1)`.
    pub grade: f32,
    /// When a crossing meteor retired this one.
    pub retired_at: Option<Instant>,
    /// True once a same-direction continuation chained off this head.
    pub chained: bool,
    /// The caret's own field `t` at the landing cell.
    pub t_land: f32,
    /// The band's walk at the landing.
    pub band: Option<(f32, f32)>,
    /// Path length in cells, `L / cw`.
    pub cells: f32,
    /// Travel direction, dominant axis.
    pub dir: Dir,
    /// Deterministic seed for hashed choices.
    pub seed: u32,
    /// Which shed fragments have been born, bit `i` for station `i`.
    pub shed: u8,
    /// Whether the arrival edge's marks have been minted.
    pub landed: bool,
    /// Whether this is the full landing (burst, flash, sparks, gold hero).
    pub ring: bool,
}

impl Meteor {
    /// The arrival edge.
    #[inline]
    #[must_use]
    pub fn arrival(&self) -> Instant {
        self.t0 + self.t_flight
    }

    /// The path vector in px.
    #[inline]
    #[must_use]
    pub const fn delta(&self) -> (f32, f32) {
        (self.x1 - self.x0, self.y1 - self.y0)
    }

    /// Path length `L` in px.
    #[inline]
    #[must_use]
    pub const fn length(&self) -> f32 {
        let (dx, dy) = self.delta();
        crate::color_math::hypot_f32(dx, dy)
    }

    /// The train's spectrum position `cells_behind_landing` cells short of the landing.
    #[inline]
    #[must_use]
    pub const fn arc(&self, cells_behind_landing: f32) -> f32 {
        tri(self.t_land - cells_behind_landing * WALK_LAY_RATE * arc_gain(self.cells))
    }

    /// Which rasterizer this flight takes.
    #[inline]
    #[must_use]
    pub const fn axis(&self) -> Axis {
        let (dx, dy) = self.delta();
        Axis::of(dx, dy)
    }

    /// True once a crossing meteor has retired this one.
    #[must_use]
    #[cfg(test)]
    pub const fn retiring(&self) -> bool {
        self.retired_at.is_some()
    }

    /// Whether the head left the glass in flight.
    #[inline]
    #[must_use]
    pub fn handed_over(&self) -> bool {
        self.chained || (!self.landed && matches!(self.retired_at, Some(at) if at < self.arrival()))
    }

    /// Whether this train has a head in the air at `at`.
    #[inline]
    #[must_use]
    pub fn in_flight_at(&self, at: Instant) -> bool {
        self.retired_at.is_none() && !self.chained && !self.landed && at < self.arrival()
    }

    /// The colour layer's alpha at `now`.
    #[must_use]
    #[cfg(test)]
    pub fn colour_alpha(&self, now: Instant) -> f32 {
        let age = ms_since(self.t0, now);
        let t = self.t_flight.as_secs_f32() * 1000.0;
        let base = if age <= t {
            1.0
        } else {
            crate::color_math::exp_f32(-(age - t) * (1.0 / TRAIN_COLOUR_TAU_MS))
        };
        base * self.retire_alpha(now)
    }

    #[inline]
    fn retire_alpha(&self, now: Instant) -> f32 {
        match self.retired_at {
            None => 1.0,
            Some(at) => {
                let u = ms_since(at, now) * (1.0 / FLIGHT_RETIRE_MS);
                crate::color_math::exp_f32(-u.max(0.0) * RETIRE_DECAY)
            }
        }
    }

    /// The instant this meteor's last pixel leaves the glass.
    #[inline]
    #[must_use]
    pub fn end(&self) -> Instant {
        match self.retired_at {
            Some(at) => at + FLIGHT_RETIRE_DUR,
            None => self.arrival() + FLIGHT_OFF_GLASS_DUR,
        }
    }
}

#[inline]
const fn dur_from_ms(ms: f32) -> Duration {
    Duration::from_nanos(((ms * (1.0 / 1000.0)) as f64 * 1_000_000_000.0).round() as u64)
}

const FLIGHT_RETIRE_DUR: Duration = dur_from_ms(FLIGHT_RETIRE_MS);
const FLIGHT_OFF_GLASS_DUR: Duration = dur_from_ms(FLIGHT_OFF_GLASS_MS);
const ROOT_SUCK_DUR: Duration = dur_from_ms(ROOT_SUCK_MS);
const PIN_DUR: Duration = dur_from_ms(PIN_MS);
const REDUCED_MOTION_FADE_DUR: Duration = dur_from_ms(REDUCED_MOTION_FADE_MS);

/// The landing pin.
#[derive(Clone, Copy, Debug)]
pub struct Pin {
    /// The arrival edge this pin was born on.
    pub at: Instant,
    /// The arc position its arms take, `tri(t_land)`.
    pub t: f32,
}

/// The landing starburst.
#[derive(Clone, Copy, Debug)]
pub struct Burst {
    /// The arrival edge.
    pub at: Instant,
    /// The landing's [`impact`].
    pub scale: f32,
    /// This burst's life in ms, [`ring_ms`].
    pub ms: f32,
    /// Live spikes, `<= BURST_N_MAX` ([`burst_n`]).
    pub n: u8,
    /// Each spike's unit screen direction.
    pub dir: [(f32, f32); BURST_N_MAX],
    /// Each spike's tip radius as a share of [`BURST_CORE_CH`], [`BURST_LEN_MIN`]`..=1.0`.
    pub len: [f32; BURST_N_MAX],
    /// The spikes' emit order, by descending length.
    pub order: [u8; BURST_N_MAX],
    /// The jets' unit screen directions, `k * 2 + side` (side 0 is leftward).
    pub jet_dir: [(f32, f32); JET_N_PER_SIDE * 2],
}

const JET_UNIT_DIR: [(f32, f32); JET_N_PER_SIDE] = {
    let mut dirs = [(0.0_f32, 0.0_f32); JET_N_PER_SIDE];
    let mut k = 0;
    while k < JET_N_PER_SIDE {
        let (st, ct) = crate::supernova::sin_cos_f32(JET_TILT_RAD[k]);
        let (rx, ry) = (ct, st * JET_RISE_SQUASH);
        let inv = 1.0 / crate::color_math::sqrt_f32(rx * rx + ry * ry).max(1e-6);
        dirs[k] = (rx * inv, ry * inv);
        k += 1;
    }
    dirs
};

#[inline]
#[must_use]
const fn mint_burst(seed: u32, at: Instant, cells: f32) -> Burst {
    let n = burst_n(cells);
    let step = std::f32::consts::TAU / n as f32;
    let phase = unit01(seed, 0x51) * step;
    let mut dir = [(0.0_f32, 0.0_f32); BURST_N_MAX];
    let mut len = [0.0_f32; BURST_N_MAX];
    let mut i = 0;
    while i < n {
        let salt = (i as u32).wrapping_mul(0x9E37_79B9) ^ 0x00B5;
        let a = phase + (i as f32 + BURST_JITTER * (unit01(seed, salt) - 0.5)) * step;
        let (sa, ca) = crate::supernova::sin_cos_f32(a);
        dir[i] = (ca, sa);
        let raw = BURST_LEN_MIN + (1.0 - BURST_LEN_MIN) * unit01(seed, salt ^ 0x1D);
        len[i] = if i.is_multiple_of(BURST_HERO_EVERY) {
            (raw * BURST_HERO_GAIN).min(1.0)
        } else {
            raw
        };
        i += 1;
    }
    let mut order = [0_u8; BURST_N_MAX];
    let mut idx = 0;
    while idx < BURST_N_MAX {
        order[idx] = idx as u8;
        idx += 1;
    }
    let mut i = 1;
    while i < n {
        let mut j = i;
        while j > 0 && len[order[j - 1] as usize] < len[order[j] as usize] {
            let tmp = order[j - 1];
            order[j - 1] = order[j];
            order[j] = tmp;
            j -= 1;
        }
        i += 1;
    }
    let mut jet_dir = [(0.0_f32, 0.0_f32); JET_N_PER_SIDE * 2];
    let mut k = 0;
    while k < JET_N_PER_SIDE {
        let (ux, uy) = JET_UNIT_DIR[k];
        let mut si = 0;
        while si < 2 {
            let side = if si == 0 { -1.0_f32 } else { 1.0 };
            let sign = if unit01(seed, 0x7A ^ si as u32) < 0.5 {
                -1.0_f32
            } else {
                1.0
            };
            jet_dir[k * 2 + si] = (ux * side, uy * sign);
            si += 1;
        }
        k += 1;
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

/// The landing fan.
#[derive(Clone, Copy, Debug)]
pub struct Fan {
    /// The arrival edge.
    pub at: Instant,
    /// Star count ([`fan_count`]), ≤ [`super::timing::FAN_MAX_N`].
    pub n: u8,
    /// Throw reach along the line, px.
    pub reach: f32,
    /// The seed that varies count, phase, size and throw.
    pub seed: u32,
    /// Whether this landing carries its gold m1 hero.
    pub hero: bool,
}

/// One spark's throw, hashed at the arrival edge.
#[derive(Clone, Copy, Debug)]
pub struct Spark {
    /// Birth velocity along the row, px/ms.
    pub vx: f32,
    /// Birth velocity down the glass, px/ms (negative is up).
    pub vy: f32,
    /// Life, ms.
    pub life: f32,
    /// The star's arm half-length while bright, px ([`SPARK_ARM_MIN_PX`]..=[`SPARK_ARM_MAX_PX`]).
    pub arm: u8,
}

impl Spark {
    /// An unthrown spark slot.
    pub const EMPTY: Self = Self {
        vx: 0.0,
        vy: 0.0,
        life: 0.0,
        arm: 0,
    };
}

impl Default for Spark {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// What the sky's probe said about the landing-row cells the flash and jets may light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkyMask {
    /// Blank cells of the landing row, `2·JET_PROBE_CELLS + 1` bits centred on the caret's column.
    pub row: u32,
}

impl Default for SkyMask {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl SkyMask {
    /// An empty mask (no probed blank cells).
    pub const EMPTY: Self = Self { row: 0 };

    #[inline]
    #[must_use]
    const fn probe(x: f32, y: f32, probe: &GlyphProbe, geom: Geom) -> Self {
        let cw = (geom.cw as f32).max(1.0);
        let py = y.round() as i32;
        let mut mask = Self::EMPTY;
        let mut k = -JET_PROBE_CELLS;
        while k <= JET_PROBE_CELLS {
            let px = (x + k as f32 * cw).round() as i32;
            if matches!(probe.at_px(px, py, geom), Some(false)) {
                mask.row |= 1_u32 << (k + JET_PROBE_CELLS);
            }
            k += 1;
        }
        mask
    }

    #[inline]
    #[must_use]
    const fn row_blank(self, k: i32) -> bool {
        if k < -JET_PROBE_CELLS || k > JET_PROBE_CELLS {
            return false;
        }
        self.row & (1_u32 << (k + JET_PROBE_CELLS)) != 0
    }

    #[inline]
    #[must_use]
    const fn flash_blank(self, k: i32) -> bool {
        self.row_blank(k)
    }
}

/// The marks the arrival edge mints together on one `Instant`.
#[derive(Clone, Copy, Debug)]
pub struct Landing {
    /// Always present — the pin is the caret.
    pub pin: Pin,
    /// The starburst.
    pub burst: Option<Burst>,
    /// A meteor impact licenses the flash.
    pub flash: bool,
    /// Always present; its composition varies.
    pub fan: Fan,
    /// Window-absolute X of the landing cell's centre, px.
    pub x: f32,
    /// Window-absolute Y of the landing cell's centre, px.
    pub y: f32,
    /// The landing's walk ([`LandingWalk`]), latched at the spawn edge.
    pub walk: LandingWalk,
    /// How many of [`Landing::spark`] this impact throws ([`SPARK_N_BASE`]).
    pub sparks: u8,
    /// The sparks' throws, the first [`Landing::sparks`] of them live.
    pub spark: [Spark; SPARK_N_MAX],
    /// The probe's verdict on the landing row ([`SkyMask`]).
    pub sky: Option<SkyMask>,
}

impl Landing {
    /// The band's walk through this landing ([`LandingWalk::at`]) `dx_cells` right of the landing.
    #[inline]
    #[must_use]
    pub const fn field(&self, dx_cells: f32) -> f32 {
        self.walk.at(dx_cells)
    }

    /// Whether this is a peel's landing (a Return's or the program's): no
    /// ring, so neither a flash nor a burst.
    #[inline]
    #[must_use]
    pub const fn peel(&self) -> bool {
        !self.flash && self.burst.is_none()
    }

    /// When the last landing pixel leaves the glass.
    #[inline]
    #[must_use]
    pub fn end(&self) -> Instant {
        const BASE_LIFE_MS: f32 = PIN_MS.max(FLASH_COLOUR_MS).max(SPARK_LIFE_MAX_MS);
        let burst_ms = match self.burst {
            Some(b) => b.ms,
            None => 0.0,
        };
        let life = BASE_LIFE_MS.max(burst_ms);
        self.pin.at + Duration::from_secs_f32(life * (1.0 / 1000.0))
    }

    /// A party landing: the celebration's bar fan at the caret with no meteor behind it.
    #[inline]
    #[must_use]
    pub const fn party(
        at: Instant,
        at_px: (f32, f32),
        n: u8,
        ring: bool,
        seed: u32,
        ch: f32,
        tint_t: f32,
    ) -> Self {
        let raw_n = n as usize;
        let clamped_n = if raw_n < 1 {
            1
        } else if raw_n > super::timing::FAN_MAX_N {
            super::timing::FAN_MAX_N
        } else {
            raw_n
        };
        let n = clamped_n as u8;
        let burst = if ring {
            Some(mint_burst(mix32(seed ^ 0xB0B5), at, JUMP_MIN_CELLS as f32))
        } else {
            None
        };
        Self {
            pin: Pin { at, t: tri(tint_t) },
            walk: LandingWalk::free(tint_t),
            burst,
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
            spark: [Spark::EMPTY; SPARK_N_MAX],
            sky: None,
        }
    }

    #[inline]
    #[must_use]
    fn static_end(&self) -> Instant {
        self.pin.at + FLIGHT_OFF_GLASS_DUR
    }

    #[inline]
    #[must_use]
    fn static_alpha(&self, now: Instant) -> f32 {
        let left = self
            .static_end()
            .saturating_duration_since(now)
            .as_secs_f32()
            * 1000.0;
        clamp01(left * (1.0 / REDUCED_MOTION_FADE_MS.max(1.0)))
    }

    /// The last instant any landing mark moves.
    #[inline]
    #[must_use]
    pub fn moving_until(&self) -> Instant {
        let mut ms = PIN_MS;
        if let Some(b) = self.burst {
            ms = ms.max(b.ms).max(FLASH_COLOUR_MS);
        }
        if self.sparks > 0 {
            let mut max_spark = 0.0_f32;
            let n = usize::from(self.sparks).min(SPARK_N_MAX);
            let mut i = 0;
            while i < n {
                let life = self.spark[i].life;
                if life > max_spark {
                    max_spark = life;
                }
                i += 1;
            }
            ms = ms.max(max_spark * SPARK_CULL_U);
        }
        self.pin.at + Duration::from_secs_f32(ms * (1.0 / 1000.0))
    }
}

const SPARK_CULL_U: f32 = spark_cull_u();

#[inline]
#[must_use]
const fn spark_cull_u() -> f32 {
    SPARK_HOLD_U + (1.0 - SPARK_HOLD_U) * (1.0 - crate::color_math::sqrt_f32(CHROMA_CULL_ALPHA))
}

/// What a spawn minted, handed back to `Engine`, which owns the companion impulse.
#[derive(Clone, Copy, Debug)]
pub struct Spawn {
    /// The spawn edge.
    pub t0: Instant,
    /// Flight duration `T`.
    pub t_flight: Duration,
    /// Travel direction.
    pub dir: Dir,
    /// Path length in cells, the integer the audio receives.
    pub cells: u16,
    /// The landing cell.
    pub landing: (u16, u16),
    /// Whether this is the full landing (burst, flash, sparks, gold hero).
    pub ring: bool,
    /// Whether this landing cues the five rain glints.
    pub rain: bool,
}

#[derive(Clone, Copy, Debug)]
enum Sow {
    Shed {
        at: Instant,
        spec: ShedSow,
    },
    Fan {
        at: Instant,
        spec: FanSow,
    },
    MiniFan {
        at: Instant,
        at_px: (f32, f32),
        seed: u32,
    },
}

/// The meteor producer: the live pool, its landings, and the station scratch.
#[derive(Clone, Debug)]
pub struct Meteors {
    /// The meteor pool: at most [`FLIGHT_MAX_LIVE`] heads in the air and [`METEOR_POOL`] total.
    pub live: Vec<Meteor>,
    /// The landing pool: one per arrived meteor, at most [`LANDING_POOL`].
    pub landings: Vec<Landing>,
    sown: Vec<Sow>,
    verts: Vec<RibbonVertex>,
    ord: u32,
    reduced: bool,
    hand_floor: Option<(u16, u16)>,
    /// The frame's lettered rows, which a train passes behind
    /// ([`KeepOut::behind_text`]).
    pub keep_out: KeepOut,
    /// Whether tests lay each train layer cut square without end easing.
    #[cfg(test)]
    pub(crate) ends_square: bool,
    /// Whether tests lay a train's fades as dims, on dark (looks6 N6's
    /// negative control) and on paper (looks7 M2's).
    #[cfg(test)]
    pub(crate) dims_dark_trains: bool,
    /// Whether tests lay a train's warm stops as its others, as they were:
    /// at their level on dark (looks6 round 3's negative control), and on
    /// paper bent through the darkened yellows as the caret's are (looks7
    /// M2's).
    #[cfg(test)]
    pub(crate) warm_stops_plain: bool,
    /// Whether tests blend a paper train across its bend's skip between
    /// stations, as it was (looks8 round 2's K10b negative control,
    /// [`step_over_train_skip`]).
    #[cfg(test)]
    pub(crate) train_skip_blended: bool,
    /// Whether tests lay a train across a lettered row's letters (the
    /// strike-through's negative control).
    #[cfg(test)]
    pub(crate) trains_over_text: bool,
    /// Whether tests lay a dark train into the letter guard's reach below a
    /// lettered row's letters (the brown stub's negative control).
    #[cfg(test)]
    pub(crate) trains_in_guard: bool,
    /// Whether tests keep a dark head's petals after it lands (the head fold's
    /// negative control).
    #[cfg(test)]
    pub(crate) petals_stay: bool,
    /// Whether tests ring a dark peel's head with its coma and lay its
    /// lights behind the letters' bodies alone, as they were (looks7 M1's
    /// negative control).
    #[cfg(test)]
    pub(crate) peel_coma: bool,
}

impl Default for Meteors {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Meteors {
    /// An empty pool, with every resident buffer reserved to its published depth.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            live: Vec::with_capacity(METEOR_POOL),
            landings: Vec::with_capacity(LANDING_POOL),
            sown: Vec::with_capacity(SOW_SCRATCH),
            verts: Vec::with_capacity(STATIONS_MAX + 2 * END_EASE_STATIONS),
            ord: 0,
            reduced: false,
            hand_floor: None,
            keep_out: KeepOut::EMPTY,
            #[cfg(test)]
            ends_square: false,
            #[cfg(test)]
            dims_dark_trains: false,
            #[cfg(test)]
            warm_stops_plain: false,
            #[cfg(test)]
            train_skip_blended: false,
            #[cfg(test)]
            trains_over_text: false,
            #[cfg(test)]
            trains_in_guard: false,
            #[cfg(test)]
            petals_stay: false,
            #[cfg(test)]
            peel_coma: false,
        }
    }

    /// Set the hand's floor.
    #[inline]
    pub const fn set_hand_floor(&mut self, floor: Option<(u16, u16)>) {
        self.hand_floor = floor;
    }

    /// The floor this pool holds, as last set by [`Meteors::set_hand_floor`].
    #[must_use]
    #[cfg(test)]
    pub const fn hand_floor_cell(&self) -> Option<(u16, u16)> {
        self.hand_floor
    }

    /// Ingest one engine event.
    #[inline]
    pub fn on_event(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>) -> Option<Spawn> {
        match *ev {
            Event::Move {
                from,
                to,
                licence,
                dir,
            } => self.on_move(from, to, licence, dir, at, ctx),
            Event::Focus(false) => {
                let mut i = 0;
                while i < self.live.len() {
                    if self.live[i].retired_at.is_none() {
                        self.live[i].retired_at = Some(at);
                    }
                    i += 1;
                }
                None
            }
            _ => None,
        }
    }

    /// Draw the meteors and their landings, first in the emit order.
    #[inline]
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.reduced = ctx.cfg.reduced_motion;
        let now = ctx.now;
        self.live.retain(|m| now < m.end());
        self.landings.retain(|l| now < l.end());
        if self.live.is_empty() && self.landings.is_empty() {
            return;
        }

        let mut verts = mem::take(&mut self.verts);
        let hand_floor = self.hand_floor;
        #[cfg(not(test))]
        let square = false;
        #[cfg(test)]
        let square = self.ends_square;
        #[cfg(not(test))]
        let clean = true;
        #[cfg(test)]
        let clean = !self.dims_dark_trains;
        #[cfg(not(test))]
        let warm = true;
        #[cfg(test)]
        let warm = !self.warm_stops_plain;
        #[cfg(not(test))]
        let stepped = true;
        #[cfg(test)]
        let stepped = !self.train_skip_blended;
        #[cfg(not(test))]
        let (behind, fold, skirts, peel_coma) = (true, true, true, false);
        #[cfg(test)]
        let (behind, fold, skirts, peel_coma) = (
            !self.trains_over_text,
            !self.petals_stay,
            !self.trains_in_guard,
            self.peel_coma,
        );
        let keep = &self.keep_out;
        let text = Behind::bodies(keep, behind);
        let train = if ctx.cfg.dark_theme && clean && skirts {
            text.guarded()
        } else {
            text
        };
        let (live, landings, sown) = (&mut self.live, &mut self.landings, &mut self.sown);

        let mut mi = 0;
        while mi < live.len() {
            let m = &mut live[mi];
            mi += 1;
            if let Some(f) = Flight::of(m, ctx) {
                draw_train(
                    &mut verts,
                    m,
                    &f,
                    ctx,
                    frame,
                    (square, clean, warm, stepped),
                    train,
                );
                // A dark peel (a Return's or the program's, which rings no
                // landing) is one tapering train: its head's coma, seven
                // lights orbiting it apart, read as floating dabs beside
                // the train and, the train hidden behind the echo and the
                // printed line, as dabs alone in the gap between them
                // (looks7 M1). Its core, laid where the train is, passes
                // behind the letters as the train does.
                let peel = clean && !m.ring && !peel_coma;
                let head = if peel { train } else { text };
                draw_head(m, &f, ctx, frame, (clean, fold, !peel), head);
                if m.retired_at.is_none() && !m.chained {
                    shed(m, &f, ctx, sown);
                }
            }
            if !m.landed && !m.chained && now >= m.arrival() && m.retired_at.is_none() {
                m.landed = true;
                if !on_glass(ctx.geom, m.x1, m.y1) {
                    continue;
                }
                let landing = mint_landing(m, ctx, hand_floor);
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
                if landings.len() >= LANDING_POOL {
                    landings.remove(0);
                }
                landings.push(landing);
            }
        }
        let halo_cap = frame.halos.len() + SPARK_HALO_CAP;
        let mut li = 0;
        while li < landings.len() {
            let l = &landings[li];
            draw_burst(l, ctx, frame);
            draw_flash(l, ctx, frame);
            draw_pin(l, ctx, frame);
            draw_sparks(l, ctx, frame, halo_cap);
            li += 1;
        }

        self.verts = verts;
    }

    /// Stage one bar fan at the caret for the sky and, on a ring bar, a landing.
    #[inline]
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

    /// Hand the decided births to the sky.
    #[inline]
    pub fn sow_into(&mut self, dust: &mut Stardust, ctx: &Ctx<'_>) {
        if self.landings.is_empty() && self.sown.is_empty() {
            return;
        }
        let mut li = 0;
        while li < self.landings.len() {
            let l = &mut self.landings[li];
            if l.sky.is_none() {
                l.sky = Some(SkyMask::probe(l.x, l.y, dust.probe(), ctx.geom));
            }
            li += 1;
        }
        let mut si = 0;
        while si < self.sown.len() {
            match self.sown[si] {
                Sow::Shed { at, spec } => dust.sow_shed(at, spec, ctx),
                Sow::Fan { at, spec } => dust.sow_fan(at, spec, ctx),
                Sow::MiniFan { at, at_px, seed } => dust.sow_mini_fan(at, at_px, seed, ctx),
            }
            si += 1;
        }
        self.sown.clear();
    }

    /// Meteors not on the 60 ms retire (`trail status` `meteors=`).
    #[inline]
    #[must_use]
    pub const fn live(&self) -> usize {
        let mut count = 0;
        let slice = self.live.as_slice();
        let mut i = 0;
        while i < slice.len() {
            if slice[i].retired_at.is_none() {
                count += 1;
            }
            i += 1;
        }
        count
    }

    /// True when nothing is in the air and no landing is finishing.
    #[must_use]
    #[cfg(test)]
    pub const fn at_rest(&self) -> bool {
        self.live.is_empty() && self.landings.is_empty()
    }

    /// The cadence law's brisk half.
    #[inline]
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        if self.reduced {
            return false;
        }
        let live = self.live.as_slice();
        let mut i = 0;
        while i < live.len() {
            let m = &live[i];
            let active = match m.retired_at {
                Some(_) => now < m.end(),
                None => now < m.arrival() + ROOT_SUCK_DUR,
            };
            if active {
                return true;
            }
            i += 1;
        }
        let landings = self.landings.as_slice();
        let mut j = 0;
        while j < landings.len() {
            if now < landings[j].moving_until() {
                return true;
            }
            j += 1;
        }
        false
    }

    #[inline]
    fn attack(&self, now: Instant) -> bool {
        if self.reduced {
            return false;
        }
        let live = self.live.as_slice();
        let mut i = 0;
        while i < live.len() {
            let m = &live[i];
            let active = match m.retired_at {
                Some(_) => now < m.end(),
                None => now < m.arrival() + PIN_DUR,
            };
            if active {
                return true;
            }
            i += 1;
        }
        let landings = self.landings.as_slice();
        let mut j = 0;
        while j < landings.len() {
            if now < landings[j].pin.at + PIN_DUR {
                return true;
            }
            j += 1;
        }
        false
    }

    /// The cadence law: the next instant a meteor changes what is on glass, `None` at rest.
    #[inline]
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        if self.live.is_empty() && self.landings.is_empty() {
            return None;
        }
        let mut cad = Cadence::at(now);
        if self.attack(now) {
            cad.brisk();
            return cad.take();
        } else if self.brisk(now) {
            cad.tail(0.0);
            return cad.take();
        }
        let live = self.live.as_slice();
        let mut i = 0;
        while i < live.len() {
            let m = &live[i];
            if self.reduced {
                let fade_from = m.end() - REDUCED_MOTION_FADE_DUR;
                if m.retired_at.is_none() && now < fade_from {
                    cad.tail_at(fade_from);
                } else {
                    cad.tail(0.0);
                }
            }
            cad.tail_at(m.end());
            i += 1;
        }
        let landings = self.landings.as_slice();
        let mut j = 0;
        while j < landings.len() {
            let l = &landings[j];
            if self.reduced {
                let fade_from = l.static_end() - REDUCED_MOTION_FADE_DUR;
                if now < fade_from {
                    cad.tail_at(fade_from);
                } else {
                    cad.tail(0.0);
                }
                cad.tail_at(l.static_end());
            } else {
                cad.tail_at(l.end());
            }
            j += 1;
        }
        cad.take()
    }

    /// Translate live geometry by a scroll.
    #[inline]
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        if rows == 0 {
            return;
        }
        if cell_h == 0 {
            self.live.clear();
            self.landings.clear();
            self.sown.clear();
            self.hand_floor = None;
            return;
        }
        if let Some((row, _)) = &mut self.hand_floor {
            match row.checked_sub(rows) {
                Some(r) => *row = r,
                None => self.hand_floor = None,
            }
        }
        let dy = f32::from(rows) * f32::from(cell_h);
        self.live.retain_mut(|m| {
            m.y0 -= dy;
            m.y1 -= dy;
            m.y0.max(m.y1) >= 0.0
        });
        self.landings.retain_mut(|l| {
            l.y -= dy;
            l.y >= 0.0
        });
        self.sown.retain_mut(|s| {
            let y = match s {
                Sow::Shed { spec, .. } => &mut spec.at_px.1,
                Sow::Fan { spec, .. } => &mut spec.at_px.1,
                Sow::MiniFan { at_px, .. } => &mut at_px.1,
            };
            *y -= dy;
            *y >= 0.0
        });
    }

    /// [`Meteors::translate_scroll`]'s row-band twin.
    #[inline]
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
            self.hand_floor = None;
            return;
        }
        if let Some((row, _)) = &mut self.hand_floor {
            match band_row(*row, top, bottom, delta) {
                Some(r) => *row = r,
                None => self.hand_floor = None,
            }
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
    #[inline]
    pub fn reset(&mut self) {
        self.live.clear();
        self.landings.clear();
        self.sown.clear();
        self.verts.clear();
        self.ord = 0;
        self.reduced = false;
        self.hand_floor = None;
    }

    #[inline]
    fn on_move(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        licence: Licence,
        dir: Dir,
        at: Instant,
        ctx: &Ctx<'_>,
    ) -> Option<Spawn> {
        if licence.is_echo() {
            return None;
        }
        let drow = i32::from(to.0) - i32::from(from.0);
        let dcol = i32::from(to.1) - i32::from(from.1);
        if drow == 0 {
            let n = dcol.unsigned_abs();
            if n < u32::from(JUMP_MIN_CELLS) {
                if n >= u32::from(MINI_FAN_MIN_CELLS) && n <= u32::from(MINI_FAN_MAX_CELLS) {
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
        let len = crate::color_math::hypot_f32(x1 - x0, y1 - y0);
        if len < 1.0 {
            return None;
        }
        let cells_n = (len / cw.max(1.0)).round().clamp(1.0, f32::from(u16::MAX)) as u16;
        let cells = f32::from(cells_n);
        let grade = (cells * (1.0 / GRADE_CELLS)).clamp(0.0, 1.0);
        let mom = clamp01(ctx.birth_disp);
        let w = ch * (W_BASE_CH + W_MOM_CH * mom) * (1.0 + W_GRADE_GAIN * grade);
        self.ord = self.ord.wrapping_add(1);
        let seed = seed_of(to.0, to.1, self.ord);

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
            t_land: if ctx.caret_t.is_finite() {
                ctx.caret_t
            } else {
                0.0
            },
            band: ctx.caret_walk,
            cells,
            dir,
            seed,
            shed: 0,
            landed: false,
            ring: !small,
        };

        let mut chained: Option<usize> = None;
        let chain_reach = CHAIN_CELLS * cw;
        let chain_reach_sq = chain_reach * chain_reach;
        let mut i = 0;
        while i < self.live.len() {
            let old = &mut self.live[i];
            let cur_i = i;
            i += 1;
            if old.retired_at.is_some()
                || old.chained
                || old.landed
                || old.dir != dir
                || at > old.arrival()
            {
                continue;
            }
            let (hx, hy) = head_at(old, at);
            let (dx, dy) = (x0 - hx, y0 - hy);
            if dx * dx + dy * dy <= chain_reach_sq {
                if at < old.arrival() {
                    let t_ms = (old.t_flight.as_secs_f32() * 1000.0).max(1.0);
                    let flown = enter_at_speed(ms_since(old.t0, at) / t_ms);
                    old.t_land -=
                        (1.0 - flown) * old.length() / cw * WALK_LAY_RATE * arc_gain(old.cells);
                    old.x1 = hx;
                    old.y1 = hy;
                    old.t_flight = at.saturating_duration_since(old.t0);
                }
                old.chained = true;
                chained = Some(cur_i);
                break;
            }
        }

        let corridor = CORRIDOR_CH * ch;
        let m_flat = m.y0 == m.y1;
        let mut i = 0;
        while i < self.live.len() {
            let old = &mut self.live[i];
            let cur_i = i;
            i += 1;
            if Some(cur_i) == chained || old.retired_at.is_some() {
                continue;
            }
            let near = seg_gap(
                (m.x0, m.y0),
                (m.x1, m.y1),
                (old.x0, old.y0),
                (old.x1, old.y1),
            ) <= corridor;
            let same_row = m_flat && old.y0 == old.y1 && m.y0 == old.y0 && at < old.arrival();
            if near || same_row {
                old.retired_at = Some(at);
            }
        }

        if self.live.len() >= METEOR_POOL {
            let mut victim: Option<(usize, Instant)> = None;
            let mut i = 0;
            while i < self.live.len() {
                let x = &self.live[i];
                if x.retired_at.is_some() {
                    let end = x.end();
                    if !matches!(victim, Some((_, best)) if end >= best) {
                        victim = Some((i, end));
                    }
                }
                i += 1;
            }
            self.live.remove(match victim {
                Some((i, _)) => i,
                None => 0,
            });
        }
        self.live.push(m);

        let mut unretired = self.live();
        while unretired > FLIGHT_MAX_LIVE {
            let mut fallback = None;
            let mut landed = None;
            let mut i = 0;
            while i < self.live.len() {
                let x = &self.live[i];
                if x.retired_at.is_none() {
                    if fallback.is_none() {
                        fallback = Some(i);
                    }
                    if !x.in_flight_at(at) {
                        landed = Some(i);
                        break;
                    }
                }
                i += 1;
            }
            let Some(i) = (match landed {
                Some(i) => Some(i),
                None => fallback,
            }) else {
                break;
            };
            self.live[i].retired_at = Some(at);
            unretired -= 1;
        }

        Some(Spawn {
            t0: at,
            t_flight: m.t_flight,
            dir,
            cells: cells_n,
            landing: to,
            ring: !small,
            rain: !small,
        })
    }
}

/// The least share of its light a dark train's colour keeps while it fades
/// along the train: below it the train narrows at that share instead of
/// dimming further, since a dimmed warm stop reads brown on a dark page.
const DARK_TRAIN_CLEAN_SHARE: f32 = 1.0;

/// A train's fade `k` of its light at half-width `half`, kept clean (a dark
/// train's light, a paper train's ink):
/// at or above `clean` it dims as asked; below, it keeps `clean` of its
/// light and narrows by `k / clean` instead, and once narrower than
/// [`DARK_TAPER_THREAD_PX`] it keeps that width and fades with its coverage
/// (a thread any thinner lands between pixel rows and flickers).
#[inline]
#[must_use]
const fn dark_clean_taper(k: f32, half: f32, clean: f32) -> (f32, f32) {
    if k >= clean {
        return (k, half);
    }
    const THREAD: f32 = DARK_TAPER_THREAD_PX * 0.5;
    if half <= THREAD {
        return (k, half);
    }
    let narrow = half * (k / clean).max(0.0);
    if narrow >= THREAD {
        (clean, narrow)
    } else {
        (clean * narrow * (1.0 / THREAD), THREAD)
    }
}

/// A dark train's station of light `k` and half-width `half` kept within
/// `room` px of its spine ([`Behind::room`]): narrowed to it at its level,
/// and narrower than [`DARK_TAPER_THREAD_PX`] faded with its coverage, as
/// [`dark_clean_taper`] lays a thread.
#[inline]
#[must_use]
const fn fit_room(k: f32, half: f32, room: f32) -> (f32, f32) {
    if half <= room {
        return (k, half);
    }
    const THREAD: f32 = DARK_TAPER_THREAD_PX * 0.5;
    if room >= THREAD {
        (k, room)
    } else {
        (k * (room * (1.0 / THREAD)).max(0.0), THREAD)
    }
}

/// How much brighter a dark train lays a warm stop than its others: at the
/// train's level a red, an orange or a yellow reads maroon, brown and olive
/// on a dark page, where a green, a blue or a violet still reads as its hue.
const DARK_WARM_TRAIN_GAIN: f32 = 1.8;

/// [`DARK_WARM_TRAIN_GAIN`]'s share for `stop`: whole from red through
/// yellow, easing out across yellow-green and across magenta.
#[inline]
#[must_use]
const fn dark_warm_gain(stop: u32) -> f32 {
    let (hue, sat, _) = crate::color_math::rgb2hsv(stop);
    let warm = if hue >= 300.0 {
        smoothstep01((hue - 300.0) * (1.0 / 45.0))
    } else {
        1.0 - smoothstep01((hue - 55.0) * (1.0 / 30.0))
    };
    1.0 + (DARK_WARM_TRAIN_GAIN - 1.0) * warm * sat.clamp(0.0, 1.0)
}

/// The narrowest a dark train's clean taper draws before it fades instead.
const DARK_TAPER_THREAD_PX: f32 = 2.0;

/// A paper train's `ink` where its thread fades to `fade` of it: lifted
/// toward its hue's full value as it goes. A darkened gold laid at a share
/// over the paper is a tint of ochre, tan and khaki (looks7 M2: `c79b58`
/// where the stick's thread faded out); a tint of its own hue at full value
/// reads a pale gold.
#[inline]
#[must_use]
const fn paper_thread_ink(ink: u32, fade: f32) -> u32 {
    if fade >= 1.0 {
        return ink;
    }
    let (h, s, v) = crate::color_math::rgb2hsv(ink);
    crate::color_math::hsv2rgb(
        h,
        s,
        v + (1.0 - v) * crate::color_math::sqrt_f32(1.0 - fade.max(0.0)),
    )
}

const WHITE_PRINTED_SHARE: f32 = 0.25;
const WHITE_PRINTED_NEAR: f32 = 0.25;
const WHITE_PRINTED_FAR: f32 = 0.85;

#[inline]
#[must_use]
const fn white_printed_share(dy: f32, rows_off: f32, ch: f32) -> f32 {
    if rows_off < 0.5 * ch {
        return 1.0;
    }
    let ch = ch.max(1.0);
    let u = (dy.abs() / ch - WHITE_PRINTED_NEAR) * (1.0 / (WHITE_PRINTED_FAR - WHITE_PRINTED_NEAR));
    WHITE_PRINTED_SHARE + (1.0 - WHITE_PRINTED_SHARE) * smoothstep01(u.clamp(0.0, 1.0))
}

#[inline]
#[must_use]
const fn dark_petal_rgb(stop: u32) -> u32 {
    let (h, _, _) = crate::color_math::rgb2hsv(stop & 0x00FF_FFFF);
    if h >= 40.0 && h <= 100.0 {
        night_rgb(stop)
    } else {
        stop
    }
}

/// A dark head's light for spectrum `stop`: a warm stop that dims to brown
/// (orange) or olive (yellow, lime) walks the night arc ([`night_rgb`]),
/// and every other stop is itself.
#[inline]
#[must_use]
const fn dark_head_rgb(stop: u32) -> u32 {
    let (h, _, _) = crate::color_math::rgb2hsv(stop & 0x00FF_FFFF);
    if h >= 15.0 && h <= 100.0 {
        night_rgb(stop)
    } else {
        stop
    }
}

const DARK_HEAD_STOP_LUT: [u32; COMA_PETALS] = {
    let mut lut = [0u32; COMA_PETALS];
    let mut i = 0;
    while i < COMA_PETALS {
        lut[i] = dark_head_rgb(spectrum_stop(i));
        i += 1;
    }
    lut
};

const DARK_PETAL_STOP_LUT: [u32; COMA_PETALS] = {
    let mut lut = [0u32; COMA_PETALS];
    let mut i = 0;
    while i < COMA_PETALS {
        lut[i] = dark_petal_rgb(spectrum_stop(i));
        i += 1;
    }
    lut
};

#[inline]
#[must_use]
const fn dark_head_stop_rgb(i: usize) -> u32 {
    let idx = if i < COMA_PETALS - 1 {
        i
    } else {
        COMA_PETALS - 1
    };
    DARK_HEAD_STOP_LUT[idx]
}

#[inline]
#[must_use]
const fn dark_petal_stop_rgb(i: usize) -> u32 {
    let idx = if i < COMA_PETALS - 1 {
        i
    } else {
        COMA_PETALS - 1
    };
    DARK_PETAL_STOP_LUT[idx]
}

struct Flight {
    l: f32,
    unit: (f32, f32),
    head: (f32, f32),
    span: f32,
    to_go: f32,
    u: f32,
    w: f32,
    head_cov: f32,
    colour_cov: f32,
    fade_w: f32,
    fade_c: f32,
    shoulder: f32,
    white_shoulder: f32,
    white_fall: f32,
    white_closed: bool,
    stretch: f32,
    flash: f32,
    stride: f32,
    axis: Axis,
    headless: bool,
    /// How long ago the head arrived, ms (0 in flight).
    after_ms: f32,
}

/// The frame's lettered rows a train passes behind ([`KeepOut::behind_text`]),
/// whether it does (`on`), and how much of each row: its letters' bodies
/// ([`TRAIN_TEXT_BAND`]), or on a dark page the train's whole reach of the
/// letter guard ([`DARK_TRAIN_TEXT_BAND`], [`DARK_TRAIN_TEXT_PAD_PX`]).
#[derive(Clone, Copy)]
struct Behind<'a> {
    keep: &'a KeepOut,
    on: bool,
    band: (f32, f32),
    pad: f32,
    /// How far past a row's last letter its span reaches ([`KeepOut::past`]).
    past: f32,
}

impl<'a> Behind<'a> {
    /// The letters' bodies of `keep`'s rows, when `on`.
    #[inline]
    const fn bodies(keep: &'a KeepOut, on: bool) -> Self {
        Self {
            keep,
            on,
            band: TRAIN_TEXT_BAND,
            pad: 0.0,
            past: 0.0,
        }
    }

    /// The same rows, kept clear of the letter guard's reach: their whole
    /// height and [`DARK_TRAIN_TEXT_PAD_PX`] round it.
    #[inline]
    const fn guarded(self) -> Self {
        Self {
            band: DARK_TRAIN_TEXT_BAND,
            pad: DARK_TRAIN_TEXT_PAD_PX,
            past: self.keep.past() as f32,
            ..self
        }
    }

    /// How many px a dark train's station centred at `(x, y)` may reach
    /// before its light enters a guarded row's hidden zone, bounded above by
    /// `limit`: `limit` for the letters' bodies alone ([`Self::bodies`]) or
    /// clear of every row within `limit`, 0 inside one. Narrowed to it, a
    /// train tapers to a point where it meets the zone rather than being
    /// sliced flat along it.
    #[inline]
    const fn room(self, x: f32, y: f32, limit: f32) -> f32 {
        let rows = self.keep.rows();
        if !self.on || self.pad <= 0.0 || rows.is_empty() || limit <= 0.0 {
            return limit;
        }
        let limit_sq = limit * limit;
        let mut min_sq = limit_sq;
        let (pad, past) = (self.pad, self.past);
        let (band0, band1) = self.band;
        let mut idx = 0;
        while idx < rows.len() {
            let (x0, x1, y0, y1) = rows[idx];
            idx += 1;
            let (bx0, bx1) = (x0 as f32 - pad, x1 as f32 + past + pad);
            let dx = (bx0 - x).max(x - bx1).max(0.0);
            let dx_sq = dx * dx;
            if dx_sq >= min_sq {
                continue;
            }
            let y0_f = y0 as f32;
            let ch = (y1 - y0) as f32;
            let by0 = y0_f + band0 * ch - pad;
            let by1 = y0_f + band1 * ch + pad;
            let dy = (by0 - y).max(y - by1).max(0.0);
            let d_sq = dx_sq + dy * dy;
            if d_sq <= 0.0 {
                return 0.0;
            }
            min_sq = min_sq.min(d_sq);
        }
        if min_sq >= limit_sq {
            limit
        } else {
            crate::color_math::sqrt_f32(min_sq)
        }
    }

    /// How much of a light at `(x, y)` shows in front of the letters.
    #[inline]
    #[must_use]
    const fn at(self, x: f32, y: f32) -> f32 {
        if self.on {
            self.keep.behind_text(
                (x, y),
                self.band,
                (self.pad, self.past),
                TRAIN_TEXT_FEATHER_PX,
            )
        } else {
            1.0
        }
    }

    /// The rows `z0..z1` of a light over columns `x0..x1` and rows
    /// `y0..y1` that a lettered row's band touches, its eased edges and all.
    #[inline]
    const fn zone(self, (x0, x1): (i32, i32), (y0, y1): (i32, i32)) -> Option<(i32, i32)> {
        let rows = self.keep.rows();
        if !self.on || rows.is_empty() {
            return None;
        }
        let feather = TRAIN_TEXT_FEATHER_PX;
        let reach = feather.ceil() as i32;
        let pad = self.pad.ceil() as i32;
        let past = self.past.ceil() as i32;
        let pad_reach = pad + reach;
        let min_x = x0 - past - pad;
        let max_x = x1 + pad;
        let min_y = y0 - pad_reach;
        let max_y = y1 + pad_reach;
        let (band0, band1) = self.band;
        let mut zone: Option<(i32, i32)> = None;
        let mut idx = 0;
        while idx < rows.len() {
            let (rx0, rx1, ry0, ry1) = rows[idx];
            idx += 1;
            if max_x <= rx0 || min_x >= rx1 || max_y <= ry0 || min_y >= ry1 {
                continue;
            }
            let ch = (ry1 - ry0) as f32;
            let b0 = (ry0 as f32 + band0 * ch).floor() as i32 - pad_reach;
            let b1 = (ry0 as f32 + band1 * ch).ceil() as i32 + pad_reach;
            let a = if b0 > y0 { b0 } else { y0 };
            let b = if b1 < y1 { b1 } else { y1 };
            if a < b {
                zone = Some(match zone {
                    Some((p, q)) => (if p < a { p } else { a }, if q > b { q } else { b }),
                    None => (a, b),
                });
            }
        }
        zone
    }

    /// Cuts the quads from `from` on out of the lettered rows' bands, since a
    /// station centred clear of a band can still reach into it: each quad's
    /// rows in a band's reach are laid a pixel row at a time at the light
    /// [`Self::at`] leaves them (none inside, eased at the edges).
    #[inline]
    fn cut_quads(self, quads: &mut Vec<GlowQuad>, from: usize) {
        if !self.on || self.keep.rows().is_empty() || from >= quads.len() {
            return;
        }
        #[inline]
        const fn scale_quad(q: &mut GlowQuad, k: f32) {
            let a = (k * 255.0).round().clamp(0.0, 255.0) as u8;
            q.color = premul_rgb(q.color, a);
            q.color2 = premul_rgb(q.color2, a);
            q.alpha = (q.alpha as f32 * k).round() as u8;
            q.alpha2 = (q.alpha2 as f32 * k).round() as u8;
        }
        let n0 = quads.len();
        let mut any_zero = false;
        let mut i = from;
        while i < n0 {
            let q = quads[i];
            let cur_i = i;
            i += 1;
            let (x0, y0) = (i32::from(q.x), i32::from(q.y));
            let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
            let Some((z0, z1)) = self.zone((x0, x1), (y0, y1)) else {
                continue;
            };
            let cx = 0.5 * (x0 + x1) as f32;
            let mut y = z0;
            while y < z1 {
                let k = self.at(cx, y as f32 + 0.5);
                if k * 255.0 >= 1.0 {
                    let mut piece = q;
                    (piece.y, piece.h) = (y as u16, 1);
                    if k < 1.0 {
                        scale_quad(&mut piece, k);
                    }
                    quads.push(piece);
                }
                y += 1;
            }
            if z1 < y1 {
                let mut below = q;
                (below.y, below.h) = (z1 as u16, (y1 - z1) as u16);
                quads.push(below);
            }
            if z0 > y0 {
                quads[cur_i].h = (z0 - y0) as u16;
            } else {
                quads[cur_i].w = 0;
                any_zero = true;
            }
        }
        if any_zero {
            let mut kept = from;
            let mut i = from;
            while i < quads.len() {
                if quads[i].w > 0 {
                    if kept != i {
                        quads[kept] = quads[i];
                    }
                    kept += 1;
                }
                i += 1;
            }
            quads.truncate(kept);
        }
    }

    /// [`Self::cut_quads`] for the ramps laid since `from`: each lit row in a
    /// band's reach keeps the light [`Self::at`] leaves it at the slab's
    /// centre column, as the quad it stands for is cut (a row left under a
    /// 255th of its light is no light). A train's slabs never overlap, so
    /// cutting a row in place draws what laying its piece last drew. A row
    /// the cut leaves lit but of no colour is the dark piece the quad's cut
    /// lays, laid into `quads` as that piece: it draws nothing, and every
    /// count of the stream counts it as the legacy one does.
    #[inline]
    fn cut_ramps(self, (quads, ramps): (&mut Vec<GlowQuad>, &mut GlowRamps), from: RampMark) {
        #[inline]
        const fn scale_end(e: GlowEnd, cov: u8, s: f32) -> GlowEnd {
            glow_end(
                premul_rgb(end_color(e), cov),
                (end_alpha(e) as f32 * s).round() as u8,
            )
        }
        if !self.on || self.keep.rows().is_empty() {
            return;
        }
        let k0 = from.items();
        let mut i = k0;
        while i < ramps.items.len() {
            let r = ramps.items[i];
            i += 1;
            debug_assert_eq!(r.joined, 0, "a train lays rows, no run: {r:?}");
            let (x0, y0) = (i32::from(r.x), i32::from(r.y));
            let (x1, y1) = (x0 + i32::from(r.w), y0 + i32::from(r.h));
            if self.zone((x0, x1), (y0, y1)).is_none() {
                continue;
            }
            let cx = 0.5 * (x0 + x1) as f32;
            let mut row = 0;
            while row < r.h {
                let y = y0 + i32::from(row);
                let k = r.ink as usize + 2 * usize::from(row);
                row += 1;
                let (a, b) = (ramps.ends[k], ramps.ends[k + 1]);
                if a | b == 0 || self.zone((x0, x1), (y, y + 1)).is_none() {
                    continue;
                }
                let s = self.at(cx, y as f32 + 0.5);
                let (a, b) = if s * 255.0 < 1.0 {
                    (0, 0)
                } else if s < 1.0 {
                    let cov = (s * 255.0).round().clamp(0.0, 255.0) as u8;
                    (scale_end(a, cov, s), scale_end(b, cov, s))
                } else {
                    continue;
                };
                ramps.ends[k] = a;
                ramps.ends[k + 1] = b;
                if a | b == 0 {
                    ramps.lit -= 1;
                    if s * 255.0 >= 1.0 {
                        quads.push(GlowQuad {
                            row: r.row,
                            x: r.x,
                            y: y as u16,
                            w: r.w,
                            h: 1,
                            ..GlowQuad::default()
                        });
                    }
                }
            }
        }
    }

    /// [`Self::cut_quads`] for the head's lights: a halo's rect splits
    /// losslessly, its falloff held by its centre and radii.
    #[inline]
    fn cut_halos(self, halos: &mut Vec<RainHalo>, from: usize) {
        if !self.on || self.keep.rows().is_empty() || from >= halos.len() {
            return;
        }
        let n0 = halos.len();
        let mut any_zero = false;
        let mut i = from;
        while i < n0 {
            let h = halos[i];
            let cur_i = i;
            i += 1;
            let (x0, y0) = (i32::from(h.x), i32::from(h.y));
            let (x1, y1) = (x0 + i32::from(h.w), y0 + i32::from(h.h));
            let Some((z0, z1)) = self.zone((x0, x1), (y0, y1)) else {
                continue;
            };
            let cx = f32::from(h.cx);
            let mut y = z0;
            while y < z1 {
                let k = self.at(cx, y as f32 + 0.5);
                if k * 255.0 >= 1.0 {
                    let mut piece = h;
                    (piece.y, piece.h) = (y as u16, 1);
                    if k < 1.0 {
                        piece.color = premul_rgb(h.color, (k * 255.0).round() as u8);
                    }
                    halos.push(piece);
                }
                y += 1;
            }
            if z1 < y1 {
                let mut below = h;
                (below.y, below.h) = (z1 as u16, (y1 - z1) as u16);
                halos.push(below);
            }
            if z0 > y0 {
                halos[cur_i].h = (z0 - y0) as u16;
            } else {
                halos[cur_i].w = 0;
                any_zero = true;
            }
        }
        if any_zero {
            let mut kept = from;
            let mut i = from;
            while i < halos.len() {
                if halos[i].w > 0 {
                    if kept != i {
                        halos[kept] = halos[i];
                    }
                    kept += 1;
                }
                i += 1;
            }
            halos.truncate(kept);
        }
    }
}

/// The share of a lettered row's height a train passes behind: its
/// letters' bodies, from just under the row's top to the baseline, so a
/// crisp thread never reads as a strike-through. Below the baseline it
/// shows again.
const TRAIN_TEXT_BAND: (f32, f32) = (0.06, 0.80);

/// How many px a train eases out over at a lettered row's band.
const TRAIN_TEXT_FEATHER_PX: f32 = 3.0;

/// The share of a lettered row's height a dark train passes behind: all of
/// it. Below the letters' bodies the letter guard still judges the row's
/// cells and would cut the train to a share of its light, and a cut red,
/// orange or yellow reads maroon, brown or olive on a dark page. A dark
/// train is laid clean or not at all ([`dark_clean_taper`]).
const DARK_TRAIN_TEXT_BAND: (f32, f32) = (0.0, 1.0);

/// How many px round a lettered row's span a dark train stays hidden: the
/// letter guard's ring, which it judges a cell's light over, and the
/// train's own feather, so the eased edge lies clear of it too.
const DARK_TRAIN_TEXT_PAD_PX: f32 =
    crate::cursor_glow::LETTER_GUARD_RING as f32 + TRAIN_TEXT_FEATHER_PX;

/// How long a dark head's petals and core take to fold into the landing
/// once the head arrives, ms; left standing they read as stray specks
/// against the caret's left edge.
const HEAD_FOLD_MS: f32 = 50.0;

impl Flight {
    #[inline]
    fn of(m: &Meteor, ctx: &Ctx<'_>) -> Option<Self> {
        let (dx, dy) = m.delta();
        let l = crate::color_math::hypot_f32(dx, dy);
        if !l.is_finite() || l < 1.0 {
            return None;
        }
        let inv_l = 1.0 / l;
        let unit = (dx * inv_l, dy * inv_l);
        let t_ms = (m.t_flight.as_secs_f32() * 1000.0).max(1.0);
        let inv_t_ms = 1.0 / t_ms;
        let age = ms_since(m.t0, ctx.now);
        let after = (age - t_ms).max(0.0);
        let reduced = ctx.cfg.reduced_motion;

        let (u, alpha_w, alpha_c) = if reduced {
            let life = t_ms + FLIGHT_OFF_GLASS_MS;
            let a = REDUCED_MOTION_ALPHA
                * clamp01((life - age) * (1.0 / REDUCED_MOTION_FADE_MS.max(1.0)));
            (1.0, a, a)
        } else {
            let (alpha_w, alpha_c) = if after <= 0.0 {
                (1.0, 1.0)
            } else {
                (
                    crate::color_math::exp_f32(-after * (1.0 / TRAIN_WHITE_TAU_MS)),
                    crate::color_math::exp_f32(-after * (1.0 / TRAIN_COLOUR_TAU_MS)),
                )
            };
            (clamp01(age * inv_t_ms), alpha_w, alpha_c)
        };
        let retire = m.retire_alpha(ctx.now);
        let (u, p, root) = if reduced {
            (1.0, 1.0, 0.0)
        } else if let Some(at) = m.retired_at {
            let age_at = ms_since(m.t0, at);
            let u_at = clamp01(age_at * inv_t_ms);
            let p_at = enter_at_speed(u_at);
            let after_at = (age_at - t_ms).max(0.0);
            let root_at = if after_at <= 0.0 {
                0.0
            } else {
                suck_in(after_at * (1.0 / ROOT_SUCK_MS))
            };
            let r = suck_in(ms_since(at, ctx.now) * (1.0 / FLIGHT_RETIRE_MS));
            (u_at, p_at, root_at + (p_at - root_at) * r)
        } else {
            let root = if after <= 0.0 {
                0.0
            } else {
                suck_in(after * (1.0 / ROOT_SUCK_MS))
            };
            (u, enter_at_speed(u), root)
        };
        let span = ((p - root) * l).max(0.0);
        if span < 1.0 {
            return None;
        }
        let to_go = ((1.0 - p) * l).max(0.0);

        let head = (m.x0 + dx * p, m.y0 + dy * p);
        let thin = 1.0 - (1.0 - THICKNESS_END_SHARE) * clamp01(after * (1.0 / ROOT_SUCK_MS));
        let cw = (ctx.geom.cw as f32).max(1.0);

        let travel = head_speed(l, t_ms, u) * SHOULDER_FRAME_MS + 2.0;
        let shoulder = (SHOULDER_SHARE * l)
            .max(travel.clamp(SHOULDER_MIN_CELLS * cw, SHOULDER_MAX_CELLS * cw));

        let close = if reduced {
            0.0
        } else {
            suck_in(after * (1.0 / WHITE_CLOSE_MS))
        };
        let open = 1.0 - close;
        let white_shoulder = shoulder * open;
        let white_fall = (WHITE_FALLOFF_SHARE * l * open).max(1.0);

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
            stretch: if u >= 1.0 {
                1.0
            } else {
                1.0 + NUCLEUS_STRETCH_GAIN
                    * crate::color_math::powf_pos_f32(1.0 - u, FLIGHT_ENTER_EXP - 1.0)
            },
            flash: if reduced { 1.0 } else { flash_scale(after) },
            stride: (l * (1.0 / STATIONS_MAX as f32)).max(STATION_STRIDE_MIN_PX),
            axis: Axis::of(dx, dy),
            headless: m.handed_over(),
            after_ms: after,
        })
    }

    #[inline]
    const fn step(&self) -> usize {
        let s = self.stride.round().max(1.0) as usize;
        if s > 1 { s } else { 1 }
    }

    #[inline]
    const fn stations(&self, span: f32) -> usize {
        let s = ((span / self.stride).ceil() as usize) + 1;
        if s < 2 {
            2
        } else if s > STATIONS_MAX {
            STATIONS_MAX
        } else {
            s
        }
    }

    #[inline]
    const fn head_travel_share(&self) -> f32 {
        clamp01(enter_at_speed(self.u))
    }

    #[inline]
    const fn width_at(&self, s: f32) -> f32 {
        if s <= 0.0 {
            return self.w;
        }
        self.w
            * (WIDTH_FLOOR_SHARE
                + WIDTH_SPAN_SHARE
                    * crate::color_math::exp_f32(-s / (WIDTH_FALLOFF_SHARE * self.l)))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layer {
    Colour,
    White,
}

#[inline]
fn draw_train(
    verts: &mut Vec<RibbonVertex>,
    m: &Meteor,
    f: &Flight,
    ctx: &Ctx<'_>,
    frame: &mut Frame<'_>,
    (square, clean, warm, stepped): (bool, bool, bool, bool),
    text: Behind<'_>,
) {
    let clip = ctx.geom.beam_clip();
    let step = f.step();
    let light = !ctx.cfg.dark_theme;

    build_stations(
        verts,
        m,
        f,
        ctx,
        Layer::Colour,
        (square, clean, warm, stepped),
        text,
    );
    let blend = if light {
        GlowBlend::Over
    } else {
        GlowBlend::Add
    };
    rasterize(
        &mut frame.under_sink(),
        clip,
        verts,
        (f.axis, step),
        UNDER_QUAD_CAP,
        blend,
        text,
    );

    if light {
        return;
    }
    build_stations(
        verts,
        m,
        f,
        ctx,
        Layer::White,
        (square, clean, warm, stepped),
        text,
    );
    rasterize(
        &mut frame.out_sink(),
        clip,
        verts,
        (f.axis, step),
        WHITE_QUAD_CAP,
        GlowBlend::Add,
        text,
    );
}

/// Lay one train into `out` within `cap` lights of what it holds, then take
/// it out of the lettered rows' bands ([`Behind::cut_quads`]): an x-major
/// train's slabs as ramps, a y-major one's column strips as quads.
#[inline]
fn rasterize(
    out: &mut RampSink<'_>,
    clip: BeamClip,
    verts: &[RibbonVertex],
    (axis, step): (Axis, usize),
    cap: usize,
    blend: GlowBlend,
    text: Behind<'_>,
) {
    let (from, ramps_from) = out.mark();
    match axis {
        Axis::Horizontal => {
            let cap = out.laid() + cap;
            ribbon_beam_into(out, clip, verts, 1.0, step, cap, blend);
        }
        Axis::Vertical => {
            let cap = out.quads().len() + cap;
            ribbon_beam_v(out.quads(), clip, verts, 1.0, step, cap, blend);
        }
    }
    text.cut_quads(out.quads(), from);
    text.cut_ramps(out.stream(), ramps_from);
}

#[inline]
fn build_stations(
    verts: &mut Vec<RibbonVertex>,
    m: &Meteor,
    f: &Flight,
    ctx: &Ctx<'_>,
    layer: Layer,
    (square, clean, warm, stepped): (bool, bool, bool, bool),
    text: Behind<'_>,
) {
    verts.clear();
    let light = !ctx.cfg.dark_theme;
    let cw = (ctx.geom.cw as f32).max(1.0);
    let ch = ctx.geom.ch as f32;

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
    if alpha < CHROMA_CULL_ALPHA {
        return;
    }
    let rail = if light && f.axis == Axis::Horizontal {
        LIGHT_RAIL_CH * ch
    } else {
        0.0
    };

    let ease_head = (END_EASE_HEAD_CELLS * cw).min(END_EASE_SPAN_SHARE * span);
    let ease_tail = (END_EASE_TAIL_CELLS * cw).min(END_EASE_SPAN_SHARE * span);
    let inv_ease_head = 1.0 / ease_head.max(1e-3);
    let inv_ease_tail = 1.0 / ease_tail.max(1e-3);
    #[inline]
    const fn station_ends(
        s: f32,
        square: bool,
        span: f32,
        inv_ease_head: f32,
        inv_ease_tail: f32,
    ) -> f32 {
        if square {
            return 1.0;
        }
        smoothstep01(s * inv_ease_head) * smoothstep01((span - s) * inv_ease_tail)
    }
    // A dark train too short to reach full light between its eased ends
    // (a Return's first frames) is drawn at full light in its middle: dimmed
    // whole it reads as a grey and brown smudge.
    let ends_peak = if light || square || !clean || ease_head + ease_tail <= span * (30.0 / 32.0) {
        1.0
    } else {
        let mut peak = 1e-3_f32;
        let mut i = 0;
        while i <= 32 {
            peak = peak.max(station_ends(
                (span * i as f32) * (1.0 / 32.0),
                square,
                span,
                inv_ease_head,
                inv_ease_tail,
            ));
            i += 1;
        }
        peak
    };
    let inv_ends_peak = 1.0 / ends_peak;
    let inv_colour_fall = 1.0 / (COLOUR_FALLOFF_SHARE * f.l);
    let inv_white_fall = 1.0 / f.white_fall;
    let inv_white_shoulder = 1.0 / f.white_shoulder.max(1.0);
    let fine = END_EASE_STATIONS as f32;
    let inv_fine = 1.0 / fine;
    let inv_body_n = 1.0 / ((n - 1) as f32);
    let tail_start = span - ease_tail;
    let light_cap = if rail > 0.0 {
        LIGHT_RAIL_ALPHA_CAP
    } else {
        LIGHT_ALPHA_CAP
    };
    let light_peak = (f.colour_cov * LIGHT_INK_GAIN).min(light_cap);
    let rows_off = (m.y1 - m.y0).abs();
    let gain = arc_gain(m.cells);
    let lay_step = (1.0 / cw) * WALK_LAY_RATE * gain;
    let mut last_paper_rgb: Option<((u32, u32), u32)> = None;
    let mut last_warm_gain: Option<(u32, f32)> = None;
    let mut phase = 0u8;
    let mut idx = 0usize;
    loop {
        let s = match phase {
            0 => {
                if idx < END_EASE_STATIONS {
                    let s = ease_head * (idx as f32) * inv_fine;
                    idx += 1;
                    s
                } else {
                    phase = 1;
                    idx = 0;
                    continue;
                }
            }
            1 => {
                if idx < n {
                    let s = span * (idx as f32) * inv_body_n;
                    idx += 1;
                    if s < ease_head || s > tail_start {
                        continue;
                    }
                    s
                } else {
                    phase = 2;
                    idx = 1;
                    continue;
                }
            }
            _ => {
                if idx <= END_EASE_STATIONS {
                    let s = span - ease_tail * (fine - idx as f32) * inv_fine;
                    idx += 1;
                    s
                } else {
                    break;
                }
            }
        };
        let end_ease = station_ends(s, square, span, inv_ease_head, inv_ease_tail);
        if end_ease <= 0.0 {
            continue;
        }
        let px = f.head.0 - f.unit.0 * s;
        let py = f.head.1 - f.unit.1 * s + rail;
        // Across a lettered row the train passes behind the letters.
        let shows = text.at(px, py);
        if shows <= 0.0 {
            continue;
        }

        let (color, cov, half) = match layer {
            Layer::Colour => {
                if light {
                    let ink = if s <= f.shoulder {
                        light_peak
                    } else {
                        light_peak * crate::color_math::exp_f32(-(s - f.shoulder) * inv_colour_fall)
                    };
                    let half = f.width_at(s) * 0.5 * LIGHT_RAIL_SQUASH;
                    // The paper train's fades along it, in time and at its
                    // ends narrow it at its whole ink rather than thinning
                    // its ink over the page: a darkened amber laid thin
                    // over the paper reads tan and khaki (`b5a053`,
                    // `cf9859`, `d3a46b`), a red-orange coral, where at its
                    // whole ink it reads gold (looks7 M2). Its width eases
                    // along it with the fades, one stick to a point.
                    let (cov, half, fade) = if square || !clean {
                        (ink * alpha * end_ease * shows, half, 1.0)
                    } else {
                        let (k, half) = dark_clean_taper(
                            ink / light_peak.max(1.0) * alpha * end_ease,
                            half,
                            1.0,
                        );
                        (light_peak * k * shows, half, k)
                    };
                    if cov < 1.0 {
                        continue;
                    }
                    let stop = spectrum(walk_pace(tri(m.t_land - (f.to_go + s) * lay_step)));
                    let key = (
                        stop,
                        if rail > 0.0 && warm {
                            fade.to_bits()
                        } else {
                            0
                        },
                    );
                    let rgb = match last_paper_rgb {
                        Some((prev, cached)) if prev == key => cached,
                        _ => {
                            let solved = if rail > 0.0 && warm {
                                paper_thread_ink(paper_train_ink(stop, ctx.cfg.theme_bg), fade)
                            } else if rail > 0.0 {
                                paper_caret_ink(stop, ctx.cfg.theme_bg)
                            } else {
                                paper_ink(
                                    stop,
                                    ctx.cfg.theme_fg,
                                    ctx.cfg.theme_bg,
                                    InkRole::OverText,
                                )
                            };
                            last_paper_rgb = Some((key, solved));
                            solved
                        }
                    };
                    (rgb, cov, half)
                } else {
                    // The dark train's fades along it and in time narrow it
                    // at a clean level rather than dimming its hue toward
                    // brown (looks6 N6); its eased ends keep their ease.
                    let exp_fall = if s <= 0.0 {
                        1.0
                    } else {
                        crate::color_math::exp_f32(-s * inv_colour_fall)
                    };
                    let k = exp_fall * alpha * shows;
                    let half = f.width_at(s) * 0.5;
                    let (k, half) = if square || !clean {
                        (k, half)
                    } else {
                        let (k, half) = dark_clean_taper(k, half, DARK_TRAIN_CLEAN_SHARE);
                        fit_room(k, half, text.room(px, py, half))
                    };
                    let cov = f.colour_cov * k * (end_ease * inv_ends_peak).min(1.0);
                    if cov < 1.0 {
                        continue;
                    }
                    let stop = spectrum(walk_pace(tri(m.t_land - (f.to_go + s) * lay_step)));
                    let cov = if warm && clean && !square {
                        let g = match last_warm_gain {
                            Some((prev, cached)) if prev == stop => cached,
                            _ => {
                                let g = dark_warm_gain(stop);
                                last_warm_gain = Some((stop, g));
                                g
                            }
                        };
                        (cov * g).min(255.0)
                    } else {
                        cov
                    };
                    (stop, cov, half)
                }
            }
            Layer::White => {
                let tail = (s - f.white_shoulder).max(0.0);
                let printed = if square {
                    if rows_off >= 0.5 * ch && (py - m.y0).abs() < 0.5 * ch {
                        WHITE_PRINTED_SHARE
                    } else {
                        1.0
                    }
                } else {
                    white_printed_share(py - m.y0, rows_off, ch)
                };
                let blend = smoothstep01((s - f.white_shoulder) * inv_white_shoulder);
                let share =
                    SHOULDER_WIDTH_SHARE + (WHITE_WIDTH_SHARE - SHOULDER_WIDTH_SHARE) * blend;
                let half = f.width_at(s) * 0.5 * share;
                // On a dark page dimmed white heat is a grey smudge (looks6
                // N6): off the printed line, along its tail and in time it
                // narrows at its own white to nothing instead.
                let exp_tail = if tail <= 0.0 {
                    1.0
                } else {
                    crate::color_math::exp_f32(-tail * inv_white_fall)
                };
                let fade = exp_tail * alpha * shows;
                let (k, half) = if light || square || !clean {
                    (fade * printed, half)
                } else {
                    let off_print = if printed >= 1.0 {
                        1.0
                    } else {
                        ((printed - WHITE_PRINTED_SHARE) * (1.0 / (1.0 - WHITE_PRINTED_SHARE)))
                            .clamp(0.0, 1.0)
                    };
                    let (k, half) = dark_clean_taper(fade * off_print, half, 1.0);
                    fit_room(k, half, text.room(px, py, half))
                };
                let cov = f.head_cov * k * (end_ease * inv_ends_peak).min(1.0);
                if cov < 1.0 {
                    continue;
                }
                (0x00FF_FFFF, cov, half)
            }
        };
        let half = half.max(0.5);
        let (major, spine) = match f.axis {
            Axis::Horizontal => (px, py),
            Axis::Vertical => (py, px),
        };
        let vertex = RibbonVertex {
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
        };
        if stepped && light && rail > 0.0 && warm && matches!(layer, Layer::Colour) {
            step_over_train_skip(verts, &vertex);
        }
        verts.push(vertex);
    }
}

/// Before `next` joins a paper train's `verts`, a step over the hues its
/// bend leaves out ([`PAPER_TRAIN_SKIP`]): where the last station lies on
/// the gold side of the skip and `next` on the green side, or the reverse,
/// two stations at their midpoint, the last's colour then `next`'s, so the
/// stick turns from gold to green at a hard edge. Blended between the two
/// stations, the gallery's `enter-bottom` Return read olive for three
/// columns at each turn of its stick on paper (looks8 round 2's K10b).
#[inline]
fn step_over_train_skip(verts: &mut Vec<RibbonVertex>, next: &RibbonVertex) {
    #[inline]
    const fn side(rgb: u32) -> u8 {
        let (gold, green) = PAPER_TRAIN_SKIP;
        let (h, s, _) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
        if s < 0.3 {
            0
        } else if h <= gold + 1.0 {
            1
        } else if h >= green - 1.0 && h < 180.0 {
            2
        } else {
            0
        }
    }
    let Some(&last) = verts.last() else {
        return;
    };
    if last.color == next.color {
        return;
    }
    let (a, b) = (side(last.color), side(next.color));
    if a == 0 || b == 0 || a == b {
        return;
    }
    let mid = RibbonVertex {
        x: 0.5 * (last.x + next.x),
        spine: 0.5 * (last.spine + next.spine),
        up: 0.5 * (last.up + next.up),
        dn: 0.5 * (last.dn + next.dn),
        core_up: 0.5 * (last.core_up + next.core_up),
        core_dn: 0.5 * (last.core_dn + next.core_dn),
        color: last.color,
        cov: 0.5 * (last.cov + next.cov),
        lift: 0.0,
        lift_span: 0.0,
    };
    verts.push(mid);
    verts.push(RibbonVertex {
        color: next.color,
        ..mid
    });
}

#[inline]
fn draw_head(
    m: &Meteor,
    f: &Flight,
    ctx: &Ctx<'_>,
    frame: &mut Frame<'_>,
    (clean, fold, coma): (bool, bool, bool),
    text: Behind<'_>,
) {
    if f.headless {
        return;
    }
    // On paper the head is its train's own eased end: squashed flat to keep
    // off the letters, a drawn core and petals read as an overline and
    // hairlines at the printed line's top after a Return.
    if !ctx.cfg.dark_theme {
        return;
    }
    let geom = ctx.geom;
    let r_c = (NUCLEUS_MIN_PX).max(NUCLEUS_W_SHARE * f.w);
    let (rx, ry) = match f.axis {
        Axis::Horizontal => (r_c * f.stretch, r_c),
        Axis::Vertical => (r_c, r_c * f.stretch),
    };
    let (nx, ny) = (rx * f.flash, ry * f.flash);
    let cx = f.head.0 + f.unit.0 * NUCLEUS_LEAD_PX;
    let cy = f.head.1 + f.unit.1 * NUCLEUS_LEAD_PX;

    let coma_on = f.fade_c >= CHROMA_CULL_ALPHA;
    let core_on = f.fade_w >= STAR_CULL_ALPHA;

    if !coma_on && !core_on {
        return;
    }
    let coma_cov = (COMA_COV_SHARE * f.head_cov * f.fade_c).clamp(0.0, 255.0);
    let core_cov = (f.head_cov * f.fade_w).clamp(0.0, 255.0);
    let (petal_x, petal_y) = (
        COMA_R_SCALE * COMA_PETAL_R * rx,
        COMA_R_SCALE * COMA_PETAL_R * ry,
    );
    // On a dark page a dimmed white reads grey and a dimmed orange brown (a
    // Return's head left a grey blob ringed with brown and green across the
    // echo, its seven dim petals summing to grey). Each light keeps the
    // head's own clean level, and its share and its fade shrink it instead,
    // so the petals stand apart; the core wears the train's hue at the head,
    // and a warm stop that dims to brown or olive walks the night arc. Once
    // the head lands its lights fold into the landing over [`HEAD_FOLD_MS`],
    // and across a lettered row they pass behind the letters as the train
    // does.
    let (coma_cov, petal_k, core_cov, core_k) = if clean {
        let level = f.head_cov.clamp(0.0, 255.0);
        let folded = if fold {
            1.0 - smoothstep01(f.after_ms * (1.0 / HEAD_FOLD_MS))
        } else {
            1.0
        };
        let shrink = folded * text.at(cx, cy);
        (
            level,
            crate::color_math::sqrt_f32(COMA_COV_SHARE * f.fade_c) * shrink,
            level,
            crate::color_math::sqrt_f32(f.fade_w) * shrink,
        )
    } else {
        (coma_cov, 1.0, core_cov, 1.0)
    };
    let (petal_x, petal_y) = (petal_x * petal_k, petal_y * petal_k);
    let from = frame.halos.len();
    if coma && coma_on && coma_cov >= 1.0 && petal_x.min(petal_y) >= HEAD_LIGHT_MIN_R_PX {
        let spin = if ctx.cfg.reduced_motion {
            0.0
        } else {
            ms_since(m.t0, ctx.now) * (std::f32::consts::TAU / COMA_SPIN_MS)
        };
        let (orbit_x, orbit_y) = (
            COMA_R_SCALE * COMA_PETAL_ORBIT * rx,
            COMA_R_SCALE * COMA_PETAL_ORBIT * ry,
        );
        let coma_alpha = coma_cov as u8;
        const COMA_PETAL_UNIT: [(f32, f32); COMA_PETALS] = {
            let mut out = [(0.0f32, 0.0f32); COMA_PETALS];
            let step = std::f32::consts::TAU / (COMA_PETALS as f32);
            let mut i = 0;
            while i < COMA_PETALS {
                out[i] = crate::supernova::sin_cos_f32(step * (i as f32));
                i += 1;
            }
            out
        };
        let (ss, cs) = crate::supernova::sin_cos_f32(spin);
        let mut i = 0;
        while i < COMA_PETALS {
            let (si, ci) = COMA_PETAL_UNIT[i];
            let ca = cs * ci - ss * si;
            let sa = ss * ci + cs * si;
            let (px, py) = (cx + orbit_x * ca, cy + orbit_y * sa);
            let rgb = if clean {
                dark_head_stop_rgb(i)
            } else {
                dark_petal_stop_rgb(i)
            };
            push_halo_add(
                frame.halos,
                geom,
                px,
                py,
                petal_x,
                petal_y,
                premul_rgb(rgb, coma_alpha),
            );
            i += 1;
        }
    }
    let (nx, ny) = (nx * core_k, ny * core_k);
    if core_on && core_cov >= 1.0 && nx.min(ny) >= HEAD_LIGHT_MIN_R_PX {
        let core_rgb = if clean {
            let cw = (geom.cw as f32).max(1.0);
            dark_head_rgb(spectrum(walk_pace(m.arc(f.to_go / cw))))
        } else {
            0x00FF_FFFF
        };
        push_halo_add(
            frame.halos,
            geom,
            cx,
            cy,
            nx,
            ny,
            premul_rgb(core_rgb, core_cov as u8),
        );
    }
    if clean {
        text.cut_halos(frame.halos, from);
    }
}

#[inline]
#[must_use]
const fn flash_scale(after_ms: f32) -> f32 {
    if after_ms <= 0.0 {
        return 1.0;
    }
    let over = FLASH_OVERSHOOT - 1.0;
    if after_ms <= FLASH_MS {
        let u = clamp01(after_ms * (1.0 / FLASH_MS));
        return 1.0 + over * (1.0 - crate::color_math::powf_pos_f32(1.0 - u, FLIGHT_ENTER_EXP));
    }
    let v = clamp01((after_ms - FLASH_MS) * (1.0 / (FLASH_RELAX_MS - FLASH_MS)));
    1.0 + over * (1.0 - smoothstep01(v))
}

#[inline]
fn draw_pin(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    // On paper the pin would land on the caret's block itself, its arms
    // reading as a strike-through and a tick; the landing's flash marks it.
    // A dark peel lands as one train: its pin stood on the new caret as a
    // crosshair star (looks7 M3).
    if ctx.cfg.reduced_motion || !ctx.cfg.dark_theme || l.peel() {
        return;
    }
    let u = clamp01(ms_since(l.pin.at, ctx.now) * (1.0 / PIN_MS));
    let ch = ctx.geom.ch as f32;
    let close = smoothstep01((u - PIN_ARM_HOLD_U) * (1.0 / (1.0 - PIN_ARM_HOLD_U)));
    let arm_scale = if close <= 0.0 {
        1.0
    } else if close >= 1.0 {
        0.0
    } else {
        crate::color_math::powf_pos_f32(1.0 - close, PIN_ARM_EXP)
    };
    let arm = PIN_ARM_CH * ch * arm_scale;
    let alpha = if u <= PIN_ALPHA_HOLD_U {
        1.0
    } else {
        1.0 - smoothstep01((u - PIN_ALPHA_HOLD_U) * (1.0 / (1.0 - PIN_ALPHA_HOLD_U)))
    };
    let cov = TRANSIENT_STAR_COV_CEIL * alpha * clamp01(ctx.cfg.intensity);
    if cov < 1.0 {
        return;
    }
    let arms_rgb = spectrum_snap(l.pin.t);
    let (cx, cy) = (l.x.round() as i32, l.y.round() as i32);
    let a = arm.round() as i32;

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
    let arm_premul = premul_rgb(arms_rgb, cov as u8);
    if reach > 0 {
        let mut ai = 0;
        while ai < 4 {
            let (x, y, w, h) = arms[ai];
            push_fx_rect(frame.out, ctx.geom, x, y, w, h, arm_premul);
            ai += 1;
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

#[inline]
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
        let turn = smoothstep01(age * (1.0 / FLASH_BURST_MS));
        let spent = spend(clamp01(
            (age - FLASH_BURST_MS) * (1.0 / (FLASH_COLOUR_MS - FLASH_BURST_MS)),
        ));
        (1.0 - turn, turn * spent)
    };
    let intensity = clamp01(ctx.cfg.intensity);
    let light = !ctx.cfg.dark_theme;
    let geom = ctx.geom;
    let (cw, ch) = (geom.cw as i32, geom.ch as i32);
    let x0 = (l.x - geom.cw as f32 * 0.5).round() as i32;
    let y0 = (l.y - geom.ch as f32 * 0.5).round() as i32;
    let colour_scale = colour_share * intensity;
    let white_scale = white_share * intensity;
    let mut k = -FLASH_CELLS;
    while k <= FLASH_CELLS {
        let cur_k = k;
        k += 1;
        let blank = matches!(l.sky, Some(s) if s.flash_blank(cur_k));
        let cap = if blank {
            FLASH_FULL_COV
        } else {
            TRANSIENT_STAR_COV_CEIL
        };
        let x = x0 + cur_k * cw;
        let colour_cov = cap * colour_scale;
        if light {
            let ink = (colour_cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
            if ink > 0 {
                let stop = spectrum_snap(l.field(cur_k as f32));
                push_ink_rect(
                    frame.out,
                    geom,
                    (x, y0, cw, ch),
                    paper_mark_ink(stop, ctx.cfg.theme_bg),
                    ink,
                );
            }
            continue;
        }
        let white_cov = cap * white_scale;
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
            let stop = spectrum_snap(l.field(cur_k as f32));
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

#[inline]
#[must_use]
const fn burst_rise_limit(dy: f32, rise_px: f32, half_thick_px: f32) -> f32 {
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

#[inline]
#[must_use]
const fn landing_ink_at(
    l: &Landing,
    x: f32,
    inv_cw: f32,
    page: Option<u32>,
    memo: &mut Option<(u32, u32)>,
) -> u32 {
    let t = l.field((x - l.x) * inv_cw);
    match page {
        Some(bg) => {
            let raw = spectrum(walk_pace(clamp01(t)));
            match *memo {
                Some((prev_raw, solved)) if prev_raw == raw => solved,
                _ => {
                    let solved = paper_mark_ink(raw, bg);
                    *memo = Some((raw, solved));
                    solved
                }
            }
        }
        None => landing_ink(t),
    }
}

#[inline]
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
    memo: &mut Option<(u32, u32)>,
) {
    let (ox, oy) = (l.x, l.y);
    let span = tip - root;
    if span < 1.0 {
        return;
    }
    let clip = ctx.geom.beam_clip();
    let inv_cw = 1.0 / (ctx.geom.cw as f32).max(1.0);
    let page = if light { Some(ctx.cfg.theme_bg) } else { None };
    let inv_span = 1.0 / span;
    let inv_stations = 1.0 / (BURST_SEC_STATIONS - 1) as f32;
    let light_ink = (cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP);
    let mut s0 = root;
    let mut done = 0.0_f32;
    let mut k = 0;
    while k < 3 {
        let thick_ch = sec_thick_px[k];
        done += BURST_SEC_SHARE[k];
        k += 1;
        let s1 = if k == 3 { tip } else { root + span * done };
        if s1 - s0 < 0.5 {
            s0 = s1;
            continue;
        }
        let thick = thick_ch.max(1.0);
        if light {
            let side = (thick.round() as i32).max(1);
            let half_side = side / 2;
            let stride = (thick * BURST_LIGHT_STEP_SHARE).max(1.0);
            let mut s = s0;
            while s <= s1 {
                let t = (s - root) * inv_span;
                let a = (light_ink * (1.0 - BURST_TIP_FADE * t)) as u8;
                let px = ox + dir.0 * s;
                let rect = (
                    px.round() as i32 - half_side,
                    (oy + dir.1 * s).round() as i32 - half_side,
                    side,
                    side,
                );
                let stop = landing_ink_at(l, px, inv_cw, page, memo);
                push_ink_rect(frame.out, ctx.geom, rect, stop, a);
                s += stride;
            }
        } else {
            frame.beams.clear();
            let ds = s1 - s0;
            let mut j = 0;
            while j < BURST_SEC_STATIONS {
                let f = j as f32 * inv_stations;
                let s = s0 + ds * f;
                let t = (s - root) * inv_span;
                let px = ox + dir.0 * s;
                frame.beams.push(BeamVertex {
                    x: px,
                    y: oy + dir.1 * s,
                    color: landing_ink_at(l, px, inv_cw, page, memo),
                    cov: (cov * (1.0 - BURST_TIP_FADE * t)).clamp(0.0, 255.0) as u8,
                });
                j += 1;
            }
            frame.lay_beam(clip, thick, step);
        }
        s0 = s1;
    }
}

#[inline]
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
    memo: &mut Option<(u32, u32)>,
) {
    let Some(sky) = l.sky else {
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
    let alpha_cap = if light { LIGHT_ALPHA_CAP } else { 255.0 };
    let page = if light { Some(ctx.cfg.theme_bg) } else { None };
    let step = burst_step(ch, JET_STEP_CH_SHARE, JET_STEP_PX);
    let sec_end = [root + span * JET_SEC_SHARE[0], tip];
    let inv_cw = 1.0 / cw;
    let inv_span = 1.0 / span;
    let mut s0 = root;
    let mut k = 0;
    while k < 2 {
        let s1 = sec_end[k];
        let cur_k = k;
        k += 1;
        let seg = s1 - s0;
        if seg < 1.0 {
            s0 = s1;
            continue;
        }
        let thick = (JET_SEC_THICK_CH[cur_k] * ch).max(1.0);
        let stations = ((seg * inv_cw) * JET_STATIONS_PER_CELL as f32)
            .ceil()
            .max(1.0) as usize
            + 1;
        let denom = (stations - 1) as f32;
        let inv_denom = 1.0 / denom;
        let w = ((seg * inv_denom).round() as i32).max(1) + 1;
        let h = (thick.round() as i32).max(1);
        let (half_w, half_h) = (w / 2, h / 2);
        frame.beams.clear();
        let mut j = 0;
        while j < stations {
            let share = j as f32 * inv_denom;
            j += 1;
            let d = s0 + seg * share;
            let dx = dir.0 * d;
            let cell = (dx * inv_cw).round() as i32;
            if !sky.row_blank(cell) {
                if !light && !frame.beams.is_empty() {
                    frame.lay_beam(clip, thick, step);
                    frame.beams.clear();
                }
                continue;
            }
            let t_all = ((d - root) * inv_span).clamp(0.0, 1.0);
            let fade =
                1.0 - smoothstep01((t_all - JET_FEATHER_SHARE) * (1.0 / (1.0 - JET_FEATHER_SHARE)));
            let c = (cov * fade * ink_gain).min(alpha_cap);
            if c < 1.0 {
                if !light && !frame.beams.is_empty() {
                    frame.lay_beam(clip, thick, step);
                    frame.beams.clear();
                }
                continue;
            }
            let px = l.x + dx;
            let stop = landing_ink_at(l, px, inv_cw, page, memo);
            if light {
                let rect = (
                    px.round() as i32 - half_w,
                    (l.y + dir.1 * d).round() as i32 - half_h,
                    w,
                    h,
                );
                push_ink_rect(frame.out, ctx.geom, rect, stop, c as u8);
            } else {
                frame.beams.push(BeamVertex {
                    x: px,
                    y: l.y + dir.1 * d,
                    color: stop,
                    cov: c as u8,
                });
            }
        }
        if !light {
            frame.lay_beam(clip, thick, step);
        }
        s0 = s1;
    }
}

#[inline]
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
        let u_spend = clamp01((u - RING_COV_HOLD_U) * (1.0 / (1.0 - RING_COV_HOLD_U)));
        const _: () = assert!(RING_R_EXP == 4);
        let om = 1.0 - u;
        let om2 = om * om;
        (1.0 - om2 * om2, spend(u_spend))
    };
    let cov = RING_COV_SHARE * TRANSIENT_STAR_COV_CEIL * alpha * clamp01(ctx.cfg.intensity);
    if cov < 1.0 || grow <= 0.0 {
        return;
    }
    let light = !ctx.cfg.dark_theme;
    let mut light_ink_memo: Option<(u32, u32)> = None;
    let cap = frame.out_ramps.laid(frame.out.len()) + BURST_QUAD_CAP;
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
    let core_step = burst_step(ch, BURST_STEP_CH_SHARE, BURST_STEP_PX);
    let mut slot = 0;
    while slot < n {
        if frame.out_ramps.laid(frame.out.len()) >= cap {
            break;
        }
        let i = usize::from(b.order[slot]);
        slot += 1;
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
            core_step,
            light,
            &mut light_ink_memo,
        );
    }

    let r_full = ring_full_radius(b.scale, ch);
    let jet_half_thick = JET_SEC_THICK_CH[0] * ch * 0.5;
    let jet_root = core_full * grow;
    if jet_root <= root {
        return;
    }
    let mut k = 0;
    while k < JET_LEN_SHARE.len() {
        let len_share = JET_LEN_SHARE[k];
        let mut si = 0;
        while si < 2 {
            if frame.out_ramps.laid(frame.out.len()) >= cap {
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
                &mut light_ink_memo,
            );
            si += 1;
        }
        k += 1;
    }
}

#[inline]
#[must_use]
const fn mint_spark(seed: u32, k: usize, ch: f32) -> Spark {
    let s = mix32(seed ^ (k as u32).wrapping_mul(0x27D4_EB2F) ^ 0x5AA5_5AA5);
    let life = SPARK_LIFE_MIN_MS + (SPARK_LIFE_MAX_MS - SPARK_LIFE_MIN_MS) * unit01(s, 1);
    let a =
        std::f32::consts::PI * (SPARK_CONE_SHARE + (1.0 - 2.0 * SPARK_CONE_SHARE) * unit01(s, 2));
    let v = (SPARK_V_MIN_CH_PER_S + (SPARK_V_MAX_CH_PER_S - SPARK_V_MIN_CH_PER_S) * unit01(s, 3))
        * ch
        * (1.0 / 1000.0);
    let sizes = SPARK_ARM_MAX_PX - SPARK_ARM_MIN_PX + 1;
    let raw_idx = (sizes as f32 * unit01(s, 4)) as i32;
    let idx = if raw_idx < sizes - 1 {
        raw_idx
    } else {
        sizes - 1
    };
    let arm = SPARK_ARM_MIN_PX + idx;
    let (sa, ca) = crate::supernova::sin_cos_f32(a);
    Spark {
        vx: v * ca * SPARK_SPREAD,
        vy: -v * sa,
        life,
        arm: arm as u8,
    }
}

#[cfg(test)]
#[inline]
#[must_use]
const fn spark_at(spark: Spark, age_ms: f32, ch: f32) -> Option<((f32, f32), f32)> {
    let g = SPARK_G_CH_PER_S2 * ch * (1.0 / 1_000_000.0);
    spark_at_with_gravity(spark, age_ms, 0.5 * g * age_ms * age_ms)
}

#[inline]
#[must_use]
const fn spark_at_with_gravity(
    spark: Spark,
    age_ms: f32,
    gravity_dy: f32,
) -> Option<((f32, f32), f32)> {
    let u = age_ms / spark.life.max(1.0);
    if !(u >= 0.0 && u < 1.0) {
        return None;
    }
    let alpha = if u <= SPARK_HOLD_U {
        1.0
    } else {
        spend((u - SPARK_HOLD_U) * (1.0 / (1.0 - SPARK_HOLD_U)))
    };
    if alpha < CHROMA_CULL_ALPHA {
        return None;
    }
    let dx = spark.vx * age_ms;
    let dy = spark.vy * age_ms + gravity_dy;
    Some(((dx, dy), alpha))
}

/// The most quads one spark's star may spend.
pub const SPARK_STAR_QUAD_CAP: usize = 24;

#[inline]
fn draw_sparks(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>, halo_cap: usize) {
    if l.sparks == 0 || ctx.cfg.reduced_motion {
        return;
    }
    let age = ms_since(l.pin.at, ctx.now);
    if age < FLASH_BURST_MS {
        return;
    }
    let geom = ctx.geom;
    let ch = geom.ch as f32;
    let inv_cw = 1.0 / (geom.cw as f32).max(1.0);
    let intensity = clamp01(ctx.cfg.intensity);
    if intensity <= 0.0 {
        return;
    }
    let g = SPARK_G_CH_PER_S2 * ch * (1.0 / 1_000_000.0);
    let gravity_dy = 0.5 * g * age * age;
    let light = !ctx.cfg.dark_theme;
    let mut light_stops = [None; SPECTRUM_STOPS];
    let limit = usize::from(l.sparks).min(l.spark.len());
    let mut si = 0;
    while si < limit {
        let spark = l.spark[si];
        si += 1;
        let Some(((dx, dy), alpha)) = spark_at_with_gravity(spark, age, gravity_dy) else {
            continue;
        };
        let cov = TRANSIENT_STAR_COV_CEIL * alpha * intensity;
        if cov < 1.0 {
            continue;
        }
        let stop = spectrum_snap_index(walk_pace(l.field(dx * inv_cw)));
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
            // A spark's cross touching the caret's block reads as part of
            // it, as a star's does.
            if ctx.near_paper_caret((x, y), arm) {
                continue;
            }
            let ink = (cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
            if ink > 0 {
                let c = match light_stops[stop] {
                    Some(c) => c,
                    None => *light_stops[stop]
                        .insert(paper_mark_ink(spectrum_stop(stop), ctx.cfg.theme_bg)),
                };
                push_ink_rect(frame.out, geom, (x - arm, y, 2 * arm + 1, 1), c, ink);
                push_ink_rect(frame.out, geom, (x, y - arm, 1, 2 * arm + 1), c, ink);
            }
            continue;
        }
        let rgb = landing_stop_ink(stop);
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
        let core = landing_spark_core_ink(stop);
        let cap = frame.out.len() + SPARK_STAR_QUAD_CAP;
        push_twinkle_star(frame.out, geom, x, y, arm, cov as u8, false, core, cap);
    }
}

#[inline]
fn shed(m: &mut Meteor, f: &Flight, ctx: &Ctx<'_>, sown: &mut Vec<Sow>) {
    let n = usize::from(shed_n(m.cells)).min(8);
    let mask = ((1_u16 << n) - 1) as u8;
    if m.shed & mask == mask {
        return;
    }
    let inv_cw = 1.0 / (ctx.geom.cw as f32).max(1.0);
    let passed = f.head_travel_share();
    let t_ms = (m.t_flight.as_secs_f32() * 1000.0).max(1.0);
    let speed = head_speed(f.l, t_ms, f.u);
    let v_head = (f.unit.0 * speed, f.unit.1 * speed);
    let mut i = 0;
    while i < n {
        let cur_i = i;
        i += 1;
        let bit = 1_u8 << cur_i;
        if m.shed & bit != 0 {
            continue;
        }
        let share = SHED_S_MIN + (SHED_S_MAX - SHED_S_MIN) * unit01(m.seed, 0x5ED0 ^ cur_i as u32);
        if passed < share {
            continue;
        }
        m.shed |= bit;
        let s = share * f.l;
        let at_px = (m.x0 + f.unit.0 * s, m.y0 + f.unit.1 * s);
        if !on_glass(ctx.geom, at_px.0, at_px.1) {
            continue;
        }
        sown.push(Sow::Shed {
            at: ctx.now,
            spec: ShedSow {
                at_px,
                v_head,
                tint_t: m.arc((f.l - s) * inv_cw),
                seed: mix32(m.seed ^ (cur_i as u32).wrapping_mul(0x9E37_79B9)),
                n: 1,
            },
        });
    }
}

#[inline]
#[must_use]
const fn spark_room(m: &Meteor, ctx: &Ctx<'_>, hand_floor: Option<(u16, u16)>) -> Option<f32> {
    let Some((row, col)) = hand_floor else {
        return None;
    };
    let geom = ctx.geom;
    let ch = (geom.ch as f32).max(1.0);
    let land_row = ((m.y1 - geom.origin_y as f32) / ch).floor();
    if !land_row.is_finite() || land_row < 0.0 || land_row != row as f32 {
        return None;
    }
    Some((m.x1 - geom.cell_center(row, col).0).max(0.0))
}

#[inline]
fn mint_landing(m: &Meteor, ctx: &Ctx<'_>, hand_floor: Option<(u16, u16)>) -> Landing {
    let at = m.arrival();
    let ch = ctx.geom.ch as f32;
    let (x, y) = (m.x1, m.y1);

    let small = !m.ring;
    let n = if small {
        let drow = (m.y1 - m.y0).abs() / ch.max(1.0);
        let extra = drow.round() as usize;
        (ENTER_LANDING_M3_MIN + extra.min(ENTER_LANDING_M3_MAX - ENTER_LANDING_M3_MIN) + 1)
            .min(super::timing::FAN_MAX_N)
    } else {
        fan_count(m.cells, ctx.disp >= FAN_PARTY_DISP)
    };

    let reach = if small {
        FAN_REACH_MIN_CH * ch
    } else {
        fan_reach_ch(m.cells, m.grade) * ch
    };

    let sparks = if small {
        0
    } else {
        ((SPARK_N_BASE + m.cells * (1.0 / SPARK_CELLS_PER)).round() as usize).min(SPARK_N_MAX)
    };
    let seed = mix32(m.seed ^ 0xFA5E);
    let mut spark = [Spark::EMPTY; SPARK_N_MAX];
    let mut k = 0;
    while k < sparks {
        spark[k] = mint_spark(seed, k, ch);
        k += 1;
    }
    if let Some(room) = spark_room(m, ctx, hand_floor) {
        let mut k = 0;
        while k < sparks {
            let sp = &mut spark[k];
            if sp.vx < 0.0 && -sp.vx * sp.life > room {
                sp.vx = -sp.vx;
            }
            k += 1;
        }
    }

    Landing {
        pin: Pin { at, t: m.arc(0.0) },
        walk: LandingWalk {
            t_land: m.t_land,
            band: m.band,
        },
        burst: if m.ring {
            Some(mint_burst(mix32(seed ^ 0xB0B5), at, m.cells))
        } else {
            None
        },
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

#[inline]
fn ms_since(a: Instant, b: Instant) -> f32 {
    b.saturating_duration_since(a).as_secs_f32() * 1000.0
}

#[inline]
const fn on_glass(geom: Geom, x: f32, y: f32) -> bool {
    super::stardust::on_glass((x.round() as i32, y.round() as i32), geom)
}

#[inline]
fn head_at(m: &Meteor, now: Instant) -> (f32, f32) {
    let t = (m.t_flight.as_secs_f32() * 1000.0).max(1.0);
    let p = enter_at_speed(ms_since(m.t0, now) / t);
    let (dx, dy) = m.delta();
    (m.x0 + dx * p, m.y0 + dy * p)
}

#[inline]
const fn head_speed(l: f32, t_ms: f32, u: f32) -> f32 {
    if t_ms <= 0.0 {
        return 0.0;
    }
    l * (1.0 - FLIGHT_P0)
        * FLIGHT_ENTER_EXP
        * crate::color_math::powf_pos_f32(1.0 - clamp01(u), FLIGHT_ENTER_EXP - 1.0)
        / t_ms
}

#[inline]
const fn seg_gap(a0: (f32, f32), a1: (f32, f32), b0: (f32, f32), b1: (f32, f32)) -> f32 {
    if segments_cross(a0, a1, b0, b1) {
        return 0.0;
    }
    point_seg(a0, b0, b1)
        .min(point_seg(a1, b0, b1))
        .min(point_seg(b0, a0, a1))
        .min(point_seg(b1, a0, a1))
}

#[inline]
const fn point_seg(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / len2).clamp(0.0, 1.0)
    };
    crate::color_math::hypot_f32(p.0 - (a.0 + vx * t), p.1 - (a.1 + vy * t))
}

#[inline]
const fn segments_cross(a0: (f32, f32), a1: (f32, f32), b0: (f32, f32), b1: (f32, f32)) -> bool {
    #[inline]
    const fn cross(o: (f32, f32), p: (f32, f32), q: (f32, f32)) -> f32 {
        (p.0 - o.0) * (q.1 - o.1) - (p.1 - o.1) * (q.0 - o.0)
    }
    let (d1, d2) = (cross(b0, b1, a0), cross(b0, b1, a1));
    let (d3, d4) = (cross(a0, a1, b0), cross(a0, a1, b1));
    (d1 * d2 < 0.0) && (d3 * d4 < 0.0)
}

#[inline]
const fn seed_of(row: u16, col: u16, ord: u32) -> u32 {
    mix32((row as u32) << 20 ^ (col as u32) << 8 ^ ord.wrapping_mul(0x9E37_79B9))
}

#[inline]
const fn unit01(seed: u32, salt: u32) -> f32 {
    hash01(mix32(seed ^ salt))
}

#[inline]
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

#[inline]
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

const _: () = {
    assert!(
        FLIGHT_MAX_LIVE * UNDER_QUAD_CAP + super::ribbon::RIBBON_QUAD_BUDGET <= 16_384,
        "§18: two meteors' trains and the ribbon must fit inside MAX_QUADS"
    );
    assert!(
        LANDING_POOL * BURST_QUAD_CAP + FLIGHT_MAX_LIVE * WHITE_QUAD_CAP <= 16_384,
        "§18: three bursts and two white layers must fit inside MAX_QUADS"
    );
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

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::cursor_glow::SoundCue;
    use crate::rainbow_kitty::stardust::{FAN_RISE_MAX_CH, Star, StarClass, StarLane};
    use crate::rainbow_kitty::timing::FAN_HERO_N;
    use crate::rainbow_kitty::{CaretSeam, Config};
    use crate::spectrum::SPECTRUM_ANCHORS;

    const fn geom() -> Geom {
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

    const fn config() -> Config {
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

    #[derive(Default)]
    struct Scratch {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        under_ramps: aterm_render::GlowRamps,
        out_ramps: aterm_render::GlowRamps,
        halos: Vec<RainHalo>,
        beams: Vec<BeamVertex>,
        cues: Vec<SoundCue>,
    }

    impl Scratch {
        const fn frame(&mut self) -> Frame<'_> {
            Frame {
                under: &mut self.under,
                out: &mut self.out,
                under_ramps: &mut self.under_ramps,
                out_ramps: &mut self.out_ramps,
                halos: &mut self.halos,
                beams: &mut self.beams,
                cues: &mut self.cues,
                caret: CaretSeam::EMPTY,
                companion: None,
                fp: 0,
                rows_cost: Some(aterm_render::RampCost::NEVER),
            }
        }
        fn clear(&mut self) {
            self.under.clear();
            self.out.clear();
            self.halos.clear();
            self.beams.clear();
            self.cues.clear();
        }
        fn emit(&mut self, m: &mut Meteors, ctx: &Ctx<'_>) {
            self.clear();
            let mut fr = self.frame();
            m.emit(ctx, &mut fr);
        }
    }

    const fn ctx_with(
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
            flow: crate::rainbow_kitty::spine::Flow::EMPTY,
        }
    }

    const fn ctx_at(now: Instant, cfg: &Config, caret: (u16, u16), caret_t: f32) -> Ctx<'_> {
        ctx_with(now, cfg, caret, caret_t, 0.5, 0.5)
    }

    const fn mv_as(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
        Event::Move {
            from,
            to,
            licence,
            dir: Dir::of(to.1 as i32 - from.1 as i32, to.0 as i32 - from.0 as i32),
        }
    }

    const fn mv(from: (u16, u16), to: (u16, u16)) -> Event {
        mv_as(from, to, Licence::Nav)
    }

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn sky() -> Stardust {
        let mut dust = Stardust::new();
        for row in 3..=10 {
            dust.probe_mut().probe_row(row, &[false; 120]);
        }
        dust
    }

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

    const fn lum(c: u32) -> f32 {
        0.2126 * ((c >> 16) & 0xFF) as f32
            + 0.7152 * ((c >> 8) & 0xFF) as f32
            + 0.0722 * (c & 0xFF) as f32
    }

    const fn chan(c: u32) -> (u32, u32, u32) {
        ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF)
    }

    const fn is_white(c: u32) -> bool {
        let (r, g, b) = chan(c);
        r == g && g == b && r > 0
    }

    const fn is_chromatic(c: u32) -> bool {
        let (r, g, b) = chan(c);
        let m = if r > g { r } else { g };
        let m = if m > b { m } else { b };
        m > 0 && !(r == g && g == b)
    }

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

    /// The heads' nuclei, by shape not colour (on a dark page the nucleus
    /// wears the train's hue at the head): each the light its corona's
    /// petals ring, the centroid of the [`COMA_PETALS`] lights nearest it.
    /// A petal's nearest lights lean to the ring's centre, off the petal.
    fn nuclei(halos: &[RainHalo]) -> Vec<&RainHalo> {
        let mut lights: Vec<&RainHalo> = Vec::new();
        for h in halos {
            if !lights
                .iter()
                .any(|l| (l.cx, l.cy, l.color) == (h.cx, h.cy, h.color))
            {
                lights.push(h);
            }
        }
        let at = |h: &RainHalo| (f32::from(h.cx), f32::from(h.cy));
        lights
            .iter()
            .copied()
            .filter(|h| {
                let (x, y) = at(h);
                let mut near: Vec<(f32, f32)> = lights
                    .iter()
                    .map(|o| at(o))
                    .filter(|&(ox, oy)| (ox, oy) != (x, y))
                    .collect();
                if near.len() < COMA_PETALS {
                    return false;
                }
                near.sort_by(|a, b| {
                    (a.0 - x)
                        .hypot(a.1 - y)
                        .total_cmp(&(b.0 - x).hypot(b.1 - y))
                });
                let ring = &near[..COMA_PETALS];
                let n = COMA_PETALS as f32;
                let (mx, my) = ring
                    .iter()
                    .fold((0.0, 0.0), |(sx, sy), p| (sx + p.0 / n, sy + p.1 / n));
                (mx - x).hypot(my - y) <= 1.5
            })
            .collect()
    }

    /// The brightest light in a halo's colour: its largest channel.
    const fn peak_of(c: u32) -> f32 {
        let (r, g, b) = chan(c);
        let rg = if r >= g { r } else { g };
        (if rg >= b { rg } else { b }) as f32
    }

    fn nucleus(halos: &[RainHalo]) -> Option<&RainHalo> {
        nuclei(halos)
            .into_iter()
            .max_by(|a, b| peak_of(a.color).total_cmp(&peak_of(b.color)))
    }

    fn nucleus_cx(halos: &[RainHalo]) -> Option<(f32, f32, f32)> {
        nucleus(halos).map(|h| (f32::from(h.cx), f32::from(h.cy), peak_of(h.color)))
    }

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

        for cells in [8.0_f32, 40.0, 200.0] {
            let t = super::super::timing::flight_ms(cells);
            let d = enter_at_speed(8.0 / t) - enter_at_speed(0.0);
            assert!(
                d >= 0.10,
                "T = {t}: the head moved {d} of the path in 8 ms, not 0.10"
            );
        }
    }

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
            let mut t = t0;
            while t <= arrival + ms(400) {
                let at = ctx_at(t, &cfg, (5, cols), 0.25);
                sc.emit(&mut m, &at);
                t += ms(8);
            }
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

    #[test]
    fn the_train_carries_the_whole_spectrum_on_every_frame_of_the_flight() {
        use crate::spectrum::{SPECTRUM_LUT, SPECTRUM_LUT_LEN};
        let cfg = config();
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

    #[test]
    fn a_flight_off_its_row_spares_the_line_it_left() {
        let cfg = config();
        let t0 = Instant::now();
        let white_on = |from: (u16, u16), to: (u16, u16), row: u16| -> f32 {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            m.on_event(&mv(from, to), t0, &ctx)
                .expect("a long leap flies");
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx);
            let near = (f32::from(from.1) - 6.0) * ctx.geom.cw as f32;
            sc.out
                .iter()
                .filter(|q| q.row == row && f32::from(q.x) >= near)
                .map(|q| lum(q.color))
                .fold(0.0f32, f32::max)
        };
        let off = white_on((5, 60), (8, 2), 5);
        let along = white_on((5, 60), (5, 2), 5);
        assert!(
            along > 0.0,
            "control: the leap along the row lays its white heat there"
        );
        assert!(
            off * 5.0 <= along * 2.0,
            "the white heat over the line it left: {off:.1} against {along:.1} along it"
        );
    }

    #[test]
    fn a_train_eases_in_at_both_ends() {
        let cfg = config();
        let t0 = Instant::now();
        let steps = |quads: &[GlowQuad], row: u16| -> (f32, f32) {
            let mut col = vec![0.0f32; 4096];
            for q in quads.iter().filter(|q| q.row == row) {
                for x in q.x..q.x + q.w {
                    let v = lum(q.color).max(lum(q.color2));
                    col[usize::from(x)] = col[usize::from(x)].max(v);
                }
            }
            let peak = col.iter().copied().fold(0.0, f32::max);
            let step = col
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f32::max);
            (step, peak)
        };
        let worst = |square: bool| -> (f32, f32) {
            let (mut white, mut colour) = (0.0f32, 0.0f32);
            for (from, to) in [((5u16, 40u16), (5u16, 2u16)), ((5, 2), (5, 40))] {
                let mut m = Meteors::new();
                m.ends_square = square;
                let ctx = ctx_at(t0, &cfg, to, 0.25);
                m.on_event(&mv(from, to), t0, &ctx).expect("a leap flies");
                for at in [16u64, 40] {
                    let mut sc = Scratch::default();
                    sc.emit(&mut m, &ctx_at(t0 + ms(at), &cfg, to, 0.25));
                    let (ws, wp) = steps(&sc.out, 5);
                    let (cs, cp) = steps(&sc.under, 5);
                    assert!(wp > 0.0 && cp > 0.0, "fixture: both layers fly at {at} ms");
                    white = white.max(ws / wp);
                    colour = colour.max(cs / cp);
                }
            }
            (white, colour)
        };
        let (white, colour) = worst(false);
        assert!(
            white <= 0.4 && colour <= 0.4,
            "a train steps by {white:.2} (white) and {colour:.2} (colour) of its peak in a column"
        );
        let (white_sq, colour_sq) = worst(true);
        assert!(
            white_sq.max(colour_sq) >= 0.6,
            "control: cut square, the trains step by only {white_sq:.2} and {colour_sq:.2}"
        );
    }

    #[test]
    fn a_returns_white_heat_eases_off_the_line_it_printed() {
        let cfg = config();
        let t0 = Instant::now();
        let worst = |square: bool| -> f32 {
            let mut worst = 0.0f32;
            for at in [16u64, 35, 60, 90] {
                let (from, to) = ((5u16, 40u16), (7u16, 2u16));
                let mut m = Meteors::new();
                m.ends_square = square;
                let ctx = ctx_at(t0, &cfg, to, 0.25);
                m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                    .expect("a Return flies");
                let mut sc = Scratch::default();
                sc.emit(&mut m, &ctx_at(t0 + ms(at), &cfg, to, 0.25));
                let mut col = vec![0.0f32; 4096];
                for q in &sc.out {
                    for x in q.x..q.x + q.w {
                        let v = lum(q.color).max(lum(q.color2));
                        col[usize::from(x)] = col[usize::from(x)].max(v);
                    }
                }
                let peak = col.iter().copied().fold(0.0, f32::max);
                assert!(peak > 0.0, "fixture: the white heat flies at {at} ms");
                let lit: Vec<usize> = (0..col.len()).filter(|&x| col[x] > 0.0).collect();
                let (lo, hi) = (lit[0] + 24, lit[lit.len() - 1].saturating_sub(24));
                for x in lo..hi.max(lo) {
                    let big = col[x].max(col[x + 1]);
                    if big < 0.1 * peak {
                        continue;
                    }
                    worst = worst.max((col[x + 1] - col[x]).abs() / big);
                }
            }
            worst
        };
        let eased = worst(false);
        assert!(
            eased <= 0.25,
            "the white heat steps by {eased:.2} of its light in a column"
        );
        let hard = worst(true);
        assert!(
            hard >= 0.45,
            "control: switched hard, the white heat steps by only {hard:.2}"
        );
    }

    #[test]
    fn a_dark_return_leaves_no_grey_or_brown_smudge() {
        // A dimmed white reads grey and a dimmed orange brown on a dark page,
        // so a dark Return's train light is its hue at a clean level or
        // nothing: few lit pixels read as a grey or brown smudge, and the
        // dimming trains (the negative control) leave many.
        let cfg = config();
        let page = cfg.theme_bg & 0x00FF_FFFF;
        let t0 = Instant::now();
        let smudge = |dims: bool| -> (usize, usize) {
            let (mut smudged, mut lit) = (0usize, 0usize);
            for at in [16u64, 35, 60, 90, 140, 200] {
                let (from, to) = ((5u16, 40u16), (7u16, 2u16));
                let mut m = Meteors::new();
                m.dims_dark_trains = dims;
                let ctx = ctx_at(t0, &cfg, to, 0.25);
                m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                    .expect("a Return flies");
                let mut sc = Scratch::default();
                let ctx = ctx_at(t0 + ms(at), &cfg, to, 0.25);
                sc.emit(&mut m, &ctx);
                let (w, h) = (usize::from(ctx.geom.win_w), usize::from(ctx.geom.win_h));
                let mut px = vec![page; w * h];
                for q in sc.under.iter().chain(&sc.out) {
                    let qw = u32::from(q.w.max(1));
                    let step = |i: u32, a: u32, b: u32| {
                        (a * (2 * qw - (2 * i + 1)) + b * (2 * i + 1) + qw) / (2 * qw)
                    };
                    for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                        for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                            let i = (x - usize::from(q.x)) as u32;
                            let (color, alpha) = if q.is_flat() {
                                (q.color, q.alpha)
                            } else {
                                let lerp = |sh: u32| {
                                    step(i, (q.color >> sh) & 0xff, (q.color2 >> sh) & 0xff) << sh
                                };
                                (
                                    lerp(16) | lerp(8) | lerp(0),
                                    step(i, u32::from(q.alpha), u32::from(q.alpha2)) as u8,
                                )
                            };
                            let slot = &mut px[y * w + x];
                            *slot = aterm_render::over_premul(*slot, color, alpha);
                        }
                    }
                }
                // The head's lights, added as the renderer adds a halo.
                let add_sat = |a: u32, b: u32| {
                    [16u32, 8, 0].iter().fold(0u32, |acc, &sh| {
                        acc | ((((a >> sh) & 0xff) + ((b >> sh) & 0xff)).min(255) << sh)
                    })
                };
                for q in sc.halos.iter().filter(|q| q.mode == HaloMode::Add) {
                    let (cx, cy) = (i32::from(q.cx), i32::from(q.cy));
                    let rx2 = i32::from(q.rx) * i32::from(q.rx);
                    let ry2 = i32::from(q.ry) * i32::from(q.ry);
                    for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                        let ny = aterm_render::halo_row_ny(y as i32 - cy, ry2);
                        if ny >= 256 {
                            continue;
                        }
                        for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                            let wt = aterm_render::halo_weight(x as i32 - cx, ny, rx2).min(255);
                            if wt > 0 {
                                let slot = &mut px[y * w + x];
                                *slot = add_sat(*slot, premul_rgb(q.color, wt as u8));
                            }
                        }
                    }
                }
                let kind = |p: u32| {
                    let (hue, sat, val) = crate::color_math::rgb2hsv(p);
                    if p == page || val < 0.16 {
                        0u8
                    } else if (sat < 0.12 && (0.25..0.62).contains(&val))
                        || ((8.0..48.0).contains(&hue) && sat > 0.3 && val < 0.45)
                    {
                        2
                    } else if val >= 0.45 {
                        1
                    } else {
                        3
                    }
                };
                let kinds: Vec<u8> = px.iter().map(|&p| kind(p)).collect();
                for y in 0..h {
                    for x in 0..w {
                        match kinds[y * w + x] {
                            0 => continue,
                            2 => {}
                            _ => {
                                lit += 1;
                                continue;
                            }
                        }
                        lit += 1;
                        // A grey or brown pixel at a clean light's soft
                        // edge is its fringe; one with no clean light
                        // within three pixels is a smudge.
                        let near_clean = (y.saturating_sub(3)..(y + 4).min(h)).any(|yy| {
                            (x.saturating_sub(3)..(x + 4).min(w)).any(|xx| kinds[yy * w + xx] == 1)
                        });
                        if !near_clean {
                            smudged += 1;
                        }
                    }
                }
            }
            (smudged, lit)
        };
        let (shipped, lit) = smudge(false);
        let (dimmed, dim_lit) = smudge(true);
        assert!(lit > 0 && dim_lit > 0, "fixture: the Return flies lit");
        assert!(
            shipped * 50 <= lit,
            "a dark Return smudges {shipped} of {lit} lit pixels grey or brown"
        );
        assert!(
            dimmed * 50 > dim_lit,
            "control: the dimming trains smudge only {dimmed} of {dim_lit}"
        );
    }

    /// On paper the pin draws nothing within a cell of the caret's block (the
    /// landing's flash marks it); on dark (the control) its arms reach there.
    #[test]
    fn on_paper_the_pin_draws_nothing_by_the_caret_s_block() {
        let t0 = Instant::now();
        let pin = |dark: bool| {
            let mut cfg = config();
            cfg.dark_theme = dark;
            if !dark {
                cfg.theme_fg = 0x0024_292F;
                cfg.theme_bg = 0x00FF_FFFF;
            }
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
            let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("fly");
            let mut sc = Scratch::default();
            sc.emit(
                &mut m,
                &ctx_at(t0 + spawn.t_flight + ms(4), &cfg, (5, 80), 0.25),
            );
            let l = *m.landings.first().expect("fixture: the meteor landed");
            sc.clear();
            {
                let at = ctx_at(t0 + spawn.t_flight + ms(20), &cfg, (5, 80), 0.25);
                let mut fr = sc.frame();
                draw_pin(&l, &at, &mut fr);
            }
            (l, sc.out.clone(), sc.halos.len())
        };
        let g = geom();
        let near = |l: &Landing, q: &GlowQuad| {
            let (cw, ch) = (g.cw as i32, g.ch as i32);
            let x0 =
                (l.x as i32 - i32::from(g.origin_x)).div_euclid(cw) * cw + i32::from(g.origin_x);
            let y0 =
                (l.y as i32 - i32::from(g.origin_y)).div_euclid(ch) * ch + i32::from(g.origin_y);
            let (qx, qy) = (i32::from(q.x), i32::from(q.y));
            let (qw, qh) = (i32::from(q.w), i32::from(q.h));
            qy < y0 + 2 * ch && qy + qh > y0 - ch && qx < x0 + 2 * cw && qx + qw > x0 - cw
        };
        let (l, paper, paper_halos) = pin(false);
        assert!(
            paper.iter().all(|q| !near(&l, q)) && paper_halos == 0,
            "a paper pin draws by the caret's block: {paper:?} and {paper_halos} halos"
        );
        let (l, dark, _) = pin(true);
        assert!(
            dark.iter().any(|q| near(&l, q)),
            "control: the dark pin's arms reach by the block"
        );
    }

    /// A train's ramps cut behind a lettered row are the quads their rows
    /// stand for cut ([`Behind::cut_ramps`], lane B5): the same lights, and
    /// a row the cut leaves lit but of no colour is the dark piece the
    /// quad's cut lays, so the stream counts alike (the negative control
    /// lays no dark piece).
    #[test]
    fn ramps_cut_behind_text_are_their_quads_cut() {
        let mut keep = KeepOut::default();
        keep.set_rows([(40, 200, 60, 80)]);
        let cut = |text: Behind<'_>, dark_pieces: bool| {
            // Two slabs down through the row's band: dim additive rows (the
            // eased edge cuts the dimmest to no colour) and source-over ones.
            let lead = GlowQuad::flat(2, 0, 50, 4, 4, 0x0010_1010, 0);
            let (mut legacy, mut quads) = (vec![lead], vec![lead]);
            let mut ramps = GlowRamps::default();
            let mark = ramps.mark();
            {
                let mut sink =
                    RampSink::new(&mut quads, &mut ramps).costed(aterm_render::RampCost::ALWAYS);
                for (x, over) in [(60u16, false), (120, true)] {
                    sink.slab();
                    for y in 50u16..92 {
                        let band = i32::from(y / 20);
                        let c = u32::from(1 + y % 4) * 0x0001_0101;
                        let (a, b) = if over {
                            ((c * 8, 0x40), (c * 6, 0x30))
                        } else {
                            ((c, 0), (c * 2, 0))
                        };
                        sink.row(band, band as u16, (x, y, 24), a, b);
                        legacy.row(band, band as u16, (x, y, 24), a, b);
                    }
                }
            }
            text.cut_quads(&mut legacy, 1);
            if dark_pieces {
                text.cut_ramps((&mut quads, &mut ramps), mark);
            } else {
                let mut none = Vec::new();
                text.cut_ramps((&mut none, &mut ramps), mark);
            }
            let mut got = Vec::new();
            ramps.expand_into(&quads, &mut got);
            let key = |q: &GlowQuad| (q.y, q.x, q.w, q.h, q.color, q.alpha, q.color2, q.alpha2);
            got.sort_by_key(key);
            legacy.sort_by_key(key);
            let dark = legacy
                .iter()
                .filter(|q| q.color | q.color2 == 0 && q.alpha | q.alpha2 == 0)
                .count();
            (got == legacy, ramps.laid(quads.len()) == legacy.len(), dark)
        };
        for text in [
            Behind::bodies(&keep, true),
            Behind::bodies(&keep, true).guarded(),
        ] {
            let (same, counted, dark) = cut(text, true);
            assert!(same && counted, "the cut ramps are the cut quads");
            assert!(dark > 0, "premise: the cut leaves a dark piece");
            assert!(
                !cut(text, false).1,
                "control: without its dark pieces the stream counts short"
            );
        }
    }

    /// The head's own lights at `at`, alone: each live meteor's head drawn
    /// as its emit draws it, `clean` and folding unless told otherwise.
    fn head_lights(
        m: &Meteors,
        at: &Ctx<'_>,
        (clean, fold): (bool, bool),
        keep: &KeepOut,
    ) -> Vec<RainHalo> {
        let mut sc = Scratch::default();
        {
            let mut fr = sc.frame();
            for me in &m.live {
                if let Some(f) = Flight::of(me, at) {
                    draw_head(
                        me,
                        &f,
                        at,
                        &mut fr,
                        (clean, fold, true),
                        Behind::bodies(keep, true),
                    );
                }
            }
        }
        sc.halos
    }

    /// Past [`HEAD_FOLD_MS`] after a Return's arrival the head lays no light;
    /// kept standing (the negative control), it does.
    #[test]
    fn the_head_folds_into_its_landing() {
        let cfg = config();
        let t0 = Instant::now();
        let keep = KeepOut::default();
        let lit = |stay: bool, after: u64| -> usize {
            let (from, to) = ((5u16, 40u16), (7u16, 2u16));
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            let spawn = m
                .on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                .expect("a Return flies");
            let at = ctx_at(t0 + spawn.t_flight + ms(after), &cfg, to, 0.25);
            let mut sc = Scratch::default();
            sc.emit(&mut m, &at);
            head_lights(&m, &at, (true, !stay), &keep).len()
        };
        for after in [HEAD_FOLD_MS as u64 + 4, 70, 120] {
            assert_eq!(
                lit(false, after),
                0,
                "{after} ms after the arrival the head still lays its lights"
            );
        }
        assert!(
            lit(true, HEAD_FOLD_MS as u64 + 4) > 0,
            "control: kept standing, the head is already gone"
        );
    }

    /// Across a lettered row the trains pass behind the letters' bodies: no
    /// train quad stands inside the row's letter band; laid over the text
    /// (the negative control), one does.
    #[test]
    fn a_train_passes_behind_a_lettered_row() {
        let cfg = config();
        let g = geom();
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let (ox, oy) = (i32::from(g.origin_x), i32::from(g.origin_y));
        let row = (ox + 2 * cw, ox + 40 * cw, oy + 6 * ch, oy + 7 * ch);
        let band = (
            row.2 as f32 + TRAIN_TEXT_BAND.0 * ch as f32 + TRAIN_TEXT_FEATHER_PX,
            row.2 as f32 + TRAIN_TEXT_BAND.1 * ch as f32 - TRAIN_TEXT_FEATHER_PX,
        );
        let inside = |x: f32, y: f32| {
            x > row.0 as f32 + TRAIN_TEXT_FEATHER_PX
                && x < row.1 as f32 - TRAIN_TEXT_FEATHER_PX
                && (band.0..=band.1).contains(&y)
        };
        let t0 = Instant::now();
        let struck = |over: bool| -> usize {
            let mut n = 0;
            for at in [16u64, 35, 60, 90, 140] {
                let (from, to) = ((5u16, 40u16), (7u16, 2u16));
                let mut m = Meteors::new();
                m.trains_over_text = over;
                m.keep_out.set_rows([row]);
                let ctx = ctx_at(t0, &cfg, to, 0.25);
                m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                    .expect("a Return flies");
                let mut sc = Scratch::default();
                sc.emit(&mut m, &ctx_at(t0 + ms(at), &cfg, to, 0.25));
                n += sc
                    .under
                    .iter()
                    .chain(&sc.out)
                    .filter(|q| {
                        // Premultiplied: a dark train's light is its colour.
                        (q.color | q.color2) & 0x00FF_FFFF != 0
                            && inside(
                                f32::from(q.x) + 0.5 * f32::from(q.w),
                                f32::from(q.y) + 0.5 * f32::from(q.h),
                            )
                    })
                    .count();
            }
            n
        };
        let behind = struck(false);
        assert_eq!(
            behind, 0,
            "{behind} train lights stand in the letters' band"
        );
        assert!(
            struck(true) > 0,
            "control: laid over the text, the thread never crosses the row"
        );
    }

    /// On a dark page at most one lit pixel in fifty of the head reads grey or
    /// brown, even at its lights' soft edges; the white head with the warm
    /// petals (the negative control) reads so far more.
    #[test]
    fn a_dark_head_is_never_grey_or_brown() {
        let cfg = config();
        let page = cfg.theme_bg & 0x00FF_FFFF;
        let t0 = Instant::now();
        let keep = KeepOut::default();
        let muddy = |clean: bool| -> (usize, usize) {
            let (mut bad, mut lit) = (0usize, 0usize);
            for at in [8u64, 16, 30, 45, 60] {
                let (from, to) = ((5u16, 40u16), (7u16, 2u16));
                let mut m = Meteors::new();
                let ctx = ctx_at(t0, &cfg, to, 0.25);
                m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                    .expect("a Return flies");
                let at = ctx_at(t0 + ms(at), &cfg, to, 0.25);
                let mut sc = Scratch::default();
                sc.emit(&mut m, &at);
                let halos = head_lights(&m, &at, (clean, true), &keep);
                let (w, h) = (usize::from(at.geom.win_w), usize::from(at.geom.win_h));
                let mut px = vec![page; w * h];
                for q in halos.iter().filter(|q| q.mode == HaloMode::Add) {
                    let (cx, cy) = (i32::from(q.cx), i32::from(q.cy));
                    let rx2 = i32::from(q.rx) * i32::from(q.rx);
                    let ry2 = i32::from(q.ry) * i32::from(q.ry);
                    for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                        let ny = aterm_render::halo_row_ny(y as i32 - cy, ry2);
                        if ny >= 256 {
                            continue;
                        }
                        for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                            let wt = aterm_render::halo_weight(x as i32 - cx, ny, rx2).min(255);
                            if wt > 0 {
                                let slot = &mut px[y * w + x];
                                *slot = aterm_render::add_sat(*slot, premul_rgb(q.color, wt as u8));
                            }
                        }
                    }
                }
                for &p in &px {
                    let (hue, sat, val) = crate::color_math::rgb2hsv(p);
                    if p == page || val < 0.16 {
                        continue;
                    }
                    lit += 1;
                    if (sat < 0.12 && (0.25..0.62).contains(&val))
                        || ((8.0..48.0).contains(&hue) && sat > 0.3 && val < 0.45)
                    {
                        bad += 1;
                    }
                }
            }
            (bad, lit)
        };
        let (bad, lit) = muddy(true);
        assert!(lit > 0, "fixture: the Return's head is lit");
        assert!(
            bad * 50 <= lit,
            "{bad} of the head's {lit} lit pixels read grey or brown"
        );
        let (old_bad, old_lit) = muddy(false);
        assert!(
            old_bad * 50 > old_lit,
            "control: the white head reads grey or brown at only {old_bad} of {old_lit}"
        );
    }

    /// On paper the meteor draws no head (the train's own eased end is the
    /// head); on dark (the control) it does.
    #[test]
    fn on_paper_the_meteor_draws_no_head() {
        let t0 = Instant::now();
        let keep = KeepOut::default();
        let heads = |dark: bool| -> usize {
            let mut cfg = config();
            cfg.dark_theme = dark;
            if !dark {
                cfg.theme_fg = 0x0024_292F;
                cfg.theme_bg = 0x00FF_FFFF;
            }
            let mut n = 0;
            for at in [0u64, 16, 35, 60] {
                let (from, to) = ((5u16, 40u16), (7u16, 2u16));
                let mut m = Meteors::new();
                let ctx = ctx_at(t0, &cfg, to, 0.25);
                m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                    .expect("a Return flies");
                let at = ctx_at(t0 + ms(at), &cfg, to, 0.25);
                let mut sc = Scratch::default();
                sc.emit(&mut m, &at);
                n += head_lights(&m, &at, (true, true), &keep).len();
            }
            n
        };
        assert_eq!(heads(false), 0, "the paper meteor drew a head");
        assert!(heads(true) > 0, "control: the dark meteor drew no head");
    }

    /// On paper a landing's sparks stand a cell clear of the caret's block on
    /// every side; on dark (the control) they fly from the block itself.
    #[test]
    fn paper_sparks_stand_a_cell_clear_of_the_caret() {
        let t0 = Instant::now();
        let g = geom();
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let near = |dark: bool| -> usize {
            let mut cfg = config();
            cfg.dark_theme = dark;
            if !dark {
                cfg.theme_fg = 0x0024_292F;
                cfg.theme_bg = 0x00FF_FFFF;
            }
            let to = (5u16, 40u16);
            let x0 = i32::from(g.origin_x) + i32::from(to.1) * cw;
            let y0 = i32::from(g.origin_y) + i32::from(to.0) * ch;
            let boxed = |x: i32, y: i32, w: i32, h: i32| {
                x < x0 + 2 * cw && x + w > x0 - cw && y < y0 + 2 * ch && y + h > y0 - ch
            };
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            let spawn = m.on_event(&mv((5, 0), to), t0, &ctx).expect("fly");
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_at(t0 + spawn.t_flight + ms(2), &cfg, to, 0.25));
            let l = *m.landings.first().expect("fixture: the meteor landed");
            let mut n = 0;
            for after in [4u64, 12, 24, 40, 60, 90, 120, 160] {
                sc.clear();
                let at = ctx_at(t0 + spawn.t_flight + ms(after), &cfg, to, 0.25);
                {
                    let mut fr = sc.frame();
                    draw_sparks(&l, &at, &mut fr, usize::MAX);
                }
                n += sc
                    .out
                    .iter()
                    .filter(|q| {
                        boxed(
                            i32::from(q.x),
                            i32::from(q.y),
                            i32::from(q.w),
                            i32::from(q.h),
                        )
                    })
                    .count();
            }
            n
        };
        let paper = near(false);
        assert_eq!(
            paper, 0,
            "{paper} spark quads within a cell of the paper caret"
        );
        assert!(
            near(true) > 0,
            "control: the dark sparks never fly by the block"
        );
    }

    #[test]
    fn a_dark_coma_is_never_olive() {
        let olive = |rgb: u32| {
            let (h, s, _) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
            s > 0.2 && (40.0..=100.0).contains(&h)
        };
        assert!(
            (0..COMA_PETALS).any(|i| olive(spectrum_stop(i))),
            "control: the spectrum carries a yellow stop"
        );
        let cfg = config();
        assert!(cfg.dark_theme, "fixture: the page is dark");
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (8, 2), 0.25);
        m.on_event(&mv((5, 60), (8, 2)), t0, &ctx)
            .expect("a Return flies");
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_at(t0 + ms(40), &cfg, (8, 2), 0.25));
        let petals: Vec<u32> = sc.halos.iter().map(|h| h.color).collect();
        assert!(
            petals.len() >= COMA_PETALS,
            "fixture: the coma is drawn ({})",
            petals.len()
        );
        for c in petals {
            assert!(!olive(c), "a dark coma petal is olive: #{c:06x}");
        }
    }

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

    #[test]
    fn a_chained_continuation_never_puts_the_head_behind_where_it_was() {
        let cfg = config();
        let t0 = Instant::now();
        let (x0, _) = geom().cell_center(5, 0);
        let (x40, _) = geom().cell_center(5, 40);

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
        let heads: std::collections::BTreeSet<u16> =
            nuclei(&sc.halos).iter().map(|h| h.cx).collect();
        assert_eq!(
            heads.len(),
            1,
            "two heads in the air after a chain: {heads:?}"
        );
        assert!(
            m.landings.iter().all(|l| (l.x - x40).abs() > 1.0),
            "the handed-over head celebrated a landing the caret left"
        );

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

    #[test]
    fn a_chained_stub_never_costs_another_row_its_head() {
        let cfg = config();
        let t0 = Instant::now();
        let (x0, _) = geom().cell_center(5, 0);
        let (x40, _) = geom().cell_center(5, 40);
        let mut m = Meteors::new();
        let b = ctx_at(t0, &cfg, (7, 40), 0.25);
        m.on_event(&mv((7, 0), (7, 40)), t0, &b).expect("B flies");
        let a_at = t0 + ms(5);
        let a = ctx_at(a_at, &cfg, (5, 40), 0.25);
        let s_a = m.on_event(&mv((5, 0), (5, 40)), a_at, &a).expect("A flies");
        assert_eq!(m.live(), FLIGHT_MAX_LIVE);
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
        let mut sc = Scratch::default();
        sc.emit(&mut m, &c);
        let (_, y7) = geom().cell_center(7, 40);
        assert!(
            nuclei(&sc.halos)
                .iter()
                .any(|h| (f32::from(h.cy) - y7).abs() <= 1.0),
            "B's nucleus is gone from row 7"
        );
    }

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

        let mut m = Meteors::new();
        m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
        let c = ctx_at(t0 + ms(10), &cfg, (7, 70), 0.25);
        m.on_event(&mv((7, 60), (7, 70)), t0 + ms(10), &c)
            .expect("flies");
        assert_eq!(m.live(), 2, "flights on different rows retired each other");

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
            let share = FLIGHT_MAX_LIVE * UNDER_QUAD_CAP;
            assert!(
                sc.under.len() <= share,
                "two meteors spent {} under quads — over their {share} share",
                sc.under.len()
            );
            t += ms(8);
        }
    }

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

        let land = ctx_at(t0 + spawn.t_flight + ms(4), &cfg, (5, 80), 0.25);
        sc.emit(&mut m, &land);
        assert_eq!(
            m.landings.len(),
            1,
            "reduced motion did not mint the static landing (the flash and the colours)"
        );
        let mut dust = sky_around(5);
        m.sow_into(&mut dust, &land);
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

        let mut dust = sky();
        assert!(m.on_event(&mv((5, 25), (5, 26)), t0, &ctx).is_none());
        sc.emit(&mut m, &ctx);
        m.sow_into(&mut dust, &ctx);
        assert_eq!(dust.live(), 0, "a one-cell scrub minted light");
    }

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

    #[test]
    fn a_fan_star_core_never_lands_on_a_probed_glyph() {
        let cfg = config();
        let t0 = Instant::now();
        let landing = (5_u16, 40_u16);
        let g = geom();
        let window = StarLane::Fan.throw_ms().expect("the fan is a throw");
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

        let mut inked = [false; 120];
        inked[usize::from(landing.1)] = true;
        let on_ink = census(&mut probed(&inked));
        assert!(
            on_ink >= FAN_HERO_N,
            "a glyph AT the landing cut the fan to {on_ink} stars — the hero and its four m2 \
             are gone, and the five rain glints have nothing to pair with (D6)"
        );

        let mut beside = [false; 120];
        beside[usize::from(landing.1) + 1] = true;
        let full = census(&mut probed(&beside));
        assert!(
            full >= FAN_HERO_N,
            "a glyph BESIDE the landing thinned the fan"
        );
        assert_eq!(on_ink, full, "ink under the caret thinned the fan");

        let mut row = [true; 120];
        row[usize::from(landing.1)] = false;
        let boxed = census(&mut probed(&row));
        assert!(
            boxed >= 1 && boxed <= full,
            "a row of ink around the landing left {boxed} of {full} stars"
        );

        let mut dust = Stardust::new();
        assert!(dust.probe().is_empty());
        assert_eq!(
            census(&mut dust),
            full,
            "an unprobed row refused a free transient"
        );

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

    #[test]
    fn a_band_move_carries_a_flight_inside_the_band_and_retires_one_that_straddles_it() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        let ch = g.ch as u16;

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
        let (cx, cy) = (wx.round() as i32, wy.round() as i32);
        assert!(
            sc.out.iter().any(|q| is_white(q.color)
                && q.w == 2
                && q.h == 2
                && i32::from(q.x) == cx - 1
                && i32::from(q.y) == cy - 1),
            "the pin's heart is not on the scrolled landing"
        );
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

    const fn owner_geom() -> Geom {
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

    const fn ctx_on(now: Instant, cfg: &Config, caret: (u16, u16), g: Geom) -> Ctx<'_> {
        let mut c = ctx_at(now, cfg, caret, 0.25);
        c.geom = g;
        c
    }

    fn landed(cells: u16, cfg: &Config, g: Geom, blank: bool) -> (Meteors, Landing, Instant) {
        landed_with(cells, cfg, g, blank, 0.25, false)
    }

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

    #[test]
    fn the_retire_decay_lands_on_the_chroma_cull() {
        let at_r = (-RETIRE_DECAY).exp();
        assert!(
            (at_r - CHROMA_CULL_ALPHA).abs() < 1e-3,
            "a retired train is at {at_r} after R, not the cull {CHROMA_CULL_ALPHA}"
        );
    }

    #[test]
    fn a_spike_root_is_derived_from_the_spacing_it_must_keep_open() {
        let want = 2.0 * (std::f32::consts::PI / BURST_N_MAX as f32).sin();
        assert!(
            (BURST_ROOT_SEP_AT_MAX_N - want).abs() < 1e-6,
            "the separation coefficient is {BURST_ROOT_SEP_AT_MAX_N}, not 2 sin(pi/12) = {want}"
        );
    }

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

    #[test]
    fn a_burst_stays_inside_its_quad_cap_at_every_grade() {
        let cfg = config();
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
        assert!(
            worst < 2.10,
            "the burst paints to {worst:.3} ch — no tighter than the ring's own 2.10 ch"
        );
    }

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

    /// On a dark page the sparks are haloed stars: a 48-cell jump (landed ten
    /// cells in so the leftward half is on the glass) throws at least sixty
    /// sparks, and at `T + 150` at least fifty chromatic halos are on the
    /// frame and at least forty sparks stand ≥ 5 px on both axes (replaces
    /// the pruned `the_sparks_are_haloed_stars_twice_the_bold_round_s_count`;
    /// 60, 64 and 60 at Prism Glass P0.11).
    #[test]
    fn the_dark_sparks_are_haloed_stars_twice_the_bold_round_s_count() {
        let cfg = config();
        let g = geom();
        let ch = g.ch as f32;
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 10), 0.25);
        let spawn = m.on_event(&mv((5, 58), (5, 10)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_at(arrival, &cfg, (5, 10), 0.25));
        let l = m.landings[0];
        assert!(
            usize::from(l.sparks) >= 60,
            "a 48-cell landing throws {} sparks, not twice the bold round's thirty",
            l.sparks
        );
        sc.emit(&mut m, &ctx_at(arrival + ms(150), &cfg, (5, 10), 0.25));
        let halos = sc.halos.iter().filter(|h| is_chromatic(h.color)).count();
        assert!(
            halos >= 50,
            "{halos} chromatic halos at T + 150: the sparks have none"
        );
        let cell_sized = |q: &GlowQuad| usize::from(q.w) == g.cw && usize::from(q.h) == g.ch;
        let mut big = 0_usize;
        for spark in l.spark.iter().take(usize::from(l.sparks)) {
            let Some(((dx, dy), _)) = spark_at(*spark, 150.0, ch) else {
                continue;
            };
            let (px, py) = ((l.x + dx).round() as i32, (l.y + dy).round() as i32);
            let (mut x_lo, mut x_hi, mut y_lo, mut y_hi) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
            for q in sc.out.iter().filter(|q| {
                is_chromatic(q.color)
                    && !cell_sized(q)
                    && (i32::from(q.x) + i32::from(q.w) / 2 - px).abs() <= 4
                    && (i32::from(q.y) + i32::from(q.h) / 2 - py).abs() <= 4
            }) {
                x_lo = x_lo.min(i32::from(q.x));
                x_hi = x_hi.max(i32::from(q.x) + i32::from(q.w));
                y_lo = y_lo.min(i32::from(q.y));
                y_hi = y_hi.max(i32::from(q.y) + i32::from(q.h));
            }
            if x_hi - x_lo >= 5 && y_hi - y_lo >= 5 {
                big += 1;
            }
        }
        assert!(
            big >= 40,
            "only {big} sparks stand ≥ 5 × 5 px at T + 150: points, not stars"
        );
    }

    #[test]
    fn a_party_landing_is_a_fan_off_the_glass_inside_315_ms() {
        let t0 = Instant::now();
        let cfg = config();
        let mut m = Meteors::new();
        let mut sc = Scratch::default();
        let at = ctx_at(t0, &cfg, (5, 40), 0.3);

        m.party(t0, 5, false, &at);
        assert_eq!(m.sown.len(), 1, "the fan is staged for the sky");
        assert!(m.landings.is_empty(), "no ring bar, no landing in the pool");
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

        let mut off = Meteors::new();
        let far = ctx_at(t0, &cfg, (200, 40), 0.3);
        off.party(t0, 9, true, &far);
        assert!(
            off.sown.is_empty() && off.landings.is_empty(),
            "a caret off the glass mints nothing"
        );
    }

    #[test]
    fn the_shower_stops_where_the_hand_stopped() {
        let cfg = config();
        let g = geom();
        let cw = g.cw as f32;
        let ch = g.ch as f32;
        let col_of = |x: f32| (x / cw).floor() as i32;

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
                sc.emit(&mut m, &ctx);
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

        assert!(
            ink_min >= 27,
            "the shower painted column {ink_min}: the hand's floor is column 28 and a star \
             centred on it spills no further than 27"
        );
        assert!(
            station_min >= 28,
            "a spark came to rest in column {station_min}, left of the hand's floor (28)"
        );

        assert!(
            ink_max >= 32,
            "the bounded shower reached only column {ink_max} — it is a wall, not a shower"
        );

        let (free_min, _, free_station, free_seen) = run(false);
        assert_eq!(free_seen, seen, "the wall must not cost the shower a spark");
        assert!(
            free_min <= 20 && free_station <= 21,
            "with no floor the shower reached column {free_min} (stations {free_station}); \
             it used to reach 18"
        );
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PoolStep {
        Arrive,
        Ring,
        Expire,
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

    struct Pool {
        m: Meteors,
        now: Instant,
        arrivals: Vec<Instant>,
        at_right: bool,
    }

    const POOL_TICK: Duration = Duration::from_micros(1);

    type PoolPatch = fn(&mut Meteors, PoolStep, &[Landing]);

    const fn no_pool_patch(_: &mut Meteors, _: PoolStep, _: &[Landing]) {}

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

    #[derive(Default)]
    struct PoolCoverage {
        states: usize,
        actions: std::collections::BTreeSet<&'static str>,
        saturated_sites: std::collections::BTreeSet<&'static str>,
    }

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

    #[test]
    fn meteors_hand_floor_and_reduced_follow_scroll_band_and_reset() {
        let mut m = Meteors::new();
        m.set_hand_floor(Some((5, 4)));
        m.translate_scroll(2, 18);
        assert_eq!(m.hand_floor_cell(), Some((3, 4)));

        m.translate_band(2, 6, 1, 18, 0);
        assert_eq!(m.hand_floor_cell(), Some((4, 4)));

        m.translate_band(2, 6, -3, 18, 0);
        assert_eq!(m.hand_floor_cell(), None);

        m.set_hand_floor(Some((2, 3)));
        m.reduced = true;
        m.reset();
        assert_eq!(m.hand_floor_cell(), None);
        assert!(!m.reduced);
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

    /// `sc`'s streams laid on a `page` the size of `g`'s window as the
    /// renderer lays them: the under quads, the over quads, then the halos
    /// (added, then laid over).
    fn composite_scratch(sc: &Scratch, g: Geom, page: u32) -> Vec<u32> {
        let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
        let mut px = vec![page; w * h];
        for q in sc.under.iter().chain(&sc.out) {
            let qw = u32::from(q.w.max(1));
            let step = |i: u32, a: u32, b: u32| {
                (a * (2 * qw - (2 * i + 1)) + b * (2 * i + 1) + qw) / (2 * qw)
            };
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                    let i = (x - usize::from(q.x)) as u32;
                    let (color, alpha) = if q.is_flat() {
                        (q.color, q.alpha)
                    } else {
                        let lerp = |sh: u32| {
                            step(i, (q.color >> sh) & 0xff, (q.color2 >> sh) & 0xff) << sh
                        };
                        (
                            lerp(16) | lerp(8) | lerp(0),
                            step(i, u32::from(q.alpha), u32::from(q.alpha2)) as u8,
                        )
                    };
                    let slot = &mut px[y * w + x];
                    *slot = aterm_render::over_premul(*slot, color, alpha);
                }
            }
        }
        for mode in [HaloMode::Add, HaloMode::Over] {
            for q in sc.halos.iter().filter(|q| q.mode == mode) {
                let over_cap = aterm_render::halo_over_cap(q.color);
                let (cx, cy) = (i32::from(q.cx), i32::from(q.cy));
                let rx2 = i32::from(q.rx) * i32::from(q.rx);
                let ry2 = i32::from(q.ry) * i32::from(q.ry);
                for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                    let ny = aterm_render::halo_row_ny(y as i32 - cy, ry2);
                    if ny >= 256 {
                        continue;
                    }
                    for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                        let wt = aterm_render::halo_weight(x as i32 - cx, ny, rx2).min(255);
                        if wt <= 0 {
                            continue;
                        }
                        let slot = &mut px[y * w + x];
                        *slot = match mode {
                            HaloMode::Add => {
                                aterm_render::add_sat(*slot, premul_rgb(q.color, wt as u8))
                            }
                            HaloMode::Over => {
                                aterm_render::over_rgb(*slot, q.color, (wt as u8).min(over_cap))
                            }
                        };
                    }
                }
            }
        }
        px
    }

    /// The trail gallery's `enter-bottom` Return on `cfg`'s page at `after`
    /// ms: from the end of the echo row (11, 26) to the new prompt (13, 2),
    /// the rows above lettered as the gallery's are.
    fn gallery_return(
        cfg: &Config,
        after: u64,
        keep: Option<i32>,
        in_guard: bool,
    ) -> (Scratch, Geom) {
        let g = gallery_geom();
        let t0 = Instant::now();
        let ctx_g = |now: Instant| Ctx {
            geom: g,
            ..ctx_at(now, cfg, (13, 2), 0.25)
        };
        let mut m = Meteors::new();
        m.trains_in_guard = in_guard;
        if let Some(past) = keep {
            let (ox, oy, cw, ch) = (12, 2, 14, 26);
            // The echo row's letters (its last at column 25) and the printed
            // line's (its last at column 18), a dark train's span reaching
            // `past` cells past them.
            m.keep_out.set_rows_past(
                [
                    (ox, ox + 26 * cw, oy + 11 * ch, oy + 12 * ch),
                    (ox, ox + 19 * cw, oy + 12 * ch, oy + 13 * ch),
                ],
                past * cw,
            );
        }
        m.on_event(&mv_as((11, 26), (13, 2), Licence::Return), t0, &ctx_g(t0))
            .expect("a Return flies");
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_g(t0 + ms(after)));
        (sc, g)
    }

    /// After a dark `enter-bottom` Return the echo row's old caret cell
    /// (column 26) keeps the page's dark across the flight, since the line's
    /// kept span reaches the cell its caret stood in
    /// (`CursorGlow::keep_out_boxes`), whether or not the train also keeps
    /// clear of the letter guard's reach (`DARK_TRAIN_TEXT_PAD_PX`); with the
    /// span cut at the letters and the train laid into the guard's reach (the
    /// negative control) a brown stub stands there. The reach is a dark span's
    /// alone ([`KeepOut::past`]): on paper the stick crosses that cell (looks7 M2).
    #[test]
    fn a_return_leaves_no_stub_at_the_cell_its_line_ended_in() {
        let mut cfg = config();
        cfg.theme_bg = 0x0011_1318;
        let (x0, x1) = (12 + 26 * 14 - 4, 12 + 27 * 14);
        let (y0, y1) = (2 + 11 * 26, 2 + 12 * 26);
        let lift = |past: i32, in_guard: bool| -> i32 {
            let mut peak = 0;
            for after in [20u64, 35, 60, 90, 120, 156] {
                let (sc, g) = gallery_return(&cfg, after, Some(past), in_guard);
                let px = composite_scratch(&sc, g, cfg.theme_bg);
                let w = usize::from(g.win_w);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let p = px[y as usize * w + x as usize];
                        for sh in [16, 8, 0] {
                            let d =
                                ((p >> sh) & 0xff) as i32 - ((cfg.theme_bg >> sh) & 0xff) as i32;
                            peak = peak.max(d);
                        }
                    }
                }
            }
            peak
        };
        let kept = lift(1, false);
        let stub = lift(0, true);
        assert!(
            kept <= 12,
            "the old caret cell lifts by {kept} across the flight"
        );
        assert!(
            stub >= 30,
            "control: cut at the letters the stub lifts only {stub}"
        );
    }

    /// Along the gallery's dark `enter-bottom` Return, every column whose
    /// brightest pixel is a warm hue holds it at a clean level, few of them
    /// dark; laid at the other stops' level (the negative control), many are.
    #[test]
    fn a_dark_return_s_warm_stops_read_clean_not_brown() {
        let mut cfg = config();
        cfg.theme_bg = 0x0011_1318;
        let muddy = |dim: bool| -> (usize, usize) {
            let (mut muddy, mut warm) = (0usize, 0usize);
            for after in [20u64, 35, 60, 90, 120, 156, 200] {
                let g = gallery_geom();
                let t0 = Instant::now();
                let ctx_g = |now: Instant| Ctx {
                    geom: g,
                    ..ctx_at(now, &cfg, (13, 2), 0.25)
                };
                let mut m = Meteors::new();
                m.warm_stops_plain = dim;
                m.on_event(&mv_as((11, 26), (13, 2), Licence::Return), t0, &ctx_g(t0))
                    .expect("a Return flies");
                let mut sc = Scratch::default();
                sc.emit(&mut m, &ctx_g(t0 + ms(after)));
                let px = composite_scratch(&sc, g, cfg.theme_bg);
                let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
                for x in 0..w {
                    let peak = (0..h)
                        .map(|y| px[y * w + x])
                        .max_by(|a, b| {
                            let v = |p: u32| crate::color_math::rgb2hsv(p).2;
                            v(*a).total_cmp(&v(*b))
                        })
                        .unwrap_or(cfg.theme_bg);
                    let (hue, sat, val) = crate::color_math::rgb2hsv(peak);
                    if val < 0.16 || sat < 0.3 || !(hue < 70.0 || hue >= 330.0) {
                        continue;
                    }
                    warm += 1;
                    if val < 0.6 {
                        muddy += 1;
                    }
                }
            }
            (muddy, warm)
        };
        let (clean, warm) = muddy(false);
        let (dim, dim_warm) = muddy(true);
        assert!(
            warm > 0 && dim_warm > 0,
            "fixture: the Return lays warm stops"
        );
        assert!(
            clean * 5 <= warm,
            "{clean} of {warm} warm columns of the Return read dark"
        );
        assert!(
            dim * 2 > dim_warm,
            "control: at the other stops' level only {dim} of {dim_warm} warm columns read dark"
        );
    }

    /// The 8-connected runs of lit pixels in `px` (a pixel off `page` by
    /// more than `delta` in any channel), their sizes, largest first.
    fn lit_runs(px: &[u32], w: usize, page: u32, delta: i32) -> Vec<usize> {
        let lit = |p: u32| {
            [16u32, 8, 0]
                .iter()
                .any(|&sh| (((p >> sh) & 0xff) as i32 - ((page >> sh) & 0xff) as i32).abs() > delta)
        };
        let h = px.len() / w;
        let mut seen = vec![false; px.len()];
        let mut runs = Vec::new();
        for start in 0..px.len() {
            if seen[start] || !lit(px[start]) {
                continue;
            }
            let (mut n, mut stack) = (0usize, vec![start]);
            seen[start] = true;
            while let Some(i) = stack.pop() {
                n += 1;
                let (x, y) = ((i % w) as i32, (i / w) as i32);
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (xx, yy) = (x + dx, y + dy);
                        if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 {
                            continue;
                        }
                        let k = yy as usize * w + xx as usize;
                        if !seen[k] && lit(px[k]) {
                            seen[k] = true;
                            stack.push(k);
                        }
                    }
                }
            }
            runs.push(n);
        }
        runs.sort_unstable_by(|a, b| b.cmp(a));
        runs
    }

    /// The gallery's judges (looks7 M2, after looks6 round 3): on paper the
    /// `enter-bottom` Return's stick was cut in two over the printed line's
    /// blank cell after `fox` (the line's span held a dark train behind the
    /// cell after its last letter, and the paper train with it), thinned to
    /// beads on olive necks through its yellow, and its fades laid its
    /// darkened amber thin over the paper, tan and khaki (`b5a053`,
    /// `cf9859`, `d3a46b`), as its thread's fade still laid its gold
    /// (`c79b58`). Along the gallery's Return the stick is one run
    /// of lit pixels whose width rises to one peak and falls from it (a
    /// pixel of anti-aliasing aside), and no column's core (its most
    /// saturated pixel) reads tan or khaki, nor olive but in the one column
    /// where the walk steps from gold to green (looks8 round 2's K10b: it
    /// read olive for three). Negative controls: the span held a cell past
    /// the letters cuts the stick in two; the fades laid as dims read tan;
    /// the caret's bend walks through the olives; blended across the
    /// bend's skip, the turn reads olive for columns.
    #[test]
    fn a_paper_return_lays_one_clean_stick() {
        let mut cfg = config();
        cfg.dark_theme = false;
        cfg.theme_fg = 0x001F_2328;
        cfg.theme_bg = 0x00FF_FFFF;
        let page = cfg.theme_bg;
        // (most runs, worst pinch px, tan cores, widest olive crossing in
        // columns) over the flight.
        let stick = |past_cell: bool,
                     dims: bool,
                     plain: bool,
                     blended: bool|
         -> (usize, usize, usize, usize) {
            let (mut runs, mut pinch, mut tan, mut olive) = (0usize, 0usize, 0usize, 0usize);
            for after in [20u64, 35, 60, 90, 120, 156] {
                let g = gallery_geom();
                let t0 = Instant::now();
                let ctx_g = |now: Instant| Ctx {
                    geom: g,
                    ..ctx_at(now, &cfg, (13, 2), 0.25)
                };
                let mut m = Meteors::new();
                m.dims_dark_trains = dims;
                m.warm_stops_plain = plain;
                m.train_skip_blended = blended;
                let (ox, oy, cw, ch) = (12, 2, 14, 26);
                let reach = i32::from(past_cell);
                m.keep_out.set_rows([
                    (ox, ox + (26 + reach) * cw, oy + 11 * ch, oy + 12 * ch),
                    (ox, ox + (19 + reach) * cw, oy + 12 * ch, oy + 13 * ch),
                ]);
                m.on_event(&mv_as((11, 26), (13, 2), Licence::Return), t0, &ctx_g(t0))
                    .expect("a Return flies");
                let mut sc = Scratch::default();
                sc.emit(&mut m, &ctx_g(t0 + ms(after)));
                // The stick alone: the landing's marks are the sky's.
                sc.out.clear();
                sc.halos.clear();
                let px = composite_scratch(&sc, g, page);
                let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
                runs = runs.max(lit_runs(&px, w, page, 24).len());
                let mut widths = Vec::new();
                let mut crossing = 0usize;
                for x in 0..w {
                    let (mut n, mut core) = (0usize, None::<(f32, u32)>);
                    for y in 0..h {
                        let p = px[y * w + x];
                        let lit = [16u32, 8, 0].iter().any(|&sh| {
                            (((p >> sh) & 0xff) as i32 - ((page >> sh) & 0xff) as i32).abs() > 24
                        });
                        if !lit {
                            continue;
                        }
                        n += 1;
                        let sat = crate::color_math::rgb2hsv(p).1;
                        if core.is_none_or(|(s, _)| sat > s) {
                            core = Some((sat, p));
                        }
                    }
                    widths.push(n);
                    if let Some((sat, p)) = core {
                        let (hue, _, val) = crate::color_math::rgb2hsv(p);
                        // A tint of ochre: a warm mid value, half grey.
                        if (20.0..50.0).contains(&hue) && (0.2..0.6).contains(&sat) && val < 0.86 {
                            tan += 1;
                        }
                        if (48.0..80.0).contains(&hue) && val < 0.85 {
                            crossing += 1;
                            olive = olive.max(crossing);
                        } else {
                            crossing = 0;
                        }
                    } else {
                        crossing = 0;
                    }
                }
                // Away from its widest column, each column is no wider than
                // the last but by a pixel of anti-aliasing; a pinch is how
                // far it narrows before it widens again.
                let Some(peak) = (0..w).max_by_key(|&x| widths[x]) else {
                    continue;
                };
                for side in [-1i32, 1] {
                    let mut least = widths[peak];
                    let mut x = peak as i32 + side;
                    while x >= 0 && (x as usize) < w && widths[x as usize] > 0 {
                        let here = widths[x as usize];
                        least = least.min(here);
                        pinch = pinch.max(here.saturating_sub(least + 1));
                        x += side;
                    }
                }
            }
            (runs, pinch, tan, olive)
        };
        let (runs, pinch, tan, olive) = stick(false, false, false, false);
        assert_eq!(runs, 1, "the paper Return's stick falls into {runs} runs");
        assert_eq!(pinch, 0, "the stick pinches by {pinch} px and widens again");
        assert_eq!(tan, 0, "{tan} columns of the stick read tan at their core");
        assert!(
            olive <= 1,
            "the stick reads olive across {olive} columns in a row"
        );
        let (_, _, _, blended) = stick(false, false, false, true);
        assert!(
            blended >= olive + 2,
            "control: blended across the bend's skip the stick reads olive across only \
             {blended} columns ({olive})"
        );
        let (cut, ..) = stick(true, false, false, false);
        assert!(
            cut >= 2,
            "control: held a cell past the letters the stick is one run"
        );
        let (_, _, dim_tan, _) = stick(false, true, false, false);
        assert!(
            dim_tan >= 10,
            "control: its fades laid as dims read tan in only {dim_tan} columns"
        );
        let (_, _, _, bent) = stick(false, false, true, false);
        assert!(
            bent >= olive + 2,
            "control: the caret's bend reads olive across only {bent} columns ({olive})"
        );
    }

    /// On a dark page a Return's landing lays no pin: its crosshair stood
    /// on the new caret after the gallery's dark Return, a hairline left of
    /// the block and a tick on its top (looks7 M3, `e120`). A jump's landing
    /// (the negative control) pins its caret.
    #[test]
    fn a_dark_return_lands_without_a_pin() {
        let cfg = config();
        assert!(cfg.dark_theme, "fixture: the page is dark");
        let t0 = Instant::now();
        let pin = |licence: Licence| -> usize {
            let (from, to) = ((5u16, 40u16), (7u16, 2u16));
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            let spawn = m
                .on_event(&mv_as(from, to, licence), t0, &ctx)
                .expect("a meteor flies");
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_at(t0 + spawn.t_flight + ms(4), &cfg, to, 0.25));
            let l = *m.landings.first().expect("fixture: the meteor landed");
            let mut n = 0;
            for after in [8u64, 20, 40, 80] {
                sc.clear();
                {
                    let at = ctx_at(t0 + spawn.t_flight + ms(after), &cfg, to, 0.25);
                    let mut fr = sc.frame();
                    draw_pin(&l, &at, &mut fr);
                }
                n += sc.out.len() + sc.halos.len();
            }
            n
        };
        let peel = pin(Licence::Return);
        assert_eq!(peel, 0, "a dark Return's landing lays {peel} pin lights");
        assert!(
            pin(Licence::Nav) > 0,
            "control: a jump's landing lays no pin"
        );
    }

    /// The gallery's judges (looks7 M1): after a dark Return in
    /// `enter-bottom` coloured dabs floated in the gap under `brown fox`
    /// and beside the train's head: the head's coma, seven lights orbiting
    /// it apart, laid where the train passed behind the echo and the
    /// printed line. Over the gallery's Return (its landing's marks aside)
    /// the lit pixels are one connected run or none; ringed with its coma
    /// and laid behind the letters' bodies alone (the negative control),
    /// they fall apart.
    #[test]
    fn a_dark_return_lays_one_train_or_none() {
        let mut cfg = config();
        cfg.theme_bg = 0x0011_1318;
        let page = cfg.theme_bg;
        let runs = |coma: bool| -> (usize, usize) {
            let (mut worst, mut lit) = (0usize, 0usize);
            for after in [10u64, 20, 35, 50, 70, 90, 120, 156, 200] {
                let g = gallery_geom();
                let t0 = Instant::now();
                let ctx_g = |now: Instant| Ctx {
                    geom: g,
                    ..ctx_at(now, &cfg, (13, 2), 0.25)
                };
                let mut m = Meteors::new();
                m.peel_coma = coma;
                let (ox, oy, cw, ch) = (12, 2, 14, 26);
                m.keep_out.set_rows_past(
                    [
                        (ox, ox + 26 * cw, oy + 11 * ch, oy + 12 * ch),
                        (ox, ox + 19 * cw, oy + 12 * ch, oy + 13 * ch),
                    ],
                    cw,
                );
                m.on_event(&mv_as((11, 26), (13, 2), Licence::Return), t0, &ctx_g(t0))
                    .expect("a Return flies");
                // The flight alone: the landing's marks are the landing's.
                for me in &mut m.live {
                    me.landed = true;
                }
                let mut sc = Scratch::default();
                sc.emit(&mut m, &ctx_g(t0 + ms(after)));
                let px = composite_scratch(&sc, g, page);
                let n = lit_runs(&px, usize::from(g.win_w), page, 24).len();
                worst = worst.max(n);
                lit += usize::from(n > 0);
            }
            (worst, lit)
        };
        let (worst, lit) = runs(false);
        assert!(lit >= 4, "fixture: the Return shows in only {lit} frames");
        assert!(
            worst <= 1,
            "the dark Return's light falls into {worst} runs"
        );
        let (ringed, _) = runs(true);
        assert!(
            ringed >= 3,
            "control: ringed with its coma it lays only {ringed} runs"
        );
    }
}

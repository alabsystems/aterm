// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stardust: the sky above the ribbon.

use aterm_render::{GlowQuad, HaloMode, RainHalo, add_sat, halo_row_ny, halo_weight, premul_rgb};
use aterm_time::Instant;

use super::companion::{KeepOut, PetShade};
use super::ribbon::paper_mark_ink;
use super::timing::{
    CHROMA_CULL_ALPHA, EDGE_IN_S, FIELD_STAR_COV_CEIL, GLINT_CAP, GLINT_REFILL_PER_S,
    KILL_RETRACT_BASE_MS, REDUCED_MOTION_FADE_MS, STAR_CULL_ALPHA, STAR_STACK_ADD,
    TRANSIENT_STAR_COV_CEIL, clamp01, edge_in, half_life, half_life_life_s, kill_span_ms,
    smoothstep01, spend,
};
use super::{Cadence, Config, Ctx, Event, Frame, TypedClass};
#[cfg(test)]
use crate::color_math::relative_luminance;
use crate::color_math::{
    RelativeLuminanceTable, relative_luminance_table, relative_luminance_with,
};
use crate::cursor_glow::{
    BandPx, Geom, InkRole, RAINBOW_CARET_LIGHT_FLOOR, RAINBOW_SPARKLE_LIGHT_SHARE, SoundCue,
    band_row,
};
use crate::effect_util::{
    STAR_ARM_FINE, STAR_ARM_HERO, STAR_ARM_INK, STAR_ARM_STD, STAR_CORE, STAR_CORE_ADD, STAR_GLINT,
    STAR_GLINT_COV, STAR_OVER_CENTRE_LAYS, STAR_OVER_LAYS, STAR_TAPER_BODY, STAR_WAIST,
    TWINKLE_GLINT_FRAC, TWINKLE_OMEGA, push_fx_rect, push_twinkle_star, star_arm, star_body_px,
    twinkle_env, twinkle_peak,
};
use crate::rainbow_kitty::meteor::{LandingWalk, tri};
use crate::rainbow_kitty::ribbon::{
    BIRTH_EDGE_FLOOR, EXPIRY_MELT_SHARE, Ribbon, SWOOSH_TOTAL_S, WALK_FAST_CELLS, expiry_melt,
    reduced_fade,
};
use crate::spectrum::{
    SPECTRUM_ANCHOR_AT, SPECTRUM_ANCHORS, SPECTRUM_LUT, SPECTRUM_LUT_LEN, spectrum, spectrum_snap,
};
#[cfg(test)]
use crate::spectrum::{SPECTRUM_STOPS, spectrum_stop};
use crate::trail_sound::SoundKind;

/// The live cap (§5.6); typical at 12 cps is ≈ 20.
pub const STAR_CAP: usize = 40;

/// How long an evicted star has to finish (§5.6): its envelope is multiplied by
/// `spend(age_since_eviction / 40 ms)`, so it leaves on the theme's own curve and never pops (T5).
pub const EVICT_FINISH_MS: f32 = 40.0;

/// The catch's shortened life, ms: a gold m1 the resident pet reached for is put on this finish on
/// the frame the paw lands, so it leaves on the theme's curve rather than blinking out (T5).
pub const CATCH_FINISH_MS: f32 = EVICT_FINISH_MS;

/// How many finishing stars the pool holds beyond [`STAR_CAP`] before the one nearest the end of
/// its finish is dropped.
pub const STAR_FINISH_SLACK: usize = 8;

/// The pool's fixed capacity: [`STAR_CAP`] live plus [`STAR_FINISH_SLACK`] finishing.
pub const STAR_POOL_MAX: usize = STAR_CAP + STAR_FINISH_SLACK;

/// The strike deal: one in this many typed cells gets an m3 grain (§5.6).
pub const DEAL_M3_IN: u32 = 2;

/// One in this many typed cells gets an m2 (§5.6).
pub const DEAL_M2_IN: u32 = 4;

/// One in this many typed cells gets a dealt m1 hero (§5.6).
pub const DEAL_M1_IN: u32 = 12;

/// One in this many laid ribbon cells carries a field star (§5.6), hashed stably per cell so the
/// sky does not shimmer as the ribbon re-plans.
pub const FIELD_DEAL_IN: u32 = 2;

/// One in this many field stars is an m2 rather than an m3.
pub const FIELD_M2_IN: u32 = 2;

/// The hot field deal: every laid cell carries a field star once the honest spine (`ctx.disp`, not
/// the resume-floored birth spine) is at or over [`FIELD_HOT_DISP`].
pub const FIELD_DEAL_IN_HOT: u32 = 1;

/// The spine at or above which the field deal is [`FIELD_DEAL_IN_HOT`] instead of
/// [`FIELD_DEAL_IN`].
pub const FIELD_HOT_DISP: f32 = 0.7;

/// The share of laid cells that carry a field star at flow's `heat == 1`: every one.
pub const FIELD_SHARE_FLOW: f32 = 1.0 / FIELD_DEAL_IN_HOT as f32;

/// The share of laid cells that carry a field star at `heat == 0`: [`FIELD_DEAL_IN`] as a
/// probability.
pub const FIELD_SHARE_COLD: f32 = 1.0 / FIELD_DEAL_IN as f32;

/// The combo ladder's first rung: every 16th key of a run takes an m2 where the strike deal gave a
/// grain or nothing (§23, "Flow state").
pub const LADDER_M2_KEYS: u32 = 16;

/// The second rung: every 32nd key of a run takes a white m2 ([`TINT_WHITE_RGB`]), so it reads as a
/// different mark from the first.
pub const LADDER_WHITE_KEYS: u32 = 32;

/// The top rung: every 64th key of a run takes an earned gold m1 ([`TINT_GOLD_RGB`]), never demoted
/// by spacing or a dry bucket (§5.8, D5).
pub const LADDER_GOLD_KEYS: u32 = 64;

/// A field star's life, s: a star cannot read its cell per frame, so it rides the cell's shape
/// (`edge-in` × the ribbon's `expiry_melt`) over the horizon at which the cell is surely gone, the
/// exit swoosh's whole span (1.69 s; §5.6).
pub const FIELD_LIFE_S: f32 = SWOOSH_TOTAL_S;

/// A field star's hold after the last key on its row ([`Star::lit`]), ms: the `H` of the field
/// lane's half-life envelope (§5.6).
pub const FIELD_HOLD_MS: f32 = 300.0;

/// A field star's half-life after hold, ms.
pub const FIELD_TAU_MS: f32 = 250.0;

/// The field fade's span after the last key, seconds (`H + 2.32·τ` through [`half_life_life_s`],
/// 0.88 s): when [`Star::dead`] drops a field star whose row the hand has left, and the end of the
/// reduced-motion fade.
#[inline]
#[must_use]
pub const fn field_fade_life_s() -> f32 {
    half_life_life_s(FIELD_HOLD_MS / 1000.0, FIELD_TAU_MS / 1000.0)
}

const FIELD_FADE_LIFE_S: f32 = field_fade_life_s();

/// The spine above which a strike also throws a second m3 from the caret's leading edge (§5.6).
pub const STRIKE_SECOND_M3_DISP: f32 = 0.5;

/// Minimum spacing between heroes, in cells (§5.2).
pub const HERO_MIN_SPACING_CELLS: u16 = 3;

/// One sky star per cell: a strike dealt onto a cell that already carries a live sky star moves up
/// to this many cells toward the caret's leading edge (the cell the next key lays) or is dropped;
/// further would put a star over glass the hand has not reached.
pub const STRIKE_REACH_CELLS: u16 = 1;

/// At most this many heroes live on one row at a time (§5.2).
pub const HERO_MAX_LIVE_PER_ROW: u8 = 2;

/// Share of m1/m2 stars whose tint is `spectrum_snap(field at the birth cell)` (§5.3): warm stars
/// over the old end, violet over the new.
pub const TINT_FIELD_SHARE: f32 = 0.75;

/// Share of m1/m2 that are `#FFFFFF` — an A-type star; halo and stubs white (§5.3).
pub const TINT_WHITE_SHARE: f32 = 0.10;

/// How far from white toward its tint an m2's body and nucleus are drawn.
pub const M2_BODY_TINT: f32 = 0.6;

/// Share that are `#FFFF00` gold, the only class that carries diagonal glints (§5.3).
pub const TINT_GOLD_SHARE: f32 = 1.0 - TINT_FIELD_SHARE - TINT_WHITE_SHARE;

/// The gold tint itself (§5.3), and the only value of [`Star::tint`] for which
/// `push_twinkle_star`'s `gold` argument may be true.
pub const TINT_GOLD_RGB: u32 = 0x00FF_FF00;

/// The A-type white tint (§5.3), and the colour of the hero's body and nucleus (D2).
pub const TINT_WHITE_RGB: u32 = 0x00FF_FFFF;

/// How many of the seven ROYGBIV stops count as warm for §5.5's φ + π flip: red, orange, yellow.
pub const WARM_STOPS: usize = 3;

/// Top of the sky band above the ribbon, in `ch` above [`super::ribbon::Band`]'s `top` (§5.4).
pub const SKY_TOP_CH: f32 = 0.30;

/// Bottom of the sky band above the ribbon, in `ch` above the band's top.
pub const SKY_BOTTOM_CH: f32 = 0.04;

/// Horizontal jitter of a birth about the cell centre, in `cw` (§5.4, hashed).
pub const SKY_JITTER_CW: f32 = 0.3;

/// The 1× cell height, device px (7×14 at 1×, 15×28 on a retina screen).
pub const CELL_1X_CH: f32 = 14.0;

/// The device-pixel scale of a cell of height `ch` against the 1× cell: 1 at 1×, 2 on the retina
/// cell, 9/7 at the test fixture's `ch` 18.
#[inline]
#[must_use]
pub const fn px_scale(ch: f32) -> f32 {
    ch * (1.0 / CELL_1X_CH)
}

/// An integer pixel size ruled at 1×, at cell height `ch`: truncated like `star_arm_px` (never
/// rounding up on a fractional scale), floored at one pixel.
#[inline]
#[must_use]
pub const fn px_at(ch: f32, at_1x: i32) -> i32 {
    let scaled = (at_1x as f32 * px_scale(ch)) as i32;
    if scaled > 1 { scaled } else { 1 }
}

/// Lower bound of the seeded arm-size scintillation rate, Hz (§5.5, D12).
pub const SCINT_F_MIN: f32 = 8.0;

/// Upper bound of the seeded arm-size rate, Hz (§5.5, D12).
pub const SCINT_F_MAX: f32 = 12.0;

/// The photosensitivity exemption bound, device px at 2× (§5.5): a mark whose bbox is at or under
/// this may scintillate in size at 8-12 Hz, since a general-flash threshold is an area threshold.
#[cfg(test)]
pub const SCINT_BBOX_MAX_PX: i32 = 10;

/// The m2's integer arm floor, px (§5.2, §5.5: "m2 … (2 ↔ 3 px)").
pub const M2_ARM_MIN_PX: i32 = 2;

/// Light-theme count scale (§5.2): counts × 0.6 in every population, since area must not accumulate
/// on a light ground (L6).
pub const LIGHT_COUNT_SCALE: f32 = 0.6;

/// Coverage divisor for a transient star whose current pixel sits over a probed-occupied or unknown
/// cell (§5.2's text-safe arm).
pub const TEXT_SAFE_DIV: f32 = 3.0;

/// The m1 hero's coverage request in the sky lane (§5.2).
pub const M1_COV_SKY: f32 = 61.0;

/// The m2's coverage request in the sky lane (§5.2).
pub const M2_COV_SKY: f32 = 48.0;

/// The m3 grain's coverage request in the sky lane (§5.2).
pub const M3_COV_SKY: f32 = 61.0;

/// The m1's request in the transient lane (§5.2).
pub const M1_COV_TRANSIENT: f32 = 50.0;

/// The m2's request in the transient lane (§5.2).
pub const M2_COV_TRANSIENT: f32 = 34.0;

/// The m3's request in the transient lane (§5.2).
pub const M3_COV_TRANSIENT: f32 = 38.0;

/// The m2's 3×3 disc, as a share of `c` (§5.7): what an m2 adds on top of `push_twinkle_star`'s
/// stack.
pub const M2_DISC_ADD: f32 = 0.35;

/// The m3's four arm rects, as a share of `c`: the grain's tinted Airy disc (§5.7, D2), each arm
/// `arm` px outboard of the 1-px core.
pub const M3_CROSS_ADD: f32 = 0.45;

/// The m3's composited centre as a multiple of `c`: §5.2's `1.6·c` (98 sky / 61 transient).
pub const M3_CENTRE_ADD: f32 = 1.6;

/// The four outboard tint stubs, as a share of `c` (§5.7): each arm's coloured tip, outside
/// `star_body_px(arm)`.
pub const STUB_COV_SHARE: f32 = 1.0;

/// How long each tint stub is, px at the 1× cell ([`CELL_1X_CH`]), running outboard from
/// `star_body_px(arm) + 1`: two, so the arm visibly ends in colour.
pub const STUB_LEN_PX: i32 = 2;

/// [`STUB_LEN_PX`] at cell height `ch` ([`px_at`]).
#[inline]
#[must_use]
pub const fn stub_len_px(ch: f32) -> i32 {
    px_at(ch, STUB_LEN_PX)
}

/// §3.2 rank 8: the halo may not outshine the core.
pub const HALO_CAP_SHARE: f32 = 1.0;

/// The m1 halo's peak, as a share of `c` (§5.7): the hero's whole request, 61 levels in the sky and
/// 50 transient.
pub const M1_HALO_SHARE: f32 = 1.0;

/// The m2 halo's peak, as a share of `c` (§5.7): `0.80 · 48 = 38` levels in the sky, 27 transient.
pub const M2_HALO_SHARE: f32 = 0.80;

/// The m1 halo's radius as a multiple of its nominal arm (`star_arm(ch, STAR_ARM_HERO)`; §5.7):
/// 8 px at 1×, 11 at `ch` 18, 16 at 2×.
pub const M1_HALO_R_ARM: f32 = 3.0;

/// The m2 halo's radius as a multiple of its nominal arm (`star_arm(ch, STAR_ARM_STD)`, §5.7),
/// floored at [`M2_HALO_R_MIN_PX`].
pub const M2_HALO_R_ARM: f32 = 2.4;

/// The m2 halo's radius floor in px at the 1× cell (§5.7's `max(6, 2.4·arm)`): below 6 px a radial
/// falloff around a 5 px body reads as a second core.
pub const M2_HALO_R_MIN_PX: f32 = 6.0;

/// [`M2_HALO_R_MIN_PX`] at cell height `ch` ([`px_scale`]).
#[inline]
#[must_use]
pub const fn m2_halo_r_min_px(ch: f32) -> f32 {
    M2_HALO_R_MIN_PX * px_scale(ch)
}

/// The m2 disc's shoulder: how far the tinted disc reaches past the centre pixel on each axis, px
/// at the 1× cell.
pub const M2_DISC_SHOULDER_PX: i32 = 1;

/// [`M2_DISC_SHOULDER_PX`] at cell height `ch` ([`px_at`]).
#[inline]
#[must_use]
pub const fn m2_disc_shoulder_px(ch: f32) -> i32 {
    px_at(ch, M2_DISC_SHOULDER_PX)
}

/// The halo breathes its hue: how far, in `SPECTRUM_LUT` entries, a halo's colour walks from its
/// star's stop on the luminance clock (+15 at the twinkle peak, −15 at the trough; [`halo_rgb`]).
pub const HALO_HUE_SWING_ENTRIES: i32 = 15;

/// The caret law on the star's own centre (`cursor_glow` §8 d: the brightest pixel of the frame is
/// under the cursor): the relative luminance no pixel a sky star composites may exceed, over the
/// ground it sits on.
pub const STAR_LIGHT_CEIL: f32 = RAINBOW_CARET_LIGHT_FLOOR * RAINBOW_SPARKLE_LIGHT_SHARE / 255.0;

/// The veil pool's fixed capacity, reserved once (§18).
pub const AURORA_CAP: usize = 64;

/// How tall the veil is, in `ch`: the box is `[bottom − 0.35 ch, bottom)`, `bottom` being the sky
/// band's anchor (the ribbon's top under `tall`, the glyph row's top under `underline`, never lower
/// than the glyph row's top).
pub const AURORA_TALL_CH: f32 = 0.35;

/// The veil's peak coverage byte at full momentum, at the line: its brightest pixel, since the
/// falloff is centred on the box's bottom edge and each veil is clipped to its own column (no two
/// veils sum).
pub const AURORA_COV_PEAK: f32 = 40.0;

/// Below this birth spine a key lays no veil.
pub const AURORA_COLD_DISP: f32 = 0.15;

/// The veil's horizontal falloff radius in `cw`, centred on the cell: at the cell's own edge
/// (`dx = 0.5 cw`) the weight is `0.88`, so a row of clipped per-cell veils reads as one band with
/// a 12 % scallop, not as beads.
pub const AURORA_REACH_CW: f32 = 2.0;

/// Veil halos stardust may draw per frame: one `RainHalo` per live cell (a veil never splits across
/// rows).
pub const AURORA_HALO_BUDGET: usize = AURORA_CAP;

/// The composited centre of a sky m1 — `STAR_STACK_ADD · c` (§5.2, §3.2 rank 4).
pub const CENTRE_M1_SKY: f32 = STAR_STACK_ADD * M1_COV_SKY;

/// The composited centre of a sky m2 — stack plus the [`M2_DISC_ADD`] disc.
pub const CENTRE_M2_SKY: f32 = (STAR_STACK_ADD + M2_DISC_ADD) * M2_COV_SKY;

/// The composited centre of a sky m3 — the core rect alone, priced at [`M3_CENTRE_ADD`] (the arms
/// are outboard and stack nothing on it).
pub const CENTRE_M3_SKY: f32 = M3_CENTRE_ADD * M3_COV_SKY;

/// The composited centre of a transient m1.
pub const CENTRE_M1_TRANSIENT: f32 = STAR_STACK_ADD * M1_COV_TRANSIENT;

/// The composited centre of a transient m2: `STAR_STACK_ADD · c = 80` (§5.2, §3.2 rank 4).
pub const CENTRE_M2_TRANSIENT: f32 = STAR_STACK_ADD * M2_COV_TRANSIENT;

/// The composited centre of a transient m3: `1.6 · 38 = 60.8`, published as 61.
pub const CENTRE_M3_TRANSIENT: f32 = M3_CENTRE_ADD * M3_COV_TRANSIENT;

const _: () = {
    assert!(
        CENTRE_M1_SKY <= FIELD_STAR_COV_CEIL
            && CENTRE_M2_SKY <= FIELD_STAR_COV_CEIL
            && CENTRE_M3_SKY <= FIELD_STAR_COV_CEIL,
        "a sky star's composited centre exceeds the sparkle law's ceiling"
    );
    assert!(
        CENTRE_M1_TRANSIENT <= TRANSIENT_STAR_COV_CEIL
            && CENTRE_M2_TRANSIENT <= TRANSIENT_STAR_COV_CEIL
            && CENTRE_M3_TRANSIENT <= TRANSIENT_STAR_COV_CEIL,
        "a transient star's composited centre exceeds the over-ink ceiling"
    );
    assert!(
        CENTRE_M3_SKY < CENTRE_M2_SKY && CENTRE_M2_SKY < CENTRE_M1_SKY,
        "the sky magnitudes must stay ordered m3 < m2 < m1"
    );
    assert!(
        M1_HALO_SHARE < HALO_CAP_SHARE * STAR_STACK_ADD
            && M2_HALO_SHARE < HALO_CAP_SHARE * STAR_STACK_ADD,
        "a star halo outshines the core it belongs to (§3.2 rank 8)"
    );
    assert!(
        M2_BODY_TINT > 0.0 && M2_BODY_TINT <= 1.0,
        "the m2 body tint is a share of the way from white to the tint"
    );
    assert!(
        HALO_HUE_SWING_ENTRIES > 0 && HALO_HUE_SWING_ENTRIES * 2 < 63,
        "the halo's hue breath reaches a neighbouring stop"
    );
    assert!(
        AURORA_COV_PEAK > 0.0 && AURORA_COV_PEAK <= 40.0,
        "the aurora's brightest pixel is over the brief's ~40"
    );
    assert!(
        AURORA_TALL_CH > 0.0 && AURORA_TALL_CH <= 0.35 && AURORA_HALO_BUDGET >= AURORA_CAP,
        "the aurora's box is taller than the brief's 0.35 ch, or its pool outruns its budget"
    );
    assert!(
        M3_CENTRE_ADD * 0.55 >= 1.3 * M3_CROSS_ADD,
        "the grain's disc outshines its core at the twinkle trough"
    );
    assert!(
        TINT_GOLD_SHARE > 0.0 && TINT_FIELD_SHARE + TINT_WHITE_SHARE + TINT_GOLD_SHARE == 1.0,
        "the 75/10/15 tint deal must sum to one"
    );
    assert!(
        1.0 / DEAL_M1_IN as f32 + 1.0 / DEAL_M2_IN as f32 + 1.0 / DEAL_M3_IN as f32 <= 1.0,
        "the 1-in-12 / 1-in-4 / 1-in-2 strike deals must fit inside one draw"
    );
};

/// Sky m1 hold, ms (§5.6 — strike m1 `120/146/460`).
pub const HOLD_M1_SKY_MS: f32 = 120.0;
/// Sky m1 half-life, ms.
pub const TAU_M1_SKY_MS: f32 = 146.0;
/// Sky m2 hold, ms (§5.6 — `80/112/340`).
pub const HOLD_M2_SKY_MS: f32 = 80.0;
/// Sky m2 half-life, ms.
pub const TAU_M2_SKY_MS: f32 = 112.0;
/// Sky m3 hold, ms (§5.6: `40/80/225`).
pub const HOLD_M3_SKY_MS: f32 = 40.0;
/// Sky m3 half-life, ms.
pub const TAU_M3_SKY_MS: f32 = 80.0;

/// Transient m1 hold, ms (§5.6 — the landing fan's hero, `40/118/315`).
pub const HOLD_M1_TRANSIENT_MS: f32 = 40.0;
/// Transient m1 half-life, ms.
pub const TAU_M1_TRANSIENT_MS: f32 = 118.0;
/// Transient m2 hold, ms (§5.6 — erase / fan / mini-fan m2, `30/93/245`).
pub const HOLD_M2_TRANSIENT_MS: f32 = 30.0;
/// Transient m2 half-life, ms.
pub const TAU_M2_TRANSIENT_MS: f32 = 93.0;
/// Transient m3 hold, ms (§5.6 — erase / shed / fan / mini-fan m3, `20/67/175`).
pub const HOLD_M3_TRANSIENT_MS: f32 = 20.0;
/// Transient m3 half-life, ms.
pub const TAU_M3_TRANSIENT_MS: f32 = 67.0;

/// The sky lift for an m1/m2 strike star, px per second (§5.6: "lift −3 px/s, integer-stepped,
/// 1-2 px over life").
pub const SKY_LIFT_PX_PER_S: f32 = -3.0;

/// [`SKY_LIFT_PX_PER_S`] at cell height `ch` ([`px_scale`]).
#[inline]
#[must_use]
pub const fn sky_lift_px_per_s(ch: f32) -> f32 {
    SKY_LIFT_PX_PER_S * px_scale(ch)
}

/// The slipstream's share: a sky star born under a hand at speed inherits a backward drift of
/// `STREAM_SHARE_CW · cw / IOI` px/s, `IOI` being the observed interval between the last two Typed
/// keys (T4): 3.0 cells of whole travel at 12 cps, 2.0 at 8 cps, nothing at a stroll.
pub const STREAM_SHARE_CW: f32 = 1.05;
/// The drift's decay, ms: `v(age) = v₀ · e^(−age/τ)`, so the sky slows from the instant it is born
/// and a star's travel is bounded whatever the hand does next.
pub const STREAM_TAU_MS: f32 = 300.0;
/// Where the drift ends, ms after birth: the longest sky class life (strike m1, 460), so the sky is
/// at rest one star-life after the last key.
pub const STREAM_END_MS: f32 = 460.0;
/// `1 − e^(−STREAM_END_MS / STREAM_TAU_MS)`, the share of the asymptote spent by the drift's end,
/// as a literal.
pub const STREAM_SPENT: f32 = 0.784_11;
/// The IOI clamp's fast end, ms: the fastest priced hand (33 cps: 525 px/s on a 15 px cell,
/// 8.2 cells of travel, frontier-clamped).
pub const STREAM_IOI_MIN_MS: f32 = 30.0;
/// The IOI clamp's slow end, ms — and the IOI a session's first key is priced at, having no
/// interval to read (under the stroll gate: nothing).
pub const STREAM_IOI_MAX_MS: f32 = 600.0;
/// The stroll gate, open: an IOI at or under this (≥ 6.7 cps) streams at the whole priced velocity.
pub const STREAM_IOI_RUN_MS: f32 = 150.0;
/// The stroll gate, closed: an IOI at or over this (≤ 3 cps) streams nothing; between the two gates
/// the velocity is smoothstepped in.
pub const STREAM_IOI_STROLL_MS: f32 = 333.0;
/// The spine gate: only a star born at `birth_disp ≥ 0.5` inherits the drift — the second grain's
/// own threshold.
pub const STREAM_DISP: f32 = 0.5;

/// The exhaust's share, cells of `cw` per key interval (§33): the near layer, four times the sky's,
/// through the same gate, clamp and decay ([`stream_px_per_s_with_share`]).
pub const EXHAUST_SHARE_CW: f32 = 4.0 * STREAM_SHARE_CW;

/// The slipstream's velocity, px/s backward, for cell width `cw` and observed key interval
/// `ioi_ms`.
#[inline]
#[must_use]
pub const fn stream_px_per_s(cw: f32, ioi_ms: f32) -> f32 {
    stream_px_per_s_with_share(STREAM_SHARE_CW, cw, ioi_ms)
}

#[inline]
#[must_use]
const fn stream_px_per_s_with_share(share: f32, cw: f32, ioi_ms: f32) -> f32 {
    if !(share.is_finite() && cw.is_finite() && ioi_ms.is_finite()) || cw <= 0.0 || share <= 0.0 {
        return 0.0;
    }
    let ioi = ioi_ms.clamp(STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS);
    let gate = smoothstep01(
        (STREAM_IOI_STROLL_MS - ioi) * (1.0 / (STREAM_IOI_STROLL_MS - STREAM_IOI_RUN_MS)),
    );
    share * cw * 1000.0 / ioi * gate
}

/// The whole travel, px, of a star born at `px_per_s`: the drift's integral,
/// `v₀ · τ · STREAM_SPENT`.
#[inline]
#[must_use]
pub const fn stream_travel_px(px_per_s: f32) -> f32 {
    px_per_s * (STREAM_TAU_MS * STREAM_SPENT / 1000.0)
}

#[inline]
#[must_use]
const fn stream_unit(age_ms: f32) -> f32 {
    if age_ms <= 0.0 {
        return 0.0;
    }
    if age_ms >= STREAM_END_MS {
        return 1.0;
    }
    ((1.0 - crate::color_math::exp_f32(-age_ms * (1.0 / STREAM_TAU_MS))) * (1.0 / STREAM_SPENT))
        .min(1.0)
}

/// The mend's pull window, ms: a key that mends a typo run ([`super::Mend`], §23) takes its strike
/// as an m2 (an m1 for an earned key) born at the erased cell's sky (grabbing the star the
/// Backspace was retiring, if live) and pulled one cell into the caret's on the throw's ease-out,
/// then pinned.
pub const MEND_PULL_MS: f32 = 110.0;

/// The drag time constant of a shed fragment, ms (§5.6).
pub const DRAG_TAU_MS: f32 = 90.0;

/// The erase throw's window, ms (§5.6: "radial throw 2-4 px over 80 ms ease-out, up and away from
/// the caret, then pinned + twinkling").
pub const ERASE_THROW_MS: f32 = 80.0;

/// The exhaust throw's window, ms (§33), under the transient lives so grains die on the wing: a
/// transient m3 (175.6 ms) reaches its reach with 16 ms left, while the 1-in-4 m2 (245.9 ms) lands
/// and twinkles for 86 ms.
pub const EXHAUST_THROW_MS: f32 = 160.0;

/// The exhaust's rise, cells of `ch`: how far a grain lifts over its throw, dealt 0.5..1.0 of it by
/// seed.
pub const EXHAUST_RISE_CH: f32 = 0.15;

/// The exhaust's m2 deal: one grain in 4 is an m2, only where the birth spine (never flow's heat)
/// is at or over [`FIELD_HOT_DISP`].
pub const EXHAUST_M2_IN: u32 = 4;

/// The exhaust's headroom: nothing is thrown while the live sky is within this many stars of
/// [`STAR_CAP`], leaving room for what a key and a nav gesture bring (a strike, a field star, a
/// five-star fan and its shed), so a spark never causes a hero's eviction.
pub const EXHAUST_HEADROOM: usize = 8;

/// The landing fan's window, ms: at the m2's 30 ms hold the throw has covered 47 % of the reach,
/// 60 % at the hero's 40 ms, so all five stars clear a 15×28 caret cell (under the host's opaque
/// caret) before their brightest hold is spent; they then twinkle pinned.
pub const FAN_THROW_MS: f32 = 110.0;

/// The mini-fan's window, ms, on the same reasoning as [`FAN_THROW_MS`]: the hop's pixels are
/// covered inside the m2's hold.
pub const MINI_FAN_THROW_MS: f32 = 100.0;

const _: () = assert!(
    {
        let u = HOLD_M2_TRANSIENT_MS / FAN_THROW_MS;
        1.0 - (1.0 - u) * (1.0 - u) >= 0.40
    },
    "the fan's four m2 must be clear of the caret cell before their hold is spent"
);

/// Lower end of the fan's per-star throw jitter, `reach·(0.55..1.05)` (§6.5 layer 11).
pub const FAN_THROW_JITTER_MIN: f32 = 0.55;

/// Upper end of the fan's per-star throw jitter (§6.5 layer 11).
pub const FAN_THROW_JITTER_MAX: f32 = 1.05;

/// The fan's vertical squash: distance buys the fan length along the line, not height, at this
/// aspect.
pub const FAN_SQUASH: f32 = 0.62;

/// The fan's rise ceiling, in `ch`, above and below the landing row's centre: the smallest fan's
/// reach (the Enter landing's D8 reach), so no fan rises higher than the smallest fan is thrown.
pub const FAN_RISE_MAX_CH: f32 = 1.6;

/// Lower end of the per-star jitter on the rise cap, `cap·(0.80..1.00)`, seeded, so re-aimed stars
/// scatter under the ceiling instead of lining up on it (§6.5 layer 11).
pub const FAN_RISE_JITTER_MIN: f32 = 0.80;

/// The focus-lost ember, ms (§8.2): every live star is put on a 300 ms `spend` finish beside the
/// ribbon's own ember.
pub const FOCUS_EMBER_MS: f32 = 300.0;

/// Minimum radial reach of an erase throw, px at the 1× cell (§5.6; scaled by [`px_scale`] where
/// thrown, like every reach and sputter below).
pub const ERASE_REACH_MIN_PX: f32 = 2.0;
/// Maximum radial reach of an erase throw, px at the 1× cell (§5.6).
pub const ERASE_REACH_MAX_PX: f32 = 4.0;
/// Minimum radial reach of a mini-fan throw, px at the 1× cell (§5.6: "1-2 px throw").
pub const MINI_FAN_REACH_MIN_PX: f32 = 1.0;
/// Maximum radial reach of a mini-fan throw, px at the 1× cell (§5.6).
pub const MINI_FAN_REACH_MAX_PX: f32 = 2.0;

/// Share of the meteor head's velocity a shed fragment inherits along the path (§5.6:
/// `v₀ = 0.12·v_head`).
pub const SHED_ALONG_SHARE: f32 = 0.12;
/// Minimum perpendicular sputter of a shed fragment, px/ms at the 1× cell (§5.6).
pub const SHED_PERP_MIN: f32 = 0.4;
/// Maximum perpendicular sputter of a shed fragment, px/ms at the 1× cell (§5.6).
pub const SHED_PERP_MAX: f32 = 1.1;

/// Erase: how many m3 grains one Backspace throws (§5.6's "3 m3 + 1 m2").
pub const ERASE_M3_N: u16 = 3;
/// Erase: how many m2 stars one Backspace throws (§5.6).
pub const ERASE_M2_N: u16 = 1;
/// A kill mints one star per this many erased cells (§5.6).
pub const KILL_CELLS_PER_STAR: u16 = 3;
/// Maximum stars minted by a single kill gesture.
pub const KILL_STAR_MAX: u16 = 8;
/// A mini-fan is exactly this many m2 stars (§5.6, D17).
pub const MINI_FAN_M2_N: u16 = 1;
/// How many m3 grains a mini-fan throws.
pub const MINI_FAN_M3_N: u16 = 2;

/// Quads stardust may spend on one frame (§18: "Stardust | 40 live | ≤ 284").
pub const STARDUST_QUAD_BUDGET: usize = 284;

/// Haloed stars stardust may draw on one frame (§18: "≤ 16", the cap's 4 m1 + 12 m2).
pub const STARDUST_HALO_BUDGET: usize = 16;

/// Source-over `RainHalo`s stardust may spend on one light frame. §3.3 draws every light star as
/// ink ([`push_twinkle_over_twin`] lays five shapes per star, nine with gold glints, each split per
/// crossed row); §18 holds a light frame to ≤ 400 of `MAX_HALOS 512`, with the meteor's train
/// chain at 96 ahead of the sky.
pub const STARDUST_LIGHT_HALO_BUDGET: usize = 268;

const _: () = assert!(
    STARDUST_LIGHT_HALO_BUDGET + 96 <= 400,
    "§18: the light frame's 400-halo law — train chain + sky"
);

/// The source-over ceiling of a light-theme star mark (§3.3: `light_ink_bold` is
/// `InkRole::Leading`, cap 236).
pub const LIGHT_ALPHA_CAP: f32 = 236.0;

/// A star's magnitude: the distribution that makes a field a field (§5.1 no. 2, §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StarClass {
    /// The hero: white body + tapers, four tinted stubs, a tinted halo, and (gold only, at twinkle
    /// peak only) four diagonal glints.
    M1,
    /// The medium star: a tint-washed body + tapers ([`M2_BODY_TINT`]), a 3×3 disc (sky lane only),
    /// four tinted stubs, a small halo.
    M2,
    /// The grain: a 1-px core with four arms at [`M3_CROSS_ADD`]·c outboard of it (its Airy disc),
    /// all in its cell's stop.
    M3,
}

impl StarClass {
    /// This class's coverage request `c` in `lane` (§5.2's two tables).
    #[inline]
    #[must_use]
    pub const fn cov(self, lane: StarLane) -> f32 {
        match (self, lane.is_sky()) {
            (Self::M1, true) => M1_COV_SKY,
            (Self::M1, false) => M1_COV_TRANSIENT,
            (Self::M2, true) => M2_COV_SKY,
            (Self::M2, false) => M2_COV_TRANSIENT,
            (Self::M3, true) => M3_COV_SKY,
            (Self::M3, false) => M3_COV_TRANSIENT,
        }
    }

    /// This class's rung on the family's one arm ladder (`effect_util::STAR_ARM_*`).
    #[inline]
    #[must_use]
    pub const fn arm_ratio(self) -> f32 {
        match self {
            Self::M1 => STAR_ARM_HERO,
            Self::M2 => STAR_ARM_STD,
            Self::M3 => STAR_ARM_FINE,
        }
    }

    /// The arm clock's floor, as a share of the nominal arm (§5.5): m2/m3 swing at full depth, m1
    /// only ±12 %.
    #[inline]
    #[must_use]
    pub const fn arm_floor(self) -> f32 {
        match self {
            Self::M1 => 0.88,
            Self::M2 | Self::M3 => 0.55,
        }
    }

    /// The smallest integer arm this class is ever drawn at, at cell height `ch` (§5.5 at `ch` 18:
    /// m1 3 ↔ 4, m2 2 ↔ 3, m3 1 ↔ 2 px).
    #[inline]
    #[must_use]
    pub const fn arm_min_px(self, ch: f32) -> i32 {
        match self {
            Self::M2 => px_at(ch, M2_ARM_MIN_PX),
            Self::M1 | Self::M3 => 1,
        }
    }

    #[inline]
    #[must_use]
    const fn arms(self, ch: f32) -> (f32, i32, f32, i32, i32) {
        let nominal = star_arm(ch, self.arm_ratio());
        let min_arm = self.arm_min_px(ch);
        let arm_floor = self.arm_floor();
        let raw_lo = (nominal * arm_floor).round() as i32;
        let arm_lo = if raw_lo > min_arm { raw_lo } else { min_arm };
        let raw_hi = nominal.round() as i32;
        let arm_hi = if raw_hi > min_arm { raw_hi } else { min_arm };
        (nominal, min_arm, arm_floor, arm_lo, arm_hi)
    }

    /// The core-brightness floor on the luminance clock (§5.5's table): m1 `0.85 + 0.15·e`, m2
    /// `0.70 + 0.30·e`, m3 `env` at full depth.
    #[inline]
    #[must_use]
    pub const fn core_floor(self) -> f32 {
        match self {
            Self::M1 => 0.85,
            Self::M2 => 0.70,
            Self::M3 => 0.0,
        }
    }

    /// This class's `(hold, τ)` in seconds for `lane` (§5.6).
    #[inline]
    #[must_use]
    pub const fn envelope_s(self, lane: StarLane) -> (f32, f32) {
        let (h, t) = match (self, lane.is_sky()) {
            (Self::M1, true) => (HOLD_M1_SKY_MS, TAU_M1_SKY_MS),
            (Self::M1, false) => (HOLD_M1_TRANSIENT_MS, TAU_M1_TRANSIENT_MS),
            (Self::M2, true) => (HOLD_M2_SKY_MS, TAU_M2_SKY_MS),
            (Self::M2, false) => (HOLD_M2_TRANSIENT_MS, TAU_M2_TRANSIENT_MS),
            (Self::M3, true) => (HOLD_M3_SKY_MS, TAU_M3_SKY_MS),
            (Self::M3, false) => (HOLD_M3_TRANSIENT_MS, TAU_M3_TRANSIENT_MS),
        };
        (h / 1000.0, t / 1000.0)
    }

    /// This class's whole life in seconds for `lane`: `H + 2.32·τ`, deriving the published
    /// 225 / 340 / 460 ms.
    #[inline]
    #[must_use]
    pub const fn life_s(self, lane: StarLane) -> f32 {
        if matches!(lane, StarLane::Field) {
            return FIELD_LIFE_S;
        }
        let (h, t) = self.envelope_s(lane);
        half_life_life_s(h, t)
    }
}

/// Which population minted a star (§5.6), and so how it is priced, how long it lives, how it moves,
/// and whether it may touch the glint bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StarLane {
    /// Born on every typed glyph, in the sky band of the new cell.
    Strike,
    /// Born once per laid ribbon cell, hash-dealt and stable per cell.
    Field,
    /// Born on Backspace (and per 3 cells of a kill, cap 8) around the erased cells.
    Erase,
    /// The exhaust (§33): one per typed key, born in the caret cell's sky band and thrown back
    /// along the row on its own wind ([`EXHAUST_SHARE_CW`], [`EXHAUST_THROW_MS`]).
    Exhaust,
    /// The meteor's shed fragments (§6.5 layer 5).
    Shed,
    /// The landing fan (§6.5 layer 11).
    Fan,
    /// A sub-floor nav hop's mini-fan (§6.12, D17).
    MiniFan,
}

impl StarLane {
    /// True for the sky lanes — priced by the sparkle law, composited centre ≤
    /// [`super::timing::FIELD_STAR_COV_CEIL`] (§5.2 ceiling 1).
    #[inline]
    #[must_use]
    pub const fn is_sky(self) -> bool {
        matches!(self, Self::Strike | Self::Field)
    }

    /// True for the transient lanes — they fly over ink, so they are priced by
    /// [`super::timing::TRANSIENT_STAR_COV_CEIL`] (§5.2 ceiling 2).
    #[inline]
    #[must_use]
    pub const fn is_transient(self) -> bool {
        !self.is_sky()
    }

    /// True where a star may cue a glint at all (§13): erase-born stardust and the exhaust never
    /// chime, and meteor-lane stars ride the rain instead.
    #[inline]
    #[must_use]
    pub const fn may_chime(self) -> bool {
        matches!(self, Self::Strike | Self::Field)
    }

    /// The throw window `T` of a radial-throw lane, ms; `None` for lanes that lift or drag.
    #[inline]
    #[must_use]
    pub const fn throw_ms(self) -> Option<f32> {
        match self {
            Self::Erase => Some(ERASE_THROW_MS),
            Self::Exhaust => Some(EXHAUST_THROW_MS),
            Self::Fan => Some(FAN_THROW_MS),
            Self::MiniFan => Some(MINI_FAN_THROW_MS),
            Self::Strike | Self::Field | Self::Shed => None,
        }
    }

    #[inline]
    #[must_use]
    const fn motion(self) -> Motion {
        match self {
            Self::Strike | Self::Field => Motion::Lift,
            Self::Shed => Motion::Drag,
            Self::Erase => Motion::Throw(ERASE_THROW_MS),
            Self::Exhaust => Motion::Throw(EXHAUST_THROW_MS),
            Self::Fan => Motion::Throw(FAN_THROW_MS),
            Self::MiniFan => Motion::Throw(MINI_FAN_THROW_MS),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Motion {
    Lift,
    Drag,
    Throw(f32),
}

impl Motion {
    #[inline]
    const fn k_at(self, age_ms: f32) -> f32 {
        match self {
            Self::Lift => age_ms,
            Self::Drag => {
                DRAG_TAU_MS * (1.0 - crate::color_math::exp_f32(-age_ms * (1.0 / DRAG_TAU_MS)))
            }
            Self::Throw(window_ms) => window_ms * (1.0 - spend(age_ms / window_ms)),
        }
    }

    #[inline]
    const fn end_ms(self, life_ms: f32) -> f32 {
        match self {
            Self::Throw(window_ms) => window_ms,
            Self::Lift | Self::Drag => life_ms,
        }
    }
}

/// A star's finish: how a star leaves before its life is up (§5.6's cap, §8.2's focus ember, field
/// stars dying with their cells).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Finish {
    /// The edge the finish started on.
    pub at: Instant,
    /// The finish's span in seconds — 0.040 for an eviction, 0.300 for the focus ember, the cell's
    /// own retract for a field star.
    pub span_s: f32,
}

const STAR_CULL_SPAN_SHARE: f32 = 1.0 - crate::color_math::sqrt_f32(STAR_CULL_ALPHA);

impl Finish {
    /// The finish's unit progress at `now`, `0..=1`.
    #[inline]
    #[must_use]
    pub fn u(&self, now: Instant) -> f32 {
        clamp01(
            now.saturating_duration_since(self.at).as_secs_f32() / self.span_s.max(f32::EPSILON),
        )
    }

    /// The envelope multiplier at `now` — `spend(u)`.
    #[inline]
    #[must_use]
    pub fn gain(&self, now: Instant) -> f32 {
        spend(self.u(now))
    }

    #[inline]
    #[must_use]
    fn ends(&self) -> Instant {
        self.at + std::time::Duration::from_secs_f32(self.span_s.max(0.0))
    }

    #[inline]
    fn keep_sooner(slot: &mut Option<Self>, at: Instant, span_s: f32) {
        let next = Self { at, span_s };
        if slot.is_none_or(|f| next.ends() < f.ends()) {
            *slot = Some(next);
        }
    }

    #[inline]
    #[must_use]
    fn cull_at(&self) -> Instant {
        self.at + std::time::Duration::from_secs_f32(self.span_s.max(0.0) * STAR_CULL_SPAN_SHARE)
    }
}

/// One star (§17.1).
#[derive(Clone, Copy, Debug)]
pub struct Star {
    /// Magnitude (§5.2).
    pub class: StarClass,
    /// Population (§5.6) — pricing, envelope, motion, budget behaviour.
    pub lane: StarLane,
    /// Window-absolute X of the centre, pinned to a device pixel at birth (§5.4): drift is
    /// expressed as integer steps of the centre.
    pub x: f32,
    /// Window-absolute Y of the centre, pinned the same way.
    pub y: f32,
    /// The glyph row that earned a sky star.
    pub home_row: Option<u16>,
    /// The birth edge.
    pub born: Instant,
    /// The last key edge that lit this star: the field lane's fade clock ([`FIELD_HOLD_MS`] /
    /// [`FIELD_TAU_MS`] run from here).
    pub lit: Instant,
    /// Deterministic seed, hashed from `(row, col, mint)` at the spawn edge (§18).
    pub seed: u32,
    /// The star's chroma, constant for its life: one of the seven snapped stops, [`TINT_WHITE_RGB`]
    /// or [`TINT_GOLD_RGB`] (§5.3, C1).
    pub tint: u32,
    /// True for a gold star (§5.3's 15 % deal, or the fan's hero, D6): the only stars with diagonal
    /// glints (§5.2).
    pub gold: bool,
    /// Birth velocity in px/ms, `(vx, vy)`: the throw for erase / fan / mini-fan, the inherited
    /// `0.12·v_head` for a shed fragment, the −3 px/s lift for a sky m1/m2.
    pub v0: (f32, f32),
    /// The seeded arm-size scintillation rate, Hz, in `[SCINT_F_MIN, SCINT_F_MAX]` (§5.5, D12).
    pub f: f32,
    /// The slipstream's travel, px ≥ 0: how far this star drifts backward over [`STREAM_END_MS`],
    /// priced once at birth from the observed key interval (T4) and clamped to its row's blank
    /// frontier ([`Stardust::stream_reach`], L4).
    pub stream: f32,
    /// The mend's pull, px ≥ 0: how far left of its home this star was born, drawn in over
    /// [`MEND_PULL_MS`] ([`Star::pos`]).
    pub pull: f32,
    /// The finish this star is on, if any ([`Finish`]).
    pub finish: Option<Finish>,
}

const _: () = assert!(
    BIRTH_EDGE_FLOOR > STAR_CULL_ALPHA,
    "the field star's birth floor sits under the star cull"
);

impl Star {
    /// Seconds since birth, saturating at zero.
    #[inline]
    #[must_use]
    pub fn age_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.born).as_secs_f32()
    }

    /// Seconds since the last key that lit this star ([`Star::lit`]), saturating at zero — the
    /// field fade's clock.
    #[inline]
    #[must_use]
    pub fn idle_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.lit).as_secs_f32()
    }

    /// This star's whole life in seconds (§5.6).
    #[inline]
    #[must_use]
    pub const fn life_s(&self) -> f32 {
        self.class.life_s(self.lane)
    }

    /// Born at full: true inside the class hold `H` (§5.6), when a star draws at its nominal arm
    /// and full core; both scintillation clocks take over at `age ≥ H`, still reading `now − born`.
    #[inline]
    #[must_use]
    pub fn in_hold(&self, now: Instant) -> bool {
        self.age_s(now) < self.class.envelope_s(self.lane).0
    }

    /// True once the star must leave the pool: past its life, past the point where its [`Finish`]
    /// alone takes it under [`STAR_CULL_ALPHA`], or (a field star) past the field fade from its
    /// row's last key ([`field_fade_life_s`]).
    #[inline]
    #[must_use]
    pub fn dead(&self, now: Instant) -> bool {
        self.age_s(now) >= self.life_s()
            || (self.lane == StarLane::Field && self.idle_s(now) >= FIELD_FADE_LIFE_S)
            || self.finish.is_some_and(|f| f.gain(now) < STAR_CULL_ALPHA)
    }

    /// The population envelope at `now`: `1` for the hold, then half-life τ (§5.6), times the
    /// [`Finish`] if any.
    #[inline]
    #[must_use]
    pub fn envelope(&self, now: Instant, reduced_motion: bool) -> f32 {
        self.envelope_at_age(self.age_s(now), now, reduced_motion)
    }

    #[inline]
    #[must_use]
    fn envelope_at_age(&self, age: f32, now: Instant, reduced_motion: bool) -> f32 {
        let fin = self.finish.map_or(1.0, |f| f.gain(now));
        if self.lane == StarLane::Field {
            return field_lane_envelope(age, self.idle_s(now), fin, reduced_motion);
        }
        if reduced_motion {
            let life = self.life_s();
            const INV_FADE_S: f32 = 1.0 / (REDUCED_MOTION_FADE_MS / 1000.0).max(f32::EPSILON);
            return clamp01((life - age) * INV_FADE_S) * fin;
        }
        let (hold, tau) = self.class.envelope_s(self.lane);
        half_life(age, hold, tau) * fin
    }

    /// Put this star on a finish ending at `at + span_s`, unless it already has one ending sooner.
    #[inline]
    pub fn finish_by(&mut self, at: Instant, span_s: f32) {
        Finish::keep_sooner(&mut self.finish, at, span_s);
    }

    /// This star's grid column: its home pixel read back as a column (stable for its life, §5.4).
    #[inline]
    #[must_use]
    pub const fn grid_col(&self, geom: Geom) -> i32 {
        px_col(self.x, geom)
    }

    /// Whether this star is in the sky of `row`: its pixel row is `row` or `row − 1`
    /// ([`in_sky_of_row`]).
    #[inline]
    #[must_use]
    pub const fn in_sky_of(&self, row: u16, geom: Geom) -> bool {
        in_sky_of_row(self.y, row, geom)
    }

    /// True for a warm tint: R, O, Y of the seven stops, plus gold and the achromatic white (§5.5).
    #[inline]
    #[must_use]
    pub const fn is_warm(&self) -> bool {
        if self.tint == TINT_WHITE_RGB || self.tint == TINT_GOLD_RGB {
            return true;
        }
        let mut i = WARM_STOPS;
        while i < SPECTRUM_ANCHORS.len() {
            if SPECTRUM_ANCHORS[i] == self.tint {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The luminance phase, radians (§5.5): `φ = hash(row, col, seed)·2π`; warm tints take `φ`,
    /// cool ones `φ + π`, so warm and cool neighbours never blink together.
    #[inline]
    #[must_use]
    pub const fn lum_phase(&self) -> f32 {
        let base = hash01(mix32(self.seed ^ SALT_PHASE)) * std::f32::consts::TAU;
        if self.is_warm() {
            base
        } else {
            base + std::f32::consts::PI
        }
    }

    /// The certified 2.86 Hz luminance envelope at `now` (§5.5; `effect_util::twinkle_env`,
    /// `TWINKLE_OMEGA 9` rad/s, under WCAG 2.3.1's general-flash bound).
    #[inline]
    #[must_use]
    pub fn twinkle(&self, now: Instant) -> f32 {
        twinkle_env(self.age_s(now), self.lum_phase())
    }

    /// True where this star's body and nucleus may carry `push_twinkle_star`'s diagonal glints:
    /// gold only, at twinkle peak only (§5.2, §5.7).
    #[inline]
    #[must_use]
    pub fn glinting(&self, now: Instant) -> bool {
        self.gold && twinkle_peak(self.twinkle(now))
    }

    #[inline]
    #[must_use]
    const fn inks(&self, cfg: &Config) -> bool {
        !matches!(self.lane, StarLane::Exhaust) || light_keeps(self.seed, cfg)
    }

    /// Whether this star moves or ramps every frame: a throw inside its window, a mend pull inside
    /// [`MEND_PULL_MS`], a shed fragment under drag (for its whole life), or a field star inside
    /// its 18 ms `edge-in`.
    #[inline]
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        let age_s = self.age_s(now);
        if self.lane == StarLane::Field && age_s < EDGE_IN_S {
            return true;
        }
        if self.pull > 0.0 && age_s * 1000.0 < MEND_PULL_MS {
            return true;
        }
        if self.v0.0 == 0.0 && self.v0.1 == 0.0 {
            return false;
        }
        match self.lane.motion() {
            Motion::Throw(window_ms) => age_s * 1000.0 < window_ms,
            Motion::Drag => true,
            Motion::Lift => false,
        }
    }

    /// Seconds until this star's integer arm next steps on §5.5's size clock,
    /// `arm = round(A·(floor + (1 − floor)·sin²(2π·f·age + φ)))` at cell height `ch`, solved in
    /// closed form: the arm toggles where `sin²` crosses `((k + 0.5)/A − floor)/(1 − floor)`, twice
    /// per `1/(2f)` period, so it is never more than 62.5 ms away.
    #[inline]
    #[must_use]
    pub fn next_arm_step_s(&self, ch: f32, now: Instant) -> Option<f32> {
        let nominal = star_arm(ch, self.class.arm_ratio());
        let floor = self.class.arm_floor();
        let depth = 1.0 - floor;
        if !(nominal.is_finite() && depth.is_finite() && self.f.is_finite())
            || nominal <= 0.0
            || depth <= 0.0
            || self.f <= 0.0
        {
            return None;
        }
        let min = self.class.arm_min_px(ch) as f32;
        let omega = std::f32::consts::TAU * self.f;
        let theta = omega * self.age_s(now) + self.lum_phase();
        let k0 = (nominal * floor - 0.5).ceil().max(min) as i32;
        let k1 = (nominal - 0.5).floor() as i32;
        let inv_nominal = 1.0 / nominal;
        let inv_depth = 1.0 / depth;
        let mut best: Option<f32> = None;
        for k in k0..=k1 {
            let c = ((k as f32 + 0.5) * inv_nominal - floor) * inv_depth;
            if c > 0.0
                && c < 1.0
                && let Some(dt) = next_sine_crossing_s(theta, c.sqrt().asin(), omega)
            {
                best = Some(best.map_or(dt, |b| b.min(dt)));
            }
        }
        best
    }

    /// Seconds until a gold star's diagonal glints next switch (`|sin(ω·age + φ)| ≥ 0.92`, §5.2,
    /// §5.5), exact on the luminance clock.
    #[inline]
    #[must_use]
    pub fn next_glint_toggle_s(&self, now: Instant) -> Option<f32> {
        if !self.gold {
            return None;
        }
        const TWINKLE_GLINT_ASIN: f32 = {
            let mut y = 1.168_f32;
            let mut i = 0;
            while i < 6 {
                let (sy, cy) = crate::supernova::sin_cos_f32(y);
                y -= (sy - TWINKLE_GLINT_FRAC) / cy;
                i += 1;
            }
            y
        };
        let theta = TWINKLE_OMEGA * self.age_s(now) + self.lum_phase();
        next_sine_crossing_s(theta, TWINKLE_GLINT_ASIN, TWINKLE_OMEGA)
    }

    /// Seconds until a lifting star's integer position next steps (§5.6):
    /// `pos = origin + round(v₀·age)` steps where `|v₀|·age` next reaches a half.
    #[inline]
    #[must_use]
    pub fn next_lift_step_s(&self, now: Instant) -> Option<f32> {
        if !matches!(self.lane.motion(), Motion::Lift) {
            return None;
        }
        let age_ms = self.age_s(now) * 1000.0;
        let mut best: Option<f32> = None;
        for v in [self.v0.0.abs(), self.v0.1.abs()] {
            if !v.is_finite() || v <= 0.0 {
                continue;
            }
            let next_ms = ((v * age_ms).round() + 0.5) / v;
            let dt = (next_ms - age_ms) * (1.0 / 1000.0);
            if dt > 0.0 {
                best = Some(best.map_or(dt, |b| b.min(dt)));
            }
        }
        best
    }

    #[inline]
    const fn stream_px(&self, age_ms: f32) -> i32 {
        if self.stream <= 0.0 {
            return 0;
        }
        (self.stream * stream_unit(age_ms)).round() as i32
    }

    /// Seconds until a streaming star's integer position next steps: where `stream · u(age)` next
    /// reaches a half, solved as `age = −τ · ln(1 − u · STREAM_SPENT)` ([`stream_unit`]).
    #[inline]
    #[must_use]
    pub fn next_stream_step_s(&self, now: Instant) -> Option<f32> {
        if self.stream <= 0.0 || !self.stream.is_finite() {
            return None;
        }
        let age_ms = self.age_s(now) * 1000.0;
        if age_ms >= STREAM_END_MS {
            return None;
        }
        let n = (self.stream * stream_unit(age_ms)).round();
        let u = (n + 0.5) / self.stream;
        if u >= 1.0 {
            return None;
        }
        let t_ms = -STREAM_TAU_MS * (1.0 - u * STREAM_SPENT).ln();
        Some(((t_ms.min(STREAM_END_MS) - age_ms) * (1.0 / 1000.0)).max(0.0))
    }

    #[inline]
    const fn pull_px(&self, age_ms: f32) -> i32 {
        if self.pull <= 0.0 || age_ms >= MEND_PULL_MS {
            return 0;
        }
        (self.pull * spend(age_ms * (1.0 / MEND_PULL_MS))).round() as i32
    }

    /// The centre at `now`, pinned to a device pixel (§5.4), by the population's [`Motion`] law.
    #[inline]
    #[must_use]
    pub fn pos(&self, now: Instant, reduced_motion: bool) -> (i32, i32) {
        if reduced_motion {
            return self.origin();
        }
        self.pos_at_age(self.age_s(now))
    }

    #[inline]
    #[must_use]
    const fn pos_at_age(&self, age_s: f32) -> (i32, i32) {
        let age_ms = age_s * 1000.0;
        let (x, y) = if self.v0.0 == 0.0 && self.v0.1 == 0.0 {
            self.origin()
        } else {
            self.at_k(self.lane.motion().k_at(age_ms))
        };
        (x - self.stream_px(age_ms) - self.pull_px(age_ms), y)
    }

    /// Where this star comes to rest: its position at the end of its motion (a throw's reach, a
    /// drift's or lift's death). §5.4's transient clearance is asked of this pixel
    /// ([`Stardust::settle`]): a core never lands on a probed glyph.
    #[inline]
    #[must_use]
    pub const fn rest(&self) -> (i32, i32) {
        let motion = self.lane.motion();
        let end_ms = motion.end_ms(self.life_s() * 1000.0);
        let (x, y) = if self.v0.0 == 0.0 && self.v0.1 == 0.0 {
            self.origin()
        } else {
            self.at_k(motion.k_at(end_ms))
        };
        (x - self.stream_px(end_ms), y)
    }

    #[inline]
    const fn origin(&self) -> (i32, i32) {
        (self.x.round() as i32, self.y.round() as i32)
    }

    #[inline]
    const fn at_k(&self, k: f32) -> (i32, i32) {
        let (bx, by) = self.origin();
        if self.v0.0 == 0.0 && self.v0.1 == 0.0 {
            return (bx, by);
        }
        (
            bx + (self.v0.0 * k).round() as i32,
            by + (self.v0.1 * k).round() as i32,
        )
    }

    #[inline]
    const fn aim(&mut self, rest: (i32, i32), k: f32) {
        let (bx, by) = self.origin();
        if k <= 0.0 {
            self.v0 = (0.0, 0.0);
            return;
        }
        let inv_k = 1.0 / k;
        self.v0 = ((rest.0 - bx) as f32 * inv_k, (rest.1 - by) as f32 * inv_k);
    }
}

/// The shared glint token bucket (§8.1 no. 2, §13, D5), refilled on the frame clock in
/// `Engine::tick`: `tokens = min(GLINT_CAP, tokens + GLINT_REFILL_PER_S · dt)`.
#[derive(Clone, Copy, Debug)]
pub struct StarBudget {
    tokens: f32,
    at: Option<Instant>,
}

impl Default for StarBudget {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl StarBudget {
    /// A full bucket, whose first [`Self::refill`] adds nothing.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tokens: GLINT_CAP,
            at: None,
        }
    }

    /// Refill on the frame clock — called once per `Engine::tick`, before any birth is dealt.
    #[inline]
    pub fn refill(&mut self, now: Instant) {
        let dt = self
            .at
            .replace(now)
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        self.tokens = (self.tokens + GLINT_REFILL_PER_S * dt).min(GLINT_CAP);
    }

    /// Try to spend one token for an m1's glint.
    #[inline]
    pub const fn try_spend(&mut self) -> bool {
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }

    /// A meteor empties the bucket on its move edge (§8.1 no. 2, §13), so the ~250 ms of typing
    /// under the rain is silent.
    #[inline]
    pub const fn empty(&mut self) {
        self.tokens = 0.0;
    }

    /// Tokens on hand — `trail status` and the budget tests.
    #[inline]
    #[must_use]
    pub const fn tokens(&self) -> f32 {
        self.tokens
    }
}

/// One glint the sky has earned and paid for (§13): one sine at a lattice degree, octave-folded
/// into `[2800, 5600)` Hz, amplitude-twinkled at the star's own scintillation rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glint {
    /// The hero's column, which the host maps to stereo pan (§13: "pan = the star's column
    /// ± 0.03").
    pub col: u16,
    /// The star's seeded scintillation rate, Hz — the glint's amplitude twinkle rides it at depth
    /// 0.5, so the sound opens and closes with the light it belongs to.
    pub f: f32,
}

/// What this frame knows about the glyphs: the snapshot §5.4's birth gate is asked of, so a star is
/// never drawn over a stroke (L4).
#[derive(Clone, Debug)]
pub struct GlyphProbe {
    rows: [ProbedRow; Self::ROWS],
    live: usize,
    at: usize,
}

impl Default for GlyphProbe {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// What one probed cell holds for §5.4's gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CellInk {
    /// A blank (`' '`): provably clear, `Some(false)` everywhere.
    #[default]
    Blank,
    /// Any other glyph, a wide continuation (`'\0'`) included: `Some(true)` at every pixel of the
    /// cell (L4's law).
    Glyph,
    /// A light horizontal box-drawing rule (`─` U+2500 and its dashed and half-length siblings):
    /// `Some(true)` only inside [`RULE_INK_TOP_CH`]`..`[`RULE_INK_BOT_CH`], `Some(false)` above and
    /// below.
    Rule,
}

/// Classify one probed char (the host's `Terminal::row_cols_into` convention: `'\0'` at a wide
/// continuation, `' '` for a blank).
#[inline]
#[must_use]
pub const fn cell_ink(ch: char) -> CellInk {
    match ch {
        ' ' => CellInk::Blank,
        '\u{2500}' | '\u{2504}' | '\u{2508}' | '\u{254C}' | '\u{2574}' | '\u{2576}' => {
            CellInk::Rule
        }
        _ => CellInk::Glyph,
    }
}

/// Where a light horizontal rule's stroke begins, as a fraction of the cell height from its top
/// (§5.4).
pub const RULE_INK_TOP_CH: f32 = 0.35;

/// Where a light horizontal rule's stroke ends, as a fraction of the cell height from its top.
pub const RULE_INK_BOT_CH: f32 = 1.0 - (super::ribbon::TALL_UP_CH - 1.0) - SKY_TOP_CH;

const _: () = assert!(
    RULE_INK_BOT_CH <= 1.0 - (super::ribbon::TALL_UP_CH - 1.0) - SKY_TOP_CH,
    "the rule's stroke band must end at or above the tall sky zone's top edge"
);
const _: () = assert!(
    SKY_TOP_CH <= RULE_INK_TOP_CH,
    "the underline sky zone must end above the rule's stroke band"
);
const _: () = assert!(
    RULE_INK_TOP_CH < 0.5 && 0.5 < RULE_INK_BOT_CH,
    "the rule's stroke band must contain the cell's vertical centre"
);
const _: () = assert!(
    SKY_BOTTOM_CH + EXHAUST_RISE_CH < 1.0 - RULE_INK_BOT_CH,
    "a rise out of the underline zone must stop short of the row above's stroke band"
);

/// The stroke band in whole pixels: the device-pixel offsets from a cell's top that a light rule's
/// band covers at cell height `ch`, half-open; the one rounding the pixel probe and the rise's
/// headroom share.
#[inline]
#[must_use]
pub const fn rule_stroke_px(ch: usize) -> std::ops::Range<i32> {
    let ch = ch as f32;
    (RULE_INK_TOP_CH * ch).ceil() as i32..(RULE_INK_BOT_CH * ch).round() as i32
}

#[derive(Clone, Debug)]
struct ProbedRow {
    row: i32,
    ink: Vec<u64>,
    rule: Vec<u64>,
    width: u16,
}

impl Default for ProbedRow {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl ProbedRow {
    const EMPTY: Self = Self {
        row: 0,
        ink: Vec::new(),
        rule: Vec::new(),
        width: 0,
    };
}

impl GlyphProbe {
    /// How many rows a probe holds — the caret's and its two neighbours, which is exactly what
    /// `CursorGlow` captures.
    pub const ROWS: usize = 3;

    /// Nothing probed: every in-grid cell answers `None`, so no sky star is born until the host has
    /// looked (§5.4).
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rows: [ProbedRow::EMPTY; Self::ROWS],
            live: 0,
            at: 0,
        }
    }

    /// Record that grid `row` was probed, with `occupied[col]` true where a glyph stands.
    #[inline]
    pub fn probe_row(&mut self, row: i32, occupied: &[bool]) {
        let slot = self.slot_for(row, occupied.len());
        let r = &mut self.rows[slot];
        for (col, &on) in occupied.iter().enumerate() {
            if on {
                r.ink[col / 64] |= 1u64 << (col % 64);
            }
        }
    }

    /// [`Self::probe_row`] with the per-cell class ([`cell_ink`]): a [`CellInk::Rule`] is recorded
    /// as ink (for [`Self::at`]) and as a rule (for [`Self::at_px`]'s stroke band).
    #[inline]
    pub fn probe_row_ink(&mut self, row: i32, cells: &[CellInk]) {
        let slot = self.slot_for(row, cells.len());
        let r = &mut self.rows[slot];
        for (col, &cell) in cells.iter().enumerate() {
            let bit = 1u64 << (col % 64);
            match cell {
                CellInk::Blank => {}
                CellInk::Glyph => r.ink[col / 64] |= bit,
                CellInk::Rule => {
                    r.ink[col / 64] |= bit;
                    r.rule[col / 64] |= bit;
                }
            }
        }
    }

    /// [`Self::probe_row_ink`] classifying each `char` with [`cell_ink`] in place.
    #[inline]
    pub fn probe_row_chars(&mut self, row: i32, chars: &[char]) {
        let slot = self.slot_for(row, chars.len());
        let r = &mut self.rows[slot];
        for (col, &ch) in chars.iter().enumerate() {
            let bit = 1u64 << (col % 64);
            match cell_ink(ch) {
                CellInk::Blank => {}
                CellInk::Glyph => r.ink[col / 64] |= bit,
                CellInk::Rule => {
                    r.ink[col / 64] |= bit;
                    r.rule[col / 64] |= bit;
                }
            }
        }
    }

    #[inline]
    fn slot_for(&mut self, row: i32, width: usize) -> usize {
        let words = width.div_ceil(64);
        let slot = if let Some(i) = self.rows[..self.live].iter().position(|r| r.row == row) {
            i
        } else if self.live < Self::ROWS {
            let i = self.live;
            self.live += 1;
            i
        } else {
            let i = self.at % Self::ROWS;
            self.at = (self.at + 1) % Self::ROWS;
            i
        };
        let r = &mut self.rows[slot];
        r.row = row;
        r.width = width.min(usize::from(u16::MAX)) as u16;
        r.ink.clear();
        r.ink.resize(words, 0);
        r.rule.clear();
        r.rule.resize(words, 0);
        slot
    }

    /// Forget everything: a reset, a layout change, a style switch, and every scroll or band move
    /// (the host re-probes before the next deal).
    #[inline]
    pub const fn clear(&mut self) {
        self.live = 0;
        self.at = 0;
    }

    /// The gate (§5.4): `Some(true)` a glyph is there, `Some(false)` provably blank, `None` outside
    /// this probe's knowledge.
    #[inline]
    #[must_use]
    pub const fn at(&self, row: i32, col: u16, rows: usize) -> Option<bool> {
        match self.cell_at(row, col, rows) {
            Some((inked, _)) => Some(inked),
            None => None,
        }
    }

    /// [`Self::at`] asked of a window pixel; a pixel left or right of the grid is `Some(false)`.
    #[inline]
    #[must_use]
    pub const fn at_px(&self, x: i32, y: i32, geom: Geom) -> Option<bool> {
        let col = px_col(x as f32, geom);
        if col < 0 || col as usize >= geom.cols {
            return Some(false);
        }
        let row = px_row(y as f32, geom);
        let Some((inked, is_rule)) = self.cell_at(row, col as u16, geom.rows) else {
            return None;
        };
        if inked && is_rule {
            let off = y - (geom.origin_y as i32) - row * geom.ch as i32;
            let band = rule_stroke_px(geom.ch);
            return Some(off >= band.start && off < band.end);
        }
        Some(inked)
    }

    /// [`Self::at`] for §5.4's sky question: does this `row − 1` cell carry ink the zone
    /// `[top − 0.30 ch, top − 0.04 ch]` could touch? A light rule answers `Some(false)` here (its
    /// band ends where the zone begins) while [`Self::at`] still says `Some(true)`.
    #[inline]
    #[must_use]
    pub const fn sky_at(&self, row: i32, col: u16, rows: usize) -> Option<bool> {
        let Some((inked, is_rule)) = self.cell_at(row, col, rows) else {
            return None;
        };
        Some(inked && !is_rule)
    }

    #[inline]
    const fn cell_at(&self, row: i32, col: u16, rows: usize) -> Option<(bool, bool)> {
        if row < 0 || row as usize >= rows {
            return Some((false, false));
        }
        let mut idx = 0;
        let mut found: Option<&ProbedRow> = None;
        while idx < self.live {
            if self.rows[idx].row == row {
                found = Some(&self.rows[idx]);
                break;
            }
            idx += 1;
        }
        let Some(r) = found else {
            return None;
        };
        if col >= r.width {
            return Some((false, false));
        }
        let i = col as usize;
        let word = i / 64;
        let shift = i % 64;
        let ink = r.ink.as_slice();
        let inked = word < ink.len() && (ink[word] >> shift) & 1 == 1;
        let rule = r.rule.as_slice();
        let is_rule = inked && word < rule.len() && (rule[word] >> shift) & 1 == 1;
        Some((inked, is_rule))
    }

    #[inline]
    const fn has_rule_in(&self, row: i32, cols: std::ops::RangeInclusive<i32>) -> bool {
        let mut idx = 0;
        let mut found: Option<&ProbedRow> = None;
        while idx < self.live {
            if self.rows[idx].row == row {
                found = Some(&self.rows[idx]);
                break;
            }
            idx += 1;
        }
        let Some(r) = found else {
            return false;
        };
        let start = *cols.start();
        let lo = if start > 0 { start } else { 0 };
        let end = *cols.end();
        let max_col = r.width as i32 - 1;
        let hi = if end < max_col { end } else { max_col };
        if hi < lo {
            return false;
        }
        let (lo, hi) = (lo as usize, hi as usize);
        let (w0, w1) = (lo / 64, hi / 64);
        let low_mask = u64::MAX << (lo % 64);
        let high_mask = u64::MAX >> (63 - (hi % 64));
        let rule = r.rule.as_slice();
        if w0 == w1 {
            return w0 < rule.len() && (rule[w0] & low_mask & high_mask) != 0;
        }
        if w0 < rule.len() && (rule[w0] & low_mask) != 0 {
            return true;
        }
        let mut w = w0 + 1;
        while w < w1 && w < rule.len() {
            if rule[w] != 0 {
                return true;
            }
            w += 1;
        }
        w1 < rule.len() && (rule[w1] & high_mask) != 0
    }

    #[cfg(test)]
    const fn rule_at(&self, row: i32, col: u16) -> bool {
        let c = col as i32;
        self.has_rule_in(row, c..=c)
    }

    /// True when the probe has no rows at all.
    #[must_use]
    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.live == 0
    }
}

/// What the meteor hands stardust for one shed station (§6.5 layer 5, §5.6).
#[derive(Clone, Copy, Debug)]
pub struct ShedSow {
    /// Window-absolute px of the station the head just passed — the fragment is born exactly here.
    pub at_px: (f32, f32),
    /// The head's own velocity in px/ms, unscaled.
    pub v_head: (f32, f32),
    /// The meteor's arc position at this station: the fragment wears `spectrum_snap(t_m(s_i))`, the
    /// train's colour where it was shed (§6.5 layer 5, C1).
    pub tint_t: f32,
    /// This station's seed, hashed from the flight's seed and the station index, so a flight sheds
    /// the same debris every time (§18).
    pub seed: u32,
    /// How many grains at this station; the meteor's law is one.
    pub n: u16,
}

/// What the meteor hands stardust for its landing fan (§6.5 layer 11, §5.6).
#[derive(Clone, Copy, Debug)]
pub struct FanSow {
    /// Window-absolute px of the landing cell's centre.
    pub at_px: (f32, f32),
    /// Throw reach in px — the length of the outermost star's throw on [`Motion::Throw`]'s ease-out
    /// over [`FAN_THROW_MS`], along the line; its rise is the sky's ([`fan_rise`]).
    pub reach: f32,
    /// The landing's walk ([`LandingWalk`]): `walk.at(0.0)` is the pin's stop, and a star thrown
    /// `dx` px along the row snaps to `walk.at(dx / cw)` (C1).
    pub walk: LandingWalk,
    /// The landing's seed — the whole of the party's variance (§6.5).
    pub seed: u32,
    /// How many stars, at most `timing::FAN_MAX_N` (`meteor::fan_count` for a big landing).
    pub n: u8,
    /// Whether this landing carries its gold m1 hero.
    pub hero: bool,
}

#[derive(Clone, Copy, Debug)]
struct Deal {
    class: StarClass,
    lane: StarLane,
    cell: (u16, u16),
    earned: bool,
    pull: Option<Pull>,
    tint: Option<(u32, bool)>,
}

#[derive(Clone, Copy, Debug)]
struct Pull {
    from: (u16, u16),
    disp: f32,
}

#[derive(Clone, Copy, Debug)]
struct Throw {
    class: StarClass,
    lane: StarLane,
    at_px: (f32, f32),
    v0: (f32, f32),
    tint: u32,
    gold: bool,
    seed: u32,
    fixed: bool,
}

/// The stardust producer: the star pool, the glint bucket, the glyph probe and the aurora.
#[derive(Clone, Debug)]
pub struct Stardust {
    stars: Vec<Star>,
    /// The one glint bucket (§13).
    pub budget: StarBudget,
    probe: GlyphProbe,
    glints: Vec<Glint>,
    minted: u32,
    reduced: bool,
    cell_h: f32,
    veil: Vec<Veil>,
    last_typed: Option<Instant>,
    ioi_ms: f32,
    sky_index: SkyIndex,
    /// The pet's body, which no star is drawn on ([`PetShade`]).
    pub shade: PetShade,
    /// The word icons and the other rows' lettered and filled cells, which no
    /// star is drawn on ([`KeepOut`]).
    pub keep_out: KeepOut,
    /// Each star the last frame painted ([`SkyMark`]).
    marks: Vec<SkyMark>,
    /// The marks' lights' keys ([`SkyMark::lights`]).
    mark_lights: Vec<u64>,
    paper_ink: PaperInkMemo,
}

/// One star a frame painted: its centre, its reach (an icon within half of it lifts the star, as
/// [`KeepOut::covers`] tests), the window-pixel box `(x0, x1, y0, y1)` its quads and halos fill,
/// and the keys of those lights ([`Stardust::mark_lights`]). The word pass draws its icons after
/// the sky, so the host lifts a star off a new icon by its lights' keys, not by its box: other
/// lights' pieces can lie inside the box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkyMark {
    /// The star's centre, window pixels.
    pub at: (i32, i32),
    /// Its arm's peak reach plus one, px: steady over the star's life, so a
    /// lift keyed on it never blinks with the arm's scintillation.
    pub reach: i32,
    /// The box its marks fill, window pixels, right and bottom exclusive.
    pub rect: (i32, i32, i32, i32),
    /// Its own lights' keys, a range of [`Stardust::mark_lights`].
    pub lights: (u32, u32),
}

impl SkyMark {
    /// The mark a star painted into `quads` and `halos` (each slice the
    /// star's own), its lights' keys appended to `keys`, or `None` when it
    /// drew nothing.
    #[inline]
    #[must_use]
    pub fn of(
        at: (i32, i32),
        reach: i32,
        (quads, halos): (&[GlowQuad], &[RainHalo]),
        keys: &mut Vec<u64>,
    ) -> Option<Self> {
        let mut rect = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        let mut any = false;
        for q in quads {
            if q.w > 0 && q.h > 0 {
                let (x, y) = (i32::from(q.x), i32::from(q.y));
                rect = (
                    rect.0.min(x),
                    rect.1.max(x + i32::from(q.w)),
                    rect.2.min(y),
                    rect.3.max(y + i32::from(q.h)),
                );
                any = true;
            }
        }
        for h in halos {
            if h.w > 0 && h.h > 0 {
                let (x, y) = (i32::from(h.x), i32::from(h.y));
                rect = (
                    rect.0.min(x),
                    rect.1.max(x + i32::from(h.w)),
                    rect.2.min(y),
                    rect.3.max(y + i32::from(h.h)),
                );
                any = true;
            }
        }
        if !any {
            return None;
        }
        let k0 = keys.len() as u32;
        keys.extend(quads.iter().map(sky_quad_key));
        keys.extend(halos.iter().map(sky_halo_key));
        Some(Self {
            at,
            reach,
            rect,
            lights: (k0, keys.len() as u32),
        })
    }
}

/// A star's quad as its mark keys it ([`SkyMark::lights`]).
#[inline]
#[must_use]
pub const fn sky_quad_key(q: &GlowQuad) -> u64 {
    let place = (q.x as u64) | (q.y as u64) << 16 | (q.w as u64) << 32 | (q.h as u64) << 48;
    let ink = (q.color as u64)
        | (q.alpha as u64) << 24
        | (q.color2 as u64) << 32
        | (q.alpha2 as u64) << 56;
    aterm_hash::fnv_mix_u64(
        aterm_hash::fnv_mix_u64(aterm_hash::FNV64_OFFSET, place),
        ink,
    )
}

/// A star's halo as its mark keys it ([`SkyMark::lights`]).
#[inline]
#[must_use]
pub const fn sky_halo_key(h: &RainHalo) -> u64 {
    let place = (h.x as u64) | (h.y as u64) << 16 | (h.w as u64) << 32 | (h.h as u64) << 48;
    let shape = (h.cx as u64) | (h.cy as u64) << 16 | (h.rx as u64) << 32 | (h.ry as u64) << 48;
    let seed = aterm_hash::fnv_mix_u64(aterm_hash::FNV64_OFFSET ^ 0x4841_4C4F, place);
    aterm_hash::fnv_mix_u64(aterm_hash::fnv_mix_u64(seed, shape), h.color as u64)
}

impl Default for Stardust {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Stardust {
    /// An empty sky with a full bucket and the pool reserved once (§18).
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            stars: Vec::with_capacity(STAR_POOL_MAX),
            budget: StarBudget::new(),
            probe: GlyphProbe::new(),
            glints: Vec::with_capacity(usize::from(HERO_MAX_LIVE_PER_ROW) * 2),
            minted: 0,
            reduced: false,
            cell_h: 0.0,
            veil: Vec::with_capacity(AURORA_CAP),
            last_typed: None,
            ioi_ms: STREAM_IOI_MAX_MS,
            sky_index: SkyIndex::EMPTY,
            shade: PetShade::EMPTY,
            keep_out: KeepOut::EMPTY,
            marks: Vec::with_capacity(STAR_POOL_MAX),
            mark_lights: Vec::with_capacity(STAR_POOL_MAX * 8),
            paper_ink: PaperInkMemo::EMPTY,
        }
    }

    /// Each star the last frame painted ([`SkyMark`]).
    #[inline]
    #[must_use]
    pub const fn marks(&self) -> &[SkyMark] {
        self.marks.as_slice()
    }

    /// The keys of the lights the last frame's stars painted, each star's a
    /// range ([`SkyMark::lights`]).
    #[inline]
    #[must_use]
    pub const fn mark_lights(&self) -> &[u64] {
        self.mark_lights.as_slice()
    }

    /// Forget the frame's marks (a frame whose output was withdrawn).
    #[inline]
    pub fn clear_marks(&mut self) {
        self.marks.clear();
        self.mark_lights.clear();
    }

    /// The frame's glyph truth, written by the host before the deal pass (§5.4).
    #[inline]
    pub const fn probe_mut(&mut self) -> &mut GlyphProbe {
        &mut self.probe
    }

    /// Read-only view of the frame's glyph truth.
    #[inline]
    #[must_use]
    pub const fn probe(&self) -> &GlyphProbe {
        &self.probe
    }

    /// Every star in the pool, finishing ones included — what is on glass.
    #[inline]
    pub fn live_iter(&self) -> impl Iterator<Item = &Star> {
        self.stars.iter()
    }

    /// Drain the glints minted since the last [`Stardust::emit`] (§13), one per token spent, at the
    /// hero's column.
    #[cfg(test)]
    pub fn take_glints(&mut self) -> std::vec::Drain<'_, Glint> {
        self.glints.drain(..)
    }

    /// Deal this event's births against the ribbon's planned bands: after [`Ribbon::plan`] and
    /// before any emit, since §5.4's zones are relative to [`super::ribbon::Band::top`] and the
    /// cell may have been laid this frame (T2).
    pub fn on_event(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) {
        match *ev {
            Event::Typed { cells, class, .. } => {
                self.note_cadence(at, cells);
                self.relight_field(ctx.caret, at, ctx.geom);
                self.deal_typed(cells, class, at, ctx, ribbon);
            }
            Event::Erase => {
                self.finish_field(ctx.caret, KILL_RETRACT_BASE_MS, at, ctx.geom);
                self.deal_backspace(at, ctx);
            }
            Event::Kill { cells, .. } => {
                self.finish_field(ctx.caret, kill_span_ms(cells), at, ctx.geom);
                self.deal_kill(cells, at, ctx);
            }
            Event::Rewrite { row, col, cells } => {
                self.finish_field((row, col), kill_span_ms(cells), at, ctx.geom);
            }
            Event::Move { .. }
            | Event::Return
            | Event::ComposerNewline
            | Event::CancelComposerNewline
            | Event::Sweep { .. } => {}
            Event::Focus(false) => self.ember(at),
            Event::Focus(true) | Event::ReducedMotion(_) => {}
        }
    }

    /// An earned hero (§5.8): a shifted capital, `!`, kitty Delight, or the landing fan's slot.
    #[inline]
    pub fn earn_hero(&mut self, at: Instant, cell: (u16, u16), ctx: &Ctx<'_>, ribbon: &Ribbon) {
        let spec = Deal {
            class: StarClass::M1,
            lane: StarLane::Strike,
            cell,
            earned: true,
            pull: None,
            tint: None,
        };
        self.deal_star(spec, at, ctx, ribbon);
    }

    #[inline]
    fn sow(&mut self, star: Star) {
        self.make_room(star.born);
        self.stars.push(star);
    }

    /// The cat catches a star: put the one live, unfinished gold sky m1 whose home pixel and birth
    /// edge are `(x, y, born)` on its catch finish, on the frame the paw lands.
    #[inline]
    pub fn catch(&mut self, x: f32, y: f32, born: Instant, at: Instant) -> bool {
        let Some(star) = self.stars.iter_mut().find(|s| {
            s.gold
                && s.class == StarClass::M1
                && s.lane.is_sky()
                && !s.dead(at)
                && s.born == born
                && s.x == x
                && s.y == y
        }) else {
            return false;
        };
        let before = star.finish;
        star.finish_by(at, CATCH_FINISH_MS / 1000.0);
        star.finish != before
    }

    /// Shed fragments (§6.5 layer 5): `spec.n` m3 grains born at `spec.at_px`, each inheriting
    /// [`SHED_ALONG_SHARE`] of the head's velocity plus a seeded perpendicular sputter, under
    /// [`DRAG_TAU_MS`].
    pub fn sow_shed(&mut self, at: Instant, spec: ShedSow, ctx: &Ctx<'_>) {
        if spec.n == 0 {
            return;
        }
        let (vx, vy) = spec.v_head;
        let speed = vx.hypot(vy).max(f32::EPSILON);
        let inv_speed = 1.0 / speed;
        let (ux, uy) = (vx * inv_speed, vy * inv_speed);
        let (along_x, along_y) = (vx * SHED_ALONG_SHARE, vy * SHED_ALONG_SHARE);
        let scale = px_scale(ctx.geom.ch as f32);
        let tint = spectrum_snap(spec.tint_t);
        for k in 0..spec.n {
            let seed = mix32(spec.seed ^ u32::from(k).wrapping_mul(0x9E37_79B9));
            if !light_keeps(seed, ctx.cfg) {
                continue;
            }
            let side = if seed & 1 == 0 { 1.0 } else { -1.0 };
            let perp = side
                * (SHED_PERP_MIN + (SHED_PERP_MAX - SHED_PERP_MIN) * hash01(mix32(seed ^ 1)))
                * scale;
            let v0 = (along_x - uy * perp, along_y + ux * perp);
            self.throw(
                Throw {
                    class: StarClass::M3,
                    lane: StarLane::Shed,
                    at_px: spec.at_px,
                    v0,
                    tint,
                    gold: false,
                    seed,
                    fixed: false,
                },
                at,
                ctx,
            );
        }
    }

    /// The landing fan (§6.5 layer 11, D6): in throw order a gold m1 and 4 m2 (with a hero, else
    /// one m2), then m3, thrown radially over [`FAN_THROW_MS`] to `reach·(0.55..1.05)`, each but
    /// the hero coloured by the landing walk at its rest column.
    pub fn sow_fan(&mut self, at: Instant, spec: FanSow, ctx: &Ctx<'_>) {
        let n = usize::from(spec.n).max(1);
        let inv_n = 1.0 / (n as f32);
        let inv_cw = 1.0 / (ctx.geom.cw as f32).max(1.0);
        let (rise_up, rise_down) = fan_rise(spec.reach, spec.at_px, ctx.geom);
        for k in 0..n {
            let seed = mix32(spec.seed ^ (k as u32).wrapping_mul(0x85EB_CA6B));
            let class = fan_class(k, spec.hero);
            if class == StarClass::M3 && !light_keeps(seed, ctx.cfg) {
                continue;
            }
            let a = ((k as f32 + hash01(seed)) * inv_n) * std::f32::consts::TAU;
            let reach = spec.reach
                * (FAN_THROW_JITTER_MIN
                    + (FAN_THROW_JITTER_MAX - FAN_THROW_JITTER_MIN) * hash01(mix32(seed ^ 3)));
            let (dx, dy) = fan_ray(a, reach, rise_up, rise_down, hash01(mix32(seed ^ 5)));
            let v0 = (dx * (1.0 / FAN_THROW_MS), dy * (1.0 / FAN_THROW_MS));
            let gold = class == StarClass::M1 && spec.hero;
            let tint = if gold {
                TINT_GOLD_RGB
            } else {
                spectrum_snap(spec.walk.at(dx * inv_cw))
            };
            self.throw(
                Throw {
                    class,
                    lane: StarLane::Fan,
                    at_px: spec.at_px,
                    v0,
                    tint,
                    gold,
                    seed,
                    fixed: class != StarClass::M3,
                },
                at,
                ctx,
            );
        }
    }

    /// The mini-fan (§6.12, D17): a same-row nav hop of 2-7 cells.
    pub fn sow_mini_fan(&mut self, at: Instant, at_px: (f32, f32), seed: u32, ctx: &Ctx<'_>) {
        let n = MINI_FAN_M2_N + MINI_FAN_M3_N;
        const INV_N: f32 = 1.0 / (MINI_FAN_M2_N + MINI_FAN_M3_N) as f32;
        let scale = px_scale(ctx.geom.ch as f32);
        for k in 0..n {
            let s = mix32(seed ^ u32::from(k).wrapping_mul(0xC2B2_AE35));
            let class = if k < MINI_FAN_M2_N {
                StarClass::M2
            } else {
                StarClass::M3
            };
            if class == StarClass::M3 && !light_keeps(s, ctx.cfg) {
                continue;
            }
            let a = ((f32::from(k) + hash01(s)) * INV_N) * std::f32::consts::TAU;
            let reach = (MINI_FAN_REACH_MIN_PX
                + (MINI_FAN_REACH_MAX_PX - MINI_FAN_REACH_MIN_PX) * hash01(mix32(s ^ 5)))
                * scale;
            let (sa, ca) = a.sin_cos();
            let v = reach * (1.0 / MINI_FAN_THROW_MS);
            let v0 = (ca * v, sa * v);
            let (tint, gold) = class_tint(class, s, ctx.caret_t);
            self.throw(
                Throw {
                    class,
                    lane: StarLane::MiniFan,
                    at_px,
                    v0,
                    tint,
                    gold,
                    seed: s,
                    fixed: true,
                },
                at,
                ctx,
            );
        }
    }

    /// Draw the sky.
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.reduced = ctx.cfg.reduced_motion;
        self.cell_h = ctx.geom.ch as f32;
        for g in self.glints.drain(..) {
            frame.cues.push(SoundCue {
                kind: SoundKind::Stardust {
                    twinkle_hz: g.f.round().clamp(0.0, 255.0) as u8,
                },
                col: g.col,
                heat: ctx.disp,
                hue: ctx.caret_t,
                dir: 0,
                shifted: false,
            });
        }
        self.cull(ctx.now);
        self.marks.clear();
        self.mark_lights.clear();
        if self.stars.is_empty() && self.veil.is_empty() {
            return;
        }
        let quad_cap = frame.out.len() + STARDUST_QUAD_BUDGET;
        let halo_cap = frame.halos.len() + STARDUST_LIGHT_HALO_BUDGET;
        let mut index = std::mem::take(&mut self.sky_index);
        if ctx.cfg.dark_theme && !self.stars.is_empty() {
            index.open(ctx.geom, (frame.out.len(), frame.halos.len()));
        }
        self.draw_veil(ctx, frame.halos);
        if self.stars.is_empty() {
            self.sky_index = index;
            return;
        }
        let mut haloed = 0usize;
        let n = self.stars.len();
        let mut classes = 0u8;
        for star in &self.stars {
            classes |= 1u8 << (star.class as u8);
        }
        let ch = ctx.geom.ch as f32;
        let mut i = 0usize;
        for class in [StarClass::M1, StarClass::M2, StarClass::M3] {
            if classes & (1u8 << (class as u8)) == 0 {
                continue;
            }
            let arms = class.arms(ch);
            for star in self.stars.iter().filter(|s| s.class == class) {
                let turn = i;
                i += 1;
                if !star.inks(ctx.cfg) {
                    continue;
                }
                let Some(paint) =
                    Paint::of_kept(star, &self.probe, &self.shade, &self.keep_out, ctx, arms)
                else {
                    continue;
                };
                let left = quad_cap.saturating_sub(frame.out.len());
                if left == 0 {
                    self.sky_index = index;
                    return;
                }
                let share = (left / (n - turn)).max(M3_QUADS).min(left);
                let star_cap = frame.out.len() + share;
                let (q0, h0) = (frame.out.len(), frame.halos.len());
                if ctx.cfg.dark_theme {
                    paint.draw_add(ctx, frame, star_cap, &mut haloed, &mut index);
                } else {
                    paint.draw_over(ctx.geom, ctx.cfg, &mut self.paper_ink, frame, halo_cap);
                }
                if let Some(mark) = SkyMark::of(
                    (paint.x, paint.y),
                    paint.reach(),
                    (&frame.out[q0..], &frame.halos[h0..]),
                    &mut self.mark_lights,
                ) {
                    self.marks.push(mark);
                }
            }
        }
        self.sky_index = index;
    }

    /// Stars in the sky, `trail status`'s `v2_stars=` row (seam point 12).
    #[inline]
    #[must_use]
    pub const fn live(&self) -> usize {
        self.stars.len()
    }

    /// True when the sky is empty — one of the three pools `Engine::next_change_deadline` folds
    /// (§18).
    #[must_use]
    #[cfg(test)]
    pub const fn at_rest(&self) -> bool {
        self.stars.is_empty() && self.veil.is_empty()
    }

    /// The cadence law, brisk half: whether any star moves every frame ([`Star::brisk`]).
    #[inline]
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        !self.reduced
            && (self.stars.iter().any(|s| s.brisk(now))
                || self.veil.iter().any(|v| v.age_s(now) < EDGE_IN_S))
    }

    /// The cadence law: the next instant a star changes what is on glass, or `None` when the sky is
    /// empty (T6).
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        if self.stars.is_empty() && self.veil.is_empty() {
            return None;
        }
        let mut cad = Cadence::at(now);
        if self.brisk(now) {
            cad.brisk();
            return cad.take();
        }
        let fade_life = FIELD_FADE_LIFE_S;
        const FADE_LEAD_S: f32 = REDUCED_MOTION_FADE_MS / 1000.0;
        let fade_from_idle = fade_life - FADE_LEAD_S;
        const FADE_FROM_FIELD: f32 = FIELD_LIFE_S - FADE_LEAD_S;
        const FIELD_HOLD_S: f32 = FIELD_HOLD_MS / 1000.0;
        const FIELD_MELT_START: f32 = FIELD_LIFE_S * (1.0 - EXPIRY_MELT_SHARE);
        for v in &self.veil {
            if cad.at_tail_floor() {
                return cad.take();
            }
            let idle = v.idle_s(now);
            let age = v.age_s(now);
            if let Some(f) = v.finish {
                cad.tail_at(f.cull_at());
                cad.tail(0.0);
            }
            if self.reduced {
                cad.tail((fade_from_idle - idle).max(0.0));
                cad.tail((FADE_FROM_FIELD - age).max(0.0));
            } else {
                if idle < FIELD_HOLD_S {
                    cad.tail(FIELD_HOLD_S - idle);
                } else {
                    cad.tail(0.0);
                }
                if age < EDGE_IN_S {
                    cad.tail(EDGE_IN_S - age);
                } else if age >= FIELD_MELT_START {
                    cad.tail(0.0);
                }
            }
            cad.tail(fade_life - idle);
            cad.tail(FIELD_LIFE_S - age);
        }
        for s in &self.stars {
            if cad.at_tail_floor() {
                return cad.take();
            }
            let age = s.age_s(now);
            let life = s.life_s();
            cad.tail(life - age);
            if let Some(f) = s.finish {
                cad.tail_at(f.cull_at());
                cad.tail(0.0);
            }
            if s.lane == StarLane::Field {
                let idle = s.idle_s(now);
                if self.reduced {
                    cad.tail((fade_from_idle - idle).max(0.0));
                } else if idle < FIELD_HOLD_S {
                    cad.tail(FIELD_HOLD_S - idle);
                }
                cad.tail(fade_life - idle);
            }
            if self.reduced {
                let fade_from = life - FADE_LEAD_S;
                if age < fade_from {
                    cad.tail(fade_from - age);
                } else {
                    cad.tail(0.0);
                }
                continue;
            }
            let (hold, _) = s.class.envelope_s(s.lane);
            if age < hold {
                cad.tail(hold - age);
            } else if let Some(step) = s.next_arm_step_s(self.cell_h, now) {
                cad.tail(step);
            }
            if cad.at_tail_floor() {
                return cad.take();
            }
            if let Some(dt) = s.next_glint_toggle_s(now) {
                cad.tail(dt);
            }
            if let Some(dt) = s.next_lift_step_s(now) {
                cad.tail(dt);
            }
            if let Some(dt) = s.next_stream_step_s(now) {
                cad.tail(dt);
            }
        }
        cad.take()
    }

    /// Drop the sky: style switch, layout change, `Engine::reset`.
    #[inline]
    pub fn reset(&mut self) {
        self.stars.clear();
        self.veil.clear();
        self.budget = StarBudget::new();
        self.probe.clear();
        self.glints.clear();
        self.minted = 0;
        self.last_typed = None;
        self.ioi_ms = STREAM_IOI_MAX_MS;
    }

    /// Seam point 12 (`translate_scroll_state`), the sky's half: move every star with the viewport
    /// by `rows` cell heights and drop those that leave the grid.
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        if rows == 0 {
            return;
        }
        if cell_h == 0 {
            self.stars.clear();
        } else {
            let dy = f32::from(rows) * f32::from(cell_h);
            self.stars.retain_mut(|s| {
                if let Some(row) = s.home_row {
                    let Some(row) = row.checked_sub(rows) else {
                        return false;
                    };
                    s.home_row = Some(row);
                }
                s.y -= dy;
                s.y >= 0.0
            });
        }
        self.veil.retain_mut(|v| {
            let Some(row) = v.row.checked_sub(rows) else {
                return false;
            };
            v.row = row;
            true
        });
        self.probe.clear();
    }

    /// Move sky stars with the glyph rows that earned them.
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
            self.stars.clear();
        } else {
            let px = BandPx::new(top, bottom, delta, cell_h, origin_y);
            self.stars.retain_mut(|s| {
                if let Some(home) = s.home_row {
                    let Some(row) = band_row(home, top, bottom, delta) else {
                        return false;
                    };
                    s.y += (i32::from(row) - i32::from(home)) as f32 * f32::from(cell_h);
                    s.home_row = Some(row);
                    true
                } else {
                    px.point(&mut s.y)
                }
            });
        }
        self.veil
            .retain_mut(|v| match band_row(v.row, top, bottom, delta) {
                Some(row) => {
                    v.row = row;
                    true
                }
                None => false,
            });
        self.probe.clear();
    }

    fn deal_typed(
        &mut self,
        cells: u16,
        class: TypedClass,
        at: Instant,
        ctx: &Ctx<'_>,
        ribbon: &Ribbon,
    ) {
        self.deal_veil(cells, at, ctx, ribbon);
        if class == TypedClass::Space {
            return;
        }
        let scale = if ctx.cfg.dark_theme {
            1.0
        } else {
            LIGHT_COUNT_SCALE
        };
        let field_step = if ctx.disp >= FIELD_HOT_DISP {
            FIELD_SHARE_FLOW
        } else {
            FIELD_SHARE_COLD
        };
        let field_share = field_step
            .max(FIELD_SHARE_COLD + (FIELD_SHARE_FLOW - FIELD_SHARE_COLD) * clamp01(ctx.flow.heat));
        let m1 = scale * (1.0 / DEAL_M1_IN as f32);
        let m2 = m1 + scale * (1.0 / DEAL_M2_IN as f32);
        let m3 = m2 + scale * (1.0 / DEAL_M3_IN as f32);
        let earned = matches!(class, TypedClass::Capital | TypedClass::Bang);
        let (row, head) = ctx.caret;
        let n_cells = cells.max(1);
        for k in 0..n_cells {
            let col = head.saturating_sub(n_cells - k);
            let cell = (row, col);
            let h = cell_hash(row, col, 0);

            let mended = match ctx.mend {
                Some(m) if (m.row, m.col) == cell => self.deal_star(
                    Deal {
                        class: if earned { StarClass::M1 } else { StarClass::M2 },
                        lane: StarLane::Strike,
                        cell: (row, col.saturating_add(1)),
                        earned,
                        pull: Some(Pull {
                            from: cell,
                            disp: m.disp,
                        }),
                        tint: None,
                    },
                    at,
                    ctx,
                    ribbon,
                ),
                _ => false,
            };

            let u = hash01(mix32(h ^ SALT_STRIKE));
            let dealt = if mended {
                None
            } else if earned || u < m1 {
                Some(StarClass::M1)
            } else if u < m2 {
                Some(StarClass::M2)
            } else if u < m3 {
                Some(StarClass::M3)
            } else {
                None
            };
            let rung = (k + 1 == n_cells && !mended)
                .then(|| ladder_rung(ctx.flow.combo))
                .flatten();
            let (dealt, earned, tint) = match rung {
                Some(Rung::Promote) => (
                    Some(dealt.map_or(StarClass::M2, |c| c.min(StarClass::M2))),
                    earned,
                    None,
                ),
                Some(Rung::White) => (
                    Some(dealt.map_or(StarClass::M2, |c| c.min(StarClass::M2))),
                    earned,
                    Some((TINT_WHITE_RGB, false)),
                ),
                Some(Rung::Gold) => (Some(StarClass::M1), true, Some((TINT_GOLD_RGB, true))),
                None => (dealt, earned, None),
            };
            if let Some(class) = dealt
                && let Some(free) = self.free_sky_cell(cell, ctx.geom)
            {
                self.deal_star(
                    Deal {
                        class,
                        lane: StarLane::Strike,
                        cell: free,
                        earned,
                        pull: None,
                        tint,
                    },
                    at,
                    ctx,
                    ribbon,
                );
            }
            let lead = (row, col.saturating_add(1));
            if ctx.disp >= STRIKE_SECOND_M3_DISP
                && deal(h, SALT_SECOND_M3, 1, scale)
                && !self.sky_star_live(lead, ctx.geom)
            {
                self.deal_star(
                    Deal {
                        class: StarClass::M3,
                        lane: StarLane::Strike,
                        cell: lead,
                        earned: false,
                        pull: None,
                        tint: None,
                    },
                    at,
                    ctx,
                    ribbon,
                );
            }

            if mended {
                continue;
            }
            if deal_p(h, SALT_FIELD, scale * field_share) {
                if self.adopt_field(cell, at, ctx.geom) {
                    continue;
                }
                let fc = if deal(h, SALT_FIELD_M2, FIELD_M2_IN, 1.0) {
                    StarClass::M2
                } else {
                    StarClass::M3
                };
                self.deal_star(
                    Deal {
                        class: fc,
                        lane: StarLane::Field,
                        cell,
                        earned: false,
                        pull: None,
                        tint: None,
                    },
                    at,
                    ctx,
                    ribbon,
                );
            }
        }
        self.sow_exhaust(at, ctx, ribbon);
    }

    fn sow_exhaust(&mut self, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) {
        // A throw is motion (§6.11).
        if ctx.cfg.reduced_motion {
            return;
        }
        let birth_disp = ctx
            .mend
            .map_or(ctx.birth_disp, |m| ctx.birth_disp.max(m.disp));
        if birth_disp < STREAM_DISP {
            return;
        }
        let g = ctx.geom;
        let cw = g.cw as f32;
        let wind = stream_px_per_s_with_share(EXHAUST_SHARE_CW, cw, self.ioi_ms);
        if wind <= 0.0 {
            return;
        }
        if self.stars.len() + EXHAUST_HEADROOM > STAR_CAP {
            let live = self.stars.iter().filter(|s| s.finish.is_none()).count();
            if live + EXHAUST_HEADROOM > STAR_CAP {
                return;
            }
        }
        let (row, col) = ctx.caret;
        let seed = mix32(cell_hash(row, col, self.minted) ^ SALT_EXHAUST);
        let class =
            if birth_disp >= FIELD_HOT_DISP && deal(seed, SALT_EXHAUST ^ 1, EXHAUST_M2_IN, 1.0) {
                StarClass::M2
            } else {
                StarClass::M3
            };
        let Some((x, y)) = self.sky_birth_px((row, col), seed, ctx, ribbon) else {
            return;
        };
        let want = stream_travel_px(wind).min(WALK_FAST_CELLS * cw) * stream_unit(EXHAUST_THROW_MS);
        let rise = EXHAUST_RISE_CH * g.ch as f32 * (0.5 + 0.5 * hash01(mix32(seed ^ 3)));
        let rows = px_row(y - rise, g)..=px_row(y, g);
        if !col.checked_sub(1).is_some_and(|c| {
            rows.into_iter()
                .all(|r| self.probe.sky_at(r, c, g.rows) == Some(false))
        }) {
            return;
        }
        let reach = self.blank_frontier_px(x, (y - rise, y), want, ctx);
        if reach < 0.5 {
            return;
        }
        let rise = rise.min(self.rule_headroom_px(px_col(x - reach, g)..=i32::from(col), y, g));
        let v0 = (
            -reach * (1.0 / EXHAUST_THROW_MS),
            -rise * (1.0 / EXHAUST_THROW_MS),
        );
        let (tint, gold) = class_tint(class, seed, self.field_t((row, col), ctx, ribbon));
        self.throw(
            Throw {
                class,
                lane: StarLane::Exhaust,
                at_px: (x, y),
                v0,
                tint,
                gold,
                seed,
                fixed: false,
            },
            at,
            ctx,
        );
    }

    #[inline]
    fn deal_backspace(&mut self, at: Instant, ctx: &Ctx<'_>) {
        let (row, col) = ctx.caret;
        for k in 0..(ERASE_M3_N + ERASE_M2_N) {
            let class = if k < ERASE_M3_N {
                StarClass::M3
            } else {
                StarClass::M2
            };
            self.throw_erase(class, (row, col), k, at, ctx);
        }
    }

    #[inline]
    fn deal_kill(&mut self, cells: u16, at: Instant, ctx: &Ctx<'_>) {
        let n = (cells / KILL_CELLS_PER_STAR).clamp(1, KILL_STAR_MAX);
        let (row, col) = ctx.caret;
        let last = ctx.geom.cols.saturating_sub(1).min(usize::from(u16::MAX)) as u16;
        for k in 0..n {
            let class = if k % (ERASE_M3_N + ERASE_M2_N) == ERASE_M3_N {
                StarClass::M2
            } else {
                StarClass::M3
            };
            let at_col = col
                .saturating_add(k * KILL_CELLS_PER_STAR + KILL_CELLS_PER_STAR / 2)
                .min(last);
            self.throw_erase(class, (row, at_col), k, at, ctx);
        }
    }

    fn throw_erase(
        &mut self,
        class: StarClass,
        cell: (u16, u16),
        k: u16,
        at: Instant,
        ctx: &Ctx<'_>,
    ) {
        let (row, col) = cell;
        let seed = mix32(self.mint_seed(row, col) ^ SALT_ERASE ^ u32::from(k));
        if class == StarClass::M3 && !light_keeps(seed, ctx.cfg) {
            return;
        }
        let (cx, cy) = ctx.geom.cell_center(row, col);
        let a = std::f32::consts::PI * (1.0 + hash01(seed));
        let reach = (ERASE_REACH_MIN_PX
            + (ERASE_REACH_MAX_PX - ERASE_REACH_MIN_PX) * hash01(mix32(seed ^ 7)))
            * px_scale(ctx.geom.ch as f32);
        let (sa, ca) = a.sin_cos();
        let v = reach * (1.0 / ERASE_THROW_MS);
        let v0 = (ca * v, sa * v);
        let (tint, gold) = deal_tint(seed, ctx.caret_t);
        self.throw(
            Throw {
                class,
                lane: StarLane::Erase,
                at_px: (cx, cy),
                v0,
                tint,
                gold,
                seed,
                fixed: false,
            },
            at,
            ctx,
        );
    }

    fn deal_star(&mut self, spec: Deal, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) -> bool {
        let mut grabbed: Option<Option<usize>> = None;
        if let Some(p) = spec.pull {
            let Some(g) = self.pull_origin(p.from, at, ctx) else {
                return false;
            };
            grabbed = Some(g);
        }
        let seed = match grabbed {
            Some(Some(i)) => self.stars[i].seed,
            _ => self.mint_seed(spec.cell.0, spec.cell.1),
        };
        let Some((x, y)) = self.sky_birth_px(spec.cell, seed, ctx, ribbon) else {
            return false;
        };
        if grabbed.is_some() {
            let g = ctx.geom;
            for (i, s) in self.stars.iter_mut().enumerate() {
                if grabbed != Some(Some(i)) && s.lane.is_sky() && star_over_cell(s, spec.cell, g) {
                    s.finish_by(at, EVICT_FINISH_MS / 1000.0);
                }
            }
        }
        let mut class = spec.class;
        if class == StarClass::M1 && !spec.earned && !self.hero_allowed(spec.cell, y, ctx.geom) {
            class = StarClass::M2;
        }
        let mut glint = false;
        if class == StarClass::M1 && spec.lane.may_chime() {
            if self.budget.try_spend() {
                glint = true;
            } else if !spec.earned {
                class = StarClass::M2;
            }
        }
        let (tint, gold) = spec
            .tint
            .unwrap_or_else(|| class_tint(class, seed, self.field_t(spec.cell, ctx, ribbon)));
        let from_x = match grabbed {
            Some(Some(i)) => self.stars[i].pos(at, self.reduced).0 as f32,
            Some(None) => x - ctx.geom.cw as f32,
            None => x,
        };
        let pull = (x - from_x).max(0.0);
        let birth_disp = spec
            .pull
            .map_or(ctx.birth_disp, |p| ctx.birth_disp.max(p.disp));
        let stream = if birth_disp >= STREAM_DISP {
            self.stream_reach(from_x, y, ctx)
        } else {
            0.0
        };
        let v0 = match (class, spec.lane) {
            _ if spec.pull.is_some() => (0.0, 0.0),
            (StarClass::M1 | StarClass::M2, StarLane::Strike) => {
                let g = ctx.geom;
                let lift = sky_lift_px_per_s(g.ch as f32) * (1.0 / 1000.0);
                let life_ms = class.life_s(spec.lane) * 1000.0;
                let room = self.rule_headroom_px(px_col(x - stream, g)..=px_col(x, g), y, g);
                (0.0, lift.max(-room / life_ms))
            }
            _ => (0.0, 0.0),
        };
        let f = scint_rate(seed);
        if glint {
            self.glints.push(Glint {
                col: spec.cell.1,
                f,
            });
        }
        let star = Star {
            class,
            lane: spec.lane,
            x,
            y,
            home_row: Some(spec.cell.0),
            born: at,
            lit: at,
            seed,
            tint,
            gold,
            v0,
            f,
            stream,
            pull,
            finish: None,
        };
        match grabbed {
            Some(Some(i)) => self.stars[i] = star,
            _ => self.sow(star),
        }
        true
    }

    #[inline]
    fn throw(&mut self, spec: Throw, at: Instant, ctx: &Ctx<'_>) {
        let mut star = Star {
            class: spec.class,
            lane: spec.lane,
            x: spec.at_px.0.round(),
            y: spec.at_px.1.round(),
            home_row: None,
            born: at,
            lit: at,
            seed: spec.seed,
            tint: spec.tint,
            gold: spec.gold,
            v0: spec.v0,
            f: scint_rate(spec.seed),
            stream: 0.0,
            pull: 0.0,
            finish: None,
        };
        if self.settle(&mut star, spec.fixed, ctx) {
            self.sow(star);
        }
    }

    fn sky_birth_px(
        &self,
        cell: (u16, u16),
        seed: u32,
        ctx: &Ctx<'_>,
        ribbon: &Ribbon,
    ) -> Option<(f32, f32)> {
        let (row, col) = cell;
        if usize::from(row) >= ctx.geom.rows || usize::from(col) >= ctx.geom.cols {
            return None;
        }
        if self.probe.sky_at(i32::from(row) - 1, col, ctx.geom.rows) != Some(false) {
            return None;
        }
        let g = ctx.geom;
        let ch = g.ch as f32;
        // The sky is never below the glyph row's top: under `underline` the
        // band's top lies among the letters, where a star reads as an
        // apostrophe on paper. Above the row's top it is in the row above's
        // foot (a pixel clear of the row's top at any cell height), which the
        // probe certified blank.
        let cell_top = f32::from(g.origin_y) + f32::from(row) * ch;
        let top = ribbon
            .band(row, col)
            .map_or_else(|| default_band_top(g, ctx.cfg, row), |b| b.top)
            .min(cell_top);
        let j = mix32(seed ^ SALT_JITTER);
        let y = (top - ch * (SKY_BOTTOM_CH + (SKY_TOP_CH - SKY_BOTTOM_CH) * hash01(j)))
            .min(cell_top - 1.0);
        let (cx, _) = g.cell_center(row, col);
        let jx = (hash01(mix32(j ^ 11)) * 2.0 - 1.0) * SKY_JITTER_CW * g.cw as f32;
        let (x, y) = ((cx + jx).round(), y.round());
        let x_lo = (cx - SKY_JITTER_CW * g.cw as f32).round();
        let x_hi = (cx + SKY_JITTER_CW * g.cw as f32).round();
        let y_lo = (top - SKY_TOP_CH * ch).round();
        let y_hi = (top - SKY_BOTTOM_CH * ch).min(cell_top - 1.0).round();
        let glass = |x: f32, y: f32| on_glass((x as i32, y as i32), g);
        if !glass(x, y) {
            return None;
        }
        if !self.pixel_taken(x, y) {
            return Some((x, y));
        }
        const NUDGE: [(f32, f32); 8] = [
            (0.0, -1.0),
            (0.0, 1.0),
            (-1.0, 0.0),
            (1.0, 0.0),
            (-1.0, -1.0),
            (1.0, -1.0),
            (-1.0, 1.0),
            (1.0, 1.0),
        ];
        NUDGE
            .iter()
            .map(|&(dx, dy)| (x + dx, y + dy))
            .find(|&(nx, ny)| {
                (x_lo..=x_hi).contains(&nx)
                    && (y_lo..=y_hi).contains(&ny)
                    && glass(nx, ny)
                    && !self.pixel_taken(nx, ny)
            })
    }

    #[inline]
    const fn pixel_taken(&self, x: f32, y: f32) -> bool {
        let stars = self.stars.as_slice();
        let mut i = 0;
        while i < stars.len() {
            if stars[i].x == x && stars[i].y == y {
                return true;
            }
            i += 1;
        }
        false
    }

    #[inline]
    fn note_cadence(&mut self, at: Instant, cells: u16) {
        if let Some(last) = self.last_typed {
            let gap_ms =
                at.saturating_duration_since(last).as_secs_f32() * 1000.0 / f32::from(cells.max(1));
            if gap_ms > 0.0 {
                self.ioi_ms = gap_ms.clamp(STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS);
            }
        }
        self.last_typed = Some(at);
    }

    #[inline]
    const fn stream_reach(&self, x: f32, y: f32, ctx: &Ctx<'_>) -> f32 {
        let want = stream_travel_px(stream_px_per_s(ctx.geom.cw as f32, self.ioi_ms));
        self.blank_frontier_px(x, (y, y), want, ctx)
    }

    const fn blank_frontier_px(&self, x: f32, band: (f32, f32), want: f32, ctx: &Ctx<'_>) -> f32 {
        let g = ctx.geom;
        if !want.is_finite() || want < 0.5 {
            return 0.0;
        }
        let col = px_col(x, g);
        if col < 0 {
            return 0.0;
        }
        let (first, last) = (px_row(band.0, g), px_row(band.1, g));
        let mut frontier = col;
        let fx_left = g.fx_left();
        let cw_i32 = g.cw as i32;
        while frontier > 0 {
            // Already clear of the priced travel: no column further left can
            // lower `want.min(x − floor_x)`, so the walk is over.
            if x - (fx_left + frontier * cw_i32) as f32 >= want {
                break;
            }
            let left = frontier - 1;
            let clear = if left >= 0 && left <= u16::MAX as i32 {
                let c = left as u16;
                let mut r = first;
                let mut all_clear = true;
                while r <= last {
                    if !matches!(self.probe.sky_at(r, c, g.rows), Some(false)) {
                        all_clear = false;
                        break;
                    }
                    r += 1;
                }
                all_clear
            } else {
                false
            };
            if !clear {
                break;
            }
            frontier = left;
        }
        want.min(x - (fx_left + frontier * cw_i32) as f32).max(0.0)
    }

    const fn rule_headroom_px(&self, cols: std::ops::RangeInclusive<i32>, y: f32, g: Geom) -> f32 {
        let row = px_row(y, g);
        if !self.probe.has_rule_in(row, cols) {
            return f32::INFINITY;
        }
        let row_top = g.origin_y as f32 + row as f32 * g.ch as f32;
        let band = rule_stroke_px(g.ch);
        if y < row_top + band.start as f32 {
            return f32::INFINITY;
        }
        (y - (row_top + band.end as f32)).max(0.0)
    }

    #[inline]
    fn pull_origin(&self, from: (u16, u16), at: Instant, ctx: &Ctx<'_>) -> Option<Option<usize>> {
        let g = ctx.geom;
        if usize::from(from.0) >= g.rows || usize::from(from.1) >= g.cols {
            return None;
        }
        if self.probe.at(i32::from(from.0) - 1, from.1, g.rows) != Some(false) {
            return None;
        }
        Some(
            self.stars
                .iter()
                .position(|s| s.lane.is_sky() && !s.dead(at) && star_over_cell(s, from, g)),
        )
    }

    fn settle(&self, star: &mut Star, fixed: bool, ctx: &Ctx<'_>) -> bool {
        const NUDGE: [(i32, i32); 12] = [
            (0, -1),
            (0, 1),
            (-1, 0),
            (1, 0),
            (-1, -1),
            (1, -1),
            (-1, 1),
            (1, 1),
            (0, -2),
            (0, 2),
            (-2, 0),
            (2, 0),
        ];
        let geom = ctx.geom;
        let origin = star.origin();
        let home = (star.lane != StarLane::Exhaust && star.lane.throw_ms().is_some())
            .then(|| (px_row(origin.1 as f32, geom), px_col(origin.0 as f32, geom)));
        let clear = |p: (i32, i32)| {
            on_glass(p, geom)
                && (home == Some((px_row(p.1 as f32, geom), px_col(p.0 as f32, geom)))
                    || self.probe.at_px(p.0, p.1, geom) != Some(true))
        };
        let motion = star.lane.motion();
        let k = motion.k_at(motion.end_ms(star.life_s() * 1000.0));
        let thrown = star.rest();
        let rest = clamp_to_glass(thrown, geom);
        if clear(rest) {
            if rest != thrown {
                star.aim(rest, k);
            }
            return true;
        }
        if let Some(p) = NUDGE
            .iter()
            .map(|&(dx, dy)| (rest.0 + dx, rest.1 + dy))
            .find(|&p| clear(p))
        {
            star.aim(p, k);
            return true;
        }
        if !fixed {
            return false;
        }
        let (dx, dy) = ((rest.0 - origin.0) as f32, (rest.1 - origin.1) as f32);
        let steps = dx.hypot(dy).ceil().max(1.0) as i32;
        let inv_steps = 1.0 / steps as f32;
        let p = (0..steps)
            .rev()
            .map(|i| {
                let t = i as f32 * inv_steps;
                (
                    origin.0 + (dx * t).round() as i32,
                    origin.1 + (dy * t).round() as i32,
                )
            })
            .find(|&p| clear(p))
            .unwrap_or(origin);
        star.aim(p, k);
        true
    }

    #[inline]
    const fn field_t(&self, cell: (u16, u16), ctx: &Ctx<'_>, ribbon: &Ribbon) -> f32 {
        match ribbon.field_at(cell.0, cell.1) {
            Some(t) => t,
            None => ctx.caret_t,
        }
    }

    #[inline]
    const fn hero_allowed(&self, cell: (u16, u16), y: f32, geom: Geom) -> bool {
        let band = px_row(y, geom);
        let stars = self.stars.as_slice();
        let mut on_row = 0usize;
        let mut i = 0;
        while i < stars.len() {
            let s = &stars[i];
            i += 1;
            if !matches!(s.class, StarClass::M1) || !s.lane.is_sky() || px_row(s.y, geom) != band {
                continue;
            }
            on_row += 1;
            let dc = (cell.1 as i32 - px_col(s.x, geom)).abs();
            if dc < HERO_MIN_SPACING_CELLS as i32 {
                return false;
            }
        }
        on_row < HERO_MAX_LIVE_PER_ROW as usize
    }

    #[inline]
    const fn sky_star_live(&self, cell: (u16, u16), geom: Geom) -> bool {
        let stars = self.stars.as_slice();
        let mut i = 0;
        while i < stars.len() {
            let s = &stars[i];
            if s.lane.is_sky() && star_over_cell(s, cell, geom) {
                return true;
            }
            i += 1;
        }
        false
    }

    #[inline]
    const fn free_sky_cell(&self, cell: (u16, u16), geom: Geom) -> Option<(u16, u16)> {
        let mut k = 0;
        while k <= STRIKE_REACH_CELLS {
            let c = (cell.0, cell.1.saturating_add(k));
            if !self.sky_star_live(c, geom) {
                return Some(c);
            }
            k += 1;
        }
        None
    }

    #[inline]
    fn adopt_field(&mut self, cell: (u16, u16), at: Instant, geom: Geom) -> bool {
        let Some(s) = self
            .stars
            .iter_mut()
            .find(|s| s.lane.is_sky() && star_over_cell(s, cell, geom))
        else {
            return false;
        };
        s.lane = StarLane::Field;
        s.home_row = Some(cell.0);
        s.lit = at;
        s.v0 = (0.0, 0.0);
        true
    }

    #[inline]
    fn finish_field(&mut self, caret: (u16, u16), span_ms: f32, at: Instant, geom: Geom) {
        let (row, col) = caret;
        let col_i32 = i32::from(col);
        let span_s = span_ms * (1.0 / 1000.0);
        for s in &mut self.stars {
            if s.lane != StarLane::Field {
                continue;
            }
            if px_col(s.x, geom) >= col_i32 && in_sky_of_row(s.y, row, geom) {
                s.finish_by(at, span_s);
            }
        }
        for v in &mut self.veil {
            if v.row == row && v.col >= col {
                v.finish_by(at, span_s);
            }
        }
    }

    #[inline]
    fn relight_field(&mut self, caret: (u16, u16), at: Instant, geom: Geom) {
        for s in &mut self.stars {
            if s.lane == StarLane::Field && in_sky_of_row(s.y, caret.0, geom) {
                s.lit = at;
            }
        }
        for v in &mut self.veil {
            if v.row == caret.0 {
                v.lit = at;
            }
        }
    }

    #[inline]
    fn ember(&mut self, at: Instant) {
        const EMBER_S: f32 = FOCUS_EMBER_MS / 1000.0;
        for s in &mut self.stars {
            s.finish_by(at, EMBER_S);
        }
        for v in &mut self.veil {
            v.finish_by(at, EMBER_S);
        }
    }

    #[inline]
    fn make_room(&mut self, now: Instant) {
        self.cull(now);
        if self.stars.len() < STAR_CAP {
            return;
        }
        let mut live = 0usize;
        let mut oldest: Option<(usize, Instant)> = None;
        for (i, s) in self.stars.iter().enumerate() {
            if s.finish.is_none() {
                live += 1;
                if oldest.is_none_or(|(_, born)| s.born < born) {
                    oldest = Some((i, s.born));
                }
            }
        }
        if live >= STAR_CAP
            && let Some((idx, _)) = oldest
        {
            self.stars[idx].finish_by(now, EVICT_FINISH_MS / 1000.0);
        }
        if self.stars.len() >= STAR_POOL_MAX {
            let nearest_end = self
                .stars
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.finish.map(|f| (i, f.u(now))))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map_or(0, |(i, _)| i);
            self.stars.remove(nearest_end);
        }
    }

    #[inline]
    fn cull(&mut self, now: Instant) {
        self.stars.retain(|s| !s.dead(now));
        self.veil.retain(|v| !v.dead(now));
    }

    #[inline]
    const fn mint_seed(&mut self, row: u16, col: u16) -> u32 {
        self.minted = self.minted.wrapping_add(1);
        cell_hash(row, col, self.minted)
    }

    fn deal_veil(&mut self, cells: u16, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) {
        let gain = veil_gain(ctx.birth_disp);
        if gain <= 0.0 {
            return;
        }
        let (row, head) = ctx.caret;
        let g = ctx.geom;
        if usize::from(row) >= g.rows {
            return;
        }
        let n = cells.max(1);
        for k in 0..n {
            let col = head.saturating_sub(n - k);
            if usize::from(col) >= g.cols {
                continue;
            }
            if self.probe.at(i32::from(row) - 1, col, g.rows) != Some(false) {
                continue;
            }
            let veil = Veil {
                row,
                col,
                t: self.field_t((row, col), ctx, ribbon),
                gain,
                born: at,
                lit: at,
                finish: None,
            };
            if let Some(v) = self.veil.iter_mut().find(|v| v.row == row && v.col == col) {
                *v = veil;
            } else if self.veil.len() < AURORA_CAP {
                self.veil.push(veil);
            } else if let Some((i, _)) = self.veil.iter().enumerate().min_by_key(|(_, v)| v.lit) {
                self.veil[i] = veil;
            }
        }
    }

    fn draw_veil(&self, ctx: &Ctx<'_>, halos: &mut Vec<RainHalo>) {
        // On paper the aurora is not drawn: ink cannot glow, and a pale tint
        // along the row above the ribbon read as a flat strip with hard
        // edges and square ends (a selection's), not as light. The stars it
        // carried on dark keep their own marks.
        if !ctx.cfg.dark_theme || self.veil.is_empty() {
            return;
        }
        let g = ctx.geom;
        let ch = g.ch as f32;
        let tall = AURORA_TALL_CH * ch;
        let rx = (AURORA_REACH_CW * g.cw as f32).round() as i32;
        let origin_x = i32::from(g.origin_x);
        let cw = g.cw as i32;
        let intensity = clamp01(ctx.cfg.intensity);
        let cap = halos.len() + AURORA_HALO_BUDGET;
        for v in &self.veil {
            let env = v.envelope(ctx.now, ctx.cfg.reduced_motion);
            if env < CHROMA_CULL_ALPHA {
                continue;
            }
            let byte = cov_byte(AURORA_COV_PEAK * v.gain * env * intensity);
            if byte == 0 {
                continue;
            }
            let bottom = v.bottom_px(g, ctx.cfg);
            let top = bottom - tall;
            let (y0, y1) = (top.round() as i32, bottom.round() as i32);
            if y1 <= y0 {
                continue;
            }
            let (cx, _) = g.cell_center(v.row, v.col);
            let x0 = origin_x + i32::from(v.col) * cw;
            let x1 = x0 + cw;
            // The veil keeps off a lettered or filled cell of the row it
            // lies in (a powerline segment's bar under `underline`), as a
            // star and the ribbon's top do.
            let tall_px = y1 - y0;
            let (y0, y1) = self.keep_out.below((x0, x1), (y0, y1));
            if y1 <= y0 {
                continue;
            }
            let rgb = spectrum(clamp01(tri(v.t)));
            let color = premul_rgb(rgb, byte);
            push_halo_quads_clipped(
                halos,
                g,
                cap,
                HaloSpec {
                    centre: (cx.round() as i32, y1),
                    radii: (rx, tall_px),
                    color,
                    mode: HaloMode::Add,
                },
                (x0, y0, x1, y1),
            );
        }
    }
}

#[inline]
const fn fan_class(k: usize, hero: bool) -> StarClass {
    match (k, hero) {
        (0, true) => StarClass::M1,
        (0, false) | (1..=4, true) => StarClass::M2,
        _ => StarClass::M3,
    }
}

#[inline]
const fn star_over_cell(star: &Star, cell: (u16, u16), geom: Geom) -> bool {
    let up = cell.0 as i32 - px_row(star.y, geom);
    px_col(star.x, geom) == cell.1 as i32 && up >= 0 && up <= 1
}

#[inline]
const fn light_keeps(seed: u32, cfg: &Config) -> bool {
    cfg.dark_theme || hash01(mix32(seed ^ SALT_LIGHT)) < LIGHT_COUNT_SCALE
}

const M3_QUADS: usize = 5;

const _: () = assert!(
    STARDUST_QUAD_BUDGET >= M3_QUADS * STAR_POOL_MAX,
    "the quad budget cannot hand every star of a full pool its five-quad floor"
);

#[derive(Clone, Copy, Debug)]
struct Paint {
    class: StarClass,
    lane: StarLane,
    x: i32,
    y: i32,
    arm: i32,
    arm_lo: i32,
    arm_hi: i32,
    c: f32,
    alpha: f32,
    core: f32,
    env: f32,
    tint: u32,
    glints: bool,
    gold: bool,
}

impl Paint {
    /// Its arm's peak reach plus one, px: the reach every cull keys on
    /// ([`Self::kept_out`], [`SkyMark::reach`]). Only the drawn arm
    /// scintillates; a cull keyed on it would add and drop a star on a cull's
    /// edge at 16-24 Hz, past [`crate::TWINKLE_FLASH_BOUND_HZ`].
    #[inline]
    const fn reach(&self) -> i32 {
        let hi = if self.arm_hi > self.arm_lo {
            self.arm_hi
        } else {
            self.arm_lo
        };
        (if self.arm > hi { self.arm } else { hi }) + 1
    }

    /// Whether `(x, y)` with peak `reach` lies where no star is drawn: on a
    /// word icon or a lettered or filled cell of another row ([`KeepOut`], its
    /// centre within half its reach of one), or, on paper, with its reach
    /// within a cell of the caret's block ([`Ctx::near_paper_caret`]); the
    /// reach is the arm at its peak, so the answer holds for the star's life
    /// while it stands still.
    #[inline]
    const fn kept_out(x: i32, y: i32, reach: i32, ctx: &Ctx<'_>, keep_out: &KeepOut) -> bool {
        keep_out.covers((x, y), reach / 2) || ctx.near_paper_caret((x, y), reach)
    }

    #[cfg(test)]
    #[inline]
    fn of(star: &Star, probe: &GlyphProbe, shade: &PetShade, ctx: &Ctx<'_>) -> Option<Self> {
        Self::of_inner(
            star,
            probe,
            shade,
            None,
            ctx,
            star.class.arms(ctx.geom.ch as f32),
        )
    }

    #[inline]
    fn of_kept(
        star: &Star,
        probe: &GlyphProbe,
        shade: &PetShade,
        keep_out: &KeepOut,
        ctx: &Ctx<'_>,
        arms: (f32, i32, f32, i32, i32),
    ) -> Option<Self> {
        Self::of_inner(star, probe, shade, Some(keep_out), ctx, arms)
    }

    #[inline]
    fn of_inner(
        star: &Star,
        probe: &GlyphProbe,
        shade: &PetShade,
        keep_out: Option<&KeepOut>,
        ctx: &Ctx<'_>,
        (nominal, min_arm, arm_floor, arm_lo, arm_hi): (f32, i32, f32, i32, i32),
    ) -> Option<Self> {
        let rm = ctx.cfg.reduced_motion;
        let age_s = star.age_s(ctx.now);
        let alpha = star.envelope_at_age(age_s, ctx.now, rm);
        if alpha < STAR_CULL_ALPHA {
            return None;
        }
        let (x, y) = if rm {
            star.origin()
        } else {
            star.pos_at_age(age_s)
        };
        if shade.hides((x, y), ctx.geom.cw) {
            return None;
        }
        if let Some(ko) = keep_out {
            let peak_reach = arm_hi.max(arm_lo) + 1;
            if Self::kept_out(x, y, peak_reach, ctx, ko) {
                return None;
            }
        }
        let held = rm || age_s < star.class.envelope_s(star.lane).0;
        let can_glint = !rm && star.gold && star.class == StarClass::M1;
        let (phase, tw) = if !held || can_glint {
            let phase = star.lum_phase();
            (phase, twinkle_env(age_s, phase))
        } else {
            (0.0, 1.0)
        };
        let (env, e) = if held {
            (1.0, 1.0)
        } else {
            (tw, clamp01((tw - 0.55) * (1.0 / 0.45)))
        };
        let floor = star.class.core_floor();
        let mut c = star.class.cov(star.lane) * alpha * clamp01(ctx.cfg.intensity);
        if star.lane.is_transient() && probe.at_px(x, y, ctx.geom) != Some(false) {
            c *= 1.0 / TEXT_SAFE_DIV;
        }
        let arm = if held {
            arm_hi
        } else {
            let s = (std::f32::consts::TAU * star.f * age_s + phase).sin();
            ((nominal * (arm_floor + (1.0 - arm_floor) * s * s)).round() as i32).max(min_arm)
        };
        Some(Self {
            class: star.class,
            lane: star.lane,
            x,
            y,
            arm,
            arm_lo,
            arm_hi,
            c,
            alpha,
            core: if star.class == StarClass::M3 {
                env
            } else {
                floor + (1.0 - floor) * e
            },
            env,
            tint: star.tint,
            glints: can_glint && twinkle_peak(tw),
            gold: star.gold,
        })
    }

    fn draw_add(
        &self,
        ctx: &Ctx<'_>,
        frame: &mut Frame<'_>,
        quad_cap: usize,
        haloed: &mut usize,
        index: &mut SkyIndex,
    ) {
        let geom = ctx.geom;
        index.ingest(frame);
        let d = if self.class == StarClass::M3 {
            1
        } else {
            star_body_px(self.arm) + 1
        };
        let watch_len = if d == 1 { 5 } else { SETTLE_PIXELS };
        let mut watch = [((0i32, 0i32), 0u32); SETTLE_PIXELS];
        for (slot, (dx, dy)) in watch[..watch_len].iter_mut().zip([
            (0, 0),
            (d, 0),
            (-d, 0),
            (0, d),
            (0, -d),
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
        ]) {
            let p = (self.x + dx, self.y + dy);
            *slot = (p, index.ground(ctx.cfg.theme_bg, frame, p.0, p.1));
        }
        let ground = watch[0].1;
        let q0 = frame.out.len();
        let h0 = frame.halos.len();
        match self.class {
            StarClass::M3 => self.draw_m3(geom, frame.out, quad_cap),
            StarClass::M2 => {
                let body = mix_rgb(TINT_WHITE_RGB, self.tint, M2_BODY_TINT);
                let disc = if self.lane.is_sky() {
                    cov_byte(self.c * M2_DISC_ADD)
                } else {
                    0
                };
                let r = self.halo_r(geom.ch as f32);
                let (halo_rgb, halo) = self.halo_request(M2_HALO_SHARE, *haloed);
                let (cov, disc, peak) = price_centre(
                    ground,
                    self.tint == TINT_WHITE_RGB,
                    (body, cov_byte(self.c * self.core)),
                    (self.tint, disc),
                    (halo_rgb, halo),
                );
                push_twinkle_star(
                    frame.out, geom, self.x, self.y, self.arm, cov, false, body, quad_cap,
                );
                if disc > 0 && frame.out.len() < quad_cap {
                    let sh = m2_disc_shoulder_px(geom.ch as f32);
                    push_fx_rect(
                        frame.out,
                        geom,
                        self.x - sh,
                        self.y - sh,
                        2 * sh + 1,
                        2 * sh + 1,
                        premul_rgb(self.tint, disc),
                    );
                }
                self.draw_stubs(geom, frame.out, quad_cap);
                self.draw_halo(geom, frame.halos, r, (halo_rgb, peak), haloed);
            }
            StarClass::M1 => {
                let r = self.halo_r(geom.ch as f32);
                let (halo_rgb, halo) = self.halo_request(M1_HALO_SHARE, *haloed);
                let (cov, _, peak) = price_centre(
                    ground,
                    self.tint == TINT_WHITE_RGB,
                    (TINT_WHITE_RGB, cov_byte(self.c * self.core)),
                    (self.tint, 0),
                    (halo_rgb, halo),
                );
                push_twinkle_star(
                    frame.out,
                    geom,
                    self.x,
                    self.y,
                    self.arm,
                    cov,
                    self.glints,
                    TINT_WHITE_RGB,
                    quad_cap,
                );
                self.draw_stubs(geom, frame.out, quad_cap);
                self.draw_halo(geom, frame.halos, r, (halo_rgb, peak), haloed);
            }
        }
        settle_under_ceiling(frame, q0, h0, &watch[..watch_len]);
    }

    #[inline]
    fn draw_m3(&self, geom: Geom, out: &mut Vec<GlowQuad>, quad_cap: usize) {
        let core = premul_rgb(self.tint, cov_byte(self.c * M3_CENTRE_ADD * self.core));
        let disc = premul_rgb(self.tint, cov_byte(self.c * M3_CROSS_ADD));
        let a = self.arm;
        push_fx_rect(out, geom, self.x, self.y, 1, 1, core);
        for (x, y, w, h) in [
            (self.x - a, self.y, a, 1),
            (self.x + 1, self.y, a, 1),
            (self.x, self.y - a, 1, a),
            (self.x, self.y + 1, 1, a),
        ] {
            if out.len() >= quad_cap {
                return;
            }
            push_fx_rect(out, geom, x, y, w, h, disc);
        }
    }

    #[inline]
    fn draw_stubs(&self, geom: Geom, out: &mut Vec<GlowQuad>, quad_cap: usize) {
        let d = star_body_px(self.arm) + 1;
        let l = stub_len_px(geom.ch as f32);
        let c = premul_rgb(self.tint, cov_byte(self.c * STUB_COV_SHARE));
        for (x, y, w, h) in [
            (self.x - d - (l - 1), self.y, l, 1),
            (self.x + d, self.y, l, 1),
            (self.x, self.y - d - (l - 1), 1, l),
            (self.x, self.y + d, 1, l),
        ] {
            if out.len() >= quad_cap {
                return;
            }
            push_fx_rect(out, geom, x, y, w, h, c);
        }
    }

    #[inline]
    const fn halo_r(&self, ch: f32) -> f32 {
        let nominal = star_arm(ch, self.class.arm_ratio());
        match self.class {
            StarClass::M1 => M1_HALO_R_ARM * nominal,
            StarClass::M2 => (M2_HALO_R_ARM * nominal).max(m2_halo_r_min_px(ch)),
            StarClass::M3 => 0.0,
        }
    }

    #[inline]
    const fn halo_request(&self, share: f32, haloed: usize) -> (u32, u8) {
        if matches!(self.lane, StarLane::Exhaust)
            || haloed >= STARDUST_HALO_BUDGET
            || self.alpha * self.env < CHROMA_CULL_ALPHA
        {
            return (self.tint, 0);
        }
        let cov = cov_byte(self.c * share * self.env);
        if cov == 0 {
            return (self.tint, 0);
        }
        (halo_rgb(self.tint, self.gold, self.env), cov)
    }

    #[inline]
    fn draw_halo(
        &self,
        geom: Geom,
        halos: &mut Vec<RainHalo>,
        r: f32,
        (rgb, peak): (u32, u8),
        haloed: &mut usize,
    ) {
        if peak == 0 {
            return;
        }
        let ri = (r.round() as i32).max(1);
        let before = halos.len();
        push_halo_quads(
            halos,
            geom,
            usize::MAX,
            (self.x, self.y),
            (ri, ri),
            premul_rgb(rgb, peak),
            HaloMode::Add,
        );
        if halos.len() > before {
            *haloed += 1;
        }
    }

    fn draw_over(
        &self,
        geom: Geom,
        cfg: &Config,
        paper_ink: &mut PaperInkMemo,
        frame: &mut Frame<'_>,
        halo_cap: usize,
    ) {
        if frame.halos.len() >= halo_cap {
            return;
        }
        let rgb = paper_ink.get(self.tint, cfg);
        let mass = (self.arm_lo as f32 * STAR_ARM_INK).max(1.0);
        let step = (self.arm - self.arm_lo).max(0);
        match self.class {
            StarClass::M3 => {
                let ink = |cov: u8, cap: f32| {
                    let a = stacked_ink_alpha(cov, InkRole::OverText).min(cap as u32);
                    rgb | (a << 24)
                };
                let r = (mass.round() as i32).max(1);
                push_halo_quads(
                    frame.halos,
                    geom,
                    halo_cap,
                    (self.x, self.y),
                    (r, r),
                    ink(cov_byte(self.c * self.core), LIGHT_ALPHA_CAP),
                    HaloMode::Over,
                );
                if frame.halos.len() + M3_NEEDLE_HALOS > halo_cap {
                    return;
                }
                let n = r + 1 + step;
                let tip = ink(
                    cov_byte(self.c * M3_CROSS_ADD),
                    InkRole::OverText.alpha_cap(),
                );
                for radii in [(n, 1), (1, n)] {
                    push_halo_quads(
                        frame.halos,
                        geom,
                        halo_cap,
                        (self.x, self.y),
                        radii,
                        tip,
                        HaloMode::Over,
                    );
                }
            }
            StarClass::M1 | StarClass::M2 => {
                push_twinkle_over_twin(
                    frame.halos,
                    geom,
                    halo_cap,
                    LightStar {
                        centre: (self.x, self.y),
                        mass,
                        step,
                        rgb,
                        gold: self.glints,
                        cov: cov_byte(self.c * self.core),
                    },
                );
            }
        }
    }
}

const M3_NEEDLE_HALOS: usize = 4;

#[cfg(test)]
#[inline]
const fn arm_trough_px(class: StarClass, ch: f32) -> i32 {
    class.arms(ch).3
}

/// A star's arm at its scintillation's peak (and while held), px: no
/// drawn arm is longer ([`arm_px_with`]).
#[cfg(test)]
#[inline]
const fn arm_peak_px(class: StarClass, ch: f32) -> i32 {
    class.arms(ch).4
}

#[cfg(test)]
#[inline]
fn arm_px_with(star: &Star, ch: f32, held: bool, age_s: f32, phase: f32) -> i32 {
    let (nominal, min, floor, _, arm_hi) = star.class.arms(ch);
    if held {
        return arm_hi;
    }
    let s = (std::f32::consts::TAU * star.f * age_s + phase).sin();
    ((nominal * (floor + (1.0 - floor) * s * s)).round() as i32).max(min)
}

#[cfg(test)]
#[inline]
fn arm_px(star: &Star, ch: f32, held: bool, now: Instant) -> i32 {
    arm_px_with(star, ch, held, star.age_s(now), star.lum_phase())
}

#[inline]
#[must_use]
const fn next_sine_crossing_s(theta: f32, a0: f32, omega: f32) -> Option<f32> {
    use std::f32::consts::PI;
    if !(omega.is_finite() && theta.is_finite() && a0.is_finite()) || omega <= 0.0 {
        return None;
    }
    let rem = theta % PI;
    let base = if rem < 0.0 { rem + PI } else { rem };
    let mut gap = f32::INFINITY;
    let cands = [a0, PI - a0, PI + a0, 2.0 * PI - a0];
    let mut i = 0;
    while i < cands.len() {
        let g = cands[i] - base;
        if g > 1e-5 && g < gap {
            gap = g;
        }
        i += 1;
    }
    if gap.is_finite() {
        Some(gap / omega)
    } else {
        None
    }
}

const SALT_STRIKE: u32 = 0x0000_0101;
const SALT_SECOND_M3: u32 = 0x0000_0202;
const SALT_FIELD: u32 = 0x0000_0404;
const SALT_FIELD_M2: u32 = 0x0000_0505;
const SALT_TINT: u32 = 0x0000_0606;
const SALT_PHASE: u32 = 0x0000_0707;
const SALT_RATE: u32 = 0x0000_0808;
const SALT_JITTER: u32 = 0x0000_0909;
const SALT_ERASE: u32 = 0x0000_0A0A;
const SALT_LIGHT: u32 = 0x0000_0B0B;
const SALT_EXHAUST: u32 = 0x0000_0E7A;

/// The family's avalanche mix (the rounds `cursor_glow` uses).
#[inline]
pub(super) const fn mix32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    h
}

#[inline]
const fn cell_hash(row: u16, col: u16, salt: u32) -> u32 {
    mix32(
        (row as u32).wrapping_mul(73_856_093)
            ^ (col as u32).wrapping_mul(19_349_663)
            ^ salt.wrapping_mul(83_492_791),
    )
}

/// A seed's uniform draw on `[0, 1)`.
#[inline]
pub(super) const fn hash01(h: u32) -> f32 {
    (h >> 8) as f32 * (1.0 / 16_777_216.0)
}

#[inline]
const fn deal(cell: u32, salt: u32, den: u32, scale: f32) -> bool {
    hash01(mix32(cell ^ salt)) < scale / den as f32
}

#[inline]
const fn deal_p(cell: u32, salt: u32, share: f32) -> bool {
    hash01(mix32(cell ^ salt)) < share
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rung {
    Promote,
    White,
    Gold,
}

#[inline]
#[must_use]
const fn ladder_rung(combo: u32) -> Option<Rung> {
    if combo == 0 {
        None
    } else if combo.is_multiple_of(LADDER_GOLD_KEYS) {
        Some(Rung::Gold)
    } else if combo.is_multiple_of(LADDER_WHITE_KEYS) {
        Some(Rung::White)
    } else if combo.is_multiple_of(LADDER_M2_KEYS) {
        Some(Rung::Promote)
    } else {
        None
    }
}

#[inline]
const fn scint_rate(seed: u32) -> f32 {
    SCINT_F_MIN + (SCINT_F_MAX - SCINT_F_MIN) * hash01(mix32(seed ^ SALT_RATE))
}

#[inline]
const fn class_tint(class: StarClass, seed: u32, field_t: f32) -> (u32, bool) {
    if matches!(class, StarClass::M3) {
        (spectrum_snap(field_t), false)
    } else {
        deal_tint(seed, field_t)
    }
}

#[inline]
const fn deal_tint(seed: u32, field_t: f32) -> (u32, bool) {
    let u = hash01(mix32(seed ^ SALT_TINT));
    if u < TINT_FIELD_SHARE {
        (spectrum_snap(field_t), false)
    } else if u < TINT_FIELD_SHARE + TINT_WHITE_SHARE {
        (TINT_WHITE_RGB, false)
    } else {
        (TINT_GOLD_RGB, true)
    }
}

#[inline]
const fn cov_byte(v: f32) -> u8 {
    if v.is_nan() {
        0
    } else {
        (v + 0.5).clamp(0.0, 255.0) as u8
    }
}

#[inline]
const fn px_row(y: f32, geom: Geom) -> i32 {
    ((y - geom.origin_y as f32) / geom.ch as f32).floor() as i32
}

#[inline]
const fn px_col(x: f32, geom: Geom) -> i32 {
    ((x - geom.origin_x as f32) / geom.cw as f32).floor() as i32
}

#[inline]
const fn in_sky_of_row(y: f32, row: u16, geom: Geom) -> bool {
    let d = row as i32 - px_row(y, geom);
    d >= 0 && d <= 1
}

/// Whether a device pixel is inside the effects box (`Geom::fx_*`: the grid plus the `head` band,
/// §5.4's row-0 sky).
#[inline]
pub(super) const fn on_glass(p: (i32, i32), geom: Geom) -> bool {
    p.0 >= geom.fx_left() && p.0 < geom.fx_right() && p.1 >= geom.fx_top() && p.1 < geom.fx_bot()
}

#[inline]
const fn clamp_to_glass(p: (i32, i32), geom: Geom) -> (i32, i32) {
    let (l, r) = (geom.fx_left(), geom.fx_right() - 1);
    let (t, b) = (geom.fx_top(), geom.fx_bot() - 1);
    let x = if p.0 < l {
        l
    } else if p.0 > r {
        r
    } else {
        p.0
    };
    let y = if p.1 < t {
        t
    } else if p.1 > b {
        b
    } else {
        p.1
    };
    (x, y)
}

#[inline]
const fn fan_rise(reach: f32, at_px: (f32, f32), geom: Geom) -> (f32, f32) {
    let squash = reach * FAN_SQUASH;
    let ceiling = FAN_RISE_MAX_CH * geom.ch as f32;
    let room_up = (at_px.1 - geom.fx_top() as f32).max(0.0);
    let room_down = ((geom.fx_bot() - 1) as f32 - at_px.1).max(0.0);
    (
        squash.min(ceiling).min(room_up),
        squash.min(ceiling).min(room_down),
    )
}

#[inline]
const fn fan_ray(a: f32, reach: f32, up: f32, down: f32, j: f32) -> (f32, f32) {
    let (sa, ca) = crate::supernova::sin_cos_f32(a);
    let (dx, dy) = (ca * reach, sa * reach);
    let cap = if dy < 0.0 { up } else { down };
    let cap = cap * (FAN_RISE_JITTER_MIN + (1.0 - FAN_RISE_JITTER_MIN) * j);
    if dy.abs() <= cap {
        return (dx, dy);
    }
    let along = crate::color_math::sqrt_f32((reach * reach - cap * cap).max(0.0));
    (along.copysign(dx), cap.copysign(dy))
}

#[inline]
const fn default_band_top(geom: Geom, cfg: &Config, row: u16) -> f32 {
    let ch = geom.ch as f32;
    let cell_top = geom.origin_y as f32 + row as f32 * ch;
    if cfg.ribbon_tall {
        cell_top + ch - super::ribbon::TALL_UP_CH * ch
    } else {
        cell_top + (SKY_TOP_CH + SKY_BOTTOM_CH) * ch
    }
}

/// One halo: a radial peak split into per-cell-row [`RainHalo`] quads sharing one centre and
/// falloff; `Add` premultiplied for dark, `Over` ink with the centre over-alpha in the high byte
/// for light.
#[inline]
pub(super) fn push_halo_quads(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cap: usize,
    centre: (i32, i32),
    radii: (i32, i32),
    color: u32,
    mode: HaloMode,
) {
    let glass = (
        geom.fx_left(),
        geom.fx_top(),
        geom.fx_right(),
        geom.fx_bot(),
    );
    let spec = HaloSpec {
        centre,
        radii,
        color,
        mode,
    };
    push_halo_quads_clipped(out, geom, cap, spec, glass);
}

#[derive(Clone, Copy, Debug)]
struct HaloSpec {
    centre: (i32, i32),
    radii: (i32, i32),
    color: u32,
    mode: HaloMode,
}

fn push_halo_quads_clipped(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cap: usize,
    spec: HaloSpec,
    clip: (i32, i32, i32, i32),
) {
    let HaloSpec {
        centre,
        radii,
        color,
        mode,
    } = spec;
    if color == 0 {
        return;
    }
    let (bl, bt) = (geom.fx_left().max(clip.0), geom.fx_top().max(clip.1));
    let (br, bb) = (geom.fx_right().min(clip.2), geom.fx_bot().min(clip.3));
    let (rx, ry) = (radii.0.max(1), radii.1.max(1));
    let (cx, cy) = centre;
    let x0 = (cx - rx).max(bl);
    let x1 = (cx + rx).min(br);
    let y0 = (cy - ry).max(bt);
    let y1 = (cy + ry).min(bb);
    if x1 <= x0 || y1 <= y0 || cx < 0 || cy < 0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = i32::from(geom.origin_y);
    let x0_u = x0 as u16;
    let w_u = (x1 - x0) as u16;
    let cx_u = cx as u16;
    let cy_u = cy as u16;
    let rx_u = rx as u16;
    let ry_u = ry as u16;
    let mut yy = y0;
    let mut row = (yy - oy).div_euclid(ch);
    let mut next_band = oy + (row + 1) * ch;
    while yy < y1 {
        if out.len() >= cap {
            return;
        }
        let band_end = next_band.min(y1);
        out.push(RainHalo {
            row: row.max(0) as u16,
            x: x0_u,
            y: yy as u16,
            w: w_u,
            h: (band_end - yy) as u16,
            color,
            cx: cx_u,
            cy: cy_u,
            rx: rx_u,
            ry: ry_u,
            mode,
        });
        yy = band_end;
        row += 1;
        next_band += ch;
    }
}

/// One cell of the aurora: the veil above one laid ribbon cell (§5.9).
#[derive(Clone, Copy, Debug)]
pub struct Veil {
    /// The laid cell's row.
    pub row: u16,
    /// The laid cell's column — the veil's box is this column, and only this column.
    pub col: u16,
    /// The ribbon's field at the cell (C2), unfolded — the bed folds it with `tri` when it draws,
    /// and so does the veil.
    pub t: f32,
    /// The veil's peak, as a share of [`AURORA_COV_PEAK`], priced once at birth from
    /// `Ctx::birth_disp` ([`veil_gain`]).
    pub gain: f32,
    /// The key that laid the cell — the `edge-in` and the horizon read it.
    pub born: Instant,
    /// The last key on the row — the field fade reads it ([`Stardust::relight_field`]).
    pub lit: Instant,
    /// The finish this cell is on, if any: its retract, or the focus ember.
    pub finish: Option<Finish>,
}

impl Veil {
    /// Seconds since the key that laid the cell, saturating at zero.
    #[inline]
    #[must_use]
    pub fn age_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.born).as_secs_f32()
    }

    /// Seconds since the last key on the row, saturating at zero.
    #[inline]
    #[must_use]
    pub fn idle_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.lit).as_secs_f32()
    }

    /// The veil's alpha at `now`: the field lane's envelope ([`field_lane_envelope`]), so it leaves
    /// with the ribbon like the field stars.
    #[inline]
    #[must_use]
    pub fn envelope(&self, now: Instant, reduced_motion: bool) -> f32 {
        let fin = self.finish.map_or(1.0, |f| f.gain(now));
        field_lane_envelope(self.age_s(now), self.idle_s(now), fin, reduced_motion)
    }

    /// True once the cell must leave the pool — the field star's own three exits ([`Star::dead`]).
    #[inline]
    #[must_use]
    pub fn dead(&self, now: Instant) -> bool {
        self.age_s(now) >= FIELD_LIFE_S
            || self.idle_s(now) >= FIELD_FADE_LIFE_S
            || self.finish.is_some_and(|f| f.gain(now) < STAR_CULL_ALPHA)
    }

    /// Put this cell on a finish ending at `at + span_s`, unless one that ends sooner already
    /// stands ([`Star::finish_by`]'s rule).
    #[inline]
    pub fn finish_by(&mut self, at: Instant, span_s: f32) {
        Finish::keep_sooner(&mut self.finish, at, span_s);
    }

    /// The window Y the veil's box ends at (exclusive) and its falloff centres on: the row's sky
    /// band anchor ([`default_band_top`], the ribbon's top under `tall`), never below the glyph
    /// row's top, so under `underline` the veil still sits wholly above the text.
    #[inline]
    #[must_use]
    pub const fn bottom_px(&self, geom: Geom, cfg: &Config) -> f32 {
        let cell_top = geom.origin_y as f32 + self.row as f32 * geom.ch as f32;
        default_band_top(geom, cfg, self.row).min(cell_top)
    }
}

/// The field lane's envelope, shared by the field star ([`Star::envelope`]) and the aurora's veil
/// ([`Veil::envelope`]).
#[inline]
#[must_use]
pub const fn field_lane_envelope(age_s: f32, idle_s: f32, fin: f32, reduced_motion: bool) -> f32 {
    if reduced_motion {
        const INV_FADE_S: f32 = 1.0 / (REDUCED_MOTION_FADE_MS / 1000.0).max(f32::EPSILON);
        return clamp01((FIELD_LIFE_S - age_s) * INV_FADE_S)
            * reduced_fade(FIELD_FADE_LIFE_S - idle_s)
            * fin;
    }
    let birth = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age_s);
    let hand = half_life(idle_s, FIELD_HOLD_MS / 1000.0, FIELD_TAU_MS / 1000.0);
    const INV_FIELD_LIFE_S: f32 = 1.0 / FIELD_LIFE_S.max(f32::EPSILON);
    birth * expiry_melt(age_s * INV_FIELD_LIFE_S) * hand * fin
}

/// The aurora's gain at a birth spine: `0` below [`AURORA_COLD_DISP`], then `smoothstep` up to `1`
/// at a spine of one (0.37 at 0.5, 0.71 at 0.7).
#[inline]
#[must_use]
pub const fn veil_gain(birth_disp: f32) -> f32 {
    if !birth_disp.is_finite() || birth_disp < AURORA_COLD_DISP {
        return 0.0;
    }
    smoothstep01((birth_disp - AURORA_COLD_DISP) * (1.0 / (1.0 - AURORA_COLD_DISP)))
}

#[inline]
#[must_use]
const fn mix_rgb(a: u32, b: u32, k: f32) -> u32 {
    #[inline]
    const fn ch(a: u32, b: u32, k: f32, sh: u32) -> u32 {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        (x + (y - x) * k).round().clamp(0.0, 255.0) as u32
    }
    let k = clamp01(k);
    (ch(a, b, k, 16) << 16) | (ch(a, b, k, 8) << 8) | ch(a, b, k, 0)
}

#[inline]
#[must_use]
const fn halo_rgb(tint: u32, gold: bool, env: f32) -> u32 {
    if gold || tint == TINT_WHITE_RGB {
        return tint;
    }
    let breath = clamp01((env - 0.55) * (1.0 / 0.45)) * 2.0 - 1.0;
    let step = (breath * HALO_HUE_SWING_ENTRIES as f32).round() as i32;
    if step == 0 {
        return tint;
    }
    let mut stop = None;
    let mut i = 0;
    while i < SPECTRUM_ANCHORS.len() {
        if SPECTRUM_ANCHORS[i] == tint {
            stop = Some(i);
            break;
        }
        i += 1;
    }
    let Some(stop) = stop else {
        return tint;
    };
    let raw = SPECTRUM_ANCHOR_AT[stop] as i32 + step;
    let max_idx = SPECTRUM_LUT_LEN as i32 - 1;
    let at = if raw < 0 {
        0
    } else if raw > max_idx {
        max_idx
    } else {
        raw
    };
    SPECTRUM_LUT[at as usize]
}

#[cfg(test)]
thread_local! {
    static GROUND_WORK: std::cell::Cell<[u64; 4]> = const { std::cell::Cell::new([0; 4]) };
}

#[cfg(test)]
fn note_ground_work(slot: usize, n: u64) {
    GROUND_WORK.with(|c| {
        let mut w = c.get();
        w[slot] += n;
        c.set(w);
    });
}

const SKY_INDEX_END: u32 = u32::MAX;

const SKY_INDEX_SLOTS: usize = 1024;

const SKY_INDEX_QUAD_NODES: usize = STARDUST_QUAD_BUDGET * 2;

const SKY_INDEX_HALO_NODES: usize = STARDUST_LIGHT_HALO_BUDGET + 9 * STARDUST_HALO_BUDGET;

const SKY_INDEX_WIDE_CELLS: i64 = 16;

#[derive(Clone, Debug)]
struct SkyIndex {
    origin: (i32, i32),
    cell: (i32, i32),
    from: (usize, usize),
    filed: (usize, usize),
    spilled: bool,
    quad_head: Vec<u32>,
    halo_head: Vec<u32>,
    quad_node: Vec<(u32, u32)>,
    halo_node: Vec<(u32, u32)>,
    dirty: Vec<u32>,
}

impl Default for SkyIndex {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl SkyIndex {
    const EMPTY: Self = Self {
        origin: (0, 0),
        cell: (0, 0),
        from: (0, 0),
        filed: (0, 0),
        spilled: false,
        quad_head: Vec::new(),
        halo_head: Vec::new(),
        quad_node: Vec::new(),
        halo_node: Vec::new(),
        dirty: Vec::new(),
    };

    #[inline]
    fn open(&mut self, geom: Geom, from: (usize, usize)) {
        if self.quad_head.len() == SKY_INDEX_SLOTS {
            for &s in &self.dirty {
                self.quad_head[s as usize] = SKY_INDEX_END;
                self.halo_head[s as usize] = SKY_INDEX_END;
            }
        } else {
            self.quad_head = vec![SKY_INDEX_END; SKY_INDEX_SLOTS];
            self.halo_head = vec![SKY_INDEX_END; SKY_INDEX_SLOTS];
            self.quad_node.reserve_exact(SKY_INDEX_QUAD_NODES);
            self.halo_node.reserve_exact(SKY_INDEX_HALO_NODES);
            self.dirty.reserve_exact(SKY_INDEX_SLOTS);
        }
        self.dirty.clear();
        self.quad_node.clear();
        self.halo_node.clear();
        self.spilled = false;
        self.origin = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        self.cell = (geom.cw as i32, geom.ch as i32);
        self.from = from;
        self.filed = (0, 0);
    }

    #[inline]
    const fn cell_at(&self, x: i32, y: i32) -> (i32, i32) {
        (
            if self.cell.0 > 0 {
                (x - self.origin.0).div_euclid(self.cell.0)
            } else {
                0
            },
            if self.cell.1 > 0 {
                (y - self.origin.1).div_euclid(self.cell.1)
            } else {
                0
            },
        )
    }

    #[inline]
    const fn slot((cx, cy): (i32, i32)) -> usize {
        Self::slot_with_cy(cx, (cy as u32).wrapping_mul(0x85EB_CA6B))
    }

    #[inline]
    const fn slot_with_cy(cx: i32, cy_term: u32) -> usize {
        let k = (cx as u32).wrapping_mul(0x9E37_79B9).wrapping_add(cy_term);
        (k >> 13) as usize & (SKY_INDEX_SLOTS - 1)
    }

    fn file(&mut self, i: usize, (x, y, w, h): (i32, i32, i32, i32), quads: bool) {
        let (cx0, cy0) = self.cell_at(x, y);
        let (cx1, cy1) = self.cell_at(x + w - 1, y + h - 1);
        let cells = i64::from(cx1 - cx0 + 1) * i64::from(cy1 - cy0 + 1);
        let arena = if quads {
            &self.quad_node
        } else {
            &self.halo_node
        };
        if cells > SKY_INDEX_WIDE_CELLS
            || arena.len() + cells as usize > arena.capacity()
            || self.dirty.len() + cells as usize > self.dirty.capacity()
        {
            self.spilled = true;
            return;
        }
        let idx = i as u32;
        for cy in cy0..=cy1 {
            let cy_term = (cy as u32).wrapping_mul(0x85EB_CA6B);
            for cx in cx0..=cx1 {
                let s = Self::slot_with_cy(cx, cy_term);
                if self.quad_head[s] == SKY_INDEX_END && self.halo_head[s] == SKY_INDEX_END {
                    self.dirty.push(s as u32);
                }
                if quads {
                    let next = self.quad_head[s];
                    self.quad_node.push((idx, next));
                    self.quad_head[s] = self.quad_node.len() as u32 - 1;
                } else {
                    let next = self.halo_head[s];
                    self.halo_node.push((idx, next));
                    self.halo_head[s] = self.halo_node.len() as u32 - 1;
                }
            }
        }
    }

    #[inline]
    fn ingest(&mut self, frame: &Frame<'_>) {
        if self.spilled {
            return;
        }
        while self.filed.0 < frame.out.len() - self.from.0 {
            let i = self.filed.0;
            self.filed.0 += 1;
            let q = frame.out[self.from.0 + i];
            if q.alpha != 0 || q.w == 0 || q.h == 0 {
                continue;
            }
            self.file(
                i,
                (
                    i32::from(q.x),
                    i32::from(q.y),
                    i32::from(q.w),
                    i32::from(q.h),
                ),
                true,
            );
        }
        while self.filed.1 < frame.halos.len() - self.from.1 {
            let i = self.filed.1;
            self.filed.1 += 1;
            let h = frame.halos[self.from.1 + i];
            if h.mode != HaloMode::Add || h.w == 0 || h.h == 0 {
                continue;
            }
            self.file(
                i,
                (
                    i32::from(h.x),
                    i32::from(h.y),
                    i32::from(h.w),
                    i32::from(h.h),
                ),
                false,
            );
        }
    }

    #[inline]
    #[must_use]
    fn ground(&self, bg: u32, frame: &Frame<'_>, x: i32, y: i32) -> u32 {
        let laid = &frame.out[self.from.0..];
        let sky = &frame.halos[self.from.1..];
        let s = Self::slot(self.cell_at(x, y));
        let ground = if self.spilled {
            sky_ground(bg, laid, sky, x, y)
        } else {
            sky_ground(
                bg,
                Chain::new(&self.quad_node, self.quad_head[s], laid),
                Chain::new(&self.halo_node, self.halo_head[s], sky),
                x,
                y,
            )
        };
        #[cfg(test)]
        {
            note_ground_work(0, laid.len() as u64);
            note_ground_work(
                1,
                sky.iter().filter(|h| h.mode == HaloMode::Add).count() as u64,
            );
            if self.spilled {
                note_ground_work(2, laid.len() as u64);
                note_ground_work(3, sky.len() as u64);
            } else {
                note_ground_work(
                    2,
                    Chain::new(&self.quad_node, self.quad_head[s], laid).count() as u64,
                );
                note_ground_work(
                    3,
                    Chain::new(&self.halo_node, self.halo_head[s], sky).count() as u64,
                );
            }
            debug_assert_eq!(
                ground,
                sky_ground(bg, laid, sky, x, y),
                "the indexed ground at ({x}, {y}) is not the scan's"
            );
        }
        ground
    }
}

struct Chain<'a, T> {
    nodes: &'a [(u32, u32)],
    at: u32,
    items: &'a [T],
}

impl<'a, T> Chain<'a, T> {
    #[inline]
    const fn new(nodes: &'a [(u32, u32)], head: u32, items: &'a [T]) -> Self {
        Self {
            nodes,
            at: head,
            items,
        }
    }
}

impl<'a, T> Iterator for Chain<'a, T> {
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<&'a T> {
        if self.at == SKY_INDEX_END {
            return None;
        }
        let (entry, next) = self.nodes[self.at as usize];
        self.at = next;
        Some(&self.items[entry as usize])
    }
}

#[inline]
#[must_use]
fn sky_ground<'a>(
    bg: u32,
    laid: impl IntoIterator<Item = &'a GlowQuad>,
    sky: impl IntoIterator<Item = &'a RainHalo>,
    x: i32,
    y: i32,
) -> u32 {
    let mut ground = bg & 0x00FF_FFFF;
    for q in laid {
        if quad_covers(q, x, y) {
            let i = (x - i32::from(q.x)) as u32;
            ground = add_sat(
                ground,
                aterm_render::glow_lerp_rgb(q.color, q.color2, i, u32::from(q.w)),
            );
        }
    }
    for h in sky {
        if h.mode == HaloMode::Add {
            ground = add_sat(ground, halo_add_at(h, x, y));
        }
    }
    ground
}

#[inline]
#[must_use]
const fn halo_weight_at(h: &RainHalo, x: i32, y: i32) -> u8 {
    let (hx, hy) = (h.x as i32, h.y as i32);
    if x < hx || x >= hx + h.w as i32 || y < hy || y >= hy + h.h as i32 {
        return 0;
    }
    let ny = halo_row_ny(y - h.cy as i32, h.ry as i32 * h.ry as i32);
    if ny >= 256 {
        return 0;
    }
    let wt = halo_weight(x - h.cx as i32, ny, h.rx as i32 * h.rx as i32);
    if wt <= 0 {
        return 0;
    }
    if wt > 255 { 255 } else { wt as u8 }
}

#[inline]
#[must_use]
const fn halo_add_at(h: &RainHalo, x: i32, y: i32) -> u32 {
    let wt = halo_weight_at(h, x, y);
    if wt == 0 { 0 } else { premul_rgb(h.color, wt) }
}

#[inline]
#[must_use]
const fn quad_covers(q: &GlowQuad, x: i32, y: i32) -> bool {
    let (qx, qy) = (q.x as i32, q.y as i32);
    q.alpha == 0 && qx <= x && x < qx + q.w as i32 && qy <= y && y < qy + q.h as i32
}

/// The pixels [`settle_under_ceiling`] watches on one star: its centre, the first pixel of each arm
/// past the body (where stub, taper and halo ring meet), and its four orthogonal neighbours.
pub const SETTLE_PIXELS: usize = 9;

fn settle_under_ceiling(frame: &mut Frame<'_>, q0: usize, h0: usize, watch: &[((i32, i32), u32)]) {
    if q0 >= frame.out.len() && h0 >= frame.halos.len() {
        return;
    }
    let table = relative_luminance_table();
    let own_at_max = |(x, y): (i32, i32)| -> (u32, u32, u8) {
        let mut own = 0u32;
        for q in &frame.out[q0..] {
            if quad_covers(q, x, y) {
                own = add_sat(own, q.color & 0x00FF_FFFF);
            }
        }
        let mut halo_color = 0u32;
        let mut halo_wt = 0u8;
        for h in &frame.halos[h0..] {
            let wt = halo_weight_at(h, x, y);
            if wt > 0 {
                own = add_sat(own, premul_rgb(h.color, wt));
                halo_color = h.color;
                halo_wt = wt;
                break;
            }
        }
        (own, halo_color, halo_wt)
    };
    let mut over = [((0i32, 0i32), 0u32, 0u32, 0u8); SETTLE_PIXELS];
    let mut over_len = 0usize;
    for &(p, ground) in watch {
        let (own, halo_color, halo_wt) = own_at_max(p);
        if !under_ceil_with(table, add_sat(ground, own))
            && let Some(slot) = over.get_mut(over_len)
        {
            *slot = (p, ground, halo_color, halo_wt);
            over_len += 1;
        }
    }
    if over_len == 0 {
        return;
    }
    let over = &over[..over_len];
    let own_at = |k: u8, (x, y): (i32, i32), halo_color: u32, halo_wt: u8| -> u32 {
        let mut own = 0u32;
        for q in &frame.out[q0..] {
            if quad_covers(q, x, y) {
                own = add_sat(own, premul_rgb(q.color, k));
            }
        }
        if halo_wt > 0 {
            own = add_sat(own, premul_rgb(premul_rgb(halo_color, k), halo_wt));
        }
        own
    };
    let fits = |k: u8| {
        over.iter().all(|&(p, ground, halo_color, halo_wt)| {
            under_ceil_with(table, add_sat(ground, own_at(k, p, halo_color, halo_wt)))
        })
    };
    let k = fit_byte(u8::MAX - 1, fits);
    for q in &mut frame.out[q0..] {
        q.color = premul_rgb(q.color, k);
        q.color2 = premul_rgb(q.color2, k);
    }
    for h in &mut frame.halos[h0..] {
        h.color = premul_rgb(h.color, k);
    }
}

#[inline]
#[must_use]
const fn under_ceil_with(table: &RelativeLuminanceTable, px: u32) -> bool {
    relative_luminance_with(table, px) <= STAR_LIGHT_CEIL
}

#[inline]
#[must_use]
const fn stacked_centre(base: u32, rgb: u32, cov: u8) -> u32 {
    let lay = premul_rgb(rgb, cov);
    let core = premul_rgb(rgb, (cov as f32 * STAR_CORE_ADD) as u8);
    add_sat(add_sat(add_sat(base, lay), lay), core)
}

#[inline]
#[must_use]
fn fit_byte(want: u8, under: impl Fn(u8) -> bool) -> u8 {
    if want == 0 || under(want) {
        return want;
    }
    if !under(0) {
        return 0;
    }
    let (mut lo, mut hi) = (0u8, want);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if under(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

#[inline]
#[must_use]
fn price_centre(
    ground: u32,
    achromatic: bool,
    body: (u32, u8),
    disc: (u32, u8),
    halo: (u32, u8),
) -> (u8, u8, u8) {
    let table = relative_luminance_table();
    let lay = |base: u32, rgb: u32, k: u8| {
        if k == 0 {
            base
        } else {
            add_sat(base, premul_rgb(rgb, k))
        }
    };
    if achromatic {
        let c0 = stacked_centre(ground, body.0, body.1);
        let c1 = lay(c0, disc.0, disc.1);
        let c2 = lay(c1, halo.0, halo.1);
        if under_ceil_with(table, c2) {
            return (body.1, disc.1, halo.1);
        }
        if !under_ceil_with(table, ground) {
            return (body.1, disc.1, halo.1);
        }
        let cov = if under_ceil_with(table, c0) {
            body.1
        } else {
            fit_byte(body.1, |k| {
                under_ceil_with(table, stacked_centre(ground, body.0, k))
            })
        };
        let centre = if cov == body.1 {
            c0
        } else {
            stacked_centre(ground, body.0, cov)
        };
        let d = fit_byte(disc.1, |k| under_ceil_with(table, lay(centre, disc.0, k)));
        let centre = lay(centre, disc.0, d);
        let peak = fit_byte(halo.1, |k| under_ceil_with(table, lay(centre, halo.0, k)));
        (cov, d, peak)
    } else {
        let c0 = lay(ground, halo.0, halo.1);
        let c1 = lay(c0, disc.0, disc.1);
        let c2 = stacked_centre(c1, body.0, body.1);
        if under_ceil_with(table, c2) {
            return (body.1, disc.1, halo.1);
        }
        if !under_ceil_with(table, ground) {
            return (body.1, disc.1, halo.1);
        }
        let peak = if under_ceil_with(table, c0) {
            halo.1
        } else {
            fit_byte(halo.1, |k| under_ceil_with(table, lay(ground, halo.0, k)))
        };
        let lit = if peak == halo.1 {
            c0
        } else {
            lay(ground, halo.0, peak)
        };
        let d = if peak == halo.1 && under_ceil_with(table, c1) {
            disc.1
        } else {
            fit_byte(disc.1, |k| under_ceil_with(table, lay(lit, disc.0, k)))
        };
        let lit = if peak == halo.1 && d == disc.1 {
            c1
        } else {
            lay(lit, disc.0, d)
        };
        let cov = fit_byte(body.1, |k| {
            under_ceil_with(table, stacked_centre(lit, body.0, k))
        });
        (cov, d, peak)
    }
}

/// One light-theme star, as [`push_twinkle_over_twin`] takes it: `push_twinkle_star`'s parameter
/// set minus the quad budget.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LightStar {
    pub(crate) centre: (i32, i32),
    pub(crate) mass: f32,
    pub(crate) step: i32,
    pub(crate) rgb: u32,
    pub(crate) gold: bool,
    pub(crate) cov: u8,
}

#[derive(Clone, Copy, Debug)]
struct PaperInkMemo {
    theme: (u32, u32),
    len: u8,
    entries: [(u32, u32); 9],
}

impl Default for PaperInkMemo {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl PaperInkMemo {
    const EMPTY: Self = Self {
        theme: (0, 0),
        len: 0,
        entries: [(0, 0); 9],
    };

    #[inline]
    const fn get(&mut self, tint: u32, cfg: &Config) -> u32 {
        let theme = (cfg.theme_fg & 0x00FF_FFFF, cfg.theme_bg & 0x00FF_FFFF);
        if self.theme.0 != theme.0 || self.theme.1 != theme.1 {
            self.theme = theme;
            self.len = 0;
        }
        let len = self.len as usize;
        let mut i = 0;
        while i < len {
            let (t, rgb) = self.entries[i];
            if t == tint {
                return rgb;
            }
            i += 1;
        }
        let rgb = star_ink_on_paper(tint, cfg);
        if len < self.entries.len() {
            self.entries[len] = (tint, rgb);
            self.len = (len + 1) as u8;
        }
        rgb
    }
}

#[inline]
const fn star_ink_on_paper(tint: u32, cfg: &Config) -> u32 {
    let (r, g, b) = ((tint >> 16) & 0xff, (tint >> 8) & 0xff, tint & 0xff);
    if r == g && g == b {
        cfg.theme_fg & 0x00FF_FFFF
    } else {
        paper_mark_ink(tint, cfg.theme_bg)
    }
}

/// The light-theme star (§3.3, §5.2): a horizontal and a vertical arm (each a full-length tip lay
/// and a shorter body lay) on a round nucleus, plus four diagonal glint dots when gold, all
/// source-over falloff ellipses in the star's ink on the page.
pub(crate) fn push_twinkle_over_twin(
    halos: &mut Vec<RainHalo>,
    geom: Geom,
    cap: usize,
    star: LightStar,
) {
    let LightStar {
        centre,
        mass,
        step,
        rgb,
        gold,
        cov,
    } = star;
    if cov == 0 || mass < 0.75 {
        return;
    }
    const LAY_ALPHA: [u32; 256] = {
        let mut lut = [0u32; 256];
        let mut c = 1usize;
        while c < 256 {
            let cap_a = c as u32;
            if c == 255 {
                lut[c] = 255u32 << 24;
            } else {
                let a = (c as f32 / 255.0) as f64;
                let x = 1.0 - a;
                let x3 = x * x * x;
                let exp = (STAR_OVER_CENTRE_LAYS / STAR_OVER_LAYS as f32) as f64;
                let mut y = 1.0 - exp * a;
                let mut step = 0;
                while step < 20 {
                    let y2 = y * y;
                    let y4 = y2 * y2;
                    y = (4.0 * y + x3 / y4) / (STAR_OVER_LAYS as f64);
                    step += 1;
                }
                let lay = (1.0 - y as f32) * 255.0;
                let v = lay.round() as u32;
                let clamped = if v < 1 {
                    1
                } else if v > cap_a {
                    cap_a
                } else {
                    v
                };
                lut[c] = clamped << 24;
            }
            c += 1;
        }
        lut
    };
    let lay_alpha = &LAY_ALPHA;
    let lay_of = |cap_a: u32| -> u32 { (rgb & 0x00FF_FFFF) | lay_alpha[cap_a as usize] };
    let core_cap = u32::from(cov).clamp(1, InkRole::Leading.alpha_cap() as u32);
    let arm_cap = u32::from(cov).clamp(1, InkRole::OverText.alpha_cap() as u32);
    let core_ink = lay_of(core_cap);
    let arm_ink = if arm_cap == core_cap {
        core_ink
    } else {
        lay_of(arm_cap)
    };
    let px = |v: f32| (v.round() as i32).max(1);
    let waist = px((mass * STAR_WAIST).max(1.0));
    let body = px((mass * STAR_TAPER_BODY).max(1.0));
    let core = px((mass * STAR_CORE).max(1.0));
    let arm_px = px(mass) + step;
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (core, core),
        core_ink,
        HaloMode::Over,
    );
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (arm_px, waist),
        arm_ink,
        HaloMode::Over,
    );
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (waist, arm_px),
        arm_ink,
        HaloMode::Over,
    );
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (body, waist),
        arm_ink,
        HaloMode::Over,
    );
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (waist, body),
        arm_ink,
        HaloMode::Over,
    );
    if gold {
        let d = px(mass * STAR_GLINT);
        let gcap = ((f32::from(cov) * STAR_GLINT_COV) as u32)
            .clamp(1, InkRole::OverText.alpha_cap() as u32);
        let glint = (rgb & 0x00FF_FFFF) | (gcap << 24);
        for (ox, oy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            push_halo_quads(
                halos,
                geom,
                cap,
                (centre.0 + ox, centre.1 + oy),
                (1, 1),
                glint,
                HaloMode::Over,
            );
        }
    }
}

#[inline]
const fn stacked_ink_alpha(cov: u8, role: InkRole) -> u32 {
    const CENTRE_ALPHA: [f32; 256] = {
        let lays = STAR_OVER_CENTRE_LAYS as usize;
        let mut lut = [0.0f32; 256];
        let mut b = 0usize;
        while b < 256 {
            let a = b as f32 / 255.0;
            let rem = 1.0 - a;
            let mut p = 1.0f32;
            let mut k = 0;
            while k < lays {
                p *= rem;
                k += 1;
            }
            lut[b] = (1.0 - p) * 255.0;
            b += 1;
        }
        lut
    };
    let cap_b = role.alpha_cap() as u8;
    let ceiling = CENTRE_ALPHA[cap_b as usize];
    let idx = if cov < cap_b { cov } else { cap_b } as usize;
    CENTRE_ALPHA[idx].clamp(1.0, ceiling) as u32
}

#[cfg(test)]
mod tests {
    use super::super::Flow;
    use super::super::spine::FLOW_ENTRY_KEYS;
    use super::*;
    use crate::rainbow_kitty::Mend;
    use crate::rainbow_kitty::ribbon::RETRACT_START_S;
    use aterm_render::add_sat;
    use std::time::Duration;

    const fn geom() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 40,
            win_w: 1080,
            win_h: 760,
            head: 40,
        }
    }

    const fn cfg(dark: bool) -> Config {
        Config {
            dark_theme: dark,
            intensity: 1.0,
            duration: Duration::from_millis(900),
            ribbon_tall: true,
            ribbon_flat: false,
            theme_fg: if dark { 0x00FF_FFFF } else { 0x0016_161C },
            theme_bg: if dark { 0x0000_0000 } else { 0x00FF_FFFF },
            reduced_motion: false,
        }
    }

    const fn geom_cell(cw: usize, ch: usize) -> Geom {
        Geom {
            cw,
            ch,
            win_w: (120 * cw) as u16,
            win_h: (40 + 40 * ch) as u16,
            ..geom()
        }
    }

    const fn ctx(now: Instant, cfg: &Config, caret: (u16, u16)) -> Ctx<'_> {
        ctx_in(geom(), now, cfg, caret)
    }

    const fn ctx_in(geom: Geom, now: Instant, cfg: &Config, caret: (u16, u16)) -> Ctx<'_> {
        Ctx {
            now,
            geom,
            cfg,
            disp: 0.4,
            birth_disp: 0.4,
            phase: 0.0,
            caret,
            caret_t: 0.5,
            caret_walk: None,
            mend: None,
            surge: 0.0,
            flow: Flow::EMPTY,
        }
    }

    #[derive(Default)]
    struct Scratch {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<aterm_render::BeamVertex>,
        cues: Vec<SoundCue>,
    }

    impl Scratch {
        const fn frame(&mut self) -> Frame<'_> {
            Frame {
                under: &mut self.under,
                out: &mut self.out,
                halos: &mut self.halos,
                beams: &mut self.beams,
                cues: &mut self.cues,
                caret: super::super::CaretSeam::EMPTY,
                companion: None,
                fp: 0,
            }
        }
    }

    fn px(quads: &[GlowQuad], x: i32, y: i32) -> u32 {
        let mut dst = 0u32;
        for q in quads {
            let inside = x >= i32::from(q.x)
                && x < i32::from(q.x) + i32::from(q.w)
                && y >= i32::from(q.y)
                && y < i32::from(q.y) + i32::from(q.h);
            if !inside {
                continue;
            }
            if q.alpha == 0 {
                dst = add_sat(dst, q.color);
            } else {
                let a = u32::from(q.alpha);
                let ch = |sh: u32| {
                    let d = (dst >> sh) & 0xff;
                    (((q.color >> sh) & 0xff) + d * (255 - a) / 255).min(255)
                };
                dst = (ch(16) << 16) | (ch(8) << 8) | ch(0);
            }
        }
        dst
    }

    const fn lum(p: u32) -> u32 {
        let r = (p >> 16) & 0xff;
        let g = (p >> 8) & 0xff;
        let b = p & 0xff;
        let rg = if r >= g { r } else { g };
        if rg >= b { rg } else { b }
    }

    fn peak_seed() -> u32 {
        (0u32..)
            .find(|&k| {
                (hash01(mix32(k ^ SALT_PHASE)) * std::f32::consts::TAU)
                    .sin()
                    .abs()
                    > 0.9995
            })
            .expect("some seed puts the luminance clock at its peak")
    }

    fn star(class: StarClass, lane: StarLane, born: Instant, tint: u32) -> Star {
        Star {
            class,
            lane,
            x: 200.0,
            y: 100.0,
            home_row: None,
            born,
            lit: born,
            seed: peak_seed(),
            tint,
            gold: tint == TINT_GOLD_RGB,
            v0: (0.0, 0.0),
            f: 10.0,
            stream: 0.0,
            pull: 0.0,
            finish: None,
        }
    }

    fn sky() -> Stardust {
        let mut sky = Stardust::new();
        for row in 2..=4 {
            sky.probe_mut().probe_row(row, &[false; 120]);
        }
        sky
    }

    fn frame_at(sky: &mut Stardust, now: Instant, cfg: &Config) -> Scratch {
        frame_in(sky, geom(), now, cfg)
    }

    fn frame_in(sky: &mut Stardust, g: Geom, now: Instant, cfg: &Config) -> Scratch {
        let mut sc = Scratch::default();
        let mut f = sc.frame();
        sky.emit(&ctx_in(g, now, cfg, (3, 10)), &mut f);
        sc
    }

    /// A star whose centre lies on the pet's body, or on the box the body's last move carries it
    /// to, is not drawn on dark or on paper; one beside the body is. The negative control notes no
    /// body and draws it.
    #[test]
    fn no_star_is_drawn_on_the_pet_s_body() {
        let t0 = Instant::now();
        // The star sits at (200, 100); the body is 70 px wide and 44 tall.
        let body = |x0: i32| Some((x0, x0 + 70, 70, 114));
        for dark in [true, false] {
            let c = cfg(dark);
            let drawn = |boxes: &[Option<(i32, i32, i32, i32)>]| {
                let mut sky = sky();
                sky.sow(star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB));
                for &b in boxes {
                    sky.shade.note(b);
                }
                let sc = frame_at(&mut sky, t0 + Duration::from_millis(40), &c);
                sc.out.len() + sc.halos.len()
            };
            assert!(drawn(&[]) > 0, "dark={dark}: the control draws the star");
            assert!(
                drawn(&[None]) > 0,
                "dark={dark}: a pet off glass shades nothing"
            );
            assert_eq!(
                drawn(&[body(170)]),
                0,
                "dark={dark}: the star is on the body"
            );
            assert!(
                drawn(&[body(230)]) > 0,
                "dark={dark}: a star beside the body is drawn"
            );
            assert_eq!(
                drawn(&[body(60), body(115)]),
                0,
                "dark={dark}: the body moving onto the star shades it a frame early"
            );
            assert!(
                drawn(&[body(170), None]) > 0,
                "dark={dark}: the body gone, the star is back"
            );
        }
    }

    const fn every_class() -> [(StarClass, StarLane, f32); 6] {
        [
            (StarClass::M1, StarLane::Strike, 143.0),
            (StarClass::M2, StarLane::Strike, 130.0),
            (StarClass::M3, StarLane::Strike, 98.0),
            (StarClass::M1, StarLane::Fan, 118.0),
            (StarClass::M2, StarLane::Fan, 80.0),
            (StarClass::M3, StarLane::Fan, 61.0),
        ]
    }

    const fn typed(class: TypedClass) -> Event {
        Event::Typed {
            cells: 1,
            shifted: matches!(class, TypedClass::Capital),
            class,
        }
    }

    const fn col_of(s: &Star) -> i32 {
        px_col(s.x, geom())
    }

    fn key_at(t0: Instant, k: u16) -> Instant {
        t0 + Duration::from_micros(u64::from(k) * 83_333)
    }

    fn disp_at_12cps(k: u16) -> f32 {
        let t = f32::from(k) / 12.0;
        (1.3 * (1.0 - (-t / 2.0).exp())).min(1.0)
    }

    fn chroma(p: u32) -> u32 {
        let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
        r.max(g).max(b) - r.min(g).min(b)
    }

    #[test]
    fn a_grain_opens_and_closes_at_least_twice_in_its_life() {
        let t0 = Instant::now();
        for dark in [true, false] {
            let c = cfg(dark);
            let g = if dark { geom() } else { geom_cell(15, 28) };
            let mut sky = sky();
            sky.sow(star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB));
            let life_ms = (StarClass::M3.life_s(StarLane::Strike) * 1000.0) as u64;
            let mut widths = Vec::new();
            for ms in 0..life_ms {
                let sc = frame_in(&mut sky, g, t0 + Duration::from_millis(ms), &c);
                let w = if dark {
                    sc.out.iter().map(|q| q.w).max().unwrap_or(0)
                } else {
                    let row = composite_over(&sc, PAPER, 180, 100, 40, 1);
                    let inked: Vec<_> = (0..40u16)
                        .filter(|&x| row[usize::from(x)] != PAPER)
                        .collect();
                    inked.last().map_or(0, |last| last - inked[0] + 1)
                };
                if widths.last() != Some(&w) {
                    widths.push(w);
                }
            }
            assert!(
                widths.len() >= 5,
                "the {} grain changed size {} times in {life_ms} ms: {widths:?}",
                if dark { "dark" } else { "light" },
                widths.len() - 1
            );
            let widest = i32::from(widths.iter().copied().max().unwrap_or(0));
            let at_2x = if dark { widest * 2 } else { widest };
            assert!(
                at_2x <= SCINT_BBOX_MAX_PX,
                "the {} grain's extent {at_2x} px at 2x is outside the size-scintillation \
                 exemption",
                if dark { "dark" } else { "light" }
            );
        }
    }

    #[test]
    fn a_light_rule_over_the_typing_row_keeps_the_sky_alive() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let ch = g.ch as f32;
        let type_under = |row_4: &[CellInk]| -> Stardust {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row_ink(4, row_4);
            sky.probe_mut().probe_row(5, &[false; 120]);
            for k in 0..40u16 {
                sky.on_event(
                    &typed(TypedClass::Glyph),
                    t0,
                    &ctx(t0, &c, (5, 10 + k)),
                    &ribbon,
                );
            }
            sky
        };
        let row_of = |s: &str| -> Vec<CellInk> { s.repeat(120).chars().map(cell_ink).collect() };

        let rule = row_of("─");
        assert!(rule.iter().all(|&cell| cell == CellInk::Rule));
        let sky = type_under(&rule);
        assert!(
            sky.live() > 0,
            "a light rule above the input line darkened the sky"
        );
        for s in sky.live_iter() {
            assert_eq!(
                px_row(s.y, g),
                4,
                "a sky star was born outside row − 1, at ({}, {})",
                s.x,
                s.y
            );
            let off = s.y as i32 - i32::from(g.origin_y) - 4 * g.ch as i32;
            assert!(
                off >= rule_stroke_px(g.ch).end,
                "a star core sits in the rule's stroke band: pixel {off} of row 4, {:.2} ch",
                off as f32 / ch
            );
            assert_eq!(
                sky.probe().at_px(s.x as i32, s.y as i32, g),
                Some(false),
                "the pixel probe called a sky pixel over the rule inked at ({}, {})",
                s.x,
                s.y
            );
        }
        let blank = type_under(&row_of(" "));
        assert_eq!(
            sky.live(),
            blank.live(),
            "the sky over a light rule is not the sky over a blank row"
        );

        assert_eq!(
            type_under(&row_of("x")).live(),
            0,
            "a row of text above the input line minted stars"
        );

        assert_eq!(
            type_under(&row_of("━")).live(),
            0,
            "a heavy rule above the input line minted stars"
        );
    }

    #[test]
    fn the_pixel_probe_answers_ink_only_inside_a_rules_stroke_band() {
        let g = geom();
        let mut probe = GlyphProbe::new();
        let mut row = vec![CellInk::Blank; 120];
        row[10] = CellInk::Rule;
        row[11] = CellInk::Glyph;
        probe.probe_row_ink(4, &row);
        let y_at = |f: f32| (f32::from(g.origin_y) + (4.0 + f) * g.ch as f32) as i32;
        let (rx, _) = g.cell_center(4, 10);
        let (gx, _) = g.cell_center(4, 11);
        let (bx, _) = g.cell_center(4, 12);

        assert_eq!(probe.at(4, 10, g.rows), Some(true), "the rule cell has ink");
        assert_eq!(
            probe.sky_at(4, 10, g.rows),
            Some(false),
            "the rule's sky is clear"
        );

        assert_eq!(probe.at(4, 11, g.rows), Some(true));
        assert_eq!(
            probe.sky_at(4, 11, g.rows),
            Some(true),
            "a glyph darkens the sky"
        );
        for f in [0.10, 0.50, 0.80] {
            assert_eq!(
                probe.at_px(gx as i32, y_at(f), g),
                Some(true),
                "a glyph is ink at {f}"
            );
        }

        assert_eq!(probe.at(4, 12, g.rows), Some(false));
        assert_eq!(probe.sky_at(4, 12, g.rows), Some(false));
        assert_eq!(probe.at_px(bx as i32, y_at(0.50), g), Some(false));

        assert_eq!(probe.at(3, 10, g.rows), None, "an unprobed row is unknown");
        assert_eq!(probe.sky_at(3, 10, g.rows), None);
        assert_eq!(probe.at_px(rx as i32, y_at(-0.50), g), None);

        let mut bools = GlyphProbe::new();
        let mut occupied = [false; 120];
        occupied[11] = true;
        bools.probe_row(4, &occupied);
        for col in [10u16, 11, 12] {
            assert_eq!(bools.at(4, col, g.rows), Some(col == 11));
            assert_eq!(bools.sky_at(4, col, g.rows), Some(col == 11));
        }
        probe.probe_row(4, &occupied);
        assert_eq!(probe.at(4, 10, g.rows), Some(false));
        assert_eq!(probe.at_px(rx as i32, y_at(0.50), g), Some(false));
    }

    #[test]
    fn the_stroke_band_is_one_set_of_pixels_for_the_probe_the_zones_and_the_headroom() {
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let tall = cfg(true);
        let mut under = cfg(true);
        under.ribbon_tall = false;
        for ch in 6usize..=48 {
            let cw = (ch / 2).max(3);
            let g = geom_cell(cw, ch);
            let band = rule_stroke_px(ch);
            let row_top = |r: i32| i32::from(g.origin_y) + r * ch as i32;
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row_ink(4, &[CellInk::Rule; 120]);
            sky.probe_mut().probe_row_ink(5, &[CellInk::Rule; 120]);
            let (x, _) = g.cell_center(4, 30);
            let x = x as i32;
            assert!(
                band.start < band.end && band.end <= ch as i32,
                "ch {ch}: the band {band:?} is empty or past the cell"
            );
            for r in [4, 5] {
                for off in 0..ch as i32 {
                    assert_eq!(
                        sky.probe().at_px(x, row_top(r) + off, g),
                        Some(band.contains(&off)),
                        "ch {ch}: pixel {off} of row {r} against the band {band:?}"
                    );
                }
            }
            if ch >= 7 {
                for w in [cw, ch] {
                    let light = ((w.min(ch) + 4) / 8).max(1) as i32;
                    let s = (ch as i32 - light) / 2;
                    assert!(
                        band.start <= s && s + light <= band.end,
                        "ch {ch} cw {w}: the renderer's stroke {s}..{} is not inside the band \
                         {band:?}",
                        s + light
                    );
                }
            }
            // Both anchors' skies are in the row above's foot (`sky_birth_px`).
            for (c, row, side) in [(&tall, 4, "tall"), (&under, 4, "underline")] {
                let cx = ctx_in(g, t0, c, (5, 30));
                let mut births = 0usize;
                for seed in 0..256u32 {
                    let Some((bx, by)) = sky.sky_birth_px((5, 30), mix32(seed), &cx, &ribbon)
                    else {
                        continue;
                    };
                    births += 1;
                    let off = by as i32 - row_top(row);
                    assert!(
                        (0..ch as i32).contains(&off) && !band.contains(&off),
                        "ch {ch} {side}: a birth at pixel {off} of row {row}, in the band {band:?}"
                    );
                    assert_eq!(
                        sky.probe().at_px(bx as i32, by as i32, g),
                        Some(false),
                        "ch {ch} {side}: a birth at ({bx}, {by}) is on ink by the probe"
                    );
                }
                assert!(births > 0, "ch {ch} {side}: no birth under a rule row");
            }
            for off in 0..ch as i32 {
                let y = (row_top(4) + off) as f32;
                let room = sky.rule_headroom_px(30..=30, y, g);
                if off < band.start {
                    assert_eq!(
                        room,
                        f32::INFINITY,
                        "ch {ch}: a flight born at pixel {off}, above the band, is clamped"
                    );
                    continue;
                }
                assert_eq!(
                    room,
                    (off - band.end).max(0) as f32,
                    "ch {ch}: the headroom at pixel {off} is not the pixels to the band's end \
                     {}",
                    band.end
                );
                if band.contains(&off) {
                    continue;
                }
                let held = y as i32 - room as i32;
                assert_eq!(
                    sky.probe().at_px(x, held, g),
                    Some(false),
                    "ch {ch}: the pixel a flight born at {off} is held at is on ink"
                );
                assert_eq!(
                    sky.probe().at_px(x, held - 1, g),
                    Some(true),
                    "ch {ch}: a flight born at {off} is held a pixel short of the stroke"
                );
            }
        }
    }

    #[test]
    fn the_headroom_under_a_rule_reads_every_column_the_flight_crosses() {
        let g = geom_cell(15, 28);
        let band = rule_stroke_px(g.ch);
        let row_top = i32::from(g.origin_y) + 4 * g.ch as i32;
        let mut sky = Stardust::new();
        let mut title = [CellInk::Blank; 120];
        title[..40].fill(CellInk::Rule);
        sky.probe_mut().probe_row_ink(4, &title);
        sky.probe_mut().probe_row(5, &[false; 120]);
        let y = (row_top + band.end) as f32;
        assert_eq!(
            sky.rule_headroom_px(39..=39, y, g),
            0.0,
            "under the rule's last cell"
        );
        assert_eq!(
            sky.rule_headroom_px(40..=40, y, g),
            f32::INFINITY,
            "under the first blank cell"
        );
        assert_eq!(
            sky.rule_headroom_px(40..=46, y, g),
            f32::INFINITY,
            "a flight over blank cells only"
        );
        assert_eq!(
            sky.rule_headroom_px(39..=46, y, g),
            0.0,
            "one rule cell at the flight's far end is the flight's headroom"
        );
        assert_eq!(
            sky.rule_headroom_px(30..=46, y + 2.0, g),
            2.0,
            "two pixels under the band's end, over a run of rule cells"
        );
        assert_eq!(
            sky.rule_headroom_px(-3..=-1, y, g),
            f32::INFINITY,
            "columns off the grid's left edge bound nothing"
        );
        assert_eq!(
            sky.rule_headroom_px(-3..=0, y, g),
            0.0,
            "…and the first on-grid rule cell beside them does"
        );
        let mut glyphs = Stardust::new();
        glyphs.probe_mut().probe_row(4, &[true; 120]);
        assert_eq!(
            glyphs.rule_headroom_px(0..=46, y, g),
            f32::INFINITY,
            "a row of glyphs has no stroke band: the cell-level gate refuses it, not the headroom"
        );
    }

    #[test]
    fn cell_ink_classifies_the_light_horizontal_rules_and_nothing_else() {
        for ch in ['─', '┄', '┈', '╌', '╴', '╶'] {
            assert_eq!(cell_ink(ch), CellInk::Rule, "{ch:?} (U+{:04X})", ch as u32);
        }
        for ch in [
            '━', '┅', '┉', '╍', '╸', '╺', '╼', '╾', '═', '│', '┃', '┼', '╭', '╮', '▁', '▔', '█',
            '-', '_', '—', '=', '~', 'a', 'A', '0', '\0', '\u{2588}',
        ] {
            assert_eq!(cell_ink(ch), CellInk::Glyph, "{ch:?} (U+{:04X})", ch as u32);
        }
        assert_eq!(cell_ink(' '), CellInk::Blank);
    }

    #[test]
    fn no_star_core_is_ever_born_over_a_probed_glyph() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();

        let mut sky = Stardust::new();
        for k in 0..40u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        assert_eq!(sky.live(), 0, "an unprobed sky band minted stars");

        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[true; 120]);
        for k in 0..40u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        assert_eq!(sky.live(), 0, "an inked sky band minted stars");

        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        for k in 0..40u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        assert!(sky.live() > 0, "a provably blank sky band minted nothing");

        let mut sky = Stardust::new();
        let mut row6 = [true; 120];
        row6[41] = false;
        sky.probe_mut().probe_row(6, &row6);
        let (ix, iy) = geom().cell_center(6, 40);
        sky.sow_mini_fan(t0, (ix, iy), 3, &ctx(t0, &c, (6, 40)));
        assert_eq!(sky.live(), 3, "a hop onto a glyph minted no mini-fan");
        for s in sky.live_iter() {
            assert_eq!(
                (s.x, s.y),
                (ix.round(), iy.round()),
                "a mini-fan star was not born on the landing pixel"
            );
            let (rx, ry) = s.rest();
            assert_eq!(
                (px_row(ry as f32, geom()), px_col(rx as f32, geom())),
                (6, 40),
                "a mini-fan core came to rest off the landing cell, at ({rx}, {ry})"
            );
        }
    }

    #[test]
    fn a_thrown_core_is_cleared_where_it_rests_nudged_walked_back_or_dropped() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom();
        let mut sky = Stardust::new();
        let mut row6 = [true; 120];
        row6[40] = false;
        sky.probe_mut().probe_row(6, &row6);
        let cx = ctx(t0, &c, (6, 40));
        let window = FAN_THROW_MS;
        let thrown = |dx: i32, dy: i32| {
            let mut s = star(StarClass::M2, StarLane::Fan, t0, TINT_WHITE_RGB);
            s.x = 365.0;
            s.y = 157.0;
            s.v0 = (dx as f32 / window, dy as f32 / window);
            assert_eq!(s.rest(), (365 + dx, 157 + dy), "the fixture's own rest");
            s
        };
        let done = t0 + Duration::from_secs_f32(window / 1000.0) + Duration::from_millis(1);

        let mut s = thrown(7, 12);
        assert!(sky.settle(&mut s, false, &cx));
        assert_eq!(s.rest(), (372, 169), "a clear rest was moved");

        let mut s = thrown(7, 8);
        assert!(sky.settle(&mut s, false, &cx));
        assert_eq!((s.x, s.y), (365.0, 157.0), "a nudge moved the birth");
        assert_eq!(
            s.rest(),
            (372, 166),
            "the rest was not nudged 1 px into the air"
        );
        assert_eq!(
            s.pos(done, false),
            s.rest(),
            "the throw does not end on its rest"
        );

        let mut s = thrown(12, 0);
        assert!(
            !sky.settle(&mut s, false, &cx),
            "a grain whose rest is 12 px into ink was kept"
        );
        let mut s = thrown(12, 0);
        assert!(sky.settle(&mut s, true, &cx));
        assert_eq!((s.x, s.y), (365.0, 157.0), "the walk-back moved the birth");
        assert_eq!(
            s.rest(),
            (368, 157),
            "the walk-back did not stop at the first clear pixel"
        );
        assert_eq!(s.pos(done, false), s.rest());

        let mut s = thrown(0, 0);
        s.lane = StarLane::Shed;
        s.class = StarClass::M3;
        s.x = 374.0;
        assert!(
            !sky.settle(&mut s, false, &cx),
            "a fragment resting on the inked cell it was shed over was kept"
        );

        let mut boxed = Stardust::new();
        boxed.probe_mut().probe_row(5, &[true; 120]);
        boxed.probe_mut().probe_row(6, &row6);
        boxed.probe_mut().probe_row(7, &[true; 120]);
        let (ox, oy) = g.cell_center(6, 40);
        boxed.sow_fan(
            t0,
            FanSow {
                at_px: (ox, oy),
                reach: 23.0,
                walk: LandingWalk::free(0.25),
                seed: 7,
                n: 14,
                hero: true,
            },
            &cx,
        );
        let fan: Vec<&Star> = boxed
            .live_iter()
            .filter(|s| s.lane == StarLane::Fan)
            .collect();
        assert_eq!(
            fan.len(),
            5,
            "boxed in by ink, a landing keeps exactly its hero and four m2, not {}",
            fan.len()
        );
        assert_eq!(
            fan.iter()
                .filter(|s| s.class == StarClass::M1 && s.gold)
                .count(),
            1,
            "the gold hero"
        );
        assert_eq!(
            fan.iter().filter(|s| s.class == StarClass::M2).count(),
            4,
            "its four m2"
        );
        for s in &fan {
            assert_eq!((s.x, s.y), (ox.round(), oy.round()), "born on the landing");
            let (rx, ry) = s.rest();
            assert_eq!(
                (px_row(ry as f32, g), px_col(rx as f32, g)),
                (6, 40),
                "a {:?} came to rest off the landing cell, at ({rx}, {ry})",
                s.class
            );
            assert_ne!(
                (rx, ry),
                (ox.round() as i32, oy.round() as i32),
                "a {:?} was pinned at the centre instead of thrown to the cell's edge",
                s.class
            );
        }
    }

    #[test]
    fn a_transient_over_ink_or_unknown_is_held_to_a_third() {
        let c = cfg(true);
        let t0 = Instant::now();
        let peak = |probe: Option<bool>| {
            let mut sky = Stardust::new();
            if let Some(inked) = probe {
                sky.probe_mut().probe_row(3, &[inked; 120]);
            }
            let mut s = star(StarClass::M2, StarLane::Fan, t0, TINT_WHITE_RGB);
            s.seed = peak_seed();
            sky.sow(s);
            let sc = frame_at(&mut sky, t0, &c);
            lum(px(&sc.out, 200, 100))
        };
        let clear = peak(Some(false));
        let inked = peak(Some(true));
        let unknown = peak(None);
        assert!(
            clear >= 79,
            "the clear-row fan m2 did not reach its 80: {clear}"
        );
        assert!(
            inked * 3 <= clear + 3 && inked > 0,
            "over ink the star is {inked}, not a third of {clear}"
        );
        assert!(
            unknown * 3 <= clear + 3 && unknown > 0,
            "over an unprobed row the star is {unknown}, not a third of {clear}"
        );
    }

    fn strike_trace(c: &Config, t0: Instant, after_sow: impl FnOnce(&mut GlyphProbe)) -> Vec<u32> {
        let mut sky = sky();
        sky.sow(star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB));
        after_sow(sky.probe_mut());
        let life_ms = (StarClass::M1.life_s(StarLane::Strike) * 1000.0) as u64;
        (0..life_ms)
            .map(|ms| {
                let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), c);
                lum(px(&sc.out, 200, 100))
            })
            .collect()
    }

    #[test]
    fn a_sky_star_keeps_its_centre_when_the_probe_forgets_its_row() {
        let t0 = Instant::now();
        let tall = cfg(true);
        let mut underline = cfg(true);
        underline.ribbon_tall = false;
        type Case<'a> = (&'a str, &'a Config, fn(&mut GlyphProbe));
        let cases: [Case<'_>; 2] = [
            ("its row forgotten", &tall, |probe| {
                for row in 20..=22 {
                    probe.probe_row(row, &[false; 120]);
                }
                assert!(
                    probe.at_px(200, 100, geom()).is_none(),
                    "the fixture did not forget the star's row"
                );
            }),
            ("its own cell inked under underline", &underline, |probe| {
                let mut row = [false; 120];
                row[px_col(200.0, geom()) as usize] = true;
                probe.probe_row(3, &row);
                assert_eq!(
                    probe.at_px(200, 100, geom()),
                    Some(true),
                    "the fixture did not ink the star's own cell"
                );
            }),
        ];
        for (label, c, change) in cases {
            let before = strike_trace(c, t0, |_| {});
            let after = strike_trace(c, t0, change);
            let peak = before.iter().copied().max().unwrap_or(0);
            assert!(peak > 0, "{label}: the strike m1 never drew");
            assert_eq!(
                before, after,
                "{label}: a sky star's trace changed when the probe changed under it"
            );
        }
    }

    #[test]
    fn the_population_cap_never_refuses_a_newborn() {
        let t0 = Instant::now();
        let mut sky = Stardust::new();
        for k in 0..400u32 {
            let at = t0 + Duration::from_millis(u64::from(k));
            let mut s = star(StarClass::M2, StarLane::Fan, at, TINT_WHITE_RGB);
            s.seed = k;
            sky.sow(s);
            let live = sky.live_iter().filter(|s| s.finish.is_none()).count();
            assert!(live <= STAR_CAP, "{live} live stars over the cap at {k}");
            assert!(
                sky.live() <= STAR_POOL_MAX,
                "the pool overran its allocation at {k}: {}",
                sky.live()
            );
            assert!(
                sky.live_iter().any(|p| p.seed == k),
                "birth {k} was refused at the cap"
            );
        }
    }

    #[test]
    fn stars_over_the_cap_finish_oldest_first_within_forty_ms() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        for k in 0..STAR_CAP as u32 {
            let class = if k == 5 { StarClass::M3 } else { StarClass::M1 };
            let mut s = star(
                class,
                StarLane::Strike,
                t0 + Duration::from_millis(u64::from(k)),
                TINT_WHITE_RGB,
            );
            s.seed = k;
            // A row clear of the caret's cell, which keeps every star off.
            s.x = 100.0 + 5.0 * k as f32;
            s.y = 200.0;
            sky.sow(s);
        }
        let evict_at = t0 + Duration::from_millis(41);
        let mut newborn = star(StarClass::M1, StarLane::Strike, evict_at, TINT_WHITE_RGB);
        newborn.seed = 1000;
        newborn.x = 400.0;
        newborn.y = 200.0;
        sky.sow(newborn);
        let finishing: Vec<u32> = sky
            .live_iter()
            .filter(|s| s.finish.is_some())
            .map(|s| s.seed)
            .collect();
        assert_eq!(
            finishing,
            vec![0],
            "the star put on its finish is not the oldest"
        );

        let lit = |sky: &mut Stardust, ms: u64, x: i32| {
            let sc = frame_at(sky, evict_at + Duration::from_millis(ms), &c);
            lum(px(&sc.out, x, 200)) > 0
        };
        assert!(
            lit(&mut sky, 10, 100),
            "the evicted star popped instead of finishing"
        );
        assert!(
            !lit(&mut sky, 40, 100),
            "the evicted star outlived its 40 ms finish"
        );
        assert!(
            lit(&mut sky, 40, 125),
            "the young grain (#5) was evicted instead of the oldest"
        );
    }

    #[test]
    fn an_earned_hero_is_never_demoted_by_spacing_but_a_dealt_one_is() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();

        for class in [TypedClass::Capital, TypedClass::Bang] {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            for col in 10..13u16 {
                sky.on_event(&typed(class), t0, &ctx(t0, &c, (5, col + 1)), &ribbon);
            }
            assert_eq!(
                sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
                3,
                "adjacent {class:?} keys were demoted by the spacing law"
            );
        }

        let m1_draw = |row: u16, col: u16| {
            hash01(mix32(cell_hash(row, col, 0) ^ SALT_STRIKE)) < 1.0 / DEAL_M1_IN as f32
        };
        let row = (1..40u16)
            .find(|&row| {
                let draws: Vec<u16> = (0..20u16).filter(|&col| m1_draw(row, col)).collect();
                draws.len() >= 3
                    && draws
                        .windows(2)
                        .any(|w| w[1] - w[0] >= HERO_MIN_SPACING_CELLS)
            })
            .expect("some row deals three heroes in twenty cells");
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(i32::from(row) - 1, &[false; 120]);
        for col in 0..20u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (row, col + 1)),
                &ribbon,
            );
        }
        assert!(sky.live() < STAR_CAP, "the fixture ran into the pool cap");
        let heroes: Vec<i32> = sky
            .live_iter()
            .filter(|p| p.class == StarClass::M1)
            .map(col_of)
            .collect();
        assert_eq!(
            heroes.len(),
            usize::from(HERO_MAX_LIVE_PER_ROW),
            "three or more dealt heroes on one row left {} live",
            heroes.len()
        );
        for (i, a) in heroes.iter().enumerate() {
            for b in &heroes[i + 1..] {
                assert!(
                    (a - b).abs() >= i32::from(HERO_MIN_SPACING_CELLS),
                    "dealt heroes at columns {a} and {b} are closer than 3 cells"
                );
            }
        }

        let later = t0 + Duration::from_secs(2);
        let mut sc = Scratch::default();
        sky.emit(&ctx(later, &c, (row, 1)), &mut sc.frame());
        assert!(sky.at_rest());
        sky.on_event(
            &typed(TypedClass::Capital),
            later,
            &ctx(later, &c, (row, 1)),
            &ribbon,
        );
        assert_eq!(
            sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
            1
        );
    }

    #[test]
    fn no_two_sky_stars_share_a_pixel() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut cx = ctx(t0, &c, (5, 1));
        cx.disp = 0.6;
        cx.birth_disp = 0.6;
        for col in 0..119u16 {
            cx.caret = (5, col + 1);
            sky.on_event(&typed(TypedClass::Glyph), t0, &cx, &ribbon);
        }
        let mut strike_per_cell = std::collections::BTreeMap::new();
        let mut pixels = std::collections::BTreeSet::new();
        for s in sky.live_iter() {
            assert!(
                pixels.insert((s.x as i32, s.y as i32)),
                "two sky stars share pixel ({}, {})",
                s.x,
                s.y
            );
            if s.lane == StarLane::Strike && s.v0 != (0.0, 0.0) {
                *strike_per_cell.entry(col_of(s)).or_insert(0) += 1;
            }
        }
        assert!(
            strike_per_cell.values().all(|&n| n == 1),
            "a cell carries two m1/m2 strike stars: {strike_per_cell:?}"
        );
        assert!(
            sky.live() >= STAR_CAP,
            "the dense sky holds only {} stars",
            sky.live()
        );
    }

    #[test]
    fn the_second_grain_is_priced_on_the_honest_spine() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let lead_grains = |disp: f32, birth: f32| {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let mut cx = ctx(t0, &c, (5, 10));
            cx.disp = disp;
            cx.birth_disp = birth;
            sky.on_event(&typed(TypedClass::Space), t0, &cx, &ribbon);
            assert_eq!(sky.live(), 0, "a space dealt a star");
            sky.on_event(&typed(TypedClass::Glyph), t0, &cx, &ribbon);
            sky.live_iter().filter(|s| col_of(s) == 10).count()
        };
        assert_eq!(lead_grains(0.1, 0.9), 0, "a resume bought the dense sky");
        assert_eq!(
            lead_grains(0.6, 0.6),
            1,
            "a hot spine did not throw its second grain"
        );
    }

    #[test]
    fn hero_births_and_glints_share_one_budget() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.budget.refill(t0);

        let mut glints = 0usize;
        for k in 0..12u16 {
            sky.on_event(
                &typed(TypedClass::Capital),
                t0,
                &ctx(t0, &c, (5, 10 + k * 4)),
                &ribbon,
            );
            glints += sky.take_glints().count();
        }
        let heroes = sky.live_iter().filter(|p| p.class == StarClass::M1).count();
        assert_eq!(heroes, 12, "an earned hero was demoted for want of a token");
        assert_eq!(
            glints, GLINT_CAP as usize,
            "{glints} glints from a bucket of {GLINT_CAP}"
        );

        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let before = sky.budget.tokens();
        for k in 0..20u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        assert!(sky.live_iter().any(|p| p.class == StarClass::M3));
        let spent = before - sky.budget.tokens();
        let dealt_heroes = sky.live_iter().filter(|p| p.class == StarClass::M1).count() as f32;
        assert!(
            spent <= dealt_heroes,
            "a grain or a medium star spent {spent} tokens"
        );

        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.budget.empty();
        for col in 0..120u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, col + 1)),
                &ribbon,
            );
        }
        assert_eq!(
            sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
            0,
            "a dealt hero was born on a dry bucket"
        );

        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.budget.empty();
        sky.on_event(
            &typed(TypedClass::Capital),
            t0,
            &ctx(t0, &c, (5, 30)),
            &ribbon,
        );
        assert_eq!(
            sky.take_glints().count(),
            0,
            "a hero chimed on top of the rain"
        );
        assert!(
            sky.live_iter().any(|p| p.class == StarClass::M1),
            "an EARNED hero was demoted for want of a token"
        );
    }

    #[test]
    fn a_paid_hero_cues_one_glint_at_its_own_column() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.on_event(
            &typed(TypedClass::Capital),
            t0,
            &ctx(t0, &c, (5, 21)),
            &ribbon,
        );
        let sc = frame_at(&mut sky, t0, &c);
        assert_eq!(
            sc.cues.len(),
            1,
            "one paid hero did not cue exactly one glint"
        );
        assert_eq!(sc.cues[0].col, 20, "the glint is not at the hero's column");
        match sc.cues[0].kind {
            SoundKind::Stardust { twinkle_hz } => assert!(
                (SCINT_F_MIN as u8..=SCINT_F_MAX as u8).contains(&twinkle_hz),
                "twinkle_hz {twinkle_hz} is outside the seeded 8-12 Hz band"
            ),
            other => panic!("a hero cued {other:?}"),
        }
    }

    #[test]
    fn a_row_zero_hero_clipped_off_the_glass_spends_nothing() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        for (head, want) in [(0u16, 0usize), (40, 1)] {
            let mut sky = Stardust::new();
            let before = sky.budget.tokens();
            let mut cx = ctx(t0, &c, (0, 5));
            cx.geom.head = head;
            cx.geom.origin_y = head;
            sky.on_event(&typed(TypedClass::Capital), t0, &cx, &ribbon);
            let heroes = sky.live_iter().filter(|s| s.class == StarClass::M1).count();
            assert_eq!(heroes, want, "head {head}: {heroes} heroes");
            assert_eq!(sky.take_glints().count(), want, "head {head}: glint count");
            assert_eq!(
                (before - sky.budget.tokens()) as usize,
                want,
                "head {head}: a clipped hero spent a token"
            );
        }
    }

    #[test]
    fn the_frame_that_echoes_the_key_already_shows_its_field_star() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        let mut field = star(StarClass::M3, StarLane::Field, t0, TINT_WHITE_RGB);
        field.x = 150.0;
        sky.sow(field);
        let echo = t0 + Duration::from_micros(300);
        let a = field.envelope(echo, false);
        assert!(
            a >= BIRTH_EDGE_FLOOR,
            "a field star's birth envelope is {a} on the echo frame"
        );
        let sc = frame_at(&mut sky, echo, &c);
        assert!(
            lum(px(&sc.out, 150, 100)) > 0,
            "the echo frame shows no field star"
        );
        let full = t0 + Duration::from_secs_f32(super::super::timing::EDGE_IN_S);
        assert!(
            (field.envelope(full, false) - 1.0).abs() < 1e-4,
            "the edge-in does not reach full: {}",
            field.envelope(full, false)
        );
    }

    #[test]
    fn a_field_star_rides_its_cell_s_envelope_not_the_grain_s_life() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let fixture = || {
            let mut sky = sky();
            let mut field = star(StarClass::M3, StarLane::Field, t0, TINT_WHITE_RGB);
            field.x = 150.0;
            sky.sow(field);
            sky.sow(star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB));
            sky
        };
        let field_alpha = |sky: &Stardust, ms: u64| {
            sky.live_iter()
                .find(|s| s.lane == StarLane::Field)
                .map_or(0.0, |s| s.envelope(at(ms), false))
        };

        let mut sky = fixture();
        let mut next_key = 83u64;
        let mut type_until = |sky: &mut Stardust, ms: u64| {
            while next_key <= ms {
                let cx = ctx(at(next_key), &c, (3, 10));
                sky.on_event(&typed(TypedClass::Space), at(next_key), &cx, &ribbon);
                next_key += 83;
            }
        };
        for ms in [500u64, 1000] {
            type_until(&mut sky, ms);
            let a = field_alpha(&sky, ms);
            assert!(
                (a - 1.0).abs() < 1e-3,
                "a field grain under a typing hand is at alpha {a} at {ms} ms, not full"
            );
        }
        let sc = frame_at(&mut sky, at(1000), &c);
        assert!(
            lum(px(&sc.out, 150, 100)) > 0,
            "the field grain died with the grain's life"
        );
        assert_eq!(
            lum(px(&sc.out, 200, 100)),
            0,
            "a strike grain outlived its 225 ms"
        );
        let melt_from = ((1.0 - EXPIRY_MELT_SHARE) * FIELD_LIFE_S * 1000.0).round() as u64;
        type_until(&mut sky, melt_from + 20);
        let a_early = field_alpha(&sky, melt_from + 20);
        type_until(&mut sky, melt_from + 220);
        let a_late = field_alpha(&sky, melt_from + 220);
        assert!(
            a_early < 1.0 && a_late < a_early && a_late > STAR_CULL_ALPHA,
            "the field grain does not melt on the cell's curve: {a_early} → {a_late}"
        );
        let sc = frame_at(&mut sky, at(melt_from + 220), &c);
        assert!(lum(px(&sc.out, 150, 100)) > 0, "gone before its melt");
        let horizon = (FIELD_LIFE_S * 1000.0).round() as u64;
        type_until(&mut sky, horizon);
        let sc = frame_at(&mut sky, at(horizon + 1), &c);
        assert!(
            sc.out.is_empty() && sky.live() == 0,
            "a field star outlived the exit swoosh"
        );

        let mut sky = fixture();
        let hold = (FIELD_HOLD_MS as u64).saturating_sub(1);
        let a = field_alpha(&sky, hold);
        assert!(
            (a - 1.0).abs() < 1e-3,
            "a field grain is at alpha {a} inside its hold"
        );
        let (a500, a700) = (field_alpha(&sky, 500), field_alpha(&sky, 700));
        assert!(
            a500 < 1.0 && a700 <= 0.5 && a700 > STAR_CULL_ALPHA,
            "the field fade is not a half-life from the last key: {a500} → {a700}"
        );
        let cull = (field_fade_life_s() * 1000.0).round() as u64;
        let sc = frame_at(&mut sky, at(cull - 10), &c);
        assert!(
            lum(px(&sc.out, 150, 100)) > 0,
            "the field grain went out before its fade's cull"
        );
        let sc = frame_at(&mut sky, at(cull + 1), &c);
        assert!(
            sc.out.is_empty() && sky.at_rest(),
            "a field star outlived the field fade after the hand stopped"
        );
    }

    #[test]
    fn a_field_star_is_pinned_and_finishes_with_its_cell() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let col = (0..120u16)
            .find(|&col| {
                let h = cell_hash(5, col, 0);
                deal(h, SALT_FIELD, FIELD_DEAL_IN, 1.0) && deal(h, SALT_FIELD_M2, FIELD_M2_IN, 1.0)
            })
            .expect("some column deals a field m2");
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.on_event(
            &typed(TypedClass::Glyph),
            t0,
            &ctx(t0, &c, (5, col + 1)),
            &ribbon,
        );
        let field: Vec<&Star> = sky
            .live_iter()
            .filter(|s| s.lane == StarLane::Field)
            .collect();
        assert_eq!(
            field.len(),
            1,
            "the dealt cell carries {} field stars",
            field.len()
        );
        assert_eq!(field[0].class, StarClass::M2);
        assert_eq!(field[0].v0, (0.0, 0.0), "a field star lifted");

        let erase_at = t0 + Duration::from_millis(50);
        sky.on_event(
            &Event::Erase,
            erase_at,
            &ctx(erase_at, &c, (5, col)),
            &ribbon,
        );
        assert!(
            sky.live_iter()
                .any(|s| s.lane == StarLane::Field && s.finish.is_some()),
            "the retracted cell's field star was not put on the retract"
        );
        let later = t0 + Duration::from_millis(200);
        let mut sc = Scratch::default();
        sky.emit(&ctx(later, &c, (5, col)), &mut sc.frame());
        assert!(
            !sky.live_iter().any(|s| s.lane == StarLane::Field),
            "the field star outlived its cell's retract"
        );
    }

    #[test]
    fn focus_loss_embers_the_sky_instead_of_popping_it() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = sky();
        for k in 0..3u32 {
            let mut s = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
            s.seed = k;
            s.x = 100.0 + 20.0 * k as f32;
            sky.sow(s);
        }
        let blur = t0 + Duration::from_millis(10);
        sky.on_event(&Event::Focus(false), blur, &ctx(blur, &c, (3, 10)), &ribbon);
        let peak = |sky: &mut Stardust, ms: u64| {
            let sc = frame_at(sky, blur + Duration::from_millis(ms), &c);
            (0..3)
                .map(|k| lum(px(&sc.out, 100 + 20 * k, 100)))
                .max()
                .unwrap_or(0)
        };
        let at_1 = peak(&mut sky, 1);
        let at_100 = peak(&mut sky, 100);
        assert!(at_1 > 0, "the sky popped on the blur frame");
        assert!(
            at_100 > 0 && at_100 < at_1,
            "the ember is not a fade: {at_1} → {at_100}"
        );
        assert_eq!(peak(&mut sky, 300), 0, "a star outlived the 300 ms ember");
        assert!(sky.at_rest());
    }

    #[test]
    fn an_idle_sky_draws_exactly_nothing() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        let sc = frame_at(&mut sky, t0, &c);
        assert!(sc.out.is_empty() && sc.halos.is_empty() && sc.under.is_empty());
        assert!(sky.at_rest());
        assert!(sky.next_change_deadline(t0).is_none());

        sky.sow(star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB));
        assert!(!sky.at_rest() && sky.next_change_deadline(t0).is_some());
        let life = Duration::from_secs_f32(StarClass::M1.life_s(StarLane::Strike));
        let sc = frame_at(&mut sky, t0 + life + Duration::from_millis(1), &c);
        assert!(sc.out.is_empty() && sc.halos.is_empty());
        assert!(sky.at_rest() && sky.next_change_deadline(t0).is_none());
    }

    #[test]
    fn every_v2_mark_forks_to_source_over_on_light() {
        let light = cfg(false);
        let t0 = Instant::now();
        for (class, lane, _) in every_class() {
            let mut sky = sky();
            sky.sow(star(class, lane, t0, spectrum_snap(0.3)));
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(1), &light);
            assert!(
                sc.out.is_empty(),
                "{class:?}/{lane:?} left an additive quad on light"
            );
            assert!(
                !sc.halos.is_empty(),
                "{class:?}/{lane:?} drew nothing on light"
            );
            for h in &sc.halos {
                assert_eq!(
                    h.mode,
                    HaloMode::Over,
                    "{class:?}/{lane:?} emitted an additive halo"
                );
                let a = h.color >> 24;
                assert!(
                    a >= 1 && a as f32 <= LIGHT_ALPHA_CAP,
                    "{class:?}/{lane:?} alpha {a}"
                );
                if h.rx != h.ry {
                    assert!(
                        a as f32 <= InkRole::OverText.alpha_cap(),
                        "{class:?}/{lane:?} needle alpha {a} over the over-text cap"
                    );
                }
            }
            let round = sc.halos.iter().filter(|h| h.rx == h.ry).count();
            if class == StarClass::M3 {
                let shapes: std::collections::BTreeSet<_> =
                    sc.halos.iter().map(|h| (h.rx, h.ry)).collect();
                let dots = shapes.iter().filter(|(rx, ry)| rx == ry).count();
                let needles: Vec<_> = shapes.iter().filter(|(rx, ry)| rx != ry).collect();
                assert_eq!(dots, 1, "a light grain is not one round dot: {shapes:?}");
                assert!(
                    needles.len() == 2
                        && needles.iter().all(|(rx, ry)| (*rx).min(*ry) == 1)
                        && needles[0] == &(needles[1].1, needles[1].0),
                    "a light grain's tips are not two crossed 1-px needles: {shapes:?}"
                );
                let centres: std::collections::BTreeSet<_> =
                    sc.halos.iter().map(|h| (h.cx, h.cy)).collect();
                assert_eq!(centres.len(), 1);
            } else {
                assert!(sc.halos.len() > round, "a light {class:?} has no arms");
            }
        }
        for (class, glints) in [(StarClass::M1, 4), (StarClass::M2, 0)] {
            let mut sky = sky();
            sky.sow(star(class, StarLane::Strike, t0, TINT_GOLD_RGB));
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(1), &light);
            let off_centre = sc
                .halos
                .iter()
                .filter(|h| (h.rx, h.ry) == (1, 1) && (h.cx, h.cy) != (200, 100))
                .count();
            assert_eq!(
                off_centre, glints,
                "a gold {class:?} on light threw {off_centre} glint dots, not {glints}"
            );
        }
    }

    const PAPER: u32 = 0x00FF_FFFF;

    fn ink_on_paper(sc: &Scratch, x0: i32, y0: i32, w: usize, h: usize) -> u64 {
        composite_over(sc, PAPER, x0, y0, w, h)
            .iter()
            .map(|&p| u64::from(765 - ((p >> 16) & 0xff) - ((p >> 8) & 0xff) - (p & 0xff)))
            .sum()
    }

    #[test]
    fn a_light_star_s_ink_is_steady_on_the_size_clock() {
        const STEP_MAX: f32 = 1.35;
        let light = cfg(false);
        let t0 = Instant::now();
        let mut failures = Vec::new();
        for g in [geom_cell(15, 28), geom()] {
            for class in [StarClass::M3, StarClass::M2, StarClass::M1] {
                let mut sky = sky();
                sky.sow(star(class, StarLane::Strike, t0, spectrum_snap(0.3)));
                let life_ms = (class.life_s(StarLane::Strike) * 1000.0) as u64;
                let mut bodies = std::collections::BTreeSet::new();
                let (mut prev, mut worst, mut worst_at) = (None::<(u64, bool)>, 1.0f32, 0u64);
                for ms in 0..life_ms {
                    let now = t0 + Duration::from_millis(ms);
                    let held = sky.stars.iter().any(|s| s.in_hold(now));
                    let sc = frame_in(&mut sky, g, now, &light);
                    let Some(first) = sc.halos.first() else {
                        prev = None;
                        continue;
                    };
                    bodies.insert((first.rx, first.ry));
                    let ink = ink_on_paper(&sc, 170, 70, 60, 60);
                    if let Some((p, was_held)) = prev
                        && was_held == held
                    {
                        let step = p.max(ink) as f32 / p.min(ink).max(1) as f32;
                        if step > worst {
                            (worst, worst_at) = (step, ms);
                        }
                    }
                    prev = Some((ink, held));
                }
                eprintln!(
                    "{class:?} at {}x{}: ink steps at most {worst:.2}x between 1 ms frames (at \
                     {worst_at} ms); body radii {bodies:?}",
                    g.cw, g.ch
                );
                if bodies.len() != 1 {
                    failures.push(format!(
                        "{class:?} at ch {}: the dot or nucleus changes size, {bodies:?}",
                        g.ch
                    ));
                }
                if worst > STEP_MAX {
                    failures.push(format!(
                        "{class:?} at ch {}: the ink steps {worst:.2}x in 1 ms at {worst_at} ms, \
                         over the dark grain's {STEP_MAX}x",
                        g.ch
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "a light star's ink rides the size clock:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn a_fan_star_is_born_at_full_and_clears_its_cell_inside_its_hold() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom();
        let cx = ctx(t0, &c, (5, 40));
        let (ox, oy) = g.cell_center(5, 40);
        let origin = (ox.round() as i32, oy.round() as i32);
        let reach = 2.55 * g.ch as f32;
        let half = (g.cw as i32 / 2, g.ch as i32 / 2);
        let at = t0 + Duration::from_millis(HOLD_M2_TRANSIENT_MS as u64);
        for seed in 0..12u32 {
            let mut sky = Stardust::new();
            sky.sow_fan(
                t0,
                FanSow {
                    at_px: (ox, oy),
                    reach,
                    walk: LandingWalk::free(0.3),
                    seed: seed.wrapping_mul(0x9E37_79B9),
                    n: 9,
                    hero: true,
                },
                &cx,
            );
            for s in sky.live_iter().filter(|s| s.class != StarClass::M3) {
                assert!(
                    s.in_hold(at - Duration::from_millis(1)),
                    "the fixture's own hold"
                );
                let (x, y) = s.pos(at, false);
                let (dx, dy) = ((x - origin.0).abs(), (y - origin.1).abs());
                assert!(
                    dx > half.0 || dy > half.1,
                    "seed {seed}: a {:?} fan star is still on the caret cell ({dx}, {dy} px \
                     from its centre) at the end of its hold",
                    s.class
                );
            }
        }
    }

    #[test]
    fn halos_are_budgeted_per_star_not_per_split() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        for k in 0..17u32 {
            let mut s = star(StarClass::M2, StarLane::Strike, t0, spectrum_snap(0.1));
            s.seed = peak_seed();
            s.x = 60.0 + 20.0 * k as f32;
            s.y = 93.0;
            sky.sow(s);
        }
        let sc = frame_at(&mut sky, t0, &c);
        let haloed: std::collections::BTreeSet<u16> = sc.halos.iter().map(|h| h.cx).collect();
        assert_eq!(
            haloed.len(),
            STARDUST_HALO_BUDGET,
            "{} stars carry a halo",
            haloed.len()
        );
        assert!(
            sc.halos.len() > STARDUST_HALO_BUDGET,
            "the halos did not split across the row boundary"
        );
    }

    /// On a dark page no star stands on the caret's block: after the
    /// gallery's dark Return a fan star's crosshair stood at the new caret's
    /// corner, its hairline at the block's edge (looks7 M3, `e120`). A star
    /// whose arm at its peak reaches the caret's cell or within
    /// [`crate::rainbow_kitty::DARK_CARET_GAP_PX`] of it is not drawn, over its
    /// life; with the caret a few cells off (the negative control) the same
    /// stars are.
    #[test]
    fn no_dark_star_stands_on_the_caret_s_block() {
        let c = cfg(true);
        let g = geom();
        let t0 = Instant::now();
        // The caret's cell at (3, 10): x 90..99, y 94..112.
        let drawn = |caret: (u16, u16)| -> usize {
            let mut n = 0;
            for (x, y) in [(94.0, 103.0), (104.0, 110.0), (86.0, 90.0), (95.0, 116.0)] {
                let mut sky = sky();
                let mut s = star(StarClass::M3, StarLane::Fan, t0, TINT_WHITE_RGB);
                (s.x, s.y) = (x, y);
                sky.sow(s);
                for at in [0u64, 30, 90, 160] {
                    let mut sc = Scratch::default();
                    let mut f = sc.frame();
                    sky.emit(
                        &ctx_in(g, t0 + Duration::from_millis(at), &c, caret),
                        &mut f,
                    );
                    n += sc.out.len() + sc.halos.len();
                }
            }
            n
        };
        let on = drawn((3, 10));
        assert_eq!(on, 0, "{on} star lights stand on the dark caret's block");
        assert!(
            drawn((3, 30)) > 0,
            "control: off the caret the stars are not drawn either"
        );
    }

    #[test]
    fn an_over_budget_frame_keeps_every_star_s_core() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        for k in 0..STAR_CAP as u32 {
            let mut s = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
            s.seed = peak_seed();
            // A row clear of the caret's cell, which keeps every star off.
            s.x = 20.0 + 24.0 * k as f32;
            s.y = 200.0;
            sky.sow(s);
        }
        for k in 0..STAR_FINISH_SLACK as u32 {
            let mut s = star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB);
            s.seed = peak_seed();
            s.x = 20.0 + 24.0 * k as f32;
            s.y = 160.0;
            sky.sow(s);
        }
        assert_eq!(sky.live(), STAR_POOL_MAX, "the fixture is not a full pool");
        let sc = frame_at(&mut sky, t0, &c);
        assert!(
            sc.out.len() <= STARDUST_QUAD_BUDGET,
            "{} quads over the budget",
            sc.out.len()
        );
        for k in 0..STAR_CAP as i32 {
            assert!(
                lum(px(&sc.out, 20 + 24 * k, 200)) > 0,
                "hero {k} lost its core to the budget"
            );
        }
        for k in 0..STAR_FINISH_SLACK as i32 {
            assert!(
                lum(px(&sc.out, 20 + 24 * k, 160)) > 0,
                "grain {k} lost its core to the budget"
            );
        }
    }

    #[test]
    fn reduced_motion_freezes_both_clocks_and_takes_the_linear_fade() {
        let mut c = cfg(true);
        c.reduced_motion = true;
        let t0 = Instant::now();
        let mut sky = sky();
        let mut s = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
        s.v0 = (0.0, sky_lift_px_per_s(geom().ch as f32) / 1000.0);
        sky.sow(s);
        let life_ms = (StarClass::M2.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut widths = std::collections::BTreeSet::new();
        for ms in (0..life_ms - 130).step_by(7) {
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
            widths.insert(sc.out.iter().map(|q| q.w).max().unwrap_or(0));
            assert!(
                lum(px(&sc.out, 200, 100)) > 0,
                "the mark drifted at {ms} ms"
            );
        }
        assert_eq!(
            widths.len(),
            1,
            "the arm clock ran under reduced motion: {widths:?}"
        );
    }

    #[test]
    fn a_scroll_moves_the_sky_and_forgets_the_probe() {
        let t0 = Instant::now();
        let mut sky = sky();
        let mut high = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
        high.y = 50.0;
        sky.sow(high);
        sky.sow(star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB));
        sky.translate_scroll(3, 18);
        assert_eq!(sky.live(), 1, "a star that left the grid was kept");
        assert_eq!(sky.live_iter().next().map(|s| s.y), Some(46.0));
        assert!(sky.probe().is_empty(), "the probe survived a scroll");
    }

    #[test]
    fn a_band_move_moves_stars_in_the_band_and_clears_the_probe() {
        let t0 = Instant::now();
        let mut sky = sky();
        assert!(!sky.probe().is_empty(), "fixture: rows 2-4 are probed");
        let mut high = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
        high.y = 50.0;
        sky.sow(high);
        sky.sow(star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB));
        for row in [5u16, 10] {
            sky.veil.push(Veil {
                row,
                col: 4,
                t: 0.3,
                gain: 1.0,
                born: t0,
                lit: t0,
                finish: None,
            });
        }

        sky.translate_band(4, 10, 1, 18, 0);

        assert_eq!(sky.live(), 2, "no star inside or outside the band was lost");
        let ys: Vec<f32> = sky.live_iter().map(|s| s.y).collect();
        assert!(
            ys.contains(&50.0),
            "the row-2 star is outside the band: {ys:?}"
        );
        assert!(ys.contains(&118.0), "the row-5 star rode to row 6: {ys:?}");
        assert_eq!(
            sky.veil.iter().map(|v| v.row).collect::<Vec<_>>(),
            vec![6],
            "the row-5 veil rode to row 6 and the row-10 veil left the band"
        );
        assert!(sky.probe().is_empty(), "the probe survived a band move");

        sky.translate_band(0, 3, -1, 18, 0);
        let ys: Vec<f32> = sky.live_iter().map(|s| s.y).collect();
        assert!(ys.contains(&32.0) && ys.contains(&118.0), "{ys:?}");
        sky.translate_band(0, 3, -2, 18, 0);
        assert_eq!(sky.live(), 1, "a star that left the band was kept");
        assert_eq!(sky.live_iter().next().map(|s| s.y), Some(118.0));

        let mut cold = Stardust::new();
        cold.sow(star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB));
        cold.translate_band(4, 10, 1, 0, 0);
        assert_eq!(cold.live(), 0);
    }

    #[test]
    fn band_moves_follow_the_earning_glyph_at_sky_and_footer_edges() {
        let now = Instant::now();
        let ribbon = Ribbon::new();
        let mut caught_pixel_only = 0;
        for tall in [false, true] {
            let mut config = cfg(true);
            config.ribbon_tall = tall;
            for top in [0u16, 4] {
                let bottom = top + 6;
                for home in [top, top + 1, bottom, bottom + 1] {
                    for delta in [-1i16, 1] {
                        let mut sky = Stardust::new();
                        if let Some(above) = home.checked_sub(1) {
                            sky.probe_mut().probe_row(i32::from(above), &[false; 120]);
                        }
                        sky.probe_mut().probe_row(i32::from(home), &[false; 120]);
                        let context = ctx(now, &config, (home, 10));
                        assert!(sky.deal_star(
                            Deal {
                                class: StarClass::M2,
                                lane: StarLane::Field,
                                cell: (home, 10),
                                earned: false,
                                pull: None,
                                tint: None,
                            },
                            now,
                            &context,
                            &ribbon
                        ));
                        let before = *sky.live_iter().next().unwrap();
                        assert_eq!(before.home_row, Some(home));
                        let moved = i32::from(home) + i32::from(delta);
                        let expected = if home > bottom {
                            Some(home)
                        } else if moved < i32::from(top) || moved > i32::from(bottom) {
                            None
                        } else {
                            Some(moved as u16)
                        };
                        sky.translate_band(top, bottom, delta, geom().ch as u16, geom().origin_y);
                        if let Some(row) = expected {
                            let after = sky.live_iter().next().unwrap();
                            assert_eq!(after.home_row, Some(row));
                            let wanted_y = before.y
                                + (i32::from(row) - i32::from(home)) as f32 * geom().ch as f32;
                            assert_eq!(after.y, wanted_y);
                            assert_eq!(
                                (after.born, after.lit, after.seed),
                                (before.born, before.lit, before.seed)
                            );
                            let mut wrong_y = before.y;
                            let retained =
                                BandPx::new(top, bottom, delta, geom().ch as u16, geom().origin_y)
                                    .point(&mut wrong_y);
                            if !retained || wrong_y != wanted_y {
                                caught_pixel_only += 1;
                            }
                        } else {
                            assert_eq!(sky.live(), 0, "a star outlived its ejected glyph row");
                        }
                    }
                }
            }
        }
        assert!(
            caught_pixel_only >= 4,
            "the fixture must catch pixel-row ownership at both band edges"
        );
    }

    #[test]
    fn sky_ownership_survives_uniform_scroll_before_a_band_move() {
        let now = Instant::now();
        let config = cfg(true);
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(6, &[false; 120]);
        sky.probe_mut().probe_row(7, &[false; 120]);
        assert!(sky.deal_star(
            Deal {
                class: StarClass::M2,
                lane: StarLane::Field,
                cell: (7, 10),
                earned: false,
                pull: None,
                tint: None,
            },
            now,
            &ctx(now, &config, (7, 10)),
            &Ribbon::new()
        ));
        let before = *sky.live_iter().next().unwrap();
        sky.translate_scroll(1, geom().ch as u16);
        assert_eq!(sky.live_iter().next().unwrap().home_row, Some(6));
        sky.translate_band(6, 10, 1, geom().ch as u16, geom().origin_y);
        let after = sky.live_iter().next().unwrap();
        assert_eq!((after.home_row, after.y), (Some(7), before.y));
        sky.translate_scroll(8, geom().ch as u16);
        assert_eq!(sky.live(), 0, "an ejected owner retires its sky star");
    }

    #[test]
    fn the_size_clocks_next_step_is_solved_not_polled() {
        let t0 = Instant::now();
        let ch = geom().ch as f32;
        for class in [StarClass::M1, StarClass::M2, StarClass::M3] {
            for (f, seed) in [(8.0, 0x11u32), (10.3, 0x2222), (12.0, 0x3333_3333)] {
                let mut s = star(class, StarLane::Strike, t0, TINT_WHITE_RGB);
                s.f = f;
                s.seed = seed;
                let hold = class.envelope_s(StarLane::Strike).0;
                for k in 0..7 {
                    let now = t0 + Duration::from_secs_f32(hold + 0.013 * k as f32);
                    let dt = s
                        .next_arm_step_s(ch, now)
                        .expect("every class swings across an integer arm");
                    assert!(
                        dt > 0.0 && dt <= 0.5 / f + 1e-4,
                        "{class:?} f={f}: step {dt} s outside (0, 1/(2f)]"
                    );
                    let here = arm_px(&s, ch, s.in_hold(now), now);
                    let mut polled = None;
                    let mut t = 0.0f32;
                    while t < 0.2 {
                        t += 0.000_05;
                        let at = now + Duration::from_secs_f32(t);
                        if arm_px(&s, ch, s.in_hold(at), at) != here {
                            polled = Some(t);
                            break;
                        }
                    }
                    let polled = polled.expect("the polled arm never stepped");
                    assert!(
                        (dt - polled).abs() < 1e-4,
                        "{class:?} f={f} k={k}: solved {dt} s, polled {polled} s"
                    );
                }
            }
        }
    }

    #[test]
    fn the_glint_toggle_is_solved_not_polled() {
        let t0 = Instant::now();
        let mut gold = star(StarClass::M1, StarLane::Strike, t0, TINT_GOLD_RGB);
        for (k, seed) in [0x5u32, 0x77, 0xABCD].into_iter().enumerate() {
            gold.seed = seed;
            let now = t0 + Duration::from_millis(30 * k as u64 + 1);
            let dt = gold.next_glint_toggle_s(now).expect("gold toggles");
            let here = gold.glinting(now);
            let mut t = 0.0f32;
            let polled = loop {
                t += 0.000_05;
                if gold.glinting(now + Duration::from_secs_f32(t)) != here {
                    break t;
                }
                assert!(t < 1.0, "the glint never toggled");
            };
            assert!(
                (dt - polled).abs() < 1e-4,
                "seed {seed:#x}: solved {dt} s, polled {polled} s"
            );
        }
        let white = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
        assert!(white.next_glint_toggle_s(t0).is_none());
    }

    const SHIPPED_GROUND: u32 = 0x001A_1B26;

    fn composite_over(sc: &Scratch, ground: u32, x0: i32, y0: i32, w: usize, h: usize) -> Vec<u32> {
        let mut canvas = vec![ground; w * h];
        for q in &sc.out {
            for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
                for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                    let (cx, cy) = (x - x0, y - y0);
                    if cx < 0 || cy < 0 || cx as usize >= w || cy as usize >= h {
                        continue;
                    }
                    let p = &mut canvas[cy as usize * w + cx as usize];
                    *p = aterm_render::over_premul(*p, q.color, q.alpha);
                }
            }
        }
        for mode in [HaloMode::Add, HaloMode::Over] {
            for a in sc.halos.iter().filter(|a| a.mode == mode) {
                let (rx2, ry2) = (
                    i32::from(a.rx) * i32::from(a.rx),
                    i32::from(a.ry) * i32::from(a.ry),
                );
                for y in i32::from(a.y)..i32::from(a.y) + i32::from(a.h) {
                    let ny = halo_row_ny(y - i32::from(a.cy), ry2);
                    if ny >= 256 {
                        continue;
                    }
                    for x in i32::from(a.x)..i32::from(a.x) + i32::from(a.w) {
                        let wt = halo_weight(x - i32::from(a.cx), ny, rx2).min(255) as u8;
                        let (cx, cy) = (x - x0, y - y0);
                        if wt == 0 || cx < 0 || cy < 0 || cx as usize >= w || cy as usize >= h {
                            continue;
                        }
                        let p = &mut canvas[cy as usize * w + cx as usize];
                        *p = match mode {
                            HaloMode::Add => add_sat(*p, premul_rgb(a.color, wt)),
                            HaloMode::Over => aterm_render::over_rgb(
                                *p,
                                a.color,
                                wt.min(aterm_render::halo_over_cap(a.color)),
                            ),
                        };
                    }
                }
            }
        }
        canvas
    }

    fn veils(sc: &Scratch) -> Vec<RainHalo> {
        sc.halos.iter().filter(|h| h.rx != h.ry).copied().collect()
    }

    #[test]
    fn no_star_outshines_the_caret_s_floor_share_on_the_shipped_ground() {
        let t0 = Instant::now();
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let tints: Vec<u32> = (0..SPECTRUM_STOPS)
            .map(spectrum_stop)
            .chain([TINT_GOLD_RGB, TINT_WHITE_RGB])
            .collect();
        let mut worst = (0.0f32, String::new());
        let mut white_hero_at_full = 0.0f32;
        for intensity in [0.7f32, 1.0] {
            let mut c = cfg(true);
            c.theme_bg = SHIPPED_GROUND;
            c.intensity = intensity;
            for (class, lane, _) in every_class() {
                for &tint in &tints {
                    let mut sky = sky();
                    sky.sow(star(class, lane, t0, tint));
                    let life_ms = (class.life_s(lane) * 1000.0) as u64;
                    let mut peak = 0.0f32;
                    for ms in (0..life_ms).step_by(5) {
                        let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
                        for &p in &composite_over(&sc, SHIPPED_GROUND, 170, 70, 60, 60) {
                            peak = peak.max(relative_luminance(p) * 255.0);
                        }
                    }
                    if peak > worst.0 {
                        worst = (
                            peak,
                            format!("{class:?}/{lane:?} {tint:#08x} at intensity {intensity}"),
                        );
                    }
                    if class == StarClass::M1
                        && lane == StarLane::Strike
                        && tint == TINT_WHITE_RGB
                        && intensity == 1.0
                    {
                        white_hero_at_full = peak;
                    }
                }
            }
        }
        assert!(
            worst.0 <= ceil,
            "{} reached Y {:.1} over the caret law's {ceil:.1}",
            worst.1,
            worst.0
        );
        assert!(
            white_hero_at_full >= ceil - 1.0,
            "NON-VACUOUS: the white hero at full intensity stands at Y {white_hero_at_full:.1}, \
             not against the ceiling {ceil:.1}"
        );
    }

    #[test]
    fn the_closer_dims_a_star_by_one_factor_until_its_arms_fit_and_leaves_one_that_fits() {
        let g = geom();
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let y = |p: u32| relative_luminance(p) * 255.0;
        let yellow = spectrum_stop(2);
        let mut sc = Scratch::default();
        push_fx_rect(&mut sc.out, g, 300, 100, 1, 1, 0x0030_3030);
        let q0 = sc.out.len();
        push_fx_rect(
            &mut sc.out,
            g,
            200,
            100,
            1,
            1,
            premul_rgb(TINT_WHITE_RGB, 40),
        );
        push_fx_rect(&mut sc.out, g, 203, 100, 2, 1, premul_rgb(yellow, 61));
        push_fx_rect(
            &mut sc.out,
            g,
            203,
            100,
            1,
            1,
            premul_rgb(TINT_WHITE_RGB, 35),
        );
        let h0 = sc.halos.len();
        push_halo_quads(
            &mut sc.halos,
            g,
            usize::MAX,
            (200, 100),
            (15, 15),
            premul_rgb(yellow, 61),
            HaloMode::Add,
        );
        assert!(sc.halos.len() > h0, "no halo was laid");
        let before: Vec<u32> = sc.out[q0..].iter().map(|q| q.color).collect();
        let halo_before = sc.halos[h0].color;
        let watch: [((i32, i32), u32); SETTLE_PIXELS] = [
            ((200, 100), SHIPPED_GROUND),
            ((203, 100), SHIPPED_GROUND),
            ((197, 100), SHIPPED_GROUND),
            ((200, 103), SHIPPED_GROUND),
            ((200, 97), SHIPPED_GROUND),
            ((201, 100), SHIPPED_GROUND),
            ((199, 100), SHIPPED_GROUND),
            ((200, 101), SHIPPED_GROUND),
            ((200, 99), SHIPPED_GROUND),
        ];
        let at = |sc: &Scratch, p: (i32, i32)| {
            let mut px = SHIPPED_GROUND;
            for q in &sc.out[q0..] {
                if quad_covers(q, p.0, p.1) {
                    px = add_sat(px, q.color);
                }
            }
            for h in &sc.halos[h0..] {
                px = add_sat(px, halo_add_at(h, p.0, p.1));
            }
            px
        };
        assert!(
            y(at(&sc, (200, 100))) <= ceil,
            "the centre is lawful as laid"
        );
        assert!(
            y(at(&sc, (203, 100))) > ceil,
            "the stub pixel is over as laid"
        );
        {
            let mut f = sc.frame();
            settle_under_ceiling(&mut f, q0, h0, &watch);
        }
        for &(p, _) in &watch {
            assert!(
                y(at(&sc, p)) <= ceil,
                "({}, {}) is at Y {:.1} after the closer",
                p.0,
                p.1,
                y(at(&sc, p))
            );
        }
        assert!(
            y(at(&sc, (203, 100))) >= ceil - 2.0,
            "the closer dimmed past the ceiling: the stub pixel is at Y {:.1}",
            y(at(&sc, (203, 100)))
        );
        let k = lum(sc.halos[h0].color) as f32 / lum(halo_before) as f32;
        assert!(k > 0.3 && k < 1.0, "the factor {k} is not a dimming");
        for (q, &was) in sc.out[q0..].iter().zip(&before) {
            for sh in [16, 8, 0] {
                let (now, then) = (((q.color >> sh) & 0xff) as f32, ((was >> sh) & 0xff) as f32);
                assert!(
                    (now - then * k).abs() <= 1.0,
                    "a lay carries {now} where the factor {k} says {}",
                    then * k
                );
            }
        }
        assert!(chroma(sc.halos[h0].color) > 0, "the halo lost its colour");
        assert_eq!(sc.out[0].color, 0x0030_3030, "the earlier quad moved");

        let mut sc = Scratch::default();
        push_fx_rect(
            &mut sc.out,
            g,
            200,
            100,
            1,
            1,
            premul_rgb(TINT_WHITE_RGB, 40),
        );
        push_fx_rect(&mut sc.out, g, 203, 100, 2, 1, premul_rgb(yellow, 30));
        push_halo_quads(
            &mut sc.halos,
            g,
            usize::MAX,
            (200, 100),
            (15, 15),
            premul_rgb(yellow, 20),
            HaloMode::Add,
        );
        let (out, halos) = (sc.out.clone(), sc.halos.clone());
        {
            let mut f = sc.frame();
            settle_under_ceiling(&mut f, 0, 0, &watch);
        }
        assert!(sc.out.iter().zip(&out).all(|(a, b)| a.color == b.color));
        assert!(sc.halos.iter().zip(&halos).all(|(a, b)| a.color == b.color));
    }

    #[test]
    fn the_aurora_rises_with_momentum_and_never_enters_a_glyph_row() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let run = || -> Vec<(u16, f32, Vec<RainHalo>)> {
            let mut sky = Stardust::new();
            let mut above = [false; 120];
            above[25..=30].fill(true);
            sky.probe_mut().probe_row(4, &above);
            let mut frames = Vec::new();
            for k in 0..40u16 {
                let at = key_at(t0, k);
                let mut cx = ctx(at, &c, (5, 10 + k));
                cx.disp = disp_at_12cps(k);
                cx.birth_disp = cx.disp;
                cx.caret_t = 0.0;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                let mut sc = Scratch::default();
                sky.emit(&cx, &mut sc.frame());
                frames.push((k, cx.disp, veils(&sc)));
            }
            frames
        };
        let row5_top = i32::from(g.origin_y) + 5 * g.ch as i32;
        let inked =
            (i32::from(g.origin_x) + 25 * g.cw as i32)..(i32::from(g.origin_x) + 31 * g.cw as i32);
        let red = run();
        assert!(red[0].2.is_empty(), "a cold key laid a veil");
        let peak = |vs: &[RainHalo]| vs.iter().map(|h| lum(h.color)).max().unwrap_or(0);
        let plain = peak(&red[12].2);
        let full = peak(&red[39].2);
        assert!(
            full > plain && plain > 0,
            "at spine {} the veil is {full}, at spine {} {plain}: it does not rise",
            red[39].1,
            red[12].1
        );
        for (k, _, vs) in &red {
            for h in vs {
                let (y0, y1) = (i32::from(h.y), i32::from(h.y) + i32::from(h.h));
                assert!(
                    y1 <= row5_top && y0 >= row5_top - g.ch as i32 && h.row == 4,
                    "key {k}: a veil at y {y0}..{y1} enters a glyph row (row 5 top {row5_top})"
                );
                assert!(
                    i32::from(h.h) <= (AURORA_TALL_CH * g.ch as f32).ceil() as i32,
                    "key {k}: a veil {} px tall is over 0.35 ch",
                    h.h
                );
                let (x0, x1) = (i32::from(h.x), i32::from(h.x) + i32::from(h.w));
                assert!(
                    x1 - x0 <= g.cw as i32 && (x0 - i32::from(g.origin_x)) % g.cw as i32 == 0,
                    "key {k}: a veil at x {x0}..{x1} is not one cell's own column"
                );
                assert!(
                    x1 <= inked.start || x0 >= inked.end,
                    "key {k}: a veil at x {x0}..{x1} sits over an inked sky cell"
                );
            }
        }
    }

    /// The aurora keeps off a filled cell of the row it lies in (a powerline segment's bar), which
    /// the sky probe sees as free. The control keeps nothing out, and its veils lie on the cells.
    #[test]
    fn the_aurora_keeps_off_a_filled_cell_above() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let bar = (
            i32::from(g.origin_x) + 20 * cw,
            i32::from(g.origin_x) + 36 * cw,
            i32::from(g.origin_y) + 4 * ch,
            i32::from(g.origin_y) + 5 * ch,
        );
        let on_bar = |kept: bool| -> usize {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            if kept {
                sky.keep_out.set([bar]);
            }
            let mut n = 0;
            for k in 0..40u16 {
                let at = key_at(t0, k);
                let mut cx = ctx(at, &c, (5, 10 + k));
                cx.disp = disp_at_12cps(k);
                cx.birth_disp = cx.disp;
                cx.caret_t = 0.0;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                let mut sc = Scratch::default();
                sky.emit(&cx, &mut sc.frame());
                n += veils(&sc)
                    .iter()
                    .filter(|h| {
                        let (x0, y0) = (i32::from(h.x), i32::from(h.y));
                        let (x1, y1) = (x0 + i32::from(h.w), y0 + i32::from(h.h));
                        x0 < bar.1 && bar.0 < x1 && y0 < bar.3 && bar.2 < y1
                    })
                    .count();
            }
            n
        };
        assert!(
            on_bar(false) > 0,
            "control: the free sky's veils lie on the bar"
        );
        assert_eq!(
            on_bar(true),
            0,
            "a veil tints a filled cell of the row above"
        );
    }

    /// The aurora keeps off a lettered or filled cell's box but leaves a word icon's to the face's
    /// shade (`CursorGlow::shade_word_icons`): its veils lie in the icon box's columns, and with
    /// the box cut as a filled cell's (the negative control) none does.
    #[test]
    fn the_aurora_leaves_a_word_icon_to_its_face_s_shade() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let icon = (
            i32::from(g.origin_x) + 20 * cw,
            i32::from(g.origin_x) + 24 * cw,
            i32::from(g.origin_y) + 4 * ch,
            i32::from(g.origin_y) + 5 * ch,
        );
        let in_box = |as_cell: bool| -> usize {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            if as_cell {
                sky.keep_out.set([icon]);
            } else {
                sky.keep_out.set_split([icon], []);
            }
            let mut n = 0;
            for k in 0..40u16 {
                let at = key_at(t0, k);
                let mut cx = ctx(at, &c, (5, 10 + k));
                cx.disp = disp_at_12cps(k);
                cx.birth_disp = cx.disp;
                cx.caret_t = 0.0;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                let mut sc = Scratch::default();
                sky.emit(&cx, &mut sc.frame());
                n += veils(&sc)
                    .iter()
                    .filter(|h| {
                        let (x0, y0) = (i32::from(h.x), i32::from(h.y));
                        let (x1, y1) = (x0 + i32::from(h.w), y0 + i32::from(h.h));
                        x0 < icon.1 && icon.0 < x1 && y0 < icon.3 && icon.2 < y1
                    })
                    .count();
            }
            n
        };
        assert!(
            in_box(false) > 0,
            "the aurora's veils are dropped whole in a word icon's box"
        );
        assert_eq!(
            in_box(true),
            0,
            "control: cut as a filled cell, the box still takes a veil"
        );
    }

    #[test]
    fn the_aurora_is_not_drawn_on_paper() {
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let veiled = |dark: bool| -> usize {
            let mut c = cfg(dark);
            if !dark {
                (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
            }
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let mut n = 0;
            for k in 0..40u16 {
                let at = key_at(t0, k);
                let mut cx = ctx(at, &c, (5, 10 + k));
                cx.disp = disp_at_12cps(k);
                cx.birth_disp = cx.disp;
                cx.caret_t = 0.0;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                let mut sc = Scratch::default();
                sky.emit(&cx, &mut sc.frame());
                let g = geom();
                n += sc
                    .halos
                    .iter()
                    .filter(|h| {
                        f32::from(h.rx) >= AURORA_REACH_CW * g.cw as f32 - 1.0
                            && h.rx > h.ry
                            && i32::from(h.w) <= g.cw as i32
                            && (i32::from(h.x) - i32::from(g.origin_x)) % g.cw as i32 == 0
                            && i32::from(h.h) <= (AURORA_TALL_CH * g.ch as f32).ceil() as i32
                    })
                    .count();
            }
            n
        };
        assert!(veiled(true) > 0, "control: the dark aurora draws");
        assert_eq!(veiled(false), 0, "a veil strip on paper");
    }

    #[test]
    fn the_aurora_leaves_with_the_ribbon() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let hot = |sky: &mut Stardust, keys: u16| -> Instant {
            let mut last = t0;
            for k in 0..keys {
                last = key_at(t0, k);
                let mut cx = ctx(last, &c, (5, 10 + k));
                cx.disp = 1.0;
                cx.birth_disp = 1.0;
                cx.caret_t = 0.0;
                sky.budget.refill(last);
                sky.on_event(&typed(TypedClass::Glyph), last, &cx, &ribbon);
            }
            last
        };
        let peak_at = |sky: &mut Stardust, now: Instant| -> u32 {
            let mut sc = Scratch::default();
            sky.emit(&ctx(now, &c, (5, 29)), &mut sc.frame());
            veils(&sc).iter().map(|h| lum(h.color)).max().unwrap_or(0)
        };
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let last = hot(&mut sky, 20);
        let at = |ms: u64| last + Duration::from_millis(ms);
        let (p0, p900) = (peak_at(&mut sky, at(0)), peak_at(&mut sky, at(900)));
        assert!(p0 > 0, "no veil on the last key");
        assert_eq!(p900, 0, "the veil outlived the ribbon's grace");
        assert!(
            (RETRACT_START_S * 1000.0) as u64 >= 900,
            "the fixture's 900 ms is not before the retract"
        );
        assert!(sky.at_rest(), "the veil is still in the pool");
        assert!(sky.next_change_deadline(at(900)).is_none());

        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let last = hot(&mut sky, 10);
        let erase = last + Duration::from_millis(20);
        sky.on_event(&Event::Erase, erase, &ctx(erase, &c, (5, 19)), &ribbon);
        let mut sc = Scratch::default();
        let done = erase + Duration::from_millis(240);
        sky.emit(&ctx(done, &c, (5, 19)), &mut sc.frame());
        let col_of = |h: &RainHalo| (i32::from(h.x) - i32::from(g.origin_x)) / g.cw as i32;
        let cols: std::collections::BTreeSet<i32> = veils(&sc).iter().map(col_of).collect();
        assert!(
            !cols.contains(&19),
            "the erased cell's veil survived the ribbon's retract: {cols:?}"
        );
        assert!(
            cols.contains(&10),
            "a cell before the caret lost its veil to the Backspace: {cols:?}"
        );
    }

    #[test]
    fn the_combo_ladder_pays_at_sixteen_thirty_two_and_sixty_four() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        type Paid = (StarClass, u32, bool);
        let mut rung: Vec<(u32, Option<Paid>)> = Vec::new();
        for k in 0..70u16 {
            let at = key_at(t0, k);
            let combo = u32::from(k) + 1;
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            cx.flow = Flow {
                heat: 1.0,
                combo,
                best: combo,
            };
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let best = sky
                .live_iter()
                .filter(|s| s.born == at && s.lane == StarLane::Strike)
                .min_by_key(|s| s.class)
                .map(|s| (s.class, s.tint, s.gold));
            rung.push((combo, best));
        }
        let paid = |combo: u32| -> Paid {
            rung.iter()
                .find(|(n, _)| *n == combo)
                .and_then(|(_, s)| *s)
                .unwrap_or_else(|| panic!("key {combo} of an open run drew NO strike star at all"))
        };
        for n in [LADDER_M2_KEYS, LADDER_M2_KEYS * 3] {
            let (class, _, _) = paid(n);
            assert!(
                class <= StarClass::M2,
                "key {n} paid a {class:?}, not the ladder's m2 or better"
            );
        }
        let (class, tint, gold) = paid(LADDER_WHITE_KEYS);
        assert_eq!(class, StarClass::M2, "the 32nd key pays an m2");
        assert_eq!(
            tint, TINT_WHITE_RGB,
            "…and it is WHITE by rule, not by deal"
        );
        assert!(!gold, "a white rung carries no glints");
        let (class, tint, gold) = paid(LADDER_GOLD_KEYS);
        assert_eq!(class, StarClass::M1, "the 64th key pays a hero");
        assert_eq!(tint, TINT_GOLD_RGB, "…gold by rule");
        assert!(gold, "…and gold-ness is the class that carries glints");

        let plain = rung
            .iter()
            .filter(|(n, _)| !n.is_multiple_of(LADDER_M2_KEYS))
            .filter(|(_, s)| s.is_none_or(|(c, _, _)| c == StarClass::M3))
            .count();
        assert!(
            plain > 20,
            "only {plain} of the non-rung keys took a grain or nothing — the ladder is not a payout, the deal moved"
        );
    }

    #[test]
    fn an_open_theme_holds_the_sky_open_through_a_dip() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let census = |disp: f32, heat: f32| -> (f32, f32, usize) {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let mut born = 0usize;
            for k in 0..60u16 {
                let at = key_at(t0, k);
                let mut cx = ctx(at, &c, (5, 10 + k));
                cx.disp = disp;
                cx.birth_disp = disp;
                cx.flow = Flow {
                    heat,
                    combo: if heat > 0.0 { FLOW_ENTRY_KEYS } else { 0 },
                    best: FLOW_ENTRY_KEYS,
                };
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                born += sky.live_iter().filter(|s| s.born == at).count();
            }
            let field: std::collections::BTreeSet<i32> = sky
                .live_iter()
                .filter(|s| s.lane == StarLane::Field)
                .map(col_of)
                .collect();
            let held = (53..69).filter(|col| field.contains(col)).count();
            (born as f32 / 60.0, held as f32 / 16.0, sky.live())
        };
        let dip = FIELD_HOT_DISP - 0.05;
        let cold = census(dip, 0.0);
        let flowing = census(dip, 1.0);
        let hot = census(0.9, 0.0);
        assert!(
            flowing.1 >= 0.999,
            "an open theme left {:.2} of the last sixteen cells without a field star",
            1.0 - flowing.1
        );
        assert!(
            flowing.1 > cold.1 + 0.2 && flowing.2 > cold.2,
            "an open theme bought no density in the dip: field-held {:.2} → {:.2}, live {} → {}",
            cold.1,
            flowing.1,
            cold.2,
            flowing.2
        );
        assert!(
            (flowing.1 - hot.1).abs() < 0.01 && flowing.2 == hot.2,
            "an open theme's sky at a dip (field-held {:.2}, live {}) is not the hot step's ({:.2}, {})",
            flowing.1,
            flowing.2,
            hot.1,
            hot.2
        );
        assert!(
            flowing.2 <= STAR_CAP,
            "the open sky ran past the live cap: {} of {STAR_CAP}",
            flowing.2
        );
    }

    #[test]
    fn one_sky_star_per_cell_and_no_pile_outshines_the_caret() {
        let mut c = cfg(true);
        c.theme_bg = SHIPPED_GROUND;
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let (mut worst, mut over) = (0.0f32, 0usize);
        let mut piles = Vec::new();
        for k in 0..40u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            cx.caret_t = f32::from(k) / 40.0;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            let mut cells = std::collections::BTreeMap::new();
            for s in sky.live_iter().filter(|s| s.lane.is_sky()) {
                *cells.entry(col_of(s)).or_insert(0usize) += 1;
            }
            piles.extend(
                cells
                    .iter()
                    .filter(|&(_, &n)| n > 1)
                    .map(|(&col, &n)| (k, col, n)),
            );
            let peak = composite_over(&sc, SHIPPED_GROUND, 0, 94, 1080, 54)
                .iter()
                .map(|&p| relative_luminance(p) * 255.0)
                .fold(0.0, f32::max);
            over += usize::from(peak > ceil);
            worst = worst.max(peak);
        }
        assert!(
            piles.is_empty(),
            "a cell carries more than one sky star: {piles:?}"
        );
        assert!(
            over == 0,
            "{over} of 40 frames reach past the caret law's {ceil:.1} — worst Y {worst:.1}"
        );
    }

    #[test]
    fn no_mark_crosses_ink_or_the_pane_edge_at_any_instant_under_either_band_anchor() {
        for tall in [true, false] {
            let mut c = cfg(true);
            c.ribbon_tall = tall;
            let t0 = Instant::now();
            let g = geom_cell(15, 28);
            let cw = g.cw as f32;
            let ribbon = Ribbon::new();
            let priced = stream_travel_px(stream_px_per_s_with_share(
                EXHAUST_SHARE_CW,
                cw,
                1000.0 / 12.0,
            )) * stream_unit(EXHAUST_THROW_MS);
            let sky_travel = stream_travel_px(stream_px_per_s(cw, 1000.0 / 12.0));
            let sweep = |sky: &Stardust, s: &Star, at: Instant, min_col: i32| -> f32 {
                let (x0, _) = s.pos(at, false);
                let mut x_end = x0;
                for step in 0..=250u64 {
                    let now = at + Duration::from_millis(step * 2);
                    if s.dead(now) {
                        break;
                    }
                    let (x, y) = s.pos(now, false);
                    x_end = x;
                    assert_eq!(
                        sky.probe().at_px(x, y, g),
                        Some(false),
                        "tall {tall}: a {:?} mark stood on ink or the unknown at ({x}, {y}), \
                         +{} ms",
                        s.lane,
                        step * 2
                    );
                    assert!(
                        px_col(x as f32, g) >= min_col,
                        "tall {tall}: a {:?} mark stood over column {} at +{} ms — the floor \
                         is column {min_col}",
                        s.lane,
                        px_col(x as f32, g),
                        step * 2
                    );
                    assert!(
                        x >= g.fx_left(),
                        "tall {tall}: a {:?} mark stood at x {x}, left of the effects box's {} \
                         px edge, +{} ms",
                        s.lane,
                        g.fx_left(),
                        step * 2
                    );
                    if s.lane == StarLane::Exhaust {
                        assert!(
                            in_sky_of_row(y as f32, 5, g),
                            "tall {tall}: a grain left the caret row's sky at y {y}, +{} ms",
                            step * 2
                        );
                    }
                }
                (x0 - x_end) as f32
            };
            let mut sky = Stardust::new();
            let mut occupied = [false; 120];
            let (mut marks, mut grains, mut clamped, mut sky_clamped) = (0usize, 0, 0, 0);
            let mut lanes: Vec<StarLane> = Vec::new();
            let mut widest = 0.0f32;
            for k in 0..20u16 {
                let col = 40 + k;
                for slot in occupied.iter_mut().take(usize::from(col) - 1) {
                    *slot = true;
                }
                sky.probe_mut().probe_row(4, &[false; 120]);
                sky.probe_mut().probe_row(band_row(&c), &occupied);
                let at = t0 + Duration::from_secs_f32(f32::from(k) / 12.0);
                let mut cx = ctx_in(g, at, &c, (5, col));
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                let floor = (g.fx_left() + (i32::from(col) - 1) * g.cw as i32) as f32;
                for s in sky.live_iter().filter(|s| s.born == at) {
                    marks += 1;
                    if !lanes.contains(&s.lane) {
                        lanes.push(s.lane);
                    }
                    let drift = sweep(&sky, s, at, i32::from(col) - 1);
                    widest = widest.max(drift);
                    let (x0, _) = s.pos(at, false);
                    if s.lane == StarLane::Exhaust {
                        grains += 1;
                        let travel = (x0 - s.rest().0) as f32;
                        assert!(
                            travel > 0.0,
                            "tall {tall}: a grain over an inked row did not move at all"
                        );
                        clamped += usize::from(travel < priced - 1.0);
                    } else if x0 as f32 - sky_travel < floor {
                        sky_clamped += 1;
                    }
                }
            }
            assert!(
                marks >= 20 && lanes.len() >= 2,
                "tall {tall}: the fixture carried {marks} marks of {} lanes — too thin to be a \
                 census of the sky",
                lanes.len()
            );
            assert!(
                widest >= cw,
                "tall {tall}: the widest drift over the whole run was {widest:.1} px — under a \
                 cell, so the sweep is not reading a moving sky at all"
            );
            assert!(
                grains >= 15,
                "tall {tall}: the inked fixture threw {grains} grains over twenty keys — the \
                 clamp is dropping them, not shortening them"
            );
            assert_eq!(
                clamped, grains,
                "tall {tall}: only {clamped} of {grains} grains were clamped short of the \
                 priced {priced:.1} px: the frontier is not what is stopping them"
            );
            assert!(
                sky_clamped > 0,
                "tall {tall}: no sky star was born within its travel of the ink — the sky's \
                 clamp was not exercised"
            );
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            sky.probe_mut().probe_row(5, &[false; 120]);
            let (mut edge_grains, mut edge_sky) = (0usize, 0usize);
            for k in 0..8u16 {
                let at = t0 + Duration::from_secs_f32(f32::from(k) / 12.0);
                let mut cx = ctx_in(g, at, &c, (5, k));
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                for s in sky.live_iter().filter(|s| s.born == at) {
                    sweep(&sky, s, at, 0);
                    let (x0, _) = s.pos(at, false);
                    let travel = if s.lane == StarLane::Exhaust {
                        priced
                    } else {
                        sky_travel
                    };
                    if (x0 as f32 - travel) < g.fx_left() as f32 {
                        if s.lane == StarLane::Exhaust {
                            edge_grains += 1;
                        } else {
                            edge_sky += 1;
                        }
                    }
                }
            }
            assert!(
                edge_grains > 0 && edge_sky > 0,
                "tall {tall}: {edge_grains} grains and {edge_sky} sky stars were born within \
                 their travel of the pane's edge — the edge clamp was not exercised"
            );
        }
    }

    #[test]
    fn the_sky_is_at_rest_one_star_life_after_the_last_key() {
        let c = cfg(true);
        let t0 = Instant::now();
        let (sky, _, _) = exhaust_run(12.0, 24, 30, 0, &c, t0);
        let last = t0 + Duration::from_secs_f32(23.0 / 12.0);
        let early = last + Duration::from_millis(100);
        let later = last + Duration::from_millis(200);
        let streaming = sky
            .live_iter()
            .filter(|s| s.pos(early, false).0 != s.pos(later, false).0)
            .count();
        assert!(
            streaming > 0,
            "nothing in the sky is streaming 100 ms after the last key"
        );
        let rest = last + Duration::from_secs_f32(STREAM_END_MS / 1000.0);
        for s in sky.live_iter() {
            let at_rest = s.pos(rest, false);
            for ms in [1u64, 50, 200, 1_000, 3_000] {
                let p = s.pos(rest + Duration::from_millis(ms), false);
                assert_eq!(
                    p.0, at_rest.0,
                    "a star still moves {ms} ms after the sky should be at rest ({:?} → {:?})",
                    at_rest, p
                );
            }
            assert!(
                s.next_stream_step_s(rest).is_none(),
                "a star at rest still offers a stream step"
            );
        }
    }

    #[test]
    fn the_stream_step_is_solved_not_polled() {
        let t0 = Instant::now();
        let mut s = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
        s.stream = stream_travel_px(stream_px_per_s(15.0, 1000.0 / 12.0));
        let mut steps = 0usize;
        let mut age_ms = 0u64;
        while age_ms < STREAM_END_MS as u64 + 100 {
            let now = t0 + Duration::from_millis(age_ms);
            let Some(dt) = s.next_stream_step_s(now) else {
                break;
            };
            let at = now + Duration::from_secs_f32(dt);
            let before = s.pos(at - Duration::from_millis(1), false).0;
            let after = s.pos(at + Duration::from_millis(1), false).0;
            assert!(
                before != after,
                "the solved step at +{dt:.4} s from {age_ms} ms moves nothing ({before} → {after})"
            );
            assert!(dt >= 0.0, "a negative step {dt} from {age_ms} ms");
            steps += 1;
            age_ms = (age_ms as f32 + dt * 1000.0).floor() as u64 + 2;
        }
        assert!(
            steps == s.stream.round() as usize,
            "{steps} solved steps for a {:.1} px travel — one per pixel expected",
            s.stream
        );
        let done = t0 + Duration::from_secs_f32(STREAM_END_MS / 1000.0);
        assert!(
            s.next_stream_step_s(done).is_none(),
            "a step offered past the drift's end"
        );
        assert_eq!(
            s.pos(done, false).0,
            200 - s.stream.round() as i32,
            "the star did not travel its whole priced distance"
        );
    }

    fn exhaust_run(
        cps: f32,
        keys: u16,
        col0: u16,
        ink_to: usize,
        c: &Config,
        t0: Instant,
    ) -> (Stardust, Geom, Vec<(Star, Instant)>) {
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        let mut occupied = [false; 120];
        for slot in occupied.iter_mut().take(ink_to) {
            *slot = true;
        }
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.probe_mut().probe_row(band_row(c), &occupied);
        let mut born = Vec::new();
        for k in 0..keys {
            let at = t0 + Duration::from_secs_f32(f32::from(k) / cps);
            let mut cx = ctx_in(g, at, c, (5, col0 + k));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            born.extend(
                sky.live_iter()
                    .filter(|s| s.born == at && s.lane == StarLane::Exhaust)
                    .map(|s| (*s, at)),
            );
        }
        (sky, g, born)
    }

    /// The row a sky star of row 5 is born in: the row above, under either anchor.
    const fn band_row(_: &Config) -> i32 {
        4
    }

    fn exhaust_census(
        class: TypedClass,
        cps: f32,
        disp: f32,
        dark: bool,
        reduced: bool,
        keys: u16,
        t0: Instant,
    ) -> (usize, usize, f32, usize, usize) {
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut c = cfg(dark);
        c.reduced_motion = reduced;
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let (mut n, mut m2, mut inked) = (0usize, 0usize, 0usize);
        for k in 0..keys {
            let at = t0 + Duration::from_secs_f32(f32::from(k) / cps);
            let mut cx = ctx_in(g, at, &c, (5, 40 + k));
            cx.disp = disp;
            cx.birth_disp = disp;
            sky.budget.refill(at);
            sky.on_event(&typed(class), at, &cx, &ribbon);
            for s in sky
                .live_iter()
                .filter(|s| s.born == at && s.lane == StarLane::Exhaust)
            {
                n += 1;
                m2 += usize::from(s.class == StarClass::M2);
                inked += usize::from(s.inks(&c));
            }
        }
        let glints = sky.take_glints().count();
        (n, m2, sky.budget.tokens(), glints, inked)
    }

    fn exhaust_duty_in(
        c: &Config,
        row_4: CellInk,
        cps: f32,
        keys: u16,
    ) -> (u32, u32, usize, usize) {
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row_ink(4, &[row_4; 120]);
        sky.probe_mut().probe_row(5, &[false; 120]);
        let span_ms = (1000.0 * f32::from(keys - 1) / cps) as u64 + 300;
        let (mut brisk_ms, mut bare_ms) = (0u64, 0u64);
        let (mut max_live, mut evicted, mut prev_fin) = (0usize, 0usize, 0usize);
        let mut next_key = 0u16;
        for ms in 0..span_ms {
            let now = t0 + Duration::from_millis(ms);
            while next_key < keys && t0 + Duration::from_secs_f32(f32::from(next_key) / cps) <= now
            {
                let mut cx = ctx_in(g, now, c, (5, 20 + (next_key % 90)));
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                sky.budget.refill(now);
                sky.on_event(&typed(TypedClass::Glyph), now, &cx, &ribbon);
                next_key += 1;
                let fin = sky.live_iter().filter(|s| s.finish.is_some()).count();
                evicted += fin.saturating_sub(prev_fin);
                prev_fin = fin;
                max_live = max_live.max(sky.live_iter().filter(|s| s.finish.is_none()).count());
            }
            brisk_ms += u64::from(sky.brisk(now));
            bare_ms += u64::from(
                sky.stars
                    .iter()
                    .any(|s| s.lane != StarLane::Exhaust && s.brisk(now))
                    || sky.veil.iter().any(|v| v.age_s(now) < EDGE_IN_S),
            );
        }
        let pct = |ms: u64| (ms * 100 / span_ms.max(1)) as u32;
        (pct(brisk_ms), pct(bare_ms), max_live, evicted)
    }

    #[test]
    fn the_exhaust_never_chimes_skips_a_space_and_deals_its_m2_only_at_a_hot_spine() {
        let t0 = Instant::now();
        let hot = exhaust_census(TypedClass::Glyph, 8.0, 0.9, true, false, 12, t0);
        assert_eq!(
            hot.0, 11,
            "a hot 8 cps hand threw {} grains over twelve keys, not one each from the second",
            hot.0
        );
        let no_grain = exhaust_census(TypedClass::Glyph, 8.0, 0.9, true, true, 12, t0);
        assert_eq!(
            no_grain.0, 0,
            "the no-grain control (reduced motion) sowed {} grains",
            no_grain.0
        );
        assert!(
            (hot.2 - no_grain.2).abs() < 1e-6,
            "eleven grains cost the glint bucket {} tokens: the exhaust is a THROW and spends \
             none (§13 — one keystroke is one melody step)",
            no_grain.2 - hot.2
        );
        assert!(
            !StarLane::Exhaust.may_chime(),
            "the exhaust lane may chime: ONE KEYSTROKE IS ONE MELODY STEP, and the key that \
             threw the grain already has its tine (§13, §19.2)"
        );
        let space = exhaust_census(TypedClass::Space, 8.0, 0.9, true, false, 12, t0);
        assert_eq!(
            space.0, 0,
            "twelve Spaces at a hot spine threw {} grains: a Space lays bed, it does not strike",
            space.0
        );
        assert!(
            (STREAM_DISP..FIELD_HOT_DISP).contains(&0.6),
            "0.6 is no longer between STREAM_DISP {STREAM_DISP} and FIELD_HOT_DISP \
             {FIELD_HOT_DISP}: the warm-but-not-hot band this clause needs has moved"
        );
        let warm = exhaust_census(TypedClass::Glyph, 8.0, 0.6, true, false, 60, t0);
        assert_eq!(
            warm.0, 59,
            "a warm hand (spine 0.6) threw {} grains over sixty keys, not one each from the \
             second: the exhaust's own gate is STREAM_DISP, not FIELD_HOT_DISP",
            warm.0
        );
        assert_eq!(
            warm.1, 0,
            "a warm hand (spine 0.6, under FIELD_HOT_DISP {FIELD_HOT_DISP}) dealt {} m2s: the \
             m2 is the HOT deal",
            warm.1
        );
        let dark = exhaust_census(TypedClass::Glyph, 8.0, 0.9, true, false, 60, t0);
        assert_eq!(
            dark.0, 59,
            "sixty keys on a dark theme threw {} grains, not one each from the second",
            dark.0
        );
        assert!(
            dark.1 > 0,
            "sixty grains at a hot spine dealt no m2 at all (1 in {EXHAUST_M2_IN}): the warm \
             clause above is vacuous"
        );
        let light = exhaust_census(TypedClass::Glyph, 8.0, 0.9, false, false, 60, t0);
        assert_eq!(
            light.0, dark.0,
            "the light theme threw {} grains where the dark threw {}: the thinning is the \
             ink's, not the throw's",
            light.0, dark.0
        );
        assert_eq!(dark.4, dark.0, "a dark grain went uninked");
        let share = light.4 as f32 / light.0 as f32;
        eprintln!(
            "the light theme inked {} of its {} grains — {share:.2}",
            light.4, light.0
        );
        assert!(
            (0.45..=0.80).contains(&share),
            "the light share is {share:.2}, not about {LIGHT_COUNT_SCALE}"
        );
    }

    #[test]
    fn the_exhaust_costs_the_frame_train_its_own_window_and_no_more() {
        let c = cfg(true);
        let t0 = Instant::now();
        let (sky, _, born) = exhaust_run(12.0, 16, 40, 0, &c, t0);
        let last = born.last().expect("the run threw grains").1;
        let at = |ms: u64| last + Duration::from_millis(ms);
        let edge_in_ms = (EDGE_IN_S * 1000.0).ceil() as u64 + 2;
        assert!(
            sky.brisk(at(edge_in_ms)),
            "the sky went quiet {edge_in_ms} ms after the key: the grain is not holding the train"
        );
        assert!(
            sky.brisk(at(EXHAUST_THROW_MS as u64 - 1)),
            "the sky went quiet before the grain's throw was over"
        );
        assert!(
            !sky.brisk(at(EXHAUST_THROW_MS as u64 + 1)),
            "the sky is still brisk after the last grain's throw: the window does not close"
        );
        let (_, gf, fast_born) = exhaust_run(30.0, 60, 20, 0, &c, t0);
        let cw = gf.cw as f32;
        let capped = WALK_FAST_CELLS * cw * stream_unit(EXHAUST_THROW_MS);
        let uncapped = stream_travel_px(stream_px_per_s_with_share(
            EXHAUST_SHARE_CW,
            cw,
            1000.0 / 30.0,
        )) * stream_unit(EXHAUST_THROW_MS);
        assert!(
            uncapped > capped + cw,
            "at 30 cps the priced travel is {uncapped:.1} px and the cap {capped:.1}: the cap \
             is not binding, so the clause below measures nothing"
        );
        assert!(!fast_born.is_empty(), "the 30 cps fixture threw nothing");
        for (s, at) in &fast_born {
            let travel = (s.pos(*at, false).0 - s.rest().0) as f32;
            assert!(
                (travel - capped).abs() < 1.5,
                "a 30 cps grain travelled {travel:.1} px where the cap is {capped:.1} \
                 (the priced, uncapped travel is {uncapped:.1})"
            );
        }
        let expect = |cps: f32| -> (u32, u32) {
            let ioi = 1000.0 / cps;
            let edge = EDGE_IN_S * 1000.0;
            let span = (1000.0 * 59.0 / cps).floor() + 300.0;
            let held = if ioi < EXHAUST_THROW_MS {
                58.0 * ioi + EXHAUST_THROW_MS
            } else {
                59.0 * EXHAUST_THROW_MS
            } + edge;
            let bare = 60.0 * edge.min(ioi);
            ((held * 100.0 / span) as u32, (bare * 100.0 / span) as u32)
        };
        for dark in [true, false] {
            let c = cfg(dark);
            let theme = if dark { "dark" } else { "light" };
            for cps in [4.0f32, 12.0] {
                let (on, off, live, evicted) = exhaust_duty_in(&c, CellInk::Blank, cps, 60);
                let (want_on, want_off) = expect(cps);
                eprintln!(
                    "{theme}, {cps} cps, 60 keys: brisk {on}% of the session with the exhaust \
                     (arithmetic {want_on}%), {off}% without it (arithmetic {want_off}%); \
                     {live} live of {STAR_CAP}, {evicted} evictions"
                );
                assert_eq!(
                    evicted, 0,
                    "{theme}, {cps} cps evicted {evicted} stars, so the no-exhaust baseline is \
                     not a counterfactual: the exhaust displaced a star the baseline keeps"
                );
                assert!(
                    on.abs_diff(want_on) <= 2,
                    "{theme}, {cps} cps holds the frame train {on}% of the session where its \
                     window arithmetic says {want_on}%: the exhaust costs more, or less, than \
                     its window"
                );
                assert!(
                    off.abs_diff(want_off) <= 2,
                    "{theme}, {cps} cps was brisk {off}% of the session with no grain in the \
                     pool, where the edge-ins alone say {want_off}%: the BASELINE has moved"
                );
            }
        }
        for (label, disp, cps, reduced) in [
            ("reduced motion", 0.9f32, 12.0f32, true),
            ("a cold spine", 0.3, 12.0, false),
            ("a 2 cps stroll", 0.9, 2.0, false),
        ] {
            let mut c = cfg(true);
            c.reduced_motion = reduced;
            let g = geom_cell(15, 28);
            let ribbon = Ribbon::new();
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let mut last = t0;
            for k in 0..16u16 {
                last = t0 + Duration::from_secs_f32(f32::from(k) / cps);
                let mut cx = ctx_in(g, last, &c, (5, 40 + k));
                cx.disp = disp;
                cx.birth_disp = disp;
                sky.budget.refill(last);
                sky.on_event(&typed(TypedClass::Glyph), last, &cx, &ribbon);
            }
            assert_eq!(
                sky.live_iter()
                    .filter(|s| s.lane == StarLane::Exhaust)
                    .count(),
                0,
                "{label} sowed an exhaust grain"
            );
            assert!(
                reduced || !sky.brisk(last + Duration::from_millis(edge_in_ms)),
                "{label} owed the engine a frame-cadence wake {edge_in_ms} ms after the key, \
                 past the field's own edge-in"
            );
        }
    }

    #[test]
    fn the_exhaust_needs_a_blank_cell_behind_the_caret() {
        let t0 = Instant::now();
        let edge_in_ms = (EDGE_IN_S * 1000.0).ceil() as u64 + 2;
        let run = |c: &Config, probe: &dyn Fn(&mut Stardust, u16)| {
            let g = geom_cell(15, 28);
            let ribbon = Ribbon::new();
            let mut sky = Stardust::new();
            let mut grains = 0usize;
            let mut last = t0;
            for k in 0..16u16 {
                let col = 40 + k;
                probe(&mut sky, col);
                last = t0 + Duration::from_secs_f32(f32::from(k) / 12.0);
                let mut cx = ctx_in(g, last, c, (5, col));
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                sky.budget.refill(last);
                sky.on_event(&typed(TypedClass::Glyph), last, &cx, &ribbon);
                grains += sky
                    .live_iter()
                    .filter(|s| s.born == last && s.lane == StarLane::Exhaust)
                    .count();
            }
            (grains, sky.brisk(last + Duration::from_millis(edge_in_ms)))
        };
        let text_to = |sky: &mut Stardust, col: u16, gap: u16| {
            let mut cells = [false; 120];
            for slot in cells.iter_mut().take(usize::from(col.saturating_sub(gap))) {
                *slot = true;
            }
            sky.probe_mut().probe_row(5, &cells);
        };
        let tall = cfg(true);
        let (grains, brisk) = run(&tall, &|sky, col| {
            let mut above = [true; 120];
            above[usize::from(col)] = false;
            sky.probe_mut().probe_row(4, &above);
            text_to(sky, col, 0);
        });
        assert_eq!(
            grains, 0,
            "tall with the row above inked but for the caret's own column threw {grains} \
             grains: every one of them was a sub-cell twitch"
        );
        assert!(
            !brisk,
            "tall with the row above inked but for the caret's own column owed the engine a \
             frame-cadence wake {edge_in_ms} ms after the key, with nothing thrown to show for it"
        );
        let (grains, _) = run(&tall, &|sky, col| {
            sky.probe_mut().probe_row(4, &[false; 120]);
            text_to(sky, col, 0);
        });
        assert_eq!(
            grains, 15,
            "tall under a blank row threw {grains} grains, not one per key from the second"
        );
        // The underline's sky is the row above's foot, as tall's is
        // (`sky_birth_px`), so its grains are tall's.
        let mut under = cfg(true);
        under.ribbon_tall = false;
        let (grains, _) = run(&under, &|sky, col| {
            sky.probe_mut().probe_row(4, &[false; 120]);
            text_to(sky, col, 0);
        });
        assert_eq!(
            grains, 15,
            "underline under a blank row threw {grains} grains, not one per key from the second"
        );
        let (grains, brisk) = run(&under, &|sky, col| {
            let mut above = [true; 120];
            above[usize::from(col)] = false;
            sky.probe_mut().probe_row(4, &above);
            text_to(sky, col, 0);
        });
        assert_eq!(
            grains, 0,
            "underline with the row above inked but for the caret's own column threw {grains} \
             grains"
        );
        assert!(
            !brisk,
            "underline with the row above inked owed a frame-cadence wake {edge_in_ms} ms after \
             the key"
        );
        let glyph_under_caret = |sky: &mut Stardust, col: u16| {
            let mut cells = [true; 120];
            cells[usize::from(col) - 1] = false;
            sky.probe_mut().probe_row(4, &[false; 120]);
            sky.probe_mut().probe_row(5, &cells);
        };
        for (c, side) in [(&under, "underline"), (&tall, "tall")] {
            assert_eq!(
                run(c, &glyph_under_caret).0,
                15,
                "{side} over a blank row threw no grain past a caret over a glyph"
            );
        }
    }

    #[test]
    fn the_exhaust_flies_along_a_rule_row_in_the_sky_zone_under_the_stroke() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        type Born = Vec<(Star, Instant)>;
        let run = |row_4: &[CellInk; 120]| -> (Stardust, Born, Born) {
            let ribbon = Ribbon::new();
            let mut sky = Stardust::new();
            let (mut grains, mut stars) = (Vec::new(), Vec::new());
            for k in 0..60u16 {
                let col = 20 + k;
                let mut line = [false; 120];
                for slot in line.iter_mut().take(usize::from(col)) {
                    *slot = true;
                }
                sky.probe_mut().probe_row_ink(4, row_4);
                sky.probe_mut().probe_row(5, &line);
                let at = t0 + Duration::from_secs_f32(f32::from(k) / 12.0);
                let mut cx = ctx_in(g, at, &c, (5, col));
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                for s in sky.live_iter().filter(|s| s.born == at) {
                    if s.lane == StarLane::Exhaust {
                        grains.push((*s, at));
                    } else if s.lane.is_sky() {
                        stars.push((*s, at));
                    }
                }
            }
            (sky, grains, stars)
        };
        let travel = |v: &[(Star, Instant)]| -> Vec<f32> {
            v.iter()
                .map(|(s, at)| (s.pos(*at, false).0 - s.rest().0) as f32)
                .collect()
        };
        let band = rule_stroke_px(g.ch);
        let row_4_top = i32::from(g.origin_y) + 4 * g.ch as i32;
        let sample = |pool: &Stardust, marks: &[(Star, Instant)]| -> (usize, usize, usize, usize) {
            let (mut samples, mut in_band, mut on_ink, mut rests) =
                (0usize, 0usize, 0usize, 0usize);
            for (s, at) in marks {
                for step in 0..=1000u64 {
                    let now = *at + Duration::from_millis(step * 2);
                    if s.dead(now) {
                        break;
                    }
                    let (x, y) = s.pos(now, false);
                    samples += 1;
                    let ruled = u16::try_from(px_col(x as f32, g))
                        .is_ok_and(|col| pool.probe().rule_at(4, col));
                    in_band += usize::from(
                        ruled && px_row(y as f32, g) == 4 && band.contains(&(y - row_4_top)),
                    );
                    on_ink += usize::from(pool.probe().at_px(x, y, g) != Some(false));
                    assert!(
                        in_sky_of_row(y as f32, 5, g),
                        "a {:?} mark left the caret row's sky at y {y}, +{} ms",
                        s.lane,
                        step * 2
                    );
                }
                let rest = s.rest();
                rests += usize::from(pool.probe().at_px(rest.0, rest.1, g) == Some(true));
            }
            (samples, in_band, on_ink, rests)
        };
        let (sky, grains, stars) = run(&[CellInk::Rule; 120]);
        let (_, blank_grains, blank_stars) = run(&[CellInk::Blank; 120]);
        let travels = travel(&grains);
        assert_eq!(
            grains.len(),
            59,
            "sixty keys under a rule row threw {} grains, not one per key from the second",
            grains.len()
        );
        assert_eq!(
            grains.len(),
            blank_grains.len(),
            "the rule row threw {} grains where the blank row threw {}",
            grains.len(),
            blank_grains.len()
        );
        for (k, t) in travels.iter().enumerate() {
            assert!(
                *t >= 3.0 * cw,
                "grain {k} ran {t:.1} px along the rule row — under three cells"
            );
        }
        let (streams, blank_streams) = (travel(&stars), travel(&blank_stars));
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
        let max = |v: &[f32]| v.iter().copied().fold(0.0f32, f32::max);
        assert!(
            streams.len() == blank_streams.len() && !streams.is_empty(),
            "the rule row bore {} sky stars where the blank row bore {}",
            streams.len(),
            blank_streams.len()
        );
        assert!(
            (mean(&streams) - mean(&blank_streams)).abs() <= 1.0
                && (max(&streams) - max(&blank_streams)).abs() <= 1.0,
            "the sky streams a mean {:.1} px (max {:.1}) along a rule row against {:.1} (max \
             {:.1}) over a blank row: the walk still calls a rule ink",
            mean(&streams),
            max(&streams),
            mean(&blank_streams),
            max(&blank_streams)
        );
        let (samples, in_band, on_ink, rests) = sample(&sky, &grains);
        let (star_samples, star_in_band, star_on_ink, star_rests) = sample(&sky, &stars);
        let min_rise = (0.5 * EXHAUST_RISE_CH * ch).round() as i32;
        let clamped = grains
            .iter()
            .filter(|(s, at)| s.pos(*at, false).1 - s.rest().1 < min_rise)
            .count();
        let full_lift = |s: &Star| {
            (-sky_lift_px_per_s(ch) / 1000.0 * s.class.life_s(s.lane) * 1000.0).round() as i32
        };
        let lifts = |want: &dyn Fn(i32, i32) -> bool| {
            stars
                .iter()
                .filter(|(s, at)| {
                    s.lane == StarLane::Strike
                        && s.class != StarClass::M3
                        && want(s.pos(*at, false).1 - s.rest().1, full_lift(s))
                })
                .count()
        };
        let (held, whole) = (
            lifts(&|lift, full| lift < full),
            lifts(&|lift, full| lift == full),
        );
        assert!(samples > 100, "only {samples} flight samples: nothing flew");
        assert_eq!(
            in_band, 0,
            "{in_band} of {samples} flight samples had the core inside the rule's stroke band \
             {band:?} of row 4"
        );
        assert_eq!(
            on_ink, 0,
            "{on_ink} of {samples} flight samples stood on ink or the unknown"
        );
        assert_eq!(rests, 0, "{rests} grains came to rest on the rule's stroke");
        assert!(
            clamped > 0,
            "no grain rose under the smallest unclamped rise ({min_rise} px): the headroom \
             clamp was not exercised"
        );
        assert!(
            star_samples > 100,
            "only {star_samples} sky samples: no star lived"
        );
        assert_eq!(
            star_in_band, 0,
            "{star_in_band} of {star_samples} sky-star samples had the core inside the rule's \
             stroke band {band:?} of row 4: the strike's lift is not held under the stroke"
        );
        assert_eq!(
            star_on_ink, 0,
            "{star_on_ink} of {star_samples} sky-star samples stood on ink or the unknown"
        );
        assert_eq!(
            star_rests, 0,
            "{star_rests} sky stars came to rest on the rule's stroke"
        );
        assert!(
            held > 0 && whole > 0,
            "of the lifting strikes {held} lift less than their whole lift and {whole} lift it \
             all: the headroom is not being read per birth"
        );
        assert!(
            blank_stars
                .iter()
                .filter(|(s, _)| s.lane == StarLane::Strike && s.class != StarClass::M3)
                .all(|(s, at)| s.pos(*at, false).1 - s.rest().1 == full_lift(s)),
            "a strike over a blank row lifted less than its whole lift"
        );
        assert!(
            grains.iter().all(|(s, _)| s.class != StarClass::M1),
            "the exhaust dealt an m1 along the rule row"
        );
        let mut title = [CellInk::Blank; 120];
        title[..40].fill(CellInk::Rule);
        let (pool, grains, stars) = run(&title);
        let crossed = grains
            .iter()
            .chain(&stars)
            .filter(|(s, at)| {
                px_col(s.pos(*at, false).0 as f32, g) >= 40 && px_col(s.rest().0 as f32, g) < 40
            })
            .count();
        let (_, in_band, on_ink, rests) = sample(&pool, &grains);
        let (_, star_in_band, star_on_ink, star_rests) = sample(&pool, &stars);
        assert!(
            crossed > 0,
            "no mark born over a blank cell crossed onto the rule"
        );
        assert_eq!(
            (
                in_band,
                on_ink,
                rests,
                star_in_band,
                star_on_ink,
                star_rests
            ),
            (0, 0, 0, 0, 0, 0),
            "a mark crossing onto a rule that ends left of the caret entered its stroke: \
             grains {in_band} in band / {on_ink} on ink / {rests} rests, stars {star_in_band} / \
             {star_on_ink} / {star_rests}"
        );
    }

    #[test]
    fn an_exhaust_grain_rests_by_the_probe_and_not_by_its_home_cell() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row_ink(4, &[CellInk::Rule; 120]);
        let cx = ctx_in(g, t0, &c, (5, 40));
        let (x, _) = g.cell_center(4, 40);
        let row_top = f32::from(g.origin_y) + 4.0 * g.ch as f32;
        let y = row_top + (0.62 * g.ch as f32).round();
        for (lane, sown) in [(StarLane::Exhaust, false), (StarLane::Erase, true)] {
            let mut s = star(StarClass::M3, lane, t0, TINT_WHITE_RGB);
            s.x = x.round();
            s.y = y;
            let window = lane.throw_ms().expect("a throw lane");
            s.v0 = (-3.0 / window, -4.0 / window);
            let rest = s.rest();
            assert_eq!(
                sky.probe().at_px(rest.0, rest.1, g),
                Some(true),
                "{lane:?}: the fixture's rest {rest:?} is not on the rule's stroke"
            );
            assert_eq!(
                sky.settle(&mut s, false, &cx),
                sown,
                "{lane:?}: a rest on the stroke was {}",
                if sown { "dropped" } else { "sown" }
            );
            if sown {
                assert_eq!(s.rest(), rest, "{lane:?}: the exempt rest was re-aimed");
            }
        }
    }

    #[test]
    fn the_exhaust_floors_its_spine_on_the_mend_s() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let run = |mended: bool| {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let mut n = 0usize;
            for k in 0..12u16 {
                let col = 40 + k;
                let at = t0 + Duration::from_secs_f32(f32::from(k) / 8.0);
                let mut cx = ctx_in(g, at, &c, (5, col));
                cx.disp = 0.3;
                cx.birth_disp = 0.3;
                cx.mend = mended.then_some(Mend {
                    at,
                    disp: 0.9,
                    row: 5,
                    col: col - 1,
                    deletes: 1,
                });
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                n += sky
                    .live_iter()
                    .filter(|s| s.born == at && s.lane == StarLane::Exhaust)
                    .count();
            }
            n
        };
        let cold = run(false);
        assert_eq!(
            cold, 0,
            "a cold hand (spine 0.3) threw {cold} grains with no mend to floor it"
        );
        let mended = run(true);
        assert_eq!(
            mended, 11,
            "a cold hand mending at 0.9 threw {mended} grains, not one per key from the second: \
             the exhaust does not floor its spine on the mend's"
        );
    }

    #[test]
    fn the_blank_frontiers_early_stop_is_the_whole_walks_answer() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let cw = g.cw as f32;
        let cx = ctx_in(g, t0, &c, (5, 60));
        let mut checked = 0usize;
        for pattern in 0..12u32 {
            let mut occupied = [false; 120];
            for (col, slot) in occupied.iter_mut().enumerate() {
                *slot = match pattern {
                    0 => false,
                    1 => true,
                    p => hash01(mix32(col as u32 ^ (p << 16))) < 0.08 * p as f32,
                };
            }
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &occupied);
            for caret in [0u16, 1, 2, 7, 30, 59, 60, 61, 119] {
                let (x, _) = g.cell_center(5, caret);
                let mut frontier = i32::from(caret);
                while frontier > 0 {
                    let left = frontier - 1;
                    if !occupied[left as usize] {
                        frontier = left;
                    } else {
                        break;
                    }
                }
                let floor_x = (g.fx_left() + frontier * g.cw as i32) as f32;
                for want in [0.4f32, 0.5, 3.0, 7.0 * cw, 16.0 * cw, 40.0 * cw] {
                    let reference = if want.is_finite() && want >= 0.5 {
                        want.min(x - floor_x).max(0.0)
                    } else {
                        0.0
                    };
                    let by = default_band_top(g, &c, 5) - 0.2 * g.ch as f32;
                    let got = sky.blank_frontier_px(x, (by, by), want, &cx);
                    assert!(
                        (got - reference).abs() < 1e-4,
                        "pattern {pattern}, caret {caret}, want {want}: the early stop \
                         answered {got} where the whole walk answers {reference}"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 12 * 9 * 6, "the census did not run in full");
    }

    fn hot_event(
        sky: &mut Stardust,
        ribbon: &Ribbon,
        world: (Geom, &Config),
        ev: &Event,
        at: Instant,
        caret: (u16, u16),
        mend: Option<Mend>,
    ) {
        let (g, c) = world;
        let mut cx = ctx_in(g, at, c, caret);
        cx.disp = 0.9;
        cx.birth_disp = 0.9;
        cx.mend = mend;
        sky.budget.refill(at);
        sky.on_event(ev, at, &cx, ribbon);
        let mut sc = Scratch::default();
        let mut f = sc.frame();
        sky.emit(&cx, &mut f);
    }

    #[expect(clippy::type_complexity, reason = "a fixture's five readings")]
    fn mend_sky(
        typo: TypedClass,
        deletes: u16,
        fix_after_ms: u64,
        mended: bool,
        c: &Config,
        t0: Instant,
    ) -> (
        Stardust,
        Instant,
        (u16, u16),
        Option<((i32, i32), StarClass)>,
        Vec<StarClass>,
    ) {
        const SLOT_MS: u64 = 125;
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let glyph = typed(TypedClass::Glyph);
        let mut last = t0;
        for k in 0..8u16 {
            last = t0 + Duration::from_millis(SLOT_MS * u64::from(k));
            let key = if k == 7 { typed(typo) } else { glyph };
            hot_event(&mut sky, &ribbon, (g, c), &key, last, (5, 5 + k), None);
        }
        let mut caret = 12u16;
        for _ in 0..deletes {
            last += Duration::from_millis(SLOT_MS);
            caret -= 1;
            hot_event(
                &mut sky,
                &ribbon,
                (g, c),
                &Event::Erase,
                last,
                (5, caret),
                None,
            );
        }
        let erased = (5u16, caret);
        let fix = last + Duration::from_millis(fix_after_ms);
        let retiring = sky
            .live_iter()
            .find(|s| s.lane.is_sky() && !s.dead(fix) && star_over_cell(s, erased, g))
            .map(|s| (s.pos(fix, false), s.class));
        let at_home = sky
            .live_iter()
            .filter(|s| s.lane.is_sky() && !s.dead(fix) && star_over_cell(s, (5, caret + 1), g))
            .map(|s| s.class)
            .collect();
        let mend = mended.then_some(Mend {
            at: last,
            disp: 0.9,
            row: erased.0,
            col: erased.1,
            deletes: u8::try_from(deletes).expect("a mend counts at most two"),
        });
        hot_event(&mut sky, &ribbon, (g, c), &glyph, fix, (5, caret + 1), mend);
        (sky, fix, erased, retiring, at_home)
    }

    #[test]
    fn a_mend_s_star_is_born_where_the_typo_was_and_dragged_to_the_fix() {
        let mut c = cfg(true);
        c.theme_bg = SHIPPED_GROUND;
        let g = geom_cell(15, 28);
        let window_ms = MEND_PULL_MS as u64;
        let over = |p: (i32, i32), cell: (u16, u16)| {
            px_col(p.0 as f32, g) == i32::from(cell.1) && in_sky_of_row(p.1 as f32, cell.0, g)
        };
        for (label, typo, deletes, fix_after_ms) in [
            ("one-slot fix", TypedClass::Glyph, 1u16, 125u64),
            ("500 ms fix", TypedClass::Glyph, 1, 500),
            ("two deletes", TypedClass::Glyph, 2, 125),
            ("capital typo", TypedClass::Capital, 1, 125),
        ] {
            let t0 = Instant::now();
            let (mut sky, fix, erased, retiring, home_before) =
                mend_sky(typo, deletes, fix_after_ms, true, &c, t0);
            let home = (erased.0, erased.1 + 1);
            if typo == TypedClass::Capital {
                assert!(
                    home_before.contains(&StarClass::M1),
                    "NON-VACUOUS: the capital typo's earned hero is not over the caret's cell"
                );
            }
            let at = |ms: u64| fix + Duration::from_millis(ms);
            let fix_born: Vec<Star> = sky
                .live_iter()
                .filter(|s| s.born == fix && s.lane.is_sky())
                .copied()
                .collect();
            let typo_at = |p: (i32, i32)| retiring.is_some_and(|(r, _)| r == p) || over(p, erased);
            let Some(star) = fix_born
                .iter()
                .find(|s| typo_at(s.pos(fix, false)) && over(s.pos(at(window_ms), false), home))
            else {
                panic!(
                    "{label}: no star of the fix's stands where the typo was ({erased:?}, retiring {retiring:?}) at +0 and over the caret's {home:?} at +{window_ms} ms — the fix bore {:?}",
                    fix_born
                        .iter()
                        .map(|s| (s.class, s.pos(fix, false), s.pos(at(window_ms), false)))
                        .collect::<Vec<_>>()
                );
            };
            assert_eq!(
                star.class,
                StarClass::M2,
                "{label}: the mend's star is an m2"
            );

            let own = |ms: u64| {
                let p = star.pos(at(ms), false);
                p.0 + star.stream_px(ms as f32)
            };
            let (x0, y0) = star.pos(fix, false);
            let mut prev = own(0);
            for ms in 1..=window_ms {
                let now = own(ms);
                assert!(
                    now >= prev,
                    "{label}: the pull went backward at +{ms} ms ({prev} → {now})"
                );
                prev = now;
            }
            let travelled = own(window_ms) - own(0);
            assert_eq!(
                travelled,
                star.pull.round() as i32,
                "{label}: pulled {travelled} px, priced {:.1}",
                star.pull
            );
            for ms in window_ms + 1..=400 {
                assert_eq!(
                    own(ms),
                    own(window_ms),
                    "{label}: the star moved at +{ms} ms, after its window"
                );
            }

            if let Some((p, class)) = retiring {
                assert_eq!(
                    x0, p.0,
                    "{label}: born at x {x0}, the retiring {class:?} stood at {}",
                    p.0
                );
                assert!(
                    (y0 - p.1).abs() <= 2,
                    "{label}: born at y {y0}, the retiring {class:?} stood at {}",
                    p.1
                );
            }
            assert!(
                !sky.live_iter()
                    .any(|s| s.born < fix && s.lane.is_sky() && star_over_cell(s, erased, g)),
                "{label}: a star born before the fix is still over the erased cell {erased:?}"
            );
            assert!(
                !fix_born
                    .iter()
                    .any(|s| s.pull == 0.0 && over(s.pos(fix, false), erased)),
                "{label}: the fix bore a fresh star over the erased cell {erased:?} beside the one it pulled away"
            );
            assert_eq!(
                fix_born
                    .iter()
                    .filter(|s| over(s.pos(at(window_ms), false), home))
                    .count(),
                1,
                "{label}: the fix bore more than one sky star over the caret's cell"
            );
            let at_home: Vec<(StarClass, StarLane, (i32, i32))> = {
                let now = at(window_ms);
                let mut cx = ctx_in(g, now, &c, home);
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                let mut sc = Scratch::default();
                let mut f = sc.frame();
                sky.emit(&cx, &mut f);
                sky.live_iter()
                    .filter(|s| s.lane.is_sky() && over(s.pos(now, false), home))
                    .map(|s| (s.class, s.lane, s.pos(now, false)))
                    .collect()
            };
            assert_eq!(
                at_home.len(),
                1,
                "{label}: the caret's cell holds {at_home:?} at +{window_ms} ms — one star per cell"
            );

            let moving_ms = window_ms.max(EXHAUST_THROW_MS as u64);
            assert!(sky.brisk(at(55)), "{label}: not brisk mid-pull");
            assert!(
                star.brisk(at(window_ms - 1)),
                "{label}: the mend's star is not brisk one millisecond before its pull ends"
            );
            assert!(
                !star.brisk(at(window_ms + 1)),
                "{label}: the mend's star is still brisk one millisecond after its pull"
            );
            assert!(
                !sky.brisk(at(moving_ms + 1)),
                "{label}: still brisk one millisecond after the last thing the fix set moving"
            );

            for ms in [0u64, 55, window_ms] {
                let now = at(ms);
                let mut cx = ctx_in(g, now, &c, home);
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                let mut sc = Scratch::default();
                let mut f = sc.frame();
                sky.emit(&cx, &mut f);
                let (x, y) = star.pos(now, false);
                let centre = composite_over(&sc, SHIPPED_GROUND, x, y, 1, 1)[0];
                let lum = relative_luminance(centre) * 255.0;
                assert!(
                    lum <= STAR_LIGHT_CEIL * 255.0,
                    "{label}: the mend's star centre at +{ms} ms reads Y {lum:.1} over the \
                     caret law's ceiling"
                );
            }
        }
    }

    use crate::kitty_pet::{PetAction, PetBrain, PetFrame, PetSense};
    use crate::rainbow_kitty::companion::{CATCH_REACH_CELLS, PetOffer, PetOnGlass, StarCatch};
    use crate::rainbow_kitty::{Dir, Engine, Licence};

    const fn paw_geom() -> Geom {
        Geom {
            cw: 10,
            ch: 20,
            rows: 30,
            cols: 100,
            origin_x: 0,
            origin_y: 0,
            win_w: 1000,
            win_h: 600,
            head: 0,
        }
    }

    const fn paw_sense(now: Instant, caret: (u16, u16)) -> PetSense {
        PetSense {
            caret_drawn: true,
            now,
            caret: Some(caret),
            wrapped: false,
            rows: 30,
            cols: 100,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        }
    }

    const FRAME_MS: u64 = 16;

    const WRAP_COL: u16 = 90;

    #[derive(Clone, Copy, Debug)]
    struct Step {
        at: Instant,
        pet: PetFrame,
        twin_pet: PetFrame,
        offer: PetOffer,
        caught: Option<(StarCatch, bool)>,
        emitted: (usize, usize, usize),
        twin_emitted: (usize, usize, usize),
    }

    struct Lockstep {
        eng: Engine,
        pet: PetBrain,
        twin_eng: Engine,
        twin_pet: PetBrain,
        sc: Scratch,
        tsc: Scratch,
        cfg: Config,
        t: Instant,
        caret: (u16, u16),
        last: Option<Step>,
    }

    impl Lockstep {
        fn new(t0: Instant, caret: (u16, u16)) -> Self {
            let mut eng = Engine::new();
            eng.set_engaged(true);
            let mut twin_eng = Engine::new();
            twin_eng.set_engaged(true);
            Self {
                eng,
                pet: PetBrain::default(),
                twin_eng,
                twin_pet: PetBrain::default(),
                sc: Scratch::default(),
                tsc: Scratch::default(),
                cfg: cfg(true),
                t: t0,
                caret,
                last: None,
            }
        }

        fn frame(&mut self) -> Step {
            let g = paw_geom();
            let t = self.t;
            clear(&mut self.sc);
            clear(&mut self.tsc);
            {
                let mut fr = self.sc.frame();
                self.eng.tick(t, g, &self.cfg, &mut fr);
            }
            {
                let mut fr = self.tsc.frame();
                self.twin_eng.tick(t, g, &self.cfg, &mut fr);
            }
            let pet = self.pet.tick(paw_sense(t, self.caret));
            let twin_pet = self.twin_pet.tick(paw_sense(t, self.caret));
            let offer = self.eng.pet_offer(g, PetOnGlass::of(&pet, g));
            let caught = self
                .pet
                .note_v2_offer(&offer)
                .map(|star| (star, self.eng.catch_star(star, t)));
            let step = Step {
                at: t,
                pet,
                twin_pet,
                offer,
                caught,
                emitted: emitted(&self.sc),
                twin_emitted: emitted(&self.tsc),
            };
            self.last = Some(step);
            step
        }

        fn next_frame(&mut self) -> Step {
            self.t += Duration::from_millis(FRAME_MS);
            self.frame()
        }

        fn advance(&mut self, gap: Duration) {
            let due = self.t + gap;
            while self.t + Duration::from_millis(FRAME_MS) < due {
                self.next_frame();
            }
            self.t = due;
        }

        fn key(&mut self, class: TypedClass) -> Step {
            let (r, c) = self.caret;
            let t = self.t;
            let to = (r, c + 1);
            for e in self.engines() {
                e.probe_mut().probe_row(i32::from(r) - 1, &[false; 100]);
                e.probe_mut().probe_row(i32::from(r), &[false; 100]);
                e.on_event(
                    Event::Move {
                        from: (r, c),
                        to,
                        licence: Licence::Typed,
                        dir: Dir::of(1, 0),
                    },
                    t,
                );
                e.on_event(
                    Event::Typed {
                        cells: 1,
                        shifted: matches!(class, TypedClass::Capital),
                        class,
                    },
                    t,
                );
            }
            self.caret = to;
            self.frame()
        }

        fn backspace(&mut self) -> Step {
            let (r, c) = self.caret;
            let t = self.t;
            let to = (r, c.saturating_sub(1));
            for e in self.engines() {
                e.on_event(
                    Event::Move {
                        from: (r, c),
                        to,
                        licence: Licence::Typed,
                        dir: Dir::of(-1, 0),
                    },
                    t,
                );
                e.on_event(Event::Erase, t);
            }
            self.caret = to;
            self.frame()
        }

        fn newline(&mut self) -> Step {
            let (r, c) = self.caret;
            let t = self.t;
            let to = (r + 1, 0);
            for e in self.engines() {
                e.on_event(
                    Event::Move {
                        from: (r, c),
                        to,
                        licence: Licence::Return,
                        dir: Dir::of(-i32::from(c), 1),
                    },
                    t,
                );
                e.on_event(Event::Return, t);
            }
            self.caret = to;
            self.frame()
        }

        fn key_after(&mut self, gap: Duration, class: TypedClass) -> Step {
            self.advance(gap);
            if self.caret.1 >= WRAP_COL {
                self.newline();
                self.advance(gap);
            }
            self.key(class)
        }

        fn engines(&mut self) -> impl Iterator<Item = &mut Engine> {
            [&mut self.eng, &mut self.twin_eng].into_iter()
        }

        fn cat(&self) -> Option<PetOnGlass> {
            self.last.and_then(|s| PetOnGlass::of(&s.pet, paw_geom()))
        }

        fn star(&self, star: StarCatch) -> Option<Star> {
            find_star(&self.eng, star)
        }

        fn twin_star(&self, star: StarCatch) -> Option<Star> {
            find_star(&self.twin_eng, star)
        }
    }

    fn clear(sc: &mut Scratch) {
        sc.under.clear();
        sc.out.clear();
        sc.halos.clear();
        sc.beams.clear();
        sc.cues.clear();
    }

    const fn emitted(sc: &Scratch) -> (usize, usize, usize) {
        (sc.under.len() + sc.out.len(), sc.halos.len(), sc.cues.len())
    }

    fn find_star(eng: &Engine, star: StarCatch) -> Option<Star> {
        eng.stardust
            .live_iter()
            .find(|s| s.born == star.born && s.x == star.x && s.y == star.y)
            .copied()
    }

    fn gold_born(eng: &Engine, at: Instant) -> Option<Star> {
        eng.stardust
            .live_iter()
            .find(|s| s.gold && s.class == StarClass::M1 && s.lane.is_sky() && s.born == at)
            .copied()
    }

    fn gold_capital_at(eng: &Engine, cell: (u16, u16)) -> Option<(u16, u16)> {
        let free = eng.stardust.free_sky_cell(cell, paw_geom())?;
        let seed = cell_hash(free.0, free.1, eng.stardust.minted.wrapping_add(1));
        deal_tint(seed, 0.0).1.then_some(free)
    }

    fn rally_until_seated(w: &mut Lockstep, gap: Duration, rounds: u32) -> u32 {
        for round in 0..rounds {
            for _ in 0..3 {
                w.key_after(gap, TypedClass::Glyph);
            }
            for _ in 0..2 {
                w.advance(gap);
                w.backspace();
            }
            if w.cat().is_some_and(|c| {
                c.contented() && w.last.is_some_and(|s| s.pet.action == PetAction::Sit)
            }) {
                return round + 1;
            }
        }
        panic!("fixture: the rally did not seat the cat in {rounds} rounds");
    }

    #[test]
    fn a_gold_star_walks_from_the_deal_through_the_paw_to_the_catch() {
        let t0 = Instant::now();
        let g = paw_geom();
        let gap = Duration::from_micros(83_333);
        let mut w = Lockstep::new(t0, (4, 0));

        for _ in 0..30 {
            w.key_after(gap, TypedClass::Glyph);
        }
        let rounds = rally_until_seated(&mut w, gap, 12);
        let seat = w.cat().expect("fixture: the cat is on glass");
        assert!(seat.contented(), "fixture: a contented seat");

        let mut dealt: Option<(Step, (u16, u16))> = None;
        'rounds: for _ in 0..rounds + 40 {
            for k in 0..3 {
                w.advance(gap);
                let cell = w.caret;
                let cat = w.cat().expect("fixture: the cat stays on glass");
                let in_reach =
                    cat.contented() && cat.columns_to(i32::from(cell.1)) <= CATCH_REACH_CELLS;
                if k == 2
                    && in_reach
                    && let Some(free) = gold_capital_at(&w.eng, cell)
                {
                    let s = w.key(TypedClass::Capital);
                    dealt = Some((s, free));
                    break 'rounds;
                }
                w.key(TypedClass::Glyph);
            }
            for _ in 0..2 {
                w.advance(gap);
                w.backspace();
            }
        }
        let (birth, free) = dealt.expect("fixture: the seed knob must read gold within 40 rounds");

        let star = gold_born(&w.eng, birth.at).expect("the capital dealt a gold m1");
        assert_eq!(
            star.grid_col(g),
            i32::from(free.1),
            "born on the knob's cell"
        );
        assert!(star.in_sky_of(4, g), "born in the sky of the cat's row");
        assert_eq!(
            gold_born(&w.twin_eng, birth.at).map(|s| (s.x, s.y, s.born)),
            Some((star.x, star.y, star.born)),
            "the twin dealt the same star: the seam mints nothing"
        );
        assert!(
            star.finish.is_none(),
            "born with its whole life ahead of it"
        );

        let catch = StarCatch {
            x: star.x,
            y: star.y,
            born: star.born,
            col: star.grid_col(g),
        };
        assert!(
            birth.pet.action.settled() && birth.pet.purr > 0.0,
            "a settled, purring cat"
        );
        assert_eq!(
            birth.offer.catch,
            Some(catch),
            "the birth frame's offer names the star"
        );
        assert_eq!(birth.caught, None, "noting is not catching");
        assert!(
            w.pet.pending_v2_offer().1,
            "the star is latched in the brain"
        );

        let mut contact = None;
        for _ in 0..20 {
            let before = w.star(catch).expect("the star lives until the paw");
            assert_eq!(before.finish, None, "the life moved before the paw landed");
            let s = w.next_frame();
            if let Some((back, hit)) = s.caught {
                assert_eq!(back, catch, "the star handed back is the star offered");
                assert!(hit, "the sky still had the star to catch");
                contact = Some(s);
                break;
            }
        }
        let contact = contact.expect("the paw lands inside the star's TTL");
        let horizon = contact.at + Duration::from_secs_f32(CATCH_FINISH_MS / 1000.0);

        let caught = w
            .star(catch)
            .expect("the caught star is still on glass on the paw's frame");
        assert_eq!(
            caught.finish,
            Some(Finish {
                at: contact.at,
                span_s: CATCH_FINISH_MS / 1000.0,
            }),
            "the catch spends the star on the sky's own finish"
        );
        assert_eq!(
            w.twin_star(catch).map(|s| s.finish),
            Some(None),
            "the un-offered twin's copy keeps its whole life"
        );
        let others = |e: &Engine| -> Vec<(u32, u32, Instant, Option<Finish>)> {
            let mut v: Vec<_> = e
                .stardust
                .live_iter()
                .filter(|s| !(s.born == catch.born && s.x == catch.x && s.y == catch.y))
                .map(|s| (s.x.to_bits(), s.y.to_bits(), s.born, s.finish))
                .collect();
            v.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
            v
        };
        assert_eq!(
            others(&w.eng),
            others(&w.twin_eng),
            "a bystander's life moved"
        );
        assert!(
            !others(&w.eng).is_empty(),
            "fixture: there were bystanders to protect"
        );

        assert_eq!(
            contact.pet.motes.iter().flatten().count(),
            contact.twin_pet.motes.iter().flatten().count(),
            "the reach spawned a mote"
        );
        assert_eq!(
            contact.emitted, contact.twin_emitted,
            "the catch drew or sounded something"
        );
        assert_eq!(
            w.eng.fingerprint(),
            w.twin_eng.fingerprint(),
            "the contact frame's light is byte-identical to the un-offered world's"
        );

        assert_eq!(
            (
                contact.pet.col.to_bits(),
                contact.pet.row.to_bits(),
                contact.pet.lift.to_bits()
            ),
            (
                contact.twin_pet.col.to_bits(),
                contact.twin_pet.row.to_bits(),
                contact.twin_pet.lift.to_bits()
            ),
            "the offered cat is at ({}, {}) and the un-offered twin at ({}, {})",
            contact.pet.col,
            contact.pet.row,
            contact.twin_pet.col,
            contact.twin_pet.row
        );

        let mut catches = 1;
        while w.t < horizon {
            let s = w.next_frame();
            catches += usize::from(s.caught.is_some());
            if let Some(s) = w.star(catch) {
                assert_eq!(
                    s.finish.map(|f| f.at),
                    Some(contact.at),
                    "the finish was re-stamped after the paw"
                );
            }
        }
        let s = w.next_frame();
        catches += usize::from(s.caught.is_some());
        assert!(w.t >= horizon);
        assert!(
            w.star(catch).is_none(),
            "the caught star is still in the sky past the horizon"
        );
        assert!(
            w.twin_star(catch).is_some_and(|s| !s.dead(w.t)),
            "fixture: the un-offered twin's star lives on past the horizon (m1 sky life 460 ms)"
        );

        for _ in 0..90 {
            let s = w.next_frame();
            catches += usize::from(s.caught.is_some());
        }
        assert_eq!(catches, 1, "the paw handed a star back {catches} times");

        let mut t_zero = None;
        for _ in 0..(20 * 1000 / FRAME_MS) {
            w.next_frame();
            if w.eng.fingerprint() == 0
                && !w.eng.needs_frame_cadence()
                && w.eng.next_change_deadline(w.t).is_none()
            {
                t_zero = Some(w.t);
                break;
            }
        }
        assert!(
            t_zero.is_some(),
            "the sky idles to exactly zero within 20 s of the catch"
        );
        assert_eq!(w.eng.status().stars, 0);
        assert_eq!(
            w.eng.pet_offer(g, w.cat()),
            PetOffer::default(),
            "an idle sky offers the resident nothing"
        );
        assert_eq!(
            w.pet.pending_v2_offer(),
            (false, false),
            "a latch outlived the catch"
        );
        assert_eq!(w.eng.fingerprint(), 0, "the sky lit again while idle");
    }

    fn steady_sky(gm: Geom, cps: f32, keys: u16) -> (Stardust, Scratch) {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut sc = Scratch::default();
        for k in 0..keys {
            let at = t0 + Duration::from_micros((f32::from(k) * 1e6 / cps) as u64);
            let mut cx = ctx_in(gm, at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k).max(0.9);
            cx.birth_disp = cx.disp;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
        }
        (sky, sc)
    }

    #[test]
    fn a_point_query_reads_one_cell_not_the_whole_sky() {
        for (label, gm, cps) in [
            ("12 cps 1x", geom(), 12.0f32),
            ("125 cps retina", geom_cell(15, 28), 125.0),
        ] {
            GROUND_WORK.with(|w| w.set([0; 4]));
            steady_sky(gm, cps, 48);
            let w = GROUND_WORK.with(std::cell::Cell::get);
            let (scan, cell) = (w[0] + w[1], w[2] + w[3]);
            assert!(scan > 0, "{label}: the sky never priced a star");
            assert!(
                cell * 10 <= scan,
                "{label}: the indexed ground still reads {cell} entries against the scan's \
                 {scan} — the cell index is not narrowing the query"
            );
        }
    }

    #[test]
    fn the_slot_hash_is_injective_over_every_box_the_cap_allows() {
        fn deltas(cap: i32) -> Vec<(i32, i32)> {
            let mut v: Vec<(i32, i32)> = Vec::new();
            for w in 1..=cap {
                for h in 1..=cap {
                    if w * h > cap {
                        continue;
                    }
                    for dx in -(w - 1)..w {
                        for dy in -(h - 1)..h {
                            if (dx, dy) != (0, 0) && !v.contains(&(dx, dy)) {
                                v.push((dx, dy));
                            }
                        }
                    }
                }
            }
            v
        }

        let cap = i32::try_from(SKY_INDEX_WIDE_CELLS).expect("the cap is small");
        let offsets = deltas(cap);
        assert_eq!(
            offsets.len(),
            136,
            "a 16-cell box reaches 136 offsets; if this moved, the cap moved \
             and the sweep below is no longer the sweep this law needs"
        );
        for cx in 0..1_i32 << 13 {
            let base = SkyIndex::slot((cx, 0));
            for &(dx, dy) in &offsets {
                assert_ne!(
                    base,
                    SkyIndex::slot((cx + dx, dy)),
                    "cells (0, 0) and ({dx}, {dy}) of ONE box share a chain at \
                     base cell ({cx}, 0) — a lay filed under both would have \
                     its light added twice by `sky_ground`"
                );
            }
        }

        let doubled = (0..1_i32 << 13)
            .filter(|&cx| SkyIndex::slot((cx, 0)) == SkyIndex::slot((cx, 19)))
            .count();
        assert!(
            doubled > 0,
            "the `(0, 19)` offset collided at none of 8192 base cells, so \
             this arm no longer shows the cap is what makes the law true"
        );
    }

    #[test]
    fn a_lay_too_wide_to_file_spills_the_index_and_still_answers() {
        let gm = geom();
        let mut sc = Scratch::default();
        sc.out.push(GlowQuad {
            row: 5,
            x: 90,
            y: 130,
            w: 360,
            h: 4,
            color: 0x0010_2030,
            alpha: 0,
            color2: 0x0010_2030,
            alpha2: 0,
        });
        let mut index = SkyIndex::default();
        index.open(gm, (0, 0));
        {
            let frame = sc.frame();
            index.ingest(&frame);
        }
        assert!(index.spilled, "a 40-cell lay was filed as if it were one");
        let frame = sc.frame();
        for (x, y) in [(90, 130), (300, 131), (449, 133), (89, 130), (450, 130)] {
            assert_eq!(
                index.ground(0, &frame, x, y),
                sky_ground(0, &*frame.out, &*frame.halos, x, y),
                "({x}, {y}) under a spilled index"
            );
        }
        index.open(gm, (0, 0));
        assert!(!index.spilled, "the spill outlived its frame");
    }

    #[test]
    fn a_cell_less_geometry_falls_back_to_one_chain() {
        let gm = Geom {
            cw: 0,
            ch: 0,
            ..geom()
        };
        let mut sc = Scratch::default();
        sc.out.push(GlowQuad {
            row: 0,
            x: 90,
            y: 100,
            w: 4,
            h: 2,
            color: 0x0020_3040,
            alpha: 0,
            color2: 0x0020_3040,
            alpha2: 0,
        });
        let mut index = SkyIndex::default();
        index.open(gm, (0, 0));
        {
            let frame = sc.frame();
            index.ingest(&frame);
        }
        let frame = sc.frame();
        for (x, y) in [
            (90, 100),
            (93, 101),
            (89, 100),
            (94, 100),
            (90, 99),
            (90, 102),
        ] {
            assert_eq!(
                index.ground(0, &frame, x, y),
                sky_ground(0, &*frame.out, &*frame.halos, x, y),
                "({x}, {y}) under a cell-less geometry"
            );
        }
    }

    /// A star on a keep-out box's edge (dark and paper) or the paper caret's margin makes at most
    /// [`crate::TWINKLE_FLASH_BOUND_HZ`] visibility transitions a second and keeps one mark reach.
    /// The negative control culls by the drawn arm and toggles past the bound.
    #[test]
    fn a_star_on_a_cull_s_edge_never_blinks_with_its_arm() {
        let t0 = Instant::now();
        let g = geom();
        let bound = crate::TWINKLE_FLASH_BOUND_HZ as usize;
        let at = |k: u64| t0 + Duration::from_micros(k * 16_667);
        let worst_second = |on: &[bool]| {
            on.windows(61)
                .map(|w| w.windows(2).filter(|p| p[0] != p[1]).count())
                .max()
                .unwrap_or(0)
        };
        let caret_far = (3u16, 10u16);
        let caret = (3u16, 30u16);
        let margin = i32::from(g.origin_x) + i32::from(caret.1) * g.cw as i32 - g.cw as i32;
        let mut control_worst = 0usize;
        for dark in [true, false] {
            let c = cfg(dark);
            for class in [StarClass::M1, StarClass::M2, StarClass::M3] {
                for lane in [StarLane::Strike, StarLane::Field] {
                    let lo = arm_trough_px(class, g.ch as f32) + 1;
                    let hi = arm_peak_px(class, g.ch as f32) + 1;
                    // (star x, keep-out box, caret) for each placement.
                    let mut placements = Vec::new();
                    let x = 200i32;
                    for x0 in (x + lo / 2 - 1)..=(x + hi / 2 + 1) {
                        placements.push((x, Some((x0, x0 + 30, 96, 104)), caret_far));
                    }
                    if !dark {
                        for sx in (margin - hi - 1)..=(margin - lo + 1) {
                            placements.push((sx, None, caret));
                        }
                    }
                    let (mut drew, mut culled) = (false, false);
                    for (sx, boxed, car) in placements {
                        let mut st = star(class, lane, t0, TINT_WHITE_RGB);
                        st.x = sx as f32;
                        let mut sky = sky();
                        sky.sow(st);
                        if let Some(b) = boxed {
                            sky.keep_out.set([b]);
                        }
                        let (mut on, mut old, mut reaches) = (Vec::new(), Vec::new(), Vec::new());
                        for k in 0..240u64 {
                            let cx = ctx_in(g, at(k), &c, car);
                            let mut sc = Scratch::default();
                            sky.emit(&cx, &mut sc.frame());
                            on.push(sc.out.len() + sc.halos.len() > 0);
                            reaches.extend(sky.marks().first().map(|m| m.reach));
                            // The control: the same cull keyed on the drawn arm.
                            let painted = Paint::of(&st, &sky.probe, &sky.shade, &cx);
                            old.push(
                                st.inks(&c)
                                    && painted.is_some_and(|p| {
                                        let reach = p.arm.max(p.arm_lo) + 1;
                                        !(sky.keep_out.covers((p.x, p.y), reach / 2)
                                            || cx.near_paper_caret((p.x, p.y), reach))
                                    }),
                            );
                        }
                        let what = format!(
                            "dark={dark} {class:?}/{lane:?} star x {sx} box {boxed:?} caret {car:?}"
                        );
                        let n = worst_second(&on);
                        assert!(
                            n <= bound,
                            "{what}: {n} visibility transitions in one second (bound {bound})"
                        );
                        reaches.dedup();
                        assert!(
                            reaches.len() <= 1,
                            "{what}: the star's mark changes reach {reaches:?}"
                        );
                        drew |= on.iter().any(|&o| o);
                        culled |= !on.iter().any(|&o| o);
                        control_worst = control_worst.max(worst_second(&old));
                    }
                    assert!(
                        drew && culled,
                        "dark={dark} {class:?}/{lane:?}: the sweep never crosses the cull's edge (drew {drew}, culled {culled})"
                    );
                }
            }
        }
        assert!(
            control_worst > bound,
            "control: culled by the drawn arm, the worst placement toggles only {control_worst} times in a second"
        );
    }
}

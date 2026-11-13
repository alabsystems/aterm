// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stardust: the sky above the ribbon.

use aterm_render::{GlowQuad, HaloMode, RainHalo, add_sat, halo_row_ny, halo_weight, premul_rgb};
use aterm_time::Instant;

use super::companion::{KeepOut, PetShade};
use super::ribbon::paper_mark_ink_with_bg_lum;
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
    TWINKLE_GLINT_FRAC, TWINKLE_OMEGA, mix_rgb, push_fx_rect, push_twinkle_star, star_arm,
    star_body_px, twinkle_env, twinkle_peak,
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

/// Lower bound of a star's seeded chime tremolo, Hz ([`Glint::f`]). It is heard, never drawn: the
/// arm it once stepped on glass (16 to 24 size steps a second at the arm's tip, under an area
/// exemption nobody ruled) is drawn at its peak for the star's whole life.
pub const SCINT_F_MIN: f32 = 8.0;

/// Upper bound of the seeded chime tremolo, Hz.
pub const SCINT_F_MAX: f32 = 12.0;

/// How long a pixel of paper rests after a star is born on it, seconds: no other star is stood
/// within reach of it sooner ([`Stardust::stand`]). A star on paper darkens its pixels once and
/// lets them go once, two turns of the flash law; a third of a second and a frame between births
/// keeps any pixel at three such stars a second or fewer, whatever the hand does.
pub const PAPER_REST_S: f32 = 1.0 / 3.0 + 0.017;

/// How many places along its flight, and how many steps to each side of the first clear one, a
/// paper star is offered ([`Stardust::stand`]).
pub const STAND_TRIES: u32 = 8;
/// The heights [`Stardust::stand`] looks along: the first place's own and those above it.
pub const STAND_ROWS: u32 = 3;

/// The births [`Stardust::stand`] remembers: more than a full pool born inside one
/// [`PAPER_REST_S`].
pub const STOOD_CAP: usize = 96;

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

    /// The paper star's mass, as a share of the nominal arm: [`Paint::draw_over`] builds its body
    /// on this arm and lays the rest of the reach as its points (m1 0.88, m2 and m3 0.55).
    #[inline]
    #[must_use]
    pub const fn arm_floor(self) -> f32 {
        match self {
            Self::M1 => 0.88,
            Self::M2 | Self::M3 => 0.55,
        }
    }

    /// The smallest integer arm this class has, at cell height `ch`.
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
    /// `(mass arm, drawn arm)` px at cell height `ch`: the arm a paper star's body is built on
    /// ([`Self::arm_floor`]) and the one arm every star is drawn at for its whole life.
    const fn arms(self, ch: f32) -> (i32, i32) {
        let nominal = star_arm(ch, self.arm_ratio());
        let min_arm = self.arm_min_px(ch);
        let arm_floor = self.arm_floor();
        let raw_lo = (nominal * arm_floor).round() as i32;
        let arm_lo = if raw_lo > min_arm { raw_lo } else { min_arm };
        let raw_hi = nominal.round() as i32;
        let arm_hi = if raw_hi > min_arm { raw_hi } else { min_arm };
        (arm_lo, arm_hi)
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
        (h * (1.0 / 1000.0), t * (1.0 / 1000.0))
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
        crate::effect_util::saturating_add_secs_f32(self.at, self.span_s.max(0.0))
    }

    #[inline]
    fn keep_sooner(slot: &mut Option<Self>, at: Instant, span_s: f32) {
        let next = Self { at, span_s };
        if !matches!(*slot, Some(f) if next.ends() >= f.ends()) {
            *slot = Some(next);
        }
    }

    #[inline]
    #[must_use]
    fn cull_at(&self) -> Instant {
        crate::effect_util::saturating_add_secs_f32(
            self.at,
            self.span_s.max(0.0) * STAR_CULL_SPAN_SHARE,
        )
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
    /// The seeded chime tremolo, Hz, in `[SCINT_F_MIN, SCINT_F_MAX]`: what [`Glint::f`] is cued
    /// with. Nothing drawn reads it.
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
    /// Whether, at its first paint, the star stood on a dark page's caret
    /// row beside the caret's block ([`Ctx::beside_dark_caret`], looks9
    /// Q10): decided once, it is then not drawn for its life, or drawn
    /// whatever the caret does there later (`None`: not yet painted).
    pub beside_caret: Option<bool>,
    /// The effects box's edge cut this star on paper once, so it is left out
    /// whole for the rest of its life ([`crate::cursor_glow::drop_cut_particle`]):
    /// judged afresh each frame, a star whose glints swell to the edge and
    /// shrink off it again blinked with its scintillation.
    pub edge_cut: bool,
    /// On paper, where this star stands for its whole life, as an offset from its home pixel
    /// ([`Stardust::stand`]); `None` on a dark page, where it flies. Ink on paper does not move: a
    /// grain crossing a pixel darkens it and lets it go, and the grains of one typed line cross the
    /// same pixels in turn, sixteen to twenty times a second at a fast hand. Stood still, the grain
    /// is born once where its flight would have shown it and only cools.
    pub still: Option<(i32, i32)>,
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

    /// Born at full: true inside the class hold `H` (§5.6), when a star draws its full core; on a
    /// dark page the luminance clock takes over at `age ≥ H`, still reading `now − born`.
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
            || matches!(self.finish, Some(f) if f.gain(now) < STAR_CULL_ALPHA)
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
        let fin = match self.finish {
            Some(f) => f.gain(now),
            None => 1.0,
        };
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
        if self.still.is_some() {
            return self.lane == StarLane::Field && self.age_s(now) < EDGE_IN_S;
        }
        let motion = self.lane.motion();
        let has_vel = self.v0.0 != 0.0 || self.v0.1 != 0.0;
        if matches!(motion, Motion::Drag) && has_vel {
            return true;
        }
        if self.lane != StarLane::Field
            && self.pull <= 0.0
            && (!has_vel || matches!(motion, Motion::Lift))
        {
            return false;
        }
        let age_s = self.age_s(now);
        if self.lane == StarLane::Field && age_s < EDGE_IN_S {
            return true;
        }
        if self.pull > 0.0 && age_s * 1000.0 < MEND_PULL_MS {
            return true;
        }
        if !has_vel {
            return false;
        }
        match motion {
            Motion::Throw(window_ms) => age_s * 1000.0 < window_ms,
            Motion::Drag => true,
            Motion::Lift => false,
        }
    }

    /// Whether what this star draws is changing at `age_s`, past its class hold, with nothing
    /// else of its own asking for a frame: a fade's tail ([`Cadence::tail`]).
    ///
    /// A flying star (a dark page's) is on its luminance clock for the rest of its life. A
    /// star stood on paper ([`Self::still`]) has no clock and only cools: off the field lane
    /// its half-life runs from the hold's end to its cull; on the field lane it is held whole
    /// while its row is typed on and cools from [`FIELD_HOLD_MS`] after the row's last key,
    /// and melts over the last [`EXPIRY_MELT_SHARE`] of its life. The arm's size clock used to
    /// ask for these frames by the way (a step at least every 62.5 ms); with the arm steady
    /// nothing did, and where the ribbon and the pet were at rest a paper star held its last
    /// painted strength and then was gone, and a dark star's twinkle was drawn at its lift's
    /// three steps a second.
    #[inline]
    #[must_use]
    fn turning(&self, age_s: f32, now: Instant) -> bool {
        if self.still.is_none() || self.lane != StarLane::Field {
            return true;
        }
        self.idle_s(now) >= FIELD_HOLD_MS / 1000.0
            || age_s >= FIELD_LIFE_S * (1.0 - EXPIRY_MELT_SHARE)
    }

    /// Seconds until a gold star's diagonal glints next switch (`|sin(ω·age + φ)| ≥ 0.92`, §5.2,
    /// §5.5), exact on the luminance clock.
    #[inline]
    #[must_use]
    pub fn next_glint_toggle_s(&self, now: Instant) -> Option<f32> {
        self.next_glint_toggle_at_age_s(self.age_s(now))
    }

    #[inline]
    #[must_use]
    fn next_glint_toggle_at_age_s(&self, age_s: f32) -> Option<f32> {
        if !self.gold || self.still.is_some() {
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
        let theta = TWINKLE_OMEGA * age_s + self.lum_phase();
        let base = sine_crossing_base(theta, TWINKLE_OMEGA)?;
        next_sine_crossing_from_base(base, TWINKLE_GLINT_ASIN, 1.0 / TWINKLE_OMEGA)
    }

    /// Seconds until a lifting star's integer position next steps (§5.6):
    /// `pos = origin + round(v₀·age)` steps where `|v₀|·age` next reaches a half.
    #[inline]
    #[must_use]
    pub fn next_lift_step_s(&self, now: Instant) -> Option<f32> {
        self.next_lift_step_at_age_s(self.age_s(now))
    }

    #[inline]
    #[must_use]
    fn next_lift_step_at_age_s(&self, age_s: f32) -> Option<f32> {
        if self.still.is_some()
            || (self.v0.0 == 0.0 && self.v0.1 == 0.0)
            || !matches!(self.lane.motion(), Motion::Lift)
        {
            return None;
        }
        let age_ms = age_s * 1000.0;
        let mut best: Option<f32> = None;
        let vs = [self.v0.0.abs(), self.v0.1.abs()];
        let mut vi = 0;
        while vi < vs.len() {
            let v = vs[vi];
            vi += 1;
            if !v.is_finite() || v <= 0.0 {
                continue;
            }
            let next_ms = ((v * age_ms).round() + 0.5) / v;
            let dt = (next_ms - age_ms) * (1.0 / 1000.0);
            if dt > 0.0 {
                best = Some(match best {
                    Some(b) => b.min(dt),
                    None => dt,
                });
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
        self.next_stream_step_at_age_s(self.age_s(now))
    }

    #[inline]
    #[must_use]
    fn next_stream_step_at_age_s(&self, age_s: f32) -> Option<f32> {
        if self.still.is_some() || self.stream <= 0.0 || !self.stream.is_finite() {
            return None;
        }
        let age_ms = age_s * 1000.0;
        if age_ms >= STREAM_END_MS {
            return None;
        }
        let n = (self.stream * stream_unit(age_ms)).round();
        let u = (n + 0.5) / self.stream;
        if u >= 1.0 {
            return None;
        }
        let t_ms = -STREAM_TAU_MS * crate::color_math::ln_pos_f32(1.0 - u * STREAM_SPENT);
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
        if let Some((dx, dy)) = self.still {
            let (x, y) = self.origin();
            return (x + dx, y + dy);
        }
        self.flight_at_age(age_s)
    }

    /// Where the star's flight has it at `age_s`, whether or not it stands still.
    #[inline]
    #[must_use]
    const fn flight_at_age(&self, age_s: f32) -> (i32, i32) {
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
        let dt = match self.at.replace(now) {
            Some(t) => now.saturating_duration_since(t).as_secs_f32(),
            None => 0.0,
        };
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
        let mut col = 0;
        while col < occupied.len() {
            if occupied[col] {
                r.ink[col >> 6] |= 1u64 << (col & 63);
            }
            col += 1;
        }
    }

    /// [`Self::probe_row`] with the per-cell class ([`cell_ink`]): a [`CellInk::Rule`] is recorded
    /// as ink (for [`Self::at`]) and as a rule (for [`Self::at_px`]'s stroke band).
    #[inline]
    pub fn probe_row_ink(&mut self, row: i32, cells: &[CellInk]) {
        let slot = self.slot_for(row, cells.len());
        let r = &mut self.rows[slot];
        let mut col = 0;
        while col < cells.len() {
            let bit = 1u64 << (col & 63);
            match cells[col] {
                CellInk::Blank => {}
                CellInk::Glyph => r.ink[col >> 6] |= bit,
                CellInk::Rule => {
                    r.ink[col >> 6] |= bit;
                    r.rule[col >> 6] |= bit;
                }
            }
            col += 1;
        }
    }

    /// [`Self::probe_row_ink`] classifying each `char` with [`cell_ink`] in place.
    #[inline]
    pub fn probe_row_chars(&mut self, row: i32, chars: &[char]) {
        let slot = self.slot_for(row, chars.len());
        let r = &mut self.rows[slot];
        let mut col = 0;
        while col < chars.len() {
            let bit = 1u64 << (col & 63);
            match cell_ink(chars[col]) {
                CellInk::Blank => {}
                CellInk::Glyph => r.ink[col >> 6] |= bit,
                CellInk::Rule => {
                    r.ink[col >> 6] |= bit;
                    r.rule[col >> 6] |= bit;
                }
            }
            col += 1;
        }
    }

    #[inline]
    fn slot_for(&mut self, row: i32, width: usize) -> usize {
        let words = width.div_ceil(64);
        let mut found = None;
        let mut idx = 0;
        while idx < self.live {
            if self.rows[idx].row == row {
                found = Some(idx);
                break;
            }
            idx += 1;
        }
        let slot = if let Some(i) = found {
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
        let word = i >> 6;
        let shift = i & 63;
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
        let (w0, w1) = (lo >> 6, hi >> 6);
        let low_mask = u64::MAX << (lo & 63);
        let high_mask = u64::MAX >> (63 - (hi & 63));
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
    /// Where paper's stars were stood within the last [`PAPER_REST_S`] ([`Self::stand`]).
    stood: Vec<Stood>,
    /// A test's negative control: a dark star kept off the caret's row
    /// beside it decided every frame from the caret then, as looks9 round 3
    /// decided it ([`Self::beside_caret`]).
    #[cfg(test)]
    beside_caret_each_frame: bool,
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
        if quads.is_empty() && halos.is_empty() {
            return None;
        }
        let k0_len = keys.len();
        keys.reserve(quads.len() + halos.len());
        let mut rect = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        let mut any = false;
        let mut qi = 0;
        while qi < quads.len() {
            let q = &quads[qi];
            keys.push(sky_quad_key(q));
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
            qi += 1;
        }
        let mut hi = 0;
        while hi < halos.len() {
            let h = &halos[hi];
            keys.push(sky_halo_key(h));
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
            hi += 1;
        }
        if !any {
            keys.truncate(k0_len);
            return None;
        }
        Some(Self {
            at,
            reach,
            rect,
            lights: (k0_len as u32, keys.len() as u32),
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

#[cfg(any(test, debug_assertions))]
thread_local! {
    static PAPER_FLIES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A test's negative control: `f` run with paper's stars flying as a dark page's do, as they did
/// before [`Stardust::stand`] ([`Star::still`]). Debug and test builds only; the flag is this
/// thread's and is put back as it was when `f` returns or panics.
#[cfg(any(test, debug_assertions))]
pub fn with_paper_stars_flying<R>(f: impl FnOnce() -> R) -> R {
    struct Back(bool);
    impl Drop for Back {
        fn drop(&mut self) {
            PAPER_FLIES.with(|c| c.set(self.0));
        }
    }
    let _back = Back(PAPER_FLIES.with(|c| c.replace(true)));
    f()
}

/// One star stood on paper: its centre and reach, px, and its birth ([`Stardust::stand`]).
#[derive(Clone, Copy, Debug)]
struct Stood {
    x: i32,
    y: i32,
    reach: i32,
    born: Instant,
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
            stood: Vec::with_capacity(STOOD_CAP),
            #[cfg(test)]
            beside_caret_each_frame: false,
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

    /// Every star in the pool as a slice, finishing ones included.
    #[inline]
    #[must_use]
    pub(super) const fn live_stars(&self) -> &[Star] {
        self.stars.as_slice()
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
    #[inline]
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

    /// Stand a newborn star still on paper ([`Star::still`]); a dark page's star flies and is left
    /// as it is. `false` when paper has no rested place for it.
    ///
    /// The star stands at a seeded point of its own flight (the slipstream's drift, a throw, a
    /// shed fragment's drag), so a line's dust lies strewn along it as the flying grains did in
    /// any one frame; a star with no flight stands where it was born. The place must be rested:
    /// no star born within [`PAPER_REST_S`] may reach a pixel this one reaches, so no pixel is
    /// darkened and let go more than three times a second however fast grains are dealt. A place
    /// that is not rested is passed over for the next point of the flight, then for steps to each
    /// side of it along the row, [`STAND_TRIES`] of each, then for the same steps one star's
    /// height up at a time, [`STAND_ROWS`] heights in all.
    fn stand(&mut self, star: &mut Star, ctx: &Ctx<'_>) -> bool {
        if ctx.cfg.dark_theme {
            return true;
        }
        #[cfg(any(test, debug_assertions))]
        if PAPER_FLIES.with(std::cell::Cell::get) {
            return true;
        }
        let at = star.born;
        self.stood
            .retain(|s| at.saturating_duration_since(s.born).as_secs_f32() < PAPER_REST_S);
        let geom = ctx.geom;
        let reach = star.class.arms(geom.ch as f32).1 + 1;
        let origin = star.origin();
        let life_ms = star.life_s() * 1000.0;
        let flown = star.v0.0 != 0.0 || star.v0.1 != 0.0;
        let mut end_ms: f32 = 0.0;
        if flown {
            end_ms = star.lane.motion().end_ms(life_ms);
        }
        if star.stream > 0.0 {
            end_ms = end_ms.max(STREAM_END_MS);
        }
        let end_s = end_ms.min(life_ms) * (1.0 / 1000.0);
        let sky = star.lane.is_sky();
        let h0 = hash01(mix32(star.seed ^ SALT_STAND));
        let step = 2 * reach + 1;
        let tries = if end_s > 0.0 { STAND_TRIES } else { 1 };
        let mut first = None;
        let mut k = 0;
        while k < tries + 2 * STAND_TRIES * STAND_ROWS {
            let p = if k < tries {
                // The golden ratio's steps spread the tries along the flight from a seeded start.
                let u = (h0 + k as f32 * 0.618_034).fract();
                star.flight_at_age(u * end_s)
            } else {
                let Some((x, y)): Option<(i32, i32)> = first else {
                    break;
                };
                let j = ((k - tries) % (2 * STAND_TRIES)) as i32;
                let up = ((k - tries) / (2 * STAND_TRIES)) as i32;
                let side = if j % 2 == 0 { 1 } else { -1 };
                (x + side * (j / 2 + 1 - up.min(1)) * step, y - up * step)
            };
            k += 1;
            let clear = on_glass(p, geom)
                && if sky {
                    let col = px_col(p.0 as f32, geom);
                    col >= 0
                        && self
                            .probe
                            .sky_at(px_row(p.1 as f32, geom), col as u16, geom.rows)
                            == Some(false)
                } else {
                    !matches!(self.probe.at_px(p.0, p.1, geom), Some(true))
                };
            if !clear {
                continue;
            }
            if first.is_none() {
                first = Some(p);
            }
            let rested = !self.stood.iter().any(|s| {
                (p.0 - s.x).abs() <= reach + s.reach && (p.1 - s.y).abs() <= reach + s.reach
            });
            if rested {
                star.still = Some((p.0 - origin.0, p.1 - origin.1));
                if self.stood.len() == STOOD_CAP {
                    self.stood.remove(0);
                }
                self.stood.push(Stood {
                    x: p.0,
                    y: p.1,
                    reach,
                    born: at,
                });
                return true;
            }
        }
        false
    }

    /// The cat catches a star: put the one live, unfinished gold sky m1 whose home pixel and birth
    /// edge are `(x, y, born)` on its catch finish, on the frame the paw lands.
    #[inline]
    pub fn catch(&mut self, x: f32, y: f32, born: Instant, at: Instant) -> bool {
        let mut found = None;
        let mut i = 0;
        while i < self.stars.len() {
            let s = &self.stars[i];
            if s.gold
                && s.class == StarClass::M1
                && s.lane.is_sky()
                && !s.dead(at)
                && s.born == born
                && s.x == x
                && s.y == y
            {
                found = Some(i);
                break;
            }
            i += 1;
        }
        let Some(idx) = found else {
            return false;
        };
        let star = &mut self.stars[idx];
        let before = star.finish;
        star.finish_by(at, CATCH_FINISH_MS / 1000.0);
        star.finish != before
    }

    /// Shed fragments (§6.5 layer 5): `spec.n` m3 grains born at `spec.at_px`, each inheriting
    /// [`SHED_ALONG_SHARE`] of the head's velocity plus a seeded perpendicular sputter, under
    /// [`DRAG_TAU_MS`].
    #[inline]
    pub fn sow_shed(&mut self, at: Instant, spec: ShedSow, ctx: &Ctx<'_>) {
        if spec.n == 0 {
            return;
        }
        let (vx, vy) = spec.v_head;
        let speed = crate::color_math::hypot_f32(vx, vy).max(f32::EPSILON);
        let inv_speed = 1.0 / speed;
        let (ux, uy) = (vx * inv_speed, vy * inv_speed);
        let (along_x, along_y) = (vx * SHED_ALONG_SHARE, vy * SHED_ALONG_SHARE);
        let scale = px_scale(ctx.geom.ch as f32);
        let tint = spectrum_snap(spec.tint_t);
        let mut k = 0;
        while k < spec.n {
            let seed = mix32(spec.seed ^ u32::from(k).wrapping_mul(0x9E37_79B9));
            k += 1;
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
    #[inline]
    pub fn sow_fan(&mut self, at: Instant, spec: FanSow, ctx: &Ctx<'_>) {
        let n = usize::from(spec.n).max(1);
        let inv_n = 1.0 / (n as f32);
        let inv_cw = 1.0 / (ctx.geom.cw as f32).max(1.0);
        let (rise_up, rise_down) = fan_rise(spec.reach, spec.at_px, ctx.geom);
        let mut k = 0;
        while k < n {
            let seed = mix32(spec.seed ^ (k as u32).wrapping_mul(0x85EB_CA6B));
            let class = fan_class(k, spec.hero);
            if class == StarClass::M3 && !light_keeps(seed, ctx.cfg) {
                k += 1;
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
            k += 1;
        }
    }

    /// The mini-fan (§6.12, D17): a same-row nav hop of 2-7 cells.
    #[inline]
    pub fn sow_mini_fan(&mut self, at: Instant, at_px: (f32, f32), seed: u32, ctx: &Ctx<'_>) {
        let n = MINI_FAN_M2_N + MINI_FAN_M3_N;
        const INV_N: f32 = 1.0 / (MINI_FAN_M2_N + MINI_FAN_M3_N) as f32;
        let scale = px_scale(ctx.geom.ch as f32);
        let mut k = 0;
        while k < n {
            let s = mix32(seed ^ u32::from(k).wrapping_mul(0xC2B2_AE35));
            let class = if k < MINI_FAN_M2_N {
                StarClass::M2
            } else {
                StarClass::M3
            };
            if class == StarClass::M3 && !light_keeps(s, ctx.cfg) {
                k += 1;
                continue;
            }
            let a = ((f32::from(k) + hash01(s)) * INV_N) * std::f32::consts::TAU;
            let reach = (MINI_FAN_REACH_MIN_PX
                + (MINI_FAN_REACH_MAX_PX - MINI_FAN_REACH_MIN_PX) * hash01(mix32(s ^ 5)))
                * scale;
            let (sa, ca) = crate::supernova::sin_cos_f32(a);
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
            k += 1;
        }
    }

    /// Draw the sky.
    #[inline]
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.reduced = ctx.cfg.reduced_motion;
        self.cell_h = ctx.geom.ch as f32;
        let mut gi = 0;
        while gi < self.glints.len() {
            let g = self.glints[gi];
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
            gi += 1;
        }
        self.glints.clear();
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
        let mut si = 0;
        while si < self.stars.len() {
            classes |= 1u8 << (self.stars[si].class as u8);
            si += 1;
        }
        let ch = ctx.geom.ch as f32;
        let table = relative_luminance_table();
        let mut i = 0usize;
        const ALL_CLASSES: [StarClass; 3] = [StarClass::M1, StarClass::M2, StarClass::M3];
        let mut ci = 0;
        while ci < ALL_CLASSES.len() {
            let class = ALL_CLASSES[ci];
            ci += 1;
            if classes & (1u8 << (class as u8)) == 0 {
                continue;
            }
            let arms = class.arms(ch);
            let mut si = 0;
            while si < self.stars.len() {
                let star = &self.stars[si];
                si += 1;
                if star.class != class {
                    continue;
                }
                let turn = i;
                i += 1;
                if !star.inks(ctx.cfg) {
                    continue;
                }
                #[cfg(not(test))]
                if star.beside_caret == Some(true) {
                    continue;
                }
                #[cfg(test)]
                if !self.beside_caret_each_frame && star.beside_caret == Some(true) {
                    continue;
                }
                let Some(paint) =
                    Paint::of_kept(star, &self.probe, &self.shade, &self.keep_out, ctx, arms)
                else {
                    continue;
                };
                if self.beside_caret(si - 1, &paint, ctx) {
                    continue;
                }
                if !ctx.cfg.dark_theme && self.stars.get(si - 1).is_some_and(|star| star.edge_cut) {
                    continue;
                }
                let left = quad_cap.saturating_sub(frame.out.len());
                if left == 0 {
                    self.sky_index = index;
                    return;
                }
                let share = (left / (n - turn)).max(M3_QUADS).min(left);
                let star_cap = frame.out.len() + share;
                let (q0, h0) = (frame.out.len(), frame.halos.len());
                if ctx.cfg.dark_theme {
                    paint.draw_add(table, ctx, frame, star_cap, &mut haloed, &mut index);
                } else {
                    paint.draw_over(
                        table,
                        ctx.geom,
                        ctx.cfg,
                        &mut self.paper_ink,
                        frame,
                        halo_cap,
                    );
                    // A paper star is drawn whole or not at all, as every
                    // particle is ([`crate::cursor_glow::drop_cut_particle`]): on
                    // the settings card's last row its points stood cut flat by
                    // the card's bottom edge.
                    // Left out once, it is left out for its life.
                    let geom = ctx.geom;
                    if crate::cursor_glow::drop_cut_particle(
                        frame.out,
                        frame.halos,
                        (q0, h0),
                        (
                            geom.fx_left(),
                            geom.fx_top(),
                            geom.fx_right(),
                            geom.fx_bot(),
                        ),
                    ) && let Some(star) = self.stars.get_mut(si - 1)
                    {
                        star.edge_cut = true;
                    }
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

    /// Whether star `i`, about to be drawn as `paint`, stands on a dark
    /// page's caret row beside the caret's block ([`Ctx::beside_dark_caret`],
    /// looks9 Q10), read at its first paint and held for its life: read
    /// every frame, a lit star the caret typed past went dark and came back,
    /// and one born beside a resting caret popped in mid-life once typing
    /// moved the caret on (looks9 round 3's review). A star thrown by a
    /// landing beside the new caret is never drawn.
    #[inline]
    fn beside_caret(&mut self, i: usize, paint: &Paint, ctx: &Ctx<'_>) -> bool {
        #[cfg(test)]
        if self.beside_caret_each_frame {
            return ctx.beside_dark_caret((paint.x, paint.y), paint.reach());
        }
        if let Some(star) = self.stars.get_mut(i) {
            if let Some(cached) = star.beside_caret {
                return cached;
            }
            let here = ctx.beside_dark_caret((paint.x, paint.y), paint.reach());
            star.beside_caret = Some(here);
            here
        } else {
            ctx.beside_dark_caret((paint.x, paint.y), paint.reach())
        }
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
        if self.reduced {
            return false;
        }
        let mut si = 0;
        while si < self.stars.len() {
            if self.stars[si].brisk(now) {
                return true;
            }
            si += 1;
        }
        let mut vi = 0;
        while vi < self.veil.len() {
            if self.veil[vi].age_s(now) < EDGE_IN_S {
                return true;
            }
            vi += 1;
        }
        false
    }

    /// The cadence law: the next instant a star changes what is on glass, or `None` when the sky is
    /// empty (T6).
    #[inline]
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
        let mut vi = 0;
        while vi < self.veil.len() {
            if cad.at_tail_floor() {
                return cad.take();
            }
            let v = &self.veil[vi];
            vi += 1;
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
        let mut si = 0;
        while si < self.stars.len() {
            if cad.at_tail_floor() {
                return cad.take();
            }
            let s = &self.stars[si];
            si += 1;
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
                if !self.reduced && age < FIELD_MELT_START {
                    cad.tail(FIELD_MELT_START - age);
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
            } else if s.turning(age, now) {
                cad.tail(0.0);
            }
            if cad.at_tail_floor() {
                return cad.take();
            }
            if let Some(dt) = s.next_glint_toggle_at_age_s(age) {
                cad.tail(dt);
            }
            if let Some(dt) = s.next_lift_step_at_age_s(age) {
                cad.tail(dt);
            }
            if let Some(dt) = s.next_stream_step_at_age_s(age) {
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
        self.stood.clear();
        self.minted = 0;
        self.last_typed = None;
        self.ioi_ms = STREAM_IOI_MAX_MS;
    }

    /// Seam point 12 (`translate_scroll_state`), the sky's half: move every star with the viewport
    /// by `rows` cell heights and drop those that leave the grid.
    #[inline]
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
            for s in &mut self.stood {
                s.y -= dy as i32;
            }
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
            self.stood.clear();
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

    #[inline]
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
        const DARK_THRESHOLDS: (f32, f32, f32) = {
            let m1 = 1.0 / DEAL_M1_IN as f32;
            let m2 = m1 + (1.0 / DEAL_M2_IN as f32);
            let m3 = m2 + (1.0 / DEAL_M3_IN as f32);
            (m1, m2, m3)
        };
        const LIGHT_THRESHOLDS: (f32, f32, f32) = {
            let m1 = LIGHT_COUNT_SCALE * (1.0 / DEAL_M1_IN as f32);
            let m2 = m1 + LIGHT_COUNT_SCALE * (1.0 / DEAL_M2_IN as f32);
            let m3 = m2 + LIGHT_COUNT_SCALE * (1.0 / DEAL_M3_IN as f32);
            (m1, m2, m3)
        };
        let (scale, (m1, m2, m3)) = if ctx.cfg.dark_theme {
            (1.0, DARK_THRESHOLDS)
        } else {
            (LIGHT_COUNT_SCALE, LIGHT_THRESHOLDS)
        };
        let field_step = if ctx.disp >= FIELD_HOT_DISP {
            FIELD_SHARE_FLOW
        } else {
            FIELD_SHARE_COLD
        };
        let field_share = field_step
            .max(FIELD_SHARE_COLD + (FIELD_SHARE_FLOW - FIELD_SHARE_COLD) * clamp01(ctx.flow.heat));
        let scaled_field_share = scale * field_share;
        let earned = matches!(class, TypedClass::Capital | TypedClass::Bang);
        let (row, head) = ctx.caret;
        let n_cells = cells.max(1);
        let mut k = 0;
        while k < n_cells {
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
            let rung = if k + 1 == n_cells && !mended {
                ladder_rung(ctx.flow.combo)
            } else {
                None
            };
            let promoted = match dealt {
                Some(c) if (c as u8) < (StarClass::M2 as u8) => c,
                _ => StarClass::M2,
            };
            let (dealt, earned, tint) = match rung {
                Some(Rung::Promote) => (Some(promoted), earned, None),
                Some(Rung::White) => (Some(promoted), earned, Some((TINT_WHITE_RGB, false))),
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

            k += 1;
            if mended {
                continue;
            }
            if deal_p(h, SALT_FIELD, scaled_field_share) {
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

    #[inline]
    fn sow_exhaust(&mut self, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) {
        // A throw is motion (§6.11).
        if ctx.cfg.reduced_motion {
            return;
        }
        let birth_disp = match ctx.mend {
            Some(m) => ctx.birth_disp.max(m.disp),
            None => ctx.birth_disp,
        };
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
            let mut live = 0usize;
            let mut si = 0;
            while si < self.stars.len() {
                if self.stars[si].finish.is_none() {
                    live += 1;
                    if live + EXHAUST_HEADROOM > STAR_CAP {
                        return;
                    }
                }
                si += 1;
            }
        }
        let (row, col) = ctx.caret;
        let Some(c) = col.checked_sub(1) else {
            return;
        };
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
        let mut r = px_row(y - rise, g);
        let r_end = px_row(y, g);
        while r <= r_end {
            if self.probe.sky_at(r, c, g.rows) != Some(false) {
                return;
            }
            r += 1;
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
        let mut k = 0;
        while k < ERASE_M3_N + ERASE_M2_N {
            let class = if k < ERASE_M3_N {
                StarClass::M3
            } else {
                StarClass::M2
            };
            self.throw_erase(class, (row, col), k, at, ctx);
            k += 1;
        }
    }

    #[inline]
    fn deal_kill(&mut self, cells: u16, at: Instant, ctx: &Ctx<'_>) {
        let n = (cells / KILL_CELLS_PER_STAR).clamp(1, KILL_STAR_MAX);
        let (row, col) = ctx.caret;
        let last = ctx.geom.cols.saturating_sub(1).min(usize::from(u16::MAX)) as u16;
        let mut k = 0;
        while k < n {
            let class = if k % (ERASE_M3_N + ERASE_M2_N) == ERASE_M3_N {
                StarClass::M2
            } else {
                StarClass::M3
            };
            let at_col = col
                .saturating_add(k * KILL_CELLS_PER_STAR + KILL_CELLS_PER_STAR / 2)
                .min(last);
            self.throw_erase(class, (row, at_col), k, at, ctx);
            k += 1;
        }
    }

    #[inline]
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
        let (sa, ca) = crate::supernova::sin_cos_f32(a);
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

    #[inline]
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
            let mut i = 0;
            while i < self.stars.len() {
                let s = &mut self.stars[i];
                if grabbed != Some(Some(i)) && s.lane.is_sky() && star_over_cell(s, spec.cell, g) {
                    s.finish_by(at, EVICT_FINISH_MS / 1000.0);
                }
                i += 1;
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
        let (tint, gold) = match spec.tint {
            Some(tg) => tg,
            None => class_tint(class, seed, self.field_t(spec.cell, ctx, ribbon)),
        };
        let from_x = match grabbed {
            Some(Some(i)) => self.stars[i].pos(at, self.reduced).0 as f32,
            Some(None) => x - ctx.geom.cw as f32,
            None => x,
        };
        let pull = (x - from_x).max(0.0);
        let birth_disp = match spec.pull {
            Some(p) => ctx.birth_disp.max(p.disp),
            None => ctx.birth_disp,
        };
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
        let mut star = Star {
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
            beside_caret: None,
            edge_cut: false,
            still: None,
        };
        if !self.stand(&mut star, ctx) {
            return false;
        }
        if glint {
            self.glints.push(Glint {
                col: spec.cell.1,
                f,
            });
        }
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
            beside_caret: None,
            edge_cut: false,
            still: None,
        };
        if self.settle(&mut star, spec.fixed, ctx) && self.stand(&mut star, ctx) {
            self.sow(star);
        }
    }

    #[inline]
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
        let top = match ribbon.band(row, col) {
            Some(b) => b.top,
            None => default_band_top(g, ctx.cfg, row),
        }
        .min(cell_top);
        let j = mix32(seed ^ SALT_JITTER);
        let y = (top - ch * (SKY_BOTTOM_CH + (SKY_TOP_CH - SKY_BOTTOM_CH) * hash01(j)))
            .min(cell_top - 1.0);
        let (cx, _) = g.cell_center(row, col);
        let jx = (hash01(mix32(j ^ 11)) * 2.0 - 1.0) * SKY_JITTER_CW * g.cw as f32;
        let (x, y) = ((cx + jx).round(), y.round());
        if !on_glass((x as i32, y as i32), g) {
            return None;
        }
        if !self.pixel_taken(x, y) {
            return Some((x, y));
        }
        let x_lo = (cx - SKY_JITTER_CW * g.cw as f32).round();
        let x_hi = (cx + SKY_JITTER_CW * g.cw as f32).round();
        let y_lo = (top - SKY_TOP_CH * ch).round();
        let y_hi = (top - SKY_BOTTOM_CH * ch).min(cell_top - 1.0).round();
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
        let mut ni = 0;
        while ni < NUDGE.len() {
            let (dx, dy) = NUDGE[ni];
            let (nx, ny) = (x + dx, y + dy);
            if nx >= x_lo
                && nx <= x_hi
                && ny >= y_lo
                && ny <= y_hi
                && on_glass((nx as i32, ny as i32), g)
                && !self.pixel_taken(nx, ny)
            {
                return Some((nx, ny));
            }
            ni += 1;
        }
        None
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

    #[inline]
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

    #[inline]
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
        let mut pos = None;
        let mut i = 0;
        while i < self.stars.len() {
            let s = &self.stars[i];
            if s.lane.is_sky() && !s.dead(at) && star_over_cell(s, from, g) {
                pos = Some(i);
                break;
            }
            i += 1;
        }
        Some(pos)
    }

    #[inline]
    const fn settle(&self, star: &mut Star, fixed: bool, ctx: &Ctx<'_>) -> bool {
        #[inline]
        const fn settle_clear(
            probe: &GlyphProbe,
            p: (i32, i32),
            geom: Geom,
            home: Option<(i32, i32)>,
        ) -> bool {
            if !on_glass(p, geom) {
                return false;
            }
            if let Some((hr, hc)) = home
                && px_row(p.1 as f32, geom) == hr
                && px_col(p.0 as f32, geom) == hc
            {
                return true;
            }
            !matches!(probe.at_px(p.0, p.1, geom), Some(true))
        }
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
        let home = if !matches!(star.lane, StarLane::Exhaust) && star.lane.throw_ms().is_some() {
            Some((px_row(origin.1 as f32, geom), px_col(origin.0 as f32, geom)))
        } else {
            None
        };
        let motion = star.lane.motion();
        let k = motion.k_at(motion.end_ms(star.life_s() * 1000.0));
        let thrown = star.rest();
        let rest = clamp_to_glass(thrown, geom);
        if settle_clear(&self.probe, rest, geom, home) {
            if rest.0 != thrown.0 || rest.1 != thrown.1 {
                star.aim(rest, k);
            }
            return true;
        }
        let mut ni = 0;
        while ni < NUDGE.len() {
            let (dx, dy) = NUDGE[ni];
            let p = (rest.0 + dx, rest.1 + dy);
            if settle_clear(&self.probe, p, geom, home) {
                star.aim(p, k);
                return true;
            }
            ni += 1;
        }
        if !fixed {
            return false;
        }
        let (dx, dy) = ((rest.0 - origin.0) as f32, (rest.1 - origin.1) as f32);
        let steps = crate::color_math::hypot_f32(dx, dy).ceil().max(1.0) as i32;
        let inv_steps = 1.0 / steps as f32;
        let mut p = origin;
        let mut i = steps - 1;
        while i >= 0 {
            let t = i as f32 * inv_steps;
            let cand = (
                origin.0 + (dx * t).round() as i32,
                origin.1 + (dy * t).round() as i32,
            );
            if settle_clear(&self.probe, cand, geom, home) {
                p = cand;
                break;
            }
            i -= 1;
        }
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
        let mut busy = [false; (STRIKE_REACH_CELLS as usize) + 1];
        let col0 = cell.1 as i32;
        let col_max = cell.1.saturating_add(STRIKE_REACH_CELLS) as i32;
        let max_slots = (col_max - col0 + 1) as usize;
        let mut busy_count = 0usize;
        let stars = self.stars.as_slice();
        let mut i = 0;
        while i < stars.len() {
            let s = &stars[i];
            if s.lane.is_sky() {
                let sc = px_col(s.x, geom);
                if sc >= col0 && sc <= col_max && in_sky_of_row(s.y, cell.0, geom) {
                    let slot = (sc - col0) as usize;
                    if !busy[slot] {
                        busy[slot] = true;
                        busy_count += 1;
                        if busy_count == max_slots {
                            return None;
                        }
                    }
                }
            }
            i += 1;
        }
        let mut k = 0;
        while k <= STRIKE_REACH_CELLS {
            let c = cell.1.saturating_add(k);
            if !busy[(c - cell.1) as usize] {
                return Some((cell.0, c));
            }
            k += 1;
        }
        None
    }

    #[inline]
    fn adopt_field(&mut self, cell: (u16, u16), at: Instant, geom: Geom) -> bool {
        let mut found = None;
        let mut i = 0;
        while i < self.stars.len() {
            let s = &self.stars[i];
            if s.lane.is_sky() && star_over_cell(s, cell, geom) {
                found = Some(i);
                break;
            }
            i += 1;
        }
        let Some(idx) = found else {
            return false;
        };
        let s = &mut self.stars[idx];
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
        let mut i = 0;
        while i < self.stars.len() {
            let s = &mut self.stars[i];
            i += 1;
            if s.lane != StarLane::Field {
                continue;
            }
            if px_col(s.x, geom) >= col_i32 && in_sky_of_row(s.y, row, geom) {
                s.finish_by(at, span_s);
            }
        }
        let mut j = 0;
        while j < self.veil.len() {
            let v = &mut self.veil[j];
            if v.row == row && v.col >= col {
                v.finish_by(at, span_s);
            }
            j += 1;
        }
    }

    #[inline]
    fn relight_field(&mut self, caret: (u16, u16), at: Instant, geom: Geom) {
        let mut i = 0;
        while i < self.stars.len() {
            let s = &mut self.stars[i];
            if s.lane == StarLane::Field && in_sky_of_row(s.y, caret.0, geom) {
                s.lit = at;
            }
            i += 1;
        }
        let mut j = 0;
        while j < self.veil.len() {
            let v = &mut self.veil[j];
            if v.row == caret.0 {
                v.lit = at;
            }
            j += 1;
        }
    }

    #[inline]
    fn ember(&mut self, at: Instant) {
        const EMBER_S: f32 = FOCUS_EMBER_MS / 1000.0;
        let mut i = 0;
        while i < self.stars.len() {
            self.stars[i].finish_by(at, EMBER_S);
            i += 1;
        }
        let mut j = 0;
        while j < self.veil.len() {
            self.veil[j].finish_by(at, EMBER_S);
            j += 1;
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
        let mut i = 0;
        while i < self.stars.len() {
            let s = &self.stars[i];
            if s.finish.is_none() {
                live += 1;
                if !matches!(oldest, Some((_, born)) if s.born >= born) {
                    oldest = Some((i, s.born));
                }
            }
            i += 1;
        }
        if live >= STAR_CAP
            && let Some((idx, _)) = oldest
        {
            self.stars[idx].finish_by(now, EVICT_FINISH_MS / 1000.0);
        }
        if self.stars.len() >= STAR_POOL_MAX {
            let mut nearest_end: Option<(usize, f32)> = None;
            let mut j = 0;
            while j < self.stars.len() {
                let s = &self.stars[j];
                if let Some(f) = s.finish {
                    let u = f.u(now);
                    if !matches!(nearest_end, Some((_, best)) if u.total_cmp(&best).is_lt()) {
                        nearest_end = Some((j, u));
                    }
                }
                j += 1;
            }
            self.stars.remove(match nearest_end {
                Some((i, _)) => i,
                None => 0,
            });
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

    #[inline]
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
        let mut k = 0;
        while k < n {
            let col = head.saturating_sub(n - k);
            k += 1;
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
            let mut matched = None;
            let mut oldest: Option<(usize, Instant)> = None;
            let mut i = 0;
            while i < self.veil.len() {
                let v = &self.veil[i];
                if v.row == row && v.col == col {
                    matched = Some(i);
                    break;
                }
                if !matches!(oldest, Some((_, best)) if v.lit >= best) {
                    oldest = Some((i, v.lit));
                }
                i += 1;
            }
            if let Some(i) = matched {
                self.veil[i] = veil;
            } else if self.veil.len() < AURORA_CAP {
                self.veil.push(veil);
            } else if let Some((i, _)) = oldest {
                self.veil[i] = veil;
            }
        }
    }

    #[inline]
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
        let mut prev_row = u16::MAX;
        let mut row_y = (0i32, 0i32);
        let mut vi = 0;
        while vi < self.veil.len() {
            let v = &self.veil[vi];
            vi += 1;
            let env = v.envelope(ctx.now, ctx.cfg.reduced_motion);
            if env < CHROMA_CULL_ALPHA {
                continue;
            }
            let byte = cov_byte(AURORA_COV_PEAK * v.gain * env * intensity);
            if byte == 0 {
                continue;
            }
            let (y0, y1) = if v.row == prev_row {
                row_y
            } else {
                let bottom = v.bottom_px(g, ctx.cfg);
                let top = bottom - tall;
                prev_row = v.row;
                row_y = (top.round() as i32, bottom.round() as i32);
                row_y
            };
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
    px_col(star.x, geom) == cell.1 as i32 && in_sky_of_row(star.y, cell.0, geom)
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
    /// Its arm's reach plus one, px: the reach every cull keys on
    /// ([`Self::kept_out`], [`SkyMark::reach`]).
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
        arms: (i32, i32),
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
        (arm_lo, arm_hi): (i32, i32),
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
        let peak_reach = arm_hi.max(arm_lo) + 1;
        if shade.hides((x, y), peak_reach, ctx.geom.cw) {
            return None;
        }
        if let Some(ko) = keep_out
            && Self::kept_out(x, y, peak_reach, ctx, ko)
        {
            return None;
        }
        let in_hold = age_s < star.class.envelope_s(star.lane).0;
        // Ink on paper only cools: born at its full core with its glints, it drops the glints
        // once at the hold's end and fades on its envelope, with no luminance clock. The clock's
        // dip to its trough and back is a flash of every pixel of the star on paper (ink at
        // 0.55 of its coverage reads a quarter of the page's light brighter), 5.7 turns a
        // second, which leaves no room under the bound for the star's own birth.
        let paper = !ctx.cfg.dark_theme;
        let held = rm || paper || in_hold;
        let can_glint = !rm && star.gold && star.class == StarClass::M1;
        let tw = if !held || (can_glint && !paper) {
            twinkle_env(age_s, star.lum_phase())
        } else {
            1.0
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
        // One arm for the star's life. It stepped between two sizes 16 to 24 times a second
        // (`sin²` at 8 to 12 Hz), which is that many flashes of the arm's tip.
        let arm = arm_hi;
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
            glints: can_glint && if paper { in_hold } else { twinkle_peak(tw) },
            gold: star.gold,
        })
    }

    #[inline]
    fn draw_add(
        &self,
        table: &RelativeLuminanceTable,
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
        let watch_offsets = [
            (0, 0),
            (d, 0),
            (-d, 0),
            (0, d),
            (0, -d),
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
        ];
        let mut wi = 0;
        while wi < watch_len {
            let (dx, dy) = watch_offsets[wi];
            let p = (self.x + dx, self.y + dy);
            watch[wi] = (p, index.ground(ctx.cfg.theme_bg, frame, p.0, p.1));
            wi += 1;
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
                    table,
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
                    table,
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
        settle_under_ceiling(table, frame, q0, h0, &watch[..watch_len]);
    }

    #[inline]
    fn draw_m3(&self, geom: Geom, out: &mut Vec<GlowQuad>, quad_cap: usize) {
        let core = premul_rgb(self.tint, cov_byte(self.c * M3_CENTRE_ADD * self.core));
        push_fx_rect(out, geom, self.x, self.y, 1, 1, core);
        if out.len() >= quad_cap {
            return;
        }
        let disc = premul_rgb(self.tint, cov_byte(self.c * M3_CROSS_ADD));
        if disc == 0 {
            return;
        }
        let a = self.arm;
        let rects = [
            (self.x - a, self.y, a, 1),
            (self.x + 1, self.y, a, 1),
            (self.x, self.y - a, 1, a),
            (self.x, self.y + 1, 1, a),
        ];
        let mut i = 0;
        while i < rects.len() {
            if out.len() >= quad_cap {
                return;
            }
            let (x, y, w, h) = rects[i];
            push_fx_rect(out, geom, x, y, w, h, disc);
            i += 1;
        }
    }

    #[inline]
    fn draw_stubs(&self, geom: Geom, out: &mut Vec<GlowQuad>, quad_cap: usize) {
        if out.len() >= quad_cap {
            return;
        }
        let c = premul_rgb(self.tint, cov_byte(self.c * STUB_COV_SHARE));
        if c == 0 {
            return;
        }
        let d = star_body_px(self.arm) + 1;
        let l = stub_len_px(geom.ch as f32);
        let rects = [
            (self.x - d - (l - 1), self.y, l, 1),
            (self.x + d, self.y, l, 1),
            (self.x, self.y - d - (l - 1), 1, l),
            (self.x, self.y + d, 1, l),
        ];
        let mut i = 0;
        while i < rects.len() {
            if out.len() >= quad_cap {
                return;
            }
            let (x, y, w, h) = rects[i];
            push_fx_rect(out, geom, x, y, w, h, c);
            i += 1;
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

    #[inline]
    fn draw_over(
        &self,
        table: &RelativeLuminanceTable,
        geom: Geom,
        cfg: &Config,
        paper_ink: &mut PaperInkMemo,
        frame: &mut Frame<'_>,
        halo_cap: usize,
    ) {
        if frame.halos.len() >= halo_cap {
            return;
        }
        let rgb = paper_ink.get(table, self.tint, cfg);
        let mass = (self.arm_lo as f32 * STAR_ARM_INK).max(1.0);
        let step = (self.arm - self.arm_lo).max(0);
        match self.class {
            StarClass::M3 => {
                #[inline]
                const fn m3_ink(rgb: u32, cov: u8, cap: f32) -> u32 {
                    let a_raw = stacked_ink_alpha(cov, InkRole::OverText);
                    let cap_u = cap as u32;
                    let a = if a_raw < cap_u { a_raw } else { cap_u };
                    rgb | (a << 24)
                }
                let r = (mass.round() as i32).max(1);
                push_halo_quads(
                    frame.halos,
                    geom,
                    halo_cap,
                    (self.x, self.y),
                    (r, r),
                    m3_ink(rgb, cov_byte(self.c * self.core), LIGHT_ALPHA_CAP),
                    HaloMode::Over,
                );
                if frame.halos.len() + M3_NEEDLE_HALOS > halo_cap {
                    return;
                }
                let n = r + 1 + step;
                let tip = m3_ink(
                    rgb,
                    cov_byte(self.c * M3_CROSS_ADD),
                    InkRole::OverText.alpha_cap(),
                );
                let radii_pair = [(n, 1), (1, n)];
                let mut ri = 0;
                while ri < radii_pair.len() {
                    push_halo_quads(
                        frame.halos,
                        geom,
                        halo_cap,
                        (self.x, self.y),
                        radii_pair[ri],
                        tip,
                        HaloMode::Over,
                    );
                    ri += 1;
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
    class.arms(ch).0
}

/// The arm every star is drawn at, px.
#[cfg(test)]
#[inline]
const fn arm_peak_px(class: StarClass, ch: f32) -> i32 {
    class.arms(ch).1
}

#[inline]
#[must_use]
const fn sine_crossing_base(theta: f32, omega: f32) -> Option<f32> {
    use std::f32::consts::PI;
    if !(omega.is_finite() && theta.is_finite()) || omega <= 0.0 {
        return None;
    }
    let rem = theta % PI;
    Some(if rem < 0.0 { rem + PI } else { rem })
}

#[inline]
#[must_use]
const fn next_sine_crossing_from_base(base: f32, a0: f32, inv_omega: f32) -> Option<f32> {
    use std::f32::consts::PI;
    if !a0.is_finite() {
        return None;
    }
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
        Some(gap * inv_omega)
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
const SALT_STAND: u32 = 0x0000_0F5D;

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

#[inline]
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
    // The clip inside the overlay pixel space: every narrowing below is then exact.
    let space = crate::cursor_glow::OVERLAY_PX_MAX;
    let (br, bb) = (br.min(space), bb.min(space));
    let (rx, ry) = (radii.0.max(1), radii.1.max(1));
    let (cx, cy) = centre;
    let x0 = (cx - rx).max(bl);
    let x1 = (cx + rx).min(br);
    let y0 = (cy - ry).max(bt);
    let y1 = (cy + ry).min(bb);
    if x1 <= x0 || y1 <= y0 || cx < 0 || cy < 0 {
        return;
    }
    // A halo whose centre or radius the space cannot hold has only its dim tail in it.
    if cx > space || cy > space || rx > space || ry > space {
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
        let fin = match self.finish {
            Some(f) => f.gain(now),
            None => 1.0,
        };
        field_lane_envelope(self.age_s(now), self.idle_s(now), fin, reduced_motion)
    }

    /// True once the cell must leave the pool — the field star's own three exits ([`Star::dead`]).
    #[inline]
    #[must_use]
    pub fn dead(&self, now: Instant) -> bool {
        self.age_s(now) >= FIELD_LIFE_S
            || self.idle_s(now) >= FIELD_FADE_LIFE_S
            || matches!(self.finish, Some(f) if f.gain(now) < STAR_CULL_ALPHA)
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
            let mut i = 0;
            while i < self.dirty.len() {
                let s = self.dirty[i] as usize;
                self.quad_head[s] = SKY_INDEX_END;
                self.halo_head[s] = SKY_INDEX_END;
                i += 1;
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

    #[inline]
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
        let mut cy = cy0;
        while cy <= cy1 {
            let cy_term = (cy as u32).wrapping_mul(0x85EB_CA6B);
            let mut cx = cx0;
            while cx <= cx1 {
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
                cx += 1;
            }
            cy += 1;
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

#[inline]
fn settle_under_ceiling(
    table: &RelativeLuminanceTable,
    frame: &mut Frame<'_>,
    q0: usize,
    h0: usize,
    watch: &[((i32, i32), u32)],
) {
    #[inline]
    const fn own_at_max(
        star_quads: &[GlowQuad],
        star_halos: &[RainHalo],
        maskable: bool,
        (x, y): (i32, i32),
    ) -> (u32, u32, u32, u8) {
        let mut own = 0u32;
        let mut qmask = 0u32;
        let mut idx = 0;
        while idx < star_quads.len() {
            let q = &star_quads[idx];
            if quad_covers(q, x, y) {
                own = add_sat(own, q.color & 0x00FF_FFFF);
                if maskable {
                    qmask |= 1u32 << idx;
                }
            }
            idx += 1;
        }
        let mut halo_color = 0u32;
        let mut halo_wt = 0u8;
        let mut hi = 0;
        while hi < star_halos.len() {
            let h = &star_halos[hi];
            let wt = halo_weight_at(h, x, y);
            if wt > 0 {
                own = add_sat(own, premul_rgb(h.color, wt));
                halo_color = h.color;
                halo_wt = wt;
                break;
            }
            hi += 1;
        }
        (own, qmask, halo_color, halo_wt)
    }
    #[inline]
    const fn own_at(
        star_quads: &[GlowQuad],
        maskable: bool,
        k: u8,
        (x, y): (i32, i32),
        mut qmask: u32,
        halo_color: u32,
        halo_wt: u8,
    ) -> u32 {
        if k == 0 {
            return 0;
        }
        let mut own = 0u32;
        if maskable {
            while qmask != 0 {
                let idx = qmask.trailing_zeros() as usize;
                qmask &= qmask - 1;
                own = add_sat(own, premul_rgb(star_quads[idx].color, k));
            }
        } else {
            let mut qi = 0;
            while qi < star_quads.len() {
                let q = &star_quads[qi];
                if quad_covers(q, x, y) {
                    own = add_sat(own, premul_rgb(q.color, k));
                }
                qi += 1;
            }
        }
        if halo_wt > 0 {
            own = add_sat(own, premul_rgb(premul_rgb(halo_color, k), halo_wt));
        }
        own
    }
    #[derive(Clone, Copy)]
    struct OverSettlePixel {
        p: (i32, i32),
        ground: u32,
        qmask: u32,
        halo_color: u32,
        halo_wt: u8,
    }
    #[inline]
    const fn settle_fits(
        table: &RelativeLuminanceTable,
        over: &[OverSettlePixel],
        star_quads: &[GlowQuad],
        maskable: bool,
        k: u8,
    ) -> bool {
        let mut oi = 0;
        while oi < over.len() {
            let px = over[oi];
            if !under_ceil_with(
                table,
                add_sat(
                    px.ground,
                    own_at(
                        star_quads,
                        maskable,
                        k,
                        px.p,
                        px.qmask,
                        px.halo_color,
                        px.halo_wt,
                    ),
                ),
            ) {
                return false;
            }
            oi += 1;
        }
        true
    }
    #[inline]
    const fn fit_settle_byte(
        table: &RelativeLuminanceTable,
        over: &[OverSettlePixel],
        star_quads: &[GlowQuad],
        maskable: bool,
        want: u8,
    ) -> u8 {
        if want == 0 || settle_fits(table, over, star_quads, maskable, want) {
            return want;
        }
        if !settle_fits(table, over, star_quads, maskable, 0) {
            return 0;
        }
        let (mut lo, mut hi) = (0u8, want);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if settle_fits(table, over, star_quads, maskable, mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }
    if q0 >= frame.out.len() && h0 >= frame.halos.len() {
        return;
    }
    let star_quads = &frame.out[q0..];
    let star_halos = &frame.halos[h0..];
    let maskable = star_quads.len() <= u32::BITS as usize;
    let mut over = [OverSettlePixel {
        p: (0, 0),
        ground: 0,
        qmask: 0,
        halo_color: 0,
        halo_wt: 0,
    }; SETTLE_PIXELS];
    let mut over_len = 0usize;
    let mut wi = 0;
    while wi < watch.len() {
        let (p, ground) = watch[wi];
        let (own, qmask, halo_color, halo_wt) = own_at_max(star_quads, star_halos, maskable, p);
        if !under_ceil_with(table, add_sat(ground, own)) && over_len < over.len() {
            over[over_len] = OverSettlePixel {
                p,
                ground,
                qmask,
                halo_color,
                halo_wt,
            };
            over_len += 1;
        }
        wi += 1;
    }
    if over_len == 0 {
        return;
    }
    let over = &over[..over_len];
    let k = fit_settle_byte(table, over, star_quads, maskable, u8::MAX - 1);
    let mut qi = q0;
    while qi < frame.out.len() {
        let q = &mut frame.out[qi];
        q.color = premul_rgb(q.color, k);
        q.color2 = premul_rgb(q.color2, k);
        qi += 1;
    }
    let mut hi = h0;
    while hi < frame.halos.len() {
        let h = &mut frame.halos[hi];
        h.color = premul_rgb(h.color, k);
        hi += 1;
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
const fn lay_add(base: u32, rgb: u32, k: u8) -> u32 {
    if k == 0 {
        base
    } else {
        add_sat(base, premul_rgb(rgb, k))
    }
}

#[inline]
#[must_use]
const fn fit_lay_add(table: &RelativeLuminanceTable, base: u32, rgb: u32, want: u8) -> u8 {
    if want == 0 || under_ceil_with(table, lay_add(base, rgb, want)) {
        return want;
    }
    if !under_ceil_with(table, base) {
        return 0;
    }
    let (mut lo, mut hi) = (0u8, want);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if under_ceil_with(table, lay_add(base, rgb, mid)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

#[inline]
#[must_use]
const fn fit_stacked_centre(table: &RelativeLuminanceTable, base: u32, rgb: u32, want: u8) -> u8 {
    if want == 0 || under_ceil_with(table, stacked_centre(base, rgb, want)) {
        return want;
    }
    if !under_ceil_with(table, stacked_centre(base, rgb, 0)) {
        return 0;
    }
    let (mut lo, mut hi) = (0u8, want);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if under_ceil_with(table, stacked_centre(base, rgb, mid)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

#[inline]
#[must_use]
const fn price_centre(
    table: &RelativeLuminanceTable,
    ground: u32,
    achromatic: bool,
    body: (u32, u8),
    disc: (u32, u8),
    halo: (u32, u8),
) -> (u8, u8, u8) {
    if achromatic {
        let c0 = stacked_centre(ground, body.0, body.1);
        let c1 = lay_add(c0, disc.0, disc.1);
        let c2 = lay_add(c1, halo.0, halo.1);
        if under_ceil_with(table, c2) {
            return (body.1, disc.1, halo.1);
        }
        if !under_ceil_with(table, ground) {
            return (body.1, disc.1, halo.1);
        }
        let cov = if under_ceil_with(table, c0) {
            body.1
        } else {
            fit_stacked_centre(table, ground, body.0, body.1)
        };
        let centre = if cov == body.1 {
            c0
        } else {
            stacked_centre(ground, body.0, cov)
        };
        let d = fit_lay_add(table, centre, disc.0, disc.1);
        let centre = lay_add(centre, disc.0, d);
        let peak = fit_lay_add(table, centre, halo.0, halo.1);
        (cov, d, peak)
    } else {
        let c0 = lay_add(ground, halo.0, halo.1);
        let c1 = lay_add(c0, disc.0, disc.1);
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
            fit_lay_add(table, ground, halo.0, halo.1)
        };
        let lit = if peak == halo.1 {
            c0
        } else {
            lay_add(ground, halo.0, peak)
        };
        let d = if peak == halo.1 && under_ceil_with(table, c1) {
            disc.1
        } else {
            fit_lay_add(table, lit, disc.0, disc.1)
        };
        let lit = if peak == halo.1 && d == disc.1 {
            c1
        } else {
            lay_add(lit, disc.0, d)
        };
        let cov = fit_stacked_centre(table, lit, body.0, body.1);
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
    y_bg: f32,
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
        y_bg: 0.0,
        len: 0,
        entries: [(0, 0); 9],
    };

    #[inline]
    const fn get(&mut self, table: &RelativeLuminanceTable, tint: u32, cfg: &Config) -> u32 {
        let theme = (cfg.theme_fg & 0x00FF_FFFF, cfg.theme_bg & 0x00FF_FFFF);
        if self.theme.0 != theme.0 || self.theme.1 != theme.1 {
            self.theme = theme;
            self.y_bg = relative_luminance_with(table, theme.1);
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
        let rgb = star_ink_on_paper(table, tint, theme.0, self.y_bg);
        if len < self.entries.len() {
            self.entries[len] = (tint, rgb);
            self.len = (len + 1) as u8;
        }
        rgb
    }
}

#[inline]
const fn star_ink_on_paper(
    table: &RelativeLuminanceTable,
    tint: u32,
    fg_masked: u32,
    y_bg: f32,
) -> u32 {
    let (r, g, b) = ((tint >> 16) & 0xff, (tint >> 8) & 0xff, tint & 0xff);
    if r == g && g == b {
        fg_masked
    } else {
        paper_mark_ink_with_bg_lum(table, tint, y_bg)
    }
}

/// The light-theme star (§3.3, §5.2): a horizontal and a vertical arm (each a full-length tip lay
/// and a shorter body lay) on a round nucleus, plus four diagonal glint dots when gold, all
/// source-over falloff ellipses in the star's ink on the page.
#[inline]
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
    let rgb_body = rgb & 0x00FF_FFFF;
    let core_cap = u32::from(cov).clamp(1, InkRole::Leading.alpha_cap() as u32);
    let arm_cap = u32::from(cov).clamp(1, InkRole::OverText.alpha_cap() as u32);
    let core_ink = rgb_body | LAY_ALPHA[core_cap as usize];
    let arm_ink = if arm_cap == core_cap {
        core_ink
    } else {
        rgb_body | LAY_ALPHA[arm_cap as usize]
    };
    #[inline]
    const fn star_px(v: f32) -> i32 {
        let r = v.round() as i32;
        if r > 1 { r } else { 1 }
    }
    let waist = star_px((mass * STAR_WAIST).max(1.0));
    let body = star_px((mass * STAR_TAPER_BODY).max(1.0));
    let core = star_px((mass * STAR_CORE).max(1.0));
    let arm_px = star_px(mass) + step;
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
        let d = star_px(mass * STAR_GLINT);
        let gcap = ((f32::from(cov) * STAR_GLINT_COV) as u32)
            .clamp(1, InkRole::OverText.alpha_cap() as u32);
        let glint = rgb_body | (gcap << 24);
        let offsets = [(-d, -d), (d, -d), (-d, d), (d, d)];
        let mut gi = 0;
        while gi < offsets.len() {
            let (ox, oy) = offsets[gi];
            push_halo_quads(
                halos,
                geom,
                cap,
                (centre.0 + ox, centre.1 + oy),
                (1, 1),
                glint,
                HaloMode::Over,
            );
            gi += 1;
        }
    }
}

#[inline]
const fn stacked_ink_alpha(cov: u8, role: InkRole) -> u32 {
    const CENTRE_ALPHA: [u32; 256] = {
        let lays = STAR_OVER_CENTRE_LAYS as usize;
        let mut lut = [0u32; 256];
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
            let v = (1.0 - p) * 255.0;
            lut[b] = (if v > 1.0 { v } else { 1.0 }) as u32;
            b += 1;
        }
        lut
    };
    let cap_b = role.alpha_cap() as u8;
    let idx = if cov < cap_b { cov } else { cap_b } as usize;
    CENTRE_ALPHA[idx]
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "stardust_tests.rs"]
mod tests;

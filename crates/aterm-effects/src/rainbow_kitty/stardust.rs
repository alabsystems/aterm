// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stardust: the sky above the ribbon. A few stars of graded magnitude,
//! scintillating out of phase, dying by alpha at a snapped stop.
//!
//! Design: `RAINBOW-KITTY-V2.md` §5 (classes and ceilings, tint, birth zones,
//! scintillation, populations and the 40 cap, recipes, earned heroes), §13
//! (the token bucket) and D1/D2/D3/D5.
//!
//! ## The five rules (§5.1)
//!
//! 1. A star is a **peak**: the brightest pixel is the smallest thing.
//! 2. A star field is a **magnitude distribution**: a rare hero with a halo and
//!    glints, some medium stars, a few faint grains.
//! 3. A star **scintillates**, visibly opening and closing, out of phase with
//!    its neighbours.
//! 4. A star lives in the **sky**, never on a stroke.
//! 5. Stars are **few and bright**, and die by alpha at a snapped colour.
//!
//! How each rule is met:
//!
//! - Magnitude: [`StarClass`], three classes from an [`M1_COV_SKY`]-priced
//!   hero with halo and glints down to an [`M3_COV_SKY`] grain.
//! - Peak: §5.7's recipes composite centres [`CENTRE_M1_SKY`] /
//!   [`CENTRE_M2_SKY`] / [`CENTRE_M3_SKY`], each ≥ 2× its 8-neighbours.
//! - Colour: chroma is fixed at birth ([`Star::tint`]) and only coverage
//!   moves; the cull is at [`STAR_CULL_ALPHA`], never a grey speck. The tint
//!   sits where the eye finds it: [`STUB_LEN_PX`]-px arm tips, the halo, the
//!   grain's tinted disc ([`M3_CROSS_ADD`]).
//! - Motion: two clocks (§5.5), luminance on the 2.86 Hz [`twinkle_env`] and
//!   arm size on the seeded [`Star::f`] ∈ [8, 12] Hz, plus a φ + π flip so a
//!   warm and a cool neighbour never blink together.
//!
//! ## Everything is derived; nothing is stored
//!
//! A [`Star`] is nine flat `Copy` fields and there is no per-frame state.
//! Envelope, arm, coverage, phase, gold-ness, halo radius and position are
//! pure functions of `(born, seed, now)`, so a frame at the same `now` is
//! byte-identical on CPU and GPU (§18).
//!
//! ## The sky carries the rainbow
//!
//! * An m3 is the pure stop of the cell it rose from ([`Paint::draw_m3`]); an
//!   m2's body is [`M2_BODY_TINT`] of the way from white to its tint, its disc
//!   and stubs the tint ([`Paint::draw_add`]); the hero keeps a white-hot
//!   centre under a full-request halo ([`M1_HALO_SHARE`]). m1/m2 tints are
//!   dealt [`TINT_FIELD_SHARE`] / [`TINT_WHITE_SHARE`] / [`TINT_GOLD_SHARE`]
//!   = 75 / 10 / 15; a grain always wears its cell's stop ([`class_tint`]).
//! * The halo may not outshine the core ([`HALO_CAP_SHARE`]).
//! * The halo breathes its hue: [`HALO_HUE_SWING_ENTRIES`] table entries
//!   around the stop on the luminance clock ([`halo_rgb`]).
//! * A hot hand deals a denser sky: every laid cell carries a field star at
//!   `disp ≥` [`FIELD_HOT_DISP`] ([`FIELD_DEAL_IN_HOT`]).
//! * The aurora (section 8): a rainbow veil above the live ribbon, one
//!   [`Veil`] cell per laid cell, priced by momentum at birth and riding the
//!   field lane's envelope.

use aterm_render::{GlowQuad, HaloMode, RainHalo, add_sat, halo_row_ny, halo_weight, premul_rgb};
use aterm_time::Instant;

use super::timing::{
    CHROMA_CULL_ALPHA, EDGE_IN_S, FIELD_STAR_COV_CEIL, GLINT_CAP, GLINT_REFILL_PER_S,
    KILL_RETRACT_BASE_MS, REDUCED_MOTION_FADE_MS, STAR_CULL_ALPHA, STAR_STACK_ADD,
    TRANSIENT_STAR_COV_CEIL, clamp01, edge_in, half_life, half_life_life_s, kill_span_ms,
    smoothstep01, spend,
};
use super::{Cadence, Config, Ctx, Event, Frame, TypedClass};
use crate::color_math::relative_luminance;
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
    SPECTRUM_ANCHOR_AT, SPECTRUM_LUT, SPECTRUM_LUT_LEN, SPECTRUM_STOPS, spectrum, spectrum_snap,
    spectrum_stop,
};
use crate::trail_sound::SoundKind;

// ===========================================================================
// 1. The pool, the deal, the tint (§5.2, §5.3, §5.6)
// ===========================================================================

/// The live cap (§5.6); typical at 12 cps is ≈ 20. Over the cap the oldest
/// star is put on its [`EVICT_FINISH_MS`] finish and the newborn takes the
/// pool: a newborn is never refused. A finishing star is still on glass, so
/// the pool may briefly exceed this ([`STAR_POOL_MAX`]).
pub const STAR_CAP: usize = 40;

/// How long an evicted star has to finish (§5.6): its envelope is multiplied
/// by `spend(age_since_eviction / 40 ms)`, so it leaves on the theme's own
/// curve and never pops (T5).
/// `stars_over_the_cap_finish_oldest_first_within_forty_ms`.
pub const EVICT_FINISH_MS: f32 = 40.0;

/// The catch's shortened life, ms: a gold m1 the resident pet reached for is
/// put on this finish on the frame the paw lands, so it leaves on the theme's
/// curve rather than blinking out (T5). The same 40 ms as eviction: the last
/// lit frame is ~22 ms after the paw and the pool is clear at 40.
pub const CATCH_FINISH_MS: f32 = EVICT_FINISH_MS;

/// How many finishing stars the pool holds beyond [`STAR_CAP`] before the one
/// nearest the end of its finish is dropped. Eight evictions in 40 ms is
/// > 200 births/s, so this only makes the pool a fixed allocation (§18).
pub const STAR_FINISH_SLACK: usize = 8;

/// The pool's fixed capacity: [`STAR_CAP`] live plus [`STAR_FINISH_SLACK`]
/// finishing. Reserved once in [`Stardust::new`]; the steady frame path never
/// allocates (§18).
pub const STAR_POOL_MAX: usize = STAR_CAP + STAR_FINISH_SLACK;

/// The strike deal: one in this many typed cells gets an m3 grain (§5.6). The
/// three strike rates are one draw per cell, bucketed m1 / m2 / m3 / none
/// ([`Stardust::deal_typed`]), so a typed cell carries at most one strike
/// star.
pub const DEAL_M3_IN: u32 = 2;

/// One in this many typed cells gets an m2 (§5.6).
pub const DEAL_M2_IN: u32 = 4;

/// One in this many typed cells gets a dealt m1 hero (§5.6). A dealt hero with
/// no token degrades to m2 (D5); an earned one does not.
pub const DEAL_M1_IN: u32 = 12;

/// One in this many laid ribbon cells carries a field star: the ribbon's
/// persistent starfield (§5.6), hashed stably per cell so the sky does not
/// shimmer as the ribbon re-plans.
///
/// The pool is `live = cps · Σ deal · life`. With the field on its
/// swoosh-bounded life ([`FIELD_LIFE_S`]) and half of it m2
/// ([`FIELD_M2_IN`]), 1-in-2 gives ≈ 15 live at 12 cps hot, 41-50 % of them
/// m2/m1 (§5.6's ≥ 40 %), ≈ 125 quads and 6 halos (§18's typical row).
pub const FIELD_DEAL_IN: u32 = 2;

/// ...and one in this many of those is an m2 rather than an m3 (§5.6). At 1-in-2
/// the live sky is 41 % m2/m1 hot and 50 % cold.
pub const FIELD_M2_IN: u32 = 2;

/// The hot field deal: one in this many laid cells carries a field star once
/// the honest spine is at or over [`FIELD_HOT_DISP`], i.e. every cell
/// (owner, 2026-09-08).
/// Momentum buys density, on the honest metric (`ctx.disp`, not the
/// resume-floored birth spine).
///
/// Dealt rates per typed key: cold 1.33, at `disp ≥ 0.5` 2.33, at
/// `disp ≥ 0.7` 2.83; born rates are lower (hero spacing and birth
/// clearance). See `a_hot_hand_deals_every_cell_a_field_star` and
/// `a_twelve_cps_sky_holds_about_twenty_stars`.
pub const FIELD_DEAL_IN_HOT: u32 = 1;

/// The spine at or above which the field deal is [`FIELD_DEAL_IN_HOT`]
/// instead of [`FIELD_DEAL_IN`].
pub const FIELD_HOT_DISP: f32 = 0.7;

/// The share of laid cells that carry a field star at flow's `heat == 1`:
/// every one ([`FIELD_DEAL_IN_HOT`] as a probability).
///
/// Flow lerps from [`FIELD_SHARE_COLD`] to this over [`super::Flow::heat`],
/// folded with the hot step by `max`, so at `heat == 0` the deal is exactly
/// the cold one and flow can only open the sky further. It keeps the sky open
/// through a fast hand's dips under 0.7 `disp` while the run holds.
pub const FIELD_SHARE_FLOW: f32 = 1.0 / FIELD_DEAL_IN_HOT as f32;

/// The share of laid cells that carry a field star at `heat == 0`:
/// [`FIELD_DEAL_IN`] as a probability.
pub const FIELD_SHARE_COLD: f32 = 1.0 / FIELD_DEAL_IN as f32;

/// The combo ladder's first rung: every 16th key of a run takes an m2 where
/// the strike deal gave a grain or nothing (§23, "Flow state").
///
/// The ladder is deterministic (earned, never drawn) and never lowers a
/// magnitude. Every rung is dealt through [`Stardust::deal_star`], so the
/// spacing law, row cap, one-star-per-cell law, live cap and quad budget all
/// hold.
pub const LADDER_M2_KEYS: u32 = 16;

/// The second rung: every 32nd key of a run takes a white m2
/// ([`TINT_WHITE_RGB`]), so it reads as a different mark from the first.
pub const LADDER_WHITE_KEYS: u32 = 32;

/// The top rung: every 64th key of a run takes an earned gold m1
/// ([`TINT_GOLD_RGB`]), never demoted by spacing or a dry bucket (§5.8, D5).
/// It chimes only if a token is on hand. Past the first, every 64th also
/// fires the caret flare (`Engine::tick`).
pub const LADDER_GOLD_KEYS: u32 = 64;

/// A field star's life, s (§5.6: field stars ride their cell's envelope and
/// die with their cells).
///
/// A star cannot read its cell per frame, so its life is the cell's shape
/// (`edge-in` × the ribbon's `expiry_melt`) over the horizon at which the
/// cell is surely gone: the exit swoosh's whole span
/// (`ribbon::SWOOSH_TOTAL_S`, 1.54 s). Under a moving hand the sky trails
/// the hand by ~1.5 s.
///
/// This is the ceiling under a moving hand. After the hand stops, the field
/// fade ([`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`], clocked from the last key on
/// the star's row, [`Star::lit`]) culls it 0.88 s after that key, before the
/// ribbon's retract; the erase edge's finish is [`Stardust::on_event`].
pub const FIELD_LIFE_S: f32 = SWOOSH_TOTAL_S;

/// A field star's hold after the last key, ms: the `H` of the field lane's
/// half-life envelope (§5.6), clocked from the last key on the star's row
/// ([`Star::lit`]) rather than from birth, since a field star rides its
/// cell, which is lit while the hand is on its row.
///
/// Long enough to cover the gap between two keys (300 ms ≥ 3.3 cps) and no
/// longer. Each key's re-light ([`Stardust::relight_field`]) extends the
/// hold rather than re-brightening, so the field does not pump with the
/// hand. A star goes out from the hand, not from the horizon.
pub const FIELD_HOLD_MS: f32 = 300.0;

/// ...and its half-life, ms. With [`FIELD_HOLD_MS`]: `α = ½` at 550 ms after
/// the last key, 0.33 at 700 ms, and the cull ([`STAR_CULL_ALPHA`],
/// `H + 2.32·τ`) at 880 ms, before the ribbon's retract
/// (`ribbon::RETRACT_START_S`, 900 ms). Pinned by
/// `field_stars_dim_with_the_ribbon_they_were_born_over` and
/// `the_field_is_dark_before_the_retract_moves`.
pub const FIELD_TAU_MS: f32 = 250.0;

/// The field fade's span after the last key, seconds (`H + 2.32·τ` through
/// [`half_life_life_s`], 0.88 s): when [`Star::dead`] drops a field star
/// whose row the hand has left, and the end of the reduced-motion fade.
#[inline]
#[must_use]
pub fn field_fade_life_s() -> f32 {
    half_life_life_s(FIELD_HOLD_MS / 1000.0, FIELD_TAU_MS / 1000.0)
}

/// The spine above which a strike also throws a second m3 from the caret's
/// leading edge (§5.6).
pub const STRIKE_SECOND_M3_DISP: f32 = 0.5;

/// Minimum spacing between heroes, in cells (§5.2). Two heroes on adjacent
/// cells read as one fat mark, not as two stars.
pub const HERO_MIN_SPACING_CELLS: u16 = 3;

/// One sky star per cell: a strike's reach when its cell is taken, in cells
/// toward the caret's leading edge. A strike dealt onto a cell that already
/// carries a live sky star moves to the first free cell within this reach
/// (the lead cell, which the next key will lay) or is dropped; further would
/// put a star over glass the hand has not reached.
///
/// Stacked stars on one cell would each be priced only over the sky laid
/// before them, so a pile could outshine the caret. A cell has one star
/// ([`Stardust::sky_star_live`]), the field deal adopts the cell's existing
/// star ([`Stardust::deal_typed`]), and the strike moves within this reach.
/// Pinned by `one_sky_star_per_cell_and_no_pile_outshines_the_caret`.
pub const STRIKE_REACH_CELLS: u16 = 1;

/// At most this many heroes live on one row at a time (§5.2).
pub const HERO_MAX_LIVE_PER_ROW: u8 = 2;

/// Share of m1/m2 stars whose tint is `spectrum_snap(field at the birth
/// cell)` (§5.3): warm stars over the old end, violet over the new. Gold
/// keeps 15 % (the glint class) and white takes the rest. Grains are not
/// dealt: every m3 wears its cell's stop ([`class_tint`]) (owner, 2026-09-08).
pub const TINT_FIELD_SHARE: f32 = 0.75;

/// Share of m1/m2 that are `#FFFFFF` — an A-type star; halo and stubs white
/// (§5.3).
pub const TINT_WHITE_SHARE: f32 = 0.10;

/// How far from white toward its tint an m2's body and nucleus are drawn
/// (owner, 2026-09-08).
/// The stack still composites to §5.2's 130 on the tint's peak channel, with
/// a channel spread of `0.6 · 113 = 68` at the centre. The hero keeps a
/// white-hot centre (its colour is its halo); the grain is pure tint.
pub const M2_BODY_TINT: f32 = 0.6;

/// Share that are `#FFFF00` gold, the only class that carries diagonal
/// glints (§5.3). The three shares sum to 1.
pub const TINT_GOLD_SHARE: f32 = 1.0 - TINT_FIELD_SHARE - TINT_WHITE_SHARE;

/// The gold tint itself (§5.3), and the only value of [`Star::tint`] for which
/// `push_twinkle_star`'s `gold` argument may be true.
pub const TINT_GOLD_RGB: u32 = 0x00FF_FF00;

/// The A-type white tint (§5.3), and the body/nucleus colour of every class
/// (D2).
pub const TINT_WHITE_RGB: u32 = 0x00FF_FFFF;

/// How many of the seven ROYGBIV stops count as warm for §5.5's φ + π flip:
/// red, orange, yellow. A warm and a cool star on adjacent cells are exactly
/// antiphase, which stops a field blinking in unison.
pub const WARM_STOPS: usize = 3;

/// Top of the sky band above the ribbon, in `ch` above [`super::ribbon::Band`]'s
/// `top` (§5.4). Under the shipped `tall` spelling `[0.30, 0.04]` is the lower
/// third of row − 1; under `underline` it is `[0.02, 0.30]·ch` of the cell:
/// one zone, two spellings (D16).
pub const SKY_TOP_CH: f32 = 0.30;

/// …and its bottom, in `ch` above the band's top (§5.4).
pub const SKY_BOTTOM_CH: f32 = 0.04;

/// Horizontal jitter of a birth about the cell centre, in `cw` (§5.4, hashed).
pub const SKY_JITTER_CW: f32 = 0.3;

/// The 1× cell height every device-pixel size in this file is ruled at, in
/// `ch` (7×14 device px at 1×, 15×28 on a retina screen). Every `*_PX`
/// constant is stated at this cell and scaled by [`px_scale`] where spent,
/// like `star_arm`, so 2× is the 1× picture at twice the size
/// (`the_sky_at_two_x_is_the_sky_at_one_x_twice_the_size`).
pub const CELL_1X_CH: f32 = 14.0;

/// The device-pixel scale of a cell of height `ch` against the 1× cell:
/// `ch / CELL_1X_CH` — 1 at 1×, 2 on the retina cell, 9/7 at the test
/// fixture's `ch` 18.
#[inline]
#[must_use]
pub fn px_scale(ch: f32) -> f32 {
    ch / CELL_1X_CH
}

/// An integer pixel size ruled at 1×, at cell height `ch`: truncated like
/// `star_arm_px` (never rounding up on a fractional scale), floored at one
/// pixel. Exact at 1× and 2×.
#[inline]
#[must_use]
pub fn px_at(ch: f32, at_1x: i32) -> i32 {
    ((at_1x as f32 * px_scale(ch)) as i32).max(1)
}

/// Lower bound of the seeded arm-size scintillation rate, Hz (§5.5, D12).
pub const SCINT_F_MIN: f32 = 8.0;

/// Upper bound of the seeded arm-size rate, Hz (§5.5, D12).
pub const SCINT_F_MAX: f32 = 12.0;

/// The photosensitivity exemption bound, device px at 2× (§5.5): a mark whose
/// bbox is at or under this may scintillate in size at 8-12 Hz, since a
/// general-flash threshold is an area threshold. Luminance of every mark
/// stays on `twinkle_env`'s 2.86 Hz clock.
#[cfg(test)]
pub const SCINT_BBOX_MAX_PX: i32 = 10;

/// The m2's integer arm floor, px (§5.2, §5.5: "m2 … (2 ↔ 3 px)"). The arm
/// clock alone reaches 1 px at its trough, the grain's silhouette, so the m2
/// ↔ m3 step would flicker. Read by [`StarClass::arm_min_px`] and scaled by
/// the cell ([`px_at`]). m1 and m3 round onto their bands without one.
pub const M2_ARM_MIN_PX: i32 = 2;

/// Light-theme count scale (§5.2): counts × 0.6 in every population (strike,
/// field, erase, shed, fan, mini-fan), since area must not accumulate on a
/// light ground (L6). Sky deals scale their probability ([`deal`]); thrown
/// populations scale their census ([`light_count`]), never below the slots a
/// law fixes (the fan's hero + 4 m2, D6; the mini-fan's m2, D17; the erase
/// m2).
pub const LIGHT_COUNT_SCALE: f32 = 0.6;

/// Coverage divisor for a transient star whose current pixel sits over a
/// probed-occupied or unknown cell (§5.2's text-safe arm): a fan star over a
/// glyph, a shed fragment on an unprobed row.
///
/// A sky star never takes it (`Paint::of` gates on
/// [`StarLane::is_transient`]): its cell was proven blank at birth and it
/// lifts ≤ 2 px, while the host's probe forgets rows on Enter and scroll;
/// re-asking would step every live star to a third (T5, C3). Pinned by
/// `a_sky_star_keeps_its_centre_when_the_probe_forgets_its_row`.
pub const TEXT_SAFE_DIV: f32 = 3.0;

// ---------------------------------------------------------------------------
// 1a. The coverage requests: §5.2's two ceilings, one floor
// ---------------------------------------------------------------------------
//
// `c` is the coverage request handed to the class's recipe. Sky stars live
// above the ribbon for 225-460 ms and are priced by the sparkle law
// ([`FIELD_STAR_COV_CEIL`]); transient stars fly over ink for ≤ 340 ms and
// are priced by [`TRANSIENT_STAR_COV_CEIL`].

/// The m1 hero's coverage request in the sky lane (§5.2). `2.35 · 61 = 143`,
/// exactly [`FIELD_STAR_COV_CEIL`]: nothing in the sky is brighter.
pub const M1_COV_SKY: f32 = 61.0;

/// The m2's coverage request in the sky lane (§5.2). `2.70 · 48 = 130`.
pub const M2_COV_SKY: f32 = 48.0;

/// The m3 grain's coverage request in the sky lane (§5.2). `1.6 · 61 = 98`.
/// Equal to [`M1_COV_SKY`]: the grain has no bars or nucleus to stack, so the
/// class's shape, not its request, grades the sky.
pub const M3_COV_SKY: f32 = 61.0;

/// The m1's request in the transient lane (§5.2). `2.35 · 50 = 118`, exactly
/// [`TRANSIENT_STAR_COV_CEIL`] — the pin-nucleus brightness class.
pub const M1_COV_TRANSIENT: f32 = 50.0;

/// The m2's request in the transient lane (§5.2).
pub const M2_COV_TRANSIENT: f32 = 34.0;

/// The m3's request in the transient lane (§5.2). `1.6 · 38 = 61`.
pub const M3_COV_TRANSIENT: f32 = 38.0;

/// The m2's 3×3 disc, as a share of `c` (§5.7): what an m2 adds on top of
/// `push_twinkle_star`'s stack. A disc rather than a second core rect (D1
/// forbids a duplicate core): it widens the peak's shoulder by a pixel, which
/// separates an m2 from a small m1.
pub const M2_DISC_ADD: f32 = 0.35;

/// The m3's four arm rects, as a share of `c`: the grain's tinted Airy disc
/// (§5.7, D2). Each arm runs `arm` px outboard of the 1-px core
/// ([`M3_QUADS`]), so the centre pixel is the white core alone.
///
/// At `0.45 · 61 = 27` levels (transient 17) the disc registers as a star
/// rather than a speck. It rides alpha only: the white core breathes on `env`
/// over a steady coloured disc (§5.5's chromatic scintillation). The peak
/// law holds at every phase: 98 : 27 at the crest, 54 : 27 at the trough.
pub const M3_CROSS_ADD: f32 = 0.45;

/// The m3's composited centre as a multiple of `c`: §5.2's `1.6·c` (98 sky /
/// 61 transient). Nothing else stacks on the centre pixel.
pub const M3_CENTRE_ADD: f32 = 1.6;

/// The four outboard tint stubs, as a share of `c` (§5.7): each arm's
/// coloured tip, outside `star_body_px(arm)`. `push_twinkle_star` draws body
/// and nucleus in one colour (D2), so the stubs and the halo carry the
/// star's temperature. At the full request the tip is 61 / 48 levels of pure
/// tint, enough to read on the dark ground.
pub const STUB_COV_SHARE: f32 = 1.0;

/// How long each tint stub is, px at the 1× cell ([`CELL_1X_CH`]), running
/// outboard from `star_body_px(arm) + 1`: two, so the arm visibly ends in
/// colour. Still four quads per star. Spent through [`stub_len_px`].
pub const STUB_LEN_PX: i32 = 2;

/// [`STUB_LEN_PX`] at cell height `ch` ([`px_at`]).
#[inline]
#[must_use]
pub fn stub_len_px(ch: f32) -> i32 {
    px_at(ch, STUB_LEN_PX)
}

/// §3.2 rank 8: the halo may not outshine the core. A halo's peak byte stays
/// at or under this share of its star's composited centre, so the core is
/// the brightest pixel on every channel the halo lights. At this share the
/// ring of halo pixels just outside the body reads as colour on glass
/// (channel spread ≥ 40 at max channel ≥ 60;
/// `stardust_is_coloured_on_the_glass_not_white_with_a_tint`). The build
/// refuses a share over the cap (the `const _` block below).
pub const HALO_CAP_SHARE: f32 = 1.0;

/// The m1 halo's peak, as a share of `c` (§5.7): the hero's whole request,
/// 61 levels in the sky and 50 transient. A pure-stop halo needs ~62 levels
/// at its peak to put a spread-40 pixel just outside the hero's white body
/// (falloff 0.65 at `d = 4` px on an 11 px halo).
///
/// The `RainHalo` falloff is full weight at its centre, so the hero's centre
/// asks for `143 + 61` on the tint's channel. [`FIELD_STAR_COV_CEIL`] binds
/// the recipe's stack; what the centre gets is priced by the caret's law
/// ([`STAR_LIGHT_CEIL`], [`price_centre`]), with the white body ceding
/// luminance to the colour.
pub const M1_HALO_SHARE: f32 = 1.0;

/// The m2 halo's peak, as a share of `c` (§5.7): `0.80 · 48 = 38` levels in
/// the sky, 27 transient. A 6 px halo at 38 puts ~21 levels of tint three
/// pixels out, on top of the body's own tint.
pub const M2_HALO_SHARE: f32 = 0.80;

/// The m1 halo's radius as a multiple of its nominal arm (`star_arm(ch,
/// STAR_ARM_HERO)`, the float, not the clocked integer; §5.7): 8 px at 1×,
/// 11 at `ch` 18, 16 at 2×. Larger would reach glyph cells on the caret's
/// row (§3.4).
///
/// Taken off the nominal arm because the clocked integer arm truncates
/// differently per cell and steps at 8–12 Hz, which would put the halo's
/// size outside D12's exemption. Its luminance and hue breathe on the
/// 2.86 Hz clock ([`halo_rgb`]).
pub const M1_HALO_R_ARM: f32 = 3.0;

/// The m2 halo's radius as a multiple of its nominal arm (`star_arm(ch,
/// STAR_ARM_STD)`, §5.7), floored at [`M2_HALO_R_MIN_PX`]. It is under the
/// floor at every cell, so the floor is the m2's halo: 6 / 7.7 / 12 px.
pub const M2_HALO_R_ARM: f32 = 2.4;

/// The m2 halo's radius floor in px at the 1× cell (§5.7's `max(6,
/// 2.4·arm)`): below 6 px a radial falloff around a 5 px body reads as a
/// second core. Spent through [`m2_halo_r_min_px`].
pub const M2_HALO_R_MIN_PX: f32 = 6.0;

/// [`M2_HALO_R_MIN_PX`] at cell height `ch` ([`px_scale`]).
#[inline]
#[must_use]
pub fn m2_halo_r_min_px(ch: f32) -> f32 {
    M2_HALO_R_MIN_PX * px_scale(ch)
}

/// The m2 disc's shoulder: how far the tinted disc reaches past the centre
/// pixel on each axis, px at the 1× cell. One, so the disc is 3×3 at 1× and
/// 5×5 at 2× ([`m2_disc_shoulder_px`]).
pub const M2_DISC_SHOULDER_PX: i32 = 1;

/// [`M2_DISC_SHOULDER_PX`] at cell height `ch` ([`px_at`]).
#[inline]
#[must_use]
pub fn m2_disc_shoulder_px(ch: f32) -> i32 {
    px_at(ch, M2_DISC_SHOULDER_PX)
}

/// The halo breathes its hue: how far, in `SPECTRUM_LUT` entries, a halo's
/// colour walks from its star's stop on the luminance clock (+15 at the
/// twinkle peak, −15 at the trough; [`halo_rgb`]). The nearest anchors are
/// 63 entries apart, so the halo never wears a neighbouring name. Gold and
/// white hold their colour (owner, 2026-09-08).
pub const HALO_HUE_SWING_ENTRIES: i32 = 15;

/// The caret law on the star's own centre (`cursor_glow` §8 d: the brightest
/// pixel of the frame is under the cursor): the relative luminance no pixel
/// a sky star composites may exceed, over the ground it sits on.
///
/// Derived from the caret's dimmest lawful state: while a starfield is alive
/// the caret's fill is at least [`RAINBOW_CARET_LIGHT_FLOOR`] (`80/255`), and
/// the field gets [`RAINBOW_SPARKLE_LIGHT_SHARE`] of it (`Y = 72/255`), less
/// two levels for byte rounding. [`M1_COV_SKY`] was solved over black for
/// the stack alone, so over a real ground with a full halo the hero needs
/// this price.
///
/// It is an orientation law, not colour restraint: [`price_centre`] spends
/// the ceiling colour first, so a tinted halo (and the m2's disc) keeps its
/// request and the white body takes the luminance left; only an achromatic
/// star prices its body first. On a ground already over the ceiling the
/// request stands.
///
/// Held per star, per frame: priced on the composited centre
/// ([`price_centre`]), then settled on the pixels the recipe lights away from
/// the centre ([`settle_under_ceiling`]), both over [`sky_ground`] (the theme
/// ground plus everything the sky already laid this frame).
pub const STAR_LIGHT_CEIL: f32 = RAINBOW_CARET_LIGHT_FLOOR * RAINBOW_SPARKLE_LIGHT_SHARE / 255.0;

// ---------------------------------------------------------------------------
// 1a′. The aurora: the veil above the live ribbon
// ---------------------------------------------------------------------------
// (owner, 2026-09-08)
//
// A soft rainbow glow in the sky band above the live ribbon, carrying the
// ribbon's spectrum walk so the two read as one rainbow. One [`Veil`] cell
// per laid cell, born on the key that laid it, priced by momentum at birth,
// riding the field lane's envelope ([`field_lane_envelope`]) so it goes out
// with the hand and is gone before the ribbon's retract. It sits strictly
// above the glyph row, inside its cell's column, and only over a cell whose
// sky the probe proved blank.

/// The veil pool's fixed capacity, reserved once (§18). At 12 cps ~19 are
/// live under a hot hand; beyond the cap the cell lit longest ago is evicted.
pub const AURORA_CAP: usize = 64;

/// How tall the veil is, in `ch`: the box is `[bottom − 0.35 ch, bottom)`,
/// `bottom` being the sky band's anchor (the ribbon's top under `tall`, the
/// glyph row's top under `underline`, never lower than the glyph row's top).
pub const AURORA_TALL_CH: f32 = 0.35;

/// The veil's peak coverage byte at full momentum, at the line: its
/// brightest pixel, since the falloff is centred on the box's bottom edge and
/// each veil is clipped to its own column (no two veils sum).
pub const AURORA_COV_PEAK: f32 = 40.0;

/// Below this birth spine a key lays no veil. The gain is
/// `smoothstep((birth_disp − 0.15) / 0.85)`: 0.37 at 0.5, 0.71 at 0.7, 1.0 at
/// 1.0. Priced once at birth from `Ctx::birth_disp`, so a veil never
/// brightens without a keystroke.
pub const AURORA_COLD_DISP: f32 = 0.15;

/// The veil's horizontal falloff radius in `cw`, centred on the cell: at the
/// cell's own edge (`dx = 0.5 cw`) the weight is `0.88`, so a row of clipped
/// per-cell veils reads as one band with a 12 % scallop, not as beads.
pub const AURORA_REACH_CW: f32 = 2.0;

/// Veil halos stardust may draw per frame: one `RainHalo` per live cell (a
/// veil never splits across rows). Counted beside [`STARDUST_HALO_BUDGET`],
/// the star halo budget; on light both spend [`STARDUST_LIGHT_HALO_BUDGET`].
pub const AURORA_HALO_BUDGET: usize = AURORA_CAP;

/// The composited centre of a sky m1 — `STAR_STACK_ADD · c` (§5.2, §3.2 rank
/// 4). Rank 4 in the luminance hierarchy: brighter than the coma, dimmer than
/// the meteor nucleus.
pub const CENTRE_M1_SKY: f32 = STAR_STACK_ADD * M1_COV_SKY;

/// The composited centre of a sky m2 — stack plus the [`M2_DISC_ADD`] disc.
pub const CENTRE_M2_SKY: f32 = (STAR_STACK_ADD + M2_DISC_ADD) * M2_COV_SKY;

/// The composited centre of a sky m3 — the core rect alone, priced at
/// [`M3_CENTRE_ADD`] (the arms are outboard and stack nothing on it).
pub const CENTRE_M3_SKY: f32 = M3_CENTRE_ADD * M3_COV_SKY;

/// The composited centre of a transient m1.
pub const CENTRE_M1_TRANSIENT: f32 = STAR_STACK_ADD * M1_COV_TRANSIENT;

/// The composited centre of a transient m2: `STAR_STACK_ADD · c = 80` (§5.2,
/// §3.2 rank 4, §20.1).
///
/// The transient m2 carries no disc: §5.2's transient row is body, tapers
/// and stubs, and 80 holds only without the disc. The disc is the sky m2's
/// shoulder, lane-gated in [`Paint::draw_add`] (D1).
pub const CENTRE_M2_TRANSIENT: f32 = STAR_STACK_ADD * M2_COV_TRANSIENT;

/// The composited centre of a transient m3: `1.6 · 38 = 60.8`, published as
/// 61. [`Paint::draw_m3`] converts `round(1.6·c)` in one step, landing on it.
pub const CENTRE_M3_TRANSIENT: f32 = M3_CENTRE_ADD * M3_COV_TRANSIENT;

/// The two ceilings hold by construction (§5.2, D1): every term is a
/// constant, so a request that would break a ceiling fails the build. The
/// envelope and `intensity` only scale down, so the emit path needs no
/// runtime clamp.
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
    // §5.1 no. 2: the classes must actually be a distribution. Equal centres
    // would be one size again under three names.
    assert!(
        CENTRE_M3_SKY < CENTRE_M2_SKY && CENTRE_M2_SKY < CENTRE_M1_SKY,
        "the sky magnitudes must stay ordered m3 < m2 < m1"
    );
    // §3.2 rank 8: the halo may not outshine the core. At the twinkle's top
    // the core is `STAR_STACK_ADD · c` and the halo `share · c`; both lanes
    // price the same shares, so one ratio covers all four halos.
    assert!(
        M1_HALO_SHARE < HALO_CAP_SHARE * STAR_STACK_ADD
            && M2_HALO_SHARE < HALO_CAP_SHARE * STAR_STACK_ADD,
        "a star halo outshines the core it belongs to (§3.2 rank 8)"
    );
    // The m2's tinted body still stacks to a peak that is the tint's own
    // channel at full, so its composited centre is unchanged on that channel.
    assert!(
        M2_BODY_TINT > 0.0 && M2_BODY_TINT <= 1.0,
        "the m2 body tint is a share of the way from white to the tint"
    );
    // The halo's hue breath stays inside its stop's name: the nearest anchors
    // of `SPECTRUM_ANCHOR_AT` are 63 entries apart.
    assert!(
        HALO_HUE_SWING_ENTRIES > 0 && HALO_HUE_SWING_ENTRIES * 2 < 63,
        "the halo's hue breath reaches a neighbouring stop"
    );
    // The aurora: a coverage byte under the sparkle ceiling by a wide margin
    // (it is a veil, not a mark), and a pool the halo budget can carry.
    assert!(
        AURORA_COV_PEAK > 0.0 && AURORA_COV_PEAK <= 40.0,
        "the aurora's brightest pixel is over the brief's ~40"
    );
    assert!(
        AURORA_TALL_CH > 0.0 && AURORA_TALL_CH <= 0.35 && AURORA_HALO_BUDGET >= AURORA_CAP,
        "the aurora's box is taller than the brief's 0.35 ch, or its pool outruns its budget"
    );
    // D1 for the grain: its tinted disc stays under its white core at the
    // twinkle trough too (`core · 0.55`; the disc rides alpha only).
    assert!(
        M3_CENTRE_ADD * 0.55 >= 1.3 * M3_CROSS_ADD,
        "the grain's disc outshines its core at the twinkle trough"
    );
    // §5.3's deal must be a partition.
    assert!(
        TINT_GOLD_SHARE > 0.0 && TINT_FIELD_SHARE + TINT_WHITE_SHARE + TINT_GOLD_SHARE == 1.0,
        "the 75/10/15 tint deal must sum to one"
    );
    // The strike deal is a partition (one draw per cell, bucketed m1 / m2 /
    // m3 / none), so its three rates must fit inside one.
    assert!(
        1.0 / DEAL_M1_IN as f32 + 1.0 / DEAL_M2_IN as f32 + 1.0 / DEAL_M3_IN as f32 <= 1.0,
        "the 1-in-12 / 1-in-4 / 1-in-2 strike deals must fit inside one draw"
    );
};

// ---------------------------------------------------------------------------
// 1b. The envelopes: §5.6's `H` / `τ` / life table
// ---------------------------------------------------------------------------
//
// `α = 1` for a hold `H`, then half-life `τ`; cull at `α < 0.20`, so
// `life = H + 2.32·τ` ([`half_life_life_s`]). Keyed on (lane kind, class),
// not population: both sky populations share one column and the four
// transient populations the other, so a star's class can be read off its
// life.

/// Sky m1 hold, ms (§5.6 — strike m1 `120/146/460`).
pub const HOLD_M1_SKY_MS: f32 = 120.0;
/// Sky m1 half-life, ms. `120 + 2.32·146 = 459` — the published 460 ms life.
pub const TAU_M1_SKY_MS: f32 = 146.0;
/// Sky m2 hold, ms (§5.6 — `80/112/340`).
pub const HOLD_M2_SKY_MS: f32 = 80.0;
/// Sky m2 half-life, ms.
pub const TAU_M2_SKY_MS: f32 = 112.0;
/// Sky m3 hold, ms (§5.6: `40/80/225`). A 225 ms life is ≈ 2.3 cycles of the
/// 8-12 Hz arm clock, so a grain reads as a star, not one slow breath.
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
/// Transient m3 hold, ms (§5.6 — erase / shed / fan / mini-fan m3,
/// `20/67/175`).
pub const HOLD_M3_TRANSIENT_MS: f32 = 20.0;
/// Transient m3 half-life, ms.
pub const TAU_M3_TRANSIENT_MS: f32 = 67.0;

// ---------------------------------------------------------------------------
// 1c. Motion: three laws, one per population family (§5.6)
// ---------------------------------------------------------------------------

/// The sky lift for an m1/m2 strike star, px per second (§5.6: "lift −3 px/s,
/// integer-stepped, 1-2 px over life"). Negative is up. At 460 ms it is a
/// single one-pixel rise, so the star reads as sky rather than particle.
/// Stated at the 1× cell, spent through [`sky_lift_px_per_s`].
pub const SKY_LIFT_PX_PER_S: f32 = -3.0;

/// [`SKY_LIFT_PX_PER_S`] at cell height `ch` ([`px_scale`]).
#[inline]
#[must_use]
pub fn sky_lift_px_per_s(ch: f32) -> f32 {
    SKY_LIFT_PX_PER_S * px_scale(ch)
}

// ---------------------------------------------------------------------------
// 1d. The slipstream: the sky streams past a hand at speed
// ---------------------------------------------------------------------------

/// The slipstream's share: a sky star born under a hand at speed inherits a
/// backward drift (toward the pane's first column) of
/// `STREAM_SHARE_CW · cw / IOI` px/s, `IOI` being the interval between the
/// last two Typed keys ([`Stardust::note_cadence`]). At 12 cps that is 3.0
/// cells of whole travel, at 8 cps 2.0 ([`stream_travel_px`]).
///
/// At speed the sky streams past the hand; at a stroll it stands still
/// ([`STREAM_IOI_STROLL_MS`]); one star-life after the hand stops it is at
/// rest ([`STREAM_END_MS`]). The velocity is the observed cadence (T4); the
/// spine only gates the birth ([`STREAM_DISP`]). Field stars take it; the
/// aurora's veils do not.
///
/// The sky is the far layer: the exhaust (§33) runs at four times this share
/// ([`EXHAUST_SHARE_CW`]) through the same law, and the parallax reads as
/// depth. Travel is clamped at the row's blank frontier and the pane edge
/// ([`Stardust::stream_reach`]).
pub const STREAM_SHARE_CW: f32 = 1.05;
/// The drift's decay, ms: `v(age) = v₀ · e^(−age/τ)`, so the sky slows
/// from the instant it is born and a star's travel is bounded whatever the
/// hand does next.
pub const STREAM_TAU_MS: f32 = 300.0;
/// Where the drift ends, ms after birth: the longest sky class life (strike
/// m1, 460; `the_sky_is_at_rest_one_star_life_after_the_last_key`), so the
/// sky is at rest one star-life after the last key. The path is the
/// exponential's first `STREAM_END_MS` ([`stream_unit`]), then still; whole
/// travel is `v₀ · τ · STREAM_SPENT`.
pub const STREAM_END_MS: f32 = 460.0;
/// `1 − e^(−STREAM_END_MS / STREAM_TAU_MS)`, the share of the asymptote spent
/// by the drift's end, as a literal
/// (`the_stream_spent_share_is_the_exponential_s`).
pub const STREAM_SPENT: f32 = 0.784_11;
/// The IOI clamp's fast end, ms: the fastest priced hand (33 cps: 525 px/s on
/// a 16 px cell, 8.2 cells of travel, frontier-clamped).
pub const STREAM_IOI_MIN_MS: f32 = 30.0;
/// The IOI clamp's slow end, ms — and the IOI a session's first key is
/// priced at, having no interval to read (under the stroll gate: nothing).
pub const STREAM_IOI_MAX_MS: f32 = 600.0;
/// The stroll gate, OPEN: an IOI at or under this (≥ 6.7 cps) streams at
/// the whole priced velocity.
pub const STREAM_IOI_RUN_MS: f32 = 150.0;
/// The stroll gate, closed: an IOI at or over this (≤ 3 cps) streams
/// nothing; between the two gates the velocity is smoothstepped in. The
/// birth-spine gate alone cannot hold a slow sustained hand still.
pub const STREAM_IOI_STROLL_MS: f32 = 333.0;
/// The spine gate: only a star born at `birth_disp ≥ 0.5` inherits the
/// drift — the second grain's own threshold.
pub const STREAM_DISP: f32 = 0.5;

/// The exhaust's share, cells of `cw` per key interval (§33): the near layer,
/// four times the sky's, through the same gate, clamp and decay
/// ([`stream_px_per_s_with_share`]).
pub const EXHAUST_SHARE_CW: f32 = 4.0 * STREAM_SHARE_CW;

/// The slipstream's velocity, px/s backward, for cell width `cw` and
/// observed key interval `ioi_ms`: [`stream_px_per_s_with_share`] at
/// [`STREAM_SHARE_CW`]. Zero for a stroll, non-finite input, or no cell.
#[must_use]
pub fn stream_px_per_s(cw: f32, ioi_ms: f32) -> f32 {
    stream_px_per_s_with_share(STREAM_SHARE_CW, cw, ioi_ms)
}

/// The stream law at any share: `share · cw / IOI` px/s under the stroll gate
/// ([`STREAM_IOI_STROLL_MS`] → [`STREAM_IOI_RUN_MS`], smoothstepped), the IOI
/// clamped. Zero for a stroll, non-finite input, or no cell.
#[must_use]
fn stream_px_per_s_with_share(share: f32, cw: f32, ioi_ms: f32) -> f32 {
    if !(share.is_finite() && cw.is_finite() && ioi_ms.is_finite()) || cw <= 0.0 || share <= 0.0 {
        return 0.0;
    }
    let ioi = ioi_ms.clamp(STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS);
    let gate =
        smoothstep01((STREAM_IOI_STROLL_MS - ioi) / (STREAM_IOI_STROLL_MS - STREAM_IOI_RUN_MS));
    share * cw * 1000.0 / ioi * gate
}

/// The whole travel, px, of a star born at `px_per_s`: the drift's integral,
/// `v₀ · τ · STREAM_SPENT`.
#[must_use]
pub fn stream_travel_px(px_per_s: f32) -> f32 {
    px_per_s / 1000.0 * STREAM_TAU_MS * STREAM_SPENT
}

/// The drift's unit progress at `age_ms`, `0..=1`: the exponential's rise
/// normalised to reach exactly 1 at [`STREAM_END_MS`], and 1 thereafter.
/// The one curve the sky position and the cadence solver share.
#[inline]
#[must_use]
fn stream_unit(age_ms: f32) -> f32 {
    if age_ms >= STREAM_END_MS {
        return 1.0;
    }
    ((1.0 - (-age_ms.max(0.0) / STREAM_TAU_MS).exp()) / STREAM_SPENT).min(1.0)
}

// ---------------------------------------------------------------------------
// 1e. The mend's pull: the fix pulls the erased cell's light into the
//     caret's cell
// ---------------------------------------------------------------------------

/// The mend's pull window, ms. When a typed key mends a typo run
/// ([`super::Mend`], §23), its strike star is an m2 born at the erased cell's
/// sky (the star the Backspace was retiring, if live), pulled one cell into
/// the caret's over this window on the throw's ease-out (`1 − spend(age/T)`),
/// then pinned. Same window as the landing fan ([`FAN_THROW_MS`]), inside the
/// m2's hold, so the star is at full the whole way.
///
/// A per-star travel ([`Star::pull`]) rather than a lane motion, because the
/// pull must end, must ask for one brisk window ([`Star::brisk`]), and must
/// survive the next key's field adoption, which zeroes `v0`
/// ([`Stardust::adopt_field`]).
pub const MEND_PULL_MS: f32 = 110.0;

/// The drag time constant of a shed fragment, ms (§5.6). Displacement is the
/// closed form `v₀·τ·(1 − e^(−age/τ))`, a pure function of birth velocity and
/// age.
pub const DRAG_TAU_MS: f32 = 90.0;

// The throw window, per lane. A radial throw is `d = reach·(1 − (1 − u)²)`
// with `u = age / T` (§5.6, §6.5 layer 11), i.e. `1 − spend(u)`. This module
// owns the integrator: a caller hands a throw over as `v0 = reach / T` in
// px/ms ([`Star::v0`]), or hands [`Stardust::sow_fan`] /
// [`Stardust::sow_mini_fan`] the reach in px. Read the window back through
// [`StarLane::throw_ms`]. Every window is shorter than its population's
// life: a thrown star reaches its reach, then sits pinned and twinkling.

/// The erase throw's window, ms (§5.6: "radial throw 2-4 px over 80 ms
/// ease-out, up and away from the caret, then pinned + twinkling").
pub const ERASE_THROW_MS: f32 = 80.0;

/// The exhaust throw's window, ms (§33): how long a grain thrown out of the
/// caret's sky is on the wind.
///
/// Under the transient lives, so grains die on the wing rather than sitting
/// pinned mid-row: a transient m3 (175.6 ms) reaches its reach with 16 ms
/// left, while the 1-in-4 m2 ([`EXHAUST_M2_IN`], 245.9 ms) lands and
/// twinkles for 86 ms. The reach is the exhaust's own front at this instant
/// ([`stream_travel_px`] at [`EXHAUST_SHARE_CW`] × `stream_unit(160)`), so
/// the window and the reach move together.
pub const EXHAUST_THROW_MS: f32 = 160.0;

/// The exhaust's rise, cells of `ch`: how far a grain lifts over its throw,
/// dealt 0.5..1.0 of it by seed. Under `tall` the flight stays in `row − 1`;
/// under `underline` it can cross into the row above, so the frontier is
/// walked over every row the flight covers ([`Stardust::blank_frontier_px`],
/// D16). Along a TUI rule row the rise is clamped under the rule's stroke
/// ([`Stardust::rule_headroom_px`]).
pub const EXHAUST_RISE_CH: f32 = 0.15;

/// The exhaust's m2 deal: one grain in 4 is an m2, only where the birth spine
/// is at or over [`FIELD_HOT_DISP`]. Gated on the birth spine, never flow's
/// heat (`flow_draws_nothing_of_its_own`). The m2 outlives its throw and
/// twinkles where it lands, making a spark shower rather than a dust line.
pub const EXHAUST_M2_IN: u32 = 4;

/// The exhaust's headroom: nothing is thrown while the live sky is within
/// this many stars of [`STAR_CAP`], leaving room for what a key and a nav
/// gesture bring (a strike, a field star, a five-star fan and its shed), so a
/// spark never causes a hero's eviction.
pub const EXHAUST_HEADROOM: usize = 8;

/// The landing fan's window, ms.
///
/// Short enough that at the m2's hold (30 ms) the throw has covered 47 % of
/// the reach and 60 % at the hero's (40 ms), so all five stars clear a 15×28
/// caret cell (under the host's opaque caret) before their brightest hold is
/// spent. Life is unchanged (`H + 2.32·τ`, 315 ms for the hero): the star
/// reaches at 110 and twinkles there, pinned. The mini-fan follows
/// ([`MINI_FAN_THROW_MS`]).
pub const FAN_THROW_MS: f32 = 110.0;

/// The mini-fan's window, ms, on the same reasoning as [`FAN_THROW_MS`]: the
/// hop's pixels are covered inside the m2's hold.
pub const MINI_FAN_THROW_MS: f32 = 100.0;

// The five law-fixed fan stars (D6) must clear the caret cell inside their
// hold. At the transient m2's hold the throw has covered `1 − (1 − H/T)²` of
// the reach; the meteor's floor jump (8 cells) reaches `2.55 ch`, so at the
// jitter floor 0.55 an m2 is `0.66 ch` from the cell's centre at 30 ms,
// outside a cell with `cw ≤ 0.6 ch` on every ray. A 40 % share keeps that
// true; `a_fan_star_is_born_at_full_and_clears_its_cell_inside_its_hold`
// measures it.
const _: () = assert!(
    {
        let u = HOLD_M2_TRANSIENT_MS / FAN_THROW_MS;
        1.0 - (1.0 - u) * (1.0 - u) >= 0.40
    },
    "the fan's four m2 must be clear of the caret cell before their hold is spent"
);

/// Lower end of the fan's per-star throw jitter, `reach·(0.55..1.05)` (§6.5
/// layer 11).
pub const FAN_THROW_JITTER_MIN: f32 = 0.55;

/// Upper end of the fan's per-star throw jitter (§6.5 layer 11).
pub const FAN_THROW_JITTER_MAX: f32 = 1.05;

// The vertical reach law (§6.5 layer 11). §6.5 sizes the fan's reach by path
// length, but a row is one ch tall whatever the path, so the throw is an
// ellipse: squashed to [`FAN_SQUASH`] and never taller than
// [`FAN_RISE_MAX_CH`] above or below the landing row's centre, while the
// reach along the line keeps §6.5's formula. A ray exceeding the cap is
// flattened toward the line, keeping its length; the census (1 gold m1 +
// 4 m2, D6) is untouched. On row 0 the fan may rise into the `Geom.head`
// band to its top and no further.

/// The fan's vertical squash: distance buys the fan length along the line,
/// not height, at this aspect. The landing starburst's core is isotropic
/// (`meteor::BURST_CORE_CH`); what the two marks share is the rise cap below.
pub const FAN_SQUASH: f32 = 0.62;

/// The fan's rise ceiling, in `ch`, above and below the landing row's centre:
/// the smallest fan's reach (the Enter landing's D8 reach), so no fan rises
/// higher than the smallest fan is thrown.
pub const FAN_RISE_MAX_CH: f32 = 1.6;

/// Lower end of the per-star jitter on the rise cap, `cap·(0.80..1.00)`,
/// seeded, so re-aimed stars scatter under the ceiling instead of lining up
/// on it (§6.5 layer 11).
pub const FAN_RISE_JITTER_MIN: f32 = 0.80;

/// The focus-lost ember, ms (§8.2): every live star is put on a 300 ms
/// `spend` finish beside the ribbon's own ember.
pub const FOCUS_EMBER_MS: f32 = 300.0;

/// Minimum radial reach of an erase throw, px at the 1× cell (§5.6; scaled
/// by [`px_scale`] where thrown, like every reach and sputter below).
pub const ERASE_REACH_MIN_PX: f32 = 2.0;
/// Maximum radial reach of an erase throw, px at the 1× cell (§5.6).
pub const ERASE_REACH_MAX_PX: f32 = 4.0;
/// Minimum radial reach of a mini-fan throw, px at the 1× cell (§5.6: "1-2
/// px throw").
pub const MINI_FAN_REACH_MIN_PX: f32 = 1.0;
/// Maximum radial reach of a mini-fan throw, px at the 1× cell (§5.6).
pub const MINI_FAN_REACH_MAX_PX: f32 = 2.0;

/// Share of the meteor head's velocity a shed fragment inherits along the path
/// (§5.6: `v₀ = 0.12·v_head`).
pub const SHED_ALONG_SHARE: f32 = 0.12;
/// Minimum perpendicular sputter of a shed fragment, px/ms at the 1× cell
/// (§5.6). The along share needs no scale: the head's velocity is already in
/// the cell's pixels.
pub const SHED_PERP_MIN: f32 = 0.4;
/// Maximum perpendicular sputter of a shed fragment, px/ms at the 1× cell
/// (§5.6).
pub const SHED_PERP_MAX: f32 = 1.1;

/// Erase: how many m3 grains one Backspace throws (§5.6's "3 m3 + 1 m2").
pub const ERASE_M3_N: u16 = 3;
/// Erase: how many m2 stars one Backspace throws (§5.6).
pub const ERASE_M2_N: u16 = 1;
/// A kill mints one star per this many erased cells (§5.6, and [`Event::Kill`]'s
/// own restatement: "1 star per 3 cells, cap 8").
pub const KILL_CELLS_PER_STAR: u16 = 3;
/// …capped here, because a clause going is one gesture and not a shower (§5.6).
pub const KILL_STAR_MAX: u16 = 8;
/// A mini-fan is exactly this many m2 stars (§5.6, D17).
pub const MINI_FAN_M2_N: u16 = 1;
/// …and this many m3 grains (§5.6, D17).
pub const MINI_FAN_M3_N: u16 = 2;

// ---------------------------------------------------------------------------
// 1d. The budgets §18 holds this producer to
// ---------------------------------------------------------------------------

/// Quads stardust may spend on one frame (§18: "Stardust | 40 live | ≤ 284").
/// The worst case is the cap's census, `4·23 + 12·10 + 24·3`; the budget is
/// spent star by star, so an over-budget frame loses the last star's
/// faintest extremities rather than a whole magnitude.
pub const STARDUST_QUAD_BUDGET: usize = 284;

/// Haloed stars stardust may draw on one frame (§18: "≤ 16", the cap's 4 m1 +
/// 12 m2). Counted per star, not per `RainHalo`, since a halo splits into one
/// quad per crossed cell row. The sky is the last emitter and the one
/// `halos.truncate` sheds.
pub const STARDUST_HALO_BUDGET: usize = 16;

/// Source-over `RainHalo`s stardust may spend on one light frame. §3.3 draws
/// every light star as ink (the `push_twinkle_over` silhouette spends up to
/// five per star, split per crossed row); §18 holds a light frame to ≤ 400 of
/// `MAX_HALOS 512`, with the meteor's train chain at 96 ahead of the sky. The
/// remaining 36 are deliberate slack.
pub const STARDUST_LIGHT_HALO_BUDGET: usize = 268;

const _: () = assert!(
    STARDUST_LIGHT_HALO_BUDGET + 96 <= 400,
    "§18: the light frame's 400-halo law — train chain + sky"
);

/// The source-over ceiling of a light-theme star mark (§3.3: `light_ink_bold`
/// is `InkRole::Leading`, cap 236). Each rect is capped at its role's own
/// ceiling first (`InkRole::alpha_cap`: needles are over-text ink at 190);
/// this is the larger of the two.
pub const LIGHT_ALPHA_CAP: f32 = 236.0;

// ===========================================================================
// 2. Class, lane, and the star itself
// ===========================================================================

/// A star's magnitude: the distribution that makes a field a field (§5.1
/// no. 2, §5.2). With [`StarLane`] it states shape and brightness explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StarClass {
    /// The hero: white body + tapers, four tinted stubs, a tinted halo, and
    /// (gold only, at twinkle peak only) four diagonal glints. Composited
    /// centre 143 (sky) / 118 (transient).
    M1,
    /// The medium star: white body + tapers, a 3×3 disc (sky lane only), four
    /// tinted stubs, a small halo. Composited centre 130 (sky) / 80
    /// (transient).
    M2,
    /// The grain: a white 1-px core with four tinted arms at
    /// [`M3_CROSS_ADD`]·c outboard of it (its Airy disc, carrying the tint).
    /// Composited centre 98 (sky) / 61 (transient). Also §2.4's round mote at
    /// arm 0.
    M3,
}

impl StarClass {
    /// This class's coverage request `c` in `lane` (§5.2's two tables).
    #[must_use]
    pub fn cov(self, lane: StarLane) -> f32 {
        match (self, lane.is_sky()) {
            (Self::M1, true) => M1_COV_SKY,
            (Self::M1, false) => M1_COV_TRANSIENT,
            (Self::M2, true) => M2_COV_SKY,
            (Self::M2, false) => M2_COV_TRANSIENT,
            (Self::M3, true) => M3_COV_SKY,
            (Self::M3, false) => M3_COV_TRANSIENT,
        }
    }

    /// This class's rung on the family's one arm ladder
    /// (`effect_util::STAR_ARM_*`), so a star's size is always a named size.
    ///
    /// The grain takes [`STAR_ARM_FINE`] rather than arm 0: at `ch` 18 that is
    /// 1.89 px, which the arm clock steps between 1 and 2 (§5.5), so a grain
    /// visibly opens.
    #[must_use]
    pub fn arm_ratio(self) -> f32 {
        match self {
            Self::M1 => STAR_ARM_HERO,
            Self::M2 => STAR_ARM_STD,
            Self::M3 => STAR_ARM_FINE,
        }
    }

    /// The arm clock's floor, as a share of the nominal arm (§5.5): m2/m3
    /// swing at full depth, m1 only ±12 %. Small stars twinkle most, as a
    /// smaller apparent disc does, and a hero never looks resized.
    #[must_use]
    pub fn arm_floor(self) -> f32 {
        match self {
            Self::M1 => 0.88,
            Self::M2 | Self::M3 => 0.55,
        }
    }

    /// The smallest integer arm this class is ever drawn at, at cell height
    /// `ch` (§5.5 at `ch` 18: m1 3 ↔ 4, m2 2 ↔ 3, m3 1 ↔ 2 px). Only the m2
    /// needs a stated floor ([`M2_ARM_MIN_PX`]), scaled by the cell ([`px_at`]).
    #[must_use]
    pub fn arm_min_px(self, ch: f32) -> i32 {
        match self {
            Self::M2 => px_at(ch, M2_ARM_MIN_PX),
            Self::M1 | Self::M3 => 1,
        }
    }

    /// The core-brightness floor on the luminance clock (§5.5's table): m1
    /// `0.85 + 0.15·e`, m2 `0.70 + 0.30·e`, m3 `env` at full depth.
    #[must_use]
    pub fn core_floor(self) -> f32 {
        match self {
            Self::M1 => 0.85,
            Self::M2 => 0.70,
            Self::M3 => 0.0,
        }
    }

    /// This class's `(hold, τ)` in seconds for `lane` (§5.6). The field lane's
    /// alpha does not read it (a field star rides its cell over
    /// [`FIELD_LIFE_S`] and the field fade, [`Star::envelope`]), but its hold
    /// `H` still applies ([`Star::in_hold`]): every class is born at full and
    /// scintillates once its hold is spent.
    #[must_use]
    pub fn envelope_s(self, lane: StarLane) -> (f32, f32) {
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

    /// This class's whole life in seconds for `lane`: `H + 2.32·τ`, deriving
    /// the published 225 / 340 / 460 ms. A field star's is [`FIELD_LIFE_S`]
    /// whatever its class.
    #[must_use]
    pub fn life_s(self, lane: StarLane) -> f32 {
        if lane == StarLane::Field {
            return FIELD_LIFE_S;
        }
        let (h, t) = self.envelope_s(lane);
        half_life_life_s(h, t)
    }
}

/// Which population minted a star (§5.6), and so how it is priced, how long
/// it lives, how it moves, and whether it may touch the glint bucket. Keyed
/// on population rather than a sky/transient bit because §5.6's envelope
/// table is: a fan m1 lives 315 ms, a strike m1 460.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StarLane {
    /// Born on every typed glyph, in the sky band of the new cell. Sky
    /// pricing. Spends the bucket (m1 only).
    Strike,
    /// Born once per laid ribbon cell, hash-dealt and stable per cell. Sky
    /// pricing. Pinned; rides the cell's shape over [`FIELD_LIFE_S`]
    /// (`edge-in` × expiry melt) and finishes with the cell's retract
    /// ([`Stardust::on_event`]'s erase arms).
    Field,
    /// Born on Backspace (and per 3 cells of a kill, cap 8) around the erased
    /// cells. Transient pricing. Never chimes: Backspace is unpitched (§13,
    /// §19.2).
    Erase,
    /// The exhaust (§33): one per typed key, born in the caret cell's sky band
    /// and thrown back along the row on its own wind ([`EXHAUST_SHARE_CW`],
    /// [`EXHAUST_THROW_MS`]). Transient pricing; never chimes (the key already
    /// has its tine, [`StarLane::may_chime`]); not meteor-lane; never touches
    /// the glint bucket.
    Exhaust,
    /// The meteor's shed fragments (§6.5 layer 5). Transient pricing,
    /// meteor-lane, silent.
    Shed,
    /// The landing fan (§6.5 layer 11). Transient pricing, meteor-lane; its
    /// 1 gold m1 + 4 m2 pair 1:1 with the five rain glints (D6).
    Fan,
    /// A sub-floor nav hop's mini-fan (§6.12, D17). Transient pricing, 1 m2 +
    /// 2 m3, thrown 1-2 px, gone by 245 ms.
    MiniFan,
}

impl StarLane {
    /// True for the sky lanes — priced by the sparkle law, composited centre
    /// ≤ [`super::timing::FIELD_STAR_COV_CEIL`] (§5.2 ceiling 1).
    #[must_use]
    pub fn is_sky(self) -> bool {
        matches!(self, Self::Strike | Self::Field)
    }

    /// True for the transient lanes — they fly over ink, so they are priced by
    /// [`super::timing::TRANSIENT_STAR_COV_CEIL`] (§5.2 ceiling 2).
    #[must_use]
    pub fn is_transient(self) -> bool {
        !self.is_sky()
    }

    /// True where a star may cue a glint at all (§13): erase-born stardust and
    /// the exhaust never chime, and meteor-lane stars ride the rain instead.
    #[must_use]
    pub fn may_chime(self) -> bool {
        matches!(self, Self::Strike | Self::Field)
    }

    /// The throw window `T` of a radial-throw lane, ms; `None` for lanes that
    /// lift or drag. A thrown star's [`Star::v0`] is `reach / T` in px/ms; it
    /// follows `reach·(1 − spend(age/T))` and sits pinned and twinkling at
    /// `reach` once `age ≥ T`. `T` is the lane's window ([`ERASE_THROW_MS`],
    /// [`FAN_THROW_MS`], [`MINI_FAN_THROW_MS`]), shorter than any class life
    /// on the lane. Published so a caller building a `v0` uses this module's
    /// units.
    #[must_use]
    pub fn throw_ms(self) -> Option<f32> {
        match self {
            Self::Erase => Some(ERASE_THROW_MS),
            Self::Exhaust => Some(EXHAUST_THROW_MS),
            Self::Fan => Some(FAN_THROW_MS),
            Self::MiniFan => Some(MINI_FAN_THROW_MS),
            Self::Strike | Self::Field | Self::Shed => None,
        }
    }

    /// Which motion law this population takes (§5.6's Motion column): three
    /// laws, each named by the population that needs it.
    #[must_use]
    fn motion(self) -> Motion {
        match self {
            // A sky m1/m2 lifts; a sky m3 and every field star are pinned,
            // which the zero `v0` expresses without a fourth law.
            Self::Strike | Self::Field => Motion::Lift,
            // §5.6: the shed inherits the head's velocity under drag τ 90.
            Self::Shed => Motion::Drag,
            // §5.6: the erase is a throw on its own window, then pinned; the
            // fan and mini-fan throw on theirs.
            Self::Erase => Motion::Throw(ERASE_THROW_MS),
            // §33: the exhaust is the erase's throw run backward along the
            // row on its own wind — the same integrator, a window of its own.
            Self::Exhaust => Motion::Throw(EXHAUST_THROW_MS),
            Self::Fan => Motion::Throw(FAN_THROW_MS),
            Self::MiniFan => Motion::Throw(MINI_FAN_THROW_MS),
        }
    }
}

/// The three motion laws of §5.6, resolved from the population.
#[derive(Clone, Copy, Debug)]
enum Motion {
    /// `d = v₀ · age` — the sky's −3 px/s rise, integer-stepped.
    Lift,
    /// `d = v₀ · τ · (1 − e^(−age/τ))` — exponential drag, the closed-form
    /// integral so a fragment's path never depends on the frame rate.
    Drag,
    /// `d = v₀ · T · (1 − spend(age/T))` — the radial ease-out, i.e. §5.6's
    /// `reach·(1 − (1 − u)²)` written on the theme's own curve, over the
    /// lane's own window `T` in ms.
    Throw(f32),
}

impl Motion {
    /// The displacement factor at `age_ms`: `pos = origin + round(v₀ · k)`.
    /// One function for the whole path, so the birth clearance and every later
    /// frame read the same curve ([`Star::pos`], [`Star::rest`]).
    #[inline]
    fn k_at(self, age_ms: f32) -> f32 {
        match self {
            Self::Lift => age_ms,
            Self::Drag => DRAG_TAU_MS * (1.0 - (-age_ms / DRAG_TAU_MS).exp()),
            Self::Throw(window_ms) => window_ms * (1.0 - spend(age_ms / window_ms)),
        }
    }

    /// The age at which this motion ends, given the star's life in ms: a throw
    /// at its window (a class that dies sooner stops short on the same ray), a
    /// drift or lift at death. [`Star::rest`] is the position here.
    #[inline]
    fn end_ms(self, life_ms: f32) -> f32 {
        match self {
            Self::Throw(window_ms) => window_ms,
            Self::Lift | Self::Drag => life_ms,
        }
    }
}

/// A star's finish: how a star leaves before its life is up (§5.6's cap,
/// §8.2's focus ember, field stars dying with their cells). Its envelope is
/// multiplied by `spend((now − at) / span)`, so it leaves on the theme's
/// retract curve, compressed, never popping (T5).
///
/// A finishing star is culled once the finish alone takes it under
/// [`STAR_CULL_ALPHA`] (`u > 1 − √0.20 ≈ 0.55`), so it finishes within `span`
/// and the pool never carries a finishing ghost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Finish {
    /// The edge the finish started on.
    pub at: Instant,
    /// The finish's span in seconds — 0.040 for an eviction, 0.300 for the
    /// focus ember, the cell's own retract for a field star.
    pub span_s: f32,
}

impl Finish {
    /// The finish's unit progress at `now`, `0..=1`.
    #[must_use]
    pub fn u(&self, now: Instant) -> f32 {
        clamp01(
            now.saturating_duration_since(self.at).as_secs_f32() / self.span_s.max(f32::EPSILON),
        )
    }

    /// The envelope multiplier at `now` — `spend(u)`.
    #[must_use]
    pub fn gain(&self, now: Instant) -> f32 {
        spend(self.u(now))
    }

    /// The instant this finish ends.
    #[must_use]
    fn ends(&self) -> Instant {
        self.at + std::time::Duration::from_secs_f32(self.span_s.max(0.0))
    }

    /// Put `slot` on a finish ending at `at + span_s`, unless it already has
    /// one ending sooner ([`Star::finish_by`], [`Veil::finish_by`]).
    fn keep_sooner(slot: &mut Option<Self>, at: Instant, span_s: f32) {
        let next = Self { at, span_s };
        if slot.is_none_or(|f| next.ends() < f.ends()) {
            *slot = Some(next);
        }
    }

    /// The instant this finish alone takes the star under
    /// [`STAR_CULL_ALPHA`], where [`Star::dead`] drops it (D3): `spend(u) <
    /// 0.20` at `u = 1 − √0.20`, well before the finish ends.
    #[must_use]
    fn cull_at(&self) -> Instant {
        self.at
            + std::time::Duration::from_secs_f32(
                self.span_s.max(0.0) * (1.0 - STAR_CULL_ALPHA.sqrt()),
            )
    }
}

/// One star (§17.1's `Star{class, lane, x, y, born, seed, tint, v0, f}`, plus
/// the dealt gold flag and the optional finish).
///
/// `Copy` and flat: the pool is a fixed [`STAR_POOL_MAX`]-slot `Vec` (§18),
/// and envelope, arm, coverage, phase and halo radius are pure functions of
/// these fields and `now` (§5.5).
#[derive(Clone, Copy, Debug)]
pub struct Star {
    /// Magnitude (§5.2).
    pub class: StarClass,
    /// Population (§5.6) — pricing, envelope, motion, budget behaviour.
    pub lane: StarLane,
    /// Window-absolute X of the centre, pinned to a device pixel at birth
    /// (§5.4): drift is expressed as integer steps of the centre.
    pub x: f32,
    /// Window-absolute Y of the centre, pinned the same way.
    pub y: f32,
    /// The glyph row that earned a sky star. Its sky can occupy the preceding
    /// pixel row, so regional scrolls follow this owner rather than `y`.
    /// Free transients have no glyph owner and follow their pixel position.
    pub home_row: Option<u16>,
    /// The birth edge. Every envelope and both scintillation clocks read
    /// `now − born`.
    pub born: Instant,
    /// The last key edge that lit this star: the field lane's fade clock
    /// ([`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`] run from here). Equal to `born`
    /// at birth; a typed key on the star's row re-stamps it
    /// ([`Stardust::relight_field`]). Other lanes never read it.
    pub lit: Instant,
    /// Deterministic seed, hashed from `(row, col, mint)` at the spawn edge
    /// (§18). Deals the tint (§5.3), the birth jitter (§5.4), the
    /// scintillation phase `φ` and rate [`Star::f`], the throw angle, and the
    /// shed station.
    pub seed: u32,
    /// The star's chroma, constant for its life: one of the seven snapped
    /// stops, [`TINT_WHITE_RGB`] or [`TINT_GOLD_RGB`] (§5.3, C1). Only alpha
    /// moves (C3).
    pub tint: u32,
    /// True for a gold star (§5.3's 15 % deal, or the fan's hero, D6): the
    /// only stars with diagonal glints (§5.2). Dealt, never inferred from
    /// [`Star::tint`], since the arc's yellow stop is the same RGB as gold.
    pub gold: bool,
    /// Birth velocity in px/ms, `(vx, vy)`: the throw for erase / fan /
    /// mini-fan, the inherited `0.12·v_head` for a shed fragment, the −3 px/s
    /// lift for a sky m1/m2. Zero for a pinned grain and every field star.
    /// T4: the observed frame delta where one exists, never the spine.
    pub v0: (f32, f32),
    /// The seeded arm-size scintillation rate, Hz, in
    /// `[SCINT_F_MIN, SCINT_F_MAX]` (§5.5, D12). The star's audio glint
    /// twinkles at the same `f` (§13).
    pub f: f32,
    /// The slipstream's travel, px ≥ 0: how far this star drifts backward over
    /// [`STREAM_END_MS`], priced once at birth from the observed key interval
    /// ([`stream_travel_px`] of [`stream_px_per_s`], T4) and clamped to its
    /// sky row's blank frontier ([`Stardust::stream_reach`], L4).
    /// Integer-stepped: `x − round(stream · stream_unit(age))` ([`Star::pos`]).
    /// Zero under a stroll or a cold hand and for every transient. An adopted
    /// field star keeps its drift.
    pub stream: f32,
    /// The mend's pull, px ≥ 0: how far left of its home this star was born,
    /// drawn in over [`MEND_PULL_MS`] as `x − round(pull · spend(age /
    /// MEND_PULL_MS))` ([`Star::pos`]). `x`, `y` are the home, where every cell
    /// law reads it ([`star_over_cell`]). Zero except for a mend's star
    /// ([`Stardust::deal_star`] with a [`Pull`]); an adopted field star keeps
    /// it.
    pub pull: f32,
    /// The finish this star is on, if any ([`Finish`]).
    pub finish: Option<Finish>,
}

// A floored birth is never under the cull: the echo frame's field star is
// drawn, not dropped by `Paint::of`'s `alpha < STAR_CULL_ALPHA` on the frame
// that acknowledges its key (T2).
const _: () = assert!(
    BIRTH_EDGE_FLOOR > STAR_CULL_ALPHA,
    "the field star's birth floor sits under the star cull"
);

impl Star {
    /// Seconds since birth, saturating at zero (a star handed a `now` before
    /// its own edge is a newborn, never a negative age).
    #[must_use]
    pub fn age_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.born).as_secs_f32()
    }

    /// Seconds since the last key that lit this star ([`Star::lit`]),
    /// saturating at zero — the field fade's clock.
    #[must_use]
    pub fn idle_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.lit).as_secs_f32()
    }

    /// This star's whole life in seconds (§5.6).
    #[must_use]
    pub fn life_s(&self) -> f32 {
        self.class.life_s(self.lane)
    }

    /// Born at full: true inside the class hold `H` (§5.6). For its first `H`
    /// ms a star is drawn at its nominal arm with its core at full (`e = 1`,
    /// `env = 1`); both scintillation clocks (§5.5) take over at `age ≥ H`.
    /// The clocks still read `now − born`, so the trace stays a pure function
    /// of `(birth, seed, now)` and the warm/cool antiphase law holds.
    ///
    /// Without it a star born at a luminance trough would spend its brightest
    /// span dim and at a 1-px arm. The field lane reads its class's sky hold.
    #[must_use]
    pub fn in_hold(&self, now: Instant) -> bool {
        self.age_s(now) < self.class.envelope_s(self.lane).0
    }

    /// True once the star must leave the pool: past its life, past the point
    /// where its [`Finish`] alone takes it under [`STAR_CULL_ALPHA`], or (a
    /// field star) past the field fade from its row's last key
    /// ([`field_fade_life_s`]). The envelope reaches the cull exactly here
    /// (D3), so a star leaves as a visible point, never a grey speck.
    #[must_use]
    pub fn dead(&self, now: Instant) -> bool {
        self.age_s(now) >= self.life_s()
            || (self.lane == StarLane::Field && self.idle_s(now) >= field_fade_life_s())
            || self.finish.is_some_and(|f| f.gain(now) < STAR_CULL_ALPHA)
    }

    /// The population envelope at `now`: `1` for the hold, then half-life τ
    /// (§5.6), times the [`Finish`] if any. Only coverage fades; chroma is
    /// fixed (C3).
    ///
    /// A field star rides its cell instead (§5.6), as the product of three
    /// alpha clocks:
    ///
    /// * the birth: dealt on the echo edge that laid the cell, it takes the
    ///   cell's 18 ms `edge-in` from [`BIRTH_EDGE_FLOOR`], not from zero, so
    ///   it shows on the frame that echoes its key (T2). The floor applies to
    ///   the birth only;
    /// * the horizon: the ribbon's `expiry_melt` over [`FIELD_LIFE_S`],
    ///   bounding the sky under a hand that keeps typing;
    /// * the hand: the field fade on [`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`] from
    ///   the last key on the star's row ([`Star::lit`],
    ///   [`Stardust::relight_field`]): full while the hand is on the row,
    ///   under half 550 ms after it leaves, culled at 880 ms, before the
    ///   ribbon's retract.
    ///
    /// The retract itself is the [`Finish`] the producer starts on the erase
    /// edge.
    ///
    /// Under reduced motion the theme's linear fade replaces the half-life
    /// ([`REDUCED_MOTION_FADE_MS`], §2.5/§6.11): full through the life, then
    /// to zero over 120 ms (for a field star, at the end of
    /// [`field_fade_life_s`]). A finish still applies.
    #[must_use]
    pub fn envelope(&self, now: Instant, reduced_motion: bool) -> f32 {
        let age = self.age_s(now);
        let fin = self.finish.map_or(1.0, |f| f.gain(now));
        if self.lane == StarLane::Field {
            // The field lane's one envelope — shared with the aurora's veil,
            // which rides the same cell the same way ([`Veil::envelope`]).
            return field_lane_envelope(age, self.idle_s(now), fin, reduced_motion);
        }
        if reduced_motion {
            let life = self.life_s();
            let fade = REDUCED_MOTION_FADE_MS / 1000.0;
            return clamp01((life - age) / fade.max(f32::EPSILON)) * fin;
        }
        let (hold, tau) = self.class.envelope_s(self.lane);
        half_life(age, hold, tau) * fin
    }

    /// Put this star on a finish ending at `at + span_s`, unless it already
    /// has one ending sooner.
    pub fn finish_by(&mut self, at: Instant, span_s: f32) {
        Finish::keep_sooner(&mut self.finish, at, span_s);
    }

    /// This star's grid column: its home pixel read back as a column (stable
    /// for its life, §5.4). The pet's catch reach
    /// (`companion::CATCH_REACH_CELLS`) is measured against it.
    #[must_use]
    pub fn grid_col(&self, geom: Geom) -> i32 {
        px_col(self.x, geom)
    }

    /// Whether this star is in the sky of `row`: "row or row − 1" depending on
    /// the ribbon spelling ([`in_sky_of_row`]). The vertical half of the
    /// catch's reach.
    #[must_use]
    pub fn in_sky_of(&self, row: u16, geom: Geom) -> bool {
        in_sky_of_row(self.y, row, geom)
    }

    /// True for a warm tint: R, O, Y of the seven stops, plus gold and the
    /// achromatic white (§5.5). White has no cool neighbour to beat against,
    /// so it takes the base phase.
    #[must_use]
    pub fn is_warm(&self) -> bool {
        if self.tint == TINT_WHITE_RGB || self.tint == TINT_GOLD_RGB {
            return true;
        }
        // By stop index, not channels: R, O, Y are stops 0-2 and G, B, I, V
        // are 3-6; a tint is one of those constants (C1).
        (0..SPECTRUM_STOPS)
            .position(|i| spectrum_stop(i) == self.tint)
            .is_none_or(|i| i < WARM_STOPS)
    }

    /// The luminance phase, radians (§5.5): `φ = hash(row, col, seed)·2π`;
    /// warm tints take `φ`, cool ones `φ + π`, so warm and cool neighbours
    /// never blink together (`warm_and_cool_neighbours_never_blink_together`).
    #[must_use]
    pub fn lum_phase(&self) -> f32 {
        let base = hash01(mix32(self.seed ^ SALT_PHASE)) * std::f32::consts::TAU;
        if self.is_warm() {
            base
        } else {
            base + std::f32::consts::PI
        }
    }

    /// The certified 2.86 Hz luminance envelope at `now` (§5.5;
    /// `effect_util::twinkle_env`, `TWINKLE_OMEGA 9` rad/s, under WCAG 2.3.1's
    /// general-flash bound). Every mark at every size rides this clock; the
    /// 8-12 Hz exemption is for size only.
    #[must_use]
    pub fn twinkle(&self, now: Instant) -> f32 {
        twinkle_env(self.age_s(now), self.lum_phase())
    }

    /// True where this star's body and nucleus may carry `push_twinkle_star`'s
    /// diagonal glints: gold only, at twinkle peak only (§5.2, §5.7).
    #[must_use]
    pub fn glinting(&self, now: Instant) -> bool {
        self.gold && twinkle_peak(self.twinkle(now))
    }

    // -- the cadence law (§18) ---------------------------------------------

    /// Whether this star moves or ramps every frame: a throw inside its window,
    /// a shed fragment under drag (for its whole life), or a field star inside
    /// its 18 ms `edge-in`. A sky star's single-pixel lift is a step, not
    /// motion, and a star in its hold is static.
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        let age_s = self.age_s(now);
        if self.lane == StarLane::Field && age_s < EDGE_IN_S {
            return true;
        }
        // The mend's pull moves every frame of its window, like a throw (1e).
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
    /// `arm = round(A·(floor + (1 − floor)·sin²(2π·f·age + φ)))` at cell
    /// height `ch`, solved in closed form: the arm toggles where `sin²` crosses
    /// `((k + 0.5)/A − floor)/(1 − floor)`, twice per `1/(2f)` period, so it is
    /// never more than 62.5 ms away. `None` when the arm never steps.
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
        let mut best: Option<f32> = None;
        for k in k0..=k1 {
            let c = ((k as f32 + 0.5) / nominal - floor) / depth;
            if c > 0.0
                && c < 1.0
                && let Some(dt) = next_sine_crossing_s(theta, c.sqrt().asin(), omega)
            {
                best = Some(best.map_or(dt, |b| b.min(dt)));
            }
        }
        best
    }

    /// Seconds until a gold star's diagonal glints next switch
    /// (`|sin(ω·age + φ)| ≥ 0.92`, §5.2, §5.5), exact on the luminance clock.
    /// `None` for any other star.
    #[must_use]
    pub fn next_glint_toggle_s(&self, now: Instant) -> Option<f32> {
        if !self.gold {
            return None;
        }
        let theta = TWINKLE_OMEGA * self.age_s(now) + self.lum_phase();
        next_sine_crossing_s(theta, TWINKLE_GLINT_FRAC.asin(), TWINKLE_OMEGA)
    }

    /// Seconds until a lifting star's integer position next steps (§5.6):
    /// `pos = origin + round(v₀·age)` steps where `|v₀|·age` next reaches a
    /// half. `None` for a pinned star or one not on the lift.
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
            let dt = (next_ms - age_ms) / 1000.0;
            if dt > 0.0 {
                best = Some(best.map_or(dt, |b| b.min(dt)));
            }
        }
        best
    }

    /// The slipstream's integer displacement at `age_ms`, px toward the pane's
    /// first column: `round(stream · stream_unit(age))`. Zero without drift.
    #[inline]
    fn stream_px(&self, age_ms: f32) -> i32 {
        if self.stream <= 0.0 {
            return 0;
        }
        (self.stream * stream_unit(age_ms)).round() as i32
    }

    /// Seconds until a streaming star's integer position next steps: where
    /// `stream · u(age)` next reaches a half, solved as
    /// `age = −τ · ln(1 − u · STREAM_SPENT)` ([`stream_unit`]). `None` without
    /// drift, past [`STREAM_END_MS`], or when the remaining travel rounds to
    /// nothing. Offered to the cadence fold as a tail, under the floor.
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
        Some(((t_ms.min(STREAM_END_MS) - age_ms) / 1000.0).max(0.0))
    }

    /// The mend's pull at `age_ms`, px left of home:
    /// `round(pull · spend(age / MEND_PULL_MS))`, the whole pull at birth and
    /// nothing from the window's end. Zero without a pull.
    #[inline]
    fn pull_px(&self, age_ms: f32) -> i32 {
        if self.pull <= 0.0 || age_ms >= MEND_PULL_MS {
            return 0;
        }
        (self.pull * spend(age_ms / MEND_PULL_MS)).round() as i32
    }

    /// The centre at `now`, pinned to a device pixel (§5.4), by the
    /// population's [`Motion`] law. Reduced motion freezes every mark at its
    /// birth pixel (§6.11); for a mend's star, its home.
    #[must_use]
    pub fn pos(&self, now: Instant, reduced_motion: bool) -> (i32, i32) {
        if reduced_motion {
            return self.origin();
        }
        let age_ms = self.age_s(now) * 1000.0;
        let (x, y) = self.at_k(self.lane.motion().k_at(age_ms));
        // The slipstream (1d) and the mend's pull (1e) ride on top of the
        // lane's own law, on X only.
        (x - self.stream_px(age_ms) - self.pull_px(age_ms), y)
    }

    /// Where this star comes to rest: its position at the end of its motion (a
    /// throw's reach, a drift's or lift's death). §5.4's transient clearance
    /// is asked of this pixel ([`Stardust::settle`]): a core never lands on a
    /// probed glyph. Under reduced motion every mark stays at its (already
    /// clear) origin.
    #[must_use]
    pub fn rest(&self) -> (i32, i32) {
        let motion = self.lane.motion();
        let end_ms = motion.end_ms(self.life_s() * 1000.0);
        let (x, y) = self.at_k(motion.k_at(end_ms));
        (x - self.stream_px(end_ms), y)
    }

    /// The birth pixel — `x`, `y` are integers by construction (§5.4).
    #[inline]
    fn origin(&self) -> (i32, i32) {
        (self.x.round() as i32, self.y.round() as i32)
    }

    /// `origin + round(v₀ · k)`, the one integer step every position is.
    #[inline]
    fn at_k(&self, k: f32) -> (i32, i32) {
        let (bx, by) = self.origin();
        if self.v0.0 == 0.0 && self.v0.1 == 0.0 {
            return (bx, by);
        }
        (
            bx + (self.v0.0 * k).round() as i32,
            by + (self.v0.1 * k).round() as i32,
        )
    }

    /// Re-aim the motion to end exactly on `rest` (the clearance's nudge and
    /// walk-back, [`Stardust::settle`]). `k` is the motion's end factor, so
    /// `at_k(k)` returns `rest` to the pixel.
    fn aim(&mut self, rest: (i32, i32), k: f32) {
        let (bx, by) = self.origin();
        if k <= 0.0 {
            self.v0 = (0.0, 0.0);
            return;
        }
        self.v0 = ((rest.0 - bx) as f32 / k, (rest.1 - by) as f32 / k);
    }
}

// ===========================================================================
// 3. The glint bucket (§13)
// ===========================================================================

/// The shared token bucket (§8.1 no. 2, §13, D5), owned by [`Stardust`] and
/// refilled on the frame clock in `Engine::tick`:
/// `tokens = min(GLINT_CAP, tokens + GLINT_REFILL_PER_S · dt)`.
///
/// Only an m1 spends a token, and a spent token cues exactly one glint at that
/// star's column on that frame; m2 and m3 never touch the bucket. An earned
/// hero (capital, `!`, kitty Delight, the landing fan's slot) with no token is
/// born anyway, silent; a dealt hero with no token degrades to m2.
/// Meteor-lane stars are exempt, and a meteor empties the bucket on its move
/// edge, so nothing chimes on top of the rain.
///
/// This gives ≤ 4 glints/s, ~0.8-1.5/s in 10 cps prose. The "≤ 3 live" of §13
/// is the synth's lane cap (§14), not the bucket's
/// (`the_bucket_keeps_chiming_as_it_refills`).
#[derive(Clone, Copy, Debug)]
pub struct StarBudget {
    /// Available glints, bounded by `GLINT_CAP`.
    tokens: f32,
    /// The last refill stamp; `None` ⇒ never refilled (a cold bucket starts
    /// full, so the first capital of a session chimes).
    at: Option<Instant>,
}

impl Default for StarBudget {
    /// The same full bucket [`StarBudget::new`] gives — a derived default
    /// would start a session mute.
    fn default() -> Self {
        Self::new()
    }
}

impl StarBudget {
    /// A full bucket (see [`StarBudget::at`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            tokens: GLINT_CAP,
            at: None,
        }
    }

    /// Refill on the frame clock — called once per `Engine::tick`, before any
    /// birth is dealt.
    pub fn refill(&mut self, now: Instant) {
        let dt = self
            .at
            .replace(now)
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        self.tokens = (self.tokens + GLINT_REFILL_PER_S * dt).min(GLINT_CAP);
    }

    /// Try to spend one token for an m1's glint. `true` ⇒ the caller cues
    /// exactly one glint at that star's column this frame. Refuses only when
    /// the bucket is dry (§13).
    pub fn try_spend(&mut self) -> bool {
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }

    /// A meteor empties the bucket on its move edge (§8.1 no. 2, §13), so the
    /// ~250 ms of typing under the rain is silent.
    pub fn empty(&mut self) {
        self.tokens = 0.0;
    }

    /// Tokens on hand — `trail status` and the budget tests.
    #[must_use]
    pub fn tokens(&self) -> f32 {
        self.tokens
    }
}

/// One glint the sky has earned and paid for (§13): one sine at a lattice
/// degree, octave-folded into [2800, 5600) Hz, amplitude-twinkled at
/// [`Glint::f`], the star's own scintillation rate.
///
/// A token is spent at the birth edge inside [`Stardust::on_event`], which has
/// no frame; [`Stardust::emit`] drains this queue into `frame.cues` as
/// `SoundKind::Stardust` on the same tick, so one token = one hero = one
/// glint, same column, same frame (`hero_births_and_glints_share_one_budget`).
/// The glint's `t = 0` is the draining tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glint {
    /// The hero's column, which the host maps to stereo pan (§13: "pan = the
    /// star's column ± 0.03").
    pub col: u16,
    /// The star's seeded scintillation rate, Hz — the glint's amplitude
    /// twinkle rides it at depth 0.5, so the sound opens and closes with the
    /// light it belongs to.
    pub f: f32,
}

// ===========================================================================
// 4. The glyph probe (§5.4's hard gate, L4, D16)
// ===========================================================================

/// What this frame knows about the glyphs: the snapshot §5.4's birth
/// requirement is asked of, so a star is never drawn over a stroke (L4).
///
/// Mirrors `CursorGlow::probed_cell_glyph`, including:
///
/// * an off-grid row (above row 0, below the last) is `Some(false)`: the
///   effects box clips those bands into padding (§5.4's `Geom.head` band);
/// * a row the probe never covered is `None`, unknown, and no star is born
///   there (L4, D16);
/// * a light horizontal rule ([`CellInk::Rule`], the `─` a TUI draws its
///   input box with) is ink only inside its stroke band
///   ([`RULE_INK_TOP_CH`]`..`[`RULE_INK_BOT_CH`]): [`Self::at`] says
///   `Some(true)` for the cell, [`Self::at_px`] says `Some(false)` above and
///   below the stroke, where the sky band lies, so the sky above a bordered
///   prompt is not dark.
///
/// The host probes the caret's row and its two neighbours, so this holds at
/// most three rows and evicts round-robin. Resident and cleared, never
/// rebuilt (§18). Written through `Engine::probe_mut` before `tick`, or
/// [`Stardust::probe_mut`] in tests.
#[derive(Clone, Debug, Default)]
pub struct GlyphProbe {
    /// The row storage: the first [`Self::live`] entries are the held rows,
    /// most recent last; entries past them are retired storage kept for their
    /// bitset capacity. At most [`Self::ROWS`] entries exist.
    rows: Vec<ProbedRow>,
    /// How many of [`Self::rows`] are held. [`Self::clear`] resets only this,
    /// so the clear on every scroll and band move (one per streamed line under
    /// a streaming TUI) and the re-probe after it allocate nothing (§18).
    live: usize,
    /// Round-robin write cursor into [`Self::rows`].
    at: usize,
}

/// What one probed cell holds for §5.4's gate: the host's per-column char
/// reduced to the one distinction the sky cares about.
///
/// The gate is cell-granular (a bitset, §18): a glyph's ink reaches the
/// baseline and below, so the sky band of the row beneath is never provably
/// clear. A light horizontal rule is the exception, a stroke through the
/// cell's vertical centre only; [`cell_ink`] names it so the probe can answer
/// per pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CellInk {
    /// A blank (`' '`): provably clear, `Some(false)` everywhere.
    #[default]
    Blank,
    /// Any other glyph, a wide continuation (`'\0'`) included: `Some(true)`
    /// at every pixel of the cell (L4's law, unchanged).
    Glyph,
    /// A light horizontal box-drawing rule (`─` U+2500 and its dashed and
    /// half-length siblings): `Some(true)` only inside
    /// [`RULE_INK_TOP_CH`]`..`[`RULE_INK_BOT_CH`], `Some(false)` above and
    /// below. Heavy (`━`) and double (`═`) rules reach into the sky band and
    /// stay [`Self::Glyph`].
    Rule,
}

/// Classify one probed char for the glyph probe (the host's
/// `Terminal::row_cols_into` convention: the lead char at its column, `'\0'`
/// at a wide continuation, `' '` for a blank).
///
/// Rules are exactly the light horizontal members of the Box Drawing block:
/// solid, triple-, quadruple- and double-dash, left and right half. Anything
/// with a vertical arm, a heavy or double stroke, or other ink is a glyph.
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

/// Where a light horizontal rule's stroke begins, as a fraction of the cell
/// height from its top (§5.4). The renderer draws the stroke
/// `round(min(cw, ch) / 8)` px thick (at least 1), centred
/// (`aterm_render::procedural`), so its top edge is at or below `0.40 · ch`
/// on any cell 8 px or taller. `0.35` leaves margin for a font-drawn `─`.
pub const RULE_INK_TOP_CH: f32 = 0.35;

/// ...and where it ends: the sky zone's top edge under `tall`, derived. The
/// ribbon's top is [`super::ribbon::TALL_UP_CH`]` − 1 = 0.10 ch` above the
/// typed cell and the sky band reaches [`SKY_TOP_CH`] above that, so the
/// zone's highest pixel is 0.60 of the way down row − 1. The stroke's bottom
/// edge is at most `0.57 · ch` on any cell 8 px or taller, so the band
/// contains the stroke and is disjoint from the sky. [`rule_stroke_px`] is
/// the band's one rounding to device pixels.
pub const RULE_INK_BOT_CH: f32 = 1.0 - (super::ribbon::TALL_UP_CH - 1.0) - SKY_TOP_CH;

// The rule clause: a sky star born in §5.4's zone never has its core on a
// rule's stroke, in either spelling, at birth or over its life (the strike's
// lift is held under the stroke like the exhaust's rise,
// [`Stardust::rule_headroom_px`]). Under `tall` the zone is `[0.60, 0.86]`
// of row − 1 and the stroke band ends at 0.60; under `underline` the zone is
// `[0.04, 0.30]` of the typed cell and the band starts at 0.35.
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
// A rise out of the `underline` zone never reaches the row above's stroke:
// the zone's top is `SKY_BOTTOM_CH` under the row boundary, the rise is at
// most `EXHAUST_RISE_CH`, and the row above's band ends `1 − RULE_INK_BOT_CH`
// above the boundary. So the only stroke a rise can meet is in the zone's own
// row, the one [`Stardust::rule_headroom_px`] reads.
const _: () = assert!(
    SKY_BOTTOM_CH + EXHAUST_RISE_CH < 1.0 - RULE_INK_BOT_CH,
    "a rise out of the underline zone must stop short of the row above's stroke band"
);

/// The stroke band in whole pixels: the device-pixel offsets from a cell's
/// top that a light rule's band ([`RULE_INK_TOP_CH`]`..`[`RULE_INK_BOT_CH`])
/// covers at cell height `ch`, half-open. The one rounding used by the pixel
/// probe ([`GlyphProbe::at_px`]) and the rise's headroom
/// ([`Stardust::rule_headroom_px`]).
///
/// The bottom edge is `round(RULE_INK_BOT_CH · ch)`, the same rounding
/// [`Stardust::sky_birth_px`] uses for the tall zone's top pixel, so zone and
/// band are disjoint pixel for pixel (`ceil` would claim the zone's top pixel
/// on many cell heights). The top edge is `ceil(RULE_INK_TOP_CH · ch)`, which
/// keeps it off the `underline` zone's lowest pixel. Pinned across `ch`
/// 6..=48 by
/// `the_stroke_band_is_one_set_of_pixels_for_the_probe_the_zones_and_the_headroom`.
#[must_use]
pub fn rule_stroke_px(ch: usize) -> std::ops::Range<i32> {
    let ch = ch as f32;
    (RULE_INK_TOP_CH * ch).ceil() as i32..(RULE_INK_BOT_CH * ch).round() as i32
}

/// One probed row of the grid: which columns carry ink.
#[derive(Clone, Debug, Default)]
struct ProbedRow {
    /// The grid row, signed so an off-grid probe is representable.
    row: i32,
    /// One bit per column, LSB-first — resident, cleared, never rebuilt.
    /// Set for [`CellInk::Glyph`] and [`CellInk::Rule`]: the cell has ink.
    ink: Vec<u64>,
    /// One bit per column, set where that ink is a [`CellInk::Rule`] — the
    /// pixel-level answer then depends on the row band asked.
    rule: Vec<u64>,
    /// Columns actually captured; a column past this holds nothing to
    /// overprint (the same convention v1's own-cell gate always used).
    width: u16,
}

impl GlyphProbe {
    /// How many rows a probe holds — the caret's and its two neighbours,
    /// which is exactly what `CursorGlow` captures.
    pub const ROWS: usize = 3;

    /// Nothing probed: every in-grid cell answers `None`, so no sky star is
    /// born until the host has looked (§5.4).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that grid `row` was probed, with `occupied[col]` true where a
    /// glyph stands. Replaces any earlier record of the same row. The bool
    /// view knows only [`CellInk::Blank`] and [`CellInk::Glyph`]; a host
    /// with the chars in hand writes [`Self::probe_row_ink`] instead.
    pub fn probe_row(&mut self, row: i32, occupied: &[bool]) {
        let slot = self.slot_for(row, occupied.len());
        let r = &mut self.rows[slot];
        for (col, &on) in occupied.iter().enumerate() {
            if on {
                r.ink[col / 64] |= 1u64 << (col % 64);
            }
        }
    }

    /// [`Self::probe_row`] with the per-cell class ([`cell_ink`]): a
    /// [`CellInk::Rule`] is recorded as ink (for [`Self::at`]) and as a rule
    /// (for [`Self::at_px`]'s stroke band).
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

    /// The slot `row` is written into — its own if it is already held,
    /// a fresh one while there is room, else the round-robin victim — with
    /// both bitsets cleared and sized for `width` columns.
    fn slot_for(&mut self, row: i32, width: usize) -> usize {
        let words = width.div_ceil(64);
        let slot = if let Some(i) = self.rows[..self.live].iter().position(|r| r.row == row) {
            i
        } else if self.live < Self::ROWS {
            if self.rows.len() == self.live {
                self.rows.push(ProbedRow::default());
            }
            self.live += 1;
            self.live - 1
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

    /// Forget everything: a reset, a layout change, a style switch, and every
    /// scroll or band move (the host re-probes before the next deal). The
    /// rows' bitsets are kept as retired storage ([`Self::live`]).
    pub fn clear(&mut self) {
        self.live = 0;
        self.at = 0;
    }

    /// The gate (§5.4): `Some(true)` a glyph is there, `Some(false)`
    /// provably blank, `None` outside this probe's knowledge.
    #[must_use]
    pub fn at(&self, row: i32, col: u16, rows: usize) -> Option<bool> {
        if row < 0 || row as usize >= rows {
            // Off-grid bands hold no glyphs by construction.
            return Some(false);
        }
        let r = self.rows[..self.live].iter().find(|r| r.row == row)?;
        if col >= r.width {
            return Some(false);
        }
        let i = usize::from(col);
        Some(r.ink.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1))
    }

    /// [`Self::at`] asked of a window pixel: the cell under `(x, y)`. A pixel
    /// left or right of the grid is off-grid, hence `Some(false)`.
    ///
    /// Over a [`CellInk::Rule`] cell the answer is the stroke's: `Some(true)`
    /// inside the band ([`rule_stroke_px`]), `Some(false)` above or below.
    /// This lets §5.4's sky zone be born over a TUI's input rule. The pixel's
    /// offset in its row is computed in integers; an f32 fraction misplaces
    /// the zone's top pixel on some cell heights.
    #[must_use]
    pub fn at_px(&self, x: i32, y: i32, geom: Geom) -> Option<bool> {
        let col = px_col(x as f32, geom);
        if col < 0 || col as usize >= geom.cols {
            return Some(false);
        }
        let row = px_row(y as f32, geom);
        let inked = self.at(row, col as u16, geom.rows)?;
        if inked && self.rule_at(row, col as u16) {
            // The pixel's offset from its cell's top edge, in whole pixels.
            let off = y - i32::from(geom.origin_y) - row * geom.ch as i32;
            return Some(rule_stroke_px(geom.ch).contains(&off));
        }
        Some(inked)
    }

    /// [`Self::at`] for §5.4's sky question: does this `row − 1` cell carry
    /// ink the zone `[top − 0.30 ch, top − 0.04 ch]` could touch? A light rule
    /// answers `Some(false)` here (its band ends where the zone begins) while
    /// [`Self::at`] still says `Some(true)`. Otherwise identical to
    /// [`Self::at`].
    #[must_use]
    pub fn sky_at(&self, row: i32, col: u16, rows: usize) -> Option<bool> {
        let inked = self.at(row, col, rows)?;
        Some(inked && !self.rule_at(row, col))
    }

    /// Is the (in-grid, held) cell's ink a [`CellInk::Rule`]? Asked only
    /// after [`Self::at`] answered `Some(true)`, so a miss is simply "no".
    fn rule_at(&self, row: i32, col: u16) -> bool {
        let Some(r) = self.rows[..self.live].iter().find(|r| r.row == row) else {
            return false;
        };
        let i = usize::from(col);
        r.rule.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
    }

    /// True when the probe has no rows at all.
    #[must_use]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }
}

// ===========================================================================
// 5. Sow specs: the transient-lane entry points the meteor needs (§6.5)
// ===========================================================================

/// What the meteor hands stardust for one shed station (§6.5 layer 5, §5.6):
/// the m3 grain(s) born as the head passes station `s_i`, transient-priced,
/// meteor-lane and silent.
///
/// The meteor owns the stations and their timing, calling
/// [`Stardust::sow_shed`] once per station with `n = 1`; stardust owns what a
/// fragment is (inherited velocity, sputter, drag, pricing).
///
/// Units: `at_px` and `v_head` are window px and px/ms; `tint_t` is a
/// position on the meteor's arc (`t_m(s_i)`, §6.4); `seed` is per station.
#[derive(Clone, Copy, Debug)]
pub struct ShedSow {
    /// Window-absolute px of the station the head just passed — the fragment
    /// is born exactly here.
    pub at_px: (f32, f32),
    /// The head's own velocity in px/ms, unscaled. The fragment inherits
    /// [`SHED_ALONG_SHARE`] of it along the path (applied here) and sputters
    /// [`SHED_PERP_MIN`]..[`SHED_PERP_MAX`] px/ms perpendicular, under
    /// [`DRAG_TAU_MS`].
    pub v_head: (f32, f32),
    /// The meteor's arc position at this station: the fragment wears
    /// `spectrum_snap(t_m(s_i))`, the train's colour where it was shed (§6.5
    /// layer 5, C1).
    pub tint_t: f32,
    /// This station's seed, hashed from the flight's seed and the station
    /// index, so a flight sheds the same debris every time (§18).
    pub seed: u32,
    /// How many grains at this station; the meteor's law is one.
    pub n: u16,
}

/// What the meteor hands stardust for its landing fan (§6.5 layer 11, §5.6).
///
/// Composition is fixed by D6: 1 gold m1 + 4 m2 + rest m3, in throw order,
/// so the heroes pair 1:1 with the five rain glints. An Enter lands small
/// with no hero ([`Self::hero`] false, D8: one m2 + 5-7 m3). Each star snaps
/// to the landing walk's colour at the column its throw rests on
/// ([`Self::walk`]), so fan, pin and band read as one landing; the hero is
/// gold.
///
/// Units: `at_px` is window px; `reach` is px, the outermost star's throw
/// along the line once [`FAN_THROW_MS`] has elapsed (each star at
/// `reach·(0.55..1.05)`, seeded). The vertical follows [`fan_rise`] (squash
/// capped at [`FAN_RISE_MAX_CH`] and the glass); a ray rising past it is
/// flattened, never dropped. This producer builds every `v0`.
#[derive(Clone, Copy, Debug)]
pub struct FanSow {
    /// Window-absolute px of the landing cell's centre.
    pub at_px: (f32, f32),
    /// Throw reach in px — the length of the outermost star's throw on
    /// [`Motion::Throw`]'s ease-out over [`FAN_THROW_MS`], along the line;
    /// its rise is the sky's ([`fan_rise`]).
    pub reach: f32,
    /// The landing's walk ([`LandingWalk`]): `walk.at(0.0)` is the pin's stop,
    /// and a star thrown `dx` px along the row snaps to `walk.at(dx / cw)`
    /// (C1). Latched by the meteor at the spawn edge (D4).
    pub walk: LandingWalk,
    /// The landing's seed — the whole of the party's variance (§6.5).
    pub seed: u32,
    /// How many stars, `min(5 + cells/1.8, 14) + party·4` capped at
    /// `timing::FAN_MAX_N`. Thinned by [`LIGHT_COUNT_SCALE`] on a light
    /// theme, never below the hero slots.
    pub n: u8,
    /// Whether this landing carries its gold m1 hero. False for an Enter (D8).
    pub hero: bool,
}

/// One sky birth, as [`Stardust::deal_star`] takes it (§5.6's Strike and
/// Field populations). A record so `earned`, which changes the outcome of a
/// dry bucket (D5), is named at every call site.
#[derive(Clone, Copy, Debug)]
struct Deal {
    /// The magnitude asked for; the budget and the spacing law may lower it.
    class: StarClass,
    /// Strike or Field (§5.6).
    lane: StarLane,
    /// The glyph cell — the sky band is resolved against its ribbon band top.
    cell: (u16, u16),
    /// Earned (§5.8: a shifted capital, `!`, kitty Delight, the fan's slot)
    /// rather than dealt. An earned hero is always born as an m1; only a dealt
    /// one is spaced, row-capped, and degraded to m2 without a token (D5).
    earned: bool,
    /// The mend's pull (1e): `Some` for the star of a key that mends a typo
    /// run, born over the erased cell and pulled into `cell`.
    pull: Option<Pull>,
    /// A tint fixed by rule rather than dealt, `(rgb, gold)` like
    /// [`deal_tint`]'s. `None` except for the combo ladder's two top rungs
    /// ([`LADDER_WHITE_KEYS`], [`LADDER_GOLD_KEYS`]).
    tint: Option<(u32, bool)>,
}

/// Where a mend's star is pulled from, as [`Stardust::deal_star`] takes it
/// ([`MEND_PULL_MS`]).
#[derive(Clone, Copy, Debug)]
struct Pull {
    /// The erased cell ([`super::Mend::row`], [`super::Mend::col`]).
    from: (u16, u16),
    /// The momentum the deleting interrupted ([`super::Mend::disp`]): the
    /// birth spine the star's slipstream is gated on, beside the live one —
    /// a star born at the momentum the fix resumes.
    disp: f32,
}

/// One free transient, as [`Stardust::throw`] takes it (§5.6's Erase, Shed,
/// Fan and MiniFan populations). Its tint is already resolved (the fan's hero
/// is gold by rule, D6).
#[derive(Clone, Copy, Debug)]
struct Throw {
    /// The magnitude — transients are never demoted; they never touch the
    /// bucket (§13).
    class: StarClass,
    /// Which transient population, for pricing, envelope and motion.
    lane: StarLane,
    /// Window-absolute px of the throw's origin.
    at_px: (f32, f32),
    /// Birth velocity, px/ms.
    v0: (f32, f32),
    /// Chroma, constant for life (C3).
    tint: u32,
    /// Gold — the fan's hero slot (D6).
    gold: bool,
    /// The deterministic seed (§18).
    seed: u32,
    /// Law-fixed: a slot the theme promises (the fan's gold m1 and four m2,
    /// D6; the mini-fan's m2, D17). Never dropped by the clearance: a rest it
    /// cannot clear within 2 px is walked back along its throw to the first
    /// clear pixel ([`Stardust::settle`]). A grain is dropped rather than
    /// drawn on ink (§5.4).
    fixed: bool,
}

// ===========================================================================
// 6. The producer
// ===========================================================================

/// The stardust producer: the star pool, the shared bucket, and the hero
/// spacing memory.
#[derive(Clone, Debug)]
pub struct Stardust {
    /// The live pool, [`STAR_CAP`] live plus [`STAR_FINISH_SLACK`] finishing
    /// (§18). Private: read through [`Stardust::live_iter`], written only by
    /// [`Stardust::sow`], reached from [`Stardust::deal_star`] (the sky's probe
    /// gate) and [`Stardust::throw`] (the transients' clearance); scrolls go
    /// through [`Stardust::translate_scroll`].
    stars: Vec<Star>,
    /// The one glint bucket (§13).
    pub budget: StarBudget,
    /// What this frame knows about the glyphs (§5.4's hard gate). Written by
    /// the host through `Engine::probe_mut` → [`Stardust::probe_mut`] before
    /// the deal pass.
    probe: GlyphProbe,
    /// Glints minted and paid for this frame, drained by the engine (§13).
    /// Resident; `drain` keeps capacity.
    glints: Vec<Glint>,
    /// How many stars this producer has minted. Folded into every seed so two
    /// stars born on one cell at one `now` differ, without a clock or RNG
    /// (§18).
    minted: u32,
    /// The reduced-motion posture of the last frame drawn, cached for the
    /// cadence law (seam point 9 is asked with a clock and nothing else).
    reduced: bool,
    /// The cell height of the last frame drawn, px — what the size clock's
    /// nominal arm is read at ([`Star::next_arm_step_s`]).
    cell_h: f32,
    /// The aurora: one [`Veil`] per laid cell whose sky the probe proved
    /// blank, at most [`AURORA_CAP`], resident (§18). Written by
    /// [`Stardust::deal_veil`]; re-lit, finished, embered, scrolled and culled
    /// beside the field stars, whose envelope it rides.
    veil: Vec<Veil>,
    /// The instant of the last Typed key, the slipstream's cadence memory
    /// ([`Stardust::note_cadence`], T4). `None` until the first key.
    last_typed: Option<Instant>,
    /// The observed key interval, ms, clamped to
    /// `[STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS]`: every slipstream birth is
    /// priced from it ([`stream_px_per_s`]). The slow end until two keys have
    /// been seen, so a first key streams nothing.
    ioi_ms: f32,
    /// The frame's laid light, indexed by grid cell ([`SkyIndex`]): the ground
    /// a star is priced over. Resident and cleared per frame: `emit` opens it
    /// on this frame's geometry before the aurora goes out (§18).
    sky_index: SkyIndex,
}

impl Default for Stardust {
    /// The same sky [`Stardust::new`] builds: full bucket, reserved pool.
    fn default() -> Self {
        Self::new()
    }
}

impl Stardust {
    /// An empty sky with a full bucket and the pool reserved once (§18).
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
            sky_index: SkyIndex::default(),
        }
    }

    /// The frame's glyph truth, written by the host before the deal pass
    /// (§5.4). Until then every in-grid cell is unknown and no sky star is
    /// born.
    pub fn probe_mut(&mut self) -> &mut GlyphProbe {
        &mut self.probe
    }

    /// Read-only view of the frame's glyph truth.
    #[must_use]
    pub fn probe(&self) -> &GlyphProbe {
        &self.probe
    }

    /// Every star in the pool, finishing ones included — what is on glass.
    pub fn live_iter(&self) -> impl Iterator<Item = &Star> {
        self.stars.iter()
    }

    /// Drain the glints minted since the last [`Stardust::emit`] (§13), one
    /// per token spent, at the hero's column. A ticking host finds this empty
    /// (`emit` writes them into `Frame::cues`); it serves non-ticking callers
    /// (style switch, shutdown). Only [`Stardust::reset`] drops them.
    #[cfg(test)]
    pub fn take_glints(&mut self) -> std::vec::Drain<'_, Glint> {
        self.glints.drain(..)
    }

    /// Deal this event's births against the ribbon's planned bands.
    ///
    /// Called after [`Ribbon::plan`] and before any emit, since §5.4's zones
    /// are relative to [`super::ribbon::Band::top`] and the cell may have been
    /// laid this frame (T2).
    ///
    /// §5.6's populations: Strike on every typed glyph, Field once per laid
    /// ribbon cell, Erase on every Backspace (3 m3 + 1 m2 at the erased cell)
    /// and per 3 cells of a kill (cap 8, spread along the span). The meteor's
    /// populations (Shed, Fan, MiniFan) arrive through
    /// [`Stardust::sow_shed`] / [`Stardust::sow_fan`] /
    /// [`Stardust::sow_mini_fan`], since the meteor owns their geometry and
    /// classification.
    ///
    /// A typed key first re-lights the caret row's field
    /// ([`Stardust::relight_field`]), then deals. The erase arms put the
    /// retracted cells' field stars on the ribbon's retract span (§8.2: 240
    /// ms, or `12·n + 240` for a kill); a focus loss puts every star on the
    /// 300 ms ember (§8.2).
    pub fn on_event(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) {
        match *ev {
            Event::Typed { cells, class, .. } => {
                self.note_cadence(at, cells);
                self.relight_field(ctx.caret, at, ctx.geom);
                self.deal_typed(cells, class, at, ctx, ribbon);
            }
            // A retracted cell's field star finishes over the cell's retract
            // (§5.6; `timing::KILL_RETRACT_BASE_MS`).
            Event::Erase => {
                self.finish_field(ctx.caret, KILL_RETRACT_BASE_MS, at, ctx.geom);
                self.deal_backspace(at, ctx);
            }
            // Both kill scales throw the same population (§5.6, §19.1);
            // `KillScope` matters only to the synth.
            Event::Kill { cells, .. } => {
                self.finish_field(ctx.caret, kill_span_ms(cells), at, ctx.geom);
                self.deal_kill(cells, at, ctx);
            }
            // A move mints no star here (the meteor's fan and shed; T1). A
            // rewrite finishes the retracted cells' field stars on the
            // ribbon's retract span and throws nothing.
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

    /// An earned hero (§5.8): a shifted capital, `!`, kitty Delight, or the
    /// landing fan's slot. Always born as an m1; it cues a glint only if a
    /// token is available (D5).
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

    /// The one entry through which a formed star enters the pool: applies the
    /// 40-cap (§5.6) and nothing else. Private, since every lawful birth goes
    /// through [`Stardust::deal_star`] (probe gate) or [`Stardust::throw`]
    /// (clearance).
    fn sow(&mut self, star: Star) {
        self.make_room(star.born);
        self.stars.push(star);
    }

    /// The cat catches a star: shorten the life of the one live star whose
    /// home pixel and birth edge are `(x, y, born)`, on the frame the paw
    /// lands. Returns whether a star was there.
    ///
    /// The triple is a star's identity, so the search stops at the first
    /// match; a dead star or a repeated catch finds nothing or re-asserts a
    /// finish [`Star::finish_by`] will not lengthen. Idempotent, never touching
    /// a bystander.
    ///
    /// Only a gold sky m1 is catchable (the reach law is the router's,
    /// [`super::companion::PetOffer`]; the class law is asserted here).
    /// Nothing is born here (T1).
    pub fn catch(&mut self, x: f32, y: f32, born: Instant, at: Instant) -> bool {
        let Some(star) = self.stars.iter_mut().find(|s| {
            s.gold
                && s.class == StarClass::M1
                && s.lane.is_sky()
                && s.born == born
                && s.x == x
                && s.y == y
        }) else {
            return false;
        };
        star.finish_by(at, CATCH_FINISH_MS / 1000.0);
        true
    }

    /// Shed fragments (§6.5 layer 5): `spec.n` m3 grains born at the station
    /// `spec.at_px`, each inheriting [`SHED_ALONG_SHARE`] of the head's
    /// velocity plus a seeded perpendicular sputter, on [`Motion::Drag`] at
    /// [`DRAG_TAU_MS`]. Transient-priced, meteor-lane, silent. On a light
    /// theme each grain is dealt at [`LIGHT_COUNT_SCALE`] (§5.2).
    ///
    /// The station arithmetic is the meteor's alone; this side adds no offset
    /// along the path. The clearance may move the rest ≤ 2 px
    /// ([`Stardust::settle`]).
    pub fn sow_shed(&mut self, at: Instant, spec: ShedSow, ctx: &Ctx<'_>) {
        let (vx, vy) = spec.v_head;
        let speed = vx.hypot(vy).max(f32::EPSILON);
        let (ux, uy) = (vx / speed, vy / speed);
        for k in 0..spec.n {
            let seed = mix32(spec.seed ^ u32::from(k).wrapping_mul(0x9E37_79B9));
            if !light_keeps(seed, ctx.cfg) {
                continue;
            }
            let side = if seed & 1 == 0 { 1.0 } else { -1.0 };
            let perp = side
                * (SHED_PERP_MIN + (SHED_PERP_MAX - SHED_PERP_MIN) * hash01(mix32(seed ^ 1)))
                * px_scale(ctx.geom.ch as f32);
            let v0 = (
                ux * speed * SHED_ALONG_SHARE - uy * perp,
                uy * speed * SHED_ALONG_SHARE + ux * perp,
            );
            self.throw(
                Throw {
                    class: StarClass::M3,
                    lane: StarLane::Shed,
                    at_px: spec.at_px,
                    v0,
                    // §6.5 layer 5: the train's own colour where it was shed.
                    tint: spectrum_snap(spec.tint_t),
                    gold: false,
                    seed,
                    // A grain, and silent: nudged or dropped like any grain.
                    fixed: false,
                },
                at,
                ctx,
            );
        }
    }

    /// The landing fan (§6.5 layer 11, D6): 1 gold m1 + 4 m2 + rest m3 in
    /// throw order, thrown radially on [`Motion::Throw`] over [`FAN_THROW_MS`]
    /// to `reach·(0.55..1.05)`, each coloured by the landing walk at its rest
    /// column ([`FanSow::walk`]). The hero is one of §5.8's earned heroes, but
    /// meteor-lane, so it never touches the typing bucket (§13). On a light
    /// theme the m3 slots are dealt at [`LIGHT_COUNT_SCALE`].
    ///
    /// The five law-fixed stars are unconditional: born on the landing cell
    /// (the caret's own, ledger-exempt, L2), with the clearance applied to
    /// where each rests ([`Stardust::settle`]); a rest that cannot clear within
    /// 2 px is walked back along the throw, since the rain glints count
    /// against these five. Grains are dropped where they cannot clear (§5.4).
    pub fn sow_fan(&mut self, at: Instant, spec: FanSow, ctx: &Ctx<'_>) {
        let n = usize::from(spec.n).max(1);
        let cw = (ctx.geom.cw as f32).max(1.0);
        // The vertical reach law: the landing's rise caps, once per fan.
        let (rise_up, rise_down) = fan_rise(spec.reach, spec.at_px, ctx.geom);
        for k in 0..n {
            let seed = mix32(spec.seed ^ (k as u32).wrapping_mul(0x85EB_CA6B));
            let class = fan_class(k, spec.hero);
            if class == StarClass::M3 && !light_keeps(seed, ctx.cfg) {
                continue;
            }
            // Even radial spread with a seeded stagger, so four landings of
            // the same size differ in position as well as in count (§20.1's
            // `every_landing_is_its_own_party`).
            let a = (k as f32 / n as f32 + hash01(seed) / n as f32) * std::f32::consts::TAU;
            let reach = spec.reach
                * (FAN_THROW_JITTER_MIN
                    + (FAN_THROW_JITTER_MAX - FAN_THROW_JITTER_MIN) * hash01(mix32(seed ^ 3)));
            // The ray, flattened toward the line where it would rise past
            // its cap — the same length, spent along the line; never dropped.
            let (dx, dy) = fan_ray(a, reach, rise_up, rise_down, hash01(mix32(seed ^ 5)));
            let v0 = (dx / FAN_THROW_MS, dy / FAN_THROW_MS);
            let gold = class == StarClass::M1 && spec.hero;
            let tint = if gold {
                // D6: the fan's own hero is gold, always — it is the mark the
                // five rain glints are counted against.
                TINT_GOLD_RGB
            } else {
                // A point mark snaps (C1) to the band's stop at the column the
                // throw rests on ([`FanSow::walk`]).
                spectrum_snap(spec.walk.at(dx / cw))
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
                    // D6 / D8: the hero and the m2 slots are the landing's
                    // promise; only a grain may be dropped on ink.
                    fixed: class != StarClass::M3,
                },
                at,
                ctx,
            );
        }
    }

    /// The mini-fan (§6.12, D17): a same-row nav hop of 2-7 cells. Exactly
    /// 1 m2 + 2 m3, thrown 1-2 px over [`MINI_FAN_THROW_MS`], gone by 245 ms.
    /// On a light theme the grains are dealt at [`LIGHT_COUNT_SCALE`]; the m2
    /// is law-fixed (D17).
    ///
    /// `at_px` is the landing cell's centre in window px; `seed` is the hop's.
    pub fn sow_mini_fan(&mut self, at: Instant, at_px: (f32, f32), seed: u32, ctx: &Ctx<'_>) {
        let n = MINI_FAN_M2_N + MINI_FAN_M3_N;
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
            let a =
                (f32::from(k) / f32::from(n) + hash01(s) / f32::from(n)) * std::f32::consts::TAU;
            let reach = (MINI_FAN_REACH_MIN_PX
                + (MINI_FAN_REACH_MAX_PX - MINI_FAN_REACH_MIN_PX) * hash01(mix32(s ^ 5)))
                * px_scale(ctx.geom.ch as f32);
            let v0 = (
                a.cos() * reach / MINI_FAN_THROW_MS,
                a.sin() * reach / MINI_FAN_THROW_MS,
            );
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
                    // D17: the m2 always winks; its 1-2 px throw rests on the
                    // landing cell, the caret's own.
                    fixed: true,
                },
                at,
                ctx,
            );
        }
    }

    /// Draw the sky.
    ///
    /// Called last in the emit order (§6.5), so §18's `halos.truncate` sheds
    /// stardust first.
    ///
    /// §5.7's three recipes on `effect_util::push_twinkle_star` (D2: body and
    /// nucleus are `#FFFFFF` in every class) and a local premultiplied halo;
    /// §5.5's two scintillation clocks; §3.3's light-theme twins; the cull at
    /// [`STAR_CULL_ALPHA`] with chroma constant (C3).
    ///
    /// Budgets are spent per star: the quad budget is shared fairly among the
    /// stars still to draw (floored at a grain's five quads), so an over-budget
    /// frame thins every star's extremities rather than dropping stars, and
    /// never exceeds [`STARDUST_QUAD_BUDGET`]. The halo budget counts haloed
    /// stars, not per-row splits.
    ///
    /// Brightest first (heroes, m2s, grains): a star is priced over the sky
    /// laid before it ([`sky_ground`]), and only a halo's tail reaches a
    /// neighbouring cell's star, so drawing haloed classes first puts every
    /// tail under every core it can reach
    /// (`one_sky_star_per_cell_and_no_pile_outshines_the_caret`).
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.reduced = ctx.cfg.reduced_motion;
        self.cell_h = ctx.geom.ch as f32;
        // §13: the token was spent at the birth edge; the cue goes out on the
        // same tick at the hero's column. `dir` is 0 (a star has no travel)
        // and `shifted` false (only the key-time seam may set it).
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
        if self.stars.is_empty() && self.veil.is_empty() {
            return;
        }
        let quad_cap = frame.out.len() + STARDUST_QUAD_BUDGET;
        let halo_cap = frame.halos.len() + STARDUST_LIGHT_HALO_BUDGET;
        // The aurora first: it is the sky's ground (light-theme ink composites
        // over it; dark-theme stars are priced over it, [`sky_ground`]), so
        // the slice it lays is kept and indexed by cell ([`SkyIndex`]).
        let mut index = std::mem::take(&mut self.sky_index);
        index.open(ctx.geom, (frame.out.len(), frame.halos.len()));
        self.draw_veil(ctx, frame.halos, halo_cap);
        let mut haloed = 0usize;
        let n = self.stars.len();
        // `i` counts every star handed a turn, drawn or culled, so the fair
        // share is what is left over the stars still to come.
        let mut i = 0usize;
        for class in [StarClass::M1, StarClass::M2, StarClass::M3] {
            for star in self.stars.iter().filter(|s| s.class == class) {
                let turn = i;
                i += 1;
                let Some(paint) = Paint::of(star, &self.probe, ctx) else {
                    continue;
                };
                let left = quad_cap.saturating_sub(frame.out.len());
                if left == 0 {
                    // The index is resident: hand its capacity back before
                    // the budget ends the frame early.
                    self.sky_index = index;
                    return;
                }
                let share = (left / (n - turn)).max(M3_QUADS).min(left);
                let star_cap = frame.out.len() + share;
                if ctx.cfg.dark_theme {
                    paint.draw_add(ctx, frame, star_cap, &mut haloed, &mut index);
                } else {
                    paint.draw_over(ctx.geom, frame, halo_cap);
                }
            }
        }
        self.sky_index = index;
    }

    /// Stars on glass, `trail status`'s `v2_stars=` row (seam point 12).
    /// Finishing stars count; at most [`STAR_CAP`] are live and the pool never
    /// exceeds [`STAR_POOL_MAX`].
    #[must_use]
    pub fn live(&self) -> usize {
        self.stars.len()
    }

    /// True when the sky is empty — one of the three pools
    /// `Engine::next_change_deadline` folds (§18).
    #[must_use]
    #[cfg(test)]
    pub fn at_rest(&self) -> bool {
        self.stars.is_empty() && self.veil.is_empty()
    }

    /// The cadence law, brisk half: whether any star moves every frame
    /// ([`Star::brisk`]). Never under reduced motion (§6.11).
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        !self.reduced
            && (self.stars.iter().any(|s| s.brisk(now))
                || self.veil.iter().any(|v| v.age_s(now) < EDGE_IN_S))
    }

    /// The cadence law: the next instant a star changes what is on glass.
    /// `None` when the sky is empty (T6).
    ///
    /// A throw, a drag or a mend's pull ([`MEND_PULL_MS`]) is brisk (the next
    /// frame); everything else is a tail, floored at [`super::TAIL_FLOOR`] so
    /// many stars cannot interleave into a finer cadence:
    ///
    /// * the end of its hold, when the scintillation clocks take over;
    /// * past its hold, the next integer arm step
    ///   ([`Star::next_arm_step_s`], ≤ 62.5 ms away); luminance changes in
    ///   between are sampled at 16-22 Hz, or at frame rate when anything is
    ///   brisk;
    /// * a gold star's glints toggling ([`Star::next_glint_toggle_s`]);
    /// * a sky star's one-pixel lift ([`Star::next_lift_step_s`]);
    /// * its death (class life or a [`Finish`]'s cull), up to a slot late;
    /// * under reduced motion: static until the linear fade opens
    ///   (`life − 120 ms`), then the floor.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        let mut cad = Cadence::at(now);
        if self.brisk(now) {
            cad.brisk();
        }
        // A veil's clocks are the field star's: hold end, fade end, finish
        // cull, horizon, and under reduced motion the linear fade's opening.
        // In between, stars over it already sample the sky.
        for v in &self.veil {
            let idle = v.idle_s(now);
            let age = v.age_s(now);
            let fade_life = field_fade_life_s();
            if let Some(f) = v.finish {
                cad.tail_at(f.cull_at());
                cad.tail(0.0);
            }
            if self.reduced {
                let fade_from = fade_life - REDUCED_MOTION_FADE_MS / 1000.0;
                cad.tail((fade_from - idle).max(0.0));
                cad.tail((FIELD_LIFE_S - REDUCED_MOTION_FADE_MS / 1000.0 - age).max(0.0));
            } else {
                if idle < FIELD_HOLD_MS / 1000.0 {
                    cad.tail(FIELD_HOLD_MS / 1000.0 - idle);
                } else {
                    cad.tail(0.0);
                }
                if age < EDGE_IN_S {
                    cad.tail(EDGE_IN_S - age);
                } else if age >= FIELD_LIFE_S * (1.0 - EXPIRY_MELT_SHARE) {
                    // The horizon's melt is moving under a hand that is
                    // still on the row.
                    cad.tail(0.0);
                }
            }
            cad.tail(fade_life - idle);
            cad.tail(FIELD_LIFE_S - age);
        }
        for s in &self.stars {
            let age = s.age_s(now);
            let life = s.life_s();
            cad.tail(life - age);
            if let Some(f) = s.finish {
                cad.tail_at(f.cull_at());
                cad.tail(0.0);
            }
            if s.lane == StarLane::Field {
                // The field fade from the row's last key ([`Star::lit`]): its
                // hold's end, its cull, and under reduced motion the linear
                // fade's opening. In between the arm clock samples the star.
                let idle = s.idle_s(now);
                let fade_life = field_fade_life_s();
                if self.reduced {
                    let fade_from = fade_life - REDUCED_MOTION_FADE_MS / 1000.0;
                    cad.tail((fade_from - idle).max(0.0));
                } else if idle < FIELD_HOLD_MS / 1000.0 {
                    cad.tail(FIELD_HOLD_MS / 1000.0 - idle);
                }
                cad.tail(fade_life - idle);
            }
            if self.reduced {
                let fade_from = life - REDUCED_MOTION_FADE_MS / 1000.0;
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
            if let Some(dt) = s.next_glint_toggle_s(now) {
                cad.tail(dt);
            }
            if let Some(dt) = s.next_lift_step_s(now) {
                cad.tail(dt);
            }
            // The slipstream's integer steps, offered as tails (≤ 22 Hz plus
            // one step, never frame rate; 1d).
            if let Some(dt) = s.next_stream_step_s(now) {
                cad.tail(dt);
            }
        }
        cad.take()
    }

    /// Drop the sky: style switch, layout change, `Engine::reset`. The bucket
    /// is refilled: a reset is a new session.
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

    /// Seam point 12 (`translate_scroll_state`), the sky's half: move every
    /// star with the viewport by `rows` cell heights and drop those that leave
    /// the grid. The probe is not translated; the host re-probes before the
    /// next deal.
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        if rows == 0 {
            return;
        }
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
        // The veil moves with its row and leaves with it.
        for v in &mut self.veil {
            v.row = v.row.checked_sub(rows).unwrap_or(u16::MAX);
        }
        self.veil.retain(|v| v.row != u16::MAX);
        self.probe.clear();
    }

    /// Move sky stars with the glyph rows that earned them. Under tall ribbon
    /// geometry a row's sky is in the previous pixel row, including the band
    /// above row zero; pixel membership alone would move a fixed footer's stars
    /// and leave the moving top row's stars behind. Free transients use
    /// [`BandPx`]'s point law. Veils follow grid rows.
    /// The probe is cleared and re-read by the host before the next deal.
    /// Without cell height, stars fail closed while row ownership still moves.
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

    // -- the deal ----------------------------------------------------------

    /// §5.6's Strike and Field populations, dealt over the cells this echo
    /// laid.
    ///
    /// The strike deal is one draw per cell, bucketed (`u < 1/12` m1, then
    /// `1/4` m2, then `1/2` m3, else none): every published rate holds and a
    /// typed cell carries at most one strike star. An earned hero (§5.8) takes
    /// the slot outright. The second grain at `disp ≥ 0.5` is born on the next
    /// cell (the caret's leading edge), priced on the honest spine.
    ///
    /// One sky star per cell ([`STRIKE_REACH_CELLS`]): every sky deal asks
    /// [`Stardust::sky_star_live`] first, and a cell keeps its star:
    ///
    /// * the strike moves to the first free cell within its reach or is
    ///   dropped (under a hot hand the typed cell holds the previous key's
    ///   grain, so heroes and m2s land one cell ahead);
    /// * the second grain is dropped when the lead cell is taken;
    /// * the field deal adopts the cell's star into the field lane
    ///   (`lane ← Field`, `lit ← at`, pinned), so every laid cell carries one
    ///   long-lived star and the hot sky stays dense across the line;
    /// * the key that mends a typo run ([`Ctx::mend`]) takes its strike as the
    ///   pulled m2 of 1e ([`MEND_PULL_MS`]), freeing the erased cell for the
    ///   fix's field star.
    fn deal_typed(
        &mut self,
        cells: u16,
        class: TypedClass,
        at: Instant,
        ctx: &Ctx<'_>,
        ribbon: &Ribbon,
    ) {
        // The aurora rides the ribbon, which lays on a Space too: every laid
        // cell gets its veil before the star deal.
        self.deal_veil(cells, at, ctx, ribbon);
        // Space lays ribbon but deals no star (`TypedClass::Space`, §8.2).
        if class == TypedClass::Space {
            return;
        }
        let scale = if ctx.cfg.dark_theme {
            1.0
        } else {
            LIGHT_COUNT_SCALE
        };
        // A hot hand deals every cell a field star (`FIELD_DEAL_IN_HOT`, on
        // the honest spine); an open theme holds that share through the dips,
        // lerped over flow's heat (`FIELD_SHARE_FLOW`).
        let field_step = if ctx.disp >= FIELD_HOT_DISP {
            FIELD_SHARE_FLOW
        } else {
            FIELD_SHARE_COLD
        };
        let field_share = field_step
            .max(FIELD_SHARE_COLD + (FIELD_SHARE_FLOW - FIELD_SHARE_COLD) * clamp01(ctx.flow.heat));
        let m1 = scale / DEAL_M1_IN as f32;
        let m2 = m1 + scale / DEAL_M2_IN as f32;
        let m3 = m2 + scale / DEAL_M3_IN as f32;
        // §5.8: a shifted capital and `!` earn their hero; only the dealt
        // 1-in-12 degrades without a token (D5).
        let earned = matches!(class, TypedClass::Capital | TypedClass::Bang);
        let (row, head) = ctx.caret;
        for k in 0..cells.max(1) {
            let col = head.saturating_sub(cells.max(1) - k);
            let cell = (row, col);
            let h = cell_hash(row, col, 0);

            // -- the mend (1e; §23) -----------------------------------------
            // The key that mends a typo run takes its strike as a pulled star:
            // born over the erased cell (grabbing the star the Backspace was
            // retiring, if live) and drawn one cell into the caret's over
            // `MEND_PULL_MS`, then pinned. An m2 by rule; an earned hero keeps
            // its magnitude. If refused (erased cell's sky inked, caret cell
            // off glass or taken) the key is dealt normally.
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

            // -- strike -----------------------------------------------------
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
            // The combo ladder, flow's payout, on the key's own cell (the last
            // of a coalesced echo). It replaces this key's strike rather than
            // adding a star beside it.
            let rung = (k + 1 == cells.max(1) && !mended)
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
            // A second grain from the caret's leading edge when the honest
            // spine is up, only onto a free cell.
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

            // -- field ------------------------------------------------------
            // Stable per cell, so the sky does not shimmer as the ribbon
            // re-plans. A cell's existing sky star is adopted into the field
            // lane rather than stacked. The mend's cell gives its light to the
            // caret's (1e); a fresh field star there would sit on the pulled
            // star's path. The cell keeps its veil.
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
        // The exhaust (§33): one grain per typed event, not per cell, after
        // the per-cell loop (a coalesced echo exhausts once at the final
        // caret) and after the Space early-return (a Space exhausts nothing).
        self.sow_exhaust(at, ctx, ribbon);
    }

    /// The exhaust (§33): one grain per typed key, born in the caret cell's
    /// sky band ([`Stardust::sky_birth_px`]) and thrown back along the row on
    /// the hand's wind ([`stream_px_per_s_with_share`] at
    /// [`EXHAUST_SHARE_CW`], over [`EXHAUST_THROW_MS`]): the emission's
    /// debris, a mark the eye can follow out of the caret. An m3 in the caret
    /// cell's stop, or 1 in [`EXHAUST_M2_IN`] an m2 at a hot birth spine. The
    /// gates below refuse in order. ≤ 3 live grains at 12 cps; all work is at
    /// the key's edge.
    fn sow_exhaust(&mut self, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) {
        // A throw is motion (§6.11).
        if ctx.cfg.reduced_motion {
            return;
        }
        // The birth spine must reach [`STREAM_DISP`] (the slipstream's gate),
        // floored by a [`super::Mend`]'s `disp` so a fix exhausts like the key
        // it replaces, whether or not the pull itself was accepted. A cold
        // hand throws nothing.
        let birth_disp = ctx
            .mend
            .map_or(ctx.birth_disp, |m| ctx.birth_disp.max(m.disp));
        if birth_disp < STREAM_DISP {
            return;
        }
        // The wind is the observed interval at the exhaust's share (T4): a
        // stroll ([`STREAM_IOI_STROLL_MS`]) or a session's first key exhausts
        // nothing.
        let g = ctx.geom;
        let cw = g.cw as f32;
        let wind = stream_px_per_s_with_share(EXHAUST_SHARE_CW, cw, self.ioi_ms);
        if wind <= 0.0 {
            return;
        }
        // A spark never costs a hero: at the cap [`Stardust::make_room`]
        // retires the oldest live star, so the exhaust yields within
        // [`EXHAUST_HEADROOM`] of the cap instead.
        let live = self.stars.iter().filter(|s| s.finish.is_none()).count();
        if live + EXHAUST_HEADROOM > STAR_CAP {
            return;
        }
        let (row, col) = ctx.caret;
        // The seed does not mint (§18): bumping the counter would reshuffle
        // every later deal. This key's deals already advanced it, and
        // [`SALT_EXHAUST`] keeps the draw independent on the same cell.
        let seed = mix32(cell_hash(row, col, self.minted) ^ SALT_EXHAUST);
        // The light theme thins the population as it thins every thrown
        // grain (§5.2).
        if !light_keeps(seed, ctx.cfg) {
            return;
        }
        let class =
            if birth_disp >= FIELD_HOT_DISP && deal(seed, SALT_EXHAUST ^ 1, EXHAUST_M2_IN, 1.0) {
                StarClass::M2
            } else {
                StarClass::M3
            };
        // §5.4's sky band of the caret cell: `row − 1` proved clear, the pixel
        // on the glass and free. The caret's cell has no laid run, so the band
        // is the spelling's anchor ([`default_band_top`]).
        let Some((x, y)) = self.sky_birth_px((row, col), seed, ctx, ribbon) else {
            return;
        };
        // Under `underline` the band is the caret's own row (D16), so that
        // cell must be clear too (a mid-line insert or ghost text can put a
        // glyph under the caret).
        if !ctx.cfg.ribbon_tall && self.probe.sky_at(i32::from(row), col, g.rows) != Some(false) {
            return;
        }
        // Where a grain on this wind is when the throw ends: the priced travel
        // capped at the band's fast leg ([`WALK_FAST_CELLS`]), times the share
        // spent by [`EXHAUST_THROW_MS`].
        let want = stream_travel_px(wind).min(WALK_FAST_CELLS * cw) * stream_unit(EXHAUST_THROW_MS);
        // The rise is priced first because it decides which grid rows the
        // flight covers ([`Stardust::blank_frontier_px`], D16).
        let rise = EXHAUST_RISE_CH * g.ch as f32 * (0.5 + 0.5 * hash01(mix32(seed ^ 3)));
        // A grain needs a cell to fly over: the one behind the caret, clear
        // for the sky on every row the flight covers
        // ([`GlyphProbe::sky_at`]). Otherwise the throw would be a sub-cell
        // twitch that holds the frame train and shows nothing
        // (`the_exhaust_needs_a_blank_cell_behind_the_caret`).
        let rows = px_row(y - rise, g)..=px_row(y, g);
        if !col.checked_sub(1).is_some_and(|c| {
            rows.into_iter()
                .all(|r| self.probe.sky_at(r, c, g.rows) == Some(false))
        }) {
            return;
        }
        // L4 on the x axis: the reach is clamped at the blank frontier of every
        // row the flight covers and at the pane's first column.
        let reach = self.blank_frontier_px(x, (y - rise, y), want, ctx);
        if reach < 0.5 {
            return;
        }
        // L4 on the y axis under a rule (§33): a rule cell's sky zone is under
        // its stroke, so along a rule row the rise is limited to the headroom
        // under the band (possibly none), the same clamp as the strike's lift
        // ([`Stardust::deal_star`]). Over a blank row it is untouched.
        let rise = rise.min(self.rule_headroom_px(px_col(x - reach, g)..=i32::from(col), y, g));
        let v0 = (-reach / EXHAUST_THROW_MS, -rise / EXHAUST_THROW_MS);
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
                // L4 on the rest: [`Stardust::settle`] nudges a rest off a
                // probed glyph within 2 px or drops the grain (§5.4).
                fixed: false,
            },
            at,
            ctx,
        );
    }

    /// §5.6's Erase population for one Backspace: 3 m3 + 1 m2 thrown up and
    /// away from the erased cell, transient-priced. Never chimes (§13, §19.2),
    /// and no m1 is dealt. On a light theme the grains are dealt at
    /// [`LIGHT_COUNT_SCALE`]; the m2 is fixed.
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

    /// §5.6's Erase population for a kill: one star per 3 cells, cap 8 (§8.2),
    /// star `k` at the centre of the `k`-th group of three from the caret in
    /// the direction the ribbon drains; three grains to every m2.
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

    /// One erase-lane throw from `cell`: 2-4 px over [`ERASE_THROW_MS`]
    /// ease-out, up and away (the angle spans the upper half-plane), then
    /// pinned and twinkling. The seed folds in the mint counter (§18), so two
    /// Backspaces at one column throw different debris.
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
        let v0 = (
            a.cos() * reach / ERASE_THROW_MS,
            a.sin() * reach / ERASE_THROW_MS,
        );
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
                // §5.6 promises no pairing for the erase: nudged or dropped.
                fixed: false,
            },
            at,
            ctx,
        );
    }

    /// Deal one sky star at `spec.cell`, subject to §5.4's zone and probe gate,
    /// §5.2's hero spacing and D5's budget interaction.
    ///
    /// Only magnitude is rationed, never light: a dealt hero too close to a
    /// live one, the third live on its row, or without a token is born as an
    /// m2; an earned hero is an m1 regardless and is silent without a token
    /// (§5.8, D5). Only §5.4's gates refuse a star outright: the probe (L4),
    /// and the effects box (a star clipped off glass must not spend a token).
    ///
    /// A mend's star ([`Deal::pull`], 1e) is dealt at its home, the caret's
    /// cell, through every gate, and born over the erased cell: a live sky
    /// star there is grabbed (re-minted in place with its seed, its erase
    /// finish pardoned), else it is born one cell left of home. Its lift is
    /// zero and its slipstream is gated on the resumed momentum. Returns
    /// whether a star was dealt.
    fn deal_star(&mut self, spec: Deal, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) -> bool {
        // The grab: `Some(slot)` names the star over the erased cell, if any.
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
            // The caret's cell belongs to the mend's star: a live sky star
            // there (the typo key's strike) goes on the cap's finish
            // ([`EVICT_FINISH_MS`]) as the pulled star sets out.
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
        // The mend's pull (1e): from the grabbed star's own pixel, or one cell
        // left of home.
        let from_x = match grabbed {
            Some(Some(i)) => self.stars[i].pos(at, self.reduced).0 as f32,
            Some(None) => x - ctx.geom.cw as f32,
            None => x,
        };
        let pull = (x - from_x).max(0.0);
        // The slipstream (1d): a star born at speed inherits the hand's
        // backward drift (every sky lane), priced by the observed interval
        // (T4), clamped at the blank frontier (L4), gated on the birth spine
        // (for a mend's star, the resumed momentum).
        let birth_disp = spec
            .pull
            .map_or(ctx.birth_disp, |p| ctx.birth_disp.max(p.disp));
        let stream = if birth_disp >= STREAM_DISP {
            self.stream_reach(from_x, y, ctx)
        } else {
            0.0
        };
        let v0 = match (class, spec.lane) {
            // A mend's star is pinned once home (1e: "then pinned").
            _ if spec.pull.is_some() => (0.0, 0.0),
            // §5.6: a strike m1/m2 lifts; a grain and every field star are
            // pinned (zero `v0`).
            (StarClass::M1 | StarClass::M2, StarLane::Strike) => {
                // The lift is held under a rule's stroke (§5.4, §33): the zone
                // is under the stroke, so the lift is bounded by the headroom
                // left over every cell the slipstream carries the star across,
                // possibly none. Over a blank row the lift is §5.6's.
                let g = ctx.geom;
                let lift = sky_lift_px_per_s(g.ch as f32) / 1000.0;
                let life_ms = class.life_s(spec.lane) * 1000.0;
                let room = self.rule_headroom_px(px_col(x - stream, g)..=px_col(x, g), y, g);
                (0.0, lift.max(-room / life_ms))
            }
            _ => (0.0, 0.0),
        };
        if glint {
            // One token = one hero = one glint, same column, same frame (§13).
            self.glints.push(Glint {
                col: spec.cell.1,
                f: scint_rate(seed),
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
            f: scint_rate(seed),
            stream,
            pull,
            finish: None,
        };
        match grabbed {
            // The grab re-mints in place: one star, one slot, no churn.
            Some(Some(i)) => self.stars[i] = star,
            _ => self.sow(star),
        }
        true
    }

    /// Throw one free transient (§5.4: thrown anywhere, but a core never lands
    /// on a probed glyph). The birth pixel is exactly the caller's; the
    /// clearance applies where the star rests ([`Stardust::settle`]).
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
            // A transient is thrown, not dealt to a cell: no slipstream, no
            // pull.
            stream: 0.0,
            pull: 0.0,
            finish: None,
        };
        if self.settle(&mut star, spec.fixed, ctx) {
            self.sow(star);
        }
    }

    // -- birth zones (§5.4) ------------------------------------------------

    /// §5.4's sky band for `cell`, or `None` where no star may be born.
    ///
    /// Four gates, in refusal order:
    ///
    /// 1. the probe: `row − 1` must read `Some(false)`, else no star (L4,
    ///    D16). Row 0 answers `Some(false)`: the band above the grid is
    ///    `Geom.head`;
    /// 2. the zone: `y ∈ [top − 0.30 ch, top − 0.04 ch]`, `top` being the
    ///    cell's ribbon band top, or the spelling's anchor with no ribbon
    ///    (D16). The jitter uses the star's seed, so two stars on one cell
    ///    differ;
    /// 3. the glass: the pixel must lie in the effects box
    ///    (`[fx_top, fx_bot)`), or it would be clipped while spending a token;
    /// 4. the pixel: the centre is rounded to a device pixel, and a pixel
    ///    already holding a live star's birth is nudged to a clear neighbour
    ///    in the zone or refused (`no_two_sky_stars_share_a_pixel`).
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
        // §5.4's gate in its sky form: a blank, an off-grid band and a light
        // rule are clear; a glyph is ink; an unprobed row is unknown. Only
        // clear gets a star.
        if self.probe.sky_at(i32::from(row) - 1, col, ctx.geom.rows) != Some(false) {
            return None;
        }
        let g = ctx.geom;
        let ch = g.ch as f32;
        let top = ribbon
            .band(row, col)
            .map_or_else(|| default_band_top(g, ctx.cfg, row), |b| b.top);
        let j = mix32(seed ^ SALT_JITTER);
        let y = top - ch * (SKY_BOTTOM_CH + (SKY_TOP_CH - SKY_BOTTOM_CH) * hash01(j));
        let (cx, _) = g.cell_center(row, col);
        let jx = (hash01(mix32(j ^ 11)) * 2.0 - 1.0) * SKY_JITTER_CW * g.cw as f32;
        let (x, y) = ((cx + jx).round(), y.round());
        // The zone's own box, for the nudge to stay inside.
        let x_lo = (cx - SKY_JITTER_CW * g.cw as f32).round();
        let x_hi = (cx + SKY_JITTER_CW * g.cw as f32).round();
        let y_lo = (top - SKY_TOP_CH * ch).round();
        let y_hi = (top - SKY_BOTTOM_CH * ch).round();
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

    /// Whether a live star's birth pixel is exactly here. The birth pixel, not
    /// the current position: a lifted neighbour is not a stack. Every lane
    /// asks every lane, so nothing in the band is born on another mark.
    fn pixel_taken(&self, x: f32, y: f32) -> bool {
        self.stars.iter().any(|s| s.x == x && s.y == y)
    }

    /// The observed cadence (T4): the interval between this Typed key and the
    /// last, clamped to `[STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS]`, read from
    /// event instants, never the spine. A second event on the same instant
    /// keeps the interval already read.
    ///
    /// A coalesced echo is priced per key: a `Typed { cells: n }` carries `n`
    /// keystrokes, so the gap is divided by `cells` before the clamp.
    /// Otherwise a burst after a pause would read as a stroll.
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

    /// The slipstream's travel for one birth (px ≥ 0): the priced travel
    /// ([`stream_travel_px`] of [`stream_px_per_s`]) clamped at the blank
    /// frontier (L4): the centre may drift left only over cells of its band's
    /// row proved clear for the sky, contiguously, never past the pane's first
    /// column. Read once, at the birth edge, since the probe forgets rows.
    fn stream_reach(&self, x: f32, y: f32, ctx: &Ctx<'_>) -> f32 {
        let want = stream_travel_px(stream_px_per_s(ctx.geom.cw as f32, self.ioi_ms));
        // Read on the band's own row; the lift is the headroom clamp's
        // concern ([`Stardust::rule_headroom_px`]), so the span is a point.
        self.blank_frontier_px(x, (y, y), want, ctx)
    }

    /// The blank frontier: how far, px ≥ 0, something at `x` whose flight
    /// covers window-Y span `band` (`(top, bottom)`, inclusive) may travel
    /// left of its priced `want`, clamped at the first column not proved clear
    /// for the sky on every grid row the span touches (contiguously from its
    /// own column) and at the pane's first column. Shared by
    /// [`Stardust::stream_reach`] and [`Stardust::sow_exhaust`]. Zero for a
    /// non-finite or sub-half-pixel `want` and off the grid's left edge.
    ///
    /// The walk uses the birth's clearance ([`GlyphProbe::sky_at`]): a blank
    /// or a light rule is clear, a glyph is ink, an unprobed row stops the
    /// walk (`the_exhaust_flies_along_a_rule_row_in_the_sky_zone_under_the_stroke`).
    /// A rule forbids only a lift into its stroke, handled by
    /// [`Stardust::rule_headroom_px`].
    ///
    /// The rows are the flight's, not `row − 1` (D16): under `tall` the band is
    /// the lower third of `row − 1`; under `underline` it is in the caret's
    /// own row and a rise can reach the row above
    /// (`no_mark_crosses_ink_or_the_pane_edge_at_any_instant_under_either_band_anchor`).
    ///
    /// The walk stops once `x − floor_x` covers `want`, since further columns
    /// cannot change the answer; the cost is bounded by the priced travel
    /// (`the_blank_frontiers_early_stop_is_the_whole_walks_answer`).
    fn blank_frontier_px(&self, x: f32, band: (f32, f32), want: f32, ctx: &Ctx<'_>) -> f32 {
        let g = ctx.geom;
        if !want.is_finite() || want < 0.5 {
            return 0.0;
        }
        let col = px_col(x, g);
        if col < 0 {
            return 0.0;
        }
        // The rows the flight's pixels fall in: one under `tall`, two under
        // `underline` when the rise crosses into the row above.
        let (first, last) = (px_row(band.0, g), px_row(band.1, g));
        let mut frontier = col;
        let floor_of = |frontier: i32| (g.fx_left() + frontier * g.cw as i32) as f32;
        while frontier > 0 {
            // Already clear of the priced travel: no column further left can
            // lower `want.min(x − floor_x)`, so the walk is over.
            if x - floor_of(frontier) >= want {
                break;
            }
            let left = frontier - 1;
            let clear = u16::try_from(left).is_ok_and(|c| {
                (first..=last).all(|r| self.probe.sky_at(r, c, g.rows) == Some(false))
            });
            if !clear {
                break;
            }
            frontier = left;
        }
        want.min(x - floor_of(frontier)).max(0.0)
    }

    /// The headroom under a rule (§5.4, §33), px ≥ 0: how far a flight born at
    /// window `y` may lift before its core enters the stroke band
    /// ([`rule_stroke_px`]) of a light rule in any cell of `cols` on the
    /// birth's row; `f32::INFINITY` when no rule cell is among them. Shared by
    /// the exhaust's rise ([`Stardust::sow_exhaust`]) and the strike's lift
    /// ([`Stardust::deal_star`]).
    ///
    /// All the flight's columns are asked (a rule may end left of the caret),
    /// the shortest headroom winning; only the birth's row, since a rise is
    /// short enough that the only stroke it can meet is in the zone's own row
    /// (see the assertion beside [`RULE_INK_BOT_CH`]).
    ///
    /// Edges are [`rule_stroke_px`]'s whole pixels, as [`GlyphProbe::at_px`]
    /// uses, so the room is exactly the pixels from the birth to the first one
    /// under the stroke; `settle` is the second guard
    /// (`the_stroke_band_is_one_set_of_pixels_for_the_probe_the_zones_and_the_headroom`,
    /// `the_headroom_under_a_rule_reads_every_column_the_flight_crosses`).
    fn rule_headroom_px(&self, cols: std::ops::RangeInclusive<i32>, y: f32, g: Geom) -> f32 {
        let row = px_row(y, g);
        if !cols
            .into_iter()
            .any(|c| u16::try_from(c).is_ok_and(|c| self.probe.rule_at(row, c)))
        {
            return f32::INFINITY;
        }
        let row_top = f32::from(g.origin_y) + row as f32 * g.ch as f32;
        let band = rule_stroke_px(g.ch);
        if y < row_top + band.start as f32 {
            return f32::INFINITY;
        }
        (y - (row_top + band.end as f32)).max(0.0)
    }

    /// Where a mend's star is pulled from (1e): the erased cell `from`, on the
    /// grid with its sky proven blank (§5.4's gate, as for the home), or
    /// `None`: no pull, and the key is dealt normally. `Some(slot)` names the
    /// live sky star over the erased cell at `at`, if any, which the fix grabs
    /// rather than leaving a second star beside it.
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

    /// §5.4's free-transient clearance, applied where the core comes to rest.
    /// A transient may fly over ink (its coverage there divided by three,
    /// §5.2); L4 forbids a core that lands on a probed glyph, so the probe is
    /// asked of [`Star::rest`]. Returns whether the star may be sown; on
    /// `true` its `v0` may have been re-aimed.
    ///
    /// First the glass: a rest outside the effects box (`Geom::fx_*`) is pulled
    /// onto the box's nearest pixel ([`clamp_to_glass`]) and the throw
    /// re-aimed, so no transient holds a pool slot while clipped to nothing.
    /// Then, in order:
    ///
    /// 1. clear: the rest is not on a probed glyph; sown as thrown;
    /// 2. nudged: the nearest clear pixel within 2 px (the eight 1-px
    ///    neighbours, then the four axis neighbours at 2 px), with the throw
    ///    re-aimed to end there ([`Star::aim`]);
    /// 3. walked back, for a law-fixed slot ([`Throw::fixed`]): the rest walks
    ///    back along its throw to the first clear pixel; a grain is dropped
    ///    instead.
    ///
    /// A radial throw's origin cell is clear by exemption: every radial throw
    /// leaves a cell the caret owns (the landing, ledger-exempt: L2, §3.4,
    /// §6.5 layer 9; a hop's landing; a cell the erase just emptied), so the
    /// walk-back always terminates. Shed fragments and the exhaust (§33)
    /// start in nobody's cell and take no exemption.
    ///
    /// Unknown is not inked: a free transient may land on an unprobed row
    /// (§5.4 distinguishes a ribbon cell's sky from open air). Asked once, at
    /// birth.
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
        // The glass first: a rest off the effects box is pulled onto its edge
        // (the ray flattens or shortens) before the probe is asked. The fan's
        // own rise caps ([`fan_rise`]) mean this only catches rounding on a
        // fan; for other lanes it is the law.
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
        // The walk back: from the rest toward the origin along the throw's
        // own ray, one pixel per step, the origin itself the last station.
        let (dx, dy) = ((rest.0 - origin.0) as f32, (rest.1 - origin.1) as f32);
        let steps = dx.hypot(dy).ceil().max(1.0) as i32;
        let p = (0..steps)
            .rev()
            .map(|i| {
                let t = i as f32 / steps as f32;
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

    /// The field position a star inherits at its birth cell (§5.3). C2: the
    /// ribbon head, the caret and a star's halo on that cell resolve one
    /// spectrum position. A cell with no ribbon light falls back to the
    /// caret's `t`.
    fn field_t(&self, cell: (u16, u16), ctx: &Ctx<'_>, ribbon: &Ribbon) -> f32 {
        ribbon.field_at(cell.0, cell.1).unwrap_or(ctx.caret_t)
    }

    // -- the cap, the heroes, the finishes, the cull ------------------------

    /// §5.2's hero spacing, read off the live pool: not within
    /// [`HERO_MIN_SPACING_CELLS`] of a live sky hero on the same band, and
    /// never a third live hero on one band. Nothing is remembered.
    fn hero_allowed(&self, cell: (u16, u16), y: f32, geom: Geom) -> bool {
        let band = px_row(y, geom);
        let mut on_row = 0usize;
        for s in &self.stars {
            if s.class != StarClass::M1 || !s.lane.is_sky() || px_row(s.y, geom) != band {
                continue;
            }
            on_row += 1;
            let dc = (i32::from(cell.1) - px_col(s.x, geom)).abs();
            if dc < i32::from(HERO_MIN_SPACING_CELLS) {
                return false;
            }
        }
        on_row < usize::from(HERO_MAX_LIVE_PER_ROW)
    }

    /// Whether a sky star is live on this cell: the one-star-per-cell check
    /// every sky deal asks ([`STRIKE_REACH_CELLS`]). Transients (fan, shed,
    /// erase) are thrown, not dealt to a cell, and do not count.
    fn sky_star_live(&self, cell: (u16, u16), geom: Geom) -> bool {
        self.stars
            .iter()
            .any(|s| s.lane.is_sky() && star_over_cell(s, cell, geom))
    }

    /// The first cell without a live sky star from `cell` toward the caret's
    /// leading edge, within [`STRIKE_REACH_CELLS`], or `None` (dropped).
    fn free_sky_cell(&self, cell: (u16, u16), geom: Geom) -> Option<(u16, u16)> {
        (0..=STRIKE_REACH_CELLS)
            .map(|k| (cell.0, cell.1.saturating_add(k)))
            .find(|&c| !self.sky_star_live(c, geom))
    }

    /// The field deal adopts a cell's existing star instead of stacking a
    /// second: the star moves to the field lane (pinned, re-lit by this key)
    /// and lives the cell's life ([`field_lane_envelope`]). Class, tint, seed
    /// and birth pixel are kept, and any finish stands. Returns whether a star
    /// was adopted; `false` leaves the deal to birth a field star.
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

    /// Put the field stars of the cells the ribbon is retracting (from `caret`
    /// to the row's far end, as `Ribbon::retract_suffix` drains) on the
    /// ribbon's retract span, so they die with their cells (§5.6).
    fn finish_field(&mut self, caret: (u16, u16), span_ms: f32, at: Instant, geom: Geom) {
        let (row, col) = caret;
        for s in &mut self.stars {
            if s.lane != StarLane::Field {
                continue;
            }
            if px_col(s.x, geom) >= i32::from(col) && in_sky_of_row(s.y, row, geom) {
                s.finish_by(at, span_ms / 1000.0);
            }
        }
        // The veil over the retracted cells goes with them.
        for v in &mut self.veil {
            if v.row == row && v.col >= col {
                v.finish_by(at, span_ms / 1000.0);
            }
        }
    }

    /// The hand is still on this row: every field star in the caret row's sky
    /// is re-lit on the key ([`Star::lit`] ← `at`), so its fade runs from the
    /// last key on its row. Row-scoped: a row the hand has left goes out on
    /// its own clock. Inside the hold this changes nothing on glass; past it,
    /// the row's sky returns to full with a keystroke behind it. A star on a
    /// [`Finish`] keeps it.
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

    /// Focus lost (§8.2): every star goes on the 300 ms `spend` ember beside
    /// the ribbon's.
    fn ember(&mut self, at: Instant) {
        for s in &mut self.stars {
            s.finish_by(at, FOCUS_EMBER_MS / 1000.0);
        }
        for v in &mut self.veil {
            v.finish_by(at, FOCUS_EMBER_MS / 1000.0);
        }
    }

    /// A newborn is never refused (§5.6). At [`STAR_CAP`] live the oldest live
    /// star is put on its [`EVICT_FINISH_MS`] finish; at [`STAR_POOL_MAX`] on
    /// glass the finishing star nearest its end is dropped, keeping the pool
    /// a fixed allocation (§18).
    ///
    /// Oldest rather than least remaining life: the oldest is the star looked
    /// at longest. Eviction is unreachable below ~30 cps sustained.
    fn make_room(&mut self, now: Instant) {
        self.cull(now);
        let live = self.stars.iter().filter(|s| s.finish.is_none()).count();
        if live >= STAR_CAP
            && let Some(oldest) = self
                .stars
                .iter_mut()
                .filter(|s| s.finish.is_none())
                .min_by_key(|s| s.born)
        {
            oldest.finish_by(now, EVICT_FINISH_MS / 1000.0);
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

    /// Drop every star past its life or its finish (D3: at [`STAR_CULL_ALPHA`]
    /// the star is still a visible point, never a grey speck).
    fn cull(&mut self, now: Instant) {
        self.stars.retain(|s| !s.dead(now));
        self.veil.retain(|v| !v.dead(now));
    }

    /// One fresh seed, hashed from `(row, col, mint)` (§18). The mint counter
    /// stands in for `born`: a pure function of the event sequence, so the
    /// same events at the same `now` produce identical frames.
    fn mint_seed(&mut self, row: u16, col: u16) -> u32 {
        self.minted = self.minted.wrapping_add(1);
        cell_hash(row, col, self.minted)
    }

    // -- the aurora (section 1a′) -------------------------------------------

    /// Lay the veil over the cells this echo laid, one [`Veil`] per cell, on
    /// glass the frame that echoes the key (T2, from [`BIRTH_EDGE_FLOOR`]).
    ///
    /// Three gates: the cell is on the grid; the birth spine is at or over
    /// [`AURORA_COLD_DISP`]; and `row − 1` probes `Some(false)` (§5.4, L4).
    /// Its colour is the ribbon's field at the cell (C2), folded like the bed
    /// (`tri`); its gain is priced once from `Ctx::birth_disp` ([`veil_gain`]).
    /// At [`AURORA_CAP`] the cell lit longest ago gives up its slot.
    fn deal_veil(&mut self, cells: u16, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) {
        let gain = veil_gain(ctx.birth_disp);
        if gain <= 0.0 {
            return;
        }
        let (row, head) = ctx.caret;
        let g = ctx.geom;
        for k in 0..cells.max(1) {
            let col = head.saturating_sub(cells.max(1) - k);
            if usize::from(row) >= g.rows || usize::from(col) >= g.cols {
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
            } else if let Some(v) = self.veil.iter_mut().min_by_key(|v| v.lit) {
                *v = veil;
            }
        }
    }

    /// Draw the aurora: one clipped [`RainHalo`] per live veil cell.
    ///
    /// The ellipse is centred on the box's bottom edge with `ry` the box's
    /// height (full at the line, nothing at the top) and `rx`
    /// [`AURORA_REACH_CW`]. The box is the cell's own column from the sky
    /// band's anchor ([`Veil::bottom_px`]) up [`AURORA_TALL_CH`], never on the
    /// glyph row or a neighbouring column, so veils never sum: the brightest
    /// pixel is `AURORA_COV_PEAK · gain · env · intensity`, at most 40.
    ///
    /// Dark: additive `spectrum(tri(t))`, premultiplied. Light: the same veil
    /// as source-over ink (`InkRole::OverText`, §3.3). Culled below §3.2's
    /// chroma floor.
    fn draw_veil(&self, ctx: &Ctx<'_>, halos: &mut Vec<RainHalo>, light_cap: usize) {
        let g = ctx.geom;
        let ch = g.ch as f32;
        let intensity = clamp01(ctx.cfg.intensity);
        let cap = if ctx.cfg.dark_theme {
            halos.len() + AURORA_HALO_BUDGET
        } else {
            light_cap
        };
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
            let top = bottom - AURORA_TALL_CH * ch;
            let (y0, y1) = (top.round() as i32, bottom.round() as i32);
            if y1 <= y0 {
                continue;
            }
            let (cx, _) = g.cell_center(v.row, v.col);
            let x0 = i32::from(g.origin_x) + i32::from(v.col) * g.cw as i32;
            let x1 = x0 + g.cw as i32;
            let rgb = spectrum(clamp01(tri(v.t)));
            let (color, mode) = if ctx.cfg.dark_theme {
                (premul_rgb(rgb, byte), HaloMode::Add)
            } else {
                let a = u32::from(byte).clamp(1, InkRole::OverText.alpha_cap() as u32);
                (
                    (InkRole::OverText.ink(rgb) & 0x00FF_FFFF) | (a << 24),
                    HaloMode::Over,
                )
            };
            push_halo_quads_clipped(
                halos,
                g,
                cap,
                HaloSpec {
                    centre: (cx.round() as i32, y1),
                    radii: ((AURORA_REACH_CW * g.cw as f32).round() as i32, y1 - y0),
                    color,
                    mode,
                },
                (x0, y0, x1, y1),
            );
        }
    }
}

/// The fan's class at throw position `k` (§5.6, D6): 1 gold m1 + 4 m2 + rest
/// m3 in throw order, or with no hero (an Enter, D8) 1 m2 + rest m3.
fn fan_class(k: usize, hero: bool) -> StarClass {
    match (k, hero) {
        (0, true) => StarClass::M1,
        (0, false) | (1..=4, true) => StarClass::M2,
        _ => StarClass::M3,
    }
}

/// Whether `star` sits over `cell`: this column, in the cell's row or the one
/// above (the sky band is in row − 1 under `tall`, in the cell's row under
/// `underline`, D16).
fn star_over_cell(star: &Star, cell: (u16, u16), geom: Geom) -> bool {
    let up = i32::from(cell.0) - px_row(star.y, geom);
    px_col(star.x, geom) == i32::from(cell.1) && (0..=1).contains(&up)
}

/// §5.2's light-theme thinning for a thrown star: on light a grain is dealt
/// at [`LIGHT_COUNT_SCALE`] by its seed; on dark every grain is born. Sky
/// deals scale their probability instead ([`deal`]).
fn light_keeps(seed: u32, cfg: &Config) -> bool {
    cfg.dark_theme || hash01(mix32(seed ^ SALT_LIGHT)) < LIGHT_COUNT_SCALE
}

// ===========================================================================
// 7. Paint: one star, resolved for one frame
// ===========================================================================

/// The quads a grain needs (one white core, four tinted arms), and the floor
/// of a star's fair share of the quad budget, so a truncated frame still
/// draws every star's core.
const M3_QUADS: usize = 5;

// The floor holds only if the full pool can afford it. `emit`'s share is
// `floor(left / stars still to draw)` and every recipe consumes at most its
// share, so the ratio never falls: it starts at
// `STARDUST_QUAD_BUDGET / STAR_POOL_MAX` and every star of a full pool gets
// at least its cross.
const _: () = assert!(
    STARDUST_QUAD_BUDGET >= M3_QUADS * STAR_POOL_MAX,
    "the quad budget cannot hand every star of a full pool its five-quad floor"
);

/// One star resolved for one frame: everything §5.7's recipes need, so they
/// read as the spec writes them (`m3(sx, sy, c, env)`, `m2(...)`, `m1(...)`).
/// Every field is derived; nothing is stored between frames.
#[derive(Clone, Copy, Debug)]
struct Paint {
    /// Magnitude, which picks the recipe.
    class: StarClass,
    /// Population — the m2's disc is a SKY-lane addition only (§5.2).
    lane: StarLane,
    /// Device-pixel centre.
    x: i32,
    /// Device-pixel centre.
    y: i32,
    /// The integer arm at this instant, the arm clock's output (§5.5, D12).
    arm: i32,
    /// `c`: the class's coverage request scaled by the envelope, by
    /// `intensity`, and for a transient over a probed-occupied or unknown cell
    /// by `1/3` ([`TEXT_SAFE_DIV`]). The ceilings are structural, so this is
    /// never clamped.
    c: f32,
    /// The population envelope itself — the star's alpha, for the chroma cull.
    alpha: f32,
    /// The class's own core-brightness multiplier on the luminance clock:
    /// `0.85 + 0.15·e` (m1), `0.70 + 0.30·e` (m2), `env` (m3).
    core: f32,
    /// `twinkle_env` itself — the halo rides it undiluted.
    env: f32,
    /// The star's chroma, fixed at birth (C3).
    tint: u32,
    /// Gold and at twinkle peak: the only condition under which any mark in
    /// this theme throws diagonal glints (§5.2).
    glints: bool,
    /// A dealt gold star — its halo holds gold rather than breathing a hue
    /// ([`halo_rgb`]).
    gold: bool,
}

impl Paint {
    /// Resolve one star for this frame, or `None` if it has nothing to draw.
    fn of(star: &Star, probe: &GlyphProbe, ctx: &Ctx<'_>) -> Option<Self> {
        let rm = ctx.cfg.reduced_motion;
        let alpha = star.envelope(ctx.now, rm);
        if alpha < STAR_CULL_ALPHA {
            // D3: below the cull a star is gone, never a grey speck.
            return None;
        }
        // §5.6's hold holds the light too ([`Star::in_hold`]): core at full,
        // halo at peak. The live twinkle is evaluated once; the glint reads it
        // even inside the hold.
        let held = rm || star.in_hold(ctx.now);
        // Under reduced motion nothing reads it (held; no glints).
        let tw = if rm { 1.0 } else { star.twinkle(ctx.now) };
        let (env, e) = if held {
            (1.0, 1.0)
        } else {
            // `e = (env − 0.55)/0.45 ∈ [0, 1]` — the twinkle as a unit depth,
            // which is what §5.5's per-class core-brightness rules take.
            (tw, clamp01((tw - 0.55) / 0.45))
        };
        let (x, y) = star.pos(ctx.now, rm);
        let floor = star.class.core_floor();
        let mut c = star.class.cov(star.lane) * alpha * clamp01(ctx.cfg.intensity);
        // §5.2's text-safe divisor applies to transients only. A sky star was
        // proven blank at birth and moves only over cells proven blank then
        // (`Stardust::stream_reach`, `Stardust::pull_origin`); the probe
        // forgets rows and, under `underline`, reports the star's own echoed
        // glyph, so re-asking would pop it to a third (`TEXT_SAFE_DIV`).
        if star.lane.is_transient() && probe.at_px(x, y, ctx.geom) != Some(false) {
            c /= TEXT_SAFE_DIV;
        }
        Some(Self {
            class: star.class,
            lane: star.lane,
            x,
            y,
            arm: arm_px(star, ctx.geom.ch as f32, held, ctx.now),
            c,
            alpha,
            core: if star.class == StarClass::M3 {
                env
            } else {
                floor + (1.0 - floor) * e
            },
            env,
            tint: star.tint,
            // [`Star::glinting`] on the one twinkle.
            glints: !rm && star.gold && twinkle_peak(tw),
            gold: star.gold,
        })
    }

    /// The dark recipes (§5.7).
    ///
    /// D2: `push_twinkle_star` draws body and nucleus in one colour, so the
    /// body is `#FFFFFF` and temperature is carried by the halo plus four
    /// [`STUB_LEN_PX`]-px stubs outside `star_body_px(arm)` (a grain's by its
    /// outboard arms). D1: the bars plus nucleus already stack to
    /// `STAR_STACK_ADD 2.35`, so no class adds a second core rect; the m2's
    /// 3×3 disc is the sky m2's shoulder only (`2.35·48 + 0.35·48 = 130`),
    /// the transient m2 keeping `2.35·34 = 80`.
    ///
    /// `quad_cap` is this star's share of the quad budget; `haloed` counts
    /// stars given a halo so far; `index` is this frame's laid sky by cell,
    /// the ground a star is priced over ([`sky_ground`]).
    ///
    /// Every class is held under the caret law ([`STAR_LIGHT_CEIL`]) in two
    /// steps. Haloed classes are priced before drawing ([`price_centre`]),
    /// colour before white where the ground lacks room. Then every class is
    /// settled after drawing ([`settle_under_ceiling`]): the off-centre pixels
    /// where stub, taper and halo ring meet are read back from its quads and
    /// the star dimmed by one factor until they fit. That matters for yellow
    /// and green heroes, whose stops carry most of the eye's luminance.
    fn draw_add(
        &self,
        ctx: &Ctx<'_>,
        frame: &mut Frame<'_>,
        quad_cap: usize,
        haloed: &mut usize,
        index: &mut SkyIndex,
    ) {
        let geom = ctx.geom;
        // Everything laid before this star (and the aurora) enters the index
        // once, then is queried by cell.
        index.ingest(frame);
        // The pixels this star's recipe stacks on, and the sky under each —
        // read before its own quads go out, since they would be in the slice.
        let d = if self.class == StarClass::M3 {
            1
        } else {
            star_body_px(self.arm) + 1
        };
        let mut watch = [((0i32, 0i32), 0u32); SETTLE_PIXELS];
        for (slot, (dx, dy)) in watch.iter_mut().zip([
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
                // The body is `M2_BODY_TINT` of the way from white to the tint;
                // the disc is the tint, and the sky m2's shoulder only (D1)
                // (owner, 2026-09-08).
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
                    // The 3×3 disc (5×5 on the retina cell): an m2's one
                    // addition, and a shoulder rather than a second core (D1).
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
        settle_under_ceiling(frame, q0, h0, &watch);
    }

    /// §5.7's m3: a white 1-px core `1.6·c·env`, plus four tinted arms of
    /// `arm` px at [`M3_CROSS_ADD`]`·c` running outboard of it.
    ///
    /// The cross is its Airy disc and its whole colour; the arms start one
    /// pixel out so the peak stays the core (D2). Five quads, one peak at
    /// `1.6·c`, each 8-neighbour at `0.45·c`: 3.6:1 at the crest and 2:1 at
    /// the trough, since the core rides `env` and the disc alpha alone (§5.5).
    /// The centre byte is `round(1.6·c·env)` in one conversion, landing on the
    /// published integer (61 transient, 98 sky).
    ///
    /// `quad_cap` is this grain's share of the quad budget, checked between
    /// pushes: the core goes first and always, and an arm over the cap is
    /// dropped (`an_over_budget_frame_keeps_every_star_s_core`,
    /// `a_grain_s_share_is_honoured_push_by_push_core_first`).
    fn draw_m3(&self, geom: Geom, out: &mut Vec<GlowQuad>, quad_cap: usize) {
        // The grain is pure tint, core included; the peak law holds on the
        // tint's brightest channel (owner, 2026-09-08).
        let core = premul_rgb(self.tint, cov_byte(self.c * M3_CENTRE_ADD * self.core));
        let disc = premul_rgb(self.tint, cov_byte(self.c * M3_CROSS_ADD));
        let a = self.arm;
        // Core first, so a star truncated by the quad budget keeps its peak.
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

    /// The four tint stubs (§5.7): [`STUB_LEN_PX`] px each, outboard from
    /// `±(star_body_px(arm) + 1)` on each axis at [`STUB_COV_SHARE`]`·c`,
    /// strictly outside the body so D2 is checkable per quad. Four quads.
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

    /// The halo's radius: [`M1_HALO_R_ARM`] / [`M2_HALO_R_ARM`] times the
    /// class's nominal arm (`star_arm`, the float), the m2's floored at
    /// [`m2_halo_r_min_px`]: 8 / 6 px at 1×, 11 / 7.7 at `ch` 18, 16 / 12 at
    /// 2×. Not the clocked integer arm, whose 8–12 Hz steps would be a size
    /// flicker outside D12's exemption. `0` for the grain.
    fn halo_r(&self, ch: f32) -> f32 {
        let nominal = star_arm(ch, self.class.arm_ratio());
        match self.class {
            StarClass::M1 => M1_HALO_R_ARM * nominal,
            StarClass::M2 => (M2_HALO_R_ARM * nominal).max(m2_halo_r_min_px(ch)),
            StarClass::M3 => 0.0,
        }
    }

    /// The star's halo request: its colour now ([`halo_rgb`]) and the peak the
    /// recipe asks, riding `env` undiluted so the colour breathes while the
    /// core holds (§5.5). Budgeted per star ([`STARDUST_HALO_BUDGET`]) and
    /// culled below §3.2's chroma floor (`α·env < 0.12`); either yields a peak
    /// of `0`, so an undrawn halo is never priced.
    ///
    /// The exhaust never asks (§33): budget is spent in pool order, and
    /// per-key debris would take the heroes' slots.
    fn halo_request(&self, share: f32, haloed: usize) -> (u32, u8) {
        let rgb = halo_rgb(self.tint, self.gold, self.env);
        if self.lane == StarLane::Exhaust
            || haloed >= STARDUST_HALO_BUDGET
            || self.alpha * self.env < CHROMA_CULL_ALPHA
        {
            return (rgb, 0);
        }
        (rgb, cov_byte(self.c * share * self.env))
    }

    /// Lay the halo [`Self::halo_request`] asked for, at the `peak` the
    /// caret law left it ([`price_centre`]) — nothing at `0`.
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

    /// The light fork (§3.3, §5.2's light twins): one operator flip, same
    /// anatomy, timings and curves.
    ///
    /// Additive light is invisible on paper-white, so every mark becomes
    /// source-over ink: m1/m2 are the family's light star (the
    /// `push_twinkle_over` silhouette at [`STAR_ARM_INK`], core in the tint's
    /// deep ink, `InkRole::Leading` cap 236, needles in over-text ink,
    /// `InkRole::OverText` cap 190), and m3 is one round dot priced by the
    /// stacked-ink law. No atmosphere halo; the ink carries the tint.
    ///
    /// The silhouette's numbers are the family's (`STAR_WAIST`,
    /// `STAR_TAPER_BODY`, `STAR_CORE`, `STAR_GLINT*`), shared with
    /// `cursor_glow`'s private rasterizer, which is not exported.
    fn draw_over(&self, geom: Geom, frame: &mut Frame<'_>, halo_cap: usize) {
        if frame.halos.len() >= halo_cap {
            return;
        }
        let arm = (self.arm as f32 * STAR_ARM_INK).max(1.0);
        match self.class {
            StarClass::M3 => {
                let cov = cov_byte(self.c * self.core);
                let a = stacked_ink_alpha(cov, InkRole::OverText).min(LIGHT_ALPHA_CAP as u32);
                let ink = (InkRole::OverText.ink(self.tint) & 0x00FF_FFFF) | (a << 24);
                let r = arm.round() as i32;
                push_halo_quads(
                    frame.halos,
                    geom,
                    halo_cap,
                    (self.x, self.y),
                    (r.max(1), r.max(1)),
                    ink,
                    HaloMode::Over,
                );
            }
            StarClass::M1 | StarClass::M2 => {
                push_twinkle_over_twin(
                    frame.halos,
                    geom,
                    halo_cap,
                    LightStar {
                        centre: (self.x, self.y),
                        arm,
                        rgb: self.tint,
                        gold: self.glints,
                        cov: cov_byte(self.c * self.core),
                    },
                );
            }
        }
    }
}

/// The arm clock at the frame's `now` (§5.5, D12, T7).
///
/// `arm(t) = A·(floor + (1 − floor)·sin²(2π·f·age + φ))`, rounded so the
/// star visibly opens 1 → 2 → 1 px, floored at the class's band
/// ([`StarClass::arm_min_px`]). `A` is the class's rung on the arm ladder.
/// Inside the hold, and under reduced motion, the arm is the nominal
/// `round(A)`: `held` is `reduced_motion || star.in_hold(now)`.
fn arm_px(star: &Star, ch: f32, held: bool, now: Instant) -> i32 {
    let nominal = star_arm(ch, star.class.arm_ratio());
    let min = star.class.arm_min_px(ch);
    if held {
        return (nominal.round() as i32).max(min);
    }
    let floor = star.class.arm_floor();
    let s = (std::f32::consts::TAU * star.f * star.age_s(now) + star.lum_phase()).sin();
    ((nominal * (floor + (1.0 - floor) * s * s)).round() as i32).max(min)
}

/// Seconds until `θ = θ_now + ω·t` next reaches `±a₀ (mod π)` — the
/// crossings of `sin²θ` (and of `|sin θ|`) with a level, for the two
/// scintillation clocks' closed-form steps. `a₀` is in `[0, π/2]`; a
/// crossing at `θ_now` itself does not count.
#[must_use]
fn next_sine_crossing_s(theta: f32, a0: f32, omega: f32) -> Option<f32> {
    use std::f32::consts::PI;
    if !(omega.is_finite() && theta.is_finite() && a0.is_finite()) || omega <= 0.0 {
        return None;
    }
    let base = theta.rem_euclid(PI);
    let mut gap = f32::INFINITY;
    for cand in [a0, PI - a0, PI + a0, 2.0 * PI - a0] {
        let g = cand - base;
        if g > 1e-5 && g < gap {
            gap = g;
        }
    }
    if gap.is_finite() {
        Some(gap / omega)
    } else {
        None
    }
}

// ===========================================================================
// 8. Small shared shapes
// ===========================================================================

/// Salt for the one strike draw (§5.6's m1 / m2 / m3 partition).
const SALT_STRIKE: u32 = 0x0000_0101;
/// Salt for the second grain's light-theme deal.
const SALT_SECOND_M3: u32 = 0x0000_0202;
/// Salt for the field deal.
const SALT_FIELD: u32 = 0x0000_0404;
/// Salt for the field's own m2 sub-deal.
const SALT_FIELD_M2: u32 = 0x0000_0505;
/// Salt for the tint deal (§5.3).
const SALT_TINT: u32 = 0x0000_0606;
/// Salt for the luminance phase φ (§5.5).
const SALT_PHASE: u32 = 0x0000_0707;
/// Salt for the arm-size rate `f` (§5.5).
const SALT_RATE: u32 = 0x0000_0808;
/// Salt for the birth jitter inside the sky band (§5.4).
const SALT_JITTER: u32 = 0x0000_0909;
/// Salt for the erase throw (§5.6).
const SALT_ERASE: u32 = 0x0000_0A0A;
/// Salt for the light-theme thinning of a thrown grain (§5.2).
const SALT_LIGHT: u32 = 0x0000_0B0B;
/// Salt for the exhaust's class deal and its rise jitter (§33). Distinct from
/// every salt above, so a cell's grain and its exhaust are independent draws.
const SALT_EXHAUST: u32 = 0x0000_0E7A;

/// The family's avalanche mix, the same two rounds `cursor_glow` uses, so a
/// v2 star and a v1 spark hashed from one cell do not correlate.
#[inline]
pub(super) fn mix32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    h
}

/// `(row, col, salt)` → seed (§18: every seed is hashed at the spawn edge; no
/// per-frame RNG anywhere).
#[inline]
fn cell_hash(row: u16, col: u16, salt: u32) -> u32 {
    mix32(
        u32::from(row).wrapping_mul(73_856_093)
            ^ u32::from(col).wrapping_mul(19_349_663)
            ^ salt.wrapping_mul(83_492_791),
    )
}

/// A seed's uniform draw on `[0, 1)`.
#[inline]
pub(super) fn hash01(h: u32) -> f32 {
    (h >> 8) as f32 / 16_777_216.0
}

/// One deal: "one in `den`", scaled by the light theme's count factor. A
/// probability test rather than a modulo, so the light fork's × 0.6 thins the
/// same sky instead of reshuffling which cells are starred.
#[inline]
fn deal(cell: u32, salt: u32, den: u32, scale: f32) -> bool {
    hash01(mix32(cell ^ salt)) < scale / den as f32
}

/// The same deal against a share rather than a denominator (for lerped rates
/// such as flow's field share): the same draw as [`deal`] with
/// `share == scale / den`, to the bit.
#[inline]
fn deal_p(cell: u32, salt: u32, share: f32) -> bool {
    hash01(mix32(cell ^ salt)) < share
}

/// The combo ladder's rungs, in claim order: the highest rung a run length
/// has reached wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rung {
    /// [`LADDER_M2_KEYS`] — the strike is an m2 at least.
    Promote,
    /// [`LADDER_WHITE_KEYS`] — an m2, white by rule.
    White,
    /// [`LADDER_GOLD_KEYS`] — an earned gold m1, glinting if a token is on
    /// hand.
    Gold,
}

/// Which rung a run of `combo` keys pays on its newest key, if any. Zero pays
/// nothing (a broken run owes nothing).
#[inline]
#[must_use]
fn ladder_rung(combo: u32) -> Option<Rung> {
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

/// The seeded arm-size scintillation rate in Hz, `8 + 4·hash01` (§5.5). The
/// audio glint that rides a star twinkles at this same number (§13).
#[inline]
fn scint_rate(seed: u32) -> f32 {
    SCINT_F_MIN + (SCINT_F_MAX - SCINT_F_MIN) * hash01(mix32(seed ^ SALT_RATE))
}

/// The tint by class (§5.3): a grain always wears its birth cell's stop and
/// is never gold (it has no glints); an m1 or m2 takes the [`deal_tint`]
/// deal.
#[inline]
fn class_tint(class: StarClass, seed: u32, field_t: f32) -> (u32, bool) {
    if class == StarClass::M3 {
        (spectrum_snap(field_t), false)
    } else {
        deal_tint(seed, field_t)
    }
}

/// The 75/10/15 tint deal (§5.3), dealt by seed and constant for life as
/// `(tint, gold)`, for an m1 or m2 ([`class_tint`] routes grains past it)
/// (owner, 2026-09-08).
///
/// 75 % take `spectrum_snap(field at the birth cell)` (warm over the old end,
/// violet over the new), 10 % an A-type white, 15 % gold, the only class with
/// diagonal glints. Gold-ness is the deal's answer, not the colour's: the
/// arc's yellow stop has gold's RGB.
#[inline]
fn deal_tint(seed: u32, field_t: f32) -> (u32, bool) {
    let u = hash01(mix32(seed ^ SALT_TINT));
    if u < TINT_FIELD_SHARE {
        (spectrum_snap(field_t), false)
    } else if u < TINT_FIELD_SHARE + TINT_WHITE_SHARE {
        (TINT_WHITE_RGB, false)
    } else {
        (TINT_GOLD_RGB, true)
    }
}

/// A coverage request as a byte: the one float→byte conversion on the emit
/// path, round-half and saturating.
///
/// A class's centre sums three or four independently converted bytes;
/// truncating would shed up to a unit each (a sky m2 would land at 126, not
/// §5.2's 130). Rounding keeps every published centre within one byte, and
/// matches `aterm_render::premul_rgb`.
#[inline]
fn cov_byte(v: f32) -> u8 {
    if v.is_nan() {
        0
    } else {
        (v + 0.5).clamp(0.0, 255.0) as u8
    }
}

/// The grid row a window-absolute Y falls in (signed: the sky band above row 0
/// is row − 1, and that is a real answer, not an error).
#[inline]
fn px_row(y: f32, geom: Geom) -> i32 {
    ((y - f32::from(geom.origin_y)) / geom.ch as f32).floor() as i32
}

/// The grid column a window-absolute X falls in (signed, like [`px_row`]).
#[inline]
fn px_col(x: f32, geom: Geom) -> i32 {
    ((x - f32::from(geom.origin_x)) / geom.cw as f32).floor() as i32
}

/// Whether a star at window Y is in the sky of grid `row`: the lower third of
/// the row above under `tall`, the top of the row under `underline` (§5.4),
/// so "row or row − 1". Shared by [`Stardust::finish_field`] and
/// [`Stardust::relight_field`].
#[inline]
fn in_sky_of_row(y: f32, row: u16, geom: Geom) -> bool {
    (0..=1).contains(&(i32::from(row) - px_row(y, geom)))
}

/// Whether a device pixel is inside the effects box (`Geom::fx_*`: the grid
/// plus the `head` band, §5.4's row-0 sky). The glass test for every birth
/// and rest in this file.
#[inline]
pub(super) fn on_glass(p: (i32, i32), geom: Geom) -> bool {
    p.0 >= geom.fx_left() && p.0 < geom.fx_right() && p.1 >= geom.fx_top() && p.1 < geom.fx_bot()
}

/// The nearest pixel of the effects box to `p` (`p` itself when on glass):
/// where [`Stardust::settle`] pulls a transient's off-glass rest, the ray
/// keeping its origin and flattening toward the line.
#[inline]
fn clamp_to_glass(p: (i32, i32), geom: Geom) -> (i32, i32) {
    (
        p.0.clamp(geom.fx_left(), geom.fx_right() - 1),
        p.1.clamp(geom.fx_top(), geom.fx_bot() - 1),
    )
}

/// The fan's rise caps for one landing, `(up, down)` px: the most a fan star
/// may rest above and below the landing's centre (see [`FAN_SQUASH`]). The
/// ellipse's semi-minor axis `reach·FAN_SQUASH`, capped at
/// [`FAN_RISE_MAX_CH`] and at the effects box, on every row: on row 0 the fan
/// may rise into the `Geom.head` band, but only up to the same 1.6 ch.
/// Computed once per landing.
fn fan_rise(reach: f32, at_px: (f32, f32), geom: Geom) -> (f32, f32) {
    let squash = reach * FAN_SQUASH;
    let ceiling = FAN_RISE_MAX_CH * geom.ch as f32;
    let room_up = (at_px.1 - geom.fx_top() as f32).max(0.0);
    let room_down = ((geom.fx_bot() - 1) as f32 - at_px.1).max(0.0);
    (
        squash.min(ceiling).min(room_up),
        squash.min(ceiling).min(room_down),
    )
}

/// One fan ray under the vertical reach law: the throw's displacement at rest,
/// px. `a` is the seeded angle, `reach` the star's radial length, `up`/`down`
/// the landing's rise caps ([`fan_rise`]), `j` the star's cap jitter in `0..1`
/// ([`FAN_RISE_JITTER_MIN`]). A ray exceeding its cap is flattened toward the
/// line, keeping its length, never dropped (D6). Window y grows downward.
fn fan_ray(a: f32, reach: f32, up: f32, down: f32, j: f32) -> (f32, f32) {
    let (dx, dy) = (a.cos() * reach, a.sin() * reach);
    let cap = if dy < 0.0 { up } else { down };
    let cap = cap * (FAN_RISE_JITTER_MIN + (1.0 - FAN_RISE_JITTER_MIN) * j);
    if dy.abs() <= cap {
        return (dx, dy);
    }
    let along = (reach * reach - cap * cap).max(0.0).sqrt();
    (along.copysign(dx), cap.copysign(dy))
}

/// D16's two band-top spellings, for a cell with nothing laid.
///
/// Under `tall` the body rises `RIBBON_TALL_UP 1.10 ch` from the row bottom,
/// so its top is 0.10 `ch` above the cell and the sky band is the lower third
/// of row − 1. Under `underline` the band sits at the cell's top, placing
/// §5.4's `[top − 0.30 ch, top − 0.04 ch]` on `[0.04, 0.30]·ch` of the cell.
fn default_band_top(geom: Geom, cfg: &Config, row: u16) -> f32 {
    let ch = geom.ch as f32;
    let cell_top = f32::from(geom.origin_y) + f32::from(row) * ch;
    if cfg.ribbon_tall {
        cell_top + ch - super::ribbon::TALL_UP_CH * ch
    } else {
        cell_top + (SKY_TOP_CH + SKY_BOTTOM_CH) * ch
    }
}

/// One halo: a radial peak split into per-cell-row [`RainHalo`] quads sharing
/// one centre and falloff. `Add` for dark atmosphere (premultiplied colour),
/// `Over` for light ink (ink RGB with the centre over-alpha in the high
/// byte). `cap` bounds `out.len()`.
///
/// The local twin of `cursor_glow`'s private `push_halo` / `push_halo_over`;
/// the meteor's coma, nucleus and spark halos use it too. The centre is the
/// falloff's truth: it is stored, never clamped into the box, since clamping
/// an off-edge centre would render the sliver far too bright.
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

/// One radial falloff as [`push_halo_quads_clipped`] takes it — the four
/// numbers of a halo that are not its box.
#[derive(Clone, Copy, Debug)]
struct HaloSpec {
    /// The falloff's centre, window px.
    centre: (i32, i32),
    /// Its radii, px.
    radii: (i32, i32),
    /// Premultiplied (`Add`) or ink-with-ceiling (`Over`).
    color: u32,
    /// The operator.
    mode: HaloMode,
}

/// [`push_halo_quads`] with an explicit clip box `(x0, y0, x1, y1)`
/// (half-open, window px) intersected with the glass, for the aurora's
/// per-column veil ([`Stardust::draw_veil`]). The centre is stored, never
/// clamped.
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
    let mut yy = y0;
    while yy < y1 {
        if out.len() >= cap {
            return;
        }
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color,
            cx: cx as u16,
            cy: cy as u16,
            rx: rx as u16,
            ry: ry as u16,
            mode,
        });
        yy = band_end;
    }
}

// ===========================================================================
// 8a. The aurora's cell, and the colour helpers the coloured sky needs
// ===========================================================================

/// One cell of the aurora: the veil above one laid ribbon cell (1a′). `Copy`
/// and flat like a [`Star`]; everything on glass derives from these fields
/// and `now`.
#[derive(Clone, Copy, Debug)]
pub struct Veil {
    /// The laid cell's row.
    pub row: u16,
    /// The laid cell's column — the veil's box is this column, and only this
    /// column.
    pub col: u16,
    /// The ribbon's field at the cell (C2), unfolded — the bed folds it with
    /// `tri` when it draws, and so does the veil.
    pub t: f32,
    /// The veil's peak, as a share of [`AURORA_COV_PEAK`], priced once at
    /// birth from `Ctx::birth_disp` ([`veil_gain`]).
    pub gain: f32,
    /// The key that laid the cell — the `edge-in` and the horizon read it.
    pub born: Instant,
    /// The last key on the row — the field fade reads it
    /// ([`Stardust::relight_field`]).
    pub lit: Instant,
    /// The finish this cell is on, if any: its retract, or the focus ember.
    pub finish: Option<Finish>,
}

impl Veil {
    /// Seconds since the key that laid the cell, saturating at zero.
    #[must_use]
    pub fn age_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.born).as_secs_f32()
    }

    /// Seconds since the last key on the row, saturating at zero.
    #[must_use]
    pub fn idle_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.lit).as_secs_f32()
    }

    /// The veil's alpha at `now`: the field lane's envelope
    /// ([`field_lane_envelope`]), so it leaves with the ribbon like the field
    /// stars.
    #[must_use]
    pub fn envelope(&self, now: Instant, reduced_motion: bool) -> f32 {
        let fin = self.finish.map_or(1.0, |f| f.gain(now));
        field_lane_envelope(self.age_s(now), self.idle_s(now), fin, reduced_motion)
    }

    /// True once the cell must leave the pool — the field star's own three
    /// exits ([`Star::dead`]).
    #[must_use]
    pub fn dead(&self, now: Instant) -> bool {
        self.age_s(now) >= FIELD_LIFE_S
            || self.idle_s(now) >= field_fade_life_s()
            || self.finish.is_some_and(|f| f.gain(now) < STAR_CULL_ALPHA)
    }

    /// Put this cell on a finish ending at `at + span_s`, unless one that
    /// ends sooner already stands ([`Star::finish_by`]'s rule).
    pub fn finish_by(&mut self, at: Instant, span_s: f32) {
        Finish::keep_sooner(&mut self.finish, at, span_s);
    }

    /// The window Y the veil's box ends at (exclusive) and its falloff centres
    /// on: the row's sky band anchor ([`default_band_top`], the ribbon's top
    /// under `tall`), never below the glyph row's top, so under `underline` the
    /// veil still sits wholly above the text.
    #[must_use]
    pub fn bottom_px(&self, geom: Geom, cfg: &Config) -> f32 {
        let cell_top = f32::from(geom.origin_y) + f32::from(self.row) * geom.ch as f32;
        default_band_top(geom, cfg, self.row).min(cell_top)
    }
}

/// The field lane's envelope, shared by the field star ([`Star::envelope`])
/// and the aurora's veil ([`Veil::envelope`]), both being the cell's sky
/// (§5.6), as the product of three alpha clocks:
///
/// * the birth: the cell's 18 ms `edge-in` from [`BIRTH_EDGE_FLOOR`] (T2);
/// * the horizon: the ribbon's `expiry_melt` over [`FIELD_LIFE_S`];
/// * the hand: [`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`] from the row's last key:
///   full while the hand is there, half at 550 ms, culled at 880 ms, before
///   the ribbon's retract.
///
/// Times `fin`, the finish if any. Under reduced motion the linear fade
/// replaces both curves at each span's end.
#[must_use]
pub fn field_lane_envelope(age_s: f32, idle_s: f32, fin: f32, reduced_motion: bool) -> f32 {
    if reduced_motion {
        let fade = REDUCED_MOTION_FADE_MS / 1000.0;
        return clamp01((FIELD_LIFE_S - age_s) / fade.max(f32::EPSILON))
            * reduced_fade(field_fade_life_s() - idle_s)
            * fin;
    }
    let birth = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age_s);
    let hand = half_life(idle_s, FIELD_HOLD_MS / 1000.0, FIELD_TAU_MS / 1000.0);
    birth * expiry_melt(age_s / FIELD_LIFE_S.max(f32::EPSILON)) * hand * fin
}

/// The aurora's gain at a birth spine: `0` below [`AURORA_COLD_DISP`], then
/// `smoothstep` up to `1` at a spine of one (0.37 at 0.5, 0.71 at 0.7).
#[must_use]
pub fn veil_gain(birth_disp: f32) -> f32 {
    if !birth_disp.is_finite() || birth_disp < AURORA_COLD_DISP {
        return 0.0;
    }
    smoothstep01((birth_disp - AURORA_COLD_DISP) / (1.0 - AURORA_COLD_DISP))
}

/// `a` moved `k` of the way toward `b`, per channel, rounded — the m2's
/// body colour ([`M2_BODY_TINT`]).
#[inline]
#[must_use]
fn mix_rgb(a: u32, b: u32, k: f32) -> u32 {
    let k = clamp01(k);
    let ch = |sh: u32| -> u32 {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        (x + (y - x) * k).round().clamp(0.0, 255.0) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// The halo's colour now: the star's stop, breathed
/// [`HALO_HUE_SWING_ENTRIES`] entries along the arc on the luminance clock
/// (`env` 0.55..=1 maps to −15..+15 from `SPECTRUM_ANCHOR_AT`), landing on a
/// whole `SPECTRUM_LUT` entry. White, dealt gold and any non-stop tint hold
/// their colour.
#[must_use]
fn halo_rgb(tint: u32, gold: bool, env: f32) -> u32 {
    if gold || tint == TINT_WHITE_RGB {
        return tint;
    }
    let Some(stop) = (0..SPECTRUM_STOPS).find(|&i| spectrum_stop(i) == tint) else {
        return tint;
    };
    // `env ∈ [0.55, 1]` → `[−1, 1]`.
    let breath = clamp01((env - 0.55) / 0.45) * 2.0 - 1.0;
    let step = (breath * HALO_HUE_SWING_ENTRIES as f32).round() as i32;
    let at = (SPECTRUM_ANCHOR_AT[stop] as i32 + step).clamp(0, SPECTRUM_LUT_LEN as i32 - 1);
    SPECTRUM_LUT[at as usize]
}

#[cfg(test)]
thread_local! {
    /// Test-only work census of the ground query: `[scan quad tests, scan halo
    /// tests, indexed quad candidates, indexed halo candidates]`. Under
    /// `cfg(test)` [`SkyIndex::ground`] checks itself against [`sky_ground`]
    /// on every query; read by `a_point_query_reads_one_cell_not_the_whole_sky`.
    static GROUND_WORK: std::cell::Cell<[u64; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// Add `n` to census slot `slot` ([`GROUND_WORK`]).
#[cfg(test)]
fn note_ground_work(slot: usize, n: u64) {
    GROUND_WORK.with(|c| {
        let mut w = c.get();
        w[slot] += n;
        c.set(w);
    });
}

/// The empty link of a [`SkyIndex`] chain.
const SKY_INDEX_END: u32 = u32::MAX;

/// [`SkyIndex`]'s chain count — a power of two. Sized well over the lays one
/// frame can file so the chains stay at a handful of entries; two `u32`
/// heads per slot is the index's largest allocation (8 KiB, resident).
const SKY_INDEX_SLOTS: usize = 1024;

/// Room for the quad chains' nodes — one per cell a laid quad touches. A
/// star's lays are ≤ 2 cells wide and the row split keeps each inside one
/// row, so twice [`STARDUST_QUAD_BUDGET`] is the frame's own ceiling.
const SKY_INDEX_QUAD_NODES: usize = STARDUST_QUAD_BUDGET * 2;

/// Room for the halo chains' nodes. The aurora's veils are one column each
/// ([`STARDUST_LIGHT_HALO_BUDGET`] of them at most), and a star's atmosphere
/// is ≤ 3 × 3 cells with [`STARDUST_HALO_BUDGET`] stars haloed.
const SKY_INDEX_HALO_NODES: usize = STARDUST_LIGHT_HALO_BUDGET + 9 * STARDUST_HALO_BUDGET;

/// How many cells one lay may be filed under. Nothing the sky lays comes
/// close (a hero's atmosphere is ~3 × 3), so this bounds filing cost.
///
/// It is also required for correctness: [`SkyIndex::slot`] must be injective
/// over the cells of one lay's box, or a lay would be counted twice. An
/// exhaustive sweep finds no colliding pair while a box is at most 19 cells;
/// at 20 it collides. Raising this needs a re-sweep:
/// `the_slot_hash_is_injective_over_every_box_the_cap_allows`.
const SKY_INDEX_WIDE_CELLS: i64 = 16;

/// The sky laid so far, indexed by cell: the slices [`sky_ground`] reads
/// (`frame.out[out_from..]` and `frame.halos[sky_from..]`), filed under the
/// grid cells they touch as they are pushed, so a point query walks one
/// cell's candidates instead of rescanning the frame.
///
/// A lay is filed under every cell its box touches, so a lay covering
/// `(x, y)` is always in that cell's chain.
///
/// Slots are shared by a hash of the cell. A collision between different
/// lays costs one extra `quad_covers` test. Two cells of the same lay on one
/// chain would double-count its light, so no two cells of one lay's box may
/// share a slot; [`SKY_INDEX_WIDE_CELLS`] guarantees it
/// (`the_slot_hash_is_injective_over_every_box_the_cap_allows`).
///
/// It never allocates: arenas are sized once, and a lay that will not fit
/// (wider than [`SKY_INDEX_WIDE_CELLS`], or past an arena's end) spills the
/// index, sending every query back to the whole-slice scan (§18).
///
/// Membership is indexed; bytes are read live from the slices at query time,
/// so [`settle_under_ceiling`] dimming a filed quad is seen, and the answer
/// equals [`sky_ground`]'s (checked by `debug_assert` in tests). The slices
/// only grow within one emit, so entries never go stale.
#[derive(Clone, Debug, Default)]
struct SkyIndex {
    /// Grid-interior top-left in window px — the cell anchor.
    origin: (i32, i32),
    /// Cell size in px, or `0` where the geometry has none (then every lay
    /// shares cell `(0, 0)`, a full scan).
    cell: (i32, i32),
    /// Where this frame's sky begins in `frame.out` / `frame.halos` — every
    /// filed entry is an offset from these.
    from: (usize, usize),
    /// How much of each slice is filed — [`SkyIndex::ingest`] catches up.
    filed: (usize, usize),
    /// A lay did not fit: read the whole slice, as the scan did.
    spilled: bool,
    /// Per-slot head of the quad chain ([`SKY_INDEX_END`] when empty).
    quad_head: Vec<u32>,
    /// Per-slot head of the `Add` halo chain.
    halo_head: Vec<u32>,
    /// `(entry, next)` arena for the quad chains.
    quad_node: Vec<(u32, u32)>,
    /// `(entry, next)` arena for the halo chains.
    halo_node: Vec<(u32, u32)>,
    /// The slots written this frame — what `open` has to empty again.
    dirty: Vec<u32>,
}

impl SkyIndex {
    /// Open the index on one frame: this geometry's cells, emptied, anchored
    /// at the slice offsets this frame's sky starts at.
    fn open(&mut self, geom: Geom, from: (usize, usize)) {
        if self.quad_head.len() == SKY_INDEX_SLOTS {
            // Only the slots this index actually wrote need emptying.
            for &s in &self.dirty {
                self.quad_head[s as usize] = SKY_INDEX_END;
                self.halo_head[s as usize] = SKY_INDEX_END;
            }
        } else {
            // The one allocation this index ever makes, on its first frame.
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

    /// The grid cell a window pixel falls in — the same floor division the
    /// pushers tag their row with, on both axes.
    #[inline]
    fn cell_at(&self, x: i32, y: i32) -> (i32, i32) {
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

    /// The chain a cell hashes to. A collision between lays costs one extra
    /// covering test; within one lay it would double-count, which
    /// [`SKY_INDEX_WIDE_CELLS`] rules out.
    #[inline]
    fn slot((cx, cy): (i32, i32)) -> usize {
        let k = (cx as u32)
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add((cy as u32).wrapping_mul(0x85EB_CA6B));
        (k >> 13) as usize & (SKY_INDEX_SLOTS - 1)
    }

    /// File entry `i` of one stream, whose box is `(x, y, w, h)`, into every
    /// cell it touches — or spill the index when it does not fit.
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
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                let s = Self::slot((cx, cy));
                if self.quad_head[s] == SKY_INDEX_END && self.halo_head[s] == SKY_INDEX_END {
                    self.dirty.push(s as u32);
                }
                if quads {
                    let next = self.quad_head[s];
                    self.quad_node.push((i as u32, next));
                    self.quad_head[s] = self.quad_node.len() as u32 - 1;
                } else {
                    let next = self.halo_head[s];
                    self.halo_node.push((i as u32, next));
                    self.halo_head[s] = self.halo_node.len() as u32 - 1;
                }
            }
        }
    }

    /// File everything pushed since the last call. Called once per star
    /// before its ground is read, so each lay is filed exactly once per frame.
    fn ingest(&mut self, frame: &Frame<'_>) {
        if self.spilled {
            return;
        }
        while self.filed.0 < frame.out.len() - self.from.0 {
            let i = self.filed.0;
            self.filed.0 += 1;
            let q = frame.out[self.from.0 + i];
            // Only premultiplied additive light is ever added ([`quad_covers`]).
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
            // `Over` veils are the light theme's ink and add nothing here.
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

    /// [`sky_ground`] at `(x, y)` off the index: the theme ground plus every
    /// laid quad and `Add` halo of this frame's sky covering the pixel, over
    /// one cell's candidates.
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

/// One cell's chain of a [`SkyIndex`], iterating the entries themselves; the
/// bytes are read from the live slice, so a lay dimmed after filing reads
/// true.
struct Chain<'a, T> {
    /// The `(entry, next)` arena the chain runs through.
    nodes: &'a [(u32, u32)],
    /// The next node, or [`SKY_INDEX_END`].
    at: u32,
    /// The slice the entries are offsets into.
    items: &'a [T],
}

impl<'a, T> Chain<'a, T> {
    /// The chain that starts at `head`.
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

    fn next(&mut self) -> Option<&'a T> {
        if self.at == SKY_INDEX_END {
            return None;
        }
        let (entry, next) = self.nodes[self.at as usize];
        self.at = next;
        Some(&self.items[entry as usize])
    }
}

/// The ground under a star: the theme ground plus everything the sky already
/// laid this frame at `(x, y)`: additive quads of earlier stars (`add_sat`,
/// as `draw_flat_add` lays them) and every `Add` halo (veils, earlier
/// atmospheres) through the renderers' falloff bytes (`halo_row_ny` /
/// `halo_weight`). [`price_centre`] prices against it, so a hero in a veil
/// is held to the same ceiling as one on bare glass.
///
/// Light a later star lays on this one's pixels is not included.
///
/// `laid` and `sky` are candidates: `add_sat` is order-independent and a
/// non-covering entry contributes zero, so [`SkyIndex::ground`]'s candidate
/// list and a test's whole slice give the same answer.
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
            // The quad's colour at this column: a ribbon slab ramps between
            // its ends (`GlowQuad::color2`); a flat quad is `q.color`.
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

/// What one `Add` halo lays on `(x, y)`: its premultiplied colour through the
/// renderers' elliptical falloff (`halo_row_ny` / `halo_weight`), `0` outside
/// its box or reach.
#[must_use]
fn halo_add_at(h: &RainHalo, x: i32, y: i32) -> u32 {
    if !(i32::from(h.x)..i32::from(h.x) + i32::from(h.w)).contains(&x)
        || !(i32::from(h.y)..i32::from(h.y) + i32::from(h.h)).contains(&y)
    {
        return 0;
    }
    let ny = halo_row_ny(y - i32::from(h.cy), i32::from(h.ry) * i32::from(h.ry));
    if ny >= 256 {
        return 0;
    }
    let wt = halo_weight(x - i32::from(h.cx), ny, i32::from(h.rx) * i32::from(h.rx));
    if wt <= 0 {
        return 0;
    }
    premul_rgb(h.color, wt.min(255) as u8)
}

/// Does this additive quad cover `(x, y)`?
#[inline]
#[must_use]
fn quad_covers(q: &GlowQuad, x: i32, y: i32) -> bool {
    // The half-open box `[x, x + w) × [y, y + h)` as plain compares:
    // [`sky_ground`] asks every laid quad, per probed pixel, and an
    // unoptimized build calls `Range::contains` out of line.
    let (qx, qy) = (i32::from(q.x), i32::from(q.y));
    q.alpha == 0 && qx <= x && x < qx + i32::from(q.w) && qy <= y && y < qy + i32::from(q.h)
}

/// The pixels [`settle_under_ceiling`] watches on one star: its centre, the
/// first pixel of each arm past the body (where stub, taper and halo ring
/// meet), and its four orthogonal neighbours.
pub const SETTLE_PIXELS: usize = 9;

/// The closer ([`STAR_LIGHT_CEIL`]): one star just drawn, read back from its
/// own quads `frame.out[q0..]` and halo pieces `frame.halos[h0..]` (one per
/// text row) at each `watch`ed pixel (with the sky already under it), and
/// dimmed by one factor until every pixel composites under the ceiling. The
/// factor is found on the bytes: a probe scales every own lay by `k/255`
/// through `premul_rgb`, the same rounding used to write them. A star that
/// fits is untouched; one that does not keeps its designed ratios and
/// colour at lower light.
///
/// Bounded: ≤ 8 probes × [`SETTLE_PIXELS`] × the star's quads (≤ 15) and
/// halo pieces; no allocation.
fn settle_under_ceiling(
    frame: &mut Frame<'_>,
    q0: usize,
    h0: usize,
    watch: &[((i32, i32), u32); SETTLE_PIXELS],
) {
    let own_at = |k: u8, (x, y): (i32, i32)| -> u32 {
        let mut own = 0u32;
        for q in &frame.out[q0..] {
            if quad_covers(q, x, y) {
                own = add_sat(own, premul_rgb(q.color, k));
            }
        }
        for h in &frame.halos[h0..] {
            let piece = RainHalo {
                color: premul_rgb(h.color, k),
                ..*h
            };
            own = add_sat(own, halo_add_at(&piece, x, y));
        }
        own
    };
    let fits = |k: u8| {
        watch
            .iter()
            .all(|&(p, ground)| under_ceil(add_sat(ground, own_at(k, p))))
    };
    let k = fit_byte(u8::MAX, fits);
    if k == u8::MAX {
        return;
    }
    for q in &mut frame.out[q0..] {
        q.color = premul_rgb(q.color, k);
        q.color2 = premul_rgb(q.color2, k);
    }
    for h in &mut frame.halos[h0..] {
        h.color = premul_rgb(h.color, k);
    }
}

/// Under [`STAR_LIGHT_CEIL`]?
#[inline]
#[must_use]
fn under_ceil(px: u32) -> bool {
    relative_luminance(px) <= STAR_LIGHT_CEIL
}

/// What `push_twinkle_star` stacks on the crossing pixel at coverage `cov`
/// in `rgb` (two bodies and the nucleus, in its own bytes) over `base`: the
/// recipe's `STAR_STACK_ADD 2.35`, integer-exact.
#[must_use]
fn stacked_centre(base: u32, rgb: u32, cov: u8) -> u32 {
    let lay = premul_rgb(rgb, cov);
    let core = premul_rgb(rgb, (f32::from(cov) * STAR_CORE_ADD) as u8);
    add_sat(add_sat(add_sat(base, lay), lay), core)
}

/// The largest `k ≤ want` for which `under(k)` holds; `under` is monotone,
/// so this bisects the byte in at most eight probes. `0` when nothing
/// passes.
#[must_use]
fn fit_byte(want: u8, under: impl Fn(u8) -> bool) -> u8 {
    if under(want) {
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

/// The caret law spent on one star's centre ([`STAR_LIGHT_CEIL`]).
///
/// `body` is the crossing's colour and requested coverage (stacked as
/// `push_twinkle_star` stacks it), `disc` the m2's 3×3 shoulder (`0` for the
/// hero), `halo` the atmosphere's colour and peak (added in full on the
/// centre pixel). Returns what each may be so the composited centre over
/// `ground` stays under the ceiling:
///
/// * a coloured star spends colour first (the halo's request, then the
///   disc's) and the white body takes what is left, so saturation costs no
///   luminance; yellow and green heroes cost the body most
///   (`the_caret_law_spends_colour_first_and_white_last_on_a_star_s_centre`);
/// * an achromatic star spends its body first, the halo taking the rest;
/// * a ground already over the ceiling leaves the request unchanged.
///
/// Every probe composites in the rasterizers' integer arithmetic.
#[must_use]
fn price_centre(
    ground: u32,
    achromatic: bool,
    body: (u32, u8),
    disc: (u32, u8),
    halo: (u32, u8),
) -> (u8, u8, u8) {
    if !under_ceil(ground) {
        return (body.1, disc.1, halo.1);
    }
    let lay = |base: u32, rgb: u32, k: u8| add_sat(base, premul_rgb(rgb, k));
    if achromatic {
        let cov = fit_byte(body.1, |k| under_ceil(stacked_centre(ground, body.0, k)));
        let centre = stacked_centre(ground, body.0, cov);
        let d = fit_byte(disc.1, |k| under_ceil(lay(centre, disc.0, k)));
        let centre = lay(centre, disc.0, d);
        let peak = fit_byte(halo.1, |k| under_ceil(lay(centre, halo.0, k)));
        (cov, d, peak)
    } else {
        let peak = fit_byte(halo.1, |k| under_ceil(lay(ground, halo.0, k)));
        let lit = lay(ground, halo.0, peak);
        let d = fit_byte(disc.1, |k| under_ceil(lay(lit, disc.0, k)));
        let lit = lay(lit, disc.0, d);
        let cov = fit_byte(body.1, |k| under_ceil(stacked_centre(lit, body.0, k)));
        (cov, d, peak)
    }
}

/// One light-theme star, as [`push_twinkle_over_twin`] takes it:
/// `push_twinkle_star`'s parameter set minus the quad budget.
#[derive(Clone, Copy, Debug)]
struct LightStar {
    /// Device-pixel centre.
    centre: (i32, i32),
    /// The arm half-length in px, already taxed by `STAR_ARM_INK` (§3.3).
    arm: f32,
    /// The star's tint — the ink roles derive their deep and over-text inks
    /// from it.
    rgb: u32,
    /// Whether to lay the four diagonal glint dots (gold, at twinkle peak).
    gold: bool,
    /// The star's additive coverage request, as a byte.
    cov: u8,
}

/// The light-theme star, the `push_twinkle_over` silhouette (§3.3, §5.2): a
/// horizontal and a vertical arm, each a full-length tip lay and a shorter
/// body lay, on a round nucleus, plus four diagonal glint dots when gold,
/// all source-over falloff ellipses. The nucleus is laid first in the tint's
/// deep ink (`InkRole::Leading`); the arms use over-text ink
/// (`InkRole::OverText`, 190), since they can cross a letterform.
///
/// The per-lay alpha solves for the three-lay centre stack
/// (`STAR_OVER_CENTRE_LAYS` of `STAR_OVER_LAYS`), so the crossing matches
/// the family's plus and the taper shows at the points.
fn push_twinkle_over_twin(halos: &mut Vec<RainHalo>, geom: Geom, cap: usize, star: LightStar) {
    let LightStar {
        centre,
        arm,
        rgb,
        gold,
        cov,
    } = star;
    if cov == 0 || arm < 0.75 {
        return;
    }
    let lay_of = |role: InkRole| -> u32 {
        let cap_a = u32::from(cov).clamp(1, role.alpha_cap() as u32);
        let a = cap_a as f32 / 255.0;
        let lay = (1.0 - (1.0 - a).powf(STAR_OVER_CENTRE_LAYS / STAR_OVER_LAYS as f32)) * 255.0;
        (role.ink(rgb) & 0x00FF_FFFF) | ((lay.round() as u32).clamp(1, cap_a) << 24)
    };
    let core_ink = lay_of(InkRole::Leading);
    let arm_ink = lay_of(InkRole::OverText);
    let px = |v: f32| (v.round() as i32).max(1);
    let waist = px((arm * STAR_WAIST).max(1.0));
    let body = px((arm * STAR_TAPER_BODY).max(1.0));
    let core = px((arm * STAR_CORE).max(1.0));
    let arm_px = px(arm);
    // Nucleus first, so a star truncated by the halo budget keeps its centre.
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
        // The four-point sparkle's dim secondary points, as the additive gold
        // star throws, so the warm half is the same mark on both grounds.
        let d = px(arm * STAR_GLINT);
        let gcap = ((f32::from(cov) * STAR_GLINT_COV) as u32)
            .clamp(1, InkRole::OverText.alpha_cap() as u32);
        let glint = (InkRole::OverText.ink(rgb) & 0x00FF_FFFF) | (gcap << 24);
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

/// The over-alpha a round light grain is priced at so it composites like
/// the three-lay plus (`cursor_glow`'s `stacked_ink_alpha` on
/// `STAR_OVER_CENTRE_LAYS`): `1 − (1 − a)³`, bounded by the role's cap.
fn stacked_ink_alpha(cov: u8, role: InkRole) -> u32 {
    let lays = STAR_OVER_CENTRE_LAYS;
    let a1 = f32::from(cov.min(role.alpha_cap() as u8)) / 255.0;
    let cap1 = role.alpha_cap() / 255.0;
    let ceiling = (1.0 - (1.0 - cap1).powf(lays)) * 255.0;
    ((1.0 - (1.0 - a1).powf(lays)) * 255.0).clamp(1.0, ceiling) as u32
}

// ===========================================================================
// 9. The laws, measured (§20.1)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::super::Flow;
    use super::super::spine::FLOW_ENTRY_KEYS;
    use super::*;
    use crate::rainbow_kitty::ribbon::RETRACT_START_S;
    use crate::rainbow_kitty::{KillScope, Mend};
    use crate::spectrum::spectrum_snap_index;
    use aterm_render::add_sat;
    use std::time::Duration;

    /// A 120×40 grid of 9×18 cells with a 40 px chrome band above it, so row
    /// 0's sky band is inside the effects box and §5.4's off-grid arm is
    /// exercisable.
    fn geom() -> Geom {
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

    fn cfg(dark: bool) -> Config {
        Config {
            dark_theme: dark,
            intensity: 1.0,
            duration: Duration::from_millis(900),
            ribbon_tall: true,
            ribbon_flat: false,
            theme_fg: 0x00FF_FFFF,
            theme_bg: 0x0000_0000,
            reduced_motion: false,
        }
    }

    /// [`geom`] at another cell size, `cw × ch` device px (e.g. 7×14 at 1×,
    /// 15×28 at 2×), for the optical-size law.
    fn geom_cell(cw: usize, ch: usize) -> Geom {
        Geom {
            cw,
            ch,
            win_w: (120 * cw) as u16,
            win_h: (40 + 40 * ch) as u16,
            ..geom()
        }
    }

    fn ctx(now: Instant, cfg: &Config, caret: (u16, u16)) -> Ctx<'_> {
        ctx_in(geom(), now, cfg, caret)
    }

    /// [`ctx`] over a chosen geometry.
    fn ctx_in(geom: Geom, now: Instant, cfg: &Config, caret: (u16, u16)) -> Ctx<'_> {
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
            flow: Default::default(),
        }
    }

    /// The borrowed host scratch a [`Frame`] is built over.
    #[derive(Default)]
    struct Scratch {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<aterm_render::BeamVertex>,
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
                caret: super::super::CaretSeam::default(),
                companion: None,
                fp: 0,
            }
        }
    }

    /// The composite at one window pixel — the renderers' own arithmetic, so a
    /// test measures the pixel the eye gets and not the request that made it.
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

    /// The brightest channel.
    fn lum(p: u32) -> u32 {
        ((p >> 16) & 0xff).max((p >> 8) & 0xff).max(p & 0xff)
    }

    /// A seed whose luminance phase is at its peak on frame 0. The envelope
    /// peaks every ~349 ms and an m3 lives 225 ms, so a sweep could miss its
    /// peak; §5.2's published centres are the `env = 1` values the peak law
    /// needs to reach.
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

    /// One star at a known pixel with a known tint, the fixture every
    /// per-class law drives. Gold iff the tint is gold.
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

    /// A sky with rows 2-4 probed blank (the fixture star at `y = 100` is row
    /// 3), so §5.2's text-safe third does not apply.
    fn sky() -> Stardust {
        let mut sky = Stardust::new();
        for row in 2..=4 {
            sky.probe_mut().probe_row(row, &[false; 120]);
        }
        sky
    }

    /// Emit one frame of a pool at `now` and hand back the scratch.
    fn frame_at(sky: &mut Stardust, now: Instant, cfg: &Config) -> Scratch {
        let mut sc = Scratch::default();
        let mut f = sc.frame();
        sky.emit(&ctx(now, cfg, (3, 10)), &mut f);
        sc
    }

    /// §5.2's published composited centres as literals: the oracle `centre()`
    /// is measured against.
    fn every_class() -> [(StarClass, StarLane, f32); 6] {
        [
            (StarClass::M1, StarLane::Strike, 143.0),
            (StarClass::M2, StarLane::Strike, 130.0),
            (StarClass::M3, StarLane::Strike, 98.0),
            (StarClass::M1, StarLane::Fan, 118.0),
            (StarClass::M2, StarLane::Fan, 80.0),
            (StarClass::M3, StarLane::Fan, 61.0),
        ]
    }

    fn typed(class: TypedClass) -> Event {
        Event::Typed {
            cells: 1,
            shifted: matches!(class, TypedClass::Capital),
            class,
        }
    }

    fn col_of(s: &Star) -> i32 {
        px_col(s.x, geom())
    }

    fn dist(a: (i32, i32), b: (i32, i32)) -> f32 {
        ((a.0 - b.0) as f32).hypot((a.1 - b.1) as f32)
    }

    /// The key instant of key `k` at exactly 12 cps.
    fn key_at(t0: Instant, k: u16) -> Instant {
        t0 + Duration::from_micros(u64::from(k) * 83_333)
    }

    /// The spine the engine's integrator reaches by key `k` of a sustained
    /// 12 cps run: `1.3 · (1 − e^(−t/2))` clamped at 1 (0.45 after one second,
    /// 1.0 at ~2.9 s), so tests price births on the number the seam would
    /// hand over.
    fn disp_at_12cps(k: u16) -> f32 {
        let t = f32::from(k) / 12.0;
        (1.3 * (1.0 - (-t / 2.0).exp())).min(1.0)
    }

    /// The max − min channel gap of a premultiplied colour — the judge's
    /// "chroma", read off one quad or halo.
    fn chroma(p: u32) -> u32 {
        let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
        r.max(g).max(b) - r.min(g).min(b)
    }

    /// D1: a star is a peak, not a blob (§5.1 no. 1, §20.1).
    ///
    /// On the composited pixel the centre exceeds each of its eight neighbours
    /// by ≥ 30 % and reaches §5.2's published centre (143/130/98 sky,
    /// 118/80/61 transient), two-sided since the ceiling is a law too. The
    /// centre is measured on its brightest channel, the tint's own for
    /// tinted classes. §3.2 rank 8 is checked as "the halo may not outshine
    /// the core" (61 under 143, 38 under 130).
    #[test]
    fn a_star_is_a_peak_not_a_blob() {
        let c = cfg(true);
        let t0 = Instant::now();
        for (class, lane, want) in every_class() {
            let mut sky = sky();
            sky.sow(star(class, lane, t0, TINT_WHITE_RGB));
            // Sweep the life: the luminance clock is seeded, so the peak frame
            // has to be found rather than assumed.
            let life_ms = (class.life_s(lane) * 1000.0) as u64;
            let mut best = (0u32, 0u64);
            for ms in 0..life_ms {
                let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
                let v = lum(px(&sc.out, 200, 100));
                if v > best.0 {
                    best = (v, ms);
                }
            }
            assert!(
                (best.0 as f32 - want).abs() <= 1.0,
                "{class:?}/{lane:?} centre {} is not §5.2's {want}",
                best.0
            );
            // §20.1's floor: every class ≥ 80/255, the transient grain ≥ 61,
            // with one byte of slack: `push_twinkle_star` truncates its nucleus
            // byte, so the transient m2's `2.35 · 34` lands on 79.
            let floor = if class == StarClass::M3 && lane.is_transient() {
                61.0
            } else {
                80.0
            };
            assert!(
                best.0 as f32 >= floor - 1.0,
                "{class:?}/{lane:?} peak {} under the {floor}/255 floor",
                best.0
            );
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(best.1), &c);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let n = lum(px(&sc.out, 200 + dx, 100 + dy));
                    assert!(
                        best.0 as f32 >= 1.3 * n as f32,
                        "{class:?}/{lane:?} neighbour ({dx},{dy}) at {n} is not 30 % under {}",
                        best.0
                    );
                }
            }
            // §3.2 rank 8: the halo may not outshine its core.
            for h in &sc.halos {
                assert!(
                    lum(h.color) < best.0,
                    "{class:?}/{lane:?} halo peak {} outshines the core {}",
                    lum(h.color),
                    best.0
                );
            }
        }
    }

    /// The stars carry the rainbow: the grain is pure tint, the m2 is tinted
    /// to its core, the hero keeps a white-hot centre under a coloured halo
    /// (§5.2 / §5.3, §20.1).
    ///
    /// On the composited pixel: an m3's centre and every quad is the pure stop
    /// (`spread == max channel`); an m2's centre carries `M2_BODY_TINT` of the
    /// tint (spread `≥ 0.5·max`, no grey quad); an m1's quads are white
    /// (`r == g == b`) except its four tinted stubs, its colour being the
    /// halo (`stardust_is_coloured_on_the_glass_not_white_with_a_tint`).
    #[test]
    fn the_grain_and_the_m2_wear_their_tint_to_the_core_and_the_hero_stays_white_hot() {
        let c = cfg(true);
        let t0 = Instant::now();
        for (class, lane, _) in every_class() {
            let mut sky = sky();
            sky.sow(star(class, lane, t0, 0x0000_00FF));
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(1), &c);
            let centre = px(&sc.out, 200, 100);
            let (r, g, b) = ((centre >> 16) & 0xff, (centre >> 8) & 0xff, centre & 0xff);
            let grey = sc.out.iter().filter(|q| chroma(q.color) == 0).count();
            match class {
                StarClass::M3 => {
                    assert!(
                        r == 0 && g == 0 && b > 0,
                        "{class:?}/{lane:?} centre {centre:#08x} is not the pure stop"
                    );
                    assert_eq!(grey, 0, "{class:?}/{lane:?} carries a grey quad");
                }
                StarClass::M2 => {
                    assert!(
                        chroma(centre) * 2 >= lum(centre) && b > r && r == g,
                        "{class:?}/{lane:?} centre {centre:#08x} is not tinted to its core"
                    );
                    assert_eq!(grey, 0, "{class:?}/{lane:?} carries a grey quad");
                }
                StarClass::M1 => {
                    assert!(
                        r == g && g == b && r > 0,
                        "{class:?}/{lane:?} centre {centre:#08x} is not white-hot"
                    );
                    let mut tinted = 0usize;
                    for q in sc.out.iter().filter(|q| chroma(q.color) > 0) {
                        tinted += 1;
                        // Chebyshev distance from the centre to the quad's
                        // NEAREST pixel — the stubs lie outside the body.
                        let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w) - 1);
                        let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h) - 1);
                        let dx = (x0 - 200).max(200 - x1).max(0);
                        let dy = (y0 - 100).max(100 - y1).max(0);
                        assert!(
                            dx.max(dy) >= 2,
                            "{class:?}/{lane:?} tinted {}x{} quad at ({dx},{dy}) is inside the body",
                            q.w,
                            q.h
                        );
                    }
                    assert_eq!(
                        tinted, 4,
                        "{class:?}/{lane:?} carries {tinted} stubs, not four"
                    );
                }
            }
        }
    }

    /// D12: a grain opens and closes (§5.1 no. 3, §5.5, §20.1). At 8-12 Hz
    /// over 225 ms the integer arm visibly steps 1 → 2 → 1 px; measured off
    /// the drawn cross's width, which must change at least four times.
    #[test]
    fn a_grain_opens_and_closes_at_least_twice_in_its_life() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        sky.sow(star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB));
        let life_ms = (StarClass::M3.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut widths = Vec::new();
        for ms in 0..life_ms {
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
            let w = sc.out.iter().map(|q| q.w).max().unwrap_or(0);
            if widths.last() != Some(&w) {
                widths.push(w);
            }
        }
        assert!(
            widths.len() >= 5,
            "the grain changed size {} times in {life_ms} ms: {widths:?}",
            widths.len() - 1
        );
        // §5.5's photosensitivity exemption is an area bound: a mark that
        // scintillates in size must stay inside 10×10 device px at 2×.
        let widest = widths.iter().copied().max().unwrap_or(0);
        assert!(
            i32::from(widest) * 2 <= SCINT_BBOX_MAX_PX,
            "the grain's bbox {widest} px is outside the size-scintillation exemption at 2x"
        );
    }

    /// §5.5: an m2 never collapses to the grain's arm. Its band is 2 ↔ 3 px;
    /// a 1-px arm would be an m3's silhouette. Measured off the drawn extent.
    #[test]
    fn an_m2_arm_never_collapses_to_the_grain() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        // A coloured tint, so the pure-tint stubs and disc are not counted
        // into the body's extent (the body is `M2_BODY_TINT` of the way).
        let tint = spectrum_snap(0.2);
        let body = mix_rgb(TINT_WHITE_RGB, tint, M2_BODY_TINT);
        sky.sow(star(StarClass::M2, StarLane::Strike, t0, tint));
        let life_ms = (StarClass::M2.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut extents = std::collections::BTreeSet::new();
        for ms in 0..life_ms {
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
            // The body's horizontal extent on the centre row.
            let (mut lo, mut hi) = (i32::MAX, i32::MIN);
            for q in sc.out.iter().filter(|q| {
                i32::from(q.y) == 100 && q.color == premul_rgb(body, lum(q.color) as u8)
            }) {
                lo = lo.min(i32::from(q.x));
                hi = hi.max(i32::from(q.x) + i32::from(q.w));
            }
            let extent = hi - lo;
            assert!(
                extent > 2 * StarClass::M2.arm_min_px(geom().ch as f32),
                "at {ms} ms the m2's arm collapsed to a {extent}-px extent"
            );
            extents.insert(extent);
        }
        assert!(
            extents.len() >= 2,
            "the m2 arm never scintillated: {extents:?}"
        );
    }

    /// C3: a star never greys (§5.1 no. 5, §3.2, §20.1). Chroma is fixed at
    /// birth, so every colour a star emits is its tint premultiplied by some
    /// coverage, never a blend toward white or the ground.
    #[test]
    fn stars_fade_by_alpha_at_a_snapped_stop() {
        let c = cfg(true);
        let t0 = Instant::now();
        let tint = spectrum_snap(0.62);
        let mut sky = sky();
        sky.sow(star(StarClass::M1, StarLane::Strike, t0, tint));
        let life_ms = (StarClass::M1.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut last_lit = 0u64;
        for ms in 0..life_ms {
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
            if !sc.out.is_empty() {
                last_lit = ms;
            }
            for q in &sc.out {
                let ok = (0..=255u16).any(|k| premul_rgb(tint, k as u8) == q.color)
                    || (0..=255u16).any(|k| premul_rgb(TINT_WHITE_RGB, k as u8) == q.color);
                assert!(
                    ok,
                    "quad colour {:#08x} is not {tint:#08x} or white at some coverage",
                    q.color
                );
            }
            // The halo breathes its hue: a whole arc entry within
            // `HALO_HUE_SWING_ENTRIES` of the stop's anchor (owner, 2026-09-08).
            let anchor = SPECTRUM_ANCHOR_AT[spectrum_snap_index(0.62)] as i32;
            for h in &sc.halos {
                let ok =
                    (anchor - HALO_HUE_SWING_ENTRIES..=anchor + HALO_HUE_SWING_ENTRIES).any(|j| {
                        let rgb = SPECTRUM_LUT[j as usize];
                        (0..=255u16).any(|k| premul_rgb(rgb, k as u8) == h.color)
                    });
                assert!(ok, "halo colour {:#08x} left the stop's breath", h.color);
            }
            // …and both ends of the breath still snap to the star's own name.
            let name = |j: i32| spectrum_snap_index(j as f32 / (SPECTRUM_LUT_LEN - 1) as f32);
            assert_eq!(name(anchor - HALO_HUE_SWING_ENTRIES), name(anchor));
            assert_eq!(name(anchor + HALO_HUE_SWING_ENTRIES), name(anchor));
        }
        // D3: the last frame a star draws is still a visible point.
        let sc = frame_at(&mut sky, t0 + Duration::from_millis(last_lit), &c);
        assert!(
            lum(px(&sc.out, 200, 100)) >= 20,
            "the star greyed out before it was culled"
        );
        // …and one frame past the life it is gone, not left as a speck.
        let sc = frame_at(&mut sky, t0 + Duration::from_millis(life_ms + 1), &c);
        assert!(sc.out.is_empty() && sky.at_rest());
    }

    /// §3.2: nothing coloured is drawn below α 0.12. Near the cull at a
    /// twinkle trough the halo is culled while the white core (floor 0.20)
    /// still draws.
    #[test]
    fn a_halo_is_culled_below_the_chroma_floor() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        let mut s = star(StarClass::M1, StarLane::Strike, t0, spectrum_snap(0.2));
        // A seed whose twinkle trough meets the end of life, so the band
        // `α ∈ [0.20, 0.218)` is swept.
        let life = StarClass::M1.life_s(StarLane::Strike);
        s.seed = (0u32..)
            .find(|&k| {
                s.seed = k;
                s.twinkle(t0 + Duration::from_secs_f32(life - 0.004)) < 0.58
            })
            .expect("some seed puts the trough at the life's end");
        sky.sow(s);
        let life_ms = (life * 1000.0) as u64;
        let mut culled_while_core_drawn = false;
        for ms in 0..life_ms {
            let now = t0 + Duration::from_millis(ms);
            let sc = frame_at(&mut sky, now, &c);
            let s = sky.live_iter().next().expect("the hero is live");
            let a = s.envelope(now, false) * s.twinkle(now);
            if a < CHROMA_CULL_ALPHA {
                assert!(
                    sc.halos.is_empty(),
                    "a halo at α·env {a} is under the chroma floor"
                );
                if !sc.out.is_empty() {
                    culled_while_core_drawn = true;
                }
            }
        }
        assert!(
            culled_while_core_drawn,
            "the sweep never reached the band where the halo culls before the core"
        );
    }

    /// §5.5: warm and cool neighbours never blink together. The cool half
    /// takes `φ + π`, so two stars on one seed with opposite temperatures are
    /// exactly antiphase.
    #[test]
    fn warm_and_cool_neighbours_never_blink_together() {
        let t0 = Instant::now();
        for (warm_i, cool_i) in [(0usize, 3usize), (1, 4), (2, 6)] {
            let w = star(StarClass::M2, StarLane::Strike, t0, spectrum_stop(warm_i));
            let cl = star(StarClass::M2, StarLane::Strike, t0, spectrum_stop(cool_i));
            assert!(
                w.is_warm() && !cl.is_warm(),
                "stops {warm_i}/{cool_i} mis-classified"
            );
            let d = (cl.lum_phase() - w.lum_phase()).abs();
            assert!(
                (d - std::f32::consts::PI).abs() < 0.05,
                "phase difference {d} is not π"
            );
        }
        // The A-type white and the gold belong to neither half and take the
        // base phase — they have no neighbour they could beat against.
        let a = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
        let g = star(StarClass::M1, StarLane::Strike, t0, TINT_GOLD_RGB);
        assert!(a.is_warm() && g.is_warm());
        assert!((a.lum_phase() - g.lum_phase()).abs() < f32::EPSILON);
    }

    /// §5.3: gold is dealt, not inferred from yellow. A field star wearing the
    /// arc's yellow (gold's RGB) never glints; a dealt gold one does at its
    /// twinkle peak.
    #[test]
    fn gold_is_dealt_not_inferred_from_yellow() {
        let t0 = Instant::now();
        let yellow = spectrum_stop(2);
        assert_eq!(
            yellow, TINT_GOLD_RGB,
            "the fixture relies on yellow == gold RGB"
        );
        let mut field = star(StarClass::M1, StarLane::Strike, t0, yellow);
        field.gold = false;
        let gold = star(StarClass::M1, StarLane::Strike, t0, yellow);
        assert!(gold.gold);
        let life_ms = (StarClass::M1.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut gold_glinted = false;
        for ms in 0..life_ms {
            let now = t0 + Duration::from_millis(ms);
            assert!(
                !field.glinting(now),
                "a yellow field star glinted at {ms} ms"
            );
            gold_glinted |= gold.glinting(now);
        }
        assert!(gold_glinted, "a gold hero never glinted in its life");
        // The deal itself says which: over many seeds, the 15 % branch alone
        // returns gold, and every gold answer wears the gold RGB.
        let mut golds = 0;
        for seed in 0..1000u32 {
            let (tint, gold) = deal_tint(seed, 0.33);
            if gold {
                golds += 1;
                assert_eq!(tint, TINT_GOLD_RGB);
            } else if tint == TINT_GOLD_RGB {
                // yellow from the arc's own stop
                assert_eq!(spectrum_snap(0.33), yellow);
            }
        }
        assert!(
            (100..=200).contains(&golds),
            "the gold deal is not ~15 %: {golds}/1000"
        );
    }

    /// §5.4's rule clause: a light horizontal rule above the input line is not
    /// a dark sky. A TUI that draws its input under a full-width `─` rule must
    /// still get stars: the rule's stroke and the sky zone (`[0.60, 0.86]` of
    /// the cell) are disjoint, so the probe records the rule as its own class
    /// and the sky is born over it with no core on the stroke. Text above the
    /// line still darkens the sky (L4), as does a heavy rule, which can reach
    /// the zone on a square cell (owner, 2026-09-08).
    #[test]
    fn a_light_rule_over_the_typing_row_keeps_the_sky_alive() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let ch = g.ch as f32;
        // Forty keys on row 5 under a probed row 4, as the host feeds it.
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

        // 1. The TUI input box: a full-width light rule above the line.
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
        // …and the same census as a BLANK row: the rule costs the sky nothing.
        let blank = type_under(&row_of(" "));
        assert_eq!(
            sky.live(),
            blank.live(),
            "the sky over a light rule is not the sky over a blank row"
        );

        // 2. A row of text above the line: L4, unchanged.
        assert_eq!(
            type_under(&row_of("x")).live(),
            0,
            "a row of text above the input line minted stars"
        );

        // 3. A heavy rule is text for this purpose.
        assert_eq!(
            type_under(&row_of("━")).live(),
            0,
            "a heavy rule above the input line minted stars"
        );
    }

    /// The probe's grains over a [`CellInk::Rule`]: the cell is inked
    /// ([`GlyphProbe::at`]), the sky is not ([`GlyphProbe::sky_at`]), and a
    /// pixel is inked exactly inside the stroke band ([`GlyphProbe::at_px`]).
    /// A glyph is inked at every grain, a blank at none, an unprobed row
    /// unknown at every grain.
    #[test]
    fn the_pixel_probe_answers_ink_only_inside_a_rules_stroke_band() {
        let g = geom();
        let mut probe = GlyphProbe::new();
        let mut row = vec![CellInk::Blank; 120];
        row[10] = CellInk::Rule;
        row[11] = CellInk::Glyph;
        probe.probe_row_ink(4, &row);
        // A window pixel `f` of the way down row 4.
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
        assert_eq!(
            probe.at_px(rx as i32, y_at(0.50), g),
            Some(true),
            "the stroke"
        );
        assert_eq!(
            probe.at_px(rx as i32, y_at(0.10), g),
            Some(false),
            "above the stroke"
        );
        assert_eq!(
            probe.at_px(rx as i32, y_at(0.80), g),
            Some(false),
            "the sky band"
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

        // The bool view is the class view with no rules in it.
        let mut bools = GlyphProbe::new();
        let mut occupied = [false; 120];
        occupied[11] = true;
        bools.probe_row(4, &occupied);
        for col in [10u16, 11, 12] {
            assert_eq!(bools.at(4, col, g.rows), Some(col == 11));
            assert_eq!(bools.sky_at(4, col, g.rows), Some(col == 11));
        }
        // Re-probing a row as bools forgets its rules (the slot is rebuilt).
        probe.probe_row(4, &occupied);
        assert_eq!(probe.at(4, 10, g.rows), Some(false));
        assert_eq!(probe.at_px(rx as i32, y_at(0.50), g), Some(false));
    }

    /// §5.4's rule clause on whole pixels: the stroke band is one set of
    /// pixels for the probe, the zones and the headroom, at every cell height
    /// 6..=48 ([`rule_stroke_px`]). Per height:
    ///
    /// 1. the pixel probe answers `Some(true)` exactly on the band's pixels of
    ///    a rule cell;
    /// 2. the renderer's stroke (`light = (min(cw, ch) + 4) / 8`, centred with
    ///    the odd pixel below, `aterm_render::procedural`) lies inside the
    ///    band on a 1:2 cell and a square one, from `ch` 7;
    /// 3. the `tall` zone's top pixel is the band's end and the `underline`
    ///    zone's bottom pixel lies above the band's start;
    /// 4. every seeded birth under a rule row is clear by the probe, in both
    ///    spellings;
    /// 5. the headroom under the rule is exactly the pixels from the birth to
    ///    the band's end, unbounded above the band's start, and the pixel it
    ///    holds a flight at is clear;
    /// 6. the strike's whole-life lift is shorter than the exhaust's rise, so
    ///    no rise out of the `underline` zone reaches the row above's stroke.
    #[test]
    fn the_stroke_band_is_one_set_of_pixels_for_the_probe_the_zones_and_the_headroom() {
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let tall = cfg(true);
        let mut under = cfg(true);
        under.ribbon_tall = false;
        let lift_ch =
            -sky_lift_px_per_s(CELL_1X_CH) * StarClass::M1.life_s(StarLane::Strike) / CELL_1X_CH;
        assert!(
            lift_ch <= EXHAUST_RISE_CH,
            "a hero's whole-life lift is {lift_ch:.3} ch, past the exhaust's rise \
             {EXHAUST_RISE_CH} the const assertion bounds"
        );
        for ch in 6usize..=48 {
            let cw = (ch / 2).max(3);
            let g = geom_cell(cw, ch);
            let chf = ch as f32;
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
            // 1. The probe, pixel for pixel, on both rows.
            for r in [4, 5] {
                for off in 0..ch as i32 {
                    assert_eq!(
                        sky.probe().at_px(x, row_top(r) + off, g),
                        Some(band.contains(&off)),
                        "ch {ch}: pixel {off} of row {r} against the band {band:?}"
                    );
                }
            }
            // 2. The renderer's stroke.
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
            // 3. The zones' edges, as `sky_birth_px` rounds them.
            let zone_top = (default_band_top(g, &tall, 5) - SKY_TOP_CH * chf).round() as i32;
            assert_eq!(
                zone_top - row_top(4),
                band.end,
                "ch {ch}: the tall zone's top pixel is not the band's end"
            );
            let zone_bot = (default_band_top(g, &under, 5) - SKY_BOTTOM_CH * chf).round() as i32;
            assert!(
                zone_bot - row_top(5) < band.start,
                "ch {ch}: the underline zone's bottom pixel {} is not above the band's start {}",
                zone_bot - row_top(5),
                band.start
            );
            // 4. The births.
            for (c, row, side) in [(&tall, 4, "tall"), (&under, 5, "underline")] {
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
            // 5. The headroom, pixel by pixel down row 4.
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
                    // Born on the stroke: no headroom, and the rest is
                    // `settle`'s to nudge.
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

    /// The headroom under a rule reads every column the flight crosses: with
    /// rule cells before column 40 and blanks from 40 on, a grain born under
    /// a blank cell crosses the rule, so every rule cell under the flight is
    /// asked and the shortest headroom wins; blanks, glyphs and off-grid
    /// columns bound nothing. Pinned on a 15×28 cell at the tall zone's top
    /// pixel and two pixels under it.
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

    /// The rule class is exactly the light horizontal members of the Box
    /// Drawing block; a vertical arm, heavy or double stroke, block element,
    /// dash or underscore is a glyph, a space a blank, and a wide continuation
    /// a glyph.
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

    /// L4 / D16: no mark is ever drawn over a probed glyph (§5.4, §20.1). An
    /// occupied `row − 1` mints nothing, an unprobed `row − 1` mints nothing,
    /// and a free transient thrown at an inked cell is nudged clear or
    /// dropped.
    #[test]
    fn no_star_core_is_ever_born_over_a_probed_glyph() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();

        // 1. row − 1 UNKNOWN: nothing is born.
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

        // 2. row − 1 occupied: still nothing.
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

        // 3. row − 1 provably blank: the sky fills, and no core lands on ink.
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let inked: Vec<bool> = (0..120).map(|i| i % 2 == 0).collect();
        sky.probe_mut().probe_row(5, &inked);
        for k in 0..40u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        assert!(sky.live() > 0, "a provably blank sky band minted nothing");
        for s in sky.live_iter() {
            assert_ne!(
                sky.probe().at_px(s.x as i32, s.y as i32, geom()),
                Some(true),
                "a star core landed on a probed glyph at ({}, {})",
                s.x,
                s.y
            );
        }

        // 4. A free transient is cleared where it rests, and the cell it is
        //    thrown from (the caret's, ledger-exempt) is clear by exemption:
        //    a hop onto a word's first glyph still winks (§6.12, D17), its
        //    three born on the landing pixel and resting on the landing cell.
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

    /// §5.4 / D6 / D17: a thrown core is cleared where it rests: nudged within
    /// 2 px, walked back if the slot is law-fixed, dropped otherwise. Three
    /// hand-aimed throws exercise each answer of `settle`, then a fan boxed in
    /// by ink keeps exactly its hero and four m2 and drops every grain. The
    /// birth pixel is always the throw's origin and the motion ends on the
    /// cleared pixel.
    #[test]
    fn a_thrown_core_is_cleared_where_it_rests_nudged_walked_back_or_dropped() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom();
        // Cell (6, 40) spans x 360..369, y 148..166; its centre rounds to
        // (365, 157). Row 6 is inked except that cell; rows 5 and 7 are
        // unprobed.
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

        // Clear: a rest in open air (row 7) is sown as thrown.
        let mut s = thrown(7, 12);
        assert!(sky.settle(&mut s, false, &cx));
        assert_eq!(s.rest(), (372, 169), "a clear rest was moved");

        // Nudged: a rest one pixel inside the inked row, with air one pixel
        // below — the throw ends on that pixel, the birth stays put.
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

        // Deep in ink: a grain is dropped …
        let mut s = thrown(12, 0);
        assert!(
            !sky.settle(&mut s, false, &cx),
            "a grain whose rest is 12 px into ink was kept"
        );
        // … and a law-fixed slot walks back along its own throw to the first
        // clear pixel: the landing cell's edge, the caret's own.
        let mut s = thrown(12, 0);
        assert!(sky.settle(&mut s, true, &cx));
        assert_eq!((s.x, s.y), (365.0, 157.0), "the walk-back moved the birth");
        assert_eq!(
            s.rest(),
            (368, 157),
            "the walk-back did not stop at the first clear pixel"
        );
        assert_eq!(s.pos(done, false), s.rest());

        // A shed fragment takes no exemption: its station is nobody's cell.
        let mut s = thrown(0, 0);
        s.lane = StarLane::Shed;
        s.class = StarClass::M3;
        s.x = 374.0;
        assert!(
            !sky.settle(&mut s, false, &cx),
            "a fragment resting on the inked cell it was shed over was kept"
        );

        // A fan boxed in by ink on rows 5-7. A 23 px reach rests every star at
        // 12.65..24.15 px, beyond a 2 px nudge back into the landing cell or
        // out past rows 5/7, so every rest is ink: the grains go and the five
        // stay, each walked back to the landing cell's edge.
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

    /// §5.2: coverage over a probed-occupied or unknown cell is a third. The
    /// same transient star over a blank cell, an inked one and an unprobed
    /// row: the second and third peak at a third of the first.
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

    /// The luminance trace of one strike m1 at its centre pixel over its life,
    /// with `after_sow` applied to the probe between birth and the first frame.
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

    /// §5.2 / T5 / C3 / D16: a sky star keeps its centre when the probe
    /// changes under it. Its clearance is its birth gate, so its luminance
    /// trace is byte-identical whatever the probe says after birth:
    ///
    /// * its row forgotten (the three-row probe evicts row − 1 on Enter), where
    ///   a re-read would drop it to a third;
    /// * its own echoed glyph inked under `underline` (the sky band is inside
    ///   the typed cell), where a re-read would price it at a third from
    ///   frame 0.
    #[test]
    fn a_sky_star_keeps_its_centre_when_the_probe_forgets_its_row() {
        let t0 = Instant::now();
        let tall = cfg(true);
        let mut underline = cfg(true);
        underline.ribbon_tall = false;
        type Case<'a> = (&'a str, &'a Config, fn(&mut GlyphProbe));
        let cases: [Case<'_>; 2] = [
            ("its row forgotten", &tall, |probe| {
                // Three fresh rows evict rows 2-4 round-robin, the star's row 3
                // included, as an Enter down at row 21 would.
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
            assert!(
                (142..=144).contains(&peak),
                "{label}: the strike m1 did not reach §5.2's 143: {peak}"
            );
            assert_eq!(
                before, after,
                "{label}: a sky star's trace changed when the probe changed under it"
            );
        }
    }

    /// §5.6: the cap never refuses a newborn. Over the cap the oldest live
    /// star is put on its finish, the newborn always enters the pool, and the
    /// pool never exceeds its fixed allocation.
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

    /// §5.6 / §20.1: over the cap the oldest star finishes within 40 ms, the
    /// oldest rather than the one with least life left. The finish is a
    /// compressed `spend` retract: still on glass 10 ms after eviction, gone
    /// by 40.
    #[test]
    fn stars_over_the_cap_finish_oldest_first_within_forty_ms() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        // 40 stars, one per ms, each on its own pixel; #5 is a grain with the
        // least remaining life of the lot.
        for k in 0..STAR_CAP as u32 {
            let class = if k == 5 { StarClass::M3 } else { StarClass::M1 };
            let mut s = star(
                class,
                StarLane::Strike,
                t0 + Duration::from_millis(u64::from(k)),
                TINT_WHITE_RGB,
            );
            s.seed = k;
            s.x = 100.0 + 5.0 * k as f32;
            sky.sow(s);
        }
        let evict_at = t0 + Duration::from_millis(41);
        let mut newborn = star(StarClass::M1, StarLane::Strike, evict_at, TINT_WHITE_RGB);
        newborn.seed = 1000;
        newborn.x = 400.0;
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
        assert!(
            sky.live_iter().any(|s| s.seed == 1000),
            "the newborn was refused"
        );

        let lit = |sky: &mut Stardust, ms: u64, x: i32| {
            let sc = frame_at(sky, evict_at + Duration::from_millis(ms), &c);
            lum(px(&sc.out, x, 100)) > 0
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

    /// §5.8 / D5: heroes are earned, deterministically. A shifted capital and
    /// a `!` always mint an m1; an ordinary glyph only on the 1-in-12 deal.
    /// The producer is a pure function of its event sequence (§18).
    #[test]
    fn heroes_are_earned_deterministically() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let build = |class: TypedClass| {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            for k in 0..6u16 {
                sky.on_event(&typed(class), t0, &ctx(t0, &c, (5, 10 + k * 4)), &ribbon);
            }
            sky
        };
        let a = build(TypedClass::Capital);
        let b = build(TypedClass::Capital);
        let key = |s: &Stardust| -> Vec<(StarClass, u32, i32, i32)> {
            s.live_iter()
                .map(|p| (p.class, p.tint, p.x as i32, p.y as i32))
                .collect()
        };
        assert_eq!(key(&a), key(&b), "the same keys built two different skies");

        // Every capital is a hero: six typed, six m1.
        let heroes = a.live_iter().filter(|p| p.class == StarClass::M1).count();
        assert_eq!(heroes, 6, "six capitals earned {heroes} heroes");
        let bangs = build(TypedClass::Bang)
            .live_iter()
            .filter(|p| p.class == StarClass::M1)
            .count();
        assert_eq!(bangs, 6, "six `!` earned {bangs} heroes");

        // …and an ordinary glyph is nowhere near that rate.
        let plain_heroes = build(TypedClass::Glyph)
            .live_iter()
            .filter(|p| p.class == StarClass::M1)
            .count();
        assert!(
            plain_heroes < heroes,
            "an ordinary glyph earned as many heroes ({plain_heroes}) as a capital"
        );
    }

    /// §5.2 / §5.8: spacing and the row cap ration dealt heroes, never earned
    /// ones. Three capitals on adjacent cells are three heroes; a row of plain
    /// glyphs carries at most two live dealt heroes, at least three cells
    /// apart, and a dead hero frees its spacing.
    #[test]
    fn an_earned_hero_is_never_demoted_by_spacing_but_a_dealt_one_is() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();

        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        for col in 10..13u16 {
            sky.on_event(
                &typed(TypedClass::Capital),
                t0,
                &ctx(t0, &c, (5, col + 1)),
                &ribbon,
            );
        }
        assert_eq!(
            sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
            3,
            "adjacent capitals were demoted by the spacing law"
        );

        // A row whose first twenty cells deal at least three heroes, two of
        // them three cells apart, inside one tick and under the pool cap.
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

        // The spacing is the pool's: once every hero has died, a capital on
        // the same row is a hero again, with no memory to ration it.
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

    /// §5.6: the strike deal is a partition and no two sky stars share a
    /// pixel. A typed cell carries at most one strike star, and sky stars
    /// dealt near one cell are born on distinct device pixels.
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
                // an m1/m2 strike — the partition's own births (grains may be
                // the leading-edge second)
                *strike_per_cell.entry(col_of(s)).or_insert(0) += 1;
            }
        }
        assert!(
            strike_per_cell.values().all(|&n| n == 1),
            "a cell carries two m1/m2 strike stars: {strike_per_cell:?}"
        );
        // The cap binds long before the row ends: what is on glass is the
        // newest forty-odd, every one on its own pixel.
        assert!(
            sky.live() >= STAR_CAP,
            "the dense sky holds only {} stars",
            sky.live()
        );
    }

    /// §5.6: the second grain is priced on the honest spine. Same key, same
    /// cell: no leading-edge grain at `disp 0.1 / birth 0.9`, one at
    /// `disp 0.6`.
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

    /// §13 / D5: one token, one hero, one glint. Only an m1 spends; a dealt
    /// hero with no token becomes an m2 while an earned one stays a silent m1;
    /// a meteor empties the bucket.
    #[test]
    fn hero_births_and_glints_share_one_budget() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.budget.refill(t0);

        // Twelve capitals inside one tick: twelve heroes, three glints — the
        // bucket's depth — and every hero past the third born silent.
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

        // m2 and m3 never touch the bucket (§13).
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

        // A dealt hero with a dry bucket is born as an m2: over a whole row of
        // plain glyphs with the bucket empty, no m1 is born at all.
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

        // The meteor's edge empties the bucket; the next earned hero is still
        // an m1 and is silent.
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

    /// §13: the bucket keeps chiming as it refills. Six capitals 400 ms apart
    /// on six rows all chime (the bucket regains 1.6 between them); §13's
    /// ≤ 3 live is the synth's steal rule, not a spend refusal.
    #[test]
    fn the_bucket_keeps_chiming_as_it_refills() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        let mut glints = 0usize;
        for k in 0..6u16 {
            let row = 5 + 3 * k;
            let at = t0 + Duration::from_millis(400 * u64::from(k));
            sky.probe_mut().probe_row(i32::from(row) - 1, &[false; 120]);
            sky.budget.refill(at);
            sky.on_event(
                &typed(TypedClass::Capital),
                at,
                &ctx(at, &c, (row, 10)),
                &ribbon,
            );
            glints += sky.take_glints().count();
        }
        assert_eq!(glints, 6, "the bucket shut after {glints} of 6 heroes");
    }

    /// §13: the spend and the cue ride the same frame. One token buys one hero
    /// and one `SoundKind::Stardust` at its column on its birth tick, carrying
    /// the star's own scintillation rate.
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

    /// §5.4: a hero clipped off the glass spends nothing. On row 0 with no
    /// head band no star is born, no token spent and no glint cued; under a
    /// 40 px head band the same capital chimes.
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

    /// §5.2 / §5.6: stardust is a magnitude distribution (§20.1: 12 cps for
    /// 4 s, ≤ 40 live every frame, ≥ 40 % of live stars m2/m1, all three
    /// magnitudes present), measured on the live pool after the first second.
    #[test]
    fn stardust_is_a_magnitude_distribution_under_the_cap() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut seen = std::collections::BTreeSet::new();
        let (mut bright, mut total) = (0usize, 0usize);
        for k in 0..48u16 {
            let at = t0 + Duration::from_millis(u64::from(k) * 83);
            sky.budget.refill(at);
            sky.on_event(
                &typed(TypedClass::Glyph),
                at,
                &ctx(at, &c, (5, 10 + k)),
                &ribbon,
            );
            let mut sc = Scratch::default();
            sky.emit(&ctx(at, &c, (5, 10 + k)), &mut sc.frame());
            assert!(sky.live() <= STAR_CAP, "the pool overran the cap at {k}");
            for s in sky.live_iter() {
                seen.insert(s.class);
                if k >= 12 {
                    total += 1;
                    bright += usize::from(s.class != StarClass::M3);
                }
            }
        }
        assert_eq!(seen.len(), 3, "one magnitude never appeared: {seen:?}");
        assert!(
            bright * 100 >= total * 40,
            "only {}% of the live sky is m2 or m1",
            bright * 100 / total.max(1)
        );
    }

    /// T2: the frame that echoes the key already shows its field star. Dealt
    /// on a sub-millisecond echo frame, it is drawn on that frame (its birth
    /// envelope opens from [`BIRTH_EDGE_FLOOR`], above the cull) and reaches
    /// full when the 18 ms `edge-in` does.
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
        // The floor is the birth's: a strike star never reads it (born at
        // full inside its hold) and a finish is unfloored.
        let strike = star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB);
        assert!((strike.envelope(echo, false) - 1.0).abs() < 1e-6);
        let mut finishing = field;
        finishing.finish_by(t0, 0.040);
        let late = t0 + Duration::from_millis(30);
        assert!(
            finishing.envelope(late, false) < STAR_CULL_ALPHA,
            "a finish read the birth floor"
        );
    }

    /// §5.6: a field star rides its cell, not the grain's life. Under a hand
    /// that keeps typing on its row (a Space every 83 ms, re-lighting the
    /// field), a field grain is at full at 500 ms and 1 s, melts on the cell's
    /// curve, and is gone at [`FIELD_LIFE_S`]. With no key after its birth it
    /// goes out on the field fade instead: full through [`FIELD_HOLD_MS`],
    /// under half by 700 ms, gone at [`field_fade_life_s`].
    #[test]
    fn a_field_star_rides_its_cell_s_envelope_not_the_grain_s_life() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(
            (StarClass::M3.life_s(StarLane::Strike) * 1000.0) as u64,
            225,
            "the strike grain's published life"
        );
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

        // -- the hand keeps typing on row 3 (the fixture's sky) -----------
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
        // The cell's melt runs over the last `EXPIRY_MELT_SHARE` of the
        // horizon.
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
        // The field rides the ribbon's grace horizon (the phrase rest).
        assert_eq!(
            horizon, 1690,
            "the swoosh horizon is the rest plus the swoosh"
        );
        type_until(&mut sky, horizon);
        let sc = frame_at(&mut sky, at(horizon + 1), &c);
        // The stars are gone; the aurora is the ribbon's light and leaves
        // with the ribbon (`the_aurora_leaves_with_the_ribbon`).
        assert!(
            sc.out.is_empty() && sky.live() == 0,
            "a field star outlived the exit swoosh"
        );

        // -- the hand stops at the grain's birth --------------------------
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

    /// The field is dark before the retract moves: `H + 2.32·τ` of
    /// [`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`] ends at or before
    /// [`RETRACT_START_S`], and the envelope is under half by 700 ms after the
    /// last key, while still inside the fade.
    #[test]
    fn the_field_is_dark_before_the_retract_moves() {
        let life = field_fade_life_s();
        assert!(
            life <= RETRACT_START_S && life > 0.7,
            "the field fade ends at {life} s against a retract that starts at {RETRACT_START_S} s"
        );
        let (hold, tau) = (FIELD_HOLD_MS / 1000.0, FIELD_TAU_MS / 1000.0);
        let a = |s: f32| half_life(s, hold, tau);
        assert!(
            (a(hold) - 1.0).abs() < 1e-6,
            "the hold does not reach its own end at full"
        );
        assert!(
            a(0.7) <= 0.5,
            "a field star is at {} of its peak 700 ms after the last key",
            a(0.7)
        );
        assert!(
            a(life) < STAR_CULL_ALPHA + 1e-3 && a(life - 0.01) >= STAR_CULL_ALPHA,
            "the fade's cull ({}) is not where its life says ({life} s)",
            a(life)
        );
    }

    /// §5.3: a typing sky is not monochrome. Forty keys at 12 cps over a blank
    /// sky, the field walking all seven stops; each live star is rendered
    /// alone and its chroma read as the largest max − min channel gap among
    /// its tinted quads and halos. Two clauses:
    ///
    /// 1. birth: ≥ 60 % of stars show chroma ≥ 24 on their first full frame
    ///    (20 ms in). Grains in indigo or violet cannot reach 24;
    /// 2. steady state (keys ≥ 20): chromatic stars average ≥ 40 % of the sky
    ///    and never fall under a quarter of it on any key frame (≈ 46 %
    ///    expected under the alpha-only fade).
    #[test]
    fn a_typing_sky_is_not_monochrome() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let chroma_of = |s: &Star, now: Instant| -> u32 {
            let mut one = Stardust::new();
            one.sow(*s);
            let sc = frame_at(&mut one, now, &c);
            sc.out
                .iter()
                .map(|q| chroma(q.color))
                .chain(sc.halos.iter().map(|h| chroma(h.color)))
                .max()
                .unwrap_or(0)
        };
        let (mut born, mut born_chromatic) = (0usize, 0usize);
        let (mut steady, mut steady_chromatic, mut steady_floor) = (0usize, 0usize, 1.0f32);
        for k in 0..40u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            // The field walks the whole arc over the run, so every stop —
            // the dim indigo and violet included — is dealt.
            cx.caret_t = f32::from(k) / 40.0;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            let (mut live, mut live_chromatic) = (0usize, 0usize);
            // The sky's own stars: the exhaust (§33) is transient-priced and
            // stands under this threshold like an erase-lane grain.
            for s in sky.live_iter().filter(|s| s.lane.is_sky()) {
                if s.born == at {
                    let full = chroma_of(s, at + Duration::from_millis(20));
                    born += 1;
                    born_chromatic += usize::from(full >= 24);
                }
                live += 1;
                live_chromatic += usize::from(chroma_of(s, at) >= 24);
            }
            if k >= 20 && live > 0 {
                steady += live;
                steady_chromatic += live_chromatic;
                steady_floor = steady_floor.min(live_chromatic as f32 / live as f32);
            }
        }
        assert!(born >= 40, "forty keys bore only {born} stars");
        assert!(
            born_chromatic * 100 >= born * 60,
            "only {born_chromatic} of {born} stars were born visibly tinted"
        );
        assert!(
            steady_chromatic * 100 >= steady * 40,
            "only {steady_chromatic} of {steady} live star-frames are chromatic — the sky reads grey"
        );
        assert!(
            steady_floor >= 0.25,
            "a steady-state frame fell to {steady_floor} chromatic — the sky reads grey"
        );
    }

    /// §5.6: a 12 cps sky holds about twenty stars (cap 40; §18: "~120
    /// quads, 6 halos"). Forty-eight keys at exactly 12 cps on the engine's
    /// spine; steady state from key 24 (past the 1.54 s field horizon, spine
    /// ≥ 0.8).
    ///
    /// With the hot field deal the run measures ~22 live. The steady mean must
    /// lie in `12..=40` (12.1 being the cold-spine arithmetic), no frame under
    /// eight or over the cap, and a typical frame's quads and haloed stars
    /// (veils budgeted apart) inside the budgets.
    #[test]
    fn a_twelve_cps_sky_holds_about_twenty_stars() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let (mut live, mut quads, mut haloed) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..48u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            let n = sky.live_iter().filter(|s| s.finish.is_none()).count();
            assert!(n <= STAR_CAP, "{n} live at key {k} is over the cap");
            if k >= 24 {
                live.push(n);
                quads.push(sc.out.len());
                // Star halos are round; the aurora's veils are the wide
                // ellipses (`rx != ry`) and are budgeted on their own.
                let stars: std::collections::BTreeSet<(u16, u16)> = sc
                    .halos
                    .iter()
                    .filter(|h| h.rx == h.ry)
                    .map(|h| (h.cx, h.cy))
                    .collect();
                haloed.push(stars.len());
            }
        }
        let mean = |v: &[usize]| v.iter().sum::<usize>() / v.len().max(1);
        let (m, lo, hi) = (
            mean(&live),
            live.iter().copied().min().unwrap_or(0),
            live.iter().copied().max().unwrap_or(0),
        );
        assert!(
            (12..=40).contains(&m),
            "a 12 cps sky holds {m} stars on average (min {lo}, max {hi}), not about twenty"
        );
        assert!(lo >= 8, "a 12 cps sky thinned to {lo} stars");
        let (q, h) = (mean(&quads), mean(&haloed));
        eprintln!("12 cps sky: {m} live (min {lo}, max {hi}), {q} quads, {h} haloed stars");
        assert!(
            (60..=STARDUST_QUAD_BUDGET).contains(&q) && (3..=STARDUST_HALO_BUDGET).contains(&h),
            "a typical 12 cps frame is {q} quads / {h} haloed, not §18's ~120 / 6"
        );
    }

    /// §5.6 / §5.1: field stars dim with the ribbon they were born over; the
    /// sky goes out from the hand, not from the swoosh horizon.
    ///
    /// Twenty keys at 12 cps, then the hand stops. The brightest field star's
    /// centre is read at +0, +350, +700 and +1000 ms (one luminance period
    /// apart, so only the envelope moves the number): monotone
    /// non-increasing, at most half of +0 by +700, and the whole field culled
    /// by +1100.
    #[test]
    fn field_stars_dim_with_the_ribbon_they_were_born_over() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let keys = 20u16;
        for k in 0..keys {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
        }
        let last = key_at(t0, keys - 1);
        let caret = (5, 10 + keys);
        let mut sc = Scratch::default();
        sky.emit(&ctx(last, &c, caret), &mut sc.frame());
        let (star, a0) = sky
            .live_iter()
            .filter(|s| s.lane == StarLane::Field)
            .map(|s| {
                let (x, y) = s.pos(last, false);
                (*s, lum(px(&sc.out, x, y)))
            })
            .max_by_key(|&(_, l)| l)
            .expect("twenty keys at 12 cps lay a field");
        assert!(
            a0 > 0,
            "the brightest field star is dark on the last key's frame"
        );
        let at_px = star.pos(last, false);
        let centre_at = |sky: &mut Stardust, ms: u64| -> u32 {
            let now = last + Duration::from_millis(ms);
            let mut sc = Scratch::default();
            sky.emit(&ctx(now, &c, caret), &mut sc.frame());
            lum(px(&sc.out, at_px.0, at_px.1))
        };
        let (a350, a700, a1000) = (
            centre_at(&mut sky, 350),
            centre_at(&mut sky, 700),
            centre_at(&mut sky, 1000),
        );
        assert!(
            a0 >= a350 && a350 >= a700 && a700 >= a1000,
            "the field star brightened after the hand stopped: {a0} → {a350} → {a700} → {a1000}"
        );
        assert!(
            a700 * 2 <= a0,
            "the field star is still at {a700}/{a0} of its last-key brightness 700 ms after the last key"
        );
        let env = |ms: u64| star.envelope(last + Duration::from_millis(ms), false);
        assert!(
            env(700) <= 0.5 * env(0),
            "the field envelope is {} of its last-key value at +700 ms",
            env(700) / env(0)
        );
        let gone = last + Duration::from_millis(1100);
        let mut sc = Scratch::default();
        sky.emit(&ctx(gone, &c, caret), &mut sc.frame());
        let left = sky
            .live_iter()
            .filter(|s| s.lane == StarLane::Field)
            .count();
        assert_eq!(
            left, 0,
            "{left} field stars are still in the pool 1.1 s after the last key"
        );
        assert_eq!(
            lum(px(&sc.out, at_px.0, at_px.1)),
            0,
            "the field star is still on glass 1.1 s after the last key"
        );
    }

    /// §5.6: a field star is pinned and finishes with its cell. Born with no
    /// lift; when a Backspace retracts its cell it goes on the cell's 240 ms
    /// retract.
    #[test]
    fn a_field_star_is_pinned_and_finishes_with_its_cell() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        // A cell whose hash deals a field m2 (the longer-lived one, so the
        // retract finish is what ends it, not its own life).
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

        // Backspace over it at +50 ms: on the retract by +200 ms it is gone,
        // 1.3 s before its own swoosh-bounded field life would have ended.
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

    /// §8.2: focus loss embers the sky beside the ribbon: every star is on
    /// glass a frame after the blur, dimmer at 100 ms, gone by 300 ms.
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

    /// T6: idle → zero (§18, §20.1). No events, no light, no cadence, no
    /// deadline; after the longest class life the pool empties itself.
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

    /// §3.3 / §5.2: every mark forks to source-over on light (§20.1). No
    /// additive quad or halo survives; every light alpha is within 236 and
    /// every arm lay within 190; no lay below peak 96; a grain is one round
    /// dot.
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
                assert_eq!(sc.halos.len(), round, "a light grain is not one round dot");
                let dots: std::collections::BTreeSet<_> =
                    sc.halos.iter().map(|h| (h.cx, h.cy)).collect();
                assert_eq!(dots.len(), 1);
            } else {
                assert!(sc.halos.len() > round, "a light {class:?} has no arms");
            }
        }
    }

    /// §5.2: counts × 0.6 on light in every population, never below the
    /// law-fixed slots: a fan keeps its hero and four m2, a mini-fan its m2, a
    /// Backspace its m2.
    #[test]
    fn light_thins_every_thrown_population_but_never_a_law_fixed_slot() {
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let census = |dark: bool| -> (usize, usize, usize, usize) {
            let c = cfg(dark);
            let cx = ctx(t0, &c, (5, 40));
            let mut sky = Stardust::new();
            sky.sow_fan(
                t0,
                FanSow {
                    at_px: (300.0, 120.0),
                    reach: 12.0,
                    walk: LandingWalk::free(0.4),
                    seed: 11,
                    n: 18,
                    hero: true,
                },
                &cx,
            );
            let fan_m3 = sky.live_iter().filter(|s| s.class == StarClass::M3).count();
            let fan_fixed = sky.live_iter().filter(|s| s.class != StarClass::M3).count();
            let mut sky = Stardust::new();
            for k in 0..8u32 {
                sky.sow_mini_fan(t0, (300.0 + 20.0 * k as f32, 120.0), k, &cx);
            }
            let mini = sky.live();
            let mut sky = Stardust::new();
            for k in 0..8u16 {
                sky.on_event(&Event::Erase, t0, &ctx(t0, &c, (5, 10 + k)), &ribbon);
            }
            (fan_m3, fan_fixed, mini, sky.live())
        };
        let dark = census(true);
        let light = census(false);
        assert_eq!(dark.1, 5, "the dark fan is not hero + 4 m2");
        assert_eq!(light.1, 5, "light thinned a law-fixed fan slot");
        assert!(
            light.0 < dark.0,
            "light did not thin the fan's grains: {light:?} vs {dark:?}"
        );
        assert_eq!(dark.2, 24, "eight dark mini-fans are 24 stars");
        assert!((8..24).contains(&light.2), "light mini-fans: {}", light.2);
        assert_eq!(dark.3, 32, "eight dark Backspaces are 32 stars");
        assert!((8..32).contains(&light.3), "light Backspaces: {}", light.3);
    }

    /// §5.6 / §6.5: the meteor's own populations. A shed sputters `n` grains in
    /// the train's colour; a fan is one gold m1, four m2 and grains in throw
    /// order; an Enter landing is one m2 with grains and no hero (D8); a
    /// mini-fan is one m2 with two m3. None touches the glint bucket (§13).
    #[test]
    fn the_meteor_lanes_sow_their_published_class_census() {
        let c = cfg(true);
        let t0 = Instant::now();
        let cx = ctx(t0, &c, (5, 40));

        let mut sky = Stardust::new();
        sky.sow_shed(
            t0,
            ShedSow {
                at_px: (300.0, 120.0),
                v_head: (2.0, 0.0),
                tint_t: 0.4,
                seed: 7,
                n: 5,
            },
            &cx,
        );
        assert_eq!(sky.live(), 5);
        assert!(sky.live_iter().all(|s| s.class == StarClass::M3
            && s.lane == StarLane::Shed
            && s.tint == spectrum_snap(0.4)));
        assert_eq!(sky.take_glints().count(), 0, "a shed fragment chimed");

        // The fan's colours are
        // `the_fan_wears_the_band_s_stop_at_each_star_s_own_column`'s.
        let fan = |hero: bool, n: u8| {
            let mut sky = Stardust::new();
            sky.sow_fan(
                t0,
                FanSow {
                    at_px: (300.0, 120.0),
                    reach: 12.0,
                    walk: LandingWalk::free(0.4),
                    seed: 11,
                    n,
                    hero,
                },
                &cx,
            );
            sky
        };
        let mut sky = fan(true, 12);
        let m1 = sky.live_iter().filter(|s| s.class == StarClass::M1).count();
        let m2 = sky.live_iter().filter(|s| s.class == StarClass::M2).count();
        assert_eq!((m1, m2), (1, 4), "the fan is not 1 hero + 4 m2");
        assert!(
            sky.live_iter()
                .any(|s| s.class == StarClass::M1 && s.tint == TINT_GOLD_RGB && s.gold),
            "the fan's own hero is not gold"
        );
        assert_eq!(sky.take_glints().count(), 0, "the fan spent a typing token");

        let enter = fan(false, 7);
        assert_eq!(
            enter
                .live_iter()
                .filter(|s| s.class == StarClass::M1)
                .count(),
            0,
            "an Enter landing has a hero"
        );
        assert_eq!(
            enter
                .live_iter()
                .filter(|s| s.class == StarClass::M2)
                .count(),
            1,
            "an Enter landing is not ONE m2"
        );
        assert_eq!(
            enter
                .live_iter()
                .filter(|s| s.class == StarClass::M3)
                .count(),
            6
        );

        let mut sky = Stardust::new();
        sky.sow_mini_fan(t0, (300.0, 120.0), 5, &cx);
        assert_eq!(sky.live(), 3);
        assert_eq!(
            sky.live_iter().filter(|s| s.class == StarClass::M2).count(),
            1
        );
        assert_eq!(
            sky.live_iter().filter(|s| s.class == StarClass::M3).count(),
            2
        );
    }

    /// §6.5 layer 11 / §6.12: at `FAN_THROW_MS` every fan star sits at
    /// `reach·(0.55..1.05)` from the landing (three-quarters of the way at
    /// half the window); at `MINI_FAN_THROW_MS` every mini-fan star is 1-2 px
    /// out.
    #[test]
    fn a_fan_reaches_its_reach_and_a_mini_fan_hops_one_or_two_pixels() {
        let c = cfg(true);
        let t0 = Instant::now();
        let cx = ctx(t0, &c, (5, 40));
        let origin = (300, 120);
        let mut sky = Stardust::new();
        sky.sow_fan(
            t0,
            FanSow {
                at_px: (300.0, 120.0),
                reach: 12.0,
                walk: LandingWalk::free(0.4),
                seed: 11,
                n: 12,
                hero: true,
            },
            &cx,
        );
        let t_full = t0 + Duration::from_millis(FAN_THROW_MS as u64);
        let t_half = t0 + Duration::from_millis((FAN_THROW_MS / 2.0) as u64);
        for s in sky.live_iter() {
            let d = dist(s.pos(t_full, false), origin);
            assert!(
                (12.0 * FAN_THROW_JITTER_MIN - 1.0..=12.0 * FAN_THROW_JITTER_MAX + 1.0)
                    .contains(&d),
                "a fan star sits {d} px out at the end of its window"
            );
            let h = dist(s.pos(t_half, false), origin);
            assert!(
                (h - 0.75 * d).abs() <= 1.5,
                "the throw is not an ease-out: {h} at T/2 vs {d}"
            );
            // …and pinned afterwards.
            assert_eq!(
                s.pos(t_full, false),
                s.pos(t_full + Duration::from_millis(100), false)
            );
        }
        let mut sky = Stardust::new();
        sky.sow_mini_fan(t0, (300.0, 120.0), 5, &cx);
        let t_full = t0 + Duration::from_millis(MINI_FAN_THROW_MS as u64);
        for s in sky.live_iter() {
            let d = dist(s.pos(t_full, false), origin);
            assert!(
                (1.0..=3.0).contains(&d),
                "a mini-fan star sits {d} px out — not 1-2 px"
            );
        }
    }

    /// A seed whose luminance phase (and so its arm, sharing `φ`) is at its
    /// trough on frame 0.
    fn trough_seed() -> u32 {
        (0u32..)
            .find(|&k| {
                (hash01(mix32(k ^ SALT_PHASE)) * std::f32::consts::TAU)
                    .sin()
                    .abs()
                    < 0.05
            })
            .expect("some seed puts the luminance clock at its trough")
    }

    /// §5.6 / §5.5: a star is born at full and scintillates once its hold is
    /// spent ([`Star::in_hold`]). A fan m2 born at the clocks' trough draws its
    /// published 80 centre and nominal 7-px body through its 30 ms hold; past
    /// the hold the clocks take over (centre toward `0.70·c`, arm toward 2).
    #[test]
    fn a_star_is_born_at_full_and_scintillates_once_its_hold_is_spent() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut first = sky();
        // A warm coloured tint: the base phase (so `trough_seed` is the star's
        // trough), and non-white stubs, so the body's extent is its own.
        let tint = spectrum_stop(1);
        let mut s = star(StarClass::M2, StarLane::Fan, t0, tint);
        s.seed = trough_seed();
        assert!(s.is_warm(), "the fixture's tint must take the base phase");
        assert!(s.twinkle(t0) < 0.58, "the fixture is not at its trough");
        first.sow(s);
        let hold_ms = HOLD_M2_TRANSIENT_MS as u64;
        // The body's extent on the centre row (its colour is `M2_BODY_TINT` of
        // the way to the tint), and the centre.
        let body = mix_rgb(TINT_WHITE_RGB, tint, M2_BODY_TINT);
        let read = |sc: &Scratch| -> (u32, i32) {
            let (mut lo, mut hi) = (i32::MAX, i32::MIN);
            for q in sc.out.iter().filter(|q| {
                i32::from(q.y) == 100 && q.color == premul_rgb(body, lum(q.color) as u8)
            }) {
                lo = lo.min(i32::from(q.x));
                hi = hi.max(i32::from(q.x) + i32::from(q.w));
            }
            (lum(px(&sc.out, 200, 100)), hi - lo)
        };
        let nominal = star_arm(geom().ch as f32, StarClass::M2.arm_ratio()).round() as i32;
        assert_eq!(nominal, 3, "the fixture's own ladder: m2 arm 2.52 → 3");
        for ms in [0, hold_ms / 2, hold_ms - 1] {
            let sc = frame_at(&mut first, t0 + Duration::from_millis(ms), &c);
            let (centre, extent) = read(&sc);
            assert!(
                (79..=81).contains(&centre),
                "at {ms} ms, inside the hold, the m2's centre is {centre}, not its published 80"
            );
            assert_eq!(
                extent,
                2 * nominal + 1,
                "at {ms} ms, inside the hold, the m2 is {extent} px across, not its nominal"
            );
        }
        let sc = frame_at(&mut first, t0 + Duration::from_millis(hold_ms + 1), &c);
        let (centre, _) = read(&sc);
        assert!(
            centre <= 70,
            "one frame past the hold the centre is still {centre}: the luminance clock never \
             took over"
        );
        // The arm clock may sit at its own peak one frame past the hold; within
        // one cycle (≤ 63 ms) it must dip below the nominal.
        let dipped = (hold_ms + 1..=hold_ms + 63).any(|ms| {
            read(&frame_at(&mut first, t0 + Duration::from_millis(ms), &c)).1 < 2 * nominal + 1
        });
        assert!(
            dipped,
            "the arm stayed at its nominal for a whole cycle past the hold: the arm clock never \
             took over"
        );
        // And the whole trace is a pure function of `(birth, seed, now)`: a
        // second sowing of the same star reads byte-identical at every ms.
        let mut again = sky();
        let mut s2 = star(StarClass::M2, StarLane::Fan, t0, tint);
        s2.seed = trough_seed();
        again.sow(s2);
        for ms in (0..(StarClass::M2.life_s(StarLane::Fan) * 1000.0) as u64).step_by(7) {
            let now = t0 + Duration::from_millis(ms);
            assert_eq!(
                frame_at(&mut first, now, &c).out,
                frame_at(&mut again, now, &c).out,
                "the hold made the star's trace non-deterministic at {ms} ms"
            );
        }
    }

    /// §6.5 layer 11: the five are clear of the caret cell before their hold is
    /// spent ([`FAN_THROW_MS`]); the host paints its opaque caret over the
    /// landing cell. Asked of the meteor's floor jump (8 cells,
    /// `reach = 2.55 ch`) at the m2's hold, over a dozen seeded landings.
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

    /// §5.6: an erase is a throw over 80 ms, then pinned: 2-4 px out by
    /// `ERASE_THROW_MS` and no further after.
    #[test]
    fn an_erase_throw_reaches_its_reach_by_eighty_ms_then_pins() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(5, &[false; 120]);
        sky.on_event(&Event::Erase, t0, &ctx(t0, &c, (5, 10)), &ribbon);
        assert_eq!(sky.live(), 4, "a Backspace is 3 m3 + 1 m2");
        let (ox, oy) = geom().cell_center(5, 10);
        let origin = (ox.round() as i32, oy.round() as i32);
        let t_full = t0 + Duration::from_millis(ERASE_THROW_MS as u64);
        let scale = px_scale(geom().ch as f32);
        for s in sky.live_iter() {
            let d = dist(s.pos(t_full, false), origin);
            assert!(
                (ERASE_REACH_MIN_PX * scale - 1.0..=ERASE_REACH_MAX_PX * scale + 1.0).contains(&d),
                "an erase star sits {d} px out at 80 ms"
            );
            assert_eq!(
                s.pos(t_full, false),
                s.pos(t_full + Duration::from_millis(60), false)
            );
            assert!(
                s.pos(t_full, false).1 <= origin.1,
                "an erase star dived into the line"
            );
        }
    }

    /// §5.6 / §8.2: a kill throws one star per three cells, cap eight, along
    /// the drained span. A Backspace stays 3 m3 + 1 m2.
    #[test]
    fn a_kill_throws_one_star_per_three_cells_capped_at_eight() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let kill = |cells: u16| {
            let mut sky = Stardust::new();
            sky.on_event(
                &Event::Kill {
                    cells,
                    scope: KillScope::Line,
                },
                t0,
                &ctx(t0, &c, (5, 10)),
                &ribbon,
            );
            sky
        };
        assert_eq!(kill(2).live(), 1);
        assert_eq!(kill(6).live(), 2);
        assert_eq!(kill(24).live(), 8);
        let big = kill(60);
        assert_eq!(big.live(), 8, "^U on 60 cells threw {} stars", big.live());
        let cols: std::collections::BTreeSet<i32> = big.live_iter().map(col_of).collect();
        assert!(cols.len() >= 6, "the kill's stars are clumped on {cols:?}");
        assert!(
            cols.iter().all(|&col| (10..=34).contains(&col)),
            "a kill star left the drained span: {cols:?}"
        );
        assert_eq!(
            big.live_iter().filter(|s| s.class == StarClass::M2).count(),
            2,
            "3 m3 : 1 m2"
        );
    }

    /// §18: two Backspaces at one cell throw different debris; every seed is
    /// hashed from `(row, col, born)`.
    #[test]
    fn two_backspaces_at_one_cell_throw_different_debris() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.on_event(&Event::Erase, t0, &ctx(t0, &c, (5, 10)), &ribbon);
        let first: std::collections::BTreeSet<u32> = sky.live_iter().map(|s| s.seed).collect();
        sky.on_event(&Event::Erase, t0, &ctx(t0, &c, (5, 10)), &ribbon);
        let both: std::collections::BTreeSet<u32> = sky.live_iter().map(|s| s.seed).collect();
        assert_eq!(first.len(), 4);
        assert_eq!(
            both.len(),
            8,
            "the second Backspace reused the first's seeds"
        );
    }

    /// §18: halos are budgeted per star, not per split. Seventeen m2s straddling
    /// a row boundary: sixteen carry an atmosphere (two splits each), the
    /// seventeenth none.
    #[test]
    fn halos_are_budgeted_per_star_not_per_split() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        for k in 0..17u32 {
            let mut s = star(StarClass::M2, StarLane::Strike, t0, spectrum_snap(0.1));
            s.seed = peak_seed();
            s.x = 60.0 + 20.0 * k as f32;
            s.y = 93.0; // rows 2 | 3 meet at y = 94
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

    /// §18: an over-budget frame thins every star's extremities, not whole
    /// stars. A full pool (forty heroes plus eight tail grains) asks ~900 quads
    /// against 284: every star keeps its centre pixel and the frame stays
    /// within budget.
    #[test]
    fn an_over_budget_frame_keeps_every_star_s_core() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        for k in 0..STAR_CAP as u32 {
            let mut s = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
            s.seed = peak_seed();
            s.x = 20.0 + 24.0 * k as f32;
            sky.sow(s);
        }
        for k in 0..STAR_FINISH_SLACK as u32 {
            let mut s = star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB);
            s.seed = peak_seed();
            s.x = 20.0 + 24.0 * k as f32;
            s.y = 160.0; // row 6, clear of the heroes' arms and stubs
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
                lum(px(&sc.out, 20 + 24 * k, 100)) > 0,
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

    /// §18: a grain's share is honoured push by push, core first. A share of
    /// one draws the core alone, two the core and one arm, five the whole
    /// cross; never a sixth quad, never an arm before the core.
    #[test]
    fn a_grain_s_share_is_honoured_push_by_push_core_first() {
        let c = cfg(true);
        let t0 = Instant::now();
        let sky = sky();
        let grain = star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB);
        let paint = Paint::of(&grain, sky.probe(), &ctx(t0, &c, (3, 10)))
            .expect("a newborn grain has something to draw");
        for share in 1..=M3_QUADS + 1 {
            let mut out = Vec::new();
            paint.draw_m3(geom(), &mut out, share);
            assert_eq!(
                out.len(),
                share.min(M3_QUADS),
                "a share of {share} drew {} quads",
                out.len()
            );
            let core = &out[0];
            assert_eq!(
                (
                    i32::from(core.x),
                    i32::from(core.y),
                    i32::from(core.w),
                    i32::from(core.h)
                ),
                (200, 100, 1, 1),
                "the first quad of a share of {share} is not the 1×1 core"
            );
        }
    }

    /// §6.11: reduced motion is static and takes the linear fade. Both clocks
    /// stop, the star does not move, and its alpha falls linearly over the
    /// last [`REDUCED_MOTION_FADE_MS`] of its life.
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
            // Static: the centre pixel never moves.
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

    /// Seam point 12: a scroll moves every star with the viewport and drops
    /// the ones that leave the grid; the probe is forgotten, because a stale
    /// row answered as blank would be a star over ink.
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

    /// Seam point 12, the band path: a viewport `[4..10]` sliding down one row
    /// carries the row-5 star and veil to row 6, drops the row-10 veil pushed
    /// past the band's edge, leaves the row-2 star (outside the band) alone,
    /// and forgets the probe. Then the pinned phase: the transcript `[0..3]`
    /// archives up and only the row-2 star rides; two more rows and it leaves
    /// through the top.
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

        // No geometry yet: stars fail closed.
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
                            // The rejected implementation uses the star's sky
                            // pixel as though it were the earning glyph row.
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

    // -- §18 cadence: the sky's clocks are solved, not polled ---------------

    /// The size clock's next step is solved in closed form (§18, §5.5): for
    /// every class and several seeded rates, phases and instants past the
    /// hold, [`Star::next_arm_step_s`] names the first instant [`arm_px`]
    /// changes, within 0.1 ms, never more than `1/(2f)` (62.5 ms) away.
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

    /// A gold star's glint toggle is solved in closed form (§5.2):
    /// [`Star::next_glint_toggle_s`] names the first instant
    /// [`Star::glinting`] flips, within 0.1 ms; a non-gold star names none.
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

    // -- the coloured sky and the aurora (owner, 2026-09-08) ---------------

    /// The example renderer's dark ground, `(17, 19, 24)` — the default dark
    /// theme the scanner reads.
    const GROUND: u32 = 0x0011_1318;

    /// The scanner's own floor for "this pixel is COLOUR": a channel spread
    /// of at least this…
    const SCAN_SPREAD_FLOOR: u32 = 40;

    /// …at a max channel of at least this.
    const SCAN_LIT_FLOOR: u32 = 60;

    /// Composite one frame's `out` quads and halos over the dark ground on a
    /// `w × h` canvas at `(x0, y0)`, with the CPU reference's arithmetic
    /// (`add_sat`, `halo_row_ny` / `halo_weight`).
    fn composite(sc: &Scratch, x0: i32, y0: i32, w: usize, h: usize) -> Vec<u32> {
        composite_over(sc, GROUND, x0, y0, w, h)
    }

    /// The shipped dark theme's ground, `#1A1B26`, as `cursor_glow`'s
    /// whole-frame gates use.
    const SHIPPED_GROUND: u32 = 0x001A_1B26;

    /// [`composite`] over a chosen ground.
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
                    *p = add_sat(*p, q.color);
                }
            }
        }
        for a in &sc.halos {
            assert_eq!(a.mode, HaloMode::Add, "a dark frame emitted an Over halo");
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
                    let wt = halo_weight(x - i32::from(a.cx), ny, rx2).min(255);
                    let (cx, cy) = (x - x0, y - y0);
                    if wt == 0 || cx < 0 || cy < 0 || cx as usize >= w || cy as usize >= h {
                        continue;
                    }
                    let p = &mut canvas[cy as usize * w + cx as usize];
                    *p = add_sat(*p, premul_rgb(a.color, wt as u8));
                }
            }
        }
        canvas
    }

    /// The aurora's halos on a dark frame — the wide ellipses; a star's halo
    /// is round.
    fn veils(sc: &Scratch) -> Vec<RainHalo> {
        sc.halos.iter().filter(|h| h.rx != h.ry).copied().collect()
    }

    /// Stardust is coloured on the glass, not white with a tint. Forty keys at
    /// 12 cps, the field walking the arc; from key 20 every frame is
    /// composited over the dark ground and read by the scanner's rule (a star
    /// pixel has max channel ≥ 60; it is colour if its spread is ≥ 40). At
    /// least 60 % of star pixels are colour (owner, 2026-09-08).
    #[test]
    fn stardust_is_coloured_on_the_glass_not_white_with_a_tint() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let (mut lit, mut coloured) = (0usize, 0usize);
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
            if k < 20 {
                continue;
            }
            // Rows 3-5 of the fixture: the sky band, the line, and below.
            let canvas = composite(&sc, 0, 94, 1080, 54);
            for &p in &canvas {
                if lum(p) >= SCAN_LIT_FLOOR {
                    lit += 1;
                    coloured += usize::from(chroma(p) >= SCAN_SPREAD_FLOOR);
                }
            }
        }
        eprintln!(
            "scanner: {coloured} of {lit} star pixels are colour ({}%)",
            coloured * 100 / lit.max(1)
        );
        assert!(lit >= 200, "only {lit} star pixels over twenty frames");
        assert!(
            coloured * 100 >= lit * 60,
            "only {coloured} of {lit} star pixels ({}%) are colour by the scanner's rule — \
             the sky reads as white dust with a tint",
            coloured * 100 / lit.max(1)
        );
    }

    /// No star outshines the caret's floor share on the shipped ground
    /// (`cursor_glow` §8 d; [`STAR_LIGHT_CEIL`]).
    ///
    /// Every class in both lanes, every tint (seven stops, gold, white), at the
    /// default intensity and at full, one star at a time over `#1A1B26`: its
    /// whole life composited with its halo, and no pixel's relative luminance
    /// past `72/255`. The white hero at full intensity sits within one level
    /// of the ceiling, so the law binds.
    ///
    /// One star at a time: stacks on one cell are priced star by star over the
    /// sky laid before ([`sky_ground`]).
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
                        // A 60 px window on the star at (200, 100).
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
        eprintln!(
            "the brightest star pixel over the shipped ground: Y {:.1} of the ceiling {ceil:.1} — {}",
            worst.0, worst.1
        );
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

    /// The closer dims a star that stacks over the ceiling away from its
    /// centre, by one factor, and leaves one that fits alone
    /// ([`settle_under_ceiling`]).
    ///
    /// A synthetic star: a lawful white centre and a full-request yellow stub
    /// beside a yellow halo's ring over the shipped ground. Afterwards every
    /// watched pixel is under the ceiling, the stub pixel within two levels of
    /// it, every own lay scaled by the same factor, the halo still the pure
    /// stop, and an earlier quad untouched. A fitting star is byte-identical.
    #[test]
    fn the_closer_dims_a_star_by_one_factor_until_its_arms_fit_and_leaves_one_that_fits() {
        let g = geom();
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let y = |p: u32| relative_luminance(p) * 255.0;
        let yellow = spectrum_stop(2);
        let mut sc = Scratch::default();
        // An earlier star's quad, off to the side: the closer's slice starts
        // after it.
        push_fx_rect(&mut sc.out, g, 300, 100, 1, 1, 0x0030_3030);
        let q0 = sc.out.len();
        // A priced centre (white 40 under a 61 yellow halo is Y 52), and the
        // arm's first pixel as a hero lays it: taper at `0.87 · 40`, tint stub
        // at 61, halo ring at 0.92.
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
        // One piece per text row the 15 px halo spans.
        assert!(sc.halos.len() > h0, "no halo was laid");
        assert!(
            sc.halos[h0..].iter().all(|h| h.color == sc.halos[h0].color),
            "the pieces of one halo carry one colour"
        );
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
        for h in &sc.halos[h0..] {
            assert_eq!(
                premul_rgb(yellow, lum(h.color) as u8),
                h.color,
                "the halo is no longer the pure stop"
            );
        }
        assert_eq!(sc.out[0].color, 0x0030_3030, "the earlier quad moved");

        // A star that fits is left byte-identical.
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

    /// The caret law spends colour first and white last on a star's centre
    /// ([`price_centre`]):
    ///
    /// * an A-type hero (`c = 61 · 0.7`, halo asked 43) keeps its body and gets
    ///   16 levels of halo, landing at `Y 71.8`;
    /// * over black at `143` the white hero keeps its centre and its halo gets
    ///   the one level left;
    /// * each of the seven stops at full intensity over the shipped ground
    ///   keeps its halo's whole 61 and a body the ceiling can carry, the
    ///   centre at or under `Y 72` on a max channel grey could not reach;
    /// * a ground already over the ceiling leaves the request as it is.
    #[test]
    fn the_caret_law_spends_colour_first_and_white_last_on_a_star_s_centre() {
        let y = |p: u32| relative_luminance(p) * 255.0;
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let white = TINT_WHITE_RGB;
        assert_eq!(fit_byte(255, |k| k <= 100), 100);
        assert_eq!(fit_byte(50, |_| true), 50);
        assert_eq!(fit_byte(50, |k| k == 0), 0);
        assert_eq!(fit_byte(50, |_| false), 0);

        let (cov, _, peak) =
            price_centre(SHIPPED_GROUND, true, (white, 43), (white, 0), (white, 43));
        assert_eq!((cov, peak), (43, 16), "the failing frame's A-type hero");
        let centre = add_sat(
            stacked_centre(SHIPPED_GROUND, white, 43),
            premul_rgb(white, 16),
        );
        assert!(
            y(centre) <= ceil && y(centre) > ceil - 1.0,
            "the A-type centre lands at Y {:.1}, not just under {ceil:.1}",
            y(centre)
        );
        let (cov, _, peak) = price_centre(0, true, (white, 61), (white, 0), (white, 61));
        assert_eq!((cov, peak), (61, 1), "the white hero over black");
        assert_eq!(lum(stacked_centre(0, white, 61)), 143);

        let grey_at_ceiling = 145;
        for i in 0..SPECTRUM_STOPS {
            let tint = spectrum_stop(i);
            let (cov, _, peak) =
                price_centre(SHIPPED_GROUND, false, (white, 61), (tint, 0), (tint, 61));
            assert_eq!(peak, 61, "stop {i} lost its halo's request");
            let centre = stacked_centre(add_sat(SHIPPED_GROUND, premul_rgb(tint, 61)), white, cov);
            eprintln!(
                "stop {i} {tint:#08x}: body cov {cov} of 61, centre #{centre:06X} — max channel {}, Y {:.1}",
                lum(centre),
                y(centre)
            );
            assert!(
                y(centre) <= ceil,
                "stop {i}: centre Y {:.1} over {ceil:.1}",
                y(centre)
            );
            assert!(
                cov >= 20 && lum(centre) > grey_at_ceiling,
                "stop {i}: body {cov}, max channel {} — the colour bought nothing",
                lum(centre)
            );
        }
        let bright = 0x00A0_A0A0;
        assert!(!under_ceil(bright));
        assert_eq!(
            price_centre(
                bright,
                false,
                (white, 61),
                (spectrum_stop(0), 17),
                (spectrum_stop(0), 61)
            ),
            (61, 17, 61),
            "a ground over the ceiling leaves the request as it is"
        );
    }

    /// A star is priced over the sky under it ([`sky_ground`]): the theme
    /// ground plus halos already laid (a veil's colour at its centre, nothing
    /// off its box), never an `Over` halo.
    #[test]
    fn a_star_is_priced_over_the_veils_and_halos_already_under_it() {
        let veil = RainHalo {
            row: 4,
            x: 90,
            y: 100,
            w: 9,
            h: 6,
            color: 0x0020_0008,
            cx: 94,
            cy: 106,
            rx: 18,
            ry: 6,
            mode: HaloMode::Add,
        };
        let over = RainHalo {
            mode: HaloMode::Over,
            ..veil
        };
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[], 94, 105),
            SHIPPED_GROUND
        );
        // One row above the veil's bottom edge on its axis: `ny = 256/36 = 7`,
        // `wt = (256 − 7)² >> 8 = 242`, `premul(#200008, 242) = #1E0008`.
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[veil], 94, 105),
            add_sat(SHIPPED_GROUND, 0x001E_0008)
        );
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[veil], 94, 99),
            SHIPPED_GROUND,
            "above the box"
        );
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[veil], 89, 105),
            SHIPPED_GROUND,
            "off the column"
        );
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[over], 94, 105),
            SHIPPED_GROUND,
            "an Over halo"
        );
        let stacked = sky_ground(SHIPPED_GROUND, &[], &[veil, veil], 94, 105);
        assert!(lum(stacked) > lum(sky_ground(SHIPPED_GROUND, &[], &[veil], 94, 105)));
        assert_eq!(
            sky_ground(0xFF1A_1B26, &[], &[], 0, 0),
            SHIPPED_GROUND,
            "the top byte is not a colour"
        );
        let quad = GlowQuad {
            row: 4,
            x: 93,
            y: 104,
            w: 3,
            h: 3,
            color: 0x0010_2030,
            alpha: 0,
            color2: 0x0010_2030,
            alpha2: 0,
        };
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[quad], &[], 94, 105),
            add_sat(SHIPPED_GROUND, 0x0010_2030),
            "an earlier star's quad"
        );
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[quad], &[], 96, 105),
            SHIPPED_GROUND,
            "off the quad"
        );
        assert_eq!(
            sky_ground(
                SHIPPED_GROUND,
                &[GlowQuad {
                    alpha: 255,
                    alpha2: 255,
                    ..quad
                }],
                &[],
                94,
                105
            ),
            SHIPPED_GROUND,
            "a source-over quad is not the sky's"
        );
    }

    /// The aurora rises with momentum and never enters a glyph row. Forty keys
    /// at 12 cps, red at the caret: cold lays no veil; at one second (spine
    /// 0.51) the newest veil is 12-18; at the last key (spine 1.0) 38-40
    /// (owner, 2026-09-08).
    /// Every veil lies wholly inside row 4, in its own column, never over a
    /// column whose row-4 cell is inked (cols 25-30), and the last frame
    /// carries at least four hues.
    #[test]
    fn the_aurora_rises_with_momentum_and_never_enters_a_glyph_row() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let run = |walk: bool| -> Vec<(u16, f32, Vec<RainHalo>)> {
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
                cx.caret_t = if walk { f32::from(k) / 40.0 } else { 0.0 };
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
        let red = run(false);
        assert!(red[0].2.is_empty(), "a cold key laid a veil");
        let peak = |vs: &[RainHalo]| vs.iter().map(|h| lum(h.color)).max().unwrap_or(0);
        let plain = peak(&red[12].2);
        let full = peak(&red[39].2);
        eprintln!(
            "aurora: {plain} at spine {:.2}, {full} at spine {:.2}; {} veils on the last frame",
            red[12].1,
            red[39].1,
            red[39].2.len()
        );
        assert!(
            (12..=18).contains(&plain),
            "at spine {} the veil is {plain}, not plain",
            red[12].1
        );
        assert!(
            (38..=40).contains(&full) && full > plain,
            "at spine {} the veil is {full} (plain was {plain}), not full",
            red[39].1
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
        let walked = run(true);
        let hues: std::collections::BTreeSet<u32> = walked[39].2.iter().map(|h| h.color).collect();
        assert!(
            hues.len() >= 4,
            "the veil wears {} colours over the walked field, not a rainbow",
            hues.len()
        );
    }

    /// The aurora leaves with the ribbon. Twenty hot keys, then the hand stops
    /// (owner, 2026-09-08):
    /// the veil holds 38-40 through the field hold (300 ms), is at half by
    /// 550, and is gone by 900 ms, before the retract (`RETRACT_START_S`). A
    /// Backspace puts the erased cells' veil on the 240 ms retract while
    /// earlier cells keep theirs.
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
        let (p0, p300, p550, p900) = (
            peak_at(&mut sky, at(0)),
            peak_at(&mut sky, at(300)),
            peak_at(&mut sky, at(550)),
            peak_at(&mut sky, at(900)),
        );
        eprintln!(
            "aurora after the last key: {p0} at +0, {p300} at +300, {p550} at +550, {p900} at +900 ms"
        );
        assert!((38..=40).contains(&p0), "the veil is {p0} on the last key");
        assert!(
            (38..=40).contains(&p300),
            "the veil is {p300} at the end of the hold"
        );
        assert!(
            (15..=25).contains(&p550),
            "the veil is {p550} at 550 ms, not about half"
        );
        assert_eq!(p900, 0, "the veil outlived the ribbon's grace");
        assert!(
            (RETRACT_START_S * 1000.0) as u64 >= 900,
            "the fixture's 900 ms is not before the retract"
        );
        assert!(sky.at_rest(), "the veil is still in the pool");
        assert!(sky.next_change_deadline(at(900)).is_none());

        // -- the erase arm --------------------------------------------------
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

    /// A hot hand deals every cell a field star ([`FIELD_DEAL_IN_HOT`]). Sixty
    /// keys on a blank sky at a fixed spine: cold (0.4) births per key are the
    /// strike deal plus half a field star, 1.33; hot (0.8) the second grain
    /// and a field star on every cell, 2.83. Counted as sky stars born on the
    /// key's edge (the exhaust is excluded) (owner, 2026-09-08).
    #[test]
    fn a_hot_hand_deals_every_cell_a_field_star() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        // Births per key, and the share of the last twenty laid cells that
        // carry a live FIELD-lane star (dealt or adopted) on the last key.
        let census = |disp: f32| -> (f32, f32, usize) {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let mut born = 0usize;
            for k in 0..60u16 {
                let at = key_at(t0, k);
                let mut cx = ctx(at, &c, (5, 10 + k));
                cx.disp = disp;
                cx.birth_disp = disp;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                born += sky
                    .live_iter()
                    .filter(|s| s.born == at && s.lane.is_sky())
                    .count();
            }
            let field: std::collections::BTreeSet<i32> = sky
                .live_iter()
                .filter(|s| s.lane == StarLane::Field)
                .map(col_of)
                .collect();
            // The last sixteen laid cells (1.33 s at 12 cps), inside the 1.54 s
            // field horizon.
            let held = (53..69).filter(|col| field.contains(col)).count();
            (born as f32 / 60.0, held as f32 / 16.0, sky.live())
        };
        let (cold, hot) = (census(0.4), census(0.8));
        eprintln!(
            "births per key: cold {:.2}, hot {:.2}; field-held cells of the last sixteen: cold {:.2}, hot {:.2}; live: cold {}, hot {}",
            cold.0, hot.0, cold.1, hot.1, cold.2, hot.2
        );
        assert!(
            (0.8..=1.0).contains(&cold.0),
            "a cold hand bears {} stars per key, not about 0.92",
            cold.0
        );
        assert!(
            (0.95..=1.05).contains(&hot.0),
            "a hot hand bears {} stars per key, not one per cell",
            hot.0
        );
        assert!(
            hot.1 >= 0.999,
            "a hot hand left {} of the last sixteen cells without a field star",
            1.0 - hot.1
        );
        assert!(
            cold.1 <= 0.75 && hot.1 > cold.1 + 0.2,
            "momentum bought no density: field-held {} → {}",
            cold.1,
            hot.1
        );
    }

    /// The combo ladder (§23): every [`LADDER_M2_KEYS`]th key of a run
    /// promotes its strike to an m2, every [`LADDER_WHITE_KEYS`]th takes a
    /// white m2, every [`LADDER_GOLD_KEYS`]th an earned gold m1.
    ///
    /// Over seventy keys the rungs land on every rung key, while the keys
    /// between still take the ordinary strike deal, showing the ladder is a
    /// payout and not a brighter deal.
    #[test]
    fn the_combo_ladder_pays_at_sixteen_thirty_two_and_sixty_four() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        // The brightest STRIKE star born on each key's own edge, by combo.
        /// `(class, tint, gold)` of the brightest strike star one key drew.
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

        // The control: the keys between the rungs are still the deal — some
        // of them a grain, some of them nothing at all.
        let plain = rung
            .iter()
            .filter(|(n, _)| !n.is_multiple_of(LADDER_M2_KEYS))
            .filter(|(_, s)| s.is_none_or(|(c, _, _)| c == StarClass::M3))
            .count();
        assert!(
            plain > 20,
            "only {plain} of the non-rung keys took a grain or nothing — the ladder is not a payout, the deal moved"
        );
        eprintln!(
            "ladder: 16 {:?}, 32 {:?}, 64 {:?}; {plain} of the 65 non-rung keys took a grain or nothing",
            paid(LADDER_M2_KEYS).0,
            paid(LADDER_WHITE_KEYS).0,
            paid(LADDER_GOLD_KEYS).0
        );
    }

    /// The sky opens ([`FIELD_SHARE_FLOW`]): an open theme deals every laid
    /// cell its grain and holds that share through a dip below
    /// [`FIELD_HOT_DISP`]. Measured cold, dipping and flowing on sixty keys.
    #[test]
    fn an_open_theme_holds_the_sky_open_through_a_dip() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        // Births per key, the share of the last sixteen laid cells holding a
        // field-lane star (dealt or adopted), and the live pool.
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
        // The dip: a hand whose honest spine has breathed under the step's
        // 0.7 but whose run has not broken.
        let dip = FIELD_HOT_DISP - 0.05;
        let cold = census(dip, 0.0);
        let flowing = census(dip, 1.0);
        let hot = census(0.9, 0.0);
        eprintln!(
            "at disp {dip}: cold {:.2} stars/key, field-held {:.2}, live {}; flowing {:.2}, {:.2}, live {}. The step at disp 0.9: {:.2}, {:.2}, live {}",
            cold.0, cold.1, cold.2, flowing.0, flowing.1, flowing.2, hot.0, hot.1, hot.2
        );
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

    /// One sky star per cell, and no pile outshines the caret
    /// ([`STRIKE_REACH_CELLS`], [`Stardust::sky_star_live`],
    /// [`Stardust::adopt_field`]). Forty keys at 12 cps over the shipped
    /// ground, composited on every echo frame: no two live sky stars share a
    /// cell, and no sky pixel exceeds `72/255`.
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
            // Rows 3-5 of the fixture: the sky band, the line, and below.
            let peak = composite_over(&sc, SHIPPED_GROUND, 0, 94, 1080, 54)
                .iter()
                .map(|&p| relative_luminance(p) * 255.0)
                .fold(0.0, f32::max);
            over += usize::from(peak > ceil);
            worst = worst.max(peak);
        }
        eprintln!(
            "pile census: {over} of 40 frames over the ceiling {ceil:.1}, worst Y {worst:.1}; piles (key, col, stars): {piles:?}"
        );
        assert!(
            piles.is_empty(),
            "a cell carries more than one sky star: {piles:?}"
        );
        assert!(
            over == 0,
            "{over} of 40 frames reach past the caret law's {ceil:.1} — worst Y {worst:.1}"
        );
    }

    /// The sky at 2× is the sky at 1× twice the size ([`CELL_1X_CH`],
    /// [`px_scale`]). The 12 cps fixture at 7×14 and 15×28, last twenty echo
    /// frames composited: the halo-lit star-pixel area is four times larger
    /// at 2× within the bound (the whole lit area ~3.3×, since 1 px hairlines
    /// double only in length), every star halo's radius doubles, and the
    /// veil box scales with the cell (`15/7` across).
    #[test]
    fn the_sky_at_two_x_is_the_sky_at_one_x_twice_the_size() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        struct Sky {
            lit: usize,
            lit_halo: usize,
            halo_r: f32,
            veil_rx: f32,
            veil_ry: f32,
        }
        let measure = |g: Geom| -> Sky {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let (mut lit, mut lit_halo) = (0usize, 0usize);
            let (mut halo_r, mut veil_rx, mut veil_ry) = (Vec::new(), Vec::new(), Vec::new());
            for k in 0..40u16 {
                let at = key_at(t0, k);
                let mut cx = ctx_in(g, at, &c, (5, 10 + k));
                cx.disp = disp_at_12cps(k);
                cx.birth_disp = cx.disp;
                cx.caret_t = f32::from(k) / 40.0;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                let mut sc = Scratch::default();
                sky.emit(&cx, &mut sc.frame());
                if k < 20 {
                    continue;
                }
                let y0 = i32::from(g.origin_y) + 3 * g.ch as i32;
                let count = |sc: &Scratch| {
                    composite(sc, 0, y0, g.cols * g.cw, 3 * g.ch)
                        .iter()
                        .filter(|&&p| lum(p) >= SCAN_LIT_FLOOR)
                        .count()
                };
                lit += count(&sc);
                // The halos alone — the sky's area, apart from the hairlines.
                let halos_only = Scratch {
                    halos: sc.halos.clone(),
                    ..Scratch::default()
                };
                lit_halo += count(&halos_only);
                for h in &sc.halos {
                    if h.rx == h.ry {
                        halo_r.push(f32::from(h.rx));
                    } else {
                        veil_rx.push(f32::from(h.rx));
                        veil_ry.push(f32::from(h.ry));
                    }
                }
            }
            let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
            assert!(
                !halo_r.is_empty() && !veil_rx.is_empty(),
                "no halos at {}x{}",
                g.cw,
                g.ch
            );
            Sky {
                lit,
                lit_halo,
                halo_r: mean(&halo_r),
                veil_rx: mean(&veil_rx),
                veil_ry: mean(&veil_ry),
            }
        };
        let one = measure(geom_cell(7, 14));
        let two = measure(geom_cell(15, 28));
        let area = two.lit as f32 / one.lit.max(1) as f32;
        let area_halo = two.lit_halo as f32 / one.lit_halo.max(1) as f32;
        let halo = two.halo_r / one.halo_r;
        let (vx, vy) = (two.veil_rx / one.veil_rx, two.veil_ry / one.veil_ry);
        eprintln!(
            "1x: {} star px ({} halo-lit), halo r {:.2}, veil {:.1}x{:.1}; 2x: {} star px ({} halo-lit), halo r {:.2}, veil {:.1}x{:.1}; ratios: area {area:.2} (halos alone {area_halo:.2}), halo {halo:.2}, veil {vx:.2}x{vy:.2}",
            one.lit,
            one.lit_halo,
            one.halo_r,
            one.veil_rx,
            one.veil_ry,
            two.lit,
            two.lit_halo,
            two.halo_r,
            two.veil_rx,
            two.veil_ry
        );
        // The halos' area is four times; the whole lit area falls short by the
        // 1 px hairlines (`STAR_WAIST`, `star_waist_px`), which double in
        // length but not width.
        assert!(
            (3.4..=4.6).contains(&area_halo),
            "the 2x sky's halos light {area_halo:.2}x the pixels of the 1x sky's, not about four"
        );
        assert!(
            area >= 3.2,
            "the 2x sky has {area:.2}x the star pixels of the 1x sky — less than the hairlines alone can explain"
        );
        assert!(
            (1.8..=2.2).contains(&halo),
            "the 2x halos are {halo:.2}x the 1x radii, not twice"
        );
        assert!(
            (1.8..=2.2).contains(&vy) && (vx - 15.0 / 7.0).abs() <= 0.2,
            "the 2x veil box is {vx:.2}x wide and {vy:.2}x tall, not the cell's own 15/7 x 2"
        );
    }

    // -- 1d. the slipstream ---------------------------------------------------

    /// A hand at `cps` on a 15×28 cell: `keys` glyphs from column `col0` on
    /// row 5, the sky row (4) probed blank past its first `ink_to` columns,
    /// the spine held hot (0.9) so the cadence, not the birth gate, is what
    /// stills a stroll. Returns the sky and the last key's instant.
    fn stream_sky(
        cps: f32,
        keys: u16,
        col0: u16,
        ink_to: usize,
        c: &Config,
        t0: Instant,
    ) -> (Stardust, Instant) {
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        let mut occupied = [false; 120];
        for slot in occupied.iter_mut().take(ink_to) {
            *slot = true;
        }
        sky.probe_mut().probe_row(4, &occupied);
        let mut last = t0;
        for k in 0..keys {
            last = t0 + Duration::from_secs_f32(f32::from(k) / cps);
            let mut cx = ctx_in(g, last, c, (5, col0 + k));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            sky.budget.refill(last);
            sky.on_event(&typed(TypedClass::Glyph), last, &cx, &ribbon);
        }
        (sky, last)
    }

    /// 1d: stars born at speed stream backwards at the hand's cadence (T4).
    /// Twenty-four keys at 8, 12 and 2 cps, spine hot. Every sky star the last
    /// key bore is read at birth and 200 ms on: at 8 and 12 cps its backward
    /// travel is within 10 % of the priced curve (`STREAM_SHARE_CW·cw/IOI`,
    /// 126 and 189 px/s, decaying on τ 300); at 2 cps it is exactly zero,
    /// by the cadence.
    #[test]
    fn stars_born_at_speed_stream_backwards_at_the_hand_s_own_cadence() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let window_ms = 200.0f32;
        let window = Duration::from_secs_f32(window_ms / 1000.0);
        for (cps, priced_v0) in [(8.0f32, 126.0f32), (12.0, 189.0), (2.0, 0.0)] {
            let v0 = stream_px_per_s(g.cw as f32, 1000.0 / cps);
            assert!(
                (v0 - priced_v0).abs() <= 0.01 * priced_v0.max(1.0),
                "{cps} cps on the owner's cell prices {v0:.2} px/s, not {priced_v0}"
            );
            let (sky, last) = stream_sky(cps, 24, 30, 0, &c, t0);
            let newborn: Vec<&Star> = sky
                .live_iter()
                .filter(|s| s.born == last && s.lane.is_sky())
                .collect();
            assert!(
                !newborn.is_empty(),
                "the last key at {cps} cps bore no sky star"
            );
            let priced_mean = stream_travel_px(v0) * stream_unit(window_ms) / (window_ms / 1000.0);
            for s in &newborn {
                let (x0, y0) = s.pos(last, false);
                let (x1, _) = s.pos(last + window, false);
                let back = x0 - x1;
                let measured = back as f32 / (window_ms / 1000.0);
                eprintln!(
                    "{cps} cps: {:?} at ({x0}, {y0}) travels {back} px back in {window_ms} ms = {measured:.1} px/s (priced v0 {v0:.1} px/s, {window_ms} ms mean {priced_mean:.1})",
                    s.class
                );
                if cps < 3.0 {
                    assert_eq!(
                        back, 0,
                        "a 2 cps stroll streams {back} px in {window_ms} ms; a stroll stands still"
                    );
                } else {
                    assert!(
                        (measured - priced_mean).abs() <= 0.10 * priced_mean,
                        "{cps} cps: {measured:.1} px/s measured against {priced_mean:.1} priced — not within 10 %"
                    );
                }
            }
        }
    }

    /// L4: no mark crosses ink or the pane edge at any instant of its flight,
    /// under either band anchor (1d, §33). Twenty keys at 12 cps, `tall` and
    /// `underline` (ink on [`band_row`], so each spelling is measured on its
    /// own row), spine hot, the band's row re-probed each key with ink up to
    /// two columns behind the caret. Every strike, field and exhaust mark is
    /// swept 2 ms at a time from birth to death against the probe as it stood
    /// on its key: no sample on ink or unknown, none left of the column behind
    /// the ink, none off the effects box, and every exhaust sample inside the
    /// caret row's sky ([`in_sky_of_row`]). Then the hand types from the
    /// pane's first column over a blank row: every mark stays on glass.
    ///
    /// Non-vacuous: at least two lanes, drift over a cell, every grain clamped
    /// short of its priced 93.7 px (kept, not dropped), and marks born within
    /// their travel of the ink and of the pane edge.
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
            // Sweep one mark born at `at` over its life: every sample on a
            // proven-blank pixel, at or right of `min_col`, on glass; the
            // exhaust inside the caret row's sky. Returns how far left of its
            // birth it ended.
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
            // (a) The ink follows the hand.
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
            eprintln!(
                "L4 SWEEP tall={tall}: {marks} marks of {} lanes, {grains} grains all clamped \
                 short of {priced:.1} px, {sky_clamped} sky stars born within their travel of \
                 the ink, widest drift {widest:.1} px ({:.2} cells)",
                lanes.len(),
                widest / cw
            );
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
            // (b) The pane's own edge, over a blank row, typing from column 0.
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

    /// 1d: the sky is at rest one star-life after the last key. The drift ends
    /// at [`STREAM_END_MS`], pinned to the longest sky class life. Twenty-four
    /// keys at 12 cps, spine hot: 100 ms after the last key some star still
    /// moves; from one star-life on no star's X moves (read at +1 ms, +50,
    /// +200, +1 s, +3 s) and none offers a stream step to the cadence fold.
    #[test]
    fn the_sky_is_at_rest_one_star_life_after_the_last_key() {
        let c = cfg(true);
        let t0 = Instant::now();
        // §5.6 publishes the m1's life as 460; `H + 2.32·τ` is 458.7.
        let life_ms = StarClass::M1.life_s(StarLane::Strike) * 1000.0;
        assert!(
            (STREAM_END_MS - life_ms).abs() <= 1.5,
            "the drift ends at {STREAM_END_MS} ms, not the longest sky life {life_ms:.1}"
        );
        let (sky, last) = stream_sky(12.0, 24, 30, 0, &c, t0);
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
        let mut travelled = Vec::new();
        for s in sky.live_iter() {
            let at_rest = s.pos(rest, false);
            travelled.push(s.pos(s.born, false).0 - at_rest.0);
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
        eprintln!(
            "{streaming} stars streaming at +100 ms; at rest at +{STREAM_END_MS} ms, travelled {travelled:?} px"
        );
    }

    /// 1d: the stream step is solved, not polled (§18). For a star with a
    /// 14.8 px travel, from every age on a 1 ms grid, the solved next step is
    /// where the integer X actually changes, and every step is at least the
    /// tail floor after the previous one.
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
        eprintln!("{steps} solved steps over a {:.1} px travel", s.stream);
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

    /// `STREAM_SPENT` is the literal of `1 − e^(−STREAM_END_MS/STREAM_TAU_MS)`.
    #[test]
    fn the_stream_spent_share_is_the_exponential_s() {
        let want = 1.0 - (-STREAM_END_MS / STREAM_TAU_MS).exp();
        assert!(
            (STREAM_SPENT - want).abs() < 1e-4,
            "STREAM_SPENT {STREAM_SPENT} is not 1 − e^(−{STREAM_END_MS}/{STREAM_TAU_MS}) = {want:.5}"
        );
    }

    // -- the exhaust (§33) -------------------------------------------------

    /// Run `keys` at `cps` on a probed sky over a 15×28 cell, with the band's
    /// row ([`band_row`]) inked up to `ink_to − 1`, and return the pool, the
    /// geometry and every exhaust grain with its birth instant. The caret
    /// starts at `col0` and walks right, so grains are thrown back over blank
    /// columns already passed. Row 4 is blank under both spellings.
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

    /// D16's two band anchors as a row number: the grid row the sky band of
    /// caret row 5 falls in (`row − 1` under `tall`, the caret's own row under
    /// `underline`; [`default_band_top`], [`in_sky_of_row`]).
    fn band_row(c: &Config) -> i32 {
        if c.ribbon_tall { 4 } else { 5 }
    }

    /// The gate census: `keys` keys of `class` at `cps` on a probed-blank sky
    /// at spine `disp`, dark or light, reduced or not: grains thrown, how many
    /// are m2, the bucket left, and the glints cued.
    fn exhaust_census(
        class: TypedClass,
        cps: f32,
        disp: f32,
        dark: bool,
        reduced: bool,
        keys: u16,
        t0: Instant,
    ) -> (usize, usize, f32, usize) {
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut c = cfg(dark);
        c.reduced_motion = reduced;
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let (mut n, mut m2) = (0usize, 0usize);
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
            }
        }
        let glints = sky.take_glints().count();
        (n, m2, sky.budget.tokens(), glints)
    }

    /// What a typing session costs the frame train: `keys` keys at `cps` under
    /// either band anchor, on a probed-blank sky (both rows) at a hot spine,
    /// stepped forward one millisecond at a time ([`Stardust::brisk`] reads
    /// the pool as it stands).
    ///
    /// Returns, as percentages of the run, the share the sky was brisk and
    /// the share it would have been with no exhaust grain (the same predicate
    /// with [`StarLane::Exhaust`] filtered out), then the peak live count and
    /// the evictions [`Stardust::make_room`] made. The counterfactual holds
    /// only while nothing is evicted, so callers assert that too.
    fn exhaust_duty(tall: bool, row_4: CellInk, cps: f32, keys: u16) -> (u32, u32, usize, usize) {
        let mut c = cfg(true);
        c.ribbon_tall = tall;
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
                let mut cx = ctx_in(g, now, &c, (5, 20 + (next_key % 90)));
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

    /// §33: the exhaust is born in the caret's sky and thrown back on the
    /// hand's wind ([`StarLane::Exhaust`], `Stardust::sow_exhaust`,
    /// [`EXHAUST_THROW_MS`]) (owner, 2026-09-10).
    ///
    /// Sixteen keys at 12 cps on a 15×28 cell, spine hot, sky row blank.
    /// Every key from the second bears exactly one grain inside the caret
    /// cell's sky band (`x` within [`SKY_JITTER_CW`] of the centre, `y` in
    /// §5.4's zone, one pixel of slack each).
    ///
    /// At [`EXHAUST_THROW_MS`] the grain has run at least 3 cells back (priced
    /// 6.2) and risen at most [`EXHAUST_RISE_CH`]. An m3 is dead by 176 ms;
    /// the 1-in-4 m2 ([`EXHAUST_M2_IN`]) is alive at 200 ms and has not moved
    /// since 160 (§5.6: thrown, then pinned).
    #[test]
    fn the_exhaust_is_born_in_the_caret_s_sky_and_thrown_back_on_the_hand_s_wind() {
        let c = cfg(true);
        let t0 = Instant::now();
        let (_, g, born) = exhaust_run(12.0, 16, 40, 0, &c, t0);
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        let top = default_band_top(g, &c, 5);
        assert_eq!(
            born.len(),
            15,
            "sixteen keys at 12 cps threw {} grains, not one each from the second key on",
            born.len()
        );
        let mut m2 = None;
        for (k, (s, at)) in born.iter().enumerate() {
            let col = 41 + k as u16;
            let (cx, _) = g.cell_center(5, col);
            assert!(
                (s.x - cx).abs() <= SKY_JITTER_CW * cw + 1.0,
                "grain {k} was born at x {} — {:.1} px off the caret cell's centre {cx}",
                s.x,
                (s.x - cx).abs()
            );
            assert!(
                s.y >= (top - SKY_TOP_CH * ch).round() - 1.0
                    && s.y <= (top - SKY_BOTTOM_CH * ch).round() + 1.0,
                "grain {k} was born at y {} — outside the caret cell's sky band [{}, {}]",
                s.y,
                (top - SKY_TOP_CH * ch).round(),
                (top - SKY_BOTTOM_CH * ch).round()
            );
            let (x0, y0) = s.pos(*at, false);
            let thrown = *at + Duration::from_millis(EXHAUST_THROW_MS as u64);
            let (x1, y1) = s.pos(thrown, false);
            assert!(
                (x0 - x1) as f32 >= 3.0 * cw,
                "grain {k} ran {} px back in {EXHAUST_THROW_MS} ms — under three cells",
                x0 - x1
            );
            assert!(
                (y0 - y1) as f32 <= EXHAUST_RISE_CH * ch + 1.0 && y1 <= y0,
                "grain {k} rose {} px, not at most {:.1}",
                y0 - y1,
                EXHAUST_RISE_CH * ch
            );
            match s.class {
                StarClass::M3 => assert!(
                    s.dead(*at + Duration::from_millis(176)),
                    "an exhaust m3 outlived its 175.6 ms transient life"
                ),
                StarClass::M2 => {
                    assert!(
                        !s.dead(*at + Duration::from_millis(200)),
                        "an exhaust m2 died before its 245.9 ms transient life"
                    );
                    assert_eq!(
                        s.pos(thrown, false),
                        s.pos(*at + Duration::from_millis(200), false),
                        "an exhaust m2 kept moving after its throw: thrown, then PINNED"
                    );
                    m2 = Some(*s);
                }
                StarClass::M1 => panic!("the exhaust dealt an m1: it deals grains and m2s only"),
            }
        }
        assert!(
            m2.is_some(),
            "fifteen grains at a hot spine dealt no m2 at all (1 in {EXHAUST_M2_IN})"
        );
    }

    /// §33: the exhaust never chimes, skips a Space, and deals its m2 only at a
    /// hot spine (T1, §13, §8.2, §5.2). Twelve or sixty keys at 8 cps:
    ///
    /// * a hot hand throws one grain per key from the second, leaving the same
    ///   bucket as the same keys under reduced motion (which sows none): the
    ///   exhaust spends no glint token;
    /// * the lane itself never chimes: [`StarLane::may_chime`] is `false`,
    ///   asserted directly, since no count can observe it (the exhaust reaches
    ///   the pool through [`Stardust::throw`], which has no glint arm);
    /// * a Space throws none (it lays bed, it does not strike, §8.2);
    /// * a warm hand (spine 0.6, between [`STREAM_DISP`] and
    ///   [`FIELD_HOT_DISP`]) deals no m2 over sixty keys, while a hot control
    ///   does;
    /// * a light theme throws [`LIGHT_COUNT_SCALE`] of the dark theme's.
    ///
    /// The cold, stroll and reduced-motion refusals are tested in
    /// `the_exhaust_costs_the_frame_train_its_own_window_and_no_more`.
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
        // The no-chime clause, asserted on the lane's own predicate (see the
        // doc).
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
        let share = light.0 as f32 / dark.0 as f32;
        eprintln!(
            "the light theme threw {} of the dark theme's {} grains — {share:.2}",
            light.0, dark.0
        );
        assert!(
            (0.45..=0.80).contains(&share),
            "the light share is {share:.2}, not about {LIGHT_COUNT_SCALE}"
        );
    }

    /// §33: the exhaust costs the frame train its own window and no more (T6).
    /// The grain is the only brisk thing in a typing sky ([`Star::brisk`] is
    /// `false` for [`Motion::Lift`]), so its cost is bounded:
    ///
    /// * from the last key the sky is brisk until [`EXHAUST_THROW_MS`] − 1 and
    ///   not at + 1, measured from +20 ms (past the field star's
    ///   [`EDGE_IN_S`]);
    /// * the travel cap ([`WALK_FAST_CELLS`]) bounds a burst's grain: at 30 cps
    ///   every grain runs the capped 8.4 cells, not the priced 15.6, over a
    ///   blank row with a distant frontier, and the cap is asserted reached
    ///   (it binds only above ~16.2 cps);
    /// * the whole session's duty matches the window arithmetic within two
    ///   points at 4 and 12 cps (key `k ≥ 1` holds `[t_k, t_k + 160)`, every
    ///   key's field star its 18 ms edge-in), and the no-exhaust baseline its
    ///   own; §18's per-frame cost gate cannot see frame counts;
    /// * within [`EXHAUST_HEADROOM`] of the cap the exhaust yields, so past the
    ///   20 cps knee evictions match the no-exhaust figure;
    /// * with the effect off (reduced motion, cold spine, stroll) no grain is
    ///   sown, and at a cold spine and a stroll the sky is quiet at +20 ms.
    #[test]
    fn the_exhaust_costs_the_frame_train_its_own_window_and_no_more() {
        let c = cfg(true);
        let t0 = Instant::now();
        let (sky, _, born) = exhaust_run(12.0, 16, 40, 0, &c, t0);
        let last = born.last().expect("the run threw grains").1;
        let at = |ms: u64| last + Duration::from_millis(ms);
        // Past the field star's 18 ms edge-in: from here the grain is the
        // only brisk thing in the sky.
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
        // The travel cap ([`WALK_FAST_CELLS`]) where it binds (above ~16.2 cps:
        // bursts, paste echoes, streamed output). At 30 cps it holds the
        // priced travel to 8.4 cells instead of 15.6, over a blank row whose
        // frontier is 300 px away, and is asserted reached.
        let (fast, gf, fast_born) = exhaust_run(30.0, 60, 20, 0, &c, t0);
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
        eprintln!(
            "60 keys at 30 cps: {} live of {STAR_CAP}, pool {} of {STAR_POOL_MAX}, {} of them exhaust",
            fast.live_iter().filter(|s| s.finish.is_none()).count(),
            fast.live(),
            fast.live_iter()
                .filter(|s| s.lane == StarLane::Exhaust)
                .count()
        );
        // The whole session's cost, held to the window arithmetic. Over 60
        // keys the first key throws nothing and holds only its edge-in; every
        // later key holds `[t_k, t_k + THROW)`. The baseline is the edge-ins
        // alone. Percentages of the census span (last key + 300 ms), with two
        // points of slack for the millisecond grid.
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
        for cps in [4.0f32, 12.0] {
            let (on, off, live, evicted) = exhaust_duty(true, CellInk::Blank, cps, 60);
            let (want_on, want_off) = expect(cps);
            eprintln!(
                "{cps} cps, 60 keys: brisk {on}% of the session with the exhaust (arithmetic \
                 {want_on}%), {off}% without it (arithmetic {want_off}%); {live} live of \
                 {STAR_CAP}, {evicted} evictions"
            );
            assert_eq!(
                evicted, 0,
                "{cps} cps evicted {evicted} stars, so the no-exhaust baseline is not a \
                 counterfactual: the exhaust displaced a star the baseline keeps"
            );
            assert!(
                on.abs_diff(want_on) <= 2,
                "{cps} cps holds the frame train {on}% of the session where its window \
                 arithmetic says {want_on}%: the exhaust costs more, or less, than its window"
            );
            assert!(
                off.abs_diff(want_off) <= 2,
                "{cps} cps was brisk {off}% of the session with no grain in the pool, where \
                 the edge-ins alone say {want_off}%: the BASELINE has moved"
            );
        }
        // The cap costs the sky nothing: past the 20 cps knee `make_room`
        // would retire the oldest live star (the strike m1 the eye follows)
        // for a grain, so the exhaust yields within `EXHAUST_HEADROOM` of the
        // cap and evictions match the no-exhaust figure (1 at 25 and 30 cps
        // on this fixture). Pinned both ways: the peak at the knee and, past
        // it, the evictions.
        for cps in [25.0f32, 30.0] {
            let (_, _, live, evicted) = exhaust_duty(true, CellInk::Blank, cps, 60);
            eprintln!("{cps} cps, 60 keys: {live} live of {STAR_CAP}, {evicted} evictions");
            assert!(
                evicted <= 1,
                "{cps} cps evicted {evicted} stars: the sky measured 1 eviction before the \
                 exhaust existed, so the spark is displacing a hero it must yield to"
            );
        }
        let (_, _, live20, evict20) = exhaust_duty(true, CellInk::Blank, 20.0, 60);
        eprintln!("20 cps, 60 keys: {live20} live of {STAR_CAP}, {evict20} evictions");
        assert!(
            live20 <= 36,
            "20 cps now peaks at {live20} live of {STAR_CAP} where the sky measured 35 with \
             and without the exhaust: the spark is pushing the population past the knee it \
             is meant to yield at"
        );
        // Under the other band anchor: both spellings cost the same, since the
        // brisk window is the throw's, not the band's.
        let (on_t, off_t, _, _) = exhaust_duty(true, CellInk::Blank, 12.0, 60);
        let (on_u, off_u, _, _) = exhaust_duty(false, CellInk::Blank, 12.0, 60);
        eprintln!(
            "12 cps, 60 keys: tall {on_t}% (bare {off_t}%), underline {on_u}% (bare {off_u}%)"
        );
        assert!(
            on_u.abs_diff(on_t) <= 4,
            "the underline spelling holds the frame train {on_u}% of the session where tall \
             holds it {on_t}%: one spelling is paying for a grain the other is not throwing"
        );
        // Off is off: no grain, and no brisk frame owed to one.
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
            // `Stardust::brisk` is `false` under reduced motion by
            // construction, so only the other two can be asked.
            assert!(
                reduced || !sky.brisk(last + Duration::from_millis(edge_in_ms)),
                "{label} owed the engine a frame-cadence wake {edge_in_ms} ms after the key, \
                 past the field's own edge-in"
            );
        }
    }

    /// §33: a grain needs a cell to fly over. The cell behind the caret must be
    /// clear for the sky ([`GlyphProbe::sky_at`]) on every row the flight
    /// covers, or nothing is thrown (otherwise the throw would be a sub-cell
    /// twitch holding the frame train). Two refusing fixtures, each with a
    /// throwing control:
    ///
    /// * `tall` with `row − 1` inked everywhere but the caret's column: no
    ///   grain, and the sky quiet past the field star's edge-in. A blank row
    ///   throws one per key from the second;
    /// * `underline` on a line being typed (the band is the caret's own row,
    ///   D16, and the just-echoed glyph at `col − 1` is ink): no grain; with
    ///   one blank cell behind the caret (after a Space), one per key.
    #[test]
    fn the_exhaust_needs_a_blank_cell_behind_the_caret() {
        let t0 = Instant::now();
        let edge_in_ms = (EDGE_IN_S * 1000.0).ceil() as u64 + 2;
        // Sixteen keys at 12 cps, `probe` written before every key as the host
        // does: the grains thrown, and whether the sky is still brisk past the
        // field's edge-in after the last key.
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
        // The line being typed as the host probes it: glyphs up to and
        // including `col − 1 − gap`.
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
        let mut under = cfg(true);
        under.ribbon_tall = false;
        let (grains, brisk) = run(&under, &|sky, col| {
            sky.probe_mut().probe_row(4, &[false; 120]);
            text_to(sky, col, 0);
        });
        assert_eq!(
            grains, 0,
            "underline on a line being typed threw {grains} grains over the glyph just echoed"
        );
        assert!(
            !brisk,
            "underline on a line being typed owed a frame-cadence wake {edge_in_ms} ms after \
             the key"
        );
        let (grains, _) = run(&under, &|sky, col| {
            sky.probe_mut().probe_row(4, &[false; 120]);
            text_to(sky, col, 1);
        });
        assert_eq!(
            grains, 15,
            "underline with one blank cell behind the caret threw {grains} grains, not one per \
             key from the second"
        );
    }

    /// §33: the exhaust flies along a rule row, in the sky zone under the
    /// stroke. A TUI's input box puts a light rule in every column of the row
    /// above the line being typed; the frontier walk reads the sky's clearance
    /// ([`GlyphProbe::sky_at`]), so marks fly there (owner, 2026-09-08).
    ///
    /// Sixty keys at 12 cps on a 15×28 cell, `tall`, `row − 1` a rule end to
    /// end and the line inked up to the caret; the same over a blank row as
    /// control:
    ///
    /// * one grain per key from the second, each running at least three cells;
    /// * sky stars stream as far as over a blank row;
    /// * every 2 ms flight sample of every grain and sky star has its core
    ///   under the stroke band ([`rule_stroke_px`], pixel probe clear) inside
    ///   the caret row's sky, and no rest is on the stroke; some grains rise
    ///   less than the smallest unclamped rise and some strikes lift less than
    ///   their full lift, while over a blank row every strike lifts fully;
    /// * no chime beyond the blank row's, and no grain is an m1;
    /// * the frame-train duty and evictions match the blank row's (the
    ///   no-exhaust baseline differs because no veil is laid over a rule
    ///   cell; reported, not compared);
    /// * a rule ending left of the caret is crossed by marks born over blanks,
    ///   none entering its stroke (the per-column law itself is
    ///   `the_headroom_under_a_rule_reads_every_column_the_flight_crosses`).
    #[test]
    fn the_exhaust_flies_along_a_rule_row_in_the_sky_zone_under_the_stroke() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        // Sixty keys, the probe written before each as the host writes a
        // bordered input box: `row_4` above the line, the line inked through
        // the cell behind the caret. Returns the pool, the grains and the sky
        // stars born, each with its birth instant.
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
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
        let max = |v: &[f32]| v.iter().copied().fold(0.0f32, f32::max);
        let min = |v: &[f32]| v.iter().copied().fold(f32::INFINITY, f32::min);
        // How far each mark travels left over its life, px.
        let travel = |v: &[(Star, Instant)]| -> Vec<f32> {
            v.iter()
                .map(|(s, at)| (s.pos(*at, false).0 - s.rest().0) as f32)
                .collect()
        };
        // Every position of every mark, 2 ms apart, against the probe at birth:
        // samples, cores inside a row-4 rule cell's stroke band (from the band
        // and cell class, not the pixel probe), cores on ink or unknown by the
        // pixel probe, and rests on ink.
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
        let (mut sky, grains, stars) = run(&[CellInk::Rule; 120]);
        let (mut blank, blank_grains, blank_stars) = run(&[CellInk::Blank; 120]);
        let (travels, blank_travels) = (travel(&grains), travel(&blank_grains));
        let (streams, blank_streams) = (travel(&stars), travel(&blank_stars));
        eprintln!(
            "RULE ROW: {} grains, travel min {:.1} / mean {:.1} / max {:.1} px ({:.2} cells \
             mean); {} sky stars streaming mean {:.1} px, max {:.1}. BLANK ROW: {} grains, \
             travel mean {:.1} px; {} sky stars streaming mean {:.1} px, max {:.1}",
            grains.len(),
            min(&travels),
            mean(&travels),
            max(&travels),
            mean(&travels) / cw,
            streams.len(),
            mean(&streams),
            max(&streams),
            blank_grains.len(),
            mean(&blank_travels),
            blank_streams.len(),
            mean(&blank_streams),
            max(&blank_streams)
        );
        // 1. The count and the travel match the blank row's.
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
        // 2. The sky's own drift — one walk, two callers, one answer.
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
        // 3. The flight under the stroke, sampled every 2 ms of every grain and
        // sky star.
        let (samples, in_band, on_ink, rests) = sample(&sky, &grains);
        let (star_samples, star_in_band, star_on_ink, star_rests) = sample(&sky, &stars);
        let min_rise = (0.5 * EXHAUST_RISE_CH * ch).round() as i32;
        let clamped = grains
            .iter()
            .filter(|(s, at)| s.pos(*at, false).1 - s.rest().1 < min_rise)
            .count();
        // A strike's unclamped lift over its life, px (3 for a hero, 2 for an
        // m2 here), against the lift each star takes; m3 and field stars are
        // pinned.
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
        eprintln!(
            "GRAINS: {samples} flight samples, {in_band} in the stroke band, {on_ink} on ink, \
             {rests} rests on ink; {clamped} of {} grains rose under the smallest unclamped \
             rise ({min_rise} px). STARS: {star_samples} samples, {star_in_band} in the band, \
             {star_on_ink} on ink, {star_rests} rests on ink; of the lifting strikes {held} \
             lift less than their whole lift and {whole} lift it all",
            grains.len()
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
        // ...and the sky's own stars: no strike lifts its core into the stroke.
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
        // The blank row's stars keep §5.6's whole lift, every one.
        assert!(
            blank_stars
                .iter()
                .filter(|(s, _)| s.lane == StarLane::Strike && s.class != StarClass::M3)
                .all(|(s, at)| s.pos(*at, false).1 - s.rest().1 == full_lift(s)),
            "a strike over a blank row lifted less than its whole lift"
        );
        // 4. No chime — the glints are the blank row's own.
        assert_eq!(
            sky.take_glints().count(),
            blank.take_glints().count(),
            "the rule row cued a different number of glints from the blank row"
        );
        assert!(
            grains.iter().all(|(s, _)| s.class != StarClass::M1),
            "the exhaust dealt an m1 along the rule row"
        );
        // 5. The duty matches a blank row's. The bare figure is reported only:
        // it is the veil's edge-in, and no veil is laid over a rule cell
        // (`deal_veil` reads `GlyphProbe::at`).
        let (on, off, _, evicted) = exhaust_duty(true, CellInk::Rule, 12.0, 60);
        let (on_b, off_b, _, evicted_b) = exhaust_duty(true, CellInk::Blank, 12.0, 60);
        eprintln!(
            "12 cps, 60 keys under a rule row: brisk {on}% of the session ({off}% without the \
             exhaust), {evicted} evictions; over a blank row {on_b}% ({off_b}%), {evicted_b}"
        );
        assert!(
            on.abs_diff(on_b) <= 1 && evicted == evicted_b,
            "the rule row holds the frame train {on}% of the session ({evicted} evictions) \
             where the blank row holds it {on_b}% ({evicted_b}): the spark costs more, or \
             less, along a rule than over a blank row"
        );
        // 6. A rule that ends left of the caret: rule cells before column 40,
        // blanks from 40 on. A grain born under a blank cell rises fully unless
        // its flight crosses onto the rule, where the headroom is read over
        // every cell of the flight.
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
        let (samples, in_band, on_ink, rests) = sample(&pool, &grains);
        let (star_samples, star_in_band, star_on_ink, star_rests) = sample(&pool, &stars);
        eprintln!(
            "TITLE RULE: {} grains and {} stars, {crossed} born over a blank cell and resting \
             over the rule; grains {samples} samples / {in_band} in band / {on_ink} on ink / \
             {rests} rests on ink; stars {star_samples} / {star_in_band} / {star_on_ink} / \
             {star_rests}",
            grains.len(),
            stars.len()
        );
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

    /// §33: a grain is not born on the caret cell's own glyph. Under
    /// `underline` the band is the caret's row (D16), so that cell must be
    /// clear too. With the caret row inked except the cell behind the caret,
    /// `underline` throws nothing; `tall` (band in the blank row above) throws
    /// one per key from the second.
    #[test]
    fn the_exhaust_is_not_born_on_the_caret_cell_s_own_glyph() {
        let t0 = Instant::now();
        for (tall, want) in [(false, 0usize), (true, 15)] {
            let mut c = cfg(true);
            c.ribbon_tall = tall;
            let g = geom_cell(15, 28);
            let ribbon = Ribbon::new();
            let mut sky = Stardust::new();
            let (mut grains, mut on_glyph) = (0usize, 0usize);
            for k in 0..16u16 {
                let col = 40 + k;
                let mut cells = [true; 120];
                cells[usize::from(col) - 1] = false;
                sky.probe_mut().probe_row(4, &[false; 120]);
                sky.probe_mut().probe_row(5, &cells);
                let at = t0 + Duration::from_secs_f32(f32::from(k) / 12.0);
                let mut cx = ctx_in(g, at, &c, (5, col));
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                for s in sky
                    .live_iter()
                    .filter(|s| s.born == at && s.lane == StarLane::Exhaust)
                {
                    grains += 1;
                    on_glyph +=
                        usize::from(sky.probe().at_px(s.x as i32, s.y as i32, g) == Some(true));
                }
            }
            assert_eq!(
                grains, want,
                "tall {tall}: a caret over a glyph threw {grains} grains, not {want}"
            );
            assert_eq!(
                on_glyph, 0,
                "tall {tall}: {on_glyph} grains were born with their core on a probed glyph"
            );
        }
    }

    /// §33: an exhaust grain's rest takes no home-cell exemption. The
    /// exhaust's origin is the caret cell's sky (under `tall`, `row − 1`), not
    /// a caret-owned cell, so a grain born in a rule cell's sky zone and
    /// thrown 4 px up into the stroke is dropped (no clear pixel within 2 px,
    /// no walk-back for a free grain). The same star on the erase lane, whose
    /// origin is exempt, is sown with its rest untouched.
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

    /// §33: the exhaust floors its spine on the mend's ([`Mend::disp`]). A cold
    /// hand (`birth_disp` 0.3) at 8 cps throws nothing over twelve keys; the
    /// same keys each carrying a mend mark at 0.9 throw one from the second.
    /// The floor is the mark's whether or not the pull was accepted.
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

    /// The blank frontier's early stop gives the same answer as walking the
    /// whole blank run (`x − floor_x` only grows leftward, and the value is
    /// `want.min(x − floor_x)`): a 190-column row in twelve ink patterns ×
    /// nine caret columns × six `want`s, 5 832 answers equal to the bit.
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
                // Twelve shapes: all blank, all ink, and ten seeded mixes, so
                // the frontier lands at every reachable distance.
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
                // The reference: the whole contiguous blank run left of the
                // caret's own column, with no early stop at all.
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
                    // The band's own row, as every caller now passes it: a
                    // point span inside row 4 under this fixture's `tall`.
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

    // -- the mend's star (1e) ----------------------------------------------

    /// One event on a hot sky (`disp` 0.9), with the frame the engine's tick
    /// would emit at the same instant.
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

    /// The typo script on the sky alone (§23, "The mend"): eight glyphs at
    /// 8 cps from column 4 of row 5, sky row blank, spine hot; `deletes`
    /// Backspaces one slot apart after the last glyph; the fix `fix_after_ms`
    /// after the last delete, carrying the engine's [`Mend`] mark when
    /// `mended`. Returns the sky, the fix's instant, the erased cell, the sky
    /// star live over it just before the fix as `(position, class)`, and the
    /// classes of sky stars over the caret's cell then.
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
        // Glyphs on columns 4..=11; the caret at 12.
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

    /// 1e: a mend's star is born where the typo was and pulled to the fix
    /// (§23, [`MEND_PULL_MS`]).
    ///
    /// The typo script four ways (fix one slot after one Backspace, 500 ms
    /// after, one slot after two, and after a Backspace over a capital typo
    /// whose hero still lifts in the caret's cell). On the fix's instant one
    /// of its stars, an m2, stands where the typo was (at the retiring star's
    /// pixel if live, else over the erased cell's sky); 110 ms on it stands
    /// over the caret's cell. Its travel never goes backward in the window,
    /// equals the pull exactly, and stops after. The retiring star is gone
    /// from the erased cell, which bears no fresh star; the fix bears one sky
    /// star over the caret's cell and at +110 ms it is the only one there.
    /// The sky is brisk inside the window and not after, and the star's
    /// centre over the shipped ground at +0, +55 and +110 ms is under the
    /// caret law's ceiling.
    #[test]
    fn a_mend_s_star_is_born_where_the_typo_was_and_dragged_to_the_fix() {
        let mut c = cfg(true);
        c.theme_bg = SHIPPED_GROUND;
        let g = geom_cell(15, 28);
        let window_ms = MEND_PULL_MS as u64;
        let ceil = STAR_LIGHT_CEIL * 255.0;
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
            eprintln!(
                "{label}: over the caret's cell {home:?} when the fix landed: {home_before:?}"
            );
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
            // Where the typo was: the retiring star's pixel, else the erased
            // cell's sky.
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

            // Its own travel: the lane's position with the slipstream's
            // steps read back, ms by ms.
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
            eprintln!(
                "{label}: born at {:?} over {erased:?}, home {:?} over {home:?}: pulled {travelled} px in {window_ms} ms (priced {:.0}), at +55 ms {} px in",
                (x0, y0),
                star.pos(at(window_ms), false),
                star.pull,
                own(55) - own(0)
            );
            assert_eq!(
                travelled,
                star.pull.round() as i32,
                "{label}: pulled {travelled} px, priced {:.1}",
                star.pull
            );
            assert!(
                travelled >= g.cw as i32,
                "{label}: pulled {travelled} px — less than the {} px cell",
                g.cw
            );
            for ms in window_ms + 1..=400 {
                assert_eq!(
                    own(ms),
                    own(window_ms),
                    "{label}: the star moved at +{ms} ms, after its window"
                );
            }

            // The grab: the light does not jump to be pulled.
            match retiring {
                Some((p, class)) => {
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
                    eprintln!("{label}: grabbed the retiring {class:?} at {p:?}");
                }
                None => eprintln!("{label}: nothing was retiring over {erased:?}"),
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

            // The cadence: one brisk window, then tails. The pool's window is
            // the longest motion the fix key started: the exhaust's throw
            // ([`EXHAUST_THROW_MS`]) outlasts the pull when the fix throws a
            // grain. The pull's own window is read off the mend's star, on
            // every fixture including the 500 ms fix that throws no grain.
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

            // The caret stays brightest: the centre under the caret law.
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
                    lum <= ceil,
                    "{label}: the mend's star centre at +{ms} ms reads Y {lum:.1} over the caret law's {ceil:.1}"
                );
            }
        }
    }

    // =======================================================================
    // The end-to-end star catch: the seam walked whole
    // =======================================================================
    //
    // `mod.rs` proves the sky's half (`Engine::pet_offer` / `catch_star`) and
    // `kitty_pet.rs` the receiver's, each against a hand-built counterpart.
    // This harness walks a real star from a real keystroke to a real cat's
    // paw and back: one engine and one brain on one clock with the host's two
    // lines between them, plus a twin pair that never hears of the seam, so
    // "nothing else moved" is a diff against the un-offered world.

    use crate::kitty_pet::{PetAction, PetBrain, PetFrame, PetSense};
    use crate::rainbow_kitty::companion::{CATCH_REACH_CELLS, PetOffer, PetOnGlass, StarCatch};
    use crate::rainbow_kitty::{Dir, Engine, Licence};

    /// The paw's grid: the pet's fixture (100×30 cells of 10×20) at origin 0,
    /// so the pet's grid px and the sky's window px coincide.
    fn paw_geom() -> Geom {
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

    fn paw_sense(now: Instant, caret: (u16, u16)) -> PetSense {
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

    /// The host's frame cadence while the pet or the sky is live.
    const FRAME_MS: u64 = 16;

    /// The column past which the typist's line wraps to the next row.
    const WRAP_COL: u16 = 90;

    /// What one lockstep frame reported, for the tests to read.
    #[derive(Clone, Copy, Debug)]
    struct Step {
        at: Instant,
        pet: PetFrame,
        twin_pet: PetFrame,
        offer: PetOffer,
        /// The star the paw handed back this frame, and whether the sky still
        /// had it (`Engine::catch_star`).
        caught: Option<(StarCatch, bool)>,
        /// `(quads, halos, cues)` the offered engine emitted this frame …
        emitted: (usize, usize, usize),
        /// … and the un-offered twin.
        twin_emitted: (usize, usize, usize),
    }

    /// One world, twice: an engine and a brain wired through the host's two
    /// lines, and a twin pair fed identical events on the same clock without
    /// the seam.
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

        /// One frame at `self.t`: both engines tick, both brains tick, then the
        /// host's two lines, in `EffectsPipeline::offer_pet_v2`'s order.
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
            // The seam — the host's two lines, verbatim from `pet_offer`'s
            // contract.
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

        /// Advance the clock one frame and run it.
        fn next_frame(&mut self) -> Step {
            self.t += Duration::from_millis(FRAME_MS);
            self.frame()
        }

        /// Run frames up to (not at) `self.t + gap` and set the clock there,
        /// so a test may read the seed knob before the next key.
        fn advance(&mut self, gap: Duration) {
            let due = self.t + gap;
            while self.t + Duration::from_millis(FRAME_MS) < due {
                self.next_frame();
            }
            self.t = due;
        }

        /// One keystroke at `self.t` in both worlds: the echo's one-cell move
        /// under the Typed licence and the key, on a row the probe has proved
        /// blank (§5.4), then the frame that echoes it.
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

        /// One Backspace at `self.t`, in both worlds: the retreat's one-cell
        /// move and the erase.
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

        /// Enter at `self.t`: the caret to the start of the next row, under
        /// the Return licence, in both worlds.
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

        /// `gap` later, the next key — an Enter first when the line is full.
        fn key_after(&mut self, gap: Duration, class: TypedClass) -> Step {
            self.advance(gap);
            if self.caret.1 >= WRAP_COL {
                self.newline();
                self.advance(gap);
            }
            self.key(class)
        }

        /// The engines an event is reported to.
        fn engines(&mut self) -> impl Iterator<Item = &mut Engine> {
            [&mut self.eng, &mut self.twin_eng].into_iter()
        }

        /// The cat as the sky will read it on the NEXT frame's offer — the
        /// last published frame, which is what the host hands `pet_offer`.
        fn cat(&self) -> Option<PetOnGlass> {
            self.last.and_then(|s| PetOnGlass::of(&s.pet, paw_geom()))
        }

        /// The offered engine's copy of `star`, by identity.
        fn star(&self, star: StarCatch) -> Option<Star> {
            find_star(&self.eng, star)
        }

        /// The un-offered twin's copy of `star`, by identity.
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

    fn emitted(sc: &Scratch) -> (usize, usize, usize) {
        (sc.under.len() + sc.out.len(), sc.halos.len(), sc.cues.len())
    }

    fn find_star(eng: &Engine, star: StarCatch) -> Option<Star> {
        eng.stardust
            .live_iter()
            .find(|s| s.born == star.born && s.x == star.x && s.y == star.y)
            .copied()
    }

    /// The gold m1 born on this instant in `eng`'s sky, if the key dealt one.
    fn gold_born(eng: &Engine, at: Instant) -> Option<Star> {
        eng.stardust
            .live_iter()
            .find(|s| s.gold && s.class == StarClass::M1 && s.lane.is_sky() && s.born == at)
            .copied()
    }

    /// The seed knob, read ahead: whether a shifted capital pressed now on
    /// `cell` deals a gold m1, and where.
    ///
    /// * The class is by rule: a capital earns its hero (§5.8), never demoted
    ///   ([`Stardust::deal_star`]).
    /// * The tint is by seed: `deal_tint` on the strike's seed
    ///   `cell_hash(row, col, minted + 1)` ([`Stardust::mint_seed`]; the
    ///   strike is a key's first mint), at the cell the strike is born on,
    ///   [`Stardust::free_sky_cell`] of the typed cell.
    ///
    /// A test drives keys until the knob reads gold: a fixed search over the
    /// event sequence (§18).
    fn gold_capital_at(eng: &Engine, cell: (u16, u16)) -> Option<(u16, u16)> {
        let free = eng.stardust.free_sky_cell(cell, paw_geom())?;
        let seed = cell_hash(free.0, free.1, eng.stardust.minted.wrapping_add(1));
        deal_tint(seed, 0.0).1.then_some(free)
    }

    /// The typo rally that seats a cat beside the caret: `fwd` keys forward and
    /// `back` Backspaces, repeated at `gap`. Three reversals inside
    /// `FROLIC_WINDOW` earn a frolic, and a second frolic inside
    /// `TENNIS_AFTER` sits the cat down to watch (`kitty_pet.rs`). Returns once
    /// the pet's frame says `Sit` with the purr tell up, or panics after
    /// `rounds`.
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

    /// A real star walked from `deal_typed` through the paw to
    /// `Stardust::catch`: a real `Engine` typing at 12 cps and a real
    /// `PetBrain` on one clock, ticked in lockstep with the host's two lines
    /// between them; a gold m1 dealt within reach via the seed knob
    /// ([`gold_capital_at`]). The only change allowed is one star's life, on
    /// one frame, to the catch horizon, in a world otherwise bit-identical to
    /// its un-offered twin.
    ///
    /// The cat is seated by the rally ([`rally_until_seated`]): the only
    /// settled, purring posture a keystroke finds, since a seat stands on the
    /// first key of a resume and no star outlives the time to sit again.
    #[test]
    fn a_gold_star_walks_from_the_deal_through_the_paw_to_the_catch() {
        let t0 = Instant::now();
        let g = paw_geom();
        let gap = Duration::from_micros(83_333); // 12 cps
        let mut w = Lockstep::new(t0, (4, 0));

        // A sentence of running earns the cat its contentment and the engine
        // its heat; then the rally seats the cat.
        for _ in 0..30 {
            w.key_after(gap, TypedClass::Glyph);
        }
        let rounds = rally_until_seated(&mut w, gap, 12);
        let seat = w.cat().expect("fixture: the cat is on glass");
        assert!(seat.contented(), "fixture: a contented seat");

        // The deal. Keep the rally going so the caret stays in the paw's reach,
        // and on the third forward key of each round read the seed knob at the
        // key's instant; press a capital where it says gold.
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

        // The capital's strike is a gold m1 at the named cell in both worlds
        // (the deal is the keystroke's, not the seam's).
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

        // The offer on the birth frame: the sky names that star to the seated,
        // purring cat, and nothing is acted on yet (the paw lands on a tick).
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

        // The paw. Frame by frame until the brain hands the star back; the
        // star's life must not move on any frame before that one.
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

        // Exactly one life moved, to the catch horizon, on this frame...
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
        // … and no other star's did: every other star in the twin's pool is
        // in the offered pool with the same life, and vice versa.
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

        // The pet drew no light of its own: no extra mote, and the sky emitted
        // exactly the twin's quads, halos and cues on the contact frame (a
        // finish starting at `now` has not yet dimmed the star).
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

        // No teleport: the reaching cat is where the un-offered twin is, to
        // the bit, on the contact frame.
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

        // Gone by the horizon, and only in the offered world.
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

        // One catch. The offer stood for one star; nothing hands it back
        // twice, and nothing else is offered while the rally's seat lasts.
        for _ in 0..90 {
            let s = w.next_frame();
            catches += usize::from(s.caught.is_some());
        }
        assert_eq!(catches, 1, "the paw handed a star back {catches} times");

        // Idle → exactly zero afterwards...
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
        let t_zero = t_zero.expect("the sky idles to exactly zero within 20 s of the catch");
        assert_eq!(w.eng.status().stars, 0);
        assert_eq!(
            w.eng.pet_offer(g, w.cat()),
            PetOffer::default(),
            "an idle sky offers the resident nothing"
        );
        // … and the pet's own frame train stands down (its settle ladder is
        // its own law; what the seam owes is that it left nothing armed).
        for _ in 0..(40 * 1000 / FRAME_MS) {
            if !w.pet.needs_frames() {
                break;
            }
            w.next_frame();
        }
        assert!(
            !w.pet.needs_frames(),
            "the pet's frame train did not stand down"
        );
        assert_eq!(
            w.pet.pending_v2_offer(),
            (false, false),
            "a latch outlived the catch"
        );
        assert_eq!(w.eng.fingerprint(), 0, "the sky lit again while idle");
        eprintln!(
            "walk: seat after {rounds} rally rounds at col {:.2}; gold dealt at {:?} (free cell {:?}) \
             {:.0} ms after t0; paw {:?} after birth; horizon {:?} after the paw; sky at zero {:.1} s \
             after the catch",
            seat.col,
            w.caret,
            free,
            (birth.at - t0).as_secs_f32() * 1000.0,
            contact.at - birth.at,
            horizon - contact.at,
            (t_zero - contact.at).as_secs_f32()
        );
    }

    /// The fan wears the band's stop at each star's own column: a fan star
    /// snaps to the landing's walk at its rest column, so the fan shows the
    /// band's neighbouring names around the pin, not the seven anchors. The
    /// hero stays gold (D6).
    #[test]
    fn the_fan_wears_the_band_s_stop_at_each_star_s_own_column() {
        use crate::rainbow_kitty::meteor::landing_field;
        let c = cfg(true);
        let t0 = Instant::now();
        let cx = ctx(t0, &c, (5, 40));
        let cw = cx.geom.cw as f32;
        let origin = (300.0_f32, 120.0_f32);
        let t_rest = t0 + Duration::from_millis(FAN_THROW_MS as u64 + 1);
        for t_land in [0.15_f32, 0.5, 0.85] {
            let mut sky = Stardust::new();
            sky.sow_fan(
                t0,
                FanSow {
                    at_px: origin,
                    reach: 12.0,
                    walk: LandingWalk::free(t_land),
                    seed: 11,
                    n: 12,
                    hero: true,
                },
                &cx,
            );
            let mut seen = std::collections::BTreeSet::new();
            let mut stars = 0;
            for s in sky.live_iter().filter(|s| !s.gold) {
                stars += 1;
                let (x, _) = s.pos(t_rest, false);
                let dx = (x as f32 - origin.0) / cw;
                // The rest pixel is rounded: the walk's stop anywhere within
                // a pixel of it is the star's.
                let ok = [-1.0_f32, 0.0, 1.0]
                    .iter()
                    .any(|px| s.tint == spectrum_snap(landing_field(t_land, dx + px / cw)));
                assert!(
                    ok,
                    "t {t_land}: a fan star {dx:+.2} cells out wears {:06x}, the band's stop \
                     there is {:06x}",
                    s.tint,
                    spectrum_snap(landing_field(t_land, dx))
                );
                seen.insert(s.tint);
            }
            assert!(stars >= 6, "the fan threw only {stars} coloured stars");
            // A 12 px fan spans under a fifth of a sweep at the band's pace:
            // two or three names, not seven.
            assert!(
                seen.len() <= 3,
                "t {t_land}: a 12 px fan wore {} names",
                seen.len()
            );
        }
    }

    /// A sky driven to steady state by `keys` keystrokes at `cps` over `gm`,
    /// and its last frame: the fixture both index pins read (row 4 blank under
    /// a caret on row 5, spine as the engine's integrator would have it).
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

    /// The ground is read by cell, not by scan, measured in point-in-rect and
    /// falloff tests per frame (independent of build lane).
    ///
    /// Every star asks `sky_ground` [`SETTLE_PIXELS`] times. A scan walks the
    /// whole laid slice (up to [`STARDUST_QUAD_BUDGET`]) per query; the index
    /// answers the same (checked by [`SkyIndex::ground`]'s `debug_assert`)
    /// off one cell's candidates, about a sixtieth of the tests on the
    /// 125 cps retina fixture. The bound is a loose tenth so sky changes do
    /// not break it, while a return to scanning lands at 1.0.
    #[test]
    fn a_point_query_reads_one_cell_not_the_whole_sky() {
        for (label, gm, cps) in [
            ("12 cps 1x", geom(), 12.0f32),
            ("125 cps retina", geom_cell(15, 28), 125.0),
        ] {
            GROUND_WORK.with(|w| w.set([0; 4]));
            let (_, sc) = steady_sky(gm, cps, 48);
            let w = GROUND_WORK.with(std::cell::Cell::get);
            let (scan, cell) = (w[0] + w[1], w[2] + w[3]);
            eprintln!(
                "{label:<16} last frame {} quads / {} halos | ground tests: scan {scan} \
                 (quads {} halos {}) -> cell {cell} (quads {} halos {})",
                sc.out.len(),
                sc.halos.len(),
                w[0],
                w[1],
                w[2],
                w[3],
            );
            assert!(scan > 0, "{label}: the sky never priced a star");
            assert!(
                cell * 10 <= scan,
                "{label}: the indexed ground still reads {cell} entries against the scan's \
                 {scan} — the cell index is not narrowing the query"
            );
        }
    }

    /// The indexed ground equals the scan's, pixel for pixel; the ceiling every
    /// star is priced under is read from it. Swept over a real frame's laid
    /// sky (the 125 cps retina fixture): every corner of every entry, the
    /// pixel just outside each edge (where a wrong slot would show), and a
    /// deterministic scatter over the whole effects box.
    #[test]
    fn the_indexed_ground_is_the_scan_s_on_every_pixel_of_a_real_frame() {
        let gm = geom_cell(15, 28);
        let (_, mut sc) = steady_sky(gm, 125.0, 48);
        assert!(
            sc.out.len() > 60 && sc.halos.len() > 8,
            "the fixture laid only {} quads / {} halos",
            sc.out.len(),
            sc.halos.len()
        );
        let mut probes: Vec<(i32, i32)> = Vec::new();
        for q in &sc.out {
            let (x, y, w, h) = (
                i32::from(q.x),
                i32::from(q.y),
                i32::from(q.w),
                i32::from(q.h),
            );
            for p in [
                (x, y),
                (x + w - 1, y),
                (x, y + h - 1),
                (x + w - 1, y + h - 1),
                (x - 1, y),
                (x + w, y),
                (x, y - 1),
                (x, y + h),
            ] {
                probes.push(p);
            }
        }
        for h in &sc.halos {
            let (x, y, w, ht) = (
                i32::from(h.x),
                i32::from(h.y),
                i32::from(h.w),
                i32::from(h.h),
            );
            for p in [
                (i32::from(h.cx), i32::from(h.cy)),
                (x, y),
                (x + w - 1, y + ht - 1),
                (x, y - 1),
                (x, y + ht),
            ] {
                probes.push(p);
            }
        }
        // A deterministic scatter over the whole box — the head band above
        // the grid and the rows no star reached included.
        for k in 0..4_000u32 {
            let hx = mix32(k ^ 0x5F35_6495);
            let hy = mix32(k ^ 0x27D4_EB2D);
            probes.push((
                gm.fx_left() + (hx % (gm.fx_right() - gm.fx_left()).max(1) as u32) as i32,
                gm.fx_top() - 4 + (hy % (gm.fx_bot() + 8 - gm.fx_top()).max(1) as u32) as i32,
            ));
        }
        let mut index = SkyIndex::default();
        index.open(gm, (0, 0));
        {
            let frame = sc.frame();
            index.ingest(&frame);
        }
        let frame = sc.frame();
        let bg = cfg(true).theme_bg;
        let mut lit = 0usize;
        for (x, y) in probes {
            let want = sky_ground(bg, &*frame.out, &*frame.halos, x, y);
            let got = index.ground(bg, &frame, x, y);
            assert_eq!(
                got, want,
                "the indexed ground at ({x}, {y}) is not the scan's"
            );
            lit += usize::from(want != bg & 0x00FF_FFFF);
        }
        assert!(
            lit > 100,
            "only {lit} of the swept pixels had any sky on them — the sweep proves nothing"
        );
    }

    /// The slot hash is injective over every box the cap allows, which is what
    /// lets the index add light at all: two cells of one box on one chain
    /// would count that lay twice. A shared chain between different lays only
    /// costs one more `quad_covers`. [`SKY_INDEX_WIDE_CELLS`] rules the
    /// collision out.
    ///
    /// The sweep covers the whole plane: with `k` a cell's pre-shift hash, the
    /// other cell hashes to `k + t`, `t = dx·A + dy·B`, and
    /// `slot(k + t) = (slot(k) + mid(t) + c) mod 1024` where the carry `c`
    /// depends only on `k & 0x1FFF`. `A` is odd, so 8192 consecutive `cx` at
    /// `cy = 0` realise every residue once, and `deltas` is closed under
    /// negation, so every ordered pair of cells of every allowed box is
    /// checked.
    #[test]
    fn the_slot_hash_is_injective_over_every_box_the_cap_allows() {
        /// Every offset two cells of one box can be apart, for a box of at
        /// most `cap` cells — deduplicated, and closed under negation.
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

        // The cap is load-bearing: the `(0, 19)` delta a 1 × 20 box allows
        // does collide on some base cells in the same sweep.
        let doubled = (0..1_i32 << 13)
            .filter(|&cx| SkyIndex::slot((cx, 0)) == SkyIndex::slot((cx, 19)))
            .count();
        assert!(
            doubled > 0,
            "the `(0, 19)` offset collided at none of 8192 base cells, so \
             this arm no longer shows the cap is what makes the law true"
        );
    }

    /// A lay the index cannot file spills it, and the answer survives: past
    /// [`SKY_INDEX_WIDE_CELLS`] every query reads the whole slice, and the two
    /// paths still agree (a lay silently left out of the ground would be the
    /// failure).
    #[test]
    fn a_lay_too_wide_to_file_spills_the_index_and_still_answers() {
        let gm = geom();
        let mut sc = Scratch::default();
        // 40 cells wide at `cw` 9: past the filing bound by a factor of 22.
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
        // And the next frame is indexed again: a spill lasts one frame.
        index.open(gm, (0, 0));
        assert!(!index.spilled, "the spill outlived its frame");
    }

    /// A degenerate geometry still answers: with a zero cell the index
    /// collapses to one cell (a full scan) rather than panicking or dropping
    /// light.
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
}

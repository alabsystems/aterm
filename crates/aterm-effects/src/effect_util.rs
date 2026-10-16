// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor emitters' shared render kit: one copy of the per-cell-row quad
//! pushers, the colour ramps, and the 4-point twinkle star. The two pushers are
//! not interchangeable: [`push_grid_quad`] takes grid-relative px and clamps to
//! the grid interior with `y / ch` row tags, while [`push_fx_rect`] takes
//! window-absolute px and clamps to the effects box with origin-anchored row
//! tags. The wrong one moves an emitter's light and mistags its damage rows.
//!
//! Every caret-anchored emitter is window-absolute. The grid form is only
//! [`push_grid_quad`], whose one caller is `output_streak` (light anchored to a
//! text run, not the caret).

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;

/// Clamp a grid-relative pixel rect to the grid interior and split it into
/// per-cell-row [`GlowQuad`]s (the renderer row-gate and CPU/GPU parity
/// invariant), blend mode chosen by the caller. Callers pass grid px (no
/// `geom.origin_*`) and rows tag `y / ch`.
///
/// A caret-anchored emitter must use [`push_fx_rect`] instead: the caret's light
/// lands in `cursor_glow_add`, whose quads are window px, so a grid-relative
/// producer there matches only at origin (0,0) and is displaced by
/// `(origin_x, origin_y)` in a real window. The one caller is `output_streak`,
/// anchored to a text run.
///
/// `alpha == 0` is additive light; `alpha > 0` selects premultiplied
/// source-over (`src + dst·(1 − a)`), which a light-theme emitter needs because
/// additive light can only brighten.
#[allow(
    clippy::too_many_arguments,
    reason = "the rect + blend mode IS the call; a struct would relocate the same list and cost \
              every existing caller a construction"
)]
pub(crate) fn push_grid_quad(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
    alpha: u8,
) {
    // Zero light is skippable only when ADDITIVE: a source-over quad whose
    // premultiplied colour rounds to black still darkens the cell by `alpha`.
    if w <= 0 || h <= 0 || (premul == 0 && alpha == 0) {
        return;
    }
    let gw = (geom.cols * geom.cw) as i32;
    let gh = (geom.rows * geom.ch) as i32;
    let x0 = x.max(0);
    let x1 = (x + w).min(gw);
    let y0 = y.max(0);
    let y1 = (y + h).min(gh);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let ch = geom.ch as i32;
    let mut yy = y0;
    while yy < y1 {
        let row = yy / ch;
        let band_end = ((row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: premul,
            alpha,
            color2: premul,
            alpha2: alpha,
        });
        yy = band_end;
    }
}

/// Push a pixel rect of premultiplied light in window px, clamped to the
/// effects box (grid + head band, identity at `head == 0`) and split into
/// per-cell-row [`GlowQuad`]s with origin-anchored row damage tags. Callers
/// pass window px (`geom.origin_*` applied); grid-relative emitters use
/// [`push_grid_quad`].
pub(crate) fn push_fx_rect(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
) {
    if premul == 0 {
        return;
    }
    // ADDITIVE light — this emitter has no other mode (see
    // [`GlowQuad::alpha`]).
    push_fx_quad(out, geom, x, y, w, h, premul, 0);
}

/// [`push_fx_rect`] with the blend mode left to the caller: the one copy of the
/// window-px band walk. `alpha == 0` is additive; `alpha > 0` is that
/// coverage as source-over opacity (`src + dst·(1 − a)`), for light-theme ink.
#[allow(
    clippy::too_many_arguments,
    reason = "the rect + blend mode IS the call, as in `push_grid_quad`"
)]
pub(crate) fn push_fx_quad(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
    alpha: u8,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    debug_assert!(
        geom.fx_right() <= i32::from(u16::MAX) && geom.fx_bot() <= i32::from(u16::MAX),
        "effects-box pixel extent exceeds u16 GlowQuad range"
    );
    // Clamp to the EFFECTS BOX (grid + head band) — identity-exact at head 0;
    // below-grid/side bands would only be skipped by the renderers' row gates.
    let x0 = x.max(geom.fx_left());
    let x1 = (x + w).min(geom.fx_right());
    let y0 = y.max(geom.fx_top());
    let y1 = (y + h).min(geom.fx_bot());
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = i32::from(geom.origin_y);
    let mut yy = y0;
    while yy < y1 {
        // Grid-row damage hint anchored at origin_y (above-grid bands tag row 0).
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: premul,
            alpha,
            color2: premul,
            alpha2: alpha,
        });
        yy = band_end;
    }
}

/// Per-channel linear interpolation between two `0x00RRGGBB` colours, `t` 0..1.
#[inline]
pub(crate) fn lerp_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |sh: u32| {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        ((ca + (cb - ca) * t) + 0.5) as u32
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// Clamped per-channel RGB mix (`t` from a → b), rounded to nearest. Not
/// [`lerp_rgb`]: its `+ 0.5`-and-truncate rounding can differ in the last ulp.
#[inline]
pub(crate) fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |sh: u32| {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        ((ca + (cb - ca) * t).round().clamp(0.0, 255.0) as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
}

/// `a + (b - a) t`, `t` clamped to 0..1.
#[inline]
pub(crate) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// The fire ramp's stops: deep red → orange → yellow → near-white core.
///
/// Module-level `const` because the ramps run per particle per frame, and a
/// local table is re-materialized on the stack whenever inlining declines.
/// [`ramp_5stop`] relies on this table's shape (positions strictly ascending,
/// first exactly 0.0, last exactly 1.0), pinned by
/// `ramp_stop_tables_hold_the_branchless_contract`.
const FIRE_STOPS: [(f32, u32); 5] = [
    (0.0, 0x002A_0000),
    (0.25, 0x008B_1A00),
    (0.5, 0x00E0_4A00),
    (0.75, 0x00FF_B020),
    (1.0, 0x00FF_F0C0),
];

/// The ocean ramp's stops: deep-sea abyss → open-ocean blue → turquoise →
/// vivid aqua crest → foam. Saturated and green-leaning through the midband
/// (pale sky-cyan reads as ice); foam white only at the very crest. Same shape
/// contract as [`FIRE_STOPS`].
const WATER_STOPS: [(f32, u32); 5] = [
    (0.0, 0x0005_2C48),
    (0.35, 0x000E_66B4),
    (0.65, 0x0014_AAC8),
    (0.85, 0x0032_DCDE),
    (1.0, 0x00C2_F2F5),
];

/// The shared 5-stop ramp core: a branchless window select feeding
/// `(t - t0) / (t1 - t0)` + [`lerp_rgb`] on the same f32 stop values, so every
/// output is bit-equal to a windowed linear walk (swept against that oracle in
/// `ramp_rewrite_is_bit_identical_to_the_stop_walk`; its `--ignored` twin runs
/// all 2^32 bit patterns for palette retunes).
///
/// Both ramps run per particle per frame in the fire/water emit paths. The
/// window index is the count of interior stop positions strictly below `t`:
/// three compares summed, no table, iterator or early-exit chain, matching
/// the walk's "first window with `t <= t1`", boundaries included.
// Not `.clamp()`: `f32::clamp` propagates NaN. `f32::min`/`max` return the other
// operand for NaN, so clamping min-first pins NaN to 1.0, and the lerp at
// `local == 1.0` lands exactly on the crest colour (channel bytes are exact in
// f32), so a poisoned `t` keeps the crest without a NaN branch. Same idiom as
// `aterm_render::sdr_glow_budget`.
#[allow(clippy::manual_clamp)]
#[inline]
fn ramp_5stop(stops: &[(f32, u32); 5], t: f32) -> u32 {
    let t = t.min(1.0).max(0.0);
    let idx =
        usize::from(t > stops[1].0) + usize::from(t > stops[2].0) + usize::from(t > stops[3].0);
    let (t0, c0) = stops[idx];
    let (t1, c1) = stops[idx + 1];
    // No `t1 > t0` divide guard: positions strictly ascend by table contract
    // (tested).
    lerp_rgb(c0, c1, (t - t0) / (t1 - t0))
}

/// Black-body-ish FIRE ramp, `t` 0 (cool, deep red) → 1 (hot, white-yellow) —
/// the one palette behind the aurora's fire comet/curtain.
#[inline]
pub(crate) fn fire_ramp(t: f32) -> u32 {
    ramp_5stop(&FIRE_STOPS, t)
}

/// Ocean ramp, `t` 0 (deep navy) → 1 (bright cyan crest, just shy of foam):
/// the water palette behind the aurora's fluid wake and the droplet nucleus.
#[inline]
pub(crate) fn water_ramp(t: f32) -> u32 {
    ramp_5stop(&WATER_STOPS, t)
}

/// The twinkle's palette: the white/gold pair of the typing starfield and glide
/// sparkles (the star itself is colour-agnostic, so the rainbow kitty landing
/// burst can draw it in a band hue).
///
/// The one gold: a sparkle's warmth is a family property, so call sites wanting
/// the warm half ask for `twinkle_rgb(true)`. (The fire ramp's `0x00FF_F0C0`
/// crest and `cursor_glow`'s `FRESH_INK_WARM` are not in this family: one is a
/// black-body stop, the other a veil tint.)
#[inline]
pub(crate) const fn twinkle_rgb(gold: bool) -> u32 {
    if gold { 0x00FF_E9A8 } else { 0x00FF_FFFF }
}

// ─── the sparkle family ─────────────────────────────────────────────────────
//
// One silhouette, one arm family, one pulse rule. Every 4-point twinkle this
// crate draws, additive on dark ([`push_twinkle_star`]) or source-over on light
// (`cursor_glow`'s `push_twinkle_over`), is the same mark, defined by the
// numbers here. A star's context may choose its colour, its size (from the
// named ratios below) and its phase, nothing else.

/// Silhouette: the arms' half-thickness at the base as a fraction of the arm's
/// half-length. Both rasterizers take arm mass from it: the source-over star as
/// each arm's falloff minor radius, the additive star as the integer bar
/// thickness (`2·arm·STAR_WAIST` rounded, floored at 1 px).
///
/// At 0.10 the waist rounds to the 1 px hairline for every ordinary arm and
/// widens only on hero grains; a thick waist reads as a typographic `+`. The
/// arm starts at this thickness and thins to a point (see [`STAR_TIP_COV`]).
pub(crate) const STAR_WAIST: f32 = 0.10;

// ─── the taper ──────────────────────────────────────────────────────────────
//
// A sparkle comes to a point (owner, 2026-08-10). An arm of constant thickness
// held to a square end, crossed with its twin over a square nucleus, is a `+`,
// however thin.
//
// The target silhouette is `aterm-render`'s `needle_star` (the word decorations'
// sparkle), whose arm half-width shrinks to nothing along its length. A cursor
// star's arm is only ~3-8 px long with a 1-2 px base, so a narrowing outline
// cannot cross a pixel boundary; at this size a needle is a coverage ramp: a
// bright body at the crossing and a short run of dimmer, thinner spans out to
// each point.
//
// The additive arm ramps per-span coverage (and span thickness once the arm has
// room); the source-over arm stacks a shorter body lay over a full-length tip
// lay, so ink is heaviest at the crossing and thinnest at the points.

/// The taper: the fraction of an arm's half-length drawn at full coverage as
/// one span through the crossing; beyond it the arm is the point. Less loses
/// the solid centre (at `arm` 3-4 the body would be one pixel); more leaves too
/// short a run to ramp over. 0.42 gives the ramp one more pixel at starfield
/// sizes than 0.5.
pub(crate) const STAR_TAPER_BODY: f32 = 0.42;

/// The taper: coverage at the point as a fraction of the requested coverage,
/// ramping linearly from full at the body's end. 0.22 visibly dissolves the tip
/// rather than ending it, while keeping the arm's reach (arm length is what the
/// size ladder [`STAR_ARM_FINE`]..[`STAR_ARM_HERO`] spends its range on).
pub(crate) const STAR_TIP_COV: f32 = 0.22;

/// The taper: the most spans one point is rasterized with (per side, per arm).
/// One span per pixel of its run up to this bound. The additive star is a hot
/// path (one per starred ribbon cell per frame) and each span costs 4 quads
/// (two arms × two sides), so at 3 a star costs at most 15.
pub(crate) const STAR_TAPER_STEPS: i32 = 3;

/// How many coincident source-over lays `cursor_glow`'s `push_twinkle_over`
/// stacks at the crossing: a TIP lay and a BODY lay per arm, plus the nucleus.
pub(crate) const STAR_OVER_LAYS: usize = 5;

/// How many lays the star's centre must still composite to. The untapered plus
/// laid three coincident marks, and `stacked_ink_alpha` prices the round
/// stardust grain against `1 − (1 − a)³` on that basis; the taper adds lays to
/// the arms, not the middle, so the light star solves a per-lay alpha that
/// reproduces this stack.
pub(crate) const STAR_OVER_CENTRE_LAYS: f32 = 3.0;

/// The taper's coverage multiplier at `u` (the fraction of the way from the
/// body's end to the point), shared by both rasterizers.
#[inline]
pub(crate) fn star_taper_cov(u: f32) -> f32 {
    1.0 - (1.0 - STAR_TIP_COV) * u.clamp(0.0, 1.0)
}

/// Silhouette: the nucleus half-extent as a fraction of the arm's half-length.
/// Source-over draws it; on additive the crossing supplies part of it and
/// [`STAR_CORE_ADD`] completes it.
pub(crate) const STAR_CORE: f32 = 0.16;

/// The additive nucleus's share of the star's coverage. The two arms already
/// stack to 2x at the crossing, so a full-coverage core would reach 3x and clip
/// a gold star to white; at 0.35 crossing plus core read as the same nucleus the
/// light star draws.
pub(crate) const STAR_CORE_ADD: f32 = 0.35;

/// SILHOUETTE — the diagonal GLINT dots' offset from the centre, as a fraction of
/// the arm's half-length (the classic four-point sparkle's secondary points).
pub(crate) const STAR_GLINT: f32 = 0.5;

/// The glints' share of the star's coverage — dim by design, so text stays
/// legible under a starfield.
pub(crate) const STAR_GLINT_COV: f32 = 1.0 / 3.0;

/// The one arm scale: a star's arm half-length as a fraction of cell height.
/// Every emitter sizes stars as `STAR_ARM` times one of the named ratios below
/// (see [`star_arm`]).
pub(crate) const STAR_ARM: f32 = 0.14;

/// Arm ratio, starfield grain: the ribbon's per-cell stars at rest, the landing
/// sparkles, the flying shower's cold end. The smallest mark that still reads as
/// a star.
pub(crate) const STAR_ARM_FINE: f32 = 0.75;

/// Arm ratio, family default: shooting-star and glide heads, Beam's stardust,
/// the comet's glint, every star at full typing spine.
pub(crate) const STAR_ARM_STD: f32 = 1.0;

/// Arm ratio — a HERO grain: Sparkle's large pour grains and the erase poof's
/// one surviving plus (which multiplies this again by `erase_hero_arm`).
pub(crate) const STAR_ARM_HERO: f32 = 1.4;

/// Arm ratio, the ink tax: source-over ink buys presence with area where
/// additive light uses coverage, so a light-theme star is its dark twin's ratio
/// times this.
pub(crate) const STAR_ARM_INK: f32 = 1.55;

/// The ladder's shape, checked at build time: the rungs stay ordered and short.
/// `the_star_arm_ladder_is_ordered_and_derived` checks that [`star_arm`] realizes
/// each rung.
const _: () = {
    assert!(
        STAR_ARM_FINE < STAR_ARM_STD && STAR_ARM_STD < STAR_ARM_HERO,
        "the star arm ladder must stay ordered fine < std < hero"
    );
    // Before the erase HERO band's own documented `erase_hero_arm` multiplier.
    assert!(
        STAR_ARM_HERO / STAR_ARM_FINE <= 2.0,
        "the family's own star sizes must stay inside 2x end to end"
    );
    assert!(
        STAR_ARM_INK > 1.0,
        "a source-over star must out-size its additive twin"
    );
};

/// A star's arm half-length in px: the cell height, [`STAR_ARM`], and ONE of the
/// named ratios. The only way an emitter is allowed to pick a star's size.
#[inline]
pub(crate) fn star_arm(cell_h: f32, ratio: f32) -> f32 {
    cell_h * STAR_ARM * ratio
}

/// The one float→integer arm conversion for the additive star: truncate, then
/// floor at the 1 px arm [`push_twinkle_star`] needs.
///
/// A plus's mass is derived from its integer arm ([`star_waist_px`] and
/// [`star_core_px`] round `2·arm·ratio`), so one extra pixel of arm flips a
/// 1 px hairline to a 2 px bar. Rounding instead of truncating would turn the
/// hero grain at a 40 px cell (7.84 px of arm) into a 2 px bar on a 3 px
/// nucleus.
#[inline]
pub(crate) fn star_arm_px(arm: f32) -> i32 {
    (arm as i32).max(1)
}

/// The stardust rule (owner, 2026-08-08): a population's body is round stardust
/// (a tiny radial mote), and the 4-point star is an accent dealt to at most one
/// particle in [`STAR_ACCENT_DEN`], decided from the particle's already-stored
/// seed so no RNG stream shifts and no grain changes shape mid-flight. Singular
/// gestures (the erase poof's one plus, the jump-landing starburst, the
/// momentum shower's one shooting-star tip) sit outside the deal.
///
/// "Singular" is a count, not a name: the ribbon starfield's gold (about 6 % of
/// starred cells) and the fast-glide shooting star's head (one per streak, but
/// a streak per qualifying frame) are populations, so both are dealt.
///
/// At 1-in-16 (6.25 %) the nearest plus on a busy ribbon is a short word away,
/// reading as punctuation on the dust rather than texture, while some crosses
/// remain. This constant reaches every dealt population on both themes: the
/// dark and light ribbon starfields, the Beam/Comet/Sparkle wakes, the erase
/// poof, and the fresh-ink spark. Measured by `cursor_glow`'s
/// `stardust_is_the_body_the_plus_is_the_accent` (dark) and
/// `the_fresh_ink_spark_is_stardust_with_a_dealt_plus` (light)
/// (owner, 2026-08-09).
pub(crate) const STAR_ACCENT_DEN: u32 = 16;

/// Deal a stored particle seed into the accent (true → the 4-point star) or the
/// stardust body (false → a round mote): 1-in-16, 6.25 %.
#[inline]
pub(crate) fn star_accent(seed: u32) -> bool {
    seed.is_multiple_of(STAR_ACCENT_DEN)
}

/// Stardust mote radius, px: seeded so no two motes match, floored so the
/// smallest is still a lit point. Slightly smaller than the erase poof's
/// round body — the trail is dust, the poof is debris.
#[inline]
pub(crate) fn dust_r(cell_h: f32, seed01: f32) -> f32 {
    (cell_h * (0.07 + 0.05 * seed01)).max(1.3)
}

/// The plus's composited centre as a multiple of the coverage its emitter asked
/// for. [`push_twinkle_star`] lays three coincident additive marks at the
/// crossing (two arms at `cov`, the nucleus at [`STAR_CORE_ADD`] of it), so the
/// centre pixel carries `2.35 · cov`. A round mote replacing a plus must be
/// priced against that, or it lands 2.35x darker; the light arm does the same
/// with `1-(1-a)^3`.
pub(crate) const STAR_STACK_ADD: f32 = 2.0 + STAR_CORE_ADD;

/// The one pulse rule: angular frequency (rad/s) of every twinkling star.
///
/// WCAG 2.3.1: this crate certifies a 3.2 Hz general-flash bound. The envelope
/// is rectified (`|sin|`), peaking twice per sine cycle, so the bound applies
/// to `ω/π`. At 9 rad/s that is [`TWINKLE_FLASH_HZ`] = 2.86 Hz.
pub(crate) const TWINKLE_OMEGA: f32 = 9.0;

/// THE ONE PULSE LAW — the envelope's floor. A star DIMS, it never blinks out:
/// a sparkle that reaches zero reads as a dropped frame, not as a twinkle.
pub(crate) const TWINKLE_FLOOR: f32 = 0.55;

/// THE ONE PULSE LAW — the envelope's depth. `TWINKLE_FLOOR + TWINKLE_DEPTH`
/// is 1.0: the law scales a star's own coverage and never exceeds it.
pub(crate) const TWINKLE_DEPTH: f32 = 0.45;

/// Where in the envelope's range a star counts as at its peak, the gate the
/// diffraction-glint accents fire on: 0.92 puts the glint on for ~26 % of each
/// pulse.
pub(crate) const TWINKLE_GLINT_FRAC: f32 = 0.92;

/// The family's flash rate in Hz: `ω/π`, since the rectified envelope peaks
/// twice per sine cycle. Held under [`TWINKLE_FLASH_BOUND_HZ`] at build time
/// and measured off the envelope by
/// `twinkle_law_stays_under_the_photosensitivity_bound`.
pub(crate) const TWINKLE_FLASH_HZ: f32 = TWINKLE_OMEGA / std::f32::consts::PI;

/// WCAG 2.3.1's general-flash threshold in Hz: three flashes in any second
/// fails, so 3.2 is the bound certified against.
///
/// Also used by the kitty-command trick flash (`trick_flash.rs`), whose
/// luminance-reversal rate is const-asserted under it; its measured test lives
/// with that emitter (`the_flash_stays_under_the_photosensitivity_bound`).
pub(crate) const TWINKLE_FLASH_BOUND_HZ: f32 = 3.2;

/// The photosensitivity bound is checked at build time: retuning
/// [`TWINKLE_OMEGA`] past it stops the build. (The test still measures the rate
/// off the real envelope, catching a rectification change.)
const _: () = assert!(
    TWINKLE_FLASH_HZ <= TWINKLE_FLASH_BOUND_HZ,
    "the unified twinkle flashes over the WCAG 2.3.1 general-flash bound"
);

/// The one twinkle: a star's brightness multiplier at `age_s` seconds of life
/// on its own `phase` (radians; seed per star so a field blinks out of step).
/// Driven by age, never a frame counter, so the rate is the same at any refresh
/// rate. Pinned by `sparkle_twinkle_is_frame_rate_independent`.
#[inline]
pub(crate) fn twinkle_env(age_s: f32, phase: f32) -> f32 {
    TWINKLE_FLOOR + TWINKLE_DEPTH * (age_s * TWINKLE_OMEGA + phase).sin().abs()
}

/// Whether this twinkle is at its peak, when a star throws its diffraction
/// glints: a fraction of the envelope's own range, so a depth retune cannot
/// change the glint duty.
#[inline]
pub(crate) fn twinkle_peak(env: f32) -> bool {
    env >= TWINKLE_FLOOR + TWINKLE_DEPTH * TWINKLE_GLINT_FRAC
}

/// The additive star's integer arm thickness (px) for half-length `arm`, from
/// [`STAR_WAIST`], floored at the 1 px hairline, so only hero grains carry
/// visible mass.
#[inline]
pub(crate) fn star_waist_px(arm: i32) -> i32 {
    ((2.0 * arm as f32 * STAR_WAIST).round() as i32).max(1)
}

/// The additive star's integer NUCLEUS side (px), from the shared [`STAR_CORE`].
#[inline]
pub(crate) fn star_core_px(arm: i32) -> i32 {
    ((2.0 * arm as f32 * STAR_CORE).round() as i32).max(1)
}

/// The additive star's integer body half-length (px), the full-coverage run of
/// an arm, from [`STAR_TAPER_BODY`]. Floored at 1 and capped at the arm, so a
/// 1 px arm is all body.
#[inline]
pub(crate) fn star_body_px(arm: i32) -> i32 {
    ((arm as f32 * STAR_TAPER_BODY).round() as i32).clamp(1, arm)
}

/// The one star: a 4-point twinkle, a horizontal and a vertical needle crossing
/// at `(sx, sy)` on a small nucleus, plus four dim diagonal glint dots when
/// `gold`. Each needle is a full-coverage body through the crossing
/// ([`STAR_TAPER_BODY`]) running into points that dim toward [`STAR_TIP_COV`]
/// and thin with them (see the taper notes above).
///
/// Every 4-point star goes through here (rainbow kitty typing starfield, glide
/// stars, jump-landing burst, shooting-star heads, Beam's stardust, the comet's
/// glint, Sparkle's star grains), so only colour and size vary with context.
/// Waist ([`STAR_WAIST`]), nucleus ([`STAR_CORE`]) and glint offset
/// ([`STAR_GLINT`]) are shared with `cursor_glow`'s source-over
/// `push_twinkle_over`. This one is hard-edged and additive: the crossing
/// supplies part of the nucleus, and the core rect at [`STAR_CORE_ADD`]
/// finishes it without clipping to white.
///
/// Window-absolute, via [`push_fx_rect`]. Returns `false` when the quad budget
/// ran out.
#[allow(
    clippy::too_many_arguments,
    reason = "output + geometry + centre + arm + coverage + glint + colour + budget; \
              the call sites are the emitters' whole star kit and share this one shape"
)]
pub(crate) fn push_twinkle_star(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    sx: i32,
    sy: i32,
    arm: i32,
    cov: u8,
    gold: bool,
    color: u32,
    max_quads: usize,
) -> bool {
    if cov == 0 || arm < 1 {
        return true;
    }
    let star = premul_rgb(color, cov);
    // The body at the shared waist: the arm's full-coverage run through the
    // crossing. An even thickness is biased up-left by the integer half so the
    // centre stays on `(sx, sy)`; at the 1 px floor this is a hairline.
    let t = star_waist_px(arm);
    let half = (t - 1) / 2;
    let b = star_body_px(arm);
    push_fx_rect(out, geom, sx - b, sy - half, 2 * b + 1, t, star); // horizontal
    if out.len() >= max_quads {
        return false;
    }
    push_fx_rect(out, geom, sx - half, sy - b, t, 2 * b + 1, star); // vertical
    if out.len() >= max_quads {
        return false;
    }
    // Nucleus: the crossing supplies half, this completes it. Drawn before the
    // points so a budget-truncated star loses its faint tips, not its centre.
    let c = star_core_px(arm);
    let ch = (c - 1) / 2;
    let core = premul_rgb(color, (f32::from(cov) * STAR_CORE_ADD) as u8);
    push_fx_rect(out, geom, sx - ch, sy - ch, c, c, core);
    // The points: each arm runs from the body to the tip in equal spans, each
    // dimmer (and, with thickness to spare, thinner) than the last. Two spans
    // per step, one per side of the crossing.
    let run = arm - b;
    let steps = run.clamp(0, STAR_TAPER_STEPS);
    for k in 0..steps {
        let d0 = b + run * k / steps; // last px of the previous span
        let d1 = b + run * (k + 1) / steps; // last px of this one
        // The ramp runs over the POINT — full where the body ends, [`STAR_TIP_COV`]
        // at the tip — sampled at this span's midpoint.
        let ramp = star_taper_cov(((d0 + d1) as f32 * 0.5 - b as f32) / run as f32);
        let tip = premul_rgb(color, (f32::from(cov) * ramp) as u8);
        if tip == 0 {
            break;
        }
        let tt = ((t as f32 * ramp).round() as i32).max(1);
        let th = (tt - 1) / 2;
        let len = d1 - d0;
        // `d0 + 1` runs right/down from the body; `-d1` mirrors it.
        for off in [d0 + 1, -d1] {
            if out.len() >= max_quads {
                return false;
            }
            push_fx_rect(out, geom, sx + off, sy - th, len, tt, tip); // horizontal
            if out.len() >= max_quads {
                return false;
            }
            push_fx_rect(out, geom, sx - th, sy + off, tt, len, tip); // vertical
        }
    }
    if gold {
        let d = ((arm as f32 * STAR_GLINT).round() as i32).max(1);
        let dim = premul_rgb(color, (f32::from(cov) * STAR_GLINT_COV) as u8);
        for (ox, oy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            if out.len() >= max_quads {
                return false;
            }
            push_fx_rect(out, geom, sx + ox, sy + oy, 1, 1, dim);
        }
    }
    true
}

/// Channel-weighted lit-pixel sum of a quad batch — the effect test suites'
/// shared brightness metric.
#[cfg(test)]
pub(crate) fn ink(out: &[GlowQuad]) -> u64 {
    out.iter()
        .map(|q| {
            let px = (q.w as u64) * (q.h as u64);
            px * (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff)) as u64
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 96,
            head: 0,
        }
    }

    /// A source-over quad whose premultiplied colour rounds to black is a
    /// shadow, not nothing: `src + dst·(1 − a)` with `src == 0` still darkens
    /// the cell by `a`, so it must not be dropped (a light-theme tail pulling its
    /// hue toward black would get a one-cell hole). Only additive zero light
    /// (`alpha == 0`) may be skipped.
    #[test]
    fn a_source_over_quad_that_rounds_to_black_still_darkens() {
        let shade = 0x0003_0201;
        let a = 20;
        assert_eq!(
            premul_rgb(shade, a),
            0,
            "fixture: the shade must round to black"
        );

        let mut out = Vec::new();
        push_grid_quad(&mut out, geom(), 8, 16, 8, 16, premul_rgb(shade, a), a);
        assert_eq!(out.len(), 1, "a black source-over quad must be pushed");
        assert_eq!((out[0].color, out[0].alpha), (0, a));

        let mut additive = Vec::new();
        push_grid_quad(&mut additive, geom(), 8, 16, 8, 16, 0, 0);
        assert!(
            additive.is_empty(),
            "zero ADDITIVE light adds nothing and is skipped"
        );
    }

    /// One pulse rule, inside the certified flash bound. The rectification is
    /// part of it: `|sin|` peaks twice per sine cycle, so the WCAG 2.3.1 budget
    /// is spent at `ω/π`.
    #[test]
    fn twinkle_law_stays_under_the_photosensitivity_bound() {
        // The declared rate is bounded at build time. This measures that the
        // declaration matches the envelope the emitters run: count bright peaks
        // over ten seconds (dropping the rectification would halve the count).
        let (dt, span) = (1.0f32 / 240.0, 10.0f32);
        let n = (span / dt) as usize;
        let mut peaks = 0usize;
        let mut high = false;
        for i in 0..n {
            let hot = twinkle_env(i as f32 * dt, 0.0) >= TWINKLE_FLOOR + TWINKLE_DEPTH * 0.9;
            if hot && !high {
                peaks += 1;
            }
            high = hot;
        }
        let hz = peaks as f32 / span;
        assert!(
            (hz - TWINKLE_FLASH_HZ).abs() < 0.2,
            "measured {hz} Hz against a declared {TWINKLE_FLASH_HZ} Hz"
        );
    }

    /// A star DIMS, it never blinks out, and the law never brightens a star past
    /// the coverage its emitter asked for.
    #[test]
    fn twinkle_law_dims_between_the_floor_and_unity() {
        assert!((TWINKLE_FLOOR + TWINKLE_DEPTH - 1.0).abs() < 1e-6);
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for i in 0..4000 {
            let v = twinkle_env(i as f32 * 0.001, 0.37);
            assert!(
                (TWINKLE_FLOOR..=1.0).contains(&v),
                "envelope left its range: {v}"
            );
            lo = lo.min(v);
            hi = hi.max(v);
        }
        assert!(
            lo < TWINKLE_FLOOR + 0.01,
            "the envelope never reaches its floor"
        );
        assert!(hi > 0.99, "the envelope never reaches full");
    }

    /// The glint gate is a fraction of the envelope's range, so a depth retune
    /// cannot change how often a star glints (~26 % duty).
    #[test]
    fn twinkle_glint_gate_holds_its_duty() {
        let n = 20_000;
        let hits = (0..n)
            .filter(|i| twinkle_peak(twinkle_env(*i as f32 * 0.0005, 0.0)))
            .count();
        let duty = hits as f32 / n as f32;
        assert!(
            (0.20..0.32).contains(&duty),
            "glint duty drifted to {duty} — the family's stars stopped agreeing on 'brightest instant'"
        );
    }

    /// The accent deal is 1-in-[`STAR_ACCENT_DEN`], measured over a uniform
    /// seed sweep. The plus must stay a small minority and must not vanish.
    #[test]
    fn the_stardust_accent_is_a_small_dealt_minority() {
        let n = 16_000u32;
        let hits = (0..n).filter(|s| star_accent(*s)).count();
        let share = hits as f32 / n as f32;
        // Exact over a contiguous sweep: `seed % DEN == 0` partitions perfectly.
        assert!(
            (share - 1.0 / STAR_ACCENT_DEN as f32).abs() < 1e-6,
            "the deal measured {share}, not 1-in-{STAR_ACCENT_DEN}"
        );
        assert!(
            hits > 0,
            "the accent must SURVIVE — it is the family's grace note"
        );
        assert!(
            share <= 0.07,
            "the plus is back to texture at {:.1} % of the population",
            share * 100.0
        );
    }

    /// The arm conversion truncates, and that matters: at every shipping cell
    /// height the family's heaviest named grain is a hairline under truncation,
    /// and somewhere in that band it is not under rounding.
    #[test]
    fn the_one_arm_conversion_truncates_and_floors() {
        assert_eq!(
            star_arm_px(7.84),
            7,
            "the conversion must truncate, not round"
        );
        assert_eq!(star_arm_px(7.0), 7);
        // …and floors, so a tiny grain still draws a mark rather than nothing.
        assert_eq!(star_arm_px(0.2), 1);
        assert_eq!(star_arm_px(0.0), 1);
        let mut rounding_would_fatten = 0;
        for ch in [24.0_f32, 28.0, 32.0, 36.0, 40.0] {
            let f = star_arm(ch, STAR_ARM_HERO);
            let arm = star_arm_px(f);
            assert!(arm >= 2, "vacuous: no arm to measure at ch {ch}");
            assert_eq!(
                star_waist_px(arm),
                1,
                "the family's heaviest named grain is {} px thick at ch {ch}",
                star_waist_px(arm)
            );
            if star_waist_px((f.round() as i32).max(1)) > 1 {
                rounding_would_fatten += 1;
            }
        }
        // Non-vacuous: rounding and truncating must disagree somewhere in the
        // shipping band.
        assert!(
            rounding_would_fatten > 0,
            "no shipping cell height distinguishes truncation from rounding — \
             this rule would be pinning nothing"
        );
    }

    /// One silhouette, additive half: every proportion (arm length and waist,
    /// nucleus, glint offset and dimming) comes from the shared constants. The
    /// source-over half is checked by `cursor_glow`'s
    /// `a_sparkle_is_the_same_mark_on_both_grounds`.
    #[test]
    fn the_additive_star_draws_the_family_silhouette() {
        let g = geom();
        let (sx, sy, arm, cov) = (100i32, 40i32, 10i32, 200u8);
        let mut out = Vec::new();
        assert!(push_twinkle_star(
            &mut out,
            g,
            sx,
            sy,
            arm,
            cov,
            true,
            0x00FF_FFFF,
            4096
        ));
        // THE BODY: the arm's full-coverage run through the crossing, at the
        // shared waist and the shared body fraction.
        let t = star_waist_px(arm);
        assert_eq!(t, (2.0 * arm as f32 * STAR_WAIST).round() as i32);
        let b = star_body_px(arm);
        assert_eq!(b, (arm as f32 * STAR_TAPER_BODY).round() as i32);
        let span = |q: &GlowQuad| (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
        let full = premul_rgb(0x00FF_FFFF, cov);
        let body = out
            .iter()
            .find(|q| span(q) == (sx - b, sx + b + 1) && q.color == full)
            .expect("a horizontal body of 2*body+1 at full coverage");
        assert_eq!(
            i32::from(body.h),
            t,
            "the horizontal body left the shared waist"
        );
        // The arm still reaches `arm` px either side: the taper dims the point,
        // it does not shorten the star. Measured as the union of the row's lit
        // columns, so body and points must be contiguous.
        let full_span = 2 * arm + 1;
        let row: Vec<_> = out
            .iter()
            .filter(|q| i32::from(q.y) <= sy && sy < i32::from(q.y) + i32::from(q.h))
            .collect();
        let lit = |x: i32| row.iter().any(|q| span(q).0 <= x && x < span(q).1);
        assert!(
            (sx - arm..=sx + arm).all(lit) && !lit(sx - arm - 1) && !lit(sx + arm + 1),
            "the tapered arm does not span exactly {full_span} px"
        );
        // The points: every span past the body is dimmer than the body, dimming
        // monotonically outward, ending near the shared tip coverage (read off
        // the right-hand point).
        let mut point: Vec<_> = row
            .iter()
            .filter(|q| span(q).0 > sx + b)
            .map(|q| (span(q).0, (q.color >> 16) & 0xff))
            .collect();
        point.sort_unstable();
        assert!(!point.is_empty(), "the arm came to a square end");
        let mut prev = u32::from(cov);
        for (x, c) in &point {
            assert!(
                *c < prev,
                "the point does not taper: {c} at x+{} under a previous {prev}",
                x - sx
            );
            prev = *c;
        }
        // Spans are sampled at their midpoints, so the outermost never lands
        // exactly on the tip coverage; it must sit in the ramp's bottom half.
        let tip = f32::from(cov) * star_taper_cov(1.0);
        let midway = f32::from(cov) * star_taper_cov(0.5);
        assert!(
            (tip..=midway).contains(&(prev as f32)),
            "the outermost span carries {prev}, outside the ramp's bottom half \
             ({tip:.0}..={midway:.0}) — the point is not reaching the shared tip"
        );
        // Nucleus, at the shared core extent and its additive share.
        let c = star_core_px(arm);
        assert_eq!(c, (2.0 * arm as f32 * STAR_CORE).round() as i32);
        let core = premul_rgb(0x00FF_FFFF, (f32::from(cov) * STAR_CORE_ADD) as u8);
        assert!(
            out.iter().any(|q| i32::from(q.w) == c && q.color == core),
            "no nucleus at the shared core extent"
        );
        // Glints: four, on the diagonals, at the shared offset and dimming.
        let d = ((arm as f32 * STAR_GLINT).round() as i32).max(1);
        let dim = premul_rgb(0x00FF_FFFF, (f32::from(cov) * STAR_GLINT_COV) as u8);
        for (ox, oy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            assert!(
                out.iter().any(|q| i32::from(q.x) == sx + ox
                    && i32::from(q.y) == sy + oy
                    && q.w == 1
                    && q.h == 1
                    && q.color == dim),
                "no glint at {ox},{oy}"
            );
        }
        // A star with no gold half throws no glints.
        let mut plain = Vec::new();
        assert!(push_twinkle_star(
            &mut plain,
            g,
            sx,
            sy,
            arm,
            cov,
            false,
            0x00FF_FFFF,
            4096
        ));
        assert!(
            plain
                .iter()
                .all(|q| !(q.w == 1 && q.h == 1 && q.color == dim))
        );
    }

    /// The branchless shape contract behind [`ramp_5stop`]: stop positions
    /// strictly ascending from exactly 0.0 to 1.0 in both tables. The core relies
    /// on it (counting interior stops, no divide guard, NaN pinned to 1.0), so a
    /// palette retune breaking it fails here with the table's name.
    #[test]
    fn ramp_stop_tables_hold_the_branchless_contract() {
        for (name, stops) in [("fire", &FIRE_STOPS), ("water", &WATER_STOPS)] {
            assert!(stops[0].0 == 0.0, "{name}: the ramp must start at 0.0");
            assert!(stops[4].0 == 1.0, "{name}: the ramp must end at 1.0");
            for w in stops.windows(2) {
                assert!(
                    w[1].0 > w[0].0,
                    "{name}: stop positions must strictly ascend ({} then {})",
                    w[0].0,
                    w[1].0
                );
            }
        }
    }

    /// The windowed-walk ramp (function-local table, `windows(2)` walk, divide
    /// guard, raw-crest fall-through): the byte-identity oracle for the
    /// branchless [`ramp_5stop`]. Do not rewrite it to call the live code.
    fn walked_ramp(stops: &[(f32, u32); 5], t: f32) -> u32 {
        let t = t.clamp(0.0, 1.0);
        for w in stops.windows(2) {
            let (t0, c0) = w[0];
            let (t1, c1) = w[1];
            if t <= t1 {
                let local = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
                return lerp_rgb(c0, c1, local);
            }
        }
        stops[stops.len() - 1].1
    }

    /// One f32 bit pattern through BOTH live ramps against the walk oracle.
    fn assert_ramps_match_walk(bits: u32) {
        let t = f32::from_bits(bits);
        assert_eq!(
            fire_ramp(t),
            walked_ramp(&FIRE_STOPS, t),
            "fire_ramp diverged from the stop walk at bits {bits:#010x} (t = {t:?})"
        );
        assert_eq!(
            water_ramp(t),
            walked_ramp(&WATER_STOPS, t),
            "water_ramp diverged from the stop walk at bits {bits:#010x} (t = {t:?})"
        );
    }

    /// Byte identity of the branchless ramps with the walk: same input,
    /// bit-equal u32 out. Frame fingerprints cannot see a one-count colour drift,
    /// so the pin is on the functions. Three passes: a prime-strided march over
    /// the whole f32 bit space (~1M probes, all exponents, signs, infinities and
    /// NaN payloads), a dense ±16Ki-ULP band around every stop position of both
    /// tables plus sign-bit mirrors, and the named specials.
    #[test]
    fn ramp_rewrite_is_bit_identical_to_the_stop_walk() {
        // 4099 is prime, so the probes never lock onto a mantissa stride.
        for bits in (0..=u32::MAX).step_by(4099) {
            assert_ramps_match_walk(bits);
        }
        for anchor in [0.0f32, 0.25, 0.35, 0.5, 0.65, 0.75, 0.85, 1.0] {
            let b = anchor.to_bits();
            for bits in b.saturating_sub(16_384)..=b.saturating_add(16_384) {
                assert_ramps_match_walk(bits);
                // The mirror: tiny negatives around -0.0 and every negated
                // anchor all clamp to 0.0, and must clamp IDENTICALLY.
                assert_ramps_match_walk(bits | 0x8000_0000);
            }
        }
        for t in [
            f32::NAN,
            -f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            f32::EPSILON,
            f32::from_bits(1), // the smallest subnormal
            -0.0,
            2.0,
            f32::MAX,
            f32::MIN,
        ] {
            assert_ramps_match_walk(t.to_bits());
        }
    }
}

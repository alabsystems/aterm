// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor emitters' shared render kit: the per-cell-row quad pushers, the colour ramps, and
//! the 4-point twinkle star.
//!
//! The two pushers are not interchangeable: [`push_grid_quad`] takes grid-relative px, clamps
//! to the grid and tags rows `y / ch`; [`push_fx_rect`] takes window px (the space of
//! `cursor_glow_add`), clamps to the effects box and tags rows from the origin. A caret-anchored
//! emitter on the grid form is displaced by the window origin; the grid form's one caller is
//! `output_streak` (light anchored to a text run).
//! [`push_grid_quad`], whose one caller is `output_streak` (light anchored to a
//! text run, not the caret).

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;

/// Clamp a grid-relative pixel rect to the grid interior and split it into per-cell-row
/// [`GlowQuad`]s tagged `y / ch` (the renderer row gate and CPU/GPU parity need one row per
/// quad).
///
/// `alpha == 0` is additive light; `alpha > 0` selects premultiplied source-over
/// (`src + dst·(1 − a)`), which a light-theme emitter needs because additive light only
/// brightens.
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
    // Zero light is skippable only when additive: a source-over quad whose
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
    let x0_u = x0 as u16;
    let w_u = (x1 - x0) as u16;
    let mut yy = y0;
    while yy < y1 {
        let row = yy / ch;
        let band_end = ((row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row as u16,
            x: x0_u,
            y: yy as u16,
            w: w_u,
            h: (band_end - yy) as u16,
            color: premul,
            alpha,
            color2: premul,
            alpha2: alpha,
        });
        yy = band_end;
    }
}

/// Push additive premultiplied light in window px (`geom.origin_*` applied), clamped to the
/// effects box (grid + head band) and split into per-cell-row [`GlowQuad`]s with
/// origin-anchored row tags.
#[inline]
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
    // Additive: this emitter has no other mode.
    push_fx_quad(out, geom, x, y, w, h, premul, 0);
}

/// [`push_fx_rect`] with the blend mode left to the caller: the one copy of the
/// window-px band walk. `alpha == 0` is additive; `alpha > 0` is that
/// coverage as source-over opacity (`src + dst·(1 − a)`), for light-theme ink.
#[allow(
    clippy::too_many_arguments,
    reason = "the rect + blend mode IS the call, as in `push_grid_quad`"
)]
#[inline]
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
    push_fx_ramp(out, geom, x, y, w, h, (premul, alpha), (premul, alpha));
}

/// A soft round mark of ADDITIVE light at `(cx, cy)` with radii `(rx, ry)`:
/// one radial profile, `(1 − ρ²)²` peaking at `peak` (coverage `0..=255`) at
/// the centre and reaching nothing at the rim, laid one pixel row at a time
/// as a few linear ramps a side ([`push_fx_ramp`]), so its edge never stands
/// as a step (a bead, a drop or a glint of light that a stack of flat slabs
/// drew in stairs).
pub(crate) fn push_soft_disc(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    (rx, ry): (f32, f32),
    color: u32,
    peak: f32,
) {
    const SEGMENTS: usize = 4;
    if peak < 1.0 || rx < 0.5 || ry < 0.5 {
        return;
    }
    let inv_rx = 1.0 / rx;
    let (y0, y1) = ((cy - ry).floor() as i32, (cy + ry).ceil() as i32);
    for y in y0..y1 {
        let dy = (y as f32 + 0.5 - cy) / ry;
        let dy2 = dy * dy;
        let rest = 1.0 - dy2;
        if rest <= 0.0 {
            continue;
        }
        let span = rx * rest.sqrt();
        let level = |x: f32| {
            let u = x * inv_rx;
            let w = (1.0 - u * u - dy2).max(0.0);
            let cov = (peak * w * w).round().clamp(0.0, 255.0) as u8;
            premul_rgb(color, cov)
        };
        let knot = |k: usize| -span + 2.0 * span * k as f32 / (2 * SEGMENTS) as f32;
        let x0 = knot(0);
        let mut ia = (cx + x0).round() as i32;
        let mut a = level(x0);
        for k in 0..2 * SEGMENTS {
            let xb = knot(k + 1);
            let ib = (cx + xb).round() as i32;
            let b = level(xb);
            if ib > ia && (a != 0 || b != 0) {
                push_fx_ramp(out, geom, ia, y, ib - ia, 1, (a, 0), (b, 0));
            }
            ia = ib;
            a = b;
        }
    }
}

/// [`push_fx_quad`] with a horizontal ramp: `(colour, alpha)` `left` at the
/// rect's left edge and `right` at its right edge, by
/// [`GlowQuad::color2`]'s law. Both ends are in one blend mode (both alphas
/// `0`, additive, or both `> 0`, source-over). Where the effects box clips the
/// rect, the cut edge takes the ramp's value at the cut, so a clipped ramp
/// keeps its slope; equal ends pass through bit-exact, which is what keeps
/// every flat caller byte-unchanged.
#[allow(
    clippy::too_many_arguments,
    reason = "the rect + both ramp ends IS the call, as in `push_fx_quad`"
)]
pub(crate) fn push_fx_ramp(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    left: (u32, u8),
    right: (u32, u8),
) {
    if w <= 0 || h <= 0 {
        return;
    }
    debug_assert_eq!(
        left.1 == 0,
        right.1 == 0,
        "a quad's blend mode is per quad, never per end"
    );
    debug_assert!(
        geom.fx_right() <= i32::from(u16::MAX) && geom.fx_bot() <= i32::from(u16::MAX),
        "effects-box pixel extent exceeds u16 GlowQuad range"
    );
    let x0 = x.max(geom.fx_left());
    let x1 = (x + w).min(geom.fx_right());
    let y0 = y.max(geom.fx_top());
    let y1 = (y + h).min(geom.fx_bot());
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    // The ramp's value at window column `px` (an edge), for a clipped end.
    let at = |px: i32| -> (u32, u8) {
        if left == right {
            return left;
        }
        let t = (px - x) as f32 / w as f32;
        let lerp = |a: u32, b: u32| (a as f32 + (b as f32 - a as f32) * t).round() as u32;
        let ch = |sh: u32| lerp((left.0 >> sh) & 0xff, (right.0 >> sh) & 0xff).min(255) << sh;
        let alpha = lerp(u32::from(left.1), u32::from(right.1)).min(255) as u8;
        // A source-over end stays source-over (`alpha == 0` would switch
        // the quad to additive).
        let alpha = if left.1 > 0 { alpha.max(1) } else { 0 };
        (ch(16) | ch(8) | ch(0), alpha)
    };
    let (c0, a0) = if x0 == x { left } else { at(x0) };
    let (c1, a1) = if x1 == x + w { right } else { at(x1) };
    let ch = geom.ch as i32;
    let oy = i32::from(geom.origin_y);
    let x0_u = x0 as u16;
    let w_u = (x1 - x0) as u16;
    let mut yy = y0;
    while yy < y1 {
        // Grid-row damage hint anchored at origin_y (above-grid bands tag row 0).
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0_u,
            y: yy as u16,
            w: w_u,
            h: (band_end - yy) as u16,
            color: c0,
            alpha: a0,
            color2: c1,
            alpha2: a1,
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
/// first exactly 0.0, last exactly 1.0).
const FIRE_STOPS: [(f32, u32); 5] = [
    (0.0, 0x002A_0000),
    (0.25, 0x008B_1A00),
    (0.5, 0x00E0_4A00),
    (0.75, 0x00FF_B020),
    (1.0, 0x00FF_F0C0),
];

/// The water ramp's stops: abyss → open-ocean blue → turquoise → aqua crest → foam. Saturated
/// and green-leaning through the midband (pale sky-cyan reads as ice). Same shape contract as
/// [`FIRE_STOPS`].
const WATER_STOPS: [(f32, u32); 5] = [
    (0.0, 0x0005_2C48),
    (0.35, 0x000E_66B4),
    (0.65, 0x0014_AAC8),
    (0.85, 0x0032_DCDE),
    (1.0, 0x00C2_F2F5),
];

/// The shared 5-stop ramp core. The window index is the count of interior stop positions
/// strictly below `t` (three compares, no branch), feeding `(t - t0) / (t1 - t0)` into
/// [`lerp_rgb`], so every output is bit-equal to the windowed walk ("first window with
/// `t <= t1`"), pinned by `ramp_rewrite_is_bit_identical_to_the_stop_walk`.
// Not `.clamp()`: `f32::clamp` propagates NaN, while `min`/`max` return the other operand, so
// min-first pins NaN to 1.0 and the lerp lands exactly on the crest colour.
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

/// The black-body-ish fire palette, `t` 0 (cool, deep red) → 1 (hot, white-yellow).
#[inline]
pub(crate) fn fire_ramp(t: f32) -> u32 {
    ramp_5stop(&FIRE_STOPS, t)
}

/// The water palette, `t` 0 (deep navy) → 1 (pale foam crest).
#[inline]
pub(crate) fn water_ramp(t: f32) -> u32 {
    ramp_5stop(&WATER_STOPS, t)
}

/// The twinkle's white/gold pair. One gold for the whole sparkle family: a caller wanting the
/// warm half asks for `twinkle_rgb(true)`.
#[inline]
pub(crate) const fn twinkle_rgb(gold: bool) -> u32 {
    if gold { 0x00FF_E9A8 } else { 0x00FF_FFFF }
}

// ─── the sparkle family ─────────────────────────────────────────────────────
//
// One silhouette, one arm ladder, one pulse rule. The additive star ([`push_twinkle_star`]) and
// its source-over twin (`rainbow_kitty::stardust`'s `push_twinkle_over_twin`) are the same
// mark, defined by the numbers here; a context chooses its colour, size and phase.

/// Silhouette: the arms' half-thickness at the base as a fraction of the arm's half-length. The
/// source-over star takes it as each arm's falloff minor radius, the additive star as its
/// integer bar thickness ([`star_waist_px`]). At 0.10 the waist is the 1 px hairline for every
/// ordinary arm and widens only on hero grains; a thick waist reads as a typographic `+`.
pub(crate) const STAR_WAIST: f32 = 0.10;

// ─── the taper ──────────────────────────────────────────────────────────────
//
// A sparkle comes to a point: a constant-thickness arm crossed with its twin is a `+`, however
// thin. A cursor star's arm is ~3-8 px on a 1-2 px base, too small for a narrowing outline to
// cross a pixel boundary, so the needle is a coverage ramp: a bright body at the crossing and a
// short run of dimmer, thinner spans out to each point. The additive arm ramps per-span
// coverage (and thickness once it has room); the source-over arm stacks a shorter body lay over
// a full-length tip lay.

/// The taper: the fraction of an arm's half-length drawn at full coverage through the
/// crossing; beyond it the arm is the point. Less loses the solid centre (at `arm` 3-4 the body
/// would be one pixel); more leaves too short a run to ramp; 0.42 gives the ramp one more pixel
/// than 0.5 at starfield sizes.
pub(crate) const STAR_TAPER_BODY: f32 = 0.42;

/// The taper: coverage at the point as a fraction of the requested coverage, ramping linearly
/// from full at the body's end. 0.22 dissolves the tip without shortening the arm.
pub(crate) const STAR_TIP_COV: f32 = 0.22;

/// The taper: the most spans one point is rasterized with (per side, per arm), one per pixel of
/// its run. The additive star is a hot path and each span costs 4 quads (two arms × two sides),
/// so at 3 a star costs at most 15 before its glints.
pub(crate) const STAR_TAPER_STEPS: i32 = 3;

/// How many coincident source-over lays `cursor_glow`'s `push_twinkle_over`
/// stacks at the crossing: a TIP lay and a BODY lay per arm, plus the nucleus.
pub(crate) const STAR_OVER_LAYS: usize = 5;

/// How many lays the star's centre composites to: `stacked_ink_alpha` prices a round grain
/// against `1 − (1 − a)³`, and the taper adds lays only to the arms, so the light star solves a
/// per-lay alpha over [`STAR_OVER_LAYS`] that reproduces this stack.
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

/// Silhouette: the diagonal glint dots' offset from the centre, as a fraction of the arm's
/// half-length.
pub(crate) const STAR_GLINT: f32 = 0.5;

/// The glints' share of the star's coverage — dim by design, so text stays
/// legible under a starfield.
pub(crate) const STAR_GLINT_COV: f32 = 1.0 / 3.0;

/// The one arm scale: a star's arm half-length as a fraction of cell height, times one of the
/// named ratios below ([`star_arm`]).
pub(crate) const STAR_ARM: f32 = 0.14;

/// Arm ratio, starfield grain: the smallest mark that still reads as a star.
pub(crate) const STAR_ARM_FINE: f32 = 0.75;

/// Arm ratio, the family default.
pub(crate) const STAR_ARM_STD: f32 = 1.0;

/// Arm ratio, a hero grain.
pub(crate) const STAR_ARM_HERO: f32 = 1.4;

/// Arm ratio, the ink tax: source-over ink buys presence with area where
/// additive light uses coverage, so a light-theme star is its dark twin's ratio
/// times this.
pub(crate) const STAR_ARM_INK: f32 = 1.55;

/// The ladder's shape, checked at build time: the rungs stay ordered and short.
const _: () = {
    assert!(
        STAR_ARM_FINE < STAR_ARM_STD && STAR_ARM_STD < STAR_ARM_HERO,
        "the star arm ladder must stay ordered fine < std < hero"
    );
    assert!(
        STAR_ARM_HERO / STAR_ARM_FINE <= 2.0,
        "the family's own star sizes must stay inside 2x end to end"
    );
    assert!(
        STAR_ARM_INK > 1.0,
        "a source-over star must out-size its additive twin"
    );
};

/// A star's arm half-length in px: cell height × [`STAR_ARM`] × a named ratio.
#[inline]
pub(crate) fn star_arm(cell_h: f32, ratio: f32) -> f32 {
    cell_h * STAR_ARM * ratio
}

/// The one float→integer arm conversion for the additive star: truncate, then floor at the
/// 1 px arm [`push_twinkle_star`] needs. Truncate, because waist and nucleus round
/// `2·arm·ratio`: rounding would turn the hero grain at a 40 px cell (7.84 px of arm) into a
/// 2 px bar on a 3 px nucleus.
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
/// poof, and the fresh-ink spark (owner, 2026-08-09). The Sparkle and Comet
/// wakes are measured by `cursor_glow`'s
/// `stardust_is_the_body_the_plus_is_the_accent`.
pub(crate) const STAR_ACCENT_DEN: u32 = 16;

/// Deal a stored particle seed: `true` is the 4-point accent, `false` a round mote.
#[inline]
pub(crate) fn star_accent(seed: u32) -> bool {
    seed.is_multiple_of(STAR_ACCENT_DEN)
}

/// Stardust mote radius, px: seeded so no two motes match, floored so the smallest is still a
/// lit point.
#[inline]
pub(crate) fn dust_r(cell_h: f32, seed01: f32) -> f32 {
    (cell_h * (0.07 + 0.05 * seed01)).max(1.3)
}

/// The plus's composited centre as a multiple of its requested coverage: [`push_twinkle_star`]
/// lays two arms at `cov` and the nucleus at [`STAR_CORE_ADD`] of it at the crossing. A round
/// mote replacing a plus is priced against this, or it lands 2.35x darker; the light arm does
/// the same with `1-(1-a)^3`.
pub(crate) const STAR_STACK_ADD: f32 = 2.0 + STAR_CORE_ADD;

/// The one pulse rule: angular frequency (rad/s) of every twinkling star.
///
/// WCAG 2.3.1: this crate certifies [`TWINKLE_FLASH_BOUND_HZ`]. The envelope
/// is rectified (`|sin|`), peaking twice per sine cycle, so the bound applies
/// to `ω/π`. At 9 rad/s that is [`TWINKLE_FLASH_HZ`] = 2.86 Hz.
pub(crate) const TWINKLE_OMEGA: f32 = 9.0;

/// The pulse's floor. A star dims, never blinks out: a sparkle that reaches zero reads as a
/// dropped frame.
pub(crate) const TWINKLE_FLOOR: f32 = 0.55;

/// The pulse's depth. `TWINKLE_FLOOR + TWINKLE_DEPTH` is 1.0: the law scales a star's own
/// coverage and never exceeds it.
pub(crate) const TWINKLE_DEPTH: f32 = 0.45;

/// Where in the envelope's range a star counts as at its peak, the gate the
/// diffraction-glint accents fire on: 0.92 puts the glint on for ~26 % of each
/// pulse.
pub(crate) const TWINKLE_GLINT_FRAC: f32 = 0.92;

/// The family's flash rate in Hz: `ω/π`, since the rectified envelope peaks twice per sine
/// cycle.
pub(crate) const TWINKLE_FLASH_HZ: f32 = TWINKLE_OMEGA / std::f32::consts::PI;

/// WCAG 2.3.1's general-flash threshold in Hz: more than three flashes in any
/// one-second period fails, so a rate of at most 3.0 is what this crate
/// certifies.
///
/// Every flash-rate law in the crate is held under it by name: the twinkle
/// (below), the kitty-command trick flash (`trick_flash.rs`, its
/// luminance-reversal rate const-asserted, its measured test
/// `the_flash_stays_under_the_photosensitivity_bound`), the rainbow cursor's
/// flare scintillation, and lightning's onset limiter and streak ripple
/// (`cursor_glow`) — and, outside the crate, the harness card's re-show
/// bound (`aterm-gui`'s `claude_card::RESHOW_MS`), re-exported at the crate
/// root for it.
pub const TWINKLE_FLASH_BOUND_HZ: f32 = 3.0;

/// Retuning [`TWINKLE_OMEGA`] past the photosensitivity bound stops the build.
const _: () = assert!(
    TWINKLE_FLASH_HZ <= TWINKLE_FLASH_BOUND_HZ,
    "the unified twinkle flashes over the WCAG 2.3.1 general-flash bound"
);

/// The one twinkle: a star's brightness multiplier at `age_s` seconds of life
/// on its own `phase` (radians; seed per star so a field blinks out of step).
/// Driven by age, never a frame counter, so the rate is the same at any refresh
/// rate; `twinkle_law_stays_under_the_photosensitivity_bound` measures the
/// rate.
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

/// The additive star's integer nucleus side (px), from the shared [`STAR_CORE`].
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

/// Which paint a piece of the star takes ([`star_pieces`]): its coverage is
/// the star's times the part's share, so a frame recolours a star by these
/// alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum StarPart {
    /// A needle's full-coverage body through the crossing.
    Body,
    /// The nucleus.
    Core,
    /// Span `step` of a point, at `taper` of the body's coverage
    /// ([`star_taper_cov`] at the span's midpoint).
    Tip { step: u8, taper: f32 },
    /// A diagonal glint dot.
    Glint,
}

/// How many distinct paints a star's pieces take ([`StarPart::id`]): the
/// body, the nucleus, each point span, the glints.
pub(crate) const STAR_PAINTS: usize = 3 + STAR_TAPER_STEPS as usize;

impl StarPart {
    /// The part's paint id, `0..STAR_PAINTS`: the body `0`, the nucleus `1`,
    /// point span `k` `2 + k`, a glint the last.
    #[must_use]
    #[inline]
    pub(crate) const fn id(self) -> u8 {
        match self {
            Self::Body => 0,
            Self::Core => 1,
            Self::Tip { step, .. } => 2 + step,
            Self::Glint => STAR_PAINTS as u8 - 1,
        }
    }
}

/// One piece of a star: a rectangle `[x, x + w) × [y, y + h)` relative to the
/// star's centre, and its part.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StarPiece {
    pub(crate) part: StarPart,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: i32,
    pub(crate) h: i32,
}

/// The most pieces one star has: two body bars, the nucleus, four spans a
/// point step, four glints.
pub(crate) const STAR_PIECES_MAX: usize = 3 + 4 * STAR_TAPER_STEPS as usize + 4;

/// A star's pieces in the order they are laid ([`star_pieces`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct StarPieces {
    pieces: [StarPiece; STAR_PIECES_MAX],
    n: usize,
}

impl std::ops::Deref for StarPieces {
    type Target = [StarPiece];

    #[inline]
    fn deref(&self) -> &[StarPiece] {
        &self.pieces[..self.n]
    }
}

/// THE STAR'S GEOMETRY, the one source of it: the pieces of the 4-point
/// twinkle of half-length `arm` (glints when `gold`), relative to its centre,
/// in the order every rasterizer lays them — the horizontal body, the
/// vertical body, the nucleus, then per point step the horizontal and the
/// vertical span right/down of the crossing and then left/up of it, then the
/// four glints. Each needle is a full-coverage body through the crossing at
/// the shared waist ([`STAR_TAPER_BODY`], [`star_waist_px`]) running into
/// [`STAR_TAPER_STEPS`] spans at most that dim toward [`STAR_TIP_COV`] and
/// thin with them. An even thickness is biased up-left by the integer half
/// so the centre stays on the centre pixel; at the 1 px floor a needle is a
/// hairline. No pieces for an arm under 1.
///
/// [`push_twinkle_star`] (the cursor family's stars), the harness card's
/// stars and the card's art registry lay exactly these.
#[must_use]
pub(crate) fn star_pieces(arm: i32, gold: bool) -> StarPieces {
    let none = StarPiece {
        part: StarPart::Body,
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };
    let mut out = StarPieces {
        pieces: [none; STAR_PIECES_MAX],
        n: 0,
    };
    if arm < 1 {
        return out;
    }
    let mut push = |part: StarPart, x: i32, y: i32, w: i32, h: i32| {
        out.pieces[out.n] = StarPiece { part, x, y, w, h };
        out.n += 1;
    };
    let t = star_waist_px(arm);
    let half = (t - 1) / 2;
    let b = star_body_px(arm);
    push(StarPart::Body, -b, -half, 2 * b + 1, t);
    push(StarPart::Body, -half, -b, t, 2 * b + 1);
    // Nucleus: on additive light the crossing supplies part of it and this
    // completes it. Laid before the points.
    let c = star_core_px(arm);
    let cc = (c - 1) / 2;
    push(StarPart::Core, -cc, -cc, c, c);
    // The points: each arm runs from the body to the tip in equal spans, each
    // dimmer (and, with thickness to spare, thinner) than the last. Two spans
    // per step, one per side of the crossing.
    let run = arm - b;
    let steps = run.clamp(0, STAR_TAPER_STEPS);
    for k in 0..steps {
        let d0 = b + run * k / steps; // last px of the previous span
        let d1 = b + run * (k + 1) / steps; // last px of this one
        // The ramp runs over the POINT — full where the body ends,
        // [`STAR_TIP_COV`] at the tip — sampled at this span's midpoint.
        let taper = star_taper_cov(((d0 + d1) as f32 * 0.5 - b as f32) / run as f32);
        let tt = ((t as f32 * taper).round() as i32).max(1);
        let th = (tt - 1) / 2;
        let len = d1 - d0;
        let part = StarPart::Tip {
            step: k as u8,
            taper,
        };
        // `d0 + 1` runs right/down from the body; `-d1` mirrors it.
        for off in [d0 + 1, -d1] {
            push(part, off, -th, len, tt);
            push(part, -th, off, tt, len);
        }
    }
    if gold {
        let d = ((arm as f32 * STAR_GLINT).round() as i32).max(1);
        for (ox, oy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            push(StarPart::Glint, ox, oy, 1, 1);
        }
    }
    out
}

/// The one star: a 4-point twinkle, a horizontal and a vertical needle crossing
/// at `(sx, sy)` on a small nucleus, plus four dim diagonal glint dots when
/// `gold` ([`star_pieces`], the one geometry). Each needle is a full-coverage
/// body through the crossing ([`STAR_TAPER_BODY`]) running into points that
/// dim toward [`STAR_TIP_COV`] and thin with them (see the taper notes above).
///
/// Every 4-point star goes through here (rainbow kitty typing starfield, glide
/// stars, jump-landing burst, shooting-star heads, Beam's stardust, the comet's
/// glint, Sparkle's star grains), so only colour and size vary with context.
/// Waist ([`STAR_WAIST`]), nucleus ([`STAR_CORE`]) and glint offset
/// ([`STAR_GLINT`]) are shared with `cursor_glow`'s source-over
/// `push_twinkle_over`. This one is hard-edged and additive: the crossing
/// supplies part of the nucleus, and the core rect at [`STAR_CORE_ADD`]
/// finishes it without clipping to white. A point span whose light rounds to
/// nothing ends the points: no later span is laid.
///
/// Window-absolute, via [`push_fx_rect`]. Returns `false` when the quad budget
/// ran out: it is checked before every piece but the first, so a truncated
/// star keeps its bodies and its nucleus and loses its faint tips.
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
    let at = |share: f32| premul_rgb(color, (f32::from(cov) * share) as u8);
    let body_paint = premul_rgb(color, cov);
    let glint_paint = if gold { at(STAR_GLINT_COV) } else { 0 };
    let mut last_tip: Option<(u8, u32)> = None;
    let mut ended = false;
    for (i, p) in star_pieces(arm, gold).iter().enumerate() {
        let paint = match p.part {
            StarPart::Body => body_paint,
            StarPart::Core => at(STAR_CORE_ADD),
            StarPart::Tip { step, taper } => {
                let tip = match last_tip {
                    Some((s, cached)) if s == step => cached,
                    _ => {
                        let c = at(taper);
                        last_tip = Some((step, c));
                        c
                    }
                };
                ended |= tip == 0;
                if ended {
                    continue;
                }
                tip
            }
            StarPart::Glint => glint_paint,
        };
        if i > 0 && out.len() >= max_quads {
            return false;
        }
        push_fx_rect(out, geom, sx + p.x, sy + p.y, p.w, p.h, paint);
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

/// The worst WCAG contrast against `ground` over one caret cell, window px
/// `(x, y, w, h)`: the block `fill` painted first, then every quad composited
/// OVER it by its own mode (`alpha == 0` a saturating add, else source-over).
/// Harsher than the renderer's usual order, which draws the opaque block
/// last; it is the case a translucent cursor or the GPU bloom meets, so a
/// body that passes here keeps its caret legible under either. Quads are read
/// flat (`color`/`alpha`), as these emitters push them.
#[cfg(test)]
pub(crate) fn caret_cell_worst_contrast(
    fill: u32,
    quads: &[GlowQuad],
    cell: (i32, i32, i32, i32),
    ground: u32,
) -> f32 {
    let (cx, cy, cw, ch) = cell;
    let mut px = vec![fill & 0x00FF_FFFF; (cw * ch) as usize];
    for q in quads {
        let (qx0, qy0) = (i32::from(q.x), i32::from(q.y));
        let (qx1, qy1) = (qx0 + i32::from(q.w), qy0 + i32::from(q.h));
        for y in qy0.max(cy)..qy1.min(cy + ch) {
            for x in qx0.max(cx)..qx1.min(cx + cw) {
                let d = &mut px[((y - cy) * cw + (x - cx)) as usize];
                let blend = |sh: u32| {
                    let (dc, sc) = ((*d >> sh) & 0xff, (q.color >> sh) & 0xff);
                    if q.alpha == 0 {
                        (dc + sc).min(255)
                    } else {
                        (sc + (dc * (255 - u32::from(q.alpha)) + 127) / 255).min(255)
                    }
                };
                *d = (blend(16) << 16) | (blend(8) << 8) | blend(0);
            }
        }
    }
    px.iter()
        .map(|&c| crate::color_math::contrast_ratio(c, ground))
        .fold(f32::INFINITY, f32::min)
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

    /// Only additive zero light is skipped: a source-over quad that rounds to black still darkens.
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

    /// The envelope's measured flash rate matches [`TWINKLE_FLASH_HZ`]: `|sin|` peaks twice per
    /// sine cycle, so dropping the rectification would halve it.
    #[test]
    fn twinkle_law_stays_under_the_photosensitivity_bound() {
        // Count bright peaks over ten seconds.
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

    /// The arm conversion truncates: at every shipping cell height the heaviest named grain is
    /// a hairline, and somewhere in that band rounding would fatten it.
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

    /// The additive star's proportions (arm and waist, nucleus, glint offset and dimming)
    /// come from the shared constants.
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
        // The body: the arm's full-coverage run through the crossing, at the shared waist
        // and the shared body fraction.
        let t = star_waist_px(arm);
        let b = star_body_px(arm);
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

    /// The windowed-walk ramp (`windows(2)` walk, divide guard, raw-crest fall-through): the
    /// byte-identity oracle for the branchless [`ramp_5stop`]. Do not rewrite it to call the
    /// live code.
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

    /// One f32 bit pattern through both live ramps against the walk oracle.
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

    /// The branchless ramps are bit-equal to the walk (frame fingerprints miss a one-count
    /// drift): a prime-strided march over the f32 bit space, a ±16Ki-ULP band around every stop
    /// with its sign-bit mirror, and the named specials.
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
                // Sign-bit mirrors all clamp to 0.0, and must clamp identically.
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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared cursor-emitter render kit: per-cell-row quad pushers, colour ramps, and 4-point twinkle star.

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;

/// Clamp a grid-relative pixel rect to the grid interior and split it into per-cell-row [`GlowQuad`]s.
#[allow(
    clippy::too_many_arguments,
    reason = "the rect + blend mode IS the call; a struct would relocate the same list and cost \
              every existing caller a construction"
)]
#[inline]
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
    let mut row = y0 / ch;
    let mut next_band = (row + 1) * ch;
    let mut yy = y0;
    while yy < y1 {
        let band_end = next_band.min(y1);
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
        row += 1;
        next_band += ch;
    }
}

/// Premultiplied `0x00RRGGBB` light scaled by `share` (`0..=1`), rounding per channel.
#[inline]
#[must_use]
pub(crate) const fn scale_premul(c: u32, share: f32) -> u32 {
    let share = share.clamp(0.0, 1.0);
    const fn ch(c: u32, share: f32, sh: u32) -> u32 {
        let v = (((c >> sh) & 0xff) as f32 * share).round() as u32;
        (if v < 255 { v } else { 255 }) << sh
    }
    ch(c, share, 16) | ch(c, share, 8) | ch(c, share, 0)
}

/// How far inside the effects box's top and bottom edges a light that crosses them eases to
/// nothing ([`fx_fade_y`]), as a share of the cell height.
pub(crate) const FX_EDGE_FADE: f32 = 0.35;
/// How far inside the effects box's left and right edges a light that crosses them eases to
/// nothing ([`fx_fade_x`]), as a share of the cell width.
pub(crate) const FX_EDGE_FADE_X: f32 = 0.6;

/// The share of its strength a light keeps at window row `y` near the effects box's top or
/// bottom edge: whole [`FX_EDGE_FADE`] of a cell height inside, easing to nothing at the
/// edge. The box clips what crosses it, so a light round a caret on the grid's last row (the
/// typing glow, a water ring) stood cut by a straight line along the edge; laid through
/// this it fades out instead.
#[inline]
#[must_use]
pub(crate) const fn fx_fade_y(geom: Geom, y: f32) -> f32 {
    let reach = (geom.ch as f32 * FX_EDGE_FADE).max(1.0);
    let d = (y - geom.fx_top() as f32).min(geom.fx_bot() as f32 - y);
    let k = (d / reach).clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
}

/// [`fx_fade_y`] across the effects box's left and right edges, at window column `x`, over
/// [`FX_EDGE_FADE_X`] of a cell width.
#[inline]
#[must_use]
pub(crate) const fn fx_fade_x(geom: Geom, x: f32) -> f32 {
    let reach = (geom.cw as f32 * FX_EDGE_FADE_X).max(1.0);
    let d = (x - geom.fx_left() as f32).min(geom.fx_right() as f32 - x);
    let k = (d / reach).clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
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
    push_fx_quad(out, geom, x, y, w, h, premul, 0);
}

/// [`push_fx_rect`] with caller-selected blend mode (`alpha == 0` additive, `alpha > 0` source-over).
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

/// Push a soft round mark of additive light at `(cx, cy)` with radii `(rx, ry)` and centre coverage `peak`.
#[inline]
pub(crate) fn push_soft_disc(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    (rx, ry): (f32, f32),
    color: u32,
    peak: f32,
) {
    const SEGMENTS: usize = 4;
    if peak < 1.0 || rx < 0.5 || ry < 0.5 || color == 0 {
        return;
    }
    let y0 = ((cy - ry).floor() as i32).max(geom.fx_top());
    let y1 = ((cy + ry).ceil() as i32).min(geom.fx_bot());
    if y0 >= y1 {
        return;
    }
    let inv_rx = 1.0 / rx;
    let inv_ry = 1.0 / ry;
    #[inline]
    const fn ellipse_level(x: f32, inv_rx: f32, dy2: f32, peak: f32, color: u32) -> u32 {
        let u = x * inv_rx;
        let w = (1.0 - u * u - dy2).max(0.0);
        let cov = (peak * w * w).round().clamp(0.0, 255.0) as u8;
        premul_rgb(color, cov)
    }
    #[inline]
    const fn ellipse_knot(k: usize, span: f32, step: f32) -> f32 {
        -span + k as f32 * step
    }
    let mut y = y0;
    while y < y1 {
        let dy = (y as f32 + 0.5 - cy) * inv_ry;
        let dy2 = dy * dy;
        let rest = 1.0 - dy2;
        if rest > 0.0 {
            let span = rx * crate::color_math::sqrt_f32(rest);
            let step = span * (1.0 / SEGMENTS as f32);
            let x0 = ellipse_knot(0, span, step);
            let mut ia = (cx + x0).round() as i32;
            let mut a = ellipse_level(x0, inv_rx, dy2, peak, color);
            let mut k = 0;
            while k < 2 * SEGMENTS {
                let xb = ellipse_knot(k + 1, span, step);
                k += 1;
                let ib = (cx + xb).round() as i32;
                let b = ellipse_level(xb, inv_rx, dy2, peak, color);
                if ib > ia && (a != 0 || b != 0) {
                    push_fx_ramp(out, geom, ia, y, ib - ia, 1, (a, 0), (b, 0));
                }
                ia = ib;
                a = b;
            }
        }
        y += 1;
    }
}

/// [`push_fx_quad`] with a horizontal `(colour, alpha)` ramp from `left` to `right`.
#[allow(
    clippy::too_many_arguments,
    reason = "the rect + both ramp ends IS the call, as in `push_fx_quad`"
)]
#[inline]
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
    if w <= 0 || h <= 0 || (left == (0, 0) && right == (0, 0)) {
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
    #[inline]
    const fn ramp_end_at(
        px: i32,
        x: i32,
        inv_w: f32,
        left: (u32, u8),
        right: (u32, u8),
    ) -> (u32, u8) {
        let t = (px - x) as f32 * inv_w;
        let color = if left.0 == right.0 {
            left.0 & 0x00FF_FFFF
        } else {
            lerp_rgb(left.0, right.0, t)
        };
        let alpha = if left.1 == right.1 {
            left.1
        } else {
            let a = (left.1 as f32 + (right.1 as f32 - left.1 as f32) * t).round() as u32;
            let clamped = if a < 255 { a as u8 } else { 255 };
            if clamped > 1 { clamped } else { 1 }
        };
        (color, alpha)
    }
    let ((c0, a0), (c1, a1)) = if left == right || (x0 == x && x1 == x + w) {
        (left, right)
    } else {
        let inv_w = 1.0 / w as f32;
        let end0 = if x0 == x {
            left
        } else {
            ramp_end_at(x0, x, inv_w, left, right)
        };
        let end1 = if x1 == x + w {
            right
        } else {
            ramp_end_at(x1, x, inv_w, left, right)
        };
        (end0, end1)
    };
    let ch = geom.ch as i32;
    let oy = i32::from(geom.origin_y);
    let x0_u = x0 as u16;
    let w_u = (x1 - x0) as u16;
    let mut row = (y0 - oy).div_euclid(ch);
    let mut next_band = oy + (row + 1) * ch;
    let mut yy = y0;
    while yy < y1 {
        let band_end = next_band.min(y1);
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
        row += 1;
        next_band += ch;
    }
}

/// Per-channel linear interpolation between two `0x00RRGGBB` colours, `t` 0..1.
#[inline]
pub(crate) const fn lerp_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    const fn mix(a: u32, b: u32, t: f32, sh: u32) -> u32 {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        ((ca + (cb - ca) * t) + 0.5) as u32
    }
    (mix(a, b, t, 16) << 16) | (mix(a, b, t, 8) << 8) | mix(a, b, t, 0)
}

/// Clamped per-channel RGB mix (`t` from `a` to `b`), rounded to nearest.
#[inline]
pub(crate) const fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    const fn ch(a: u32, b: u32, t: f32, sh: u32) -> u32 {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        ((ca + (cb - ca) * t).round().clamp(0.0, 255.0) as u32) << sh
    }
    ch(a, b, t, 16) | ch(a, b, t, 8) | ch(a, b, t, 0)
}

/// `a + (b - a) t`, `t` clamped to 0..1.
#[inline]
pub(crate) const fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// The fire ramp's stops: deep red → orange → yellow → near-white core.
const FIRE_STOPS: [(f32, u32); 5] = [
    (0.0, 0x002A_0000),
    (0.25, 0x008B_1A00),
    (0.5, 0x00E0_4A00),
    (0.75, 0x00FF_B020),
    (1.0, 0x00FF_F0C0),
];

/// The water ramp's stops: abyss → open-ocean blue → turquoise → aqua crest → foam. Saturated
/// and green-leaning through the midband (pale sky-cyan reads as ice).
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
#[allow(clippy::manual_clamp)]
#[inline]
const fn ramp_5stop(stops: &[(f32, u32); 5], t: f32) -> u32 {
    let t = t.min(1.0).max(0.0);
    let idx = (t > stops[1].0) as usize + (t > stops[2].0) as usize + (t > stops[3].0) as usize;
    let (t0, c0) = stops[idx];
    let (t1, c1) = stops[idx + 1];
    lerp_rgb(c0, c1, (t - t0) / (t1 - t0))
}

/// The black-body-ish fire palette, `t` 0 (cool, deep red) → 1 (hot, white-yellow).
#[inline]
pub(crate) const fn fire_ramp(t: f32) -> u32 {
    ramp_5stop(&FIRE_STOPS, t)
}

/// The water palette, `t` 0 (deep navy) → 1 (pale foam crest).
#[inline]
pub(crate) const fn water_ramp(t: f32) -> u32 {
    ramp_5stop(&WATER_STOPS, t)
}

/// Return the twinkle family's white (`false`) or gold (`true`) RGB colour.
#[inline]
pub(crate) const fn twinkle_rgb(gold: bool) -> u32 {
    if gold { 0x00FF_E9A8 } else { 0x00FF_FFFF }
}

/// Star arm half-thickness at the base as a fraction of the arm's half-length.
pub(crate) const STAR_WAIST: f32 = 0.10;

/// Fraction of an arm's half-length drawn at full coverage through the crossing.
pub(crate) const STAR_TAPER_BODY: f32 = 0.42;

/// Coverage at the arm's tip as a fraction of the requested coverage.
pub(crate) const STAR_TIP_COV: f32 = 0.22;

/// Maximum spans one point is rasterized with per side and arm.
pub(crate) const STAR_TAPER_STEPS: i32 = 3;

/// Coincident source-over lays stacked at the star's crossing.
pub(crate) const STAR_OVER_LAYS: usize = 5;

/// Effective source-over lay count at the star's centre.
pub(crate) const STAR_OVER_CENTRE_LAYS: f32 = 3.0;

/// Taper coverage multiplier at fraction `u` from the body's end to the point.
#[inline]
pub(crate) const fn star_taper_cov(u: f32) -> f32 {
    1.0 - (1.0 - STAR_TIP_COV) * u.clamp(0.0, 1.0)
}

/// Nucleus half-extent as a fraction of the arm's half-length.
pub(crate) const STAR_CORE: f32 = 0.16;

/// Additive nucleus's share of the star's coverage.
pub(crate) const STAR_CORE_ADD: f32 = 0.35;

/// Diagonal glint dots' offset from the centre as a fraction of the arm's half-length.
pub(crate) const STAR_GLINT: f32 = 0.5;

/// Glints' share of the star's coverage.
pub(crate) const STAR_GLINT_COV: f32 = 1.0 / 3.0;

/// Base star arm half-length as a fraction of cell height.
pub(crate) const STAR_ARM: f32 = 0.14;

/// Arm ratio, starfield grain: the smallest mark that still reads as a star.
pub(crate) const STAR_ARM_FINE: f32 = 0.75;

/// Arm ratio, the family default.
pub(crate) const STAR_ARM_STD: f32 = 1.0;

/// Arm ratio, a hero grain.
pub(crate) const STAR_ARM_HERO: f32 = 1.4;

/// Source-over arm scale multiplier relative to additive light.
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
pub(crate) const fn star_arm(cell_h: f32, ratio: f32) -> f32 {
    cell_h * STAR_ARM * ratio
}

/// Convert float arm half-length to integer px for the additive star (truncated, floored at 1).
#[inline]
pub(crate) const fn star_arm_px(arm: f32) -> i32 {
    let px = arm as i32;
    if px < 1 { 1 } else { px }
}

/// Denominator of the 1-in-N 4-point star accent share among round stardust motes.
pub(crate) const STAR_ACCENT_DEN: u32 = 16;

/// Deal a stored particle seed: `true` is the 4-point accent, `false` a round mote.
#[inline]
pub(crate) const fn star_accent(seed: u32) -> bool {
    seed.is_multiple_of(STAR_ACCENT_DEN)
}

/// Seeded stardust mote radius in px.
#[inline]
pub(crate) const fn dust_r(cell_h: f32, seed01: f32) -> f32 {
    (cell_h * (0.07 + 0.05 * seed01)).max(1.3)
}

/// Composited centre coverage of the additive star as a multiple of requested coverage.
pub(crate) const STAR_STACK_ADD: f32 = 2.0 + STAR_CORE_ADD;

/// Angular frequency (rad/s) of every twinkling star.
pub(crate) const TWINKLE_OMEGA: f32 = 9.0;

/// Minimum brightness multiplier of the twinkle pulse.
pub(crate) const TWINKLE_FLOOR: f32 = 0.55;

/// Depth of the twinkle pulse (`TWINKLE_FLOOR + TWINKLE_DEPTH == 1.0`).
pub(crate) const TWINKLE_DEPTH: f32 = 0.45;

/// Envelope fraction above which a star fires its diffraction-glint accents.
pub(crate) const TWINKLE_GLINT_FRAC: f32 = 0.92;

/// Twinkle family flash rate in Hz (`TWINKLE_OMEGA / π`).
pub(crate) const TWINKLE_FLASH_HZ: f32 = TWINKLE_OMEGA / std::f32::consts::PI;

/// WCAG 2.3.1 general-flash threshold in Hz certified by this crate.
pub const TWINKLE_FLASH_BOUND_HZ: f32 = 3.0;

/// Retuning [`TWINKLE_OMEGA`] past the photosensitivity bound stops the build.
const _: () = assert!(
    TWINKLE_FLASH_HZ <= TWINKLE_FLASH_BOUND_HZ,
    "the unified twinkle flashes over the WCAG 2.3.1 general-flash bound"
);

/// Brightness multiplier of a twinkling star at `age_s` seconds and `phase` radians.
#[inline]
pub(crate) const fn twinkle_env(age_s: f32, phase: f32) -> f32 {
    let (s, _) = crate::supernova::sin_cos_f32(age_s * TWINKLE_OMEGA + phase);
    TWINKLE_FLOOR + TWINKLE_DEPTH * s.abs()
}

/// Whether a twinkle envelope multiplier `env` is within the peak glint gate.
#[inline]
pub(crate) const fn twinkle_peak(env: f32) -> bool {
    env >= TWINKLE_FLOOR + TWINKLE_DEPTH * TWINKLE_GLINT_FRAC
}

/// Integer arm thickness (px) of the additive star for half-length `arm`.
#[inline]
pub(crate) const fn star_waist_px(arm: i32) -> i32 {
    let w = (2.0 * arm as f32 * STAR_WAIST).round() as i32;
    if w > 1 { w } else { 1 }
}

/// The additive star's integer nucleus side (px), from the shared [`STAR_CORE`].
#[inline]
pub(crate) const fn star_core_px(arm: i32) -> i32 {
    let c = (2.0 * arm as f32 * STAR_CORE).round() as i32;
    if c > 1 { c } else { 1 }
}

/// Integer full-coverage body half-length (px) of the additive star for half-length `arm`.
#[inline]
pub(crate) const fn star_body_px(arm: i32) -> i32 {
    let b = (arm as f32 * STAR_TAPER_BODY).round() as i32;
    if b < 1 {
        1
    } else if b > arm {
        arm
    } else {
        b
    }
}

/// Which paint a piece of the star takes ([`star_pieces`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum StarPart {
    /// A needle's full-coverage body through the crossing.
    Body,
    /// The nucleus.
    Core,
    /// Span `step` of a point at `taper` of the body's coverage.
    Tip { step: u8, taper: f32 },
    /// A diagonal glint dot.
    Glint,
}

/// Number of distinct paints taken by a star's pieces ([`StarPart::id`]).
pub(crate) const STAR_PAINTS: usize = 3 + STAR_TAPER_STEPS as usize;

impl StarPart {
    /// Paint index in `0..STAR_PAINTS`.
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

/// One rectangular piece `[x, x + w) × [y, y + h)` of a star relative to its centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StarPiece {
    pub(crate) part: StarPart,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: i32,
    pub(crate) h: i32,
}

/// Maximum number of pieces in one star.
pub(crate) const STAR_PIECES_MAX: usize = 3 + 4 * STAR_TAPER_STEPS as usize + 4;

/// A star's pieces in the order they are laid ([`star_pieces`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct StarPieces {
    pieces: [StarPiece; STAR_PIECES_MAX],
    n: usize,
}

impl StarPieces {
    /// Borrow the active pieces slice.
    #[must_use]
    #[inline]
    pub(crate) const fn as_slice(&self) -> &[StarPiece] {
        self.pieces.split_at(self.n).0
    }
}

impl std::ops::Deref for StarPieces {
    type Target = [StarPiece];

    #[inline]
    fn deref(&self) -> &[StarPiece] {
        self.as_slice()
    }
}

/// Compute the geometry pieces of the 4-point twinkle star of half-length `arm`.
#[must_use]
#[inline]
pub(crate) const fn star_pieces(arm: i32, gold: bool) -> StarPieces {
    if let Some(cached) = star_pieces_cached(arm, gold) {
        return *cached;
    }
    build_star_pieces(arm, gold)
}

#[inline]
const fn star_pieces_cached(arm: i32, gold: bool) -> Option<&'static StarPieces> {
    const CACHED_ARMS: usize = 16;
    const CACHE: [[StarPieces; 2]; CACHED_ARMS] = {
        let none = StarPiece {
            part: StarPart::Body,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        let empty = StarPieces {
            pieces: [none; STAR_PIECES_MAX],
            n: 0,
        };
        let mut table = [[empty; 2]; CACHED_ARMS];
        let mut idx = 0;
        while idx < CACHED_ARMS {
            let a = (idx + 1) as i32;
            table[idx] = [build_star_pieces(a, false), build_star_pieces(a, true)];
            idx += 1;
        }
        table
    };
    if arm >= 1 && arm <= CACHED_ARMS as i32 {
        Some(&CACHE[(arm - 1) as usize][gold as usize])
    } else {
        None
    }
}

#[must_use]
#[inline]
const fn build_star_pieces(arm: i32, gold: bool) -> StarPieces {
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
    let t = star_waist_px(arm);
    let half = (t - 1) / 2;
    let b = star_body_px(arm);
    out.pieces[out.n] = StarPiece {
        part: StarPart::Body,
        x: -b,
        y: -half,
        w: 2 * b + 1,
        h: t,
    };
    out.n += 1;
    out.pieces[out.n] = StarPiece {
        part: StarPart::Body,
        x: -half,
        y: -b,
        w: t,
        h: 2 * b + 1,
    };
    out.n += 1;
    let c = star_core_px(arm);
    let cc = (c - 1) / 2;
    out.pieces[out.n] = StarPiece {
        part: StarPart::Core,
        x: -cc,
        y: -cc,
        w: c,
        h: c,
    };
    out.n += 1;
    let run = arm - b;
    let steps = if run < 0 {
        0
    } else if run > STAR_TAPER_STEPS {
        STAR_TAPER_STEPS
    } else {
        run
    };
    let mut k = 0;
    while k < steps {
        let d0 = b + run * k / steps;
        let d1 = b + run * (k + 1) / steps;
        let taper = star_taper_cov(((d0 + d1) as f32 * 0.5 - b as f32) / run as f32);
        let tt_raw = (t as f32 * taper).round() as i32;
        let tt = if tt_raw > 1 { tt_raw } else { 1 };
        let th = (tt - 1) / 2;
        let len = d1 - d0;
        let part = StarPart::Tip {
            step: k as u8,
            taper,
        };
        let offs = [d0 + 1, -d1];
        let mut oi = 0;
        while oi < 2 {
            let off = offs[oi];
            out.pieces[out.n] = StarPiece {
                part,
                x: off,
                y: -th,
                w: len,
                h: tt,
            };
            out.n += 1;
            out.pieces[out.n] = StarPiece {
                part,
                x: -th,
                y: off,
                w: tt,
                h: len,
            };
            out.n += 1;
            oi += 1;
        }
        k += 1;
    }
    if gold {
        let d_raw = (arm as f32 * STAR_GLINT).round() as i32;
        let d = if d_raw > 1 { d_raw } else { 1 };
        let glints = [(-d, -d), (d, -d), (-d, d), (d, d)];
        let mut gi = 0;
        while gi < 4 {
            let (ox, oy) = glints[gi];
            out.pieces[out.n] = StarPiece {
                part: StarPart::Glint,
                x: ox,
                y: oy,
                w: 1,
                h: 1,
            };
            out.n += 1;
            gi += 1;
        }
    }
    out
}

/// Push a 4-point additive twinkle star centred at `(sx, sy)` with half-length `arm`.
#[allow(
    clippy::too_many_arguments,
    reason = "output + geometry + centre + arm + coverage + glint + colour + budget; \
              the call sites are the emitters' whole star kit and share this one shape"
)]
#[inline]
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
    #[inline]
    const fn star_paint_at(color: u32, cov_f: f32, share: f32) -> u32 {
        premul_rgb(color, (cov_f * share) as u8)
    }
    let cov_f = f32::from(cov);
    let body_paint = premul_rgb(color, cov);
    let core_paint = star_paint_at(color, cov_f, STAR_CORE_ADD);
    let glint_paint = if gold {
        star_paint_at(color, cov_f, STAR_GLINT_COV)
    } else {
        0
    };
    let mut last_tip: Option<(u8, u32)> = None;
    let mut ended = false;
    let fallback;
    let pieces = match star_pieces_cached(arm, gold) {
        Some(cached) => cached.as_slice(),
        None => {
            fallback = build_star_pieces(arm, gold);
            fallback.as_slice()
        }
    };
    let mut i = 0;
    while i < pieces.len() {
        let p = &pieces[i];
        let is_subsequent = i > 0;
        i += 1;
        let paint = match p.part {
            StarPart::Body => body_paint,
            StarPart::Core => core_paint,
            StarPart::Tip { step, taper } => {
                let tip = match last_tip {
                    Some((s, cached)) if s == step => cached,
                    _ => {
                        let c = star_paint_at(color, cov_f, taper);
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
        if is_subsequent && out.len() >= max_quads {
            return false;
        }
        push_fx_rect(out, geom, sx + p.x, sy + p.y, p.w, p.h, paint);
    }
    true
}

/// Channel-weighted lit-pixel sum of a quad batch.
#[cfg(test)]
#[inline]
#[must_use]
pub(crate) const fn ink(out: &[GlowQuad]) -> u64 {
    let mut sum = 0u64;
    let mut i = 0;
    while i < out.len() {
        let q = &out[i];
        let px = (q.w as u64) * (q.h as u64);
        let ch = (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff)) as u64;
        sum += px * ch;
        i += 1;
    }
    sum
}

/// Worst WCAG contrast against `ground` over one caret cell at `(x, y, w, h)`.
#[cfg(test)]
pub(crate) fn caret_cell_worst_contrast(
    fill: u32,
    quads: &[GlowQuad],
    cell: (i32, i32, i32, i32),
    ground: u32,
) -> f32 {
    let (cx, cy, cw, ch) = cell;
    let mut px = vec![fill & 0x00FF_FFFF; (cw * ch) as usize];
    let mut qi = 0;
    while qi < quads.len() {
        let q = &quads[qi];
        qi += 1;
        let (qx0, qy0) = (i32::from(q.x), i32::from(q.y));
        let (qx1, qy1) = (qx0 + i32::from(q.w), qy0 + i32::from(q.h));
        let y_end = qy1.min(cy + ch);
        let x_start = qx0.max(cx);
        let x_end = qx1.min(cx + cw);
        let mut y = qy0.max(cy);
        while y < y_end {
            let mut x = x_start;
            while x < x_end {
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
                x += 1;
            }
            y += 1;
        }
    }
    px.iter()
        .map(|&c| crate::color_math::contrast_ratio(c, ground))
        .fold(f32::INFINITY, f32::min)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn geom() -> Geom {
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
        let tip = f32::from(cov) * star_taper_cov(1.0);
        let midway = f32::from(cov) * star_taper_cov(0.5);
        assert!(
            (tip..=midway).contains(&(prev as f32)),
            "the outermost span carries {prev}, outside the ramp's bottom half \
             ({tip:.0}..={midway:.0}) — the point is not reaching the shared tip"
        );
        let c = star_core_px(arm);
        let core = premul_rgb(0x00FF_FFFF, (f32::from(cov) * STAR_CORE_ADD) as u8);
        assert!(
            out.iter().any(|q| i32::from(q.w) == c && q.color == core),
            "no nucleus at the shared core extent"
        );
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
    /// byte-identity oracle for the branchless [`ramp_5stop`].
    const fn walked_ramp(stops: &[(f32, u32); 5], t: f32) -> u32 {
        let t = t.clamp(0.0, 1.0);
        let mut i = 0;
        while i + 1 < stops.len() {
            let (t0, c0) = stops[i];
            let (t1, c1) = stops[i + 1];
            if t <= t1 {
                let local = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
                return lerp_rgb(c0, c1, local);
            }
            i += 1;
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
        for bits in (0..=u32::MAX).step_by(4099) {
            assert_ramps_match_walk(bits);
        }
        for anchor in [0.0f32, 0.25, 0.35, 0.5, 0.65, 0.75, 0.85, 1.0] {
            let b = anchor.to_bits();
            for bits in b.saturating_sub(16_384)..=b.saturating_add(16_384) {
                assert_ramps_match_walk(bits);
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
            f32::from_bits(1),
            -0.0,
            2.0,
            f32::MAX,
            f32::MIN,
        ] {
            assert_ramps_match_walk(t.to_bits());
        }
    }
}

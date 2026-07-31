// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor emitters' shared render kit — ONE copy of the per-cell-row quad
//! pushers, the colour ramps, and the 4-point twinkle star that the `cursor_*`
//! modules each carried privately. Two rect pushers exist ON PURPOSE and are
//! NOT interchangeable: [`push_grid_rect`] takes GRID-RELATIVE px and clamps to
//! the grid interior with `y / ch` row tags, while [`push_fx_rect`] takes
//! WINDOW-ABSOLUTE px and clamps to the effects box with origin-anchored row
//! tags — routing an emitter through the wrong one moves its light and mistags
//! its damage rows.

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;

/// Clamp a GRID-RELATIVE pixel rect to the grid interior and split it into
/// per-cell-row [`GlowQuad`]s (the renderer row-gate + CPU/GPU parity
/// invariant). Callers pass grid px — no `geom.origin_*` applied — and rows tag
/// `y / ch`: the contract of the grid-anchored emitters (comet, fireball,
/// droplet). Window-absolute emitters use [`push_fx_rect`] instead.
pub(crate) fn push_grid_rect(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
) {
    if w <= 0 || h <= 0 || premul == 0 {
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
        });
        yy = band_end;
    }
}

/// Push a pixel rect of premultiplied light in WINDOW px, CLAMPED to the
/// EFFECTS BOX (grid + head band — identity-exact at `head == 0`) and SPLIT
/// into per-cell-row [`GlowQuad`]s with origin-anchored row DAMAGE tags (the
/// renderer row-gate + CPU/GPU parity invariant). Callers pass window px
/// (`geom.origin_*` already applied); grid-relative emitters use
/// [`push_grid_rect`] instead.
pub(crate) fn push_fx_rect(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
) {
    if w <= 0 || h <= 0 || premul == 0 {
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
        // Grid-row DAMAGE HINT, anchored at origin_y (above-grid bands tag row 0).
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: premul,
        });
        yy = band_end;
    }
}

/// Per-channel linear interpolation between two `0x00RRGGBB` colours, `t` 0..1.
pub(crate) fn lerp_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |sh: u32| {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        ((ca + (cb - ca) * t) + 0.5) as u32
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// Black-body-ish FIRE ramp, `t` 0 (cool, deep red) → 1 (hot, white-yellow) —
/// the one palette behind the aurora's fire comet/curtain and the fireball
/// nucleus.
pub(crate) fn fire_ramp(t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    // deep red → orange → yellow → near-white core.
    let stops = [
        (0.0f32, 0x002A_0000u32),
        (0.25, 0x008B_1A00),
        (0.5, 0x00E0_4A00),
        (0.75, 0x00FF_B020),
        (1.0, 0x00FF_F0C0),
    ];
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

/// OCEAN ramp, `t` 0 (deep navy) → 1 (bright cyan crest, just shy of foam) —
/// the one water palette behind the aurora's fluid wake, the droplet nucleus,
/// and the word-decoration splash (ORCA_PALETTE).
pub(crate) fn water_ramp(t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    // deep-sea abyss → open-ocean blue → turquoise → vivid aqua crest → foam.
    // Deliberately SATURATED and green-leaning through the midband: the old pale
    // sky-cyan stops read as ICE (live review: "WE ARE NOT DOING ICE") — real
    // water is rich blue-green, and foam-white appears only at the very crest.
    let stops = [
        (0.0f32, 0x0005_2C48u32),
        (0.35, 0x000E_66B4),
        (0.65, 0x0014_AAC8),
        (0.85, 0x0032_DCDE),
        (1.0, 0x00C2_F2F5),
    ];
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

/// THE TWINKLE'S OWN PALETTE — the white/gold pair the typing starfield and the
/// glide sparkles have always used. Hoisted out of [`push_twinkle_star`] when the
/// star became colour-agnostic (the Nyan landing burst draws the SAME shape in a
/// rainbow band hue), so those two call sites stay byte-identical.
#[inline]
pub(crate) const fn twinkle_rgb(gold: bool) -> u32 {
    if gold { 0x00FF_E9A8 } else { 0x00FF_FFFF }
}

/// THE ONE STAR. A 4-point twinkle: a horizontal and a vertical arm crossing at
/// `(sx, sy)`, plus — when `gold` — four dim diagonal glint dots. This is the
/// only 4-point star shape any emitter draws: the Nyan typing starfield,
/// glide stars, jump-landing burst and shooting-star heads, Beam's stardust,
/// and Sparkle's star grains all come through here, so a star is a star
/// wherever it appears and only its COLOUR and SIZE change with context
/// (owner: "use the same star pattern as in the cursor trail … unify on
/// rainbows and sparkles"). Window-absolute: draws through [`push_fx_rect`].
/// Returns `false` when the quad budget ran out, checked between pushes
/// exactly as the call sites' inlined originals did, so their emission stays
/// byte-identical.
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
    push_fx_rect(out, geom, sx - arm, sy, 2 * arm + 1, 1, star); // horizontal arm
    if out.len() >= max_quads {
        return false;
    }
    push_fx_rect(out, geom, sx, sy - arm, 1, 2 * arm + 1, star); // vertical arm
    if gold {
        let d = (arm / 2).max(1);
        let dim = premul_rgb(color, cov / 3);
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

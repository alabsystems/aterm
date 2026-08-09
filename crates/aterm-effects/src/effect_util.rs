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
/// star became colour-agnostic (the rainbow kitty landing burst draws the SAME shape in a
/// rainbow band hue), so those two call sites stay byte-identical.
///
/// THE ONE GOLD. Four near-identical warm whites used to be strewn across the
/// star kit — `0x00FF_E9A8` here, `0x00FF_F2C0` on the flying shower's warm
/// grains and its shooting-star tail, `0x00FF_EED2` on the word-decoration glow
/// star, `0x00FF_F2C8` on the supernova ember star — each independently trying
/// to be "the gold" and each landing a few counts off the others. A sparkle's
/// warmth is a FAMILY property, so it lives here and nowhere else; a call site
/// that wants the warm half of the palette asks for `twinkle_rgb(true)`.
/// (The FIRE ramp's `0x00FF_F0C0` crest and `cursor_glow`'s `FRESH_INK_WARM`
/// are NOT in this family — one is a black-body stop, the other a veil tint.)
#[inline]
pub(crate) const fn twinkle_rgb(gold: bool) -> u32 {
    if gold { 0x00FF_E9A8 } else { 0x00FF_FFFF }
}

// ─── THE SPARKLE FAMILY ─────────────────────────────────────────────────────
//
// ONE SILHOUETTE, ONE ARM FAMILY, ONE PULSE LAW.
//
// Every 4-point twinkle this crate draws — additive on dark
// ([`push_twinkle_star`]), source-over on light (`cursor_glow`'s
// `push_twinkle_over`) — is the SAME MARK, and the numbers that define it live
// here rather than at the eleven call sites that used to each invent their own.
// The owner's report was that "sparkle icons don't seem like they were drawn by
// the same artist": the light star had a waist and a nucleus the dark star had
// never heard of, nine emitters passed arm ratios spanning 6x with nothing in
// common, and six different pulse frequencies ran under three different
// rectifications. A star's CONTEXT may still choose its colour, its size (from
// the named ratios below) and its phase — nothing else.

/// SILHOUETTE — the arms' HALF-THICKNESS as a fraction of the arm's half-length.
/// Both rasterizers take their arm mass from this one number: the source-over
/// star as the minor radius of each arm's falloff ellipse, the additive star as
/// the integer bar height/width (`2·arm·STAR_WAIST` rounded, floored at the 1 px
/// hairline the small grains have always been — so the mark stays proportional
/// and only the big hero grains actually fatten).
/// STARDUST, NOT A PLUS SIGN.
///
/// The waist is a fraction of the ARM LENGTH, so `waist_px = 2·arm·STAR_WAIST`.
/// At 0.34 that made every arm 68 % as thick as it was long, with a core square
/// 80 % of the arm on a side — a typographic `+` with a block in the middle,
/// which is exactly what the owner saw and named. The additive star had ALWAYS
/// been two 1-pixel hairlines; that hairline IS the stardust read.
///
/// 0.34 and 0.40 came from `push_twinkle_over`, the SOURCE-OVER helper, where
/// they were chosen because ink on white needs AREA to register at all. Pushing
/// a light-theme compromise onto the additive star — which never had that
/// problem, because every lit pixel is at full coverage — is how a delicate
/// glint became a fat cross. The unification was right; the number it unified
/// on was the wrong one of the two.
///
/// At 0.10 the waist rounds to the shipped 1 px hairline for every ordinary
/// arm and only widens on genuine hero grains, which is what the ladder is for.
pub(crate) const STAR_WAIST: f32 = 0.10;

/// SILHOUETTE — the NUCLEUS's half-extent as a fraction of the arm's half-length.
/// A sparkle has a bright centre; on source-over that has to be drawn, and on
/// additive the crossing supplies part of it and [`STAR_CORE_ADD`] completes it.
pub(crate) const STAR_CORE: f32 = 0.16;

/// The ADDITIVE nucleus's share of the star's coverage. The dark star's two arms
/// already stack to 2x coverage where they cross, so a full-coverage core rect on
/// top would put the centre at 3x and clip a gold star to white. At half the
/// arms' coverage the crossing and the core together read as the same nucleus the
/// light star draws outright, without turning every sparkle into a blown dot.
pub(crate) const STAR_CORE_ADD: f32 = 0.35;

/// SILHOUETTE — the diagonal GLINT dots' offset from the centre, as a fraction of
/// the arm's half-length (the classic four-point sparkle's secondary points).
pub(crate) const STAR_GLINT: f32 = 0.5;

/// The glints' share of the star's coverage — dim by design, so text stays
/// legible under a starfield.
pub(crate) const STAR_GLINT_COV: f32 = 1.0 / 3.0;

/// THE ONE ARM SCALE: a star's arm half-length as a fraction of the CELL HEIGHT.
/// Every emitter sizes its stars as `STAR_ARM` times one of the named ratios
/// below (see [`star_arm`]) — the family's sizes are a deliberate ladder, not
/// nine unrelated floats.
pub(crate) const STAR_ARM: f32 = 0.14;

/// Arm ratio — the STARFIELD GRAIN: the ribbon's per-cell stars at rest, the
/// landing sparkles, the flying shower's cold end. The smallest mark that still
/// reads as a star rather than as a lit pixel.
pub(crate) const STAR_ARM_FINE: f32 = 0.75;

/// Arm ratio — the FAMILY DEFAULT: shooting-star and glide heads, Beam's
/// stardust, the comet's glint, every star at full typing spine.
pub(crate) const STAR_ARM_STD: f32 = 1.0;

/// Arm ratio — a HERO grain: Sparkle's large pour grains and the erase poof's
/// one surviving plus (which multiplies this again by `erase_hero_arm`).
pub(crate) const STAR_ARM_HERO: f32 = 1.4;

/// Arm ratio — the INK TAX. Additive light buys presence with per-pixel
/// coverage; source-over ink has to buy it with AREA, so the same radius that
/// reads as a star on black reads as a speck on white. A LIGHT-theme star is its
/// dark twin's ratio times this one. (Was `RAINBOW_LAND_STAR_LIGHT_SCALE`, which
/// applied only to the landing's colour stars while the other four light stars
/// paid taxes of 1.06, 1.71, 1.86 and 1.0 — i.e. none at all.)
pub(crate) const STAR_ARM_INK: f32 = 1.55;

/// A star's arm half-length in px: the cell height, [`STAR_ARM`], and ONE of the
/// named ratios. The only way an emitter is allowed to pick a star's size.
#[inline]
pub(crate) fn star_arm(cell_h: f32, ratio: f32) -> f32 {
    cell_h * STAR_ARM * ratio
}

/// THE STARDUST LAW (owner, 2026-08-08: "make sure that the cursor sparkles
/// look like these cute small stardust like sparkles and fewer of the '+'
/// like sparkles" — reaffirming 2026-08-06's "sparks are the body, '+' is
/// the accent").
///
/// The star-kit unification gave every mark ONE silhouette — and thereby made
/// every typing-trail particle a 4-point plus, which is exactly the look the
/// owner rejected twice. The family therefore has TWO members with ONE law
/// between them: the population's BODY is round stardust (a tiny radial
/// mote), and the 4-point star is an ACCENT dealt to at most one particle in
/// [`STAR_ACCENT_DEN`] — decided from the particle's ALREADY-STORED seed so
/// no RNG stream shifts and a grain never changes shape mid-flight. Heroes
/// (the erase poof's one plus, landing stars, shooting-star heads, the
/// starfield's recruited gold) sit OUTSIDE the deal: they are singular
/// gestures, not population.
pub(crate) const STAR_ACCENT_DEN: u32 = 8;

/// Deal a stored particle seed into the accent (true → the 4-point star) or
/// the stardust body (false → a round mote). 1-in-8 = 12.5%, under the ≤15%
/// share the sparkle rebalance promised.
#[inline]
pub(crate) fn star_accent(seed: u32) -> bool {
    seed % STAR_ACCENT_DEN == 0
}

/// Stardust mote radius, px: seeded so no two motes match, floored so the
/// smallest is still a lit point. Slightly smaller than the erase poof's
/// round body — the trail is dust, the poof is debris.
#[inline]
pub(crate) fn dust_r(cell_h: f32, seed01: f32) -> f32 {
    (cell_h * (0.07 + 0.05 * seed01)).max(1.3)
}

/// THE ONE PULSE LAW — angular frequency (rad/s) of every twinkling star.
///
/// WCAG 2.3.1: this crate certifies a 3.2 Hz general-flash bound (see
/// `cursor_rainbow`'s `twinkle_flash_rate_stays_under_the_photosensitivity_bound`).
/// The envelope is RECTIFIED (`|sin|`), which DOUBLES the flash rate — two
/// bright peaks per sine cycle — so the bound applies to `ω/π`, not `ω/2π`.
/// At 9 rad/s that is [`TWINKLE_FLASH_HZ`] = 2.86 Hz, inside the bound with
/// headroom, and it is the rate the ribbon starfield already ran at.
pub(crate) const TWINKLE_OMEGA: f32 = 9.0;

/// THE ONE PULSE LAW — the envelope's floor. A star DIMS, it never blinks out:
/// a sparkle that reaches zero reads as a dropped frame, not as a twinkle.
pub(crate) const TWINKLE_FLOOR: f32 = 0.55;

/// THE ONE PULSE LAW — the envelope's depth. `TWINKLE_FLOOR + TWINKLE_DEPTH`
/// is 1.0: the law scales a star's own coverage and never exceeds it.
pub(crate) const TWINKLE_DEPTH: f32 = 0.45;

/// Where in the envelope's range a star counts as being at its PEAK — the gate
/// the diffraction-glint accents fire on. 0.92 of the range puts the glint on
/// for ~26 % of each pulse, the duty the Comet and Sparkle grains were hand-tuned
/// to before the law was shared.
pub(crate) const TWINKLE_GLINT_FRAC: f32 = 0.92;

/// The family's FLASH rate in Hz — `ω/π`, because the rectified envelope peaks
/// twice per sine cycle. Pinned under the WCAG 2.3.1 general-flash bound by
/// `twinkle_law_stays_under_the_photosensitivity_bound`.
pub(crate) const TWINKLE_FLASH_HZ: f32 = TWINKLE_OMEGA / std::f32::consts::PI;

/// THE ONE TWINKLE. A star's brightness multiplier at `age_s` seconds of life,
/// on its own `phase` (radians — seed it per star so a field blinks out of step).
///
/// Driven by AGE, never by a frame counter: an envelope stepped per frame runs
/// at whatever rate the display happens to refresh at, which made the
/// word-decoration sparkles blink at double speed on a 120 Hz panel (and put
/// them over the flash bound at 60). Pinned by
/// `sparkle_twinkle_is_frame_rate_independent`.
#[inline]
pub(crate) fn twinkle_env(age_s: f32, phase: f32) -> f32 {
    TWINKLE_FLOOR + TWINKLE_DEPTH * (age_s * TWINKLE_OMEGA + phase).sin().abs()
}

/// Is this twinkle at its PEAK — the instant a star throws its diffraction
/// glints? One gate, expressed as a fraction of the law's own range, so a
/// retuned depth cannot silently change the glint duty.
#[inline]
pub(crate) fn twinkle_peak(env: f32) -> bool {
    env >= TWINKLE_FLOOR + TWINKLE_DEPTH * TWINKLE_GLINT_FRAC
}

/// The additive star's integer arm THICKNESS (px) for an arm of half-length
/// `arm`, from the shared [`STAR_WAIST`]. Floored at the 1 px hairline the small
/// grains have always been, so the family's ordinary starfield sizes are
/// unchanged and only the hero grains carry visible mass.
#[inline]
pub(crate) fn star_waist_px(arm: i32) -> i32 {
    ((2.0 * arm as f32 * STAR_WAIST).round() as i32).max(1)
}

/// The additive star's integer NUCLEUS side (px), from the shared [`STAR_CORE`].
#[inline]
pub(crate) fn star_core_px(arm: i32) -> i32 {
    ((2.0 * arm as f32 * STAR_CORE).round() as i32).max(1)
}

/// THE ONE STAR. A 4-point twinkle: a horizontal and a vertical arm crossing at
/// `(sx, sy)` on a small NUCLEUS, plus — when `gold` — four dim diagonal glint
/// dots. This is the only 4-point star shape any emitter draws: the rainbow kitty
/// typing starfield, glide stars, jump-landing burst and shooting-star heads,
/// Beam's stardust, the Comet's debris glint and Sparkle's star grains all come
/// through here, so a star is a star wherever it appears and only its COLOUR and
/// SIZE change with context (owner: "use the same star pattern as in the cursor
/// trail … unify on rainbows and sparkles").
///
/// ONE SILHOUETTE WITH ITS LIGHT TWIN. The arms' thickness ([`STAR_WAIST`]), the
/// nucleus ([`STAR_CORE`]) and the glint offset ([`STAR_GLINT`]) are the family's,
/// shared verbatim with `cursor_glow`'s `push_twinkle_over` — the source-over
/// star that draws this same mark on a white ground. Before that, the light star
/// had a waist and a core and the dark star had neither, which is exactly what
/// "drawn by a different artist" looks like. Only the COMPOSITING differs: this
/// one is hard-edged and additive, so its crossing already supplies part of the
/// nucleus and the core rect is drawn at [`STAR_CORE_ADD`] of the arms' coverage
/// to finish it without clipping the centre to white.
///
/// Window-absolute: draws through [`push_fx_rect`]. Returns `false` when the
/// quad budget ran out, checked between pushes exactly as the call sites'
/// inlined originals did.
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
    // ARMS at the shared waist. An even thickness is biased UP-LEFT by the
    // integer half so the mark's centre stays on `(sx, sy)`, the pixel every
    // caller placed it at; at the 1 px floor this is the shipped hairline
    // verbatim.
    let t = star_waist_px(arm);
    let half = (t - 1) / 2;
    push_fx_rect(out, geom, sx - arm, sy - half, 2 * arm + 1, t, star); // horizontal
    if out.len() >= max_quads {
        return false;
    }
    push_fx_rect(out, geom, sx - half, sy - arm, t, 2 * arm + 1, star); // vertical
    if out.len() >= max_quads {
        return false;
    }
    // NUCLEUS: the crossing supplies half of it, this completes it.
    let c = star_core_px(arm);
    let ch = (c - 1) / 2;
    let core = premul_rgb(color, (f32::from(cov) * STAR_CORE_ADD) as u8);
    push_fx_rect(out, geom, sx - ch, sy - ch, c, c, core);
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

    /// ONE PULSE LAW, and it is inside the flash bound this crate certifies.
    ///
    /// The family used to run SIX frequencies (8, 9, 11, 13, 7..13 rad/s and a
    /// per-frame step) under THREE rectifications (`|sin|`, raw `sin`,
    /// `sin.max(0)`), so "how fast does a sparkle blink" had no answer. It has
    /// one now, and the rectification is part of it: `|sin|` peaks TWICE per
    /// sine cycle, so the WCAG 2.3.1 general-flash budget is spent at `ω/π`.
    /// The raw-`sin` sites were being measured against the wrong number.
    #[test]
    fn twinkle_law_stays_under_the_photosensitivity_bound() {
        assert!(
            TWINKLE_FLASH_HZ <= 3.2,
            "the unified twinkle flashes at {TWINKLE_FLASH_HZ} Hz — over the 3.2 Hz bound"
        );
        // MEASURED, not merely declared: count the envelope's bright peaks over
        // ten seconds and check the rate against the constant. A future retune
        // that drops the rectification would halve this and be caught.
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

    /// The GLINT gate is a fraction of the law's own range, so retuning the
    /// depth cannot silently change how often a star throws its diffraction
    /// points. ~26 % duty — what the Comet and Sparkle grains were hand-tuned to
    /// with their private `> 0.82` / `> 0.85` thresholds.
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

    /// ONE ARM FAMILY. Nine call sites used to pass arm ratios spanning 6x with
    /// no shared constant; the sizes are a ladder now, and this pins its shape:
    /// ordered, derived from the one scale, and (before the erase HERO band's
    /// own documented multiplier) inside 2x end to end.
    #[test]
    fn the_star_arm_ladder_is_ordered_and_derived() {
        assert!(STAR_ARM_FINE < STAR_ARM_STD && STAR_ARM_STD < STAR_ARM_HERO);
        assert!(
            STAR_ARM_HERO / STAR_ARM_FINE <= 2.0,
            "the family's own sizes span {}x",
            STAR_ARM_HERO / STAR_ARM_FINE
        );
        assert!(
            STAR_ARM_INK > 1.0,
            "a source-over star must out-size its additive twin"
        );
        for r in [STAR_ARM_FINE, STAR_ARM_STD, STAR_ARM_HERO, STAR_ARM_INK] {
            assert!((star_arm(16.0, r) - 16.0 * STAR_ARM * r).abs() < 1e-6);
        }
    }

    /// ONE SILHOUETTE, additive half. Every proportion of the mark comes from
    /// the shared constants: the arms' length and waist, the nucleus, the glint
    /// offset and its dimming. (The source-over half is checked against these
    /// same numbers by `cursor_glow`'s `a_sparkle_is_the_same_mark_on_both_grounds`.)
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
        // Arms: full span, at the shared waist.
        let t = star_waist_px(arm);
        assert_eq!(t, (2.0 * arm as f32 * STAR_WAIST).round() as i32);
        let span = |q: &GlowQuad| (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
        let horiz = out
            .iter()
            .find(|q| span(q) == (sx - arm, sx + arm + 1))
            .expect("a horizontal arm of 2*arm+1");
        assert!(
            out.iter()
                .filter(|q| span(q) == (sx - (t - 1) / 2, sx - (t - 1) / 2 + t))
                .map(|q| i32::from(q.h))
                .sum::<i32>()
                >= 2 * arm + 1,
            "the vertical arm is not 2*arm+1 tall at the shared waist"
        );
        assert_eq!(
            i32::from(horiz.h),
            t,
            "the horizontal arm left the shared waist"
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
}

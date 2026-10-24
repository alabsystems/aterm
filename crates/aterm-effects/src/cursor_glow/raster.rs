// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure raster emission: the colour rules (the rainbow spectrum, light-theme ink, the fresh-ink glyph palette) and the quad/halo pushers.

use super::*;

/// The comet debris glint's coverage as a fraction of the grain's own.
pub(super) const COMET_GLINT_COV: f32 = 0.5;

/// The seven names every point-mark of this family snaps to, red → violet (`0x00RRGGBB`): canonical ROYGBIV in spectral order.
pub(super) const RAINBOW_BANDS: [u32; SPECTRUM_STOPS] = {
    let mut bands = [0u32; SPECTRUM_STOPS];
    let mut i = 0;
    while i < SPECTRUM_STOPS {
        bands[i] = crate::spectrum::SPECTRUM_LUT[crate::spectrum::SPECTRUM_ANCHOR_AT[i]];
        i += 1;
    }
    bands
};

/// The transient over-ink coverage cap: the fresh-ink pop, crown, starfield, and the rainbow cursor's halo and twinkle.
pub(crate) const OVER_INK_COV_CAP: f32 = 47.0;

/// The caret is the brightest thing in the effect: the least WCAG relative luminance the block cursor's fill may carry while the effect is live on a dark page, on a `0..=255` scale.
pub(crate) const RAINBOW_CARET_LIGHT_FLOOR: f32 = 80.0;

/// The share of [`RAINBOW_CARET_LIGHT_FLOOR`] the sparkle field is allowed.
pub(crate) const RAINBOW_SPARKLE_LIGHT_SHARE: f32 = 0.9;

/// The sparkle field's own ceiling in additive coverage: what a starfield point-mark may ask for so its composited centre stays under [`RAINBOW_CARET_LIGHT_FLOOR`].
pub(super) const RAINBOW_SPARKLE_COV_MAX: f32 = 61.0;

/// The sparkle ceiling must be the tighter of the two bounds; a ceiling above
/// what the emitter can already ask for would be dead code.
const _: () = assert!(
    RAINBOW_SPARKLE_COV_MAX < OVER_INK_COV_CAP * 1.5,
    "the sparkle ceiling must bind before the legibility clamp"
);

/// The field's ceiling: the most light every over-ink stream together may put on one pixel outside the caret, in levels of one channel, measured over the ground.
pub(crate) const RAINBOW_FIELD_LEVEL: f32 = 145.0;

/// The field level is the level [`RAINBOW_SPARKLE_COV_MAX`] was solved against.
const _: () = assert!(RAINBOW_FIELD_LEVEL > 144.0 && RAINBOW_FIELD_LEVEL < 146.0);

/// Drift floor of the jump streak's chroma fold, in `t` (the arc's parameter): a third of the cycle, so it tracks the lay rate.
pub(super) const RAINBOW_STREAK_DRIFT_FLOOR: f32 = RAINBOW_CYCLE / 3.0;
/// Where the fold is complete: an uncorrelated reading, which across a row boundary averages `0.5` of the arc.
pub(super) const RAINBOW_STREAK_DRIFT_MAX: f32 = 0.5;

const _: () = assert!(
    RAINBOW_STREAK_DRIFT_FLOOR >= 1.0 / RAINBOW_TRAVERSE_SETTLED
        && RAINBOW_STREAK_DRIFT_FLOOR < RAINBOW_STREAK_DRIFT_MAX
        && RAINBOW_STREAK_DRIFT_MAX <= RAINBOW_CYCLE * 0.5,
    "the streak's drift floor must admit one cell's lay and leave the fold room, \
     and the fold must complete no later than the farthest two readings can be"
);

/// Ceiling on the luma (0..255, Rec. 709) of any hue composited source-over on a white ground.
pub(super) const LIGHT_INK_MAX_LUMA: f32 = 68.0;
/// Companion ceiling on any single channel.
pub(super) const LIGHT_INK_MAX_CHANNEL: f32 = 150.0;
/// Legibility ceiling on any light-theme mark's per-pixel centre over-alpha (`aterm_render::halo_over_cap`): `over_rgb(glyph, ink, cap)` keeps `(255−cap)/255` of the glyph, so near-black ink stays near-black and a white counter stays light grey.
pub(crate) const LIGHT_INK_ALPHA_CAP: f32 = 190.0;
/// Ceiling on the opacity of any body drawn under the letters on a light ground (the beam family's bodies, every crown, the comet's coma, the rod's sleeve, the typing glow): the full byte.
pub(crate) const LIGHT_BODY_ALPHA_CAP: f32 = 255.0;

/// Rec. 709 luma of a `0x00RRGGBB` colour, 0..255.
#[inline]
pub(super) const fn luma709(rgb: u32) -> f32 {
    0.2126 * ((rgb >> 16) & 0xff) as f32
        + 0.7152 * ((rgb >> 8) & 0xff) as f32
        + 0.0722 * (rgb & 0xff) as f32
}

/// Leading-only ink ([`light_ink_bold`]): one ink weight for every hue, so a sweep does not pulse light and dark, at the most saturation that weight allows.
pub(super) const LIGHT_INK_BOLD_MAX_LUMA: f32 = 120.0;
pub(super) const LIGHT_INK_BOLD_MAX_CHANNEL: f32 = 215.0;

/// [`light_ink`]'s vivid twin (see the one light-ink rule above).
#[inline]
pub(super) const fn light_ink_bold(rgb: u32) -> u32 {
    let (r, g, b) = (
        ((rgb >> 16) & 0xff) as f32,
        ((rgb >> 8) & 0xff) as f32,
        (rgb & 0xff) as f32,
    );
    let hi = r.max(g).max(b);
    let lo = r.min(g).min(b);
    if hi <= 0.5 {
        return 0;
    }
    let span = hi - lo;
    const fn pure(c: f32, lo: f32, span: f32) -> f32 {
        if span <= 1e-3 { 1.0 } else { (c - lo) / span }
    }
    let (pr, pg, pb) = (pure(r, lo, span), pure(g, lo, span), pure(b, lo, span));
    let y0 = 0.2126 * pr + 0.7152 * pg + 0.0722 * pb;
    let yt = LIGHT_INK_BOLD_MAX_LUMA / 255.0;
    let vmax = LIGHT_INK_BOLD_MAX_CHANNEL / 255.0;
    let s = if y0 >= 1.0 - 1e-4 {
        0.0
    } else {
        ((1.0 - yt / vmax) / (1.0 - y0)).clamp(0.0, 1.0)
    };
    let v = (yt / (1.0 - s + s * y0)).min(vmax);
    const fn ch(v: f32, s: f32, c: f32) -> u32 {
        ((255.0 * v * (1.0 - s + s * c)) + 0.5).clamp(0.0, 255.0) as u32
    }
    (ch(v, s, pr) << 16) | (ch(v, s, pg) << 8) | ch(v, s, pb)
}

/// Light-theme ink: `rgb` scaled uniformly (hue and saturation intact) until it satisfies both [`LIGHT_INK_MAX_LUMA`] and [`LIGHT_INK_MAX_CHANNEL`].
#[inline]
pub(super) const fn light_ink(rgb: u32) -> u32 {
    let rgb = rgb & 0x00FF_FFFF;
    let y = luma709(rgb);
    let r = (rgb >> 16) & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = rgb & 0xff;
    let hi_rg = if r > g { r } else { g };
    let hi = (if hi_rg > b { hi_rg } else { b }) as f32;
    let k = (LIGHT_INK_MAX_LUMA / y.max(1.0))
        .min(LIGHT_INK_MAX_CHANNEL / hi.max(1.0))
        .min(1.0);
    if k >= 1.0 {
        return rgb;
    }
    const fn ch(rgb: u32, k: f32, sh: u32) -> u32 {
        ((((rgb >> sh) & 0xff) as f32 * k) + 0.5) as u32
    }
    (ch(rgb, k, 16) << 16) | (ch(rgb, k, 8) << 8) | ch(rgb, k, 0)
}

/// The compositing role of one light-theme mark (see the one light-ink rule).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InkRole {
    /// Provably confined to the inter-line leading (the rail line), or drawn beneath the glyph ink (the under-ink stream: water's light wake, light lightning, the beam family's light bodies and Lumen's light glow): nothing it darkens is ever a glyph's stroke, so it may be vivid.
    Leading,
    /// Can composite over a letterform at any moment: the conservative ink and the shared legibility ceiling.
    OverText,
}

impl InkRole {
    /// This role's darkening policy.
    #[inline]
    pub(crate) const fn ink(self, rgb: u32) -> u32 {
        match self {
            Self::Leading => light_ink_bold(rgb),
            Self::OverText => light_ink(rgb),
        }
    }

    /// This role's per-pixel centre over-alpha ceiling.
    #[inline]
    pub(crate) const fn alpha_cap(self) -> f32 {
        match self {
            Self::Leading => RAINBOW_LIGHT_RAIL_ALPHA_CAP,
            Self::OverText => LIGHT_INK_ALPHA_CAP,
        }
    }
}

/// The one sweep: where a cell sits on the spectrum given its column and the ribbon's phase clock.
#[inline]
pub(crate) fn rainbow_sweep_at(col: u16, phase: f32) -> f32 {
    rainbow_sweep_reflect(col as f32 * RAINBOW_LIGHT_RAIL_SPREAD + rainbow_sweep_phase(phase))
}

/// Fold an unbounded spectrum position onto the sweep's ping-pong `0..1`, for callers that offset a resolved position (the caret's halo rings step outward along the wheel).
#[inline]
pub(crate) fn rainbow_sweep_reflect(x: f32) -> f32 {
    let raw = x.rem_euclid(2.0);
    if raw <= 1.0 { raw } else { 2.0 - raw }
}

/// Resolve a reflected spectrum position for a persistent mark — the caret's read of the same field the ribbon paints, past the cycle's turnaround.
#[inline]
pub(crate) fn rainbow_thing_of(sweep: f32) -> u32 {
    let phase = rainbow_cycle_phase(sweep);
    if phase > 1.0 {
        return rainbow_cycle_ink(phase);
    }
    spectrum(phase)
}

/// Re-pacing of the sweep's ends, off (`0.0`): the fold is the pure triangle, so every keystroke advances the arc at the mark's own rate.
pub(super) const RAINBOW_END_DWELL: f32 = 0.0;

/// The named stop nearest a sweep position ([`spectrum_snap`]), the quantized spectrum every point-mark of this family snaps to, so red and violet stay nameable on a thin underline.
#[inline]
#[cfg(test)]
pub(crate) const fn rainbow_band_of(sweep: f32) -> u32 {
    spectrum_snap(sweep)
}

/// [`rainbow_band_of`] at a column and phase (test-only: live kitty marks read the cached classic field).
#[cfg(test)]
#[inline]
pub(crate) fn rainbow_band_at(col: u16, phase: f32) -> u32 {
    rainbow_band_of(rainbow_sweep_at(col, phase))
}

/// Legibility ceiling on a rail's per-pixel centre over-alpha.
pub(super) const RAINBOW_LIGHT_RAIL_ALPHA_CAP: f32 = 236.0;

/// How fast the band ramp sweeps along the rails, per column and per unit of the ribbon's phase clock.
pub(super) const RAINBOW_LIGHT_RAIL_SPREAD: f32 = 0.045;
/// The integrated rainbow clock lives on this exact ring.
pub(crate) const RAINBOW_PHASE_RING: f32 = 1024.0;
/// The sweep's rate per unit of phase: 179 whole reflected (period-two) sweeps per [`RAINBOW_PHASE_RING`], so the ring's wrap is exact.
pub(super) const RAINBOW_LIGHT_RAIL_FLOW: f32 = 179.0 / 512.0;

/// Spectrum-clock position on the reflected sweep's period-two domain.
#[inline]
pub(super) fn rainbow_sweep_phase(phase: f32) -> f32 {
    (phase.rem_euclid(RAINBOW_PHASE_RING) * RAINBOW_LIGHT_RAIL_FLOW).rem_euclid(2.0)
}

/// Lift a standalone unit-turn animation clock onto the rainbow phase ring so one `0..1` turn traverses one whole reflected spectrum cycle, turns 0 and 1 both at sweep position zero.
#[inline]
pub(crate) fn rainbow_phase_from_unit_turn(turn: f32) -> f32 {
    ((turn.rem_euclid(1.0) * 2.0) / RAINBOW_LIGHT_RAIL_FLOW).rem_euclid(RAINBOW_PHASE_RING)
}

/// Adjacency bound for the ancillary laid-hue lattice, kept for its compile-time checks below; the live kitty field is bounded by the classic geometry.
pub(super) const RAINBOW_UNDERLINE_SWEEP_MAX: f32 = 0.22;

/// Reference step for the ancillary rainbow-kitty rolling hue clock.
pub(super) const RAINBOW_KITTY_HUE_STEP: f32 = 0.0125;

/// The shortest traverse, in cells.
pub(super) const RAINBOW_TRAVERSE_MIN_CELLS: f32 = 26.0;

/// The short mark's traverse, in cells: `1/16` of the arc per cell, so a five-char word's last cell reaches yellow (`4/16` → 45°) at `17.6°` per key, the per-cell lay stays under the adjacency ceiling (checked below), and a 4-key line's head (`3/16`) stays clear of the green-blue crossing.
pub(super) const RAINBOW_SHORT_TRAVERSE_CELLS: f32 = 16.0;

const _: () = assert!(
    (1.0 + RAINBOW_END_DWELL) / RAINBOW_SHORT_TRAVERSE_CELLS + RAINBOW_LAID_END_PULL
        <= RAINBOW_UNDERLINE_SWEEP_MAX,
    "the short walk's step must stay under the adjacency ceiling"
);

/// The settled traverse, in cells: what one red→violet pass costs once a line is long.
pub(super) const RAINBOW_TRAVERSE_SETTLED: f32 = 36.0;

/// The wrap in cells: violet back to red, the price of a head that keeps advancing.
pub(super) const RAINBOW_WRAP_CELLS: f32 = 2.0;

/// The wrap as a share of the arc (the unit the walk is stated in), so it keeps the crossing's rate at every local rate the walk takes.
pub(super) const RAINBOW_WRAP_SHARE: f32 = RAINBOW_WRAP_CELLS / RAINBOW_TRAVERSE_SETTLED;

/// One full turn of the spectrum in arc units: the red→violet arc (`1.0`) plus the wrap that closes it.
pub(super) const RAINBOW_CYCLE: f32 = 1.0 + RAINBOW_WRAP_SHARE;

const _: () = assert!(
    RAINBOW_TRAVERSE_SETTLED > 0.0 && RAINBOW_TRAVERSE_SETTLED.is_finite(),
    "the settled traverse is the walk's lower bound on advance; it may not be zero"
);
const _: () = assert!(
    78.0 / RAINBOW_WRAP_CELLS <= 1420.0 / RAINBOW_TRAVERSE_SETTLED,
    "the wrap may not move faster than the green-blue crossing it is paced against"
);

/// The walk folded into one turn of the spectrum: `[0, 1]` is the red→violet arc, `(1, `[`RAINBOW_CYCLE`]`)` is the wrap that closes it.
#[inline]
pub(super) fn rainbow_cycle_phase(p: f32) -> f32 {
    // Fast path: most callers pass a value already inside one turn, and the
    // hot ribbon calls this per slab per vertex, so avoid `rem_euclid` (a
    // library `fmod`) there.
    if (0.0..RAINBOW_CYCLE).contains(&p) {
        return p;
    }
    if !p.is_finite() {
        return 0.0;
    }
    let folded = p - RAINBOW_CYCLE * (p * RAINBOW_CYCLE_INV).floor();
    if !(0.0..RAINBOW_CYCLE).contains(&folded) {
        return 0.0;
    }
    folded
}

/// Reciprocal of [`RAINBOW_CYCLE`], so the fold is a multiply.
pub(super) const RAINBOW_CYCLE_INV: f32 = 1.0 / RAINBOW_CYCLE;

/// The cycle's colour: `spectrum` on the arc, and past it the wrap leg, violet to red the short way through magenta.
#[inline]
pub(super) fn rainbow_cycle_ink(p: f32) -> u32 {
    let phase = rainbow_cycle_phase(p);
    if phase <= 1.0 {
        spectrum(phase)
    } else {
        crate::effect_util::lerp_rgb(
            crate::spectrum::SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1],
            crate::spectrum::SPECTRUM_ANCHORS[0],
            ((phase - 1.0) / RAINBOW_WRAP_SHARE).clamp(0.0, 1.0),
        )
    }
}

/// The longest traverse, in cells: past this a long mark carries more than one traverse rather than an arc so thin that a word is one colour.
pub(super) const RAINBOW_TRAVERSE_MAX_CELLS: f32 = 144.0;

/// Phaser's hue step, in turns per typed cell: fast enough that the fat beam visibly sweeps the spectrum.
pub(super) const RAINBOW_PHASER_HUE_STEP: f32 = 0.14;

/// The rolling hue step for the non-kitty styles (sparkle, lumen, fire, laser, beam, water, comet): how fast the colour a grain or wake is dealt turns as you type.
pub(super) const RAINBOW_ROLL_HUE_STEP: f32 = 0.07;

/// The lay rate of the ancillary lattice, in fractions of one red→violet traverse per typed cell, at the reference 40-cell traverse.
pub(super) const RAINBOW_LAID_SWEEP_PER_CELL: f32 = RAINBOW_KITTY_HUE_STEP * RAINBOW_LAID_HUE_SWEEP;

/// The fastest one keystroke can lay, in fractions of a traverse: the shortest traverse ([`RAINBOW_TRAVERSE_MIN_CELLS`]) at the steepest point of the end re-pacing.
pub(super) const RAINBOW_LAID_SWEEP_MAX_PER_CELL: f32 =
    (1.0 + RAINBOW_END_DWELL) / RAINBOW_TRAVERSE_MIN_CELLS;

/// The slowest lay: the longest traverse at the re-pacing's shallowest point (the arc's ends).
pub(super) const RAINBOW_LAID_SWEEP_MIN_PER_CELL: f32 =
    (1.0 - RAINBOW_END_DWELL) / RAINBOW_TRAVERSE_MAX_CELLS;

/// Width of the end-pull's landing, strictly inside [`RAINBOW_LAID_SWEEP_MIN_PER_CELL`], so at most one cell per turnaround resolves to the exact anchor.
pub(super) const RAINBOW_LAID_END_PULL: f32 = 0.003;

/// How close to an end the lattice can land without being on it: half of one cell's lay at the fastest lay ([`RAINBOW_TRAVERSE_MIN_CELLS`]), compressed by `1 - a` under end dwell.
pub(super) const RAINBOW_LAID_END_REACH: f32 =
    (1.0 - RAINBOW_END_DWELL) * 0.5 / RAINBOW_TRAVERSE_MIN_CELLS;

/// Turns of laid hue → spectrum position: one turn of the rolling hue paints one complete red→violet→red ping-pong.
pub(super) const RAINBOW_LAID_HUE_SWEEP: f32 = 2.0;

/// The adjacency bound: no retune of step, gain, clamp or dwell may push
/// neighbouring cells further apart than [`RAINBOW_UNDERLINE_SWEEP_MAX`] (red
/// beside violet), taken at the fastest lay.
const _: () = assert!(
    RAINBOW_LAID_SWEEP_MAX_PER_CELL + RAINBOW_LAID_END_PULL <= RAINBOW_UNDERLINE_SWEEP_MAX,
    "one typed cell must step at most RAINBOW_UNDERLINE_SWEEP_MAX of the spectrum"
);

/// The end-pull's landing is narrower than the slowest lay
/// ([`RAINBOW_LAID_SWEEP_MIN_PER_CELL`]), or two consecutive keystrokes could
/// lay the same field.
const _: () = assert!(
    RAINBOW_LAID_END_PULL < RAINBOW_LAID_SWEEP_MIN_PER_CELL,
    "the end-pull's landing must be narrower than one lay at the slowest rate"
);

/// The reference pace [`RAINBOW_LAID_SWEEP_PER_CELL`] must be a pace the rate
/// rule can resolve to for some mark length, and the landing error plus one
/// fastest lay must stay inside the adjacency ceiling.
const _: () = assert!(
    RAINBOW_LAID_SWEEP_MIN_PER_CELL <= RAINBOW_LAID_SWEEP_PER_CELL
        && RAINBOW_LAID_SWEEP_PER_CELL <= RAINBOW_LAID_SWEEP_MAX_PER_CELL
        && RAINBOW_LAID_END_REACH + RAINBOW_LAID_SWEEP_MAX_PER_CELL <= RAINBOW_UNDERLINE_SWEEP_MAX,
    "the reference pace must be reachable and its landing error inside the ceiling"
);

/// The contrast ratio the tint aims for against the theme's own ground: WCAG AA for small text.
pub(super) const FRESH_INK_GLYPH_TARGET_RATIO: f32 = 4.5;
/// Below this ratio the theme has no usable side — an equiluminant or near-invisible fg/bg pair — and the tint suppresses itself entirely rather than inventing a side the theme does not have.
pub(super) const FRESH_INK_GLYPH_MIN_THEME_RATIO: f32 = 1.5;
/// The ink must be a hue (max channel − min channel) and must differ visibly from the foreground it replaces, or the substitution buys nothing.
pub(super) const FRESH_INK_GLYPH_MIN_SPREAD: i32 = 24;
pub(super) const FRESH_INK_GLYPH_MIN_DELTA: i32 = 24;
/// Bisection depth on the one-parameter ink family.
pub(super) const FRESH_INK_GLYPH_BISECT: usize = 24;

/// WCAG relative luminance of a `0x00RRGGBB` colour, with the piecewise sRGB transfer (linear toe below 0.03928, 2.4 power above); a plain power law differs enough near black to move a ratio across the AA line.
#[cfg(test)]
#[inline]
pub(super) fn rel_luminance(c: u32) -> f32 {
    crate::color_math::relative_luminance(c)
}

/// WCAG contrast ratio between two `0x00RRGGBB` colours.
#[cfg(test)]
#[inline]
pub(super) fn wcag_ratio(a: u32, b: u32) -> f32 {
    let (x, y) = (rel_luminance(a), rel_luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// Per-channel max / min of two colours, repacked: the corners of the box the whole tint path lives in (see [`glyph_ink_bound`]).
#[inline]
pub(super) const fn chan_max(a: u32, b: u32) -> u32 {
    let ar = (a >> 16) & 0xff;
    let br = (b >> 16) & 0xff;
    let ag = (a >> 8) & 0xff;
    let bg = (b >> 8) & 0xff;
    let ab = a & 0xff;
    let bb = b & 0xff;
    let r = if ar > br { ar } else { br };
    let g = if ag > bg { ag } else { bg };
    let bl = if ab > bb { ab } else { bb };
    (r << 16) | (g << 8) | bl
}
#[inline]
pub(super) const fn chan_min(a: u32, b: u32) -> u32 {
    let ar = (a >> 16) & 0xff;
    let br = (b >> 16) & 0xff;
    let ag = (a >> 8) & 0xff;
    let bg = (b >> 8) & 0xff;
    let ab = a & 0xff;
    let bb = b & 0xff;
    let r = if ar < br { ar } else { br };
    let g = if ag < bg { ag } else { bg };
    let bl = if ab < bb { ab } else { bb };
    (r << 16) | (g << 8) | bl
}
/// Chroma presence: how far apart this colour's extreme channels are.
#[inline]
pub(super) const fn chan_spread(c: u32) -> i32 {
    let (r, g, b) = (
        ((c >> 16) & 0xff) as i32,
        ((c >> 8) & 0xff) as i32,
        (c & 0xff) as i32,
    );
    let hi_rg = if r > g { r } else { g };
    let hi = if hi_rg > b { hi_rg } else { b };
    let lo_rg = if r < g { r } else { g };
    let lo = if lo_rg < b { lo_rg } else { b };
    hi - lo
}
/// Largest per-channel difference — "is this visibly a different colour".
#[inline]
pub(super) const fn chan_dist(a: u32, b: u32) -> i32 {
    let dr = (((a >> 16) & 0xff) as i32 - ((b >> 16) & 0xff) as i32).abs();
    let dg = (((a >> 8) & 0xff) as i32 - ((b >> 8) & 0xff) as i32).abs();
    let db = ((a & 0xff) as i32 - (b & 0xff) as i32).abs();
    let hi_rg = if dr > dg { dr } else { dg };
    if hi_rg > db { hi_rg } else { db }
}

/// Which side of the ground the theme's own ink sits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum GlyphInkSide {
    /// Dark ink on a light ground: the tint mixes down toward black.
    Dark,
    /// Light ink on a dark ground: the tint mixes up toward white.
    Light,
}

/// The side the theme's ink is on and the luminance every point of the tint path must satisfy, or `None` when this theme gets no tint.
#[inline]
pub(super) fn glyph_ink_bound(fg: u32, bg: u32) -> Option<(GlyphInkSide, f32)> {
    // A sentinel (or any set high byte) is not a colour: masking it would read
    // as pure black and flip the side.
    if fg > 0x00FF_FFFF || bg > 0x00FF_FFFF || fg == bg {
        return None;
    }
    let table = crate::color_math::relative_luminance_table();
    let (lfg, lbg) = (
        crate::color_math::relative_luminance_with(table, fg),
        crate::color_math::relative_luminance_with(table, bg),
    );
    let (hi, lo) = if lfg > lbg { (lfg, lbg) } else { (lbg, lfg) };
    let theme = (hi + 0.05) / (lo + 0.05);
    if !theme.is_finite() || theme < FRESH_INK_GLYPH_MIN_THEME_RATIO {
        return None;
    }
    if lfg < lbg {
        let aa = (lbg + 0.05) / FRESH_INK_GLYPH_TARGET_RATIO - 0.05;
        Some((GlyphInkSide::Dark, aa.max(lfg)))
    } else {
        let aa = FRESH_INK_GLYPH_TARGET_RATIO * (lbg + 0.05) - 0.05;
        Some((GlyphInkSide::Light, aa.min(lfg)))
    }
}

/// One band as glyph ink for this theme: saturated, then bisected along a one-parameter family toward black (dark side) or white (light side) until the box corner against the foreground satisfies the bound.
#[inline]
pub(super) fn fresh_ink_glyph_ink(band: u32, fg: u32, side: GlyphInkSide, cap: f32) -> Option<u32> {
    let (r, g, b) = (
        ((band >> 16) & 0xff) as f32,
        ((band >> 8) & 0xff) as f32,
        (band & 0xff) as f32,
    );
    let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
    if hi <= 0.5 {
        return None;
    }
    let span = (hi - lo).max(1e-3);
    let sat = |c: f32| ((c - lo) / span * hi).clamp(0.0, 255.0);
    let s = [sat(r), sat(g), sat(b)];
    let table = crate::color_math::relative_luminance_table();
    let fg_rgb = [16u32, 8, 0].map(|sh| ((fg >> sh) & 0xff) as usize);
    let cand_rgb = |k: f32| -> [usize; 3] {
        s.map(|c| {
            let v = match side {
                GlyphInkSide::Dark => c * k,
                GlyphInkSide::Light => c + (255.0 - c) * (1.0 - k),
            };
            ((v + 0.5) as usize).min(255)
        })
    };
    let pack = |[r, g, b]: [usize; 3]| ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
    let ok = |ink: [usize; 3]| -> bool {
        let corner = match side {
            GlyphInkSide::Dark => [
                ink[0].max(fg_rgb[0]),
                ink[1].max(fg_rgb[1]),
                ink[2].max(fg_rgb[2]),
            ],
            GlyphInkSide::Light => [
                ink[0].min(fg_rgb[0]),
                ink[1].min(fg_rgb[1]),
                ink[2].min(fg_rgb[2]),
            ],
        };
        let l = crate::color_math::relative_luminance_rgb(table, corner[0], corner[1], corner[2]);
        match side {
            GlyphInkSide::Dark => l <= cap,
            GlyphInkSide::Light => l >= cap,
        }
    };
    let c1 = cand_rgb(1.0);
    if ok(c1) {
        return Some(pack(c1));
    }
    let c0 = cand_rgb(0.0);
    if !ok(c0) {
        return None;
    }
    let (mut lo_k, mut hi_k) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (c0, c1);
    for _ in 0..FRESH_INK_GLYPH_BISECT {
        let mid = 0.5 * (lo_k + hi_k);
        let mid_rgb = cand_rgb(mid);
        if ok(mid_rgb) {
            lo_k = mid;
            lo_rgb = mid_rgb;
        } else {
            hi_k = mid;
            hi_rgb = mid_rgb;
        }
        if lo_rgb == hi_rgb {
            break;
        }
    }
    Some(pack(lo_rgb))
}

/// One memoized fresh-ink glyph palette: the `(theme_fg, theme_bg)` key and the seven band inks, or `None` for a memoized refusal (see [`fresh_ink_glyph_palette`]).
pub(super) type GlyphPaletteMemo = ((u32, u32), Option<[u32; RAINBOW_BANDS.len()]>);

/// The seven band inks for one theme, or `None` when this theme gets no tint.
pub(super) fn fresh_ink_glyph_palette(fg: u32, bg: u32) -> Option<[u32; RAINBOW_BANDS.len()]> {
    let (side, cap) = glyph_ink_bound(fg, bg)?;
    let mut out = [0u32; RAINBOW_BANDS.len()];
    for (slot, &band) in out.iter_mut().zip(RAINBOW_BANDS.iter()) {
        let ink = fresh_ink_glyph_ink(band, fg, side, cap)?;
        if chan_spread(ink) < FRESH_INK_GLYPH_MIN_SPREAD
            || chan_dist(ink, fg) < FRESH_INK_GLYPH_MIN_DELTA
        {
            return None;
        }
        *slot = ink;
    }
    Some(out)
}

/// Radius growth per unit of impact above 1: ×2.0 at the 3.5 cap (`1 + 0.4 · 2.5`).
pub(super) const CLASSIC_RING_RADIUS_PER_IMPACT: f32 = 0.4;
/// Life growth per unit of impact above 1: the built-in 180 ms reaches 300 ms at the cap (`0.18 · (1 + 0.267 · 2.5)`).
pub(super) const CLASSIC_RING_LIFE_PER_IMPACT: f32 = 0.267;

/// How much wider a ring of impact `scale` expands: `1.0` at impact 1 (and for anything non-finite or below), `2.0` at the 3.5 cap.
#[inline]
pub(super) const fn classic_ring_radius_factor(scale: f32) -> f32 {
    if !scale.is_finite() {
        return 1.0;
    }
    let impact = scale.clamp(1.0, crate::rainbow_kitty::timing::IMPACT_MAX);
    1.0 + CLASSIC_RING_RADIUS_PER_IMPACT * (impact - 1.0)
}

/// A ring's life in seconds: `base` graded by impact `scale` — `base` itself at impact 1, `base · 1.667` at the cap (0.18 s → 0.30 s).
#[inline]
pub(super) const fn classic_ring_life(base: f32, scale: f32) -> f32 {
    if !scale.is_finite() {
        return base;
    }
    let impact = scale.clamp(1.0, crate::rainbow_kitty::timing::IMPACT_MAX);
    base * (1.0 + CLASSIC_RING_LIFE_PER_IMPACT * (impact - 1.0))
}

/// The comet colour for `style` at path position `pos` (0 tail .. 1 head): the single ramp shared by the live animator ([`CursorGlow::comet_color`]) and the settings-card demo.
#[inline]
pub fn style_comet_color(style: GlowStyle, color: u32, accent: u32, hue: f32, pos: f32) -> u32 {
    match style {
        GlowStyle::Lumen => lerp_rgb(accent, color, pos),
        GlowStyle::Comet => {
            let dusty = lerp_rgb(accent, 0x0000_0000, 0.45);
            let body = lerp_rgb(dusty, color, pos);
            if pos > 0.75 {
                lerp_rgb(body, 0x00F0_FAFF, (pos - 0.75) * 4.0 * 0.80)
            } else {
                body
            }
        }
        GlowStyle::Phaser => hsv2rgb((hue + pos).fract(), 0.95, 1.0),
        GlowStyle::Classic => crate::classic_wake::comet_color(false, color, accent, hue, pos),
        GlowStyle::Sparkle => hsv2rgb((hue + pos * 0.5).fract(), 0.95, 1.0),
        GlowStyle::RainbowKitty => rainbow_cycle_ink(pos),
        GlowStyle::Fire => fire_ramp((pos).min(0.83)),
        GlowStyle::Laser => lerp_rgb(0x0000_0000, color, 0.70 + 0.30 * pos),
        GlowStyle::Beam => lerp_rgb(0x0000_0000, color, 0.60 + 0.40 * pos),
        GlowStyle::Water => water_ramp(pos),
    }
}

/// The particle colour for `style` at remaining life `fade` (1 fresh → 0 dead) with per-particle seed `hue` — shared by the live ember/droplet/spark emitters and the settings-card effect demo.
#[inline]
pub fn style_particle_color(style: GlowStyle, color: u32, hue: f32, fade: f32) -> u32 {
    match style {
        GlowStyle::Fire => fire_ramp(fade.min(0.83)),
        GlowStyle::Water => water_ramp(0.35 + 0.55 * fade + 0.1 * hue),
        GlowStyle::Laser => lerp_rgb(color, 0x00FF_FFFF, 0.55 * fade),
        GlowStyle::Beam => lerp_rgb(color, 0x00FF_FFFF, 0.35 + 0.45 * fade),
        GlowStyle::Comet => lerp_rgb(color, 0x00FF_FFFF, 0.65 * fade),
        GlowStyle::RainbowKitty => {
            lerp_rgb(spectrum_snap(hue.rem_euclid(1.0)), 0x00FF_FFFF, 0.35 * fade)
        }
        _ => lerp_rgb(hsv2rgb(hue, 0.85, 1.0), 0x00FF_FFFF, 0.35 * fade),
    }
}

#[inline]
#[allow(
    clippy::too_many_arguments,
    reason = "centre + per-axis radii + colour + peak; a param struct would obscure the geometry at the many emitter call sites"
)]
pub(crate) fn push_halo_over(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    color: u32,
    peak: u8,
) {
    if peak < 96 {
        return;
    }
    let scale = (peak as f32 / 255.0).max(0.55);
    push_halo_rows(
        out,
        geom,
        (cx, cy),
        (rx * scale, ry * scale),
        color,
        HaloMode::Over,
    );
}

/// Fit a round glow's centre and radii inside the effects box so its radial falloff fades to zero before the box edge.
#[inline]
pub(crate) fn fit_glow_in_box(
    geom: Geom,
    (cx, cy): (f32, f32),
    (rx, ry): (f32, f32),
) -> ((f32, f32), (f32, f32)) {
    let fit = |c: f32, r: f32, lo: f32, hi: f32, step: f32| {
        let mut c = c;
        if c - r < lo {
            c = (c + step).min(lo + r).min(hi);
        }
        if c + r > hi {
            c = (c - step).max(hi - r).max(lo);
        }
        (c, (c - lo).min(hi - c).max(0.0))
    };
    let (cx, room_x) = fit(
        cx,
        rx,
        geom.fx_left() as f32,
        geom.fx_right() as f32,
        geom.cw as f32,
    );
    let (cy, room_y) = fit(
        cy,
        ry,
        geom.fx_top() as f32,
        geom.fx_bot() as f32,
        geom.ch as f32,
    );
    let k = (room_x / rx.max(1e-3))
        .min(room_y / ry.max(1e-3))
        .clamp(0.0, 1.0);
    ((cx, cy), (rx * k, ry * k))
}

/// Clamp a round glow's radii inside the effects box around a fixed centre.
#[inline]
pub(crate) fn hold_glow_radii_in_box(
    geom: Geom,
    (cx, cy): (f32, f32),
    (rx, ry): (f32, f32),
) -> (f32, f32) {
    let room = |c: f32, lo: i32, hi: i32| (c - lo as f32).min(hi as f32 - c).max(1.0);
    (
        rx.min(room(cx, geom.fx_left(), geom.fx_right())),
        ry.min(room(cy, geom.fx_top(), geom.fx_bot())),
    )
}

/// How many nested veils a soft veil stacks ([`push_soft_veil`]).
pub(crate) const SOFT_VEIL_LAYERS: usize = 10;

/// A source-over veil that falls off radially from its centre to nothing at its rim, in `ink` with at most `cap` of opacity at the centre.
pub(crate) fn push_soft_veil(
    halos: &mut Vec<RainHalo>,
    geom: Geom,
    centre: (f32, f32),
    radii: (f32, f32),
    ink: u32,
    cap: u8,
) {
    let ((cx, cy), (rx, ry)) = fit_glow_in_box(geom, centre, radii);
    if rx < 1.0 || ry < 1.0 || cap == 0 {
        return;
    }
    let a = f32::from(cap) / 255.0;
    let layer = 1.0 - (1.0 - a).powf(1.0 / SOFT_VEIL_LAYERS as f32);
    let layer_cap = ((layer * 255.0).round() as u32).clamp(1, u32::from(cap));
    for k in 0..SOFT_VEIL_LAYERS {
        let shrink = 1.0 - k as f32 / SOFT_VEIL_LAYERS as f32;
        push_halo_rows(
            halos,
            geom,
            (cx, cy),
            (rx * shrink, ry * shrink),
            (ink & 0x00FF_FFFF) | (layer_cap << 24),
            HaloMode::Over,
        );
    }
}

/// Share of the theme's text contrast on its paper that a light band keeps under a wide glyph.
pub(crate) const LIGHT_WIDE_BED_KEEP: f32 = 0.75;

/// The one dash: a velocity-aligned two-vertex AA beam trailing `trail_s` seconds of a particle's live ballistic motion behind `(x, y)`, shared by the laser ablation spark, the rainbow kitty shooting-star body and the sparkle mini-comet.
#[inline]
#[allow(
    clippy::too_many_arguments,
    reason = "output + geometry + position + live velocity + trail/thickness knobs + \
              the two tints + coverage; the dash IS this parameter set"
)]
pub(super) fn push_velocity_dash(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    trail_s: f32,
    thick: f32,
    tail: u32,
    head: u32,
    cov: u8,
) {
    let verts = dash_verts(x, y, vx, vy, trail_s, tail, head, cov);
    comet_beam(out, geom.beam_clip(), &verts, thick, 1, 0.0);
}

/// [`push_velocity_dash`] on a light ground: the same dash painted source-over ([`comet_beam_over`]) in `ink` at opacity `alpha` (its tail at a quarter of it), capped at the may-touch-text ceiling ([`LIGHT_INK_ALPHA_CAP`]).
#[inline]
#[allow(
    clippy::too_many_arguments,
    reason = "the dash's parameter set, with one ink in place of two tints"
)]
pub(super) fn push_velocity_dash_over(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    trail_s: f32,
    thick: f32,
    ink: u32,
    alpha: u8,
) {
    let alpha = alpha.min(LIGHT_INK_ALPHA_CAP as u8);
    let verts = dash_verts(x, y, vx, vy, trail_s, ink, ink, alpha);
    comet_beam_over(out, geom.beam_clip(), &verts, thick, 1, 0.0);
}

/// The dash's two vertices: the tail `trail_s` seconds back along the live
/// velocity at a quarter of `cov`, the head at `(x, y)` at full.
#[inline]
#[allow(
    clippy::too_many_arguments,
    reason = "position + live velocity + trail + the two tints + coverage"
)]
const fn dash_verts(
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    trail_s: f32,
    tail: u32,
    head: u32,
    cov: u8,
) -> [BeamVertex; 2] {
    [
        BeamVertex {
            x: x - vx * trail_s,
            y: y - vy * trail_s,
            color: tail,
            cov: cov / 4,
        },
        BeamVertex {
            x,
            y,
            color: head,
            cov,
        },
    ]
}

/// A light-ground grain: one source-over mote of radius `r` at `(cx, cy)` in the may-touch-text ink ([`InkRole::OverText`] of `rgb`), its opacity `alpha` capped at that role's ceiling ([`LIGHT_INK_ALPHA_CAP`]) through the colour's high byte, so the falloff's opaque centre never reads above it.
#[inline]
pub(super) fn push_light_mote(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    (cx, cy): (f32, f32),
    r: f32,
    rgb: u32,
    alpha: u8,
) {
    let role = InkRole::OverText;
    let cap = u32::from(alpha).min(role.alpha_cap() as u32);
    if cap < 4 {
        return;
    }
    let color = (role.ink(rgb) & 0x00FF_FFFF) | (cap << 24);
    push_halo_rows(out, geom, (cx, cy), (r, r), color, HaloMode::Over);
}

/// A bead of ink on paper at `(cx, cy)`: a round disc of radius `r` in the `rim` ink with a disc of the `core` ink inside it (three fifths of the radius), both anti-aliased at their edges and laid source-over at opacity `alpha`, one pixel row at a time.
pub(super) fn push_paper_bead(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    r: f32,
    (rim, core): (u32, u32),
    alpha: u8,
) {
    if alpha < 4 || r < 0.75 {
        return;
    }
    let mut disc = |rr: f32, rgb: u32| {
        let rr2 = rr * rr;
        let (y0, y1) = ((cy - rr).floor() as i32, (cy + rr).ceil() as i32);
        for y in y0..y1 {
            let dy = y as f32 + 0.5 - cy;
            let rest = rr2 - dy * dy;
            if rest <= 0.0 {
                continue;
            }
            let half = rest.sqrt();
            let rows = (rr - dy.abs() + 0.5).clamp(0.0, 1.0);
            let (xl, xr) = (cx - half, cx + half);
            let mut lay = |x: i32, w: i32, f: f32| {
                let a = (f32::from(alpha) * (f * rows).clamp(0.0, 1.0)) as u8;
                if a > 0 && w > 0 {
                    push_fx_quad(out, geom, x, y, w, 1, premul_rgb(rgb, a), a);
                }
            };
            let (pl, pr) = (xl.floor() as i32, (xr.ceil() as i32) - 1);
            if pl == pr {
                lay(pl, 1, xr - xl);
                continue;
            }
            lay(pl, 1, (pl + 1) as f32 - xl);
            if pr > pl + 1 {
                lay(pl + 1, pr - pl - 1, 1.0);
            }
            lay(pr, 1, xr - pr as f32);
        }
    };
    disc(r, rim);
    disc(r * 0.6, core);
}

/// The crescent's shadow disc offset as a share of the moon's radius from its centre.
pub(super) const CRESCENT_SHADOW_OFFSET: (f32, f32) = (0.42, -0.30);
/// The crescent's shadow disc radius, as a share of the moon's.
pub(super) const CRESCENT_SHADOW_R: f32 = 0.84;

/// A crescent moon of radius `r` at `(cx, cy)`: the lit disc minus a shadow disc ([`CRESCENT_SHADOW_R`] of its radius, at [`CRESCENT_SHADOW_OFFSET`] from its centre), so a thick limb tapers into two horns.
pub(super) fn push_crescent(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    r: f32,
    rgb: u32,
    cov: u8,
    over: bool,
) {
    if cov == 0 || r < 1.0 {
        return;
    }
    let (sx, sy) = (
        cx + CRESCENT_SHADOW_OFFSET.0 * r,
        cy + CRESCENT_SHADOW_OFFSET.1 * r,
    );
    let sr = CRESCENT_SHADOW_R * r;
    let mut span = |y: i32, xl: f32, xr: f32| {
        if xr - xl < 0.05 {
            return;
        }
        let mut lay = |x: i32, w: i32, f: f32| {
            let a = (f32::from(cov) * f.clamp(0.0, 1.0)) as u8;
            if a > 0 {
                let alpha = if over { a } else { 0 };
                push_fx_quad(out, geom, x, y, w, 1, premul_rgb(rgb, a), alpha);
            }
        };
        let (pl, pr) = (xl.floor() as i32, (xr.ceil() as i32) - 1);
        if pl == pr {
            lay(pl, 1, xr - xl);
            return;
        }
        lay(pl, 1, (pl + 1) as f32 - xl);
        if pr > pl + 1 {
            lay(pl + 1, pr - pl - 1, 1.0);
        }
        lay(pr, 1, xr - pr as f32);
    };
    let (r2, sr2) = (r * r, sr * sr);
    let (y0, y1) = ((cy - r).floor() as i32, (cy + r).ceil() as i32);
    for y in y0..y1 {
        let yc = y as f32 + 0.5;
        let dy = yc - cy;
        if dy.abs() >= r {
            continue;
        }
        let half = (r2 - dy * dy).sqrt();
        let (xl, xr) = (cx - half, cx + half);
        let ds = yc - sy;
        if ds.abs() >= sr {
            span(y, xl, xr);
            continue;
        }
        let shalf = (sr2 - ds * ds).sqrt();
        let (sl, sr_x) = (sx - shalf, sx + shalf);
        span(y, xl, xr.min(sl));
        span(y, xl.max(sr_x), xr);
    }
}

/// Push one radial halo of premultiplied peak light: its rect is clamped to the window interior and split into per-cell-row [`RainHalo`] quads sharing one centre and falloff radii.
#[inline]
pub(super) fn push_halo(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    peak: u32,
) {
    if peak == 0 {
        return;
    }
    push_halo_rows(out, geom, (cx, cy), (rx, ry), peak, HaloMode::Add);
}

/// The radial-halo band walk behind [`push_halo`], [`push_halo_over`] and the kitty cursor's exit veils: the halo at `(cx, cy)` with radii `(rx, ry)` (rounded, floored to 1) is clamped to the effects box (grid + head band) and split into per-cell-row [`RainHalo`]s in `mode`, sharing centre and radii.
pub(crate) fn push_halo_rows(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    (cx, cy): (f32, f32),
    (rx, ry): (f32, f32),
    color: u32,
    mode: HaloMode,
) {
    let rxi = (rx.round() as i32).max(1);
    let ryi = (ry.round() as i32).max(1);
    let (cxi, cyi) = (cx.round() as i32, cy.round() as i32);
    let x0 = (cxi - rxi).max(geom.fx_left());
    let x1 = (cxi + rxi).min(geom.fx_right());
    let y0 = (cyi - ryi).max(geom.fx_top());
    let y1 = (cyi + ryi).min(geom.fx_bot());
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    if cxi < 0 || cyi < 0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = geom.origin_y as i32;
    let x0_u = x0 as u16;
    let w_u = (x1 - x0) as u16;
    let cx_u = cxi as u16;
    let cy_u = cyi as u16;
    let rx_u = rxi as u16;
    let ry_u = ryi as u16;
    let mut yy = y0;
    while yy < y1 {
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
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
    }
}

/// A round stardust grain on dark ground: a radial mote drawn at the composited light of the four-point plus it stands in for.
#[inline]
pub(super) fn push_dust_halo(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cx: f32,
    cy: f32,
    r: f32,
    rgb: u32,
    cov: u8,
) {
    if cov == 0 {
        return;
    }
    push_halo(out, geom, cx, cy, r, r, premul_rgb(rgb, cov));
    let core = (r * STAR_CORE).max(1.0);
    let hot = (f32::from(cov) * (crate::effect_util::STAR_STACK_ADD - 1.0)).min(255.0) as u8;
    push_halo(out, geom, cx, cy, core, core, premul_rgb(rgb, hot));
}

/// Push a flattened elliptical annulus at `(cx, cy)` with outer radii `(rx, ry)` and a `thick` px rim, faded near the effects box edges.
pub(crate) fn push_ripple_ring(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    (rx, ry): (f32, f32),
    thick: f32,
    premul: u32,
) {
    let rslab = 1;
    let inner_rx = (rx - thick).max(0.0);
    let inner_ry = ry * (inner_rx / rx);
    let (cxi, cyi) = (cx as i32, cy as i32);
    let ry_i = ry as i32;
    let mut dy = -ry_i;
    while dy < ry_i {
        let h = rslab.min(ry_i - dy);
        let ym = (dy as f32 + h as f32 * 0.5) / ry;
        let outer = rx * (1.0 - ym * ym).max(0.0).sqrt();
        let inner = if inner_ry > 0.5 {
            let yin = (dy as f32 + h as f32 * 0.5) / inner_ry;
            inner_rx * (1.0 - yin * yin).max(0.0).sqrt()
        } else {
            0.0
        };
        let (o, i) = (outer as i32, inner as i32);
        if o > i {
            let y = cyi + dy;
            let fade_y = crate::effect_util::fx_fade_y(geom, y as f32 + 0.5 * h as f32);
            let mid = 0.5 * (o + i) as f32;
            for (x, xm) in [(cxi - o, cx - mid), (cxi + i, cx + mid)] {
                let fade = fade_y * crate::effect_util::fx_fade_x(geom, xm);
                if fade > 0.0 {
                    push_rect(
                        out,
                        geom,
                        x,
                        y,
                        o - i,
                        h,
                        crate::effect_util::scale_premul(premul, fade),
                    );
                }
            }
        }
        dy += h;
    }
}

/// Fractional part `x − ⌊x⌋`: unlike `f32::fract`, never negative.
#[inline]
pub(super) fn fract(x: f32) -> f32 {
    x - x.floor()
}

/// A lightning bolt's brightness at `u` (0 born → 1 dead) of its life: one flash.
#[inline]
pub(super) const fn bolt_flash(u: f32) -> f32 {
    if u < BOLT_HOLD {
        1.0
    } else {
        1.0 - BOLT_FADE * smoothstep01((u - BOLT_HOLD) / (1.0 - BOLT_HOLD))
    }
}

/// How long a bolt holds its full strike, as a share of its life.
pub(super) const BOLT_HOLD: f32 = 0.15;
/// How far a bolt's brightness eases down across the rest of its life.
pub(super) const BOLT_FADE: f32 = 0.60;
/// Where in a bolt's life its channel starts retracting from its root.
pub(super) const BOLT_BURN_START: f32 = 0.55;

/// The beam power envelope: full power for the first 40% of a spark's life, then one cosine down to zero.
#[inline]
pub(super) fn beam_power(frac: f32) -> f32 {
    if frac < 0.40 {
        1.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * (frac - 0.40) / 0.60).cos())
    }
}

/// HSV (all `0..1`; `h` in turns, any real, wrapped) → `0x00RRGGBB`: the colour wheel the classic wake and the phaser turn.
#[inline]
pub(crate) fn hsv2rgb(h: f32, s: f32, v: f32) -> u32 {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let u = |c: f32| ((c.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32;
    (u(r) << 16) | (u(g) << 8) | u(b)
}

/// Cubic smoothstep `0..1` (Hermite): the classic AA/feather easing.
#[inline]
pub(super) const fn smoothstep01(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

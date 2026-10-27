// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIGHT-GROUND LAW: every emitter has a light mode of its own.

use super::*;
use crate::color_math::relative_luminance;

/// The contrast bar every legibility ceiling in the cursor family is solved against: the theme's text over the light a body lays under it, 5.25:1 (the rainbow ribbon's L3).
pub(crate) const BODY_CONTRAST_BAR: f32 = 5.25;

/// The margin past [`BODY_CONTRAST_BAR`] for the emitter's own rounding (one level of composite rounding and one Bayer level of dither).
pub(crate) const BODY_CONTRAST_GUARD: f32 = 0.05;

/// The darkest a light-ground body may composite, in WCAG relative luminance: the theme's text keeps [`BODY_CONTRAST_BAR`] (plus the guard) over it.
#[inline]
#[must_use]
pub(crate) const fn light_body_floor(theme_fg: u32) -> f32 {
    light_floor_at(theme_fg, BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD)
}

/// The darkest a light-ground mark may composite, in WCAG relative luminance, for `theme_fg` to keep `bar` over it.
#[inline]
#[must_use]
pub(crate) const fn light_floor_at(theme_fg: u32, bar: f32) -> f32 {
    bar * (relative_luminance(theme_fg) + 0.05) - 0.05
}

/// A walk toward white by `w`: `c + w · (255 − c)`. Channel differences
/// all scale by `1 − w`, so the hue direction is kept and only saturation falls.
#[inline]
const fn toward_white(rgb: u32, w: f32) -> u32 {
    let ch16 = ((rgb >> 16) & 0xff) as f32;
    let ch8 = ((rgb >> 8) & 0xff) as f32;
    let ch0 = (rgb & 0xff) as f32;
    let r = ((ch16 + w * (255.0 - ch16)) + 0.5).clamp(0.0, 255.0) as u32;
    let g = ((ch8 + w * (255.0 - ch8)) + 0.5).clamp(0.0, 255.0) as u32;
    let b = ((ch0 + w * (255.0 - ch0)) + 0.5).clamp(0.0, 255.0) as u32;
    (r << 16) | (g << 8) | b
}

/// The composite of `ink` at opacity `a` over `bg`, through the renderer's
/// own source-over arithmetic.
#[inline]
const fn over_at(bg: u32, ink: u32, a: u8) -> u32 {
    aterm_render::over_premul(bg & 0x00FF_FFFF, premul_rgb(ink, a), a) & 0x00FF_FFFF
}

/// The opacity `0..=255` at which source-over `ink` on `theme_bg` leaves `theme_fg` `keep` of its contrast on the bare paper, composited as the renderer composites a quad.
#[must_use]
#[inline]
pub(crate) const fn light_alpha_keeping(ink: u32, theme_fg: u32, theme_bg: u32, keep: f32) -> f32 {
    let table = crate::color_math::relative_luminance_table();
    let (fg, bg) = (theme_fg & 0x00FF_FFFF, theme_bg & 0x00FF_FFFF);
    let lum_fg = crate::color_math::relative_luminance_with(table, fg);
    #[inline]
    const fn ratio(table: &crate::color_math::RelativeLuminanceTable, lum_fg: f32, c: u32) -> f32 {
        let l = crate::color_math::relative_luminance_with(table, c);
        (lum_fg.max(l) + 0.05) / (lum_fg.min(l) + 0.05)
    }
    let want = ratio(table, lum_fg, bg) * keep;
    if ratio(table, lum_fg, over_at(bg, ink, u8::MAX)) >= want {
        return u8::MAX as f32;
    }
    let (mut lo, mut hi) = (0u8, u8::MAX);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if ratio(table, lum_fg, over_at(bg, ink, mid)) >= want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo as f32
}

/// THE LIGHT BODY'S INK: `rgb`'s vivid ink ([`light_ink_bold`], the most chroma one ink weight allows), walked toward white only as far as the body's composite at its strongest opacity `a_max` over `theme_bg` must be lifted for `theme_fg` to keep [`BODY_CONTRAST_BAR`] over it ([`light_body_floor`]).
#[must_use]
#[inline]
pub(crate) const fn light_body_ink(rgb: u32, theme_fg: u32, theme_bg: u32, a_max: u8) -> u32 {
    light_ink_over_floor(rgb, theme_bg, a_max, light_body_floor(theme_fg))
}

/// [`light_body_ink`]'s solve against any `floor` (a WCAG relative luminance the composite at `a_max` over `theme_bg` keeps): `rgb`'s vivid ink walked toward white only as far as that floor asks, with the same light exception.
#[must_use]
#[inline]
pub(crate) const fn light_ink_over_floor(rgb: u32, theme_bg: u32, a_max: u8, floor: f32) -> u32 {
    ink_over_floor_from(light_ink_bold(rgb & 0x00FF_FFFF), theme_bg, a_max, floor)
}

/// [`light_ink_over_floor`] for a glow that must stay its hue's clean colour
/// (looks6 N7): a chromatic hue whose vivid ink is too deep for `floor` is
/// lifted first in value, its chroma kept, toward the hue's brightest ink,
/// and walked toward white only where even that brightest ink cannot keep
/// the floor. A walk toward white greys a bright hue (Sparkle's gold glow on
/// paper turned khaki); a lift in value keeps it gold. Hues too deep to clear
/// the floor at their brightest, and greys, take [`light_ink_over_floor`]'s
/// walk unchanged.
#[must_use]
#[inline]
pub(crate) const fn light_ink_vivid_over_floor(
    rgb: u32,
    theme_bg: u32,
    a_max: u8,
    floor: f32,
) -> u32 {
    use crate::color_math::rgb2hsv;
    let bold = light_ink_bold(rgb & 0x00FF_FFFF);
    let r = (bold >> 16) & 0xff;
    let g = (bold >> 8) & 0xff;
    let b = bold & 0xff;
    let peak = if r > g { r } else { g };
    let peak = if peak > b { peak } else { b };
    #[inline]
    const fn scaled(bold: u32, k: f32) -> u32 {
        #[inline]
        const fn ch(bold: u32, k: f32, sh: u32) -> u32 {
            ((((bold >> sh) & 0xff) as f32 * k + 0.5).min(255.0) as u32) << sh
        }
        ch(bold, k, 16) | ch(bold, k, 8) | ch(bold, k, 0)
    }
    let table = crate::color_math::relative_luminance_table();
    let top = if peak == 0 { 1.0 } else { 255.0 / peak as f32 };
    let top_rgb = scaled(bold, top);
    if a_max == 0
        || rgb2hsv(bold).1 < VIVID_LIFT_CHROMA_MIN
        || crate::color_math::relative_luminance_with(table, over_at(theme_bg, bold, a_max))
            >= floor
        || crate::color_math::relative_luminance_with(table, over_at(theme_bg, top_rgb, a_max))
            < floor
    {
        return light_ink_over_floor(rgb, theme_bg, a_max, floor);
    }
    let (mut lo, mut hi) = (1.0f32, top);
    let (mut lo_rgb, mut hi_rgb) = (bold, top_rgb);
    let mut i = 0;
    while i < FRESH_INK_GLYPH_BISECT {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = scaled(bold, mid);
        let mid_lum =
            crate::color_math::relative_luminance_with(table, over_at(theme_bg, mid_rgb, a_max));
        if mid_rgb == hi_rgb || (mid_rgb != lo_rgb && mid_lum >= floor) {
            hi = mid;
            hi_rgb = mid_rgb;
        } else {
            lo = mid;
            lo_rgb = mid_rgb;
        }
        i += 1;
    }
    hi_rgb
}

/// The HSV saturation below which [`light_ink_vivid_over_floor`] treats a hue as grey and keeps the walk toward white.
const VIVID_LIFT_CHROMA_MIN: f32 = 0.2;

/// [`light_ink_over_floor`]'s walk from a given starting `ink` rather than
/// the vivid ink of its hue: `ink` itself where its composite at `a_max`
/// keeps `floor`, else walked toward white only as far as the floor asks,
/// with the same light exception.
#[must_use]
#[inline]
pub(crate) const fn ink_over_floor_from(ink: u32, theme_bg: u32, a_max: u8, floor: f32) -> u32 {
    let ink = ink & 0x00FF_FFFF;
    if a_max == 0 {
        return ink;
    }
    let table = crate::color_math::relative_luminance_table();
    if crate::color_math::relative_luminance_with(table, over_at(theme_bg, ink, a_max)) >= floor {
        return ink;
    }
    let (w_bar, bar_rgb) =
        if crate::color_math::relative_luminance_with(table, over_at(theme_bg, 0x00FF_FFFF, a_max))
            >= floor
        {
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            let (mut lo_rgb, mut hi_rgb) = (ink, 0x00FF_FFFF);
            let mut i = 0;
            while i < FRESH_INK_GLYPH_BISECT {
                let mid = 0.5 * (lo + hi);
                let mid_rgb = toward_white(ink, mid);
                let matches = if mid_rgb == lo_rgb {
                    false
                } else if mid_rgb == hi_rgb {
                    true
                } else {
                    crate::color_math::relative_luminance_with(
                        table,
                        over_at(theme_bg, mid_rgb, a_max),
                    ) >= floor
                };
                if matches {
                    hi = mid;
                    hi_rgb = mid_rgb;
                } else {
                    lo = mid;
                    lo_rgb = mid_rgb;
                }
                if lo_rgb == hi_rgb {
                    break;
                }
                i += 1;
            }
            (hi, hi_rgb)
        } else {
            (1.0, 0x00FF_FFFF)
        };
    if !crate::rainbow_kitty::ribbon::seen(over_at(theme_bg, ink, a_max), theme_bg) {
        ink
    } else if crate::rainbow_kitty::ribbon::seen(over_at(theme_bg, bar_rgb, a_max), theme_bg) {
        bar_rgb
    } else {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        let (mut lo_rgb, mut hi_rgb) = (ink, 0x00FF_FFFF);
        let mut hi_known = false;
        let mut i = 0;
        while i < FRESH_INK_GLYPH_BISECT {
            let mid = 0.5 * (lo + hi);
            let mid_rgb = toward_white(ink, mid);
            let matches = if mid_rgb == lo_rgb {
                false
            } else if hi_known && mid_rgb == hi_rgb {
                true
            } else {
                !crate::rainbow_kitty::ribbon::seen(over_at(theme_bg, mid_rgb, a_max), theme_bg)
            };
            if matches {
                hi = mid;
                hi_rgb = mid_rgb;
                hi_known = true;
            } else {
                lo = mid;
                lo_rgb = mid_rgb;
            }
            if lo_rgb == hi_rgb {
                break;
            }
            i += 1;
        }
        if w_bar <= lo { bar_rgb } else { lo_rgb }
    }
}

/// The quad budget one light mark may spend past the stream's length: an ellipse is one slab per eighth of a cell across and one quad per pixel row down, about a thousand quads for the widest crown at a Retina cell.
pub(crate) const LIGHT_MARK_QUADS: usize = 4096;

/// A soft round mark of `ink` under the letters, centred on `(cx, cy)`: `left` and `right` of it (equal for a symmetric mark; Lumen's glow reaches further back over the letters just typed than ahead), `reach` above and below (held inside the row that holds `cy`, so the lines around stay clean), `peak` opaque at the centre and melting to nothing at the rim.
#[allow(
    clippy::too_many_arguments,
    reason = "stream + geometry + centre + both radii + ink + peak + budget: the ellipse IS this \
              parameter set"
)]
#[inline]
pub(crate) fn push_ink_ellipse(
    under: &mut Vec<GlowQuad>,
    geom: Geom,
    centre: (f32, f32),
    radii: (f32, f32),
    reach: f32,
    ink: u32,
    peak: f32,
    budget: usize,
) {
    push_ink_ellipse_shaped(
        under,
        geom,
        centre,
        radii,
        reach,
        ink,
        peak,
        budget,
        |_, _| 1.0,
    );
}

/// The opacity of a night ground ([`push_night_ground`]) at its centre.
pub(crate) const NIGHT_GROUND_PEAK: f32 = 255.0;
/// The brightest channel under which an additive light lays no night ground
/// ([`push_night_ground`]): too faint to tint the page.
pub(crate) const NIGHT_GROUND_FAINT: u32 = 2;
/// The ramps a night ground's slab is laid in on each side of its whole core.
const NIGHT_GROUND_RAMPS: usize = 6;
/// The most quads a frame's night grounds lay, so they never crowd the
/// letters' own marks out of the under stream.
pub(crate) const NIGHT_GROUND_QUADS: usize = 4096;
/// The gentlest close a night ground may take ([`NightGround::tau`]): a
/// steeper one lays the ground's footprint as an edge.
const NIGHT_GROUND_GENTLE: f32 = 8.0;
/// How much faster than solved a night ground closes: its quads follow its
/// curve in ramps and a halo's light in a slab's nearest row, and a quarter
/// more ground covers both.
const NIGHT_GROUND_MARGIN: f32 = 1.25;
/// The share of its light past which a halo's ground is laid whole: the
/// ground is within 2% of whole there.
const NIGHT_GROUND_WHOLE: f32 = 4.0;

/// A tinted dark page's night ground ([`push_night_ground`]): its ink and
/// how it closes under a light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct NightGround {
    /// The ground's ink: the page's own OKLab lightness, at the chroma
    /// [`night_ground`] solves for.
    pub(crate) ink: u32,
    /// The light, in its brightest channel, under which the ground closes
    /// by `1 - 1/e`.
    pub(crate) tau: f32,
}

impl NightGround {
    /// The ground's opacity under a light whose brightest channel is `s`:
    /// `1 - e^(-s/tau)`. Two lights over one pixel add and their grounds
    /// compose (`1 - (1-a)(1-b)`), and so the grounds of any lights compose
    /// to the ground of their sum: overlapping lights are grounded as one.
    #[inline]
    #[must_use]
    pub(crate) const fn alpha(self, s: f32) -> f32 {
        NIGHT_GROUND_PEAK * (1.0 - crate::color_math::exp_f32(-s.max(0.0) / self.tau.max(0.1)))
    }
}

/// The night ground of the tinted dark page `page` under the lights of
/// `family` (a style's palette), solved on the page, or none when no ground
/// keeps them off green (a green light is the user's own).
///
/// Its ink keeps the page's OKLab lightness, so the ground lies on the page
/// as part of the light's glow, never as a dark hole. Of the inks round the
/// page's lightness (OKLab chroma within 0.06 of neutral), it takes the one
/// nearest neutral, as the reference page is, whose ground may close as
/// gently as [`NIGHT_GROUND_GENTLE`] or more, with the gentlest close under
/// which no light of the family at any strength reads green over its ground
/// where it does not on the reference dark page ([`REF_DARK_PAGE`]), a hue
/// named as the eye names one (66..180° at a saturation of 0.2 and a value
/// of 0.12 or more); failing that, the ink with the gentlest close. On
/// Solarized Dark's teal a neutral ink must close by a light of a few
/// levels, where a partial ground tips a gold or amber light green, and
/// lays its footprint as a hard edge; an ink a shade cooler or rosier than
/// neutral closes over tens of levels. The close is then quickened by
/// [`NIGHT_GROUND_MARGIN`].
#[inline]
pub(crate) const fn night_ground(page: u32, family: &[u32]) -> Option<NightGround> {
    #[inline]
    const fn ch(c: u32, sh: u32) -> f32 {
        ((c >> sh) & 0xff) as f32
    }
    #[inline]
    const fn rgb(c: u32) -> [f32; 3] {
        [ch(c, 16), ch(c, 8), ch(c, 0)]
    }
    #[inline]
    const fn green(c: [f32; 3]) -> bool {
        #[inline]
        const fn px(v: f32) -> u32 {
            v.round().clamp(0.0, 255.0) as u32
        }
        let (h, s, v) = crate::color_math::rgb2hsv(px(c[0]) << 16 | px(c[1]) << 8 | px(c[2]));
        s >= 0.2 && v >= 0.12 && h >= 66.0 && h < 180.0
    }
    #[inline]
    const fn passes(p: [f32; 3], r: [f32; 3], lights: &[[f32; 3]], ng: NightGround) -> bool {
        let q = rgb(ng.ink);
        // From six closes up the ground is whole to a quarter of a percent.
        let s_max = (6.0 * ng.tau).ceil().min(255.0) as u32;
        let mut s_int = 1u32;
        while s_int <= s_max {
            let s = s_int as f32;
            s_int += 1;
            let g = ng.alpha(s) / 255.0;
            let one_minus_g = 1.0 - g;
            let base0 = p[0] * one_minus_g + q[0] * g;
            let base1 = p[1] * one_minus_g + q[1] * g;
            let base2 = p[2] * one_minus_g + q[2] * g;
            let mut li = 0;
            while li < lights.len() {
                let c = &lights[li];
                li += 1;
                if green([base0 + s * c[0], base1 + s * c[1], base2 + s * c[2]])
                    && !green([r[0] + s * c[0], r[1] + s * c[1], r[2] + s * c[2]])
                {
                    return false;
                }
            }
        }
        true
    }
    let page = page & 0x00FF_FFFF;
    let (p, r) = (rgb(page), rgb(REF_DARK_PAGE));
    let lightness = crate::color_math::oklab(page)[0];
    // Each light at a strength of one in its brightest channel.
    let mut lights_buf = [[0.0f32; 3]; 16];
    let mut lights_len = 0usize;
    let mut fi = 0;
    while fi < family.len() {
        let c = rgb(family[fi]);
        fi += 1;
        let top = c[0].max(c[1]).max(c[2]);
        if top > 0.0 && lights_len < lights_buf.len() {
            let inv_top = 1.0 / top;
            lights_buf[lights_len] = [c[0] * inv_top, c[1] * inv_top, c[2] * inv_top];
            lights_len += 1;
        }
    }
    let (lights, _) = lights_buf.split_at(lights_len);
    const TOP: f32 = 32.0;
    let floor = 0.5f32;
    // (distance from the page, ground)
    let mut best: Option<(f32, NightGround)> = None;
    let mut gentlest: Option<NightGround> = None;
    let mut ai = -4i8;
    while ai <= 6 {
        let mut bi = -4i8;
        while bi <= 4 {
            let (a, b) = (ai as f32 * 0.01, bi as f32 * 0.01);
            bi += 1;
            let ink = crate::color_math::oklab_to_rgb([lightness, a, b]) & 0x00FF_FFFF;
            let (mut lo, mut hi) = (floor, TOP);
            if !passes(p, r, lights, NightGround { ink, tau: lo }) {
                continue;
            }
            if passes(p, r, lights, NightGround { ink, tau: hi }) {
                lo = hi;
            } else {
                let mut step = 0;
                while step < 7 {
                    let mid = 0.5 * (lo + hi);
                    if passes(p, r, lights, NightGround { ink, tau: mid }) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                    step += 1;
                }
            }
            let ng = NightGround { ink, tau: lo };
            if !matches!(gentlest, Some(g) if lo <= g.tau) {
                gentlest = Some(ng);
            }
            let far = crate::color_math::hypot_f32(a, b);
            if lo >= NIGHT_GROUND_GENTLE && !matches!(best, Some((d, _)) if far >= d) {
                best = Some((far, ng));
            }
        }
        ai += 1;
    }
    let ng = match best {
        Some((_, ng)) => ng,
        None => match gentlest {
            Some(g) => g,
            None => return None,
        },
    };
    Some(NightGround {
        tau: ng.tau / NIGHT_GROUND_MARGIN,
        ..ng
    })
}

/// THE NIGHT GROUND: a soft mark of the page's own lightness without its
/// tint ([`night_ground`]) under a warm light on a tinted dark page
/// ([`tinted_dark_page`]), so the light added over it reads as it reads on
/// the reference dark page. Added to Solarized Dark's teal `#002b36`, a
/// gold or amber light's green leads the page's own until the light is
/// nearly whole: gold at a tenth of its strength and up to three quarters
/// reads sage, ember olive (Sparkle's caret bloom read sage and Fire's
/// flames and bloom green, looks7 final). No colour added to the teal
/// avoids it; laid over a near-neutral ground it never arises.
///
/// The ground follows the halo `h` it lies under, as opaque at each pixel
/// as the halo's light there asks ([`NightGround::alpha`]), so the two fade
/// out together and the ground never shows past the light as a rim. It
/// keeps to the halo's own slice. At the page's own lightness it neither
/// darkens nor lightens the letters it lies under.
/// Brightest 8-bit RGB channel of `c` (`0x00RRGGBB`).
#[inline]
pub(super) const fn max_channel(c: u32) -> u32 {
    let r = (c >> 16) & 0xff;
    let g = (c >> 8) & 0xff;
    let b = c & 0xff;
    let rg = if r > g { r } else { g };
    if rg > b { rg } else { b }
}

#[inline]
const fn night_ground_px(
    ng: NightGround,
    centre: f32,
    (cx, rx, ny): (f32, f32, f32),
    x: i32,
) -> f32 {
    let dx = (x as f32 - cx).abs();
    let u = dx / rx;
    let w = (1.0 - (ny + u * u)).max(0.0);
    ng.alpha(centre * w * w)
}

#[inline]
pub(crate) fn push_night_ground(
    under: &mut Vec<GlowQuad>,
    geom: Geom,
    h: &RainHalo,
    ng: NightGround,
    budget: usize,
) {
    let centre = max_channel(h.color) as f32;
    if centre < NIGHT_GROUND_FAINT as f32 || h.rx == 0 || h.ry == 0 || h.w == 0 || h.h == 0 {
        return;
    }
    let (cx, cy) = (f32::from(h.cx), f32::from(h.cy));
    let (rx, ry) = (f32::from(h.rx), f32::from(h.ry));
    let (left, right) = (
        i32::from(h.x).max(geom.fx_left()),
        (i32::from(h.x) + i32::from(h.w)).min(geom.fx_right()),
    );
    // The share of the reach within which the ground is laid whole.
    let n_core = 1.0 - crate::color_math::sqrt_f32((NIGHT_GROUND_WHOLE * ng.tau / centre).min(1.0));
    let slab = if ry <= 12.0 { 1 } else { 2 };
    let (top, bot) = (
        i32::from(h.y).max(geom.fx_top()),
        (i32::from(h.y) + i32::from(h.h)).min(geom.fx_bot()),
    );
    let mut y = top;
    while y < bot && under.len() + 2 * NIGHT_GROUND_RAMPS < budget {
        let y1 = (y + slab).min(bot);
        // The slab's row nearest the centre: the ground is never thinner
        // than the light over any row of it.
        let dy = if (y as f32) <= cy && cy < y1 as f32 {
            0.0
        } else {
            (y as f32 - cy).abs().min(((y1 - 1) as f32 - cy).abs())
        };
        let ny = (dy / ry) * (dy / ry);
        if ny < 1.0 {
            let rim = rx * crate::color_math::sqrt_f32(1.0 - ny);
            let core = if n_core > ny {
                rx * crate::color_math::sqrt_f32(n_core - ny)
            } else {
                0.0
            };
            // Knots on pixel edges; a pixel's light is sampled at its left
            // edge, a gradient's at its centre.
            let mid = cx + 0.5;
            let mut edges = [0i32; 2 * NIGHT_GROUND_RAMPS + 2];
            let mut k = 0;
            while k <= NIGHT_GROUND_RAMPS {
                let dx = rim - (rim - core) * k as f32 / NIGHT_GROUND_RAMPS as f32;
                let (l, r) = if k == 0 {
                    ((mid - dx).floor() as i32, (mid + dx).ceil() as i32)
                } else {
                    ((mid - dx).round() as i32, (mid + dx).round() as i32)
                };
                edges[k] = l;
                edges[2 * NIGHT_GROUND_RAMPS + 1 - k] = r;
                k += 1;
            }
            let band = (y - i32::from(geom.origin_y))
                .div_euclid(geom.ch as i32)
                .max(0) as u16;
            // The ground a pixel's light asks for (`x - cx` is where the
            // halo samples it), and a segment's edges set so its first and
            // last pixels' centres take theirs.
            let slice_geom = (cx, rx, ny);
            let mut ei = 0;
            while ei + 1 < edges.len() {
                let (p0, p1) = (edges[ei], edges[ei + 1]);
                ei += 1;
                let (q0, q1) = (p0.max(left), p1.min(right));
                if q1 <= q0 {
                    continue;
                }
                let (first, last) = (
                    night_ground_px(ng, centre, slice_geom, q0),
                    night_ground_px(ng, centre, slice_geom, q1 - 1),
                );
                if first.max(last) < 1.0 {
                    continue;
                }
                let (l, r) = if q1 - q0 == 1 {
                    (first, first)
                } else {
                    let half = (last - first) / (2 * (q1 - q0 - 1)) as f32;
                    (first - half, last + half)
                };
                let (l, r) = (
                    l.round().clamp(1.0, 255.0) as u8,
                    r.round().clamp(1.0, 255.0) as u8,
                );
                under.push(GlowQuad::gradient(
                    band,
                    q0 as u16,
                    y as u16,
                    (q1 - q0) as u16,
                    (y1 - y) as u16,
                    premul_rgb(ng.ink, l),
                    l,
                    premul_rgb(ng.ink, r),
                    r,
                ));
            }
        }
        y = y1;
    }
}

/// A round coma of `ink` around `(cx, cy)`, `r` px across its radius and held in its row: a disc at `fill` opacity inside a rim `rim_w` px wide at `rim` opacity, one scanline at a time, its outer edge anti-aliased.
#[allow(
    clippy::too_many_arguments,
    reason = "the ring's parameter set: centre, radius, rim, ink, two opacities, budget"
)]
#[inline]
pub(crate) fn push_ink_ring(
    under: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    r: f32,
    rim_w: f32,
    ink: u32,
    (fill, rim): (f32, f32),
    budget: usize,
) {
    if geom.ch == 0 || r <= 0.5 {
        return;
    }
    let chf = geom.ch as f32;
    let oy = f32::from(geom.origin_y);
    let row_top = oy + ((cy - oy) * (1.0 / chf)).floor() * chf;
    let y0 = (cy - r).max(row_top).floor() as i32;
    let y1 = (cy + r).min(row_top + chf).ceil() as i32;
    #[inline]
    fn put_ring_px(
        under: &mut Vec<GlowQuad>,
        geom: Geom,
        (x, y, w): (i32, i32, i32),
        ink: u32,
        alpha: f32,
        share: f32,
        budget: usize,
    ) {
        let al = (alpha * share).round().clamp(0.0, 255.0) as u8;
        if al > 0 && under.len() < budget {
            let pm = premul_rgb(ink, al);
            crate::effect_util::push_fx_ramp(under, geom, x, y, w, 1, (pm, al), (pm, al));
        }
    }
    #[inline]
    fn put_ring_span(
        under: &mut Vec<GlowQuad>,
        geom: Geom,
        y: i32,
        (a, b): (f32, f32),
        ink: u32,
        alpha: f32,
        budget: usize,
    ) {
        if b <= a || alpha < 1.0 {
            return;
        }
        let (ia, ib) = (a.ceil() as i32, b.floor() as i32);
        if ib < ia {
            put_ring_px(
                under,
                geom,
                (a.floor() as i32, y, 1),
                ink,
                alpha,
                b - a,
                budget,
            );
            return;
        }
        if ia as f32 > a {
            put_ring_px(
                under,
                geom,
                (ia - 1, y, 1),
                ink,
                alpha,
                ia as f32 - a,
                budget,
            );
        }
        if ib > ia {
            put_ring_px(under, geom, (ia, y, ib - ia), ink, alpha, 1.0, budget);
        }
        if b > ib as f32 {
            put_ring_px(under, geom, (ib, y, 1), ink, alpha, b - ib as f32, budget);
        }
    }
    let ri = (r - rim_w).max(0.0);
    let (r2, ri2) = (r * r, ri * ri);
    let mut y = y0;
    while y < y1 {
        let dy = (y as f32 + 0.5 - cy).abs();
        if dy < r {
            let dy2 = dy * dy;
            let outer = crate::color_math::sqrt_f32(r2 - dy2);
            let inner = if dy < ri {
                crate::color_math::sqrt_f32(ri2 - dy2)
            } else {
                0.0
            };
            put_ring_span(under, geom, y, (cx - outer, cx - inner), ink, rim, budget);
            put_ring_span(under, geom, y, (cx - inner, cx + inner), ink, fill, budget);
            put_ring_span(under, geom, y, (cx + inner, cx + outer), ink, rim, budget);
        }
        y += 1;
    }
}

/// [`push_ink_ellipse`] with its opacity scaled along x: `share(x0, x1)` is the share `0..=1` of the mark kept at a sample whose neighbourhood spans window x `x0..x1` (one sample's spacing either side of it), so a sample next to anything held back is held back too and the slabs between two samples never ramp past the lower one's share inside it.
#[allow(
    clippy::too_many_arguments,
    reason = "the ellipse's parameter set and its share along x"
)]
#[inline]
pub(crate) fn push_ink_ellipse_shaped(
    under: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    radii: (f32, f32),
    reach: f32,
    ink: u32,
    peak: f32,
    budget: usize,
    share: impl Fn(f32, f32) -> f32,
) {
    if geom.ch == 0 {
        return;
    }
    let chf = geom.ch as f32;
    let oy = f32::from(geom.origin_y);
    let row_top = oy + ((cy - oy) * (1.0 / chf)).floor() * chf;
    let available = (cy - row_top).min(row_top + chf - cy).max(0.0);
    soft_ellipse(
        under,
        geom,
        (cx, cy),
        radii,
        reach.min(available),
        ink,
        peak,
        budget,
        share,
        aterm_render::GlowBlend::Over,
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ellipse's parameter set, its share along x and its blend"
)]
#[inline]
fn soft_ellipse(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    (left, right): (f32, f32),
    reach: f32,
    ink: u32,
    peak: f32,
    budget: usize,
    share: impl Fn(f32, f32) -> f32,
    blend: aterm_render::GlowBlend,
) {
    use aterm_render::{RibbonVertex, ribbon_beam};
    if peak < 1.0 || left.max(right) < 0.5 || geom.ch == 0 || geom.cw == 0 || reach < 0.5 {
        return;
    }
    const STEPS: usize = 24;
    const U_ROUND: [(f32, f32); STEPS + 1] = {
        let mut table = [(0.0f32, 0.0f32); STEPS + 1];
        let mut i = 0;
        while i <= STEPS {
            let u = i as f32 / STEPS as f32 * 2.0 - 1.0;
            let round = crate::color_math::sqrt_f32((1.0 - u * u).max(0.0));
            table[i] = (u, round);
            i += 1;
        }
        table
    };
    let u_round = &U_ROUND;
    let mut verts = [RibbonVertex {
        x: 0.0,
        spine: cy,
        up: 0.0,
        dn: 0.0,
        core_up: 0.0,
        core_dn: 0.0,
        color: ink,
        cov: 0.0,
        lift: 0.0,
        lift_span: 0.0,
    }; STEPS + 1];
    let mut xs = [0.0f32; STEPS + 1];
    let mut i = 0;
    while i <= STEPS {
        let (u, _) = u_round[i];
        xs[i] = cx + u * if u < 0.0 { left } else { right };
        i += 1;
    }
    let mut i = 0;
    while i <= STEPS {
        let (_, round) = u_round[i];
        let r = (reach * round).max(0.5);
        let v = &mut verts[i];
        v.x = xs[i];
        v.up = r;
        v.dn = r;
        v.core_up = r * 0.3;
        v.core_dn = r * 0.3;
        let kept = share(xs[i.saturating_sub(1)], xs[(i + 1).min(STEPS)]).clamp(0.0, 1.0);
        v.cov = peak * round * round * kept;
        i += 1;
    }
    let stride = (geom.cw / 8).max(1);
    ribbon_beam(out, geom.beam_clip(), &verts, 1.0, stride, budget, blend);
}

/// Re-ink quads rasterized in WHITE (so each channel is the coverage that lit it) as source-over ink at that coverage, capped at `cap`: the light-ground form of any additive beam, its own anti-aliasing intact.
#[inline]
pub(crate) fn reink_quads(quads: &mut [GlowQuad], cap: u8, ink_at: impl Fn(&GlowQuad) -> u32) {
    let mut i = 0;
    while i < quads.len() {
        let q = &mut quads[i];
        let a = ((q.color & 0xff) as u8).min(cap);
        let a2 = ((q.color2 & 0xff) as u8).min(cap);
        if a == 0 && a2 == 0 {
            q.color = 0;
            q.color2 = 0;
            q.alpha = 0;
            q.alpha2 = 0;
            i += 1;
            continue;
        }
        let ink = ink_at(q);
        let (a, a2) = (a.max(1), a2.max(1));
        let c1 = premul_rgb(ink, a);
        q.color = c1;
        q.color2 = if a == a2 { c1 } else { premul_rgb(ink, a2) };
        q.alpha = a;
        q.alpha2 = a2;
        i += 1;
    }
}

/// The light ground one frame draws on: the theme's text and paper, which every under-letter body solves its ink against.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LightGround {
    pub(crate) fg: u32,
    pub(crate) bg: u32,
    /// Test-only: answer the bare vivid ink ([`light_ink_bold`]), never solved against the text (`CursorGlow::light_ground`'s negative control).
    #[cfg(test)]
    pub(crate) unsolved: bool,
}

impl LightGround {
    /// The ground of a resolved config.
    #[inline]
    #[must_use]
    pub(crate) const fn of(cfg: &GlowConfig) -> Self {
        Self {
            fg: cfg.theme_fg,
            bg: cfg.theme_bg,
            #[cfg(test)]
            unsolved: false,
        }
    }

    /// [`light_body_ink`] on this ground at the body ceiling ([`LIGHT_BODY_ALPHA_CAP`], opaque): the bodies under the letters stack (a crown over a tail over the typing glow), and a stack of one ink composites toward the ink itself, so the ink alone keeps the bar.
    #[inline]
    #[must_use]
    pub(crate) const fn body_ink(self, rgb: u32) -> u32 {
        #[cfg(test)]
        if self.unsolved {
            return light_ink_bold(rgb & 0x00FF_FFFF);
        }
        light_body_ink(rgb, self.fg, self.bg, LIGHT_BODY_ALPHA_CAP as u8)
    }

    /// [`Self::body_ink`] for a body that keeps its own colour: `rgb` as
    /// itself rather than its hue's one-weight vivid ink, walked toward
    /// white only as far as the theme's text over it at the body ceiling
    /// asks ([`ink_over_floor_from`]).
    #[inline]
    #[must_use]
    pub(crate) const fn vivid_body_ink(self, rgb: u32) -> u32 {
        ink_over_floor_from(
            rgb,
            self.bg,
            LIGHT_BODY_ALPHA_CAP as u8,
            light_body_floor(self.fg),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: u32, b: u32) -> f32 {
        let (x, y) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Every hue's body ink, at its strongest, leaves the theme's text at the
    /// bar over it on every light theme that can meet the bar (a sweep of
    /// foregrounds from black to mid grey on white and cream), and on one
    /// that cannot (Solarized Light) it is still a colour the eye picks out.
    #[test]
    fn the_light_body_keeps_the_theme_text_at_the_bar_over_it() {
        let a = LIGHT_BODY_ALPHA_CAP as u8;
        let hues = [
            0x00FF_0000,
            0x00FF_7F00,
            0x00FF_FF00,
            0x0000_FF00,
            0x0000_00FF,
            0x004B_0082,
            0x0094_00D3,
            0x0009_69DA,
            0x009E_D6FF,
        ];
        for bg in [0x00FF_FFFF, 0x00FD_F6E3] {
            for grey in (0..=0x77u32).step_by(0x11) {
                let fg = (grey << 16) | (grey << 8) | grey;
                for rgb in hues {
                    let ink = light_body_ink(rgb, fg, bg, a);
                    let blend = over_at(bg, ink, a);
                    assert!(
                        crate::rainbow_kitty::ribbon::seen(blend, bg),
                        "{rgb:06x} still reads as a colour on {bg:06x}"
                    );
                    if grey <= 0x33 {
                        assert!(
                            contrast(fg, blend) >= BODY_CONTRAST_BAR,
                            "{rgb:06x} under {fg:06x} on {bg:06x}: {:.2}:1",
                            contrast(fg, blend)
                        );
                    }
                }
            }
        }
        let (fg, bg) = (0x0065_7B83, 0x00FD_F6E3);
        for rgb in hues {
            let blend = over_at(bg, light_body_ink(rgb, fg, bg, a), a);
            assert!(crate::rainbow_kitty::ribbon::seen(blend, bg), "{rgb:06x}");
        }
        let old = over_at(
            0x00FF_FFFF,
            light_ink(0x0000_00FF),
            LIGHT_INK_ALPHA_CAP as u8,
        );
        assert!(contrast(0x001F_2328, old) < 3.0, "the old blue body");
    }
}

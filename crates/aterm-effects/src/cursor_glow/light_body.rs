// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIGHT-GROUND LAW: every emitter has a light mode of its own. Additive
//! light cannot show on white paper, so on a light ground each style draws
//! its own shape as ink: its body source-over UNDER the letters (the
//! `glow_under` stream, so glyphs keep full contrast on top) and held inside
//! the row it marks, its small accents in conservative ink over the text.
//!
//! The ink under the letters is solved against the theme's own text and
//! background ([`light_body_ink`]): a body may darken the paper only as far
//! as leaves the theme's text legible over it at [`BODY_CONTRAST_BAR`], the
//! bar the rainbow family's dark bed is solved against, read here as a
//! minimum brightness instead of a maximum. A strength (alpha) cap alone is
//! no legibility rule: saturated blue under near-black text at the old fixed
//! cap measured 1.97:1.
//!
//! One primitive per shape, shared by every emitter so two bodies cannot
//! drift apart: [`push_ink_ellipse`] (a round glow or coma held in its row)
//! and [`reink_quads`] (any beam rasterized in white, re-inked at its own
//! anti-aliased coverage).

use super::*;
use crate::color_math::relative_luminance;

/// The contrast bar every legibility ceiling in the cursor family is solved
/// against: the theme's text over the light a body lays under it, 5.25:1
/// (the rainbow ribbon's L3). On a dark ground it caps how bright the bed may
/// composite; on a light one it floors how dark the ink may.
pub(crate) const BODY_CONTRAST_BAR: f32 = 5.25;

/// The margin past [`BODY_CONTRAST_BAR`] for the emitter's own rounding (one
/// level of composite rounding and one Bayer level of dither).
pub(crate) const BODY_CONTRAST_GUARD: f32 = 0.05;

/// The darkest a light-ground body may composite, in WCAG relative
/// luminance: the theme's text keeps [`BODY_CONTRAST_BAR`] (plus the guard)
/// over it. A function of the foreground alone.
#[must_use]
pub(crate) fn light_body_floor(theme_fg: u32) -> f32 {
    light_floor_at(theme_fg, BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD)
}

/// The darkest a light-ground mark may composite, in WCAG relative
/// luminance, for `theme_fg` to keep `bar` over it.
#[must_use]
pub(crate) fn light_floor_at(theme_fg: u32, bar: f32) -> f32 {
    bar * (relative_luminance(theme_fg) + 0.05) - 0.05
}

/// A walk toward white by `w`: `c + w · (255 − c)`. Channel differences
/// survive unscaled, so the hue direction is kept and only saturation falls.
fn toward_white(rgb: u32, w: f32) -> u32 {
    let ch = |sh: u32| {
        let c = ((rgb >> sh) & 0xff) as f32;
        ((c + w * (255.0 - c)) + 0.5).clamp(0.0, 255.0) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// The composite of `ink` at opacity `a` over `bg`, through the renderer's
/// own source-over arithmetic.
#[inline]
fn over_at(bg: u32, ink: u32, a: u8) -> u32 {
    aterm_render::over_premul(bg & 0x00FF_FFFF, premul_rgb(ink, a), a) & 0x00FF_FFFF
}

/// The opacity `0..=255` at which source-over `ink` on `theme_bg` leaves
/// `theme_fg` `keep` of its contrast on the bare paper, composited as the
/// renderer composites a quad. A light ground's ink darkens the paper with
/// its opacity, so the answer is bisected; an ink that keeps it even opaque
/// answers 255.
#[must_use]
pub(crate) fn light_alpha_keeping(ink: u32, theme_fg: u32, theme_bg: u32, keep: f32) -> f32 {
    use crate::color_math::{relative_luminance_table, relative_luminance_with};
    let table = relative_luminance_table();
    let (fg, bg) = (theme_fg & 0x00FF_FFFF, theme_bg & 0x00FF_FFFF);
    let lum_fg = relative_luminance_with(table, fg);
    let ratio = |c: u32| {
        let l = relative_luminance_with(table, c);
        (lum_fg.max(l) + 0.05) / (lum_fg.min(l) + 0.05)
    };
    let want = ratio(bg) * keep;
    let holds = |a: u8| ratio(over_at(bg, ink, a)) >= want;
    if holds(u8::MAX) {
        return f32::from(u8::MAX);
    }
    let (mut lo, mut hi) = (0u8, u8::MAX);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if holds(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    f32::from(lo)
}

/// THE LIGHT BODY'S INK: `rgb`'s vivid ink ([`light_ink_bold`], the most
/// chroma one ink weight allows), walked toward white only as far as the
/// body's composite at its strongest opacity `a_max` over `theme_bg` must be
/// lifted for `theme_fg` to keep [`BODY_CONTRAST_BAR`] over it
/// ([`light_body_floor`]). A weaker coverage only composites lighter on a
/// light ground, so the solve at `a_max` holds across the whole fade.
///
/// The light exception, mirroring the dark bed's `BED_LUMA_MIN`: where the
/// bar would lift the body past the point it still reads as a colour against
/// the ground (`ribbon::seen`) — a theme whose own text-to-ground contrast is
/// under the bar, as Solarized Light's 4.1:1 is, or one with mid-grey text —
/// the body takes the palest ink that still reads, the most its text can be
/// given while the body shows.
#[must_use]
pub(crate) fn light_body_ink(rgb: u32, theme_fg: u32, theme_bg: u32, a_max: u8) -> u32 {
    light_ink_over_floor(rgb, theme_bg, a_max, light_body_floor(theme_fg))
}

/// [`light_body_ink`]'s solve against any `floor` (a WCAG relative
/// luminance the composite at `a_max` over `theme_bg` keeps): `rgb`'s vivid
/// ink walked toward white only as far as that floor asks, with the same
/// light exception. [`light_body_ink`] is this at [`light_body_floor`].
#[must_use]
pub(crate) fn light_ink_over_floor(rgb: u32, theme_bg: u32, a_max: u8, floor: f32) -> u32 {
    let ink = light_ink_bold(rgb & 0x00FF_FFFF);
    if a_max == 0 {
        return ink;
    }
    let table = crate::color_math::relative_luminance_table();
    let lum_at = |w: f32| {
        crate::color_math::relative_luminance_with(
            table,
            over_at(theme_bg, toward_white(ink, w), a_max),
        )
    };
    if lum_at(0.0) >= floor {
        return ink;
    }
    let bisect = |pass: &dyn Fn(f32) -> bool, want_low: bool| {
        // `pass` is monotone in `w`: false then true (`want_low`: the least
        // passing `w`) or true then false (the greatest passing `w`).
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..FRESH_INK_GLYPH_BISECT {
            let mid = 0.5 * (lo + hi);
            if pass(mid) == want_low {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        if want_low { hi } else { lo }
    };
    let w_bar = if lum_at(1.0) >= floor {
        bisect(&|w| lum_at(w) >= floor, true)
    } else {
        1.0
    };
    let seen_at = |w: f32| {
        crate::rainbow_kitty::ribbon::seen(over_at(theme_bg, toward_white(ink, w), a_max), theme_bg)
    };
    let w_seen = if !seen_at(0.0) {
        0.0
    } else if seen_at(w_bar) {
        w_bar
    } else {
        bisect(&seen_at, false)
    };
    toward_white(ink, w_bar.min(w_seen))
}

/// The quad budget one light mark may spend past the stream's length: an
/// ellipse is one slab per eighth of a cell across and one quad per pixel row
/// down, about a thousand quads for the widest crown at a Retina cell.
pub(crate) const LIGHT_MARK_QUADS: usize = 4096;

/// A soft round mark of `ink` under the letters, centred on `(cx, cy)`:
/// `left` and `right` of it (equal for a symmetric mark; Lumen's glow
/// reaches further back over the letters just typed than ahead), `reach`
/// above and below (held inside the row that holds `cy`, so the lines around
/// stay clean), `peak` opaque at the centre and melting to nothing at the
/// rim. Eighth-cell slabs, so the falloff never shows as steps. The one round
/// body every light glow shares (Lumen's crown, the comet's coma and
/// nucleus, the typing glow, the comet's debris), so two of them cannot
/// drift apart.
#[allow(
    clippy::too_many_arguments,
    reason = "stream + geometry + centre + both radii + ink + peak + budget: the ellipse IS this \
              parameter set"
)]
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

/// A round coma of `ink` around `(cx, cy)`, `r` px across its radius and held
/// in its row: a disc at `fill` opacity inside a rim `rim_w` px wide at `rim`
/// opacity, one scanline at a time, its outer edge anti-aliased. Source-over
/// under the letters like every light body ([`push_ink_ellipse`]).
#[allow(
    clippy::too_many_arguments,
    reason = "the ring's parameter set: centre, radius, rim, ink, two opacities, budget"
)]
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
    let row_top = oy + ((cy - oy) / chf).floor() * chf;
    let y0 = (cy - r).max(row_top).floor() as i32;
    let y1 = (cy + r).min(row_top + chf).ceil() as i32;
    // One horizontal span `[a, b)` at opacity `alpha`: its whole pixels as
    // one quad, each part-covered end pixel at its covered share.
    let span = |under: &mut Vec<GlowQuad>, y: i32, a: f32, b: f32, alpha: f32| {
        if b <= a || alpha < 1.0 {
            return;
        }
        let mut put = |x: i32, w: i32, share: f32| {
            let al = (alpha * share).round().clamp(0.0, 255.0) as u8;
            if al > 0 && under.len() < budget {
                let pm = premul_rgb(ink, al);
                crate::effect_util::push_fx_ramp(under, geom, x, y, w, 1, (pm, al), (pm, al));
            }
        };
        let (ia, ib) = (a.ceil() as i32, b.floor() as i32);
        if ib < ia {
            put(a.floor() as i32, 1, b - a);
            return;
        }
        if ia as f32 > a {
            put(ia - 1, 1, ia as f32 - a);
        }
        if ib > ia {
            put(ia, ib - ia, 1.0);
        }
        if b > ib as f32 {
            put(ib, 1, b - ib as f32);
        }
    };
    let ri = (r - rim_w).max(0.0);
    let (r2, ri2) = (r * r, ri * ri);
    for y in y0..y1 {
        let dy = (y as f32 + 0.5 - cy).abs();
        if dy >= r {
            continue;
        }
        let dy2 = dy * dy;
        let outer = (r2 - dy2).sqrt();
        let inner = if dy < ri { (ri2 - dy2).sqrt() } else { 0.0 };
        span(under, y, cx - outer, cx - inner, rim);
        span(under, y, cx - inner, cx + inner, fill);
        span(under, y, cx + inner, cx + outer, rim);
    }
}

/// [`push_ink_ellipse`] with its opacity scaled along x: `share(x0, x1)` is
/// the share `0..=1` of the mark kept at a sample whose neighbourhood spans
/// window x `x0..x1` (one sample's spacing either side of it), so a sample
/// next to anything held back is held back too and the slabs between two
/// samples never ramp past the lower one's share inside it. The typing glow
/// keeps its ink off the letters this way
/// (`the_light_glow_keeps_the_newest_letter_within_half_a_point`).
#[allow(
    clippy::too_many_arguments,
    reason = "the ellipse's parameter set and its share along x"
)]
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
    // Inside the row that holds the centre.
    let row_top = f32::from(geom.origin_y) + ((cy - f32::from(geom.origin_y)) / chf).floor() * chf;
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
    let us_xs: [(f32, f32); STEPS + 1] = std::array::from_fn(|i| {
        let u = i as f32 / STEPS as f32 * 2.0 - 1.0;
        (u, cx + u * if u < 0.0 { left } else { right })
    });
    for (i, v) in verts.iter_mut().enumerate() {
        let (u, x) = us_xs[i];
        // The ellipse's height at this x, and a falloff toward its rim.
        let round = (1.0 - u * u).max(0.0).sqrt();
        let r = (reach * round).max(0.5);
        v.x = x;
        v.up = r;
        v.dn = r;
        v.core_up = r * 0.3;
        v.core_dn = r * 0.3;
        let kept = share(us_xs[i.saturating_sub(1)].1, us_xs[(i + 1).min(STEPS)].1).clamp(0.0, 1.0);
        v.cov = peak * round * round * kept;
    }
    let stride = (geom.cw / 8).max(1);
    ribbon_beam(out, geom.beam_clip(), &verts, 1.0, stride, budget, blend);
}

/// Re-ink quads rasterized in WHITE (so each channel is the coverage that lit
/// it) as source-over ink at that coverage, capped at `cap`: the light-ground
/// form of any additive beam, its own anti-aliasing intact. `ink_at` picks
/// each quad's ink (a run's colour along its path, or one ink for all).
pub(crate) fn reink_quads(quads: &mut [GlowQuad], cap: u8, ink_at: impl Fn(&GlowQuad) -> u32) {
    for q in quads {
        let a = ((q.color & 0xff) as u8).min(cap);
        let a2 = ((q.color2 & 0xff) as u8).min(cap);
        if a == 0 && a2 == 0 {
            q.color = 0;
            q.color2 = 0;
            q.alpha = 0;
            q.alpha2 = 0;
            continue;
        }
        let ink = ink_at(q);
        // A quad is additive at both ends or source-over at both ends.
        let (a, a2) = (a.max(1), a2.max(1));
        let c1 = premul_rgb(ink, a);
        q.color = c1;
        q.color2 = if a == a2 { c1 } else { premul_rgb(ink, a2) };
        q.alpha = a;
        q.alpha2 = a2;
    }
}

/// The light ground one frame draws on: the theme's text and paper, which
/// every under-letter body solves its ink against.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LightGround {
    pub(crate) fg: u32,
    pub(crate) bg: u32,
    /// Test-only: answer the bare vivid ink ([`light_ink_bold`]), never
    /// solved against the text (`CursorGlow::light_ground`'s negative
    /// control).
    #[cfg(test)]
    pub(crate) unsolved: bool,
}

impl LightGround {
    /// The ground of a resolved config.
    #[must_use]
    pub(crate) fn of(cfg: &GlowConfig) -> Self {
        Self {
            fg: cfg.theme_fg,
            bg: cfg.theme_bg,
            #[cfg(test)]
            unsolved: false,
        }
    }

    /// [`light_body_ink`] on this ground at the body ceiling
    /// ([`LIGHT_BODY_ALPHA_CAP`], opaque): the bodies under the letters
    /// stack (a crown over a tail over the typing glow), and a stack of one
    /// ink composites toward the ink itself, so the ink alone keeps the bar.
    #[must_use]
    pub(crate) fn body_ink(self, rgb: u32) -> u32 {
        #[cfg(test)]
        if self.unsolved {
            return light_ink_bold(rgb & 0x00FF_FFFF);
        }
        light_body_ink(rgb, self.fg, self.bg, LIGHT_BODY_ALPHA_CAP as u8)
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
    /// Negative control: the old fixed recipe (bold ink at the body cap) puts
    /// near-black text under 3:1 over pure blue.
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
                    // Dark text (the light themes aterm ships): the bar holds.
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
        // Solarized Light: the bar is out of reach, the body still shows.
        let (fg, bg) = (0x0065_7B83, 0x00FD_F6E3);
        for rgb in hues {
            let blend = over_at(bg, light_body_ink(rgb, fg, bg, a), a);
            assert!(crate::rainbow_kitty::ribbon::seen(blend, bg), "{rgb:06x}");
        }
        // Negative control: the old tall ribbon's recipe (conservative ink at
        // the over-text ceiling, never solved against the text) puts
        // near-black text under 3:1 over its blue.
        let old = over_at(
            0x00FF_FFFF,
            light_ink(0x0000_00FF),
            LIGHT_INK_ALPHA_CAP as u8,
        );
        assert!(contrast(0x001F_2328, old) < 3.0, "the old blue body");
    }
}

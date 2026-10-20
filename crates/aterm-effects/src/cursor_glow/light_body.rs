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
    (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD) * (relative_luminance(theme_fg) + 0.05) - 0.05
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
    let ink = light_ink_bold(rgb & 0x00FF_FFFF);
    if a_max == 0 {
        return ink;
    }
    let floor = light_body_floor(theme_fg);
    let lum_at = |w: f32| relative_luminance(over_at(theme_bg, toward_white(ink, w), a_max));
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
/// `half_w` either side, `reach` above and below (held inside the row that
/// holds `cy`, so the lines around stay clean), `peak` opaque at the centre
/// and melting to nothing at the rim. Eighth-cell slabs, so the falloff
/// never shows as steps. The one round body every light glow shares (Lumen's
/// crown, every style's crown, the comet's coma, the rod's aura, the typing
/// glow), so two of them cannot drift apart.
#[allow(
    clippy::too_many_arguments,
    reason = "stream + geometry + centre + both radii + ink + peak + budget: the ellipse IS this \
              parameter set"
)]
pub(crate) fn push_ink_ellipse(
    under: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    half_w: f32,
    reach: f32,
    ink: u32,
    peak: f32,
    budget: usize,
) {
    use aterm_render::{GlowBlend, RibbonVertex, ribbon_beam};
    if peak < 1.0 || half_w < 0.5 || geom.ch == 0 || geom.cw == 0 {
        return;
    }
    let chf = geom.ch as f32;
    // Inside the row that holds the centre.
    let row_top = f32::from(geom.origin_y) + ((cy - f32::from(geom.origin_y)) / chf).floor() * chf;
    let available = (cy - row_top).min(row_top + chf - cy).max(0.0);
    let reach = reach.min(available);
    if reach < 0.5 {
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
    for (i, v) in verts.iter_mut().enumerate() {
        let u = i as f32 / STEPS as f32 * 2.0 - 1.0;
        // The ellipse's height at this x, and a falloff toward its rim.
        let round = (1.0 - u * u).max(0.0).sqrt();
        let r = (reach * round).max(0.5);
        v.x = cx + u * half_w;
        v.up = r;
        v.dn = r;
        v.core_up = r * 0.3;
        v.core_dn = r * 0.3;
        v.cov = peak * round * round;
    }
    let stride = (geom.cw / 8).max(1);
    ribbon_beam(
        under,
        geom.beam_clip(),
        &verts,
        1.0,
        stride,
        budget,
        GlowBlend::Over,
    );
}

/// Re-ink quads rasterized in WHITE (so each channel is the coverage that lit
/// it) as source-over ink at that coverage, capped at `cap`: the light-ground
/// form of any additive beam, its own anti-aliasing intact. `ink_at` picks
/// each quad's ink (a run's colour along its path, or one ink for all).
pub(crate) fn reink_quads(quads: &mut [GlowQuad], cap: u8, ink_at: impl Fn(&GlowQuad) -> u32) {
    for q in quads {
        let ink = ink_at(q);
        let a = ((q.color & 0xff) as u8).min(cap);
        let a2 = ((q.color2 & 0xff) as u8).min(cap);
        // A quad is additive at both ends or source-over at both ends.
        let (a, a2) = if a == 0 && a2 == 0 {
            (0, 0)
        } else {
            (a.max(1), a2.max(1))
        };
        q.color = premul_rgb(ink, a);
        q.color2 = premul_rgb(ink, a2);
        q.alpha = a;
        q.alpha2 = a2;
    }
}

/// THE INK-FIRE'S LIGHT-GROUND CEILING: the strongest coverage the flame
/// field may lay on paper. The flames draw under the glyph ink, so the only
/// legibility limit there is the theme's text over the flame: the largest
/// alpha (up to [`LIGHT_BODY_ALPHA_CAP`]) at which the field's darkest ink
/// ([`aterm_render::fire_field::FIRE_OVER_RIM_INK`]) composited over the
/// ground keeps [`light_body_floor`] for `theme_fg`, the bar every body under
/// the letters keeps. Where the theme cannot meet the bar even on bare paper,
/// the light exception: the least coverage at which the rim still reads as a
/// colour, so the flame shows.
#[must_use]
pub(crate) fn light_fire_cov_ceiling(theme_fg: u32, theme_bg: u32) -> u8 {
    let rim = aterm_render::fire_field::FIRE_OVER_RIM_INK;
    let floor = light_body_floor(theme_fg);
    let cap = LIGHT_BODY_ALPHA_CAP as u8;
    let keeps = |a: u8| relative_luminance(over_at(theme_bg, rim, a)) >= floor;
    if keeps(cap) {
        return cap;
    }
    if keeps(0) {
        // The composite darkens monotonically with coverage.
        let (mut lo, mut hi) = (0u8, cap);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if keeps(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        return lo;
    }
    (1..=cap)
        .find(|&a| crate::rainbow_kitty::ribbon::seen(over_at(theme_bg, rim, a), theme_bg))
        .unwrap_or(cap)
}

/// The light ground one frame draws on: the theme's text and paper, which
/// every under-letter body solves its ink against.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LightGround {
    pub(crate) fg: u32,
    pub(crate) bg: u32,
}

impl LightGround {
    /// The ground of a resolved config.
    #[must_use]
    pub(crate) fn of(cfg: &GlowConfig) -> Self {
        Self {
            fg: cfg.theme_fg,
            bg: cfg.theme_bg,
        }
    }

    /// [`light_body_ink`] on this ground at the body ceiling
    /// ([`LIGHT_BODY_ALPHA_CAP`], opaque): the bodies under the letters
    /// stack (a crown over a tail over the typing glow), and a stack of one
    /// ink composites toward the ink itself, so the ink alone keeps the bar.
    #[must_use]
    pub(crate) fn body_ink(self, rgb: u32) -> u32 {
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

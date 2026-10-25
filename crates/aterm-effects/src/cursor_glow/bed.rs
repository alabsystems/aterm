// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dark-ground cursor-glow bed and leading rail laid beneath glyph ink.

use super::*;
use aterm_render::{GlowBlend, RibbonVertex, ribbon_beam};

/// The contrast the theme's ink keeps over the bed alone.
pub(super) const DARK_BED_BAR: f32 = 4.8;

/// The bed's opacity over a fresh cell.
pub(super) const DARK_BED_ALPHA_CAP: f32 = 236.0;

/// Luminance budget clamp bounds for the dark bed.
const DARK_BED_LUMA_MIN: f32 = 0.020;
const DARK_BED_LUMA_MAX: f32 = 0.150;

/// The bed's luminance budget for the theme's ink `ink`.
#[inline]
#[must_use]
pub(crate) const fn dark_bed_budget(ink: u32) -> f32 {
    let bar =
        (crate::color_math::relative_luminance(ink & 0x00FF_FFFF) + 0.05) / DARK_BED_BAR - 0.05;
    bar.clamp(DARK_BED_LUMA_MIN, DARK_BED_LUMA_MAX)
}

/// How far a bed on a page too lit for any light under its letters lies
/// from the page toward its stop's own colour, in linear light at the
/// page's luminance ([`page_lit_ink`]). Whole, laid in sRGB over the page
/// at a soft edge's half opacity, Solarized Dark's red stop reads at 58% of
/// the page's luminance (a dark seam round a vivid core); at this share the
/// edge keeps seven tenths of it and the core still wears its hue.
pub(crate) const PAGE_LIT_CHROMA: f32 = 0.7;

/// The bed's ink on a page too lit for its letters to take any light
/// (Solarized Dark: its text reads 4.75:1 on `#002b36`, and a dim letter
/// keeps 97% of its own reading under the lights): the stop `rgb`'s
/// colour at the page's own luminance, [`PAGE_LIT_CHROMA`] of the way
/// from the page in linear light, never over the page's luminance. The
/// bed lays neither light nor shade there, only its hue. Held under the
/// page's luminance instead, the dark bed solved to black, and every
/// style's bed, tube and tracer drew black smoke on the page (looks7
/// round 2; the ribbons' maroon-black blocks, Beam's black tube, Phaser's
/// black word gaps).
#[must_use]
pub(crate) const fn page_lit_ink(rgb: u32, page: u32) -> u32 {
    #[inline]
    const fn at(c: u32, sh: u32) -> f32 {
        crate::color_math::relative_luminance_table().linear[((c >> sh) & 0xff) as usize]
    }
    #[inline]
    const fn lum(c: [f32; 3]) -> f32 {
        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    }
    #[inline]
    const fn mix_ch(pi: f32, qi: f32, k: f32) -> f32 {
        let qk = qi * k;
        let qk = if qk < 1.0 { qk } else { 1.0 };
        let m = pi + (qk - pi) * PAGE_LIT_CHROMA;
        if m > 0.0 { m } else { 0.0 }
    }
    #[inline]
    const fn byte(c: f32) -> f32 {
        let n = if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * crate::color_math::powf_pos_f32(c, 1.0 / 2.4) - 0.055
        };
        (n * 255.0).clamp(0.0, 255.0)
    }
    #[inline]
    const fn ceil_u8(b: f32) -> u32 {
        let u = b.ceil() as u32;
        if u < 255 { u } else { 255 }
    }
    let p = [at(page, 16), at(page, 8), at(page, 0)];
    let q = [at(rgb, 16), at(rgb, 8), at(rgb, 0)];
    let (lp, lq) = (lum(p), lum(q));
    if lp <= 0.0 || lq <= 0.0 {
        return page & 0x00FF_FFFF;
    }
    let k = lp / lq;
    let b0 = byte(mix_ch(p[0], q[0], k));
    let b1 = byte(mix_ch(p[1], q[1], k));
    let b2 = byte(mix_ch(p[2], q[2], k));
    let lo = [b0.floor() as u32, b1.floor() as u32, b2.floor() as u32];
    let hi = [ceil_u8(b0), ceil_u8(b1), ceil_u8(b2)];
    // Of the bytes round the mix, the brightest that stays at or under the
    // page's luminance: rounded at a dim page's few bytes a channel swings
    // the luminance by several percent either way.
    let page_l = crate::color_math::relative_luminance(page & 0x00FF_FFFF);
    let mut best = (lo[0] << 16) | (lo[1] << 8) | lo[2];
    let mut best_l = crate::color_math::relative_luminance(best);
    let mut bit = 1u32;
    while bit < 8 {
        let c0 = if (bit & 1) != 0 { hi[0] } else { lo[0] };
        let c1 = if (bit & 2) != 0 { hi[1] } else { lo[1] };
        let c2 = if (bit & 4) != 0 { hi[2] } else { lo[2] };
        let c = (c0 << 16) | (c1 << 8) | c2;
        let l = crate::color_math::relative_luminance(c);
        if l <= page_l && l > best_l {
            best = c;
            best_l = l;
        }
        bit += 1;
    }
    best
}

/// A lit page's bed in the air ([`page_lit_ink`]): where no letter lies
/// (a blank cell: the leading past a line's end, a blank row a Return's
/// tube crosses), no letter's reading bounds the bed, and it wears the
/// light it wears on the reference dark theme ([`CursorGlow::STREAK_REF_INK`]),
/// laid as on that theme (held to that budget, each channel's larger of
/// it and the page counted). The letter guard cuts whatever of it reaches
/// a letter. Laid at the page's luminance in the air too, Beam's ice tube
/// and Lumen's grey tracer did not show on Solarized Dark at all.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AirLight {
    budget: f32,
}

impl AirLight {
    /// The air's light on `page` for a bed solved to `budget`; `None` where
    /// the bed is not lit at the page's luminance, or where the page's own
    /// letters already allow it the reference theme's budget (on the
    /// reference ground itself a dim shape's bed is lit by its own budget,
    /// and the air adds nothing: Sparkle's gold rail stays gold and keeps
    /// to the leading).
    #[must_use]
    pub(crate) const fn of(page: u32, budget: f32, shape: BedShape) -> Option<Self> {
        let lit = page_lit(page, budget);
        let air = dim_budget(dark_bed_budget(CursorGlow::STREAK_REF_INK), page, shape.dim);
        if lit && air > budget {
            Some(Self { budget: air })
        } else {
            None
        }
    }

    /// The bed's ink for the stop `rgb` in the air over `page`.
    #[inline]
    #[must_use]
    pub(crate) const fn ink(self, rgb: u32, page: u32, sat: f32) -> u32 {
        dark_bed_ink_unlit(rgb, page, self.budget, sat)
    }
}

/// The bed's ink for the stop `rgb` over the page `bg` under `budget`; on a
/// page whose luminance the solved ink falls under, [`page_lit_ink`].
#[must_use]
pub(crate) const fn dark_bed_ink(rgb: u32, bg: u32, budget: f32, sat: f32) -> u32 {
    let solved = dark_bed_ink_unlit(rgb, bg, budget, sat);
    if crate::color_math::relative_luminance(solved)
        < crate::color_math::relative_luminance(bg & 0x00FF_FFFF)
    {
        let rgb = crate::spectrum::spectrum_with_min_saturation(rgb & 0x00FF_FFFF, sat);
        page_lit_ink(rgb, bg)
    } else {
        solved
    }
}

/// [`dark_bed_ink`] held under the budget alone, never lit at the page's
/// luminance (a test's negative control: on a lit page it lays a shade).
#[must_use]
pub(crate) const fn dark_bed_ink_unlit(rgb: u32, bg: u32, budget: f32, sat: f32) -> u32 {
    let rgb = crate::spectrum::spectrum_with_min_saturation(rgb & 0x00FF_FFFF, sat);
    #[inline]
    const fn ch(c: u32, sh: u32) -> u32 {
        (c >> sh) & 0xff
    }
    let r = ch(rgb, 16);
    let g = ch(rgb, 8);
    let b = ch(rgb, 0);
    let rg = if r > g { r } else { g };
    let mx = if rg > b { rg } else { b };
    let full = if mx == 0 {
        [0.0; 3]
    } else {
        let k = 255.0 / mx as f32;
        #[inline]
        const fn scale_ch(c: u32, k: f32) -> f32 {
            let v = (c as f32 * k + 0.5) as u32;
            (if v < 255 { v } else { 255 }) as f32
        }
        [scale_ch(r, k), scale_ch(g, k), scale_ch(b, k)]
    };
    let bg_rgb = [ch(bg, 16) as usize, ch(bg, 8) as usize, ch(bg, 0) as usize];
    #[inline]
    const fn pick_rgb(full: [f32; 3], t: f32) -> [usize; 3] {
        #[inline]
        const fn pick_c(c: f32, t: f32) -> usize {
            let v = if t <= 0.5 {
                c * 2.0 * t
            } else {
                c + (255.0 - c) * (2.0 * t - 1.0)
            };
            let u = v as usize;
            if u < 255 { u } else { 255 }
        }
        [pick_c(full[0], t), pick_c(full[1], t), pick_c(full[2], t)]
    }
    #[inline]
    const fn seen(bg_rgb: [usize; 3], ink: [usize; 3]) -> f32 {
        let r = if ink[0] > bg_rgb[0] {
            ink[0]
        } else {
            bg_rgb[0]
        };
        let g = if ink[1] > bg_rgb[1] {
            ink[1]
        } else {
            bg_rgb[1]
        };
        let b = if ink[2] > bg_rgb[2] {
            ink[2]
        } else {
            bg_rgb[2]
        };
        crate::color_math::relative_luminance_rgb(
            crate::color_math::relative_luminance_table(),
            r,
            g,
            b,
        )
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (pick_rgb(full, 0.0), pick_rgb(full, 1.0));
    let (mut lo_ok, mut hi_ok) = (false, false);
    let mut step = 0;
    while step < 20 {
        step += 1;
        if lo_rgb[0] == hi_rgb[0] && lo_rgb[1] == hi_rgb[1] && lo_rgb[2] == hi_rgb[2] {
            break;
        }
        let mid = 0.5 * (lo + hi);
        let mid_rgb = pick_rgb(full, mid);
        if lo_ok && mid_rgb[0] == lo_rgb[0] && mid_rgb[1] == lo_rgb[1] && mid_rgb[2] == lo_rgb[2] {
            lo = mid;
        } else if hi_ok
            && mid_rgb[0] == hi_rgb[0]
            && mid_rgb[1] == hi_rgb[1]
            && mid_rgb[2] == hi_rgb[2]
        {
            hi = mid;
        } else if seen(bg_rgb, mid_rgb) <= budget {
            lo = mid;
            lo_rgb = mid_rgb;
            lo_ok = true;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
            hi_ok = true;
        }
    }
    let [r, g, b] = lo_rgb;
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// A style's bed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BedShape {
    /// The bed's half-height at a fresh cell, as a share of the cell height.
    pub(crate) reach: f32,
    /// The share of that reach a fading cell gives up as it fades.
    pub(crate) thin: f32,
    /// The least HSV chroma the bed's ink carries ([`dark_bed_ink`]).
    pub(crate) sat: f32,
    /// The coverage of a fresh cell of the style, which lays the bed at [`DARK_BED_ALPHA_CAP`].
    pub(crate) full: f32,
    /// A wedge's length in cells (`0` for none).
    pub(crate) wedge: f32,
    /// The vivid rail's reach below the row, as a share of the cell height (`0` for none).
    pub(crate) rail: f32,
    /// How far the rail's soft top rises above the row's bottom, as a share of its reach.
    pub(crate) rail_rise: f32,
    /// The hue family the bed's ink takes ([`BedHue`]).
    pub(crate) hue: BedHue,
    /// The share of the reach the bed's full-opacity core holds.
    pub(crate) core: f32,
    /// The share of the bar's luminance budget the bed is solved to ([`dark_bed_ink`]).
    pub(crate) dim: f32,
    /// A jump's piece of a run is a wedge as long as the path it laid ([`Self::wedge`]).
    pub(crate) leap_path: bool,
    /// A leap's tube runs its whole path ([`LEAP_GAP_CELLS`], [`CursorGlow::split_air_runs`]).
    pub(crate) leap_air: bool,
    /// The rail a style that lays none on the reference dark lays on a lit
    /// page ([`Self::on_lit_page`]), as a share of the cell height (`0` for none).
    pub(crate) lit_rail: f32,
    /// The HSV saturation under which the bed keeps its colour's own chroma
    /// instead of [`Self::sat`] (`0` for none): a near-grey colour's hue is
    /// noise, and floored to `sat` it laid a hue the colour does not have
    /// (Lumen on Solarized Dark's grey cursor `#93a1a1` drew a saturated
    /// cyan tracer `#94edfb`, looks7 round 3).
    pub(crate) own_chroma: f32,
    /// The bed's colour is one the user chose ([`crate::cursor_glow::ColourPins`],
    /// [`Self::pinned`]): every ink the bed lays keeps the colour's own chroma.
    pub(crate) pinned: bool,
}

/// Whether `page` is too lit for a bed solved to `budget` to lay any light
/// under its letters (Solarized Dark: [`page_lit_ink`]). Asked of the
/// letters' own budget ([`dark_bed_budget`]), it is the page's: a shape
/// dimmed under it ([`BedShape::dim`], Sparkle's gold rail's) solves to
/// black on any page.
#[must_use]
pub(crate) const fn page_lit(page: u32, budget: f32) -> bool {
    let probe = dark_bed_ink_unlit(0x00FF_0000, page, budget, 1.0);
    crate::color_math::relative_luminance(probe)
        < crate::color_math::relative_luminance(page & 0x00FF_FFFF)
}

/// How far below its row's bottom a bed's rail lies on a lit page
/// ([`BedShape::on_lit_page`]), px: the ribbons' hairline
/// ([`crate::rainbow_kitty::ribbon::rail_gap_px`]), and never inside the
/// judge's ring round a descender's foot ([`crate::cursor_glow::LETTER_GUARD_RING`]).
#[inline]
#[must_use]
pub(crate) const fn lit_rail_drop(ch: f32) -> f32 {
    crate::rainbow_kitty::ribbon::rail_gap_px(ch).max(crate::cursor_glow::LETTER_GUARD_RING as f32)
}

/// The vivid rail's ink for the colour `rgb` of a bed of `shape`
/// ([`crate::rainbow_kitty::ribbon::rail_ink`]); a near-grey colour of a
/// shape that keeps its own chroma ([`BedShape::own_chroma`]) keeps it,
/// brought to the rail's brightness.
/// A chosen colour ([`BedShape::pinned`]) keeps its chroma the same way.
#[must_use]
pub(crate) const fn bed_rail_ink(rgb: u32, shape: BedShape) -> u32 {
    #[inline]
    const fn max_u32(a: u32, b: u32) -> u32 {
        if a > b { a } else { b }
    }
    #[inline]
    const fn up(rgb: u32, mx: u32, sh: u32) -> u32 {
        let v = (((rgb >> sh) & 0xff) * 255) / mx;
        if v < 255 { v } else { 255 }
    }
    let rgb = rgb & 0x00FF_FFFF;
    let (_, s, _) = crate::color_math::rgb2hsv(rgb);
    if shape.pinned || s < shape.own_chroma {
        let mx = max_u32(
            max_u32((rgb >> 16) & 0xff, (rgb >> 8) & 0xff),
            max_u32(rgb & 0xff, 1),
        );
        (up(rgb, mx, 16) << 16) | (up(rgb, mx, 8) << 8) | up(rgb, mx, 0)
    } else {
        crate::rainbow_kitty::ribbon::rail_ink(rgb)
    }
}

/// The hue family of a style's bed ([`BedShape::hue`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BedHue {
    /// The stop's own hue.
    True,
    /// The night arc ([`night_hue`]).
    Night,
}

impl BedShape {
    /// The shape every style's bed starts from.
    pub(crate) const BASE: Self = Self {
        reach: 0.30,
        thin: 0.30,
        sat: 0.90,
        full: 215.0,
        wedge: 0.0,
        rail: 0.0,
        rail_rise: 0.0,
        hue: BedHue::True,
        core: 0.5,
        dim: 1.0,
        leap_path: false,
        leap_air: false,
        lit_rail: 0.0,
        own_chroma: 0.0,
        pinned: false,
    };

    /// The shape for a colour the user chose ([`crate::cursor_glow::ColourPins`]):
    /// a chosen colour is drawn as itself, so its bed, the air's light and
    /// the rail keep its own chroma, on every page; only their light is
    /// spent to the budget. Floored to [`Self::sat`] and the rail's
    /// saturation, Beam pinned to `#b0b0c8` (0.12) laid its Solarized Dark
    /// rail in `#6464ff` (0.61) and Lumen pinned to `#d09090` (0.31) in
    /// `#ff0000` (looks7 round 3's review).
    #[inline]
    #[must_use]
    pub(crate) const fn pinned(self) -> Self {
        Self {
            pinned: true,
            ..self
        }
    }

    /// The shape on a lit page ([`page_lit`]): its rail a hairline below
    /// the judge's ring ([`lit_rail_drop`]) with no soft top rising into the
    /// row, where no cut of the row's letters reaches it; a style whose bed
    /// lays no rail on the reference dark lays [`Self::lit_rail`] there. On
    /// Solarized Dark a letter keeps 97% of its own reading and its bed lays
    /// only its hue at the page's luminance: Beam, Lumen and Comet drew
    /// nothing behind the caret while typing, each a bare glowing caret
    /// (looks7 round 3).
    #[inline]
    #[must_use]
    pub(crate) const fn on_lit_page(self) -> Self {
        Self {
            rail: if self.rail > 0.0 {
                self.rail
            } else {
                self.lit_rail
            },
            rail_rise: 0.0,
            ..self
        }
    }

    /// The saturation floor the bed's ink takes for the colour `rgb`
    /// ([`Self::own_chroma`]).
    #[inline]
    #[must_use]
    pub(crate) const fn sat_for(self, rgb: u32) -> f32 {
        let (_, s, _) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
        if self.pinned || s < self.own_chroma {
            s
        } else {
            self.sat
        }
    }

    /// The saturation floor the bed's ink under the letters takes for the
    /// colour `rgb`: a chosen colour's own ([`Self::pinned`]), else
    /// [`Self::sat`] (a near-grey's bed under a lit page's letters lies at
    /// the page's luminance, where a grey only greys the page, so its
    /// floored hue is kept).
    #[inline]
    #[must_use]
    pub(crate) const fn under_sat_for(self, rgb: u32) -> f32 {
        if self.pinned {
            crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF).1
        } else {
            self.sat
        }
    }

    /// The shape of `style`'s bed, or `None` for a style that lays none.
    #[inline]
    pub(super) const fn of(style: GlowStyle, beam_pw: f32) -> Option<Self> {
        match style {
            GlowStyle::Lumen => Some(Self {
                reach: 0.26,
                thin: 0.45,
                sat: 0.90,
                full: 215.0,
                leap_air: true,
                lit_rail: LUMEN_LIT_RAIL,
                own_chroma: LUMEN_OWN_CHROMA,
                ..Self::BASE
            }),
            GlowStyle::Comet => Some(Self {
                reach: 0.40,
                thin: 0.75,
                sat: 0.85,
                full: 215.0,
                wedge: 3.5,
                rail: 0.16,
                leap_path: true,
                leap_air: true,
                ..Self::BASE
            }),
            GlowStyle::Laser => Some(Self {
                reach: 0.22,
                thin: 0.30,
                sat: 0.85,
                full: 255.0,
                leap_air: true,
                ..Self::BASE
            }),
            GlowStyle::Phaser => Some(Self {
                reach: 0.40,
                thin: 0.15,
                sat: 1.0,
                full: 126.0,
                rail: 0.18,
                hue: BedHue::Night,
                ..Self::BASE
            }),
            GlowStyle::Sparkle => Some(Self {
                reach: 0.40,
                thin: 0.30,
                sat: 0.0,
                full: 215.0,
                wedge: 4.0,
                rail: SPARKLE_RAIL,
                rail_rise: SPARKLE_RAIL_RISE,
                core: 0.12,
                dim: 0.0,
                ..Self::BASE
            }),
            GlowStyle::Beam => Some(Self {
                reach: 0.14 + 0.26 * beam_pw,
                thin: 0.0,
                sat: 0.85,
                full: 235.0,
                leap_air: true,
                lit_rail: BEAM_LIT_RAIL,
                ..Self::BASE
            }),
            _ => None,
        }
    }

    /// The bed's opacity and half-height ([`DARK_BED_DY`]).
    #[inline]
    const fn at(self, cov: u8, ch: f32, k: f32) -> (f32, f32) {
        let share = ((cov as f32) / self.full).clamp(0.0, 1.0) * k.clamp(0.0, 1.0);
        let reach = ch * self.reach * (1.0 - self.thin * (1.0 - share));
        let room = (ch * (0.5 - DARK_BED_DY)).max(0.0);
        (
            DARK_BED_ALPHA_CAP * share,
            reach.clamp(0.5f32.min(room), room),
        )
    }
}

/// The bed's spine below the cell's middle, as a share of the cell height.
const DARK_BED_DY: f32 = 0.04;

/// Beam's rail on a lit page ([`BedShape::lit_rail`]): its tube's light in
/// the air below its letters, as deep as the ribbons' vivid rail may reach.
const BEAM_LIT_RAIL: f32 = 0.22;
/// Lumen's rail on a lit page ([`BedShape::lit_rail`]): a hairline of its
/// tracer's light.
const LUMEN_LIT_RAIL: f32 = 0.10;
/// The saturation under which Lumen's bed keeps its colour's own chroma
/// ([`BedShape::own_chroma`]): a grey cursor's (Solarized Dark's `#93a1a1`
/// is 0.09) keeps its grey; the dark gallery's green `#50fa7b` (0.68) is
/// floored as before.
const LUMEN_OWN_CHROMA: f32 = 0.2;

/// Sparkle's gold rail's reach below the row, as a share of the cell height ([`BedShape::rail`]).
const SPARKLE_RAIL: f32 = 0.24;
/// Sparkle's rail's soft top ([`BedShape::rail_rise`]).
const SPARKLE_RAIL_RISE: f32 = 0.5;

/// How far a typed run's bed takes to rise from nothing at its tail, in cells.
const DARK_BED_TAIL_CELLS: f32 = 1.0;

/// Head feather length in cells when the caret has moved past the run.
pub(crate) const DARK_BED_HEAD_FEATHER_CELLS: f32 = 0.9;

/// The knots a head feather is laid in ([`DARK_BED_HEAD_FEATHER_CELLS`]).
const DARK_BED_HEAD_FEATHER_KNOTS: usize = 6;

/// The rail's full-strength plateau as a share of its reach.
const DARK_RAIL_CORE: f32 = 0.5;

/// Number of nested passes used to lay a leap's tube.
const LEAP_LAYERS: usize = 6;
/// Maximum air gap in cells bridged within a leap run.
const LEAP_GAP_CELLS: usize = 3;
/// How much wider than the bed's own girth a leap's outermost pass reaches.
const LEAP_SPREAD: f32 = 1.4;

/// The night arc's hue, degrees, for a hue `h`.
#[inline]
#[must_use]
pub(crate) const fn night_hue(h: f32) -> f32 {
    let r = (h - 90.0) % 360.0;
    let u = if r < 0.0 { r + 360.0 } else { r };
    if u <= 180.0 {
        195.0 + 75.0 * u / 180.0
    } else if u <= 300.0 {
        270.0 + 60.0 * (u - 180.0) / 120.0
    } else {
        330.0 - 135.0 * (u - 300.0) / 60.0
    }
}

/// `rgb` on the night arc ([`night_hue`]), its saturation and value kept.
#[inline]
#[must_use]
pub(crate) const fn night_rgb(rgb: u32) -> u32 {
    let (h, s, v) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
    crate::color_math::hsv2rgb(night_hue(h), s, v)
}

/// `budget` ([`dark_bed_budget`]) dimmed to `dim` of its rise over `page`.
#[inline]
#[must_use]
pub(crate) const fn dim_budget(budget: f32, page: u32, dim: f32) -> f32 {
    let floor = crate::color_math::relative_luminance(page & 0x00FF_FFFF);
    floor + (budget - floor).max(0.0) * dim.clamp(0.0, 1.0)
}

/// The dark bed's reusable working buffers.
pub(crate) struct BedScratch {
    /// The bed's ink per sample ([`lay_dark_bed`]).
    pub(crate) inks: Vec<u32>,
    /// The rail's ink per sample, in run order (empty for a shape with no rail).
    pub(crate) rails: Vec<u32>,
    verts: Vec<RibbonVertex>,
    tube: Vec<BeamVertex>,
    /// Each sample's taper (the wedge's and the tail's), in run order.
    taper: Vec<f32>,
    /// Each run's wedge length in cells ([`wedge_runs`]).
    pub(crate) wedges: Vec<f32>,
    /// Caret cell `(row, col)` when laying the bed around a live caret.
    pub(crate) caret: Option<(i32, i32)>,
    /// How far under the row's bottom the rail's spine lies, px: on a lit
    /// page ([`AirLight`]) the ribbon's hairline ([`crate::rainbow_kitty::ribbon::rail_gap_px`]),
    /// so a descender's foot keeps its ring of page clear of the rail's light
    /// (laid at the row's bottom, Phaser's spectrum underline was cut to
    /// nothing under Solarized Dark's letters).
    pub(crate) rail_drop: f32,
    /// Each sample of the run being inked: whether it lies in the air
    /// ([`air_lit`]).
    pub(crate) air: Vec<bool>,
}

/// Fill `out` with whether each sample of `run` wears the air's light
/// ([`AirLight`]): on a lit page (`lit`), a sample whose cell and the cells
/// either side of it hold no text, nor its neighbours' along the run
/// (unless `alone`, a test's control: its own cell's text alone). A sample
/// beside text keeps the page's luminance, so the bed's colour, drawn
/// between samples, turns from the hue to the light in the air and never
/// over a letter: read by its own cell, the sample past a line's last
/// letter lit the bed's ramp under that letter.
pub(crate) fn air_lit(
    run: &[CometSample],
    geom: Geom,
    text: &TextView<'_>,
    (lit, alone): (bool, bool),
    out: &mut Vec<bool>,
) {
    out.clear();
    if !lit {
        out.resize(run.len(), false);
        return;
    }
    let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let (inv_cw, inv_ch) = (1.0 / (geom.cw as f32), 1.0 / (geom.ch as f32));
    let reach = i32::from(!alone);
    out.reserve(run.len());
    let mut prev_cell: Option<((i32, i32), bool)> = None;
    for s in run {
        let cell = (
            ((s.y - oyf) * inv_ch).floor() as i32,
            ((s.x - oxf) * inv_cw).floor() as i32,
        );
        let ok = match prev_cell {
            Some((prev, ans)) if prev == cell => ans,
            _ => {
                let (row, col) = cell;
                let mut clear = !text.over_text(row, col);
                if clear && reach != 0 {
                    clear = !text.over_text(row, col - 1) && !text.over_text(row, col + 1);
                }
                prev_cell = Some((cell, clear));
                clear
            }
        };
        out.push(ok);
    }
    if alone {
        return;
    }
    let mut before = true;
    for i in 0..out.len() {
        let own = out[i];
        let after = out.get(i + 1).copied().unwrap_or(true);
        out[i] = own && before && after;
        before = own;
    }
}

impl BedScratch {
    pub(crate) const EMPTY: Self = Self {
        inks: Vec::new(),
        rails: Vec::new(),
        verts: Vec::new(),
        tube: Vec::new(),
        taper: Vec::new(),
        wedges: Vec::new(),
        caret: None,
        rail_drop: 0.0,
        air: Vec::new(),
    };
}

impl Default for BedScratch {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Lay the dark bed under `runs` into `under` in `scratch.inks` ([`dark_bed_ink`]).
#[allow(
    clippy::too_many_arguments,
    reason = "the runs, the grid, the pair's page, the shape, the frame's text and the three buffers it fills"
)]
pub(crate) fn lay_dark_bed(
    runs: &[Vec<CometSample>],
    geom: Geom,
    page: u32,
    shape: BedShape,
    text: &TextView<'_>,
    scratch: &mut BedScratch,
    ledger: &mut InkLedger,
    under: &mut Vec<GlowQuad>,
) {
    let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
    let (inv_cw, inv_ch) = (1.0 / cwf, 1.0 / chf);
    let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let clip = geom.beam_clip();
    let stride = (geom.cw / 8).max(1);
    let quad_budget = under.len().saturating_add(CursorGlow::MAX_QUADS);
    let dy = chf * DARK_BED_DY;
    let BedScratch {
        inks,
        rails,
        verts,
        tube,
        taper,
        wedges,
        caret,
        rail_drop,
        air: _,
    } = scratch;
    let (caret, rail_drop) = (*caret, *rail_drop);
    let cell_of = |x: f32, y: f32| {
        (
            ((y - oyf) * inv_ch).floor() as i32,
            ((x - oxf) * inv_cw).floor() as i32,
        )
    };
    const fn smooth(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
    let mut at = 0usize;
    for (index, run) in runs.iter().enumerate() {
        if run.is_empty() {
            continue;
        }
        let wedge_cells = wedges.get(index).copied().unwrap_or(shape.wedge);
        let ink = &inks[at..at + run.len()];
        let rail = rails.get(at..at + run.len());
        at += run.len();
        let one_row = run.iter().all(|s| s.y == run[0].y);
        let dir = if run.len() > 1 && run[run.len() - 1].x < run[0].x {
            -1.0
        } else {
            1.0
        };
        let (first, last) = (run[0], run[run.len() - 1]);
        let (tail_x, head_x) = (first.x - dir * cwf * 0.5, last.x + dir * cwf * 0.5);
        let inv_tail_w = 1.0 / (cwf * DARK_BED_TAIL_CELLS);
        let inv_wedge_w = if wedge_cells > 0.0 {
            1.0 / (cwf * wedge_cells)
        } else {
            0.0
        };
        let taper_at = |x: f32| {
            if !one_row {
                return 1.0;
            }
            let tail = smooth((x - tail_x).abs() * inv_tail_w);
            let wedge = if inv_wedge_w > 0.0 {
                let behind = ((last.x - x) * dir).max(0.0);
                (1.0 - behind * inv_wedge_w).clamp(0.0, 1.0)
            } else {
                1.0
            };
            tail * wedge
        };
        taper.clear();
        taper.extend(run.iter().map(|s| taper_at(s.x)));
        if ledger.is_on() {
            let mut prev: Option<((i32, i32), u32)> = None;
            for ((s, &rgb), &k) in run.iter().zip(ink).zip(taper.iter()) {
                let a = shape.at(s.cov, chf, k).0 as u8;
                let composite = aterm_render::over_premul(page, premul_rgb(rgb, a), a);
                let here = cell_of(s.x, s.y);
                ledger.lay_under(here, composite);
                if let Some((cell, before)) = prev
                    && cell != here
                {
                    ledger.lay_under(here, before);
                    ledger.lay_under(cell, composite);
                }
                prev = Some((here, composite));
            }
        }
        if one_row {
            let vertex = |x: f32, s: &CometSample, rgb: u32, k: f32| {
                let (alpha, reach) = shape.at(s.cov, chf, k);
                RibbonVertex {
                    x,
                    spine: s.y + dy,
                    up: reach,
                    dn: reach,
                    core_up: reach * shape.core,
                    core_dn: reach * shape.core,
                    color: rgb,
                    cov: alpha,
                    lift: 0.0,
                    lift_span: 0.0,
                }
            };
            verts.clear();
            verts.push(vertex(tail_x, &first, ink[0], 0.0));
            for ((s, &rgb), &k) in run.iter().zip(ink).zip(taper.iter()) {
                verts.push(vertex(s.x, s, rgb, k));
            }
            let (head_row, head_col) = cell_of(last.x, last.y);
            let air_head = run.len() > 1 && !text.over_text(head_row, head_col);
            if air_head && let Some(v) = verts.last_mut() {
                let (alpha, _) = shape.at(last.cov, chf, taper[taper.len() - 1] * 0.5);
                v.cov = alpha;
            }
            let head_k = if air_head { 0.0 } else { taper_at(head_x) };
            verts.push(vertex(head_x, &last, ink[ink.len() - 1], head_k));
            let left = caret.is_some_and(|c| c != (head_row, head_col)) && head_k > 0.0;
            let feather_span = dir * cwf * DARK_BED_HEAD_FEATHER_CELLS;
            let from = head_x - feather_span;
            let taper_from = taper_at(from);
            const HEAD_FEATHER_KNOTS: [(f32, f32); DARK_BED_HEAD_FEATHER_KNOTS + 1] = {
                let mut out = [(0.0f32, 0.0f32); DARK_BED_HEAD_FEATHER_KNOTS + 1];
                let mut j = 0;
                while j <= DARK_BED_HEAD_FEATHER_KNOTS {
                    let u = j as f32 / DARK_BED_HEAD_FEATHER_KNOTS as f32;
                    out[j] = (u, 1.0 - smooth(u));
                    j += 1;
                }
                out
            };
            let feather_over =
                |verts: &mut Vec<RibbonVertex>, at: &dyn Fn(f32, f32) -> RibbonVertex| {
                    while verts.len() > 1 && verts.last().is_some_and(|v| (v.x - from) * dir >= 0.0)
                    {
                        verts.pop();
                    }
                    for &(u, w) in &HEAD_FEATHER_KNOTS {
                        verts.push(at(from + feather_span * u, taper_from * w));
                    }
                };
            if left {
                feather_over(verts, &|x, k| vertex(x, &last, ink[ink.len() - 1], k));
            }
            if shape.dim > 0.0 {
                ribbon_beam(
                    under,
                    clip,
                    verts,
                    1.0,
                    stride,
                    quad_budget,
                    GlowBlend::Over,
                );
            }
            let row = cell_of(first.x, first.y).0;
            if let Some(rail) = rail
                && shape.rail > 0.0
                && row + 1 < geom.rows as i32
            {
                let bottom = oyf + (row + 1) as f32 * chf + rail_drop;
                let reach = shape.rail * chf;
                let rail_up = reach * shape.rail_rise;
                let rail_core_dn = reach * DARK_RAIL_CORE * (1.0 - shape.rail_rise);
                let leading =
                    |x: f32| !text.over_text(row + 1, ((x - oxf) * inv_cw).floor() as i32);
                let rail_vertex = |x: f32, s: &CometSample, rgb: u32, k: f32| RibbonVertex {
                    x,
                    spine: bottom,
                    up: rail_up,
                    dn: reach,
                    core_up: 0.0,
                    core_dn: rail_core_dn,
                    color: rgb,
                    cov: if leading(x) {
                        shape.at(s.cov, chf, k).0
                    } else {
                        0.0
                    },
                    lift: 0.0,
                    lift_span: 0.0,
                };
                verts.clear();
                verts.push(rail_vertex(tail_x, &first, rail[0], 0.0));
                for ((s, &rgb), &k) in run.iter().zip(rail).zip(taper.iter()) {
                    verts.push(rail_vertex(s.x, s, rgb, k));
                }
                verts.push(rail_vertex(
                    head_x,
                    &last,
                    rail[rail.len() - 1],
                    taper_at(head_x),
                ));
                if left {
                    feather_over(verts, &|x, k| {
                        rail_vertex(x, &last, rail[rail.len() - 1], k)
                    });
                }
                let from = under.len();
                ribbon_beam(
                    under,
                    clip,
                    verts,
                    1.0,
                    stride,
                    quad_budget,
                    GlowBlend::Over,
                );
                if rail_up <= 0.0 && rail_drop >= crate::cursor_glow::LETTER_GUARD_RING as f32 {
                    ledger.note_rail(from, under.len());
                }
            }
        } else {
            let over = |s: &CometSample| {
                let (row, col) = cell_of(s.x, s.y);
                shape.leap_air || text.over_text(row, col)
            };
            let mut i = 0;
            while i < run.len() {
                if !over(&run[i]) {
                    i += 1;
                    continue;
                }
                let start = i;
                loop {
                    while i < run.len() && over(&run[i]) {
                        i += 1;
                    }
                    let mut j = i;
                    while j < run.len() && !over(&run[j]) && j - i < LEAP_GAP_CELLS {
                        j += 1;
                    }
                    if j > i && j < run.len() && over(&run[j]) {
                        i = j;
                        continue;
                    }
                    break;
                }
                let stretch = &run[start..i];
                let mut reach_sum = 0.0f32;
                for s in stretch {
                    reach_sum += shape.at(s.cov, chf, 1.0).1;
                }
                let reach = reach_sum / stretch.len() as f32;
                let vertex = |s: &CometSample, rgb: u32, x: f32, y: f32| BeamVertex {
                    x,
                    y: y + dy,
                    color: rgb,
                    cov: shape.at(s.cov, chf, 1.0).0 as u8,
                };
                tube.clear();
                if start > 0 {
                    let (a, b) = (run[start - 1], run[start]);
                    tube.push(vertex(&b, ink[start], 0.5 * (a.x + b.x), 0.5 * (a.y + b.y)));
                }
                for (s, &rgb) in stretch.iter().zip(&ink[start..i]) {
                    tube.push(vertex(s, rgb, s.x, s.y));
                }
                if i < run.len() {
                    let (a, b) = (run[i - 1], run[i]);
                    tube.push(vertex(&a, ink[i - 1], 0.5 * (a.x + b.x), 0.5 * (a.y + b.y)));
                }
                if tube.len() >= 2 && shape.dim > 0.0 {
                    let (a, b) = (tube[0], tube[tube.len() - 1]);
                    let len = crate::color_math::hypot_f32(b.x - a.x, b.y - a.y);
                    let f = (cwf * 0.5 / len.max(1.0)).min(0.3);
                    let at = |k: f32, cov: u8| BeamVertex {
                        x: a.x + (b.x - a.x) * k,
                        y: a.y + (b.y - a.y) * k,
                        color: if k < 0.5 { a.color } else { b.color },
                        cov,
                    };
                    const LAYER_COV: [u8; 256] = {
                        let mut lut = [0u8; 256];
                        let mut a_idx = 0usize;
                        while a_idx < 256 {
                            let a = a_idx as f32 / 255.0;
                            let rem = (1.0 - a) as f64;
                            let root = if rem <= 0.0 {
                                0.0
                            } else {
                                let mut r = 1.0f64;
                                let mut step = 0;
                                while step < 20 {
                                    let r2 = r * r;
                                    let r5 = r2 * r2 * r;
                                    r = (5.0 * r + rem / r5) / (LEAP_LAYERS as f64);
                                    step += 1;
                                }
                                r as f32
                            };
                            let layer = 1.0 - root;
                            lut[a_idx] = (layer * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                            a_idx += 1;
                        }
                        lut
                    };
                    let (ca, cb) = (LAYER_COV[usize::from(a.cov)], LAYER_COV[usize::from(b.cov)]);
                    tube.clear();
                    tube.extend([at(0.0, 0), at(f, ca), at(1.0 - f, cb), at(1.0, 0)]);
                    for k in 0..LEAP_LAYERS {
                        let shrink = 1.0 - k as f32 * (1.0 / LEAP_LAYERS as f32);
                        comet_beam_over(
                            under,
                            clip,
                            tube,
                            2.0 * reach * LEAP_SPREAD * shrink,
                            1,
                            0.0,
                        );
                    }
                }
            }
        }
    }
    under.truncate(quad_budget);
}

/// A wedge's runs ([`BedShape::wedge`]) into the pool `out`, returning how
/// many are live.
fn wedge_runs(
    runs: &[Vec<CometSample>],
    typing: &[Vec<bool>],
    leaps: &[(Instant, u16, u16, u16)],
    geom: Geom,
    shape: BedShape,
    out: &mut Vec<Vec<CometSample>>,
    lengths: &mut Vec<f32>,
) -> usize {
    lengths.clear();
    let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
    let (inv_cw, inv_ch) = (1.0 / cwf, 1.0 / chf);
    let wedge = shape.wedge;
    let knots = (wedge * 4.0).ceil().max(1.0) as usize;
    let mut n = 0usize;
    let mut next = |out: &mut Vec<Vec<CometSample>>| {
        if n == out.len() {
            out.push(Vec::new());
        }
        n += 1;
        let dst = &mut out[n - 1];
        dst.clear();
        n - 1
    };
    for (index, run) in runs.iter().enumerate() {
        let Some(&head) = run.last() else {
            continue;
        };
        if !run.iter().all(|s| s.y == head.y) {
            let at = next(out);
            out[at].extend_from_slice(run);
            lengths.push(wedge);
            continue;
        }
        let kinds = typing.get(index).map_or(&[][..], Vec::as_slice);
        let typed = |i: usize| !shape.leap_path || kinds.get(i).copied().unwrap_or(true);
        let mut start = 0;
        while start < run.len() {
            let kind = typed(start);
            let mut end = start + 1;
            while end < run.len() && typed(end) == kind {
                end += 1;
            }
            let piece = &run[start..end];
            let at = next(out);
            let head = piece[piece.len() - 1];
            let tail = piece[0];
            let mut back = if piece.len() > 1 && tail.x > head.x {
                1.0
            } else {
                -1.0
            };
            let mut cells = wedge;
            if !kind {
                cells = (head.x - tail.x).abs() * inv_cw + 1.0;
                let row = ((head.y - f32::from(geom.origin_y)) * inv_ch)
                    .floor()
                    .clamp(0.0, f32::from(u16::MAX)) as u16;
                let col = ((head.x - f32::from(geom.origin_x)) * inv_cw).floor();
                if let Some(&(_, _, first, last)) = leaps.iter().find(|(_, r, first, last)| {
                    *r == row && col >= f32::from(*first) - 1.0 && col <= f32::from(*last) + 1.0
                }) {
                    let (first, last) = (f32::from(first), f32::from(last));
                    if last - col > col - first {
                        back = 1.0;
                    }
                    let far = if back > 0.0 { last } else { first };
                    cells = cells.max((col - far).abs() + 1.0);
                }
            }
            let knots = if kind {
                knots
            } else {
                (cells * 4.0).ceil().max(1.0) as usize
            };
            let mut cov = head.cov;
            for s in piece {
                cov = cov.max(s.cov);
            }
            lengths.push(cells);
            let span = cells * cwf;
            let inv_knots = 1.0 / knots as f32;
            let dst = &mut out[at];
            dst.reserve(knots + 1);
            for k in (0..=knots).rev() {
                let u = k as f32 * inv_knots;
                dst.push(CometSample {
                    x: head.x + back * (span * u),
                    y: head.y,
                    cov,
                    pos: 1.0 - u,
                });
            }
            start = end;
        }
    }
    n
}

/// Clip the quads `out[from..]` to `(x0, y0, x1, y1)`, dropping any wholly outside.
pub(crate) fn clip_quads_to(
    out: &mut Vec<GlowQuad>,
    from: usize,
    (x0, y0, x1, y1): (i32, i32, i32, i32),
) {
    let mut kept = from;
    for i in from..out.len() {
        let q = out[i];
        let (qx0, qy0) = (i32::from(q.x), i32::from(q.y));
        let (qx1, qy1) = (qx0 + i32::from(q.w), qy0 + i32::from(q.h));
        let (cx0, cx1) = (qx0.max(x0), qx1.min(x1));
        let (cy0, cy1) = (qy0.max(y0), qy1.min(y1));
        if cx0 >= cx1 || cy0 >= cy1 {
            continue;
        }
        let mut c = q;
        c.x = cx0 as u16;
        c.y = cy0 as u16;
        c.w = (cx1 - cx0) as u16;
        c.h = (cy1 - cy0) as u16;
        if cx0 != qx0 || cx1 != qx1 {
            let inv_w = 1.0 / f32::from(q.w.max(1));
            if q.color != q.color2 {
                if cx0 != qx0 {
                    c.color =
                        crate::effect_util::lerp_rgb(q.color, q.color2, (cx0 - qx0) as f32 * inv_w);
                }
                if cx1 != qx1 {
                    c.color2 =
                        crate::effect_util::lerp_rgb(q.color, q.color2, (cx1 - qx0) as f32 * inv_w);
                }
            }
            if q.alpha != 0 {
                if q.alpha == q.alpha2 {
                    c.alpha = q.alpha.max(1);
                    c.alpha2 = q.alpha2.max(1);
                } else {
                    let lerp_a = |a: u8, b: u8, x: i32| {
                        let t = (x - qx0) as f32 * inv_w;
                        (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8
                    };
                    c.alpha = if cx0 != qx0 {
                        lerp_a(q.alpha, q.alpha2, cx0)
                    } else {
                        q.alpha
                    }
                    .max(1);
                    c.alpha2 = if cx1 != qx1 {
                        lerp_a(q.alpha, q.alpha2, cx1)
                    } else {
                        q.alpha2
                    }
                    .max(1);
                }
            }
        }
        out[kept] = c;
        kept += 1;
    }
    out.truncate(kept);
}

/// Split swept `runs` into the air segments drawn by the dark beam.
pub(crate) fn split_air_runs(
    runs: &[Vec<CometSample>],
    geom: Geom,
    text: &TextView<'_>,
    caret: bool,
    typed: Option<&[Vec<bool>]>,
    air: &mut Vec<Vec<CometSample>>,
) -> usize {
    let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
    let (inv_cw, inv_ch) = (1.0 / cwf, 1.0 / chf);
    let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let over_text = |s: &CometSample| {
        let (row, col) = (
            ((s.y - oyf) * inv_ch).floor() as i32,
            ((s.x - oxf) * inv_cw).floor() as i32,
        );
        text.over_text(row, col)
    };
    let knee = |t: &CometSample, a: &CometSample| {
        let (dx, dy) = (a.x - t.x, a.y - t.y);
        let exit = |d: f32, half: f32| {
            if d.abs() > 1e-3 {
                half / d.abs()
            } else {
                f32::INFINITY
            }
        };
        let k = exit(dx, cwf * 0.5).min(exit(dy, chf * 0.5)).min(1.0);
        CometSample {
            x: t.x + dx * k,
            y: t.y + dy * k,
            cov: 0,
            pos: a.pos,
        }
    };
    let mut n = 0usize;
    for (ri, run) in runs.iter().enumerate() {
        let head = caret && ri + 1 == runs.len();
        let lit = |i: usize| {
            typed.is_none_or(|kinds| {
                kinds
                    .get(ri)
                    .and_then(|kinds| kinds.get(i))
                    .copied()
                    .unwrap_or(true)
            })
        };
        let is_air = |i: usize| lit(i) && ((head && i + 1 == run.len()) || !over_text(&run[i]));
        let mut i = 0;
        while i < run.len() {
            if !is_air(i) {
                i += 1;
                continue;
            }
            let start = i;
            while i < run.len() && is_air(i) {
                i += 1;
            }
            if n == air.len() {
                air.push(Vec::new());
            }
            let out = &mut air[n];
            out.clear();
            if start > 0 {
                out.push(knee(&run[start - 1], &run[start]));
            }
            out.extend_from_slice(&run[start..i]);
            if i < run.len() {
                out.push(knee(&run[i], &run[i - 1]));
            }
            n += 1;
        }
    }
    n
}

/// The most blank columns between two letters whose bed is bridged as one
/// word gap ([`bridge_word_gaps`]).
pub(crate) const BED_WORD_GAP_COLS: i32 = 2;

/// A word gap's bed takes the line between its letters' inks (looks7 M4).
/// The bed walks the trail's hue knot by knot at one luminance, and at that
/// luminance a saturated blue reads far brighter than the violet or the teal
/// beside it. A letter shows its knot only between its strokes; a knot that
/// fell in a word gap stood its hue there as a solid block: Phaser's dark bar
/// a bright blue stripe between `the` and `quick` (the gallery's `context`
/// `mid`: `#032BE7` in the gap against `#034591` and `#2F1DD3` on the
/// letters beside it, all within 0.063 to 0.075 luminance; the stripe lies
/// in the bed alone, the letter guard and the over-ink account on or off
/// alike). A sample in a cell no letter covers, with a letter within
/// [`BED_WORD_GAP_COLS`] blank columns on each side of it on its row, takes
/// the ink interpolated between the first samples on those two letters, by
/// its x between theirs. A longer run of blanks, a gap one of whose letters
/// has no sample in the run, and every letter keep their own ink. `inks` is
/// one entry a sample, `runs` in order.
pub(crate) fn bridge_word_gaps(
    runs: &[Vec<CometSample>],
    geom: Geom,
    lettered: impl Fn(i32, i32) -> bool,
    inks: &mut [u32],
) {
    let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
    if cwf <= 0.0 || chf <= 0.0 {
        return;
    }
    let (inv_cw, inv_ch) = (1.0 / cwf, 1.0 / chf);
    let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let cell_of = |s: &CometSample| {
        (
            ((s.y - oyf) * inv_ch).floor() as i32,
            ((s.x - oxf) * inv_cw).floor() as i32,
        )
    };
    let reach = BED_WORD_GAP_COLS + 1;
    let mut at = 0usize;
    for run in runs {
        let base = at;
        at += run.len();
        if inks.len() < at {
            return;
        }
        for (i, s) in run.iter().enumerate() {
            let (row, col) = cell_of(s);
            if lettered(row, col) {
                continue;
            }
            let mut left = None;
            for d in 1..=reach {
                let c = col - d;
                if c < 0 {
                    break;
                }
                if lettered(row, c) {
                    left = Some(c);
                    break;
                }
            }
            let Some(a) = left else {
                continue;
            };
            let max_b = (col + reach).min(a + 1 + BED_WORD_GAP_COLS);
            let mut right = None;
            for c in col + 1..=max_b {
                if lettered(row, c) {
                    right = Some(c);
                    break;
                }
            }
            let Some(b) = right else {
                continue;
            };
            let on = |c: i32| run.iter().position(|q| cell_of(q) == (row, c));
            let (Some(ja), Some(jb)) = (on(a), on(b)) else {
                continue;
            };
            let (xa, xb) = (run[ja].x, run[jb].x);
            let u = if (xb - xa).abs() > 1e-3 {
                ((s.x - xa) / (xb - xa)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let (ca, cb) = (inks[base + ja], inks[base + jb]);
            inks[base + i] = crate::effect_util::lerp_rgb(ca, cb, u);
        }
    }
}

impl CursorGlow {
    /// Lay the dark bed under `self.comet_runs[..runs_len]`.
    #[allow(
        clippy::too_many_arguments,
        reason = "config, geometry, the runs, their shape, colour and caret, and the stream"
    )]
    pub(super) fn emit_dark_bed(
        &mut self,
        cfg: &GlowConfig,
        geom: Geom,
        runs_len: usize,
        shape: BedShape,
        color: &dyn Fn(&CursorGlow, f32) -> u32,
        caret: Option<(u16, u16)>,
        under: &mut Vec<GlowQuad>,
    ) {
        let (ink, page) = Self::bed_pair(cfg);
        let budget = dim_budget(dark_bed_budget(ink), page, shape.dim);
        let mut scratch = std::mem::take(&mut self.bed_scratch);
        scratch.inks.clear();
        scratch.rails.clear();
        scratch.caret = caret.map(|(r, c)| (i32::from(r), i32::from(c)));
        let mut wedges = std::mem::take(&mut self.wedge_runs);
        self.leap_spans.clear();
        if shape.leap_path {
            for spark in self.sparks.iter().filter(|spark| !spark.typing) {
                match self
                    .leap_spans
                    .iter_mut()
                    .find(|(born, row, ..)| *born == spark.born && *row == spark.row)
                {
                    Some((_, _, first, last)) => {
                        *first = (*first).min(spark.col);
                        *last = (*last).max(spark.col);
                    }
                    None => self
                        .leap_spans
                        .push((spark.born, spark.row, spark.col, spark.col)),
                }
            }
        }
        scratch.wedges.clear();
        let runs: &[Vec<CometSample>] = if shape.wedge > 0.0 {
            let n = wedge_runs(
                &self.comet_runs[..runs_len],
                &self.comet_typing[..runs_len.min(self.comet_typing.len())],
                &self.leap_spans,
                geom,
                shape,
                &mut wedges,
                &mut scratch.wedges,
            );
            &wedges[..n]
        } else {
            &self.comet_runs[..runs_len]
        };
        let shape = if self.colour_pins.trail && cfg.style.draws_trail_color() {
            shape.pinned()
        } else {
            shape
        };
        let air = AirLight::of(page, budget, shape).filter(|_| !self.bed_air_off());
        let lit_page = page_lit(page, dark_bed_budget(ink)) && !self.bed_air_off();
        let shape = if lit_page { shape.on_lit_page() } else { shape };
        scratch.rail_drop = if lit_page {
            lit_rail_drop(geom.ch as f32)
        } else {
            0.0
        };
        let air_text = TextView::new(self.probed_text_spans(), Some(&self.frame_text));
        let mut lit = std::mem::take(&mut scratch.air);
        let mut last: Option<(u32, bool, u32, u32)> = None;
        for run in runs {
            air_lit(
                run,
                geom,
                &air_text,
                (air.is_some(), self.bed_air_alone()),
                &mut lit,
            );
            for (s, &in_air) in run.iter().zip(&lit) {
                let raw = color(self, s.pos);
                let (bed_c, rail_c) = match last {
                    Some((prev, was_air, b, r)) if prev == raw && was_air == in_air => (b, r),
                    _ => {
                        let hue = match shape.hue {
                            BedHue::Night => night_rgb(raw),
                            BedHue::True => raw,
                        };
                        // Under the letters a lit page's bed lays its hue at
                        // the page's luminance: a grey there only greys the
                        // page, so the floored hue is kept (and is the page's).
                        let b = match air {
                            Some(air) if in_air => air.ink(hue, page, shape.sat_for(hue)),
                            _ => self.bed_ink(hue, page, budget, shape.under_sat_for(hue)),
                        };
                        let r = if shape.rail > 0.0 {
                            bed_rail_ink(raw, shape)
                        } else {
                            0
                        };
                        last = Some((raw, in_air, b, r));
                        (b, r)
                    }
                };
                scratch.inks.push(bed_c);
                if shape.rail > 0.0 {
                    scratch.rails.push(rail_c);
                }
            }
        }
        scratch.air = lit;
        if !self.bed_gaps_unbridged() {
            let ft = &self.frame_text;
            bridge_word_gaps(
                runs,
                geom,
                |row, col| ft.pair(row, col).is_some_and(|(ink, _)| ink.is_some()),
                &mut scratch.inks,
            );
        }
        let text = TextView::new(self.probed_text_spans(), Some(&self.frame_text));
        lay_dark_bed(
            runs,
            geom,
            page,
            shape,
            &text,
            &mut scratch,
            &mut self.ink_ledger,
            under,
        );
        self.bed_scratch = scratch;
        self.wedge_runs = wedges;
    }

    /// A test's negative control: a word gap's bed keeps its own knot's ink
    /// ([`bridge_word_gaps`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_word_gaps(&mut self, on: bool) {
        self.bed_gaps_unbridged = !on;
    }

    /// Whether a word gap's bed keeps its own knot's ink ([`Self::set_bed_word_gaps`]).
    #[inline]
    const fn bed_gaps_unbridged(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            self.bed_gaps_unbridged
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            false
        }
    }

    /// [`dark_bed_ink`] ([`Self::bed_unsolved`]).
    fn bed_ink(&self, rgb: u32, page: u32, budget: f32, sat: f32) -> u32 {
        if self.bed_unsolved() {
            let rgb = crate::spectrum::spectrum_with_min_saturation(rgb & 0x00FF_FFFF, sat);
            let r = (rgb >> 16) & 0xff;
            let g = (rgb >> 8) & 0xff;
            let b = rgb & 0xff;
            let rg = if r > g { r } else { g };
            let rgb_max = if rg > b { rg } else { b };
            let mx = if rgb_max > 1 { rgb_max } else { 1 };
            #[inline]
            const fn up(rgb: u32, mx: u32, sh: u32) -> u32 {
                let v = (((rgb >> sh) & 0xff) * 255) / mx;
                if v < 255 { v } else { 255 }
            }
            (up(rgb, mx, 16) << 16) | (up(rgb, mx, 8) << 8) | up(rgb, mx, 0)
        } else if self.bed_page_unlit() {
            dark_bed_ink_unlit(rgb, page, budget, sat)
        } else {
            dark_bed_ink(rgb, page, budget, sat)
        }
    }

    /// Whether the bed is held to its budget alone ([`Self::bed_page_unlit`]).
    #[inline]
    const fn bed_page_unlit(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            self.bed_page_unlit
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            false
        }
    }

    /// A test's negative control: a lit page's bed laid at the page's
    /// luminance in the air too (`on` false: no [`AirLight`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_air_light(&mut self, on: bool) {
        self.bed_air_off = !on;
    }

    /// A test's negative control: a sample in the air by its own cell's
    /// text alone (`on` false), not its neighbours' ([`air_lit`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_air_beside_text(&mut self, on: bool) {
        self.bed_air_alone = !on;
    }

    #[inline]
    const fn bed_air_off(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            self.bed_air_off
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            false
        }
    }

    #[inline]
    const fn bed_air_alone(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            self.bed_air_alone
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            false
        }
    }

    /// Split `self.comet_runs[..runs_len]` into `self.air_runs`.
    pub(super) fn split_air_runs(
        &mut self,
        geom: Geom,
        runs_len: usize,
        caret: bool,
        typed_only: bool,
    ) -> usize {
        let text = TextView::new(self.probed_text_spans(), Some(&self.frame_text));
        let typed = typed_only.then(|| &self.comet_typing[..runs_len.min(self.comet_typing.len())]);
        split_air_runs(
            &self.comet_runs[..runs_len],
            geom,
            &text,
            caret,
            typed,
            &mut self.air_runs,
        )
    }

    /// The pixel box of the caret's cell `(row, col)`, window px.
    #[inline]
    pub(crate) const fn caret_box(geom: Geom, row: u16, col: u16) -> (i32, i32, i32, i32) {
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let x0 = (geom.origin_x as i32) + (col as i32) * cw;
        let y0 = (geom.origin_y as i32) + (row as i32) * ch;
        (x0, y0, x0 + cw, y0 + ch)
    }

    /// Whether an air run is only the head's bridge.
    #[inline]
    pub(super) const fn is_bridge(run: &[CometSample]) -> bool {
        matches!(run, [knee, _] if knee.cov == 0)
    }

    /// What this frame shows of text.
    #[inline]
    pub(crate) const fn text_view(&self) -> TextView<'_> {
        TextView::new(self.probed_text_spans(), Some(&self.frame_text))
    }

    /// The caret row's glyph column span `first..end`.
    #[inline]
    #[must_use]
    pub(crate) const fn caret_row_glyphs(&self, (row, col): (u16, u16)) -> Option<(i32, i32)> {
        let row = row as i32;
        let view = self.text_view();
        if view.spans.reads_row(row) {
            return view.spans.line_at(row);
        }
        match view.frame_line(row) {
            Some(span) => span,
            None => {
                if col > 0 {
                    Some((0, col as i32))
                } else {
                    None
                }
            }
        }
    }
}

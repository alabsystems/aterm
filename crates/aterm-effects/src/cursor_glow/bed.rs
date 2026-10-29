// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dark-ground cursor-glow bed and leading rail laid beneath glyph ink.

use super::*;
use aterm_render::{BeamRows, GlowBlend, RampSink, RibbonVertex, ribbon_beam_into};

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
#[inline]
pub(crate) const fn page_lit_ink(rgb: u32, page: u32) -> u32 {
    if (rgb & 0x00FF_FFFF) == (page & 0x00FF_FFFF) {
        return page & 0x00FF_FFFF;
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
    let table = crate::color_math::relative_luminance_table();
    let p = [
        table.linear[((page >> 16) & 0xff) as usize],
        table.linear[((page >> 8) & 0xff) as usize],
        table.linear[(page & 0xff) as usize],
    ];
    let q = [
        table.linear[((rgb >> 16) & 0xff) as usize],
        table.linear[((rgb >> 8) & 0xff) as usize],
        table.linear[(rgb & 0xff) as usize],
    ];
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
    let page_l = lp;
    let mut best = (lo[0] << 16) | (lo[1] << 8) | lo[2];
    let mut best_l = crate::color_math::relative_luminance_rgb(
        table,
        lo[0] as usize,
        lo[1] as usize,
        lo[2] as usize,
    );
    let mut bit = 1u32;
    while bit < 8 {
        let c0 = if (bit & 1) != 0 { hi[0] } else { lo[0] };
        let c1 = if (bit & 2) != 0 { hi[1] } else { lo[1] };
        let c2 = if (bit & 4) != 0 { hi[2] } else { lo[2] };
        let c = (c0 << 16) | (c1 << 8) | c2;
        let l =
            crate::color_math::relative_luminance_rgb(table, c0 as usize, c1 as usize, c2 as usize);
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
    #[inline]
    pub(crate) const fn of(page: u32, budget: f32, shape: BedShape) -> Option<Self> {
        const REF_BUDGET: f32 = dark_bed_budget(CursorGlow::STREAK_REF_INK);
        let air = dim_budget(REF_BUDGET, page, shape.dim);
        if air > budget && page_lit(page, budget) {
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
#[inline]
pub(crate) const fn dark_bed_ink(rgb: u32, bg: u32, budget: f32, sat: f32) -> u32 {
    let solved = dark_bed_ink_unlit(rgb, bg, budget, sat);
    let table = crate::color_math::relative_luminance_table();
    if crate::color_math::relative_luminance_with(table, solved)
        < crate::color_math::relative_luminance_with(table, bg & 0x00FF_FFFF)
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
#[inline]
pub(crate) const fn dark_bed_ink_unlit(rgb: u32, bg: u32, budget: f32, sat: f32) -> u32 {
    #[inline]
    const fn ch(c: u32, sh: u32) -> u32 {
        (c >> sh) & 0xff
    }
    let bg_rgb = [ch(bg, 16) as usize, ch(bg, 8) as usize, ch(bg, 0) as usize];
    let table = crate::color_math::relative_luminance_table();
    if crate::color_math::relative_luminance_rgb(table, bg_rgb[0], bg_rgb[1], bg_rgb[2]) > budget {
        return 0;
    }
    let rgb = crate::spectrum::spectrum_with_min_saturation(rgb & 0x00FF_FFFF, sat);
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
    const fn seen(
        table: &crate::color_math::RelativeLuminanceTable,
        bg_rgb: [usize; 3],
        ink: [usize; 3],
    ) -> f32 {
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
        crate::color_math::relative_luminance_rgb(table, r, g, b)
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let mut lo_rgb = [0usize; 3];
    let mut hi_rgb = pick_rgb(full, 1.0);
    let (mut lo_ok, mut hi_ok) = (true, false);
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
        } else if seen(table, bg_rgb, mid_rgb) <= budget {
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
    /// The share of the rail's reach its full-strength plateau holds
    /// ([`DARK_RAIL_CORE`]; `0`: it falls off from its top).
    pub(crate) rail_core: f32,
    /// A soft top the rail lays in the air below the row, over its spine,
    /// as a share of the cell height (`0`: its top is its spine).
    pub(crate) rail_soft: f32,
    /// The share of the rail's reach that follows the bed's taper behind its
    /// head (`0`: an even tube; Comet's nucleus-led tail, [`COMET_RAIL_TAPER`]).
    pub(crate) rail_taper: f32,
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
    /// The rail is a comet's icy tail ([`icy_rail_ink`]): ice blue at its
    /// tail to near-white at its nucleus, its light lifted over the plain
    /// rail's ([`ICY_RAIL_ALPHA`]), laid clear of the judge's ring below the
    /// row ([`lit_rail_drop`]) and, on a dark page, within its leading
    /// ([`DARK_RAIL_LEADING_CH`]), with a white nucleus under the caret
    /// ([`CursorGlow::push_comet_nucleus`]). On the reference dark Comet's
    /// typing trail was a deep-blue sliver under the last letter, its dust
    /// grey specks (looks8 round 2's K4).
    pub(crate) icy: bool,
    /// The rail is laid only under the caret's row: a run the caret has left
    /// (a submitted line) keeps its bed and loses its rail. Water's lit rail
    /// lay on after an Enter as a cyan dash detached under the typed line's
    /// last word (looks8 round 2's K4).
    pub(crate) rail_caret_row: bool,
    /// The rail is foam ([`foam_rail`]): its ink's saturation cut by this
    /// share ([`bed_rail_ink`]), its swollen crest broken over the swell's
    /// troughs, and a droplet shed under each break (`0`: a whole rail in
    /// the rail's ink). Water's lit rail was a crisp saturated 2-3 px cyan
    /// sine under its row that read as a spell-check squiggle (looks9 Q3).
    pub(crate) foam: f32,
    /// Where the row under the rail is no air (a selection, a powerline
    /// bar, a lettered row, the pane's floor), the rail lies in its own
    /// row's foot band ([`ROW_RAIL_CH`]), parted round each foot of the row
    /// ([`part_round_feet`]), as the ribbons' rail does: laid under the row,
    /// the frame cut it off the selection, and in the gallery's `context`
    /// Comet and Sparkle drew a bare glowing caret (looks10 L2), and on
    /// Solarized Dark, whose letters take no light, so did Beam, Lumen and
    /// Water, Phaser only a faint dark wash (looks10 round 2).
    pub(crate) row_rail: bool,
}

/// Whether `page` is too lit for a bed solved to `budget` to lay any light
/// under its letters (Solarized Dark: [`page_lit_ink`]). Asked of the
/// letters' own budget ([`dark_bed_budget`]), it is the page's: a shape
/// dimmed under it ([`BedShape::dim`], Sparkle's gold rail's) solves to
/// black on any page.
#[must_use]
#[inline]
pub(crate) const fn page_lit(page: u32, budget: f32) -> bool {
    let probe = dark_bed_ink_unlit(0x00FF_0000, page, budget, 1.0);
    let table = crate::color_math::relative_luminance_table();
    crate::color_math::relative_luminance_with(table, probe)
        < crate::color_math::relative_luminance_with(table, page & 0x00FF_FFFF)
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

/// The deepest below its row's bottom a comet's icy tail lies, px: laid
/// [`lit_rail_drop`] below the row, as deep as its nucleus ([`COMET_RAIL`]).
/// Comet's dark dust floats out beneath it ([`crate::cursor_glow::CursorGlow`]'s
/// air seats), never stippling the tail's stroke.
#[inline]
#[must_use]
pub(crate) const fn icy_rail_floor(ch: f32) -> f32 {
    lit_rail_drop(ch) + COMET_RAIL * ICY_AIR_RAIL_GROW * ch
}

/// How much deeper than its nucleus's depth ([`COMET_RAIL`]) a comet's icy
/// tail lies over a bare row ([`BedScratch::rail_air_room`]): held to the
/// leading it was a 2 px hairline on the reference dark (looks9 round 2's Q2).
pub(crate) const ICY_AIR_RAIL_GROW: f32 = 1.35;

/// The vivid rail's ink for the colour `rgb` of a bed of `shape`
/// ([`crate::rainbow_kitty::ribbon::rail_ink`]); a near-grey colour of a
/// shape that keeps its own chroma ([`BedShape::own_chroma`]) keeps it,
/// brought to the rail's brightness.
/// A chosen colour ([`BedShape::pinned`]) keeps its chroma the same way.
#[must_use]
#[inline]
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
    if shape.pinned
        || (shape.own_chroma > 0.0 && crate::color_math::rgb2hsv(rgb).1 < shape.own_chroma)
    {
        let mx = max_u32(
            max_u32((rgb >> 16) & 0xff, (rgb >> 8) & 0xff),
            max_u32(rgb & 0xff, 1),
        );
        (up(rgb, mx, 16) << 16) | (up(rgb, mx, 8) << 8) | up(rgb, mx, 0)
    } else if shape.foam > 0.0 {
        let (h, s, v) = crate::color_math::rgb2hsv(crate::rainbow_kitty::ribbon::rail_ink(rgb));
        crate::color_math::hsv2rgb(h, s * (1.0 - shape.foam), v)
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
        rail_core: DARK_RAIL_CORE,
        rail_soft: 0.0,
        rail_taper: 0.0,
        hue: BedHue::True,
        core: 0.5,
        dim: 1.0,
        leap_path: false,
        leap_air: false,
        lit_rail: 0.0,
        own_chroma: 0.0,
        pinned: false,
        icy: false,
        rail_caret_row: false,
        foam: 0.0,
        row_rail: false,
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
        if !self.pinned && self.own_chroma <= 0.0 {
            return self.sat;
        }
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
                rail_core: 0.0,
                rail_soft: LUMEN_RAIL_SOFT,
                own_chroma: LUMEN_OWN_CHROMA,
                row_rail: true,
                ..Self::BASE
            }),
            GlowStyle::Comet => Some(Self {
                reach: 0.40,
                thin: 0.75,
                sat: 0.85,
                full: 215.0,
                wedge: 3.5,
                rail: COMET_RAIL,
                rail_taper: COMET_RAIL_TAPER,
                icy: true,
                leap_path: true,
                leap_air: true,
                row_rail: true,
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
                row_rail: true,
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
                row_rail: true,
                ..Self::BASE
            }),
            GlowStyle::Beam => Some(Self {
                reach: 0.14 + 0.26 * beam_pw,
                thin: 0.0,
                sat: 0.85,
                full: 235.0,
                leap_air: true,
                lit_rail: BEAM_LIT_RAIL,
                row_rail: true,
                ..Self::BASE
            }),
            _ => None,
        }
    }

    /// Water's bed, on a lit page alone (`CursorGlow::lay_water_bed`): on
    /// the reference dark its own stream reads over the page, but on a lit
    /// page (Solarized Dark) the letter law held that stream to a whisper
    /// behind the caret (23 px lifted a 24th of the way, looks8 K4). There
    /// it lays what Beam and Lumen lay: its hue under the letters at the
    /// page's luminance and its stream as a rail in the air below them
    /// ([`WATER_LIT_RAIL`], [`lit_rail_drop`]), full at its wake's cap.
    /// Its rail is foam ([`Self::foam`]): soft from its peak both ways
    /// ([`WATER_RAIL_SOFT`], no plateau), pale, broken, shedding droplets.
    pub(super) const WATER_LIT: Self = Self {
        reach: 0.30,
        thin: 0.45,
        sat: 0.85,
        full: CursorGlow::WATER_WAKE_COV_CAP,
        leap_air: true,
        lit_rail: WATER_LIT_RAIL,
        rail_core: 0.0,
        rail_soft: WATER_RAIL_SOFT,
        rail_caret_row: true,
        foam: WATER_FOAM,
        row_rail: true,
        ..Self::BASE
    };

    /// Test-only: Water's lit rail a whole crisp sine in the rail's ink, as
    /// looks8 laid it (looks9 Q3's negative control).
    #[cfg(test)]
    #[inline]
    pub(crate) const fn foamless(self) -> Self {
        Self {
            foam: 0.0,
            ..self.rail_even()
        }
    }

    /// Test-only: Water's foam as looks9 laid it, a pale sea-foam aqua
    /// ([`WATER_FOAM`] then 0.6) with a steep top ([`WATER_RAIL_SOFT`] then
    /// 0.15; looks10 L5's negative control).
    #[cfg(test)]
    pub(crate) const fn sea_foam(self) -> Self {
        Self {
            foam: 0.6,
            rail_soft: 0.15,
            ..self
        }
    }

    /// Test-only: the rail an even tube at full strength from its top, as
    /// looks7 laid every rail (looks8 K10's negative control).
    #[cfg(test)]
    #[inline]
    pub(crate) const fn rail_even(self) -> Self {
        Self {
            rail_core: DARK_RAIL_CORE,
            rail_soft: 0.0,
            rail_taper: 0.0,
            ..self
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
/// Water's rail on a lit page ([`BedShape::WATER_LIT`]): its stream's light
/// in the air below its letters, a band between Lumen's hairline and Beam's
/// tube.
const WATER_LIT_RAIL: f32 = 0.16;
/// Water's foam's soft top, a share of the cell height ([`BedShape::rail_soft`]):
/// at 0.15 its crests rose from the page in two pixels, a hard white edge on
/// Solarized Dark (looks10 L5).
const WATER_RAIL_SOFT: f32 = 0.22;
/// The share of the rail ink's saturation Water's foam gives up
/// ([`BedShape::foam`]): its vivid cyan (`#00c9d7` over Solarized Dark) a
/// watery cyan. At 0.6 it was a pale sea-foam aqua, its crests near white
/// (`#83d5db`) on Solarized Dark's teal (looks10 L5).
const WATER_FOAM: f32 = 0.35;
/// Where the foam's crest breaks ([`foam_rail`]): its light is whole from
/// [`FOAM_BREAK`]`.1` of the way up from a trough to a crest, none under
/// [`FOAM_BREAK`]`.0`, eased between.
const FOAM_BREAK: (f32, f32) = (0.22, 0.5);
/// The swell of the break line ([`foam_rail`]), a share of the crest's
/// height, and its rate as a share of the swell's: some troughs stay
/// joined and others break wide, so the foam never reads as a regular
/// dash.
const FOAM_WOBBLE: (f32, f32) = (0.22, 0.43);
/// A droplet's radius and its least fall below the trough it is shed
/// under, as shares of the cell height, and its light as a share of the
/// foam's there. Each trough's droplet falls up to half as far again, by
/// the golden ratio's walk over the troughs, so they never line up.
const FOAM_DROP: (f32, f32, f32) = (0.09, 0.08, 1.0);
/// The foam's second break line ([`foam_rail`]), a share of the crest's
/// height, and its rate as a share of the swell's, against the first
/// ([`FOAM_WOBBLE`]): with one, the crest broke at every trough a like
/// arc's length apart, a droplet under each, a dash-dot rule on Solarized
/// Dark (looks9 round 2's Q3). The two beat together, so crests run one,
/// two or three swells long and the breaks open narrow or wide.
const FOAM_WOBBLE_2: (f32, f32) = (0.16, 0.71);
/// How far the foam's swell lifts and lowers whole crests ([`swell_rail`]),
/// a share of its depth, by a slow beat of its own ([`FOAM_SWELL_VARY_RATE`],
/// a share of the swell's rate): crests of one height read as a regular rule
/// (looks9 round 2's Q3).
pub(super) const FOAM_SWELL_VARY: f32 = 0.5;
/// How far the foam's swell's phase wanders from an even sine, radians
/// ([`foam_theta`]), by a beat at [`FOAM_PM_RATE`] of the swell's rate: its
/// crests and breaks fall closer and farther apart, never at one pitch.
const FOAM_PM: f32 = 0.9;
/// The rate of [`FOAM_PM`]'s beat, a share of the swell's.
const FOAM_PM_RATE: f32 = 0.29;

/// The rate of [`FOAM_SWELL_VARY`]'s beat, a share of the swell's.
const FOAM_SWELL_VARY_RATE: f32 = 0.37;

/// The foam's swell's phase at `x` ([`swell_rail`], [`foam_rail`]): `x * k +
/// phase`, wandering by [`FOAM_PM`] when `varied`.
#[inline]
const fn foam_theta(x: f32, k: f32, phase: f32, varied: bool) -> f32 {
    let th = x * k + phase;
    if varied {
        th + FOAM_PM * crate::supernova::sin_cos_f32(x * k * FOAM_PM_RATE + 1.1).0
    } else {
        th
    }
}
/// The share of the foam's breaks that shed no droplet ([`foam_rail`]), by
/// the golden ratio's walk over the troughs; the rest shed droplets of sizes
/// from [`FOAM_DROP_SIZES`]`.0` to `.1` of [`FOAM_DROP`]'s.
const FOAM_DROP_SKIP: f32 = 0.25;
/// The least and greatest droplet over [`FOAM_DROP`]'s radius.
const FOAM_DROP_SIZES: (f32, f32) = (0.6, 1.25);
/// The cells over which the foam's crest feathers in to the caret
/// ([`foam_rail`]): its last arc ran at full light into the caret's edge,
/// cut flat there (looks9 round 2's Q3).
const FOAM_HEAD_FEATHER_CELLS: f32 = 1.2;
/// The foam's body of water ([`foam_body`]): its light at the swell's
/// surface as a share of the rail's there, and how deep below the surface it
/// fades out over a bare row, a share of the cell height. Its crests alone,
/// broken over the troughs, read as a dash-dot rule on Solarized Dark, pale
/// arcs with a droplet between them at one height under the row (looks9
/// round 3's Q3); over a soft, continuous swell of water fading downward,
/// the crests are whitecaps on a wave, and no droplet stands between them.
pub(super) const FOAM_BODY: (f32, f32) = (0.5, 0.5);

/// Lay the foam's body of water ([`FOAM_BODY`]) along the swollen rail
/// `verts` into `body`: the swell's surface as its top, soft over the rail's
/// own reach above it, its light fading down from the surface to nothing
/// `deep` px below, whole along the rail (no break), and feathering in to the
/// caret as the crest does ([`FOAM_HEAD_FEATHER_CELLS`]) unless `head_cut`.
#[inline]
pub(super) fn foam_body(
    verts: &[RibbonVertex],
    body: &mut Vec<RibbonVertex>,
    (deep, cw): (f32, f32),
    head_cut: bool,
) {
    body.clear();
    if verts.len() < 2 || deep <= 0.0 {
        return;
    }
    let head = verts[verts.len() - 1].x;
    let inv_span = 1.0 / (FOAM_HEAD_FEATHER_CELLS * cw);
    body.reserve(verts.len());
    let mut i = 0;
    while i < verts.len() {
        let v = &verts[i];
        i += 1;
        let u = if head_cut {
            1.0
        } else {
            ((head - v.x).abs() * inv_span).clamp(0.0, 1.0)
        };
        body.push(RibbonVertex {
            dn: deep.max(v.dn),
            core_up: 0.0,
            core_dn: 0.0,
            cov: v.cov * FOAM_BODY.0 * (u * u * (3.0 - 2.0 * u)),
            ..*v
        });
    }
}
/// Lumen's rail on a lit page ([`BedShape::lit_rail`]): a soft line of its
/// tracer's light, falling off from its peak both ways
/// ([`LUMEN_RAIL_SOFT`], no plateau). As a 0.10 ch band at full strength
/// over half its reach it was a crisp near-white 2 px hairline on Solarized
/// Dark that read as a text underline (looks8 K10).
const LUMEN_LIT_RAIL: f32 = 0.16;
/// Lumen's rail's soft top, a share of the cell height ([`BedShape::rail_soft`]).
const LUMEN_RAIL_SOFT: f32 = 0.10;
/// Comet's rail at its nucleus, a share of the cell height: as deep as
/// Beam's lit tube there ([`BEAM_LIT_RAIL`]), tapering behind it
/// ([`COMET_RAIL_TAPER`]). At an even 0.16 ch its typing trail on the
/// reference dark was very quiet (looks8 K4).
const COMET_RAIL: f32 = BEAM_LIT_RAIL;
/// The share of Comet's rail that follows its bed's taper
/// ([`BedShape::rail_taper`]): full at its nucleus, a third of that at its
/// tail. As an even band it read as Beam's tube on Solarized Dark, both a
/// cyan-blue underline rail (looks8 K10).
const COMET_RAIL_TAPER: f32 = 0.66;
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
        195.0 + u * (75.0 / 180.0)
    } else if u <= 300.0 {
        270.0 + (u - 180.0) * (60.0 / 120.0)
    } else {
        330.0 - (u - 300.0) * (135.0 / 60.0)
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
    /// The rail's swell, `(depth px, radians per px, phase)`: its spine
    /// falls `depth * (1 + sin(x * k + phase)) / 2` below the row's bottom,
    /// never rising into the row (`None`: a straight rail). Water's lit rail
    /// was a straight cyan bar, Beam's tube in another hue (looks8 round 2's
    /// K4).
    pub(crate) rail_wave: Option<(f32, f32, f32)>,
    /// The deepest the rail may lie below [`Self::rail_drop`], px
    /// ([`DARK_RAIL_LEADING_CH`]; infinite: the shape's own depth).
    pub(crate) rail_room: f32,
    /// Whether a rail held to [`Self::rail_room`] takes the shape's own depth
    /// under a row whose next row is bare: no letter there for the leading to
    /// keep clear of. Held to the leading over an empty row, Comet's icy
    /// tail was a 2 px hairline on the reference dark (looks9 round 2's Q2).
    pub(crate) rail_air_room: bool,
    /// Whether [`Self::rail_air_room`] reads the next row by its letters
    /// alone, as looks9 round 2 read it, so a filled row bare of letters (a
    /// selection, a powerline bar) took the tail's whole depth (a test's
    /// control).
    pub(crate) rail_air_text_only: bool,
    /// Whether Comet's icy tail in its own row's foot band
    /// ([`BedShape::row_rail`]) is laid as round 1 laid it, a soft 2 px line
    /// falling off with its taper, which in the gallery's `context` was a
    /// hairline under the last letters (a test's control). Laid whole
    /// across the foot band's depth and falling off with the taper's square
    /// root, it is the comet's tail (looks10 L2).
    pub(crate) row_tail_faint: bool,
    /// Whether Comet's icy tail over air runs whole to the caret's far edge
    /// under its nucleus, as looks9 laid it: with the nucleus it saturated
    /// to a hard white bar at the caret's foot (looks9's final look; a
    /// test's control). Feathered out across the caret's cell, the nucleus
    /// is the tail's round head (looks10).
    pub(crate) nucleus_bar: bool,
    /// Whether a foam rail is laid as looks9 round 1 laid it ([`foam_rail`]):
    /// one break line, crests of one height at one pitch, and a droplet of
    /// one size under every break (a test's control).
    pub(crate) foam_even: bool,
    /// Whether a foam rail's last arc runs whole into the caret, cut flat at
    /// its edge, as looks9 round 1 laid it ([`foam_rail`]; a test's control).
    pub(crate) foam_head_cut: bool,
    /// Whether a foam rail is laid without its body of water ([`FOAM_BODY`]),
    /// its crests and droplets alone over the page, as looks9 round 2 laid it
    /// (a test's control).
    pub(crate) foam_bodiless: bool,
    /// The foam rail's body of water ([`FOAM_BODY`]), laid under its crests.
    body: Vec<RibbonVertex>,
    /// Whether the row under the caret's is air, bare of letters and on the
    /// page's own ground: the foam's body takes its whole depth there
    /// ([`FOAM_BODY`]), and keeps to the rail's own over anything else.
    pub(crate) foam_air: bool,
    /// The droplets a foam rail sheds ([`foam_rail`]): `(x, y, size, light,
    /// ink)`, `size` over [`FOAM_DROP`]'s radius.
    pub(crate) drops: Vec<(f32, f32, f32, f32, u32)>,
    /// The feet of the rows whose glyphs the host named, window px
    /// (`CursorGlow::foot_boxes`): a rail in its own row's foot band
    /// ([`BedShape::row_rail`]) parts round each.
    pub(crate) feet: Vec<(i32, i32, i32, i32)>,
    /// The rows [`Self::feet`] were read on, window px: a rail lies in its
    /// own row only on one of them.
    pub(crate) soles: [Option<(i32, i32, i32, i32)>; 2],
    /// A rail in its row's foot band resampled to part round its feet
    /// ([`part_round_feet`]).
    parted: Vec<RibbonVertex>,
    /// A test's negative control: no rail lies in its own row
    /// ([`BedShape::row_rail`]), as looks9 laid them.
    pub(crate) row_rail_off: bool,
    /// A test's negative control: a swollen rail in its own row is lifted by
    /// its swell's fall over the foot band's top, toward the letters, as
    /// looks10 round 2 first laid it ([`BedShape::row_rail`]).
    pub(crate) row_rail_swell_lifts: bool,
    /// The tinted dark page the frame lays night grounds on
    /// (`CursorGlow::night_grounded`), whose rail is laid page-matched
    /// ([`PageMatched`]); `None` elsewhere.
    pub(crate) rail_page: Option<u32>,
    /// A test's negative control: the page-matched rail's lights are laid at
    /// the least opacity whatever their colour's hue ([`PageMatched::hue_free`]).
    pub(crate) rail_hue_free: bool,
}

/// A beam's sink that lays each source-over light of the beam over the
/// tinted dark page `page` as it reads on the reference dark page
/// ([`super::raster::page_matched`]); an added light passes as laid. Faded
/// along the tail by its opacity over Solarized Dark's teal, Sparkle's amber
/// rail let the page's green and blue through and read green as it faded
/// (`#2c3c2d`, hue 124, under the trail gallery's typed line, looks10 L11).
///
/// Each light keeps its own hue ([`super::raster::page_matched_held`]):
/// solved at the least opacity, a fading rail's colour carried the teal's
/// missing red, and laid over the bed under it, not the page, it read rust.
struct PageMatched<'s, S: BeamRows> {
    out: &'s mut S,
    page: u32,
    /// A test's negative control: each light at the least opacity that
    /// reaches its reading, whatever its colour's hue ([`super::raster::page_matched`]).
    hue_free: bool,
}

impl<S: BeamRows> PageMatched<'_, S> {
    #[inline]
    const fn end(&self, (c, a): (u32, u8)) -> (u32, u8) {
        if a == 0 {
            return (c, a);
        }
        #[inline]
        const fn un(c: u32, sh: u32, a: u32) -> u32 {
            let v = (((c >> sh) & 0xff) * 255 + a / 2) / a;
            if v < 255 { v } else { 255 }
        }
        let a32 = a as u32;
        let rgb = (un(c, 16, a32) << 16) | (un(c, 8, a32) << 8) | un(c, 0, a32);
        if self.hue_free {
            super::raster::page_matched(rgb, a, self.page)
        } else {
            super::raster::page_matched_held(rgb, a, self.page)
        }
    }
}

impl<S: BeamRows> BeamRows for PageMatched<'_, S> {
    type Mark = S::Mark;
    #[inline]
    fn laid(&self) -> usize {
        self.out.laid()
    }
    #[inline]
    fn mark(&mut self) -> S::Mark {
        self.out.mark()
    }
    #[inline]
    fn truncate(&mut self, mark: S::Mark) {
        self.out.truncate(mark);
    }
    #[inline]
    fn slab(&mut self) {
        self.out.slab();
    }
    #[inline]
    fn row(&mut self, band: i32, tag: u16, xyw: (u16, u16, u16), a: (u32, u8), b: (u32, u8)) {
        let (a, b) = (self.end(a), self.end(b));
        self.out.row(band, tag, xyw, a, b);
    }
    #[inline]
    fn rect(&mut self, band: i32, tag: u16, xywh: (u16, u16, u16, u16), end: (u32, u8)) {
        let end = self.end(end);
        self.out.rect(band, tag, xywh, end);
    }
    #[inline]
    fn quad(&mut self, q: GlowQuad) {
        let ((color, alpha), (color2, alpha2)) =
            (self.end((q.color, q.alpha)), self.end((q.color2, q.alpha2)));
        self.out.quad(GlowQuad {
            color,
            alpha,
            color2,
            alpha2,
            ..q
        });
    }
    #[inline]
    fn rides(&self, lights: usize) -> bool {
        self.out.rides(lights)
    }
}

/// A comet's icy tail at path position `pos` (tail 0, head 1)
/// ([`BedShape::icy`]): ice blue to near-white.
#[inline]
#[must_use]
pub(crate) const fn icy_rail_ink(pos: f32) -> u32 {
    let p = pos.clamp(0.0, 1.0);
    crate::effect_util::lerp_rgb(ICY_TAIL, ICY_NUCLEUS, p * p * p)
}

/// How deep a rail lies in its own row's foot band ([`BedShape::row_rail`]),
/// as a share of the cell height: the ribbons' in-row rail's.
const ROW_RAIL_CH: f32 = crate::rainbow_kitty::ribbon::ROW_RAIL_CH;

/// The deepest a swollen rail laid in its own row's foot band falls, a share
/// of the band's depth ([`row_rail_depth`]): the rail thins by its fall, so
/// its crests keep to the band's top and its troughs to its bottom.
const ROW_RAIL_SWELL_SHARE: f32 = 0.5;
/// The depth of a rail laid in its own row's foot band
/// ([`BedShape::row_rail`]), px.
#[inline]
const fn row_rail_depth(chf: f32) -> f32 {
    (ROW_RAIL_CH * chf).floor().max(1.0)
}
/// How far an in-row rail keeps off a foot's cell past its own pitch, px
/// ([`part_round_feet`]): the ribbons' ([`crate::rainbow_kitty::ribbon::FOOT_CLEAR_PX`]).
const FOOT_CLEAR_PX: f32 = crate::rainbow_kitty::ribbon::FOOT_CLEAR_PX;
/// The run over which an in-row rail eases in beside a foot, px.
const FOOT_EASE_PX: f32 = crate::rainbow_kitty::ribbon::FOOT_EASE_PX;
/// The least pitch an in-row rail is resampled at, px ([`part_round_feet`]).
const ROW_RAIL_PITCH_PX: f32 = 2.0;

/// The share of its light a stroke at window `(x, y)` keeps beside `feet`
/// (cells whose ink may reach under the baseline, window px): none within
/// `clear` px of a foot's cell of its row, easing in to whole over the next
/// `ease` px (the ribbons' `KeepOut::foot_share`).
#[inline]
#[must_use]
pub(crate) const fn foot_share(
    feet: &[(i32, i32, i32, i32)],
    (x, y): (f32, f32),
    clear: f32,
    ease: f32,
) -> f32 {
    let mut share = 1.0f32;
    let inv_ease = 1.0 / ease.max(1.0);
    let mut i = 0;
    while i < feet.len() {
        let (x0, x1, y0, y1) = feet[i];
        i += 1;
        if y < y0 as f32 || y >= y1 as f32 {
            continue;
        }
        let d = (x0 as f32 - x).max(x - x1 as f32) - clear;
        if d <= 0.0 {
            return 0.0;
        }
        let k = (d * inv_ease).min(1.0);
        share = share.min(k * k * (3.0 - 2.0 * k));
    }
    share
}

/// `feet` (window px) with the cell `(row, col)` of `geom` cut out of the
/// run that holds it.
pub(crate) fn carve_cell(feet: &mut Vec<(i32, i32, i32, i32)>, geom: Geom, (row, col): (i32, i32)) {
    let (cw, ch) = (geom.cw as i32, geom.ch as i32);
    let (x0, y0) = (
        i32::from(geom.origin_x) + col * cw,
        i32::from(geom.origin_y) + row * ch,
    );
    let x1 = x0 + cw;
    let mut i = 0;
    while i < feet.len() {
        let (a, b, fy0, fy1) = feet[i];
        if fy0 <= y0 && y0 < fy1 && a < x1 && x0 < b {
            feet.swap_remove(i);
            if a < x0 {
                feet.push((a, x0, fy0, fy1));
            }
            if x1 < b {
                feet.push((x1, b, fy0, fy1));
            }
            continue;
        }
        i += 1;
    }
}

/// `verts`, a rail in its own row's foot band at `y`, resampled every
/// `pitch` px and each sample's light taken down to its share beside the
/// row's `feet` ([`foot_share`]): a beam lays each slab at the light
/// interpolated between its vertices, so the rail's own samples, a few to a
/// cell, would carry its light over a foot's ring.
pub(crate) fn part_round_feet(
    verts: &mut Vec<RibbonVertex>,
    out: &mut Vec<RibbonVertex>,
    feet: &[(i32, i32, i32, i32)],
    y: f32,
    (clear, ease, pitch): (f32, f32, f32),
) {
    out.clear();
    let pitch = pitch.max(1.0);
    let shared = |mut v: RibbonVertex| {
        v.cov *= foot_share(feet, (v.x, y), clear, ease);
        v
    };
    if verts.len() == 1 {
        out.push(shared(verts[0]));
    }
    let mut i = 0;
    while i + 1 < verts.len() {
        let (a, b) = (verts[i], verts[i + 1]);
        let n = ((b.x - a.x).abs() / pitch).ceil().max(1.0) as usize;
        let mut j = usize::from(i > 0);
        while j <= n {
            let t = j as f32 / n as f32;
            let lerp = |u: f32, v: f32| u + (v - u) * t;
            out.push(shared(RibbonVertex {
                x: lerp(a.x, b.x),
                spine: lerp(a.spine, b.spine),
                up: lerp(a.up, b.up),
                dn: lerp(a.dn, b.dn),
                core_up: lerp(a.core_up, b.core_up),
                core_dn: lerp(a.core_dn, b.core_dn),
                color: crate::effect_util::lerp_rgb(a.color, b.color, t),
                cov: lerp(a.cov, b.cov),
                lift: lerp(a.lift, b.lift),
                lift_span: lerp(a.lift_span, b.lift_span),
            }));
            j += 1;
        }
        i += 1;
    }
    std::mem::swap(verts, out);
}

/// An icy tail's colour at its tail ([`icy_rail_ink`]).
pub(crate) const ICY_TAIL: u32 = 0x0046_A6FF;
/// An icy tail's colour at its nucleus ([`icy_rail_ink`]).
pub(crate) const ICY_NUCLEUS: u32 = 0x00E4_F6FF;

/// Fill `out` with whether each sample of `run` wears the air's light
/// ([`AirLight`]): on a lit page (`lit`), a sample whose cell and the cells
/// either side of it hold no text, nor its neighbours' along the run
/// (unless `alone`, a test's control: its own cell's text alone). A sample
/// beside text keeps the page's luminance, so the bed's colour, drawn
/// between samples, turns from the hue to the light in the air and never
/// over a letter: read by its own cell, the sample past a line's last
/// letter lit the bed's ramp under that letter.
#[inline]
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
    let mut ri = 0;
    while ri < run.len() {
        let s = &run[ri];
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
        ri += 1;
    }
    if alone {
        return;
    }
    let mut before = true;
    let len = out.len();
    let mut i = 0;
    while i < len {
        let own = out[i];
        let after = if i + 1 < len { out[i + 1] } else { true };
        out[i] = own && before && after;
        before = own;
        i += 1;
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
        rail_wave: None,
        rail_room: f32::INFINITY,
        rail_air_room: false,
        rail_air_text_only: false,
        row_tail_faint: false,
        nucleus_bar: false,
        foam_even: false,
        foam_head_cut: false,
        foam_bodiless: false,
        body: Vec::new(),
        foam_air: false,
        drops: Vec::new(),
        feet: Vec::new(),
        soles: [None; 2],
        parted: Vec::new(),
        row_rail_off: false,
        row_rail_swell_lifts: false,
        rail_page: None,
        rail_hue_free: false,
    };
}

impl Default for BedScratch {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// An icy rail's opacity at full light ([`BedShape::icy`]): each vertex's
/// light is this times the square root of the plain rail's share (its
/// spark's light times its taper), so it keeps the plain rail's order along
/// the run, its taper, its fall to nothing at its tail and its feather off a
/// head the caret left, and lifts a spark's fading light the way the eye
/// reads a comet's tail. Laid at the plain rail's linear share, Comet's
/// typing tail was as dim as its sparks' age (looks8 round 2's K4).
const ICY_RAIL_ALPHA: f32 = 245.0;

/// How far below its row's bottom a dark page's rail may reach, in cells:
/// the leading above the next row's tallest letters. An icy rail, laid
/// [`lit_rail_drop`] below the row clear of the judge's ring round its
/// letters, takes its depth from what is left ([`BedScratch::rail_room`]).
pub(crate) const DARK_RAIL_LEADING_CH: f32 = 0.2;

/// Swell a rail's spine ([`BedScratch::rail_wave`]): each span between its
/// vertices cut to a vertex every quarter cell, each vertex's spine fallen
/// `depth * (1 + sin(x * k + phase)) / 2` below the row's bottom; with `vary`
/// (foam's, [`FOAM_SWELL_VARY`]) the swell's pitch wanders ([`foam_theta`])
/// and a slow beat lifts and lowers whole crests by up to `vary * depth`.
#[inline]
pub(super) fn swell_rail(
    verts: &mut Vec<RibbonVertex>,
    (depth, k, phase): (f32, f32, f32),
    cw: f32,
    vary: f32,
) {
    #[inline]
    const fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
    // Foam's swell wanders in pitch ([`foam_theta`]) and lifts and lowers
    // whole crests ([`FOAM_SWELL_VARY`]); `vary` 0: an even sine.
    #[inline]
    const fn rail_fall(x: f32, (depth, k, phase): (f32, f32, f32), vary: f32) -> f32 {
        let th = foam_theta(x, k, phase, vary > 0.0);
        let beat_term = if vary > 0.0 {
            let beat = crate::supernova::sin_cos_f32(th * FOAM_SWELL_VARY_RATE + 2.3).0;
            vary * 0.5 * (1.0 + beat)
        } else {
            0.0
        };
        depth * (0.5 * (1.0 + crate::supernova::sin_cos_f32(th).0) + beat_term)
    }
    let orig_len = verts.len();
    if orig_len < 2 || depth <= 0.0 {
        return;
    }
    let wave = (depth, k, phase);
    let step = (cw * 0.25).max(1.0);
    let inv_step = 1.0 / step;
    let mut total_len = 1usize;
    let mut pi = 0;
    while pi + 1 < orig_len {
        let n = (((verts[pi + 1].x - verts[pi].x).abs() * inv_step).ceil() as usize).max(1);
        total_len += n;
        pi += 1;
    }
    let mut b = verts[orig_len - 1];
    verts.resize(total_len, b);
    let mut write_end = total_len - 1;
    verts[write_end] = RibbonVertex {
        spine: b.spine + rail_fall(b.x, wave, vary),
        ..b
    };
    let mut pi = orig_len - 1;
    while pi > 0 {
        pi -= 1;
        let a = verts[pi];
        let n = (((b.x - a.x).abs() * inv_step).ceil() as usize).max(1);
        write_end -= n;
        let inv_n = 1.0 / n as f32;
        let mut j = 0;
        while j < n {
            let t = j as f32 * inv_n;
            let x = lerp_f32(a.x, b.x, t);
            verts[write_end + j] = RibbonVertex {
                x,
                spine: lerp_f32(a.spine, b.spine, t) + rail_fall(x, wave, vary),
                up: lerp_f32(a.up, b.up, t),
                dn: lerp_f32(a.dn, b.dn, t),
                core_up: lerp_f32(a.core_up, b.core_up, t),
                core_dn: lerp_f32(a.core_dn, b.core_dn, t),
                color: crate::effect_util::lerp_rgb(a.color, b.color, t),
                cov: lerp_f32(a.cov, b.cov, t),
                lift: lerp_f32(a.lift, b.lift, t),
                lift_span: lerp_f32(a.lift_span, b.lift_span, t),
            };
            j += 1;
        }
        b = a;
    }
}

/// Break a swollen rail into foam ([`BedShape::foam`]): each vertex's light
/// kept by how far it lies up from the swell's trough toward its crest,
/// against a break line that wobbles from trough to trough
/// ([`FOAM_BREAK`], [`FOAM_WOBBLE`]); under each open break, a droplet
/// ([`FOAM_DROP`]) in `drops`, below the spine there with the light and ink
/// the rail had there. Unless `even` (looks9 round 1's foam, a test's
/// control) a second break line beats against the first ([`FOAM_WOBBLE_2`]),
/// the troughs are the swollen spine's own ([`foam_theta`]) and some breaks
/// shed nothing and the rest droplets of many sizes ([`FOAM_DROP_SKIP`]);
/// unless `head_cut` (a test's control) the crest feathers in to the rail's
/// last vertex, the caret ([`FOAM_HEAD_FEATHER_CELLS`]).
#[inline]
pub(super) fn foam_rail(
    verts: &mut [RibbonVertex],
    (depth, k, phase): (f32, f32, f32),
    (ch, cw): (f32, f32),
    (even, head_cut): (bool, bool),
    drops: &mut Vec<(f32, f32, f32, f32, u32)>,
) {
    drops.clear();
    if verts.len() < 2 || depth <= 0.0 || k <= 0.0 {
        return;
    }
    let second = if even { 0.0 } else { FOAM_WOBBLE_2.0 };
    const INV_BREAK_SPAN: f32 = 1.0 / (FOAM_BREAK.1 - FOAM_BREAK.0);
    const INV_DROP_SPAN: f32 = 1.0 / (1.0 - FOAM_DROP_SKIP);
    let keep = |x: f32| {
        let th = foam_theta(x, k, phase, !even);
        let up = 0.5 * (1.0 - th.sin());
        let mut line = up + FOAM_WOBBLE.0 * (th * FOAM_WOBBLE.1 + 1.7).sin();
        if second != 0.0 {
            line += second * (th * FOAM_WOBBLE_2.1 + 0.4).sin();
        }
        let t = ((line - FOAM_BREAK.0) * INV_BREAK_SPAN).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let tau = std::f32::consts::TAU;
    let feather_head = !head_cut && !verts.is_empty();
    let (head, inv_span) = if feather_head {
        (
            verts[verts.len() - 1].x,
            1.0 / (FOAM_HEAD_FEATHER_CELLS * cw),
        )
    } else {
        (0.0, 0.0)
    };
    if !even {
        // The troughs: the swollen spine's deepest vertices (its pitch
        // wanders, [`foam_theta`]), fused with the per-vertex coverage taper
        // so [`keep`] evaluates at most once per vertex.
        let mut n = 0.0f32;
        let len = verts.len();
        let mut i = 0;
        while i < len {
            let is_trough = i > 0
                && i + 1 < len
                && verts[i].spine > verts[i - 1].spine
                && verts[i].spine >= verts[i + 1].spine;
            let v = &mut verts[i];
            i += 1;
            let orig_cov = v.cov;
            if orig_cov <= 0.0 {
                if is_trough {
                    n += 1.0;
                }
                continue;
            }
            let u = if feather_head {
                ((head - v.x).abs() * inv_span).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if !is_trough && u <= 0.0 {
                v.cov = 0.0;
                continue;
            }
            let k_cov = keep(v.x);
            if is_trough {
                n += 1.0;
                if k_cov < 0.5 {
                    let grain = (n * 0.414_214 + 0.17).rem_euclid(1.0);
                    if grain >= FOAM_DROP_SKIP {
                        let walk = (n * 0.618_034).rem_euclid(1.0);
                        let drop_u = (grain - FOAM_DROP_SKIP) * INV_DROP_SPAN;
                        let size =
                            FOAM_DROP_SIZES.0 + (FOAM_DROP_SIZES.1 - FOAM_DROP_SIZES.0) * drop_u;
                        drops.push((
                            v.x,
                            v.spine + (FOAM_DROP.1 * (1.0 + 0.5 * walk) + FOAM_DROP.0 * size) * ch,
                            size,
                            orig_cov * FOAM_DROP.2,
                            v.color,
                        ));
                    }
                }
            }
            if feather_head {
                if u > 0.0 {
                    v.cov *= k_cov;
                    v.cov *= u * u * (3.0 - 2.0 * u);
                } else {
                    v.cov = 0.0;
                }
            } else {
                v.cov *= k_cov;
            }
        }
    } else {
        let mut i = 0;
        while i + 1 < verts.len() {
            let (a, b) = (verts[i], verts[i + 1]);
            i += 1;
            let (lo, hi) = if a.x <= b.x { (a.x, b.x) } else { (b.x, a.x) };
            if hi - lo <= f32::EPSILON {
                continue;
            }
            // The troughs (`sin = 1`) in `[lo, hi)`.
            let first = ((lo * k + phase - std::f32::consts::FRAC_PI_2) / tau).ceil();
            let mut n = first;
            loop {
                let x = (std::f32::consts::FRAC_PI_2 + n * tau - phase) / k;
                if x >= hi {
                    break;
                }
                n += 1.0;
                if x < lo {
                    continue;
                }
                let t = (x - a.x) / (b.x - a.x);
                let cov = a.cov + (b.cov - a.cov) * t;
                if cov <= 0.0 || keep(x) >= 0.5 {
                    continue;
                }
                let spine = a.spine + (b.spine - a.spine) * t;
                let walk = ((n - 1.0) * 0.618_034).rem_euclid(1.0);
                drops.push((
                    x,
                    spine + (FOAM_DROP.1 * (1.0 + 0.5 * walk) + FOAM_DROP.0) * ch,
                    1.0,
                    cov * FOAM_DROP.2,
                    crate::effect_util::lerp_rgb(a.color, b.color, t),
                ));
            }
        }
        // The crest feathers in to the caret, its last arc no longer cut flat at
        // the caret's edge.
        if feather_head {
            let mut i = 0;
            while i < verts.len() {
                let v = &mut verts[i];
                if v.cov > 0.0 {
                    let u = ((head - v.x).abs() * inv_span).clamp(0.0, 1.0);
                    if u > 0.0 {
                        v.cov *= keep(v.x);
                        v.cov *= u * u * (3.0 - 2.0 * u);
                    } else {
                        v.cov = 0.0;
                    }
                }
                i += 1;
            }
        } else {
            let mut i = 0;
            while i < verts.len() {
                let v = &mut verts[i];
                if v.cov > 0.0 {
                    v.cov *= keep(v.x);
                }
                i += 1;
            }
        }
    }
}

/// Lay the dark bed under `runs` into `under` in `scratch.inks` ([`dark_bed_ink`]).
#[inline]
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
    under: &mut RampSink<'_>,
) {
    let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
    let (inv_cw, inv_ch) = (1.0 / cwf, 1.0 / chf);
    let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let clip = geom.beam_clip();
    let stride = (geom.cw / 8).max(1);
    let quad_budget = under.laid().saturating_add(CursorGlow::MAX_QUADS);
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
        rail_wave,
        rail_room,
        rail_air_room,
        rail_air_text_only,
        row_tail_faint,
        nucleus_bar,
        foam_even,
        foam_head_cut,
        foam_bodiless,
        body,
        foam_air,
        drops,
        feet,
        soles,
        parted,
        row_rail_off,
        row_rail_swell_lifts,
        rail_page,
        rail_hue_free,
    } = scratch;
    let (rail_wave, rail_room, rail_air_room) = (*rail_wave, *rail_room, *rail_air_room);
    let rail_air_text_only = *rail_air_text_only;
    let (row_tail_faint, nucleus_bar) = (*row_tail_faint, *nucleus_bar);
    let (foam_even, foam_head_cut, foam_bodiless) = (*foam_even, *foam_head_cut, *foam_bodiless);
    let (caret, rail_drop) = (*caret, *rail_drop);
    let (row_rail_off, row_rail_swell_lifts) = (*row_rail_off, *row_rail_swell_lifts);
    let (rail_page, rail_hue_free) = (*rail_page, *rail_hue_free);
    #[inline]
    const fn bed_cell_of(
        x: f32,
        y: f32,
        oxf: f32,
        oyf: f32,
        inv_cw: f32,
        inv_ch: f32,
    ) -> (i32, i32) {
        (
            ((y - oyf) * inv_ch).floor() as i32,
            ((x - oxf) * inv_cw).floor() as i32,
        )
    }
    #[inline]
    const fn smooth(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
    #[inline]
    const fn run_taper_at(
        one_row: bool,
        tail_x: f32,
        last_x: f32,
        dir: f32,
        inv_tail_w: f32,
        inv_wedge_w: f32,
        x: f32,
    ) -> f32 {
        if !one_row {
            return 1.0;
        }
        let tail = smooth((x - tail_x).abs() * inv_tail_w);
        let wedge = if inv_wedge_w > 0.0 {
            let behind = ((last_x - x) * dir).max(0.0);
            (1.0 - behind * inv_wedge_w).clamp(0.0, 1.0)
        } else {
            1.0
        };
        tail * wedge
    }
    #[inline]
    const fn bed_ribbon_vertex(
        shape: &BedShape,
        chf: f32,
        dy: f32,
        x: f32,
        s: &CometSample,
        rgb: u32,
        k: f32,
    ) -> RibbonVertex {
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
    }
    #[inline]
    const fn rail_depth(shape: &BedShape, reach: f32, k: f32) -> (f32, f32) {
        let dn = reach * (1.0 - shape.rail_taper * (1.0 - k.clamp(0.0, 1.0)));
        (dn, dn * shape.rail_core * (1.0 - shape.rail_rise))
    }
    #[inline]
    const fn rail_leading(text: &TextView<'_>, row: i32, oxf: f32, inv_cw: f32, x: f32) -> bool {
        !text.over_text(row + 1, ((x - oxf) * inv_cw).floor() as i32)
    }
    #[inline]
    const fn bed_rail_vertex(
        shape: &BedShape,
        (chf, bottom, rail_up, reach, row, oxf, inv_cw, in_row, whole): (
            f32,
            f32,
            f32,
            f32,
            i32,
            f32,
            f32,
            bool,
            bool,
        ),
        text: &TextView<'_>,
        x: f32,
        s: &CometSample,
        rgb: u32,
        k: f32,
    ) -> RibbonVertex {
        let (dn, core_dn) = rail_depth(shape, reach, k);
        // An icy tail in its own row's foot band is laid whole down to the
        // band's last pixel row, thinning with its taper, and falls off with
        // its taper's square root ([`BedScratch::row_tail_faint`]): hung
        // from the band's top, its profile melted out before the band's last
        // pixel row.
        let (spine, up, dn, core_up, core_dn, k) = if whole {
            // Hung from the band's last pixel row: its light whole on that
            // row and the one above, melting to the band's top, which it
            // reaches only by the caret, and nothing past either edge.
            let up = (reach - 1.0).max(0.0) * (0.5 + 0.5 * k.clamp(0.0, 1.0));
            (
                bottom + reach - 1.0,
                up,
                1.0,
                up * aterm_render::RIBBON_CORE_SHARE,
                0.5,
                crate::color_math::sqrt_f32(k.clamp(0.0, 1.0)),
            )
        } else {
            (bottom, rail_up, dn, 0.0, core_dn, k)
        };
        let lit = if shape.icy {
            let share = (s.cov as f32 / shape.full).clamp(0.0, 1.0) * k.clamp(0.0, 1.0);
            ICY_RAIL_ALPHA * crate::color_math::sqrt_f32(share)
        } else {
            shape.at(s.cov, chf, k).0
        };
        RibbonVertex {
            x,
            spine,
            up,
            dn,
            core_up,
            core_dn,
            color: rgb,
            // In its own row's foot band the rail parts round the row's
            // feet ([`part_round_feet`]), not the row below's letters.
            cov: if in_row || rail_leading(text, row, oxf, inv_cw, x) {
                lit
            } else {
                0.0
            },
            lift: 0.0,
            lift_span: 0.0,
        }
    }
    #[inline]
    const fn sample_over_text(
        shape: &BedShape,
        text: &TextView<'_>,
        (oxf, oyf, inv_cw, inv_ch): (f32, f32, f32, f32),
        s: &CometSample,
    ) -> bool {
        let (row, col) = bed_cell_of(s.x, s.y, oxf, oyf, inv_cw, inv_ch);
        shape.leap_air || text.over_text(row, col)
    }
    #[inline]
    const fn leap_beam_vertex(
        shape: &BedShape,
        chf: f32,
        dy: f32,
        s: &CometSample,
        rgb: u32,
        x: f32,
        y: f32,
    ) -> BeamVertex {
        BeamVertex {
            x,
            y: y + dy,
            color: rgb,
            cov: shape.at(s.cov, chf, 1.0).0 as u8,
        }
    }
    #[inline]
    const fn leap_tube_at(a: BeamVertex, b: BeamVertex, k: f32, cov: u8) -> BeamVertex {
        BeamVertex {
            x: a.x + (b.x - a.x) * k,
            y: a.y + (b.y - a.y) * k,
            color: if k < 0.5 { a.color } else { b.color },
            cov,
        }
    }
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
    #[inline]
    fn feather_over(
        verts: &mut Vec<RibbonVertex>,
        (from, dir, feather_span, taper_from): (f32, f32, f32, f32),
        at: impl Fn(f32, f32) -> RibbonVertex,
    ) {
        while verts.len() > 1 && matches!(verts.last(), Some(v) if (v.x - from) * dir >= 0.0) {
            verts.pop();
        }
        let mut j = 0;
        while j < HEAD_FEATHER_KNOTS.len() {
            let (u, w) = HEAD_FEATHER_KNOTS[j];
            verts.push(at(from + feather_span * u, taper_from * w));
            j += 1;
        }
    }
    let cell_params = (oxf, oyf, inv_cw, inv_ch);
    let mut at = 0usize;
    let mut index = 0;
    while index < runs.len() {
        let run = &runs[index];
        if run.is_empty() {
            index += 1;
            continue;
        }
        let wedge_cells = match wedges.get(index) {
            Some(&w) => w,
            None => shape.wedge,
        };
        index += 1;
        let ink = &inks[at..at + run.len()];
        let rail = rails.get(at..at + run.len());
        at += run.len();
        let mut one_row = true;
        let y0 = run[0].y;
        let mut si = 1;
        while si < run.len() {
            if run[si].y != y0 {
                one_row = false;
                break;
            }
            si += 1;
        }
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
        taper.clear();
        taper.reserve(run.len());
        let mut ti = 0;
        while ti < run.len() {
            taper.push(run_taper_at(
                one_row,
                tail_x,
                last.x,
                dir,
                inv_tail_w,
                inv_wedge_w,
                run[ti].x,
            ));
            ti += 1;
        }
        if ledger.is_on() {
            let mut prev: Option<((i32, i32), u32)> = None;
            let mut li = 0;
            while li < run.len() {
                let s = &run[li];
                let rgb = ink[li];
                let k = taper[li];
                li += 1;
                let a = shape.at(s.cov, chf, k).0 as u8;
                let composite = aterm_render::over_premul(page, premul_rgb(rgb, a), a);
                let here = bed_cell_of(s.x, s.y, oxf, oyf, inv_cw, inv_ch);
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
            verts.clear();
            verts.push(bed_ribbon_vertex(
                &shape, chf, dy, tail_x, &first, ink[0], 0.0,
            ));
            let mut vi = 0;
            while vi < run.len() {
                let s = &run[vi];
                verts.push(bed_ribbon_vertex(
                    &shape, chf, dy, s.x, s, ink[vi], taper[vi],
                ));
                vi += 1;
            }
            let (head_row, head_col) = bed_cell_of(last.x, last.y, oxf, oyf, inv_cw, inv_ch);
            let air_head = run.len() > 1 && !text.over_text(head_row, head_col);
            if air_head && let Some(v) = verts.last_mut() {
                let (alpha, _) = shape.at(last.cov, chf, taper[taper.len() - 1] * 0.5);
                v.cov = alpha;
            }
            let head_taper = run_taper_at(
                one_row,
                tail_x,
                last.x,
                dir,
                inv_tail_w,
                inv_wedge_w,
                head_x,
            );
            let head_k = if air_head { 0.0 } else { head_taper };
            verts.push(bed_ribbon_vertex(
                &shape,
                chf,
                dy,
                head_x,
                &last,
                ink[ink.len() - 1],
                head_k,
            ));
            let left = matches!(caret, Some(c) if c != (head_row, head_col)) && head_k > 0.0;
            // The feather's span is read by the ribbon's feather (`left`) and
            // by the icy rail's under a round head (`round_head` below).
            let feather_geom = if left || shape.icy {
                let feather_span = dir * cwf * DARK_BED_HEAD_FEATHER_CELLS;
                let from = head_x - feather_span;
                let taper_from =
                    run_taper_at(one_row, tail_x, last.x, dir, inv_tail_w, inv_wedge_w, from);
                (from, dir, feather_span, taper_from)
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };
            if left {
                let last_ink = ink[ink.len() - 1];
                feather_over(verts, feather_geom, |x, k| {
                    bed_ribbon_vertex(&shape, chf, dy, x, &last, last_ink, k)
                });
            }
            if shape.dim > 0.0 {
                ribbon_beam_into(
                    under,
                    clip,
                    verts,
                    1.0,
                    stride,
                    quad_budget,
                    GlowBlend::Over,
                );
            }
            let row = bed_cell_of(first.x, first.y, oxf, oyf, inv_cw, inv_ch).0;
            let under_tail = {
                let (x0, x1) = (tail_x.min(head_x), tail_x.max(head_x));
                (
                    ((x0 - oxf) * inv_cw).floor() as i32 - 1,
                    ((x1 - oxf) * inv_cw).floor() as i32 + 1,
                )
            };
            // The row under the tail is air: bare of letters and, under the
            // tail and a cell past either end, on the page's own ground.
            let air_below = row + 1 < geom.rows as i32
                && text.knows_row(row + 1)
                && text.row_is_blank(row + 1)
                && (rail_air_text_only || text.grounds_read_as_page(row + 1, under_tail, page));
            // Over anything else a rail that may lie in its own row does,
            // on a row whose feet the host named ([`BedShape::row_rail`]).
            let row_y0 = i32::from(geom.origin_y) + row * geom.ch as i32;
            let in_row = shape.row_rail
                && !row_rail_off
                && !air_below
                && (matches!(soles[0], Some((_, _, y0, _)) if y0 == row_y0)
                    || matches!(soles[1], Some((_, _, y0, _)) if y0 == row_y0));
            if let Some(rail) = rail
                && shape.rail > 0.0
                && (row + 1 < geom.rows as i32 || in_row)
                && (!shape.rail_caret_row || matches!(caret, Some((r, _)) if r == row))
            {
                let soft = if in_row { 0.0 } else { shape.rail_soft * chf };
                let row_rail = row_rail_depth(chf);
                // In its row a swollen rail (Water's foam) swells within the
                // foot band: its deepest fall held to a share of the band's
                // depth and the rail thinned by it, so its crests keep to the
                // band's top and no trough sinks into the filled row below.
                // Lifted by its fall over the band's top, its crests lay on
                // the letters' feet: on Solarized Dark the `o` of `fox` read
                // 1.39:1 against its own 4.28 (looks10 round 2, `context`).
                let swell_vary = if shape.foam > 0.0 && !foam_even {
                    FOAM_SWELL_VARY
                } else {
                    0.0
                };
                let rail_wave = match rail_wave {
                    Some((depth, k, phase)) if in_row => {
                        let most = if row_rail_swell_lifts {
                            row_rail * ROW_RAIL_SWELL_SHARE
                        } else {
                            row_rail * ROW_RAIL_SWELL_SHARE / (1.0 + swell_vary)
                        };
                        Some((depth.min(most), k, phase))
                    }
                    wave => wave,
                };
                let swell_fall = match rail_wave {
                    Some((depth, _, _)) if in_row => depth * (1.0 + swell_vary),
                    _ => 0.0,
                };
                let bottom = if in_row {
                    oyf + (row + 1) as f32 * chf
                        - row_rail
                        - if row_rail_swell_lifts {
                            swell_fall
                        } else {
                            0.0
                        }
                } else {
                    oyf + (row + 1) as f32 * chf + rail_drop + soft
                };
                // Over air the tail takes its whole depth
                // ([`BedScratch::rail_air_room`]). Over a selection or a
                // powerline bar bare of letters, read by its letters alone,
                // the tail spilled its whole depth into the fill.
                let reach = if in_row {
                    if row_rail_swell_lifts {
                        row_rail
                    } else {
                        (row_rail - swell_fall).max(1.0)
                    }
                } else if rail_air_room
                    && text.knows_row(row + 1)
                    && text.row_is_blank(row + 1)
                    && (rail_air_text_only || text.grounds_read_as_page(row + 1, under_tail, page))
                {
                    shape.rail * chf * ICY_AIR_RAIL_GROW
                } else {
                    (shape.rail * chf).min(rail_room.max(1.0))
                };
                let rail_rise = if in_row { 0.0 } else { reach * shape.rail_rise };
                let rail_up = rail_rise + soft;
                let whole = in_row && shape.icy && !row_tail_faint;
                let rail_params = (chf, bottom, rail_up, reach, row, oxf, inv_cw, in_row, whole);
                verts.clear();
                verts.push(bed_rail_vertex(
                    &shape,
                    rail_params,
                    text,
                    tail_x,
                    &first,
                    rail[0],
                    0.0,
                ));
                let mut rvi = 0;
                while rvi < run.len() {
                    let s = &run[rvi];
                    verts.push(bed_rail_vertex(
                        &shape,
                        rail_params,
                        text,
                        s.x,
                        s,
                        rail[rvi],
                        taper[rvi],
                    ));
                    rvi += 1;
                }
                verts.push(bed_rail_vertex(
                    &shape,
                    rail_params,
                    text,
                    head_x,
                    &last,
                    rail[rail.len() - 1],
                    head_taper,
                ));
                // Under the caret Comet's nucleus is the tail's round head
                // ([`CursorGlow::push_comet_nucleus`], on the same terms): the
                // tail feathers out across the caret's cell
                // ([`BedScratch::nucleus_bar`]).
                let round_head = shape.icy
                    && !shape.pinned
                    && !in_row
                    && !nucleus_bar
                    && matches!(caret, Some(c) if c == (head_row, head_col))
                    && row + 1 < geom.rows as i32
                    && !text.over_text(row + 1, head_col - 1)
                    && !text.over_text(row + 1, head_col)
                    && !text.over_text(row + 1, head_col + 1);
                if left || round_head {
                    let last_rail = rail[rail.len() - 1];
                    feather_over(verts, feather_geom, |x, k| {
                        bed_rail_vertex(&shape, rail_params, text, x, &last, last_rail, k)
                    });
                }
                if in_row {
                    let pitch = (stride as f32).max(ROW_RAIL_PITCH_PX);
                    part_round_feet(
                        verts,
                        parted,
                        feet,
                        bottom,
                        (FOOT_CLEAR_PX + pitch, FOOT_EASE_PX, pitch),
                    );
                }
                drops.clear();
                if let Some(wave) = rail_wave {
                    let vary = if shape.foam > 0.0 && !foam_even {
                        FOAM_SWELL_VARY
                    } else {
                        0.0
                    };
                    swell_rail(verts, wave, cwf, vary);
                    if shape.foam > 0.0 && !foam_bodiless {
                        // Over air the water's body takes its whole depth;
                        // over letters or a filled row it keeps to the rail's.
                        let deep = if *foam_air { FOAM_BODY.1 * chf } else { reach };
                        foam_body(verts, body, (deep, cwf), foam_head_cut);
                    }
                    if shape.foam > 0.0 {
                        foam_rail(verts, wave, (chf, cwf), (foam_even, foam_head_cut), drops);
                        // Over its body of water the foam sheds no droplet
                        // between its crests: a bead there at the crests'
                        // height was the dot of a dash-dot rule. In its row
                        // it sheds none into the filled row below.
                        if body.len() >= 2 || in_row {
                            drops.clear();
                        }
                    }
                }
                let from = under.mark();
                if body.len() >= 2 {
                    ribbon_beam_into(under, clip, body, 1.0, stride, quad_budget, GlowBlend::Over);
                    body.clear();
                }
                match rail_page {
                    Some(page) => {
                        let mut matched = PageMatched {
                            out: &mut *under,
                            page,
                            hue_free: rail_hue_free,
                        };
                        ribbon_beam_into(
                            &mut matched,
                            clip,
                            verts,
                            1.0,
                            stride,
                            quad_budget,
                            GlowBlend::Over,
                        );
                    }
                    None => {
                        ribbon_beam_into(
                            under,
                            clip,
                            verts,
                            1.0,
                            stride,
                            quad_budget,
                            GlowBlend::Over,
                        );
                    }
                }
                // A droplet keeps the judge's ring clear of the next row's
                // letters as the rail keeps their cells.
                const DROP_KNOTS: [(f32, f32, f32); 5] = [
                    (-1.0, 0.0, 0.35),
                    (-0.5, 0.8, crate::color_math::sqrt_f32(0.75)),
                    (0.0, 1.0, 1.0),
                    (0.5, 0.8, crate::color_math::sqrt_f32(0.75)),
                    (1.0, 0.0, 0.35),
                ];
                let r_drop = (FOAM_DROP.0 * chf).max(1.0);
                let mut di = 0;
                while di < drops.len() {
                    let (x, y, size, cov, rgb) = drops[di];
                    di += 1;
                    let r = (r_drop * size).max(1.0);
                    let clear = r + crate::cursor_glow::LETTER_GUARD_RING as f32;
                    if !rail_leading(text, row, oxf, inv_cw, x - clear)
                        || !rail_leading(text, row, oxf, inv_cw, x + clear)
                    {
                        continue;
                    }
                    verts.clear();
                    let mut ki = 0;
                    while ki < DROP_KNOTS.len() {
                        let (dx, w, half_share) = DROP_KNOTS[ki];
                        ki += 1;
                        let half = r * half_share;
                        verts.push(RibbonVertex {
                            x: x + dx * r,
                            spine: y,
                            up: half,
                            dn: half,
                            core_up: half * 0.35,
                            core_dn: half * 0.35,
                            color: rgb,
                            cov: cov * w,
                            lift: 0.0,
                            lift_span: 0.0,
                        });
                    }
                    ribbon_beam_into(
                        under,
                        clip,
                        verts,
                        1.0,
                        stride,
                        quad_budget,
                        GlowBlend::Over,
                    );
                }
                if in_row {
                    ledger.note_foot_rail_laid(from, under.mark());
                } else if rail_rise <= 0.0
                    && rail_drop >= crate::cursor_glow::LETTER_GUARD_RING as f32
                {
                    ledger.note_rail_laid(from, under.mark());
                }
            }
        } else {
            let mut i = 0;
            while i < run.len() {
                if !sample_over_text(&shape, text, cell_params, &run[i]) {
                    i += 1;
                    continue;
                }
                let start = i;
                i += 1;
                loop {
                    while i < run.len() && sample_over_text(&shape, text, cell_params, &run[i]) {
                        i += 1;
                    }
                    let mut j = i;
                    while j < run.len()
                        && !sample_over_text(&shape, text, cell_params, &run[j])
                        && j - i < LEAP_GAP_CELLS
                    {
                        j += 1;
                    }
                    if j > i
                        && j < run.len()
                        && sample_over_text(&shape, text, cell_params, &run[j])
                    {
                        i = j + 1;
                        continue;
                    }
                    break;
                }
                let stretch = &run[start..i];
                let mut reach_sum = 0.0f32;
                let mut sti = 0;
                while sti < stretch.len() {
                    reach_sum += shape.at(stretch[sti].cov, chf, 1.0).1;
                    sti += 1;
                }
                let reach = reach_sum / stretch.len() as f32;
                tube.clear();
                if start > 0 {
                    let (a, b) = (run[start - 1], run[start]);
                    tube.push(leap_beam_vertex(
                        &shape,
                        chf,
                        dy,
                        &b,
                        ink[start],
                        0.5 * (a.x + b.x),
                        0.5 * (a.y + b.y),
                    ));
                }
                let ink_stretch = &ink[start..i];
                let mut sti = 0;
                while sti < stretch.len() {
                    let s = &stretch[sti];
                    tube.push(leap_beam_vertex(
                        &shape,
                        chf,
                        dy,
                        s,
                        ink_stretch[sti],
                        s.x,
                        s.y,
                    ));
                    sti += 1;
                }
                if i < run.len() {
                    let (a, b) = (run[i - 1], run[i]);
                    tube.push(leap_beam_vertex(
                        &shape,
                        chf,
                        dy,
                        &a,
                        ink[i - 1],
                        0.5 * (a.x + b.x),
                        0.5 * (a.y + b.y),
                    ));
                }
                if tube.len() >= 2 && shape.dim > 0.0 {
                    let (a, b) = (tube[0], tube[tube.len() - 1]);
                    let len = crate::color_math::hypot_f32(b.x - a.x, b.y - a.y);
                    let f = (cwf * 0.5 / len.max(1.0)).min(0.3);
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
                    tube.extend([
                        leap_tube_at(a, b, 0.0, 0),
                        leap_tube_at(a, b, f, ca),
                        leap_tube_at(a, b, 1.0 - f, cb),
                        leap_tube_at(a, b, 1.0, 0),
                    ]);
                    let mut k = 0;
                    while k < LEAP_LAYERS {
                        let shrink = 1.0 - k as f32 * (1.0 / LEAP_LAYERS as f32);
                        comet_beam_over(
                            &mut *under,
                            clip,
                            tube,
                            2.0 * reach * LEAP_SPREAD * shrink,
                            1,
                            0.0,
                        );
                        k += 1;
                    }
                }
            }
        }
    }
    under.keep_laid(quad_budget);
}

/// How many cells back a jump's wedge reaches from its `head` on its row, and which way it
/// points (`1.0`: toward higher columns, from `back`'s guess): over the piece it laid from
/// `tail`, and over the live jump's whole laid path around the head (`leaps`,
/// [`CursorGlow::gather_leap_spans`]), its faded tail cells included: the path Trail length cut.
#[inline]
pub(super) const fn leap_wedge(
    leaps: &[(Instant, u16, u16, u16)],
    geom: Geom,
    head: CometSample,
    tail: CometSample,
    back: f32,
) -> (f32, f32) {
    let (inv_cw, inv_ch) = (1.0 / geom.cw as f32, 1.0 / geom.ch as f32);
    let mut cells = (head.x - tail.x).abs() * inv_cw + 1.0;
    let mut back = back;
    let row = ((head.y - geom.origin_y as f32) * inv_ch)
        .floor()
        .clamp(0.0, u16::MAX as f32) as u16;
    let col = ((head.x - geom.origin_x as f32) * inv_cw).floor();
    let mut found_leap = None;
    let mut li = 0;
    while li < leaps.len() {
        let (_, r, first, last) = leaps[li];
        if r == row && col >= first as f32 - 1.0 && col <= last as f32 + 1.0 {
            found_leap = Some((first, last));
            break;
        }
        li += 1;
    }
    if let Some((first, last)) = found_leap {
        let (first, last) = (first as f32, last as f32);
        if last - col > col - first {
            back = 1.0;
        }
        let far = if back > 0.0 { last } else { first };
        cells = cells.max((col - far).abs() + 1.0);
    }
    (cells, back)
}

/// A wedge's runs ([`BedShape::wedge`]) into the pool `out`, returning how
/// many are live.
#[inline]
fn wedge_runs(
    runs: &[Vec<CometSample>],
    typing: &[Vec<bool>],
    leaps: &[(Instant, u16, u16, u16)],
    geom: Geom,
    shape: BedShape,
    out: &mut Vec<Vec<CometSample>>,
    lengths: &mut Vec<f32>,
) -> usize {
    #[inline]
    fn next_wedge_slot<'a>(
        out: &'a mut Vec<Vec<CometSample>>,
        n: &mut usize,
    ) -> &'a mut Vec<CometSample> {
        let idx = *n;
        if idx == out.len() {
            out.push(Vec::new());
        }
        *n = idx + 1;
        let dst = &mut out[idx];
        dst.clear();
        dst
    }
    #[inline]
    const fn is_typed_sample(leap_path: bool, kinds: &[bool], i: usize) -> bool {
        !leap_path || (if i < kinds.len() { kinds[i] } else { true })
    }
    lengths.clear();
    let cwf = geom.cw as f32;
    let wedge = shape.wedge;
    let knots = (wedge * 4.0).ceil().max(1.0) as usize;
    let mut n = 0usize;
    let mut index = 0;
    while index < runs.len() {
        let run = &runs[index];
        let Some(&head) = run.last() else {
            index += 1;
            continue;
        };
        let mut same_row = true;
        let mut si = 0;
        while si + 1 < run.len() {
            if run[si].y != head.y {
                same_row = false;
                break;
            }
            si += 1;
        }
        if !same_row {
            next_wedge_slot(out, &mut n).extend_from_slice(run);
            lengths.push(wedge);
            index += 1;
            continue;
        }
        let kinds = match typing.get(index) {
            Some(v) => v.as_slice(),
            None => &[],
        };
        index += 1;
        let mut start = 0;
        while start < run.len() {
            let kind = is_typed_sample(shape.leap_path, kinds, start);
            let mut end = start + 1;
            while end < run.len() && is_typed_sample(shape.leap_path, kinds, end) == kind {
                end += 1;
            }
            let piece = &run[start..end];
            let head = piece[piece.len() - 1];
            let tail = piece[0];
            let back = if piece.len() > 1 && tail.x > head.x {
                1.0
            } else {
                -1.0
            };
            let (cells, back) = if kind {
                (wedge, back)
            } else {
                leap_wedge(leaps, geom, head, tail, back)
            };
            let knots = if kind {
                knots
            } else {
                (cells * 4.0).ceil().max(1.0) as usize
            };
            let mut cov = head.cov;
            let mut pi = 0;
            while pi < piece.len() {
                cov = cov.max(piece[pi].cov);
                pi += 1;
            }
            lengths.push(cells);
            let span = cells * cwf;
            let inv_knots = 1.0 / knots as f32;
            let dst = next_wedge_slot(out, &mut n);
            dst.reserve(knots + 1);
            let mut k = knots;
            loop {
                let u = k as f32 * inv_knots;
                dst.push(CometSample {
                    x: head.x + back * (span * u),
                    y: head.y,
                    cov,
                    pos: 1.0 - u,
                });
                if k == 0 {
                    break;
                }
                k -= 1;
            }
            start = end;
        }
    }
    n
}

/// Clip the quads `out[from..]` to `(x0, y0, x1, y1)`, dropping any wholly outside.
#[inline]
pub(crate) fn clip_quads_to(out: &mut Vec<GlowQuad>, from: usize, rect: (i32, i32, i32, i32)) {
    let mut kept = from;
    let mut i = from;
    while i < out.len() {
        if let Some(c) = clip_quad(out[i], rect) {
            out[kept] = c;
            kept += 1;
        }
        i += 1;
    }
    out.truncate(kept);
}

/// [`clip_quads_to`] over a stream's lights laid since `from` (its
/// [`BeamRows::mark`] then), its ramps with them: each ramp row is clipped
/// as the quad it stands for (a run's rows as its one quad), and a light
/// the clip leaves dark keeps its count as a dark quad at the stream's end,
/// as the legacy quad stays.
#[inline]
pub(crate) fn clip_stream_to(
    out: &mut RampSink<'_>,
    from: (usize, aterm_render::RampMark),
    rect: (i32, i32, i32, i32),
) {
    let (x0, y0, x1, y1) = rect;
    let mut darkened = 0usize;
    let (quads, ramps) = out.stream();
    if ramps.items.len() <= from.1.items() {
        // Quads alone since `from`: the legacy clip.
        clip_quads_to(quads, from.0, rect);
        return;
    }
    ramps.retain_since(
        quads,
        from,
        |q| match clip_quad(*q, rect) {
            Some(c) => {
                *q = c;
                true
            }
            None => false,
        },
        |r, ends| {
            let (rx0, ry0) = (i32::from(r.x), i32::from(r.y));
            let (rx1, ry1) = (rx0 + i32::from(r.w), ry0 + i32::from(r.h));
            let (cx0, cx1) = (rx0.max(x0), rx1.min(x1));
            let (cy0, cy1) = (ry0.max(y0), ry1.min(y1));
            if cx0 >= cx1 || cy0 >= cy1 {
                return false;
            }
            r.keep_rows((cy0 - ry0) as u16, (cy1 - cy0) as u16);
            if (cx0, cx1) != (rx0, rx1) {
                let mut k = 0;
                while k < r.h {
                    if !r.continues(k) {
                        let e = r.ink as usize + 2 * usize::from(r.end_row(k));
                        if let Some(q) = r.row_quad(ends, k) {
                            let c = clip_quad(q, rect).expect("a row inside the clip keeps a quad");
                            ends[e] = aterm_render::glow_end(c.color, c.alpha);
                            ends[e + 1] = aterm_render::glow_end(c.color2, c.alpha2);
                            darkened += usize::from(ends[e] | ends[e + 1] == 0);
                        }
                    }
                    k += 1;
                }
            }
            (r.x, r.w) = (cx0 as u16, (cx1 - cx0) as u16);
            true
        },
    );
    let mut d = 0;
    while d < darkened {
        out.quads().push(GlowQuad::default());
        d += 1;
    }
}

/// One quad of [`clip_quads_to`]: `q` within `(x0, y0, x1, y1)`, its ends
/// re-ramped at a horizontal cut; `None` wholly outside.
#[inline]
const fn clip_quad(q: GlowQuad, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> Option<GlowQuad> {
    #[inline]
    const fn lerp_a(a: u8, b: u8, x: i32, qx0: i32, inv_w: f32) -> u8 {
        let t = (x - qx0) as f32 * inv_w;
        (a as f32 + (b as f32 - a as f32) * t).round() as u8
    }
    let (qx0, qy0) = (q.x as i32, q.y as i32);
    let (qx1, qy1) = (qx0 + q.w as i32, qy0 + q.h as i32);
    let cx0 = if qx0 > x0 { qx0 } else { x0 };
    let cx1 = if qx1 < x1 { qx1 } else { x1 };
    let cy0 = if qy0 > y0 { qy0 } else { y0 };
    let cy1 = if qy1 < y1 { qy1 } else { y1 };
    if cx0 >= cx1 || cy0 >= cy1 {
        return None;
    }
    let mut c = q;
    c.x = cx0 as u16;
    c.y = cy0 as u16;
    c.w = (cx1 - cx0) as u16;
    c.h = (cy1 - cy0) as u16;
    if cx0 != qx0 || cx1 != qx1 {
        let w = if q.w > 1 { q.w } else { 1 };
        let inv_w = 1.0 / w as f32;
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
                c.alpha = if q.alpha > 1 { q.alpha } else { 1 };
                c.alpha2 = if q.alpha2 > 1 { q.alpha2 } else { 1 };
            } else {
                let a0 = if cx0 != qx0 {
                    lerp_a(q.alpha, q.alpha2, cx0, qx0, inv_w)
                } else {
                    q.alpha
                };
                let a1 = if cx1 != qx1 {
                    lerp_a(q.alpha, q.alpha2, cx1, qx0, inv_w)
                } else {
                    q.alpha2
                };
                c.alpha = if a0 > 1 { a0 } else { 1 };
                c.alpha2 = if a1 > 1 { a1 } else { 1 };
            }
        }
    }
    Some(c)
}

/// Punch `(x0, y0, x1, y1)` out of every quad of `out` (using `scratch`): a
/// quad that meets it is replaced, in its place in the stream, by its pieces
/// above, left of, right of and below it. Each of the stream's `ramps` (in
/// draw order) stays before the first piece of the quad it stood before, and
/// after every quad where it stood after them. Returns how many quads it cut.
pub(crate) fn punch_quads(
    out: &mut Vec<GlowQuad>,
    ramps: &mut [aterm_render::GlowRamp],
    scratch: &mut Vec<GlowQuad>,
    (x0, y0, x1, y1): (i32, i32, i32, i32),
) -> usize {
    #[inline]
    const fn quad_meets(q: &GlowQuad, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> bool {
        let (qx0, qy0) = (q.x as i32, q.y as i32);
        q.w > 0 && q.h > 0 && qx0 < x1 && qx0 + q.w as i32 > x0 && qy0 < y1 && qy0 + q.h as i32 > y0
    }
    let hole = (x0, y0, x1, y1);
    let mut any_meets = false;
    let mut mi = 0;
    while mi < out.len() {
        if quad_meets(&out[mi], hole) {
            any_meets = true;
            break;
        }
        mi += 1;
    }
    if !any_meets {
        return 0;
    }
    scratch.clear();
    let mut cut = 0;
    let mut next = 0usize;
    let mut i = 0;
    while i < out.len() {
        let q = out[i];
        while next < ramps.len() && ramps[next].at as usize <= i {
            ramps[next].at = scratch.len().min(u32::MAX as usize) as u32;
            next += 1;
        }
        i += 1;
        if !quad_meets(&q, hole) {
            scratch.push(q);
            continue;
        }
        cut += 1;
        let (qx0, qy0) = (i32::from(q.x), i32::from(q.y));
        let (qx1, qy1) = (qx0 + i32::from(q.w), qy0 + i32::from(q.h));
        let (my0, my1) = (qy0.max(y0), qy1.min(y1));
        let pieces = [
            (qx0, qy0, qx1, y0),
            (qx0, my0, x0, my1),
            (x1, my0, qx1, my1),
            (qx0, y1, qx1, qy1),
        ];
        let mut pi = 0;
        while pi < pieces.len() {
            if let Some(p) = clip_quad(q, pieces[pi]) {
                scratch.push(p);
            }
            pi += 1;
        }
    }
    let end_at = scratch.len().min(u32::MAX as usize) as u32;
    while next < ramps.len() {
        ramps[next].at = end_at;
        next += 1;
    }
    std::mem::swap(out, scratch);
    cut
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
    #[inline]
    const fn air_over_text(
        text: &TextView<'_>,
        (oxf, oyf, inv_cw, inv_ch): (f32, f32, f32, f32),
        s: &CometSample,
    ) -> bool {
        let (row, col) = (
            ((s.y - oyf) * inv_ch).floor() as i32,
            ((s.x - oxf) * inv_cw).floor() as i32,
        );
        text.over_text(row, col)
    }
    #[inline]
    const fn air_lit(row_kinds: Option<&[bool]>, i: usize) -> bool {
        match row_kinds {
            Some(kinds) if i < kinds.len() => kinds[i],
            _ => true,
        }
    }
    #[inline]
    const fn sample_is_air(
        run: &[CometSample],
        row_kinds: Option<&[bool]>,
        head: bool,
        text: &TextView<'_>,
        cell_params: (f32, f32, f32, f32),
        i: usize,
    ) -> bool {
        air_lit(row_kinds, i)
            && ((head && i + 1 == run.len()) || !air_over_text(text, cell_params, &run[i]))
    }
    #[inline]
    const fn knee(t: &CometSample, a: &CometSample, half_cw: f32, half_ch: f32) -> CometSample {
        #[inline]
        const fn exit(d: f32, half: f32) -> f32 {
            let ad = d.abs();
            if ad > 1e-3 { half / ad } else { f32::INFINITY }
        }
        let (dx, dy) = (a.x - t.x, a.y - t.y);
        let k = exit(dx, half_cw).min(exit(dy, half_ch)).min(1.0);
        CometSample {
            x: t.x + dx * k,
            y: t.y + dy * k,
            cov: 0,
            pos: a.pos,
        }
    }
    let cell_params = (oxf, oyf, inv_cw, inv_ch);
    let (half_cw, half_ch) = (cwf * 0.5, chf * 0.5);
    let mut n = 0usize;
    let mut ri = 0;
    while ri < runs.len() {
        let run = &runs[ri];
        let head = caret && ri + 1 == runs.len();
        let row_kinds = match typed {
            Some(kinds) => Some(match kinds.get(ri) {
                Some(v) => v.as_slice(),
                None => &[],
            }),
            None => None,
        };
        ri += 1;
        let mut i = 0;
        while i < run.len() {
            if !sample_is_air(run, row_kinds, head, text, cell_params, i) {
                i += 1;
                continue;
            }
            let start = i;
            while i < run.len() && sample_is_air(run, row_kinds, head, text, cell_params, i) {
                i += 1;
            }
            if n == air.len() {
                air.push(Vec::new());
            }
            let out = &mut air[n];
            out.clear();
            if start > 0 {
                out.push(knee(&run[start - 1], &run[start], half_cw, half_ch));
            }
            out.extend_from_slice(&run[start..i]);
            if i < run.len() {
                out.push(knee(&run[i], &run[i - 1], half_cw, half_ch));
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
    #[inline]
    const fn gap_cell_of(
        (oxf, oyf, inv_cw, inv_ch): (f32, f32, f32, f32),
        s: &CometSample,
    ) -> (i32, i32) {
        (
            ((s.y - oyf) * inv_ch).floor() as i32,
            ((s.x - oxf) * inv_cw).floor() as i32,
        )
    }
    #[inline]
    const fn first_samples_on(
        run: &[CometSample],
        (oxf, oyf, inv_cw, inv_ch): (f32, f32, f32, f32),
        row: i32,
        a: i32,
        b: i32,
    ) -> Option<(usize, usize)> {
        let mut ja = None;
        let mut jb = None;
        let mut qi = 0;
        while qi < run.len() {
            let s = &run[qi];
            if ((s.y - oyf) * inv_ch).floor() as i32 == row {
                let col = ((s.x - oxf) * inv_cw).floor() as i32;
                if ja.is_none() && col == a {
                    ja = Some(qi);
                    if jb.is_some() {
                        break;
                    }
                }
                if jb.is_none() && col == b {
                    jb = Some(qi);
                    if ja.is_some() {
                        break;
                    }
                }
            }
            qi += 1;
        }
        match (ja, jb) {
            (Some(ja), Some(jb)) => Some((ja, jb)),
            _ => None,
        }
    }
    let cell_params = (oxf, oyf, inv_cw, inv_ch);
    let reach = BED_WORD_GAP_COLS + 1;
    let mut at = 0usize;
    let mut ri = 0;
    while ri < runs.len() {
        let run = &runs[ri];
        ri += 1;
        let base = at;
        at += run.len();
        if inks.len() < at {
            return;
        }
        let mut prev_cell = (i32::MIN, i32::MIN);
        let mut prev_pair: Option<(usize, usize)> = None;
        let mut i = 0;
        while i < run.len() {
            let s = &run[i];
            let cell = gap_cell_of(cell_params, s);
            let pair = if cell == prev_cell {
                prev_pair
            } else {
                let (row, col) = cell;
                let resolved = if lettered(row, col) {
                    None
                } else {
                    let mut left = None;
                    let mut d = 1;
                    while d <= reach {
                        let c = col - d;
                        if c < 0 {
                            break;
                        }
                        if lettered(row, c) {
                            left = Some(c);
                            break;
                        }
                        d += 1;
                    }
                    if let Some(a) = left {
                        let max_b = (col + reach).min(a + 1 + BED_WORD_GAP_COLS);
                        let mut right = None;
                        let mut c = col + 1;
                        while c <= max_b {
                            if lettered(row, c) {
                                right = Some(c);
                                break;
                            }
                            c += 1;
                        }
                        match right {
                            Some(b) => first_samples_on(run, cell_params, row, a, b),
                            None => None,
                        }
                    } else {
                        None
                    }
                };
                prev_cell = cell;
                prev_pair = resolved;
                resolved
            };
            let Some((ja, jb)) = pair else {
                i += 1;
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
            i += 1;
        }
    }
}

impl CursorGlow {
    /// Each live jump's laid path into [`CursorGlow::leap_spans`], `(born, row, first col, last
    /// col)` with its faded tail cells: the extent a jump's wedge reaches back over, the path
    /// Trail length cut.
    pub(super) fn gather_leap_spans(&mut self) {
        self.leap_spans.clear();
        let sparks = self.sparks.as_slice();
        let mut si = 0;
        while si < sparks.len() {
            let spark = &sparks[si];
            si += 1;
            if spark.typing {
                continue;
            }
            let mut found = false;
            let mut li = 0;
            while li < self.leap_spans.len() {
                let (born, row, first, last) = &mut self.leap_spans[li];
                if *born == spark.born && *row == spark.row {
                    *first = (*first).min(spark.col);
                    *last = (*last).max(spark.col);
                    found = true;
                    break;
                }
                li += 1;
            }
            if !found {
                self.leap_spans
                    .push((spark.born, spark.row, spark.col, spark.col));
            }
        }
    }

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
        color: &impl Fn(&CursorGlow, f32) -> u32,
        caret: Option<(u16, u16)>,
        under: &mut RampSink<'_>,
    ) {
        #[inline]
        const fn caret_i32(caret: Option<(u16, u16)>) -> Option<(i32, i32)> {
            match caret {
                Some((r, c)) => Some((r as i32, c as i32)),
                None => None,
            }
        }
        let (ink, page) = Self::bed_pair(cfg);
        let ink_budget = dark_bed_budget(ink);
        let budget = dim_budget(ink_budget, page, shape.dim);
        let mut scratch = std::mem::take(&mut self.bed_scratch);
        scratch.inks.clear();
        scratch.rails.clear();
        scratch.caret = caret_i32(caret);
        #[cfg(test)]
        let matched = !self.rail_unmatched;
        #[cfg(not(test))]
        let matched = true;
        #[cfg(test)]
        {
            scratch.rail_hue_free = self.rail_hue_free;
        }
        scratch.rail_page = if matched && self.night_grounded(cfg) {
            Some(cfg.theme_bg & 0x00FF_FFFF)
        } else {
            None
        };
        let mut wedges = std::mem::take(&mut self.wedge_runs);
        self.leap_spans.clear();
        if shape.leap_path {
            self.gather_leap_spans();
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
        let air = if self.bed_air_off() {
            None
        } else {
            AirLight::of(page, budget, shape)
        };
        let page_is_lit = page_lit(page, ink_budget);
        let lit_page = page_is_lit && !self.bed_air_off();
        let shape = if lit_page { shape.on_lit_page() } else { shape };
        // A comet's tail is ice on a dark page and in a lit page's air; a
        // lit page with no light in its air (a test's control) lays the
        // plain rail under its letters.
        let shape = BedShape {
            icy: shape.icy && (lit_page || !page_is_lit),
            ..shape
        };
        // A lit page's rail, and a comet's icy tail on any dark page, lie
        // below the judge's ring round the letters' feet, where the guard
        // parts them only round a descender.
        scratch.rail_drop = if lit_page || shape.icy {
            lit_rail_drop(geom.ch as f32)
        } else {
            0.0
        };
        scratch.rail_room = if shape.icy && !lit_page {
            DARK_RAIL_LEADING_CH * geom.ch as f32 - scratch.rail_drop
        } else {
            f32::INFINITY
        };
        #[cfg(test)]
        let hairline = self.comet_tail_hairline;
        #[cfg(not(test))]
        let hairline = false;
        scratch.rail_air_room = shape.icy && !hairline;
        #[cfg(test)]
        {
            scratch.rail_air_text_only = self.comet_tail_text_only;
            scratch.row_tail_faint = self.comet_row_tail_faint;
            scratch.nucleus_bar = self.comet_nucleus_bar;
        }
        scratch.feet.clear();
        scratch.soles = [None; 2];
        if shape.row_rail {
            self.foot_boxes(geom, &mut scratch.feet, &mut scratch.soles);
            // The caret's own cell is no foot: its block covers what lies
            // there, and parted round it the rail stopped short of the head
            // it leads to (looks10 L2).
            if let Some((r, c)) = caret {
                carve_cell(&mut scratch.feet, geom, (i32::from(r), i32::from(c)));
            }
        }
        let text = TextView::new(self.probed_text_spans(), Some(&self.frame_text));
        let mut lit = std::mem::take(&mut scratch.air);
        let mut last: Option<(u32, bool, u32, u32)> = None;
        let air_opts = (air.is_some(), self.bed_air_alone());
        let mut ri = 0;
        while ri < runs.len() {
            let run = &runs[ri];
            ri += 1;
            air_lit(run, geom, &text, air_opts, &mut lit);
            let mut si = 0;
            while si < run.len() {
                let s = &run[si];
                let in_air = lit[si];
                si += 1;
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
                        let r = if shape.rail <= 0.0 {
                            0
                        } else if shape.icy && !shape.pinned {
                            icy_rail_ink(s.pos)
                        } else {
                            bed_rail_ink(raw, shape)
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
                |row, col| matches!(ft.pair(row, col), Some((Some(_), _))),
                &mut scratch.inks,
            );
        }
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
        if shape.icy && !shape.pinned {
            self.push_comet_nucleus(geom, caret, runs_len, shape, under.quads());
        }
    }

    /// A test's negative control: no bed's rail lies in its own row's foot
    /// band ([`BedShape::row_rail`]), as looks9 laid them.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_row_rails(&mut self, on: bool) {
        self.bed_scratch.row_rail_off = !on;
    }

    /// A test's negative control: a swollen rail in its own row is lifted
    /// over the foot band's top by its swell's fall
    /// ([`BedScratch::row_rail_swell_lifts`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_row_rail_swell_lifts(&mut self, on: bool) {
        self.bed_scratch.row_rail_swell_lifts = on;
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
    #[inline]
    const fn bed_ink(&self, rgb: u32, page: u32, budget: f32, sat: f32) -> u32 {
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
    #[inline]
    pub(super) fn split_air_runs(
        &mut self,
        geom: Geom,
        runs_len: usize,
        caret: bool,
        typed_only: bool,
    ) -> usize {
        let text = TextView::new(self.probed_text_spans(), Some(&self.frame_text));
        let typed = if typed_only {
            Some(&self.comet_typing[..runs_len.min(self.comet_typing.len())])
        } else {
            None
        };
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

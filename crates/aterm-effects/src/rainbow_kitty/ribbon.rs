// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow kitty ribbon geometry, physics, and instanced quad generation.
use std::mem;

use aterm_time::Instant;

use aterm_render::{
    BeamClip, BeamRows, BeamVertex, GlowBlend, RampMark, RampSink, RibbonVertex, over_premul,
    premul_rgb, ribbon_beam_into,
};

use crate::color_math::{hsv2rgb, oklab, relative_luminance, rgb2hsv};
use crate::cursor_glow::{InkRole, band_pos, band_row};
use crate::effect_util::lerp_rgb;
use crate::spectrum::{spectrum, spectrum_with_min_saturation};

use super::companion::KeepOut;
use super::meteor::tri;
use super::prism_glass::PaletteCache;
use super::spine::{DISP_RELEASE_TAU, PHASE_RATE};
use super::stardust::GlyphProbe;
use super::timing::{
    EDGE_IN_S, FLIGHT_MIN_MS, IOI_DEFAULT_MS, JUMP_MIN_CELLS, REDUCED_MOTION_FADE_MS, clamp01,
    edge_in, flight_ms, ioi_next, kill_span_s, phrase_rest_ms, smoothstep01, spend, suck_in,
};
use super::{Cadence, Config, Ctx, Event, Frame, Licence, TypedClass, level_step_spend};

/// Quads the ribbon may spend on one frame (RAINBOW-KITTY-V2.md §18: a 76-cell hot line at retina).
pub const RIBBON_QUAD_BUDGET: usize = 10_240;

/// Cells the hot edge reaches back from the head (§4.1).
pub const HOT_EDGE_CELLS: f32 = 3.0;

/// The hot edge's line thickness, px.
pub const HOT_EDGE_THICKNESS: f32 = 1.0;

/// How far back from its head end a run the hand has left feathers in, in
/// cells ([`Ribbon::head_feather`]): the body, its rail and its hot edge thin
/// to nothing at the end the caret jumped off instead of standing square-cut
/// there, at the end and along the top edge beside it. Wide enough that its
/// pixel-fine boundaries step by no more than a fifth of the body a column,
/// narrow enough that the band an Enter leaves goes dark with a submit's.
pub const HEAD_FEATHER_CELLS: f32 = 1.2;

/// The furthest back, in cells, the crisp edge may reach when a key is born on a change of speed or under an open theme ([`edge_cells`]): the same alpha law over a longer `d`, so a longer streak, never a brighter one.
pub const HOT_EDGE_CELLS_MAX: f32 = 5.0;

/// The hot edge's falloff shape, normalised at the head cell.
pub const HOT_EDGE_ALPHA: f32 = 0.38;

/// The hot edge's coverage request ceiling.
pub const HOT_EDGE_COV_MAX: f32 = 118.0;

/// The relative luminance the hot edge's ink is lifted to when its stop is darker.
pub const HOT_EDGE_LUMA_FLOOR: f32 = 2.0 * BED_LUMA_MAX;

/// The spine value from which the hot edge's momentum gain starts rising (over [`HOT_EDGE_DISP_SPAN`]).
pub const HOT_EDGE_DISP_MIN: f32 = 0.15;

/// Width of the smoothstep that fades the hot edge in over [`HOT_EDGE_DISP_MIN`].
pub const HOT_EDGE_DISP_SPAN: f32 = 0.30;

/// The share of the hot edge's gain a cold hand gets (a word's first key, momentum zero).
pub const HOT_EDGE_GAIN_FLOOR: f32 = 0.5;

/// How far, in `ch`, the hot edge's hairline may sit from the caret's cell band before it becomes a highlighter on another row.
#[cfg(test)]
pub const HOT_EDGE_CARET_REACH_CH: f32 = 0.55;

/// Cells of the classic walk's first phase, which advances `d/16` per cell so a short word already shows several stops.
pub const WALK_FAST_CELLS: f32 = 16.0;

/// The classic walk's steady lay rate, `t`-units per cell, after [`WALK_FAST_CELLS`].
pub const WALK_LAY_RATE: f32 = 1.0 / 36.0;

/// The birth floor.
pub const BIRTH_EDGE_FLOOR: f32 = 0.35;

/// The tall body's reach above the spine, in `ch`.
pub const TALL_UP_CH: f32 = 1.10;

/// The tall body's full-strength plateau above the spine, in `ch`.
pub const TALL_CORE_CH: f32 = 0.40;

/// The downward melt's floor, in `ch`.
pub const DN_FLOOR_CH: f32 = 0.25;

/// The downward reach at full momentum, in `ch`.
pub const DN_TOP_CH: f32 = DN_FLOOR_CH + WAVE_AMP_CELLS;

/// The underline spelling's reach above the spine, in `ch`: the row's foot
/// band, under the letters' baseline (a 26 px row's letters end on its 21st
/// pixel row). Reaching `1.0 - DN_TOP_CH` (0.64 ch) it washed the letters'
/// lower half and read as the pet's ribbon cut short, not a thin rainbow
/// under the words (looks10 L3).
pub const UNDERLINE_UP_CH: f32 = 0.20;

/// The underline spelling's reach below the spine at the hand, in `ch`: the
/// melt's floor ([`DN_FLOOR_CH`]) at every momentum, where the pet's comet
/// body opens to [`COMET_DN_HAND_CH`], so the underline stays a thin band
/// over its rail (looks10 L3).
pub const UNDERLINE_DN_CH: f32 = DN_FLOOR_CH;

/// The leading's half-thickness in `ch`.
pub const LEAD_CH: f32 = 0.11;

/// The tall highlighter's shoulder.
pub const SHOULDER_TALL: f32 = 1.0;

/// The underline spelling's quieter glyph-band wash.
pub const SHOULDER_UNDERLINE: f32 = 0.55;

/// The 0.75-cycle wave's amplitude in cells at full spine (about 2 px at `ch 18`).
pub const WAVE_AMP_CELLS: f32 = 0.110;

/// Cycles of the wave across the whole mark, whatever its length.
pub const WAVE_CYCLES: f32 = 0.75;

/// The share of the mark over which the wave's amplitude smoothsteps up from the head.
pub const WAVE_HEAD_PIN: f32 = 0.35;

/// Cells over which the body's downward wedge closes behind the head.
pub const BLOOM_REACH_CELLS: f32 = 8.0;

/// Vertices per cell along the major axis, at most.
pub const SLABS_PER_CELL: usize = 3;

const STRIP_LIFT_GAIN: f32 = 0.35;

/// The comet's reach below the spine at the hand, in `ch`, at full momentum.
pub const COMET_DN_HAND_CH: f32 = 0.55;

/// The tall body's reach above the spine at the comet's tail, in `ch`.
pub const COMET_UP_TAIL_CH: f32 = 0.80;

/// Cells behind the head over which the comet's `up` tapers from [`TALL_UP_CH`] to [`COMET_UP_TAIL_CH`] (smoothstep).
pub const COMET_TAPER_CELLS: f32 = 12.0;

/// The light glass's reach above its row's bottom at the hand, in `ch` ([`GLASS_BODY`],
/// `docs/design/PRISM-GLASS.md` §2.2).
pub const GLASS_UP_CH: f32 = 0.96;

/// The glass's reach [`COMET_TAPER_CELLS`] behind the hand on the comet, in `ch` (smoothstep
/// from [`GLASS_UP_CH`]).
pub const GLASS_TAIL_UP_CH: f32 = 0.86;

/// The glass's core, as a share of its reach.
pub const GLASS_CORE_SHARE: f32 = 0.5;

/// How far the glass's top swings with the spine's wave at full momentum, in `ch` (main's spine
/// swings [`WAVE_AMP_CELLS`]): the glass stands on its row's bottom, so its wave is its height's.
pub const LIGHT_GLASS_WAVE_CH: f32 = 0.035;

/// The glass's top edge, px: its light melts to nothing over the top px of its reach
/// (`aterm_render::glass_profile`).
pub const GLASS_EDGE_PX: f32 = 2.5;

/// The least glass a run draws, px: a run none of whose boundaries stands this tall (the frame's
/// intensity scales the glass's height, never its opacity) draws no body, so the glass is
/// whole or absent and no pastel of it is ever laid.
pub const GLASS_MIN_PX: f32 = 2.0;

/// The glass's ceiling (§15.2 1.7): it lays whole opacity hot and [`BODY_COLD_SHARE`] of it cold,
/// where main's paper bed lays its role's cap ([`light_role`]).
pub const GLASS_ALPHA: f32 = 255.0;

/// The largest walk step a glass boundary lerps across (§2.2 Seams): one cell's at the walk's
/// fastest lay ([`WALK_FAST_CELLS`]), the step the glass's legibility proof covers. Between two
/// cells of a run whose walk differs by more (an in-run walk jump), and at a cohort seam, the
/// glass steps hard, each side in its own ink.
pub const GLASS_HARD_STEP_T: f32 = 1.0 / 16.0;

/// The rounding a walk step is read through before it is a hard step ([`GLASS_HARD_STEP_T`]):
/// two cells laid one fast step apart differ by it to within a float's error.
const GLASS_HARD_STEP_SLACK: f32 = 1e-3;

/// The most rows the hand's structural set holds ([`Ribbon::structural_rows`],
/// `docs/design/PRISM-GLASS.md` §15.2 1.10): the hand's row and the rows nearest it.
pub const STRUCTURAL_ROWS_MAX: u16 = 8;

/// The low glass a wrapped row the hand has left sinks to, in `ch` ([`GlassWrap`], §2.4 The sink,
/// WRAP_SINK_CH).
pub const WRAP_SINK_CH: f32 = 0.34;

/// How long a wrapped row the hand has left takes to sink to [`WRAP_SINK_CH`], seconds
/// (smoothstep, §2.4 WRAP_SINK_S).
pub const WRAP_SINK_S: f32 = 0.40;

/// The page the hand's row of a multi-row structural set keeps above its glass, px (§2.4 The
/// hand-row inset, WRAP_HAND_TOP_PX): the glass reaches at most a cell less this up its row.
pub const WRAP_HAND_TOP_PX: f32 = 3.0;

/// The flat ribbon's reach above its spine on paper, as a share of the pet's body's.
pub const FLAT_PAPER_UP_SHARE: f32 = 0.72;

/// The flat ribbon's full-strength plateau on paper, as a share of its
/// reach, as it was laid (the glow [`FLAT_PAPER_GAIN`] replaced).
pub const FLAT_PAPER_CORE_SHARE: f32 = 0.85;

/// The flat ribbon's body on paper: a soft even band, as its rail is on a
/// dim page (looks8 K3), at this share of the pet's light, with no lit
/// plateau or lift. Laid as the pet's glow, its plateau near its top, it
/// read as the pet's ribbon without the rail, a near twin at 1x (looks9 Q6).
pub const FLAT_PAPER_GAIN: f32 = 0.62;

/// The rail's ink luminance floor on a dark page, where a deeper blue or violet would sink
/// into the page.
pub const RAIL_LUMA_FLOOR: f32 = 0.20;

/// The rail's ink luminance floor on a light page ([`paper_rail_ink`]). The dark page's floor
/// ([`RAIL_LUMA_FLOOR`]) lifted blue and violet toward white there: the line under the typed
/// words paled to periwinkle and lavender over that stretch (pure blue's ink #6767FF, its palest
/// entry #6967FF at RGB chroma 152, where the walk's other inks read 201 or more), on a page where
/// a deeper line only reads more. Here they keep their colour down to this luminance (the palest
/// entry #4E4BFF, chroma 180), over the floor a film reads a vivid line at (0.12,
/// `aterm_lab::trail_film::LINE_LUMA`).
pub const PAPER_RAIL_LUMA_FLOOR: f32 = 0.14;

/// The rail's luminance ceiling.
pub const RAIL_LUMA_CEIL: f32 = 0.54;

const _: () = assert!(
    RAIL_LUMA_FLOOR < RAIL_LUMA_CEIL && PAPER_RAIL_LUMA_FLOOR < RAIL_LUMA_FLOOR,
    "the rail's floors must sit under its ceiling, the paper's under the dark page's"
);

/// The rail's coverage as a share of the body's at the same slab.
pub const RAIL_GAIN: f32 = 1.0;

/// The rail's full-strength plateau as a share of its reach.
pub const RAIL_CORE_SHARE: f32 = aterm_render::RIBBON_CORE_SHARE;

/// The share of its reach the flat ribbon's rail on a dim page holds at
/// full strength ([`Ribbon::flat_dim`]): none. Its band is as deep as a
/// rail may reach everywhere along the run (an even height, where the pet's
/// rail follows its body's wave) and falls off from its top with no
/// plateau, so the flat spelling keeps its own signature on the dim page:
/// with a fifth of its reach at full strength and the pet's own
/// coverage it read as the pet's rail (lift 74.6 against the pet's 74.8 on
/// Solarized Dark, looks7 final).
pub const FLAT_DIM_RAIL_CORE: f32 = 0.0;

/// The flat ribbon's rail coverage on a dim page as a share of the pet's
/// ([`RAIL_GAIN`]): about half the pet's peak, a soft band of the
/// rainbow's light below the letters, not the pet's lit under-edge.
pub const FLAT_DIM_RAIL_GAIN: f32 = 0.5;

/// The from-the-hand attack's duration, seconds.
pub const ATTACK_WIPE_S: f32 = 0.040;

/// The wipe front's softness, in cells.
pub const ATTACK_WIPE_FRONT: f32 = 0.35;

/// How far below the row bottom the vivid rail may reach, in cells.
pub const RAIL_REACH_MAX_CH: f32 = DN_TOP_CH;

/// The hairline of body the vivid rail leaves under the row, in cells
/// ([`rail_gap_px`]): a descender's foot ends on the row's last pixel row,
/// and a rail laid at the row's bottom would meet it and leave the foot
/// little contrast against the rail's vivid ink.
pub const RAIL_GAP_CH: f32 = 0.06;

/// How deep the rail lies inside its own row, in cells (whole pixels),
/// where the air below is a filled cell's (a selection, a powerline
/// segment): there the frame cuts the light the rail lays on it, and on
/// Solarized Dark the ribbons over a filled row drew a bed and no rail
/// (looks8 K9). It keeps the row's foot band, under the letters' baseline
/// and the judge's ring round it (a 26 px row's letters end on its 21st
/// pixel row, its descenders on its 25th), and only on a row whose glyphs
/// the host named: it parts round each cell whose ink may reach under the
/// baseline ([`FOOT_CLEAR_PX`]), so it meets no letter's ring and the
/// letter guard leaves it whole
/// ([`crate::cursor_glow::InkLedger::note_foot_rail`]).
pub const ROW_RAIL_CH: f32 = 0.12;

/// How far below its row's bottom the in-row rail ([`ROW_RAIL_CH`]) dips
/// under a foot, px: its top the judge's ring under the row, clear of a
/// descender that reaches the row's last pixel. It dips only where no
/// letter of the row below meets it, into that row's blank top; parted
/// round each foot, Solarized Dark's context rail broke under the `q` of
/// `quick` (looks9 Q9).
pub const ROW_RAIL_DIP_GAP_PX: f32 = crate::cursor_glow::LETTER_GUARD_RING as f32;

/// How much thinner the in-row rail ([`ROW_RAIL_CH`]) lies where it dips
/// under a foot ([`ROW_RAIL_DIP_GAP_PX`]), px: its foot and the judge's ring
/// round it then keep within the row below's blank top over a short glyph
/// (`crate::cursor_glow`'s short glyphs, their ink a third of the cell
/// down). Whole there, a letter beside the foot's cell below broke the dip:
/// Solarized Dark's context rail left a 10 px gap past the `q` of `quick`
/// with the `a` of `a selected` under the next cell (looks9 round 2's Q9).
pub const ROW_RAIL_DIP_THIN_PX: f32 = 1.0;

/// How far the in-row rail ([`ROW_RAIL_CH`]) keeps off a foot's cell, px:
/// the judge's ring and a pixel, and a slab more (a slab spans to the next
/// vertex at its own light), then easing in over [`FOOT_EASE_PX`].
pub const FOOT_CLEAR_PX: f32 = crate::cursor_glow::LETTER_GUARD_RING as f32 + 1.0;
/// The run over which the in-row rail eases in beside a foot, px.
pub const FOOT_EASE_PX: f32 = 3.0;

/// The pitch the in-row rail ([`ROW_RAIL_CH`]) is laid at beside a foot's ink and a thin
/// caret, px ([`part_row_rail`]): a slab is flat at the light of its middle, so the gap round
/// a descender is cut to the pixel.
pub const ROW_RAIL_PITCH_PX: f32 = 1.0;

/// The run over which the in-row rail ([`ROW_RAIL_CH`]) eases in beside a foot's ink, px,
/// where the host's face names that ink ([`KeepOut::foot_gap`]): the rail keeps
/// [`FOOT_CLEAR_PX`] off the ink's columns (the judge's ring and a pixel; laid a pixel apart,
/// its slabs carry no light across that) and is whole two pixels on, a gap as wide as the
/// descender and its ring, as a typeset underline's skip is.
pub const ROW_RAIL_INK_EASE_PX: f32 = 2.0;

/// How far the in-row rail ([`ROW_RAIL_CH`]) keeps off a bar or an underline caret's cell,
/// px: the judge's ring and a pixel, as off a foot ([`FOOT_CLEAR_PX`]), so the line under the
/// words ends in a clean gap before the caret and none of its light lies in the caret's cell,
/// where a bar stands the cell's whole height and an underline in the foot band itself.
pub const CARET_CLEAR_PX: f32 = FOOT_CLEAR_PX;

/// The run over which the in-row rail eases out before a thin caret ([`CARET_CLEAR_PX`]), px.
pub const CARET_EASE_PX: f32 = 2.0;

/// [`RAIL_GAP_CH`] in whole device pixels at cell height `ch`, and never
/// inside the judge's ring round a descender's foot
/// ([`crate::cursor_glow::LETTER_GUARD_RING`]), as a bed's rail on a lit page
/// keeps (`cursor_glow::bed::lit_rail_drop`). Rounded alone, a row under 25 px
/// (a 13 px font's 16 px row at 1x, the Settings card's) left a pixel: the
/// rail was not clear of its row's ring, so the letter guard weighed its
/// vivid ink as the brightest ground of every lettered cell above it and cut
/// the whole row's ribbon to about half its light, where the letters read
/// 5.4:1 uncut (the Settings cursor demo's dark ribbon, settings round 2).
#[inline]
#[must_use]
pub const fn rail_gap_px(ch: f32) -> f32 {
    (RAIL_GAP_CH * ch)
        .round()
        .max(crate::cursor_glow::LETTER_GUARD_RING as f32)
}

const _: () = assert!(RAIL_REACH_MAX_CH < aterm_render::SUB_CEILING);

/// The bed's byte ceiling.
pub const UNDER_COV_CAP: f32 = 236.0;

/// The coverage at which a planned boundary counts as on the glass for `trail status` ([`Ribbon::lit_segments`]).
pub const STATUS_LIT_COV: u8 = (UNDER_COV_CAP / 2.0) as u8;

/// The paint scanner's colour floor.
pub const SCAN_INK_MAX: u32 = 60;

/// …and the spread half of it — see [`SCAN_INK_MAX`].
pub const SCAN_INK_SPREAD: u32 = 40;

/// Whether a composited `0x00RRGGBB` pixel reads as band ink to the glass census ([`SCAN_INK_MAX`] / [`SCAN_INK_SPREAD`]).
#[inline]
#[must_use]
pub const fn reads_as_ink(rgb: u32) -> bool {
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let mx = if r > g { r } else { g };
    let mx = if mx > b { mx } else { b };
    let mn = if r < g { r } else { g };
    let mn = if mn < b { mn } else { b };
    mx >= SCAN_INK_MAX && mx - mn >= SCAN_INK_SPREAD
}

const BODY_FRAME_TOP: f32 = 251.0;

use crate::cursor_glow::{BODY_CONTRAST_BAR, BODY_CONTRAST_GUARD};

/// A place in a stream laid through a [`RampSink`]: its quads' length and
/// its ramps' mark ([`BeamRows::mark`]).
pub type StreamMark = (usize, RampMark);

/// What [`Ribbon::emit`] lays of a run: its body, the vivid rail under its row, or the rail
/// in its own row's foot band ([`ROW_RAIL_CH`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lay {
    Body,
    Rail,
    RowRail,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static BODY_BAND_WHOLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static FEATHER_BY_ROOM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A test's negative control: `f` run with the head feather laid finely only in a frame under
/// [`FEATHER_ROOM_SHARE`] of the quad budget, as it was: a long line's feather was a stair of
/// flat slabs that walked under it as the line flowed into its fold. Debug and test builds
/// only; the flag is this thread's and is put back as it was when `f` returns or panics.
#[cfg(any(test, debug_assertions))]
pub fn with_head_feather_by_room<R>(f: impl FnOnce() -> R) -> R {
    struct Back(bool);
    impl Drop for Back {
        fn drop(&mut self) {
            FEATHER_BY_ROOM.with(|c| c.set(self.0));
        }
    }
    let _back = Back(FEATHER_BY_ROOM.with(|c| c.replace(true)));
    f()
}

/// A test's negative control: `f` run with the underline's body laid whole in its row's foot
/// band under a parted rail, as it was: its light lay in every gap the rail leaves round a
/// descender and before a thin caret. Debug and test builds only; the flag is this thread's
/// and is put back as it was when `f` returns or panics.
#[cfg(any(test, debug_assertions))]
pub fn with_body_band_whole<R>(f: impl FnOnce() -> R) -> R {
    struct Back(bool);
    impl Drop for Back {
        fn drop(&mut self) {
            BODY_BAND_WHOLE.with(|c| c.set(self.0));
        }
    }
    let _back = Back(BODY_BAND_WHOLE.with(|c| c.replace(true)));
    f()
}

/// `verts`, a rail in its own row's foot band, laid every [`ROW_RAIL_PITCH_PX`] wherever
/// `near` (asked of a span of x) says its light changes, each vertex passed through `part`;
/// `head` is the head's vertex, and its place in the result is returned. The plan's own
/// vertices stand a few to a cell and a beam lerps between them, so a gap cut at them alone
/// would be a ramp a cell wide either side of a descender.
#[inline]
fn part_row_rail(
    verts: &mut Vec<RibbonVertex>,
    out: &mut Vec<RibbonVertex>,
    head: usize,
    near: impl Fn(f32, f32) -> bool,
    part: impl Fn(RibbonVertex) -> RibbonVertex,
) -> usize {
    out.clear();
    let mut head_out = 0;
    let mut i = 0;
    while i < verts.len() {
        let b = verts[i];
        if i > 0 {
            let a = verts[i - 1];
            let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
            if near(lo, hi) {
                let n = ((hi - lo) / ROW_RAIL_PITCH_PX).ceil().max(1.0) as usize;
                let mut j = 1;
                while j < n {
                    let t = j as f32 / n as f32;
                    let lerp = |u: f32, v: f32| u + (v - u) * t;
                    out.push(part(RibbonVertex {
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
            }
        }
        if i == head {
            head_out = out.len();
        }
        out.push(part(b));
        i += 1;
    }
    mem::swap(verts, out);
    head_out
}

/// Whether the stream laid up to `to` still stands within the stream at
/// `at`: a rail span past it was cut away with what it drew.
#[inline]
const fn rail_within(to: StreamMark, at: StreamMark) -> bool {
    to.0 <= at.0 && to.1.items() <= at.1.items()
}

const BED_LUMA_MIN: f32 = 0.020;

const BED_LUMA_MAX: f32 = 0.150;

/// The bed's chroma floor.
pub const BED_SAT_FLOOR: f32 = 1.0;

/// A dark theme whose text reads under this on its page is dim: its bed is
/// laid at the page's own luminance ([`dim_page_bed`]).
pub const DIM_THEME_BARE: f32 = 7.0;

/// The light bed's one weight.
pub const PAPER_WEIGHT: f32 = 0.24;

/// The light bed's bar on a theme whose own text sits under the body's (Solarized Light's `#586e75` on `#fdf6e3` is 4.97:1).
pub const PAPER_BAR_THEME_SHARE: f32 = 0.8;

/// The leading-only body's weight (the underline spelling, [`InkRole::Leading`]).
pub const PAPER_WEIGHT_LEADING: f32 = 0.30;

/// The least contrast the caret block keeps against a light page
/// ([`paper_caret_ink`]): the caret law's 4.5:1, as every effect-owned
/// caret's ([`crate::cursor_fx::CARET_PAGE_MIN_CONTRAST`]). Held to a
/// graphic's 3:1, a hot burst's green and orange stops would print their
/// carets under 4.5:1 on white.
pub const PAPER_CARET_CONTRAST: f32 = crate::cursor_fx::CARET_PAGE_MIN_CONTRAST;

/// The least contrast a point mark keeps against a light page ([`paper_mark_ink`]).
pub const PAPER_MARK_CONTRAST: f32 = 4.5;

const PAPER_HUE_BEND: [(f32, f32); 4] = [(30.0, 30.0), (72.0, 40.0), (90.0, 85.0), (110.0, 110.0)];

/// The paper caret's hue bend ([`paper_caret_ink`]): yellow walks to an
/// amber and the yellow-greens step from it to a clean green, as a Return's
/// stick steps ([`PAPER_TRAIN_HUE_BEND`]). Bent as a mark is
/// ([`PAPER_HUE_BEND`]), the yellow-greens kept their hue, and deepened to
/// the caret law's 4.5:1 the gallery's `burst` caret read olive (`#557e01`)
/// and then khaki (`#2e3c14`) on GitHub Light (looks10 L7). Stepped at 89
/// degrees, the burst's lime head (88 degrees) still carried a bronze caret
/// (`#946a01`, `#8f6601` on Default Light) against the ribbon's lime: from
/// 76 degrees it steps to the clean green the head reads as (looks10 round
/// 2's L7), and a greenish yellow (68 degrees) keeps its amber.
const PAPER_CARET_HUE_BEND: [(f32, f32); 5] = [
    (30.0, 30.0),
    (72.0, 40.0),
    (76.0, 43.0),
    (76.0, 118.0),
    (122.0, 122.0),
];

/// A paper train's hue bend ([`paper_train_ink`]): as [`PAPER_HUE_BEND`],
/// yellow walks to gold, and gold steps to a clean green: the darkened
/// yellows and yellow-greens between them (an olive, a khaki, a lime gone
/// dark at the paper's contrast) are never laid. Bent through them over its
/// whole interval, the `enter-bottom` Return's stick read olive and khaki
/// (`b1b774`) for a cell and more (looks7 M2); walked through them in six
/// degrees, its turn from amber to green still read olive (`#8b8013` at its
/// gold's end, looks8 K10); stepped from 44 to 88 degrees, its green began
/// at a darkened lime (`#329213`, looks9 Q5).
const PAPER_TRAIN_HUE_BEND: [(f32, f32); 5] = [
    (30.0, 30.0),
    (72.0, 44.0),
    (89.0, 47.0),
    (89.0, 118.0),
    (122.0, 122.0),
];

/// The least contrast a paper train's warm span (its orange and gold,
/// [`PAPER_TRAIN_GOLD_HUES`]) keeps against a light page: a graphic's 3:1.
/// Held to the caret's 4.5:1, a gold is darkened to an ochre on white
/// (`#a27b13`, `#b37113` along the gallery's `enter-bottom` Return, looks9
/// Q5): no gold reaches 4.5:1 on white but as a brown. The stick is a mark
/// on the page, never under a letter (`meteor`'s `paper_launch_drop`), and
/// its other hues keep the caret's bar.
pub const PAPER_TRAIN_GOLD_CONTRAST: f32 = 3.0;

/// The bent hues (degrees) a paper train lays at [`PAPER_TRAIN_GOLD_CONTRAST`],
/// easing to the caret's bar over [`PAPER_TRAIN_GOLD_EASE`] degrees past
/// either end.
pub const PAPER_TRAIN_GOLD_HUES: (f32, f32) = (25.0, 55.0);

/// Degrees over which a paper train's bar eases from the gold's to the caret's.
const PAPER_TRAIN_GOLD_EASE: f32 = 10.0;

/// The hues a paper train never lays, degrees: its bend's step from gold to
/// green ([`PAPER_TRAIN_HUE_BEND`]), which its stick crosses at a hard edge
/// (`meteor`'s `step_over_train_skip`).
pub(crate) const PAPER_TRAIN_SKIP: (f32, f32) =
    (PAPER_TRAIN_HUE_BEND[2].1, PAPER_TRAIN_HUE_BEND[3].1);

/// Entries in the bed-ink lookup table.
pub const BED_INK_LUT_LEN: usize = 129;

/// Share of the ceiling a cold key's cell takes (momentum buys the rest, [`Cell::cov0`]), so a single keystroke lays a readable band.
pub const BODY_COLD_SHARE: f32 = 0.88;

/// The host's duration knob, as a multiplier on the base cell life.
pub const LIFE_DURATION_GAIN: f32 = 1.2;

/// Floor of the base cell life, seconds.
pub const LIFE_BASE_MIN: f32 = 0.26;

/// Ceiling of the base cell life, seconds.
pub const LIFE_BASE_MAX: f32 = 0.55;

/// Linear momentum term of the cell life.
pub const LIFE_SPINE_LINEAR: f32 = 2.6;

/// …and the superlinear term, so an earned hot run drags a rainbow across many cells and wrapped lines instead of merely a longer wake.
pub const LIFE_SPINE_SQUARE: f32 = 11.0;

/// Four-letter guarantee.
pub const CHAIN_KEYS: f32 = 4.5;

/// The ribbon chains across any plausible rhythm, even one key every five seconds.
pub const CHAIN_GAP_MAX: f32 = 5.0;
pub(crate) const CHAIN_GAP_MAX_DUR: std::time::Duration =
    std::time::Duration::from_secs(CHAIN_GAP_MAX as u64);

/// Fade margin added to the chained life, seconds.
pub const CHAIN_MARGIN: f32 = 0.10;

/// …capped just above `CHAIN_KEYS × CHAIN_GAP_MAX` so the slowest chained rhythm still earns its full four-letter span.
pub const CHAIN_LIFE_MAX: f32 = 23.0;

/// New light a jump's corridor may lay, in cells back from the landing, so a Ctrl-A across 200 columns does not paint the whole row.
pub const WAKE_MAX_CELLS: u16 = 32;

/// A wake's life.
pub const WAKE_LIFE_S: f32 = 1.10;

/// A same-row hop under [`JUMP_MIN_CELLS`] (an arrow key) lays its wake at this share of [`WAKE_LIFE_S`].
pub const WAKE_HOP_LIFE_SHARE: f32 = 0.5;

/// Cells the four-letter guarantee reaches for.
pub const FOUR_LETTER_CELLS: u16 = 4;

/// The phrase rest's floor, seconds.
pub const PHRASE_REST_MIN_S: f32 = super::timing::PHRASE_PAUSE_MS as f32 / 1000.0;

/// One extension cell lights per this step while the swoosh finishes reaching four letters.
pub const REACH_STEP_S: f32 = 0.05;

/// Beats of [`REACH_STEP_S`] the reach spends.
pub const REACH_BEATS: u16 = 3;

/// Cells the reach may stage in one tick, across every live cohort.
pub const REACH_STAGE: usize = 8;

/// The retract drains the ribbon tail->head over this long, whatever its length.
pub const RETRACT_DUR_S: f32 = 0.40;

/// How long a retracted cell takes to go out once the drain selects it.
pub const RETRACT_FADE_S: f32 = 0.24;

/// The fold flow's whole life.
pub const FLOW_TOTAL_S: f32 = 1.50;
const FLOW_TOTAL_DUR: std::time::Duration =
    std::time::Duration::from_millis((FLOW_TOTAL_S * 1000.0) as u64);
const _: () = assert!(FLOW_TOTAL_S < SWOOSH_TOTAL_S);

/// The flow's far-first lead.
pub const FLOW_LEAD_S: f32 = RETRACT_DUR_S;

/// The flow's last breath.
pub const FLOW_TAIL_S: f32 = RETRACT_FADE_S;

/// The drift of the coldest hand, cells.
pub const FLOW_DRIFT_MIN_CELLS: f32 = 2.0;

/// The drift of the hottest hand, cells.
pub const FLOW_DRIFT_MAX_CELLS: f32 = FLOW_TOTAL_S / (4.0 * REACH_STEP_S);

/// The drift's cap as a share of the band's kept length.
pub const FLOW_DRIFT_MAX_SHARE: f32 = 0.40;

/// The settle.
pub const FLOW_SETTLE_SHARE: f32 = 0.40;

/// The seam's level.
pub const SEAM_LEVEL: f32 = (1.0 - 1.0 / HOT_EDGE_CELLS) * (1.0 - 1.0 / HOT_EDGE_CELLS);
/// The re-anchor's floor, cells, matching the `raw_dist > 2` floor of
/// `cursor_glow`'s `classify_move`; a shorter backward hop re-anchors only on
/// a proof (`short_reanchor`, `rewrap_start`).
pub const RE_ANCHOR_MIN_CELLS: u16 = 3;

/// The exit swoosh past the grace.
pub const SWOOSH_AFTER_GRACE_S: f32 =
    REACH_STEP_S * REACH_BEATS as f32 + RETRACT_DUR_S + RETRACT_FADE_S;

/// The whole exit swoosh at the floor grace.
pub const SWOOSH_TOTAL_S: f32 = PHRASE_REST_MIN_S + SWOOSH_AFTER_GRACE_S;

/// The margin a cell's life keeps past its swoosh, so the swoosh always ends the ribbon before the natural melt can.
pub const SWOOSH_LIFE_MARGIN_S: f32 = 0.01;

/// The life floor at the floor grace, 1.70 s.
pub const SWOOSH_LIFE_S: f32 = SWOOSH_TOTAL_S + SWOOSH_LIFE_MARGIN_S;

/// The swoosh's retract offset at the floor grace.
pub const RETRACT_START_S: f32 = retract_start_s(PHRASE_REST_MIN_S);

/// The idle at which a cohort of grace `grace_s` starts its retract.
#[inline]
#[must_use]
pub const fn retract_start_s(grace_s: f32) -> f32 {
    grace_s + REACH_STEP_S * REACH_BEATS as f32
}

/// The whole swoosh of a cohort of grace `grace_s`.
#[inline]
#[must_use]
pub const fn swoosh_total_s(grace_s: f32) -> f32 {
    grace_s + SWOOSH_AFTER_GRACE_S
}

/// The life floor of a cell whose phrase rests `grace_s`.
#[inline]
#[must_use]
pub const fn swoosh_life_s(grace_s: f32) -> f32 {
    swoosh_total_s(grace_s) + SWOOSH_LIFE_MARGIN_S
}

/// Focus loss embers the ribbon out over this long on `spend` (§8.2).
pub const FOCUS_EMBER_S: f32 = 0.30;

/// A stop is on the glass while its composite is a colour the eye can pick from the ground.
pub const SEEN_SPREAD: f32 = 40.0;

/// The level half of [`SEEN_SPREAD`].
pub const SEEN_CONTRAST: f32 = 1.90;

/// The ember's per-stop floor, in that stop's extinction units.
pub const EMBER_FLOOR_MARGIN: f32 = 1.05;

/// The curtain.
pub const CURTAIN_S: f32 = RETRACT_FADE_S;

/// The curtain's stagger.
pub const CURTAIN_STAGGER_S: f32 = CURTAIN_S * (RETRACT_DUR_S / (RETRACT_DUR_S + RETRACT_FADE_S));

/// What each cell of a falling curtain spends over once its [`CURTAIN_STAGGER_S`] turn has come.
pub const CURTAIN_SPEND_S: f32 = CURTAIN_S - CURTAIN_STAGGER_S;

/// The curtain's cliff bound.
#[cfg(test)]
pub const CURTAIN_MAX_STEP_FRAC: f32 = 0.34;

/// The other half of the law.
#[cfg(test)]
pub const CURTAIN_MIN_FRAMES: usize = 5;

/// The content retirement's melt.
pub const RETIRE_MELT_S: f32 = 0.12;

/// The share of its light a line keeps once a Return has left it printed ([`Cohort::printed`]).
pub const RETURN_PRINTED_SHARE: f32 = 0.3;

/// How long the dim to [`RETURN_PRINTED_SHARE`] takes after the Return.
pub const RETURN_DIM_S: f32 = 0.06;

/// The share of a cell's life over which the expiry melt runs; identity over the first 70 %.
pub const EXPIRY_MELT_SHARE: f32 = 0.30;

/// The expiry melt's exponent.
pub const EXPIRY_MELT_GAMMA: f32 = 1.6;

/// The coverage share the run's outer tail boundary keeps, ramping to full across the tail cell, so the oldest edge feathers out.
pub const RUN_TAIL_EASE: f32 = 0.10;

/// The cells over which a run the quad budget cuts thins to [`RUN_TAIL_EASE`] at the cut, so the
/// end the budget chose is a feather like the end the band chose ([`Ribbon::lay_capped`]). Six,
/// fitted against the two steps the asserts below hold: the baseline strip rides the same ramp, so
/// each level of it prints `1 + STRIP_LIFT_GAIN` levels, and at four or five cells the first steps
/// read as an edge.
pub const BUDGET_FEATHER_CELLS: usize = 6;

const _: () = assert!(
    (1.0 - RUN_TAIL_EASE) / BUDGET_FEATHER_CELLS as f32 * (1.0 + STRIP_LIFT_GAIN) * UNDER_COV_CAP
        <= 60.0,
    "the budget feather's step from one column to the next must stay under 60 levels"
);
const _: () = assert!(
    (RUN_TAIL_EASE + (1.0 - RUN_TAIL_EASE) / (2.0 * BUDGET_FEATHER_CELLS as f32))
        * (1.0 + STRIP_LIFT_GAIN)
        * UNDER_COV_CAP
        <= 60.0,
    "the budget feather's first printed column, a slab's centre, must stay under 60 levels"
);

/// The instant a cell's expiry is measured from.
#[inline]
#[must_use]
pub fn live_since(cohorts: &[Cohort], cell: &Cell) -> Instant {
    if let Some(c) = Cohort::find(cohorts, cell.cohort) {
        return cell.born.max(c.alive_at);
    }
    cell.born
}

#[inline]
fn is_fresher_or_equal(a: &Cohort, b: &Cohort) -> bool {
    match a.alive_at.cmp(&b.alive_at) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => a.walk_d(a.col1) >= b.walk_d(b.col1),
    }
}

/// Which layer a cell is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    #[default]
    Base,
    Over,
}

/// One laid ribbon cell, the atom of the field.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub row: u16,
    pub col: u16,
    pub cohort: u32,
    pub t: f32,
    pub born: Instant,
    pub attack_at: Instant,
    pub life_s: f32,
    pub cov0: f32,
    pub typing: bool,
    pub retract_at: Option<Instant>,
    pub retire_at: Option<Instant>,
    pub birth_disp: f32,
    pub edge_cells: f32,
    pub layer: Layer,
    pub rearm: Option<(Instant, f32)>,
    pub released_at: Option<Instant>,
}

impl Cell {
    /// True once the cell is on its way out (retracting after a Backspace or kill, or retired by content) and so no longer owns its column.
    #[inline]
    #[must_use]
    pub const fn leaving(&self) -> bool {
        self.retract_at.is_some() || self.retire_at.is_some()
    }
}

/// A cohort: one contiguous run of cells laid by one typing burst on one row, sharing a hue walk.
#[derive(Clone, Copy, Debug)]
pub struct Cohort {
    pub id: u32,
    pub row: u16,
    pub col0: u16,
    pub col1: u16,
    pub anchor_col: u16,
    pub t0: f32,
    pub d0: f32,
    pub born: Instant,
    pub alive_at: Instant,
    pub abandoned: bool,
    pub wake: bool,
    pub retract_col: Option<u16>,
    pub drain_right_first: bool,
    pub phase: Phase,
    pub rejoinable: bool,
    pub phrase: u32,
    pub grace_s: f32,
    pub flow: Option<Instant>,
    pub flow_drift: f32,
    release_clock: Option<ReleaseClock>,
    parts_revoked_at: Option<Instant>,
    line_killed: bool,
    printed: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct ReleaseClock {
    alive_at: Instant,
    retract_col: Option<u16>,
    phase: Phase,
    rejoinable: bool,
    cells: usize,
}

#[derive(Clone, Copy, Debug)]
struct ReleasePart {
    cohort: u32,
    stamp: Instant,
    cells: usize,
}

impl ReleasePart {
    #[inline]
    fn find(parts: &[Self], cohort: u32, stamp: Instant) -> Result<usize, usize> {
        let mut lo = 0usize;
        let mut hi = parts.len();
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let p = &parts[mid];
            match (p.cohort, p.stamp).cmp(&(cohort, stamp)) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(mid),
            }
        }
        Err(lo)
    }

    #[inline]
    fn find_next(parts: &[Self], cohort: u32, after: Option<Instant>) -> usize {
        let mut i = match after {
            Some(stamp) => match Self::find(parts, cohort, stamp) {
                Ok(idx) => idx + 1,
                Err(idx) => idx,
            },
            None => {
                let mut lo = 0usize;
                let mut hi = parts.len();
                while lo < hi {
                    let mid = lo + ((hi - lo) >> 1);
                    if parts[mid].cohort < cohort {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                lo
            }
        };
        while i < parts.len()
            && parts[i].cohort == cohort
            && matches!(after, Some(stamp) if parts[i].stamp <= stamp)
        {
            i += 1;
        }
        i
    }
}

impl Cohort {
    #[inline]
    fn revoke_releases(&mut self, at: Instant) {
        self.release_clock = None;
        self.parts_revoked_at = Some(match self.parts_revoked_at {
            Some(t) => t.max(at),
            None => at,
        });
    }

    #[inline]
    fn holds_partial(&self, cell: &Cell, parts: &[ReleasePart]) -> bool {
        !self.abandoned
            && if let Some(stamp) = cell.released_at {
                ReleasePart::find(parts, self.id, stamp).is_ok()
            } else {
                false
            }
    }

    /// The band is leaving at `at`.
    #[inline]
    #[must_use]
    pub fn leaving_at(&self, at: Instant) -> bool {
        self.abandoned || !matches!(self.phase_at(self.idle_s(at)), Some(p) if !p.is_retracting())
    }

    /// The idle at which this cohort's retract begins.
    #[inline]
    #[must_use]
    pub const fn retract_start(&self) -> f32 {
        if self.flow.is_some() {
            0.0
        } else {
            retract_start_s(self.grace_s)
        }
    }

    /// How long this cohort's retract's farthest-first stagger spans.
    #[inline]
    #[must_use]
    pub const fn retract_span(&self) -> f32 {
        if self.flow.is_some() {
            FLOW_LEAD_S
        } else {
            RETRACT_DUR_S
        }
    }

    /// The drift's progress, `0..1` at `idle` seconds into the flow.
    #[inline]
    #[must_use]
    pub const fn flow_progress(&self, idle: f32) -> f32 {
        1.0 - spend(clamp01(idle * (1.0 / FLOW_TOTAL_S)))
    }

    /// The flow's fade at a column, `1 -> 0`.
    #[inline]
    #[must_use]
    pub const fn flow_drain(&self, col: u16, idle: f32) -> f32 {
        let s = FLOW_LEAD_S * (1.0 - self.drain_rank(col));
        spend(clamp01((idle - s) / (FLOW_TOTAL_S - s)))
    }

    /// The idle at which this cohort's swoosh — or its fold flow — is over.
    #[inline]
    #[must_use]
    pub const fn swoosh_total(&self) -> f32 {
        if self.flow.is_some() {
            FLOW_TOTAL_S
        } else {
            swoosh_total_s(self.grace_s)
        }
    }

    /// The phase this cohort is in `idle_s` after its last live key.
    #[inline]
    #[must_use]
    pub const fn phase_at(&self, idle_s: f32) -> Option<Phase> {
        if self.flow.is_none() {
            return Phase::at(idle_s, self.grace_s);
        }
        if idle_s.is_nan() || idle_s <= 0.0 {
            Some(Phase::Laying)
        } else if idle_s < FLOW_TOTAL_S {
            Some(Phase::Retracting)
        } else {
            None
        }
    }

    /// The clock the exit runs from.
    #[inline]
    #[must_use]
    pub fn clock(&self) -> Instant {
        match self.flow {
            Some(f) => self.alive_at.max(f),
            None => self.alive_at,
        }
    }

    /// Seconds of idle at `now` on [`Cohort::clock`].
    #[inline]
    #[must_use]
    pub fn idle_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.clock()).as_secs_f32()
    }

    /// The drain rank of a column, `0..=1`.
    #[inline]
    #[must_use]
    pub const fn drain_rank(&self, col: u16) -> f32 {
        let span = (self.col1.saturating_sub(self.col0) as f32).max(1.0);
        if self.drain_right_first {
            col.saturating_sub(self.col0) as f32 / span
        } else {
            self.col1.saturating_sub(1).saturating_sub(col) as f32 / span
        }
    }

    /// The walk's distance at a column.
    #[inline]
    #[must_use]
    pub const fn walk_d(&self, col: u16) -> f32 {
        self.d0 + (col as f32 - self.anchor_col as f32)
    }

    /// The walk's zero.
    #[inline]
    #[must_use]
    pub const fn walk_zero(&self) -> f32 {
        self.t0 - walk_t(self.d0)
    }

    /// The field at a column of this cohort.
    #[inline]
    #[must_use]
    pub const fn t_at(&self, col: u16) -> f32 {
        self.walk_zero() + walk_t(self.walk_d(col))
    }

    #[inline]
    #[must_use]
    const fn walk_at(&self, col: u16) -> WalkPos {
        WalkPos {
            t: self.t_at(col),
            d: self.walk_d(col),
        }
    }

    #[inline]
    #[must_use]
    const fn origin_walk(&self) -> WalkPos {
        WalkPos {
            t: self.t0,
            d: self.d0,
        }
    }

    #[inline]
    #[must_use]
    pub(crate) const fn find(cohorts: &[Self], id: u32) -> Option<&Self> {
        let mut i = 0;
        while i < cohorts.len() {
            if cohorts[i].id == id {
                return Some(&cohorts[i]);
            }
            i += 1;
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WalkPos {
    t: f32,
    d: f32,
}

impl Default for WalkPos {
    #[inline]
    fn default() -> Self {
        Self::COLD
    }
}

impl WalkPos {
    const COLD: Self = Self { t: 0.0, d: 0.0 };

    #[inline]
    const fn restarted(self) -> Self {
        Self { t: self.t, d: 0.0 }
    }
}

/// A cohort's place in the exit swoosh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Laying,
    Grace,
    Reaching,
    Retracting,
    Fading,
}

impl Phase {
    /// The phase a cohort of grace `grace_s` ([`Cohort::grace_s`]) is in `idle_s` seconds after its last live key.
    #[inline]
    #[must_use]
    pub const fn at(idle_s: f32, grace_s: f32) -> Option<Self> {
        let grace = if grace_s.is_finite() {
            grace_s.max(PHRASE_REST_MIN_S)
        } else {
            PHRASE_REST_MIN_S
        };
        let reach_end = retract_start_s(grace);
        let retract_end = reach_end + RETRACT_DUR_S;
        if idle_s.is_nan() || idle_s <= 0.0 {
            // NaN-refusing spelling: a NaN idle takes the laying arm too, which draws the mark rather than dropping it.
            return Some(Self::Laying);
        }
        if idle_s < grace {
            Some(Self::Grace)
        } else if idle_s < reach_end {
            Some(Self::Reaching)
        } else if idle_s < retract_end {
            Some(Self::Retracting)
        } else if idle_s < swoosh_total_s(grace) {
            Some(Self::Fading)
        } else {
            None
        }
    }

    /// True once the swoosh owns the mark and the retract is moving it.
    #[inline]
    #[must_use]
    pub const fn is_retracting(self) -> bool {
        matches!(self, Self::Retracting | Self::Fading)
    }
}

/// One planned sample of a cohort's spine at a cell boundary, what `aterm_render::RibbonVertex` is built from.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub x: f32,
    pub spine: f32,
    pub up: f32,
    pub dn: f32,
    pub t: f32,
    pub cov: u8,
    /// The glass's reach above its row's bottom here, px, on a frame the bed draws by its
    /// glass ([`BedInkLut::glass_lit`], §15.2 1.7): [`GLASS_BODY`]'s, its wave, the frame's
    /// intensity, its exits and its oldest end's ease (§15.2 1.8) and its row's sink and inset
    /// (§15.2 1.10, [`GlassWrap`], [`WRAP_HAND_TOP_PX`]) folded in. `0` on every other
    /// frame, where the body is main's (`spine`, `up`, `dn`), which the plan keeps on a glass
    /// frame too ([`Ribbon::band`]).
    pub glass_up: f32,
    /// The share of its light the line under the body lays here ([`Ribbon::emit`]): what the
    /// body's height takes and its opacity does not, so the line leaves with the body, never at
    /// whole light until the body is gone. On a frame the bed draws by its glass, what
    /// [`Self::glass_up`] folds in besides the glass's shape (its exits, the flow's last breath,
    /// its oldest end's ease and the frame's intensity; not the flow's settle, which lowers the
    /// band); on every other frame, where `cov` carries the envelope, the flow's last breath
    /// alone.
    pub line_keep: f32,
    /// On a glass frame, the walk the boundary opens its right cell at less the walk it closes
    /// its left cell at (`t`): a hard step (§2.2 Seams, [`GLASS_HARD_STEP_T`]) the glass lays as
    /// two vertices at one x. `0` at every other boundary and on every other frame.
    pub glass_jump: f32,
}

/// The vertical extent of the ribbon's body at one cell, `[spine - up, spine + dn]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub top: f32,
    pub spine: f32,
    pub bottom: f32,
}

impl Band {
    /// True when `y` lies inside the body.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn contains(&self, y: f32) -> bool {
        self.top <= y && y <= self.bottom
    }
}

/// The two profiles, one emitter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyProfile {
    pub up_ch: f32,
    pub dn_ch: f32,
    pub core_up_ch: f32,
    pub shoulder: f32,
    /// The tall ribbon's profile; the underline's when false.
    pub tall: bool,
}

/// The light glass's body (`docs/design/PRISM-GLASS.md` §2.2, §15.2 1.7), what a frame the
/// bed draws by its glass lays in place of [`Ribbon::body_profile`]'s: standing on its row's
/// bottom with no reach below it, [`GLASS_UP_CH`] tall at the hand (easing to
/// [`GLASS_TAIL_UP_CH`] over [`COMET_TAPER_CELLS`] on the comet), its core half its reach, and
/// its top a [`GLASS_EDGE_PX`] melt (`aterm_render::glass_profile`) with no shoulder.
pub const GLASS_BODY: BodyProfile = BodyProfile {
    up_ch: GLASS_UP_CH,
    dn_ch: 0.0,
    core_up_ch: GLASS_CORE_SHARE * GLASS_UP_CH,
    shoulder: 1.0,
    // The glass lights only the tall spelling (`glass_table`).
    tall: true,
};

#[derive(Clone, Debug)]
struct FieldLane {
    row: u16,
    t: Vec<f32>,
    touched: Vec<u16>,
}

impl Default for FieldLane {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl FieldLane {
    const EMPTY: Self = Self {
        row: 0,
        t: Vec::new(),
        touched: Vec::new(),
    };

    #[inline]
    fn clear(&mut self) {
        let mut i = 0;
        while i < self.touched.len() {
            let col = usize::from(self.touched[i]);
            if col < self.t.len() {
                self.t[col] = f32::NAN;
            }
            i += 1;
        }
        self.touched.clear();
    }

    #[inline]
    fn resize(&mut self, cols: u16) {
        self.t.clear();
        self.t.resize(usize::from(cols), f32::NAN);
        self.touched.clear();
    }
}

/// The field index.
#[derive(Clone, Debug)]
pub struct FieldIndex {
    lanes: Vec<FieldLane>,
    spare: Vec<FieldLane>,
    cols: u16,
}

impl Default for FieldIndex {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl FieldIndex {
    /// Empty field index.
    pub const EMPTY: Self = Self {
        lanes: Vec::new(),
        spare: Vec::new(),
        cols: 0,
    };

    /// The field at a cell, or `None` where no ribbon light is laid.
    #[inline]
    #[must_use]
    pub const fn at(&self, row: u16, col: u16) -> Option<f32> {
        let lanes = self.lanes.as_slice();
        let mut i = lanes.len();
        let mut found = None;
        while i > 0 {
            i -= 1;
            if lanes[i].row == row {
                found = Some(&lanes[i]);
                break;
            }
        }
        let Some(lane) = found else {
            return None;
        };
        let ts = lane.t.as_slice();
        let idx = col as usize;
        if idx >= ts.len() {
            return None;
        }
        let t = ts[idx];
        if t.is_nan() { None } else { Some(t) }
    }

    /// Rows carrying field light this frame.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn live_rows(&self) -> usize {
        self.lanes.len()
    }

    #[inline]
    fn begin(&mut self, cols: u16) {
        if self.cols != cols {
            self.cols = cols;
            self.lanes.clear();
            self.spare.clear();
            return;
        }
        let mut i = 0;
        while i < self.lanes.len() {
            if self.lanes[i].touched.is_empty() {
                let dark = self.lanes.swap_remove(i);
                self.spare.push(dark);
            } else {
                i += 1;
            }
        }
        let mut j = 0;
        while j < self.lanes.len() {
            self.lanes[j].clear();
            j += 1;
        }
    }

    #[inline]
    fn lane_mut(&mut self, row: u16) -> &mut FieldLane {
        if matches!(self.lanes.last(), Some(l) if l.row == row) {
            return self.lanes.last_mut().expect("just checked");
        }
        let mut i = 0;
        while i < self.lanes.len() {
            if self.lanes[i].row == row {
                let last = self.lanes.len() - 1;
                self.lanes.swap(i, last);
                return &mut self.lanes[last];
            }
            i += 1;
        }
        let mut lane = self.spare.pop().unwrap_or(FieldLane::EMPTY);
        lane.row = row;
        if lane.t.len() != usize::from(self.cols) {
            lane.resize(self.cols);
        }
        self.lanes.push(lane);
        self.lanes.last_mut().expect("just pushed")
    }

    #[inline]
    fn reset(&mut self) {
        self.lanes.clear();
        self.spare.clear();
    }

    #[inline]
    fn translate(&mut self, rows: u16) {
        let mut i = 0;
        while i < self.lanes.len() {
            if let Some(row) = self.lanes[i].row.checked_sub(rows) {
                self.lanes[i].row = row;
                i += 1;
            } else {
                let mut dark = self.lanes.swap_remove(i);
                dark.clear();
                self.spare.push(dark);
            }
        }
    }

    #[inline]
    fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        let mut i = 0;
        while i < self.lanes.len() {
            if let Some(row) = band_row(self.lanes[i].row, top, bottom, delta) {
                self.lanes[i].row = row;
                i += 1;
            } else {
                let mut dark = self.lanes.swap_remove(i);
                dark.clear();
                self.spare.push(dark);
            }
        }
    }

    #[inline]
    fn set(&mut self, row: u16, col: u16, t: f32) {
        let lane = self.lane_mut(row);
        let Some(slot) = lane.t.get_mut(usize::from(col)) else {
            return;
        };
        if !slot.is_nan() {
            return;
        }
        lane.touched.push(col);
        *slot = t;
    }
}

/// The bed's one ceiling, in relative luminance.
#[inline]
#[must_use]
pub const fn bed_luma_budget(theme_fg: u32) -> f32 {
    let bar =
        (relative_luminance(theme_fg) + 0.05) / (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD) - 0.05;
    bar.clamp(BED_LUMA_MIN, BED_LUMA_MAX)
}

#[inline]
const fn scale_rgb(rgb: u32, k: f32) -> u32 {
    let r = ((((rgb >> 16) & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    let g = ((((rgb >> 8) & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    let b = (((rgb & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    (r << 16) | (g << 8) | b
}

#[inline]
const fn toward_white(rgb: u32, w: f32) -> u32 {
    crate::effect_util::lerp_rgb(rgb, 0x00FF_FFFF, w)
}

#[inline]
const fn solve_for_luma(
    table: &crate::color_math::RelativeLuminanceTable,
    base_rgb: u32,
    k_full: f32,
    walk_white: bool,
    target: f32,
) -> u32 {
    #[inline]
    const fn eval_family(base_rgb: u32, k_full: f32, walk_white: bool, u: f32) -> u32 {
        if walk_white {
            toward_white(base_rgb, u)
        } else {
            scale_rgb(base_rgb, u * k_full)
        }
    }
    let hi0 = eval_family(base_rgb, k_full, walk_white, 1.0);
    if crate::color_math::relative_luminance_with(table, hi0) < target {
        return hi0;
    }
    let lo0 = eval_family(base_rgb, k_full, walk_white, 0.0);
    if crate::color_math::relative_luminance_with(table, lo0) >= target {
        return lo0;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (lo0, hi0);
    let mut i = 0;
    while i < 24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = eval_family(base_rgb, k_full, walk_white, mid);
        if mid_rgb == lo_rgb {
            lo = mid;
        } else if mid_rgb == hi_rgb {
            hi = mid;
        } else if crate::color_math::relative_luminance_with(table, mid_rgb) < target {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        i += 1;
    }
    lo_rgb
}

#[inline]
const fn onto_luma(rgb: u32, target: f32) -> u32 {
    onto_luma_with(crate::color_math::relative_luminance_table(), rgb, target)
}

#[inline]
const fn onto_luma_with(
    table: &crate::color_math::RelativeLuminanceTable,
    rgb: u32,
    target: f32,
) -> u32 {
    let r = (rgb >> 16) & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = rgb & 0xff;
    let mx = if r > g { r } else { g };
    let mx = if mx > b { mx } else { b };
    if mx == 0 {
        return solve_for_luma(table, rgb, 1.0, true, target);
    }
    let k_full = 255.0 / mx as f32;
    let full = scale_rgb(rgb, k_full);
    if crate::color_math::relative_luminance_with(table, full) >= target {
        solve_for_luma(table, rgb, k_full, false, target)
    } else {
        solve_for_luma(table, full, 1.0, true, target)
    }
}

/// The dim theme's bed `bed`: its colour at the page's own luminance
/// ([`crate::cursor_glow::page_lit_ink`]); `bed` unchanged on a theme whose
/// text `fg` reads past [`DIM_THEME_BARE`]. A dim theme's letter keeps 97%
/// of its own reading under the lights, which leaves a bed no light to lay
/// under it. Held instead until each channel's larger of bed and page kept
/// a letter's edge at four fifths of the theme's reading, the bed scaled
/// toward black: Solarized Dark's ribbons laid maroon-black blocks behind
/// the letters (`#150301` on `#002b36`) and the flat rainbow no rainbow at
/// all (looks7 round 2). The bed's edge is a soft ramp, so the judge's ring
/// at a letter crossing it reads a ramp's step, not each channel's larger
/// of bed and page.
#[inline]
#[must_use]
pub const fn dim_page_bed(bed: u32, fg: u32, page: u32) -> u32 {
    if !dim_theme(fg, page) {
        return bed;
    }
    crate::cursor_glow::page_lit_ink(bed & 0x00FF_FFFF, page & 0x00FF_FFFF)
}

/// Whether text `fg` on the dark page `page` reads under [`DIM_THEME_BARE`].
#[inline]
#[must_use]
pub const fn dim_theme(fg: u32, page: u32) -> bool {
    let table = crate::color_math::relative_luminance_table();
    let (fg, page) = (fg & 0x00FF_FFFF, page & 0x00FF_FFFF);
    let (lf, lp) = (
        crate::color_math::relative_luminance_with(table, fg),
        crate::color_math::relative_luminance_with(table, page),
    );
    lf > lp && (lf + 0.05) / (lp + 0.05) < DIM_THEME_BARE
}

/// [`dim_page_bed`]'s forerunner (a test's negative control): the bed scaled
/// toward black until each channel's larger of it and the page keeps a
/// letter's edge (the judge's solid share of the text's reading) at four
/// fifths of the theme's own.
#[cfg(test)]
#[inline]
pub(crate) const fn dim_edge_held(bed: u32, fg: u32, page: u32) -> u32 {
    const DIM_EDGE_GUARD: f32 = 0.03;
    const DIM_EDGE_SOLID: f32 = 0.85;
    #[inline]
    const fn chmax(c: u32, page: u32) -> u32 {
        #[inline]
        const fn ch(c: u32, page: u32, sh: u32) -> u32 {
            let ca = (c >> sh) & 0xff;
            let pa = (page >> sh) & 0xff;
            let mx = if ca > pa { ca } else { pa };
            mx << sh
        }
        ch(c, page, 16) | ch(c, page, 8) | ch(c, page, 0)
    }
    let table = crate::color_math::relative_luminance_table();
    let (fg, page) = (fg & 0x00FF_FFFF, page & 0x00FF_FFFF);
    let (lf, lp) = (
        crate::color_math::relative_luminance_with(table, fg),
        crate::color_math::relative_luminance_with(table, page),
    );
    if !(lf > lp && (lf + 0.05) / (lp + 0.05) < DIM_THEME_BARE) {
        return bed;
    }
    let edge = (lp + 0.05) * DIM_EDGE_SOLID / (PAPER_BAR_THEME_SHARE + DIM_EDGE_GUARD) - 0.05;
    let hi0 = bed & 0x00FF_FFFF;
    if crate::color_math::relative_luminance_with(table, chmax(hi0, page)) <= edge {
        return bed;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (None, Some(hi0));
    let mut step = 0;
    while step < 24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = scale_rgb(bed, mid);
        if matches!(lo_rgb, Some(prev) if prev == mid_rgb) {
            lo = mid;
        } else if matches!(hi_rgb, Some(prev) if prev == mid_rgb) {
            hi = mid;
        } else if crate::color_math::relative_luminance_with(table, chmax(mid_rgb, page)) <= edge {
            lo = mid;
            lo_rgb = Some(mid_rgb);
        } else {
            hi = mid;
            hi_rgb = Some(mid_rgb);
        }
        step += 1;
    }
    match lo_rgb {
        Some(rgb) => rgb,
        None => scale_rgb(bed, lo),
    }
}

/// The bed's on-glass ink: the stop, its hue bent off the dark bed's mud
/// ([`DARK_HUE_BEND`]), held to the letters' luminance budget.
#[inline]
#[must_use]
pub const fn bed_ink(rgb: u32, budget: f32) -> u32 {
    onto_luma(
        dark_hue_bent(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR)),
        budget,
    )
}

/// [`bed_ink`] without the dark hue bend (a test's control).
#[cfg(test)]
#[inline]
pub(crate) const fn bed_ink_unbent(rgb: u32, budget: f32) -> u32 {
    onto_luma(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR), budget)
}

/// Held to the letters' luminance budget, every hue from orange to
/// yellow-green has no vivid colour left in sRGB, and unbent, a third of the
/// walk draws the dark bed in brown and olive mud. The bend draws orange as a
/// red-orange and yellow as a deep orange, both as vivid as the budget
/// allows, and crosses the basin to green inside the yellow→green interval,
/// as the light bed's [`PAPER_HUE_BEND`] bends yellow off the paper. Degrees
/// in → out.
const DARK_HUE_BEND: [(f32, f32); 4] = [(0.0, 0.0), (30.0, 14.0), (60.0, 24.0), (90.0, 92.0)];

#[inline]
#[must_use]
const fn dark_hue_bent(rgb: u32) -> u32 {
    let (h, s, v) = rgb2hsv(rgb);
    if v <= 0.0 || s <= 0.0 || !(h >= 0.0 && h < 90.0) {
        return rgb;
    }
    let bent = bent_hue(h, &DARK_HUE_BEND);
    if bent == h { rgb } else { hsv2rgb(bent, s, v) }
}

/// The bed's light twin, its ink on paper.
#[cfg(test)]
#[inline]
#[must_use]
pub(crate) const fn paper_ink(rgb: u32, fg: u32, bg: u32, role: InkRole) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let theme = PaperTheme::new(table, fg, bg, role);
    paper_ink_with_theme(table, rgb, &theme)
}

#[derive(Clone, Copy)]
pub(crate) struct PaperTheme {
    bg: u32,
    bg_lab: [f32; 3],
    y_fg: f32,
    cap: f32,
    weight_sq: f32,
    bar: Option<f32>,
}

impl PaperTheme {
    const ZERO: Self = Self {
        bg: 0,
        bg_lab: [0.0; 3],
        y_fg: 0.0,
        cap: 0.0,
        weight_sq: 0.0,
        bar: None,
    };

    #[inline]
    #[must_use]
    pub(crate) const fn new(
        table: &crate::color_math::RelativeLuminanceTable,
        fg: u32,
        bg: u32,
        role: InkRole,
    ) -> Self {
        let bg = bg & 0x00FF_FFFF;
        let cap = (role.alpha_cap() / 255.0).clamp(0.0, 1.0);
        let y_fg = crate::color_math::relative_luminance_with(table, fg);
        let (weight, bar) = match role {
            InkRole::OverText => {
                let y_bg = crate::color_math::relative_luminance_with(table, bg);
                let ratio_bg_fg = (y_bg.max(y_fg) + 0.05) / (y_bg.min(y_fg) + 0.05);
                (
                    PAPER_WEIGHT,
                    Some(
                        (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD)
                            .min(PAPER_BAR_THEME_SHARE * ratio_bg_fg),
                    ),
                )
            }
            InkRole::Leading => (PAPER_WEIGHT_LEADING, None),
        };
        Self {
            bg,
            bg_lab: oklab(bg),
            y_fg,
            cap,
            weight_sq: weight * weight,
            bar,
        }
    }
}

#[inline]
#[must_use]
pub(crate) const fn paper_ink_with_theme(
    table: &crate::color_math::RelativeLuminanceTable,
    rgb: u32,
    theme: &PaperTheme,
) -> u32 {
    #[inline]
    const fn ratio(y: f32, y_fg: f32) -> f32 {
        (y.max(y_fg) + 0.05) / (y.min(y_fg) + 0.05)
    }
    #[inline]
    const fn worst_lum(
        table: &crate::color_math::RelativeLuminanceTable,
        ink: u32,
        bg: u32,
        cap: f32,
    ) -> f32 {
        #[inline]
        const fn ch(ink: u32, bg: u32, cap: f32, sh: u32) -> usize {
            let (i, b) = (((ink >> sh) & 0xff) as f32, ((bg >> sh) & 0xff) as f32);
            (b + (i - b) * cap - 1.0).max(0.0) as usize
        }
        crate::color_math::relative_luminance_rgb(
            table,
            ch(ink, bg, cap, 16),
            ch(ink, bg, cap, 8),
            ch(ink, bg, cap, 0),
        )
    }
    let bg = theme.bg;
    let bg_lab = theme.bg_lab;
    let y_fg = theme.y_fg;
    let cap = theme.cap;
    let weight_sq = theme.weight_sq;
    let bar = theme.bar;
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    let r = (rgb >> 16) & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = rgb & 0xff;
    let mx = if r > g { r } else { g };
    let mx = if mx > b { mx } else { b };
    if mx == 0 {
        return bg;
    }
    let full = scale_rgb(rgb, 255.0 / mx as f32);
    let hi0 = (lerp_rgb(bg, full, cap), full);
    let hi0_ok = match bar {
        Some(bar) => ratio(worst_lum(table, hi0.1, bg, cap), y_fg) >= bar,
        None => true,
    } && {
        let lab0 = oklab(hi0.0);
        let (dl0, da0, db0) = (
            lab0[0] - bg_lab[0],
            lab0[1] - bg_lab[1],
            lab0[2] - bg_lab[2],
        );
        dl0 * dl0 + da0 * da0 + db0 * db0 <= weight_sq
    };
    if hi0_ok {
        return hi0.1;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let mut lo_pair: Option<(u32, u32)> = None;
    let mut hi_pair = Some(hi0);
    let mut step = 0;
    while step < 24 {
        let mid = 0.5 * (lo + hi);
        let pair = (lerp_rgb(bg, full, mid * cap), lerp_rgb(bg, full, mid));
        let same_lo = match lo_pair {
            Some(prev) => prev.0 == pair.0 && prev.1 == pair.1,
            None => false,
        };
        let same_hi = match hi_pair {
            Some(prev) => prev.0 == pair.0 && prev.1 == pair.1,
            None => false,
        };
        if same_lo {
            lo = mid;
        } else if same_hi {
            hi = mid;
        } else {
            let ok = match bar {
                Some(bar) => ratio(worst_lum(table, pair.1, bg, cap), y_fg) >= bar,
                None => true,
            } && {
                let lab = oklab(pair.0);
                let (dl, da, db) = (lab[0] - bg_lab[0], lab[1] - bg_lab[1], lab[2] - bg_lab[2]);
                dl * dl + da * da + db * db <= weight_sq
            };
            if ok {
                lo = mid;
                lo_pair = Some(pair);
            } else {
                hi = mid;
                hi_pair = Some(pair);
            }
        }
        step += 1;
    }
    match lo_pair {
        Some((_, ink)) => ink,
        None => lerp_rgb(bg, full, lo),
    }
}

/// The caret's ink on paper.
#[inline]
#[must_use]
pub const fn paper_caret_ink(rgb: u32, bg: u32) -> u32 {
    paper_vivid_ink_bent(rgb, bg, PAPER_CARET_CONTRAST, &PAPER_CARET_HUE_BEND)
}

#[inline]
#[must_use]
pub(crate) const fn paper_caret_ink_with_bg_lum(
    table: &crate::color_math::RelativeLuminanceTable,
    rgb: u32,
    y_bg: f32,
) -> u32 {
    let (h, s, v) = rgb2hsv(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR));
    if v <= 0.0 {
        return 0;
    }
    paper_vivid_ink_hsv_with_bg_lum(
        table,
        bent_hue(h, &PAPER_HUE_BEND),
        s,
        y_bg,
        PAPER_CARET_CONTRAST,
    )
}

/// The caret's colour for the walk's stop `arc`.
#[inline]
#[must_use]
pub(crate) const fn caret_ink(arc: u32, cfg: &Config) -> u32 {
    if cfg.dark_theme {
        arc
    } else {
        paper_caret_ink(arc, cfg.theme_bg)
    }
}

/// A point mark's ink on paper.
#[inline]
#[must_use]
pub const fn paper_mark_ink(rgb: u32, bg: u32) -> u32 {
    paper_vivid_ink(rgb, bg, PAPER_MARK_CONTRAST)
}

#[inline]
#[must_use]
pub(crate) const fn paper_mark_ink_with_bg_lum(
    table: &crate::color_math::RelativeLuminanceTable,
    rgb: u32,
    y_bg: f32,
) -> u32 {
    let (h, s, v) = rgb2hsv(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR));
    if v <= 0.0 {
        return 0;
    }
    paper_vivid_ink_hsv_with_bg_lum(
        table,
        bent_hue(h, &PAPER_HUE_BEND),
        s,
        y_bg,
        PAPER_MARK_CONTRAST,
    )
}

/// A paper train's ink (a Return's stick): the caret's ink, its walk from
/// gold to green stepping over the darkened yellows
/// ([`PAPER_TRAIN_HUE_BEND`]), its orange and gold held to a graphic's bar
/// ([`PAPER_TRAIN_GOLD_CONTRAST`]) so they read gold rather than ochre.
#[inline]
#[must_use]
pub const fn paper_train_ink(rgb: u32, bg: u32) -> u32 {
    let (h, s, v) = rgb2hsv(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR));
    if v <= 0.0 {
        return 0;
    }
    let h = bent_hue(h, &PAPER_TRAIN_HUE_BEND);
    paper_vivid_ink_hsv(h, s, bg, paper_train_bar(h))
}

#[inline]
#[must_use]
pub(crate) const fn paper_train_ink_with_bg_lum(
    table: &crate::color_math::RelativeLuminanceTable,
    rgb: u32,
    y_bg: f32,
) -> u32 {
    let (h, s, v) = rgb2hsv(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR));
    if v <= 0.0 {
        return 0;
    }
    let h = bent_hue(h, &PAPER_TRAIN_HUE_BEND);
    paper_vivid_ink_hsv_with_bg_lum(table, h, s, y_bg, paper_train_bar(h))
}

/// The contrast bar a paper train's ink of bent hue `h` keeps: the gold's
/// over [`PAPER_TRAIN_GOLD_HUES`], the caret's away from it.
#[inline]
const fn paper_train_bar(h: f32) -> f32 {
    let (lo, hi) = PAPER_TRAIN_GOLD_HUES;
    let out = (lo - h).max(h - hi).max(0.0) * (1.0 / PAPER_TRAIN_GOLD_EASE);
    let u = if out < 1.0 { out } else { 1.0 };
    let gold = 1.0 - u * u * (3.0 - 2.0 * u);
    PAPER_CARET_CONTRAST + (PAPER_TRAIN_GOLD_CONTRAST - PAPER_CARET_CONTRAST) * gold
}

#[inline]
const fn paper_vivid_ink(rgb: u32, bg: u32, bar: f32) -> u32 {
    paper_vivid_ink_bent(rgb, bg, bar, &PAPER_HUE_BEND)
}

/// The hue `h` walked along `bend`'s knots (degrees in -> out); off the
/// bend's span no knot pair holds it and it is left as it is.
#[inline]
const fn bent_hue(h: f32, bend: &[(f32, f32)]) -> f32 {
    let mut i = 0;
    while i + 1 < bend.len() {
        let (x0, y0) = bend[i];
        let (x1, y1) = bend[i + 1];
        if h >= x0 && h < x1 {
            return y0 + (y1 - y0) * (h - x0) / (x1 - x0);
        }
        i += 1;
    }
    h
}

#[inline]
const fn paper_vivid_ink_bent(rgb: u32, bg: u32, bar: f32, bend: &[(f32, f32)]) -> u32 {
    let (h, s, v) = rgb2hsv(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR));
    if v <= 0.0 {
        return 0;
    }
    paper_vivid_ink_hsv(bent_hue(h, bend), s, bg, bar)
}

#[inline]
const fn paper_vivid_ink_hsv(h: f32, s: f32, bg: u32, bar: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let y_bg = crate::color_math::relative_luminance_with(table, bg);
    paper_vivid_ink_hsv_with_bg_lum(table, h, s, y_bg, bar)
}

#[inline]
const fn paper_vivid_ink_hsv_with_bg_lum(
    table: &crate::color_math::RelativeLuminanceTable,
    h: f32,
    s: f32,
    y_bg: f32,
    bar: f32,
) -> u32 {
    let full = hsv2rgb(h, s, 1.0);
    #[inline]
    const fn holds(
        table: &crate::color_math::RelativeLuminanceTable,
        y_bg: f32,
        bar: f32,
        cand: u32,
    ) -> bool {
        let y = crate::color_math::relative_luminance_with(table, cand);
        (y.max(y_bg) + 0.05) / (y.min(y_bg) + 0.05) >= bar
    }
    if holds(table, y_bg, bar, full) {
        return full;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (None, full);
    let mut i = 0;
    while i < 24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = scale_rgb(full, mid);
        let same_lo = match lo_rgb {
            Some(prev) => prev == mid_rgb,
            None => false,
        };
        if mid_rgb == hi_rgb {
            hi = mid;
        } else if same_lo || holds(table, y_bg, bar, mid_rgb) {
            lo = mid;
            lo_rgb = Some(mid_rgb);
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        i += 1;
    }
    match lo_rgb {
        Some(rgb) => rgb,
        None => 0,
    }
}

#[inline]
const fn channel_contrast_lin(a_lin: f32, b_lin: f32) -> f32 {
    let (hi, lo) = if a_lin >= b_lin {
        (a_lin, b_lin)
    } else {
        (b_lin, a_lin)
    };
    (hi + 0.05) / (lo + 0.05)
}

#[inline]
#[must_use]
pub(crate) const fn bg_linear_rgb(linear: &[f32; 256], bg: u32) -> (f32, f32, f32) {
    (
        linear[((bg >> 16) & 0xff) as usize],
        linear[((bg >> 8) & 0xff) as usize],
        linear[(bg & 0xff) as usize],
    )
}

/// Whether a composited pixel is still a stop.
#[cfg(test)]
#[inline]
#[must_use]
pub(crate) const fn seen(rgb: u32, bg: u32) -> bool {
    let linear = &crate::color_math::relative_luminance_table().linear;
    seen_with_bg_lin(linear, rgb, bg_linear_rgb(linear, bg))
}

#[inline]
#[must_use]
pub(crate) const fn seen_with_bg_lin(
    linear: &[f32; 256],
    rgb: u32,
    bg_lin: (f32, f32, f32),
) -> bool {
    let (r, g, b) = (
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    );
    let mx = if r > g { r } else { g };
    let mx = if mx > b { mx } else { b };
    let mn = if r < g { r } else { g };
    let mn = if mn < b { mn } else { b };
    let spread = (mx - mn) as f32;
    if spread < SEEN_SPREAD {
        return false;
    }
    let stand = channel_contrast_lin(linear[r as usize], bg_lin.0)
        .max(channel_contrast_lin(linear[g as usize], bg_lin.1))
        .max(channel_contrast_lin(linear[b as usize], bg_lin.2));
    stand >= SEEN_CONTRAST
}

#[inline]
#[must_use]
const fn seen_floor_with_bg_lin(
    linear: &[f32; 256],
    ink: u32,
    bg: u32,
    bg_lin: (f32, f32, f32),
) -> f32 {
    let mut a = 255u16;
    while a >= 1 {
        let alpha = a as u8;
        if !seen_with_bg_lin(
            linear,
            over_premul(bg, premul_rgb(ink, alpha), alpha),
            bg_lin,
        ) {
            return (alpha as f32) + 1.0;
        }
        a -= 1;
    }
    1.0
}

/// The crisp edge's reach, priced at birth.
#[inline]
#[must_use]
pub const fn edge_cells(surge: f32, heat: f32) -> f32 {
    let stretch = clamp01(surge).max(clamp01(heat));
    HOT_EDGE_CELLS + (HOT_EDGE_CELLS_MAX - HOT_EDGE_CELLS) * stretch
}

/// The hot edge's ink.
#[inline]
#[must_use]
pub const fn hot_edge_ink(rgb: u32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    if crate::color_math::relative_luminance_with(table, rgb) >= HOT_EDGE_LUMA_FLOOR {
        rgb
    } else {
        onto_luma_with(table, rgb, HOT_EDGE_LUMA_FLOOR)
    }
}

/// The vivid rail's ink on a dark page.
#[inline]
#[must_use]
pub const fn rail_ink(rgb: u32) -> u32 {
    rail_ink_over(rgb, RAIL_LUMA_FLOOR)
}

/// The vivid rail's ink on a light page: [`rail_ink`] over [`PAPER_RAIL_LUMA_FLOOR`].
#[inline]
#[must_use]
pub const fn paper_rail_ink(rgb: u32) -> u32 {
    rail_ink_over(rgb, PAPER_RAIL_LUMA_FLOOR)
}

/// The rail's ink over the luminance `floor`: `rgb` at full channel, lifted toward white to the
/// floor under it and lowered to [`RAIL_LUMA_CEIL`] over it.
#[inline]
#[must_use]
const fn rail_ink_over(rgb: u32, floor: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    let r = (rgb >> 16) & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = rgb & 0xff;
    let mx = if r > g { r } else { g };
    let mx = if mx > b { mx } else { b };
    if mx == 0 {
        return onto_luma_with(table, rgb, floor);
    }
    let full = scale_rgb(rgb, 255.0 / mx as f32);
    let y = crate::color_math::relative_luminance_with(table, full);
    if y < floor {
        onto_luma_with(table, full, floor)
    } else if y > RAIL_LUMA_CEIL {
        onto_luma_with(table, full, RAIL_LUMA_CEIL)
    } else {
        full
    }
}

/// The light theme's compositing role for the body (§3.3, L6).
#[inline]
#[must_use]
pub(crate) const fn light_role(cfg: &Config) -> InkRole {
    if cfg.ribbon_tall {
        InkRole::OverText
    } else {
        InkRole::Leading
    }
}

/// The ceiling a body's coverage is planned under: [`GLASS_ALPHA`] on a frame the bed draws by
/// its `glass` (§15.2 1.7), main's [`cov_cap`] on every other.
#[inline]
#[must_use]
const fn body_cov_cap(cfg: &Config, glass: bool) -> f32 {
    if glass { GLASS_ALPHA } else { cov_cap(cfg) }
}

#[inline]
#[must_use]
const fn cov_cap(cfg: &Config) -> f32 {
    if cfg.dark_theme {
        UNDER_COV_CAP
    } else {
        light_role(cfg).alpha_cap()
    }
}

/// Clamp a requested occupied-cell bed coverage into `[0.0, body_cov_cap(cfg, glass)]`.
#[inline]
#[must_use]
pub(crate) const fn clamp_bed_cov(cfg: &Config, glass: bool, requested: f32) -> f32 {
    let cap = body_cov_cap(cfg, glass);
    if requested.is_nan() || requested <= 0.0 {
        0.0
    } else if requested >= cap {
        cap
    } else {
        requested
    }
}

#[inline]
const fn walk_arc_at(i: usize) -> u32 {
    spectrum(walk_pace(i as f32 * (1.0 / (BED_INK_LUT_LEN - 1) as f32)))
}

static HOT_EDGE_INK_LUT: [u32; BED_INK_LUT_LEN] = {
    let mut table = [0u32; BED_INK_LUT_LEN];
    let mut i = 0;
    while i < BED_INK_LUT_LEN {
        table[i] = hot_edge_ink(walk_arc_at(i));
        i += 1;
    }
    table
};

/// Shared 129-stop hot-edge ink LUT across the ribbon and meteor landing marks.
#[inline]
#[must_use]
pub(crate) const fn hot_edge_ink_lut() -> &'static [u32; BED_INK_LUT_LEN] {
    &HOT_EDGE_INK_LUT
}

static RAIL_INK_LUT: [u32; BED_INK_LUT_LEN] = {
    let mut table = [0u32; BED_INK_LUT_LEN];
    let mut i = 0;
    while i < BED_INK_LUT_LEN {
        table[i] = rail_ink(walk_arc_at(i));
        i += 1;
    }
    table
};

#[inline]
const fn rail_ink_lut() -> &'static [u32; BED_INK_LUT_LEN] {
    &RAIL_INK_LUT
}

static PAPER_RAIL_INK_LUT: [u32; BED_INK_LUT_LEN] = {
    let mut table = [0u32; BED_INK_LUT_LEN];
    let mut i = 0;
    while i < BED_INK_LUT_LEN {
        table[i] = paper_rail_ink(walk_arc_at(i));
        i += 1;
    }
    table
};

#[inline]
const fn paper_rail_ink_lut() -> &'static [u32; BED_INK_LUT_LEN] {
    &PAPER_RAIL_INK_LUT
}

static DARK_BED_MAX_LUT: [u32; BED_INK_LUT_LEN] = {
    let mut table = [0u32; BED_INK_LUT_LEN];
    let mut i = 0;
    while i < BED_INK_LUT_LEN {
        table[i] = bed_ink(walk_arc_at(i), BED_LUMA_MAX);
        i += 1;
    }
    table
};

#[inline]
const fn dark_bed_max_lut() -> &'static [u32; BED_INK_LUT_LEN] {
    &DARK_BED_MAX_LUT
}

static DARK_SEEN_COMMON_LUT: [(u32, [f32; BED_INK_LUT_LEN]); 5] = {
    let linear = &crate::color_math::relative_luminance_table().linear;
    let grounds = [
        0x0000_0000,
        0x0010_1016,
        0x0011_1318,
        0x0016_161C,
        0x001A_1B26,
    ];
    let mut out = [(0u32, [0.0f32; BED_INK_LUT_LEN]); 5];
    let mut g_idx = 0;
    while g_idx < 5 {
        let g = grounds[g_idx];
        let g_lin = bg_linear_rgb(linear, g);
        let mut seen = [0.0f32; BED_INK_LUT_LEN];
        let mut i = 0;
        while i < BED_INK_LUT_LEN {
            seen[i] = seen_floor_with_bg_lin(linear, DARK_BED_MAX_LUT[i], g, g_lin);
            i += 1;
        }
        out[g_idx] = (g, seen);
        g_idx += 1;
    }
    out
};

#[inline]
const fn dark_seen_common_lut(bg: u32) -> Option<&'static [f32; BED_INK_LUT_LEN]> {
    let bg = bg & 0x00FF_FFFF;
    let mut i = 0;
    while i < DARK_SEEN_COMMON_LUT.len() {
        if DARK_SEEN_COMMON_LUT[i].0 == bg {
            return Some(&DARK_SEEN_COMMON_LUT[i].1);
        }
        i += 1;
    }
    None
}

/// The bed's ink table for one theme, rebuilt only when the theme or spelling changes.
///
/// Under Prism Glass's glass (`docs/design/PRISM-GLASS.md` §15.2 1.5: `glass_lit`, a light frame's
/// tall spelling on a page whose solved palette has headroom) the bed draws by that palette's
/// glass129 instead ([`Self::glass`]), filled when the glass lights or the page changes. Main's
/// own table is still built for the theme beside it, byte for byte, and is what [`Self::key`],
/// [`Self::lut`] and [`Self::seen`] read on every frame.
#[derive(Clone, Copy, Debug)]
pub struct BedInkLut {
    key: Option<(u32, u32, bool, bool)>,
    lut: [u32; BED_INK_LUT_LEN],
    seen: [f32; BED_INK_LUT_LEN],
    /// The glass table the bed draws by under `glass_lit`; `None` on every other frame.
    glass: Option<GlassBed>,
}

/// A light page's glass129 as the bed draws it (§15.2 1.5), light only, and the lip129 its rail
/// is laid in.
#[derive(Clone, Copy, Debug)]
struct GlassBed {
    /// The theme fg and page it was filled for.
    pair: (u32, u32),
    /// The palette's glass129.
    lut: [u32; BED_INK_LUT_LEN],
    /// What of each glass the page shows.
    seen: [f32; BED_INK_LUT_LEN],
    /// The palette's lip129: the DEEP tone under its glass (`docs/design/PRISM-GLASS.md` §1),
    /// the rail's ink on a glass frame ([`BedInkLut::rail_at`]).
    lip: [u32; BED_INK_LUT_LEN],
    /// The lip's pairs `(k, k + 1)` its sampler takes the nearer entry across instead of
    /// lerping ([`BedInkLut::sample_snapped`]): the gold|green fold (`Palette::fold_at`), and
    /// the cyan waypoint where the palette snaps there (`Palette::cyan_snap`).
    lip_snaps: [Option<u8>; 2],
}

#[cfg(test)]
thread_local! {
    /// A negative control's seam: the flat spelling lights the glass as the comet does, as it
    /// did from §15.2 1.5 until composed O11(a) ([`glass_table`]).
    static FLAT_GLASS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The glass129 the bed draws a frame by (`glass_lit`, §15.0.4, §15.2 1.5): on a light frame
/// whose light draws glass, the tall comet spelling, a page whose palette `palettes` holds solved
/// (the frame advances a pending solve by its counted units) and which has headroom. A dark
/// frame, the underline and flat spellings and a light below the glass level never ask the
/// cache, so no solve unit runs there.
///
/// The flat spelling keeps main's paper bed, its soft even band (looks9 Q6), as the underline
/// spelling does (composed O11(a)): under the glass it laid the comet's own body at the comet's
/// height and colours, its taper the only difference (§15.2 1.7: 0.96 ch against 0.86 ch twelve
/// cells back), and on GitHub Light 969 px of the ribbon's box told the two apart where looks10's
/// paper told 7684.
#[inline]
fn glass_table<'p>(
    cfg: &Config,
    palettes: &'p mut PaletteCache,
) -> Option<&'p super::prism_glass::Palette> {
    #[cfg(test)]
    let flat = cfg.ribbon_flat && !FLAT_GLASS.with(std::cell::Cell::get);
    #[cfg(not(test))]
    let flat = cfg.ribbon_flat;
    if !cfg.light.glass() || !cfg.ribbon_tall || flat || cfg.dark_theme {
        return None;
    }
    palettes
        .frame(cfg.theme_fg, cfg.theme_bg)
        .filter(|p| p.headroom())
}

impl Default for BedInkLut {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl BedInkLut {
    /// Unsynced bed ink lookup table.
    pub const EMPTY: Self = Self {
        key: None,
        lut: [0; BED_INK_LUT_LEN],
        seen: [0.0; BED_INK_LUT_LEN],
        glass: None,
    };

    #[inline]
    fn sync(&mut self, cfg: &Config) {
        let key = (cfg.theme_fg, cfg.theme_bg, cfg.dark_theme, cfg.ribbon_tall);
        if let Some((fg, bg, dark, tall)) = self.key
            && fg == key.0
            && bg == key.1
            && dark == key.2
            && tall == key.3
        {
            return;
        }
        let budget = bed_luma_budget(cfg.theme_fg);
        let role = light_role(cfg);
        let dark_max = if cfg.dark_theme && (budget - BED_LUMA_MAX).abs() == 0.0 {
            Some(dark_bed_max_lut())
        } else {
            None
        };
        if let Some(max_lut) = dark_max
            && let Some(seen_lut) = dark_seen_common_lut(cfg.theme_bg)
        {
            self.lut = *max_lut;
            self.seen = *seen_lut;
            self.key = Some(key);
            return;
        }
        let table = crate::color_math::relative_luminance_table();
        let linear = &table.linear;
        let bg_lin = bg_linear_rgb(linear, cfg.theme_bg);
        let light_a = (cov_cap(cfg) + 16.0).min(255.0) as u8;
        let is_dim_theme = cfg.dark_theme && dim_theme(cfg.theme_fg, cfg.theme_bg);
        let bg_masked = cfg.theme_bg & 0x00FF_FFFF;
        let paper_theme = if cfg.dark_theme {
            PaperTheme::ZERO
        } else {
            PaperTheme::new(table, cfg.theme_fg, bg_masked, role)
        };
        let mut i = 0;
        while i < BED_INK_LUT_LEN {
            let bed = if let Some(max_lut) = dark_max {
                max_lut[i]
            } else {
                let arc = walk_arc_at(i);
                if cfg.dark_theme {
                    let raw = bed_ink(arc, budget);
                    if is_dim_theme {
                        crate::cursor_glow::page_lit_ink(raw & 0x00FF_FFFF, bg_masked)
                    } else {
                        raw
                    }
                } else {
                    let paper = paper_ink_with_theme(table, arc, &paper_theme);
                    let shows = seen_with_bg_lin(
                        linear,
                        over_premul(cfg.theme_bg, premul_rgb(paper, light_a), light_a)
                            & 0x00FF_FFFF,
                        bg_lin,
                    );
                    if cfg.ribbon_tall && !shows {
                        crate::cursor_glow::light_body_ink(arc, cfg.theme_fg, cfg.theme_bg, light_a)
                    } else {
                        paper
                    }
                }
            };
            self.lut[i] = bed;
            self.seen[i] = seen_floor_with_bg_lin(linear, bed, cfg.theme_bg, bg_lin);
            i += 1;
        }
        self.key = Some(key);
    }

    /// Sync the glass to the frame: under `glass_lit`, the glass129 it draws by and the lip129
    /// its rail is laid in, the palette's ([`glass_table`]), filled when the glass lights or the
    /// page changes; `None` on every other frame. Main's table ([`Self::sync`]) is synced beside
    /// it.
    #[inline]
    fn sync_glass(&mut self, cfg: &Config, palette: Option<&super::prism_glass::Palette>) {
        let Some(palette) = palette else {
            self.glass = None;
            return;
        };
        let table = &palette.glass;
        let pair = (cfg.theme_fg, cfg.theme_bg);
        if self.glass.as_ref().is_some_and(|g| g.pair == pair) {
            return;
        }
        let linear = &crate::color_math::relative_luminance_table().linear;
        let bg_lin = bg_linear_rgb(linear, cfg.theme_bg);
        let mut seen = [0.0; BED_INK_LUT_LEN];
        let mut i = 0;
        while i < BED_INK_LUT_LEN {
            seen[i] = seen_floor_with_bg_lin(linear, table[i], cfg.theme_bg, bg_lin);
            i += 1;
        }
        self.glass = Some(GlassBed {
            pair,
            lut: *table,
            seen,
            lip: palette.lip,
            lip_snaps: [
                Some(palette.fold_at),
                if palette.cyan_snap {
                    Some(super::prism_glass::CYAN_AT as u8)
                } else {
                    None
                },
            ],
        });
    }

    /// The table a ribbon's frame syncs for `cfg`, built from empty with an empty palette
    /// cache: what the host unreachability pin holds each frame's table to
    /// (`docs/design/PRISM-GLASS.md` §15.1 P0.9). Debug and test builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub fn synced(cfg: &Config) -> Self {
        let mut lut = Self::EMPTY;
        let mut palettes = PaletteCache::new();
        lut.sync(cfg);
        lut.sync_glass(cfg, glass_table(cfg, &mut palettes));
        lut
    }

    /// Whether the bed draws by the glass (`glass_lit`, §15.2 1.5).
    #[inline]
    #[must_use]
    pub const fn glass_lit(&self) -> bool {
        self.glass.is_some()
    }

    /// The glass129 the bed draws by under `glass_lit`; `None` on every other frame.
    #[inline]
    #[must_use]
    pub const fn glass(&self) -> Option<&[u32; BED_INK_LUT_LEN]> {
        match &self.glass {
            Some(g) => Some(&g.lut),
            None => None,
        }
    }

    /// The light words (§15.0.2(a)): under the glass, the page it was filled for, its inks and
    /// what of each the page shows; none on any other frame. Never compared. Debug and test
    /// builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn light_words(&self, w: &mut impl crate::state_words::Words) {
        use crate::state_words::Words as _;
        let Some(g) = &self.glass else {
            return;
        };
        w.put("bed.glass.fg", u64::from(g.pair.0));
        w.put("bed.glass.bg", u64::from(g.pair.1));
        w.list("bed.glass.lut", g.lut, |f, ink| {
            f.put("ink", u64::from(ink))
        });
        w.list("bed.glass.seen", g.seen, |f, seen| f.f32("seen", seen));
    }

    /// The ink at walk position `t`, folded through `meteor::tri`, so the station under the caret is the caret's stop by construction.
    #[inline]
    #[must_use]
    pub const fn at(&self, t: f32) -> u32 {
        if self.key.is_none() {
            return 0;
        }
        match &self.glass {
            Some(g) => Self::sample(&g.lut, t),
            None => Self::sample(&self.lut, t),
        }
    }

    /// The hot edge's ink at walk position `t`.
    #[inline]
    #[must_use]
    pub const fn hot_at(&self, t: f32) -> u32 {
        if self.key.is_none() {
            return 0;
        }
        Self::sample(hot_edge_ink_lut(), t)
    }

    /// The vivid rail's ink at walk position `t`: under the glass the lip's
    /// (`docs/design/PRISM-GLASS.md` §1, DEEP tone: at least .10 OKLab L under the glass at the
    /// same walk, so the line reads on the glass it lies over), on every other light frame the
    /// paper's ([`paper_rail_ink`]) and on a dark one main's ([`rail_ink`]).
    #[inline]
    #[must_use]
    pub const fn rail_at(&self, t: f32) -> u32 {
        let Some((_, _, dark, _)) = self.key else {
            return 0;
        };
        match &self.glass {
            Some(g) => Self::sample_snapped(&g.lip, g.lip_snaps, t),
            None if dark => Self::sample(rail_ink_lut(), t),
            None => Self::sample(paper_rail_ink_lut(), t),
        }
    }

    /// Where the rail's ink from walk position `a` to `b` steps at a snap of the lip
    /// ([`Self::sample_snapped`]): the share of the way from `a` to `b` the step lies at, and the
    /// entries on `a`'s side of it and on `b`'s. A run lays the rail by its vertices, lerping
    /// between them, and vertices a few entries apart on either side of the gold|green fold lerp
    /// through the olive the snap is there to keep out, so the rail is laid as two vertices at the
    /// step. `None` on a frame the glass does not draw, and where no snap lies between them.
    #[inline]
    #[must_use]
    pub const fn rail_step(&self, a: f32, b: f32) -> Option<(f32, u32, u32)> {
        if self.key.is_none() {
            return None;
        }
        let Some(g) = &self.glass else {
            return None;
        };
        let n = (BED_INK_LUT_LEN - 1) as f32;
        let (xa, xb) = (clamp01(tri(a)) * n, clamp01(tri(b)) * n);
        let mut k = 0;
        while k < g.lip_snaps.len() {
            if let Some(at) = g.lip_snaps[k] {
                let i = at as usize;
                let edge = i as f32 + 0.5;
                if i + 1 < BED_INK_LUT_LEN && (xa < edge) != (xb < edge) {
                    let u = (edge - xa) / (xb - xa);
                    return Some(if xa < edge {
                        (u, g.lip[i], g.lip[i + 1])
                    } else {
                        (u, g.lip[i + 1], g.lip[i])
                    });
                }
            }
            k += 1;
        }
        None
    }

    /// The colour floor at walk position `t`.
    #[inline]
    #[must_use]
    pub const fn seen_floor_at(&self, t: f32) -> f32 {
        if self.key.is_none() {
            return 0.0;
        }
        match &self.glass {
            Some(g) => Self::floor_in(&g.seen, t),
            None => Self::floor_in(&self.seen, t),
        }
    }

    /// Main's colour floor at walk position `t`: [`Self::seen_floor_at`] of main's table, which
    /// the glass leaves as it was (§15.2 1.8: main's envelope, and every decision it makes, reads
    /// main's floor on a frame the glass draws).
    #[inline]
    #[must_use]
    pub const fn main_seen_floor_at(&self, t: f32) -> f32 {
        if self.key.is_none() {
            return 0.0;
        }
        Self::floor_in(&self.seen, t)
    }

    /// The floor `seen` holds at walk position `t`.
    #[inline]
    const fn floor_in(seen: &[f32; BED_INK_LUT_LEN], t: f32) -> f32 {
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let idx = x as usize;
        let i = if idx < BED_INK_LUT_LEN - 1 {
            idx
        } else {
            BED_INK_LUT_LEN - 1
        };
        let j = if i + 1 < BED_INK_LUT_LEN - 1 {
            i + 1
        } else {
            BED_INK_LUT_LEN - 1
        };
        seen[i].max(seen[j])
    }

    /// [`Self::sample`], save that across each pair `(k, k + 1)` of `snaps` it takes the nearer
    /// entry instead of lerping (`docs/design/PRISM-GLASS.md` §1, the FOLD SNAP): the lip's gold
    /// and green at the fold lerp through olive.
    #[inline]
    #[must_use]
    const fn sample_snapped(table: &[u32; BED_INK_LUT_LEN], snaps: [Option<u8>; 2], t: f32) -> u32 {
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let idx = x as usize;
        let i = if idx < BED_INK_LUT_LEN - 1 {
            idx
        } else {
            BED_INK_LUT_LEN - 1
        };
        let j = if i + 1 < BED_INK_LUT_LEN - 1 {
            i + 1
        } else {
            BED_INK_LUT_LEN - 1
        };
        let f = x - i as f32;
        let mut k = 0;
        while k < snaps.len() {
            if let Some(at) = snaps[k]
                && at as usize == i
            {
                return if f < 0.5 { table[i] } else { table[j] };
            }
            k += 1;
        }
        lerp_rgb(table[i], table[j], f)
    }

    /// The colour `table` lays at walk position `t`: the bed's sampler, which Prism Glass's scan
    /// scores (`prism_glass::scan`).
    #[inline]
    #[must_use]
    pub(crate) const fn sample(table: &[u32; BED_INK_LUT_LEN], t: f32) -> u32 {
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let idx = x as usize;
        let i = if idx < BED_INK_LUT_LEN - 1 {
            idx
        } else {
            BED_INK_LUT_LEN - 1
        };
        let j = if i + 1 < BED_INK_LUT_LEN - 1 {
            i + 1
        } else {
            BED_INK_LUT_LEN - 1
        };
        lerp_rgb(table[i], table[j], x - i as f32)
    }
}

const CROSS_LUT0: usize = crate::spectrum::SPECTRUM_ANCHOR_AT[3] + CROSS_GUARD;
const CROSS_LUT1: usize = crate::spectrum::SPECTRUM_ANCHOR_AT[4] - CROSS_GUARD;

const CROSS_GUARD: usize = 24;
const CROSS_ARC0: f32 = CROSS_LUT0 as f32 / (crate::spectrum::SPECTRUM_LUT_LEN - 1) as f32;
const CROSS_ARC1: f32 = CROSS_LUT1 as f32 / (crate::spectrum::SPECTRUM_LUT_LEN - 1) as f32;

const CROSS_PACE: [u16; CROSS_PACE_KNOTS] = [
    33539, 33818, 34076, 34375, 34679, 35026, 35233, 35530, 35828, 36087, 36381, 36642, 36900,
    37159, 37456, 37760, 38011, 38231, 38537, 38825, 39085, 39343, 39598, 39897, 40189, 40573,
    40831, 41141, 41399, 41696, 41916, 42169, 42470, 42683, 42939, 43158, 43336, 43542, 43795,
    43969, 44174, 44364, 44569, 44782, 44996, 45199, 45337, 45554, 45715, 45896, 46035, 46197,
    46409, 46552, 46712, 46840, 47051, 47224, 47523, 47738, 47952, 48208, 48423, 48553, 48830,
];

/// Knots in [`CROSS_PACE`].
pub const CROSS_PACE_KNOTS: usize = 65;

/// The arc position of a walk position.
#[inline]
#[must_use]
pub const fn walk_arc(t: f32) -> f32 {
    walk_pace(clamp01(tri(t)))
}

/// Where a walk position lands on the arc.
#[inline]
#[must_use]
pub const fn walk_pace(x: f32) -> f32 {
    let x = clamp01(x);
    if x <= CROSS_ARC0 || x >= CROSS_ARC1 {
        return x;
    }
    const SCALE: f32 = (CROSS_PACE_KNOTS - 1) as f32 / (CROSS_ARC1 - CROSS_ARC0);
    let f = (x - CROSS_ARC0) * SCALE;
    let u = f as usize;
    let i = if u < CROSS_PACE_KNOTS - 2 {
        u
    } else {
        CROSS_PACE_KNOTS - 2
    };
    let (a, b) = (CROSS_PACE[i] as f32, CROSS_PACE[i + 1] as f32);
    (a + (b - a) * (f - i as f32)) * (1.0 / 65535.0)
}

/// The classic walk.
#[inline]
#[must_use]
pub const fn walk_t(d: f32) -> f32 {
    if !d.is_finite() {
        return 0.0;
    }
    if d <= WALK_FAST_CELLS {
        d * (1.0 / WALK_FAST_CELLS)
    } else {
        1.0 + (d - WALK_FAST_CELLS) * WALK_LAY_RATE
    }
}

/// A cell's terminal approach to zero, read on its own clock.
#[inline]
#[must_use]
pub const fn expiry_melt(u: f32) -> f32 {
    let rem = 1.0 - clamp01(u);
    if rem >= EXPIRY_MELT_SHARE {
        return 1.0;
    }
    let taper = smoothstep01(rem * (1.0 / EXPIRY_MELT_SHARE));
    if taper <= 0.0 {
        0.0
    } else if taper >= 1.0 {
        1.0
    } else {
        crate::color_math::powf_pos_f32(taper, EXPIRY_MELT_GAMMA)
    }
}

/// The focus ember's level, per stop.
#[inline]
#[must_use]
pub const fn ember_level(floor: f32, f: f32) -> f32 {
    let floor = floor.max(0.0);
    clamp01(floor + (1.0 - floor) * clamp01(f))
}

/// The one linear fade of the theme (§2.5, §6.11).
#[inline]
#[must_use]
pub const fn reduced_fade(remaining_s: f32) -> f32 {
    clamp01(remaining_s * (1000.0 / REDUCED_MOTION_FADE_MS))
}

/// Seconds until [`expiry_melt`] next takes a mark of `peak` levels down one u8 level, `u` of the way through its `life_s`.
#[inline]
#[must_use]
pub(crate) const fn level_step_melt(peak: f32, u: f32, life_s: f32) -> Option<f32> {
    if !(peak.is_finite() && life_s.is_finite() && u.is_finite()) || peak <= 0.0 || life_s <= 0.0 {
        return None;
    }
    let u = clamp01(u);
    let level = (peak * expiry_melt(u)).round();
    if level < 1.0 {
        return None;
    }
    let taper = crate::color_math::powf_pos_f32(
        ((level - 0.5) / peak).clamp(0.0, 1.0),
        1.0 / EXPIRY_MELT_GAMMA,
    );
    let x = 0.5
        - crate::supernova::sin_cos_f32(
            crate::color_math::asin_f32((1.0 - 2.0 * taper).clamp(-1.0, 1.0)) * (1.0 / 3.0),
        )
        .0;
    let u_next = 1.0 - EXPIRY_MELT_SHARE * x;
    Some(((u_next - u) * life_s).max(0.0))
}

/// How far the glass's oldest end eases in, in cells (`docs/design/PRISM-GLASS.md` §2.2 The oldest
/// end, TAIL_CELLS).
pub const GLASS_TAIL_CELLS: f32 = 2.5;

/// How short a trail keeps some of its glass's height at its oldest end, in cells (§2.2): the
/// hand's own height eases out over this many cells behind it.
pub const GLASS_SHORT_TRAIL_CELLS: f32 = 3.0;

/// A glass run's oldest end (§2.2 The oldest end, §15.2 1.8): the run's height at window x is
/// its reach times smooth01(d / [`GLASS_TAIL_CELLS`]), `d` the cells from its oldest end, plus
/// smooth01(1 − behind / [`GLASS_SHORT_TRAIL_CELLS`]), `behind` the cells from its hand, so a
/// short trail's hand keeps its height. Laid per vertex, so the wedge emission draws the ease
/// with no stair. A run continued at its oldest end (a seam, a neighbour) has no end there.
#[derive(Clone, Copy, Debug)]
struct GlassTail {
    /// The run's left edge, window x.
    x0: f32,
    inv_cw: f32,
    /// The run's cells.
    len: f32,
    /// The hand's boundary, cells from the run's left edge.
    head: f32,
    /// Whether the oldest end is the run's right (a stream laid leftward).
    oldest_right: bool,
    /// Whether the oldest end is open.
    open: bool,
}

impl GlassTail {
    /// The height share at window x `x`.
    #[inline]
    const fn at(self, x: f32) -> f32 {
        let pos = (x - self.x0) * self.inv_cw;
        let (d, behind) = if self.oldest_right {
            (self.len - pos, pos - self.head)
        } else {
            (pos, self.head - pos)
        };
        let ease = if self.open {
            smoothstep01(d * (1.0 / GLASS_TAIL_CELLS))
        } else {
            1.0
        };
        clamp01(ease + smoothstep01(1.0 - behind.max(0.0) * (1.0 / GLASS_SHORT_TRAIL_CELLS)))
    }
}

/// A row of the hand's structural set the hand has left ([`Ribbon::structural_rows`],
/// `docs/design/PRISM-GLASS.md` §2.4 The sink, §15.2 1.10): light only, kept beside main's cells
/// in a side table keyed by the cohort and its row, which moves with them wherever main moves
/// rows. The row's glass sinks by height from `left_at` to [`WRAP_SINK_CH`] over
/// [`WRAP_SINK_S`] ([`wrapped_up`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlassWrap {
    /// The cohort on the row.
    pub cohort: u32,
    /// The row.
    pub row: u16,
    /// When the hand left the row, or when the glass lit over a row it had already left.
    pub left_at: Instant,
}

impl GlassWrap {
    /// How far the row has sunk at `now`: `0` at `left_at`, `1` [`WRAP_SINK_S`] after it
    /// (smoothstep); `1` at once under reduced motion, which steps main's row holds the same way.
    #[inline]
    fn sunk(self, now: Instant, reduced: bool) -> f32 {
        if reduced {
            return 1.0;
        }
        smoothstep01(
            now.saturating_duration_since(self.left_at).as_secs_f32() * (1.0 / WRAP_SINK_S),
        )
    }
}

/// The glass's reach `up` px on a row of the hand's structural set (§2.4): a row the hand has left,
/// `sunk` of the way down, up × lerp(1, min(1, [`WRAP_SINK_CH`] / up), sunk) on a `chf` px cell,
/// and at most `cap` px (the hand's row of a multi-row set, [`WRAP_HAND_TOP_PX`] under its
/// cell's top).
#[inline]
const fn wrapped_up(up: f32, chf: f32, sunk: f32, cap: f32) -> f32 {
    let up = if sunk > 0.0 && up > 0.0 {
        let low = (WRAP_SINK_CH * chf / up).min(1.0);
        up * (1.0 + (low - 1.0) * sunk)
    } else {
        up
    };
    up.min(cap)
}

/// Seconds until a sink falling `fall` px over [`WRAP_SINK_S`] (smoothstep), `x` of the way
/// through, next moves its height a pixel; `None` once it has no pixel left to fall.
#[inline]
#[must_use]
pub(crate) const fn level_step_sink(fall: f32, x: f32) -> Option<f32> {
    if !(fall.is_finite() && x.is_finite()) || fall <= 0.0 || x >= 1.0 {
        return None;
    }
    let x = clamp01(x);
    let next = ((fall * smoothstep01(x)).round() + 0.5) / fall;
    if next >= 1.0 {
        return None;
    }
    let x_next = 0.5
        - crate::supernova::sin_cos_f32(
            crate::color_math::asin_f32((1.0 - 2.0 * next).clamp(-1.0, 1.0)) * (1.0 / 3.0),
        )
        .0;
    Some(((x_next - x) * WRAP_SINK_S).max(0.0))
}

/// The lip's rows the glass's price reserves under each slab, in `ch`: the lip's deep height
/// (`docs/design/PRISM-GLASS.md` §2.3 DEEP), so the lip a later phase lays under the glass fits
/// the density the glass chose.
pub const GLASS_LIP_RESERVE_CH: f32 = 0.25;

/// The lights a wedge row of the glass lays at most (`aterm_render::ribbon_glass_into`): its
/// flat run, the melt's run and the dark side's, and the chord a long melt is split at, where a
/// slab's row lays one.
pub const GLASS_WEDGE_ROW_LIGHTS: f32 = 4.0;

/// The rows a glass slab lays at a cell `ch` px tall, the lip's reserved rows beside them: its
/// reach at the hand with the wave's swing, one more where its top straddles a row, and
/// [`GLASS_LIP_RESERVE_CH`]. Some 36 at a Retina's 28 px.
#[inline]
#[must_use]
pub const fn glass_slab_rows(ch: f32) -> f32 {
    ((GLASS_UP_CH + LIGHT_GLASS_WAVE_CH) * ch).ceil() + 1.0 + (GLASS_LIP_RESERVE_CH * ch).ceil()
}

/// The lights a glass run's open oldest end lays past its slabs' own rows at `slabs` a cell
/// `ch` px tall (§2.2 The oldest end): every slab of the ease ([`GLASS_TAIL_CELLS`], one more
/// for its straddle) is a wedge, each wedge row laying [`GLASS_WEDGE_ROW_LIGHTS`] where a slab's
/// row lays one, over the rows its top climbs (at most smoothstep's steepest, 1.5 times the
/// mean climb) and its melt's ([`GLASS_EDGE_PX`]), with a row's straddle at each side. A wedge
/// row's runs each stand in their own column, so none rides a ramp
/// ([`aterm_render::glow_ramps::RAMP_COST`] folds a column's rows only): each is charged whole
/// against [`RIBBON_QUAD_BUDGET`], as the ramp sink's `laid` counts it.
#[inline]
#[must_use]
pub const fn glass_tail_wedge_lights(ch: f32, slabs: usize) -> f32 {
    let reach = (GLASS_UP_CH + LIGHT_GLASS_WAVE_CH) * ch;
    let per_cell = slabs as f32;
    let wedges = (GLASS_TAIL_CELLS * per_cell).ceil() + 1.0;
    let climb = reach * 1.5 / (GLASS_TAIL_CELLS * per_cell);
    let edge_rows = (climb + GLASS_EDGE_PX).ceil() + 2.0;
    wedges * edge_rows * (GLASS_WEDGE_ROW_LIGHTS - 1.0)
}

/// What a glass frame of `cells` cells over `rows` rows of `ch` px costs at `slabs` a cell, in
/// lights: each slab's [`glass_slab_rows`], one cell's slabs more for the run's last boundary as
/// main prices it, and each row's open oldest end ([`glass_tail_wedge_lights`]).
#[inline]
#[must_use]
pub const fn glass_frame_price(ch: f32, cells: usize, rows: usize, slabs: usize) -> f32 {
    (cells + 1) as f32 * slabs as f32 * glass_slab_rows(ch)
        + rows as f32 * glass_tail_wedge_lights(ch, slabs)
}

/// The rows the plan's cells stand on, sorted by row ([`Ribbon::plan`]).
#[inline]
const fn rows_of(sorted: &[(u16, u16, u32)]) -> usize {
    let mut rows = 0;
    let mut i = 0;
    while i < sorted.len() {
        if i == 0 || sorted[i].0 != sorted[i - 1].0 {
            rows += 1;
        }
        i += 1;
    }
    rows
}

/// How [`Ribbon::envelope`] folds a cell's factors: main's one product ([`MainEnv`]), or the
/// glass's opacity and height ([`GlassEnv`]).
trait Envelope: Copy {
    /// The envelope of a cell not yet born.
    const DARK: Self;
    /// The birth's edge-in (its wipe's ramp).
    fn edge_in(&mut self, f: f32);
    /// A factor that moves the light over time: the melt, an exit or a cell's own relight
    /// (under reduced motion, its fade).
    fn fade(&mut self, f: f32);
    /// The focus ember's factor and its relight's, from the ember's floor: main's [`Self::fade`].
    #[inline]
    fn ember(&mut self, f: f32) {
        self.fade(f);
    }
    /// A dim no clock of the motion makes: a printed line's.
    fn scale(&mut self, f: f32);
    /// The ember's floor for `cell`, from what the envelope carries so far.
    fn ember_floor(&self, rib: &Ribbon, ctx: &Ctx<'_>, cell: &Cell) -> f32;
}

/// Main's envelope: the product of every factor, in [`Ribbon::envelope`]'s order.
#[derive(Clone, Copy, Debug)]
struct MainEnv(f32);

impl Envelope for MainEnv {
    const DARK: Self = Self(0.0);

    #[inline]
    fn edge_in(&mut self, f: f32) {
        self.0 *= f;
    }

    #[inline]
    fn fade(&mut self, f: f32) {
        self.0 *= f;
    }

    #[inline]
    fn scale(&mut self, f: f32) {
        self.0 *= f;
    }

    #[inline]
    fn ember_floor(&self, rib: &Ribbon, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        rib.ember_floor(ctx, cell, self.0)
    }
}

/// The glass's envelope (`docs/design/PRISM-GLASS.md` §2.2 Exits by height, §15.2 1.8): its
/// opacity moves only at the edge-in and, under reduced motion, the fade; every other factor (the
/// melt, the swoosh, the retract, the curtain, the ember, a relight, the printed dim) scales its
/// height. Its ember holds the height [`GLASS_MIN_PX`] (with main's margin) of a `reach` px glass,
/// so every run stays drawn through it, and under reduced motion too: the ember and its relight
/// move the height ([`Envelope::ember`]), so an unfocused window's glass stands at its least
/// height at whole opacity, never a translucent pastel held for as long as the window is
/// unfocused.
#[derive(Clone, Copy, Debug)]
struct GlassEnv {
    opacity: f32,
    height: f32,
    reduced: bool,
    /// The cell lies on a row the hand has left, sunk to [`WRAP_SINK_CH`] ([`GlassWrap`]): its
    /// exits fade its opacity at the sunk height, so the row retires as a streak along its foot,
    /// as the dark ribbon's does, rather than shrinking to a hairline.
    left: bool,
    reach: f32,
}

impl GlassEnv {
    #[inline]
    const fn new(reduced: bool, reach: f32) -> Self {
        Self {
            opacity: 1.0,
            height: 1.0,
            reduced,
            left: false,
            reach,
        }
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static LEFT_ROWS_SHRINK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A test's negative control: `f` run with a row the hand has left melting by height, as Prism
/// Glass 1.12 did ([`GlassEnv::left`]). Debug and test builds only.
#[cfg(any(test, debug_assertions))]
pub fn with_left_rows_shrinking<R>(f: impl FnOnce() -> R) -> R {
    LEFT_ROWS_SHRINK.with(|c| c.set(true));
    let out = f();
    LEFT_ROWS_SHRINK.with(|c| c.set(false));
    out
}

/// Whether a row the hand has left retires by its glass's opacity ([`GlassEnv::left`]).
#[inline]
fn left_rows_fade() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        !LEFT_ROWS_SHRINK.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        true
    }
}

impl Envelope for GlassEnv {
    const DARK: Self = Self {
        opacity: 0.0,
        height: 0.0,
        reduced: false,
        left: false,
        reach: 0.0,
    };

    #[inline]
    fn edge_in(&mut self, f: f32) {
        self.opacity *= f;
    }

    #[inline]
    fn fade(&mut self, f: f32) {
        if self.reduced || self.left {
            self.opacity *= f;
        } else {
            self.height *= f;
        }
    }

    #[inline]
    fn ember(&mut self, f: f32) {
        self.height *= f;
    }

    #[inline]
    fn scale(&mut self, f: f32) {
        self.height *= f;
    }

    #[inline]
    fn ember_floor(&self, _: &Ribbon, _: &Ctx<'_>, _: &Cell) -> f32 {
        glass_height_floor(self.reach * self.height)
    }
}

/// The share of its height a glass `reach` px tall keeps through an ember: [`GLASS_MIN_PX`]
/// with main's margin ([`EMBER_FLOOR_MARGIN`]).
#[inline]
const fn glass_height_floor(reach: f32) -> f32 {
    if !reach.is_finite() || reach <= 0.0 {
        return 0.0;
    }
    GLASS_MIN_PX * EMBER_FLOOR_MARGIN / reach
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static GLASS_EXITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count one glass exit read ([`take_glass_exits`]).
#[cfg(any(test, debug_assertions))]
#[inline]
fn count_glass_exit() {
    GLASS_EXITS.with(|n| n.set(n.get() + 1));
}

/// The glass's exits this thread ran since the last take, zeroed: the glass envelopes folded
/// (opacity and height), the glass slab prices ([`glass_frame_price`]) and the glass cadences
/// ([`Ribbon::next_change_deadline`]'s one-pixel steps). The host's light-fork pin holds it at
/// zero on every dark ground. Debug and test builds only.
#[cfg(any(test, debug_assertions))]
pub fn take_glass_exits() -> usize {
    GLASS_EXITS.with(|n| n.replace(0))
}

/// A boundary's planned shape: `(spine, up, dn, t, cov, glass_up, line_keep)` ([`Segment`]).
type Sample = (f32, f32, f32, f32, f32, f32, f32);

#[derive(Clone, Copy, Debug)]
struct Run {
    row: u16,
    col0: u16,
    col1: u16,
    lo: usize,
    hi: usize,
    head_col: u16,
    head: usize,
    wet: bool,
    at_caret: bool,
    born: Instant,
    stream_dir: i8,
    /// How far the run is held inside its row ([`Ribbon::plan_over`]), `0`
    /// to `1`: at `1` the body is laid inside the typed row, as on a pane's
    /// last row, and the rail is dark, so no light lies on the next line's
    /// ascenders; between, both shapes blend and the rail dims.
    hold: f32,
}

/// How long a line's light takes to settle into its row when the next row
/// inks under it (and to reach below again when it clears), seconds.
pub const HOLD_EASE_S: f32 = 0.15;

/// How long a held row takes to let go of its row, seconds: when the next
/// row clears, and when a line on it flows into the fold.
pub const HOLD_RELEASE_S: f32 = 0.60;

/// The feather a run the hand has left wears at its head end ([`Ribbon::head_feather`]): `1` everywhere for a run at the caret.
#[derive(Clone, Copy, Debug)]
struct HeadFeather {
    /// The head boundary's window x.
    head_x: f32,
    /// The feather's reach back from it, px; `0` for none.
    width: f32,
    inv_width: f32,
}

impl HeadFeather {
    /// The share of its light a boundary at window x `x` keeps: nothing at the head end, easing to all of it [`HEAD_FEATHER_CELLS`] back.
    #[inline]
    const fn at(self, x: f32) -> f32 {
        if self.width <= 0.0 {
            return 1.0;
        }
        smoothstep01((x - self.head_x).abs() * self.inv_width)
    }
}

/// How [`Ribbon::emit`] lays a run's vertices: main's beam at a shoulder (`aterm_render::ribbon_beam_into`,
/// source-over), or the light glass (§15.2 1.7: `aterm_render::ribbon_glass_into`, its slabs
/// wedges at [`aterm_render::WEDGE_EDGE_PX`] and its top a [`GLASS_EDGE_PX`] melt).
#[derive(Clone, Copy, Debug)]
enum BodyLay {
    Beam(f32),
    Glass,
}

impl BodyLay {
    /// Lay `verts` into `out`; `false` where the `budget` ran out.
    #[inline]
    fn lay(
        self,
        out: &mut RampSink<'_>,
        clip: BeamClip,
        verts: &[RibbonVertex],
        stride: usize,
        budget: usize,
    ) -> bool {
        match self {
            Self::Beam(shoulder) => {
                ribbon_beam_into(out, clip, verts, shoulder, stride, budget, GlowBlend::Over)
            }
            Self::Glass => aterm_render::ribbon_glass_into(
                out,
                clip,
                verts,
                stride,
                budget,
                aterm_render::WEDGE_EDGE_PX,
                GLASS_EDGE_PX,
            ),
        }
    }
}

/// A boundary as the emit lays it: a planned [`Segment`]'s fields with its coverage in `f32`, or one laid between two of them.
#[derive(Clone, Copy, Debug)]
struct LaidSegment {
    x: f32,
    spine: f32,
    up: f32,
    dn: f32,
    t: f32,
    cov: f32,
    glass_up: f32,
    line_keep: f32,
    glass_jump: f32,
}

const FEATHER_FINE_PX: f32 = 1.0;

/// How deep from a run's tail its boundaries are laid finely, in cells
/// ([`for_each_feathered`]).
const RUN_END_FINE_CELLS: f32 = 1.0;

/// The strides a run's end is laid finely into: a pixel apart on the
/// gallery's 14 px cells, two on a Retina's 30.
const RUN_END_FINE_SPLIT: usize = 5;

/// The share of the quad budget a frame's estimate, its tails laid finely
/// with it, must stay under: below [`FEATHER_ROOM_SHARE`], since the
/// estimate leaves out the head feathers' own fine windows.
const RUN_END_ROOM_SHARE: f32 = 0.5;

const FEATHER_ROOM_ROWS: f32 = 1.6;
const FEATHER_ROOM_SHARE: f32 = 0.6;

/// Lays each of `segs` (in reverse) through `each`, and between two of
/// them a boundary a pixel where the head's `feather` reaches, or every
/// `end_px` px within `end_reach` px of the left end of `segs` (where the
/// plan eases a run's tail in) unless the head is there. The rasterizer
/// holds a window's light flat across each stride, so a run's tail easing
/// in from [`RUN_TAIL_EASE`] over its cell's three slabs would stand as a
/// stair of flat blocks. The reach is a place, not a coverage step, so a
/// window keeps its layout while its light changes.
#[inline]
const fn laid_segment(s: &Segment) -> LaidSegment {
    LaidSegment {
        x: s.x,
        spine: s.spine,
        up: s.up,
        dn: s.dn,
        t: s.t,
        cov: s.cov as f32,
        glass_up: s.glass_up,
        line_keep: s.line_keep,
        glass_jump: s.glass_jump,
    }
}

#[inline]
fn for_each_feathered(
    segs: &[Segment],
    feather: HeadFeather,
    (end_reach, end_px): (f32, f32),
    mut each: impl FnMut(Option<usize>, LaidSegment),
) {
    let tail = match segs.first() {
        Some(s) => s.x,
        None => 0.0,
    };
    let feather_active = feather.width > 0.0;
    let end_active = end_reach > 0.0 && end_px > 0.0 && (tail - feather.head_x).abs() >= 1.0;
    if !feather_active && !end_active {
        let mut i = segs.len();
        while i > 0 {
            i -= 1;
            each(Some(i), laid_segment(&segs[i]));
        }
        return;
    }
    let mut prev: Option<LaidSegment> = None;
    let mut i = segs.len();
    while i > 0 {
        i -= 1;
        let here = laid_segment(&segs[i]);
        if let Some(p) = prev {
            let feathered = feather_active
                && (p.x - feather.head_x)
                    .abs()
                    .min((here.x - feather.head_x).abs())
                    < feather.width;
            let at_end = end_active && (p.x - tail).abs().min((here.x - tail).abs()) < end_reach;
            let px = if feathered {
                FEATHER_FINE_PX
            } else if at_end {
                end_px
            } else {
                0.0
            };
            if px > 0.0 {
                let n = ((here.x - p.x).abs() / px).floor() as usize;
                if n > 1 {
                    let inv_nf = 1.0 / (n as f32);
                    // A hard step's right side opens at its own walk: the boundaries laid
                    // between `p` and `here` lerp from `p` to `here`'s right side.
                    let here_t = if here.glass_jump == 0.0 {
                        here.t
                    } else {
                        here.t + here.glass_jump
                    };
                    let (dx, d_spine, d_up, d_dn, dt, d_cov, d_glass, d_keep) = (
                        here.x - p.x,
                        here.spine - p.spine,
                        here.up - p.up,
                        here.dn - p.dn,
                        here_t - p.t,
                        here.cov - p.cov,
                        here.glass_up - p.glass_up,
                        here.line_keep - p.line_keep,
                    );
                    let mut k = 1;
                    while k < n {
                        let u = k as f32 * inv_nf;
                        each(
                            None,
                            LaidSegment {
                                x: p.x + dx * u,
                                spine: p.spine + d_spine * u,
                                up: p.up + d_up * u,
                                dn: p.dn + d_dn * u,
                                t: p.t + dt * u,
                                cov: p.cov + d_cov * u,
                                glass_up: p.glass_up + d_glass * u,
                                line_keep: p.line_keep + d_keep * u,
                                glass_jump: 0.0,
                            },
                        );
                        k += 1;
                    }
                }
            }
        }
        each(Some(i), here);
        prev = Some(here);
    }
}

/// The attach's level this frame, set by [`Ribbon::plan_run`] at the head boundary of the run under the caret: the head boundary's coverage (`base`), the head cell's uniform attack (`uniform`, [`BIRTH_EDGE_FLOOR`] to 1 over the edge-in) and the wipe's probe ([`Ribbon::wipe_of`]).
#[derive(Clone, Copy, Debug)]
struct AttachFloor {
    row: u16,
    col: u16,
    base: f32,
    uniform: f32,
    probe: Cell,
}

impl AttachFloor {
    #[inline]
    fn level(&self, rib: &Ribbon, ctx: &Ctx<'_>, dist: f32) -> f32 {
        self.base * self.uniform * rib.wipe_of(ctx, &self.probe, dist)
    }
}

#[derive(Clone, Copy, Debug)]
struct ShortReanchorProof {
    from: (u16, u16),
    to: (u16, u16),
    at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct RewrapStart {
    from: (u16, u16),
    to: (u16, u16),
    start: u16,
    width: Option<u16>,
    space: Option<u16>,
}

/// The ribbon producer.
#[derive(Clone, Debug)]
pub struct Ribbon {
    /// Test-only: leave a run the hand has left square-cut at its head end (no [`Ribbon::head_feather`]), the negative control of the feather's test.
    #[cfg(test)]
    head_square: bool,
    /// Test-only: lift the underline off a last or held row's floor by its
    /// reach below, as the tall ribbon is lifted, as it was (looks10 L3's
    /// negative control).
    #[cfg(test)]
    underline_lifted: bool,
    /// Test-only: ease the underline off the letters that land on the row
    /// below with the row's hold, as the tall ribbon eases, as it did
    /// (looks10 round 3's negative control).
    #[cfg(test)]
    underline_eases_off_letters: bool,
    /// Test-only: keep the hot edge on a run the hand has left, the negative
    /// control of the hot edge's hand test.
    #[cfg(test)]
    hot_edge_left_lit: bool,
    /// Test-only: lay the boundaries inside a head feather's reach at the
    /// stride ([`for_each_feathered`]), the negative control of the smooth
    /// feather's test.
    #[cfg(test)]
    feather_coarse: bool,
    /// Test-only: lay a run's tail at the stride, the negative control of the
    /// tail's fine lay ([`RUN_END_FINE_CELLS`]).
    #[cfg(test)]
    tail_stair: bool,
    /// Test-only: step the glass's cadence by main's coverage levels, the negative control of
    /// its one-pixel steps ([`Ribbon::next_change_deadline`]).
    #[cfg(test)]
    glass_level_steps: bool,
    /// Test-only: centre the hot edge on the row's top under a kept box, the
    /// negative control of its inset.
    #[cfg(test)]
    hot_edge_on_bar: bool,
    /// Test-only: step a line's hold to its target in one frame, the negative
    /// control of the hold's ease.
    #[cfg(test)]
    hold_steps: bool,
    /// Test-only: lay the rail at the row's bottom ([`RAIL_GAP_CH`] off), the
    /// negative control of the descenders' hairline.
    #[cfg(test)]
    rail_flush: bool,
    /// Test-only: the hairline rounded alone, a pixel on a short row, inside
    /// the judge's ring ([`rail_gap_px`]'s negative control).
    #[cfg(test)]
    pub(crate) rail_gap_rounded: bool,
    /// Test-only: the rail keeps below its row over a filled cell, as it did
    /// (looks8 K9's negative control).
    #[cfg(test)]
    pub(crate) row_rail_off: bool,
    /// Test-only: no rail is laid, so a frame's last laid vertices ([`Self::verts`]) are its
    /// body's: the glass's own, which its rail laid after it would leave behind.
    #[cfg(test)]
    pub(crate) rail_off: bool,
    /// Test-only: the line lays its whole light whatever the body keeps of its height, and under
    /// the glass none once no glass of its run stands [`GLASS_MIN_PX`]: it went out in one frame
    /// at the end of every glass exit and of the fold flow, as it did ([`Segment::line_keep`]'s
    /// negative control).
    #[cfg(test)]
    pub(crate) line_whole: bool,
    /// Test-only: the line under the glass lerps between its vertices across the lip's snaps,
    /// through the olive between its gold and green, as it did ([`BedInkLut::rail_step`]'s
    /// negative control).
    #[cfg(test)]
    pub(crate) rail_lerps_the_fold: bool,
    /// Test-only: a glass frame lays each run's rail after its own glass, before the next run's,
    /// as main's frames lay theirs (the glass-first order's negative control).
    #[cfg(test)]
    pub(crate) glass_rails_by_run: bool,
    /// Test-only: the in-row rail parts round every foot, as it did
    /// (looks9 Q9's negative control, [`ROW_RAIL_DIP_GAP_PX`]).
    #[cfg(test)]
    pub(crate) row_rail_parted: bool,
    /// Test-only: a row held over letters, and the pane's last row, lay no rail in their own
    /// foot band, as they did: the row a person types on had a dim foot band and no line
    /// (CT-05's negative control).
    #[cfg(test)]
    pub(crate) held_rail_off: bool,
    /// The caret is a bar or an underline ([`Self::set_thin_caret`]).
    thin_caret: bool,
    /// The in-row rail's vertices laid finely ([`part_row_rail`]).
    parted: Vec<RibbonVertex>,
    /// Test-only: draw the pet's and the flat ribbon alike on paper (no
    /// rail, one body), the negative control of their reading apart there.
    #[cfg(test)]
    paper_alike: bool,
    /// Test-only: lay the flat ribbon's body on paper as the pet's glow, its
    /// lit plateau and its light following each cell's (looks9 Q6's
    /// negative control, [`FLAT_PAPER_GAIN`]).
    #[cfg(test)]
    flat_paper_glow: bool,
    /// Test-only: leave each row's hold behind when a scroll or a band move
    /// carries its line away, the negative control of the carried hold.
    #[cfg(test)]
    holds_left_behind: bool,
    /// Each row's hold ([`Run::hold`]), with when it was last eased, and its
    /// next frame's list (resident and reused). By row: the runs of a row
    /// share seams, so they share a floor.
    holds: Vec<(u16, f32, Instant)>,
    holds_next: Vec<(u16, f32, Instant)>,
    /// Prior shape of each moved or newborn cohort until the next plan.
    /// Kept apart from row holds so an earlier move cannot rewrite a later
    /// run's source. A fresh identity explicitly has no previous shape.
    carried_holds: Vec<(u32, Option<(f32, Instant)>)>,
    /// The live cells, resident and reused (§18: no allocation per frame).
    cells: Vec<Cell>,
    cohorts: Vec<Cohort>,
    plan: Vec<Segment>,
    index: FieldIndex,
    slabs: usize,
    next_cohort: u32,
    caret: Option<(u16, u16)>,
    runs: Vec<Run>,
    sorted: Vec<(u16, u16, u32)>,
    verts: Vec<RibbonVertex>,
    released_cohorts: Vec<u32>,
    /// Partial-release counts, sorted by `(cohort, stamp)`. Every key has
    /// resident stamped cells; an exact return must still match its saved count.
    release_parts: Vec<ReleasePart>,
    ink: BedInkLut,
    /// The light pages' palettes the bed's glass draws by ([`glass_table`]), light only.
    palettes: PaletteCache,
    /// The instant of the last frame whose plan the bed's glass drew, laid or not
    /// ([`Self::glass_drew_at`]), light only.
    glass_drew: Option<Instant>,
    /// The under stream's span that frame's glass was laid in ([`Self::glass_span`]), light
    /// only: `None` on a frame it laid nothing.
    glass_span: Option<(StreamMark, StreamMark)>,
    /// The rows of the hand's structural set the hand has left ([`GlassWrap`]), light only:
    /// empty on every frame the bed draws by main's table.
    glass_wrap: Vec<GlassWrap>,
    /// The hand's row when the last plan's structural set held more than one row
    /// ([`WRAP_HAND_TOP_PX`]), light only.
    wrap_hand: Option<u16>,
    /// Test-only: lay no sink, the negative control of the wrap's sink and of its wakes.
    #[cfg(test)]
    wrap_unsunk: bool,
    ground: u32,
    ink_quads: usize,
    /// The frame's rails laid clear of their row's ring, as `[from, to)`
    /// spans of the under stream, each marked whether it lies in its own
    /// row's foot band ([`Self::rail_spans`]).
    rail_spans: Vec<(StreamMark, StreamMark, bool)>,
    ember_at: Option<Instant>,
    rearm: Option<(Instant, f32)>,
    translated_at: Option<Instant>,
    reduced: bool,
    pane: Option<(u16, u16)>,
    pane_rows: Option<(u16, u16)>,
    attach: Option<(u16, u16)>,
    attach_floor: Option<AttachFloor>,
    cell_h: f32,
    /// The glass's slabs a cell at the last plan ([`Self::glass_slabs_per_cell`]).
    glass_slabs: usize,
    /// The glass's px a `ch` at the last plan: the cell's height times the frame's intensity
    /// under `glass_lit`, `0` on every other frame ([`Self::next_change_deadline`]'s pixel steps).
    glass_px: f32,
    /// Test-only: main's ember floor reads the glass's colour floor
    /// ([`BedInkLut::seen_floor_at`]), the 1.5 leak `main_seen_floor_at` closed (§15.2 1.8),
    /// for the paper twin's negative control.
    #[cfg(test)]
    pub(crate) ember_reads_glass: bool,
    wedge_px: f32,
    scroll_followed: bool,
    followed_now: Vec<(u16, u16, Instant)>,
    short_reanchor: Option<ShortReanchorProof>,
    shifted_now: Vec<(u16, u16, u16, i16)>,
    pending_erase: u16,
    pen: Option<(u16, u16)>,
    last_space: Option<(u16, u16)>,
    arrived: Option<(u16, u16)>,
    prev_space: Option<(u16, u16)>,
    relocate: Option<Relocate>,
    soft_wrap: Option<SoftWrap>,
    carry: Option<(SoftWrap, Instant)>,
    reflow: Option<SoftWrap>,
    lift: Option<Lift>,
    rewrap_start: Option<RewrapStart>,
    revive: Option<Revive>,
    split_off: Option<SplitOff>,
    last_walk: Option<LastWalk>,
    fresh_line: Option<FreshLine>,
    seam: Option<Seam>,
    ioi_ms: f32,
    last_key_at: Option<Instant>,
    phrase_open: Option<u32>,
    next_phrase: u32,
    curtain: Option<Instant>,
    hand_floor: Option<(u16, u16)>,
    /// What the body's top never feathers up into ([`KeepOut`]): a word
    /// icon or a lettered or filled cell standing on the run's row.
    pub keep_out: KeepOut,
    /// A test's negative control: a body reaching past the effects box's top
    /// is cut flat there by the box's clip, not laid shorter inside it
    /// ([`crate::cursor_glow::RISE_CLEAR_PX`]).
    #[cfg(any(test, debug_assertions))]
    pub(crate) rise_cut: bool,
}

/// How far beside a [`KeepOut`] box the body's top eases back to its own
/// reach, in cells.
const KEEP_OUT_EASE_CELLS: f32 = 0.5;

#[derive(Clone, Copy, Debug)]
struct Relocate {
    row: u16,
    w: u16,
    at: Instant,
    walk: Option<WalkPos>,
}

/// How long a [`Relocate`] waits for the caret to reveal the moved word.
pub const RELOCATE_PATIENCE_S: f32 = 2.0;
const RELOCATE_PATIENCE_DUR: std::time::Duration =
    std::time::Duration::from_secs(RELOCATE_PATIENCE_S as u64);

/// The soft-wrapped caret (`CursorGlow::soft_wrapped_caret`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftWrap {
    pub origin: (u16, u16),
    pub landing: (u16, u16),
    pub word_col0: u16,
}

/// The lifted word (`CursorGlow::lifted_word`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lift {
    pub origin: (u16, u16),
    pub landing: (u16, u16),
    pub dst: (u16, u16),
}

/// The revive lays a line back on its own walk (`RAINBOW-KITTY-V2.md` §43).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Revive {
    pub row: u16,
    pub col0: u16,
    pub col1: u16,
    pub origin: RunOrigin,
}

impl Revive {
    #[inline]
    const fn covers(&self, row: u16, col: u16) -> bool {
        row == self.row && self.col0 <= col && col < self.col1
    }
}

/// A typed run's walk origin, kept outside the ribbon ([`Revive`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunOrigin {
    pub anchor_col: u16,
    pub t0: f32,
    pub d0: f32,
}

impl RunOrigin {
    /// `coh`'s walk.
    #[inline]
    #[must_use]
    pub const fn of(coh: &Cohort) -> Self {
        Self {
            anchor_col: coh.anchor_col,
            t0: coh.t0,
            d0: coh.d0,
        }
    }

    #[inline]
    const fn walk(self) -> WalkPos {
        WalkPos {
            t: self.t0,
            d: self.d0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SplitOff {
    copy: u32,
    dr: i16,
    col0: u16,
    col1: u16,
}

#[derive(Clone, Copy, Debug)]
struct LastWalk {
    end: WalkPos,
    alive_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seam {
    row: u16,
    at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FreshLine {
    since: Instant,
    chord_at: Option<Instant>,
    awaiting_move: bool,
    left_row: Option<u16>,
}

impl FreshLine {
    #[inline]
    fn takes(self, row: u16, at: Instant) -> bool {
        !self.awaiting_move
            && self.chord_at != Some(at)
            && (at >= self.since || matches!(self.left_row, Some(left) if left != row))
    }
}

#[derive(Clone, Copy)]
enum RetractX {
    Identity,
    SuckIn { caret_x: f32, keep: f32 },
    FlowRightFirst { start: f32, s: f32 },
    FlowLeftFirst { end: f32, s: f32 },
}

impl RetractX {
    #[inline]
    const fn at(self, x: f32) -> f32 {
        match self {
            Self::Identity => x,
            Self::SuckIn { caret_x, keep } => caret_x + (x - caret_x) * keep,
            Self::FlowRightFirst { start, s } => {
                if x < start - 0.5 {
                    x
                } else {
                    (x - s).max(start)
                }
            }
            Self::FlowLeftFirst { end, s } => {
                if x > end + 0.5 {
                    x
                } else {
                    (x + s).min(end)
                }
            }
        }
    }
}

impl Default for Ribbon {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Ribbon {
    /// A ribbon with nothing laid.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            #[cfg(test)]
            head_square: false,
            #[cfg(test)]
            underline_lifted: false,
            #[cfg(test)]
            underline_eases_off_letters: false,
            #[cfg(test)]
            hot_edge_left_lit: false,
            #[cfg(test)]
            feather_coarse: false,
            #[cfg(test)]
            tail_stair: false,
            #[cfg(test)]
            glass_level_steps: false,
            #[cfg(test)]
            hot_edge_on_bar: false,
            #[cfg(test)]
            hold_steps: false,
            #[cfg(test)]
            rail_flush: false,
            #[cfg(test)]
            rail_gap_rounded: false,
            #[cfg(test)]
            row_rail_off: false,
            #[cfg(test)]
            rail_off: false,
            #[cfg(test)]
            line_whole: false,
            #[cfg(test)]
            rail_lerps_the_fold: false,
            #[cfg(test)]
            glass_rails_by_run: false,
            #[cfg(test)]
            held_rail_off: false,
            thin_caret: false,
            parted: Vec::new(),
            #[cfg(test)]
            row_rail_parted: false,
            #[cfg(test)]
            paper_alike: false,
            #[cfg(test)]
            flat_paper_glow: false,
            #[cfg(test)]
            holds_left_behind: false,
            holds: Vec::new(),
            holds_next: Vec::new(),
            carried_holds: Vec::new(),
            cells: Vec::new(),
            cohorts: Vec::new(),
            plan: Vec::new(),
            index: FieldIndex::EMPTY,
            slabs: 0,
            next_cohort: 0,
            caret: None,
            runs: Vec::new(),
            sorted: Vec::new(),
            verts: Vec::new(),
            released_cohorts: Vec::new(),
            release_parts: Vec::new(),
            ink: BedInkLut::EMPTY,
            palettes: PaletteCache::new(),
            glass_drew: None,
            glass_span: None,
            glass_wrap: Vec::new(),
            wrap_hand: None,
            #[cfg(test)]
            wrap_unsunk: false,
            ground: 0,
            ink_quads: 0,
            rail_spans: Vec::new(),
            ember_at: None,
            rearm: None,
            translated_at: None,
            reduced: false,
            pane: None,
            pane_rows: None,
            attach: None,
            attach_floor: None,
            cell_h: 0.0,
            glass_px: 0.0,
            #[cfg(test)]
            ember_reads_glass: false,
            glass_slabs: 0,
            wedge_px: 0.0,
            scroll_followed: false,
            followed_now: Vec::new(),
            short_reanchor: None,
            shifted_now: Vec::new(),
            pending_erase: 0,
            pen: None,
            last_space: None,
            arrived: None,
            prev_space: None,
            relocate: None,
            soft_wrap: None,
            carry: None,
            reflow: None,
            lift: None,
            rewrap_start: None,
            revive: None,
            split_off: None,
            last_walk: None,
            fresh_line: None,
            seam: None,
            ioi_ms: 0.0,
            last_key_at: None,
            phrase_open: None,
            next_phrase: 0,
            curtain: None,
            hand_floor: None,
            keep_out: KeepOut::EMPTY,
            #[cfg(any(test, debug_assertions))]
            rise_cut: false,
        }
    }

    /// The host has queued the licensed move of the exact held park whose source row's content followed.
    #[inline]
    pub(crate) const fn prove_short_reanchor(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        at: Instant,
    ) {
        debug_assert!(
            from.0 == to.0 && {
                let d = from.1.saturating_sub(to.1);
                d >= 1 && d <= 2
            }
        );
        self.short_reanchor = Some(ShortReanchorProof { from, to, at });
    }

    /// The live cells, in pool (append) order. Read-only.
    #[inline]
    #[must_use]
    pub const fn cells(&self) -> &[Cell] {
        self.cells.as_slice()
    }

    /// The live cohorts. Read-only.
    #[inline]
    #[must_use]
    pub const fn cohorts(&self) -> &[Cohort] {
        self.cohorts.as_slice()
    }

    #[inline]
    #[must_use]
    const fn cohort_index(&self, id: u32) -> Option<usize> {
        let cohorts = self.cohorts.as_slice();
        let mut i = 0;
        while i < cohorts.len() {
            if cohorts[i].id == id {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    #[inline]
    #[must_use]
    pub(crate) const fn cohort_by_id(&self, id: u32) -> Option<&Cohort> {
        Cohort::find(self.cohorts.as_slice(), id)
    }

    #[inline]
    const fn cohort_by_id_mut(&mut self, id: u32) -> Option<&mut Cohort> {
        let Some(idx) = self.cohort_index(id) else {
            return None;
        };
        Some(&mut self.cohorts.as_mut_slice()[idx])
    }

    /// The instant `cell`'s expiry is measured from — see [`live_since`].
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn live_since(&self, cell: &Cell) -> Instant {
        live_since(&self.cohorts, cell)
    }

    /// The callback-relative swoosh as a fault.
    #[inline]
    #[cfg(test)]
    pub fn restart_unobserved_swooshes(&mut self, now: Instant) {
        let mut ci = 0;
        while ci < self.cohorts.len() {
            let coh = &mut self.cohorts[ci];
            ci += 1;
            let unobserved = matches!(coh.phase, Phase::Laying | Phase::Grace);
            if !unobserved || coh.phase_at(coh.idle_s(now)).is_some() {
                continue;
            }
            let lead_s = if coh.flow.is_some() {
                1e-3
            } else if coh.grace_s.is_finite() {
                coh.grace_s.max(PHRASE_REST_MIN_S)
            } else {
                PHRASE_REST_MIN_S
            };
            let restart = std::time::Duration::try_from_secs_f32(lead_s)
                .ok()
                .and_then(|d| now.checked_sub(d))
                .unwrap_or(now);
            coh.alive_at = restart;
            if coh.flow.is_some() {
                coh.flow = Some(restart);
            }
        }
    }

    /// This frame's planned boundary samples, valid after [`Ribbon::plan`].
    #[inline]
    #[must_use]
    pub const fn plan_segments(&self) -> &[Segment] {
        self.plan.as_slice()
    }

    /// The glass's slabs a cell on the last plan (§15.2 1.8: [`glass_frame_price`]'s density, laid
    /// a whole number to each of the plan's vertices); `0` on a frame the bed does not draw by its
    /// glass.
    #[inline]
    #[must_use]
    pub const fn glass_slabs_per_cell(&self) -> usize {
        self.glass_slabs
    }

    /// The frame's vertex density.
    #[inline]
    #[must_use]
    pub const fn slabs_per_cell(&self) -> usize {
        if self.slabs < 1 {
            1
        } else if self.slabs > SLABS_PER_CELL {
            SLABS_PER_CELL
        } else {
            self.slabs
        }
    }

    /// The caret cell as last observed, or `None` before any.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn caret(&self) -> Option<(u16, u16)> {
        self.caret
    }

    /// The two profiles (§4).
    #[inline]
    #[must_use]
    pub const fn body_profile(cfg: &Config) -> BodyProfile {
        let dn_ch = if !cfg.ribbon_tall {
            UNDERLINE_DN_CH
        } else if Self::comet(cfg) {
            COMET_DN_HAND_CH
        } else {
            DN_TOP_CH
        };
        if cfg.ribbon_tall {
            BodyProfile {
                up_ch: TALL_UP_CH,
                dn_ch,
                core_up_ch: TALL_CORE_CH,
                shoulder: SHOULDER_TALL,
                tall: true,
            }
        } else {
            BodyProfile {
                up_ch: UNDERLINE_UP_CH,
                dn_ch,
                core_up_ch: LEAD_CH,
                shoulder: SHOULDER_UNDERLINE,
                tall: false,
            }
        }
    }

    /// Whether the comet body and the from-the-hand attack apply (`RAINBOW-KITTY-V2.md` §30).
    #[inline]
    #[must_use]
    pub const fn comet(cfg: &Config) -> bool {
        !cfg.ribbon_flat
    }

    /// Whether the vivid rail is drawn: under the comet body on either
    /// page, so the pet's ribbon keeps its under-edge on paper too, where
    /// without it the pet's and the flat ribbon read alike.
    #[inline]
    #[must_use]
    pub const fn rail_lit(cfg: &Config) -> bool {
        Self::comet(cfg)
    }

    /// Whether the flat ribbon lies on a dim theme's page ([`dim_theme`]):
    /// its body there lays only its hue at the page's luminance
    /// ([`dim_page_bed`]), and the warm half of the rainbow, at a luminance
    /// that low, is the page's own teal or olive (Solarized Dark's flat
    /// rainbow showed no rainbow past `jumps`, looks7 round 3). It lays a
    /// soft rail in the air below its letters ([`FLAT_DIM_RAIL_CORE`]).
    #[inline]
    #[must_use]
    pub const fn flat_dim(cfg: &Config) -> bool {
        !Self::comet(cfg) && cfg.dark_theme && dim_theme(cfg.theme_fg, cfg.theme_bg)
    }

    /// Whether a rail is drawn under the body ([`Self::rail_lit`],
    /// [`Self::flat_dim`]).
    #[inline]
    #[must_use]
    pub const fn rail_drawn(cfg: &Config) -> bool {
        Self::rail_lit(cfg) || Self::flat_dim(cfg)
    }

    /// The frame's rails laid clear of their row's ring, as `[from, to)`
    /// spans of the under stream, its quads' and its ramps', each marked
    /// whether it lies in its own row's foot band, clear of its letters' feet
    /// ([`crate::cursor_glow::InkLedger::note_rail_laid`],
    /// [`crate::cursor_glow::InkLedger::note_foot_rail_laid`]).
    #[inline]
    #[must_use]
    pub const fn rail_spans(&self) -> &[(StreamMark, StreamMark, bool)] {
        self.rail_spans.as_slice()
    }

    /// Whether the caret is a bar or an underline: the rail in the caret's row's foot band
    /// ends in a gap before its cell ([`CARET_CLEAR_PX`]). Under a block it runs on, and the
    /// block covers it.
    #[inline]
    pub const fn set_thin_caret(&mut self, thin: bool) {
        self.thin_caret = thin;
    }

    /// Forget the rails' spans: the frame drew none, or what it drew was cut away.
    #[inline]
    pub fn forget_rails(&mut self) {
        self.rail_spans.clear();
    }

    /// Forget that this frame's body drew by the glass: what it drew was cut away.
    #[inline]
    pub const fn unlay_glass(&mut self) {
        self.glass_drew = None;
        self.glass_span = None;
    }

    /// The span of the under stream the last frame's glass was laid in, `[from, to)` of its
    /// quads and ramps (§15.2 1.9: the glass is light-owned, the yield's to cut and never the
    /// letter guard's); `None` on a frame the glass did not draw or laid nothing.
    #[inline]
    #[must_use]
    pub const fn glass_span(&self) -> Option<(StreamMark, StreamMark)> {
        self.glass_span
    }

    /// Ingest one engine event at its own edge, before this frame's plan.
    #[inline]
    pub fn on_event(&mut self, ev: &super::Event, at: Instant, ctx: &Ctx<'_>) {
        // Under a curtain nothing is laid (§2.8): the cells on glass belong to an ended coordinate space.
        if self.curtain.is_some() {
            if let Event::Move { to, .. } = *ev {
                self.caret = Some(to);
            }
            return;
        }
        if matches!(
            *ev,
            Event::CancelComposerNewline
                | Event::Erase
                | Event::Kill { .. }
                | Event::Return
                | Event::Move {
                    licence: Licence::Nav | Licence::Synthetic,
                    ..
                }
        ) && matches!(self.fresh_line, Some(gate) if gate.awaiting_move)
        {
            self.fresh_line = None;
        }
        let revoke = match *ev {
            Event::Erase | Event::Kill { .. } | Event::Return | Event::ComposerNewline => {
                Some(ctx.caret.0)
            }
            Event::Rewrite { row, .. } => Some(row),
            Event::Move {
                from,
                licence: Licence::Nav | Licence::Return | Licence::Synthetic,
                ..
            } => Some(from.0),
            _ => None,
        };
        if let Some(row) = revoke {
            let mut ci = 0;
            while ci < self.cohorts.len() {
                if self.cohorts[ci].row == row {
                    self.cohorts[ci].revoke_releases(at);
                }
                ci += 1;
            }
        }
        match *ev {
            Event::Typed { cells, class, .. } => {
                // A key typed while the focus ember burns clears it, as a focus regain does.
                if self.ember_at.is_some() {
                    self.regain_focus(at);
                }
                self.note_key(at);
                self.pending_erase = 0;
                let landing = self.typed_landing(
                    ctx.caret,
                    cells,
                    ctx.geom.cols.min(u16::MAX as usize) as u16,
                );
                let landing = self.insert_landing(landing, cells);
                self.pen = Some(landing);
                let ctx = &Ctx {
                    caret: landing,
                    ..*ctx
                };
                if class == TypedClass::Space {
                    let (row, col_end) = landing;
                    if let Some(c) = col_end.checked_sub(1) {
                        self.note_space((row, c));
                    }
                }
                self.lay(cells, at, ctx);
            }
            Event::Erase => {
                self.pen = Some(ctx.caret);
                self.touch_hand(ctx.caret.0, ctx.caret.1);
                self.erase(ctx.caret, 1, RETRACT_FADE_S, at);
            }
            Event::Kill { cells, scope } => {
                self.pen = Some(ctx.caret);
                self.erase(ctx.caret, cells.max(1), kill_span_s(cells), at);
                if scope == super::KillScope::Line {
                    self.phrase_open = None;
                    self.arm_fresh_line(at, None);
                    self.mark_line_killed(ctx.caret.0);
                }
            }
            Event::Move {
                from, to, licence, ..
            } => {
                self.caret = Some(to);
                if !licence.is_echo() {
                    self.pen = Some(to);
                    self.arrived = Some(to);
                    self.touch_hand(to.0, to.1);
                }
                let same_row = to.0 == from.0;
                if licence == Licence::Return || (!same_row && licence == Licence::Nav) {
                    self.arm_fresh_line(at, if same_row { None } else { Some(from.0) });
                    if licence == Licence::Return
                        && let Some(gate) = &mut self.fresh_line
                    {
                        gate.awaiting_move = false;
                    }
                }
                if licence.is_echo() {
                    // An echo's motion is inert (its key or sweep laid the cells) except the hand leaving the row, an erase's retreat, and a re-anchor.
                    if !same_row {
                        // A repaint's home is not a gesture: only a row change the hand could have made ([`Ribbon::hand_changed_row`]) leaves the row; anything else under an echo licence moves only the caret mirror.
                        if Self::hand_changed_row(from, to) {
                            self.leave_row(from, to, at, licence);
                        }
                    } else if to.1 < from.1 {
                        if let Some(lift) = self.lift
                            && lift.origin == from
                            && lift.landing == to
                        {
                            self.lift_word(lift, at);
                        } else if self.pending_erase > 0 {
                            let erased = from.1 - to.1;
                            self.retract_suffix(to.0, to.1, erased, at, RETRACT_FADE_S);
                            self.pending_erase = self.pending_erase.saturating_sub(erased);
                        } else if licence == Licence::Typed
                            && !self.scroll_followed
                            && (from.1 - to.1 >= RE_ANCHOR_MIN_CELLS
                                || matches!(
                                    self.short_reanchor,
                                    Some(proof)
                                        if proof.from == from && proof.to == to && proof.at == at
                                )
                                || matches!(
                                    self.rewrap_start,
                                    Some(proof)
                                        if proof.from == from
                                            && proof.to == to
                                            && proof.width.is_some()
                                ))
                        {
                            self.re_anchor(from, to, at);
                            self.short_reanchor = None;
                        }
                    } else if to.1 > from.1 {
                        self.settle_carry(to.0, from.1, to.1, at);
                        self.settle_relocate(to.0, from.1, at, ctx);
                    }
                } else {
                    let scrub = same_row
                        && licence.is_keyed_nav()
                        && self.scrub(to.0, from.1, to.1, at, ctx);
                    let jump = !same_row || to.1.abs_diff(from.1) >= JUMP_MIN_CELLS;
                    if scrub {
                    } else if jump {
                        let wakes = same_row && licence.is_keyed_nav();
                        let keep = if wakes {
                            self.wake(to.0, from.1, to.1, at, ctx, WAKE_LIFE_S, true)
                        } else {
                            None
                        };
                        let program = licence == Licence::Pty;
                        let toward = if program {
                            Some((None, from.1, false))
                        } else if same_row {
                            Some((Some(to.0), to.1, to.1 < from.1))
                        } else {
                            None
                        };
                        if to.0 > from.0 && licence == Licence::Return {
                            self.print_row(from.0, at);
                        }
                        self.abandon(at, keep, toward, program);
                    } else if same_row && licence == Licence::Nav && from.1 != to.1 {
                        self.wake(
                            to.0,
                            from.1,
                            to.1,
                            at,
                            ctx,
                            WAKE_LIFE_S * WAKE_HOP_LIFE_SHARE,
                            false,
                        );
                    }
                }
            }
            Event::Focus(on) => {
                if on {
                    self.regain_focus(at);
                } else {
                    self.ember_at = Some(at);
                    self.rearm = None;
                    self.seam = None;
                }
            }
            Event::Sweep { row, col0, col1 } => {
                let (row, end) = self.insert_landing((row, col1), col1.saturating_sub(col0));
                let col0 = end.saturating_sub(col1.saturating_sub(col0));
                self.sweep(row, col0, end, at, ctx);
            }
            Event::Rewrite { row, col, cells } => {
                let span_s = kill_span_s(cells);
                self.retract_suffix(row, col, cells.max(1), at, span_s);
                self.caret = Some((row, col));
            }
            Event::Return => {
                self.phrase_open = None;
                self.arm_fresh_line(at, None);
            }
            Event::ComposerNewline => {
                self.phrase_open = None;
                self.arm_fresh_line(at, None);
                if let Some(gate) = &mut self.fresh_line {
                    gate.chord_at = Some(at);
                    gate.awaiting_move = true;
                }
            }
            Event::CancelComposerNewline => {}
            Event::ReducedMotion(_) => {}
        }
    }

    #[inline]
    fn arm_fresh_line(&mut self, at: Instant, left_row: Option<u16>) {
        self.seam = None;
        self.fresh_line = Some(match self.fresh_line {
            Some(gate) => FreshLine {
                since: gate.since.min(at),
                chord_at: gate.chord_at,
                awaiting_move: gate.awaiting_move,
                left_row: gate.left_row.or(left_row),
            },
            None => FreshLine {
                since: at,
                chord_at: None,
                awaiting_move: false,
                left_row,
            },
        });
    }

    /// Arm the seam on `row` at `at` ([`Ribbon::seam`]).
    #[inline]
    pub const fn arm_seam(&mut self, row: u16, at: Instant) {
        self.seam = Some(Seam { row, at });
    }

    /// The focused pane's `(first column, width)`; `None` is the whole grid.
    #[inline]
    pub const fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// The focused pane's `(first row, rows)`.
    #[inline]
    pub const fn set_pane_rows(&mut self, pane_rows: Option<(u16, u16)>) {
        self.pane_rows = pane_rows;
    }

    #[inline]
    const fn pane_bot(&self, ctx: &Ctx<'_>, chf: f32) -> f32 {
        let grid = ctx.geom.fx_bot() as f32;
        match self.pane_rows {
            Some((row0, rows)) => {
                (ctx.geom.origin_y as f32 + row0.saturating_add(rows) as f32 * chf).min(grid)
            }
            None => grid,
        }
    }

    /// Drop the curtain ([`CURTAIN_S`]).
    #[inline]
    pub fn curtain(&mut self, at: Instant) {
        self.phrase_open = None;
        self.arm_fresh_line(at, None);
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = None;
            gate.chord_at = None;
            gate.awaiting_move = false;
        }
        self.clear_space_transient_state();
        if self.cells.is_empty() || self.curtain.is_some() {
            return;
        }
        let hand = self.curtain_hand();
        let mut i = 0;
        while i < self.cohorts.len() {
            // A cohort already retracting froze its target when the hand let go of it; that is the same fact, measured earlier, and it stays.
            if self.cohorts[i].retract_col.is_none() {
                let coh = self.cohorts[i];
                let (target, right_first) = Self::target_toward(&coh, hand);
                self.cohorts[i].retract_col = target;
                self.cohorts[i].drain_right_first = right_first;
            }
            i += 1;
        }
        self.curtain = Some(at);
    }

    #[inline]
    fn curtain_hand(&self) -> Option<(u16, u16)> {
        let mut best_non_wake: Option<&Cohort> = None;
        let mut best_any: Option<&Cohort> = None;
        let cohorts = self.cohorts.as_slice();
        let mut i = 0;
        while i < cohorts.len() {
            let c = &cohorts[i];
            if !c.wake && !matches!(best_non_wake, Some(prev) if !is_fresher_or_equal(c, prev)) {
                best_non_wake = Some(c);
            }
            if !matches!(best_any, Some(prev) if !is_fresher_or_equal(c, prev)) {
                best_any = Some(c);
            }
            i += 1;
        }
        let c = best_non_wake.or(best_any)?;
        Some((c.row, c.col1))
    }

    /// Whether a curtain is falling.
    #[inline]
    #[must_use]
    pub const fn curtained(&self) -> bool {
        self.curtain.is_some()
    }

    /// The phrase rest in force, seconds.
    #[inline]
    #[must_use]
    pub const fn rest_s(&self) -> f32 {
        let ioi = if self.ioi_ms > 0.0 {
            self.ioi_ms
        } else {
            IOI_DEFAULT_MS
        };
        phrase_rest_ms(ioi) / 1000.0
    }

    /// The open phrase's id — read-only, for the twins.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn phrase_open(&self) -> Option<u32> {
        self.phrase_open
    }

    /// The hand's structural set (`docs/design/PRISM-GLASS.md` §14 amendment 4, §15.2 1.10), as
    /// `(top, bottom)` rows, inclusive: the hand's row `hand` and the contiguous rows beside it
    /// that each hold a live cohort of the hand's open phrase that is neither abandoned nor a
    /// wake, the [`STRUCTURAL_ROWS_MAX`] rows nearest the hand at most. Just the hand's row when no
    /// phrase is open or no row beside it holds one.
    #[inline]
    #[must_use]
    pub const fn structural_rows(&self, hand: u16) -> (u16, u16) {
        let Some(open) = self.phrase_open else {
            return (hand, hand);
        };
        let (mut top, mut bottom) = (hand, hand);
        let (mut up, mut down) = (true, true);
        while (up || down) && bottom - top + 1 < STRUCTURAL_ROWS_MAX {
            if up {
                match top.checked_sub(1) {
                    Some(row) if self.holds_phrase_on(open, row) => top = row,
                    _ => up = false,
                }
            }
            if down && bottom - top + 1 < STRUCTURAL_ROWS_MAX {
                match bottom.checked_add(1) {
                    Some(row) if self.holds_phrase_on(open, row) => bottom = row,
                    _ => down = false,
                }
            }
        }
        (top, bottom)
    }

    /// Whether `row` holds a cohort of phrase `open` that counts toward the structural set.
    #[inline]
    const fn holds_phrase_on(&self, open: u32, row: u16) -> bool {
        let cohorts = self.cohorts.as_slice();
        let mut i = 0;
        while i < cohorts.len() {
            if cohorts[i].row == row && Self::of_phrase(&cohorts[i], open) {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Whether `coh` is a live cohort of phrase `open`, neither abandoned nor a wake
    /// ([`Self::structural_rows`]).
    #[inline]
    const fn of_phrase(coh: &Cohort, open: u32) -> bool {
        coh.phrase == open && !coh.abandoned && !coh.wake && !matches!(coh.phase, Phase::Fading)
    }

    /// The wrap's side table ([`GlassWrap`]) for the frame at `ctx` (§2.4, §15.2 1.10). Under
    /// the glass: each cohort of the hand's open phrase on a row of its structural set the hand is
    /// not on is recorded left now (on a row another record already holds, at that record's
    /// instant, so a cohort merged or relaid there keeps its row's sink); the row the hand is on
    /// keeps none (Backspace clears it); a record whose cohort no longer stands on its row is
    /// dropped. On any other frame the table is empty, so the glass's falling edge clears it.
    #[inline]
    fn sync_glass_wrap(&mut self, ctx: &Ctx<'_>) {
        if !self.ink.glass_lit() {
            self.glass_wrap.clear();
            self.wrap_hand = None;
            return;
        }
        #[cfg(any(test, debug_assertions))]
        count_glass_exit();
        let hand = ctx.caret.0;
        let (top, bottom) = self.structural_rows(hand);
        self.wrap_hand = if bottom > top { Some(hand) } else { None };
        self.glass_wrap.retain(|w| w.row != hand);
        if let Some(open) = self.phrase_open
            && bottom > top
        {
            for coh in &self.cohorts {
                if coh.row == hand
                    || coh.row < top
                    || coh.row > bottom
                    || !Self::of_phrase(coh, open)
                    || self
                        .glass_wrap
                        .iter()
                        .any(|w| w.cohort == coh.id && w.row == coh.row)
                {
                    continue;
                }
                let left_at = self
                    .glass_wrap
                    .iter()
                    .filter(|w| w.row == coh.row)
                    .map(|w| w.left_at)
                    .min()
                    .unwrap_or(ctx.now);
                self.glass_wrap.push(GlassWrap {
                    cohort: coh.id,
                    row: coh.row,
                    left_at,
                });
            }
        }
        let cohorts = &self.cohorts;
        self.glass_wrap
            .retain(|w| cohorts.iter().any(|c| c.id == w.cohort && c.row == w.row));
    }

    /// How far run `cohort`'s glass on `row` has sunk at `now` ([`GlassWrap::sunk`]); `0` on a
    /// row the hand has not left.
    #[inline]
    fn sunk_on(&self, cohort: u32, row: u16, now: Instant) -> f32 {
        #[cfg(test)]
        if self.wrap_unsunk {
            return 0.0;
        }
        match self
            .glass_wrap
            .iter()
            .find(|w| w.cohort == cohort && w.row == row)
        {
            Some(w) => w.sunk(now, self.reduced),
            None => 0.0,
        }
    }

    /// A carry's half of the wrap's table ([`Self::translate_run`]): the record of `cohort` on
    /// `row` goes to `new_row` with its cells, under the carried part's own cohort `id` when the
    /// carry split it (the part left behind keeping its own), and is dropped when `new_row` is off
    /// the pane.
    #[inline]
    fn carry_glass_wrap(&mut self, cohort: u32, row: u16, id: u32, new_row: u16) {
        let Some(i) = self
            .glass_wrap
            .iter()
            .position(|w| w.cohort == cohort && w.row == row)
        else {
            return;
        };
        let on_pane = match self.pane_rows {
            Some((row0, rows)) => new_row >= row0 && new_row < row0.saturating_add(rows),
            None => true,
        };
        if id != cohort {
            if on_pane {
                let carried = GlassWrap {
                    cohort: id,
                    row: new_row,
                    ..self.glass_wrap[i]
                };
                self.glass_wrap.push(carried);
            }
        } else if on_pane {
            self.glass_wrap[i].row = new_row;
        } else {
            self.glass_wrap.remove(i);
        }
    }

    /// The rows of the hand's structural set the hand has left, light only ([`GlassWrap`]):
    /// empty on every frame the bed draws by main's table.
    #[inline]
    #[must_use]
    pub const fn glass_wraps(&self) -> &[GlassWrap] {
        self.glass_wrap.as_slice()
    }

    /// The hand's row when its structural set spans more than one row on a glass frame, which
    /// keeps [`WRAP_HAND_TOP_PX`] of page above its glass; `None` on every other frame.
    #[inline]
    #[must_use]
    pub const fn wrap_hand(&self) -> Option<u16> {
        self.wrap_hand
    }

    #[inline]
    fn note_key(&mut self, at: Instant) {
        let (first, rest, gap) = match self.last_key_at {
            Some(t) => {
                let g = (at.saturating_duration_since(t).as_secs_f32() * 1000.0).round();
                (
                    false,
                    g >= self.rest_s() * 1000.0,
                    g.clamp(0.0, f32::from(u16::MAX) * 64.0) as u32,
                )
            }
            None => (true, true, 0),
        };
        if rest || self.phrase_open.is_none() {
            self.open_phrase();
        }
        let ioi = if self.ioi_ms > 0.0 {
            self.ioi_ms
        } else {
            IOI_DEFAULT_MS
        };
        self.ioi_ms = ioi_next(ioi, gap, first, rest);
        self.last_key_at = Some(at);
    }

    #[inline]
    const fn open_phrase(&mut self) {
        let id = if self.next_phrase > 1 {
            self.next_phrase
        } else {
            1
        };
        let next = id.wrapping_add(1);
        self.next_phrase = if next > 1 { next } else { 1 };
        self.phrase_open = Some(id);
    }

    #[inline]
    const fn phrase_for_lay(&mut self) -> u32 {
        if self.phrase_open.is_none() {
            self.open_phrase();
        }
        match self.phrase_open {
            Some(id) => id,
            None => 1,
        }
    }

    #[inline]
    fn hold_phrase(&mut self, at: Instant, row: u16) {
        let rested = if let Some(t) = self.last_key_at {
            at.saturating_duration_since(t).as_secs_f32() >= self.rest_s()
        } else {
            false
        };
        let open = match self.phrase_open {
            Some(open) if !rested => open,
            _ => {
                self.open_phrase();
                let id = self.phrase_open.unwrap_or(1);
                let mut ci = 0;
                while ci < self.cohorts.len() {
                    let coh = &mut self.cohorts[ci];
                    if !coh.abandoned && coh.row == row {
                        coh.phrase = id;
                    }
                    ci += 1;
                }
                id
            }
        };
        self.last_key_at = Some(at);
        self.hold(open, row, None, at);
    }

    #[inline]
    fn hold(&mut self, open: u32, hand_row: u16, laid_into: Option<usize>, at: Instant) {
        #[inline]
        const fn held_by(
            i: usize,
            c: &Cohort,
            open: u32,
            hand_row: u16,
            laid_into: Option<usize>,
        ) -> bool {
            !c.abandoned
                && c.phrase == open
                && (c.row == hand_row || matches!(laid_into, Some(idx) if idx == i))
        }
        let grace = self.rest_s();
        let floor = swoosh_life_s(grace);
        let mut held_count = 0usize;
        let mut single_held_id = 0u32;
        let mut i = 0;
        while i < self.cohorts.len() {
            let coh = &mut self.cohorts[i];
            if held_by(i, coh, open, hand_row, laid_into) {
                held_count += 1;
                single_held_id = coh.id;
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
                coh.grace_s = grace;
                if coh.flow.take().is_some() {
                    coh.flow_drift = 0.0;
                }
            }
            i += 1;
        }
        if held_count == 0 {
            return;
        }
        if held_count == 1 {
            let mut ci = 0;
            while ci < self.cells.len() {
                let cell = &mut self.cells[ci];
                if cell.cohort == single_held_id && cell.life_s < floor {
                    cell.life_s = floor;
                }
                ci += 1;
            }
            return;
        }
        let cohorts = &self.cohorts;
        let mut cached_coh: Option<(u32, bool)> = None;
        let mut ci = 0;
        while ci < self.cells.len() {
            let cell = &mut self.cells[ci];
            ci += 1;
            if cell.life_s >= floor {
                continue;
            }
            let held = match cached_coh {
                Some((id, h)) if id == cell.cohort => h,
                _ => {
                    let mut h = false;
                    let mut i = 0;
                    while i < cohorts.len() {
                        let c = &cohorts[i];
                        if c.id == cell.cohort && held_by(i, c, open, hand_row, laid_into) {
                            h = true;
                            break;
                        }
                        i += 1;
                    }
                    cached_coh = Some((cell.cohort, h));
                    h
                }
            };
            if held {
                cell.life_s = floor;
            }
        }
    }

    #[inline]
    fn take_row_back(&mut self, open: u32, row: u16, at: Instant) {
        let mut ci = 0;
        while ci < self.cohorts.len() {
            let coh = &mut self.cohorts[ci];
            if !coh.abandoned && coh.row == row && coh.flow.is_some() {
                coh.phrase = open;
            }
            ci += 1;
        }
        self.hold(open, row, None, at);
    }

    #[inline]
    const fn retract_target(&self, coh: &Cohort) -> (Option<u16>, bool) {
        Self::target_toward(coh, self.caret)
    }

    #[inline]
    fn flow_target(&self, coh: &Cohort) -> (Option<u16>, bool, f32) {
        let below = matches!(self.caret, Some((row, _)) if coh.row > row);
        let mut lo = u16::MAX;
        let mut hi = 0u16;
        let mut lo_born: Option<Instant> = None;
        let mut lo_disp = 0.0_f32;
        let mut hi_born: Option<Instant> = None;
        let mut hi_disp = 0.0_f32;
        let mut has_leaving = false;
        let cells = self.cells.as_slice();
        let mut ci = 0;
        while ci < cells.len() {
            let c = &cells[ci];
            if c.row == coh.row && c.cohort == coh.id {
                if !c.leaving() {
                    if c.col < lo {
                        lo = c.col;
                        lo_born = Some(c.born);
                        lo_disp = c.birth_disp;
                    } else if c.col == lo && !matches!(lo_born, Some(b) if c.born < b) {
                        lo_born = Some(c.born);
                        lo_disp = c.birth_disp;
                    }
                    if c.col > hi {
                        hi = c.col;
                        hi_born = Some(c.born);
                        hi_disp = c.birth_disp;
                    } else if c.col == hi && !matches!(hi_born, Some(b) if c.born < b) {
                        hi_born = Some(c.born);
                        hi_disp = c.birth_disp;
                    }
                } else {
                    has_leaving = true;
                }
            }
            ci += 1;
        }
        let no_staying = lo > hi;
        let (lo, hi) = if no_staying {
            (coh.col0, coh.col1.saturating_sub(1).max(coh.col0))
        } else {
            (lo, hi)
        };
        let edge = if below { lo } else { hi };
        let disp = if !no_staying && !has_leaving {
            if below { lo_disp } else { hi_disp }
        } else {
            let mut best_born: Option<Instant> = None;
            let mut disp = 0.0_f32;
            let mut ci = 0;
            while ci < cells.len() {
                let c = &cells[ci];
                if c.row == coh.row
                    && c.cohort == coh.id
                    && c.col == edge
                    && !matches!(best_born, Some(b) if c.born < b)
                {
                    best_born = Some(c.born);
                    disp = c.birth_disp;
                }
                ci += 1;
            }
            disp
        };
        let heat = smoothstep01((disp - HOT_EDGE_DISP_MIN) * (1.0 / HOT_EDGE_DISP_SPAN));
        let kept = f32::from(hi - lo) + 1.0;
        let drift = (FLOW_DRIFT_MIN_CELLS + (FLOW_DRIFT_MAX_CELLS - FLOW_DRIFT_MIN_CELLS) * heat)
            .min(FLOW_DRIFT_MAX_SHARE * kept);
        (Some(edge), below, drift)
    }

    #[inline]
    const fn target_toward(coh: &Cohort, hand: Option<(u16, u16)>) -> (Option<u16>, bool) {
        match hand {
            None => (None, false),
            Some((hrow, hcol)) if coh.row == hrow => (Some(hcol), false),
            Some((hrow, _)) if coh.row < hrow => {
                let sub = coh.col1.saturating_sub(1);
                (Some(if sub > coh.col0 { sub } else { coh.col0 }), false)
            }
            Some(_) => (Some(coh.col0), true),
        }
    }

    #[inline]
    fn erase(&mut self, caret: (u16, u16), cells: u16, span_s: f32, at: Instant) {
        self.retract_suffix(caret.0, caret.1, cells, at, span_s);
        self.pending_erase = self.pending_erase.saturating_add(cells);
        self.hold_phrase(at, caret.0);
    }

    #[inline]
    fn alive_at(&self, cell: &Cell, at: Instant) -> bool {
        !cell.leaving()
            && at
                .saturating_duration_since(live_since(&self.cohorts, cell))
                .as_secs_f32()
                < cell.life_s
    }

    #[inline]
    fn owned(&self, row: u16, col: u16, at: Instant) -> bool {
        let cells = self.cells.as_slice();
        let mut i = 0;
        while i < cells.len() {
            let c = &cells[i];
            if c.row == row && c.col == col && self.alive_at(c, at) {
                return true;
            }
            i += 1;
        }
        false
    }

    /// A key whose echo has not landed is held, not laid.
    #[inline]
    pub(super) fn hold_typed(
        &mut self,
        ev: &super::Event,
        at: Instant,
        ctx: &Ctx<'_>,
        cell: Option<(u16, u16)>,
    ) {
        if let Event::Typed { class, .. } = *ev {
            self.pending_erase = 0;
            if class == TypedClass::Space
                && let Some(cell) = cell
            {
                self.note_space(cell);
            }
            self.hold_phrase(at, ctx.caret.0);
        }
    }

    #[inline]
    fn sweep(&mut self, row: u16, col0: u16, col1: u16, at: Instant, ctx: &Ctx<'_>) {
        let cols = ctx.geom.cols.min(u16::MAX as usize) as u16;
        let col_end = col1.min(cols);
        let mut col = col0;
        while col < col_end {
            let mut own = false;
            let mut ci = 0;
            while ci < self.cells.len() {
                let c = &self.cells[ci];
                if c.row == row && c.col == col && c.born >= at && self.alive_at(c, at) {
                    own = true;
                    break;
                }
                ci += 1;
            }
            if !own {
                self.lay_cell(row, col, at, ctx, true);
            }
            col += 1;
        }
    }

    #[inline]
    fn regain_focus(&mut self, at: Instant) {
        if let Some(ember) = self.ember_at.take() {
            let r = at.saturating_duration_since(ember).as_secs_f32() * (1.0 / FOCUS_EMBER_S);
            if r < 1.0 {
                self.rearm = Some((at, spend(clamp01(r))));
            }
        }
    }

    #[inline]
    fn lay(&mut self, n: u16, at: Instant, ctx: &Ctx<'_>) {
        if n == 0 || ctx.geom.cols == 0 {
            return;
        }
        let cols = ctx.geom.cols.min(i32::MAX as usize) as i32;
        let (pane_col0, pane_cols) = match self.pane {
            Some((c0, w)) => (i32::from(c0), i32::from(w).max(1)),
            None => (0, cols),
        };
        let pane_col1 = pane_col0.saturating_add(pane_cols).min(cols);
        let (row, col_end) = ctx.caret;
        let mut back = i32::from(n);
        while back >= 1 {
            let mut c = i32::from(col_end) - back;
            back -= 1;
            let mut r = i32::from(row);
            while c < pane_col0 {
                r -= 1;
                c += pane_cols;
            }
            if r < 0 || c >= pane_col1 || r > i32::from(u16::MAX) || c > i32::from(u16::MAX) {
                continue;
            }
            self.lay_cell(r as u16, c as u16, at, ctx, true);
        }
    }

    #[inline]
    const fn birth_price(ctx: &Ctx<'_>) -> f32 {
        clamp01(match ctx.mend {
            Some(m) => ctx.birth_disp.max(m.disp),
            None => ctx.birth_disp,
        })
    }

    #[inline]
    fn cell_life(&self, at: Instant, ctx: &Ctx<'_>) -> f32 {
        let full = ctx.cfg.duration.as_secs_f32().max(0.001);
        let base = (full * LIFE_DURATION_GAIN).clamp(LIFE_BASE_MIN, LIFE_BASE_MAX);
        let d = Self::birth_price(ctx);
        let life = base * (1.0 + LIFE_SPINE_LINEAR * d + LIFE_SPINE_SQUARE * d * d);
        let mut newest: Option<Instant> = None;
        let cohorts = self.cohorts.as_slice();
        let mut ci = 0;
        while ci < cohorts.len() {
            let c = &cohorts[ci];
            if !matches!(newest, Some(m) if c.alive_at <= m) {
                newest = Some(c.alive_at);
            }
            ci += 1;
        }
        let life = if let Some(newest) = newest {
            let gap = at.saturating_duration_since(newest).as_secs_f32();
            if gap <= CHAIN_GAP_MAX {
                life.max((CHAIN_KEYS * gap + CHAIN_MARGIN).min(CHAIN_LIFE_MAX))
            } else {
                life
            }
        } else {
            life
        };
        life.max(swoosh_life_s(self.rest_s()))
    }

    #[inline]
    #[must_use]
    const fn hand_changed_row(from: (u16, u16), to: (u16, u16)) -> bool {
        if to.0 == from.0.saturating_add(1) {
            return true;
        }
        from.0 == to.0.saturating_add(1) && to.1 > from.1
    }

    #[inline]
    #[must_use]
    const fn insert_landing(&self, landing: (u16, u16), cells: u16) -> (u16, u16) {
        if cells == 0 {
            return landing;
        }
        let (row, end) = landing;
        let first = end.saturating_sub(cells);
        let shifted = self.shifted_now.as_slice();
        let mut i = 0;
        while i < shifted.len() {
            let (r, lo, hi, dc) = shifted[i];
            i += 1;
            if dc < 0 {
                continue;
            }
            let dc = dc as u16;
            if r != row || cells > dc {
                continue;
            }
            let (moved0, moved1) = (lo.saturating_add(dc), hi.saturating_add(dc));
            if first >= moved0 && end.saturating_sub(1) <= moved1 {
                return (row, moved0);
            }
        }
        landing
    }

    #[inline]
    #[must_use]
    const fn typed_landing(&self, caret: (u16, u16), cells: u16, cols: u16) -> (u16, u16) {
        let Some(pen) = self.pen else {
            return caret;
        };
        if cells == 0 {
            return caret;
        }
        if caret.0 >= pen.0 {
            let top = self.fold_top_row(caret, cells, cols);
            if top == caret.0 as i32 || top == pen.0 as i32 {
                return caret;
            }
        } else if Self::hand_changed_row(pen, caret) {
            return caret;
        }
        let cell_slice = self.cells.as_slice();
        let mut has_pen_row = false;
        let mut i = 0;
        while i < cell_slice.len() {
            if cell_slice[i].row == pen.0 {
                has_pen_row = true;
                break;
            }
            i += 1;
        }
        if !has_pen_row {
            return caret;
        }
        let step = if cells > 1 { cells } else { 1 };
        (pen.0, pen.1.saturating_add(step))
    }

    #[inline]
    #[must_use]
    const fn fold_pane(&self, cols: u16) -> (i32, i32) {
        let (c0, w) = match self.pane {
            Some(p) => p,
            None => (0, cols),
        };
        (c0 as i32, if w > 1 { w as i32 } else { 1 })
    }

    #[inline]
    #[must_use]
    const fn fold_top_row(&self, caret: (u16, u16), cells: u16, cols: u16) -> i32 {
        let (pane_col0, pane_cols) = self.fold_pane(cols);
        let left = caret.1 as i32 - cells as i32 - pane_col0;
        let d = left.div_euclid(pane_cols);
        caret.0 as i32 + if d < 0 { d } else { 0 }
    }

    #[inline]
    #[must_use]
    const fn folded_down(from: (u16, u16), to: (u16, u16), licence: Licence) -> bool {
        licence.is_echo() && to.0 == from.0.saturating_add(1)
    }

    #[inline]
    fn leave_row(&mut self, from: (u16, u16), to: (u16, u16), at: Instant, licence: Licence) {
        self.carry = None;
        if let Some(sw) = self.soft_wrap
            && sw.origin == from
            && sw.landing == to
            && Self::folded_down(from, to, licence)
        {
            self.arm_seam(to.0, at);
            self.flow_row(from.0, at);
            self.pending_erase = 0;
            self.last_space = None;
            self.prev_space = None;
            self.arrived = None;
            self.relocate = None;
            self.carry = Some((sw, at));
            return;
        }
        let pane_col0 = match self.pane {
            Some((c0, _)) => c0,
            None => 0,
        };
        let w = if let Some(rewrap) = self.rewrap_start
            && rewrap.from == from
            && rewrap.to == to
            && let Some(width) = rewrap.width
        {
            width
        } else {
            self.moved_word_len(from, at)
        };
        let held_after = self.space_held_after_the_wrap(from);
        let re_wrapped = licence == Licence::Typed && to.1 > pane_col0.saturating_add(1);
        let origin = if re_wrapped {
            self.relay_origin(from, w)
        } else {
            None
        };
        if re_wrapped {
            self.split_off = None;
        }
        if Self::folded_down(from, to, licence) {
            self.arm_seam(to.0, at);
            if re_wrapped && w > 0 {
                let moved = (w + 1).min(from.1.saturating_sub(pane_col0));
                self.retract_span(from.0, from.1 - moved, moved, at, RETRACT_FADE_S);
            }
            self.flow_row(from.0, at);
        } else {
            // A line the hand left downward by anything but a navigation (a Return, the line
            // feed after it) is printed text now: its light dims to [`RETURN_PRINTED_SHARE`]
            // over [`RETURN_DIM_S`], clear of its letters by the time the output lands (at full
            // light the band under its last word holds it under 4.5:1 on paper), and goes on
            // out through the swoosh's retract.
            if to.0 > from.0 && licence != Licence::Nav {
                self.print_row(from.0, at);
            }
            self.abandon_row(from.0, at, Some(from.1));
            if licence == Licence::Typed
                && self.pending_erase > 0
                && to.0.checked_add(1) == Some(from.0)
                && let Some(open) = self.phrase_open
            {
                self.take_row_back(open, to.0, at);
            }
        }
        self.pending_erase = 0;
        self.reseat_spaces(held_after, to);
        self.arrived = None;
        self.relocate = None;
        let rewrap = self.take_rewrap_start(from, to);
        if re_wrapped {
            if let Some(rewrap) = rewrap
                && let Some(end) = rewrap.start.checked_add(w)
                && end < to.1
            {
                self.relay_word(to.0, rewrap.start, end, at, origin);
                if let Some(space) = rewrap.space {
                    self.note_space((to.0, space));
                }
            } else {
                self.begin_relocate(to, w, at, origin);
            }
        }
    }

    #[inline]
    fn flow_row(&mut self, row: u16, at: Instant) {
        let mut ci = 0;
        while ci < self.cohorts.len() {
            if self.cohorts[ci].row == row {
                Self::flow_cohort(&mut self.cohorts[ci], at);
            }
            ci += 1;
        }
    }

    #[inline]
    fn flow_cohort(coh: &mut Cohort, at: Instant) {
        if coh.abandoned || coh.wake || coh.flow.is_some() {
            return;
        }
        if !matches!(
            Phase::at(
                at.saturating_duration_since(coh.alive_at).as_secs_f32(),
                coh.grace_s
            ),
            Some(Phase::Laying | Phase::Grace | Phase::Reaching)
        ) {
            return;
        }
        coh.flow = Some(at);
        coh.release_clock = None;
    }

    #[inline]
    #[must_use]
    const fn relay_origin(&self, from: (u16, u16), w: u16) -> Option<WalkPos> {
        if w == 0 {
            return None;
        }
        let Some(last) = from.1.checked_sub(1) else {
            return None;
        };
        let Some(first) = from.1.checked_sub(w) else {
            return None;
        };
        let cells = self.cells.as_slice();
        let cohorts = self.cohorts.as_slice();
        let mut i = cells.len();
        while i > 0 {
            i -= 1;
            let c = &cells[i];
            if c.row == from.0 && c.col == last {
                return match Cohort::find(cohorts, c.cohort) {
                    Some(coh) => Some(coh.walk_at(first)),
                    None => None,
                };
            }
        }
        let Some(split) = self.split_off else {
            return None;
        };
        if last < split.col0 || last >= split.col1 {
            return None;
        }
        let mut k = 0;
        while k < cohorts.len() {
            let copy = &cohorts[k];
            if copy.id == split.copy {
                let Some(neg_dr) = split.dr.checked_neg() else {
                    return None;
                };
                return if matches!(copy.row.checked_add_signed(neg_dr), Some(r) if r == from.0) {
                    Some(copy.walk_at(first))
                } else {
                    None
                };
            }
            k += 1;
        }
        None
    }

    #[inline]
    fn re_anchor(&mut self, from: (u16, u16), to: (u16, u16), at: Instant) {
        let n = from.1 - to.1;
        let rewrap = self.take_rewrap_start(from, to);
        let w = match rewrap {
            Some(RewrapStart {
                width: Some(width), ..
            }) => width,
            _ => self.moved_word_len(from, at),
        };
        let held_after = self.space_held_after_the_wrap(from);
        let origin = self.relay_origin(from, w);
        self.split_off = None;
        self.retract_suffix(to.0, to.1, n, at, RETRACT_DUR_S);
        self.reseat_spaces(held_after, to);
        if let Some(RewrapStart {
            space: Some(space), ..
        }) = rewrap
        {
            self.note_space((to.0, space));
        }
        self.arrived = None;
        if let Some(raw_floor) = self.hand_floor_on(to.0) {
            let floor = match rewrap {
                Some(r) => raw_floor.min(r.start),
                None => raw_floor,
            };
            if to.1.saturating_sub(1).saturating_sub(w) < floor {
                return;
            }
        }
        if let (Some(r), Some(glyph)) = (rewrap, to.1.checked_sub(1))
            && r.start <= glyph.saturating_sub(w)
            && r.start < glyph
        {
            self.relocate = None;
            self.relay_word(to.0, r.start, to.1, at, origin);
            return;
        }
        self.begin_relocate(to, w, at, origin);
    }

    /// The content witness says the text this typed rewrap relays starts at `start` on its landing row.
    #[inline]
    pub(super) const fn note_rewrap_start(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        start: u16,
        width: Option<u16>,
        space: Option<u16>,
    ) {
        // The follow pass may also sample an earlier buffered move.
        if width.is_none() && matches!(self.rewrap_start, Some(RewrapStart { width: Some(_), .. }))
        {
            return;
        }
        self.rewrap_start = Some(RewrapStart {
            from,
            to,
            start,
            width,
            space,
        });
    }

    #[inline]
    const fn take_rewrap_start(&mut self, from: (u16, u16), to: (u16, u16)) -> Option<RewrapStart> {
        let Some(rewrap) = self.rewrap_start else {
            return None;
        };
        if rewrap.from.0 != from.0
            || rewrap.from.1 != from.1
            || rewrap.to.0 != to.0
            || rewrap.to.1 != to.1
        {
            return None;
        }
        self.rewrap_start = None;
        Some(rewrap)
    }

    /// Correct a Space's cell from an exact ordered key batch and its licensed same-row sweep.
    #[inline]
    pub(super) const fn observe_space_cell(&mut self, row: u16, col: u16) {
        self.note_space((row, col));
    }

    #[inline]
    const fn note_space(&mut self, cell: (u16, u16)) {
        if let Some((r, c)) = self.last_space {
            if r != cell.0 || c != cell.1 {
                self.prev_space = self.last_space;
            }
        } else {
            self.prev_space = None;
        }
        self.last_space = Some(cell);
    }

    #[inline]
    const fn word_space(&self, from: (u16, u16)) -> Option<(u16, u16)> {
        match self.last_space {
            Some(cell) if cell.0 == from.0 && cell.1 == from.1 => self.prev_space,
            other => other,
        }
    }

    #[inline]
    const fn space_held_after_the_wrap(&self, from: (u16, u16)) -> bool {
        matches!(self.last_space, Some((r, c)) if r == from.0 && c == from.1)
    }

    #[inline]
    const fn reseat_spaces(&mut self, held_after: bool, to: (u16, u16)) {
        self.prev_space = None;
        self.last_space = if held_after { Some(to) } else { None };
    }

    /// Replay the `Move` the seam judged a soft-wrapped caret ([`SoftWrap`]).
    #[inline]
    pub(super) fn soft_wrap_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, sw: SoftWrap) {
        self.soft_wrap = Some(sw);
        self.on_event(ev, at, ctx);
        self.soft_wrap = None;
    }

    /// Replay the `Move` the seam judged a lifted word ([`Lift`]).
    #[inline]
    pub(super) fn lift_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, lift: Lift) {
        self.lift = Some(lift);
        self.on_event(ev, at, ctx);
        self.lift = None;
    }

    /// Replay the `Move` whose echo the seam saw carry the soft-wrapped word `sw` off its row ([`Ribbon::settle_carry`]).
    #[inline]
    pub(super) fn reflow_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, sw: SoftWrap) {
        self.reflow = Some(sw);
        self.on_event(ev, at, ctx);
        self.reflow = None;
    }

    /// Replay the `Sweep` the seam named the revive of a retired typed run ([`Revive`]).
    #[inline]
    pub(super) fn revive_sweep(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, rv: Revive) {
        self.revive = Some(rv);
        self.on_event(ev, at, ctx);
        self.revive = None;
    }

    /// The soft-wrapped word waiting for the key that reflows it, if any.
    #[inline]
    #[must_use]
    pub(super) const fn carry(&self) -> Option<SoftWrap> {
        match self.carry {
            Some((sw, _)) => Some(sw),
            None => None,
        }
    }

    #[inline]
    fn lift_word(&mut self, lift: Lift, at: Instant) {
        let (row, end) = lift.origin;
        let col0 = lift.landing.1;
        self.last_space = None;
        self.prev_space = None;
        self.arrived = None;
        self.relocate = None;
        let mut lit0 = end;
        while lit0 > col0 && self.owned(row, lit0 - 1, at) {
            lit0 -= 1;
        }
        if lit0 == end {
            return;
        }
        let lit = end - lit0;
        let t0 = self.relay_origin((row, end), lit);
        self.retract_span(row, lit0, lit, at, RETRACT_FADE_S);
        let (dst_row, dst0) = lift.dst;
        self.relay_word(
            dst_row,
            dst0.saturating_add(lit0 - col0),
            dst0.saturating_add(end - col0),
            at,
            t0,
        );
        self.flow_row(dst_row, at);
    }

    #[inline]
    fn settle_carry(&mut self, row: u16, from_col: u16, to_col: u16, at: Instant) {
        let Some((sw, wrapped_at)) = self.carry else {
            return;
        };
        let (landing_row, landing) = sw.landing;
        if landing_row != row || from_col != landing {
            return;
        }
        self.carry = None;
        let (src_row, src_end) = (sw.origin.0, sw.origin.1.saturating_add(1));
        let n = src_end.saturating_sub(sw.word_col0);
        if n == 0
            || self.reflow != Some(sw)
            || at.saturating_duration_since(wrapped_at) > RELOCATE_PATIENCE_DUR
            || to_col <= landing.saturating_add(n)
        {
            return;
        }
        let mut lit0 = src_end;
        while lit0 > sw.word_col0 && self.owned(src_row, lit0 - 1, at) {
            lit0 -= 1;
        }
        if lit0 == src_end {
            return;
        }
        let lit = src_end - lit0;
        let t0 = self.relay_origin((src_row, src_end), lit);
        self.retract_span(src_row, lit0, lit, at, RETRACT_FADE_S);
        let dst0 = landing + (lit0 - sw.word_col0);
        self.relay_word(row, dst0, to_col.min(landing + n + 1), at, t0);
    }

    /// Cells of the word the app moved off `from`'s row.
    #[inline]
    pub(crate) fn moved_word_len(&self, from: (u16, u16), at: Instant) -> u16 {
        let after_space = match self.word_space(from) {
            Some((row, s)) if row == from.0 && s < from.1 => Some(s + 1),
            _ => None,
        };
        let arrived = match self.arrived {
            Some((row, mut col)) if row == from.0 && col <= from.1 => {
                while col > 0 && self.typed_live(row, col - 1, at) {
                    col -= 1;
                }
                Some(col)
            }
            _ => None,
        };
        match after_space.max(arrived) {
            Some(start) => from.1 - start,
            None => 0,
        }
    }

    #[inline]
    fn typed_live(&self, row: u16, col: u16, at: Instant) -> bool {
        let cells = self.cells.as_slice();
        let mut i = 0;
        while i < cells.len() {
            let c = &cells[i];
            if c.row == row && c.col == col && c.typing && self.alive_at(c, at) {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Resident typed suffix immediately before `from`, for a content proof whose preceding Space was program text rather than a typed event.
    #[inline]
    pub(crate) fn typed_suffix_len(&self, from: (u16, u16), at: Instant) -> u16 {
        let mut col = from.1;
        while col > 0 && self.typed_live(from.0, col - 1, at) {
            col -= 1;
        }
        from.1 - col
    }

    #[inline]
    fn begin_relocate(&mut self, to: (u16, u16), w: u16, at: Instant, walk: Option<WalkPos>) {
        let pane_col0 = match self.pane {
            Some((c0, _)) => c0,
            None => 0,
        };
        let (row, landing) = to;
        let Some(glyph) = landing.checked_sub(1) else {
            return;
        };
        if w == 0 {
            return;
        }
        if glyph >= w && glyph - w >= pane_col0 {
            self.relay_word(row, glyph - w, glyph, at, walk);
            return;
        }
        self.cells
            .retain(|c| !(c.row == row && c.col < landing && c.born >= at && c.typing));
        self.relocate = Some(Relocate { row, w, at, walk });
    }

    #[inline]
    fn settle_relocate(&mut self, row: u16, from_col: u16, at: Instant, ctx: &Ctx<'_>) {
        let Some(rel) = self.relocate else {
            return;
        };
        if rel.row != row {
            return;
        }
        if at.saturating_duration_since(rel.at) > RELOCATE_PATIENCE_DUR {
            self.relocate = None;
            return;
        }
        let pane_col0 = match self.pane {
            Some((c0, _)) => c0,
            None => 0,
        };
        let Some(glyph) = from_col.checked_sub(1) else {
            return;
        };
        if glyph < rel.w || glyph - rel.w < pane_col0 {
            return;
        }
        let _ = ctx;
        self.relay_word(row, glyph - rel.w, from_col, at, rel.walk);
        self.relocate = None;
    }

    #[inline]
    fn relay_word(&mut self, row: u16, col0: u16, col1: u16, at: Instant, walk: Option<WalkPos>) {
        let mut first = true;
        let mut minted: Option<u32> = None;
        let mut cached_like: Option<(u32, (f32, f32, f32, f32))> = None;
        let mut col = col0;
        while col < col1 {
            let cur_col = col;
            col += 1;
            if self.owned(row, cur_col, at) {
                continue;
            }
            let own = match minted {
                Some(id) => self.cohort_index(id),
                None => None,
            };
            let idx = match (walk, own) {
                (_, Some(i)) => i,
                (Some(walk), None) if first => {
                    let i = self.mint_cohort(row, cur_col, col0, walk, at, false);
                    minted = Some(self.cohorts[i].id);
                    i
                }
                _ => self.join_cohort(row, cur_col, at, false),
            };
            first = false;
            let cohort = self.cohorts[idx];
            let (life_s, cov0, birth_disp, edge_cells) = match cached_like {
                Some((id, pricing)) if id == cohort.id => pricing,
                _ => {
                    let mut best: Option<&Cell> = None;
                    let cells = self.cells.as_slice();
                    let mut ci = 0;
                    while ci < cells.len() {
                        let c = &cells[ci];
                        if c.row == row
                            && c.cohort == cohort.id
                            && c.typing
                            && !matches!(best, Some(prev) if c.born < prev.born)
                        {
                            best = Some(c);
                        }
                        ci += 1;
                    }
                    let pricing = match best {
                        Some(c) => (c.life_s, c.cov0, c.birth_disp, c.edge_cells),
                        None => (SWOOSH_LIFE_S, BODY_COLD_SHARE, 0.0, HOT_EDGE_CELLS),
                    };
                    cached_like = Some((cohort.id, pricing));
                    pricing
                }
            };
            let cell = Cell {
                row,
                col: cur_col,
                cohort: cohort.id,
                t: cohort.t_at(cur_col),
                born: at,
                attack_at: at,
                life_s,
                cov0,
                typing: true,
                retract_at: None,
                retire_at: None,
                birth_disp,
                edge_cells,
                layer: Layer::Base,
                rearm: None,
                released_at: None,
            };
            self.place(idx, cell, at, row);
        }
    }

    #[inline]
    fn scrub(&mut self, row: u16, from: u16, to: u16, at: Instant, ctx: &Ctx<'_>) -> bool {
        #[inline]
        const fn owner(cells: &[Cell], row: u16, col: u16) -> Option<usize> {
            let mut i = cells.len();
            while i > 0 {
                i -= 1;
                let c = &cells[i];
                if c.row == row && c.col == col && !c.leaving() {
                    return Some(i);
                }
            }
            None
        }
        let (lo, hi) = (from.min(to), from.max(to));
        if lo == hi {
            return false;
        }
        let reduced = ctx.cfg.reduced_motion;
        let mut last_coh: Option<(u32, Instant)> = None;
        let mut col = lo;
        while col < hi {
            let Some(i) = owner(&self.cells, row, col) else {
                return false;
            };
            col += 1;
            let cell = self.cells[i];
            if self.env_of(ctx, &cell) <= 0.0 {
                return false;
            }
            let alive_at = match last_coh {
                Some((id, alive_at)) if id == cell.cohort => alive_at,
                _ => {
                    let Some(coh) = self.cohort_by_id(cell.cohort) else {
                        return false;
                    };
                    if coh.leaving_at(at) {
                        return false;
                    }
                    last_coh = Some((coh.id, coh.alive_at));
                    coh.alive_at
                }
            };
            let spent = at
                .saturating_duration_since(cell.born.max(alive_at))
                .as_secs_f32();
            let melt_opens = if reduced {
                (cell.life_s - REDUCED_MOTION_FADE_MS / 1000.0).max(0.0)
            } else {
                cell.life_s * (1.0 - EXPIRY_MELT_SHARE)
            };
            if spent > melt_opens {
                return false;
            }
        }
        let mut last_held: Option<u32> = None;
        let mut col = lo;
        while col < hi {
            let cur_col = col;
            col += 1;
            let Some(i) = owner(&self.cells, row, cur_col) else {
                continue;
            };
            let id = self.cells[i].cohort;
            if last_held == Some(id) {
                continue;
            }
            if let Some(coh) = self.cohort_by_id_mut(id) {
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
                last_held = Some(id);
            }
        }
        true
    }

    #[inline]
    fn abandon_row(&mut self, row: u16, at: Instant, toward: Option<u16>) {
        if matches!(self.seam, Some(s) if s.row == row) {
            self.seam = None;
        }
        let mut ci = 0;
        while ci < self.cohorts.len() {
            let coh = &mut self.cohorts[ci];
            ci += 1;
            if coh.row != row {
                continue;
            }
            coh.revoke_releases(at);
            if coh.abandoned {
                continue;
            }
            let Some(rewound) = std::time::Duration::try_from_secs_f32(coh.retract_start())
                .ok()
                .and_then(|d| at.checked_sub(d))
            else {
                continue;
            };
            if coh.retract_col.is_none() {
                coh.retract_col = toward;
            }
            coh.abandoned = true;
            coh.rejoinable = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
        }
    }

    #[inline]
    fn print_row(&mut self, row: u16, at: Instant) {
        let mut ci = 0;
        while ci < self.cohorts.len() {
            let coh = &mut self.cohorts[ci];
            if coh.row == row && coh.printed.is_none() {
                coh.printed = Some(at);
            }
            ci += 1;
        }
    }

    #[inline]
    const fn touch_hand(&mut self, row: u16, col: u16) {
        self.hand_floor = Some(match self.hand_floor {
            Some((r, c)) if r == row => (row, if c < col { c } else { col }),
            _ => (row, col),
        });
    }

    #[inline]
    const fn hand_floor_on(&self, row: u16) -> Option<u16> {
        match self.hand_floor {
            Some((r, c)) if r == row => Some(c),
            _ => None,
        }
    }

    /// The hand's floor ([`Ribbon::hand_floor`]) as `(row, col)`, or `None` when no row has seen the hand yet.
    #[inline]
    pub(super) const fn hand_floor_cell(&self) -> Option<(u16, u16)> {
        self.hand_floor
    }

    #[inline]
    fn lay_cell(&mut self, row: u16, col: u16, at: Instant, ctx: &Ctx<'_>, typing: bool) {
        // The hand's own ink is the floor ([`Ribbon::hand_floor`]).
        if typing {
            self.touch_hand(row, col);
        }
        let life_s = self.cell_life(at, ctx);
        let birth_disp = Self::birth_price(ctx);
        let cov0 = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        let reviving = matches!(self.revive, Some(rv) if rv.covers(row, col));
        let gate = match self.fresh_line {
            Some(gate) if !reviving && gate.takes(row, at) => Some(gate),
            _ => None,
        };
        let idx = self.join_cohort(row, col, at, gate.is_some());
        if let Some(gate) = gate {
            let joined = self.cohorts[idx];
            let mut re_laid = false;
            if joined.born < gate.since {
                let mut ci = 0;
                while ci < self.cells.len() {
                    let c = &self.cells[ci];
                    if c.row == row && c.col == col && c.cohort == joined.id {
                        re_laid = true;
                        break;
                    }
                    ci += 1;
                }
            }
            if !re_laid {
                self.fresh_line = None;
            }
        }
        let idx = if typing {
            self.fold_wakes_into_band(idx, row, col, at)
        } else {
            idx
        };
        let cohort = self.cohorts[idx];
        let inherited = typing && self.attach == Some((row, col));
        const EDGE_ATTACK_INHERITED_DUR: std::time::Duration = std::time::Duration::from_nanos(
            ((EDGE_IN_S + ATTACK_WIPE_S) as f64 * 1_000_000_000.0).round() as u64,
        );
        let attack_at = if inherited {
            at.checked_sub(EDGE_ATTACK_INHERITED_DUR).unwrap_or(at)
        } else {
            at
        };
        let cell = Cell {
            row,
            col,
            cohort: cohort.id,
            t: cohort.t_at(col),
            born: at,
            attack_at,
            life_s,
            cov0,
            typing,
            retract_at: None,
            retire_at: None,
            birth_disp,
            edge_cells: edge_cells(ctx.surge, ctx.flow.heat),
            layer: Layer::Base,
            rearm: None,
            released_at: None,
        };
        self.place(idx, cell, at, ctx.caret.0);
        self.seat_fold_successor(idx, col, ctx);
    }

    #[inline]
    fn seat_fold_successor(&mut self, idx: usize, col: u16, ctx: &Ctx<'_>) {
        let cols = ctx.geom.cols.min(u16::MAX as usize) as u16;
        let (pane_col0, pane_col1) = match self.pane {
            Some((c0, w)) => (c0, c0.saturating_add(w.max(1))),
            None => (0, cols),
        };
        if col.saturating_add(1) != pane_col1 {
            return;
        }
        let a = self.cohorts[idx];
        let Some(below) = a.row.checked_add(1) else {
            return;
        };
        let stale = a.walk_at(col);
        let mut bi = 0;
        while bi < self.cohorts.len() {
            let b = self.cohorts[bi];
            if b.id == a.id
                || b.row != below
                || b.anchor_col < pane_col0
                || b.anchor_col >= pane_col1
                || b.origin_walk() != stale
            {
                bi += 1;
                continue;
            }
            let seat = a.walk_at(pane_col1.saturating_add(b.anchor_col - pane_col0));
            let coh = &mut self.cohorts[bi];
            coh.t0 = seat.t;
            coh.d0 = seat.d;
            let origin = *coh;
            let walk_zero = origin.walk_zero();
            let mut ci = 0;
            while ci < self.cells.len() {
                let c = &mut self.cells[ci];
                if c.cohort == origin.id {
                    c.t = walk_zero + walk_t(origin.walk_d(c.col));
                }
                ci += 1;
            }
            bi += 1;
        }
    }

    #[inline]
    fn place(&mut self, idx: usize, mut cell: Cell, at: Instant, hand_row: u16) {
        let (row, col, typing) = (cell.row, cell.col, cell.typing);
        let (col0, col1) = (self.cohorts[idx].col0, self.cohorts[idx].col1);
        if col >= col0 && col < col1 {
            let mut slot = None;
            let mut i = 0;
            while i < self.cells.len() {
                let c = &self.cells[i];
                if c.row == row && c.col == col && c.cohort == cell.cohort {
                    slot = Some(i);
                    break;
                }
                i += 1;
            }
            if let Some(slot) = slot {
                let old = self.cells.remove(slot);
                self.custody_replaced(idx, old.released_at, 1);
                let alive = self.alive_at(&old, at);
                if alive && old.typing == typing {
                    cell.attack_at = old.attack_at.min(cell.attack_at);
                    cell.cov0 = cell.cov0.max(old.cov0);
                    cell.life_s = cell.life_s.max(old.life_s);
                }
            }
        }
        self.cells.push(cell);
        let coh = &mut self.cohorts[idx];
        coh.col0 = coh.col0.min(col);
        coh.col1 = coh.col1.max(col.saturating_add(1));
        if typing {
            coh.wake = false;
            if coh.flow.take().is_some() {
                coh.retract_col = None;
                coh.flow_drift = 0.0;
            }
            let open = self.phrase_for_lay();
            self.cohorts[idx].phrase = open;
            self.hold(open, hand_row, Some(idx), at);
        } else {
            coh.alive_at = coh.alive_at.max(at);
            coh.phase = Phase::Laying;
            coh.retract_col = None;
        }
        self.prune_release_parts();
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn wake(
        &mut self,
        row: u16,
        from: u16,
        to: u16,
        at: Instant,
        ctx: &Ctx<'_>,
        life_s: f32,
        takeover: bool,
    ) -> Option<u32> {
        let cols = ctx.geom.cols.min(u16::MAX as usize) as u16;
        if cols == 0 || from == to || to >= cols {
            return None;
        }
        let leftward = to < from;
        let far = from.min(cols - 1);
        let span = far.abs_diff(to);
        let flight_s = if takeover {
            flight_ms(f32::from(span))
        } else {
            FLIGHT_MIN_MS
        } / 1000.0;
        let n = f32::from(span).max(1.0);
        let birth_disp = clamp01(ctx.birth_disp);
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        let (anchor_col, walk) = self.wake_origin(row, to, at);
        let minted = if takeover {
            Some(self.own_wake_cohort(row, to, from, anchor_col, walk, at))
        } else {
            None
        };
        let mut reach = 0;
        if span >= WAKE_MAX_CELLS {
            let mut k = span;
            loop {
                let col = if leftward { to + k } else { to - k };
                if self.corridor_keeps_glass_light(ctx, row, col, at) {
                    reach = k;
                    break;
                }
                if k == WAKE_MAX_CELLS {
                    break;
                }
                k -= 1;
            }
        }

        let mut k = span;
        loop {
            let col = if leftward { to + k } else { to - k };
            let j = span - k;
            let born_new = std::time::Duration::try_from_secs_f32(flight_s * f32::from(j) / n)
                .ok()
                .and_then(|d| at.checked_add(d))
                .unwrap_or(at);
            let (over, base_cov, base_leaving) = self.corridor_column(ctx, row, col, at);
            let over_live = if let Some(i) = over {
                let c = self.cells[i];
                !c.leaving() && self.env_of(ctx, &c) > 0.0
            } else {
                false
            };
            let base_live = base_cov > 0.0 && !base_leaving;
            if takeover || !(base_live || over_live) {
                let mut skip_cell = false;
                let (born, attack_at, cov0, rearm, adopted) = match over {
                    Some(i) => {
                        let old = self.cells[i];
                        let level = clamp01(self.env_of(ctx, &old));
                        self.cells.remove(i);
                        (
                            old.born,
                            old.attack_at,
                            old.cov0,
                            Some((at, level)),
                            Some(old.cohort),
                        )
                    }
                    None => {
                        if k >= WAKE_MAX_CELLS && k > reach && !Self::reads_as_glass_light(base_cov)
                        {
                            skip_cell = true;
                            (born_new, born_new, cov0_new, None, None)
                        } else if base_cov > 0.0 {
                            let cov0 = cov0_new.max(base_cov);
                            (
                                born_new,
                                born_new,
                                cov0,
                                Some((born_new, clamp01(base_cov / cov0))),
                                None,
                            )
                        } else {
                            (born_new, born_new, cov0_new, None, None)
                        }
                    }
                };
                if !skip_cell {
                    let idx = match minted {
                        Some(idx) => idx,
                        None => self.join_wake_cohort(row, col, anchor_col, walk, at),
                    };
                    let cohort = self.cohorts[idx];
                    if let Some(old) = adopted {
                        self.copy_hold(old, cohort.id, row);
                    }
                    let cell = Cell {
                        row,
                        col,
                        cohort: cohort.id,
                        t: cohort.t_at(col),
                        born,
                        attack_at,
                        life_s,
                        cov0,
                        typing: false,
                        retract_at: None,
                        retire_at: None,
                        birth_disp,
                        edge_cells: HOT_EDGE_CELLS,
                        layer: Layer::Over,
                        rearm,
                        released_at: None,
                    };
                    self.place(idx, cell, at, row);
                }
            }
            if k == 0 {
                break;
            }
            k -= 1;
        }
        match minted {
            Some(idx) => Some(self.cohorts[idx].id),
            None => None,
        }
    }

    #[inline]
    fn corridor_column(
        &self,
        ctx: &Ctx<'_>,
        row: u16,
        col: u16,
        at: Instant,
    ) -> (Option<usize>, f32, bool) {
        let mut over = None;
        let mut base = None;
        let mut i = self.cells.len();
        while i > 0 {
            i -= 1;
            let c = &self.cells[i];
            if c.row != row || c.col != col {
                continue;
            }
            match c.layer {
                Layer::Over if over.is_none() => {
                    over = Some(i);
                    if base.is_some() {
                        break;
                    }
                }
                Layer::Base if base.is_none() => {
                    base = Some(c);
                    if over.is_some() {
                        break;
                    }
                }
                _ => {}
            }
        }
        let (base_cov, base_leaving) = match base {
            Some(c) => {
                let env = if c.leaving() {
                    0.0
                } else {
                    self.env_of(ctx, c)
                };
                let leaving = match self.cohort_by_id(c.cohort) {
                    Some(k) => k.leaving_at(at),
                    None => true,
                };
                (c.cov0 * env, leaving)
            }
            None => (0.0, false),
        };
        (over, base_cov, base_leaving)
    }

    #[inline]
    fn corridor_keeps_glass_light(&self, ctx: &Ctx<'_>, row: u16, col: u16, at: Instant) -> bool {
        let (over, base_cov, _) = self.corridor_column(ctx, row, col, at);
        over.is_some() || Self::reads_as_glass_light(base_cov)
    }

    #[inline]
    const fn reads_as_glass_light(share: f32) -> bool {
        UNDER_COV_CAP * share >= STATUS_LIT_COV as f32
    }

    #[inline]
    fn own_wake_cohort(
        &mut self,
        row: u16,
        to: u16,
        from: u16,
        anchor_col: u16,
        walk: WalkPos,
        at: Instant,
    ) -> usize {
        let (lo, hi) = (to.min(from), to.max(from));
        let mut i = 0;
        while i < self.cohorts.len() {
            let c = &self.cohorts[i];
            if c.row == row
                && c.wake
                && !c.abandoned
                && c.col0 <= hi
                && lo < c.col1
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= WAKE_LIFE_S
            {
                let coh = &mut self.cohorts[i];
                coh.alive_at = at;
                coh.phase = Phase::Laying;
                coh.retract_col = None;
                return i;
            }
            i += 1;
        }
        self.mint_cohort(row, to, anchor_col, walk, at, true)
    }

    #[inline]
    fn wake_origin(&self, row: u16, landing: u16, at: Instant) -> (u16, WalkPos) {
        let mut best_on_row: Option<&Cohort> = None;
        let mut best_any: Option<&Cohort> = None;
        let mut i = 0;
        while i < self.cohorts.len() {
            let c = &self.cohorts[i];
            i += 1;
            if at.saturating_duration_since(c.alive_at) > CHAIN_GAP_MAX_DUR {
                continue;
            }
            if c.row == row && !matches!(best_on_row, Some(prev) if !is_fresher_or_equal(c, prev)) {
                best_on_row = Some(c);
            }
            if !matches!(best_any, Some(prev) if !is_fresher_or_equal(c, prev)) {
                best_any = Some(c);
            }
        }
        if let Some(c) = best_on_row {
            return (c.anchor_col, c.origin_walk());
        }
        let walk = match best_any {
            Some(c) => c.walk_at(c.col1),
            None => WalkPos::COLD,
        };
        (landing, walk.restarted())
    }

    #[inline]
    fn join_wake_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        walk: WalkPos,
        at: Instant,
    ) -> usize {
        let mut i = 0;
        while i < self.cohorts.len() {
            let c = &self.cohorts[i];
            if c.row == row
                && c.wake
                && !c.abandoned
                && col + 1 >= c.col0
                && col <= c.col1
                && at.saturating_duration_since(c.alive_at) <= CHAIN_GAP_MAX_DUR
            {
                return i;
            }
            i += 1;
        }
        self.mint_cohort(row, col, anchor_col, walk, at, true)
    }

    #[inline]
    fn mint_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        walk: WalkPos,
        at: Instant,
        wake: bool,
    ) -> usize {
        let id = self.next_cohort;
        self.next_cohort = self.next_cohort.wrapping_add(1);
        let grace_s = if wake {
            PHRASE_REST_MIN_S
        } else {
            self.rest_s()
        };
        self.cohorts.push(Cohort {
            id,
            row,
            col0: col,
            col1: col + 1,
            anchor_col,
            t0: walk.t,
            d0: walk.d,
            born: at,
            alive_at: at,
            abandoned: false,
            wake,
            retract_col: None,
            drain_right_first: false,
            phase: Phase::Laying,
            rejoinable: false,
            phrase: 0,
            grace_s,
            flow: None,
            flow_drift: 0.0,
            release_clock: None,
            parts_revoked_at: None,
            line_killed: false,
            printed: None,
        });
        // A fresh identity cannot borrow the row's previous occupant, even
        // when it is minted before the first content move in this batch.
        self.stage_hold(id, None);
        self.cohorts.len() - 1
    }

    #[inline]
    fn join_cohort(&mut self, row: u16, col: u16, at: Instant, new_line: bool) -> usize {
        let mut rejoin = None;
        let mut i = 0;
        while i < self.cohorts.len() {
            let c = &self.cohorts[i];
            if c.row == row
                && col + 1 >= c.col0
                && col <= c.col1
                && (!c.abandoned || (rejoin.is_none() && c.rejoinable))
                && at.saturating_duration_since(c.alive_at) <= CHAIN_GAP_MAX_DUR
                && self.stands(c)
            {
                if !c.abandoned {
                    return i;
                }
                rejoin = Some(i);
            }
            i += 1;
        }
        if let Some(i) = rejoin {
            let coh = &mut self.cohorts[i];
            coh.abandoned = false;
            coh.retract_col = None;
            coh.printed = None;
            return i;
        }
        if let Some(rv) = self.revive
            && rv.covers(row, col)
        {
            return self.mint_cohort(row, col, rv.origin.anchor_col, rv.origin.walk(), at, false);
        }
        let mut continued: Option<&Cohort> = None;
        let mut ci = 0;
        while ci < self.cohorts.len() {
            let c = &self.cohorts[ci];
            if Self::chain_fresh(at, c.alive_at)
                && !matches!(continued, Some(prev) if !is_fresher_or_equal(c, prev))
            {
                continued = Some(c);
            }
            ci += 1;
        }
        let walk = if let Some(c) = continued {
            if c.row == row && !new_line && self.stands(c) {
                c.walk_at(col)
            } else {
                c.walk_at(c.col1)
            }
        } else if let Some(w) = self.last_walk
            && Self::chain_fresh(at, w.alive_at)
        {
            // A finished swoosh's walk continues too, inside the chain window (`retire` drops the cohort with its cells).
            w.end
        } else {
            WalkPos::COLD
        };
        let walk = if new_line { walk.restarted() } else { walk };
        self.mint_cohort(row, col, col, walk, at, false)
    }

    #[inline]
    fn chain_fresh(at: Instant, t: Instant) -> bool {
        at.saturating_duration_since(t) <= CHAIN_GAP_MAX_DUR
    }

    #[inline]
    const fn stands(&self, coh: &Cohort) -> bool {
        let cells = self.cells.as_slice();
        let mut i = 0;
        while i < cells.len() {
            let c = &cells[i];
            if c.cohort == coh.id
                && c.row == coh.row
                && c.retire_at.is_none()
                && (c.retract_at.is_none() || !coh.line_killed)
            {
                return true;
            }
            i += 1;
        }
        false
    }

    #[inline]
    const fn mark_line_killed(&mut self, row: u16) {
        let cohorts = self.cohorts.as_mut_slice();
        let cells = self.cells.as_slice();
        let mut k = 0;
        while k < cohorts.len() {
            let coh = cohorts[k];
            if coh.row == row {
                let mut has_live = false;
                let mut i = 0;
                while i < cells.len() {
                    let c = &cells[i];
                    if c.cohort == coh.id && c.row == row && !c.leaving() {
                        has_live = true;
                        break;
                    }
                    i += 1;
                }
                if !has_live {
                    cohorts[k].line_killed = true;
                }
            }
            k += 1;
        }
    }

    #[inline]
    fn fold_wakes_into_band(&mut self, idx: usize, row: u16, col: u16, at: Instant) -> usize {
        let joined = self.cohorts[idx];
        let band_id = if joined.wake {
            let mut found = None;
            let mut j = 0;
            while j < self.cohorts.len() {
                let c = &self.cohorts[j];
                if c.id != joined.id
                    && c.row == row
                    && !c.wake
                    && !c.abandoned
                    && c.retract_col.is_none()
                    && !matches!(c.phase, Phase::Reaching | Phase::Retracting | Phase::Fading)
                    && Self::chain_fresh(at, c.alive_at)
                    && c.col1 >= joined.col0
                    && joined.col1 >= c.col0
                {
                    found = Some(j);
                    break;
                }
                j += 1;
            }
            let Some(j) = found else {
                return idx;
            };
            self.cohorts[j].id
        } else {
            joined.id
        };
        loop {
            let mut wake_idx = None;
            let mut w = 0;
            while w < self.cohorts.len() {
                let c = &self.cohorts[w];
                if c.wake
                    && c.id != band_id
                    && c.row == row
                    && !c.abandoned
                    && Self::chain_fresh(at, c.alive_at)
                    && col.saturating_add(1) >= c.col0
                    && col <= c.col1
                {
                    wake_idx = Some(w);
                    break;
                }
                w += 1;
            }
            let Some(w) = wake_idx else {
                break;
            };
            let wake = self.cohorts[w];
            let Some(b) = self.cohort_index(band_id) else {
                break;
            };
            let band = self.cohorts[b];
            let mut like: Option<Cell> = None;
            let mut li = 0;
            while li < self.cells.len() {
                let c = &self.cells[li];
                if c.row == band.row
                    && c.cohort == band.id
                    && c.typing
                    && !matches!(like, Some(prev) if c.born < prev.born)
                {
                    like = Some(*c);
                }
                li += 1;
            }
            let mut ci = 0usize;
            while ci < self.cells.len() {
                let c = self.cells[ci];
                let mut wake_has_col = false;
                if c.cohort == band.id && c.row == row && c.col >= wake.col0 && c.col < wake.col1 {
                    let mut wi = 0;
                    while wi < self.cells.len() {
                        let wc = &self.cells[wi];
                        if wc.cohort == wake.id && wc.col == c.col {
                            wake_has_col = true;
                            break;
                        }
                        wi += 1;
                    }
                }
                if wake_has_col {
                    let gone = self.cells.remove(ci);
                    self.custody_replaced(b, gone.released_at, 1);
                } else {
                    ci += 1;
                }
            }
            self.carry_release(wake.id, band.id);
            self.merge_hold(wake.id, band.id, row);
            let band_walk_zero = band.walk_zero();
            let mut mi = 0;
            while mi < self.cells.len() {
                let cell = &mut self.cells[mi];
                if cell.cohort == wake.id {
                    cell.cohort = band.id;
                    cell.t = band_walk_zero + walk_t(band.walk_d(cell.col));
                    if let Some(like) = like {
                        cell.life_s = cell.life_s.max(like.life_s);
                        cell.cov0 = cell.cov0.max(like.cov0);
                        cell.birth_disp = cell.birth_disp.max(like.birth_disp);
                        cell.edge_cells = like.edge_cells;
                    }
                }
                mi += 1;
            }
            self.cohorts[b].col0 = band.col0.min(wake.col0);
            self.cohorts[b].col1 = band.col1.max(wake.col1);
            self.cohorts.remove(w);
        }
        self.prune_release_parts();
        self.cohort_index(band_id)
            .unwrap_or(idx.min(self.cohorts.len().saturating_sub(1)))
    }

    #[inline]
    fn retract_span(&mut self, row: u16, col: u16, min_cells: u16, at: Instant, span_s: f32) {
        let last = col.saturating_add(min_cells.max(1) - 1);
        let n = f32::from(last - col) + 1.0;
        let step = span_s / n;
        let mut i = 0;
        while i < self.cells.len() {
            let cell = &mut self.cells[i];
            i += 1;
            if cell.row != row || cell.col < col || cell.col > last || cell.leaving() {
                continue;
            }
            let rank = f32::from(last - cell.col);
            cell.retract_at = std::time::Duration::try_from_secs_f32(rank * step)
                .ok()
                .and_then(|d| at.checked_add(d));
        }
    }

    #[inline]
    fn retract_suffix(&mut self, row: u16, col: u16, min_cells: u16, at: Instant, span_s: f32) {
        let followed = &self.followed_now;
        let mut last = col.saturating_add(min_cells.saturating_sub(1));
        let mut i = 0;
        while i < self.cells.len() {
            let c = &self.cells[i];
            if c.row == row
                && c.col > last
                && !c.leaving()
                && !followed.contains(&(c.row, c.col, c.born))
            {
                last = c.col;
            }
            i += 1;
        }
        let mut right_latest: Option<Instant> = None;
        let mut j = 0;
        while j < self.cells.len() {
            let cell = &mut self.cells[j];
            j += 1;
            if cell.row != row
                || cell.col <= last
                || followed.contains(&(cell.row, cell.col, cell.born))
            {
                continue;
            }
            if let Some(prev) = cell.retract_at {
                let capped = if prev > at {
                    cell.retract_at = Some(at);
                    at
                } else {
                    prev
                };
                if at.saturating_duration_since(capped).as_secs_f32() < RETRACT_FADE_S {
                    right_latest = Some(match right_latest {
                        Some(m) => m.max(capped),
                        None => capped,
                    });
                }
            }
        }
        let base_at = if let Some(t) = right_latest
            && let Some(t) = t.checked_add(std::time::Duration::from_millis(16))
        {
            at.max(t)
        } else {
            at
        };
        let n = f32::from(last - col) + 1.0;
        let step = span_s / n;
        let mut k = 0;
        while k < self.cells.len() {
            let cell = &mut self.cells[k];
            k += 1;
            if cell.row != row
                || cell.col < col
                || cell.col > last
                || cell.leaving()
                || followed.contains(&(cell.row, cell.col, cell.born))
            {
                continue;
            }
            let rank = f32::from(last - cell.col);
            let mut retract_at = std::time::Duration::try_from_secs_f32(rank * step)
                .ok()
                .and_then(|d| at.checked_add(d));
            if let Some(t) = &mut retract_at
                && *t < base_at
            {
                *t = base_at;
            }
            cell.retract_at = retract_at;
        }
    }

    #[inline]
    fn abandon(
        &mut self,
        at: Instant,
        keep: Option<u32>,
        toward: Option<(Option<u16>, u16, bool)>,
        rejoinable: bool,
    ) {
        let mut i = 0;
        while i < self.cohorts.len() {
            let coh = &mut self.cohorts[i];
            i += 1;
            if keep == Some(coh.id) {
                continue;
            }
            coh.revoke_releases(at);
            if !coh.abandoned {
                coh.rejoinable = rejoinable;
            }
            let Some(rewound) = std::time::Duration::try_from_secs_f32(coh.retract_start())
                .ok()
                .and_then(|d| at.checked_sub(d))
            else {
                continue;
            };
            coh.abandoned = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
            if let Some((row, col, leftward)) = toward
                && !matches!(row, Some(row) if coh.row != row)
                && coh.retract_col.is_none()
            {
                coh.retract_col = Some(col);
                coh.drain_right_first = leftward;
            }
        }
    }

    /// Retire specific live cells on the fast melt.
    #[inline]
    pub fn retire_cells(&mut self, cells: &[(u16, u16, Instant)], at: Instant) -> usize {
        let mut n = 0;
        let mut ci = 0;
        let mut last_revoked: Option<u32> = None;
        while ci < self.cells.len() {
            let cell = &mut self.cells[ci];
            ci += 1;
            if !cells.contains(&(cell.row, cell.col, cell.born)) {
                continue;
            }
            if cell.leaving() {
                // Replaced content cannot restore a prior partial release, even when its visual retract has already finished.
                if cell.released_at.take().is_some() && last_revoked != Some(cell.cohort) {
                    let mut ki = 0;
                    while ki < self.cohorts.len() {
                        let coh = &mut self.cohorts[ki];
                        if coh.id == cell.cohort {
                            coh.revoke_releases(at);
                            last_revoked = Some(cell.cohort);
                            break;
                        }
                        ki += 1;
                    }
                }
                continue;
            }
            cell.retire_at = Some(at);
            if last_revoked != Some(cell.cohort) {
                let mut ki = 0;
                while ki < self.cohorts.len() {
                    let coh = &mut self.cohorts[ki];
                    if coh.id == cell.cohort {
                        coh.revoke_releases(at);
                        last_revoked = Some(cell.cohort);
                        break;
                    }
                    ki += 1;
                }
            }
            n += 1;
        }
        self.prune_release_parts();
        n
    }

    /// Release specific live cells to the swoosh.
    #[inline]
    pub fn release_cells(&mut self, cells: &[(u16, u16, Instant)], at: Instant) -> usize {
        const RETRACT_START_DUR: std::time::Duration = std::time::Duration::from_nanos(
            (RETRACT_START_S as f64 * 1_000_000_000.0).round() as u64,
        );
        let Some(rewound) = at.checked_sub(RETRACT_START_DUR) else {
            return 0;
        };
        let mut n = 0;
        let mut touched = std::mem::take(&mut self.released_cohorts);
        touched.clear();
        let mut ci = 0;
        while ci < self.cells.len() {
            let cell = &self.cells[ci];
            if !cell.leaving()
                && cells.contains(&(cell.row, cell.col, cell.born))
                && !touched.contains(&cell.cohort)
            {
                touched.push(cell.cohort);
            }
            ci += 1;
        }
        let mut ti = 0;
        while ti < touched.len() {
            let id = touched[ti];
            ti += 1;
            let mut whole = true;
            let mut count = 0usize;
            let mut live_count = 0usize;
            let mut j = 0;
            while j < self.cells.len() {
                let c = &self.cells[j];
                j += 1;
                if c.cohort != id {
                    continue;
                }
                count += 1;
                if !c.leaving() {
                    live_count += 1;
                    if whole && !cells.contains(&(c.row, c.col, c.born)) {
                        whole = false;
                    }
                }
            }
            if !whole {
                let mut stamped = 0usize;
                let mut k = 0;
                while k < self.cells.len() {
                    let cell = &mut self.cells[k];
                    if cell.cohort == id
                        && !cell.leaving()
                        && cells.contains(&(cell.row, cell.col, cell.born))
                    {
                        cell.retract_at = Some(at);
                        cell.released_at = Some(at);
                        n += 1;
                        stamped += 1;
                    }
                    k += 1;
                }
                if stamped > 0 && self.cohort_by_id(id).is_some() {
                    self.hold_part(ReleasePart {
                        cohort: id,
                        stamp: at,
                        cells: stamped,
                    });
                }
                continue;
            }
            n += live_count;
            let Some(coh) = self.cohort_by_id_mut(id) else {
                continue;
            };
            if !coh.abandoned {
                coh.release_clock = Some(ReleaseClock {
                    alive_at: coh.alive_at,
                    retract_col: coh.retract_col,
                    phase: coh.phase,
                    rejoinable: coh.rejoinable,
                    cells: count,
                });
            }
            coh.rejoinable = false;
            if coh.abandoned {
                continue;
            }
            coh.abandoned = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
        }
        self.released_cohorts = touched;
        self.prune_release_parts();
        n
    }

    /// A resident partial release can return without being laid again.
    #[inline]
    pub(super) fn partial_custody_valid(&self, cell: &Cell, now: Instant) -> bool {
        let Some(stamp) = cell.released_at else {
            return false;
        };
        if cell.retire_at.is_some() {
            return false;
        }
        let Some(cohort) = self.cohort_by_id(cell.cohort) else {
            return false;
        };
        let alive = now
            .saturating_duration_since(cell.born.max(cohort.alive_at))
            .as_secs_f32()
            < cell.life_s;
        if cohort.leaving_at(now)
            || matches!(cohort.parts_revoked_at, Some(cutoff) if stamp < cutoff)
            || !alive
        {
            return false;
        }
        if let Some(index) = self.matching_partial_release(cell.cohort, stamp) {
            self.release_parts[index].cells > 0
        } else {
            false
        }
    }

    #[inline]
    fn matching_partial_release(&self, id: u32, stamp: Instant) -> Option<usize> {
        let index = ReleasePart::find(&self.release_parts, id, stamp).ok()?;
        let mut resident = 0usize;
        let mut i = 0;
        while i < self.cells.len() {
            let cell = &self.cells[i];
            if cell.cohort == id && cell.released_at == Some(stamp) {
                resident += 1;
            }
            i += 1;
        }
        if resident == self.release_parts[index].cells {
            Some(index)
        } else {
            None
        }
    }

    /// Lift the releases whose text the witness found back.
    #[inline]
    pub(super) fn restore_releases(
        &mut self,
        candidates: &[(u32, Option<Instant>)],
        accepted: &mut Vec<u32>,
    ) {
        accepted.clear();
        let mut ci = 0;
        while ci < candidates.len() {
            let (id, stamp) = candidates[ci];
            ci += 1;
            let Some(k) = self.cohort_index(id) else {
                continue;
            };
            let Some(stamp) = stamp else {
                let Some(saved) = self.cohorts[k].release_clock else {
                    continue;
                };
                let mut count = 0usize;
                let mut j = 0;
                while j < self.cells.len() {
                    if self.cells[j].cohort == id {
                        count += 1;
                    }
                    j += 1;
                }
                if count != saved.cells {
                    continue;
                }
                let coh = &mut self.cohorts[k];
                coh.release_clock = None;
                coh.alive_at = saved.alive_at;
                coh.retract_col = saved.retract_col;
                coh.phase = saved.phase;
                coh.rejoinable = saved.rejoinable;
                coh.abandoned = false;
                accepted.push(id);
                continue;
            };
            let Some(part_at) = self.matching_partial_release(id, stamp) else {
                continue;
            };
            let mut j = 0;
            while j < self.cells.len() {
                let cell = &mut self.cells[j];
                if cell.cohort == id && cell.released_at == Some(stamp) {
                    cell.retract_at = None;
                    cell.released_at = None;
                }
                j += 1;
            }
            self.release_parts.remove(part_at);
            accepted.push(id);
        }
        accepted.sort_unstable();
        accepted.dedup();
    }

    #[inline]
    fn custody_replaced(&mut self, idx: usize, stamp: Option<Instant>, n: usize) {
        let Some(stamp) = stamp else {
            return;
        };
        if n == 0 {
            return;
        }
        if let Some(cohort) = self.cohorts.get(idx)
            && let Ok(part_at) = ReleasePart::find(&self.release_parts, cohort.id, stamp)
        {
            self.release_parts[part_at].cells = self.release_parts[part_at].cells.saturating_sub(n);
        }
    }

    /// Keep the first saved count for a key, as the cohort's original
    /// first-match lookup did when a merge brought the same stamp back.
    #[inline]
    fn hold_part(&mut self, part: ReleasePart) {
        if let Err(at) = ReleasePart::find(&self.release_parts, part.cohort, part.stamp) {
            self.release_parts.insert(at, part);
        }
    }

    /// Drop only orphaned keys. A surviving fragment keeps its saved count,
    /// including zero, so a split or a missing identity cannot weaken the guard.
    #[inline]
    fn prune_release_parts(&mut self) {
        if self.release_parts.is_empty() {
            return;
        }
        let (cells, cohorts) = (&self.cells, &self.cohorts);
        let mut cached_cohort: Option<(u32, bool)> = None;
        self.release_parts.retain(|part| {
            let cohort_live = match cached_cohort {
                Some((id, live)) if id == part.cohort => live,
                _ => {
                    let live = Cohort::find(cohorts, part.cohort).is_some();
                    cached_cohort = Some((part.cohort, live));
                    live
                }
            };
            if !cohort_live {
                return false;
            }
            let mut i = 0;
            while i < cells.len() {
                let cell = &cells[i];
                if cell.cohort == part.cohort && cell.released_at == Some(part.stamp) {
                    return true;
                }
                i += 1;
            }
            false
        });
    }

    #[inline]
    fn carry_release(&mut self, from: u32, into: u32) {
        if from == into {
            return;
        }
        let Some(into_idx) = self.cohort_index(into) else {
            return;
        };
        let Some(from_coh) = self.cohort_by_id(from) else {
            return;
        };
        let revoked = from_coh.parts_revoked_at;
        loop {
            let mut found = None;
            let mut at = 0;
            while at < self.release_parts.len() {
                if self.release_parts[at].cohort == from {
                    found = Some(at);
                    break;
                }
                at += 1;
            }
            let Some(at) = found else {
                break;
            };
            let part = self.release_parts.remove(at);
            self.hold_part(ReleasePart {
                cohort: into,
                ..part
            });
        }
        let target = &mut self.cohorts[into_idx];
        if let Some(t) = revoked {
            target.parts_revoked_at = Some(match target.parts_revoked_at {
                Some(u) => u.max(t),
                None => t,
            });
        }
    }

    /// A split carries only the keys whose cells reached its new identity.
    /// Each fragment keeps the original expected count until it reunites.
    #[inline]
    fn split_release(&mut self, from: u32, into: u32) {
        let mut after = None;
        loop {
            let at = ReleasePart::find_next(&self.release_parts, from, after);
            let Some(&part) = self.release_parts.get(at) else {
                break;
            };
            if part.cohort != from {
                break;
            }
            after = Some(part.stamp);
            let mut has_cell = false;
            let mut i = 0;
            while i < self.cells.len() {
                let cell = &self.cells[i];
                if cell.cohort == into && cell.released_at == Some(part.stamp) {
                    has_cell = true;
                    break;
                }
                i += 1;
            }
            if has_cell {
                self.hold_part(ReleasePart {
                    cohort: into,
                    ..part
                });
            }
        }
        self.prune_release_parts();
    }

    /// Let go of the partial releases revoked since the last tick.
    #[inline]
    pub(super) fn settle_revoked_parts(&mut self) {
        let mut i = 0;
        while i < self.cohorts.len() {
            let coh = &mut self.cohorts[i];
            i += 1;
            let Some(t) = coh.parts_revoked_at.take() else {
                continue;
            };
            let id = coh.id;
            self.release_parts
                .retain(|part| part.cohort != id || part.stamp >= t);
        }
        self.prune_release_parts();
    }

    #[inline]
    fn heal_seams(&mut self) {
        #[inline]
        const fn hot(c: &Cohort) -> bool {
            !c.wake && !c.abandoned && c.retract_col.is_none() && c.flow.is_none()
        }
        if self.cohorts.len() < 2 {
            return;
        }
        let mut i = 0;
        while i < self.cohorts.len() {
            let a = self.cohorts[i];
            if !hot(&a) || !self.stands(&a) {
                i += 1;
                continue;
            }
            let mut found_j = None;
            let mut idx_b = 0;
            while idx_b < self.cohorts.len() {
                let b = &self.cohorts[idx_b];
                if b.id != a.id && b.row == a.row && b.col0 == a.col1 && hot(b) && self.stands(b) {
                    found_j = Some(idx_b);
                    break;
                }
                idx_b += 1;
            }
            let Some(j) = found_j else {
                i += 1;
                continue;
            };
            let b = self.cohorts[j];
            let (into, from) = if a.born <= b.born { (i, j) } else { (j, i) };
            let (into_id, from_id) = (self.cohorts[into].id, self.cohorts[from].id);
            let mut idx = 0;
            while idx < self.cells.len() {
                let c = self.cells[idx];
                let shadowed = if c.cohort == into_id {
                    let mut sh = false;
                    let mut di = 0;
                    while di < self.cells.len() {
                        let d = &self.cells[di];
                        if d.cohort == from_id
                            && d.row == c.row
                            && d.col == c.col
                            && d.born >= c.born
                        {
                            sh = true;
                            break;
                        }
                        di += 1;
                    }
                    sh
                } else {
                    false
                };
                if shadowed {
                    let gone = self.cells.remove(idx);
                    self.custody_replaced(into, gone.released_at, 1);
                } else {
                    idx += 1;
                }
            }
            self.carry_release(from_id, into_id);
            self.merge_hold(from_id, into_id, a.row);
            let mut ci = 0;
            while ci < self.cells.len() {
                if self.cells[ci].cohort == from_id {
                    self.cells[ci].cohort = into_id;
                }
                ci += 1;
            }
            let merged = self.cohorts[from];
            let coh = &mut self.cohorts[into];
            coh.col0 = coh.col0.min(merged.col0);
            coh.col1 = coh.col1.max(merged.col1);
            coh.alive_at = coh.alive_at.max(merged.alive_at);
            coh.phase = Phase::Laying;
            if merged.anchor_col < coh.anchor_col {
                coh.anchor_col = merged.anchor_col;
                coh.t0 = merged.t0;
                coh.d0 = merged.d0;
            }
            let origin = *coh;
            let walk_zero = origin.walk_zero();
            let mut ci = 0;
            while ci < self.cells.len() {
                let c = &mut self.cells[ci];
                if c.cohort == into_id {
                    c.t = walk_zero + walk_t(origin.walk_d(c.col));
                }
                ci += 1;
            }
            self.cohorts.remove(from);
            i = 0;
        }
        self.prune_release_parts();
    }

    /// Build this frame's [`Ribbon::plan`] and [`FieldIndex`].
    #[inline]
    pub fn plan(&mut self, ctx: &Ctx<'_>) {
        self.plan_over(ctx, None);
    }

    /// [`Self::plan`] told what the glyphs below the light hold (`below`, the
    /// frame's [`GlyphProbe`]). A row of light whose next row is probed with
    /// ink under it (a column either side of its span included) is held
    /// inside its own row: its body takes the pane's last-row shape (spine
    /// lifted by the lower reach, so nothing reaches past the row's bottom)
    /// and its rail stays dark, so no light lies on the next line's
    /// ascenders. The pane's last row is held too (no row is under it). A row
    /// first lit over ink is born held, one first lit over a blank or unknown
    /// row is born free; one whose next row inks while it is lit eases into
    /// its row over [`HOLD_EASE_S`], and one whose next row clears, or whose
    /// line flows into the fold, lets go over [`HOLD_RELEASE_S`]; while the
    /// probe does not know the row below (a scroll's frame clears it) the row
    /// keeps its level. A scroll or a band move carries each row's hold with
    /// its line.
    #[inline]
    pub fn plan_over(&mut self, ctx: &Ctx<'_>, below: Option<&GlyphProbe>) {
        self.ink.sync(ctx.cfg);
        self.ink
            .sync_glass(ctx.cfg, glass_table(ctx.cfg, &mut self.palettes));
        self.ground = ctx.cfg.theme_bg;
        self.caret = Some(ctx.caret);
        self.reduced = ctx.cfg.reduced_motion;
        self.cell_h = ctx.geom.ch as f32;
        self.glass_px = if self.ink.glass_lit() {
            self.cell_h * clamp01(ctx.cfg.intensity)
        } else {
            0.0
        };
        self.wedge_px = (Self::body_profile(ctx.cfg).dn_ch - DN_FLOOR_CH).max(0.0) * self.cell_h;
        self.advance_swoosh(ctx);
        self.retire(ctx);
        if let Some(s) = self.seam
            && ctx.now.saturating_duration_since(s.at) >= FLOW_TOTAL_DUR
        {
            self.seam = None;
        }
        self.heal_seams();
        self.sync_glass_wrap(ctx);
        self.build(ctx, below);
        self.carried_holds.clear();
        self.scroll_followed = false;
        self.followed_now.clear();
        self.short_reanchor = None;
        if matches!(self.rewrap_start, Some(rewrap) if rewrap.width.is_some()) {
            self.rewrap_start = None;
        }
        self.shifted_now.clear();
    }

    #[inline]
    fn advance_swoosh(&mut self, ctx: &Ctx<'_>) {
        if self.cohorts.is_empty() {
            return;
        }
        let now = ctx.now;
        let pane_col0 = match self.pane {
            Some((c0, _)) => c0,
            None => 0,
        };
        let mut reach = [(0u16, 0u16, 0u32); REACH_STAGE];
        let mut n_reach = 0usize;
        let mut i = 0;
        while i < self.cohorts.len() {
            let coh = self.cohorts[i];
            let idle = coh.idle_s(now);
            let Some(phase) = coh.phase_at(idle) else {
                self.cohorts[i].phase = Phase::Fading;
                i += 1;
                continue;
            };
            self.cohorts[i].phase = phase;
            if phase.is_retracting() && self.cohorts[i].retract_col.is_none() {
                let coh = self.cohorts[i];
                if coh.flow.is_some() {
                    let (target, right_first, drift) = self.flow_target(&coh);
                    self.cohorts[i].retract_col = target;
                    self.cohorts[i].drain_right_first = right_first;
                    self.cohorts[i].flow_drift = drift;
                } else {
                    let (target, right_first) = self.retract_target(&coh);
                    self.cohorts[i].retract_col = target;
                    self.cohorts[i].drain_right_first = right_first;
                }
            }
            if phase != Phase::Reaching || ctx.cfg.reduced_motion || self.curtain.is_some() {
                i += 1;
                continue;
            }
            const INV_REACH_STEP_S: f32 = 1.0 / REACH_STEP_S;
            let beats = (((idle - coh.grace_s) * INV_REACH_STEP_S).floor() as i32 + 1)
                .clamp(0, i32::from(REACH_BEATS)) as u16;
            let span = coh.col1.saturating_sub(coh.col0);
            let want = FOUR_LETTER_CELLS.saturating_sub(span).min(beats);
            let reach_floor = self.hand_floor_on(coh.row).unwrap_or(0).max(pane_col0);
            let mut k = 0;
            while k < want {
                let Some(c) = coh.col0.checked_sub(k + 1) else {
                    break;
                };
                if c < reach_floor {
                    break;
                }
                if n_reach == REACH_STAGE {
                    break;
                }
                reach[n_reach] = (coh.row, c, coh.id);
                n_reach += 1;
                k += 1;
            }
            i += 1;
        }
        let mut ri = 0;
        while ri < n_reach {
            let (row, col, cohort) = reach[ri];
            ri += 1;
            let mut occupied = false;
            let mut ci = 0;
            while ci < self.cells.len() {
                let c = &self.cells[ci];
                if c.row == row && c.col == col {
                    occupied = true;
                    break;
                }
                ci += 1;
            }
            if occupied {
                continue;
            }
            self.lay_reach_cell(row, col, cohort);
        }
    }

    #[inline]
    fn lay_reach_cell(&mut self, row: u16, col: u16, cohort: u32) {
        let Some(coh_idx) = self.cohort_index(cohort) else {
            return;
        };
        let coh = self.cohorts[coh_idx];
        let mut tail: Option<Cell> = None;
        let mut i = 0;
        while i < self.cells.len() {
            let c = &self.cells[i];
            if c.row == row
                && c.cohort == cohort
                && !matches!(tail, Some(prev) if c.col >= prev.col)
            {
                tail = Some(*c);
            }
            i += 1;
        }
        let Some(tail) = tail else {
            return;
        };
        self.cells.push(Cell {
            row,
            col,
            cohort,
            t: coh.t_at(col),
            born: tail.born,
            attack_at: tail.attack_at,
            life_s: tail.life_s,
            cov0: tail.cov0,
            typing: false,
            retract_at: tail.retract_at,
            retire_at: tail.retire_at,
            birth_disp: tail.birth_disp,
            edge_cells: tail.edge_cells,
            layer: tail.layer,
            rearm: tail.rearm,
            released_at: None,
        });
        self.cohorts[coh_idx].col0 = self.cohorts[coh_idx].col0.min(col);
    }

    #[inline]
    fn retire(&mut self, ctx: &Ctx<'_>) {
        if self.cells.is_empty()
            && self.cohorts.is_empty()
            && self.ember_at.is_none()
            && self.curtain.is_none()
        {
            return;
        }
        let now = ctx.now;
        let ember_done = if let Some(at) = self.ember_at {
            now.saturating_duration_since(at).as_secs_f32() >= FOCUS_EMBER_S
        } else {
            false
        };
        let curtain_done = if let Some(at) = self.curtain {
            now.saturating_duration_since(at).as_secs_f32() >= CURTAIN_S
        } else {
            false
        };
        if ember_done || curtain_done {
            self.cells.clear();
            self.cohorts.clear();
            self.release_parts.clear();
            self.ember_at = None;
            self.curtain = None;
            return;
        }
        let cohorts = &self.cohorts;
        let release_parts = &self.release_parts;
        let mut cached_coh: Option<(&Cohort, bool)> = None;
        self.cells.retain(|cell| {
            if let Some(at) = cell.retire_at
                && now.saturating_duration_since(at).as_secs_f32() >= RETIRE_MELT_S
            {
                return false;
            }
            let (coh, phase_live) = match cached_coh {
                Some((c, live)) if c.id == cell.cohort => (c, live),
                _ => {
                    let Some(c) = Cohort::find(cohorts, cell.cohort) else {
                        return false;
                    };
                    let live = c.phase_at(c.idle_s(now)).is_some();
                    cached_coh = Some((c, live));
                    (c, live)
                }
            };
            if !phase_live {
                return false;
            }
            if coh.flow.is_none()
                && now
                    .saturating_duration_since(cell.born.max(coh.alive_at))
                    .as_secs_f32()
                    >= cell.life_s
            {
                return false;
            }
            if let Some(at) = cell.retract_at
                && now.saturating_duration_since(at).as_secs_f32() >= RETRACT_FADE_S
            {
                let held = coh.holds_partial(cell, release_parts);
                if !held {
                    return false;
                }
            }
            true
        });
        let cells = &self.cells;
        let mut gone_freshest: Option<Cohort> = None;
        self.cohorts.retain_mut(|coh| {
            let resident = if coh.flow.is_none() {
                let mut res = false;
                let mut i = 0;
                while i < cells.len() {
                    if cells[i].cohort == coh.id {
                        res = true;
                        break;
                    }
                    i += 1;
                }
                res
            } else {
                let mut resident = false;
                let mut lo = u16::MAX;
                let mut hi = 0u16;
                let mut i = 0;
                while i < cells.len() {
                    let cell = &cells[i];
                    if cell.cohort == coh.id {
                        resident = true;
                        if !cell.leaving() {
                            if cell.col < lo {
                                lo = cell.col;
                            }
                            if cell.col > hi {
                                hi = cell.col;
                            }
                        }
                    }
                    i += 1;
                }
                if lo <= hi
                    && let Some(edge) = &mut coh.retract_col
                {
                    *edge = (*edge).clamp(lo, hi);
                    coh.flow_drift = coh
                        .flow_drift
                        .min(FLOW_DRIFT_MAX_SHARE * (f32::from(hi - lo) + 1.0));
                }
                resident
            };
            if !resident {
                let fresher = match gone_freshest {
                    Some(a) => is_fresher_or_equal(coh, &a),
                    None => true,
                };
                if fresher {
                    gone_freshest = Some(*coh);
                }
            }
            resident
        });
        if let Some(gone) = gone_freshest {
            self.last_walk = Some(LastWalk {
                end: gone.walk_at(gone.col1),
                alive_at: gone.alive_at,
            });
        }
        self.prune_release_parts();
    }

    #[inline]
    fn build(&mut self, ctx: &Ctx<'_>, below: Option<&GlyphProbe>) {
        let cols = ctx.geom.cols.min(u16::MAX as usize) as u16;
        self.plan.clear();
        self.runs.clear();
        if let Some((row, col)) = self.attach
            && ctx.caret != (row, col)
        {
            let mut active_at_attach = false;
            let mut i = 0;
            while i < self.cells.len() {
                let c = &self.cells[i];
                if c.row == row && c.col == col && !c.leaving() {
                    active_at_attach = true;
                    break;
                }
                i += 1;
            }
            if !active_at_attach {
                let mut j = 0;
                while j < self.cells.len() {
                    let c = &mut self.cells[j];
                    if c.row == row
                        && c.col == col
                        && let Some(at) = c.retract_at
                        && at < ctx.now
                    {
                        c.retract_at = Some(ctx.now);
                    }
                    j += 1;
                }
            }
        }
        self.attach = None;
        self.attach_floor = None;
        self.index.begin(cols);
        if self.cells.is_empty() {
            self.holds.clear();
            self.holds_next.clear();
            return;
        }
        let mut sorted = mem::take(&mut self.sorted);
        sorted.clear();
        sorted.reserve(self.cells.len());
        let mut strict_order = true;
        let mut prev_rc: Option<(u16, u16)> = None;
        let mut i = 0;
        while i < self.cells.len() {
            let c = &self.cells[i];
            // Expired partial releases may remain in content custody, but own neither field nor geometry until their glyphs return.
            if let Some(at) = c.retract_at
                && ctx.now.saturating_duration_since(at).as_secs_f32() >= RETRACT_FADE_S
            {
                i += 1;
                continue;
            }
            let rc = (c.row, c.col);
            if let Some(prev) = prev_rc
                && prev >= rc
            {
                strict_order = false;
            }
            prev_rc = Some(rc);
            sorted.push((c.row, c.col, i.min(u32::MAX as usize) as u32));
            i += 1;
        }
        #[inline]
        fn cell_sort_rank(cells: &[Cell], now: Instant, i: u32) -> u8 {
            match cells.get(i as usize) {
                Some(c) if c.layer == Layer::Over => u8::from(now < c.born) * 2,
                _ => 1,
            }
        }
        if !strict_order {
            let now = ctx.now;
            let cells = self.cells.as_slice();
            sorted.sort_unstable_by(|a, b| {
                (a.0, a.1)
                    .cmp(&(b.0, b.1))
                    .then(cell_sort_rank(cells, now, a.2).cmp(&cell_sort_rank(cells, now, b.2)))
                    .then(b.2.cmp(&a.2))
            });
            sorted.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        }
        let mut si = 0;
        while si < sorted.len() {
            let (row, col, idx) = sorted[si];
            si += 1;
            let Some(cell) = self.cells.get(idx as usize) else {
                continue;
            };
            self.index.set(row, col, cell.t);
        }
        self.slabs = Self::slabs_for(ctx, sorted.len());
        self.glass_slabs = if self.ink.glass_lit() {
            #[cfg(any(test, debug_assertions))]
            count_glass_exit();
            // The plan keeps main's vertices (band() is main's); the glass lays its slabs between
            // them, a whole number to each, never fewer than one.
            let plan = self.slabs_per_cell();
            plan * (Self::glass_slabs_for(ctx, sorted.len(), rows_of(&sorted)) / plan).max(1)
        } else {
            0
        };
        self.build_runs(ctx, &sorted, below);
        self.sorted = sorted;
        self.runs
            .sort_unstable_by(|a, b| b.at_caret.cmp(&a.at_caret).then(b.born.cmp(&a.born)));
    }

    #[inline]
    const fn slabs_for(ctx: &Ctx<'_>, cells: usize) -> usize {
        let prof = Self::body_profile(ctx.cfg);
        let ch = ctx.geom.ch as f32;
        let body = ((prof.up_ch + DN_TOP_CH) * ch).ceil().max(1.0) + 2.0;
        let rail = if Self::rail_drawn(ctx.cfg) {
            (DN_TOP_CH * ch).ceil() + 1.0
        } else {
            0.0
        };
        let rows = body + rail;
        let cells = cells + 1;
        let per_slab = (cells as f32 * rows).max(1.0);
        let fit = (RIBBON_QUAD_BUDGET as f32 / per_slab).floor();
        let n = fit.max(1.0) as usize;
        if n > SLABS_PER_CELL {
            SLABS_PER_CELL
        } else {
            n
        }
    }

    /// The glass's slab density for `cells` cells over `rows` rows (`docs/design/PRISM-GLASS.md`
    /// §2.2 Quad budget, §15.2 1.8): the densest of one to [`SLABS_PER_CELL`] slabs a cell whose
    /// price stays within [`RIBBON_QUAD_BUDGET`] ([`glass_frame_price`]), one where none does.
    #[inline]
    const fn glass_slabs_for(ctx: &Ctx<'_>, cells: usize, rows: usize) -> usize {
        let ch = ctx.geom.ch as f32;
        let mut n = SLABS_PER_CELL;
        while n > 1 && glass_frame_price(ch, cells, rows, n) > RIBBON_QUAD_BUDGET as f32 {
            n -= 1;
        }
        n
    }

    #[inline]
    fn build_runs(
        &mut self,
        ctx: &Ctx<'_>,
        sorted: &[(u16, u16, u32)],
        below: Option<&GlyphProbe>,
    ) {
        #[inline]
        const fn cohort_of(cells: &[Cell], e: (u16, u16, u32)) -> Option<u32> {
            let idx = e.2 as usize;
            if idx < cells.len() {
                Some(cells[idx].cohort)
            } else {
                None
            }
        }
        // One answer per row (its runs share seams, so they share a floor).
        let mut held_row: Option<(u16, usize, (f32, bool))> = None;
        let chf = ctx.geom.ch as f32;
        let pane_bot = self.pane_bot(ctx, chf);
        let mut s = 0usize;
        while s < sorted.len() {
            let row = sorted[s].0;
            let (row_end, (hold, letters_below)) = match held_row {
                Some((r, end, hold)) if r == row => (end, hold),
                _ => {
                    let mut end = s + 1;
                    while end < sorted.len() && sorted[end].0 == row {
                        end += 1;
                    }
                    let row_cells = &sorted[s..end];
                    let last = match row_cells.last() {
                        Some(e) => e.1,
                        None => sorted[s].1,
                    };
                    let floor = f32::from(ctx.geom.origin_y) + (f32::from(row) + 1.0) * chf;
                    let held = if floor >= pane_bot {
                        Some(true)
                    } else if let Some(p) = below {
                        Self::inked_below(p, ctx, row, sorted[s].1, last)
                    } else {
                        None
                    };
                    let held = match held {
                        Some(true) => {
                            let mut seen_coh = None;
                            let mut flowing = false;
                            let mut rci = 0;
                            while rci < row_cells.len() {
                                let e = row_cells[rci];
                                rci += 1;
                                let Some(id) = cohort_of(&self.cells, e) else {
                                    continue;
                                };
                                if seen_coh == Some(id) {
                                    continue;
                                };
                                seen_coh = Some(id);
                                if let Some(c) = self.cohort_by_id(id)
                                    && c.flow.is_some()
                                    && c.phase.is_retracting()
                                {
                                    flowing = true;
                                    break;
                                }
                            }
                            Some(!flowing)
                        }
                        Some(false) => Some(false),
                        None => None,
                    };
                    let previous = self.previous_hold(row, row_cells);
                    let hold = self.ease_hold(ctx, row, held, previous);
                    let hold = (hold, held == Some(true) && floor < pane_bot);
                    held_row = Some((row, end, hold));
                    (end, hold)
                }
            };
            let run_coh = cohort_of(&self.cells, sorted[s]);
            let mut e = s;
            while e + 1 < row_end
                && sorted[e + 1].1 == sorted[e].1 + 1
                && cohort_of(&self.cells, sorted[e + 1]) == run_coh
            {
                e += 1;
            }
            let row = sorted[s].0;
            #[inline]
            const fn nb(
                cells: &[Cell],
                sorted: &[(u16, u16, u32)],
                row: u16,
                i: Option<usize>,
                col: Option<u16>,
            ) -> Option<Cell> {
                let (Some(i), Some(col)) = (i, col) else {
                    return None;
                };
                if i >= sorted.len() {
                    return None;
                }
                let e = sorted[i];
                let idx = e.2 as usize;
                if e.0 == row && e.1 == col && idx < cells.len() {
                    Some(cells[idx])
                } else {
                    None
                }
            }
            let abuts_left = nb(
                &self.cells,
                sorted,
                row,
                s.checked_sub(1),
                sorted[s].1.checked_sub(1),
            )
            .is_some();
            let seam = if abuts_left
                && let Some(r) = self.runs.last()
                && r.row == row
                && r.col1.checked_add(1) == Some(sorted[s].1)
                && let Some(idx) = r.hi.checked_sub(1)
            {
                self.plan.get(idx).copied()
            } else {
                None
            };
            let right = nb(
                &self.cells,
                sorted,
                row,
                Some(e + 1),
                sorted[e].1.checked_add(1),
            );
            self.plan_run(ctx, &sorted[s..=e], seam, right, (hold, letters_below));
            s = e + 1;
        }
        mem::swap(&mut self.holds, &mut self.holds_next);
        self.holds_next.clear();
    }

    /// The strongest prior shape among the cohorts actually drawing this row.
    /// A moved-out run contributes nothing; a partial move leaves its original
    /// source alongside the copy's carried shape. A no-move, no-birth frame
    /// reads its row directly, and a move scans each contiguous cohort once.
    #[inline]
    fn previous_hold(&self, row: u16, row_cells: &[(u16, u16, u32)]) -> Option<(f32, Instant)> {
        let original = self.row_hold(row);
        if self.carried_holds.is_empty() {
            return original;
        }
        let mut seen = None;
        let mut strongest: Option<(f32, Instant)> = None;
        let mut i = 0;
        while i < row_cells.len() {
            let idx = row_cells[i].2;
            i += 1;
            let Some(cell) = self.cells.get(idx as usize) else {
                continue;
            };
            if seen == Some(cell.cohort) {
                continue;
            };
            seen = Some(cell.cohort);
            let mut found = None;
            let mut j = 0;
            while j < self.carried_holds.len() {
                let (id, held) = self.carried_holds[j];
                if id == cell.cohort {
                    found = Some(held);
                    break;
                }
                j += 1;
            }
            let previous = found.unwrap_or(original);
            strongest = Self::strongest_hold(strongest, previous);
        }
        strongest
    }

    #[inline]
    const fn row_hold(&self, row: u16) -> Option<(f32, Instant)> {
        let holds = self.holds.as_slice();
        let mut j = 0;
        while j < holds.len() {
            if holds[j].0 == row {
                return Some((holds[j].1, holds[j].2));
            }
            j += 1;
        }
        None
    }

    #[inline]
    fn strongest_hold(
        a: Option<(f32, Instant)>,
        b: Option<(f32, Instant)>,
    ) -> Option<(f32, Instant)> {
        match (a, b) {
            (Some(a), Some(b)) => Some(if a.0 > b.0 || (a.0 == b.0 && a.1 > b.1) {
                a
            } else {
                b
            }),
            (Some(a), None) => Some(a),
            (None, b) => b,
        }
    }

    #[inline]
    const fn cohort_hold(&self, cohort: u32, row: u16) -> Option<(f32, Instant)> {
        let carried = self.carried_holds.as_slice();
        let mut i = 0;
        while i < carried.len() {
            if carried[i].0 == cohort {
                return carried[i].1;
            }
            i += 1;
        }
        self.row_hold(row)
    }

    #[inline]
    fn stage_hold(&mut self, cohort: u32, previous: Option<(f32, Instant)>) {
        let mut i = 0;
        while i < self.carried_holds.len() {
            let (id, held) = &mut self.carried_holds[i];
            if *id == cohort {
                *held = previous;
                return;
            }
            i += 1;
        }
        self.carried_holds.push((cohort, previous));
    }

    /// Adopting old visual cells keeps their shape while any old cells left
    /// behind retain their own custody. Fresh identities explicitly carry no
    /// prior shape, so they cannot revive a departed occupant's row hold.
    #[inline]
    fn copy_hold(&mut self, from: u32, into: u32, row: u16) {
        if from == into || self.carried_holds.is_empty() {
            return;
        }
        let previous =
            Self::strongest_hold(self.cohort_hold(from, row), self.cohort_hold(into, row));
        self.stage_hold(into, previous);
    }

    /// A whole-cohort merge retires the removed identity after transferring
    /// its strongest shape to the survivor.
    #[inline]
    fn merge_hold(&mut self, from: u32, into: u32, row: u16) {
        if from == into {
            return;
        }
        self.copy_hold(from, into, row);
        self.carried_holds.retain(|h| h.0 != from);
    }

    /// Row `row`'s hold this frame, eased toward `held` from `last`
    /// ([`HOLD_EASE_S`] in, [`HOLD_RELEASE_S`] out), or born at it; recorded
    /// for the next frame. `None` (the row below not known: the frame of a
    /// scroll, which clears the probe, or a row away from the caret's) keeps
    /// the level the row had, and a row first lit so is born free.
    #[inline]
    fn ease_hold(
        &mut self,
        ctx: &Ctx<'_>,
        row: u16,
        held: Option<bool>,
        last: Option<(f32, Instant)>,
    ) -> f32 {
        #[cfg(test)]
        let steps = self.hold_steps;
        #[cfg(not(test))]
        let steps = false;
        let level = match (held, last) {
            (None, Some((level, _))) => level,
            (None, None) => 0.0,
            (Some(h), Some((level, at))) if !steps && !ctx.cfg.reduced_motion => {
                let target = if h { 1.0 } else { 0.0 };
                let dt = ctx.now.saturating_duration_since(at).as_secs_f32();
                if target > level {
                    (level + dt * (1.0 / HOLD_EASE_S)).min(target)
                } else {
                    (level - dt * (1.0 / HOLD_RELEASE_S)).max(target)
                }
            }
            (Some(h), _) => {
                if h {
                    1.0
                } else {
                    0.0
                }
            }
        };
        self.holds_next.push((row, level, ctx.now));
        level
    }

    /// Does `probe` know ink on the row below `row`, under `col0..=col1` or a
    /// column either side? `Some(false)` when it knows every one of them
    /// blank, `None` when it knows no ink there and not all of them.
    #[inline]
    const fn inked_below(
        probe: &GlyphProbe,
        ctx: &Ctx<'_>,
        row: u16,
        col0: u16,
        col1: u16,
    ) -> Option<bool> {
        let below = row as i32 + 1;
        if below < 0 || (below as usize) >= ctx.geom.rows {
            return None;
        }
        let mut known = true;
        let mut col = col0.saturating_sub(1);
        let end = col1.saturating_add(1);
        loop {
            match probe.at(below, col, ctx.geom.rows) {
                Some(true) => return Some(true),
                Some(false) => {}
                None => known = false,
            }
            if col == end {
                break;
            }
            col += 1;
        }
        if known { Some(false) } else { None }
    }
}

#[derive(Clone, Copy)]
struct RunSample {
    chf: f32,
    prof: BodyProfile,
    reduced: bool,
    disp: f32,
    room: f32,
    span: f32,
    row_bot: f32,
    dn_floor: f32,
    settle: f32,
    settle_scale: f32,
    breath_scale: f32,
    comet: bool,
    fx_top: f32,
    pane_bot: f32,
    free_last_row: bool,
    held_last_row: bool,
    hold: f32,
    /// The row below holds letters this frame ([`Ribbon::inked_below`]).
    letters_below: bool,
    phase_rad: f32,
    /// How far the run's row has sunk ([`GlassWrap`]), on glass only; `0` elsewhere.
    sunk: f32,
    /// The most the run's glass reaches, px ([`WRAP_HAND_TOP_PX`]), on glass only; unbounded
    /// elsewhere.
    glass_cap: f32,
}

impl Ribbon {
    #[inline]
    const fn run_sample(
        &self,
        ctx: &Ctx<'_>,
        row: u16,
        coh: Option<&Cohort>,
        (hold, letters_below): (f32, bool),
        (settle, breath): (f32, f32),
    ) -> RunSample {
        let chf = ctx.geom.ch as f32;
        let prof = Self::body_profile(ctx.cfg);
        #[cfg(test)]
        let prof = if self.underline_lifted {
            BodyProfile { tall: true, ..prof }
        } else {
            prof
        };
        let reduced = ctx.cfg.reduced_motion;
        let disp = clamp01(ctx.disp);
        let room = (DN_TOP_CH - DN_FLOOR_CH) * chf;
        let span = match coh {
            Some(c) => (c.col1.saturating_sub(c.col0) as f32).max(1.0),
            None => 1.0,
        };
        let row_bot = ctx.geom.origin_y as f32 + (row as f32 + 1.0) * chf;
        let dn_floor = DN_FLOOR_CH * chf;
        let settle_scale = 1.0 - FLOW_SETTLE_SHARE * settle;
        let breath_scale = 1.0 - breath;
        let comet = Self::comet(ctx.cfg);
        let fx_top = ctx.geom.fx_top() as f32;
        let pane_bot = self.pane_bot(ctx, chf);
        let dn_thresh = prof.dn_ch * chf;
        let free_last_row = (pane_bot - row_bot) < dn_thresh;
        let held_last_row = 0.0 < dn_thresh;
        let phase_rad = ctx.phase * std::f32::consts::TAU;
        RunSample {
            chf,
            prof,
            reduced,
            disp,
            room,
            span,
            row_bot,
            dn_floor,
            settle,
            settle_scale,
            breath_scale,
            comet,
            fx_top,
            pane_bot,
            free_last_row,
            held_last_row,
            hold,
            letters_below,
            phase_rad,
            sunk: 0.0,
            glass_cap: f32::INFINITY,
        }
    }

    /// `cell`'s shape at `sp` of its run's span: `(spine, up, dn, cov, glass_up, line_keep)`
    /// ([`Segment`]).
    #[inline]
    fn sample_with(
        &self,
        ctx: &Ctx<'_>,
        cell: &Cell,
        coh: Option<&Cohort>,
        sp: f32,
        rs: &RunSample,
    ) -> (f32, f32, f32, f32, f32, f32) {
        let chf = rs.chf;
        let prof = rs.prof;
        let disp = rs.disp;
        let amp = if rs.reduced || disp <= 0.0 || rs.settle >= 1.0 || sp <= 0.0 {
            0.0
        } else {
            (WAVE_AMP_CELLS * chf * disp).min(rs.room)
                * smoothstep01(sp * (1.0 / WAVE_HEAD_PIN))
                * (1.0 - rs.settle)
        };
        let wave = if amp == 0.0 {
            0.0
        } else {
            crate::supernova::sin_cos_f32((sp * std::f32::consts::TAU * WAVE_CYCLES) - rs.phase_rad)
                .0
                * amp
        };
        let behind = sp * rs.span;
        let bloom = if disp <= 0.0 || behind >= BLOOM_REACH_CELLS {
            0.0
        } else {
            clamp01(disp * (1.0 - smoothstep01(behind * (1.0 / BLOOM_REACH_CELLS))))
        };
        let row_bot = rs.row_bot;
        let wave_pos = wave.max(0.0);
        let wave_neg = wave.min(0.0);
        let spine_raw = row_bot + wave;
        let comet_up_ch = if rs.comet {
            prof.up_ch
                * (1.0
                    - (1.0 - COMET_UP_TAIL_CH / TALL_UP_CH)
                        * smoothstep01(behind * (1.0 / COMET_TAPER_CELLS)))
        } else {
            prof.up_ch
        };
        // The body over a floor at `pane_bot`: a held row's floor is its own
        // bottom, where it is laid as a last row is.
        #[inline]
        const fn shape_at(
            (pane_bot, last_row): (f32, bool),
            rs: &RunSample,
            bloom: f32,
            wave_pos: f32,
            wave_neg: f32,
            comet_up_ch: f32,
            spine_raw: f32,
        ) -> (f32, f32, f32) {
            let dn_ch = if last_row { DN_TOP_CH } else { rs.prof.dn_ch };
            let dn_open = rs.chf * (DN_FLOOR_CH + (dn_ch - DN_FLOOR_CH) * bloom);
            let dn = (dn_open - wave_pos).max(rs.dn_floor);
            let dn = (rs.dn_floor + (dn - rs.dn_floor) * rs.settle_scale) * rs.breath_scale;
            let up_ch = if last_row { rs.prof.up_ch } else { comet_up_ch };
            let up = (up_ch * rs.chf + wave_neg).max(0.0) * rs.settle_scale * rs.breath_scale;
            if !rs.prof.tall {
                // The underline keeps to its row's foot band over a floor:
                // lifted by its reach below, as the tall ribbon is, it lay
                // across the lower half of a last or held row's letters
                // (looks10 L3: the dim Solarized `c` of an Enter at the
                // bottom row at 96.9% of its own reading). Its reach below
                // ends at the floor instead.
                let spine = spine_raw.min(pane_bot).max(rs.fx_top);
                let room = pane_bot - spine;
                let dn = if dn < room {
                    dn
                } else if room > 0.0 {
                    room
                } else {
                    0.0
                };
                return (spine, up, dn);
            }
            let spine = spine_raw.min(pane_bot - dn).max(rs.fx_top);
            (spine, up, dn)
        }
        #[inline]
        const fn mix(a: f32, b: f32, hold: f32) -> f32 {
            a + (b - a) * hold
        }
        let pane_bot = rs.pane_bot;
        let hold = rs.hold;
        // The underline settles at once off the letters that land on the
        // row below: eased in with the row's hold ([`HOLD_EASE_S`]), its
        // reach below lay over the new letters' tops for the ease (the
        // settings card's Return on GitHub Light: the `p` of `[main 4f2c9e1]`
        // read 4.45:1 on its tinted ground, looks10 round 3). Kept to its
        // floor (looks10 L3) the underline's spine does not move, as the tall
        // body's does, so only its reach below changes; it lets go of the row
        // as it did ([`HOLD_RELEASE_S`]), over a row whose letters are gone.
        #[cfg(test)]
        let settles = rs.letters_below && !self.underline_eases_off_letters;
        #[cfg(not(test))]
        let settles = rs.letters_below;
        let hold = if settles && !rs.prof.tall { 1.0 } else { hold };
        let (spine, up, dn) = if hold <= 0.0 || pane_bot <= row_bot {
            shape_at(
                (pane_bot, rs.free_last_row),
                rs,
                bloom,
                wave_pos,
                wave_neg,
                comet_up_ch,
                spine_raw,
            )
        } else if hold >= 1.0 {
            shape_at(
                (row_bot, rs.held_last_row),
                rs,
                bloom,
                wave_pos,
                wave_neg,
                comet_up_ch,
                spine_raw,
            )
        } else {
            let free = shape_at(
                (pane_bot, rs.free_last_row),
                rs,
                bloom,
                wave_pos,
                wave_neg,
                comet_up_ch,
                spine_raw,
            );
            let held = shape_at(
                (row_bot, rs.held_last_row),
                rs,
                bloom,
                wave_pos,
                wave_neg,
                comet_up_ch,
                spine_raw,
            );
            (
                mix(free.0, held.0, hold),
                mix(free.1, held.1, hold),
                mix(free.2, held.2, hold),
            )
        };
        if self.ink.glass_lit() {
            // The glass's exits scale its height, its opacity moving only at the edge-in and
            // the reduced fade (§2.2 Exits by height).
            let env = self.glass_env(ctx, cell, coh);
            let glass = Self::glass_up_at(ctx, rs, sp) * env.height;
            let cov = Self::glass_cov(ctx.cfg, cell, env);
            return (
                spine,
                up,
                dn,
                cov,
                glass,
                env.height * clamp01(ctx.cfg.intensity) * rs.breath_scale,
            );
        }
        (
            spine,
            up,
            dn,
            self.cov_of(ctx, cell, coh),
            0.0,
            rs.breath_scale,
        )
    }

    /// The glass's reach above its row's bottom at a boundary `sp` of the run's span from its
    /// head ([`GLASS_BODY`], `docs/design/PRISM-GLASS.md` §2.2, §15.2 1.7): [`GLASS_UP_CH`] at
    /// the hand, easing to [`GLASS_TAIL_UP_CH`] over [`COMET_TAPER_CELLS`] on the comet, its top
    /// swinging [`LIGHT_GLASS_WAVE_CH`] with main's wave (where main's spine swings, at its
    /// phase), under the flow's settle and breath as main's height is, and scaled by the frame's
    /// intensity: the glass's intensity is its height, never its opacity. On a row of the hand's
    /// structural set it is then sunk and capped there ([`wrapped_up`], §15.2 1.10).
    #[inline]
    fn glass_up_at(ctx: &Ctx<'_>, rs: &RunSample, sp: f32) -> f32 {
        let chf = rs.chf;
        let disp = rs.disp;
        let behind = sp * rs.span;
        let reach_ch = if rs.comet {
            GLASS_UP_CH
                - (GLASS_UP_CH - GLASS_TAIL_UP_CH)
                    * smoothstep01(behind * (1.0 / COMET_TAPER_CELLS))
        } else {
            GLASS_UP_CH
        };
        let wave = if rs.reduced || disp <= 0.0 || rs.settle >= 1.0 || sp <= 0.0 {
            0.0
        } else {
            crate::supernova::sin_cos_f32((sp * std::f32::consts::TAU * WAVE_CYCLES) - rs.phase_rad)
                .0
                * LIGHT_GLASS_WAVE_CH
                * chf
                * disp
                * smoothstep01(sp * (1.0 / WAVE_HEAD_PIN))
                * (1.0 - rs.settle)
        };
        let up = (reach_ch * chf - wave).max(0.0)
            * rs.settle_scale
            * rs.breath_scale
            * clamp01(ctx.cfg.intensity);
        wrapped_up(up, chf, rs.sunk, rs.glass_cap)
    }

    #[inline]
    fn env_of(&self, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        self.env_with(ctx, cell, self.cohort_by_id(cell.cohort))
    }

    /// Read a live cell's visibility envelope at `ctx.now` for conformance tests.
    #[cfg(test)]
    #[inline]
    #[must_use]
    pub(crate) fn debug_cell_envelope(&self, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        self.env_of(ctx, cell)
    }

    /// The cell's envelope as main folds it: one product of every factor, its ember floor main's
    /// ([`BedInkLut::main_seen_floor_at`]). Every decision the ribbon makes on a cell's light
    /// reads it, on a frame the glass draws too.
    #[inline]
    fn env_with(&self, ctx: &Ctx<'_>, cell: &Cell, coh: Option<&Cohort>) -> f32 {
        self.envelope(ctx, cell, coh, MainEnv(1.0)).0
    }

    /// The cell's glass envelope (`docs/design/PRISM-GLASS.md` §2.2 Exits by height, §15.2 1.8):
    /// `(opacity, height)`, the factors [`Self::envelope`] folds split between the glass's
    /// opacity and its height ([`GlassEnv`]).
    #[inline]
    fn glass_env(&self, ctx: &Ctx<'_>, cell: &Cell, coh: Option<&Cohort>) -> GlassEnv {
        #[cfg(any(test, debug_assertions))]
        count_glass_exit();
        let reach = GLASS_TAIL_UP_CH * (ctx.geom.ch as f32 * clamp01(ctx.cfg.intensity));
        let mut env = GlassEnv::new(ctx.cfg.reduced_motion, reach);
        env.left =
            left_rows_fade() && coh.is_some_and(|c| self.sunk_on(c.id, c.row, ctx.now) > 0.0);
        self.envelope(ctx, cell, coh, env)
    }

    /// Fold the cell's envelope into `e`: its attack, its melt, each exit, the printed dim, the
    /// focus ember and the relight, in that order ([`Envelope`]).
    #[inline]
    fn envelope<E: Envelope>(
        &self,
        ctx: &Ctx<'_>,
        cell: &Cell,
        coh: Option<&Cohort>,
        mut e: E,
    ) -> E {
        let now = ctx.now;
        if now < cell.born {
            return E::DARK;
        }
        let reduced = ctx.cfg.reduced_motion;
        let attack_now = match cell.retire_at {
            Some(at) => now.min(at),
            None => now,
        };
        let age = attack_now
            .saturating_duration_since(cell.attack_at)
            .as_secs_f32();
        let spent_to = match coh {
            Some(Cohort { flow: Some(f), .. }) => now.min(*f),
            _ => now,
        };
        let spent = spent_to
            .saturating_duration_since(match coh {
                Some(c) => cell.born.max(c.alive_at),
                None => cell.born,
            })
            .as_secs_f32();
        let u = (spent / cell.life_s.max(1e-3)).clamp(0.0, 1.0);
        match cell.rearm {
            Some((re_at, level)) => {
                let r = attack_now.saturating_duration_since(re_at).as_secs_f32();
                e.fade(level + (1.0 - level) * edge_in(r));
            }
            None => e.edge_in(BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age)),
        }
        e.fade(if reduced {
            reduced_fade(cell.life_s - spent)
        } else {
            expiry_melt(u)
        });
        if let Some(at) = cell.retract_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            e.fade(if reduced {
                reduced_fade(RETRACT_FADE_S - r)
            } else {
                spend(clamp01(r * (1.0 / RETRACT_FADE_S)))
            });
        }
        if let Some(at) = cell.retire_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            e.fade(if reduced {
                reduced_fade(RETIRE_MELT_S - r)
            } else {
                spend(clamp01(r * (1.0 / RETIRE_MELT_S)))
            });
        }
        if let Some(at) = self.curtain {
            let r = now.saturating_duration_since(at).as_secs_f32();
            e.fade(if reduced {
                reduced_fade(CURTAIN_S - r)
            } else {
                let rank = match coh {
                    Some(c) => c.drain_rank(cell.col),
                    None => 1.0,
                };
                let t0 = CURTAIN_STAGGER_S * (1.0 - rank);
                spend(clamp01((r - t0) * (1.0 / CURTAIN_SPEND_S)))
            });
        }
        if let Some(Cohort {
            printed: Some(at), ..
        }) = coh
        {
            let r = now.saturating_duration_since(*at).as_secs_f32();
            e.scale(1.0 - (1.0 - RETURN_PRINTED_SHARE) * smoothstep01(r * (1.0 / RETURN_DIM_S)));
        }
        if let Some(coh) = coh
            && coh.phase.is_retracting()
        {
            let idle = coh.idle_s(now);
            if reduced {
                e.fade(reduced_fade(coh.swoosh_total() - idle));
            } else {
                // Farthest from the hand first ([`Cohort::drain_rank`]), each cell taking `RETRACT_FADE_S`, so the mark is drawn back into the hand.
                if coh.flow.is_some() {
                    e.fade(ember_level(
                        e.ember_floor(self, ctx, cell),
                        coh.flow_drain(cell.col, idle),
                    ));
                } else {
                    let t0 =
                        coh.retract_start() + coh.retract_span() * (1.0 - coh.drain_rank(cell.col));
                    e.fade(spend(clamp01((idle - t0) * (1.0 / RETRACT_FADE_S))));
                }
            }
        }
        if let Some(at) = self.ember_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            let f = if reduced {
                reduced_fade(FOCUS_EMBER_S - r)
            } else {
                spend(clamp01(r * (1.0 / FOCUS_EMBER_S)))
            };
            e.ember(ember_level(e.ember_floor(self, ctx, cell), f));
        }
        if let Some((at, fade)) = self.rearm {
            let level = ember_level(e.ember_floor(self, ctx, cell), fade);
            let r = attack_now.saturating_duration_since(at).as_secs_f32();
            e.ember(level + (1.0 - level) * edge_in(r));
        }
        e
    }

    /// The cell's coverage: main's cap times its envelope and the frame's intensity; under glass
    /// the glass's whole opacity ([`GLASS_ALPHA`]) times its envelope's opacity alone, its height
    /// carrying the rest ([`Self::glass_env`], [`Self::glass_up_at`]).
    #[inline]
    fn cov_of(&self, ctx: &Ctx<'_>, cell: &Cell, coh: Option<&Cohort>) -> f32 {
        let glass = self.ink.glass_lit();
        let cap = body_cov_cap(ctx.cfg, glass);
        if glass {
            return Self::glass_cov(ctx.cfg, cell, self.glass_env(ctx, cell, coh));
        }
        clamp_bed_cov(
            ctx.cfg,
            glass,
            cap * cell.cov0 * self.env_with(ctx, cell, coh) * clamp01(ctx.cfg.intensity),
        )
    }

    /// The glass's coverage of `cell` under its envelope `env`: the glass's whole ceiling
    /// ([`body_cov_cap`]), hot or cold, moved only by the opacity's places.
    #[inline]
    const fn glass_cov(cfg: &Config, cell: &Cell, env: GlassEnv) -> f32 {
        clamp_bed_cov(cfg, true, body_cov_cap(cfg, true) * cell.cov0 * env.opacity)
    }

    /// Main's ember floor for `cell` carrying `carried` of its light, against main's floor.
    #[inline]
    const fn ember_floor(&self, ctx: &Ctx<'_>, cell: &Cell, carried: f32) -> f32 {
        self.ember_floor_of(
            cov_cap(ctx.cfg) * cell.cov0 * clamp01(ctx.cfg.intensity) * carried,
            cell.t,
        )
    }

    #[inline]
    const fn ember_floor_of(&self, full_cov: f32, t: f32) -> f32 {
        if !full_cov.is_finite() || full_cov <= 0.0 {
            return 0.0;
        }
        #[cfg(test)]
        if self.ember_reads_glass {
            return (self.ink.seen_floor_at(t) * EMBER_FLOOR_MARGIN / full_cov).max(0.0);
        }
        (self.ink.main_seen_floor_at(t) * EMBER_FLOOR_MARGIN / full_cov).max(0.0)
    }

    #[inline]
    fn wipe_state(&self, ctx: &Ctx<'_>, cell: &Cell) -> Option<(f32, f32, f32)> {
        // Typed cells only: a wake's cells share one birth instant, so wiping each from its own edge would print a comb; its lay sweep is its attack.
        if !cell.typing
            || !Self::comet(ctx.cfg)
            || ctx.cfg.reduced_motion
            || ctx.now < cell.attack_at
        {
            return None;
        }
        let age = ctx
            .now
            .saturating_duration_since(cell.attack_at)
            .as_secs_f32();
        if age >= ATTACK_WIPE_S {
            return None;
        }
        let ramp = edge_in(age);
        if ramp <= 0.0 {
            return None;
        }
        let front = clamp01(age * (1.0 / ATTACK_WIPE_S)) * (1.0 + ATTACK_WIPE_FRONT);
        let uniform = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * ramp;
        Some((front, ramp, uniform))
    }

    #[inline]
    const fn wipe_eval(state: Option<(f32, f32, f32)>, dist: f32) -> f32 {
        let Some((front, ramp, uniform)) = state else {
            return 1.0;
        };
        let gate = smoothstep01((front - clamp01(dist)) * (1.0 / ATTACK_WIPE_FRONT));
        let wiped = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * ramp * gate;
        wiped / uniform
    }

    #[inline]
    fn wipe_of(&self, ctx: &Ctx<'_>, cell: &Cell, dist: f32) -> f32 {
        Self::wipe_eval(self.wipe_state(ctx, cell), dist)
    }

    #[inline]
    fn retract_x(&self, ctx: &Ctx<'_>, coh: Option<&Cohort>) -> RetractX {
        if ctx.cfg.reduced_motion {
            return RetractX::Identity;
        }
        if let Some(at) = self.curtain {
            let u =
                clamp01(ctx.now.saturating_duration_since(at).as_secs_f32() * (1.0 / CURTAIN_S));
            let Some(Cohort {
                retract_col: Some(col),
                ..
            }) = coh
            else {
                return RetractX::Identity;
            };
            let caret_x = f32::from(ctx.geom.origin_x) + f32::from(*col) * ctx.geom.cw as f32;
            return RetractX::SuckIn {
                caret_x,
                keep: 1.0 - suck_in(u),
            };
        }
        let Some(coh) = coh else {
            return RetractX::Identity;
        };
        if !coh.phase.is_retracting() {
            return RetractX::Identity;
        }
        let idle = coh.idle_s(ctx.now);
        if coh.flow.is_some() {
            let Some(edge) = coh.retract_col else {
                return RetractX::Identity;
            };
            let cw = ctx.geom.cw as f32;
            let ox = f32::from(ctx.geom.origin_x);
            let s = coh.flow_drift * cw * coh.flow_progress(idle);
            return if coh.drain_right_first {
                RetractX::FlowRightFirst {
                    start: ox + f32::from(edge) * cw,
                    s,
                }
            } else {
                RetractX::FlowLeftFirst {
                    end: ox + (f32::from(edge) + 1.0) * cw,
                    s,
                }
            };
        }
        let col = match coh.retract_col {
            Some(col) => col,
            None => match self.caret {
                Some((_, col)) => col,
                None => return RetractX::Identity,
            },
        };
        let u = clamp01((idle - coh.retract_start()) / (coh.retract_span() + RETRACT_FADE_S));
        let caret_x = f32::from(ctx.geom.origin_x) + f32::from(col) * ctx.geom.cw as f32;
        RetractX::SuckIn {
            caret_x,
            keep: 1.0 - suck_in(u),
        }
    }

    #[inline]
    fn head_col(
        &self,
        ctx: &Ctx<'_>,
        run: &[(u16, u16, u32)],
        coh: Option<&Cohort>,
    ) -> (u16, bool, bool, i8) {
        #[inline]
        const fn run_col_live(
            cells: &[Cell],
            run: &[(u16, u16, u32)],
            col0: u16,
            col: u16,
        ) -> bool {
            let idx = (col - col0) as usize;
            if idx >= run.len() {
                return false;
            }
            let cell_idx = run[idx].2 as usize;
            if cell_idx >= cells.len() {
                return false;
            }
            let c = &cells[cell_idx];
            c.col == col && !c.leaving()
        }
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let (crow, ccol) = ctx.caret;
        let at_caret = crow == row && ccol >= col0 && ccol <= col1 + 1;
        if at_caret
            && let Some(own) = ccol.checked_sub(1)
            && own >= col0
            && own <= col1
            && run_col_live(&self.cells, run, col0, own)
        {
            return (own, true, true, -1);
        }
        if at_caret
            && ccol == col0
            && run_col_live(&self.cells, run, col0, col0)
            && matches!(coh, Some(k) if k.wake)
        {
            return (col0, true, true, 1);
        }
        let mut newest: Option<(Instant, u16)> = None;
        let mut ri = 0;
        while ri < run.len() {
            let e = run[ri];
            if let Some(c) = self.cells.get(e.2 as usize)
                && !c.leaving()
                && !matches!(newest, Some((b, _)) if c.born < b)
            {
                newest = Some((c.born, c.col));
            }
            ri += 1;
        }
        match newest {
            Some((_, col)) => (col, at_caret, true, -1),
            None => (col1, at_caret, false, -1),
        }
    }

    #[inline]
    fn plan_run(
        &mut self,
        ctx: &Ctx<'_>,
        run: &[(u16, u16, u32)],
        seam: Option<Segment>,
        right_nb: Option<Cell>,
        (hold, letters_below): (f32, bool),
    ) {
        #[inline]
        const fn cell_at<'a>(
            cells: &'a [Cell],
            run: &[(u16, u16, u32)],
            col0: u16,
            col: u16,
        ) -> Option<&'a Cell> {
            let Some(rel) = col.checked_sub(col0) else {
                return None;
            };
            let i = rel as usize;
            if i >= run.len() {
                return None;
            }
            let e = run[i];
            let cell_idx = e.2 as usize;
            if e.1 == col && cell_idx < cells.len() {
                Some(&cells[cell_idx])
            } else {
                None
            }
        }
        #[inline]
        const fn mid_f32(u: f32, v: f32) -> f32 {
            (u + v) * 0.5
        }
        #[inline]
        const fn lerp_f32(u: f32, v: f32, f: f32) -> f32 {
            u + (v - u) * f
        }
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let Some(first_cell) = self.cells.get(run[0].2 as usize) else {
            return;
        };
        let cohort = first_cell.cohort;
        let coh = self.cohort_by_id(cohort).copied();
        let coh = coh.as_ref();
        let slabs = self.slabs_per_cell();
        let (head_col, at_caret, wet, stream_dir) = self.head_col(ctx, run, coh);
        let mut born_max: Option<Instant> = None;
        let mut ri = 0;
        while ri < run.len() {
            let e = run[ri];
            if let Some(c) = self.cells.get(e.2 as usize)
                && c.typing
                && !matches!(born_max, Some(b) if c.born <= b)
            {
                born_max = Some(c.born);
            }
            ri += 1;
        }
        let born = born_max.unwrap_or(ctx.now);
        let inv_span = 1.0 / f32::from(col1 - col0).max(1.0);
        let cw = ctx.geom.cw as f32;
        let pane_end = match self.pane {
            Some((c0, w)) => c0.saturating_add(w),
            None => ctx.geom.cols.min(u16::MAX as usize) as u16,
        };
        let caret_cell = head_col.wrapping_add(1);
        let under_caret_cell = cell_at(&self.cells, run, col0, caret_cell);
        let under_caret_leaving = match under_caret_cell {
            Some(c) => c.leaving(),
            None => match right_nb.as_ref() {
                Some(nb) => nb.leaving(),
                None => false,
            },
        };
        let caret_free = under_caret_cell.is_none() && right_nb.is_none() || under_caret_leaving;
        let attach = stream_dir < 0
            && wet
            && at_caret
            && ctx.caret.1 == caret_cell
            && ctx.caret.1 < pane_end
            && caret_free;
        let attack = if attach && let Some(&head) = cell_at(&self.cells, run, col0, head_col) {
            let probe = Cell {
                attack_at: head.born,
                ..head
            };
            let age = ctx.now.saturating_duration_since(head.born).as_secs_f32();
            Some((
                BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age),
                probe,
            ))
        } else {
            None
        };
        if attack.is_some() {
            self.attach = Some((row, caret_cell));
        }
        let lo = self.plan.len();
        let mut prev: Option<(f32, Sample)> = None;
        let mut left_sample = None;
        let mut left_wipe = None;
        let mut doubled: Option<usize> = None;
        let caret_side_right = stream_dir < 0;
        let ox = f32::from(ctx.geom.origin_x);
        let inv_slabs = 1.0 / (slabs as f32);
        let retract = self.retract_x(ctx, coh);
        let flow_settle = if !ctx.cfg.reduced_motion
            && let Some(c) = coh
            && c.flow.is_some()
            && c.phase.is_retracting()
        {
            let idle = c.idle_s(ctx.now);
            (
                c.flow_progress(idle),
                smoothstep01((idle - (FLOW_TOTAL_S - FLOW_TAIL_S)) * (1.0 / FLOW_TAIL_S)),
            )
        } else {
            (0.0, 0.0)
        };
        let rs = self.run_sample(ctx, row, coh, (hold, letters_below), flow_settle);
        let glass = self.ink.glass_lit();
        // On glass (§2.4) a row the hand has left sinks, and the hand's row of a multi-row set
        // keeps its inset; main's row hold moves main's body only, so each row's glass has one
        // height owner.
        let rs = if glass {
            RunSample {
                sunk: self.sunk_on(cohort, row, ctx.now),
                glass_cap: if self.wrap_hand == Some(row) {
                    (rs.chf - WRAP_HAND_TOP_PX).max(0.0)
                } else {
                    f32::INFINITY
                },
                ..rs
            }
        } else {
            rs
        };
        // On glass (§2.2 The oldest end) the run's height eases in from its oldest end, the end
        // away from its hand, unless a seam or a neighbour continues it there.
        let tail = GlassTail {
            x0: ox + f32::from(col0) * cw,
            inv_cw: 1.0 / cw,
            len: f32::from(col1 - col0) + 1.0,
            head: f32::from(head_col - col0) + if stream_dir > 0 { 0.0 } else { 1.0 },
            oldest_right: stream_dir > 0,
            open: if stream_dir > 0 {
                right_nb.is_none()
            } else {
                seam.is_none()
            },
        };
        let tail_at = |x: f32| if glass { tail.at(x) } else { 1.0 };
        let mut boundary = u32::from(col0);
        let boundary_end = u32::from(col1) + 1;
        while boundary <= boundary_end {
            let left = if let Some(c) = boundary.checked_sub(1)
                && c <= u32::from(u16::MAX)
            {
                cell_at(&self.cells, run, col0, c as u16)
            } else {
                None
            };
            let right = if boundary <= u32::from(u16::MAX) {
                cell_at(&self.cells, run, col0, boundary as u16)
            } else {
                None
            };
            let (a, b) = match (left, right) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (a, a),
                (None, Some(b)) => (b, b),
                (None, None) => {
                    left_sample = None;
                    left_wipe = None;
                    boundary += 1;
                    continue;
                }
            };
            debug_assert_eq!(left_sample.is_some(), left.is_some());
            let sa = match left_sample {
                Some(sa) => sa,
                None => self.sample_with(
                    ctx,
                    a,
                    coh,
                    f32::from(head_col.abs_diff(a.col)) * inv_span,
                    &rs,
                ),
            };
            let sb = if a.col == b.col {
                sa
            } else {
                self.sample_with(
                    ctx,
                    b,
                    coh,
                    f32::from(head_col.abs_diff(b.col)) * inv_span,
                    &rs,
                )
            };
            left_sample = if right.is_some() { Some(sb) } else { None };
            let wa = match left_wipe {
                Some(wa) => wa,
                None => self.wipe_state(ctx, a),
            };
            let wb = if a.col == b.col {
                wa
            } else {
                self.wipe_state(ctx, b)
            };
            left_wipe = if right.is_some() { Some(wb) } else { None };
            let mut la = if left.is_some() {
                sa.3 * Self::wipe_eval(wa, if caret_side_right { 0.0 } else { 1.0 })
            } else {
                0.0
            };
            let mut lb = if right.is_some() {
                sb.3 * Self::wipe_eval(wb, if caret_side_right { 1.0 } else { 0.0 })
            } else {
                0.0
            };
            let mut shape = (
                mid_f32(sa.0, sb.0),
                mid_f32(sa.1, sb.1),
                mid_f32(sa.2, sb.2),
                mid_f32(a.t, b.t),
                mid_f32(sa.4, sb.4),
                mid_f32(sa.5, sb.5),
            );
            // On glass (§2.2 Seams) an in-run walk jump is a hard step: the boundary closes
            // its left cell at that cell's walk and opens its right one at its own.
            let mut jump = 0.0;
            if glass
                && left.is_some()
                && right.is_some()
                && (b.t - a.t).abs() > GLASS_HARD_STEP_T + GLASS_HARD_STEP_SLACK
            {
                shape.3 = a.t;
                jump = b.t - a.t;
            }
            if left.is_none()
                && let Some(seam) = seam
            {
                la = f32::from(seam.cov);
                // A cohort seam keeps its neighbour's shape, and on glass opens at its own
                // walk: a hard step.
                let t = if glass { shape.3 } else { seam.t };
                shape = (
                    seam.spine,
                    seam.up,
                    seam.dn,
                    t,
                    seam.glass_up,
                    seam.line_keep,
                );
            }
            if right.is_none()
                && let Some(nb) = right_nb.as_ref()
            {
                let nb_coh = self.cohort_by_id(nb.cohort);
                lb = self.cov_of(ctx, nb, nb_coh) * self.wipe_of(ctx, nb, 1.0);
            }
            // The glass's run eases in by height ([`GlassTail`]), never by its opacity.
            let natural = if glass {
                la.max(lb)
            } else {
                la.max(lb * RUN_TAIL_EASE)
            };
            let floored = if let Some(l) = left
                && let Some(floor) = self.attach_floor_at(ctx, row, l, 1.0)
            {
                Some(natural.max(floor))
            } else {
                None
            };
            let cov = floored.unwrap_or(natural);
            let here = (shape.0, shape.1, shape.2, shape.3, cov, shape.4, shape.5);
            if let Some((uniform, probe)) = attack
                && boundary == u32::from(caret_cell)
            {
                self.attach_floor = Some(AttachFloor {
                    row,
                    col: caret_cell,
                    base: cov,
                    uniform,
                    probe,
                });
            }
            let x = ox + boundary as f32 * cw;
            if let Some((px, p)) = prev {
                let has_attach_floor = matches!(
                    self.attach_floor,
                    Some(af) if af.row == row && af.col == a.col && a.leaving()
                );
                let mut j = 1;
                while j < slabs {
                    let f = j as f32 * inv_slabs;
                    let own =
                        sa.3 * Self::wipe_eval(wa, if caret_side_right { 1.0 - f } else { f });
                    let mut cov = lerp_f32(p.4, here.4, f).min(own);
                    if has_attach_floor && let Some(floor) = self.attach_floor_at(ctx, row, a, f) {
                        cov = cov.max(floor);
                    }
                    let xf = lerp_f32(px, x, f);
                    self.plan.push(Segment {
                        x: retract.at(xf).round(),
                        spine: lerp_f32(p.0, here.0, f),
                        up: lerp_f32(p.1, here.1, f),
                        dn: lerp_f32(p.2, here.2, f),
                        t: lerp_f32(p.3, here.3, f),
                        cov: cov.clamp(0.0, 255.0) as u8,
                        glass_up: lerp_f32(p.5, here.5, f) * tail_at(xf),
                        line_keep: lerp_f32(p.6, here.6, f) * tail_at(xf),
                        glass_jump: 0.0,
                    });
                    j += 1;
                }
            }
            let rx = retract.at(x).round();
            let tail_here = tail_at(x);
            self.plan.push(Segment {
                x: rx,
                spine: here.0,
                up: here.1,
                dn: here.2,
                t: here.3,
                cov: here.4.clamp(0.0, 255.0) as u8,
                glass_up: here.5 * tail_here,
                line_keep: here.6 * tail_here,
                glass_jump: jump,
            });
            // The boundary's right side: its walk past a hard step.
            let open = if jump == 0.0 {
                here
            } else {
                (
                    here.0,
                    here.1,
                    here.2,
                    here.3 + jump,
                    here.4,
                    here.5,
                    here.6,
                )
            };
            let continues =
                right.is_some() || (boundary == u32::from(col1) + 1 && right_nb.is_some());
            let onward = if floored.is_some() && continues {
                doubled = Some((boundary - u32::from(col0)) as usize);
                let seam = (open.0, open.1, open.2, open.3, natural, open.5, open.6);
                self.plan.push(Segment {
                    x: rx,
                    spine: seam.0,
                    up: seam.1,
                    dn: seam.2,
                    t: seam.3,
                    cov: seam.4.clamp(0.0, 255.0) as u8,
                    glass_up: seam.5 * tail_here,
                    line_keep: seam.6 * tail_here,
                    glass_jump: 0.0,
                });
                seam
            } else {
                open
            };
            prev = Some((x, onward));
            boundary += 1;
        }
        if attach
            && !under_caret_leaving
            && let Some((px, p)) = prev
            && let Some(af) = self.attach_floor
        {
            // Under a block the band crosses the whole cell the caret covers; behind a bar
            // (`head_dx` 0.08) it ends just past the bar, twice the insertion point across, and
            // lays no coloured block beside it (composed r1's judges; O4(c)).
            let x = px + cw * (2.0 * ctx.cfg.head_dx);
            let mut j = 1;
            while j <= slabs {
                let f = j as f32 * inv_slabs;
                let cov = af.level(self, ctx, f);
                let xf = px + (x - px) * f;
                self.plan.push(Segment {
                    x: retract.at(xf).round(),
                    spine: p.0,
                    up: p.1,
                    dn: p.2,
                    t: p.3,
                    cov: cov.clamp(0.0, 255.0) as u8,
                    glass_up: p.5 * tail_at(xf),
                    line_keep: p.6 * tail_at(xf),
                    glass_jump: 0.0,
                });
                j += 1;
            }
        }
        let hi = self.plan.len();
        if hi > lo {
            let head_boundary = usize::from(head_col - col0) + 1;
            let shift = usize::from(matches!(doubled, Some(d) if d < head_boundary));
            let head = if stream_dir > 0 {
                lo
            } else {
                (lo + head_boundary * slabs + shift).min(hi - 1)
            };
            self.runs.push(Run {
                row,
                col0,
                col1,
                lo,
                hi,
                head_col,
                head,
                wet,
                at_caret,
                born,
                stream_dir,
                hold,
            });
        }
    }

    #[inline]
    fn attach_floor_at(&self, ctx: &Ctx<'_>, row: u16, cell: &Cell, dist: f32) -> Option<f32> {
        let af = self.attach_floor?;
        if af.row == row && af.col == cell.col && cell.leaving() {
            Some(af.level(self, ctx, dist))
        } else {
            None
        }
    }

    /// Draw the body into `frame.under` and the hot edge into `frame.out`.
    #[inline]
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.ink_quads = 0;
        self.rail_spans.clear();
        self.glass_drew = None;
        self.glass_span = None;
        if self.plan.is_empty() || self.runs.is_empty() {
            return;
        }
        let ink_from = (frame.under.len(), frame.under_ramps.mark());
        // Under glass (§15.2 1.7) the body is the glass's: [`GLASS_BODY`] laid on its row's
        // bottom through the glass's wedge walk, under a whole ceiling, with none of the paper
        // spellings' treatments, and the comet's rail laid over it in the lip's DEEP ink
        // ([`BedInkLut::rail_at`]): the rainbow behind the typed words and the line under them.
        let glass = self.ink.glass_lit();
        // The frame's plan draws by the glass, so it is the glass arm's even where no run
        // reaches [`GLASS_MIN_PX`] and nothing is laid: main's paper would lay its body there.
        if glass {
            self.glass_drew = Some(ctx.now);
        }
        let prof = Self::body_profile(ctx.cfg);
        let shoulder = prof.shoulder;
        #[cfg(test)]
        let alike = self.paper_alike && !ctx.cfg.dark_theme;
        #[cfg(not(test))]
        let alike = false;
        let flat_dim = Self::flat_dim(ctx.cfg);
        #[cfg(test)]
        let rail_off = self.rail_off;
        #[cfg(not(test))]
        let rail_off = false;
        let rail = (Self::rail_lit(ctx.cfg) || flat_dim) && !alike && !rail_off;
        #[cfg(test)]
        let line_whole = self.line_whole;
        #[cfg(not(test))]
        let line_whole = false;
        #[cfg(test)]
        let rail_lerps_the_fold = self.rail_lerps_the_fold;
        #[cfg(not(test))]
        let rail_lerps_the_fold = false;
        let chf = ctx.geom.ch as f32;
        let ceiling = if glass {
            GLASS_ALPHA
        } else if ctx.cfg.dark_theme {
            BODY_FRAME_TOP
        } else {
            light_role(ctx.cfg).alpha_cap()
        };
        let fx_top = ctx.geom.fx_top() as f32;
        let clip = ctx.geom.beam_clip();
        let budget = frame.under_ramps.laid(frame.under.len()) + RIBBON_QUAD_BUDGET;
        let core_up = prof.core_up_ch * chf;
        let lift_span = LEAD_CH * chf;
        let box_top = self.rise_ceiling(ctx);
        let mut verts = mem::take(&mut self.verts);
        let runs = mem::take(&mut self.runs);
        let stride = ctx.geom.cw.max(1).div_ceil(self.slabs_per_cell());
        let glass_stride = ctx.geom.cw.max(1).div_ceil(self.glass_slabs.max(1));
        let mut focused_multi_run_row = None;
        let mut ri = 0;
        while ri < runs.len() {
            let run = &runs[ri];
            if run.at_caret {
                let row = run.row;
                let mut rj = 0;
                while rj < runs.len() {
                    if rj != ri && runs[rj].row == row {
                        focused_multi_run_row = Some(row);
                        break;
                    }
                    rj += 1;
                }
                break;
            }
            ri += 1;
        }
        let pane_bot = self.pane_bot(ctx, chf);
        let dn_floor = DN_FLOOR_CH * chf;
        let rail_reach_max = RAIL_REACH_MAX_CH * chf;
        #[cfg(test)]
        let rail_gap = if self.rail_flush {
            0.0
        } else if self.rail_gap_rounded {
            (RAIL_GAP_CH * chf).round().max(1.0)
        } else {
            rail_gap_px(chf)
        };
        #[cfg(not(test))]
        let rail_gap = rail_gap_px(chf);
        let rail_clear = rail_gap >= crate::cursor_glow::LETTER_GUARD_RING as f32;
        #[cfg(test)]
        let row_rail_off = self.row_rail_off;
        #[cfg(not(test))]
        let row_rail_off = false;
        #[cfg(test)]
        let row_rail_parted = self.row_rail_parted;
        #[cfg(not(test))]
        let row_rail_parted = false;
        #[cfg(test)]
        let held_rail_off = self.held_rail_off;
        #[cfg(not(test))]
        let held_rail_off = false;
        #[cfg(any(test, debug_assertions))]
        let feather_by_room = FEATHER_BY_ROOM.with(std::cell::Cell::get);
        #[cfg(any(test, debug_assertions))]
        let body_band_whole = BODY_BAND_WHOLE.with(std::cell::Cell::get);
        #[cfg(not(any(test, debug_assertions)))]
        let body_band_whole = false;
        let mut parted = mem::take(&mut self.parted);
        // A bar or an underline caret's cell, which the in-row rail of its row ends before.
        let caret_gap = if self.thin_caret {
            let x0 = f32::from(ctx.geom.origin_x) + f32::from(ctx.caret.1) * ctx.geom.cw as f32;
            Some((ctx.caret.0, x0, x0 + ctx.geom.cw as f32))
        } else {
            None
        };
        let row_rail = (ROW_RAIL_CH * chf).floor().max(1.0);
        // A slab spans to the next vertex at its own light: the in-row
        // rail keeps a slab more off a foot.
        let foot_clear = FOOT_CLEAR_PX + ctx.geom.cw as f32 / self.slabs_per_cell() as f32;
        let mut rail_spans = mem::take(&mut self.rail_spans);
        rail_spans.clear();
        let plan = &self.plan;
        let ink = &self.ink;
        let keep_out_ease = KEEP_OUT_EASE_CELLS * ctx.geom.cw as f32;
        // The flat ribbon on paper lies flatter than the pet's comet body:
        // lower, and a soft even band ([`FLAT_PAPER_GAIN`]).
        let flat_paper = !ctx.cfg.dark_theme && !Self::comet(ctx.cfg) && !alike && !glass;
        #[cfg(test)]
        let flat_even = flat_paper && !self.flat_paper_glow;
        #[cfg(not(test))]
        let flat_even = flat_paper;
        #[cfg(test)]
        let coarse = self.feather_coarse;
        let roomy = (self.plan.len() as f32) * FEATHER_ROOM_ROWS * chf
            < RIBBON_QUAD_BUDGET as f32 * FEATHER_ROOM_SHARE;
        // Each run's tail laid finely adds its fine windows to its plan's;
        // only a frame with room for a tail a row lays them (counted by
        // rows, so a row's runs lay alike together and apart).
        let roomy_ends = roomy && {
            let fine = stride.div_ceil(RUN_END_FINE_SPLIT).max(1);
            let end_windows = RUN_END_FINE_CELLS
                * (ctx.geom.cw as f32 / fine as f32 - self.slabs_per_cell() as f32);
            let mut tail_rows = 0usize;
            let mut i = 0;
            while i < runs.len() {
                let run = &runs[i];
                let mut seen_row = false;
                let mut j = 0;
                while j < i {
                    if runs[j].row == run.row {
                        seen_row = true;
                        break;
                    }
                    j += 1;
                }
                if !seen_row {
                    tail_rows += 1;
                }
                i += 1;
            }
            (self.plan.len() as f32 + tail_rows as f32 * end_windows.max(0.0))
                * FEATHER_ROOM_ROWS
                * chf
                < RIBBON_QUAD_BUDGET as f32 * RUN_END_ROOM_SHARE
        };
        // Where the glass a frame laid ends in the under stream: its rails follow it.
        let mut glass_to = None;
        {
            let mut paint = |out: &mut RampSink<'_>, run: &Run, layer: Lay| -> bool {
                let rail_layer = !matches!(layer, Lay::Body);
                // A span past the stream's end was cut away with what it
                // drew (the body-first repaint): it is spent.
                let at = out.mark();
                rail_spans.retain(|&(_, to, _)| rail_within(to, at));
                let row_bottom = f32::from(ctx.geom.origin_y) + (f32::from(run.row) + 1.0) * chf;
                // The rail lies in its own row's bottom band ([`ROW_RAIL_CH`]) wherever it
                // may not lie under the row, on a row whose glyphs the host named:
                // - over a filled cell, whole (the row's hold keeps the body off the row
                //   below, and this rail never leaves its row);
                // - on the pane's last row, whole (no row is under it);
                // - on a row held over letters, by its hold: the rail under the row dims as
                //   the row settles into itself and this one takes its light, so the row a
                //   person types on (a prompt over a full page's last row, a composer over
                //   its rule) keeps the vivid line under its words. Before, such a row laid
                //   its dim foot band and no rail.
                let row_top_i = (row_bottom - chf) as i32;
                let feet_known = rail
                    && !row_rail_off
                    && run.hi > run.lo
                    && self.keep_out.knows_feet((row_top_i, row_bottom as i32));
                let over_fill = feet_known && {
                    let (a, b) = (plan[run.lo].x, plan[run.hi - 1].x);
                    let below = row_bottom + rail_gap;
                    self.keep_out.fills(
                        (a.min(b).floor() as i32, a.max(b).ceil() as i32 + 1),
                        (below as i32, (below + rail_reach_max).ceil() as i32),
                    )
                };
                let floor_row = row_bottom >= pane_bot;
                let row_share = if !feet_known {
                    0.0
                } else if over_fill {
                    1.0
                } else if held_rail_off {
                    0.0
                } else if floor_row {
                    1.0
                } else {
                    run.hold
                };
                let in_row = matches!(layer, Lay::RowRail);
                let dark = match layer {
                    Lay::Body => false,
                    Lay::Rail => !rail || over_fill || run.hold >= 1.0 || floor_row,
                    Lay::RowRail => row_share <= 0.0,
                };
                if dark {
                    return true;
                }
                // A glass run nowhere [`GLASS_MIN_PX`] tall draws no glass: the glass is whole
                // or absent. Its line lays the share of its height the glass keeps
                // ([`Segment::line_keep`]), so it has faded with the glass by then and fades on
                // to none.
                if glass
                    && (!rail_layer || line_whole)
                    && !plan[run.lo..run.hi]
                        .iter()
                        .any(|s| s.glass_up >= GLASS_MIN_PX)
                {
                    return true;
                }
                let hold = if in_row { 1.0 - row_share } else { run.hold };
                // The face named the ink of this row's feet: the in-row rail parts round the
                // ink ([`part_row_rail`]), not round each foot's cell. Over a filled row it
                // keeps its dip under each foot's cell, as it was laid before this lay had a
                // row of its own to hold.
                let inked = in_row && !over_fill && self.keep_out.inks_row(row_top_i);
                // The glass's run ends where its height does (§15.2 1.8): no head feather.
                let head_feather_of = if glass && !rail_layer {
                    HeadFeather {
                        width: 0.0,
                        inv_width: 0.0,
                        ..self.head_feather(ctx, run)
                    }
                } else {
                    self.head_feather(ctx, run)
                };
                // The head feather is laid a pixel apart wherever the frame's estimate, with
                // this feather's own windows, stays within the budget, not only under
                // [`FEATHER_ROOM_SHARE`] of it. Laid at the plan's strides it is a stair of
                // flat slabs, and a line that flows into the fold carries its boundaries along
                // under the feather ([`RetractX`]): the stair then walks through the feather's
                // 1.2 cells and every pixel there steps up and down 20 times a second for the
                // flow's 1.5 s. A line of 50 cells or more was over the share.
                let fine = roomy
                    || (self.plan.len() as f32 + head_feather_of.width / FEATHER_FINE_PX)
                        * FEATHER_ROOM_ROWS
                        * chf
                        < RIBBON_QUAD_BUDGET as f32;
                #[cfg(test)]
                let fine = fine && !coarse;
                #[cfg(any(test, debug_assertions))]
                let fine = if feather_by_room { roomy } else { fine };
                let fine_feather = if fine {
                    head_feather_of
                } else {
                    HeadFeather {
                        width: 0.0,
                        inv_width: 0.0,
                        ..head_feather_of
                    }
                };
                verts.clear();
                verts.reserve(run.hi - run.lo);
                let mut head_rev = 0usize;
                let head_rel = Some(run.head - run.lo);
                #[cfg(test)]
                let stair = self.tail_stair;
                #[cfg(not(test))]
                let stair = false;
                // The glass's oldest end is its height's ease ([`GlassTail`]), laid at the
                // plan's vertices: no fine run end.
                let end_fine = if roomy_ends && !stair && !(glass && !rail_layer) {
                    (
                        RUN_END_FINE_CELLS * ctx.geom.cw as f32,
                        stride.div_ceil(RUN_END_FINE_SPLIT) as f32,
                    )
                } else {
                    (0.0, 0.0)
                };
                // A bar or an underline caret on this row: what lies in the row's foot band ends
                // before its cell.
                let gap = match caret_gap {
                    Some((row, x0, x1)) if row == run.row => Some((x0, x1)),
                    _ => None,
                };
                if rail_layer {
                    let gap = if in_row { gap } else { None };
                    // The rail is the leading's line ([`InkRole::Leading`]): on paper it lays the
                    // body's share of the body's ceiling at the leading's own, so the line under
                    // the tall body's over-text wash is the line under the underline's body.
                    let rail_cap = if glass || ctx.cfg.dark_theme {
                        1.0
                    } else {
                        InkRole::Leading.alpha_cap() / cov_cap(ctx.cfg)
                    };
                    let rail_scale = RAIL_GAIN * (1.0 - hold) * rail_cap;
                    let flat_dim_scale = FLAT_DIM_RAIL_GAIN * (1.0 - hold);
                    let rail_core = if flat_dim {
                        FLAT_DIM_RAIL_CORE
                    } else {
                        RAIL_CORE_SHARE
                    };
                    // Beside a foot the in-row rail dips under the row, into
                    // the row below's blank top, as far as it would have
                    // parted ([`ROW_RAIL_DIP_GAP_PX`]), a pixel thinner there
                    // ([`ROW_RAIL_DIP_THIN_PX`]): it passes over a short
                    // glyph's cell (its ink a third of the cell down), so a
                    // letter beside the foot's cell below no longer breaks it.
                    let dip = row_rail + ROW_RAIL_DIP_GAP_PX;
                    let dip_rail = (row_rail - ROW_RAIL_DIP_THIN_PX).max(1.0);
                    let row_bottom_i = row_bottom as i32;
                    let dip_bottom = (row_bottom
                        + ROW_RAIL_DIP_GAP_PX
                        + dip_rail
                        + crate::cursor_glow::LETTER_GUARD_RING as f32)
                        .ceil() as i32;
                    let dips = |x: f32| {
                        !row_rail_parted
                            && self.keep_out.letters_meet(
                                (
                                    (x - foot_clear).floor() as i32,
                                    (x + foot_clear).ceil() as i32 + 1,
                                ),
                                (row_bottom_i, dip_bottom),
                            ) == Some(false)
                    };
                    let mut prev_t = None;
                    for_each_feathered(&plan[run.lo..run.hi], fine_feather, end_fine, |i, seg| {
                        // The vivid rail lies a hairline below the row's bottom.
                        let top = if in_row {
                            row_bottom - row_rail
                        } else {
                            (row_bottom + rail_gap).max(seg.spine)
                        };
                        let (top, foot, dipped) = if in_row && !inked {
                            let share = self.keep_out.foot_share(
                                (seg.x, row_bottom - 1.0),
                                foot_clear,
                                FOOT_EASE_PX,
                            );
                            if share < 1.0 && dips(seg.x) {
                                (top + (1.0 - share) * dip, 1.0, 1.0 - share)
                            } else {
                                (top, share, 0.0)
                            }
                        } else {
                            (top, 1.0, 0.0)
                        };
                        let dn = if in_row {
                            row_rail + (dip_rail - row_rail) * dipped
                        } else if flat_dim {
                            rail_reach_max
                        } else {
                            (seg.spine + seg.dn - top).max(dn_floor).min(rail_reach_max)
                        };
                        // The line lays the share of its height the body keeps
                        // ([`Segment::line_keep`]): the body's coverage is its opacity alone.
                        let keep = if line_whole { 1.0 } else { seg.line_keep };
                        let feather_cov = seg.cov * head_feather_of.at(seg.x) * keep;
                        let v = RibbonVertex {
                            x: seg.x,
                            spine: top,
                            up: 0.0,
                            dn,
                            core_up: 0.0,
                            core_dn: dn * rail_core,
                            color: ink.rail_at(seg.t),
                            // The flat rail on a dim page holds about half the
                            // pet's peak: its coverage is capped at that share
                            // of whole, since the body's own runs past it.
                            cov: if flat_dim {
                                feather_cov.min(255.0) * flat_dim_scale
                            } else {
                                feather_cov * rail_scale
                            } * foot,
                            lift: 0.0,
                            lift_span: 0.0,
                        };
                        // A glass hard step (§2.2 Seams) is two vertices at its x, its right side's
                        // walk first, as the glass lays it.
                        let right = if seg.glass_jump == 0.0 {
                            None
                        } else {
                            Some(RibbonVertex {
                                color: ink.rail_at(seg.t + seg.glass_jump),
                                ..v
                            })
                        };
                        // Where the lip's ink steps between this vertex and the last
                        // ([`BedInkLut::rail_step`]), two vertices at the step, one in each
                        // side's entry, laid as the run is: nothing lerps across it.
                        let near_t = seg.t + seg.glass_jump;
                        if let (Some(pt), Some(&p)) = (prev_t, verts.last())
                            && !rail_lerps_the_fold
                            && let Some((u, from, to)) = ink.rail_step(pt, near_t)
                        {
                            let mix = |a: f32, b: f32| a + (b - a) * u;
                            let at = |color: u32| RibbonVertex {
                                x: mix(p.x, v.x),
                                spine: mix(p.spine, v.spine),
                                dn: mix(p.dn, v.dn),
                                core_dn: mix(p.core_dn, v.core_dn),
                                cov: mix(p.cov, v.cov),
                                color,
                                ..v
                            };
                            verts.push(at(from));
                            verts.push(at(to));
                        }
                        if i == head_rel {
                            head_rev = verts.len();
                        }
                        if let Some(right) = right {
                            verts.push(right);
                        }
                        verts.push(v);
                        prev_t = Some(seg.t);
                    });
                    if inked || gap.is_some() {
                        let ring = crate::cursor_glow::LETTER_GUARD_RING as f32;
                        let clear = FOOT_CLEAR_PX;
                        let reach = clear + ROW_RAIL_INK_EASE_PX;
                        let caret_reach = CARET_CLEAR_PX + CARET_EASE_PX;
                        let keep_out = &self.keep_out;
                        head_rev = part_row_rail(
                            &mut verts,
                            &mut parted,
                            head_rev,
                            |lo, hi| {
                                (inked && keep_out.ink_near((lo, hi), row_top_i, reach))
                                    || matches!(gap, Some((x0, x1))
                                        if x0 - caret_reach <= hi && lo <= x1 + caret_reach)
                            },
                            |mut v| {
                                let g = keep_out.foot_gap(
                                    v.x,
                                    row_top_i,
                                    (clear, ROW_RAIL_INK_EASE_PX),
                                );
                                // The ink's ring ends over the band's top: nothing to leave.
                                let lane = g.end as f32 + ring;
                                if inked && g.keep < 1.0 && lane > v.spine {
                                    // Under the ink and its ring, over the ink below and its
                                    // ring and inside the pane, the rail runs on, whole
                                    // ([`ROW_RAIL_DIP_GAP_PX`]); with no such room (the
                                    // pane's last row, a tall letter below) it parts round
                                    // the ink, as a typeset underline skips a descender.
                                    let room = (g.below as f32 - ring).min(pane_bot) - lane;
                                    if room >= dip_rail.min(2.0) && !row_rail_parted {
                                        let depth = room.min(row_rail);
                                        let core = if v.dn > 0.0 { v.core_dn / v.dn } else { 0.0 };
                                        v.spine = lane + (v.spine - lane) * g.keep;
                                        v.dn = depth + (v.dn - depth) * g.keep;
                                        v.core_dn = v.dn * core;
                                    } else {
                                        v.cov *= g.keep;
                                    }
                                }
                                if let Some((x0, x1)) = gap {
                                    let d = (x0 - v.x).max(v.x - x1) - CARET_CLEAR_PX;
                                    v.cov *= smoothstep01(d * (1.0 / CARET_EASE_PX));
                                }
                                v
                            },
                        );
                    }
                } else if glass {
                    // The glass stands on its row's bottom with no reach below it, as tall as
                    // the plan's glass reach where the row's keep-outs and the box's rise
                    // ceiling let it be.
                    let row_top = f32::from(ctx.geom.origin_y) + f32::from(run.row) * chf;
                    let has_ceiling = self.keep_out.has_ceiling_at(row_top);
                    let spine = row_bottom.min(pane_bot).max(fx_top);
                    for_each_feathered(&plan[run.lo..run.hi], fine_feather, end_fine, |i, seg| {
                        if i == head_rel {
                            head_rev = verts.len();
                        }
                        // Whole opacity, its edge the melt's: no strip lift (§15.2 1.8).
                        let cov = seg.cov * head_feather_of.at(seg.x);
                        let up = self.kept_up(
                            (seg.x, spine, seg.glass_up),
                            row_top,
                            keep_out_ease,
                            (has_ceiling, box_top),
                        );
                        let vertex = |t: f32| RibbonVertex {
                            x: seg.x,
                            spine,
                            up,
                            dn: 0.0,
                            core_up: GLASS_CORE_SHARE * up,
                            core_dn: 0.0,
                            color: ink.at(t),
                            cov,
                            lift: 0.0,
                            lift_span: 0.0,
                        };
                        // A hard step is two vertices at one x, laid right side first as the
                        // run is: nothing lerps between them.
                        if seg.glass_jump != 0.0 {
                            verts.push(vertex(seg.t + seg.glass_jump));
                        }
                        verts.push(vertex(seg.t));
                    });
                } else {
                    let row_top = f32::from(ctx.geom.origin_y) + f32::from(run.row) * chf;
                    let has_ceiling = self.keep_out.has_ceiling_at(row_top);
                    for_each_feathered(&plan[run.lo..run.hi], fine_feather, end_fine, |i, seg| {
                        if i == head_rel {
                            head_rev = verts.len();
                        }
                        let (cov, strip) = if flat_even {
                            (seg.cov * FLAT_PAPER_GAIN * head_feather_of.at(seg.x), 0.0)
                        } else {
                            let cov = seg.cov * head_feather_of.at(seg.x);
                            (cov, (cov * STRIP_LIFT_GAIN).min(ceiling - cov).max(0.0))
                        };
                        let up = self.kept_up(
                            (seg.x, seg.spine, seg.up),
                            row_top,
                            keep_out_ease,
                            (has_ceiling, box_top),
                        );
                        let (up, core) = if flat_even {
                            (up * FLAT_PAPER_UP_SHARE, 0.0)
                        } else if flat_paper {
                            let up = up * FLAT_PAPER_UP_SHARE;
                            (up, up * FLAT_PAPER_CORE_SHARE)
                        } else {
                            (up, core_up.min(up))
                        };
                        verts.push(RibbonVertex {
                            x: seg.x,
                            spine: seg.spine,
                            up,
                            dn: seg.dn,
                            core_up: core,
                            core_dn: 0.0,
                            color: ink.at(seg.t),
                            cov,
                            lift: strip,
                            // The lift's bump ends where the body does under
                            // the box's top, its top soft there too.
                            lift_span: lift_span.min((seg.spine - box_top).max(0.0)),
                        });
                    });
                    // The underline's body on a row that keeps its line in its own foot band
                    // is a band there too ([`UNDERLINE_UP_CH`]), under the rail: it parts
                    // where the rail does, by the row's share, round the ink of a foot the
                    // face named and before a thin caret. Left whole, it lay in the gaps the
                    // rail leaves (a bottom prompt's `q` on GitHub Light stood in a mint band
                    // that touched its ink, and the band ran on to the bar caret). The tall
                    // body is a wash behind its row's letters, not a band under them, and a
                    // filled row's rail keeps its cell-wide parting: both are left as laid.
                    let share = if prof.tall || over_fill || body_band_whole {
                        0.0
                    } else {
                        row_share
                    };
                    let inked = share > 0.0 && self.keep_out.inks_row(row_top_i);
                    let gap = if share > 0.0 { gap } else { None };
                    if inked || gap.is_some() {
                        let reach = FOOT_CLEAR_PX + ROW_RAIL_INK_EASE_PX;
                        let caret_reach = CARET_CLEAR_PX + CARET_EASE_PX;
                        let keep_out = &self.keep_out;
                        head_rev = part_row_rail(
                            &mut verts,
                            &mut parted,
                            head_rev,
                            |lo, hi| {
                                (inked && keep_out.ink_near((lo, hi), row_top_i, reach))
                                    || matches!(gap, Some((x0, x1))
                                        if x0 - caret_reach <= hi && lo <= x1 + caret_reach)
                            },
                            |mut v| {
                                let mut keep = if inked {
                                    keep_out
                                        .foot_gap(
                                            v.x,
                                            row_top_i,
                                            (FOOT_CLEAR_PX, ROW_RAIL_INK_EASE_PX),
                                        )
                                        .keep
                                } else {
                                    1.0
                                };
                                if let Some((x0, x1)) = gap {
                                    let d = (x0 - v.x).max(v.x - x1) - CARET_CLEAR_PX;
                                    keep *= smoothstep01(d * (1.0 / CARET_EASE_PX));
                                }
                                let keep = 1.0 - share * (1.0 - keep);
                                v.cov *= keep;
                                v.lift *= keep;
                                v
                            },
                        );
                    }
                }
                let start = out.mark();
                let lay = if rail_layer {
                    BodyLay::Beam(1.0)
                } else if glass {
                    BodyLay::Glass
                } else {
                    BodyLay::Beam(shoulder)
                };
                let stride = if matches!(lay, BodyLay::Glass) {
                    glass_stride
                } else {
                    stride
                };
                let mut complete = lay.lay(out, clip, &verts, stride, budget);
                let head = verts.len().saturating_sub(1 + head_rev);
                if !complete {
                    out.truncate(start);
                    verts.reverse();
                    complete = Self::repaint_capped_run_from_head(
                        out,
                        clip,
                        &mut verts,
                        head,
                        lay,
                        (stride, budget),
                        BUDGET_FEATHER_CELLS as f32 * ctx.geom.cw as f32,
                    );
                }
                let end = out.mark();
                if rail_layer
                    && (rail_clear || in_row)
                    && (end.0 > start.0 || end.1.items() > start.1.items())
                {
                    rail_spans.push((start, end, in_row));
                }
                complete
            };
            let mut retry_body_first = false;
            let mut prior_rail_drawn = false;
            let mut sink = frame.under_sink();
            let under = &mut sink;
            // Under glass every run's glass is laid first and every run's rail after it. The
            // glass is opaque: a rail laid before a later run's glass would lie under it, as the
            // line under a wrapped row's words would under the next row's glass. And the span the
            // yield owns ([`Self::glass_span`]) is then the glass's alone, the rails the guard's.
            #[cfg(test)]
            let glass_first = glass && !self.glass_rails_by_run;
            #[cfg(not(test))]
            let glass_first = glass;
            let mut i = if glass_first {
                let mut whole = true;
                let mut gi = 0;
                while gi < runs.len() {
                    if !paint(under, &runs[gi], Lay::Body) {
                        whole = false;
                        break;
                    }
                    gi += 1;
                }
                glass_to = Some(under.mark());
                let mut ri = 0;
                while whole && ri < runs.len() {
                    let run = &runs[ri];
                    if !(paint(under, run, Lay::Rail) && paint(under, run, Lay::RowRail)) {
                        break;
                    }
                    ri += 1;
                }
                runs.len()
            } else {
                0
            };
            while i < runs.len() {
                let run = &runs[i];
                if !paint(under, run, Lay::Body) {
                    retry_body_first = prior_rail_drawn && focused_multi_run_row == Some(run.row);
                    break;
                }
                let rail_from = under.laid();
                if !(paint(under, run, Lay::Rail) && paint(under, run, Lay::RowRail)) {
                    retry_body_first = if let Some(row) = focused_multi_run_row {
                        let mut later_on_row = false;
                        let mut li = i + 1;
                        while li < runs.len() {
                            if runs[li].row == row {
                                later_on_row = true;
                                break;
                            }
                            li += 1;
                        }
                        later_on_row
                    } else {
                        false
                    };
                    break;
                }
                prior_rail_drawn |= under.laid() > rail_from;
                i += 1;
            }
            if retry_body_first {
                let row = focused_multi_run_row.expect("retry requires a multi-run focused row");
                under.truncate(ink_from);
                let mut bodies_complete = true;
                let mut bi = 0;
                while bi < runs.len() {
                    let run = &runs[bi];
                    if run.row == row && !paint(under, run, Lay::Body) {
                        bodies_complete = false;
                        break;
                    }
                    bi += 1;
                }
                if bodies_complete {
                    let mut rails_complete = true;
                    let mut ri = 0;
                    while ri < runs.len() {
                        let run = &runs[ri];
                        if run.row == row
                            && !(paint(under, run, Lay::Rail) && paint(under, run, Lay::RowRail))
                        {
                            rails_complete = false;
                            break;
                        }
                        ri += 1;
                    }
                    if rails_complete {
                        let mut oi = 0;
                        while oi < runs.len() {
                            let run = &runs[oi];
                            if run.row != row
                                && !(paint(under, run, Lay::Body)
                                    && paint(under, run, Lay::Rail)
                                    && paint(under, run, Lay::RowRail))
                            {
                                break;
                            }
                            oi += 1;
                        }
                    }
                }
            }
        }
        let at = (frame.under.len(), frame.under_ramps.mark());
        if let Some(to) = glass_to
            && to != ink_from
        {
            self.glass_span = Some((ink_from, to));
        }
        rail_spans.retain(|&(_, to, _)| rail_within(to, at));
        self.rail_spans = rail_spans;
        self.runs = runs;
        self.verts = verts;
        self.parted = parted;
        self.emit_hot_edge(ctx, frame);
        #[inline]
        const fn end_reads_as_ink(
            bg: u32,
            last_ink: &mut Option<(u32, u8, bool)>,
            color: u32,
            alpha: u8,
        ) -> bool {
            if let Some((prev_c, prev_a, res)) = *last_ink
                && prev_c == color
                && prev_a == alpha
            {
                return res;
            }
            let res = reads_as_ink(over_premul(bg, color, alpha));
            *last_ink = Some((color, alpha, res));
            res
        }
        let bg = ctx.cfg.theme_bg;
        let ramps = &*frame.under_ramps;
        let mut last_ink: Option<(u32, u8, bool)> = None;
        let mut quads = 0usize;
        let under_slice = &frame.under[ink_from.0..];
        let mut qi = 0;
        while qi < under_slice.len() {
            let q = &under_slice[qi];
            if end_reads_as_ink(bg, &mut last_ink, q.color, q.alpha) {
                quads += 1;
            }
            qi += 1;
        }
        let mut rows = 0usize;
        let since_rows = ramps.since(ink_from.1);
        let mut ri = 0;
        while ri < since_rows.len() {
            let ends = since_rows[ri].kept_ends(&ramps.ends);
            let mut ei = 0;
            while ei + 1 < ends.len() {
                let (e0, e1) = (ends[ei], ends[ei + 1]);
                if e0 | e1 != 0
                    && end_reads_as_ink(
                        bg,
                        &mut last_ink,
                        aterm_core::render::end_color(e0),
                        aterm_core::render::end_alpha(e0),
                    )
                {
                    rows += 1;
                }
                ei += 2;
            }
            ri += 1;
        }
        self.ink_quads = quads + rows;
    }

    /// The reach `up` of a body boundary at window x `x` standing on
    /// `spine` in `row`, cut so its top (and the feather it wears there)
    /// stays in its own row under a [`KeepOut`] box standing on the row,
    /// and under `box_top`, the effects box's rise ceiling
    /// ([`Self::rise_ceiling`]): the whole profile laid in the room under
    /// it, where the box's clip would cut its soft top flat.
    ///
    /// [`Self::kept_up`] with the top held `inset` px inside the row under
    /// a box: a line drawn centred on the top (the hot edge) keeps its
    /// whole thickness off the box.
    #[inline]
    const fn kept_up(
        &self,
        (x, spine, up): (f32, f32, f32),
        row_top: f32,
        ease: f32,
        (has_ceiling, box_top): (bool, f32),
    ) -> f32 {
        let room = (spine - box_top).max(0.0);
        if !has_ceiling {
            return up.min(room);
        }
        let top = spine - up;
        if top >= row_top {
            return up.min(room);
        }
        let ceiling = self.keep_out.ceiling(x, row_top, top, ease);
        (spine - ceiling.max(top)).max(0.0).min(room)
    }

    /// The effects box's rise ceiling for this frame
    /// ([`crate::cursor_glow::Geom::rise_ceiling`]; none under the negative
    /// control, where the box's clip cuts the body).
    #[inline]
    const fn rise_ceiling(&self, ctx: &Ctx<'_>) -> f32 {
        #[cfg(any(test, debug_assertions))]
        if self.rise_cut {
            return f32::NEG_INFINITY;
        }
        ctx.geom.rise_ceiling()
    }

    #[inline]
    const fn head_feather(&self, ctx: &Ctx<'_>, run: &Run) -> HeadFeather {
        let plan = self.plan.as_slice();
        let head_x = if run.head < plan.len() {
            plan[run.head].x
        } else {
            0.0
        };
        let past = if run.stream_dir < 0 {
            run.head_col.checked_add(1)
        } else {
            run.head_col.checked_sub(1)
        };
        let meets = match past {
            Some(col) => self.index.at(run.row, col).is_some(),
            None => false,
        };
        let at_end = (run.head == run.lo || run.head + 1 == run.hi) && !meets;
        let at_end = at_end && self.curtain.is_none();
        #[cfg(test)]
        if self.head_square {
            return HeadFeather {
                head_x,
                width: 0.0,
                inv_width: 0.0,
            };
        }
        let (width, inv_width) = if !run.at_caret && at_end {
            let w = HEAD_FEATHER_CELLS * ctx.geom.cw as f32;
            (w, if w > 0.0 { 1.0 / w } else { 0.0 })
        } else {
            (0.0, 0.0)
        };
        HeadFeather {
            head_x,
            width,
            inv_width,
        }
    }

    #[inline]
    fn repaint_capped_run_from_head(
        out: &mut RampSink<'_>,
        clip: BeamClip,
        verts: &mut [RibbonVertex],
        head: usize,
        lay: BodyLay,
        priced: (usize, usize),
        feather_px: f32,
    ) -> bool {
        debug_assert!(head < verts.len());
        verts[..=head].reverse();
        let left_complete =
            Self::lay_capped(out, clip, &mut verts[..=head], lay, priced, feather_px);
        verts[..=head].reverse();
        left_complete && Self::lay_capped(out, clip, &mut verts[head..], lay, priced, feather_px)
    }

    /// Lays `verts` in order, or as many as `priced` (the stride and the budget) carries whole, the
    /// last of them thinned over `feather_px` to [`RUN_TAIL_EASE`] so the cut reads as the band
    /// thinning away. `false` where it cut. Each segment is laid to learn the price, as the sink
    /// folds rows; thinning can only drop a row, so what fit still fits.
    #[inline]
    fn lay_capped(
        out: &mut RampSink<'_>,
        clip: BeamClip,
        verts: &mut [RibbonVertex],
        lay: BodyLay,
        (stride, budget): (usize, usize),
        feather_px: f32,
    ) -> bool {
        let start = out.mark();
        let mut fit = 1;
        while fit < verts.len() && lay.lay(out, clip, &verts[fit - 1..=fit], stride, budget) {
            fit += 1;
        }
        if fit >= verts.len() {
            return true;
        }
        out.truncate(start);
        Self::thin_toward_cut(&mut verts[..fit], feather_px);
        let _ = lay.lay(out, clip, &verts[..fit], stride, budget);
        false
    }

    /// Thins `verts` toward its last, the cut, over `feather_px` or the whole span where that is
    /// shorter: its first stays whole, since a run's head is shared with the half laid before it.
    #[inline]
    fn thin_toward_cut(verts: &mut [RibbonVertex], feather_px: f32) {
        let (Some(first), Some(last)) = (verts.first(), verts.last()) else {
            return;
        };
        let cut = last.x;
        let feather = feather_px.min((first.x - cut).abs()).max(1.0);
        for vertex in verts {
            let inward = ((vertex.x - cut).abs() / feather).min(1.0);
            let share = RUN_TAIL_EASE + (1.0 - RUN_TAIL_EASE) * inward;
            vertex.cov *= share;
            vertex.lift *= share;
        }
    }

    /// How many of the last frame's ribbon quads read as band ink on the glass.
    #[inline]
    #[must_use]
    pub const fn ink_quad_count(&self) -> usize {
        self.ink_quads
    }

    /// Whether the frame at `now` drew its plan by the bed's glass (§15.2 1.5, 1.7): Prism
    /// Glass's `glass` arm, which draws into the under stream's quads and ramps. A lit plan is
    /// the arm's even when none of its runs reaches [`GLASS_MIN_PX`] and the frame lays nothing,
    /// since main's paper lays its body there; a frame with no plan, or the bed on main's table,
    /// is not.
    #[inline]
    #[must_use]
    pub fn glass_drew_at(&self, now: Instant) -> bool {
        self.glass_drew == Some(now)
    }

    /// The instant of the last frame whose plan drew by the bed's glass ([`Self::glass_drew_at`]):
    /// `None` on every frame whose bed draws by main's table, for the host's dark pin. Debug and
    /// test builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn glass_drew(&self) -> Option<Instant> {
        self.glass_drew
    }

    /// How many light pages' palettes the ribbon holds, solved or solving
    /// ([`crate::rainbow_kitty::prism_glass::PaletteCache`]): none while no light page lit a
    /// frame, for the host's dark pin. Debug and test builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub fn palettes_held(&self) -> usize {
        self.palettes.len() + usize::from(self.palettes.pending().is_some())
    }

    /// The glass's height in pixels a `ch` the last plan gave the ribbon (§15.2 1.7): zero on
    /// every frame whose bed draws by main's table, for the host's dark pin. Debug and test
    /// builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn glass_px(&self) -> f32 {
        self.glass_px
    }

    /// Whether the last frame's plan holds the glass's geometry (§15.2 1.7): a boundary with a
    /// glass reach ([`Segment::glass_up`]) or a hard step ([`Segment::glass_jump`]). Never on a
    /// frame the bed draws by main's table. Debug and test builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub fn glass_planned(&self) -> bool {
        self.plan
            .iter()
            .any(|s| s.glass_up != 0.0 || s.glass_jump != 0.0)
    }

    /// The light words (§15.0.2(a)): the bed's glass table and the palettes it draws by, and the
    /// wrap's side table and the hand's row of a multi-row set (§15.2 1.10). Never compared.
    /// Debug and test builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn light_words(&self, w: &mut impl crate::state_words::Words) {
        use crate::state_words::Words as _;
        self.ink.light_words(w);
        self.palettes.light_words(w);
        if !self.glass_wrap.is_empty() || self.wrap_hand.is_some() {
            w.list("glass.wrap", &self.glass_wrap, |f, r: &GlassWrap| {
                f.put("cohort", u64::from(r.cohort));
                f.put("row", u64::from(r.row));
                f.at("left_at", r.left_at);
            });
            w.u16_opt("glass.wrap_hand", self.wrap_hand);
        }
    }

    /// The hot edge's gain.
    #[inline]
    #[must_use]
    pub const fn hot_edge_gain(cfg: &Config, disp: f32, typed: bool) -> f32 {
        if !cfg.dark_theme || cfg.reduced_motion {
            return 0.0;
        }
        let floor = if typed { HOT_EDGE_GAIN_FLOOR } else { 0.0 };
        floor
            + (1.0 - floor)
                * smoothstep01((clamp01(disp) - HOT_EDGE_DISP_MIN) * (1.0 / HOT_EDGE_DISP_SPAN))
    }

    #[inline]
    fn emit_hot_edge(&self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        if !ctx.cfg.dark_theme || ctx.cfg.reduced_motion {
            return;
        }
        let Some(run) = self.runs.first() else {
            return;
        };
        #[cfg(not(test))]
        let left_lit = false;
        #[cfg(test)]
        let left_lit = self.hot_edge_left_lit;
        if !run.wet || !(run.at_caret || left_lit) {
            return;
        }
        let mut head_cell: Option<&Cell> = None;
        let cells = self.cells.as_slice();
        let mut ci = 0;
        while ci < cells.len() {
            let c = &cells[ci];
            if c.row == run.row
                && c.col == run.head_col
                && !matches!(head_cell, Some(prev) if c.born < prev.born)
            {
                head_cell = Some(c);
            }
            ci += 1;
        }
        let Some(head_cell) = head_cell else {
            return;
        };
        let base_gain = Self::hot_edge_gain(ctx.cfg, head_cell.birth_disp, head_cell.typing);
        if base_gain <= 0.0 {
            return;
        }
        let gain = base_gain * self.env_of(ctx, head_cell);
        if gain <= 0.0 {
            return;
        }
        let mouth_x = self.plan[run.head].x;
        let dir = f32::from(run.stream_dir);
        let cells = head_cell
            .edge_cells
            .clamp(HOT_EDGE_CELLS, HOT_EDGE_CELLS_MAX);
        let cw = ctx.geom.cw as f32;
        let seam = if let Some(s) = self.seam
            && s.row == run.row
            && run.at_caret
            && dir < 0.0
            && let Some(floor_col) = self.hand_floor_on(run.row)
        {
            let floor_x = f32::from(ctx.geom.origin_x) + f32::from(floor_col) * cw;
            let lvl = SEAM_LEVEL
                * spend(clamp01(
                    ctx.now.saturating_duration_since(s.at).as_secs_f32() * (1.0 / FLOW_TOTAL_S),
                ));
            if lvl > 0.0 {
                Some((floor_x, lvl))
            } else {
                None
            }
        } else {
            None
        };
        let reach = cells * cw;
        let seam_lvl = match seam {
            Some((_, l)) => l,
            None => 0.0,
        };
        let intensity = clamp01(ctx.cfg.intensity);
        let gain_intensity = gain * intensity;
        let feather = self.head_feather(ctx, run);
        // The edge is a line a pixel thick centred on the body's top: held at
        // the row's top under a lettered or filled cell above, its upper half
        // would lie on the cell's foot, so it is held half its thickness in.
        #[cfg(test)]
        let inset = if self.hot_edge_on_bar {
            0.0
        } else {
            0.5 * HOT_EDGE_THICKNESS
        };
        #[cfg(not(test))]
        let inset = 0.5 * HOT_EDGE_THICKNESS;
        let inv_reach = 1.0 / reach;
        let neg_dir = -dir;
        let row_top =
            f32::from(ctx.geom.origin_y) + f32::from(run.row) * (ctx.geom.ch as f32) + inset;
        let keep_out_ease = KEEP_OUT_EASE_CELLS * cw;
        let has_ceiling = self.keep_out.has_ceiling_at(row_top);
        let box_top = self.rise_ceiling(ctx) + inset;
        frame.beams.clear();
        let step = self.slabs_per_cell();
        let mut idx = run.lo;
        while idx < run.hi {
            let seg = &self.plan[idx];
            idx += step;
            let behind = (mouth_x - seg.x) * neg_dir;
            let in_seam = matches!(seam, Some((floor_x, _)) if behind >= 0.0 && seg.x >= floor_x);
            if !(behind >= 0.0 && behind <= reach) && !in_seam {
                continue;
            }
            let a_rel = {
                let t = (1.0 - behind * inv_reach).max(0.0);
                (t * t).max(if in_seam { seam_lvl } else { 0.0 })
            };
            let cov = (HOT_EDGE_COV_MAX * a_rel * gain_intensity * feather.at(seg.x))
                .clamp(0.0, HOT_EDGE_COV_MAX);
            frame.beams.push(BeamVertex {
                x: seg.x,
                y: seg.spine
                    - self.kept_up(
                        (seg.x, seg.spine, seg.up),
                        row_top,
                        keep_out_ease,
                        (has_ceiling, box_top),
                    ),
                color: self.ink.hot_at(seg.t),
                cov: cov as u8,
            });
        }
        if frame.beams.len() < 2 {
            frame.beams.clear();
            return;
        }
        frame.lay_beam(ctx.geom.beam_clip(), HOT_EDGE_THICKNESS, 1);
        frame.beams.clear();
    }

    /// The field at a cell, or `None`.
    #[inline]
    #[must_use]
    pub const fn field_at(&self, row: u16, col: u16) -> Option<f32> {
        self.index.at(row, col)
    }

    /// The walk at a cell.
    #[inline]
    #[must_use]
    pub const fn walk_origin_at(&self, row: u16, col: u16) -> Option<(f32, f32)> {
        let cells = self.cells.as_slice();
        let mut i = cells.len();
        let mut owner_cohort = None;
        while i > 0 {
            i -= 1;
            let c = &cells[i];
            if c.row == row && c.col == col {
                owner_cohort = Some(c.cohort);
                break;
            }
        }
        let Some(cohort_id) = owner_cohort else {
            return None;
        };
        let cohorts = self.cohorts.as_slice();
        let mut j = 0;
        while j < cohorts.len() {
            let c = &cohorts[j];
            if c.id == cohort_id {
                return Some((c.walk_d(col), c.walk_zero()));
            }
            j += 1;
        }
        None
    }

    /// The caret's position in the field (seam point 4).
    #[inline]
    #[must_use]
    pub fn field_at_caret(&self) -> f32 {
        if self.cells.is_empty() {
            return 0.0;
        }
        if let Some((row, col)) = self.caret
            && let Some(t) = self.field_at(row, col)
        {
            return t;
        }
        let mut best_on_row: Option<(Instant, f32)> = None;
        let mut best_any: Option<(Instant, f32)> = None;
        let mut i = 0;
        while i < self.cells.len() {
            let c = &self.cells[i];
            i += 1;
            if !c.typing || c.leaving() {
                continue;
            }
            if matches!(self.caret, Some((row, _)) if row == c.row)
                && !matches!(best_on_row, Some((b, _)) if c.born < b)
            {
                best_on_row = Some((c.born, c.t));
            }
            if !matches!(best_any, Some((b, _)) if c.born < b) {
                best_any = Some((c.born, c.t));
            }
        }
        match (best_on_row, best_any) {
            (Some((_, t)), _) | (None, Some((_, t))) => t,
            (None, None) => 0.0,
        }
    }

    /// The source RGB a companion cursor inherits to meet the laid ribbon with no palette seam (seam point 5).
    #[inline]
    #[must_use]
    pub fn head_rgb(&self, cfg: &Config) -> Option<u32> {
        self.caret?;
        Some(caret_ink(spectrum(walk_arc(self.field_at_caret())), cfg))
    }

    /// The body's vertical extent at a cell, or `None` where no cell is laid (§4.2, §5.4, D16).
    #[inline]
    #[must_use]
    pub const fn band(&self, row: u16, col: u16) -> Option<Band> {
        let runs = self.runs.as_slice();
        let mut i = 0;
        let mut found = None;
        while i < runs.len() {
            let r = &runs[i];
            if r.row == row && r.col0 <= col && col <= r.col1 {
                found = Some(r);
                break;
            }
            i += 1;
        }
        let Some(run) = found else {
            return None;
        };
        let slabs = self.slabs_per_cell();
        let idx = run.lo + (col - run.col0) as usize * slabs + slabs / 2;
        let cap = run.hi.saturating_sub(1);
        let target = if idx < cap { idx } else { cap };
        let plan = self.plan.as_slice();
        if target >= plan.len() {
            return None;
        }
        let seg = &plan[target];
        Some(Band {
            top: seg.spine - seg.up,
            spine: seg.spine,
            bottom: seg.spine + seg.dn,
        })
    }

    /// Live cells — `Status::cells` and one term of the idle test.
    #[inline]
    #[must_use]
    pub const fn live_cells(&self) -> usize {
        self.cells.len()
    }

    /// The planned boundaries lit on the last frame, at or over [`STATUS_LIT_COV`].
    #[inline]
    pub fn lit_segments(&self) -> impl Iterator<Item = &Segment> {
        self.plan.iter().filter(|s| s.cov >= STATUS_LIT_COV)
    }

    /// Count of planned boundaries lit on the last frame, at or over [`STATUS_LIT_COV`].
    #[inline]
    #[must_use]
    pub const fn lit_segment_count(&self) -> usize {
        let plan = self.plan.as_slice();
        let mut count = 0;
        let mut i = 0;
        while i < plan.len() {
            if plan[i].cov >= STATUS_LIT_COV {
                count += 1;
            }
            i += 1;
        }
        count
    }

    /// What is on the glass, by plan.
    #[cfg(test)]
    pub fn ink_segments(&self) -> impl Iterator<Item = &Segment> {
        let bg = self.ground;
        let ink = &self.ink;
        self.plan.iter().filter(move |s| {
            if s.cov == 0 {
                return false;
            }
            let a = (f32::from(s.cov) * (1.0 + STRIP_LIFT_GAIN)).min(BODY_FRAME_TOP) as u8;
            reads_as_ink(over_premul(bg, premul_rgb(ink.at(s.t), a), a))
        })
    }

    /// Milliseconds left in a falling curtain, or `None` when none is falling.
    #[inline]
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        let at = self.curtain?;
        let r = now.saturating_duration_since(at).as_secs_f32();
        Some(((CURTAIN_S - r).max(0.0) * 1000.0).round() as u32)
    }

    /// True when nothing is laid.
    #[inline]
    #[must_use]
    pub const fn at_rest(&self) -> bool {
        self.cells.is_empty() && self.cohorts.is_empty()
    }

    /// The cadence law, brisk half.
    #[inline]
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        #[inline]
        fn edge_ramping(now: Instant, at: Instant) -> bool {
            now.saturating_duration_since(at).as_secs_f32() < EDGE_IN_S
        }
        const TWIN_TRANSLATED_WINDOW_DUR: std::time::Duration =
            super::witness::TWIN_MIN.saturating_add(std::time::Duration::from_millis(16));
        if (!self.reduced && self.curtain.is_some() && !self.cells.is_empty())
            || (!self.reduced && self.seam.is_some() && !self.cells.is_empty())
            || matches!(self.rearm, Some((at, _)) if edge_ramping(now, at))
            || matches!(
                self.translated_at,
                Some(at) if now.saturating_duration_since(at) < TWIN_TRANSLATED_WINDOW_DUR
            )
        {
            return true;
        }
        let mut i = self.cells.len();
        while i > 0 {
            i -= 1;
            if edge_ramping(now, self.cells[i].attack_at) {
                return true;
            }
        }
        if !self.reduced {
            let mut ci = 0;
            while ci < self.cohorts.len() {
                let c = &self.cohorts[ci];
                if c.phase_at(c.idle_s(now)) == Some(Phase::Retracting) {
                    return true;
                }
                ci += 1;
            }
        }
        false
    }

    /// Seconds until the mark's settle under a falling spine moves a pixel.
    #[inline]
    #[must_use]
    pub const fn settle_step_s(&self, disp: f32) -> Option<f32> {
        if self.cells.is_empty() || !disp.is_finite() || disp <= 0.0 {
            return None;
        }
        let wedge = self.wedge_px * disp * (1.0 / DISP_RELEASE_TAU);
        let wave = if self.reduced {
            0.0
        } else {
            WAVE_AMP_CELLS * self.cell_h * disp * std::f32::consts::TAU * PHASE_RATE * disp
        };
        let rate = wedge + wave;
        if rate > 0.0 { Some(1.0 / rate) } else { None }
    }

    /// The cadence law.
    #[inline]
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        #[inline]
        fn secs_since(now: Instant, at: Instant) -> f32 {
            now.saturating_duration_since(at).as_secs_f32()
        }
        if self.cells.is_empty()
            && self.cohorts.is_empty()
            && self.ember_at.is_none()
            && self.curtain.is_none()
        {
            return None;
        }
        let mut cad = Cadence::at(now);
        if self.brisk(now) {
            cad.brisk();
            return cad.take();
        }
        let dur = std::time::Duration::from_secs_f32;
        const RETRACT_FADE_DUR: std::time::Duration = std::time::Duration::from_nanos(
            (RETRACT_FADE_S as f64 * 1_000_000_000.0).round() as u64,
        );
        const RETIRE_MELT_DUR: std::time::Duration = std::time::Duration::from_nanos(
            (RETIRE_MELT_S as f64 * 1_000_000_000.0).round() as u64,
        );
        const FOCUS_EMBER_DUR: std::time::Duration = std::time::Duration::from_nanos(
            (FOCUS_EMBER_S as f64 * 1_000_000_000.0).round() as u64,
        );
        const CURTAIN_DUR: std::time::Duration =
            std::time::Duration::from_nanos((CURTAIN_S as f64 * 1_000_000_000.0).round() as u64);
        let reduced_fade_s = REDUCED_MOTION_FADE_MS / 1000.0;
        let (ember_left, ember_u, ember_spend) = match self.ember_at {
            Some(at) => {
                let s = secs_since(now, at);
                let u = clamp01(s * (1.0 / FOCUS_EMBER_S));
                (Some(FOCUS_EMBER_S - s), Some(u), Some(spend(u)))
            }
            None => (None, None, None),
        };
        // On glass (§15.2 1.8) the exits move the glass's height, not its opacity: each cell's
        // steps are one pixel of its reach at the hand, its ember the glass's height floor.
        #[cfg(test)]
        let glass_px = if self.glass_level_steps {
            0.0
        } else {
            self.glass_px
        };
        #[cfg(not(test))]
        let glass_px = self.glass_px;
        #[cfg(any(test, debug_assertions))]
        if glass_px > 0.0 {
            count_glass_exit();
        }
        let mut cached_coh: Option<(u32, Option<&Cohort>)> = None;
        let mut ci = 0;
        while ci < self.cells.len() {
            if cad.at_tail_floor() {
                break;
            }
            let cell = &self.cells[ci];
            ci += 1;
            let coh = match cached_coh {
                Some((id, c)) if id == cell.cohort => c,
                _ => {
                    let c = self.cohort_by_id(cell.cohort);
                    cached_coh = Some((cell.cohort, c));
                    c
                }
            };
            let age = secs_since(
                now,
                match coh {
                    Some(c) => cell.born.max(c.alive_at),
                    None => cell.born,
                },
            );
            let life = cell.life_s.max(1e-3);
            let flowing = matches!(coh, Some(c) if c.flow.is_some());
            if !flowing {
                cad.tail(cell.life_s - age);
            }
            if let Some(at) = cell.retract_at
                && secs_since(now, at) >= RETRACT_FADE_S
            {
                continue;
            }
            if self.reduced {
                if !flowing {
                    cad.tail((cell.life_s - age - reduced_fade_s).max(0.0));
                }
                if let Some(at) = cell.retract_at {
                    cad.tail((RETRACT_FADE_S - secs_since(now, at) - reduced_fade_s).max(0.0));
                }
                if let Some(at) = cell.retire_at {
                    cad.tail((RETIRE_MELT_S - secs_since(now, at) - reduced_fade_s).max(0.0));
                }
                if let Some(left) = ember_left {
                    cad.tail((left - reduced_fade_s).max(0.0));
                }
                if let Some(c) = coh {
                    cad.tail((c.swoosh_total() - c.idle_s(now) - reduced_fade_s).max(0.0));
                }
                continue;
            }
            let base = if glass_px > 0.0 {
                GLASS_UP_CH * glass_px
            } else {
                UNDER_COV_CAP * cell.cov0
            };
            let u = clamp01(age / life);
            let melt = expiry_melt(u);
            let (retract_u, f_r) = match cell.retract_at {
                Some(at) => {
                    let ur = clamp01(secs_since(now, at) * (1.0 / RETRACT_FADE_S));
                    (Some(ur), spend(ur))
                }
                None => (None, 1.0),
            };
            let (swoosh_u, f_s) = if let Some(c) = coh {
                let idle = c.idle_s(now);
                if c.phase_at(idle) == Some(Phase::Fading) {
                    let t0 = c.retract_start() + c.retract_span() * (1.0 - c.drain_rank(cell.col));
                    let us = clamp01((idle - t0) * (1.0 / RETRACT_FADE_S));
                    (Some(us), spend(us))
                } else {
                    (None, 1.0)
                }
            } else {
                (None, 1.0)
            };
            let (retire_u, f_m) = match cell.retire_at {
                Some(at) => {
                    let um = clamp01(secs_since(now, at) * (1.0 / RETIRE_MELT_S));
                    (Some(um), spend(um))
                }
                None => (None, 1.0),
            };
            let carried = base * melt * f_r * f_s * f_m;
            let (ember_floor, f_e) = match ember_spend {
                Some(s) => {
                    let floor = if glass_px > 0.0 {
                        glass_height_floor(carried * (GLASS_TAIL_UP_CH / GLASS_UP_CH))
                    } else {
                        self.ember_floor_of(carried, cell.t)
                    };
                    (floor, ember_level(floor, s))
                }
                None => (0.0, 1.0),
            };
            if !flowing && let Some(step) = level_step_melt(base * f_r * f_e * f_s * f_m, u, life) {
                cad.tail(step);
            }
            if let Some(ur) = retract_u
                && let Some(step) =
                    level_step_spend(base * melt * f_e * f_s * f_m, ur, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(ue) = ember_u
                && let Some(step) =
                    level_step_spend(carried * (1.0 - ember_floor).max(0.0), ue, FOCUS_EMBER_S)
            {
                cad.tail(step);
            }
            if let Some(us) = swoosh_u
                && let Some(step) =
                    level_step_spend(base * melt * f_r * f_e * f_m, us, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(um) = retire_u
                && let Some(step) =
                    level_step_spend(base * melt * f_r * f_e * f_s, um, RETIRE_MELT_S)
            {
                cad.tail(step);
            }
            if let Some(at) = cell.retract_at {
                cad.tail_at(at);
                cad.tail_at(at + RETRACT_FADE_DUR);
            }
            if let Some(at) = cell.retire_at {
                cad.tail_at(at + RETIRE_MELT_DUR);
            }
        }
        // A wrapped row the hand has left (§2.4) steps by one pixel of its sink, which falls from
        // the glass's reach at the hand to [`WRAP_SINK_CH`]; under reduced motion it has sunk.
        #[cfg(test)]
        let sinks = !self.wrap_unsunk;
        #[cfg(not(test))]
        let sinks = true;
        if glass_px > 0.0 && !self.reduced && sinks {
            let up = GLASS_UP_CH * glass_px;
            let fall = up - up.min(WRAP_SINK_CH * self.cell_h);
            let mut wi = 0;
            while wi < self.glass_wrap.len() {
                let x = secs_since(now, self.glass_wrap[wi].left_at) * (1.0 / WRAP_SINK_S);
                if let Some(step) = level_step_sink(fall, x) {
                    cad.tail(step);
                }
                wi += 1;
            }
        }
        let mut ki = 0;
        while ki < self.cohorts.len() {
            let coh = &self.cohorts[ki];
            ki += 1;
            // Edges: the grace's end, the two beats after it, the retract's first frame and the swoosh's end; the retract's end is a tail.
            if coh.flow.is_some() {
                cad.edge(coh.clock() + dur(coh.swoosh_total()));
                if !self.reduced {
                    cad.tail_at(coh.clock() + dur(coh.retract_span()));
                }
                continue;
            }
            let grace = coh.grace_s.max(PHRASE_REST_MIN_S);
            let beats = [
                grace,
                grace + REACH_STEP_S,
                grace + 2.0 * REACH_STEP_S,
                coh.retract_start(),
                coh.swoosh_total(),
            ];
            let mut bi = 0;
            while bi < beats.len() {
                cad.edge(coh.alive_at + dur(beats[bi]));
                bi += 1;
            }
            cad.tail_at(coh.alive_at + dur(coh.retract_start() + coh.retract_span()));
        }
        if let Some(at) = self.ember_at {
            cad.tail_at(at + FOCUS_EMBER_DUR);
        }
        if let Some(at) = self.curtain {
            cad.tail_at(at + CURTAIN_DUR);
        }
        cad.take()
    }

    /// The band follows its text along the row.
    #[inline]
    pub fn shift_run(
        &mut self,
        run: super::witness::ShiftRun,
        moved: &mut Vec<(u16, u16, Instant)>,
    ) -> usize {
        let super::witness::ShiftRun {
            row,
            cohort,
            lo,
            hi,
            dc,
        } = run;
        if dc == 0 || lo > hi {
            return 0;
        }
        let Some(ci) = self.cohort_index(cohort) else {
            return 0;
        };
        if self.cohorts[ci].row != row {
            return 0;
        }
        let carry_partial = self.partial_custody_follows(ci, lo, hi, true);
        let mut i = 0usize;
        while i < self.cells.len() {
            let c = self.cells[i];
            let stale = if c.row == row
                && c.cohort == cohort
                && !c.leaving()
                && !(c.col >= lo && c.col <= hi)
                && let Some(src) = c.col.checked_add_signed(-dc)
                && src >= lo
                && src <= hi
            {
                let mut has_src = false;
                let mut di = 0;
                while di < self.cells.len() {
                    let d = &self.cells[di];
                    if d.row == row && d.cohort == cohort && !d.leaving() && d.col == src {
                        has_src = true;
                        break;
                    }
                    di += 1;
                }
                has_src
            } else {
                false
            };
            if stale {
                self.cells.remove(i);
            } else {
                i += 1;
            }
        }
        let walk = self.cohorts[ci];
        let walk_zero = walk.walk_zero();
        let release_parts = &self.release_parts;
        let mut n = 0usize;
        let (mut c0, mut c1) = (u16::MAX, 0u16);
        let mut j = 0;
        while j < self.cells.len() {
            let c = &mut self.cells[j];
            j += 1;
            if c.row != row || c.cohort != cohort {
                continue;
            }
            if ((!c.leaving() && c.col >= lo && c.col <= hi)
                || (carry_partial && c.col >= lo && walk.holds_partial(c, release_parts)))
                && let Some(col) = c.col.checked_add_signed(dc)
            {
                moved.push((c.row, c.col, c.born));
                c.col = col;
                c.t = walk_zero + walk_t(walk.walk_d(col));
                self.followed_now.push((row, col, c.born));
                n += 1;
            }
            c0 = c0.min(c.col);
            c1 = c1.max(c.col.saturating_add(1));
        }
        if n == 0 {
            return 0;
        }
        self.shifted_now.push((row, lo, hi, dc));
        let coh = &mut self.cohorts[ci];
        coh.col0 = c0;
        coh.col1 = c1;
        if coh.flow.is_some()
            && let Some(edge) = coh.retract_col
            && edge >= lo
            && edge <= hi
            && let Some(edge) = edge.checked_add_signed(dc)
        {
            coh.retract_col = Some(edge);
        }
        self.index.reset();
        self.plan.clear();
        self.runs.clear();
        self.prune_release_parts();
        n
    }

    #[inline]
    const fn partial_custody_follows(&self, ci: usize, lo: u16, hi: u16, suffix: bool) -> bool {
        let cohorts = self.cohorts.as_slice();
        if ci >= cohorts.len() {
            return false;
        }
        let cohort = &cohorts[ci];
        if cohort.abandoned {
            return false;
        }
        let cells = self.cells.as_slice();
        let mut i = 0;
        while i < cells.len() {
            let cell = &cells[i];
            if cell.cohort == cohort.id
                && !cell.leaving()
                && !((suffix && cell.col < lo) || (cell.col >= lo && cell.col <= hi))
            {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The band follows its text across rows.
    #[inline]
    pub fn translate_run(
        &mut self,
        run: super::witness::FollowRun,
        at: Instant,
        away: bool,
        moved: &mut Vec<(u16, u16, Instant)>,
    ) -> usize {
        let super::witness::FollowRun {
            row,
            cohort,
            lo,
            hi,
            dr,
        } = run;
        let Some(ci) = self.cohort_index(cohort) else {
            return 0;
        };
        if self.cohorts[ci].row != row {
            return 0;
        }
        let Some(new_row) = row.checked_add_signed(dr) else {
            return 0;
        };
        if dr == 0 || lo > hi {
            return 0;
        }
        #[inline]
        fn carried(
            cell: &Cell,
            lo: u16,
            hi: u16,
            carry_partial: bool,
            source: &Cohort,
            release_parts: &[ReleasePart],
        ) -> bool {
            (cell.col >= lo && cell.col <= hi)
                || (carry_partial && source.holds_partial(cell, release_parts))
        }
        let source = self.cohorts[ci];
        let carry_partial = self.partial_custody_follows(ci, lo, hi, false);
        let release_parts = &self.release_parts;
        let mut n = 0usize;
        let mut all = 0usize;
        let (mut c0, mut c1) = (u16::MAX, 0u16);
        let (mut k0, mut k1) = (u16::MAX, 0u16);
        let mut i = 0;
        while i < self.cells.len() {
            let c = &self.cells[i];
            i += 1;
            if c.row != row || c.cohort != cohort {
                continue;
            }
            all += 1;
            if carried(c, lo, hi, carry_partial, &source, release_parts) {
                n += 1;
                c0 = c0.min(c.col);
                c1 = c1.max(c.col + 1);
            } else {
                k0 = k0.min(c.col);
                k1 = k1.max(c.col + 1);
            }
        }
        if n == 0 {
            return 0;
        }
        let id = if all == n {
            self.cohorts[ci].row = new_row;
            cohort
        } else {
            let mut copy = self.cohorts[ci];
            copy.id = self.next_cohort;
            self.next_cohort = self.next_cohort.wrapping_add(1);
            copy.row = new_row;
            copy.release_clock = None;
            copy.col0 = c0;
            copy.col1 = c1;
            if copy.flow.is_some() && c0 < c1 {
                if let Some(e) = &mut copy.retract_col {
                    *e = (*e).clamp(c0, c1 - 1);
                }
                copy.flow_drift = copy
                    .flow_drift
                    .min(FLOW_DRIFT_MAX_SHARE * f32::from(c1 - c0));
            }
            let old = &mut self.cohorts[ci];
            old.col0 = k0;
            old.col1 = k1;
            old.release_clock = None;
            self.cohorts.push(copy);
            if away {
                self.split_off = Some(SplitOff {
                    copy: copy.id,
                    dr,
                    col0: k0,
                    col1: k1,
                });
            }
            copy.id
        };
        let release_parts = &self.release_parts;
        let mut j = 0;
        while j < self.cells.len() {
            let c = &mut self.cells[j];
            if c.row == row
                && c.cohort == cohort
                && carried(c, lo, hi, carry_partial, &source, release_parts)
            {
                moved.push((c.row, c.col, c.born));
                c.row = new_row;
                c.cohort = id;
                self.followed_now.push((new_row, c.col, c.born));
            }
            j += 1;
        }
        if id != cohort {
            self.split_release(cohort, id);
        }
        // The wrap's sink rides with its row ([`GlassWrap`], light only).
        if !self.glass_wrap.is_empty() {
            self.carry_glass_wrap(cohort, row, id, new_row);
        }
        if let Some(coh) = self.cohort_by_id_mut(id) {
            if away {
                Self::flow_cohort(coh, at);
            } else if coh.flow.take().is_some() {
                coh.retract_col = None;
                coh.flow_drift = 0.0;
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
            }
        }
        if !away {
            self.translated_at = Some(at);
            if matches!(self.seam, Some(s) if s.row == new_row) {
                self.seam = None;
            }
        }
        // A run carries its own prior shape, including across multiple hops
        // in this batch. Never mutate another run's prior row state here.
        let previous = self.cohort_hold(cohort, row);
        self.stage_hold(id, previous);
        self.index.reset();
        self.plan.clear();
        self.runs.clear();
        n
    }

    /// A scroll carries the ribbon with its text (seam point 12).
    #[inline]
    pub fn translate_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.cells.retain_mut(|c| {
            if c.row < rows {
                return false;
            }
            c.row -= rows;
            true
        });
        self.cohorts.retain_mut(|c| {
            if c.row < rows {
                return false;
            }
            c.row -= rows;
            true
        });
        let cells = &self.cells;
        self.cohorts.retain(|coh| {
            let mut i = 0;
            while i < cells.len() {
                if cells[i].cohort == coh.id {
                    return true;
                }
                i += 1;
            }
            false
        });
        let cohorts = &self.cohorts;
        let mut last_coh: Option<(u32, bool)> = None;
        self.cells.retain(|c| match last_coh {
            Some((id, keep)) if id == c.cohort => keep,
            _ => {
                let mut keep = false;
                let mut i = 0;
                while i < cohorts.len() {
                    if cohorts[i].id == c.cohort {
                        keep = true;
                        break;
                    }
                    i += 1;
                }
                last_coh = Some((c.cohort, keep));
                keep
            }
        });
        self.prune_release_parts();
        if let Some((row, _)) = &mut self.caret {
            *row = row.saturating_sub(rows);
        }
        #[inline]
        const fn scroll_pt(pt: Option<(u16, u16)>, rows: u16) -> Option<(u16, u16)> {
            match pt {
                Some((row, col)) if row >= rows => Some((row - rows, col)),
                _ => None,
            }
        }
        self.pen = scroll_pt(self.pen, rows);
        self.arrived = scroll_pt(self.arrived, rows);
        self.hand_floor = scroll_pt(self.hand_floor, rows);
        self.last_space = scroll_pt(self.last_space, rows);
        self.prev_space = scroll_pt(self.prev_space, rows);
        self.attach = scroll_pt(self.attach, rows);
        self.attach_floor = None;
        self.seam = match self.seam {
            Some(s) if s.row >= rows => Some(Seam {
                row: s.row - rows,
                ..s
            }),
            _ => None,
        };
        self.relocate = match self.relocate {
            Some(r) if r.row >= rows => Some(Relocate {
                row: r.row - rows,
                ..r
            }),
            _ => None,
        };
        self.carry = None;
        if let Some(proof) = &mut self.short_reanchor {
            proof.from.0 = proof.from.0.saturating_sub(rows);
            proof.to.0 = proof.to.0.saturating_sub(rows);
        }
        if let Some(proof) = &mut self.rewrap_start {
            proof.from.0 = proof.from.0.saturating_sub(rows);
            proof.to.0 = proof.to.0.saturating_sub(rows);
        }
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = match gate.left_row {
                Some(row) => row.checked_sub(rows),
                None => None,
            };
        }
        self.holds.retain_mut(|h| match h.0.checked_sub(rows) {
            Some(row) => {
                h.0 = row;
                true
            }
            None => false,
        });
        #[cfg(test)]
        if self.holds_left_behind {
            self.holds.clear();
        }
        // The wrap's sinks ride with their rows ([`GlassWrap`], light only).
        if !self.glass_wrap.is_empty() {
            self.glass_wrap
                .retain_mut(|w| match w.row.checked_sub(rows) {
                    Some(row) => {
                        w.row = row;
                        true
                    }
                    None => false,
                });
        }
        self.index.translate(rows);
        self.plan.clear();
        self.runs.clear();
        self.scroll_followed = true;
    }

    /// A row band carries its ribbon with its text (seam point 12).
    #[inline]
    pub fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.cells
            .retain_mut(|c| match band_row(c.row, top, bottom, delta) {
                Some(row) => {
                    c.row = row;
                    true
                }
                None => false,
            });
        self.cohorts
            .retain_mut(|c| match band_row(c.row, top, bottom, delta) {
                Some(row) => {
                    c.row = row;
                    true
                }
                None => false,
            });
        let cells = &self.cells;
        self.cohorts.retain(|coh| {
            let mut i = 0;
            while i < cells.len() {
                if cells[i].cohort == coh.id {
                    return true;
                }
                i += 1;
            }
            false
        });
        let cohorts = &self.cohorts;
        let mut last_coh: Option<(u32, bool)> = None;
        self.cells.retain(|c| match last_coh {
            Some((id, keep)) if id == c.cohort => keep,
            _ => {
                let mut keep = false;
                let mut i = 0;
                while i < cohorts.len() {
                    if cohorts[i].id == c.cohort {
                        keep = true;
                        break;
                    }
                    i += 1;
                }
                last_coh = Some((c.cohort, keep));
                keep
            }
        });
        self.prune_release_parts();
        if let Some((row, _)) = &mut self.caret {
            *row = band_pos(*row, top, bottom, delta);
        }
        #[inline]
        const fn band_pt(
            pt: Option<(u16, u16)>,
            top: u16,
            bottom: u16,
            delta: i16,
        ) -> Option<(u16, u16)> {
            match pt {
                Some((row, col)) => match band_row(row, top, bottom, delta) {
                    Some(row) => Some((row, col)),
                    None => None,
                },
                None => None,
            }
        }
        self.pen = band_pt(self.pen, top, bottom, delta);
        self.arrived = band_pt(self.arrived, top, bottom, delta);
        self.hand_floor = band_pt(self.hand_floor, top, bottom, delta);
        self.last_space = band_pt(self.last_space, top, bottom, delta);
        self.prev_space = band_pt(self.prev_space, top, bottom, delta);
        self.attach = band_pt(self.attach, top, bottom, delta);
        self.attach_floor = None;
        self.seam = if let Some(s) = self.seam
            && let Some(row) = band_row(s.row, top, bottom, delta)
        {
            Some(Seam { row, ..s })
        } else {
            None
        };
        self.relocate = if let Some(r) = self.relocate
            && let Some(row) = band_row(r.row, top, bottom, delta)
        {
            Some(Relocate { row, ..r })
        } else {
            None
        };
        self.carry = None;
        if let Some(proof) = &mut self.short_reanchor {
            proof.from.0 = band_pos(proof.from.0, top, bottom, delta);
            proof.to.0 = band_pos(proof.to.0, top, bottom, delta);
        }
        if let Some(proof) = &mut self.rewrap_start {
            proof.from.0 = band_pos(proof.from.0, top, bottom, delta);
            proof.to.0 = band_pos(proof.to.0, top, bottom, delta);
        }
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = match gate.left_row {
                Some(row) => band_row(row, top, bottom, delta),
                None => None,
            };
        }
        self.holds
            .retain_mut(|h| match band_row(h.0, top, bottom, delta) {
                Some(row) => {
                    h.0 = row;
                    true
                }
                None => false,
            });
        #[cfg(test)]
        if self.holds_left_behind {
            self.holds.clear();
        }
        // The wrap's sinks ride with their rows ([`GlassWrap`], light only).
        if !self.glass_wrap.is_empty() {
            self.glass_wrap
                .retain_mut(|w| match band_row(w.row, top, bottom, delta) {
                    Some(row) => {
                        w.row = row;
                        true
                    }
                    None => false,
                });
        }
        self.index.translate_band(top, bottom, delta);
        self.plan.clear();
        self.runs.clear();
        self.scroll_followed = true;
    }

    /// [`super::Engine::lift_hand`]'s ribbon half.
    #[inline]
    pub(super) const fn lift_hand(&mut self, row: u16, to: u16) {
        #[inline]
        const fn map(r: u16, row: u16, to: u16) -> u16 {
            if r == row { to } else { r }
        }
        #[inline]
        const fn map_pt(pt: Option<(u16, u16)>, row: u16, to: u16) -> Option<(u16, u16)> {
            match pt {
                Some((r, c)) => Some((map(r, row, to), c)),
                None => None,
            }
        }
        self.caret = map_pt(self.caret, row, to);
        self.pen = map_pt(self.pen, row, to);
        self.arrived = map_pt(self.arrived, row, to);
        self.hand_floor = map_pt(self.hand_floor, row, to);
        self.last_space = map_pt(self.last_space, row, to);
        self.prev_space = map_pt(self.prev_space, row, to);
        self.attach = map_pt(self.attach, row, to);
        self.attach_floor = None;
        self.relocate = match self.relocate {
            Some(r) => Some(Relocate {
                row: map(r.row, row, to),
                ..r
            }),
            None => None,
        };
        if let Some((sw, _)) = &mut self.carry {
            sw.origin.0 = map(sw.origin.0, row, to);
            sw.landing.0 = map(sw.landing.0, row, to);
        }
        if let Some(proof) = &mut self.short_reanchor {
            proof.from.0 = map(proof.from.0, row, to);
            proof.to.0 = map(proof.to.0, row, to);
        }
        if let Some(proof) = &mut self.rewrap_start {
            proof.from.0 = map(proof.from.0, row, to);
            proof.to.0 = map(proof.to.0, row, to);
        }
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = match gate.left_row {
                Some(r) => Some(map(r, row, to)),
                None => None,
            };
        }
        // The wrap's sinks ride with their rows ([`GlassWrap`], light only).
        if !self.glass_wrap.is_empty() {
            let wraps = self.glass_wrap.as_mut_slice();
            let mut i = 0;
            while i < wraps.len() {
                wraps[i].row = map(wraps[i].row, row, to);
                i += 1;
            }
        }
    }

    /// The seam on the hand's lifted line ([`super::Engine::lift_hand`]) rides from `row` to `to` with it.
    #[inline]
    pub(super) const fn lift_seam(&mut self, row: u16, to: u16) {
        if let Some(seam) = &mut self.seam
            && seam.row == row
        {
            seam.row = to;
        }
    }

    /// The seam's row ([`Ribbon::seam`]), for the engine's tests.
    #[cfg(test)]
    pub(super) const fn seam_row(&self) -> Option<u16> {
        match self.seam {
            Some(s) => Some(s.row),
            None => None,
        }
    }

    /// Drop everything — style switch, layout change, `Engine::reset`.
    #[inline]
    pub fn reset(&mut self) {
        self.cells.clear();
        self.cohorts.clear();
        self.release_parts.clear();
        self.holds.clear();
        self.holds_next.clear();
        self.carried_holds.clear();
        self.glass_wrap.clear();
        self.wrap_hand = None;
        self.plan.clear();
        self.runs.clear();
        self.sorted.clear();
        self.verts.clear();
        self.index.reset();
        self.caret = None;
        self.ember_at = None;
        self.rearm = None;
        self.clear_space_transient_state();
        self.ink_quads = 0;
        self.last_walk = None;
        self.fresh_line = None;
        self.seam = None;
        self.phrase_open = None;
        self.curtain = None;
    }

    #[inline]
    fn clear_space_transient_state(&mut self) {
        self.pending_erase = 0;
        self.pen = None;
        self.arrived = None;
        self.hand_floor = None;
        self.last_space = None;
        self.prev_space = None;
        self.relocate = None;
        self.carry = None;
        self.split_off = None;
        self.short_reanchor = None;
        self.rewrap_start = None;
        self.followed_now.clear();
        self.shifted_now.clear();
        self.scroll_followed = false;
        self.attach = None;
        self.attach_floor = None;
    }
}

#[cfg(any(test, debug_assertions))]
mod state_words;
#[cfg(any(test, debug_assertions))]
pub(crate) use state_words::{lift_words, revive_words, run_origin_words, soft_wrap_words};

#[cfg(test)]
#[path = "ribbon_tests.rs"]
mod tests;

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
pub(crate) fn dark_bed_budget(ink: u32) -> f32 {
    let bar =
        (crate::color_math::relative_luminance(ink & 0x00FF_FFFF) + 0.05) / DARK_BED_BAR - 0.05;
    bar.clamp(DARK_BED_LUMA_MIN, DARK_BED_LUMA_MAX)
}

/// The bed's ink for the stop `rgb` over the page `bg` under `budget`.
#[must_use]
pub(crate) fn dark_bed_ink(rgb: u32, bg: u32, budget: f32, sat: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let rgb = crate::spectrum::spectrum_with_min_saturation(rgb & 0x00FF_FFFF, sat);
    let ch = |c: u32, sh: u32| (c >> sh) & 0xff;
    let mx = ch(rgb, 16).max(ch(rgb, 8)).max(ch(rgb, 0));
    let full = if mx == 0 {
        [0.0; 3]
    } else {
        let k = 255.0 / mx as f32;
        [16u32, 8, 0].map(|sh| ((ch(rgb, sh) as f32 * k).round() as u32).min(255) as f32)
    };
    let bg_rgb = [16u32, 8, 0].map(|sh| ch(bg, sh) as usize);
    let pick_rgb = |t: f32| -> [usize; 3] {
        full.map(|c| {
            let v = if t <= 0.5 {
                c * 2.0 * t
            } else {
                c + (255.0 - c) * (2.0 * t - 1.0)
            };
            (v as usize).min(255)
        })
    };
    let seen = |ink: [usize; 3]| {
        crate::color_math::relative_luminance_rgb(
            table,
            ink[0].max(bg_rgb[0]),
            ink[1].max(bg_rgb[1]),
            ink[2].max(bg_rgb[2]),
        )
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (pick_rgb(0.0), pick_rgb(1.0));
    let (mut lo_ok, mut hi_ok) = (false, false);
    for _ in 0..20 {
        if lo_rgb == hi_rgb {
            break;
        }
        let mid = 0.5 * (lo + hi);
        let mid_rgb = pick_rgb(mid);
        if lo_ok && mid_rgb == lo_rgb {
            lo = mid;
        } else if hi_ok && mid_rgb == hi_rgb {
            hi = mid;
        } else if seen(mid_rgb) <= budget {
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
    };

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
pub(crate) fn night_hue(h: f32) -> f32 {
    let u = (h - 90.0).rem_euclid(360.0);
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
pub(crate) fn night_rgb(rgb: u32) -> u32 {
    let (h, s, v) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
    crate::color_math::hsv2rgb(night_hue(h), s, v)
}

/// `budget` ([`dark_bed_budget`]) dimmed to `dim` of its rise over `page`.
#[inline]
#[must_use]
pub(crate) fn dim_budget(budget: f32, page: u32, dim: f32) -> f32 {
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
    } = scratch;
    let caret = *caret;
    let cell_of = |x: f32, y: f32| {
        (
            ((y - oyf) / chf).floor() as i32,
            ((x - oxf) / cwf).floor() as i32,
        )
    };
    let smooth = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
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
            let feather = |j: usize| {
                let u = j as f32 / DARK_BED_HEAD_FEATHER_KNOTS as f32;
                (from + feather_span * u, taper_from * (1.0 - smooth(u)))
            };
            let feather_over =
                |verts: &mut Vec<RibbonVertex>, at: &dyn Fn(f32, f32) -> RibbonVertex| {
                    while verts.len() > 1 && verts.last().is_some_and(|v| (v.x - from) * dir >= 0.0)
                    {
                        verts.pop();
                    }
                    for j in 0..=DARK_BED_HEAD_FEATHER_KNOTS {
                        let (x, k) = feather(j);
                        verts.push(at(x, k));
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
                let bottom = oyf + (row + 1) as f32 * chf;
                let reach = shape.rail * chf;
                let rail_up = reach * shape.rail_rise;
                let rail_core_dn = reach * DARK_RAIL_CORE * (1.0 - shape.rail_rise);
                let leading = |x: f32| !text.over_text(row + 1, ((x - oxf) / cwf).floor() as i32);
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
                let reach = stretch
                    .iter()
                    .map(|s| shape.at(s.cov, chf, 1.0).1)
                    .sum::<f32>()
                    / stretch.len() as f32;
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
                tube.extend(
                    stretch
                        .iter()
                        .zip(&ink[start..i])
                        .map(|(s, &rgb)| vertex(s, rgb, s.x, s.y)),
                );
                if i < run.len() {
                    let (a, b) = (run[i - 1], run[i]);
                    tube.push(vertex(&a, ink[i - 1], 0.5 * (a.x + b.x), 0.5 * (a.y + b.y)));
                }
                if tube.len() >= 2 && shape.dim > 0.0 {
                    let (a, b) = (tube[0], tube[tube.len() - 1]);
                    let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                    let f = (cwf * 0.5 / len.max(1.0)).min(0.3);
                    let at = |k: f32, cov: u8| BeamVertex {
                        x: a.x + (b.x - a.x) * k,
                        y: a.y + (b.y - a.y) * k,
                        color: if k < 0.5 { a.color } else { b.color },
                        cov,
                    };
                    static LAYER_COV: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
                    let lut = LAYER_COV.get_or_init(|| {
                        std::array::from_fn(|a| {
                            let a = a as f32 / 255.0;
                            let layer = 1.0 - (1.0 - a).powf(1.0 / LEAP_LAYERS as f32);
                            (layer * 255.0).round().clamp(0.0, 255.0) as u8
                        })
                    });
                    let (ca, cb) = (lut[usize::from(a.cov)], lut[usize::from(b.cov)]);
                    tube.clear();
                    tube.extend([at(0.0, 0), at(f, ca), at(1.0 - f, cb), at(1.0, 0)]);
                    for k in 0..LEAP_LAYERS {
                        let shrink = 1.0 - k as f32 / LEAP_LAYERS as f32;
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
    let cwf = geom.cw as f32;
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
                cells = (head.x - tail.x).abs() / cwf + 1.0;
                let row = ((head.y - f32::from(geom.origin_y)) / geom.ch as f32)
                    .floor()
                    .clamp(0.0, f32::from(u16::MAX)) as u16;
                let col = ((head.x - f32::from(geom.origin_x)) / cwf).floor();
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
            let cov = piece.iter().map(|s| s.cov).max().unwrap_or(head.cov);
            lengths.push(cells);
            let span = cells * cwf;
            out[at].extend((0..=knots).rev().map(|k| {
                let d = span * k as f32 / knots as f32;
                CometSample {
                    x: head.x + back * d,
                    y: head.y,
                    cov,
                    pos: 1.0 - d / span,
                }
            }));
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
            let w = f32::from(q.w.max(1));
            if q.color != q.color2 {
                let lerp = |a: u32, b: u32, x: i32| {
                    let t = (x - qx0) as f32 / w;
                    let ch = |sh: u32| {
                        let (u, v) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
                        ((u + (v - u) * t).round() as u32).min(255)
                    };
                    (ch(16) << 16) | (ch(8) << 8) | ch(0)
                };
                if cx0 != qx0 {
                    c.color = lerp(q.color, q.color2, cx0);
                }
                if cx1 != qx1 {
                    c.color2 = lerp(q.color, q.color2, cx1);
                }
            }
            if q.alpha != 0 {
                if q.alpha == q.alpha2 {
                    c.alpha = q.alpha.max(1);
                    c.alpha2 = q.alpha2.max(1);
                } else {
                    let lerp_a = |a: u8, b: u8, x: i32| {
                        let t = (x - qx0) as f32 / w;
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
    let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let over_text = |s: &CometSample| {
        let (row, col) = (
            ((s.y - oyf) / chf).floor() as i32,
            ((s.x - oxf) / cwf).floor() as i32,
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
        let mut last: Option<(u32, u32, u32)> = None;
        for run in runs {
            for s in run {
                let raw = color(self, s.pos);
                let (bed_c, rail_c) = match last {
                    Some((prev, b, r)) if prev == raw => (b, r),
                    _ => {
                        let hue = match shape.hue {
                            BedHue::Night => night_rgb(raw),
                            BedHue::True => raw,
                        };
                        let b = self.bed_ink(hue, page, budget, shape.sat);
                        let r = if shape.rail > 0.0 {
                            crate::rainbow_kitty::ribbon::rail_ink(raw & 0x00FF_FFFF)
                        } else {
                            0
                        };
                        last = Some((raw, b, r));
                        (b, r)
                    }
                };
                scratch.inks.push(bed_c);
                if shape.rail > 0.0 {
                    scratch.rails.push(rail_c);
                }
            }
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

    /// [`dark_bed_ink`] ([`Self::bed_unsolved`]).
    fn bed_ink(&self, rgb: u32, page: u32, budget: f32, sat: f32) -> u32 {
        if self.bed_unsolved() {
            let rgb = crate::spectrum::spectrum_with_min_saturation(rgb & 0x00FF_FFFF, sat);
            let mx = ((rgb >> 16) & 0xff)
                .max((rgb >> 8) & 0xff)
                .max(rgb & 0xff)
                .max(1);
            let up = |sh: u32| ((((rgb >> sh) & 0xff) * 255) / mx).min(255);
            (up(16) << 16) | (up(8) << 8) | up(0)
        } else {
            dark_bed_ink(rgb, page, budget, sat)
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

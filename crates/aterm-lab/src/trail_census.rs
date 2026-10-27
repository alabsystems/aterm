// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-census <png>`: a trail frame's census, layer by layer, as JSON: the checkers behind Prism Glass's numbers
//! (docs/design/PRISM-GLASS.md §15.1 P0.13), rebuilt over the frames and sidecars `trail-contrast` reads.
//!
//! A frame carries no layer tags, so the census reads the layers off its pixels against the frame's clean twin
//! (`image clean`, the same frame with no effect drawn). A trail pixel is one the effects changed (a channel off by
//! more than `trail-contrast`'s [`DIFF_DELTA`]) where the twin shows its cell's ground, so a glyph the effects tint is
//! judged as text, never as trail. The heuristic that sorts trail pixels into layers runs in this order:
//! 1. The caret's box (the cursor sidecar's cell, grown by `--caret-pad`) and each `--pet` box take every changed
//!    pixel in them.
//! 2. A small connected group of trail pixels ([`MARK_AREA`], [`MARK_W`], [`MARK_H`]) is a mark, a star, unless its
//!    box lies inside the glyph ink of the cells it touches: that is the band seen through a letter's counter.
//! 3. A band row is a row whose probe ([`PROBE`]) holds trail pixels across a cell's width of columns or more.
//! 4. In each column of a band row the stroke (main's rail, or Prism Glass's lip) is told from the band by colour.
//!    The stroke's colour is the most chromatic trail pixel under the row's bottom edge, within [`HANG`]; when it is
//!    [`STROKE_DE`] or more from the band's colour (the probe's median), each pixel by the edge goes to the colour
//!    that explains it better over the twin ([`explain`]). From the edge the stroke climbs into its own row while it
//!    explains the pixels better (at most [`RISE`]); below it, a pixel the band explains better is the band melting
//!    under its row. Where the next row is a band row too, the stroke is only the run that starts on the edge, and it
//!    ends where the next row's band explains a pixel better.
//! 5. Every other trail pixel of a band row (and, over a row that is no band row, up to [`REACH_UP`] above it) is band;
//!    what is left is `other`.
//!
//! What the heuristic cannot tell apart: a rail that lies wholly inside its row (main's in-row rail over a filled row
//! below) counts as band; a star drawn on the band counts as band; a lip that touches the next row's glass with no
//! seam, where that glass is the more chromatic, counts as that row's band; and a sliver of band that glyph ink cuts
//! off from the rest counts as a mark.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::instance::{take_value, take_values};
use crate::paths::from_caller;
use crate::pyjson::J;
use crate::trail_contrast::{
    BLANK_BELOW, DIFF_DELTA, Grid, Image, PxBox, Rgb, SOLID_SHARE, abspath, caret_cell, clean_twin,
    compact, contrast, frame_grid, hex, median, mode, most_contrasting, parse_hex, round_to,
    sidecar, solid_ink_ratios,
};
use crate::{Fail, Res};

/// The band-row probe, as shares of a row from its top: a band lies on its row's bottom edge (the spine) and fills
/// this stretch, where a stroke hanging from the row above (main's rail reaches RAIL_REACH_MAX_CH 0.36 ch under its
/// row, the lip less) stops short of its middle, and a bed reaching up from the row below (TALL_UP_CH 1.10 ch) stays
/// under a tenth of a row.
pub const PROBE: (f64, f64) = (0.50, 0.75);
/// How far above a band row's bottom edge its stroke may climb into the row, as a share of a row: main's in-row rail
/// (ROW_RAIL_CH 0.12 ch over a filled row below) and the hairline it leaves (RAIL_GAP_CH 0.06 ch), with room.
pub const RISE: f64 = 0.25;
/// How far below a band row's bottom edge its stroke may hang into the next row, as a share of a row (main's rail and
/// its melt reach under 0.45 ch at the hand).
pub const HANG: f64 = 0.50;
/// How far above a band row's top its band may reach where the row above is no band row (main's bed, TALL_UP_CH 1.10
/// ch above its spine), as a share of a row.
pub const REACH_UP: f64 = 0.25;
/// A stroke distinct from its band: its colour at least this far from the band's (`OkLab` Euclidean distance).
pub const STROKE_DE: f64 = 0.05;
/// A mark: a connected group of at most this share of a cell's pixels...
pub const MARK_AREA: f64 = 0.25;
/// ...at most this many cells wide...
pub const MARK_W: f64 = 1.5;
/// ...and at most this share of a row tall.
pub const MARK_H: f64 = 0.75;
/// A band core pixel lies at least this many px inside its column's band, above and below, away from the soft edge.
pub const EDGE_PX: i64 = 2;
/// A band core pixel, and a column whose band top the step measure reads, keeps at least this far (Chebyshev, px)
/// from the twin's glyph ink: `trail-contrast`'s ring and a pixel, past the letter guard's dimming.
pub const INK_CLEAR: i64 = 3;
/// A caret core pixel lies at least this far (Chebyshev, px) inside the caret's drawn pixels: past a 2 px keyline and
/// the cut-out's rim.
pub const KEYLINE_PX: i64 = 2;
/// The fourth check's accent position: a mark over glyph ink across at least this share of its columns...
pub const ACCENT_SHARE: f64 = 0.40;
/// ...whose lowest pixel is at most this many device px above that ink.
pub const ACCENT_GAP_PX: i64 = 9;
/// A seam row: under this share of its span's columns coloured.
pub const SEAM_SHARE: f64 = 0.05;
/// A coloured row of a wrapped cohort: at least this share of its columns coloured.
pub const COLOURED_SHARE: f64 = 0.50;
/// The tail the step measure reads: this many cells from the band's oldest end.
pub const TAIL_CELLS: i64 = 6;
/// A riser: a step in the band's top between neighbouring columns of at least this many px.
pub const RISER_PX: i64 = 2;
/// A whisker: a tail column whose band is at most this many px tall.
pub const WHISKER_PX: i64 = 2;
/// A pet-box pixel is the lip running on under the pet when the stroke beside the box covers it at least half way
/// over the twin, to within this distance (sRGB, Euclidean), and better than the band beside it: the census cannot
/// see under the sprite, and the sprite's own colours are neither the lip's nor the band's.
pub const LIP_MATCH: f64 = 40.0;
/// How far beside a pet's box the stroke it continues is looked for, in cells.
pub const LIP_REACH_CELLS: i64 = 2;
/// Hues are binned only above this chroma (below it the hue is noise).
pub const HUE_C: f64 = 0.04;
/// Mud colours listed per layer.
pub const MUD_LISTED: usize = 5;

/// The verb's own help (`trail-census --help`).
pub const HELP: &str = "\
usage: targo --unverified lab trail-census <png> [--dims FILE | --origin X,Y --cell WxH] [--clean PNG]
         [--cursor FILE | --caret ROW,COL] [--caret-pad PX] [--pet X0,Y0,X1,Y1]... [--selection RRGGBB]
         [--page RRGGBB] [--pixels FILE] [--mud-max N] [--glyph-min X]

The Prism Glass census of a trail frame (docs/design/PRISM-GLASS.md section 15.1 P0.13), as JSON. It reads the frame
against its clean twin (<stem>.<frame>.clean.png or <stem>.clean.png, `image clean`; required) on the grid of its dims
sidecar (<stem>.dims.txt, --dims, or --origin and --cell), and sorts the pixels the effects changed into layers: the
caret's box (the .cursor.txt sidecar or --caret), each --pet box (px, end exclusive), stars (small marks apart from
the band), the band, and the rail/lip (the stroke on a band row's bottom edge, told from the band by colour). The
module's documentation states the heuristic. It reports:
  layers     OKLab L and C (min, median, max), the hue sectors (30 degrees each, C >= .04) and the mud counts of the
             band, rail/lip and caret pixels, each layer on its own, all of them and their core (band: 2 px inside its
             column and 3 px from glyph ink; rail/lip: each column's most chromatic pixel; caret: 2 px inside it).
             mud_spec: C < .12, or h 85-125 at L < .82, or h 40-85 at L < .62; mud_broad: C < .10 at L < .75, or
             h 60-115 at L < .75
  glyphs     the solid ink of every glyph cell the band or rail/lip touches, each pixel against its local ground as
             trail-contrast judges it: min, p1, median
  top/tail   the band's top edge (the first pixel at half its column's peak change) per column clear of ink: the
             max step and the risers (steps of 2 px or more) per band row, and over the 6 cells at the oldest end
             (away from the caret) of the caret's band row, else the first, with its whiskers (columns 1-2 px tall)
  joins      per pair of band rows: seam rows (under 5% of both rows' columns coloured), join clear rows (under 5% of
             the lower row's) and the seam (the narrowest clear run between them); cohorts: rows at least 50% coloured
  stroke     rail/lip pixels on each band row's last pixel row (rb-1) and how far up they climb; for a band row over
             a row that is not one: what hangs into the next row (pixels, depth, columns, pixels over its ink) and the
             clear rows above its ink
  pets       per --pet box, its pixels under a band row's edge that the stroke beside the box explains better
             than the band beside it does (the lip running on under the pet)
  stars      each mark, and the marks in accent position (over glyph ink across 40% of their columns or more, and
             9 px or less above it)
  caret      its fill, keyline and cut-out (the glyph under it) against the fill
  selection  filled cells (--selection's colour, else every cell ground off the page) and their pixels the trail drew
--pixels FILE writes every sorted pixel as TSV (x, y, layer, row, colour, L, C, h, core, mud_spec, mud_broad).

exit: 0; 1 a threshold failed (--mud-max: band or rail/lip core mud under either rule over N; --glyph-min: the glyph
minimum under X; each named in `failures`); 2 a usage, file or decoding error
";

/// What a pixel of the frame is to the census.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// The effects left it alone, or it is text they tinted.
    None,
    /// Inside the caret's box.
    Caret,
    /// Inside a pet's box.
    Pet,
    /// A small mark apart from the band.
    Star,
    /// A band row's band (main's bed, or Prism Glass's glass).
    Band,
    /// A band row's stroke on its bottom edge (main's rail, or Prism Glass's lip).
    RailLip,
    /// A trail pixel no layer's zone holds.
    Other,
}

impl Layer {
    /// The layer's name in the report.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Layer::None => "none",
            Layer::Caret => "caret",
            Layer::Pet => "pet",
            Layer::Star => "star",
            Layer::Band => "band",
            Layer::RailLip => "rail_lip",
            Layer::Other => "other",
        }
    }
}

/// `OkLab` L, `OkLCh` C and h (degrees in `[0, 360)`) of an sRGB colour.
#[must_use]
pub fn lch(rgb: Rgb) -> (f64, f64, f64) {
    let (l, a, b) =
        aterm_types::scheme_signature::oklab(aterm_types::Rgb::new(rgb[0], rgb[1], rgb[2]));
    (l, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0))
}

/// The spec's mud rule (§1): C < .12, or h 85-125 at L < .82 (olive), or h 40-85 at L < .62 (brown).
#[must_use]
pub fn mud_spec((l, c, h): (f64, f64, f64)) -> bool {
    c < 0.12
        || ((85.0..=125.0).contains(&h) && l < 0.82)
        || ((40.0..=85.0).contains(&h) && l < 0.62)
}

/// The broad rule: C < .10 at L < .75, or h 60-115 at L < .75.
#[must_use]
pub fn mud_broad((l, c, h): (f64, f64, f64)) -> bool {
    l < 0.75 && (c < 0.10 || (60.0..=115.0).contains(&h))
}

/// How well `over`, drawn over `under` at some coverage, explains `p`: the coverage (0 to 1) that brings it nearest
/// and how far it then still is (sRGB, Euclidean).
#[must_use]
pub fn explain(p: Rgb, over: Rgb, under: Rgb) -> (f64, f64) {
    let d: [f64; 3] = std::array::from_fn(|k| f64::from(over[k]) - f64::from(under[k]));
    let q: [f64; 3] = std::array::from_fn(|k| f64::from(p[k]) - f64::from(under[k]));
    let dd: f64 = d.iter().map(|v| v * v).sum();
    let a = if dd > 0.0 {
        (q.iter().zip(&d).map(|(q, d)| q * d).sum::<f64>() / dd).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let off: f64 = q.iter().zip(&d).map(|(q, d)| (q - a * d).powi(2)).sum();
    (a, off.sqrt())
}

/// The `OkLab` distance between two colours.
fn de(a: Rgb, b: Rgb) -> f64 {
    let rgb = |c: Rgb| aterm_types::Rgb::new(c[0], c[1], c[2]);
    aterm_types::scheme_signature::oklab_distance(rgb(a), rgb(b))
}

/// `share` of `px`, in whole pixels.
fn part(px: i64, share: f64) -> i64 {
    (px as f64 * share).round() as i64
}

/// Sorted `v`'s value at rank `⌊(n − 1) × q⌋` (`q` 0.5 the lower median, 0.01 the first percentile).
fn rank(sorted: &[f64], q: f64) -> Option<f64> {
    let n = sorted.len();
    (n > 0).then(|| sorted[((n - 1) as f64 * q) as usize])
}

/// A small connected group of trail pixels: a star.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Its box, end exclusive.
    pub at: PxBox,
    /// Its pixels.
    pub px: i64,
}

/// A frame and its twin, every pixel sorted into a [`Layer`].
pub struct Scene<'a> {
    frame: &'a Image,
    clean: &'a Image,
    grid: Grid,
    page: Rgb,
    w: i64,
    h: i64,
    /// Each cell's ground in the twin (its commonest colour), row-major.
    grounds: Vec<Rgb>,
    /// Each cell's glyph ink box in the twin, row-major.
    ink_boxes: Vec<Option<PxBox>>,
    /// The pixels the effects changed.
    changed: Vec<bool>,
    /// The pixels that are their cell's ground in the twin (off the grid: the page).
    ground: Vec<bool>,
    /// The pixels within [`INK_CLEAR`] of the twin's glyph ink.
    near_ink: Vec<bool>,
    layer: Vec<Layer>,
    /// The band row a band or rail/lip pixel belongs to.
    owner: Vec<i64>,
    /// The band rows, by grid row.
    band_rows: Vec<bool>,
    marks: Vec<Mark>,
}

impl<'a> Scene<'a> {
    /// `frame` against its twin `clean` (the same size) on `grid`, each pixel sorted as the module says, with
    /// `caret` and `pets` taking every changed pixel in them.
    #[must_use]
    pub fn new(
        frame: &'a Image,
        clean: &'a Image,
        grid: Grid,
        page: Rgb,
        caret: Option<PxBox>,
        pets: &[PxBox],
    ) -> Scene<'a> {
        let (w, h) = (
            i64::try_from(frame.width).unwrap_or(0),
            i64::try_from(frame.height).unwrap_or(0),
        );
        let n = frame.width * frame.height;
        let mut s = Scene {
            frame,
            clean,
            grid,
            page,
            w,
            h,
            grounds: Vec::new(),
            ink_boxes: Vec::new(),
            changed: vec![false; n],
            ground: vec![true; n],
            near_ink: vec![false; n],
            layer: vec![Layer::None; n],
            owner: vec![-1; n],
            band_rows: Vec::new(),
            marks: Vec::new(),
        };
        for r in 0..grid.rows {
            for c in 0..grid.cols {
                let b = grid.cell_box(r, c, 1);
                s.grounds.push(mode(clean.region(b)).unwrap_or(page));
            }
        }
        let mut ink = vec![false; n];
        for y in 0..h {
            for x in 0..w {
                let i = s.at(x, y);
                let (f, c) = (s.f(x, y), s.c(x, y));
                s.changed[i] = (0..3).any(|k| f[k].abs_diff(c[k]) > DIFF_DELTA);
                if let Some(cell) = s.cell(x, y) {
                    s.ground[i] = c == s.grounds[cell];
                    ink[i] = !s.ground[i];
                }
            }
        }
        s.ink_boxes = vec![None; s.grounds.len()];
        for y in 0..h {
            for x in 0..w {
                if let Some(cell) = s.cell(x, y).filter(|_| ink[s.at(x, y)]) {
                    let b = s.ink_boxes[cell].get_or_insert(PxBox {
                        x0: x,
                        y0: y,
                        x1: x + 1,
                        y1: y + 1,
                    });
                    *b = PxBox {
                        x0: b.x0.min(x),
                        y0: b.y0.min(y),
                        x1: b.x1.max(x + 1),
                        y1: b.y1.max(y + 1),
                    };
                }
            }
        }
        s.near_ink = s.dilate(&ink, INK_CLEAR);
        s.sort(caret, pets);
        s
    }

    fn at(&self, x: i64, y: i64) -> usize {
        usize::try_from(y * self.w + x).unwrap_or(0)
    }

    fn holds(&self, x: i64, y: i64) -> bool {
        (0..self.w).contains(&x) && (0..self.h).contains(&y)
    }

    fn f(&self, x: i64, y: i64) -> Rgb {
        self.frame.at(
            usize::try_from(x).unwrap_or(0),
            usize::try_from(y).unwrap_or(0),
        )
    }

    fn c(&self, x: i64, y: i64) -> Rgb {
        self.clean.at(
            usize::try_from(x).unwrap_or(0),
            usize::try_from(y).unwrap_or(0),
        )
    }

    /// The cell index of a pixel on the grid.
    fn cell(&self, x: i64, y: i64) -> Option<usize> {
        let g = &self.grid;
        let (col, row) = ((x - g.ox).div_euclid(g.cw), (y - g.oy).div_euclid(g.ch));
        (x >= g.ox && y >= g.oy && g.holds(row, col))
            .then(|| usize::try_from(row * g.cols + col).unwrap_or(0))
    }

    fn row_top(&self, r: i64) -> i64 {
        self.grid.oy + r * self.grid.ch
    }

    /// The grid's pixel columns.
    fn columns(&self) -> std::ops::Range<i64> {
        self.grid.ox..(self.grid.ox + self.grid.cols * self.grid.cw).min(self.w)
    }

    /// `mask` grown by `reach` px (Chebyshev).
    fn dilate(&self, mask: &[bool], reach: i64) -> Vec<bool> {
        let mut across = vec![false; mask.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                across[self.at(x, y)] = ((x - reach).max(0)..=(x + reach).min(self.w - 1))
                    .any(|xx| mask[self.at(xx, y)]);
            }
        }
        let mut out = vec![false; mask.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                out[self.at(x, y)] = ((y - reach).max(0)..=(y + reach).min(self.h - 1))
                    .any(|yy| across[self.at(x, yy)]);
            }
        }
        out
    }

    /// A trail pixel no layer has taken yet.
    fn open(&self, x: i64, y: i64) -> bool {
        let i = self.at(x, y);
        self.holds(x, y) && self.layer[i] == Layer::None && self.changed[i] && self.ground[i]
    }

    fn claim(&mut self, x: i64, y: i64, layer: Layer, row: i64) {
        let i = self.at(x, y);
        self.layer[i] = layer;
        self.owner[i] = row;
    }

    /// A pixel's layer, and the band row that owns it (`-1` for none).
    #[cfg(test)]
    #[must_use]
    pub fn layer_at(&self, x: i64, y: i64) -> (Layer, i64) {
        let i = self.at(x, y);
        (self.layer[i], self.owner[i])
    }

    /// The band rows.
    #[must_use]
    pub fn band_rows(&self) -> Vec<i64> {
        (0..self.grid.rows)
            .filter(|&r| self.is_band_row(r))
            .collect()
    }

    fn is_band_row(&self, r: i64) -> bool {
        usize::try_from(r)
            .ok()
            .and_then(|r| self.band_rows.get(r))
            .copied()
            .unwrap_or(false)
    }

    /// Sort every pixel (the module's steps).
    fn sort(&mut self, caret: Option<PxBox>, pets: &[PxBox]) {
        for y in 0..self.h {
            for x in 0..self.w {
                let i = self.at(x, y);
                if !self.changed[i] {
                    continue;
                }
                if caret.is_some_and(|b| b.holds(x, y)) {
                    self.layer[i] = Layer::Caret;
                } else if pets.iter().any(|b| b.holds(x, y)) {
                    self.layer[i] = Layer::Pet;
                }
            }
        }
        self.take_marks();
        let (cw, ch) = (self.grid.cw, self.grid.ch);
        let cols: Vec<i64> = self.columns().collect();
        self.band_rows = (0..self.grid.rows)
            .map(|r| {
                let (y0, y1) = self.probe(r);
                let hit = cols
                    .iter()
                    .filter(|&&x| (y0..y1).any(|y| self.open(x, y)))
                    .count();
                i64::try_from(hit).unwrap_or(0) >= cw
            })
            .collect();
        // The band's colour per column: the probe's median, or where a letter fills the probe, the median of the
        // row above the stroke's reach.
        let refs: Vec<Vec<Option<Rgb>>> = (0..self.grid.rows)
            .map(|r| {
                if !self.is_band_row(r) {
                    return Vec::new();
                }
                let (y0, y1) = self.probe(r);
                let (top, above) = (self.row_top(r), self.row_top(r) + ch - part(ch, RISE));
                let median_of = |x: i64, ys: std::ops::Range<i64>| {
                    let px: Vec<Rgb> = ys
                        .filter(|&y| self.open(x, y))
                        .map(|y| self.f(x, y))
                        .collect();
                    median(&px)
                };
                cols.iter()
                    .map(|&x| median_of(x, y0..y1).or_else(|| median_of(x, top..above)))
                    .collect()
            })
            .collect();
        let band_ref = |r: i64, i: usize| -> Option<Rgb> {
            usize::try_from(r)
                .ok()
                .and_then(|r| refs.get(r))
                .and_then(|row| row.get(i))
                .copied()
                .flatten()
        };
        let (rise, hang, reach_up) = (part(ch, RISE), part(ch, HANG), part(ch, REACH_UP));
        for r in 0..self.grid.rows {
            if !self.is_band_row(r) {
                continue;
            }
            let (top, rb) = (self.row_top(r), self.row_top(r) + ch);
            let next_band = self.is_band_row(r + 1);
            for (i, &x) in cols.iter().enumerate() {
                let band = band_ref(r, i);
                let next = if next_band { band_ref(r + 1, i) } else { None };
                let mut below = Vec::new();
                for y in rb..(rb + hang).min(self.h) {
                    if self.open(x, y) {
                        below.push(y);
                    } else if next_band {
                        break;
                    }
                }
                let stroke = below
                    .iter()
                    .map(|&y| self.f(x, y))
                    .max_by(|a, b| lch(*a).1.total_cmp(&lch(*b).1));
                let distinct = stroke.filter(|&s| {
                    [band, next]
                        .iter()
                        .flatten()
                        .all(|&b| de(s, b) >= STROKE_DE)
                });
                if let Some(s) = distinct {
                    for y in (rb - rise..rb).rev() {
                        if !self.open(x, y) {
                            break;
                        }
                        let (p, u) = (self.f(x, y), self.c(x, y));
                        if band.is_some_and(|b| explain(p, b, u).1 < explain(p, s, u).1) {
                            break;
                        }
                        self.claim(x, y, Layer::RailLip, r);
                    }
                    for &y in &below {
                        let (p, u) = (self.f(x, y), self.c(x, y));
                        let by_band = [band, next]
                            .iter()
                            .flatten()
                            .map(|&b| explain(p, b, u).1)
                            .reduce(f64::min);
                        if by_band.is_some_and(|d| d < explain(p, s, u).1) {
                            if next_band {
                                break;
                            }
                            self.claim(x, y, Layer::Band, r);
                        } else {
                            self.claim(x, y, Layer::RailLip, r);
                        }
                    }
                } else if !next_band {
                    for &y in &below {
                        self.claim(x, y, Layer::Band, r);
                    }
                }
                let from = if self.is_band_row(r - 1) {
                    top
                } else {
                    (top - reach_up).max(0)
                };
                for y in from..rb.min(self.h) {
                    if self.open(x, y) {
                        self.claim(x, y, Layer::Band, r);
                    }
                }
            }
        }
        for y in 0..self.h {
            for x in 0..self.w {
                if self.open(x, y) {
                    self.claim(x, y, Layer::Other, -1);
                }
            }
        }
    }

    /// Row `r`'s probe, its first and its end pixel row.
    fn probe(&self, r: i64) -> (i64, i64) {
        let top = self.row_top(r);
        (
            top + part(self.grid.ch, PROBE.0),
            top + part(self.grid.ch, PROBE.1),
        )
    }

    /// Find the marks (the module's step 2) and take their pixels.
    fn take_marks(&mut self) {
        let (cw, ch) = (self.grid.cw, self.grid.ch);
        let max_area = part(cw * ch, MARK_AREA);
        let (max_w, max_h) = (part(cw, MARK_W), part(ch, MARK_H));
        let mut seen = vec![false; self.layer.len()];
        let mut stack = Vec::new();
        let mut group = Vec::new();
        for y0 in 0..self.h {
            for x0 in 0..self.w {
                if seen[self.at(x0, y0)] || !self.open(x0, y0) {
                    continue;
                }
                seen[self.at(x0, y0)] = true;
                stack.push((x0, y0));
                group.clear();
                let mut b = PxBox {
                    x0,
                    y0,
                    x1: x0 + 1,
                    y1: y0 + 1,
                };
                while let Some((x, y)) = stack.pop() {
                    group.push((x, y));
                    b = PxBox {
                        x0: b.x0.min(x),
                        y0: b.y0.min(y),
                        x1: b.x1.max(x + 1),
                        y1: b.y1.max(y + 1),
                    };
                    for (dx, dy) in [
                        (-1, -1),
                        (0, -1),
                        (1, -1),
                        (-1, 0),
                        (1, 0),
                        (-1, 1),
                        (0, 1),
                        (1, 1),
                    ] {
                        let (xx, yy) = (x + dx, y + dy);
                        if self.holds(xx, yy) && !seen[self.at(xx, yy)] && self.open(xx, yy) {
                            seen[self.at(xx, yy)] = true;
                            stack.push((xx, yy));
                        }
                    }
                }
                let px = i64::try_from(group.len()).unwrap_or(i64::MAX);
                let small = px <= max_area && b.x1 - b.x0 <= max_w && b.y1 - b.y0 <= max_h;
                if small && !self.inside_ink(b) {
                    for &(x, y) in &group {
                        let i = self.at(x, y);
                        self.layer[i] = Layer::Star;
                    }
                    self.marks.push(Mark { at: b, px });
                }
            }
        }
    }

    /// Whether `b` lies inside the twin's glyph ink box of the cells it touches (a counter).
    fn inside_ink(&self, b: PxBox) -> bool {
        let mut ink: Option<PxBox> = None;
        for (x, y) in [
            (b.x0, b.y0),
            (b.x1 - 1, b.y0),
            (b.x0, b.y1 - 1),
            (b.x1 - 1, b.y1 - 1),
        ] {
            let Some(i) = self.cell(x, y).and_then(|c| self.ink_boxes[c]) else {
                continue;
            };
            ink = Some(ink.map_or(i, |u| PxBox {
                x0: u.x0.min(i.x0),
                y0: u.y0.min(i.y0),
                x1: u.x1.max(i.x1),
                y1: u.y1.max(i.y1),
            }));
        }
        ink.is_some_and(|u| u.x0 <= b.x0 && b.x1 <= u.x1 && u.y0 <= b.y0 && b.y1 <= u.y1)
    }

    /// The core pixels (as the help says, per layer).
    fn core(&self) -> Vec<bool> {
        let mut core = vec![false; self.layer.len()];
        let mut best: HashMap<(i64, i64), (f64, usize)> = HashMap::new();
        for y in 0..self.h {
            for x in 0..self.w {
                let i = self.at(x, y);
                match self.layer[i] {
                    Layer::Band => {
                        let r = self.owner[i];
                        core[i] = !self.near_ink[i]
                            && (-EDGE_PX..=EDGE_PX).all(|d| {
                                self.holds(x, y + d)
                                    && self.layer[self.at(x, y + d)] == Layer::Band
                                    && self.owner[self.at(x, y + d)] == r
                            });
                    }
                    Layer::RailLip => {
                        let c = lch(self.f(x, y)).1;
                        let slot = best.entry((self.owner[i], x)).or_insert((c, i));
                        if c > slot.0 {
                            *slot = (c, i);
                        }
                    }
                    Layer::Caret => {
                        core[i] = (-KEYLINE_PX..=KEYLINE_PX).all(|dy| {
                            (-KEYLINE_PX..=KEYLINE_PX).all(|dx| {
                                let (xx, yy) = (x + dx, y + dy);
                                self.holds(xx, yy) && self.caret_px(xx, yy)
                            })
                        });
                    }
                    _ => {}
                }
            }
        }
        for (_, i) in best.into_values() {
            core[i] = true;
        }
        core
    }

    /// A caret pixel the census reads colour from: changed, and ground in the twin (not the cut-out).
    fn caret_px(&self, x: i64, y: i64) -> bool {
        let i = self.at(x, y);
        self.layer[i] == Layer::Caret && self.ground[i]
    }

    /// The census of the band, rail/lip and caret layers.
    fn layers(&self, core: &[bool]) -> J {
        let mut tallies: [(Tally, Tally); 3] = Default::default();
        for y in 0..self.h {
            for x in 0..self.w {
                let i = self.at(x, y);
                let slot = match self.layer[i] {
                    Layer::Band => 0,
                    Layer::RailLip => 1,
                    Layer::Caret if self.ground[i] => 2,
                    _ => continue,
                };
                let p = self.f(x, y);
                tallies[slot].0.add(p);
                if core[i] {
                    tallies[slot].1.add(p);
                }
            }
        }
        let [band, rail, caret] = tallies;
        let both =
            |(all, core): (Tally, Tally)| J::obj([("all", all.report()), ("core", core.report())]);
        J::obj([
            ("band", both(band)),
            ("rail_lip", both(rail)),
            ("caret", both(caret)),
        ])
    }

    /// The glyph cells the band or rail/lip touches, their solid ink judged as `trail-contrast` judges it; and the
    /// minimum.
    fn glyphs(&self, caret: Option<(i64, i64)>) -> (J, Option<f64>) {
        let mut touched = vec![false; self.grounds.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                if matches!(self.layer[self.at(x, y)], Layer::Band | Layer::RailLip)
                    && let Some(c) = self.cell(x, y)
                {
                    touched[c] = true;
                }
            }
        }
        let mut ratios = Vec::new();
        let (mut cells, mut worst): (i64, Option<(f64, i64, i64)>) = (0, None);
        for (i, _) in touched.iter().enumerate().filter(|(_, t)| **t) {
            let i = i64::try_from(i).unwrap_or(0);
            let (row, col) = (i / self.grid.cols, i % self.grid.cols);
            if caret == Some((row, col)) {
                continue;
            }
            let b = self.grid.cell_box(row, col, 1);
            let clean_px = self.clean.region(b);
            let ground = mode(clean_px.iter().copied()).unwrap_or(self.page);
            let extreme = most_contrasting(&clean_px, ground).unwrap_or(ground);
            if contrast(extreme, ground) < BLANK_BELOW {
                continue;
            }
            let Some(judged) = solid_ink_ratios(self.frame, self.clean, b, ground, extreme) else {
                continue;
            };
            cells += 1;
            for (_, r) in judged {
                if worst.is_none_or(|w| r < w.0) {
                    worst = Some((r, row, col));
                }
                ratios.push(r);
            }
        }
        ratios.sort_by(f64::total_cmp);
        let at = |q: f64| rank(&ratios, q).map_or(J::Null, |v| J::Float(round_to(v, 2)));
        let min = ratios.first().copied();
        (
            J::obj([
                ("cells", J::Int(cells)),
                ("px", J::Int(i64::try_from(ratios.len()).unwrap_or(0))),
                ("min", at(0.0)),
                ("p1", at(0.01)),
                ("median", at(0.5)),
                (
                    "worst",
                    worst.map_or(J::Null, |(r, row, col)| {
                        J::obj([
                            ("row", J::Int(row)),
                            ("col", J::Int(col)),
                            ("contrast", J::Float(round_to(r, 2))),
                        ])
                    }),
                ),
            ]),
            min,
        )
    }

    /// Band row `r`'s band per column: its top and bottom (the first and last pixel at half the column's peak change)
    /// and whether the top keeps [`INK_CLEAR`] from glyph ink, by column of [`Scene::columns`].
    fn band_edges(&self, r: i64) -> Vec<Option<(i64, i64, bool)>> {
        let ch = self.grid.ch;
        let (from, to) = (
            (self.row_top(r) - part(ch, REACH_UP)).max(0),
            (self.row_top(r) + ch + part(ch, HANG)).min(self.h),
        );
        self.columns()
            .map(|x| {
                let ys: Vec<(i64, u8)> = (from..to)
                    .filter(|&y| {
                        let i = self.at(x, y);
                        self.layer[i] == Layer::Band && self.owner[i] == r
                    })
                    .map(|y| {
                        let (f, c) = (self.f(x, y), self.c(x, y));
                        (y, (0..3).map(|k| f[k].abs_diff(c[k])).max().unwrap_or(0))
                    })
                    .collect();
                let peak = ys.iter().map(|p| p.1).max()?;
                let half: Vec<i64> = ys
                    .iter()
                    .filter(|p| u16::from(p.1) * 2 >= u16::from(peak))
                    .map(|p| p.0)
                    .collect();
                let (top, bottom) = (*half.first()?, *half.last()?);
                Some((top, bottom, !self.near_ink[self.at(x, top)]))
            })
            .collect()
    }

    /// The band's top edge per band row, and its oldest end's tail: on the caret's row when that is a band row (else
    /// the first band row), the [`TAIL_CELLS`] at the end away from the caret.
    fn tops(&self, caret: Option<PxBox>) -> (J, J) {
        let steps = |edges: &[Option<(i64, i64, bool)>]| -> (i64, i64, i64) {
            let read: Vec<Option<i64>> = edges
                .iter()
                .map(|e| e.filter(|e| e.2).map(|e| e.0))
                .collect();
            let (mut max_step, mut risers) = (0, 0);
            for pair in read.windows(2) {
                if let [Some(a), Some(b)] = pair {
                    let step = (a - b).abs();
                    max_step = max_step.max(step);
                    risers += i64::from(step >= RISER_PX);
                }
            }
            let cols = read.iter().flatten().count();
            (i64::try_from(cols).unwrap_or(0), max_step, risers)
        };
        let ch = self.grid.ch;
        let on_caret =
            |r: i64| caret.is_some_and(|c| c.y0 < self.row_top(r) + ch && c.y1 > self.row_top(r));
        let band_rows = self.band_rows();
        let tail_row = band_rows
            .iter()
            .copied()
            .find(|&r| on_caret(r))
            .or_else(|| band_rows.first().copied());
        let mut rows = Vec::new();
        let mut tail = J::Null;
        for r in band_rows {
            let edges = self.band_edges(r);
            let (cols, max_step, risers) = steps(&edges);
            rows.push(J::obj([
                ("row", J::Int(r)),
                ("cols", J::Int(cols)),
                ("max_step", J::Int(max_step)),
                ("risers", J::Int(risers)),
            ]));
            if Some(r) != tail_row {
                continue;
            }
            let lit: Vec<usize> = (0..edges.len()).filter(|&i| edges[i].is_some()).collect();
            let (Some(&first), Some(&last)) = (lit.first(), lit.last()) else {
                continue;
            };
            let x_of = |i: usize| self.grid.ox + i64::try_from(i).unwrap_or(0);
            let right = caret.is_some_and(|c| {
                on_caret(r)
                    && ((c.x0 + c.x1) / 2 - x_of(first)).abs()
                        < (x_of(last) - (c.x0 + c.x1) / 2).abs()
            });
            let span = usize::try_from(TAIL_CELLS * self.grid.cw).unwrap_or(0);
            let range = if right {
                (last + 1).saturating_sub(span).max(first)..last + 1
            } else {
                first..(first + span).min(last + 1)
            };
            let tail_edges = &edges[range];
            let (cols, max_step, risers) = steps(tail_edges);
            let whiskers = tail_edges
                .iter()
                .flatten()
                .filter(|e| e.1 - e.0 < WHISKER_PX)
                .count();
            tail = J::obj([
                ("row", J::Int(r)),
                ("end", J::s(if right { "right" } else { "left" })),
                ("cells", J::Int(TAIL_CELLS)),
                ("cols", J::Int(cols)),
                ("max_step", J::Int(max_step)),
                ("risers", J::Int(risers)),
                ("whiskers", J::Int(i64::try_from(whiskers).unwrap_or(0))),
            ]);
        }
        (J::Arr(rows), tail)
    }

    /// A pixel the trail coloured, for the seam measures: changed, and in no caret, pet or star.
    fn coloured(&self, x: i64, y: i64) -> bool {
        let i = self.at(x, y);
        self.changed[i] && !matches!(self.layer[i], Layer::Caret | Layer::Pet | Layer::Star)
    }

    /// The columns (as pixel x) where band row `r` has band.
    fn band_span(&self, r: i64) -> Vec<i64> {
        let ch = self.grid.ch;
        let (from, to) = (
            (self.row_top(r) - part(ch, REACH_UP)).max(0),
            (self.row_top(r) + ch + part(ch, HANG)).min(self.h),
        );
        self.columns()
            .filter(|&x| {
                (from..to).any(|y| {
                    let i = self.at(x, y);
                    self.layer[i] == Layer::Band && self.owner[i] == r
                })
            })
            .collect()
    }

    /// The share of `span`'s columns coloured on pixel row `y`.
    fn share(&self, span: &[i64], y: i64) -> f64 {
        let hit = span.iter().filter(|&&x| self.coloured(x, y)).count();
        hit as f64 / span.len().max(1) as f64
    }

    /// The seams between neighbouring band rows, and each wrapped cohort's coloured rows.
    fn joins(&self) -> (J, J) {
        let ch = self.grid.ch;
        let rows = self.band_rows();
        let mut joins = Vec::new();
        for pair in rows.windows(2) {
            let [r, s] = [pair[0], pair[1]];
            if s != r + 1 {
                continue;
            }
            let (upper, lower) = (self.band_span(r), self.band_span(s));
            let mut union = upper.clone();
            union.extend(lower.iter().copied());
            union.sort_unstable();
            union.dedup();
            let (y0, y1) = (
                self.row_top(r) + ch / 2,
                (self.row_top(s) + ch / 2).min(self.h),
            );
            let seam_rows = (y0..y1)
                .filter(|&y| self.share(&union, y) < SEAM_SHARE)
                .count();
            let clear_rows = (y0..y1)
                .filter(|&y| self.share(&lower, y) < SEAM_SHARE)
                .count();
            let mut gaps = Vec::new();
            for &x in &lower {
                let owned = |y: i64, row: i64, layers: &[Layer]| {
                    let i = self.at(x, y);
                    self.owner[i] == row && layers.contains(&self.layer[i])
                };
                let bottom = (y0..y1)
                    .rev()
                    .find(|&y| owned(y, r, &[Layer::Band, Layer::RailLip]));
                let top = (y0..y1).find(|&y| owned(y, s, &[Layer::Band]));
                if let (Some(b), Some(t)) = (bottom, top) {
                    gaps.push(t - b - 1);
                }
            }
            joins.push(J::obj([
                ("rows", J::Arr(vec![J::Int(r), J::Int(s)])),
                ("seam_rows", J::Int(i64::try_from(seam_rows).unwrap_or(0))),
                (
                    "join_clear_rows",
                    J::Int(i64::try_from(clear_rows).unwrap_or(0)),
                ),
                (
                    "seam_px",
                    gaps.iter().copied().min().map_or(J::Null, J::Int),
                ),
                ("seam_cols", J::Int(i64::try_from(gaps.len()).unwrap_or(0))),
            ]));
        }
        let mut cohorts = Vec::new();
        let mut at = 0;
        while at < rows.len() {
            let mut end = at;
            while end + 1 < rows.len() && rows[end + 1] == rows[end] + 1 {
                end += 1;
            }
            if end > at {
                let mut span: Vec<i64> = (at..=end).flat_map(|i| self.band_span(rows[i])).collect();
                span.sort_unstable();
                span.dedup();
                let (y0, y1) = (
                    self.row_top(rows[at]),
                    (self.row_top(rows[end]) + ch).min(self.h),
                );
                let half = (y0..y1)
                    .filter(|&y| self.share(&span, y) >= COLOURED_SHARE)
                    .count();
                cohorts.push(J::obj([
                    ("rows", J::Arr(vec![J::Int(rows[at]), J::Int(rows[end])])),
                    ("coloured_rows", J::Int(i64::try_from(half).unwrap_or(0))),
                    ("px_rows", J::Int(y1 - y0)),
                ]));
            }
            at = end + 1;
        }
        (J::Arr(joins), J::Arr(cohorts))
    }

    /// The stroke on each band row's last pixel row, and what hangs into a next row that is no band row.
    fn stroke(&self) -> J {
        let ch = self.grid.ch;
        let hang = part(ch, HANG);
        let (mut on_last, mut rise) = (0i64, 0i64);
        for y in 0..self.h {
            for x in 0..self.w {
                let i = self.at(x, y);
                if self.layer[i] != Layer::RailLip {
                    continue;
                }
                let rb = self.row_top(self.owner[i]) + ch;
                on_last += i64::from(y == rb - 1);
                rise = rise.max(rb - y);
            }
        }
        let mut rows = Vec::new();
        for r in self.band_rows() {
            if self.is_band_row(r + 1) {
                continue;
            }
            let (top, rb) = (self.row_top(r), self.row_top(r) + ch);
            let (mut px, mut depth, mut cols, mut over_ink) = (0i64, 0i64, 0i64, 0i64);
            let mut gaps = Vec::new();
            for x in self.columns() {
                let mut hung = false;
                let mut bottom = None;
                for y in (top - part(ch, REACH_UP)).max(0)..(rb + hang).min(self.h) {
                    let i = self.at(x, y);
                    let own =
                        self.owner[i] == r && matches!(self.layer[i], Layer::Band | Layer::RailLip);
                    let on_ink = y >= rb
                        && self.changed[i]
                        && !self.ground[i]
                        && self.layer[i] == Layer::None;
                    if y >= rb && own {
                        px += 1;
                        depth = depth.max(y - rb + 1);
                        hung = true;
                    }
                    over_ink += i64::from(on_ink);
                    if own || on_ink {
                        bottom = Some(y);
                    }
                }
                cols += i64::from(hung);
                let ink_top = (rb..(rb + ch).min(self.h))
                    .find(|&y| self.cell(x, y).is_some() && !self.ground[self.at(x, y)]);
                if r + 1 < self.grid.rows
                    && let (Some(b), Some(t)) = (bottom, ink_top)
                {
                    gaps.push(t - b - 1);
                }
            }
            rows.push(J::obj([
                ("row", J::Int(r)),
                ("hang_px", J::Int(px)),
                ("hang_depth", J::Int(depth)),
                ("hang_cols", J::Int(cols)),
                ("over_next_ink_px", J::Int(over_ink)),
                (
                    "next_ink",
                    J::obj([
                        ("cols", J::Int(i64::try_from(gaps.len()).unwrap_or(0))),
                        (
                            "clear_min",
                            gaps.iter().copied().min().map_or(J::Null, J::Int),
                        ),
                    ]),
                ),
            ]));
        }
        J::obj([
            ("rb_minus_1_px", J::Int(on_last)),
            ("rise_max", J::Int(rise)),
            ("rows", J::Arr(rows)),
        ])
    }

    /// The colours on pixel row `y` nearest beside `b` (within [`LIP_REACH_CELLS`], one each side) whose pixel `keep`
    /// takes.
    fn beside(&self, b: PxBox, y: i64, keep: impl Fn(Layer, i64) -> bool) -> Vec<Rgb> {
        let reach = LIP_REACH_CELLS * self.grid.cw;
        let take = |x: i64| {
            let i = self.at(x, y);
            keep(self.layer[i], self.owner[i]).then(|| self.f(x, y))
        };
        let left = ((b.x0 - reach).max(0)..b.x0.min(self.w))
            .rev()
            .find_map(take);
        let right = (b.x1.max(0)..(b.x1 + reach).min(self.w)).find_map(take);
        [left, right].into_iter().flatten().collect()
    }

    /// Per pet box, its pixels under a band row's bottom edge (where the lip lies) that the stroke beside the box on
    /// the same pixel row explains, at least half way and better than the band beside it does: the lip running on
    /// under the pet's transparent pixels.
    fn pets(&self, pets: &[PxBox]) -> J {
        let ch = self.grid.ch;
        let out = pets.iter().map(|b| {
            let (mut px, mut stroke_rows) = (0i64, 0i64);
            let mut lit_cols = vec![false; usize::try_from((b.x1 - b.x0).max(0)).unwrap_or(0)];
            for r in self.band_rows() {
                let rb = self.row_top(r) + ch;
                for y in rb.max(b.y0).max(0)..(rb + part(ch, HANG)).min(b.y1).min(self.h) {
                    let strokes = self.beside(*b, y, |l, o| l == Layer::RailLip && o == r);
                    if strokes.is_empty() {
                        continue;
                    }
                    stroke_rows += 1;
                    let bands = self.beside(*b, y, |l, _| l == Layer::Band);
                    for x in b.x0.max(0)..b.x1.min(self.w) {
                        let i = self.at(x, y);
                        if self.layer[i] != Layer::Pet || !self.ground[i] {
                            continue;
                        }
                        let (p, u) = (self.f(x, y), self.c(x, y));
                        let by_stroke = strokes
                            .iter()
                            .map(|&s| explain(p, s, u))
                            .filter(|e| e.0 >= 0.5)
                            .map(|e| e.1)
                            .reduce(f64::min);
                        let by_band = bands.iter().map(|&s| explain(p, s, u).1).reduce(f64::min);
                        if by_stroke
                            .is_some_and(|s| s <= LIP_MATCH && by_band.is_none_or(|d| s < d))
                        {
                            px += 1;
                            if let Some(slot) = usize::try_from(x - b.x0)
                                .ok()
                                .and_then(|c| lit_cols.get_mut(c))
                            {
                                *slot = true;
                            }
                        }
                    }
                }
            }
            let cols = lit_cols.iter().filter(|c| **c).count();
            J::obj([
                (
                    "box",
                    J::Arr(vec![J::Int(b.x0), J::Int(b.y0), J::Int(b.x1), J::Int(b.y1)]),
                ),
                ("stroke_rows", J::Int(stroke_rows)),
                ("lip_px", J::Int(px)),
                ("lip_cols", J::Int(i64::try_from(cols).unwrap_or(0))),
            ])
        });
        J::Arr(out.collect())
    }

    /// Each mark, and how many are in accent position.
    fn stars(&self) -> J {
        let mut accent = 0;
        let marks: Vec<J> = self
            .marks
            .iter()
            .map(|m| {
                let bottom = m.at.y1 - 1;
                let tops: Vec<i64> = (m.at.x0..m.at.x1)
                    .filter_map(|x| {
                        (bottom + 1..(bottom + 1 + self.grid.ch).min(self.h))
                            .find(|&y| self.cell(x, y).is_some() && !self.ground[self.at(x, y)])
                    })
                    .collect();
                let over = tops.len() as f64 / (m.at.x1 - m.at.x0).max(1) as f64;
                let gap = tops.iter().min().map(|t| t - bottom - 1);
                let is_accent = over >= ACCENT_SHARE && gap.is_some_and(|g| g <= ACCENT_GAP_PX);
                accent += i64::from(is_accent);
                J::obj([
                    (
                        "box",
                        J::Arr(vec![
                            J::Int(m.at.x0),
                            J::Int(m.at.y0),
                            J::Int(m.at.x1),
                            J::Int(m.at.y1),
                        ]),
                    ),
                    ("px", J::Int(m.px)),
                    ("over_ink", J::Float(round_to(over, 2))),
                    ("gap", gap.map_or(J::Null, J::Int)),
                    ("accent", J::Bool(is_accent)),
                ])
            })
            .collect();
        J::obj([
            ("count", J::Int(i64::try_from(marks.len()).unwrap_or(0))),
            ("accent", J::Int(accent)),
            ("marks", J::Arr(marks)),
        ])
    }

    /// The caret at `cell` drawn in `b`: its fill, keyline and cut-out.
    fn caret(&self, cell: (i64, i64), b: PxBox) -> J {
        let mut out = J::obj([
            ("row", J::Int(cell.0)),
            ("col", J::Int(cell.1)),
            (
                "box",
                J::Arr(vec![J::Int(b.x0), J::Int(b.y0), J::Int(b.x1), J::Int(b.y1)]),
            ),
        ]);
        let (mut inner, mut ring) = (Vec::new(), Vec::new());
        for y in b.y0.max(0)..b.y1.min(self.h) {
            for x in b.x0.max(0)..b.x1.min(self.w) {
                if !self.caret_px(x, y) {
                    continue;
                }
                let edge = (x - b.x0).min(b.x1 - 1 - x).min(y - b.y0).min(b.y1 - 1 - y);
                if edge < KEYLINE_PX {
                    ring.push(self.f(x, y));
                } else {
                    inner.push(self.f(x, y));
                }
            }
        }
        let Some(fill) = mode(inner).or_else(|| mode(ring.iter().copied())) else {
            out.set("drawn", J::Bool(false));
            return out;
        };
        out.set("drawn", J::Bool(true));
        out.set("fill", J::s(hex(fill)));
        out.set(
            "fill_vs_page",
            J::Float(round_to(contrast(fill, self.page), 2)),
        );
        match mode(ring) {
            Some(k) => {
                out.set("keyline", J::s(hex(k)));
                out.set(
                    "keyline_vs_page",
                    J::Float(round_to(contrast(k, self.page), 2)),
                );
            }
            None => out.set("keyline", J::Null),
        }
        let cb = self.grid.cell_box(cell.0, cell.1, 1);
        let clean_px = self.clean.region(cb);
        let ground = mode(clean_px.iter().copied()).unwrap_or(self.page);
        let extreme = most_contrasting(&clean_px, ground).unwrap_or(ground);
        if contrast(extreme, ground) < BLANK_BELOW {
            out.set("cut_out", J::Null);
            return out;
        }
        let solid = SOLID_SHARE * contrast(extreme, ground);
        let ink: Vec<Rgb> = self
            .frame
            .region(cb)
            .into_iter()
            .zip(&clean_px)
            .filter(|&(_, &c)| contrast(c, ground) >= solid)
            .map(|(p, _)| p)
            .collect();
        let Some(cut) = median(&ink) else {
            out.set("cut_out", J::Null);
            return out;
        };
        out.set("cut_out", J::s(hex(cut)));
        out.set(
            "cut_out_contrast",
            J::Float(round_to(contrast(cut, fill), 2)),
        );
        let mut local: Vec<f64> = solid_ink_ratios(self.frame, self.clean, cb, ground, extreme)
            .unwrap_or_default()
            .into_iter()
            .map(|(_, r)| r)
            .collect();
        local.sort_by(f64::total_cmp);
        let at = |q: f64| rank(&local, q).map_or(J::Null, |v| J::Float(round_to(v, 2)));
        out.set(
            "cut_out_local",
            J::obj([("min", at(0.0)), ("median", at(0.5))]),
        );
        out
    }

    /// The filled cells (`colour`'s, else every cell ground off the page) and their pixels the trail drew.
    fn selection(&self, colour: Option<Rgb>) -> J {
        let (mut cells, mut px, mut drawn, mut drawn_cells) = (0i64, 0i64, 0i64, 0i64);
        for (i, &g) in self.grounds.iter().enumerate() {
            if g == self.page || colour.is_some_and(|c| c != g) {
                continue;
            }
            cells += 1;
            let i = i64::try_from(i).unwrap_or(0);
            let b = self
                .grid
                .cell_box(i / self.grid.cols, i % self.grid.cols, 1);
            let mut hit = false;
            for y in b.y0..b.y1.min(self.h) {
                for x in b.x0..b.x1.min(self.w) {
                    let at = self.at(x, y);
                    if !self.ground[at] {
                        continue;
                    }
                    px += 1;
                    if matches!(self.layer[at], Layer::Band | Layer::RailLip | Layer::Other) {
                        drawn += 1;
                        hit = true;
                    }
                }
            }
            drawn_cells += i64::from(hit);
        }
        J::obj([
            (
                "colour",
                colour.map_or_else(|| J::s("any off the page"), |c| J::s(hex(c))),
            ),
            ("cells", J::Int(cells)),
            ("px", J::Int(px)),
            ("under_band_px", J::Int(drawn)),
            ("under_band_cells", J::Int(drawn_cells)),
        ])
    }

    /// Every sorted pixel, as TSV.
    fn pixels(&self, core: &[bool]) -> String {
        let mut out =
            String::from("x\ty\tlayer\trow\tcolour\tL\tC\th\tcore\tmud_spec\tmud_broad\n");
        for y in 0..self.h {
            for x in 0..self.w {
                let i = self.at(x, y);
                if self.layer[i] == Layer::None {
                    continue;
                }
                let p = self.f(x, y);
                let v = lch(p);
                let _ = writeln!(
                    out,
                    "{x}\t{y}\t{}\t{}\t{}\t{:.4}\t{:.4}\t{:.1}\t{}\t{}\t{}",
                    self.layer[i].name(),
                    self.owner[i],
                    hex(p),
                    v.0,
                    v.1,
                    v.2,
                    u8::from(core[i]),
                    u8::from(mud_spec(v)),
                    u8::from(mud_broad(v)),
                );
            }
        }
        out
    }

    /// The trail pixels no layer's zone holds.
    fn other_px(&self) -> i64 {
        i64::try_from(self.layer.iter().filter(|l| **l == Layer::Other).count()).unwrap_or(0)
    }
}

/// One layer's colours: `OkLab` L and C, hues and mud.
#[derive(Default)]
struct Tally {
    l: Vec<f64>,
    c: Vec<f64>,
    hues: [i64; 12],
    spec: i64,
    broad: i64,
    mud: HashMap<Rgb, i64>,
}

impl Tally {
    fn add(&mut self, rgb: Rgb) {
        let v = lch(rgb);
        self.l.push(v.0);
        self.c.push(v.1);
        if v.1 >= HUE_C {
            self.hues[((v.2 / 30.0) as usize).min(11)] += 1;
        }
        let (s, b) = (mud_spec(v), mud_broad(v));
        self.spec += i64::from(s);
        self.broad += i64::from(b);
        if s || b {
            *self.mud.entry(rgb).or_default() += 1;
        }
    }

    fn report(mut self) -> J {
        let stat = |v: &mut Vec<f64>| -> J {
            v.sort_by(f64::total_cmp);
            match (v.first(), rank(v, 0.5), v.last()) {
                (Some(&lo), Some(mid), Some(&hi)) => J::Arr(
                    [lo, mid, hi]
                        .iter()
                        .map(|&x| J::Float(round_to(x, 3)))
                        .collect(),
                ),
                _ => J::Null,
            }
        };
        let mut mud: Vec<(Rgb, i64)> = self.mud.into_iter().collect();
        mud.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let listed = mud.iter().take(MUD_LISTED).map(|&(rgb, n)| {
            let v = lch(rgb);
            let rule = match (mud_spec(v), mud_broad(v)) {
                (true, true) => "both",
                (true, false) => "spec",
                _ => "broad",
            };
            J::obj([
                ("colour", J::s(hex(rgb))),
                ("px", J::Int(n)),
                ("L", J::Float(round_to(v.0, 3))),
                ("C", J::Float(round_to(v.1, 3))),
                ("h", J::Float(round_to(v.2, 1))),
                ("rule", J::s(rule)),
            ])
        });
        J::obj([
            ("px", J::Int(i64::try_from(self.l.len()).unwrap_or(0))),
            ("L", stat(&mut self.l)),
            ("C", stat(&mut self.c)),
            (
                "hues",
                J::Arr(self.hues.iter().map(|&n| J::Int(n)).collect()),
            ),
            ("mud_spec", J::Int(self.spec)),
            ("mud_broad", J::Int(self.broad)),
            ("mud_colours", J::Arr(listed.collect())),
        ])
    }
}

/// What `trail-census` was asked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
    pub png: String,
    pub dims: Option<String>,
    pub origin: Option<String>,
    pub cell: Option<String>,
    pub clean: Option<String>,
    pub cursor: Option<String>,
    pub caret: Option<String>,
    pub caret_pad: i64,
    pub pets: Vec<String>,
    pub selection: Option<String>,
    pub page: Option<String>,
    pub pixels: Option<String>,
    pub mud_max: Option<i64>,
    pub glyph_min: Option<f64>,
}

fn number<T: std::str::FromStr>(flag: &str, v: &str, kind: &str) -> Result<T, Fail> {
    v.trim()
        .parse()
        .map_err(|_| Fail::usage(format!("trail-census: {flag} takes {kind}, not '{v}'")))
}

/// Read `trail-census`'s arguments.
/// # Errors
/// A usage error (exit 2): an unknown option, a flag with no or a malformed value, no frame or more than one.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let out = Args {
        pets: take_values(&mut a, "--pet")?,
        dims: take_value(&mut a, "--dims")?,
        origin: take_value(&mut a, "--origin")?,
        cell: take_value(&mut a, "--cell")?,
        clean: take_value(&mut a, "--clean")?,
        cursor: take_value(&mut a, "--cursor")?,
        caret: take_value(&mut a, "--caret")?,
        caret_pad: take_value(&mut a, "--caret-pad")?
            .map(|v| number("--caret-pad", &v, "a count of px"))
            .transpose()?
            .unwrap_or(0),
        selection: take_value(&mut a, "--selection")?,
        page: take_value(&mut a, "--page")?,
        pixels: take_value(&mut a, "--pixels")?,
        mud_max: take_value(&mut a, "--mud-max")?
            .map(|v| number("--mud-max", &v, "a count"))
            .transpose()?,
        glyph_min: take_value(&mut a, "--glyph-min")?
            .map(|v| number("--glyph-min", &v, "a ratio"))
            .transpose()?,
        png: String::new(),
    };
    if out.caret_pad < 0 {
        return Err(Fail::usage(
            "trail-census: --caret-pad takes a count of px, not a negative one",
        ));
    }
    if let Some(flag) = a.iter().find(|x| x.starts_with('-') && x.len() > 1) {
        return Err(Fail::usage(format!(
            "trail-census: unknown option '{flag}' (trail-census --help)"
        )));
    }
    match a.as_slice() {
        [png] => Ok(Args {
            png: png.clone(),
            ..out
        }),
        [] => Err(Fail::usage(
            "trail-census: name the frame to read (trail-census --help)",
        )),
        [_, extra, ..] => Err(Fail::usage(format!(
            "trail-census: one frame at a time, not also '{extra}'"
        ))),
    }
}

/// `X0,Y0,X1,Y1` as a box.
fn parse_box(text: &str) -> Result<PxBox, String> {
    let v: Vec<i64> = text
        .split(',')
        .map(|p| p.trim().parse())
        .collect::<Result<_, _>>()
        .map_err(|_| {
            format!(
                "--pet wants X0,Y0,X1,Y1 in px, got {}",
                crate::util::py_repr(text)
            )
        })?;
    match v.as_slice() {
        &[x0, y0, x1, y1] if x0 < x1 && y0 < y1 => Ok(PxBox { x0, y0, x1, y1 }),
        _ => Err(format!(
            "--pet wants X0,Y0,X1,Y1 with X0 < X1 and Y0 < Y1, got {}",
            crate::util::py_repr(text)
        )),
    }
}

/// A sidecar of the frame or, for a frame named `<stem>.png`, of its stem.
fn sidecar_of(png: &Path, suffix: &str) -> Option<PathBuf> {
    sidecar(png, suffix, false).or_else(|| sidecar(png, suffix, true))
}

/// The report for `a`, and whether a threshold failed.
/// # Errors
/// A file that cannot be read, decoded or written, no clean twin, a grid that does not fit the frame, or a malformed
/// box or colour.
pub fn report(a: &Args) -> Result<J, String> {
    let png = PathBuf::from(from_caller(&a.png));
    let frame = Image::read(&png)?;
    let dims = a
        .dims
        .as_deref()
        .map(|d| PathBuf::from(from_caller(d)))
        .or_else(|| sidecar_of(&png, ".dims.txt"));
    let grid = frame_grid(&frame, a.origin.as_deref(), a.cell.as_deref(), dims)?;
    let clean_path = a
        .clean
        .as_deref()
        .map(|c| PathBuf::from(from_caller(c)))
        .or_else(|| sidecar(&png, ".clean.png", true))
        .ok_or("no clean twin: pass --clean PNG (`aterm ctl image clean`) or keep <stem>.<frame>.clean.png")?;
    let clean = clean_twin(&frame, &clean_path)?;
    let cursor = a
        .cursor
        .as_deref()
        .map(|c| PathBuf::from(from_caller(c)))
        .or_else(|| sidecar(&png, ".cursor.txt", true));
    let (caret, caret_facts) = caret_cell(a.caret.as_deref(), cursor)?;
    let caret = caret.filter(|&(r, c)| grid.holds(r, c));
    let page = a
        .page
        .as_deref()
        .map_or_else(|| Ok(clean.most_common()), parse_hex)?;
    let selection = a.selection.as_deref().map(parse_hex).transpose()?;
    let pets = a
        .pets
        .iter()
        .map(|p| parse_box(p))
        .collect::<Result<Vec<_>, _>>()?;
    let caret_box = caret.map(|(r, c)| {
        let b = grid.cell_box(r, c, 1);
        PxBox {
            x0: b.x0 - a.caret_pad,
            y0: b.y0 - a.caret_pad,
            x1: b.x1 + a.caret_pad,
            y1: b.y1 + a.caret_pad,
        }
    });

    let scene = Scene::new(&frame, &clean, grid, page, caret_box, &pets);
    let core = scene.core();
    if let Some(path) = &a.pixels {
        let path = from_caller(path);
        std::fs::write(&path, scene.pixels(&core))
            .map_err(|e| format!("cannot write {path}: {e}"))?;
    }
    let layers = scene.layers(&core);
    let (glyphs, glyph_min) = scene.glyphs(caret);
    let (top, tail) = scene.tops(caret_box);
    let (joins, cohorts) = scene.joins();
    let mut caret_report = match (caret, caret_box) {
        (Some(cell), Some(b)) => scene.caret(cell, b),
        _ => J::Null,
    };
    for (k, v) in caret_facts {
        if caret_report != J::Null {
            caret_report.set(&k, v);
        }
    }

    let mut failures = Vec::new();
    if let Some(max) = a.mud_max {
        for layer in ["band", "rail_lip"] {
            for rule in ["mud_spec", "mud_broad"] {
                let n = layers
                    .get(layer)
                    .and_then(|l| l.get("core"))
                    .and_then(|c| c.get(rule))
                    .and_then(J::as_i64)
                    .unwrap_or(0);
                if n > max {
                    failures.push(J::s(format!("{layer} core: {n} px of {rule} over {max}")));
                }
            }
        }
    }
    if let Some(min) = a.glyph_min
        && glyph_min.is_none_or(|g| g < min)
    {
        let said = glyph_min.map_or_else(|| "None".to_string(), crate::pyjson::py_float);
        failures.push(J::s(format!("glyph minimum {said} under {min:.2}:1")));
    }

    let mut out = J::obj([
        ("image", J::s(abspath(&a.png))),
        (
            "size",
            J::Arr(vec![
                J::Int(i64::try_from(frame.width).unwrap_or(0)),
                J::Int(i64::try_from(frame.height).unwrap_or(0)),
            ]),
        ),
        ("grid", grid.describe()),
        ("page", J::s(hex(page))),
        ("clean", J::s(abspath(&clean_path.to_string_lossy()))),
        (
            "band_rows",
            J::Arr(scene.band_rows().into_iter().map(J::Int).collect()),
        ),
    ]);
    out.set("layers", layers);
    out.set("glyphs", glyphs);
    out.set("top", top);
    out.set("tail", tail);
    out.set("joins", joins);
    out.set("cohorts", cohorts);
    out.set("stroke", scene.stroke());
    out.set("pets", scene.pets(&pets));
    out.set("stars", scene.stars());
    out.set("caret", caret_report);
    out.set("selection", scene.selection(selection));
    out.set("other_px", J::Int(scene.other_px()));
    out.set("failures", J::Arr(failures));
    Ok(out)
}

/// The report as JSON that reads, diffs and greps: the report, its sections and each layer open up one key to a line,
/// lists of objects put one on a line, and everything else is written compact.
#[must_use]
pub fn render(v: &J, depth: usize) -> String {
    let indent = " ".repeat(depth + 1);
    match v {
        J::Obj(m) if depth < 2 || v.get("all").is_some() => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, item)| {
                    format!(
                        "{indent}{}: {}",
                        J::s(k.as_str()).py(),
                        render(item, depth + 1)
                    )
                })
                .collect();
            format!("{{\n{}\n{}}}", items.join(",\n"), " ".repeat(depth))
        }
        J::Arr(a) if !a.is_empty() && a.iter().all(|i| matches!(i, J::Obj(_))) => {
            let items: Vec<String> = a
                .iter()
                .map(|i| format!("{indent}{}", compact(i)))
                .collect();
            format!("[\n{}\n{}]", items.join(",\n"), " ".repeat(depth))
        }
        _ => compact(v),
    }
}

/// `trail-census`: print the report; exit 1 when a threshold failed.
/// # Errors
/// A usage, file or decoding error (exit 2).
pub fn cmd_trail_census(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let a = parse_args(args)?;
    let out = report(&a).map_err(|e| Fail::usage(format!("trail-census: {e}")))?;
    println!("{}", render(&out, 0));
    let failed = out
        .get("failures")
        .and_then(J::as_arr)
        .is_some_and(|f| !f.is_empty());
    Ok(i32::from(failed))
}

#[cfg(test)]
#[path = "trail_census_tests.rs"]
mod tests;

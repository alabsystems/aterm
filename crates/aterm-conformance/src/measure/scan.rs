// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The paint scanner: did a captured take carry effect ink?

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub use super::frame::{Frame, FrameError, Rgb};

/// The brightest channel an ink pixel needs: keeps a band's tapered fringe from voting. 60 keeps the luminance-capped rainbow's dimmest stops (yellow, green and cyan sit at 70-94) inside the domain.
pub const SAT_MIN: u8 = 60;
/// The channel spread (max - min) an ink pixel needs: the colour discriminator.
pub const SAT_SPREAD: u8 = 40;
/// The human-facing hue diagnostic's bar: half a 30-degree bucket of real circular distance inside one painted object.
pub const ROW_RAINBOW_MIN_HUE_DEGREES: f64 = 15.0;

/// The thin ribbon component (the underline highlighter): at least three quarters of a cell wide (floor [`RIBBON_COMPONENT_MIN_WIDTH_FLOOR`]), at most [`RIBBON_COMPONENT_MAX_HEIGHT`] tall, and two pixels more than its width.
pub const RIBBON_COMPONENT_MIN_PIXELS_FLOOR: usize = 8;
pub const RIBBON_COMPONENT_MIN_WIDTH_FLOOR: usize = 6;
pub const RIBBON_COMPONENT_MAX_HEIGHT: usize = 4;

/// The attached run: a dense saturated run at least two cells wide on the bottom row of the solid block cursor, at least 3/4 dense.
pub const ATTACHED_RIBBON_MIN_CELLS: usize = 2;
pub const ATTACHED_RIBBON_DENSITY_NUM: usize = 3;
pub const ATTACHED_RIBBON_DENSITY_DEN: usize = 4;

/// The cyan bar: ROYGBIV has no cyan, so a paint-semantic pixel in [165, 195] degrees at saturation >= 0.35 names a real off-palette pixel.
pub const CYAN_HUE_LO: f64 = 165.0;
pub const CYAN_HUE_HI: f64 = 195.0;
pub const CYAN_SAT_MIN: f64 = 0.35;

/// Companion morphology: a four-connected foreground component at least this wide, tall and populous.
pub const COMP_MIN_W: usize = 12;
pub const COMP_MIN_H: usize = 18;
pub const COMP_MIN_PIXELS: usize = 24;
/// The resident pet's compact idle pose: admitted only on the isolating frames (frame 0 and the three final cleared-plane stills) and only while the engine claims the pet.
pub const COMPACT_PET_MIN_H: usize = 9;
/// The earned flying head at its momentum-zero resting size (measured 16 px tall at cell height 14): admitted on the isolating frames while the engine claims the cat.
pub const CAT_STILL_MIN_H: usize = 15;
/// How far a foreground pixel stands from the backdrop in some channel.
pub const FOREGROUND_DELTA: u8 = 24;
/// A companion component wider than this many cell heights has fused with the typed row through a glyph, the caret or the band, so its box is the row's and excludes nothing.
pub const COMPANION_ALONE_MAX_WIDTH_CELL_HEIGHTS: usize = 3;

/// The flat-slab hue diagnostic's column ceiling: the underline band's slabs stand 1-5 px, the caret 14+ and a coat 18+.
pub const BAND_MAX_SLAB_HEIGHT_PX: usize = 6;
/// A slab run must be this wide to join the row-union hue diagnostic, so a detached coat pixel cannot pair with the band.
pub const BAND_MIN_RUN_PX: usize = 3;
/// The band domain's height cap above one cell: the tall body reaches 1.1 cell, every healthy band object measured <= cell + 1, the flying head's coat cell + 4 and more.
pub const BAND_OBJECT_ABOVE_CELL_MAX_PX: usize = 2;
/// The litness floor for one frame: one cell-row of band at the 7 px pitch, 3 px over the widest non-band residue (the pet's accents, 4 px) and 3 px under the weakest healthy frame (the per-mark decay trough, 10-12 px).
pub const RIBBON_LIT_MIN_PX: usize = 7;
/// The claim-bound window's close guard: the raster leads the ledger's close, so the last two driven frames belong to the exhale.
pub const RIBBON_CLOSE_GUARD_FRAMES: i64 = 2;

/// The driven window's far-end grace: one in-flight keystroke's PTY-to-glass latency past the closing bracket read.
pub const DRIVEN_GRACE_US: i64 = 150_000;
/// A dark run that reaches the driven window's final sampled frame is the authored exhale for its first 0.4 s; only the excess is charged.
pub const EXHALE_CLOSE_ALLOWANCE_US: i64 = 400_000;

/// The resting sprites the engine authors under the strict floor (the curled sleepers and the gallop's low first frame), witnessed inside their emitted `pet_body` at the compact floor, more than two cells wide, unclipped by the cell on its left, right and top.
pub const RESTING_POSE_STEMS: [&str; 2] = ["sleep", "run_0"];

/// Saturated changed pixels of one frame: pixel index -> hue in degrees.
pub type SatMap = BTreeMap<usize, f64>;

/// The terminal cell size the take was rendered at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub w: usize,
    pub h: usize,
}

impl Cell {
    /// `None` unless both edges are positive.
    #[must_use]
    pub fn new(w: usize, h: usize) -> Option<Self> {
        (w > 0 && h > 0).then_some(Self { w, h })
    }
}

/// A set of the twelve 30-degree hue buckets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HueSet(u16);

impl HueSet {
    /// The bucket of a hue angle.
    #[must_use]
    pub fn bucket(degrees: f64) -> u8 {
        // Truncation toward zero, then wrap: 360 degrees is bucket 0.
        ((degrees / 30.0) as u32 % 12) as u8
    }

    #[must_use]
    pub fn of(buckets: &[u8]) -> Self {
        let mut set = Self::default();
        for &b in buckets {
            set.insert(b);
        }
        set
    }

    pub const fn insert(&mut self, bucket: u8) {
        self.0 |= 1 << (bucket % 12);
    }

    pub const fn union_with(&mut self, other: Self) {
        self.0 |= other.0;
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub fn buckets(self) -> Vec<u8> {
        (0..12).filter(|b| self.0 & (1 << b) != 0).collect()
    }
}

/// A component's shape: `(pixels, width, height)`, all zero for none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Shape {
    pub pixels: usize,
    pub width: usize,
    pub height: usize,
}

impl Shape {
    pub const NONE: Self = Self {
        pixels: 0,
        width: 0,
        height: 0,
    };

    #[must_use]
    pub const fn new(pixels: usize, width: usize, height: usize) -> Self {
        Self {
            pixels,
            width,
            height,
        }
    }

    #[must_use]
    pub const fn is_some(self) -> bool {
        self.pixels > 0
    }
}

/// An inclusive pixel box `(x0, y0)..=(x1, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelBox {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl PixelBox {
    #[must_use]
    pub const fn new(x0: i64, y0: i64, x1: i64, y1: i64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    /// Whether this box holds the inclusive box `(x0, y0, x1, y1)` whole.
    #[must_use]
    pub const fn contains(&self, x0: i64, y0: i64, x1: i64, y1: i64) -> bool {
        x0 >= self.x0 && y0 >= self.y0 && x1 <= self.x1 && y1 <= self.y1
    }
}

/// A foreground component with its inclusive bounding box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Component {
    pub shape: Shape,
    pub bbox: PixelBox,
}

/// A dense run attached to the cursor: `(pixels, width, row)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RibbonRun {
    pub pixels: usize,
    pub width: usize,
    pub row: usize,
}

impl RibbonRun {
    pub const NONE: Self = Self {
        pixels: 0,
        width: 0,
        row: 0,
    };

    #[must_use]
    pub const fn new(pixels: usize, width: usize, row: usize) -> Self {
        Self { pixels, width, row }
    }
}

/// The cyan sample nearest pure cyan in a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CyanSample {
    pub degrees: f64,
    pub rgb: Rgb,
}

/// Everything one frame's raster says against the baseline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DynamicMetrics {
    /// Saturated changed pixels.
    pub ink: usize,
    /// Their hue buckets.
    pub hues: HueSet,
    /// The most hue buckets on any one raster row.
    pub row_hue_peak: usize,
    /// The hue diagnostic: the widest in-object, in-row separation, or the widest flat-slab row span.
    pub row_hue_separation: f64,
    pub cyan_pixels: usize,
    pub cyan_worst: Option<CyanSample>,
    /// The strongest thin ribbon component.
    pub ribbon_component: Shape,
    /// The strongest cursor-attached run.
    pub ribbon_run: RibbonRun,
    /// The band's own population (the litness domain).
    pub band_ink: usize,
    pub band_hues: HueSet,
    /// Ink of objects standing wholly inside the drawn animal's box, refused to `band_ink`.
    pub animal_ink: usize,
}

/// The continuous hue angle of a paint-semantic pixel, else `None`: the colour floor ([`SAT_MIN`], [`SAT_SPREAD`]) and HSV hue in degrees.
#[must_use]
pub fn saturated_hue_degrees(rgb: Rgb) -> Option<f64> {
    let [r, g, b] = rgb;
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    if mx < SAT_MIN || mx - mn < SAT_SPREAD {
        return None;
    }
    Some(hsv_hue(rgb) * 360.0)
}

/// HSV hue in `[0, 1)`, computed as Python's `colorsys.rgb_to_hsv` does it so
/// the calibrated bucket edges stay where they were measured.
fn hsv_hue([r, g, b]: Rgb) -> f64 {
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let maxc = r.max(g).max(b);
    let minc = r.min(g).min(b);
    if maxc == minc {
        return 0.0;
    }
    let range = maxc - minc;
    let rc = (maxc - r) / range;
    let gc = (maxc - g) / range;
    let bc = (maxc - b) / range;
    let h = if r == maxc {
        bc - gc
    } else if g == maxc {
        2.0 + rc - bc
    } else {
        4.0 + gc - rc
    };
    (h / 6.0).rem_euclid(1.0)
}

/// The hue angle when this pixel is cyan by the bar, else `None`.
#[must_use]
pub fn cyan_band_hue(rgb: Rgb) -> Option<f64> {
    let degrees = saturated_hue_degrees(rgb)?;
    if !(CYAN_HUE_LO..=CYAN_HUE_HI).contains(&degrees) {
        return None;
    }
    let [r, g, b] = rgb;
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    (f64::from(mx - mn) / f64::from(mx) >= CYAN_SAT_MIN).then_some(degrees)
}

/// The shortest distance between two hue angles on the wheel.
#[must_use]
pub fn circular_hue_distance_degrees(a: f64, b: f64) -> f64 {
    let direct = (a - b).abs();
    direct.min(360.0 - direct)
}

/// The largest pairwise circular distance, in O(n log n).
#[must_use]
pub fn max_circular_hue_distance_degrees(angles: &[f64]) -> f64 {
    if angles.len() < 2 {
        return 0.0;
    }
    let mut ordered = angles.to_vec();
    ordered.sort_by(f64::total_cmp);
    let count = ordered.len();
    let extended: Vec<f64> = ordered
        .iter()
        .copied()
        .chain(ordered.iter().map(|a| a + 360.0))
        .collect();
    let mut best = 0.0_f64;
    for (i, &angle) in ordered.iter().enumerate() {
        let (lo, hi) = (i + 1, i + count);
        let target = angle + 180.0;
        let pivot = lo + extended[lo..hi].partition_point(|&a| a < target);
        for candidate in [pivot.wrapping_sub(1), pivot] {
            if (lo..hi).contains(&candidate) {
                best = best.max(circular_hue_distance_degrees(angle, extended[candidate]));
            }
        }
    }
    best
}

/// The four-connected components of a saturated pixel map, in ascending order of their first pixel.
#[must_use]
pub fn ink_objects(w: usize, h: usize, sat: &SatMap) -> Vec<Vec<usize>> {
    let mut seen = BTreeSet::new();
    let mut objects = Vec::new();
    for &start in sat.keys() {
        if !seen.insert(start) {
            continue;
        }
        let mut stack = vec![start];
        let mut members = Vec::new();
        while let Some(p) = stack.pop() {
            members.push(p);
            let (x, y) = (p % w, p / w);
            let neighbours = [
                (x > 0).then(|| p - 1),
                (x + 1 < w).then(|| p + 1),
                (y > 0).then(|| p - w),
                (y + 1 < h).then(|| p + w),
            ];
            for q in neighbours.into_iter().flatten() {
                if sat.contains_key(&q) && seen.insert(q) {
                    stack.push(q);
                }
            }
        }
        objects.push(members);
    }
    objects
}

/// The band's own population on glass: `(band_ink, band_hues)`.
#[must_use]
pub fn band_ink_metrics(
    w: usize,
    h: usize,
    sat: &SatMap,
    cell_h: usize,
    exclude: &[PixelBox],
) -> (usize, HueSet) {
    let (ink, hues, _) = band_ink_partition(w, h, sat, cell_h, exclude);
    (ink, hues)
}

/// [`band_ink_metrics`] plus the ink it refused as the animal's, counted before the band rules so the report names what the box removed.
#[must_use]
pub fn band_ink_partition(
    w: usize,
    h: usize,
    sat: &SatMap,
    cell_h: usize,
    exclude: &[PixelBox],
) -> (usize, HueSet, usize) {
    let cap = cell_h + BAND_OBJECT_ABOVE_CELL_MAX_PX;
    let mut band_ink = 0;
    let mut band_hues = HueSet::default();
    let mut animal_ink = 0;
    for members in ink_objects(w, h, sat) {
        if !exclude.is_empty() {
            let (x0, y0, x1, y1) = bounds(w, &members);
            if exclude.iter().any(|b| b.contains(x0, y0, x1, y1)) {
                animal_ink += members.len();
                continue;
            }
        }
        if let Some((ink, hues)) = count_band_object(w, &members, sat, cap, cell_h) {
            band_ink += ink;
            band_hues.union_with(hues);
            continue;
        }
        let mut columns: BTreeMap<usize, usize> = BTreeMap::new();
        for &p in &members {
            *columns.entry(p % w).or_default() += 1;
        }
        let remainder: SatMap = members
            .iter()
            .filter(|&&p| columns[&(p % w)] < cell_h)
            .map(|&p| (p, sat[&p]))
            .collect();
        if remainder.len() == members.len() {
            continue;
        }
        for piece in ink_objects(w, h, &remainder) {
            if let Some((ink, hues)) = count_band_object(w, &piece, sat, cap, cell_h) {
                band_ink += ink;
                band_hues.union_with(hues);
            }
        }
    }
    (band_ink, band_hues, animal_ink)
}

/// Inclusive bounds of a member list, as `(x0, y0, x1, y1)`.
fn bounds(w: usize, members: &[usize]) -> (i64, i64, i64, i64) {
    let xs = members.iter().map(|p| p % w);
    let ys = members.iter().map(|p| p / w);
    let as_i = |v: usize| i64::try_from(v).unwrap_or(i64::MAX);
    (
        as_i(xs.clone().min().unwrap_or(0)),
        as_i(ys.clone().min().unwrap_or(0)),
        as_i(xs.max().unwrap_or(0)),
        as_i(ys.max().unwrap_or(0)),
    )
}

/// One object's band pixels under the height cap, or `None` when it is refused
/// as too tall.
fn count_band_object(
    w: usize,
    members: &[usize],
    sat: &SatMap,
    cap: usize,
    cell_h: usize,
) -> Option<(usize, HueSet)> {
    let (ymin, ymax) = members
        .iter()
        .map(|p| p / w)
        .fold((usize::MAX, 0), |(lo, hi), y| (lo.min(y), hi.max(y)));
    let trimmed: Vec<usize>;
    let members = if ymax - ymin + 1 > cap {
        let mut rows: BTreeMap<usize, usize> = BTreeMap::new();
        for &p in members {
            *rows.entry(p / w).or_default() += 1;
        }
        let densest = rows.values().copied().max().unwrap_or(0);
        let floor = RIBBON_COMPONENT_MIN_WIDTH_FLOOR.max(densest / 2);
        let band_rows: BTreeSet<usize> = rows
            .iter()
            .filter(|&(_, &n)| n >= floor)
            .map(|(&y, _)| y)
            .collect();
        let (first, last) = (band_rows.first()?, band_rows.last()?);
        if last - first + 1 > cap {
            return None;
        }
        trimmed = members
            .iter()
            .copied()
            .filter(|p| band_rows.contains(&(p / w)))
            .collect();
        &trimmed[..]
    } else {
        members
    };
    let mut columns: BTreeMap<usize, usize> = BTreeMap::new();
    for &p in members {
        *columns.entry(p % w).or_default() += 1;
    }
    let mut ink = 0;
    let mut hues = HueSet::default();
    for &p in members {
        if columns[&(p % w)] >= cell_h {
            continue;
        }
        ink += 1;
        hues.insert(HueSet::bucket(sat[&p]));
    }
    Some((ink, hues))
}

/// The peak hue separation between samples sharing a raster row and one object.
#[must_use]
pub fn object_row_hue_separation(w: usize, h: usize, sat: &SatMap) -> f64 {
    let mut best = 0.0_f64;
    for members in ink_objects(w, h, sat) {
        let mut rows: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
        for p in members {
            rows.entry(p / w).or_default().push(sat[&p]);
        }
        for angles in rows.values() {
            best = best.max(max_circular_hue_distance_degrees(angles));
        }
    }
    best
}

/// The peak hue span across one raster row's flat slab runs: the quantized, seam-split band's diagnostic.
#[must_use]
pub fn banded_row_span(w: usize, sat: &SatMap) -> f64 {
    let mut columns: BTreeMap<usize, usize> = BTreeMap::new();
    for &p in sat.keys() {
        *columns.entry(p % w).or_default() += 1;
    }
    let mut rows: BTreeMap<usize, Vec<(usize, f64)>> = BTreeMap::new();
    for (&p, &angle) in sat {
        if columns[&(p % w)] > BAND_MAX_SLAB_HEIGHT_PX {
            continue;
        }
        rows.entry(p / w).or_default().push((p % w, angle));
    }
    let mut best = 0.0_f64;
    for xs in rows.values_mut() {
        xs.sort_by_key(|&(x, _)| x);
        let mut runs: Vec<Vec<(usize, f64)>> = Vec::new();
        let mut cur = vec![xs[0]];
        for pair in xs.windows(2) {
            if pair[1].0 - pair[0].0 == 1 {
                cur.push(pair[1]);
            } else {
                runs.push(std::mem::replace(&mut cur, vec![pair[1]]));
            }
        }
        runs.push(cur);
        let angles: Vec<f64> = runs
            .iter()
            .filter(|run| run.len() >= BAND_MIN_RUN_PX)
            .flat_map(|run| run.iter().map(|&(_, a)| a))
            .collect();
        if !angles.is_empty() {
            best = best.max(max_circular_hue_distance_degrees(&angles));
        }
    }
    best
}

/// The terminal backdrop: the majority colour of the four corners and the centre (ties go to the first in that order).
#[must_use]
pub fn frame_background(frame: &Frame) -> Rgb {
    let (w, h) = (frame.width(), frame.height());
    let points = [
        (0, 0),
        (w - 1, 0),
        (0, h - 1),
        (w - 1, h - 1),
        (w / 2, h / 2),
    ];
    let colours: Vec<Rgb> = points
        .iter()
        .map(|&(x, y)| frame.pixel(y * w + x))
        .collect();
    let count = |c: &Rgb| colours.iter().filter(|o| *o == c).count();
    let mut best = colours[0];
    for c in &colours {
        if count(c) > count(&best) {
            best = *c;
        }
    }
    best
}

/// Every foreground component meeting the companion floor ([`COMP_MIN_W`] x `min_height`, [`COMP_MIN_PIXELS`]), largest shape first (equal shapes keep scan order), with its bounding box.
#[must_use]
pub fn tall_component_records(
    frame: &Frame,
    background: Rgb,
    min_height: usize,
    region: Option<PixelBox>,
) -> Vec<Component> {
    let (w, h) = (frame.width(), frame.height());
    let full = PixelBox::new(0, 0, as_i(w) - 1, as_i(h) - 1);
    let PixelBox {
        x0: left,
        y0: top,
        x1: right,
        y1: bottom,
    } = region.unwrap_or(full);
    let x_lo = clamp_to(left, w);
    let x_hi = clamp_to(right.saturating_add(1), w);
    let y_lo = clamp_to(top, h);
    let y_hi = clamp_to(bottom.saturating_add(1), h);
    let mut foreground = vec![false; w * h];
    let far = |a: u8, b: u8| a.abs_diff(b) >= FOREGROUND_DELTA;
    for y in y_lo..y_hi {
        for x in x_lo..x_hi {
            let p = y * w + x;
            let px = frame.pixel(p);
            if far(px[0], background[0]) || far(px[1], background[1]) || far(px[2], background[2]) {
                foreground[p] = true;
            }
        }
    }
    let mut seen = vec![false; w * h];
    let mut found = Vec::new();
    for start in 0..w * h {
        if !foreground[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        let mut count = 0;
        let (mut x0, mut y0) = (start % w, start / w);
        let (mut x1, mut y1) = (x0, y0);
        while let Some(p) = stack.pop() {
            let (x, y) = (p % w, p / w);
            count += 1;
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            let neighbours = [
                (x > 0).then(|| p - 1),
                (x + 1 < w).then(|| p + 1),
                (y > 0).then(|| p - w),
                (y + 1 < h).then(|| p + w),
            ];
            for q in neighbours.into_iter().flatten() {
                if foreground[q] && !seen[q] {
                    seen[q] = true;
                    stack.push(q);
                }
            }
        }
        let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
        if cw >= COMP_MIN_W && ch >= min_height && count >= COMP_MIN_PIXELS {
            found.push(Component {
                shape: Shape::new(count, cw, ch),
                bbox: PixelBox::new(as_i(x0), as_i(y0), as_i(x1), as_i(y1)),
            });
        }
    }
    found.sort_by_key(|c| std::cmp::Reverse(c.shape));
    found
}

fn as_i(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// `v` clamped into `0..=len`, as an index.
fn clamp_to(v: i64, len: usize) -> usize {
    usize::try_from(v.max(0)).unwrap_or(usize::MAX).min(len)
}

/// The shapes of [`tall_component_records`] over the whole frame.
#[must_use]
pub fn tall_components(frame: &Frame, background: Rgb, min_height: usize) -> Vec<Shape> {
    tall_component_records(frame, background, min_height, None)
        .into_iter()
        .map(|c| c.shape)
        .collect()
}

/// The largest strict companion-shaped component, or [`Shape::NONE`].
#[must_use]
pub fn largest_tall_component(frame: &Frame) -> Shape {
    tall_components(frame, frame_background(frame), COMP_MIN_H)
        .first()
        .copied()
        .unwrap_or(Shape::NONE)
}

/// Whether frame `frame_no` of `frame_count` is an isolating frame: frame 0 (before typing) or one of the three final cleared-plane stills.
#[must_use]
pub const fn compact_pet_witness_frame(frame_no: usize, frame_count: usize) -> bool {
    frame_no == 0 || frame_no >= frame_count.saturating_sub(3)
}

/// The relaxed height floor an isolating frame may fall back to under the engine's own claim: the pet's compact pose, else the cat's resting head.
#[must_use]
pub const fn companion_witness_floor(
    pet_active: bool,
    frame_no: usize,
    frame_count: usize,
    cat_active: bool,
) -> Option<usize> {
    if !compact_pet_witness_frame(frame_no, frame_count) {
        return None;
    }
    if pet_active {
        Some(COMPACT_PET_MIN_H)
    } else if cat_active {
        Some(CAT_STILL_MIN_H)
    } else {
        None
    }
}

/// The companion component: the largest strict one, else the isolating frames' relaxed floor under the engine's claim.
#[must_use]
pub fn companion_component(
    frame: &Frame,
    background: Rgb,
    pet_active: bool,
    frame_no: usize,
    frame_count: usize,
    strict: Option<&[Component]>,
    cat_active: bool,
) -> Shape {
    companion_record(
        frame,
        background,
        pet_active,
        frame_no,
        frame_count,
        strict,
        cat_active,
    )
    .map_or(Shape::NONE, |c| c.shape)
}

/// The bounding box of the component [`companion_component`] yields.
#[must_use]
pub fn companion_component_box(
    frame: &Frame,
    background: Rgb,
    pet_active: bool,
    frame_no: usize,
    frame_count: usize,
    strict: Option<&[Component]>,
    cat_active: bool,
) -> Option<PixelBox> {
    companion_record(
        frame,
        background,
        pet_active,
        frame_no,
        frame_count,
        strict,
        cat_active,
    )
    .map(|c| c.bbox)
}

fn companion_record(
    frame: &Frame,
    background: Rgb,
    pet_active: bool,
    frame_no: usize,
    frame_count: usize,
    strict: Option<&[Component]>,
    cat_active: bool,
) -> Option<Component> {
    let owned;
    let strict = match strict {
        Some(list) => list,
        None => {
            owned = tall_component_records(frame, background, COMP_MIN_H, None);
            &owned[..]
        }
    };
    if let Some(first) = strict.first() {
        return Some(*first);
    }
    let floor = companion_witness_floor(pet_active, frame_no, frame_count, cat_active)?;
    tall_component_records(frame, background, floor, None)
        .first()
        .copied()
}

/// Whether `pose` (`pet_<stem>` / `pet_dog_<stem>`) names a sprite authored under the strict floor ([`RESTING_POSE_STEMS`]).
#[must_use]
pub fn resting_pose(pose: Option<&str>) -> bool {
    let Some(pose) = pose.filter(|p| !p.is_empty()) else {
        return false;
    };
    let Some(stem) = ["pet_dog_", "pet_"]
        .iter()
        .find_map(|prefix| pose.strip_prefix(prefix))
    else {
        return false;
    };
    RESTING_POSE_STEMS.iter().any(|s| stem.starts_with(s))
}

/// The resting sprite's width floor: one pixel over a two-cell text impostor.
#[must_use]
pub const fn resting_pet_min_width(cell_w: usize) -> usize {
    2 * cell_w + 1
}

/// The pet's morphology inside its emitted body rectangle.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn located_pet_component(
    frame: &Frame,
    background: Rgb,
    body: Option<PixelBox>,
    frame_no: usize,
    frame_count: usize,
    pose: Option<&str>,
    cell_w: Option<usize>,
) -> Shape {
    let Some(body) = body else {
        return Shape::NONE;
    };
    let floor = companion_witness_floor(true, frame_no, frame_count, false).unwrap_or(COMP_MIN_H);
    if let Some(first) = tall_component_records(frame, background, floor, Some(body)).first() {
        return first.shape;
    }
    match cell_w {
        Some(cell_w) if floor > COMPACT_PET_MIN_H && resting_pose(pose) => {
            resting_pet_component(frame, background, body, cell_w)
        }
        _ => Shape::NONE,
    }
}

/// The claimed resting sprite inside its cell: the compact height floor, more than two cells wide, touching neither the cell's left, right nor top edge (a row running through the cell is clipped there and is the row's).
#[must_use]
pub fn resting_pet_component(
    frame: &Frame,
    background: Rgb,
    body: PixelBox,
    cell_w: usize,
) -> Shape {
    tall_component_records(frame, background, COMPACT_PET_MIN_H, Some(body))
        .into_iter()
        .find(|c| {
            c.shape.width >= resting_pet_min_width(cell_w)
                && c.bbox.x0 != body.x0
                && c.bbox.x1 != body.x1
                && c.bbox.y0 != body.y0
        })
        .map_or(Shape::NONE, |c| c.shape)
}

/// One `trail status` row, as the engine printed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusRow(pub String);

/// A required status token that is missing or malformed: the row is refused, never read as a default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusError {
    pub key: String,
    pub raw: Option<String>,
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "missing or malformed {}={:?}", self.key, self.raw)
    }
}

impl std::error::Error for StatusError {}

impl StatusRow {
    #[must_use]
    pub fn new(line: impl Into<String>) -> Self {
        Self(line.into())
    }

    fn malformed(&self, key: &str) -> StatusError {
        StatusError {
            key: key.to_string(),
            raw: self.value(key).map(str::to_string),
        }
    }

    /// One unquoted `key=value` token.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<&str> {
        self.0
            .split_whitespace()
            .find_map(|t| t.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
    }

    /// A quoted `key="…"` value (`style="rainbow kitty"`).
    #[must_use]
    pub fn quoted(&self, key: &str) -> Option<&str> {
        let needle = format!("{key}=\"");
        let at = self
            .0
            .match_indices(&needle)
            .find(|&(i, _)| i == 0 || self.0.as_bytes()[i - 1].is_ascii_whitespace())?
            .0;
        let rest = &self.0[at + needle.len()..];
        rest.split_once('"').map(|(v, _)| v)
    }

    /// A required boolean token.
    /// # Errors
    /// The token is missing or not `true`/`false`.
    pub fn bool(&self, key: &str) -> Result<bool, StatusError> {
        match self.value(key) {
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            _ => Err(self.malformed(key)),
        }
    }

    /// A required unsigned integer token.
    /// # Errors
    /// The token is missing or not all ASCII digits.
    pub fn uint(&self, key: &str) -> Result<u64, StatusError> {
        self.value(key)
            .filter(|raw| !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|raw| raw.parse().ok())
            .ok_or_else(|| self.malformed(key))
    }

    /// A required finite, non-negative decimal token: `nan` and `inf` compare false against every bound, which would switch a gate off.
    /// # Errors
    /// The token is missing, not a number, negative or not finite.
    pub fn nonneg_float(&self, key: &str) -> Result<f64, StatusError> {
        self.value(key)
            .and_then(|raw| raw.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| self.malformed(key))
    }

    /// The emitted body's exclusive `x0,x1,y0,y1` as an inclusive box.
    /// # Errors
    /// Malformed, empty or reversed geometry.
    pub fn pet_body(&self) -> Result<Option<PixelBox>, StatusError> {
        let raw = match self.value("pet_body") {
            None | Some("none") => return Ok(None),
            Some(raw) => raw,
        };
        let parts: Vec<i64> = raw
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| self.malformed("pet_body"))?;
        let [x0, x1, y0, y1] = parts[..] else {
            return Err(self.malformed("pet_body"));
        };
        if x1 <= x0 || y1 <= y0 {
            return Err(self.malformed("pet_body"));
        }
        Ok(Some(PixelBox::new(x0, y0, x1 - 1, y1 - 1)))
    }

    /// The engine's name for the resident sprite it drew.
    #[must_use]
    pub fn pet_pose(&self) -> Option<&str> {
        self.value("pet_pose")
    }
}

/// The last frame whose paired row still reports the ribbon driven: the admission ledger advanced (`spawns` above the previous row's) or an older producer's glide spine is alive (`speed > 0`).
/// # Errors
/// A row with a malformed `spawns`, or a present but malformed `speed`.
pub fn driven_last_frame(statuses: &[StatusRow]) -> Result<Option<usize>, StatusError> {
    let mut last = None;
    let mut previous: Option<u64> = None;
    for (frame_no, status) in statuses.iter().enumerate() {
        let spawns = status.uint("spawns")?;
        let speed = match status.value("speed") {
            Some(_) => status.nonneg_float("speed")?,
            None => 0.0,
        };
        let fed = previous.is_some_and(|p| spawns > p);
        if fed || speed > 0.0 {
            last = Some(frame_no);
        }
        previous = Some(spawns);
    }
    Ok(last)
}

/// The longest false run strictly between the first and last true.
#[must_use]
pub fn longest_interior_gap(flags: &[bool]) -> usize {
    let (Some(first), Some(last)) = (
        flags.iter().position(|&f| f),
        flags.iter().rposition(|&f| f),
    ) else {
        return 0;
    };
    let (mut longest, mut run) = (0, 0);
    for &flag in &flags[first..=last] {
        if flag {
            run = 0;
        } else {
            run += 1;
            longest = longest.max(run);
        }
    }
    longest
}

/// `(original index, verdict)` for every frame that is not a byte-identical resample of the one before it.
#[must_use]
pub fn distinct_samples(flags: &[bool], duplicates: &[bool]) -> Vec<(usize, bool)> {
    assert_eq!(
        flags.len(),
        duplicates.len(),
        "verdict and duplicate vectors differ in length"
    );
    flags
        .iter()
        .zip(duplicates)
        .enumerate()
        .filter(|(_, (_, dup))| !**dup)
        .map(|(i, (flag, _))| (i, *flag))
        .collect()
}

/// Original-index bounds of the first and last two-frame run of distinct positive samples: one onset glint cannot start a mature band.
#[must_use]
pub fn sustained_rainbow_bounds(flags: &[bool], duplicates: &[bool]) -> Option<(usize, usize)> {
    let samples = distinct_samples(flags, duplicates);
    let evidence: Vec<bool> = samples.iter().map(|&(_, f)| f).collect();
    let first = (0..evidence.len().saturating_sub(1)).find(|&i| evidence[i] && evidence[i + 1])?;
    let last = (1..evidence.len())
        .rev()
        .find(|&i| evidence[i - 1] && evidence[i])?;
    (first < last).then(|| (samples[first].0, samples[last].0))
}

/// The longest distinct dark run inside the mature two-frame-bounded rainbow.
#[must_use]
pub fn longest_sustained_interior_gap(flags: &[bool], duplicates: &[bool]) -> usize {
    let Some((first, last)) = sustained_rainbow_bounds(flags, duplicates) else {
        return 0;
    };
    let evidence: Vec<bool> = distinct_samples(flags, duplicates)
        .into_iter()
        .filter(|&(i, _)| first <= i && i <= last)
        .map(|(_, f)| f)
        .collect();
    longest_interior_gap(&evidence)
}

/// Consecutive trues at the end.
#[must_use]
pub fn trailing_true_run(flags: &[bool]) -> usize {
    flags.iter().rev().take_while(|&&f| f).count()
}

/// The longest run of consecutive trues.
#[must_use]
pub fn longest_true_run(flags: &[bool]) -> usize {
    let (mut longest, mut run) = (0, 0);
    for &flag in flags {
        if flag {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// `(distinct visible frames, longest distinct run, sustained interior gap)`.
#[must_use]
pub fn rainbow_summary(flags: &[bool], duplicates: &[bool]) -> (usize, usize, usize) {
    let evidence: Vec<bool> = distinct_samples(flags, duplicates)
        .into_iter()
        .map(|(_, f)| f)
        .collect();
    (
        evidence.iter().filter(|&&f| f).count(),
        longest_true_run(&evidence),
        longest_sustained_interior_gap(flags, duplicates),
    )
}

/// A recording's own index (`index.json` beside a `ctl video` take): each frame's stamp on the recorder's clock, the covered span's start, and the ticks whose redraw proved the screen unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordingIndex {
    pub stamps: BTreeMap<String, i64>,
    pub covered_from: Option<i64>,
    pub ticks: Vec<i64>,
}

impl RecordingIndex {
    /// Parse `index.json`.
    /// # Errors
    /// The bytes are not a JSON object.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: aterm_json::Value =
            aterm_json::from_slice(bytes).map_err(|e| format!("index.json: {e}"))?;
        if !value.is_object() {
            return Err("index.json is not an object".into());
        }
        let stamps = value
            .get("frames")
            .and_then(aterm_json::Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some((
                            row.get("file")?.as_str()?.to_string(),
                            row.get("t_us")?.as_i64()?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let covered_from = value
            .pointer("/meta/covered_us/0")
            .and_then(aterm_json::Value::as_i64);
        let ticks = value
            .get("ticks")
            .and_then(aterm_json::Value::as_array)
            .map(|ticks| ticks.iter().filter_map(aterm_json::Value::as_i64).collect())
            .unwrap_or_default();
        Ok(Self {
            stamps,
            covered_from,
            ticks,
        })
    }

    /// Read and parse `path`.
    /// # Errors
    /// The file cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&bytes)
    }
}

/// The continuity reading of a recorded take's driven window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrivenWindow {
    /// The longest blackout in wall-clock microseconds, or `-1` when the take carried no bracket to bound the window with (not measured).
    pub dark_us: i64,
    pub frames: usize,
    /// The widest gap in the union of frame and tick instants.
    pub sampling_hole_us: i64,
    /// The median frame interval.
    pub nominal_dt_us: i64,
    /// The excused span before the band's first sustained lit frame.
    pub mature_lead_us: i64,
}

impl DrivenWindow {
    pub const NOT_MEASURED: Self = Self {
        dark_us: -1,
        frames: 0,
        sampling_hole_us: 0,
        nominal_dt_us: 0,
        mature_lead_us: 0,
    };
}

/// The longest blackout inside the window the trail is driven in.
#[must_use]
pub fn driven_window_darkness(
    index: Option<&RecordingIndex>,
    driven_ms: Option<(i64, i64)>,
    names: &[String],
    flags: &[bool],
    duplicates: &[bool],
) -> DrivenWindow {
    let Some(index) = index else {
        return DrivenWindow::NOT_MEASURED;
    };
    let ordered: Vec<(i64, &str)> = names
        .iter()
        .filter_map(|n| index.stamps.get(n).map(|&t| (t, n.as_str())))
        .collect();
    let Some((lo_ms, hi_ms)) = driven_ms else {
        return DrivenWindow::NOT_MEASURED;
    };
    if ordered.len() < 2 {
        return DrivenWindow::NOT_MEASURED;
    }
    let mut deltas: Vec<i64> = ordered.windows(2).map(|w| w[1].0 - w[0].0).collect();
    deltas.sort_unstable();
    let nominal = deltas[deltas.len() / 2];
    let lit_by_name: BTreeMap<&str, bool> = names
        .iter()
        .map(String::as_str)
        .zip(flags.iter().copied())
        .collect();
    let dup_by_name: BTreeMap<&str, bool> = names
        .iter()
        .map(String::as_str)
        .zip(duplicates.iter().copied())
        .collect();
    let anchor = index.covered_from.unwrap_or(ordered[0].0);
    let low = anchor + lo_ms * 1000;
    let high = anchor + hi_ms * 1000 + DRIVEN_GRACE_US;
    let window: Vec<(i64, &str)> = ordered
        .into_iter()
        .filter(|&(t, _)| low <= t && t <= high)
        .collect();
    if window.len() < 2 {
        return DrivenWindow::NOT_MEASURED;
    }
    let mut resolved = Vec::with_capacity(window.len());
    let mut previous = false;
    for &(t, name) in &window {
        let duplicate = dup_by_name.get(name).copied().unwrap_or(false);
        let lit = if duplicate {
            previous
        } else {
            lit_by_name.get(name).copied().unwrap_or(false)
        };
        previous = lit;
        resolved.push((t, lit, duplicate));
    }
    let distinct: Vec<(i64, bool)> = resolved
        .iter()
        .filter(|&&(_, _, dup)| !dup)
        .map(|&(t, lit, _)| (t, lit))
        .collect();
    let window_start = window[0].0;
    let onset = distinct
        .windows(2)
        .find(|pair| pair[0].1 && pair[1].1)
        .map(|pair| pair[0].0);
    let charge_from = onset.unwrap_or(window_start);
    let mature_lead = charge_from - window_start;
    let mut worst = 0_i64;
    let mut run: Vec<i64> = Vec::new();
    for &(t, lit, _) in &resolved {
        if t < charge_from {
            continue;
        }
        if lit {
            if let (Some(first), Some(last)) = (run.first(), run.last()) {
                worst = worst.max(last - first + nominal);
            }
            run.clear();
        } else {
            run.push(t);
        }
    }
    if let (Some(first), Some(last)) = (run.first(), run.last()) {
        let allowance = if onset.is_some() {
            EXHALE_CLOSE_ALLOWANCE_US
        } else {
            0
        };
        worst = worst.max((last - first + nominal - allowance).max(0));
    }
    let window_end = window[window.len() - 1].0;
    let samples: BTreeSet<i64> = window
        .iter()
        .map(|&(t, _)| t)
        .chain(
            index
                .ticks
                .iter()
                .copied()
                .filter(|&t| window_start <= t && t <= window_end),
        )
        .collect();
    let samples: Vec<i64> = samples.into_iter().collect();
    let hole = samples.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0);
    DrivenWindow {
        dark_us: worst,
        frames: window.len(),
        sampling_hole_us: hole,
        nominal_dt_us: nominal,
        mature_lead_us: mature_lead,
    }
}

/// The strongest thin ribbon component over the saturated set: 8-connected, bridging one-pixel seams along the row (never vertically, which would merge separate glyph bodies).
#[must_use]
pub fn thin_ribbon_component(w: usize, h: usize, dynamic: &BTreeSet<usize>, cell: Cell) -> Shape {
    let min_width = RIBBON_COMPONENT_MIN_WIDTH_FLOOR.max((3 * cell.w).div_ceil(4));
    let min_pixels = RIBBON_COMPONENT_MIN_PIXELS_FLOOR.max(min_width + 2);
    let mut unseen = dynamic.clone();
    let mut best = Shape::NONE;
    while let Some(seed) = unseen.pop_first() {
        let mut stack = vec![seed];
        let mut pixels = 0;
        let (mut x0, mut y0) = (seed % w, seed / w);
        let (mut x1, mut y1) = (x0, y0);
        while let Some(point) = stack.pop() {
            pixels += 1;
            let (y, x) = (point / w, point % w);
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            for ny in y.saturating_sub(1)..(y + 2).min(h) {
                for nx in x.saturating_sub(1)..(x + 2).min(w) {
                    let n = ny * w + nx;
                    if unseen.remove(&n) {
                        stack.push(n);
                    }
                }
            }
            for nx in [x.checked_sub(2), Some(x + 2)].into_iter().flatten() {
                if nx < w {
                    let n = y * w + nx;
                    if unseen.remove(&n) {
                        stack.push(n);
                    }
                }
            }
        }
        let candidate = Shape::new(pixels, x1 - x0 + 1, y1 - y0 + 1);
        if candidate.pixels >= min_pixels
            && candidate.width >= min_width
            && candidate.height <= RIBBON_COMPONENT_MAX_HEIGHT
            && candidate > best
        {
            best = candidate;
        }
    }
    best
}

/// A dense saturated run attached to the solid block cursor.
#[must_use]
pub fn attached_ribbon_run(
    w: usize,
    h: usize,
    dynamic: &BTreeSet<usize>,
    changed: &BTreeSet<usize>,
    cell: Cell,
) -> RibbonRun {
    let by_row = |points: &BTreeSet<usize>| {
        let mut rows: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
        for &p in points {
            rows.entry(p / w).or_default().insert(p % w);
        }
        rows
    };
    let points_by_row = by_row(dynamic);
    let changed_by_row = by_row(changed);
    let empty = BTreeSet::new();
    let mut cursor_spans: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
    for top in 0..(h + 1).saturating_sub(cell.h) {
        let mut common = changed_by_row.get(&top).unwrap_or(&empty).clone();
        if common.len() < cell.w {
            continue;
        }
        for y in top + 1..top + cell.h {
            let row = changed_by_row.get(&y).unwrap_or(&empty);
            common.retain(|x| row.contains(x));
            if common.len() < cell.w {
                break;
            }
        }
        if common.len() < cell.w {
            continue;
        }
        let bottom = top + cell.h - 1;
        let ordered: Vec<usize> = common.into_iter().collect();
        let (mut start, mut previous) = (ordered[0], ordered[0]);
        for x in ordered[1..].iter().copied().map(Some).chain([None]) {
            if x == Some(previous + 1) {
                previous += 1;
                continue;
            }
            if previous - start + 1 >= cell.w {
                cursor_spans
                    .entry(bottom)
                    .or_default()
                    .push((start, previous));
            }
            if let Some(x) = x {
                start = x;
                previous = x;
            }
        }
    }
    let min_width = ATTACHED_RIBBON_MIN_CELLS * cell.w;
    let mut best = RibbonRun::NONE;
    for (&y, spans) in &cursor_spans {
        let xs: Vec<usize> = points_by_row
            .get(&y)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let mut start = 0;
        for end in 1..=xs.len() {
            if end < xs.len() && xs[end] - xs[end - 1] <= 2 {
                continue;
            }
            let pixels = end - start;
            let (lo, hi) = (xs[start], xs[end - 1]);
            let width = hi - lo + 1;
            let attached = spans
                .iter()
                .any(|&(left, right)| lo <= right + 2 && hi + 2 >= left);
            if attached
                && width >= min_width
                && ATTACHED_RIBBON_DENSITY_DEN * pixels >= ATTACHED_RIBBON_DENSITY_NUM * width
                && (width, pixels) > (best.width, best.pixels)
            {
                best = RibbonRun::new(pixels, width, y);
            }
            start = end;
        }
    }
    best
}

/// Whether one frame carries ribbon geometry (thin or attached).
#[must_use]
pub const fn ribbon_present(metrics: &DynamicMetrics) -> bool {
    metrics.ribbon_component.pixels > 0 || metrics.ribbon_run.pixels > 0
}

/// Whether one frame carries trail light for the continuity charge: ribbon geometry, or the band's own population at [`RIBBON_LIT_MIN_PX`].
#[must_use]
pub const fn driven_lit(metrics: &DynamicMetrics) -> bool {
    ribbon_present(metrics) || metrics.band_ink >= RIBBON_LIT_MIN_PX
}

/// One frame's raster against the baseline.
#[must_use]
pub fn dynamic_metrics(
    base: &Frame,
    frame: &Frame,
    cell: Cell,
    exclude: &[PixelBox],
) -> DynamicMetrics {
    let (w, h) = (frame.width(), frame.height());
    let mut m = DynamicMetrics::default();
    let mut dynamic = BTreeSet::new();
    let mut changed = BTreeSet::new();
    let mut sat = SatMap::new();
    for y in 0..h {
        if frame.row(y) == base.row(y) {
            continue;
        }
        let mut row_hues = HueSet::default();
        for x in 0..w {
            let p = y * w + x;
            let px = frame.pixel(p);
            if px == base.pixel(p) {
                continue;
            }
            changed.insert(p);
            let Some(degrees) = saturated_hue_degrees(px) else {
                continue;
            };
            let bucket = HueSet::bucket(degrees);
            m.ink += 1;
            dynamic.insert(p);
            if cyan_band_hue(px).is_some() {
                m.cyan_pixels += 1;
                if m.cyan_worst
                    .is_none_or(|c| (degrees - 180.0).abs() < (c.degrees - 180.0).abs())
                {
                    m.cyan_worst = Some(CyanSample { degrees, rgb: px });
                }
            }
            m.hues.insert(bucket);
            row_hues.insert(bucket);
            sat.insert(p, degrees);
        }
        m.row_hue_peak = m.row_hue_peak.max(row_hues.len());
    }
    m.row_hue_separation = object_row_hue_separation(w, h, &sat).max(banded_row_span(w, &sat));
    m.ribbon_component = thin_ribbon_component(w, h, &dynamic, cell);
    m.ribbon_run = attached_ribbon_run(w, h, &dynamic, &changed, cell);
    let (band_ink, band_hues, animal_ink) = band_ink_partition(w, h, &sat, cell.h, exclude);
    m.band_ink = band_ink;
    m.band_hues = band_hues;
    m.animal_ink = animal_ink;
    m
}

/// Something the scanner noticed on one frame, for the reader of a red row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameNote {
    /// A mature claim past the ledger's last driven frame: the authored exhale.
    RibbonExhale { bands: u64 },
    /// A driven mature claim over too little band ink.
    RibbonUnlit { band_ink: usize },
    /// The companion was found under a relaxed (text-height) floor.
    CompactPetWitness,
    /// ...and outside the isolating frames, through the resting-pose bind.
    RestingPoseWitness { pose: Option<String> },
    /// The companion component has fused with the typed row.
    CompanionFused { width: usize, limit: usize },
    /// The engine claims a companion the raster does not carry.
    CompanionMissing,
    /// An isolating frame carries two or more companion-shaped components.
    CompanionStack { shapes: Vec<Shape> },
}

/// What one frame read.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameReport {
    pub name: String,
    pub metrics: DynamicMetrics,
    pub companion: Shape,
    pub duplicate: bool,
    pub notes: Vec<FrameNote>,
}

impl std::fmt::Display for FrameReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = &self.metrics;
        write!(
            f,
            "{}: dynamic_saturated={} hue_buckets={:?} row_hue_peak={} \
             row_hue_separation={:.1}deg ribbon_component={}/{}x{} ribbon_run={}/{}@{} \
             band_ink={} animal_ink={} cyan={} companion={}/{}x{}",
            self.name,
            m.ink,
            m.hues.buckets(),
            m.row_hue_peak,
            m.row_hue_separation,
            m.ribbon_component.pixels,
            m.ribbon_component.width,
            m.ribbon_component.height,
            m.ribbon_run.pixels,
            m.ribbon_run.width,
            m.ribbon_run.row,
            m.band_ink,
            m.animal_ink,
            m.cyan_pixels,
            self.companion.pixels,
            self.companion.width,
            self.companion.height,
        )?;
        if self.duplicate {
            f.write_str(" identical-to-previous")?;
        }
        for note in &self.notes {
            write!(f, " {note:?}")?;
        }
        Ok(())
    }
}

/// The whole take's reading: what the verdict is decided from.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanReport {
    pub frames: usize,
    pub cell: Cell,
    /// The busiest single frame's ink and its hue count.
    pub best_ink: usize,
    pub best_hues: usize,
    pub total_ink: usize,
    pub union_hues: usize,
    /// The busiest single frame's cyan count, the frames with any, and the take's total.
    pub cyan_px: usize,
    pub cyan_frames: usize,
    pub cyan_total: usize,
    /// The sample nearest pure cyan across the take.
    pub cyan_hue: Option<f64>,
    pub rainbow_frames: usize,
    pub rainbow_run: usize,
    pub rainbow_gap: usize,
    pub driven: DrivenWindow,
    pub status_frames: usize,
    pub ribbon_claimed: usize,
    /// Frames the driven mature claim covered: a zero contradiction count is only evidence when this is not zero.
    pub ribbon_bound: usize,
    pub ribbon_window_hues: usize,
    /// Bound frames that were unlit, in runs of two or more.
    pub ribbon_dark: usize,
    pub pet_claimed: usize,
    pub pet_missing: usize,
    pub pet_final_run: usize,
    pub cat_claimed: usize,
    pub cat_missing: usize,
    pub cat_gap: usize,
    pub cat_last_present: bool,
    pub cat_final_run: usize,
    pub companion_frames: usize,
    pub companion_stack: usize,
    pub companion_fused: usize,
    /// The companion found on frame 0.
    pub base_comp: Shape,
    pub per_frame: Vec<FrameReport>,
}

/// Why a take could not be scanned.
#[derive(Debug)]
pub enum ScanError {
    NoFrames(PathBuf),
    Frame {
        name: String,
        error: FrameError,
    },
    StatusCount {
        rows: usize,
        frames: usize,
    },
    MalformedStatus {
        frame: usize,
        error: StatusError,
    },
    SizeMismatch {
        name: String,
        got: (usize, usize),
        want: (usize, usize),
    },
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFrames(dir) => write!(f, "no PNG frames in {}", dir.display()),
            Self::Frame { name, error } => write!(f, "{name}: {error}"),
            Self::StatusCount { rows, frames } => {
                write!(f, "{rows} status rows for {frames} captured frames")
            }
            Self::MalformedStatus { frame, error } => {
                write!(f, "frame {frame}: malformed trail status: {error}")
            }
            Self::SizeMismatch { name, got, want } => write!(
                f,
                "{name} is {}x{}, frame 0 is {}x{}",
                got.0, got.1, want.0, want.1
            ),
        }
    }
}

impl std::error::Error for ScanError {}

/// What a take is scanned with besides its frames.
#[derive(Clone, Debug)]
pub struct TakeContext {
    pub cell: Cell,
    /// One `trail status` row per frame (image bursts).
    pub statuses: Option<Vec<StatusRow>>,
    /// The recording's index (video takes).
    pub index: Option<RecordingIndex>,
    /// The `video status` bracket around the keystrokes, in milliseconds.
    pub driven_ms: Option<(i64, i64)>,
}

/// The PNG frames in `dir`, sorted by name.
/// # Errors
/// The directory cannot be read, or holds no PNG.
pub fn take_frames(dir: &Path) -> Result<Vec<PathBuf>, ScanError> {
    let mut pngs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|_| ScanError::NoFrames(dir.to_path_buf()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .collect();
    pngs.sort();
    if pngs.is_empty() {
        return Err(ScanError::NoFrames(dir.to_path_buf()));
    }
    Ok(pngs)
}

/// Scan the PNG take in `dir` (decoded one frame at a time).
/// # Errors
/// As [`scan_frames`], and a frame that cannot be decoded.
pub fn scan_dir(dir: &Path, ctx: &TakeContext) -> Result<ScanReport, ScanError> {
    let pngs = take_frames(dir)?;
    let names: Vec<String> = pngs
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    let frames = pngs.iter().zip(&names).map(|(path, name)| {
        Frame::load(path).map_err(|error| ScanError::Frame {
            name: name.clone(),
            error,
        })
    });
    scan_frames(&names, frames, ctx)
}

/// Scan one take: `names[i]` is frame `i`'s file name (the index's key), and `frames` yields the decoded frames in the same order.
/// # Errors
/// No frames, a status row count that differs from the frame count, a malformed status row, a frame of another size than frame 0, or a frame that could not be decoded.
#[allow(clippy::too_many_lines)]
pub fn scan_frames(
    names: &[String],
    frames: impl Iterator<Item = Result<Frame, ScanError>>,
    ctx: &TakeContext,
) -> Result<ScanReport, ScanError> {
    let n = names.len();
    if n == 0 {
        return Err(ScanError::NoFrames(PathBuf::new()));
    }
    let cell = ctx.cell;
    let statuses = ctx.statuses.as_deref();
    if let Some(rows) = statuses
        && rows.len() != n
    {
        return Err(ScanError::StatusCount {
            rows: rows.len(),
            frames: n,
        });
    }
    let driven_last: i64 = match statuses {
        Some(rows) => driven_last_frame(rows)
            .map_err(|error| ScanError::MalformedStatus { frame: 0, error })?
            .map_or(-1, |f| i64::try_from(f).unwrap_or(i64::MAX)),
        None => -1,
    };
    let mut frames = frames;
    let base = frames.next().ok_or(ScanError::NoFrames(PathBuf::new()))??;
    let background = frame_background(&base);
    let alone_max_w = COMPANION_ALONE_MAX_WIDTH_CELL_HEIGHTS * cell.h;

    let mut r = ScanReport {
        frames: n,
        cell,
        best_ink: 0,
        best_hues: 0,
        total_ink: 0,
        union_hues: 0,
        cyan_px: 0,
        cyan_frames: 0,
        cyan_total: 0,
        cyan_hue: None,
        rainbow_frames: 0,
        rainbow_run: 0,
        rainbow_gap: 0,
        driven: DrivenWindow::NOT_MEASURED,
        status_frames: statuses.map_or(0, <[StatusRow]>::len),
        ribbon_claimed: 0,
        ribbon_bound: 0,
        ribbon_window_hues: 0,
        ribbon_dark: 0,
        pet_claimed: 0,
        pet_missing: 0,
        pet_final_run: 0,
        cat_claimed: 0,
        cat_missing: 0,
        cat_gap: 0,
        cat_last_present: false,
        cat_final_run: 0,
        companion_frames: 0,
        companion_stack: 0,
        companion_fused: 0,
        base_comp: Shape::NONE,
        per_frame: Vec::with_capacity(n),
    };
    let mut union_hues = HueSet::default();
    let mut cyan_worst: Option<CyanSample> = None;
    let mut rainbow_flags = Vec::with_capacity(n);
    let mut lit_flags = Vec::with_capacity(n);
    let mut duplicate_flags = Vec::with_capacity(n);
    let mut dark_candidates = Vec::with_capacity(n);
    let mut bound_frames = 0_usize;
    let mut window_hues = HueSet::default();
    let mut bound_lit = Vec::new();
    let mut pet_visibility = Vec::new();
    let mut cat_visibility = Vec::new();
    let mut prev: Option<Frame> = None;
    let mut prev_metrics = DynamicMetrics::default();

    for frame_no in 0..n {
        let frame = if frame_no == 0 {
            base.clone()
        } else {
            frames
                .next()
                .ok_or_else(|| ScanError::NoFrames(PathBuf::new()))??
        };
        let name = &names[frame_no];
        if (frame.width(), frame.height()) != (base.width(), base.height()) {
            return Err(ScanError::SizeMismatch {
                name: name.clone(),
                got: (frame.width(), frame.height()),
                want: (base.width(), base.height()),
            });
        }
        let duplicate = frame_no > 0 && prev.as_ref() == Some(&frame);
        let mut notes = Vec::new();
        let (mut pet, mut cat, mut bands) = (false, false, 0_u64);
        let mut located_pet = false;
        let mut claimed_body = None;
        let mut claimed_pose: Option<String> = None;
        if let Some(rows) = statuses {
            let status = &rows[frame_no];
            let malformed = |error| ScanError::MalformedStatus {
                frame: frame_no,
                error,
            };
            pet = status.bool("pet_active").map_err(malformed)?;
            cat = status.bool("cat_active").map_err(malformed)?;
            bands = status.uint("ribbon_hue_bands").map_err(malformed)?;
            located_pet = pet && status.value("pet_body").is_some();
            if located_pet {
                claimed_body = status.pet_body().map_err(malformed)?;
                claimed_pose = status.pet_pose().map(str::to_string);
            }
            r.pet_claimed += usize::from(pet);
            r.cat_claimed += usize::from(cat);
        }
        let companion_active = pet || cat;
        let isolating = compact_pet_witness_frame(frame_no, n);

        let mut component = Shape::NONE;
        let mut animal_box = None;
        let mut strict: Vec<Component> = Vec::new();
        if companion_active {
            if located_pet {
                component = located_pet_component(
                    &frame,
                    background,
                    claimed_body,
                    frame_no,
                    n,
                    claimed_pose.as_deref(),
                    Some(cell.w),
                );
                animal_box = if component.is_some() {
                    claimed_body
                } else {
                    None
                };
                if isolating {
                    strict = tall_component_records(&frame, background, COMP_MIN_H, None);
                }
            } else {
                strict = tall_component_records(&frame, background, COMP_MIN_H, None);
                component =
                    companion_component(&frame, background, pet, frame_no, n, Some(&strict), cat);
                animal_box = companion_component_box(
                    &frame,
                    background,
                    pet,
                    frame_no,
                    n,
                    Some(&strict),
                    cat,
                );
            }
            if frame_no == 0 {
                r.base_comp = component;
            }
        }
        let fused = !located_pet && component.width > alone_max_w;
        let exclude: Vec<PixelBox> = animal_box.filter(|_| !fused).into_iter().collect();

        let metrics = if frame_no == 0 || duplicate {
            // Frame 0 is the baseline; a resample inherits its predecessor's
            // shape and adds no ink.
            prev_metrics.clone()
        } else {
            dynamic_metrics(&base, &frame, cell, &exclude)
        };
        let has_ribbon = ribbon_present(&metrics);
        if frame_no > 0 && !duplicate {
            r.total_ink += metrics.ink;
            union_hues.union_with(metrics.hues);
            if metrics.ink > r.best_ink
                || (metrics.ink == r.best_ink && metrics.hues.len() > r.best_hues)
            {
                r.best_ink = metrics.ink;
                r.best_hues = metrics.hues.len();
            }
            r.cyan_total += metrics.cyan_pixels;
            r.cyan_px = r.cyan_px.max(metrics.cyan_pixels);
            if metrics.cyan_pixels > 0 {
                r.cyan_frames += 1;
                if let Some(w) = metrics.cyan_worst
                    && cyan_worst
                        .is_none_or(|c| (w.degrees - 180.0).abs() < (c.degrees - 180.0).abs())
                {
                    cyan_worst = Some(w);
                }
            }
        }

        let frame_i = i64::try_from(frame_no).unwrap_or(i64::MAX);
        if statuses.is_some() && bands >= 4 {
            r.ribbon_claimed += 1;
            if frame_i > driven_last - RIBBON_CLOSE_GUARD_FRAMES {
                dark_candidates.push(false);
                notes.push(FrameNote::RibbonExhale { bands });
            } else {
                bound_frames += 1;
                r.ribbon_bound += 1;
                let dark = frame_no > 0 && metrics.band_ink < RIBBON_LIT_MIN_PX;
                dark_candidates.push(dark);
                if frame_no > 0 {
                    window_hues.union_with(metrics.band_hues);
                    bound_lit.push(!dark);
                }
                if dark {
                    notes.push(FrameNote::RibbonUnlit {
                        band_ink: metrics.band_ink,
                    });
                }
            }
        } else {
            dark_candidates.push(false);
        }

        if companion_active {
            if component.is_some() {
                r.companion_frames += 1;
                if component.height < COMP_MIN_H {
                    notes.push(FrameNote::CompactPetWitness);
                    if located_pet && !isolating {
                        notes.push(FrameNote::RestingPoseWitness {
                            pose: claimed_pose.clone(),
                        });
                    }
                }
                if fused {
                    r.companion_fused += 1;
                    notes.push(FrameNote::CompanionFused {
                        width: component.width,
                        limit: alone_max_w,
                    });
                }
            } else {
                r.pet_missing += usize::from(pet);
                r.cat_missing += usize::from(cat);
                notes.push(FrameNote::CompanionMissing);
            }
            if strict.len() >= 2 && isolating {
                r.companion_stack += 1;
                notes.push(FrameNote::CompanionStack {
                    shapes: strict.iter().take(3).map(|c| c.shape).collect(),
                });
            }
        }
        if statuses.is_some() {
            cat_visibility.push(cat && component.is_some());
            pet_visibility.push(pet && component.is_some());
        }
        rainbow_flags.push(has_ribbon);
        lit_flags.push(driven_lit(&metrics));
        duplicate_flags.push(duplicate);
        r.per_frame.push(FrameReport {
            name: name.clone(),
            metrics: metrics.clone(),
            companion: component,
            duplicate,
            notes,
        });
        prev = Some(frame);
        prev_metrics = metrics;
    }

    (r.rainbow_frames, r.rainbow_run, r.rainbow_gap) =
        rainbow_summary(&rainbow_flags, &duplicate_flags);
    if statuses.is_some() && bound_frames > 0 {
        r.rainbow_gap = longest_interior_gap(&bound_lit);
    }
    let mut run = 0;
    for dark in dark_candidates.into_iter().chain([false]) {
        if dark {
            run += 1;
        } else {
            if run >= 2 {
                r.ribbon_dark += run;
            }
            run = 0;
        }
    }
    r.ribbon_window_hues = window_hues.len();
    r.union_hues = union_hues.len();
    r.cyan_hue = cyan_worst.map(|c| c.degrees);
    r.driven = if ctx.index.is_some() {
        driven_window_darkness(
            ctx.index.as_ref(),
            ctx.driven_ms,
            names,
            &lit_flags,
            &duplicate_flags,
        )
    } else {
        DrivenWindow::NOT_MEASURED
    };
    r.cat_gap = longest_interior_gap(&cat_visibility);
    r.cat_last_present = cat_visibility.last().copied().unwrap_or(false);
    r.cat_final_run = trailing_true_run(&cat_visibility);
    r.pet_final_run = trailing_true_run(&pet_visibility);
    Ok(r)
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;

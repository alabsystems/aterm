// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **Bake-time vector rasterizer** for the cat-art v4 glyph pipeline: fill absolute
//! cubic-Bézier/line paths into a [`Tile`] with scanline even-odd coverage.
//!
//! The v4 cat glyphs are exactly-traced SVG paths (vtracer cutout output) codegen'd
//! into const drawlists; at bake time each layer's paths are rasterized here into the
//! same straight-alpha RGBA8 [`Tile`] the procedural cat baker draws on. The AA idiom
//! matches [`Tile::fill`]: 4×4 subsamples per pixel, coverage → `alpha·hit/16` through
//! [`Tile::over`].
//!
//! Semantics (normative for the glyph asset format):
//! - Each element of `paths` is ONE path (possibly several subpaths via `Move`), filled
//!   with the **even-odd** rule — subpath holes (donuts, eye cutouts) work regardless
//!   of winding direction.
//! - A layer's coverage is the **union** across its paths (a subsample is covered if it
//!   is inside *any* path). vtracer cutout clusters are disjoint-to-adjacent, so
//!   merging a dozen same-colour fragments into one layer never self-cancels and never
//!   double-blends the layer's alpha.
//! - Unclosed subpaths are implicitly closed (standard fill semantics).
//! - Coordinates are sanitized: non-finite → 0, magnitude clamped to [`COORD_LIMIT`].
//!   The scanline itself only ever visits tile-bounded rows/columns, so hostile
//!   coordinates cannot blow up time or memory.
//!
//! This is bake-time-only code: floats appear in parameters and scratch buffers, never
//! in any `Eq`/hashed key type (the `BakeKey` discipline of the cat baker).

use crate::atlas::Tile;
use crate::clampf;

/// One absolute path command in the glyph's own normalized frame (0..1, y down),
/// scaled into device space by the [`PathTransform`] at fill time.
///
/// Deliberately `PartialEq` only (floats) — never put this in an `Eq`'d key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCmd {
    /// Start a new subpath at `(x, y)` (implicitly closing any open one at fill time).
    Move(f32, f32),
    /// Straight segment to `(x, y)`.
    Line(f32, f32),
    /// Cubic Bézier `(x1, y1, x2, y2, x, y)` — two control points, then the endpoint.
    Cubic(f32, f32, f32, f32, f32, f32),
    /// Close the current subpath back to its `Move` point.
    Close,
}

/// The side length of a glyph's own fixed-point frame: quantized path coordinates
/// live in the integer grid `0..=FIXED_ONE` (`x = round(px/viewbox * FIXED_ONE)`), so
/// the whole const drawlist is integer — `Eq`-safe and byte-deterministic (the codegen
/// contract, docs/cat-art-v4-design.md §1).
pub const FIXED_ONE: u16 = 4096;

/// One absolute path command in a glyph's **fixed-point** `0..=FIXED_ONE` frame — the
/// integer counterpart of [`PathCmd`] that the codegen'd cat drawlists store.
///
/// Being all-integer, this is fully `Eq`/`Hash`-safe (unlike [`PathCmd`]), so it may
/// appear in `const` data and hashed keys. [`fill_path_fixed`] maps it into device
/// space at bake time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathSeg {
    /// Start a new subpath at `(x, y)` (implicitly closing any open one at fill time).
    Move(u16, u16),
    /// Straight segment to `(x, y)`.
    Line(u16, u16),
    /// Cubic Bézier `(x1, y1, x2, y2, x, y)` — two control points, then the endpoint.
    Cubic(u16, u16, u16, u16, u16, u16),
    /// Close the current subpath back to its `Move` point.
    Close,
}

/// Affine scale+offset mapping glyph coordinates into device (tile) pixels:
/// `device = (x·scale_x + dx, y·scale_y + dy)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathTransform {
    pub scale_x: f32,
    pub scale_y: f32,
    pub dx: f32,
    pub dy: f32,
}

impl PathTransform {
    /// Map the glyph's 0..1 frame onto a `w`×`h` pixel box at the tile origin.
    #[must_use]
    pub fn fit(w: u32, h: u32) -> Self {
        Self {
            scale_x: w as f32,
            scale_y: h as f32,
            dx: 0.0,
            dy: 0.0,
        }
    }
}

/// Device-space coordinate bound: anything larger (or non-finite) is clamped/zeroed
/// before rasterization, so degenerate assets stay safe and fast.
pub const COORD_LIMIT: f32 = 1.0e6;

/// Cubic flattening tolerance in device pixels (~0.15 px at target scale keeps the
/// error invisible under the 4×4 supersampling).
const FLATTEN_TOL: f32 = 0.15;

/// A non-horizontal device-space edge in canonical top-to-bottom form, tagged with the
/// path it came from (even-odd parity is per path; the layer unions paths).
#[derive(Clone, Copy)]
struct Edge {
    path: u32,
    ytop: f32,
    ybot: f32,
    xtop: f32,
    dxdy: f32,
}

/// Fill `paths` (even-odd per path, union across paths) into `tile` with `rgb` at
/// `alpha`, 4×4-supersampled, blended through [`Tile::over`]/[`Tile::over_run`].
/// Degenerate input (empty paths, zero-area geometry, off-tile geometry) is a no-op.
///
/// The scanline core is built for the perf gate (30-layer glyph at 160×100 well under
/// 1 ms): an active-edge list amortizes edge lookups across the monotonically
/// increasing sub-scanlines, union spans land in a difference-array coverage
/// accumulator (O(1) per span interior), and full-coverage pixel runs blend as one
/// [`Tile::over_run`] (a 4-byte pattern fill when opaque).
pub fn fill_path(
    tile: &mut Tile,
    paths: &[&[PathCmd]],
    rgb: (f32, f32, f32),
    alpha: f32,
    t: PathTransform,
) {
    if alpha <= 0.0 || tile.width() == 0 || tile.height() == 0 || paths.is_empty() {
        return;
    }
    // Flatten every path to device-space edges; track the union bbox.
    let mut bbox = BBox::empty();
    // Size the edge list up front. It otherwise doubles from empty to several
    // hundred entries on EVERY call — one call per glyph layer per bake, ×5 for the
    // haloed outline/whisker layers — paying a realloc+memcpy at each doubling step.
    // Every command contributes at most one edge except `Cubic`, which flattens to
    // `n` segments (`flatten_cubic` bounds `n` at 64; it is small single digits at
    // glyph curvature and FLATTEN_TOL), so 4-per-cubic is a cheap first fit that
    // costs at most one growth on a big layer. `reserve` is a pure allocation hint:
    // it cannot change which edges get pushed, only how often the `Vec` grows, so
    // the raster is untouched.
    let est = paths.iter().fold(0usize, |n, p| {
        p.iter().fold(n, |n, cmd| {
            let e = match *cmd {
                PathCmd::Cubic(..) => 4,
                _ => 1,
            };
            n.saturating_add(e)
        })
    });
    let mut edges: Vec<Edge> = Vec::with_capacity(est);
    for (pid, p) in paths.iter().enumerate() {
        flatten_path(p, pid as u32, t, &mut bbox, &mut edges);
    }
    let Some((x0, x1, y0, y1)) = bbox.pixel_range(tile.width(), tile.height()) else {
        return;
    };
    let width = x1 - x0;
    edges.sort_unstable_by(|a, b| a.ytop.total_cmp(&b.ytop));
    let mut active: Vec<Edge> = Vec::new();
    let mut next = 0usize;
    // Per-sub-scanline scratch: (path, x) crossings and merged union spans.
    let mut xs: Vec<(u32, f32)> = Vec::new();
    let mut spans: Vec<(f32, f32)> = Vec::new();
    // Row coverage accumulators (reset in the flush pass, so rows cost O(touched)):
    // `diff` takes ±4 at full-pixel span boundaries (prefix sum = interior coverage),
    // `extra` takes the per-subsample counts of the few partial boundary pixels.
    let mut diff = vec![0i32; width + 1];
    let mut extra = vec![0i32; width];
    const INV16: f32 = 1.0 / 16.0;
    for py in y0..y1 {
        // Touched pixel range of this row (union over its 4 sub-scanlines).
        let mut lo_t = width;
        let mut hi_t = 0usize;
        for sub in 0..4u32 {
            // ×0.25 is bit-identical to /4 (power of two) — same sample lattice as
            // `Tile::fill`.
            let sy = py as f32 + (sub as f32 + 0.5) * 0.25;
            // Activate edges whose top has been reached; retire the expired.
            while let Some(e) = edges.get(next)
                && e.ytop <= sy
            {
                if e.ybot > sy {
                    active.push(*e);
                }
                next += 1;
            }
            active.retain(|e| e.ybot > sy);
            if active.is_empty() {
                continue;
            }
            // Crossings, grouped by path for even-odd pairing.
            xs.clear();
            for e in &active {
                xs.push((e.path, e.xtop + (sy - e.ytop) * e.dxdy));
            }
            xs.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
            spans.clear();
            let mut i = 0usize;
            while let (Some(a), Some(b)) = (xs.get(i), xs.get(i + 1)) {
                if a.0 == b.0 {
                    spans.push((a.1, b.1));
                    i += 2;
                } else {
                    i += 1; // unpaired trailing crossing of a path: drop it
                }
            }
            // Union-merge the spans (already sorted per path; re-sort across paths),
            // then accumulate each disjoint span into the row.
            spans.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
            let mut cur: Option<(f32, f32)> = None;
            for &(a, b) in spans.iter() {
                if b <= a {
                    continue;
                }
                cur = match cur {
                    None => Some((a, b)),
                    Some((ca, cb)) if a <= cb => Some((ca, cb.max(b))),
                    Some((ca, cb)) => {
                        add_span(&mut diff, &mut extra, x0, ca, cb, &mut lo_t, &mut hi_t);
                        Some((a, b))
                    }
                };
            }
            if let Some((ca, cb)) = cur {
                add_span(&mut diff, &mut extra, x0, ca, cb, &mut lo_t, &mut hi_t);
            }
        }
        if lo_t >= hi_t {
            continue;
        }
        // Flush the row: prefix-sum `diff` into coverage, fold in `extra`, reset both
        // (so the next row starts clean without a full-width memset), and blend —
        // full-coverage runs go through `over_run`, partial pixels through `over`.
        if let (Some(ds), Some(es)) = (diff.get_mut(lo_t..hi_t), extra.get_mut(lo_t..hi_t)) {
            let mut run = 0i32;
            for (d, e) in ds.iter_mut().zip(es.iter_mut()) {
                run += *d;
                *d = 0;
                *e = (*e + run).clamp(0, 16);
            }
        }
        if let Some(d) = diff.get_mut(hi_t) {
            *d = 0;
        }
        let mut i = lo_t;
        while i < hi_t {
            let c = extra.get_mut(i).map_or(0, std::mem::take);
            if c <= 0 {
                i += 1;
            } else if c >= 16 {
                let mut j = i + 1;
                while j < hi_t && extra.get(j).copied() == Some(16) {
                    if let Some(e) = extra.get_mut(j) {
                        *e = 0;
                    }
                    j += 1;
                }
                tile.over_run((x0 + i) as i32, py as i32, (j - i) as u32, rgb, alpha);
                i = j;
            } else {
                tile.over((x0 + i) as i32, py as i32, rgb, alpha * c as f32 * INV16);
                i += 1;
            }
        }
    }
}

/// Fill fixed-point [`PathSeg`] drawlists (the codegen'd cat glyph layers) into `tile`
/// — the u16 counterpart of [`fill_path`]. Each coordinate in the glyph's `0..=FIXED_ONE`
/// frame is mapped to its `0..1` normalized position (`v / FIXED_ONE`) and then through
/// the same [`PathTransform`]/scanline core, so the raster is byte-identical to filling
/// the equivalent [`PathCmd`] paths. This is the `fill_path` overload the bake path calls
/// on `PathSeg` slices directly (docs/cat-art-v4-design.md §1).
pub fn fill_path_fixed(
    tile: &mut Tile,
    paths: &[&[PathSeg]],
    rgb: (f32, f32, f32),
    alpha: f32,
    t: PathTransform,
) {
    if alpha <= 0.0 || tile.width() == 0 || tile.height() == 0 || paths.is_empty() {
        return;
    }
    const INV_FIXED: f32 = 1.0 / FIXED_ONE as f32;
    let c = |v: u16| f32::from(v) * INV_FIXED;
    // Lower each fixed-point path to `PathCmd`s in the shared 0..1 frame, then reuse
    // the exact `fill_path` core (the drawlists are const, so this stays off any
    // `Eq`'d key path).
    //
    // The lowering is 1:1 — exactly one `PathCmd` per `PathSeg` — so it goes into ONE
    // exactly-sized flat buffer and the per-path slices are cut back out of it by
    // offset: two allocations per call instead of one `Vec` per path plus the outer
    // `Vec` plus the ref list. The per-path GROUPING is preserved exactly (each `refs`
    // entry is still one whole path, in the original order) — `fill_path` fills
    // even-odd PER PATH and unions ACROSS paths, so handing it one merged slice would
    // turn overlapping subpaths into holes. Same `f32` expression applied to the same
    // values in the same order as before, so the commands are bit-identical and the
    // raster is unchanged.
    let total = paths.iter().fold(0usize, |n, p| n.saturating_add(p.len()));
    let mut flat: Vec<PathCmd> = Vec::with_capacity(total);
    for p in paths {
        flat.extend(p.iter().map(|seg| match *seg {
            PathSeg::Move(x, y) => PathCmd::Move(c(x), c(y)),
            PathSeg::Line(x, y) => PathCmd::Line(c(x), c(y)),
            PathSeg::Cubic(x1, y1, x2, y2, x, y) => {
                PathCmd::Cubic(c(x1), c(y1), c(x2), c(y2), c(x), c(y))
            }
            PathSeg::Close => PathCmd::Close,
        }));
    }
    let mut refs: Vec<&[PathCmd]> = Vec::with_capacity(paths.len());
    let mut off = 0usize;
    for p in paths {
        let end = off.saturating_add(p.len());
        // Always in range — `flat` was built from exactly these lengths, in order.
        refs.push(flat.get(off..end).unwrap_or(&[]));
        off = end;
    }
    fill_path(tile, &refs, rgb, alpha, t);
}

/// Accumulate one disjoint span `[a, b)` (device x) into the row: ±4 into `diff` for
/// the fully-covered pixel range, exact subsample counts into `extra` for the (at most
/// a few) partial boundary pixels. Grows the row's touched range.
fn add_span(
    diff: &mut [i32],
    extra: &mut [i32],
    x0: usize,
    a: f32,
    b: f32,
    lo_t: &mut usize,
    hi_t: &mut usize,
) {
    let width = extra.len();
    let clamp_px = |v: f32| -> usize {
        // Bounded float → index: sanitized coords keep `v` finite and small.
        clampf(v - x0 as f32, 0.0, width as f32) as usize
    };
    // Pixels whose subsample lattice (px + (i+0.5)/4) intersects [a, b) at all.
    let p_lo = clamp_px((a - 0.875).ceil());
    let p_hi = clamp_px((b - 0.125).ceil());
    if p_lo >= p_hi {
        return;
    }
    *lo_t = (*lo_t).min(p_lo);
    *hi_t = (*hi_t).max(p_hi);
    // Fully-covered pixels: px + 0.125 >= a && px + 0.875 < b.
    let f0 = clamp_px((a - 0.125).ceil()).max(p_lo);
    let f1 = clamp_px((b - 0.875).ceil()).min(p_hi);
    let bits = |px: usize| -> i32 {
        let base = (x0 + px) as f32;
        let mut n = 0i32;
        for s in 0..4u8 {
            let pos = base + (f32::from(s) + 0.5) * 0.25;
            if pos >= a && pos < b {
                n += 1;
            }
        }
        n
    };
    if f0 < f1 {
        if let Some(d) = diff.get_mut(f0) {
            *d += 4;
        }
        if let Some(d) = diff.get_mut(f1) {
            *d -= 4;
        }
        for px in p_lo..f0 {
            if let Some(e) = extra.get_mut(px) {
                *e += bits(px);
            }
        }
        for px in f1..p_hi {
            if let Some(e) = extra.get_mut(px) {
                *e += bits(px);
            }
        }
    } else {
        // Span too narrow for any fully-covered pixel: everything is boundary.
        for px in p_lo..p_hi {
            if let Some(e) = extra.get_mut(px) {
                *e += bits(px);
            }
        }
    }
}

/// Union bounding box of device-space geometry.
struct BBox {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl BBox {
    fn empty() -> Self {
        Self {
            min_x: f32::INFINITY,
            min_y: f32::INFINITY,
            max_x: f32::NEG_INFINITY,
            max_y: f32::NEG_INFINITY,
        }
    }

    fn add(&mut self, x: f32, y: f32) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
    }

    /// Clamp to the tile and widen by one pixel of slack for the AA lattice; `None`
    /// when nothing overlaps the tile.
    fn pixel_range(&self, w: u32, h: u32) -> Option<(usize, usize, usize, usize)> {
        if self.min_x > self.max_x || self.min_y > self.max_y {
            return None;
        }
        let x0 = clampf(self.min_x.floor(), 0.0, w as f32) as usize;
        let x1 = clampf(self.max_x.ceil() + 1.0, 0.0, w as f32) as usize;
        let y0 = clampf(self.min_y.floor(), 0.0, h as f32) as usize;
        let y1 = clampf(self.max_y.ceil() + 1.0, 0.0, h as f32) as usize;
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        Some((x0, x1, y0, y1))
    }
}

/// Non-finite → 0, magnitude clamped to [`COORD_LIMIT`] (the "out-of-range coords
/// clamp" contract).
fn sanitize(v: f32) -> f32 {
    if v.is_finite() {
        clampf(v, -COORD_LIMIT, COORD_LIMIT)
    } else {
        0.0
    }
}

/// Flatten one path (cubics → line segments at [`FLATTEN_TOL`]) into canonical edges
/// tagged `path`, implicitly closing every subpath, growing `bbox` as it goes.
fn flatten_path(
    cmds: &[PathCmd],
    path: u32,
    t: PathTransform,
    bbox: &mut BBox,
    edges: &mut Vec<Edge>,
) {
    let map = |x: f32, y: f32| -> (f32, f32) {
        (
            sanitize(x * t.scale_x + t.dx),
            sanitize(y * t.scale_y + t.dy),
        )
    };
    let mut cur: Option<(f32, f32)> = None;
    let mut start: Option<(f32, f32)> = None;
    let mut push = |edges: &mut Vec<Edge>, p0: (f32, f32), p1: (f32, f32)| {
        bbox.add(p0.0, p0.1);
        bbox.add(p1.0, p1.1);
        if p0.1 == p1.1 {
            return; // horizontal edges never cross a sub-scanline
        }
        let (top, bot) = if p0.1 < p1.1 { (p0, p1) } else { (p1, p0) };
        edges.push(Edge {
            path,
            ytop: top.1,
            ybot: bot.1,
            xtop: top.0,
            dxdy: (bot.0 - top.0) / (bot.1 - top.1),
        });
    };
    for cmd in cmds {
        match *cmd {
            PathCmd::Move(x, y) => {
                // Implicitly close the open subpath.
                if let (Some(c), Some(s)) = (cur, start)
                    && c != s
                {
                    push(edges, c, s);
                }
                let p = map(x, y);
                cur = Some(p);
                start = Some(p);
            }
            PathCmd::Line(x, y) => {
                let p = map(x, y);
                if let Some(c) = cur {
                    push(edges, c, p);
                } else {
                    start = Some(p); // tolerate a missing Move: begin here
                }
                cur = Some(p);
            }
            PathCmd::Cubic(x1, y1, x2, y2, x, y) => {
                let p3 = map(x, y);
                if let Some(p0) = cur {
                    let p1 = map(x1, y1);
                    let p2 = map(x2, y2);
                    flatten_cubic(p0, p1, p2, p3, &mut |a, b| push(edges, a, b));
                } else {
                    start = Some(p3);
                }
                cur = Some(p3);
            }
            PathCmd::Close => {
                if let (Some(c), Some(s)) = (cur, start)
                    && c != s
                {
                    push(edges, c, s);
                }
                cur = start;
            }
        }
    }
    if let (Some(c), Some(s)) = (cur, start)
        && c != s
    {
        push(edges, c, s);
    }
}

/// Uniform-parameter cubic flattening. The segment count comes from the classic
/// control-polygon deviation bound: the curve is within `¾·max(|P1−L(⅓)|, |P2−L(⅔)|)`
/// of its chord (L = chord lerp), and uniform n-splitting shrinks the residual as
/// `1/n²`, so `n = ceil(sqrt(err/tol))` meets [`FLATTEN_TOL`]. Bounded n keeps hostile
/// curves cheap.
fn flatten_cubic(
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
    emit: &mut impl FnMut((f32, f32), (f32, f32)),
) {
    let lerp3 = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let e1x = p1.0 - lerp3(p0.0, p3.0, 1.0 / 3.0);
    let e1y = p1.1 - lerp3(p0.1, p3.1, 1.0 / 3.0);
    let e2x = p2.0 - lerp3(p0.0, p3.0, 2.0 / 3.0);
    let e2y = p2.1 - lerp3(p0.1, p3.1, 2.0 / 3.0);
    let err = 0.75 * e1x.abs().max(e1y.abs()).max(e2x.abs()).max(e2y.abs());
    let n = if err <= FLATTEN_TOL {
        1
    } else {
        // sqrt of a clamped positive — bounded 1..=64.
        clampf((err / FLATTEN_TOL).sqrt().ceil(), 1.0, 64.0) as u32
    };
    let mut prev = p0;
    for k in 1..=n {
        let t = k as f32 / n as f32;
        let mt = 1.0 - t;
        let a = mt * mt * mt;
        let b = 3.0 * mt * mt * t;
        let c = 3.0 * mt * t * t;
        let d = t * t * t;
        let q = (
            a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
            a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
        );
        emit(prev, q);
        prev = q;
    }
}

/// Parse one glyph-asset path string — absolute `M`/`L`/`C`/`Z` with implicit command
/// repetition (SVG semantics: numbers after a completed `M` continue as `L`) — into
/// [`PathCmd`]s. `None` on any other command letter, malformed number, or a dangling
/// argument count. Separators are whitespace and commas.
#[must_use]
pub fn parse_path(d: &str) -> Option<Vec<PathCmd>> {
    let mut out = Vec::new();
    let mut nums: Vec<f32> = Vec::new();
    let mut cmd: Option<char> = None;
    let bytes = d.as_bytes();
    let mut i = 0usize;
    let flush = |cmd: char, nums: &mut Vec<f32>, out: &mut Vec<PathCmd>| -> bool {
        let need = match cmd {
            'M' | 'L' => 2,
            'C' => 6,
            'Z' => return nums.is_empty(),
            _ => return false,
        };
        if !nums.len().is_multiple_of(need) {
            return false;
        }
        let mut first = true;
        for chunk in nums.chunks_exact(need) {
            match (cmd, chunk) {
                ('M', [x, y]) => {
                    // Only the first pair is a Move; the rest are implicit Lines.
                    if first {
                        out.push(PathCmd::Move(*x, *y));
                    } else {
                        out.push(PathCmd::Line(*x, *y));
                    }
                }
                ('L', [x, y]) => out.push(PathCmd::Line(*x, *y)),
                ('C', [x1, y1, x2, y2, x, y]) => {
                    out.push(PathCmd::Cubic(*x1, *y1, *x2, *y2, *x, *y));
                }
                _ => return false,
            }
            first = false;
        }
        let had = !nums.is_empty();
        nums.clear();
        had || cmd == 'Z'
    };
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_whitespace() || c == ',' {
            i += 1;
        } else if c.is_ascii_alphabetic() {
            if let Some(prev) = cmd
                && !flush(prev, &mut nums, &mut out)
            {
                return None;
            }
            match c {
                'M' | 'L' | 'C' => cmd = Some(c),
                'Z' => {
                    out.push(PathCmd::Close);
                    cmd = Some('Z');
                }
                _ => return None,
            }
            i += 1;
        } else {
            // A number: [-]digits[.digits][e[-]digits]
            let s = i;
            if c == '-' || c == '+' {
                i += 1;
            }
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || bytes[i] == b'.'
                    || bytes[i] == b'e'
                    || bytes[i] == b'E'
                    || ((bytes[i] == b'-' || bytes[i] == b'+')
                        && matches!(bytes.get(i.wrapping_sub(1)), Some(b'e' | b'E'))))
            {
                i += 1;
            }
            let tok = d.get(s..i)?;
            let v: f32 = tok.parse().ok()?;
            if cmd.is_none() || cmd == Some('Z') {
                return None; // numbers before any command / after Z with no command
            }
            nums.push(v);
        }
    }
    if let Some(prev) = cmd
        && !flush(prev, &mut nums, &mut out)
    {
        return None;
    }
    if !nums.is_empty() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit circle at (0.5, 0.5), radius r, as 4 cubics (kappa arcs).
    fn circle(r: f32) -> Vec<PathCmd> {
        let k = 0.552_284_8 * r;
        let (cx, cy) = (0.5f32, 0.5f32);
        vec![
            PathCmd::Move(cx + r, cy),
            PathCmd::Cubic(cx + r, cy + k, cx + k, cy + r, cx, cy + r),
            PathCmd::Cubic(cx - k, cy + r, cx - r, cy + k, cx - r, cy),
            PathCmd::Cubic(cx - r, cy - k, cx - k, cy - r, cx, cy - r),
            PathCmd::Cubic(cx + k, cy - r, cx + r, cy - k, cx + r, cy),
            PathCmd::Close,
        ]
    }

    fn alpha_at(t: &Tile, x: u32, y: u32) -> u8 {
        t.pixels()[((y * t.width() + x) * 4 + 3) as usize]
    }

    #[test]
    fn two_layer_asset_is_deterministic() {
        let tri = [
            PathCmd::Move(0.1, 0.9),
            PathCmd::Line(0.9, 0.9),
            PathCmd::Line(0.5, 0.2),
            PathCmd::Close,
        ];
        let render = || {
            let mut t = Tile::new(64, 64);
            fill_path(
                &mut t,
                &[&circle(0.4)],
                (1.0, 0.5, 0.2),
                1.0,
                PathTransform::fit(64, 64),
            );
            fill_path(
                &mut t,
                &[&tri],
                (0.1, 0.2, 0.9),
                0.8,
                PathTransform::fit(64, 64),
            );
            t.pixels().to_vec()
        };
        let a = render();
        let b = render();
        assert_eq!(a, b, "same asset ⇒ byte-identical raster");
        // And it actually painted something opaque in the middle.
        assert!(a[(32 * 64 + 32) * 4 + 3] > 200, "center covered");
    }

    #[test]
    fn even_odd_holes_work() {
        // Outer square with an inner square subpath in the SAME path: even-odd must
        // leave the inner region transparent regardless of winding.
        let donut = [
            PathCmd::Move(0.1, 0.1),
            PathCmd::Line(0.9, 0.1),
            PathCmd::Line(0.9, 0.9),
            PathCmd::Line(0.1, 0.9),
            PathCmd::Close,
            PathCmd::Move(0.35, 0.35),
            PathCmd::Line(0.65, 0.35),
            PathCmd::Line(0.65, 0.65),
            PathCmd::Line(0.35, 0.65),
            PathCmd::Close,
        ];
        let mut t = Tile::new(80, 80);
        fill_path(
            &mut t,
            &[&donut],
            (0.0, 0.0, 0.0),
            1.0,
            PathTransform::fit(80, 80),
        );
        assert_eq!(alpha_at(&t, 40, 40), 0, "hole is transparent");
        assert!(alpha_at(&t, 40, 16) > 200, "ring is painted");
        // Union across separate paths must NOT cancel: two overlapping squares as two
        // paths of one layer cover their intersection.
        let sq1 = [
            PathCmd::Move(0.1, 0.1),
            PathCmd::Line(0.6, 0.1),
            PathCmd::Line(0.6, 0.6),
            PathCmd::Line(0.1, 0.6),
            PathCmd::Close,
        ];
        let sq2 = [
            PathCmd::Move(0.4, 0.4),
            PathCmd::Line(0.9, 0.4),
            PathCmd::Line(0.9, 0.9),
            PathCmd::Line(0.4, 0.9),
            PathCmd::Close,
        ];
        let mut t2 = Tile::new(80, 80);
        fill_path(
            &mut t2,
            &[&sq1, &sq2],
            (0.0, 0.0, 0.0),
            1.0,
            PathTransform::fit(80, 80),
        );
        assert!(
            alpha_at(&t2, 40, 40) > 200,
            "sibling overlap unions, never cancels"
        );
    }

    #[test]
    fn fill_path_fixed_matches_float_frame() {
        // A fixed-point drawlist rasterizes byte-identically to the equivalent 0..1
        // `PathCmd` paths: the u16 endpoints are exact multiples of FIXED_ONE/8, so
        // `v / FIXED_ONE` reproduces the float coordinates with no rounding drift.
        let q = |f: f32| (f * FIXED_ONE as f32) as u16;
        let seg = [
            PathSeg::Move(q(0.125), q(0.75)),
            PathSeg::Line(q(0.875), q(0.75)),
            PathSeg::Cubic(q(0.875), q(0.25), q(0.125), q(0.25), q(0.5), q(0.5)),
            PathSeg::Close,
        ];
        let cmd = [
            PathCmd::Move(0.125, 0.75),
            PathCmd::Line(0.875, 0.75),
            PathCmd::Cubic(0.875, 0.25, 0.125, 0.25, 0.5, 0.5),
            PathCmd::Close,
        ];
        let mut a = Tile::new(48, 48);
        let mut b = Tile::new(48, 48);
        fill_path_fixed(
            &mut a,
            &[&seg],
            (0.2, 0.7, 0.9),
            0.9,
            PathTransform::fit(48, 48),
        );
        fill_path(
            &mut b,
            &[&cmd],
            (0.2, 0.7, 0.9),
            0.9,
            PathTransform::fit(48, 48),
        );
        assert_eq!(
            a.pixels(),
            b.pixels(),
            "fixed-point frame == float frame raster"
        );
        assert!(a.pixels().iter().any(|&x| x != 0), "actually painted");
        // Degenerate fixed input is a no-op (mirrors the float path's safety).
        let mut t = Tile::new(16, 16);
        let before = t.pixels().to_vec();
        fill_path_fixed(
            &mut t,
            &[],
            (1.0, 1.0, 1.0),
            1.0,
            PathTransform::fit(16, 16),
        );
        fill_path_fixed(
            &mut t,
            &[&[PathSeg::Move(0, 0)]],
            (1.0, 1.0, 1.0),
            1.0,
            PathTransform::fit(16, 16),
        );
        assert_eq!(
            t.pixels(),
            &before[..],
            "degenerate fixed input paints nothing"
        );
    }

    #[test]
    fn degenerate_paths_are_safe() {
        let mut t = Tile::new(16, 16);
        let before = t.pixels().to_vec();
        // Empty layer, Move-only path, zero-area line, Close with no Move.
        fill_path(
            &mut t,
            &[],
            (1.0, 1.0, 1.0),
            1.0,
            PathTransform::fit(16, 16),
        );
        let move_only = [PathCmd::Move(0.5, 0.5)];
        let zero_area = [
            PathCmd::Move(0.2, 0.2),
            PathCmd::Line(0.8, 0.2),
            PathCmd::Close,
        ];
        let lone_close = [PathCmd::Close];
        fill_path(
            &mut t,
            &[&move_only, &zero_area, &lone_close, &[]],
            (1.0, 1.0, 1.0),
            1.0,
            PathTransform::fit(16, 16),
        );
        assert_eq!(t.pixels(), &before[..], "degenerate input paints nothing");
    }

    #[test]
    fn out_of_range_coords_clamp() {
        let mut t = Tile::new(32, 32);
        // A triangle with far-out-of-range and non-finite coordinates must neither
        // panic nor hang; painting stays inside the tile by construction.
        let wild = [
            PathCmd::Move(-1.0e9, -1.0e9),
            PathCmd::Line(1.0e9, 0.5),
            PathCmd::Line(f32::NAN, f32::INFINITY),
            PathCmd::Close,
        ];
        fill_path(
            &mut t,
            &[&wild],
            (1.0, 0.0, 0.0),
            1.0,
            PathTransform::fit(32, 32),
        );
        // A sane path partially off-tile clips to the tile edge.
        let half_off = [
            PathCmd::Move(-0.5, 0.25),
            PathCmd::Line(0.5, 0.25),
            PathCmd::Line(0.5, 0.75),
            PathCmd::Line(-0.5, 0.75),
            PathCmd::Close,
        ];
        let mut t2 = Tile::new(32, 32);
        fill_path(
            &mut t2,
            &[&half_off],
            (0.0, 1.0, 0.0),
            1.0,
            PathTransform::fit(32, 32),
        );
        assert!(alpha_at(&t2, 0, 16) > 200, "clipped fill reaches the edge");
        assert_eq!(alpha_at(&t2, 31, 16), 0, "right half stays empty");
    }

    #[test]
    fn parse_path_roundtrips_the_asset_grammar() {
        let cmds = parse_path("M 0.1 0.2 C 0.3 0.4 0.5 0.6 0.7 0.8 L 0.9 1.0 Z").unwrap();
        assert_eq!(
            cmds,
            vec![
                PathCmd::Move(0.1, 0.2),
                PathCmd::Cubic(0.3, 0.4, 0.5, 0.6, 0.7, 0.8),
                PathCmd::Line(0.9, 1.0),
                PathCmd::Close,
            ]
        );
        // Implicit repetition: M pair, then implicit L; C repeats as C.
        let imp = parse_path("M0 0 1 0 C1 0 1 1 0 1 0 1 0 0 0 0Z").unwrap();
        assert_eq!(
            imp,
            vec![
                PathCmd::Move(0.0, 0.0),
                PathCmd::Line(1.0, 0.0),
                PathCmd::Cubic(1.0, 0.0, 1.0, 1.0, 0.0, 1.0),
                PathCmd::Cubic(0.0, 1.0, 0.0, 0.0, 0.0, 0.0),
                PathCmd::Close,
            ]
        );
        // Malformed input is refused, never panics.
        assert!(parse_path("M 0.1").is_none(), "dangling args");
        assert!(parse_path("Q 1 2 3 4").is_none(), "unsupported command");
        assert!(parse_path("M x y").is_none(), "non-numeric");
        assert!(parse_path("1 2 3").is_none(), "numbers before a command");
        assert_eq!(parse_path("").unwrap(), vec![]);
    }

    /// Perf gate for the v4 glyph pipeline: a 30-layer glyph (each layer a ringed blob
    /// — 8 outer cubics + a 4-cubic hole) rasterized at 160×100 must land well under
    /// 1 ms. Timing-sensitive, so it follows the repo's manual-timing idiom:
    ///
    /// ```sh
    /// cargo test -p aterm-scene --release bench_fill_path -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
    fn bench_fill_path_30_layer_glyph() {
        use std::time::Instant;
        // 30 deterministic pseudo-random layers.
        let mut rng = crate::Rng::new(0xCA7A_57E1);
        let layers: Vec<Vec<PathCmd>> = (0..30)
            .map(|_| {
                let (cx, cy) = (rng.range(0.3, 0.7), rng.range(0.3, 0.7));
                let r = rng.range(0.15, 0.45);
                let mut p = Vec::new();
                for ring in 0..2 {
                    let rr = if ring == 0 { r } else { r * 0.45 };
                    let segs = if ring == 0 { 8 } else { 4 };
                    for s in 0..segs {
                        let a0 = s as f32 / segs as f32 * std::f32::consts::TAU;
                        let a1 = (s + 1) as f32 / segs as f32 * std::f32::consts::TAU;
                        let wob = rng.range(0.9, 1.1);
                        let (x0, y0) = (cx + rr * a0.cos(), cy + rr * a0.sin());
                        let (x1, y1) = (cx + rr * a1.cos(), cy + rr * a1.sin());
                        if s == 0 {
                            p.push(PathCmd::Move(x0, y0));
                        }
                        let am = (a0 + a1) * 0.5;
                        let (mx, my) = (cx + rr * wob * am.cos(), cy + rr * wob * am.sin());
                        p.push(PathCmd::Cubic(mx, my, mx, my, x1, y1));
                    }
                    p.push(PathCmd::Close);
                }
                p
            })
            .collect();
        // Warm up, then time.
        let raster = |layers: &[Vec<PathCmd>]| {
            let mut t = Tile::new(160, 100);
            for (i, l) in layers.iter().enumerate() {
                let g = i as f32 / 30.0;
                fill_path(
                    &mut t,
                    &[l.as_slice()],
                    (0.8, g, 0.3),
                    1.0,
                    PathTransform::fit(160, 100),
                );
            }
            t
        };
        for _ in 0..8 {
            assert!(raster(&layers).pixels().iter().any(|&b| b != 0));
        }
        let iters = 96usize;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let start = Instant::now();
            let t = raster(&layers);
            samples.push(start.elapsed());
            assert!(t.pixels().iter().any(|&b| b != 0));
        }
        samples.sort();
        let median = samples[iters / 2];
        println!(
            "bench_fill_path: median {median:?} over {iters} 30-layer 160x100 glyph rasters \
             (min {:?}, max {:?})",
            samples[0],
            samples[iters - 1]
        );
        assert!(
            median < std::time::Duration::from_millis(1),
            "30-layer glyph raster must stay well under 1ms, got {median:?}"
        );
    }
}

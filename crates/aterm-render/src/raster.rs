// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The scanline signed-area coverage rasterizer every outline path in this crate fills
//! through.
//!
//! # What it computes
//!
//! One `f32` accumulator per grid cell holds the derivative of coverage along x: an edge
//! crossing a scanline deposits the signed area it adds into the cells it touches (`+`
//! for a downward edge, `−` for an upward one, so a closed contour's contributions
//! across a row sum to zero). Coverage is the running prefix sum, and
//! [`Rasterizer::for_each_pixel`] carries one accumulator across the whole flat grid
//! rather than resetting per row: each row sums to zero, so the next starts at zero.
//! Nonzero winding is approximated as `|acc|`, unclamped: overlapping same-winding
//! contours can exceed 1, and every caller clamps on its way to 8 bits.
//!
//! That design is also why [`crate::variation::RASTER_PAD`] exists: an edge that lands
//! outside the grid loses its area for good, every later texel is offset by a constant,
//! and the glyph paints as a filled block. The pad keeps outlines strictly inside.
//!
//! # The fill and the flattening
//!
//! The fill ([`Rasterizer::draw_line`] and everything downstream) computes exact
//! analytic per-cell area. It is the crate's cross-machine determinism, which the
//! GPU/CPU parity suites lean on, so reordering one of its expressions can change
//! output; the golden grids in `raster_tests.rs` pin it. The flattening
//! ([`Rasterizer::draw_quad`], [`Rasterizer::draw_cubic`]) is held instead to measured
//! accuracy against an analytic reference (`tests/raster_accuracy_survey.rs`) under the
//! [`FLATTEN_SAGITTA_PX`] budget (`docs/measured/fontdue-oracle-decision-2026-08-29.md`
//! has the argument).
//!
//! # Hardening
//!
//! The fill also runs over fonts aterm did not author, into a grid sized from the
//! font's declared `glyph_bounding_box`, which its outline can exceed. So the escape
//! paths (a cell index left or right of the buffer, an interior span of billions of
//! cells, a curve asking for millions of segments) are clamped or capped: a bad font
//! costs a wrong-looking glyph, never a panic or a hung frame. For in-grid outlines,
//! which `RASTER_PAD` guarantees for this crate's feeders, the clamps are unreachable.
//! An escaping edge costs exactly the area that left the grid: [`Rasterizer::add`]
//! drops the one out-of-range write and the x march continues. The far-left
//! `linestart + x0i < 0` skip is the exception, kept because `RASTER_PAD` was written
//! around it.

/// A point in the rasterizer's grid space: x right, y DOWN, in cells.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Point {
    /// Horizontal position, in cells.
    pub x: f32,
    /// Vertical position, in cells, increasing DOWNWARD.
    pub y: f32,
}

/// Construct a [`Point`] — the terse form the outline feeders use per vertex.
#[inline]
pub const fn point(x: f32, y: f32) -> Point {
    Point { x, y }
}

/// Linear interpolation between two points at parameter `t`.
#[inline]
fn lerp(t: f32, p0: Point, p1: Point) -> Point {
    point(p0.x + t * (p1.x - p0.x), p0.y + t * (p1.y - p0.y))
}

/// The greatest distance, in grid cells, a flattened curve's polyline may depart from
/// the true curve (its sagitta).
///
/// Derived from the output format, not tuned: the mask is 8 bits, so one level is
/// `1/255` of full coverage. A polyline with sagitta `t` px displaces the edge by at most
/// `t`, and an edge spans at most 1 px of a cell, so flattening adds at most `t` of
/// coverage error to any cell. Holding that under one output level gives
///
/// ```text
/// t ≤ 1/255 px = 0.0039 px
/// ```
///
/// It moves only if the mask's depth moves. Against an analytic reference over the
/// embedded faces at 8..32 px the corpus mean error is 0.075/255 (a 0.144 px budget
/// reads 0.862); `tests/raster_accuracy_survey.rs` guards it. The cost, about 1.5× fill
/// time per glyph, is cold-path only: every raster is memoized by `GlyphKey` in the
/// renderer's glyph cache and the GPU atlas.
pub const FLATTEN_SAGITTA_PX: f32 = 1.0 / 255.0;

/// `(4·t)²` — the squared second difference at which a quadratic's own chord
/// already meets [`FLATTEN_SAGITTA_PX`], so it flattens to a single line.
const ONE_LINE_DEVSQ: f32 = (4.0 * FLATTEN_SAGITTA_PX) * (4.0 * FLATTEN_SAGITTA_PX);

/// `1/(4·t)` — the factor that turns a second difference into the square of the
/// segment count that meets [`FLATTEN_SAGITTA_PX`]. Precomputed so the hot
/// flattening loop pays no division.
const INV_FOUR_SAGITTA: f32 = 1.0 / (4.0 * FLATTEN_SAGITTA_PX);

/// Hard ceiling on the segments one curve may flatten to, so a hostile outline costs a
/// wrong shape rather than a hung frame. No in-grid curve reaches it
/// ([`Rasterizer::draw_quad`]).
const MAX_FLATTEN_SEGMENTS: usize = 4096;

/// A `width` × `height` signed-area accumulation grid.
///
/// Feed it edges with [`draw_line`](Self::draw_line),
/// [`draw_quad`](Self::draw_quad) and [`draw_cubic`](Self::draw_cubic) — in any
/// order, contours implicitly closed by the caller — then read coverage out
/// with [`for_each_pixel`](Self::for_each_pixel).
pub struct Rasterizer {
    width: usize,
    height: usize,
    /// `width * height` accumulators plus a short tail of slack, so a span that
    /// ends exactly at the right edge has somewhere to deposit its final cell
    /// without a bounds check on the hot path.
    a: Vec<f32>,
}

impl Rasterizer {
    /// A cleared `width` × `height` grid.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            a: vec![0.0; width * height + 4],
        }
    }

    /// Deposit `v` at flat index `i`, dropping writes that escape the buffer.
    ///
    /// Escapes are only reachable from an outline that leaves the grid; the
    /// padded callers in this crate never produce one, and a write past the
    /// readable `width * height` prefix could not be observed by
    /// [`for_each_pixel`] even if it landed.
    #[inline]
    fn add(&mut self, i: isize, v: f32) {
        if i >= 0
            && let Some(slot) = self.a.get_mut(i as usize)
        {
            *slot += v;
        }
    }

    /// Accumulate one straight edge from `p0` to `p1`.
    ///
    /// Direction carries the winding sign, so `draw_line(a, b)` and
    /// `draw_line(b, a)` cancel exactly. A horizontal edge contributes nothing
    /// and is skipped.
    pub fn draw_line(&mut self, p0: Point, p1: Point) {
        if (p0.y - p1.y).abs() <= f32::EPSILON {
            return;
        }
        // Walk the edge downward whatever way it was given, remembering which
        // way it really pointed: that sign IS the winding contribution.
        let (dir, p0, p1) = if p0.y < p1.y {
            (1.0f32, p0, p1)
        } else {
            (-1.0f32, p1, p0)
        };
        let dxdy = (p1.x - p0.x) / (p1.y - p0.y);
        let mut x = p0.x;
        if p0.y < 0.0 {
            // Entered the grid partway down: advance x to the y = 0 crossing.
            x -= p0.y * dxdy;
        }
        // Saturating float→int casts clamp a negative or absurd endpoint into
        // range on their own, which is exactly the wanted behaviour here.
        let y_first = p0.y as usize;
        let y_last = self.height.min(p1.y.ceil() as usize);
        for y in y_first..y_last {
            let linestart = y * self.width;
            // The part of this scanline the edge actually spans, in y.
            let dy = ((y + 1) as f32).min(p1.y) - (y as f32).max(p0.y);
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            let (x0, x1) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let x0floor = x0.floor();
            let x0i = x0floor as i32;
            let x1ceil = x1.ceil();
            let x1i = x1ceil as i32;
            let linestart_x0i = linestart as isize + x0i as isize;
            if linestart_x0i < 0 {
                // Off the left of the whole buffer: nothing can be credited.
                // Deliberately without advancing `x`, so the march does not
                // reinterpret the segment.
                continue;
            }
            if x1i <= x0i + 1 {
                // The whole span sits in one column pair: split the area by
                // where its midpoint falls inside the column.
                let xmf = 0.5 * (x + xnext) - x0floor;
                self.add(linestart_x0i, d - d * xmf);
                self.add(linestart_x0i + 1, d * xmf);
            } else {
                // A span crossing columns: `s` is the per-cell share of x, and
                // the two end columns get triangular corners (`a0`, `am`) while
                // the interior gets full slabs.
                let s = (x1 - x0).recip();
                let x0f = x0 - x0floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                self.add(linestart_x0i, d * a0);
                if x1i == x0i + 2 {
                    // Exactly two interior-free columns: the middle takes
                    // whatever the two corners did not.
                    self.add(linestart_x0i + 1, d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - x0f);
                    self.add(linestart_x0i + 1, d * (a1 - a0));
                    // Full slabs over the interior columns `x0i+2 .. x1i-1`, both bounds
                    // clamped to columns whose flat index lands in the buffer (`xi ∈
                    // [-linestart, a.len() - linestart)`). The far-left guard fires only
                    // when `linestart + x0i < 0`, so on a later row a negative `x0i` gets
                    // here, and an unclamped loop could spin through tens of millions of
                    // dropped writes. For an in-grid span both clamps are no-ops.
                    let base = linestart as isize;
                    let lo = (x0i as isize + 2).max(-base);
                    let hi = (x1i as isize - 1).min(self.a.len() as isize - base);
                    if lo < hi {
                        let delta = d * s;
                        for slot in &mut self.a[(base + lo) as usize..(base + hi) as usize] {
                            *slot += delta;
                        }
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.add(linestart as isize + (x1i - 1) as isize, d * (1.0 - a2 - am));
                }
                self.add(linestart as isize + x1i as isize, d * am);
            }
            x = xnext;
        }
    }

    /// Accumulate a quadratic Bézier (`p1` is the control point), flattened to a
    /// polyline whose sagitta (greatest perpendicular departure from the curve) is at
    /// most [`FLATTEN_SAGITTA_PX`]. A nearly straight curve becomes one line; otherwise
    /// the segment count grows with the square root of the second difference.
    ///
    /// # The derivation
    ///
    /// `B(t) = (1-t)²p0 + 2t(1-t)p1 + t²p2`, so `B''(t) = 2·dev` with
    /// `dev = p0 - 2·p1 + p2` — constant, which is what makes a quadratic's
    /// error exactly computable rather than bounded. The curve's departure
    /// from its own chord peaks at the midpoint:
    ///
    /// ```text
    /// B(½) - ½(p0 + p2) = ¼(p0 + 2p1 + p2) - ½(p0 + p2) = -dev/4
    /// ```
    ///
    /// so one line has sagitta `|dev|/4`, admissible when `|dev| ≤ 4t`, i.e.
    /// `devsq ≤ (4t)²`. Splitting the parameter into `n` equal pieces gives
    /// each sub-quad the second difference `dev/n²`, so the `n`-segment
    /// sagitta is `|dev|/(4n²)` and `n ≥ √(|dev|/(4t))` meets the budget.
    pub fn draw_quad(&mut self, p0: Point, p1: Point, p2: Point) {
        let devx = p0.x - 2.0 * p1.x + p2.x;
        let devy = p0.y - 2.0 * p1.y + p2.y;
        let devsq = devx * devx + devy * devy;
        // One line, when its own sagitta `|dev|/4` already fits the budget.
        if devsq < ONE_LINE_DEVSQ {
            self.draw_line(p0, p2);
            return;
        }
        // Segment count, capped. The grid is at most 4096 cells a side, so an in-grid
        // `|dev|` is at most `8192·√2 ≈ 1.16e4` px and `n` at most 860 (the cubic's
        // ~1489), well under the cap: only a hostile curve reaches it. Rooting `devsq`
        // first keeps the arithmetic finite near f32's ceiling. `saturating_add`, not
        // `1 +`: a coordinate that scales to infinity saturates the cast at `usize::MAX`.
        let n = ((devsq.sqrt() * INV_FOUR_SAGITTA).sqrt().floor() as usize)
            .saturating_add(1)
            .min(MAX_FLATTEN_SEGMENTS);
        let nrecip = (n as f32).recip();
        let mut p = p0;
        let mut t = 0.0;
        for _ in 0..n - 1 {
            t += nrecip;
            let pn = lerp(t, lerp(t, p0, p1), lerp(t, p1, p2));
            self.draw_line(p, pn);
            p = pn;
        }
        self.draw_line(p, p2);
    }

    /// Accumulate a cubic Bézier (`p1`, `p2` are the control points), the CFF outline
    /// form, flattened to the same [`FLATTEN_SAGITTA_PX`] budget as
    /// [`draw_quad`](Self::draw_quad) by the same uniform split.
    ///
    /// # The derivation
    ///
    /// A cubic's second derivative is not constant, so the budget is met
    /// through a bound rather than an equality:
    ///
    /// ```text
    /// B''(t) = 6[(1-t)·d1 + t·d2]    d1 = p0 - 2p1 + p2,  d2 = p1 - 2p2 + p3
    /// ```
    ///
    /// which is a linear interpolation, so `|B''| ≤ 6·D` with
    /// `D = max(|d1|, |d2|)`. A curve departs from its chord over a parameter
    /// span `Δt` by at most `⅛·max|B''|·Δt²`, so `n` equal pieces
    /// (`Δt = 1/n`) have sagitta at most `3D/(4n²)`, met by
    /// `n ≥ √(3D/(4t))`. One line is admissible when `3D/4 ≤ t`.
    pub fn draw_cubic(&mut self, p0: Point, p1: Point, p2: Point, p3: Point) {
        let d1x = p0.x - 2.0 * p1.x + p2.x;
        let d1y = p0.y - 2.0 * p1.y + p2.y;
        let d2x = p1.x - 2.0 * p2.x + p3.x;
        let d2y = p1.y - 2.0 * p2.y + p3.y;
        // `D²`, so the single root below is the only one paid.
        let dsq = (d1x * d1x + d1y * d1y).max(d2x * d2x + d2y * d2y);
        // One line, when `3D/4` already fits: `D ≤ 4t/3`, i.e.
        // `dsq ≤ (4t/3)² = (4t)²/9`.
        if dsq < ONE_LINE_DEVSQ * (1.0 / 9.0) {
            self.draw_line(p0, p3);
            return;
        }
        // `saturating_add` for the same infinite-coordinate reason as the quad.
        let n = ((3.0 * dsq.sqrt() * INV_FOUR_SAGITTA).sqrt().floor() as usize)
            .saturating_add(1)
            .min(MAX_FLATTEN_SEGMENTS);
        let nrecip = (n as f32).recip();
        let mut p = p0;
        let mut t = 0.0;
        for _ in 0..n - 1 {
            t += nrecip;
            let pn = lerp(
                t,
                lerp(t, lerp(t, p0, p1), lerp(t, p1, p2)),
                lerp(t, lerp(t, p1, p2), lerp(t, p2, p3)),
            );
            self.draw_line(p, pn);
            p = pn;
        }
        self.draw_line(p, p3);
    }

    /// Visit every cell in row-major order with `(flat index, coverage 0..=1)`.
    ///
    /// Coverage is the running prefix sum of the accumulators, folded to nonzero winding
    /// as `|acc|`. It is not clamped (a cell covered twice reports 2; callers clamp on
    /// the way to 8 bits), and the accumulator is not reset between rows (module docs).
    pub fn for_each_pixel<F: FnMut(usize, f32)>(&self, mut f: F) {
        let mut acc = 0.0f32;
        for (i, c) in self.a[..self.width * self.height].iter().enumerate() {
            acc += *c;
            f(i, acc.abs());
        }
    }
}

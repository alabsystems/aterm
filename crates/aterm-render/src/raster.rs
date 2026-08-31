// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The scanline SIGNED-AREA coverage rasterizer every outline path in this
//! crate fills through (retiring `ab_glyph_rasterizer`).
//!
//! # What it computes
//!
//! One `f32` accumulator per grid cell holds the *derivative* of coverage along
//! x: an edge crossing a scanline deposits, into the cells it touches, the
//! SIGNED area it adds (`+` for a downward edge, `−` for an upward one, so a
//! closed contour's contributions across a row sum to zero). Coverage itself is
//! then the running prefix sum of that buffer — which is why
//! [`Rasterizer::for_each_pixel`] carries ONE accumulator across the whole flat
//! grid rather than resetting per row: each row is trusted to sum to zero, so
//! the accumulator arrives at the next row already back at zero. Nonzero
//! winding is approximated the way every rasterizer of this family does it:
//! the reported value is `|acc|`, UNCLAMPED — overlapping contours of the same
//! winding can push it above 1, and every caller in this crate is already
//! responsible for the `clamp` on its way to 8 bits.
//!
//! That one-accumulator design is also the failure mode
//! [`crate::variation::RASTER_PAD`] exists to prevent — an edge that lands
//! outside the grid loses its area, and the loss never comes back: every texel
//! after it is offset by a constant and the glyph paints as a filled block. The
//! pad keeps outlines strictly inside the grid, so nothing here is ever asked
//! to be clever about the boundary.
//!
//! # Two halves, guarded two different ways
//!
//! This module does two separable jobs, and they are held to two different
//! standards on purpose (`docs/measured/fontdue-oracle-decision-2026-08-29.md`
//! is the full argument).
//!
//! **The FILL — [`Rasterizer::draw_line`] and everything downstream of it — is
//! held to f32 BIT EQUALITY against `ab_glyph_rasterizer`.** The incremental
//! `x += dxdy * dy` march down a segment's scanlines, the two-cell fast path
//! when a scanline's x span lands inside one column, the `s = 1/(x1 - x0)`
//! trapezoid split when it does not, the accumulator's cross-row carry: every
//! one of those computes EXACT analytic per-cell area, where two independent
//! implementations agree to the bit because there is only one right answer.
//! `tests/rasterizer_oracle.rs` feeds both rasterizers the same flattened
//! polyline — `draw_line` calls only — and demands identical bits over tens of
//! thousands of real glyph rasters. That equality is also the only expression
//! of cross-machine determinism this crate has, and the GPU/CPU parity suites
//! lean on it. Reordering an expression in the fill can break it.
//!
//! **The FLATTENING — [`Rasterizer::draw_quad`] and
//! [`Rasterizer::draw_cubic`] — is NOT.** Its old constants (`devsq < 0.333`,
//! `tol = 3.0`, `FLATNESS = 0.35`) were the retired crate's tuning, and pinning
//! them pinned aterm to a rasterizer 3.2× less accurate than the `fontdue` it
//! also retired. They are gone, replaced by [`FLATTEN_SAGITTA_PX`] — a budget
//! derived from the 8-bit mask — and guarded by measured accuracy against an
//! independent analytic reference (`tests/raster_accuracy_survey.rs`) instead.
//!
//! # Hardening — and the ONE place it is a deliberate difference
//!
//! The grid-escape paths — a cell index left of the buffer, a cell index right
//! of it, a span whose interior loop would otherwise run for billions of
//! iterations, and a curve whose deviation asks for millions of flattening
//! segments — are CLAMPED or CAPPED rather than left to panic or hang, because
//! the same fill also runs over font files aterm did not author, into a grid
//! sized from the font's DECLARED `glyph_bounding_box`. A font whose outline
//! data exceeds its own declared box is a two-line edit away, and it must cost
//! a wrong-looking glyph, not a hung frame.
//!
//! For every outline that stays inside the grid — which is every outline this
//! crate feeds it, because [`crate::variation::RASTER_PAD`] guarantees it —
//! the clamps are unreachable and the FILL is bit-for-bit the retired crate's.
//! `tests/rasterizer_oracle.rs` proves that over tens of thousands of real
//! glyph rasters, over the shared polyline described above.
//!
//! For an outline that ESCAPES the grid, the two rasterizers deliberately
//! DIFFER, and it is worth being exact about how. The retired crate's write
//! macro `continue`s the scanline loop on an out-of-range index: it abandons
//! the rest of that scanline AND skips the `x = xnext` step, so the incremental
//! x march freezes and every LATER row of the same segment is computed from a
//! stale x — the escape corrupts rows that are still inside the grid.
//! [`Rasterizer::add`] here drops the single out-of-range write and marches on,
//! so an escape costs exactly the area that left the grid. The one escape path
//! that is reproduced verbatim is the far-left `linestart + x0i < 0` skip,
//! because [`crate::variation::RASTER_PAD`] was written around its exact
//! behaviour. The in-module tests drive all of them.

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

/// The greatest distance, in grid cells, that a flattened curve's polyline is
/// permitted to depart from the true curve (its SAGITTA).
///
/// **Derived from the output format, not tuned.** The coverage mask this
/// rasterizer feeds is 8 bits, so one output level is `1/255` of full coverage.
/// A polyline whose sagitta is `t` px displaces the true edge by at most `t`,
/// and an edge crossing a cell spans at most 1 px of it, so the coverage error
/// flattening can contribute to any one cell is at most `t`. Holding that
/// contribution under a single output level gives
///
/// ```text
/// t ≤ 1/255 px = 0.0039 px
/// ```
///
/// which is what this constant is. It moves only if the mask's depth moves —
/// not when the font set, the corpus or the px sweep changes.
///
/// The retired `ab_glyph_rasterizer` targeted `1/(4√3) = 0.144 px` here (see
/// [`Rasterizer::draw_quad`] for how its two constants both encode that one
/// number), 37× looser, and it cost real accuracy. Measured against an
/// independent analytic reference over **2,108 glyph rasters** from the two
/// embedded faces across 8..32 px — 2,320 reached, 212 dropped as
/// self-overlapping, a class every signed-area rasterizer gets wrong
/// identically (`tests/raster_accuracy_survey.rs`, which holds both the
/// measurement and the guard):
///
/// ```text
///                       corpus mean /255   worst cell /255
/// 0.144 px (retired)          0.862              42.8
/// 1/255 px (this)             0.075               4.4
/// fontdue 0.9.3               0.261              21.3
/// ```
///
/// So the tightening is 11.5× on the mean, and — the point of the exercise —
/// finally puts this module AHEAD of the `fontdue` it replaced (3.5× on the
/// mean) instead of 3.2× behind it, on every one of the 16 (face, size) rows
/// rather than only in aggregate.
///
/// **The worst-cell column is at the edge of what the instrument resolves, and
/// is quoted as a bound, not a measurement.** Displacing the reference's
/// sub-scanline phase by 1/303 px — which changes no geometry — moves a single
/// reference cell by up to 7.97/255, more than the 4.4 the shipped path scores.
/// So 4.4 is an UPPER bound on the true worst-cell error and the ~4.8× lead
/// over fontdue is a LOWER bound on the true one. The mean is the figure to
/// quote; `raster_accuracy_survey::reference_phase_sensitivity` prints the
/// spread.
///
/// # What it costs, and where
///
/// Tightening the sagitta buys segments, and segments cost fill time. Measured
/// on the REAL atlas path (`variation::varied_glyph_raster_with_face`, 94
/// printable-ASCII glyphs of the embedded face, 20 reps, opt-level 2):
///
/// ```text
///          12 px      16 px      24 px
/// 0.144    0.507 µs   0.563 µs   0.732 µs
/// 1/255    0.797 µs   0.901 µs   1.027 µs
///          1.57×      1.60×      1.40×
/// ```
///
/// That is a COLD-path cost. Every rasterized glyph is memoized by `GlyphKey`
/// (which carries the quantised px) in the renderer's `glyphs` map and in the
/// GPU atlas built from it, and that cache is cleared only at its 16,384-entry
/// cap or on a font change — so the ~0.3 µs is paid once per distinct
/// (glyph, size, style) in a session and never per frame. A pane's whole
/// working set is a few hundred rasters; even a pathological run that fills the
/// cap end to end pays ~5 ms more in total, spread across the session, for a
/// per-frame cost of exactly zero.
pub const FLATTEN_SAGITTA_PX: f32 = 1.0 / 255.0;

/// `(4·t)²` — the squared second difference at which a quadratic's own chord
/// already meets [`FLATTEN_SAGITTA_PX`], so it flattens to a single line.
const ONE_LINE_DEVSQ: f32 = (4.0 * FLATTEN_SAGITTA_PX) * (4.0 * FLATTEN_SAGITTA_PX);

/// `1/(4·t)` — the factor that turns a second difference into the square of the
/// segment count that meets [`FLATTEN_SAGITTA_PX`]. Precomputed so the hot
/// flattening loop pays no division.
const INV_FOUR_SAGITTA: f32 = 1.0 / (4.0 * FLATTEN_SAGITTA_PX);

/// Hard ceiling on the segments ONE curve may flatten to, so a hostile outline
/// costs a wrong shape rather than a hung frame. See
/// [`Rasterizer::draw_quad`] for why no in-grid curve can reach it.
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
            // The part of THIS scanline the edge actually spans, in y.
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
                // Off the left of the buffer entirely (only reachable on row
                // 0). Nothing can be credited; deliberately WITHOUT advancing
                // `x`, so the march does not silently reinterpret the segment.
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
                    // Full slabs, over the interior columns `x0i+2 .. x1i-1`.
                    //
                    // BOTH bounds are clamped, to the columns whose flat index
                    // actually lands in the buffer — `xi ∈ [-linestart,
                    // a.len() - linestart)`. Clamping only the top was not
                    // enough on either end: a large NEGATIVE `x0i` that got
                    // past the far-left guard (it only fires when
                    // `linestart + x0i < 0`, so on any row but the first a
                    // deeply negative `x0i` sails through) leaves the loop
                    // running from that huge negative start, and an
                    // `interior_end` measured against `a.len()` rather than
                    // against `a.len() - linestart` still allows a whole
                    // buffer's worth of writes that land nowhere. Simulating
                    // the unclamped arithmetic for ONE edge
                    // `(-16e6, 0) -> (16e6, 4096)` on the 4096x4096 grid this
                    // crate's own size guard permits gives 21,493,120
                    // dropped-write iterations across 2,752 scanlines.
                    //
                    // For an in-grid span the clamps are both no-ops (`lo` is
                    // `x0i + 2`, `hi` is `x1i - 1`), so the retired crate's
                    // arithmetic is reproduced exactly where it matters.
                    let base = linestart as isize;
                    let lo = (x0i as isize + 2).max(-base);
                    let hi = (x1i as isize - 1).min(self.a.len() as isize - base);
                    for i in lo..hi {
                        self.add(base + i, d * s);
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.add(linestart as isize + (x1i - 1) as isize, d * (1.0 - a2 - am));
                }
                self.add(linestart as isize + x1i as isize, d * am);
            }
            x = xnext;
        }
    }

    /// Accumulate a quadratic Bézier (`p1` is the control point).
    ///
    /// Flattened to a polyline whose SAGITTA — its greatest perpendicular
    /// departure from the true curve — is at most [`FLATTEN_SAGITTA_PX`]. A
    /// nearly straight curve becomes ONE line; otherwise the segment count
    /// grows with the square root of the second difference.
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
    /// so ONE line has sagitta `|dev|/4`, admissible when `|dev| ≤ 4t`, i.e.
    /// `devsq ≤ (4t)²`. Splitting the parameter into `n` equal pieces gives
    /// each sub-quad the second difference `dev/n²`, so the `n`-segment
    /// sagitta is `|dev|/(4n²)` and `n ≥ √(|dev|/(4t))` meets the budget.
    ///
    /// The retired `ab_glyph_rasterizer` constants this replaced —
    /// `devsq < 0.333` and `tol = 3.0` in `n = 1 + ⁴√(tol·devsq)` — are the
    /// SAME number seen twice: `√0.333/4 = 0.144 px`, and
    /// `⁴√3 = 1/(2·√0.144)`, so both encode a sagitta budget of
    /// `1/(4√3) = 0.144 px`. That was its tuning, not a correctness property,
    /// and [`FLATTEN_SAGITTA_PX`] replaces it with one derived from the output
    /// format.
    pub fn draw_quad(&mut self, p0: Point, p1: Point, p2: Point) {
        let devx = p0.x - 2.0 * p1.x + p2.x;
        let devy = p0.y - 2.0 * p1.y + p2.y;
        let devsq = devx * devx + devy * devy;
        // One line, when its own sagitta `|dev|/4` already fits the budget.
        if devsq < ONE_LINE_DEVSQ {
            self.draw_line(p0, p2);
            return;
        }
        // Segment count, CAPPED — the quad's counterpart to the cubic's cap,
        // and it was missing while TrueType (the format aterm actually
        // renders) is the quadratic path.
        //
        // `devsq` is the squared second difference of three outline points, so
        // wild control points make it enormous. The cap is chosen to be
        // unreachable by any curve that could matter: the grid is at most 4096
        // cells on a side, so an in-grid `|dev| = |p0 - 2p1 + p2|` cannot exceed
        // `8192·√2 ≈ 1.16e4` px, and `√(|dev|/(4t))` at `t = 1/255` is then at
        // most 860 — a fifth of the cap. No real glyph's flattening changes,
        // while a hostile one costs a wrong shape instead of a hung frame.
        // (The cubic's worst in-grid `n` is `√3` times that, ~1489, also under
        // the cap.) Taking the root of `devsq` FIRST also keeps the arithmetic
        // finite for a `devsq` near f32's ceiling, which multiplying by a
        // tolerance first would not.
        // `saturating_add`, not `1 +`: a control point at f32 INFINITY makes the
        // float→int cast saturate at `usize::MAX`, and `1 +` that is an
        // overflow panic in a debug build. A font can encode a coordinate that
        // scales to infinity, so this is reachable from a file, not only from a
        // test.
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

    /// Accumulate a cubic Bézier (`p1`, `p2` are the control points) — the CFF
    /// outline form.
    ///
    /// Flattened to the same [`FLATTEN_SAGITTA_PX`] budget as
    /// [`draw_quad`](Self::draw_quad), by the same uniform split.
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
    ///
    /// This replaces the retired crate's recursive half-split on
    /// `FLATNESS = 0.35` of control-polygon-vs-chord LENGTH excess — a
    /// straightness proxy with no sagitta reading, so it could not be aimed at
    /// a budget the output format derives. Uniform splitting also drops the
    /// recursion (and its `MAX_DEPTH` of 16, worth 65,536 segments) for a flat
    /// loop under the same cap the quad uses.
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
    /// Coverage is the running prefix sum of the accumulators, folded to
    /// nonzero winding as `|acc|`. It is NOT clamped — a cell covered twice
    /// over reports more than 1, and callers clamp on the way to 8 bits. The
    /// accumulator is NOT reset between rows either: see the module docs for
    /// why that is the design and not an oversight.
    pub fn for_each_pixel<F: FnMut(usize, f32)>(&self, mut f: F) {
        let mut acc = 0.0f32;
        for (i, c) in self.a[..self.width * self.height].iter().enumerate() {
            acc += *c;
            f(i, acc.abs());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Point, Rasterizer, point};

    /// Fill a `w`×`h` grid and read the coverage back as a flat row-major
    /// buffer — the shape every caller in this crate consumes.
    fn cov(w: usize, h: usize, f: impl FnOnce(&mut Rasterizer)) -> Vec<f32> {
        let mut r = Rasterizer::new(w, h);
        f(&mut r);
        let mut out = vec![0.0f32; w * h];
        r.for_each_pixel(|i, a| out[i] = a);
        out
    }

    /// Close a polygon into the rasterizer, edge by edge.
    fn polygon(r: &mut Rasterizer, pts: &[Point]) {
        for (i, p) in pts.iter().enumerate() {
            let next = pts.get(i + 1).unwrap_or(&pts[0]);
            r.draw_line(*p, *next);
        }
    }

    /// Compare against a pinned grid at f32 bit-equality — the same standard
    /// `tests/rasterizer_oracle.rs` holds the two implementations to, because
    /// antialiased coverage is where "close enough" hides a visible bug.
    #[track_caller]
    fn assert_grid(actual: &[f32], expected: &[f32], w: usize, what: &str) {
        assert_eq!(actual.len(), expected.len(), "{what}: grid size");
        for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (a - b).abs() <= 1e-6,
                "{what}: coverage differs at cell {i} (x={}, y={}): got {a}, pinned {b}",
                i % w,
                i / w
            );
        }
    }

    // -----------------------------------------------------------------------
    // GOLDEN GRIDS, held IN TREE
    //
    // `tests/rasterizer_oracle.rs` proves equality against the retired crate,
    // but only for as long as that dev-dependency is there. These four grids
    // are the evidence that outlives it, in the same shape `crates/aterm-hash`
    // pins vectors for the hash it replaced.
    //
    // The TWO LINE-ONLY grids are not invented: the oracle's
    // `in_module_golden_shapes_match_the_oracle` feeds those exact outlines
    // through `ab_glyph_rasterizer` and requires the same numbers, so the pin
    // is the retired crate's own answer, recorded. The oracle feeds it the
    // quad and the cubic too — but pre-flattened by the harness, so what it
    // pins there is the FILL, and the two curve grids below additionally
    // carry this module's own flattening. See each of them.
    // -----------------------------------------------------------------------

    /// An axis-aligned unit square on whole-pixel boundaries: the case where
    /// coverage must be exactly 1.0 inside and exactly 0.0 outside, with no
    /// antialiasing anywhere.
    #[test]
    fn golden_integer_aligned_square() {
        let got = cov(5, 5, |r| {
            polygon(
                r,
                &[
                    point(1.0, 1.0),
                    point(3.0, 1.0),
                    point(3.0, 3.0),
                    point(1.0, 3.0),
                ],
            );
        });
        #[rustfmt::skip]
        let want = [
            0.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 1.0, 0.0, 0.0,
            0.0, 1.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        assert_grid(&got, &want, 5, "integer-aligned square");
    }

    /// A triangle on fractional coordinates: every edge crosses columns at a
    /// different sub-cell offset, so this exercises the trapezoid split, the
    /// two-cell fast path and the interior slabs in one figure.
    #[test]
    fn golden_fractional_triangle() {
        let got = cov(4, 4, |r| {
            polygon(r, &[point(0.5, 0.25), point(3.25, 1.75), point(1.0, 3.5)]);
        });
        #[rustfmt::skip]
        let want = [
            0.263_549, 0.208_807, 0.0,       0.0,
            0.307_692, 0.995_739, 0.657_107, 0.041_351,
            0.153_846, 0.950_397, 0.335_317, 0.0,
            0.019_231, 0.160_714, 0.0,       0.0,
        ];
        assert_grid(&got, &want, 4, "fractional triangle");
    }

    /// A quadratic — the TrueType outline form, and the one whose flattening
    /// segment count this module caps.
    ///
    /// UNLIKE the two grids above, this one is NOT the retired crate's answer:
    /// it is the FILL's answer over the first-party flattening, so it moved
    /// when [`FLATTEN_SAGITTA_PX`] replaced `ab_glyph_rasterizer`'s 0.144 px
    /// budget. Every cell moved UP — a coarse polyline cuts the chord inside a
    /// convex curve and loses the ink between them, and this figure's curve
    /// bulges right — which is the same deficit the corpus-wide survey
    /// measures. What pins these numbers is therefore not an oracle but a
    /// bound: `tests/raster_accuracy_survey.rs` holds the shipped path to
    /// mean ≤ 0.20/255 and per-glyph max ≤ 8/255 against an independent
    /// analytic reference, and this grid is here to make a re-record LOUD.
    #[test]
    fn golden_quadratic() {
        let got = cov(6, 6, |r| {
            r.draw_quad(point(0.5, 0.5), point(5.5, 2.0), point(0.5, 5.5));
            r.draw_line(point(0.5, 5.5), point(0.5, 0.5));
        });
        #[rustfmt::skip]
        let want = [
            0.209_617, 0.138_579, 0.0,       0.0, 0.0, 0.0,
            0.5,       0.987_065, 0.469_326, 0.0, 0.0, 0.0,
            0.5,       1.0,       0.963_990, 0.0, 0.0, 0.0,
            0.5,       1.0,       0.629_342, 0.0, 0.0, 0.0,
            0.5,       0.713_891, 0.037_827, 0.0, 0.0, 0.0,
            0.159_617, 0.011_752, 0.0,       0.0, 0.0, 0.0,
        ];
        assert_grid(&got, &want, 6, "quadratic");
    }

    /// A cubic — the CFF outline form, flattened to the same sagitta budget by
    /// the same uniform split. Pinned on the same terms as
    /// [`golden_quadratic`]: first-party output, guarded by the accuracy bound
    /// rather than by the retired crate.
    #[test]
    fn golden_cubic() {
        let got = cov(6, 6, |r| {
            r.draw_cubic(
                point(0.5, 0.5),
                point(5.5, 1.0),
                point(5.5, 5.0),
                point(0.5, 5.5),
            );
            r.draw_line(point(0.5, 5.5), point(0.5, 0.5));
        });
        #[rustfmt::skip]
        let want = [
            0.234_436, 0.332_630, 0.060_644, 0.0,       0.0,       0.0,
            0.5,       1.0,       0.953_358, 0.400_362, 0.0,       0.0,
            0.5,       1.0,       1.0,       0.992_968, 0.141_929, 0.0,
            0.5,       1.0,       1.0,       0.992_968, 0.141_929, 0.0,
            0.5,       1.0,       0.953_358, 0.400_362, 0.0,       0.0,
            0.234_436, 0.332_630, 0.060_644, 0.0,       0.0,       0.0,
        ];
        assert_grid(&got, &want, 6, "cubic");
    }

    /// Winding cancels exactly: an edge and its reverse leave the grid at
    /// literal zero, which is what lets `for_each_pixel` carry one accumulator
    /// across every row of the buffer.
    #[test]
    fn reversed_edges_cancel_to_exact_zero() {
        let got = cov(8, 8, |r| {
            for (a, b) in [
                (point(1.3, 0.7), point(6.9, 5.2)),
                (point(0.1, 7.9), point(7.5, 0.2)),
                (point(3.0, 1.0), point(3.0, 7.0)),
            ] {
                r.draw_line(a, b);
                r.draw_line(b, a);
            }
        });
        for (i, v) in got.iter().enumerate() {
            assert_eq!(*v, 0.0, "cell {i} did not cancel: {v}");
        }
    }

    /// `|acc|` is reported UNCLAMPED: two overlapping same-winding contours
    /// push a cell above 1.0, and every caller clamps on its way to 8 bits. A
    /// `.min(1.0)` here would silently darken every overlapping contour.
    #[test]
    fn overlapping_contours_report_above_one_unclamped() {
        let got = cov(4, 4, |r| {
            for _ in 0..2 {
                polygon(
                    r,
                    &[
                        point(1.0, 1.0),
                        point(3.0, 1.0),
                        point(3.0, 3.0),
                        point(1.0, 3.0),
                    ],
                );
            }
        });
        // Cell (1, 1) of the 4-wide grid — inside both copies of the square.
        let doubled = got[4 + 1];
        assert!(
            (doubled - 2.0).abs() < 1e-6,
            "doubly-covered cell reported {doubled} instead of 2.0"
        );
    }

    // -----------------------------------------------------------------------
    // The grid-escape paths.
    //
    // The differential oracle CANNOT cover these: outside the grid the two
    // rasterizers deliberately differ (see the module docs), so an oracle case
    // here would be asserting a disagreement. What must hold is that every path
    // TERMINATES, writes nothing outside the readable grid, and leaves the
    // in-grid figure it shares the buffer with intact.
    // -----------------------------------------------------------------------

    /// A reference square, plus the same square with an escaping contour added.
    /// The escape may cost its own area; it may not corrupt the buffer's
    /// bounds, hang, or panic.
    fn escape_case(what: &str, escape: impl FnOnce(&mut Rasterizer)) {
        let mut r = Rasterizer::new(8, 8);
        polygon(
            &mut r,
            &[
                point(1.0, 1.0),
                point(3.0, 1.0),
                point(3.0, 3.0),
                point(1.0, 3.0),
            ],
        );
        escape(&mut r);
        let mut out = vec![0.0f32; 64];
        r.for_each_pixel(|i, a| out[i] = a);
        for (i, v) in out.iter().enumerate() {
            assert!(v.is_finite(), "{what}: cell {i} is not finite: {v}");
        }
    }

    /// Far LEFT of the grid — including from a row past the first, where the
    /// `linestart + x0i < 0` guard does not fire and the interior slab loop's
    /// lower bound is the thing doing the work.
    #[test]
    fn an_edge_far_left_of_the_grid_terminates() {
        escape_case("far left, row 0", |r| {
            r.draw_line(point(-1.0e7, 0.0), point(-1.0e7, 8.0));
        });
        escape_case("far left, crossing in", |r| {
            r.draw_line(point(-1.6e7, 4.0), point(1.6e7, 8.0));
        });
        escape_case("far left, single scanline", |r| {
            r.draw_line(point(-3.0e7, 5.1), point(4.0, 5.9));
        });
    }

    /// Far RIGHT of the grid, where the interior loop's upper bound and
    /// [`Rasterizer::add`]'s drop are what keep it bounded.
    #[test]
    fn an_edge_far_right_of_the_grid_terminates() {
        escape_case("far right", |r| {
            r.draw_line(point(3.0e7, 0.0), point(3.0e7, 8.0));
        });
        escape_case("far right, crossing out", |r| {
            r.draw_line(point(1.0, 0.5), point(3.0e2, 7.5));
        });
    }

    /// A span so wide that an unclamped interior loop would run for tens of
    /// millions of iterations per scanline. Simulating the pre-clamp arithmetic
    /// for this exact edge on a 4096×4096 grid gives 21,493,120 dropped-write
    /// iterations; the assertion is simply that the call returns.
    #[test]
    fn an_absurdly_wide_span_terminates() {
        let mut r = Rasterizer::new(4096, 4096);
        r.draw_line(point(-1.6e7, 0.0), point(1.6e7, 4096.0));
        r.draw_line(point(1.6e7, 4096.0), point(-1.6e7, 0.0));
        let mut nonzero = 0usize;
        r.for_each_pixel(|_, a| {
            if a != 0.0 {
                nonzero += 1;
            }
        });
        assert_eq!(nonzero, 0, "an edge and its reverse must still cancel");
    }

    /// A quadratic whose control point is astronomically far away asks the
    /// deviation heuristic for millions of flattening segments. The cap is what
    /// makes it return.
    #[test]
    fn an_absurd_quadratic_terminates() {
        escape_case("absurd quad", |r| {
            r.draw_quad(point(1.0, 1.0), point(1.0e15, 1.0e15), point(2.0, 7.0));
        });
        escape_case("absurd cubic", |r| {
            r.draw_cubic(
                point(1.0, 1.0),
                point(1.0e15, -1.0e15),
                point(-1.0e15, 1.0e15),
                point(2.0, 7.0),
            );
        });
    }

    /// Non-finite input must not hang or panic — a font can encode a coordinate
    /// that scales to infinity or NaN.
    ///
    /// Finiteness of the OUTPUT is deliberately not asserted, and neither
    /// rasterizer offers it: a NaN accumulator poisons the running prefix sum
    /// and the whole glyph reads NaN. What is asserted is the property that
    /// actually protects the frame — the call returns, and the quantisation
    /// every caller performs on the way to an 8-bit mask still yields bytes
    /// (Rust's saturating float→int cast makes `NaN as u8` zero, so a hostile
    /// coordinate costs a blank glyph, not a panic).
    #[test]
    fn non_finite_coordinates_terminate_without_panicking() {
        for (what, escape) in [
            (
                "infinite endpoint",
                Box::new(|r: &mut Rasterizer| {
                    r.draw_line(point(f32::INFINITY, 0.0), point(1.0, 8.0));
                }) as Box<dyn FnOnce(&mut Rasterizer)>,
            ),
            (
                "nan endpoint",
                Box::new(|r: &mut Rasterizer| {
                    r.draw_line(point(f32::NAN, 0.0), point(1.0, 8.0));
                }),
            ),
            (
                "nan control point",
                Box::new(|r: &mut Rasterizer| {
                    r.draw_quad(point(1.0, 1.0), point(f32::NAN, f32::NAN), point(2.0, 7.0));
                }),
            ),
            (
                "infinite cubic control point",
                Box::new(|r: &mut Rasterizer| {
                    r.draw_cubic(
                        point(1.0, 1.0),
                        point(f32::INFINITY, 1.0),
                        point(1.0, f32::NEG_INFINITY),
                        point(2.0, 7.0),
                    );
                }),
            ),
        ] {
            let mut r = Rasterizer::new(8, 8);
            escape(&mut r);
            let mut bytes = Vec::with_capacity(64);
            r.for_each_pixel(|_, a| bytes.push((a * 255.0 + 0.5).clamp(0.0, 255.0) as u8));
            assert_eq!(bytes.len(), 64, "{what}: the walk must visit every cell");
        }
    }

    /// A degenerate grid must not panic on construction or on a fill.
    #[test]
    fn degenerate_grids_are_inert() {
        for (w, h) in [(0usize, 0usize), (0, 4), (4, 0), (1, 1)] {
            let got = cov(w, h, |r| {
                polygon(r, &[point(0.5, 0.5), point(3.5, 0.5), point(3.5, 3.5)]);
            });
            assert_eq!(got.len(), w * h);
            for v in &got {
                assert!(v.is_finite());
            }
        }
    }
}

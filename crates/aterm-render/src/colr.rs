// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! COLR (vector color glyph) rasterization (M3).
//!
//! `sbix`/`CBDT` color emoji are bitmaps (handled via `glyph_raster_image`); COLR
//! glyphs are VECTOR — a stack of outline layers, each filled with a palette color
//! (COLRv0) or a gradient (COLRv1), under affine transforms. ttf-parser drives the
//! paint graph through its [`Painter`](ttf_parser::colr::Painter) trait; this module
//! implements that trait with a small self-contained scanline rasterizer (no extra
//! deps — outlines come from ttf-parser, the fill is ours), compositing each layer
//! into an RGBA8 buffer the renderer blits like any other color glyph.
//!
//! Scope: solid layers (COLRv0 + COLRv1 solid) render exactly, with full affine
//! transform support. Linear, radial and sweep gradients are filled PER PIXEL
//! from the `ColorLine` stops and extend mode (pad / repeat / reflect),
//! evaluated in paint space through the inverse of the affine stack; stop
//! interpolation is premultiplied-alpha, per the COLRv1 spec. Sweep angles
//! carry the spec's 1.0 bias (`degrees = v·180 + 180`), 0° at +x, measured
//! counter-clockwise in the font's y-up space. Ill-formed gradient geometry
//! (coincident linear points, coincident radial circles, an empty sweep) skips
//! the layer, as the spec requires. Clip boxes and composite modes beyond
//! src-over are treated as src-over (a documented refinement — the layer
//! outlines self-clip, which renders emoji-style layered COLR correctly).
//! Per the spec, a PaintGlyph outline is FIXED in the coordinate space where
//! the PaintGlyph appears: transforms pushed in its subtree reposition the
//! child paint's gradient geometry only, never the outline itself.

use ttf_parser::colr::{ColorStop, CompositeMode, GradientExtend, Paint, Painter};
use ttf_parser::{Face, GlyphId, RgbaColor, Transform};

/// A 2×3 affine: `(x,y) -> (a·x + c·y + e, b·x + d·y + f)` (ttf-parser convention).
#[derive(Clone, Copy)]
struct Affine {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Affine {
    fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
    /// `self ∘ t` — apply `t` first, then `self`.
    fn then(self, t: Affine) -> Affine {
        Affine {
            a: self.a * t.a + self.c * t.b,
            b: self.b * t.a + self.d * t.b,
            c: self.a * t.c + self.c * t.d,
            d: self.b * t.c + self.d * t.d,
            e: self.a * t.e + self.c * t.f + self.e,
            f: self.b * t.e + self.d * t.f + self.f,
        }
    }
    /// The inverse transform, or `None` when (near-)singular.
    fn invert(self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        Some(Affine {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }
}

impl From<Transform> for Affine {
    fn from(t: Transform) -> Self {
        Affine {
            a: t.a,
            b: t.b,
            c: t.c,
            d: t.d,
            e: t.e,
            f: t.f,
        }
    }
}

/// Flatten a glyph's outline into pixel-space contours (a flat polyline per
/// contour), transforming every point through `xform`.
struct OutlineCollector {
    xform: Affine,
    contours: Vec<Vec<(f32, f32)>>,
    cur: Vec<(f32, f32)>,
    last: (f32, f32),
}

impl OutlineCollector {
    fn flush(&mut self) {
        if self.cur.len() > 1 {
            self.contours.push(std::mem::take(&mut self.cur));
        } else {
            self.cur.clear();
        }
    }
    fn push(&mut self, x: f32, y: f32) {
        let (px, py) = self.xform.apply(x, y);
        self.cur.push((px, py));
        self.last = (x, y);
    }
}

impl ttf_parser::OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.push(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x0, y0) = self.last;
        const N: usize = 10;
        for i in 1..=N {
            let t = i as f32 / N as f32;
            let mt = 1.0 - t;
            let bx = mt * mt * x0 + 2.0 * mt * t * x1 + t * t * x;
            let by = mt * mt * y0 + 2.0 * mt * t * y1 + t * t * y;
            self.push(bx, by);
        }
        self.last = (x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x0, y0) = self.last;
        const N: usize = 12;
        for i in 1..=N {
            let t = i as f32 / N as f32;
            let mt = 1.0 - t;
            let bx =
                mt * mt * mt * x0 + 3.0 * mt * mt * t * x1 + 3.0 * mt * t * t * x2 + t * t * t * x;
            let by =
                mt * mt * mt * y0 + 3.0 * mt * mt * t * y1 + 3.0 * mt * t * t * y2 + t * t * t * y;
            self.push(bx, by);
        }
        self.last = (x, y);
    }
    fn close(&mut self) {
        self.flush();
    }
}

/// Component equality (kept explicit rather than leaning on ttf-parser's derive
/// surface, which has shifted between versions).
fn rgba_eq(a: RgbaColor, b: RgbaColor) -> bool {
    (a.red, a.green, a.blue, a.alpha) == (b.red, b.green, b.blue, b.alpha)
}

/// Premultiplied-alpha linear interpolation between two straight-alpha colors,
/// returned straight (identical to a straight lerp when both stops are opaque,
/// which is the common emoji case; the premultiplied form is what the COLRv1
/// spec prescribes and avoids gray fringes toward transparent stops).
fn lerp_premul(c0: RgbaColor, c1: RgbaColor, u: f32) -> RgbaColor {
    let (a0, a1) = (f32::from(c0.alpha) / 255.0, f32::from(c1.alpha) / 255.0);
    let a = a0 + (a1 - a0) * u;
    let ch = |x0: u8, x1: u8| {
        let p0 = f32::from(x0) * a0;
        let p1 = f32::from(x1) * a1;
        let p = p0 + (p1 - p0) * u;
        if a > 0.0 {
            (p / a).round().clamp(0.0, 255.0) as u8
        } else {
            0
        }
    };
    RgbaColor::new(
        ch(c0.red, c1.red),
        ch(c0.green, c1.green),
        ch(c0.blue, c1.blue),
        (a * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// Collect a gradient color line into (offset, color) pairs sorted by offset
/// (the spec allows unsorted stops; ttf-parser hands them through raw).
fn collect_stops(iter: impl Iterator<Item = ColorStop>) -> Vec<(f32, RgbaColor)> {
    let mut v: Vec<(f32, RgbaColor)> = iter.map(|s| (s.stop_offset, s.color)).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

/// Fold the COLRv1 linear-gradient `p2` rotation into a projection vector `q`
/// with `t(p) = (p − p0)·q / (q·q)`: lines of constant color run parallel to
/// p0→p2, and `t(p1) = 1`. `None` when the geometry is ill-formed per the spec
/// (p2 = p0, or p1 on the p0→p2 line — including p1 = p0).
fn linear_projection(dx: f32, dy: f32, rx: f32, ry: f32) -> Option<(f32, f32)> {
    let r2 = rx * rx + ry * ry;
    if r2 <= 0.0 || !r2.is_finite() {
        return None;
    }
    let k = (dx * rx + dy * ry) / r2;
    let (qx, qy) = (dx - k * rx, dy - k * ry);
    let q2 = qx * qx + qy * qy;
    if q2 > f32::EPSILON && q2.is_finite() {
        Some((qx, qy))
    } else {
        None
    }
}

/// A gradient color line resolved to concrete, sorted stops.
struct ColorRamp {
    extend: GradientExtend,
    /// (offset, straight-alpha color), ascending by offset, never empty.
    stops: Vec<(f32, RgbaColor)>,
}

impl ColorRamp {
    /// Sample the ramp at parameter `t`, extend-mapped over the stop range.
    fn sample(&self, t: f32) -> RgbaColor {
        let first = self.stops[0];
        let last = self.stops[self.stops.len() - 1];
        if t.is_nan() {
            return first.1;
        }
        let (lo, hi) = (first.0, last.0);
        let span = hi - lo;
        if span <= 0.0 {
            // All stops coincide; the portion above the shared offset shows the
            // last stop, and that is where pad extension lands.
            return last.1;
        }
        let t = match self.extend {
            GradientExtend::Pad => t.clamp(lo, hi),
            GradientExtend::Repeat => lo + (t - lo).rem_euclid(span),
            GradientExtend::Reflect => {
                let m = ((t - lo) / span).rem_euclid(2.0);
                lo + span * if m > 1.0 { 2.0 - m } else { m }
            }
        };
        if t <= first.0 {
            return first.1;
        }
        if t >= last.0 {
            return last.1;
        }
        let mut i = 0;
        while i + 1 < self.stops.len() && self.stops[i + 1].0 < t {
            i += 1;
        }
        let (o0, c0) = self.stops[i];
        let (o1, c1) = self.stops[i + 1];
        let d = o1 - o0;
        if d <= 0.0 {
            return c1;
        }
        lerp_premul(c0, c1, (t - o0) / d)
    }
}

/// Per-pixel gradient geometry in PAINT space (the coordinate system active
/// when the paint was applied).
enum GradientKind {
    /// `p0` plus the projected gradient vector `q` ([`linear_projection`]);
    /// `inv_q2` is `1/(q·q)`.
    Linear {
        x0: f32,
        y0: f32,
        qx: f32,
        qy: f32,
        inv_q2: f32,
    },
    /// Two-circle radial: center/radius `c0`,`r0` and the deltas to `c1`,`r1`.
    Radial {
        x0: f32,
        y0: f32,
        r0: f32,
        dx: f32,
        dy: f32,
        dr: f32,
    },
    /// Sweep around `(cx, cy)`, `sweep_deg` degrees from `start_deg` (both
    /// after the spec's bias), counter-clockwise when positive.
    Sweep {
        cx: f32,
        cy: f32,
        start_deg: f32,
        sweep_deg: f32,
    },
}

/// A gradient fill: geometry + color ramp + the device→paint inverse transform.
struct GradientFill {
    kind: GradientKind,
    ramp: ColorRamp,
    inv: Affine,
}

impl GradientFill {
    /// The gradient color under device pixel center `(x, y)`, or `None` where
    /// the gradient is undefined (outside a one-sided radial cone).
    fn sample_at(&self, x: f32, y: f32) -> Option<RgbaColor> {
        let (ux, uy) = self.inv.apply(x, y);
        let t = match self.kind {
            GradientKind::Linear {
                x0,
                y0,
                qx,
                qy,
                inv_q2,
            } => ((ux - x0) * qx + (uy - y0) * qy) * inv_q2,
            GradientKind::Radial {
                x0,
                y0,
                r0,
                dx,
                dy,
                dr,
            } => {
                // Solve |p − c(t)| = r(t) with c(t)=c0+t·cd, r(t)=r0+t·dr:
                // a·t² − 2b·t + c = 0 (the HTML-canvas two-circle equation).
                let (px, py) = (ux - x0, uy - y0);
                let a = dx * dx + dy * dy - dr * dr;
                let b = px * dx + py * dy + r0 * dr;
                let c = px * px + py * py - r0 * r0;
                let r_ok = |t: f32| r0 + t * dr >= 0.0;
                if a.abs() <= 1e-6 {
                    // Focal point on the end circle: degenerates to linear.
                    if b.abs() <= 1e-12 {
                        return None;
                    }
                    let t = c / (2.0 * b);
                    if !r_ok(t) {
                        return None;
                    }
                    t
                } else {
                    let disc = b * b - a * c;
                    if disc < 0.0 {
                        return None;
                    }
                    let sq = disc.sqrt();
                    let (t1, t2) = ((b + sq) / a, (b - sq) / a);
                    let (hi, lo) = if t1 >= t2 { (t1, t2) } else { (t2, t1) };
                    if r_ok(hi) {
                        hi
                    } else if r_ok(lo) {
                        lo
                    } else {
                        return None;
                    }
                }
            }
            GradientKind::Sweep {
                cx,
                cy,
                start_deg,
                sweep_deg,
            } => {
                let ang = (uy - cy).atan2(ux - cx).to_degrees();
                let diff = if sweep_deg >= 0.0 {
                    (ang - start_deg).rem_euclid(360.0)
                } else {
                    (start_deg - ang).rem_euclid(360.0)
                };
                diff / sweep_deg.abs()
            }
        };
        Some(self.ramp.sample(t))
    }
}

/// A resolved layer fill: one solid color, or a per-pixel gradient.
enum FillSource {
    Solid(RgbaColor),
    Gradient(GradientFill),
}

/// RGBA8 accumulator + the ttf-parser COLR painter state.
struct ColrCanvas<'a> {
    face: &'a Face<'a>,
    w: usize,
    h: usize,
    buf: Vec<u8>,
    stack: Vec<Affine>,
    cur: Affine,
    palette: u16,
    foreground: RgbaColor,
    /// The PaintGlyph clip waiting for its fill: the glyph id plus the affine
    /// that was CURRENT when `outline_glyph` ran. COLRv1 fixes the clip
    /// outline in the coordinate space where PaintGlyph appears; a transform
    /// pushed between PaintGlyph and its child paint moves only the paint
    /// (gradient geometry), never the outline — so the outline's transform
    /// must be captured here, not read from `cur` again at paint time.
    pending: Option<(GlyphId, Affine)>,
}

impl ColrCanvas<'_> {
    /// src-over composite `color` at coverage `cov` (0..=1) into pixel (x,y).
    fn blend(&mut self, x: usize, y: usize, color: RgbaColor, cov: f32) {
        if x >= self.w || y >= self.h {
            return;
        }
        let sa = f32::from(color.alpha) / 255.0 * cov;
        if sa <= 0.0 {
            return;
        }
        let i = (y * self.w + x) * 4;
        for (k, sc) in [color.red, color.green, color.blue].into_iter().enumerate() {
            let dst = f32::from(self.buf[i + k]);
            self.buf[i + k] = (f32::from(sc) * sa + dst * (1.0 - sa)).round() as u8;
        }
        let da = f32::from(self.buf[i + 3]) / 255.0;
        self.buf[i + 3] = ((sa + da * (1.0 - sa)) * 255.0).round() as u8;
    }

    /// Scanline-fill the pending PaintGlyph outline — transformed by the
    /// affine captured when `outline_glyph` ran, NOT by paint-time `cur` —
    /// with `src` (a solid color, or a gradient sampled per pixel), 4×
    /// vertical supersampling + analytic horizontal coverage, nonzero winding.
    fn fill_pending(&mut self, src: &FillSource) {
        let Some((gid, clip_xform)) = self.pending.take() else {
            return;
        };
        let mut oc = OutlineCollector {
            xform: clip_xform,
            contours: Vec::new(),
            cur: Vec::new(),
            last: (0.0, 0.0),
        };
        if self.face.outline_glyph(gid, &mut oc).is_none() {
            return;
        }
        oc.flush();
        let contours = oc.contours;
        if contours.is_empty() {
            return;
        }
        const SS: usize = 4;
        let (w, h) = (self.w, self.h);
        // Flatten the contours into ONE edge list per layer instead of re-walking
        // the Vec-of-Vec (with its `% n` wrap) for each of the h×SS sample lines.
        // The closing edge still comes from the `contour[(i + 1) % n]` rule, so
        // contour closure is preserved, and edges are emitted in the SAME
        // contour-then-index order — the per-scanline crossings are therefore
        // pushed in the same order and the STABLE `sort_by` below yields the
        // identical ordering. Horizontal edges are dropped: `y0 == y1` can never
        // satisfy the straddle test `(y0 <= sy && y1 > sy) || (y1 <= sy && y0 > sy)`
        // (one endpoint would have to be both ≤ and > sy), so removing them is
        // exact. A NaN endpoint makes `y0 == y1` false, so such an edge is KEPT and
        // still fails the test, exactly as before.
        let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
        // The layer's y extent, folded from the same edges. `f32::min`/`max` ignore
        // NaN, so a NaN endpoint cannot poison the range (and its edge can never
        // cross a scanline anyway).
        let (mut min_y, mut max_y) = (f32::INFINITY, f32::NEG_INFINITY);
        for contour in &contours {
            let n = contour.len();
            for i in 0..n {
                let (x0, y0) = contour[i];
                let (x1, y1) = contour[(i + 1) % n];
                if y0 == y1 {
                    continue;
                }
                edges.push((x0, y0, x1, y1));
                min_y = min_y.min(y0).min(y1);
                max_y = max_y.max(y0).max(y1);
            }
        }
        if edges.is_empty() {
            return;
        }
        // Scan only the rows the layer's y extent can reach. OUTSIDE that range no
        // edge satisfies the straddle test for any sample line, so `xs` would be
        // empty, `wind` would stay 0, `add_span` would never run and `cov` would
        // stay all-zero — and the `c > 0.0` guard already suppresses every `blend`
        // for a zero row. So the skipped rows wrote nothing: this is exact, not an
        // approximation. The 1-row margin absorbs the +0.125/+0.875 sample offsets.
        // Float→int casts saturate (and NaN → 0), so a degenerate extent collapses
        // to an empty range rather than wrapping.
        let py_lo = (min_y - 1.0).floor().max(0.0) as usize;
        let py_hi = ((max_y + 1.0).ceil().max(0.0) as usize).min(h);
        // Both scratch buffers are hoisted out of the loops and reused: `fill(0.0)`
        // is exactly the old fresh `vec![0.0f32; w]`, and `clear()` + the identical
        // push sequence is exactly the old fresh `Vec` — only the capacity survives.
        let mut cov = vec![0.0f32; w];
        let mut xs: Vec<(f32, i32)> = Vec::new();
        for py in py_lo..py_hi {
            cov.fill(0.0);
            for s in 0..SS {
                let sy = py as f32 + (s as f32 + 0.5) / SS as f32;
                // Collect (x, winding-dir) crossings of all edges at scanline sy.
                xs.clear();
                for &(x0, y0, x1, y1) in &edges {
                    if (y0 <= sy && y1 > sy) || (y1 <= sy && y0 > sy) {
                        let t = (sy - y0) / (y1 - y0);
                        xs.push((x0 + t * (x1 - x0), if y1 > y0 { 1 } else { -1 }));
                    }
                }
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut wind = 0;
                let mut prev = 0.0f32;
                for &(x, d) in &xs {
                    if wind != 0 {
                        add_span(&mut cov, prev, x, 1.0 / SS as f32);
                    }
                    wind += d;
                    prev = x;
                }
            }
            for (px, &c) in cov.iter().enumerate() {
                if c > 0.0 {
                    let color = match src {
                        FillSource::Solid(color) => *color,
                        FillSource::Gradient(g) => {
                            match g.sample_at(px as f32 + 0.5, py as f32 + 0.5) {
                                Some(color) => color,
                                // Undefined gradient region (outside a
                                // one-sided radial cone): nothing is painted.
                                None => continue,
                            }
                        }
                    };
                    self.blend(px, py, color, c.min(1.0));
                }
            }
        }
    }

    /// Resolve a `Paint` into a concrete fill. Gradients are evaluated per
    /// pixel; geometry the COLRv1 spec calls ill-formed (coincident linear
    /// points, coincident radial circles, an empty sweep) resolves to `None`
    /// and the layer is not rendered, exactly as the spec requires. A paint
    /// transform that cannot be inverted falls back to the first stop so the
    /// layer still shows. `stops(palette, coords)` — default (non-variable)
    /// coords, matching the outline path.
    fn resolve_paint(&self, paint: &Paint) -> Option<FillSource> {
        let (extend, stops, kind) = match paint {
            Paint::Solid(c) => return Some(FillSource::Solid(*c)),
            Paint::LinearGradient(g) => {
                let (qx, qy) =
                    linear_projection(g.x1 - g.x0, g.y1 - g.y0, g.x2 - g.x0, g.y2 - g.y0)?;
                (
                    g.extend,
                    collect_stops(g.stops(self.palette, &[])),
                    GradientKind::Linear {
                        x0: g.x0,
                        y0: g.y0,
                        qx,
                        qy,
                        inv_q2: 1.0 / (qx * qx + qy * qy),
                    },
                )
            }
            Paint::RadialGradient(g) => {
                let (dx, dy, dr) = (g.x1 - g.x0, g.y1 - g.y0, g.r1 - g.r0);
                if dx == 0.0 && dy == 0.0 && dr == 0.0 {
                    return None;
                }
                (
                    g.extend,
                    collect_stops(g.stops(self.palette, &[])),
                    GradientKind::Radial {
                        x0: g.x0,
                        y0: g.y0,
                        r0: g.r0,
                        dx,
                        dy,
                        dr,
                    },
                )
            }
            Paint::SweepGradient(g) => {
                // F2DOT14 angles carry the spec's 1.0 bias: degrees = v·180+180.
                let start_deg = g.start_angle * 180.0 + 180.0;
                let sweep_deg = (g.end_angle - g.start_angle) * 180.0;
                if sweep_deg == 0.0 || !sweep_deg.is_finite() {
                    return None;
                }
                (
                    g.extend,
                    collect_stops(g.stops(self.palette, &[])),
                    GradientKind::Sweep {
                        cx: g.center_x,
                        cy: g.center_y,
                        start_deg,
                        sweep_deg,
                    },
                )
            }
        };
        // An empty color line: keep the pre-gradient fallback (foreground)
        // rather than dropping the layer of a degenerate font.
        let Some(&(_, first)) = stops.first() else {
            return Some(FillSource::Solid(self.foreground));
        };
        // One stop — or several that all carry one color — is a solid fill.
        if stops.iter().all(|s| rgba_eq(s.1, first)) {
            return Some(FillSource::Solid(first));
        }
        let Some(inv) = self.cur.invert() else {
            return Some(FillSource::Solid(first));
        };
        Some(FillSource::Gradient(GradientFill {
            kind,
            ramp: ColorRamp { extend, stops },
            inv,
        }))
    }
}

impl<'a> Painter<'a> for ColrCanvas<'a> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        // Snapshot `cur` NOW: this is the space the PaintGlyph clip lives in.
        // A PaintTransform in the SUBTREE (push_transform arriving before the
        // child paint) repositions gradient geometry only. Fluent leans on
        // this: 🔥's radial ramps sit under mirroring transforms whose
        // pad-extended far ends are alpha 0 — warping the outline through
        // those same transforms both displaces the silhouette and samples the
        // ramp out where it is transparent, all but erasing the glyph. When
        // PaintGlyphs nest, each snapshot pairs the inner glyph with the
        // transforms above ITS PaintGlyph (outer clips are not intersected —
        // the module-level src-over/self-clip simplification, unchanged).
        self.pending = Some((glyph_id, self.cur));
    }
    fn paint(&mut self, paint: Paint<'a>) {
        match self.resolve_paint(&paint) {
            Some(src) => self.fill_pending(&src),
            // Ill-formed gradient geometry: the spec says the layer must not
            // render; consume the pending outline so painter state stays sane.
            None => self.pending = None,
        }
    }
    fn push_transform(&mut self, transform: Transform) {
        self.stack.push(self.cur);
        self.cur = self.cur.then(Affine::from(transform));
    }
    fn pop_transform(&mut self) {
        if let Some(t) = self.stack.pop() {
            self.cur = t;
        }
    }
    // Clip + layer composition beyond src-over are not modeled; the layer outlines
    // self-clip, so ignoring these renders solid-layer COLR correctly.
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, _clipbox: ttf_parser::colr::ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _mode: CompositeMode) {}
    fn pop_layer(&mut self) {}
}

/// Rasterize the COLR color glyph `gid` into an `w×h` RGBA8 buffer (em square fit
/// to the box, centered, y-flipped). Returns `None` if `gid` is not a COLR glyph or
/// the face has no COLR table. Pure + panic-free.
pub(crate) fn rasterize_colr(face: &Face, gid: GlyphId, w: usize, h: usize) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || !face.is_color_glyph(gid) {
        return None;
    }
    let upem = f32::from(face.units_per_em());
    if upem <= 0.0 {
        return None;
    }
    // Fit the em square into the box (by height), centered horizontally, y-flipped:
    // font (0,0) -> bottom-left, (upem,upem) -> top, so the glyph fills the cell.
    let scale = h as f32 / upem;
    let base = Affine {
        a: scale,
        b: 0.0,
        c: 0.0,
        d: -scale,
        e: (w as f32 - upem * scale) / 2.0,
        f: h as f32,
    };
    let fg = RgbaColor::new(0, 0, 0, 255);
    let mut canvas = ColrCanvas {
        face,
        w,
        h,
        buf: vec![0u8; w * h * 4],
        stack: Vec::new(),
        cur: base,
        palette: 0,
        foreground: fg,
        pending: None,
    };
    face.paint_color_glyph(gid, 0, fg, &mut canvas)?;
    // The blend accumulator stored PREMULTIPLIED rgb — each layer composited over a
    // transparent start yields rgb = color·alpha — but `GlyphImage::Rgba` is blitted
    // as STRAIGHT alpha (blit_rgba_* do bg·(1−a) + rgb·a, like the PNG color-emoji
    // path). Un-premultiply so a partially-transparent COLR layer is not darkened
    // twice (rendered at ~a² instead of a). Fully-opaque/transparent pixels are
    // already correct (×1 / rgb=0), so skip them — the opaque case stays byte-exact.
    let mut buf = canvas.buf;
    unpremultiply_straight(&mut buf);
    Some(buf)
}

/// Convert a premultiplied RGBA8 accumulator to STRAIGHT alpha in place.
/// Fully-opaque/transparent pixels are already correct (×1 / rgb=0), so they
/// are skipped — the opaque case stays byte-exact.
fn unpremultiply_straight(buf: &mut [u8]) {
    for px in buf.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        if a == 0 || a == 255 {
            continue;
        }
        let (rgb, _alpha) = px.split_at_mut(3);
        for c in rgb {
            *c = (((u32::from(*c) * 255) + a / 2) / a).min(255) as u8;
        }
    }
}

/// Add `amount` coverage to `cov` over the half-open pixel span `[x0, x1)`, with
/// fractional coverage at the partially-covered end pixels.
fn add_span(cov: &mut [f32], x0: f32, x1: f32, amount: f32) {
    let (x0, x1) = (x0.max(0.0), x1.min(cov.len() as f32));
    if x1 <= x0 {
        return;
    }
    let xi0 = x0.floor() as usize;
    let xi1 = x1.ceil() as usize;
    for (px, slot) in cov.iter_mut().enumerate().take(xi1).skip(xi0) {
        let left = (px as f32).max(x0);
        let right = ((px + 1) as f32).min(x1);
        if right > left {
            *slot += amount * (right - left);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_span_full_and_partial_coverage() {
        let mut cov = vec![0.0f32; 5];
        add_span(&mut cov, 1.0, 3.0, 1.0); // pixels 1,2 fully
        assert_eq!(cov, vec![0.0, 1.0, 1.0, 0.0, 0.0]);
        let mut cov = vec![0.0f32; 5];
        add_span(&mut cov, 1.5, 2.5, 1.0); // half of 1, half of 2
        assert!((cov[1] - 0.5).abs() < 1e-6 && (cov[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn affine_compose_then_apply() {
        // Translate-then-scale composition matches manual math.
        let scale = Affine {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 2.0,
            e: 0.0,
            f: 0.0,
        };
        let translate = Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 3.0,
            f: 4.0,
        };
        let m = scale.then(translate); // apply translate first, then scale
        assert_eq!(m.apply(0.0, 0.0), (6.0, 8.0));
    }

    /// Locate ttf-parser's bundled COLR test font in the cargo registry cache, if
    /// present (it ships `tests/fonts/colr_1.ttf`). Returns the bytes or `None`.
    fn colr_test_font() -> Option<Vec<u8>> {
        let home = std::env::var_os("CARGO_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cargo"))
            })?;
        let reg = home.join("registry/src");
        let entries = std::fs::read_dir(&reg).ok()?;
        for idx in entries.flatten() {
            let mut p = idx.path();
            // newest ttf-parser-* dir wins (any works for the COLR font)
            if let Ok(rd) = std::fs::read_dir(&p) {
                let mut best: Option<std::path::PathBuf> = None;
                for e in rd.flatten() {
                    let name = e.file_name();
                    if name.to_string_lossy().starts_with("ttf-parser-") {
                        best = Some(e.path());
                    }
                }
                if let Some(b) = best {
                    p = b.join("tests/fonts/colr_1.ttf");
                    if let Ok(bytes) = std::fs::read(&p) {
                        return Some(bytes);
                    }
                }
            }
        }
        None
    }

    #[test]
    fn colr_glyph_rasterizes_straight_alpha() {
        let Some(bytes) = colr_test_font() else {
            eprintln!("SKIP: ttf-parser colr_1.ttf not in the cargo cache");
            return;
        };
        let face = Face::parse(&bytes, 0).expect("valid font");
        // The blend accumulator stores PREMULTIPLIED rgb but `GlyphImage::Rgba`
        // is blitted as STRAIGHT alpha, so `rasterize_colr` un-premultiplies
        // before returning. The exact arithmetic is pinned by the synthetic
        // `unpremultiply_restores_straight_rgb` (this font's one translucent
        // solid is BLACK, which premultiplication cannot darken further); the
        // end-to-end probe here pins the rest: every single-SOLID-layer glyph
        // comes back carrying its layer's palette rgb at each fully-covered
        // pixel, with the layer's alpha landing in the alpha channel.
        // (The font's solid glyphs are probed, not assumed: its FIRST color
        // glyph is a linear gradient, which renders as a per-pixel spread.)
        let fg = RgbaColor::new(0, 0, 0, 255);
        let (mut opaque_checked, mut translucent_checked) = (false, false);
        for gid in (0..face.number_of_glyphs()).map(GlyphId) {
            if !face.is_color_glyph(gid) {
                continue;
            }
            let mut probe = PaintProbe::default();
            if face.paint_color_glyph(gid, 0, fg, &mut probe).is_none()
                || probe.solid_layers != 1
                || probe.gradient_layers != 0
            {
                continue;
            }
            let color = probe.solid_color.expect("solid layer recorded");
            if color.alpha == 0 {
                continue;
            }
            let (w, h) = (48, 48);
            let rgba = rasterize_colr(&face, gid, w, h).expect("COLR glyph rasterizes");
            assert_eq!(rgba.len(), w * h * 4);
            // Fully-covered pixels land at exactly the layer's alpha.
            let full: Vec<&[u8; 4]> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[3] == color.alpha)
                .collect();
            assert!(!full.is_empty(), "gid {}: no fully-covered pixels", gid.0);
            for p in &full {
                assert!(
                    p[0].abs_diff(color.red) <= 2
                        && p[1].abs_diff(color.green) <= 2
                        && p[2].abs_diff(color.blue) <= 2,
                    "gid {}: fully-covered pixel {:?} (a={}) must carry the layer's \
                     straight rgb ({},{},{}), not a premultiplied-dark value",
                    gid.0,
                    &p[..3],
                    p[3],
                    color.red,
                    color.green,
                    color.blue
                );
            }
            if color.alpha == 255 {
                opaque_checked = true;
            } else {
                translucent_checked = true;
            }
        }
        assert!(
            opaque_checked && translucent_checked,
            "colr_1.ttf must exercise both the opaque fast path and the \
             un-premultiply path (opaque={opaque_checked}, translucent={translucent_checked})"
        );
    }

    #[test]
    fn unpremultiply_restores_straight_rgb() {
        // A premultiplied half-transparent orange (straight 200,100,40 at
        // α=128 → premultiplied 100,50,20), an opaque pixel (byte-exact
        // passthrough) and a transparent one (untouched).
        let mut buf = vec![
            100, 50, 20, 128, //
            9, 8, 7, 255, //
            0, 0, 0, 0,
        ];
        unpremultiply_straight(&mut buf);
        let p = &buf[0..4];
        assert!(
            p[0].abs_diff(200) <= 2 && p[1].abs_diff(100) <= 2 && p[2].abs_diff(40) <= 2,
            "straight rgb must be restored, got {p:?}"
        );
        assert_eq!(&buf[4..8], &[9, 8, 7, 255], "opaque pixels stay byte-exact");
        assert_eq!(&buf[8..12], &[0, 0, 0, 0], "transparent pixels stay zero");
    }

    const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// A gradient stop as the fixtures below write it: an offset plus a raw RGBA
    /// quadruple, before `RgbaColor::new` gives it a type. Named because the tuple
    /// nests two deep and `-D warnings` counts that as a complex type.
    type RawStop = (f32, (u8, u8, u8, u8));

    fn ramp(extend: GradientExtend, stops: &[RawStop]) -> ColorRamp {
        ColorRamp {
            extend,
            stops: stops
                .iter()
                .map(|&(o, (r, g, b, a))| (o, RgbaColor::new(r, g, b, a)))
                .collect(),
        }
    }

    #[test]
    fn color_ramp_pad_repeat_reflect() {
        let two = |e| ramp(e, &[(0.0, (0, 0, 0, 255)), (1.0, (200, 100, 50, 255))]);
        // The midpoint interpolates each channel linearly.
        let mid = two(GradientExtend::Pad).sample(0.5);
        assert_eq!(
            (mid.red, mid.green, mid.blue, mid.alpha),
            (100, 50, 25, 255)
        );
        // Pad clamps outside the stop range.
        let p = two(GradientExtend::Pad);
        assert_eq!(p.sample(-1.0).red, 0);
        assert_eq!(p.sample(2.0).red, 200);
        // Repeat tiles: t=1.25 ≡ 0.25. Reflect mirrors: t=1.25 ≡ 0.75.
        assert_eq!(
            two(GradientExtend::Repeat).sample(1.25).red,
            p.sample(0.25).red
        );
        assert_eq!(
            two(GradientExtend::Reflect).sample(1.25).red,
            p.sample(0.75).red
        );
    }

    #[test]
    fn stop_interpolation_is_premultiplied() {
        // Opaque red → transparent blue at u=0.5: the premultiplied midpoint
        // keeps the straight rgb fully red at half alpha; a straight-space lerp
        // would muddy it toward (128,0,128).
        let c = lerp_premul(
            RgbaColor::new(255, 0, 0, 255),
            RgbaColor::new(0, 0, 255, 0),
            0.5,
        );
        assert_eq!((c.red, c.green, c.blue, c.alpha), (255, 0, 0, 128));
    }

    #[test]
    fn linear_projection_respects_p2_rotation() {
        // p2 perpendicular to p0→p1: plain projection along p0→p1.
        assert_eq!(linear_projection(10.0, 0.0, 0.0, 10.0), Some((10.0, 0.0)));
        // Skewed p2: constant-colour lines run parallel to p0→p2 —
        // t(p1) == 1 and t(p0 + r) == 0.
        let (qx, qy) = linear_projection(10.0, 0.0, 5.0, 10.0).expect("well-formed");
        let q2 = qx * qx + qy * qy;
        assert!(((10.0 * qx) / q2 - 1.0).abs() < 1e-5);
        assert!(((5.0 * qx + 10.0 * qy) / q2).abs() < 1e-5);
        // p1 ON the p0→p2 line (and p2 == p0) are ill-formed per the spec.
        assert_eq!(linear_projection(5.0, 10.0, 5.0, 10.0), None);
        assert_eq!(linear_projection(10.0, 0.0, 0.0, 0.0), None);
    }

    #[test]
    fn radial_t_is_radius_fraction() {
        // Classic concentric radial (r0=0 → r1=10 around the origin).
        let g = GradientFill {
            kind: GradientKind::Radial {
                x0: 0.0,
                y0: 0.0,
                r0: 0.0,
                dx: 0.0,
                dy: 0.0,
                dr: 10.0,
            },
            ramp: ramp(
                GradientExtend::Pad,
                &[(0.0, (0, 0, 0, 255)), (1.0, (200, 0, 0, 255))],
            ),
            inv: IDENTITY,
        };
        assert_eq!(g.sample_at(0.0, 0.0).expect("defined").red, 0);
        assert_eq!(g.sample_at(5.0, 0.0).expect("defined").red, 100);
        assert_eq!(g.sample_at(0.0, 10.0).expect("defined").red, 200);
        // Pad extends beyond r1.
        assert_eq!(g.sample_at(30.0, 0.0).expect("defined").red, 200);
    }

    #[test]
    fn sweep_t_follows_angle() {
        let g = GradientFill {
            kind: GradientKind::Sweep {
                cx: 0.0,
                cy: 0.0,
                start_deg: 0.0,
                sweep_deg: 180.0,
            },
            ramp: ramp(
                GradientExtend::Pad,
                &[(0.0, (0, 0, 0, 255)), (1.0, (200, 0, 0, 255))],
            ),
            inv: IDENTITY,
        };
        assert_eq!(g.sample_at(10.0, 0.0).expect("defined").red, 0); // 0°
        assert_eq!(g.sample_at(0.0, 10.0).expect("defined").red, 100); // 90° CCW
        assert_eq!(g.sample_at(-10.0, 1e-4).expect("defined").red, 200); // 180°
    }

    /// Records how many painted layers were multi-colour gradients vs solids
    /// (and the last solid colour seen), without rasterizing anything.
    #[derive(Default)]
    struct PaintProbe {
        gradient_layers: usize,
        solid_layers: usize,
        solid_color: Option<RgbaColor>,
    }

    impl Painter<'_> for PaintProbe {
        fn outline_glyph(&mut self, _: GlyphId) {}
        fn paint(&mut self, paint: Paint<'_>) {
            let stops: Vec<RgbaColor> = match &paint {
                Paint::Solid(c) => {
                    self.solid_layers += 1;
                    self.solid_color = Some(*c);
                    return;
                }
                Paint::LinearGradient(g) => g.stops(0, &[]).map(|s| s.color).collect(),
                Paint::RadialGradient(g) => g.stops(0, &[]).map(|s| s.color).collect(),
                Paint::SweepGradient(g) => g.stops(0, &[]).map(|s| s.color).collect(),
            };
            let multi = stops
                .first()
                .is_some_and(|f| stops.iter().any(|c| !rgba_eq(*c, *f)));
            if multi {
                self.gradient_layers += 1;
            } else {
                self.solid_layers += 1;
            }
        }
        fn push_transform(&mut self, _: Transform) {}
        fn pop_transform(&mut self) {}
        fn push_clip(&mut self) {}
        fn push_clip_box(&mut self, _: ttf_parser::colr::ClipBox) {}
        fn pop_clip(&mut self) {}
        fn push_layer(&mut self, _: CompositeMode) {}
        fn pop_layer(&mut self) {}
    }

    /// A painter identical to [`ColrCanvas`] except that every gradient is
    /// FLATTENED to its first colour stop — the old renderer's behaviour,
    /// reconstructed as a reference so tests can measure exactly what the
    /// per-pixel gradient fill changes. Referenced only by the seguiemj guard
    /// below, so gated to Windows the way `variable_system_face` is in
    /// `hinted.rs` — `-D warnings` would flag it as dead code elsewhere.
    #[cfg(windows)]
    struct FlattenCanvas<'a> {
        inner: ColrCanvas<'a>,
    }

    #[cfg(windows)]
    impl<'a> Painter<'a> for FlattenCanvas<'a> {
        fn outline_glyph(&mut self, gid: GlyphId) {
            // Delegate so the clip-transform snapshot lives in ONE place.
            self.inner.outline_glyph(gid);
        }
        fn paint(&mut self, paint: Paint<'a>) {
            match self.inner.resolve_paint(&paint) {
                Some(FillSource::Gradient(g)) => self
                    .inner
                    .fill_pending(&FillSource::Solid(g.ramp.stops[0].1)),
                Some(solid) => self.inner.fill_pending(&solid),
                None => self.inner.pending = None,
            }
        }
        fn push_transform(&mut self, t: Transform) {
            self.inner.push_transform(t);
        }
        fn pop_transform(&mut self) {
            self.inner.pop_transform();
        }
        fn push_clip(&mut self) {}
        fn push_clip_box(&mut self, _: ttf_parser::colr::ClipBox) {}
        fn pop_clip(&mut self) {}
        fn push_layer(&mut self, _: CompositeMode) {}
        fn pop_layer(&mut self) {}
    }

    /// Render `gid` exactly as [`rasterize_colr`] does (same box-fit affine,
    /// palette, foreground and un-premultiply) but through [`FlattenCanvas`].
    #[cfg(windows)]
    fn rasterize_first_stop_reference(
        face: &Face,
        gid: GlyphId,
        w: usize,
        h: usize,
    ) -> Option<Vec<u8>> {
        let upem = f32::from(face.units_per_em());
        if upem <= 0.0 {
            return None;
        }
        let scale = h as f32 / upem;
        let base = Affine {
            a: scale,
            b: 0.0,
            c: 0.0,
            d: -scale,
            e: (w as f32 - upem * scale) / 2.0,
            f: h as f32,
        };
        let fg = RgbaColor::new(0, 0, 0, 255);
        let mut canvas = FlattenCanvas {
            inner: ColrCanvas {
                face,
                w,
                h,
                buf: vec![0u8; w * h * 4],
                stack: Vec::new(),
                cur: base,
                palette: 0,
                foreground: fg,
                pending: None,
            },
        };
        face.paint_color_glyph(gid, 0, fg, &mut canvas)?;
        let mut buf = canvas.inner.buf;
        unpremultiply_straight(&mut buf);
        Some(buf)
    }

    /// The flatness metric: the maximum number of distinct straight colours
    /// inside any single horizontal or vertical run of CONSTANT alpha
    /// (α ≥ 128, run length ≥ 8 px). Chosen because it is immune to the two
    /// ways a first-stop-flattened render can still amass distinct colours
    /// glyph-wide — layer-over-layer alpha compositing (each overlap region
    /// blends its own shade) and coverage/premultiply artefacts (both scale
    /// rgb per ALPHA, so under constant alpha a flat region stays ONE colour).
    /// A flattened render is piecewise-constant within such a run (1 colour,
    /// rarely 2-3 at a region coincidence); a per-pixel gradient sweeps many.
    /// α ≥ 128 rather than = 255 because Fluent glyphs like 🔥 are built
    /// entirely from less-than-fully-opaque gradient stops.
    fn max_constant_alpha_run_spread(rgba: &[u8], w: usize, h: usize) -> usize {
        let px = rgba.as_chunks::<4>().0;
        assert_eq!(px.len(), w * h);
        fn scan(seq: impl Iterator<Item = [u8; 4]>, best: &mut usize) {
            let mut colors: std::collections::HashSet<(u8, u8, u8)> =
                std::collections::HashSet::new();
            let mut run_alpha = 0u8;
            let mut len = 0usize;
            // The transparent sentinel forces the final flush.
            for p in seq.chain(std::iter::once([0, 0, 0, 0])) {
                if p[3] >= 128 && (len == 0 || p[3] == run_alpha) {
                    run_alpha = p[3];
                    colors.insert((p[0], p[1], p[2]));
                    len += 1;
                    continue;
                }
                // Run ends: alpha changed, dropped below 128, or the sentinel.
                if len >= 8 {
                    *best = (*best).max(colors.len());
                }
                colors.clear();
                len = 0;
                if p[3] >= 128 {
                    run_alpha = p[3];
                    colors.insert((p[0], p[1], p[2]));
                    len = 1;
                }
            }
        }
        let mut best = 0;
        for y in 0..h {
            scan((0..w).map(|x| px[y * w + x]), &mut best);
        }
        for x in 0..w {
            scan((0..h).map(|y| px[y * w + x]), &mut best);
        }
        best
    }

    /// REGRESSION (Windows parity rank 7 — "colour emoji stop rendering
    /// flat"): gradients used to collapse to their first colour stop, so a
    /// pure-gradient glyph rasterized to at most one opaque colour per layer.
    /// The per-pixel fill must produce a genuine spread.
    #[test]
    fn colr_gradient_renders_a_colour_spread() {
        let Some(bytes) = colr_test_font() else {
            eprintln!("SKIP: ttf-parser colr_1.ttf not in the cargo cache");
            return;
        };
        let face = Face::parse(&bytes, 0).expect("valid font");
        let fg = RgbaColor::new(0, 0, 0, 255);
        let mut best = 0usize;
        for gid in (0..face.number_of_glyphs()).map(GlyphId) {
            if !face.is_color_glyph(gid) {
                continue;
            }
            let mut probe = PaintProbe::default();
            if face.paint_color_glyph(gid, 0, fg, &mut probe).is_none() {
                continue;
            }
            // Only pure-gradient glyphs with few layers (belt-and-braces on
            // top of the run metric's own compositing immunity).
            if probe.gradient_layers == 0 || probe.solid_layers != 0 || probe.gradient_layers > 4 {
                continue;
            }
            let Some(rgba) = rasterize_colr(&face, gid, 64, 64) else {
                continue;
            };
            best = best.max(max_constant_alpha_run_spread(&rgba, 64, 64));
            if best >= 8 {
                break;
            }
        }
        assert!(
            best >= 8,
            "a multi-stop gradient glyph must sweep >= 8 distinct colours \
             within one constant-alpha run, best glyph gave {best}"
        );
    }

    /// The font Windows actually ships: Fluent's entire emoji design is
    /// gradient fills, and the old first-stop flattening read as poster paint
    /// next to every other app on the OS. Guard the per-pixel gradient path on
    /// seguiemj itself. Skips when the font is absent or carries no COLRv1
    /// gradients (pre-Win11 builds are COLRv0).
    #[cfg(windows)]
    #[test]
    fn seguiemj_fluent_emoji_are_not_flat() {
        let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\seguiemj.ttf") else {
            eprintln!("SKIP: no seguiemj.ttf on this host");
            return;
        };
        let face = Face::parse(&bytes, 0).expect("seguiemj parses");
        let fg = RgbaColor::new(0, 0, 0, 255);
        let mut checked = 0usize;
        for ch in ['😀', '🔥', '🚀'] {
            let Some(gid) = face.glyph_index(ch) else {
                continue;
            };
            let mut probe = PaintProbe::default();
            if face.paint_color_glyph(gid, 0, fg, &mut probe).is_none()
                || probe.gradient_layers == 0
            {
                continue; // COLRv0-era font: nothing to guard
            }
            // Fluent's gradients are SUBTLE (a 128px face sweeps only ~20
            // quantized shades), so no absolute colour-count threshold can
            // separate them from a flattened render's own AA/compositing
            // noise. Compare against ground truth instead: the same glyph
            // through the same canvas with gradients first-stop-flattened. A
            // flattening regression makes the two renders identical, so this
            // can never pass vacuously.
            let (w, h) = (64, 64);
            let real = rasterize_colr(&face, gid, w, h).expect("emoji rasterizes");
            let flat = rasterize_first_stop_reference(&face, gid, w, h).expect("reference renders");
            let (mut painted, mut differing) = (0usize, 0usize);
            let (mut real_painted, mut real_opaque, mut flat_painted) = (0usize, 0usize, 0usize);
            for (r, f) in real.as_chunks::<4>().0.iter().zip(flat.as_chunks::<4>().0) {
                if r[3] > 8 {
                    real_painted += 1;
                    if r[3] == 255 {
                        real_opaque += 1;
                    }
                }
                if f[3] > 8 {
                    flat_painted += 1;
                }
                if r[3] == 0 && f[3] == 0 {
                    continue;
                }
                painted += 1;
                if r.iter().zip(f).any(|(a, b)| a.abs_diff(*b) > 4) {
                    differing += 1;
                }
            }
            assert!(painted > 0, "U+{:X} painted nothing", ch as u32);
            let frac = differing as f64 / painted as f64;
            eprintln!(
                "U+{:X}: {} gradient + {} solid layers; {differing}/{painted} px \
                 ({:.0}%) differ from the first-stop flattening; real render \
                 {real_painted} painted (a>8) / {real_opaque} opaque px, flat \
                 reference {flat_painted} painted",
                ch as u32,
                probe.gradient_layers,
                probe.solid_layers,
                frac * 100.0
            );
            assert!(
                frac >= 0.10,
                "U+{:X} renders (near-)flat: only {:.1}% of painted pixels differ \
                 from the first-stop reference",
                ch as u32,
                frac * 100.0
            );
            // "Differs from flat" alone is a dishonest oracle: ERASING the
            // glyph differs too (the first gradient attempt warped outlines
            // through paint-time transforms, sampled Fluent's pad ramps out at
            // their alpha-0 ends, and 🔥 dropped from ~2900 to ~670 painted
            // px — and this test cheered). Differing must come from COLOUR,
            // not from lost coverage: the gradient render must paint
            // essentially every pixel the flat fill of the SAME outlines
            // paints, and a solid core of them at full opacity.
            assert!(
                real_painted as f64 >= 0.90 * flat_painted as f64,
                "U+{:X}: gradient render erased coverage: {real_painted} painted \
                 px (a>8) vs {flat_painted} in the flat reference",
                ch as u32
            );
            assert!(
                real_opaque as f64 >= 0.25 * flat_painted as f64,
                "U+{:X}: gradient render lost its opaque core: {real_opaque} \
                 fully-opaque px vs {flat_painted} flat-painted",
                ch as u32
            );
            checked += 1;
        }
        if checked == 0 {
            eprintln!("SKIP: seguiemj here has no COLRv1 gradient emoji");
        }
    }
}

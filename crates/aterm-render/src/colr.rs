// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! COLR (vector color glyph) rasterization.
//!
//! `sbix`/`CBDT` emoji are bitmaps (`glyph_raster_image`); COLR glyphs are outline layers
//! filled with a palette color (COLRv0) or a gradient (COLRv1) under affine transforms.
//! aterm-font walks the paint graph through its [`Painter`](aterm_font::colr::Painter)
//! trait; this module implements it with a small scanline rasterizer into an RGBA8 buffer.
//!
//! Solid layers render exactly. Linear, radial and sweep gradients are filled per pixel
//! from the `ColorLine` stops and extend mode, in paint space through the inverse affine,
//! with premultiplied stop interpolation. Sweep angles carry the spec's 1.0 bias
//! (`degrees = v·180 + 180`), 0° at +x, counter-clockwise in y-up font space. Ill-formed
//! gradient geometry skips the layer, as the spec requires. Clips are ignored and every
//! composite mode is src-over; layer outlines self-clip, which is enough for emoji. A
//! PaintGlyph outline is fixed in the space where the PaintGlyph appears: transforms in its
//! subtree move only the child paint's gradient geometry.

use aterm_font::colr::{ColorStop, CompositeMode, GradientExtend, Paint, Painter};
use aterm_font::{Face, GlyphId, RgbaColor, Transform};

/// A 2×3 affine: `(x,y) -> (a·x + c·y + e, b·x + d·y + f)` (aterm-font convention).
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
    #[inline]
    const fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
    /// `self ∘ t` — apply `t` first, then `self`.
    #[inline]
    const fn then(self, t: Affine) -> Affine {
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
    #[inline]
    const fn invert(self) -> Option<Affine> {
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

impl aterm_font::OutlineSink for OutlineCollector {
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

/// Component-wise color equality.
#[inline]
const fn rgba_eq(a: RgbaColor, b: RgbaColor) -> bool {
    a.red == b.red && a.green == b.green && a.blue == b.blue && a.alpha == b.alpha
}

/// Premultiplied-alpha lerp between two straight-alpha colors, returned straight. The
/// COLRv1 spec prescribes it; it avoids gray fringes toward transparent stops.
#[inline]
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
/// (the spec allows unsorted stops; aterm-font hands them through raw).
fn collect_stops(iter: impl Iterator<Item = ColorStop>) -> Vec<(f32, RgbaColor)> {
    let mut v: Vec<(f32, RgbaColor)> = iter.map(|s| (s.stop_offset, s.color)).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

/// Fold the COLRv1 linear-gradient `p2` rotation into a projection vector `q`
/// with `t(p) = (p − p0)·q / (q·q)`: lines of constant color run parallel to
/// p0→p2, and `t(p1) = 1`. `None` when the geometry is ill-formed per the spec
/// (p2 = p0, or p1 on the p0→p2 line — including p1 = p0).
#[inline]
const fn linear_projection(dx: f32, dy: f32, rx: f32, ry: f32) -> Option<(f32, f32)> {
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

/// Per-pixel gradient geometry in paint space (the coordinate system active
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

/// RGBA8 accumulator + the aterm-font COLR painter state.
struct ColrCanvas<'a> {
    face: &'a Face<'a>,
    w: usize,
    h: usize,
    buf: Vec<u8>,
    stack: Vec<Affine>,
    cur: Affine,
    palette: u16,
    foreground: RgbaColor,
    /// The PaintGlyph clip waiting for its fill: the glyph id and the affine current when
    /// `outline_glyph` ran. COLRv1 fixes the outline in the space where PaintGlyph
    /// appears, so it is captured then, not read from `cur` at paint time.
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

    /// Scanline-fill the pending PaintGlyph outline (in the affine captured by
    /// `outline_glyph`, not paint-time `cur`) with `src`: 4× vertical supersampling,
    /// analytic horizontal coverage, nonzero winding.
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
        // One flat edge list per layer, closed by the `(i + 1) % n` rule. Horizontal edges
        // never straddle a sample line, so dropping them is exact.
        let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
        // The layer's y extent. `f32::min`/`max` ignore NaN, so a NaN endpoint cannot
        // poison it.
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
        // Scan only rows the layer's y extent reaches: outside it no edge straddles a
        // sample line, so skipping them is exact. The 1-row margin covers the sample
        // offsets; saturating casts collapse a degenerate extent to an empty range.
        let py_lo = (min_y - 1.0).floor().max(0.0) as usize;
        let py_hi = ((max_y + 1.0).ceil().max(0.0) as usize).min(h);
        // Scratch buffers reused across rows and sample lines.
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

    /// Resolve a `Paint` into a fill. Ill-formed gradient geometry (coincident linear
    /// points, coincident radial circles, an empty sweep) is `None`: the spec says the
    /// layer does not render. A non-invertible paint transform falls back to the first
    /// stop. Stops use default (non-variable) coords, matching the outline path.
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
        // An empty color line fills with the foreground rather than dropping the layer.
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
        // Snapshot `cur` now: the PaintGlyph clip lives in this space, and a transform
        // pushed in its subtree moves only the gradient geometry. Fluent's 🔥 depends on
        // it: warping the outline through its mirroring paint transforms samples the pad
        // ramps at their alpha-0 ends and all but erases the glyph. Nested PaintGlyph
        // clips are not intersected (the module's src-over simplification).
        self.pending = Some((glyph_id, self.cur));
    }
    fn paint(&mut self, paint: Paint<'a>) {
        match self.resolve_paint(&paint) {
            Some(src) => self.fill_pending(&src),
            // Ill-formed geometry (`resolve_paint`): the layer does not render; drop the
            // pending outline so painter state stays sane.
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
    fn push_clip_box(&mut self, _clipbox: aterm_font::colr::ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _mode: CompositeMode) {}
    fn pop_layer(&mut self) {}
}

/// Rasterize the COLR color glyph `gid` into an `w×h` RGBA8 buffer (fitted by
/// [`colr_fit`], y-flipped). Returns `None` if `gid` is not a COLR glyph or the
/// face has no COLR table. Pure + panic-free.
pub(crate) fn rasterize_colr(face: &Face, gid: GlyphId, w: usize, h: usize) -> Option<Vec<u8>> {
    rasterize_colr_run(face, &[(gid, 0.0, 0.0)], w, h)
}

/// A font-unit rectangle, y up.
#[derive(Clone, Copy, Debug, PartialEq)]
struct UnitBox {
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
}

impl UnitBox {
    #[inline]
    const fn width(self) -> f32 {
        self.x_max - self.x_min
    }
    #[inline]
    const fn height(self) -> f32 {
        self.y_max - self.y_min
    }
    #[inline]
    const fn shifted(self, x: f32, y: f32) -> UnitBox {
        UnitBox {
            x_min: self.x_min + x,
            y_min: self.y_min + y,
            x_max: self.x_max + x,
            y_max: self.y_max + y,
        }
    }
    #[inline]
    const fn union(self, o: UnitBox) -> UnitBox {
        UnitBox {
            x_min: self.x_min.min(o.x_min),
            y_min: self.y_min.min(o.y_min),
            x_max: self.x_max.max(o.x_max),
            y_max: self.y_max.max(o.y_max),
        }
    }
    #[inline]
    const fn is_proper(self) -> bool {
        self.width() > 0.0
            && self.height() > 0.0
            && self.width().is_finite()
            && self.height().is_finite()
    }
}

/// The glyph's LAYOUT box: its advance across, and down the face's line
/// widened to its global ink box, so no glyph of the face loses its top or
/// its descent. The fit of a face that declares no emoji frame
/// ([`emoji_frame`]).
fn layout_box(face: &Face, gid: GlyphId) -> Option<UnitBox> {
    let upem = f32::from(face.units_per_em());
    if upem <= 0.0 {
        return None;
    }
    let advance = match face.glyph_hor_advance(gid) {
        Some(advance) if advance > 0 => f32::from(advance),
        _ => upem,
    };
    let ink = face.global_bounding_box();
    let (mut top, mut bottom) = (
        f32::from(face.ascender().max(ink.y_max)),
        f32::from(face.descender().min(ink.y_min)),
    );
    if top <= bottom {
        (top, bottom) = (upem, 0.0);
    }
    Some(UnitBox {
        x_min: 0.0,
        y_min: bottom,
        x_max: advance,
        y_max: top,
    })
}

/// Where `gid` paints, at most: its COLRv1 clip box, else its outline's box,
/// else its layout box.
fn paint_box(face: &Face, gid: GlyphId) -> Option<UnitBox> {
    let clip = face
        .tables()
        .colr
        .and_then(|colr| colr.clip_box(gid, &[]))
        .map(|b| UnitBox {
            x_min: b.x_min,
            y_min: b.y_min,
            x_max: b.x_max,
            y_max: b.y_max,
        });
    let outline = || {
        face.glyph_bounding_box(gid).map(|b| UnitBox {
            x_min: f32::from(b.x_min),
            y_min: f32::from(b.y_min),
            x_max: f32::from(b.x_max),
            y_max: f32::from(b.y_max),
        })
    };
    clip.or_else(outline)
        .filter(|b| b.is_proper())
        .or_else(|| layout_box(face, gid))
}

/// The face's EMOJI FRAME: the COLRv1 clip box of its 😀 (U+1F600), the
/// square a full-size emoji of the face is drawn in, when the face declares
/// one. Segoe UI Emoji's is 2050 units square, (369, -287)..(2419, 1763) in
/// a 2812-unit advance, and every full-bleed emoji (😀 ✅ ❌ ⭐ 🚀) shares
/// it (measured on Windows 11 26200, 2026-10-17); a smaller emoji (⚡, 👍)
/// clips tighter inside it. `None` for a face without a ClipList, a COLRv0
/// face, or one with no 😀.
fn emoji_frame(face: &Face) -> Option<UnitBox> {
    let gid = face.glyph_index('\u{1F600}')?;
    let b = face.tables().colr?.clip_box(gid, &[])?;
    let frame = UnitBox {
        x_min: b.x_min,
        y_min: b.y_min,
        x_max: b.x_max,
        y_max: b.y_max,
    };
    frame.is_proper().then_some(frame)
}

/// The base transform that draws the font-unit run `glyphs` (each `(gid, x,
/// y)`: an origin, y up) into a `w × h` pixel box, y down.
///
/// ONE SCALE PER FACE, so the emoji of a line read as one size. A single glyph
/// fits the face's [`emoji_frame`] — or, for a face that declares none, its
/// [`layout_box`] — into the box with a pixel of air on every side, centred
/// both ways. Fitting the em square instead put Segoe UI Emoji's ✅ 382 units
/// right of centre and cut its side and its descent off; fitting the layout
/// box drew it whole but at 14 of the 18 px a two-cell slot has (both
/// measured on Windows 11, 2026-10-17). A glyph that paints past the frame
/// (❤'s clip box is 2419 units wide) shrinks just enough that its paint box
/// still fits the box, and is kept inside it.
///
/// A RUN of several glyphs (a ZWJ family Segoe UI Emoji draws as figures side
/// by side, the last members with no advance of their own) fits the union of
/// their paint boxes the same way.
fn colr_fit(face: &Face, glyphs: &[(GlyphId, f32, f32)], w: usize, h: usize) -> Option<Affine> {
    let mut content: Option<UnitBox> = None;
    for &(gid, x, y) in glyphs {
        let b = paint_box(face, gid)?.shifted(x, y);
        content = Some(content.map_or(b, |c| c.union(b)));
    }
    let content = content?;
    let frame = match glyphs {
        [(gid, x, y)] => emoji_frame(face)
            .or_else(|| layout_box(face, *gid))?
            .shifted(*x, *y),
        _ => content,
    };
    let (wf, hf) = (w as f32, h as f32);
    let air = if w >= 8 && h >= 8 { 1.0 } else { 0.0 };
    let scale = ((wf - 2.0 * air) / frame.width())
        .min((hf - 2.0 * air) / frame.height())
        .min(wf / content.width())
        .min(hf / content.height());
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    // Centre the frame, then move (never cut) the paint box back inside.
    let inside = |offset: f32, lo: f32, hi: f32, room: f32| {
        if lo + offset < 0.0 {
            -lo
        } else if hi + offset > room {
            room - hi
        } else {
            offset
        }
    };
    let e = wf / 2.0 - scale * (frame.x_min + frame.x_max) / 2.0;
    let e = inside(e, scale * content.x_min, scale * content.x_max, wf);
    // y flips: font `y` lands on pixel row `f - scale·y`.
    let f = hf / 2.0 + scale * (frame.y_min + frame.y_max) / 2.0;
    let f = inside(f, -scale * content.y_max, -scale * content.y_min, hf);
    Some(Affine {
        a: scale,
        b: 0.0,
        c: 0.0,
        d: -scale,
        e,
        f,
    })
}

/// Rasterize the COLR glyphs `glyphs` (each `(gid, x, y)`: its origin in font
/// units, y up — the shaper's pen plus offsets) into one `w × h` RGBA8 buffer,
/// fitted by [`colr_fit`]. `None` if any glyph is not a COLR glyph, or the
/// face has no COLR table. Pure + panic-free.
pub(crate) fn rasterize_colr_run(
    face: &Face,
    glyphs: &[(GlyphId, f32, f32)],
    w: usize,
    h: usize,
) -> Option<Vec<u8>> {
    if w == 0
        || h == 0
        || glyphs.is_empty()
        || glyphs.iter().any(|&(g, ..)| !face.is_color_glyph(g))
    {
        return None;
    }
    let base = colr_fit(face, glyphs, w, h)?;
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
    for &(gid, x, y) in glyphs {
        canvas.stack.clear();
        canvas.pending = None;
        canvas.cur = base.then(Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: x,
            f: y,
        });
        face.paint_color_glyph(gid, 0, fg, &mut canvas)?;
    }
    // The accumulator holds premultiplied rgb, but `GlyphImage::Rgba` blits as straight
    // alpha (like the PNG emoji path); un-premultiply so a translucent layer is not
    // darkened twice (a² instead of a).
    let mut buf = canvas.buf;
    unpremultiply_straight(&mut buf);
    Some(buf)
}

/// Convert a premultiplied RGBA8 accumulator to straight alpha in place. Opaque and
/// transparent pixels are already correct and are skipped, so opaque output is byte-exact.
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

    /// Segoe UI Emoji's bytes, or this host COULD NOT RUN the test.
    #[cfg(windows)]
    fn segoe_ui_emoji() -> Vec<u8> {
        const SEGOE: &str = r"C:\Windows\Fonts\seguiemj.ttf";
        std::fs::read(SEGOE).unwrap_or_else(|e| {
            aterm_spec::host::could_not_run(format!("{SEGOE} does not read here ({e})"))
        })
    }

    /// The `(columns, rows)` an RGBA raster inks (alpha >= 32), in order.
    #[cfg(windows)]
    fn inked_extent(rgba: &[u8], w: usize, h: usize) -> (Vec<usize>, Vec<usize>) {
        let inked = |x: usize, y: usize| rgba[(y * w + x) * 4 + 3] >= 32;
        let cols = (0..w).filter(|&x| (0..h).any(|y| inked(x, y))).collect();
        let rows = (0..h).filter(|&y| (0..w).any(|x| inked(x, y))).collect();
        (cols, rows)
    }

    /// WINDOWS' COLOUR EMOJI FIT THEIR BOX WHOLE, AND FILL IT. Segoe UI Emoji
    /// draws each glyph centered in an advance wider than its em, with
    /// descents below the line, so the emoji of an agent's reply (✅ ❌ ⭐ ⚡ ⏳
    /// 😀 🚀 👍, and ⚠ ❤ asked for with VS16), fitted into the Windows
    /// default's two-cell box (18 x 23 px), keep a clear texel on every side —
    /// nothing cut off — sit centered across it within a pixel, and the
    /// full-bleed ones span at least 15 of its 18 columns (the face's emoji
    /// frame less a pixel of air each side; the layout-box fit drew them 14
    /// wide). The faces of one frame share one bottom row.
    #[test]
    #[cfg(windows)]
    fn segoe_ui_emoji_glyphs_draw_whole_and_centred_in_their_box() {
        let bytes = segoe_ui_emoji();
        let face = Face::parse(&bytes, 0).expect("Segoe UI Emoji parses");
        let (w, h) = (18usize, 23usize);
        let mut bottoms = Vec::new();
        for ch in ['✅', '❌', '⭐', '⚡', '⏳', '😀', '🚀', '👍', '⚠', '❤'] {
            let gid = face.glyph_index(ch).expect("Segoe UI Emoji maps it");
            let rgba = rasterize_colr(&face, gid, w, h).expect("a COLR glyph");
            let (cols, rows) = inked_extent(&rgba, w, h);
            let (left, right) = (cols[0], w - 1 - cols[cols.len() - 1]);
            let (top, bottom) = (rows[0], h - 1 - rows[rows.len() - 1]);
            assert!(
                left > 0 && right > 0 && top > 0 && bottom > 0,
                "{ch}: ink reaches the box's edge (margins l{left} r{right} t{top} b{bottom})"
            );
            assert!(
                left.abs_diff(right) <= 1,
                "{ch}: not centered across the box (margins l{left} r{right})"
            );
            if matches!(ch, '✅' | '❌' | '😀') {
                assert!(
                    cols.len() >= 15,
                    "{ch}: drawn {} columns wide in an 18-column box",
                    cols.len()
                );
            }
            if !matches!(ch, '⚠' | '❤') {
                bottoms.push(bottom);
            }
        }
        let (lo, hi) = (bottoms.iter().min(), bottoms.iter().max());
        assert!(
            hi.zip(lo).is_some_and(|(hi, lo)| hi - lo <= 1),
            "the emoji sit on different baselines: {bottoms:?}"
        );
    }

    /// A ZWJ FAMILY IS ITS MEMBERS SIDE BY SIDE. Segoe UI Emoji shapes 👨‍👩‍👧
    /// to three colour glyphs, the last with no advance of its own (it stands
    /// after the woman by its outline alone). Painted at the shaper's pen
    /// positions in one canvas, the run fits the two-cell box whole: ink in
    /// each third of it, a clear texel on every side.
    #[test]
    #[cfg(windows)]
    fn a_segoe_family_paints_every_member_inside_its_box() {
        let bytes = segoe_ui_emoji();
        let face = Face::parse(&bytes, 0).expect("Segoe UI Emoji parses");
        let shape_face = aterm_font::shape::ShapeFace::from_slice(&bytes, 0).expect("shapes");
        let mut buffer = aterm_font::shape::UnicodeBuffer::new();
        buffer.push_str("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}");
        let shaped = aterm_font::shape::shape(&shape_face, &[], buffer);
        let mut pen = 0i32;
        let mut run = Vec::new();
        for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let gid = GlyphId(u16::try_from(info.glyph_id).expect("a u16 glyph id"));
            if face.is_color_glyph(gid) {
                run.push((gid, (pen + pos.x_offset) as f32, pos.y_offset as f32));
            }
            pen += pos.x_advance;
        }
        assert_eq!(run.len(), 3, "three members: {run:?}");
        let (w, h) = (18usize, 23usize);
        let rgba = rasterize_colr_run(&face, &run, w, h).expect("a COLR run");
        let (cols, rows) = inked_extent(&rgba, w, h);
        assert!(
            cols[0] > 0
                && cols[cols.len() - 1] < w - 1
                && rows[0] > 0
                && rows[rows.len() - 1] < h - 1,
            "the family reaches the box's edge: columns {cols:?}, rows {rows:?}"
        );
        for third in 0..3 {
            assert!(
                cols.iter().any(|&x| x * 3 / w == third),
                "no member inks third {third} of the box: columns {cols:?}"
            );
        }
    }

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

    /// `tests/fixtures/colr_1.ttf`: the googlefonts/color-fonts COLRv1 test font
    /// (Apache-2.0) that ttf-parser 0.25.1 ships in its `tests/fonts`.
    fn colr_test_font() -> &'static [u8] {
        include_bytes!("../tests/fixtures/colr_1.ttf")
    }

    #[test]
    fn colr_glyph_rasterizes_straight_alpha() {
        let bytes = colr_test_font();
        let face = Face::parse(bytes, 0).expect("valid font");
        // Every single-solid-layer glyph returns its palette rgb at fully-covered pixels,
        // with the layer's alpha in the alpha channel. The exact un-premultiply arithmetic
        // is `unpremultiply_restores_straight_rgb` (this font's translucent solid is black).
        // The font's first colour glyph is a gradient, so solids are found by probing.
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
        // Premultiplied half-transparent orange (straight 200,100,40 at α=128), an opaque
        // pixel and a transparent one.
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

    /// A fixture gradient stop: offset plus raw RGBA (named to keep clippy's complex-type
    /// lint quiet).
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

    /// Counts multi-colour gradient layers vs solid layers (and the last solid colour)
    /// without rasterizing.
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
        fn push_clip_box(&mut self, _: aterm_font::colr::ClipBox) {}
        fn pop_clip(&mut self) {}
        fn push_layer(&mut self, _: CompositeMode) {}
        fn pop_layer(&mut self) {}
    }

    /// The most distinct colours in any horizontal or vertical run of constant alpha
    /// (α ≥ 128, ≥ 8 px). Compositing and coverage artefacts scale rgb per alpha, so a flat
    /// fill stays one colour (rarely 2-3) within such a run while a gradient sweeps many.
    /// α ≥ 128, not 255: Fluent glyphs such as 🔥 use only translucent stops.
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

    /// A pure-gradient glyph sweeps many colours, not one per layer (a gradient flattened to
    /// its first stop renders flat).
    #[test]
    fn colr_gradient_renders_a_colour_spread() {
        let bytes = colr_test_font();
        let face = Face::parse(bytes, 0).expect("valid font");
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
            // Pure-gradient glyphs with few layers only.
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
}

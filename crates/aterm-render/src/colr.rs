// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! COLR (vector color glyph) rasterization.

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

fn collect_stops(iter: impl Iterator<Item = ColorStop>) -> Vec<(f32, RgbaColor)> {
    let mut v: Vec<(f32, RgbaColor)> = iter.map(|s| (s.stop_offset, s.color)).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

/// Fold the COLRv1 linear-gradient `p2` rotation into a projection vector `q`
/// with `t(p) = (p − p0)·q / (q·q)`: lines of constant color run parallel to
/// p0→p2, and `t(p1) = 1`.
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

/// The two-circle radial gradient from circle `(x0, y0, r0)` to `(x1, y1, r1)`, or `None` for
/// one that cannot paint: a parameter or delta that is not finite (a font's variation deltas can
/// make one), or two equal circles.
fn radial_kind(
    (x0, y0, r0): (f32, f32, f32),
    (x1, y1, r1): (f32, f32, f32),
) -> Option<GradientKind> {
    let (dx, dy, dr) = (x1 - x0, y1 - y0, r1 - r0);
    if ![x0, y0, r0, dx, dy, dr].into_iter().all(f32::is_finite)
        || (dx == 0.0 && dy == 0.0 && dr == 0.0)
    {
        return None;
    }
    Some(GradientKind::Radial {
        x0,
        y0,
        r0,
        dx,
        dy,
        dr,
    })
}

/// The sweep gradient around `(cx, cy)` from `start_angle` to `end_angle` (half-turns, as COLR
/// stores them), or `None` for one that cannot paint: a centre or angle that is not finite, or
/// no sweep.
fn sweep_kind((cx, cy): (f32, f32), start_angle: f32, end_angle: f32) -> Option<GradientKind> {
    let start_deg = start_angle * 180.0 + 180.0;
    let sweep_deg = (end_angle - start_angle) * 180.0;
    if ![cx, cy, start_deg, sweep_deg]
        .into_iter()
        .all(f32::is_finite)
        || sweep_deg == 0.0
    {
        return None;
    }
    Some(GradientKind::Sweep {
        cx,
        cy,
        start_deg,
        sweep_deg,
    })
}

struct ColorRamp {
    extend: GradientExtend,
    /// (offset, straight-alpha color), ascending by offset, never empty.
    stops: Vec<(f32, RgbaColor)>,
}

impl ColorRamp {
    fn sample(&self, t: f32) -> RgbaColor {
        let (Some(&first), Some(&last)) = (self.stops.first(), self.stops.last()) else {
            return RgbaColor::new(0, 0, 0, 0);
        };
        if t.is_nan() {
            return first.1;
        }
        let (lo, hi) = (first.0, last.0);
        let span = hi - lo;
        if !span.is_finite() || span <= 0.0 {
            return last.1;
        }
        if !t.is_finite() {
            return match self.extend {
                GradientExtend::Pad if t.is_sign_positive() => last.1,
                _ => first.1,
            };
        }
        let t = match self.extend {
            GradientExtend::Pad => t.clamp(lo, hi),
            GradientExtend::Repeat => lo + (t - lo).rem_euclid(span),
            GradientExtend::Reflect => {
                let m = ((t - lo) / span).rem_euclid(2.0);
                lo + span * if m > 1.0 { 2.0 - m } else { m }
            }
        };
        if !t.is_finite() || t <= first.0 {
            return first.1;
        }
        if t >= last.0 {
            return last.1;
        }
        // The last stop below `t`, found by bisection: each pixel samples the
        // ramp, and a ramp may hold thousands of stops.
        let i = self.stops[1..].partition_point(|s| {
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_ramp_stop_read();
            s.0 < t
        });
        let (o0, c0) = self.stops[i];
        let Some(&(o1, c1)) = self.stops.get(i + 1) else {
            return last.1;
        };
        let d = o1 - o0;
        if !d.is_finite() || d <= 0.0 {
            return c1;
        }
        lerp_premul(c0, c1, (t - o0) / d)
    }
}

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

struct GradientFill {
    kind: GradientKind,
    ramp: ColorRamp,
    inv: Affine,
}

impl GradientFill {
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

enum FillSource {
    Solid(RgbaColor),
    Gradient(GradientFill),
}

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
    /// `outline_glyph` ran.
    pending: Option<(GlyphId, Affine)>,
}

impl ColrCanvas<'_> {
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
        let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
        let (mut min_y, mut max_y) = (f32::INFINITY, f32::NEG_INFINITY);
        for contour in &contours {
            let n = contour.len();
            for i in 0..n {
                let (x0, y0) = contour[i];
                let (x1, y1) = contour[(i + 1) % n];
                if !x0.is_finite()
                    || !y0.is_finite()
                    || !x1.is_finite()
                    || !y1.is_finite()
                    || y0 == y1
                {
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
        let py_lo = (min_y - 1.0).floor().max(0.0) as usize;
        let py_hi = ((max_y + 1.0).ceil().max(0.0) as usize).min(h);
        let mut cov = vec![0.0f32; w];
        let mut xs: Vec<(f32, i32)> = Vec::new();
        for py in py_lo..py_hi {
            cov.fill(0.0);
            for s in 0..SS {
                let sy = py as f32 + (s as f32 + 0.5) / SS as f32;
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
                                None => continue,
                            }
                        }
                    };
                    self.blend(px, py, color, c.min(1.0));
                }
            }
        }
    }

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
                let kind = radial_kind((g.x0, g.y0, g.r0), (g.x1, g.y1, g.r1))?;
                (g.extend, collect_stops(g.stops(self.palette, &[])), kind)
            }
            Paint::SweepGradient(g) => {
                let kind = sweep_kind((g.center_x, g.center_y), g.start_angle, g.end_angle)?;
                (g.extend, collect_stops(g.stops(self.palette, &[])), kind)
            }
        };
        let Some(&(_, first)) = stops.first() else {
            return Some(FillSource::Solid(self.foreground));
        };
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
        self.pending = Some((glyph_id, self.cur));
    }
    fn paint(&mut self, paint: Paint<'a>) {
        if self.pending.is_none() {
            return;
        }
        match self.resolve_paint(&paint) {
            Some(src) => self.fill_pending(&src),
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
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, _clipbox: aterm_font::colr::ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _mode: CompositeMode) {}
    fn pop_layer(&mut self) {}
}

/// Rasterize the COLR color glyph `gid` into an `w×h` RGBA8 buffer (fitted by [`colr_fit`],
/// y-flipped).
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

/// Rasterizes the positioned COLR `glyphs` run into a `w × h` RGBA8 buffer.
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
    let buf_len = w.checked_mul(h)?.checked_mul(4)?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(buf_len).ok()?;
    buf.resize(buf_len, 0);
    let base = colr_fit(face, glyphs, w, h)?;
    let fg = RgbaColor::new(0, 0, 0, 255);
    let mut canvas = ColrCanvas {
        face,
        w,
        h,
        buf,
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
    let mut buf = canvas.buf;
    unpremultiply_straight(&mut buf);
    Some(buf)
}

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

fn add_span(cov: &mut [f32], x0: f32, x1: f32, amount: f32) {
    if x0.is_nan() || x1.is_nan() {
        return;
    }
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
#[path = "colr_tests.rs"]
mod tests;

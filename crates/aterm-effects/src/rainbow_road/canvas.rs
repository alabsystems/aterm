// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The road's and the cat's drawing kit: shapes in design units (lines, cubics, quadratics and
//! ellipses), an affine stack onto a device [`Tile`], fills through [`fill_path`] (4×4
//! supersampled, union across paths), strokes as coverage of the distance to their centreline
//! (round caps and joins, as the design's SVG strokes), butt-ended bands, and a separable box
//! blur (three passes, about a gaussian) for glows and shadows, each drawn on a layer of the
//! canvas or, for a disc's glow, a layer cropped to the disc and the blur's reach.

use aterm_scene::{PathCmd, PathTransform, Tile, fill_path};

/// A colour, each channel `0.0..=1.0`.
pub(super) type Rgb = (f32, f32, f32);

/// `0xRRGGBB` as an [`Rgb`].
pub(super) fn hex(v: u32) -> Rgb {
    let channel = |shift: u32| ((v >> shift) & 0xff) as f32 / 255.0;
    (channel(16), channel(8), channel(0))
}

/// An SVG-style affine map: `(x, y) → (a·x + c·y + e, b·x + d·y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Affine {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Affine {
    pub(super) const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub(super) const fn translate(x: f32, y: f32) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }

    pub(super) const fn scale(s: f32) -> Self {
        Self {
            a: s,
            d: s,
            ..Self::IDENTITY
        }
    }

    /// SVG's `rotate(deg)`: clockwise on a y-down screen.
    pub(super) fn rotate(deg: f32) -> Self {
        let (sin, cos) = deg.to_radians().sin_cos();
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            e: 0.0,
            f: 0.0,
        }
    }

    /// SVG's `rotate(deg cx cy)`.
    pub(super) fn rotate_about(deg: f32, cx: f32, cy: f32) -> Self {
        Self::translate(cx, cy)
            .then(Self::rotate(deg))
            .then(Self::translate(-cx, -cy))
    }

    /// `self` after `inner`: a point goes through `inner` first.
    pub(super) fn then(self, inner: Self) -> Self {
        Self {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    pub(super) fn apply(self, (x, y): (f32, f32)) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// How much the map scales a length (its determinant's square root).
    pub(super) fn length_scale(self) -> f32 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }
}

/// A path in design units, built as the design's SVG writes it.
#[derive(Clone, Debug, Default)]
pub(super) struct Shape {
    cmds: Vec<PathCmd>,
    at: (f32, f32),
}

/// The cubic control length of a circle's quarter, per unit of radius.
const KAPPA: f32 = 0.552_284_8;

impl Shape {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn m(mut self, x: f32, y: f32) -> Self {
        self.cmds.push(PathCmd::Move(x, y));
        self.at = (x, y);
        self
    }

    pub(super) fn l(mut self, x: f32, y: f32) -> Self {
        self.cmds.push(PathCmd::Line(x, y));
        self.at = (x, y);
        self
    }

    /// SVG's relative `l`.
    pub(super) fn rl(self, dx: f32, dy: f32) -> Self {
        let (x, y) = self.at;
        self.l(x + dx, y + dy)
    }

    pub(super) fn c(mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) -> Self {
        self.cmds.push(PathCmd::Cubic(x1, y1, x2, y2, x, y));
        self.at = (x, y);
        self
    }

    /// A quadratic, as the cubic it is.
    pub(super) fn q(self, qx: f32, qy: f32, x: f32, y: f32) -> Self {
        let (x0, y0) = self.at;
        let k = 2.0 / 3.0;
        self.c(
            x0 + k * (qx - x0),
            y0 + k * (qy - y0),
            x + k * (qx - x),
            y + k * (qy - y),
            x,
            y,
        )
    }

    /// SVG's relative `q`.
    pub(super) fn rq(self, dqx: f32, dqy: f32, dx: f32, dy: f32) -> Self {
        let (x, y) = self.at;
        self.q(x + dqx, y + dqy, x + dx, y + dy)
    }

    pub(super) fn z(mut self) -> Self {
        self.cmds.push(PathCmd::Close);
        self
    }

    /// An ellipse as four cubics, starting at its left end as the design's arcs do.
    pub(super) fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Self {
        let (kx, ky) = (rx * KAPPA, ry * KAPPA);
        Self::new()
            .m(cx - rx, cy)
            .c(cx - rx, cy + ky, cx - kx, cy + ry, cx, cy + ry)
            .c(cx + kx, cy + ry, cx + rx, cy + ky, cx + rx, cy)
            .c(cx + rx, cy - ky, cx + kx, cy - ry, cx, cy - ry)
            .c(cx - kx, cy - ry, cx - rx, cy - ky, cx - rx, cy)
            .z()
    }

    /// A closed polygon through `points`.
    pub(super) fn polygon(points: &[(f32, f32)]) -> Self {
        let mut shape = Self::new();
        for (at, &(x, y)) in points.iter().enumerate() {
            shape = if at == 0 {
                shape.m(x, y)
            } else {
                shape.l(x, y)
            };
        }
        shape.z()
    }

    /// An open polyline through `points`.
    pub(super) fn polyline(points: &[(f32, f32)]) -> Self {
        let mut shape = Self::new();
        for (at, &(x, y)) in points.iter().enumerate() {
            shape = if at == 0 {
                shape.m(x, y)
            } else {
                shape.l(x, y)
            };
        }
        shape
    }

    /// The four-point star the design draws with quadratics, centred on `(x, y)`, `r` to a tip.
    pub(super) fn star(x: f32, y: f32, r: f32) -> Self {
        Self::new()
            .m(x, y - r)
            .q(x, y, x + r, y)
            .q(x, y, x, y + r)
            .q(x, y, x - r, y)
            .q(x, y, x, y - r)
            .z()
    }

    /// The shape through `map`.
    pub(super) fn mapped(&self, map: Affine) -> Self {
        let p = |x: f32, y: f32| map.apply((x, y));
        let cmds = self
            .cmds
            .iter()
            .map(|cmd| match *cmd {
                PathCmd::Move(x, y) => {
                    let (x, y) = p(x, y);
                    PathCmd::Move(x, y)
                }
                PathCmd::Line(x, y) => {
                    let (x, y) = p(x, y);
                    PathCmd::Line(x, y)
                }
                PathCmd::Cubic(x1, y1, x2, y2, x, y) => {
                    let ((x1, y1), (x2, y2), (x, y)) = (p(x1, y1), p(x2, y2), p(x, y));
                    PathCmd::Cubic(x1, y1, x2, y2, x, y)
                }
                PathCmd::Close => PathCmd::Close,
            })
            .collect();
        Self {
            cmds,
            at: p(self.at.0, self.at.1),
        }
    }

    /// The shape's subpaths as polylines, each cubic cut into `per_cubic(control length)`
    /// pieces; a closed subpath ends where it began.
    pub(super) fn flatten(&self, per_cubic: impl Fn(f32) -> usize) -> Vec<Vec<(f32, f32)>> {
        let mut lines: Vec<Vec<(f32, f32)>> = Vec::new();
        let mut start = (0.0, 0.0);
        let mut at = (0.0, 0.0);
        for cmd in &self.cmds {
            match *cmd {
                PathCmd::Move(x, y) => {
                    lines.push(vec![(x, y)]);
                    start = (x, y);
                    at = (x, y);
                }
                PathCmd::Line(x, y) => {
                    current(&mut lines, at).push((x, y));
                    at = (x, y);
                }
                PathCmd::Cubic(x1, y1, x2, y2, x, y) => {
                    let control =
                        dist(at, (x1, y1)) + dist((x1, y1), (x2, y2)) + dist((x2, y2), (x, y));
                    let n = per_cubic(control).max(1);
                    let p0 = at;
                    let line = current(&mut lines, at);
                    for k in 1..=n {
                        let u = k as f32 / n as f32;
                        let v = 1.0 - u;
                        let (a, b, c, d) = (v * v * v, 3.0 * v * v * u, 3.0 * v * u * u, u * u * u);
                        line.push((
                            a * p0.0 + b * x1 + c * x2 + d * x,
                            a * p0.1 + b * y1 + c * y2 + d * y,
                        ));
                    }
                    at = (x, y);
                }
                PathCmd::Close => {
                    if at != start {
                        current(&mut lines, at).push(start);
                    }
                    at = start;
                }
            }
        }
        lines
    }
}

/// The polyline being drawn, begun at `at` where no move began one.
fn current(lines: &mut Vec<Vec<(f32, f32)>>, at: (f32, f32)) -> &mut Vec<(f32, f32)> {
    if lines.is_empty() {
        lines.push(vec![at]);
    }
    let last = lines.len() - 1;
    &mut lines[last]
}

pub(super) fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// How far, in design units, a glow fades out before each edge of its canvas; 0 leaves an edge
/// as it is.
#[derive(Clone, Copy, Default)]
pub(super) struct Fade {
    pub(super) left: f32,
    pub(super) right: f32,
    pub(super) bottom: f32,
}

/// A device tile drawn in design units through an affine stack.
pub(super) struct Canvas {
    pub(super) tile: Tile,
    /// Design units to device pixels.
    pub(super) map: Affine,
    /// Where the tile's top-left pixel stands, in device pixels of the canvas it is a layer of:
    /// `(0, 0)` for a canvas of its own and a whole layer, the crop's corner for the layer
    /// [`Self::dot_glow`] cuts to its disc. Only a dot is drawn on a cropped layer: its centre is
    /// mapped as on that canvas, then moved back by this whole number of pixels, which in `f32`
    /// is exact (the corner is 0 or lies at or before the centre, so the difference is a multiple
    /// of the centre's last place no larger than it). Each subsample's offset from the centre is
    /// then the difference of the same two exact values on either tile, rounded alike, so the
    /// disc covers the very pixels it covers there. A path is never drawn on one ([`Self::fill`]
    /// and [`Self::stroke`] assert it): its curves are flattened from moved points, which round
    /// otherwise and can put a subsample across an edge, a level off in an edge pixel.
    origin: (i32, i32),
}

impl Canvas {
    pub(super) fn new(w: u32, h: u32, map: Affine) -> Self {
        Self {
            tile: Tile::new(w, h),
            map,
            origin: (0, 0),
        }
    }

    /// An empty canvas of the same size and map: a layer to blur or composite.
    pub(super) fn layer(&self) -> Self {
        Self::new(self.tile.width(), self.tile.height(), self.map)
    }

    /// An empty layer over just the part of this canvas a glow of a disc needs: the device box
    /// of the design box `(x0, y0)..(x1, y1)` the disc lies in, grown by a blur of `sigma`
    /// design units' whole reach (and a few pixels for edge coverage), clipped to the canvas.
    /// Private to [`Self::dot_glow`], so nothing but a dot is drawn on it.
    fn layer_around(&self, (x0, y0, x1, y1): (f32, f32, f32, f32), sigma: f32) -> Self {
        let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|p| self.map.apply(p));
        let (mut lo, mut hi) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
        for (x, y) in corners {
            lo = (lo.0.min(x), lo.1.min(y));
            hi = (hi.0.max(x), hi.1.max(y));
        }
        let grow = (blur_reach(sigma * self.unit()) + 3) as f32;
        let (cw, ch) = (self.tile.width() as i32, self.tile.height() as i32);
        let left = ((lo.0 - grow).floor() as i32).clamp(0, cw);
        let top = ((lo.1 - grow).floor() as i32).clamp(0, ch);
        let right = ((hi.0 + grow).ceil() as i32).clamp(left, cw);
        let bottom = ((hi.1 + grow).ceil() as i32).clamp(top, ch);
        Self {
            tile: Tile::new((right - left) as u32, (bottom - top) as u32),
            map: self.map,
            origin: (self.origin.0 + left, self.origin.1 + top),
        }
    }

    /// Draw with `inner` applied before the current map.
    pub(super) fn with<T>(&mut self, inner: Affine, draw: impl FnOnce(&mut Self) -> T) -> T {
        let outer = self.map;
        self.map = outer.then(inner);
        let out = draw(self);
        self.map = outer;
        out
    }

    /// Design units per device pixel's reciprocal: device pixels per design unit.
    pub(super) fn unit(&self) -> f32 {
        self.map.length_scale()
    }

    pub(super) fn fill(&mut self, shape: &Shape, rgb: Rgb, alpha: f32) {
        debug_assert_eq!(self.origin, (0, 0), "a path on a cropped layer");
        let device = shape.mapped(self.map);
        fill_path(
            &mut self.tile,
            &[&device.cmds],
            rgb,
            alpha,
            PathTransform {
                scale_x: 1.0,
                scale_y: 1.0,
                dx: 0.0,
                dy: 0.0,
            },
        );
    }

    /// Stroke `shape`'s centreline `width` design units wide, with round caps and joins.
    pub(super) fn stroke(&mut self, shape: &Shape, width: f32, rgb: Rgb, alpha: f32) {
        debug_assert_eq!(self.origin, (0, 0), "a path on a cropped layer");
        let device = shape.mapped(self.map);
        let lines = device.flatten(|control| ((control / 2.5).ceil() as usize).clamp(4, 64));
        let width = width * self.unit();
        stroke_lines(&mut self.tile, &lines, width, rgb, alpha);
    }

    /// A band `width` design units wide along the polyline `points`, cut square at both ends.
    pub(super) fn band(&mut self, points: &[(f32, f32)], width: f32, rgb: Rgb, alpha: f32) {
        if points.len() < 2 {
            return;
        }
        let half = width / 2.0;
        let normal_at = |at: usize| {
            let (a, b) = (
                points[at.saturating_sub(1)],
                points[(at + 1).min(points.len() - 1)],
            );
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = dx.hypot(dy).max(1e-6);
            (-dy / len, dx / len)
        };
        let mut outline: Vec<(f32, f32)> = (0..points.len())
            .map(|at| {
                let (nx, ny) = normal_at(at);
                (points[at].0 + nx * half, points[at].1 + ny * half)
            })
            .collect();
        outline.extend((0..points.len()).rev().map(|at| {
            let (nx, ny) = normal_at(at);
            (points[at].0 - nx * half, points[at].1 - ny * half)
        }));
        self.fill(&Shape::polygon(&outline), rgb, alpha);
    }

    /// A disc of radius `r` design units at `(x, y)`.
    pub(super) fn dot(&mut self, x: f32, y: f32, r: f32, rgb: Rgb, alpha: f32) {
        let (dx, dy) = self.map.apply((x, y));
        let (dx, dy) = (dx - self.origin.0 as f32, dy - self.origin.1 as f32);
        let r = r * self.unit();
        self.tile.disc(dx, dy, r, rgb, alpha);
    }

    /// A disc of radius `r` design units at `(x, y)` in `rgb`, blurred by a gaussian of about
    /// `sigma` design units and drawn over this canvas at `alpha`: the very pixels the disc
    /// blurred on a whole layer gives, from a tile and a blur the size of the disc and the blur's
    /// reach rather than of the canvas.
    pub(super) fn dot_glow(
        &mut self,
        (x, y): (f32, f32),
        r: f32,
        rgb: Rgb,
        sigma: f32,
        alpha: f32,
    ) {
        let mut layer = self.layer_around((x - r, y - r, x + r, y + r), sigma);
        layer.dot(x, y, r, rgb, 1.0);
        self.glow(&layer, sigma, alpha, (0.0, 0.0));
    }

    /// `layer` (a layer of this canvas) blurred by a gaussian of about `sigma` design units,
    /// drawn over this canvas where it stands at `alpha`, moved `(dx, dy)` design units (rounded
    /// to whole device pixels).
    pub(super) fn glow(&mut self, layer: &Self, sigma: f32, alpha: f32, offset: (f32, f32)) {
        self.glow_fading(layer, sigma, alpha, offset, Fade::default());
    }

    /// [`Self::glow`], the blur faded out over `fade`'s design units to the canvas's left, right
    /// and bottom edges, so where it reaches them it ends softly, never cut.
    pub(super) fn glow_fading(
        &mut self,
        layer: &Self,
        sigma: f32,
        alpha: f32,
        (dx, dy): (f32, f32),
        fade: Fade,
    ) {
        let unit = self.unit();
        let mut blurred = blur(&layer.tile, sigma * unit);
        let lw = blurred.width() as usize;
        // Where the layer's top-left pixel stands on this canvas, whose edges the fade is from.
        let (ox, oy) = (
            (layer.origin.0 - self.origin.0) as f32,
            (layer.origin.1 - self.origin.1) as f32,
        );
        let (w, h) = (self.tile.width() as f32, self.tile.height() as f32);
        let (left, right, bottom) = (fade.left * unit, fade.right * unit, fade.bottom * unit);
        // Moved by `dy`, the blur's row `y` lands `dy` pixels lower.
        let shift = dy * unit;
        for (at, px) in blurred
            .pixels_mut()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            if px[3] == 0 {
                continue;
            }
            let (x, y) = (
                ox + (at % lw) as f32 + 0.5,
                oy + (at / lw) as f32 + 0.5 + shift,
            );
            let mut keep = 1.0;
            if left > 0.0 {
                keep *= super::smooth(x / left);
            }
            if right > 0.0 {
                keep *= super::smooth((w - x) / right);
            }
            if bottom > 0.0 {
                keep *= super::smooth((h - y) / bottom);
            }
            px[3] = (f32::from(px[3]) * keep).round() as u8;
        }
        composite(
            &mut self.tile,
            &blurred,
            alpha,
            (
                layer.origin.0 - self.origin.0 + (dx * unit).round() as i32,
                layer.origin.1 - self.origin.1 + (dy * unit).round() as i32,
            ),
        );
    }

    /// `layer` (a layer of this canvas) drawn over this canvas as it is, where it stands.
    pub(super) fn draw(&mut self, layer: &Self) {
        composite(
            &mut self.tile,
            &layer.tile,
            1.0,
            (
                layer.origin.0 - self.origin.0,
                layer.origin.1 - self.origin.1,
            ),
        );
    }
}

/// Stroke polylines (device pixels) `width` wide: each pixel's coverage is its centre's
/// distance inside the stroke's edge, a one-pixel ramp; a stroke under a pixel wide is drawn a
/// pixel wide and that much fainter, so its ink stays its width.
fn stroke_lines(tile: &mut Tile, lines: &[Vec<(f32, f32)>], width: f32, rgb: Rgb, alpha: f32) {
    if width.is_nan() || width <= 0.0 || alpha <= 0.0 {
        return;
    }
    let (half, alpha) = if width < 1.0 {
        (0.5, alpha * width)
    } else {
        (width / 2.0, alpha)
    };
    let reach = half + 1.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in lines.iter().flatten() {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let (tw, th) = (tile.width() as i32, tile.height() as i32);
    let bx0 = ((x0 - reach).floor() as i32).clamp(0, tw);
    let by0 = ((y0 - reach).floor() as i32).clamp(0, th);
    let bx1 = ((x1 + reach).ceil() as i32 + 1).clamp(0, tw);
    let by1 = ((y1 + reach).ceil() as i32 + 1).clamp(0, th);
    if bx0 >= bx1 || by0 >= by1 {
        return;
    }
    let bw = (bx1 - bx0) as usize;
    let mut cover = vec![0.0_f32; bw * (by1 - by0) as usize];
    let mut seg = |a: (f32, f32), b: (f32, f32)| {
        let sx0 = ((a.0.min(b.0) - reach).floor() as i32).max(bx0);
        let sy0 = ((a.1.min(b.1) - reach).floor() as i32).max(by0);
        let sx1 = ((a.0.max(b.0) + reach).ceil() as i32 + 1).min(bx1);
        let sy1 = ((a.1.max(b.1) + reach).ceil() as i32 + 1).min(by1);
        let (vx, vy) = (b.0 - a.0, b.1 - a.1);
        let len2 = vx * vx + vy * vy;
        for py in sy0..sy1 {
            for px in sx0..sx1 {
                let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
                let t = if len2 > 0.0 {
                    (((cx - a.0) * vx + (cy - a.1) * vy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let d = (cx - a.0 - t * vx).hypot(cy - a.1 - t * vy);
                let c = (half + 0.5 - d).clamp(0.0, 1.0);
                if c > 0.0 {
                    let at = (py - by0) as usize * bw + (px - bx0) as usize;
                    if c > cover[at] {
                        cover[at] = c;
                    }
                }
            }
        }
    };
    for line in lines {
        match line.as_slice() {
            [] => {}
            [one] => seg(*one, *one),
            many => {
                for pair in many.windows(2) {
                    seg(pair[0], pair[1]);
                }
            }
        }
    }
    for (row, line) in cover.chunks_exact(bw).enumerate() {
        for (col, &c) in line.iter().enumerate() {
            if c > 0.0 {
                tile.over(bx0 + col as i32, by0 + row as i32, rgb, alpha * c);
            }
        }
    }
}

/// `src` over `dst` at `alpha`, moved `(dx, dy)` pixels.
fn composite(dst: &mut Tile, src: &Tile, alpha: f32, (dx, dy): (i32, i32)) {
    let w = src.width() as usize;
    for (row, line) in src.pixels().chunks_exact(w * 4).enumerate() {
        for (col, px) in line.as_chunks::<4>().0.iter().enumerate() {
            if px[3] == 0 {
                continue;
            }
            dst.over(
                col as i32 + dx,
                row as i32 + dy,
                (
                    f32::from(px[0]) / 255.0,
                    f32::from(px[1]) / 255.0,
                    f32::from(px[2]) / 255.0,
                ),
                alpha * f32::from(px[3]) / 255.0,
            );
        }
    }
}

/// An sRGB-encoded channel in linear light.
fn to_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// A linear-light channel sRGB-encoded.
fn to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// `src` blurred by three box passes each way, about a gaussian of `sigma` pixels, in
/// premultiplied linear light, as SVG's `feGaussianBlur` works (its `color-interpolation-filters`
/// is `linearRGB`): the colours a blur mixes are mixed as light, then encoded again. Pixels past
/// the edge are transparent. Only the drawn pixels' box and the blur's reach around it are
/// worked.
fn blur(src: &Tile, sigma: f32) -> Tile {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let mut out = Tile::new(src.width(), src.height());
    let r = blur_radius(sigma);
    let px = src.pixels();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (at, p) in px.as_chunks::<4>().0.iter().enumerate() {
        if p[3] != 0 {
            let (x, y) = (at % w, at / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    if x0 >= x1 || y0 >= y1 {
        return out;
    }
    let reach = blur_reach(sigma);
    let (x0, y0) = (x0.saturating_sub(reach), y0.saturating_sub(reach));
    let (x1, y1) = ((x1 + reach).min(w), (y1 + reach).min(h));
    let (bw, bh) = (x1 - x0, y1 - y0);
    let mut buf: Vec<[f32; 4]> = Vec::with_capacity(bw * bh);
    for y in y0..y1 {
        for p in px[(y * w + x0) * 4..(y * w + x1) * 4].as_chunks::<4>().0 {
            let a = f32::from(p[3]) / 255.0;
            let light = |v: u8| to_linear(f32::from(v) / 255.0) * a;
            buf.push([light(p[0]), light(p[1]), light(p[2]), a]);
        }
    }
    let mut line: Vec<[f32; 4]> = Vec::new();
    for _ in 0..3 {
        for y in 0..bh {
            box_pass(&mut buf, y * bw, 1, bw, r, &mut line);
        }
    }
    for _ in 0..3 {
        for x in 0..bw {
            box_pass(&mut buf, x, bw, bh, r, &mut line);
        }
    }
    let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let encode = |v: f32| to(to_srgb(v.clamp(0.0, 1.0)));
    let dst = out.pixels_mut();
    for (at, p) in buf.iter().enumerate() {
        let a = p[3];
        if a <= 1.0 / 512.0 {
            continue;
        }
        let i = ((y0 + at / bw) * w + x0 + at % bw) * 4;
        dst[i..i + 4].copy_from_slice(&[
            encode(p[0] / a),
            encode(p[1] / a),
            encode(p[2] / a),
            to(a),
        ]);
    }
    out
}

/// The radius of each of [`blur`]'s three boxes for a gaussian of about `sigma` pixels.
fn blur_radius(sigma: f32) -> usize {
    // Three boxes of width b have variance 3·(b² − 1)/12.
    let box_w = (4.0 * sigma * sigma + 1.0).sqrt();
    (((box_w - 1.0) / 2.0).round() as usize).max(1)
}

/// How many pixels past the drawn ones [`blur`] works: its three boxes' spread, and one more.
fn blur_reach(sigma: f32) -> usize {
    3 * blur_radius(sigma) + 1
}

/// One box pass over the `n` values of `buf` from `start`, `stride` apart, `2r + 1` wide.
fn box_pass(
    buf: &mut [[f32; 4]],
    start: usize,
    stride: usize,
    n: usize,
    r: usize,
    line: &mut Vec<[f32; 4]>,
) {
    line.clear();
    line.extend((0..n).map(|i| buf[start + i * stride]));
    let inv = 1.0 / (2 * r + 1) as f32;
    let add = |sum: &mut [f32; 4], v: &[f32; 4], sign: f32| {
        for (s, v) in sum.iter_mut().zip(v) {
            *s += sign * v;
        }
    };
    let mut sum = [0.0_f32; 4];
    for v in line.iter().take(r.min(n)) {
        add(&mut sum, v, 1.0);
    }
    for i in 0..n {
        if let Some(v) = line.get(i + r) {
            add(&mut sum, v, 1.0);
        }
        if i > r {
            add(&mut sum, &line[i - r - 1], -1.0);
        }
        for (out, s) in buf[start + i * stride].iter_mut().zip(sum) {
            *out = (s * inv).max(0.0);
        }
    }
}

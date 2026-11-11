// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Procedural signed-distance coverage bitmaps for message band and glass icons.

use aterm_core::render::BandIcon;

/// Face metrics used to fit procedural icons to a cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconMetrics {
    /// Pixels from the cell's top to the baseline.
    pub baseline: f32,
    /// Capital height in pixels.
    pub cap: f32,
    /// Stroke weight in pixels.
    pub stem: f32,
    /// Pixels from the cell's top to the highest printable ASCII ink.
    pub head: f32,
    /// Ring family diameter in pixels (`● ○ ◐ ◌ ⊘`).
    pub disc: f32,
}

/// The metrics of an icon drawn alone in a square `side` pixels on a side (a chrome surface's `DrawPrim::BandIcon`): capitals two thirds of the box on a baseline a sixth of it over its foot, a stroke a ninth of it.
#[must_use]
pub fn boxed_icon_metrics(side: usize) -> IconMetrics {
    let side = side as f32;
    let cap = (side * 0.68).round();
    IconMetrics {
        baseline: (side * 0.84).round(),
        cap,
        stem: (side / 9.0).round().max(1.0),
        head: (side * 0.08).round(),
        disc: cap,
    }
}

/// Width of an icon's raster in pixels (one cell, or three cells when `spill` is set).
#[must_use]
#[inline]
pub const fn band_icon_width(cell_w: usize, spill: bool) -> usize {
    if spill { cell_w * 3 } else { cell_w }
}

/// The stroke the ring family's marks (`○ ◌ ◈` and the rest of [`BandIcon::is_glass_mark`]
/// but `⤓`) are drawn with in a cell of their own, whole pixels: the face's stem held to its
/// share of the family's radius.
#[cfg(test)]
pub(crate) fn mark_stroke(cell_w: usize, cell_h: usize, m: IconMetrics) -> f32 {
    Shape::fit(cell_w, cell_h, m, false, BandIcon::Ring).mark_t()
}

/// Row-major `band_icon_width(cell_w, spill) × cell_h` 8-bit coverage bitmap for `icon`.
#[must_use]
pub fn band_icon_coverage(
    icon: BandIcon,
    cell_w: usize,
    cell_h: usize,
    m: IconMetrics,
    spill: bool,
) -> Vec<u8> {
    let w = band_icon_width(cell_w, spill);
    let mut buf = vec![0u8; w * cell_h];
    if cell_w < 2 || cell_h < 2 {
        return buf;
    }
    let shape = Shape::fit(cell_w, cell_h, m, spill, icon);
    for y in 0..cell_h {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            buf[y * w + x] = coverage(shape.distance(icon, px, py).max(shape.top - py));
        }
    }
    buf
}

/// The coverage, of 255, a pixel whose centre lies `d` pixels outside a shape's edge is laid
/// at (inside where `d` is negative): a ramp a pixel wide centred on the edge, so a pixel is at
/// half ink or more (128) exactly where its centre is on or inside the edge.
fn coverage(d: f32) -> u8 {
    ((0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8
}
/// The most of an icon's half height its stroke may take (about a fifth of the icon), whole pixels: [`Shape::fit`] holds the face's stem to it at the icon's own half height (the capitals', held to the cell's width, or `⤓`'s [`Shape::tall`]) and [`Shape::mark_t`] at the ring family's own radius.
const STROKE_SHARE: f32 = 0.44;

/// Where `↺` ([`Shape::reset`]) is open, in degrees on the raster (`y` down, so `-90` is the
/// top): from a little above its left side round the top left to a little right of its top, the
/// end that carries the head.
const RESET_GAP: [f32; 2] = [-175.0, -70.0];

/// One icon's frame on its raster: its centre, half height, stroke, the horizontal room it may use on each side of the centre, the row it stands on and the highest row it may ink.
struct Shape {
    cx: f32,
    cy: f32,
    half: f32,
    t: f32,
    room: f32,
    bot: f32,
    top: f32,
    /// The ring family's half diameter ([`IconMetrics::disc`], never under three fifths of the capitals in whole pixels), which [`Self::mark_frame`] holds to `half`.
    ring: f32,
    /// The half height an icon narrower than it is tall may keep in a cell of its own (`⤓`):
    /// the capitals' where the row up to the ceiling allows, whatever the cell's width. Like
    /// [`Self::flat`] it keeps none of the pixel and a half [`Self::half`] leaves under the
    /// ceiling: its stem's round end stops at its half height. Its stroke is held to its share
    /// of this, not of `half`, whose share in a narrow cell (Iosevka's half-em) is under both
    /// weights' stems and would draw a bold `⤓` at the regular stem.
    tall: f32,
    /// `◌`'s dots ([`Self::ring_dots`]), placed once per raster: placing them draws them to
    /// judge whether they stand apart.
    dots: std::cell::OnceCell<RingDots>,
    /// The half height `⏸`'s and `!`'s bars span: the capitals' where the row up to the ceiling
    /// allows, held to the room as [`Self::half`] is. `half` leaves a pixel and a half under the
    /// ceiling for the badges (their rings, the warning triangle, `↻`'s head), which reach past
    /// their half height; a bar cut on whole rows ends at its half height, so it drops that margin
    /// and keeps its share of the capitals where the tallest letter stands close over them (held
    /// to `half`, `⏸` on Menlo from 8 to 10 px would be two to four rows).
    flat: f32,
}

impl Shape {
    /// A glass mark's room is its own cell's width, edge to edge, on either raster.
    fn fit(cell_w: usize, cell_h: usize, m: IconMetrics, spill: bool, icon: BandIcon) -> Self {
        let mark = icon.is_glass_mark();
        let (fw, fh) = (cell_w as f32, cell_h as f32);
        let cap = if m.cap.is_finite() && m.cap > 1.0 {
            m.cap.min(fh)
        } else {
            fh * 0.55
        };
        let baseline = if m.baseline.is_finite() {
            m.baseline.clamp(1.0, fh)
        } else {
            fh * 0.8
        };
        let stem = if m.stem.is_finite() { m.stem } else { 1.0 };
        let top = if m.head.is_finite() {
            m.head.clamp(1.0, fh)
        } else {
            1.0
        };
        let (ox, wide) = if spill { (fw, 3.0 * fw) } else { (0.0, fw) };
        let (span, clear) = if mark { (fw, 0.0) } else { (wide, 1.0) };
        let cy = (baseline - cap * 0.5).max(1.0);
        let half0 = (cap * 0.5)
            .min(cy - top - 1.5)
            .min(span * 0.5 - 0.5 - clear)
            .max(1.0);
        let disc = if m.disc.is_finite() && m.disc > 1.0 {
            m.disc.max((cap * 0.6).floor())
        } else {
            cap
        };
        let ring = (disc * 0.5).max(1.0);
        let tall = (cap * 0.5).min(cy - top).max(half0);
        let own_half = if icon == BandIcon::ToBar { tall } else { half0 };
        let t = stem
            .round()
            .clamp(1.0, (own_half * STROKE_SHARE).round().max(1.0));
        let crisp = |odd: bool| {
            ox + if odd {
                (fw * 0.5).floor() + 0.5
            } else {
                (fw * 0.5).round()
            }
        };
        let room_at = |cx: f32| {
            let at = if mark { cx - ox } else { cx };
            let room = (at.min(span - at) - clear).max(1.0);
            (room, half0.min(room - 0.5).max(1.0))
        };
        let cx = if mark && icon != BandIcon::ToBar {
            let size = |cx: f32| {
                let (room, half) = room_at(cx);
                ring_radius(ring, half, room, cx.fract())
            };
            let odd = !cell_w.is_multiple_of(2);
            let (mid, off) = (crisp(odd), crisp(!odd));
            if size(off) > size(mid) { off } else { mid }
        } else {
            crisp((t as u32) % 2 == 1)
        };
        let (room, half) = room_at(cx);
        Shape {
            cx,
            cy,
            half,
            t,
            room,
            bot: cy + half,
            top,
            ring,
            tall,
            flat: (cap * 0.5).min(cy - top).min(room - 0.5).max(half),
            dots: std::cell::OnceCell::new(),
        }
    }

    /// A unit-box point in raster pixels.
    fn p(&self, u: f32, v: f32) -> (f32, f32) {
        (self.cx + u * self.half, self.cy + v * self.half)
    }

    /// A round-capped stroke from `a` to `b` (unit box) of width `w` pixels.
    fn stroke(&self, px: f32, py: f32, a: (f32, f32), b: (f32, f32), w: f32) -> f32 {
        let (ax, ay) = self.p(a.0, a.1);
        let (bx, by) = self.p(b.0, b.1);
        super::seg_dist(px, py, ax, ay, bx, by) - w * 0.5
    }

    /// A polyline of strokes (round joins).
    fn polyline(&self, px: f32, py: f32, pts: &[(f32, f32)], w: f32) -> f32 {
        pts.windows(2)
            .map(|s| self.stroke(px, py, s[0], s[1], w))
            .fold(f32::INFINITY, f32::min)
    }

    /// A mark one stroke wide on the centre line, from pixel row `y0` to `y1` (whole rows, so its ends are crisp).
    fn bar(&self, px: f32, py: f32, y0: f32, y1: f32) -> f32 {
        rect(
            px,
            py,
            self.cx - self.t * 0.5,
            y0,
            self.cx + self.t * 0.5,
            y1,
        )
    }

    /// A filled polygon (unit box), any winding, convex or not.
    fn polygon(&self, px: f32, py: f32, pts: &[(f32, f32)]) -> f32 {
        let v: Vec<(f32, f32)> = pts.iter().map(|&(u, w)| self.p(u, w)).collect();
        polygon_sd(px, py, &v)
    }

    /// A round icon's outer radius and centre row: a pixel over the capital height (a round letter's overshoot), its sides and its top on pixel boundaries, its bottom no lower than the baseline's row.
    fn ring_frame(&self) -> (f32, f32) {
        let frac = self.cx.fract();
        let r = ((self.half + 0.5 - frac).round() + frac).min(self.room);
        let top = (self.cy - r).floor();
        (r, top + r)
    }

    /// The warning triangle: its apex row, its base row (the baseline, where the icon stands on it), its half base and the rounding of its corners.
    fn triangle(&self) -> (f32, f32, f32, f32) {
        let bottom = self.bot.round();
        let round = self.t * 0.45;
        let h = (2.4 * self.half)
            .round()
            .min((bottom - self.top + 0.98 * round).floor())
            .max(2.0);
        (bottom - h, bottom, (0.58 * h).min(self.room), round)
    }

    /// The signed distance (pixels, negative inside) to `icon`.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per icon of the closed set, each a few primitives"
    )]
    fn distance(&self, icon: BandIcon, px: f32, py: f32) -> f32 {
        let t = self.t;
        match icon {
            BandIcon::Info => {
                let (r, cyr) = self.ring_frame();
                let disc = (px - self.cx).hypot(py - cyr) - r;
                let (top, n) = (cyr - r, 2.0 * r);
                let h = (n * 0.64).round();
                let y0 = top + ((n - h) * 0.5).floor();
                let dh = t.max((n / 7.0).round());
                let gap = (n / 16.0).round().max(1.0);
                let mark =
                    self.bar(px, py, y0, y0 + dh)
                        .min(self.bar(px, py, y0 + dh + gap, y0 + h));
                disc.max(-mark)
            }
            BandIcon::Success => {
                let w = t * 1.1;
                let s = ((self.room - w * 0.5) / (0.9 * self.half)).min(1.0);
                self.polyline(
                    px,
                    py,
                    &[
                        (-0.84 * s, 0.06 * s),
                        (-0.3 * s, 0.62 * s),
                        (0.9 * s, -0.7 * s),
                    ],
                    w,
                )
            }
            BandIcon::Error => {
                let w = t * 1.05;
                let k = 0.66f32.min((self.room - w * 0.5) / self.half);
                self.stroke(px, py, (-k, -k), (k, k), w).min(self.stroke(
                    px,
                    py,
                    (-k, k),
                    (k, -k),
                    w,
                ))
            }
            BandIcon::Warn => {
                let (apex, bottom, wb, round) = self.triangle();
                let h = bottom - apex;
                let tri = rounded_triangle(px, py, self.cx, apex, bottom, wb, round);
                let k = wb / h;
                let d1 = bottom - (h * 0.1).round().max(2.0);
                let d0 = d1 - t;
                let s1 = d0 - 1.0;
                let s0 = (apex + round + (t * 0.5 + 1.0) / k).ceil().min(s1 - 1.0);
                let mark = self.bar(px, py, s0, s1).min(self.bar(px, py, d0, d1));
                tri.max(-mark)
            }
            BandIcon::Download => {
                let bottom = self.bot.round();
                let v = |y: f32| (y - self.cy) / self.half;
                let tray = v(bottom - t * 0.5);
                let top = v(self.cy - self.half + t * 0.5);
                let tip = v(bottom - t - (0.2 * self.half).round().max(1.0) - t * 0.5);
                let a = 0.5;
                let stem = self.stroke(px, py, (0.0, top), (0.0, tip), t);
                let head = self.polyline(px, py, &[(-a, tip - a), (0.0, tip), (a, tip - a)], t);
                let tw = 0.74f32.min((self.room - t * 0.5) / self.half);
                let tray = self.stroke(px, py, (-tw, tray), (tw, tray), t);
                stem.min(head).min(tray)
            }
            BandIcon::Upload => {
                let v = |y: f32| (y - self.cy) / self.half;
                let bottom = v(self.bot.round() - t * 0.5);
                let tip = v(self.cy - self.half + t * 0.5);
                let a = 0.56;
                let stem = self.stroke(px, py, (0.0, bottom), (0.0, tip), t);
                let head = self.polyline(px, py, &[(-a, tip + a), (0.0, tip), (a, tip + a)], t);
                stem.min(head)
            }
            BandIcon::Update => {
                let (ro, cyr) = self.ring_frame();
                let r = (ro - t * 0.5) * 0.92;
                let (ox, oy) = (self.cx, cyr);
                let (dx, dy) = (px - ox, py - oy);
                let ang = dy.atan2(dx).to_degrees();
                let (g0, g1) = (-100.0f32, -15.0f32);
                let in_gap = ang > g0 && ang < g1;
                let arc = if in_gap {
                    let end = |deg: f32| {
                        let a = deg.to_radians();
                        (px - (ox + r * a.cos())).hypot(py - (oy + r * a.sin())) - t * 0.5
                    };
                    end(g0).min(end(g1))
                } else {
                    (dx.hypot(dy) - r).abs() - t * 0.5
                };
                let a = g0.to_radians();
                let (ex, ey) = (ox + r * a.cos(), oy + r * a.sin());
                let (tx, ty) = (-a.sin(), a.cos());
                let (nx, ny) = (-ty, tx);
                let hl = (t + 0.8).max(r * 0.42);
                let hw = (t * 0.55 + 1.0).max(r * 0.34);
                let head = polygon_sd(
                    px,
                    py,
                    &[
                        (ex + tx * hl, ey + ty * hl),
                        (ex + nx * hw, ey + ny * hw),
                        (ex - nx * hw, ey - ny * hw),
                    ],
                );
                arc.min(head)
            }
            BandIcon::Pause => {
                let w = t.max((self.half * 0.5).round());
                let mut gap = (self.half * 0.4).round().max(2.0);
                if (self.cx - gap * 0.5).fract() != 0.0 {
                    gap += 1.0;
                }
                let (y0, y1) = (
                    (self.cy - 0.8 * self.flat).round(),
                    (self.cy + 0.8 * self.flat).round(),
                );
                let (l0, r0) = (self.cx - gap * 0.5 - w, self.cx + gap * 0.5);
                rect(px, py, l0, y0, l0 + w, y1).min(rect(px, py, r0, y0, r0 + w, y1))
            }
            BandIcon::Sparkle => {
                let k = 0.3;
                self.polygon(
                    px,
                    py,
                    &[
                        (0.0, -1.0),
                        (k, -k),
                        (1.0, 0.0),
                        (k, k),
                        (0.0, 1.0),
                        (-k, k),
                        (-1.0, 0.0),
                        (-k, -k),
                    ],
                )
            }
            BandIcon::Alert => {
                let bottom = (self.cy + self.flat).round();
                let top = (self.cy - self.flat).round();
                let dh = t.max((self.half * 0.26).round());
                let gap = (self.half * 0.2).round().max(1.0);
                self.bar(px, py, top, bottom - dh - gap)
                    .min(self.bar(px, py, bottom - dh, bottom))
            }
            BandIcon::Dot => self.p_disc(px, py, 0.0, (t * 0.85).max(1.0)),
            BandIcon::Remove => {
                let (r, cyr) = self.ring_frame();
                let disc = (px - self.cx).hypot(py - cyr) - r;
                let bh = (t * 1.5).round().max(2.0);
                let y0 = (cyr - bh * 0.5).round();
                let hw = (r * 0.6).max(t);
                disc.max(-rect(px, py, self.cx - hw, y0, self.cx + hw, y0 + bh))
            }
            BandIcon::Disc => self.mark_disc(px, py),
            BandIcon::Ring => self.mark_ring(px, py),
            BandIcon::HalfDisc => {
                let split = (self.cx + 0.5).floor();
                self.mark_ring(px, py)
                    .min(self.mark_disc(px, py).max(px - split))
            }
            BandIcon::DottedRing => self.dotted_ring(px, py),
            BandIcon::SlashedRing => {
                let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
                let k = (r - t * 0.5) * std::f32::consts::FRAC_1_SQRT_2;
                let slash =
                    super::seg_dist(px, py, self.cx - k, cyr + k, self.cx + k, cyr - k) - t * 0.5;
                self.mark_ring(px, py).min(slash)
            }
            BandIcon::ToBar => {
                let h = self.tall;
                let bottom = (self.cy + h).round();
                let bar = bottom - t * 0.5;
                let tip = bar - t;
                let a = (0.5 * h).min(self.room - t * 0.5);
                let bw = (0.74 * h).min(self.room - t * 0.5);
                let seg = |x0: f32, y0: f32, x1: f32, y1: f32| {
                    super::seg_dist(px, py, x0, y0, x1, y1) - t * 0.5
                };
                let stem = seg(self.cx, self.cy - h + t * 0.5, self.cx, tip);
                let head = seg(self.cx - a, tip - a, self.cx, tip).min(seg(
                    self.cx,
                    tip,
                    self.cx + a,
                    tip - a,
                ));
                stem.min(head)
                    .min(seg(self.cx - bw, bar, self.cx + bw, bar))
            }
            BandIcon::Fisheye => {
                let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
                let gap = (r * 0.3).round().max(1.0);
                let dot = (r - t - gap).max(t * 0.5);
                self.mark_ring(px, py)
                    .min((px - self.cx).hypot(py - cyr) - dot)
            }
            BandIcon::InsetDiamond => self.inset_diamond(px, py),
            BandIcon::House => {
                let (r, cyr) = self.mark_frame();
                let (l, rt, top, floor) = (self.cx - r, self.cx + r, cyr - r, cyr + r);
                let house = polygon_sd(
                    px,
                    py,
                    &[(self.cx, top), (rt, cyr), (rt, floor), (l, floor), (l, cyr)],
                );
                self.mark_outline(house).max(self.mark_box(px, py))
            }
            BandIcon::Diamond => {
                let (r, cyr) = self.mark_frame();
                let diamond =
                    ((px - self.cx).abs() + (py - cyr).abs() - r) * std::f32::consts::FRAC_1_SQRT_2;
                diamond.max(self.mark_box(px, py))
            }
            BandIcon::Fork => self.fork(px, py),
            BandIcon::Enter => self.enter(px, py),
            BandIcon::Reset => self.reset(px, py),
            BandIcon::Lock => self.lock(px, py),
            BandIcon::Hourglass => self.hourglass(px, py),
            BandIcon::Drive => self.drive(px, py),
            BandIcon::More => {
                let r = (t * 0.6).max(0.8);
                let y = self.bot.round() - r;
                let v = (y - self.cy) / self.half;
                let s = 0.66f32.min((self.room - r) / self.half);
                [-s, 0.0, s]
                    .iter()
                    .map(|&u| {
                        let (x, yy) = self.p(u, v);
                        (px - x).hypot(py - yy) - r
                    })
                    .fold(f32::INFINITY, f32::min)
            }
        }
    }

    /// A pictogram's rows ([`BandIcon::is_pictogram`]): its top and its bottom, whole pixels two half heights apart, standing on the row the badges stand on.
    fn picto_rows(&self) -> (f32, f32) {
        let bottom = self.bot.round();
        (bottom - (2.0 * self.half).round().max(2.0), bottom)
    }

    /// A half width near `w` that puts both sides of the centre on pixel edges, held to the room.
    fn snap(&self, w: f32) -> f32 {
        let f = self.cx.fract();
        ((w + f).round() - f).min(self.room).max(f.max(0.5))
    }

    /// The padlock: a body four fifths as wide as the icon is tall, its lower half, under a shackle one stroke wide that keeps a hole at least a pixel wide (two where the stroke is even) where the body is as wide as that, its legs a pixel or more inside the body's sides where the body has the room.
    fn lock(&self, px: f32, py: f32) -> f32 {
        let t = self.t;
        let (y0, y1) = self.picto_rows();
        let h = y1 - y0;
        let bw = self.snap(0.8 * self.half);
        let body_top = y1 - (0.5 * h).round().max(2.0);
        let r = (0.1 * h).min(0.4 * bw);
        let body = round_rect(px, py, self.cx - bw, body_top, self.cx + bw, y1, r);
        let so = self
            .snap(0.62 * self.half)
            .min(bw - 1.0)
            .max(t + 1.0 - self.cx.fract())
            .min(bw);
        let rs = so - t * 0.5;
        let arc_y = y0 + so;
        let dx = px - self.cx;
        let shackle = if py < arc_y {
            (dx.hypot(py - arc_y) - rs).abs() - t * 0.5
        } else {
            ((dx.abs() - rs).abs() - t * 0.5).max(py - (body_top + r))
        };
        body.min(shackle)
    }

    /// The hourglass: a cap at its top and its bottom, the full width, one stroke tall or a seventh of the icon where that is less (so a small bold one keeps its bulbs open), and between them the glass, one stroke drawn in to a waist at the middle row; the lower bulb holds the run-down sand, a triangle on the bottom cap.
    fn hourglass(&self, px: f32, py: f32) -> f32 {
        let t = self.t;
        let (y0, y1) = self.picto_rows();
        let bw = self.snap(self.half);
        let cap = t.min(((y1 - y0) / 7.0).round().max(1.0));
        let caps = rect(px, py, self.cx - bw, y0, self.cx + bw, y0 + cap).min(rect(
            px,
            py,
            self.cx - bw,
            y1 - cap,
            self.cx + bw,
            y1,
        ));
        let (gw, waist, mid) = (0.69 * bw, 0.12 * bw, (y0 + y1) * 0.5);
        let (top, foot) = (y0 + t * 0.5, y1 - t * 0.5);
        let dx = (px - self.cx).abs();
        let glass = super::seg_dist(dx, py, gw, top, waist, mid)
            .min(super::seg_dist(dx, py, waist, mid, gw, foot))
            - t * 0.5;
        let floor = y1 - cap;
        let apex = mid + 0.3 * (floor - mid);
        let sand = polygon_sd(
            px,
            py,
            &[
                (self.cx, apex),
                (self.cx + 0.6 * gw, floor),
                (self.cx - 0.6 * gw, floor),
            ],
        );
        caps.min(glass).min(sand)
    }

    /// The drive, drawn as a drum: a filled top, an ellipse a third of the drum's height deep, its sides one stroke wide, and its bottom rim; a middle rim where it clears the top and the bottom rim by two rows or more down the centre.
    fn drive(&self, px: f32, py: f32) -> f32 {
        let t = self.t;
        let (y0, y1) = self.picto_rows();
        let h = y1 - y0;
        let w = self.snap(0.95 * self.half);
        let ry = (h / 6.0).max(t * 0.5 + 0.5);
        let (top_c, foot_c) = (y0 + ry, y1 - ry - t * 0.5);
        let a = w - t * 0.5;
        let lid = ellipse_sd(px - self.cx, py - top_c, w, ry);
        let sides = rect(px, py, self.cx - w, top_c, self.cx - w + t, foot_c).min(rect(
            px,
            py,
            self.cx + w - t,
            top_c,
            self.cx + w,
            foot_c,
        ));
        let rim = |c: f32| {
            let d = if py < c {
                ((px - self.cx).abs() - a).hypot(py - c) - t * 0.5
            } else {
                ellipse_sd(px - self.cx, py - c, a, ry).abs() - t * 0.5
            };
            d.max((px - self.cx).abs() - w)
        };
        let mut drum = lid.min(sides).min(rim(foot_c));
        let gap = (foot_c - top_c - 1.5 * t) * 0.5;
        if gap >= 2.0 {
            drum = drum.min(rim(top_c + t * 0.5 + gap));
        }
        drum
    }

    /// `⎇` in the family's box ([`Self::mark_box`]), one stroke wide ([`Self::mark_t`]): a foot along the box's floor from its left side, a rise to its top, a head along the top to its right side, and apart from the rise a second foot along the floor to the right side, the line the first one steps off. The rise and the second foot stand apart on the floor's row: a clear pixel between them on the measured faces at 2x, and at least a pixel under half ink between them on every measured 1x face.
    fn fork(&self, px: f32, py: f32) -> f32 {
        let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
        let (l, rt) = (self.cx - r, self.cx + r);
        let (hi, lo) = (cyr - r + t * 0.5, cyr + r - t * 0.5);
        let seg =
            |x0: f32, y0: f32, x1: f32, y1: f32| super::seg_dist(px, py, x0, y0, x1, y1) - t * 0.5;
        let span = 2.0 * r;
        let step = (l + 0.25 * span).round() + 0.5 * (t % 2.0);
        let top = (l + 0.55 * span).round() + 0.5 * (t % 2.0);
        let foot = (top + t).max(step + 2.0 * t + 1.0).min(rt - t * 0.5);
        seg(l, lo, step, lo)
            .min(seg(step, lo, top, hi))
            .min(seg(top, hi, rt, hi))
            .min(seg(foot, lo, rt, lo))
            .max(self.mark_box(px, py))
    }

    /// `⎆` in the family's box ([`Self::mark_box`]): `◆`'s diamond as an outline one stroke wide ([`Self::mark_t`]), and inside it an arrow along its centre row from its left corner to its right-pointing tip, with two diagonal head strokes. The shaft and head use the outline's stroke where the diamond holds three of them across its inside, else a thinner stroke down to a pixel; a half-pixel centreline joins the shaft through the head to its tip without closing the smallest face.
    fn enter(&self, px: f32, py: f32) -> f32 {
        let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
        let diamond =
            ((px - self.cx).abs() + (py - cyr).abs() - r) * std::f32::consts::FRAC_1_SQRT_2;
        let inner = (r - std::f32::consts::SQRT_2 * t).max(1.0);
        let ta = t.min((inner / 3.0).floor()).max(1.0);
        let half = (0.55 * inner).max(ta * 0.5 + 0.5);
        let (base, tip) = (self.cx, self.cx + half);
        let y0 = (cyr - ta * 0.5).round();
        let mid = y0 + ta * 0.5;
        let nose = super::seg_dist(px, py, base, mid, tip, mid) - 0.25;
        let stem = rect(px, py, self.cx - r, y0, base, y0 + ta).min(nose);
        let head = super::seg_dist(px, py, base, mid - half, tip, mid).min(super::seg_dist(
            px,
            py,
            tip,
            mid,
            base,
            mid + half,
        )) - ta * 0.5;
        self.mark_outline(diamond)
            .min(stem.min(head).max(diamond))
            .max(self.mark_box(px, py))
    }

    /// `↺` in the family's box ([`Self::mark_box`]): `○`'s line ([`Self::mark_ring`]'s band,
    /// one [`Self::mark_t`] across, pushed out by [`Self::ring_lack`]) open across its top left
    /// ([`RESET_GAP`]), each open end a round cap on the line's centre, and at the end just right
    /// of the top an arrowhead pointing back along the line, leftward: [`BandIcon::Update`]'s
    /// turning arrow mirrored and drawn at the family's size. The head is `t + 1.5` pixels long
    /// or seven tenths of the radius, whichever is more, and `t + 1` each side of the line's
    /// centre, held to stop a fifth of a pixel short of the ring's centre so the mark stays
    /// hollow on the smallest rings, and never under half a pixel; its outer half reaches past
    /// the box, which cuts it, so its barb lies inside the line. Below its centre row the mark is
    /// `○` exactly.
    fn reset(&self, px: f32, py: f32) -> f32 {
        let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
        let (dx, dy) = (px - self.cx, py - cyr);
        let mid = r - t * 0.5;
        let on_line = |a: f32| (self.cx + mid * a.cos(), cyr + mid * a.sin());
        let [g0, g1] = RESET_GAP.map(f32::to_radians);
        let angle = dy.atan2(dx);
        let line = if angle > g0 && angle < g1 {
            let cap = |a: f32| {
                let (x, y) = on_line(a);
                (px - x).hypot(py - y) - t * 0.5
            };
            cap(g0).min(cap(g1))
        } else {
            let d = dx.hypot(dy);
            (d - r - self.ring_lack()).max(r - t - d)
        };
        let (ex, ey) = on_line(g1);
        let (tx, ty) = (g1.sin(), -g1.cos());
        let (nx, ny) = (g1.cos(), g1.sin());
        let long = (t + 1.5).max(r * 0.7);
        let wide = (t + 1.0).min(mid - 0.2).max(0.5);
        let head = polygon_sd(
            px,
            py,
            &[
                (ex + tx * long, ey + ty * long),
                (ex + nx * wide, ey + ny * wide),
                (ex - nx * wide, ey - ny * wide),
            ],
        );
        line.min(head).max(self.mark_box(px, py))
    }

    /// The ring family's radius ([`ring_radius`]) and centre row: the face's `●` on the capitals' centre line, its sides and its top on pixel boundaries.
    fn mark_frame(&self) -> (f32, f32) {
        let r = ring_radius(self.ring, self.half, self.room, self.cx.fract());
        let top = (self.cy - r).round();
        (r, top + r)
    }

    /// `●`: the family's disc ([`Self::mark_frame`]).
    fn mark_disc(&self, px: f32, py: f32) -> f32 {
        let (r, cyr) = self.mark_frame();
        (px - self.cx).hypot(py - cyr) - r
    }
    /// The family's stroke: the face's stem ([`Self::t`]) held to the share of the family's own radius ([`STROKE_SHARE`]) that [`Self::fit`] holds it to of the capitals'.
    fn mark_t(&self) -> f32 {
        let (r, _) = self.mark_frame();
        self.t.min((r * STROKE_SHARE).round()).max(1.0)
    }

    /// `○`: the family's disc as a ring one stroke wide ([`Self::mark_t`]), its outer edge the
    /// disc's pushed out by what the stroke lacks of covering a whole pixel at the ring's top
    /// ([`Self::ring_lack`]), cut to the family's box ([`Self::mark_box`]).
    fn mark_ring(&self, px: f32, py: f32) -> f32 {
        let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
        let d = (px - self.cx).hypot(py - cyr);
        (d - r - self.ring_lack())
            .max(r - t - d)
            .max(self.mark_box(px, py))
    }

    /// What `○`'s stroke, one [`Self::mark_t`] across inside the disc's edge, lacks of covering a
    /// whole pixel at the ring's top (pixels; none where one is whole): of the pixels in the
    /// stroke's rows in the column nearest the ring's centre, the least by which one falls short
    /// of lying half a pixel inside both of the stroke's edges. Only a stroke one pixel across
    /// can lack any (a wider one always holds a row whose centre is half a pixel inside it), and
    /// only on an even ring, whose centre is a pixel edge: the top's two pixels sit half a pixel
    /// either side of the axis, where the disc's edge has curved down through them, so each would
    /// be laid at 242 on a ring 6 px across and at 234 on one of 4.
    fn ring_lack(&self) -> f32 {
        let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
        let x = (self.cx - 0.5).round() + 0.5 - self.cx;
        let top = cyr - r;
        (0..t as usize)
            .map(|j| {
                let d = x.hypot(top + 0.5 + j as f32 - cyr);
                (d - r).max(r - t - d) + 0.5
            })
            .fold(f32::INFINITY, f32::min)
            .max(0.0)
    }

    /// `◌`: `○`'s ring drawn as dots ([`Self::ring_dots`]), each standing clear of its
    /// neighbours, cut to the family's box ([`Self::mark_box`]).
    fn dotted_ring(&self, px: f32, py: f32) -> f32 {
        let dots = self.dots.get_or_init(|| self.ring_dots());
        dots.at
            .iter()
            .map(|&at| self.ring_dot(px, py, at, dots.w, dots.grow))
            .fold(f32::INFINITY, f32::min)
    }

    /// One of `◌`'s dots: a disc centred at `at`, `w` pixels across and grown by `grow` past
    /// that, cut to the family's box ([`Self::mark_box`]).
    fn ring_dot(&self, px: f32, py: f32, (x, y): (f32, f32), w: f32, grow: f32) -> f32 {
        ((px - x).hypot(py - y) - w * 0.5 - grow).max(self.mark_box(px, py))
    }

    /// `◌`'s dots: eight on `○`'s stroke, or the four of them on the axes, each standing apart from
    /// the rest as drawn ([`Self::stand_apart`]). A dot `w` pixels across has its centre on the
    /// centre line of a stroke `w` wide inside the disc's edge, so it lies on `○`'s stroke, at one
    /// of eight places an eighth of a turn apart starting on the right-hand axis. The dots are the
    /// widest, from the stroke's width ([`Self::mark_t`]) down to two pixels, whose eight stand
    /// apart once they keep their places and grow together by what the dot nearest to covering a
    /// pixel whole lacks of it (nothing unless they are two pixels across and every one is off the
    /// pixel grid: 0.14 px on a 16 × 32 cell at stem 2). The room between places does not settle
    /// it: on a ring 16 px across with a four-pixel stroke, four-pixel dots leave 0.59 px between
    /// neighbours' edges on the chord between their places, and still meet corner to corner through
    /// pixels over half ink. Where the stroke is one pixel, or no wider dots stand apart (a
    /// two-pixel stroke on a ring of 7 or 8 px, Menlo 14 bold's), the dots are one pixel across,
    /// each on a whole pixel: of the four pixels around its place, the one nearest it whose centre
    /// stays on the stroke, a tie (the diagonals of a ring 11 px across lie as near two pixels)
    /// going clockwise. An even ring's centre is a pixel corner, so its axis places lie on pixel
    /// edges; there all eight places first turn clockwise by the angle (`asin(½ / on)`) that
    /// carries the axis ones half a pixel onto a row or column of pixel centres, a few hundredths
    /// of a pixel inside the centre itself, which the snap then takes; the eight stay one rotation
    /// of the ring. A one-pixel
    /// stroke on a ring 6 px across is a loop of sixteen pixels, so eight dots and their gaps fill
    /// it, every other pixel (left unturned, its diagonals take the pixels beside its axis dots and
    /// touch them). The loops of rings 4 and 5 px across have twelve pixels or fewer, where eight
    /// dots touch: on those `◌` is its four axis dots.
    fn ring_dots(&self) -> RingDots {
        use std::f32::consts::FRAC_1_SQRT_2 as H;
        const WAYS: [(f32, f32); 8] = [
            (1.0, 0.0),
            (H, H),
            (0.0, 1.0),
            (-H, H),
            (-1.0, 0.0),
            (-H, -H),
            (0.0, -1.0),
            (H, -H),
        ];
        let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
        let pixel = |v: f32| v.floor() + 0.5;
        let place = |on: f32, (c, s): (f32, f32)| (self.cx + on * c, cyr + on * s);
        let mut w = t;
        while w > 1.0 {
            let on = r - w * 0.5;
            let at = WAYS.map(|way| place(on, way));
            let lack = at
                .iter()
                .map(|&(x, y)| (x - pixel(x)).hypot(y - pixel(y)) - (w * 0.5 - 0.5))
                .fold(f32::INFINITY, f32::min);
            let dots = RingDots {
                at: at.to_vec(),
                w,
                grow: lack.max(0.0),
            };
            if self.stand_apart(&dots) {
                return dots;
            }
            w -= 1.0;
        }
        let on = r - 0.5;
        let turn = if self.cx.fract() == 0.0 {
            (0.5 / on).asin()
        } else {
            0.0
        };
        let (sin, cos) = turn.sin_cos();
        let dot = |(c, s): (f32, f32)| {
            let (c, s) = (c * cos - s * sin, s * cos + c * sin);
            let (x, y) = place(on, (c, s));
            let (x0, y0) = (pixel(x - 0.5), pixel(y - 0.5));
            [
                (x0, y0),
                (x0 + 1.0, y0),
                (x0, y0 + 1.0),
                (x0 + 1.0, y0 + 1.0),
            ]
            .into_iter()
            .min_by_key(|&(qx, qy)| {
                let (dx, dy) = (qx - self.cx, qy - cyr);
                let off = (dx.hypot(dy) - on).abs() > 0.5;
                let far = ((qx - x).hypot(qy - y) * 1024.0).round() as i32;
                (off, far, c * dy - s * dx < 0.0)
            })
            .unwrap_or((x, y))
        };
        let eight = RingDots {
            at: WAYS.map(dot).to_vec(),
            w: 1.0,
            grow: 0.0,
        };
        if self.stand_apart(&eight) {
            eight
        } else {
            RingDots {
                at: eight.at.into_iter().step_by(2).collect(),
                w: 1.0,
                grow: 0.0,
            }
        }
    }

    /// Whether `dots`, laid as [`Self::dotted_ring`] lays them, stand apart: each lays some
    /// pixel at half ink or more ([`coverage`]), and none of those pixels is beside, over, under
    /// or corner to corner with one another dot lays so. A dot's own such pixels, the pixel
    /// centres in a disc cut to a box, are one group, so the dots lay as many groups as there
    /// are dots, a pixel under half ink between each and the next.
    fn stand_apart(&self, dots: &RingDots) -> bool {
        let reach = (dots.w * 0.5 + dots.grow).ceil() as i32 + 1;
        let inked: Vec<Vec<(i32, i32)>> = dots
            .at
            .iter()
            .map(|&at| {
                let (x0, y0) = (at.0.floor() as i32, at.1.floor() as i32);
                (y0 - reach..=y0 + reach)
                    .flat_map(|y| (x0 - reach..=x0 + reach).map(move |x| (x, y)))
                    .filter(|&(x, y)| {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        coverage(self.ring_dot(px, py, at, dots.w, dots.grow)) >= 128
                    })
                    .collect()
            })
            .collect();
        let near = |a: &[(i32, i32)], b: &[(i32, i32)]| {
            a.iter().any(|p| {
                b.iter()
                    .any(|q| (p.0 - q.0).abs().max((p.1 - q.1).abs()) <= 1)
            })
        };
        inked.iter().all(|a| !a.is_empty())
            && inked
                .iter()
                .enumerate()
                .all(|(i, a)| inked[i + 1..].iter().all(|b| !near(a, b)))
    }

    /// `◈`: `◆`'s outline one stroke wide ([`Self::mark_t`]) with a small filled diamond at its
    /// centre, cut to the family's box ([`Self::mark_box`]). A band one pixel across on the
    /// diagonal holds a pixel whole only where its centre line runs through pixel centres, which
    /// the band just inside `◆`'s edge never does (it peaked at 202 or 218), so an outline one
    /// pixel across is drawn in whole pixels instead: the pixels whose centres sit one sum of
    /// offsets `|dx| + |dy|` from the ring's centre form a staircase (that sum is a whole number
    /// on either parity of ring), and the outline is the outermost staircase `◆` inks at least
    /// half, `◆`'s outline as `○` is `●`'s, laid crisp. Its dot is the staircases out to the
    /// drawn dot's edge, at least the innermost, crisp too, a clear staircase standing between it
    /// and the outline. On a ring 4 px across there is no room for one: the dot's staircase is
    /// the one inside the outline's, and the two fill the ring's twelve pixels whole, so `◈`
    /// reads filled there, apart from `◉` (the effort menu's row above it), whose centre is
    /// under half ink on that ring.
    fn inset_diamond(&self, px: f32, py: f32) -> f32 {
        use std::f32::consts::{FRAC_1_SQRT_2, SQRT_2};
        let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
        let sum = (px - self.cx).abs() + (py - cyr).abs();
        let diamond = |a: f32| (sum - a) * FRAC_1_SQRT_2;
        let inner = (r - SQRT_2 * t - (1.0 + SQRT_2)).max(0.5);
        let ink = if t > 1.0 {
            self.mark_outline(diamond(r)).min(diamond(inner))
        } else {
            let low = if self.cx.fract() == 0.0 { 1.0 } else { 0.0 };
            let (outline, dot) = (r.floor(), inner.floor().max(low));
            ((sum - outline).abs() - 0.5).min(sum - dot - 0.5)
        };
        ink.max(self.mark_box(px, py))
    }

    /// The band one stroke wide ([`Self::mark_t`]) just inside a shape whose signed distance at the pixel is `d`: its outline, drawn as `○` is from `●`.
    fn mark_outline(&self, d: f32) -> f32 {
        let t = self.mark_t();
        (d + t * 0.5).abs() - t * 0.5
    }

    /// The family's box ([`Self::mark_frame`]): the disc's ink box, which the marks drawn in it (`○` and the marks drawn on it, `◌ ◈ ⌂ ◆ ⎇ ⎆`) are cut to, so neither a corner's tip nor a stroke or dot grown to cover whole pixels inks a row or column the disc does not.
    fn mark_box(&self, px: f32, py: f32) -> f32 {
        let (r, cyr) = self.mark_frame();
        rect(px, py, self.cx - r, cyr - r, self.cx + r, cyr + r)
    }

    /// A disc of radius `r` pixels on the centre line, `v` half heights below the centre.
    fn p_disc(&self, px: f32, py: f32, v: f32, r: f32) -> f32 {
        let (x, y) = self.p(0.0, v);
        (px - x).hypot(py - y) - r
    }
}

/// `◌`'s dots ([`Shape::ring_dots`]): their centres (eight, or the four of them on the axes),
/// each dot `w` pixels across and grown by `grow` past that.
struct RingDots {
    at: Vec<(f32, f32)>,
    w: f32,
    grow: f32,
}

/// The ring family's radius about a centre `frac` (`0` or `0.5`) past a pixel edge: the face's `●`'s half height `ring` held to `half` and `room`, a diameter of the other parity rounded a half pixel down.
fn ring_radius(ring: f32, half: f32, room: f32, frac: f32) -> f32 {
    let face = (ring - frac - 0.5).ceil();
    (face.min((half - frac).round()) + frac).min(room).max(1.0)
}

/// The signed distance to the axis-aligned box `[x0, x1) × [y0, y1)`.
fn rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let dx = (x0 - px).max(px - x1);
    let dy = (y0 - py).max(py - y1);
    if dx <= 0.0 && dy <= 0.0 {
        dx.max(dy)
    } else {
        dx.max(0.0).hypot(dy.max(0.0))
    }
}

/// The signed distance to the isosceles triangle with its apex at `(cx, apex)` and its base `cx ± wb` on row `bottom`, its corners rounded by `round` pixels while its edges stay where they are: the triangle shrunk about its incentre by `round`, then grown back by it.
fn rounded_triangle(px: f32, py: f32, cx: f32, apex: f32, bottom: f32, wb: f32, round: f32) -> f32 {
    let h = bottom - apex;
    let side = wb.hypot(h);
    let rho = wb * h / (wb + side);
    let f = ((rho - round) / rho).max(0.0);
    let iy = bottom - rho;
    let v = |x: f32, y: f32| (cx + (x - cx) * f, iy + (y - iy) * f);
    polygon_sd(
        px,
        py,
        &[v(cx, apex), v(cx + wb, bottom), v(cx - wb, bottom)],
    ) - round
}

/// The signed distance to the box `[x0, x1) × [y0, y1)` with its corners rounded by `r` pixels.
fn round_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let r = r.min((x1 - x0) * 0.5).min((y1 - y0) * 0.5).max(0.0);
    rect(px, py, x0 + r, y0 + r, x1 - r, y1 - r) - r
}

/// The signed distance (near its edge, which is all coverage reads) to the ellipse about the origin with half axes `a` and `b`, at `(x, y)`.
fn ellipse_sd(x: f32, y: f32, a: f32, b: f32) -> f32 {
    let (u, v) = (x / a, y / b);
    let k0 = u.hypot(v);
    let k1 = (u / a).hypot(v / b);
    if k1 > 0.0 {
        k0 * (k0 - 1.0) / k1
    } else {
        -a.min(b)
    }
}

/// The signed distance from `(px, py)` to a closed polygon (negative inside), any winding, convex or concave — the even-odd crossing test for the sign, the nearest edge for the magnitude.
fn polygon_sd(px: f32, py: f32, v: &[(f32, f32)]) -> f32 {
    let n = v.len();
    if n < 3 {
        return f32::INFINITY;
    }
    let mut d = f32::INFINITY;
    let mut inside = false;
    for i in 0..n {
        let (ax, ay) = v[i];
        let (bx, by) = v[(i + n - 1) % n];
        d = d.min(super::seg_dist(px, py, ax, ay, bx, by));
        if (ay > py) != (by > py) && px < (bx - ax) * (py - ay) / (by - ay) + ax {
            inside = !inside;
        }
    }
    if inside { -d } else { d }
}

#[cfg(test)]
#[path = "icons_tests.rs"]
mod tests;

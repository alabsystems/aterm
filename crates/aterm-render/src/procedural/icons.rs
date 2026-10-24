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
            let d = shape.distance(icon, px, py).max(shape.top - py);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            buf[y * w + x] = (cov * 255.0).round() as u8;
        }
    }
    buf
}
/// The most of an icon's half height its stroke may take (about a fifth of the icon), whole pixels: [`Shape::fit`] holds the face's stem to it at the icon's own half height (the capitals', held to the cell's width, or `⤓`'s [`Shape::tall`]) and [`Shape::mark_t`] at the ring family's own radius.
const STROKE_SHARE: f32 = 0.44;

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
            BandIcon::DottedRing => {
                let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
                let on = r - t * 0.5;
                let step = std::f32::consts::FRAC_PI_4;
                let k = ((py - cyr).atan2(px - self.cx) / step).round() * step;
                (px - (self.cx + on * k.cos())).hypot(py - (cyr + on * k.sin())) - t * 0.5
            }
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
            BandIcon::InsetDiamond => {
                use std::f32::consts::{FRAC_1_SQRT_2, SQRT_2};
                let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
                let diamond =
                    |a: f32| ((px - self.cx).abs() + (py - cyr).abs() - a) * FRAC_1_SQRT_2;
                let inner = (r - SQRT_2 * t - (1.0 + SQRT_2)).max(0.5);
                self.mark_outline(diamond(r))
                    .min(diamond(inner))
                    .max(self.mark_box(px, py))
            }
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

    /// `○`: the family's disc as a ring one stroke wide ([`Self::mark_t`]), its outer edge the disc's.
    fn mark_ring(&self, px: f32, py: f32) -> f32 {
        let ((r, cyr), t) = (self.mark_frame(), self.mark_t());
        ((px - self.cx).hypot(py - cyr) - (r - t * 0.5)).abs() - t * 0.5
    }

    /// The band one stroke wide ([`Self::mark_t`]) just inside a shape whose signed distance at the pixel is `d`: its outline, drawn as `○` is from `●`.
    fn mark_outline(&self, d: f32) -> f32 {
        let t = self.mark_t();
        (d + t * 0.5).abs() - t * 0.5
    }

    /// The family's box ([`Self::mark_frame`]): the disc's ink box, which the marks drawn in it (`◈ ⌂`) are cut to, so a corner's tip never inks a pixel the disc does not.
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
mod tests {
    use super::*;

    const M: IconMetrics = IconMetrics {
        baseline: 19.0,
        cap: 13.0,
        stem: 1.6,
        head: 3.0,
        disc: 13.0,
    };

    /// The faces the band draws at, measured (`Renderer::icon_metrics`): the system mono at 20 px (a 12 × 24 cell, regular and bold) and at 27 px (17 × 32), and a 16 × 32 cell with a capital of 19.
    const FACES: [(usize, usize, IconMetrics); 6] = [
        (
            12,
            24,
            IconMetrics {
                baseline: 20.0,
                cap: 15.0,
                stem: 1.65,
                head: 3.0,
                disc: 15.0,
            },
        ),
        (
            12,
            24,
            IconMetrics {
                baseline: 20.0,
                cap: 15.0,
                stem: 2.74,
                head: 3.0,
                disc: 15.0,
            },
        ),
        (
            17,
            32,
            IconMetrics {
                baseline: 26.0,
                cap: 20.0,
                stem: 2.24,
                head: 3.0,
                disc: 20.0,
            },
        ),
        (
            17,
            32,
            IconMetrics {
                baseline: 26.0,
                cap: 20.0,
                stem: 3.69,
                head: 3.0,
                disc: 20.0,
            },
        ),
        (
            16,
            32,
            IconMetrics {
                baseline: 25.0,
                cap: 19.0,
                stem: 3.0,
                head: 3.0,
                disc: 19.0,
            },
        ),
        (
            16,
            32,
            IconMetrics {
                baseline: 25.0,
                cap: 19.0,
                stem: 2.0,
                head: 3.0,
                disc: 19.0,
            },
        ),
    ];

    /// The ink box of a raster `w` wide: `(x0, x1, y0, y1)`, half-open.
    fn ink_box(buf: &[u8], w: usize) -> (usize, usize, usize, usize) {
        let h = buf.len() / w;
        let inked = |x: usize, y: usize| buf[y * w + x] > 0;
        let x0 = (0..w).find(|&x| (0..h).any(|y| inked(x, y))).expect("ink");
        let x1 = (0..w).rfind(|&x| (0..h).any(|y| inked(x, y))).expect("ink") + 1;
        let y0 = (0..h).find(|&y| (0..w).any(|x| inked(x, y))).expect("ink");
        let y1 = (0..h).rfind(|&y| (0..w).any(|x| inked(x, y))).expect("ink") + 1;
        (x0, x1, y0, y1)
    }

    /// An icon beside ink keeps to its own cell: no ink in its top row and, for a band's badge,
    /// none in its edge columns, at either stroke/cell parity. Cells under 8 px wide are left out:
    /// there the faces' own letters ink their edge columns too, and the band lays its badge
    /// between blanks, where it spills.
    #[test]
    fn every_icon_draws_antialiased_ink_inside_its_cell() {
        for (cw, ch, m) in [
            (12usize, 24usize, M),
            (
                8,
                17,
                IconMetrics {
                    baseline: 13.0,
                    cap: 9.0,
                    stem: 1.0,
                    head: 2.0,
                    disc: 9.0,
                },
            ),
            (
                20,
                40,
                IconMetrics {
                    baseline: 31.0,
                    cap: 22.0,
                    stem: 2.8,
                    head: 3.0,
                    disc: 22.0,
                },
            ),
            (
                12,
                24,
                IconMetrics {
                    baseline: 19.0,
                    cap: 13.0,
                    stem: 1.0,
                    head: 3.0,
                    disc: 13.0,
                },
            ),
            (
                11,
                23,
                IconMetrics {
                    baseline: 18.0,
                    cap: 12.0,
                    stem: 2.0,
                    head: 3.0,
                    disc: 12.0,
                },
            ),
        ]
        .into_iter()
        .chain(FACES)
        {
            for icon in BandIcon::ALL {
                let buf = band_icon_coverage(icon, cw, ch, m, false);
                assert_eq!(buf.len(), cw * ch);
                let ink: u32 = buf.iter().map(|&c| u32::from(c)).sum();
                assert!(ink > 255, "{icon:?} at {cw}x{ch} draws ink");
                if !matches!(icon, BandIcon::Pause | BandIcon::Alert) {
                    assert!(
                        buf.iter().any(|&c| c > 0 && c < 255),
                        "{icon:?} at {cw}x{ch} is antialiased"
                    );
                }
                for y in (0..ch).filter(|_| !icon.is_glass_mark()) {
                    assert_eq!(buf[y * cw], 0, "{icon:?} {cw}x{ch}: left column ({y})");
                    assert_eq!(
                        buf[y * cw + cw - 1],
                        0,
                        "{icon:?} {cw}x{ch} stem {}: right column ({y})",
                        m.stem
                    );
                }
                assert!(buf[..cw].iter().all(|&c| c == 0), "{icon:?}: top row clear");
            }
        }
    }

    /// An icon between blank cells is drawn at the capitals' height on a three-cell raster: a clear column at both ends, no ink above the tallest letter or below the baseline's row, the badges spilling into both blank cells, and centred on its own cell.
    #[test]
    fn a_spilled_icon_is_as_tall_as_the_capitals_and_stays_in_its_three_cells() {
        for (cw, ch, m) in FACES {
            let w = 3 * cw;
            let cap = m.cap as usize;
            let baseline = m.baseline as usize;
            for icon in BandIcon::ALL {
                let buf = band_icon_coverage(icon, cw, ch, m, true);
                assert_eq!(buf.len(), w * ch, "{icon:?}: three cells wide");
                let (x0, x1, y0, y1) = ink_box(&buf, w);
                let at = format!("{icon:?} {cw}x{ch} stem {}", m.stem);
                assert!(
                    x0 >= 1 && x1 < w,
                    "{at}: a clear column at each end ({x0}..{x1})"
                );
                assert!(
                    y0 >= m.head as usize,
                    "{at}: no ink over the tallest letter ({y0} < {})",
                    m.head
                );
                assert!(
                    y1 <= baseline + 1,
                    "{at}: no ink below the baseline's row ({y1} > {})",
                    baseline + 1
                );
                let tall = y1 - y0;
                match icon {
                    BandIcon::Dot | BandIcon::More => {}
                    BandIcon::Info | BandIcon::Update | BandIcon::Remove => {
                        assert!((cap + 1..=cap + 3).contains(&tall), "{at}: {tall} vs {cap}");
                    }
                    BandIcon::Warn => {
                        assert!((cap + 2..=cap + 5).contains(&tall), "{at}: {tall} vs {cap}");
                        assert_eq!(y1, baseline, "{at}: it stands on the baseline");
                    }
                    _ => assert!(
                        (cap * 3 / 4..=cap + 1).contains(&tall),
                        "{at}: {tall} vs {cap}"
                    ),
                }
                if matches!(icon, BandIcon::Info | BandIcon::Warn) {
                    assert!(
                        x0 < cw && x1 > 2 * cw,
                        "{at}: a badge spills into both blank cells ({x0}..{x1})"
                    );
                }
                if !matches!(
                    icon,
                    BandIcon::Success | BandIcon::Update | BandIcon::Download | BandIcon::Upload
                ) {
                    let mid = (x0 + x1) as f32 * 0.5;
                    let cell_mid = cw as f32 * 1.5;
                    assert!(
                        (mid - cell_mid).abs() <= 1.0,
                        "{at}: centred on its cell ({mid} vs {cell_mid})"
                    );
                }
            }
        }
    }

    /// The strokes follow the given stem (bold draws more ink on every stroked icon and cuts a heavier mark from the badges), and no two icons draw the same image.
    #[test]
    fn the_stroke_follows_the_stem_and_the_icons_are_distinct() {
        let regular_m = IconMetrics {
            baseline: 25.0,
            cap: 18.0,
            stem: 1.0,
            head: 3.0,
            disc: 18.0,
        };
        let bold = IconMetrics {
            stem: 2.6,
            ..regular_m
        };
        let ink = |b: &[u8]| b.iter().map(|&c| u32::from(c)).sum::<u32>();
        for spill in [false, true] {
            let mut seen: Vec<Vec<u8>> = Vec::new();
            for icon in BandIcon::ALL {
                let regular = band_icon_coverage(icon, 16, 32, regular_m, spill);
                let heavy = band_icon_coverage(icon, 16, 32, bold, spill);
                match icon {
                    BandIcon::Sparkle | BandIcon::Disc | BandIcon::Diamond => {
                        let (a, b) = (f64::from(ink(&heavy)), f64::from(ink(&regular)));
                        assert!((a - b).abs() <= 0.1 * b, "{icon:?}: {a} vs {b}");
                    }
                    BandIcon::Pause => assert!(ink(&heavy) >= ink(&regular), "{icon:?}"),
                    BandIcon::Info | BandIcon::Warn | BandIcon::Remove => {
                        assert!(ink(&heavy) < ink(&regular), "{icon:?}: a heavier cut");
                    }
                    _ => assert!(ink(&heavy) > ink(&regular), "{icon:?}: bold is heavier"),
                }
                assert!(!seen.contains(&regular), "{icon:?} repeats another icon");
                seen.push(regular);
            }
        }
    }

    /// Down a badge's centre column the `i` and the `!` are two clean cuts (fully clear rows) with a solid row between, the stem the longer cut and at least five rows, and the badge closes under the mark.
    #[test]
    fn a_badges_dot_and_stem_are_cut_apart_on_whole_rows() {
        for (cw, ch, m) in FACES {
            for icon in [BandIcon::Info, BandIcon::Warn] {
                let w = 3 * cw;
                let buf = band_icon_coverage(icon, cw, ch, m, true);
                let x = cw + cw / 2;
                let col: Vec<u8> = (0..ch).map(|y| buf[y * w + x]).collect();
                let first = col.iter().position(|&c| c > 0).expect("ink");
                let last = col.iter().rposition(|&c| c > 0).expect("ink");
                let inside = &col[first..=last];
                let mut runs = Vec::new();
                let mut y = 0;
                while y < inside.len() {
                    if inside[y] == 0 {
                        let start = y;
                        while y < inside.len() && inside[y] == 0 {
                            y += 1;
                        }
                        runs.push((start, y));
                    } else {
                        y += 1;
                    }
                }
                let at = format!("{icon:?} {cw}x{ch} stem {}", m.stem);
                assert_eq!(runs.len(), 2, "{at}: two cuts {col:?}");
                let between = &inside[runs[0].1..runs[1].0];
                assert!(
                    !between.is_empty() && between.iter().all(|&c| c == 255),
                    "{at}: a solid row between dot and stem {col:?}"
                );
                assert!(
                    inside[runs[1].1..].contains(&255),
                    "{at}: the badge closes under its mark {col:?}"
                );
                let (stem, dot) = match icon {
                    BandIcon::Info => (runs[1], runs[0]),
                    _ => (runs[0], runs[1]),
                };
                let (stem, dot) = (stem.1 - stem.0, dot.1 - dot.0);
                assert!(stem >= 5 && stem > dot, "{at}: stem {stem} rows, dot {dot}");
            }
        }
    }

    /// The glass's ring family is one set: `●`, `○`, `◐`, `◌`, `⊘` and `◉`, and the `◈`, `⌂` and
    /// `◆` drawn in their box, share one ink box on every face, inside their own cell on either
    /// raster, no taller than the face's own `●` ([`IconMetrics::disc`]) where it is under the
    /// capitals. Each mark's own shape (hollow, struck, dotted, halved, roofed) is asserted.
    #[test]
    fn the_ring_family_shares_one_outline() {
        let shorter = FACES.map(|(cw, ch, m)| {
            (
                cw,
                ch,
                IconMetrics {
                    disc: m.cap - 4.0,
                    ..m
                },
            )
        });
        for (cw, ch, m) in FACES.into_iter().chain([(12, 24, M)]).chain(shorter) {
            for spill in [false, true] {
                let w = band_icon_width(cw, spill);
                let draw = |icon| band_icon_coverage(icon, cw, ch, m, spill);
                let (disc, ring, half, dots, slashed) = (
                    draw(BandIcon::Disc),
                    draw(BandIcon::Ring),
                    draw(BandIcon::HalfDisc),
                    draw(BandIcon::DottedRing),
                    draw(BandIcon::SlashedRing),
                );
                let (eye, diamond, house, filled) = (
                    draw(BandIcon::Fisheye),
                    draw(BandIcon::InsetDiamond),
                    draw(BandIcon::House),
                    draw(BandIcon::Diamond),
                );
                let at = format!("{cw}x{ch} stem {} disc {} spill {spill}", m.stem, m.disc);
                let outline = ink_box(&disc, w);
                assert_eq!(ink_box(&ring, w), outline, "{at}: ○ is ●'s size");
                assert_eq!(ink_box(&half, w), outline, "{at}: ◐ is ●'s size");
                assert_eq!(ink_box(&dots, w), outline, "{at}: ◌ is ●'s size");
                assert_eq!(ink_box(&slashed, w), outline, "{at}: ⊘ is ●'s size");
                assert_eq!(ink_box(&eye, w), outline, "{at}: ◉ is ●'s size");
                assert_eq!(ink_box(&diamond, w), outline, "{at}: ◈ is ●'s size");
                assert_eq!(ink_box(&house, w), outline, "{at}: ⌂ is ●'s size");
                assert_eq!(ink_box(&filled, w), outline, "{at}: ◆ is ●'s size");
                let (x0, x1, y0, y1) = outline;
                let own = if spill { cw } else { 0 };
                assert!(
                    x0 >= own && x1 <= own + cw,
                    "{at}: the family keeps to its own cell ({x0}..{x1})"
                );
                if m.disc < m.cap {
                    assert!(
                        (y1 - y0) as f32 <= m.disc + 1.0,
                        "{at}: the family is no taller than the face's `●` ({outline:?})"
                    );
                    let roomy = |disc: f32| {
                        let m = IconMetrics { disc, ..m };
                        let wide = band_icon_width(2 * cw, spill);
                        let (_, _, f0, f1) = ink_box(
                            &band_icon_coverage(BandIcon::Disc, 2 * cw, ch, m, spill),
                            wide,
                        );
                        f1 - f0
                    };
                    assert!(
                        roomy(m.cap) > roomy(m.disc),
                        "{at}: control, the capitals' disc is taller"
                    );
                }
                let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
                assert_eq!(disc[cy * w + cx], 255, "{at}: ● is filled");
                assert_eq!(ring[cy * w + cx], 0, "{at}: ○ is hollow");
                assert_eq!(dots[cy * w + cx], 0, "{at}: ◌ is hollow");
                assert!(slashed[cy * w + cx] > 0, "{at}: ⊘ is struck through");
                assert!(eye[cy * w + cx] > 0, "{at}: ◉ has its dot");
                assert!(diamond[cy * w + cx] > 0, "{at}: ◈ has its dot");
                assert_eq!(house[cy * w + cx], 0, "{at}: ⌂ is hollow");
                assert!(filled[cy * w + cx] > 0, "{at}: ◆ is filled");
                for (x, y) in [(x0, y0), (x1 - 1, y0), (x0, y1 - 1), (x1 - 1, y1 - 1)] {
                    assert_eq!(filled[y * w + x], 0, "{at}: ◆'s corner ({x}, {y}) is clear");
                }
                let r = (x1 - x0) as f32 * 0.5;
                let room =
                    r - std::f32::consts::SQRT_2 * m.stem.round() >= 1.5 + std::f32::consts::SQRT_2;
                let gapped: &[(&str, &Vec<u8>)] = if room {
                    &[("◉", &eye), ("◈", &diamond)]
                } else {
                    &[("◉", &eye)]
                };
                for &(name, buf) in gapped {
                    let row = &buf[cy * w + cx..cy * w + x1];
                    let clear = row.iter().position(|&c| c == 0);
                    assert!(
                        clear.is_some_and(|k| row[k..].iter().any(|&c| c > 0)),
                        "{at}: {name}'s dot stands clear of its outline {row:?}"
                    );
                }
                assert!(
                    eye.iter().zip(&ring).all(|(&e, &o)| e >= o),
                    "{at}: ◉ is ○ and a dot"
                );
                let corner = |x: usize, y: usize| house[y * w + x];
                assert_eq!(corner(x0, y0), 0, "{at}: ⌂'s top left corner is clear");
                assert_eq!(corner(x1 - 1, y0), 0, "{at}: ⌂'s top right corner is clear");
                assert!(
                    (x0..x1).all(|x| house[(y1 - 1) * w + x] > 0),
                    "{at}: ⌂ stands on its floor"
                );
                let ink = |b: &[u8]| b.iter().map(|&c| u32::from(c)).sum::<u32>();
                assert!(ink(&dots) < ink(&ring), "{at}: ◌ has gaps");
                assert!(
                    dots.iter().zip(&ring).all(|(&d, &r)| d == 0 || r > 0),
                    "{at}: ◌'s dots lie on ○'s line"
                );
                for y in y0..y1 {
                    for x in x0..x1 {
                        let (left, right) = (x + 2 < cx, x > cx + 2);
                        let want = if left {
                            disc[y * w + x]
                        } else if right {
                            ring[y * w + x]
                        } else {
                            continue;
                        };
                        assert_eq!(half[y * w + x], want, "{at}: ◐ at ({x}, {y})");
                    }
                }
            }
        }
    }

    /// `⤓` (Land) is as tall as the capitals and stands on the baseline's row, in a cell of its own
    /// as between blanks, as the font's own glyph does beside a clock's digits (`⤓18:00`).
    #[test]
    fn the_land_arrow_keeps_the_capitals_height_in_its_own_cell() {
        for (cw, ch, m) in FACES.into_iter().chain([(12, 24, M)]) {
            let cap = m.cap.round() as usize;
            let baseline = m.baseline as usize;
            for spill in [false, true] {
                let w = band_icon_width(cw, spill);
                let buf = band_icon_coverage(BandIcon::ToBar, cw, ch, m, spill);
                let (_, _, y0, y1) = ink_box(&buf, w);
                let at = format!("{cw}x{ch} stem {} spill {spill}", m.stem);
                assert!(
                    (cap - 1..=cap + 1).contains(&(y1 - y0)),
                    "{at}: ⤓ is {} rows tall, the capitals {cap}",
                    y1 - y0
                );
                assert_eq!(y1, baseline, "{at}: ⤓ stands on the baseline");
                if !spill {
                    let band = band_icon_coverage(BandIcon::Download, cw, ch, m, spill);
                    let (_, _, b0, b1) = ink_box(&band, w);
                    assert!(b1 - b0 < y1 - y0, "{at}: control, the band's ⇣ is shorter");
                }
            }
        }
    }

    /// Windows' default face, Iosevka Term Semibold (its bold slot ExtraBold), measured in the Windows lane at 24, 32, 40 and 48 px under its default hinting (`Renderer::icon_metrics`, regular and bold): `[cell w, cell h, baseline, cap, head, disc, regular stem, bold stem]`. Its cells are half an em wide, so the capitals' half height outruns the cell's.
    const IOSEVKA_WINDOWS: [[f32; 8]; 4] = [
        [12.0, 30.0, 23.0, 18.0, 2.0, 11.0, 2.408, 2.969],
        [16.0, 40.0, 31.0, 24.0, 3.0, 14.0, 3.204, 3.969],
        [20.0, 50.0, 39.0, 29.0, 5.0, 17.0, 4.0, 4.953],
        [24.0, 60.0, 46.0, 35.0, 6.0, 21.0, 4.796, 5.953],
    ];

    /// `⤓` alone in a cell narrower than its capitals are tall takes the face's stem in whole
    /// pixels, so a bold `⤓` draws heavier, as the face's bold letters beside it do. Every other
    /// icon's stroke is held to its share of the cell's half width (the ring family's, of its
    /// radius), which here is under both stems, so each draws one raster at either weight.
    #[test]
    fn a_bold_land_arrow_takes_the_bold_stem_in_a_narrow_cell() {
        for [cw, ch, baseline, cap, head, disc, regular, bold] in IOSEVKA_WINDOWS {
            let (cw, ch) = (cw as usize, ch as usize);
            let draw = |icon, stem| {
                let m = IconMetrics {
                    baseline,
                    cap,
                    stem,
                    head,
                    disc,
                };
                band_icon_coverage(icon, cw, ch, m, false)
            };
            let stroke = |buf: &[u8]| {
                let (_, _, y0, y1) = ink_box(buf, cw);
                let row = &buf[(y0 + (y1 - y0) / 4) * cw..][..cw];
                row.iter().filter(|&&c| c == 255).count()
            };
            let ink = |b: &[u8]| b.iter().map(|&c| u32::from(c)).sum::<u32>();
            let (light, heavy) = (draw(BandIcon::ToBar, regular), draw(BandIcon::ToBar, bold));
            let at = format!("{cw}x{ch} stems {regular} and {bold}");
            assert_eq!(
                stroke(&light),
                regular.round() as usize,
                "{at}: the regular ⤓'s stroke"
            );
            assert_eq!(
                stroke(&heavy),
                bold.round() as usize,
                "{at}: the bold ⤓'s stroke"
            );
            assert!(ink(&heavy) > ink(&light), "{at}: the bold ⤓ draws heavier");
            for icon in BandIcon::ALL.into_iter().filter(|&i| i != BandIcon::ToBar) {
                assert!(
                    draw(icon, regular) == draw(icon, bold),
                    "{at}: {icon:?} keeps its stroke held to the cell's width or the ring's radius"
                );
            }
        }
    }

    /// `↻` reads as a ring: the head never fills its middle, and its top row is clear.
    #[test]
    fn the_update_ring_keeps_its_hole_open() {
        for (cw, ch, m) in FACES.into_iter().chain([(12, 24, M)]) {
            for spill in [false, true] {
                let w = band_icon_width(cw, spill);
                let buf = band_icon_coverage(BandIcon::Update, cw, ch, m, spill);
                let cy = (m.baseline - m.cap * 0.5).floor() as usize;
                let cx = if spill { cw } else { 0 } + cw / 2;
                for y in cy..=cy + 1 {
                    for x in cx - 1..=cx {
                        assert_eq!(
                            buf[y * w + x],
                            0,
                            "{cw}x{ch} stem {} spill {spill}: the ring's middle ({x}, {y}) is clear",
                            m.stem
                        );
                    }
                }
                assert!(buf[..w].iter().all(|&c| c == 0), "{cw}x{ch}: top row clear");
            }
        }
    }

    /// Small faces measured at 1x (`Renderer::icon_metrics`), where the face's tallest letter is a row or two under the cell's top: `[cell w, cell h, baseline, cap, stem, head, disc]`.
    const SMALL_FACES: [(&str, [f32; 7]); 10] = [
        ("SF Mono 13", [8.0, 15.0, 12.0, 10.0, 1.078, 1.0, 9.0]),
        ("SF Mono 11", [7.0, 13.0, 11.0, 8.0, 0.910, 2.0, 8.0]),
        ("Menlo 11", [7.0, 13.0, 10.0, 8.0, 0.988, 1.0, 7.0]),
        ("Menlo 12 bold", [7.0, 14.0, 11.0, 9.0, 1.722, 1.0, 8.0]),
        ("Menlo 14 bold", [8.0, 16.0, 13.0, 11.0, 2.004, 1.0, 9.0]),
        ("Courier New 12 bold", [7.0, 14.0, 10.0, 8.0, 1.2, 1.0, 7.0]),
        ("Andale Mono 11", [7.0, 12.0, 10.0, 8.0, 0.839, 1.0, 6.0]),
        ("Andale Mono 16", [10.0, 18.0, 15.0, 12.0, 1.224, 2.0, 8.0]),
        ("Monaco 14 bold", [8.0, 19.0, 15.0, 11.0, 2.161, 3.0, 11.0]),
        ("Menlo 10", [6.0, 12.0, 9.0, 8.0, 0.898, 0.0, 7.0]),
    ];

    /// No icon inks over the face's tallest letter, or the cell's top row, on the measured small faces, spilled or in a cell of its own.
    #[test]
    fn no_icon_inks_over_the_tallest_letter_on_the_small_faces() {
        let mut crossed = Vec::new();
        for (name, [cw, ch, baseline, cap, stem, head, disc]) in SMALL_FACES {
            let (cw, ch) = (cw as usize, ch as usize);
            let m = IconMetrics {
                baseline,
                cap,
                stem,
                head,
                disc,
            };
            let ceiling = head.max(1.0) as usize;
            for spill in [true, false] {
                let w = band_icon_width(cw, spill);
                for icon in BandIcon::ALL {
                    let (_, _, y0, _) = ink_box(&band_icon_coverage(icon, cw, ch, m, spill), w);
                    assert!(
                        y0 >= ceiling,
                        "{name} spill {spill} {icon:?}: ink on row {y0}, over row {ceiling}"
                    );
                }
                let shape = Shape::fit(cw, ch, m, spill, BandIcon::Update);
                let uncut = (0..ceiling).any(|y| {
                    (0..w).any(|x| {
                        shape.distance(BandIcon::Update, x as f32 + 0.5, y as f32 + 0.5) < 0.5
                    })
                });
                if spill && uncut {
                    crossed.push(name);
                }
            }
        }
        assert_eq!(
            crossed.len(),
            SMALL_FACES.len(),
            "control: the uncut ↻ crosses the tallest letter only on {crossed:?}"
        );
    }

    /// The faces from 8 to 10 px (1x, regular) that [`SMALL_FACES`] leaves out, measured as it
    /// is (`Renderer::icon_metrics`): `[cell w, cell h, baseline, cap, stem, head, disc]`. They
    /// stand apart because that list's control counts on every face's uncut `↻` crossing the
    /// tallest letter.
    const SMALLEST_FACES: [(&str, [f32; 7]); 5] = [
        ("SF Mono 8", [5.0, 9.0, 8.0, 6.0, 0.663, 1.0, 6.0]),
        ("SF Mono 9", [6.0, 11.0, 9.0, 7.0, 0.745, 1.0, 6.0]),
        ("SF Mono 10", [6.0, 12.0, 10.0, 8.0, 0.827, 1.0, 7.0]),
        ("Menlo 8", [5.0, 9.0, 7.0, 6.0, 0.722, 0.0, 6.0]),
        ("Menlo 9", [5.0, 10.0, 8.0, 7.0, 0.808, 0.0, 6.0]),
    ];

    /// On the small faces `⏸` and `!` between blanks are `[3cap/4, cap + 1]` rows and `⤓` is the
    /// capitals' height (a row either way) standing on the baseline, spilled and in a cell of
    /// its own, all under the tallest letter: none of them keeps the pixel and a half
    /// [`Shape::half`] leaves under the ceiling. Control: drawn with that margin kept, they fall
    /// short where the ceiling is the capitals' own top row (Menlo from 8 to 10 px), and `⏸` on
    /// Andale Mono at 16 too.
    #[test]
    fn the_bars_and_the_land_arrow_keep_the_capitals_height_on_the_small_faces() {
        let mut short = Vec::new();
        for (name, [cw, ch, baseline, cap, stem, head, disc]) in
            SMALLEST_FACES.into_iter().chain(SMALL_FACES)
        {
            let (cw, ch) = (cw as usize, ch as usize);
            let m = IconMetrics {
                baseline,
                cap,
                stem,
                head,
                disc,
            };
            let (ceiling, rows, base) = (head.max(1.0) as usize, cap as usize, baseline as usize);
            let fits = |icon: BandIcon, (_, _, y0, y1): (usize, usize, usize, usize)| {
                let (lo, hi) = if icon == BandIcon::ToBar {
                    (rows - 1, rows + 1)
                } else {
                    (rows * 3 / 4, rows + 1)
                };
                y0 >= ceiling
                    && (lo..=hi).contains(&(y1 - y0))
                    && (icon != BandIcon::ToBar || y1 == base)
            };
            for icon in [BandIcon::Pause, BandIcon::Alert, BandIcon::ToBar] {
                let spills: &[bool] = if icon == BandIcon::ToBar {
                    &[true, false]
                } else {
                    &[true]
                };
                let mut fell = false;
                for &spill in spills {
                    let w = band_icon_width(cw, spill);
                    let shape = Shape::fit(cw, ch, m, spill, icon);
                    let kept = Shape {
                        flat: shape.half,
                        tall: (m.cap * 0.5)
                            .min(shape.cy - shape.top - 1.5)
                            .max(shape.half),
                        ..shape
                    };
                    let draw = |fitted: &Shape| -> Vec<u8> {
                        (0..w * ch)
                            .map(|i| {
                                let (px, py) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
                                let d = fitted.distance(icon, px, py).max(fitted.top - py);
                                ((0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8
                            })
                            .collect()
                    };
                    let buf = band_icon_coverage(icon, cw, ch, m, spill);
                    let at = format!("{name} spill {spill} {icon:?}");
                    assert_eq!(
                        draw(&shape),
                        buf,
                        "{at}: the control draws as the band does"
                    );
                    let (now, then) = (ink_box(&buf, w), ink_box(&draw(&kept), w));
                    assert!(
                        fits(icon, now),
                        "{at}: ink box {now:?}, capitals {cap} rows tall on row {baseline}, the \
                         tallest letter on row {head}"
                    );
                    fell |= !fits(icon, then);
                }
                if fell {
                    short.push((name, icon));
                }
            }
        }
        assert_eq!(
            short,
            [
                ("Menlo 8", BandIcon::Pause),
                ("Menlo 8", BandIcon::Alert),
                ("Menlo 8", BandIcon::ToBar),
                ("Menlo 9", BandIcon::Pause),
                ("Menlo 9", BandIcon::Alert),
                ("Menlo 9", BandIcon::ToBar),
                ("Andale Mono 16", BandIcon::Pause),
                ("Menlo 10", BandIcon::Pause),
                ("Menlo 10", BandIcon::Alert),
                ("Menlo 10", BandIcon::ToBar),
            ],
            "control: drawn with the margin kept, these fall short"
        );
    }

    /// A `●` well under the capitals never closes `○` or `⊘`: the family is never under three fifths of the capitals, in whole pixels (Andale Mono's bold `●` at 16 px keeps its 7).
    #[test]
    fn a_small_disc_never_closes_the_rings() {
        let cells = [
            (
                14,
                30,
                IconMetrics {
                    baseline: 24.0,
                    cap: 18.0,
                    stem: 2.0,
                    head: 3.0,
                    disc: 18.0,
                },
            ),
            (
                7,
                15,
                IconMetrics {
                    baseline: 12.0,
                    cap: 9.0,
                    stem: 1.0,
                    head: 2.0,
                    disc: 9.0,
                },
            ),
        ];
        for (cw, ch, face) in cells {
            let w = band_icon_width(cw, true);
            for share in [0.5f32, 0.39, 0.25] {
                let m = IconMetrics {
                    disc: (face.cap * share).round(),
                    ..face
                };
                let at = format!("{cw}x{ch} cap {} disc {}", m.cap, m.disc);
                let draw = |icon| band_icon_coverage(icon, cw, ch, m, true);
                let (disc, ring, slashed) = (
                    draw(BandIcon::Disc),
                    draw(BandIcon::Ring),
                    draw(BandIcon::SlashedRing),
                );
                let (x0, x1, y0, y1) = ink_box(&disc, w);
                let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
                let clear = |b: &[u8], side: fn(isize) -> bool| {
                    (y0..y1)
                        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                        .filter(|&(x, y)| {
                            let diag = (x as isize - cx as isize) + (y as isize - cy as isize);
                            disc[y * w + x] == 255 && b[y * w + x] == 0 && side(diag)
                        })
                        .count()
                };
                let hollow = clear(&ring, |_| true);
                assert!(hollow >= 5, "{at}: ○'s hollow is {hollow} pixels");
                if m.cap >= 18.0 {
                    let (above, below) = (clear(&slashed, |d| d < 0), clear(&slashed, |d| d > 0));
                    assert!(
                        above > 0 && below > 0,
                        "{at}: ⊘ clears {above} pixels above its slash, {below} below"
                    );
                }
                assert!(
                    (y1 - y0) as f32 >= (m.cap * 0.6).floor(),
                    "{at}: the family is {} rows, under three fifths of the capitals",
                    y1 - y0
                );
            }
        }
        let (cw, ch, face) = cells[0];
        let rows = |disc: f32| {
            let m = IconMetrics { disc, ..face };
            let (_, _, y0, y1) =
                ink_box(&band_icon_coverage(BandIcon::Disc, cw, ch, m, true), cw * 3);
            y1 - y0
        };
        assert!(
            rows(11.0) < rows(12.0),
            "control: {} and {} rows",
            rows(11.0),
            rows(12.0)
        );
        let andale = IconMetrics {
            baseline: 15.0,
            cap: 12.0,
            stem: 2.188,
            head: 2.0,
            disc: 7.0,
        };
        let (_, _, y0, y1) = ink_box(
            &band_icon_coverage(BandIcon::Disc, 10, 18, andale, true),
            30,
        );
        assert!(
            (y1 - y0) as f32 <= andale.disc,
            "Andale Mono 16 px bold: a `●` of 7 draws the family {} rows",
            y1 - y0
        );
    }

    /// Bold faces whose `●` is well under their capitals, measured at 1x: `[cell w, cell h, baseline, cap, stem, head, disc]`, and whether a ring the face's stem wide, unheld, in the drawn `●` has no clear pixel.
    const BOLD_FACES: [(&str, [f32; 7], bool); 10] = [
        ("Menlo 11", [7.0, 13.0, 10.0, 8.0, 1.576, 1.0, 7.0], false),
        (
            "Andale Mono 13",
            [8.0, 15.0, 12.0, 9.0, 1.718, 2.0, 7.0],
            false,
        ),
        (
            "Andale Mono 14",
            [8.0, 16.0, 13.0, 10.0, 1.659, 2.0, 7.0],
            false,
        ),
        (
            "Andale Mono 15",
            [9.0, 17.0, 14.0, 11.0, 2.063, 2.0, 7.0],
            false,
        ),
        (
            "Andale Mono 27",
            [16.0, 30.0, 24.0, 19.0, 4.055, 3.0, 13.0],
            false,
        ),
        ("PT Mono 12", [7.0, 13.0, 10.0, 8.0, 1.561, 0.0, 6.0], true),
        (
            "PT Mono 14",
            [8.0, 16.0, 13.0, 10.0, 1.820, 1.0, 7.0],
            false,
        ),
        (
            "PT Mono 16",
            [10.0, 18.0, 14.0, 11.0, 2.078, 1.0, 7.0],
            false,
        ),
        (
            "Courier New 15",
            [9.0, 17.0, 12.0, 9.0, 1.502, 1.0, 7.0],
            false,
        ),
        ("SF Mono 12", [7.0, 14.0, 12.0, 9.0, 1.639, 1.0, 9.0], false),
    ];

    /// A bold `○` keeps at least four clear pixels inside `●` on each measured face, spilled and in a cell of its own.
    #[test]
    fn a_bold_ring_stays_open_on_the_measured_faces() {
        let hollow = |disc: &[u8], ring: &[u8], w: usize| {
            let (x0, x1, y0, y1) = ink_box(disc, w);
            (y0..y1)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .filter(|&(x, y)| disc[y * w + x] == 255 && ring[y * w + x] == 0)
                .count()
        };
        for (name, [cw, ch, baseline, cap, stem, head, dot], closes) in BOLD_FACES {
            let (cw, ch) = (cw as usize, ch as usize);
            let m = IconMetrics {
                baseline,
                cap,
                stem,
                head,
                disc: dot,
            };
            for spill in [true, false] {
                let w = band_icon_width(cw, spill);
                let disc = band_icon_coverage(BandIcon::Disc, cw, ch, m, spill);
                let ring = band_icon_coverage(BandIcon::Ring, cw, ch, m, spill);
                let open = hollow(&disc, &ring, w);
                assert!(
                    open >= 4,
                    "{name} bold spill {spill}: ○ keeps {open} clear pixels inside ●"
                );
                if spill && closes {
                    let (x0, x1, y0, y1) = ink_box(&disc, w);
                    let (cx, cy, r) = (
                        (x0 + x1) as f32 * 0.5,
                        (y0 + y1) as f32 * 0.5,
                        (x1 - x0) as f32 * 0.5,
                    );
                    let t = m.stem.round();
                    let unheld: Vec<u8> = (0..disc.len())
                        .map(|i| {
                            let (px, py) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
                            let d = ((px - cx).hypot(py - cy) - (r - t * 0.5)).abs() - t * 0.5;
                            ((0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8
                        })
                        .collect();
                    assert_eq!(
                        hollow(&disc, &unheld, w),
                        0,
                        "{name}: control, a ring the face's stem wide closes"
                    );
                }
            }
        }
        let (cw, ch) = (15, 28);
        let face = |stem: f32| IconMetrics {
            baseline: 23.0,
            cap: 17.0,
            stem,
            head: 3.0,
            disc: 15.0,
        };
        let weight = |stem: f32| {
            let ring = band_icon_coverage(BandIcon::Ring, cw, ch, face(stem), true);
            let w = band_icon_width(cw, true);
            let (x0, _, y0, y1) = ink_box(&ring, w);
            let row = &ring[(y0 + y1) / 2 * w..][..w];
            row[x0..].iter().take_while(|&&c| c >= 128).count()
        };
        assert_eq!(
            weight(3.278),
            3,
            "control: SF Mono 24 px bold ○ is its stem"
        );
        assert_eq!(
            weight(1.988),
            2,
            "control: SF Mono 24 px regular ○ is its stem"
        );
    }

    /// The deck's pictograms ([`BandIcon::is_pictogram`]) at the sizes they are drawn: on the grid's faces in a cell of their own and between blanks (the band-icons scene), between blanks on the small faces measured at 1x ([`SMALL_FACES`]), and alone in a chrome surface's square box ([`boxed_icon_metrics`]) of 11 to 13 px at 1x and 22 to 26 at 2x (the deck's need pills), each as `(where, raster width, raster, icon)`.
    fn pictogram_rasters() -> Vec<(String, usize, Vec<u8>, BandIcon)> {
        let mut drawn = Vec::new();
        let pictograms = || BandIcon::ALL.into_iter().filter(|i| i.is_pictogram());
        for (cw, ch, m) in FACES.into_iter().chain([(12, 24, M)]) {
            for spill in [false, true] {
                for icon in pictograms() {
                    drawn.push((
                        format!("{cw}x{ch} stem {} spill {spill}", m.stem),
                        band_icon_width(cw, spill),
                        band_icon_coverage(icon, cw, ch, m, spill),
                        icon,
                    ));
                }
            }
        }
        for (name, [cw, ch, baseline, cap, stem, head, disc]) in SMALL_FACES {
            let (cw, ch) = (cw as usize, ch as usize);
            let m = IconMetrics {
                baseline,
                cap,
                stem,
                head,
                disc,
            };
            for icon in pictograms() {
                drawn.push((
                    format!("{name} spilled"),
                    band_icon_width(cw, true),
                    band_icon_coverage(icon, cw, ch, m, true),
                    icon,
                ));
            }
        }
        for side in (11..=13).chain(22..=26) {
            for icon in pictograms() {
                drawn.push((
                    format!("a {side} px box"),
                    side,
                    band_icon_coverage(icon, side, side, boxed_icon_metrics(side), false),
                    icon,
                ));
            }
        }
        drawn
    }

    /// The pixels of a raster `w` wide under a quarter's coverage that no path of such pixels (side by side or one over another) joins to the raster's edge: the clear its ink closes in.
    fn holes(buf: &[u8], w: usize) -> Vec<(usize, usize)> {
        let h = buf.len() / w;
        let clear = |i: usize| buf[i] < 64;
        let mut open = vec![false; buf.len()];
        let mut todo: Vec<usize> = (0..buf.len())
            .filter(|&i| {
                let (x, y) = (i % w, i / w);
                clear(i) && (x == 0 || y == 0 || x + 1 == w || y + 1 == h)
            })
            .collect();
        for &i in &todo {
            open[i] = true;
        }
        while let Some(i) = todo.pop() {
            let (x, y) = (i % w, i / w);
            let next = [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ];
            for (nx, ny) in next {
                if nx < w && ny < h && clear(ny * w + nx) && !open[ny * w + nx] {
                    open[ny * w + nx] = true;
                    todo.push(ny * w + nx);
                }
            }
        }
        (0..buf.len())
            .filter(|&i| clear(i) && !open[i])
            .map(|i| (i % w, i / w))
            .collect()
    }

    /// THE PICTOGRAMS KEEP THEIR PARTS APART: no part smears into another at any size they are drawn ([`pictogram_rasters`]), so each keeps the hole its picture has, a clear its own ink closes in. The padlock's is under its shackle, over a body solid across its middle half in its lower third; the hourglass's is in its upper bulb, with more ink in the lower half, where the sand has run; the drum's is under its solid top, which puts more ink in its upper half.
    #[test]
    fn the_pictograms_keep_their_parts_apart() {
        for (at, w, buf, icon) in pictogram_rasters() {
            let at = format!("{icon:?} {at}");
            let (x0, x1, y0, y1) = ink_box(&buf, w);
            let mid = (y0 + y1) as f32 * 0.5;
            let holes = holes(&buf, w);
            let upper = |&(_, y): &(usize, usize)| (y as f32 + 0.5) < mid;
            let ink = |rows: std::ops::Range<usize>| -> u32 {
                rows.flat_map(|y| buf[y * w..(y + 1) * w].iter())
                    .map(|&c| u32::from(c))
                    .sum()
            };
            let (top, foot) = (ink(y0..(y0 + y1) / 2), ink((y0 + y1).div_ceil(2)..y1));
            match icon {
                BandIcon::Lock => {
                    assert!(
                        !holes.is_empty() && holes.iter().all(upper),
                        "{at}: a hole, under the shackle {holes:?}"
                    );
                    let (body, side) = (y1 - (y1 - y0) / 3, (x1 - x0) / 4);
                    for y in body..y1 {
                        assert!(
                            (x0 + side..x1 - side).all(|x| buf[y * w + x] == 255),
                            "{at}: the body is solid on row {y}"
                        );
                    }
                }
                BandIcon::Hourglass => {
                    assert!(holes.iter().any(upper), "{at}: the upper bulb is hollow");
                    assert!(foot > top, "{at}: the sand has run down ({foot} vs {top})");
                }
                _ => {
                    assert!(
                        holes.iter().any(|h| !upper(h)),
                        "{at}: the drum is hollow under its top {holes:?}"
                    );
                    assert!(top > foot, "{at}: the top is solid ({top} vs {foot})");
                }
            }
        }
    }

    /// A pictogram in a chrome surface's box keeps to its square and draws at least five rows tall from 11 px: its ink stays a pixel clear of the box's sides and inside the box's rows, and it is centred on the box.
    #[test]
    fn a_boxed_pictogram_keeps_to_its_square() {
        for side in (11..=13).chain(22..=26) {
            for icon in BandIcon::ALL.into_iter().filter(|i| i.is_pictogram()) {
                let buf = band_icon_coverage(icon, side, side, boxed_icon_metrics(side), false);
                let (x0, x1, y0, y1) = ink_box(&buf, side);
                let at = format!("{icon:?} in a {side} px box");
                assert!(
                    x0 >= 1 && x1 < side,
                    "{at}: a clear column each side ({x0}..{x1})"
                );
                assert!(y0 >= 1 && y1 <= side, "{at}: inside the rows ({y0}..{y1})");
                assert!(y1 - y0 >= 5, "{at}: {} rows tall", y1 - y0);
                assert!(
                    (x0 + x1).abs_diff(side) <= 1,
                    "{at}: centred ({x0}..{x1} in {side})"
                );
            }
        }
    }
}

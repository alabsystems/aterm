// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BAND'S DRAWN ICONS (docs/DESIGN-unified-messages-2026-09-21.md,
//! rulings 251 and 258): one vector icon for every status glyph a message
//! row can carry, drawn from the face's own capital height and stroke weight.
//!
//! The band's glyph cell used to take its `⚠ ✕ ✓ ℹ ↻` from whichever face
//! held them — a symbol font for one, the fallback for another, the primary
//! for a third — so a row of them read as four type designs at four weights.
//! The owner chose one drawn set ("Drawn icons, drop spinner", 2026-09-25).
//! The row's cell keeps its CHARACTER (the text grid, a screen reader, copy
//! and the `messages` verb all read it); only the pixels of its glyph are
//! drawn here, and only on a band row that asks
//! ([`aterm_core::render::ChromeRaster::icons`]).
//!
//! # Geometry
//!
//! Every icon is a union of signed-distance primitives — strokes, rings,
//! discs, pixel-row bars and filled polygons — around a centre on the middle
//! of the face's capital height, its height the capital height (ruling 258):
//! the round icons overshoot it by about a pixel as a round letter does, the
//! warning triangle stands on the baseline and reaches two pixels over it, the
//! marks that sit on a baseline sit on the face's. An icon whose neighbour
//! cells are BLANK (the band's ` G TITLE`) draws on a raster three cells wide
//! and spills symmetrically into them — never outside the row, never over
//! the title, never below the baseline's row (so the seam's descender
//! ink-skip never sees it); an icon beside ink stays inside its own cell
//! with a clear column on both sides, scaled down to fit (ruling 257).
//!
//! Strokes are the face's measured stem ([`IconMetrics::stem`], the regular
//! or bold `l`) rounded to a whole pixel, and the icon's centre is placed so
//! a vertical stroke's edges fall on pixel boundaries; the marks that must be
//! read (the `i`, the `!`, the pause bars) are cut on whole pixel rows, so
//! the icon has the weight of the words beside it and its stems are crisp.
//! Coverage is the analytic box filter of the distance at each pixel centre,
//! `clamp(½ − d)`: 256 levels on every slope, the same image on the CPU and
//! in the GPU atlas (a Mono glyph through the one cache).

use aterm_core::render::BandIcon;

/// What an icon is fitted to: the face's baseline and capital height in the
/// cell (device pixels from the cell's top) and its stem width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconMetrics {
    /// Pixels from the cell's top to the baseline.
    pub baseline: f32,
    /// The capital height, in pixels.
    pub cap: f32,
    /// The stroke weight, in pixels (the face's `l` stem at the cell's
    /// weight).
    pub stem: f32,
    /// Pixels from the cell's top to the highest ink of the face's printable
    /// ASCII, regular and bold (`Renderer::chrome_room_for`'s head): no icon
    /// reaches above it, so a row lifted clear of its rail (ruling 248),
    /// which clips its glyphs at the row's top, never clips an icon.
    pub head: f32,
}

/// The width of an icon's raster: its own cell, or with `spill` the blank
/// cell on each side too (the icon's own cell is then the middle third).
#[must_use]
pub const fn band_icon_width(cell_w: usize, spill: bool) -> usize {
    if spill { cell_w * 3 } else { cell_w }
}

/// The coverage bitmap of `icon` for a `cell_w × cell_h` cell: row-major
/// [`band_icon_width`]` × cell_h` bytes of 8-bit coverage, tinted by the
/// cell's ink at blit time like any Mono glyph. With `spill` the raster
/// starts one cell left of the icon's own and is three cells wide; without,
/// it is the cell. Empty for a degenerate cell.
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
    let shape = Shape::fit(cell_w, cell_h, m, spill);
    for y in 0..cell_h {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = shape.distance(icon, px, py);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            buf[y * w + x] = (cov * 255.0).round() as u8;
        }
    }
    buf
}

/// One icon's frame on its raster: its centre, half height, stroke, the
/// horizontal room it may use on each side of the centre, the row it stands
/// on and the highest row it may ink.
struct Shape {
    cx: f32,
    cy: f32,
    half: f32,
    t: f32,
    room: f32,
    bot: f32,
    top: f32,
}

impl Shape {
    /// The frame for a cell: centred on the cell's width and on the middle
    /// of the capital height, its half height half the capital height where
    /// the room allows (the whole row's height above the centre and, without
    /// `spill`, the cell's width less a clear column each side).
    fn fit(cell_w: usize, cell_h: usize, m: IconMetrics, spill: bool) -> Self {
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
        // The highest row an icon may ink: the face's tallest letter, and
        // never the cell's top row.
        let top = if m.head.is_finite() {
            m.head.clamp(1.0, fh)
        } else {
            1.0
        };
        let (ox, wide) = if spill { (fw, 3.0 * fw) } else { (0.0, fw) };
        let cy = (baseline - cap * 0.5).max(1.0);
        // A round icon overshoots its half height by up to a pixel and the
        // ring's arrowhead by another half: both stay under `top`.
        let half0 = (cap * 0.5)
            .min(cy - top - 1.5)
            .min(wide * 0.5 - 1.5)
            .max(1.0);
        // A whole-pixel stroke, at most about a fifth of the icon, and a
        // centre that puts a vertical stroke's edges on pixel boundaries (a
        // pixel's middle for an odd stroke, an edge for an even one).
        let t = stem.round().clamp(1.0, (half0 * 0.44).round().max(1.0));
        let cx = ox
            + if (t as u32) % 2 == 1 {
                (fw * 0.5).floor() + 0.5
            } else {
                (fw * 0.5).round()
            };
        // A crisp centre in a cell of the other parity sits half a pixel off
        // the cell's middle: the room is measured from where it is, so a
        // clear column stays on both sides (ruling 257). A round icon
        // overshoots its half height by up to a pixel; the shapes wider than
        // their height (the triangle, the tick, the cross, the tray) narrow
        // themselves to the room.
        let room = (cx.min(wide - cx) - 1.0).max(1.0);
        let half = half0.min(room - 0.5).max(1.0);
        Shape {
            cx,
            cy,
            half,
            t,
            room,
            bot: cy + half,
            top,
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

    /// A mark one stroke wide on the centre line, from pixel row `y0` to
    /// `y1` (whole rows, so its ends are crisp).
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

    /// A round icon's outer radius and centre row: a pixel over the capital
    /// height (a round letter's overshoot), its sides and its top on pixel
    /// boundaries, its bottom no lower than the baseline's row.
    fn ring_frame(&self) -> (f32, f32) {
        let frac = self.cx.fract();
        let r = ((self.half + 0.5 - frac).round() + frac).min(self.room);
        let top = (self.cy - r).floor();
        (r, top + r)
    }

    /// The warning triangle: its apex row, its base row (the baseline, where
    /// the icon stands on it), its half base and the rounding of its corners.
    /// It is `2.4` half heights tall where its rounded tip (about the
    /// rounding below the sharp apex) stays under the face's tallest letter.
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
                // A disc a pixel over the capital height with a bold `i` cut
                // out of it on whole rows — a clear band, the dot, one clear
                // row, the stem, a clear band — the `i` nearly two thirds of
                // the disc (ruling 258). At the band's bold stem a RING would
                // spend six of the badge's seventeen rows on its own stroke
                // and leave the `i` a stub unless it grew four pixels past
                // the capitals; the disc gives the `i` the whole badge, as
                // the warning triangle gives its `!`.
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
                // A tick whose round caps stay inside the room: in a cell of
                // its own a heavy stroke draws a slightly smaller tick rather
                // than reaching the cell's edge column.
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
                // A filled triangle standing on the baseline and reaching
                // about three pixels over the capitals (a point reads smaller
                // than a flat top), its corners softened, with `!` cut out on
                // whole rows from the base up: a band of the badge under the
                // dot, the dot, one clear row, then the stem up to where the
                // triangle is wide enough to keep a clear pixel each side of
                // it. At the bold stem an OUTLINE's inside held a `!` of four
                // rows; the cut-out `!` is ten (ruling 258).
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
                // An arrow down onto a tray; the tray's lower edge on the
                // baseline, the stem's top on the capital line.
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
                // An open ring, its gap at the top right; the end at the top
                // carries a small filled head pointing clockwise, into the
                // gap. The head grows FORWARD from the arc's end and no wider
                // than the ring's inside can spare, so the ring's hole stays
                // open (ruling 253).
                let (ro, cyr) = self.ring_frame();
                let r = (ro - t * 0.5) * 0.92;
                let (ox, oy) = (self.cx, cyr);
                let (dx, dy) = (px - ox, py - oy);
                let ang = dy.atan2(dx).to_degrees();
                // The arc runs clockwise from -15° through the bottom to
                // -100° (260°): the gap is (-100°, -15°).
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
                // Clockwise tangent at the end (y down): (-sin, cos).
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
                // Two bars a whole number of pixels wide on whole rows, a
                // clear gap between whose parity follows the centre's, so
                // each bar's edges fall on pixel boundaries.
                let w = t.max((self.half * 0.5).round());
                let mut gap = (self.half * 0.4).round().max(2.0);
                if (self.cx - gap * 0.5).fract() != 0.0 {
                    gap += 1.0;
                }
                let (y0, y1) = (
                    (self.cy - 0.8 * self.half).round(),
                    (self.cy + 0.8 * self.half).round(),
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
                // A `!` on whole rows: the stem from the capital line, one
                // clear row, the dot on the baseline.
                let bottom = self.bot.round();
                let top = (self.cy - self.half).round();
                let dh = t.max((self.half * 0.26).round());
                let gap = (self.half * 0.2).round().max(1.0);
                self.bar(px, py, top, bottom - dh - gap)
                    .min(self.bar(px, py, bottom - dh, bottom))
            }
            BandIcon::Dot => self.p_disc(px, py, 0.0, (t * 0.85).max(1.0)),
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

    /// A disc of radius `r` pixels on the centre line, `v` half heights
    /// below the centre.
    fn p_disc(&self, px: f32, py: f32, v: f32, r: f32) -> f32 {
        let (x, y) = self.p(0.0, v);
        (px - x).hypot(py - y) - r
    }
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

/// The signed distance to the isosceles triangle with its apex at
/// `(cx, apex)` and its base `cx ± wb` on row `bottom`, its corners rounded
/// by `round` pixels while its edges stay where they are: the triangle
/// shrunk about its incentre by `round`, then grown back by it.
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

/// The signed distance from `(px, py)` to a closed polygon (negative
/// inside), any winding, convex or concave — the even-odd crossing test for
/// the sign, the nearest edge for the magnitude.
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
    };

    /// The faces the band draws at, measured (`Renderer::icon_metrics`): the
    /// system mono at 20 px (a 12 × 24 cell, regular and bold) and at 27 px
    /// (17 × 32), and a 16 × 32 cell with a capital of 19.
    const FACES: [(usize, usize, IconMetrics); 6] = [
        (
            12,
            24,
            IconMetrics {
                baseline: 20.0,
                cap: 15.0,
                stem: 1.65,
                head: 3.0,
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

    /// An icon BESIDE INK keeps to its own cell (ruling 257): it draws ink
    /// inside it, and none touches the cell's left or right column or its
    /// top row — an icon never bleeds into a neighbouring cell — whichever
    /// parity the stroke and the cell have. Every icon but the two drawn on
    /// whole pixels (the pause bars, the `!`) is antialiased.
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
                },
            ),
            // The two parity mismatches: an odd stroke in an even cell, an
            // even stroke in an odd one (the centre sits half a pixel off).
            (
                12,
                24,
                IconMetrics {
                    baseline: 19.0,
                    cap: 13.0,
                    stem: 1.0,
                    head: 3.0,
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
                for y in 0..ch {
                    assert_eq!(buf[y * cw], 0, "{icon:?} {cw}x{ch}: left column ({y})");
                    assert_eq!(
                        buf[y * cw + cw - 1],
                        0,
                        "{icon:?} {cw}x{ch} stem {}: right column ({y})",
                        m.stem
                    );
                }
                // Glyph-sized: the ink box stays inside the cell's height.
                assert!(buf[..cw].iter().all(|&c| c == 0), "{icon:?}: top row clear");
            }
        }
    }

    /// An icon between BLANK cells is drawn at the words' size (ruling 258):
    /// on a raster three cells wide it keeps a clear column at both ends (so
    /// it never reaches the title's cell or past the blank on its left),
    /// never inks over the face's tallest letter nor a row below the
    /// baseline's (so a lifted rail row never clips it and the seam's
    /// descender ink-skip never sees it), and stands as tall as the capitals
    /// — the badges a pixel or more over them, spilling into the blank cells
    /// — centred on its own cell.
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
                    BandIcon::Info | BandIcon::Update => {
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
                    // Symmetric about the stroke's crisp centre, which is at
                    // most half a pixel off the cell's middle.
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

    /// The icons are ONE set: their strokes are the stem they are given — a
    /// bold stem draws more ink than a regular one on every stroked icon (and
    /// cuts a heavier mark out of the two badges) — and two different icons
    /// never draw the same image, spilled or in a cell of their own.
    #[test]
    fn the_stroke_follows_the_stem_and_the_icons_are_distinct() {
        // A 16 × 32 cell: room for a 3-pixel bold stroke beside a 1-pixel
        // regular one. Both odd, so both icons take the same crisp centre
        // and the same size.
        let regular_m = IconMetrics {
            baseline: 25.0,
            cap: 18.0,
            stem: 1.0,
            head: 3.0,
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
                    // A filled star has no stroke: the same shape.
                    BandIcon::Sparkle => {
                        let (a, b) = (f64::from(ink(&heavy)), f64::from(ink(&regular)));
                        assert!((a - b).abs() <= 0.1 * b, "{icon:?}: {a} vs {b}");
                    }
                    // The bars are at least half the half height wide: a
                    // stem under that leaves them as they are.
                    BandIcon::Pause => assert!(ink(&heavy) >= ink(&regular), "{icon:?}"),
                    // A badge's mark is CUT OUT at the stem: a bold mark cuts more.
                    BandIcon::Info | BandIcon::Warn => {
                        assert!(ink(&heavy) < ink(&regular), "{icon:?}: a heavier cut");
                    }
                    _ => assert!(ink(&heavy) > ink(&regular), "{icon:?}: bold is heavier"),
                }
                assert!(!seen.contains(&regular), "{icon:?} repeats another icon");
                seen.push(regular);
            }
        }
    }

    /// The badges' marks are legible at the band's size: down the badge's
    /// centre column the `i` and the `!` are two CLEAN cuts (fully clear
    /// rows) with a solid row between them — never a half-covered gap that
    /// blurs the dot into the stem (ruling 253) — the stem the longer cut and
    /// at least five rows (ruling 258: the round-13 `!` was two), and the
    /// badge closes under the mark.
    #[test]
    fn a_badges_dot_and_stem_are_cut_apart_on_whole_rows() {
        for (cw, ch, m) in FACES {
            for icon in [BandIcon::Info, BandIcon::Warn] {
                let w = 3 * cw;
                let buf = band_icon_coverage(icon, cw, ch, m, true);
                // The cell's middle column: inside the cut for every stroke
                // width and either crisp centre.
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
                // `i`: dot above the stem; `!`: stem above the dot.
                let (stem, dot) = match icon {
                    BandIcon::Info => (runs[1], runs[0]),
                    _ => (runs[0], runs[1]),
                };
                let (stem, dot) = (stem.1 - stem.0, dot.1 - dot.0);
                assert!(stem >= 5 && stem > dot, "{at}: stem {stem} rows, dot {dot}");
            }
        }
    }

    /// `↻` reads as a RING at the band's size: its middle is clear, so the
    /// head never fills the hole (the round-13 captures showed a blob at a
    /// 12 × 24 cell with a 2 px stem), and its top row stays clear.
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

    /// Deterministic: the same inputs give the same bytes (the CPU blit and
    /// the GPU atlas upload one cached raster, and a re-raster after a cache
    /// eviction must match it).
    #[test]
    fn a_raster_is_a_pure_function_of_its_inputs() {
        for icon in BandIcon::ALL {
            for spill in [false, true] {
                assert_eq!(
                    band_icon_coverage(icon, 11, 23, M, spill),
                    band_icon_coverage(icon, 11, 23, M, spill)
                );
            }
        }
    }
}

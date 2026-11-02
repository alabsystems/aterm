// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scanline signed-area coverage rasterizer for outline paths.

/// Point in rasterizer grid space (`x` right, `y` down, in cells).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Point {
    /// Horizontal position in cells.
    pub x: f32,
    /// Vertical position in cells (increasing downward).
    pub y: f32,
}

/// Constructs a [`Point`].
#[inline]
pub const fn point(x: f32, y: f32) -> Point {
    Point { x, y }
}

/// Linear interpolation between two points at parameter `t`.
#[inline]
const fn lerp(t: f32, p0: Point, p1: Point) -> Point {
    point(p0.x + t * (p1.x - p0.x), p0.y + t * (p1.y - p0.y))
}

/// Maximum sagitta error in grid cells when flattening curves.
pub const FLATTEN_SAGITTA_PX: f32 = 1.0 / 255.0;

const ONE_LINE_DEVSQ: f32 = (4.0 * FLATTEN_SAGITTA_PX) * (4.0 * FLATTEN_SAGITTA_PX);

const INV_FOUR_SAGITTA: f32 = 1.0 / (4.0 * FLATTEN_SAGITTA_PX);

const MAX_FLATTEN_SEGMENTS: usize = 4096;

/// Signed-area coverage accumulation grid of `width` by `height` cells.
pub struct Rasterizer {
    width: usize,
    height: usize,
    /// `width * height` accumulators plus a short tail of slack, so a span that
    /// ends exactly at the right edge has somewhere to deposit its final cell
    /// without a bounds check on the hot path.
    a: Vec<f32>,
}

impl Rasterizer {
    /// Creates a cleared `width` by `height` rasterizer grid.
    #[inline]
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            a: vec![0.0; width.saturating_mul(height).saturating_add(4)],
        }
    }

    /// Deposit `v` at flat index `i`, dropping writes that escape the buffer.
    #[inline]
    fn add(&mut self, i: i64, v: f32) {
        if let Ok(idx) = usize::try_from(i)
            && let Some(slot) = self.a.get_mut(idx)
        {
            *slot += v;
        }
    }

    /// Accumulates a straight directed edge from `p0` to `p1`.
    pub fn draw_line(&mut self, p0: Point, p1: Point) {
        if !p0.x.is_finite()
            || !p0.y.is_finite()
            || !p1.x.is_finite()
            || !p1.y.is_finite()
            || (p0.y - p1.y).abs() <= f32::EPSILON
        {
            return;
        }
        let (dir, p0, p1) = if p0.y < p1.y {
            (1.0f32, p0, p1)
        } else {
            (-1.0f32, p1, p0)
        };
        let dxdy = (p1.x - p0.x) / (p1.y - p0.y);
        if !dxdy.is_finite() {
            return;
        }
        let mut x = p0.x;
        if p0.y < 0.0 {
            x -= p0.y * dxdy;
            if !x.is_finite() {
                return;
            }
        }
        let y_first = p0.y as usize;
        let y_last = self.height.min(p1.y.ceil() as usize);
        for y in y_first..y_last {
            let linestart = y * self.width;
            let dy = ((y + 1) as f32).min(p1.y) - (y as f32).max(p0.y);
            let xnext = x + dxdy * dy;
            if !xnext.is_finite() {
                break;
            }
            let d = dy * dir;
            let (x0, x1) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let x0floor = x0.floor();
            let x0i = x0floor as i64;
            let x1ceil = x1.ceil();
            let x1i = x1ceil as i64;
            let base = i64::try_from(linestart).unwrap_or(i64::MAX);
            let Some(linestart_x0i) = base.checked_add(x0i) else {
                continue;
            };
            if linestart_x0i < 0 {
                continue;
            }
            if x1i <= x0i.saturating_add(1) {
                let xmf = 0.5 * (x + xnext) - x0floor;
                self.add(linestart_x0i, d - d * xmf);
                self.add(linestart_x0i.saturating_add(1), d * xmf);
            } else {
                let s = (x1 - x0).recip();
                let x0f = x0 - x0floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                self.add(linestart_x0i, d * a0);
                if x1i == x0i.saturating_add(2) {
                    self.add(linestart_x0i.saturating_add(1), d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - x0f);
                    self.add(linestart_x0i.saturating_add(1), d * (a1 - a0));
                    let a_len = i64::try_from(self.a.len()).unwrap_or(i64::MAX);
                    let lo = x0i.saturating_add(2).max(base.saturating_neg());
                    let hi = x1i.saturating_sub(1).min(a_len.saturating_sub(base));
                    if lo < hi
                        && let (Ok(start), Ok(end)) = (
                            usize::try_from(base.saturating_add(lo)),
                            usize::try_from(base.saturating_add(hi)),
                        )
                        && let Some(slice) = self.a.get_mut(start..end)
                    {
                        let delta = d * s;
                        for slot in slice {
                            *slot += delta;
                        }
                    }
                    let a2 = a1 + (i128::from(x1i) - i128::from(x0i) - 3) as f32 * s;
                    self.add(
                        base.saturating_add(x1i.saturating_sub(1)),
                        d * (1.0 - a2 - am),
                    );
                }
                self.add(base.saturating_add(x1i), d * am);
            }
            x = xnext;
        }
    }

    /// Flattens and accumulates a quadratic Bézier curve within [`FLATTEN_SAGITTA_PX`].
    pub fn draw_quad(&mut self, p0: Point, p1: Point, p2: Point) {
        if ![p0, p1, p2]
            .iter()
            .all(|p| p.x.is_finite() && p.y.is_finite())
        {
            return;
        }
        let devx = p0.x - 2.0 * p1.x + p2.x;
        let devy = p0.y - 2.0 * p1.y + p2.y;
        let devsq = devx * devx + devy * devy;
        if devsq < ONE_LINE_DEVSQ {
            self.draw_line(p0, p2);
            return;
        }
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

    /// Flattens and accumulates a cubic Bézier curve within [`FLATTEN_SAGITTA_PX`].
    pub fn draw_cubic(&mut self, p0: Point, p1: Point, p2: Point, p3: Point) {
        if ![p0, p1, p2, p3]
            .iter()
            .all(|p| p.x.is_finite() && p.y.is_finite())
        {
            return;
        }
        let d1x = p0.x - 2.0 * p1.x + p2.x;
        let d1y = p0.y - 2.0 * p1.y + p2.y;
        let d2x = p1.x - 2.0 * p2.x + p3.x;
        let d2y = p1.y - 2.0 * p2.y + p3.y;
        let dsq = (d1x * d1x + d1y * d1y).max(d2x * d2x + d2y * d2y);
        if dsq < ONE_LINE_DEVSQ * (1.0 / 9.0) {
            self.draw_line(p0, p3);
            return;
        }
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

    /// Calls `f(flat_index, coverage)` for every cell in row-major order.
    #[inline]
    pub fn for_each_pixel<F: FnMut(usize, f32)>(&self, mut f: F) {
        let mut acc = 0.0f32;
        let len = self.width.saturating_mul(self.height).min(self.a.len());
        for (i, c) in self.a[..len].iter().enumerate() {
            acc += *c;
            f(i, acc.abs());
        }
    }
}

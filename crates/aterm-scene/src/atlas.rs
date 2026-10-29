// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Straight-alpha RGBA8 tile rasterization shared by aterm's live effects.

/// A scratch RGBA8 tile (straight alpha) drawn into during baking.
pub struct Tile {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Tile {
    /// A transparent `w`×`h` RGBA8 tile.
    #[must_use]
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            px: vec![0; w as usize * h as usize * 4],
        }
    }

    /// Reset this tile to a transparent `w`×`h` surface, reusing its existing
    /// pixel buffer capacity whenever `w * h * 4` fits.
    #[inline]
    pub fn reset(&mut self, w: u32, h: u32) {
        self.w = w;
        self.h = h;
        let len = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        self.px.clear();
        self.px.resize(len, 0);
    }

    /// Tile width in texels.
    #[inline]
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.w
    }

    /// Tile height in texels.
    #[inline]
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.h
    }

    /// The straight-alpha RGBA8 texels, row-major, length `w*h*4`.
    #[inline]
    #[must_use]
    pub const fn pixels(&self) -> &[u8] {
        self.px.as_slice()
    }

    /// Mutable access to the straight-alpha RGBA8 texels, for callers that
    /// composite whole tiles (e.g. montage/gallery sheets) rather than blend
    /// per-pixel through [`Tile::over`].
    #[inline]
    pub const fn pixels_mut(&mut self) -> &mut [u8] {
        self.px.as_mut_slice()
    }

    /// Source-over one pixel (straight alpha).
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn over(&mut self, x: i32, y: i32, rgb: (f32, f32, f32), a: f32) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h || a <= 0.0 || a.is_nan() {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        let sa = crate::clampf(a, 0.0, 1.0);
        let to = |v: f32| (crate::clampf(v, 0.0, 1.0) * 255.0 + 0.5) as u8;
        if sa >= 1.0 {
            self.px[i..i + 4].copy_from_slice(&[to(rgb.0), to(rgb.1), to(rgb.2), 255]);
            return;
        }
        let (dr, dg, db, da) = (
            self.px[i] as f32 / 255.0,
            self.px[i + 1] as f32 / 255.0,
            self.px[i + 2] as f32 / 255.0,
            self.px[i + 3] as f32 / 255.0,
        );
        let inv_sa = 1.0 - sa;
        let oa = sa + da * inv_sa;
        let mix = |s: f32, d: f32| {
            if oa <= 0.0 {
                0.0
            } else {
                (s * sa + d * da * inv_sa) / oa
            }
        };
        self.px[i] = to(mix(rgb.0, dr));
        self.px[i + 1] = to(mix(rgb.1, dg));
        self.px[i + 2] = to(mix(rgb.2, db));
        self.px[i + 3] = to(oa);
    }

    /// Source-over a horizontal run of `len` pixels starting at `(x, y)`.
    #[inline]
    pub(crate) fn over_run(&mut self, x: i32, y: i32, len: u32, rgb: (f32, f32, f32), a: f32) {
        if y < 0 || y as u32 >= self.h || a <= 0.0 || a.is_nan() || len == 0 {
            return;
        }
        let start = x.max(0);
        let end = (i64::from(x) + i64::from(len)).min(i64::from(self.w));
        if i64::from(start) >= end {
            return;
        }
        let row = y as u32 * self.w;
        let sa = crate::clampf(a, 0.0, 1.0);
        let to = |v: f32| (crate::clampf(v, 0.0, 1.0) * 255.0 + 0.5) as u8;
        let span = &mut self.px[(row + start as u32) as usize * 4..(row + end as u32) as usize * 4];
        if sa >= 1.0 {
            let quad = [to(rgb.0), to(rgb.1), to(rgb.2), 255];
            span.as_chunks_mut::<4>().0.fill(quad);
            return;
        }
        let inv_sa = 1.0 - sa;
        for px in span.as_chunks_mut::<4>().0 {
            let (dr, dg, db, da) = (
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
                px[3] as f32 / 255.0,
            );
            let oa = sa + da * inv_sa;
            let mix = |s: f32, d: f32| {
                if oa <= 0.0 {
                    0.0
                } else {
                    (s * sa + d * da * inv_sa) / oa
                }
            };
            px[0] = to(mix(rgb.0, dr));
            px[1] = to(mix(rgb.1, dg));
            px[2] = to(mix(rgb.2, db));
            px[3] = to(oa);
        }
    }

    /// Fill `bbox` with `rgb` at `alpha`, modulated by the 4×4-supersampled coverage of
    /// `qx(px) + qy(py) <= limit` (sample coords are pixel centers in tile space).
    #[cfg_attr(trust_verify, trust::skip)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fill_quad<Fx: Fn(f32) -> f32, Fy: Fn(f32) -> f32>(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        rgb: (f32, f32, f32),
        alpha: f32,
        limit: f32,
        qx: Fx,
        qy: Fy,
    ) {
        if !alpha.is_finite() || alpha <= 0.0 || !limit.is_finite() || limit < 0.0 {
            return;
        }
        const SUB: [f32; 4] = [0.125, 0.375, 0.625, 0.875];
        const INV16: f32 = 1.0 / 16.0;
        let x0 = x0.max(0);
        let y0 = y0.max(0);
        let x1 = x1.min(self.w as i32);
        let y1 = y1.min(self.h as i32);
        for py in y0..y1 {
            let py_f = py as f32;
            let dy2 = SUB.map(|off| qy(py_f + off));
            let dy2_min = dy2[0].min(dy2[1]).min(dy2[2]).min(dy2[3]);
            if dy2_min > limit {
                continue;
            }
            let dy2_max = dy2[0].max(dy2[1]).max(dy2[2]).max(dy2[3]);
            for px in x0..x1 {
                let px_f = px as f32;
                let dx0 = qx(px_f + SUB[0]);
                let dx3 = qx(px_f + SUB[3]);
                if dx0.max(dx3) + dy2_max <= limit {
                    let dx1 = qx(px_f + SUB[1]);
                    let dx2 = qx(px_f + SUB[2]);
                    if dx1.max(dx2) + dy2_max <= limit {
                        self.over(px, py, rgb, alpha * 16.0 * INV16);
                        continue;
                    }
                }
                let dx1 = qx(px_f + SUB[1]);
                let dx2 = qx(px_f + SUB[2]);
                let dx2_arr = [dx0, dx1, dx2, dx3];
                let dx2_min = dx0.min(dx1).min(dx2).min(dx3);
                if dx2_min + dy2_min > limit {
                    continue;
                }
                let mut hit = 0u32;
                for &yv in &dy2 {
                    if yv > limit {
                        continue;
                    }
                    for &xv in &dx2_arr {
                        if xv + yv <= limit {
                            hit = hit.saturating_add(1);
                        }
                    }
                }
                if hit > 0 {
                    self.over(px, py, rgb, alpha * hit as f32 * INV16);
                }
            }
        }
    }

    /// A filled disc of radius `r` centered at `(cx, cy)`.
    pub fn disc(&mut self, cx: f32, cy: f32, r: f32, rgb: (f32, f32, f32), a: f32) {
        if !(cx.is_finite() && cy.is_finite() && r.is_finite()) || r < 0.0 {
            return;
        }
        let rr = r * r;
        self.fill_quad(
            ((cx - r) as i32).saturating_sub(1),
            ((cy - r) as i32).saturating_sub(1),
            ((cx + r) as i32).saturating_add(2),
            ((cy + r) as i32).saturating_add(2),
            rgb,
            a,
            rr,
            |x| {
                let dx = x - cx;
                dx * dx
            },
            |y| {
                let dy = y - cy;
                dy * dy
            },
        );
    }

    /// An axis-aligned filled ellipse with radii `(rx, ry)` centered at `(cx, cy)`.
    pub fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, rgb: (f32, f32, f32), a: f32) {
        if !(cx.is_finite() && cy.is_finite() && rx.is_finite() && ry.is_finite())
            || rx <= 0.0
            || ry <= 0.0
        {
            return;
        }
        self.fill_quad(
            ((cx - rx) as i32).saturating_sub(1),
            ((cy - ry) as i32).saturating_sub(1),
            ((cx + rx) as i32).saturating_add(2),
            ((cy + ry) as i32).saturating_add(2),
            rgb,
            a,
            1.0,
            |x| {
                let nx = (x - cx) / rx;
                nx * nx
            },
            |y| {
                let ny = (y - cy) / ry;
                ny * ny
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::Tile;

    #[test]
    fn nan_alpha_leaves_painted_pixels_unchanged() {
        let mut t = Tile::new(4, 1);
        t.over_run(0, 0, 4, (1.0, 0.5, 0.25), 1.0);
        let painted = t.pixels().to_vec();
        assert_ne!(painted, vec![0; 16]);

        t.over(1, 0, (0.0, 1.0, 0.0), f32::NAN);
        t.over_run(0, 0, 4, (0.0, 0.0, 1.0), f32::NAN);
        assert_eq!(t.pixels(), &painted[..]);
    }
}

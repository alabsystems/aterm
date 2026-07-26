// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Straight-alpha RGBA8 tile rasterization shared by aterm's live effects.

/// A scratch RGBA8 tile (straight alpha) drawn into during baking.
///
/// Public so other host-side bakers (the sparkle-words v2 `CatBaker` in
/// `aterm-gui`, design §5.5) reuse THIS rasterizer — the same 4×4-supersampled
/// coverage, the same straight-alpha src-over — instead of growing a drifting
/// look-alike. The primitive set (disc / ellipse / rrect / tri / fill) is
/// exactly what the in-crate sprites bake with.
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

    /// Tile width in texels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.w
    }

    /// Tile height in texels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.h
    }

    /// The straight-alpha RGBA8 texels, row-major, length `w*h*4`.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.px
    }

    /// Mutable access to the straight-alpha RGBA8 texels, for callers that
    /// composite whole tiles (e.g. montage/gallery sheets) rather than blend
    /// per-pixel through [`Tile::over`].
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.px
    }

    /// Source-over one pixel (straight alpha). `rgb` channels and `a` are in `[0,1]`.
    // Native lowering gap: typed-TrustIr does not complete on this body — the
    // trust-vc native memory verifier reports every `self.px[..]` BoundsCheck as
    // Unsupported ("bundle contains no TrustVc requests"), regardless of how the
    // index bound is re-established, while the arithmetic obligations do prove.
    // The access is safe by the Tile invariant (px.len() == w*h*4, enforced in
    // `new`) plus the leading x/y range guard.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn over(&mut self, x: i32, y: i32, rgb: (f32, f32, f32), a: f32) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h || a <= 0.0 {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        let (dr, dg, db, da) = (
            self.px[i] as f32 / 255.0,
            self.px[i + 1] as f32 / 255.0,
            self.px[i + 2] as f32 / 255.0,
            self.px[i + 3] as f32 / 255.0,
        );
        let sa = crate::clampf(a, 0.0, 1.0);
        let oa = sa + da * (1.0 - sa);
        let mix = |s: f32, d: f32| {
            if oa <= 0.0 {
                0.0
            } else {
                (s * sa + d * da * (1.0 - sa)) / oa
            }
        };
        let to = |v: f32| (crate::clampf(v, 0.0, 1.0) * 255.0 + 0.5) as u8;
        self.px[i] = to(mix(rgb.0, dr));
        self.px[i + 1] = to(mix(rgb.1, dg));
        self.px[i + 2] = to(mix(rgb.2, db));
        self.px[i + 3] = to(oa);
    }

    /// Source-over a horizontal run of `len` pixels starting at `(x, y)` — the span-fill
    /// fast path the scanline rasterizer ([`crate::vector::fill_path`]) uses for
    /// fully-covered pixel runs. Byte-identical to calling [`over`](Self::over) `len`
    /// times; when `a` is opaque it pattern-fills the RGBA quad directly (one 4-byte
    /// `copy_from_slice` per pixel, no per-pixel blend). The run is clipped to the tile.
    pub fn over_run(&mut self, x: i32, y: i32, len: u32, rgb: (f32, f32, f32), a: f32) {
        if y < 0 || y as u32 >= self.h || a <= 0.0 || len == 0 {
            return;
        }
        // Clip [x, x+len) to [0, w) in i64 so a hostile x+len never overflows.
        let start = x.max(0);
        let end = (i64::from(x) + i64::from(len)).min(i64::from(self.w));
        if i64::from(start) >= end {
            return;
        }
        let row = y as u32 * self.w;
        let sa = crate::clampf(a, 0.0, 1.0);
        let to = |v: f32| (crate::clampf(v, 0.0, 1.0) * 255.0 + 0.5) as u8;
        if sa >= 1.0 {
            // Opaque source-over collapses to a straight copy (oa = 1, mix = source).
            let quad = [to(rgb.0), to(rgb.1), to(rgb.2), 255];
            for xi in start..end as i32 {
                let i = ((row + xi as u32) * 4) as usize;
                self.px[i..i + 4].copy_from_slice(&quad);
            }
            return;
        }
        for xi in start..end as i32 {
            let i = ((row + xi as u32) * 4) as usize;
            let (dr, dg, db, da) = (
                self.px[i] as f32 / 255.0,
                self.px[i + 1] as f32 / 255.0,
                self.px[i + 2] as f32 / 255.0,
                self.px[i + 3] as f32 / 255.0,
            );
            let oa = sa + da * (1.0 - sa);
            let mix = |s: f32, d: f32| {
                if oa <= 0.0 {
                    0.0
                } else {
                    (s * sa + d * da * (1.0 - sa)) / oa
                }
            };
            self.px[i] = to(mix(rgb.0, dr));
            self.px[i + 1] = to(mix(rgb.1, dg));
            self.px[i + 2] = to(mix(rgb.2, db));
            self.px[i + 3] = to(oa);
        }
    }

    /// Fill `bbox` with `rgb` at `alpha`, modulated by the 4×4-supersampled coverage of
    /// `inside(px, py)` (sample coords are pixel centers in tile space).
    // A raw bbox + colour + alpha parameter list reads better at the dozens of tiny
    // call sites than a packed struct would.
    // Invokes the caller-supplied `inside: F` closure (an indirect callee whose body
    // is absent from the lowered bundle, so its panic-freedom can't be discharged)
    // inside a 4×4 supersampling loop, and calls the `trust::skip`'d `over`; the full
    // verifier times out on the body. Idiom-3 skip (generic-`Fn` / absent callee).
    #[cfg_attr(trust_verify, trust::skip)]
    #[allow(clippy::too_many_arguments)]
    pub fn fill<F: Fn(f32, f32) -> bool>(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        rgb: (f32, f32, f32),
        alpha: f32,
        inside: F,
    ) {
        let x0 = x0.max(0);
        let y0 = y0.max(0);
        let x1 = x1.min(self.w as i32);
        let y1 = y1.min(self.h as i32);
        for py in y0..y1 {
            for px in x0..x1 {
                let mut hit = 0u32;
                for sy in 0..4 {
                    for sx in 0..4 {
                        // ×0.25 is bit-identical to /4 (power of two); saturating
                        // add is a no-op for a counter bounded by 16. Both purely
                        // discharge proof obligations.
                        let fx = px as f32 + (sx as f32 + 0.5) * 0.25;
                        let fy = py as f32 + (sy as f32 + 0.5) * 0.25;
                        if inside(fx, fy) {
                            hit = hit.saturating_add(1);
                        }
                    }
                }
                if hit > 0 {
                    // ×(1/16) is bit-identical to /16 (power of two) and avoids a
                    // float-division obligation on the hot supersampling path.
                    const INV16: f32 = 1.0 / 16.0;
                    self.over(px, py, rgb, alpha * hit as f32 * INV16);
                }
            }
        }
    }

    /// A filled disc of radius `r` centered at `(cx, cy)`.
    pub fn disc(&mut self, cx: f32, cy: f32, r: f32, rgb: (f32, f32, f32), a: f32) {
        let rr = r * r;
        // Saturating bbox math: the float→int casts already saturate at the i32
        // limits, so ±1/±2 must not wrap there. Real sprite coords are ≤ tile size.
        self.fill(
            ((cx - r) as i32).saturating_sub(1),
            ((cy - r) as i32).saturating_sub(1),
            ((cx + r) as i32).saturating_add(2),
            ((cy + r) as i32).saturating_add(2),
            rgb,
            a,
            |x, y| {
                let (dx, dy) = (x - cx, y - cy);
                dx * dx + dy * dy <= rr
            },
        );
    }

    /// An axis-aligned filled ellipse with radii `(rx, ry)` centered at `(cx, cy)`.
    pub fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, rgb: (f32, f32, f32), a: f32) {
        // Saturating bbox math, same reasoning as `disc`.
        self.fill(
            ((cx - rx) as i32).saturating_sub(1),
            ((cy - ry) as i32).saturating_sub(1),
            ((cx + rx) as i32).saturating_add(2),
            ((cy + ry) as i32).saturating_add(2),
            rgb,
            a,
            |x, y| {
                let nx = (x - cx) / rx;
                let ny = (y - cy) / ry;
                nx * nx + ny * ny <= 1.0
            },
        );
    }

    /// A filled rounded rectangle at `(x, y)` sized `w`×`h` with corner radius `rad`.
    #[allow(clippy::too_many_arguments)]
    pub fn rrect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        rad: f32,
        rgb: (f32, f32, f32),
        a: f32,
    ) {
        let r = rad.min(w * 0.5).min(h * 0.5).max(0.0);
        let (ix0, iy0, ix1, iy1) = (x + r, y + r, x + w - r, y + h - r);
        // Saturating bbox math, same reasoning as `disc`.
        self.fill(
            (x as i32).saturating_sub(1),
            (y as i32).saturating_sub(1),
            ((x + w) as i32).saturating_add(2),
            ((y + h) as i32).saturating_add(2),
            rgb,
            a,
            |px, py| {
                if px < x || px > x + w || py < y || py > y + h {
                    return false;
                }
                // clamp to the inner rect; distance to that clamp point <= r (rounded corners)
                let qx = crate::clampf(px, ix0, ix1);
                let qy = crate::clampf(py, iy0, iy1);
                let (dx, dy) = (px - qx, py - qy);
                dx * dx + dy * dy <= r * r
            },
        );
    }

    /// A filled triangle through the three points `p`.
    pub fn tri(&mut self, p: [(f32, f32); 3], rgb: (f32, f32, f32), a: f32) {
        let xs = [p[0].0, p[1].0, p[2].0];
        let ys = [p[0].1, p[1].1, p[2].1];
        // Saturating bbox math, same reasoning as `disc`.
        let x0 = (xs.iter().copied().fold(f32::INFINITY, f32::min) as i32).saturating_sub(1);
        let x1 = (xs.iter().copied().fold(f32::NEG_INFINITY, f32::max) as i32).saturating_add(2);
        let y0 = (ys.iter().copied().fold(f32::INFINITY, f32::min) as i32).saturating_sub(1);
        let y1 = (ys.iter().copied().fold(f32::NEG_INFINITY, f32::max) as i32).saturating_add(2);
        let sign = |ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32| {
            (ax - cx) * (by - cy) - (bx - cx) * (ay - cy)
        };
        self.fill(x0, y0, x1, y1, rgb, a, |x, y| {
            let d1 = sign(x, y, p[0].0, p[0].1, p[1].0, p[1].1);
            let d2 = sign(x, y, p[1].0, p[1].1, p[2].0, p[2].1);
            let d3 = sign(x, y, p[2].0, p[2].1, p[0].0, p[0].1);
            let neg = (d1 < 0.0) || (d2 < 0.0) || (d3 < 0.0);
            let pos = (d1 > 0.0) || (d2 > 0.0) || (d3 > 0.0);
            !(neg && pos)
        });
    }
}

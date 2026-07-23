// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The procedural **scene atlas**: one RGBA8 texture holding every sprite a scene draws,
//! baked in code (no asset files) so the feature ships with zero art dependencies and
//! degrades gracefully — exactly the spirit of aterm's `procedural.rs` glyph path. A
//! sprite sheet can later *override* atlas regions (the "hybrid" art direction) with no
//! renderer change, since the renderer only ever samples texel rects.
//!
//! ## Two coloring tricks that keep the atlas tiny and flexible
//!
//! 1. **Grayscale + tint.** Creatures are baked in GRAYSCALE (brightness = shading); the
//!    draw-time multiply tint turns one cat into any fur colour, so infinite skins and
//!    friend-cat variety cost zero extra texels. Dark detail (eyes/nose, baked near-black)
//!    survives any tint.
//! 2. **Neutral light.** The additive sprites ([`Sprite::Glow`], [`Sprite::Star`]) are
//!    white falloffs; their hue comes from the draw-time tint, so the sun, fireflies, and
//!    comet sparks all reuse two sprites.
//!
//! Everything is anti-aliased via 4×4 supersampled coverage, so the soft edges read well
//! at any scale (sprites are baked at a reference size and scaled at draw).

/// Every sprite the atlas bakes. Neutral/grayscale; tinted at draw time.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Sprite {
    /// A 4×4 solid white block — for solid fills (sky gradient bands, bars) via tint.
    Pixel,
    /// A soft radial white falloff — additive light (sun glow, fireflies, comet heads).
    Glow,
    /// A 4-point sparkle star — additive twinkle (cosmos stars, sparkles).
    Star,
    /// Cat, curled & asleep (the idle pose).
    CatCurl,
    /// Cat, sitting upright (resting/grooming).
    CatSit,
    /// Cat, standing — walk-cycle frame A.
    CatWalkA,
    /// Cat, standing — walk-cycle frame B.
    CatWalkB,
    /// Cat, crouched mid-pounce (playing).
    CatPounce,
    /// A little A-frame cat house.
    House,
    /// A soft cloud (also a rain cloud when tinted grey).
    Cloud,
    /// A small flower (center + petals; petals tintable, center baked yellow-ish).
    Flower,
    /// A tuft of grass blades.
    GrassTuft,
    /// A butterfly (two wings + body); wings tintable.
    Butterfly,
    /// A shaded planet/celestial body (grayscale → tinted) for the Cosmos scene.
    Planet,
    /// An orca (killer whale) — FIXED black/white (not tinted) so it's iconic on any theme.
    Orca,
    /// A small fish (grayscale → tinted to any species colour) for the Ocean scene.
    Fish,
    /// A rising bubble (soft ring + highlight).
    Bubble,
    /// A strand of kelp (grayscale → tinted green).
    Kelp,
    /// A drifting jellyfish (translucent bell + tentacles, grayscale → tinted).
    Jelly,
    /// A leaf (grayscale → tinted) — drifts down for the cat to chase.
    Leaf,
}

impl Sprite {
    /// Every sprite, stable order (also the atlas pack order).
    pub const ALL: [Sprite; 20] = [
        Sprite::Pixel,
        Sprite::Glow,
        Sprite::Star,
        Sprite::CatCurl,
        Sprite::CatSit,
        Sprite::CatWalkA,
        Sprite::CatWalkB,
        Sprite::CatPounce,
        Sprite::House,
        Sprite::Cloud,
        Sprite::Flower,
        Sprite::GrassTuft,
        Sprite::Butterfly,
        Sprite::Planet,
        Sprite::Orca,
        Sprite::Fish,
        Sprite::Bubble,
        Sprite::Kelp,
        Sprite::Jelly,
        Sprite::Leaf,
    ];
    /// Number of distinct sprites.
    pub const COUNT: usize = Self::ALL.len();

    /// Dense index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The baked tile size (texels) for this sprite.
    #[must_use]
    pub const fn size(self) -> (u32, u32) {
        match self {
            Sprite::Pixel => (4, 4),
            Sprite::Glow => (64, 64),
            Sprite::Star => (40, 40),
            Sprite::CatCurl
            | Sprite::CatSit
            | Sprite::CatWalkA
            | Sprite::CatWalkB
            | Sprite::CatPounce => (96, 96),
            Sprite::House => (104, 88),
            Sprite::Cloud => (112, 60),
            Sprite::Flower => (36, 36),
            Sprite::GrassTuft => (56, 44),
            Sprite::Butterfly => (52, 44),
            Sprite::Planet => (64, 64),
            Sprite::Orca => (112, 64),
            Sprite::Fish => (44, 30),
            Sprite::Bubble => (28, 28),
            Sprite::Kelp => (44, 104),
            Sprite::Jelly => (52, 68),
            Sprite::Leaf => (36, 30),
        }
    }

    /// The reference aspect ratio (`w/h`) — scenes scale dest rects to preserve it.
    #[must_use]
    pub fn aspect(self) -> f32 {
        let (w, h) = self.size();
        // `size()` never returns a zero height (min is 4), so the guard is a no-op;
        // an explicit dominating branch on the float divisor (rather than `h.max(1)`,
        // whose callee the full verifier cannot see through) makes the division
        // provably nonzero-divisor. The unreachable arm mirrors `w / 1`.
        let hf = h as f32;
        if hf > 0.0 { w as f32 / hf } else { w as f32 }
    }
}

/// A texel rectangle inside the atlas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasRect {
    pub ax: u16,
    pub ay: u16,
    pub aw: u16,
    pub ah: u16,
}

/// The baked RGBA8 atlas plus each sprite's texel rect. `version` lets the host re-upload
/// only when the atlas actually changes (e.g. a skin/theme switch rebakes).
#[derive(Clone, Debug)]
pub struct Atlas {
    /// Atlas width in texels.
    pub width: u32,
    /// Atlas height in texels.
    pub height: u32,
    /// Straight-alpha RGBA8 pixels, row-major, length `width*height*4`.
    pub rgba: Vec<u8>,
    /// Monotonic version (bumped each bake).
    pub version: u64,
    rects: [AtlasRect; Sprite::COUNT],
}

impl Atlas {
    /// The texel rect of a sprite as a `(ax, ay, aw, ah)` tuple.
    #[must_use]
    pub fn rect(self_: &Atlas, s: Sprite) -> (u16, u16, u16, u16) {
        // `s.index()` is the dense discriminant, always `< COUNT`; the checked
        // lookup discharges the bounds obligation (the default is unreachable).
        let r = self_.rects.get(s.index()).copied().unwrap_or_default();
        (r.ax, r.ay, r.aw, r.ah)
    }

    /// Bake the whole atlas at the given `version`. Deterministic and allocation-bounded.
    #[must_use]
    pub fn bake(version: u64) -> Atlas {
        // Shelf-pack tiles into a fixed-width atlas with 1px gutters (avoid bleed).
        const WIDTH: u32 = 512;
        const PAD: u32 = 1;
        // The packed height is fully determined by the fixed sprite set (~300 texels
        // today, `next_power_of_two` → 512). This cap is far above anything the set
        // can reach; it exists purely to hand the prover a hard allocation bound.
        // For the same reason all the shelf arithmetic saturates: the sums are tiny
        // in every real bake, so saturation is unreachable and provably panic-free.
        const MAX_HEIGHT: u32 = 1 << 12;
        let mut rects = [AtlasRect::default(); Sprite::COUNT];

        let (mut cx, mut cy, mut shelf_h) = (PAD, PAD, 0u32);
        // `Sprite::ALL[i].index() == i`, so zipping replaces the `rects[s.index()]`
        // stores one-for-one (and removes the index bounds obligation).
        for (slot, s) in rects.iter_mut().zip(Sprite::ALL) {
            let (w, h) = s.size();
            if cx.saturating_add(w).saturating_add(PAD) > WIDTH {
                cx = PAD;
                cy = cy.saturating_add(shelf_h).saturating_add(PAD);
                shelf_h = 0;
            }
            *slot = AtlasRect {
                ax: cx as u16,
                ay: cy as u16,
                aw: w as u16,
                ah: h as u16,
            };
            cx = cx.saturating_add(w).saturating_add(PAD);
            shelf_h = shelf_h.max(h);
        }
        let packed = cy
            .saturating_add(shelf_h)
            .saturating_add(PAD)
            .min(MAX_HEIGHT);
        // Re-clamp the unmodeled `next_power_of_two` result to `MAX_HEIGHT` so the
        // stored height stays a bounded power of two. A no-op in practice: packed is
        // ~300 for the fixed sprite set, so `npot` is 512.
        let npot = packed.next_power_of_two();
        let height = npot.min(MAX_HEIGHT);
        // Largest legitimate atlas: WIDTH(512) x MAX_HEIGHT(4096) x 4 = 8_388_608 texel
        // bytes, far below the checker's 268_435_456-element allocation ceiling.
        const MAX_TEXELS: usize = WIDTH as usize * MAX_HEIGHT as usize * 4;
        let count = WIDTH as usize * height as usize * 4;
        // Dominating allocation guard (base64-style) on the *exact* count operand: the
        // unmodeled `next_power_of_two` provenance defeats the checker's range analysis
        // through `min`, so bound the count with a control-flow-dominating comparison it
        // recognises directly. Unreachable on every real bake — packed <= MAX_HEIGHT (a
        // power of two) ⟹ npot <= MAX_HEIGHT ⟹ count <= MAX_TEXELS — and it fails safe
        // (an empty atlas) for any future change that would break that invariant.
        if count > MAX_TEXELS {
            return Atlas {
                width: WIDTH,
                height: 0,
                rgba: Vec::new(),
                version,
                rects,
            };
        }
        let mut atlas = Atlas {
            width: WIDTH,
            height,
            rgba: vec![0; count],
            version,
            rects,
        };
        for (&r, s) in rects.iter().zip(Sprite::ALL) {
            let mut tile = Tile::new(r.aw as u32, r.ah as u32);
            draw_sprite(s, &mut tile);
            atlas.blit(&tile, r.ax as u32, r.ay as u32);
        }
        atlas
    }

    fn blit(&mut self, tile: &Tile, ox: u32, oy: u32) {
        // All address math is widened to usize and saturating, and every access
        // goes through the checked slice APIs. On the only caller (`bake`) the
        // tile always sits fully inside `rgba`, so no guard ever fires and the
        // copy is byte-identical to the unchecked version.
        let (tw, th) = (tile.w as usize, tile.h as usize);
        let (aw, ah) = (self.width as usize, self.height as usize);
        for ty in 0..th {
            for tx in 0..tw {
                let dx = (ox as usize).saturating_add(tx);
                let dy = (oy as usize).saturating_add(ty);
                if dx >= aw || dy >= ah {
                    continue;
                }
                let si = ty.saturating_mul(tw).saturating_add(tx).saturating_mul(4);
                let di = dy.saturating_mul(aw).saturating_add(dx).saturating_mul(4);
                if let (Some(dst), Some(src)) = (
                    self.rgba.get_mut(di..di.saturating_add(4)),
                    tile.px.get(si..si.saturating_add(4)),
                ) && dst.len() == src.len()
                {
                    dst.copy_from_slice(src);
                }
            }
        }
    }
}

// =====================================================================================
// Software rasterizer for baking — straight-alpha src-over, 4×4 supersampled coverage.
// =====================================================================================

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

    /// Radial falloff (for [`Sprite::Glow`]): white center → transparent edge, smooth.
    fn radial(&mut self) {
        if self.w == 0 || self.h == 0 {
            // Nothing to draw (the loops below would be empty anyway); hoisting the
            // guard also makes `r` provably non-zero for the division.
            return;
        }
        let cx = self.w as f32 * 0.5;
        let cy = self.h as f32 * 0.5;
        let r = (self.w.min(self.h) as f32) * 0.5;
        for py in 0..self.h as i32 {
            for px in 0..self.w as i32 {
                let dx = px as f32 + 0.5 - cx;
                let dy = py as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt() / r;
                let a = crate::clampf(1.0 - d, 0.0, 1.0);
                // Tighter, dimmer falloff (a³ × 0.85): a small bright core that fades fast,
                // so large additive glows don't wash the scene to white when they overlap.
                let a = a * a * a * 0.85;
                self.over(px, py, (1.0, 1.0, 1.0), a);
            }
        }
    }
}

// Grayscale palette for tintable creatures (brightness → fur shading after tint).
const FUR: (f32, f32, f32) = (0.95, 0.95, 0.95);
const FUR_SHADE: (f32, f32, f32) = (0.66, 0.66, 0.66);
const DARK: (f32, f32, f32) = (0.10, 0.10, 0.10); // eyes — survives any tint
const CATCH: (f32, f32, f32) = (0.98, 0.98, 0.98); // eye catch-light
const PINK: (f32, f32, f32) = (0.98, 0.58, 0.66); // kitten nose (tints gently with fur)

fn draw_sprite(s: Sprite, t: &mut Tile) {
    match s {
        Sprite::Pixel => t.fill(
            0,
            0,
            t.w as i32,
            t.h as i32,
            (1.0, 1.0, 1.0),
            1.0,
            |_, _| true,
        ),
        Sprite::Glow => t.radial(),
        Sprite::Star => draw_star(t),
        Sprite::CatCurl => draw_cat(t, Pose::Curl),
        Sprite::CatSit => draw_cat(t, Pose::Sit),
        Sprite::CatWalkA => draw_cat(t, Pose::Walk(0)),
        Sprite::CatWalkB => draw_cat(t, Pose::Walk(1)),
        Sprite::CatPounce => draw_cat(t, Pose::Pounce),
        Sprite::House => draw_house(t),
        Sprite::Cloud => draw_cloud(t),
        Sprite::Flower => draw_flower(t),
        Sprite::GrassTuft => draw_grass(t),
        Sprite::Butterfly => draw_butterfly(t),
        Sprite::Planet => draw_planet(t),
        Sprite::Orca => draw_orca(t),
        Sprite::Fish => draw_fish(t),
        Sprite::Bubble => draw_bubble(t),
        Sprite::Kelp => draw_kelp(t),
        Sprite::Jelly => draw_jelly(t),
        Sprite::Leaf => draw_leaf(t),
    }
}

/// A leaf: a pointed ellipse with a central vein (grayscale → tinted green/autumn).
fn draw_leaf(t: &mut Tile) {
    let w = t.w as f32;
    let h = t.h as f32;
    let cx = w * 0.5;
    let cy = h * 0.5;
    // blade: two arcs meeting at points left/right → a leaf shape.
    t.fill(0, 0, w as i32, h as i32, FUR, 1.0, |x, y| {
        let nx = (x - cx) / (w * 0.44);
        let ny = (y - cy) / (h * 0.34);
        // pointed at the horizontal ends: taper the vertical radius toward |nx|→1.
        let taper = (1.0 - nx * nx).max(0.0);
        ny * ny <= taper && nx.abs() <= 1.0
    });
    // central vein + midrib shade.
    t.fill(0, 0, w as i32, h as i32, FUR_SHADE, 0.9, |x, y| {
        (y - cy).abs() < 1.2 && (x - cx).abs() < w * 0.42
    });
}

#[derive(Clone, Copy)]
enum Pose {
    Curl,
    Sit,
    Walk(u8),
    Pounce,
}

/// Draw a cute, recognizable cat into a 96×96 tile (grayscale; tinted at draw). The
/// silhouette reads at a glance: round head, big triangle ears, oval body, swishy tail,
/// two big eyes with a catch-light.
fn draw_cat(t: &mut Tile, pose: Pose) {
    let w = t.w as f32;
    let h = t.h as f32;
    let cx = w * 0.5;
    // ground line near the bottom of the tile.
    let gy = h * 0.92;

    let head = |t: &mut Tile, hx: f32, hy: f32, hr: f32, look: f32| {
        // ears (behind head so the head edge overlaps them cleanly)
        let ear = |t: &mut Tile, ex: f32, dir: f32| {
            t.tri(
                [
                    (ex, hy - hr * 0.45),
                    (ex + dir * hr * 0.85, hy - hr * 1.55),
                    (ex + dir * hr * 0.95, hy - hr * 0.2),
                ],
                FUR,
                1.0,
            );
            // inner ear shade
            t.tri(
                [
                    (ex + dir * hr * 0.18, hy - hr * 0.5),
                    (ex + dir * hr * 0.62, hy - hr * 1.18),
                    (ex + dir * hr * 0.66, hy - hr * 0.32),
                ],
                FUR_SHADE,
                1.0,
            );
        };
        ear(t, hx - hr * 0.45, -1.0);
        ear(t, hx + hr * 0.45, 1.0);
        // head
        t.disc(hx, hy, hr, FUR, 1.0);
        // cheeks (slightly wider, shaded) for a kitten roundness
        t.disc(hx - hr * 0.5, hy + hr * 0.25, hr * 0.45, FUR, 1.0);
        t.disc(hx + hr * 0.5, hy + hr * 0.25, hr * 0.45, FUR, 1.0);
        // eyes — big and round (kitten-cute), with a bright catch-light.
        let eye = |t: &mut Tile, ex: f32| {
            t.ellipse(ex, hy + hr * 0.02, hr * 0.24, hr * 0.31, DARK, 1.0);
            t.disc(
                ex + look * hr * 0.05 - hr * 0.06,
                hy - hr * 0.08,
                hr * 0.09,
                CATCH,
                0.95,
            );
        };
        eye(t, hx - hr * 0.36 + look * hr * 0.12);
        eye(t, hx + hr * 0.36 + look * hr * 0.12);
        // little pink nose + a soft smile.
        t.tri(
            [
                (hx, hy + hr * 0.34),
                (hx - hr * 0.11, hy + hr * 0.2),
                (hx + hr * 0.11, hy + hr * 0.2),
            ],
            PINK,
            1.0,
        );
    };

    match pose {
        Pose::Curl => {
            // a curled, sleeping loaf: a compact rounded body with the tail wrapped around the
            // FRONT to a soft rounded tip (no sharp fish-tail point), head tucked, eyes shut.
            t.ellipse(cx - w * 0.02, gy - h * 0.15, w * 0.37, h * 0.19, FUR, 1.0);
            t.ellipse(cx, gy - h * 0.09, w * 0.34, h * 0.10, FUR_SHADE, 0.45);
            // wrapped tail: a crescent hugging only the body's RIGHT/front side...
            t.fill(
                (cx - w * 0.06) as i32,
                (gy - h * 0.24) as i32,
                (cx + w * 0.42) as i32,
                gy as i32,
                FUR,
                1.0,
                |x, y| {
                    let big = {
                        let nx = (x - cx) / (w * 0.40);
                        let ny = (y - (gy - h * 0.14)) / (h * 0.21);
                        nx * nx + ny * ny <= 1.0
                    };
                    let inner = {
                        let nx = (x - cx) / (w * 0.27);
                        let ny = (y - (gy - h * 0.13)) / (h * 0.12);
                        nx * nx + ny * ny <= 1.0
                    };
                    big && !inner && x > cx - w * 0.02 && y < gy - h * 0.06
                },
            );
            // ...capped with a rounded tail tip resting by the tucked paws.
            t.disc(cx + w * 0.30, gy - h * 0.085, h * 0.055, FUR, 1.0);
            // tucked head, closed eyes (a soft arc)
            let (hx, hy, hr) = (cx - w * 0.24, gy - h * 0.2, h * 0.15);
            t.disc(hx, hy, hr, FUR, 1.0);
            // ears
            t.tri(
                [
                    (hx - hr * 0.6, hy - hr * 0.3),
                    (hx - hr * 0.2, hy - hr * 1.2),
                    (hx + hr * 0.1, hy - hr * 0.4),
                ],
                FUR,
                1.0,
            );
            t.tri(
                [
                    (hx + hr * 0.1, hy - hr * 0.45),
                    (hx + hr * 0.5, hy - hr * 1.15),
                    (hx + hr * 0.7, hy - hr * 0.3),
                ],
                FUR,
                1.0,
            );
            // closed eye line
            t.fill(
                (hx - hr * 0.6) as i32,
                (hy - 2.0) as i32,
                (hx - hr * 0.05) as i32,
                (hy + 3.0) as i32,
                DARK,
                0.9,
                |x, y| {
                    let t0 = (x - (hx - hr * 0.55)) / (hr * 0.5);
                    let yc = hy - (t0 * (1.0 - t0)) * hr * 0.5;
                    (y - yc).abs() < 1.0
                },
            );
        }
        Pose::Sit => {
            // upright sitting cat: haunch, body, front legs, tail at side, head on top
            t.ellipse(cx, gy - h * 0.16, w * 0.3, h * 0.18, FUR, 1.0); // haunch
            t.rrect(
                cx - w * 0.2,
                gy - h * 0.5,
                w * 0.4,
                h * 0.42,
                w * 0.18,
                FUR,
                1.0,
            ); // body
            // front legs
            t.rrect(
                cx - w * 0.14,
                gy - h * 0.16,
                w * 0.1,
                h * 0.16,
                w * 0.05,
                FUR,
                1.0,
            );
            t.rrect(
                cx + w * 0.04,
                gy - h * 0.16,
                w * 0.1,
                h * 0.16,
                w * 0.05,
                FUR,
                1.0,
            );
            // tail curled to the right
            tail(t, cx + w * 0.16, gy - h * 0.12, 1.0, w, h);
            // head
            head(t, cx, gy - h * 0.62, h * 0.2, 0.0);
        }
        Pose::Walk(frame) => {
            // standing cat in profile-ish 3/4: body oval, 4 legs (two phases), tail up
            t.ellipse(cx, gy - h * 0.34, w * 0.34, h * 0.2, FUR, 1.0); // body
            let swing = if frame == 0 { 1.0 } else { -1.0 };
            let leg = |t: &mut Tile, lx: f32, s: f32| {
                t.rrect(
                    lx - w * 0.04,
                    gy - h * 0.22 + s * h * 0.02,
                    w * 0.08,
                    h * 0.2,
                    w * 0.04,
                    FUR_SHADE,
                    1.0,
                );
            };
            leg(t, cx - w * 0.2, swing);
            leg(t, cx - w * 0.06, -swing);
            leg(t, cx + w * 0.08, swing);
            leg(t, cx + w * 0.22, -swing);
            tail(t, cx - w * 0.3, gy - h * 0.4, -1.0, w, h);
            head(t, cx + w * 0.26, gy - h * 0.46, h * 0.18, 0.4);
        }
        Pose::Pounce => {
            // crouched, butt up, front low — playful
            t.ellipse(cx + w * 0.06, gy - h * 0.34, w * 0.3, h * 0.18, FUR, 1.0); // raised rear
            t.ellipse(cx - w * 0.18, gy - h * 0.18, w * 0.22, h * 0.12, FUR, 1.0); // low front
            // front paws stretched
            t.rrect(
                cx - w * 0.34,
                gy - h * 0.12,
                w * 0.1,
                h * 0.1,
                w * 0.04,
                FUR_SHADE,
                1.0,
            );
            t.rrect(
                cx - w * 0.2,
                gy - h * 0.12,
                w * 0.1,
                h * 0.1,
                w * 0.04,
                FUR_SHADE,
                1.0,
            );
            // back legs tucked
            t.rrect(
                cx + w * 0.18,
                gy - h * 0.2,
                w * 0.1,
                h * 0.16,
                w * 0.05,
                FUR_SHADE,
                1.0,
            );
            // tail up high
            tail(t, cx + w * 0.3, gy - h * 0.42, 1.0, w, h);
            head(t, cx - w * 0.34, gy - h * 0.26, h * 0.16, -0.5);
        }
    }
}

/// A swishy tail curving up from `(bx, by)` toward `dir` (±1 = right/left).
fn tail(t: &mut Tile, bx: f32, by: f32, dir: f32, w: f32, h: f32) {
    let seg = 7;
    for i in 0..seg {
        let s = i as f32 / (seg - 1) as f32;
        let x = bx + dir * (s * w * 0.18) + dir * (s * s) * w * 0.12;
        let y = by - s * h * 0.34;
        let r = (1.0 - s * 0.5) * w * 0.06 + 1.5;
        t.disc(x, y, r, FUR, 1.0);
    }
}

fn draw_star(t: &mut Tile) {
    let c = t.w as f32 * 0.5;
    let r = c * 0.92;
    // 4-point star: two crossed slim triangles each way.
    for &dir in &[1.0f32, -1.0] {
        t.tri(
            [(c, c - r), (c - r * 0.16, c), (c + r * 0.16, c)],
            (1.0, 1.0, 1.0),
            1.0,
        );
        t.tri(
            [(c, c + r), (c - r * 0.16, c), (c + r * 0.16, c)],
            (1.0, 1.0, 1.0),
            1.0,
        );
        t.tri(
            [(c - r, c), (c, c - r * 0.16), (c, c + r * 0.16)],
            (1.0, 1.0, 1.0),
            1.0,
        );
        t.tri(
            [(c + r, c), (c, c - r * 0.16), (c, c + r * 0.16)],
            (1.0, 1.0, 1.0),
            1.0,
        );
        let _ = dir;
    }
    t.disc(c, c, r * 0.22, (1.0, 1.0, 1.0), 1.0);
}

fn draw_house(t: &mut Tile) {
    let w = t.w as f32;
    let h = t.h as f32;
    let cx = w * 0.5;
    // body — a soft dusty-blue painted cottage: cool against the warm tabby so the cat
    // reads as its own shape in front of it, and a different hue from the green meadow.
    t.rrect(
        w * 0.16,
        h * 0.42,
        w * 0.68,
        h * 0.5,
        w * 0.04,
        (0.58, 0.68, 0.80),
        1.0,
    );
    // a subtle lower-wall shade for a touch of depth.
    t.rrect(
        w * 0.16,
        h * 0.74,
        w * 0.68,
        h * 0.18,
        w * 0.04,
        (0.50, 0.60, 0.72),
        0.6,
    );
    // roof
    t.tri(
        [(w * 0.08, h * 0.46), (cx, h * 0.06), (w * 0.92, h * 0.46)],
        (0.72, 0.40, 0.33),
        1.0,
    );
    // door (round-top)
    t.disc(cx, h * 0.66, w * 0.14, (0.12, 0.09, 0.11), 1.0);
    t.rrect(
        cx - w * 0.14,
        h * 0.66,
        w * 0.28,
        h * 0.26,
        0.0,
        (0.12, 0.09, 0.11),
        1.0,
    );
    // a little paw sign on the roof
    t.disc(cx, h * 0.28, h * 0.06, (0.95, 0.85, 0.6), 1.0);
}

fn draw_cloud(t: &mut Tile) {
    let w = t.w as f32;
    let h = t.h as f32;
    let cy = h * 0.55;
    for &(ox, oy, r) in &[
        (0.24f32, 0.1f32, 0.26f32),
        (0.45, -0.1, 0.34),
        (0.66, 0.06, 0.28),
        (0.5, 0.18, 0.3),
    ] {
        t.disc(
            w * ox + w * 0.05,
            cy + h * oy,
            h * r + w * 0.06,
            (1.0, 1.0, 1.0),
            1.0,
        );
    }
}

fn draw_flower(t: &mut Tile) {
    let c = t.w as f32 * 0.5;
    let r = c * 0.5;
    for i in 0..5 {
        let a = i as f32 / 5.0 * std::f32::consts::TAU;
        let px = c + a.cos() * r;
        let py = c + a.sin() * r;
        t.disc(px, py, r * 0.62, FUR, 1.0); // tintable petals (grayscale)
    }
    t.disc(c, c, r * 0.55, (1.0, 0.85, 0.42), 1.0); // fixed yellow center
}

fn draw_grass(t: &mut Tile) {
    let w = t.w as f32;
    let h = t.h as f32;
    let blades = 6;
    for i in 0..blades {
        let x = w * (0.1 + 0.8 * i as f32 / (blades - 1) as f32);
        let lean = ((i % 2) as f32 - 0.5) * w * 0.12;
        t.tri(
            [(x - w * 0.03, h), (x + lean, h * 0.1), (x + w * 0.03, h)],
            FUR,
            1.0,
        );
    }
}

/// A shaded planet: a grayscale sphere (bright top-left → dark lower-right) so the draw
/// tint colours it, plus a couple of darker bands for a gas-giant feel.
fn draw_planet(t: &mut Tile) {
    let c = t.w as f32 * 0.5;
    let r = c * 0.92;
    // sphere shading via concentric discs offset toward the light.
    let steps = 14;
    for i in 0..steps {
        let f = i as f32 / (steps - 1) as f32; // 0 outer → 1 inner/lit
        let rr = r * (1.0 - f * 0.55);
        let off = r * 0.22 * f;
        let g = 0.45 + 0.55 * f; // dark rim → bright lit center
        t.disc(c - off, c - off, rr, (g, g, g), 1.0);
    }
    // a couple of subtle darker bands.
    for &(by, h) in &[(0.42f32, 0.05f32), (0.62, 0.06)] {
        t.fill(
            0,
            (t.h as f32 * by) as i32,
            t.w as i32,
            (t.h as f32 * (by + h)) as i32,
            (0.4, 0.4, 0.4),
            0.35,
            |x, y| {
                let dx = (x - c) / r;
                let dy = (y - c) / r;
                dx * dx + dy * dy <= 1.0
            },
        );
    }
}

// Orca colours are FIXED (not tint-driven) so the killer whale stays iconic on any theme.
const OBLACK: (f32, f32, f32) = (0.09, 0.10, 0.14);
const OWHITE: (f32, f32, f32) = (0.95, 0.96, 0.99);

/// An orca facing RIGHT (the host flips for left): black body + white belly + eye patch,
/// dorsal + pectoral fins, tail fluke.
fn draw_orca(t: &mut Tile) {
    let w = t.w as f32;
    let h = t.h as f32;
    let cx = w * 0.5;
    let cy = h * 0.5;
    // tail fluke (left)
    t.tri(
        [(w * 0.18, cy), (w * 0.02, cy - h * 0.24), (w * 0.12, cy)],
        OBLACK,
        1.0,
    );
    t.tri(
        [(w * 0.18, cy), (w * 0.02, cy + h * 0.24), (w * 0.12, cy)],
        OBLACK,
        1.0,
    );
    // dorsal fin (top)
    t.tri(
        [
            (cx - w * 0.06, cy - h * 0.22),
            (cx + w * 0.04, cy - h * 0.5),
            (cx + w * 0.12, cy - h * 0.2),
        ],
        OBLACK,
        1.0,
    );
    // pectoral fin (lower)
    t.tri(
        [
            (cx + w * 0.0, cy + h * 0.08),
            (cx - w * 0.08, cy + h * 0.42),
            (cx + w * 0.14, cy + h * 0.16),
        ],
        OBLACK,
        1.0,
    );
    // body + head bulge
    t.ellipse(cx, cy, w * 0.40, h * 0.27, OBLACK, 1.0);
    t.disc(w * 0.80, cy, h * 0.23, OBLACK, 1.0);
    // white belly (lower)
    t.ellipse(cx + w * 0.06, cy + h * 0.13, w * 0.3, h * 0.12, OWHITE, 1.0);
    // white eye patch + eye
    t.ellipse(w * 0.76, cy - h * 0.07, w * 0.06, h * 0.09, OWHITE, 1.0);
    t.disc(w * 0.84, cy - h * 0.01, h * 0.04, OBLACK, 1.0);
}

/// A small fish facing RIGHT (grayscale → tinted): body + tail + top fin + eye.
fn draw_fish(t: &mut Tile) {
    let w = t.w as f32;
    let h = t.h as f32;
    let cx = w * 0.46;
    let cy = h * 0.5;
    t.ellipse(cx, cy, w * 0.34, h * 0.34, FUR, 1.0);
    t.tri(
        [
            (w * 0.14, cy),
            (w * 0.02, cy - h * 0.34),
            (w * 0.02, cy + h * 0.34),
        ],
        FUR,
        1.0,
    );
    t.tri(
        [
            (cx - w * 0.06, cy - h * 0.28),
            (cx, cy - h * 0.5),
            (cx + w * 0.08, cy - h * 0.26),
        ],
        FUR_SHADE,
        1.0,
    );
    t.disc(cx + w * 0.2, cy - h * 0.03, h * 0.07, DARK, 1.0);
    t.disc(cx + w * 0.22, cy - h * 0.06, h * 0.03, CATCH, 0.9);
}

/// A rising bubble: a soft translucent disc with a bright highlight.
fn draw_bubble(t: &mut Tile) {
    let c = t.w as f32 * 0.5;
    let r = c * 0.85;
    t.disc(c, c, r, (0.85, 0.92, 1.0), 0.22);
    t.disc(c, c, r * 0.96, (0.7, 0.85, 1.0), 0.12);
    t.disc(c - r * 0.3, c - r * 0.3, r * 0.24, (1.0, 1.0, 1.0), 0.8);
}

/// A wavy kelp strand rooted at the bottom (grayscale → tinted green).
fn draw_kelp(t: &mut Tile) {
    let w = t.w as f32;
    let h = t.h as f32;
    let n = 20;
    for i in 0..n {
        let s = i as f32 / (n - 1) as f32; // 0 base(bottom) → 1 tip(top)
        let y = h * (1.0 - s * 0.98);
        let x = w * 0.5 + (s * 6.0).sin() * w * 0.22;
        let r = (1.0 - s * 0.55) * w * 0.13 + 2.0;
        t.disc(x, y, r, if i % 2 == 0 { FUR } else { FUR_SHADE }, 1.0);
    }
}

/// A drifting jellyfish: a translucent bell + wavy tentacles (grayscale → tinted).
fn draw_jelly(t: &mut Tile) {
    let w = t.w as f32;
    let h = t.h as f32;
    let cx = w * 0.5;
    // bell (upper dome)
    t.fill(0, 0, w as i32, (h * 0.5) as i32, FUR, 0.8, |x, y| {
        let nx = (x - cx) / (w * 0.42);
        let ny = (y - h * 0.42) / (h * 0.4);
        nx * nx + ny * ny <= 1.0 && y <= h * 0.42
    });
    // tentacles
    for i in 0..5 {
        let base = cx + (i as f32 - 2.0) * w * 0.15;
        for k in 0..11 {
            let s = k as f32 / 10.0;
            let yy = h * 0.4 + s * h * 0.56;
            let xx = base + (s * 7.0 + i as f32).sin() * w * 0.05;
            t.disc(xx, yy, w * 0.035 * (1.0 - s * 0.4) + 1.0, FUR_SHADE, 0.65);
        }
    }
}

fn draw_butterfly(t: &mut Tile) {
    let w = t.w as f32;
    let h = t.h as f32;
    let cx = w * 0.5;
    let cy = h * 0.5;
    // wings (tintable)
    t.ellipse(cx - w * 0.22, cy - h * 0.12, w * 0.2, h * 0.26, FUR, 1.0);
    t.ellipse(cx + w * 0.22, cy - h * 0.12, w * 0.2, h * 0.26, FUR, 1.0);
    t.ellipse(
        cx - w * 0.18,
        cy + h * 0.2,
        w * 0.14,
        h * 0.18,
        FUR_SHADE,
        1.0,
    );
    t.ellipse(
        cx + w * 0.18,
        cy + h * 0.2,
        w * 0.14,
        h * 0.18,
        FUR_SHADE,
        1.0,
    );
    // body
    t.rrect(
        cx - w * 0.03,
        cy - h * 0.3,
        w * 0.06,
        h * 0.6,
        w * 0.03,
        DARK,
        1.0,
    );
    // antennae
    t.disc(cx - w * 0.06, cy - h * 0.36, 1.6, DARK, 1.0);
    t.disc(cx + w * 0.06, cy - h * 0.36, 1.6, DARK, 1.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bake_is_deterministic_and_sized() {
        let a = Atlas::bake(1);
        let b = Atlas::bake(1);
        assert_eq!(a.rgba, b.rgba, "baking is deterministic");
        assert_eq!(a.rgba.len(), (a.width * a.height * 4) as usize);
        assert!(a.height.is_power_of_two());
    }

    #[test]
    fn every_sprite_has_a_nonempty_rect_inside_the_atlas() {
        let a = Atlas::bake(1);
        for s in Sprite::ALL {
            let (ax, ay, aw, ah) = Atlas::rect(&a, s);
            assert!(aw > 0 && ah > 0, "{s:?} has a real rect");
            assert!(
                ax as u32 + aw as u32 <= a.width && ay as u32 + ah as u32 <= a.height,
                "{s:?} rect fits in the atlas"
            );
        }
    }

    #[test]
    fn cat_sprites_have_visible_coverage() {
        // A baked cat must actually put down opaque pixels (not an empty tile).
        let a = Atlas::bake(1);
        for s in [
            Sprite::CatCurl,
            Sprite::CatSit,
            Sprite::CatWalkA,
            Sprite::CatPounce,
        ] {
            let (ax, ay, aw, ah) = Atlas::rect(&a, s);
            let mut opaque = 0u32;
            for y in ay..ay + ah {
                for x in ax..ax + aw {
                    let i = ((y as u32 * a.width + x as u32) * 4 + 3) as usize;
                    if a.rgba[i] > 200 {
                        opaque += 1;
                    }
                }
            }
            let area = aw as u32 * ah as u32;
            assert!(
                opaque > area / 12,
                "{s:?}: only {opaque}/{area} opaque texels — silhouette too thin"
            );
        }
    }

    /// Dump the atlas to a PNG for visual iteration: `ATERM_SCENE_DUMP=/path cargo test
    /// -p aterm-scene dump_atlas_png -- --ignored --nocapture`.
    #[test]
    #[ignore = "visual aid; writes a PNG only when ATERM_SCENE_DUMP is set"]
    fn dump_atlas_png() {
        let Ok(dir) = std::env::var("ATERM_SCENE_DUMP") else {
            return;
        };
        let a = Atlas::bake(1);
        let path = format!("{dir}/scene_atlas.png");
        let file = std::fs::File::create(&path).expect("create png");
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), a.width, a.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().expect("png header");
        w.write_image_data(&a.rgba).expect("png data");
        println!("wrote {path} ({}x{})", a.width, a.height);
    }
}

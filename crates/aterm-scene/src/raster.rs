// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The **reference software compositor**: turn a [`SceneFrame`] + [`Atlas`] into RGBA8
//! pixels. This is the single source of the scene's pixel math — the CPU renderer
//! composites the band the same way and the GPU shader reproduces it, so CPU/GPU/headless
//! all land identical pixels (parity by construction). It is also what the golden-frame
//! tests and the PNG dumps render through.
//!
//! Blending matches aterm's existing contracts exactly: `over` sprites are straight-alpha
//! source-over (like inline images), `add` sprites are premultiplied saturating-add (like
//! the LUMEN glow / `DecoBlend::Add`). Sampling is bilinear for smooth scaling.

use crate::atlas::Atlas;
use crate::scene::{LocalSprite, SceneFrame};
use crate::{clampf, scene::Env};

/// Bilinearly sample the atlas inside the given texel rect at normalized `(u, v)` in
/// `[0,1]`, returning straight-alpha linear-ish `[r,g,b,a]` in `[0,1]`. Out-of-range
/// samples clamp to the rect edge (no bleed into neighbouring sprites).
#[must_use]
pub fn sample(atlas: &Atlas, rect: (u16, u16, u16, u16), u: f32, v: f32) -> [f32; 4] {
    let (ax, ay, aw, ah) = rect;
    if aw == 0 || ah == 0 {
        return [0.0; 4];
    }
    // Texel-space coordinate within the sprite (clamped to the inner texel grid).
    let fx = clampf(u, 0.0, 1.0) * (aw as f32 - 1.0);
    let fy = clampf(v, 0.0, 1.0) * (ah as f32 - 1.0);
    let x0 = fx.floor() as u32;
    let y0 = fy.floor() as u32;
    let x1 = (x0 + 1).min(aw as u32 - 1);
    let y1 = (y0 + 1).min(ah as u32 - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let texel = |lx: u32, ly: u32| -> [f32; 4] {
        // The rect fields are u16 and the offsets are clamped inside the sprite
        // (a no-op: callers already pass `lx < aw`, `ly < ah`), so the widened
        // usize address math cannot wrap on a 64-bit target. The checked fetch
        // makes the lookup total; for a well-formed atlas
        // (`rgba.len() == width*height*4`, rects inside) it never misses, and a
        // malformed one reads as transparent black exactly like a zero-size rect.
        let lx = lx.min((aw as u32).saturating_sub(1)) as usize;
        let ly = ly.min((ah as u32).saturating_sub(1)) as usize;
        let px = ax as usize + lx;
        let py = ay as usize + ly;
        // Saturating is a no-op here (u16-derived coordinates on a 64-bit target);
        // the slice-pattern destructure replaces four indexed reads with one total
        // match, so no bounds obligations remain.
        let i = py
            .saturating_mul(atlas.width as usize)
            .saturating_add(px)
            .saturating_mul(4);
        // Copy the four bytes into a fixed `[u8; 4]` through the checked slice API,
        // then destructure the *array* (irrefutable, statically-bounded reads — no
        // per-element slice-bounds obligation). All-or-nothing, identical to the
        // previous behaviour: an out-of-range fetch leaves the buffer transparent
        // black, exactly like a zero-size rect. Mirrors the guarded `copy_from_slice`
        // in `Atlas::blit`.
        let mut buf = [0u8; 4];
        if let Some(src) = atlas.rgba.get(i..i.saturating_add(4))
            && src.len() == buf.len()
        {
            buf.copy_from_slice(src);
        }
        let [tr, tg, tb, ta] = buf;
        [
            tr as f32 / 255.0,
            tg as f32 / 255.0,
            tb as f32 / 255.0,
            ta as f32 / 255.0,
        ]
    };
    let a = texel(x0, y0);
    let b = texel(x1, y0);
    let c = texel(x0, y1);
    let d = texel(x1, y1);
    let mut out = [0.0f32; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bot = c[k] + (d[k] - c[k]) * tx;
        out[k] = top + (bot - top) * ty;
    }
    out
}

/// Unpack `0x00RRGGBB` into `[r,g,b]` in `[0,1]`.
fn unpack(c: u32) -> [f32; 3] {
    [
        ((c >> 16) & 0xff) as f32 / 255.0,
        ((c >> 8) & 0xff) as f32 / 255.0,
        (c & 0xff) as f32 / 255.0,
    ]
}

/// An RGBA8 (straight-alpha) software framebuffer.
pub struct Canvas {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

impl Canvas {
    /// A canvas filled with opaque `bg` (`0x00RRGGBB`). Dimensions are clamped to
    /// 4096×4096 — far above any scene band the compositor is ever asked to render —
    /// which keeps the framebuffer allocation provably bounded.
    #[must_use]
    pub fn filled(w: u32, h: u32, bg: u32) -> Self {
        const MAX_DIM: u32 = 4096;
        let w = w.min(MAX_DIM);
        let h = h.min(MAX_DIM);
        let c = unpack(bg);
        let px = [
            (c[0] * 255.0 + 0.5) as u8,
            (c[1] * 255.0 + 0.5) as u8,
            (c[2] * 255.0 + 0.5) as u8,
            255u8,
        ];
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for chunk in rgba.as_chunks_mut::<4>().0 {
            *chunk = px;
        }
        Canvas { w, h, rgba }
    }

    fn blend_px(&mut self, x: u32, y: u32, rgb: [f32; 3], a: f32, additive: bool) {
        if x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        // Widened, saturating address math (a no-op for any canvas `filled` can
        // build, where `rgba.len() == w*h*4` and `w,h <= 4096`) plus a checked,
        // destructured fetch: on every real canvas the lookup hits and the pixel
        // bytes read/written are identical to the unchecked version.
        let i = (y as usize)
            .saturating_mul(self.w as usize)
            .saturating_add(x as usize)
            .saturating_mul(4);
        let Some([pr, pg, pb, _pa]) = self.rgba.get_mut(i..i.saturating_add(4)) else {
            return;
        };
        let d = [*pr as f32 / 255.0, *pg as f32 / 255.0, *pb as f32 / 255.0];
        let out = if additive {
            // premultiplied saturating add (like the LUMEN glow): dst + src*a
            [
                clampf(d[0] + rgb[0] * a, 0.0, 1.0),
                clampf(d[1] + rgb[1] * a, 0.0, 1.0),
                clampf(d[2] + rgb[2] * a, 0.0, 1.0),
            ]
        } else {
            // straight-alpha source-over: dst*(1-a) + src*a
            [
                d[0] * (1.0 - a) + rgb[0] * a,
                d[1] * (1.0 - a) + rgb[1] * a,
                d[2] * (1.0 - a) + rgb[2] * a,
            ]
        };
        // Irrefutable destructure of the fixed-size `[f32; 3]` (no indexing, hence no
        // bounds obligation on the branch-merged array); the arithmetic is unchanged.
        let [o0, o1, o2] = out;
        *pr = (o0 * 255.0 + 0.5) as u8;
        *pg = (o1 * 255.0 + 0.5) as u8;
        *pb = (o2 * 255.0 + 0.5) as u8;
        // keep the canvas opaque (it's a background); alpha stays 255.
    }

    /// Composite one sprite (the per-sprite kernel shared with the CPU renderer).
    pub fn draw_sprite(&mut self, atlas: &Atlas, s: &LocalSprite, additive: bool) {
        let rect = Atlas::rect(atlas, s.sprite);
        // Irrefutable destructures of the fixed-size arrays (no indexing, hence no
        // bounds obligations); the arithmetic is unchanged.
        let [tint_r, tint_g, tint_b] = unpack(s.tint);
        let x0 = s.dst.x.floor().max(0.0) as u32;
        let y0 = s.dst.y.floor().max(0.0) as u32;
        let x1 = ((s.dst.x + s.dst.w).ceil() as i64).clamp(0, self.w as i64) as u32;
        let y1 = ((s.dst.y + s.dst.h).ceil() as i64).clamp(0, self.h as i64) as u32;
        if s.dst.w <= 0.0 || s.dst.h <= 0.0 {
            return;
        }
        for py in y0..y1 {
            for px in x0..x1 {
                // dest-pixel center → normalized sprite uv
                let mut u = (px as f32 + 0.5 - s.dst.x) / s.dst.w;
                let v = (py as f32 + 0.5 - s.dst.y) / s.dst.h;
                if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                    continue;
                }
                if s.flip_x {
                    u = 1.0 - u;
                }
                let [tex_r, tex_g, tex_b, tex_a] = sample(atlas, rect, u, v);
                let a = tex_a * s.alpha;
                if a <= 0.0 {
                    continue;
                }
                let rgb = [tex_r * tint_r, tex_g * tint_g, tex_b * tint_b];
                self.blend_px(px, py, rgb, a, additive);
            }
        }
    }
}

/// Composite a whole [`SceneFrame`] over an opaque `bg` into a fresh [`Canvas`] — the
/// reference render path (over sprites first, then additive light). `env` supplies the
/// pixel size.
#[must_use]
pub fn composite(env: &Env, frame: &SceneFrame, atlas: &Atlas, bg: u32) -> Canvas {
    let mut c = Canvas::filled(env.w.max(1.0) as u32, env.h.max(1.0) as u32, bg);
    for s in &frame.over {
        c.draw_sprite(atlas, s, false);
    }
    for s in &frame.add {
        c.draw_sprite(atlas, s, true);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalSprite, Rect, SceneFrame, Sprite};

    #[test]
    fn composite_produces_a_nonempty_frame() {
        // A synthetic frame (no concrete scene) is enough to prove the reference compositor
        // paints: a full-bleed opaque block over the cleared background.
        let env = Env::new(480.0, 160.0);
        let mut frame = SceneFrame::new();
        frame.push_over(LocalSprite::new(
            Sprite::Pixel,
            Rect::new(0.0, 0.0, 480.0, 120.0),
        ));
        frame.push_add(LocalSprite::new(
            Sprite::Glow,
            Rect::new(200.0, 40.0, 120.0, 120.0),
        ));
        let atlas = crate::Atlas::bake(1);
        let canvas = composite(&env, &frame, &atlas, 0x001A1B26);
        assert_eq!(canvas.rgba.len(), (480 * 160 * 4) as usize);
        let changed = canvas
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| !(p[0] == 0x1A && p[1] == 0x1B && p[2] == 0x26))
            .count();
        assert!(
            changed > 1000,
            "compositor actually painted pixels: {changed}"
        );
    }
}

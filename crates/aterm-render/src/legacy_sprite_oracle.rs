// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sprite path the fx engine replaced (`SpriteQuad`, `FreeSprite` and
//! their atlases, until lane B7 of `docs/DESIGN-card-engine-2026-10-19.md`),
//! kept as the differential oracle the companions and the rain are held to:
//! the stamp is the old `stamp_free_sprite` (whose white-rain and cat-quad
//! twins drew the same bytes over their texels), the knock boxes are the old
//! `knock_rects`, and [`crate::Renderer::set_legacy_sprites`] draws them where
//! the old composite did, so a frame through either path can be compared
//! byte for byte.

use std::sync::Arc;

use aterm_core::render::{FxArt, FxTexels};

use crate::blend;

/// One sprite of the old path: a destination box in grid-interior pixels,
/// sampled by texel centre from the `src` sub-rectangle of `art`'s straight
/// RGBA texels, tinted and faded like a Linear fx sprite.
#[derive(Clone, Debug)]
pub struct LegacySprite {
    /// Left edge of the destination box.
    pub x: i32,
    /// Top edge of the destination box.
    pub y: i32,
    /// Destination width.
    pub w: u16,
    /// Destination height.
    pub h: u16,
    /// The texels (the old atlas).
    pub art: Arc<FxArt>,
    /// `[x, y, w, h]` within `art`.
    pub src: [u16; 4],
    /// Multiplied into every texel's colour.
    pub tint: u32,
    /// Multiplied into every texel's alpha.
    pub alpha: u8,
    /// Mirror the source horizontally.
    pub flip_x: bool,
    /// Drawn under the text and knocked out (the old `FreeZ::UnderText`).
    pub under: bool,
}

/// The sprites the old composite drew, in draw order: those `under` the
/// text right after the background pass, the rest over the decorations.
#[derive(Clone, Debug, Default)]
pub struct LegacySprites {
    /// The sprites.
    pub sprites: Vec<LegacySprite>,
}

impl LegacySprites {
    /// The old `knock_rects`: the frame-pixel box of every under-text sprite
    /// with a non-zero alpha and size.
    pub fn knock_rects(&self, pad_x: i32, pad_y: i32) -> impl Iterator<Item = [i32; 4]> {
        self.sprites
            .iter()
            .filter(|s| s.under && s.alpha > 0 && s.w > 0 && s.h > 0)
            .map(move |s| {
                let x0 = pad_x.saturating_add(s.x);
                let y0 = pad_y.saturating_add(s.y);
                [
                    x0,
                    y0,
                    x0.saturating_add(i32::from(s.w)),
                    y0.saturating_add(i32::from(s.h)),
                ]
            })
    }
}

/// The old `stamp_free_sprite`: `s` over the pixel rows `[y_lo, y_hi)` of a
/// `w × h` frame whose grid interior starts at `(pad_x, pad_y)`.
#[allow(clippy::too_many_arguments)]
pub fn stamp(
    pixels: &mut [u32],
    w: usize,
    h: usize,
    pad_x: usize,
    pad_y: usize,
    s: &LegacySprite,
    y_lo: usize,
    y_hi: usize,
) {
    assert_eq!(
        s.art.texels(),
        FxTexels::Rgba8Straight,
        "the old atlases held straight RGBA"
    );
    let [ax, ay, aw, ah] = s.src;
    if s.w == 0 || s.h == 0 || aw == 0 || ah == 0 {
        return;
    }
    let (atlas_w, atlas_h) = (u32::from(s.art.w()), u32::from(s.art.h()));
    if u32::from(ax) + u32::from(aw) > atlas_w || u32::from(ay) + u32::from(ah) > atlas_h {
        return;
    }
    let rgba = s.art.data();
    let ox = pad_x as i32 + s.x;
    let oy = pad_y as i32 + s.y;
    let px0 = ox.max(0) as usize;
    let py0 = (oy.max(0) as usize).max(y_lo);
    let xend = ((ox + i32::from(s.w)).max(0) as usize).min(w);
    let yend = ((oy + i32::from(s.h)).max(0) as usize).min(h).min(y_hi);
    if px0 >= xend || py0 >= yend {
        return;
    }
    let (aw, ah) = (usize::from(aw), usize::from(ah));
    let (dw, dh) = (usize::from(s.w), usize::from(s.h));
    let tr = (s.tint >> 16) & 0xff;
    let tg = (s.tint >> 8) & 0xff;
    let tb = s.tint & 0xff;
    let qa = u32::from(s.alpha);
    let mul8 = |c: u32, f: u32| -> u32 { (c * f + 127) / 255 };
    let one_to_one = aw == dw && ah == dh;
    for py in py0..yend {
        let dy = (py as i32 - oy) as usize;
        let sy = if one_to_one {
            dy
        } else {
            (((2 * dy + 1) * ah) / (2 * dh)).min(ah - 1)
        };
        let arow = (usize::from(ay) + sy) * atlas_w as usize;
        for px in px0..xend {
            let dx = (px as i32 - ox) as usize;
            let sx0 = if one_to_one {
                dx
            } else {
                (((2 * dx + 1) * aw) / (2 * dw)).min(aw - 1)
            };
            let sx = if s.flip_x { aw - 1 - sx0 } else { sx0 };
            let i = (arow + usize::from(ax) + sx) * 4;
            let a8 = mul8(u32::from(rgba[i + 3]), qa);
            if a8 == 0 {
                continue;
            }
            let cr = mul8(u32::from(rgba[i]), tr);
            let cg = mul8(u32::from(rgba[i + 1]), tg);
            let cb = mul8(u32::from(rgba[i + 2]), tb);
            let idx = py * w + px;
            pixels[idx] = blend(pixels[idx], (cr << 16) | (cg << 8) | cb, a8 as u8);
        }
    }
}

impl LegacySprite {
    /// The old path's sprite that drew what `s` draws, where one existed:
    /// the old sprite had no clip, so a cut `s` maps only when its visible
    /// window covers whole texels (a peek's window at 1:1, a pane's edge at
    /// an integer scale) and is not mirrored inside a narrowed window (the
    /// old mirror turned the window, the new one turns the art). `None`
    /// otherwise, and for a blank `s`.
    #[must_use]
    pub fn of(s: &crate::sprites::FreeSprite) -> Option<Self> {
        if s.is_blank() || s.w == 0 || s.h == 0 {
            return None;
        }
        let (aw, ah) = (s.art.w(), s.art.h());
        let v = s.visible();
        let whole = v.x0 == s.x
            && v.y0 == s.y
            && v.x1 == s.x.saturating_add(i32::from(s.w))
            && v.y1 == s.y.saturating_add(i32::from(s.h));
        let src = if whole {
            [0, 0, aw, ah]
        } else {
            if !s.w.is_multiple_of(aw) || !s.h.is_multiple_of(ah) {
                return None;
            }
            let (kx, ky) = (i32::from(s.w / aw), i32::from(s.h / ah));
            let (dx, dy) = (v.x0 - s.x, v.y0 - s.y);
            let (vw, vh) = (v.x1 - v.x0, v.y1 - v.y0);
            if dx % kx != 0 || vw % kx != 0 || dy % ky != 0 || vh % ky != 0 {
                return None;
            }
            if s.flip_x && vw != i32::from(s.w) {
                return None;
            }
            [
                u16::try_from(dx / kx).ok()?,
                u16::try_from(dy / ky).ok()?,
                u16::try_from(vw / kx).ok()?,
                u16::try_from(vh / ky).ok()?,
            ]
        };
        Some(Self {
            x: v.x0,
            y: v.y0,
            w: u16::try_from(v.x1 - v.x0).ok()?,
            h: u16::try_from(v.y1 - v.y0).ok()?,
            art: Arc::clone(&s.art),
            src,
            tint: s.tint,
            alpha: s.alpha,
            flip_x: s.flip_x,
            under: s.z == crate::sprites::FreeZ::UnderText,
        })
    }
}

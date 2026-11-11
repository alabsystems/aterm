// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ⬆️'s hover ring on a composer's top rule: while the pointer is on the ⬆️ of a newer CLI
//! build, one premultiplied sprite rings it, under the glyphs ([`content`], [`layer`]).
//!
//! The sprite is [`RING_CELLS`] cells by one, rastered on the CPU from the signed distance to one
//! pill 82% of the cell high, centred on the rule's `─` stroke and grown to clear the small
//! emoji's rows by a pixel inside its rim: a hairline rim in the rule's ink where the ring sits,
//! floored to 3:1 on the ground ([`ring_ink`]), washed [`RING_WASH`] inside.
//! Coverage is `clamp(0.5 - d, 0, 1)` at pixel centres. The harness switch beside the ⬆️ is words
//! alone and has no sprite.

use std::sync::Arc;

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, FxTexels, PxRect,
};

use crate::cli_marks::ICON_CONTRAST;
use crate::color_math::{floor_contrast_keeping_hue, hypot_f32};
use crate::effect_util::lerp_rgb as mix;

/// Where the ring takes its hue: the hue of the rule it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingHue {
    /// The rule's own tinted ink where the ring sits (`0x00RRGGBB`): a harness theme's arc at
    /// the arrow's column ([`crate::glass_inks::GlassInks::rule`]).
    Rule(u32),
    /// The harness off, or no harness theme: no hue, the ground moved toward the ink.
    Neutral,
}

/// What one ring sprite is drawn from: equal keys raster equal bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingKey {
    /// The cell, `(cw, ch)` device pixels.
    pub cell: (u16, u16),
    /// The pane's ground (`0x00RRGGBB`).
    pub ground: u32,
    /// The pane's ink.
    pub fg: u32,
    /// The ring's hue.
    pub hue: RingHue,
}

/// The ring's floor against the ground: the non-text floor.
pub const RING_CONTRAST: f32 = ICON_CONTRAST;

/// The cells the ring spans, centred on the arrow's two.
pub const RING_CELLS: u16 = 3;

/// The wash inside the ring.
pub const RING_WASH: f32 = 0.14;

/// The ring's ink: the rule's tint where it sits, or with no hue the ground moved toward the ink,
/// floored to 3:1 on the ground.
#[must_use]
#[inline]
pub const fn ring_ink(key: &RingKey) -> u32 {
    let (g, f) = (key.ground & 0x00FF_FFFF, key.fg & 0x00FF_FFFF);
    let ink = match key.hue {
        RingHue::Rule(rule) => rule & 0x00FF_FFFF,
        RingHue::Neutral => mix(g, f, 0.30),
    };
    floor_contrast_keeping_hue(ink, g, RING_CONTRAST)
}

/// A pill's signed distance: from pixel centre `(px, py)` to the pill `x0..x1` by `y0..y1`.
#[inline]
const fn pill(px: f32, (dy, dy_abs): (f32, f32), (a, b, r): (f32, f32, f32)) -> f32 {
    let dx = if px < a {
        a - px
    } else if px > b {
        px - b
    } else {
        return dy_abs - r;
    };
    hypot_f32(dx, dy) - r
}

/// Coverage at signed distance `d`.
#[inline]
const fn cov(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// `rgb` over the premultiplied pixel `p` at `alpha`.
#[inline]
const fn over(p: [f32; 4], rgb: [f32; 3], alpha: f32) -> [f32; 4] {
    if alpha <= 0.0 {
        return p;
    }
    let k = 1.0 - alpha;
    [
        rgb[0] * alpha + p[0] * k,
        rgb[1] * alpha + p[1] * k,
        rgb[2] * alpha + p[2] * k,
        alpha + p[3] * k,
    ]
}

/// A premultiplied channel `0..=1` as a byte.
#[inline]
const fn to_byte(v: f32) -> u8 {
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

/// The ring's pill `(x0, y0, x1, y1)` and its rim's width, in device pixels: 82% of the cell high,
/// centred where the procedural `─` stroke is (aterm-render's box metrics), grown where needed
/// to leave a clear pixel between the small emoji's rows and the rim.
#[inline]
const fn geometry((cw, ch): (u16, u16)) -> ((f32, f32, f32, f32), f32) {
    let w = RING_CELLS as f32 * cw as f32;
    let ch_f = ch as f32;
    let s = ch_f * (1.0 / 28.0);
    let h = (0.82 * ch_f).round().max(2.0);
    let min_cell = if cw < ch { cw } else { ch };
    let light_px = (min_cell as usize + 4) / 8;
    let light = (if light_px > 1 { light_px } else { 1 }) as f32;
    let cy = ((ch_f - light) * 0.5).floor() + light * 0.5;
    let rim = s.round().max(1.0);
    let y0 = (cy - h * 0.5).round().max(0.0);
    let y1 = (y0 + h).min(ch_f);
    let (top, rows) = aterm_core::terminal::Script::small_emoji_rows(ch as usize);
    let clear = 1.0 + rim;
    let y0 = y0.min(top as f32 - clear).max(0.0);
    let y1 = y1.max((top + rows) as f32 + clear).min(ch_f);
    ((1.5 * s, y0, w - 1.5 * s, y1), rim)
}

/// The ring's premultiplied `R, G, B, A` bytes, [`RING_CELLS`] cells by one: a hairline pill in
/// [`ring_ink`], washed [`RING_WASH`] inside.
#[must_use]
#[inline]
pub fn raster(key: &RingKey) -> (u16, u16, Vec<u8>) {
    let (cw, ch) = key.cell;
    let w = RING_CELLS.saturating_mul(cw);
    if w == 0 || ch == 0 {
        return (w, ch, Vec::new());
    }
    let ((x0, y0, x1, y1), rim) = geometry(key.cell);
    let r = ((y1 - y0) * 0.5).max(0.0);
    let cy = (y0 + y1) * 0.5;
    let pill_x = (x0 + r, (x1 - r).max(x0 + r), r);
    let ink = ring_ink(key);
    let ink_rgb = [
        ((ink >> 16) & 0xff) as f32 * (1.0 / 255.0),
        ((ink >> 8) & 0xff) as f32 * (1.0 / 255.0),
        (ink & 0xff) as f32 * (1.0 / 255.0),
    ];
    let wash_px = {
        let p = over([0.0; 4], ink_rgb, RING_WASH);
        let a = to_byte(p[3]);
        [
            to_byte(p[0]).min(a),
            to_byte(p[1]).min(a),
            to_byte(p[2]).min(a),
            a,
        ]
    };
    let mut out = Vec::with_capacity(usize::from(w) * usize::from(ch) * 4);
    let row_bytes = usize::from(w) * 4;
    let mut y = 0;
    while y < ch {
        let dy = f32::from(y) + 0.5 - cy;
        let dy_pair = (dy, dy.abs());
        let mid_d = dy_pair.1 - r;
        if mid_d >= 0.5 {
            out.resize(out.len() + row_bytes, 0);
            y += 1;
            continue;
        }
        let mid_px = if mid_d + rim <= -0.5 {
            wash_px
        } else {
            let cov_d = cov(mid_d);
            let mut p = over([0.0; 4], ink_rgb, cov_d * RING_WASH);
            p = over(p, ink_rgb, (cov_d - cov(mid_d + rim)).clamp(0.0, 1.0));
            let a = to_byte(p[3]);
            [
                to_byte(p[0]).min(a),
                to_byte(p[1]).min(a),
                to_byte(p[2]).min(a),
                a,
            ]
        };
        let mut x = 0;
        while x < w {
            let px = f32::from(x) + 0.5;
            if px >= pill_x.0 && px <= pill_x.1 {
                out.extend_from_slice(&mid_px);
                x += 1;
                continue;
            }
            let d = pill(px, dy_pair, pill_x);
            if d >= 0.5 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                x += 1;
                continue;
            }
            if d + rim <= -0.5 {
                out.extend_from_slice(&wash_px);
                x += 1;
                continue;
            }
            let cov_d = cov(d);
            let mut p = over([0.0; 4], ink_rgb, cov_d * RING_WASH);
            p = over(p, ink_rgb, (cov_d - cov(d + rim)).clamp(0.0, 1.0));
            let a = to_byte(p[3]);
            // Premultiplied: no channel above its alpha.
            out.extend_from_slice(&[
                to_byte(p[0]).min(a),
                to_byte(p[1]).min(a),
                to_byte(p[2]).min(a),
                a,
            ]);
            x += 1;
        }
        y += 1;
    }
    (w, ch, out)
}

/// The ring's sprite content ([`raster`]) at the content's origin; empty where the cell has no
/// size.
#[must_use]
#[inline]
pub fn content(key: &RingKey) -> Arc<FxContent> {
    let (w, h, bytes) = raster(key);
    let items = if w == 0 || h == 0 {
        Vec::new()
    } else if let Some(art) = FxArt::new(w, h, FxTexels::Rgba8Premul, bytes) {
        vec![FxItem::Sprite(FxSprite {
            x: 0,
            y: 0,
            dw: w,
            dh: h,
            flip_x: false,
            art: Arc::new(art),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })]
    } else {
        Vec::new()
    };
    Arc::new(FxContent::new(items, Vec::new()))
}

/// The [`FxLayer`] placing `content` under the terminal glyphs at grid `offset`, inside `clip`.
#[must_use]
#[inline]
pub fn layer(id: u64, content: Arc<FxContent>, offset: (i32, i32), clip: PxRect) -> FxLayer {
    let mut layer = FxLayer::new(id, content, FxSlot::Under, FxSpace::Grid, FxBlend::Exact);
    layer.offset = offset;
    layer.clip = clip;
    layer
}

#[cfg(test)]
#[path = "up_ring_tests.rs"]
mod tests;

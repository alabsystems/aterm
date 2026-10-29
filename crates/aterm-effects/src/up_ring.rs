// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ⬆️'s hover ring on a composer's top rule: while the pointer is on the ⬆️ of a newer CLI
//! build, one premultiplied sprite rings it, under the glyphs ([`content`], [`layer`]).
//!
//! The sprite is [`RING_CELLS`] cells by one, rastered on the CPU from the signed distance to one
//! pill 82% of the cell high, centred on the rule's `─` stroke and grown to clear the small
//! emoji's rows by a pixel inside its rim: a hairline rim in the rule's ink at
//! the line's end, floored to 3:1 on the ground ([`ring_ink`]), washed [`RING_WASH`] inside.
//! Coverage is `clamp(0.5 - d, 0, 1)` at pixel centres. The harness switch beside the ⬆️ is words
//! alone and has no sprite.

use std::sync::Arc;

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, FxTexels, PxRect,
};

use crate::cli_marks::ICON_CONTRAST;
use crate::color_math::{floor_contrast_keeping_hue, hypot_f32};
use crate::harness_card::{CardPalette, dark_ground};

/// Where the ring takes its hue: the hue of the rule it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingHue {
    /// A harness theme's arc at its end: Cruise's spectrum, Land's sunset, Autopilot Off's
    /// moonbow.
    Arc(CardPalette),
    /// No harness theme: one hue, the chrome band's accent (`0x00RRGGBB`).
    Accent(u32),
    /// The harness is off: no hue, the ground moved toward the ink.
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
    /// The rule's own ink, which the arc is laid over.
    pub rule: u32,
    /// The ring's hue.
    pub hue: RingHue,
}

/// The ring's floor against the ground: the non-text floor.
pub const RING_CONTRAST: f32 = ICON_CONTRAST;

/// The cells the ring spans, centred on the arrow's two.
pub const RING_CELLS: u16 = 3;

/// The wash inside the ring.
pub const RING_WASH: f32 = 0.14;

/// `a` moved `t` of the way toward `b`, per channel (`0x00RRGGBB`).
#[inline]
const fn mix(a: u32, b: u32, t: f32) -> u32 {
    #[inline]
    const fn ch(a: u32, b: u32, t: f32, s: u32) -> u32 {
        let (x, y) = (((a >> s) & 0xff) as f32, ((b >> s) & 0xff) as f32);
        (x + (y - x) * t).round().clamp(0.0, 255.0) as u32
    }
    (ch(a, b, t, 16) << 16) | (ch(a, b, t, 8) << 8) | ch(a, b, t, 0)
}

/// The ring's ink: the rule's tint at the line's end (the arc lifted a quarter toward the ink,
/// laid 68% over the rule, as the gui inks the rule's `─`), floored to 3:1 on the ground.
#[must_use]
#[inline]
pub const fn ring_ink(key: &RingKey) -> u32 {
    let (g, f) = (key.ground, key.fg);
    let ink = match key.hue {
        RingHue::Arc(palette) => mix(
            key.rule,
            mix(palette.art(1.0, dark_ground(g)), f, 0.25),
            0.68,
        ),
        RingHue::Accent(accent) => accent,
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
    let art = if w == 0 || h == 0 {
        None
    } else {
        FxArt::new(w, h, FxTexels::Rgba8Premul, bytes)
    };
    let item = art.map(|art| {
        FxItem::Sprite(FxSprite {
            x: 0,
            y: 0,
            dw: w,
            dh: h,
            flip_x: false,
            art: Arc::new(art),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })
    });
    Arc::new(FxContent::new(item.into_iter().collect(), Vec::new()))
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
mod tests {
    use super::*;
    use crate::color_math::contrast_ratio;

    const DARK: u32 = 0x0011_1318;
    const DARK_FG: u32 = 0x00D0_D2D6;
    const PAPER: u32 = 0x00F6_F7F9;
    const PAPER_FG: u32 = 0x002B_2D33;

    fn key(ground: u32, fg: u32, hue: RingHue) -> RingKey {
        RingKey {
            cell: (15, 28),
            ground,
            fg,
            rule: mix(ground, fg, 0.35),
            hue,
        }
    }

    fn px(bytes: &[u8], w: u16, x: u16, y: u16) -> [u8; 4] {
        let at = (usize::from(y) * usize::from(w) + usize::from(x)) * 4;
        [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
    }

    /// THE RING IS A HAIRLINE PILL ROUND THE ARROW: one sprite three cells wide at the content's
    /// origin; its corners are clear, its middle is the wash, its left end at mid-height is the
    /// ring's ink, and no channel exceeds its alpha. CONTROL: a key with no cell size rasters
    /// nothing.
    #[test]
    fn the_ring_is_a_hairline_pill_round_the_arrow() {
        for (ground, fg) in [(DARK, DARK_FG), (PAPER, PAPER_FG)] {
            let k = key(ground, fg, RingHue::Arc(CardPalette::Cruise));
            let shown = content(&k);
            let items = shown.items();
            assert_eq!(items.len(), 1, "the ring alone");
            let FxItem::Sprite(ring) = &items[0] else {
                panic!("the ring is a sprite: {:?}", items[0]);
            };
            assert_eq!((ring.x, ring.dw, ring.dh), (0, 45, 28));
            let (w, h, bytes) = raster(&k);
            assert_eq!((w, h), (45, 28));
            assert_eq!(px(&bytes, w, 0, 0)[3], 0, "the corner is clear");
            assert_eq!(px(&bytes, w, w - 1, h - 1)[3], 0, "the far corner is clear");
            let wash = (RING_WASH * 255.0).round() as u8;
            let middle = px(&bytes, w, w / 2, h / 2)[3];
            assert!(middle.abs_diff(wash) <= 1, "the wash: {middle}");
            // The hairline is one device pixel wide wherever it falls, so it may straddle two:
            // their ink over the wash adds up to most of a whole pixel.
            let edge: u32 = (0..w / 2)
                .map(|x| u32::from(px(&bytes, w, x, h / 2)[3].saturating_sub(wash)))
                .sum();
            assert!(edge >= 180, "the hairline: {edge}");
            for x in 0..w {
                for y in 0..h {
                    let p = px(&bytes, w, x, y);
                    assert!(
                        p[..3].iter().all(|&c| c <= p[3]),
                        "premultiplied at {x},{y}"
                    );
                }
            }
        }
        let none = RingKey {
            cell: (0, 0),
            ..key(DARK, DARK_FG, RingHue::Neutral)
        };
        assert!(
            content(&none).items().is_empty(),
            "CONTROL: nothing to raster"
        );
    }

    #[test]
    fn the_rim_clears_the_small_emojis_rows_by_a_pixel() {
        for cell in [(15, 28), (8, 16), (7, 14), (9, 18), (18, 36)] {
            let k = RingKey {
                cell,
                ..key(DARK, DARK_FG, RingHue::Arc(CardPalette::Cruise))
            };
            let (w, h, bytes) = raster(&k);
            let (top, rows) = aterm_core::terminal::Script::small_emoji_rows(h as usize);
            let clear = top.saturating_sub(1)..(top + rows + 1).min(h as usize);
            let inked: Vec<usize> = (0..h)
                .filter(|&y| px(&bytes, w, w / 2, y)[3] > (RING_WASH * 255.0) as u8 + 8)
                .map(usize::from)
                .collect();
            assert!(
                inked.iter().all(|y| !clear.contains(y)),
                "{cell:?}: {inked:?}"
            );
            assert!(
                inked.iter().any(|&y| y < clear.start),
                "upper rim: {cell:?}"
            );
            assert!(inked.iter().any(|&y| y >= clear.end), "lower rim: {cell:?}");
        }
    }

    /// EVERY RING READS ON ITS GROUND: on dark, on paper and on mid greys inked white and black,
    /// under every palette, the accent and the harness off, the ring's ink reads at 3:1 on the
    /// ground. CONTROL: Dracula's accent unfloored reads under 3:1 on paper, floored at 3:1.
    #[test]
    fn every_ring_reads_on_its_ground() {
        let mut checked = 0;
        for (ground, fg) in [
            (DARK, DARK_FG),
            (PAPER, PAPER_FG),
            (0x0028_2A36, 0x00F8_F8F2),
            (0x003C_3C3C, 0x00FF_FFFF),
            (0x0078_7878, 0x00FF_FFFF),
            (0x0082_8282, 0),
        ] {
            for hue in [
                RingHue::Arc(CardPalette::Cruise),
                RingHue::Arc(CardPalette::Land),
                RingHue::Arc(CardPalette::Off),
                RingHue::Accent(0x003B_8EEA),
                RingHue::Neutral,
            ] {
                let r = contrast_ratio(ring_ink(&key(ground, fg, hue)), ground);
                assert!(r >= RING_CONTRAST, "{hue:?} {ground:06x}: ring {r:.2}:1");
                checked += 1;
            }
        }
        assert_eq!(checked, 6 * 5);
        let violet = 0x00BD_93F9;
        let raw = contrast_ratio(violet, PAPER);
        assert!(raw < RING_CONTRAST, "CONTROL: unfloored {raw:.2}:1");
        let k = key(PAPER, PAPER_FG, RingHue::Accent(violet));
        assert!(contrast_ratio(ring_ink(&k), PAPER) >= RING_CONTRAST);
    }
}

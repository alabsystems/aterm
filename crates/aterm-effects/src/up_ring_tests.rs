// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::color_math::contrast_ratio;
use crate::glass_inks::GlassInks;
use crate::harness_card::CardPalette;

const DARK: u32 = 0x0011_1318;
const DARK_FG: u32 = 0x00D0_D2D6;
const PAPER: u32 = 0x00F6_F7F9;
const PAPER_FG: u32 = 0x002B_2D33;

fn key(ground: u32, fg: u32, hue: RingHue) -> RingKey {
    RingKey {
        cell: (15, 28),
        ground,
        fg,
        hue,
    }
}

/// `palette`'s rule at the right end of a line on `ground` inked `fg`, where a ⬆️ sits.
fn arc(palette: CardPalette, ground: u32, fg: u32) -> RingHue {
    let rule = mix(ground, fg, 0.35);
    RingHue::Rule(GlassInks::of(Some(palette), ground, fg, rule).rule(0.02))
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
        let k = key(ground, fg, arc(CardPalette::Cruise, ground, fg));
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
            ..key(DARK, DARK_FG, arc(CardPalette::Cruise, DARK, DARK_FG))
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
/// under every palette's rule and with no hue, the ring's ink reads at 3:1 on the ground.
/// CONTROL: a pale violet rule unfloored reads under 3:1 on paper, floored at 3:1.
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
            arc(CardPalette::Cruise, ground, fg),
            arc(CardPalette::Land, ground, fg),
            arc(CardPalette::Off, ground, fg),
            RingHue::Neutral,
        ] {
            let r = contrast_ratio(ring_ink(&key(ground, fg, hue)), ground);
            assert!(r >= RING_CONTRAST, "{hue:?} {ground:06x}: ring {r:.2}:1");
            checked += 1;
        }
    }
    assert_eq!(checked, 6 * 4);
    let violet = 0x00BD_93F9;
    let raw = contrast_ratio(violet, PAPER);
    assert!(raw < RING_CONTRAST, "CONTROL: unfloored {raw:.2}:1");
    let k = key(PAPER, PAPER_FG, RingHue::Rule(violet));
    assert!(contrast_ratio(ring_ink(&k), PAPER) >= RING_CONTRAST);
    assert_eq!(
        ring_ink(&key(
            0xFF00_0000 | PAPER,
            0xFF00_0000 | PAPER_FG,
            RingHue::Rule(0xFF00_0000 | violet),
        )),
        ring_ink(&k),
    );
    assert_eq!(
        ring_ink(&key(
            0xFF00_0000 | PAPER,
            0xFF00_0000 | PAPER_FG,
            RingHue::Neutral,
        )),
        ring_ink(&key(PAPER, PAPER_FG, RingHue::Neutral)),
    );
}

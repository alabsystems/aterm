// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A rounded bar as fx items (`docs/DESIGN-card-engine-2026-10-19.md` §6):
//! the body one source-over rectangle, the two semicircular caps one Index8 art
//! per `(width, radius)` whose texels are the cap's antialiased coverage
//! (`SS × SS` samples a pixel) and whose palette turns each coverage into the
//! bar's colour at that share of its opacity, `(premul(rgb, a), a)` with
//! `a = (alpha·hits + SS²/2) / SS²`, so every pixel lands the bytes a
//! per-pixel-row coverage run would.

use std::sync::Arc;

use aterm_core::render::{FxArt, FxBlend, FxInk, FxItem, FxRect, FxSprite, FxTexels, PxRect};

/// The cap coverage's supersampling: `SS × SS` samples a pixel.
const SS: u32 = 4;

/// The palette entries one bar takes: the body's two (a flat rectangle's
/// ramp ends) and one a non-zero coverage count.
pub const BAR_INKS: usize = 2 + (SS * SS) as usize;

/// A bar's declared CPU-GPU parity: its layer is [`FxBlend::Exact`], the
/// same bytes on every arm.
pub const PARITY: u8 = FxBlend::Exact.parity();

/// A rounded bar `(x, y, w, h)` of `rgb` at `alpha`, in its layer's space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoundedBar {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    /// Straight colour.
    pub rgb: [u8; 3],
    /// Opacity of the body.
    pub alpha: u8,
}

/// The cap arts a host has built, by `(w, h')` ([`cap_key`]): a bar that
/// keeps its width keeps its art, so a moving or fading bar builds none.
#[derive(Debug, Default)]
pub struct CapArts {
    arts: Vec<((u32, u32), Arc<FxArt>)>,
}

/// The arts a host keeps at most; the oldest goes first.
const CAP_ARTS_KEPT: usize = 8;

impl CapArts {
    /// The cap art of a `w × h` bar, built on first use.
    fn get(&mut self, w: u32, h: u32) -> Option<Arc<FxArt>> {
        let key = cap_key(w, h);
        if let Some(at) = self.arts.iter().position(|(k, _)| *k == key) {
            let hit = self.arts.remove(at);
            let art = Arc::clone(&hit.1);
            self.arts.push(hit);
            return Some(art);
        }
        let art = cap_art(key.0, key.1)?;
        if self.arts.len() == CAP_ARTS_KEPT {
            self.arts.remove(0);
        }
        self.arts.push((key, Arc::clone(&art)));
        Some(art)
    }
}

/// The bar's corner radius and the rows each cap spans: `min(w, h) / 2` and
/// `min(⌈radius⌉, ⌊h / 2⌋)`.
fn radius_and_cap(w: u32, h: u32) -> (f64, u32) {
    let radius = (f64::from(w) / 2.0).min(f64::from(h) / 2.0);
    (radius, (radius.ceil() as u32).min(h / 2))
}

/// The height a `w × h` bar's caps are computed at: past `w + 2` rows the
/// caps no longer depend on the height (each is `⌈w / 2⌉` rows of the same
/// semicircle, a body row at least between them), so every taller bar of a
/// width shares one art.
#[must_use]
pub fn cap_key(w: u32, h: u32) -> (u32, u32) {
    (w, h.min(w.saturating_add(2)))
}

/// The `SS × SS` samples of pixel `(px, py)` inside the rounded rectangle
/// `(0, 0, w, h)`: the arithmetic is exact (sixteenths of a pixel), so the
/// count is the one a bar at any integer origin gets for the pixel at the
/// same offset.
fn hits(w: u32, h: u32, px: u32, py: u32) -> u8 {
    let (radius, _) = radius_and_cap(w, h);
    let (fw, fh) = (f64::from(w), f64::from(h));
    let inside = |x: f64, y: f64| -> bool {
        if x < 0.0 || x >= fw || y < 0.0 || y >= fh {
            return false;
        }
        let cx = x.clamp(radius, fw - radius);
        let cy = y.clamp(radius, fh - radius);
        let (dx, dy) = (x - cx, y - cy);
        dx * dx + dy * dy <= radius * radius
    };
    let mut n = 0u8;
    for sy in 0..SS {
        for sx in 0..SS {
            let x = f64::from(px) + (f64::from(sx) + 0.5) / f64::from(SS);
            let y = f64::from(py) + (f64::from(sy) + 0.5) / f64::from(SS);
            n += u8::from(inside(x, y));
        }
    }
    n
}

/// The cap art of a `w × h` bar: `w × 2·cap` Index8 texels, the top cap's
/// rows over the bottom cap's, each texel its pixel's sample count (0 draws
/// nothing). `None` for a bar with no cap rows.
fn cap_art(w: u32, h: u32) -> Option<Arc<FxArt>> {
    let (_, cap) = radius_and_cap(w, h);
    if cap == 0 || w == 0 {
        return None;
    }
    let rows = (0..cap).chain(h - cap..h);
    let data: Vec<u8> = rows
        .flat_map(|py| (0..w).map(move |px| hits(w, h, px, py)))
        .collect();
    crate::fx::bitmap(
        FxTexels::Index8,
        u16::try_from(w).ok()?,
        u16::try_from(2 * cap).ok()?,
        data,
    )
}

/// `(c·a + 127) / 255`, a channel premultiplied by `a`.
const fn premul(c: u8, a: u8) -> u32 {
    (c as u32 * a as u32 + 127) / 255
}

/// Source-over `rgb` at `a`, premultiplied.
const fn over(rgb: [u8; 3], a: u8) -> FxInk {
    FxInk::over(
        (premul(rgb[0], a) << 16) | (premul(rgb[1], a) << 8) | premul(rgb[2], a),
        a,
    )
}

/// Append `bar`'s items to `items` and its [`BAR_INKS`] inks to `palette`:
/// the body rectangle between the caps, and each cap a sprite of the bar's
/// cap art at 1:1, clipped to its own rows. A bar of no area or opacity adds
/// nothing.
pub fn push_items(
    bar: RoundedBar,
    arts: &mut CapArts,
    items: &mut Vec<FxItem>,
    palette: &mut Vec<FxInk>,
) {
    let RoundedBar {
        x,
        y,
        w,
        h,
        rgb,
        alpha,
    } = bar;
    if w == 0 || h == 0 || alpha == 0 {
        return;
    }
    let base = u32::try_from(palette.len()).unwrap_or(u32::MAX);
    let body = over(rgb, alpha);
    palette.extend([body, body]);
    palette.extend((1..=SS * SS).map(|k| {
        let a = ((u32::from(alpha) * k + SS * SS / 2) / (SS * SS)) as u8;
        over(rgb, a)
    }));
    let to16 = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    let to32 = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
    let (_, cap) = radius_and_cap(w, h);
    if h > 2 * cap {
        items.push(FxItem::Rect(FxRect {
            x: to32(x),
            y: to32(y + cap),
            w: to16(w),
            h: to16(h - 2 * cap),
            ink: base,
        }));
    }
    let Some(art) = arts.get(w, h) else {
        return;
    };
    let (x0, x1) = (to32(x), to32(x + w));
    for (top, rows) in [(y, (y, y + cap)), (y + h - 2 * cap, (y + h - cap, y + h))] {
        items.push(FxItem::Sprite(FxSprite {
            x: x0,
            y: to32(top),
            dw: to16(w),
            dh: to16(2 * cap),
            flip_x: false,
            art: Arc::clone(&art),
            ink: base + 2,
            clip: PxRect::new(x0, to32(rows.0), x1, to32(rows.1)),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tall bar's caps are its key's: the art a bar of any height past
    /// `w + 2` draws is the one computed at `w + 2`, top and bottom.
    #[test]
    fn a_tall_bars_caps_are_its_keys_caps() {
        for w in 1..=16u32 {
            let key = cap_key(w, 400).1;
            let (_, cap) = radius_and_cap(w, key);
            for h in [key, key + 1, key + 7, 3 * key + 5, 400] {
                assert_eq!(radius_and_cap(w, h).1, cap, "w{w} h{h}: the cap's rows");
                for py in 0..cap {
                    for px in 0..w {
                        assert_eq!(hits(w, h, px, py), hits(w, key, px, py), "top");
                        assert_eq!(
                            hits(w, h, px, h - cap + py),
                            hits(w, key, px, key - cap + py),
                            "w{w} h{h}: bottom"
                        );
                    }
                }
            }
        }
    }

    /// The art holds the coverage counts and nothing past a full pixel; the
    /// caps of a bar as wide as tall meet with no body between them.
    #[test]
    fn a_cap_art_is_its_coverage() {
        let art = cap_art(6, 60).expect("an art");
        assert_eq!((art.w(), art.h()), (6, 6));
        assert!(art.max_index() <= 16 && art.max_index() > 0);
        assert_eq!(art.texel(0, 0), 0, "a corner is shaved");
        assert_eq!(art.texel(2, 2), 16, "the cap's heart is solid");
        let mut items = Vec::new();
        let mut palette = Vec::new();
        let bar = RoundedBar {
            x: 0,
            y: 0,
            w: 6,
            h: 6,
            rgb: [9; 3],
            alpha: 200,
        };
        push_items(bar, &mut CapArts::default(), &mut items, &mut palette);
        assert_eq!(palette.len(), BAR_INKS);
        assert!(
            items.iter().all(|i| matches!(i, FxItem::Sprite(_))),
            "6 × 6: two caps, no body"
        );
    }
}

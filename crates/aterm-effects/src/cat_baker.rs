// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Full-colour RGBA tile baker and LRU cache of the cat and pet arts.

use std::sync::Arc;

use aterm_core::render::{FxArt, FxTexels};
use aterm_scene::vector::{FIXED_ONE, PathSeg};
use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_glyphs_gen::{CatGlyphId, GLYPHS, GlyphRole, Layer, Recolor};
use crate::color_math::{
    hsv2rgb, relative_luminance, relative_luminance_table, relative_luminance_with, rgb2hsv,
};
use crate::effect_util::lerp_rgb as mix_rgb;

/// Structural entry ceiling (§5.5): the LRU never holds more than 32 tiles.
pub const MAX_SLOTS: usize = 32;
/// Structural memory ceiling for the whole cache: 2 MiB at any cell size.
pub const MAX_ATLAS_BYTES: usize = 2 << 20;
/// Bake-rate cap (§5.5): at most 2 tiles baked per presented frame.
pub const MAX_BAKES_PER_FRAME: u32 = 2;

/// Extra texel width baked onto every tile for the catch-light dot.
pub const PATCH_STRIP: u16 = 4;

/// A resolved cache hit: the tile's art at its design size (straight RGBA,
/// drawn by a [`FreeSprite`](aterm_render::sprites::FreeSprite)).
#[derive(Clone, Debug)]
pub struct CatTile {
    /// The baked art: `key.w × key.h` for a generated cat, the host's size
    /// for a host tile.
    pub art: Arc<FxArt>,
}

struct Slot {
    key: SlotKey,
    art: Arc<FxArt>,
    last_used: u64,
}

/// A slot's identity: a generated cat's glyph key or the stable id of a
/// host-authored tile ([`CatBaker::host_tile`]); the two never alias.
#[derive(Clone, Copy, PartialEq)]
enum SlotKey {
    Cat(BakeKeyV4),
    Host(u64),
}

/// Host-side LRU bake cache of interned [`FxArt`]s (§5.5): what to keep baked, never where it
/// lives on a GPU (`aterm-gpu`'s fx atlas keeps its own residency, uploading an art's region only
/// when it is first drawn).
pub struct CatBaker {
    cell_w: u16,
    cell_h: u16,
    /// The widest tile a slot holds: `4·ch`, which makes the `W ≤ 4·ch` width cap structural.
    slot_w: u16,
    /// `min(32, ⌊2 MiB / slot_bytes⌋)` occupied-or-free slots.
    slots: Vec<Option<Slot>>,
    /// Monotonic; bumped on every bake and every wholesale clear (a rebake must
    /// repaint).
    version: u64,
    /// Reusable destination tile and open-eye reflection mask across cache-miss bakes.
    scratch: Option<Tile>,
    eye_mask_scratch: Option<Tile>,
    /// LRU clock, advanced once per tick by [`CatBaker::begin_frame`].
    clock: u64,
    bakes_left: u32,
}

impl Default for CatBaker {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl CatBaker {
    /// Construct an uninitialized cat-tile baker with no allocated slots or scratch buffers.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cell_w: 0,
            cell_h: 0,
            slot_w: 0,
            slots: Vec::new(),
            version: 0,
            scratch: None,
            eye_mask_scratch: None,
            clock: 0,
            bakes_left: 0,
        }
    }

    /// Structural slot count for a cell height.
    #[inline]
    #[must_use]
    pub const fn slot_count(cell_h: u16) -> usize {
        if cell_h == 0 {
            return 0;
        }
        let ch = cell_h as u64;
        let slot_bytes = (4 * ch) * (2 * ch) * 4;
        let n = (MAX_ATLAS_BYTES as u64) / slot_bytes;
        if n > MAX_SLOTS as u64 {
            MAX_SLOTS
        } else {
            n as usize
        }
    }

    /// Per-tick prologue: advance the LRU clock, reset the bake budget, and on a cell-metric
    /// change clear the cache and bump its version (§5.5 invalidation).
    #[inline]
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        self.clock = self.clock.wrapping_add(1);
        self.bakes_left = MAX_BAKES_PER_FRAME;
        if (cell_w, cell_h) != (self.cell_w, self.cell_h) {
            self.clear();
            self.cell_w = cell_w;
            self.cell_h = cell_h;
            if cell_h == 0 {
                self.slot_w = 0;
                self.slots.clear();
                return;
            }
            self.slot_w = cell_h.saturating_mul(4);
            let slots = Self::slot_count(cell_h);
            self.slots.resize_with(slots, || None);
        }
    }

    /// Wholesale clear and version bump (config reload, toggle, metric change).
    #[inline]
    pub fn clear(&mut self) {
        self.scratch = None;
        self.eye_mask_scratch = None;
        let mut cleared_any = false;
        let slots = self.slots.as_mut_slice();
        let mut i = 0;
        while i < slots.len() {
            cleared_any |= slots[i].take().is_some();
            i += 1;
        }
        if cleared_any {
            self.version = self.version.wrapping_add(1);
        }
    }

    /// Monotonic cache version (folded into the frame fingerprint — a rebake must repaint).
    #[inline]
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// The LRU / bake-budget frame clock, advanced once per [`begin_frame`](CatBaker::begin_frame).
    #[doc(hidden)]
    #[cfg(test)]
    pub const fn frame_clock(&self) -> u64 {
        self.clock
    }

    /// How many tiles are baked.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        let slots = self.slots.as_slice();
        let mut n = 0;
        let mut i = 0;
        while i < slots.len() {
            if slots[i].is_some() {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// Whether no tile is baked.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        let slots = self.slots.as_slice();
        let mut i = 0;
        while i < slots.len() {
            if slots[i].is_some() {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Whether a [`BakeKeyV4`] glyph tile fits inside the current slot dimensions.
    #[inline]
    const fn fits_v4(&self, key: &BakeKeyV4) -> bool {
        // The whole baked tile is `key.w + PATCH_STRIP` wide and must fit the slot.
        self.cell_h != 0
            && key.w != 0
            && (key.w as u32) + (PATCH_STRIP as u32) <= (self.slot_w as u32)
            && key.h != 0
            && (key.h as u32) <= 2 * (self.cell_h as u32)
    }

    /// Look up a [`BakeKeyV4`] glyph tile, baking on a miss when the per-frame budget allows.
    #[inline]
    pub fn get_v4(&mut self, key: &BakeKeyV4) -> Option<CatTile> {
        if !self.fits_v4(key) {
            return None;
        }
        let slot_key = SlotKey::Cat(*key);
        if let Some(tile) = self.touch(slot_key) {
            return Some(tile);
        }
        let i = self.bake_slot()?;
        let mut scratch = self.scratch.take();
        let mut eye_mask_scratch = self.eye_mask_scratch.take();
        let tile = key.bake_scratch(&mut scratch, &mut eye_mask_scratch);
        let tw = tile.width() as usize;
        let out = self.install(i, slot_key, tile.pixels(), tw, key.w, key.h);
        self.scratch = scratch;
        self.eye_mask_scratch = eye_mask_scratch;
        out
    }

    /// Look up a resident `BakeKeyV4` tile and stamp it used this frame, spending no bake budget
    /// on a miss.
    #[inline]
    pub fn peek_v4(&mut self, key: &BakeKeyV4) -> Option<CatTile> {
        if !self.fits_v4(key) {
            return None;
        }
        self.touch(SlotKey::Cat(*key))
    }

    /// The cache hit: find `key`'s slot and stamp it used this frame (the LRU
    /// touch), returning its art.
    #[inline]
    fn touch(&mut self, key: SlotKey) -> Option<CatTile> {
        let clock = self.clock;
        let slots = self.slots.as_mut_slice();
        let mut i = 0;
        while i < slots.len() {
            if let Some(s) = &mut slots[i]
                && s.key == key
            {
                s.last_used = clock;
                return Some(CatTile {
                    art: Arc::clone(&s.art),
                });
            }
            i += 1;
        }
        None
    }

    /// Spend one bake from this frame's budget and pick the slot to bake into: a
    /// free slot first, else the least-recently-used (lowest index on ties).
    #[inline]
    const fn bake_slot(&mut self) -> Option<usize> {
        if self.bakes_left == 0 {
            return None;
        }
        self.bakes_left -= 1;
        let slots = self.slots.as_slice();
        let mut victim: Option<(usize, u64)> = None;
        let mut idx = 0;
        while idx < slots.len() {
            let Some(s) = slots[idx].as_ref() else {
                return Some(idx);
            };
            if s.last_used != self.clock {
                match victim {
                    Some((_, best_used)) if s.last_used >= best_used => {}
                    _ => victim = Some((idx, s.last_used)),
                }
            }
            idx += 1;
        }
        match victim {
            Some((idx, _)) => Some(idx),
            None => None,
        }
    }

    /// Intern the `w × h` top-left of the `tw`-wide straight RGBA `src` as
    /// slot `i`'s art.
    #[inline]
    fn install(
        &mut self,
        i: usize,
        key: SlotKey,
        src: &[u8],
        tw: usize,
        w: u16,
        h: u16,
    ) -> Option<CatTile> {
        let (wu, hu) = (usize::from(w), usize::from(h));
        let data = if tw == wu {
            src[..wu * hu * 4].to_vec()
        } else {
            let mut data = Vec::with_capacity(wu * hu * 4);
            let stride = tw * 4;
            let row_bytes = wu * 4;
            let mut r = 0;
            while r < hu && (r + 1) * stride <= src.len() {
                let off = r * stride;
                data.extend_from_slice(&src[off..off + row_bytes]);
                r += 1;
            }
            data
        };
        let art = aterm_render::fx::bitmap(FxTexels::Rgba8Straight, w, h, data)?;
        self.slots[i] = Some(Slot {
            key,
            art: Arc::clone(&art),
            last_used: self.clock,
        });
        self.version = self.version.wrapping_add(1);
        Some(CatTile { art })
    }

    /// Bake a host-authored straight-alpha RGBA tile into the cache, keyed by a stable `host_id`
    /// so later calls are cache hits.
    #[inline]
    pub fn host_tile(&mut self, host_id: u64, w: u16, h: u16, rgba: &[u8]) -> Option<CatTile> {
        if self.cell_h == 0 || w == 0 || h == 0 {
            return None;
        }
        let slot_h = 2 * usize::from(self.cell_h);
        if usize::from(w) > usize::from(self.slot_w)
            || usize::from(h) > slot_h
            || rgba.len() < usize::from(w) * usize::from(h) * 4
        {
            return None;
        }
        let slot_key = SlotKey::Host(host_id);
        if let Some(tile) = self.touch(slot_key) {
            return Some(tile);
        }
        let i = self.bake_slot()?;
        self.install(i, slot_key, rgba, usize::from(w), w, h)
    }

    /// Peek [`CatBaker::host_tile`]'s cache without supplying texels.
    #[inline]
    pub fn host_peek(&mut self, host_id: u64) -> Option<CatTile> {
        // Mirrors `host_tile`'s uninitialised refusal; `begin_frame` clears the
        // slots on any metric change, so nothing could match anyway.
        if self.cell_h == 0 {
            return None;
        }
        self.touch(SlotKey::Host(host_id))
    }

    /// Whether this frame still has cold-bake budget left for an initialised cache.
    #[inline]
    #[must_use]
    pub const fn can_bake(&self) -> bool {
        self.cell_h > 0 && self.bakes_left > 0
    }
}

/// Sixteen similarity-ordered coat stops, black → gray → blue → brown → ginger → cream → white;
/// adjacent indices are near-identical coats.
pub const COAT_RAMP: [u32; 16] = [
    0x0010_1014,
    0x001B_1B22,
    0x002E_2E38,
    0x004A_4A56,
    0x006E_6E78,
    0x008B_8B93,
    0x007A_6A5C,
    0x008D_7458,
    0x00A0_8064,
    0x00B2_8A5F,
    0x00C2_925A,
    0x00D9_9A4E,
    0x00E8_A54B,
    0x00EE_C27F,
    0x00F4_DCB0,
    0x00FA_F4E8,
];

/// Sixteen low-chroma natural iris stops (hazel, moss, storm blue, rose grey), visible without
/// dominating the expression.
pub const EYE_RAMP: [u32; 16] = [
    0x007A_5146,
    0x0087_5D50,
    0x008B_6C55,
    0x0093_7954,
    0x0081_8058,
    0x006F_7B5F,
    0x0063_796A,
    0x005D_7B78,
    0x005E_7388,
    0x0064_7B96,
    0x0071_899F,
    0x0085_9EAC,
    0x007D_7894,
    0x008F_7789,
    0x0095_837B,
    0x00A2_9181,
];

const CATCH_LIGHT: u32 = 0x00FF_FFFF;
const CATCH_LIGHT_RGB: (f32, f32, f32) = rgb(CATCH_LIGHT);

const OPEN_EYE_REFLECTION_MIN_PX: f32 = 3.0;
const OPEN_EYE_SHALLOW_ASPECT: f32 = 0.6;
const OPEN_EYE_PRIMARY_FRAC: f32 = 0.17;
const OPEN_EYE_PRIMARY_MIN_RADIUS_PX: f32 = 0.62;
const OPEN_EYE_PRIMARY_MAX_RADIUS_PX: f32 = 1.08;
const OPEN_EYE_ECHO_MIN_PX: f32 = 4.5;
const OPEN_EYE_ECHO_FRAC: f32 = 0.075;
const OPEN_EYE_ECHO_MIN_RADIUS_PX: f32 = 0.55;
const OPEN_EYE_ECHO_MAX_RADIUS_PX: f32 = 0.72;

const BYTE_TO_F32: [f32; 256] = {
    let mut table = [0.0f32; 256];
    let mut i = 0usize;
    while i < 256 {
        table[i] = i as f32 / 255.0;
        i += 1;
    }
    table
};

/// Unpack an authored `0x00RR_GGBB` fill into `0..=1` float channels.
#[inline]
pub(crate) const fn rgb(hex: u32) -> (f32, f32, f32) {
    (
        BYTE_TO_F32[((hex >> 16) & 0xff) as usize],
        BYTE_TO_F32[((hex >> 8) & 0xff) as usize],
        BYTE_TO_F32[(hex & 0xff) as usize],
    )
}

/// Paint a supersampled reflection disc only over opaque dark eye texels covered by `eye_mask`.
#[inline]
pub(crate) fn paint_dark_masked_disc(
    tile: &mut Tile,
    eye_mask: &Tile,
    cx: f32,
    cy: f32,
    r: f32,
    col: (f32, f32, f32),
    alpha: f32,
) {
    if r <= 0.0 || alpha <= 0.0 {
        return;
    }
    debug_assert_eq!(tile.width(), eye_mask.width());
    debug_assert_eq!(tile.height(), eye_mask.height());
    let cx = cx.floor() + 0.5;
    let cy = cy.floor() + 0.5;
    let rr = r * r;
    let x0 = ((cx - r) as i32).saturating_sub(1).max(0);
    let y0 = ((cy - r) as i32).saturating_sub(1).max(0);
    let x1 = ((cx + r) as i32).saturating_add(2).min(tile.width() as i32);
    let y1 = ((cy + r) as i32)
        .saturating_add(2)
        .min(tile.height() as i32);
    let tile_w = tile.width();
    let mask_px = eye_mask.pixels();
    let alpha_16 = alpha * 0.0625;
    let mut y = y0;
    while y < y1 {
        let cur_y = y;
        y += 1;
        let dy0 = cur_y as f32 + 0.125 - cy;
        let dy3 = dy0 + 0.75;
        let dy0_2 = dy0 * dy0;
        let dy3_2 = dy3 * dy3;
        let dy2_min = if dy0 > 0.0 {
            dy0_2
        } else if dy3 < 0.0 {
            dy3_2
        } else {
            0.015_625
        };
        if dy2_min > rr {
            continue;
        }
        let dy1 = dy0 + 0.25;
        let dy2_v = dy0 + 0.50;
        let rem = [rr - dy0_2, rr - dy1 * dy1, rr - dy2_v * dy2_v, rr - dy3_2];
        let dy2_max = dy0_2.max(dy3_2);
        let row_base4 = (cur_y as u32 * tile_w) as usize * 4;
        let mut idx4 = row_base4 + (x0 as usize) * 4;
        let mut x = x0;
        while x < x1 {
            let cur_x = x;
            let i = idx4;
            x += 1;
            idx4 += 4;
            if mask_px[i + 3] == 0 {
                continue;
            }
            let px = tile.pixels();
            if px[i + 3] != 255 || px[i] >= 96 || px[i + 1] >= 96 || px[i + 2] >= 96 {
                continue;
            }
            let dx0 = cur_x as f32 + 0.125 - cx;
            let dx3 = dx0 + 0.75;
            let dx0_2 = dx0 * dx0;
            let dx3_2 = dx3 * dx3;
            let dx2_min = if dx0 > 0.0 {
                dx0_2
            } else if dx3 < 0.0 {
                dx3_2
            } else {
                0.015_625
            };
            if dx2_min + dy2_min > rr {
                continue;
            }
            if dx0_2.max(dx3_2) + dy2_max <= rr {
                tile.over(cur_x, cur_y, col, alpha);
                continue;
            }
            let dx1 = dx0 + 0.25;
            let dx2_v = dx0 + 0.50;
            let dx2 = [dx0_2, dx1 * dx1, dx2_v * dx2_v, dx3_2];
            let mut hit = 0u32;
            let mut ky = 0;
            while ky < 4 {
                let rem_k = rem[ky];
                if rem_k >= 0.0 {
                    hit += u32::from(dx2[0] <= rem_k)
                        + u32::from(dx2[1] <= rem_k)
                        + u32::from(dx2[2] <= rem_k)
                        + u32::from(dx2[3] <= rem_k);
                }
                ky += 1;
            }
            if hit > 0 {
                tile.over(cur_x, cur_y, col, hit as f32 * alpha_16);
            }
        }
    }
}

/// Pale outline ink for the darkest background-luminance band (v4 §2.1).
pub const OUTLINE_PALE: u32 = 0x00F4_E6DD;

const EYE_INK_DARK: u32 = 0x0018_1520;
const EYE_INK_PALE: u32 = 0x00FF_F2E8;
const MIN_EYE_CONTRAST: f32 = 3.0;

/// Quantized local terminal palette carried in every cat bake key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CatColorKey {
    /// `0..=11` = 30-degree hue family, `12` = neutral foreground.
    pub accent: u8,
    /// Background WCAG-luminance band, darkest `0` to lightest `3`; `4` = mixed (spans the
    /// crossover).
    pub background: u8,
}

const BG_BAND_DARKEST_MAX: f32 = 0.04;
const BG_BAND_DARK_MAX: f32 = 0.18;
const BG_BAND_MID_MAX: f32 = 0.58;

impl Default for CatColorKey {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl CatColorKey {
    /// Default dark neutral palette key.
    pub const DEFAULT: Self = Self {
        accent: 12,
        background: 0,
    };

    /// Resolve matched-word, neighboring-text, and local-background colors on the cold rescan path.
    #[inline]
    #[must_use]
    pub const fn from_rgb(background: u32, foreground: u32, surrounding: u32) -> Self {
        Self::from_rgb_with(
            relative_luminance_table(),
            background,
            foreground,
            surrounding,
        )
    }

    #[inline]
    #[must_use]
    pub(crate) const fn from_rgb_with(
        table: &crate::color_math::RelativeLuminanceTable,
        background: u32,
        foreground: u32,
        surrounding: u32,
    ) -> Self {
        const INV_30: f32 = 1.0 / 30.0;
        let harmonized = mix_rgb(foreground, surrounding, 0.35);
        let (h, s, _) = rgb2hsv(harmonized);
        let accent = if s < 0.12 {
            12
        } else {
            (((h + 15.0) * INV_30).floor() as u8) % 12
        };
        let background = background_band_with(table, background);
        Self { accent, background }
    }

    /// Like [`Self::from_rgb`], plus one bit of footprint structure that RGB averaging loses.
    #[inline]
    #[must_use]
    pub const fn from_rgb_span(
        background: u32,
        foreground: u32,
        surrounding: u32,
        min_background_band: u8,
        max_background_band: u8,
    ) -> Self {
        Self::from_rgb_span_with(
            relative_luminance_table(),
            background,
            foreground,
            surrounding,
            min_background_band,
            max_background_band,
        )
    }

    #[inline]
    #[must_use]
    pub(crate) const fn from_rgb_span_with(
        table: &crate::color_math::RelativeLuminanceTable,
        background: u32,
        foreground: u32,
        surrounding: u32,
        min_background_band: u8,
        max_background_band: u8,
    ) -> Self {
        let mut key = Self::from_rgb_with(table, background, foreground, surrounding);
        let min_b = if min_background_band < 3 {
            min_background_band
        } else {
            3
        };
        let max_b = if max_background_band < 3 {
            max_background_band
        } else {
            3
        };
        if (key.background >= 1 || max_b >= 3) && min_b <= 1 && max_b >= 2 {
            key.background = 4;
        }
        key
    }

    #[inline]
    #[must_use]
    pub const fn background_band(background: u32) -> u8 {
        background_band(background)
    }

    #[inline]
    #[must_use]
    pub(crate) const fn background_band_with(
        table: &crate::color_math::RelativeLuminanceTable,
        background: u32,
    ) -> u8 {
        background_band_with(table, background)
    }

    #[inline]
    #[must_use]
    pub(crate) const fn background_band_rgb(
        table: &crate::color_math::RelativeLuminanceTable,
        rgb: [u8; 3],
    ) -> u8 {
        background_band_from_lum(crate::color_math::relative_luminance_rgb(
            table,
            rgb[0] as usize,
            rgb[1] as usize,
            rgb[2] as usize,
        ))
    }

    #[inline]
    #[must_use]
    pub const fn dark(self) -> bool {
        self.background <= 1
    }
}

#[inline]
const fn background_band(background: u32) -> u8 {
    background_band_from_lum(relative_luminance(background))
}

#[inline]
const fn background_band_with(
    table: &crate::color_math::RelativeLuminanceTable,
    background: u32,
) -> u8 {
    background_band_from_lum(relative_luminance_with(table, background))
}

#[inline]
const fn background_band_from_lum(l: f32) -> u8 {
    if l < BG_BAND_DARKEST_MAX {
        0
    } else if l < BG_BAND_DARK_MAX {
        1
    } else if l < BG_BAND_MID_MAX {
        2
    } else {
        3
    }
}

const CONTEXT_ACCENTS: [u32; 13] = [
    0x00E8_6F6F,
    0x00E8_AC6F,
    0x00E8_E86F,
    0x00AC_E86F,
    0x006F_E86F,
    0x006F_E8AC,
    0x006F_E8E8,
    0x006F_ACE8,
    0x006F_6FE8,
    0x00AC_6FE8,
    0x00E8_6FE8,
    0x00E8_6FAC,
    0x0096_98A6,
];

/// Resolved genome fills for one bake.
#[derive(Clone, Copy, Debug)]
pub struct ResolvedFills {
    /// Coat colourway (a [`COAT_RAMP`] stop) applied to `Recolor::Coat` layers.
    pub coat: (f32, f32, f32),
    /// Iris stop (an [`EYE_RAMP`] colour) applied to `Recolor::Iris` layers.
    pub iris: (f32, f32, f32),
    /// Context-tinted ink used only when authored eyes disappear into the coat.
    pub eye_ink: (f32, f32, f32),
    /// Contrast-bearing, context-tinted silhouette/feature ink.
    pub outline: (f32, f32, f32),
    /// Pale outer keyline painted beneath `outline` for a mixed-background footprint.
    pub outline_underlay: Option<(f32, f32, f32)>,
    coat_lum: f32,
}

#[derive(Clone, Copy)]
struct CoatEyePair {
    coat: (f32, f32, f32),
    eye_ink: (f32, f32, f32),
    coat_lum: f32,
}

const OUTLINE_BASES: [u32; 5] = [
    OUTLINE_PALE,
    0x00FF_F2E8,
    0x0018_1520,
    0x0010_0D16,
    EYE_INK_DARK,
];

struct FillsLut {
    iris: [[(f32, f32, f32); CONTEXT_ACCENTS.len()]; EYE_RAMP.len()],
    outline: [[(f32, f32, f32); CONTEXT_ACCENTS.len()]; OUTLINE_BASES.len()],
    underlay: [(f32, f32, f32); CONTEXT_ACCENTS.len()],
    coat_eye: [[[CoatEyePair; 2]; CONTEXT_ACCENTS.len()]; COAT_RAMP.len()],
}

impl FillsLut {
    #[inline]
    const fn build() -> Self {
        let zero = (0.0f32, 0.0f32, 0.0f32);
        let mut iris = [[zero; CONTEXT_ACCENTS.len()]; EYE_RAMP.len()];
        let mut iris_idx = 0;
        while iris_idx < EYE_RAMP.len() {
            let iris_stop = EYE_RAMP[iris_idx];
            let mut accent_idx = 0;
            while accent_idx < CONTEXT_ACCENTS.len() {
                iris[iris_idx][accent_idx] =
                    rgb(mix_rgb(iris_stop, CONTEXT_ACCENTS[accent_idx], 0.28));
                accent_idx += 1;
            }
            iris_idx += 1;
        }

        let mut outline = [[zero; CONTEXT_ACCENTS.len()]; OUTLINE_BASES.len()];
        let mut bg_idx = 0;
        while bg_idx < OUTLINE_BASES.len() {
            let outline_base = OUTLINE_BASES[bg_idx];
            let mut accent_idx = 0;
            while accent_idx < CONTEXT_ACCENTS.len() {
                outline[bg_idx][accent_idx] =
                    rgb(mix_rgb(outline_base, CONTEXT_ACCENTS[accent_idx], 0.10));
                accent_idx += 1;
            }
            bg_idx += 1;
        }

        let mut underlay = [zero; CONTEXT_ACCENTS.len()];
        let mut accent_idx = 0;
        while accent_idx < CONTEXT_ACCENTS.len() {
            underlay[accent_idx] = rgb(mix_rgb(EYE_INK_PALE, CONTEXT_ACCENTS[accent_idx], 0.10));
            accent_idx += 1;
        }

        let zero_pair = CoatEyePair {
            coat: zero,
            eye_ink: zero,
            coat_lum: 0.0,
        };
        let mut coat_eye = [[[zero_pair; 2]; CONTEXT_ACCENTS.len()]; COAT_RAMP.len()];
        let mut coat_idx = 0;
        while coat_idx < COAT_RAMP.len() {
            let coat_stop = COAT_RAMP[coat_idx];
            let mut accent_idx = 0;
            while accent_idx < CONTEXT_ACCENTS.len() {
                let accent = CONTEXT_ACCENTS[accent_idx];
                let base_coat = rgb(mix_rgb(coat_stop, accent, 0.12));
                let dark_eye = rgb(mix_rgb(EYE_INK_DARK, accent, 0.06));
                let pale_eye = rgb(mix_rgb(EYE_INK_PALE, accent, 0.06));
                let mut lift = 0;
                while lift < 2 {
                    let coat = if lift != 0 {
                        lift_coat_luminance(base_coat, COAT_MIN_LUM_DARK_BG)
                    } else {
                        base_coat
                    };
                    let eye_ink = if contrast_ratio(pale_eye, coat) > contrast_ratio(dark_eye, coat)
                    {
                        pale_eye
                    } else {
                        dark_eye
                    };
                    let coat_lum = relative_luminance(packed_rgb(coat));
                    coat_eye[coat_idx][accent_idx][lift] = CoatEyePair {
                        coat,
                        eye_ink,
                        coat_lum,
                    };
                    lift += 1;
                }
                accent_idx += 1;
            }
            coat_idx += 1;
        }

        Self {
            iris,
            outline,
            underlay,
            coat_eye,
        }
    }
}

static FILLS_LUT: FillsLut = FillsLut::build();

#[inline]
const fn fills_lut() -> &'static FillsLut {
    &FILLS_LUT
}

impl ResolvedFills {
    /// Resolve genome ramp indices into concrete colours (the cache key → fills step).
    #[inline]
    #[must_use]
    pub const fn from_indices(coat: u8, iris: u8, dark_bg: bool) -> Self {
        let colors = CatColorKey {
            accent: 12,
            background: if dark_bg { 0 } else { 3 },
        };
        Self::from_context(coat, iris, colors)
    }

    /// Resolve genome fills through the local palette.
    #[inline]
    #[must_use]
    pub const fn from_context(coat: u8, iris: u8, colors: CatColorKey) -> Self {
        let accent_idx = if colors.accent < 12 {
            colors.accent as usize
        } else {
            12
        };
        let coat_idx = if (coat as usize) < COAT_RAMP.len() - 1 {
            coat as usize
        } else {
            COAT_RAMP.len() - 1
        };
        let iris_idx = if (iris as usize) < EYE_RAMP.len() - 1 {
            iris as usize
        } else {
            EYE_RAMP.len() - 1
        };
        let outline_idx = match colors.background {
            1 => 1,
            2 => 2,
            3 => 3,
            4 => 4,
            _ => 0,
        };
        let lift = matches!(colors.background, 0 | 4) as usize;
        let lut = fills_lut();
        let CoatEyePair {
            coat,
            eye_ink,
            coat_lum,
        } = lut.coat_eye[coat_idx][accent_idx][lift];
        Self {
            coat,
            iris: lut.iris[iris_idx][accent_idx],
            eye_ink,
            outline: lut.outline[outline_idx][accent_idx],
            outline_underlay: if colors.background == 4 {
                Some(lut.underlay[accent_idx])
            } else {
                None
            },
            coat_lum,
        }
    }
}

#[inline]
const fn packed_rgb((r, g, b): (f32, f32, f32)) -> u32 {
    #[inline]
    const fn channel(value: f32) -> u32 {
        (value.clamp(0.0, 1.0) * 255.0).round() as u32
    }
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

const COAT_MIN_LUM_DARK_BG: f32 = 0.10;

/// Component-wise linear mix of two float colours, `t` from a → b.
#[inline]
const fn mix3(a: (f32, f32, f32), b: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
    (
        a.0 + (b.0 - a.0) * t,
        a.1 + (b.1 - a.1) * t,
        a.2 + (b.2 - a.2) * t,
    )
}

#[inline]
const fn lift_coat_luminance(coat: (f32, f32, f32), floor: f32) -> (f32, f32, f32) {
    let table = relative_luminance_table();
    let mut lo_rgb = packed_rgb(coat);
    if relative_luminance_with(table, lo_rgb) >= floor {
        return coat;
    }
    let (h, s, _v) = rgb2hsv(lo_rgb);
    let pale = rgb(hsv2rgb(h, s * 0.55, 0.78));
    let mut hi_rgb = packed_rgb(pale);
    if relative_luminance_with(table, hi_rgb) <= floor {
        return pale;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let mut step = 0;
    while step < 6 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = packed_rgb(mix3(coat, pale, mid));
        if mid_rgb == lo_rgb
            || (mid_rgb != hi_rgb && relative_luminance_with(table, mid_rgb) < floor)
        {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        step += 1;
    }
    mix3(coat, pale, hi)
}

/// `COAT_RAMP[coat]` lifted to [`COAT_MIN_LUM_DARK_BG`] as on the dark background classes, without
/// the accent nudge (which cancels out of a two-coat comparison).
#[cfg(test)]
pub(crate) const fn dark_bg_coat_stop(coat: u8) -> (f32, f32, f32) {
    let idx = coat as usize;
    let stop_idx = if idx < COAT_RAMP.len() - 1 {
        idx
    } else {
        COAT_RAMP.len() - 1
    };
    let stop = rgb(COAT_RAMP[stop_idx]);
    lift_coat_luminance(stop, COAT_MIN_LUM_DARK_BG)
}

#[inline]
const fn contrast_ratio(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let table = relative_luminance_table();
    let a = relative_luminance_with(table, packed_rgb(a));
    let b = relative_luminance_with(table, packed_rgb(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Fill resolution for one glyph layer to its baked `(rgb, alpha)`.
#[inline]
#[must_use]
pub const fn resolve_layer(layer: &Layer, fills: &ResolvedFills) -> ((f32, f32, f32), f32) {
    // Whiskers reach onto the terminal background, so they need the outline's
    // contrast guarantee. Internal face ink stays fixed so expressions survive
    // on pale coats.
    if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker) {
        return (fills.outline, 1.0);
    }
    let col = match layer.recolor {
        Recolor::Coat => fills.coat,
        Recolor::Iris => fills.iris,
        Recolor::Fixed => rgb(layer.fill),
    };
    (col, 1.0)
}

#[inline]
const fn resolve_glyph_layer(
    layer: &Layer,
    fills: &ResolvedFills,
    use_eye_ink: bool,
) -> ((f32, f32, f32), f32) {
    if use_eye_ink
        && matches!(layer.role, GlyphRole::Eye)
        && matches!(layer.recolor, Recolor::Fixed)
    {
        return (fills.eye_ink, 1.0);
    }
    resolve_layer(layer, fills)
}

/// The mixed-background keyline under an outline layer.
#[inline]
pub(crate) fn paint_outline_halo(
    tile: &mut Tile,
    layer: &Layer,
    fills: &ResolvedFills,
    alpha: f32,
    xform: PathTransform,
) {
    let Some(underlay) = fills.outline_underlay else {
        return;
    };
    let halo = (xform.scale_x.min(xform.scale_y) * 0.04).clamp(0.75, 1.25);
    let offsets = [(halo, 0.0), (-halo, 0.0), (0.0, halo), (0.0, -halo)];
    let mut k = 0;
    while k < 4 {
        let (dx, dy) = offsets[k];
        fill_path_fixed(
            tile,
            layer.paths,
            underlay,
            alpha,
            PathTransform {
                dx: xform.dx + dx,
                dy: xform.dy + dy,
                ..xform
            },
        );
        k += 1;
    }
}

/// Accessory attachment: the normalized head-frame point where the accessory's centre lands, and
/// its size relative to the head box.
#[inline]
#[must_use]
pub const fn accessory_attach(acc: CatGlyphId) -> (f32, f32, f32) {
    match acc {
        CatGlyphId::AccBow => (0.30, 0.22, 0.40),
        CatGlyphId::AccCrown => (0.50, 0.10, 0.46),
        CatGlyphId::AccBell => (0.50, 0.87, 0.26),
        _ => (0.50, 0.50, 0.40),
    }
}

/// Fit an accessory into its attachment box, keeping the authored viewbox
/// aspect: a wide bow gets shorter, a tall bell narrower.
#[inline]
const fn accessory_transform(acc: CatGlyphId, w: u32, h: u32) -> PathTransform {
    const GLYPH_ASPECTS: [f32; GLYPHS.len()] = {
        const INV_1000: f32 = 1.0 / 1000.0;
        let mut table = [0.0f32; GLYPHS.len()];
        let mut i = 0;
        while i < GLYPHS.len() {
            table[i] = (GLYPHS[i].aspect_x1000 as f32 * INV_1000).max(0.001);
            i += 1;
        }
        table
    };
    let (cx, cy, s) = accessory_attach(acc);
    let max_w = s * w as f32;
    let max_h = s * h as f32;
    let aspect = GLYPH_ASPECTS[acc as usize];
    let (bw, bh) = if max_w / max_h > aspect {
        (max_h * aspect, max_h)
    } else {
        (max_w, max_w / aspect)
    };
    PathTransform {
        scale_x: bw,
        scale_y: bh,
        dx: cx * w as f32 - 0.5 * bw,
        dy: cy * h as f32 - 0.5 * bh,
    }
}

/// The eye state baked into a cursor-cat tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EyesFrame {
    /// Eyes as authored (round button eyes / open arcs).
    #[default]
    Open,
    /// A content half-lidded squint (sustained-momentum "happy" tell).
    Happy,
    /// Eyes shut for a blink (the idle "alive" tell).
    Blink,
}

impl EyesFrame {
    /// Vertical openness of the eye layers: `1.0` as authored, small is shut.
    #[inline]
    const fn openness(self) -> f32 {
        match self {
            EyesFrame::Open => 1.0,
            EyesFrame::Happy => 0.5,
            EyesFrame::Blink => 0.14,
        }
    }
}

#[inline]
const fn layer_eye_line(layer: &Layer) -> Option<f32> {
    let mut lo = u16::MAX;
    let mut hi = 0u16;
    let mut seen = false;
    let mut p = 0;
    while p < layer.paths.len() {
        let path = layer.paths[p];
        let mut s = 0;
        while s < path.len() {
            match path[s] {
                PathSeg::Move(_, y) | PathSeg::Line(_, y) => {
                    if y < lo {
                        lo = y;
                    }
                    if y > hi {
                        hi = y;
                    }
                    seen = true;
                }
                PathSeg::Cubic(_, y1, _, y2, _, y) => {
                    let ys = [y1, y2, y];
                    let mut k = 0;
                    while k < 3 {
                        let yk = ys[k];
                        if yk < lo {
                            lo = yk;
                        }
                        if yk > hi {
                            hi = yk;
                        }
                        k += 1;
                    }
                    seen = true;
                }
                PathSeg::Close => {}
            }
            s += 1;
        }
        p += 1;
    }
    if seen {
        Some((lo as f32 + hi as f32) * (0.5 / FIXED_ONE as f32))
    } else {
        None
    }
}

/// Normalized control-hull bounds `(x0, y0, x1, y1)` of one subpath.
#[inline]
#[must_use]
pub(crate) const fn path_bounds(path: &[PathSeg]) -> Option<(f32, f32, f32, f32)> {
    const INV_FIXED_ONE: f32 = 1.0 / FIXED_ONE as f32;
    let (mut x0, mut y0, mut x1, mut y1) = (u16::MAX, u16::MAX, 0u16, 0u16);
    let mut seen = false;
    let mut i = 0;
    while i < path.len() {
        match path[i] {
            PathSeg::Move(x, y) | PathSeg::Line(x, y) => {
                if x < x0 {
                    x0 = x;
                }
                if y < y0 {
                    y0 = y;
                }
                if x > x1 {
                    x1 = x;
                }
                if y > y1 {
                    y1 = y;
                }
                seen = true;
            }
            PathSeg::Cubic(ax, ay, bx, by, x, y) => {
                let pts = [(ax, ay), (bx, by), (x, y)];
                let mut k = 0;
                while k < 3 {
                    let (px, py) = pts[k];
                    if px < x0 {
                        x0 = px;
                    }
                    if py < y0 {
                        y0 = py;
                    }
                    if px > x1 {
                        x1 = px;
                    }
                    if py > y1 {
                        y1 = py;
                    }
                    k += 1;
                }
                seen = true;
            }
            PathSeg::Close => {}
        }
        i += 1;
    }
    if seen {
        Some((
            x0 as f32 * INV_FIXED_ONE,
            y0 as f32 * INV_FIXED_ONE,
            x1 as f32 * INV_FIXED_ONE,
            y1 as f32 * INV_FIXED_ONE,
        ))
    } else {
        None
    }
}

#[inline]
const fn closest_eye_bounds(
    eye_bounds: &[Option<(f32, f32, f32, f32)>],
    pcx: f32,
    pcy: f32,
) -> Option<(usize, (f32, f32, f32, f32))> {
    let mut best: Option<(usize, (f32, f32, f32, f32))> = None;
    let mut best_dist = f32::INFINITY;
    let mut index = 0;
    while index < eye_bounds.len() {
        if let Some((x0, y0, x1, y1)) = eye_bounds[index] {
            let (x, y) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
            let (dx, dy) = (x - pcx, y - pcy);
            let dist = dx * dx + dy * dy;
            if best.is_none() || dist < best_dist {
                best = Some((index, (x0, y0, x1, y1)));
                best_dist = dist;
            }
        }
        index += 1;
    }
    best
}

#[cfg(test)]
const fn closest_eye_path(
    eyes: &Layer,
    pcx: f32,
    pcy: f32,
) -> Option<(usize, (f32, f32, f32, f32))> {
    let mut inline_bounds = [None; MAX_GLYPH_EYE_PATHS];
    let mut i = 0;
    while i < eyes.paths.len() {
        inline_bounds[i] = path_bounds(eyes.paths[i]);
        i += 1;
    }
    closest_eye_bounds(inline_bounds.split_at(eyes.paths.len()).0, pcx, pcy)
}

const MAX_GLYPH_EYE_PATHS: usize = 8;

#[derive(Clone, Copy)]
struct OpenEyeReflection {
    pcx: f32,
    pcy: f32,
    echo_x: f32,
    echo_y: f32,
    eye_w_norm: f32,
    eye_h_norm: f32,
}

#[derive(Clone, Copy)]
struct GlyphMeta {
    fixed_eye_lum: Option<f32>,
    eye_line: Option<f32>,
    eye_paths: &'static [&'static [PathSeg]],
    reflections: [Option<OpenEyeReflection>; MAX_GLYPH_EYE_PATHS],
    reflection_count: u8,
}

impl GlyphMeta {
    #[inline]
    const fn from_def(def: &crate::cat_glyphs_gen::GlyphDef) -> Self {
        let mut has_coat = false;
        let mut fixed_eye_fill: Option<u32> = None;
        let mut eye_layer: Option<&Layer> = None;
        let mut catch_light_layer: Option<&Layer> = None;
        let mut i = 0;
        while i < def.layers.len() {
            let layer = &def.layers[i];
            if matches!(layer.recolor, Recolor::Coat) {
                has_coat = true;
            }
            if matches!(layer.role, GlyphRole::Eye) {
                if matches!(layer.recolor, Recolor::Fixed) && fixed_eye_fill.is_none() {
                    fixed_eye_fill = Some(layer.fill);
                }
                if eye_layer.is_none() {
                    eye_layer = Some(layer);
                }
            }
            if matches!(layer.role, GlyphRole::CatchLight) && catch_light_layer.is_none() {
                catch_light_layer = Some(layer);
            }
            i += 1;
        }
        let fixed_eye_lum = match fixed_eye_fill {
            Some(fill) if has_coat => {
                Some(relative_luminance_with(relative_luminance_table(), fill))
            }
            _ => None,
        };
        let eye_line = match eye_layer {
            Some(eyes) => layer_eye_line(eyes),
            None => None,
        };
        let eye_paths: &'static [&'static [PathSeg]] = match eye_layer {
            Some(eyes) => eyes.paths,
            None => &[],
        };
        let mut reflections = [None; MAX_GLYPH_EYE_PATHS];
        let mut reflection_count = 0usize;
        if let (Some(eyes), Some(catch_lights)) = (eye_layer, catch_light_layer) {
            assert!(eyes.paths.len() <= MAX_GLYPH_EYE_PATHS);
            assert!(catch_lights.paths.len() <= MAX_GLYPH_EYE_PATHS);
            let mut all_eye_bounds = [None; MAX_GLYPH_EYE_PATHS];
            let mut e = 0;
            while e < eyes.paths.len() {
                all_eye_bounds[e] = path_bounds(eyes.paths[e]);
                e += 1;
            }
            let eye_bounds_slice = all_eye_bounds.split_at(eyes.paths.len()).0;
            let mut c = 0;
            while c < catch_lights.paths.len() {
                if let Some((cx0, cy0, cx1, cy1)) = path_bounds(catch_lights.paths[c]) {
                    let (pcx, pcy) = ((cx0 + cx1) * 0.5, (cy0 + cy1) * 0.5);
                    if let Some((_, eye_bounds)) = closest_eye_bounds(eye_bounds_slice, pcx, pcy) {
                        let (ex0, ey0, ex1, ey1) = eye_bounds;
                        let eye_w_norm = ex1 - ex0;
                        let eye_h_norm = ey1 - ey0;
                        let (ecx, ecy) = ((ex0 + ex1) * 0.5, (ey0 + ey1) * 0.5);
                        let echo_x = (pcx + 1.55 * (ecx - pcx))
                            .clamp(ex0 + 0.25 * eye_w_norm, ex1 - 0.25 * eye_w_norm);
                        let echo_y = (pcy + 1.55 * (ecy - pcy))
                            .clamp(ey0 + 0.25 * eye_h_norm, ey1 - 0.25 * eye_h_norm);
                        reflections[reflection_count] = Some(OpenEyeReflection {
                            pcx,
                            pcy,
                            echo_x,
                            echo_y,
                            eye_w_norm,
                            eye_h_norm,
                        });
                        reflection_count += 1;
                    }
                }
                c += 1;
            }
        }
        Self {
            fixed_eye_lum,
            eye_line,
            eye_paths,
            reflections,
            reflection_count: reflection_count as u8,
        }
    }
}

static GLYPH_META: [GlyphMeta; GLYPHS.len()] = {
    let mut meta = [GlyphMeta {
        fixed_eye_lum: None,
        eye_line: None,
        eye_paths: &[],
        reflections: [None; MAX_GLYPH_EYE_PATHS],
        reflection_count: 0,
    }; GLYPHS.len()];
    let mut i = 0;
    while i < GLYPHS.len() {
        meta[i] = GlyphMeta::from_def(&GLYPHS[i]);
        i += 1;
    }
    meta
};

#[cfg(test)]
const fn open_eye_reflection_eligible(bounds: (f32, f32, f32, f32), xform: PathTransform) -> bool {
    let (ex0, ey0, ex1, ey1) = bounds;
    let eye_w = (ex1 - ex0) * xform.scale_x;
    let eye_h = (ey1 - ey0) * xform.scale_y;
    eye_w.min(eye_h) >= OPEN_EYE_REFLECTION_MIN_PX
        && !(eye_w > 0.0 && eye_h < OPEN_EYE_SHALLOW_ASPECT * eye_w)
}

#[inline]
pub(crate) fn prepare_scratch_tile(slot: &mut Option<Tile>, w: u32, h: u32) -> &mut Tile {
    match slot {
        Some(tile) => {
            tile.reset(w, h);
            tile
        }
        None => slot.insert(Tile::new(w, h)),
    }
}

#[inline]
pub(crate) fn take_or_new_tile(recycled: Option<Tile>, w: u32, h: u32) -> Tile {
    match recycled {
        Some(mut tile) => {
            tile.reset(w, h);
            tile
        }
        None => Tile::new(w, h),
    }
}

#[inline]
fn paint_open_eye_reflections(
    tile: &mut Tile,
    meta: &GlyphMeta,
    xform: PathTransform,
    mask_scratch: &mut Option<Tile>,
) {
    if meta.reflection_count == 0 || xform.scale_x.min(xform.scale_y) < OPEN_EYE_REFLECTION_MIN_PX {
        return;
    }
    let mut painted_mask = false;

    let n_ref = usize::from(meta.reflection_count);
    let mut ri = 0;
    while ri < n_ref {
        let slot = &meta.reflections[ri];
        ri += 1;
        let Some(reflection) = slot else {
            continue;
        };
        let eye_w = reflection.eye_w_norm * xform.scale_x;
        let eye_h = reflection.eye_h_norm * xform.scale_y;
        let minor = eye_w.min(eye_h);
        if minor < OPEN_EYE_REFLECTION_MIN_PX
            || (eye_w > 0.0 && eye_h < OPEN_EYE_SHALLOW_ASPECT * eye_w)
        {
            continue;
        }
        if !painted_mask {
            let mask = prepare_scratch_tile(mask_scratch, tile.width(), tile.height());
            fill_path_fixed(mask, meta.eye_paths, (0.0, 0.0, 0.0), 1.0, xform);
            painted_mask = true;
        }
        let Some(eye_mask) = mask_scratch.as_ref() else {
            continue;
        };

        let primary_r = (minor * OPEN_EYE_PRIMARY_FRAC).clamp(
            OPEN_EYE_PRIMARY_MIN_RADIUS_PX,
            OPEN_EYE_PRIMARY_MAX_RADIUS_PX,
        );
        paint_dark_masked_disc(
            tile,
            eye_mask,
            xform.dx + reflection.pcx * xform.scale_x,
            xform.dy + reflection.pcy * xform.scale_y,
            primary_r,
            CATCH_LIGHT_RGB,
            1.0,
        );

        if minor >= OPEN_EYE_ECHO_MIN_PX {
            let echo_r = (minor * OPEN_EYE_ECHO_FRAC)
                .clamp(OPEN_EYE_ECHO_MIN_RADIUS_PX, OPEN_EYE_ECHO_MAX_RADIUS_PX);
            paint_dark_masked_disc(
                tile,
                eye_mask,
                xform.dx + reflection.echo_x * xform.scale_x,
                xform.dy + reflection.echo_y * xform.scale_y,
                echo_r,
                (1.0, 0.95, 0.98),
                0.72,
            );
        }
    }
}

#[inline]
fn paint_glyph(
    tile: &mut Tile,
    id: CatGlyphId,
    fills: &ResolvedFills,
    xform: PathTransform,
    eyes: EyesFrame,
    reflections: bool,
    mask_scratch: &mut Option<Tile>,
) {
    let idx = id as usize;
    let def = &GLYPHS[idx];
    let meta = &GLYPH_META[idx];
    let use_eye_ink = match meta.fixed_eye_lum {
        Some(eye_lum) => {
            let coat_lum = fills.coat_lum;
            (eye_lum.max(coat_lum) + 0.05) / (eye_lum.min(coat_lum) + 0.05) < MIN_EYE_CONTRAST
        }
        None => false,
    };
    let eye_xform = if !matches!(eyes, EyesFrame::Open)
        && let Some(yc) = meta.eye_line
    {
        let openness = eyes.openness();
        PathTransform {
            scale_y: openness * xform.scale_y,
            dy: xform.dy + yc * (1.0 - openness) * xform.scale_y,
            ..xform
        }
    } else {
        xform
    };
    let mut li = 0;
    while li < def.layers.len() {
        let layer = &def.layers[li];
        li += 1;
        // A shut eye shows no iris or catch-light.
        if matches!(eyes, EyesFrame::Blink)
            && matches!(layer.role, GlyphRole::Iris | GlyphRole::CatchLight)
        {
            continue;
        }
        let lxform = if matches!(
            layer.role,
            GlyphRole::Eye | GlyphRole::Iris | GlyphRole::CatchLight
        ) {
            eye_xform
        } else {
            xform
        };
        let (col, alpha) = resolve_glyph_layer(layer, fills, use_eye_ink);
        if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker) {
            paint_outline_halo(tile, layer, fills, alpha, lxform);
        }
        fill_path_fixed(tile, layer.paths, col, alpha, lxform);
    }
    if reflections && matches!(eyes, EyesFrame::Open) {
        paint_open_eye_reflections(tile, meta, xform, mask_scratch);
    }
}

#[inline]
fn paint_variant_onto(
    tile: &mut Tile,
    id: CatGlyphId,
    accessory: Option<CatGlyphId>,
    fills: &ResolvedFills,
    (w, h): (u32, u32),
    eyes: EyesFrame,
    mask_scratch: &mut Option<Tile>,
) {
    if w == 0 || h == 0 {
        return;
    }
    paint_glyph(
        tile,
        id,
        fills,
        PathTransform::fit(w, h),
        eyes,
        true,
        mask_scratch,
    );
    if let Some(acc) = accessory {
        paint_glyph(
            tile,
            acc,
            fills,
            accessory_transform(acc, w, h),
            EyesFrame::Open,
            false,
            mask_scratch,
        );
    }
    if tile.width() > w {
        let r = f32::from(PATCH_STRIP) * 0.5;
        tile.disc(w as f32 + r, r, r.max(1.0), CATCH_LIGHT_RGB, 1.0);
    }
}

/// Bake one authored glyph variant to a `(w + PATCH_STRIP) × h` RGBA [`Tile`].
#[inline]
#[must_use]
pub fn bake_variant(id: CatGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    bake_variant_with(id, None, fills, w, h, EyesFrame::Open)
}

/// [`bake_variant`] with an optional accessory composited at its [`accessory_attach`] anchor.
#[inline]
#[must_use]
pub fn bake_variant_with(
    id: CatGlyphId,
    accessory: Option<CatGlyphId>,
    fills: &ResolvedFills,
    w: u32,
    h: u32,
    eyes: EyesFrame,
) -> Tile {
    let mut tile = Tile::new(w + u32::from(PATCH_STRIP), h);
    paint_variant_onto(&mut tile, id, accessory, fills, (w, h), eyes, &mut None);
    tile
}

/// Bake-cache key: everything that changes the baked texels, all integer so it is `Eq`/`Hash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BakeKeyV4 {
    /// The authored variant/special glyph.
    pub variant: CatGlyphId,
    /// Optional overlay accessory glyph (bow/crown/bell).
    pub accessory: Option<CatGlyphId>,
    /// [`COAT_RAMP`] index for `Recolor::Coat` layers.
    pub coat: u8,
    /// [`EYE_RAMP`] index for `Recolor::Iris` layers.
    pub iris: u8,
    /// Quantized local foreground/background context.
    pub colors: CatColorKey,
    /// Art-box width in px (the tile is `w + PATCH_STRIP` wide).
    pub w: u16,
    /// Art-box (and tile) height in px.
    pub h: u16,
    /// Blink/squint frame ([`EyesFrame::Open`] = authored art).
    pub eyes: EyesFrame,
}

impl BakeKeyV4 {
    /// Resolve this key's ramp indices + local palette into bake fills.
    #[inline]
    #[must_use]
    pub const fn fills(&self) -> ResolvedFills {
        ResolvedFills::from_context(self.coat, self.iris, self.colors)
    }

    /// Bake the tile this key describes (the cache-miss path).
    #[inline]
    #[must_use]
    pub fn bake(&self) -> Tile {
        bake_variant_with(
            self.variant,
            self.accessory,
            &self.fills(),
            u32::from(self.w),
            u32::from(self.h),
            self.eyes,
        )
    }

    #[inline]
    fn bake_scratch<'a>(
        &self,
        scratch: &'a mut Option<Tile>,
        mask_scratch: &mut Option<Tile>,
    ) -> &'a Tile {
        let w = u32::from(self.w);
        let h = u32::from(self.h);
        let tile = prepare_scratch_tile(scratch, w, h);
        paint_variant_onto(
            tile,
            self.variant,
            self.accessory,
            &self.fills(),
            (w, h),
            self.eyes,
            mask_scratch,
        );
        tile
    }
}

#[cfg(test)]
#[path = "cat_baker_tests.rs"]
mod tests;

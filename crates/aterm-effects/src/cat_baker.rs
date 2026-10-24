// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Full-colour RGBA tile baker and LRU cache of the cat and pet arts.

use std::sync::Arc;

use aterm_core::render::{FxArt, FxTexels};
use aterm_scene::vector::{FIXED_ONE, PathSeg};
use aterm_scene::{PathTransform, Tile, fill_path_fixed, mix_rgb};

use crate::cat_glyphs_gen::{CatGlyphId, GLYPHS, GlyphRole, Layer, Recolor};
use crate::color_math::{hsv2rgb, relative_luminance, rgb2hsv};

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
#[derive(Default)]
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

impl CatBaker {
    /// Structural slot count for a cell height.
    #[inline]
    #[must_use]
    pub const fn slot_count(cell_h: u16) -> usize {
        let ch = if cell_h < 1 { 1 } else { cell_h as usize };
        let slot_bytes = (4 * ch) * (2 * ch) * 4;
        let n = MAX_ATLAS_BYTES / slot_bytes;
        if n < 1 {
            1
        } else if n > MAX_SLOTS {
            MAX_SLOTS
        } else {
            n
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
        for s in &mut self.slots {
            cleared_any |= s.take().is_some();
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
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Whether no tile is baked.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
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
        let slot = self.slots.iter_mut().flatten().find(|s| s.key == key)?;
        slot.last_used = clock;
        Some(CatTile {
            art: Arc::clone(&slot.art),
        })
    }

    /// Spend one bake from this frame's budget and pick the slot to bake into: a
    /// free slot first, else the least-recently-used (lowest index on ties).
    #[inline]
    fn bake_slot(&mut self) -> Option<usize> {
        if self.bakes_left == 0 {
            return None;
        }
        self.bakes_left -= 1;
        let mut victim: Option<(usize, u64)> = None;
        for (idx, slot) in self.slots.iter().enumerate() {
            let Some(s) = slot.as_ref() else {
                return Some(idx);
            };
            if s.last_used != self.clock
                && victim.is_none_or(|(_, best_used)| s.last_used < best_used)
            {
                victim = Some((idx, s.last_used));
            }
        }
        victim.map(|(idx, _)| idx)
    }

    /// Intern the `w × h` top-left of the `tw`-wide straight RGBA `src` as
    /// slot `i`'s art.
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
            for row in src.chunks_exact(tw * 4).take(hu) {
                data.extend_from_slice(&row[..wu * 4]);
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

const OPEN_EYE_REFLECTION_MIN_PX: f32 = 3.0;
const OPEN_EYE_SHALLOW_ASPECT: f32 = 0.6;
const OPEN_EYE_PRIMARY_FRAC: f32 = 0.17;
const OPEN_EYE_PRIMARY_MIN_RADIUS_PX: f32 = 0.62;
const OPEN_EYE_PRIMARY_MAX_RADIUS_PX: f32 = 1.08;
const OPEN_EYE_ECHO_MIN_PX: f32 = 4.5;
const OPEN_EYE_ECHO_FRAC: f32 = 0.075;
const OPEN_EYE_ECHO_MIN_RADIUS_PX: f32 = 0.55;
const OPEN_EYE_ECHO_MAX_RADIUS_PX: f32 = 0.72;

/// Unpack an authored `0x00RR_GGBB` fill into `0..=1` float channels.
#[inline]
pub(crate) const fn rgb(hex: u32) -> (f32, f32, f32) {
    (
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    )
}

/// Paint a supersampled reflection disc only over opaque dark eye texels covered by `eye_mask`.
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
    for y in y0..y1 {
        let dy0 = y as f32 + 0.125 - cy;
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
        let dy2 = [dy0_2, dy1 * dy1, dy2_v * dy2_v, dy3_2];
        let dy2_max = dy0_2.max(dy3_2);
        let row_base = (y as u32 * tile_w) as usize;
        for x in x0..x1 {
            let i = (row_base + x as usize) * 4;
            let covered_by_eye = mask_px.get(i + 3).is_some_and(|&a| a != 0);
            let dark_eye = covered_by_eye
                && tile
                    .pixels()
                    .get(i..i + 4)
                    .is_some_and(|p| p[3] == 255 && p[..3].iter().all(|&c| c < 96));
            if !dark_eye {
                continue;
            }
            let dx0 = x as f32 + 0.125 - cx;
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
                tile.over(x, y, col, alpha);
                continue;
            }
            let dx1 = dx0 + 0.25;
            let dx2_v = dx0 + 0.50;
            let dx2 = [dx0_2, dx1 * dx1, dx2_v * dx2_v, dx3_2];
            let mut hit = 0u32;
            for &dy2 in &dy2 {
                if dy2 <= rr {
                    let rem = rr - dy2;
                    hit += u32::from(dx2[0] <= rem)
                        + u32::from(dx2[1] <= rem)
                        + u32::from(dx2[2] <= rem)
                        + u32::from(dx2[3] <= rem);
                }
            }
            if hit > 0 {
                tile.over(x, y, col, alpha * hit as f32 / 16.0);
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
        Self {
            accent: 12,
            background: 0,
        }
    }
}

impl CatColorKey {
    /// Resolve matched-word, neighboring-text, and local-background colors on the cold rescan path.
    #[inline]
    #[must_use]
    pub fn from_rgb(background: u32, foreground: u32, surrounding: u32) -> Self {
        let harmonized = mix_rgb(foreground, surrounding, 0.35);
        let (h, s, _) = rgb2hsv(harmonized);
        let accent = if s < 0.12 {
            12
        } else {
            (((h + 15.0) / 30.0).floor() as u8) % 12
        };
        let background = background_band(background);
        Self { accent, background }
    }

    /// Like [`Self::from_rgb`], plus one bit of footprint structure that RGB averaging loses.
    #[inline]
    #[must_use]
    pub fn from_rgb_span(
        background: u32,
        foreground: u32,
        surrounding: u32,
        min_background_band: u8,
        max_background_band: u8,
    ) -> Self {
        let mut key = Self::from_rgb(background, foreground, surrounding);
        if (key.background >= 1 || max_background_band.min(3) >= 3)
            && min_background_band.min(3) <= 1
            && max_background_band.min(3) >= 2
        {
            key.background = 4;
        }
        key
    }

    #[inline]
    #[must_use]
    pub fn background_band(background: u32) -> u8 {
        background_band(background)
    }

    #[inline]
    #[must_use]
    pub const fn dark(self) -> bool {
        self.background <= 1
    }
}

#[inline]
fn background_band(background: u32) -> u8 {
    let l = relative_luminance(background);
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
}

#[derive(Clone, Copy)]
struct CoatEyePair {
    coat: (f32, f32, f32),
    eye_ink: (f32, f32, f32),
}

const OUTLINE_BASES: [u32; 5] = [
    OUTLINE_PALE,
    0x00FF_F2E8,
    0x0018_1520,
    0x0010_0D16,
    EYE_INK_DARK,
];

struct FillsLut {
    coat_eye: [[[CoatEyePair; 2]; CONTEXT_ACCENTS.len()]; COAT_RAMP.len()],
    iris: [[(f32, f32, f32); CONTEXT_ACCENTS.len()]; EYE_RAMP.len()],
    outline: [[(f32, f32, f32); CONTEXT_ACCENTS.len()]; OUTLINE_BASES.len()],
    underlay: [(f32, f32, f32); CONTEXT_ACCENTS.len()],
}

fn fills_lut() -> &'static FillsLut {
    static LUT: std::sync::OnceLock<FillsLut> = std::sync::OnceLock::new();
    LUT.get_or_init(|| FillsLut {
        coat_eye: std::array::from_fn(|coat_idx| {
            let coat_stop = COAT_RAMP[coat_idx];
            std::array::from_fn(|accent_idx| {
                let accent = CONTEXT_ACCENTS[accent_idx];
                let base_coat = rgb(mix_rgb(coat_stop, accent, 0.12));
                let dark_eye = rgb(mix_rgb(EYE_INK_DARK, accent, 0.06));
                let pale_eye = rgb(mix_rgb(EYE_INK_PALE, accent, 0.06));
                std::array::from_fn(|lift| {
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
                    CoatEyePair { coat, eye_ink }
                })
            })
        }),
        iris: std::array::from_fn(|iris_idx| {
            let iris = EYE_RAMP[iris_idx];
            std::array::from_fn(|accent_idx| rgb(mix_rgb(iris, CONTEXT_ACCENTS[accent_idx], 0.28)))
        }),
        outline: std::array::from_fn(|bg_idx| {
            let outline_base = OUTLINE_BASES[bg_idx];
            std::array::from_fn(|accent_idx| {
                rgb(mix_rgb(outline_base, CONTEXT_ACCENTS[accent_idx], 0.10))
            })
        }),
        underlay: std::array::from_fn(|accent_idx| {
            rgb(mix_rgb(EYE_INK_PALE, CONTEXT_ACCENTS[accent_idx], 0.10))
        }),
    })
}

impl ResolvedFills {
    /// Resolve genome ramp indices into concrete colours (the cache key → fills step).
    #[inline]
    #[must_use]
    pub fn from_indices(coat: u8, iris: u8, dark_bg: bool) -> Self {
        let colors = CatColorKey {
            accent: 12,
            background: if dark_bg { 0 } else { 3 },
        };
        Self::from_context(coat, iris, colors)
    }

    /// Resolve genome fills through the local palette.
    #[inline]
    #[must_use]
    pub fn from_context(coat: u8, iris: u8, colors: CatColorKey) -> Self {
        let accent_idx = usize::from(colors.accent.min(12));
        let coat_idx = usize::from(coat).min(COAT_RAMP.len() - 1);
        let iris_idx = usize::from(iris).min(EYE_RAMP.len() - 1);
        let outline_idx = match colors.background {
            1 => 1,
            2 => 2,
            3 => 3,
            4 => 4,
            _ => 0,
        };
        let lift = usize::from(matches!(colors.background, 0 | 4));
        let lut = fills_lut();
        let CoatEyePair { coat, eye_ink } = lut.coat_eye[coat_idx][accent_idx][lift];
        Self {
            coat,
            iris: lut.iris[iris_idx][accent_idx],
            eye_ink,
            outline: lut.outline[outline_idx][accent_idx],
            outline_underlay: (colors.background == 4).then_some(lut.underlay[accent_idx]),
        }
    }
}

#[inline]
fn packed_rgb((r, g, b): (f32, f32, f32)) -> u32 {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
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

fn lift_coat_luminance(coat: (f32, f32, f32), floor: f32) -> (f32, f32, f32) {
    let table = crate::color_math::relative_luminance_table();
    let lum_rgb = |rgb: u32| crate::color_math::relative_luminance_with(table, rgb);
    let mut lo_rgb = packed_rgb(coat);
    if lum_rgb(lo_rgb) >= floor {
        return coat;
    }
    let (h, s, _v) = rgb2hsv(lo_rgb);
    let pale = rgb(hsv2rgb(h, s * 0.55, 0.78));
    let mut hi_rgb = packed_rgb(pale);
    if lum_rgb(hi_rgb) <= floor {
        return pale;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..6 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = packed_rgb(mix3(coat, pale, mid));
        if mid_rgb == lo_rgb || (mid_rgb != hi_rgb && lum_rgb(mid_rgb) < floor) {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
    }
    mix3(coat, pale, hi)
}

/// `COAT_RAMP[coat]` lifted to [`COAT_MIN_LUM_DARK_BG`] as on the dark background classes, without
/// the accent nudge (which cancels out of a two-coat comparison).
#[cfg(test)]
pub(crate) fn dark_bg_coat_stop(coat: u8) -> (f32, f32, f32) {
    let stop = rgb(COAT_RAMP[usize::from(coat).min(COAT_RAMP.len() - 1)]);
    lift_coat_luminance(stop, COAT_MIN_LUM_DARK_BG)
}

#[inline]
fn contrast_ratio(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let table = crate::color_math::relative_luminance_table();
    let a = crate::color_math::relative_luminance_with(table, packed_rgb(a));
    let b = crate::color_math::relative_luminance_with(table, packed_rgb(b));
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
fn resolve_glyph_layer(
    layer: &Layer,
    fills: &ResolvedFills,
    coat_lum: Option<(&crate::color_math::RelativeLuminanceTable, f32)>,
) -> ((f32, f32, f32), f32) {
    if layer.role == GlyphRole::Eye
        && layer.recolor == Recolor::Fixed
        && let Some((table, coat_lum)) = coat_lum
    {
        let eye_lum = crate::color_math::relative_luminance_with(table, layer.fill);
        let contrast = (eye_lum.max(coat_lum) + 0.05) / (eye_lum.min(coat_lum) + 0.05);
        if contrast < MIN_EYE_CONTRAST {
            return (fills.eye_ink, 1.0);
        }
    }
    resolve_layer(layer, fills)
}

/// The mixed-background keyline under an outline layer.
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
    for (dx, dy) in [(halo, 0.0), (-halo, 0.0), (0.0, halo), (0.0, -halo)] {
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
fn accessory_transform(acc: CatGlyphId, w: u32, h: u32) -> PathTransform {
    let (cx, cy, s) = accessory_attach(acc);
    let max_w = s * w as f32;
    let max_h = s * h as f32;
    let aspect = (f32::from(GLYPHS[acc as usize].aspect_x1000) / 1000.0).max(0.001);
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
        Some((lo as f32 + hi as f32) * 0.5 / FIXED_ONE as f32)
    } else {
        None
    }
}

/// Normalized control-hull bounds `(x0, y0, x1, y1)` of one subpath.
#[must_use]
pub(crate) const fn path_bounds(path: &[PathSeg]) -> Option<(f32, f32, f32, f32)> {
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
            x0 as f32 / FIXED_ONE as f32,
            y0 as f32 / FIXED_ONE as f32,
            x1 as f32 / FIXED_ONE as f32,
            y1 as f32 / FIXED_ONE as f32,
        ))
    } else {
        None
    }
}

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
fn closest_eye_path(eyes: &Layer, pcx: f32, pcy: f32) -> Option<(usize, (f32, f32, f32, f32))> {
    let mut inline_bounds = [None; MAX_GLYPH_EYE_PATHS];
    let bounds = &mut inline_bounds[..eyes.paths.len()];
    for (slot, eye) in bounds.iter_mut().zip(eyes.paths.iter()) {
        *slot = path_bounds(eye);
    }
    closest_eye_bounds(bounds, pcx, pcy)
}

const MAX_GLYPH_EYE_PATHS: usize = 8;

#[derive(Clone, Copy)]
struct OpenEyeReflection {
    pcx: f32,
    pcy: f32,
    echo_x: f32,
    echo_y: f32,
    eye_bounds: (f32, f32, f32, f32),
}

#[derive(Clone, Copy)]
struct GlyphMeta {
    needs_coat_lum: bool,
    eye_line: Option<f32>,
    eye_paths: &'static [&'static [PathSeg]],
    reflections: [Option<OpenEyeReflection>; MAX_GLYPH_EYE_PATHS],
    reflection_count: u8,
}

impl GlyphMeta {
    const fn from_def(def: &crate::cat_glyphs_gen::GlyphDef) -> Self {
        let mut has_coat = false;
        let mut has_fixed_eye = false;
        let mut eye_layer: Option<&Layer> = None;
        let mut catch_light_layer: Option<&Layer> = None;
        let mut i = 0;
        while i < def.layers.len() {
            let layer = &def.layers[i];
            if matches!(layer.recolor, Recolor::Coat) {
                has_coat = true;
            }
            if matches!(layer.role, GlyphRole::Eye) {
                if matches!(layer.recolor, Recolor::Fixed) {
                    has_fixed_eye = true;
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
                        let (ecx, ecy) = ((ex0 + ex1) * 0.5, (ey0 + ey1) * 0.5);
                        let echo_x = (pcx + 1.55 * (ecx - pcx))
                            .clamp(ex0 + 0.25 * (ex1 - ex0), ex1 - 0.25 * (ex1 - ex0));
                        let echo_y = (pcy + 1.55 * (ecy - pcy))
                            .clamp(ey0 + 0.25 * (ey1 - ey0), ey1 - 0.25 * (ey1 - ey0));
                        reflections[reflection_count] = Some(OpenEyeReflection {
                            pcx,
                            pcy,
                            echo_x,
                            echo_y,
                            eye_bounds,
                        });
                        reflection_count += 1;
                    }
                }
                c += 1;
            }
        }
        Self {
            needs_coat_lum: has_coat && has_fixed_eye,
            eye_line,
            eye_paths,
            reflections,
            reflection_count: reflection_count as u8,
        }
    }
}

static GLYPH_META: [GlyphMeta; GLYPHS.len()] = {
    let mut meta = [GlyphMeta {
        needs_coat_lum: false,
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

fn open_eye_reflection_eligible(bounds: (f32, f32, f32, f32), xform: PathTransform) -> bool {
    let (ex0, ey0, ex1, ey1) = bounds;
    let eye_w = (ex1 - ex0) * xform.scale_x;
    let eye_h = (ey1 - ey0) * xform.scale_y;
    eye_w.min(eye_h) >= OPEN_EYE_REFLECTION_MIN_PX
        && !(eye_w > 0.0 && eye_h < OPEN_EYE_SHALLOW_ASPECT * eye_w)
}

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

    for reflection in meta.reflections[..usize::from(meta.reflection_count)]
        .iter()
        .flatten()
    {
        let eye_bounds = reflection.eye_bounds;
        if !open_eye_reflection_eligible(eye_bounds, xform) {
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
        let (ex0, ey0, ex1, ey1) = eye_bounds;
        let eye_w = (ex1 - ex0) * xform.scale_x;
        let eye_h = (ey1 - ey0) * xform.scale_y;
        let minor = eye_w.min(eye_h);

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
            rgb(CATCH_LIGHT),
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
    let coat_lum = meta.needs_coat_lum.then(|| {
        let table = crate::color_math::relative_luminance_table();
        (
            table,
            crate::color_math::relative_luminance_with(table, packed_rgb(fills.coat)),
        )
    });
    let eye_xform = (!matches!(eyes, EyesFrame::Open))
        .then_some(meta.eye_line)
        .flatten()
        .map_or(xform, |yc| {
            let openness = eyes.openness();
            PathTransform {
                scale_y: openness * xform.scale_y,
                dy: xform.dy + yc * (1.0 - openness) * xform.scale_y,
                ..xform
            }
        });
    for layer in def.layers {
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
        let (col, alpha) = resolve_glyph_layer(layer, fills, coat_lum);
        if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker) {
            paint_outline_halo(tile, layer, fills, alpha, lxform);
        }
        fill_path_fixed(tile, layer.paths, col, alpha, lxform);
    }
    if reflections && matches!(eyes, EyesFrame::Open) {
        paint_open_eye_reflections(tile, meta, xform, mask_scratch);
    }
}

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
        tile.disc(w as f32 + r, r, r.max(1.0), rgb(CATCH_LIGHT), 1.0);
    }
}

/// Bake one authored glyph variant to a `(w + PATCH_STRIP) × h` RGBA [`Tile`].
#[inline]
#[must_use]
pub fn bake_variant(id: CatGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    bake_variant_with(id, None, fills, w, h, EyesFrame::Open)
}

/// [`bake_variant`] with an optional accessory composited at its [`accessory_attach`] anchor.
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
    pub fn fills(&self) -> ResolvedFills {
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
mod tests {
    use super::*;
    use crate::cat_glyphs_gen::GLYPH_IDS;

    fn vk(coat: u8) -> BakeKeyV4 {
        BakeKeyV4 {
            variant: CatGlyphId::S100,
            accessory: None,
            coat,
            iris: 3,
            colors: CatColorKey::default(),
            w: 40,
            h: 24,
            eyes: EyesFrame::Open,
        }
    }

    #[test]
    fn a_slot_served_this_frame_is_never_evicted_under_its_own_quad() {
        const CH: u16 = 90;
        let slots = CatBaker::slot_count(CH);
        assert_eq!(slots, 8, "fixture: a small-atlas geometry");

        let mut baker = CatBaker::default();
        let mut filled = 0usize;
        while filled < slots {
            baker.begin_frame(CH / 2, CH);
            for coat in 0..slots as u8 {
                if baker.get_v4(&vk(coat)).is_some() {
                    filled = filled.max(usize::from(coat) + 1);
                }
            }
        }

        baker.begin_frame(CH / 2, CH);
        let mut served = Vec::new();
        for coat in 0..slots as u8 {
            let tile = baker
                .get_v4(&vk(coat))
                .expect("a resident key is an exact hit and never needs a bake");
            served.push(tile.art.id());
        }
        let stranger = baker.get_v4(&vk(200));

        assert!(
            stranger.is_none(),
            "every slot was served this frame, so the bake must be REFUSED \
             (callers hold a last-good tile and retry); got {stranger:?}"
        );
        for coat in 0..slots as u8 {
            assert_eq!(
                baker.peek_v4(&vk(coat)).map(|t| t.art.id()),
                Some(served[usize::from(coat)]),
                "the stranger re-baked over a slot already drawn this frame"
            );
        }
    }

    #[test]
    fn slot_count_is_structural() {
        for ch in [14u16, 20, 40, 56, 90] {
            let bytes =
                CatBaker::slot_count(ch) * (4 * usize::from(ch)) * (2 * usize::from(ch)) * 4;
            assert!(bytes <= MAX_ATLAS_BYTES, "ch={ch}: {bytes} > 2 MiB");
        }
    }

    #[test]
    fn lru_evicts_oldest_and_version_bumps() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        let cap = b.slots.len();
        assert_eq!(cap, 32);
        let v0 = b.version();
        for i in 0..cap as u8 {
            b.begin_frame(10, 20);
            assert!(b.get_v4(&vk(i)).is_some(), "slot {i} bakes");
        }
        assert_eq!(b.version(), v0 + cap as u64, "one bump per bake");
        b.begin_frame(10, 20);
        assert!(b.get_v4(&vk(0)).is_some());
        b.begin_frame(10, 20);
        assert!(b.get_v4(&vk(200)).is_some(), "cap-full insert evicts");
        assert_eq!(b.slots.iter().flatten().count(), cap, "cap never exceeded");
        assert!(
            !b.slots
                .iter()
                .flatten()
                .any(|s| s.key == SlotKey::Cat(vk(1))),
            "the least-recently-used entry was evicted"
        );
        let v1 = b.version();
        b.clear();
        assert_eq!(b.version(), v1 + 1);
        assert!(b.is_empty(), "a cleared baker holds no tile");
    }

    #[test]
    fn bake_budget_is_two_per_frame() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        assert!(b.get_v4(&vk(1)).is_some());
        assert!(b.get_v4(&vk(2)).is_some());
        assert!(
            b.get_v4(&vk(3)).is_none(),
            "third bake this frame is deferred"
        );
        assert!(b.get_v4(&vk(1)).is_some(), "a HIT still resolves");
        b.begin_frame(10, 20);
        assert!(b.get_v4(&vk(3)).is_some());
    }

    #[test]
    fn metric_change_clears_and_bumps() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        assert!(b.get_v4(&vk(0)).is_some());
        assert!(!b.is_empty());
        let v = b.version();
        b.begin_frame(12, 24);
        assert!(b.version() > v, "metric change bumps the version");
        assert!(b.is_empty(), "metric change clears");
    }

    #[test]
    fn get_v4_rejects_art_wider_than_slot_minus_strip() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        let mut k = vk(0);
        k.w = b.slot_w - PATCH_STRIP + 1;
        assert!(b.get_v4(&k).is_none(), "w + strip > slot_w ⇒ None");
    }

    #[test]
    fn a_hit_hands_back_the_held_art_and_only_a_bake_bumps_the_version() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        let a1 = b.get_v4(&vk(0)).expect("bake").art;
        let v1 = b.version();
        let a2 = b.get_v4(&vk(0)).expect("hit").art;
        assert!(Arc::ptr_eq(&a1, &a2), "a hit hands back the baked art");
        assert_eq!(b.version(), v1, "a hit bakes nothing");
        b.begin_frame(10, 20);
        let a3 = b.get_v4(&vk(1)).expect("bake 2").art;
        assert!(b.version() > v1, "a bake bumps the version");
        assert_ne!(a3.id(), a1.id(), "another key bakes another art");
    }

    #[test]
    fn a_host_tile_is_the_hosts_texels_at_its_size() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        let px: Vec<u8> = (0..6 * 5 * 4).map(|i| (i * 7 % 251) as u8).collect();
        let art = b.host_tile(0x51, 6, 5, &px).expect("fits").art;
        assert_eq!(
            (art.w(), art.h(), art.texels()),
            (6, 5, FxTexels::Rgba8Straight)
        );
        assert_eq!(art.data(), &px[..], "the art is the host's bytes");
        let held = Arc::clone(&art);
        for coat in 0..40u8 {
            b.begin_frame(10, 20);
            let _ = b.get_v4(&vk(coat));
        }
        assert!(b.host_peek(0x51).is_none(), "fixture: the tile was evicted");
        assert_eq!(
            held.data(),
            &px[..],
            "an evicted art a holder keeps is never rewritten"
        );
    }

    fn fills(coat: u8, iris: u8, dark: bool) -> ResolvedFills {
        ResolvedFills::from_indices(coat, iris, dark)
    }

    fn bake_reflection_control(
        id: CatGlyphId,
        fills: &ResolvedFills,
        w: u32,
        h: u32,
        eyes: EyesFrame,
        reflections: bool,
    ) -> Tile {
        let mut tile = Tile::new(w + u32::from(PATCH_STRIP), h);
        if w == 0 || h == 0 {
            return tile;
        }
        paint_glyph(
            &mut tile,
            id,
            fills,
            PathTransform::fit(w, h),
            eyes,
            reflections,
            &mut None,
        );
        let r = f32::from(PATCH_STRIP) * 0.5;
        tile.disc(w as f32 + r, r, r.max(1.0), rgb(CATCH_LIGHT), 1.0);
        tile
    }

    #[test]
    fn v4_bake_variant_is_deterministic() {
        let a = bake_variant(CatGlyphId::S100, &fills(5, 3, false), 96, 64);
        let b = bake_variant(CatGlyphId::S100, &fills(5, 3, false), 96, 64);
        assert_eq!(a.pixels(), b.pixels(), "same key ⇒ byte-identical tile");
        assert!(a.pixels().iter().any(|&x| x != 0), "actually painted");
        assert_eq!(
            a.width(),
            96 + u32::from(PATCH_STRIP),
            "art + patch strip wide"
        );

        let key = BakeKeyV4 {
            variant: CatGlyphId::S100,
            accessory: None,
            coat: 5,
            iris: 3,
            colors: CatColorKey {
                accent: 12,
                background: 3,
            },
            w: 96,
            h: 64,
            eyes: EyesFrame::Open,
        };
        assert_eq!(key.bake().pixels(), a.pixels(), "key bake == direct bake");
    }

    #[test]
    fn v4_coat_recolor_produces_distinct_tiles() {
        let idx = [0u8, 8, 15];
        let tiles: Vec<Vec<u8>> = idx
            .iter()
            .map(|&c| {
                bake_variant(CatGlyphId::S100, &fills(c, 3, false), 96, 64)
                    .pixels()
                    .to_vec()
            })
            .collect();
        for i in 0..tiles.len() {
            for j in (i + 1)..tiles.len() {
                assert_ne!(
                    tiles[i], tiles[j],
                    "coat ramp {} vs {} must bake distinct",
                    idx[i], idx[j]
                );
            }
        }
    }

    #[test]
    fn v4_fixed_patterned_variant_is_coat_invariant() {
        let def = &GLYPHS[CatGlyphId::SpecTuxedo as usize];
        assert!(
            def.layers.iter().all(|l| l.recolor != Recolor::Coat),
            "tuxedo must carry no Recolor::Coat layer (patterned coat is fixed)"
        );
        let a = bake_variant(CatGlyphId::SpecTuxedo, &fills(0, 3, false), 96, 96);
        let b = bake_variant(CatGlyphId::SpecTuxedo, &fills(15, 3, false), 96, 96);
        assert_eq!(a.pixels(), b.pixels(), "fixed patches ⇒ coat-invariant");
        assert!(a.pixels().iter().any(|&x| x != 0), "actually painted");
    }

    #[test]
    fn dark_coats_are_lifted_on_dark_backgrounds_only() {
        let lum = |c: (f32, f32, f32)| relative_luminance(packed_rgb(c));
        let dark_ctx = CatColorKey {
            accent: 12,
            background: 0,
        };
        let light_ctx = CatColorKey {
            accent: 12,
            background: 3,
        };
        let band1_ctx = CatColorKey {
            accent: 12,
            background: 1,
        };
        for coat in 0..4u8 {
            let rescued = ResolvedFills::from_context(coat, 4, dark_ctx);
            assert!(
                lum(rescued.coat) >= COAT_MIN_LUM_DARK_BG - 0.005,
                "coat {coat} clears the floor on band 0, got {}",
                lum(rescued.coat)
            );
            let unlifted = ResolvedFills::from_context(coat, 4, light_ctx);
            assert!(
                lum(unlifted.coat) < COAT_MIN_LUM_DARK_BG,
                "coat {coat} keeps its authored darkness on light bands"
            );
            let band1 = ResolvedFills::from_context(coat, 4, band1_ctx);
            let raw = rgb(mix_rgb(
                COAT_RAMP[usize::from(coat)],
                CONTEXT_ACCENTS[12],
                0.12,
            ));
            assert_eq!(band1.coat, raw, "coat {coat} is bit-exact on band 1");
        }
        let pale = ResolvedFills::from_context(13, 4, dark_ctx);
        let raw = rgb(mix_rgb(COAT_RAMP[13], CONTEXT_ACCENTS[12], 0.12));
        assert_eq!(pale.coat, raw, "coats above the floor are bit-exact");
        let charcoal = ResolvedFills::from_context(2, 4, dark_ctx);
        let (h_before, ..) = rgb2hsv(mix_rgb(COAT_RAMP[2], CONTEXT_ACCENTS[12], 0.12));
        let (h_after, ..) = rgb2hsv(packed_rgb(charcoal.coat));
        assert!(
            lum(charcoal.coat) >= COAT_MIN_LUM_DARK_BG - 0.005,
            "coat 2 was lifted"
        );
        assert!(
            (h_before - h_after).abs() < 12.0,
            "the rescue preserves the coat's hue family ({h_before}° → {h_after}°)"
        );
        let lifted: Vec<_> = (0..4u8)
            .map(|c| ResolvedFills::from_context(c, 4, dark_ctx).coat)
            .map(packed_rgb)
            .collect();
        for i in 0..lifted.len() {
            for j in i + 1..lifted.len() {
                assert_ne!(lifted[i], lifted[j], "coats {i} and {j} stay distinct");
            }
        }
    }

    #[test]
    fn dark_average_footprint_keeps_the_pale_outline() {
        let dark_bg = 0x000A_0A10;
        let band2_bg = 0x007A_7A7A;
        assert_eq!(background_band(band2_bg), 2, "test constant is band 2");
        let mid_clip = CatColorKey::from_rgb_span(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0, 0, 2);
        assert_eq!(
            mid_clip.background, 0,
            "a decisively dark average with a mid clip keeps the pale outline"
        );
        let bright_clip = CatColorKey::from_rgb_span(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0, 0, 3);
        assert_eq!(
            bright_clip.background, 4,
            "a bright strip inside a dark average stays mixed"
        );
        let mixed = CatColorKey::from_rgb_span(band2_bg, 0x00C0_C8D0, 0x00A0_A8B0, 1, 3);
        assert_eq!(mixed.background, 4, "a true crossover span stays mixed");
        let uniform = CatColorKey::from_rgb_span(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0, 0, 1);
        assert_eq!(
            uniform,
            CatColorKey::from_rgb(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0),
            "uniform spans are unchanged"
        );
    }

    #[test]
    fn context_palette_is_bounded_and_color_aware() {
        assert_eq!(std::mem::size_of::<CatColorKey>(), 2);
        let bg = 0x001A_1B26;
        let red_a = CatColorKey::from_rgb(bg, 0x00F0_6878, 0x00E8_7180);
        let red_b = CatColorKey::from_rgb(bg, 0x00EE_6A79, 0x00E5_7381);
        let blue = CatColorKey::from_rgb(bg, 0x0068_A8F0, 0x0071_B0E8);
        assert_eq!(red_a, red_b, "nearby colors reuse one atlas tile");
        assert_ne!(
            red_a.accent, blue.accent,
            "surrounding hue changes the art palette"
        );
        for color in [red_a, red_b, blue] {
            assert!(color.accent <= 12);
            assert!(color.background <= 3);
        }
        let red_fill = ResolvedFills::from_context(8, 4, red_a);
        let blue_fill = ResolvedFills::from_context(8, 4, blue);
        assert_ne!(red_fill.coat, blue_fill.coat);
        assert_ne!(red_fill.iris, blue_fill.iris);
    }

    #[test]
    fn context_palette_default_is_neutral_and_hue_families_are_cardinal() {
        assert_eq!(
            CatColorKey::default(),
            CatColorKey {
                accent: 12,
                background: 0,
            },
            "an absent context must not invent a red tint"
        );
        assert_eq!(
            CatColorKey::from_rgb(0, 0x00CC_CCCC, 0x0088_8888).accent,
            12,
            "achromatic text selects the neutral family"
        );

        for (rgb, expected) in [
            (0x00FF_0000, 0),
            (0x00FF_8000, 1),
            (0x00FF_FF00, 2),
            (0x0000_FF00, 4),
            (0x0000_FFFF, 6),
            (0x0000_00FF, 8),
            (0x00FF_00FF, 10),
        ] {
            assert_eq!(
                CatColorKey::from_rgb(0, rgb, rgb).accent,
                expected,
                "cardinal color #{rgb:06X}"
            );
        }
        for red_edge in [0x00FF_0004, 0x00FF_0400] {
            assert_eq!(
                CatColorKey::from_rgb(0, red_edge, red_edge).accent,
                0,
                "the circular red seam must share one cache key"
            );
        }
    }

    #[test]
    fn exterior_whiskers_follow_context_ink_but_internal_features_stay_fixed() {
        let fills = ResolvedFills::from_context(
            8,
            4,
            CatColorKey {
                accent: 6,
                background: 0,
            },
        );
        let layer = |role| Layer {
            role,
            recolor: Recolor::Fixed,
            fill: 0x0011_2233,
            paths: &[],
        };
        assert_eq!(
            resolve_layer(&layer(GlyphRole::Outline), &fills).0,
            fills.outline
        );
        assert_eq!(
            resolve_layer(&layer(GlyphRole::Whisker), &fills).0,
            fills.outline
        );
        assert_eq!(
            resolve_layer(&layer(GlyphRole::Eye), &fills).0,
            rgb(0x0011_2233),
            "internal face ink must preserve the authored expression"
        );
    }

    #[test]
    fn context_outline_clears_wcag_contrast_at_every_band_edge() {
        const MIN_CONTRAST: f32 = 3.0;
        let bands = [
            (0.0, BG_BAND_DARKEST_MAX),
            (BG_BAND_DARKEST_MAX, BG_BAND_DARK_MAX),
            (BG_BAND_DARK_MAX, BG_BAND_MID_MAX),
            (BG_BAND_MID_MAX, 1.0),
        ];
        for (background, (lo, hi)) in bands.into_iter().enumerate() {
            for accent in 0..=12 {
                let outline = ResolvedFills::from_context(
                    8,
                    4,
                    CatColorKey {
                        accent,
                        background: background as u8,
                    },
                )
                .outline;
                let channel = |value: f32| (value * 255.0).round() as u32;
                let packed =
                    (channel(outline.0) << 16) | (channel(outline.1) << 8) | channel(outline.2);
                let outline_luminance = relative_luminance(packed);
                for background_luminance in [lo, hi] {
                    let contrast = (outline_luminance.max(background_luminance) + 0.05)
                        / (outline_luminance.min(background_luminance) + 0.05);
                    assert!(
                        contrast >= MIN_CONTRAST,
                        "band {background}, accent {accent}, bg L={background_luminance:.3}, \
                         outline #{packed:06X} (L={outline_luminance:.3}) has {contrast:.3}:1"
                    );
                }
            }
        }
    }

    #[test]
    fn mixed_background_dual_keyline_covers_every_luminance() {
        const MIN_CONTRAST: f32 = 3.0;
        for accent in 0..=12 {
            let colors = CatColorKey::from_rgb_span(
                0x007F_7F7F,
                0x00EE_EEEE,
                0x0088_99AA,
                CatColorKey::background_band(0x0000_0000),
                CatColorKey::background_band(0x00FF_FFFF),
            );
            assert_eq!(colors.background, 4);
            let fills = ResolvedFills::from_context(8, 4, CatColorKey { accent, ..colors });
            let dark_luminance = relative_luminance(packed_rgb(fills.outline));
            let pale_luminance = relative_luminance(packed_rgb(
                fills
                    .outline_underlay
                    .expect("mixed context has a pale underlay"),
            ));
            for gray in 0..=255u32 {
                let background_luminance = relative_luminance((gray << 16) | (gray << 8) | gray);
                let contrast = |ink: f32| {
                    (ink.max(background_luminance) + 0.05) / (ink.min(background_luminance) + 0.05)
                };
                let best = contrast(dark_luminance).max(contrast(pale_luminance));
                assert!(
                    best >= MIN_CONTRAST,
                    "mixed accent {accent}, gray {gray} (L={background_luminance:.3}): \
                     best dual-keyline contrast is {best:.3}:1"
                );
            }
        }
    }

    #[test]
    fn mixed_background_bake_contains_both_keyline_inks() {
        let fills = ResolvedFills::from_context(
            8,
            4,
            CatColorKey {
                accent: 12,
                background: 4,
            },
        );
        let dark = packed_rgb(fills.outline);
        let pale = packed_rgb(fills.outline_underlay.expect("mixed underlay"));
        let tile = bake_variant(CatGlyphId::S100, &fills, 96, 96);
        let has_rgb = |wanted: u32| {
            tile.pixels().as_chunks::<4>().0.iter().any(|px| {
                px[3] > 0
                    && (u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2])) == wanted
            })
        };
        assert!(has_rgb(dark), "mixed bake contains its dark keyline");
        assert!(has_rgb(pale), "mixed bake contains its pale halo");
    }

    #[test]
    fn v4_accessory_overlay_changes_texels() {
        let bare = bake_variant(CatGlyphId::S100, &fills(5, 3, false), 96, 96);
        let bow = bake_variant_with(
            CatGlyphId::S100,
            Some(CatGlyphId::AccBow),
            &fills(5, 3, false),
            96,
            96,
            EyesFrame::Open,
        );
        assert_ne!(bare.pixels(), bow.pixels(), "overlay must change texels");
    }

    #[test]
    fn eyes_frame_bakes_distinct_but_open_is_authored() {
        let f = fills(5, 3, false);
        let open = bake_variant_with(CatGlyphId::S100, None, &f, 96, 64, EyesFrame::Open);
        let happy = bake_variant_with(CatGlyphId::S100, None, &f, 96, 64, EyesFrame::Happy);
        let blink = bake_variant_with(CatGlyphId::S100, None, &f, 96, 64, EyesFrame::Blink);
        let plain = bake_variant(CatGlyphId::S100, &f, 96, 64);
        assert_eq!(open.pixels(), plain.pixels(), "Open == default open bake");
        assert_ne!(open.pixels(), happy.pixels(), "a squint changes texels");
        assert_ne!(open.pixels(), blink.pixels(), "a blink changes texels");
        assert_ne!(happy.pixels(), blink.pixels(), "squint and blink differ");
    }

    #[test]
    fn flying_cursor_reflections_skip_happy_and_blink_eyes() {
        const W: u32 = 38;
        const H: u32 = 31;
        let fills = fills(8, 4, false);
        for state in [EyesFrame::Happy, EyesFrame::Blink] {
            let without = bake_reflection_control(CatGlyphId::S100, &fills, W, H, state, false);
            let with = bake_reflection_control(CatGlyphId::S100, &fills, W, H, state, true);
            assert_eq!(
                without.pixels(),
                with.pixels(),
                "{state:?} must never receive open-eye reflections"
            );
        }
    }

    #[test]
    fn flying_reflection_roster_is_contained_and_nonvacuous() {
        let mut failures = Vec::new();
        let mut eligible = 0usize;
        let mut exercised = 0usize;
        let mut preview_exercised = false;
        let mut live_expression_exercised = false;
        for &id in GLYPH_IDS {
            let def = &GLYPHS[id as usize];
            for h in [16u32, 26, 31, 48, 120] {
                let w = (h as f32 * f32::from(def.aspect_x1000) / 1000.0)
                    .round()
                    .max(1.0) as u32;
                let xform = PathTransform::fit(w, h);
                let mut eligible_indices = Vec::new();
                if let (Some(eyes), Some(catch_lights)) = (
                    def.layers.iter().find(|layer| layer.role == GlyphRole::Eye),
                    def.layers
                        .iter()
                        .find(|layer| layer.role == GlyphRole::CatchLight),
                ) {
                    for catch_light in catch_lights.paths {
                        let Some((cx0, cy0, cx1, cy1)) = path_bounds(catch_light) else {
                            continue;
                        };
                        let (pcx, pcy) = ((cx0 + cx1) * 0.5, (cy0 + cy1) * 0.5);
                        let Some((eye, bounds)) = closest_eye_path(eyes, pcx, pcy) else {
                            continue;
                        };
                        if open_eye_reflection_eligible(bounds, xform)
                            && !eligible_indices.contains(&eye)
                        {
                            eligible_indices.push(eye);
                        }
                    }
                }

                let mut eligible_masks = Vec::new();
                if let Some(eyes) = def.layers.iter().find(|layer| layer.role == GlyphRole::Eye) {
                    for &eye in &eligible_indices {
                        let mut mask = Tile::new(w, h);
                        fill_path_fixed(
                            &mut mask,
                            std::slice::from_ref(&eyes.paths[eye]),
                            (0.0, 0.0, 0.0),
                            1.0,
                            xform,
                        );
                        eligible_masks.push((eye, mask));
                    }
                }

                for dark_bg in [false, true] {
                    let fills = fills(8, 4, dark_bg);
                    let plain = bake_reflection_control(id, &fills, w, h, EyesFrame::Open, false);
                    let glossy = bake_reflection_control(id, &fills, w, h, EyesFrame::Open, true);
                    let tile_w = plain.width() as usize;
                    let changed: Vec<usize> = plain
                        .pixels()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(glossy.pixels().as_chunks::<4>().0)
                        .enumerate()
                        .filter(|(pixel, (a, b))| pixel % tile_w < w as usize && a != b)
                        .map(|(pixel, _)| pixel)
                        .collect();

                    if eligible_masks.is_empty() && !changed.is_empty() {
                        failures.push(format!(
                            "{} h={h} dark_bg={dark_bg}: ineligible glyph changed at {:?}",
                            def.id, changed
                        ));
                    }
                    for &pixel in &changed {
                        let x = pixel % tile_w;
                        let y = pixel / tile_w;
                        let mi = (y * w as usize + x) * 4;
                        let owners: Vec<usize> = eligible_masks
                            .iter()
                            .filter_map(|(eye, mask)| (mask.pixels()[mi + 3] != 0).then_some(*eye))
                            .collect();
                        let before = &plain.pixels()[pixel * 4..pixel * 4 + 4];
                        if owners.is_empty()
                            || before[3] != 255
                            || !before[..3].iter().all(|&c| c < 96)
                        {
                            failures.push(format!(
                                "{} h={h} dark_bg={dark_bg}: changed ({x},{y}) \
                                 owners={owners:?}, before={before:?}",
                                def.id
                            ));
                        }
                    }

                    for (eye, mask) in &eligible_masks {
                        eligible += 1;
                        let local: Vec<usize> = changed
                            .iter()
                            .copied()
                            .filter(|pixel| {
                                let x = pixel % tile_w;
                                let y = pixel / tile_w;
                                mask.pixels()[(y * w as usize + x) * 4 + 3] != 0
                            })
                            .collect();
                        if local.is_empty() {
                            continue;
                        }
                        exercised += 1;
                        preview_exercised |= id == CatGlyphId::S100 && h == 31;
                        live_expression_exercised |= id == CatGlyphId::S121 && h == 31;
                        let dark_count = |tile: &Tile| {
                            mask.pixels()
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .enumerate()
                                .filter(|(pixel, mask_px)| {
                                    if mask_px[3] == 0 {
                                        return false;
                                    }
                                    let x = pixel % w as usize;
                                    let y = pixel / w as usize;
                                    let i = (y * tile_w + x) * 4;
                                    let px = &tile.pixels()[i..i + 4];
                                    px[3] == 255 && px[..3].iter().all(|&c| c < 96)
                                })
                                .count()
                        };
                        let before = dark_count(&plain);
                        let after = dark_count(&glossy);
                        if after == 0 || after * 2 < before {
                            failures.push(format!(
                                "{} h={h} dark_bg={dark_bg}: eye {eye} lost its \
                                 dark core, before={before}, after={after}",
                                def.id
                            ));
                        }
                    }
                }
            }
        }
        assert!(
            eligible > 0,
            "the roster sweep must find eligible open eyes"
        );
        assert!(
            exercised > 0 && preview_exercised && live_expression_exercised,
            "the roster sweep must affect both S100 preview and live S121 cursor heads"
        );
        assert!(
            failures.is_empty(),
            "the generic flying reflection pass regressed:\n  {}",
            failures.join("\n  ")
        );
    }

    #[test]
    fn accessory_overlay_preserves_each_authored_aspect() {
        let (w, h) = (96, 72);
        for accessory in [
            CatGlyphId::AccBow,
            CatGlyphId::AccCrown,
            CatGlyphId::AccBell,
        ] {
            let transform = accessory_transform(accessory, w, h);
            let authored = f32::from(GLYPHS[accessory as usize].aspect_x1000) / 1000.0;
            let rendered = transform.scale_x / transform.scale_y;
            assert!(
                (rendered - authored).abs() <= 0.001,
                "{accessory:?}: overlay aspect {rendered:.3} != authored {authored:.3}"
            );
            let (_, _, scale) = accessory_attach(accessory);
            assert!(transform.scale_x <= scale * w as f32 + f32::EPSILON);
            assert!(transform.scale_y <= scale * h as f32 + f32::EPSILON);
        }
    }

    #[test]
    fn get_v4_reuses_scratch_tiles_and_matches_direct_bake() {
        let mut baker = CatBaker::default();
        baker.begin_frame(10, 20);
        let first = BakeKeyV4 {
            variant: CatGlyphId::S121,
            eyes: EyesFrame::Open,
            ..vk(2)
        };
        let t0 = baker.get_v4(&first).expect("first bake fits");
        let scratch_ptr = baker
            .scratch
            .as_ref()
            .expect("scratch tile retained")
            .pixels()
            .as_ptr();
        let mask_ptr = baker
            .eye_mask_scratch
            .as_ref()
            .expect("eye mask scratch retained")
            .pixels()
            .as_ptr();

        for (coat, eyes, w) in [
            (2, EyesFrame::Happy, 40),
            (2, EyesFrame::Blink, 40),
            (7, EyesFrame::Open, 40),
            (9, EyesFrame::Open, 34),
        ] {
            baker.begin_frame(10, 20);
            let key = BakeKeyV4 {
                variant: CatGlyphId::S121,
                coat,
                w,
                eyes,
                ..vk(coat)
            };
            let placed = baker.get_v4(&key).expect("subsequent bake fits");
            assert_eq!(
                baker.scratch.as_ref().unwrap().pixels().as_ptr(),
                scratch_ptr,
                "same-or-smaller bake must reuse destination scratch tile buffer"
            );
            assert_eq!(
                baker.eye_mask_scratch.as_ref().unwrap().pixels().as_ptr(),
                mask_ptr,
                "open-eye reflection mask scratch buffer must stay resident and reused"
            );
            let direct = key.bake();
            let tw = direct.width() as usize;
            let aw = usize::from(key.w);
            assert_eq!(
                (placed.art.w(), placed.art.h()),
                (key.w, key.h),
                "the art is the design box"
            );
            for row in 0..usize::from(key.h) {
                let d = row * aw * 4;
                let s = row * tw * 4;
                assert_eq!(
                    &placed.art.data()[d..d + aw * 4],
                    &direct.pixels()[s..s + aw * 4],
                    "reused scratch bake must match fresh key.bake() texels (coat={coat}, eyes={eyes:?}, w={w}, row={row})"
                );
            }
        }
        let _ = t0;
        baker.clear();
        assert!(baker.scratch.is_none() && baker.eye_mask_scratch.is_none());
    }
}

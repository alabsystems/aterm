// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Full-colour RGBA tile baker and LRU atlas cache for cat and pet quads.

use std::sync::Arc;

use aterm_render::SceneAtlas;
use aterm_scene::vector::{FIXED_ONE, PathSeg};
use aterm_scene::{PathTransform, Tile, fill_path_fixed, mix_rgb};

use crate::cat_glyphs_gen::{CatGlyphId, GLYPHS, GlyphRole, Layer, Recolor};
use crate::color_math::{hsv2rgb, relative_luminance, rgb2hsv};

/// Structural slot ceiling (§5.5): the LRU never exceeds 32 tiles.
pub const MAX_SLOTS: usize = 32;
/// Structural memory ceiling for the whole atlas: 2 MiB at any cell size.
pub const MAX_ATLAS_BYTES: usize = 2 << 20;
/// Bake-rate cap (§5.5): at most 2 tiles baked per presented frame.
pub const MAX_BAKES_PER_FRAME: u32 = 2;

/// Extra texel width baked onto every tile for the catch-light dot.
pub const PATCH_STRIP: u16 = 4;

/// A resolved cache hit: where the tile lives in the baker's atlas.
#[derive(Clone, Copy, Debug)]
pub struct CatTile {
    /// Tile origin X in atlas texels (the design box spans `ax..ax+key.w`).
    pub ax: u16,
    /// Tile origin Y in atlas texels (the design box spans `ay..ay+h`).
    pub ay: u16,
}

struct Slot {
    key: SlotKey,
    last_used: u64,
}

/// A slot's identity: a generated cat's glyph key or the stable id of a
/// host-authored tile ([`CatBaker::host_tile`]); the two never alias.
#[derive(Clone, Copy, PartialEq)]
enum SlotKey {
    Cat(BakeKeyV4),
    Host(u64),
}

/// Host-side LRU bake cache + the published `Arc<SceneAtlas>` (§5.5).
#[derive(Default)]
pub struct CatBaker {
    cell_w: u16,
    cell_h: u16,
    /// Slot (and atlas) width: `4·ch` — the §5.2 `W ≤ 4·ch` cap made structural.
    slot_w: u16,
    /// `min(32, ⌊2 MiB / slot_bytes⌋)` occupied-or-free slots, single column.
    slots: Vec<Option<Slot>>,
    /// Master straight-alpha RGBA8 texels (`slot_w × slots.len()·ch`), resident.
    rgba: Vec<u8>,
    /// Monotonic; bumped on every bake and every wholesale clear (a rebake must
    /// repaint and re-upload).
    version: u64,
    /// The last published snapshot; rebuilt only when `dirty`, so the steady
    /// state allocates nothing.
    published: Option<Arc<SceneAtlas>>,
    /// The snapshot published before [`CatBaker::published`], kept so the next
    /// dirty publish can recycle its buffer instead of cloning the atlas.
    spare: Option<Arc<SceneAtlas>>,
    dirty: bool,
    /// Per-slot write stamp (parallel to `slots`): the `version` this slot's
    /// band in `rgba` was last baked at.
    slot_version: Vec<u64>,
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

    /// Per-tick prologue: advance the LRU clock, reset the bake budget, and on a cell-metric change wholesale-clear + version-bump (§5.5 invalidation).
    #[inline]
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        self.clock = self.clock.wrapping_add(1);
        self.bakes_left = MAX_BAKES_PER_FRAME;
        if (cell_w, cell_h) != (self.cell_w, self.cell_h) {
            self.clear();
            self.cell_w = cell_w;
            self.cell_h = cell_h;
            if cell_h == 0 {
                return;
            }
            self.slot_w = cell_h.saturating_mul(4);
            let slots = Self::slot_count(cell_h);
            self.slots.clear();
            self.slots.resize_with(slots, || None);
            self.slot_version.clear();
            self.slot_version.resize(slots, 0);
            self.rgba.clear();
            self.rgba.resize(
                usize::from(self.slot_w) * slots * 2 * usize::from(cell_h) * 4,
                0,
            );
        }
    }

    /// Wholesale clear and version bump (config reload, toggle, metric change).
    #[inline]
    pub fn clear(&mut self) {
        self.scratch = None;
        self.eye_mask_scratch = None;
        if self.slots.iter().all(Option::is_none) && self.published.is_none() {
            return;
        }
        for s in &mut self.slots {
            *s = None;
        }
        self.slot_version.fill(0);
        self.rgba.fill(0);
        self.version = self.version.wrapping_add(1);
        self.published = None;
        self.spare = None;
        self.dirty = false;
    }

    /// Monotonic atlas version (folded into the frame fingerprint — a rebake must repaint).
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

    /// The published atlas snapshot, rebuilt lazily after any bake.
    pub fn atlas(&mut self) -> Option<Arc<SceneAtlas>> {
        if self.dirty {
            let width = u32::from(self.slot_w);
            let height = (self.slots.len() * 2 * usize::from(self.cell_h)) as u32;
            let band = 2 * usize::from(self.cell_h) * usize::from(self.slot_w) * 4;
            let spare = self.spare.take();
            let recycled = spare.and_then(|mut arc| {
                let synced = arc.version;
                let buf = Arc::get_mut(&mut arc)?;
                if buf.rgba.len() != self.rgba.len()
                    || buf.width != width
                    || buf.height != height
                    || band == 0
                {
                    return None;
                }
                for (i, stamp) in self.slot_version.iter().enumerate() {
                    if *stamp <= synced {
                        continue;
                    }
                    let lo = i * band;
                    let hi = lo + band;
                    if let (Some(dst), Some(src)) =
                        (buf.rgba.get_mut(lo..hi), self.rgba.get(lo..hi))
                    {
                        dst.copy_from_slice(src);
                    }
                }
                buf.version = self.version;
                Some(arc)
            });
            let published = match recycled {
                Some(arc) => arc,
                None => Arc::new(SceneAtlas {
                    width,
                    height,
                    rgba: self.rgba.clone(),
                    version: self.version,
                }),
            };
            self.spare = self.published.take();
            self.published = Some(published);
            self.dirty = false;
        }
        self.published.clone()
    }

    /// Look up a [`BakeKeyV4`] glyph tile, baking on a miss when the per-frame budget allows, and blit it at its slot origin (atlas row `i·2·ch`).
    pub fn get_v4(&mut self, key: &BakeKeyV4) -> Option<CatTile> {
        // The whole baked tile is `key.w + PATCH_STRIP` wide and must fit the slot.
        if self.cell_h == 0
            || key.w == 0
            || u32::from(key.w) + u32::from(PATCH_STRIP) > u32::from(self.slot_w)
        {
            return None;
        }
        let ch = usize::from(self.cell_h);
        let slot_h = 2 * ch;
        if key.h == 0 || usize::from(key.h) > slot_h {
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
        let out = self.install(i, slot_key, tile.pixels(), tw, usize::from(key.h));
        self.scratch = scratch;
        self.eye_mask_scratch = eye_mask_scratch;
        Some(out)
    }

    /// Look up a resident `BakeKeyV4` tile and stamp it used this frame without spending bake budget on a miss.
    #[inline]
    pub fn peek_v4(&mut self, key: &BakeKeyV4) -> Option<CatTile> {
        if self.cell_h == 0
            || key.w == 0
            || u32::from(key.w) + u32::from(PATCH_STRIP) > u32::from(self.slot_w)
        {
            return None;
        }
        let slot_h = 2 * usize::from(self.cell_h);
        if key.h == 0 || usize::from(key.h) > slot_h {
            return None;
        }
        self.touch(SlotKey::Cat(*key))
    }

    /// The cache hit: find `key`'s slot and stamp it used this frame (the LRU
    /// touch), returning its atlas placement.
    #[inline]
    fn touch(&mut self, key: SlotKey) -> Option<CatTile> {
        let slot_h = 2 * usize::from(self.cell_h);
        let (i, slot) = self
            .slots
            .iter_mut()
            .enumerate()
            .find_map(|(i, s)| s.as_mut().filter(|s| s.key == key).map(|s| (i, s)))?;
        slot.last_used = self.clock;
        Some(CatTile {
            ax: 0,
            ay: (i * slot_h) as u16,
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
        self.slots.iter().position(Option::is_none).or_else(|| {
            self.slots
                .iter()
                .enumerate()
                .filter(|(_, s)| s.as_ref().is_none_or(|s| s.last_used != self.clock))
                .min_by_key(|(idx, s)| (s.as_ref().map_or(0, |s| s.last_used), *idx))
                .map(|(idx, _)| idx)
        })
    }

    fn install(&mut self, i: usize, key: SlotKey, src: &[u8], tw: usize, rows: usize) -> CatTile {
        let slot_h = 2 * usize::from(self.cell_h);
        let atlas_w = usize::from(self.slot_w);
        let y0 = i * slot_h;
        let copy_rows = rows.min(slot_h);
        for row in 0..copy_rows {
            let d = (y0 + row) * atlas_w * 4;
            let s = row * tw * 4;
            self.rgba[d..d + tw * 4].copy_from_slice(&src[s..s + tw * 4]);
            self.rgba[d + tw * 4..d + atlas_w * 4].fill(0);
        }
        if copy_rows < slot_h {
            let tail_start = (y0 + copy_rows) * atlas_w * 4;
            let tail_end = (y0 + slot_h) * atlas_w * 4;
            self.rgba[tail_start..tail_end].fill(0);
        }
        self.slots[i] = Some(Slot {
            key,
            last_used: self.clock,
        });
        self.version = self.version.wrapping_add(1);
        if let Some(stamp) = self.slot_version.get_mut(i) {
            *stamp = self.version;
        }
        self.dirty = true;
        CatTile {
            ax: 0,
            ay: y0 as u16,
        }
    }

    /// Bake a host-authored straight-alpha RGBA tile into the shared atlas, keyed by a stable `host_id` so later calls are cache hits.
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
        Some(self.install(i, slot_key, rgba, usize::from(w), usize::from(h)))
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

    /// Whether this frame still has cold-bake budget left for an initialised atlas.
    #[inline]
    #[must_use]
    pub const fn can_bake(&self) -> bool {
        self.cell_h > 0 && self.bakes_left > 0
    }

    /// Touch a held [`CatBaker::host_tile`] slot at `(ax, ay)` (`last_used = clock`) and confirm it still holds the expected host-authored tile.
    #[inline]
    pub fn touch_held_host_slot(&mut self, host_id: u64, ax: u16, ay: u16) -> bool {
        if self.cell_h == 0 || ax != 0 {
            return false;
        }
        let slot_h = 2 * usize::from(self.cell_h);
        if !usize::from(ay).is_multiple_of(slot_h) {
            return false;
        }
        let i = usize::from(ay) / slot_h;
        let Some(Some(slot)) = self.slots.get_mut(i) else {
            return false;
        };
        if slot.key != SlotKey::Host(host_id) {
            return false;
        }
        slot.last_used = self.clock;
        true
    }
}

/// COAT_RAMP — 16 similarity-ordered stops, black→gray→blue→brown→ginger→ cream→white (§5.3; adjacent indices are near-identical coats).
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

/// Sixteen low-chroma natural iris stops (hazel, moss, storm blue, rose grey), visible without dominating the expression.
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
        let dy2: [f32; 4] = std::array::from_fn(|sy| {
            let fy = y as f32 + (sy as f32 + 0.5) * 0.25;
            let dy = fy - cy;
            dy * dy
        });
        if dy2.iter().all(|&d| d > rr) {
            continue;
        }
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
            let dx2: [f32; 4] = std::array::from_fn(|sx| {
                let fx = x as f32 + (sx as f32 + 0.5) * 0.25;
                let dx = fx - cx;
                dx * dx
            });
            let mut hit = 0u32;
            for &dy2 in &dy2 {
                if dy2 > rr {
                    continue;
                }
                for &dx2 in &dx2 {
                    if dx2 + dy2 <= rr {
                        hit = hit.saturating_add(1);
                    }
                }
            }
            if hit > 0 {
                tile.over(x, y, col, alpha * hit as f32 / 16.0);
            }
        }
    }
}

/// v4 §2.1 pale outline lift for the darkest background-luminance band.
pub const OUTLINE_PALE: u32 = 0x00F4_E6DD;

const EYE_INK_DARK: u32 = 0x0018_1520;
const EYE_INK_PALE: u32 = 0x00FF_F2E8;
const MIN_EYE_CONTRAST: f32 = 3.0;

/// Quantized local terminal palette carried in every cat bake key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CatColorKey {
    /// `0..=11` = 30-degree hue family, `12` = neutral foreground.
    pub accent: u8,
    /// Background WCAG-luminance band, darkest `0` to lightest `3`.
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
        let accent = CONTEXT_ACCENTS[usize::from(colors.accent.min(12))];
        let coat = COAT_RAMP[usize::from(coat).min(COAT_RAMP.len() - 1)];
        let iris = EYE_RAMP[usize::from(iris).min(EYE_RAMP.len() - 1)];
        let outline_base = match colors.background {
            0 => OUTLINE_PALE,
            1 => 0x00FF_F2E8,
            2 => 0x0018_1520,
            3 => 0x0010_0D16,
            4 => EYE_INK_DARK,
            _ => OUTLINE_PALE,
        };
        let coat = rgb(mix_rgb(coat, accent, 0.12));
        let coat = if matches!(colors.background, 0 | 4) {
            lift_coat_luminance(coat, COAT_MIN_LUM_DARK_BG)
        } else {
            coat
        };
        let dark_eye = rgb(mix_rgb(EYE_INK_DARK, accent, 0.06));
        let pale_eye = rgb(mix_rgb(EYE_INK_PALE, accent, 0.06));
        let eye_ink = if contrast_ratio(pale_eye, coat) > contrast_ratio(dark_eye, coat) {
            pale_eye
        } else {
            dark_eye
        };
        Self {
            coat,
            iris: rgb(mix_rgb(iris, accent, 0.28)),
            eye_ink,
            outline: rgb(mix_rgb(outline_base, accent, 0.10)),
            outline_underlay: (colors.background == 4)
                .then(|| rgb(mix_rgb(EYE_INK_PALE, accent, 0.10))),
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
    let lum = |c: (f32, f32, f32)| crate::color_math::relative_luminance_with(table, packed_rgb(c));
    if lum(coat) >= floor {
        return coat;
    }
    let (h, s, _v) = rgb2hsv(packed_rgb(coat));
    let pale = rgb(hsv2rgb(h, s * 0.55, 0.78));
    if lum(pale) <= floor {
        return pale;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..6 {
        let mid = 0.5 * (lo + hi);
        if lum(mix3(coat, pale, mid)) < floor {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    mix3(coat, pale, hi)
}

/// `COAT_RAMP[coat]` lifted to [`COAT_MIN_LUM_DARK_BG`] as on the dark background classes, without the accent nudge (which cancels out of a two-coat comparison).
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
    coat_recolored: bool,
) -> ((f32, f32, f32), f32) {
    let resolved = resolve_layer(layer, fills);
    if coat_recolored
        && layer.role == GlyphRole::Eye
        && layer.recolor == Recolor::Fixed
        && contrast_ratio(resolved.0, fills.coat) < MIN_EYE_CONTRAST
    {
        (fills.eye_ink, resolved.1)
    } else {
        resolved
    }
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

/// Accessory attachment: the normalized head-frame point where the accessory's own centre lands, plus its size relative to the head box (~0.4×).
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

fn closest_eye_path(eyes: &Layer, pcx: f32, pcy: f32) -> Option<(usize, (f32, f32, f32, f32))> {
    eyes.paths
        .iter()
        .enumerate()
        .filter_map(|(index, eye)| path_bounds(eye).map(|bounds| (index, bounds)))
        .min_by(|(_, a), (_, b)| {
            let distance = |bounds: &(f32, f32, f32, f32)| {
                let (x0, y0, x1, y1) = *bounds;
                let (x, y) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                (x - pcx).mul_add(x - pcx, (y - pcy) * (y - pcy))
            };
            distance(a).total_cmp(&distance(b))
        })
}

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
    layers: &[Layer],
    xform: PathTransform,
    mask_scratch: &mut Option<Tile>,
) {
    if xform.scale_x.min(xform.scale_y) < OPEN_EYE_REFLECTION_MIN_PX {
        return;
    }
    let Some(eyes) = layers.iter().find(|layer| layer.role == GlyphRole::Eye) else {
        return;
    };
    let Some(catch_lights) = layers
        .iter()
        .find(|layer| layer.role == GlyphRole::CatchLight)
    else {
        return;
    };
    let mut painted_mask = false;

    for catch_light in catch_lights.paths {
        let Some((cx0, cy0, cx1, cy1)) = path_bounds(catch_light) else {
            continue;
        };
        let (pcx, pcy) = ((cx0 + cx1) * 0.5, (cy0 + cy1) * 0.5);
        let Some((_, eye_bounds)) = closest_eye_path(eyes, pcx, pcy) else {
            continue;
        };
        if !open_eye_reflection_eligible(eye_bounds, xform) {
            continue;
        }
        if !painted_mask {
            let mask = prepare_scratch_tile(mask_scratch, tile.width(), tile.height());
            fill_path_fixed(mask, eyes.paths, (0.0, 0.0, 0.0), 1.0, xform);
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
            xform.dx + pcx * xform.scale_x,
            xform.dy + pcy * xform.scale_y,
            primary_r,
            rgb(CATCH_LIGHT),
            1.0,
        );

        if minor >= OPEN_EYE_ECHO_MIN_PX {
            let echo_r = (minor * OPEN_EYE_ECHO_FRAC)
                .clamp(OPEN_EYE_ECHO_MIN_RADIUS_PX, OPEN_EYE_ECHO_MAX_RADIUS_PX);
            let (ecx, ecy) = ((ex0 + ex1) * 0.5, (ey0 + ey1) * 0.5);
            let echo_x = (pcx + 1.55 * (ecx - pcx))
                .clamp(ex0 + 0.25 * (ex1 - ex0), ex1 - 0.25 * (ex1 - ex0));
            let echo_y = (pcy + 1.55 * (ecy - pcy))
                .clamp(ey0 + 0.25 * (ey1 - ey0), ey1 - 0.25 * (ey1 - ey0));
            paint_dark_masked_disc(
                tile,
                eye_mask,
                xform.dx + echo_x * xform.scale_x,
                xform.dy + echo_y * xform.scale_y,
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
    let def = &GLYPHS[id as usize];
    let coat_recolored = def
        .layers
        .iter()
        .any(|layer| layer.recolor == Recolor::Coat);
    let eye_line = (!matches!(eyes, EyesFrame::Open))
        .then(|| {
            def.layers
                .iter()
                .find(|l| l.role == GlyphRole::Eye)
                .and_then(layer_eye_line)
        })
        .flatten();
    let openness = eyes.openness();
    for layer in def.layers {
        // A shut eye shows no iris or catch-light.
        if matches!(eyes, EyesFrame::Blink)
            && matches!(layer.role, GlyphRole::Iris | GlyphRole::CatchLight)
        {
            continue;
        }
        let lxform = match eye_line {
            Some(yc)
                if matches!(
                    layer.role,
                    GlyphRole::Eye | GlyphRole::Iris | GlyphRole::CatchLight
                ) =>
            {
                PathTransform {
                    scale_y: openness * xform.scale_y,
                    dy: xform.dy + yc * (1.0 - openness) * xform.scale_y,
                    ..xform
                }
            }
            _ => xform,
        };
        let (col, alpha) = resolve_glyph_layer(layer, fills, coat_recolored);
        if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker) {
            paint_outline_halo(tile, layer, fills, alpha, lxform);
        }
        fill_path_fixed(tile, layer.paths, col, alpha, lxform);
    }
    if reflections && matches!(eyes, EyesFrame::Open) {
        paint_open_eye_reflections(tile, def.layers, xform, mask_scratch);
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
    let r = f32::from(PATCH_STRIP) * 0.5;
    tile.disc(w as f32 + r, r, r.max(1.0), rgb(CATCH_LIGHT), 1.0);
}

/// Bake one authored glyph variant to an exact-size RGBA [`Tile`].
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
        let tw = w + u32::from(PATCH_STRIP);
        let tile = prepare_scratch_tile(scratch, tw, h);
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
            served.push(tile.ay);
        }
        let stranger = baker.get_v4(&vk(200));

        assert!(
            stranger.is_none(),
            "every slot was served this frame, so the bake must be REFUSED \
             (callers hold a last-good tile and retry); got {stranger:?}"
        );
        if let Some(t) = stranger {
            assert!(
                !served.contains(&t.ay),
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
        assert!(b.atlas().is_none(), "cleared baker publishes no atlas");
        assert_eq!(b.slots.iter().flatten().count(), 0);
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
        assert!(b.atlas().is_some());
        let v = b.version();
        b.begin_frame(12, 24);
        assert!(b.version() > v, "metric change bumps the version");
        assert_eq!(b.slots.iter().flatten().count(), 0, "metric change clears");
        assert!(b.atlas().is_none());
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
    fn atlas_publishes_on_change_only() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        assert!(b.atlas().is_none(), "no bake → no atlas");
        b.get_v4(&vk(0)).expect("bake");
        let a1 = b.atlas().expect("atlas after bake");
        let a2 = b.atlas().expect("atlas");
        assert!(Arc::ptr_eq(&a1, &a2), "no rebake → same snapshot Arc");
        assert_eq!(a1.version, b.version());
        b.begin_frame(10, 20);
        b.get_v4(&vk(1)).expect("bake 2");
        let a3 = b.atlas().expect("atlas");
        assert!(a3.version > a1.version, "bake bumps the published version");
    }

    #[test]
    fn recycled_publish_is_byte_equal_to_full_clone() {
        let mut fast = CatBaker::default();
        let mut mirror = CatBaker::default();
        let mut retained: Vec<Arc<SceneAtlas>> = Vec::new();
        let mut published = 0usize;
        for f in 0..96u64 {
            let (cw, ch) = if f == 61 { (12u16, 24u16) } else { (10, 20) };
            fast.begin_frame(cw, ch);
            mirror.begin_frame(cw, ch);
            if f % 3 != 2 {
                let key = vk((f % 37) as u8);
                assert_eq!(fast.get_v4(&key).is_some(), mirror.get_v4(&key).is_some());
            }
            if f % 2 == 0 {
                let px = vec![((f * 7) % 251) as u8; 40 * 24 * 4];
                let a = fast.host_tile(f % 13, 40, 24, &px);
                let b = mirror.host_tile(f % 13, 40, 24, &px);
                assert_eq!(a.is_some(), b.is_some());
            }
            match (fast.atlas(), mirror.atlas()) {
                (Some(a), Some(b)) => {
                    published += 1;
                    assert_eq!(
                        (a.width, a.height, a.version),
                        (b.width, b.height, b.version),
                        "frame {f}: published header must be identical"
                    );
                    assert!(
                        a.rgba == b.rgba,
                        "frame {f}: the recycled publish must be BYTE-EQUAL to a full clone"
                    );
                    retained.push(b);
                }
                (a, b) => assert!(a.is_none() && b.is_none(), "frame {f}: publish parity"),
            }
        }
        assert!(
            published > 40,
            "the loop must actually publish ({published})"
        );
    }

    #[test]
    fn dirty_publish_recycles_and_never_mutates_a_held_snapshot() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        b.get_v4(&vk(0)).expect("bake 1");
        let a1 = b.atlas().expect("publish 1");
        let p1 = Arc::as_ptr(&a1);
        drop(a1);
        b.begin_frame(10, 20);
        b.get_v4(&vk(1)).expect("bake 2");
        let held = b.atlas().expect("publish 2");
        let p2 = Arc::as_ptr(&held);
        let held_bytes = held.rgba.clone();
        assert_ne!(p1, p2, "consecutive publishes are distinct snapshots");

        b.begin_frame(10, 20);
        b.get_v4(&vk(2)).expect("bake 3");
        let a3 = b.atlas().expect("publish 3");
        assert_eq!(
            Arc::as_ptr(&a3),
            p1,
            "the third publish must reuse the first buffer instead of cloning \
             the whole atlas"
        );
        assert_eq!(
            a3.version,
            b.version(),
            "a recycled publish carries the live version"
        );
        drop(a3);

        b.begin_frame(10, 20);
        b.get_v4(&vk(3)).expect("bake 4");
        let a4 = b.atlas().expect("publish 4");
        assert_ne!(Arc::as_ptr(&a4), p2, "a held snapshot must never be reused");
        assert_ne!(
            Arc::as_ptr(&a4),
            p1,
            "the live snapshot must never be reused"
        );
        assert_eq!(
            held.rgba, held_bytes,
            "a held snapshot must never be mutated"
        );
        assert_eq!(held.version, 2, "…nor re-stamped");
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
            let atlas_w = usize::from(baker.slot_w);
            for row in 0..usize::from(key.h) {
                let d = (usize::from(placed.ay) + row) * atlas_w * 4;
                let s = row * tw * 4;
                assert_eq!(
                    &baker.rgba[d..d + tw * 4],
                    &direct.pixels()[s..s + tw * 4],
                    "reused scratch bake must match fresh key.bake() texels (coat={coat}, eyes={eyes:?}, w={w}, row={row})"
                );
            }
        }
        let _ = t0;
        baker.clear();
        assert!(baker.scratch.is_none() && baker.eye_mask_scratch.is_none());
    }
}

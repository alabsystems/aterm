// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sparkle-words v2 **CatBaker** (docs/sparkle-words-v2-design.md §5.3–§5.5):
//! bakes full-color RGBA peeking cats at EXACT destination pixel size — the
//! NEAREST-1:1 contract of the `cat_quads` render stream — into one
//! `SceneAtlas`-compatible texture, using `aterm-scene`'s [`Tile`] rasterizer
//! (the same 4×4-supersampled coverage the scene sprites bake with).
//!
//! * **Key** (§5.5): `(art-relevant gkey bits, magic class, pose, eyes_frame,
//!   W, Hart)`. `eyes_frame ∈ {open, closed, twitch}` plus the 3 Fortune wave
//!   frames and the 2 v2.2 Butterfly flutter frames; the **open and twitch
//!   bakes are iris-only** — pupils become live quads in the gaze stage —
//!   with a pupil + catch-light patch baked at this identity's exact pupil
//!   size into the v2.2 [`PATCH_STRIP`] x-strip at `(W, 0)` of the same tile,
//!   in the same pass (§5.8 texel provenance; body quads sample only
//!   `[ax, ax + W)`, so no reveal state can ever show the strip).
//! * **Cache**: LRU by last-used frame over `slots = min(32, ⌊1 MiB /
//!   slot_bytes⌋)` slots with `slot_bytes = (4·ch)·ch·4` — the 1 MiB bound is
//!   structural at ANY cell height, never assumed. ≤ 2 bakes per frame (the
//!   entrance tolerates the delay; a screen of new cats fills over a few
//!   frames at near-zero reveal height).
//! * **Invalidation**: wholesale clear + version bump on cell-metric change
//!   (detected in [`CatBaker::begin_frame`]) and on config reload / toggle
//!   (`WordDecorations::reset` → [`CatBaker::clear`]).
//!
//! Art is the sparkle-words **v3 kawaii style contract** (v3 design §2.1) over
//! the v2 mechanics (§2.2): bold warm-near-black outlines (`max(2, ch/9)` px,
//! 1 px snapped at small cells; interiors 0.7×) replacing the v2 rim-light
//! system, a wider rounded face with rounded-tri ears + tip discs, big round
//! PUPIL **button eyes** with a dark-coat rescue (forced iris ring + belly
//! rim), the gaze catch-light living in the patch strip (a white dot the gaze
//! stage drifts — open bakes keep only a static 1 px sparkle), rose nose + ω
//! mouth + outline-color whiskers + blush, the 8 pattern classes redrawn
//! inside the outline + the longhair ruff (gkey bits 55–56 == 3), the 3-bit
//! expression field (gkey bits 52–54: Normal / HappyClosed / Wink / Grumpy),
//! and the rare accessories (Bow / Sunglasses / WitchHat / Crown — magic-word
//! window, plain cats only). Fortune / Nebula / Butterfly / Sakura keep their
//! v2 recipes under the new outline system.

use std::sync::Arc;

use aterm_render::SceneAtlas;
use aterm_scene::vector::{FIXED_ONE, PathSeg};
use aterm_scene::{PathTransform, Tile, fill_path_fixed, mix_rgb};

use crate::cat_glyphs_gen::{CatGlyphId, GLYPHS, GlyphRole, Layer, Recolor};
use crate::color_math::{hsv2rgb, relative_luminance, rgb2hsv};

/// Structural slot ceiling (§5.5): the LRU never exceeds 32 tiles.
pub const MAX_SLOTS: usize = 32;
/// Structural memory ceiling for the whole atlas (§5.5): 2 MiB at any cell
/// size. v2.9 (2-band head): the tile grew from `ch` to `2·ch` rows tall (the
/// head now spans rows r−2, r−1 and the chin slice in r), doubling
/// `slot_bytes`; the bound is raised 1 → 2 MiB so the slot count stays 32
/// across the whole working range (ch ≤ 40) and degrades gracefully above it.
pub const MAX_ATLAS_BYTES: usize = 2 << 20;
/// Bake-rate cap (§5.5): at most 2 tiles baked per presented frame.
pub const MAX_BAKES_PER_FRAME: u32 = 2;

/// v2.2 pupil-patch x-strip (§5.5): every tile is baked `W + PATCH_STRIP`
/// texels wide — the art occupies x ∈ [0, W) and the reserved gaze catch-light
/// dot is baked at `(W, 0)`, horizontally OUTSIDE every sampled quad (quads
/// sample only `[ax, ax + W)`), so no reveal state can ever expose it.
pub const PATCH_STRIP: u16 = 4;

/// A resolved cache hit: where the tile lives in the baker's atlas.
#[derive(Clone, Copy, Debug)]
pub struct CatTile {
    /// Tile origin X in atlas texels (the design box spans `ax..ax+key.w`).
    pub ax: u16,
    /// Tile origin Y in atlas texels (the design box spans `ay..ay+h`).
    pub ay: u16,
}

/// What a [`CatBaker`] atlas slot holds — the cat-art v4 authored-glyph key
/// ([`BakeKeyV4`]).
struct Slot {
    key: BakeKeyV4,
    last_used: u64,
    /// `Some(id)` for a HOST-authored tile ([`CatBaker::host_tile`], e.g. the
    /// kitty cursor) rather than a generated cat: keyed by `id`, not by `key`, so
    /// the two key spaces never alias in the shared atlas.
    host: Option<u64>,
}

/// Host-side LRU bake cache + the published `Arc<SceneAtlas>` (§5.5).
#[derive(Default)]
pub struct CatBaker {
    cell_w: u16,
    cell_h: u16,
    /// Free-overlay Phase 3 (FREE_OVERLAY_LAYER_DESIGN §7 / v3 §5): publish
    /// EXACT-SIZE tiles — `(W + PATCH_STRIP) × Hart` with the art at tile top
    /// (row 0) — instead of the legacy v2.9 `2·ch`-tall band tile whose art
    /// occupies the bottom `Hart` rows. The rasterization itself stays in the
    /// one proven `2·ch` frame; the exact-size tile is CUT from it at blit
    /// time (art rows shifted up by `2·ch − Hart`, the patch x-strip kept at
    /// its `(W, 0)` anchor), so the stored texels are byte-identical to the
    /// legacy bake's revealed rows by construction. The slot band stays
    /// `2·ch` tall (`Hart = round(1.7·ch) ≤ 2·ch`), so the atlas layout, slot
    /// count, and memory bound are unchanged. Flipped via
    /// [`CatBaker::set_free_tiles`], which wholesale-clears on change (the
    /// two shapes must never alias in one atlas).
    free_tiles: bool,
    /// Slot (and atlas) width: `4·ch` — the §5.2 `W ≤ 4·ch` cap made structural.
    slot_w: u16,
    /// `min(32, ⌊1 MiB / slot_bytes⌋)` occupied-or-free slots, single column.
    slots: Vec<Option<Slot>>,
    /// Master straight-alpha RGBA8 texels (`slot_w × slots.len()·ch`), resident.
    rgba: Vec<u8>,
    /// Monotonic; bumped on every bake and every wholesale clear (a rebake must
    /// repaint AND re-upload).
    version: u64,
    /// The last published snapshot; rebuilt only when `dirty` (≤ 2 bakes/frame
    /// transient — zero steady-state allocation).
    published: Option<Arc<SceneAtlas>>,
    dirty: bool,
    /// LRU clock, advanced once per tick by [`CatBaker::begin_frame`].
    clock: u64,
    bakes_left: u32,
}

impl CatBaker {
    /// The §5.5 structural slot count for a cell height: `min(32, ⌊2 MiB /
    /// ((4·ch)·(2·ch)·4)⌋)`, floored at 1 so a pathological cell size still
    /// bakes. v2.9: the slot is now `2·ch` rows tall (the 2-band head), so
    /// `slot_bytes = (4·ch)·(2·ch)·4 = 32·ch²`; at ch = 40 that is 51.2 KB and
    /// 2 MiB still holds 32 slots (§5.5).
    pub fn slot_count(cell_h: u16) -> usize {
        let ch = usize::from(cell_h.max(1));
        let slot_bytes = (4 * ch) * (2 * ch) * 4;
        (MAX_ATLAS_BYTES / slot_bytes).clamp(1, MAX_SLOTS)
    }

    /// Per-tick prologue: advance the LRU clock, reset the bake budget, and on
    /// a cell-metric change wholesale-clear + version-bump (§5.5 invalidation).
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
            self.rgba.clear();
            // v2.9 (2-band head): each slot is `2·ch` rows tall.
            self.rgba.resize(
                usize::from(self.slot_w) * slots * 2 * usize::from(cell_h) * 4,
                0,
            );
        }
    }

    /// Select the free-overlay EXACT-SIZE tile shape (see
    /// [`CatBaker::free_tiles`]) vs the legacy `2·ch` band tile. Wholesale
    /// clear + version bump on an actual flip — cached tiles of the other
    /// shape would be sampled at the wrong y origin. Idempotent per frame
    /// (the emitter calls it every tick with the config flag).
    pub fn set_free_tiles(&mut self, free: bool) {
        if self.free_tiles != free {
            self.clear();
            self.free_tiles = free;
        }
    }

    /// Whether the baker is in the free-overlay exact-size tile mode.
    pub fn free_tiles(&self) -> bool {
        self.free_tiles
    }

    /// Wholesale clear + version bump (config reload / toggle / metric change).
    /// A no-op when already empty, so the per-frame master-off `reset()` path
    /// costs nothing.
    pub fn clear(&mut self) {
        if self.slots.iter().all(Option::is_none) && self.published.is_none() {
            return;
        }
        for s in &mut self.slots {
            *s = None;
        }
        self.rgba.fill(0);
        self.version = self.version.wrapping_add(1);
        self.published = None;
        self.dirty = false;
    }

    /// Monotonic atlas version (folded into the frame fingerprint — a rebake
    /// must repaint).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The LRU / bake-budget frame clock, advanced once per
    /// [`begin_frame`](CatBaker::begin_frame). Diagnostic: it is how a host
    /// that ticks several panes into ONE presented frame proves the two-bake
    /// budget stayed per-FRAME instead of multiplying with the pane count.
    #[doc(hidden)]
    pub fn frame_clock(&self) -> u64 {
        self.clock
    }

    /// The published atlas snapshot, rebuilt lazily after any bake. `None`
    /// until the first bake (and after [`CatBaker::clear`]) — cat-free frames
    /// carry no atlas (byte-identical off).
    pub fn atlas(&mut self) -> Option<Arc<SceneAtlas>> {
        if self.dirty {
            self.published = Some(Arc::new(SceneAtlas {
                width: u32::from(self.slot_w),
                // v2.9 (2-band head): each slot band is `2·ch` rows tall.
                height: (self.slots.len() * 2 * usize::from(self.cell_h)) as u32,
                rgba: self.rgba.clone(),
                version: self.version,
            }));
            self.dirty = false;
        }
        self.published.clone()
    }

    /// cat-art v4 (design §2): look up a [`BakeKeyV4`] authored-glyph tile,
    /// baking on a miss when the per-frame budget allows. The generated tile is
    /// baked exact-size (`w + PATCH_STRIP` wide, `h` tall, art at row 0 via
    /// [`bake_variant_with`]) and blitted at the slot origin so the free-overlay
    /// sprite path samples it with the art top at atlas row `i·2·ch`. The authored
    /// glyph bakes its own eyes + catch-light, so there is no live pupil patch.
    /// `None` = budget exhausted / degenerate metrics — the caller retries next
    /// frame (§5.5).
    pub fn get_v4(&mut self, key: &BakeKeyV4) -> Option<CatTile> {
        // The whole baked tile is `key.w + PATCH_STRIP` wide and must fit the slot.
        if self.cell_h == 0
            || key.w == 0
            || u32::from(key.w) + u32::from(PATCH_STRIP) > u32::from(self.slot_w)
        {
            return None;
        }
        let ch = usize::from(self.cell_h);
        // v2.9 (2-band head): the slot band is `2·ch` rows tall.
        let slot_h = 2 * ch;
        // Degenerate caller-supplied art height (the tile stores `h` rows).
        if key.h == 0 || usize::from(key.h) > slot_h {
            return None;
        }
        if let Some(i) = self.slots.iter().position(|s| {
            s.as_ref()
                .is_some_and(|s| s.host.is_none() && s.key == *key)
        }) {
            let slot = self.slots[i].as_mut().expect("position() found it");
            slot.last_used = self.clock;
            return Some(CatTile {
                ax: 0,
                ay: (i * slot_h) as u16,
            });
        }
        if self.bakes_left == 0 {
            return None;
        }
        self.bakes_left -= 1;
        // Free slot first; else evict the least-recently-used (lowest index
        // tiebreak — deterministic).
        let i = self.slots.iter().position(Option::is_none).or_else(|| {
            self.slots
                .iter()
                .enumerate()
                .min_by_key(|(idx, s)| (s.as_ref().map_or(0, |s| s.last_used), *idx))
                .map(|(idx, _)| idx)
        })?;
        // Bake the authored tile exact-size (art at row 0, catch-light in the strip).
        let tile = key.bake();
        let tw = tile.width() as usize; // == key.w + PATCH_STRIP
        let atlas_w = usize::from(self.slot_w);
        let y0 = i * slot_h;
        let stored_rows = usize::from(key.h);
        for row in 0..slot_h {
            let d = ((y0 + row) * atlas_w) * 4;
            self.rgba[d..d + atlas_w * 4].fill(0);
            if row < stored_rows {
                let s = row * tw * 4;
                self.rgba[d..d + tw * 4].copy_from_slice(&tile.pixels()[s..s + tw * 4]);
            }
        }
        self.slots[i] = Some(Slot {
            key: *key,
            last_used: self.clock,
            host: None,
        });
        self.version = self.version.wrapping_add(1);
        self.dirty = true;
        Some(CatTile {
            ax: 0,
            ay: y0 as u16,
        })
    }

    /// Bake a HOST-AUTHORED straight-alpha RGBA tile into a slot of the SAME
    /// atlas the cats share, keyed by a stable `host_id` so it bakes once and is
    /// then a pure cache hit. `rgba` is `w·h·4` bytes, row-major; the tile must
    /// fit a slot (`w ≤ slot_w`, `h ≤ 2·ch`). Returns the atlas placement
    /// (`ax = 0`, `ay = slot·2·ch`), or `None` if the baker is uninitialised,
    /// the tile doesn't fit, or the ≤2-bakes/frame budget is spent (retry next
    /// frame). Metric changes clear it exactly like a cat tile, so a caller that
    /// re-paints `rgba` at the new cell scale gets a fresh bake automatically.
    /// Used by the kitty cursor ([`crate::word_decorations::WordDecorations::kitty_cursor`]),
    /// whose host-provided sprite ([`crate::kitty_cursor`]) is not a generated glyph.
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
        if let Some(i) = self
            .slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.host == Some(host_id)))
        {
            let slot = self.slots[i].as_mut().expect("position() found it");
            slot.last_used = self.clock;
            return Some(CatTile {
                ax: 0,
                ay: (i * slot_h) as u16,
            });
        }
        if self.bakes_left == 0 {
            return None;
        }
        self.bakes_left -= 1;
        let i = self.slots.iter().position(Option::is_none).or_else(|| {
            self.slots
                .iter()
                .enumerate()
                .min_by_key(|(idx, s)| (s.as_ref().map_or(0, |s| s.last_used), *idx))
                .map(|(idx, _)| idx)
        })?;
        let atlas_w = usize::from(self.slot_w);
        let y0 = i * slot_h;
        let tw = usize::from(w);
        let th = usize::from(h);
        for row in 0..slot_h {
            let d = (y0 + row) * atlas_w * 4;
            self.rgba[d..d + atlas_w * 4].fill(0);
            if row < th {
                let s = row * tw * 4;
                self.rgba[d..d + tw * 4].copy_from_slice(&rgba[s..s + tw * 4]);
            }
        }
        // Host slots carry a placeholder key (never matched — the lookup guards
        // on `host.is_none()`); `host: Some(id)` is the real identity.
        self.slots[i] = Some(Slot {
            key: BakeKeyV4 {
                variant: CatGlyphId::S100,
                accessory: None,
                coat: 0,
                iris: 0,
                colors: CatColorKey::default(),
                w: 0,
                h: 0,
                eyes: EyesFrame::Open,
            },
            last_used: self.clock,
            host: Some(host_id),
        });
        self.version = self.version.wrapping_add(1);
        self.dirty = true;
        Some(CatTile {
            ax: 0,
            ay: y0 as u16,
        })
    }
}

// ────────────────────────────── §5.3 palettes ──────────────────────────────

/// COAT_RAMP — 16 similarity-ordered stops, black→gray→blue→brown→ginger→
/// cream→white (§5.3; adjacent indices are near-identical coats).
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

/// EYE_RAMP — 16 low-chroma natural stops. The original saturated gold ramp
/// made large terminal eyes read as glowing or predatory; these hazel, moss,
/// storm-blue, and rose-grey tones keep the iris visible without becoming the
/// entire expression (§5.3; `eyes` samples 8 positions `ord/7` along it).
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

fn rgb(hex: u32) -> (f32, f32, f32) {
    (
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    )
}

// ═══════════════════════════ cat-art v4 bake path ═══════════════════════════
//
// Additive to the procedural baker above (docs/cat-art-v4-design.md §2): semantic
// reference glyphs (`cat_glyphs_gen::GLYPHS`) rasterized by the `aterm-scene` vector
// filler, with genome-driven fills, a pale outline lift on dark backgrounds, and no
// paws. Built ALONGSIDE the procedural path — the old baker/genome/word_decorations
// stay live until the Cleanup phase.

/// v4 §2.1 pale outline lift for the darkest background-luminance band.
pub const OUTLINE_PALE: u32 = 0x00F4_E6DD;

// Eye ink is selected once per resolved fill set. The authored ink remains the
// default; these two restrained anchors are only used when it would disappear
// into a recolored coat.
const EYE_INK_DARK: u32 = 0x0018_1520;
const EYE_INK_PALE: u32 = 0x00FF_F2E8;
const MIN_EYE_CONTRAST: f32 = 3.0;

/// Quantized local terminal palette carried in every cat bake key. Twelve hue
/// families plus neutral, crossed with four uniform background-luminance bands
/// and one dark+light mixed class, bound the context surface to 65 possibilities
/// instead of fragmenting the atlas on arbitrary 24-bit cell colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CatColorKey {
    /// `0..=11` = 30-degree hue family, `12` = neutral foreground.
    pub accent: u8,
    /// Background WCAG-luminance band, darkest `0` through lightest `3`; `4`
    /// means the sampled footprint spans both sides of the crossover and uses
    /// the dual pale-underlay/dark-keyline treatment.
    pub background: u8,
}

const BG_BAND_DARKEST_MAX: f32 = 0.04;
const BG_BAND_DARK_MAX: f32 = 0.18;
const BG_BAND_MID_MAX: f32 = 0.58;

impl Default for CatColorKey {
    fn default() -> Self {
        Self {
            accent: 12,
            background: 0,
        }
    }
}

impl CatColorKey {
    /// Resolve matched-word, neighboring-text, and local-background colors on
    /// the cold rescan path. Only this two-byte key reaches the bake cache.
    #[must_use]
    pub fn from_rgb(background: u32, foreground: u32, surrounding: u32) -> Self {
        let harmonized = mix_rgb(foreground, surrounding, 0.35);
        let (h, s, _) = rgb2hsv(harmonized);
        let accent = if s < 0.12 {
            12
        } else {
            // Round to the nearest 30-degree family. Adding half a bucket
            // before the modulo also closes the red seam: 359° and 1° share
            // family 0 instead of fragmenting into opposite ends of the key.
            (((h + 15.0) / 30.0).floor() as u8) % 12
        };
        let background = background_band(background);
        Self { accent, background }
    }

    /// Resolve the same bounded key while retaining one bit of footprint
    /// structure lost by RGB averaging. A span that crosses the dark/light
    /// outline boundary uses the mixed class; uniform spans remain byte-exact
    /// with [`Self::from_rgb`].
    #[must_use]
    pub fn from_rgb_span(
        background: u32,
        foreground: u32,
        surrounding: u32,
        min_background_band: u8,
        max_background_band: u8,
    ) -> Self {
        let mut key = Self::from_rgb(background, foreground, surrounding);
        // The mixed (dual pale-underlay/dark-keyline) treatment applies when
        // the averaged background has left the darkest band, OR when the span
        // contains a genuinely BRIGHT (band 3) region — the callers average
        // gamma-space bytes, so a black footprint can hide a sizeable white
        // strip inside a "band 0" average, and a pale outline would vanish
        // over it (the dual treatment carries ≥3:1 on both halves). What a
        // decisively dark average DOES veto is the mixed class for mere
        // mid-luminance clips: freezing the dark-keyline mix for a whole
        // flight over a dark terminal drew the cat as an outline-less dark
        // blob (the "kitty looks too dark" report).
        if (key.background >= 1 || max_background_band.min(3) >= 3)
            && min_background_band.min(3) <= 1
            && max_background_band.min(3) >= 2
        {
            key.background = 4;
        }
        key
    }

    #[must_use]
    pub fn background_band(background: u32) -> u8 {
        background_band(background)
    }

    #[must_use]
    pub fn dark(self) -> bool {
        self.background <= 1
    }
}

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

/// Restrained anchors at exact 30-degree hue steps. They tint, rather than
/// replace, the genome coat; the cat keeps its collectible identity while
/// belonging to the surrounding terminal palette.
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

/// v4 §2.1 resolved genome fills for one bake: coat, iris, adaptive eye ink,
/// and a background-contrast outline. Floats are bake-time only;
/// [`BakeKeyV4`] carries only integer ramp and context indices.
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
    /// Pale outer keyline painted beneath `outline` for a mixed-background
    /// footprint. `None` on a uniform luminance band.
    pub outline_underlay: Option<(f32, f32, f32)>,
}

impl ResolvedFills {
    /// Resolve genome ramp indices into concrete colours (the cache key → fills
    /// step). `coat`/`iris` are clamped into their ramps.
    #[must_use]
    pub fn from_indices(coat: u8, iris: u8, dark_bg: bool) -> Self {
        let colors = CatColorKey {
            accent: 12,
            background: if dark_bg { 0 } else { 3 },
        };
        Self::from_context(coat, iris, colors)
    }

    /// Resolve genome fills through a bounded local terminal palette. Coat is
    /// nudged only 12%, preserving identity; iris gets a stronger 28% echo of
    /// nearby text; outline uses a warm high-contrast base plus a quiet tint.
    #[must_use]
    pub fn from_context(coat: u8, iris: u8, colors: CatColorKey) -> Self {
        let accent = CONTEXT_ACCENTS[usize::from(colors.accent.min(12))];
        let coat = COAT_RAMP[usize::from(coat).min(COAT_RAMP.len() - 1)];
        let iris = EYE_RAMP[usize::from(iris).min(EYE_RAMP.len() - 1)];
        // The light/dark crossover is deliberately at WCAG luminance 0.18.
        // With the 10% accent tint below, every outline clears 3:1 against
        // the worst endpoint of its entire quantization band (pinned in tests).
        let outline_base = match colors.background {
            0 => OUTLINE_PALE,
            1 => 0x00FF_F2E8,
            2 => 0x0018_1520,
            3 => 0x0010_0D16,
            4 => EYE_INK_DARK,
            _ => OUTLINE_PALE,
        };
        // DARK-COAT RESCUE (the body's version of the §2.1 outline lift): on
        // the DARKEST band a charcoal/black coat is nearly invisible — the
        // pale outline draws a ring around a void (the "kitty looks too dark"
        // report). Lift such coats toward a pale same-hue tone until the body
        // clears the band's backgrounds. Bounded (≤6 bisection steps at bake
        // time, the §6.5 luminance-bisection idiom) and identity-preserving:
        // hue is untouched and coats already at/above the floor are bit-exact
        // — the lift engages only below it. Band 0 and the mixed class only:
        // band 1's own backgrounds SPAN the floor (0.04..0.18), so lifting
        // there would converge the coat onto mid-dark themes — on band 1 a
        // dark coat already reads as a silhouette against the brighter ground.
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

fn packed_rgb((r, g, b): (f32, f32, f32)) -> u32 {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

/// Minimum WCAG relative luminance for a coat BODY over the dark background
/// bands (0/1/mixed): high enough that the darkest ramp stops read as a dark
/// grey cat instead of a void (~2.5:1 over a black terminal), low enough that
/// the collectible still reads as a dark coat. Coats already at/above it are
/// untouched.
const COAT_MIN_LUM_DARK_BG: f32 = 0.10;

/// Component-wise linear mix of two float colours, `t` from a → b.
fn mix3(a: (f32, f32, f32), b: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
    (
        a.0 + (b.0 - a.0) * t,
        a.1 + (b.1 - a.1) * t,
        a.2 + (b.2 - a.2) * t,
    )
}

/// Lift `coat` toward a pale same-hue tone until its relative luminance
/// reaches `floor`. Identity-preserving no-op when the coat is already
/// at/above the floor; otherwise ≤6 bisection steps at bake time (luminance
/// is monotone along the mix but gamma-curved, so closed-form t would drift).
fn lift_coat_luminance(coat: (f32, f32, f32), floor: f32) -> (f32, f32, f32) {
    let lum = |c: (f32, f32, f32)| relative_luminance(packed_rgb(c));
    if lum(coat) >= floor {
        return coat;
    }
    // The pale target keeps the coat's hue family: value raised, saturation
    // softened — "the same cat, catching the light".
    let (h, s, _v) = rgb2hsv(packed_rgb(coat));
    let pale = rgb(hsv2rgb(h, s * 0.55, 0.78));
    if lum(pale) <= floor {
        return pale; // degenerate authored coat; the pale tone IS the rescue
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

fn contrast_ratio(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let a = relative_luminance(packed_rgb(a));
    let b = relative_luminance(packed_rgb(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// v4 §2.1 fill resolution for one glyph layer → its baked `(rgb, alpha)`:
/// `Recolor::Coat`→the coat colourway, `Recolor::Iris`→the iris stop,
/// `Recolor::Fixed`→the source `layer.fill` (unpacked `0x00RR_GGBB`). `Outline` and
/// exterior `Whisker` roles use the context-resolved contrast ink, keyed off the
/// painter role so it wins over the recolor arm. Layers bake opaque (alpha 1.0);
/// authored coverage carries the shape.
///
/// `coat_shade` is forward-compat (no current asset triggers it): when a `CoatShade`
/// recolor ever appears in the roster it resolves to `mix(coat, #000, 0.28)`.
#[must_use]
pub fn resolve_layer(layer: &Layer, fills: &ResolvedFills) -> ((f32, f32, f32), f32) {
    // Whiskers extend beyond the coat onto the terminal background, so they
    // need the same context contrast guarantee as the outer silhouette.
    // Internal face ink stays fixed; lifting eyes/mouth with the outline would
    // erase expressions on pale coats.
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

/// v4 §2 overlay-accessory attachment: the normalized head-frame point where the
/// accessory glyph's OWN centre `(0.5, 0.5)` lands, plus its size relative to the
/// head box (~0.4×). Bow → left-ear base, crown → dome crest, bell → collar/chin
/// (the resolved-blocker overlay set). A non-accessory id centres head-sized
/// (defensive — callers pass the three `Acc*` ids).
#[must_use]
pub fn accessory_attach(acc: CatGlyphId) -> (f32, f32, f32) {
    match acc {
        CatGlyphId::AccBow => (0.30, 0.22, 0.40),
        CatGlyphId::AccCrown => (0.50, 0.10, 0.46),
        CatGlyphId::AccBell => (0.50, 0.87, 0.26),
        _ => (0.50, 0.50, 0.40),
    }
}

/// Fit an accessory into its attachment box without changing the authored
/// viewbox aspect. The attachment scale remains the maximum box fraction; a
/// wide bow becomes shorter and a tall bell becomes narrower instead of being
/// stretched to the head's proportions.
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

/// v4 §2 blink/expression bake axis: the eye state baked into a cursor-cat tile.
/// A pure frame-selection input (not per-pixel work) — the animating cursor cat
/// ([`crate::kitty_cursor::CursorCat`]) blinks and squints by selecting a distinct
/// `eyes` value, which bakes a distinct (but cache-cheap) tile. `Open` is the
/// authored art verbatim, so every existing bake and word-cat stays byte-exact.
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
    /// Vertical openness the eye layers keep: `1.0` = as authored, small = shut.
    /// The eye family (Eye/Iris/CatchLight) is squashed toward its shared eye
    /// line by this factor, so a round eye becomes a lidded slit or a closed line.
    fn openness(self) -> f32 {
        match self {
            EyesFrame::Open => 1.0,
            EyesFrame::Happy => 0.5,
            EyesFrame::Blink => 0.14,
        }
    }
}

/// The eye line (mid-y in the glyph's 0..1 frame) of an `Eye`-role layer — the
/// midpoint of every path coordinate's y. Blink/squint squashes the eye family
/// toward this shared line, so the closure reads at the exact authored height for
/// ANY head or coat. `None` for an empty layer.
fn layer_eye_line(layer: &Layer) -> Option<f32> {
    let mut lo = u16::MAX;
    let mut hi = 0u16;
    let mut seen = false;
    let mut take = |y: u16| {
        lo = lo.min(y);
        hi = hi.max(y);
        seen = true;
    };
    for path in layer.paths {
        for seg in *path {
            match *seg {
                PathSeg::Move(_, y) | PathSeg::Line(_, y) => take(y),
                PathSeg::Cubic(_, y1, _, y2, _, y) => {
                    take(y1);
                    take(y2);
                    take(y);
                }
                PathSeg::Close => {}
            }
        }
    }
    seen.then(|| (f32::from(lo) + f32::from(hi)) * 0.5 / f32::from(FIXED_ONE))
}

/// Fill every layer of `GLYPHS[id]` in painter order through `xform`, resolving each
/// layer's colour with [`resolve_layer`]. `eyes` selects a blink/squint frame: the
/// eye family is squashed toward the shared eye line, and a full blink drops the
/// iris + catch-light (a shut eye shows neither).
fn paint_glyph(
    tile: &mut Tile,
    id: CatGlyphId,
    fills: &ResolvedFills,
    xform: PathTransform,
    eyes: EyesFrame,
) {
    let def = &GLYPHS[id as usize];
    let coat_recolored = def
        .layers
        .iter()
        .any(|layer| layer.recolor == Recolor::Coat);
    // The shared eye line, resolved once from the authored Eye layer so the iris
    // and catch-light stay nested with the lid as it closes.
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
        // Squash the eye family vertically toward the shared eye line (fixing the
        // line, compressing everything above/below it): scale_y·k, with dy lifted
        // so the line stays put. Only the eye family moves; the silhouette holds.
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
        if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker)
            && let Some(underlay) = fills.outline_underlay
        {
            // A mixed terminal footprint cannot be served by one solid ink at
            // every luminance. Paint a compact pale halo beneath the dark
            // keyline: one of the two has >=3:1 contrast at every WCAG
            // luminance. This runs only on a bounded atlas cache miss (at most
            // two bakes per frame), never in the steady-state render loop.
            let halo = (lxform.scale_x.min(lxform.scale_y) * 0.04).clamp(0.75, 1.25);
            for (dx, dy) in [(halo, 0.0), (-halo, 0.0), (0.0, halo), (0.0, -halo)] {
                fill_path_fixed(
                    tile,
                    layer.paths,
                    underlay,
                    alpha,
                    PathTransform {
                        dx: lxform.dx + dx,
                        dy: lxform.dy + dy,
                        ..lxform
                    },
                );
            }
        }
        fill_path_fixed(tile, layer.paths, col, alpha, lxform);
    }
}

/// v4 §2: bake one authored glyph variant to an exact-size RGBA [`Tile`]. The glyph's
/// `0..1` frame fills a `w × h` art box at the tile origin; the tile is baked
/// `w + PATCH_STRIP` wide with a white gaze catch-light dot in the reserved strip at
/// `(w, 0)` — the §2 patch-strip contract preserved from the procedural baker (the
/// live gaze quads sample only `[0, w)`, so no reveal state exposes the strip).
/// Painter order, per-layer fills via [`resolve_layer`]. Deterministic: the const
/// integer drawlist + the fixed filler give byte-identical tiles for one key.
#[must_use]
pub fn bake_variant(id: CatGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    bake_variant_with(id, None, fills, w, h, EyesFrame::Open)
}

/// [`bake_variant`] with an optional overlay accessory glyph composited over the head
/// at its [`accessory_attach`] anchor (bow left-ear, crown crest, bell collar), scaled
/// ~0.4× the head box. `accessory = None` bakes the bare variant.
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
    if w == 0 || h == 0 {
        return tile;
    }
    // Base variant: its 0..1 frame fills the w×h art box at the tile origin.
    paint_glyph(&mut tile, id, fills, PathTransform::fit(w, h), eyes);
    // Overlay accessory: map its own frame into a scaled box centred on the attach
    // point (its centre 0.5,0.5 lands on (cx·w, cy·h)) while preserving the
    // accessory's authored viewbox aspect. An accessory carries no eye family, so
    // it always bakes open.
    if let Some(acc) = accessory {
        paint_glyph(
            &mut tile,
            acc,
            fills,
            accessory_transform(acc, w, h),
            EyesFrame::Open,
        );
    }
    // §2 gaze catch-light: a small white dot baked into the reserved PATCH_STRIP at
    // (w, 0), horizontally outside the [0, w) art the body quads sample.
    let r = f32::from(PATCH_STRIP) * 0.5;
    tile.disc(w as f32 + r, r, r.max(1.0), rgb(CATCH_LIGHT), 1.0);
    tile
}

/// v4 §2 bake-cache key: everything that changes the baked texels, all integer so it
/// is `Eq`/`Hash`-safe (the [`BakeKey`] discipline). The coat/iris ramp INDICES (not
/// the resolved floats) plus the local color key drive the fills. Exact `w × h`
/// dimensions already encode any caller-side age scaling, so age is deliberately
/// not a second cache axis. An accessory overlay is a DISTINCT key — a bow cat
/// never aliases its bare twin.
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
    /// Blink/squint frame ([`EyesFrame::Open`] = authored art). A distinct value
    /// is a distinct (cache-cheap) tile — the cursor cat's live blink.
    pub eyes: EyesFrame,
}

impl BakeKeyV4 {
    /// Resolve this key's ramp indices + local palette into bake fills.
    #[must_use]
    pub fn fills(&self) -> ResolvedFills {
        ResolvedFills::from_context(self.coat, self.iris, self.colors)
    }

    /// Bake the tile this key describes (the cache-miss path).
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
}

struct SlotV4 {
    key: BakeKeyV4,
    tile: Tile,
    last_used: u64,
}

/// v4 §2 host-side LRU bake cache: a compact parallel to [`CatBaker`] keyed on
/// [`BakeKeyV4`], honouring the same structural budgets ([`MAX_SLOTS`] slot ceiling,
/// [`MAX_BAKES_PER_FRAME`] bake-rate cap). Holds exact-size authored tiles (each
/// `w + PATCH_STRIP` wide); the wire phase reads them into the scene atlas. The
/// [`MAX_ATLAS_BYTES`] bound is enforced by the procedural baker's slot sizing; here
/// the slot COUNT ceiling caps residency.
#[derive(Default)]
pub struct CatBakerV4 {
    slots: Vec<SlotV4>,
    clock: u64,
    bakes_left: u32,
    version: u64,
}

impl CatBakerV4 {
    /// Per-tick prologue: advance the LRU clock and reset the per-frame bake budget.
    pub fn begin_frame(&mut self) {
        self.clock = self.clock.wrapping_add(1);
        self.bakes_left = MAX_BAKES_PER_FRAME;
    }

    /// Monotonic version, bumped on every bake (folded into a frame fingerprint — a
    /// rebake must re-upload).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Number of resident tiles (`≤ MAX_SLOTS`).
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the cache holds no tiles.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Look `key` up, baking on a miss when the per-frame budget allows. `None` =
    /// budget exhausted this frame (retry next frame — the entrance tolerates the
    /// delay, §5.5). The returned tile is `key.w + PATCH_STRIP` wide, art in `[0, w)`.
    pub fn get(&mut self, key: &BakeKeyV4) -> Option<&Tile> {
        if let Some(i) = self.slots.iter().position(|s| s.key == *key) {
            self.slots[i].last_used = self.clock;
            return Some(&self.slots[i].tile);
        }
        if self.bakes_left == 0 {
            return None;
        }
        self.bakes_left -= 1;
        let tile = key.bake();
        self.version = self.version.wrapping_add(1);
        let i = if self.slots.len() < MAX_SLOTS {
            self.slots.push(SlotV4 {
                key: *key,
                tile,
                last_used: self.clock,
            });
            self.slots.len() - 1
        } else {
            // Evict the least-recently-used (lowest index tiebreak — deterministic).
            let evict = self
                .slots
                .iter()
                .enumerate()
                .min_by_key(|(idx, s)| (s.last_used, *idx))
                .map(|(idx, _)| idx)?;
            self.slots[evict] = SlotV4 {
                key: *key,
                tile,
                last_used: self.clock,
            };
            evict
        };
        Some(&self.slots[i].tile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ───────────── CatBaker LRU / atlas structural tests (v4 keys) ─────────────

    /// A distinct v4 key per `coat` index at a working art size (ch=20 slot:
    /// 80×40, so w=40/h=24 fits with the patch strip).
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

    /// The §5.5 structural slot count is a pure function of cell height:
    /// `slots = min(32, ⌊2 MiB / ((4·ch)·(2·ch)·4)⌋)`.
    #[test]
    fn slot_count_is_structural() {
        assert_eq!(
            CatBaker::slot_count(14),
            32,
            "small cells hit the 32 ceiling"
        );
        assert_eq!(CatBaker::slot_count(20), 32);
        assert_eq!(
            CatBaker::slot_count(40),
            32,
            "51.2 KB/slot still fits 32 at 2 MiB (§5.5, 1.6 MiB)"
        );
        assert_eq!(
            CatBaker::slot_count(56),
            20,
            "2 MiB / (224·112·4) degrades gracefully"
        );
        for ch in [14u16, 20, 40, 56, 90] {
            let bytes =
                CatBaker::slot_count(ch) * (4 * usize::from(ch)) * (2 * usize::from(ch)) * 4;
            assert!(bytes <= MAX_ATLAS_BYTES, "ch={ch}: {bytes} > 2 MiB");
        }
    }

    /// Same key ⇒ byte-equal bakes, across fresh bakers (determinism).
    #[test]
    fn bake_is_deterministic_across_bakers() {
        let read = |b: &mut CatBaker| -> Vec<u8> {
            let t = b.get_v4(&vk(5)).expect("bake");
            let slot_h = 2 * usize::from(b.cell_h);
            let aw = usize::from(b.slot_w);
            let y0 = usize::from(t.ay);
            let mut out = Vec::new();
            for row in 0..slot_h {
                let s = ((y0 + row) * aw + usize::from(t.ax)) * 4;
                out.extend_from_slice(&b.rgba[s..s + (40 + usize::from(PATCH_STRIP)) * 4]);
            }
            out
        };
        let mut a = CatBaker::default();
        a.begin_frame(10, 20);
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        assert_eq!(read(&mut a), read(&mut b), "same key must bake byte-equal");
    }

    /// LRU eviction at the cap: the least-recently-used slot is replaced; a
    /// re-request of the evicted key re-bakes; the version bumps on every bake
    /// and on wholesale clear.
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
        // Touch slot for coat 0 so coat 1 becomes the LRU victim.
        b.begin_frame(10, 20);
        assert!(b.get_v4(&vk(0)).is_some());
        b.begin_frame(10, 20);
        assert!(b.get_v4(&vk(200)).is_some(), "cap-full insert evicts");
        assert_eq!(b.slots.iter().flatten().count(), cap, "cap never exceeded");
        assert!(
            !b.slots.iter().flatten().any(|s| s.key == vk(1)),
            "the least-recently-used entry was evicted"
        );
        let v1 = b.version();
        b.clear();
        assert_eq!(b.version(), v1 + 1);
        assert!(b.atlas().is_none(), "cleared baker publishes no atlas");
        assert_eq!(b.slots.iter().flatten().count(), 0);
    }

    /// ≤ 2 bakes per frame: the third distinct key in one frame is deferred.
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

    /// Cell-metric change ⇒ wholesale clear + version bump (§5.5).
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

    /// A key wider than the slot minus the patch strip is rejected (no bake).
    #[test]
    fn get_v4_rejects_art_wider_than_slot_minus_strip() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20); // slot_w = 80
        let mut k = vk(0);
        k.w = b.slot_w - PATCH_STRIP + 1;
        assert!(b.get_v4(&k).is_none(), "w + strip > slot_w ⇒ None");
    }

    /// The published atlas is version-keyed and only rebuilt after a bake.
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

    // ───────────────────────── cat-art v4 bake path ─────────────────────────

    fn fills(coat: u8, iris: u8, dark: bool) -> ResolvedFills {
        ResolvedFills::from_indices(coat, iris, dark)
    }

    /// v4 §7: same key ⇒ byte-identical bake, across independent calls and a fresh
    /// bake through the LRU cache.
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

        // The cache re-bakes byte-identically (and the strip catch-light landed).
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
        let mut cache = CatBakerV4::default();
        cache.begin_frame();
        let cached = cache.get(&key).expect("bake").pixels().to_vec();
        assert_eq!(cached, a.pixels(), "cache bake == direct bake");
    }

    /// v4 §7: a `Recolor::Coat` variant recolors — ≥ 3 distinct ramp indices give
    /// pairwise-distinct tiles (the solid-coat colourway spans the ramp).
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

    /// v4 §7: an inherently-patterned variant whose patches are all `Recolor::Fixed`
    /// (the tuxedo — its coat layers are tagged `fixed`) is INVARIANT under a coat
    /// change; recoloring it would betray the reference.
    #[test]
    fn v4_fixed_patterned_variant_is_coat_invariant() {
        // No `Recolor::Coat` layer ⇒ the coat index cannot move any texel.
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

    /// v4 §7: the dark-background bake differs from the light one — the outline is
    /// lifted to [`OUTLINE_PALE`].
    #[test]
    fn v4_dark_bg_lifts_outline() {
        let light = bake_variant(CatGlyphId::S100, &fills(2, 3, false), 96, 64);
        let dark = bake_variant(CatGlyphId::S100, &fills(2, 3, true), 96, 64);
        assert_ne!(
            light.pixels(),
            dark.pixels(),
            "dark_bg must lift the outline to a distinct tile"
        );
    }

    /// DARK-COAT RESCUE: on the dark background bands the darkest coat stops
    /// are lifted to the luminance floor (a dark-grey cat, not a void), coats
    /// already above the floor are bit-exact, hue survives the lift, and the
    /// light bands never rescue (dark-on-light reads fine).
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
            // Band 1 must NOT lift: its own backgrounds span the floor
            // (0.04..0.18), so a lifted coat would converge onto mid-dark
            // themes — the authored dark coat reads as a silhouette there.
            let band1 = ResolvedFills::from_context(coat, 4, band1_ctx);
            let raw = rgb(mix_rgb(
                COAT_RAMP[usize::from(coat)],
                CONTEXT_ACCENTS[12],
                0.12,
            ));
            assert_eq!(band1.coat, raw, "coat {coat} is bit-exact on band 1");
        }
        // A mid/pale coat is untouched by the rescue: bit-exact across bands
        // up to the (band-driven) accent, i.e. the lift itself is a no-op.
        let pale = ResolvedFills::from_context(13, 4, dark_ctx);
        let raw = rgb(mix_rgb(COAT_RAMP[13], CONTEXT_ACCENTS[12], 0.12));
        assert_eq!(pale.coat, raw, "coats above the floor are bit-exact");
        // Hue survives the lift (identity: a blue-charcoal cat stays blue).
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
        // Lifted coats stay DISTINCT (collectible identity): the four darkest
        // stops may converge in brightness but not into one colour.
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

    /// A footprint whose AVERAGE is decisively dark keeps the dark-band pale
    /// outline when it clipped a MID-luminance region — but a genuinely BRIGHT
    /// (band 3) clip keeps the mixed dual treatment (gamma-space averaging can
    /// hide a sizeable white strip inside a band-0 average, and a pale outline
    /// would vanish over it). (The old rule froze the dark-keyline mix for a
    /// whole flight over a dark terminal — the "kitty looks too dark" report.)
    #[test]
    fn dark_average_footprint_keeps_the_pale_outline() {
        let dark_bg = 0x000A_0A10; // band 0
        let band2_bg = 0x007A_7A7A; // band 2 (relative luminance ~0.198)
        assert_eq!(background_band(band2_bg), 2, "test constant is band 2");
        // Averaged-dark + a clipped MID region: stays band 0 (pale outline).
        let mid_clip = CatColorKey::from_rgb_span(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0, 0, 2);
        assert_eq!(
            mid_clip.background, 0,
            "a decisively dark average with a mid clip keeps the pale outline"
        );
        // Averaged-dark + a genuinely BRIGHT clip: the dual treatment (its
        // dark keyline + pale underlay keep ≥3:1 on both halves).
        let bright_clip = CatColorKey::from_rgb_span(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0, 0, 3);
        assert_eq!(
            bright_clip.background, 4,
            "a bright strip inside a dark average stays mixed"
        );
        // A genuinely mid average that spans the crossover keeps the mixed class.
        let mixed = CatColorKey::from_rgb_span(band2_bg, 0x00C0_C8D0, 0x00A0_A8B0, 1, 3);
        assert_eq!(mixed.background, 4, "a true crossover span stays mixed");
        // Uniform spans remain byte-exact with from_rgb (the pinned contract).
        let uniform = CatColorKey::from_rgb_span(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0, 0, 1);
        assert_eq!(
            uniform,
            CatColorKey::from_rgb(dark_bg, 0x00C0_C8D0, 0x00A0_A8B0),
            "uniform spans are unchanged"
        );
    }

    /// Local RGB is deliberately quantized before it reaches the LRU: nearby
    /// reds share one tile, a blue context selects another, and the key remains
    /// exactly two bytes regardless of arbitrary terminal colors.
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

        for (index, &accent) in CONTEXT_ACCENTS[..12].iter().enumerate() {
            let (hue, _, _) = rgb2hsv(accent);
            let expected = index as f32 * 30.0;
            let distance = (hue - expected).abs().min(360.0 - (hue - expected).abs());
            assert!(
                distance <= 0.6,
                "anchor {index} is {hue:.2}°, expected {expected:.2}°"
            );
        }
    }

    #[test]
    fn context_background_selects_contrast_outline() {
        let dark = CatColorKey::from_rgb(0x0008_0910, 0x00F0_E8E0, 0x00D0_C8C0);
        let light = CatColorKey::from_rgb(0x00FA_FAF4, 0x0020_242A, 0x0040_4850);
        assert!(dark.dark());
        assert!(!light.dark());
        let dark_fill = ResolvedFills::from_context(8, 4, dark);
        let light_fill = ResolvedFills::from_context(8, 4, light);
        let lum = |c: (f32, f32, f32)| 0.2126 * c.0 + 0.7152 * c.1 + 0.0722 * c.2;
        assert!(lum(dark_fill.outline) > lum(light_fill.outline));
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

    /// v4 §7: an accessory overlay changes texels vs the bare variant AND is a
    /// distinct cache key (a bow cat never aliases its bare twin).
    #[test]
    fn v4_accessory_overlay_differs_and_is_distinct_key() {
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

        let base = BakeKeyV4 {
            variant: CatGlyphId::S100,
            accessory: None,
            coat: 5,
            iris: 3,
            colors: CatColorKey {
                accent: 12,
                background: 3,
            },
            w: 96,
            h: 96,
            eyes: EyesFrame::Open,
        };
        let bowed = BakeKeyV4 {
            accessory: Some(CatGlyphId::AccBow),
            ..base
        };
        assert_ne!(base, bowed, "accessory is a distinct BakeKeyV4");

        // Distinct keys occupy distinct cache slots.
        let mut cache = CatBakerV4::default();
        cache.begin_frame();
        cache.get(&base).expect("bake bare");
        cache.begin_frame();
        cache.get(&bowed).expect("bake bow");
        assert_eq!(cache.len(), 2, "two distinct keys ⇒ two slots");
    }

    /// The blink/squint axis bakes distinct tiles (open ≠ happy ≠ blink) while
    /// `Open` is byte-identical to the plain authored bake, and each `eyes` value
    /// is a distinct BakeKeyV4 (its own cache slot) so a live blink never aliases
    /// the open cat.
    #[test]
    fn eyes_frame_bakes_distinct_but_open_is_authored() {
        let f = fills(5, 3, false);
        let open = bake_variant_with(CatGlyphId::S100, None, &f, 96, 64, EyesFrame::Open);
        let happy = bake_variant_with(CatGlyphId::S100, None, &f, 96, 64, EyesFrame::Happy);
        let blink = bake_variant_with(CatGlyphId::S100, None, &f, 96, 64, EyesFrame::Blink);
        let plain = bake_variant(CatGlyphId::S100, &f, 96, 64);
        assert_eq!(open.pixels(), plain.pixels(), "Open == authored art");
        assert_ne!(open.pixels(), happy.pixels(), "a squint changes texels");
        assert_ne!(open.pixels(), blink.pixels(), "a blink changes texels");
        assert_ne!(happy.pixels(), blink.pixels(), "squint and blink differ");

        let base = BakeKeyV4 {
            variant: CatGlyphId::S100,
            accessory: None,
            coat: 5,
            iris: 3,
            colors: CatColorKey::default(),
            w: 40,
            h: 32,
            eyes: EyesFrame::Open,
        };
        let winking = BakeKeyV4 {
            eyes: EyesFrame::Blink,
            ..base
        };
        assert_ne!(base, winking, "eyes is a distinct BakeKeyV4 axis");
        let mut cache = CatBakerV4::default();
        cache.begin_frame();
        cache.get(&base).expect("bake open");
        cache.begin_frame();
        cache.get(&winking).expect("bake blink");
        assert_eq!(cache.len(), 2, "open and blink occupy distinct slots");
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

    /// v4 §7: the LRU honours the per-frame bake budget ([`MAX_BAKES_PER_FRAME`]) —
    /// a miss past the budget returns `None` and retries next frame.
    #[test]
    fn v4_cache_respects_bake_budget() {
        let mut cache = CatBakerV4::default();
        cache.begin_frame();
        let mk = |c: u8| BakeKeyV4 {
            variant: CatGlyphId::S100,
            accessory: None,
            coat: c,
            iris: 3,
            colors: CatColorKey::default(),
            w: 40,
            h: 32,
            eyes: EyesFrame::Open,
        };
        for c in 0..MAX_BAKES_PER_FRAME as u8 {
            assert!(cache.get(&mk(c)).is_some(), "within budget bakes");
        }
        assert!(
            cache.get(&mk(200)).is_none(),
            "past the per-frame budget ⇒ None (retry next frame)"
        );
        cache.begin_frame();
        assert!(cache.get(&mk(200)).is_some(), "budget resets next frame");
    }

    /// v4 perf gate (§7): one `bake_variant` at a working size must stay under
    /// 1.5 ms. Manual-timing idiom (matches `bench_cat_bake`):
    ///
    /// ```sh
    /// cargo test -p aterm-effects --release v4_bench_bake_variant -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
    fn v4_bench_bake_variant() {
        use std::time::Instant;
        let f = ResolvedFills::from_context(
            5,
            3,
            CatColorKey {
                accent: 12,
                background: 4,
            },
        );
        for _ in 0..8 {
            assert!(
                bake_variant(CatGlyphId::SpecTabbybell, &f, 156, 156)
                    .pixels()
                    .iter()
                    .any(|&b| b != 0)
            );
        }
        let iters = 64usize;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let start = Instant::now();
            let t = bake_variant(CatGlyphId::SpecTabbybell, &f, 156, 156);
            samples.push(start.elapsed());
            assert!(t.pixels().iter().any(|&b| b != 0));
        }
        samples.sort();
        let median = samples[iters / 2];
        println!(
            "v4_bench_bake_variant: median {median:?} over {iters} bakes of the \
             densest mixed-background glyph (SpecTabbybell, 156x156; min {:?}, max {:?})",
            samples[0],
            samples[iters - 1]
        );
        assert!(
            median < std::time::Duration::from_micros(1500),
            "§7 gate: median {median:?} >= 1.5 ms"
        );
    }
}

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
    /// The snapshot published BEFORE [`CatBaker::published`], kept so the next
    /// dirty publish can recycle its buffer instead of cloning the whole atlas
    /// (see [`CatBaker::atlas`]). A consumer that keeps only the newest snapshot
    /// (the renderer's `ensure_free_atlas` holds exactly one) has released this
    /// one by the time it is reused, so `Arc::get_mut` succeeds; if anything is
    /// still holding it the publish falls back to today's full clone. Invariant:
    /// `spare.is_some()` implies `published.is_some()` (it is only ever filled
    /// from `published`), so [`CatBaker::clear`]'s already-empty early-out stays
    /// exact.
    spare: Option<Arc<SceneAtlas>>,
    dirty: bool,
    /// Per-slot write stamp: the `version` this slot's band in `rgba` was last
    /// baked at, parallel to `slots`. A recycled snapshot carries the `version`
    /// its bytes were synced at, so every band stamped ABOVE that is exactly the
    /// set of bands that have changed since — the rest are already byte-equal.
    slot_version: Vec<u64>,
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
            self.slot_version.clear();
            self.slot_version.resize(slots, 0);
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
        self.slot_version.fill(0);
        self.rgba.fill(0);
        self.version = self.version.wrapping_add(1);
        self.published = None;
        self.spare = None;
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
            let width = u32::from(self.slot_w);
            // v2.9 (2-band head): each slot band is `2·ch` rows tall.
            let height = (self.slots.len() * 2 * usize::from(self.cell_h)) as u32;
            let band = 2 * usize::from(self.cell_h) * usize::from(self.slot_w) * 4;
            // Steady-state publish: recycle the buffer published two frames ago
            // and patch ONLY the slot bands baked since it was last synced. The
            // published bytes are IDENTICAL to the full clone below — `rgba` is
            // written in exactly two places (the `get_v4` / `host_tile` bake
            // blits, each of which rewrites one whole band and stamps it) plus
            // `clear`, which drops both snapshots — so a band whose stamp is at
            // or below the buffer's own synced `version` still holds byte-equal
            // texels. Width, height and the byte count never move (only `clear`
            // / a metric change resize `rgba`, and both drop the buffers), so
            // nothing downstream of the atlas sees anything but the same pixels
            // it would have seen from a fresh clone.
            let spare = self.spare.take();
            let recycled = spare.and_then(|mut arc| {
                let synced = arc.version;
                // Still referenced by a consumer (or the shape moved under a
                // metric change): fall back to the full clone.
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
        if let Some(stamp) = self.slot_version.get_mut(i) {
            *stamp = self.version;
        }
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
        if let Some(stamp) = self.slot_version.get_mut(i) {
            *stamp = self.version;
        }
        self.dirty = true;
        Some(CatTile {
            ax: 0,
            ay: y0 as u16,
        })
    }

    /// Peek [`CatBaker::host_tile`]'s cache WITHOUT supplying texels: the
    /// exact hit branch of `host_tile` — same lookup, same `last_used` LRU
    /// touch (dropping the touch would change which slot a later miss
    /// evicts, which CAN change a pixel: a still-alive tile silently gone
    /// for a frame) — returning `None` on a miss instead of baking.
    ///
    /// Exists for callers whose tile bytes are EXPENSIVE to produce yet a
    /// pure function of the id itself (the pet's mote lane in
    /// [`crate::word_decorations::WordDecorations::pet_cursor`]):
    /// `host_tile` discards `rgba` on every hit, so pre-baking the bytes
    /// just to look the slot up is discarded work on every warm frame.
    /// Peek first; rasterize and call `host_tile` only on the miss. No
    /// version bump and no bake-budget spend here — both belong exclusively
    /// to the miss path, exactly as inside `host_tile` today (its hit
    /// branch touches neither).
    pub fn host_peek(&mut self, host_id: u64) -> Option<CatTile> {
        // The uninitialised-baker refusal, mirrored from `host_tile`. Also
        // unreachable-with-a-match in practice: `begin_frame` wholesale-
        // clears the slots on any metric change, so a zero cell height has
        // no populated slot for the lookup below to find.
        if self.cell_h == 0 {
            return None;
        }
        let slot_h = 2 * usize::from(self.cell_h);
        let i = self
            .slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.host == Some(host_id)))?;
        let slot = self.slots[i].as_mut().expect("position() found it");
        slot.last_used = self.clock;
        Some(CatTile {
            ax: 0,
            ay: (i * slot_h) as u16,
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

// Cursor-head reflection LOD. The authored catch-light remains the primary art
// direction; these device-sized passes make it survive the ~31 px settings/
// shipping bake and add one quiet lower echo. Large heads stay restrained by
// the radius ceilings, while truly tiny word cats keep their authored pixels.
const OPEN_EYE_REFLECTION_MIN_PX: f32 = 3.0;
/// Authored eyes flatter than this are arcs/lids, not open pupils. Keep their
/// existing catch-light art, but never synthesize a glossy pupil pass over it.
const OPEN_EYE_SHALLOW_ASPECT: f32 = 0.6;
const OPEN_EYE_PRIMARY_FRAC: f32 = 0.17;
const OPEN_EYE_PRIMARY_MIN_RADIUS_PX: f32 = 0.62;
const OPEN_EYE_PRIMARY_MAX_RADIUS_PX: f32 = 1.08;
const OPEN_EYE_ECHO_MIN_PX: f32 = 4.5;
const OPEN_EYE_ECHO_FRAC: f32 = 0.075;
// A pixel-centred 4x4 sample's far corner is ~0.53 px away. Clearing that
// distance gives the echo one genuinely pale centre texel instead of four
// weak gray flecks, while the ceiling keeps it subordinate at large sizes.
const OPEN_EYE_ECHO_MIN_RADIUS_PX: f32 = 0.55;
const OPEN_EYE_ECHO_MAX_RADIUS_PX: f32 = 0.72;

fn rgb(hex: u32) -> (f32, f32, f32) {
    (
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    )
}

/// Paint a supersampled reflection disc, but only over an already-opaque dark
/// eye texel covered by `eye_mask`. This is the hard containment seam shared by
/// the flying head and resident pet: a reflection can replace dark eye/pupil
/// ink, never a dark coat or outline merely because it happens to share the
/// same colour range. The differential tests independently rasterize the eye
/// geometry and prove every changed texel has coverage, including anti-aliased
/// boundary texels.
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
    // Catch-lights this small are visual punctuation, not soft shading. Snap
    // their centre to a device pixel so the 4x4 coverage pass leaves one crisp
    // high point instead of distributing the same energy over four gray
    // pixels. The dark-eye destination mask below still provides the hard
    // silhouette boundary.
    let cx = cx.floor() + 0.5;
    let cy = cy.floor() + 0.5;
    let rr = r * r;
    let x0 = ((cx - r) as i32).saturating_sub(1).max(0);
    let y0 = ((cy - r) as i32).saturating_sub(1).max(0);
    let x1 = ((cx + r) as i32).saturating_add(2).min(tile.width() as i32);
    let y1 = ((cy + r) as i32)
        .saturating_add(2)
        .min(tile.height() as i32);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y as u32 * tile.width() + x as u32) * 4) as usize;
            let covered_by_eye = eye_mask.pixels().get(i..i + 4).is_some_and(|p| p[3] != 0);
            let dark_eye = covered_by_eye
                && tile
                    .pixels()
                    .get(i..i + 4)
                    .is_some_and(|p| p[3] == 255 && p[..3].iter().all(|&c| c < 96));
            if !dark_eye {
                continue;
            }
            let mut hit = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = x as f32 + (sx as f32 + 0.5) * 0.25;
                    let fy = y as f32 + (sy as f32 + 0.5) * 0.25;
                    let (dx, dy) = (fx - cx, fy - cy);
                    if dx * dx + dy * dy <= rr {
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
/// `eyes` value, which bakes a distinct (but cache-cheap) tile. `Open` keeps the
/// authored eye geometry and primary catch-light, then adds a resolution-aware
/// glossy finish at shipping sizes; expression geometry stays authored.
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

/// Normalized control-hull bounds of one authored path. Bézier curves remain
/// inside their control hull, so this can overestimate an eye by a fraction but
/// can never put a reflection outside geometry the path could occupy.
fn path_bounds(path: &[PathSeg]) -> Option<(f32, f32, f32, f32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u16::MAX, u16::MAX, 0u16, 0u16);
    let mut seen = false;
    let mut take = |x: u16, y: u16| {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
        seen = true;
    };
    for seg in path {
        match *seg {
            PathSeg::Move(x, y) | PathSeg::Line(x, y) => take(x, y),
            PathSeg::Cubic(ax, ay, bx, by, x, y) => {
                take(ax, ay);
                take(bx, by);
                take(x, y);
            }
            PathSeg::Close => {}
        }
    }
    let norm = |v: u16| f32::from(v) / f32::from(FIXED_ONE);
    seen.then(|| (norm(x0), norm(y0), norm(x1), norm(y1)))
}

/// Pair one authored catch-light centre with its closest authored eye path.
/// Returning the path index lets the roster-wide differential test use the
/// exact same ownership decision as the production painter.
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

/// Reinforce the authored upper catch-light and add a smaller lower-right echo
/// to each genuinely open eye. Each catch-light is paired to the nearest eye,
/// so asymmetric/winking heads do not depend on two independently-authored path
/// lists having identical ordering. A head without authored catch-lights opts
/// out: its blank/closed expression stays exactly as the artist drew it.
fn paint_open_eye_reflections(tile: &mut Tile, layers: &[Layer], xform: PathTransform) {
    let Some(eyes) = layers.iter().find(|layer| layer.role == GlyphRole::Eye) else {
        return;
    };
    let Some(catch_lights) = layers
        .iter()
        .find(|layer| layer.role == GlyphRole::CatchLight)
    else {
        return;
    };
    let mut eye_mask = Tile::new(tile.width(), tile.height());
    fill_path_fixed(&mut eye_mask, eyes.paths, (0.0, 0.0, 0.0), 1.0, xform);

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
        let (ex0, ey0, ex1, ey1) = eye_bounds;
        let eye_w = (ex1 - ex0) * xform.scale_x;
        let eye_h = (ey1 - ey0) * xform.scale_y;
        let minor = eye_w.min(eye_h);

        // Keep the primary exactly where the authored catch-light put it, but
        // give it a device-pixel floor so 4×4 coverage cannot gray it away.
        let primary_r = (minor * OPEN_EYE_PRIMARY_FRAC).clamp(
            OPEN_EYE_PRIMARY_MIN_RADIUS_PX,
            OPEN_EYE_PRIMARY_MAX_RADIUS_PX,
        );
        paint_dark_masked_disc(
            tile,
            &eye_mask,
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
            // Continue the authored primary→eye-centre ray just past the
            // centre. This lands in the lower pupil, unlike a percentage of
            // the whole aperture (which can land out in the coloured iris).
            let echo_x = (pcx + 1.55 * (ecx - pcx))
                .clamp(ex0 + 0.25 * (ex1 - ex0), ex1 - 0.25 * (ex1 - ex0));
            let echo_y = (pcy + 1.55 * (ecy - pcy))
                .clamp(ey0 + 0.25 * (ey1 - ey0), ey1 - 0.25 * (ey1 - ey0));
            paint_dark_masked_disc(
                tile,
                &eye_mask,
                xform.dx + echo_x * xform.scale_x,
                xform.dy + echo_y * xform.scale_y,
                echo_r,
                (1.0, 0.95, 0.98),
                0.72,
            );
        }
    }
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
    reflections: bool,
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
    if reflections && matches!(eyes, EyesFrame::Open) {
        paint_open_eye_reflections(tile, def.layers, xform);
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
    paint_glyph(&mut tile, id, fills, PathTransform::fit(w, h), eyes, true);
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
            true,
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
    use crate::cat_glyphs_gen::GLYPH_IDS;

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

    /// CB-1: the RECYCLED publish is byte-equal to the full clone it replaces.
    ///
    /// Two bakers are driven with identical inputs. `fast` drops each published
    /// snapshot immediately, so its spare is uniquely owned and `atlas()` takes
    /// the recycle-and-patch path; `mirror` RETAINS every snapshot, so its spare
    /// is never uniquely owned and it always falls back to the full
    /// `rgba.clone()`. Every published field must match, on every frame, across
    /// bakes, host tiles, hits, eviction and the metric-change clear — a stale
    /// or torn band (the one way this optimization could regress invisibly)
    /// shows up here as a byte diff, not as a wrong pixel three months later.
    #[test]
    fn recycled_publish_is_byte_equal_to_full_clone() {
        let mut fast = CatBaker::default();
        let mut mirror = CatBaker::default();
        let mut retained: Vec<Arc<SceneAtlas>> = Vec::new();
        let mut published = 0usize;
        for f in 0..96u64 {
            // A metric change part-way through proves the clear path drops both
            // buffers (a recycled buffer of the OLD size must never be patched).
            let (cw, ch) = if f == 61 { (12u16, 24u16) } else { (10, 20) };
            fast.begin_frame(cw, ch);
            mirror.begin_frame(cw, ch);
            if f % 3 != 2 {
                // More keys than slots: bakes, hits and LRU eviction interleave.
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

    /// CB-1: the publish really does recycle — the third dirty publish lands in
    /// the buffer the FIRST one allocated — and a snapshot a consumer is still
    /// holding is NEVER mutated (the one hazard of an in-place publish). Pointer
    /// identity is sound here: every snapshot stays alive inside the baker or in
    /// a local for the whole test, so no address can be recycled underneath us.
    #[test]
    fn dirty_publish_recycles_and_never_mutates_a_held_snapshot() {
        let mut b = CatBaker::default();
        b.begin_frame(10, 20);
        b.get_v4(&vk(0)).expect("bake 1");
        let a1 = b.atlas().expect("publish 1");
        let p1 = Arc::as_ptr(&a1);
        drop(a1); // only the baker holds it now
        b.begin_frame(10, 20);
        b.get_v4(&vk(1)).expect("bake 2");
        // KEEP this one: it is the spare the third publish would otherwise reuse
        // after the fourth, so it pins the fallback path below.
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

        // The spare is now the snapshot `held` still references, so `get_mut`
        // must fail and the publish must allocate rather than mutate it.
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

    // ───────────────────────── cat-art v4 bake path ─────────────────────────

    fn fills(coat: u8, iris: u8, dark: bool) -> ResolvedFills {
        ResolvedFills::from_indices(coat, iris, dark)
    }

    /// Test-only negative-control bake: identical art + patch-strip path, with
    /// the new reflection pass selectable so a pixel diff isolates that pass.
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
        );
        let r = f32::from(PATCH_STRIP) * 0.5;
        tile.disc(w as f32 + r, r, r.max(1.0), rgb(CATCH_LIGHT), 1.0);
        tile
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
    /// `Open` is byte-identical to the default open bake (authored geometry plus
    /// its bounded glossy finish), and each `eyes` value is a distinct BakeKeyV4
    /// (its own cache slot) so a live blink never aliases the open cat.
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

    /// Exact settings-preview size (18 px cell × 1.70 rows ≈ 31 px): the
    /// glossy pass must visibly change the default flying head, and every one
    /// of those changes must replace a fully-opaque dark texel inside the
    /// authored eye silhouette. Most of the original dark eye/pupil pixels must
    /// remain. Happy, blink, and an authored shallow-eye head are byte-identical
    /// with the pass toggled, proving no floating reflections land on closures.
    #[test]
    fn flying_cursor_reflections_are_contained_and_expression_aware() {
        const W: u32 = 38;
        const H: u32 = 31;
        let fills = fills(8, 4, false);
        let plain = bake_reflection_control(CatGlyphId::S100, &fills, W, H, EyesFrame::Open, false);
        let glossy = bake_reflection_control(CatGlyphId::S100, &fills, W, H, EyesFrame::Open, true);
        let eye_layer = GLYPHS[CatGlyphId::S100 as usize]
            .layers
            .iter()
            .find(|layer| layer.role == GlyphRole::Eye)
            .expect("default head authors eyes");
        let xform = PathTransform::fit(W, H);
        let mut eye_mask = Tile::new(W, H);
        fill_path_fixed(&mut eye_mask, eye_layer.paths, (0.0, 0.0, 0.0), 1.0, xform);

        let changed: Vec<(usize, &[u8; 4], &[u8; 4])> = plain
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(glossy.pixels().as_chunks::<4>().0)
            .enumerate()
            .filter_map(|(i, (a, b))| {
                (i % (plain.width() as usize) < W as usize && a != b).then_some((i, a, b))
            })
            .collect();
        assert!(
            !changed.is_empty(),
            "the exact settings-preview bake must gain visible reflection texels"
        );
        assert!(
            changed
                .iter()
                .any(|(_, _, after)| after[..3].iter().all(|&c| c >= 160)),
            "the reflection diff must contain a visibly pale texel"
        );
        for &(i, before, _) in &changed {
            let x = i % plain.width() as usize;
            let y = i / plain.width() as usize;
            let mi = (y * W as usize + x) * 4;
            let mask = &eye_mask.pixels()[mi..mi + 4];
            assert_ne!(
                mask[3], 0,
                "changed texel ({x},{y}) left the authored eye footprint"
            );
            assert!(
                before[3] == 255 && before[..3].iter().all(|&c| c < 96),
                "changed texel ({x},{y}) was not dark eye/pupil ink: {before:?}"
            );
        }

        for path in eye_layer.paths {
            let (x0, y0, x1, y1) = path_bounds(path).expect("eye path has bounds");
            let x0 = (x0 * W as f32).floor().max(0.0) as usize;
            let y0 = (y0 * H as f32).floor().max(0.0) as usize;
            let x1 = (x1 * W as f32).ceil().min(W as f32) as usize;
            let y1 = (y1 * H as f32).ceil().min(H as f32) as usize;
            let dark_count = |tile: &Tile| {
                (y0..y1)
                    .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                    .filter(|&(x, y)| {
                        let i = (y * tile.width() as usize + x) * 4;
                        let p = &tile.pixels()[i..i + 4];
                        p[3] == 255 && p[..3].iter().all(|&c| c < 96)
                    })
                    .count()
            };
            let before = dark_count(&plain);
            let after = dark_count(&glossy);
            assert!(before >= 4, "fixture eye has too little dark ink: {before}");
            assert!(
                after >= 2 && after * 2 >= before,
                "reflection consumed the eye core: before={before}, after={after}"
            );
        }

        for state in [EyesFrame::Happy, EyesFrame::Blink] {
            let without = bake_reflection_control(CatGlyphId::S100, &fills, W, H, state, false);
            let with = bake_reflection_control(CatGlyphId::S100, &fills, W, H, state, true);
            assert_eq!(
                without.pixels(),
                with.pixels(),
                "{state:?} must never receive open-eye reflections"
            );
        }
        // Use a large bake so the shallow-expression assertion exercises the
        // aspect gate itself, rather than passing incidentally through the
        // tiny-eye size floor.
        const SHALLOW_H: u32 = 120;
        let shallow_w =
            (f32::from(GLYPHS[CatGlyphId::S101 as usize].aspect_x1000) * SHALLOW_H as f32 / 1000.0)
                .round() as u32;
        let shallow_plain = bake_reflection_control(
            CatGlyphId::S101,
            &fills,
            shallow_w,
            SHALLOW_H,
            EyesFrame::Open,
            false,
        );
        let shallow_glossy = bake_reflection_control(
            CatGlyphId::S101,
            &fills,
            shallow_w,
            SHALLOW_H,
            EyesFrame::Open,
            true,
        );
        assert_eq!(
            shallow_plain.pixels(),
            shallow_glossy.pixels(),
            "authored shallow eyes must not gain reflections"
        );
    }

    /// The glossy pass sits in the generic v4 baker, so its safety proof must
    /// cover the generic authored roster rather than only the default S100
    /// preview. Sweep tiny word-cat, settings-preview, enlarged cursor and
    /// reference-art sizes on both grounds. Every changed texel must belong to
    /// the exact authored path production paired with a catch-light, every eye
    /// the pass actually affects is checked independently, and at least half
    /// of its dark pupil/core must remain. Glyphs with no eligible open eye
    /// must remain byte-identical when the pass is toggled. Non-vacuity is
    /// pinned to both the settings-preview head and the live cursor's authored
    /// tongue-out/oops expression; several authored heads already paint the entire
    /// candidate glint area white and are intentionally byte-identical under
    /// reinforcement.
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

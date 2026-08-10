// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ambient animal-word **roster**'s bake path: one authored species head
//! ([`crate::animal_glyphs_gen::ANIMAL_GLYPHS`]) rasterized into one EXACT-SIZE
//! straight-alpha RGBA8 tile, cached in a small LRU, and handed to the shared
//! cat atlas through [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! This is the decision twin of [`crate::pet_baker`] — same rasterizer
//! ([`fill_path_fixed`]), same fill resolution ([`resolve_layer`] over
//! [`ResolvedFills`]), same structural budgets, same no-patch-strip /
//! no-eyes-axis shape — with two further deletions of its own:
//!
//! * **No coat/iris key axes.** The cat and the pet wear the caret genome's
//!   collectible coat; a camel is camel-colored. Every animal layer is authored
//!   `Recolor::Fixed` (species-true paint), so the only context that can change
//!   a texel is the outline's contrast ink + mixed-background underlay — which
//!   ride [`CatColorKey`] exactly as they do for the other rosters. The fills
//!   resolver still wants ramp indices; they are pinned to 0 and, with no
//!   `Coat`/`Iris` layers to consume them, provably never reach a pixel.
//! * **No eye-dot LOD rig.** The pet's grown-eye machinery exists because its
//!   full-body poses author terminal-size faces inside a 1.7-row body. Species
//!   heads are two-row PORTRAITS — the art contract (see
//!   `art/animal/README.md`) requires features bold enough to survive a 16 px
//!   bake as authored, pinned by `animal_art_quality`. Below
//!   [`FINE_DETAIL_MIN_H`] the sub-pixel charm roles are simply culled.
//!
//! Budgets are shared verbatim with the other rosters: [`MAX_SLOTS`] resident
//! tiles, [`MAX_ATLAS_BYTES`] resident texels, [`MAX_BAKES_PER_FRAME`] cold
//! bakes per presented frame, wholesale clear on cell-metric change. One
//! screenful of DISTINCT species (each word a different head at one size) is
//! bounded by the emitter's own per-frame animal cap, so 32 slots hold a busy
//! screen without thrashing.

use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::animal_glyphs_gen::{ANIMAL_GLYPHS, AnimalGlyphId};
use crate::cat_baker::{
    CatColorKey, MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS, ResolvedFills, resolve_layer,
};
use crate::cat_glyphs_gen::GlyphRole;

/// Below this tile height (device px) the fine charm roles — whisker, blush,
/// pattern, catch-light, detail — are culled: at a 2-row bake under small cell
/// metrics they rasterize to sub-pixel noise that muddies the silhouette the
/// 16 px review bar protects. Eyes, nose, mouth and muzzle always paint — the
/// art contract authors them bold enough to survive.
pub const FINE_DETAIL_MIN_H: u32 = 40;

/// Bake-cache key for one animal tile: everything that can change a texel, and
/// nothing that cannot. All-integer by construction (the
/// [`BakeKey`](crate::cat_baker::BakeKeyV4) discipline) — the resolved outline
/// ink is derived from [`CatColorKey`] at bake time and never stored in a
/// hashed key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnimalBakeKey {
    /// The authored species head.
    pub species: AnimalGlyphId,
    /// Quantized local terminal palette (accent family + background band):
    /// drives the outline contrast ink and the mixed-background underlay.
    pub colors: CatColorKey,
    /// Tile width in px. The tile is EXACTLY this wide — no patch strip.
    pub w: u16,
    /// Tile height in px.
    pub h: u16,
}

impl AnimalBakeKey {
    /// Resolve the local palette into concrete bake fills. The coat/iris ramp
    /// indices are pinned (no animal layer consumes them — see module docs).
    #[must_use]
    pub fn fills(&self) -> ResolvedFills {
        ResolvedFills::from_context(0, 0, self.colors)
    }

    /// Bake the tile this key describes (the cache-miss path).
    #[must_use]
    pub fn bake(&self) -> Tile {
        bake_species(
            self.species,
            &self.fills(),
            u32::from(self.w),
            u32::from(self.h),
        )
    }

    /// A stable `u64` identity for
    /// [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile) — FNV-1a
    /// over the key's integer fields (NOT `DefaultHasher`; the atlas slot must
    /// survive process restarts and std releases). The high bit is set, like
    /// the pet's, so an animal id can never collide with a small hand-assigned
    /// host id; the salt below keeps it from colliding with a PET tile whose
    /// integer fields happen to line up.
    #[must_use]
    pub fn host_id(&self) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        const ANIMAL_SALT: u64 = 0xA111_3A15;
        let mut h = FNV_OFFSET;
        let mut eat = |b: u64| {
            h ^= b;
            h = h.wrapping_mul(FNV_PRIME);
        };
        eat(ANIMAL_SALT);
        eat(self.species as u64);
        eat(u64::from(self.colors.accent));
        eat(u64::from(self.colors.background));
        eat(u64::from(self.w));
        eat(u64::from(self.h));
        h | (1 << 63)
    }
}

/// Rasterize one authored species head into a fresh `w × h` [`Tile`], painter
/// order = layer order, each layer's colour resolved by [`resolve_layer`] and
/// its fixed-point frame mapped onto the tile by [`PathTransform::fit`].
///
/// Deterministic: the const integer drawlist plus the fixed-point filler give
/// byte-identical output for identical arguments — the LRU below is a pure
/// function of its key.
///
/// The one non-plain fill is the MIXED-background halo, inherited verbatim
/// from the pet bake: when [`ResolvedFills::outline_underlay`] is `Some`, the
/// outline gets a compact pale keyline underneath so the silhouette clears
/// contrast on footprints that straddle the light/dark crossover.
#[must_use]
pub fn bake_species(species: AnimalGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    let mut tile = Tile::new(w, h);
    if w == 0 || h == 0 {
        return tile;
    }
    let xform = PathTransform::fit(w, h);
    let fine_lod = h < FINE_DETAIL_MIN_H;
    for layer in ANIMAL_GLYPHS[species as usize].layers {
        if fine_lod
            && matches!(
                layer.role,
                GlyphRole::Whisker
                    | GlyphRole::Blush
                    | GlyphRole::Pattern
                    | GlyphRole::CatchLight
                    | GlyphRole::Detail
            )
        {
            continue;
        }
        let (col, alpha) = resolve_layer(layer, fills);
        if layer.role == GlyphRole::Outline
            && let Some(underlay) = fills.outline_underlay
        {
            // Sub-pixel halo band, clamped exactly like the pet's: wider reads
            // as a second outline instead of a legibility backstop.
            let halo = (xform.scale_x.min(xform.scale_y) * 0.04).clamp(0.75, 1.25);
            for (dx, dy) in [(halo, 0.0), (-halo, 0.0), (0.0, halo), (0.0, -halo)] {
                fill_path_fixed(
                    &mut tile,
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
        fill_path_fixed(&mut tile, layer.paths, col, alpha, xform);
    }
    tile
}

/// One resident tile: its key, its texels, and the frame it was last touched on.
struct AnimalSlot {
    key: AnimalBakeKey,
    tile: Tile,
    last_used: u64,
}

/// Host-side LRU of exact-size animal tiles (see the module docs for budgets).
///
/// `Default` is a valid empty baker: the first
/// [`begin_frame`](AnimalBaker::begin_frame) adopts the cell metrics.
#[derive(Default)]
pub struct AnimalBaker {
    slots: Vec<AnimalSlot>,
    /// Sum of resident texel bytes, kept incrementally so the
    /// [`MAX_ATLAS_BYTES`] residency bound costs O(1) per admission.
    bytes: usize,
    cell_w: u16,
    cell_h: u16,
    /// LRU clock, advanced once per [`begin_frame`](AnimalBaker::begin_frame).
    clock: u64,
    /// Cold bakes still allowed this frame.
    bakes_left: u32,
    /// Monotonic; bumped on every bake and every wholesale clear, so a host
    /// that folds it into a frame fingerprint re-uploads after either.
    version: u64,
}

impl AnimalBaker {
    /// Per-tick prologue: advance the LRU clock, reset the per-frame cold-bake
    /// budget, and wholesale-clear on a cell-metric change. Call exactly once
    /// per PRESENTED frame, not once per pane (the [`MAX_BAKES_PER_FRAME`] cap
    /// is a frame budget).
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        self.clock = self.clock.wrapping_add(1);
        self.bakes_left = MAX_BAKES_PER_FRAME;
        if (cell_w, cell_h) != (self.cell_w, self.cell_h) {
            self.clear();
            self.cell_w = cell_w;
            self.cell_h = cell_h;
        }
    }

    /// Drop every resident tile and bump the version. A no-op when already
    /// empty, so a per-frame "effect is off" reset costs nothing.
    pub fn clear(&mut self) {
        if self.slots.is_empty() {
            return;
        }
        self.slots.clear();
        self.bytes = 0;
        self.version = self.version.wrapping_add(1);
    }

    /// Monotonic bake/clear counter — fold it into the frame fingerprint.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Resident tile count (`≤ MAX_SLOTS`).
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the cache holds no tiles.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Resident texel bytes (`≤ MAX_ATLAS_BYTES`) — the residency-bound proof
    /// hook, same as the pet's.
    #[doc(hidden)]
    pub fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// The authored aspect (width ÷ height) of a species head, from its asset
    /// viewbox. Callers size the destination box from THIS so a long camel
    /// face and a round penguin dome both keep the shape their artist drew.
    /// Clamped away from zero so a degenerate asset cannot produce a NaN/∞
    /// destination rect.
    #[must_use]
    pub fn aspect(species: AnimalGlyphId) -> f32 {
        (f32::from(ANIMAL_GLYPHS[species as usize].aspect_x1000) / 1000.0).max(0.001)
    }

    /// Look `key` up, baking on a miss when the per-frame budget allows.
    ///
    /// Returns straight-alpha RGBA8, exactly `key.w · key.h · 4` bytes, rows
    /// top-down — ready for [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile)
    /// with [`AnimalBakeKey::host_id`]. `None` means "not this frame" (budget
    /// spent or degenerate key) — keep drawing the previously resolved tile
    /// and ask again next frame, exactly like the pet path.
    pub fn tile(&mut self, key: &AnimalBakeKey) -> Option<&[u8]> {
        if key.w == 0 || key.h == 0 {
            return None;
        }
        let want = usize::from(key.w) * usize::from(key.h) * 4;
        // A tile that alone exceeds the residency budget is refused rather
        // than admitted by evicting everything else.
        if want > MAX_ATLAS_BYTES {
            return None;
        }
        if let Some(i) = self.slots.iter().position(|s| s.key == *key) {
            self.slots[i].last_used = self.clock;
            return Some(self.slots[i].tile.pixels());
        }
        if self.bakes_left == 0 {
            return None;
        }
        self.bakes_left -= 1;
        let tile = key.bake();
        // Make room BEFORE inserting, so the pushed slot's index is final and
        // the tile being handed out can never be the one evicted.
        while !self.slots.is_empty()
            && (self.slots.len() >= MAX_SLOTS || self.bytes + want > MAX_ATLAS_BYTES)
        {
            self.evict_lru();
        }
        self.bytes += tile.pixels().len();
        self.slots.push(AnimalSlot {
            key: *key,
            tile,
            last_used: self.clock,
        });
        self.version = self.version.wrapping_add(1);
        Some(self.slots[self.slots.len() - 1].tile.pixels())
    }

    /// Drop the least-recently-used slot, lowest index breaking ties — the
    /// tiebreak keeps eviction a deterministic function of the request
    /// sequence.
    fn evict_lru(&mut self) {
        let Some(victim) = self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(i, s)| (s.last_used, *i))
            .map(|(i, _)| i)
        else {
            return;
        };
        self.bytes -= self.slots[victim].tile.pixels().len();
        self.slots.remove(victim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_species() -> AnimalGlyphId {
        crate::animal_glyphs_gen::ANIMAL_GLYPH_IDS[0]
    }

    #[test]
    fn bake_is_deterministic_and_exact_size() {
        let key = AnimalBakeKey {
            species: any_species(),
            colors: CatColorKey::default(),
            w: 40,
            h: 28,
        };
        let a = key.bake();
        let b = key.bake();
        assert_eq!(a.pixels(), b.pixels(), "bake must be a pure function");
        assert_eq!(a.pixels().len(), 40 * 28 * 4, "exact-size tile, no strip");
        assert!(
            a.pixels().iter().any(|&p| p != 0),
            "a species head must paint something"
        );
    }

    #[test]
    fn cache_hits_and_frame_budget_bound_cold_bakes() {
        let mut baker = AnimalBaker::default();
        baker.begin_frame(8, 16);
        let key = AnimalBakeKey {
            species: any_species(),
            colors: CatColorKey::default(),
            w: 32,
            h: 30,
        };
        assert!(baker.tile(&key).is_some(), "first bake fits the budget");
        let v = baker.version();
        assert!(baker.tile(&key).is_some(), "hit");
        assert_eq!(baker.version(), v, "a hit is not a bake");
        // Distinct keys past the per-frame budget defer to the next frame.
        let mut deferred = 0;
        for w in 33..64u16 {
            let k = AnimalBakeKey { w, ..key };
            if baker.tile(&k).is_none() {
                deferred += 1;
            }
        }
        assert!(deferred > 0, "the per-frame cold-bake budget must bind");
    }

    #[test]
    fn metric_change_clears_residency() {
        let mut baker = AnimalBaker::default();
        baker.begin_frame(8, 16);
        let key = AnimalBakeKey {
            species: any_species(),
            colors: CatColorKey::default(),
            w: 24,
            h: 20,
        };
        assert!(baker.tile(&key).is_some());
        assert!(!baker.is_empty());
        baker.begin_frame(9, 18);
        assert!(baker.is_empty(), "cell-metric change clears the cache");
        assert_eq!(baker.resident_bytes(), 0);
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ambient animal-word roster's bake path: one authored species head
//! ([`crate::animal_glyphs_gen::ANIMAL_GLYPHS`]) rasterized into one exact-size
//! straight-alpha RGBA8 tile, cached in a small LRU, and handed to the shared
//! cat atlas through [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! The twin of [`crate::pet_baker`] (same rasterizer [`fill_path_fixed`], same
//! fill resolution [`resolve_layer`] over [`ResolvedFills`], same budgets, no
//! patch strip or eyes axis), minus:
//!
//! * **Coat/iris key axes.** Every animal layer is `Recolor::Fixed`
//!   (species-true paint), so only the outline's contrast ink and
//!   mixed-background underlay, via [`CatColorKey`], can change a texel. The
//!   ramp indices the resolver wants are pinned to 0 and never reach a pixel.
//! * **The eye-dot LOD rig.** Species heads are two-row portraits whose art
//!   contract (`art/animal/README.md`, pinned by `animal_art_quality`) requires
//!   features bold enough for a 16 px bake. Below [`FINE_DETAIL_MIN_H`] the
//!   sub-pixel charm roles are culled.
//!
//! Budgets are the shared [`ExactTileBaker`](crate::pet_baker::ExactTileBaker)'s:
//! [`MAX_SLOTS`](crate::cat_baker::MAX_SLOTS) resident tiles,
//! [`MAX_ATLAS_BYTES`](crate::cat_baker::MAX_ATLAS_BYTES) resident texels,
//! [`MAX_BAKES_PER_FRAME`](crate::cat_baker::MAX_BAKES_PER_FRAME) cold bakes
//! per frame, wholesale clear on cell-metric change. The emitter's per-frame
//! animal cap bounds distinct species on screen, so 32 slots hold a busy
//! screen without thrashing.

use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::animal_glyphs_gen::{ANIMAL_GLYPHS, AnimalGlyphId};
use crate::cat_baker::{CatColorKey, ResolvedFills, paint_outline_halo, resolve_layer};
use crate::cat_glyphs_gen::GlyphRole;
use crate::pet_baker::{ExactTileBaker, ExactTileKey, host_tile_id};

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
    /// [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile): FNV-1a
    /// over the key's integer fields (not `DefaultHasher`, so the id survives
    /// restarts and std releases). High bit set, like the pet's, to avoid small
    /// hand-assigned host ids; the salt separates it from pet tiles.
    #[must_use]
    pub fn host_id(&self) -> u64 {
        const ANIMAL_SALT: u64 = 0xA111_3A15;
        host_tile_id(&[
            ANIMAL_SALT,
            self.species as u64,
            u64::from(self.colors.accent),
            u64::from(self.colors.background),
            u64::from(self.w),
            u64::from(self.h),
        ])
    }
}

/// Rasterize one authored species head into a fresh `w × h` [`Tile`], painter
/// order = layer order, each layer's colour resolved by [`resolve_layer`] and
/// its fixed-point frame mapped onto the tile by [`PathTransform::fit`].
/// Deterministic, so the LRU is a pure function of its key.
///
/// As in the pet bake, when [`ResolvedFills::outline_underlay`] is `Some` the
/// outline gets a compact pale keyline underneath, so the silhouette clears
/// contrast across the light/dark crossover.
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
        if layer.role == GlyphRole::Outline {
            paint_outline_halo(&mut tile, layer, fills, alpha, xform);
        }
        fill_path_fixed(&mut tile, layer.paths, col, alpha, xform);
    }
    tile
}

impl ExactTileKey for AnimalBakeKey {
    fn size(&self) -> (u16, u16) {
        (self.w, self.h)
    }

    fn bake_tile(&self) -> Tile {
        self.bake()
    }
}

/// Host-side LRU of exact-size animal tiles — the
/// [`crate::pet_baker::PetBaker`] structure itself (see the module docs for
/// budgets).
pub type AnimalBaker = ExactTileBaker<AnimalBakeKey>;

impl AnimalBaker {
    /// The authored aspect (width ÷ height) of a species head, from its asset
    /// viewbox. Callers size the destination box from THIS so a long camel
    /// face and a round penguin dome both keep the shape their artist drew.
    /// Clamped away from zero so a degenerate asset cannot produce a NaN/∞
    /// destination rect.
    #[must_use]
    pub fn aspect(species: AnimalGlyphId) -> f32 {
        (f32::from(ANIMAL_GLYPHS[species as usize].aspect_x1000) / 1000.0).max(0.001)
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

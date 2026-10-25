// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bake path for the ambient animal-word roster: one species head
//! ([`crate::animal_glyphs_gen::ANIMAL_GLYPHS`]) rasterized into an exact-size
//! straight-alpha RGBA8 tile, cached in an LRU and handed to the shared cat atlas
//! through [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! It is [`crate::pet_baker`]'s path with no coat/iris key axes (every animal layer
//! is `Recolor::Fixed`, so only [`CatColorKey`](crate::cat_baker::CatColorKey) can change a texel) and no eye-dot
//! LOD: heads are authored bold enough for a 16 px bake (`art/animal/README.md`),
//! so below [`FINE_DETAIL_MIN_H`](crate::animal_baker::FINE_DETAIL_MIN_H) only the sub-pixel charm roles are culled.

use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::animal_glyphs_gen::{ANIMAL_GLYPHS, AnimalGlyphId};
use crate::cat_baker::{
    CatColorKey, ResolvedFills, paint_outline_halo, resolve_layer, take_or_new_tile,
};
use crate::cat_glyphs_gen::GlyphRole;
use crate::pet_baker::{ExactTileBaker, ExactTileKey, host_tile_id};

/// Below this tile height (device px) the fine charm roles (whisker, blush,
/// pattern, catch-light, detail) are culled: at small cell metrics they
/// rasterize to sub-pixel noise that muddies the silhouette. Eyes, nose, mouth
/// and muzzle always paint.
pub const FINE_DETAIL_MIN_H: u32 = 40;

/// Bake-cache key for one animal tile: everything that can change a texel, and
/// nothing that cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnimalBakeKey {
    /// The authored species head.
    pub species: AnimalGlyphId,
    /// Quantized local terminal palette (accent family + background band):
    /// drives the outline contrast ink and the mixed-background underlay.
    pub colors: CatColorKey,
    /// Tile width in px (exact: no patch strip).
    pub w: u16,
    /// Tile height in px.
    pub h: u16,
}

impl AnimalBakeKey {
    /// Resolve the local palette into bake fills; the coat/iris ramp indices are
    /// pinned to 0 (no animal layer reads them).
    #[inline]
    #[must_use]
    pub const fn fills(&self) -> ResolvedFills {
        ResolvedFills::from_context(0, 0, self.colors)
    }

    /// Bake the tile this key describes (the cache-miss path).
    #[inline]
    #[must_use]
    pub fn bake(&self) -> Tile {
        bake_species(
            self.species,
            &self.fills(),
            u32::from(self.w),
            u32::from(self.h),
        )
    }

    /// A stable `u64` id for [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile):
    /// FNV-1a over the key's fields, so it survives restarts and std releases. The
    /// salt separates it from pet tiles.
    #[inline]
    #[must_use]
    pub const fn host_id(&self) -> u64 {
        const ANIMAL_SALT: u64 = 0xA111_3A15;
        host_tile_id(&[
            ANIMAL_SALT,
            self.species as u64,
            self.colors.accent as u64,
            self.colors.background as u64,
            self.w as u64,
            self.h as u64,
        ])
    }
}

/// Rasterize one species head into a fresh `w × h` [`Tile`] in layer order.
/// Deterministic, so the LRU is a pure function of its key.
#[must_use]
pub fn bake_species(species: AnimalGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    let mut tile = Tile::new(w, h);
    paint_species_onto(&mut tile, species, fills, w, h);
    tile
}

fn paint_species_onto(
    tile: &mut Tile,
    species: AnimalGlyphId,
    fills: &ResolvedFills,
    w: u32,
    h: u32,
) {
    if w == 0 || h == 0 {
        return;
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
            paint_outline_halo(tile, layer, fills, alpha, xform);
        }
        fill_path_fixed(tile, layer.paths, col, alpha, xform);
    }
}

impl ExactTileKey for AnimalBakeKey {
    #[inline]
    fn size(&self) -> (u16, u16) {
        (self.w, self.h)
    }

    #[inline]
    fn bake_tile(&self) -> Tile {
        self.bake()
    }

    #[inline]
    fn bake_tile_with(&self, recycled: Option<Tile>, _mask_scratch: &mut Option<Tile>) -> Tile {
        let (w, h) = (u32::from(self.w), u32::from(self.h));
        let mut tile = take_or_new_tile(recycled, w, h);
        paint_species_onto(&mut tile, self.species, &self.fills(), w, h);
        tile
    }
}

/// Host-side LRU of exact-size animal tiles.
pub type AnimalBaker = ExactTileBaker<AnimalBakeKey>;

impl AnimalBaker {
    /// The authored aspect (width ÷ height) of a species head, from its asset
    /// viewbox; callers size the destination box from it so each head keeps
    /// its drawn shape. Clamped away from zero so a degenerate asset cannot
    /// produce a NaN/∞ destination rect.
    #[inline]
    #[must_use]
    pub const fn aspect(species: AnimalGlyphId) -> f32 {
        let a = (ANIMAL_GLYPHS[species as usize].aspect_x1000 as f32) / 1000.0;
        if a > 0.001 { a } else { 0.001 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn any_species() -> AnimalGlyphId {
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
}

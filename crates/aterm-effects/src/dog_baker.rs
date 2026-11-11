// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The dog roster's bake path: one authored breed head ([`crate::dog_glyphs_gen::DOG_GLYPHS`])
//! rasterized into an exact-size straight-alpha RGBA8 tile, cached in a small LRU, and handed
//! to the shared cat atlas through [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! The [`crate::pet_baker`] pattern (same rasterizer, fill resolution and budgets) without the
//! patch strip or the eye and iris axes: a dog head is one static bake, so its `GlyphRole::Eye`
//! layers take [`ResolvedFills::eye_ink`](crate::cat_baker::ResolvedFills::eye_ink) (dark on pale coats, pale on dark) instead of the
//! authored fill. Glints stay authored (`GlyphRole::Detail`).

use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::{
    CatColorKey, ResolvedFills, paint_outline_halo, resolve_layer, take_or_new_tile,
};
use crate::cat_glyphs_gen::GlyphRole;
use crate::dog_glyphs_gen::{DOG_GLYPHS, DogGlyphId};
use crate::pet_baker::{ExactTileBaker, ExactTileKey, host_tile_id};

/// The [`EYE_RAMP`](crate::cat_baker::EYE_RAMP) stop [`ResolvedFills::from_context`] requires.
/// No dog layer uses `Recolor::Iris`, so it never touches a texel.
const DOG_IRIS_STOP: u8 = 2;

/// FNV namespace salt hashed first by [`DogBakeKey::host_id`], so a dog id never collides with a
/// pet key that shares its field values (both set the high bit in the same atlas).
const DOG_HOST_SALT: u64 = 0x646f_675f_6865_6164; // b"dog_head"

/// Bake-cache key for one dog tile: everything that can change a texel, and nothing that
/// cannot. All-integer, like [`BakeKeyV4`](crate::cat_baker::BakeKeyV4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DogBakeKey {
    /// The authored breed head this summon rolled.
    pub breed: DogGlyphId,
    /// [`COAT_RAMP`](crate::cat_baker::COAT_RAMP) index for `Recolor::Coat` layers.
    pub coat: u8,
    /// Quantized local terminal palette (accent family + background band).
    pub colors: CatColorKey,
    /// Tile width in px, exactly: no patch strip.
    pub w: u16,
    /// Tile height in px.
    pub h: u16,
}

impl DogBakeKey {
    /// Resolve this key's ramp index + local palette into concrete bake fills (floats appear
    /// only past this point).
    #[inline]
    #[must_use]
    pub const fn fills(&self) -> ResolvedFills {
        ResolvedFills::from_context(self.coat, DOG_IRIS_STOP, self.colors)
    }

    /// Bake the tile this key describes (the cache-miss path).
    #[inline]
    #[must_use]
    pub fn bake(&self) -> Tile {
        bake_breed(
            self.breed,
            &self.fills(),
            u32::from(self.w),
            u32::from(self.h),
        )
    }

    /// A stable `u64` id for [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile):
    /// FNV-1a over the salt and the key's integer fields, high bit set.
    #[inline]
    #[must_use]
    pub const fn host_id(&self) -> u64 {
        host_tile_id(&[
            DOG_HOST_SALT,
            self.breed as u64,
            self.coat as u64,
            self.colors.accent as u64,
            self.colors.background as u64,
            self.w as u64,
            self.h as u64,
        ])
    }
}

/// Rasterize one authored breed head into a fresh `w × h` [`Tile`] in layer order:
/// `GlyphRole::Eye` layers take [`ResolvedFills::eye_ink`], outlines get the pet's halo.
#[inline]
#[must_use]
pub fn bake_breed(breed: DogGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    let mut tile = Tile::new(w, h);
    paint_breed_onto(&mut tile, breed, fills, w, h);
    tile
}

#[inline]
fn paint_breed_onto(tile: &mut Tile, breed: DogGlyphId, fills: &ResolvedFills, w: u32, h: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let xform = PathTransform::fit(w, h);
    let layers = DOG_GLYPHS[breed as usize].layers;
    let mut i = 0;
    while i < layers.len() {
        let layer = &layers[i];
        i += 1;
        let (col, alpha) = if layer.role == GlyphRole::Eye {
            (fills.eye_ink, 1.0)
        } else {
            resolve_layer(layer, fills)
        };
        if matches!(layer.role, GlyphRole::Outline) {
            paint_outline_halo(tile, layer, fills, alpha, xform);
        }
        fill_path_fixed(tile, layer.paths, col, alpha, xform);
    }
}

impl ExactTileKey for DogBakeKey {
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
        if w != 0 && h != 0 {
            paint_breed_onto(&mut tile, self.breed, &self.fills(), w, h);
        }
        tile
    }
}

/// Host-side LRU of exact-size dog tiles: the [`crate::pet_baker::PetBaker`] structure.
pub type DogBaker = ExactTileBaker<DogBakeKey>;

impl DogBaker {
    /// The authored aspect (width ÷ height) of a breed head, clamped away from zero so a
    /// degenerate asset cannot produce a NaN/∞ destination rect.
    #[inline]
    #[must_use]
    pub const fn aspect(breed: DogGlyphId) -> f32 {
        const BREED_ASPECTS: [f32; DOG_GLYPHS.len()] = {
            let mut out = [0.0f32; DOG_GLYPHS.len()];
            let mut i = 0;
            while i < DOG_GLYPHS.len() {
                let a = (DOG_GLYPHS[i].aspect_x1000 as f32) / 1000.0;
                out[i] = if a > 0.001 { a } else { 0.001 };
                i += 1;
            }
            out
        };
        BREED_ASPECTS[breed as usize]
    }
}

#[cfg(test)]
#[path = "dog_baker_tests.rs"]
mod tests;

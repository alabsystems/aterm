// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The dog roster's bake path: one authored breed head ([`crate::dog_glyphs_gen::DOG_GLYPHS`])
//! rasterized into an exact-size straight-alpha RGBA8 tile, cached in a small LRU, and handed
//! to the shared cat atlas through [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! The [`crate::pet_baker`] pattern (same rasterizer, fill resolution and budgets) without the
//! patch strip or the eye and iris axes: a dog head is one static bake, so its `GlyphRole::Eye`
//! layers take [`ResolvedFills::eye_ink`] (dark on pale coats, pale on dark) instead of the
//! authored fill. Glints stay authored (`GlyphRole::Detail`).

use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::{CatColorKey, ResolvedFills, paint_outline_halo, resolve_layer};
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
    pub fn fills(&self) -> ResolvedFills {
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
    pub fn host_id(&self) -> u64 {
        host_tile_id(&[
            DOG_HOST_SALT,
            self.breed as u64,
            u64::from(self.coat),
            u64::from(self.colors.accent),
            u64::from(self.colors.background),
            u64::from(self.w),
            u64::from(self.h),
        ])
    }
}

/// Rasterize one authored breed head into a fresh `w × h` [`Tile`] in layer order:
/// `GlyphRole::Eye` layers take [`ResolvedFills::eye_ink`], outlines get the pet's halo.
#[must_use]
pub fn bake_breed(breed: DogGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    let mut tile = Tile::new(w, h);
    if w == 0 || h == 0 {
        return tile;
    }
    let xform = PathTransform::fit(w, h);
    for layer in DOG_GLYPHS[breed as usize].layers {
        let (col, alpha) = if layer.role == GlyphRole::Eye {
            (fills.eye_ink, 1.0)
        } else {
            resolve_layer(layer, fills)
        };
        if matches!(layer.role, GlyphRole::Outline) {
            paint_outline_halo(&mut tile, layer, fills, alpha, xform);
        }
        fill_path_fixed(&mut tile, layer.paths, col, alpha, xform);
    }
    tile
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
}

/// Host-side LRU of exact-size dog tiles: the [`crate::pet_baker::PetBaker`] structure.
pub type DogBaker = ExactTileBaker<DogBakeKey>;

impl DogBaker {
    /// The authored aspect (width ÷ height) of a breed head, clamped away from zero so a
    /// degenerate asset cannot produce a NaN/∞ destination rect.
    #[inline]
    #[must_use]
    pub fn aspect(breed: DogGlyphId) -> f32 {
        (f32::from(DOG_GLYPHS[breed as usize].aspect_x1000) / 1000.0).max(0.001)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cat_baker::{MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS};
    use crate::dog_glyphs_gen::DOG_GLYPH_IDS;

    /// Find a breed by a substring of its authored asset id.
    fn breed(needle: &str) -> DogGlyphId {
        DOG_GLYPH_IDS
            .iter()
            .copied()
            .find(|id| DOG_GLYPHS[*id as usize].id.contains(needle))
            .unwrap_or_else(|| panic!("dog roster has no breed matching `{needle}`"))
    }

    fn key(coat: u8) -> DogBakeKey {
        DogBakeKey {
            breed: breed("retriever"),
            coat,
            colors: CatColorKey::default(),
            w: 48,
            h: 40,
        }
    }

    /// Determinism: one key ⇒ byte-identical texels, across repeated lookups,
    /// across independent bakers, and against a direct [`bake_breed`].
    #[test]
    fn same_key_bakes_byte_identical_tiles() {
        let k = key(5);
        let mut a = DogBaker::default();
        a.begin_frame(10, 20);
        let first = a.tile(&k).expect("bake").to_vec();
        let second = a.tile(&k).expect("hit").to_vec();
        assert_eq!(first, second);

        let mut b = DogBaker::default();
        b.begin_frame(10, 20);
        assert_eq!(first, b.tile(&k).expect("bake").to_vec());

        let direct = bake_breed(k.breed, &k.fills(), u32::from(k.w), u32::from(k.h));
        assert_eq!(first, direct.pixels());
    }

    /// Exact-size, no patch strip, and the key axes actually move texels.
    #[test]
    fn tiles_are_exact_size_and_axes_are_wired() {
        let mut b = DogBaker::default();
        let mut bake = |k: &DogBakeKey| {
            b.begin_frame(10, 20);
            b.tile(k).expect("bake").to_vec()
        };
        let base = key(9);
        let plain = bake(&base);
        assert_eq!(
            plain.len(),
            usize::from(base.w) * usize::from(base.h) * 4,
            "dog tiles carry no reserved gaze strip"
        );
        assert_ne!(
            plain,
            bake(&DogBakeKey { coat: 1, ..base }),
            "coat moves texels"
        );
        assert_ne!(
            plain,
            bake(&DogBakeKey {
                breed: breed("husky"),
                ..base
            }),
            "a different breed is a different tile"
        );
        assert_ne!(
            plain,
            bake(&DogBakeKey {
                colors: CatColorKey {
                    accent: 12,
                    background: 3,
                },
                ..base
            }),
            "the background band moves texels"
        );
    }

    /// Every breed across all 16 coat stops draws a substantial, multi-colour dog.
    #[test]
    fn every_breed_draws_a_dog() {
        let (w, h) = (96u32, 82u32);
        for &id in DOG_GLYPH_IDS {
            for coat in 0..16u8 {
                let fills = ResolvedFills::from_indices(coat, 2, coat < 4);
                let tile = bake_breed(id, &fills, w, h);
                let px = tile.pixels();
                let (lo, hi) = (h / 3, 2 * h / 3);
                let band = ((hi - lo) * w) as usize;
                let mut opaque = 0usize;
                for y in lo..hi {
                    for x in 0..w {
                        let i = ((y * w + x) * 4) as usize;
                        if px[i + 3] > 128 {
                            opaque += 1;
                        }
                    }
                }
                let coverage = opaque as f32 / band as f32;
                assert!(
                    coverage > 0.30,
                    "{} coat {coat}: covers only {:.1}% of its middle band",
                    DOG_GLYPHS[id as usize].id,
                    coverage * 100.0
                );
                let mut colors: Vec<u32> = px
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| p[3] > 200)
                    .map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
                    .collect();
                colors.sort_unstable();
                colors.dedup();
                assert!(
                    colors.len() >= 3,
                    "{} coat {coat}: only {} distinct opaque colours",
                    DOG_GLYPHS[id as usize].id,
                    colors.len()
                );
            }
        }
    }

    /// `host_id` is stable, key-separating, and namespaced away from pet ids (high bit + salt).
    #[test]
    fn host_ids_are_stable_distinct_and_namespaced() {
        let a = key(1);
        assert_eq!(a.host_id(), key(1).host_id());
        assert_ne!(a.host_id(), key(2).host_id());
        assert_ne!(a.host_id(), DogBakeKey { w: 49, ..a }.host_id());
        assert!(
            (a.host_id() >> 63) == 1,
            "dog host ids live in the high half"
        );
    }

    /// The bake-rate cap and LRU ceiling hold (the pet baker's contract).
    #[test]
    fn budgets_hold() {
        let mut b = DogBaker::default();
        b.begin_frame(10, 20);
        for c in 0..MAX_BAKES_PER_FRAME as u8 {
            assert!(b.tile(&key(c)).is_some());
        }
        assert!(b.tile(&key(200)).is_none(), "budget spent ⇒ deferred");
        assert!(b.tile(&key(0)).is_some(), "a HIT is not a bake");
        b.begin_frame(10, 20);
        assert!(b.tile(&key(200)).is_some(), "the budget resets each frame");
        for c in 0..40u8 {
            b.begin_frame(10, 20);
            b.tile(&key(c));
            assert!(b.len() <= MAX_SLOTS);
            assert!(b.resident_bytes() <= MAX_ATLAS_BYTES);
        }
    }
}

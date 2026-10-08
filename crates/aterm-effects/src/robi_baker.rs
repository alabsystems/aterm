// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Robi roster's bake path: one authored robot pose
//! ([`crate::robi_glyphs_gen::ROBI_GLYPHS`]) rasterized into one exact-size
//! straight-alpha RGBA8 tile, cached in a small LRU, and handed to the shared
//! cat atlas through [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! The [`crate::dog_baker`] pattern without recolor axes: Robi authors only
//! `Recolor::Fixed` layers, so the bake key is just pose + size and his
//! animation frames do not fragment the shared LRU.

use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::rgb;
use crate::pet_baker::{ExactTileBaker, ExactTileKey, host_tile_id};
use crate::robi_glyphs_gen::{ROBI_GLYPHS, RobiGlyphId};

/// FNV namespace salt hashed first by [`RobiBakeKey::host_id`], so Robi ids
/// cannot collide with pet/dog/animal keys in the shared atlas.
const ROBI_HOST_SALT: u64 = 0x726f_6269_5f62_6f74; // b"robi_bot"

/// Bake-cache key for one Robi tile: everything that can change a texel, and
/// nothing that cannot — for an all-`Fixed` roster that is just pose + size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RobiBakeKey {
    /// The authored pose frame.
    pub pose: RobiGlyphId,
    /// Tile width in px (exact; no patch strip).
    pub w: u16,
    /// Tile height in px.
    pub h: u16,
}

impl RobiBakeKey {
    /// Bake the tile this key describes (the cache-miss path).
    #[must_use]
    pub fn bake(&self) -> Tile {
        bake_pose(self.pose, u32::from(self.w), u32::from(self.h))
    }

    /// A stable `u64` identity for
    /// [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile) — FNV-1a
    /// over a namespace salt plus the key's own integer fields, high bit set
    /// (the [`crate::pet_baker`] contract, in a salted namespace of its own).
    #[must_use]
    pub fn host_id(&self) -> u64 {
        host_tile_id(&[
            ROBI_HOST_SALT,
            self.pose as u64,
            u64::from(self.w),
            u64::from(self.h),
        ])
    }

    /// Shared-atlas identity of one vertical slice of this tile. Robi is taller
    /// than the atlas's 2-row host-tile limit, so the emitter splits him into
    /// stacked slices, each with its own id in the namespaced high half.
    #[must_use]
    pub fn host_id_slice(&self, slice: u16) -> u64 {
        (self.host_id() ^ (u64::from(slice) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)) | (1 << 63)
    }
}

/// Rasterize one authored Robi pose into a fresh `w × h` [`Tile`], in layer
/// order, each layer at its authored fixed fill.
#[must_use]
pub fn bake_pose(pose: RobiGlyphId, w: u32, h: u32) -> Tile {
    let mut tile = Tile::new(w, h);
    if w == 0 || h == 0 {
        return tile;
    }
    let xform = PathTransform::fit(w, h);
    for layer in ROBI_GLYPHS[pose as usize].layers {
        fill_path_fixed(&mut tile, layer.paths, rgb(layer.fill), 1.0, xform);
    }
    tile
}

impl ExactTileKey for RobiBakeKey {
    fn size(&self) -> (u16, u16) {
        (self.w, self.h)
    }

    fn bake_tile(&self) -> Tile {
        self.bake()
    }
}

/// Host-side LRU of exact-size Robi tiles (see [`crate::pet_baker::PetBaker`]
/// for the budget).
pub type RobiBaker = ExactTileBaker<RobiBakeKey>;

impl RobiBaker {
    /// The authored aspect (width ÷ height) of a pose, from its asset viewbox.
    /// Clamped away from zero so a degenerate asset cannot produce a NaN/∞
    /// destination rect.
    #[must_use]
    pub fn aspect(pose: RobiGlyphId) -> f32 {
        (f32::from(ROBI_GLYPHS[pose as usize].aspect_x1000) / 1000.0).max(0.001)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cat_baker::{MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS};
    use crate::robi_glyphs_gen::ROBI_GLYPH_IDS;

    fn key(pose: RobiGlyphId) -> RobiBakeKey {
        RobiBakeKey { pose, w: 44, h: 60 }
    }

    /// The roster carries the full show vocabulary: stand, walk cycle, jumping
    /// jacks, ladder climb, all three monkey-bar hangs, and the tiling ladder.
    #[test]
    fn roster_covers_the_whole_show() {
        assert_eq!(ROBI_GLYPH_IDS.len(), ROBI_GLYPHS.len());
        for needle in [
            "robi_stand",
            "robi_walk_0",
            "robi_walk_1",
            "robi_jacks_0",
            "robi_jacks_1",
            "robi_climb_0",
            "robi_climb_1",
            "robi_hang_both",
            "robi_hang_l",
            "robi_hang_r",
            "robi_ladder",
        ] {
            assert!(
                ROBI_GLYPHS.iter().any(|g| g.id == needle),
                "roster is missing `{needle}`"
            );
        }
        for &id in ROBI_GLYPH_IDS {
            let a = RobiBaker::aspect(id);
            assert!(a.is_finite() && a > 0.0);
            assert!(
                !ROBI_GLYPHS[id as usize].layers.is_empty(),
                "{}: a pose with no layers would bake an empty tile",
                ROBI_GLYPHS[id as usize].id
            );
        }
    }

    /// Determinism: one key ⇒ byte-identical texels, across repeated lookups,
    /// across independent bakers, and against a direct [`bake_pose`].
    #[test]
    fn same_key_bakes_byte_identical_tiles() {
        let k = key(RobiGlyphId::RobiStand);
        let mut a = RobiBaker::default();
        a.begin_frame(10, 20);
        let first = a.tile(&k).expect("bake").to_vec();
        let second = a.tile(&k).expect("hit").to_vec();
        assert_eq!(first, second);

        let mut b = RobiBaker::default();
        b.begin_frame(10, 20);
        assert_eq!(first, b.tile(&k).expect("bake").to_vec());

        let direct = bake_pose(k.pose, u32::from(k.w), u32::from(k.h));
        assert_eq!(first, direct.pixels());
    }

    /// Tiles are exact-size, and the pose axis changes texels.
    #[test]
    fn tiles_are_exact_size_and_the_pose_axis_is_wired() {
        let mut b = RobiBaker::default();
        let mut bake = |k: &RobiBakeKey| {
            b.begin_frame(10, 20);
            b.tile(k).expect("bake").to_vec()
        };
        let base = key(RobiGlyphId::RobiStand);
        let plain = bake(&base);
        assert_eq!(plain.len(), usize::from(base.w) * usize::from(base.h) * 4);
        assert_ne!(
            plain,
            bake(&RobiBakeKey {
                pose: RobiGlyphId::RobiJacks1,
                ..base
            }),
            "a different pose is a different tile"
        );
    }

    /// Every pose draws a substantial, multi-colour robot.
    #[test]
    fn every_pose_draws_a_robot() {
        for &id in ROBI_GLYPH_IDS {
            let is_ladder = ROBI_GLYPHS[id as usize].id == "robi_ladder";
            let (w, h) = if is_ladder {
                (36u32, 24u32)
            } else {
                (52u32, 72u32)
            };
            let tile = bake_pose(id, w, h);
            let px = tile.pixels();
            let mut opaque = 0usize;
            for p in px.as_chunks::<4>().0 {
                if p[3] > 128 {
                    opaque += 1;
                }
            }
            let coverage = opaque as f32 / (w * h) as f32;
            assert!(
                coverage > 0.20,
                "{}: covers only {:.1}% of its tile",
                ROBI_GLYPHS[id as usize].id,
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
            let want = if is_ladder { 2 } else { 4 };
            assert!(
                colors.len() >= want,
                "{}: only {} distinct opaque colours",
                ROBI_GLYPHS[id as usize].id,
                colors.len()
            );
        }
    }

    /// The `host_id` handed to the shared atlas is stable, key-separating, and
    /// namespaced into the high half.
    #[test]
    fn host_ids_are_stable_distinct_and_namespaced() {
        let a = key(RobiGlyphId::RobiStand);
        assert_eq!(a.host_id(), key(RobiGlyphId::RobiStand).host_id());
        assert_ne!(a.host_id(), key(RobiGlyphId::RobiWalk0).host_id());
        assert_ne!(a.host_id(), RobiBakeKey { w: 45, ..a }.host_id());
        assert!((a.host_id() >> 63) == 1);
    }

    /// The bake-rate cap and LRU ceiling hold (the pet baker's contract).
    #[test]
    fn budgets_hold() {
        let mut b = RobiBaker::default();
        b.begin_frame(10, 20);
        let sized = |h: u16| RobiBakeKey {
            pose: RobiGlyphId::RobiStand,
            w: 40,
            h,
        };
        for h in 0..MAX_BAKES_PER_FRAME as u16 {
            assert!(b.tile(&sized(30 + h)).is_some());
        }
        assert!(b.tile(&sized(90)).is_none(), "budget spent ⇒ deferred");
        assert!(b.tile(&sized(30)).is_some(), "a HIT is not a bake");
        b.begin_frame(10, 20);
        assert!(b.tile(&sized(90)).is_some(), "the budget resets each frame");
        for h in 0..40u16 {
            b.begin_frame(10, 20);
            b.tile(&sized(30 + h));
            assert!(b.len() <= MAX_SLOTS);
            assert!(b.resident_bytes() <= MAX_ATLAS_BYTES);
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Robi roster's bake path: one authored robot pose
//! ([`crate::robi_glyphs_gen::ROBI_GLYPHS`]) rasterized into one EXACT-SIZE
//! straight-alpha RGBA8 tile, cached in a small LRU, and handed to the shared
//! cat atlas through [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! This is the [`crate::dog_baker`] pattern applied to the helper robot, minus
//! every recolor axis: Robi authors ONLY `Recolor::Fixed` layers (a white robot
//! with a cyan glow is his identity, not a genome), so the bake key carries no
//! coat/iris/palette context at all — a pose at a size is one tile, ever. That
//! keeps his ten animation frames from fragmenting the shared LRU while he
//! cycles them at walk speed.

use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::{MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS};
use crate::robi_glyphs_gen::{ROBI_GLYPHS, RobiGlyphId};

/// FNV namespace salt eaten FIRST by [`RobiBakeKey::host_id`], so a Robi key's
/// id stream can never collide with a pet/dog/animal key that happens to share
/// its field values (all rosters set the high bit and live in the same shared
/// atlas).
const ROBI_HOST_SALT: u64 = 0x726f_6269_5f62_6f74; // b"robi_bot"

/// Bake-cache key for one Robi tile: everything that can change a texel, and
/// nothing that cannot — for an all-`Fixed` roster that is just pose + size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RobiBakeKey {
    /// The authored pose frame.
    pub pose: RobiGlyphId,
    /// Tile width in px. The tile is EXACTLY this wide — no patch strip.
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
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h = FNV_OFFSET;
        let mut eat = |b: u64| {
            h ^= b;
            h = h.wrapping_mul(FNV_PRIME);
        };
        eat(ROBI_HOST_SALT);
        eat(self.pose as u64);
        eat(u64::from(self.w));
        eat(u64::from(self.h));
        h | (1 << 63)
    }

    /// The shared-atlas identity of one VERTICAL SLICE of this tile. Robi is
    /// taller than the atlas's 2-row host-tile ceiling, so the emitter splits
    /// his texels into stacked slices — each needs its own stable id, in the
    /// same namespaced high half.
    #[must_use]
    pub fn host_id_slice(&self, slice: u16) -> u64 {
        (self.host_id() ^ (u64::from(slice) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)) | (1 << 63)
    }
}

/// Rasterize one authored Robi pose into a fresh `w × h` [`Tile`], painter
/// order = layer order, every layer at its authored fixed fill (the roster has
/// no recolor axes — see the module docs).
#[must_use]
pub fn bake_pose(pose: RobiGlyphId, w: u32, h: u32) -> Tile {
    let mut tile = Tile::new(w, h);
    if w == 0 || h == 0 {
        return tile;
    }
    let xform = PathTransform::fit(w, h);
    for layer in ROBI_GLYPHS[pose as usize].layers {
        let hex = layer.fill;
        let col = (
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
        );
        fill_path_fixed(&mut tile, layer.paths, col, 1.0, xform);
    }
    tile
}

/// One resident tile: its key, its texels, and the frame it was last touched on.
struct RobiSlot {
    key: RobiBakeKey,
    tile: Tile,
    last_used: u64,
}

/// Host-side LRU of exact-size Robi tiles — the [`crate::pet_baker::PetBaker`]
/// structure verbatim (see its docs for the budget rationale).
///
/// `Default` is a valid empty baker: the first
/// [`begin_frame`](RobiBaker::begin_frame) adopts the cell metrics.
#[derive(Default)]
pub struct RobiBaker {
    slots: Vec<RobiSlot>,
    bytes: usize,
    cell_w: u16,
    cell_h: u16,
    clock: u64,
    bakes_left: u32,
    version: u64,
}

impl RobiBaker {
    /// Per-tick prologue: advance the LRU clock, reset the per-frame cold-bake
    /// budget, and wholesale-clear on a cell-metric change. Call exactly once
    /// per PRESENTED frame.
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        self.clock = self.clock.wrapping_add(1);
        self.bakes_left = MAX_BAKES_PER_FRAME;
        if (cell_w, cell_h) != (self.cell_w, self.cell_h) {
            self.clear();
            self.cell_w = cell_w;
            self.cell_h = cell_h;
        }
    }

    /// Drop every resident tile and bump the version; silent when already empty.
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

    /// Resident texel bytes (`≤ MAX_ATLAS_BYTES`).
    #[doc(hidden)]
    pub fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// The authored aspect (width ÷ height) of a pose, from its asset viewbox.
    /// Clamped away from zero so a degenerate asset cannot produce a NaN/∞
    /// destination rect.
    #[must_use]
    pub fn aspect(pose: RobiGlyphId) -> f32 {
        (f32::from(ROBI_GLYPHS[pose as usize].aspect_x1000) / 1000.0).max(0.001)
    }

    /// Look `key` up, baking on a miss when the per-frame budget allows.
    /// Returns straight-alpha RGBA8, exactly `key.w · key.h · 4` bytes — ready
    /// for [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile) with
    /// [`RobiBakeKey::host_id`]. `None` means "not this frame" — keep drawing
    /// the previous tile and ask again next frame.
    pub fn tile(&mut self, key: &RobiBakeKey) -> Option<&[u8]> {
        if key.w == 0 || key.h == 0 {
            return None;
        }
        let want = usize::from(key.w) * usize::from(key.h) * 4;
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
        while !self.slots.is_empty()
            && (self.slots.len() >= MAX_SLOTS || self.bytes + want > MAX_ATLAS_BYTES)
        {
            self.evict_lru();
        }
        self.bytes += tile.pixels().len();
        self.slots.push(RobiSlot {
            key: *key,
            tile,
            last_used: self.clock,
        });
        self.version = self.version.wrapping_add(1);
        Some(self.slots[self.slots.len() - 1].tile.pixels())
    }

    /// Drop the least-recently-used slot, lowest index breaking ties.
    fn evict_lru(&mut self) {
        let Some(victim) = self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(idx, s)| (s.last_used, *idx))
            .map(|(idx, _)| idx)
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

    /// EXACT-SIZE tiles, and the pose axis actually moves texels.
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

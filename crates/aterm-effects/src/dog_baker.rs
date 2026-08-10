// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The dog roster's bake path: one authored breed head
//! ([`crate::dog_glyphs_gen::DOG_GLYPHS`]) rasterized into one EXACT-SIZE
//! straight-alpha RGBA8 tile, cached in a small LRU, and handed to the shared
//! cat atlas through [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! This is the [`crate::pet_baker`] pattern applied verbatim to the typed-word
//! dog cameo — same rasterizer ([`fill_path_fixed`]), same fill resolution
//! ([`resolve_layer`] over [`ResolvedFills`]), same structural budgets, no patch
//! strip and no eyes axis (a breed head authors its own expression). Two
//! deliberate deltas from the pet:
//!
//! * **No iris axis.** The dog roster authors no `Recolor::Iris` layers — dog
//!   eyes are the adaptive eye ink (below), so an iris key axis could only
//!   fragment the cache. [`DogBakeKey::fills`] passes a fixed mid-ramp stop.
//! * **Adaptive eye ink.** Cat heads carry the gaze machinery that guarantees
//!   eye legibility; a dog head is one static bake, so its `GlyphRole::Eye`
//!   layers resolve to [`ResolvedFills::eye_ink`] — the context ink that is
//!   dark on pale coats and pale on dark coats — instead of the authored fixed
//!   fill. Glints stay authored (`GlyphRole::Detail`, white).

use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::{
    CatColorKey, MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS, ResolvedFills, resolve_layer,
};
use crate::cat_glyphs_gen::GlyphRole;
use crate::dog_glyphs_gen::{DOG_GLYPHS, DogGlyphId};

/// The fixed [`EYE_RAMP`](crate::cat_baker::EYE_RAMP) stop passed through
/// [`ResolvedFills::from_context`] for the dog bake. The roster authors no
/// `Recolor::Iris` layers, so this can never touch a texel — it exists only
/// because the resolver's signature demands an index.
const DOG_IRIS_STOP: u8 = 2;

/// FNV namespace salt eaten FIRST by [`DogBakeKey::host_id`], so a dog key's
/// id stream can never collide with a pet key that happens to share its field
/// values (both rosters set the high bit and live in the same shared atlas).
const DOG_HOST_SALT: u64 = 0x646f_675f_6865_6164; // b"dog_head"

/// Bake-cache key for one dog tile: everything that can change a texel, and
/// nothing that cannot. All-integer by construction — the
/// [`BakeKey`](crate::cat_baker::BakeKeyV4) discipline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DogBakeKey {
    /// The authored breed head this summon rolled.
    pub breed: DogGlyphId,
    /// [`COAT_RAMP`](crate::cat_baker::COAT_RAMP) index for `Recolor::Coat` layers.
    pub coat: u8,
    /// Quantized local terminal palette (accent family + background band).
    pub colors: CatColorKey,
    /// Tile width in px. The tile is EXACTLY this wide — no patch strip.
    pub w: u16,
    /// Tile height in px.
    pub h: u16,
}

impl DogBakeKey {
    /// Resolve this key's ramp index + local palette into concrete bake fills
    /// (the key → colours step; floats appear only past this point).
    #[must_use]
    pub fn fills(&self) -> ResolvedFills {
        ResolvedFills::from_context(self.coat, DOG_IRIS_STOP, self.colors)
    }

    /// Bake the tile this key describes (the cache-miss path).
    #[must_use]
    pub fn bake(&self) -> Tile {
        bake_breed(
            self.breed,
            &self.fills(),
            u32::from(self.w),
            u32::from(self.h),
        )
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
        eat(DOG_HOST_SALT);
        eat(self.breed as u64);
        eat(u64::from(self.coat));
        eat(u64::from(self.colors.accent));
        eat(u64::from(self.colors.background));
        eat(u64::from(self.w));
        eat(u64::from(self.h));
        h | (1 << 63)
    }
}

/// Rasterize one authored breed head into a fresh `w × h` [`Tile`], painter
/// order = layer order, each layer's colour resolved by [`resolve_layer`] with
/// the two dog deltas: `GlyphRole::Eye` layers take the adaptive
/// [`ResolvedFills::eye_ink`] (see the module docs), and the mixed-background
/// outline halo rides exactly the pet's rule.
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
        if matches!(layer.role, GlyphRole::Outline)
            && let Some(underlay) = fills.outline_underlay
        {
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
struct DogSlot {
    key: DogBakeKey,
    tile: Tile,
    last_used: u64,
}

/// Host-side LRU of exact-size dog tiles — the [`crate::pet_baker::PetBaker`]
/// structure verbatim (see its docs for the budget rationale).
///
/// `Default` is a valid empty baker: the first
/// [`begin_frame`](DogBaker::begin_frame) adopts the cell metrics.
#[derive(Default)]
pub struct DogBaker {
    slots: Vec<DogSlot>,
    bytes: usize,
    cell_w: u16,
    cell_h: u16,
    clock: u64,
    bakes_left: u32,
    version: u64,
}

impl DogBaker {
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

    /// The authored aspect (width ÷ height) of a breed head, from its asset
    /// viewbox. Clamped away from zero so a degenerate asset cannot produce a
    /// NaN/∞ destination rect.
    #[must_use]
    pub fn aspect(breed: DogGlyphId) -> f32 {
        (f32::from(DOG_GLYPHS[breed as usize].aspect_x1000) / 1000.0).max(0.001)
    }

    /// Look `key` up, baking on a miss when the per-frame budget allows.
    /// Returns straight-alpha RGBA8, exactly `key.w · key.h · 4` bytes — ready
    /// for [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile) with
    /// [`DogBakeKey::host_id`]. `None` means "not this frame" — keep drawing
    /// the previous tile and ask again next frame.
    pub fn tile(&mut self, key: &DogBakeKey) -> Option<&[u8]> {
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
        self.slots.push(DogSlot {
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
    use crate::dog_glyphs_gen::{DOG_GLYPH_IDS, DOG_HEADS};

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

    /// The roster is populated (a WIDE selection — at least eight breeds), the
    /// summon pool is every breed, and each has sane aspect + layers.
    #[test]
    fn roster_is_a_wide_selection_of_dogs() {
        assert!(
            DOG_HEADS.len() >= 8,
            "the dog summon promises a wide selection — got {}",
            DOG_HEADS.len()
        );
        assert_eq!(DOG_GLYPH_IDS.len(), DOG_GLYPHS.len());
        assert_eq!(
            DOG_HEADS.len(),
            DOG_GLYPH_IDS.len(),
            "every authored breed is in the summon pool"
        );
        for &id in DOG_GLYPH_IDS {
            let a = DogBaker::aspect(id);
            assert!(a.is_finite() && a > 0.0);
            assert!(
                !DOG_GLYPHS[id as usize].layers.is_empty(),
                "{}: a breed with no layers would bake an empty tile",
                DOG_GLYPHS[id as usize].id
            );
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

    /// EXACT-SIZE, no patch strip, and the key axes actually move texels.
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

    /// Every breed at every ramp extreme draws a substantial, multi-colour dog
    /// (the pet's smoke test, swept across the whole roster).
    #[test]
    fn every_breed_draws_a_dog() {
        let (w, h) = (96u32, 82u32);
        for &id in DOG_GLYPH_IDS {
            for coat in [0u8, 8, 15] {
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

    /// The `host_id` handed to the shared atlas is stable, key-separating, and
    /// namespaced away from pet ids (high bit + salt).
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

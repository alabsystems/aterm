// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow-kitty **pet**'s bake path: one authored full-body pose glyph
//! ([`crate::pet_glyphs_gen::PET_GLYPHS`]) rasterized into one EXACT-SIZE
//! straight-alpha RGBA8 tile, cached in a small LRU, and handed to the shared
//! cat atlas through
//! [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! This is the decision twin of the cat path in [`crate::cat_baker`] — same
//! rasterizer ([`fill_path_fixed`]), same fill resolution ([`resolve_layer`]
//! over [`ResolvedFills`]), same structural budgets — with exactly two
//! deliberate differences, both of which delete work rather than add it:
//!
//! * **No patch strip.** A cat tile is baked `w + PATCH_STRIP` texels wide so
//!   the live gaze stage has somewhere to sample a pupil + catch-light from
//!   while the body quad samples only `[0, w)`. The pet has **no live gaze
//!   quads**: its eyes are authored into each pose (a crouch stares at the
//!   landing, a groom shuts them, a startle blows them wide), so the reserved
//!   strip would be four columns of transparent texels carried through every
//!   blit, every atlas upload, and every LRU byte budget for nothing. A pet
//!   tile is therefore exactly `w · h · 4` bytes with the art filling all of
//!   it — which is also precisely the shape `host_tile` wants.
//! * **No [`EyesFrame`](crate::cat_baker::EyesFrame) axis.** The cat's blink /
//!   squint is a bake-time squash of the eye family toward a shared eye line,
//!   because ONE authored head has to cover every mood. The pet roster instead
//!   *authors* its eye state per pose — the pose IS the expression — so an
//!   `eyes` key axis would multiply the cache by three while every one of those
//!   tiles fought the pose's own authored intent (a squashed startle is not a
//!   blink, it is a broken startle). Blinking is a POSE choice made upstream in
//!   [`crate::kitty_pet`], not a bake parameter here.
//!
//! ## Budgets (all structural, none assumed)
//!
//! * [`MAX_SLOTS`] resident tiles — reused verbatim from the cat baker. The pet
//!   roster is ~2 dozen poses and only ONE identity (coat/iris) and ONE size are
//!   live at a time, so 32 slots hold a whole gait cycle plus its idle and
//!   startle branches without ever thrashing.
//! * [`MAX_ATLAS_BYTES`] of resident texels — the same 2 MiB ceiling the cat
//!   atlas is sized against, applied here as a *residency* bound so a large cell
//!   metric cannot turn 32 slots into 32 huge tiles. Whichever of the two bounds
//!   binds first wins; a single tile larger than the whole budget is refused
//!   outright rather than draining the cache to admit it.
//! * [`MAX_BAKES_PER_FRAME`] cold bakes per presented frame. A pet that just
//!   changed gait fills its new frames over a few ticks; the caller simply
//!   re-draws its last resolved pose until the tile lands (the same tolerance
//!   the cat entrance has).
//! * A wholesale [`PetBaker::clear`] on cell-metric change. The pixel size is
//!   already IN the key, so a stale tile can never be *mis-sampled* — but it
//!   would occupy a slot no live key can ever hit again, so it is dropped
//!   eagerly rather than waiting to age out.

use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::{
    CatColorKey, MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS, ResolvedFills, resolve_layer,
};
use crate::cat_glyphs_gen::GlyphRole;
use crate::pet_glyphs_gen::{PET_GLYPHS, PetGlyphId};

/// Bake-cache key for one pet tile: everything that can change a texel, and
/// nothing that cannot.
///
/// All-integer by construction — the [`BakeKey`](crate::cat_baker::BakeKeyV4)
/// discipline. The resolved coat/iris/outline colours are `f32` triples, so
/// they are re-derived from these ramp INDICES at bake time
/// ([`PetBakeKey::fills`]) and never stored in a hashed key; a float in an
/// `Eq`/`Hash` type is how a cache silently stops hitting (or starts colliding)
/// on a denormal.
///
/// Note what is deliberately absent: no `eyes` axis (the pose authors its own
/// eye state, see the module docs) and no age/scale axis (the exact `w × h`
/// already encodes every caller-side scaling decision, so a second axis could
/// only fragment the cache).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PetBakeKey {
    /// The authored full-body pose (gait frame, idle, startle, …).
    pub pose: PetGlyphId,
    /// [`COAT_RAMP`](crate::cat_baker::COAT_RAMP) index for `Recolor::Coat` layers.
    pub coat: u8,
    /// [`EYE_RAMP`](crate::cat_baker::EYE_RAMP) index for `Recolor::Iris` layers.
    pub iris: u8,
    /// Quantized local terminal palette (accent family + background band).
    pub colors: CatColorKey,
    /// Tile width in px. The tile is EXACTLY this wide — no patch strip.
    pub w: u16,
    /// Tile height in px.
    pub h: u16,
}

impl PetBakeKey {
    /// Resolve this key's ramp indices + local palette into concrete bake fills
    /// (the key → colours step; floats appear only past this point).
    #[must_use]
    pub fn fills(&self) -> ResolvedFills {
        ResolvedFills::from_context(self.coat, self.iris, self.colors)
    }

    /// Bake the tile this key describes (the cache-miss path).
    #[must_use]
    pub fn bake(&self) -> Tile {
        bake_pose(
            self.pose,
            &self.fills(),
            u32::from(self.w),
            u32::from(self.h),
        )
    }

    /// A stable `u64` identity for
    /// [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile), which
    /// keys host-authored tiles by an opaque id rather than by a cat key.
    ///
    /// FNV-1a over the key's own integer fields, deliberately NOT
    /// `DefaultHasher`: the atlas slot must stay the same tile across process
    /// restarts and across std releases, and `DefaultHasher`'s algorithm is
    /// explicitly not part of std's stable contract. The high bit is set so a
    /// pet id can never collide with a small hand-assigned host id (the kitty
    /// cursor's, for instance) in the shared atlas.
    #[must_use]
    pub fn host_id(&self) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h = FNV_OFFSET;
        let mut eat = |b: u64| {
            h ^= b;
            h = h.wrapping_mul(FNV_PRIME);
        };
        eat(self.pose as u64);
        eat(u64::from(self.coat));
        eat(u64::from(self.iris));
        eat(u64::from(self.colors.accent));
        eat(u64::from(self.colors.background));
        eat(u64::from(self.w));
        eat(u64::from(self.h));
        h | (1 << 63)
    }
}

/// Rasterize one authored pose into a fresh `w × h` [`Tile`], painter order =
/// layer order, each layer's colour resolved by [`resolve_layer`] and its
/// fixed-point `0..=FIXED_ONE` frame mapped onto the tile by
/// [`PathTransform::fit`].
///
/// Deterministic: the const integer drawlist plus the fixed-point filler give
/// byte-identical output for identical arguments, on any host, forever — which
/// is what makes the LRU above safe to treat as a pure function of its key.
///
/// The one thing painted that is not a plain layer fill is the MIXED-background
/// halo: when [`ResolvedFills::outline_underlay`] is `Some` (background band 4
/// — the pet's footprint spans both sides of the light/dark crossover, which a
/// roaming full-body pet does constantly), the silhouette and whisker layers get
/// a compact pale keyline painted underneath the dark one. Neither ink alone
/// clears 3:1 at every luminance a mixed footprint contains; the PAIR does. It
/// costs four extra fills on a cache miss and nothing at all in steady state.
#[must_use]
pub fn bake_pose(pose: PetGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    let mut tile = Tile::new(w, h);
    if w == 0 || h == 0 {
        return tile;
    }
    let xform = PathTransform::fit(w, h);
    for layer in PET_GLYPHS[pose as usize].layers {
        let (col, alpha) = resolve_layer(layer, fills);
        if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker)
            && let Some(underlay) = fills.outline_underlay
        {
            // Halo radius tracks the tile scale but is clamped to a sub-pixel
            // band: any wider and the pale ring reads as a second outline
            // instead of a legibility backstop.
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
struct PetSlot {
    key: PetBakeKey,
    tile: Tile,
    last_used: u64,
}

/// Host-side LRU of exact-size pet tiles (see the module docs for the budgets
/// and for why there is neither a patch strip nor an eyes axis).
///
/// `Default` is a valid empty baker: the first
/// [`begin_frame`](PetBaker::begin_frame) adopts the cell metrics.
#[derive(Default)]
pub struct PetBaker {
    slots: Vec<PetSlot>,
    /// Sum of `slots[..].tile.pixels().len()`, kept incrementally so the
    /// [`MAX_ATLAS_BYTES`] residency bound costs O(1) per admission instead of
    /// a walk.
    bytes: usize,
    cell_w: u16,
    cell_h: u16,
    /// LRU clock, advanced once per [`begin_frame`](PetBaker::begin_frame).
    clock: u64,
    /// Cold bakes still allowed this frame.
    bakes_left: u32,
    /// Monotonic; bumped on every bake and every wholesale clear, so a host that
    /// folds it into a frame fingerprint re-uploads after either.
    version: u64,
}

impl PetBaker {
    /// Per-tick prologue: advance the LRU clock, reset the per-frame cold-bake
    /// budget, and wholesale-clear on a cell-metric change.
    ///
    /// Call this exactly once per PRESENTED frame, not once per pane — the
    /// [`MAX_BAKES_PER_FRAME`] cap is a frame budget, and calling it per pane
    /// would multiply the cap by the pane count.
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        self.clock = self.clock.wrapping_add(1);
        self.bakes_left = MAX_BAKES_PER_FRAME;
        if (cell_w, cell_h) != (self.cell_w, self.cell_h) {
            self.clear();
            self.cell_w = cell_w;
            self.cell_h = cell_h;
        }
    }

    /// Drop every resident tile and bump the version. A no-op (not even a
    /// version bump) when already empty, so a per-frame "effect is off" reset
    /// costs nothing and cannot make a quiescent host look dirty forever.
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

    /// Resident texel bytes (`≤ MAX_ATLAS_BYTES`). Diagnostic: it is how a test
    /// proves the residency bound is enforced rather than merely intended.
    #[doc(hidden)]
    pub fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// The authored aspect (width ÷ height) of a pose, from its asset viewbox.
    ///
    /// Callers size the destination box from THIS rather than from the cell
    /// grid: the pet roster's poses are not all the same shape (a stretch is
    /// long and low, a sit is tall and narrow), and fitting them all into one
    /// box would squash the art the animator drew. Clamped away from zero so a
    /// degenerate asset cannot produce a NaN/∞ destination rect.
    #[must_use]
    pub fn aspect(pose: PetGlyphId) -> f32 {
        (f32::from(PET_GLYPHS[pose as usize].aspect_x1000) / 1000.0).max(0.001)
    }

    /// Look `key` up, baking on a miss when the per-frame budget allows.
    ///
    /// Returns straight-alpha RGBA8, exactly `key.w · key.h · 4` bytes, rows
    /// top-down — ready to hand straight to
    /// [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile) with
    /// [`PetBakeKey::host_id`].
    ///
    /// `None` means "not this frame": the cold-bake budget is spent, or the key
    /// is degenerate (zero-sized, or a single tile larger than the entire
    /// residency budget). This is why the return is an `Option<&[u8]>` and not
    /// the bare `&[u8]` the shape sketch suggested — a deferred bake and an
    /// all-transparent tile are different facts, and a caller that cannot tell
    /// them apart will happily blit a hole into the scene and then never retry.
    /// The correct response to `None` is to keep drawing the previously resolved
    /// pose and ask again next frame.
    pub fn tile(&mut self, key: &PetBakeKey) -> Option<&[u8]> {
        if key.w == 0 || key.h == 0 {
            return None;
        }
        let want = usize::from(key.w) * usize::from(key.h) * 4;
        // A tile that alone exceeds the residency budget is refused rather than
        // admitted by evicting everything else — otherwise one absurd cell
        // metric turns the cache into a one-entry miss machine.
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
        // the tile we are about to hand out can never be the one evicted.
        while !self.slots.is_empty()
            && (self.slots.len() >= MAX_SLOTS || self.bytes + want > MAX_ATLAS_BYTES)
        {
            self.evict_lru();
        }
        self.bytes += tile.pixels().len();
        self.slots.push(PetSlot {
            key: *key,
            tile,
            last_used: self.clock,
        });
        self.version = self.version.wrapping_add(1);
        Some(self.slots[self.slots.len() - 1].tile.pixels())
    }

    /// Drop the least-recently-used slot, lowest index breaking ties — the
    /// tiebreak is what makes eviction (and therefore the whole cache) a
    /// deterministic function of the request sequence.
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
    use crate::pet_glyphs_gen::PET_GLYPH_IDS;

    /// Find a pose by a substring of its authored asset id. Tests address the
    /// roster through the ASSET vocabulary, not through generated variant
    /// spellings, so a codegen rename cannot silently retarget a test.
    fn pose(needle: &str) -> PetGlyphId {
        PET_GLYPH_IDS
            .iter()
            .copied()
            .find(|id| PET_GLYPHS[*id as usize].id.contains(needle))
            .unwrap_or_else(|| panic!("pet roster has no pose matching `{needle}`"))
    }

    /// A distinct key per `coat` index at a plausible working size.
    fn key(coat: u8) -> PetBakeKey {
        PetBakeKey {
            pose: pose("stand"),
            coat,
            iris: 3,
            colors: CatColorKey::default(),
            w: 48,
            h: 28,
        }
    }

    /// The roster is non-empty and every pose has a sane authored aspect — the
    /// precondition every other test in here leans on.
    #[test]
    fn roster_is_populated_and_aspects_are_sane() {
        assert!(!PET_GLYPH_IDS.is_empty(), "pet roster must not be empty");
        assert_eq!(
            PET_GLYPH_IDS.len(),
            PET_GLYPHS.len(),
            "id table and glyph table must agree"
        );
        for &id in PET_GLYPH_IDS {
            let a = PetBaker::aspect(id);
            let authored = PET_GLYPHS[id as usize].aspect_x1000;
            assert!(
                a.is_finite() && a > 0.0,
                "{}: aspect {a} must be finite and positive",
                PET_GLYPHS[id as usize].id
            );
            assert!(authored > 0, "an authored viewbox must not be degenerate");
            assert_eq!(
                a,
                f32::from(authored) / 1000.0,
                "aspect must read the authored viewbox verbatim"
            );
            assert!(
                !PET_GLYPHS[id as usize].layers.is_empty(),
                "{}: a pose with no layers would bake an empty tile",
                PET_GLYPHS[id as usize].id
            );
        }
    }

    /// Determinism: one key ⇒ byte-identical texels, across repeated lookups,
    /// across independent bakers, and against a direct [`bake_pose`].
    #[test]
    fn same_key_bakes_byte_identical_tiles() {
        let k = key(5);
        let mut a = PetBaker::default();
        a.begin_frame(10, 20);
        let first = a.tile(&k).expect("bake").to_vec();
        let second = a.tile(&k).expect("hit").to_vec();
        assert_eq!(first, second, "a cache HIT must return the same texels");

        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        let other = b.tile(&k).expect("bake").to_vec();
        assert_eq!(first, other, "a fresh baker must bake byte-identically");

        let direct = bake_pose(k.pose, &k.fills(), u32::from(k.w), u32::from(k.h));
        assert_eq!(
            first,
            direct.pixels(),
            "the cached bake must equal the direct bake"
        );
    }

    /// EXACT-SIZE, no patch strip: the tile is `w · h · 4` bytes, full stop.
    /// (A cat tile of the same art box would be `(w + PATCH_STRIP) · h · 4`.)
    #[test]
    fn tile_is_exact_size_with_no_patch_strip() {
        let k = key(7);
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        let px = b.tile(&k).expect("bake");
        assert_eq!(
            px.len(),
            usize::from(k.w) * usize::from(k.h) * 4,
            "pet tiles carry no reserved gaze strip"
        );
    }

    /// The cold-bake cap actually caps: exactly [`MAX_BAKES_PER_FRAME`] misses
    /// are served per frame, further misses are deferred, HITS are unaffected,
    /// and the budget resets on the next frame.
    #[test]
    fn bake_rate_is_capped_per_frame() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        for c in 0..MAX_BAKES_PER_FRAME as u8 {
            assert!(b.tile(&key(c)).is_some(), "miss {c} is within budget");
        }
        assert!(
            b.tile(&key(200)).is_none(),
            "a miss past the per-frame budget must be deferred"
        );
        assert!(b.tile(&key(0)).is_some(), "a HIT is not a bake");
        assert_eq!(b.len(), MAX_BAKES_PER_FRAME as usize);
        b.begin_frame(10, 20);
        assert!(b.tile(&key(200)).is_some(), "the budget resets each frame");
    }

    /// LRU eviction holds the slot ceiling and picks the right victim: after
    /// filling every slot and touching key 0, the next admission evicts key 1.
    #[test]
    fn lru_holds_the_slot_ceiling_and_evicts_the_oldest() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        for c in 0..MAX_SLOTS as u8 {
            b.begin_frame(10, 20);
            assert!(b.tile(&key(c)).is_some(), "slot {c} bakes");
            assert!(b.len() <= MAX_SLOTS, "the ceiling is never exceeded");
        }
        assert_eq!(b.len(), MAX_SLOTS, "the cache filled to its ceiling");
        assert!(
            b.resident_bytes() <= MAX_ATLAS_BYTES,
            "residency stays inside the byte budget"
        );
        // Touch key 0 so key 1 becomes the least-recently-used.
        b.begin_frame(10, 20);
        assert!(b.tile(&key(0)).is_some(), "hit refreshes key 0");
        assert!(b.tile(&key(200)).is_some(), "a full cache still admits");
        assert_eq!(b.len(), MAX_SLOTS, "admission evicted exactly one slot");
        assert!(
            b.slots.iter().any(|s| s.key == key(0)),
            "the recently-touched key survived"
        );
        assert!(
            !b.slots.iter().any(|s| s.key == key(1)),
            "the least-recently-used key was evicted"
        );
    }

    /// A cleared baker rebakes — and the clear bumps the version so the host
    /// re-uploads. An already-empty clear is silent (no spurious version bump).
    #[test]
    fn a_cleared_baker_rebakes() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        let before = b.tile(&key(3)).expect("bake").to_vec();
        let v = b.version();
        b.clear();
        assert!(b.is_empty(), "clear drops every slot");
        assert_eq!(b.resident_bytes(), 0);
        assert_eq!(b.version(), v + 1, "clear bumps the version");
        b.clear();
        assert_eq!(b.version(), v + 1, "clearing an empty baker is silent");
        b.begin_frame(10, 20);
        let after = b.tile(&key(3)).expect("rebake");
        assert_eq!(before, after, "the rebake is byte-identical");
    }

    /// A cell-metric change wholesale-clears (a stale-size tile can never be
    /// hit again, so it must not squat on a slot).
    #[test]
    fn metric_change_clears_the_cache() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        assert!(b.tile(&key(0)).is_some());
        let v = b.version();
        b.begin_frame(12, 24);
        assert!(b.is_empty(), "a metric change clears");
        let bumped = b.version();
        assert!(bumped > v, "a metric change bumps the version");
        b.begin_frame(12, 24);
        assert_eq!(b.version(), bumped, "an unchanged metric is a no-op");
        assert!(b.is_empty());
    }

    /// Degenerate keys never bake: a zero dimension, and a tile that alone
    /// exceeds the whole residency budget.
    #[test]
    fn degenerate_keys_are_refused_without_spending_a_bake() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        for bad in [
            PetBakeKey { w: 0, ..key(0) },
            PetBakeKey { h: 0, ..key(0) },
            PetBakeKey {
                w: u16::MAX,
                h: u16::MAX,
                ..key(0)
            },
        ] {
            assert!(b.tile(&bad).is_none(), "{bad:?} must be refused");
        }
        assert!(b.is_empty(), "a refusal admits nothing");
        // The refusals did not consume the frame's bake budget.
        assert!(b.tile(&key(0)).is_some());
        assert!(b.tile(&key(1)).is_some());
        assert!(b.tile(&key(2)).is_none(), "still exactly two bakes/frame");
    }

    /// Distinct genome/context inputs bake distinct tiles — proof the key axes
    /// are actually wired through to the fills rather than being decoration.
    #[test]
    fn key_axes_change_the_texels() {
        let mut b = PetBaker::default();
        let mut bake = |k: &PetBakeKey| {
            b.begin_frame(10, 20);
            b.tile(k).expect("bake").to_vec()
        };
        let base = key(2);
        let coat = bake(&PetBakeKey { coat: 14, ..base });
        let plain = bake(&base);
        assert_ne!(plain, coat, "the coat ramp must move texels");
        let dark = bake(&PetBakeKey {
            colors: CatColorKey {
                accent: 12,
                background: 3,
            },
            ..base
        });
        assert_ne!(plain, dark, "the background band must move texels");
        let other = bake(&PetBakeKey {
            pose: pose("sit"),
            ..base
        });
        assert_ne!(plain, other, "a different pose is a different tile");
        // Distinct keys occupy distinct slots (no aliasing in the LRU).
        assert_eq!(b.len(), 4, "four distinct keys ⇒ four slots");
    }

    /// The `host_id` handed to the shared atlas is stable and key-separating.
    #[test]
    fn host_ids_are_stable_and_distinct() {
        let a = key(1);
        let b = key(2);
        assert_eq!(a.host_id(), key(1).host_id(), "same key ⇒ same host id");
        assert_ne!(
            a.host_id(),
            b.host_id(),
            "distinct keys ⇒ distinct host ids"
        );
        assert_ne!(
            a.host_id(),
            PetBakeKey { w: 49, ..a }.host_id(),
            "size participates in the host id"
        );
        assert!(
            (a.host_id() >> 63) == 1,
            "pet host ids live in the high half"
        );
    }

    /// Smoke test: a baked standing pose is actually a CAT, not an empty box.
    /// The middle horizontal band (the body — above the feet, below the ear
    /// tips) must carry substantial opaque coverage, and the tile must contain
    /// more than one distinct colour.
    #[test]
    fn a_standing_pose_draws_a_cat() {
        let (w, h) = (128u32, 76u32);
        let stand = pose("stand");
        let fills = ResolvedFills::from_indices(9, 4, true);
        let tile = bake_pose(stand, &fills, w, h);
        let px = tile.pixels();
        assert_eq!(px.len(), (w * h * 4) as usize);

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
            coverage > 0.20,
            "the standing pose covers only {:.1}% of its middle band — it did \
             not draw a cat",
            coverage * 100.0
        );

        // More than one colour: an outline plus a coat at minimum.
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
            "a pose should paint an outline, a coat and face ink — got {} \
             distinct opaque colours",
            colors.len()
        );
    }
}

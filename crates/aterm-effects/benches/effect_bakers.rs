// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The SHARED FREE-SPRITE BAKER: `CatBaker` (cat_baker.rs) plus the authored-glyph
// rasterizer under it (`bake_variant_with` -> `paint_glyph`). One atlas is behind
// EVERY free sprite the effects draw — word-cats, the cursor companion, the pet
// and its departures, the dog, Robi's body slices, the animal roster and the
// sing-along notes all resolve through it — and until this file landed not one
// texel of it was measured.
//
// THE PER-FRAME SHAPE, which a bench must reproduce in this exact order or it
// measures early-outs (cat_baker.rs:137 / :233 / :309 / :211):
//
//     baker.set_free_tiles(true);          // ONCE, up front — a flip CLEARS
//     baker.begin_frame(cell_w, cell_h);   // LRU clock += 1, bakes_left = 2
//     baker.get_v4(&key)                   // generated-cat probe   (O(32) scan)
//     baker.host_tile(id, w, h, &rgba)     // host-sprite probe     (O(32) scan)
//     baker.atlas()                        // THE PUBLISH
//
// There is no injected `Instant` anywhere in this module and this bench samples
// no wall clock for state: the baker's only clock is the frame counter that
// `begin_frame` advances, so every workload below advances it by exactly one per
// simulated frame and the whole thing is deterministic.
//
// WHAT EACH WORKLOAD LANDS ON
//
//   off_disabled_reset      The master-off cost. `pipeline::tick` early-outs on
//                           `enabled_any()` and never reaches the baker at all, so
//                           the honest disabled number is ZERO calls; what this
//                           arm times is the residual toggle/reload path that DOES
//                           touch it — `WordDecorations::reset` -> `clear()` on an
//                           already-empty baker (cat_baker.rs:180 early-out) plus
//                           `atlas()` -> None. An upper bound on "effect off".
//
//   off_enabled_no_sprites  Enabled, initialised, nothing to draw: `begin_frame`
//                           + `atlas()` -> None. The cost of concluding there is
//                           nothing to publish.
//
//   idle_settled_shallow /
//   idle_settled_deep       Enabled, a full screen of companions on it, NOTHING
//                           changed. 29 probes per frame (8 word-cats + the cursor
//                           companion via `get_v4`; 16 live note sprites over their
//                           2 shared tiles, the pet, and Robi's 3 body slices via
//                           `host_tile`), zero bakes, `atlas()` returning a bare
//                           `Arc::clone`. This is CB-4: every probe is an O(MAX_SLOTS)
//                           linear scan and it is paid on frames with no bakes at
//                           all. The two arms differ ONLY in where the probed keys
//                           sit in the slot vec (indices 0..14 vs 17..31, forced by
//                           baking throwaway filler keys first), which is what shows
//                           the scan is O(slots) and not O(live sprites).
//
//   bake_only / bake_publish   CB-1, the headline. Identical frames except that
//                           `bake_publish` calls `atlas()`: their difference IS the
//                           `self.rgba.clone()` at cat_baker.rs:217. One host tile
//                           misses per frame (a fresh id — what a pose/context key
//                           change does), so `dirty` is true and the FULL 32-slot
//                           atlas is re-cloned for a one-slot delta. Swept over
//                           cell heights 14/20/40 because the clone is 1024*ch^2
//                           bytes: 196 KB / 400 KB / 1.6 MB, independent of how
//                           little changed.
//
//   frame_companion_changed The realistic composite: "a companion is on screen and
//                           ONE thing changed" — the 29 settled probes, one fresh
//                           bake, one publish. What an animating pet actually costs
//                           per presented frame.
//
//   raster_*                CB-2 and CB-3, the pure rasterizer with no cache around
//                           it. `median_uniform` vs `median_mixed` (and the same for
//                           the densest roster glyph) differ only in the background
//                           BAND: band 4 makes `fills.outline_underlay` `Some`, which
//                           turns every Outline/Whisker layer into FIVE fills at
//                           +/-halo offsets (cat_baker.rs:913-928). That ratio is the
//                           halo tax. `median_happy` / `median_blink` are the CB-3
//                           arms: `EyesFrame::Open` short-circuits `layer_eye_line`
//                           entirely (cat_baker.rs:870), so only these two pay for the
//                           Eye-layer path-segment walk. Read them as a NET, not as
//                           that walk alone — `Happy` also squashes the eye family
//                           (smaller paths to scan) and `Blink` additionally drops the
//                           Iris and CatchLight layers outright, and on this machine
//                           both of those savings outweigh the walk, so the non-Open
//                           bakes come out CHEAPER than the baseline. That is the
//                           honest reading of CB-3's per-bake re-derivation at these
//                           sizes: it is real work, and it is below the noise of the
//                           fills around it. `median_accessory` is the two-glyph
//                           overlay bake.
//
//   fills_lifted/unlifted   `ResolvedFills::from_context`: coat 0 on a dark band
//                           runs the 6-step luminance bisection, coat 5 early-returns.
//                           Priced so it can be ruled out rather than argued about.
//
//   mote_hit_only vs
//   mote_rebake_each_frame  The pets.json PET-01 shape, at the door where it lands.
//                           `host_tile` resolves its id BEFORE it ever reads `rgba`,
//                           so a caller that rasterizes the tile eagerly throws the
//                           pixels away on every cache hit. The pet's own mote baker
//                           is private to `word_decorations`, so this uses
//                           `kitty_sing::bake_note` — the sibling lane's public
//                           raster, and structurally the same work as the Note mote
//                           (two `fill_path`s plus a disc into a small square Tile).
//                           The delta is the per-frame waste.
//
//   frame_thrash            40 distinct v4 keys through 32 slots at 2 bakes/frame,
//                           publishing every frame: permanent LRU eviction, permanent
//                           rebaking, a full clone every frame. Where CB-1 and CB-2
//                           compound.
//
// EVERY WORKLOAD IS GUARDED. `verify_*` runs before timing and asserts the state the
// workload depends on, bounded from BOTH sides where a one-sided assertion would pass
// on an idle engine — "some probe hit" is satisfied by a screen with one sprite on it,
// so the guards assert the EXACT probe count, the EXACT version delta (bakes are
// version bumps, so `version` is an external witness for "a bake really happened" and
// for "no bake happened"), and the exact published byte count. Each guard also prints
// the workload's EMITTED VOLUME — published atlas bytes, blitted tile bytes, painted
// texels — so a later regression in COUNT stays separable from one in per-item COST.
//
// The four traps that would make a naive version of this file measure nothing, all
// of which the guards would catch: (1) `MAX_BAKES_PER_FRAME` is 2 PER `begin_frame`,
// so a warm-up that loops `get_v4` without one bakes twice and early-outs forever;
// (2) `atlas()` only clones when `dirty`, so a publish workload with no bake in it
// times an `Arc::clone`; (3) a key that does not fit (`w + PATCH_STRIP > 4*ch`, or
// `h > 2*ch`) returns `None` having done nothing; (4) `set_free_tiles` wholesale-clears
// on a flip, so it is called once before warm-up and never inside a timed sample.

use std::sync::Arc;

use aterm_effects::cat_baker::{
    BakeKeyV4, CatBaker, CatColorKey, CatTile, EyesFrame, MAX_BAKES_PER_FRAME, MAX_SLOTS,
    PATCH_STRIP, ResolvedFills, bake_variant_with,
};
use aterm_effects::cat_glyphs_gen::{CatGlyphId, GLYPHS, GlyphRole};
use aterm_effects::color_math::relative_luminance;
use aterm_effects::kitty_sing::{self, NoteKind};
use aterm_effects::pet_baker::{PetBakeKey, PetBaker};
use aterm_effects::pet_glyphs_gen::PetGlyphId;
use aterm_effects::robi_baker::{RobiBakeKey, RobiBaker};
use aterm_effects::robi_glyphs_gen::RobiGlyphId;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

// ─────────────────────────── geometry (shipping laws) ───────────────────────────

/// Cell heights the publish is swept over: a small laptop cell, the ship default,
/// and a retina cell. The atlas clone is `1024 * ch^2` bytes at each.
const CELL_HEIGHTS: [u16; 3] = [14, 20, 40];

/// The reference metric every non-swept workload runs at.
const REF_CH: u16 = 20;

/// Uniform background luminance band: `ResolvedFills::outline_underlay` is `None`,
/// so each Outline/Whisker layer is filled exactly once.
const BAND_UNIFORM: u8 = 0;
/// The MIXED class (band 4): the footprint spans the light/dark crossover, so
/// `outline_underlay` is `Some` and every Outline/Whisker layer is filled FIVE
/// times. `pet_baker`'s own docs say a roaming full-body pet is here constantly.
const BAND_MIXED: u8 = 4;

/// The eight ambient word-cat variants a busy screen carries at once
/// (`word_decorations::MAX_CATS`).
const WORD_CAT_VARIANTS: [CatGlyphId; 8] = [
    CatGlyphId::S100,
    CatGlyphId::S101,
    CatGlyphId::S102,
    CatGlyphId::S103,
    CatGlyphId::S104,
    CatGlyphId::S105,
    CatGlyphId::S106,
    CatGlyphId::S107,
];

/// The median-cost roster glyph (a plain head, 124 path segments against the
/// roster median of 125) and the genuinely densest one: `SpecStretch` carries
/// 235 segments over 13 layers, the top of the whole roster — measured with a
/// temporary segment-count pass over `GLYPH_IDS`, printed next to every raster
/// arm below so the choice stays checkable.
const GLYPH_MEDIAN: CatGlyphId = CatGlyphId::S100;
const GLYPH_DENSE: CatGlyphId = CatGlyphId::SpecStretch;

/// aterm cells are about half as wide as they are tall. `cell_w` reaches only
/// `begin_frame`'s metric-change detector; every slot dimension comes from `ch`.
fn cell_w(ch: u16) -> u16 {
    (ch / 2).max(1)
}

/// The ambient word-cat art box: two columns wide, 1.7 rows tall. Fits the slot
/// law (`w + PATCH_STRIP <= 4*ch`, `h <= 2*ch`) at every cell height here.
fn cat_art_box(ch: u16) -> (u16, u16) {
    (2 * ch, (1.7 * f32::from(ch)).round() as u16)
}

/// `WordDecorations::pet_cursor`'s natural pet size, verbatim: `ART_ROWS` rows
/// tall, the authored viewbox aspect wide.
fn pet_art_box(ch: u16) -> (u16, u16) {
    let h = (aterm_effects::kitty_pet::ART_ROWS * f32::from(ch)).round();
    let w = (h * aterm_effects::kitty_pet::ART_ASPECT).round();
    (w as u16, h as u16)
}

/// `robi::body_size_px`'s law at the fullscreen end of its window scaling, where
/// he is biggest and his body needs the most atlas slices.
fn robi_body_box(ch: u16) -> (u16, u16) {
    let h = (aterm_effects::robi::ART_ROWS_MAX * f32::from(ch)).round() as u16;
    let w = ((f32::from(h) * aterm_effects::robi::ART_ASPECT).round() as u16).min(4 * ch);
    (w, h)
}

/// `WordDecorations::pet_mote_side`, verbatim.
fn mote_side(ch: u16) -> u16 {
    ((f32::from(ch.max(4)) * 0.55).round() as u16).max(6)
}

/// The exact byte count `CatBaker::atlas` clones on a dirty publish:
/// `slot_w(4*ch) * MAX_SLOTS * 2*ch * 4`.
fn atlas_bytes(ch: u16) -> usize {
    4 * usize::from(ch) * MAX_SLOTS * 2 * usize::from(ch) * 4
}

/// Which slot a resolved tile landed in, from its atlas y origin.
fn slot_of(tile: CatTile, ch: u16) -> usize {
    usize::from(tile.ay) / (2 * usize::from(ch))
}

fn cat_key(ch: u16, variant: CatGlyphId, coat: u8, band: u8, eyes: EyesFrame) -> BakeKeyV4 {
    let (w, h) = cat_art_box(ch);
    BakeKeyV4 {
        variant,
        accessory: None,
        coat,
        iris: 3,
        colors: CatColorKey {
            accent: 12,
            background: band,
        },
        w,
        h,
        eyes,
    }
}

/// A throwaway key used only to push the real sprites deeper into the slot vec.
/// Distinct from every real key by its `iris` index (a cache axis no real sprite
/// here uses), and pairwise distinct so each one really occupies a slot — a
/// repeated key would HIT instead of baking and the fillers would silently stop
/// filling.
fn filler_key(ch: u16, i: usize) -> BakeKeyV4 {
    let mut key = cat_key(ch, GLYPH_MEDIAN, 5, BAND_UNIFORM, EyesFrame::Open);
    key.iris = 8 + (i as u8 % 8);
    key.coat = 6 + (i as u8 / 8);
    key
}

/// The slot admission law (`get_v4` cat_baker.rs:237/245, `host_tile` :314): a
/// key that violates it returns `None` having done NO work, which is the
/// quietest way for a bench to measure nothing at all.
fn assert_fits(ch: u16, w: u16, h: u16, what: &str) {
    assert!(
        u32::from(w) + u32::from(PATCH_STRIP) <= u32::from(4 * ch),
        "{what}: w={w} + PATCH_STRIP exceeds the {}-texel slot width at ch={ch} — \
         the probe would be refused and this workload would time an early-out",
        4 * ch
    );
    assert!(
        h <= 2 * ch,
        "{what}: h={h} exceeds the {}-row slot band at ch={ch}",
        2 * ch
    );
}

fn packed(c: (f32, f32, f32)) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (q(c.0) << 16) | (q(c.1) << 8) | q(c.2)
}

/// Texels a bake actually painted (alpha != 0) — the emitted-volume counter for
/// the rasterizer arms, the way published bytes are the counter for the atlas.
fn painted_texels(pixels: &[u8]) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[3] != 0)
        .count()
}

/// Total authored path segments in a glyph — the input volume of a bake, the
/// counter that keeps a raster regression in COUNT separable from one in COST.
fn path_segments(id: CatGlyphId) -> usize {
    GLYPHS[id as usize]
        .layers
        .iter()
        .map(|l| l.paths.iter().map(|p| p.len()).sum::<usize>())
        .sum()
}

/// How many Outline/Whisker layers a glyph carries — each one costs FOUR extra
/// `fill_path_fixed` calls on the mixed band, so this is the halo multiplier.
fn halo_layers(id: CatGlyphId) -> usize {
    GLYPHS[id as usize]
        .layers
        .iter()
        .filter(|l| matches!(l.role, GlyphRole::Outline | GlyphRole::Whisker))
        .count()
}

// ───────────────────────────── the resident screen ─────────────────────────────

/// One host-authored sprite as it reaches the shared atlas: an opaque stable id
/// plus the exact-size RGBA the emitter hands to `CatBaker::host_tile`.
#[derive(Clone)]
struct HostSprite {
    id: u64,
    w: u16,
    h: u16,
    rgba: Vec<u8>,
}

/// A settled screen of companions sharing one atlas — everything built once,
/// deterministically, and warmed so every tile below is resident.
struct Screen {
    ch: u16,
    cw: u16,
    baker: CatBaker,
    /// 8 ambient word-cats + the cursor companion, probed through `get_v4`.
    cats: Vec<BakeKeyV4>,
    /// The 2 distinct note tiles the 16 live note sprites share.
    notes: Vec<HostSprite>,
    /// The pet body plus Robi's stacked body slices, probed through `host_tile`.
    bodies: Vec<HostSprite>,
    /// Real pet texels, re-blitted under a FRESH id by the bake workloads: the
    /// payload size and content are the shipping ones, only the id varies.
    pet_body: HostSprite,
}

impl Screen {
    fn new(ch: u16, fillers: usize) -> Self {
        let cw = cell_w(ch);
        let mut baker = CatBaker::default();
        // ONCE, before anything is baked: a flip wholesale-clears, and a clear
        // landing inside a timed sample would measure an `rgba.fill(0)`.
        baker.set_free_tiles(true);

        let mut cats: Vec<BakeKeyV4> = WORD_CAT_VARIANTS
            .iter()
            .enumerate()
            .map(|(i, &v)| cat_key(ch, v, i as u8, BAND_UNIFORM, EyesFrame::Open))
            .collect();
        // The cursor companion: its own glyph, on the mixed band, with a live
        // eyes frame (`word_decorations.rs:3283` bakes `eyes: pose.eyes`).
        cats.push(cat_key(
            ch,
            CatGlyphId::SpecFluffy,
            5,
            BAND_MIXED,
            EyesFrame::Happy,
        ));

        let notes = Vec::from([NoteKind::Eighth, NoteKind::Beamed].map(|kind| {
            let (nw, nh) = kitty_sing::note_nat_size(kind, ch);
            HostSprite {
                id: kitty_sing::note_host_id(kind, nw, nh),
                w: nw,
                h: nh,
                rgba: kitty_sing::bake_note(nw, nh, kind).pixels().to_vec(),
            }
        }));

        // Real pet texels through the real door: PetBaker bakes the pose, its
        // bytes go to the shared atlas under `PetBakeKey::host_id`.
        let (pw, ph) = pet_art_box(ch);
        let pet_key = PetBakeKey {
            pose: PetGlyphId::PetWalk0,
            coat: 5,
            iris: 3,
            colors: CatColorKey {
                accent: 12,
                background: BAND_MIXED,
            },
            w: pw,
            h: ph,
        };
        let mut pet_baker = PetBaker::default();
        pet_baker.begin_frame(cw, ch);
        let pet_body = HostSprite {
            id: pet_key.host_id(),
            w: pw,
            h: ph,
            rgba: pet_baker.tile(&pet_key).expect("pet pose bakes").to_vec(),
        };

        // Robi is taller than the atlas's `2*ch` host-tile ceiling, so the
        // emitter stacks his body as slices — one atlas slot each.
        let (rw, rh) = robi_body_box(ch);
        let robi_key = RobiBakeKey {
            pose: RobiGlyphId::RobiWalk0,
            w: rw,
            h: rh,
        };
        let mut robi_baker = RobiBaker::default();
        robi_baker.begin_frame(cw, ch);
        let robi_rgba = robi_baker
            .tile(&robi_key)
            .expect("robi pose bakes")
            .to_vec();
        let mut bodies = vec![pet_body.clone()];
        let max_slice = 2 * ch;
        let (mut y0, mut n) = (0u16, 0u16);
        while y0 < rh {
            let sh = max_slice.min(rh - y0);
            let from = usize::from(y0) * usize::from(rw) * 4;
            let to = usize::from(y0 + sh) * usize::from(rw) * 4;
            bodies.push(HostSprite {
                id: robi_key.host_id_slice(n),
                w: rw,
                h: sh,
                rgba: robi_rgba[from..to].to_vec(),
            });
            y0 += sh;
            n += 1;
        }

        let mut screen = Screen {
            ch,
            cw,
            baker,
            cats,
            notes,
            bodies,
            pet_body,
        };
        screen.warm(fillers);
        screen
    }

    /// Bake `fillers` throwaway tiles and then every real one, ONE bake per
    /// simulated frame. `MAX_BAKES_PER_FRAME` is a per-`begin_frame` budget, so
    /// a warm-up that skipped `begin_frame` would bake twice and then silently
    /// early-out at `bakes_left == 0` forever, leaving a nearly empty atlas.
    fn warm(&mut self, fillers: usize) {
        let Screen {
            ch,
            cw,
            baker,
            cats,
            notes,
            bodies,
            ..
        } = self;
        let (ch, cw) = (*ch, *cw);
        for i in 0..fillers {
            baker.begin_frame(cw, ch);
            assert!(
                baker.get_v4(&filler_key(ch, i)).is_some(),
                "filler {i} must bake — check the art box still fits the slot"
            );
        }
        for key in cats.iter() {
            baker.begin_frame(cw, ch);
            assert!(
                baker.get_v4(key).is_some(),
                "word-cat key must bake at ch={ch}"
            );
        }
        for s in notes.iter().chain(bodies.iter()) {
            baker.begin_frame(cw, ch);
            assert!(
                baker.host_tile(s.id, s.w, s.h, &s.rgba).is_some(),
                "host sprite {}x{} must bake at ch={ch}",
                s.w,
                s.h
            );
        }
    }

    /// Probes issued by one settled frame: 9 `get_v4` + 16 note sprites + the
    /// pet + Robi's slices.
    fn probes_per_frame(&self) -> usize {
        self.cats.len() + kitty_sing::MAX_NOTES + self.bodies.len()
    }
}

/// What one settled frame's probing found. `hits` is the reachability witness;
/// the slot range is the CB-4 witness that the scan really walks the vec (the
/// two idle arms put the same keys at deliberately different depths).
struct FrameProbe {
    hits: usize,
    min_slot: usize,
    max_slot: usize,
}

/// Every free-sprite family that shares the atlas asking for its resident tile,
/// in the order `pipeline::tick` drives them. No bakes: this is the steady state.
fn probe_settled_frame(
    baker: &mut CatBaker,
    ch: u16,
    cats: &[BakeKeyV4],
    notes: &[HostSprite],
    bodies: &[HostSprite],
) -> FrameProbe {
    let mut out = FrameProbe {
        hits: 0,
        min_slot: usize::MAX,
        max_slot: 0,
    };
    let seen = |out: &mut FrameProbe, tile: Option<CatTile>| {
        if let Some(t) = tile {
            let s = slot_of(t, ch);
            out.hits += 1;
            out.min_slot = out.min_slot.min(s);
            out.max_slot = out.max_slot.max(s);
        }
    };
    for key in cats {
        let t = baker.get_v4(key);
        seen(&mut out, t);
    }
    // 16 live note sprites resolve through 2 shared tiles — the whole shower is
    // two atlas slots, but it is sixteen probes.
    for i in 0..kitty_sing::MAX_NOTES {
        let n = &notes[i % notes.len()];
        let t = baker.host_tile(n.id, n.w, n.h, &n.rgba);
        seen(&mut out, t);
    }
    for b in bodies {
        let t = baker.host_tile(b.id, b.w, b.h, &b.rgba);
        seen(&mut out, t);
    }
    out
}

/// One frame in which exactly one thing changed: a fresh host id (what a pose or
/// local-palette change produces) misses, bakes, and marks the atlas dirty.
fn bake_fresh_tile(baker: &mut CatBaker, id: u64, sprite: &HostSprite) -> Option<CatTile> {
    baker.host_tile(id, sprite.w, sprite.h, &sprite.rgba)
}

// ─────────────────────────────── off / idle ───────────────────────────────

/// The disabled path. `pipeline::tick` never reaches the baker when
/// `enabled_any()` is false, so the true cost is zero calls; this times the
/// toggle/reload path that does touch it, and proves `clear()` takes its
/// already-empty early-out (which is exactly why it costs nothing).
fn verify_off_disabled() {
    let mut baker = CatBaker::default();
    baker.set_free_tiles(true);
    let v0 = baker.version();
    baker.clear();
    baker.clear();
    assert_eq!(
        baker.version(),
        v0,
        "clear() on an empty baker must take its already-empty early-out — a \
         version bump means it really cleared and this arm is timing an rgba.fill(0)"
    );
    assert!(
        baker.atlas().is_none(),
        "a baker that never baked must publish no atlas"
    );
    println!("volume off_disabled_reset: 0 bytes published, 0 probes, 0 bakes");
}

/// Enabled and initialised, nothing on screen. Guarded from both sides: the
/// baker must be in a state where a bake WOULD succeed (else this arm is timing
/// an uninitialised no-op), yet must publish nothing across the frame.
fn verify_off_enabled_no_sprites(ch: u16) {
    let cw = cell_w(ch);
    let mut proof = CatBaker::default();
    proof.set_free_tiles(true);
    proof.begin_frame(cw, ch);
    assert!(
        proof
            .get_v4(&cat_key(ch, GLYPH_MEDIAN, 5, BAND_UNIFORM, EyesFrame::Open))
            .is_some(),
        "begin_frame({cw}, {ch}) must leave the baker able to bake — otherwise \
         the idle arm times a degenerate baker instead of an idle one"
    );

    let mut baker = CatBaker::default();
    baker.set_free_tiles(true);
    baker.begin_frame(cw, ch);
    let v0 = baker.version();
    baker.begin_frame(cw, ch);
    assert_eq!(
        baker.version(),
        v0,
        "a repeated begin_frame at the SAME metrics must not clear"
    );
    assert!(baker.atlas().is_none(), "nothing baked ⇒ nothing published");
    println!("volume off_enabled_no_sprites: 0 bytes published, 0 probes, 0 bakes");
}

/// The settled screen: every probe hits, nothing bakes, the publish is a bare
/// `Arc::clone`. Bounded from both sides — an exact probe count (an idle atlas
/// with one sprite would satisfy "some probe hit"), an exact zero version delta,
/// and the slot depth the arm is supposed to be measuring.
fn verify_idle(screen: &mut Screen, name: &str, min_depth: usize, max_depth: usize) {
    let expect = screen.probes_per_frame();
    let Screen {
        ch,
        cw,
        baker,
        cats,
        notes,
        bodies,
        ..
    } = screen;
    let (ch, cw) = (*ch, *cw);
    baker.begin_frame(cw, ch);
    let v0 = baker.version();
    let probe = probe_settled_frame(baker, ch, cats, notes, bodies);
    assert_eq!(
        probe.hits, expect,
        "{name}: every one of the {expect} settled probes must HIT — a miss means \
         a tile was evicted during warm-up and this arm would be timing bakes"
    );
    assert_eq!(
        baker.version(),
        v0,
        "{name}: a settled frame must bake NOTHING (version is bumped per bake)"
    );
    assert!(
        probe.min_slot >= min_depth && probe.max_slot <= max_depth,
        "{name}: probed slots span {}..={}, expected inside {min_depth}..={max_depth} \
         — the filler keys did not land where this arm needs them",
        probe.min_slot,
        probe.max_slot
    );
    let a1 = baker.atlas().expect("a warm baker publishes an atlas");
    let a2 = baker.atlas().expect("a warm baker publishes an atlas");
    assert!(
        Arc::ptr_eq(&a1, &a2),
        "{name}: with nothing dirty the publish must be a bare Arc::clone"
    );
    assert_eq!(a1.rgba.len(), atlas_bytes(ch), "{name}: atlas size law");
    println!(
        "volume {name}: {} probes/frame over slots {}..={}, 0 bytes cloned \
         (resident atlas {} bytes, re-published by Arc)",
        probe.hits,
        probe.min_slot,
        probe.max_slot,
        a1.rgba.len()
    );
}

// ─────────────────────────── bake + publish (CB-1) ───────────────────────────

/// One fresh-id bake per frame, with and without the publish. Both sides bounded:
/// EXACTLY one version bump per frame (not zero — that would mean the tile was a
/// hit and nothing baked; not two — that would mean the workload is baking more
/// than the "one thing changed" it claims), a fresh Arc every publishing frame,
/// and the exact 1024*ch^2 clone.
fn verify_bake_publish(ch: u16) {
    let mut screen = Screen::new(ch, 0);
    let cw = screen.cw;
    assert_fits(ch, screen.pet_body.w, screen.pet_body.h, "pet body");
    let (aw, ah) = cat_art_box(ch);
    assert_fits(ch, aw, ah, "word-cat art box");
    let mut prev = None;
    let mut bytes = 0usize;
    for i in 0..4u64 {
        screen.baker.begin_frame(cw, ch);
        let v0 = screen.baker.version();
        let tile = bake_fresh_tile(&mut screen.baker, 1_000 + i, &screen.pet_body);
        assert!(
            tile.is_some(),
            "ch={ch}: a fresh host id must resolve — geometry {}x{} against the \
             slot law (w <= {}, h <= {})",
            screen.pet_body.w,
            screen.pet_body.h,
            4 * ch,
            2 * ch
        );
        assert_eq!(
            screen.baker.version(),
            v0 + 1,
            "ch={ch}: a fresh id must bake EXACTLY once per frame"
        );
        let atlas = screen.baker.atlas().expect("a dirty baker publishes");
        if let Some(p) = prev.as_ref() {
            assert!(
                !Arc::ptr_eq(p, &atlas),
                "ch={ch}: a dirty publish must allocate a NEW Arc (that allocation \
                 plus its full rgba.clone() is the number this arm exists to print)"
            );
        }
        assert_eq!(
            atlas.rgba.len(),
            atlas_bytes(ch),
            "ch={ch}: the publish clones the whole 32-slot atlas"
        );
        let painted = painted_texels(&atlas.rgba);
        assert!(
            painted > 0 && painted < atlas.rgba.len() / 4,
            "ch={ch}: expected a partly-occupied atlas, got {painted} painted texels \
             of {} — an empty atlas would make this a memcpy of zeros",
            atlas.rgba.len() / 4
        );
        bytes = atlas.rgba.len();
        prev = Some(atlas);
    }
    let blit = usize::from(screen.pet_body.w) * usize::from(screen.pet_body.h) * 4;
    println!(
        "volume bake_publish/ch{ch}: {bytes} bytes cloned per frame for a {blit}-byte \
         tile change ({:.1}x amplification); at 120 fps that is {:.1} MB/s",
        bytes as f64 / blit as f64,
        bytes as f64 * 120.0 / 1e6
    );
}

/// The realistic composite frame: settled probes + one change + one publish.
fn verify_frame_companion_changed(ch: u16) {
    let mut screen = Screen::new(ch, 0);
    let expect = screen.probes_per_frame();
    let cw = screen.cw;
    for i in 0..4u64 {
        screen.baker.begin_frame(cw, ch);
        let v0 = screen.baker.version();
        let Screen {
            baker,
            cats,
            notes,
            bodies,
            ..
        } = &mut screen;
        let probe = probe_settled_frame(baker, ch, cats, notes, bodies);
        assert_eq!(
            probe.hits, expect,
            "the settled screen must stay resident while the companion churns — \
             a drop here means the fresh bakes are evicting the screen itself"
        );
        let tile = bake_fresh_tile(&mut screen.baker, 2_000 + i, &screen.pet_body);
        assert!(tile.is_some(), "the changed companion tile must bake");
        assert_eq!(
            screen.baker.version(),
            v0 + 1,
            "exactly one thing changed ⇒ exactly one bake"
        );
        let atlas = screen.baker.atlas().expect("a dirty baker publishes");
        assert_eq!(atlas.rgba.len(), atlas_bytes(ch));
    }
    println!(
        "volume frame_companion_changed/ch{ch}: {expect} probes + 1 bake + \
         {} bytes cloned per frame",
        atlas_bytes(ch)
    );
}

// ───────────────────────────── rasterizer (CB-2/CB-3) ─────────────────────────────

/// The halo tax is only real if the mixed band actually turns on the underlay and
/// the glyph actually has Outline/Whisker layers to multiply. Both are asserted,
/// and the two bakes are asserted to differ (identical output would mean the band
/// never reached the painter).
fn verify_raster(ch: u16) {
    let (w, h) = cat_art_box(ch);
    let (w32, h32) = (u32::from(w), u32::from(h));
    for id in [GLYPH_MEDIAN, GLYPH_DENSE] {
        let uniform_fills = ResolvedFills::from_context(
            5,
            3,
            CatColorKey {
                accent: 12,
                background: BAND_UNIFORM,
            },
        );
        let mixed_fills = ResolvedFills::from_context(
            5,
            3,
            CatColorKey {
                accent: 12,
                background: BAND_MIXED,
            },
        );
        assert!(
            uniform_fills.outline_underlay.is_none(),
            "band {BAND_UNIFORM} must NOT arm the halo (else the uniform arm is not a baseline)"
        );
        let underlay = mixed_fills
            .outline_underlay
            .expect("band 4 must arm the pale underlay — the whole point of the mixed arm");
        let halo = halo_layers(id);
        assert!(
            halo >= 1,
            "{id:?}: no Outline/Whisker layer — the mixed arm would bake identically \
             to the uniform one and measure nothing"
        );
        assert!(
            halo < GLYPHS[id as usize].layers.len(),
            "{id:?}: every layer is an outline ({halo}) — that is not a cat, it is a \
             broken roster entry"
        );

        let uniform = bake_variant_with(id, None, &uniform_fills, w32, h32, EyesFrame::Open);
        let mixed = bake_variant_with(id, None, &mixed_fills, w32, h32, EyesFrame::Open);
        assert_ne!(
            uniform.pixels(),
            mixed.pixels(),
            "{id:?}: the halo must change texels"
        );
        let pale = packed(underlay);
        assert!(
            mixed.pixels().as_chunks::<4>().0.iter().any(|px| px[3] > 0
                && (u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2])) == pale),
            "{id:?}: the pale halo ink is absent from the mixed bake — the \
             underlay loop at cat_baker.rs:914 never ran"
        );
        let up = painted_texels(uniform.pixels());
        let mp = painted_texels(mixed.pixels());
        assert!(
            up > 0 && mp > up,
            "{id:?}: expected the halo to paint MORE than the bare glyph ({up} → {mp})"
        );
        println!(
            "volume raster {id:?}: {} path segments over {} layers, tile {}x{} = {} bytes, \
             painted {up} texels uniform / {mp} mixed, {halo} halo layers ⇒ {} extra \
             fill_path_fixed calls per mixed bake",
            path_segments(id),
            GLYPHS[id as usize].layers.len(),
            uniform.width(),
            uniform.height(),
            uniform.pixels().len(),
            halo * 4
        );
    }

    // CB-3: `EyesFrame::Open` short-circuits `layer_eye_line` entirely, so the
    // happy/blink arms are the only ones that pay for the Eye-layer walk.
    //
    // `Happy` is the WITNESS that the walk really ran. The eye-family squash is
    // applied only under `eye_line == Some(_)` (cat_baker.rs:889), and `Happy`
    // changes nothing else about the bake — so a Happy tile that differs from
    // the Open tile can only mean `layer_eye_line` returned a line, i.e. it
    // walked the Eye layer's segments. (`Blink` alone would NOT prove it: it
    // drops the Iris/CatchLight layers whether or not an eye line was found.)
    let fills = ResolvedFills::from_context(
        5,
        3,
        CatColorKey {
            accent: 12,
            background: BAND_UNIFORM,
        },
    );
    let eye_segs: usize = GLYPHS[GLYPH_MEDIAN as usize]
        .layers
        .iter()
        .filter(|l| l.role == GlyphRole::Eye)
        .map(|l| l.paths.iter().map(|p| p.len()).sum::<usize>())
        .sum();
    assert!(
        eye_segs >= 8,
        "{GLYPH_MEDIAN:?} carries only {eye_segs} Eye path segments — layer_eye_line \
         would be a trivial walk and the blink arm would isolate nothing"
    );
    let open = bake_variant_with(GLYPH_MEDIAN, None, &fills, w32, h32, EyesFrame::Open);
    for eyes in [EyesFrame::Happy, EyesFrame::Blink] {
        let shut = bake_variant_with(GLYPH_MEDIAN, None, &fills, w32, h32, eyes);
        assert_ne!(
            open.pixels(),
            shut.pixels(),
            "{eyes:?} must change texels (it also proves the eye-line squash ran)"
        );
    }
    let crowned = bake_variant_with(
        GLYPH_MEDIAN,
        Some(CatGlyphId::AccCrown),
        &fills,
        w32,
        h32,
        EyesFrame::Open,
    );
    assert_ne!(
        open.pixels(),
        crowned.pixels(),
        "the accessory overlay must paint a second glyph"
    );
    println!(
        "volume raster eyes: {eye_segs} Eye path segments walked per non-Open bake; \
         accessory adds {} painted texels",
        painted_texels(crowned.pixels()).saturating_sub(painted_texels(open.pixels()))
    );
}

/// `from_context`'s dark-coat rescue: coat 0 on a dark band runs the 6-step
/// bisection, coat 5 is already above the floor and early-returns. Both arms
/// asserted so neither can silently become the other.
fn verify_fills() {
    let dark = CatColorKey {
        accent: 12,
        background: BAND_UNIFORM,
    };
    let band1 = CatColorKey {
        accent: 12,
        background: 1,
    };
    // The private COAT_MIN_LUM_DARK_BG floor, mirrored here only to assert the
    // two arms land on opposite sides of it.
    const FLOOR: f32 = 0.10;
    let lifted = relative_luminance(packed(ResolvedFills::from_context(0, 3, dark).coat));
    let unlifted_src = relative_luminance(packed(ResolvedFills::from_context(0, 3, band1).coat));
    assert!(
        unlifted_src < FLOOR && lifted >= FLOOR - 0.005,
        "coat 0 must cross the floor when the rescue runs ({unlifted_src:.4} → {lifted:.4}) \
         — if it did not, the lifted arm is timing the early return"
    );
    let already = relative_luminance(packed(ResolvedFills::from_context(5, 3, dark).coat));
    assert!(
        already >= FLOOR,
        "coat 5 must already clear the floor ({already:.4}) so its arm times the \
         early return and the delta is the bisection"
    );
    println!("volume fills_resolve: 0 bytes emitted (colour resolution only)");
}

// ────────────────────────── the discarded-bake tax (PET-01) ──────────────────────────

/// Four live motes, all resident. The guard's job is to prove the re-baked bytes
/// are genuinely thrown away: every probe HITS (so `host_tile` returns before it
/// ever reads `rgba`) and the version does not move (so nothing was written).
fn verify_mote(ch: u16) -> Vec<HostSprite> {
    let side = mote_side(ch);
    let cw = cell_w(ch);
    let motes: Vec<HostSprite> = (0..aterm_effects::kitty_pet::PET_MOTES_MAX)
        .map(|i| {
            let kind = if i % 2 == 0 {
                NoteKind::Eighth
            } else {
                NoteKind::Beamed
            };
            HostSprite {
                id: 0x51_0000 + i as u64,
                w: side,
                h: side,
                rgba: kitty_sing::bake_note(side, side, kind).pixels().to_vec(),
            }
        })
        .collect();
    let painted = painted_texels(&motes[0].rgba);
    assert!(
        painted > 0,
        "the mote raster must actually paint — an empty tile would make the \
         rebake arm a memset and measure nothing"
    );

    let mut baker = CatBaker::default();
    baker.set_free_tiles(true);
    for m in &motes {
        baker.begin_frame(cw, ch);
        assert!(baker.host_tile(m.id, m.w, m.h, &m.rgba).is_some());
    }
    baker.begin_frame(cw, ch);
    let v0 = baker.version();
    for m in &motes {
        assert!(
            baker.host_tile(m.id, m.w, m.h, &m.rgba).is_some(),
            "a resident mote must HIT — on a miss the rebake arm would be honest \
             work instead of the discarded work it exists to price"
        );
    }
    assert_eq!(
        baker.version(),
        v0,
        "the eagerly re-baked bytes must be discarded (no bake, no version bump)"
    );
    println!(
        "volume mote_rebake_each_frame: {} tiles × {side}x{side} = {} bytes \
         rasterized and discarded per frame ({painted} painted texels each)",
        motes.len(),
        motes.len() * usize::from(side) * usize::from(side) * 4
    );
    motes
}

// ──────────────────────────────── thrash ────────────────────────────────

/// More distinct keys than slots, at the full bake budget, publishing every frame.
const THRASH_KEYS: usize = 40;
/// Simulated frames per timed iteration — one full pass through the key cycle
/// and beyond, so eviction is the steady state rather than a warm-up artifact.
const THRASH_FRAMES: usize = 32;

/// The `i`-th key of the thrash cycle. `(variant, coat, iris)` reconstructs `i`
/// for every `i < 48`, so all [`THRASH_KEYS`] keys really are distinct — a
/// repeated one would HIT, and a workload built to miss would quietly settle.
fn thrash_key(ch: u16, i: usize) -> BakeKeyV4 {
    let mut key = cat_key(
        ch,
        WORD_CAT_VARIANTS[i % 8],
        (i % 16) as u8,
        BAND_MIXED,
        EyesFrame::Open,
    );
    key.iris = (i / 16) as u8;
    key
}

/// One thrashing frame: two misses (the whole budget) and a full publish.
fn thrash_frame(baker: &mut CatBaker, ch: u16, cursor: &mut usize) -> usize {
    baker.begin_frame(cell_w(ch), ch);
    let mut baked = 0usize;
    for _ in 0..MAX_BAKES_PER_FRAME {
        let key = thrash_key(ch, *cursor);
        *cursor = (*cursor + 1) % THRASH_KEYS;
        baked += usize::from(baker.get_v4(&key).is_some());
    }
    let atlas = baker.atlas().expect("a thrashing baker always publishes");
    black_box(&atlas);
    baked
}

/// Guarded on the two facts that make this the pathological case: every probe is
/// a MISS that bakes (exactly `MAX_BAKES_PER_FRAME` version bumps per frame), and
/// the cache is genuinely evicting rather than settling.
fn verify_thrash(ch: u16) {
    let mut baker = CatBaker::default();
    baker.set_free_tiles(true);
    let mut cursor = 0usize;
    for _ in 0..THRASH_FRAMES {
        thrash_frame(&mut baker, ch, &mut cursor);
    }
    let v0 = baker.version();
    for _ in 0..THRASH_FRAMES {
        let baked = thrash_frame(&mut baker, ch, &mut cursor);
        assert_eq!(
            baked, MAX_BAKES_PER_FRAME as usize,
            "every thrash probe must resolve — a None means the budget accounting moved"
        );
    }
    assert_eq!(
        baker.version() - v0,
        MAX_BAKES_PER_FRAME as u64 * THRASH_FRAMES as u64,
        "with {THRASH_KEYS} keys over {MAX_SLOTS} slots every probe must MISS and \
         bake; a smaller delta means the cache settled and this arm is timing hits"
    );
    // Eviction witness: spend the frame's whole budget on fresh keys, then ask
    // for a key baked long ago. The hit path runs BEFORE the budget check, so
    // `None` can only mean that key is no longer resident.
    baker.begin_frame(cell_w(ch), ch);
    for i in 0..MAX_BAKES_PER_FRAME as usize {
        let mut key = thrash_key(ch, i);
        key.coat = 100 + i as u8;
        assert!(baker.get_v4(&key).is_some(), "budget bake");
    }
    assert!(
        baker.get_v4(&thrash_key(ch, cursor)).is_none(),
        "the oldest key in the cycle should have been EVICTED — if it is still \
         resident the working set fits and this is not a thrash workload"
    );
    println!(
        "volume frame_thrash/ch{ch}: {} bakes + {} bytes cloned per frame, \
         {THRASH_FRAMES} frames per iteration",
        MAX_BAKES_PER_FRAME,
        atlas_bytes(ch)
    );
}

// ──────────────────────────────── the bench ────────────────────────────────

fn effect_bakers(c: &mut Criterion) {
    println!("\n── effect_bakers: emitted volume per simulated frame (deterministic) ──");
    verify_off_disabled();
    verify_off_enabled_no_sprites(REF_CH);
    verify_raster(REF_CH);
    verify_fills();
    for ch in CELL_HEIGHTS {
        verify_bake_publish(ch);
    }
    verify_frame_companion_changed(REF_CH);
    verify_thrash(REF_CH);
    let motes = verify_mote(REF_CH);

    let mut group = c.benchmark_group("effect_bakers");

    // ── OFF: what a user who never turned this on pays ────────────────────
    group.bench_function("off_disabled_reset", |b| {
        let mut baker = CatBaker::default();
        baker.set_free_tiles(true);
        b.iter(|| {
            baker.set_free_tiles(black_box(true));
            baker.clear();
            black_box(baker.atlas())
        });
    });
    group.bench_function("off_enabled_no_sprites", |b| {
        let mut baker = CatBaker::default();
        baker.set_free_tiles(true);
        baker.begin_frame(cell_w(REF_CH), REF_CH);
        b.iter(|| {
            baker.begin_frame(black_box(cell_w(REF_CH)), black_box(REF_CH));
            black_box(baker.atlas())
        });
    });

    // ── ON but idle: the settled screen, CB-4's O(MAX_SLOTS) probe scan ───
    for (name, fillers, lo, hi) in [
        ("idle_settled_shallow", 0usize, 0usize, MAX_SLOTS - 1),
        ("idle_settled_deep", 17usize, 17usize, MAX_SLOTS - 1),
    ] {
        let mut screen = Screen::new(REF_CH, fillers);
        verify_idle(&mut screen, name, lo, hi);
        group.bench_function(name, |b| {
            let Screen {
                ch,
                cw,
                baker,
                cats,
                notes,
                bodies,
                ..
            } = &mut screen;
            let (ch, cw) = (*ch, *cw);
            b.iter(|| {
                baker.begin_frame(black_box(cw), black_box(ch));
                let probe = probe_settled_frame(baker, ch, cats, notes, bodies);
                black_box(baker.atlas());
                black_box(probe.hits)
            });
        });
    }

    // ── CB-1: one thing changed. The pair's difference IS the atlas clone. ──
    for ch in CELL_HEIGHTS {
        let mut screen = Screen::new(ch, 0);
        group.bench_function(format!("bake_only/ch{ch}"), |b| {
            let cw = screen.cw;
            let mut id = 10_000u64;
            b.iter(|| {
                screen.baker.begin_frame(black_box(cw), black_box(ch));
                id += 1;
                black_box(bake_fresh_tile(&mut screen.baker, id, &screen.pet_body))
            });
        });
    }
    for ch in CELL_HEIGHTS {
        let mut screen = Screen::new(ch, 0);
        group.bench_function(format!("bake_publish/ch{ch}"), |b| {
            let cw = screen.cw;
            let mut id = 20_000u64;
            b.iter(|| {
                screen.baker.begin_frame(black_box(cw), black_box(ch));
                id += 1;
                let tile = bake_fresh_tile(&mut screen.baker, id, &screen.pet_body);
                black_box(tile);
                black_box(screen.baker.atlas())
            });
        });
    }

    // ── the realistic frame: settled screen + one change + publish ────────
    {
        let mut screen = Screen::new(REF_CH, 0);
        group.bench_function(format!("frame_companion_changed/ch{REF_CH}"), |b| {
            let Screen {
                ch,
                cw,
                baker,
                cats,
                notes,
                bodies,
                pet_body,
            } = &mut screen;
            let (ch, cw) = (*ch, *cw);
            let mut id = 30_000u64;
            b.iter(|| {
                baker.begin_frame(black_box(cw), black_box(ch));
                let probe = probe_settled_frame(baker, ch, cats, notes, bodies);
                id += 1;
                black_box(bake_fresh_tile(baker, id, pet_body));
                black_box(baker.atlas());
                black_box(probe.hits)
            });
        });
    }

    // ── CB-2 / CB-3: the rasterizer with no cache around it ───────────────
    {
        let (w, h) = cat_art_box(REF_CH);
        let (w32, h32) = (u32::from(w), u32::from(h));
        let uniform = ResolvedFills::from_context(
            5,
            3,
            CatColorKey {
                accent: 12,
                background: BAND_UNIFORM,
            },
        );
        let mixed = ResolvedFills::from_context(
            5,
            3,
            CatColorKey {
                accent: 12,
                background: BAND_MIXED,
            },
        );
        for (name, id, fills, eyes, acc) in [
            (
                "raster/median_uniform",
                GLYPH_MEDIAN,
                &uniform,
                EyesFrame::Open,
                None,
            ),
            (
                "raster/median_mixed",
                GLYPH_MEDIAN,
                &mixed,
                EyesFrame::Open,
                None,
            ),
            (
                "raster/dense_uniform",
                GLYPH_DENSE,
                &uniform,
                EyesFrame::Open,
                None,
            ),
            (
                "raster/dense_mixed",
                GLYPH_DENSE,
                &mixed,
                EyesFrame::Open,
                None,
            ),
            (
                "raster/median_happy",
                GLYPH_MEDIAN,
                &uniform,
                EyesFrame::Happy,
                None,
            ),
            (
                "raster/median_blink",
                GLYPH_MEDIAN,
                &uniform,
                EyesFrame::Blink,
                None,
            ),
            (
                "raster/median_accessory",
                GLYPH_MEDIAN,
                &uniform,
                EyesFrame::Open,
                Some(CatGlyphId::AccCrown),
            ),
        ] {
            group.bench_function(name, |b| {
                b.iter(|| {
                    bake_variant_with(
                        black_box(id),
                        black_box(acc),
                        black_box(fills),
                        black_box(w32),
                        black_box(h32),
                        black_box(eyes),
                    )
                });
            });
        }
    }

    // ── the colour resolution behind every bake ───────────────────────────
    for (name, coat) in [("fills/unlifted", 5u8), ("fills/lifted", 0u8)] {
        group.bench_function(name, |b| {
            b.iter(|| {
                ResolvedFills::from_context(
                    black_box(coat),
                    black_box(3),
                    black_box(CatColorKey {
                        accent: 12,
                        background: BAND_UNIFORM,
                    }),
                )
            });
        });
    }

    // ── PET-01: the bake whose pixels the cache hit throws away ───────────
    {
        let ch = REF_CH;
        let cw = cell_w(ch);
        let side = mote_side(ch);
        let mut baker = CatBaker::default();
        baker.set_free_tiles(true);
        for m in &motes {
            baker.begin_frame(cw, ch);
            assert!(baker.host_tile(m.id, m.w, m.h, &m.rgba).is_some());
        }
        group.bench_function("mote_hit_only", |b| {
            b.iter(|| {
                baker.begin_frame(black_box(cw), black_box(ch));
                let mut hits = 0usize;
                for m in &motes {
                    hits += usize::from(baker.host_tile(m.id, m.w, m.h, &m.rgba).is_some());
                }
                black_box(hits)
            });
        });
        group.bench_function("mote_rebake_each_frame", |b| {
            b.iter(|| {
                baker.begin_frame(black_box(cw), black_box(ch));
                let mut hits = 0usize;
                for (i, m) in motes.iter().enumerate() {
                    let kind = if i % 2 == 0 {
                        NoteKind::Eighth
                    } else {
                        NoteKind::Beamed
                    };
                    let paint = kitty_sing::bake_note(black_box(side), black_box(side), kind);
                    hits += usize::from(baker.host_tile(m.id, m.w, m.h, paint.pixels()).is_some());
                }
                black_box(hits)
            });
        });
    }

    // ── the compounding case ──────────────────────────────────────────────
    {
        let ch = REF_CH;
        let mut baker = CatBaker::default();
        baker.set_free_tiles(true);
        let mut cursor = 0usize;
        for _ in 0..THRASH_FRAMES {
            thrash_frame(&mut baker, ch, &mut cursor);
        }
        group.bench_function(format!("frame_thrash/ch{ch}"), |b| {
            b.iter(|| {
                let mut baked = 0usize;
                for _ in 0..THRASH_FRAMES {
                    baked += thrash_frame(&mut baker, black_box(ch), &mut cursor);
                }
                black_box(baked)
            });
        });
    }

    group.finish();
}

criterion_group!(benches, effect_bakers);
criterion_main!(benches);

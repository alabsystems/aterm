// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::pet_glyphs_gen::PET_GLYPH_IDS;

/// The cat pose `pet_{needle}`, by exact asset id (tests address the roster by id, not
/// generated variant names).
fn pose(needle: &str) -> PetGlyphId {
    pose_id(&format!("pet_{needle}"))
}

/// The pose with exactly this id, either species.
fn pose_id(id: &str) -> PetGlyphId {
    PET_GLYPH_IDS
        .iter()
        .copied()
        .find(|g| PET_GLYPHS[*g as usize].id == id)
        .unwrap_or_else(|| panic!("pet roster has no pose with id `{id}`"))
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

/// One key bakes byte-identical texels across hits, independent bakers and [`bake_pose`].
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

/// A pet tile is exactly `w · h · 4` bytes, with no patch strip.
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

/// Exactly [`MAX_BAKES_PER_FRAME`] misses per frame; further misses defer, hits are free, and
/// the budget resets each frame.
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

/// Filling every slot and touching key 0, the next admission evicts key 1.
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

/// A clear bumps the version and the rebake is byte-identical; clearing an empty baker is
/// silent.
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

/// A cell-metric change clears the cache and bumps the version; an unchanged one does not.
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

/// A zero dimension, or one tile over the whole residency budget, is refused without
/// spending a bake.
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
    assert!(b.tile(&key(0)).is_some());
    assert!(b.tile(&key(1)).is_some());
    assert!(b.tile(&key(2)).is_none(), "still exactly two bakes/frame");
}

/// Coat, background band and pose each change the texels.
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
    assert_eq!(b.len(), 4, "four distinct keys ⇒ four slots");
}

/// `host_id` is a pure function of the key and lives in the high half (`bit 63 == 1`).
#[test]
fn host_ids_are_stable_and_namespaced() {
    for &pose_id in PET_GLYPH_IDS {
        for coat in 0..16u8 {
            let k = PetBakeKey {
                pose: pose_id,
                coat,
                iris: coat & 7,
                colors: CatColorKey::default(),
                w: 48 + u16::from(coat),
                h: 28 + u16::from(coat & 3),
            };
            let expected = host_tile_id(&[
                k.pose as u64,
                k.coat as u64,
                k.iris as u64,
                k.colors.accent as u64,
                k.colors.background as u64,
                k.w as u64,
                k.h as u64,
            ]);
            assert_eq!(k.host_id(), expected, "same key ⇒ same host id");
            assert_eq!(k.host_id() >> 63, 1, "pet host ids live in the high half");
        }
    }
}

/// At the 1.5× ship size (36 px), for every pose on both grounds, every authored eye subpath
/// keeps its own ≥2×2 block of opaque dark ink (each channel under 64) near its centre, far
/// eyes included, and a pair authored with daylight keeps a non-dark texel between cores.
#[test]
fn face_lod_keeps_a_dark_eye_core_at_ship_size() {
    const H: u32 = 36;
    let mut failures: Vec<String> = Vec::new();
    for &id in PET_GLYPH_IDS {
        let needle = PET_GLYPHS[id as usize].id;
        let w = (H as f32 * PetBaker::aspect(id)).round() as u32;
        for dark_bg in [false, true] {
            let fills = ResolvedFills::from_indices(9, 4, dark_bg);
            let tile = bake_pose(id, &fills, w, H);
            let px = tile.pixels();
            let dark = |x: i32, y: i32| -> bool {
                if x < 0 || y < 0 || x >= w as i32 || y >= H as i32 {
                    return false;
                }
                let i = ((y as u32 * w + x as u32) * 4) as usize;
                px[i + 3] == 255 && px[i] < 64 && px[i + 1] < 64 && px[i + 2] < 64
            };
            for layer in PET_GLYPHS[id as usize].layers {
                if layer.role != GlyphRole::Eye {
                    continue;
                }
                let bounds: Vec<(f32, f32, f32, f32)> =
                    layer.paths.iter().filter_map(|p| path_bounds(p)).collect();
                let max_w_n = bounds.iter().map(|b| b.2 - b.0).fold(0.0, f32::max);
                for b in &bounds {
                    assert!(
                        (b.2 - b.0) >= EYE_DOT_FAR_OCCLUDED_RATIO * max_w_n,
                        "{needle}: an authored eye sits under the \
                         occlusion gate — the roster and \
                         EYE_DOT_FAR_OCCLUDED_RATIO have drifted"
                    );
                }
                let mut cores: Vec<(i32, i32)> = Vec::new();
                for b in &bounds {
                    let cx = ((b.0 + b.2) * 0.5 * w as f32).round() as i32;
                    let cy = ((b.1 + b.3) * 0.5 * H as f32).round() as i32;
                    let found = (cy - 2..=cy).find_map(|by| {
                        (cx - 2..=cx)
                            .find(|&bx| (bx..bx + 2).all(|x| (by..by + 2).all(|y| dark(x, y))))
                            .map(|bx| (bx, by))
                    });
                    match found {
                        Some(origin) => cores.push(origin),
                        None => failures.push(format!(
                            "{needle} (dark_bg={dark_bg}): the eye centred \
                             at ({cx},{cy}) keeps no 2×2 dark core in a \
                             {w}×{H} tile"
                        )),
                    }
                }
                if cores.len() < bounds.len() {
                    // Already failed above; the probes need every core.
                    continue;
                }
                for pi in 0..bounds.len() {
                    for pj in pi + 1..bounds.len() {
                        let gap_n = (bounds[pj].0 - bounds[pi].2).max(bounds[pi].0 - bounds[pj].2);
                        if gap_n * (w as f32) < 1.0 {
                            continue;
                        }
                        let (ax, ay) = cores[pi];
                        let (bx, by) = cores[pj];
                        if (ax - bx).abs() < 2 && (ay - by).abs() < 2 {
                            failures.push(format!(
                                "{needle} (dark_bg={dark_bg}): two authored \
                                 eyes share one dark core at ({ax},{ay}) — \
                                 the pose renders one-eyed"
                            ));
                            continue;
                        }
                        let (l, r) = if ax < bx { (ax, bx) } else { (bx, ax) };
                        let row = (ay + by) / 2 + 1;
                        if l + 2 < r
                            && !(l + 2..r).any(|x| (row - 1..=row + 1).any(|y| !dark(x, y)))
                        {
                            failures.push(format!(
                                "{needle} (dark_bg={dark_bg}): no daylight \
                                 between the eye cores at ({ax},{ay}) and \
                                 ({bx},{by}) — the pair fused into one mask"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "the ship-size face regressed:\n  {}",
        failures.join("\n  ")
    );
}

/// At 36 px the walk keeps catch-light white (from the LOD reflections) but no blush pink or
/// tabby brown; above [`FACE_DETAIL_MIN_H`] all three authored inks are back.
#[test]
fn face_lod_rebuilds_reflections_but_culls_other_charm_ink() {
    let p = pose("walk_0");
    let fills = ResolvedFills::from_indices(9, 4, false);
    let bake_at = |h: u32| {
        let w = (h as f32 * PetBaker::aspect(p)).round() as u32;
        bake_pose(p, &fills, w, h)
    };
    let small = bake_at(36);
    let large = bake_at(96);
    let has = |tile: &Tile, ink: [u8; 3]| {
        tile.pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|px| px[3] == 255 && px[..3] == ink)
    };
    let catch = PET_GLYPHS[p as usize]
        .layers
        .iter()
        .find(|l| l.role == GlyphRole::CatchLight)
        .expect("walk pose authors catch-lights");
    let (catch_col, _) = resolve_layer(catch, &fills);
    let byte = |c: f32| (c * 255.0 + 0.5) as u8;
    let catch_ink = [byte(catch_col.0), byte(catch_col.1), byte(catch_col.2)];
    assert!(
        has(&small, catch_ink),
        "the LOD must rebuild a crisp catch-light at shipping size"
    );
    assert!(
        has(&large, catch_ink),
        "the authored catch-light is missing above the LOD threshold"
    );

    for role in [GlyphRole::Blush, GlyphRole::Pattern] {
        let layer = PET_GLYPHS[p as usize]
            .layers
            .iter()
            .find(|l| l.role == role)
            .expect("walk pose authors both culled charm layers");
        let (col, _) = resolve_layer(layer, &fills);
        let ink = [byte(col.0), byte(col.1), byte(col.2)];
        assert!(
            !has(&small, ink),
            "{role:?} ink survives a 36 px bake — the LOD cull is not \
             running"
        );
        assert!(
            has(&large, ink),
            "{role:?} ink is missing from a 96 px bake — the cull leaked \
             above its threshold"
        );
    }
}

/// A 2× Retina default (cell_h 42, a 71 px tile) bakes the full authored face; at the
/// boundary, [`FACE_DETAIL_MIN_H`] keeps the catch-light and one pixel below culls it.
#[test]
fn retina_scale_bakes_hit_the_identity_path() {
    let mac_2x_h = (crate::kitty_pet::ART_ROWS * 42.0).round() as u32;
    assert!(
        mac_2x_h >= FACE_DETAIL_MIN_H,
        "a 2x Retina default bake ({mac_2x_h} px) must clear the LOD \
         threshold ({FACE_DETAIL_MIN_H} px)"
    );
    let p = pose("stand");
    let fills = ResolvedFills::from_indices(9, 4, false);
    let catch_light = PET_GLYPHS[p as usize]
        .layers
        .iter()
        .find(|l| l.role == GlyphRole::CatchLight)
        .expect("the standing pose authors a catch-light");
    let (col, _) = resolve_layer(catch_light, &fills);
    let byte = |c: f32| (c * 255.0 + 0.5) as u8;
    let ink = [byte(col.0), byte(col.1), byte(col.2)];
    let has_authored_catch_light = |h: u32| {
        let w = (h as f32 * PetBaker::aspect(p)).round() as u32;
        bake_pose_with_reflections(p, &fills, w, h, false)
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|px| px[3] == 255 && px[..3] == ink)
    };
    assert!(
        has_authored_catch_light(72),
        "a 72 px bake (the 2x Retina default) must paint the full \
         authored face, catch-light included"
    );
    assert!(
        has_authored_catch_light(FACE_DETAIL_MIN_H),
        "the first full-face height lost its catch-light — the LOD \
         leaked above its threshold"
    );
    assert!(
        !has_authored_catch_light(FACE_DETAIL_MIN_H - 1),
        "one pixel under the threshold must still bake the LOD face"
    );
}

#[test]
fn lru_eviction_recycles_tile_and_mask_scratch() {
    let mut b = PetBaker::default();
    for c in 0..MAX_SLOTS as u8 {
        b.begin_frame(10, 20);
        let k = PetBakeKey {
            pose: pose(if c % 2 == 0 { "stand" } else { "walk_0" }),
            coat: c,
            ..key(c)
        };
        assert!(b.tile(&k).is_some());
    }
    let mask_ptr = b
        .eye_mask_scratch
        .as_ref()
        .expect("LOD bake retains eye mask scratch")
        .pixels()
        .as_ptr();
    let victim_ptr = b.slots[0].tile.pixels().as_ptr();

    b.begin_frame(10, 20);
    let next = PetBakeKey {
        pose: pose("sit"),
        coat: 99,
        ..key(99)
    };
    let baked = b.tile(&next).expect("evicting bake succeeds").to_vec();
    assert_eq!(
        b.slots.last().unwrap().tile.pixels().as_ptr(),
        victim_ptr,
        "same-size LRU eviction must recycle the evicted slot's Tile buffer"
    );
    assert_eq!(
        b.eye_mask_scratch.as_ref().unwrap().pixels().as_ptr(),
        mask_ptr,
        "LOD bake must reuse eye_mask_scratch across cold bakes"
    );
    assert_eq!(
        baked.as_slice(),
        next.bake().pixels(),
        "recycled Tile bake must be byte-identical to a fresh bake"
    );
    b.clear();
    assert!(b.eye_mask_scratch.is_none());
    let parked_ptr = b
        .tile_scratch
        .as_ref()
        .expect("clear parks one Tile buffer for post-clear rebake")
        .pixels()
        .as_ptr();
    assert_eq!(parked_ptr, victim_ptr);
    b.begin_frame(10, 20);
    assert!(b.tile(&next).is_some());
    assert_eq!(
        b.slots[0].tile.pixels().as_ptr(),
        parked_ptr,
        "first post-clear bake at the same size must reuse the parked Tile buffer"
    );
}

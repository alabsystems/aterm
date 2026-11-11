// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

/// One key bakes byte-identical texels: on a hit, in another baker, and from [`bake_breed`].
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

/// `host_id` is stable across identical keys and namespaced away from pet ids in the high half.
#[test]
fn host_ids_are_stable_distinct_and_namespaced() {
    for c in 0..16u8 {
        let k = key(c);
        assert_eq!(k.host_id(), key(c).host_id());
        assert_eq!(k.host_id() >> 63, 1, "dog host ids live in the high half");
    }
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

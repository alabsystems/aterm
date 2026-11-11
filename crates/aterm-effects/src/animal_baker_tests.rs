// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

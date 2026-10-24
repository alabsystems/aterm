// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the atlas residency: each mutant breaks the law it names. The frame encode's
//! background-buffer slice and the armed tray card ring have no law map of their own; `aterm-models`'
//! `non_vacuity` proves and catches all three.

/// Tier-0 for the atlas residency, glyph and fx: it holds at `Buggy = 0`, and each mutant
/// breaks the law it names. An fx append, or a rewrite of a region the LRU evicted, that skips
/// the reader wait breaks `AtlasRowsSynchronized` (its reader clause) and nothing else; a shared palette texture
/// and a frame that skips its slot wait each break `PaletteRingExclusive` and nothing else.
#[test]
fn metal_atlas_residency_model_catches_each_fx_mutant_by_its_law() {
    use aterm_spec::interp::{violated_invariants, with_buggy};
    let m = aterm_models_render::metal_atlas_residency_model();
    assert!(violated_invariants(&with_buggy(&m, 0)).is_empty());
    for (buggy, law) in [
        (2, "AtlasRowsSynchronized"),
        (5, "AtlasRowsSynchronized"),
        (3, "PaletteRingExclusive"),
        (4, "PaletteRingExclusive"),
    ] {
        assert_eq!(
            violated_invariants(&with_buggy(&m, buggy))
                .into_iter()
                .collect::<Vec<_>>(),
            vec![law],
            "Buggy = {buggy}"
        );
    }
}

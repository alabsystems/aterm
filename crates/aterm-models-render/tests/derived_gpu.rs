// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the frame encode's background-buffer slice, the atlas residency and the armed tray
//! card ring: each proves at `Buggy = 0` and its mutants are caught.

#[test]
fn gpu_encode_model_never_slices_an_empty_buffer() {
    let m = aterm_models_render::gpu_encode_model();
    assert_eq!(
        aterm_spec::verify::sweep_faults(std::slice::from_ref(&m)),
        Vec::<String>::new()
    );
    aterm_spec::verify::prove_and_catch_scalar(&m, "GpuEncode: NeverSliceEmpty");
}

#[test]
fn metal_tray_card_ring_model_rewrites_no_slot_under_a_reader_and_waits_for_no_other() {
    let m = aterm_models_render::metal_tray_card_ring_model();
    assert_eq!(
        aterm_spec::verify::sweep_faults(std::slice::from_ref(&m)),
        Vec::<String>::new()
    );
    aterm_spec::verify::prove_and_catch_scalar(&m, "MetalTrayCardRing");
}

/// Tier-0 for the atlas residency, glyph and fx: it holds at `Buggy = 0`, and each mutant
/// breaks the law it names. An fx append that skips the reader wait breaks
/// `AtlasRowsSynchronized` (its reader clause) and nothing else; a shared palette texture
/// and a frame that skips its slot wait each break `PaletteRingExclusive` and nothing else.
#[test]
fn metal_atlas_residency_model_catches_each_fx_mutant_by_its_law() {
    use aterm_spec::interp::{violated_invariants, with_buggy};
    let m = aterm_models_render::metal_atlas_residency_model();
    assert_eq!(
        aterm_spec::verify::sweep_faults(std::slice::from_ref(&m)),
        Vec::<String>::new()
    );
    assert!(violated_invariants(&with_buggy(&m, 0)).is_empty());
    for (buggy, law) in [
        (2, "AtlasRowsSynchronized"),
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
    aterm_spec::verify::prove_and_catch_scalar(&m, "MetalAtlasResidency");
}

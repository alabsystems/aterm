// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the two small kernels (RLE compaction, ASCII grapheme fast path): each proves at `Buggy = 0`, every action fires, and the mutant is caught.

use aterm_models_core::{ascii_grapheme_fast_path_model, rle_local_compact_model};

#[test]
fn rle_local_compact_model_proves_and_catches_mutant() {
    let m = rle_local_compact_model();
    let states = aterm_spec::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        aterm_spec::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    aterm_spec::verify::prove_and_catch_scalar(&m, "RleLocalCompactModel");
}

#[test]
fn ascii_grapheme_fast_path_model_proves_and_catches_mutant() {
    let m = ascii_grapheme_fast_path_model();
    let states = aterm_spec::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        aterm_spec::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    aterm_spec::verify::prove_and_catch_scalar(&m, "AsciiGraphemeFastPath");
}

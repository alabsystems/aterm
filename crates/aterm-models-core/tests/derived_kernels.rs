// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the four small kernels (RLE compaction, ASCII grapheme fast path, `OnceCell` gate protocol, `cfg_if!` expansion): each proves at `Buggy = 0`, every action fires, and the mutant is caught.

use aterm_models_core::{
    ascii_grapheme_fast_path_model, cfg_if_expansion_model, once_cell_gate_protocol_model,
    rle_local_compact_model,
};

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
}

#[test]
fn once_cell_gate_protocol_model_proves_and_catches_mutant() {
    let m = once_cell_gate_protocol_model();
    let states = aterm_spec::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        aterm_spec::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    assert!(
        aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&m, 2)).is_err(),
        "Buggy=2 (returning un-reread candidate) must violate ReturnedValueMatchesCell"
    );
}

#[test]
fn cfg_if_expansion_model_proves_and_catches_mutants() {
    let m = cfg_if_expansion_model();
    let states = aterm_spec::interp::prove_and_catch(&m);
    assert!(states > 1, "must explore multiple states: {states}");
    assert_eq!(
        aterm_spec::interp::fired_actions(&m).len(),
        m.actions.len(),
        "all actions must fire at Buggy=0"
    );
    assert!(
        aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&m, 2)).is_err(),
        "Buggy=2 (un-grouped arms) must violate DeadArmFullyAbsent"
    );
}

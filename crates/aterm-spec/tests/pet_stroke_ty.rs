// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0: the bounded stroke model proves and catches each interaction law.

use aterm_spec::{derive::pet_stroke_detector_model, verify};

#[test]
fn pet_stroke_model_proves_and_catches_stationary_and_cooldown_bypasses() {
    let model = pet_stroke_detector_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|m| m.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, model.name);
    // Each behavior claim must catch its own mutant, independently of the
    // earlier invariant that a whole-model check would stop at.
    for law in [
        "OnlyCompleteStrokesEarn",
        "CooldownCannotBeBypassed",
        "ResetForgetsContact",
    ] {
        let mut one = model.clone();
        one.invariants.retain(|invariant| invariant.name == law);
        assert_eq!(one.invariants.len(), 1);
        verify::prove_and_catch_scalar(&one, law);
    }
}

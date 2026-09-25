// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_pass_stamps_model, interp, verify};

/// The pass record proves over its bounded space (interpreter, and `ty` where installed)
/// and `Buggy = 1` — the record as it was until 2026-09-23 — is caught. Each historical
/// defect is then replayed on its own, so every invariant is shown to catch the bug it was
/// written for: PK-3's cache-served success healing a sibling's ladder and standing a
/// waiter down, and PK-4's vendor-door write read as a failed pass and as a pass
/// attempted. The healthy model takes the same traces with every invariant holding.
#[test]
fn the_pass_record_proves_and_catches_both_historical_defects() {
    let model = atpkg_pass_stamps_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the pass record must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "atpkg pass stamps");

    let buggy = interp::with_buggy(&model, 1);
    let replay = |m: &aterm_spec::derive::Model, trace: &[&str]| {
        let mut state = m.init_state();
        for action in trace {
            assert!(m.fire(action, &mut state), "{action}: {state:?}");
        }
        state
    };
    let holds =
        |m: &aterm_spec::derive::Model, state, invariant: &str| m.check_invariant(invariant, state);

    // PK-3: this lane's pass failed, then a SIBLING's pass was served from the cache.
    let heal = ["OwnPassUnreached", "PassUnreached"];
    let state = replay(&buggy, &heal);
    assert!(!holds(&buggy, &state, "HealedOnlyByAReachedPass"));
    let state = replay(&model, &heal);
    assert!(holds(&model, &state, "HealedOnlyByAReachedPass"));
    // …and a waiter stood down behind that cache-served pass.
    let stand_down = ["Wait", "PassUnreached"];
    let state = replay(&buggy, &stand_down);
    assert!(!holds(&buggy, &state, "StandDownOnlyBehindAReachedPass"));
    let state = replay(&model, &stand_down);
    assert!(holds(&model, &state, "StandDownOnlyBehindAReachedPass"));

    // PK-4: a pass reached the index, then a vendor door wrote a row.
    let door = ["PassReached", "OtherWrite"];
    let state = replay(&buggy, &door);
    assert!(!holds(&buggy, &state, "FailedMeansTheLastPassFailed"));
    assert!(!holds(&buggy, &state, "SpacingCountsOnlyFullPasses"));
    // The HEALED reader is not what PK-4 breaks: isolation of the two defects.
    assert!(holds(&buggy, &state, "HealedOnlyByAReachedPass"));
    let state = replay(&model, &door);
    assert!(holds(&model, &state, "FailedMeansTheLastPassFailed"));
    assert!(holds(&model, &state, "SpacingCountsOnlyFullPasses"));
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_pass_stamps_model, interp, verify};

/// The pass record proves and `Buggy = 1` is caught. Each defect is replayed alone, so every
/// invariant is shown catching its own bug: a cache-served success healing a sibling's ladder
/// and standing a waiter down, and a vendor-door write read as a failed pass and as a pass
/// attempted. The healthy model holds every invariant on the same traces.
#[test]
fn the_pass_record_proves_and_catches_both_historical_defects() {
    let model = atpkg_pass_stamps_model();
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

    // This lane's pass failed, then a sibling's pass was served from the cache.
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

    // A pass reached the index, then a vendor door wrote a row.
    let door = ["PassReached", "OtherWrite"];
    let state = replay(&buggy, &door);
    assert!(!holds(&buggy, &state, "FailedMeansTheLastPassFailed"));
    assert!(!holds(&buggy, &state, "SpacingCountsOnlyFullPasses"));
    // The healed reader is untouched by the vendor-door defect: the two defects are isolated.
    assert!(holds(&buggy, &state, "HealedOnlyByAReachedPass"));
    let state = replay(&model, &door);
    assert!(holds(&model, &state, "FailedMeansTheLastPassFailed"));
    assert!(holds(&model, &state, "SpacingCountsOnlyFullPasses"));
}

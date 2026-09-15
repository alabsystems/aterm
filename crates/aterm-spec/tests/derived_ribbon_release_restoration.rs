// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::ribbon_release_restoration_model, verify};

#[test]
fn ribbon_release_restoration_proves_and_catches_partial_revival() {
    let model = ribbon_release_restoration_model();
    verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn failed_observations_retain_custody_until_a_complete_return() {
    let model = ribbon_release_restoration_model();
    for observation in [
        "ObserveUnsampled",
        "ObservePartial",
        "ObserveMissing",
        "ObserveChanged",
    ] {
        let mut state = model.init_state();
        assert!(model.fire("Release", &mut state));
        assert!(model.fire(observation, &mut state));
        let before = state.clone();
        assert!(model.fire("TryRestore", &mut state));
        assert_eq!(state, before, "{observation} cannot restore or revoke");
        assert!(model.fire("ObserveFull", &mut state));
        assert!(model.fire("TryRestore", &mut state));
        assert_eq!(state["phase"], 2);
        assert_eq!(state["clock"], 1);
        assert_eq!(state["custody"], 0);
        assert_eq!(state["cells"], 2);
        assert_eq!(state["births"], 2);
        assert!(!model.action_enabled("TryRestore", &state));
    }
}

#[test]
fn independent_abandonment_and_identity_changes_revoke_restoration() {
    let model = ribbon_release_restoration_model();
    for barrier in ["Abandon", "Retire", "Replace"] {
        let mut state = model.init_state();
        assert!(model.fire("Release", &mut state));
        assert!(model.fire(barrier, &mut state));
        assert!(model.fire("ObserveFull", &mut state));
        let before = state.clone();
        assert!(model.fire("TryRestore", &mut state));
        assert_eq!(state, before, "{barrier} permanently revokes custody");
    }
    let mut already_abandoned = model.init_state();
    assert!(model.fire("Abandon", &mut already_abandoned));
    assert!(model.fire("Release", &mut already_abandoned));
    assert!(model.fire("ObserveFull", &mut already_abandoned));
    assert!(model.fire("TryRestore", &mut already_abandoned));
    assert_eq!(already_abandoned["phase"], 1);
    assert_eq!(already_abandoned["custody"], 0);
}

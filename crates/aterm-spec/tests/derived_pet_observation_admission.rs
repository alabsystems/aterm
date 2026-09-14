// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::console_observation_admission_model, verify};

#[test]
fn pet_observation_admission_proves_and_catches_incoherent_motion() {
    let model = console_observation_admission_model();
    verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn pet_observation_admission_preserves_unfed_motion_and_recovers_existing_flight() {
    let model = console_observation_admission_model();
    let mut state = model.init_state();
    assert!(model.fire("TickLaunch", &mut state), "unfed legacy host");
    assert!(model.fire("TickAdvance", &mut state));
    assert!(model.fire("ObserveIncoherent", &mut state));
    assert!(!model.action_enabled("TickAdvance", &state));
    assert!(!model.action_enabled("TickRetire", &state));
    let pending = state["legacy"];
    assert!(model.fire("TickStill", &mut state));
    assert_eq!(state["legacy"], pending, "suspension retains the flight");
    assert_eq!(state["moved"], 0);
    assert!(model.fire("ObserveCoherent", &mut state));
    assert!(model.fire("TickAdvance", &mut state), "coherent recovery");
    assert!(model.fire("TickRetire", &mut state));
    assert!(model.fire("ObserveIncoherent", &mut state));
    assert!(!model.action_enabled("TickLaunch", &state));
    assert!(model.fire("ObserveCoherent", &mut state));
    assert!(model.action_enabled("TickLaunch", &state));
}

#[test]
fn pet_observation_admission_rejects_launch_and_advance_as_separate_mutants() {
    let model = console_observation_admission_model();
    for in_flight in [false, true] {
        let mut state = model.init_state();
        if in_flight {
            assert!(model.fire("TickLaunch", &mut state));
        }
        assert!(model.fire("ObserveIncoherent", &mut state));
        let mut mutant = state.clone();
        mutant.insert("prior_legacy", state["legacy"]);
        mutant.insert("legacy", 1);
        mutant.insert("moved", i64::from(in_flight));
        mutant.insert("ticked", 1);
        assert!(!model.successors("TickStill", &state).contains(&mutant));
        assert!(!model.check_invariant("IncoherentObservationFreezesMotion", &mutant));
    }
}

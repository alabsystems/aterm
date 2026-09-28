// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{
    derive::{
        atpkg_index_probe_completion_cadence_model, atpkg_index_probe_cooldown_model,
        atpkg_index_shared_handoff_model, atpkg_index_successor_selection_model,
        atpkg_index_wake_highwater_model,
    },
    interp, verify,
};

#[test]
fn a_suppressed_host_rechecks_when_the_shared_stamp_expires() {
    let model = atpkg_index_shared_handoff_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg index shared handoff");

    let buggy = interp::with_buggy(&model, 1);
    let mut fixed = model.init_state();
    let mut old = buggy.init_state();
    assert!(model.fire("Stamp", &mut fixed));
    assert!(buggy.fire("Stamp", &mut old));
    for _ in 0..5 {
        assert!(model.fire("TickBeforeHandoff", &mut fixed));
        assert!(buggy.fire("TickBeforeHandoff", &mut old));
    }
    for action in ["Handoff", "TickAfterHandoff", "AtExpiry"] {
        assert!(model.fire(action, &mut fixed));
        assert!(buggy.fire(action, &mut old));
    }
    assert_eq!(fixed["requests"], 1);
    assert_eq!(old["requests"], 0);
    assert!(!buggy.check_invariant("NoSecondLocalCooldown", &old));
}

#[test]
fn the_next_probe_uses_the_shared_stamps_completion_clock() {
    let model = atpkg_index_probe_completion_cadence_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg index probe completion cadence");

    let buggy = interp::with_buggy(&model, 1);
    let mut fixed = model.init_state();
    let mut old = buggy.init_state();
    for action in ["Start", "NetworkTick", "Complete"] {
        assert!(model.fire(action, &mut fixed));
        assert!(buggy.fire(action, &mut old));
    }
    for _ in 0..5 {
        assert!(model.fire("Tick", &mut fixed));
        assert!(buggy.fire("Tick", &mut old));
    }
    assert!(model.fire("LocalCheck", &mut fixed));
    assert!(buggy.fire("LocalCheck", &mut old));
    assert_eq!(fixed["phase"], 2, "the new probe still waits for the stamp");
    assert_eq!(old["phase"], 3, "the old start clock probes too soon");
    assert!(!buggy.check_invariant("NoProbeBeforeSharedStampExpires", &old));
    assert!(model.fire("Tick", &mut fixed));
    assert!(model.fire("DueCheck", &mut fixed));
    assert_eq!(fixed["phase"], 3, "the next tick starts the probe");
}

#[test]
fn cross_process_probe_cooldown_proves_and_catches_duplicate_ranges() {
    let model = atpkg_index_probe_cooldown_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the cross-process probe must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "atpkg index probe cooldown");

    let mut healthy = model.init_state();
    for action in ["Acquire", "ProbeMissing", "Release", "Acquire"] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
    }
    assert!(model.action_enabled("SkipFresh", &healthy));
    assert!(!model.action_enabled("ProbeMissing", &healthy));

    let buggy = interp::with_buggy(&model, 1);
    let mut duplicate = buggy.init_state();
    for action in [
        "Acquire",
        "ProbeMissing",
        "Release",
        "Acquire",
        "ProbeMissing",
    ] {
        assert!(
            buggy.fire(action, &mut duplicate),
            "{action}: {duplicate:?}"
        );
    }
    assert!(!buggy.check_invariant("NoDuplicateRangeInsideCooldown", &duplicate));

    let mut two_owners = buggy.init_state();
    assert!(buggy.fire("Acquire", &mut two_owners));
    assert!(buggy.fire("Acquire", &mut two_owners));
    assert!(!buggy.check_invariant("OneOwner", &two_owners));

    let mut short_error = buggy.init_state();
    assert!(buggy.fire("Acquire", &mut short_error));
    assert!(buggy.fire("ProbeError", &mut short_error));
    assert!(!buggy.check_invariant("StampMatchesOutcome", &short_error));

    let mut short_published = buggy.init_state();
    assert!(buggy.fire("Acquire", &mut short_published));
    assert!(buggy.fire("ProbePublished", &mut short_published));
    assert!(!buggy.check_invariant("StampMatchesOutcome", &short_published));
}

#[test]
fn independent_hints_prove_highest_and_no_false_missing() {
    let model = atpkg_index_successor_selection_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg successor selection");

    let mut healthy = model.init_state();
    let buggy = interp::with_buggy(&model, 1);
    let mut first_hit = buggy.init_state();
    for action in ["ObserveLow", "ObserveHigh", "Choose"] {
        assert!(model.fire(action, &mut healthy));
        assert!(buggy.fire(action, &mut first_hit));
    }
    assert_eq!(healthy["chosen"], 5);
    assert_eq!(first_hit["chosen"], 3);
    assert!(!buggy.check_invariant("BestAvailableHint", &first_hit));

    let mut partial = buggy.init_state();
    for action in ["ObserveMissing", "ObserveDeferred", "Choose"] {
        assert!(buggy.fire(action, &mut partial));
    }
    assert_eq!(partial["chosen"], 1);
    assert!(!buggy.check_invariant("BestAvailableHint", &partial));
}

#[test]
fn failed_index_highwater_proves_no_older_replay() {
    let model = atpkg_index_wake_highwater_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg index wake highwater");

    let mut healthy = model.init_state();
    let buggy = interp::with_buggy(&model, 1);
    let mut older_replay = buggy.init_state();
    for action in ["FailMiddle", "HintLow"] {
        assert!(model.fire(action, &mut healthy));
        assert!(buggy.fire(action, &mut older_replay));
    }
    assert_eq!((healthy["unlanded"], healthy["wake"]), (2, 0));
    assert!(!buggy.check_invariant("NoReplayOfOlderHint", &older_replay));

    let mut lowered = buggy.init_state();
    for action in ["FailHigh", "FailLow"] {
        assert!(buggy.fire(action, &mut lowered));
    }
    assert!(!buggy.check_invariant("FailureMarkNeverDowngrades", &lowered));
}

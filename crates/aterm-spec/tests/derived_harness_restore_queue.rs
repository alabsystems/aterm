// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::{derive::harness_restored_first_attempt_model, verify};

#[test]
fn restored_tabs_get_first_attempts_before_a_retry() {
    let model = harness_restored_first_attempt_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the restore queue must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "restored tabs' first attempts precede retry");
}

#[test]
fn network_probe_shutdown_cannot_notify_before_the_waiter_parks() {
    let model = aterm_spec::derive::netprobe_shutdown_park_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "network probe shutdown cannot lose its wake");
}

/// HARNESS WORK IN FLIGHT AT A SEAMLESS UPDATE'S COMMIT (round four of the
/// 2026-09 update robustness work, plan item 7): a restored agent the park
/// caught mid-step is carried to the successor and relaunched there, no step
/// starts while the terminal is parked, and a restart record the outgoing
/// instance left in flight in a shell tab is swept at the Commit. NEGATIVE
/// CONTROLS: the three mutants that shipped, each caught by its invariant.
#[test]
fn harness_work_in_flight_at_a_commit_is_carried_never_dropped() {
    use aterm_spec::derive::harness_restored_carry_model;
    let model = harness_restored_carry_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the restored carry must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness work in flight at a Commit");

    // The park lands mid-step: what it froze includes the agent whose step is
    // running, whatever that step ends in, and the successor relaunches it.
    let mut state = model.init_state();
    for action in [
        "StepStart",
        "Park",
        "StepRetries",
        "Commit",
        "SuccRelaunches",
        "SweepCarriesOn",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!((state["resolved"], state["restart"]), (1, 0));
    // A step the park caught that relaunched after all is carried too: the
    // successor's step is idempotent, and nothing is lost either way.
    let mut state = model.init_state();
    for action in ["StepStart", "Park", "StepResolves", "Commit"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(state["succ"], 1, "carried as the park froze it");
    // No step starts while parked; a rollback lets the queue run again.
    let mut parked = model.init_state();
    assert!(model.fire("Park", &mut parked));
    assert!(!model.action_enabled("StepStart", &parked));
    assert!(model.fire("Rollback", &mut parked));
    assert!(model.action_enabled("StepStart", &parked));

    // Each mutant, alone, breaks exactly its claim.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut dropped = buggy.init_state();
    for action in ["ParkDropsQueue", "Commit"] {
        assert!(buggy.fire(action, &mut dropped), "{action}: {dropped:?}");
    }
    assert!(!buggy.check_invariant("NoSilentLoss", &dropped));
    let mut late = buggy.init_state();
    for action in ["Park", "StepStartWhileParked"] {
        assert!(buggy.fire(action, &mut late), "{action}: {late:?}");
    }
    assert!(!buggy.check_invariant("NoStepWhileParked", &late));
    let mut stranded = buggy.init_state();
    for action in ["Park", "CommitWithoutSweep"] {
        assert!(buggy.fire(action, &mut stranded), "{action}: {stranded:?}");
    }
    assert!(!buggy.check_invariant("NoStrandedRestart", &stranded));
}

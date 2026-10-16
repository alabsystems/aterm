// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateSuccessorAttach` (warm successor P3, the parallel
//! attach): where an update's successor rebuilds its layout, and what its
//! adoption proof waits for. Tier-1 is aterm-gui's `restore_pass_conformance`.

use aterm_spec::{
    derive::{native_update_successor_attach_liveness, native_update_successor_attach_model},
    interp, verify,
};

#[test]
fn the_handoff_rebuilds_in_the_attach_pass_and_proves_only_when_every_window_painted() {
    let model = native_update_successor_attach_model();
    verify::prove_and_catch_scalar(&model, "successor attach");
    let done = |s: &interp::State| {
        s["proved"] == 1
            || (s["lane"] == 2
                && s["queued"] == 0
                && s["painted0"] == 1
                && s["painted_extras"] == s["extras"])
    };
    assert!(
        interp::find_deadlock(&model, done).is_none(),
        "every healthy boot proves (handoff) or ends rebuilt and painted (cold)"
    );

    // P3's point: on the handoff lane windows 1..N exist before window 0 has
    // presented, so all of them wait for their first drawable together.
    let mut early = model.init_state();
    for action in ["Widen", "StartHandoff", "Attach0", "RebuildAtAttach"] {
        assert!(model.fire(action, &mut early), "{action}: {early:?}");
    }
    assert_eq!((early["extras"], early["painted0"]), (1, 0), "{early:?}");
    assert!(!model.action_enabled("Prove", &early), "{early:?}");
    for action in ["Present0", "LeaveResume"] {
        assert!(model.fire(action, &mut early), "{action}: {early:?}");
    }
    assert!(
        !model.action_enabled("Prove", &early),
        "window 0 alone proves nothing while an extra window is unpainted"
    );
    assert!(model.fire("PresentExtras", &mut early));
    assert!(model.fire("Prove", &mut early));

    // RESTORE-1 stays: a cold launch leaves the attach pass with the rebuild
    // owed, and rebuilds only at a park after a present.
    let mut cold = model.init_state();
    for action in ["Widen", "StartCold", "Attach0"] {
        assert!(model.fire(action, &mut cold), "{action}: {cold:?}");
    }
    assert!(!model.action_enabled("RebuildAtAttach", &cold), "{cold:?}");
    assert!(model.fire("LeaveResume", &mut cold));
    assert!(!model.action_enabled("RebuildAtPark", &cold), "{cold:?}");
    assert!(model.fire("Present0", &mut cold));
    assert!(model.fire("RebuildAtPark", &mut cold));
    assert!(
        !model.action_enabled("Prove", &cold),
        "a cold launch owes no proof"
    );

    // THE COLD DRAIN: the park pass may run a slice, which leaves the rest of
    // the rebuild owed to the next park; a degraded handoff rebuilds whole.
    let mut drain = model.init_state();
    for action in [
        "Widen",
        "StartCold",
        "Attach0",
        "LeaveResume",
        "Present0",
        "RebuildSliceAtPark",
        "RebuildSliceAtPark",
    ] {
        assert!(model.fire(action, &mut drain), "{action}: {drain:?}");
    }
    assert_eq!((drain["queued"], drain["draining"]), (1, 1), "{drain:?}");
    assert!(model.fire("RebuildAtPark", &mut drain));
    assert_eq!((drain["queued"], drain["draining"]), (0, 0), "{drain:?}");
    let mut degraded = model.init_state();
    for action in ["StartDegraded", "Attach0", "LeaveResume", "Present0"] {
        assert!(model.fire(action, &mut degraded), "{action}: {degraded:?}");
    }
    assert!(
        !model.action_enabled("RebuildSliceAtPark", &degraded),
        "{degraded:?}"
    );
    assert!(model.action_enabled("RebuildAtPark", &degraded));

    // NEGATIVE CONTROLS: each invariant's historical or tempting defect.
    let buggy = interp::with_buggy(&model, 1);
    let mut cold_early = buggy.init_state();
    for action in ["Widen", "StartCold", "Attach0", "RebuildAtAttachCold"] {
        assert!(
            buggy.fire(action, &mut cold_early),
            "{action}: {cold_early:?}"
        );
    }
    assert!(!buggy.check_invariant("ColdRestoreAfterFirstPaint", &cold_early));
    let mut deferred = buggy.init_state();
    for action in ["StartHandoff", "Attach0", "LeaveResumeOwing"] {
        assert!(buggy.fire(action, &mut deferred), "{action}: {deferred:?}");
    }
    assert!(!buggy.check_invariant("HandoffAttachesInOnePass", &deferred));
    let mut unkicked = buggy.init_state();
    for action in ["Widen", "StartHandoff", "Attach0", "RebuildWithoutKick"] {
        assert!(buggy.fire(action, &mut unkicked), "{action}: {unkicked:?}");
    }
    assert!(!buggy.check_invariant("EveryRebuiltWindowPresents", &unkicked));
    let mut trap = buggy.init_state();
    for action in [
        "Widen",
        "StartHandoff",
        "Attach0",
        "Present0",
        "ProveAtFirstPresent",
    ] {
        assert!(buggy.fire(action, &mut trap), "{action}: {trap:?}");
    }
    assert!(
        !buggy.check_invariant("ProofNeedsDrainedAndPainted", &trap),
        "the multi-window trap: a proof at window 0's first present alone"
    );
    let mut sliced_handoff = buggy.init_state();
    for action in [
        "StartDegraded",
        "Attach0",
        "LeaveResume",
        "Present0",
        "RebuildSliceHanded",
    ] {
        assert!(
            buggy.fire(action, &mut sliced_handoff),
            "{action}: {sliced_handoff:?}"
        );
    }
    assert!(!buggy.check_invariant("HandedRebuildsWhole", &sliced_handoff));
    let mut sliced_early = buggy.init_state();
    for action in ["StartCold", "Attach0", "RebuildSliceBeforePaint"] {
        assert!(
            buggy.fire(action, &mut sliced_early),
            "{action}: {sliced_early:?}"
        );
    }
    assert!(
        buggy.check_invariant("ColdRestoreAfterFirstPaint", &sliced_early),
        "a slice leaves the rebuild owed, so the whole-pass invariant cannot see it"
    );
    assert!(!buggy.check_invariant("ColdSlicesAfterFirstPaint", &sliced_early));
}

#[test]
fn the_successor_proves_under_fairness_and_an_unkicked_window_never_does() {
    let model = native_update_successor_attach_model();
    let live = native_update_successor_attach_liveness();
    verify::liveness_proves_and_catches_tiered(&model, &live, "NativeUpdateSuccessorAttach");
}

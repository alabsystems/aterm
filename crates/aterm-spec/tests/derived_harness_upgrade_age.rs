// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::harness_upgrade_age_model, interp, verify};

/// AN AGE BELONGS TO ONE PROCESS (the owner's tab of 2026-09-28): proved at
/// `Buggy = 0`, caught at `Buggy = 1` on the live path — the record minted
/// behind process 0, a day on it, the owner's restart by hand onto the newest
/// build, a newer build landing: the shipped build carried the three-day-old
/// age onto the new holder and judged it overdue at once.
#[test]
fn an_age_is_never_carried_onto_another_process() {
    let model = harness_upgrade_age_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the age machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade age");

    // The live path, fixed: the restart's next visit closes the record, and a
    // newer build mints nothing to judge.
    let mut s = model.init_state();
    for action in ["Time", "HandRestart", "Visit", "NewBuild"] {
        assert!(model.fire(action, &mut s), "{action} at {s:?}");
    }
    assert_eq!(
        (s["rec"], s["aged"]),
        (2, 0),
        "a fresh record for the new holder: {s:?}"
    );
    assert!(
        !model.action_enabled("Judge", &s),
        "nothing to judge: {s:?}"
    );
    // …and a record that a visit has not yet closed is retargeted onto the
    // new process, its age begun again.
    let mut r = model.init_state();
    for action in ["Time", "HandRestart", "NewBuild"] {
        assert!(model.fire(action, &mut r), "{action} at {r:?}");
    }
    assert_eq!((r["rec"], r["aged"]), (2, 0), "{r:?}");
    assert!(!model.action_enabled("Judge", &r), "{r:?}");

    // NEGATIVE CONTROL: the build that shipped.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    // (`Visit` closes nothing in this build, so it is not enabled.)
    for action in ["Time", "HandRestart", "NewBuild", "Judge"] {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(!buggy.check_invariant("AgeIsThisProcess", &b), "{b:?}");
}

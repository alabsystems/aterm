// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::harness_upgrade_age_model, interp, verify};

/// An age belongs to one process: proved at `Buggy = 0`, caught at `Buggy = 1` on the live path
/// (the record minted behind process 0, a day on it, a restart by hand onto the newest build, a
/// newer build landing), where the old age is carried onto the new holder and judged overdue
/// at once.
#[test]
fn an_age_is_never_carried_onto_another_process() {
    let model = harness_upgrade_age_model();
    verify::prove_and_catch_scalar(&model, "harness upgrade age");

    // The live path: the restart's next visit closes the record, and a newer build mints
    // nothing to judge.
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

    // `Buggy = 1` on the same path.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    // (`Visit` closes nothing at `Buggy = 1`, so it is not enabled.)
    for action in ["Time", "HandRestart", "NewBuild", "Judge"] {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(!buggy.check_invariant("AgeIsThisProcess", &b), "{b:?}");
}

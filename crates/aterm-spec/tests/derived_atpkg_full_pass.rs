// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_full_pass_rule_model, interp, verify};

fn walk(model: &aterm_spec::derive::Model, actions: &[&str]) -> interp::State {
    let mut state = model.init_state();
    for action in actions {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

/// Tier-0: every invariant holds over the whole bounded space at `Buggy=0`, each has its
/// own counterexample at `Buggy=1`, and the two defects are replayed step by step: the
/// session lane's misread (a write after the last success read as a failed pass) and the
/// queued child that ran back to back behind a failed pass.
#[test]
fn the_full_pass_rule_proves_and_catches_the_session_lanes_misread() {
    let model = atpkg_full_pass_rule_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the full-pass rule must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "atpkg full-pass rule");
    assert!(
        verify::uncaught_invariants(&model).is_empty(),
        "every invariant is falsified by the Buggy=1 lanes"
    );
    let buggy = interp::with_buggy(&model, 1);

    // A success six hours old, then a vendor head-watch write: the pass is owed.
    let misread = [
        "SessionLook",
        "TakeLockFresh",
        "PassSucceeds",
        "Tick",
        "Tick",
        "Tick",
        "OtherWrite",
        "SessionLook",
    ];
    let fixed = walk(&model, &misread);
    assert_eq!(fixed["queued"], 1, "the fixed lane starts the owed pass");
    assert_eq!(fixed["verdict"], 0);
    let old = walk(&buggy, &misread);
    assert_eq!(old["queued"], 0, "the misread holds it back");
    assert_eq!(old["verdict"], 1, "and reads the success as a failure");
    assert!(!buggy.check_invariant("NoOwedPassHeldBack", &old));
    assert!(!buggy.check_invariant("NoSuccessReadAsFailure", &old));

    // Two lanes decide in the gap before either child holds the lock; the first child's
    // pass fails. The fixed second child stands down behind it; the old one ran at once.
    let queue = [
        "SessionLook",
        "WindowWalk",
        "TakeLockFresh",
        "PassFails",
        "TakeLockBehind",
    ];
    let stood_down = walk(&model, &queue);
    assert_eq!((stood_down["running"], stood_down["queued"]), (0, 0));
    let ran = walk(&buggy, &queue);
    assert_eq!(ran["running"], 1, "the old child ran behind the failure");
    assert!(!buggy.check_invariant("NoPassBackToBack", &ran));

    // THE PROGRESS OBLIGATION IS ON THE TRUTH: with the interval past the record's
    // saturating age, the lanes can never read "an interval old" and park a failed (or
    // succeeded) pass forever — caught at Buggy=0, as a lane that parks it would be.
    let parked = interp::with_consts(&model, &[("Interval", 5)]);
    assert!(
        matches!(interp::bmc(&parked), Err((_, "NoOwedPassHeldBack"))),
        "a rule that parks an owed pass is caught"
    );
}

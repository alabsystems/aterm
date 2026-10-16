// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::walk::walk;
use aterm_spec::{derive::atpkg_full_pass_rule_model, interp, verify};

/// Tier-0: every invariant holds at `Buggy=0` and has its own counterexample at `Buggy=1`, and the
/// two defects replay step by step: a write after the last success misread as a failed pass, and a
/// queued child running back to back behind a failed pass.
#[test]
fn the_full_pass_rule_proves_and_catches_the_session_lanes_misread() {
    let model = atpkg_full_pass_rule_model();
    verify::prove_and_catch_scalar(&model, "atpkg full-pass rule");
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

    // Two lanes decide before either child holds the lock, and the first child's pass fails: the
    // fixed second child stands down behind it, the buggy one runs at once.
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

    // The progress obligation bites: with `Interval` past the record's saturating age no lane can
    // read "an interval old", so an owed pass parks forever, and that is caught at `Buggy=0`.
    let parked = interp::with_consts(&model, &[("Interval", 5)]);
    assert!(
        matches!(interp::bmc(&parked), Err((_, "NoOwedPassHeldBack"))),
        "a rule that parks an owed pass is caught"
    );
}

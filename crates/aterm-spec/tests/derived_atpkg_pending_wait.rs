// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_pending_wait_model, interp, verify};

/// The pending stub's wait proves over the whole bounded space and catches both readers'
/// mistakes: looking at the shim before the pass (a pass that sets the program up and ends
/// between the two reads made it say "the install stopped" about an installed program),
/// and taking a pass that ended without a recorded failure for an install.
#[test]
fn a_waiting_stub_reads_the_pass_before_the_shim() {
    let model = atpkg_pending_wait_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the pending wait must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "atpkg pending stub wait");

    // The healthy race: the stub reads a running pass, the pass sets the program up and
    // ends, and the stub's second read finds the shim — it runs the program.
    let mut healthy = model.init_state();
    for action in ["Download", "Observe", "SetUp", "Finish", "End"] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
    }
    assert!(model.action_enabled("DecideRun", &healthy));
    assert!(!model.action_enabled("DecideFail", &healthy));

    // A pass that dies before it sets the program up: the stub says so.
    let mut died = model.init_state();
    for action in ["Download", "End", "Observe", "DecideFail"] {
        assert!(model.fire(action, &mut died), "{action}: {died:?}");
    }
    assert!(model.check_invariant("NoFalseFailure", &died));

    // The negative control: the same race under the buggy reader is a false failure.
    let buggy = interp::with_buggy(&model, 1);
    let mut race = buggy.init_state();
    for action in [
        "Download",
        "Observe",
        "SetUp",
        "Finish",
        "End",
        "DecideFail",
    ] {
        assert!(buggy.fire(action, &mut race), "{action}: {race:?}");
    }
    assert!(!buggy.check_invariant("NoFalseFailure", &race));

    // …and a pass that died before setting the program up, taken for an install.
    let mut dead = buggy.init_state();
    for action in ["Download", "End", "Observe", "DecideRun"] {
        assert!(buggy.fire(action, &mut dead), "{action}: {dead:?}");
    }
    assert!(!buggy.check_invariant("RunsOnlyInstalled", &dead));
}

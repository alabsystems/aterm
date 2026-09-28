// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{
    derive::{atpkg_session_index_eligibility_model, atpkg_session_index_handoff_model},
    interp, verify,
};

#[test]
fn an_empty_local_look_does_not_delay_the_first_eligible_index_probe() {
    let model = atpkg_session_index_eligibility_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg session index eligibility");

    let mut healthy = model.init_state();
    let buggy = interp::with_buggy(&model, 1);
    let mut old = buggy.init_state();
    for action in ["EmptyLook", "Tick", "Enable", "FirstEligibleLook"] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
        assert!(buggy.fire(action, &mut old), "{action}: {old:?}");
    }
    assert_eq!(healthy["requests"], 1);
    assert_eq!(old["requests"], 0);
    assert!(!buggy.check_invariant("NoArtificialNetworkWarmup", &old));
}

#[test]
fn a_near_index_answer_obeys_the_seat_window_and_verified_floor() {
    let model = atpkg_session_index_handoff_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg session index handoff");

    let mut s = model.init_state();
    for action in [
        "Start",
        "OtherSessionStarts",
        "NearPublished",
        "WindowOpens",
        "TryLaunch",
    ] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["requests"], s["launches"]), (1, 0));
    assert!(model.fire("WindowCloses", &mut s));
    assert!(model.fire("TryLaunch", &mut s));
    assert_eq!(s["launches"], 1);
    assert!(model.fire("Complete", &mut s));
    assert!(model.fire("FarReplay", &mut s));
    assert_eq!(s["launches"], 1);

    let mut landed = model.init_state();
    for action in ["Start", "NearPublished", "VerifyFloor", "TryLaunch"] {
        assert!(model.fire(action, &mut landed), "{action}");
    }
    assert_eq!(landed["launches"], 0);

    let buggy = interp::with_buggy(&model, 1);
    let mut duplicate = buggy.init_state();
    for action in ["Start", "OtherSessionStarts"] {
        assert!(buggy.fire(action, &mut duplicate));
    }
    assert!(!buggy.check_invariant("OneNetworkWorker", &duplicate));
    let mut overlap = buggy.init_state();
    for action in ["Start", "NearPublished", "WindowOpens", "TryLaunch"] {
        assert!(buggy.fire(action, &mut overlap));
    }
    assert!(!buggy.check_invariant("NoWindowOverlap", &overlap));
    let mut twice = buggy.init_state();
    for action in [
        "Start",
        "NearPublished",
        "TryLaunch",
        "Complete",
        "FarReplay",
    ] {
        assert!(buggy.fire(action, &mut twice));
    }
    assert!(!buggy.check_invariant("OnePassPerAnswer", &twice));
}

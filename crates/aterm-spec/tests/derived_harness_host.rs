// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::harness_worker_lifecycle_model, interp, verify};

#[test]
fn one_supervisor_per_session_across_restarts_and_reloads() {
    let model = harness_worker_lifecycle_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the harness worker lifecycle must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness worker lifecycle");

    // A reload while a worker parks in its wait: the shipped host starts the
    // next worker only once the old one has ended.
    let schedule = ["Arrive", "Start", "Reload"];
    let mut state = model.init_state();
    for action in schedule {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!model.action_enabled("Start", &state), "{state:?}");
    assert!(model.fire("Exit", &mut state));
    assert!(model.fire("Start", &mut state));

    // A host that did not wait: two supervisors on one session.
    let eager = interp::with_buggy(&model, 1);
    let mut two = eager.init_state();
    for action in ["Arrive", "Start", "Reload", "Start"] {
        assert!(eager.fire(action, &mut two), "{action}: {two:?}");
    }
    assert!(!eager.check_invariant("OneSupervisor", &two));

    // The budget: Budget failures restart in place, the next one is off, and
    // off stays off until the policy changes.
    let mut s = model.init_state();
    assert!(model.fire("Arrive", &mut s) && model.fire("Start", &mut s));
    for _ in 0..model.consts.iter().find(|c| c.0 == "Budget").unwrap().1 {
        assert!(model.fire("Fail", &mut s));
        assert_eq!(s["cur"], 1, "{s:?}");
    }
    assert!(model.fire("Fail", &mut s));
    assert_eq!((s["cur"], s["faulted"]), (0, 1), "{s:?}");
    assert!(!model.action_enabled("Start", &s));

    // The budget is the SESSION's: the program leaving and coming back
    // clears the badge but not the count, so the next failure is off at once
    // (a flap gives a crash-looping engine no fresh budget)…
    let mut flap = s.clone();
    for action in ["Leave", "Arrive", "Start", "Fail"] {
        assert!(model.fire(action, &mut flap), "{action}: {flap:?}");
    }
    assert_eq!((flap["cur"], flap["faulted"]), (0, 1), "{flap:?}");
    // …the window ageing a failure out gives one run back…
    let mut aged = s.clone();
    for action in ["Leave", "Age", "Age", "Arrive", "Start", "Fail"] {
        assert!(model.fire(action, &mut aged), "{action}: {aged:?}");
    }
    assert_eq!((aged["cur"], aged["faulted"]), (1, 0), "{aged:?}");
    // …and a changed policy forgives it all.
    assert!(model.fire("Reload", &mut s) && model.fire("Start", &mut s));
    assert_eq!(s["faults"], 0, "{s:?}");
}

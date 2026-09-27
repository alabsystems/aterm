// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::say_replay_budget_model, interp, verify};

#[test]
fn bridge_wide_replay_pages_rotate_after_control_work() {
    let model = say_replay_budget_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "Fabric replay budget and rotation");

    let mut state = model.init_state();
    for action in [
        "Control", "FetchA", "FetchB", "FetchA", "FetchB", "Refill", "FetchA", "FetchB", "FetchA",
        "FetchB",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!((state["got_a"], state["got_b"]), (4, 4));

    let buggy = interp::with_buggy(&model, 1);
    let mut old = buggy.init_state();
    assert!(buggy.fire("FetchA", &mut old));
    assert!(!buggy.check_invariant("ControlsFirst", &old));
    assert!(buggy.fire("FetchA", &mut old));
    assert!(!buggy.check_invariant("FairRotation", &old));
    for action in ["FetchA", "FetchA", "FetchB"] {
        assert!(buggy.fire(action, &mut old));
    }
    assert!(!buggy.check_invariant("SharedBound", &old));

    let mut queued = model.init_state();
    assert!(model.fire("Control", &mut queued));
    assert!(model.fire("QueuePriority", &mut queued));
    assert!(model.fire("FetchA", &mut queued));
    assert!(!model.action_enabled("FetchB", &queued));
    assert!(model.fire("HandlePriority", &mut queued));
    assert!(model.fire("FetchB", &mut queued));

    let mut bypass = buggy.init_state();
    assert!(buggy.fire("Control", &mut bypass));
    assert!(buggy.fire("QueuePriority", &mut bypass));
    assert!(buggy.fire("FetchA", &mut bypass));
    assert!(buggy.fire("FetchB", &mut bypass));
    assert!(!buggy.check_invariant("PriorityPageBound", &bypass));

    let mut pushed = model.init_state();
    assert!(model.fire("Control", &mut pushed));
    assert!(model.fire("BeginPush", &mut pushed));
    assert!(model.fire("FetchA", &mut pushed));
    assert!(!model.action_enabled("FetchB", &pushed));
    assert!(model.fire("EndPush", &mut pushed));
    assert!(model.fire("FetchB", &mut pushed));

    let mut burst = buggy.init_state();
    assert!(buggy.fire("Control", &mut burst));
    assert!(buggy.fire("BeginPush", &mut burst));
    assert!(buggy.fire("FetchA", &mut burst));
    assert!(buggy.fire("FetchB", &mut burst));
    assert!(!buggy.check_invariant("PushPageBound", &burst));
}

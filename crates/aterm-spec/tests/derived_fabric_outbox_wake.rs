// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::fabric_outbox_wake_model, interp, verify};

#[test]
fn prompt_outbox_wake_and_lost_event_backstop_prove_and_catch() {
    let model = fabric_outbox_wake_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the outbox wake contract must stay enrolled in spec-link"
    );
    verify::prove_and_catch_scalar(&model, "Fabric outbox prompt and backstop");

    let mut prompt = model.init_state();
    assert!(model.fire("Queue", &mut prompt));
    assert!(model.action_enabled("PromptDrain", &prompt));
    assert!(model.fire("PromptDrain", &mut prompt));
    assert_eq!(prompt["delivered"], 1);

    let mut lost = model.init_state();
    for action in ["Queue", "LoseEvent", "BackstopTick", "BackstopTick"] {
        assert!(model.fire(action, &mut lost), "{action}: {lost:?}");
    }
    assert_eq!(lost["delivered"], 1);

    let buggy = interp::with_buggy(&model, 1);
    let mut no_prompt = buggy.init_state();
    assert!(buggy.fire("Queue", &mut no_prompt));
    assert!(!buggy.check_invariant("PromptAvailable", &no_prompt));
    for _ in 0..2 {
        assert!(buggy.fire("BackstopTick", &mut no_prompt));
    }
    assert!(!buggy.check_invariant("BackstopBounded", &no_prompt));
}

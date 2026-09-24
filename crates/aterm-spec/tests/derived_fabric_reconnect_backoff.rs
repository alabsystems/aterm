// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::fabric_reconnect_backoff_model, interp, verify};

#[test]
fn fabric_retry_wait_ignores_events_until_deadline_but_closes_on_aterm_loss() {
    let model = fabric_reconnect_backoff_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the reconnect wait contract must stay enrolled in spec-link"
    );
    verify::prove_and_catch_scalar(&model, "Fabric reconnect retry backoff");

    let mut busy = model.init_state();
    assert!(model.fire("Event", &mut busy));
    assert!(model.fire("Event", &mut busy));
    assert_eq!(busy["result"], 0);
    assert!(model.fire("Deadline", &mut busy));
    assert_eq!(busy["result"], 1);

    let mut closed = model.init_state();
    assert!(model.fire("Event", &mut closed));
    assert!(model.fire("Closed", &mut closed));
    assert_eq!(closed["result"], 2);
    assert!(!model.action_enabled("Deadline", &closed));

    let buggy = interp::with_buggy(&model, 1);
    let mut early = buggy.init_state();
    assert!(buggy.fire("Event", &mut early));
    assert!(!buggy.check_invariant("RetryNeedsDeadline", &early));
}

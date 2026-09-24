// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::broadcast_cursor_checkpoint_model, interp, verify};

#[test]
fn irrelevant_cursor_checkpoints_may_lag_but_delivered_records_may_not() {
    let model = broadcast_cursor_checkpoint_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "broadcast cursor checkpoint");

    let mut state = model.init_state();
    for action in [
        "Irrelevant",
        "Crash",
        "ReplayIrrelevant",
        "Deliver",
        "Crash",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(state["reoffered"], 0);
    assert_eq!(state["durable"], state["delivered"]);

    // Unsafe alternative: removing the delivered-message write.
    // The same stale irrelevant cursor is harmless, but the delivered row is
    // offered again after the second crash.
    let old = interp::with_buggy(&model, 1);
    let mut buggy = old.init_state();
    for action in [
        "Irrelevant",
        "Crash",
        "ReplayIrrelevant",
        "Deliver",
        "Crash",
    ] {
        assert!(old.fire(action, &mut buggy), "{action}: {buggy:?}");
    }
    assert_eq!(buggy["reoffered"], 1);
    assert!(!old.check_invariant("NoDeliveredReplay", &buggy));
}

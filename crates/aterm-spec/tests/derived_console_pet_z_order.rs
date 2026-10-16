// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::console_pet_z_order_model, interp, verify};

#[test]
fn pet_z_order_proves_and_catches_every_mutant() {
    let model = console_pet_z_order_model();
    verify::prove_and_catch_scalar(&model, "console pet z-order hold");
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(0),
        "every action fires at Buggy=0"
    );
    let declared: std::collections::BTreeSet<_> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(
        interp::fired_actions(&interp::with_buggy(&model, 1)),
        declared,
        "every action fires at Buggy=1"
    );
}

#[test]
fn pet_z_order_goes_under_at_once_and_surfaces_only_after_the_dwell() {
    let model = console_pet_z_order_model();
    let mut state = model.init_state();
    assert!(model.fire("Clear", &mut state));
    assert_eq!(state["under"], 0, "a clear body is drawn over");
    assert!(
        !model.action_enabled("ClearDwelt", &state),
        "no run under way"
    );
    assert!(model.fire("Ink", &mut state));
    assert_eq!(state["under"], 1, "ink sends it under on the same frame");
    for _ in 0..3 {
        assert!(model.fire("Clear", &mut state));
        assert_eq!(state["under"], 1, "held while the dwell counts");
    }
    assert!(model.fire("ClearDwelt", &mut state));
    assert_eq!(state["under"], 0, "surfaces once the dwell has passed");
    assert!(model.fire("Ink", &mut state));
    assert!(model.fire("Hide", &mut state));
    assert_eq!(state["under"], 0, "nothing drawn releases the hold");

    // `Buggy = 1` is the per-frame rule: a clear body beside the caret sinks on the caret's
    // keep-off ring, and a body leaving ink surfaces on the very next clear frame.
    let buggy = interp::with_buggy(&model, 1);
    let mut old = buggy.init_state();
    assert!(buggy.fire("Clear", &mut old));
    assert!(!buggy.check_invariant("ClearNeverSinks", &old));
    let mut old = buggy.init_state();
    for action in ["Ink", "Ink", "Clear", "Clear"] {
        assert!(buggy.fire(action, &mut old), "{action}");
    }
    assert!(!buggy.check_invariant("SurfacingWaitsTheDwell", &old));
}

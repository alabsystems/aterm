// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::harness_upgrade_startup_cadence_model, interp, verify};

#[test]
fn a_ready_socket_does_not_retire_the_short_startup_window() {
    let model = harness_upgrade_startup_cadence_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the startup cadence must stay enrolled in spec-link"
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade startup cadence");

    let mut healthy = model.init_state();
    for tick in 1..=7 {
        assert!(model.fire("Step", &mut healthy));
        assert_eq!(healthy["short"], i64::from(tick <= 5), "tick {tick}");
        if tick == 1 {
            assert!(model.fire("Ready", &mut healthy));
        }
    }

    let buggy = interp::with_buggy(&model, 1);
    let mut premature = buggy.init_state();
    assert!(buggy.fire("Step", &mut premature));
    assert!(buggy.fire("Ready", &mut premature));
    assert!(buggy.fire("Step", &mut premature));
    assert!(!buggy.check_invariant("ShortUntilBudget", &premature));
}

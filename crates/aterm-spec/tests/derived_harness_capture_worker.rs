// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::harness_capture_worker_lifecycle_model, interp, verify};

#[test]
fn capture_must_join_its_reader_before_return() {
    let model = harness_capture_worker_lifecycle_model();
    verify::prove_and_catch_scalar(&model, "harness capture worker lifecycle");

    let mut healthy = model.init_state();
    assert!(model.fire("TimeoutKill", &mut healthy));
    assert!(!model.action_enabled("Return", &healthy));
    assert!(model.fire("ReaderFinish", &mut healthy));
    assert!(model.action_enabled("Return", &healthy));

    let buggy = interp::with_buggy(&model, 1);
    let mut early = buggy.init_state();
    assert!(buggy.fire("ChildExit", &mut early));
    assert!(buggy.fire("Deadline", &mut early));
    assert!(buggy.fire("Return", &mut early));
    assert!(!buggy.check_invariant("NoReturnBeforeWorkersJoin", &early));
}

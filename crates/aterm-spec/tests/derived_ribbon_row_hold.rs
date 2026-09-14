// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::ribbon_row_hold_model, verify};

#[test]
fn ribbon_row_hold_proves_and_catches_global_renewal() {
    let model = ribbon_row_hold_model();
    verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn ribbon_row_hold_permits_returning_to_the_earlier_row() {
    let model = ribbon_row_hold_model();
    let mut state = model.init_state();
    for action in ["TouchRow0", "TouchRow1", "TouchRow0"] {
        assert!(model.fire(action, &mut state));
        assert!(model.check_invariant("OnlyOwnerRenews", &state));
        assert!(model.check_invariant("OwnerKeepsItsLease", &state));
    }
    assert_eq!(state["clock0"], 3);
    assert_eq!(state["clock1"], 2);
}

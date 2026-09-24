// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::observation_screen_generation_model, interp, verify};

#[test]
fn the_change_test_sees_an_equal_seq_re_entry() {
    let model = observation_screen_generation_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the screen-generation model must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "observation screen generation");

    // The measured schedule: box A drawn and seen, the screen re-entered, box B
    // drawn to the same count. The shipped test sees it.
    let schedule = ["Write", "Observe", "Reenter", "Write", "Observe"];
    let mut state = model.init_state();
    for action in schedule {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(
        state["seq"], state["seen_seq"],
        "box B's seq is the one seen"
    );
    assert_eq!(state["missed"], 0);

    // The old `(seq, alt)` test on the same schedule: box B lands on seq 2,
    // the seq already seen, and is missed.
    let old = interp::with_buggy(&model, 1);
    let mut stale = old.init_state();
    for action in schedule {
        assert!(old.fire(action, &mut stale), "{action}: {stale:?}");
    }
    assert_eq!(stale["missed"], 1);
    assert!(!old.check_invariant("EveryChangeIsSeen", &stale));
}

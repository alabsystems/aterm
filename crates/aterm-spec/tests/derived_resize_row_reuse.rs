// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 allocation ownership for rows-only resize. Tier-1 binds the same
//! transitions to Grid/PageStore; the model alone makes no claim about Rust.

use aterm_spec::{derive::resize_row_reuse_model, interp, verify};

#[test]
fn resize_slices_are_conserved_and_reused_before_allocating() {
    let model = resize_row_reuse_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the resize-row reuse model must remain registered"
    );
    verify::prove_and_catch_scalar(&model, "resize-row slices returned on shrink");
    let bypass = interp::with_consts(&model, &[("BypassReuse", 1)]);
    verify::prove_and_catch_scalar(&bypass, "resize-row slices reused on grow");

    // Repeated flaps with intervening output cannot spend another slice.
    let mut state = model.init_state();
    for _ in 0..12 {
        for action in ["Shrink", "Output", "Grow"] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
            assert!(model.check_invariant("OwnedExactlyOnce", &state));
            assert!(model.check_invariant("BoundedByHighWater", &state));
        }
        assert_eq!(
            (state["live"], state["free"], state["allocated"]),
            (2, 0, 2)
        );
    }
    assert!(model.fire("Grow", &mut state));
    assert_eq!(
        (state["live"], state["allocated"], state["peak"]),
        (3, 3, 3)
    );
    for action in ["Shrink", "Shrink", "Rebuild"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(
        (
            state["live"],
            state["free"],
            state["allocated"],
            state["peak"]
        ),
        (1, 0, 1, 1),
        "a rebuilt store keeps no pointers or allocation count from its predecessor"
    );
    assert!(model.fire("Grow", &mut state));
    assert_eq!((state["allocated"], state["peak"]), (2, 2));
}

#[test]
fn historical_discard_and_independent_bypass_are_both_caught() {
    let model = resize_row_reuse_model();
    let historical = interp::with_buggy(&model, 1);
    let mut leaked = historical.init_state();
    assert!(historical.fire("Shrink", &mut leaked));
    assert!(!historical.check_invariant("OwnedExactlyOnce", &leaked));
    assert!(historical.check_invariant("BoundedByHighWater", &leaked));
    assert!(historical.fire("Grow", &mut leaked));
    assert!(!historical.check_invariant("BoundedByHighWater", &leaked));

    // Keeping the freed slice still leaks if the next Row ignores it. This
    // defect conserves ownership, so it must fail the independent bound.
    let bypass = interp::with_consts(&model, &[("Buggy", 1), ("BypassReuse", 1)]);
    let mut extra = bypass.init_state();
    for action in ["Shrink", "Grow"] {
        assert!(bypass.fire(action, &mut extra), "{action}: {extra:?}");
        assert!(bypass.check_invariant("OwnedExactlyOnce", &extra));
    }
    assert_eq!((extra["allocated"], extra["peak"]), (3, 2));
    assert!(!bypass.check_invariant("BoundedByHighWater", &extra));
}

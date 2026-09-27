// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_flip_quiet_model, interp, verify};

/// The held toolchain flip proves over the whole bounded space and catches the three ways
/// the decision goes wrong: an unreadable process table read as quiet, a revoked build
/// held, and a hold past the ceiling.
#[test]
fn a_busy_toolchain_holds_its_flip_up_to_the_ceiling_and_a_revoked_one_never_waits() {
    let model = atpkg_flip_quiet_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the flip gate must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "atpkg flip when quiet");

    // The healthy path: in use for three looks, then the ceiling flips it anyway.
    let mut s = model.init_state();
    assert!(model.fire("Use", &mut s));
    for _ in 0..3 {
        assert!(model.fire("Hold", &mut s), "{s:?}");
        assert!(model.fire("Tick", &mut s), "{s:?}");
    }
    assert!(!model.action_enabled("Hold", &s), "no hold at the ceiling");
    assert!(model.fire("Flip", &mut s));
    assert!(model.check_invariant("NoFlipUnderABuild", &s));

    // A revoked build flips at once, busy or not.
    let mut revoked = model.init_state();
    for action in ["Use", "Revoke"] {
        assert!(model.fire(action, &mut revoked), "{action}");
    }
    assert!(!model.action_enabled("Hold", &revoked));
    assert!(model.fire("Flip", &mut revoked));

    // NEGATIVE CONTROLS, one per claim, under the buggy decision.
    let buggy = interp::with_buggy(&model, 1);
    let mut unsure = buggy.init_state();
    for action in ["Unsure", "Flip"] {
        assert!(buggy.fire(action, &mut unsure), "{action}");
    }
    assert!(!buggy.check_invariant("NoFlipUnderABuild", &unsure));
    let mut held = buggy.init_state();
    for action in ["Use", "Revoke", "Hold"] {
        assert!(buggy.fire(action, &mut held), "{action}");
    }
    assert!(!buggy.check_invariant("RevokedNeverWaits", &held));
    let mut late = buggy.init_state();
    assert!(buggy.fire("Use", &mut late));
    for _ in 0..4 {
        assert!(buggy.fire("Hold", &mut late), "{late:?}");
        assert!(buggy.fire("Tick", &mut late), "{late:?}");
    }
    assert!(!buggy.check_invariant("WaitIsBounded", &late));
}

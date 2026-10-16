// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `TextSelectionLifecycle`: `ty` proves the four call rules at
//! `Buggy = 0` and catches each dropped phase check at `Buggy = 1`.

use aterm_spec::{derive::text_selection_lifecycle_model, interp, verify};

#[test]
fn text_selection_calls_act_only_in_their_phase() {
    let model = text_selection_lifecycle_model();
    verify::prove_and_catch_scalar(&model, "text selection lifecycle");

    // The committed machine: press, drag, release, shift-click, clear.
    let mut s = model.init_state();
    for (action, phase, end) in [
        ("Complete", 0, 0),
        ("Update", 0, 0),
        ("Extend", 0, 0),
        ("Start", 1, 0),
        ("Extend", 1, 0),
        ("Update", 1, 1),
        ("Complete", 2, 1),
        ("Update", 2, 1),
        ("Extend", 1, 2),
        ("Complete", 2, 2),
        ("Clear", 0, 2),
    ] {
        assert!(model.fire(action, &mut s), "{action}");
        assert_eq!((s["phase"], s["end"]), (phase, end), "after {action}");
    }

    // Each dropped phase check is caught by its own rule, from a reachable
    // state of the mutant.
    let buggy = interp::with_buggy(&model, 1);
    for (path, broken) in [
        (&["Start", "Complete", "Update"][..], "UpdateOnlyInProgress"),
        (&["Complete"][..], "CompleteOnlyFromInProgress"),
        (&["Start", "Extend"][..], "ExtendOnlyFromComplete"),
        (&["Start", "Complete", "Clear"][..], "ClearIsNone"),
    ] {
        let mut s = buggy.init_state();
        for action in path {
            assert!(buggy.fire(action, &mut s), "{action}");
        }
        assert!(
            !buggy.check_invariant(broken, &s),
            "{path:?} breaks {broken}"
        );
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `TrickListenerPurity` (`aterm_models_effects::trick_listener_purity_model`):
//! proved at `Buggy = 0`, caught at `Buggy = 1`, every action live.

use aterm_models_effects::trick_listener_purity_model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

#[test]
fn trick_listener_purity_has_no_dead_action() {
    let model = trick_listener_purity_model();
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires: {fired:?}"
    );

    let bare = walk(&model, &["Letter", "EdgeFire"]);
    assert_eq!((bare["said"], bare["named"]), (1, 0));
    let named = walk(&model, &["Letter", "EdgeName", "Letter", "EdgeFire"]);
    assert_eq!((named["said"], named["named"]), (2, 1));
    let marked = walk(&model, &["Letter", "MarkName", "Letter", "EdgeFire"]);
    assert_eq!((marked["said"], marked["named"]), (2, 1));

    let parked = walk(&model, &["Letter", "EdgeGlued"]);
    assert_eq!((parked["said"], parked["named"]), (0, 0));
    let entered = walk(&model, &["Letter", "EdgeGlued", "SubmitFinal"]);
    assert_eq!(entered["said"], 2);
    let vouched = walk(&model, &["Letter", "EdgeGlued", "Letter", "EdgeNameFires"]);
    assert_eq!((vouched["said"], vouched["named"]), (2, 1));

    let buggy = interp::with_buggy(&model, 1);
    for (path, broken) in [
        (&["Letter", "Letter"][..], "APrefixNeverSpeaks"),
        (
            &["Letter", "EdgeUnknown", "Letter", "EdgeQuiet"][..],
            "OnlyAPureLineSpeaks",
        ),
        (&["Letter", "EdgeQuiet"][..], "OnlyTheNameNamesTheLine"),
        (
            &["Letter", "EdgeGlued"][..],
            "AGluedRunWaitsForEnterOrTheName",
        ),
        (
            &["Letter", "EdgeGlued", "EdgeQuiet", "EdgeFire"][..],
            "AGluedRunWaitsForEnterOrTheName",
        ),
    ] {
        let s = walk(&buggy, path);
        assert!(
            !buggy.check_invariant(broken, &s),
            "{path:?} breaks {broken}"
        );
    }
}

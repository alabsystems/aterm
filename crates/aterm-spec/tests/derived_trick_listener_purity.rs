// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `TrickListenerPurity` (`aterm_spec::derive::trick_listener_purity_model`):
//! proved at `Buggy = 0`, caught at `Buggy = 1`, every action live. Tier-1 is
//! `aterm-effects/tests/conformance_trick_listener.rs`.

use crate::walk::walk;
use aterm_spec::derive::trick_listener_purity_model;
use aterm_spec::{interp, verify};

#[test]
fn trick_listener_purity_proves_catches_and_has_no_dead_action() {
    let model = trick_listener_purity_model();
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires: {fired:?}"
    );
    verify::prove_and_catch_scalar(
        &model,
        "kitty listener: only a pure, unpoisoned line speaks, at an edge, and only the name makes it final",
    );

    // `sit␣` alone is tentative; after `kitty␣`, or `kitty,`, it is final.
    let bare = walk(&model, &["Letter", "EdgeFire"]);
    assert_eq!((bare["said"], bare["named"]), (1, 0));
    let named = walk(&model, &["Letter", "EdgeName", "Letter", "EdgeFire"]);
    assert_eq!((named["said"], named["named"]), (2, 1));
    let marked = walk(&model, &["Letter", "MarkName", "Letter", "EdgeFire"]);
    assert_eq!((marked["said"], marked["named"]), (2, 1));

    // The mutant's `site` fires on the completing letter, and its `npm run sit`
    // fires on a line that is prose: each breaks its own law.
    let buggy = interp::with_buggy(&model, 1);
    for (path, broken) in [
        (&["Letter", "Letter"][..], "APrefixNeverSpeaks"),
        (
            &["Letter", "EdgeUnknown", "Letter", "EdgeQuiet"][..],
            "OnlyAPureLineSpeaks",
        ),
        (&["Letter", "EdgeQuiet"][..], "OnlyTheNameNamesTheLine"),
    ] {
        let s = walk(&buggy, path);
        assert!(
            !buggy.check_invariant(broken, &s),
            "{path:?} breaks {broken}"
        );
    }
}

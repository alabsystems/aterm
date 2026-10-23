// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of the holder fence: every law holds over the whole reachable
//! space, and each `aterm_grid::FenceKnob`'s mutant breaks the law it
//! guards (the dials follow the knobs' order).

use aterm_models_core::holder_fence_model;
use aterm_spec::interp;

/// The laws the mutant at `dial` breaks.
fn broken_at(dial: i64) -> Vec<&'static str> {
    interp::violated_invariants(&interp::with_buggy(&holder_fence_model(), dial))
        .into_iter()
        .collect()
}

#[test]
fn the_fence_admits_only_the_current_drawers_rows_and_keeps_what_nobody_took() {
    let m = holder_fence_model();
    interp::bmc(&m).unwrap_or_else(|(s, law)| panic!("{law} broken at {s:?}"));
    let own = ["AdmittedIsOwn"];
    for (dial, laws) in [
        (1, &own[..]),
        (2, &["SuspendedSpansStaySealed"][..]),
        (3, &own[..]),
        (4, &own[..]),
        (5, &["CardSurvives"][..]),
        (6, &own[..]),
        (7, &own[..]),
        (8, &own[..]),
        (9, &["AdmittedIsOwn", "CardSurvives"][..]),
        (10, &["CardSurvives", "StoppedKeepsMark"][..]),
        (11, &["AdmittedAltIsOwn"][..]),
    ] {
        assert_eq!(broken_at(dial), laws, "Buggy = {dial}");
    }
    assert_eq!(m.mutant_dials(), (1..=11).collect::<Vec<_>>());
}

/// The design's scenario: Ctrl-Z, a full-screen tool, `fg` keep the card;
/// a rewrite while stopped, then `fg`, costs it.
#[test]
fn a_stop_and_a_tool_keep_the_card_and_a_rewrite_takes_it() {
    let m = holder_fence_model();
    let mut s = m.init_state();
    for action in ["LaunchA", "StopA", "Alt1049", "AltDraw", "AltExit", "FgA"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["fg"], s["ta"], s["ka"], s["fa"]), (1, 0, 1, 0));
    for action in ["StopA", "Scribble", "FgA"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["ka"], s["fa"], s["hurt"]), (0, 0, 1));
}

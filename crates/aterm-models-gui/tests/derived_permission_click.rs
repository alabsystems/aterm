// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 checks of `ClaudePermissionClick`: every invariant holds on the clean model, each
//! `Buggy` dial breaks its paired invariant, and a click whose restart lands with the pill unmoved
//! ends there, where the retired end at the pill signals again.

use aterm_models_gui::claude_permission_click_model;
use aterm_spec::interp;

/// NEGATIVE CONTROL: each law holds with no mutant on, and its own break is caught: the k-th
/// `Buggy` dial breaks the k-th law (a ghost law would pass its break too).
#[test]
fn each_law_holds_and_its_own_break_is_caught() {
    let model = claude_permission_click_model();
    assert!(interp::violated_invariants(&interp::with_buggy(&model, 0)).is_empty());
    for (dial, law) in [
        (1, "AClickOnlyTurnsYoloOn"),
        (2, "TheSignalKeepsItsAsk"),
        (3, "ADoubleClickIsOneClick"),
        (4, "ARefusalIsSaid"),
        (5, "NoSignalIntoAWait"),
        (6, "NoLineWhereNoneCanBeMade"),
        (7, "OneRestartPerClick"),
        (8, "NoAskOutlivesItsTab"),
        (9, "NoAskOutlivesItsConversation"),
    ] {
        let broken = interp::violated_invariants(&interp::with_buggy(&model, dial));
        assert!(
            broken.contains(&law),
            "Buggy = {dial} breaks {law}: {broken:?}"
        );
    }
}

/// A click on the off chip whose restart lands out of bypass (a home that disables it) ends at
/// its restart: nothing stands and nothing signals again. NEGATIVE CONTROL: the retired end at the pill
/// (`Buggy = 7`) keeps the ask while the pill shows what it showed, and signals a second restart.
#[test]
fn a_click_ends_at_its_restart_not_at_the_pill() {
    let model = claude_permission_click_model();
    let mut state = model.init_state();
    for action in ["Click", "Signal", "Land"] {
        assert!(model.fire(action, &mut state), "{action}");
    }
    assert_eq!((state["stands"], state["lane"], state["typed"]), (0, 0, 1));
    assert_eq!(state["bit"], state["from"], "the pill unmoved");
    assert!(model.successors("Signal", &state).is_empty(), "no second");
    let buggy = interp::with_buggy(&model, 7);
    let mut kept = buggy.init_state();
    for action in ["Click", "Signal", "Land", "Signal"] {
        assert!(buggy.fire(action, &mut kept), "{action}");
    }
    assert!(!buggy.check_invariant("OneRestartPerClick", &kept));
}

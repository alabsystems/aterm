// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 checks of `ClaudeSlashPress`: every invariant holds on the clean model, each `Buggy`
//! dial breaks its paired invariant, and the vim NORMAL probe trace stops named without typing
//! motion characters.

use aterm_models_gui::claude_slash_press_model;
use aterm_spec::interp;

/// NEGATIVE CONTROL: each law holds with no mutant on, and its own break is caught: the
/// k-th `Buggy` dial breaks the k-th law (a ghost law would pass its break too).
#[test]
fn each_law_holds_and_its_own_break_is_caught() {
    let model = claude_slash_press_model();
    assert!(interp::violated_invariants(&interp::with_buggy(&model, 0)).is_empty());
    for (dial, law) in [
        (1, "NoTypeOverDraft"),
        (2, "NoTypeIntoBox"),
        (3, "NoTypeWhileBusy"),
        (4, "NoMotionsInVim"),
        (5, "NoEnterUnlessOurEcho"),
        (6, "NoEnterAfterPerson"),
        (7, "AStopIsNamed"),
    ] {
        let broken = interp::violated_invariants(&interp::with_buggy(&model, dial));
        assert!(
            broken.contains(&law),
            "Buggy = {dial} breaks {law}: {broken:?}"
        );
    }
}

/// NEGATIVE CONTROL: the measured vim NORMAL trace (2.1.304): the probe draws nothing, so
/// the press can only stop, named — and the retired whole-command write (the rest typed with
/// no read-back) leaves motions' garbage.
#[test]
fn in_vim_normal_the_probe_only_stops() {
    let model = claude_slash_press_model();
    let mut state = model.init_state();
    for action in ["VimNormal", "Click"] {
        assert!(model.fire(action, &mut state), "{action}");
    }
    assert!(
        model.successors("Echo", &state).is_empty(),
        "nothing echoes"
    );
    assert!(model.successors("Type", &state).is_empty(), "no rest typed");
    assert!(model.fire("Stop", &mut state));
    assert_eq!(state["named"], 1);
    assert_eq!(state["composer"], 0, "the composer as it was");
    let buggy = interp::with_buggy(&model, 4);
    let mut raced = buggy.init_state();
    for action in ["VimNormal", "Click", "Type"] {
        assert!(buggy.fire(action, &mut raced), "{action}");
    }
    assert!(!buggy.check_invariant("NoMotionsInVim", &raced));
}

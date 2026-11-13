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
        (1, "AClickFlipsTheBit"),
        (2, "TheSignalKeepsItsAsk"),
        (3, "ADoubleClickIsOneClick"),
        (4, "ARefusalIsSaid"),
        (5, "NoSignalIntoAWait"),
        (6, "NoLineWhereNoneCanBeMade"),
        (7, "OneRestartPerClick"),
        (8, "NoAskOutlivesItsTab"),
        (9, "NoAskOutlivesItsConversation"),
        (10, "NoRestartIntoTheModeItLeft"),
        (11, "NoBypassLineWhereBypassIsOff"),
    ] {
        let broken = interp::violated_invariants(&interp::with_buggy(&model, dial));
        assert!(
            broken.contains(&law),
            "Buggy = {dial} breaks {law}: {broken:?}"
        );
    }
}

/// A CLICK GOES EACH WAY, AND NOWHERE WHERE THE SETTINGS LEAVE IT NOTHING TO CHANGE: from off it
/// asks the bypass line; from on it asks the settings' line; from on where the settings' default
/// is yolo, and from off where the settings turn bypass off, it stands nothing, and saying so is
/// the one step offered. NEGATIVE CONTROLS: the retired one-way click (`Buggy = 1`) asks bypass
/// from bypass, the click that ignores the default (`Buggy = 10`) stands a restart into the mode
/// it left, and the click that ignores the bar (`Buggy = 11`) asks a line Claude Code refuses.
#[test]
fn a_click_flips_the_bit_and_explains_where_the_default_is_yolo() {
    let model = claude_permission_click_model();
    let mut off = model.init_state();
    assert!(model.fire("Click", &mut off));
    assert_eq!((off["from"], off["to"]), (0, 1), "off asks bypass");
    let mut on = model.init_state();
    assert!(model.fire("ToYolo", &mut on));
    let mut left = on.clone();
    assert!(model.fire("Click", &mut left));
    assert_eq!((left["from"], left["to"]), (1, 0), "on asks the settings");
    assert!(
        model.successors("Explain", &on).is_empty(),
        "nothing to say"
    );
    assert!(model.fire("DefaultYolo", &mut on));
    assert!(
        model.successors("Click", &on).is_empty(),
        "no click where the default is yolo"
    );
    assert!(model.fire("Explain", &mut on));
    assert_eq!((on["stands"], on["lane"], on["typed"]), (0, 0, 0));
    let mut barred = model.init_state();
    assert!(
        model.successors("Explain", &barred).is_empty(),
        "nothing to say on the off chip while bypass is allowed"
    );
    assert!(model.fire("BypassBarred", &mut barred));
    assert!(
        model.successors("Click", &barred).is_empty(),
        "no click onto a line the settings turn off"
    );
    assert!(model.fire("Explain", &mut barred));
    assert_eq!(
        (barred["stands"], barred["lane"], barred["typed"]),
        (0, 0, 0)
    );
    let mut lifted = model.init_state();
    for action in ["BypassBarred", "BypassAllowed", "Click"] {
        assert!(model.fire(action, &mut lifted), "{action}");
    }
    assert_eq!((lifted["from"], lifted["to"]), (0, 1), "the bar lifted");
    for (dial, steps, law) in [
        (1, &["ToYolo", "Click"][..], "AClickFlipsTheBit"),
        (
            10,
            &["ToYolo", "DefaultYolo", "Click"][..],
            "NoRestartIntoTheModeItLeft",
        ),
        (
            11,
            &["BypassBarred", "Click"][..],
            "NoBypassLineWhereBypassIsOff",
        ),
    ] {
        let buggy = interp::with_buggy(&model, dial);
        let mut state = buggy.init_state();
        for action in steps {
            assert!(buggy.fire(action, &mut state), "{dial}: {action}");
        }
        assert!(!buggy.check_invariant(law, &state), "{dial} breaks {law}");
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

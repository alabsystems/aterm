// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::{harness_upgrade_goal_pause_liveness, harness_upgrade_goal_pause_model};
use aterm_spec::{interp, verify};

/// THE LIVE UPGRADE'S GOAL PAUSE (the owner's decision of 2026-09-28): a
/// goal the upgrade paused is resumed before the host lets the tab go, the
/// Esc is pressed only at a turn's head, the goal is resumed once and never
/// over a person's hand, and no pause is made under an open switch or before
/// the ladder's Land rung. Each invariant is caught by its own `Buggy`
/// member, and each dead action ALONE by an invariant
/// (`verify::audit_dead_negative_controls`). Tier-1 against the real
/// `goal_step` is in aterm-agent's `harness::upgrade_drive` goal tests.
#[test]
fn the_goal_pause_is_resumed_once_and_never_cuts_a_tool_off() {
    let model = harness_upgrade_goal_pause_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade goal pause");
    let dead: Vec<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !interp::fired_actions(&interp::with_buggy(&model, 0)).contains(a))
        .collect();
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &dead),
        Ok(8),
        "{dead:?}"
    );

    // The incident's goal: its turns chain, and before the Land rung nothing
    // pauses it — the move never comes.
    let mut s = model.init_state();
    assert!(!model.action_enabled("GoalPause", &s), "not at first sight");
    assert!(model.fire("TurnEnds", &mut s));
    assert_eq!(
        s["turn"], 1,
        "the next goal turn begins at its head at once"
    );
    assert!(!model.action_enabled("LadderMove", &s));
    assert!(model.fire("Advance", &mut s));
    assert!(model.fire("GoalPause", &mut s));
    // The typed pause lets the running turn finish: nothing of it is stopped.
    assert!(model.fire("TurnWorks", &mut s));
    assert!(model.fire("PauseTakes", &mut s));
    assert_eq!((s["goal"], s["turn"]), (2, 2));
    assert!(model.fire("Took", &mut s));
    assert!(
        !model.action_enabled("LadderMove", &s),
        "its last turn still runs"
    );
    assert!(model.fire("TurnEnds", &mut s));
    assert_eq!(s["turn"], 0, "no turn follows a paused goal's last");
    assert!(model.fire("LadderMove", &mut s));
    assert!(!model.action_enabled("LetGo", &s), "the resume is owed");
    assert!(model.fire("GoalResume", &mut s));
    assert_eq!((s["goal"], s["held"], s["resumes"]), (1, 3, 1));
    assert!(model.action_enabled("LetGo", &s));

    // A person's hand: their resume of the paused goal takes it from aterm,
    // which resumes nothing after the move.
    let mut p = model.init_state();
    for a in [
        "Advance",
        "GoalPause",
        "PauseTakes",
        "Took",
        "PersonResumes",
    ] {
        assert!(model.fire(a, &mut p), "{a}");
    }
    assert!(model.fire("Release", &mut p));
    assert!(model.fire("PersonPauses", &mut p));
    assert!(model.fire("TurnEnds", &mut p));
    assert!(model.fire("LadderMove", &mut p));
    assert!(!model.action_enabled("GoalResume", &p), "theirs");
    assert!(model.action_enabled("LetGo", &p));

    // A person's hand on a pause the lane has not seen yet: what shows may be
    // theirs (their Esc, their own `/goal pause`) — released, never taken,
    // and nothing is resumed after the move (the review of 2026-09-28).
    let mut q = model.init_state();
    for a in ["Advance", "GoalPause", "PersonPauses"] {
        assert!(model.fire(a, &mut q), "{a}");
    }
    assert!(!model.action_enabled("Took", &q), "never claimed");
    assert!(model.fire("Release", &mut q));
    assert!(model.fire("TurnEnds", &mut q));
    assert!(model.fire("LadderMove", &mut q));
    assert!(!model.action_enabled("GoalResume", &q), "theirs");

    // An open switch keeps the pause off, and the resume.
    let mut w = model.init_state();
    for a in ["Advance", "SwitchOpens"] {
        assert!(model.fire(a, &mut w), "{a}");
    }
    assert!(!model.action_enabled("GoalPause", &w));

    // Each mutant, caught.
    let buggy = interp::with_buggy(&model, 1);
    let mut e = buggy.init_state();
    for a in ["Advance", "GoalPause", "EscMidWork"] {
        assert!(buggy.fire(a, &mut e), "{a}");
    }
    assert!(!buggy.check_invariant("NoToolCutOff", &e));
    let mut h = buggy.init_state();
    for a in ["Advance", "TurnEnds", "GoalPause", "GoalEsc"] {
        assert!(buggy.fire(a, &mut h), "{a}");
    }
    assert!(buggy.check_invariant("NoToolCutOff", &h), "the head: {h:?}");
    let mut l = buggy.init_state();
    for a in [
        "Advance",
        "GoalPause",
        "PauseTakes",
        "Took",
        "TurnEnds",
        "LadderMove",
        "LetGoPaused",
    ] {
        assert!(buggy.fire(a, &mut l), "{a}");
    }
    assert!(!buggy.check_invariant("NeverPausedPastMove", &l));
    let mut t = buggy.init_state();
    for a in [
        "Advance",
        "GoalPause",
        "PauseTakes",
        "Took",
        "TurnEnds",
        "ContinuePaused",
    ] {
        assert!(buggy.fire(a, &mut t), "{a}");
    }
    assert!(!buggy.check_invariant("NothingTypedOverThePause", &t));
}

/// "THE UPGRADE LANDS UNDER A GOAL" under its stated fairness (the clock, the
/// lane's look and acts, and Codex taking a typed pause), and each named
/// mutant breaks it ALONE: the rule before the owner's decision, a goal
/// holding its tab for as long as it runs (`GoalHoldsTheTab`), the host
/// letting a current tab go with its goal left paused (`LetGoPaused`), and
/// the supervisor typing a turn onto the paused thread at each idle point
/// (`ContinuePaused`).
#[test]
fn the_upgrade_lands_under_a_goal_and_leaves_it_pursued() {
    let model = harness_upgrade_goal_pause_model();
    let live = harness_upgrade_goal_pause_liveness();
    assert!(
        aterm_spec::xref::liveness_registry()
            .iter()
            .any(|(m, l)| m.name == model.name && l.name == live.name)
    );
    verify::liveness_proves_and_catches_tiered(&model, &live, "HarnessUpgradeGoalPause lands");

    let held = verify::buggy_baseline_with(&model, "GoalHoldsTheTab");
    let cycle = interp::nonprogress_under(&held, &live).expect("held for ever");
    assert_eq!(
        (
            cycle.entry()["pauses"],
            cycle.entry()["goal"],
            cycle.entry()["behind"]
        ),
        (0, 1, 1),
        "{cycle}"
    );
    assert!(!cycle.moves.contains("LadderMove"), "{cycle}");

    // The supervisor typing a turn onto the paused thread at each of its
    // idle points: the move never finds the thread idle.
    let typed = verify::buggy_baseline_with(&model, "ContinuePaused");
    let cycle = interp::nonprogress_under(&typed, &live).expect("kept busy for ever");
    assert_eq!(
        (
            cycle.entry()["held"],
            cycle.entry()["goal"],
            cycle.entry()["behind"]
        ),
        (2, 2, 1),
        "{cycle}"
    );
    assert!(!cycle.moves.contains("LadderMove"), "{cycle}");

    let left = verify::buggy_baseline_with(&model, "LetGoPaused");
    let cycle = interp::nonprogress_under(&left, &live).expect("left paused for ever");
    assert_eq!(
        (
            cycle.entry()["looked"],
            cycle.entry()["held"],
            cycle.entry()["behind"]
        ),
        (0, 2, 0),
        "{cycle}"
    );
}

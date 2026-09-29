// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::{harness_upgrade_ladder_liveness, harness_upgrade_ladder_model};
use aterm_spec::{interp, verify};

/// THE LIVE UPGRADE'S LADDER (the owner's decision of 2026-09-28): every
/// floor holds at every rung — never over a draft, a box, a keystroke within
/// 20 s or running work of this client's own — a look at a pause the ladder
/// counts from Settled never waits for a screen that only repainted, and a
/// turn in ANOTHER session on the same daemon holds the client only until
/// its screen has placed it there, which it never does under a goal of its
/// own (the second review of 2026-09-28: the goal's own turns run unseen,
/// and the rule that placed through them ended the client mid-goal), nor for
/// a subagent's turn or a root's with no other Codex attached (the third
/// review: the owner's tab's own subagents, taken for another session's).
/// Each invariant is caught by its own
/// `Buggy` member, and each dead action ALONE by an invariant — the audit
/// `aterm-gui`'s `spec_xref_closure` runs (`verify::audit_dead_negative_controls`:
/// the first cut's liveness-only mutants failed it). Tier-1 against the real
/// gate is in aterm-agent's `harness::upgrade_drive` ladder tests.
#[test]
fn the_ladder_keeps_every_floor_and_counts_a_quiet_looking_pause() {
    let model = harness_upgrade_ladder_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade ladder");
    // EVERY dead action is a negative control some invariant catches ALONE.
    let dead: Vec<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !interp::fired_actions(&interp::with_buggy(&model, 0)).contains(a))
        .collect();
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &dead),
        Ok(11),
        "{dead:?}"
    );

    // A repainting screen whose words stand still: Prefer waits for the
    // screen's own quiet, Settled takes the reader's run of looks.
    let mut s = model.init_state();
    assert!(model.fire("Look", &mut s));
    assert!(model.fire("Look", &mut s));
    assert!(model.fire("Repaint", &mut s));
    assert!(
        !model.action_enabled("Move", &s),
        "Prefer: the screen's quiet"
    );
    assert!(model.fire("Advance", &mut s));
    assert!(
        model.action_enabled("Move", &s),
        "Settled: the words' stillness"
    );

    // A person near the tab (not typing) holds it until KeysOnly.
    let mut near = model.init_state();
    assert!(model.fire("Type", &mut near));
    assert!(model.fire("KeysPause", &mut near));
    assert!(model.fire("Still", &mut near));
    for _ in 0..2 {
        assert!(!model.action_enabled("Move", &near));
        assert!(model.fire("Advance", &mut near));
    }
    assert!(model.action_enabled("Move", &near), "KeysOnly");

    // At Land the first pause, no settle — but never within the keys gap,
    // over a draft, a box, or a running turn of this client's own.
    let mut land = model.init_state();
    for _ in 0..3 {
        assert!(model.fire("Advance", &mut land));
    }
    assert!(model.action_enabled("Move", &land));
    for (hold, release) in [
        ("Type", "KeysPause"),
        ("DraftTyped", "DraftCleared"),
        ("BoxUp", "BoxDown"),
        ("TurnStarts", "TurnEnds"),
    ] {
        let mut s = land.clone();
        assert!(model.fire(hold, &mut s), "{hold}");
        assert!(!model.action_enabled("Move", &s), "{hold}");
        assert!(model.fire(release, &mut s), "{release}");
        assert!(model.action_enabled("Move", &s), "{release}");
    }

    // ANOTHER SESSION'S TURN holds it until two looks of one still run found
    // it running — then never again, running or not; the pin waits for it.
    let mut other = land.clone();
    assert!(model.fire("OtherStarts", &mut other));
    assert!(!model.action_enabled("Move", &other), "not placed yet");
    assert!(model.fire("Look", &mut other));
    assert!(
        !model.action_enabled("Move", &other),
        "one look places nothing"
    );
    assert!(model.fire("Look", &mut other));
    assert!(
        model.action_enabled("Move", &other),
        "placed: it holds nothing"
    );
    assert!(
        !model.action_enabled("Pin", &other),
        "the pin restarts the daemon"
    );
    // Its goal ends and starts again between two looks: still placed.
    assert!(model.fire("OtherEnds", &mut other));
    assert!(model.fire("OtherStarts", &mut other));
    assert!(model.action_enabled("Move", &other));
    // A turn of this client's own ends the still run, and with it the place.
    assert!(model.fire("TurnStarts", &mut other));
    assert!(model.fire("TurnEnds", &mut other));
    assert!(!model.action_enabled("Move", &other));

    // UNDER A GOAL on this screen nothing is placed: another session's turn
    // holds it through any number of looks, while this client's own goal
    // turn runs unseen under the still run — the looks go on reading it.
    let mut goal = land.clone();
    assert!(model.fire("GoalPursued", &mut goal));
    assert!(model.fire("OtherStarts", &mut goal));
    assert!(model.fire("Look", &mut goal));
    assert!(model.fire("Look", &mut goal));
    assert_eq!(goal["other_seen"], 0, "no look places under a goal");
    assert!(!model.action_enabled("Move", &goal), "not placed");
    assert!(model.fire("TurnStarts", &mut goal));
    assert_eq!(goal["seen"], 2, "its own turn ran unseen: the run stands");
    assert!(!model.action_enabled("Move", &goal));
    assert!(
        !model.action_enabled("GoalPaused", &goal),
        "its footer stands until its turn has ended"
    );
    assert!(model.fire("TurnEnds", &mut goal));
    assert!(model.fire("GoalPaused", &mut goal));
    assert!(
        !model.action_enabled("Move", &goal),
        "the goal paused: placed only by looks after it"
    );
    assert!(model.fire("Look", &mut goal));
    assert!(model.fire("Look", &mut goal));
    assert!(model.action_enabled("Move", &goal), "placed");
    // A SUBAGENT of this client's conversation (the owner's daemon of
    // 2026-09-28) — or any turn the lane never places: its turn never
    // draws, the looks go on reading the screen still, and it holds the move
    // through any number of them, placed never; the pin waits for it too.
    // Ended, the move is back.
    let mut sub = land.clone();
    assert!(model.fire("HiddenStarts", &mut sub));
    assert!(model.fire("Look", &mut sub));
    assert!(model.fire("Look", &mut sub));
    assert_eq!(sub["seen"], 2, "it drew nothing: the run stands");
    assert!(!model.action_enabled("Move", &sub), "never placed");
    assert!(!model.action_enabled("Pin", &sub));
    assert!(model.fire("HiddenEnds", &mut sub));
    assert!(model.action_enabled("Move", &sub));
    // Without another session, the goal's turns alone: it moves between
    // them (the kernel names the thread), never during one.
    let mut own = land.clone();
    assert!(model.fire("GoalPursued", &mut own));
    assert!(model.fire("TurnStarts", &mut own));
    assert!(!model.action_enabled("Move", &own));
    assert!(model.fire("TurnEnds", &mut own));
    assert!(model.action_enabled("Move", &own));

    // NEGATIVE CONTROLS: each unsafe move breaks its own invariant.
    let buggy = interp::with_buggy(&model, 1);
    for (hold, mutant, invariant) in [
        ("Type", "NowWaivesThePerson", "NeverWithinKeysGap"),
        ("DraftTyped", "MoveOverADraft", "NeverOverADraft"),
        ("BoxUp", "MoveOverABox", "NeverOverABox"),
        ("TurnStarts", "MoveMidTurn", "NoRunningWorkInterrupted"),
    ] {
        let mut cut = buggy.init_state();
        for _ in 0..3 {
            assert!(buggy.fire("Advance", &mut cut));
        }
        assert!(buggy.fire(hold, &mut cut), "{hold}");
        assert!(buggy.fire(mutant, &mut cut), "{mutant}");
        assert!(!buggy.check_invariant(invariant, &cut), "{invariant}");
    }
    // The still run placing through a goal: this client's own goal turn,
    // unseen through two looks, ended mid-goal.
    let mut placed = buggy.init_state();
    assert!(buggy.fire("Advance", &mut placed));
    assert!(buggy.fire("GoalPursued", &mut placed));
    assert!(buggy.fire("TurnStarts", &mut placed));
    assert!(buggy.fire("Look", &mut placed));
    assert!(buggy.fire("Look", &mut placed));
    assert!(!buggy.action_enabled("Move", &placed));
    assert!(buggy.fire("PlacedUnderAGoal", &mut placed));
    assert!(!buggy.check_invariant("NoRunningWorkInterrupted", &placed));
    // The still run placing as it ran (the round before the third review):
    // this client's own subagent, running through two looks, taken for
    // another session's, and the client ended over it.
    let mut ran = buggy.init_state();
    assert!(buggy.fire("Advance", &mut ran));
    assert!(buggy.fire("HiddenStarts", &mut ran));
    assert!(buggy.fire("Look", &mut ran));
    assert!(buggy.fire("Look", &mut ran));
    assert!(!buggy.action_enabled("Move", &ran));
    assert!(buggy.fire("PlacedAsItRan", &mut ran));
    assert!(!buggy.check_invariant("NoRunningWorkInterrupted", &ran));
    let mut seq = buggy.init_state();
    assert!(buggy.fire("Look", &mut seq));
    assert!(buggy.fire("Look", &mut seq));
    assert!(buggy.fire("Advance", &mut seq));
    assert!(buggy.fire("SeqSettle", &mut seq));
    assert!(!buggy.check_invariant("QuietPauseCounts", &seq));
    // …and each livelock is also a bad STATE the moment its mutant decides.
    let mut keys = buggy.init_state();
    for _ in 0..2 {
        assert!(buggy.fire("Advance", &mut keys));
    }
    assert!(buggy.fire("Type", &mut keys));
    assert!(buggy.fire("KeysPause", &mut keys));
    assert!(buggy.fire("AttendedWithoutLadder", &mut keys));
    assert!(!buggy.check_invariant("TheGraceNarrowsAtKeysOnly", &keys));
    let mut mark = buggy.init_state();
    assert!(buggy.fire("MarkChanges", &mut mark));
    assert!(buggy.fire("BlindReader", &mut mark));
    assert!(!buggy.check_invariant("EitherMarkIsRead", &mark));
    let mut held = buggy.init_state();
    assert!(buggy.fire("OtherStarts", &mut held));
    assert!(buggy.fire("Look", &mut held));
    assert!(buggy.fire("Look", &mut held));
    assert!(buggy.fire("OtherTurnHolds", &mut held));
    assert!(!buggy.check_invariant("AnotherSessionsTurnHoldsNothing", &held));
    let mut pin = buggy.init_state();
    for _ in 0..3 {
        assert!(buggy.fire("Advance", &mut pin));
    }
    assert!(buggy.fire("Move", &mut pin));
    assert!(buggy.fire("PinDropsDue", &mut pin));
    assert!(!buggy.check_invariant("AnUnpinnedDaemonKeepsItsTabDue", &pin));
}

/// "THE UPGRADE LANDS" under its stated fairness (the clock, the lane's look
/// and its move, and the pin), and each named mutant breaks it ALONE —
/// livelocks all, which no deadlock check sees: a person near the tab for
/// ever (`AttendedWithoutLadder`), a mark the reader never reads
/// (`BlindReader`), another session's goal for ever (`OtherTurnHolds`), and a
/// current, unpinned daemon nobody's step visits (`PinDropsDue`).
#[test]
fn the_upgrade_lands_under_fairness_and_catches_the_livelocks() {
    let model = harness_upgrade_ladder_model();
    let live = harness_upgrade_ladder_liveness();
    assert!(
        aterm_spec::xref::liveness_registry()
            .iter()
            .any(|(m, l)| m.name == model.name && l.name == live.name)
    );
    verify::liveness_proves_and_catches_tiered(&model, &live, "HarnessUpgradeLadder lands");
    let moved = |state: &interp::State| state["behind"] == 0;

    // A person back within the grace for ever: nothing is stuck, and the move
    // never comes.
    let near = verify::buggy_baseline_with(&model, "AttendedWithoutLadder");
    assert_eq!(interp::find_deadlock(&near, moved), None);
    let cycle = interp::nonprogress_under(&near, &live).expect("held for ever");
    assert_eq!(
        (cycle.entry()["narrows"], cycle.entry()["keys"]),
        (0, 1),
        "{cycle}"
    );

    // The mark the reader never reads: the mark `»` whenever the lane could
    // look, and the move never taken.
    let blind = verify::buggy_baseline_with(&model, "BlindReader");
    let cycle = interp::nonprogress_under(&blind, &live).expect("blind for ever");
    assert_eq!(
        (cycle.entry()["reads"], cycle.entry()["glyph"]),
        (0, 1),
        "{cycle}"
    );
    assert!(!cycle.moves.contains("Move"), "{cycle}");

    // Another tab's goal for ever: this idle client held for as long.
    let held = verify::buggy_baseline_with(&model, "OtherTurnHolds");
    let cycle = interp::nonprogress_under(&held, &live).expect("held for ever");
    assert_eq!(
        (
            cycle.entry()["scoped"],
            cycle.entry()["other"],
            cycle.entry()["behind"]
        ),
        (0, 1, 1),
        "{cycle}"
    );

    // An unpinned daemon nobody's step visits: moved, idle, never pinned.
    let pin = verify::buggy_baseline_with(&model, "PinDropsDue");
    let cycle = interp::nonprogress_under(&pin, &live).expect("unpinned for ever");
    assert_eq!(
        (
            cycle.entry()["behind"],
            cycle.entry()["kept"],
            cycle.entry()["pinned"],
            cycle.entry()["work"],
            cycle.entry()["other"]
        ),
        (0, 0, 0, 0, 0),
        "{cycle}"
    );
}

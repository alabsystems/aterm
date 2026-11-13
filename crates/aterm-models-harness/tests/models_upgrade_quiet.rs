// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `HarnessUpgradeQuiet`: its move laws read the state a move leaves, so they hold a
//! move to its guard whichever action makes it; each mutant breaks its own law alone; and the
//! deadline a look voids is told again. Tier 1 is `aterm-agent`'s `upgrade_quiet_tests`. The
//! non-vacuity sweep and the liveness audit are `aterm-models`' `non_vacuity`.

use aterm_models_harness::{
    harness_upgrade_quiet_liveness, harness_upgrade_quiet_model, harness_upgrade_wall_liveness,
};
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::{only, walk};

/// Each mutant and the law it was written to break.
const MUTANTS: [(&str, &str); 16] = [
    ("TellOffAPoint", "NoTellOffAPoint"),
    ("TellOverAPerson", "NoTellOverAPerson"),
    ("TellsEarly", "TellsOnlyPastTheBound"),
    ("TellsPastTheCap", "TellsBounded"),
    ("MoveOverWork", "NoUnwarnedMoveEndsWork"),
    ("MoveAtABreak", "NoUnwarnedMoveEndsWork"),
    ("WaitsDropped", "EndedWaitsAreCarried"),
    ("DeadlineUntold", "NoDeadlineUntold"),
    ("TellNeverTyped", "TellsAreTyped"),
    ("OwnWorkHolds", "OwnWorkYields"),
    ("ManagerHoldsCarry", "ManagersNeverHoldTheCarry"),
    ("SilentWorkHolds", "SilentWorkYields"),
    ("SilentMovesEarly", "NoUnwarnedMoveEndsWork"),
    ("TellsASilentSession", "NoTellIntoASilentSession"),
    ("MovesAtALiveWall", "NoUnwarnedMoveEndsWork"),
    ("WallHoldsForGood", "WallsYield"),
];

/// `m` with every mutant removed but `keep`.
fn alone(m: &Model, keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || MUTANTS.iter().all(|(mutant, _)| *mutant != a.name));
    one
}

/// The move laws are no flags a mutant sets: at every state the round reaches unmoved, the state
/// a move would leave there is judged by what it moved over. A move over real work or follows, or
/// at a break with nothing named, before any deadline passed breaks `NoUnwarnedMoveEndsWork`
/// (outside a silent session's outlasted move over follows, which no move over real work is);
/// one over waits that carried none breaks `EndedWaitsAreCarried`; and a deadline passed that no
/// line named breaks `NoDeadlineUntold`. So `MoveAtIdle` or `MoveOverOwn` with a guard widened past
/// the law, or `MoveOverOwn` leaving the waits behind, would fail at the committed dial.
/// NEGATIVE CONTROL: the same states left unmoved, and each law holds.
#[test]
fn the_move_laws_judge_the_state_a_move_leaves() {
    let m = interp::with_buggy(&harness_upgrade_quiet_model(), 0);
    assert!(
        interp::violated_invariants(&m).is_empty(),
        "every law holds"
    );
    let (mut unwarned, mut at_a_break, mut dropped, mut untold) = (0, 0, 0, 0);
    let mut silent_work = 0;
    for s in interp::reachable_states(&m)
        .into_iter()
        .filter(|s| s["moved"] == 0)
    {
        for law in [
            "NoUnwarnedMoveEndsWork",
            "EndedWaitsAreCarried",
            "NoDeadlineUntold",
        ] {
            assert!(m.check_invariant(law, &s), "{law} unmoved: {s:?}");
        }
        let mut moved = s.clone();
        moved.insert("moved", 1);
        let warned = m.check_invariant("NoUnwarnedMoveEndsWork", &moved);
        let aged_wall = s["posture"] == 3 && s["dated"] == 1 && s["aged"] == 1;
        let ends_unwarned = s["due"] == 0
            && (s["work"] >= 2 || (s["work"] == 0 && s["posture"] != 2 && !aged_wall))
            && !(s["silent"] == 1 && s["outlasted"] == 1 && s["tells"] == 0 && s["work"] != 2);
        assert_eq!(warned, !ends_unwarned, "{s:?}");
        unwarned += usize::from(s["work"] == 2 && !warned);
        silent_work += usize::from(
            s["work"] == 2 && s["silent"] == 1 && s["outlasted"] == 1 && s["tells"] == 0 && !warned,
        );
        at_a_break += usize::from(s["work"] == 0 && s["posture"] == 1 && !warned);
        // The same move with the waits' commands left behind.
        assert_eq!(
            m.check_invariant("EndedWaitsAreCarried", &moved),
            s["work"] != 1,
            "{s:?}"
        );
        let mut carried = moved.clone();
        carried.insert("carried", 1);
        assert!(m.check_invariant("EndedWaitsAreCarried", &carried), "{s:?}");
        dropped += usize::from(s["work"] == 1);
        let mut passed = s.clone();
        passed.insert("due", 1);
        assert_eq!(
            m.check_invariant("NoDeadlineUntold", &passed),
            s["tells"] > 0,
            "{s:?}"
        );
        untold += usize::from(s["tells"] == 0);
    }
    assert!(
        unwarned > 0 && at_a_break > 0 && dropped > 0 && untold > 0 && silent_work > 0,
        "{unwarned} {at_a_break} {dropped} {untold} {silent_work}"
    );
}

#[test]
fn each_mutant_breaks_its_own_law_alone() {
    let m = harness_upgrade_quiet_model();
    let committed = interp::fired_actions(&interp::with_buggy(&m, 0));
    for (mutant, law) in MUTANTS {
        assert!(!committed.contains(mutant), "{mutant} is dead at Buggy = 0");
        let one = alone(&m, mutant);
        assert_eq!(
            interp::violated_invariants(&interp::with_buggy(&one, 1)),
            std::collections::BTreeSet::from([law]),
            "{mutant} breaks {law} and no other law"
        );
        assert!(
            interp::violated_invariants(&interp::with_buggy(&only(&one, law), 0)).is_empty(),
            "{law} holds"
        );
    }
    let laws: Vec<&str> = MUTANTS.iter().map(|(_, law)| *law).collect();
    for inv in &m.invariants {
        assert!(laws.contains(&inv.name), "{} has its own mutant", inv.name);
    }
    let walls = harness_upgrade_wall_liveness();
    for mutant in harness_upgrade_quiet_liveness()
        .mutants
        .iter()
        .chain(walls.mutants.iter())
    {
        assert!(
            MUTANTS.iter().any(|(name, _)| name == mutant),
            "{mutant}, a liveness mutant, has an invariant twin"
        );
    }
}

/// A deadline is over one unbroken hold: a person at the session voids it and its line, and the
/// work that holds the round again is told again; a deadline nothing broke moves the round.
#[test]
fn a_voided_deadline_is_told_again_and_one_that_stands_moves_the_round() {
    let m = harness_upgrade_quiet_model();
    let told = [
        "RealWorkStarts",
        "BreakSettles",
        "HoldOutlasts",
        "TellAtPoint",
        "DeadlinePasses",
    ];
    let due = walk(&m, &told);
    assert_eq!((due["tells"], due["due"]), (1, 1));
    assert!(m.action_enabled("MoveOverOwn", &due));
    assert!(
        !m.action_enabled("DeadlineVoids", &due),
        "the work it named still runs, nobody there"
    );
    let mut person = told.to_vec();
    person.extend(["PersonHolds", "DeadlineVoids"]);
    let voided = walk(&m, &person);
    assert_eq!(
        (voided["tells"], voided["due"], voided["held"]),
        (0, 0, 0),
        "the line, its deadline and the hold"
    );
    assert!(!m.action_enabled("MoveOverOwn", &voided));
    person.push("BreakSettles");
    let back = walk(&m, &person);
    assert!(
        !m.action_enabled("MoveOverOwn", &back) && !m.action_enabled("TellAtPoint", &back),
        "the hold starts again: no move on the old line, no line before the bound"
    );
    person.extend(["HoldOutlasts", "TellAtPoint"]);
    let again = walk(&m, &person);
    assert_eq!((again["tells"], again["due"]), (1, 0), "told its own");
    // The work ending by itself at an idle point voids it too: what starts later is other work.
    let mut over = told.to_vec();
    over.extend(["WorkEnds", "IdlePoint", "DeadlineVoids", "RealWorkStarts"]);
    let other = walk(&m, &over);
    assert!(!m.action_enabled("MoveOverOwn", &other), "{other:?}");
}

/// A person's hold on the carry holds the tell and the move over the agent's own wherever the
/// round reaches it unmoved, and a manager's wait holds neither: a managed worker over its waits
/// moves, and one whose work has held it is told. NEGATIVE CONTROL: `ManagerHoldsCarry` leaves a
/// managed worker over its waits with no move and no tell.
#[test]
fn a_persons_hold_on_the_carry_holds_it_and_a_managers_wait_does_not() {
    let m = harness_upgrade_quiet_model();
    let committed = interp::with_buggy(&m, 0);
    let (mut held, mut managed) = (0, 0);
    for s in interp::reachable_states(&committed)
        .into_iter()
        .filter(|s| s["moved"] == 0)
    {
        if s["off"] == 1 {
            assert!(
                !m.action_enabled("MoveOverOwn", &s) && !m.action_enabled("TellAtPoint", &s),
                "{s:?}"
            );
            held += 1;
        } else if s["managed"] == 1 {
            let mut free = s.clone();
            free.insert("managed", 0);
            for action in ["MoveOverOwn", "TellAtPoint"] {
                assert_eq!(
                    m.action_enabled(action, &s),
                    m.action_enabled(action, &free),
                    "{action} at {s:?}"
                );
            }
            managed += usize::from(m.action_enabled("MoveOverOwn", &s));
        }
    }
    assert!(held > 0 && managed > 0, "{held} {managed}");
    let over_waits = walk(&m, &["ManagerWaits", "WaitStarts", "BreakSettles"]);
    assert!(m.action_enabled("MoveOverOwn", &over_waits));
    let buggy = interp::with_buggy(&alone(&m, "ManagerHoldsCarry"), 1);
    let mut stuck = over_waits.clone();
    assert!(buggy.fire("ManagerHoldsCarry", &mut stuck));
    assert!(
        !buggy.action_enabled("MoveOverOwn", &stuck)
            && !buggy.action_enabled("TellAtPoint", &stuck),
        "{stuck:?}"
    );
}

/// A session nothing is typed into is never told a deadline, and its own follows hold its round
/// only until that hold outlasts the bound a told deadline would have: then the round moves over
/// them at its next point, nobody near and no deadline told before standing; a person near holds
/// it and their look breaks the hold. Its real work holds the round past that bound, as work that
/// would be lost. NEGATIVE CONTROL: `SilentWorkHolds` leaves the follows held.
#[test]
fn a_silent_sessions_follows_are_moved_over_once_their_hold_outlasts_a_told_deadline() {
    let m = harness_upgrade_quiet_model();
    let held = walk(
        &m,
        &["TaskLands", "FollowStarts", "BreakSettles", "HoldOutlasts"],
    );
    assert!(!m.action_enabled("TellAtPoint", &held), "no line: {held:?}");
    assert!(!m.action_enabled("MoveOverOwn", &held), "not yet: {held:?}");
    let outlasted = walk(
        &m,
        &[
            "TaskLands",
            "FollowStarts",
            "BreakSettles",
            "HoldOutlasts",
            "SilentHoldOutlasts",
        ],
    );
    assert!(m.action_enabled("MoveOverOwn", &outlasted), "{outlasted:?}");
    for at in ["BreakSettles", "IdlePoint"] {
        let work = walk(
            &m,
            &[
                "TaskLands",
                "RealWorkStarts",
                at,
                "HoldOutlasts",
                "SilentHoldOutlasts",
            ],
        );
        assert!(
            !m.action_enabled("MoveOverOwn", &work) && !m.action_enabled("TellAtPoint", &work),
            "real work holds a silent round: {work:?}"
        );
    }
    let mut near = outlasted.clone();
    assert!(m.fire("PersonNear", &mut near));
    assert!(!m.action_enabled("MoveOverOwn", &near), "{near:?}");
    assert!(m.fire("HoldBreaks", &mut near));
    assert_eq!((near["held"], near["outlasted"]), (0, 0), "{near:?}");
    let told = walk(
        &m,
        &[
            "FollowStarts",
            "BreakSettles",
            "HoldOutlasts",
            "TellAtPoint",
            "TaskLands",
            "SilentHoldOutlasts",
        ],
    );
    assert!(
        !m.action_enabled("MoveOverOwn", &told),
        "a told deadline governs: {told:?}"
    );
    let buggy = interp::with_buggy(&alone(&m, "SilentWorkHolds"), 1);
    let mut stuck = outlasted.clone();
    assert!(buggy.fire("SilentWorkHolds", &mut stuck));
    assert!(!buggy.action_enabled("MoveOverOwn", &stuck), "{stuck:?}");
}

/// A wall its transcript dates (a login wall, a usage limit) moves an idle round once it has stood
/// its bound with nobody near, and not before; a person near restarts its clock, the agent's own
/// work holds it, and a wall only the screen shows never ages. NEGATIVE CONTROL:
/// `WallHoldsForGood` leaves the aged wall unmoved.
#[test]
fn a_dated_wall_moves_an_idle_round_once_it_has_stood_its_bound() {
    let m = harness_upgrade_quiet_model();
    let wall = walk(&m, &["DatedWallStands"]);
    assert!(
        !m.action_enabled("MoveAtIdle", &wall),
        "within its bound: {wall:?}"
    );
    let aged = walk(&m, &["DatedWallStands", "WallAges"]);
    assert!(m.action_enabled("MoveAtIdle", &aged), "{aged:?}");
    let mut near = aged.clone();
    assert!(m.fire("PersonNear", &mut near));
    assert!(
        !m.action_enabled("MoveAtIdle", &near),
        "a person near: {near:?}"
    );
    let working = walk(&m, &["WaitStarts", "DatedWallStands", "WallAges"]);
    assert!(!m.action_enabled("MoveAtIdle", &working), "{working:?}");
    let undated = walk(&m, &["WallStands"]);
    assert!(!m.action_enabled("WallAges", &undated), "{undated:?}");
    let buggy = interp::with_buggy(&alone(&m, "WallHoldsForGood"), 1);
    let mut stuck = aged.clone();
    assert!(buggy.fire("WallHoldsForGood", &mut stuck));
    assert!(!buggy.action_enabled("MoveAtIdle", &stuck), "{stuck:?}");
}

/// Both liveness laws hold under the fairness they name and no less, and each mutant they name
/// breaks them alone; the sweep in `aterm-models` reads the same registry.
#[test]
fn the_liveness_laws_hold_and_their_mutants_break_them() {
    let m = harness_upgrade_quiet_model();
    for (live, mutants) in [
        (harness_upgrade_quiet_liveness(), 4),
        (harness_upgrade_wall_liveness(), 1),
    ] {
        assert_eq!(
            aterm_spec::verify::audit_liveness(&m, &live),
            Ok(mutants),
            "{}",
            live.name
        );
    }
}

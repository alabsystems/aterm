// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `HarnessUpgradeQuiet`: its move laws read the state a move leaves, so they hold a
//! move to its guard whichever action makes it; each mutant breaks its own law alone; and the
//! deadline a look voids is told again. Tier 1 is `aterm-agent`'s `upgrade_quiet_tests`. The
//! non-vacuity sweep and the liveness audit are `aterm-models`' `non_vacuity`.

use aterm_models_harness::{harness_upgrade_quiet_liveness, harness_upgrade_quiet_model};
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::{only, walk};

/// Each mutant and the law it was written to break.
const MUTANTS: [(&str, &str); 10] = [
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
];

/// `m` with every mutant removed but `keep`.
fn alone(m: &Model, keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || MUTANTS.iter().all(|(mutant, _)| *mutant != a.name));
    one
}

/// The move laws are no flags a mutant sets: at every state the round reaches unmoved, the state
/// a move would leave there is judged by what it moved over. A move over real work, or at a
/// break with nothing named, before any deadline passed breaks `NoUnwarnedMoveEndsWork`; one over
/// waits that carried none breaks `EndedWaitsAreCarried`; and a deadline passed that no line
/// named breaks `NoDeadlineUntold`. So `MoveAtIdle` or `MoveOverOwn` with a guard widened past
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
        let ends_unwarned =
            s["due"] == 0 && (s["work"] == 2 || (s["work"] == 0 && s["posture"] != 2));
        assert_eq!(warned, !ends_unwarned, "{s:?}");
        unwarned += usize::from(s["work"] == 2 && !warned);
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
        unwarned > 0 && at_a_break > 0 && dropped > 0 && untold > 0,
        "{unwarned} {at_a_break} {dropped} {untold}"
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
    for mutant in &harness_upgrade_quiet_liveness().mutants {
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

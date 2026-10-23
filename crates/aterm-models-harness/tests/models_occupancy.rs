// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `OccupancyDoor` (`docs/DESIGN-harness-occupancy-2026-10-20.md` §7, as the red-team
//! corrects it): proved at `Buggy = 0` in each autopilot (`Mode`, Cruise committed, Off
//! and Land its scenarios) and caught at `Buggy = 1`, each law broken by the mutant written to
//! break it with every other mutant removed, the liveness audited as the non-vacuity sweep audits
//! it, and walked along the paths the design names: a nudge at a hold of age, the upgrade's notice
//! first, a wake over the notice under Land, the K Entry, a hold that outlasts its breaks and a
//! harness line's answer, and a crash between the intent row and the line. Tier-1 is
//! `aterm-agent/tests/conformance_occupancy.rs`.

use aterm_models_harness::{occupancy_door_liveness, occupancy_door_model};
use aterm_spec::derive::Model;
use aterm_spec::walk::walk;
use aterm_spec::{interp, verify};

/// Each mutant, its dial, the law it was written to break, and the autopilot it breaks it under.
const MUTANTS: [(&str, i64, &str, i64); 22] = [
    ("RestartClock", 1, "TheClockNeverRestarts", 1),
    ("LoseDeadline", 1, "TheDeadlineStaysArmed", 1),
    ("TypeAtAnyDoor", 2, "NoLineIntoATurn", 1),
    ("TypeUnderPerson", 3, "NoLineOverAPerson", 1),
    ("TypeWithoutAJob", 4, "NoLineWithoutAJob", 1),
    ("TypeUnderOff", 5, "LinesKeepTheirModes", 0),
    ("TypeThroughBrake", 6, "BrakesFirst", 1),
    ("TypeOverUntaken", 7, "OneActAtATime", 1),
    ("TypeUnpaced", 8, "PaceHolds", 1),
    ("TypeWithoutRow", 9, "IntentBeforeLine", 1),
    ("TypeStale", 10, "NeverToldStale", 1),
    ("StackSecond", 11, "OneLinePerDoor", 1),
    ("TypeKeepsDue", 12, "OncePerEpisode", 1),
    ("SeedIntentAsTold", 13, "TheBookMatchesTheScreen", 1),
    ("UpgradeFirst", 14, "AWakeOutranksTheUpgrade", 1),
    ("LateRaise", 15, "RaisedByK", 1),
    ("RestartOnIdle", 16, "TheHoldOutlastsItsBreaks", 1),
    ("CloseOnOurTurn", 17, "TheHarnessTurnKeepsTheHold", 1),
    ("CloseAtTheAnswer", 18, "TheAnswersPointKeepsTheHold", 1),
    ("CloseEarly", 19, "AnEpisodeEndsWithItsCondition", 1),
    ("TypeOverNotice", 20, "TheNoticeGoesFirst", 1),
    ("NudgeUnheld", 21, "TheNudgeWaitsForItsAge", 1),
];

/// The model under autopilot `mode` (0 Off, 1 Cruise, 2 Land).
fn under(mode: i64) -> Model {
    interp::with_consts(&occupancy_door_model(), &[("Mode", mode)])
}

/// `m` with every mutant removed but `keep`.
fn alone(m: &Model, keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || MUTANTS.iter().all(|(mutant, ..)| *mutant != a.name));
    one
}

#[test]
fn occupancy_door_proves_and_catches() {
    let mut fired = std::collections::BTreeSet::new();
    for mode in [0, 1, 2] {
        let m = under(mode);
        assert!(
            interp::violated_invariants(&m).is_empty(),
            "every law holds under Mode = {mode}"
        );
        fired.extend(interp::fired_actions(&m));
    }
    for action in occupancy_door_model().actions.iter().map(|a| a.name) {
        let mutant = MUTANTS.iter().any(|(name, ..)| *name == action);
        assert_eq!(
            fired.contains(action),
            !mutant,
            "{action} fires with no mutant on exactly when it is no mutant: {fired:?}"
        );
    }
    verify::prove_and_catch_scalar(
        &occupancy_door_model(),
        "occupancy door: a line only at a break, past every guard, once an episode, the \
         upgrade's notice first but under a wake, raised by K looks, a hold's clock never \
         restarted and never ended by the harness's own line",
    );
}

/// Each mutant breaks the law it was written to break, alone, and the committed door keeps it.
#[test]
fn each_mutant_breaks_its_own_law_alone() {
    let model = occupancy_door_model();
    for (mutant, dial, law, mode) in MUTANTS {
        let m = alone(&under(mode), mutant);
        let wrong = interp::with_buggy(&aterm_spec::walk::only(&m, law), dial);
        assert!(
            interp::violated_invariants(&wrong).contains(&law),
            "{mutant} (Buggy = {dial}, Mode = {mode}) breaks {law}"
        );
        let right = interp::with_buggy(&aterm_spec::walk::only(&m, law), 0);
        assert!(
            interp::violated_invariants(&right).is_empty(),
            "the committed door keeps {law} under Mode = {mode}"
        );
    }
    let laws: Vec<&str> = MUTANTS.iter().map(|(_, _, law, _)| *law).collect();
    for inv in model.invariants.iter().filter(|inv| inv.name != "LooksBox") {
        assert!(laws.contains(&inv.name), "{} has its own mutant", inv.name);
    }
}

/// The non-vacuity sweep over the model's three configurations, and the liveness audit, as
/// `aterm-models`' `non_vacuity` runs them: each law caught, the bound unbroken, each dead
/// action an audited negative control, `TheDueIsToldOrRaised` proved with `Sample` and
/// `SampleLast` its only (and load-bearing) fairness, and each of its two mutants breaking it
/// alone.
#[test]
fn occupancy_door_is_non_vacuous_and_live() {
    let configs = [under(1), under(0), under(2)];
    let faults = verify::sweep_faults(&configs);
    assert!(faults.is_empty(), "{faults:#?}");
    verify::audit_liveness(&occupancy_door_model(), &occupancy_door_liveness())
        .expect("TheDueIsToldOrRaised holds, is minimal and is caught");
}

/// A NUDGE: a hold comes of age at a break, the nudge opens, its intent row goes before its one
/// line, and the line stands untaken and paced; a second line at that door, or in that episode,
/// is not enabled. Under Land the nudge is held (it is Cruise's); under Off nothing is.
#[test]
fn a_nudge_goes_once_at_a_break_under_cruise_only() {
    let m = under(1);
    let at_door = walk(&m, &["Break", "HoldAges", "OpenNudge", "Intend"]);
    assert!(m.action_enabled("Type", &at_door), "{at_door:?}");
    let told = walk(&m, &["Break", "HoldAges", "OpenNudge", "Intend", "Type"]);
    assert_eq!(
        (told["told"], told["wrote"], told["lines"], told["due"]),
        (1, 1, 1, 0)
    );
    assert_eq!((told["untaken"], told["paced"]), (1, 1));
    assert!(!m.action_enabled("OpenNudge", &told), "once an episode");
    assert!(
        !m.action_enabled("Close", &told),
        "not while its condition holds"
    );
    let early = interp::with_buggy(&m, 19);
    let mut again = told.clone();
    assert!(early.fire("CloseEarly", &mut again));
    assert!(
        early.action_enabled("OpenNudge", &again),
        "the same hold nudged again"
    );
    assert!(!early.check_invariant("AnEpisodeEndsWithItsCondition", &again));
    // Not before its hold's age.
    let young = walk(&m, &["Break", "Holds"]);
    assert!(!m.action_enabled("OpenNudge", &young), "{young:?}");
    let unheld = interp::with_buggy(&m, 21);
    let mut wrong = young.clone();
    assert!(unheld.fire("NudgeUnheld", &mut wrong));
    assert!(!unheld.check_invariant("TheNudgeWaitsForItsAge", &wrong));
    // Not into a turn: the same steps from a live turn have no line.
    let mut turn = walk(&m, &["Break", "HoldAges", "OpenNudge", "Intend"]);
    assert!(m.fire("Turn", &mut turn));
    assert!(!m.action_enabled("Type", &turn), "{turn:?}");
    assert!(m.action_enabled("Skip", &turn), "the intent is skipped");
    for mode in [0, 2] {
        let held = under(mode);
        let st = walk(&held, &["Break", "HoldAges", "OpenNudge", "Intend"]);
        assert!(!held.action_enabled("Type", &st), "Mode = {mode}: {st:?}");
    }
}

/// F11, DECIDED UPGRADE-FIRST: with the upgrade's notice due (`IdleHost::upgrade_due`, asked only
/// after the grant), a nudge waits and the notice is the break's one line; the next break has
/// the nudge. A wake outranks the notice: under Land it goes with the notice still due, and the
/// notice does not.
#[test]
fn the_upgrade_notice_goes_first_but_under_a_wake() {
    let m = under(1);
    let st = walk(
        &m,
        &["Break", "HoldAges", "OpenNudge", "Intend", "UpgradeDue"],
    );
    assert!(!m.action_enabled("Type", &st), "{st:?}");
    let over = interp::with_buggy(&m, 20);
    let mut wrong = st.clone();
    assert!(over.fire("TypeOverNotice", &mut wrong));
    assert!(!over.check_invariant("TheNoticeGoesFirst", &wrong));
    let noticed = walk(
        &m,
        &[
            "Break",
            "HoldAges",
            "OpenNudge",
            "Intend",
            "UpgradeDue",
            "UpgradeTypes",
        ],
    );
    assert!(!m.action_enabled("Type", &noticed), "one line a door");
    let next = walk(
        &m,
        &[
            "Break",
            "HoldAges",
            "OpenNudge",
            "Intend",
            "UpgradeDue",
            "UpgradeTypes",
            "IdleRead",
        ],
    );
    assert!(m.action_enabled("Type", &next), "{next:?}");
    let land = under(2);
    let wake = walk(
        &land,
        &["Break", "Holds", "OpenWake", "Intend", "UpgradeDue"],
    );
    assert!(land.action_enabled("Type", &wake), "{wake:?}");
    assert!(!land.action_enabled("UpgradeTypes", &wake), "{wake:?}");
    let first = interp::with_buggy(&land, 14);
    let wrong = walk(
        &first,
        &[
            "Break",
            "Holds",
            "OpenWake",
            "Intend",
            "UpgradeDue",
            "UpgradeFirst",
        ],
    );
    assert!(!first.check_invariant("AWakeOutranksTheUpgrade", &wrong));
}

/// THE K ENTRY: a due line untold for K looks is raised at the K-th; restarted, or with its
/// deadline lost, it is not.
#[test]
fn a_due_line_is_raised_at_its_kth_look() {
    let m = under(1);
    let mut st = walk(&m, &["Break", "HoldAges", "OpenNudge", "PersonIn"]);
    let k = m.consts.iter().find(|(name, _)| *name == "K").expect("K").1;
    for _ in 0..k - 1 {
        assert!(m.fire("Sample", &mut st), "{st:?}");
    }
    assert!(m.fire("SampleLast", &mut st), "{st:?}");
    assert_eq!((st["esc"], st["age"]), (1, k));
    let buggy = interp::with_buggy(&m, 1);
    let restarted = walk(
        &buggy,
        &[
            "Break",
            "HoldAges",
            "OpenNudge",
            "PersonIn",
            "Sample",
            "RestartClock",
        ],
    );
    assert_eq!(restarted["age"], 0);
    assert!(!buggy.check_invariant("TheClockNeverRestarts", &restarted));
    let lost = walk(
        &buggy,
        &[
            "Break",
            "HoldAges",
            "OpenNudge",
            "PersonIn",
            "LoseDeadline",
            "Sample",
        ],
    );
    assert!(!buggy.action_enabled("SampleLast", &lost), "{lost:?}");
    assert!(!buggy.check_invariant("TheDeadlineStaysArmed", &lost));
}

/// THE HOLD'S CLOCK (F7): a hold of age outlasts an idle read with no point (a new break, and a
/// new door), and ends only at a live turn or a point, its rules' condition with it; a clock
/// that restarts at the idle read ends it there.
#[test]
fn a_hold_outlasts_its_breaks() {
    let m = under(1);
    let st = walk(&m, &["Break", "HoldAges", "IdleRead", "IdleRead"]);
    assert_eq!((st["held"], st["cond"]), (1, 1));
    assert!(!m.action_enabled("HoldCloses", &st), "{st:?}");
    let ended = walk(&m, &["Break", "HoldAges", "Turn", "HoldCloses"]);
    assert_eq!((ended["held"], ended["cond"], ended["hbreak"]), (0, 0, 0));
    let restarts = interp::with_buggy(&m, 16);
    let wrong = walk(&restarts, &["Break", "HoldAges", "RestartOnIdle"]);
    assert_eq!(wrong["held"], 0);
    assert!(!restarts.check_invariant("TheHoldOutlastsItsBreaks", &wrong));
}

/// THE HARNESS'S OWN LINE (§3.1, F5): the turn that answers a line typed in a hold of age, and
/// the point that ends that answer, keep the hold; the turn after that point, or a break's read
/// of the answer, ends the answer, and a turn the harness did not type may end the hold. Closed
/// at the line's own turn, or at its answer's point, the hold breaks the law each copy breaks.
#[test]
fn a_harness_lines_answer_keeps_the_hold() {
    let m = under(1);
    let typed = ["Break", "HoldAges", "OpenNudge", "Intend", "Type", "Turn"];
    let turn = walk(&m, &typed);
    assert_eq!((turn["held"], turn["owed"]), (1, 1));
    assert!(!m.action_enabled("HoldCloses", &turn), "{turn:?}");
    let mut point = turn.clone();
    assert!(m.fire("Point", &mut point));
    assert!(!m.action_enabled("HoldCloses", &point), "{point:?}");
    let mut after = point.clone();
    assert!(m.fire("Turn", &mut after));
    assert_eq!(after["owed"], 0);
    assert!(m.action_enabled("HoldCloses", &after), "{after:?}");
    let mut read = turn.clone();
    assert!(m.fire("Break", &mut read));
    assert_eq!((read["held"], read["owed"]), (1, 0));
    let ours = interp::with_buggy(&m, 17);
    let mut wrong = turn.clone();
    assert!(ours.fire("CloseOnOurTurn", &mut wrong));
    assert!(!ours.check_invariant("TheHarnessTurnKeepsTheHold", &wrong));
    let answer = interp::with_buggy(&m, 18);
    let mut wrong = point.clone();
    assert!(answer.fire("CloseAtTheAnswer", &mut wrong));
    assert!(!answer.check_invariant("TheAnswersPointKeepsTheHold", &wrong));
}

/// A CRASH between the intent row and the line: re-seeded from the rows, the episode is untold,
/// and its one line still goes; seeded as told, the book says told and the screen never got it.
#[test]
fn a_crash_after_the_intent_leaves_the_episode_untold() {
    let m = under(1);
    let st = walk(
        &m,
        &[
            "Break",
            "HoldAges",
            "OpenNudge",
            "Intend",
            "Crash",
            "Intend",
            "Type",
        ],
    );
    assert_eq!((st["told"], st["wrote"]), (1, 1));
    let seeded = interp::with_buggy(&m, 13);
    let wrong = walk(
        &seeded,
        &[
            "Break",
            "HoldAges",
            "OpenNudge",
            "Intend",
            "SeedIntentAsTold",
        ],
    );
    assert_eq!((wrong["told"], wrong["wrote"]), (1, 0));
    assert!(!seeded.check_invariant("TheBookMatchesTheScreen", &wrong));
}

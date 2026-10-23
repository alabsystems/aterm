// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `OccupancyDoor`.

use aterm_models_harness::occupancy_door_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

const MUTANTS: [(&str, i64, &str, i64); 23] = [
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
    ("StuckWithoutKey", 22, "TheStuckLineNeedsItsKey", 1),
];

fn under(mode: i64) -> Model {
    interp::with_consts(&occupancy_door_model(), &[("Mode", mode)])
}

fn keyed(mode: i64) -> Model {
    interp::with_consts(&occupancy_door_model(), &[("Mode", mode), ("Key", 1)])
}

fn alone(m: &Model, keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || MUTANTS.iter().all(|(mutant, ..)| *mutant != a.name));
    one
}

#[test]
fn occupancy_door_fires_exactly_its_committed_moves() {
    let mut fired = std::collections::BTreeSet::new();
    for (mode, m) in [0, 1, 2]
        .map(|mode| (mode, under(mode)))
        .into_iter()
        .chain([1, 2].map(|mode| (mode, keyed(mode))))
    {
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
}

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
    let young = walk(&m, &["Break", "Holds"]);
    assert!(!m.action_enabled("OpenNudge", &young), "{young:?}");
    let unheld = interp::with_buggy(&m, 21);
    let mut wrong = young.clone();
    assert!(unheld.fire("NudgeUnheld", &mut wrong));
    assert!(!unheld.check_invariant("TheNudgeWaitsForItsAge", &wrong));
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
    let untaken = walk(
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
    assert_eq!(untaken["untaken"], 1, "{untaken:?}");
    assert!(!m.action_enabled("Type", &untaken), "{untaken:?}");
    let next = walk(
        &m,
        &[
            "Break",
            "HoldAges",
            "OpenNudge",
            "Intend",
            "UpgradeDue",
            "UpgradeTypes",
            "ActTaken",
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

#[test]
fn the_stuck_line_needs_its_key_and_goes_once_an_episode() {
    let m = keyed(1);
    let open = ["Break", "Holds", "OpenStuck", "Intend"];
    let at_door = walk(&m, &open);
    assert!(m.action_enabled("Type", &at_door), "{at_door:?}");
    assert!(
        !m.action_enabled("OpenWake", &walk(&m, &["Break", "Holds"])),
        "the key-on scenarios leave the wake out"
    );
    let then = |more: &[&'static str]| [&open[..], more].concat();
    let notice = walk(&m, &then(&["UpgradeDue"]));
    assert!(!m.action_enabled("Type", &notice), "{notice:?}");
    assert!(m.action_enabled("UpgradeTypes", &notice), "{notice:?}");
    let after = walk(
        &m,
        &then(&["UpgradeDue", "UpgradeTypes", "ActTaken", "IdleRead"]),
    );
    assert!(m.action_enabled("Type", &after), "{after:?}");
    let told = walk(&m, &then(&["Type"]));
    assert_eq!(
        (told["told"], told["wrote"], told["due"], told["skey"]),
        (1, 1, 0, 1)
    );
    assert!(!m.action_enabled("OpenStuck", &told), "once an episode");
    let reopened = walk(&m, &then(&["Type", "Fails", "Close", "Holds"]));
    assert!(m.action_enabled("OpenStuck", &reopened), "{reopened:?}");
    let land = keyed(2);
    assert!(land.action_enabled("Type", &walk(&land, &open)));
    let off = keyed(0);
    assert!(!off.action_enabled("Type", &walk(&off, &open)));
    let unkeyed = under(1);
    let held = walk(&unkeyed, &["Break", "Holds"]);
    assert!(!unkeyed.action_enabled("OpenStuck", &held), "{held:?}");
    let ignored = interp::with_buggy(&unkeyed, 22);
    let wrong = walk(
        &ignored,
        &["Break", "Holds", "StuckWithoutKey", "Intend", "Type"],
    );
    assert_eq!((wrong["wrote"], wrong["skey"]), (1, 0));
    assert!(!ignored.check_invariant("TheStuckLineNeedsItsKey", &wrong));
}

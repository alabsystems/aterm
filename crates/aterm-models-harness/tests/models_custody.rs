// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `LineCustody`: it holds at `Buggy = 0` in its committed configuration and in each
//! scenario (a host that serves no input word, no restart lane), each law breaks under its own
//! mutant alone, and walks follow the paths ATERM-KI-1101-3 names. Tier 1 is
//! `aterm-agent/tests/conformance_custody.rs`. The non-vacuity sweep is `aterm-models`'
//! `non_vacuity`.

use aterm_models_harness::line_custody_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

/// Each mutant, its dial and the law it was written to break.
const MUTANTS: [(&str, i64, &str); 10] = [
    ("SettleOnMove", 1, "NoTwoCopies"),
    ("SettleWithoutTurn", 2, "NoTwoCopies"),
    ("TurnSettlesUnread", 3, "NoTwoCopies"),
    ("RestartUnread", 4, "ARestartHandsTheShellNothing"),
    ("LostWithinMinute", 5, "TheHostWaitsOnALineThatMayCome"),
    ("RestartAgain", 6, "OneRestartPerLine"),
    ("HostOverStands", 7, "TheHostWaitsOnALineThatMayCome"),
    ("WriteWhileHeld", 8, "NoTwoCopies"),
    ("RestartBehind", 9, "ARestartHandsTheShellNothing"),
    ("UpgradeOverStands", 10, "ARestartHandsTheShellNothing"),
];

/// A bound on the states the model reaches at any dial.
const STATE_BOUND: usize = 20_000;

/// The steps a host that serves no input word never takes: no read says the line was read.
const UNSERVED: [&str; 7] = [
    "AsksRestart",
    "Minute",
    "RestartDeferred",
    "RestartMade",
    "RestartUnmade",
    "TurnSettles",
    "WaitsLost",
];

/// The steps no restart lane takes.
const LANELESS: [&str; 4] = [
    "AsksRestart",
    "RestartDeferred",
    "RestartMade",
    "RestartUnmade",
];

/// `m` with every mutant removed but `keep`.
fn alone(m: &Model, keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || MUTANTS.iter().all(|(mutant, ..)| *mutant != a.name));
    one
}

/// The committed configuration and each scenario, with the steps each never takes.
fn configurations() -> [(Model, &'static [&'static str]); 3] {
    let m = line_custody_model();
    [
        (m.clone(), &[]),
        (interp::with_consts(&m, &[("Served", 0)]), &UNSERVED),
        (interp::with_consts(&m, &[("Lane", 0)]), &LANELESS),
    ]
}

#[test]
fn line_custody_fires_exactly_its_moves_in_each_configuration() {
    for (m, never) in configurations() {
        assert!(
            interp::violated_invariants(&m).is_empty(),
            "every law holds: {:?}",
            m.consts
        );
        let fired = interp::fired_actions(&m);
        for action in m.actions.iter().map(|a| a.name) {
            let dead = never.contains(&action) || MUTANTS.iter().any(|(name, ..)| *name == action);
            assert_eq!(
                fired.contains(action),
                !dead,
                "{action} ({:?}): {fired:?}",
                m.consts
            );
        }
    }
}

#[test]
fn each_mutant_breaks_its_own_law_alone() {
    let m = line_custody_model();
    for (mutant, dial, law) in MUTANTS {
        let one = alone(&m, mutant);
        let wrong = interp::with_buggy(&aterm_spec::walk::only(&one, law), dial);
        assert!(
            interp::violated_invariants(&wrong).contains(law),
            "{mutant} (Buggy = {dial}) breaks {law}"
        );
        let right = interp::with_buggy(&aterm_spec::walk::only(&one, law), 0);
        assert!(
            interp::violated_invariants(&right).is_empty(),
            "{law} holds"
        );
    }
    for inv in &m.invariants {
        assert!(
            MUTANTS.iter().any(|(_, _, law)| *law == inv.name),
            "{} has its own mutant",
            inv.name
        );
    }
}

#[test]
fn line_custody_stays_small() {
    for (m, _) in configurations() {
        for dial in std::iter::once(0).chain(MUTANTS.iter().map(|(_, dial, _)| *dial)) {
            let n = interp::reachable_states(&interp::with_buggy(&m, dial)).len();
            assert!(
                n < STATE_BOUND,
                "Buggy = {dial} ({:?}): {n} states",
                m.consts
            );
        }
    }
}

/// A line the worker read and drew none of holds every write until its minute and rest; then
/// it is restarted once, and the line written again to the process after, lost the same way,
/// waits, told the host as lost.
#[test]
fn a_read_line_is_restarted_once_and_a_second_loss_waits() {
    let m = line_custody_model();
    let held = walk(&m, &["WritesRead"]);
    assert_eq!((held["cust"], held["held"], held["q"]), (1, 1, 3));
    for write in ["WritesQueued", "WritesRead", "Types"] {
        assert!(!m.action_enabled(write, &held), "{write} while held");
    }
    assert!(m.action_enabled("GateHolds", &held));
    assert!(
        !m.action_enabled("AsksRestart", &held),
        "not within its minute"
    );
    assert!(
        !m.action_enabled("HostTypes", &held),
        "the host waits on it"
    );
    let ripe = walk(&m, &["WritesRead", "Minute", "Stirs"]);
    assert!(!m.action_enabled("AsksRestart", &ripe), "not at rest");
    let again = walk(
        &m,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartMade",
            "WritesRead",
            "Minute",
        ],
    );
    assert_eq!((again["restart"], again["asks"]), (1, 1));
    assert!(!m.action_enabled("AsksRestart", &again), "restarted once");
    let lost = walk(
        &m,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartMade",
            "WritesRead",
            "Minute",
            "WaitsLost",
        ],
    );
    assert_eq!((lost["lost"], lost["cust"]), (1, 1));
    assert!(
        m.action_enabled("HostTypes", &lost),
        "told the host as lost"
    );
    let deferred = walk(
        &m,
        &["WritesRead", "Minute", "AsksRestart", "RestartDeferred"],
    );
    assert!(
        m.action_enabled("AsksRestart", &deferred),
        "a deferral is asked again"
    );
    let submitted = walk(
        &m,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartMade",
            "Types",
        ],
    );
    assert_eq!((submitted["restart"], submitted["asks"]), (0, 0));
}

/// A line read whose minute passed is held while input queued behind it is unread: no restart
/// asked, a turn outlasted; once that input is read, the restart is asked. The host's upgrade,
/// which ends the agent's process, waits on a line that stands and goes once it is lost.
#[test]
fn input_behind_a_read_line_holds_its_restart_and_the_upgrade_waits_on_a_line_that_stands() {
    let m = line_custody_model();
    let behind = walk(&m, &["WritesRead", "Minute", "Queues"]);
    assert_eq!((behind["held"], behind["ripe"], behind["bq"]), (1, 1, 1));
    assert!(m.action_enabled("GateHolds", &behind), "the gate holds");
    for loss in ["AsksRestart", "WaitsLost"] {
        assert!(
            !m.action_enabled(loss, &behind),
            "{loss} behind unread input"
        );
    }
    let turned = walk(
        &m,
        &["WritesRead", "Minute", "Queues", "ProgramDrops", "TurnRuns"],
    );
    assert!(
        !m.action_enabled("TurnSettles", &turned),
        "unread: no settle"
    );
    assert!(m.action_enabled("TurnOutlasted", &turned));
    let read = walk(&m, &["WritesRead", "Minute", "Queues", "ReadsBehind"]);
    assert!(
        m.action_enabled("AsksRestart", &read),
        "read behind: restarted"
    );
    let queued = walk(&m, &["WritesQueued"]);
    assert!(
        !m.action_enabled("HostUpgrades", &queued),
        "the upgrade waits on a line that stands"
    );
    let lost = walk(
        &m,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartUnmade",
            "WaitsLost",
        ],
    );
    assert!(
        m.action_enabled("HostUpgrades", &lost),
        "a lost line lets it go"
    );
    let ended = walk(
        &m,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartUnmade",
            "WaitsLost",
            "HostUpgrades",
            "SeesEnded",
        ],
    );
    assert_eq!(ended["cust"], 0, "the upgrade's move ends its program");
}

/// A line still queued outlasts every turn; drawn after one, it is handed back to its writer
/// and entered once.
#[test]
fn a_queued_line_outlasts_a_turn_and_is_entered_once_drawn() {
    let m = line_custody_model();
    let turned = walk(&m, &["WritesQueued", "TurnRuns"]);
    assert!(
        !m.action_enabled("TurnSettles", &turned),
        "unread: no settle"
    );
    assert!(m.action_enabled("TurnOutlasted", &turned));
    let drawn = walk(
        &m,
        &[
            "WritesQueued",
            "TurnRuns",
            "TurnOutlasted",
            "ReadsAndDraws",
            "LookDrawn",
        ],
    );
    assert_eq!((drawn["cust"], drawn["st"], drawn["q"]), (0, 1, 1));
    assert!(
        !m.action_enabled("Types", &drawn),
        "its Enter is its writer's"
    );
    let entered = walk(
        &m,
        &[
            "WritesQueued",
            "TurnRuns",
            "TurnOutlasted",
            "ReadsAndDraws",
            "LookDrawn",
            "Enters",
        ],
    );
    assert_eq!((entered["st"], entered["q"]), (2, 0));
    let went = walk(
        &m,
        &["WritesQueued", "TurnRuns", "ReadsAndDrops", "TurnSettles"],
    );
    assert_eq!(went["cust"], 0, "a turn and a clear read settle it");
}

/// A break keeps a draw it saw: the point settles it drawn though someone cleared it since; a
/// person's keys drawn after it, and its program's end, settle it.
#[test]
fn a_break_keeps_a_draw_and_a_person_or_an_end_settles_it() {
    let m = line_custody_model();
    let kept = walk(
        &m,
        &[
            "WritesRead",
            "ProgramDraws",
            "BreakKeepsDraw",
            "PersonClears",
        ],
    );
    assert_eq!((kept["drawn"], kept["q"]), (1, 0));
    let settled = walk(
        &m,
        &[
            "WritesRead",
            "ProgramDraws",
            "BreakKeepsDraw",
            "PersonClears",
            "LookDrawn",
        ],
    );
    assert_eq!((settled["cust"], settled["st"]), (0, 0));
    let person = walk(
        &m,
        &["WritesRead", "ProgramDrops", "PersonDraws", "LookPerson"],
    );
    assert_eq!(person["cust"], 0);
    let ended = walk(&m, &["WritesQueued", "ProgramEnds", "SeesEnded"]);
    assert_eq!(ended["cust"], 0);
    assert!(
        !m.action_enabled("LookDrawn", &walk(&m, &["WritesQueued", "ProgramReads"])),
        "a read that shows none of it settles nothing"
    );
}

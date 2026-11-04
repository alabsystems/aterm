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
const MUTANTS: [(&str, i64, &str); 15] = [
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
    (
        "TurnSettlesUnderDialog",
        11,
        "ADrawnLineGoesBackToItsWriter",
    ),
    ("AnswersComposer", 12, "ADrawnLineGoesBackToItsWriter"),
    ("AnswersWithoutRead", 13, "ABoxAnswerFollowsARead"),
    ("RestartTakenOnWord", 14, "NoTwoCopies"),
    ("SettleKeepsAsked", 15, "OneRestartPerLine"),
];

/// A bound on the states the model reaches at any dial (the box over the composer and its
/// custody-permission marker each distinguish states under it, a restart standing asked adds half
/// again): the most, at `Buggy = 1`, were 60256 when it was set.
const STATE_BOUND: usize = 80_000;

/// The steps a host that serves no input word never takes: no read says the line was read.
const UNSERVED: [&str; 10] = [
    "AnswersBox",
    "AsksRestart",
    "Minute",
    "RestartDeferred",
    "RestartMade",
    "RestartMadeDeferred",
    "RestartStands",
    "RestartUnmade",
    "TurnSettles",
    "WaitsLost",
];

/// The steps no restart lane takes.
const LANELESS: [&str; 6] = [
    "AsksRestart",
    "RestartDeferred",
    "RestartMade",
    "RestartMadeDeferred",
    "RestartStands",
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

/// The host's word that it made the restart, with the process the line was written to named after
/// it, shows nothing ended: the line stays held, every write held behind it, its copy may still be
/// drawn, and the restart stands asked, asked again; made once another process is named, the line
/// ended with its program. Settled on the word alone (`RestartTakenOnWord`), a write goes while
/// the read line may still reach the program.
#[test]
fn a_restart_word_with_the_lines_process_standing_holds_the_line() {
    let m = line_custody_model();
    let stands = walk(
        &m,
        &["WritesRead", "Minute", "AsksRestart", "RestartStands"],
    );
    assert_eq!(
        (
            stands["cust"],
            stands["q"],
            stands["restart"],
            stands["asks"]
        ),
        (1, 2, 3, 0)
    );
    for write in ["WritesQueued", "WritesRead", "Types"] {
        assert!(!m.action_enabled(write, &stands), "{write} while it stands");
    }
    assert!(
        m.action_enabled("AsksRestart", &stands),
        "asked again on its back-off"
    );
    assert!(
        m.action_enabled("ProgramDraws", &stands),
        "its copy may still come"
    );
    let made = walk(
        &m,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartStands",
            "AsksRestart",
            "RestartMade",
        ],
    );
    assert_eq!(
        (made["cust"], made["q"], made["restart"], made["asks"]),
        (0, 0, 1, 1)
    );
    let taken = interp::with_buggy(&alone(&m, "RestartTakenOnWord"), 14);
    let twice = walk(
        &taken,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartTakenOnWord",
            "WritesRead",
        ],
    );
    assert_eq!(twice["twice"], 1, "a second copy behind one that may come");
}

/// A RESTART THE HOST MADE AND ANSWERED AS A DEFERRAL SPENDS THE LINE'S RESTART ONCE THE LINE
/// SETTLES: its signal went and the process it started is not yet registered, so its word is a
/// deferral; the program ended with the line, which settles once another process is named, and the
/// line written to that process, lost the same way, waits rather than restarting it again. A
/// deferral that made nothing is spent the same way by whatever settles its line (here its draw,
/// entered by its writer, after which an act submitted makes a restart askable again). Left
/// unspent by the settle (`SettleKeepsAsked`), the second loss is restarted again: two restarts
/// for one line short of an act submitted.
#[test]
fn a_restart_made_under_a_deferrals_word_is_spent_once_its_line_settles() {
    let m = line_custody_model();
    let made = walk(
        &m,
        &["WritesRead", "Minute", "AsksRestart", "RestartMadeDeferred"],
    );
    assert_eq!(
        (
            made["cust"],
            made["moved"],
            made["q"],
            made["restart"],
            made["asks"]
        ),
        (1, 1, 0, 3, 1)
    );
    assert!(
        !m.action_enabled("AsksRestart", &made),
        "its program ended: the line settles before any ask"
    );
    let again = walk(
        &m,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartMadeDeferred",
            "SeesEnded",
            "WritesRead",
            "Minute",
        ],
    );
    assert_eq!((again["restart"], again["asks"]), (1, 1));
    assert!(!m.action_enabled("AsksRestart", &again), "spent");
    assert!(
        m.action_enabled("WaitsLost", &again),
        "the second loss waits"
    );
    let drawn = walk(
        &m,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartDeferred",
            "ProgramDraws",
            "LookDrawn",
        ],
    );
    assert_eq!((drawn["cust"], drawn["restart"], drawn["st"]), (0, 1, 1));
    let entered = walk(
        &m,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartDeferred",
            "ProgramDraws",
            "LookDrawn",
            "Enters",
        ],
    );
    assert_eq!((entered["restart"], entered["asks"]), (0, 0));
    let kept = interp::with_buggy(&alone(&m, "SettleKeepsAsked"), 15);
    let twice = walk(
        &kept,
        &[
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartMadeDeferred",
            "SettleKeepsAsked",
            "WritesRead",
            "Minute",
            "AsksRestart",
            "RestartMade",
        ],
    );
    assert_eq!(twice["asks"], 2, "a second restart for the line");
    assert!(interp::violated_invariants(&kept).contains("OneRestartPerLine"));
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

/// A box over the composer is a read that cannot say what it holds: under it a turn after the
/// line neither settles nor outlasts it, the look settles nothing it has not kept, and the gate
/// holds every line's write, no restart asked, but lets the loop's answer to the box go only once
/// an earlier or fresh clear input read proves the worker read the line. Custody permission alone
/// does not claim the session's later input gate sent the key or closed the box. Once the box goes
/// (after that key, or on its own), a line drawn under it is handed back to its writer and entered
/// once, and a line dropped under it goes with the turn kept.
#[test]
fn a_box_over_the_composer_keeps_a_turn_for_the_read_after_it() {
    let m = line_custody_model();
    let queued = walk(&m, &["WritesQueued", "DialogShows"]);
    assert_eq!((queued["held"], queued["q"]), (0, 4));
    assert!(
        !m.action_enabled("AnswersBox", &queued),
        "a line still queued holds the box's answer"
    );
    let freshly_read = walk(&m, &["WritesQueued", "ProgramReads", "DialogShows"]);
    assert_eq!((freshly_read["held"], freshly_read["q"]), (0, 3));
    assert!(
        m.action_enabled("AnswersBox", &freshly_read),
        "this gate's fresh clear read lets the box's answer go"
    );
    let answered = walk(
        &m,
        &["WritesQueued", "ProgramReads", "DialogShows", "AnswersBox"],
    );
    assert_eq!(
        (answered["held"], answered["dlg"], answered["answered"]),
        (1, 1, 1),
        "the fresh proof is kept and custody permits the answer"
    );
    let read_before = walk(&m, &["WritesRead", "Queues", "DialogShows"]);
    assert_eq!((read_before["held"], read_before["bq"]), (1, 1));
    assert!(
        m.action_enabled("AnswersBox", &read_before),
        "an earlier clear read remains proof; the ordinary input gate holds what queued behind"
    );
    let held_behind = walk(&m, &["WritesRead", "Queues", "DialogShows", "AnswersBox"]);
    assert_eq!(
        (
            held_behind["answered"],
            held_behind["dlg"],
            held_behind["bq"],
        ),
        (1, 1, 1),
        "custody permits it, but does not claim the later input gate sent or closed the box"
    );
    let unserved = interp::with_consts(&m, &[("Served", 0)]);
    let unknown = walk(&unserved, &["WritesRead", "DialogShows"]);
    assert_eq!(unknown["held"], 2);
    assert!(
        !unserved.action_enabled("AnswersBox", &unknown),
        "no input word supplies no proof for the box's answer"
    );
    let drawn = walk(
        &m,
        &["WritesRead", "ProgramDraws", "TurnRuns", "DialogShows"],
    );
    assert_eq!((drawn["q"], drawn["ran"], drawn["dlg"]), (1, 1, 1));
    for step in [
        "TurnSettles",
        "TurnOutlasted",
        "LookDrawn",
        "BreakKeepsDraw",
        "AsksRestart",
        "WaitsLost",
    ] {
        assert!(!m.action_enabled(step, &drawn), "{step} under the box");
    }
    assert!(m.action_enabled("GateHolds", &drawn), "the gate holds");
    assert!(
        m.action_enabled("AnswersBox", &drawn),
        "the box's answer goes"
    );
    let ripe = walk(
        &m,
        &[
            "WritesRead",
            "ProgramDrops",
            "TurnRuns",
            "DialogShows",
            "Minute",
        ],
    );
    assert!(
        !m.action_enabled("AsksRestart", &ripe),
        "no restart under the box"
    );
    let entered = walk(
        &m,
        &[
            "WritesRead",
            "ProgramDraws",
            "TurnRuns",
            "DialogShows",
            "AnswersBox",
            "DialogGoes",
            "LookDrawn",
            "Enters",
        ],
    );
    assert_eq!(
        (
            entered["cust"],
            entered["st"],
            entered["q"],
            entered["stray"]
        ),
        (0, 2, 0, 0),
        "handed back and entered once"
    );
    let dropped = walk(
        &m,
        &[
            "WritesRead",
            "ProgramDrops",
            "TurnRuns",
            "DialogShows",
            "DialogGoes",
            "TurnSettles",
        ],
    );
    assert_eq!(dropped["cust"], 0, "the kept turn settles it");
    let kept = walk(
        &m,
        &[
            "WritesRead",
            "ProgramDraws",
            "BreakKeepsDraw",
            "DialogShows",
            "LookDrawn",
        ],
    );
    assert_eq!(
        (kept["cust"], kept["st"]),
        (0, 1),
        "a kept draw is handed back under the box"
    );
}

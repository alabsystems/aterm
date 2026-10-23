// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 conformance tests for the PTY keeper's four state machines.

use aterm_models_core::{
    keeper_companion_custody_model, keeper_death_judgement_model, keeper_relaunch_brake_model,
    pty_keeper_custody_model,
};
use aterm_spec::walk::{only, walk};
use aterm_spec::{interp, verify};

#[test]
fn pty_keeper_custody_fires_every_committed_move() {
    let model = pty_keeper_custody_model();
    let committed: Vec<&str> = interp::fired_actions(&model).into_iter().collect();
    for action in [
        "RegisterA",
        "CloseTabA",
        "Quit",
        "KeeperHonoursBye",
        "WindowCrashes",
        "StartSuccessor",
        "Commit",
        "OutgoingReleasesA",
        "SuccessorClosesTabA",
        "OutgoingDrained",
        "Rollback",
        "SuccessorFailStops",
        "SuccessorCrashes",
        "SuccessorCrashesFirst",
        "Relaunch",
        "OfferA",
        "OfferB",
        "AdoptA",
        "RelaunchedLinkDrops",
        "RelaunchedWindowQuits",
        "RelaunchedWindowCrashes",
        "KeeperCrashes",
        "KeeperRestarts",
    ] {
        assert!(
            committed.contains(&action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
    assert!(
        committed.contains(&"LinkDrops"),
        "a live window's link dropping is an event the committed keeper survives"
    );
}

/// The lifecycles a person meets.
#[test]
fn pty_keeper_custody_walks_the_lifecycles() {
    let m = pty_keeper_custody_model();

    let s = walk(
        &m,
        &[
            "RegisterA",
            "RegisterB",
            "WindowCrashes",
            "Relaunch",
            "OfferA",
            "OfferB",
            "AdoptA",
            "AdoptB",
        ],
    );
    assert_eq!(
        (
            s["r_reads_a"],
            s["r_reads_b"],
            s["k_holds_a"],
            s["k_holds_b"]
        ),
        (1, 1, 1, 1),
        "{s:?}"
    );
    assert_eq!((s["lost_a"], s["lost_b"]), (0, 0));

    let s2 = {
        let mut s2 = s.clone();
        assert!(m.fire("RelaunchedWindowCrashes", &mut s2));
        s2
    };
    assert_eq!((s2["orphan_a"], s2["lost_a"]), (1, 0), "{s2:?}");

    let early = walk(&m, &["RegisterA", "WindowCrashes"]);
    assert_eq!((early["lost_a"], early["lost_b"]), (0, 1), "{early:?}");
    assert_eq!(early["ever_registered_b"], 0);

    let quit = walk(&m, &["RegisterA", "RegisterB", "Quit", "KeeperHonoursBye"]);
    assert_eq!((quit["k_holds_a"], quit["k_holds_b"]), (0, 0), "{quit:?}");
    assert!(
        !m.action_enabled("Relaunch", &quit),
        "no relaunch after a quit"
    );

    let closed = walk(
        &m,
        &[
            "RegisterA",
            "RegisterB",
            "CloseTabA",
            "WindowCrashes",
            "Relaunch",
        ],
    );
    assert!(!m.action_enabled("OfferA", &closed), "{closed:?}");
    assert!(m.action_enabled("OfferB", &closed), "{closed:?}");

    let update = walk(&m, &["RegisterA", "RegisterB", "StartSuccessor", "Commit"]);
    assert_eq!(
        (update["s_reads_a"], update["orphan_a"]),
        (1, 0),
        "{update:?}"
    );
    assert!(!m.action_enabled("Relaunch", &update));

    let mid = walk(
        &m,
        &["RegisterA", "RegisterB", "StartSuccessor", "WindowCrashes"],
    );
    assert_eq!(
        mid["orphan_a"], 0,
        "a hold while the successor lives: {mid:?}"
    );
    let mut mid2 = mid.clone();
    assert!(m.fire("SuccessorFailStops", &mut mid2));
    assert_eq!((mid2["orphan_a"], mid2["lost_a"]), (1, 0), "{mid2:?}");
    assert!(m.action_enabled("Relaunch", &mid2));
}

/// An offer is made to a connection. The relaunched window's link drops before it registered
/// its offers: they are orphans again while it still holds them (no second offer), its
/// reconnected link registers them, and its later quit (BYE on the new link) relaunches
/// nothing. At `Buggy = 1` the offer stays with the dead link, and the quit reads as a crash.
#[test]
fn pty_keeper_custody_an_offer_returns_when_its_link_drops() {
    let m = pty_keeper_custody_model();
    let offered = [
        "RegisterA",
        "WindowCrashes",
        "Relaunch",
        "OfferA",
        "RelaunchedLinkDrops",
    ];
    let s = walk(&m, &offered);
    assert_eq!(
        (
            s["orphan_a"],
            s["k_holds_a"],
            s["r_holds_a"],
            s["r_relinked"]
        ),
        (1, 1, 1, 1),
        "{s:?}"
    );
    assert!(
        !m.action_enabled("OfferA", &s),
        "the window still holds it: no second offer"
    );
    let mut adopted = s.clone();
    assert!(m.fire("AdoptA", &mut adopted));
    assert_eq!((adopted["orphan_a"], adopted["r_reads_a"]), (0, 1));
    for from in [s, adopted] {
        let mut quit = from.clone();
        assert!(m.fire("RelaunchedWindowQuits", &mut quit));
        assert_eq!(quit["bye"], 1, "{quit:?}");
        assert!(!m.action_enabled("Relaunch", &quit), "{quit:?}");
    }
    let buggy = interp::with_buggy(&m, 1);
    let mut stuck = walk(&m, &offered[..4]);
    assert!(buggy.fire("RelaunchedLinkDrops", &mut stuck));
    assert_eq!(
        (stuck["orphan_a"], stuck["r_relinked"]),
        (0, 0),
        "the mutant leaves the offer with the dead link: {stuck:?}"
    );
    let mut quit = walk(&m, &offered);
    assert!(buggy.fire("RelaunchedWindowQuits", &mut quit));
    assert_eq!(quit["bye"], 0, "the mutant reads the quit as a crash");
    assert!(buggy.action_enabled("Relaunch", &quit), "{quit:?}");
}

/// A release from one of two claimants (the outgoing window of an update, read after its
/// successor registered) must not close the custody copy the successor's shell depends on. The
/// same schedule loses the shell with no keeper fault at `Buggy = 1` and keeps it at
/// `Buggy = 0`, and the step that differs is the release.
#[test]
fn pty_keeper_custody_a_release_from_one_of_two_claimants_keeps_the_copy() {
    let base = pty_keeper_custody_model();
    let schedule = [
        "RegisterA",
        "StartSuccessor",
        "Commit",
        "OutgoingReleasesA",
        "OutgoingDrained",
        "SuccessorCrashes",
    ];
    let fixed = walk(&base, &schedule);
    assert_eq!(
        (fixed["k_holds_a"], fixed["orphan_a"], fixed["lost_a"]),
        (1, 1, 0),
        "the successor's crash leaves an orphan, not a loss: {fixed:?}"
    );
    let buggy = interp::with_buggy(&base, 1);
    let lost = walk(&buggy, &schedule);
    assert_eq!(
        (lost["lost_a"], lost["k_faults"], lost["ever_registered_a"]),
        (1, 0, 1),
        "the mutant loses the shell with no keeper fault: {lost:?}"
    );
    assert!(!buggy.check_invariant("LossNeedsKeeperFaultA", &lost));
    let upto = walk(&base, &schedule[..3]);
    let upto_buggy = walk(&buggy, &schedule[..3]);
    assert_eq!((upto["k_holds_a"], upto_buggy["k_holds_a"]), (1, 1));
    let mut kept = upto;
    let mut closed = upto_buggy;
    assert!(base.fire("OutgoingReleasesA", &mut kept));
    assert!(buggy.fire("OutgoingReleasesA", &mut closed));
    assert_eq!((kept["k_holds_a"], closed["k_holds_a"]), (1, 0));

    let s = walk(
        &base,
        &[
            "RegisterA",
            "StartSuccessor",
            "Commit",
            "SuccessorClosesTabA",
        ],
    );
    assert_eq!((s["closed_a"], s["k_holds_a"]), (1, 1), "{s:?}");
    let mut drained = s.clone();
    assert!(base.fire("OutgoingDrained", &mut drained));
    assert_eq!((drained["k_holds_a"], drained["orphan_a"]), (0, 0));
}

/// `Buggy = 1` is caught on every design invariant by its own schedule.
#[test]
fn pty_keeper_custody_the_defects_are_caught_on_every_invariant() {
    let base = pty_keeper_custody_model();
    let m = interp::with_buggy(&base, 1);
    for (inv, show) in [
        ("NeverTwoReadersA", ["w_reads_a", "s_reads_a", "r_reads_a"]),
        ("NeverTwoReadersB", ["w_reads_b", "s_reads_b", "r_reads_b"]),
        (
            "OfferOnlyWithNoLiveHolderA",
            ["r_holds_a", "w_live", "s_live"],
        ),
        (
            "OfferOnlyWithNoLiveHolderB",
            ["r_holds_b", "w_live", "s_live"],
        ),
        (
            "ClosedNeverReachesAWindowA",
            ["closed_a", "s_holds_a", "r_holds_a"],
        ),
        (
            "ClosedNeverReachesAWindowB",
            ["closed_b", "s_holds_b", "r_holds_b"],
        ),
        ("NoRelaunchAfterQuit", ["bye", "r_live", "relaunches"]),
        (
            "LossNeedsKeeperFaultA",
            ["lost_a", "k_faults", "ever_registered_a"],
        ),
        (
            "LossNeedsKeeperFaultB",
            ["lost_b", "k_faults", "ever_registered_b"],
        ),
    ] {
        let (bad, _) = interp::bmc(&only(&m, inv)).expect_err(inv);
        let shown: Vec<i64> = show.iter().map(|v| bad[*v]).collect();
        assert!(!m.check_invariant(inv, &bad), "{inv}: {show:?} = {shown:?}");
    }
    assert_eq!(
        verify::uncaught_invariants(&base),
        vec!["RelaunchSpace"],
        "every design invariant is caught; the relaunch count is the one space guard"
    );
}

#[test]
fn keeper_relaunch_brake_fires_every_committed_move() {
    let model = keeper_relaunch_brake_model();
    let committed: Vec<&str> = interp::fired_actions(&model).into_iter().collect();
    for action in [
        "PickRepeatedDeaths",
        "PickBootTrial",
        "Death",
        "CleanQuit",
        "Decide",
        "RelaunchFails",
        "Healthy",
        "ManualLaunch",
    ] {
        assert!(committed.contains(&action), "{action}: {committed:?}");
    }
}

/// The brake as a person meets it: three relaunches, then a hold that only a
/// manual launch lifts; a healthy relaunch resets; a quit relaunches nothing.
#[test]
fn keeper_relaunch_brake_walks_the_lifecycles() {
    let m = keeper_relaunch_brake_model();
    let spent = walk(
        &m,
        &[
            "PickBootTrial",
            "Death",
            "Decide",
            "RelaunchFails",
            "Decide",
            "RelaunchFails",
            "Decide",
            "RelaunchFails",
            "Decide",
        ],
    );
    assert_eq!(
        (
            spent["grants"],
            spent["streak"],
            spent["held"],
            spent["phase"]
        ),
        (3, 3, 1, 3),
        "three relaunches for the boot trial, then a hold: {spent:?}"
    );
    let mut lifted = spent.clone();
    assert!(m.fire("ManualLaunch", &mut lifted));
    assert_eq!((lifted["streak"], lifted["held"]), (0, 0));

    let healthy = walk(&m, &["PickRepeatedDeaths", "Death", "Decide", "Healthy"]);
    assert_eq!(healthy["streak"], 0, "{healthy:?}");

    let quit = walk(&m, &["PickRepeatedDeaths", "CleanQuit", "Decide"]);
    assert_eq!((quit["grants"], quit["phase"]), (0, 0), "{quit:?}");
}

#[test]
fn keeper_relaunch_brake_the_defects_are_caught_on_every_invariant() {
    let base = keeper_relaunch_brake_model();
    let m = interp::with_buggy(&base, 1);
    let (storm, _) = interp::bmc(&only(&m, "StreakBounded")).expect_err("the sentinel writer");
    assert_eq!(
        (storm["kind"], storm["streak"]),
        (1, 4),
        "a spent brake read as elapsed relaunches again: {storm:?}"
    );
    let (early, _) = interp::bmc(&only(&m, "HoldOnlyWhenSpent")).expect_err("the double count");
    assert_eq!(
        (early["kind"], early["held"], early["grants"]),
        (2, 1, 2),
        "a boot trial held after two relaunches: {early:?}"
    );
    let (quit, _) = interp::bmc(&only(&m, "CleanQuitNeverRelaunches")).expect_err("quit as death");
    assert_eq!(quit["after_quit"], 1, "{quit:?}");
}

#[test]
fn keeper_death_judgement_fires_every_committed_move() {
    let model = keeper_death_judgement_model();
    let committed: Vec<&str> = interp::fired_actions(&model).into_iter().collect();
    for action in [
        "PickP3Window",
        "PickPreByeWindow",
        "QuitWithBye",
        "QuitByeLost",
        "Killed",
        "PanicExit",
        "StatusLost",
        "MarkerSwept",
        "MarkerInherited",
        "Judge",
        "ShellDies",
        "KeeperSeesShellExit",
    ] {
        assert!(committed.contains(&action), "{action}: {committed:?}");
    }
}

/// The ends a person meets, judged by name.
#[test]
fn keeper_death_judgement_walks_the_ends() {
    let m = keeper_death_judgement_model();
    let lost = walk(&m, &["QuitByeLost", "Judge"]);
    assert_eq!((lost["judged_quit"], lost["orphaned"]), (1, 0), "{lost:?}");
    for tail in [
        &["Judge"][..],
        &["MarkerSwept", "Judge"],
        &["MarkerInherited", "Judge"],
    ] {
        let mut schedule = vec!["Killed"];
        schedule.extend_from_slice(tail);
        let s = walk(&m, &schedule);
        assert_eq!(
            (s["judged_quit"], s["orphaned"]),
            (0, 1),
            "{schedule:?}: {s:?}"
        );
    }
    let panic = walk(&m, &["PanicExit", "Judge"]);
    assert_eq!(panic["orphaned"], 1, "{panic:?}");
    let p3 = walk(&m, &["PickP3Window", "QuitByeLost", "Judge"]);
    assert_eq!(p3["orphaned"], 1, "{p3:?}");
    let old = walk(&m, &["PickPreByeWindow", "QuitWithBye", "Judge"]);
    assert_eq!((old["bye"], old["judged_quit"]), (0, 1), "{old:?}");
    let pruned = walk(&m, &["Killed", "Judge", "ShellDies", "KeeperSeesShellExit"]);
    assert_eq!(
        (pruned["orphaned"], pruned["watch_pending"]),
        (0, 0),
        "{pruned:?}"
    );
    let early = walk(&m, &["Killed", "ShellDies", "Judge"]);
    assert_eq!(
        (early["orphaned"], early["watch_pending"]),
        (0, 0),
        "{early:?}"
    );
}

/// `Buggy = 1` is caught on every invariant, each by the schedule its mutant
/// was written for.
#[test]
fn keeper_death_judgement_the_defects_are_caught_on_every_invariant() {
    let base = keeper_death_judgement_model();
    let m = interp::with_buggy(&base, 1);
    let (resurrected, _) =
        interp::bmc(&only(&m, "CleanQuitNeverResurrected")).expect_err("the marker ignored");
    assert_eq!(
        (
            resurrected["quit"],
            resurrected["bye"],
            resurrected["ever_orphaned"]
        ),
        (1, 0, 1),
        "{resurrected:?}"
    );
    let replay = walk(&m, &["QuitByeLost", "Judge"]);
    assert!(!m.check_invariant("CleanQuitNeverResurrected", &replay));
    let (closed, _) = interp::bmc(&only(&m, "CrashNeverClosed")).expect_err("the marker alone");
    assert_eq!(
        (closed["quit"], closed["judged_quit"]),
        (0, 1),
        "{closed:?}"
    );
    let replay = walk(&m, &["PanicExit", "Judge"]);
    assert!(!m.check_invariant("CrashNeverClosed", &replay));
    let (stale, _) = interp::bmc(&only(&m, "NeverListsTheDead")).expect_err("no prune");
    assert_eq!(
        (
            stale["orphaned"],
            stale["shell_live"],
            stale["watch_pending"]
        ),
        (1, 0, 0),
        "{stale:?}"
    );
}

/// Every action of `KeeperCompanionCustody`, so a guard tightened by mistake shows up as a dead
/// action, not a quieter proof.
const COMPANION_ACTIONS: [&str; 10] = [
    "Release",
    "ReaderDies",
    "OfferNew",
    "OfferOld",
    "RecipientDrops",
    "Claim",
    "ReaderCompanion",
    "ReaderReleases",
    "SuccessorCommits",
    "OutgoingEnds",
];

#[test]
fn keeper_companion_custody_fires_every_committed_move() {
    let model = keeper_companion_custody_model();
    let committed = interp::fired_actions(&model);
    for action in COMPANION_ACTIONS {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
}

/// Each defect breaks the invariant written for it, alone (the plan's mutant table), and holds
/// with no defect on.
#[test]
fn keeper_companion_custody_each_defect_breaks_its_own_invariant() {
    let model = keeper_companion_custody_model();
    for (buggy, invariant) in [
        (1, "ExactOnlyFromTheReader"),
        (2, "NeverToAnOldWindow"),
        (3, "ReleasedCompanionClosed"),
        (4, "PoisonBounded"),
        (5, "ExactOnlyFromTheReader"),
    ] {
        let mutant = only(&interp::with_buggy(&model, buggy), invariant);
        assert!(
            interp::bmc(&mutant).is_err(),
            "Buggy = {buggy} must break {invariant}"
        );
        assert!(
            interp::bmc(&only(&model, invariant)).is_ok(),
            "{invariant} holds with no defect on"
        );
    }
}

/// The plan's hand-traced counterexamples, walked on each mutant: each ends where its invariant
/// is broken, and the committed machine keeps the invariant along the same steps.
#[test]
fn keeper_companion_custody_the_traced_counterexamples_end_where_the_plan_says() {
    let model = keeper_companion_custody_model();
    let traces: [(i64, &str, &[&str]); 5] = [
        (
            1,
            "ExactOnlyFromTheReader",
            &["ReaderDies", "OfferNew", "Claim", "ReaderDies", "OfferNew"],
        ),
        (2, "NeverToAnOldWindow", &["ReaderDies", "OfferOld"]),
        (3, "ReleasedCompanionClosed", &["Release"]),
        (
            4,
            "PoisonBounded",
            &[
                "ReaderDies",
                "OfferNew",
                "RecipientDrops",
                "OfferNew",
                "RecipientDrops",
                "OfferNew",
            ],
        ),
        (
            5,
            "ExactOnlyFromTheReader",
            &["SuccessorCommits", "ReaderDies", "OutgoingEnds", "OfferNew"],
        ),
    ];
    for (buggy, invariant, steps) in traces {
        let mutant = interp::with_buggy(&model, buggy);
        let end = walk(&mutant, steps);
        assert!(
            !mutant.check_invariant(invariant, &end),
            "Buggy = {buggy}: {steps:?} ends where {invariant} is broken: {end:?}"
        );
        let committed = walk(&model, steps);
        assert!(
            model.check_invariant(invariant, &committed),
            "the committed machine keeps {invariant} along {steps:?}: {committed:?}"
        );
    }
}

/// The lifecycles a person meets.
#[test]
fn keeper_companion_custody_walks_the_lifecycles() {
    let m = keeper_companion_custody_model();
    let s = walk(
        &m,
        &[
            "ReaderDies",
            "OfferNew",
            "Claim",
            "ReaderCompanion",
            "ReaderDies",
        ],
    );
    assert_eq!(
        (s["comp"], s["dead_reader"], s["attempts"], s["state"]),
        (2, 2, 0, 1),
        "{s:?}"
    );
    let once = walk(&m, &["ReaderDies", "OfferNew", "Claim", "ReaderDies"]);
    assert_eq!((once["comp"], once["attempts"]), (1, 1), "{once:?}");
    let twice = walk(
        &m,
        &[
            "ReaderDies",
            "OfferNew",
            "Claim",
            "ReaderDies",
            "OfferNew",
            "RecipientDrops",
        ],
    );
    assert_eq!((twice["comp"], twice["attempts"]), (0, 2), "{twice:?}");
    let old = walk(&m, &["ReaderDies", "OfferOld", "RecipientDrops"]);
    assert_eq!((old["comp"], old["attempts"]), (1, 0), "{old:?}");
    let claimed = walk(&m, &["ReaderDies", "OfferOld", "Claim"]);
    assert_eq!(claimed["comp"], 0, "{claimed:?}");
    assert!(
        !m.action_enabled("ReaderCompanion", &claimed),
        "a window that does not journal sends no region"
    );
    let released = walk(
        &m,
        &[
            "ReaderDies",
            "OfferNew",
            "Claim",
            "ReaderReleases",
            "ReaderDies",
        ],
    );
    assert_eq!(
        (released["comp"], released["attempts"]),
        (0, 0),
        "{released:?}"
    );
    let update = walk(
        &m,
        &[
            "SuccessorCommits",
            "ReaderCompanion",
            "OutgoingEnds",
            "ReaderDies",
        ],
    );
    assert_eq!(
        (
            update["comp"],
            update["dead_reader"],
            update["attempts"],
            update["state"]
        ),
        (2, 2, 0, 1),
        "{update:?}"
    );
    let settled = walk(
        &m,
        &[
            "SuccessorCommits",
            "ReaderCompanion",
            "ReaderDies",
            "OutgoingEnds",
        ],
    );
    assert_eq!(
        (settled["comp"], settled["attempts"], settled["state"]),
        (2, 0, 1),
        "{settled:?}"
    );
    let unsettled = walk(&m, &["SuccessorCommits", "ReaderDies", "OutgoingEnds"]);
    assert_eq!(
        (unsettled["comp"], unsettled["attempts"], unsettled["state"]),
        (1, 1, 1),
        "{unsettled:?}"
    );
    let between = walk(&m, &["SuccessorCommits", "ReaderDies"]);
    assert_eq!(
        (between["state"], between["live"], between["outgoing"]),
        (0, 0, 1),
        "the outgoing window still claims, and reads nothing: {between:?}"
    );
    assert!(
        !m.action_enabled("ReaderCompanion", &between),
        "the outgoing window is no reader that could settle"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of the PTY keeper's three machines (P2 and P3 of
//! `docs/DESIGN-pty-keeper-2026-09-26.md`, §5.4, §6.1 and §6.2):
//! `PtyKeeperCustody` (`aterm_spec::derive::pty_keeper_custody_model`),
//! `KeeperRelaunchBrake` (`keeper_relaunch_brake_model`) and
//! `KeeperDeathJudgement` (`keeper_death_judgement_model`). Each is proved at
//! `Buggy = 0` and caught at `Buggy = 1` on every design invariant, and the
//! lifecycles a person meets are walked by name. Tier-1 is
//! `crates/aterm-keeper/src/conformance_custody.rs`.

use aterm_spec::derive::{
    Model, keeper_death_judgement_model, keeper_relaunch_brake_model, pty_keeper_custody_model,
};
use aterm_spec::{interp, verify};

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

fn walk(m: &Model, actions: &[&str]) -> interp::State {
    let mut state = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

fn registered(name: &str) -> bool {
    aterm_spec::xref::model_registry()
        .iter()
        .any(|m| m.name == name)
}

#[test]
fn pty_keeper_custody_proves_and_catches() {
    let model = pty_keeper_custody_model();
    assert!(registered(model.name), "enrolled in the spec-link registry");
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
        "Relaunch",
        "OfferA",
        "OfferB",
        "AdoptA",
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
    verify::prove_and_catch_scalar(
        &model,
        "pty keeper custody: one reader, offers only with no live holder, closed tabs \
         never handed on, no relaunch after quit, loss only with a keeper fault",
    );
}

/// The lifecycles a person meets.
#[test]
fn pty_keeper_custody_walks_the_lifecycles() {
    let m = pty_keeper_custody_model();

    // A crash with both masters registered: both kept, both offered to the
    // relaunched window, the keeper still holding its copies.
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

    // The relaunched window crashes too: the keeper's copy keeps the shells,
    // and they are orphans again.
    let s2 = {
        let mut s2 = s.clone();
        assert!(m.fire("RelaunchedWindowCrashes", &mut s2));
        s2
    };
    assert_eq!((s2["orphan_a"], s2["lost_a"]), (1, 0), "{s2:?}");

    // A crash inside the spawn-to-REGISTER window loses that shell: the
    // accepted loss window, which the invariant names.
    let early = walk(&m, &["RegisterA", "WindowCrashes"]);
    assert_eq!((early["lost_a"], early["lost_b"]), (0, 1), "{early:?}");
    assert_eq!(early["ever_registered_b"], 0);

    // A quit: the keeper closes its copies and never relaunches.
    let quit = walk(&m, &["RegisterA", "RegisterB", "Quit", "KeeperHonoursBye"]);
    assert_eq!((quit["k_holds_a"], quit["k_holds_b"]), (0, 0), "{quit:?}");
    assert!(
        !m.action_enabled("Relaunch", &quit),
        "no relaunch after a quit"
    );

    // A closed tab is released and never offered.
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

    // A committed update is a hold: nothing to relaunch while the successor
    // reads, and nothing offered.
    let update = walk(&m, &["RegisterA", "RegisterB", "StartSuccessor", "Commit"]);
    assert_eq!(
        (update["s_reads_a"], update["orphan_a"]),
        (1, 0),
        "{update:?}"
    );
    assert!(!m.action_enabled("Relaunch", &update));

    // The window crashes mid-update: the successor fail-stops, and only then
    // are the masters orphans (F11 closed).
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

/// P2's open finding, the P3 fix: a RELEASE from ONE of two claimants — the
/// outgoing window of an update, read after its successor registered — must
/// not close the custody copy the successor's shell depends on. The same
/// schedule loses the shell with no keeper fault at `Buggy = 1` and keeps it
/// at `Buggy = 0`, and the step that differs is the release.
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
    // The step that loses the copy is the release: before it both runs hold
    // it, after it only the fixed one does.
    let upto = walk(&base, &schedule[..3]);
    let upto_buggy = walk(&buggy, &schedule[..3]);
    assert_eq!((upto["k_holds_a"], upto_buggy["k_holds_a"]), (1, 1));
    let mut kept = upto;
    let mut closed = upto_buggy;
    assert!(base.fire("OutgoingReleasesA", &mut kept));
    assert!(buggy.fire("OutgoingReleasesA", &mut closed));
    assert_eq!((kept["k_holds_a"], closed["k_holds_a"]), (1, 0));

    // The other direction: the successor closes the tab while the outgoing
    // window still claims it. The copy stays for that claimant's end, and its
    // end closes it (the shell was hung up) — never an orphan.
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
fn keeper_relaunch_brake_proves_and_catches() {
    let model = keeper_relaunch_brake_model();
    assert!(registered(model.name), "enrolled in the spec-link registry");
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
    verify::prove_and_catch_scalar(
        &model,
        "keeper relaunch brake: never after a quit, never a storm, a hold only when spent",
    );
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
    assert!(
        verify::uncaught_invariants(&base).is_empty(),
        "StreakBounded is a design claim with a mutant, never a SPACE_GUARDS row"
    );
}

/// The bounded spaces the interpreter walks, stated so a model change that
/// explodes them is seen here first.
#[test]
fn the_keeper_spaces_fit_the_interpreter() {
    for m in [
        pty_keeper_custody_model(),
        keeper_relaunch_brake_model(),
        keeper_death_judgement_model(),
    ] {
        for buggy in [0, 1] {
            let mut space = interp::with_buggy(&m, buggy);
            space.invariants.clear();
            let reached = interp::bmc(&space).expect("no invariants to break");
            eprintln!("{} Buggy={buggy}: {reached} states", m.name);
        }
    }
}

#[test]
fn keeper_death_judgement_proves_and_catches() {
    let model = keeper_death_judgement_model();
    assert!(registered(model.name), "enrolled in the spec-link registry");
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
    verify::prove_and_catch_scalar(
        &model,
        "keeper death judgement: a clean quit never resurrected, a crash never closed as a \
         quit, an orphan never listed after its shell died",
    );
}

/// The ends a person meets, judged by name.
#[test]
fn keeper_death_judgement_walks_the_ends() {
    let m = keeper_death_judgement_model();
    // A quit whose BYE was lost: exit(0) and the marker gone — closed.
    let lost = walk(&m, &["QuitByeLost", "Judge"]);
    assert_eq!((lost["judged_quit"], lost["orphaned"]), (1, 0), "{lost:?}");
    // SIGKILL: the marker stays with its lock free — an orphan, even when a
    // sweep removes the marker (the status still says a signal).
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
    // A panic's exit path removes the marker too, but its status is 101.
    let panic = walk(&m, &["PanicExit", "Judge"]);
    assert_eq!(panic["orphaned"], 1, "{panic:?}");
    // A P3 window's lost BYE has no marker to prove the quit: the design's
    // exception, judged a crash.
    let p3 = walk(&m, &["PickP3Window", "QuitByeLost", "Judge"]);
    assert_eq!(p3["orphaned"], 1, "{p3:?}");
    // A pre-BYE window's exit(0) is closed, as before the keeper.
    let old = walk(&m, &["PickPreByeWindow", "QuitWithBye", "Judge"]);
    assert_eq!((old["bye"], old["judged_quit"]), (0, 1), "{old:?}");
    // An orphan whose shell dies is pruned when the keeper reads the exit.
    let pruned = walk(&m, &["Killed", "Judge", "ShellDies", "KeeperSeesShellExit"]);
    assert_eq!(
        (pruned["orphaned"], pruned["watch_pending"]),
        (0, 0),
        "{pruned:?}"
    );
    // A shell that died before the judgement is never orphaned at all.
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
    // P3's classifier (no marker): a clean quit whose BYE was lost is orphaned.
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
    // The marker trusted alone: a panic read as a quit.
    let (closed, _) = interp::bmc(&only(&m, "CrashNeverClosed")).expect_err("the marker alone");
    assert_eq!(
        (closed["quit"], closed["judged_quit"]),
        (0, 1),
        "{closed:?}"
    );
    let replay = walk(&m, &["PanicExit", "Judge"]);
    assert!(!m.check_invariant("CrashNeverClosed", &replay));
    // P3's stale orphan: listed after its shell died.
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
    assert!(
        verify::uncaught_invariants(&base).is_empty(),
        "every invariant is a design claim with a mutant"
    );
}

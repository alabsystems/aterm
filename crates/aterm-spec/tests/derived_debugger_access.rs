// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `DebuggerAccessChange` (`docs/DESIGN-macos-debugger-access-2026-09-29.md` §5.1,
//! §5.4): `DevToolsSecurity -enable`/`-disable` runs only on a fresh "Allow…" in aterm's alert,
//! over a fresh `-status` read taken under the Mac-wide change lock, one child per Mac at a time;
//! a surface says "allowed" only when a probe read it; the park keeps re-reading while a change
//! is under way (`UnderWayIsPolled`); an exited child's result is always taken and re-probed.
//! The race between the worker's re-read and the tool's own write is named: `NoClobber` fails at
//! `Buggy = 0`. Tier-1 is `aterm-gui/src/debugger_access.rs`.

use crate::walk::walk;
use aterm_spec::derive::{
    Invariant, debugger_access_change_liveness, debugger_access_change_model, eq, int, var,
};
use aterm_spec::{interp, verify};

/// Probe (the rule reads as the precondition), press, consent, the locked
/// fresh re-read, and the launch: a child in flight.
const TO_FLIGHT: [&str; 5] = ["Probe", "Press", "Consent", "Recheck", "Launch"];

#[test]
fn the_change_runs_only_on_consent_over_a_fresh_read_one_child_at_a_time() {
    let model = debugger_access_change_model();
    verify::prove_and_catch_scalar(&model, "debugger access change");

    // The happy path: an observed `disabled`, the owner's two answers (the
    // press and aterm's alert), the locked re-read, one child, it applies, the
    // park takes the result, and only the re-probe says "allowed".
    let mut state = walk(&model, &TO_FLIGHT);
    assert_eq!(
        (
            state["launches"],
            state["inflight"],
            state["lock"],
            state["polling"]
        ),
        (1, 1, 1, 1),
        "{state:?}"
    );
    assert!(
        !model.action_enabled("Press", &state),
        "no press while the child runs: {state:?}"
    );
    assert!(
        !model.action_enabled("PeerLaunch", &state),
        "no peer child while ours holds the lock: {state:?}"
    );
    assert!(model.fire("ChildApplied", &mut state), "{state:?}");
    assert_eq!(
        (
            state["world"],
            state["claim"],
            state["phase"],
            state["lock"]
        ),
        (1, 0, 5, 0),
        "the exit alone claims nothing, and the result waits unread"
    );
    assert!(
        !model.action_enabled("Probe", &state),
        "no probe ends a change whose result nobody took: {state:?}"
    );
    for action in ["Notice", "Probe"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(
        (
            state["obs"],
            state["claim"],
            state["phase"],
            state["polling"]
        ),
        (1, 1, 0, 0)
    );

    // Cancel in macOS's dialog: the child exits unchanged, the re-probe says
    // so, and nothing is retried — another launch takes another consent.
    let mut cancelled = walk(&model, &TO_FLIGHT);
    for action in ["ChildUnchanged", "Notice", "Probe"] {
        assert!(
            model.fire(action, &mut cancelled),
            "{action}: {cancelled:?}"
        );
    }
    assert_eq!(
        (
            cancelled["obs"],
            cancelled["claim"],
            cancelled["launches"],
            cancelled["fresh"]
        ),
        (0, 0, 1, 0)
    );
    assert!(!model.action_enabled("Launch", &cancelled));

    // A decline in aterm's alert spawns nothing.
    let declined = walk(&model, &["Probe", "Press", "Decline"]);
    assert_eq!(
        (declined["phase"], declined["launches"], declined["fresh"]),
        (0, 0, 0)
    );

    // The rule changed after the render: the worker's re-read aborts, holds
    // nothing, spends the consent, and nothing launches — whether it moved to
    // a shape Apple's tool does not recognise or already to the target.
    for (moved, world) in [("Customize", 2), ("SetOutside", 1)] {
        let aborted = walk(&model, &["Probe", "Press", "Consent", moved, "Recheck"]);
        assert_eq!(
            (
                aborted["phase"],
                aborted["obs"],
                aborted["launches"],
                aborted["lock"],
                aborted["fresh"],
                aborted["polling"]
            ),
            (0, world, 0, 0, 0, 0),
            "{moved}: {aborted:?}"
        );
    }
    let drifted = walk(&model, &["Probe", "Customize", "Probe"]);
    assert!(
        !model.action_enabled("Press", &drifted),
        "no button over an unrecognised rule: {drifted:?}"
    );

    // A peer aterm's child holds the lock: our worker reads nothing past it,
    // spawns nothing, and the peer's exit frees the next change.
    let refused = walk(
        &model,
        &["Probe", "Press", "PeerLaunch", "Consent", "Recheck"],
    );
    assert_eq!(
        (
            refused["phase"],
            refused["launches"],
            refused["inflight"],
            refused["lock"],
            refused["obs"]
        ),
        (0, 0, 1, 2, 0),
        "{refused:?}"
    );
    let mut freed = refused;
    assert!(model.fire("PeerExit", &mut freed));
    for action in ["Press", "Consent", "Recheck", "Launch"] {
        assert!(model.fire(action, &mut freed), "{action}: {freed:?}");
    }
    assert_eq!((freed["inflight"], freed["lock"]), (1, 1));

    // A failed re-probe ends the change and claims nothing.
    let mut failed = walk(&model, &TO_FLIGHT);
    for action in ["ChildApplied", "Notice", "ProbeFails"] {
        assert!(model.fire(action, &mut failed), "{action}: {failed:?}");
    }
    assert_eq!((failed["phase"], failed["obs"], failed["claim"]), (0, 3, 0));

    // Each mutant, alone on the `Buggy = 1` baseline, breaks its invariant.
    let buggy = interp::with_buggy(&model, 1);
    let retried = walk(
        &verify::buggy_baseline_with(&model, "RetryAfterCancel"),
        &[
            "Probe",
            "Press",
            "Consent",
            "Recheck",
            "Launch",
            "ChildUnchanged",
            "Notice",
            "RetryAfterCancel",
            "Launch",
        ],
    );
    assert!(
        !buggy.check_invariant("NoActWithoutConsent", &retried),
        "{retried:?}"
    );
    let banked = walk(
        &verify::buggy_baseline_with(&model, "SpendBanked"),
        &["Probe", "SpendBanked", "Recheck", "Launch"],
    );
    assert!(
        !buggy.check_invariant("NoActWithoutConsent", &banked),
        "{banked:?}"
    );
    let doubled = walk(
        &verify::buggy_baseline_with(&model, "IgnorePeerLock"),
        &[
            "Probe",
            "Press",
            "Consent",
            "PeerLaunch",
            "IgnorePeerLock",
            "Launch",
        ],
    );
    assert!(!buggy.check_invariant("OneChild", &doubled), "{doubled:?}");
    let exit_claimed = walk(
        &verify::buggy_baseline_with(&model, "ClaimFromExitCode"),
        &[
            "Probe",
            "Press",
            "Consent",
            "Recheck",
            "Launch",
            "ChildUnchanged",
            "ClaimFromExitCode",
        ],
    );
    assert!(
        !buggy.check_invariant("ClaimIsObserved", &exit_claimed),
        "{exit_claimed:?}"
    );
    let stale = walk(
        &verify::buggy_baseline_with(&model, "SkipRecheck"),
        &["Probe", "Press", "Consent", "SkipRecheck", "Launch"],
    );
    assert!(
        !buggy.check_invariant("EveryLaunchFollowsAFreshRead", &stale),
        "{stale:?}"
    );
    let unpolled = walk(
        &verify::buggy_baseline_with(&model, "ResultWakeLost"),
        &[
            "Probe",
            "Press",
            "Consent",
            "Recheck",
            "Launch",
            "ChildApplied",
            "ResultWakeLost",
        ],
    );
    assert!(
        !buggy.check_invariant("UnderWayIsPolled", &unpolled),
        "{unpolled:?}"
    );
    // A second press while our child runs: the lock refuses its worker, and
    // the lane goes idle over a child that is still running.
    let orphaned = walk(
        &verify::buggy_baseline_with(&model, "PressWhileInFlight"),
        &[
            "Probe",
            "Press",
            "Consent",
            "Recheck",
            "Launch",
            "PressWhileInFlight",
            "Consent",
            "Recheck",
        ],
    );
    assert_eq!(orphaned["inflight"], 1, "the lock kept it to one child");
    assert!(
        !buggy.check_invariant("UnderWayIsPolled", &orphaned),
        "{orphaned:?}"
    );
    for (mutant, invariant) in [
        ("RetryAfterCancel", "NoActWithoutConsent"),
        ("SpendBanked", "NoActWithoutConsent"),
        ("IgnorePeerLock", "OneChild"),
        ("ClaimFromExitCode", "ClaimIsObserved"),
        ("SkipRecheck", "EveryLaunchFollowsAFreshRead"),
        ("ResultWakeLost", "UnderWayIsPolled"),
        ("PressWhileInFlight", "UnderWayIsPolled"),
        ("AdoptLive", "ReReadsOnlyAnEndedChange"),
    ] {
        let isolated = verify::buggy_baseline_with(&model, mutant);
        match interp::bmc(&isolated) {
            Err((_, broken)) => assert_eq!(broken, invariant, "{mutant}"),
            Ok(n) => panic!("{mutant} alone breaks nothing over {n} states"),
        }
    }
}

/// A records re-read mid-session: an idle lane adopts a dead marker's change as aterm's and
/// owes it a re-probe (0 → 6, polling on), follows a peer's running child as in flight (0 → 4)
/// until its exit leaves a result to read, and with a change of its own under way removes a
/// dead marker. The mutant takes a running child for a dead one.
#[test]
fn a_records_re_read_adopts_only_an_ended_change() {
    let model = debugger_access_change_model();

    // A peer's change ends; its marker stays until it records the re-probe.
    let mut adopted = walk(&model, &["Probe", "PeerLaunch", "PeerExit"]);
    assert_eq!((adopted["stale"], adopted["phase"]), (1, 0), "{adopted:?}");
    assert!(!model.action_enabled("Follow", &adopted), "{adopted:?}");
    assert!(model.fire("Adopt", &mut adopted), "{adopted:?}");
    assert_eq!(
        (adopted["phase"], adopted["polling"], adopted["stale"]),
        (6, 1, 0),
        "the adopted change is under way, re-read: {adopted:?}"
    );
    assert!(
        !model.action_enabled("Press", &adopted),
        "no press before its re-probe: {adopted:?}"
    );
    assert!(model.fire("Probe", &mut adopted), "{adopted:?}");
    assert_eq!((adopted["phase"], adopted["polling"]), (0, 0));
    // …or the peer recorded it first: nothing is left to adopt.
    let settled = walk(&model, &["Probe", "PeerLaunch", "PeerExit", "PeerSettle"]);
    assert!(!model.action_enabled("Adopt", &settled), "{settled:?}");

    // A peer's child still running is followed, never adopted.
    let mut followed = walk(&model, &["Probe", "PeerLaunch"]);
    assert!(!model.action_enabled("Adopt", &followed), "{followed:?}");
    assert!(model.fire("Follow", &mut followed), "{followed:?}");
    assert_eq!((followed["phase"], followed["polling"]), (4, 1));
    assert!(!model.action_enabled("Press", &followed), "{followed:?}");
    for exit in ["ChildApplied", "ChildUnchanged", "ChildClobbers"] {
        assert!(
            !model.action_enabled(exit, &followed),
            "{exit}: a followed child ends by its peer's exit"
        );
    }
    for action in ["PeerExit", "Notice", "Probe"] {
        assert!(model.fire(action, &mut followed), "{action}: {followed:?}");
    }
    assert_eq!(
        (followed["phase"], followed["stale"], followed["lock"]),
        (0, 0, 0),
        "{followed:?}"
    );

    // A dead marker found with this lane's own change under way goes.
    let mut discarded = walk(
        &model,
        &[
            "Probe",
            "Press",
            "Consent",
            "Recheck",
            "Launch",
            "ChildApplied",
            "Notice",
            "PeerLaunch",
            "PeerExit",
        ],
    );
    assert_eq!((discarded["phase"], discarded["stale"]), (6, 1));
    assert!(!model.action_enabled("Adopt", &discarded), "{discarded:?}");
    assert!(model.fire("Discard", &mut discarded), "{discarded:?}");
    assert_eq!((discarded["phase"], discarded["stale"]), (6, 0));

    // `AdoptLive`: a running child taken for a dead one.
    let buggy = interp::with_buggy(&model, 1);
    let misread = walk(
        &verify::buggy_baseline_with(&model, "AdoptLive"),
        &["Probe", "PeerLaunch", "AdoptLive"],
    );
    assert_eq!((misread["phase"], misread["inflight"]), (6, 1));
    assert!(
        !buggy.check_invariant("ReReadsOnlyAnEndedChange", &misread),
        "{misread:?}"
    );
}

/// The residual race is named, not hidden: at the committed config a rule customised while the
/// child waits is overwritten by the write that lands after authentication, so `NoClobber`
/// must fail there.
#[test]
fn the_race_between_the_re_read_and_the_tools_write_is_named() {
    let model = debugger_access_change_model();
    let mut with_no_clobber = model.clone();
    with_no_clobber.invariants.push(Invariant {
        name: "NoClobber",
        kind: aterm_spec::derive::InvariantKind::Claim,
        expr: eq(var("clobbered"), int(0)),
    });
    let committed = interp::with_buggy(&with_no_clobber, 0);
    let Err((state, broken)) = interp::bmc(&committed) else {
        panic!("NoClobber holds at Buggy = 0: the model no longer admits the named race");
    };
    assert_eq!(broken, "NoClobber", "{state:?}");
    assert_eq!(state["clobbered"], 1, "{state:?}");
    // The shape: launched over a fresh `disabled`, customised in flight, then
    // the child's write.
    let mut raced = walk(&model, &TO_FLIGHT);
    for action in ["Customize", "ChildClobbers"] {
        assert!(model.fire(action, &mut raced), "{action}: {raced:?}");
    }
    assert_eq!(
        (raced["world"], raced["clobbered"], raced["unarmed"]),
        (1, 1, 0)
    );
}

#[test]
fn an_exited_change_is_always_re_read_and_a_lost_wake_is_caught() {
    let model = debugger_access_change_model();
    let live = debugger_access_change_liveness();
    verify::liveness_proves_and_catches_tiered(&model, &live, "DebuggerAccessChange");

    // `ResultWakeLost`: the result waits in a slot nothing re-reads, and the row never learns
    // what the child did.
    let lost = verify::buggy_baseline_with(&model, "ResultWakeLost");
    let starved = interp::nonprogress_under(&lost, &live)
        .expect("a swallowed result wake with no re-read leaves the change unread");
    assert_eq!(starved.entry()["phase"], 5, "{starved}");
    assert_eq!(starved.entry()["polling"], 0, "{starved}");
}

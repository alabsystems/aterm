// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `DriverGeometry`: a driver's own lease never re-grids the session it may still
//! type into, so its `if-gen=` fence is never refused by a repaint its own hand caused, and the
//! fence still refuses the program's own output. Each `Buggy = 1` mutant is its own action.
//! Tier-1 (the real `turn` and `lease` verbs on a real pty) is
//! `aterm-gui/src/driver_geometry_conformance.rs`.

use crate::walk::{only, walk};
use aterm_spec::derive::{driver_geometry_liveness, driver_geometry_model};
use aterm_spec::{interp, verify};

#[test]
fn a_drivers_own_lease_never_moves_the_screen_its_fence_judged() {
    let model = driver_geometry_model();
    verify::prove_and_catch_scalar(&model, "driver geometry");
    let dead = [
        "RegridUnderTheHand",
        "CommitLiveOnly",
        "CommitTypingOnly",
        "DropNoWake",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(dead.len()),
        "every mutant is a negative control caught alone"
    );

    // The driver reads the quiet screen and takes the lease; the band's refresh cannot re-grid
    // under its hand, so the fence passes. Its input over, the row is born and the lease goes.
    let mut state = model.init_state();
    for action in ["Read", "Take"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(
        !model.fire("Regrid", &mut state.clone()),
        "no re-grid under a hand that may type: {state:?}"
    );
    for action in ["Fence", "EndInput", "Regrid", "Release"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!((state["refused"], state["rows"]), (0, 1), "{state:?}");

    // The fence keeps its guarantee: the program's own output between the
    // read and the paste refuses it — and is never counted against the hand.
    let moved = walk(&model, &["Read", "Output", "Take", "Fence", "Drop"]);
    assert_eq!(
        (moved["refused"], moved["refused_own"]),
        (1, 0),
        "{moved:?}"
    );

    // So does a re-grid another fact (mail) caused before the lease: the screen the driver
    // judged moved, and it reads again.
    let mail = walk(&model, &["Read", "Note", "Regrid", "Take", "Fence"]);
    assert_eq!((mail["refused"], mail["refused_own"]), (1, 0), "{mail:?}");

    // A cooperative lease held and let go re-grids nothing while held.
    let lease = walk(&model, &["Take", "Drop"]);
    assert_eq!((lease["gen"], lease["rows"]), (0, 0), "{lease:?}");

    // `RegridUnderTheHand`: the row commits on the lease, the program repaints, and the fence
    // the driver judged a moment earlier reads "changed".
    let incident = verify::buggy_baseline_with(&model, "RegridUnderTheHand");
    let refused = walk(&incident, &["Read", "Take", "RegridUnderTheHand", "Fence"]);
    assert_eq!(refused["refused_own"], 1, "{refused:?}");
    let mut state = incident.init_state();
    for action in ["Read", "Take", "RegridUnderTheHand", "Fence"] {
        assert!(incident.fire(action, &mut state), "{action}");
    }
    assert!(!incident.check_invariant("OwnHandNeverRefusesTheFence", &state));
    // …and a `lease acquire` alone resizes the tab.
    let mut state = incident.init_state();
    for action in ["Take", "RegridUnderTheHand"] {
        assert!(incident.fire(action, &mut state), "{action}");
    }
    assert!(!incident.check_invariant("NoRegridUnderTheHand", &state));
}

/// A refresh senses the facts under the lease, the lease moves (a live `lease acquire` and
/// `lease release` can land under 0.1 ms apart), and the commit births a row for a hand already
/// gone. A lease that moved since the sense holds the commit until the wake that moved it
/// re-derives the want, for a `lease release` and for a `turn`'s end of input and release
/// landing back to back.
#[test]
fn a_lease_that_moved_between_the_sense_and_the_commit_raises_no_row() {
    let model = driver_geometry_model();
    // The words a typing hand formed never commit once it is gone; the release's own wake
    // re-derives the want, and there is nothing to re-grid.
    let mut state = model.init_state();
    for action in ["Take", "Sense", "Drop"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(
        !model.fire("Commit", &mut state.clone()),
        "a row born for a hand already gone: {state:?}"
    );
    assert!(model.fire("Sense", &mut state), "{state:?}");
    assert!(!model.fire("Commit", &mut state.clone()), "{state:?}");
    assert_eq!((state["rows"], state["gen"]), (0, 0), "{state:?}");
    // The same for a turn's end of input and its release, back to back: the
    // words the settling turn formed are not born after it is gone.
    let mut state = model.init_state();
    for action in ["Take", "EndInput", "Sense", "Release"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!model.fire("Commit", &mut state.clone()), "{state:?}");
    assert!(model.fire("Sense", &mut state), "{state:?}");
    assert_eq!((state["rows"], state["gen"]), (0, 0), "{state:?}");
    // A turn's settle is still not held: words sensed after its input ended,
    // committed while it settles, are born, as `Regrid` births them.
    let settled = walk(&model, &["Take", "EndInput", "Sense", "Commit", "Release"]);
    assert_eq!((settled["rows"], settled["ghost"]), (1, 0), "{settled:?}");
    // …and a row the hand did not raise (mail) is born once the hand goes.
    let mail = walk(
        &model,
        &["Take", "Note", "Sense", "Drop", "Sense", "Commit"],
    );
    assert_eq!((mail["rows"], mail["ghost"]), (1, 0), "{mail:?}");

    // `CommitLiveOnly` (the live hold alone) and `CommitTypingOnly` (the snapshot's typing
    // alone) each birth the ghost row on its schedule, and only this invariant sees it.
    for (mutant, schedule) in [
        ("CommitLiveOnly", &["Take", "Sense", "Drop"][..]),
        (
            "CommitTypingOnly",
            &["Take", "EndInput", "Sense", "Release"][..],
        ),
    ] {
        let race = verify::buggy_baseline_with(&model, mutant);
        let mut flap = walk(&race, schedule);
        assert!(race.fire(mutant, &mut flap), "{mutant}: {flap:?}");
        assert_eq!((flap["rows"], flap["ghost"]), (1, 1), "{mutant}: {flap:?}");
        assert!(!race.check_invariant("NoBirthForAGoneHand", &flap));
        let (cex, _) = interp::bmc(&only(&race, "NoBirthForAGoneHand"))
            .expect_err("the mutant hold must birth a ghost row");
        assert_eq!(cex["ghost"], 1, "{mutant}: {cex:?}");
        for kept in [
            "NoRegridUnderTheHand",
            "OwnHandNeverRefusesTheFence",
            "EveryMovedLeaseOwesAWake",
        ] {
            assert!(
                interp::bmc(&only(&race, kept)).is_ok(),
                "{mutant} is the snapshot race alone, not {kept}"
            );
        }
    }
    // `CommitTypingOnly` does close the `lease release` race: only the turn's schedule sees it.
    let first_fix = verify::buggy_baseline_with(&model, "CommitTypingOnly");
    let state = walk(&first_fix, &["Take", "Sense", "Drop"]);
    assert!(!first_fix.fire("CommitTypingOnly", &mut state.clone()));
}

/// The hold's progress half: proven under weak fairness of `Sense` (a posted wake is handled),
/// `Commit` and `Catch` (the timer's owed commit), each of which the proof needs, and broken by
/// `DropNoWake` (a release that posts no wake) alone; the same mutant breaks
/// `EveryMovedLeaseOwesAWake`, its safety half.
#[test]
fn a_row_a_moved_lease_holds_catches_up_once_the_wake_is_handled() {
    let model = driver_geometry_model();
    let live = driver_geometry_liveness();
    verify::liveness_proves_and_catches_tiered(&model, &live, "DriverGeometry held row");
    // Mail wants the row while a lease holds it; the lease goes with no wake, and the held row
    // never catches up: no `Sense` is owed, and the words formed under the lease never commit.
    let silent = verify::buggy_baseline_with(&model, "DropNoWake");
    let stuck = walk(&silent, &["Take", "Note", "Sense", "DropNoWake"]);
    assert_eq!((stuck["rows"], stuck["other"], stuck["woke"]), (0, 1, 0));
    assert!(!silent.fire("Sense", &mut stuck.clone()));
    assert!(!silent.fire("Commit", &mut stuck.clone()));
    assert!(!live.goal_holds(&silent, &stuck));
    // …and its safety half, alone: a moved lease with no wake owed.
    assert!(!silent.check_invariant("EveryMovedLeaseOwesAWake", &stuck));
    let (cex, _) = interp::bmc(&only(&silent, "EveryMovedLeaseOwesAWake"))
        .expect_err("a release with no wake owes none");
    assert_eq!((cex["hand"], cex["woke"]), (0, 0), "{cex:?}");
}

/// A peer's lease that came and went unseen: mail wants the row while another session the
/// window shows holds a lease; the refresh's projection is turned away, and the peer's lease is
/// let go before its wake runs, so its sense finds its slot's copy unchanged and projects
/// nothing. No `Sense` or `Commit` is owed; only `Catch`, the timer's owed commit
/// (`rows_deadline`), catches the row up. This schedule is why `Catch`'s fairness is needed.
#[test]
fn a_row_a_peers_unseen_lease_held_is_caught_up_by_the_timer() {
    let model = driver_geometry_model();
    let live = driver_geometry_liveness();
    let stranded = walk(
        &model,
        &[
            "PeerTake",
            "Note",
            "Sense",
            "Turned",
            "PeerDrop",
            "PeerSense",
        ],
    );
    assert_eq!(
        (
            stranded["rows"],
            stranded["other"],
            stranded["peer"],
            stranded["sensed"]
        ),
        (0, 1, 0, 0),
        "{stranded:?}"
    );
    assert!(!live.goal_holds(&model, &stranded), "{stranded:?}");
    for owed in ["Sense", "Commit", "PeerSense"] {
        assert!(
            !model.fire(owed, &mut stranded.clone()),
            "{owed} is not what catches it up: {stranded:?}"
        );
    }
    let mut caught = stranded.clone();
    for action in ["Catch", "Commit"] {
        assert!(model.fire(action, &mut caught), "{action}: {caught:?}");
    }
    assert_eq!((caught["rows"], caught["ghost"]), (1, 0), "{caught:?}");
    assert!(live.goal_holds(&model, &caught));
    // The peer's hold stands while its lease does: no catch under it.
    let held = walk(&model, &["PeerTake", "Note", "Sense", "Turned"]);
    assert!(!model.fire("Catch", &mut held.clone()), "{held:?}");
    assert!(!model.fire("Commit", &mut held.clone()), "{held:?}");
}

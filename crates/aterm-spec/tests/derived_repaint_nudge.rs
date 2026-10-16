// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `RepaintNudge`: the host's closed-loop repair of a displaced
//! full-screen tab never strands a tab one row short, never heals on one
//! answer, tries once per epoch, never touches the engine, never ends silent
//! on a tab that still owes rows, and always ends.

use aterm_spec::{
    derive::{repaint_nudge_liveness, repaint_nudge_model},
    interp, verify,
};

#[test]
fn the_repaint_nudge_never_strands_a_tab_and_always_ends() {
    let model = repaint_nudge_model();
    verify::prove_and_catch_scalar(&model, "repaint nudge");
    let live = repaint_nudge_liveness();
    verify::liveness_proves_and_catches_tiered(&model, &live, "RepaintNudge TheNudgeEnds");

    // The healthy run: debt, a quiet moment, both answers, healed.
    let mut s = model.init_state();
    for a in [
        "Debt", "Arm", "Quiet", "Hold", "App1", "Release", "App2", "Finish",
    ] {
        assert!(model.fire(a, &mut s), "{a}: {s:?}");
    }
    assert_eq!((s["healed"], s["held"], s["debt"]), (1, 0, 0));

    // A silent app: the driver still lets go and gives up within its budget, after one try,
    // and the PTY was held, never the engine.
    let mut s = model.init_state();
    for a in [
        "Debt", "Arm", "Tick", "Tick", "Hold", "Tick", "Tick", "Release", "Tick", "Tick", "GiveUp",
    ] {
        assert!(model.fire(a, &mut s), "{a}: {s:?}");
    }
    assert_eq!(
        (s["phase"], s["held"], s["gaveup"], s["healed"], s["tries"]),
        (4, 0, 1, 0, 1)
    );
    assert_eq!(s["engine"], 0);

    // A flap during the hold that the app never saw: the driver forgets the hold (the PTY is
    // the layout's) and, nothing answered, gives up out loud. A resize that lasts gave the app
    // a size of its own: superseded, never a give-up; one that comes back after the wait is
    // the window's give-up. `DropOnFlap` and `ReturnUnwatched` end the first and the last
    // silently.
    let mut s = model.init_state();
    for a in [
        "Debt", "Arm", "Quiet", "Hold", "Flap", "Forget", "Tick", "Tick", "GiveUp",
    ] {
        assert!(model.fire(a, &mut s), "{a}: {s:?}");
    }
    assert_eq!((s["phase"], s["held"], s["gaveup"]), (4, 0, 1));
    let mut s = model.init_state();
    for a in [
        "Debt", "Arm", "Quiet", "Hold", "Resize", "Forget", "Tick", "Tick",
    ] {
        assert!(model.fire(a, &mut s), "{a}: {s:?}");
    }
    assert!(!model.action_enabled("GiveUp", &s), "{s:?}");
    assert!(model.fire("Supersede", &mut s), "{s:?}");
    assert!(
        !model.action_enabled("Report", &s),
        "the resize stands: {s:?}"
    );
    let mut unwatched = s.clone();
    for a in ["Return", "Report"] {
        assert!(model.fire(a, &mut s), "{a}: {s:?}");
    }
    assert_eq!((s["phase"], s["gaveup"], s["superseded"]), (4, 1, 0));
    let old = interp::with_buggy(&model, 1);
    assert!(old.fire("ReturnUnwatched", &mut unwatched), "{unwatched:?}");
    assert!(
        !old.check_invariant("AnUnrepairedEndIsReported", &unwatched),
        "{unwatched:?}"
    );
    let mut s = old.init_state();
    for a in ["Debt", "Arm", "Quiet", "Hold", "Flap", "DropOnFlap"] {
        assert!(old.fire(a, &mut s), "{a}: {s:?}");
    }
    assert!(
        !old.check_invariant("AnUnrepairedEndIsReported", &s),
        "{s:?}"
    );

    // The mutants: a retry without a budget breaks OneTryPerEpoch; a hold that resizes the
    // engine breaks NudgeNeverTouchesTheEngine.
    let old = interp::with_buggy(&model, 1);
    let mut s = old.init_state();
    for a in [
        "Debt",
        "Arm",
        "Tick",
        "Tick",
        "Hold",
        "Tick",
        "Tick",
        "Release",
        "Tick",
        "Tick",
        "RetryForever",
        "Tick",
        "Tick",
        "Hold",
    ] {
        assert!(old.fire(a, &mut s), "{a}: {s:?}");
    }
    assert!(!old.check_invariant("OneTryPerEpoch", &s), "{s:?}");
    let mut s = old.init_state();
    for a in ["Debt", "Arm", "Quiet", "HoldTheEngine"] {
        assert!(old.fire(a, &mut s), "{a}: {s:?}");
    }
    assert!(
        !old.check_invariant("NudgeNeverTouchesTheEngine", &s),
        "{s:?}"
    );

    // A halt stands before the hold: nothing starts (Hold is not enabled), and
    // one that arrives during the hold lifts it at once.
    let mut s = model.init_state();
    for a in ["Debt", "Arm", "Quiet", "Halt"] {
        assert!(model.fire(a, &mut s), "{a}: {s:?}");
    }
    assert!(
        !model.fire("Hold", &mut s),
        "a standing halt must block the hold"
    );
    let mut s = model.init_state();
    for a in ["Debt", "Arm", "Quiet", "Hold", "Halt", "Release"] {
        assert!(model.fire(a, &mut s), "{a}: {s:?}");
    }
    assert_eq!((s["held"], s["phase"]), (0, 3));

    // The driver's record is lost while the PTY is short: the hard deadline
    // still lets it go, and the hold never outlives the cap.
    let mut s = model.init_state();
    for a in [
        "Debt",
        "Arm",
        "Quiet",
        "Hold",
        "DriverVanishes",
        "Age",
        "Age",
        "ReleaseByDeadline",
    ] {
        assert!(model.fire(a, &mut s), "{a}: {s:?}");
    }
    assert_eq!((s["held"], s["orphan"], s["age"]), (0, 0, 0));

    // Without the cap an orphaned hold outlives every deadline.
    let old = interp::with_buggy(&model, 1);
    let mut s = old.init_state();
    for a in [
        "Debt",
        "Arm",
        "Quiet",
        "Hold",
        "DriverVanishes",
        "Age",
        "Age",
        "NoCap",
    ] {
        assert!(old.fire(a, &mut s), "{a}: {s:?}");
    }
    assert!(
        !old.check_invariant("AHeldPtyIsReleasedByTheDeadline", &s),
        "{s:?}"
    );
}

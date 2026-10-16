// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{
    derive::{
        exceptions_digest_ends_liveness, exceptions_digest_ends_model, exceptions_digest_model,
    },
    interp, verify,
};

#[test]
fn a_later_never_wants_a_row_and_a_dismissal_holds() {
    let model = exceptions_digest_model();
    verify::prove_and_catch_scalar(
        &model,
        "Exceptions digest: no row, no loss, dismissal holds",
    );

    // An exception beside a Now row, dismissed, suppressed, resurfaced.
    let mut s = model.init_state();
    for action in [
        "PostNow",
        "PostLater",
        "PostLater",
        "FileAll",
        "Suppress",
        "Resurface",
        "PostLater",
        "FoldNow",
    ] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["glass"], s["now_rows"], s["open"]), (0, 0, 1));
    assert!(
        !model.action_enabled("PostLater", &{
            let mut q = model.init_state();
            assert!(model.fire("PostLater", &mut q));
            assert!(model.fire("FileAll", &mut q));
            q
        }),
        "a dismissed fact cannot be posted again inside its span"
    );

    // The cap files, it does not drop.
    let mut c = model.init_state();
    for _ in 0..12 {
        assert!(model.fire("PostLater", &mut c));
    }
    assert!(!model.action_enabled("PostLater", &c));
    assert!(model.fire("PostLaterAtCap", &mut c));
    assert_eq!((c["open"], c["filed"], c["lost"]), (12, 1, 0));

    // Each mutant breaks exactly its own invariant.
    let buggy = interp::with_buggy(&model, 1);
    let mut seated = buggy.init_state();
    assert!(buggy.fire("PostLaterSeated", &mut seated));
    assert!(!buggy.check_invariant("LaterNeverWantsARow", &seated));
    let mut through = buggy.init_state();
    assert!(buggy.fire("PostLater", &mut through));
    assert!(buggy.fire("FileAll", &mut through));
    assert!(buggy.fire("PostThroughQuiet", &mut through));
    assert!(!buggy.check_invariant("DismissalHolds", &through));
    let mut dropped = buggy.init_state();
    for _ in 0..12 {
        assert!(buggy.fire("PostLater", &mut dropped));
    }
    assert!(buggy.fire("CapDrops", &mut dropped));
    assert!(!buggy.check_invariant("NothingIsLost", &dropped));
    let mut over = buggy.init_state();
    for _ in 0..12 {
        assert!(buggy.fire("PostLater", &mut over));
    }
    assert!(buggy.fire("OverCap", &mut over));
    assert!(!buggy.check_invariant("OpenIsBounded", &over));
}

/// The digest's end is a progress claim, stated as liveness. The upgrade sends a look only when
/// its rows change, so a row that leaves in one change yields one empty look and no second;
/// without its own clock the entry (and the menu-bar dot) stands for ever.
#[test]
fn an_unnamed_entry_ends_by_its_own_clock_and_the_mutants_are_caught() {
    let model = exceptions_digest_ends_model();
    let live = exceptions_digest_ends_liveness();
    // Safety around it: no early end, no orphan clock (each falsified by its mutant).
    verify::prove_and_catch_scalar(
        &model,
        "Exceptions digest ends: never early, no orphan clock",
    );
    // The claim itself, on both tiers: proven under Tick and Settle alone, each
    // fairness load-bearing, both mutants breaking it alone.
    verify::liveness_proves_and_catches_tiered(
        &model,
        &live,
        "ExceptionsDigestEnds AnUnnamedEntryEnds",
    );

    // The healthy path: the cause ends, one look misses the entry, time passes, the loop takes
    // its wake, and the entry is filed with its clock.
    let mut s = model.init_state();
    for action in ["CauseEnds", "Miss", "Tick", "Tick", "Settle"] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["open"], s["missed"], s["ended_age"]), (0, 0, 2));
    // A flicker is not an end: named again, the clock is gone.
    let mut f = model.init_state();
    for action in ["Miss", "Tick", "Name"] {
        assert!(model.fire(action, &mut f), "{action}: {f:?}");
    }
    assert_eq!((f["open"], f["missed"], f["age"]), (1, 0, 0));
    assert!(!model.action_enabled("Settle", &f), "nothing to settle");

    // `TwoLooksToEnd`: one empty look arms no clock, and no second look comes.
    let two = verify::buggy_baseline_with(&model, "TwoLooksToEnd");
    let stuck = interp::nonprogress_under(&two, &live)
        .expect("one look and no second one leaves the entry open for ever");
    assert!(
        stuck.entry()["frozen"] == 1 && stuck.entry()["open"] == 1 && stuck.cycle.is_empty(),
        "the run stutters at the missed, clockless entry:\n{stuck}"
    );
    // A clock nobody wakes: the deadline is named, time passes, nothing settles.
    let unwired = verify::buggy_baseline_with(&model, "NobodyCallsSettle");
    let orphan = interp::nonprogress_under(&unwired, &live)
        .expect("a deadline with no caller leaves the entry open for ever");
    assert_eq!(
        (orphan.entry()["unwired"], orphan.entry()["age"]),
        (1, 2),
        "the whole clock ran and nothing ended the entry:\n{orphan}"
    );
    // Each safety mutant breaks exactly its own invariant.
    let buggy = interp::with_buggy(&model, 1);
    let mut early = buggy.init_state();
    for action in ["Miss", "EndsBeforeTheClockRan"] {
        assert!(buggy.fire(action, &mut early), "{action}: {early:?}");
    }
    assert!(!buggy.check_invariant("NoEarlyEnd", &early));
    assert!(buggy.check_invariant("ClockDiesWithTheEntry", &early));
    let mut orphan_clock = buggy.init_state();
    for action in ["Miss", "Tick", "Tick", "ClockOutlivesTheEntry"] {
        assert!(
            buggy.fire(action, &mut orphan_clock),
            "{action}: {orphan_clock:?}"
        );
    }
    assert!(!buggy.check_invariant("ClockDiesWithTheEntry", &orphan_clock));
    assert!(buggy.check_invariant("NoEarlyEnd", &orphan_clock));
}

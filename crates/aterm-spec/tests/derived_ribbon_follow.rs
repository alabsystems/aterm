// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{
    derive::{ribbon_arrival_evidence_model, ribbon_follow_twin_model},
    interp, verify,
};

/// The twin contract proves at `Buggy = 0` and CATCHES the 2026-09-22 veto
/// at `Buggy = 1` (a STANDING record read as ABSENT): one twin inside an
/// arrived block refused the follow and the row melted — the owner's vanish.
#[test]
fn ribbon_follow_twin_proves_and_catches_the_twin_veto() {
    let model = ribbon_follow_twin_model();
    verify::prove_and_catch_scalar(&model, model.name);
}

/// The owner's shape at four records, both ways: ARRIVED, STANDING (the
/// `Y` of `WHY` under the `Y` of `HEY`), ARRIVED, ARRIVED, every glyph gone
/// from its own row. The contract names the whole run, `1..=4`; the veto
/// names nothing and counts nothing.
#[test]
fn a_twin_inside_an_arrived_block_follows_and_the_veto_melts_it() {
    let scan = [
        "ScanArrivedGone",
        "ScanStandingGone",
        "ScanArrivedGone",
        "ScanArrivedGone",
        "Decide",
    ];
    let model = ribbon_follow_twin_model();
    let mut state = model.init_state();
    for action in scan {
        assert!(model.fire(action, &mut state), "{action} is enabled");
    }
    assert_eq!(
        (state["named"], state["lo"], state["hi"], state["missed"]),
        (1, 1, 4, 0)
    );
    for inv in [
        "AllStandingNeverFollows",
        "AnArrivedBlockFollows",
        "OnlyAnArrivedBlockFollows",
        "MissedIsAnArrivalNotNamed",
        "StateBounded",
    ] {
        assert!(model.check_invariant(inv, &state), "{inv}");
    }
    let mut buggy = ribbon_follow_twin_model();
    for c in &mut buggy.consts {
        if c.0 == "Buggy" {
            c.1 = 1;
        }
    }
    let mut state = buggy.init_state();
    for action in scan {
        assert!(buggy.fire(action, &mut state), "{action} is enabled");
    }
    assert_eq!(state["named"], 0, "the veto names nothing");
    assert!(!buggy.check_invariant("AnArrivedBlockFollows", &state));
}

/// The writer's contract proves at `Buggy = 0`, and each historical writer
/// is caught — by its OWN invariant alone, so no invariant is a ghost:
/// `Buggy = 1` (twins only at arm, 2026-09-23: fzf's list landing under the
/// query was followed), `Buggy = 2` (fail-closed: an offset clear only once
/// seen holding something else — the missed follows), `Buggy = 3` (no twin
/// at all, before 2026-09-22: Ctrl-U under an identical line lit it).
#[test]
fn ribbon_arrival_evidence_proves_and_each_old_writer_is_caught_by_its_own_law() {
    let model = ribbon_arrival_evidence_model();
    verify::prove_and_catch_scalar(&model, model.name);
    for (bug, law) in [
        (1, "ACopyThatStoodIsNoArrival"),
        (2, "AnythingElseIsAnArrival"),
        (3, "CopyThereFirstIsNoArrival"),
    ] {
        let buggy = interp::with_buggy(&model, bug);
        let (_, caught) = interp::bmc(&buggy).expect_err("the old writer is caught");
        assert_eq!(caught, law, "Buggy = {bug}");
        let mut alone = buggy.clone();
        alone.invariants.retain(|i| i.name == law);
        assert!(
            interp::bmc(&alone).is_err(),
            "{law} alone catches Buggy = {bug}"
        );
    }
}

/// The fzf trace, both ways: armed with the row below showing something
/// else, then the list lands there (SAME) and stands two units — 40 ms. The
/// contract has written a twin: no arrival. The 2026-09-23 writer has not.
#[test]
fn a_copy_that_lands_beside_the_line_and_stands_forty_ms_is_no_arrival() {
    let trace = ["ArmDiff", "SameAfterOne", "SameAfterTwo"];
    for (bug, twin) in [(0, 1), (1, 0)] {
        let model = interp::with_buggy(&ribbon_arrival_evidence_model(), bug);
        let mut st = model.init_state();
        for a in trace {
            assert!(model.fire(a, &mut st), "{a}");
        }
        assert_eq!((st["stood"], st["twin"]), (1, twin), "Buggy = {bug}");
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{
    derive::{harness_upgrade_look_model, harness_worker_lifecycle_model},
    interp, verify,
};

#[test]
fn one_supervisor_per_session_across_restarts_and_reloads() {
    let model = harness_worker_lifecycle_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the harness worker lifecycle must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness worker lifecycle");

    // A reload while a worker parks in its wait: the shipped host starts the
    // next worker only once the old one has ended.
    let schedule = ["Arrive", "Start", "Reload"];
    let mut state = model.init_state();
    for action in schedule {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(!model.action_enabled("Start", &state), "{state:?}");
    assert!(model.fire("Exit", &mut state));
    assert!(model.fire("Start", &mut state));

    // A host that did not wait: two supervisors on one session.
    let eager = interp::with_buggy(&model, 1);
    let mut two = eager.init_state();
    for action in ["Arrive", "Start", "Reload", "Start"] {
        assert!(eager.fire(action, &mut two), "{action}: {two:?}");
    }
    assert!(!eager.check_invariant("OneSupervisor", &two));

    // The budget: Budget failures restart in place unbadged; the next one
    // is badged — and STILL restarted (the philosophy review of 2026-09-25:
    // it was turned off until the policy changed), the badge going as the
    // restart runs.
    let mut s = model.init_state();
    assert!(model.fire("Arrive", &mut s) && model.fire("Start", &mut s));
    for _ in 0..model.consts.iter().find(|c| c.0 == "Budget").unwrap().1 {
        assert!(model.fire("Fail", &mut s));
        assert_eq!((s["cur"], s["faulted"]), (1, 0), "{s:?}");
    }
    assert!(model.fire("Fail", &mut s));
    assert_eq!((s["cur"], s["faulted"]), (1, 1), "{s:?}");
    let mut restarted = s.clone();
    assert!(model.fire("Restart", &mut restarted));
    assert_eq!((restarted["cur"], restarted["faulted"]), (1, 0));

    // The budget is the SESSION's: the program leaving and coming back
    // clears the badge but not the count, so the next failure is badged at
    // once (a flap gives a crash-looping engine no fresh budget)…
    let mut flap = s.clone();
    for action in ["Leave", "Exit", "Arrive", "Start", "Fail"] {
        assert!(model.fire(action, &mut flap), "{action}: {flap:?}");
    }
    assert_eq!((flap["cur"], flap["faulted"]), (1, 1), "{flap:?}");
    // …the window ageing a failure out gives one unbadged run back…
    let mut aged = s.clone();
    for action in ["Leave", "Exit", "Age", "Age", "Arrive", "Start", "Fail"] {
        assert!(model.fire(action, &mut aged), "{action}: {aged:?}");
    }
    assert_eq!((aged["cur"], aged["faulted"]), (1, 0), "{aged:?}");
    // …and a changed policy forgives it all.
    for action in ["Reload", "Exit", "Start"] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["faults"], s["faulted"]), (0, 0), "{s:?}");

    // A host that gave up past the budget: badged with nobody supervising.
    let quits = interp::with_buggy(&model, 1);
    let mut q = quits.init_state();
    assert!(quits.fire("Arrive", &mut q) && quits.fire("Start", &mut q));
    for _ in 0..=quits.consts.iter().find(|c| c.0 == "Budget").unwrap().1 {
        assert!(quits.fire("Fail", &mut q), "{q:?}");
    }
    assert_eq!((q["cur"], q["faulted"]), (0, 1), "{q:?}");
    assert!(!quits.check_invariant("NeverGivesUp", &q));
}

/// THE UPGRADE'S LOOKS ARE NEVER LET GO FOR WHAT A LOOK COULD NOT READ, AND
/// A FRESH LAUNCH IS BEHIND FROM ITS ATTACH (the live re-test of 155c72a28,
/// 2026-09-26): a look refused by the instance's socket — or before Claude
/// Code's record is written — owes a later look, so an upgrade due, a READY
/// answer's restart included, always has one coming; only a look that reads
/// nothing to take lets it go. The note behind owed from the attach is asked
/// until the record reads, and the state it mints is behind from the attach.
/// NEGATIVE CONTROLS: the host of 155c72a28 strands a READY'd session after
/// a refused look, and — its note given up at the attach — mints the state
/// at the first step, its age from then.
#[test]
fn an_upgrade_is_never_let_go_for_a_look_that_could_not_read() {
    let model = harness_upgrade_look_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the upgrade's look machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade look");

    // A fresh launch: the note unread at the attach, asked again once the
    // record is written, the state behind from the attach — then the first
    // look steps it.
    let mut f = model.init_state();
    for action in ["NoteUnread", "Regain", "Note", "Look"] {
        assert!(model.fire(action, &mut f), "{action}: {f:?}");
    }
    assert_eq!((f["noted"], f["aged"], f["ready"]), (1, 1, 1), "{f:?}");
    // READY, then a refused look: looked at again, and restarted once read.
    for action in ["Lose", "Unread", "Unread", "Regain", "Look"] {
        assert!(model.fire(action, &mut f), "{action}: {f:?}");
    }
    assert_eq!((f["done"], f["looks"]), (1, 0), "{f:?}");
    // Past the host's asks, the first idle point asks the note before its step.
    let mut i = model.init_state();
    for action in ["Regain", "Look"] {
        assert!(model.fire(action, &mut i), "{action}: {i:?}");
    }
    assert_eq!((i["owed"], i["noted"], i["aged"]), (0, 1, 1), "{i:?}");
    // A look that reads nothing to take lets it go.
    let mut n = model.init_state();
    for action in ["Regain", "Settle", "NotDue"] {
        assert!(model.fire(action, &mut n), "{action}: {n:?}");
    }
    assert_eq!((n["looks"], n["owed"], n["noted"]), (0, 0, 0), "{n:?}");

    // The host of 155c72a28: the READY'd session let go by a refused look…
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    for action in ["Regain", "Look", "Lose", "Unread"] {
        assert!(buggy.fire(action, &mut b), "{action}: {b:?}");
    }
    assert_eq!((b["ready"], b["looks"]), (1, 0), "{b:?}");
    assert!(!buggy.check_invariant("NeverDropped", &b));
    // …and the note given up at the attach: behind from the first step.
    let mut g = buggy.init_state();
    for action in ["NoteUnread", "Regain", "Look"] {
        assert!(buggy.fire(action, &mut g), "{action}: {g:?}");
    }
    assert_eq!((g["noted"], g["aged"]), (1, 0), "{g:?}");
    assert!(!buggy.check_invariant("BehindFromTheAttach", &g));
}

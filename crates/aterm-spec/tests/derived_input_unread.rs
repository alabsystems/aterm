// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `InputUnreadGate`: the unread-input gate proves over the whole
//! bounded space and the screen-only fence (`Buggy = 1`) is caught. Tier-1 —
//! the real pty, the real sink and the shipped `refuses` — is
//! `aterm-session/tests/conformance_input_unread.rs`.

use aterm_spec::{derive::input_unread_gate_model, interp, verify};

#[test]
fn input_unread_gate_proves_and_catches_the_screen_only_fence() {
    let model = input_unread_gate_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the unread-input gate must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(
        &model,
        "input unread gate: no driver key behind stale unread input",
    );
}

/// The incident's schedule, step by step: the program freezes, the human's
/// Enter is queued, and it ages past `Refuse`. The gate withholds the
/// driver's key; the screen-only fence admits it and lands it behind the
/// stale Enter.
#[test]
fn the_incident_schedule_is_refused_by_the_gate_and_admitted_by_the_screen_fence() {
    let model = input_unread_gate_model();
    let schedule = ["Freeze", "HumanKey", "Tick", "Tick"];

    let mut gated = model.init_state();
    for action in schedule {
        assert!(model.fire(action, &mut gated), "{action}: {gated:?}");
    }
    assert_eq!((gated["q"], gated["a1"]), (1, 2));
    assert!(!model.action_enabled("DriverKey", &gated));
    assert!(
        model.action_enabled("HumanKey", &gated),
        "the human's window keyboard is never gated"
    );

    let fence = interp::with_buggy(&model, 1);
    let mut screen = fence.init_state();
    for action in schedule {
        assert!(fence.fire(action, &mut screen), "{action}: {screen:?}");
    }
    assert!(fence.fire("DriverKey", &mut screen));
    assert!(!fence.check_invariant("NoDriverKeyBehindStaleInput", &screen));
}

/// What the gate still admits: a key into an empty queue, a key behind
/// input younger than `Refuse`, and a key behind canonical type-ahead of any
/// age. A stopped job refuses at any age, because nothing will read the
/// queue until it is continued.
#[test]
fn the_gate_admits_fresh_and_canonical_input_and_refuses_a_stopped_job() {
    let model = input_unread_gate_model();
    let mut s = model.init_state();
    assert!(model.action_enabled("DriverKey", &s), "empty queue");

    assert!(model.fire("HumanKey", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.action_enabled("DriverKey", &s), "younger than Refuse");

    assert!(model.fire("StopJob", &mut s));
    assert!(!model.action_enabled("DriverKey", &s), "a stopped job");
    assert!(model.fire("ContJob", &mut s));

    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(!model.action_enabled("DriverKey", &s), "raw and stale");
    assert!(model.fire("ModeFlip", &mut s));
    assert!(
        model.action_enabled("DriverKey", &s),
        "canonical type-ahead"
    );
    assert!(model.fire("DriverKey", &mut s));
    assert!(model.check_invariant("NoDriverKeyBehindStaleInput", &s));
}

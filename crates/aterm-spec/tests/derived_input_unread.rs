// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `InputUnreadGate`: the unread-input gate proves over the whole
//! bounded space, and each of its three invariants is caught by the mutant
//! family (`Buggy = 1`): the screen-only fence of 2026-09-24, the static 1 s
//! refusal the owner overruled on 2026-09-29, and a stall published on age
//! alone. Tier-1 — the real pty, the real sink and the shipped `gate` and
//! `classify` — is `aterm-session/tests/conformance_input_unread.rs`.

use crate::walk::walk;
use aterm_spec::derive::input_unread_gate_model;
use aterm_spec::{interp, verify};

#[test]
fn input_unread_gate_proves_and_catches_every_invariant() {
    let model = input_unread_gate_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the unread-input gate must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(
        &model,
        "input unread gate: no key behind a stall, no refusal without one",
    );
    assert_eq!(
        verify::uncaught_invariants(&model),
        Vec::<&str>::new(),
        "each invariant, alone, is falsified by the mutant family"
    );
}

/// THE INCIDENT: the program freezes, the human's Enter is queued, it ages
/// into the stall band and the watch publishes the stall. The gate withholds
/// the driver's key (a refusal that IS established, so it latches nothing);
/// the screen-only fence admits it and lands it behind the stale Enter.
#[test]
fn the_incident_is_refused_once_published_and_admitted_by_the_screen_fence() {
    let model = input_unread_gate_model();
    let schedule = ["Freeze", "HumanKey", "Tick", "Tick", "Publish"];
    let mut gated = walk(&model, &schedule);
    assert_eq!((gated["q"], gated["a1"], gated["published"]), (1, 2, 1));
    assert!(!model.action_enabled("DriverKey", &gated));
    assert!(model.fire("DriverRefused", &mut gated));
    assert!(model.check_invariant("NoRefusalWithoutAnEstablishedStall", &gated));
    assert!(
        model.action_enabled("HumanKey", &gated),
        "the human's window keyboard is never gated"
    );

    let fence = interp::with_buggy(&model, 1);
    let mut screen = walk(&fence, &schedule);
    assert!(fence.fire("DriverKey", &mut screen));
    assert!(!fence.check_invariant("NoDriverKeyBehindStalledInput", &screen));
}

/// THE OWNER'S DIRECTION (2026-09-29): a program that is merely SLOW — it
/// reads, it has not read yet — has input in the warn band and even in the
/// stall band, and a driver's key is WRITTEN, with the warning; the watch
/// does not publish it. The static cap (the mutant) refuses the same key
/// with nothing established, and publishes the same program on age alone.
#[test]
fn a_slow_program_is_written_to_with_a_warning_and_never_published() {
    let model = input_unread_gate_model();
    // Warn band: written, warned.
    let mut s = walk(&model, &["HumanKey", "Tick"]);
    assert!(!model.action_enabled("DriverRefused", &s), "no refusal");
    assert!(model.fire("DriverKey", &mut s));
    assert_eq!(s["warned"], 1, "the reply carries the warning");
    // Stall band, still reading: written, never published.
    let s = walk(&model, &["HumanKey", "Tick", "Tick"]);
    assert!(!model.action_enabled("Publish", &s), "a reading program");
    assert!(!model.action_enabled("DriverRefused", &s));
    assert!(model.action_enabled("DriverKey", &s));
    // A fresh queue: written, no warning.
    let mut fresh = walk(&model, &["HumanKey"]);
    assert!(model.fire("DriverKey", &mut fresh));
    assert_eq!(fresh["warned"], 0);

    // THE STATIC CAP: the same warn-band key refused, the same slow program
    // published.
    let cap = interp::with_buggy(&model, 1);
    let mut s = walk(&cap, &["HumanKey", "Tick"]);
    assert!(cap.fire("DriverRefused", &mut s));
    assert!(!cap.check_invariant("NoRefusalWithoutAnEstablishedStall", &s));
    let mut s = walk(&cap, &["HumanKey", "Tick", "Tick"]);
    assert!(cap.fire("Publish", &mut s));
    assert!(!cap.check_invariant("NoReadingProgramPublished", &s));
}

/// What else the gate decides: a stopped job refuses at any age (nothing will
/// read the queue until it is continued), canonical type-ahead of any age is
/// written, and a published stall whose program has since read — what is
/// queued now is younger than the stall — is written to and withdrawn.
#[test]
fn a_stopped_job_refuses_typeahead_passes_and_a_read_lifts_the_stall() {
    let model = input_unread_gate_model();
    let mut s = walk(&model, &["HumanKey", "StopJob"]);
    assert!(!model.action_enabled("DriverKey", &s), "a stopped job");
    assert!(model.fire("DriverRefused", &mut s));
    assert!(model.check_invariant("NoRefusalWithoutAnEstablishedStall", &s));

    let mut s = walk(&model, &["Freeze", "HumanKey", "Tick", "Tick", "ModeFlip"]);
    assert!(
        model.action_enabled("DriverKey", &s),
        "canonical type-ahead"
    );
    assert!(!model.action_enabled("Publish", &s));
    assert!(model.fire("DriverKey", &mut s));
    assert_eq!(s["warned"], 0, "type-ahead is not a warning");

    // Published, then the program wakes and reads: the gate opens at once
    // (the queue is empty) and the watch withdraws.
    let mut s = walk(
        &model,
        &[
            "Freeze", "HumanKey", "Tick", "Tick", "Publish", "Thaw", "Consume",
        ],
    );
    assert!(model.action_enabled("DriverKey", &s));
    assert!(model.fire("Withdraw", &mut s));
    assert_eq!(s["published"], 0);
}

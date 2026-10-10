// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-0 of the explicit restart (`HarnessRestartStamp`, design of
//! 2026-10-08): its stamp stands before its signal, its exit is recognised
//! by lookup whoever reads it — however many acts there were — and it always
//! ends the program it stamped, its KILL waiting out another hand on the
//! session and its exit decided only once its program ended (review of
//! 1a733ed60), and however many acts there were, one watch ends it (bug hunt
//! 7).

use aterm_spec::derive::{Model, harness_restart_stamp_liveness, harness_restart_stamp_model};
use aterm_spec::verify;

/// Fires `actions` in turn from the model's initial state.
fn run(model: &Model, actions: &[&str]) -> aterm_spec::interp::State {
    let mut state = model.init_state();
    for action in actions {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

/// THE ACT'S END IS RELAUNCHED WHOEVER READS IT, NOTHING IS THE ACT'S UNLESS
/// ITS STAMP STOOD, THE ACT ENDS ITS PROGRAM, AND ITS KILL WAITS OUT ANOTHER
/// HAND: proved over every reachable state, each mutant caught by its
/// invariant alone. A hearing program's exit
/// on `TERM` is read as the act's, by this instance or by a successor after a
/// Commit; a deaf program is killed by the act's thread, or by the successor's
/// carrier when a Commit cut the act; a person's exit with no stamp is read by
/// the other rules.
#[test]
fn the_restart_act_is_read_by_its_stamp_and_ends_its_program() {
    let model = harness_restart_stamp_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the restart's stamp must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "the explicit restart's stamp");

    // A hearing program: TERM, its own exit, read as the act's.
    let heard = run(&model, &["Stamp", "Term", "Hears", "Read"]);
    assert_eq!((heard["ended"], heard["read"], heard["stamped"]), (1, 1, 0));
    // A deaf one: the escalation's KILL, read as the act's.
    let deaf = run(&model, &["Stamp", "Term", "Escalate", "Read"]);
    assert_eq!((deaf["ended"], deaf["read"]), (1, 1));
    // A Commit inside the grace: the thread dies with the outgoing instance,
    // the successor's carrier finishes the KILL, and the successor's reader
    // reads it by the stamp on disk.
    let cut = run(
        &model,
        &["Stamp", "Term", "Commit", "CarrierFinishes", "Read"],
    );
    assert_eq!((cut["ended"], cut["read"]), (1, 1));
    // TWO ACTS (review of 3eaa8f83c): the second finds the first's stamp
    // standing and JOINS it; the program the first act's TERM ends is gone
    // at the second's re-read, so it sends nothing — and takes back nothing
    // it did not write: the first act's end is relaunched.
    let joined = run(
        &model,
        &["Stamp", "Term", "Stamp", "Hears", "SendsNothing", "Read"],
    );
    assert_eq!((joined["ended"], joined["read"]), (1, 1));
    // ONE WATCH PER PROCESS (bug hunt 7, ruling 19 of 2026-10-09): an act
    // that joins and signals gives the running watch its time; however many
    // acts there were, one watch ends the program.
    let many = run(
        &model,
        &[
            "Stamp", "Term", "Stamp", "Term", "Stamp", "Term", "Escalate", "Read",
        ],
    );
    assert_eq!((many["esc"], many["ended"], many["read"]), (0, 1, 1));
    assert_eq!(
        run(&model, &["Stamp", "Term", "Stamp", "Term"])["esc"],
        1,
        "a joined act's signal starts no second watch"
    );
    // An act that WROTE its stamp and then sent nothing (a person's own
    // signal ended the program between its halves) takes it back: the
    // person's end is read by the other rules.
    let fresh = run(&model, &["Stamp", "EndsOtherwise", "SendsNothing", "Read"]);
    assert_eq!((fresh["ended"], fresh["read"]), (2, 2));
    // ANOTHER HAND LANDS INSIDE THE GRACE (review of 1a733ed60, ruling 11):
    // a hold, a lease, a named turn, another supervisor's claim. The act's
    // KILL waits it out — no escalation, and no carrier after a Commit,
    // while it stands — and ends the program once it is lifted: read as the
    // act's. And no act starts under one (its fences).
    let held = run(&model, &["Stamp", "Term", "HandOn"]);
    assert!(
        !model.action_enabled("Escalate", &held),
        "the act's KILL waits while another hand holds the session"
    );
    // Before any Commit, so its own guard is what disables it.
    assert!(
        !model.action_enabled("Stamp", &held),
        "no act starts under it"
    );
    let cut = run(&model, &["Stamp", "Term", "HandOn", "Commit"]);
    assert!(
        !model.action_enabled("CarrierFinishes", &cut),
        "so does the carrier's"
    );
    let lifted = run(
        &model,
        &["Stamp", "Term", "HandOn", "HandOff", "Escalate", "Read"],
    );
    assert_eq!(
        (lifted["ended"], lifted["read"], lifted["held_kill"]),
        (1, 1, 0)
    );
    assert!(
        model.action_enabled(
            "Stamp",
            &run(&model, &["Stamp", "Term", "HandOn", "HandOff"])
        ),
        "a later act starts once the hand is lifted (and joins the stamp)"
    );
    // WHOSE EXIT IT WAS IS DECIDED AT ITS END (ruling 15): a program that
    // still runs — stopped away from its tab while the hand came and went —
    // is no exit to read; and an end read under a hand is the holder's, not
    // this model's. Lifted, the act's end is read as the act's.
    assert!(
        !model.action_enabled("Read", &held),
        "nothing is decided while the act's program runs"
    );
    let heard_held = run(&model, &["Stamp", "Term", "HandOn", "Hears"]);
    assert!(
        !model.action_enabled("Read", &heard_held),
        "an end read under another hand is the holder's (the documented rule)"
    );
    let read_lifted = run(
        &model,
        &["Stamp", "Term", "HandOn", "Hears", "HandOff", "Read"],
    );
    assert_eq!((read_lifted["ended"], read_lifted["read"]), (1, 1));
    // EACH MUTANT ALONE, ON THE SAFE BUGGY BASELINE, BREAKS ITS OWN
    // INVARIANT: the counterexample's first violated invariant is named.
    for (mutant, broken) in [
        ("SignalBeforeStamp", "TheActsEndIsRelaunched"),
        ("CommitForgetsStamp", "TheActsEndIsRelaunched"),
        ("TermOnly", "TheActEndsItsProgram"),
        ("StampAfterEnd", "OnlyAStampedEndIsTheActs"),
        ("TakeBackAlways", "TheActsEndIsRelaunched"),
        ("KillUnderHand", "TheKillWaitsOutAnotherHand"),
        ("DecidedWhileRunning", "TheActsEndIsRelaunched"),
        ("WatchPerAct", "OneWatchPerProcess"),
    ] {
        let alone = verify::buggy_baseline_with(&model, mutant);
        let (state, invariant) =
            aterm_spec::interp::bmc(&alone).expect_err(&format!("{mutant} alone is caught"));
        assert_eq!(invariant, broken, "{mutant}: {state:?}");
    }
    // A person's exit with no act: read by the other rules.
    let person = run(&model, &["EndsOtherwise", "Read"]);
    assert_eq!((person["ended"], person["read"]), (2, 2));
    assert!(
        !model.action_enabled("Stamp", &person),
        "no stamp is written for a process that already ended"
    );
}

/// THE RESTART ENDS ITS PROGRAM under the named fairness of the act's own
/// steps, its escalation and the carrier's finish (strong: their watch looks
/// again while another hand holds it off), and of another hand's release —
/// never of the program's handler, a person or a Commit. NEGATIVE CONTROL:
/// `TermOnly`, the act with no escalation, leaves a deaf program running for
/// ever.
#[test]
fn the_restart_ends_its_program_under_fairness_and_term_only_does_not() {
    let model = harness_restart_stamp_model();
    let live = harness_restart_stamp_liveness();
    assert!(
        aterm_spec::xref::liveness_registry()
            .iter()
            .any(|(m, l)| m.name == model.name && l.name == live.name),
        "the restart's liveness must stay enrolled in the liveness registry"
    );
    verify::liveness_proves_and_catches_tiered(&model, &live, "HarnessRestartStamp liveness");
    let term_only = verify::buggy_baseline_with(&model, "TermOnly");
    let survivor = aterm_spec::interp::nonprogress_under(&term_only, &live)
        .expect("a deaf program outlives an act that only sends TERM");
    // The survivor stays stamped and alive for ever, its escalation dropped
    // each time an act starts one (acts may repeat; none ends it).
    let entry = survivor.entry();
    assert_eq!(
        (entry["alive"], entry["stamped"]),
        (1, 1),
        "the survivor is stamped and alive:\n{survivor}"
    );
    assert!(
        (survivor.cycle.is_empty() && entry["esc"] == 0) || survivor.moves.contains("TermOnly"),
        "nobody escalates:\n{survivor}"
    );
    assert!(
        survivor
            .cycle
            .iter()
            .all(|(_, state)| state["alive"] == 1 && state["stamped"] == 1),
        "{survivor}"
    );
}

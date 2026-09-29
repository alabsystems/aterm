// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `MainThreadStallRefusal`: a main-thread verb is refused at once
//! while the main thread is stalled, and only then. The machine proves over
//! its whole bounded space and each `Buggy = 1` defect is caught. Tier-1 (the
//! real `beat_into`, census reset, hop count, `take_hop` and `main_stall`, in
//! lockstep) is `aterm-gui`'s watchdog test
//! `main_stall_conforms_to_the_stall_refusal_model`.

use aterm_spec::{derive::main_thread_stall_refusal_model, interp, verify};

#[test]
fn main_stall_refusal_proves_and_catches_every_defect() {
    let model = main_thread_stall_refusal_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the stall refusal must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(
        &model,
        "main-thread stall refusal: refused iff the main thread is stalled",
    );
}

/// Each invariant is caught by its OWN defect: the census stamp never refuses
/// a thread that moves, and the heartbeat-only and count-until-reply rules
/// never let a stall through.
#[test]
fn each_stall_refusal_invariant_is_caught_by_its_own_defect() {
    assert!(
        verify::uncaught_invariants(&main_thread_stall_refusal_model()).is_empty(),
        "every stall-refusal invariant must be falsified by a Buggy = 1 member"
    );
}

/// Two of the schedules the defects were written for, step by step (the
/// third, a queued reply, is `a_hop_whose_reply_is_queued_…` below). A
/// `metrics reset` while the thread is stuck at a work root: the honest
/// reader still refuses, and the census stamp lets the verb through. A hop
/// posted after an idle longer than the bar: the honest reader lets it
/// through, and the heartbeat-only rule refuses it.
#[test]
fn a_reset_under_a_stall_and_a_hop_after_idleness_are_caught() {
    let model = main_thread_stall_refusal_model();
    let buggy = interp::with_buggy(&model, 1);

    let reset_under_stall = ["BeatWork", "Tick", "Reset", "Tick", "Probe"];
    let mut honest = model.init_state();
    for action in reset_under_stall {
        assert!(model.fire(action, &mut honest), "{action}: {honest:?}");
    }
    assert_eq!((honest["refused"], honest["stalled"]), (1, 1));
    let mut census = buggy.init_state();
    assert!(buggy.fire("MutateCensusStamp", &mut census));
    for action in reset_under_stall {
        assert!(buggy.fire(action, &mut census), "{action}: {census:?}");
    }
    assert_eq!((census["refused"], census["stalled"]), (0, 1));
    assert!(!buggy.check_invariant("RefusesAStalledMainThread", &census));
    assert!(buggy.check_invariant("NeverRefusesAMovingMainThread", &census));

    let fresh_hop = ["BeatIdle", "Tick", "Tick", "Tick", "Post", "Probe"];
    let mut honest = model.init_state();
    for action in fresh_hop {
        assert!(model.fire(action, &mut honest), "{action}: {honest:?}");
    }
    assert_eq!((honest["refused"], honest["stalled"]), (0, 0));
    let mut beat_only = buggy.init_state();
    assert!(buggy.fire("MutateHeartbeatOnly", &mut beat_only));
    for action in fresh_hop {
        assert!(
            buggy.fire(action, &mut beat_only),
            "{action}: {beat_only:?}"
        );
    }
    assert_eq!((beat_only["refused"], beat_only["stalled"]), (1, 0));
    assert!(!buggy.check_invariant("NeverRefusesAMovingMainThread", &beat_only));
    assert!(buggy.check_invariant("RefusesAStalledMainThread", &beat_only));
}

/// A dialog is a designed freeze: however long a hop waits behind it, this
/// refusal never speaks for it (the dialog has its own), and no hop is
/// answered or taken while it stands.
#[test]
fn a_dialog_is_never_read_as_a_stall() {
    let model = main_thread_stall_refusal_model();
    let mut s = model.init_state();
    for action in ["BeatWork", "BeatPark", "Post", "Tick", "Tick", "Probe"] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["refused"], s["stalled"]), (0, 0));
    assert!(!model.action_enabled("Answer", &s));
    assert!(!model.action_enabled("Take", &s));
}

/// The defect a `settings set` exposed. The main thread takes the hop and
/// queues its write; the reply comes turns later, from the config worker, and
/// meanwhile the thread parks idle again. The honest reader counted the hop
/// out when the main thread took it and lets the next verb through. Counted
/// until its reply instead, the hop makes that healthy, idle thread read as
/// stuck once the write has taken the bar.
#[test]
fn a_hop_whose_reply_is_queued_stops_counting_when_it_is_taken() {
    let model = main_thread_stall_refusal_model();
    let buggy = interp::with_buggy(&model, 1);
    let queued_reply = [
        "BeatIdle", "Post", "Take", "BeatIdle", "Tick", "Tick", "Probe",
    ];

    let mut honest = model.init_state();
    for action in queued_reply {
        assert!(model.fire(action, &mut honest), "{action}: {honest:?}");
    }
    assert_eq!((honest["hop"], honest["waiting"]), (0, 1));
    assert_eq!((honest["refused"], honest["stalled"]), (0, 0));
    // The queued reply comes, from a turn of the main thread.
    assert!(model.fire("Reply", &mut honest), "{honest:?}");
    assert_eq!(honest["waiting"], 0);

    let mut until_reply = buggy.init_state();
    assert!(buggy.fire("MutateCountsUntilReply", &mut until_reply));
    for action in queued_reply {
        assert!(
            buggy.fire(action, &mut until_reply),
            "{action}: {until_reply:?}"
        );
    }
    assert_eq!((until_reply["refused"], until_reply["stalled"]), (1, 0));
    assert!(!buggy.check_invariant("NeverRefusesAMovingMainThread", &until_reply));
    assert!(buggy.check_invariant("RefusesAStalledMainThread", &until_reply));

    // A thread stuck in the handler that took the hop is at a work root: it
    // is still refused, reply queued or not.
    let mut stuck = model.init_state();
    for action in ["BeatIdle", "Post", "Take", "Tick", "Tick", "Probe"] {
        assert!(model.fire(action, &mut stuck), "{action}: {stuck:?}");
    }
    assert_eq!((stuck["refused"], stuck["stalled"]), (1, 1));
}

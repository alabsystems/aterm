// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `SubscribeAnnouncementOrder`: an `@*` subscription tells its
//! reader a session is watched only once the watch exists, and the fabric
//! bridge stops reading a session's opt-ins only on the evidence of a watch.
//! Tier-1 is `aterm-gui` `subscribe.rs`'s
//! `membership_passes_refine_the_announcement_model` and `aterm-link`
//! `bridge.rs`'s `bridge_rounds_refine_the_announcement_model`.

use aterm_spec::{derive::subscribe_announcement_order_model, interp, verify};

const LAW: &str = "NotAnnouncedBeforeItsWatchOrExit";
const STOPPED: &str = "StoppedOnlyWhileWatched";
const NO_LOSS: &str = "NoTopicAddLostSilently";

/// Fire `actions` in order from `state`, each one required to be enabled.
fn run(model: &aterm_spec::derive::Model, state: &mut interp::State, actions: &[&str]) {
    for action in actions {
        assert!(model.fire(action, state), "{action}: {state:?}");
    }
}

/// Every law holds at `state`.
fn lawful(model: &aterm_spec::derive::Model, state: &interp::State) -> bool {
    [LAW, STOPPED, NO_LOSS]
        .iter()
        .all(|law| model.check_invariant(law, state))
}

#[test]
fn a_session_is_never_announced_before_its_watch_or_its_exit() {
    let model = subscribe_announcement_order_model();
    verify::prove_and_catch_scalar(&model, "subscribe announcement order");
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(0),
        "every action must fire at Buggy=0"
    );

    // A registration after the pass's one read is in neither decision: nothing is said, and the
    // next pass adopts and announces it after the seed.
    let mut late = model.init_state();
    run(
        &model,
        &mut late,
        &["FreeSlot", "Read", "Register", "Drain", "Seed", "Write"],
    );
    assert_eq!((late["told"], late["drained"], late["watched"]), (0, 0, 0));
    run(&model, &mut late, &["Read", "Drain", "Seed", "Write"]);
    assert_eq!((late["told"], late["watched"], late["ackwire"]), (1, 1, 1));
    assert!(model.check_invariant(LAW, &late));

    // At the cap the session is announced at once as `watch=deferred`; a slot that frees later
    // adopts it with an ack and no second announcement.
    let mut deferred = model.init_state();
    run(
        &model,
        &mut deferred,
        &["Register", "Read", "Drain", "Seed", "Write"],
    );
    assert_eq!(
        (deferred["told"], deferred["watched"], deferred["ackwire"]),
        (2, 0, 0)
    );
    run(
        &model,
        &mut deferred,
        &["FreeSlot", "Read", "Drain", "Seed", "Write"],
    );
    assert_eq!(
        (deferred["told"], deferred["watched"], deferred["ackwire"]),
        (2, 1, 1)
    );

    // A session gone before the read is announced plainly, unwatched: its exit
    // is in the same batch, so the pair goes out together.
    let mut pair = model.init_state();
    run(
        &model,
        &mut pair,
        &["Register", "Exit", "Read", "Drain", "Seed", "Write"],
    );
    assert_eq!((pair["told"], pair["watched"], pair["live"]), (1, 0, 0));
    assert!(model.check_invariant(LAW, &pair));

    // Bridge: a session with no ack on record is read every round, so an add made after one
    // successful read is learned by the next.
    let mut unacked = model.init_state();
    run(
        &model,
        &mut unacked,
        &["Register", "Roster", "Ask", "Answer", "Add"],
    );
    assert_eq!(
        (unacked["stopped"], unacked["pushed"], unacked["known"]),
        (0, 0, 0)
    );
    assert!(lawful(&model, &unacked));
    run(&model, &mut unacked, &["Ask", "Answer"]);
    assert_eq!(unacked["known"], 1, "the next round's read learned it");

    // An add in the window between an early read and the seed; the ack's read fails and stays
    // owed until one succeeds, after which the bridge stops reading S and its changes arrive
    // pushed.
    let mut window = model.init_state();
    run(
        &model,
        &mut window,
        &[
            "Register", "Roster", "Ask", "Answer", "Add", "FreeSlot", "Read", "Drain", "Seed",
            "Write", "HearAck", "Ask", "Fail",
        ],
    );
    assert_eq!(
        (window["acked"], window["stopped"], window["known"]),
        (1, 0, 0)
    );
    assert!(lawful(&model, &window));
    run(&model, &mut window, &["Ask", "Answer"]);
    assert_eq!((window["stopped"], window["known"]), (1, 1));

    // An ack that never reaches the bridge leaves S read every round: the
    // bridge stops only on an ack it holds.
    let mut lost = model.init_state();
    run(
        &model,
        &mut lost,
        &[
            "FreeSlot", "Register", "Read", "Drain", "Seed", "Write", "DropAck", "Roster", "Ask",
            "Answer",
        ],
    );
    assert_eq!((lost["watched"], lost["acked"], lost["stopped"]), (1, 0, 0));

    // An outage keeps the ack's bookkeeping and owes every session a read.
    let mut outage = model.init_state();
    run(
        &model,
        &mut outage,
        &[
            "FreeSlot", "Register", "Read", "Drain", "Seed", "Write", "Add", "Outage",
        ],
    );
    assert_eq!(
        (outage["pushed"], outage["acked"], outage["stopped"]),
        (0, 1, 0)
    );
    assert!(lawful(&model, &outage));
    run(&model, &mut outage, &["Roster", "Ask", "Answer"]);
    assert_eq!((outage["known"], outage["stopped"]), (1, 1));

    // Buggy=1, two-read pass: the registration lands between adoption's read and the roster's,
    // so the session is announced as watched with no watch and no exit.
    let buggy = interp::with_buggy(&model, 1);
    let mut split = buggy.init_state();
    run(
        &buggy,
        &mut split,
        &["FreeSlot", "Read", "Register", "Drain", "Seed", "Write"],
    );
    assert_eq!((split["told"], split["watched"], split["live"]), (1, 0, 1));
    assert!(!buggy.check_invariant(LAW, &split));
    // ...and its cap twin: a deferred session announced as if it were watched.
    let mut capped = buggy.init_state();
    run(
        &buggy,
        &mut capped,
        &["Register", "Read", "Drain", "Seed", "Write"],
    );
    assert_eq!(
        (capped["told"], capped["watched"], capped["live"]),
        (1, 0, 1)
    );
    assert!(!buggy.check_invariant(LAW, &capped));

    // Buggy=1, read-once bridge: S is read once, before any watch exists, so the add that
    // follows is pushed by nobody.
    let mut once = buggy.init_state();
    run(
        &buggy,
        &mut once,
        &["Register", "Roster", "Ask", "Answer", "Add"],
    );
    assert_eq!((once["stopped"], once["watched"], once["known"]), (1, 0, 0));
    assert!(!buggy.check_invariant(STOPPED, &once));
    assert!(!buggy.check_invariant(NO_LOSS, &once));
    // ...and a read the endpoint refused, recorded as done.
    let mut refused = buggy.init_state();
    run(
        &buggy,
        &mut refused,
        &["Register", "Add", "Roster", "Ask", "Fail"],
    );
    assert_eq!((refused["stopped"], refused["known"]), (1, 0));
    assert!(!buggy.check_invariant(NO_LOSS, &refused));
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NetSlotDisplacement`: the network drive's listener keeps at most `Cap`
//! connections in flight, claims each connection's unauthenticated phase exactly once
//! (handshake, deadline or eviction), and never force-closes one its handshake claimed.

use aterm_spec::derive::net_slot_displacement_model;
use aterm_spec::interp::with_buggy;

const INVARIANTS: [&str; 3] = ["InFlightBounded", "ClaimExactlyOnce", "AuthedNeverCut"];

#[test]
fn net_slot_displacement_proves_and_catches_the_cut_relay() {
    let model = net_slot_displacement_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, "net slot displacement");
    let uncaught = aterm_spec::verify::uncaught_invariants(&model);
    assert!(uncaught.is_empty(), "ghost invariants: {uncaught:?}");
}

/// Each mutant, alone, breaks the invariants it is named for, and only those.
#[test]
fn each_mutant_breaks_its_own_invariant() {
    let buggy = with_buggy(&net_slot_displacement_model(), 1);
    for (path, broken) in [
        // The newcomer takes the evicted connection's slot before it is freed.
        (
            &["Accept", "Accept", "Evict", "Accept"][..],
            &["InFlightBounded"][..],
        ),
        // The handshake stores AUTHED over the deadline's claim.
        (
            &["Accept", "Deadline", "Authenticate"][..],
            &["ClaimExactlyOnce"][..],
        ),
        // The watchdog closes a socket its handshake already claimed.
        (
            &["Accept", "Authenticate", "Deadline"][..],
            &["AuthedNeverCut"][..],
        ),
        // The listener stores EVICTED over a relay at capacity.
        (
            &["Accept", "Accept", "Authenticate", "Authenticate", "Evict"][..],
            &["ClaimExactlyOnce", "AuthedNeverCut"][..],
        ),
    ] {
        let mut state = buggy.init_state();
        for action in path {
            assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
        }
        for invariant in INVARIANTS {
            assert_eq!(
                buggy.check_invariant(invariant, &state),
                !broken.contains(&invariant),
                "{path:?} and {invariant}: {state:?}"
            );
        }
    }
}

/// The committed machine: at capacity a pending connection is evicted, the
/// newcomer waits for its slot (or is refused), and nothing evicts a relay.
#[test]
fn a_newcomer_waits_for_the_slot_it_freed_and_never_takes_a_relays() {
    let model = net_slot_displacement_model();
    let mut full = model.init_state();
    for action in ["Accept", "Accept", "Authenticate"] {
        assert!(model.fire(action, &mut full), "{action}: {full:?}");
    }
    assert!(!model.action_enabled("Accept", &full), "every slot is held");
    let mut evicted = full.clone();
    assert!(model.fire("Evict", &mut evicted));
    assert!(
        !model.action_enabled("Accept", &evicted),
        "the newcomer waits for the displaced connection to let go"
    );
    assert!(
        !model.action_enabled("Evict", &evicted),
        "one eviction at a time"
    );
    let mut refused = evicted.clone();
    assert!(model.fire("Refuse", &mut refused));
    assert!(model.fire("Release", &mut evicted));
    assert!(model.fire("Accept", &mut evicted));
    assert_eq!(evicted["waiting"], 0);

    // Every slot a relay: nothing to evict.
    let mut relays = model.init_state();
    for action in ["Accept", "Accept", "Authenticate", "Authenticate"] {
        assert!(model.fire(action, &mut relays), "{action}: {relays:?}");
    }
    assert!(!model.action_enabled("Evict", &relays));
    assert!(!model.action_enabled("Deadline", &relays));
}

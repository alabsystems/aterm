// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateSpeculativeSidecar`, the outgoing process's artifact diet
//! (docs/DESIGN-warm-successor-2026-09-29.md §5): a layout sidecar written before the park
//! reaches the successor only once the park read it back byte-equal to its own layout; a moved
//! desk or a disturbed file is discarded and rewritten; a refused speculation costs only the
//! park's old write; every retired attempt sweeps what it wrote; every attempt is granted or
//! retired.

use aterm_spec::{
    derive::{native_update_speculative_sidecar_liveness, native_update_speculative_sidecar_model},
    interp, verify,
};

/// Each invariant mutant with the invariant it was written for.
const MUTANTS: [(&str, &str); 5] = [
    ("SealUnverified", "SpeculationVerifiedBeforeSeal"),
    ("VerifyTheMemoryCopy", "SealHoldsTheParksBytes"),
    ("GrantBeforeSeal", "SuccessorReadsTheParksBytes"),
    (
        "RetireKeepsSpeculation",
        "NothingSpeculativeOutlivesTheAttempt",
    ),
    // Also the liveness property's mutant (below).
    ("AwaitSpeculation", "ParkNeverWaitsOnSpeculation"),
];

/// The model at `Buggy = 1` with only `mutant`'s action live among the mutants.
fn only(mutant: &str) -> aterm_spec::derive::Model {
    let mut model = interp::with_buggy(&native_update_speculative_sidecar_model(), 1);
    model.actions.retain(|action| {
        action.name == mutant || !MUTANTS.iter().any(|(name, _)| *name == action.name)
    });
    model
}

/// Each invariant is falsified by one mutant alone, and with no mutant live the `Buggy = 1`
/// model is clean, so each catch is the mutant's.
#[test]
fn each_mutant_alone_breaks_the_invariant_it_was_written_for() {
    for (mutant, invariant) in MUTANTS {
        match interp::bmc(&only(mutant)) {
            Err((_, violated)) => assert_eq!(
                violated, invariant,
                "{mutant} alone breaks {invariant}, not {violated}"
            ),
            Ok(states) => panic!("{mutant} alone broke nothing over {states} states"),
        }
    }
    assert!(interp::bmc(&only("")).is_ok());
    // A stale speculation kept unverified also seals the wrong bytes; the shortest counterexample
    // above keeps an unmoved desk, so check `SealHoldsTheParksBytes` on its own.
    let mut stale = only("SealUnverified");
    stale
        .invariants
        .retain(|invariant| invariant.name == "SealHoldsTheParksBytes");
    assert!(
        interp::bmc(&stale).is_err(),
        "keeping a speculation of a desk that moved seals bytes the park never wrote"
    );
}

/// The three ways a park seals the sidecar (kept, discarded and rewritten, written at the park)
/// each end granted with the successor reading the park's bytes.
#[test]
fn the_park_keeps_rewrites_or_writes_and_the_successor_reads_its_bytes() {
    let model = native_update_speculative_sidecar_model();
    let run = |schedule: &[&str], seal: &str| {
        let mut state = model.init_state();
        for action in schedule {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        for other in ["KeepVerified", "DiscardAndRewrite", "WriteAtPark"] {
            assert_eq!(
                model.action_enabled(other, &state),
                other == seal,
                "only {seal} seals after {schedule:?}: {state:?}"
            );
        }
        assert!(model.fire(seal, &mut state), "{seal}: {state:?}");
        let sealed = state.clone();
        for action in ["Grant", "SuccessorReads"] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        assert_eq!(state["read"], 1, "{schedule:?} then {seal}: {state:?}");
        assert_eq!(
            state["disk"], 0,
            "the successor consumed the sidecar: {state:?}"
        );
        sealed
    };
    let kept = run(&["Speculate", "Park"], "KeepVerified");
    assert_eq!((kept["disk"], kept["verified"]), (1, 1));
    let moved = run(&["Speculate", "LayoutMoves", "Park"], "DiscardAndRewrite");
    assert_eq!(moved["disk"], 2);
    let disturbed = run(&["Speculate", "DiskDisturbed", "Park"], "DiscardAndRewrite");
    assert_eq!(disturbed["disk"], 2);
    let refused = run(&["RefuseWrite", "Speculate", "Park"], "WriteAtPark");
    assert_eq!((refused["spec"], refused["disk"]), (0, 2));

    // A retire from any held phase leaves nothing at the name.
    for schedule in [
        &["Speculate"][..],
        &["Speculate", "Park"],
        &["Speculate", "Park", "KeepVerified"],
        &["Speculate", "Park", "KeepVerified", "Grant"],
    ] {
        let mut state = model.init_state();
        for action in schedule {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        assert!(model.fire("Retire", &mut state), "{schedule:?}: {state:?}");
        assert_eq!(state["disk"], 0, "{schedule:?} then Retire: {state:?}");
    }
}

/// Liveness: every attempt is granted or retired under the stated fairness; a park that waits on
/// a refused speculation is the caught mutant.
#[test]
fn every_attempt_is_granted_or_retired() {
    let model = native_update_speculative_sidecar_model();
    let live = native_update_speculative_sidecar_liveness();
    verify::liveness_proves_and_catches_tiered(&model, &live, "NativeUpdateSpeculativeSidecar");
}

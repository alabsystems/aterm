// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateSuccessorWarmBeforeClaim`
//! (docs/DESIGN-warm-successor-2026-09-29.md): before its claim a successor mutates no
//! environment once a thread exists and owns, shows and proves nothing; the warm prologue runs
//! before the dial except on the relative-socket shape; every successor proves or exits.

use aterm_spec::{
    derive::{
        native_update_successor_warm_before_claim_liveness,
        native_update_successor_warm_before_claim_model,
    },
    interp, verify,
};

/// Each invariant mutant with the invariant it was written for.
const MUTANTS: [(&str, &str); 9] = [
    ("PublishToEnv", "NoEnvMutationAfterSpawn"),
    ("BindWhileWarming", "NothingOwnedBeforeClaim"),
    ("RevealPlaceholder", "NeverRevealedBeforeClaim"),
    ("WarmProves", "WarmPresentNeverProves"),
    ("FoldHint", "HintNotInProof"),
    ("WarmBeforeDialOnRelativeSocket", "NoCwdChangeAfterSpawn"),
    ("PresentAtHintedSize", "CarriedPresentAtCarriedSize"),
    (
        "PresentUnderStaleConfig",
        "CarriedPresentUnderCurrentConfig",
    ),
    // Also the liveness property's mutant (below).
    ("JoinWorkerBeforeDial", "PrologueJoinsNoThread"),
];

/// The model at `Buggy = 1` with only `mutant`'s action live among the mutants.
fn only(mutant: &str) -> aterm_spec::derive::Model {
    let mut model = interp::with_buggy(&native_update_successor_warm_before_claim_model(), 1);
    model.actions.retain(|action| {
        action.name == mutant || !MUTANTS.iter().any(|(name, _)| *name == action.name)
    });
    model
}

/// Each invariant is falsified by one mutant alone, not carried by another's counterexample.
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
    // FailedClaimLeavesNothing: a bind while warming, then a failed claim.
    let mut bind = only("BindWhileWarming");
    bind.invariants
        .retain(|invariant| invariant.name == "FailedClaimLeavesNothing");
    assert!(
        interp::bmc(&bind).is_err(),
        "a bind while warming, then a failed claim, leaves something owned at exit"
    );
    // With no mutant live the Buggy = 1 model is clean, so the catches are the mutants'.
    assert!(interp::bmc(&only("")).is_ok());
}

/// The launched lane warms before it dials; the relative-socket shape warms after the intake; a
/// stale hint or a config edit during the hold blocks the carried present until re-warmed.
#[test]
fn the_warm_precedes_the_dial_except_where_the_intake_changes_directory() {
    let model = native_update_successor_warm_before_claim_model();

    let mut launched = model.init_state();
    assert!(model.fire("Capture", &mut launched));
    assert!(
        !model.action_enabled("Dial", &launched),
        "no dial before the prologue: {launched:?}"
    );
    for action in [
        "SpawnWarm",
        "WarmDone",
        "Dial",
        "ClaimOk",
        "TakeOwned",
        "Adopt",
    ] {
        assert!(model.fire(action, &mut launched), "{action}: {launched:?}");
    }
    assert!(model.fire("RealPresent", &mut launched));
    assert!(model.fire("Prove", &mut launched));

    let mut relative = model.init_state();
    for action in ["RelativeSocket", "Capture"] {
        assert!(model.fire(action, &mut relative), "{action}: {relative:?}");
    }
    assert!(
        !model.action_enabled("SpawnWarm", &relative),
        "the relative shape never warms before the dial: {relative:?}"
    );
    for action in ["Dial", "ClaimOk", "TakeOwned", "Adopt"] {
        assert!(model.fire(action, &mut relative), "{action}: {relative:?}");
    }
    assert!(
        !model.action_enabled("SpawnLate", &relative),
        "{relative:?}"
    );
    assert!(
        !model.action_enabled("RealPresent", &relative),
        "{relative:?}"
    );
    for action in ["PrepareSocketDir", "SpawnLate", "RealPresent", "Prove"] {
        assert!(model.fire(action, &mut relative), "{action}: {relative:?}");
    }
    assert!(model.check_invariant("NoCwdChangeAfterSpawn", &relative));

    let mut stale = model.init_state();
    for action in [
        "Capture",
        "SpawnWarm",
        "StaleHint",
        "WarmDone",
        "Dial",
        "ClaimOk",
        "TakeOwned",
        "Adopt",
    ] {
        assert!(model.fire(action, &mut stale), "{action}: {stale:?}");
    }
    assert!(
        !model.action_enabled("RealPresent", &stale),
        "no carried present at a stale hint's size: {stale:?}"
    );
    assert!(model.fire("Reshape", &mut stale));
    assert!(model.fire("RealPresent", &mut stale));

    let mut edited = model.init_state();
    for action in [
        "Capture",
        "SpawnWarm",
        "WarmDone",
        "Dial",
        "ConfigEdit",
        "ClaimOk",
        "TakeOwned",
        "Adopt",
    ] {
        assert!(model.fire(action, &mut edited), "{action}: {edited:?}");
    }
    assert!(
        !model.action_enabled("RealPresent", &edited),
        "no carried present under the config the early warm read: {edited:?}"
    );
    assert!(model.fire("Rewarm", &mut edited));
    assert!(model.fire("RealPresent", &mut edited));
}

/// Liveness: every successor proves or exits under the stated fairness; joining the worker before
/// the dial is the caught mutant.
#[test]
fn every_warm_successor_proves_or_exits() {
    let model = native_update_successor_warm_before_claim_model();
    let live = native_update_successor_warm_before_claim_liveness();
    verify::liveness_proves_and_catches_tiered(
        &model,
        &live,
        "NativeUpdateSuccessorWarmBeforeClaim",
    );
}

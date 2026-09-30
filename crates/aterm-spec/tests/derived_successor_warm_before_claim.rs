// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateSuccessorWarmBeforeClaim`
//! (docs/DESIGN-warm-successor-2026-09-29.md): before its claim a successor
//! mutates no environment once a thread exists, and owns, shows and proves
//! nothing — and each mutant (the post-claim `publish()` into `environ`, a
//! pre-claim bind, a revealed placeholder, a proving warm present, a folded
//! hint) is caught. P2's order: the warm prologue runs before the dial except
//! on the relative-socket shape, a stale hint is re-selected before any
//! carried present, and every successor proves or exits.

use aterm_spec::{
    derive::{
        native_update_successor_warm_before_claim_liveness,
        native_update_successor_warm_before_claim_model,
    },
    interp, verify,
};

#[test]
fn successor_warm_before_claim_proves_and_catches() {
    verify::prove_and_catch_scalar(
        &native_update_successor_warm_before_claim_model(),
        "successor warm before claim",
    );
}

/// Each invariant mutant with the invariant it was written for.
const MUTANTS: [(&str, &str); 8] = [
    ("PublishToEnv", "NoEnvMutationAfterSpawn"),
    ("BindWhileWarming", "NothingOwnedBeforeClaim"),
    ("RevealPlaceholder", "NeverRevealedBeforeClaim"),
    ("WarmProves", "WarmPresentNeverProves"),
    ("FoldHint", "HintNotInProof"),
    ("WarmBeforeDialOnRelativeSocket", "NoCwdChangeAfterSpawn"),
    ("PresentAtHintedSize", "CarriedPresentAtCarriedSize"),
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

/// Every invariant is falsified by one mutant ALONE, so none of the nine is
/// carried by another's counterexample.
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
    // And with no mutant live the Buggy = 1 model is clean: the catches above
    // are the mutants', not damage the dial does elsewhere.
    assert!(interp::bmc(&only("")).is_ok());
}

/// P2's order, walked: the launched lane warms before it dials; the relative
/// socket shape cannot, and warms after the intake; a stale hint blocks the
/// carried present until the join re-selects the carried size.
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
}

/// Liveness: every successor proves or exits, under the fairness stated on
/// `native_update_successor_warm_before_claim_liveness`, and a prologue that
/// joins its worker before the dial is the caught mutant — interpreter and,
/// where installed, `ty`.
#[test]
fn every_warm_successor_proves_or_exits() {
    let model = native_update_successor_warm_before_claim_model();
    let live = native_update_successor_warm_before_claim_liveness();
    assert!(
        aterm_spec::xref::liveness_registry()
            .iter()
            .any(|(registered, obligation)| registered.name == model.name
                && obligation.name == live.name),
        "the warm successor's liveness must stay enrolled"
    );
    verify::liveness_proves_and_catches_tiered(
        &model,
        &live,
        "NativeUpdateSuccessorWarmBeforeClaim",
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateSuccessorWarmBeforeClaim`
//! (docs/DESIGN-warm-successor-2026-09-29.md): before its claim a successor
//! mutates no environment once a thread exists, and owns, shows and proves
//! nothing — and each mutant (the post-claim `publish()` into `environ`, a
//! pre-claim bind, a revealed placeholder, a proving warm present, a folded
//! hint) is caught.

use aterm_spec::interp;

#[test]
fn successor_warm_before_claim_proves_and_catches() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::native_update_successor_warm_before_claim_model(),
        "successor warm before claim",
    );
}

const MUTANTS: [(&str, &str); 5] = [
    ("PublishToEnv", "NoEnvMutationAfterSpawn"),
    ("BindWhileWarming", "NothingOwnedBeforeClaim"),
    ("RevealPlaceholder", "NeverRevealedBeforeClaim"),
    ("WarmProves", "WarmPresentNeverProves"),
    ("FoldHint", "HintNotInProof"),
];

/// The model at `Buggy = 1` with only `mutant`'s action live among the mutants.
fn only(mutant: &str) -> aterm_spec::derive::Model {
    let mut model = interp::with_buggy(
        &aterm_spec::derive::native_update_successor_warm_before_claim_model(),
        1,
    );
    model.actions.retain(|action| {
        action.name == mutant || !MUTANTS.iter().any(|(name, _)| *name == action.name)
    });
    model
}

/// Every invariant is falsified by one mutant ALONE, so none of the six is
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

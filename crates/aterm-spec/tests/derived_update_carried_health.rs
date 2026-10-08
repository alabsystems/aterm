// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateCarriedHealth` (round-seven update audit, findings
//! 206 and 207): a warning carried across a self-update is handed to a producer
//! here that will heal it, its digest line is owed exactly while it stands, and
//! the ledger's healing never ends the stall.

use aterm_spec::{derive::native_update_carried_health_model, interp, verify};

#[test]
fn carried_health_proves_and_catches() {
    verify::prove_and_catch_scalar(&native_update_carried_health_model(), "carried health");
}

/// Each mutant with the invariant it was written for.
const MUTANTS: [(&str, &str); 5] = [
    ("SeedRemembersOnly", "WarningHasHealer"),
    ("SeedStallRemembersOnly", "StallHasHealer"),
    ("RecoverHealsStall", "StallStandsUntilACheckHere"),
    ("SeedForgetsEntry", "CheckLineFollowsWarning"),
    ("StallHealKeepsEntry", "StallLineFollowsWarning"),
];

/// The model at `Buggy = 1` with only `mutant`'s action live among the mutants.
fn only(mutant: &str) -> aterm_spec::derive::Model {
    let mut model = interp::with_buggy(&native_update_carried_health_model(), 1);
    model.actions.retain(|action| {
        action.name == mutant || !MUTANTS.iter().any(|(name, _)| *name == action.name)
    });
    model
}

/// Every invariant is falsified by one mutant ALONE, and with no mutant live
/// the `Buggy = 1` model is clean, so each catch is the mutant's.
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
}

/// The successor's life, walked: the carried check heals on the recovery it is
/// owed and a later episode is said again; the stall stands through the
/// ledger's healing and heals on this process's first completed check.
#[test]
fn a_carried_warning_heals_on_its_own_proof_and_the_next_episode_speaks() {
    let model = native_update_carried_health_model();
    let mut state = model.init_state();
    for action in [
        "Seed",
        "LedgerHeals",
        "Recover",
        "LedgerFails",
        "Announce",
        "CheckCompletes",
        "StallHeals",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &state),
                "{} after {action}",
                invariant.name
            );
        }
        if action == "Recover" {
            assert_eq!(state["stall"], 1, "the ledger's healing leaves the stall");
        }
    }
    assert_eq!(
        (state["up"], state["entry"]),
        (1, 1),
        "the new episode is said"
    );
    assert_eq!((state["stall"], state["sentry"]), (0, 0));
}

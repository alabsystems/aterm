// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateRosterFreshness`: no lane swaps a stage whose roster authorization was
//! last confirmed a window or more ago, and a check confirms only what the roster it read still
//! trusts, so a revocation published a window before a swap is never missed.

use aterm_spec::{derive::native_update_roster_freshness_model, interp};

/// Each mutant with the invariant it was written for.
const MUTANTS: [(&str, &str); 2] = [
    ("SwapOnTheFloorAlone", "NoSwapPastTheWindow"),
    ("ConfirmWithoutTheRoster", "NoSwapPastTheWindow"),
];

/// The model at `Buggy = 1` with only `mutant`'s action live among the mutants.
fn only(mutant: &str) -> aterm_spec::derive::Model {
    let mut model = interp::with_buggy(&native_update_roster_freshness_model(), 1);
    model.actions.retain(|action| {
        action.name == mutant || !MUTANTS.iter().any(|(name, _)| *name == action.name)
    });
    model
}

/// Each mutant alone breaks its invariant, and with no mutant live the `Buggy = 1` model is
/// clean, so each catch is the mutant's.
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

/// Staged and confirmed at 0, the signer revoked at tick 1, the next launch a window after the
/// confirmation: its swap is not enabled, and the check that runs next retires the stage.
#[test]
fn a_stale_stage_waits_for_the_check_which_retires_or_confirms_it() {
    let model = native_update_roster_freshness_model();
    let mut state = model.init_state();
    for action in ["Tick", "Revoke", "Tick"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(
        !model.action_enabled("Swap", &state),
        "a window after its confirmation, the launch does not swap: {state:?}"
    );
    let mut trusted = state.clone();
    assert!(model.fire("Check", &mut state));
    assert_eq!(state["staged"], 0, "the revoking roster retires it");
    assert!(!model.action_enabled("Swap", &state));

    // With no revocation the check confirms and the swap goes on.
    trusted.insert("revoked", 0);
    trusted.insert("rev_at", 0);
    assert!(!model.action_enabled("Swap", &trusted));
    assert!(model.fire("Check", &mut trusted));
    assert_eq!((trusted["staged"], trusted["conf"]), (1, 2));
    assert!(model.fire("Swap", &mut trusted));
    assert_eq!(trusted["late"], 0);
}

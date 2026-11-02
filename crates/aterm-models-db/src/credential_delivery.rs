// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Connection-bound custody proofs and sealed credential delivery. Membership, policy,
//! source reads and vault publication are separate runtime checks, exercised on fixture vaults.

use aterm_spec::derive::Model;

/// Only the live transcript, its pinned custodian and its exact binding recipient open a
/// delivery. The implementation projection runs the actual signatures and age codec.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn credential_delivery_model() -> Model {
    aterm_spec::ty_model! {
        DbCredentialDelivery {
            const Buggy = 0;
            var scenario = 0; var accepted = 0;
            action Valid when (scenario == 0) { scenario = 1; accepted = 1; }
            action NonCustodian when (scenario == 0) { scenario = 2; accepted = if Buggy == 1 {1} else {0}; }
            action OtherRecipient when (scenario == 0) { scenario = 3; accepted = if Buggy == 2 {1} else {0}; }
            action ReplayNonce when (scenario == 0) { scenario = 4; accepted = if Buggy == 3 {1} else {0}; }
            action OtherCustodian when (scenario == 0) { scenario = 5; accepted = if Buggy == 4 {1} else {0}; }
            action OtherOffer when (scenario == 0) { scenario = 6; accepted = if Buggy == 5 {1} else {0}; }
            action Expired when (scenario == 0) { scenario = 7; accepted = if Buggy == 6 {1} else {0}; }
            action WrongWriter when (scenario == 0) { scenario = 8; accepted = if Buggy == 7 {1} else {0}; }
            invariant OnlyBoundDelivery: accepted == 0 || scenario == 1;
            bound ScenarioRange: scenario <= 8;
            bound AcceptedRange: accepted <= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_delivery_guard_has_a_failing_mutant() {
        aterm_spec::interp::prove_and_catch(&super::credential_delivery_model());
    }
}

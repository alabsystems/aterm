// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded handshake fault scenarios. The implementation test changes policy
/// between the actual challenge and response; preflight itself grants nothing.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn enrollment_gate_model() -> Model {
    aterm_spec::ty_model! {
        DbEnrollmentGate {
            const Buggy = 0;
            var scenario = 0; var challenged = 0; var granted = 0; var retained = 0;
            action Eligible when (scenario == 0) {
                scenario = 1; challenged = 1; granted = if Buggy == 5 {0} else {1};
                retained = if Buggy == 4 {0} else {1};
            }
            action NoEvidence when (scenario == 0) {
                scenario = 2; challenged = if Buggy == 1 {1} else {0};
            }
            action ExcludedBefore when (scenario == 0) {scenario = 3;}
            action RetiredBefore when (scenario == 0) {scenario = 4;}
            action ForkBefore when (scenario == 0) {scenario = 5;}
            action ExcludedDuring when (scenario == 0) {
                scenario = 6; challenged = 1; granted = if Buggy == 2 {1} else {0};
            }
            action RetiredDuring when (scenario == 0) {scenario = 7; challenged = 1;}
            action ForkDuring when (scenario == 0) {scenario = 8; challenged = 1;}
            action InvalidProof when (scenario == 0) {
                scenario = 9; challenged = 1; retained = if Buggy == 3 {1} else {0};
            }
            invariant QualifiedChallenge: 2 > scenario || scenario > 5 || challenged == 0;
            invariant NoGrantAfterRefusal: 2 > scenario || granted == 0;
            invariant NoWriteAfterRefusal: 2 > scenario || retained == 0;
            invariant RetainBeforeWitness: granted == 0 || retained == 1;
            invariant EligibleProgress: 1 > scenario || scenario > 1 || (challenged == 1 && granted == 1);
        }
    }
}

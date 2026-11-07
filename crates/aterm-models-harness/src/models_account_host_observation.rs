// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Live issuer identity and durable history are independent observation premises.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_host_observation_model() -> Model {
    aterm_spec::ty_model! {
        AccountHostObservation {
            const Buggy = 0;
            var kind = 0; var case = 0; var checked = 0; var accepted = 0;
            action UseClaim when (checked == 0 && kind == 0 && case <= 14) { kind = 1; }
            action NextCase when (checked == 0 && (case <= 13 || (kind == 0 && case <= 15))) { case = case + 1; }
            action Check when (checked == 0) {
                checked = 1;
                accepted = if (Buggy == 1 && case == 1) || (Buggy == 2 && case == 2)
                    || (Buggy == 3 && case == 3) || (Buggy == 4 && case == 4)
                    || (Buggy == 5 && case == 5) || (Buggy == 6 && case == 6)
                    || (Buggy == 7 && case == 7) {1}
                    else {if Buggy == 8 && case == 12 {1}
                    else {if Buggy == 9 && kind == 0 && case == 9 {1}
                    else {if Buggy == 10 && kind == 0 && case == 8 {1}
                    else {if (Buggy == 20 && case == 15) || (Buggy == 21 && case == 16) {1}
                    else {if (Buggy == 15 && kind == 0 && case == 13) || (Buggy == 16 && kind == 0 && case == 14) {1}
                    else {if (Buggy == 11 && case == 10) || (Buggy == 12 && case == 11)
                        || (Buggy == 13 && case == 13) || (Buggy == 14 && case == 14)
                        || (Buggy == 17 && kind == 1 && case == 8) || (Buggy == 18 && kind == 1 && case == 9)
                        || (Buggy == 19 && case == 0) {0}
                    else {if case == 0 || case == 10 || case == 11
                        || (kind == 1 && (case == 8 || case == 9 || case == 13 || case == 14)) {1} else {0}}}}}}}};
            }
            invariant ExactParent: accepted == 0 || case <= 0 || case > 1;
            invariant ExactBinding: accepted == 0 || case <= 1 || case > 2;
            invariant ExactActor: accepted == 0 || case <= 2 || case > 3;
            invariant ExactEpoch: accepted == 0 || case <= 3 || case > 4;
            invariant Conversation: accepted == 0 || case <= 4 || case > 5;
            invariant RetainedMemory: accepted == 0 || case <= 5 || case > 6;
            invariant DurableRead: accepted == 0 || case <= 6 || case > 7;
            invariant ExistingJournal: accepted == 0 || case <= 11 || case > 12;
            invariant ClosedPreparation: accepted == 0 || kind == 1 || case <= 7 || case > 8;
            invariant ClosureQuiescent: accepted == 0 || kind == 1 || case <= 8 || case > 9;
            invariant NoClosureGrant: accepted == 0 || kind == 1 || case <= 12 || case > 13;
            invariant NoClosureCompletion: accepted == 0 || kind == 1 || case <= 13 || case > 14;
            invariant CompletionDrained: accepted == 0 || case <= 14 || case > 15;
            invariant ExactClosedPhase: accepted == 0 || case <= 15;
            invariant Available: checked == 0 || case > 0 || accepted == 1;
            invariant UnacknowledgedClaimVisible: checked == 0 || kind == 0 || case <= 7 || case > 8 || accepted == 1;
            invariant ActiveCohortClaimVisible: checked == 0 || kind == 0 || case <= 8 || case > 9 || accepted == 1;
            invariant ExpiryDoesNotEraseHistory: checked == 0 || case <= 9 || case > 10 || accepted == 1;
            invariant RevocationDoesNotEraseHistory: checked == 0 || case <= 10 || case > 11 || accepted == 1;
            invariant UsedClaimVisible: checked == 0 || kind == 0 || case <= 12 || case > 13 || accepted == 1;
            invariant SealedClaimVisible: checked == 0 || kind == 0 || case <= 13 || accepted == 1;
            bound Finite: 0 <= kind && kind <= 1 && 0 <= case && case <= 16
                && 0 <= checked && checked <= 1 && 0 <= accepted && accepted <= 1;
        }
    }
}

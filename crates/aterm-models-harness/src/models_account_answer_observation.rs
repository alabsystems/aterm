// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Authentication failure belongs to the observed login; a provider quota
/// still belongs to the account after that login has been replaced.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_answer_observation_model() -> Model {
    aterm_spec::ty_model! {
        AccountAnswerObservation {
            const Buggy = 0;
            var case = 0; var signed_out = 0; var limited = 0;
            var bindings = 1; var unchanged = 1;
            action CurrentAuth when (case == 0) {
                case = 1; signed_out = if Buggy == 1 {0} else {1};
                limited = if Buggy == 6 {1} else {0};
                bindings = if Buggy == 7 {0} else {1};
            }
            action SupersededAuth when (case == 0) {
                case = 2; signed_out = if Buggy == 2 {1} else {0};
                unchanged = if Buggy == 8 {0} else {1};
            }
            action RenewedDuringAuth when (case == 0) {
                case = 3; signed_out = if Buggy == 3 {1} else {0};
            }
            action ChangedPolicyAuth when (case == 0) {
                case = 4; signed_out = if Buggy == 4 {1} else {0};
            }
            action SupersededQuota when (case == 0) {
                case = 5; limited = if Buggy == 5 {0} else {1};
            }
            action MissingRowAuth when (case == 0) {
                case = 6; signed_out = if Buggy == 9 {1} else {0};
            }
            action UnreadableRowAuth when (case == 0) {
                case = 7; signed_out = if Buggy == 10 {1} else {0};
            }
            invariant CurrentAuthRecorded: case == 0 || case > 1 || signed_out == 1;
            invariant OnlyCurrentAuth: case <= 1 || signed_out == 0;
            invariant QuotaAcrossRenewal: case <= 4 || case > 5 || limited == 1;
            invariant OnlyQuotaLimits: case == 5 || limited == 0;
            invariant RetainsBindings: bindings == 1;
            invariant RefusalsUnchanged: case <= 1 || case == 5 || unchanged == 1;
        }
    }
}

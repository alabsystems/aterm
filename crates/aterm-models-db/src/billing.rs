// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Current account pins, current authenticated credentials and retained vault
/// observations compose a request-count billing preparation. This models local
/// observations, not unseen offline revocations or provider charging behavior.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn billing_model() -> Model {
    aterm_spec::ty_model! {
        DbBillingPreparation {
            const Buggy = 0;
            var scenario = 0; var accepted = 0;
            action Valid when (scenario == 0) {
                scenario = 1; accepted = if Buggy == 10 {0} else {1};
            }
            action MissingPin when (scenario == 0) {
                scenario = 2; accepted = if Buggy == 1 {1} else {0};
            }
            action ChangedPolicy when (scenario == 0) {
                scenario = 3; accepted = if Buggy == 2 {1} else {0};
            }
            action ChangedCredential when (scenario == 0) {
                scenario = 4; accepted = if Buggy == 3 {1} else {0};
            }
            action Stopped when (scenario == 0) {
                scenario = 5; accepted = if Buggy == 4 {1} else {0};
            }
            action LocalEdit when (scenario == 0) {
                scenario = 6; accepted = if Buggy == 5 {1} else {0};
            }
            action Rollback when (scenario == 0) {
                scenario = 7; accepted = if Buggy == 6 {1} else {0};
            }
            action Expired when (scenario == 0) {
                scenario = 8; accepted = if Buggy == 7 {1} else {0};
            }
            action Unrelated when (scenario == 0) {
                scenario = 9; accepted = if Buggy == 8 {0} else {1};
            }
            action ForeignRoot when (scenario == 0) {
                scenario = 10; accepted = if Buggy == 9 {1} else {0};
            }
            invariant LocalProgress: 1 > scenario || scenario > 1 || accepted == 1;
            invariant UnrelatedProgress: 9 > scenario || scenario > 9 || accepted == 1;
            invariant NoUnboundPreparation: scenario == 0 || scenario == 1 || scenario == 9 || accepted == 0;
        }
    }
}

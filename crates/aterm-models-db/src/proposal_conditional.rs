// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Conditional metadata writes need complete observed history under their
/// writer lock. An exact retry reports the original receipt without a new write.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn proposal_conditional_model() -> Model {
    aterm_spec::ty_model! {
        DbProposalConditional {
            const Buggy = 0;
            var scenario = 0; var incomplete = 0; var accepted = 0;
            var effects = 0; var same = 1;
            action FreshCreate when (scenario == 0) {
                scenario = 1; accepted = if Buggy == 1 { 0 } else { 1 };
                effects = if Buggy == 8 { 0 } else { 1 };
            }
            action FreshReplace when (scenario == 0) {
                scenario = 2; accepted = if Buggy == 1 { 0 } else { 1 }; effects = 1;
            }
            action GapCreate when (scenario == 0) {
                scenario = 3; incomplete = 1;
                accepted = if Buggy == 2 { 1 } else { 0 };
                effects = if Buggy == 2 { 1 } else { 0 };
            }
            action GapReplace when (scenario == 0) {
                scenario = 4; incomplete = 1;
                accepted = if Buggy == 3 { 1 } else { 0 };
                effects = if Buggy == 3 { 1 } else { 0 };
            }
            action RetryGap when (scenario == 0) {
                scenario = 5; incomplete = 1;
                accepted = if Buggy == 6 { 0 } else { 1 };
                effects = if Buggy == 4 { 2 } else { 0 };
                same = if Buggy == 5 { 0 } else { 1 };
            }
            action HealCreate when (scenario == 0) {
                scenario = 6; accepted = if Buggy == 7 { 0 } else { 1 }; effects = 1;
            }
            invariant EligibleProgress: scenario == 0 || scenario == 3 || scenario == 4 || accepted == 1;
            invariant GapRefusesFresh: 3 > scenario || scenario > 4 || accepted == 0;
            invariant NoGapWrites: incomplete == 0 || effects == 0;
            invariant OriginalReceipt: same == 1;
            invariant OneEffect: effects <= 1;
            invariant FreshPersistence: scenario == 0 || scenario == 5 || accepted == 0 || effects == 1;
        }
    }
}

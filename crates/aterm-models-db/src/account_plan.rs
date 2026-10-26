// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A bounded projection of proposal closure review, never login admission.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_plan_model() -> Model {
    aterm_spec::ty_model! {
        DbAccountPlan {
            const Buggy = 0;
            var scenario = 0; var complete = 0; var proposal = 0; var configuration = 0; var effects = 0;
            action Clean when (scenario == 0) {
                scenario = 1; complete = if Buggy == 1 {0} else {1};
                effects = if Buggy == 2 {1} else {0};
            }
            action Label when (scenario == 0) {
                scenario = 2; complete = 1; proposal = 1; configuration = if Buggy == 3 {1} else {0};
            }
            action Dependency when (scenario == 0) {
                scenario = 3; complete = 1; proposal = 1; configuration = if Buggy == 4 {0} else {1};
            }
            action Alias when (scenario == 0) {
                scenario = 4; complete = 1; proposal = 1; configuration = if Buggy == 5 {0} else {1};
            }
            action Auth when (scenario == 0) {
                scenario = 5; complete = 1; proposal = 1; configuration = if Buggy == 6 {0} else {1};
            }
            action Source when (scenario == 0) {
                scenario = 6; complete = 1; proposal = 1; configuration = if Buggy == 7 {0} else {1};
            }
            action Rewrite when (scenario == 0) {
                scenario = 7; complete = 1; proposal = if Buggy == 8 {0} else {1}; configuration = 0;
            }
            action Unrelated when (scenario == 0) {
                scenario = 8; complete = 1; proposal = if Buggy == 9 {1} else {0};
            }
            action Conflict when (scenario == 0) {scenario = 9; complete = if Buggy == 10 {1} else {0};}
            action Missing when (scenario == 0) {scenario = 10; complete = if Buggy == 11 {1} else {0};}
            action History when (scenario == 0) {scenario = 11; complete = if Buggy == 12 {1} else {0};}
            action Limit when (scenario == 0) {scenario = 12; complete = if Buggy == 13 {1} else {0};}
            action Scope when (scenario == 0) {
                scenario = 13; complete = 1; proposal = 1; configuration = if Buggy == 14 {0} else {1};
            }
            action Cycle when (scenario == 0) {scenario = 14; complete = if Buggy == 15 {1} else {0};}
            invariant LocalReview: scenario == 0 || scenario > 8 || complete == 1;
            invariant NoEffects: effects == 0;
            invariant LabelInert: 2 > scenario || scenario > 2 || configuration == 0;
            invariant BindConfiguration: 3 > scenario || (scenario > 6 && (13 > scenario || scenario > 13)) || configuration == 1;
            invariant RetainProvenance: 2 > scenario || scenario > 7 || proposal == 1;
            invariant UnrelatedInert: 8 > scenario || scenario > 8 || (proposal == 0 && configuration == 0);
            invariant WithholdIncomplete: 9 > scenario || scenario == 13 || complete == 0;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Pooled followers claim an expired canary before a command is released.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_canary_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolCanary {
            const Buggy = 0;
            var phase = 0; var case = 0; var attempted = 0;
            var first = 0; var second = 0; var owner = 0; var audit = 0; var budget = 0;
            action AnotherCase when (phase == 0 && case <= 3) { case = case + 1; }
            action Plan when (phase == 0) {
                phase = 1;
                owner = if Buggy == 1 {1} else {0};
                audit = if Buggy == 1 {1} else {0};
            }
            action First when (phase == 1) {
                phase = 2; attempted = 1;
                first = if Buggy == 9 {0}
                    else if case <= 1 || case == 3 || (Buggy == 6 && case == 2)
                        || (Buggy == 7 && case == 4) {1} else {0};
                owner = if case == 4 {if Buggy == 7 {1} else {3}}
                    else if Buggy == 2 {0}
                    else if case <= 1 || (Buggy == 6 && case == 2)
                        || (Buggy == 8 && case == 3) {1} else {0};
                audit = if Buggy == 5 {0}
                    else if case <= 1 || (Buggy == 6 && case == 2)
                        || (Buggy == 7 && case == 4) || (Buggy == 8 && case == 3) {1} else {0};
                budget = if Buggy == 4 {1} else {0};
            }
            action Second when (phase == 2) {
                phase = 3;
                second = if case == 3 || (Buggy == 3 && case <= 1) {1} else {0};
            }
            action Drop when (phase == 1 || phase == 2) {
                phase = 3;
                first = if Buggy == 10 {1} else {first};
                owner = if Buggy == 10 {1} else {owner};
                audit = if Buggy == 10 {1} else {audit};
            }
            invariant Planning: phase == 0 || phase > 1 || (first == 0 && second == 0 && owner == 0 && audit == 0);
            invariant SingleTakeover: case == 3 || first + second <= 1;
            invariant CommandOwner: (first == 0 || case == 3 || owner == 1) && (second == 0 || case == 3 || owner == 2);
            invariant Budget: budget == 0;
            invariant Audited: audit == if owner == 1 || owner == 2 {1} else {0};
            invariant Eligible: case <= 1 || case == 3 || first + second == 0;
            invariant Followers: case <= 2 || case > 3 || (owner == 0 && audit == 0);
            invariant Progress: attempted == 0 || case > 1 || first == 1;
            invariant Abandoned: attempted == 1 || (first == 0 && second == 0 && owner == 0 && audit == 0);
            invariant CapturedOwner: attempted == 0 || case <= 3 || owner == 3;
        }
    }
}

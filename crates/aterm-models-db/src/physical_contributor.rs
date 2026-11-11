// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local physical absence is distinct from pending or damaged enrollment.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn physical_contributor_model() -> Model {
    aterm_spec::ty_model! {
        DbPhysicalContributor {
            const Buggy = 0;
            var scenario = 0; var done = 0; var accepted = 0; var present = 0; var held = 1;
            action Case when (done == 0 && 23 > scenario) { scenario = scenario + 1; }
            action Read when (done == 0) {
                done = 1;
                accepted = if Buggy == 4 {0} else { if scenario <= 2 || scenario == 10 || scenario == 11 || scenario == 13 || (Buggy == 1 && scenario == 3) || (Buggy == 2 && scenario > 13 && 17 > scenario) || (Buggy == 3 && scenario == 9) || (Buggy == 6 && scenario == 23) {1} else {0} };
                present = if Buggy == 4 {0} else {if scenario == 2 || scenario == 11 || scenario == 12 || (Buggy == 3 && scenario == 9) {1} else {0}};
                held = if Buggy == 5 && (scenario == 2 || scenario == 11) {0} else {1};
            }
            invariant Available: done == 0 || (scenario > 2 && (10 > scenario || scenario > 11) && (13 > scenario || scenario > 13)) || accepted == 1;
            invariant Unavailable: scenario <= 2 || scenario == 10 || scenario == 11 || scenario == 13 || accepted == 0;
            invariant Present: done == 0 || (scenario > 2 && (10 > scenario || scenario > 11) && (13 > scenario || scenario > 13)) || present == (if scenario == 2 || scenario == 11 {1} else {0});
            invariant Custody: present == 0 || held == 1;
            bound Cases: 0 <= scenario && scenario <= 23;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded index publication and exact retry under the account writer. Cases
/// are absent, exact, replaceable, active, malformed, dangling, wrong purpose,
/// and stale witness. No state grants native execution authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn receipt_index_model() -> Model {
    aterm_spec::ty_model! {
        ReceiptIndex {
            const Buggy = 0;
            var scenario = 0; var failed = 0;
            var done = 0; var accepted = 0; var preserved = 1;
            action Scenario when (done == 0 && scenario <= 6) { scenario = scenario + 1; }
            action Failed when (done == 0 && failed == 0) { failed = 1; }
            action Publish when (done == 0) {
                done = 1;
                accepted = if Buggy == 3 {0}
                    else if (scenario <= 2 || (Buggy == 2 && (scenario == 3 || scenario == 7)))
                        && (failed == 0 || Buggy == 1) {1} else {0};
                preserved = if Buggy == 2 && (scenario == 3 || scenario == 7) {0} else {1};
            }
            invariant Unambiguous: accepted == 0 || scenario <= 2;
            invariant Acknowledged: accepted == 0 || failed == 0;
            invariant PreserveForbidden: scenario <= 2 || preserved == 1;
            invariant Available: done == 0 || scenario > 2 || failed == 1 || accepted == 1;
            bound Finite: 0 <= scenario && scenario <= 7 && 0 <= failed && failed <= 1
                && 0 <= done && done <= 1 && 0 <= accepted && accepted <= 1
                && 0 <= preserved && preserved <= 1;
        }
    }
}

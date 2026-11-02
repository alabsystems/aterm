// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Legacy health belongs to the unmanaged default login; local limits keep their force.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_health_scope_model() -> Model {
    aterm_spec::ty_model! {
        AccountHealthScope {
            const Buggy = 0;
            var scope = 0; var issue = 0; var phase = 0;
            var used = 0; var ended = 0; var limited = 0;
            action Scope when (phase == 0 && 7 > scope) { scope = scope + 1; }
            action Issue when (phase == 0 && 3 > issue) { issue = issue + 1; }
            action Assess when (phase == 0) {
                phase = 1;
                used = if issue == 0 && (Buggy == 1 || scope == 0 || scope == 4 || scope == 7) {1} else {0};
                ended = if issue == 1 && (Buggy == 2 || scope == 0 || scope == 4 || scope == 7) {1} else {0};
                limited = if issue == 3 {if Buggy == 5 {0} else {1}}
                    else if issue == 2 {if Buggy == 3 {1} else if Buggy == 4 {0}
                        else if scope == 0 || scope == 4 || scope == 7 {1} else {0}} else {0};
            }
            invariant UsageScoped: phase == 0 || issue > 0 || scope == 0 || scope == 4 || scope == 7 || used == 0;
            invariant EndsScoped: phase == 0 || 1 > issue || issue > 1 || scope == 0 || scope == 4 || scope == 7 || ended == 0;
            invariant LimitsScoped: phase == 0 || 2 > issue || issue > 2 || scope == 0 || scope == 4 || scope == 7 || limited == 0;
            invariant DefaultLimitRetained: phase == 0 || 2 > issue || issue > 2
                || (scope > 0 && 4 > scope) || (scope > 4 && 7 > scope) || limited == 1;
            invariant LocalLimitRetained: phase == 0 || 3 > issue || limited == 1;
        }
    }
}

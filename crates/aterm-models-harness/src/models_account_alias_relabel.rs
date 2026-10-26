// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Explicit alias identity follows its login address across a default-account relabel.
/// Rediscovery accepts the resulting default identity and notices a later account change.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_alias_relabel_model() -> Model {
    aterm_spec::ty_model! {
        AccountAliasRelabel {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var rows = 0; var default = 0; var identity = 0; var relocation = 0; var payload = 1; var browser_safe = 1; var unlisted = 0;
            action AnotherCase when (phase == 0 && 12 > case) {case = case + 1;}
            action Build when (phase == 0) {
                phase = 1;
                rows = if Buggy == 1 && case == 2 {3} else if case == 11 || case == 12 {3} else {2};
                default = if Buggy == 2 && case == 3 {1} else if case == 2 || case == 3 || case == 4 || case == 6 || case == 9 || case == 10 {2}
                    else if case == 11 || case == 12 {3} else {1};
                identity = if Buggy == 3 && case == 7 {1} else if case == 7 || case == 8 || case == 12 {0} else {1};
                relocation = if Buggy == 4 && case == 4 {0} else if Buggy == 6 && case == 10 {1} else if case == 10 {2}
                    else if case == 2 || case == 3 || case == 4 || case == 6 || case == 9 {1} else {0};
                payload = if Buggy == 5 {0} else {1};
                browser_safe = if Buggy == 7 && case == 7 {0} else {1};
                unlisted = if Buggy == 8 && case == 1 {1} else {0};
            }
            action ChangedDefault when (phase == 1) {
                phase = 2;
                unlisted = if Buggy == 9 {0} else {1};
            }
            invariant NoDuplicateDefault: phase == 0 || ((case == 11 || case == 12) && rows == 3)
                || (11 > case && rows == 2);
            invariant DefaultFollowsLogin: phase == 0 || ((case == 2 || case == 3 || case == 4 || case == 6 || case == 9 || case == 10) && default == 2)
                || ((case == 11 || case == 12) && default == 3) || ((case == 1 || case == 5 || case == 7 || case == 8) && default == 1);
            invariant IdentityBoundToLogin: phase == 0 || ((case == 7 || case == 8 || case == 12) && identity == 0)
                || ((7 > case || case == 9 || case == 10 || case == 11) && identity == 1);
            invariant RelocationDecision: phase == 0 || (case == 10 && relocation == 2)
                || ((case == 2 || case == 3 || case == 4 || case == 6 || case == 9) && relocation == 1)
                || ((case == 1 || case == 5 || case == 7 || case == 8 || case == 11 || case == 12) && relocation == 0);
            invariant PayloadPreserved: payload == 1;
            invariant BrowserBoundToLogin: browser_safe == 1;
            invariant NoNeedlessRediscovery: phase == 0 || phase == 2 || unlisted == 0;
            invariant ChangedDefaultDetected: phase == 0 || phase == 1 || unlisted == 1;
        }
    }
}

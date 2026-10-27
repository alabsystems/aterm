// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Changes observed before or during one refresh. This models the final read,
/// not an atomic file lease against replacements after that observation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn codex_refresh_publication_model() -> Model {
    aterm_spec::ty_model! {
        CodexRefreshPublication {
            const Buggy = 0;
            var case = 0; var early = 0; var phase = 0;
            var wrote = 0; var returned = 0; var reused = 0; var kept = 1; var posted = 0;
            action AnotherCase when (phase == 0 && case <= 7) { case = case + 1; }
            action BeforeRequest when (phase == 0 && early == 0) { early = 1; }
            action Refresh when (phase == 0) {
                phase = 1;
                wrote = if case == 0 || case == 6 {if Buggy == 3 {0} else {1}}
                    else {if Buggy == 1 {1} else {0}};
                returned = if case <= 1 || case == 6 {if Buggy == 5 {0} else {1}}
                    else {if Buggy == 2 {1} else {0}};
                reused = if case == 1 {if Buggy == 4 {0} else {1}} else {0};
                kept = if Buggy == 6 {0} else {1};
                posted = if early == 0 || case == 0 || case == 6 || Buggy == 7 {1} else {0};
            }
            invariant FreshPublication: phase == 0 || (case > 0 && case <= 5) || case > 6
                || (wrote == 1 && returned == 1 && reused == 0);
            invariant ReuseKnownRotation: phase == 0 || case == 0 || case > 1
                || (wrote == 0 && returned == 1 && reused == 1);
            invariant RefuseUnknown: phase == 0 || case <= 1 || case == 6
                || (wrote == 0 && returned == 0 && reused == 0);
            invariant PreserveObserved: kept == 1;
            invariant NoObsoleteRequest: phase == 0 || early == 0 || case == 0 || case == 6
                || posted == 0;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Refresh credentials follow recorded OAuth authority. Only absent provenance
/// permits a legacy fallback; invalid records never release the refresh token.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn codex_refresh_authority_model() -> Model {
    aterm_spec::ty_model! {
        CodexRefreshAuthority {
            const Buggy = 0;
            var case = 0; var phase = 0; var result = 0; var posted = 0;
            var target = 1; var client = 1; var kept = 1;
            action AnotherCase when (phase == 0 && case <= 29) { case = case + 1; }
            action Refresh when (phase == 0) {
                phase = 1;
                result = if Buggy == 1 {1} else {if Buggy == 2 {0} else {
                    if case <= 2 || case == 16 || case == 17 || case == 21 || case == 22 || case == 30 {1} else {0}
                }};
                posted = if Buggy == 6 {1} else {
                    if case <= 2 || case == 16 || case == 17 || case == 21 || case == 22 || case > 28 {1} else {0}
                };
                target = if Buggy == 3 {0} else {1};
                client = if Buggy == 4 {0} else {1};
                kept = if Buggy == 5 {0} else {1};
            }
            invariant Admission: phase == 0 || result ==
                if case <= 2 || case == 16 || case == 17 || case == 21 || case == 22 || case == 30 {1} else {0};
            invariant RequestBoundary: phase == 0 || posted ==
                if case <= 2 || case == 16 || case == 17 || case == 21 || case == 22 || case > 28 {1} else {0};
            invariant TargetAuthority: target == 1;
            invariant ClientBinding: client == 1;
            invariant PreserveEvidence: kept == 1;
        }
    }
}

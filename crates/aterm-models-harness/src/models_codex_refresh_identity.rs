// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Workspace routing, token-user agreement and login metadata in one observed
/// credential snapshot. This does not authenticate JWT signatures.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn codex_refresh_identity_model() -> Model {
    aterm_spec::ty_model! {
        CodexRefreshIdentity {
            const Buggy = 0;
            var case = 0; var phase = 0; var result = 0;
            var scoped = 1; var kept = 1; var posted = 0;
            action AnotherCase when (phase == 0 && case <= 16) { case = case + 1; }
            action Observe when (phase == 0) {
                phase = 1;
                result = if case <= 1 || case == 11 || case == 12 {
                    if Buggy == 3 {0} else {1}
                } else {if case == 2 || case == 15 {
                    if Buggy == 3 {0} else {2}
                } else {if Buggy == 1 {2} else {if Buggy == 2 {1} else {0}}}};
                scoped = if Buggy == 4 {0} else {1};
                kept = if Buggy == 5 {0} else {1};
                posted = if case <= 1 || (case > 7 && case <= 12) || Buggy == 6 {1} else {0};
            }
            invariant Decision: phase == 0 || result ==
                if case <= 1 || case == 11 || case == 12 {1}
                else {if case == 2 || case == 15 {2} else {0}};
            invariant WorkspaceRetained: scoped == 1;
            invariant EvidenceRetained: kept == 1;
            invariant RequestBoundary: phase == 0 || posted ==
                if case <= 1 || (case > 7 && case <= 12) {1} else {0};
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Restoring a conversation retains captured attribution without inventing legacy evidence.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_restore_auth_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeRestoreAuth {
            const Buggy = 0;
            var case = 0; var phase = 0;
            var preserved = 1; var synthesized = 0; var accepted = 0;
            action NextCase when (phase == 0 && 8 > case) { case = case + 1; }
            action Restore when (phase == 0) {
                phase = 1;
                preserved = if Buggy == 1 {0} else {1};
                synthesized = if case == 0 && Buggy == 2 {1} else {0};
                accepted = if case == 8 {if Buggy == 3 {1} else {0}}
                    else {if Buggy == 4 {0} else {1}};
            }
            invariant CapturedAttribution: preserved == 1;
            invariant NoInventedEvidence: synthesized == 0;
            invariant ProviderIsolation: phase == 0 || 8 > case || accepted == 0;
            invariant RestoreProgress: phase == 0 || case == 8 || accepted == 1;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// After its own successful publication, a refresh returns that snapshot.
/// Later edits and removals neither change that result nor get restored.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn codex_refresh_result_model() -> Model {
    aterm_spec::ty_model! {
        CodexRefreshResult {
            const Buggy = 0;
            var case = 0;var phase = 0;var written = 0;var result = 0;var bound = 1;var kept = 1;
            action AnotherCase when (phase == 0 && case <= 9) {case = case + 1;}
            action Refresh when (phase == 0) {
                phase = 1;
                written = if case == 9 || (case == 10 && Buggy == 5) {0} else {1};
                result = if (case == 5 || case == 6) && Buggy == 2 {0}
                    else {if case == 9 && Buggy == 3 {1}
                        else {if case == 9 || (case == 10 && Buggy == 5) {0} else {1}}};
                bound = if case > 0 && case <= 8 && Buggy == 1 {0} else {1};
                kept = if case > 0 && case <= 8 && Buggy == 4 {0} else {1};
            }
            invariant Publication: phase == 0 || written == if case == 9 {0} else {1};
            invariant Result: result == written;
            invariant Snapshot: bound == 1;
            invariant KeepLater: kept == 1;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two independent provider quota windows, including reordered slots, missing resets,
/// a rounded percentage below exhaustion, and a refusal without a full window.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn codex_usage_walls_model() -> Model {
    aterm_spec::ty_model! {
        CodexUsageWalls {
            const Buggy = 0;
            var phase = 0; var case = 0; var parsed = 0; var stored = 0; var blocked = 0;
            action Case when (phase == 0 && 9 > case) { case = case + 1; }
            action Parse when (phase == 0) {
                phase = 1;
                parsed = if 5 > case {
                    if Buggy == 1 {1} else {if Buggy == 2 {2} else {3}}
                } else {if case == 5 || case > 7 {1}
                    else {if case == 6 {2} else {if Buggy == 3 {3} else {0}}}};
            }
            action Record when (phase == 1) {
                phase = 2;
                stored = if Buggy == 4 && 5 > case {1} else {parsed};
                blocked = if 3 > case {if Buggy == 5 {0} else {1}} else {0};
            }
            invariant ExactParsing: phase == 0 || parsed == (if 5 > case {3}
                else {if case == 5 || case > 7 {1} else {if case == 6 {2} else {0}}});
            invariant AllWallsPersist: phase <= 1 || stored == parsed;
            invariant LaterResetStillBlocks: phase <= 1 || case > 2 || blocked == 1;
        }
    }
}

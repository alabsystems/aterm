// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A held Codex command uses the suggested login despite inherited credentials,
/// preserves its arguments and unrelated environment, and only reads account state.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cold_resume_environment_model() -> Model {
    aterm_spec::ty_model! {
        ColdResumeEnvironment {
            const Buggy = 0;
            var case = 0; var carried = 0; var phase = 0;
            var login = 1; var clean = 1; var kept = 1;
            var argv = 1; var unchanged = 1; var rendered = 1;
            action NextCase when (phase == 0 && 2 > case) { case = case + 1; }
            action Contaminate when (phase == 0 && carried == 0) { carried = 1; }
            action Render when (phase == 0) {
                phase = 1;
                login = if case == 1 && Buggy == 1 {1}
                    else if case == 2 && Buggy == 2 {0}
                    else if case == 1 {0} else {1};
                clean = if carried == 1 && (case == 0 || Buggy == 3) {0} else {1};
                kept = if Buggy == 4 {0} else {1};
                argv = if Buggy == 5 {0} else {1};
                unchanged = if Buggy == 6 {0} else {1};
                rendered = if Buggy == 7 {0} else {1};
            }
            invariant SelectedLogin: phase == 0 || (case == 1 && login == 0) || (case == 0 && login == 1) || (case == 2 && login == 1);
            invariant ManagedOverridesCleared: phase == 0 || case == 0 || clean == 1;
            invariant UnrelatedRetained: kept == 1;
            invariant PreservedArguments: argv == 1;
            invariant AdviceOnly: unchanged == 1;
            invariant CommandRendered: rendered == 1;
        }
    }
}

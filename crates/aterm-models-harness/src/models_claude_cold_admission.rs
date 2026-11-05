// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Admitting a retained Claude conversation's cold relaunch against current account custody.
/// Cases 20 and 21 are a shell whose environment cannot be read (macOS gives none of an Apple
/// binary such as `/bin/zsh`) with the agent's own launch environment recorded: they admit on
/// that recorded base (`RecordedBase`). Cases 6 and 22 are such a shell with nothing recorded,
/// 23 a recorded base whose home is another, and 24 a readable shell's credential beside a
/// recorded base: they wait (`Admission`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_cold_admission_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeColdAdmission {
            const Buggy = 0;
            var case = 0; var phase = 0;
            var admitted = 0; var binding = 1; var profile = 1;
            action NextCase when (phase == 0 && 24 > case) { case = case + 1; }
            action Decide when (phase == 0) {
                phase = 1;
                admitted = if case == 0 || case == 4 || case == 7 || case == 8 || case == 9 || case == 12 || case == 14 || case == 15 || case == 16
                    {if Buggy == 2 {0} else {1}}
                    else {if case == 20 || case == 21 {if Buggy == 2 || Buggy == 5 {0} else {1}}
                    else {if Buggy == 1 {1} else {0}}};
                binding = if Buggy == 3 {0} else {1};
                profile = if Buggy == 4 {0} else {1};
            }
            invariant Admission: phase == 0 || case == 0 || case == 4 || case == 7 || case == 8 || case == 9 || case == 12 || case == 14 || case == 15 || case == 16 || case == 20 || case == 21 || admitted == 0;
            invariant Progress: phase == 0 || ((case > 0 && 4 > case) || case == 5 || case == 6 || case == 10 || case == 11 || case == 13 || case > 16) || admitted == 1;
            invariant RecordedBase: phase == 0 || 20 > case || case > 21 || admitted == 1;
            invariant Binding: binding == 1;
            invariant Profile: profile == 1;
        }
    }
}

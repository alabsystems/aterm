// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A held command respects the executable's login isolation, default availability and pins.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cold_resume_capability_model() -> Model {
    aterm_spec::ty_model! {
        ColdResumeCapability {
            const Buggy = 0;
            var case = 0; var phase = 0;
            var login = 0; var routed = 0; var unchanged = 1;
            action NextCase when (phase == 0 && 12 > case) { case = case + 1; }
            action Advise when (phase == 0) {
                phase = 1;
                login = if Buggy == 1 && case == 1 {1}
                    else if Buggy == 2 && case == 4 {1}
                    else if Buggy == 3 && case == 7 {0}
                    else if Buggy == 4 && case == 8 {0}
                    else if Buggy == 5 && case == 9 {0}
                    else if Buggy == 6 && case == 0 {0}
                    else if Buggy == 7 && case == 5 {2}
                    else if case == 0 || case == 10 || case == 11 {1}
                    else if case == 7 || case == 8 || case == 9 {2} else {0};
                routed = if Buggy == 8 && case == 11 {1}
                    else if case == 0 || case == 5 || case == 6 || case == 10 || case == 12 {1} else {0};
                unchanged = if Buggy == 9 {0} else {1};
            }
            invariant NamedIsolation: phase == 0 || login == 0 || login == 2 || case == 0 || case == 10 || case == 11;
            invariant PinRespected: phase == 0 || 7 > case || case > 7 || login == 2;
            invariant DefaultAvailable: phase == 0 || 8 > case || case > 9 || login == 2;
            invariant SupportedProgress: phase == 0 || (case > 0 && 10 > case) || case > 10 || (login == 1 && routed == 1);
            invariant DefaultFallback: phase == 0 || 5 > case || (case > 6 && 12 > case) || (login == 0 && routed == 1);
            invariant HealthyRetained: phase == 0 || 11 > case || case > 11 || (login == 1 && routed == 0);
            invariant AdviceOnly: unchanged == 1;
        }
    }
}

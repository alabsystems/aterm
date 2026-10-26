// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// New roster labels cannot inherit retained account authority or an in-flight sign-in.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_create_label_model() -> Model {
    aterm_spec::ty_model! {
        AccountCreateLabel {
            const Buggy = 0;
            var case = 1; var phase = 0; var local = 0;
            var authority = 0; var redirected = 0;
            action AnotherCase when (phase == 0 && case <= 29) { case = case + 1; }
            action Create when (phase == 0) {
                phase = 1;
                local = if case == 1 { if Buggy == 1 {0} else {1} }
                    else if case == 2 { if Buggy == 2 {1} else {0} }
                    else if case == 3 { if Buggy == 3 {1} else {0} }
                    else if case == 4 { if Buggy == 4 {1} else {0} }
                    else if case == 5 { if Buggy == 5 {1} else {0} }
                    else if case == 6 { if Buggy == 6 {1} else {0} }
                    else if case == 7 { if Buggy == 7 {1} else {0} }
                    else if case == 8 { if Buggy == 8 {1} else {0} }
                    else if case == 9 { if Buggy == 9 {1} else {0} }
                    else if case == 10 { if Buggy == 10 {1} else {0} }
                    else if case == 11 { if Buggy == 11 {1} else {0} }
                    else if case == 12 { if Buggy == 12 {1} else {0} }
                    else if case == 13 { if Buggy == 13 {1} else {0} }
                    else if case == 14 { if Buggy == 14 {1} else {0} }
                    else if case == 15 { if Buggy == 15 {1} else {0} }
                    else if case == 16 { if Buggy == 16 {1} else {0} }
                    else if case == 17 { if Buggy == 17 {1} else {0} }
                    else if case == 18 { if Buggy == 18 {0} else {1} }
                    else if case == 19 { if Buggy == 19 {0} else {1} }
                    else if case == 20 { if Buggy == 20 {1} else {0} }
                    else if case == 21 { if Buggy == 21 {1} else {0} }
                    else if case == 22 { if Buggy == 22 {1} else {0} }
                    else if case == 23 { if Buggy == 23 {1} else {0} }
                    else if case == 24 { if Buggy == 24 {1} else {0} }
                    else if case == 25 { if Buggy == 25 {1} else {0} }
                    else if case == 26 { if Buggy == 26 {1} else {0} }
                    else if case == 27 { if Buggy == 27 {0} else {1} }
                    else if case == 28 { if Buggy == 28 {0} else {1} }
                    else if case == 29 { if Buggy == 29 {1} else {0} }
                    else if case == 30 { if Buggy == 30 {1} else {0} } else {0};
                authority = if Buggy == 31 {1} else {0};
                redirected = if Buggy == 32 {1} else {0};
            }
            invariant RetainedLabelsRefused: local == 0 || case == 1 || case == 18
                || case == 19 || case == 27 || case == 28;
            invariant UnusedLabelsAccepted: phase == 0 || local == 1
                || (case > 1 && 18 > case) || (case > 19 && 27 > case) || case > 28;
            invariant NoAuthorityMutation: authority == 0;
            invariant DefaultHomeNotRedirected: redirected == 0;
        }
    }
}

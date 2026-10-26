// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// An unusable higher-priority source retains its place and refuses without publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_source_presence_model() -> Model {
    aterm_spec::ty_model! {
        AccountSourcePresence {
            const Buggy = 0;
            var case = 0; var selected = 0; var present = 0; var kept = 1;
            action Primary when (case == 0) {
                case = 1; present = 1; kept = 0;
                selected = if Buggy == 1 {2} else {1};
            }
            action Fallback when (case == 0) {
                case = 2; present = if Buggy == 2 {1} else {0}; kept = 0;
                selected = if Buggy == 3 {0} else {2};
            }
            action PrimaryUnavailable when (case == 0) {
                case = 3; present = if Buggy == 4 {0} else {1};
                selected = if Buggy == 5 {2} else {0}; kept = if Buggy == 6 {0} else {1};
            }
            action FallbackUnavailable when (case == 0) {
                case = 4; present = 0;
                selected = if Buggy == 7 {2} else {0}; kept = if Buggy == 8 {0} else {1};
            }
            invariant PrimaryWins: case == 0 || case > 1 || selected == 1;
            invariant AbsentAllowsFallback: 2 > case || case > 2 || selected == 2;
            invariant UnusableRefuses: 3 > case || selected == 0;
            invariant RefusalRetains: 3 > case || kept == 1;
            invariant Presence: case == 0 || ((case == 1 || case == 3) && present == 1)
                || ((case == 2 || case == 4) && present == 0);
        }
    }
}

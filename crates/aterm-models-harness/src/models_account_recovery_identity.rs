// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Recovery compares known local identity with the saved check at publication.
/// Unknown metadata remains compatible; an external replacement may precede the writer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_recovery_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountRecoveryIdentity {
            const Buggy = 0;
            var provider = 0; var credential = 0; var late = 0; var phase = 0;
            var published = 0; var bound = 0; var metadata = 1; var preserved = 1;
            action Provider when (phase == 0 && provider == 0) { provider = 1; }
            action Credential when (phase == 0 && credential <= 2) { credential = credential + 1; }
            action Timing when (phase == 0 && late == 0) { late = 1; }
            action Recover when (phase == 0) {
                phase = 1;
                published = if Buggy == 1 || (Buggy == 3 && late == 1) {1}
                    else if Buggy == 2 && credential == 0 {0}
                    else if credential <= 1 || (provider == 0 && credential == 3) {1} else {0};
                bound = if Buggy == 6 {1}
                    else if Buggy == 1 || (Buggy == 3 && late == 1) {1}
                    else if Buggy == 2 && credential == 0 {0}
                    else if credential <= 1 || (provider == 0 && credential == 3) {1} else {0};
                metadata = if Buggy == 5 {0}
                    else if Buggy == 1 || (Buggy == 3 && late == 1) {0}
                    else if Buggy == 2 && credential == 0 {1}
                    else if credential <= 1 || (provider == 0 && credential == 3) {0} else {1};
                preserved = if Buggy == 4 {0} else {1};
            }
            invariant Publication: phase == 0 || published == if credential <= 1 || (provider == 0 && credential == 3) {1} else {0};
            invariant Association: phase == 0 || bound == published;
            invariant Refusal: phase == 0 || published == 1 || metadata == 1;
            invariant Evidence: preserved == 1;
        }
    }
}

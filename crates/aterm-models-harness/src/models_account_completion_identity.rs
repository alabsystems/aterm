// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Fault-free publication after a successful status checks known local identity.
/// Unrelated shared custody requires a principal; profile fields may be unknown.
/// This models the commit boundary, not provider authentication or a file lease.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_completion_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountCompletionIdentity {
            const Buggy = 0;
            var shared = 0; var alternate = 0; var credential = 0; var phase = 0;
            var published = 0; var bound = 0; var metadata = 1;
            var preserved = 1; var checkpoint = 0; var retry = 0;
            action Shared when (phase == 0 && shared == 0) { shared = 1; }
            action Alternate when (phase == 0 && alternate == 0) { alternate = 1; }
            action Credential when (phase == 0 && credential <= 2) { credential = credential + 1; }
            action Complete when (phase == 0) {
                phase = 1;
                published = if Buggy == 1 && credential == 2 {1}
                    else if Buggy == 2 && alternate == 1 && credential == 1 {0}
                    else if Buggy == 3 && (credential == 0 || credential == 3) {0}
                    else if credential == 2 || (shared == 1 && credential == 0) {0} else {1};
                bound = if Buggy == 8 && alternate == 1 {0}
                    else if credential == 2 || (shared == 1 && credential == 0) {0} else {1};
                metadata = if Buggy == 7 {0}
                    else if credential == 2 || (shared == 1 && credential == 0) {1} else {0};
                preserved = if Buggy == 4 {0} else {1};
                checkpoint = if Buggy == 5 {1} else if Buggy == 9 {0}
                    else if credential == 2 || (shared == 1 && credential == 0) {0} else {1};
                retry = if Buggy == 6 && credential == 2 {1} else {0};
            }
            invariant Publication: phase == 0 || published == if credential == 2 || (shared == 1 && credential == 0) {0} else {1};
            invariant Association: phase == 0 || bound == published;
            invariant Checkpoint: phase == 0 || checkpoint == published;
            invariant Refusal: phase == 0 || published == 1 || metadata == 1;
            invariant Evidence: preserved == 1;
            invariant NoFallback: retry == 0;
        }
    }
}

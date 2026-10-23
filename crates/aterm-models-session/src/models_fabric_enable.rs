// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication of a complete capability by `aterm fabric on`.

use aterm_spec::derive::Model;

/// A rejected mint cannot publish a replacement or reach broker supervision.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_capability_publication_model() -> Model {
    aterm_spec::ty_model! {
        FabricCapabilityPublication {
            const Buggy = 0;
            const Grants = 3;
            var completed = 0;
            var refused = 0;
            var published = 0;
            var supervised = 0;

            action Mint when (
                published == 0 && completed <= Grants - 1 &&
                (refused == 0 || Buggy == 1)
            ) {
                completed = completed + 1;
            }
            action Reject when (
                published == 0 && completed <= Grants - 1 && refused == 0
            ) {
                refused = 1;
            }
            action Publish when (
                published == 0 &&
                ((completed == Grants && refused == 0) || Buggy == 1)
            ) {
                published = 1;
            }
            action Supervise when (published == 1 || Buggy == 1) {
                supervised = 1;
            }

            invariant CompleteBeforeActivation:
                (published == 0 && supervised == 0) ||
                (completed == Grants && refused == 0 && published == 1);
        }
    }
}

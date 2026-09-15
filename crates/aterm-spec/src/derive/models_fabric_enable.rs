// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication of a complete capability by `tools/fabric-enable.sh`.

use super::Model;

/// A rejected mint cannot publish a replacement or reach broker supervision.
/// Three grants bound the exhaustive model; Tier-1 runs the same counter with
/// eight grants against subprocess-effect traces from the real enable script.
/// Existing capability bytes and temporary-file cleanup are checked separately
/// by that shell fixture at every mint and on each failure exit.
/// `Buggy=1` restores continuing after rejection and premature publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_capability_publication_model() -> Model {
    crate::ty_model! {
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

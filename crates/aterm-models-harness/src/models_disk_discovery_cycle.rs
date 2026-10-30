// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exhaustion across a bounded two-page inventory, before the existing disk-worker lease.

use aterm_spec::derive::Model;

/// A cycle is done only after committed coverage of the same fully assessed inventory.
/// `covered` is the model's independent observation count, not the cursor's end marker.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn disk_discovery_cycle_model() -> Model {
    aterm_spec::ty_model! {
        DiskDiscoveryCycle {
            const Buggy = 0;
            var progress = 0;
            var covered = 0;
            var staged = 0;
            var end = 0;
            var hard = 0;
            var applied = 0;
            var ready = 0;

            action Observe when (staged == 0) {
                staged = 1;
                end = progress;
                covered = if (progress == 0) { 0 } else { covered };
                hard = 0;
                applied = 0;
                ready = 0;
            }
            action ChangedInventory when (staged == 0) {
                progress = 0;
                covered = 0;
                staged = 1;
                end = 0;
                hard = 0;
                applied = 0;
                ready = 0;
            }
            action HardLimit when (staged == 1) { hard = 1; }
            action Commit when (staged == 1 && hard == 0) {
                progress = if (end == 1) { 0 } else { 1 };
                covered = if (covered == 0) { 1 } else { 2 };
                staged = 0;
                applied = 1;
                ready = end;
            }
            action Cut when (staged == 1 && hard == 1) {
                progress = 0;
                covered = 0;
                staged = 0;
                applied = 0;
                ready = 0;
            }
            action Stop when (staged == 1) {
                progress = 0;
                covered = 0;
                staged = 0;
                applied = 0;
                ready = 0;
            }
            action Clear when (progress > 0 || staged > 0 || ready > 0) {
                progress = 0;
                covered = 0;
                staged = 0;
                applied = 0;
                ready = 0;
            }
            action Abandon when (staged == 1) {
                progress = 0;
                covered = 0;
                staged = 0;
                applied = 0;
                ready = 0;
            }

            action CompleteEarly when (Buggy == 1 && staged == 1 && end == 0 && hard == 0) {
                staged = 0;
                covered = 1;
                applied = 1;
                ready = 1;
            }
            action KeepChangedInventory when (Buggy == 2 && staged == 0 && progress == 1) {
                covered = 0;
                staged = 1;
                end = 1;
                hard = 0;
                applied = 0;
                ready = 0;
            }
            action CompleteCut when (Buggy == 3 && staged == 1 && hard == 1) {
                staged = 0;
                covered = 2;
                applied = 1;
                ready = 1;
            }
            action CompleteStopped when (Buggy == 4 && staged == 1 && hard == 0) {
                staged = 0;
                covered = 2;
                applied = 0;
                ready = 1;
            }
            action CountAbandoned when (Buggy == 5 && staged == 1 && end == 0 && hard == 0) {
                progress = 1;
                covered = 0;
                staged = 0;
                applied = 0;
                ready = 0;
            }

            invariant CompleteCoverage: ready == 0 || covered == 2;
            invariant BoundedAssessment: ready == 0 || hard == 0;
            invariant AppliedPage: ready == 0 || applied == 1;
            bound ProgressBox: progress <= 1;
            bound CoveredBox: covered <= 2;
            bound StagedBox: staged <= 1;
            bound ReadyBox: ready <= 1;
        }
    }
}

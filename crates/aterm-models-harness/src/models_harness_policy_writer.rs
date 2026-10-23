// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's `[harness]` policy has one writer: the config watcher.

use aterm_spec::derive::{Liveness, Model, eq, int, or_, var};

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_policy_one_writer_model() -> Model {
    aterm_spec::ty_model! {
        HarnessPolicyOneWriter {
            const Buggy = 0;
            const MaxEdits = 3;
            var disk = 0;
            var edits = 0;
            var has_posted = 0;
            var last_posted = 0;
            var queued = 0;
            var queued_val = 0;
            var in_force = 1;
            var committed = 0;
            var forget_pending = 0;

            action WriteOff when (disk == 1 && edits <= MaxEdits - 1) {
                disk = 0;
                edits = edits + 1;
            }
            action WriteOn when (disk == 0 && edits <= MaxEdits - 1) {
                disk = 1;
                edits = edits + 1;
            }
            action Poll when (
                queued == 0
                    && (has_posted == 0 || last_posted > disk || disk > last_posted)
            ) {
                has_posted = 1;
                last_posted = disk;
                queued = 1;
                queued_val = disk;
            }
            action Deliver when (queued == 1) {
                in_force = if (committed == 1) { queued_val } else { in_force };
                queued = 0;
                queued_val = 0;
            }
            action AdmitOff when (committed == 1) { in_force = in_force; }
            action AdmitOn when (committed == 1) { in_force = in_force; }
            action Commit when (committed == 0) {
                committed = 1;
                forget_pending = 1;
            }
            action Forget when (forget_pending == 1) {
                has_posted = 0;
                forget_pending = 0;
            }

            action SecondWriterOff when (Buggy == 1 && committed == 1) { in_force = 0; }
            action SecondWriterOn when (Buggy == 1 && committed == 1) { in_force = 1; }
            action CommitWithoutForget when (Buggy == 1 && committed == 0) { committed = 1; }

            invariant Agrees: committed == 0
                || has_posted == 0
                || queued == 1
                || forget_pending == 1
                || in_force == last_posted;
        }
    }
}

/// The window catches up with the file (`[]<>`): once committed, the policy it runs keeps returning
/// to the one on disk, so after the last edit it runs that one for good.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(harness_policy_one_writer_model)]
pub fn harness_policy_one_writer_liveness() -> Liveness {
    Liveness {
        name: "TheWindowCatchesUp",
        goal: or_(
            eq(var("committed"), int(0)),
            eq(var("in_force"), var("disk")),
        ),
        weak: vec!["Poll", "Deliver", "Forget"],
        strong: vec![],
        mutants: vec!["SecondWriterOff", "SecondWriterOn", "CommitWithoutForget"],
    }
}

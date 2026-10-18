// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's `[harness]` policy has one writer: the config watcher.
//!
//! The derived twin of the `clean { … }` island in aterm-gui's `config_watcher_island.rs`;
//! `config_watcher`'s `the_watcher_is_the_policys_one_writer` and `app_native`'s
//! `a_late_admission_never_moves_the_watchers_harness_policy` drive the real code against it.
//!
//! `disk` is the file's policy (0: `enabled = false`, the kill switch; 1: approvals on). The
//! watcher posts a read's policy once per change (`has_posted`, `last_posted`: its dedupe)
//! into the event loop's queue (`queued`, `queued_val`: one post in flight); `Deliver` is the
//! window handling it, which runs it (`in_force`) only once `committed`, since a seamless
//! successor drops every wake handled before its Commit. The Commit handler raises
//! `forget_pending` (`REPOST_AFTER_COMMIT`); the watcher consumes it on its next loop
//! (`Forget`). The config lane admits generations carrying either policy at any time and
//! writes none of them.
//!
//! `Buggy = 1` mutants: an admission that adopts its own `[harness]` (a stale one landing
//! after a posted kill switch lifts it, and the deduped poll never puts it back), and a Commit
//! that raises no forget.

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
            // The lane's admissions carry either policy and write none.
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

            // Mutants (dead at Buggy = 0). A second writer: an admission adopting its own
            // `[harness]`.
            action SecondWriterOff when (Buggy == 1 && committed == 1) { in_force = 0; }
            action SecondWriterOn when (Buggy == 1 && committed == 1) { in_force = 1; }
            // A Commit that raises no forget (`REPOST_AFTER_COMMIT` missing).
            action CommitWithoutForget when (Buggy == 1 && committed == 0) { committed = 1; }

            invariant Agrees: committed == 0
                || has_posted == 0
                || queued == 1
                || forget_pending == 1
                || in_force == last_posted;
        }
    }
}

/// The window catches up with the file (`[]<>`): once committed, the policy it runs keeps
/// returning to the one on disk, so after the last edit it runs that one for good. Each
/// fairness is load-bearing: the watcher polls (`Poll`), the window handles its queue
/// (`Deliver`), the watcher consumes the Commit's flag (`Forget`). Edits are bounded
/// (`MaxEdits`): a file rewritten forever outruns any reader. Each mutant alone breaks it.
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

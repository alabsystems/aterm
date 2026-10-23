// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of carrying unsaved native editor drafts across a self-update.

use aterm_spec::derive::Model;

/// One document through one update attempt.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_editor_carry_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateEditorCarry {
            const Buggy = 0;
            const MaxEdits = 3;
            const Placeholder = 9;
            var head = 0;
            var inflight = 0;
            var target = 0;
            var mem = 0;
            var journal = 0;
            var owner = 0;
            var reseat = 0;
            var phase = 0;
            var restored = 0;
            var succ = 0;
            var late = 0;

            action Type when (phase <= 2 && head <= MaxEdits - 1) {
                head = head + 1;
            }
            action Plan when (phase <= 2 && inflight == 0 && reseat == 0 && head > mem) {
                inflight = 1;
                target = head;
            }
            action Land when (inflight == 1 && owner == 0) {
                inflight = 0;
                mem = target;
                journal = target;
                late = if phase == 2 { 1 } else { late };
                target = 0;
            }
            action Refuse when (inflight == 1 && owner == 1) {
                inflight = 0;
                target = 0;
            }
            action Start when (
                phase == 0
                    && (head == 0
                        || (reseat == 0 && inflight == 0 && mem == head && journal == head))
            ) {
                phase = 1;
            }
            action StandDown when (phase == 1) {
                phase = 0;
            }
            action Park when (phase == 1) {
                phase = 2;
            }
            action Restore when (phase == 2 && restored == 0) {
                restored = 1;
                succ = journal;
                owner = 1;
            }
            action RestoreIdentical when (phase == 2 && restored == 0 && owner == 0) {
                restored = 1;
                succ = journal;
            }
            action RestoreFails when (phase == 2 && restored == 0) {
                restored = 1;
                succ = Placeholder;
            }
            action Commit when (
                phase == 2
                    && restored == 1
                    && (Buggy == 1
                        || ((late == 0 || Buggy == 3)
                            && (head == 0
                                || (reseat == 0
                                    && inflight == 0
                                    && mem == head
                                    && journal == head))))
            ) {
                phase = 3;
            }
            action Rollback when (phase == 2) {
                phase = 0;
                restored = 0;
                succ = 0;
                late = 0;
                reseat = if Buggy == 1 { reseat } else { 1 };
            }
            action Reseat when (
                reseat == 1 && inflight == 0 && (phase == 0 || (Buggy == 2 && phase <= 2))
            ) {
                reseat = 0;
                owner = 0;
                mem = head;
                journal = head;
            }

            invariant NoDraftLost:
                phase <= 2 || succ == head || (succ == Placeholder && journal == head);
            invariant JournalOwnedOutsideHandoff: phase > 0 || reseat == 1 || owner == 0;
        }
    }
}

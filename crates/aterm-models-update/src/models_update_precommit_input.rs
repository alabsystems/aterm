// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the self-update successor's pre-Commit input queue.

use aterm_spec::derive::Model;

/// Formal model of one window's pre-Commit input queue in the successor.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_precommit_input_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdatePreCommitInput {
            const Buggy = 0;
            var queued = 0;
            var reordered = 0;
            var dropped = 0;
            var incoherent = 0;
            var settled = 0;
            var repaired = 0;
            var stranded = 0;

            action Type when (settled == 0) {
                queued = 1;
            }
            action LoseFocus when (settled == 0) {
                reordered = if queued == 1 { 1 } else { reordered };
                incoherent = if queued == 1 || Buggy == 1 { 1 } else { incoherent };
            }
            action Drop when (settled == 0 && queued == 1) {
                dropped = 1;
                incoherent = if Buggy == 1 { incoherent } else { 1 };
            }
            action Settle when (settled == 0) {
                settled = 1;
                repaired = incoherent;
                stranded = if (reordered == 1 || dropped == 1) && incoherent == 0 { 1 } else { 0 };
            }

            invariant ReorderedInputIsRepaired: stranded == 0;
            invariant RepairOnlyForACause: repaired == 0 || reordered == 1 || dropped == 1;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded boot checkpoint selection and immutable accounting.

use aterm_spec::derive::*;

/// One boot, with actual continuous capture cuts abstracted to 1 (180 s) and 2
/// (900 s or later). Target references may share an actual cut. Saved and pending
/// state is the native owner's projection, separate from the stored document.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn boot_cost_checkpoint_model() -> Model {
    aterm_spec::ty_model! {
        BootCostCheckpoint {
            const Buggy = 0;
            var phase = 0;
            var first = 0;
            var second = 0;
            var actual_first = 0;
            var actual_second = 0;
            var nodes = 0;
            var captures = 0;
            var own = 0;
            var captured_own = 0;
            var saved = 0;
            var pending = 0;
            var failed = 0;
            var repeated = 0;
            var restored = 0;
            var conflict = 0;

            action Capture180 when (phase == 0) {
                phase = 1;
                first = 1;
                actual_first = 1;
                nodes = 1;
                captures = 1;
                own = 5;
                captured_own = 5;
                pending = 1;
            }
            action Capture900 when (phase == 1) {
                phase = 2;
                second = 2;
                actual_second = 2;
                nodes = 2;
                captures = 2;
                pending = 1;
            }
            action FirstCaptureAfter900 when (phase == 0) {
                phase = 2;
                first = if (Buggy == 2) { 1 } else { 2 };
                second = 2;
                actual_first = 2;
                actual_second = 2;
                nodes = if (Buggy == 1) { 2 } else { 1 };
                captures = 1;
                own = 5;
                captured_own = 5;
                pending = 1;
            }
            action WriteLanded when (nodes > saved) {
                saved = nodes;
                pending = 0;
            }
            action WriteFailed when (nodes > saved && pending == 1 && failed == 0) {
                pending = if (Buggy == 4) { 0 } else { pending };
                failed = 1;
            }
            action Repeat when (phase == 2 && repeated == 0) {
                own = if (Buggy == 3) { 7 } else { own };
                repeated = 1;
            }
            action Restore when (nodes > 0 && restored == 0) {
                first = if (Buggy == 5) { 3 } else { first };
                restored = 1;
            }
            action ConflictingSource when (nodes > 0 && conflict == 0) {
                own = if (Buggy == 6) { own + 7 } else { own };
                conflict = 1;
            }

            invariant TargetsNameActualCuts:
                first == actual_first && second == actual_second;
            invariant OneNodePerActualCapture: nodes == captures;
            invariant CapturedAccountingIsImmutable: own == captured_own;
            invariant FailedWriteRetainsPending: saved == nodes || pending == 1;
            bound FixedSize:
                phase <= 2 && nodes <= 2 && captures <= 2 && saved <= 2 &&
                first <= 3 && second <= 2 && own <= 14 && pending <= 1 &&
                failed <= 1 && repeated <= 1 && restored <= 1 && conflict <= 1;
        }
    }
}

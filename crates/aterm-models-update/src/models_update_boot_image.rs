// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a launch's boot apply may do to the process's image: a window keeps it.

use aterm_spec::derive::Model;

/// Boot apply by image class (`ImageReplacement`) over one staged build and its launches.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_boot_image_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateBootImage {
            const Buggy = 0;
            const Escape = 2;
            var class = 0;
            var classified = 0;
            var stage = 0;
            var crash_loop = 0;
            var ended_left = 0;
            var phase = 0;
            var execs = 0;
            var reverted = 0;
            var escaped = 0;
            var left = 0;
            var swaps = 0;
            var press_only = 0;
            var record_unusable = 0;

            action Stage when (phase == 0 && classified == 0 && stage == 0) {
                stage = 1;
            }
            action PriorLeftLaunchEnded when (
                phase == 0 && classified == 0 && stage == 1 && Escape > ended_left
            ) {
                ended_left = ended_left + 1;
            }
            action PressOnly when (phase == 0 && classified == 0 && press_only == 0) {
                press_only = 1;
            }
            action RecordUnusable when (
                phase == 0 && classified == 0 && stage == 1 && record_unusable == 0
            ) {
                record_unusable = 1;
            }
            action CrashLoop when (
                phase == 0 && classified == 0 && crash_loop == 0 && reverted == 0
            ) {
                crash_loop = 1;
            }
            action ClassifyReplaceable when (phase == 0 && classified == 0) {
                classified = 1;
                class = 0;
            }
            action ClassifyWindow when (phase == 0 && classified == 0) {
                classified = 1;
                class = 1;
            }
            action ClassifySuccessor when (phase == 0 && classified == 0) {
                classified = 1;
                class = 2;
            }

            action RevertExec when (
                phase == 0 && classified == 1 && crash_loop == 1 && class <= 1
            ) {
                crash_loop = 0;
                reverted = 1;
                execs = execs + 1;
                stage = if stage == 1 { 2 } else { stage };
            }
            action RevertExit when (
                phase == 0 && classified == 1 && crash_loop == 1 && class == 2
            ) {
                crash_loop = 0;
                reverted = 1;
                stage = if stage == 1 { 2 } else { stage };
                phase = 2;
            }
            action RevertExecSuccessor when (
                Buggy == 2 && phase == 0 && classified == 1 && crash_loop == 1 && class == 2
            ) {
                crash_loop = 0;
                reverted = 1;
                execs = execs + 1;
                stage = if stage == 1 { 2 } else { stage };
            }

            action BootNothing when (
                phase == 0 && classified == 1 && crash_loop == 0 &&
                (stage == 0 || stage == 2 || class == 2)
            ) {
                phase = 1;
            }
            action BootSwapExec when (
                phase == 0 && classified == 1 && crash_loop == 0 && stage == 1 && class == 0
            ) {
                swaps = swaps + 1;
                execs = execs + 1;
                stage = 2;
                ended_left = 0;
                phase = 1;
            }
            action BootSwapExecKeepsStage when (
                Buggy == 3 && phase == 0 && classified == 1 && crash_loop == 0 &&
                stage == 1 && class == 0 && swaps == 0
            ) {
                swaps = swaps + 1;
                execs = execs + 1;
                phase = 1;
            }
            action BootLeave when (
                phase == 0 && classified == 1 && crash_loop == 0 && stage == 1 &&
                class == 1 && press_only == 0 && record_unusable == 0 && Escape > ended_left
            ) {
                left = 1;
                phase = 1;
            }
            action BootLeaveForPress when (
                phase == 0 && classified == 1 && crash_loop == 0 && stage == 1 &&
                class == 1 && press_only == 1
            ) {
                phase = 1;
            }
            action BootSwapExecKeptImage when (
                Buggy == 1 && phase == 0 && classified == 1 && crash_loop == 0 &&
                stage == 1 && class == 1 && Escape > ended_left
            ) {
                swaps = swaps + 1;
                execs = execs + 1;
                stage = 2;
                ended_left = 0;
                phase = 1;
            }
            action BootEscapeSwapExec when (
                phase == 0 && classified == 1 && crash_loop == 0 && stage == 1 &&
                class == 1 && press_only == 0 && record_unusable == 0 && Escape <= ended_left
            ) {
                swaps = swaps + 1;
                execs = execs + 1;
                escaped = 1;
                stage = 2;
                ended_left = 0;
                phase = 1;
            }
            action BootRecordUnusableSwapExec when (
                phase == 0 && classified == 1 && crash_loop == 0 && stage == 1 &&
                class == 1 && press_only == 0 && record_unusable == 1
            ) {
                swaps = swaps + 1;
                execs = execs + 1;
                escaped = 1;
                stage = 2;
                ended_left = 0;
                phase = 1;
            }
            action BootEscapeSwapExecPressOnly when (
                Buggy == 4 && phase == 0 && classified == 1 && crash_loop == 0 &&
                stage == 1 && class == 1 && press_only == 1 && Escape <= ended_left
            ) {
                swaps = swaps + 1;
                execs = execs + 1;
                escaped = 1;
                stage = 2;
                ended_left = 0;
                phase = 1;
            }

            action SessionParentSwap when (phase == 1 && stage == 1) {
                swaps = swaps + 1;
                stage = 2;
                ended_left = 0;
                phase = 2;
            }
            action LaunchEnds when (phase == 1) {
                ended_left = if left == 1 && stage == 1 && Escape > ended_left {
                    ended_left + 1
                } else {
                    ended_left
                };
                phase = 3;
            }
            action NextLaunch when (phase > 1) {
                phase = 0;
                classified = 0;
                class = 0;
                crash_loop = 0;
                execs = 0;
                reverted = 0;
                escaped = 0;
                left = 0;
                press_only = 0;
                record_unusable = 0;
            }

            invariant LaunchedSuccessorNeverExecs: class <= 1 || execs == 0;
            invariant WindowExecsOnlyToRevertOrEscape:
                class == 0 || class == 2 || execs <= reverted + escaped;
            invariant PressOnlyWindowExecsOnlyToRevert:
                class == 0 || class == 2 || press_only == 0 || execs <= reverted;
            invariant OneSwapPerStage: swaps <= 1;
            bound LeftLaunchesBounded: ended_left <= Escape;
        }
    }
}

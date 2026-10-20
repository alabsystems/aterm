// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a launch's boot apply may do to the process's image: a window keeps it.

use aterm_spec::derive::Model;

/// The boot apply by image class (aterm-update `ImageReplacement`), over one staged build
/// and the launches that meet it.
///
/// An `execve` a few hundred milliseconds after LaunchServices launched a process leaves
/// LaunchServices answering pid -1 for the app for that process's life (no Quit & Reopen, no
/// menu-bar item, TCC attributing it to the rollback path). So a launch is classed once at
/// boot: `class` 0 is a replaceable launch (`--headless`, an explicit control socket, the
/// cold lane's relaunch, a fork-lane successor, every launch off macOS), 1 a macOS window no
/// parent handed off to (`WindowKeepsImage`), 2 a seamless successor LaunchServices launched.
///
/// A replaceable launch with a stage ready swaps and execs at boot (`BootSwapExec`). A window
/// leaves the stage for its own in-session lane, whose parent swaps before LaunchServices
/// launches the build it installed and never execs (`SessionParentSwap`). With
/// `[update] auto_apply` off (`press_only`) that lane is a person's press: the window leaves
/// the stage on no record (`BootLeaveForPress`) and never escapes. With it on, the window
/// leaves it on the left-stage record (`BootLeave`) until `Escape` earlier launches left the
/// same stage that way and ended with it still there (`ended_left`, counted on positive
/// evidence of each end); then it swaps and execs at boot as a replaceable launch does
/// (`BootEscapeSwapExec`), so an old build whose own lane cannot land the stage never
/// strands it. A record that cannot count this launch (`record_unusable`: unreadable,
/// unwritable, or a process the kernel will not name) escapes the same way
/// (`BootRecordUnusableSwapExec`). A launched successor swaps nothing (`BootNothing`).
///
/// A crash-loop revert puts the previous build back and retires the stage: it re-execs the
/// restored build (`RevertExec`) in every class but a launched successor, which exits instead
/// (`RevertExit`). `NextLaunch` starts another process over the same disk, so a stage is
/// swapped at most once across launches (`OneSwapPerStage`).
///
/// Mutants: `Buggy=1` swaps and execs a kept image at boot before the escape
/// (`BootSwapExecKeptImage`, the boot this change removed; `WindowExecsOnlyToRevertOrEscape`).
/// `Buggy=2` re-execs a launched successor's restored build (`RevertExecSuccessor`;
/// `LaunchedSuccessorNeverExecs`). `Buggy=3` swaps and execs without retiring the stage
/// (`BootSwapExecKeepsStage`), which a window's lane then swaps again (`OneSwapPerStage`).
/// `Buggy=4` escapes a stage that waits for a press (`BootEscapeSwapExecPressOnly`;
/// `PressOnlyWindowExecsOnlyToRevert`).
///
/// Tier-1: aterm-update `install_tests.rs`'s
/// `a_window_launch_leaves_the_stage_for_its_window_and_execs_only_to_revert_or_escape`
/// binds the real `boot_image_lane` and `revert_lane` over every class, crash loop, stage,
/// count, setting and record, and runs the real boot router (`boot_lane_under_apply_lock`)
/// over a scratch staging root (leave, leave for a press, escape once two recorded launches
/// have ended, escape on a record that cannot take this launch), with `Buggy=1`, `Buggy=2`
/// and `Buggy=4` as negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_boot_image_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateBootImage {
            const Buggy = 0;
            const Escape = 2;
            // 0 Replaceable, 1 WindowKeepsImage, 2 LaunchedSuccessor.
            var class = 0;
            var classified = 0;
            // 0 nothing staged, 1 ready, 2 gone (installed, or retired by a revert).
            var stage = 0;
            var crash_loop = 0;
            // Earlier launches that left this stage for their window and have ended.
            var ended_left = 0;
            // 0 booting, 1 running, 2 gone by a handoff or a revert's exit, 3 ended.
            var phase = 0;
            // This process's image replacements, and why it may have made them.
            var execs = 0;
            var reverted = 0;
            var escaped = 0;
            // This launch left the stage for its window's automatic lane, on the record.
            var left = 0;
            var swaps = 0;
            // `[update] auto_apply` is off for this launch: its stage waits for a press.
            var press_only = 0;
            // The left-stage record cannot count this launch.
            var record_unusable = 0;

            // The disk and the class, before the boot decides.
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

            // The crash-loop revert, before any swap: the restored image boots on.
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

            // The boot's lane for the stage.
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

            // The running window's in-session lane: the parent swaps, LaunchServices
            // launches the build it installed, and the parent leaves at Commit.
            action SessionParentSwap when (phase == 1 && stage == 1) {
                swaps = swaps + 1;
                stage = 2;
                ended_left = 0;
                phase = 2;
            }
            // The process ends with the stage still there, cleanly or not.
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

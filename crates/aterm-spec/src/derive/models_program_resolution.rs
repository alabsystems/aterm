// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A foreground process whose first argv lookup misses still gets named on a
//! static screen. The GUI's StatusObserver is the Tier-1 binding.

use super::*;

/// One foreground group and its two bounded naming schedules. `attempts`
/// projects the real saturating unknown-name counter (1 = 250 ms, 2 = 1 s,
/// 3 = 5 s); `Elapse` abstracts passage of the current deadline, and `Decide`
/// is the GUI's unknown-name `program_due` guard. A Claude name disarms both
/// timers. A transient shell name instead owns one short confirmation; after
/// that, a later single screen move owns a delayed recheck even when the
/// screen becomes static. `Buggy=1` restores the missing timers and
/// same-generation rejection that stranded Claude's approval screen.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn program_resolution_retry_model() -> Model {
    crate::ty_model! {
        ProgramResolutionRetry {
            const Buggy = 0;
            var attempts = 0;
            var known = 0;
            var agent = 0;
            var armed = 0;
            var confirm_armed = 0;
            var confirmed = 0;
            var elapsed = 0;
            var screen_moved = 0;
            var checked = 0;
            var retry_due = 0;

            action Start when (attempts == 0 && known == 0) {
                attempts = 1;
                armed = if Buggy == 1 { 0 } else { 1 };
            }
            action Elapse when (
                elapsed == 0 &&
                ((attempts > 0 && known == 0) ||
                 (known == 1 && agent == 0 && confirm_armed == 1))
            ) {
                elapsed = 1;
            }
            action MoveScreen when (attempts > 0 && known == 0 && screen_moved == 0) {
                screen_moved = 1;
            }
            action Decide when (attempts > 0 && known == 0 && elapsed == 1 && checked == 0) {
                checked = 1;
                retry_due = if Buggy == 1 && screen_moved == 0 { 0 } else { 1 };
            }
            action Retry when (checked == 1 && retry_due == 1 && known == 0) {
                attempts = if attempts <= 2 { attempts + 1 } else { attempts };
                elapsed = 0;
                screen_moved = 0;
                checked = 0;
                retry_due = 0;
            }
            action Resolve when (attempts > 0 && known == 0) {
                known = 1;
                agent = 1;
                attempts = 0;
                armed = 0;
                confirm_armed = 0;
                confirmed = 1;
                elapsed = 0;
                checked = 0;
                retry_due = 0;
            }
            action ResolveShell when (attempts > 0 && known == 0) {
                known = 1;
                attempts = 0;
                armed = 0;
                confirm_armed = if Buggy == 1 { 0 } else { 1 };
                confirmed = 0;
                elapsed = 0;
                checked = 0;
                retry_due = 0;
            }
            action ConfirmShell when (
                known == 1 && agent == 0 && confirm_armed == 1 && elapsed == 1
            ) {
                confirm_armed = 0;
                confirmed = 1;
                elapsed = 0;
                screen_moved = 0;
            }
            action MoveNamed when (
                known == 1 && agent == 0 && confirmed == 1 &&
                confirm_armed == 0 && screen_moved == 0
            ) {
                screen_moved = 1;
                confirm_armed = if Buggy == 1 { 0 } else { 1 };
            }
            action NameBecomesClaude when (known == 1 && agent == 0) {
                agent = 1;
                confirm_armed = 0;
                confirmed = 1;
                elapsed = 0;
                screen_moved = 0;
            }
            action AgentLeaves when (known == 1 && agent == 1) {
                agent = 0;
                confirm_armed = 1;
                confirmed = 1;
                screen_moved = 1;
            }
            action GroupLeaves when (known == 1 || attempts > 0) {
                attempts = 0;
                known = 0;
                agent = 0;
                armed = 0;
                confirm_armed = 0;
                confirmed = 0;
                elapsed = 0;
                screen_moved = 0;
                checked = 0;
                retry_due = 0;
            }

            invariant UnnamedGroupOwnsRetry:
                if attempts > 0 && known == 0 { armed == 1 } else { armed == 0 };
            invariant StaticMissGetsRetry:
                if checked == 1 && known == 0 && elapsed == 1 && screen_moved == 0 {
                    retry_due == 1
                } else { retry_due <= 1 };
            invariant NamedShellKeepsNeededDeadline:
                if known == 1 && agent == 0 && (confirmed == 0 || screen_moved == 1) {
                    confirm_armed == 1
                } else { confirm_armed <= 1 };
        }
    }
}

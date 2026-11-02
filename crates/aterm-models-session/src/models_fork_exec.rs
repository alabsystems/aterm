// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the fork-to-exec child window of `aterm_pty`'s `spawn_shell`.

use aterm_spec::derive::Model;

/// The child's program counter through the fixed step list, and what the window did.
///
/// `PassFd` puts the descriptor the caller passes at its number (a no-op when it passes none).
/// It runs after the master's close, so a master at that number cannot close it (`passHeld`;
/// `Buggy = 4` passes first and loses it), and moves the exec-status write end off that number
/// first, so the pass's `dup2` cannot close it and a failed exec is still told (`statusOpen`;
/// `Buggy = 5` leaves the write end there).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fork_exec_model() -> Model {
    aterm_spec::ty_model! {
        ForkExec {
            const Buggy = 0;
            var pc = 0;
            var masterClosed = 0;
            var unsafeOpRan = 0;
            var envPrebuilt = 1;
            var passHeld = 0;
            var statusOpen = 1;

            action Fork when (pc == 0) {
                pc = 1;
                envPrebuilt = if Buggy == 3 { 0 } else { envPrebuilt };
            }
            action Setrlimit when (pc == 1) { pc = 2; }
            action Chdir when (pc == 2) { pc = 3; }
            action CloseMaster when (pc == 3) {
                pc = 4;
                masterClosed = if Buggy == 2 { 0 } else { 1 };
            }
            action PassFd when (pc == 4) {
                pc = 5;
                passHeld = if Buggy == 4 { 0 } else { 1 };
                statusOpen = if Buggy == 5 { 0 } else { statusOpen };
            }
            action UnsafeEnvOp when (Buggy == 1 && pc > 0 && pc <= 5) { unsafeOpRan = 1; }
            action Exec when (pc == 5) { pc = 6; }

            invariant OnlySafeBeforeExec: pc <= 5 || unsafeOpRan == 0;
            invariant MasterClosedBeforeExec: pc <= 5 || masterClosed == 1;
            invariant SafeImpliesEnvPrebuilt: unsafeOpRan == 1 || envPrebuilt == 1;
            invariant PassedHeldAtExec: pc <= 5 || passHeld == 1;
            invariant FailedExecIsTold: pc <= 5 || statusOpen == 1;
            bound StepList: pc <= 6;
        }
    }
}

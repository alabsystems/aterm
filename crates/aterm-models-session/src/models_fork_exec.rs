// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fork→exec child window of `aterm_pty`'s `spawn_shell`.
//!
//! The frontend is multi-threaded, so between `fork` and `exec` POSIX permits only
//! async-signal-safe calls: a lock a vanished thread held (the environment lock, the
//! allocator) deadlocks or aborts the child. The child must also close the forkpty master
//! before exec, or the fd leaks into the shell.

use aterm_spec::derive::Model;

/// The child's program counter through the fixed step list, and what the window did.
///
/// State: `pc` 0 before fork, 1 forked (window open), 2 after `setrlimit`, 3 after
/// `chdir`, 4 after `close(master)`, 5 exec'd (window closed); `masterClosed`;
/// `unsafeOpRan` (a non-async-signal-safe op ran in the window); `envPrebuilt` (the parent
/// built envp before forkpty, so exec needs no allocation).
///
/// Mutants: `Buggy = 1` runs environment work in the window (`UnsafeEnvOp`, dead at the
/// committed configuration: the audited negative control the pty anchors waive),
/// `Buggy = 2` advances past `CloseMaster` without closing, `Buggy = 3` leaves envp to
/// the child.
///
/// Tier-0: `aterm-models-session/tests/derived_isolation.rs`. The child branch cannot be
/// driven in-process, so the binding is the anchors on `aterm-pty`'s fork/exec seam.
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
            action UnsafeEnvOp when (Buggy == 1 && pc > 0 && pc <= 4) { unsafeOpRan = 1; }
            action Exec when (pc == 4) { pc = 5; }

            // No async-signal-unsafe op ran between fork and exec.
            invariant OnlySafeBeforeExec: pc <= 4 || unsafeOpRan == 0;
            // The forkpty master is closed before exec: it never leaks into the shell.
            invariant MasterClosedBeforeExec: pc <= 4 || masterClosed == 1;
            // With no unsafe op in the window, a pre-built envp exists for exec.
            invariant SafeImpliesEnvPrebuilt: unsafeOpRan == 1 || envPrebuilt == 1;
            bound StepList: pc <= 5;
        }
    }
}

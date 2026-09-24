// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pending stub waiting for its program on a person's terminal: the READER of
//! `<prefix>/progress.json` and the program's shim (`atpkg::cli::wait_then_run`) against
//! the WRITER, the pass installing that program.

use super::*;

/// One program a running pass is installing, and the stub a person typed its name into.
/// Writer: `row` is the program's phase in the pass (0 queued, 1 downloading, 2 set up —
/// its shim laid, 3 done, 4 failed), `shim` whether its real shim resolves (laid with the
/// set-up phase, never removed), `running` whether the pass is live (it can end, or die,
/// at any moment, and never comes back). Reader: `step` is where the stub's poll is (0
/// about to read, 1 deciding, 2 done), `seen` what its read captured, `stub` what it did
/// (0 still waiting, 1 ran the program, 2 said it failed).
///
/// The stub reads whether the pass is RUNNING first and whether the shim resolves
/// second, and decides: run it when the shim resolves; say it failed when the pass
/// recorded a failure or had already ended when read; else wait and poll again. That
/// order is the whole contract: a pass lays the shim before it ends, so a pass seen
/// ended has laid every shim it ever will.
///
/// `Buggy=1` is the two ways a reader gets this wrong, one per invariant. It reads the
/// other way round — the shim first, then whether the pass still runs — so a pass that
/// sets the program up and ends between the two reads makes the stub say "the install
/// stopped" about a program that is installed (`NoFalseFailure`). And it takes a pass that
/// ended without recording a failure for an install, running a program whose pass died
/// before laying it (`RunsOnlyInstalled`). Tier-1 (`atpkg::cli`'s tests) drives the real
/// decision (`pending_wait_step`) over every deciding state, with a reader that asks
/// "ended?" before "installed?" as the caught negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_pending_wait_model() -> Model {
    crate::ty_model! {
        AtpkgPendingWait {
            const Buggy = 0;
            var row = 0;
            var shim = 0;
            var running = 1;
            var step = 0;
            var seen = 0;
            var stub = 0;

            // The writer: the pass moves the program on, lays its shim when it sets it
            // up, may fail it before then, and ends (or dies) whenever.
            action Download when (running == 1 && row == 0) {
                row = 1;
            }
            action SetUp when (running == 1 && row == 1) {
                row = 2;
                shim = 1;
            }
            action Finish when (running == 1 && row == 2) {
                row = 3;
            }
            action FailRow when (running == 1 && row <= 1) {
                row = 4;
            }
            action End when (running == 1) {
                running = 0;
            }

            // The reader's first read: whether the pass runs (the shim, when buggy).
            action Observe when (step == 0 && stub == 0) {
                seen = if Buggy == 1 { shim } else { running };
                step = 1;
            }
            // The decision, over what was read and the second read.
            action DecideRun when (
                step == 1
                    && ((Buggy == 0 && shim == 1)
                        || (Buggy == 1 && (seen == 1 || (running == 0 && row <= 3))))
            ) {
                stub = 1;
                step = 2;
            }
            action DecideFail when (
                step == 1
                    && ((Buggy == 0 && shim == 0 && (row == 4 || seen == 0))
                        || (Buggy == 1 && seen == 0 && (row == 4 || running == 0)))
            ) {
                stub = 2;
                step = 2;
            }
            action DecideWait when (
                step == 1
                    && ((Buggy == 0 && shim == 0 && row <= 3 && seen == 1)
                        || (Buggy == 1 && seen == 0 && row <= 3 && running == 1))
            ) {
                step = 0;
            }

            // A failure is said only about a program that is not installed, or whose
            // install failed.
            invariant NoFalseFailure: stub <= 1 || shim == 0 || row == 4;
            // A program is run only once it is installed.
            invariant RunsOnlyInstalled: stub == 0 || stub == 2 || shim == 1;
        }
    }
}

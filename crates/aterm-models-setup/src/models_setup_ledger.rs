// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the setup run's ledger and the keys the person changed (aterm-cli's
//! `macos_doctor::initial::ledger` and `::kept`).

use aterm_spec::derive::*;

/// SetupLedger: two stages of the first-launch setup run (two, so the running record is a set,
/// as the concurrent lanes need), the process that runs them ending at any point, and one
/// setting the person may change after aterm set it and read it back.
///
/// A stage begins only when it is not done, not running and not parked, and each begin counts
/// an attempt. The process ending leaves the stages it was in recorded as running with the
/// profile lock free; the next launch resumes each of them once and parks one that had already
/// begun twice. A quit or an update's handoff marks the stages it cuts off first, and the next
/// launch gives their begin back instead. A stage that has not ended within the hang limit
/// parks while it runs. Check again clears every park and the attempts. A key read back as
/// aterm set it is recorded as applied; when a later read differs, the person changed it, and
/// no unattended pass writes it again until Restore or turning its group on forgets that.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn setup_ledger_model() -> Model {
    aterm_spec::ty_model! {
        SetupLedger {
            const Buggy = 0;

            var alive = 1;
            var done_a = 0;
            var run_a = 0;
            var att_a = 0;
            var park_a = 0;
            var ever_a = 0;
            var quit_a = 0;
            var done_b = 0;
            var run_b = 0;
            var att_b = 0;
            var park_b = 0;
            var ever_b = 0;
            var quit_b = 0;
            var applied = 0;
            var kept = 0;
            var wrote_kept = 0;

            action BeginA when (alive == 1 && done_a == 0 && run_a == 0 && park_a == 0) {
                run_a = 1;
                att_a = att_a + 1;
            }
            action BeginB when (alive == 1 && done_b == 0 && run_b == 0 && park_b == 0) {
                run_b = 1;
                att_b = att_b + 1;
            }
            action EndA when (alive == 1 && run_a == 1) {
                done_a = 1;
                ever_a = 1;
                run_a = 0;
                att_a = 0;
                park_a = 0;
            }
            action EndB when (alive == 1 && run_b == 1) {
                done_b = 1;
                ever_b = 1;
                run_b = 0;
                att_b = 0;
                park_b = 0;
            }
            action FailA when (alive == 1 && run_a == 1) {
                run_a = 0;
                att_a = 0;
                park_a = 0;
            }
            action FailB when (alive == 1 && run_b == 1) {
                run_b = 0;
                att_b = 0;
                park_b = 0;
            }
            action HangA when (alive == 1 && run_a == 1 && park_a == 0) {
                park_a = 1;
            }
            action HangB when (alive == 1 && run_b == 1 && park_b == 0) {
                park_b = 1;
            }
            action CheckAgain when (alive == 1 && park_a + park_b > 0) {
                park_a = 0;
                att_a = 0;
                park_b = 0;
                att_b = 0;
            }
            action Crash when (alive == 1) {
                alive = 0;
            }
            action Quit when (alive == 1) {
                quit_a = run_a;
                quit_b = run_b;
                alive = 0;
            }
            action Relaunch when (alive == 0) {
                park_a = if (run_a == 1 && quit_a == 0 && att_a > 1) { 1 } else { park_a };
                att_a = if (run_a == 1 && quit_a == 1 && att_a > 0) {
                    att_a - 1
                } else if (run_a == 1 && quit_a == 0 && att_a == 0) {
                    1
                } else {
                    att_a
                };
                run_a = 0;
                quit_a = 0;
                park_b = if (run_b == 1 && quit_b == 0 && att_b > 1) { 1 } else { park_b };
                att_b = if (run_b == 1 && quit_b == 1 && att_b > 0) {
                    att_b - 1
                } else if (run_b == 1 && quit_b == 0 && att_b == 0) {
                    1
                } else {
                    att_b
                };
                run_b = 0;
                quit_b = 0;
                alive = 1;
            }
            action ApplyKey when (alive == 1 && applied == 0 && kept == 0) {
                applied = 1;
            }
            action PersonChanges when (applied == 1) {
                applied = 0;
                kept = 1;
            }
            action UnattendedPass when (alive == 1 && applied + kept > 0) {
                kept = kept;
            }
            action Restore when (alive == 1 && applied + kept > 0) {
                applied = 0;
                kept = 0;
            }
            action GroupOn when (alive == 1 && kept == 1) {
                applied = 0;
                kept = 0;
            }

            // Each mutant fires where one defect would, and no further, so the mutated space
            // stays finite: a begin of a done stage, a relaunch that loses a done stage (and
            // with it everything recorded of the stage), and one that resumes a stage aterm
            // stopped in twice.
            action RerunDone when (Buggy == 1 && alive == 1 && done_a == 1 && run_a == 0 &&
                att_a == 0) {
                run_a = 1;
                att_a = att_a + 1;
            }
            action RelaunchForgetsDone when (Buggy == 1 && alive == 0 && ever_a == 1) {
                done_a = 0;
                run_a = 0;
                att_a = 0;
                park_a = 0;
                quit_a = 0;
                alive = 1;
            }
            action RelaunchNeverParks when (Buggy == 1 && alive == 0 && run_a == 1 &&
                quit_a == 0 && att_a == 2 && park_a == 0) {
                run_a = 0;
                alive = 1;
            }
            action ApplyKept when (Buggy == 1 && alive == 1 && kept == 1) {
                wrote_kept = 1;
            }

            invariant NotDoneAndRunning: done_a + run_a <= 1 && done_b + run_b <= 1;
            invariant CrashKeepsDone: ever_a <= done_a && ever_b <= done_b;
            invariant KeptNeverWritten: wrote_kept == 0;
            invariant TwoBeginsPark: (att_a <= 1 || run_a == 1 || park_a == 1) &&
                (att_b <= 1 || run_b == 1 || park_b == 1);
            invariant AtMostTwoBegins: att_a <= 2 && att_b <= 2;
            bound AttemptsBox: att_a <= 3 && att_b <= 3;
        }
    }
}

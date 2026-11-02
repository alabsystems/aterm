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
/// begun twice. A quit or an update's handoff is two steps: it notes the stages it would cut
/// off, then either goes (the process ends, and the next launch gives their begin back) or is
/// refused (a handoff that does not happen), which takes the notes back, so a later crash in
/// the same stage still counts; while it decides, no stage begins, so every stage it would cut
/// off is one it noted. A stage that has not ended within the hang limit parks while it
/// runs; one the process then leaves, however it leaves, counts that as one crash, so the next
/// launch resumes it once and parks it the second time. Check again clears every park and the
/// attempts. A key read back as aterm set it is recorded as applied; when a later read differs,
/// the person changed it, and no unattended pass writes it again until Restore or turning its
/// group on forgets that.
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
            var noting = 0;
            var applied = 0;
            var kept = 0;
            var wrote_kept = 0;

            action BeginA when (alive == 1 && noting == 0 && done_a == 0 && run_a == 0 &&
                park_a == 0) {
                run_a = 1;
                att_a = att_a + 1;
                quit_a = 0;
            }
            action BeginB when (alive == 1 && noting == 0 && done_b == 0 && run_b == 0 &&
                park_b == 0) {
                run_b = 1;
                att_b = att_b + 1;
                quit_b = 0;
            }
            action EndA when (alive == 1 && run_a == 1) {
                done_a = 1;
                ever_a = 1;
                run_a = 0;
                att_a = 0;
                park_a = 0;
                quit_a = 0;
            }
            action EndB when (alive == 1 && run_b == 1) {
                done_b = 1;
                ever_b = 1;
                run_b = 0;
                att_b = 0;
                park_b = 0;
                quit_b = 0;
            }
            action FailA when (alive == 1 && run_a == 1) {
                run_a = 0;
                att_a = 0;
                park_a = 0;
                quit_a = 0;
            }
            action FailB when (alive == 1 && run_b == 1) {
                run_b = 0;
                att_b = 0;
                park_b = 0;
                quit_b = 0;
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
                noting = 0;
            }
            // A quit, or an update's handoff, notes the stages it cuts off, then goes or is
            // refused. While it decides, the stages running go on (an end takes its note), and
            // none begins: a begin then would be cut off with no note, a stop counted as a crash.
            action NoteQuit when (alive == 1 && noting == 0) {
                quit_a = if (run_a == 1) { 1 } else { quit_a };
                quit_b = if (run_b == 1) { 1 } else { quit_b };
                noting = 1;
            }
            action CommitQuit when (alive == 1 && noting == 1) {
                alive = 0;
                noting = 0;
            }
            action RefuseQuit when (alive == 1 && noting == 1) {
                quit_a = 0;
                quit_b = 0;
                noting = 0;
            }
            // A stage still recorded running was cut off. One a quit cut off gives its begin
            // back, unless it had run past the hang limit (its park, set while it ran): that
            // counts as aterm stopping in it, as a crash does, and the park goes with it.
            action Relaunch when (alive == 0) {
                park_a = if (run_a == 1 && (quit_a == 0 || park_a == 1) && att_a > 1) {
                    1
                } else if (run_a == 1) {
                    0
                } else {
                    park_a
                };
                att_a = if (run_a == 1 && quit_a == 1 && park_a == 0 && att_a > 0) {
                    att_a - 1
                } else if (run_a == 1 && (quit_a == 0 || park_a == 1) && att_a == 0) {
                    1
                } else {
                    att_a
                };
                run_a = 0;
                quit_a = 0;
                park_b = if (run_b == 1 && (quit_b == 0 || park_b == 1) && att_b > 1) {
                    1
                } else if (run_b == 1) {
                    0
                } else {
                    park_b
                };
                att_b = if (run_b == 1 && quit_b == 1 && park_b == 0 && att_b > 0) {
                    att_b - 1
                } else if (run_b == 1 && (quit_b == 0 || park_b == 1) && att_b == 0) {
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
            // with it everything recorded of the stage), one that resumes a stage aterm
            // stopped in twice, a refused handoff that leaves its notes, and a begin while a
            // quit decides.
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
            action RefuseKeepsNote when (Buggy == 1 && alive == 1 && noting == 1) {
                noting = 0;
            }
            action BeginWhileQuitting when (Buggy == 1 && alive == 1 && noting == 1 &&
                done_a == 0 && run_a == 0 && park_a == 0) {
                run_a = 1;
                att_a = att_a + 1;
                quit_a = 0;
            }

            invariant NotDoneAndRunning: done_a + run_a <= 1 && done_b + run_b <= 1;
            invariant CrashKeepsDone: ever_a <= done_a && ever_b <= done_b;
            invariant KeptNeverWritten: wrote_kept == 0;
            invariant TwoBeginsPark: (att_a <= 1 || run_a == 1 || park_a == 1) &&
                (att_b <= 1 || run_b == 1 || park_b == 1);
            invariant AtMostTwoBegins: att_a <= 2 && att_b <= 2;
            // A note of a quit stands only while that quit decides, or once it went: a refused
            // handoff leaves none, which would give a later crash's begin back.
            invariant QuitMarksOnlyInAQuit: quit_a + quit_b == 0 || noting == 1 || alive == 0;
            // Every stage running while a quit decides is one its note names, so the quit that
            // goes counts no stage it cut off as a crash.
            invariant QuitNotesEveryRunningStage: noting == 0 ||
                ((run_a == 0 || quit_a == 1) && (run_b == 0 || quit_b == 1));
            bound AttemptsBox: att_a <= 3 && att_b <= 3;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal models of the setup run's ledger (aterm-cli's `macos_doctor::initial::ledger`) and of
//! the record of the keys that changed after aterm set them (`::kept`).

use aterm_spec::derive::*;

/// SetupLedger: two stages of the first-launch setup run (two, so the running record is a set,
/// as the concurrent lanes need) and the process that runs them ending at any point. The keys
/// that changed after aterm set them are [`setup_kept_key_model`]'s: no action of either
/// reads the other's state, so one product of the two would only multiply their spaces.
///
/// A stage begins only when it is not done, not running and not parked, and each begin counts
/// an attempt. The process ending leaves the stages it was in recorded as running with the
/// profile lock free; the next launch resumes each of them once and parks one that had already
/// begun twice. That launch comes by itself: while the run is unfinished its login job
/// (`trigger`) is armed, written before any record that owes a pass and removed only once the
/// run finished, and launchd starts it at the next login and after any end but its own, so
/// resuming waits for nobody; a person's launch (`Relaunch`) resumes it the same way, at any time.
/// A park is a pause, never a hold: every park comes due by itself (`due`, the
/// ledger's pause, which grows with each park of the stage), and a begin once it has is the
/// stage's last before another stop parks it again; nothing but a person's Check again lifts a
/// park sooner. A quit or an update's handoff is two steps: it notes the stages it would cut
/// off, then either goes (the process ends, and the next launch gives their begin back) or is
/// refused (a handoff that does not happen), which takes the notes back, so a later crash in
/// the same stage still counts; while it decides, no stage begins, so every stage it would cut
/// off is one it noted. A restart of the Mac (or a logout) ends the process however far its
/// quit got, and is no crash either: the next launch, in another boot, gives the begin of each
/// stage it cut off back, as for a quit. A stage that has not ended within the hang limit parks while it
/// runs; one the process then leaves, however it leaves, counts that as one crash, so the next
/// launch resumes it once and parks it the second time. Check again, a person's press, clears
/// every park and the attempts. After dead work is recovered, a valid software identity adopts
/// a legacy ledger once, and strictly newer software gives unfinished stages a fresh budget,
/// its park, pause and counted stops gone with it. The model keeps only whether each stage
/// recorded an identity: every NewSoftware input is a freshly proved higher build with a
/// different full commit, not another observation of the same installed software. Unknown,
/// same, older and otherwise ineligible identities leave the budget unchanged. None of these
/// transitions cuts off a live stage or a quit, or changes finished progress.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn setup_ledger_model() -> Model {
    aterm_spec::ty_model! {
        SetupLedger {
            const Buggy = 0;

            var alive = 1;
            var software_known_a = 0;
            var software_known_b = 0;
            var done_a = 0;
            var run_a = 0;
            var att_a = 0;
            var park_a = 0;
            var due_a = 0;
            var ever_a = 0;
            var quit_a = 0;
            var done_b = 0;
            var run_b = 0;
            var att_b = 0;
            var park_b = 0;
            var due_b = 0;
            var ever_b = 0;
            var quit_b = 0;
            var noting = 0;
            var rebooted = 0;
            var trigger = 1;
            var strike_a = 0;
            var strike_b = 0;
            // The defect a `Buggy=1` run carries (0 none yet, else the mutant's number): one per
            // run, as the conformance plants one at a time, so the mutants' spaces add rather
            // than multiply.
            var mutated = 0;

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
            // A parked stage's pause is over: its begin lifts the park, counted as the one begin
            // left before another stop parks it again, and runs it, with no press.
            action BeginDueA when (alive == 1 && noting == 0 && done_a == 0 && run_a == 0 &&
                park_a == 1 && due_a == 1) {
                run_a = 1;
                att_a = 2;
                park_a = 0;
                due_a = 0;
                quit_a = 0;
                strike_a = 1;
            }
            action BeginDueB when (alive == 1 && noting == 0 && done_b == 0 && run_b == 0 &&
                park_b == 1 && due_b == 1) {
                run_b = 1;
                att_b = 2;
                park_b = 0;
                due_b = 0;
                quit_b = 0;
                strike_b = 1;
            }
            // The run's last stage to end finishes it, which removes its login job.
            action EndA when (alive == 1 && run_a == 1) {
                trigger = if (done_b == 1) { 0 } else { trigger };
                done_a = 1;
                ever_a = 1;
                run_a = 0;
                att_a = 0;
                park_a = 0;
                due_a = 0;
                quit_a = 0;
                strike_a = 0;
            }
            action EndB when (alive == 1 && run_b == 1) {
                trigger = if (done_a == 1) { 0 } else { trigger };
                done_b = 1;
                ever_b = 1;
                run_b = 0;
                att_b = 0;
                park_b = 0;
                due_b = 0;
                quit_b = 0;
                strike_b = 0;
            }
            action FailA when (alive == 1 && run_a == 1) {
                run_a = 0;
                att_a = 0;
                park_a = 0;
                due_a = 0;
                quit_a = 0;
                strike_a = 0;
            }
            action FailB when (alive == 1 && run_b == 1) {
                run_b = 0;
                att_b = 0;
                park_b = 0;
                due_b = 0;
                quit_b = 0;
                strike_b = 0;
            }
            // A stage past the hang limit parks while it runs: a stop aterm counts, which the
            // process leaving it after adds nothing to.
            action HangA when (alive == 1 && run_a == 1 && park_a == 0) {
                park_a = 1;
                due_a = 1;
                strike_a = if (strike_a <= 1) { strike_a + 1 } else { 2 };
            }
            action HangB when (alive == 1 && run_b == 1 && park_b == 0) {
                park_b = 1;
                due_b = 1;
                strike_b = if (strike_b <= 1) { strike_b + 1 } else { 2 };
            }
            action CheckAgain when (alive == 1 && park_a + park_b > 0) {
                park_a = 0;
                due_a = 0;
                att_a = 0;
                park_b = 0;
                due_b = 0;
                att_b = 0;
                strike_a = 0;
                strike_b = 0;
            }
            // A legacy stage receives one valid installed identity. Real identities and
            // their ordering are native inputs; each bit remembers that adoption was spent.
            // Adoption also requires an actually unattributed legacy record: unread record
            // identity is a native refusal input, not evidence for either positive action.
            // A pass owning only A cannot consume B's software change or its fresh budget.
            action AdoptSoftwareA when (alive == 1 && noting == 0 && done_a == 0 && run_a == 0 &&
                software_known_a == 0) {
                software_known_a = 1;
                park_a = 0;
                due_a = 0;
                att_a = 0;
                strike_a = 0;
            }
            action AdoptSoftwareB when (alive == 1 && noting == 0 && done_b == 0 && run_b == 0 &&
                software_known_b == 0) {
                software_known_b = 1;
                park_b = 0;
                due_b = 0;
                att_b = 0;
                strike_b = 0;
            }
            // Each firing has a distinct, strictly newer native witness. The quotient need
            // not grow a build counter to permit any number of genuine software changes.
            action NewSoftwareA when (alive == 1 && noting == 0 && done_a == 0 && run_a == 0 &&
                software_known_a == 1) {
                park_a = 0;
                due_a = 0;
                att_a = 0;
                strike_a = 0;
            }
            action NewSoftwareB when (alive == 1 && noting == 0 && done_b == 0 && run_b == 0 &&
                software_known_b == 1) {
                park_b = 0;
                due_b = 0;
                att_b = 0;
                strike_b = 0;
            }
            // Native refusal inputs include unreadable identity, the recorded identity,
            // older builds, an equal build with another commit and a newer build of the
            // recorded commit. None supplies the fresh fact NewSoftware requires.
            action RetainSoftwareA when (alive == 1 && noting == 0 && done_a == 0 && run_a == 0) {
                software_known_a = software_known_a;
            }
            action RetainSoftwareB when (alive == 1 && noting == 0 && done_b == 0 && run_b == 0) {
                software_known_b = software_known_b;
            }
            // The process ends in the stages it runs: each counts it as aterm stopping in it,
            // but one its hang already parked, whose stop that park counted.
            action Crash when (alive == 1) {
                alive = 0;
                noting = 0;
                strike_a = if (run_a == 1 && park_a == 0 && strike_a <= 1) { strike_a + 1 } else {
                    strike_a
                };
                strike_b = if (run_b == 1 && park_b == 0 && strike_b <= 1) { strike_b + 1 } else {
                    strike_b
                };
            }
            // The Mac restarts, or its account logs out, whether aterm runs (its quit, if any,
            // cut short) or ended already: the next launch is in another boot.
            action Reboot when (rebooted == 0) {
                alive = 0;
                noting = 0;
                rebooted = 1;
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
            // A stage still recorded running was cut off. One a quit or a restart cut off gives
            // its begin back, unless it had run past the hang limit (its park, set while it
            // ran): that counts as aterm stopping in it, as a crash does, and the park goes with
            // it. Each park it sets has its pause. The launch is the login job's (or a window's)
            // at the next login or after the end: launchd starts the job while it is armed.
            action Login when (alive == 0 && trigger == 1) {
                park_a = if (run_a == 1 && ((quit_a == 0 && rebooted == 0) || park_a == 1) &&
                    att_a > 1) {
                    1
                } else if (run_a == 1) {
                    0
                } else {
                    park_a
                };
                due_a = if (run_a == 1 && ((quit_a == 0 && rebooted == 0) || park_a == 1) &&
                    att_a > 1) {
                    1
                } else if (run_a == 1) {
                    0
                } else {
                    due_a
                };
                att_a = if (run_a == 1 && (quit_a == 1 || rebooted == 1) && park_a == 0 &&
                    att_a > 0) {
                    att_a - 1
                } else if (run_a == 1 && ((quit_a == 0 && rebooted == 0) || park_a == 1) &&
                    att_a == 0) {
                    1
                } else {
                    att_a
                };
                run_a = 0;
                quit_a = 0;
                park_b = if (run_b == 1 && ((quit_b == 0 && rebooted == 0) || park_b == 1) &&
                    att_b > 1) {
                    1
                } else if (run_b == 1) {
                    0
                } else {
                    park_b
                };
                due_b = if (run_b == 1 && ((quit_b == 0 && rebooted == 0) || park_b == 1) &&
                    att_b > 1) {
                    1
                } else if (run_b == 1) {
                    0
                } else {
                    due_b
                };
                att_b = if (run_b == 1 && (quit_b == 1 || rebooted == 1) && park_b == 0 &&
                    att_b > 0) {
                    att_b - 1
                } else if (run_b == 1 && ((quit_b == 0 && rebooted == 0) || park_b == 1) &&
                    att_b == 0) {
                    1
                } else {
                    att_b
                };
                run_b = 0;
                quit_b = 0;
                rebooted = 0;
                alive = 1;
            }
            // A person opens aterm (or macOS reopens its windows at login), armed or not: it
            // resumes the run exactly as the login job does.
            action Relaunch when (alive == 0) {
                park_a = if (run_a == 1 && ((quit_a == 0 && rebooted == 0) || park_a == 1) &&
                    att_a > 1) {
                    1
                } else if (run_a == 1) {
                    0
                } else {
                    park_a
                };
                due_a = if (run_a == 1 && ((quit_a == 0 && rebooted == 0) || park_a == 1) &&
                    att_a > 1) {
                    1
                } else if (run_a == 1) {
                    0
                } else {
                    due_a
                };
                att_a = if (run_a == 1 && (quit_a == 1 || rebooted == 1) && park_a == 0 &&
                    att_a > 0) {
                    att_a - 1
                } else if (run_a == 1 && ((quit_a == 0 && rebooted == 0) || park_a == 1) &&
                    att_a == 0) {
                    1
                } else {
                    att_a
                };
                run_a = 0;
                quit_a = 0;
                park_b = if (run_b == 1 && ((quit_b == 0 && rebooted == 0) || park_b == 1) &&
                    att_b > 1) {
                    1
                } else if (run_b == 1) {
                    0
                } else {
                    park_b
                };
                due_b = if (run_b == 1 && ((quit_b == 0 && rebooted == 0) || park_b == 1) &&
                    att_b > 1) {
                    1
                } else if (run_b == 1) {
                    0
                } else {
                    due_b
                };
                att_b = if (run_b == 1 && (quit_b == 1 || rebooted == 1) && park_b == 0 &&
                    att_b > 0) {
                    att_b - 1
                } else if (run_b == 1 && ((quit_b == 0 && rebooted == 0) || park_b == 1) &&
                    att_b == 0) {
                    1
                } else {
                    att_b
                };
                run_b = 0;
                quit_b = 0;
                rebooted = 0;
                alive = 1;
            }
            // Each mutant fires where one defect would, and no further, so the mutated space
            // stays finite: a begin of a done stage, a relaunch that loses a done stage (and
            // with it everything recorded of the stage), one that resumes a stage aterm
            // stopped in twice, one that parks it for good (no pause, so only a press lifts
            // it), one that counts a restart that cut a stage off as aterm stopping in it, a
            // refused handoff that leaves its notes, a begin while a quit decides, and a login
            // job removed while the run still owes a pass (so nothing resumes it).
            action RerunDone when (Buggy == 1 && (mutated == 0 || mutated == 1) && alive == 1 &&
                done_a == 1 && run_a == 0 && att_a == 0) {
                mutated = 1;
                run_a = 1;
                att_a = att_a + 1;
            }
            action RelaunchForgetsDone when (Buggy == 1 && (mutated == 0 || mutated == 2) &&
                alive == 0 && ever_a == 1) {
                mutated = 2;
                done_a = 0;
                run_a = 0;
                att_a = 0;
                park_a = 0;
                due_a = 0;
                quit_a = 0;
                alive = 1;
            }
            action RelaunchNeverParks when (Buggy == 1 && (mutated == 0 || mutated == 3) &&
                alive == 0 && run_a == 1 && quit_a == 0 && att_a == 2 && park_a == 0) {
                mutated = 3;
                run_a = 0;
                alive = 1;
            }
            action RelaunchParksForGood when (Buggy == 1 && (mutated == 0 || mutated == 4) &&
                alive == 0 && run_a == 1 && quit_a == 0 && att_a == 2 && park_a == 0) {
                mutated = 4;
                run_a = 0;
                park_a = 1;
                alive = 1;
            }
            action RelaunchCountsRestart when (Buggy == 1 && (mutated == 0 || mutated == 5) &&
                alive == 0 && rebooted == 1 && run_a == 1 && quit_a == 0 && park_a == 0 &&
                run_b == 0) {
                mutated = 5;
                park_a = if (att_a > 1) { 1 } else { 0 };
                due_a = if (att_a > 1) { 1 } else { 0 };
                att_a = if (att_a == 0) { 1 } else { att_a };
                run_a = 0;
                rebooted = 0;
                alive = 1;
            }
            action RefuseKeepsNote when (Buggy == 1 && (mutated == 0 || mutated == 6) &&
                alive == 1 && noting == 1) {
                mutated = 6;
                noting = 0;
            }
            action BeginWhileQuitting when (Buggy == 1 && (mutated == 0 || mutated == 7) &&
                alive == 1 && noting == 1 && done_a == 0 && run_a == 0 && park_a == 0) {
                mutated = 7;
                run_a = 1;
                att_a = att_a + 1;
                quit_a = 0;
            }
            action DisarmWhileOwed when (Buggy == 1 && (mutated == 0 || mutated == 8) &&
                alive == 1 && trigger == 1 && done_a + done_b <= 1) {
                mutated = 8;
                trigger = 0;
            }

            invariant NotDoneAndRunning: done_a + run_a <= 1 && done_b + run_b <= 1;
            invariant CrashKeepsDone: ever_a <= done_a && ever_b <= done_b;
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
            // Every park has its pause, so it comes due by itself: none waits for a press.
            invariant EveryParkComesDue: park_a <= due_a && park_b <= due_b;
            // An unfinished run keeps its login job, so whatever ends aterm, it resumes by itself.
            invariant UnfinishedRunArmed: (done_a == 1 && done_b == 1) || trigger == 1;
            // A stage that is not running parks only once aterm stopped in it twice (a crash
            // while it ran, or its hang), never for a restart of the Mac or a quit, which give
            // the begin back (`strike_a`, `strike_b`: the stops since the stage's last end, fail,
            // press or fresh budget, at most two, a due begin counting as one, as the last begin
            // before another stop parks it); a stage that runs parks only by its hang.
            invariant ParksNeedTwoStops: (park_a == 0 || run_a == 1 || strike_a > 1) &&
                (park_b == 0 || run_b == 1 || strike_b > 1);
            bound AttemptsBox: att_a <= 3 && att_b <= 3;
        }
    }
}

/// SetupKeptKey: one preference aterm sets during setup, which something other than aterm may
/// change after aterm set it and read it back (a sign-out, whose first change after aterm did it
/// is kept, since only a person signs in, is not this model's). A key read back as aterm set it
/// is recorded as applied. Nothing records who changes it after that (the person, an app, an
/// installer, macOS): the first unattended pass that reads it otherwise sets it again, as it sets
/// any key not as wanted, and only a change after that (something holds it otherwise) keeps it,
/// so that no unattended pass writes it again until Restore or turning its group on forgets
/// that. The record is durable, so a process ending at any point leaves it as its last save
/// wrote it (the conformance walk reads it back after every step). It is a model of its own,
/// beside [`setup_ledger_model`]: none of its actions reads a stage's state, nor any stage
/// action its.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn setup_kept_key_model() -> Model {
    aterm_spec::ty_model! {
        SetupKeptKey {
            const Buggy = 0;

            var applied = 0;
            var again = 0;
            var differs = 0;
            var kept = 0;
            var wrote_kept = 0;

            action ApplyKey when (applied == 0 && kept == 0) {
                applied = 1;
                again = 0;
                differs = 0;
            }
            // Something other than aterm changes the key: nothing says who.
            action OtherWriterChanges when (applied == 1 && differs == 0) {
                differs = 1;
            }
            // The first pass that reads it otherwise sets it again, as any key not as wanted.
            action ApplyAgain when (applied == 1 && differs == 1 && again == 0) {
                differs = 0;
                again = 1;
            }
            // Changed again after aterm set it again: something holds it otherwise, and it is
            // left as it is.
            action KeepChanged when (applied == 1 && differs == 1 && again == 1) {
                applied = 0;
                kept = 1;
            }
            // A pass with nothing new to note: a kept key left as it is, or one as wanted.
            action UnattendedPass when (kept == 1 || (applied == 1 && differs == 0)) {
                kept = kept;
            }
            action Restore when (applied + kept > 0) {
                applied = 0;
                again = 0;
                differs = 0;
                kept = 0;
            }
            action GroupOn when (kept == 1) {
                applied = 0;
                again = 0;
                differs = 0;
                kept = 0;
            }
            // Each mutant fires where one defect would: a pass that writes a kept key, which then
            // reads back as aterm set it, afresh, and one that keeps the first change it reads.
            action ApplyKept when (Buggy == 1 && kept == 1) {
                wrote_kept = 1;
                kept = 0;
                applied = 1;
                again = 0;
                differs = 0;
            }
            action KeepAnyDrift when (Buggy == 1 && applied == 1 && differs == 1 && again == 0) {
                applied = 0;
                kept = 1;
            }

            invariant KeptNeverWritten: wrote_kept == 0;
            // A key is kept only once aterm set it again after a change: one change, which
            // nothing attributes, is set again, never left as someone's choice.
            invariant KeptOnlyAfterSetAgain: kept <= again;
        }
    }
}

/// Every park is lifted by itself: `[]<>` stage A, where it is not running, is not parked, with
/// no press (stage B's is the same by symmetry; a goal over both at once does not hold, as
/// crashes may park the two in turn for good, each lifted while the other waits). The parked
/// stage's due begin is strongly fair (aterm's quits and crashes may keep taking it away, but
/// it is offered again at each launch), the login job's launch after aterm ended and the quit
/// that noted its stages weakly fair (launchd starts the armed job at the next login and after
/// any end but its own, and a quit that started goes or is refused); a person's own launch
/// (`Relaunch`) is not assumed, so the run's parks lift with nobody at the Mac. Every park comes
/// due by itself, so a press only lifts one sooner. A relaunch that parks the stage with no pause
/// (`RelaunchParksForGood`) leaves only a person's Check again to lift it, which nothing fair
/// does, and a login job removed while the run still owes a pass (`DisarmWhileOwed`) leaves the
/// stage parked once aterm ends, with nothing to launch it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(setup_ledger_model)]
pub fn setup_ledger_liveness() -> Liveness {
    Liveness {
        name: "EveryParkLifts",
        goal: or_(eq(var("park_a"), int(0)), eq(var("run_a"), int(1))),
        weak: vec!["Login", "CommitQuit"],
        strong: vec!["BeginDueA"],
        mutants: vec!["RelaunchParksForGood", "DisarmWhileOwed"],
    }
}

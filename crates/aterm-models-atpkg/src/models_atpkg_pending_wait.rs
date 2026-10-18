// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pending stub waiting for its program on a person's terminal: the READER of
//! `<prefix>/pass-owed.json`, `<prefix>/progress.json` and the program's shim
//! (`atpkg::cli::wait_then_run`) against the WRITERS, the lane that promised a pass and
//! the pass installing that program.

use aterm_spec::derive::*;

/// One program a pass is installing, and the stub a person typed its name into.
/// Writer: `owed` whether a pass is PROMISED and not yet planned (the window's seed writes
/// the promise before it lays a stub, the update takes it over and retires it once its plan
/// lands or it ends; it may also lapse — the window died, the deadline passed — and never
/// comes back), `begun` whether the promised pass began, `running` whether the pass is live
/// (it can end, or die, at any moment; a pass that ends before it planned the program, the
/// promise still owed — a set completion that stopped before it could carry the vendors —
/// is followed by the pass that lands them alone, which plans afresh), `planned` what its plan says
/// of the program (0 no plan yet, 1 planned, 2 left out — a name the signed index does not
/// serve here), `row` the program's phase in the pass (0 queued, 1 downloading, 2 set up —
/// its shim laid, 3 done, 4 failed), `shim` whether the real shim of the name typed resolves
/// (laid with the set-up phase, never removed), `bare` whether the program set up without
/// that name (a command its build does not expose: no shim for it is ever laid). Reader:
/// `step` is where the stub's poll is (0 about to read
/// the promise, 1 about to read the pass, 2 deciding, 3 done), `seen` what its first read
/// captured, `coming` what the two reads together say (a pass promised or live), `left`
/// whether they say the pass's plan left the program out (the promise gone, the pass live
/// with no row for it), `done` whether the pass read said the program's row is done, `stub`
/// what it did (0 still waiting, 1 ran the program, 2 said it failed or is not coming).
///
/// The stub reads the PROMISE first, whether the pass is RUNNING (and its row) second and
/// whether the shim resolves third, and decides: run it when the shim resolves; say it
/// failed when the pass recorded a failure, when neither read said a pass is coming, when
/// the plan left it out, or when its row read done (the program is set up and no shim of the
/// name stands); else wait and poll again. That order is the whole contract, because
/// each writer retires its sign only after the next one is up: the promise only once the
/// pass planned (or ended), the pass only once it laid every shim it ever will, the row's
/// done only once the program's shims are laid. A promise
/// seen gone was kept or broken for good, a live pass seen after it has planned — so no row
/// there is a plan without the program — a row seen done has every shim it ever will, and a
/// pass seen ended has laid every shim it ever will. A promise that lapsed is broken: a pass that starts after it is a new one, not the
/// one promised. The promise outlives a pass that ended without planning the program for as
/// long as another pass of the same update is to plan it, so a stub reads no gap between
/// the two.
///
/// `Buggy=1` is the ways a reader gets this wrong, one per invariant. It reads the other way
/// round — the shim first, then the pass, then the promise — so a pass that sets the program
/// up and ends between the reads makes the stub say "the install stopped" about a program
/// that is installed (`NoFalseFailure`), and a pass that begins and takes the promise over
/// between them makes it say so about a pass that is running (`NoFailureWhileComing`), as
/// does a pass read before its plan with the promise read gone after it: a plan that HAS
/// the program, taken for one that left it out. A program that sets up and finishes between
/// its shim read and its row read is taken for one that finished without the name
/// (`NoFalseFailure`). And it takes a pass that ended without
/// recording a failure for an install, running a program whose pass died before laying it
/// (`RunsOnlyInstalled`). `Buggy=2` is the writer's mistake the promise's hand-over exists
/// to avoid: retiring it while the pass that keeps it runs unplanned (when the set completion
/// stops, before the vendors' plan lands), so a stub reads a live pass with no row and the
/// promise gone and says the program is not coming, just before the plan brings it
/// (`NoFailureWhileComing`). Tier-1 (`atpkg::cli`'s tests) drives the real decision
/// (`pending_wait_step`) over every deciding state, with a reader that asks "ended?" before
/// "installed?" as the caught negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_pending_wait_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgPendingWait {
            const Buggy = 0;
            var owed = 1;
            var begun = 0;
            var running = 0;
            var planned = 0;
            var row = 0;
            var shim = 0;
            var bare = 0;
            var step = 0;
            var seen = 0;
            var coming = 0;
            var left = 0;
            var done = 0;
            var stub = 0;

            // The writers: the promised pass begins and plans, with the program or without
            // it; the promise is retired once the pass planned or ended, or lapses before any
            // pass began.
            action Begin when (owed == 1 && begun == 0) {
                begun = 1;
                running = 1;
            }
            action Plan when (running == 1 && planned == 0) {
                planned = 1;
            }
            action PlanWithout when (running == 1 && planned == 0) {
                planned = 2;
            }
            action Retire when (
                owed == 1 && begun == 1 && (planned > 0 || running == 0 || Buggy == 2)
            ) {
                owed = 0;
            }
            action Lapse when (owed == 1 && begun == 0) {
                owed = 0;
            }
            // A pass that ended before it planned the program, the promise still owed, is
            // followed by the one that lands it alone.
            action Rebegin when (owed == 1 && begun == 1 && running == 0 && (planned == 0 || planned == 2)) {
                running = 1;
                planned = 0;
            }
            // The pass moves a planned program on, lays its shim when it sets it up, may fail
            // it before then, and ends (or dies) whenever.
            action Download when (running == 1 && planned == 1 && row == 0) {
                row = 1;
            }
            action SetUp when (running == 1 && row == 1) {
                row = 2;
                shim = 1;
            }
            // …or sets it up without the name typed: its build exposes no such command.
            action SetUpWithout when (running == 1 && row == 1) {
                row = 2;
                bare = 1;
            }
            action Finish when (running == 1 && row == 2) {
                row = 3;
            }
            action FailRow when (running == 1 && planned == 1 && row <= 1) {
                row = 4;
            }
            action End when (running == 1) {
                running = 0;
            }

            // The reader's first read: the promise (the shim, when buggy).
            action ObserveOwed when (step == 0 && stub == 0) {
                seen = if Buggy == 1 { shim } else { owed };
                step = 1;
            }
            // Its second: whether the pass runs, which with a promise seen says a pass is
            // coming (the pass alone, when buggy), and whether it runs with no row for the
            // program — a plan that left it out once the promise was seen gone (when buggy,
            // whatever the promise, which it reads last) — and whether the program's row is
            // done.
            action ObservePass when (step == 1) {
                coming = if Buggy == 1 { running } else { if seen == 1 { 1 } else { running } };
                left = if running == 1 && (planned == 0 || planned == 2) && (Buggy == 1 || seen == 0) {
                    1
                } else {
                    0
                };
                done = if row == 3 { 1 } else { 0 };
                step = 2;
            }
            // The decision, over what was read and the last read (the shim; the promise,
            // when buggy).
            action DecideRun when (
                step == 2
                    && (((Buggy == 0 || Buggy == 2) && shim == 1)
                        || (Buggy == 1 && (seen == 1 || (coming == 0 && owed == 0 && row <= 3))))
            ) {
                stub = 1;
                step = 3;
            }
            action DecideFail when (
                step == 2
                    && (((Buggy == 0 || Buggy == 2)
                        && shim == 0
                        && (row == 4 || coming == 0 || left == 1 || done == 1))
                        || (Buggy == 1
                            && seen == 0
                            && (row == 4
                                || (coming == 0 && owed == 0)
                                || (left == 1 && owed == 0)
                                || done == 1)))
            ) {
                stub = 2;
                step = 3;
            }
            action DecideWait when (
                step == 2
                    && (((Buggy == 0 || Buggy == 2)
                        && shim == 0
                        && row <= 3
                        && coming == 1
                        && left == 0
                        && done == 0)
                        || (Buggy == 1
                            && seen == 0
                            && row <= 3
                            && done == 0
                            && (coming == 1 || owed == 1)
                            && (left == 0 || owed == 1)))
            ) {
                step = 0;
            }

            // A failure is said only about a program that is not installed, or whose
            // install failed.
            invariant NoFalseFailure: stub <= 1 || shim == 0 || row == 4;
            // …and only once nothing is bringing it: the pass ended, the promise was broken
            // before any pass began, the pass's plan left the program out, or the program set
            // up without the name.
            invariant NoFailureWhileComing:
                stub <= 1
                    || row == 4
                    || planned == 2
                    || bare == 1
                    || (running == 0 && (begun == 1 || owed == 0));
            // A program is run only once it is installed.
            invariant RunsOnlyInstalled: stub == 0 || stub == 2 || shim == 1;
        }
    }
}

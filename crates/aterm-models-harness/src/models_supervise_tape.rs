// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS TAPE'S RECOVERY (the replay plan's §6.4 and its model 1): what a reader may make of the bytes a tape's writer left behind — whole, cut short by a crash, or damaged at rest.

use aterm_spec::derive::Model;

/// One tape and its one reading (`aterm-agent/src/supervise/tape/frame.rs`'s `TapeWriter` and `recover.rs`'s `recover`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervise_tape_recovery_model() -> Model {
    aterm_spec::ty_model! {
        SuperviseTapeRecovery {
            const Buggy = 0;
            const MaxFrames = 3;
            var written = 0;
            var torn = 0;
            var rot = 0;
            var magic_rot = 0;
            var misplaced = 0;
            var flipped = 0;
            var crashed = 0;
            var done = 0;
            var recovered = 0;
            var refused = 0;
            var said_torn = 0;

            action Write when (crashed == 0 && written <= MaxFrames - 1) {
                written = written + 1;
            }
            action Tear when (crashed == 0) {
                torn = 1;
                crashed = 1;
            }
            action Crash when (crashed == 0) {
                crashed = 1;
            }
            action Rot when (crashed == 1 && done == 0 && flipped == 0 && written > 1) {
                rot = 1;
                flipped = 1;
            }
            action RotLast when (crashed == 1 && done == 0 && flipped == 0 && written > 0) {
                written = written - 1;
                torn = 1;
                flipped = 1;
            }
            action RotMagic when (crashed == 1 && done == 0 && flipped == 0 && written > 0) {
                magic_rot = 1;
                flipped = 1;
            }
            action Misplace when (crashed == 1 && done == 0 && flipped == 0 && torn == 0 && written > 0) {
                misplaced = 1;
                flipped = 1;
            }
            action Recover when (crashed == 1 && done == 0) {
                done = 1;
                refused = if magic_rot == 1 && Buggy == 5 {
                    0
                } else if magic_rot == 1 {
                    1
                } else if misplaced == 1 && Buggy == 6 {
                    0
                } else if misplaced == 1 {
                    1
                } else if rot == 1 && (Buggy == 1 || Buggy == 3) {
                    0
                } else {
                    rot
                };
                recovered = if magic_rot == 1 && Buggy == 5 {
                    written
                } else if magic_rot == 1 {
                    0
                } else if misplaced == 1 && Buggy == 6 {
                    written
                } else if misplaced == 1 {
                    0
                } else if rot == 1 && Buggy == 1 {
                    written
                } else if rot == 1 && Buggy == 3 {
                    written - 1
                } else if rot == 1 {
                    0
                } else if Buggy == 2 {
                    written + torn
                } else {
                    written
                };
                said_torn = if magic_rot == 1 && Buggy == 5 {
                    torn
                } else if magic_rot == 1 {
                    0
                } else if misplaced == 1 {
                    0
                } else if Buggy == 4 {
                    0
                } else if rot == 1 && (Buggy == 1 || Buggy == 3) {
                    torn
                } else if rot == 1 {
                    0
                } else {
                    torn
                };
            }

            invariant RecoveredIsPrefix: recovered <= written;
            invariant NoSilentLoss: done == 0 || refused == 1 || recovered == written;
            invariant RotRefused: done == 0 || (rot == 0 && magic_rot == 0) || refused == 1;
            invariant MisplacedRefused: done == 0 || misplaced == 0 || refused == 1;
            invariant TornSaid: done == 0 || refused == 1 || said_torn == torn;
        }
    }
}

/// A REPLAY'S CURSOR (the replay plan's §5.2 and its model 2, both arms): what a replay of a harness tape serves the loop it re-runs, and when it stops serving.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervise_replay_cursor_model() -> Model {
    aterm_spec::ty_model! {
        SuperviseReplayCursor {
            const Buggy = 0;
            const Acts = 0;
            scenario Acts = 1;
            const Records = 4;
            var pos = 0;
            var diverged = 0;
            var live = 0;
            var skipped_act = 0;
            var served_after = 0;
            var served_future = 0;

            action AskNext when (diverged == 0 && pos <= Records - 1) {
                pos = pos + 1;
            }
            action AskOtherRead when (diverged == 0 && pos <= Records - 1) {
                skipped_act = if Buggy == 2 && pos <= 1 { 1 } else { skipped_act };
                served_future = if Buggy == 4 && pos <= 1 { 1 } else { served_future };
                pos = if Buggy == 2 && pos <= 1 { 3 } else { pos };
                diverged = if (Buggy == 2 || Buggy == 4) && pos <= 1 { 0 } else { 1 };
            }
            action AskRepeat when (diverged == 0 && (pos == 1 || pos == 3)) {
                diverged = if Acts == 1 { 0 } else { 1 };
            }
            action AskClosingAct when (diverged == 0 && (pos == 0 || pos == 2)) {
                pos = if Acts == 1 { pos + 2 } else { pos };
                diverged = if Acts == 1 { 0 } else { 1 };
            }
            action AskOtherAct when (diverged == 0 && pos <= Records - 1) {
                diverged = 1;
            }
            action AskPastEnd when (diverged == 0 && pos == Records && live == 0) {
                live = if Buggy == 1 { live + 1 } else { live };
                diverged = if Buggy == 1 { 0 } else { 1 };
            }
            action AskAfterDivergence when (diverged == 1 && served_after <= 1) {
                served_after = if Buggy == 3 { served_after + 1 } else { served_after };
            }

            invariant NoLiveEffect: live == 0;
            invariant NoActSkipped: skipped_act == 0;
            invariant NothingServedAfterDivergence: served_after == 0;
            invariant NothingFromTheFuture: served_future == 0;
        }
    }
}

/// THE STOP FLAG'S LATCH ON A TAPE (the replay plan's model 3): a flag another thread raises — the hosted loop's stop — recorded so that its replay cannot diverge.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_raised_latch_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorRaisedLatch {
            const Buggy = 0;
            const Ops = 4;
            var up = 0;
            var marked = 0;
            var mark = 0;
            var pos = 0;
            var rec = 0;
            var rpos = 0;
            var diverged = 0;
            var done = 0;

            action Raise when (rec == 0 && up == 0) {
                up = 1;
                mark = if Buggy == 1 { pos } else { mark };
                marked = if Buggy == 1 { 1 } else { marked };
            }
            action LoopOp when (rec == 0 && pos <= Ops - 1) {
                mark = if Buggy == 0 && up == 1 && marked == 0 && (pos == 0 || pos == 2) {
                    pos
                } else {
                    mark
                };
                marked = if Buggy == 0 && up == 1 && marked == 0 && (pos == 0 || pos == 2) {
                    1
                } else {
                    marked
                };
                pos = pos + 1;
            }
            action EndRecord when (rec == 0 && pos == Ops) {
                rec = 1;
            }
            action ReplayOp when (rec == 1 && rpos <= Ops - 1 && diverged == 0) {
                diverged = if marked == 1 && mark == rpos && (rpos == 1 || rpos == 3) { 1 } else { 0 };
                rpos = rpos + 1;
            }
            action ReplayEnd when (rec == 1 && rpos == Ops && done == 0) {
                diverged = if marked == 1 && mark == Ops { 1 } else { diverged };
                done = 1;
            }

            invariant TapedByTheRead: marked == 0 || mark == 0 || mark == 2;
            invariant NoSpuriousDivergence: diverged == 0;
        }
    }
}

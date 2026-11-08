// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// Cancellation polling across owner loss, failed storage and source closure.
/// Availability is a due poll with healthy storage; process closure is a separate
/// environment event. Neither is assumed to follow from time or owner loss.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exec_settlement_model() -> Model {
    aterm_spec::ty_model! {
        ExecCancellationSettlement {
            const Buggy = 0;
            // NoGo, prospective marker, Go, disk Consumed, live Consumed.
            var case = 0; var failed = 0; var gone = 0; var polled = 0;
            var pending = 0; var ack = 0; var custody = 1; var cancelled = 0;
            var native_consumed = 0; var marker = 0; var late = 0;
            var done = 0; var terminate = 0;
            action NextCase when (polled == 0 && pending == 0 && case <= 3 && done == 0) {
                case = case + 1;
                marker = 1;
                native_consumed = if case == 3 {1} else {0};
                custody = if case == 3 {0} else {1};
            }
            action Fail when (failed == 0 && ack == 0 && done == 0) {
                failed = 1; polled = 0;
            }
            action Recover when (failed == 1 && done == 0) {
                failed = 0; polled = 0;
            }
            action SourceGone when (gone == 0 && done == 0) { gone = 1; }
            action Poll when (done == 0) {
                polled = 1;
                cancelled = if case == 4 || Buggy == 3 {0} else {1};
                pending = if case == 4 || Buggy == 3 || Buggy == 8 {0} else {1};
                ack = if case == 4 || Buggy == 3 {0}
                    else if Buggy == 1 || ack == 1 || (failed == 0 && (Buggy <= 8 || Buggy > 9)) {1} else {0};
                custody = if gone == 1 || case == 4 || (Buggy == 10 && case == 1) {0} else {1};
                native_consumed = if case == 4 || (Buggy == 5 && case == 3) {1} else {0};
                marker = if Buggy == 4 {0} else if case > 0 {1} else {0};
                terminate = if Buggy == 6 && case == 1 {1}
                    else if (case == 2 || case == 3) && gone == 0 {1} else {0};
                late = if Buggy == 7 && case <= 3 {1} else {0};
                done = if case == 4 || (Buggy == 2 && gone == 1) {1}
                    else if Buggy == 3 {0}
                    else if gone == 1 && (Buggy == 1 || ack == 1 || (failed == 0 && (Buggy <= 8 || Buggy > 9))) {1}
                    else {0};
            }
            invariant DurableAck: ack == 0 || failed == 0;
            invariant PendingRetained: cancelled == 0 || pending == 1;
            invariant NoPrematureFinish: done == 0 || case == 4
                || (ack == 1 && custody == 0);
            invariant AdmissionClosed: polled == 0 || case == 4 || cancelled == 1;
            invariant NoPhantomConsumption: native_consumed == if case == 4 {1} else {0};
            invariant AttemptHistory: marker == if case > 0 {1} else {0};
            invariant TerminationNeedsGo: terminate == 0 || case == 2 || case == 3;
            invariant NoLateAdmission: late == 0;
            invariant Available: polled == 0 || case == 4 || failed == 1 || ack == 1;
            invariant CustodyUntilClosure: gone == 1 || case == 4 || custody == 1;
            bound Finite: 0 <= case && case <= 4 && 0 <= failed && failed <= 1
                && 0 <= gone && gone <= 1 && 0 <= polled && polled <= 1
                && 0 <= pending && pending <= 1 && 0 <= ack && ack <= 1
                && 0 <= custody && custody <= 1 && 0 <= cancelled && cancelled <= 1
                && 0 <= native_consumed && native_consumed <= 1
                && 0 <= marker && marker <= 1 && 0 <= late && late <= 1
                && 0 <= done && done <= 1 && 0 <= terminate && terminate <= 1;
        }
    }
}

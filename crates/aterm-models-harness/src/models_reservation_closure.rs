// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// Closing storage preserves execution possibility across every committed prefix.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn reservation_closure_model() -> Model {
    aterm_spec::ty_model! {
        ReservationClosure {
            const Buggy = 0;
            var complete = 0; var legacy = 0; var custody = 1; var exact = 1;
            var ack = 0; var ready = 0;
            action NoExec when (complete == 0 && custody == 1 && exact == 1) {
                complete = 1; ack = 0; ready = 0;
            }
            action EmptySeal when (complete == 0 && custody == 1 && exact == 1) {
                complete = 2; ack = 0; ready = 0;
            }
            action ExecutedSeal when (complete == 0 && custody == 1 && exact == 1) {
                complete = 3; ack = 0; ready = 0;
            }
            action Prefix when (complete == 3 && legacy == 0 && custody == 1 && exact == 1) {
                legacy = 1; ack = 0; ready = 0;
            }
            action LoseCustody when (custody == 1) { custody = 0; ack = 0; ready = 0; }
            action WrongBinding when (exact == 1) { exact = 0; ack = 0; ready = 0; }
            action Close {
                ready = if complete > 0 && custody == 1 && exact == 1 {1} else {0};
                ack = if (complete > 0 || Buggy == 4) && (custody == 1 || Buggy == 3)
                    && (exact == 1 || Buggy == 5) {if Buggy == 2 && legacy == 1 {0} else {1}} else {0};
                legacy = if (complete > 0 || Buggy == 4) && (custody == 1 || Buggy == 3)
                    && (exact == 1 || Buggy == 5) {
                        if Buggy == 2 && legacy == 1 {legacy} else {
                            if complete == 1 || complete == 0 || Buggy == 1 {2}
                            else {if complete == 2 {3} else {4}}
                        }
                    } else {legacy};
            }
            invariant NoFalseNoExec: legacy <= 1 || legacy > 2 || complete == 1;
            invariant MatchingTerminal: legacy <= 1 || legacy == complete + 1;
            invariant TerminalBeforeAck: ack == 0 || complete > 0;
            invariant CustodyBeforeAck: ack == 0 || custody == 1;
            invariant ExactBeforeAck: ack == 0 || exact == 1;
            invariant ResumePrefix: ready == 0 || ack == 1;
            bound Finite: 0 <= complete && complete <= 3 && 0 <= legacy && legacy <= 4
                && 0 <= custody && custody <= 1 && 0 <= exact && exact <= 1
                && 0 <= ack && ack <= 1 && 0 <= ready && ready <= 1;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A complete recipient history shares four record slots with unrelated terminal
/// records. Legacy history is never promoted to complete history. A rollback may
/// be the first possible grant, but a forward grant cannot follow it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn recipient_journal_model() -> Model {
    aterm_spec::ty_model! {
        RecipientJournal {
            const Buggy = 0;
            var mode = 0; var state = 0; var forward = 0; var rollback = 0;
            var records = 0; var reserved = 0; var other = 0;
            var closed = 0; var legacy = 0; var downgraded = 0; var complete = 0;
            action Prepare when ((mode == 0 && records + other + 4 <= 4)
                || (Buggy == 6 && mode == 1)) {
                mode = 2; state = 1; records = records + 1; reserved = 3;
            }
            action Forward when (mode == 2 && (state == 1
                || (Buggy == 1 && state == 3) || (Buggy == 8 && state == 4))) {
                state = 2; forward = 1; records = records + 1;
                reserved = if Buggy == 5 {0} else {2};
            }
            action Rollback when (mode == 2 && (state == 1 || state == 2
                || (Buggy == 2 && state == 3))) {
                state = 3; rollback = 1; records = records + 1; reserved = 1;
            }
            action NoExec when (mode == 2 && (state == 1 || (Buggy == 3 && state == 2))) {
                state = 6; records = records + 1; reserved = 0; closed = 1;
            }
            action Seal when (mode == 2 && 1 <= state && state <= 3) {
                state = if state == 1 {4} else {5}; records = records + 1;
                reserved = 0; closed = 1;
            }
            action Other when (other == 0 && records + reserved + 1 <= 4) { other = 1; }
            action Legacy when (mode == 0 && records + other + 1 <= 4) {
                mode = 1; legacy = 1; records = 1;
            }
            action LegacyWrite when (mode == 2 && downgraded == 0) {
                downgraded = if Buggy == 4 {1} else {0};
            }
            action Read {
                complete = if mode == 2 || (Buggy == 7 && mode == 1) {1} else {0};
            }
            invariant Capacity: records + reserved + other <= 4;
            invariant Counted: records == (if mode == 0 {0} else {if mode == 1 {1}
                else {1 + forward + rollback + (if 4 <= state {1} else {0})}});
            invariant NoExecNeverGranted: state <= 5 || (forward == 0 && rollback == 0);
            invariant ForwardBeforeRollback: rollback == 0 || state <= 1 || 3 <= state;
            invariant NoDowngrade: downgraded == 0;
            invariant LegacyNeverComplete: legacy == 0 || mode == 1;
            invariant ReadOnlyComplete: complete == 0 || mode == 2;
            invariant Terminal: closed == 0 || 4 <= state;
            invariant Finite: 0 <= mode && mode <= 2 && 0 <= state && state <= 6
                && 0 <= forward && forward <= 1 && 0 <= rollback && rollback <= 1
                && 0 <= records && records <= 5 && 0 <= reserved && reserved <= 3
                && 0 <= other && other <= 1 && 0 <= closed && closed <= 1
                && 0 <= legacy && legacy <= 1 && 0 <= downgraded && downgraded <= 1
                && 0 <= complete && complete <= 1;
        }
    }
}

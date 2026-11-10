// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// An independent binding precedes recipient history and retains its remaining budget.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn bound_recipient_journal_model() -> Model {
    aterm_spec::ty_model! {
        BoundRecipientJournal {
            const Buggy = 0;
            var bound = 0; var phase = 0; var matches = 1;
            var records = 0; var reserved = 0; var ack = 0; var ready = 0; var seen = 0;
            action Bind when (bound == 0 && phase == 0) {
                bound = 1; records = records + 1;
                reserved = if Buggy == 3 {0} else {4}; ack = 0; ready = 0;
            }
            action Record {
                ready = if bound == 1 && matches == 1 && phase <= 1 {1} else {0};
                ack = if (bound == 1 || Buggy == 1) && (matches == 1 || Buggy == 2)
                    && phase <= 1 {if Buggy == 6 {0} else {1}} else {0};
                phase = if (bound == 1 || Buggy == 1) && (matches == 1 || Buggy == 2)
                    && phase <= 1 && (Buggy <= 5 || Buggy > 6) {phase + 1} else {phase};
                records = if (bound == 1 || Buggy == 1) && (matches == 1 || Buggy == 2)
                    && phase <= 1 && (Buggy <= 5 || Buggy > 6) {if Buggy == 7 {records} else {records + 1}} else {records};
                reserved = if (bound == 1 || Buggy == 1) && (matches == 1 || Buggy == 2)
                    && phase <= 1 && (Buggy <= 5 || Buggy > 6) {if phase == 0 {if Buggy == 4 {4} else {3}} else {0}} else {reserved};
            }
            action WrongBinding when (matches == 1) {
                matches = 0; ack = 0; ready = 0; seen = 0;
            }
            action Read {
                seen = if matches == 1 && (phase > 0 || (Buggy == 5 && bound == 1)) {1} else {0};
                ack = 0; ready = 0;
            }
            invariant BindingFirst: phase == 0 || bound == 1;
            invariant AckExact: ack == 0 || (bound == 1 && matches == 1 && phase > 0);
            invariant Progress: ready == 0 || ack == 1;
            invariant Counted: records == bound + phase;
            invariant Reserved: reserved == (if bound == 0 || phase == 2 {0} else {if phase == 0 {4} else {3}});
            invariant ReadExact: seen == 0 || (phase > 0 && matches == 1);
            bound Finite: 0 <= bound && bound <= 1 && 0 <= phase && phase <= 2
                && 0 <= matches && matches <= 1 && 0 <= records && records <= 3
                && 0 <= reserved && reserved <= 4 && 0 <= ack && ack <= 1
                && 0 <= ready && ready <= 1 && 0 <= seen && seen <= 1;
        }
    }
}

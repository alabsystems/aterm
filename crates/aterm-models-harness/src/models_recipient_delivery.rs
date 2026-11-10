// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// One phase's callback follows durable history and is never replayed on reopen.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn recipient_delivery_model() -> Model {
    aterm_spec::ty_model! {
        RecipientDelivery {
            const Buggy = 0;
            var open = 1; var recorded = 0; var ever = 0; var intact = 1;
            var calls = 0; var replayed = 0; var ack = 0; var ready = 0; var seen = 0;
            action Send when (open == 1) {
                ready = if recorded == 0 && intact == 1 {1} else {0};
                ack = if (recorded == 0 || Buggy == 3) && (intact == 1 || Buggy == 7) {if Buggy == 5 {0} else {1}} else {0};
                recorded = if Buggy == 1 || Buggy == 5 {recorded} else {if intact == 1 || Buggy == 7 {1} else {recorded}};
                ever = if Buggy == 5 {ever} else {if intact == 1 || Buggy == 7 {1} else {ever}};
                calls = if (recorded == 0 || Buggy == 3) && (intact == 1 || Buggy == 7) && (Buggy <= 4 || Buggy > 5) {if calls == 0 {1} else {2}} else {calls};
            }
            action LostAck when (open == 1 && recorded == 0 && intact == 1) {
                recorded = 1; ever = 1; ack = 0; ready = 0;
            }
            action Crash when (open == 1) {
                open = 0; recorded = if Buggy == 4 {0} else {recorded}; ack = 0; ready = 0; seen = 0;
            }
            action Reopen when (open == 0) {
                open = intact; calls = if Buggy == 2 && recorded == 1 {if calls == 0 {1} else {2}} else {calls};
                replayed = if Buggy == 2 && recorded == 1 {1} else {replayed}; ack = 0; ready = 0;
            }
            action LoseCustody when (intact == 1) { intact = 0; ack = 0; ready = 0; }
            action Read { seen = if Buggy == 6 {1} else {recorded}; ack = 0; ready = 0; }
            invariant RecordedBeforeCallback: calls == 0 || recorded == 1;
            invariant AtMostOnce: calls <= 1;
            invariant NoRecoveryCallback: replayed == 0;
            invariant HistorySticky: ever == 0 || recorded == 1;
            invariant Progress: ready == 0 || ack == 1;
            invariant ReadOnlyRecorded: seen == 0 || recorded == 1;
            invariant CustodyBeforeAck: ack == 0 || intact == 1;
            bound Finite: 0 <= open && open <= 1 && 0 <= recorded && recorded <= 1
                && 0 <= ever && ever <= 1 && 0 <= calls && calls <= 2 && 0 <= replayed && replayed <= 1
                && 0 <= ack && ack <= 1 && 0 <= ready && ready <= 1 && 0 <= seen && seen <= 1
                && 0 <= intact && intact <= 1;
        }
    }
}

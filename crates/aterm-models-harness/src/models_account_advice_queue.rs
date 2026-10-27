// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Advice publication has one current owner; claiming preserves another queued decision.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_advice_queue_model() -> Model {
    aterm_spec::ty_model! {
        AccountAdviceQueue {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var returned = 0; var queued = 0; var taken = 0; var usable = 0; var cancelled = 0;
            action AnotherCase when (phase == 0 && case <= 20) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Apply when (phase == 1) {
                phase = 2;
                returned = if (Buggy == 4 && case == 6) || (Buggy == 8 && case == 14) || (Buggy == 11 && case == 16) || (Buggy == 16 && case == 21) {1}
                    else {if Buggy == 9 && case == 15 {0}
                    else {if case == 1 || (case > 6 && case <= 13) || case == 15 || (case > 17 && case <= 20) {1} else {0}}};
                queued = if (Buggy == 1 && case == 2) || (Buggy == 3 && case == 4)
                    || (Buggy == 4 && case == 6) || (Buggy == 7 && case == 13)
                    || (Buggy == 8 && case == 14) || (Buggy == 10 && case == 17)
                    || (Buggy == 11 && case == 16) || (Buggy == 13 && case == 19)
                    || (Buggy == 14 && case == 20) || (Buggy == 16 && case == 21) {1}
                    else {if (Buggy == 2 && case == 3) || (Buggy == 5 && case == 8)
                    || (Buggy == 6 && case == 10) || (Buggy == 9 && case == 15)
                    || (Buggy == 12 && case == 12) {0}
                    else {if case == 2 || case == 3 || case == 5 || case == 10 || case == 14 {2}
                    else {if case == 1 || (case > 6 && case <= 9) || case == 11 || case == 12 || case == 15 || case == 18 {1} else {0}}}};
                taken = if (Buggy == 5 && case == 8) || (Buggy == 6 && case == 10) || (Buggy == 12 && case == 12) {1}
                    else {if (Buggy == 7 && case == 13) || (Buggy == 13 && case == 19) || (Buggy == 14 && case == 20) {0}
                    else {if case == 13 || case == 19 || case == 20 {1} else {0}}};
                usable = if Buggy == 15 && case == 18 {1}
                    else {if case <= 3 || case == 5 || (case > 6 && case <= 10) || case == 14 || case == 15 {1} else {0}};
                cancelled = if Buggy == 17 && case == 20 {0} else {if case == 20 {1} else {0}};
            }
            invariant QueueIdentity: phase <= 1 || queued ==
                if case == 2 || case == 3 || case == 5 || case == 10 || case == 14 {2}
                else {if case == 1 || (case > 6 && case <= 9) || case == 11 || case == 12 || case == 15 || case == 18 {1} else {0}};
            invariant CurrentPublication: phase <= 1 || returned ==
                if case == 1 || (case > 6 && case <= 13) || case == 15 || (case > 17 && case <= 20) {1} else {0};
            invariant CurrentClaim: phase <= 1 || taken ==
                if case == 13 || case == 19 || case == 20 {1} else {0};
            invariant QueueContext: phase <= 1 || usable ==
                if case <= 3 || case == 5 || (case > 6 && case <= 10) || case == 14 || case == 15 {1} else {0};
            invariant ClaimInvalidatesProducer: phase <= 1 || cancelled == if case == 20 {1} else {0};
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A captured account observation can edit local state only while its context is current.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_local_observation_model() -> Model {
    aterm_spec::ty_model! {
        AccountLocalObservation {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var calls = 0; var changed = 0; var returned = 0;
            action AnotherCase when (phase == 0 && case <= 7) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Apply when (phase == 1) {
                phase = 2;
                calls = if Buggy == 1 && case == 2 {1}
                    else {if Buggy == 4 && case == 1 {2}
                    else {if case == 1 || case == 6 || case == 7 {1} else {0}}};
                changed = if Buggy == 2 && case == 3 {1}
                    else {if case == 1 || case == 6 || case == 7 {1} else {0}};
                returned = if Buggy == 3 && case == 4 {1}
                    else {if case == 1 || case == 6 || case == 7 {1} else {0}};
            }
            invariant CurrentCalls: phase <= 1 || calls ==
                if case == 1 || case == 6 || case == 7 {1} else {0};
            invariant GuardedEdit: phase <= 1 || changed ==
                if case == 1 || case == 6 || case == 7 {1} else {0};
            invariant HonestReturn: phase <= 1 || returned ==
                if case == 1 || case == 6 || case == 7 {1} else {0};
            invariant AtMostOnce: calls <= 1;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A finite reservation and priority machine, including repeated passes and
/// rejected overlapping reservations. Only admitted work changes priority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn preparation_order_model() -> Model {
    aterm_spec::ty_model! {
        PreparationOrder {
            const Buggy = 0;
            var active = 0; var next = 0; var answer = 2;
            var last = 2; var op = 0;
            action Begin when (active == 0) {
                active = 1; op = 1;
                answer = if Buggy == 4 {1 - next} else {next};
                next = if Buggy == 5 {1 - next} else {next};
            }
            action Busy when (active == 1) {
                op = 2; answer = if Buggy == 6 {next} else {2};
            }
            action Native when (active == 1) {
                op = 3; last = 0; next = if Buggy == 1 {0} else {1};
            }
            action Shared when (active == 1) {
                op = 4; last = 1; next = if Buggy == 2 {1} else {0};
            }
            action End when (active == 1) {
                op = 5; answer = 2; active = if Buggy == 3 {1} else {0};
                next = if Buggy == 7 {1 - next} else {next};
            }
            invariant PriorityFollowsAdmission: (last == 2 && next == 0) || (last == 0 && next == 1) || (last == 1 && next == 0);
            invariant ReservationOffersPriority: 1 > op || op > 1 || (active == 1 && answer == next);
            invariant BusyDoesNotReserveAgain: 2 > op || op > 2 || (active == 1 && answer == 2);
            invariant ReleaseEndsReservation: 5 > op || op > 5 || active == 0;
        }
    }
}

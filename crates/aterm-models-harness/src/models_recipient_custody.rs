// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// Pre-recipe enrollment and exact legacy-reservation binding; no execution grant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn recipient_custody_model() -> Model {
    aterm_spec::ty_model! {
        RecipientCustody {
            const Buggy = 0;
            var phase = 0; var journal = 0; var reservation = 0;
            var identity = 1; var generation = 1; var bound = 0;
            var enroll_ack = 0; var bind_ack = 0;
            var enroll_ready = 0; var bind_ready = 0;
            action Enroll {
                enroll_ready = if (phase == 0 && journal == 0) || phase == 1 || (phase == 2 && journal == 1) {1} else {0};
                enroll_ack = if Buggy == 1 {1} else {
                    if (phase == 0 && journal == 0) || phase == 1 || (phase == 2 && journal == 1)
                    {if Buggy == 7 && phase == 1 {0} else {1}} else {0}
                };
                phase = if (phase == 0 && journal == 0) || phase == 1 || (phase == 2 && journal == 1) {2} else {phase};
                journal = if (phase == 0 && journal == 0) || phase == 1 || (phase == 2 && journal == 1) {1} else {journal};
                bind_ack = 0; bind_ready = 0;
            }
            action Bind {
                bind_ready = if phase == 2 && journal == 1 && reservation == 1 && identity == 1 && generation == 1 {1} else {0};
                bind_ack = if (phase == 2 || Buggy == 2) && journal == 1
                    && (reservation == 1 || Buggy == 3) && (identity == 1 || Buggy == 4)
                    && (generation == 1 || Buggy == 5) {if Buggy == 6 {0} else {1}} else {0};
                bound = if (phase == 2 || Buggy == 2) && journal == 1
                    && (reservation == 1 || Buggy == 3) && (identity == 1 || Buggy == 4)
                    && (generation == 1 || Buggy == 5) {if Buggy == 8 {bound} else {1}} else {bound};
                enroll_ack = 0; enroll_ready = 0;
            }
            action Pending when (phase == 0 && journal == 0) {
                phase = 1; enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0;
            }
            action Journal when (phase == 1 && journal == 0) {
                journal = 1; enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0;
            }
            action Reserve when (phase == 2 && journal == 1 && reservation == 0 && bound == 0 && identity == 1) {
                reservation = 1; enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0;
            }
            action LoseJournal when (phase == 2 && journal == 1) {
                journal = 0; enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0;
            }
            action LoseEnrollment when (phase == 2 && journal == 1) {
                phase = 0; enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0;
            }
            action ForgetAll when (phase == 2) {
                phase = 0; journal = 0; identity = 0; bound = 0; generation = 1;
                enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0;
            }
            action WrongStamp when (identity == 1) {
                identity = 0; enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0;
            }
            action LoseReservation when (bound == 1 && reservation == 1) {
                reservation = 0; enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0;
            }
            action ReplaceReservation when (bound == 1 && reservation == 1 && generation == 1) {
                generation = 0; enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0;
            }
            action Crash { enroll_ack = 0; bind_ack = 0; enroll_ready = 0; bind_ready = 0; }
            invariant EnrollSound: enroll_ack == 0 || (phase == 2 && journal == 1);
            invariant BindSound: bind_ack == 0 || (phase == 2 && journal == 1);
            invariant ReservationRequired: bind_ack == 0 || reservation == 1;
            invariant IdentityRequired: bind_ack == 0 || identity == 1;
            invariant GenerationRequired: bind_ack == 0 || generation == 1;
            invariant EnrollmentProgress: enroll_ready == 0 || enroll_ack == 1;
            invariant BindingProgress: bind_ready == 0 || bind_ack == 1;
            invariant BindingRecorded: bind_ack == 0 || bound == 1;
            bound Finite: 0 <= phase && phase <= 2 && 0 <= journal && journal <= 1
                && 0 <= reservation && reservation <= 1 && 0 <= identity && identity <= 1
                && 0 <= generation && generation <= 1 && 0 <= bound && bound <= 1
                && 0 <= enroll_ack && enroll_ack <= 1 && 0 <= bind_ack && bind_ack <= 1
                && 0 <= enroll_ready && enroll_ready <= 1 && 0 <= bind_ready && bind_ready <= 1;
        }
    }
}

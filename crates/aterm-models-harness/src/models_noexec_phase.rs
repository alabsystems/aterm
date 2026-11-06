// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Phase-bearing closure observations, exact retries and legacy non-inference.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn noexec_phase_model() -> Model {
    aterm_spec::ty_model! {
        NoExecPhase {
            const Buggy = 0;
            var disk = 0; var ack = 0; var attempted = 0; var retried = 0;
            var failed = 0; var conflict = 0; var read = 0;
            var accepted_conflict = 0; var inferred_legacy = 0; var blocked = 0;
            action Close when (disk == 0 && attempted == 0) { disk = 1; ack = 1; }
            action Legacy when (disk == 0 && attempted == 0) { disk = 2; }
            action Refused when (disk == 0 && attempted == 0) {
                attempted = 1; failed = 1;
                ack = if Buggy == 3 {1} else {0};
            }
            action LostReply when (disk == 0 && attempted == 0) {
                disk = 1; attempted = 1;
            }
            action Retry when (attempted == 1 && retried == 0 && ack == 0) {
                retried = 1; failed = 0;
                disk = if Buggy == 4 {disk} else {1};
                ack = if Buggy == 4 {0} else {1};
                blocked = if Buggy == 4 {1} else {0};
            }
            action Conflict when (disk == 1 && conflict == 0) {
                conflict = 1;
                accepted_conflict = if Buggy == 1 {1} else {0};
            }
            action Read when (disk > 0 && read == 0) {
                read = 1;
                inferred_legacy = if disk == 2 && Buggy == 2 {1} else {0};
            }
            invariant ExactPhase: accepted_conflict == 0;
            invariant LegacyUnknown: inferred_legacy == 0;
            invariant NoFailedAcknowledgement: failed == 0 || ack == 0;
            invariant RetryProgress: blocked == 0;
            bound Finite: 0 <= disk && disk <= 2 && 0 <= ack && ack <= 1
                && 0 <= attempted && attempted <= 1 && 0 <= retried && retried <= 1
                && 0 <= failed && failed <= 1 && 0 <= conflict && conflict <= 1
                && 0 <= read && read <= 1 && 0 <= accepted_conflict && accepted_conflict <= 1
                && 0 <= inferred_legacy && inferred_legacy <= 1 && 0 <= blocked && blocked <= 1;
        }
    }
}

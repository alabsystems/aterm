// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Readiness for a typed Claim acknowledgement, distinct from journal history.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_claim_readiness_model() -> Model {
    aterm_spec::ty_model! {
        AccountClaimReadiness {
            const Buggy = 0;
            // lifecycle: Started, Queued, closed NoExec, closed unknown,
            // closed unknown with a recovered durable terminal classification.
            // state: Prepared, NoExec, MayExecute, Sealed.
            var lifecycle = 0; var state = 0;
            var identity = 1; var published = 1; var writer = 0; var confirmed = 1;
            var current = 1; var cohort = 1; var phase = 1; var granted = 0;
            var dispatched = 0; var drained = 1;
            var checked = 0; var ack = 0;
            action Queue when (checked == 0 && lifecycle == 0) { lifecycle = 1; }
            action CloseNoExec when (checked == 0 && lifecycle == 0) { lifecycle = 2; }
            action CloseUnknown when (checked == 0 && lifecycle == 0) { lifecycle = 3; }
            action ReadNoExec when (checked == 0 && state == 0) { state = 1; }
            action ReadMay when (checked == 0 && state == 0) { state = 2; }
            action ReadSealed when (checked == 0 && state == 0) { state = 3; }
            action WrongIdentity when (checked == 0 && identity == 1) { identity = 0; }
            action Unpublished when (checked == 0 && published == 1) { published = 0; }
            action WriterActive when (checked == 0 && writer == 0) { writer = 1; }
            action Unconfirmed when (checked == 0 && confirmed == 1 && lifecycle <= 3) { confirmed = 0; }
            action Revoke when (checked == 0 && current == 1) { current = 0; }
            action CohortUnknown when (checked == 0 && cohort == 1) { cohort = 0; }
            action WrongPhase when (checked == 0 && phase == 1) { phase = 0; }
            action Grant when (checked == 0 && granted == 0) { granted = 1; }
            action Dispatch when (checked == 0 && dispatched == 0) { dispatched = 1; }
            action PendingDrain when (checked == 0 && drained == 1) { drained = 0; }
            action RecoverTerminal when (checked == 0 && lifecycle == 0 && state > 1 && confirmed == 1) { lifecycle = 4; }
            action Check when (checked == 0) {
                checked = 1;
                ack = if (published == 1 || Buggy == 1) && (writer == 0 || Buggy == 2)
                    && (identity == 1 || Buggy == 3) && (confirmed == 1 || Buggy == 14)
                    && ((state == 0 && (lifecycle == 0 || Buggy == 4) && (current == 1 || Buggy == 5)
                        && (cohort == 1 || Buggy == 6) && (granted == 0 || Buggy == 7)
                        && (dispatched == 0 || Buggy == 15))
                    || (state == 1 && (lifecycle == 2 || Buggy == 8) && (cohort == 1 || Buggy == 9)
                        && (phase == 1 || Buggy == 10) && (granted == 0 || Buggy == 11)
                        && (drained == 1 || Buggy == 16))
                    || (state == 2 && (lifecycle == 0 || lifecycle > 1 || Buggy == 17)
                        && (lifecycle <= 1 || lifecycle > 2 || Buggy == 18)
                        && (granted == 1 || lifecycle == 4 || Buggy == 19))
                    || (state == 3 && (lifecycle > 1 || Buggy == 20)
                        && (lifecycle <= 1 || lifecycle > 2 || Buggy == 21) && (drained == 1 || Buggy == 22))) {
                        if Buggy == 12 || (Buggy == 13 && state > 1) {0} else {1}
                    } else {0};
            }
            invariant Published: ack == 0 || published == 1;
            invariant WriterFinished: ack == 0 || writer == 0;
            invariant ExactIdentity: ack == 0 || identity == 1;
            invariant Confirmed: ack == 0 || confirmed == 1;
            invariant PreparedReady: ack == 0 || state > 0
                || (lifecycle == 0 && current == 1 && cohort == 1 && granted == 0 && dispatched == 0);
            invariant NoExecProven: ack == 0 || state <= 0 || state > 1
                || (lifecycle == 2 && cohort == 1 && phase == 1 && granted == 0 && drained == 1);
            invariant MayConsistent: ack == 0 || state <= 1 || state > 2
                || ((lifecycle == 0 || lifecycle > 1) && (lifecycle <= 1 || lifecycle > 2) && (granted == 1 || lifecycle == 4));
            invariant SealedConsistent: ack == 0 || state <= 2
                || (lifecycle > 2 && drained == 1);
            invariant ReadyAvailable: checked == 0 || published == 0 || writer == 1
                || identity == 0 || confirmed == 0
                || (state == 0 && (lifecycle > 0 || current == 0 || cohort == 0 || granted == 1 || dispatched == 1))
                || (state == 1 && (lifecycle <= 1 || lifecycle > 2 || cohort == 0 || phase == 0 || granted == 1 || drained == 0))
                || (state == 2 && (lifecycle == 1 || lifecycle == 2 || (granted == 0 && lifecycle <= 3)))
                || (state == 3 && (lifecycle <= 2 || drained == 0))
                || ack == 1;
            bound Finite: 0 <= lifecycle && lifecycle <= 4 && 0 <= state && state <= 3
                && 0 <= identity && identity <= 1 && 0 <= published && published <= 1
                && 0 <= writer && writer <= 1 && 0 <= confirmed && confirmed <= 1
                && 0 <= current && current <= 1 && 0 <= cohort && cohort <= 1
                && 0 <= phase && phase <= 1 && 0 <= granted && granted <= 1
                && 0 <= dispatched && dispatched <= 1 && 0 <= drained && drained <= 1
                && 0 <= checked && checked <= 1 && 0 <= ack && ack <= 1;
        }
    }
}

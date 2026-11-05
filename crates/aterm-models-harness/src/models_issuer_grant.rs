// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One operation, two append attempts and an old attempt's delayed cleanup.
/// Kernel quiescence is supplied positively to Close and ProbeReplacement;
/// external process lifetimes and storage durability have separate models.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn issuer_grant_model() -> Model {
    aterm_spec::ty_model! {
        IssuerGrant {
            const Buggy = 0;
            // record: absent, writing, uncertain, acknowledged.
            // phase: open, closed NoExec, closed Unknown.
            var record = 0; var attempt = 0; var phase = 0;
            var valid = 1; var granted = 0; var cleanup = 0;
            var bad_grant = 0; var bad_cleanup = 0; var bad_close = 0;
            var probed = 0; var unsafe_replace = 0;
            action Reserve when (phase == 0 && valid == 1 && attempt <= 1 && (record == 0 || record == 2)) {
                record = 1; attempt = attempt + 1;
            }
            action Ack when (record == 1) { record = 3; }
            action Fail when (record == 1) { record = 2; }
            action Invalidate when (valid == 1) { valid = 0; }
            action Grant when (phase == 0) {
                granted = if (record == 3 && valid == 1) || Buggy == 2 || (record == 3 && Buggy == 4) {1} else {granted};
                bad_grant = if (Buggy == 2 && record <= 2) || (record == 3 && valid == 0 && Buggy == 4) {1} else {bad_grant};
            }
            action OldCleanup when (attempt == 2 && cleanup == 0) {
                record = if Buggy == 5 {2} else {record};
                bad_cleanup = if Buggy == 5 && (record == 1 || record == 3) {1} else {0};
                cleanup = 1;
            }
            action Close when (phase == 0 && granted == 0) {
                phase = if record == 0 || Buggy == 1 {1} else {if record == 2 || (record == 3 && granted == 0) || Buggy == 6 {2} else {0}};
                bad_close = if record == 1 && (Buggy == 1 || Buggy == 6) {1} else {0};
            }
            action ProbeReplacement when (phase == 0 && probed == 0) {
                record = if attempt > 0 && Buggy == 3 {0} else {record};
                probed = 1;
                unsafe_replace = if attempt > 0 && Buggy == 3 {1} else {0};
            }
            invariant NoExecBeforeAttempt: phase <= 0 || phase > 1 || attempt == 0;
            invariant GrantNeedsAcknowledgementAndRevalidation: bad_grant == 0;
            invariant OldCleanupCannotChangeNewAttempt: bad_cleanup == 0;
            invariant AppendCannotClose: bad_close == 0;
            invariant ReplacementPreservesUncertainty: unsafe_replace == 0;
            invariant EvidencePersists: attempt == 0 || record > 0;
            bound Finite: 0 <= record && record <= 3 && 0 <= attempt && attempt <= 2
                && 0 <= phase && phase <= 2 && 0 <= valid && valid <= 1 && 0 <= granted && granted <= 1
                && 0 <= cleanup && cleanup <= 1 && 0 <= bad_grant && bad_grant <= 1
                && 0 <= bad_cleanup && bad_cleanup <= 1 && 0 <= bad_close && bad_close <= 1
                && 0 <= probed && probed <= 1 && 0 <= unsafe_replace && unsafe_replace <= 1;
        }
    }
}

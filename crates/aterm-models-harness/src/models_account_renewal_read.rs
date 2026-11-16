// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Renewal scheduling retains one recovered publication. A later change may
/// invalidate worker admission, but cannot rewrite the captured candidate.
///
/// `evidence` is what credential-free files show of the signed-out row's own login: 0 nothing,
/// 1 it served or was signed in after its recorded sign-out, 2 a reading not after that
/// sign-out. A login shown live after its sign-out is no candidate for a sign-in; evidence that
/// is not newer hides nothing.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_renewal_read_model() -> Model {
    aterm_spec::ty_model! {
        AccountRenewalRead {
            const Buggy = 0;
            var phase = 0; var provider = 1; var fault = 0; var later = 0; var evidence = 0;
            var available = 0; var admitted = 0; var retained = 1; var candidate = 0;
            action AnotherProvider when (phase == 0 && evidence == 0 && 4 > provider) { provider = provider + 1; }
            action AnotherFault when (phase == 0 && evidence == 0 && 6 > fault) { fault = fault + 1; }
            action LaterChange when (phase == 0 && evidence == 0 && later == 0) { later = 1; }
            action LoginEvidence when (phase == 0 && provider == 1 && fault == 0 && later == 0 && evidence <= 1) {
                evidence = evidence + 1;
            }
            action Read when (phase == 0) {
                phase = 1;
                available = if 2 > fault {
                    if Buggy == 1 {0} else {1}
                } else { if Buggy == 2 {1} else {0} };
                admitted = if 2 > fault && later == 0 {
                    if Buggy == 3 {0} else {1}
                } else { if Buggy == 4 {1} else {0} };
                retained = if Buggy == 5 {0} else {1};
                candidate = if 2 > fault && evidence == 1 {
                    if Buggy == 6 {1} else {0}
                } else if 2 > fault {
                    if Buggy == 7 && evidence == 2 {0} else {1}
                } else {0};
            }
            invariant RecoveredCandidatesAvailable: phase == 0 || fault > 1 || available == 1;
            invariant UnknownCandidatesRefused: phase == 0 || 2 > fault || available == 0;
            invariant CapturedPolicyAdmissible: phase == 0 || fault > 1 || later == 1 || admitted == 1;
            invariant ChangedPolicyRefused: phase == 0 || (2 > fault && later == 0) || admitted == 0;
            invariant CapturedRankingRetained: retained == 1;
            invariant LiveLoginIsNoCandidate: phase == 0 || evidence == 0 || evidence == 2 || candidate == 0;
            invariant SignedOutLoginStaysCandidate: phase == 0 || fault > 1 || evidence == 1 || candidate == 1;
        }
    }
}

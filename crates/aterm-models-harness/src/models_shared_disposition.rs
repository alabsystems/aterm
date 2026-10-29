// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Shared automatic sign-in distinguishes policy refusal from provider failure
/// and retains grant consumption when a running attempt is stopped.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_disposition_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedDisposition {
            const Buggy = 0;
            var before = 1; var after = 1; var provider = 1; var phase = 0;
            var consumed = 0; var entered = 0; var bound = 0; var failure = 0;
            action StopBefore when (phase == 0 && before == 1) { before = 0; }
            action StopDuring when (phase == 0 && after == 1) { after = 0; }
            action ProviderFail when (phase == 0 && provider == 1) { provider = 0; }
            action Run when (phase == 0) {
                phase = 1; entered = before;
                consumed = if Buggy == 2 && after == 0 {0} else {before};
                bound = if Buggy == 5 {0} else if before == 1 && (after == 1 || Buggy == 3) && provider == 1 {1} else {0};
                failure = if Buggy == 4 {0} else if Buggy == 1 && (before == 0 || after == 0) {1}
                    else if before == 1 && after == 1 && provider == 0 {1} else {0};
            }
            action Replay when (phase == 1 && before == 1) {
                phase = 2; entered = if Buggy == 6 {2} else {entered};
            }
            invariant PolicyRefusal: phase == 0 || (before == 1 && after == 1) || failure == 0;
            invariant Consumption: phase == 0 || consumed == before;
            invariant PublicationAuthority: bound == 0 || (before == 1 && after == 1);
            invariant ProviderFailure: phase == 0 || before == 0 || after == 0 || provider == 1 || failure == 1;
            invariant EligibleSuccess: phase == 0 || before == 0 || after == 0 || provider == 0 || bound == 1;
            invariant NoReplay: entered <= 1;
        }
    }
}

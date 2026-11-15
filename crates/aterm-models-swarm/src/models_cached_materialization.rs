// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local cache admission after a bodyless or failed response, without a changed log.
/// `cached` means the exact envelope verifies under the configured source and
/// signer; `unsettled` that the selection this Mac applied is not the current one, and
/// `recorded` that an apply of this build recorded what it reads (a state an older aterm wrote
/// records nothing). The native conformance test supplies signed bytes and real state.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cached_materialization_model() -> Model {
    aterm_spec::ty_model! {
        VaultCachedMaterialization {
            const Buggy = 0;
            var full = 0; var unsettled = 0; var cached = 1; var active = 1;
            var recorded = 1; var older = 0; var repairs = 1;
            var phase = 0; var pick = 0; var network = 0;
            action RequestFull when (phase == 0 && full == 0) { full = 1; }
            action Unsettle when (phase == 0 && unsettled == 0) { unsettled = 1; }
            action LoseCache when (phase == 0 && cached == 1) { cached = 0; }
            action Revoke when (phase == 0 && active == 1) { active = 0; }
            action LegacyState when (phase == 0 && recorded == 1) { recorded = 0; }
            action Older when (phase == 0 && older == 0) { older = 1; }
            action BoundedReader when (phase == 0 && repairs == 1) { repairs = 0; full = 1; }
            action Decide when (phase == 0) {
                phase = 1;
                pick = if Buggy == 5 {0}
                    else if Buggy == 8 && repairs == 0 && older == 1 {0}
                    else if (full == 1 || Buggy == 3)
                        && (cached == 1 || Buggy == 1)
                        && (active == 1 || Buggy == 2)
                        && (repairs == 0 || older == 0 || Buggy == 6)
                        && (unsettled == 1 || recorded == 0 || Buggy == 4) {1}
                    else {0};
                network = if Buggy == 7 && full == 1 && cached == 1 && active == 1
                    && (repairs == 0 || older == 0) && (unsettled == 1 || recorded == 0) {1}
                    else {0};
            }
            invariant CacheAuthority: pick == 0 || (cached == 1 && active == 1);
            invariant RepairProgress: phase == 0 || full == 0 || cached == 0
                || active == 0 || (repairs == 1 && older == 1)
                || (unsettled == 0 && recorded == 1) || pick == 1;
            invariant ConditionalQuiet: full == 1 || pick == 0;
            invariant SettledQuiet: unsettled == 1 || recorded == 0 || pick == 0;
            invariant OlderPreserved: older == 0 || repairs == 0 || pick == 0;
            invariant NoNetworkCredit: network == 0;
            bound CacheBox: full <= 1 && unsettled <= 1 && cached <= 1 && active <= 1
                && recorded <= 1 && older <= 1 && repairs <= 1 && phase <= 1 && pick <= 1 && network <= 1;
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One reader choosing among a held head, an applicable signed descendant and
/// a higher non-descendant. Source kinds are failure, missing, held body, 204,
/// invalid body, authoritative advancing body, older body and pre-rotation
/// older body. Selection 0 retains the
/// source result (including ordinary missing-store repair); 1/2/3 identify
/// held/descendant/rejected peer heads. A descendant can remove the reader,
/// and local cache damage preserves the held version floor but loses its body.
/// Legacy 1 preserves the held entries; legacy 2 drops one and 3 narrows its
/// audience. Those unparented
/// heads outrank the descendant, but need a recovered baseline for comparison.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn peer_selection_model() -> Model {
    aterm_spec::ty_model! {
        VaultPeerSelection {
            const Buggy = 0;
            var exact = 0; var good = 0; var fork = 0;
            var legacy = 0;
            var source = 0; var bounded = 0; var revoke = 0; var cached = 1;
            var phase = 0; var pick = 0;
            action HaveExact when (phase == 0 && exact == 0) { exact = 1; }
            action HaveGood when (phase == 0 && good == 0) { good = 1; }
            action HaveFork when (phase == 0 && fork == 0) { fork = 1; }
            action NextLegacy when (phase == 0 && 3 > legacy) { legacy = legacy + 1; }
            action NextSource when (phase == 0 && 7 > source) { source = source + 1; }
            action Bounded when (phase == 0 && bounded == 0) { bounded = 1; }
            action Revoke when (phase == 0 && revoke == 0) { revoke = 1; }
            action LoseCache when (phase == 0 && cached == 1) { cached = 0; }
            action Select when (phase == 0) {
                phase = 1;
                pick = if source == 5 && Buggy == 5 && good == 1 {2}
                    else if source == 5 {0}
                    else if source == 2 && Buggy == 6 && exact == 1 {1}
                    else if Buggy == 1 && fork == 1 {3}
                    else if Buggy == 2 && fork == 1 {0}
                    else if Buggy == 3 && exact == 1 {1}
                    else if Buggy == 9 && legacy == 2 && cached == 0 {5}
                    else if Buggy == 12 && legacy == 3 {5}
                    else if Buggy == 10 && legacy == 1 && cached == 0 && exact == 0
                        && (2 > source || source > 2) {4}
                    else if Buggy == 11 && legacy == 1 && cached == 0 && exact == 1 {1}
                    else if legacy == 1 && (cached == 1 || exact == 1 || source == 2) {4}
                    else if good == 1 {2}
                    else if Buggy == 8 && cached == 1 && source == 3 && bounded == 0
                        && exact == 1 {1}
                    else if Buggy == 7 && cached == 0 && source == 3 && bounded == 0 {0}
                    else if exact == 1 && (((bounded == 1 || cached == 0)
                        && (2 > source || source > 2))
                        || source == 0 || source == 4 || Buggy == 4) {1}
                    else {0};
            }
            invariant AdmittedOnly: (3 > pick || pick > 3) && 5 > pick;
            invariant DescendantProgress: phase == 0 || source == 5 || good == 0
                || (legacy == 1 && (cached == 1 || exact == 1 || source == 2)) || pick == 2;
            invariant HeldRecovery: phase == 0 || source == 5 || good == 1 || exact == 0
                || source == 2 || (bounded == 0 && cached == 1 && (source == 1 || source == 3
                    || source == 6 || source == 7)) || legacy == 1 || pick == 1;
            invariant RepairRetained: phase == 0 || bounded == 1 || good == 1 || cached == 0
                || source == 0 || source == 2 || source == 4
                || source == 5 || legacy == 1 || pick == 0;
            invariant SourceAuthority: 5 > source || source > 5 || pick == 0;
            invariant SourceReceipt: phase == 0 || 2 > source || source > 2
                || good == 1 || legacy == 1 || pick == 0;
            invariant LegacyCoverage: 4 > pick || legacy == 1;
            invariant LegacyBaseline: 4 > pick || cached == 1 || exact == 1 || source == 2;
            invariant LegacyProgress: phase == 0 || source == 5 || 1 > legacy || legacy > 1
                || (cached == 0 && exact == 0 && (2 > source || source > 2)) || pick == 4;
            bound SelectionBox: pick <= 5 && source <= 7 && phase <= 1 && cached <= 1
                && legacy <= 3;
        }
    }
}

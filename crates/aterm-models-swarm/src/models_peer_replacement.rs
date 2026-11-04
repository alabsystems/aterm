// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// An explicitly compiled signer replacement starts a new lineage, irrespective
/// of the retired lineage's sequence or cache. An unrelated key gains no entry.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn peer_replacement_model() -> Model {
    aterm_spec::ty_model! {
        VaultPeerReplacement {
            const Buggy = 0;
            var retired = 0; var valid = 1; var newer = 0;
            var cached = 1; var bounded = 0; var phase = 0; var pick = 0;
            var primary = 0;
            action Retire when (phase == 0 && retired == 0) { retired = 1; }
            action WrongKey when (phase == 0 && valid == 1) { valid = 0; }
            action Newer when (phase == 0 && newer == 0) { newer = 1; }
            action LoseCache when (phase == 0 && cached == 1) { cached = 0; }
            action Bounded when (phase == 0 && bounded == 0) { bounded = 1; }
            action UsePrimary when (phase == 0 && primary == 0) { primary = 1; }
            action Select when (phase == 0) {
                phase = 1;
                pick = if primary == 1 && Buggy == 4 {1}
                    else if primary == 1 {0}
                    else if Buggy == 1 {1}
                    else if retired == 0 || valid == 0 {0}
                    else if Buggy == 2 && newer == 0 {0}
                    else if Buggy == 3 && cached == 0 {0}
                    else {1};
            }
            invariant AuthorizedReplacement: pick == 0 || (retired == 1 && valid == 1);
            invariant ReplacementProgress: phase == 0 || primary == 1 || retired == 0
                || valid == 0 || pick == 1;
            invariant ReplacementSource: primary == 0 || pick == 0;
            bound ReplacementBox: pick <= 1 && phase <= 1 && retired <= 1 && valid <= 1
                && newer <= 1 && cached <= 1 && bounded <= 1 && primary <= 1;
        }
    }
}

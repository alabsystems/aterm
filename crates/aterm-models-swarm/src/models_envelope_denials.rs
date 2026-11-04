// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Both readable envelope audiences contribute denial evidence; malformed input refuses.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn envelope_denials_model() -> Model {
    aterm_spec::ty_model! {
        VaultEnvelopeDenials {
            const Buggy = 0;
            // 0 absent, 1 clean, 2 denied, 3 malformed.
            var everyone = 0; var host = 0;
            var phase = 0; var denied = 0; var failed = 0;
            action Everyone when (phase == 0 && everyone <= 2) { everyone = everyone + 1; }
            action Host when (phase == 0 && host <= 2) { host = host + 1; }
            action Read when (phase == 0) {
                phase = 1;
                failed = if Buggy == 3 {0}
                    else if Buggy == 5 || everyone == 3 || host == 3 {1} else {0};
                denied = if Buggy == 4 {1}
                    else if everyone == 3 || host == 3 {0}
                    else if Buggy == 1 { if host == 2 {1} else {0} }
                    else if Buggy == 2 { if everyone == 2 {1} else {0} }
                    else if everyone == 2 || host == 2 {1} else {0};
            }
            invariant EveryoneDenial: phase == 0 || everyone <= 1 || everyone == 3
                || failed == 1 || denied == 1;
            invariant HostDenial: phase == 0 || host <= 1 || host == 3
                || failed == 1 || denied == 1;
            invariant MalformedRefuses: phase == 0 || (everyone <= 2 && host <= 2) || failed == 1;
            invariant NoInventedDenial: denied == 0 || everyone == 2 || host == 2;
            invariant WellFormedAvailable: phase == 0 || everyone == 3 || host == 3 || failed == 0;
            bound ReadBox: everyone <= 3 && host <= 3 && phase <= 1 && denied <= 1 && failed <= 1;
        }
    }
}

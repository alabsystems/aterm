// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The final observation is a one-shot decision, not a readiness wait.
use aterm_spec::derive::*;

/// Bound by the lab's actual Boundary transitions and negative controls. A quiet peer
/// changes no arm state. Omitting the actual final sample (mutant 1) or retrying after a
/// real final refusal (mutant 2) cannot publish a qualified arm. Expiry, process/file
/// binding and real CPU/load/GPU sampling are separate runtime preconditions, exercised
/// by capacity and lab tests rather than represented as invented model observations.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn timing_comparison_final_model() -> Model {
    aterm_spec::ty_model! {
        TimingComparisonFinal {
            const Buggy = 0;
            // 0 setup, 1 running, 2 refused, 3 qualified, 4 consumed.
            var phase = 0;
            var attempted = 0;
            var real_final = 0;
            var bad_seen = 0;
            var published = 0;
            var peer = 0;
            action Entry when (phase == 0) { phase = 1; }
            action QuietPeer when (peer == 0) { peer = 1; }
            action BadFinal when (phase == 1) {
                attempted = 1;
                bad_seen = 1;
                phase = 2;
            }
            action QuietFinal when (phase == 1) {
                attempted = 1;
                real_final = 1;
                phase = 3;
            }
            action RefusedCleanup when (phase == 2) { phase = 4; }
            action QualifiedCleanup when (phase == 3) { phase = 4; published = 1; }
            action OmitActualFinal when (Buggy == 1 && phase == 1 && peer == 1) {
                attempted = 1;
                phase = 3;
            }
            action RetryRefusedFinal when (Buggy == 2 && phase == 2) {
                real_final = 1;
                phase = 3;
            }
            invariant PublicationRequiresActualFinal: published == 0 || real_final == 1;
            invariant FirstRefusalIsTerminal: published == 0 || bad_seen == 0;
            bound OneFinalDecision: phase <= 4 && attempted <= 1 && real_final <= 1
                && bad_seen <= 1 && published <= 1 && peer <= 1;
        }
    }
}

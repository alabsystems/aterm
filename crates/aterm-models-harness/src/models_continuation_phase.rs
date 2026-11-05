// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A retained closure preserves the active phase across two continuations,
/// including a child retired before its first Resume. Kernel closure evidence
/// and admission authority are separate prerequisites, modeled elsewhere.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn continuation_phase_model() -> Model {
    aterm_spec::ty_model! {
        ContinuationPhase {
            const Buggy = 0;
            var active = 0; var closed = 0; var captured = 0;
            var admitted = 0; var child = 0; var round = 0;
            action StartRollback when (active == 0 && closed == 0) { active = 1; }
            action Capture when (closed == 0) {
                captured = if Buggy == 1 {0} else {active};
                closed = 1;
            }
            action Admit when (closed == 1 && admitted == 0) {
                child = if Buggy == 2 {0} else {captured};
                admitted = 1;
            }
            action RetireChild when (admitted == 1 && round == 0) {
                active = child; closed = 0; admitted = 0; round = 1;
            }
            invariant CapturePreservesPhase: closed == 0 || captured == active;
            invariant AdmissionPreservesPhase: admitted == 0 || child == captured;
            bound Finite: 0 <= active && active <= 1 && 0 <= closed && closed <= 1
                && 0 <= captured && captured <= 1 && 0 <= admitted && admitted <= 1
                && 0 <= child && child <= 1 && 0 <= round && round <= 1;
        }
    }
}

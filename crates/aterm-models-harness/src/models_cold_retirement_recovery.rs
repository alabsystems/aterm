// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A fresh no-grant reply closes only a plain cold recipe without account successor debt.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cold_retirement_recovery_model() -> Model {
    aterm_spec::ty_model! {
        ColdRetirementRecovery {
            const Buggy = 0;
            var cold = 0; var candidate = 0; var held = 0;
            var proof = 0; var closed = 0; var dispatched = 0;
            action Cold when (proof == 0 && held == 0 && cold == 0) { cold = 1; }
            action Account when (proof == 0 && held == 0 && candidate == 0) { candidate = 1; }
            action PriorRetirement when (proof == 0 && held == 0) { held = 1; }
            action Unknown when (proof == 0) { closed = if Buggy == 2 {1} else {0}; }
            action NoGrant when (proof == 0) {
                proof = 1;
                closed = if Buggy == 1 {0} else {if cold == 1 && candidate == 0 {1} else {0}};
                held = if cold == 1 && candidate == 0 {0} else {1};
                dispatched = if Buggy == 3 {1} else {0};
            }
            invariant QualifiedProgress: proof == 0 || cold == 0 || candidate == 1 || closed == 1;
            invariant ExactColdOnly: closed == 0 || (proof == 1 && cold == 1 && candidate == 0);
            invariant NoDispatch: dispatched == 0;
            bound Finite: 0 <= cold && cold <= 1 && 0 <= candidate && candidate <= 1
                && 0 <= held && held <= 1 && 0 <= proof && proof <= 1
                && 0 <= closed && closed <= 1 && 0 <= dispatched && dispatched <= 1;
        }
    }
}

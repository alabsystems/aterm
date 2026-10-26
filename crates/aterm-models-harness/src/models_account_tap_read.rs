// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Usage-tap account projections recover admitted publication before inspecting roster shape.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_tap_read_model() -> Model {
    aterm_spec::ty_model! {
        AccountTapRead {
            const Buggy = 0;
            var phase = 0; var provider = 1; var reader = 1; var fault = 1; var shape = 0;
            var available = 0; var coherent = 1; var stable = 1; var preserved = 1;
            action AnotherProvider when (phase == 0 && 4 > provider) { provider = provider + 1; }
            action AnotherReader when (phase == 0 && 3 > reader) { reader = reader + 1; }
            action AnotherFault when (phase == 0 && 5 > fault) { fault = fault + 1; }
            action AnotherShape when (phase == 0 && 2 > shape) { shape = shape + 1; }
            action Read when (phase == 0) {
                phase = 1;
                available = if 3 > fault { if Buggy == 1 {0} else {1} }
                            else { if Buggy == 2 {1} else {0} };
                coherent = if Buggy == 3 {0} else {1};
                stable = if Buggy == 4 {0} else {1};
                preserved = if Buggy == 5 {0} else {1};
            }
            invariant ReplayableReadAvailable: phase == 0 || fault > 2 || available == 1;
            invariant UnresolvedReadRefused: phase == 0 || 3 > fault || available == 0;
            invariant ExactAccountProjection: coherent == 1;
            invariant StableRepeatAndRefusal: stable == 1;
            invariant CredentialsPreserved: preserved == 1;
        }
    }
}

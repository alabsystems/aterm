// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded admission scenarios for the native preparation queue. Denied
/// candidates consume neither cooldown nor the pass's one accepted slot.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_preparation_admission_model() -> Model {
    aterm_spec::ty_model! {
        NativePreparationAdmission {
            const Buggy = 0;
            var phase = 0; var picked = 0; var count = 0;
            var a = 0; var b = 0; var offered = 0;
            action Reject when (phase == 0) {
                phase = 1; offered = 2;
                count = if Buggy == 1 {1} else {0};
                a = if Buggy == 1 {10} else {0};
            }
            action Retry when (phase == 1) {
                phase = 2; offered = 1;
                picked = if Buggy == 2 {0} else {1};
                count = if Buggy == 2 {0} else {1};
                a = if Buggy == 2 {0} else {11};
            }
            action Skip when (phase == 0) {
                phase = 3;
                picked = if Buggy == 3 {0} else {2};
                count = if Buggy == 3 {0} else {1};
                b = if Buggy == 3 {0} else {10};
                offered = if Buggy == 4 {3} else {2};
            }
            action Cooling when (phase == 2) {
                phase = 4;
                picked = if Buggy == 5 {1} else {0};
                offered = if Buggy == 5 {1} else {0};
                a = if Buggy == 5 {12} else {11};
            }
            action Grow when (phase == 3) {
                phase = 5;
                picked = if Buggy == 6 {0} else {1};
                offered = if Buggy == 6 {0} else {1};
                count = if Buggy == 6 {1} else {2};
                a = if Buggy == 6 {0} else {11};
            }
            invariant DeniedIsUncharged: 1 > phase || phase > 1 || (count == 0 && a == 0 && b == 0);
            invariant RetryIsAvailable: 2 > phase || phase > 2 || (picked == 1 && a == 11 && count == 1);
            invariant DeniedPrefixDoesNotBlock: 3 > phase || phase > 3 || (picked == 2 && a == 0 && b == 10 && count == 1);
            invariant StopsAtAdmission: 3 > phase || phase > 3 || offered == 2;
            invariant CoolingIsNotOffered: 4 > phase || phase > 4 || (picked == 0 && offered == 0 && a == 11);
            invariant NewAccountsAreOffered: 5 > phase || phase > 5 || (picked == 1 && offered == 1 && count == 2 && a == 11);
        }
    }
}

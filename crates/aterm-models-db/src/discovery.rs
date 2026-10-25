// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded full-cache admission for every pair of idle or active endpoint states,
/// both age orders and separate or simultaneous capacity limits. Discovery never
/// grants membership.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn discovery_replacement_model() -> Model {
    aterm_spec::ty_model! {
        DbDiscoveryReplacement {
            const Buggy = 0;
            // States: untried, busy, failed, verified, verified+failed,
            // verified+busy, failed+busy, verified+failed+busy.
            var left = 1; var right = 1; var global = 0; var older = 1;
            var phase = 0; var victim = 0; var added = 0; var authority = 0; var collateral = 0;
            action AnotherLeft when (phase == 0 && left <= 7) { left = left + 1; }
            action AnotherRight when (phase == 0 && right <= 7) { right = right + 1; }
            action GlobalLimit when (phase == 0 && global <= 1) { global = global + 1; }
            action RightOlder when (phase == 0 && older == 1) { older = 2; }
            action Choose when (phase == 0) { phase = 1; }
            action Observe when (phase == 1) {
                phase = 2;
                victim = if Buggy == 1 && left == 5 && right == 5 {0}
                    else if Buggy == 10 && global == 1 && left == 5 && right == 5 {1}
                    else if Buggy == 2 && left == 2 && right == 4 {1}
                    else if Buggy == 3 && left == 1 && right == 4 {1}
                    else if Buggy == 4 && left == 4 && right == 4 {1}
                    else if Buggy == 5 && left == 5 && right == 3 {1}
                    else if Buggy == 6 && left == 3 && right == 3 {
                        if older == 1 {2} else {1}
                    } else if left == 3 && right == 3 {older}
                    else if left == 3 {1} else if right == 3 {2}
                    else if global == 1 {0}
                    else if left == 5 && right == 5 {older}
                    else if left == 5 {1} else if right == 5 {2} else {0};
                added = if Buggy == 1 && left == 5 && right == 5 {0}
                    else if Buggy == 10 && global == 1 && left == 5 && right == 5 {1}
                    else if left == 3 || right == 3 {1}
                    else if global == 1 {0}
                    else if left == 5 || right == 5 {1}
                    else if Buggy == 8 {1} else {0};
                authority = if Buggy == 7 {1} else {0};
                collateral = if Buggy == 9 && global == 2 {1} else {0};
            }
            invariant ExistingProtected: (victim == 0 || victim == 2 || left == 3 || left == 5)
                && (victim == 0 || victim == 1 || right == 3 || right == 5);
            invariant RecoverFailedSlot: phase <= 1
                || (global == 1 && (3 > left || left > 3) && (3 > right || right > 3))
                || (((3 > left || left > 3) && (5 > left || left > 5)
                    && (3 > right || right > 3) && (5 > right || right > 5)) || added == 1);
            invariant CapacityBound: (added == 0 && victim == 0)
                || (added == 1 && (victim == 1 || victim == 2));
            invariant FailedUnverifiedFirst: phase <= 1
                || ((3 > left || left > 3) && (3 > right || right > 3))
                || (victim == 1 && left == 3) || (victim == 2 && right == 3);
            invariant OldestFirst: phase <= 1 || left > right || right > left
                || (global == 1 && left == 5)
                || ((3 > left || left > 3) && (5 > left || left > 5)) || victim == older;
            invariant NoAuthorityFromBeacon: authority == 0;
            invariant NoCollateralEviction: collateral == 0;
            invariant UnrelatedIPCannotDisplaceVerified: global == 0 || global == 2
                || ((victim == 0 || victim == 2 || 5 > left || left > 5)
                    && (victim == 0 || victim == 1 || 5 > right || right > 5));
        }
    }
}

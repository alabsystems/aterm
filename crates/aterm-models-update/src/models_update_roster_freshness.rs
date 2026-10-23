// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A stage's roster authorization goes stale.

use aterm_spec::derive::Model;

/// `NativeUpdateRosterFreshness`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_roster_freshness_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateRosterFreshness {
            const Buggy = 0;
            const F = 2;
            const T = 5;
            var now = 0;
            var conf = 0;
            var revoked = 0;
            var rev_at = 0;
            var staged = 1;
            var swapped = 0;
            var late = 0;

            action Tick when (now <= T - 1) {
                now = now + 1;
            }
            action Revoke when (revoked == 0) {
                revoked = 1;
                rev_at = now;
            }
            action Check when (staged == 1 && swapped == 0) {
                staged = if revoked == 1 { 0 } else { 1 };
                conf = if revoked == 1 { conf } else { now };
            }
            action Swap when (staged == 1 && swapped == 0 && now - conf <= F - 1) {
                swapped = 1;
                late = if revoked == 1 && F <= now - rev_at { 1 } else { 0 };
            }

            action SwapOnTheFloorAlone when (Buggy == 1 && staged == 1 && swapped == 0) {
                swapped = 1;
                late = if revoked == 1 && F <= now - rev_at { 1 } else { 0 };
            }
            action ConfirmWithoutTheRoster when (Buggy == 1 && staged == 1 && swapped == 0) {
                conf = now;
            }

            invariant NoSwapPastTheWindow: late == 0;
        }
    }
}

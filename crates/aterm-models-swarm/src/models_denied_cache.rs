// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One listener observation from every retained verdict and prior diagnostic. Known 0/1/2
/// means unread/empty/one denied node. Inputs 0/1 are authoritative lists; 2..10 are the seven
/// unavailable reasons and two distinct read errors. Diagnostic changes never grant authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fleet_denied_cache_model() -> Model {
    aterm_spec::ty_model! {
        FleetDeniedCache {
            const Buggy = 0;
            var known = 0; var cause = 0; var input = 0;
            var before = 0; var previous = 0; var announced = 0; var phase = 0;
            action Known when (phase == 0 && 2 > known) { known = known + 1; }
            action Cause when (phase == 0 && 9 > cause) { cause = cause + 1; }
            action Input when (phase == 0 && 10 > input) { input = input + 1; }
            action Observe when (phase == 0) {
                before = known;
                previous = cause;
                known = if Buggy == 5 {known}
                    else if input <= 1 {input + 1}
                    else if Buggy == 1 {1}
                    else if Buggy == 2 {0}
                    else {known};
                cause = if Buggy == 6 {cause}
                    else if input <= 1 {0} else {input - 1};
                announced = if Buggy == 3 {1} else if Buggy == 4 {0}
                    else if input <= 1 {if cause > 0 {1} else {0}}
                    else if cause == input - 1 {0} else {1};
                phase = 1;
            }
            invariant FreshVerdict: phase == 0 || input > 1 || known == input + 1;
            invariant KeepsVerdict: phase == 0 || input <= 1 || known == before;
            invariant ObservedCause: phase == 0 || (input <= 1 && cause == 0)
                || (input > 1 && cause == input - 1);
            invariant ReportsTransitions: phase == 0
                || (input <= 1 && ((previous == 0 && announced == 0)
                    || (previous > 0 && announced == 1)))
                || (input > 1 && ((previous == input - 1 && announced == 0)
                    || ((previous > input - 1 || input - 1 > previous) && announced == 1)));
            bound CacheBox: known <= 2 && cause <= 9 && input <= 10 && before <= 2
                && previous <= 9 && announced <= 1 && phase <= 1;
        }
    }
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The test run's budget dispatcher (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.4, A9).

use aterm_spec::derive::*;

/// Three test binaries, charged 2, 2 and 1 of a share of 3, dispatched in that order: the head
/// of the queue starts only when the free share covers its charge, and a binary that ends
/// returns its charge once. `held` is what the running binaries hold, and `free` the rest. The
/// production projection (`aterm_verify::test_plan::dispatch`) logs `Start<i>` once binary `i`
/// holds its charge and `End<i>` before it returns it.
///
/// Buggy 1 admits the head while the free share is below its charge; 2 returns an ended
/// binary's charge a second time.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn test_dispatch_model() -> Model {
    aterm_spec::ty_model! {
        TestDispatch {
            const Buggy = 0;
            const Share = 3;
            const C0 = 2;
            const C1 = 2;
            const C2 = 1;

            var free = 3;
            var held = 0;
            var next = 0;
            var run0 = 0;
            var run1 = 0;
            var run2 = 0;
            var done0 = 0;
            var done1 = 0;
            var done2 = 0;
            var returned_twice = 0;

            action Start0 when (next == 0 && C0 <= free) {
                free = free - C0;
                held = held + C0;
                run0 = 1;
                next = 1;
            }
            action Start1 when (next == 1 && C1 <= free) {
                free = free - C1;
                held = held + C1;
                run1 = 1;
                next = 2;
            }
            action Start2 when (next == 2 && C2 <= free) {
                free = free - C2;
                held = held + C2;
                run2 = 1;
                next = 3;
            }
            action End0 when (run0 == 1) {
                run0 = 0;
                done0 = 1;
                free = free + C0;
                held = held - C0;
            }
            action End1 when (run1 == 1) {
                run1 = 0;
                done1 = 1;
                free = free + C1;
                held = held - C1;
            }
            action End2 when (run2 == 1) {
                run2 = 0;
                done2 = 1;
                free = free + C2;
                held = held - C2;
            }

            action AdmitShort1 when (Buggy == 1 && next == 1 && C1 > free) {
                free = 0;
                held = held + C1;
                run1 = 1;
                next = 2;
            }
            action ReturnTwice0 when (Buggy == 2 && done0 == 1 && returned_twice == 0) {
                free = free + C0;
                returned_twice = 1;
            }

            invariant ChargesStayWithinTheShare: held <= Share;
            invariant EveryChargeIsAccounted: free + held == Share;
            bound DispatchIsFinite: next <= 3 && held <= Share + C1 && free <= Share + C0
                && returned_twice <= 1;
        }
    }
}

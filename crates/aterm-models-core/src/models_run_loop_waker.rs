// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the event loop's run-loop wake timer.

use aterm_spec::derive::Model;

/// `aterm_objc::WakeTimer` over CoreFoundation's repeating-timer semantics.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn run_loop_waker_model() -> Model {
    aterm_spec::ty_model! {
        RunLoopWaker {
            const Buggy = 0;
            const LagCap = 2;
            var cache = 0;
            var spent = 0;
            var armed = 0;
            var want = 0;
            var delivered = 0;
            var lag = 0;
            var steps = 0;
            var tiny = 0;
            var deaf = 0;
            var mutated = 0;

            action MutateTinyInterval when (Buggy == 1 && mutated == 0) {
                tiny = 1;
                mutated = 1;
            }
            action MutateDeafPlan when (Buggy == 1 && mutated == 0) {
                deaf = 1;
                mutated = 1;
            }
            action RequestNow {
                want = 1;
                delivered = 0;
                armed = if spent == 0 && cache == 1 { armed } else { 1 };
                lag = if spent == 0 && cache == 1 { lag } else { 0 };
                spent = 0;
                cache = 1;
            }
            action RequestAt {
                want = 2;
                delivered = 0;
                armed = if spent == 0 && cache == 2 { armed } else { 1 };
                lag = if spent == 0 && cache == 2 { lag } else { 0 };
                spent = 0;
                cache = 2;
            }
            action RequestWait {
                want = 0;
                delivered = 0;
                armed = if cache == 0 { armed } else { 0 };
                lag = if cache == 0 { lag } else { 0 };
                spent = 0;
                cache = 0;
            }
            action Suspend when (armed == 1 && lag <= LagCap - 1) {
                lag = lag + 1;
            }
            action Fire when (armed == 1) {
                delivered = 1;
                steps = if tiny == 1 { lag + 1 } else { 1 };
                lag = 0;
                armed = if tiny == 1 { 1 } else { 0 };
                spent = if deaf == 1 || cache == 0 { spent } else { 1 };
            }

            invariant NoLostWake: want == 0 || armed == 1 || delivered == 1;
            invariant BoundedCatchUp: steps <= 1;
        }
    }
}

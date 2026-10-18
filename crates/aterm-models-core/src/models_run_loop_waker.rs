// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The event loop's run-loop wake timer: bounded CoreFoundation catch-up, and
//! no wake lost to a spent timer.

use aterm_spec::derive::Model;

/// `aterm_objc::WakeTimer` (the winit event-loop waker) over CoreFoundation's repeating-timer
/// semantics; `aterm-objc`'s `wake_timer` module says why the interval is a year.
///
/// `Suspend` is lateness, in units of a tiny interval; `Fire` records how many catch-up steps
/// CoreFoundation's walk took. With the shipped interval any survivable lateness is one step,
/// and a fired timer is then spent (parked a year out): a plan that trusts "already armed for
/// now" after the fire loses every later wake. `cache`/`spent` project `WakePlan`; `armed` is
/// what CoreFoundation will actually do.
///
/// `Buggy = 1` admits one of two defects: a tiny interval (the timer stays due after firing and
/// the walk is as long as the lateness), or a plan deaf to the callout. Tier-1 (`aterm-objc`'s
/// `conformance_wake_timer`) drives the real `WakeTimer` against real CoreFoundation on a test
/// thread's run loop, with both defects as negative controls.
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

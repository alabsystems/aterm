// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the native acquisition handoff. The genuine channel worker and
//! pure policy guards are bound in aterm-gpu's acquire_worker inline tests.

use aterm_spec::derive::metal_drawable_acquire_model;

#[test]
fn each_historical_failure_has_an_independent_derived_counterexample() {
    let model = aterm_spec::interp::with_buggy(&metal_drawable_acquire_model(), 1);
    for (actions, invariant) in [
        (vec!["Request", "Request"], "OneOutstanding"),
        (vec!["Request", "MainProgress"], "NoCallerWait"),
        (
            vec!["Request", "Close", "Retire"],
            "RetirementFollowsAcquisition",
        ),
        (
            vec!["Request", "Complete", "Take", "Invalidate", "AcceptResult"],
            "NoInvalidAcceptance",
        ),
        (vec!["Close", "AcceptResult"], "NoInvalidAcceptance"),
    ] {
        let mut state = model.init_state();
        for action in actions {
            assert!(model.fire(action, &mut state), "{action} must be reachable");
        }
        assert!(!model.check_invariant(invariant, &state), "{invariant}");
    }
}

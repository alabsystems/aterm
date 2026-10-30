// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The completion prerequisite of the disk-worker model, including its negative controls.

use aterm_models_harness::disk_discovery_cycle_model;
use aterm_spec::interp;

#[test]
fn discovery_cycle_laws_hold_and_each_mutant_has_a_real_counterexample() {
    let model = disk_discovery_cycle_model();
    assert!(interp::violated_invariants(&model).is_empty());
    assert!(interp::reachable_states(&model).len() < 10_000);
    for (dial, law) in [
        (1, "CompleteCoverage"),
        (2, "CompleteCoverage"),
        (3, "BoundedAssessment"),
        (4, "AppliedPage"),
        (5, "CompleteCoverage"),
    ] {
        let wrong = interp::with_buggy(&model, dial);
        assert!(
            interp::violated_invariants(&wrong).contains(law),
            "mutant {dial}: {law}"
        );
    }
}

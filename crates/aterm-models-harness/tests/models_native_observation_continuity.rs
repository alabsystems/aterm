// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_harness::native_observation_continuity_model;

#[test]
fn each_passive_continuity_law_has_a_falsifying_control() {
    let base = native_observation_continuity_model();
    let own = &[
        "Capture",
        "OwnClaim",
        "OwnRenew",
        "OwnRelease",
        "OwnRetake",
        "ProveOwnContinuity",
    ];
    for (bug, law, tail) in [
        (1, "OwnLifecycleAvailable", own.as_slice()),
        (2, "OwnLifecycleAvailable", own.as_slice()),
        (8, "OwnLifecycleAvailable", own.as_slice()),
        (
            3,
            "ForeignExpiryNeverRevives",
            &[
                "Capture",
                "ForeignRelaunch",
                "ForeignExpiry",
                "ProveAfterForeignExpiry",
            ][..],
        ),
        (
            4,
            "ForeignReturnNeverRevives",
            &[
                "Capture",
                "ForeignRelaunch",
                "ForeignReturn",
                "ProveAfterForeignReturn",
            ][..],
        ),
        (
            5,
            "OtherDutyDoesNotRevoke",
            &["Capture", "ForeignUpgradeOnly", "ProveUnrelatedDuty"][..],
        ),
        (
            6,
            "ExhaustionNeverRevives",
            &[
                "Capture",
                "NearExhaustion",
                "OverflowForeign",
                "OverflowReturn",
                "ProveAfterOverflow",
            ][..],
        ),
        (
            6,
            "ExhaustionCannotCapture",
            &[
                "Capture",
                "NearExhaustion",
                "OverflowForeign",
                "OverflowReturn",
                "CaptureAfterOverflow",
            ][..],
        ),
        (
            7,
            "ExhaustionCannotCapture",
            &[
                "Capture",
                "NearExhaustion",
                "OverflowForeign",
                "OverflowReturn",
                "CaptureAfterOverflow",
            ][..],
        ),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&base, bug);
        let mut good = base.init_state();
        let mut bad = mutant.init_state();
        let mut caught = false;
        for action in tail {
            assert!(base.fire(action, &mut good));
            assert!(base.check_invariant(law, &good));
            assert!(mutant.fire(action, &mut bad));
            caught |= !base.check_invariant(law, &bad);
        }
        assert!(caught, "mutant {bug} must violate {law}");
    }
}

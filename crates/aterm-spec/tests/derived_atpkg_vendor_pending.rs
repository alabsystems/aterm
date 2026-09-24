// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_vendor_pending_check_model, interp, verify};

#[test]
fn independent_vendor_results_are_bounded_and_harvested_once() {
    let model = atpkg_vendor_pending_check_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "vendor pending lifecycle must stay enrolled in spec-link"
    );
    verify::prove_and_catch_scalar(&model, "atpkg vendor pending check");

    // Fast second vendor is offered before the first check completes.
    let mut second_first = model.init_state();
    for action in ["StartFirst", "StartSecond", "SecondReady", "OfferSecond"] {
        assert!(
            model.fire(action, &mut second_first),
            "{action}: {second_first:?}"
        );
    }
    assert_eq!((second_first["first"], second_first["second"]), (1, 3));
    assert!(!model.action_enabled("StartFirst", &second_first));
    for action in ["FirstReady", "OfferFirst"] {
        assert!(
            model.fire(action, &mut second_first),
            "{action}: {second_first:?}"
        );
    }
    assert_eq!(
        (second_first["completed"], second_first["harvested"]),
        (2, 2)
    );

    // First vendor can also finish first; an unreachable second vendor still
    // receives a retry rather than being silently treated as checked.
    let mut first_first = model.init_state();
    for action in [
        "StartFirst",
        "StartSecond",
        "FirstReady",
        "OfferFirst",
        "SecondFailed",
        "RetrySecond",
    ] {
        assert!(
            model.fire(action, &mut first_first),
            "{action}: {first_first:?}"
        );
    }
    assert_eq!((first_first["first"], first_first["second"]), (3, 4));

    let buggy = interp::with_buggy(&model, 1);
    let mut extra = buggy.init_state();
    for action in ["StartFirst", "StartSecond", "SpawnExtra"] {
        assert!(buggy.fire(action, &mut extra));
    }
    assert!(!buggy.check_invariant("TwoActiveChecks", &extra));

    let mut duplicate = buggy.init_state();
    for action in ["StartFirst", "FirstReady", "CompleteTwice"] {
        assert!(buggy.fire(action, &mut duplicate));
    }
    assert!(!buggy.check_invariant("OneCompletionPerSlot", &duplicate));

    let mut duplicate_harvest = buggy.init_state();
    for action in ["StartFirst", "FirstReady", "OfferFirst", "OfferTwice"] {
        assert!(buggy.fire(action, &mut duplicate_harvest));
    }
    assert!(!buggy.check_invariant("EachCompletionConsumedOnce", &duplicate_harvest));

    let mut dropped = buggy.init_state();
    for action in ["StartFirst", "FirstReady", "DropFirstReady"] {
        assert!(buggy.fire(action, &mut dropped));
    }
    assert!(!buggy.check_invariant("OutstandingReadyIsOwned", &dropped));

    let mut lost_retry = buggy.init_state();
    for action in ["StartFirst", "FirstFailed", "LoseFirstRetry"] {
        assert!(buggy.fire(action, &mut lost_retry));
    }
    assert!(!buggy.check_invariant("FailedHeadHasRetry", &lost_retry));
}

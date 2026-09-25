// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{derive::atpkg_contention_release_park_model, interp, verify};

#[test]
fn a_contended_lane_retries_on_the_first_slice_after_release() {
    let model = atpkg_contention_release_park_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "atpkg contention release park");

    let mut held = model.init_state();
    assert!(model.fire("Check", &mut held));
    assert_eq!(held["decision"], 0, "a live holder keeps the park idle");
    assert!(model.fire("NextSlice", &mut held));
    assert!(model.fire("Release", &mut held));
    assert!(model.fire("Check", &mut held));
    assert_eq!(held["decision"], 3, "the next check retries");

    let mut bumped = model.init_state();
    assert!(model.fire("Release", &mut bumped));
    assert!(model.fire("OfferBump", &mut bumped));
    assert!(model.fire("Check", &mut bumped));
    assert_eq!(bumped["decision"], 1, "the bump wins release");

    let mut vendor = model.init_state();
    assert!(model.fire("Release", &mut vendor));
    assert!(model.fire("OfferVendor", &mut vendor));
    assert!(model.fire("Check", &mut vendor));
    assert_eq!(vendor["decision"], 2, "the ready vendor answer wins");

    let mut disabled = model.init_state();
    assert!(model.fire("Release", &mut disabled));
    assert!(model.fire("Disable", &mut disabled));
    assert!(model.fire("Check", &mut disabled));
    assert_eq!(disabled["decision"], 0, "no lock I/O in other parks");

    let buggy = interp::with_buggy(&model, 1);
    let mut early = buggy.init_state();
    assert!(buggy.fire("Check", &mut early));
    assert!(!buggy.check_invariant("HeldWriterKeepsParked", &early));
    let mut late = buggy.init_state();
    assert!(buggy.fire("Release", &mut late));
    assert!(buggy.fire("Check", &mut late));
    assert!(!buggy.check_invariant("ReleasedWriterWakesOnCheck", &late));
}

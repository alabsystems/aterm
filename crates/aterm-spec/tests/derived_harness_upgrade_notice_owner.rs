// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::{derive::harness_upgrade_notice_owner_model, interp, verify};

#[test]
fn ready_from_tab_a_never_authorizes_tab_b_or_duplicate_owners() {
    let model = harness_upgrade_notice_owner_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "harness upgrade notice owner");

    let mut foreign = model.init_state();
    assert!(model.fire("Announce", &mut foreign));
    assert!(model.fire("Ready", &mut foreign));
    assert!(model.fire("OtherTab", &mut foreign));
    assert!(!model.action_enabled("Terminate", &foreign));

    let mut duplicate = model.init_state();
    assert!(model.fire("Announce", &mut duplicate));
    assert!(model.fire("Ready", &mut duplicate));
    assert!(model.fire("DuplicateOwner", &mut duplicate));
    assert!(!model.action_enabled("Terminate", &duplicate));

    let mut partial = model.init_state();
    assert!(model.fire("Announce", &mut partial));
    assert!(model.fire("Ready", &mut partial));
    assert!(model.fire("IncompleteScan", &mut partial));
    assert!(!model.action_enabled("Terminate", &partial));

    let buggy = interp::with_buggy(&model, 1);
    let mut stolen = buggy.init_state();
    assert!(buggy.fire("Announce", &mut stolen));
    assert!(buggy.fire("Ready", &mut stolen));
    assert!(buggy.fire("OtherTab", &mut stolen));
    assert!(buggy.fire("Terminate", &mut stolen));
    assert!(!buggy.check_invariant("OnlyIssuerSignaled", &stolen));

    let mut partial = buggy.init_state();
    assert!(buggy.fire("Announce", &mut partial));
    assert!(buggy.fire("Ready", &mut partial));
    assert!(buggy.fire("IncompleteScan", &mut partial));
    assert!(buggy.fire("Terminate", &mut partial));
    assert!(!buggy.check_invariant("NoPartialScanSignal", &partial));
}

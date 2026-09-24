// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 checks sink-local paste ordering and queued-key kernel delivery.
//! Tier-1 drives the real sink and GUI decisions in `app_input` tests.

use aterm_spec::{
    derive::{paste_order_sink_isolation_model, queued_key_kernel_delivery_model},
    interp, verify,
};

#[test]
fn a_spill_receipt_cannot_grant_queued_key_present_priority() {
    let model = queued_key_kernel_delivery_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    if let Err(verify::NotRun { model }) = verify::prove_and_catch_tiered(&model, model.name) {
        eprintln!("TIER-0 SKIPPED: `{model}` requires an unavailable external checker");
    }

    let healthy = interp::with_buggy(&model, 0);
    let mut state = healthy.init_state();
    assert!(healthy.fire("AcceptSpill", &mut state));
    assert!(!healthy.action_enabled("PostWake", &state));
    assert!(healthy.fire("DrainSuccess", &mut state));
    assert!(
        !healthy.action_enabled("PostWake", &state),
        "a later whole-sink drain cannot turn an old spill receipt into a direct one"
    );

    let mutant = interp::with_buggy(&model, 1);
    let mut state = mutant.init_state();
    assert!(mutant.fire("AcceptSpill", &mut state));
    assert!(mutant.fire("PostWake", &mut state));
    assert!(!mutant.check_invariant("WakeRequiresDirectReceipt", &state));
}

#[test]
fn a_paste_in_a_cannot_order_a_key_in_b() {
    let model = paste_order_sink_isolation_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the sink-isolation machine must stay enrolled in the spec-link registry"
    );
    if let Err(verify::NotRun { model }) = verify::prove_and_catch_tiered(&model, model.name) {
        eprintln!(
            "TIER-0 SKIPPED (this test is NOT a pass for it): `{model}` requires an unavailable external checker"
        );
    }

    let healthy = interp::with_buggy(&model, 0);
    let mut state = healthy.init_state();
    assert!(healthy.fire("BeginA", &mut state));
    assert!(healthy.fire("ReadB", &mut state));
    assert_eq!(state["decision"], 0);
    assert!(healthy.check_invariant("DecisionReadsOnlySelectedSink", &state));

    let mut partially_drained = healthy.init_state();
    assert!(healthy.fire("BeginA", &mut partially_drained));
    assert!(healthy.fire("BeginA", &mut partially_drained));
    assert!(healthy.fire("CompleteA", &mut partially_drained));
    assert_eq!(partially_drained["pending_a"], 1);
    assert!(healthy.fire("ReadA", &mut partially_drained));
    assert_eq!(partially_drained["decision"], 1);

    let mutant = interp::with_buggy(&model, 1);
    let mut state = mutant.init_state();
    assert!(mutant.fire("BeginA", &mut state));
    assert!(mutant.fire("ReadB", &mut state));
    assert_eq!(state["decision"], 1);
    assert!(!mutant.check_invariant("DecisionReadsOnlySelectedSink", &state));
}

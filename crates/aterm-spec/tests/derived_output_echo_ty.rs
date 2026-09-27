// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for sink-ordered output-echo publication. The exhaustive gate proves
//! the healthy model and catches `Buggy=1`; the three concrete traces below keep
//! the stale-completion, torn-sample and publish-before-accept mutants
//! independently non-vacuous.

use aterm_spec::derive::{Model, output_echo_receipt_publication_model};
use aterm_spec::{interp, verify};
use std::collections::BTreeSet;

fn drive(model: &Model, actions: &[&str]) -> interp::State {
    let mut state = model.init_state();
    for action in actions {
        assert!(
            model.fire(action, &mut state),
            "{} action `{action}` must be enabled at {state:?}",
            model.name,
        );
    }
    state
}

#[test]
fn output_echo_receipt_publication_proves_and_catches() {
    let model = output_echo_receipt_publication_model();
    let _covered = verify::prove_and_catch_scalar(&model, model.name);

    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the cross-reference/strict-vacuity registry must include this machine",
    );
    assert_eq!(
        interp::fired_actions(&model),
        BTreeSet::from([
            "AcceptNewerBoundary",
            "AcceptOlderEcho",
            "PublishNewerBoundary",
            "PublishOlderEcho",
            "Sample",
        ]),
        "every healthy action must be reachable at the committed config",
    );
}

#[test]
fn delayed_older_completion_cannot_replace_the_newer_boundary() {
    let model = output_echo_receipt_publication_model();
    let trace = [
        "AcceptOlderEcho",
        "AcceptNewerBoundary",
        "PublishNewerBoundary",
        "PublishOlderEcho",
    ];

    let healthy = drive(&model, &trace);
    assert_eq!(healthy["published_order"], 2);
    assert_eq!(healthy["accepted_shadow"], 0);
    assert_eq!(healthy["boundary_shadow"], 1);
    assert!(model.check_invariant("LatestCompletedOrderWins", &healthy));
    assert!(model.check_invariant("BoundaryRetiresOlderEcho", &healthy));

    let buggy = interp::with_buggy(&model, 1);
    let overwritten = drive(&buggy, &trace);
    assert_eq!(overwritten["published_order"], 1);
    assert_eq!(overwritten["accepted_shadow"], 1);
    assert_eq!(overwritten["boundary_shadow"], 1);
    assert!(
        !buggy.check_invariant("LatestCompletedOrderWins", &overwritten),
        "the completion-order mutant must regress the published high-water",
    );
    assert!(
        !buggy.check_invariant("BoundaryRetiresOlderEcho", &overwritten),
        "the delayed older completion must reproduce the revived echo shadow",
    );
    assert!(buggy.check_invariant("SampleIsOnePublishedVersion", &overwritten));
    assert!(buggy.check_invariant("StateBounded", &overwritten));
}

#[test]
fn sample_cannot_mix_the_boundary_order_with_older_echo_timestamps() {
    let model = output_echo_receipt_publication_model();
    let prefix = [
        "AcceptOlderEcho",
        "PublishOlderEcho",
        "AcceptNewerBoundary",
        "PublishNewerBoundary",
    ];

    let mut healthy = drive(&model, &prefix);
    assert!(model.fire("Sample", &mut healthy));
    assert_eq!(healthy["sample_order"], 2);
    assert_eq!(healthy["sample_accepted"], 0);
    assert_eq!(healthy["sample_boundary"], 1);
    assert!(model.check_invariant("SampleIsOnePublishedVersion", &healthy));

    let buggy = interp::with_buggy(&model, 1);
    let mut torn = drive(&buggy, &prefix);
    assert!(
        buggy
            .invariants
            .iter()
            .all(|invariant| buggy.check_invariant(invariant.name, &torn)),
        "the prefix must be healthy so only the torn read supplies this counterexample",
    );
    assert!(buggy.fire("Sample", &mut torn));
    assert_eq!(torn["sample_order"], 2);
    assert_eq!(torn["sample_accepted"], 1);
    assert_eq!(torn["sample_boundary"], 0);
    assert!(buggy.check_invariant("LatestCompletedOrderWins", &torn));
    assert!(buggy.check_invariant("BoundaryRetiresOlderEcho", &torn));
    assert!(
        !buggy.check_invariant("SampleIsOnePublishedVersion", &torn),
        "the independent-atomic sampler mutant must be a concrete counterexample",
    );
    assert!(buggy.check_invariant("StateBounded", &torn));
}

#[test]
fn an_echo_the_sink_has_not_accepted_is_never_published() {
    let model = output_echo_receipt_publication_model();
    assert!(
        !model.action_enabled("PublishOlderEcho", &model.init_state()),
        "the committed tracker publishes only an accepted order"
    );

    let buggy = interp::with_buggy(&model, 1);
    let early = drive(&buggy, &["PublishOlderEcho"]);
    assert_eq!((early["accepted"], early["published_order"]), (0, 1));
    assert!(
        !buggy.check_invariant("PublishFollowsAcceptance", &early),
        "the attempt-minted order must publish ahead of acceptance",
    );
    for other in [
        "LatestCompletedOrderWins",
        "BoundaryRetiresOlderEcho",
        "SampleIsOnePublishedVersion",
        "StateBounded",
    ] {
        assert!(
            buggy.check_invariant(other, &early),
            "only the acceptance law may see the early publication, not `{other}`",
        );
    }
}

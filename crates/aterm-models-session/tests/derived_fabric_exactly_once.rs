// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `FabricExactlyOnce`, exactly once across the bridge's round at one inbox
//! offset (`aterm-link`'s `bridge.rs`, the endpoint's `cmd_deliver` and `cmd_link_log`):
//! proved at `Buggy = 0` with every action firing, each of the five defects caught on the
//! invariant written for it, and the hand-traced counterexamples walked step by step.
//! Tier-1 is `aterm-link`'s deterministic simulation (`sim`,
//! `the_round_refines_its_exactly_once_model`).

use aterm_models_session::fabric_exactly_once_model;
use aterm_spec::walk::{only, walk};
use aterm_spec::{interp, verify};

/// Every action of the committed machine, so a guard tightened by mistake shows up as a
/// dead action, not a quieter proof. `CommitEarly` is the defect 2's alone.
const ACTIONS: [&str; 12] = [
    "Tell",
    "DeliverTaken",
    "Publish",
    "Reuse",
    "Deliver",
    "Refill",
    "Commit",
    "Kill",
    "Relaunch",
    "MoveTakes",
    "MoveKeeps",
    "MoveKeepsUncommits",
];

#[test]
fn fabric_exactly_once_proves_and_catches() {
    let model = fabric_exactly_once_model();
    let committed = interp::fired_actions(&model);
    for action in ACTIONS {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
    assert!(
        !committed.contains("CommitEarly"),
        "the early commit is the defect's, never the committed machine's"
    );
    verify::prove_and_catch_scalar(
        &model,
        "exactly once at one inbox offset: no record is a row twice, and the group never \
         passes a record that is not a row, across kills, relaunches and moves of the log",
    );
}

/// Each defect breaks the invariant written for it, and the committed machine keeps it.
#[test]
fn fabric_exactly_once_each_defect_breaks_its_own_invariant() {
    let model = fabric_exactly_once_model();
    for (buggy, invariant) in [
        (1, "NoDuplicate"),
        (2, "NoSkip"),
        (3, "NoSkip"),
        (4, "NoDuplicate"),
        (5, "NoSkip"),
    ] {
        assert!(
            interp::bmc(&only(&interp::with_buggy(&model, buggy), invariant)).is_err(),
            "Buggy = {buggy} must break {invariant}"
        );
        assert!(
            interp::bmc(&only(&model, invariant)).is_ok(),
            "{invariant} holds with no defect on"
        );
    }
}

/// The hand-traced counterexamples, one per defect, walked on the mutant: each ends in a
/// state its invariant forbids, and the committed machine keeps the invariant along every
/// step of the same trace it can take.
#[test]
fn fabric_exactly_once_the_traced_counterexamples_end_where_the_doc_says() {
    let model = fabric_exactly_once_model();
    let traces: [(i64, &str, Vec<&str>); 5] = [
        // 1: a kill before the commit; the relaunch's group hands the record over again,
        // and an endpoint with no dedup makes it a second row.
        (
            1,
            "NoDuplicate",
            vec!["Publish", "Deliver", "Kill", "Relaunch", "Deliver"],
        ),
        // 2: the group is committed past the record before it is delivered, and the
        // bridge dies: the record is never a row and never offered again.
        (2, "NoSkip", vec!["Publish", "CommitEarly"]),
        // 3: a move takes the record; the endpoint keeps its offset, and the record the
        // offset names now is answered as the old one's row and committed past.
        (
            3,
            "NoSkip",
            vec![
                "Publish",
                "Deliver",
                "Commit",
                "MoveTakes",
                "Tell",
                "Reuse",
                "Deliver",
                "Commit",
            ],
        ),
        // 4: a move that kept the record and only its commit; an endpoint that forgets
        // every offset at any move takes the redelivery as a new row.
        (
            4,
            "NoDuplicate",
            vec![
                "Publish",
                "Deliver",
                "Commit",
                "MoveKeepsUncommits",
                "Tell",
                "Deliver",
            ],
        ),
        // 5: the owed commit outlives a move that took the record: the next record at
        // the offset is committed past before anything delivers it.
        (
            5,
            "NoSkip",
            vec!["Publish", "Deliver", "MoveTakes", "Tell", "Reuse", "Commit"],
        ),
    ];
    for (buggy, invariant, steps) in traces {
        let mutant = interp::with_buggy(&model, buggy);
        let end = walk(&mutant, &steps);
        assert!(
            !mutant.check_invariant(invariant, &end),
            "Buggy = {buggy}: {steps:?} breaks {invariant}: {end:?}"
        );
        let mut state = model.init_state();
        for step in &steps {
            if !model.fire(step, &mut state) {
                break;
            }
            assert!(
                model.check_invariant(invariant, &state),
                "Buggy = 0 keeps {invariant} along {steps:?}: {state:?}"
            );
        }
    }
}

/// What the round buys, on the committed machine: a kill between the delivery and the
/// commit costs a redelivery the endpoint dedups; a move that takes the record lets the
/// next record at its offset be a row of its own.
#[test]
fn fabric_exactly_once_walks_the_round_a_bridge_makes() {
    let m = fabric_exactly_once_model();
    let redelivered = walk(
        &m,
        &[
            "Publish", "Deliver", "Kill", "Relaunch", "Deliver", "Commit",
        ],
    );
    assert_eq!(
        (redelivered["nr"], redelivered["c"]),
        (1, 2),
        "one row, and the group past it: {redelivered:?}"
    );
    let reused = walk(
        &m,
        &[
            "Publish",
            "Deliver",
            "Commit",
            "MoveTakes",
            "Tell",
            "Reuse",
            "Deliver",
            "Commit",
        ],
    );
    assert_eq!(
        (reused["nr"], reused["nr2"], reused["c"]),
        (1, 1, 2),
        "the record a move took was one row, the one at its offset now is another: {reused:?}"
    );
}

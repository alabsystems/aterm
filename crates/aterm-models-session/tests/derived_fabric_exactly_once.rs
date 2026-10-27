// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 tests for `FabricExactlyOnce`: action coverage, mutant invariant checks, and counterexample traces.

use aterm_models_session::fabric_exactly_once_model;
use aterm_spec::interp;
use aterm_spec::walk::{only, walk};

const ACTIONS: [&str; 14] = [
    "GivenUp",
    "OwePast",
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
fn fabric_exactly_once_fires_every_committed_move() {
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
}

#[test]
fn fabric_exactly_once_each_defect_breaks_its_own_invariant() {
    let model = fabric_exactly_once_model();
    for (buggy, invariant) in [
        (1, "NoDuplicate"),
        (2, "NoSkip"),
        (3, "NoSkip"),
        (4, "NoDuplicate"),
        (5, "NoSkip"),
        (6, "NoSkip"),
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

#[test]
fn fabric_exactly_once_the_traced_counterexamples_end_where_the_doc_says() {
    let model = fabric_exactly_once_model();
    let traces: [(i64, &str, Vec<&str>); 6] = [
        (
            1,
            "NoDuplicate",
            vec!["Publish", "Deliver", "Kill", "Relaunch", "Deliver"],
        ),
        (2, "NoSkip", vec!["Publish", "CommitEarly"]),
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
        (
            5,
            "NoSkip",
            vec!["Publish", "Deliver", "MoveTakes", "Tell", "Reuse", "Commit"],
        ),
        (6, "NoSkip", vec!["Publish", "OwePast", "Commit"]),
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
    let given_up = walk(&m, &["Publish", "GivenUp", "OwePast", "Commit"]);
    assert_eq!(
        (given_up["nr"], given_up["c"], given_up["gone"]),
        (0, 2, 1),
        "retention gave the record up unread and the group went past it for a later one: \
         no row, and NoSkip holds by the give-up: {given_up:?}"
    );
    assert!(m.check_invariant("NoSkip", &given_up));
    let mut kept = walk(&m, &["Publish", "GivenUp"]);
    assert!(
        !m.fire("MoveTakes", &mut kept),
        "a record a cut gave up is on stable media: no move takes it: {kept:?}"
    );
}

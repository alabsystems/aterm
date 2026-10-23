// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 conformance tests for the `IncarnationLease` model.

use aterm_models_session::incarnation_lease_model;
use aterm_spec::interp;
use aterm_spec::walk::{only, walk};

const ACTIONS: [&str; 12] = [
    "Plan",
    "Take",
    "TakeFlushed",
    "KillRenamed",
    "KillFlushed",
    "Present",
    "Kill",
    "WritebackInc",
    "WritebackLease",
    "WritebackSeq",
    "PowerLoss",
    "OldAttach",
];

#[test]
fn incarnation_lease_fires_every_committed_move() {
    let model = incarnation_lease_model();
    let committed = interp::fired_actions(&model);
    for action in ACTIONS {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
}

#[test]
fn incarnation_lease_each_defect_breaks_its_own_invariant_and_the_fence() {
    let model = incarnation_lease_model();
    for (buggy, invariant) in [
        (1, "Fence"),
        (2, "LeaseDurable"),
        (3, "LeaseDurable"),
        (4, "Durable"),
    ] {
        for claim in [invariant, "Fence"] {
            let mutant = only(&interp::with_buggy(&model, buggy), claim);
            assert!(
                interp::bmc(&mutant).is_err(),
                "Buggy = {buggy} must break {claim}"
            );
            assert!(
                interp::bmc(&only(&model, claim)).is_ok(),
                "{claim} holds with no defect on"
            );
        }
    }
}

#[test]
fn incarnation_lease_the_traced_counterexamples_end_where_the_doc_says() {
    let model = incarnation_lease_model();
    let first_boot: &[&str] = &[
        "Plan",
        "TakeFlushed",
        "Present",
        "WritebackLease",
        "Plan",
        "Take",
        "Present",
    ];
    let run_out: &[&str] = &["Plan", "KillRenamed"];
    let traces: [(i64, Vec<&str>); 4] = [
        (
            1,
            [first_boot, &["PowerLoss", "Plan", "Take", "Present"]].concat(),
        ),
        (
            2,
            [
                first_boot,
                run_out,
                &[
                    "Plan",
                    "Take",
                    "Present",
                    "PowerLoss",
                    "Plan",
                    "TakeFlushed",
                    "Present",
                ],
            ]
            .concat(),
        ),
        (
            3,
            [
                first_boot,
                run_out,
                &[
                    "Plan",
                    "Take",
                    "Present",
                    "PowerLoss",
                    "Plan",
                    "TakeFlushed",
                    "Present",
                ],
            ]
            .concat(),
        ),
        (
            4,
            vec![
                "Plan",
                "TakeFlushed",
                "Present",
                "PowerLoss",
                "Plan",
                "TakeFlushed",
                "Present",
            ],
        ),
    ];
    for (buggy, steps) in traces {
        let mutant = interp::with_buggy(&model, buggy);
        let end = walk(&mutant, &steps);
        assert!(
            !mutant.check_invariant("Fence", &end),
            "Buggy = {buggy}: {steps:?} presents an incarnation twice: {end:?}"
        );
        let mut state = model.init_state();
        for step in &steps {
            if !model.fire(step, &mut state) {
                break;
            }
            assert!(
                model.check_invariant("Fence", &state),
                "Buggy = 0 keeps the fence along {steps:?}: {state:?}"
            );
        }
    }
}

#[test]
fn incarnation_lease_walks_the_attaches_a_node_makes() {
    let m = incarnation_lease_model();

    let steady = walk(
        &m,
        &["Plan", "TakeFlushed", "Present", "Plan", "Take", "Present"],
    );
    assert_eq!(
        (steady["hi"], steady["m_inc"], steady["c_used"]),
        (2, 2, 2),
        "the second attach of a boot is covered by the first one's flush: {steady:?}"
    );

    let rebooted = walk(
        &m,
        &[
            "Plan",
            "TakeFlushed",
            "Present",
            "WritebackLease",
            "WritebackSeq",
            "PowerLoss",
            "Plan",
        ],
    );
    assert_eq!(
        (rebooted["tr"], rebooted["pn"]),
        (0, 3),
        "a lease from another boot is not trusted: the attach takes the incarnation above \
         the whole lease: {rebooted:?}"
    );

    let old = walk(&m, &["Plan", "TakeFlushed", "Present", "OldAttach", "Plan"]);
    assert_eq!(
        (old["tr"], old["pn"], old["hi"]),
        (0, 4, 3),
        "an older build's `inc + 1` leaves the lease's top behind, and the next attach \
         leases above it: {old:?}"
    );
}

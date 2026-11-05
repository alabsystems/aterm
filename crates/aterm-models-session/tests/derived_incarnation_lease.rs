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

/// The model's scenario off the boot volume, where a disk can vanish within one boot.
fn off_the_boot_volume() -> aterm_spec::derive::Model {
    interp::with_consts(&incarnation_lease_model(), &[("OffVolume", 1)])
}

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
    assert!(
        !committed.contains("Vanish"),
        "the boot volume's disk never vanishes within a boot: {committed:?}"
    );
}

#[test]
fn incarnation_lease_off_the_boot_volume_vanishes_and_never_trusts_a_lease() {
    let off = interp::fired_actions(&off_the_boot_volume());
    for action in ACTIONS.iter().filter(|a| **a != "Take").chain(&["Vanish"]) {
        assert!(
            off.contains(action),
            "{action} fires off the boot volume: {off:?}"
        );
    }
    assert!(
        !off.contains("Take"),
        "off the boot volume every take flushes `inc`, whatever boot its lease names: {off:?}"
    );
    let named = walk(
        &off_the_boot_volume(),
        &["Plan", "TakeFlushed", "Present", "Plan"],
    );
    assert_eq!(
        (
            named["c_lb"],
            named["boot"],
            named["tr"],
            named["pn"],
            named["sp"]
        ),
        (1, 1, 0, 2, 1),
        "the lease names this boot, and is not trusted by it: the next take leases one \
         incarnation above `inc`: {named:?}"
    );
}

#[test]
fn incarnation_lease_each_defect_breaks_its_own_invariant_and_the_fence() {
    let model = incarnation_lease_model();
    let off = off_the_boot_volume();
    for (config, buggy, invariant) in [
        (&model, 1, "Fence"),
        (&model, 2, "LeaseDurable"),
        (&model, 3, "LeaseDurable"),
        (&model, 4, "Durable"),
        (&off, 5, "Fence"),
    ] {
        for claim in [invariant, "Fence"] {
            let mutant = only(&interp::with_buggy(config, buggy), claim);
            assert!(
                interp::bmc(&mutant).is_err(),
                "Buggy = {buggy} must break {claim}"
            );
            assert!(
                interp::bmc(&only(config, claim)).is_ok(),
                "{claim} holds with no defect on"
            );
        }
    }
    assert!(
        interp::bmc(&interp::with_buggy(&model, 5)).is_ok(),
        "on the boot volume the named boot is the trusted one: Buggy = 5 is no defect there"
    );
}

#[test]
fn incarnation_lease_the_traced_counterexamples_end_where_the_doc_says() {
    let (model, off) = (incarnation_lease_model(), off_the_boot_volume());
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
    let traces: [(&_, i64, Vec<&str>); 5] = [
        (
            &model,
            1,
            [first_boot, &["PowerLoss", "Plan", "Take", "Present"]].concat(),
        ),
        (
            &off,
            5,
            [first_boot, &["Vanish", "Plan", "Take", "Present"]].concat(),
        ),
        (
            &model,
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
            &model,
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
            &model,
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
    for (config, buggy, steps) in traces {
        let mutant = interp::with_buggy(config, buggy);
        let end = walk(&mutant, &steps);
        assert!(
            !mutant.check_invariant("Fence", &end),
            "Buggy = {buggy}: {steps:?} presents an incarnation twice: {end:?}"
        );
        let mut state = config.init_state();
        for step in &steps {
            if !config.fire(step, &mut state) {
                break;
            }
            assert!(
                config.check_invariant("Fence", &state),
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

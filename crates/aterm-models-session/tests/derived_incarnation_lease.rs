// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `IncarnationLease`, the incarnation lease (`aterm-link`'s `state.rs`, "The
//! incarnation lease"): proved at `Buggy = 0` with every action firing, each of the four
//! defects caught on the invariant written for it and on the fence itself, and the
//! hand-traced counterexamples walked step by step. Tier-1 is `aterm-link/src/state.rs`'s
//! `the_incarnation_lease_refines_its_model`.

use aterm_models_session::incarnation_lease_model;
use aterm_spec::walk::{only, walk};
use aterm_spec::{interp, verify};

/// Every action, so a guard tightened by mistake shows up as a dead action, not a quieter proof.
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
fn incarnation_lease_proves_and_catches() {
    let model = incarnation_lease_model();
    let committed = interp::fired_actions(&model);
    for action in ACTIONS {
        assert!(
            committed.contains(action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
    verify::prove_and_catch_scalar(
        &model,
        "incarnation lease: a trusted lease names a durable top, every presented incarnation is \
         covered on the media, and none is ever presented twice, across every kill and power loss",
    );
}

/// Each defect breaks the invariant written for it, alone, and every one of them breaks the
/// fence itself: the claims are not decoration around a property nothing threatens.
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

/// The hand-traced counterexamples, one per defect, walked on the mutant: each ends in a
/// state the fence forbids (an incarnation presented twice), and the committed machine
/// either cannot take the same steps or keeps the fence along them.
#[test]
fn incarnation_lease_the_traced_counterexamples_end_where_the_doc_says() {
    let model = incarnation_lease_model();
    // Two attaches of the first boot share one flush; the second lease's record of what
    // was taken is written back, and the power goes.
    let first_boot: &[&str] = &[
        "Plan",
        "TakeFlushed",
        "Present",
        "WritebackLease",
        "Plan",
        "Take",
        "Present",
    ];
    // Then a third attach runs the lease out and is killed after its rename.
    let run_out: &[&str] = &["Plan", "KillRenamed"];
    let traces: [(i64, Vec<&str>); 4] = [
        // 1: the next boot trusts the written-back lease (`used = 1`, top 2 — the top the
        // media holds) and takes 2 again.
        (
            1,
            [first_boot, &["PowerLoss", "Plan", "Take", "Present"]].concat(),
        ),
        // 2: the retry trusts the lease although `inc` (renamed in, not flushed) is not its
        // top, takes 3 under the unflushed top, and the power goes before the top lands.
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
        // 3: the lease is written before its top is flushed; the kill after the rename
        // leaves a lease that names the unflushed top, and the retry trusts it.
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
        // 4: the will goes out while the first flush is still in flight; the power goes
        // before it lands, and the next boot takes the same incarnation.
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

/// What the lease buys, on the committed machine: in one boot a second attach takes its
/// incarnation with no flush; after a power loss the attach leases afresh above everything
/// the media holds; and an older build's attach on the same dir is fenced too.
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

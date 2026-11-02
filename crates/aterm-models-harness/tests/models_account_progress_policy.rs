// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded policy-epoch safety for a normal future-Wait receipt. The GUI's real
//! Off/account-advice fixture supplies runtime conformance of selected fields; these
//! checks exercise the authored machine and its negative controls.

use aterm_models_harness::account_progress_policy_model;
use aterm_spec::derive::InvariantKind;
use aterm_spec::{interp, verify};

const FAULTS: [(i64, &str, &str); 8] = [
    (1, "NoOffFence", "PublicationUsesCurrentPolicy"),
    (2, "NoOnFence", "PublicationUsesCurrentPolicy"),
    (3, "AcceptOldProducer", "PublicationUsesCurrentPolicy"),
    (4, "ReviveRetiredRecord", "ProjectionUsesCapturedAuthority"),
    (5, "KeepPausedNext", "NoNextWithoutCurrentDecision"),
    (6, "DropFreshProducer", "PublicationUsesCurrentPolicy"),
    (7, "IgnoreReadRevision", "StaleReadWithdrawsClaims"),
    (8, "RewriteHistoryOnRead", "PublicationUsesCurrentPolicy"),
];

#[test]
fn account_progress_policy_has_bounded_moves_and_nonvacuous_claims() {
    let model = account_progress_policy_model();
    let (verdict, fired) = interp::bmc_fired(&model);
    let states = verdict.expect("the committed policy protocol keeps every claim");
    assert!(states < 50_000, "normal policy protocol: {states} states");
    assert!(
        model
            .actions
            .iter()
            .all(|action| fired.contains(action.name))
    );
    for (fault, label, law) in FAULTS {
        let one = aterm_spec::walk::only(&model, law);
        let wrong = interp::with_buggy(&one, fault);
        assert!(
            interp::bmc(&wrong).is_err(),
            "{label} breaks {law} through its direct registry dial"
        );
        let selected = interp::with_consts(&one, &[("Fault", fault), ("Buggy", 1)]);
        assert!(
            interp::bmc(&selected).is_err(),
            "{label} breaks {law} through the tiered helper's selected dial"
        );
    }
    for law in model
        .invariants
        .iter()
        .filter(|law| law.kind == InvariantKind::Claim)
    {
        assert!(
            FAULTS.iter().any(|(_, _, name)| *name == law.name),
            "{} has an exercised negative control",
            law.name
        );
    }
    eprintln!("AccountProgressPolicy: {states} committed states; all eight faults caught");
}

#[test]
fn account_progress_policy_proves_and_catches_each_fault_tiered() {
    let model = account_progress_policy_model();
    for (fault, label, _) in FAULTS {
        let selected = interp::with_consts(&model, &[("Fault", fault)]);
        let coverage = verify::prove_and_catch_scalar(&selected, label);
        eprintln!("AccountProgressPolicy {label}: {coverage:?}");
    }
}

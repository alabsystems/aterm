// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;

#[cfg(target_os = "macos")]
use super::persistent_notice_is_owed;
use super::{
    ReconciledStatusOutcome, StatusReconciliation, commit_matches, compiled_update_pin_sha256,
    persisted_claims_stage, reconcile_status_outcome, status_reconciliation_projection,
    update_pubkey_sha256,
};

#[test]
fn update_pin_fingerprint_hashes_decoded_key_and_fails_closed() {
    let encoded = aterm_codec::base64::encode(&[0_u8; 32]).unwrap();
    assert_eq!(
        update_pubkey_sha256(&encoded).unwrap().as_deref(),
        Some("66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925")
    );
    assert_eq!(update_pubkey_sha256("").unwrap(), None);
    assert!(update_pubkey_sha256("not-base64").is_err());
    let short = aterm_codec::base64::encode(&[0_u8; 31]).unwrap();
    assert!(update_pubkey_sha256(&short).is_err());

    let compiled = compiled_update_pin_sha256();
    assert!(
        compiled.len() == 64 && compiled.bytes().all(|b| b.is_ascii_hexdigit()),
        "the armed master prints its fingerprint: {compiled}"
    );
    assert_eq!(
        compiled,
        update_pubkey_sha256(aterm_update_core::pins::PAPER_MASTER_PUBKEYS[0])
            .unwrap()
            .unwrap()
    );
}

fn observed_outcome(reconciliation: &StatusReconciliation) -> (&str, bool) {
    match &reconciliation.outcome {
        ReconciledStatusOutcome::Preserved(outcome) => (outcome, false),
        ReconciledStatusOutcome::Neutralized(outcome) => (outcome, true),
    }
}

#[test]
fn status_reconciliation_exhaustively_refines_the_model() {
    let model = aterm_models_update::native_update_status_reconciliation_model();
    let reachable_inputs: BTreeSet<_> = model
        .successors("PickStatusInputs", &model.init_state())
        .into_iter()
        .collect();
    assert_eq!(
        reachable_inputs.len(),
        16,
        "2 running × 2 ledger × 2 Ready × 2 persisted classes"
    );

    let persisted_cases = [
        ("staged 0.2 (build 2) — applies on next launch", true),
        ("network check failed", false),
    ];
    let ready_builds = [None, Some(1), Some(2), Some(3)];
    let mut projected_inputs = BTreeSet::new();
    let mut calls = 0usize;

    for running_build in 1..=2 {
        for checked_from_build in 1..=2 {
            for ready_build in ready_builds {
                for (persisted, staged_claim) in persisted_cases {
                    assert_eq!(persisted_claims_stage(persisted), staged_claim);
                    let reconciliation = reconcile_status_outcome(
                        running_build,
                        checked_from_build,
                        ready_build,
                        persisted.to_string(),
                    );
                    let expected_ready = ready_build.is_some_and(|build| build > running_build);
                    assert_eq!(
                        reconciliation.current_build, running_build,
                        "the ledger may never relabel its caller"
                    );
                    assert_eq!(
                        reconciliation.ready_present, expected_ready,
                        "Ready must be effective iff its build is strictly newer"
                    );

                    let expected_neutralized =
                        !expected_ready && (checked_from_build != running_build || staged_claim);
                    let (reported_outcome, neutralized) = observed_outcome(&reconciliation);
                    assert_eq!(neutralized, expected_neutralized);
                    if expected_neutralized {
                        let expected = if checked_from_build == running_build {
                            "no update is staged"
                        } else {
                            "no update is staged (checked from another build of aterm)"
                        };
                        assert_eq!(reported_outcome, expected);
                    } else {
                        assert_eq!(reported_outcome, persisted);
                    }

                    let (previous, next) = status_reconciliation_projection(
                        running_build,
                        checked_from_build,
                        reconciliation.ready_present,
                        persisted,
                        reconciliation.current_build,
                        reported_outcome,
                        neutralized,
                    );
                    assert!(
                        reachable_inputs.contains(&previous),
                        "real input projection is unreachable: {previous:?}"
                    );
                    projected_inputs.insert(previous.clone());

                    let label = format!(
                        "status reconciliation running={running_build} ledger={checked_from_build} ready={ready_build:?} staged={staged_claim}"
                    );
                    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
                        &model,
                        &[],
                        &previous,
                        &next,
                        Some("ReconcileStatus"),
                        &label,
                    );
                    assert!(admitted, "real reducer transition rejected: {why}");
                    for invariant in &model.invariants {
                        assert!(
                            model.check_invariant(invariant.name, &next),
                            "real reducer violated {}: {next:?}",
                            invariant.name
                        );
                    }
                    calls += 1;
                }
            }
        }
    }

    assert_eq!(
        calls, 32,
        "the concrete decision lattice must stay exhaustive"
    );
    assert_eq!(
        projected_inputs, reachable_inputs,
        "Tier-1 must cover every bounded PickStatusInputs class"
    );
}

#[test]
fn status_reconciliation_model_negative_controls_are_non_vacuous() {
    let model = aterm_models_update::native_update_status_reconciliation_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    let (caller_previous, caller_bug) = status_reconciliation_projection(
        1,
        2,
        false,
        "network check failed",
        2,
        "network check failed",
        false,
    );
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &caller_previous,
        &caller_bug,
        Some("ReconcileStatus"),
        "status reconciliation caller-build negative control",
    );
    assert!(!admitted, "healthy model admitted caller relabeling: {why}");
    assert!(
        buggy
            .successors("ReconcileTrustingLedger", &caller_previous)
            .contains(&caller_bug),
        "Buggy=1 must reproduce caller relabeling"
    );
    assert!(!buggy.check_invariant("CallerBuildIsAuthoritative", &caller_bug));

    let persisted = "staged 0.2 (build 2) — applies on next launch";
    let real = reconcile_status_outcome(2, 2, Some(2), persisted.to_string());
    assert!(
        !real.ready_present,
        "an equal-build Ready marker must be treated as absent"
    );
    let (ready_previous, _) = status_reconciliation_projection(
        2,
        2,
        real.ready_present,
        persisted,
        real.current_build,
        observed_outcome(&real).0,
        observed_outcome(&real).1,
    );
    let (_, ready_bug) =
        status_reconciliation_projection(2, 2, false, persisted, 2, persisted, false);
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &ready_previous,
        &ready_bug,
        Some("ReconcileStatus"),
        "status reconciliation strict-newer negative control",
    );
    assert!(!admitted, "healthy model admitted stale Ready prose: {why}");
    assert!(
        buggy
            .successors("ReconcileOnMarkerPresence", &ready_previous)
            .contains(&ready_bug),
        "Buggy=1 must reproduce the mere-presence defect"
    );
    assert!(!buggy.check_invariant("AbsentReadyCannotAdvertiseStage", &ready_bug));

    for (running, ledger, ready, persisted, mutant, law) in [
        (
            1,
            2,
            Some(2),
            "staged 0.2 (build 2) — applies on next launch",
            "NeutralizeDespiteReady",
            "ReadyPreservesPersistedOutcome",
        ),
        (
            2,
            2,
            None,
            "up to date (latest release build 2)",
            "NeutralizeEveryAbsentReady",
            "HonestTerminalOutcomeIsPreserved",
        ),
    ] {
        let real = reconcile_status_outcome(running, ledger, ready, persisted.to_string());
        let (reported, neutralized) = observed_outcome(&real);
        assert!(!neutralized, "the real reducer preserves {persisted:?}");
        assert_eq!(reported, persisted);
        let (previous, preserved) = status_reconciliation_projection(
            running,
            ledger,
            real.ready_present,
            persisted,
            real.current_build,
            reported,
            neutralized,
        );
        let rewritten = buggy.successors(mutant, &previous)[0].clone();
        assert_ne!(rewritten, preserved);
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &previous,
            &rewritten,
            Some("ReconcileStatus"),
            "status reconciliation over-correction negative control",
        );
        assert!(!admitted, "healthy model admitted {mutant}: {why}");
        assert!(
            !buggy.check_invariant(law, &rewritten),
            "{mutant} escapes {law}"
        );
    }
}

#[test]
fn status_reconciliation_never_reports_an_old_build_or_absent_stage() {
    let cases = [
        (
            53,
            54,
            None,
            "installed 0.54 (build 54); activating now",
            false,
        ),
        (
            53,
            53,
            None,
            "skipping re-stage of build 54 for another 5m; NOT skipping apply: \
             staged 0.54 (build 54) is verified and ready to apply",
            false,
        ),
    ];
    for (running, ledger, ready, persisted, preserve) in cases {
        let reconciliation =
            reconcile_status_outcome(running, ledger, ready, persisted.to_string());
        assert_eq!(reconciliation.current_build, running);
        let got = reconciliation.outcome.into_string();
        if preserve {
            assert_eq!(got, persisted);
        } else {
            let expected = if ledger == running {
                "no update is staged"
            } else {
                "no update is staged (checked from another build of aterm)"
            };
            assert_eq!(got, expected);
            assert!(!got.contains("applies on next launch"));
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn ledger_reader_rejects_oversized_and_non_regular_inputs() {
    let dir = crate::paths::scratch_dir("ledger-cap");
    let valid = dir.join("valid.toml");
    std::fs::write(&valid, b"enabled = true\n").unwrap();
    assert_eq!(
        super::read_ledger_text(&valid).as_deref(),
        Some("enabled = true\n")
    );

    let oversized = dir.join("oversized.toml");
    let file = std::fs::File::create(&oversized).unwrap();
    file.set_len(super::MAX_LEDGER_BYTES + 1).unwrap();
    assert!(super::read_ledger_text(&oversized).is_none());
    assert!(super::read_ledger_text(&dir).is_none());
    let link = dir.join("link.toml");
    std::os::unix::fs::symlink(&valid, &link).unwrap();
    assert!(super::read_ledger_text(&link).is_none());
    assert!(super::read_ledger_text_checked(&link).is_err());
}

#[test]
fn commit_matches_classifies_every_pairing() {
    const FULL: &str = "4e91d3334041788ad92f5b6568cb648cb25805d6";
    for (a, b, matches, why) in [
        (
            "4e91d3334041",
            FULL,
            true,
            "12-hex binary stamp vs 40-hex appcast commit",
        ),
        (FULL, "4e91d3334041", true, "order must not matter"),
        (
            "  4E91D3334041 ",
            FULL,
            true,
            "case and surrounding whitespace are ignored",
        ),
        (FULL, FULL, true, "identical full hashes"),
        (
            "4e91d3334041",
            "deadbeefcafe0000000000000000000000000000",
            false,
            "different commits",
        ),
        ("4e91d3334041-dirty", FULL, false, "dirty vs its own base"),
        (
            "4e91d3334041-dirty",
            "4e91d3334041-dirty",
            false,
            "dirty vs itself",
        ),
        ("unknown", FULL, false, "the unknown stamp"),
        ("", FULL, false, "empty"),
        (
            "-",
            FULL,
            false,
            "the control socket's no-staged-commit sentinel",
        ),
        ("nothexatall12", FULL, false, "non-hex"),
        (
            "4e91d3",
            FULL,
            false,
            "6 hex is below git's abbreviation floor",
        ),
        (
            "4e91d33",
            FULL,
            true,
            "7 hex is git's floor and is accepted",
        ),
    ] {
        assert_eq!(commit_matches(a, b), matches, "{why}: {a:?} vs {b:?}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_second_persistent_class_still_speaks_but_the_same_one_stays_quiet() {
    assert!(!persistent_notice_is_owed(None, None));
    assert!(!persistent_notice_is_owed(Some("pipeline"), None));
    assert!(persistent_notice_is_owed(None, Some("pipeline")));
    assert!(!persistent_notice_is_owed(
        Some("pipeline"),
        Some("pipeline")
    ));
    assert!(persistent_notice_is_owed(Some("pipeline"), Some("apply")));
    assert!(!persistent_notice_is_owed(Some("apply"), Some("apply")));
    assert!(persistent_notice_is_owed(Some("apply"), Some("pipeline")));
    assert!(!persistent_notice_is_owed(Some("pipeline"), Some("stage")));
    assert!(!persistent_notice_is_owed(Some("stage"), Some("pipeline")));
}

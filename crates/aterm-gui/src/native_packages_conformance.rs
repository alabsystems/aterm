// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the Settings Packages worker reducer.
//!
//! The test drives the genuine shipping [`PackagesService`], projects its
//! single-flight state and rendered result class, and validates each accepted
//! transition against the derived `NativePackagesWorker` model. Rejected stale
//! and wrong-operation completions plus a false-success presentation are
//! independent negative controls.

#![cfg(test)]

use aterm_spec::derive::{Model, native_packages_worker_model};
use aterm_spec::interp::{State, admits};

use crate::packages_screen::{
    PackagesBusy, PackagesCommandOutcome, PackagesModelState, PackagesService,
    PackagesStatusReport, PackagesWorkerCompletion,
};

fn report(outcome: &str) -> PackagesStatusReport {
    report_with_rows(outcome, &[])
}

/// A report whose record carries `rows` (program, state) as a pass wrote them.
fn report_with_rows(outcome: &str, rows: &[(&str, &str)]) -> PackagesStatusReport {
    let programs = rows
        .iter()
        .map(|(name, state)| {
            (
                (*name).to_string(),
                atpkg::ProgramStatus {
                    installed_build: Some(1),
                    state: (*state).to_string(),
                    tree_root: String::new(),
                },
            )
        })
        .collect();
    let status = atpkg::Status {
        schema: 1,
        updated_at: "2026-07-21T00:00:00Z".to_string(),
        enabled: true,
        index_source: "alabsystems/aterm".to_string(),
        outcome: outcome.to_string(),
        seams: Vec::new(),
        last_success_at: String::new(),
        last_index_reached_at: String::new(),
        last_index_build: 0,
        index_build_changed_at: String::new(),
        last_pass: String::new(),
        last_pass_at: String::new(),
        last_pass_attempted_index_build: 0,
        last_pass_attempted_at: String::new(),
        metered_hold_until: String::new(),
        programs,
        extra: Default::default(),
    };
    PackagesStatusReport::from_parts(true, true, "fp".to_string(), Some(&status), &[])
}

fn project(model: &Model, real: PackagesModelState) -> State {
    let mut state = model.init_state();
    state.insert(
        "sequence",
        i64::try_from(real.sequence).expect("bounded conformance sequence"),
    );
    state.insert("inflight", i64::from(real.inflight));
    state.insert("operation", i64::from(real.operation));
    state.insert("observed", i64::from(real.observed));
    state.insert("last_operation", i64::from(real.last_operation));
    state.insert("last_result", i64::from(real.last_result));
    state.insert("presented_result", i64::from(real.presented_result));
    state
}

fn assert_transition(
    model: &Model,
    before: PackagesModelState,
    after: PackagesModelState,
    action: &'static str,
) {
    let before = project(model, before);
    let after = project(model, after);
    assert_eq!(
        model.successors(action, &before).as_slice(),
        std::slice::from_ref(&after),
        "real Packages transition must conform specifically to {action}"
    );
    assert_eq!(admits(model, &before, &after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &after),
            "post-state violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
}

#[test]
fn real_packages_service_conforms_for_refresh_failure_preservation_and_success() {
    let model = native_packages_worker_model();
    let mut service = PackagesService::new();
    assert_eq!(project(&model, service.model_state()), model.init_state());

    let before = service.model_state();
    let refresh_sequence = service.begin(None).expect("start refresh");
    assert_transition(&model, before, service.model_state(), "BeginRefresh");
    let before = service.model_state();
    assert!(service.finish(
        refresh_sequence,
        PackagesWorkerCompletion::refresh(report("up to date")),
    ));
    assert_transition(&model, before, service.model_state(), "FinishRefresh");

    let before = service.model_state();
    let check_sequence = service
        .begin(Some(PackagesBusy::Check))
        .expect("start package check");
    assert_transition(&model, before, service.model_state(), "BeginCheck");

    // A completion for the wrong kind cannot clear the genuine reservation.
    let reserved = service.model_state();
    assert!(!service.finish(
        check_sequence,
        PackagesWorkerCompletion::refresh(report("up to date")),
    ));
    assert_eq!(service.model_state(), reserved);
    assert!(
        model
            .successors("FinishRefresh", &project(&model, reserved))
            .is_empty(),
        "the model guard rejects the same wrong-kind completion"
    );

    // Nor may an otherwise matching completion with a stale sequence settle it.
    assert!(!service.finish(
        check_sequence + 1,
        PackagesWorkerCompletion::command(
            report("up to date"),
            PackagesCommandOutcome::Failed {
                operation: PackagesBusy::Check,
                message: "stale failure".to_string(),
            },
        ),
    ));
    assert_eq!(service.model_state(), reserved);

    let before = service.model_state();
    assert!(service.finish(
        check_sequence,
        PackagesWorkerCompletion::command(
            // Deliberately retain the old success: the process outcome must win.
            report("up to date"),
            PackagesCommandOutcome::Failed {
                operation: PackagesBusy::Check,
                message: "atpkg update exited with status 7".to_string(),
            },
        ),
    ));
    let failed = service.model_state();
    assert_transition(&model, before, failed, "FinishCheckFailure");
    assert_eq!(failed.last_result, 2);
    assert_eq!(failed.presented_result, 2);

    // A status-only pass imports fresh facts without erasing that user result.
    let before = service.model_state();
    let refresh_sequence = service.begin(None).unwrap();
    assert_transition(&model, before, service.model_state(), "BeginRefresh");
    let before = service.model_state();
    assert!(service.finish(
        refresh_sequence,
        PackagesWorkerCompletion::refresh(report("up to date")),
    ));
    assert_transition(&model, before, service.model_state(), "FinishRefresh");
    assert_eq!(service.model_state().last_result, 2);

    let before = service.model_state();
    let install_sequence = service.begin(Some(PackagesBusy::Install)).unwrap();
    assert_transition(&model, before, service.model_state(), "BeginInstall");
    let before = service.model_state();
    assert!(service.finish(
        install_sequence,
        PackagesWorkerCompletion::command(
            report("installed"),
            PackagesCommandOutcome::Succeeded {
                operation: PackagesBusy::Install,
            },
        ),
    ));
    assert_transition(
        &model,
        before,
        service.model_state(),
        "FinishInstallSuccess",
    );

    let before = service.model_state();
    let _aborted_sequence = service.begin(Some(PackagesBusy::Check)).unwrap();
    assert_transition(&model, before, service.model_state(), "BeginCheck");
    let before = service.model_state();
    assert!(service.abort(before.sequence));
    assert_transition(&model, before, service.model_state(), "Abort");

    // Independent negative control: the historical stale-success rendering is
    // neither invariant-safe nor an admitted post-state for the real failure.
    let mut false_success = project(&model, failed);
    false_success.insert("presented_result", 1);
    assert!(!model.check_invariant("FinalResultIsPresented", &false_success));
}

/// A FAILING ROW DOES NOT SPEAK FOR A VERB (2026-09-22). The Packages badge's
/// attention headline (`Needs attention: codex refused` — a refusal named as one since
/// Phase 4, 2026-09-23) is what a page with no verb
/// result shows; a verb that completed over the same failing record keeps its own
/// headline, so the model's `FinalResultIsPresented` binds the shipping page with
/// failing rows in it — not only the row-less reports above. Negative control: the
/// rendering this slice first shipped, the attention line in place of a completed
/// check's headline, is the state the invariant refuses.
#[test]
fn a_failing_row_is_presented_as_attention_never_as_a_verb_result() {
    use crate::packages_screen::{ATTENTION_PREFIX, presented_result_of};
    let model = native_packages_worker_model();
    let failing = || report_with_rows("1 failed", &[("codex", "error: codex refused: x")]);
    let mut service = PackagesService::new();
    let headline =
        |service: &PackagesService| service.state(true, true, false).projection().headline;

    let before = service.model_state();
    let sequence = service.begin(None).expect("start refresh");
    assert_transition(&model, before, service.model_state(), "BeginRefresh");
    let before = service.model_state();
    assert!(service.finish(sequence, PackagesWorkerCompletion::refresh(failing())));
    assert_transition(&model, before, service.model_state(), "FinishRefresh");
    assert_eq!(
        headline(&service),
        format!("{ATTENTION_PREFIX}codex refused"),
        "not vacuous: the attention headline IS on the page, presenting no result"
    );

    let before = service.model_state();
    let sequence = service
        .begin(Some(PackagesBusy::Check))
        .expect("start package check");
    assert_transition(&model, before, service.model_state(), "BeginCheck");
    let before = service.model_state();
    assert!(service.finish(
        sequence,
        PackagesWorkerCompletion::command(
            failing(),
            PackagesCommandOutcome::Succeeded {
                operation: PackagesBusy::Check,
            },
        ),
    ));
    let completed = service.model_state();
    assert_transition(&model, before, completed, "FinishCheckSuccess");
    assert_eq!(headline(&service), "Package check completed");

    let mut attention_over_success = project(&model, completed);
    attention_over_success.insert(
        "presented_result",
        i64::from(presented_result_of(&format!(
            "{ATTENTION_PREFIX}codex refused"
        ))),
    );
    assert!(
        !model.check_invariant("FinalResultIsPresented", &attention_over_success),
        "the attention line in place of a completed check's headline is refused"
    );
}

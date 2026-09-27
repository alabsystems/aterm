// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the Settings Packages worker reducer.
//!
//! The test drives the genuine shipping [`PackagesService`], projects its
//! single-flight state and rendered result class, and validates each accepted
//! transition against the derived `NativePackagesWorker` model. The one fact the
//! service cannot report about itself, the outcome the last verb's worker
//! reported, is kept beside it by [`Packages`]. Rejected stale and
//! wrong-operation completions, a false-success presentation, an abort that
//! leaves its verb busy, and a refresh that erases the verb's result are
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
        last_index_build: 0,
        index_build_changed_at: String::new(),
        last_pass: String::new(),
        last_pass_at: String::new(),
        last_pass_attempted_index_build: 0,
        last_pass_attempted_at: String::new(),
        pass_seq: 0,
        programs,
        extra: Default::default(),
    };
    PackagesStatusReport::from_parts(true, true, "fp".to_string(), Some(&status), &[])
}

/// The genuine service plus `expected_result`: the outcome (1 success, 2
/// failure) the last verb's worker reported, cleared when a verb begins. A
/// refresh reports none, so it leaves `expected_result` alone, and the service
/// must too (`RefreshKeepsVerbResult`).
struct Packages {
    service: PackagesService,
    expected_result: u8,
}

/// One observation: the service's projection and the environment's fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Observed {
    real: PackagesModelState,
    expected_result: u8,
}

impl Packages {
    fn new() -> Self {
        Self {
            service: PackagesService::new(),
            expected_result: 0,
        }
    }

    fn observe(&self) -> Observed {
        Observed {
            real: self.service.model_state(),
            expected_result: self.expected_result,
        }
    }

    fn begin(&mut self, busy: Option<PackagesBusy>) -> Option<u64> {
        let sequence = self.service.begin(busy);
        if sequence.is_some() && busy.is_some() {
            self.expected_result = 0;
        }
        sequence
    }

    fn finish(&mut self, sequence: u64, completion: PackagesWorkerCompletion) -> bool {
        let reported = completion.command.as_ref().map(|command| match command {
            PackagesCommandOutcome::Succeeded { .. } => 1,
            PackagesCommandOutcome::Failed { .. } => 2,
        });
        let accepted = self.service.finish(sequence, completion);
        if let (true, Some(result)) = (accepted, reported) {
            self.expected_result = result;
        }
        accepted
    }

    fn abort(&mut self, sequence: u64) -> bool {
        self.service.abort(sequence)
    }
}

fn project(model: &Model, observed: Observed) -> State {
    let Observed {
        real,
        expected_result,
    } = observed;
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
    state.insert("expected_result", i64::from(expected_result));
    state
}

fn assert_transition(model: &Model, before: Observed, after: Observed, action: &'static str) {
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
    let mut packages = Packages::new();
    assert_eq!(project(&model, packages.observe()), model.init_state());

    let before = packages.observe();
    let refresh_sequence = packages.begin(None).expect("start refresh");
    assert_transition(&model, before, packages.observe(), "BeginRefresh");
    let before = packages.observe();
    assert!(packages.finish(
        refresh_sequence,
        PackagesWorkerCompletion::refresh(report("up to date")),
    ));
    assert_transition(&model, before, packages.observe(), "FinishRefresh");

    let before = packages.observe();
    let check_sequence = packages
        .begin(Some(PackagesBusy::Check))
        .expect("start package check");
    assert_transition(&model, before, packages.observe(), "BeginCheck");

    // A completion for the wrong kind cannot clear the genuine reservation.
    let reserved = packages.observe();
    assert!(!packages.finish(
        check_sequence,
        PackagesWorkerCompletion::refresh(report("up to date")),
    ));
    assert_eq!(packages.observe(), reserved);
    assert!(
        model
            .successors("FinishRefresh", &project(&model, reserved))
            .is_empty(),
        "the model guard rejects the same wrong-kind completion"
    );

    // Nor may an otherwise matching completion with a stale sequence settle it.
    assert!(!packages.finish(
        check_sequence + 1,
        PackagesWorkerCompletion::command(
            report("up to date"),
            PackagesCommandOutcome::Failed {
                operation: PackagesBusy::Check,
                message: "stale failure".to_string(),
            },
        ),
    ));
    assert_eq!(packages.observe(), reserved);

    let before = packages.observe();
    assert!(packages.finish(
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
    let failed = packages.observe();
    assert_transition(&model, before, failed, "FinishCheckFailure");
    assert_eq!(failed.real.last_result, 2);
    assert_eq!(failed.real.presented_result, 2);

    // A status-only pass imports fresh facts without erasing that user result.
    let before = packages.observe();
    let refresh_sequence = packages.begin(None).unwrap();
    assert_transition(&model, before, packages.observe(), "BeginRefresh");
    let before = packages.observe();
    assert!(packages.finish(
        refresh_sequence,
        PackagesWorkerCompletion::refresh(report("up to date")),
    ));
    let refreshed = packages.observe();
    assert_transition(&model, before, refreshed, "FinishRefresh");
    assert_eq!(refreshed.real.last_result, 2);

    // Negative control: a `finish` that assigns the refresh's absent command
    // over `last_command` erases the verb's result. Neither admitted nor safe.
    let mut erased = project(&model, refreshed);
    erased.insert("last_operation", 0);
    erased.insert("last_result", 0);
    erased.insert("presented_result", 0);
    assert_eq!(admits(&model, &project(&model, before), &erased), None);
    assert!(!model.check_invariant("RefreshKeepsVerbResult", &erased));

    let before = packages.observe();
    let install_sequence = packages.begin(Some(PackagesBusy::Install)).unwrap();
    assert_transition(&model, before, packages.observe(), "BeginInstall");
    let before = packages.observe();
    assert!(packages.finish(
        install_sequence,
        PackagesWorkerCompletion::command(
            report("installed"),
            PackagesCommandOutcome::Succeeded {
                operation: PackagesBusy::Install,
            },
        ),
    ));
    assert_transition(&model, before, packages.observe(), "FinishInstallSuccess");

    let before = packages.observe();
    let _aborted_sequence = packages.begin(Some(PackagesBusy::Check)).unwrap();
    assert_transition(&model, before, packages.observe(), "BeginCheck");
    let before = packages.observe();
    assert!(packages.abort(before.real.sequence));
    let aborted = packages.observe();
    assert_transition(&model, before, aborted, "Abort");
    assert_eq!(packages.service.busy(), None, "abort releases the verb too");

    // Negative control: an abort that clears `inflight` but keeps `busy` would
    // project the check as still running with no worker behind it.
    let mut stuck_busy = project(&model, aborted);
    stuck_busy.insert("operation", 2);
    assert_eq!(admits(&model, &project(&model, before), &stuck_busy), None);
    assert!(!model.check_invariant("SingleFlightHasOneKind", &stuck_busy));

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
    let mut packages = Packages::new();
    let headline =
        |service: &PackagesService| service.state(true, true, false).projection().headline;

    let before = packages.observe();
    let sequence = packages.begin(None).expect("start refresh");
    assert_transition(&model, before, packages.observe(), "BeginRefresh");
    let before = packages.observe();
    assert!(packages.finish(sequence, PackagesWorkerCompletion::refresh(failing())));
    assert_transition(&model, before, packages.observe(), "FinishRefresh");
    assert_eq!(
        headline(&packages.service),
        format!("{ATTENTION_PREFIX}codex refused"),
        "not vacuous: the attention headline IS on the page, presenting no result"
    );

    let before = packages.observe();
    let sequence = packages
        .begin(Some(PackagesBusy::Check))
        .expect("start package check");
    assert_transition(&model, before, packages.observe(), "BeginCheck");
    let before = packages.observe();
    assert!(packages.finish(
        sequence,
        PackagesWorkerCompletion::command(
            failing(),
            PackagesCommandOutcome::Succeeded {
                operation: PackagesBusy::Check,
            },
        ),
    ));
    let completed = packages.observe();
    assert_transition(&model, before, completed, "FinishCheckSuccess");
    assert_eq!(headline(&packages.service), "Package check completed");

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

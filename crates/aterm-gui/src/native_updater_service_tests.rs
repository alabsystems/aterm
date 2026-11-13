// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const TEST_DIGEST: &str = "abababababababababababababababababababababababababababababababab";
const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A reconcile read before `note_apply_failure`'s write cannot lower the apply lane.
#[test]
fn a_reconcile_older_than_the_failure_cannot_lower_the_apply_lane() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    let _ = service.absorb_failure_state(&status(Some(11)));

    let recorded = aterm_update::RecordedApplyFailure {
        apply_failures: 1,
        failures_for_target: 1,
        target_build: 11,
        reason: "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
        persistent: false,
    };
    assert!(
        service.note_apply_failure(&recorded),
        "precondition: the failure is recorded and published"
    );
    assert_eq!(service.snapshot().apply_failures_for_target, 1);

    let mut stale = status(Some(11));
    stale.apply_failure_build = 11;
    stale.apply_failures_for_target = 0;
    stale.failing_applies = 0;
    stale.apply_failure = String::new();
    service.absorb_failure_state(&stale);

    assert_eq!(
        service.snapshot().apply_failures_for_target,
        1,
        "a reconcile older than the failure lowered the artifact-scoped count"
    );
    assert!(
        !service.snapshot().apply_failure.is_empty(),
        "a reconcile older than the failure erased the reason the surfaces render"
    );
}

/// The last-check time follows every reduced check and every reconcile (background
/// checks write only the ledger), forward only.
#[test]
fn the_last_check_time_follows_checks_and_reconciles_forward_only() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    assert_eq!(service.snapshot().checked_at, None);
    let CheckStart::Start(ticket) = service.request_check() else {
        panic!("an idle service starts a check");
    };
    let mut checked = status(None);
    checked.checked_at = Some(1_000);
    service.finish_check(ticket, checked);
    assert_eq!(service.snapshot().checked_at, Some(1_000));
    assert!(service.note_checked_at(Some(2_000)));
    assert_eq!(service.snapshot().checked_at, Some(2_000));
    assert!(
        !service.note_checked_at(Some(1_500)),
        "an older read is not news"
    );
    assert!(!service.note_checked_at(None));
    assert_eq!(service.snapshot().checked_at, Some(2_000));
}

fn status(staged_build: Option<u64>) -> DurableUpdateStatus {
    DurableUpdateStatus {
        linux_host: false,
        linux: None,
        enabled: true,
        current_build: 10,
        staged_build,
        staged_version: staged_build.map(|build| format!("1.0.{build}")),
        staged_commit: Some(TEST_COMMIT.to_string()),
        staged_dmg_sha256: staged_build.map(|_| TEST_DIGEST.to_string()),
        changelog: Some("# Better\n\nFast and safe.".to_string()),
        outcome: if staged_build.is_some() {
            "staged".to_string()
        } else {
            "up to date".to_string()
        },
        failing_checks: 0,
        failing_persistent: false,
        failing_kind: String::new(),
        failing_applies: 0,
        apply_failure: String::new(),
        apply_failure_build: 0,
        apply_failures_for_target: 0,
        installable: Some(true),
        channel_unreadable: false,
        checked_at: None,
    }
}

#[test]
fn linux_update_status_survives_reduction_without_macos_apply_authority() {
    let mut durable = status(Some(12));
    durable.linux_host = true;
    durable.linux = Some(aterm_update::LinuxUpdateStatus {
        installed_build: 10,
        installed_version: String::new(),
        staged_build: Some(12),
        staged_version: Some("1.0.12".into()),
        staged_commit: Some(TEST_COMMIT.into()),
        trial_phase: None,
        trial_starts: 0,
        trial_healthy: false,
        refused_newer: false,
    });
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    let CheckStart::Start(ticket) = service.request_check() else {
        panic!("check admitted")
    };
    assert_eq!(
        service.finish_check(ticket, durable.clone()),
        CheckCompletion::Reduced
    );
    assert_eq!(service.snapshot().linux, durable.linux);
    assert!(service.snapshot().linux_host);
    assert!(service.snapshot().staged.is_none());
    assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
}

fn installed(
    build: u64,
    commit: &str,
    receipt_build: Option<u64>,
    receipt_digest: Option<&str>,
) -> InstalledUpdate {
    InstalledUpdate {
        build,
        commit: commit.to_string(),
        version: None,
        receipt_build,
        receipt_dmg_sha256: receipt_digest.map(str::to_string),
        trial_launches: 0,
    }
}

fn exact_installed(build: u64) -> InstalledUpdate {
    installed(
        build,
        "0123456789abcdef0123456789abcdef01234567",
        Some(build),
        Some(TEST_DIGEST),
    )
}

fn started(service: &mut NativeUpdaterService) -> UpdaterWorkTicket {
    match service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected a new check, got {other:?}"),
    }
}

fn stage(service: &mut NativeUpdaterService, build: u64) -> UpdaterWorkTicket {
    let ticket = started(service);
    assert_eq!(
        service.finish_check(ticket, status(Some(build))),
        CheckCompletion::Reduced
    );
    ticket
}

#[test]
fn failed_reexec_rearms_only_the_exact_verified_stage() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    stage(&mut service, 11);
    let preflight = match service.begin_apply_preflight() {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("expected preflight, got {other:?}"),
    };
    let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
        ApplyDecision::Execute(command) => command,
        other => panic!("expected execute decision, got {other:?}"),
    };
    let attempt = command.attempt();
    assert!(service.abort_apply(&attempt, "exec returned EIO"));
    assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
    assert_eq!(service.snapshot().reexec_count, 0);
    assert_eq!(
        service.snapshot().staged.as_ref().map(|stage| stage.build),
        Some(11)
    );
    assert!(
        service
            .snapshot()
            .error
            .as_deref()
            .is_some_and(|error| error.contains("EIO"))
    );

    let retry = match service.begin_apply_preflight() {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("failed attempt must stay retryable, got {other:?}"),
    };
    let retry_command = match service.finish_apply_preflight(retry, ClosePreflight::Ready) {
        ApplyDecision::Execute(command) => command,
        other => panic!("expected retry command, got {other:?}"),
    };
    assert_ne!(retry_command.attempt(), attempt);
    assert_eq!(
        service.finish_returned_apply(
            &attempt,
            ReturnedApplyFacts::new(true, Some(11), Some(TEST_COMMIT), Some(TEST_DIGEST), None,),
            "replayed failure from first attempt"
        ),
        ReturnedApplyDisposition::Ignored,
        "an async completion carries its exact ticket, never current-by-lookup authority"
    );
    assert_eq!(service.snapshot().phase, UpdaterPhase::Applying);
}

/// A durable status shaped the way the App imports an installed-bundle
/// activation: the sealed identity under `installed_activation_digest`.
fn activation_status(build: u64) -> DurableUpdateStatus {
    let mut status = status(Some(build));
    status.staged_dmg_sha256 = Some(installed_activation_digest(build, TEST_COMMIT));
    status
}

fn stage_activation(service: &mut NativeUpdaterService, build: u64) -> UpdaterWorkTicket {
    let ticket = started(service);
    assert_eq!(
        service.finish_check(ticket, activation_status(build)),
        CheckCompletion::Reduced
    );
    assert!(
        service
            .snapshot()
            .staged
            .as_ref()
            .is_some_and(StagedUpdate::is_installed_activation),
        "the imported stage carries the activation identity"
    );
    ticket
}

/// An installed bundle seals a 12-char short sha: a usable identity for an activation
/// (the durable import and the activation constructor share one rule), never for a
/// download.
#[test]
fn a_short_sha_is_a_usable_stage_identity_and_a_stub_is_not() {
    assert!(usable_commit_identity("248091d23ab0"));
    assert!(usable_commit_identity(" 248091D23AB0 "));
    assert!(usable_commit_identity(&"a".repeat(40)));
    assert!(
        !usable_commit_identity("248091"),
        "six hex is not an identity"
    );
    assert!(!usable_commit_identity(&"a".repeat(41)));
    assert!(!usable_commit_identity("unknown"));
    assert!(!usable_commit_identity("248091d23ab0-dirty"));
    let mut download = DurableUpdateStatus {
        linux_host: false,
        linux: None,
        enabled: true,
        current_build: 11,
        staged_build: Some(12),
        staged_version: Some("0.27.0".to_string()),
        staged_commit: Some("248091d23ab0".to_string()),
        staged_dmg_sha256: Some("ab".repeat(32)),
        changelog: None,
        outcome: String::new(),
        failing_checks: 0,
        failing_persistent: false,
        failing_kind: String::new(),
        failing_applies: 0,
        apply_failure: String::new(),
        apply_failure_build: 0,
        apply_failures_for_target: 0,
        installable: Some(true),
        channel_unreadable: false,
        checked_at: None,
    };
    assert!(staged_from_status(11, 0, &download).is_none());
    download.staged_commit = Some("0".repeat(40));
    assert!(staged_from_status(11, 0, &download).is_some());

    let installed = InstalledUpdate {
        build: 12,
        commit: "248091D23AB0".to_string(),
        version: Some("0.27.0".to_string()),
        receipt_build: None,
        receipt_dmg_sha256: None,
        trial_launches: 0,
    };
    let stage = installed
        .activation_stage(11, 0)
        .expect("a short-sha bundle newer than the process is an activation");
    assert_eq!(stage.commit.as_deref(), Some("248091d23ab0"), "lowercased");
    assert!(stage.is_installed_activation());
    let status = DurableUpdateStatus {
        linux_host: false,
        linux: None,
        enabled: true,
        current_build: 11,
        staged_build: Some(12),
        staged_version: Some("0.27.0".to_string()),
        staged_commit: stage.commit.clone(),
        staged_dmg_sha256: Some(stage.dmg_sha256.clone()),
        changelog: None,
        outcome: String::new(),
        failing_checks: 0,
        failing_persistent: false,
        failing_kind: String::new(),
        failing_applies: 0,
        apply_failure: String::new(),
        apply_failure_build: 0,
        apply_failures_for_target: 0,
        installable: Some(true),
        channel_unreadable: false,
        checked_at: None,
    };
    let imported = staged_from_status(11, 0, &status).expect("the import must accept it");
    assert_eq!(imported.commit, stage.commit);
    assert_eq!(imported.dmg_sha256, stage.dmg_sha256);
    let stub = InstalledUpdate {
        commit: "unknown".to_string(),
        ..installed
    };
    assert!(stub.activation_stage(11, 0).is_none());
}

/// NO UPDATE AUTHORITY NAMES A BUILD AT OR BELOW THE RUNNING ONE, so no lane hands a
/// session to a build older than the one that wrote its records: a download staged
/// there and an installed bundle there mint no stage and back no activation, and the
/// one same-image hop (the QA seam, a launch-record renewal) runs this very build.
#[test]
fn no_update_authority_names_a_build_at_or_below_the_running_one() {
    let running = 11;
    let commit = "0".repeat(40);
    let staged_at = |build: u64| DurableUpdateStatus {
        linux_host: false,
        linux: None,
        enabled: true,
        current_build: running,
        staged_build: Some(build),
        staged_version: Some(format!("0.{build}.0")),
        staged_commit: Some(commit.clone()),
        staged_dmg_sha256: Some("ab".repeat(32)),
        changelog: None,
        outcome: String::new(),
        failing_checks: 0,
        failing_persistent: false,
        failing_kind: String::new(),
        failing_applies: 0,
        apply_failure: String::new(),
        apply_failure_build: 0,
        apply_failures_for_target: 0,
        installable: Some(true),
        channel_unreadable: false,
        checked_at: None,
    };
    let installed_at = |build: u64| InstalledUpdate {
        build,
        commit: commit.clone(),
        version: Some(format!("0.{build}.0")),
        receipt_build: None,
        receipt_dmg_sha256: None,
        trial_launches: 0,
    };
    for build in [running - 1, running] {
        assert!(
            staged_from_status(running, 0, &staged_at(build)).is_none(),
            "a download of build {build} is no stage"
        );
        assert!(verified_download_build(running, &staged_at(build)).is_none());
        assert!(
            installed_at(build).activation_stage(running, 0).is_none(),
            "an installed build {build} is no activation"
        );
        assert!(!installed_at(build).backs_activation(running, build, &commit));
    }
    let newer = running + 1;
    assert!(staged_from_status(running, 0, &staged_at(newer)).is_some());
    assert!(installed_at(newer).activation_stage(running, 0).is_some());
    assert!(installed_at(newer).backs_activation(running, newer, &commit));
}

/// While a stage is held `finish_check` cannot run, so the verdict arrives by reconcile:
/// badged once on the false→true edge, idempotent on repeats, cleared when the ledger
/// heals.
#[test]
fn absorbing_a_persistent_failure_while_staged_badges_once_and_is_idempotent() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    stage(&mut service, 11);
    let rev0 = service.snapshot().revision;
    let mut failing = status(Some(11));
    failing.failing_persistent = true;
    failing.failing_kind = "apply".to_string();
    assert!(service.absorb_failure_state(&failing));
    let snap = service.snapshot();
    assert!(snap.failing_persistent && snap.failing_kind == "apply");
    assert!(snap.staged.is_some(), "the stage is untouched");
    assert_eq!(
        snap.attention_revision,
        Some(snap.revision),
        "badged on the edge"
    );
    assert!(snap.revision > rev0);
    let rev1 = snap.revision;
    assert!(
        !service.absorb_failure_state(&failing),
        "a repeat changes nothing"
    );
    assert_eq!(
        service.snapshot().revision,
        rev1,
        "no revision churn per cycle"
    );
    let healed = status(Some(11));
    assert!(service.absorb_failure_state(&healed));
    assert!(!service.snapshot().failing_persistent);
}

#[test]
fn the_activation_identity_is_well_formed_pure_and_distinct() {
    let a = installed_activation_digest(11, TEST_COMMIT);
    assert_eq!(a.len(), 64);
    assert!(
        a.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(a, installed_activation_digest(11, TEST_COMMIT), "pure");
    assert_eq!(
        a,
        installed_activation_digest(11, &format!("  {} \n", TEST_COMMIT.to_ascii_uppercase())),
        "commit case and surrounding whitespace do not fork the identity"
    );
    let ticket = ApplyAttemptTicket::for_test(11, TEST_COMMIT, &a);
    assert!(ticket.is_installed_activation());
    let mut flipped = a.into_bytes();
    flipped[0] = if flipped[0] == b'0' { b'1' } else { b'0' };
    let flipped = String::from_utf8(flipped).expect("ascii hex");
    assert!(
        !ApplyAttemptTicket::for_test(11, TEST_COMMIT, &flipped).is_installed_activation(),
        "negative control: a non-matching digest is not an installed activation"
    );
}

/// With no durable marker, an activation stage stands while the bundle under the
/// executable is still that build and newer, and retires otherwise.
#[test]
fn an_activation_stage_is_backed_by_the_installed_bundle_not_the_ready_marker() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    stage_activation(&mut service, 11);
    let backing = installed(11, TEST_COMMIT, None, None);
    assert_eq!(
        service.reconcile_durable_stage(true, None, None, None, Some(&backing)),
        DurableStageDisposition::Unchanged,
        "no durable marker is needed; the bundle is the backing"
    );
    assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
    let moved_on = installed(12, TEST_COMMIT, None, None);
    assert_eq!(
        service.reconcile_durable_stage(true, None, None, None, Some(&moved_on)),
        DurableStageDisposition::Retired
    );
    assert!(service.snapshot().staged.is_none());
    assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
    assert_eq!(
        service.last_transitions()[0].action,
        UpdaterTransitionAction::RetireStage
    );
    stage_activation(&mut service, 11);
    let rolled_back = installed(10, TEST_COMMIT, None, None);
    assert_eq!(
        service.reconcile_durable_stage(true, None, None, None, Some(&rolled_back)),
        DurableStageDisposition::Retired
    );
    stage_activation(&mut service, 11);
    assert_eq!(
        service.reconcile_durable_stage(true, None, None, None, None),
        DurableStageDisposition::Unchanged
    );
    assert!(
        service.snapshot().staged.is_some(),
        "an unobservable bundle must not throw the activation away"
    );
    let moved_on = installed(12, TEST_COMMIT, None, None);
    assert_eq!(
        service.reconcile_durable_stage(true, None, None, None, Some(&moved_on)),
        DurableStageDisposition::Retired
    );
}

#[test]
fn activation_observation_conforms_in_idle_and_returned_lanes() {
    let model = aterm_models_update::native_update_activation_observation_model();
    let observations = [
        (None, false),
        (Some(installed(11, TEST_COMMIT, None, None)), false),
        (Some(installed(12, TEST_COMMIT, None, None)), true),
        (Some(installed(11, &"f".repeat(40), None, None)), true),
    ];
    for returned in [false, true] {
        for (observation, changed) in &observations {
            let mut service = NativeUpdaterService::new(10, "1.0.10", true);
            stage_activation(&mut service, 11);
            let stage = service.snapshot().staged.clone();
            let mut before = model.init_state();
            if observation.is_some() {
                let action = if *changed {
                    "ObserveChange"
                } else {
                    "ObserveMatch"
                };
                before = model.successors(action, &before).remove(0);
            }
            if returned {
                before = model.successors("Return", &before).remove(0);
                let ApplyPreflightStart::Inspect(ticket) = service.begin_apply_preflight() else {
                    panic!("activation must enter preflight");
                };
                let ApplyDecision::Execute(command) =
                    service.finish_apply_preflight(ticket, ClosePreflight::Ready)
                else {
                    panic!("exact ready preflight must issue one apply authority");
                };
                let attempt = command.attempt();
                let disposition = service.finish_returned_apply(
                    &attempt,
                    ReturnedApplyFacts::new(true, None, None, None, observation.as_ref()),
                    "candidate timed out; installed-bundle probe may be unavailable",
                );
                assert_eq!(
                    disposition,
                    if *changed {
                        ReturnedApplyDisposition::Retired
                    } else {
                        ReturnedApplyDisposition::Rearmed
                    }
                );
                assert_eq!(
                    service.finish_returned_apply(
                        &attempt,
                        ReturnedApplyFacts::new(true, None, None, None, observation.as_ref()),
                        "duplicate completion",
                    ),
                    ReturnedApplyDisposition::Ignored
                );
            } else {
                assert_eq!(
                    service.reconcile_durable_stage(true, None, None, None, observation.as_ref()),
                    if *changed {
                        DurableStageDisposition::Retired
                    } else {
                        DurableStageDisposition::Unchanged
                    }
                );
            }
            let retained = service.snapshot().staged.is_some();
            let mut after = before.clone();
            after.insert("reduced", 1);
            after.insert("retained", i64::from(retained));
            assert_eq!(
                model.successors("Reduce", &before),
                vec![after.clone()],
                "real activation reducer must conform: returned={returned}, observation={observation:?}"
            );
            for invariant in &model.invariants {
                assert!(model.check_invariant(invariant.name, &after));
            }
            if retained {
                assert_eq!(service.snapshot().staged, stage);
                assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
                assert!(service.active_apply.is_none());
            }
            if returned && observation.is_none() {
                let mut historical = after;
                historical.insert("retained", 0);
                assert!(!model.successors("Reduce", &before).contains(&historical));
                assert!(!model.check_invariant("NoRetirementWithoutEvidence", &historical));
            }
        }
    }
}

/// Nothing else: an activation already in place, an in-flight apply and an empty reducer
/// keep their state.
#[test]
fn a_download_stage_retires_for_activation_but_nothing_else_does() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    assert!(
        !service.retire_stage_for_activation(),
        "nothing staged: no-op"
    );
    stage(&mut service, 11);
    assert!(
        service.retire_stage_for_activation(),
        "a download stage retires"
    );
    assert!(service.snapshot().staged.is_none());
    assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
    stage_activation(&mut service, 12);
    assert!(
        !service.retire_stage_for_activation(),
        "an activation stays"
    );
    assert!(service.snapshot().staged.is_some());
    let preflight = match service.begin_apply_preflight() {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("expected preflight, got {other:?}"),
    };
    let _command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
        ApplyDecision::Execute(command) => command,
        other => panic!("expected execute decision, got {other:?}"),
    };
    assert!(
        !service.retire_stage_for_activation(),
        "an in-flight apply is untouchable"
    );
    assert_eq!(service.snapshot().phase, UpdaterPhase::Applying);
}

#[test]
fn consumed_durable_stage_retires_memory_and_reports_installed_activation() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    stage(&mut service, 11);
    let preflight = match service.begin_apply_preflight() {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("expected preflight, got {other:?}"),
    };
    let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
        ApplyDecision::Execute(command) => command,
        other => panic!("expected execute decision, got {other:?}"),
    };
    let attempt = command.attempt();

    let installed = exact_installed(11);
    assert_eq!(
        service.finish_returned_apply(
            &attempt,
            ReturnedApplyFacts::new(true, None, None, None, Some(&installed)),
            "child failed readiness after bundle swap"
        ),
        ReturnedApplyDisposition::InstalledNeedsRelaunch { build: 11 }
    );
    assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
    assert!(service.snapshot().staged.is_none());
    assert_eq!(service.snapshot().reexec_count, 0);
    assert_eq!(
        service.last_transitions()[0].action,
        UpdaterTransitionAction::RetireApply
    );
    assert!(!service.abort_apply(&attempt, "stale retry callback"));
}

#[test]
fn durable_hex_identity_is_case_canonical_across_reconcile_and_returned_apply() {
    let upper_commit = TEST_COMMIT.to_ascii_uppercase();
    let upper_digest = TEST_DIGEST.to_ascii_uppercase();

    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    stage(&mut service, 11);
    assert_eq!(
        service.reconcile_durable_stage(
            true,
            Some(11),
            Some(&upper_commit),
            Some(&upper_digest),
            None,
        ),
        DurableStageDisposition::Unchanged,
        "hex letter case is not a different staged artifact"
    );
    assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);

    let (mut service, attempt) = applying_attempt();
    assert_eq!(
        service.finish_returned_apply(
            &attempt,
            ReturnedApplyFacts::new(
                true,
                Some(11),
                Some(&upper_commit),
                Some(&upper_digest),
                None,
            ),
            "exec returned after observing equivalent uppercase marker",
        ),
        ReturnedApplyDisposition::Rearmed,
        "a returned apply must rearm the same digest regardless of hex spelling"
    );
    assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
}

#[test]
fn another_process_consuming_stage_clears_stale_arrow_and_reports_install() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    stage(&mut service, 11);
    assert_eq!(
        service.reconcile_durable_stage(true, Some(11), Some(TEST_COMMIT), Some(TEST_DIGEST), None),
        DurableStageDisposition::Unchanged
    );
    assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);

    let installed = exact_installed(11);
    assert_eq!(
        service.reconcile_durable_stage(true, None, None, None, Some(&installed)),
        DurableStageDisposition::InstalledNeedsRelaunch { build: 11 }
    );
    assert_eq!(service.snapshot().phase, UpdaterPhase::Idle);
    assert!(service.snapshot().staged.is_none());
    assert!(service.snapshot().attention_revision.is_none());
    assert_eq!(
        service.last_transitions()[0].action,
        UpdaterTransitionAction::RetireStage
    );
}

fn applying_attempt() -> (NativeUpdaterService, ApplyAttemptTicket) {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    stage(&mut service, 11);
    let preflight = match service.begin_apply_preflight() {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("expected preflight, got {other:?}"),
    };
    let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
        ApplyDecision::Execute(command) => command,
        other => panic!("expected execute decision, got {other:?}"),
    };
    let attempt = command.attempt();
    (service, attempt)
}

#[test]
fn durable_installed_ticket_reconciliation_matrix_is_fail_closed() {
    let durable_cases = [
        (None, None, None),
        (Some(11), Some(TEST_COMMIT), Some(TEST_DIGEST)),
        (Some(11), Some("different-commit"), Some(TEST_DIGEST)),
        (
            Some(11),
            Some(TEST_COMMIT),
            Some("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"),
        ),
        (Some(12), Some("superseding-commit"), Some(TEST_DIGEST)),
    ];
    let installed_cases = [
        None,
        Some(installed(
            10,
            "0123456789abcdef",
            Some(11),
            Some(TEST_DIGEST),
        )),
        Some(exact_installed(11)),
        Some(installed(
            11,
            "fedcba9876543210fedcba9876543210fedcba98",
            Some(11),
            Some(TEST_DIGEST),
        )),
        Some(installed(
            11,
            "0123456789abcdef0123456789abcdef01234567",
            Some(11),
            Some("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"),
        )),
        Some(installed(
            12,
            "0123456789abcdef0123456789abcdef01234567",
            Some(12),
            Some(TEST_DIGEST),
        )),
    ];

    for durable_enabled in [false, true] {
        for (durable_build, durable_commit, durable_digest) in durable_cases {
            for installed in &installed_cases {
                for stale_ticket in [false, true] {
                    let (mut service, attempt) = applying_attempt();
                    let ticket = if stale_ticket {
                        ApplyAttemptTicket {
                            operation: attempt.operation.saturating_add(1),
                            ..attempt.clone()
                        }
                    } else {
                        attempt.clone()
                    };
                    let disposition = service.finish_returned_apply(
                        &ticket,
                        ReturnedApplyFacts::new(
                            durable_enabled,
                            durable_build,
                            durable_commit,
                            durable_digest,
                            installed.as_ref(),
                        ),
                        "matrix completion",
                    );
                    let installed_wins = installed.as_ref().is_some_and(|installed| {
                        installed.proves_artifact(11, Some(TEST_COMMIT), TEST_DIGEST)
                    });
                    let durable_exact = durable_enabled
                        && durable_build == Some(11)
                        && durable_commit == Some(TEST_COMMIT)
                        && durable_digest == Some(TEST_DIGEST);
                    let expected = if stale_ticket {
                        ReturnedApplyDisposition::Ignored
                    } else if installed_wins {
                        ReturnedApplyDisposition::InstalledNeedsRelaunch { build: 11 }
                    } else if durable_exact {
                        ReturnedApplyDisposition::Rearmed
                    } else {
                        ReturnedApplyDisposition::Retired
                    };
                    assert_eq!(
                        disposition, expected,
                        "enabled={durable_enabled} durable=({durable_build:?},{durable_commit:?},{durable_digest:?}) installed={installed:?} stale={stale_ticket}"
                    );
                    if stale_ticket {
                        assert_eq!(service.snapshot().phase, UpdaterPhase::Applying);
                        assert_eq!(service.snapshot().reexec_count, 1);
                    }
                }
            }
        }
    }
}

#[test]
fn disabled_status_never_imports_or_authorizes_a_leftover_stage() {
    let mut disabled = status(Some(11));
    disabled.enabled = false;
    let mut service = NativeUpdaterService::from_durable_status(10, "1.0.10", disabled);
    assert!(!service.snapshot().enabled);
    assert_eq!(service.snapshot().phase, UpdaterPhase::Disabled);
    assert!(service.snapshot().staged.is_none());
    assert_eq!(
        service.begin_apply_preflight(),
        ApplyPreflightStart::Disabled
    );

    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    let check = started(&mut service);
    let mut disabled_completion = status(Some(12));
    disabled_completion.enabled = false;
    assert_eq!(
        service.finish_check(check, disabled_completion),
        CheckCompletion::Reduced
    );
    assert!(!service.snapshot().enabled);
    assert_eq!(service.snapshot().phase, UpdaterPhase::Disabled);
    assert!(service.snapshot().staged.is_none());
    assert_eq!(
        service.request_check(),
        CheckStart::Rejected(CheckBlock::Disabled)
    );
    assert_eq!(
        service.begin_apply_preflight(),
        ApplyPreflightStart::Disabled
    );
}

#[test]
fn missing_or_unsealed_commit_never_mints_production_stage_authority() {
    let invalid = [
        None,
        Some(String::new()),
        Some("unknown".to_string()),
        Some("0123456".to_string()),
        Some("0123456789ab".to_string()),
        Some("0".repeat(39)),
        Some("0".repeat(41)),
        Some("0".repeat(64)),
        Some(format!("{}-dirty", TEST_COMMIT)),
        Some("not-hex!".to_string()),
    ];
    for commit in invalid {
        let mut durable = status(Some(11));
        durable.staged_commit = commit.clone();
        let service = NativeUpdaterService::from_durable_status(10, "1.0.10", durable);
        assert!(
            service.snapshot().staged.is_none(),
            "invalid commit {commit:?} must not produce apply authority"
        );
    }
}

#[test]
fn wrong_preflight_identity_cannot_authorize_reexec() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    stage(&mut service, 11);
    let ticket = match service.begin_apply_preflight() {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("expected preflight, got {other:?}"),
    };
    let forged = ApplyPreflightTicket {
        operation: ticket.operation.saturating_add(1),
        ..ticket
    };
    assert!(matches!(
        service.finish_apply_preflight(forged, ClosePreflight::Ready),
        ApplyDecision::Ignored
    ));
    assert_eq!(service.snapshot().phase, UpdaterPhase::Staged);
    assert_eq!(service.snapshot().reexec_count, 0);
}

#[test]
fn attention_acknowledges_one_revision_without_hiding_update() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    stage(&mut service, 11);
    let attention = service
        .snapshot()
        .attention_revision
        .expect("stage publishes attention");
    assert!(service.acknowledge_attention(attention));
    assert!(!service.snapshot().attention_pending());
    assert!(service.snapshot().staged.is_some());
    assert!(!service.acknowledge_attention(attention));
}

/// NOTHING READ, NOTHING CLAIMED (s-16): a fresh service has observed nothing
/// about whether this copy can replace itself, so it is neither installable nor
/// not; a check's read says which, and a platform with no updater knows at
/// construction.
#[test]
fn installability_is_unobserved_until_a_read_says() {
    let fresh = NativeUpdaterService::new(10, "1.0.10", true);
    assert_eq!(
        fresh.snapshot().installable,
        UpdateInstallability::Unobserved
    );
    assert_eq!(
        NativeUpdaterService::new(10, "1.0.10", false)
            .snapshot()
            .installable,
        UpdateInstallability::NotInstallable,
        "no updater on this platform is known without a read"
    );
    for (read, want) in [
        (Some(false), UpdateInstallability::NotInstallable),
        (Some(true), UpdateInstallability::Installable),
        (None, UpdateInstallability::Unobserved),
    ] {
        let mut service = NativeUpdaterService::new(10, "1.0.10", true);
        let ticket = started(&mut service);
        let mut checked = status(None);
        checked.installable = read;
        let _ = service.finish_check(ticket, checked);
        assert_eq!(service.snapshot().installable, want, "{read:?}");
    }
    let mut seeded = NativeUpdaterService::new(10, "1.0.10", true)
        .seed_installability(UpdateInstallability::NotInstallable);
    assert_eq!(
        seeded.snapshot().installable,
        UpdateInstallability::NotInstallable
    );
    assert!(seeded.note_installability(true));
    assert_eq!(
        seeded.snapshot().installable,
        UpdateInstallability::Installable
    );
    assert!(
        !seeded.note_installability(true),
        "an unchanged read republishes nothing"
    );
}

#[test]
fn durable_strings_are_bounded_on_utf8_boundaries() {
    let mut oversized = status(Some(11));
    oversized.changelog = Some("🚀".repeat(MAX_CHANGELOG_BYTES));
    let service = NativeUpdaterService::from_durable_status(10, "1.0.10", oversized);
    let notes = service
        .snapshot()
        .staged
        .as_ref()
        .and_then(|staged| staged.changelog.as_deref())
        .expect("staged notes");
    assert!(notes.len() <= MAX_CHANGELOG_BYTES);
    assert!(notes.is_char_boundary(notes.len()));
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the process-global native updater reducer.
//!
//! The trace is emitted by the genuine service while it accepts a worker result and an
//! apply preflight.  The test independently projects those scalar states into the
//! drift-free `NativeUpdater` model; it does not reconstruct an expected state from the
//! action definition.

#![cfg(test)]

use aterm_spec::derive::{
    Model, native_update_admission_model, native_update_apply_ladder_model,
    native_update_attempt_identity_model, native_update_auto_intent_model,
    native_update_disk_transaction_model, native_update_hidden_output_quiet_model,
    native_updater_model,
};
use aterm_spec::interp::{State, admits};

use crate::native_update_admission::{
    AdmissionBlock, AdmissionDecision, AdmissionFacts, ApplyLane, classify,
};
use crate::native_update_auto_intent::{
    ApplyPhase, ArmDecision, ArmFacts, AttemptDisposition, AttemptResult, PollDecision, PollFacts,
    WaitReason, arm, finish, poll,
};
use crate::native_updater_service::{
    ApplyDecision, ApplyMode, ApplyPreflightStart, CheckCompletion, CheckStart, ClosePreflight,
    DurableUpdateStatus, NativeUpdaterService, UpdaterModelState, UpdaterTransition,
};

fn status(staged_build: Option<u64>, failing_checks: u32) -> DurableUpdateStatus {
    DurableUpdateStatus {
        linux_host: false,
        linux: None,
        enabled: true,
        current_build: 10,
        staged_build,
        staged_version: staged_build.map(|build| format!("1.0.{build}")),
        staged_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        staged_dmg_sha256: Some(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        changelog: Some("# Release notes".to_string()),
        outcome: if failing_checks == 0 {
            "staged".to_string()
        } else {
            "network failed".to_string()
        },
        failing_checks,
        failing_persistent: false,
        failing_kind: String::new(),
        failing_applies: 0,
        apply_failure: String::new(),
        apply_failure_build: 0,
        apply_failures_for_target: 0,
        installable: true,
        channel_unreadable: false,
        checked_at: None,
    }
}

fn project(model: &Model, state: UpdaterModelState) -> State {
    let mut projected = model.init_state();
    projected.insert("phase", i64::from(state.phase));
    projected.insert(
        "request_generation",
        i64::try_from(state.request_generation).expect("bounded request generation"),
    );
    projected.insert(
        "work_generation",
        i64::try_from(state.work_generation).expect("bounded work generation"),
    );
    projected.insert(
        "artifact_generation",
        i64::try_from(state.artifact_generation).expect("bounded artifact generation"),
    );
    projected.insert("active_work", i64::from(state.active_work));
    projected.insert(
        "stale_completion_pending",
        i64::from(state.stale_completion_pending),
    );
    projected.insert("verified", i64::from(state.verified));
    projected.insert("close_preflight", i64::from(state.close_preflight));
    projected.insert(
        "install_on_clean_quit",
        i64::from(state.install_on_clean_quit),
    );
    projected.insert(
        "reexec_count",
        i64::try_from(state.reexec_count).expect("bounded reexec count"),
    );
    projected.insert("stale_staged", i64::from(state.stale_staged));
    projected
}

fn assert_transition(model: &Model, transition: UpdaterTransition) {
    let action = transition
        .action
        .model_action()
        .expect("conformance trace contains only modeled safety transitions");
    let before = project(model, transition.before);
    let after = project(model, transition.after);
    assert_eq!(
        model.successors(action, &before).as_slice(),
        std::slice::from_ref(&after),
        "real updater transition must conform specifically to {action}"
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

fn assert_last_batch(model: &Model, service: &NativeUpdaterService) {
    assert!(!service.last_transitions().is_empty());
    for transition in service.last_transitions() {
        assert_transition(model, *transition);
    }
}

#[test]
fn real_updater_service_conforms_for_single_flight_stage_defer_and_safe_apply() {
    let model = native_updater_model();
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    assert_eq!(project(&model, service.model_state()), model.init_state());

    let check = match service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected new updater work, got {other:?}"),
    };
    assert_last_batch(&model, &service);

    // Single-flight negative control: a second view joins the exact service ticket;
    // neither generation nor model state advances and no second worker is minted.
    let running = service.model_state();
    assert_eq!(service.request_check(), CheckStart::Joined(check));
    assert_eq!(service.model_state(), running);
    assert!(service.last_transitions().is_empty());

    // The shipping updater API performs check+download+verify+stage in one worker. The
    // genuine reducer exposes the three logical transitions as one atomic completion.
    assert_eq!(
        service.finish_check(check, status(Some(11), 0)),
        CheckCompletion::Reduced
    );
    assert_eq!(service.last_transitions().len(), 3);
    assert_last_batch(&model, &service);

    assert!(service.install_when_safe());
    assert_last_batch(&model, &service);

    let preflight = match service.begin_apply_preflight(ApplyMode::CleanQuit) {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("expected close preflight, got {other:?}"),
    };
    assert!(service.last_transitions().is_empty());

    let command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
        ApplyDecision::Execute(command) => command,
        other => panic!("expected one reexec decision, got {other:?}"),
    };
    assert_eq!(service.last_transitions().len(), 2);
    assert_last_batch(&model, &service);

    let mut process_reexec_calls = 0;
    command.execute(|| process_reexec_calls += 1);
    assert_eq!(process_reexec_calls, 1);

    // A replay cannot produce a second command, and the model catches the independent
    // double-reexec mutant rather than trusting the service's own decision bit.
    assert!(matches!(
        service.finish_apply_preflight(preflight, ClosePreflight::Ready),
        ApplyDecision::Ignored
    ));
    let applying = project(&model, service.model_state());
    let mut double_apply = applying.clone();
    double_apply.insert("reexec_count", 2);
    assert!(!model.check_invariant("OneLiveApplyAuthority", &double_apply));
    assert_eq!(admits(&model, &applying, &double_apply), None);
}

#[test]
fn real_service_retries_and_drops_a_stale_stage_completion() {
    let model = native_updater_model();
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    let first = match service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected first check, got {other:?}"),
    };
    assert_last_batch(&model, &service);
    assert_eq!(
        service.finish_check(first, status(None, 1)),
        CheckCompletion::Reduced
    );
    assert_last_batch(&model, &service);

    let second = match service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected retry check, got {other:?}"),
    };
    assert_last_batch(&model, &service);

    let before_stale = service.model_state();
    assert!(matches!(
        service.finish_check(first, status(Some(99), 0)),
        CheckCompletion::Ignored(_)
    ));
    assert_eq!(service.model_state(), before_stale);
    assert!(service.last_transitions().is_empty());

    assert_eq!(
        service.finish_check(second, status(Some(12), 0)),
        CheckCompletion::Reduced
    );
    assert_last_batch(&model, &service);

    // Independent stale-stage negative control: an old artifact generation marked staged
    // violates the named invariant, proving the conformance assertion is non-vacuous.
    let current = project(&model, service.model_state());
    let mut stale = current.clone();
    stale.insert("artifact_generation", current["request_generation"] - 1);
    stale.insert("stale_staged", 1);
    assert!(!model.check_invariant("CurrentStagedArtifact", &stale));
}

#[test]
fn blocked_close_preflight_never_advances_to_apply() {
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    let check = match service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected check, got {other:?}"),
    };
    assert_eq!(
        service.finish_check(check, status(Some(11), 0)),
        CheckCompletion::Reduced
    );
    let before = service.model_state();
    let preflight = match service.begin_apply_preflight(ApplyMode::Immediate) {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("expected preflight, got {other:?}"),
    };
    assert!(matches!(
        service.finish_apply_preflight(
            preflight,
            ClosePreflight::Blocked(vec!["dirty editor revision".to_string()])
        ),
        ApplyDecision::Blocked(_)
    ));
    assert_eq!(service.model_state(), before);
    assert_eq!(service.snapshot().reexec_count, 0);
}

#[test]
fn failed_apply_rearms_exact_stage_and_stale_attempt_cannot_abort_retry() {
    let model = native_updater_model();
    let identity_model = native_update_attempt_identity_model();
    let mut identity = identity_model.init_state();
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    let check = match service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected check, got {other:?}"),
    };
    assert_last_batch(&model, &service);
    assert_eq!(
        service.finish_check(check, status(Some(11), 0)),
        CheckCompletion::Reduced
    );
    assert_last_batch(&model, &service);

    let preflight = match service.begin_apply_preflight(ApplyMode::Immediate) {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("expected preflight, got {other:?}"),
    };
    let first_command = match service.finish_apply_preflight(preflight, ClosePreflight::Ready) {
        ApplyDecision::Execute(command) => command,
        other => panic!("expected apply command, got {other:?}"),
    };
    assert_last_batch(&model, &service);
    let first_identity = identity_model.successors("StartAttempt", &identity)[0].clone();
    assert_exact_model_action(&identity_model, "StartAttempt", &identity, &first_identity);
    identity = first_identity;
    let first_attempt = first_command.attempt();
    assert!(service.abort_apply(&first_attempt, "child readiness failed"));
    assert_last_batch(&model, &service);
    let retryable_identity = identity_model.successors("AbortCurrent", &identity)[0].clone();
    assert_exact_model_action(
        &identity_model,
        "AbortCurrent",
        &identity,
        &retryable_identity,
    );
    identity = retryable_identity;
    assert_eq!(service.model_state().phase, 4);
    assert!(service.model_state().verified);
    assert_eq!(service.model_state().reexec_count, 0);

    let retry = match service.begin_apply_preflight(ApplyMode::Immediate) {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("aborted stage must remain retryable, got {other:?}"),
    };
    let retry_command = match service.finish_apply_preflight(retry, ClosePreflight::Ready) {
        ApplyDecision::Execute(command) => command,
        other => panic!("expected retry command, got {other:?}"),
    };
    assert_last_batch(&model, &service);
    let retry_identity = identity_model.successors("StartAttempt", &identity)[0].clone();
    assert_exact_model_action(&identity_model, "StartAttempt", &identity, &retry_identity);
    identity = retry_identity;
    let retry_attempt = retry_command.attempt();
    assert_ne!(retry_attempt, first_attempt);
    assert!(identity_model.check_invariant("RetryUsesFreshIdentity", &identity));

    // Negative control for the attempt-nonce regression: a delayed failure from
    // attempt A cannot cancel the live authority for attempt B.
    assert!(!service.abort_apply(&first_attempt, "stale attempt callback"));
    assert!(service.last_transitions().is_empty());
    assert_eq!(service.model_state().phase, 5);
    assert_eq!(service.model_state().reexec_count, 1);
    let applying = project(&model, service.model_state());
    assert!(model.check_invariant("OneLiveApplyAuthority", &applying));
    let replay_rejected = identity_model.successors("ReplayOldAbort", &identity)[0].clone();
    assert_exact_model_action(
        &identity_model,
        "ReplayOldAbort",
        &identity,
        &replay_rejected,
    );
    assert_eq!(replay_rejected, identity);

    assert!(service.abort_apply(&retry_attempt, "second child readiness failed"));
    assert_last_batch(&model, &service);
    let final_identity = identity_model.successors("AbortCurrent", &identity)[0].clone();
    assert_exact_model_action(&identity_model, "AbortCurrent", &identity, &final_identity);
    assert_eq!(service.model_state().phase, 4);
    assert_eq!(service.model_state().reexec_count, 0);
}

#[test]
fn healthy_no_update_completion_returns_to_idle_through_the_derived_action() {
    let model = native_updater_model();
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);
    let check = match service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected check, got {other:?}"),
    };
    assert_last_batch(&model, &service);
    assert_eq!(
        service.finish_check(check, status(None, 0)),
        CheckCompletion::Reduced
    );
    assert_last_batch(&model, &service);
    assert_eq!(service.model_state().phase, 0);
    assert!(!service.model_state().active_work);
}

/// Bind the real event-loop dispatch predicate to the disk transaction's
/// first-present boundary. Allocating a window is the historical negative
/// control: it must not enable proof/disarm until a successful drawable present.
#[test]
fn real_gui_boot_health_dispatch_requires_first_present_before_model_disarm() {
    let model = native_update_disk_transaction_model();
    let mut state = model.init_state();
    for action in [
        "ConsumeStartupAuthority",
        "ObserveBootHealth",
        "EnterDiskLane",
        "PrepareFixedNew",
        "ArmExactTrial",
        "AtomicSwap",
        "RecordExactReceipt",
        "VerifyExactRollback",
    ] {
        let next = model.successors(action, &state)[0].clone();
        assert_exact_model_action(&model, action, &state, &next);
        state = next;
    }

    assert!(state["trial"] == 1 && state["first_present_done"] == 0);
    assert!(
        !crate::should_dispatch_boot_health_confirmation(
            false, false, false, false, true, true, false
        ),
        "negative control: a live OS window is not evidence that any content presented"
    );
    assert!(
        model.successors("ProveInstalledHealth", &state).is_empty(),
        "the healthy model must agree with the shipping pre-present guard"
    );
    assert!(
        model.successors("DisarmTrial", &state).is_empty(),
        "pre-present trial authority must remain armed"
    );

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let pre_present_disarm = buggy.successors("DisarmBeforeHealthProof", &state)[0].clone();
    assert!(
        !buggy.check_invariant("HealthDisarmRequiresFirstPresent", &pre_present_disarm),
        "negative control must be catchable independently of the production guard"
    );

    let presented = model.successors("PresentInstalledUi", &state)[0].clone();
    assert_exact_model_action(&model, "PresentInstalledUi", &state, &presented);
    assert!(crate::should_dispatch_boot_health_confirmation(
        false, false, true, false, true, true, false,
    ));
    assert!(
        !crate::should_dispatch_boot_health_confirmation(
            false, false, true, false, true, true, true
        ),
        "an uncommitted handoff candidate proves no health: its parent may still reject it \
         (2026-09-19), so the present it made is not this build's healthy launch"
    );
    assert!(
        !crate::should_dispatch_boot_health_confirmation(
            false, false, true, true, true, true, false
        ),
        "the same present cannot enqueue a second confirmation"
    );
    assert!(
        !crate::should_dispatch_boot_health_confirmation(
            true, false, true, false, true, false, false
        ),
        "headless proves health at the control-socket boundary, not on glass"
    );
    assert!(
        crate::should_dispatch_boot_health_confirmation(
            true, true, false, false, true, false, false
        ),
        "a bound control socket (or none configured) is the headless health proof"
    );
    let proved = model.successors("ProveInstalledHealth", &presented)[0].clone();
    assert_exact_model_action(&model, "ProveInstalledHealth", &presented, &proved);
}

fn admission_before(model: &Model, facts: AdmissionFacts) -> State {
    let mut state = model.init_state();
    if !facts.staged_verified {
        state = model.successors("InvalidateArtifact", &state)[0].clone();
    }
    if facts.live_ptys > 0 || facts.foreground_jobs > 0 {
        state = model.successors("ObserveForegroundJob", &state)[0].clone();
    }
    // `unknown_foregrounds` used to be folded in here, which encoded the old
    // policy: an unprobeable foreground made the whole native state "unsafe" and
    // blocked every lane, seamless included. It is not a native-state fact — it is
    // a fact about whether a DESTRUCTIVE replacement would hang up a running job,
    // which only the cold lane can do. It now enters the model through
    // `ObserveForegroundJob` (already fired whenever `live_ptys > 0`, and the
    // matrix below never generates `unknown_foregrounds > live_ptys`).
    if !facts.native_state_certified {
        state = model.successors("ObserveUnsafeNativeState", &state)[0].clone();
    }
    if !facts.seamless_capable {
        state = model.successors("LoseSeamlessLane", &state)[0].clone();
    }
    state
}

fn project_admission_decision(before: &State, decision: AdmissionDecision) -> State {
    let mut after = before.clone();
    match decision {
        AdmissionDecision::Apply(lane) => {
            after.insert("phase", 1);
            after.insert(
                "decision",
                match lane {
                    ApplyLane::Seamless => 1,
                    ApplyLane::Cold => 2,
                },
            );
            after.insert("attempt_count", (before["attempt_count"] + 1).min(2));
            after.insert("retry_eligible", 0);
        }
        AdmissionDecision::Block(block) => {
            after.insert("phase", 3);
            after.insert("decision", 3);
            after.insert(
                "retry_eligible",
                i64::from(block != AdmissionBlock::UnverifiedStage),
            );
        }
    }
    after
}

fn admission_action(decision: AdmissionDecision) -> &'static str {
    match decision {
        AdmissionDecision::Apply(ApplyLane::Seamless) => "ClassifySeamless",
        AdmissionDecision::Apply(ApplyLane::Cold) => "ClassifyCold",
        AdmissionDecision::Block(AdmissionBlock::UnverifiedStage) => "BlockUnverifiedArtifact",
        AdmissionDecision::Block(AdmissionBlock::NativeStateUncertified) => {
            "BlockUnsafeNativeState"
        }
        // An unprobeable foreground now blocks for the same REASON as
        // `LivePtysNeedSeamless` — there is something on a PTY that a destructive
        // swap could hang up, and the lossless lane is unavailable — so it refines
        // the same model action. It is no longer a native-state certification fact.
        AdmissionDecision::Block(AdmissionBlock::ForegroundProbeUnknown)
        | AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless) => {
            "BlockForegroundWithoutSeamless"
        }
    }
}

/// Exhaust the bounded admission fact matrix against the genuine shipping
/// classifier. This binds the model's foreground-job progress and cold-fallback
/// safety predicates to compiled code rather than duplicating the policy in a
/// test-only oracle.
#[test]
fn real_update_admission_classifier_conforms_for_every_bounded_fact_combination() {
    let model = native_update_admission_model();
    for staged_verified in [false, true] {
        for seamless_capable in [false, true] {
            for live_ptys in [0, 1] {
                for foreground_jobs in [0, 1] {
                    for unknown_foregrounds in [0, 1] {
                        for native_state_certified in [false, true] {
                            if foreground_jobs > live_ptys || unknown_foregrounds > live_ptys {
                                continue;
                            }
                            let facts = AdmissionFacts {
                                staged_verified,
                                seamless_capable,
                                native_state_certified,
                                live_ptys,
                                foreground_jobs,
                                unknown_foregrounds,
                            };
                            let before = admission_before(&model, facts);
                            let decision = classify(facts);
                            let after = project_admission_decision(&before, decision);
                            let action = admission_action(decision);
                            assert_eq!(
                                model.successors(action, &before).as_slice(),
                                std::slice::from_ref(&after),
                                "shipping admission decision {decision:?} for {facts:?} must refine {action}"
                            );
                            assert_eq!(admits(&model, &before, &after), Some(action));
                            for invariant in &model.invariants {
                                assert!(
                                    model.check_invariant(invariant.name, &after),
                                    "shipping decision violates {} for {facts:?}: {after:?}",
                                    invariant.name
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // Regression-shaped negative control: blocking a safe seamless handoff only
    // because a foreground job exists violates the named progress invariant.
    let facts = AdmissionFacts {
        staged_verified: true,
        seamless_capable: true,
        native_state_certified: true,
        live_ptys: 1,
        foreground_jobs: 1,
        unknown_foregrounds: 0,
    };
    assert_eq!(
        classify(facts),
        AdmissionDecision::Apply(ApplyLane::Seamless)
    );
    let before = admission_before(&model, facts);
    let mut regressed = before.clone();
    regressed.insert("phase", 3);
    regressed.insert("decision", 3);
    regressed.insert("retry_eligible", 1);
    assert!(!model.check_invariant("ForegroundJobsDoNotBlockSeamless", &regressed));
    assert_eq!(admits(&model, &before, &regressed), None);
}

/// The admitted seamless lane carries a live foreground job into the replacement,
/// while the cold lane is unreachable for that same runtime fact projection.
#[test]
fn real_seamless_admission_projects_to_job_preserving_replacement() {
    let model = native_update_admission_model();
    let facts = AdmissionFacts {
        staged_verified: true,
        seamless_capable: true,
        native_state_certified: true,
        live_ptys: 3,
        foreground_jobs: 3,
        unknown_foregrounds: 0,
    };
    let before = admission_before(&model, facts);
    let authorized = project_admission_decision(&before, classify(facts));
    let replaced = model.successors("CompleteSeamlessHandoff", &authorized)[0].clone();
    assert_eq!(replaced["adopted_foreground"], 1);
    assert!(model.check_invariant("ReplacementPreservesForeground", &replaced));
    assert!(
        model
            .successors("CompleteColdFallback", &authorized)
            .is_empty()
    );
}

fn assert_exact_model_action(model: &Model, action: &'static str, before: &State, after: &State) {
    assert_eq!(
        model.successors(action, before).as_slice(),
        std::slice::from_ref(after),
        "shipping reducer projection must conform specifically to {action}"
    );
    assert_eq!(admits(model, before, after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "post-state violates {}::{}: {after:?}",
            model.name,
            invariant.name
        );
    }
}

/// Bind the event-order regression to the shipping pure auto-intent reducer and
/// the genuine updater service. A stage wake arms while a manual worker is active;
/// that worker's completion stages the artifact, after which the retained intent
/// becomes an attempt. A cheap state block retries; a physical handoff failure
/// becomes sticky manual-only so timers cannot repeat process work.
#[test]
fn real_auto_intent_survives_manual_check_collision_and_unsuccessful_attempts() {
    let model = native_update_auto_intent_model();
    let mut modeled = model.init_state();
    let mut service = NativeUpdaterService::new(10, "1.0.10", true);

    let check = match service.request_check() {
        CheckStart::Start(ticket) => ticket,
        other => panic!("expected active manual check, got {other:?}"),
    };
    let checking = model.successors("StartManualCheck", &modeled)[0].clone();
    assert_exact_model_action(&model, "StartManualCheck", &modeled, &checking);
    modeled = checking;

    let armed = arm(ArmFacts {
        enabled: true,
        current_build: service.snapshot().current_build,
        armed_build: None,
        armed_exact: false,
        manual_only_exact: false,
        manual_only_build: None,
        incoming_build: 11,
    });
    assert_eq!(armed, ArmDecision::Set(11));
    let stage_wake = model.successors("StageWakeDuringCheck", &modeled)[0].clone();
    assert_exact_model_action(&model, "StageWakeDuringCheck", &modeled, &stage_wake);
    modeled = stage_wake;

    // The compiled poll policy waits without consuming the armed target while
    // the real updater service still owns the manual-check ticket.
    assert_eq!(
        poll(PollFacts {
            enabled: true,
            deadline_ready: true,
            current_build: service.snapshot().current_build,
            target_build: 11,
            work_active: service.snapshot().active.is_some(),
            applying: false,
            activity_quiet: true,
            phase: ApplyPhase::PreferIdle,
            staged_ready: false,
            staged_build: None,
            staged_exact_target: false,
        }),
        PollDecision::Wait(WaitReason::WorkActive)
    );
    assert!(model.check_invariant("StageDuringCheckRetainsIntent", &modeled));

    assert_eq!(
        service.finish_check(check, status(Some(11), 0)),
        CheckCompletion::Reduced
    );
    assert_eq!(
        service.snapshot().phase,
        crate::native_updater_service::UpdaterPhase::Staged
    );
    let imported = model.successors("ManualCheckCompletesAndImportsStage", &modeled)[0].clone();
    assert_exact_model_action(
        &model,
        "ManualCheckCompletesAndImportsStage",
        &modeled,
        &imported,
    );
    modeled = imported;

    let staged_build = service
        .snapshot()
        .staged
        .as_ref()
        .map(|staged| staged.build);
    assert_eq!(
        poll(PollFacts {
            enabled: true,
            deadline_ready: true,
            current_build: service.snapshot().current_build,
            target_build: 11,
            work_active: service.snapshot().active.is_some(),
            applying: false,
            activity_quiet: true,
            phase: ApplyPhase::PreferIdle,
            staged_ready: true,
            staged_build,
            staged_exact_target: true,
        }),
        PollDecision::Attempt {
            build: 11,
            quiet: true,
            phase: ApplyPhase::PreferIdle,
        }
    );
    let quiet = model.successors("QuietElapsed", &modeled)[0].clone();
    assert_exact_model_action(&model, "QuietElapsed", &modeled, &quiet);
    modeled = quiet;
    let attempting = model.successors("Attempt", &modeled)[0].clone();
    assert_exact_model_action(&model, "Attempt", &modeled, &attempting);
    modeled = attempting;

    // THE PARK IS ITS OWN STEP between the attempt and its acceptance
    // (2026-09-19, the late park): the attempt starts by launching the
    // successor with every reader live, and only a parked attempt can be
    // accepted — `AcceptedRequiresParkedReaders`.
    let parked = model.successors("ParkReaders", &modeled)[0].clone();
    assert_exact_model_action(&model, "ParkReaders", &modeled, &parked);
    assert!(
        model.successors("AttemptAccepted", &modeled).is_empty(),
        "an unparked attempt cannot be accepted"
    );
    let accepted = model.successors("AttemptAccepted", &parked)[0].clone();
    assert_eq!(
        finish(AttemptResult::Accepted),
        AttemptDisposition::Complete
    );
    assert_exact_model_action(&model, "AttemptAccepted", &parked, &accepted);
    assert_eq!(accepted["accepted"], 1);
    assert!(model.check_invariant("AcceptedRequiresParkedReaders", &accepted));

    assert_eq!(finish(AttemptResult::Blocked), AttemptDisposition::Retry);
    let retryable = model.successors("AttemptDidNotReplace", &modeled)[0].clone();
    assert_exact_model_action(&model, "AttemptDidNotReplace", &modeled, &retryable);
    assert!(model.check_invariant("UnsuccessfulAttemptRetainsIntent", &retryable));
    modeled = model.successors("Attempt", &retryable)[0].clone();
    assert_exact_model_action(&model, "Attempt", &retryable, &modeled);

    assert_eq!(
        finish(AttemptResult::Failed),
        AttemptDisposition::ManualOnly
    );
    let manual_only = model.successors("AttemptPhysicalFailure", &modeled)[0].clone();
    assert_exact_model_action(&model, "AttemptPhysicalFailure", &modeled, &manual_only);
    assert!(model.check_invariant("PhysicalFailureIsManualOnly", &manual_only));
}

/// Bind the model's `GraceWindowCloses` transition to the genuine poll policy.
///
/// THE REGRESSION: `activity_quiet` samples a MACHINE-WIDE input clock plus every
/// live PTY's latest output. On a daily driver those are basically never
/// simultaneously idle, so the old "activity always defers" rule meant a
/// verified staged build waited for a moment that never arrived — and the user
/// ended up clicking Install by hand, which is the exact outcome automatic apply
/// exists to remove. Deferral is bounded by the ladder: in its first phase
/// activity still wins, in every later phase the same still-busy facts attempt,
/// and the phase rides along so the park gate reads the ladder.
#[test]
fn real_auto_intent_bounds_activity_deferral_instead_of_waiting_forever() {
    let model = native_update_auto_intent_model();
    let mut modeled = model.init_state();
    for action in ["StageWakeIdle", "Activity"] {
        let after = model.successors(action, &modeled)[0].clone();
        assert_exact_model_action(&model, action, &modeled, &after);
        modeled = after;
    }
    assert_eq!(modeled["quiet"], 0, "the machine is busy");

    let busy = PollFacts {
        enabled: true,
        deadline_ready: true,
        current_build: 10,
        target_build: 11,
        work_active: false,
        applying: false,
        activity_quiet: false,
        phase: ApplyPhase::PreferIdle,
        staged_ready: true,
        staged_build: Some(11),
        staged_exact_target: true,
    };
    // Inside the window the real policy waits, exactly as the model's `Activity`
    // step leaves it: intent retained, nothing consumed.
    assert_eq!(poll(busy), PollDecision::Wait(WaitReason::Activity));
    assert!(model.check_invariant("UnsuccessfulAttemptRetainsIntent", &modeled));

    let closed = model.successors("GraceWindowCloses", &modeled)[0].clone();
    assert_exact_model_action(&model, "GraceWindowCloses", &modeled, &closed);
    assert_eq!(closed["grace_expired"], 1);

    // Past it, the same still-busy facts attempt — in every later phase.
    for phase in [
        ApplyPhase::PreferOutputGap,
        ApplyPhase::KeysOnly,
        ApplyPhase::Land,
    ] {
        assert_eq!(
            poll(PollFacts { phase, ..busy }),
            PollDecision::Attempt {
                build: 11,
                quiet: false,
                phase,
            }
        );
    }
    let attempted = model.successors("Attempt", &closed)[0].clone();
    assert_exact_model_action(&model, "Attempt", &closed, &attempted);
    assert_eq!(
        attempted["parked"], 0,
        "the attempt STARTS by launching the successor; nothing is parked yet"
    );

    // THE PARK IS ITS OWN GATED STEP (2026-09-19, the late park). The launch
    // above cost the user nothing — every reader stayed live through the
    // successor's swap and boot — and this is the step they feel.
    let parked = model.successors("ParkReaders", &attempted)[0].clone();
    assert_exact_model_action(&model, "ParkReaders", &attempted, &parked);
    assert_eq!(parked["parked"], 1);
    assert!(model.check_invariant("AutomaticAttemptRequiresQuietOrClosedGraceWindow", &parked));

    // Negative control: parking with neither a quiet machine nor a closed window
    // is the unsafe shape the invariant exists to reject.
    let mut unbounded = parked.clone();
    unbounded.insert("grace_expired", 0);
    assert!(!model.check_invariant(
        "AutomaticAttemptRequiresQuietOrClosedGraceWindow",
        &unbounded
    ));
}

/// THE REAL PARK GATE, bound to the model step it implements (2026-09-19).
///
/// `Attempt` is the LAUNCH and is gated by the poll policy (the test above);
/// `ParkReaders` is the freeze and is gated by `prelaunch_park_admitted`. Bind
/// the shipping predicate to the model's guard over every combination of the
/// facts the model can express — the model's `quiet` is the ladder's first
/// phase asking for a quiet moment, its `grace_expired` is every later phase —
/// so a rule that drifts out of one of them fails here rather than on a user's
/// terminal.
///
/// THE MASTERS ARE ENUMERATED TOO (the 2026-09-22/23 update audit, plan P1-3):
/// this used to pin `masters_quiet: true`, so the one gate that held every
/// automatic attempt behind a `--hold` pane or a flooding job was invisible to
/// the binding. `masters_quiet` now comes from the REAL peek over real PTY
/// masters — a quiet live session, a live one with unread output, and an
/// EXITED one whose slave hung up with bytes still queued — and the shipping
/// gate must park exactly where the model does AND the masters allow: a live
/// session's unread output holds it (until the `Land` bound has waited
/// `PRELAUNCH_LAND_MAX_WAITS` times); an exited pane is never counted as
/// output, and holds it only as what it is — a dead session — until its pane
/// closes.
#[cfg(unix)]
#[test]
fn real_park_gate_admits_exactly_the_model_s_reader_park() {
    use crate::app_update_handoff::{
        PRELAUNCH_LAND_MAX_WAITS, ParkGate, ParkGateFacts, handoff_masters_closed,
        handoff_masters_have_activity, prelaunch_hold_cap, prelaunch_park_admitted,
    };
    use crate::native_update_auto_intent::ActivityFacts;
    let model = native_update_auto_intent_model();
    // Walk to the Attempting phase (launched, nothing parked) the way the
    // reducer does: a stage lands while idle, the quiet epoch elapses, attempt.
    let mut state = model.init_state();
    for action in ["StageWakeIdle", "QuietElapsed", "Attempt"] {
        let next = model.successors(action, &state)[0].clone();
        assert_exact_model_action(&model, action, &state, &next);
        state = next;
    }
    assert_eq!(state["phase"], 3);
    assert_eq!(state["parked"], 0);

    // The model's two facts, over both automatic modes and both ways of being
    // admissible. `held_for` is zero throughout: the cap is the OTHER gate and
    // has its own enumeration in `app_update_handoff::park_gate_tests`. A calm
    // machine otherwise, so the ladder's finer preferences (keys, output) do
    // not enter: those are enumerated in the same module.
    let calm = |quiet: bool| ActivityFacts {
        quiet,
        hands_off_keys: true,
        output_quiet: true,
        focused: true,
        consent_warmup: false,
    };
    // Real masters. An EXITED pane: its command wrote, then its slave closed —
    // the state a `--hold` pane sits in for as long as it stays open.
    let openpty = || {
        let (mut master, mut slave) = (-1i32, -1i32);
        // SAFETY: openpty(3) into two valid out-slots; no termios/winsize.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty");
        (master, slave)
    };
    let write_byte = |fd: i32| {
        // SAFETY: bounded write of a stack byte to a test-owned slave.
        assert_eq!(unsafe { libc::write(fd, [0x62u8].as_ptr().cast(), 1) }, 1);
    };
    let (quiet_master, quiet_slave) = openpty();
    let (busy_master, busy_slave) = openpty();
    write_byte(busy_slave);
    let (exited_master, exited_slave) = openpty();
    write_byte(exited_slave);
    aterm_pty::close_fd(exited_slave);
    let exited = vec![(3u64, exited_master, 4003i32)];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !handoff_masters_closed(&exited) {
        assert!(
            std::time::Instant::now() < deadline,
            "the exited slave hung up"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let desks = [
        (
            "a quiet live session",
            vec![(1u64, quiet_master, 4001i32)],
            true,
        ),
        (
            "unread live output",
            vec![(2u64, busy_master, 4002i32)],
            false,
        ),
        ("an exited --hold pane", exited.clone(), true),
        (
            "an exited pane beside a quiet live one",
            vec![
                (1u64, quiet_master, 4001i32),
                (3u64, exited_master, 4003i32),
            ],
            true,
        ),
    ];

    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        for quiet in [false, true] {
            for grace_expired in [false, true] {
                let mut modeled = state.clone();
                modeled.insert("quiet", i64::from(quiet));
                modeled.insert("grace_expired", i64::from(grace_expired));
                let model_parks = !model.successors("ParkReaders", &modeled).is_empty();
                assert_eq!(
                    model_parks,
                    quiet || grace_expired,
                    "the model's guard is quiet-or-past-grace"
                );
                // The shipping predicate, given the same two facts: the
                // model's `grace_expired` is any phase after the first.
                let phases: &[ApplyPhase] = if grace_expired {
                    &[
                        ApplyPhase::PreferOutputGap,
                        ApplyPhase::KeysOnly,
                        ApplyPhase::Land,
                    ]
                } else {
                    &[ApplyPhase::PreferIdle]
                };
                for &phase in phases {
                    for (desk, live, quiet_masters) in &desks {
                        let masters_quiet = !handoff_masters_have_activity(live);
                        let masters_alive = !handoff_masters_closed(live);
                        assert_eq!(masters_quiet, *quiet_masters, "the real peek over {desk}");
                        for land_waits in [0, PRELAUNCH_LAND_MAX_WAITS] {
                            let real = prelaunch_park_admitted(
                                ParkGateFacts {
                                    mode,
                                    phase,
                                    activity: calm(quiet),
                                    masters_quiet,
                                    masters_alive,
                                    land_waits,
                                    held_for: std::time::Duration::ZERO,
                                },
                                prelaunch_hold_cap(mode),
                            ) == ParkGate::Park;
                            let bound_waited =
                                phase == ApplyPhase::Land && land_waits >= PRELAUNCH_LAND_MAX_WAITS;
                            assert_eq!(
                                real,
                                model_parks && masters_alive && (masters_quiet || bound_waited),
                                "{mode:?} quiet={quiet} {phase:?} {desk} after {land_waits} \
                                 waits: the shipping gate parks exactly where the model \
                                 does and the masters allow"
                            );
                        }
                    }
                }
            }
        }
    }
    for fd in [
        quiet_slave,
        busy_slave,
        quiet_master,
        busy_master,
        exited_master,
    ] {
        aterm_pty::close_fd(fd);
    }

    // NEGATIVE CONTROL: the mutant parks with neither fact, and the invariant
    // catches it — which is what makes the guard above a claim about the code
    // rather than a restatement of it.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut busy = state.clone();
    busy.insert("quiet", 0);
    busy.insert("grace_expired", 0);
    assert!(
        model.successors("ParkReaders", &busy).is_empty(),
        "the healthy model never parks a busy terminal"
    );
    let parked = buggy.successors("ParkReaders", &busy)[0].clone();
    assert!(!buggy.check_invariant("AutomaticAttemptRequiresQuietOrClosedGraceWindow", &parked));
    // And the shipping gate refuses the same state on both automatic lanes.
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        assert!(matches!(
            prelaunch_park_admitted(
                ParkGateFacts {
                    mode,
                    phase: ApplyPhase::PreferIdle,
                    activity: calm(false),
                    masters_quiet: true,
                    masters_alive: true,
                    land_waits: 0,
                    held_for: std::time::Duration::ZERO,
                },
                prelaunch_hold_cap(mode),
            ),
            ParkGate::Wait(_)
        ));
    }
}

/// THE LADDER, bound to its model (docs/DESIGN-auto-apply-ladder-2026-09-21.md).
///
/// The shipping `apply_phase` tiles the wall clock into the model's four
/// phases, and the shipping `automatic_park_refusal` — the ONE predicate the
/// entry gate and the park gate both read — must admit exactly the states in
/// which the model's `Park` is enabled, over every phase and every combination
/// of the five facts (the four about the terminal, and the user's consent
/// warm-up, which no phase relaxes). The mutant's stand-down is the 2026-09-20
/// incident, caught here as a wedge: the busy terminal that never updates.
#[test]
fn real_apply_ladder_admits_exactly_the_model_s_park() {
    use crate::native_update_auto_intent::{
        ActivityFacts, LANDS_WITHIN, PREFER_IDLE_WINDOW, PREFER_OUTPUT_GAP_WINDOW, apply_phase,
        automatic_park_refusal,
    };
    let model = native_update_apply_ladder_model();

    // The wall clock tiles into the model's phases, in order, ending at the bound.
    let phases = [
        (ApplyPhase::PreferIdle, 0, std::time::Duration::ZERO),
        (ApplyPhase::PreferOutputGap, 1, PREFER_IDLE_WINDOW),
        (
            ApplyPhase::KeysOnly,
            2,
            PREFER_IDLE_WINDOW + PREFER_OUTPUT_GAP_WINDOW,
        ),
        (ApplyPhase::Land, 3, LANDS_WITHIN),
    ];
    for (phase, modeled, since_armed) in phases {
        assert_eq!(apply_phase(since_armed), phase);
        assert_eq!(
            apply_phase(since_armed + std::time::Duration::from_secs(1)),
            phase
        );
        for bits in 0..32u32 {
            let facts = ActivityFacts {
                quiet: bits & 1 != 0,
                hands_off_keys: bits & 2 != 0,
                output_quiet: bits & 4 != 0,
                focused: bits & 8 != 0,
                consent_warmup: bits & 16 != 0,
            };
            let mut state = model.init_state();
            state.insert("phase", modeled);
            state.insert("quiet", i64::from(facts.quiet));
            state.insert("keys", i64::from(facts.hands_off_keys));
            state.insert("output", i64::from(facts.output_quiet));
            state.insert("focused", i64::from(facts.focused));
            state.insert("warmup", i64::from(facts.consent_warmup));
            let model_parks = !model.successors("Park", &state).is_empty();
            let real_parks = automatic_park_refusal(phase, facts).is_none();
            assert_eq!(
                real_parks, model_parks,
                "{phase:?} {facts:?}: the shipping predicate and the model's guard agree"
            );
            if model_parks {
                let parked = model.successors("Park", &state)[0].clone();
                assert!(model.check_invariant("ParkedOnlyWhenTheLadderAdmits", &parked));
            }
        }
    }

    // THE WARM-UP AT THE BOUND. Past `LANDS_WITHIN` the shipping predicate and
    // the model both still refuse a park over the user's warm-up, and the
    // model's warm-up ending is what lets the landing through.
    let mut at_bound = model.init_state();
    for _ in 0..3 {
        at_bound = model.successors("Advance", &at_bound)[0].clone();
    }
    assert_eq!(at_bound["phase"], 3);
    let warming = model.successors("WarmupStarts", &at_bound)[0].clone();
    assert_exact_model_action(&model, "WarmupStarts", &at_bound, &warming);
    assert!(model.successors("Park", &warming).is_empty());
    let busy_warmup = ActivityFacts {
        quiet: false,
        hands_off_keys: false,
        output_quiet: false,
        focused: true,
        consent_warmup: true,
    };
    assert!(automatic_park_refusal(apply_phase(LANDS_WITHIN), busy_warmup).is_some());
    let ended = model.successors("WarmupEnds", &warming)[0].clone();
    assert_eq!(model.successors("Park", &ended)[0]["landed"], 1);
    assert_eq!(
        automatic_park_refusal(
            apply_phase(LANDS_WITHIN),
            ActivityFacts {
                consent_warmup: false,
                ..busy_warmup
            }
        ),
        None
    );
    // NEGATIVE CONTROL: the mutant's ruleless park lands over the warm-up and
    // the invariant catches it.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let over_dialog = buggy.successors("ParkWithoutTheRule", &warming)[0].clone();
    assert!(!buggy.check_invariant("ParkedOnlyWhenTheLadderAdmits", &over_dialog));

    // The healthy ladder always lands; the mutant wedges a busy terminal.
    let landed = |state: &State| state["landed"] == 1;
    assert!(aterm_spec::interp::find_deadlock(&model, landed).is_none());
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let wedge = aterm_spec::interp::find_deadlock(&buggy, landed)
        .expect("the incident: stood down, never landing");
    assert_eq!(wedge["manual_only"], 1);
    assert!(!buggy.check_invariant("ActivityNeverLatchesManualOnly", &wedge));
}

/// THE 2026-09-21 AUDIT, bound to the model steps that describe it.
///
/// `Lapse`: a physical-failure latch lapsing resumes the ladder where the clock
/// is. The shipping `lapse_expired_auto_apply_manual_only` releases the latch
/// and leaves `App::auto_apply_ladder` alone, so `automatic_apply_phase` after
/// the lapse is the model's post-`Lapse` phase — `Land`, when the latch held
/// past the bound — and never `PreferIdle` again.
///
/// `ParkMissed`: a park that missed every freeze rung stands its successor
/// down as ACTIVITY. The shipping `park_miss_disposition` answers
/// `ActivityRevoked`, and the shipping `HandoffFailureLane::classify` keeps
/// that outcome out of every physical shape — so the model's `manual_only`
/// stays 0, exactly as the real lane's latch does.
#[cfg(unix)]
#[test]
fn real_lapse_keeps_the_anchor_and_a_park_miss_stays_in_the_activity_lane() {
    use crate::app_native::HandoffFailureLane;
    use crate::app_update_handoff::{
        PRELAUNCH_MAX_PARK_MISSES, ParkMissDisposition, park_miss_disposition,
    };
    use crate::native_update_auto_intent::LANDS_WITHIN;
    let model = native_update_apply_ladder_model();

    // The model: KeysOnly, a physical failure, the clock reaches Land under
    // the latch, and the lapse keeps Land.
    let mut keys_only = model.init_state();
    for _ in 0..2 {
        keys_only = model.successors("Advance", &keys_only)[0].clone();
    }
    let latched = model.successors("PhysicalFailure", &keys_only)[0].clone();
    assert_exact_model_action(&model, "PhysicalFailure", &keys_only, &latched);
    let aged = model.successors("Advance", &latched)[0].clone();
    assert_exact_model_action(&model, "Advance", &latched, &aged);
    // The latch's deadline passes first: a lapse is only ever AT the deadline
    // (plan P0-6).
    let due = model.successors("Due", &aged)[0].clone();
    assert_exact_model_action(&model, "Due", &aged, &due);
    let lapsed = model.successors("Lapse", &due)[0].clone();
    assert_exact_model_action(&model, "Lapse", &due, &lapsed);
    assert_eq!(lapsed["phase"], 3);

    // The shipping App on the same arc: anchored past the bound, latched with
    // a deadline that has passed.
    let mut app = crate::App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let now = std::time::Instant::now();
    let armed_at = now - LANDS_WITHIN - std::time::Duration::from_secs(30);
    app.auto_apply_ladder = Some(crate::AutoApplyLadder {
        build,
        armed_at,
        announced: ApplyPhase::KeysOnly,
    });
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        build,
        dmg_sha256: [0xab; 32],
        activation: false,
        retry_at: Some(now - std::time::Duration::from_secs(1)),
    });
    assert!(
        app.lapse_expired_auto_apply_manual_only(),
        "the latch lapses"
    );
    assert!(app.auto_apply_manual_only.is_none());
    assert_eq!(
        app.auto_apply_ladder.map(|ladder| ladder.armed_at),
        Some(armed_at),
        "the lapse keeps the anchor"
    );
    assert_eq!(
        app.automatic_apply_phase(now),
        ApplyPhase::Land,
        "the real phase after the lapse is the model's: {}",
        lapsed["phase"]
    );

    // The park miss: the model keeps the lane; the shipping disposition and
    // classifier keep it in the activity lane. From a QUIET terminal, so the
    // miss is its own transition (a machine that was already busy would leave
    // the state where `Busy` leaves it).
    let quiet = model.successors("Quiet", &keys_only)[0].clone();
    let missed = model.successors("ParkMissed", &quiet)[0].clone();
    assert_exact_model_action(&model, "ParkMissed", &quiet, &missed);
    assert_eq!(missed["manual_only"], 0);
    assert_eq!(missed["latched"], 0);
    assert_eq!(missed["phase"], keys_only["phase"]);
    let ParkMissDisposition::StandDown(stand_down) = park_miss_disposition(
        PRELAUNCH_MAX_PARK_MISSES,
        "the last rung missed".to_string(),
    ) else {
        panic!("past the last rung the attempt stands down");
    };
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        assert_eq!(
            HandoffFailureLane::classify(
                mode,
                stand_down.outcome,
                crate::ChildDeathEvidence::Unobserved,
                false
            ),
            HandoffFailureLane::ActivityRevoked,
            "{mode:?}: a park miss never reaches a physical shape"
        );
    }
}

/// THE 2026-09-22/23 UPDATE AUDIT (plan P0-6), bound to the model step that
/// describes it: `BundleSwap` — the failed candidate had already boot-applied the
/// bundle, so the next reconcile retires the download for the installed-bundle
/// activation of the SAME update — keeps the latch and its deadline.
///
/// The shipping arc, driven for real: a STRUCTURAL failure returns through
/// `abort_reaped_native_apply_before_reconcile` (a latch ten minutes out), then
/// `reconcile_native_update_facts` is fed the disk that failure left (the
/// installed bundle IS the target build). The real latch, projected onto the
/// model's `latched`/`due`/`swapped`, must be exactly the model's post-`BundleSwap`
/// state. The negative control is the mutant `BundleSwapClearsLatch` — the
/// v0.87–v0.91 retire arm, which cleared the latch and let the confirming retry
/// run 0.5 s after the failure — and `NoEarlyRelease` catches it.
#[test]
fn real_bundle_swap_keeps_the_latch_as_the_model_s_bundle_swap() {
    use crate::app_native::{
        HandoffFailureLane, NativeUpdateReconcileFacts, NativeUpdateReconcileTicket,
        PhysicalFailureShape,
    };
    use crate::native_updater_service::{ApplyAttemptTicket, InstalledUpdate};
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let model = native_update_apply_ladder_model();

    // The model: KeysOnly, a physical failure latches, and the bundle swaps.
    let mut keys_only = model.init_state();
    for _ in 0..2 {
        keys_only = model.successors("Advance", &keys_only)[0].clone();
    }
    let latched = model.successors("PhysicalFailure", &keys_only)[0].clone();

    // The shipping App on the same arc.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let build = app.native_updater_service.snapshot().current_build + 1;
    let ticket = ApplyAttemptTicket::for_test(build, COMMIT, &"ab".repeat(32));
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    let _ = app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
        HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
    );
    let before = app
        .auto_apply_manual_only
        .expect("PRECONDITION: the structural failure latched the lane");
    let _ = app.reconcile_native_update_facts(NativeUpdateReconcileFacts {
        _ticket: NativeUpdateReconcileTicket::for_test(1),
        observation_sequence: 1,
        observed_at: std::time::Instant::now(),
        durable: Some(DurableUpdateStatus {
            current_build: app.native_updater_service.snapshot().current_build,
            staged_dmg_sha256: Some("ab".repeat(32)),
            ..status(Some(build), 0)
        }),
        installed: Some(InstalledUpdate {
            build,
            commit: COMMIT.to_string(),
            version: None,
            receipt_build: Some(build),
            receipt_dmg_sha256: Some("ab".repeat(32)),
        }),
    });
    assert!(
        app.native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .is_some_and(|staged| staged.build == build && staged.is_installed_activation()),
        "PRECONDITION: the download retired for the activation of the same build"
    );

    // Project the real latch onto the model's variables.
    let after = app.auto_apply_manual_only;
    let now = std::time::Instant::now();
    let mut projected = latched.clone();
    projected.insert("swapped", 1);
    projected.insert("latched", i64::from(after.is_some()));
    projected.insert(
        "due",
        i64::from(
            after
                .and_then(|manual| manual.retry_at)
                .is_some_and(|at| at <= now),
        ),
    );
    assert_exact_model_action(&model, "BundleSwap", &latched, &projected);
    assert_eq!(
        after.map(|manual| manual.retry_at),
        Some(before.retry_at),
        "the deadline is the one the failure bought"
    );

    // NEGATIVE CONTROL: the mutant clears the latch before its deadline, the
    // invariant catches it, and the shipping reducer did not take that step.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let cleared = buggy.successors("BundleSwapClearsLatch", &latched)[0].clone();
    assert!(!buggy.check_invariant("NoEarlyRelease", &cleared));
    assert_ne!(
        cleared, projected,
        "the shipping retire arm must not be the v0.87–v0.91 mutant"
    );
}

/// THE 2026-09-22/23 UPDATE AUDIT (plan P0-3), bound to the model steps that
/// describe it: a capture failure is sorted into a MISS or a REFUSAL by its
/// type, and a refusal never reaches the activity lane.
///
/// Every `CaptureFailure` variant goes through the shipping
/// `classify_capture_failure`. Timing and storage are `Missed` — the model's
/// `ParkMissed`, which the lane re-parks on the next rung. A `Refused` is
/// `Refused`, never `Missed`; its stand-down is typed `CaptureRefused`, and the
/// shipping `HandoffFailureLane::classify` files that in the refusal lane on
/// both automatic modes — even with the main thread's activity flag raised —
/// which is the model's healthy `CaptureRefused` (`quiet` and `refusals`
/// untouched).
///
/// NEGATIVE CONTROL: the retired mapping, replayed through the shipping
/// functions it used — every capture `Err` became `ParkAttempt::Missed`, the
/// last rung's `park_miss_disposition` stood the successor down as
/// `ActivityRevoked`, and `classify` filed THAT in the activity lane. Projected
/// onto the model it is the mutant `CaptureRefusedAsActivity` (the terminal reads
/// busy, one refusal re-filed), and `RefusalNeverRetriesAsActivity` catches it.
#[cfg(unix)]
#[test]
fn real_capture_refusal_is_never_a_park_miss_and_never_activity() {
    use crate::app_native::HandoffFailureLane;
    use crate::app_update_handoff::{
        CaptureFailure, CaptureRefusal, PRELAUNCH_MAX_PARK_MISSES, ParkAttempt,
        ParkMissDisposition, capture_refusal_stand_down, classify_capture_failure,
        park_miss_disposition,
    };
    let model = native_update_apply_ladder_model();

    // The model at Land with a refusing desk and a quiet terminal: every one of
    // the park's alternatives is enabled there.
    let mut land = model.init_state();
    for _ in 0..3 {
        land = model.successors("Advance", &land)[0].clone();
    }
    let quiet = model.successors("Quiet", &land)[0].clone();
    let refusing = model.successors("DeskRefuses", &quiet)[0].clone();
    assert_exact_model_action(&model, "DeskRefuses", &quiet, &refusing);

    // TIMING AND STORAGE ARE MISSES — the model's `ParkMissed`.
    for failure in [
        CaptureFailure::Deadline { freeze_ms: 20 },
        CaptureFailure::EngineBusy,
        CaptureFailure::Storage,
    ] {
        match classify_capture_failure(&failure) {
            ParkAttempt::Missed(reason) => assert_eq!(reason, failure.to_string()),
            other => panic!("{failure:?} is timing, so it re-parks: {other:?}"),
        }
        let missed = model.successors("ParkMissed", &refusing)[0].clone();
        assert_exact_model_action(&model, "ParkMissed", &refusing, &missed);
    }

    // A REFUSAL IS A REFUSAL, whether or not it names a session.
    for failure in [
        CaptureFailure::Refused {
            local_id: Some(3),
            cause: "visible checkpoint set could not be committed canonically: too many \
                    sessions"
                .to_string(),
        },
        CaptureFailure::Refused {
            local_id: None,
            cause: "duplicate local id".to_string(),
        },
    ] {
        let CaptureFailure::Refused { local_id, .. } = &failure else {
            unreachable!("built as a refusal");
        };
        let refusal = match classify_capture_failure(&failure) {
            ParkAttempt::Refused(refusal) => refusal,
            ParkAttempt::Missed(reason) => {
                panic!("a deterministic refusal was filed as a park miss: {reason}")
            }
            other => panic!("{failure:?} must be Refused: {other:?}"),
        };
        assert_eq!(
            refusal,
            CaptureRefusal {
                local_id: *local_id,
                cause: failure.to_string(),
            }
        );
        let stand_down = capture_refusal_stand_down(&refusal);
        assert_eq!(
            stand_down.outcome,
            crate::UpdateHandoffOutcome::CaptureRefused
        );
        if let Some(local_id) = local_id {
            assert!(
                stand_down
                    .detail
                    .contains(&format!("refused session {local_id}")),
                "the stand-down names the session: {}",
                stand_down.detail
            );
        }
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            for revoked_by_activity in [false, true] {
                assert_eq!(
                    HandoffFailureLane::classify(
                        mode,
                        stand_down.outcome,
                        crate::ChildDeathEvidence::Unobserved,
                        revoked_by_activity,
                    ),
                    HandoffFailureLane::Refused,
                    "{mode:?} activity={revoked_by_activity}: a refusal is its own lane"
                );
            }
        }
        // …which is the model's healthy `CaptureRefused`: answered, not busy.
        let answered = model.successors("CaptureRefused", &refusing)[0].clone();
        assert_exact_model_action(&model, "CaptureRefused", &refusing, &answered);
        assert_eq!(answered["quiet"], refusing["quiet"]);
        assert_eq!(answered["refusals"], 0);

        // NEGATIVE CONTROL — the retired mapping through the shipping functions.
        let retired = ParkAttempt::Missed(failure.to_string());
        let ParkAttempt::Missed(reason) = retired else {
            unreachable!("the retired mapping");
        };
        let ParkMissDisposition::StandDown(retired_stand_down) =
            park_miss_disposition(PRELAUNCH_MAX_PARK_MISSES, reason)
        else {
            panic!("past the last rung the retired lane stood down");
        };
        assert_eq!(
            HandoffFailureLane::classify(
                ApplyMode::AutomaticPastGrace,
                retired_stand_down.outcome,
                crate::ChildDeathEvidence::Unobserved,
                false,
            ),
            HandoffFailureLane::ActivityRevoked,
            "the retired mapping filed the refusal as the machine being busy"
        );
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        assert!(
            model
                .successors("CaptureRefusedAsActivity", &refusing)
                .is_empty(),
            "the healthy ladder has no refusal it files as activity"
        );
        let refiled = buggy.successors("CaptureRefusedAsActivity", &refusing)[0].clone();
        assert_eq!(
            refiled["quiet"], 0,
            "the retired lane read the refusal as activity"
        );
        assert!(!buggy.check_invariant("RefusalNeverRetriesAsActivity", &refiled));
    }
}

/// REAL REFUSED DESKS through the shipping park capture (plan P0-3): the four
/// desks the 2026-09-22/23 audit found the outgoing build refusing on — a NUL
/// in a reported cwd, a 5K fullscreen grid (99x338, over the old per-grid
/// cap), a styled full-width row ending in a combining mark, and a parser left
/// inside an unterminated OSC — each park either (degraded, if it must) or
/// comes back `Refused`. None is ever a `Missed`: that answer re-parked the
/// same refusal on a wider rung and then retried it as the machine being busy
/// every fifteen minutes, forever. Before the producer slice every one of these
/// was a capture `Err`, and before this slice every capture `Err` was `Missed`.
#[cfg(unix)]
#[test]
fn real_refused_desks_park_degraded_or_refused_and_never_missed() {
    use crate::app_update_handoff::ParkAttempt;
    let caps = crate::seamless::WireCaps::current();
    let mut combining_row = b"\x1b[1m".to_vec();
    combining_row.extend_from_slice(&[b'a'; 148]);
    combining_row.extend_from_slice("e\u{301}".as_bytes());
    let desks = [
        (
            "a NUL in the reported cwd",
            None,
            b"\x1b]7;file:///tmp/a%00b\x07".to_vec(),
        ),
        (
            "a 5K fullscreen grid",
            Some((99, 338)),
            b"$ ls\r\n".to_vec(),
        ),
        (
            "a styled full-width row ending in a combining mark",
            Some((55, 149)),
            combining_row,
        ),
        (
            "a parser left inside an unterminated OSC",
            None,
            b"\x1b]0;x".to_vec(),
        ),
    ];
    for (desk, geometry, bytes) in desks {
        let mut app = crate::App::headless_for_test();
        for session in app.pool.iter() {
            // A fresh engine AT the desk's geometry rather than a resize under the
            // held guard: the resize would be `grep_guard`'s L0 shape, whose
            // `#[cfg(test)]` strip does not see this file's inner attribute.
            if let Some((rows, cols)) = geometry {
                *crate::term_lock(&session.term) = aterm_core::terminal::Terminal::new(rows, cols);
            }
            crate::term_lock(&session.term).process(&bytes);
        }
        match app.capture_park_outcome_for_conformance(caps) {
            Ok(_repainted) => {}
            Err(ParkAttempt::Refused(_)) => {}
            Err(ParkAttempt::Missed(reason)) => {
                panic!("{desk}: a deterministic desk was filed as a park miss: {reason}")
            }
            Err(other) => panic!("{desk}: the capture answers only a miss or a refusal: {other:?}"),
        }
    }
}

/// The hold cap and the re-park, bound to the model steps that describe them:
/// a terminal that never goes quiet stands the attempt down with its intent
/// retained and no reader ever stopped, and a park that missed its budget
/// resumes the readers and is gated afresh.
#[test]
fn real_hold_cap_and_repark_match_the_model_s_unparked_states() {
    let model = native_update_auto_intent_model();
    let mut state = model.init_state();
    for action in ["StageWakeIdle", "QuietElapsed", "Attempt"] {
        state = model.successors(action, &state)[0].clone();
    }
    // The machine goes busy during the hold: the park waits.
    let busy = model.successors("HoldActivity", &state)[0].clone();
    assert_exact_model_action(&model, "HoldActivity", &state, &busy);
    assert_eq!(busy["quiet"], 0);
    assert!(
        model.successors("ParkReaders", &busy).is_empty(),
        "no park while the terminal is busy"
    );
    // It never goes quiet: the cap stands the attempt down, intent retained.
    let stood_down = model.successors("HoldCapStandsDown", &busy)[0].clone();
    assert_exact_model_action(&model, "HoldCapStandsDown", &busy, &stood_down);
    assert_eq!(stood_down["phase"], 2, "back to Ready, not ManualOnly");
    assert_eq!(stood_down["intent"], 1, "the intent survives a hold cap");
    assert_eq!(stood_down["parked"], 0, "no reader was ever stopped");
    assert!(model.check_invariant("UnsuccessfulAttemptRetainsIntent", &stood_down));

    // Or it goes quiet again and the park lands, misses its budget, and is
    // gated afresh — with the readers back both times.
    let quiet_again = model.successors("HoldQuietElapsed", &busy)[0].clone();
    assert_exact_model_action(&model, "HoldQuietElapsed", &busy, &quiet_again);
    let parked = model.successors("ParkReaders", &quiet_again)[0].clone();
    assert_eq!(parked["parked"], 1);
    let reparked = model.successors("ReparkAfterMissedBudget", &parked)[0].clone();
    assert_exact_model_action(&model, "ReparkAfterMissedBudget", &parked, &reparked);
    assert_eq!(reparked["parked"], 0, "the readers resumed");
    assert_eq!(reparked["accepted"], 0, "nothing was granted");
    assert!(
        !model.successors("ParkReaders", &reparked).is_empty(),
        "and the re-park is gated afresh on a still-quiet machine"
    );

    // NO ACCEPTANCE WITHOUT A PARK: the unparked state cannot commit.
    assert!(
        model.successors("AttemptAccepted", &reparked).is_empty(),
        "a Commit over a screen nobody froze is unreachable"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let unparked_accept = buggy.successors("AttemptAccepted", &reparked)[0].clone();
    assert!(!buggy.check_invariant("AcceptedRequiresParkedReaders", &unparked_accept));
}

#[test]
fn real_auto_intent_reducer_preserves_newer_target_across_stale_wake() {
    let model = native_update_auto_intent_model();
    let initial = model.init_state();
    let armed = model.successors("ArmNewerIntent", &initial)[0].clone();
    assert_eq!(
        arm(ArmFacts {
            enabled: true,
            current_build: 10,
            armed_build: Some(12),
            armed_exact: true,
            manual_only_exact: false,
            manual_only_build: None,
            incoming_build: 9,
        }),
        ArmDecision::Keep
    );
    let after_stale = model.successors("ObserveStaleWake", &armed)[0].clone();
    assert_exact_model_action(&model, "ObserveStaleWake", &armed, &after_stale);
    assert_eq!(after_stale["intent"], 1);

    // Negative ordering control: the former clear-on-stale behavior is rejected.
    let mut cleared = after_stale.clone();
    cleared.insert("intent", 0);
    assert!(!model.check_invariant("NewerIntentSurvivesStaleWake", &cleared));
}

/// Bind the hidden-output model to the genuine monotonic admission predicate,
/// future-deadline constructor, and auto-intent reducer. The negative control is
/// the retired presentation-ack predicate that made a background tab block forever.
#[test]
fn real_hidden_output_quiet_clock_ages_without_present_ack() {
    let model = native_update_hidden_output_quiet_model();
    let mut state = model.init_state();
    for action in ["HiddenOutput", "WakeHandledNoPresent"] {
        let after = model.successors(action, &state)[0].clone();
        assert_exact_model_action(&model, action, &state, &after);
        state = after;
    }

    let latest_output_ns = 1_u64;
    let recent_now_ns = latest_output_ns + crate::AUTOMATIC_UPDATE_QUIET_EPOCH_NS - 1;
    assert!(!crate::automatic_output_activity_quiet(
        recent_now_ns,
        latest_output_ns
    ));
    let recent = model.successors("PollRecentActivity", &state)[0].clone();
    assert_exact_model_action(&model, "PollRecentActivity", &state, &recent);
    state = recent;

    let retry_now = std::time::Instant::now();
    let retry_at = crate::automatic_update_activity_retry_at(retry_now);
    assert_eq!(
        retry_at.saturating_duration_since(retry_now),
        crate::AUTOMATIC_UPDATE_QUIET_EPOCH
    );
    assert!(retry_at > retry_now);

    let quiet_now_ns = latest_output_ns + crate::AUTOMATIC_UPDATE_QUIET_EPOCH_NS;
    assert!(crate::automatic_output_activity_quiet(
        quiet_now_ns,
        latest_output_ns
    ));
    let quiet = model.successors("QuietEpochElapses", &state)[0].clone();
    assert_exact_model_action(&model, "QuietEpochElapses", &state, &quiet);
    assert_eq!(quiet["presentation_stamp"], 1);
    state = quiet;

    assert_eq!(
        poll(PollFacts {
            enabled: true,
            deadline_ready: true,
            current_build: 10,
            target_build: 11,
            work_active: false,
            applying: false,
            activity_quiet: crate::automatic_output_activity_quiet(quiet_now_ns, latest_output_ns,),
            phase: ApplyPhase::PreferIdle,
            staged_ready: true,
            staged_build: Some(11),
            staged_exact_target: true,
        }),
        PollDecision::Attempt {
            build: 11,
            quiet: true,
            phase: ApplyPhase::PreferIdle,
        }
    );
    let attempted = model.successors("Attempt", &state)[0].clone();
    assert_exact_model_action(&model, "Attempt", &state, &attempted);

    // Retired behavior: an unacknowledged latency sample made quiet false even
    // after arbitrarily old output. The model's Buggy branch additionally derives
    // retry_at from that expired output deadline, exposing both failures.
    let presentation_stamp = 1_u64;
    assert_ne!(presentation_stamp, 0);
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut stuck = buggy.init_state();
    for action in [
        "HiddenOutput",
        "WakeHandledNoPresent",
        "PollRecentActivity",
        "QuietEpochElapses",
    ] {
        assert!(buggy.fire(action, &mut stuck));
    }
    assert!(!buggy.check_invariant("OldHiddenPresentationCannotGate", &stuck));
    assert!(!buggy.check_invariant("ActivityRetryIsStrictlyFuture", &stuck));
}

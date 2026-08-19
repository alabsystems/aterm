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
    Model, native_update_admission_model, native_update_attempt_identity_model,
    native_update_auto_intent_model, native_update_disk_transaction_model,
    native_update_hidden_output_quiet_model, native_updater_model,
};
use aterm_spec::interp::{State, admits};

use crate::native_update_admission::{
    AdmissionBlock, AdmissionDecision, AdmissionFacts, ApplyLane, classify,
};
use crate::native_update_auto_intent::{
    ArmDecision, ArmFacts, AttemptDisposition, AttemptResult, PollDecision, PollFacts, WaitReason,
    arm, finish, poll,
};
use crate::native_updater_service::{
    ApplyDecision, ApplyMode, ApplyPreflightStart, CheckCompletion, CheckStart, ClosePreflight,
    DurableUpdateStatus, NativeUpdaterService, UpdaterModelState, UpdaterTransition,
};

fn status(staged_build: Option<u64>, failing_checks: u32) -> DurableUpdateStatus {
    DurableUpdateStatus {
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
        !crate::should_dispatch_boot_health_confirmation(false, false, false, false, true, true),
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
        false, false, true, false, true, true,
    ));
    assert!(
        !crate::should_dispatch_boot_health_confirmation(false, false, true, true, true, true),
        "the same present cannot enqueue a second confirmation"
    );
    assert!(
        !crate::should_dispatch_boot_health_confirmation(true, false, true, false, true, false),
        "headless proves health at the control-socket boundary, not on glass"
    );
    assert!(
        crate::should_dispatch_boot_health_confirmation(true, true, false, false, true, false),
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
            activity_grace_expired: false,
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
            activity_grace_expired: false,
            staged_ready: true,
            staged_build,
            staged_exact_target: true,
        }),
        PollDecision::Attempt {
            build: 11,
            quiet: true
        }
    );
    let quiet = model.successors("QuietElapsed", &modeled)[0].clone();
    assert_exact_model_action(&model, "QuietElapsed", &modeled, &quiet);
    modeled = quiet;
    let attempting = model.successors("Attempt", &modeled)[0].clone();
    assert_exact_model_action(&model, "Attempt", &modeled, &attempting);
    modeled = attempting;

    let accepted = model.successors("AttemptAccepted", &modeled)[0].clone();
    assert_eq!(
        finish(AttemptResult::Accepted),
        AttemptDisposition::Complete
    );
    assert_exact_model_action(&model, "AttemptAccepted", &modeled, &accepted);
    assert_eq!(accepted["accepted"], 1);

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
/// exists to remove. Deferral is now bounded: inside the window activity still
/// wins, past it the lossless lane lands anyway and reports `quiet: false` so the
/// host takes the lane that activity cannot revoke.
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
        activity_grace_expired: false,
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

    // Past it, the same still-busy facts attempt.
    assert_eq!(
        poll(PollFacts {
            activity_grace_expired: true,
            ..busy
        }),
        PollDecision::Attempt {
            build: 11,
            quiet: false
        }
    );
    let attempted = model.successors("Attempt", &closed)[0].clone();
    assert_exact_model_action(&model, "Attempt", &closed, &attempted);
    assert_eq!(attempted["parked"], 1);
    assert!(model.check_invariant(
        "AutomaticAttemptRequiresQuietOrClosedGraceWindow",
        &attempted
    ));

    // Negative control: parking with neither a quiet machine nor a closed window
    // is the unsafe shape the invariant exists to reject.
    let mut unbounded = attempted.clone();
    unbounded.insert("grace_expired", 0);
    assert!(!model.check_invariant(
        "AutomaticAttemptRequiresQuietOrClosedGraceWindow",
        &unbounded
    ));
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
            activity_grace_expired: false,
            staged_ready: true,
            staged_build: Some(11),
            staged_exact_target: true,
        }),
        PollDecision::Attempt {
            build: 11,
            quiet: true
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

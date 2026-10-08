// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the process-global native updater reducer.
//!
//! The trace is emitted by the genuine service while it accepts a worker result
//! and an apply preflight. The test projects those scalar states onto the
//! `NativeUpdater` model independently; it does not reconstruct an expected state
//! from the action definition.

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
#[cfg(unix)]
use crate::native_updater_service::ApplyMode;
use crate::native_updater_service::{
    ApplyDecision, ApplyPreflightStart, CheckCompletion, CheckStart, ClosePreflight,
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

    let preflight = match service.begin_apply_preflight() {
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
    let preflight = match service.begin_apply_preflight() {
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

    let preflight = match service.begin_apply_preflight() {
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

    let retry = match service.begin_apply_preflight() {
        ApplyPreflightStart::Inspect(ticket) => ticket,
        other => panic!("aborted stage must remain retryable, got {other:?}"),
    };
    let retry_command = match service.finish_apply_preflight(retry, ClosePreflight::Ready) {
        ApplyDecision::Execute(command) => command,
        other => panic!("expected retry command, got {other:?}"),
    };
    assert_last_batch(&model, &service);
    // Negative control for the fresh-nonce law: the real retry below mints a new
    // attempt. Re-arming under the failed attempt's own nonce is the one identity
    // a late replay of that failure would still match.
    let buggy_identity = aterm_spec::interp::with_buggy(&identity_model, 1);
    assert!(
        identity_model
            .successors("RetryReusingFailedNonce", &identity)
            .is_empty()
    );
    let reused = buggy_identity.successors("RetryReusingFailedNonce", &identity)[0].clone();
    assert!(!buggy_identity.check_invariant("RetryUsesFreshIdentity", &reused));
    let retry_identity = identity_model.successors("StartAttempt", &identity)[0].clone();
    assert_exact_model_action(&identity_model, "StartAttempt", &identity, &retry_identity);
    identity = retry_identity;
    let retry_attempt = retry_command.attempt();
    assert_ne!(retry_attempt, first_attempt);
    assert!(identity_model.check_invariant("RetryUsesFreshIdentity", &identity));

    // Negative control for the attempt nonce: a delayed failure from attempt A
    // cannot cancel the live authority for attempt B.
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
    let canceled = buggy_identity.successors("AcceptStaleAbort", &identity)[0].clone();
    assert_eq!(admits(&identity_model, &identity, &canceled), None);
    assert!(!buggy_identity.check_invariant("StaleAbortCannotCancelRetry", &canceled));

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
/// first-present boundary. Negative control: allocating a window must not
/// enable proof/disarm before a successful drawable present.
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
    // `unknown_foregrounds` is not a native-state fact: it says whether a
    // destructive replacement (the cold lane only) would hang up a running
    // job. It enters the model through `ObserveForegroundJob` (fired whenever
    // `live_ptys > 0`; the matrix never generates `unknown_foregrounds >
    // live_ptys`).
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
        // An unprobeable foreground blocks for the same reason as
        // `LivePtysNeedSeamless` (a destructive swap could hang up something
        // on a PTY and the lossless lane is unavailable), so it refines the
        // same model action.
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

    // Negative control: blocking a safe seamless handoff only because a
    // foreground job exists violates the named progress invariant.
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
    // The same block is exactly the model's named v0.53 mutant, which the
    // healthy model never admits and the progress law refuses.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let v053 = buggy.successors("BlockForegroundDespiteSeamless", &before)[0].clone();
    assert_eq!(
        (v053["phase"], v053["decision"], v053["retry_eligible"]),
        (3, 3, 1)
    );
    assert_eq!(admits(&model, &before, &v053), None);
    assert!(!buggy.check_invariant("ForegroundJobsDoNotBlockSeamless", &v053));

    // Negative control for the cold lane: with the seamless lane gone and a live
    // job on a PTY, the real classifier blocks. Without its live-session check,
    // a release macOS build (whose cold arm re-checks only by `debug_assert!`)
    // would run the destructive re-exec over the job.
    let facts = AdmissionFacts {
        seamless_capable: false,
        ..facts
    };
    assert_eq!(
        classify(facts),
        AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless)
    );
    let before = admission_before(&model, facts);
    let hung_up = buggy.successors("ReexecColdOverLiveSessions", &before)[0].clone();
    assert_eq!(admits(&model, &before, &hung_up), None);
    assert!(!buggy.check_invariant("ColdFallbackNeverDropsForeground", &hung_up));
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

/// Bind the event-order case to the shipping auto-intent reducer and the
/// genuine updater service. A stage wake arms while a manual worker is active;
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

    // The park is its own step between the attempt and its acceptance: the
    // attempt launches the successor with every reader live, and only a parked
    // attempt can be accepted (`AcceptedRequiresParkedReaders`).
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
/// `activity_quiet` samples a machine-wide input clock plus every live PTY's
/// latest output, which on a daily driver are almost never idle together, so
/// "activity always defers" would never apply a staged build. Deferral is
/// bounded by the ladder: in its first phase activity still wins, in every
/// later phase the same busy facts attempt, and the phase rides along so the
/// park gate reads the ladder.
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

    // The park is its own gated step. The launch above cost the user nothing
    // (every reader stayed live through the successor's swap and boot); this is
    // the step they feel.
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

/// The real park gate, bound to the model step it implements.
///
/// `Attempt` is the launch, gated by the poll policy (the test above);
/// `ParkReaders` is the freeze, gated by `prelaunch_park_admitted`. The
/// shipping predicate is bound to the model's guard over every combination of
/// facts the model expresses (the model's `quiet` is the ladder's first phase
/// asking for a quiet moment; `grace_expired` is every later phase).
///
/// The masters are enumerated too: `masters_quiet` comes from the real peek
/// over real PTY masters (a quiet live session, a live one with unread output,
/// and an exited one whose slave hung up with bytes still queued), and the
/// gate must park exactly where the model does and the masters allow. A live
/// session's unread output holds it until the `Land` bound has waited
/// `PRELAUNCH_LAND_MAX_WAITS` times; an exited pane is never counted as output
/// and holds it only as a dead session, until its pane closes.
///
/// Unix-only like the gate it binds: the `app_update_handoff` items below are
/// `#[cfg(unix)]` (the in-place handoff has no Windows lane). The model-side
/// checks above run everywhere.
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
    // admissible. `held_for` is zero throughout: the cap is the other gate,
    // enumerated in `app_update_handoff::park_gate_tests` along with the
    // ladder's finer preferences (keys, output).
    let calm = |quiet: bool| ActivityFacts {
        quiet,
        hands_off_keys: true,
        output_quiet: true,
        focused: true,
        consent_warmup: false,
        harness_restored_pending: false,
    };
    // Real masters. An exited pane: its command wrote, then its slave closed —
    // the state a `--hold` pane sits in for as long as it stays open.
    // Close-on-exec from birth: a child another test spawns meanwhile never
    // keeps the slave open past its exec, so a closed slave reads as hung up
    // (the fd-copy sweep of 2026-09-27).
    let openpty = || aterm_pty::open_pty_pair().expect("open_pty_pair");
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
    // A hang detector, not 2 s: the hang-up waits for every copy of the slave,
    // and a child another test is forking holds one until it execs (the fd-copy
    // sweep of 2026-09-27). A slave that never closes still fails.
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
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
                        for (land_waits, land_gate_relaxed) in
                            [(0, false), (PRELAUNCH_LAND_MAX_WAITS, false), (0, true)]
                        {
                            let real = prelaunch_park_admitted(
                                ParkGateFacts {
                                    mode,
                                    phase,
                                    activity: calm(quiet),
                                    masters_quiet,
                                    masters_alive,
                                    land_waits,
                                    land_gate_relaxed,
                                    held_for: std::time::Duration::ZERO,
                                    recording: false,
                                },
                                prelaunch_hold_cap(mode),
                            ) == ParkGate::Park;
                            // The successor's policy brings the
                            // bound forward; it relaxes nothing else.
                            let bound_waited = phase == ApplyPhase::Land
                                && (land_waits >= PRELAUNCH_LAND_MAX_WAITS || land_gate_relaxed);
                            assert_eq!(
                                real,
                                model_parks && masters_alive && (masters_quiet || bound_waited),
                                "{mode:?} quiet={quiet} {phase:?} {desk} after {land_waits} \
                                 waits, policy-relaxed={land_gate_relaxed}: the shipping gate \
                                 parks exactly where the model does and the masters allow"
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

    // Negative control: the mutant parks with neither fact, and the invariant
    // catches it.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut busy = state.clone();
    busy.insert("quiet", 0);
    busy.insert("grace_expired", 0);
    assert!(
        model.successors("ParkReaders", &busy).is_empty(),
        "the healthy model never parks a busy terminal"
    );
    let parked = buggy.successors("ParkWithoutQuietOrGrace", &busy)[0].clone();
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
                    land_gate_relaxed: false,
                    held_for: std::time::Duration::ZERO,
                    recording: false,
                },
                prelaunch_hold_cap(mode),
            ),
            ParkGate::Wait(_)
        ));
    }
}

/// The ladder, bound to its model (docs/DESIGN-auto-apply-ladder-2026-09-21.md).
///
/// The shipping `apply_phase` tiles the wall clock into the model's four
/// phases, and the shipping `automatic_park_refusal` (the one predicate the
/// entry gate and the park gate both read) must admit exactly the states in
/// which the model's `Park` is enabled, over every phase and every combination
/// of the six facts: four about the terminal, and two bounded holds no phase
/// relaxes (the user's consent warm-up and a cold restore's agents still being
/// relaunched, both projected onto the model's one `warmup`). The mutant's
/// stand-down is caught as a wedge: a busy terminal that never updates.
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
        for bits in 0..64u32 {
            let facts = ActivityFacts {
                quiet: bits & 1 != 0,
                hands_off_keys: bits & 2 != 0,
                output_quiet: bits & 4 != 0,
                focused: bits & 8 != 0,
                consent_warmup: bits & 16 != 0,
                harness_restored_pending: bits & 32 != 0,
            };
            let mut state = model.init_state();
            state.insert("phase", modeled);
            state.insert("quiet", i64::from(facts.quiet));
            state.insert("keys", i64::from(facts.hands_off_keys));
            state.insert("output", i64::from(facts.output_quiet));
            state.insert("focused", i64::from(facts.focused));
            // The model's bounded hold: either of the two the shipping facts
            // name. If the shipping predicate ignored the restored queue, the
            // model would refuse with `warmup = 1` and the real one would park.
            state.insert(
                "warmup",
                i64::from(facts.consent_warmup || facts.harness_restored_pending),
            );
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

    // The warm-up at the bound: past `LANDS_WITHIN` the shipping predicate and
    // the model both still refuse a park over the user's warm-up, and the
    // warm-up ending is what lets the landing through.
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
        harness_restored_pending: false,
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
    // The restored agents' queue is the same hold at the bound: refused over
    // a busy terminal where nothing else refuses.
    assert_eq!(
        automatic_park_refusal(
            apply_phase(LANDS_WITHIN),
            ActivityFacts {
                consent_warmup: false,
                harness_restored_pending: true,
                ..busy_warmup
            }
        ),
        Some(crate::native_update_auto_intent::RESTORED_PENDING_REFUSAL)
    );
    // Negative control: the mutant's ruleless park lands over the warm-up and
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

/// The `Lapse` and `ParkMissed` model steps, bound to shipping code.
///
/// `Lapse`: a physical-failure latch lapsing resumes the ladder where the clock
/// is. `lapse_expired_auto_apply_manual_only` releases the latch and leaves
/// `App::auto_apply_ladder` alone, so `automatic_apply_phase` after the lapse
/// is the model's post-`Lapse` phase (`Land` when the latch held past the
/// bound), never `PreferIdle` again.
///
/// `ParkMissed`: a park that missed every freeze rung stands its successor
/// down as activity. `park_miss_disposition` answers `ActivityRevoked`, and
/// `HandoffFailureLane::classify` keeps that outcome out of every physical
/// shape, so the model's `manual_only` stays 0, as the real latch does.
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
    // The latch's deadline passes first: a lapse is only ever at the deadline.
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
        restored_hold_said: false,
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
    // classifier keep it in the activity lane. From a quiet terminal, so the
    // miss is its own transition.
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

/// The `BundleSwap` model step: when the failed candidate had already
/// boot-applied the bundle, the next reconcile retires the download for the
/// installed-bundle activation of the same update and keeps the latch and its
/// deadline.
///
/// The shipping arc runs for real: a structural failure returns through
/// `abort_reaped_native_apply_before_reconcile` (a latch ten minutes out), then
/// `reconcile_native_update_facts` is fed the disk that failure left (the
/// installed bundle is the target build). The real latch, projected onto the
/// model's `latched`/`due`/`swapped`, must be exactly the post-`BundleSwap`
/// state. Negative control: the mutant `BundleSwapClearsLatch` (clearing the
/// latch lets the confirming retry run 0.5 s after the failure) is caught by
/// `NoEarlyRelease`.
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
            trial_launches: 0,
        }),
        installed_unproven: false,
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

    // Negative control: the mutant clears the latch before its deadline, the
    // invariant catches it, and the shipping reducer did not take that step.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let cleared = buggy.successors("BundleSwapClearsLatch", &latched)[0].clone();
    assert!(!buggy.check_invariant("NoEarlyRelease", &cleared));
    assert_ne!(
        cleared, projected,
        "the shipping retire arm must not be the v0.87–v0.91 mutant"
    );
}

/// The `Supersede` model step: a verified newer release behind a latch that
/// converged structurally on the installed activation, with no deadline left,
/// takes the activation's place.
///
/// The shipping arc runs for real: two structural failures of build 11's
/// activation converge the lane, and the first count (the trial one launch
/// from its revert) withdraws the day's re-sample, so nothing automatic lapses
/// the latch (the model's `Converge`). A verified download of 12 is reconciled
/// (`NewerRelease`); the look starts the retire, and the retire's answer
/// arrives with the disk it left: the running build back at the install path
/// and 12 staged. The real latch, stage, intent and ladder, projected onto the
/// model, must be exactly the post-`Supersede` state: unlatched, the newer
/// build staged and armed on a ladder of its own, build 11 never armed or
/// launched.
///
/// The first retire is refused (the verification ran out of budget): the real
/// latch, verdict and retire project onto `SupersedeRefused` (latched, the
/// release not spent, waiting out its retry deadline), and the deadline
/// passing onto `SupersedeRetryDue`, after which the look starts the retire
/// again and it lands.
///
/// Negative controls: three mutants from the same state (the look that keeps
/// the latch, a release for one more attempt at the activation, and a refusal
/// that spends the release for good) are not what ships, and
/// `ANewerReleaseClearsAConvergedLatch` catches each.
#[test]
fn real_newer_release_supersedes_a_converged_activation_as_the_model_s_supersede() {
    use crate::app_native::{
        ActivationSupersedeCompletion, HandoffFailureLane, NativeUpdateReconcileFacts,
        NativeUpdateReconcileTicket, PhysicalFailureShape,
    };
    use crate::native_updater_service::{ApplyAttemptTicket, InstalledUpdate};
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let model = native_update_apply_ladder_model();

    // The model: KeysOnly, a physical failure latches, the bundle swaps, the
    // lane converges on the activation, and a newer release arrives.
    let mut keys_only = model.init_state();
    for _ in 0..2 {
        keys_only = model.successors("Advance", &keys_only)[0].clone();
    }
    let latched = model.successors("PhysicalFailure", &keys_only)[0].clone();
    let swapped = model.successors("BundleSwap", &latched)[0].clone();
    let converged = model.successors("Converge", &swapped)[0].clone();
    let waiting = model.successors("NewerRelease", &converged)[0].clone();

    // The shipping App on the same arc.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    app.native_updater_service = NativeUpdaterService::new(10, "1.0.10", true);
    let spent_trial = crate::app_native::BOOT_TRIAL_LAUNCH_LIMIT - 1;
    let bundle = |build: u64| InstalledUpdate {
        build,
        commit: COMMIT.to_string(),
        version: None,
        receipt_build: None,
        receipt_dmg_sha256: None,
        trial_launches: if build == 11 { spent_trial } else { 0 },
    };
    let facts = |sequence: u64, installed: u64, newer: Option<u64>| NativeUpdateReconcileFacts {
        _ticket: NativeUpdateReconcileTicket::for_test(sequence),
        observation_sequence: sequence,
        observed_at: std::time::Instant::now(),
        durable: Some(DurableUpdateStatus {
            staged_dmg_sha256: newer.map(|_| "cd".repeat(32)),
            ..status(newer, 0)
        }),
        installed: Some(bundle(installed)),
        installed_unproven: false,
    };
    let _ = app.reconcile_native_update_facts(facts(1, 11, None));
    let activation = crate::native_updater_service::installed_activation_digest(11, COMMIT);
    for _ in 0..crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
        let ticket = ApplyAttemptTicket::for_test(11, COMMIT, &activation);
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        let _ = app.abort_reaped_native_apply_before_reconcile(
            &ticket,
            "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
        );
    }
    // The first count: no room, so the re-sample is withdrawn — no deadline left.
    let _ = app.reconcile_native_update_facts(facts(2, 11, None));
    let latch = app
        .auto_apply_manual_only
        .expect("PRECONDITION: the converged lane is latched");
    assert!(
        latch.activation && latch.retry_at.is_none() && app.auto_apply_structural_verdict.is_some(),
        "PRECONDITION: a structural latch on the activation with no deadline — the model's \
         `Converge`: {latch:?}"
    );

    // NewerRelease, and the look.
    let _ = app.reconcile_native_update_facts(facts(3, 11, Some(12)));
    let job = app
        .native_activation_supersede
        .clone()
        .expect("the look starts the retire of 11's activation");
    assert!(
        app.auto_apply_intent.is_none(),
        "nothing arms 11 while the retire runs"
    );
    // The retire is refused for the moment: nothing moved.
    app.finish_activation_supersede(ActivationSupersedeCompletion {
        job,
        outcome: Err(aterm_update::SupersedeRefusal::ForNow(
            "the installed build 11 stays in place: the staged bundle does not verify: the \
             apply budget ran out"
                .to_string(),
        )),
        facts: Some(facts(4, 11, Some(12))),
    });
    let project_refusal =
        |app: &crate::App, from: &aterm_spec::interp::State| {
            let mut projected = from.clone();
            let verdict = app.auto_apply_structural_verdict.expect("the verdict");
            projected.insert("latched", i64::from(app.auto_apply_manual_only.is_some()));
            projected.insert("looked", i64::from(verdict.newer_spent >= 12));
            projected.insert(
                "retry",
                i64::from(verdict.newer_retry.is_some_and(|retry| {
                    retry.newer == 12 && std::time::Instant::now() < retry.at
                })),
            );
            projected
        };
    let refused = project_refusal(&app, &waiting);
    assert_exact_model_action(&model, "SupersedeRefused", &waiting, &refused);
    assert!(
        app.native_activation_supersede.is_none() && app.auto_apply_intent.is_none(),
        "nothing arms 11 after the refusal, and nothing runs"
    );
    // Negative control: the refusal that spends the release for good.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let spent = buggy.successors("RefusedRetireSpendsTheRelease", &waiting)[0].clone();
    assert_ne!(
        spent, refused,
        "the shipping refusal gives the release back"
    );
    assert!(!buggy.check_invariant("ANewerReleaseClearsAConvergedLatch", &spent));
    // The retry deadline passes, and the lane's look starts the retire again.
    let verdict = app.auto_apply_structural_verdict.expect("the verdict");
    let retry = verdict.newer_retry.expect("a retry waits");
    app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
        newer_retry: Some(crate::SupersedeRetry {
            at: std::time::Instant::now() - std::time::Duration::from_secs(1),
            ..retry
        }),
        ..verdict
    });
    let mut due = project_refusal(&app, &refused);
    due.insert("retry", 0);
    assert_exact_model_action(&model, "SupersedeRetryDue", &refused, &due);
    assert!(!app.lapse_expired_auto_apply_manual_only());
    let job = app
        .native_activation_supersede
        .clone()
        .expect("at its deadline the look starts the retire again");
    // The retire answers, with the disk it left.
    app.finish_activation_supersede(ActivationSupersedeCompletion {
        job,
        outcome: Ok(aterm_update::SupersededActivation {
            activation_build: 11,
            restored_build: 10,
            newer_build: 12,
            trial_launches: spent_trial,
            trial_disarm_deferred: None,
        }),
        facts: Some(facts(5, 10, Some(12))),
    });

    // Project the real lane onto the model's variables.
    let waiting = due;
    let now = std::time::Instant::now();
    let stage = app.native_updater_service.snapshot().staged.clone();
    let mut projected = waiting.clone();
    projected.insert("latched", i64::from(app.auto_apply_manual_only.is_some()));
    projected.insert(
        "superseded",
        i64::from(
            stage
                .as_ref()
                .is_some_and(|stage| stage.build == 12 && !stage.is_installed_activation())
                && app.auto_apply_intent.map(|intent| intent.build) == Some(12),
        ),
    );
    projected.insert(
        "looked",
        i64::from(
            app.auto_apply_structural_verdict
                .is_some_and(|verdict| verdict.newer_spent >= 12),
        ),
    );
    let phase = app.automatic_apply_phase(now);
    let modeled_phase = match phase {
        ApplyPhase::PreferIdle => 0,
        ApplyPhase::PreferOutputGap => 1,
        ApplyPhase::KeysOnly => 2,
        ApplyPhase::Land => 3,
    };
    projected.insert("phase", modeled_phase);
    projected.insert("reached", modeled_phase);
    assert_exact_model_action(&model, "Supersede", &waiting, &projected);
    assert_eq!(
        app.auto_apply_ladder.map(|ladder| ladder.build),
        Some(12),
        "the newer build is armed on a ladder of its own"
    );
    assert!(
        app.native_updater_service.snapshot().active.is_none(),
        "build 11 was never launched for it"
    );

    // Negative controls: the look that keeps the latch, and a release for one
    // more attempt at the activation; neither ships, both are caught.
    for mutant in ["NewerReleaseIgnored", "NewerReleaseRetriesTheActivation"] {
        assert!(model.successors(mutant, &waiting).is_empty());
        let bad = buggy.successors(mutant, &waiting)[0].clone();
        assert_ne!(bad, projected, "the shipping look is not {mutant}");
        assert!(
            !buggy.check_invariant("ANewerReleaseClearsAConvergedLatch", &bad),
            "{mutant} is caught"
        );
    }
}

/// A capture failure is sorted into a miss or a refusal by its type, and a
/// refusal never reaches the activity lane.
///
/// Every `CaptureFailure` variant goes through the shipping
/// `classify_capture_failure`. Timing and storage are `Missed` (the model's
/// `ParkMissed`, re-parked on the next rung). A `Refused` is `Refused`, never
/// `Missed`; its stand-down is typed `CaptureRefused`, and
/// `HandoffFailureLane::classify` files that in the refusal lane on both
/// automatic modes, even with the main thread's activity flag raised: the
/// model's healthy `CaptureRefused` (`quiet` and `refusals` untouched).
///
/// Negative control: mapping every capture `Err` to `ParkAttempt::Missed`,
/// replayed through the shipping functions (the last rung's
/// `park_miss_disposition` stands the successor down as `ActivityRevoked` and
/// `classify` files that in the activity lane). Projected onto the model it is
/// the mutant `CaptureRefusedAsActivity`, which `RefusalNeverRetriesAsActivity`
/// catches.
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

    // Timing and storage are misses: the model's `ParkMissed`.
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

    // A refusal is a refusal, whether or not it names a session.
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

        // Negative control: every `Err` as `Missed`, through the shipping functions.
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

/// Real refused desks through the shipping park capture: a NUL in a reported
/// cwd, a 5K fullscreen grid (99x338), a styled full-width row ending in a
/// combining mark, and a parser left inside an unterminated OSC. Each either
/// parks (degraded, if it must) or comes back `Refused`, never `Missed`: a
/// miss would re-park the same refusal on a wider rung and then retry it as
/// the machine being busy, forever.
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
            // A fresh engine at the desk's geometry rather than a resize under
            // the held guard: the resize would trip the main-loop census, whose
            // `#[cfg(test)]` mask does not see this file's inner attribute.
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

    // No acceptance without a park: the unparked state cannot commit.
    assert!(
        model.successors("AttemptAccepted", &reparked).is_empty(),
        "a Commit over a screen nobody froze is unreachable"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let unparked_accept = buggy.successors("AcceptWithoutPark", &reparked)[0].clone();
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
/// future-deadline constructor and auto-intent reducer. Negative control: a
/// presentation-ack predicate, which makes a background tab block forever.
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
    // Negative control on the shipping predicate: output inside the epoch is
    // activity to the real clock. Fed the hidden tab's present stamp instead
    // (zero, for a tab that never presented), the same predicate calls it
    // quiet mid-output: the mutant's reading, whose following attempt the
    // clock rule refuses.
    let hidden_present_ns = 0_u64;
    assert!(crate::automatic_output_activity_quiet(
        recent_now_ns,
        hidden_present_ns
    ));
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let misread = buggy.successors("QuietFromLastPresent", &recent)[0].clone();
    assert_eq!(admits(&model, &recent, &misread), None);
    let mid_output = buggy.successors("Attempt", &misread)[0].clone();
    assert!(!buggy.check_invariant("AttemptOnlyAfterAgedQuiet", &mid_output));
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

    // The presentation-ack mutant: an unacknowledged latency sample makes quiet
    // false even after arbitrarily old output, and the model's mutant also
    // derives retry_at from that expired output deadline, exposing both.
    let presentation_stamp = 1_u64;
    assert_ne!(presentation_stamp, 0);
    let mut stuck = buggy.init_state();
    for action in [
        "HiddenOutput",
        "WakeHandledNoPresent",
        "PollRecentActivity",
        "QuietGatedOnPresentationSample",
    ] {
        assert!(buggy.fire(action, &mut stuck));
    }
    assert!(!buggy.check_invariant("OldHiddenPresentationCannotGate", &stuck));
    assert!(!buggy.check_invariant("ActivityRetryIsStrictlyFuture", &stuck));
}

/// The observable state of a structural latch, as the Tier-1 bind of
/// `NativeUpdateStructuralLatch` reads it. `day`, `newer`, `pending` and `room`
/// are the environment (the clock, the checker's stage, the lane having
/// looked, the boot sentinel's count), which the test drives and waives; the
/// rest is read off the shipping state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StructuralLatchObservation {
    pub(crate) room: bool,
    /// The installed facts no longer report the latched build, so no count can
    /// be read (`App::native_installed_trial` is not the latch's); `room` is
    /// then no reading.
    pub(crate) unread: bool,
    /// …and the latest read could not PROVE the installed bundle at all
    /// (`App::native_installed_trial_unproven`, round eight #80): the model's
    /// `unread = 2`.
    pub(crate) unproven: bool,
    /// Due looks in a row that found no count
    /// (`AutoApplyStructuralVerdict::unmeasured_looks`).
    pub(crate) looks: u8,
    /// Due looks in a row over an unproven read
    /// (`AutoApplyStructuralVerdict::unproven_looks`; the model's `doubts`).
    pub(crate) doubts: u8,
    pub(crate) latched: bool,
    pub(crate) pending: bool,
    pub(crate) day: bool,
    pub(crate) owed: bool,
    pub(crate) newer: bool,
    pub(crate) newer_spent: bool,
    pub(crate) said: bool,
    pub(crate) escaped: bool,
    /// A retire of the latched activation is in flight
    /// (`App::native_activation_supersede`).
    pub(crate) superseding: bool,
    /// It landed: the newer release is the lane's stage in the activation's
    /// place.
    pub(crate) superseded: bool,
    /// It was refused, and said.
    pub(crate) refused: bool,
    /// A refused retire's release waits out its retry deadline
    /// (`AutoApplyStructuralVerdict::newer_retry`, not yet due).
    pub(crate) backoff: bool,
    /// The latched build was launched for an attempt while a newer release
    /// waited behind it.
    pub(crate) booted_old: bool,
}

/// Project one [`StructuralLatchObservation`] onto the model's variables. The
/// re-sample is owed or spent, never both and never neither, in shipping state
/// (`AutoApplyStructuralVerdict::resample_at`): `spent` is its complement.
pub(crate) fn project_structural_latch(
    model: &Model,
    observed: StructuralLatchObservation,
) -> State {
    let mut state = model.init_state();
    state.insert("phase", 1);
    state.insert("room", i64::from(observed.room));
    state.insert(
        "unread",
        if observed.unproven {
            2
        } else {
            i64::from(observed.unread)
        },
    );
    state.insert("looks", i64::from(observed.looks));
    state.insert("doubts", i64::from(observed.doubts));
    state.insert("latched", i64::from(observed.latched));
    state.insert("pending", i64::from(observed.pending));
    state.insert("day", i64::from(observed.day));
    state.insert("owed", i64::from(observed.owed));
    state.insert("spent", i64::from(!observed.owed));
    state.insert("newer", i64::from(observed.newer));
    state.insert("newer_spent", i64::from(observed.newer_spent));
    state.insert("said", i64::from(observed.said));
    state.insert("escaped", i64::from(observed.escaped));
    state.insert("superseding", i64::from(observed.superseding));
    state.insert("superseded", i64::from(observed.superseded));
    state.insert("refused", i64::from(observed.refused));
    state.insert("backoff", i64::from(observed.backoff));
    state.insert("booted_old", i64::from(observed.booted_old));
    state
}

/// The decision, exhaustively: for every reachable model state in which the
/// lane looks at the latch (`Decide` enabled), the shipping `structural_latch`
/// is fed the facts that state names (the day's re-sample due, an unspent
/// newer download, the trial's room), and its answer, applied as
/// `App::look_at_structural_latch` applies it (a supersede starts the retire
/// and spends the release while the latch holds; a release clears the latch
/// and spends the re-sample; a hold keeps it, spends the re-sample and is
/// said), must be exactly the model's `Decide` successor.
///
/// Negative control: the `Buggy = 1` `Decide` from the same states answers a
/// newer release looked at before the day with one more attempt at the older
/// build (room) or a release held for the Version menu (no room), and strands
/// the latch with a trial-blind re-sample where nothing newer is on offer. The
/// shipping decision differs wherever anything was earned, and the invariants
/// catch it there. For a newer release looked at after the day, `Buggy = 1`
/// starts the retire as the shipping look does; its defect is the refusal
/// that follows (`SupersedeRefused`, bound in
/// [`real_structural_latch_arc_is_the_model_s_arc`]).
#[test]
fn real_structural_latch_decision_is_exactly_the_model_s_decide() {
    use crate::native_update_auto_intent::{
        StructuralLatchDecision, StructuralLatchFacts, structural_latch,
    };
    let model = aterm_spec::derive::native_update_structural_latch_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut frontier = vec![model.init_state()];
    let mut seen = std::collections::BTreeSet::new();
    let mut decided = 0;
    let mut earned = 0;
    let mut superseded = 0;
    let mut waited = 0;
    let mut unread_counted = 0;
    let mut unread_held = 0;
    let mut unproven_counted = 0;
    let mut unproven_moved = 0;
    assert_eq!(
        model
            .consts
            .iter()
            .find(|(name, _)| *name == "UnreadLooks")
            .map(|(_, bound)| *bound),
        Some(i64::from(
            crate::app_native::STRUCTURAL_TRIAL_UNMEASURED_LOOKS
        )),
        "the model's bound is the host's"
    );
    assert_eq!(
        model
            .consts
            .iter()
            .find(|(name, _)| *name == "UnprovenLooks")
            .map(|(_, bound)| *bound),
        Some(i64::from(
            crate::app_native::STRUCTURAL_TRIAL_UNPROVEN_LOOKS
        )),
        "the model's unproven bound is the host's"
    );
    while let Some(state) = frontier.pop() {
        if !seen.insert(format!("{state:?}")) {
            continue;
        }
        for action in &model.actions {
            frontier.extend(model.successors(action.name, &state));
        }
        if !model.action_enabled("Decide", &state) {
            continue;
        }
        decided += 1;
        let unspent = state["newer"] == 1 && state["newer_spent"] == 0;
        let unread = state["unread"] >= 1;
        let unproven = state["unread"] == 2;
        let facts = StructuralLatchFacts {
            resample_due: state["day"] == 1 && state["owed"] == 1,
            resample_owed: state["owed"] == 1,
            unspent_newer_download: (unspent && state["backoff"] == 0).then_some(12),
            newer_download_waiting: unspent && state["backoff"] == 1,
            trial_room: (!unread).then_some(state["room"] == 1),
        };
        let decision = structural_latch(facts);
        let looks = u8::try_from(state["looks"]).expect("a small count");
        // The host's count of unreadable looks (`App::look_at_structural_latch`,
        // each over a new observation): kept by a look that is not due,
        // cleared by a reading.
        let mut looks_after = if unread { looks } else { 0 };
        // The host's count of due looks over UNPROVEN reads (round eight, #80's
        // review; `App::look_over_unproven_reads`): kept by a look that is not
        // due, cleared by any proven read.
        let doubts = u8::try_from(state["doubts"]).expect("a small count");
        let mut doubts_after = if unproven { doubts } else { 0 };
        let mut moved = false;
        let mut asked = false;
        // (latched, re-sample spent, release offered, said, retire started)
        let (latched, resample_spent, newer_spent, said, superseding) = match decision {
            StructuralLatchDecision::Hold => (true, false, false, false, false),
            StructuralLatchDecision::Supersede { newer } => {
                assert_eq!(newer, 12);
                superseded += 1;
                (true, false, true, false, true)
            }
            StructuralLatchDecision::Release => {
                earned += 1;
                (false, true, false, false, false)
            }
            StructuralLatchDecision::HoldForTrial => {
                earned += 1;
                (true, true, false, true, false)
            }
            StructuralLatchDecision::Unmeasured => {
                assert!(
                    unread,
                    "a measured trial never answers Unmeasured: {facts:?}"
                );
                if unproven {
                    if doubts >= crate::app_native::STRUCTURAL_TRIAL_UNPROVEN_LOOKS {
                        // THE UNPROVEN BOUND: the re-sample moves to the next
                        // day, still owed; nothing asked, nothing held.
                        unproven_moved += 1;
                        doubts_after = 0;
                        moved = true;
                    } else {
                        unproven_counted += 1;
                        doubts_after = doubts + 1;
                        asked = true;
                    }
                    (true, false, false, false, false)
                } else if looks >= crate::app_native::STRUCTURAL_TRIAL_UNMEASURED_LOOKS {
                    // THE BOUND: held, nothing promised, said.
                    unread_held += 1;
                    looks_after = 0;
                    (true, true, false, true, false)
                } else {
                    // Counted, and another observation asked for.
                    unread_counted += 1;
                    looks_after = looks + 1;
                    asked = true;
                    (true, false, false, false, false)
                }
            }
        };
        let after = project_structural_latch(
            &model,
            StructuralLatchObservation {
                room: state["room"] == 1,
                unread,
                unproven,
                looks: looks_after,
                doubts: doubts_after,
                latched,
                pending: asked,
                day: state["day"] == 1 && !moved,
                owed: state["owed"] == 1 && !resample_spent,
                newer: state["newer"] == 1,
                newer_spent: state["newer_spent"] == 1 || newer_spent,
                said: state["said"] == 1 || said,
                escaped: state["escaped"] == 1,
                superseding,
                superseded: state["superseded"] == 1,
                refused: state["refused"] == 1,
                backoff: state["backoff"] == 1,
                // The shipping lane releases the latch only for the re-sample,
                // and only with no unspent newer release behind it (offered or
                // waiting out a refusal's retry deadline).
                booted_old: state["booted_old"] == 1 || (!latched && unspent),
            },
        );
        assert_exact_model_action(&model, "Decide", &state, &after);
        if facts.newer_download_waiting {
            waited += 1;
        }
        let mutant = buggy.successors("Decide", &state);
        // After the day, `Buggy = 1` looks at an offered release as the shipping
        // lane does (the retire starts); its defect is in the refusal.
        let refusal_scope = facts.unspent_newer_download.is_some() && state["day"] == 1;
        if asked {
            // A counted look is the same under `Buggy = 1`: its defect is at
            // the bound.
            assert_eq!(mutant.as_slice(), std::slice::from_ref(&after));
        } else if refusal_scope {
            assert_eq!(
                mutant.as_slice(),
                std::slice::from_ref(&after),
                "the round-three look is the shipping one: {state:?}"
            );
        } else if !matches!(decision, StructuralLatchDecision::Hold) {
            assert_ne!(
                mutant.as_slice(),
                std::slice::from_ref(&after),
                "the shipping decision must not be the mutant where anything was earned"
            );
            assert!(
                mutant.iter().any(|bad| buggy
                    .invariants
                    .iter()
                    .any(|invariant| !buggy.check_invariant(invariant.name, bad))),
                "the mutant's step is caught: {state:?}"
            );
        }
    }
    assert!(
        decided > 0
            && earned > 0
            && superseded > 0
            && waited > 0
            && unread_counted > 0
            && unread_held > 0
            && unproven_counted > 0
            && unproven_moved > 0,
        "the sweep decided {decided}, earned {earned}, superseded {superseded}, waited \
         {waited}, counted {unread_counted} unreadable looks and held {unread_held} at the \
         bound, counted {unproven_counted} unproven looks and moved {unproven_moved} a day"
    );
    // A release waiting out a refusal's retry deadline outranks the day (the
    // older build is not launched while a newer one is on its way), whatever
    // the trial; a promise the trial cannot afford is withdrawn all the same.
    for (trial_room, owed, expected) in [
        (Some(true), true, StructuralLatchDecision::Hold),
        (None, true, StructuralLatchDecision::Hold),
        (Some(false), true, StructuralLatchDecision::HoldForTrial),
        (Some(false), false, StructuralLatchDecision::Hold),
    ] {
        assert_eq!(
            structural_latch(StructuralLatchFacts {
                resample_due: owed,
                resample_owed: owed,
                unspent_newer_download: None,
                newer_download_waiting: true,
                trial_room,
            }),
            expected,
            "{trial_room:?} owed={owed}"
        );
    }
    // A newer release supersedes whatever the trial says (room, no room, or
    // unmeasured): it launches nothing.
    for trial_room in [Some(true), Some(false), None] {
        assert_eq!(
            structural_latch(StructuralLatchFacts {
                resample_due: true,
                resample_owed: true,
                unspent_newer_download: Some(12),
                newer_download_waiting: false,
                trial_room,
            }),
            StructuralLatchDecision::Supersede { newer: 12 },
            "{trial_room:?}"
        );
    }
    // An unmeasured trial spends nothing on a due re-sample…
    assert_eq!(
        structural_latch(StructuralLatchFacts {
            resample_due: true,
            resample_owed: true,
            unspent_newer_download: None,
            newer_download_waiting: false,
            trial_room: None,
        }),
        StructuralLatchDecision::Unmeasured
    );
    // …and withdraws no promise either: only a count that rules the re-sample
    // out does.
    assert_eq!(
        structural_latch(StructuralLatchFacts {
            resample_due: false,
            resample_owed: true,
            unspent_newer_download: None,
            newer_download_waiting: false,
            trial_room: None,
        }),
        StructuralLatchDecision::Hold
    );
}

/// The shipping arc, projected step by step: the real `App` converges on the
/// installed activation of build 11 through the completion lane, and every
/// step after is projected off the real latch, verdict, retire and notice and
/// checked against the model's own step:
///
/// * the first count read after it (`TrialHasRoom`/`TrialIsSpent`, `Decide`);
/// * a re-publish of 11 offered (`ArmSameBuild`);
/// * a verified download of 12 reconciled (`NewerArrives`, `Decide`: the
///   retire of 11 starts);
/// * the retire landing (`Supersede`: 12 staged and armed, 11 never launched)
///   or refused, about the disk or the moment (`SupersedeRefused`, `Decide`:
///   the latch stands, said, and the release is given back to wait out its
///   retry deadline);
/// * after a refusal, the day passing (`DayPasses`, `Decide`: the waiting
///   release outranks it), the retry coming due (`RetryDue`, `Decide`: the
///   retire starts again) and landing (`Supersede`).
///
/// Run with room in the boot trial and with the trial one launch from its
/// revert, where the first count withdraws the promised re-sample and a newer
/// release supersedes all the same. The day's one re-sample with nothing newer
/// on disk (`DayPasses`, `Decide`, `AttemptFails`) is its own arc below.
///
/// Each anchored shipping function is entered on the way (the `#[refines]`
/// probes), so the bind is about the code that runs.
#[test]
fn real_structural_latch_arc_is_the_model_s_arc() {
    use crate::app_native::{
        ActivationSupersedeCompletion, HandoffFailureLane, PhysicalFailureShape,
    };
    use crate::native_updater_service::{ApplyAttemptTicket, InstalledUpdate};
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let model = aterm_spec::derive::native_update_structural_latch_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window must open"
    );

    // How the retire answers: it lands, or it is refused about the disk's
    // shape, or about the moment.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Answer {
        Lands,
        RefusedByTheDisk,
        RefusedForNow,
    }

    for room in [true, false] {
        for answer in [
            Answer::Lands,
            Answer::RefusedByTheDisk,
            Answer::RefusedForNow,
        ] {
            let launches = if room {
                0
            } else {
                crate::app_native::BOOT_TRIAL_LAUNCH_LIMIT - 1
            };
            let bundle = |build: u64| InstalledUpdate {
                build,
                commit: COMMIT.to_string(),
                version: None,
                receipt_build: None,
                receipt_dmg_sha256: None,
                trial_launches: if build == 11 { launches } else { 0 },
            };
            let facts = |sequence: u64, installed: u64, newer: Option<u64>| {
                crate::app_native::NativeUpdateReconcileFacts {
                    _ticket: crate::app_native::NativeUpdateReconcileTicket::for_test(sequence),
                    observation_sequence: sequence,
                    observed_at: std::time::Instant::now(),
                    durable: Some(DurableUpdateStatus {
                        staged_dmg_sha256: newer.map(|_| "cd".repeat(32)),
                        ..status(newer, 0)
                    }),
                    installed: Some(bundle(installed)),
                    installed_unproven: false,
                }
            };
            let mut app = crate::App::headless_for_test();
            app.native_updater_service = NativeUpdaterService::new(10, "1.0.10", true);
            let _ = app.reconcile_native_update_facts(facts(1, 11, None));
            let activation = crate::native_updater_service::installed_activation_digest(11, COMMIT);
            let fail = |app: &mut crate::App| {
                let ticket = ApplyAttemptTicket::for_test(11, COMMIT, &activation);
                ticket.make_current_apply_for_test(&mut app.native_updater_service);
                let _ = app.abort_reaped_native_apply_before_reconcile(
                    &ticket,
                    "overlap handoff failed safely: handoff proof ended AdoptionMismatch"
                        .to_string(),
                    HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
                );
            };
            fail(&mut app);
            fail(&mut app);
            // Everything the environment drove, and the lane's own looks, are
            // the test's bookkeeping; everything else is read off the shipping
            // state. `refused` is the environment's answer; the refusal being
            // said is read off `update status` at its step (the model's `said`
            // is a record, so it stays said).
            let observe =
                |app: &crate::App, day: bool, newer: bool, escaped: bool, refused: bool| {
                    let verdict = app
                        .auto_apply_structural_verdict
                        .expect("the converged lane stands on a structural verdict");
                    let superseded = app
                        .native_updater_service
                        .snapshot()
                        .staged
                        .as_ref()
                        .is_some_and(|stage| stage.build == 12 && !stage.is_installed_activation());
                    project_structural_latch(
                        &model,
                        StructuralLatchObservation {
                            room,
                            unread: false,
                            unproven: app.native_installed_trial_unproven,
                            looks: verdict.unmeasured_looks,
                            doubts: verdict.unproven_looks,
                            latched: app.auto_apply_manual_only.is_some(),
                            pending: false,
                            day,
                            owed: verdict.resample_at.is_some(),
                            newer,
                            newer_spent: verdict.newer_spent >= 12,
                            // A hold says the stranded notice again; a refused retire
                            // says it in `update status` (checked at its step). With
                            // room and no refusal nothing on this arc is said.
                            said: (!room && app.auto_apply_stranded_announced == Some((11, false)))
                                || refused,
                            escaped,
                            superseding: app.native_activation_supersede.is_some(),
                            superseded,
                            refused,
                            backoff: verdict.newer_retry.is_some_and(|retry| {
                                retry.newer == 12 && std::time::Instant::now() < retry.at
                            }),
                            booted_old: false,
                        },
                    )
                };
            let arc = format!("room={room} {answer:?}");

            // Converged, and no count read yet: the attempt that converged just
            // returned, and its launch may still be given back.
            assert!(
                app.native_installed_trial.is_none(),
                "{arc}: a returned attempt leaves the count unmeasured"
            );
            // The first count read after it — the model's `TrialHasRoom` /
            // `TrialIsSpent`, a look like any other observation — then its Decide.
            let mut counted = model.init_state();
            assert!(model.fire(
                if room { "TrialHasRoom" } else { "TrialIsSpent" },
                &mut counted
            ));
            let _ = app.reconcile_native_update_facts(facts(2, 11, None));
            let converged = observe(&app, false, false, false, false);
            assert_exact_model_action(&model, "Decide", &counted, &converged);
            if !room {
                // Negative control: the promise left standing (a retry on the
                // notice for a day) is caught.
                let promise = buggy.successors("Decide", &counted)[0].clone();
                assert_ne!(promise, converged);
                assert!(!buggy.check_invariant("NoRetryPromisedPastTheTrial", &promise));
            }

            // ArmSameBuild: build 11 under another digest is offered, and stays out.
            let escaped = app.arm_native_auto_apply(11, &"ef".repeat(32));
            let after = observe(&app, false, false, escaped, false);
            assert_exact_model_action(&model, "ArmSameBuild", &converged, &after);

            // NewerArrives, then the reconcile's look: Decide starts the retire.
            let arrived = model.successors("NewerArrives", &after)[0].clone();
            let _ = app.reconcile_native_update_facts(facts(3, 11, Some(12)));
            let decided = observe(&app, false, true, false, false);
            assert_exact_model_action(&model, "Decide", &arrived, &decided);
            assert_eq!(
                app.native_activation_supersede.as_ref().map(|job| (
                    job.current_build,
                    job.activation,
                    job.newer
                )),
                Some((10, 11, 12)),
                "{arc}: the retire of 11's activation for 12 is under way"
            );
            assert!(
                app.auto_apply_manual_only.is_some() && app.auto_apply_intent.is_none(),
                "{arc}: the latch holds while it runs, and nothing arms 11"
            );
            // Negative control: looking before the day, one more attempt at 11
            // (room) or 12 held for the Version menu (no room); caught.
            let mutant = buggy.successors("Decide", &arrived)[0].clone();
            assert_ne!(
                mutant, decided,
                "{arc}: the shipping look is not the mutant"
            );
            assert!(
                buggy
                    .invariants
                    .iter()
                    .any(|invariant| !buggy.check_invariant(invariant.name, &mutant)),
                "{arc}: the mutant's look is caught"
            );
            let job = app
                .native_activation_supersede
                .clone()
                .expect("the retire in flight");
            let land = |app: &mut crate::App,
                        job: crate::app_native::ActivationSupersedeJob,
                        sequence: u64| {
                app.finish_activation_supersede(ActivationSupersedeCompletion {
                    job,
                    outcome: Ok(aterm_update::SupersededActivation {
                        activation_build: 11,
                        restored_build: 10,
                        newer_build: 12,
                        trial_launches: launches,
                        trial_disarm_deferred: None,
                    }),
                    facts: Some(facts(sequence, 10, Some(12))),
                });
            };

            if answer == Answer::Lands {
                // Supersede: the retire answered (10 back at the install path,
                // 11's trial disarmed), and the disk read after it names the
                // running build installed and 12 staged.
                let landed = model.successors("Supersede", &decided)[0].clone();
                land(&mut app, job, 4);
                let after_retire = observe(&app, false, true, false, false);
                assert_exact_model_action(&model, "Supersede", &decided, &after_retire);
                assert_eq!(after_retire, landed);
                assert_eq!(
                    app.auto_apply_intent.map(|intent| intent.build),
                    Some(12),
                    "{arc}: the lane arms the newer build"
                );
                assert!(
                    !model.action_enabled("AttemptFails", &after_retire),
                    "{arc}: and the model has no attempt at 11 left"
                );
                continue;
            }

            // Supersede refused: the latch stands, the refusal is said, the
            // release is given back to wait out its retry deadline, and the
            // observation read after it is the lane's next look.
            let refused = model.successors("SupersedeRefused", &decided)[0].clone();
            let relooked = model.successors("Decide", &refused)[0].clone();
            let by_the_disk = answer == Answer::RefusedByTheDisk;
            app.finish_activation_supersede(ActivationSupersedeCompletion {
                job,
                outcome: Err(if by_the_disk {
                    aterm_update::SupersedeRefusal::Settled(
                        "the installed build 11 stays in place: no boot trial is armed for it"
                            .to_string(),
                    )
                } else {
                    aterm_update::SupersedeRefusal::ForNow(
                        "the installed build 11 stays in place: the staged bundle does not \
                         verify: the apply budget ran out"
                            .to_string(),
                    )
                }),
                facts: Some(facts(4, 11, Some(12))),
            });
            assert!(
                aterm_update::apply_lane_report(10).is_some_and(|report| {
                    report.last_refusal.contains("could not take its place")
                        && report.last_refusal.contains("tries again")
                        && report.last_refusal.contains("the Version menu moves it") == by_the_disk
                }),
                "{arc}: the refusal is said in `update status`, with the retry — and the press \
                 only when the disk refused"
            );
            let held = observe(&app, false, true, false, true);
            assert_eq!(
                held, relooked,
                "{arc}: refused, said, given back, and looked at again"
            );
            assert!(
                app.native_activation_supersede.is_none()
                    && app
                        .auto_apply_manual_only
                        .is_some_and(|latch| latch.activation),
                "{arc}: the activation latch stands"
            );
            // Negative control: a refusal that spends the release (never offered
            // again) is caught at the look after it.
            let spent = buggy.successors("SupersedeRefused", &decided)[0].clone();
            assert_ne!(
                spent, refused,
                "{arc}: the shipping refusal gives the release back"
            );
            let stranded = buggy.successors("Decide", &spent)[0].clone();
            assert!(
                !buggy.check_invariant("ANewerReleaseClearsTheLatch", &stranded),
                "{arc}: the spent release is caught"
            );
            // Not before its retry deadline.
            let _ = app.reconcile_native_update_facts(facts(5, 11, Some(12)));
            assert!(app.native_activation_supersede.is_none(), "{arc}");

            // The day passes and the lane looks: the waiting release outranks it.
            let dawn = model.successors("DayPasses", &relooked)[0].clone();
            let dawn_look = model.successors("Decide", &dawn)[0].clone();
            if room {
                let latch = app.auto_apply_manual_only.expect("still latched");
                let verdict = app.auto_apply_structural_verdict.expect("the verdict");
                let past = std::time::Instant::now() - std::time::Duration::from_secs(1);
                app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    retry_at: Some(past),
                    ..latch
                });
                app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
                    resample_at: Some(past),
                    ..verdict
                });
                assert!(
                    !app.lapse_expired_auto_apply_manual_only(),
                    "{arc}: the day releases nothing while a newer release waits"
                );
                assert_eq!(
                    app.auto_apply_manual_only.and_then(|latch| latch.retry_at),
                    app.structural_supersede_retry_at(),
                    "{arc}: the day's deadline moves to the retry's, so the fold does not spin"
                );
            } else {
                assert!(!app.lapse_expired_auto_apply_manual_only());
                let _ = app.reconcile_native_update_facts(facts(6, 11, Some(12)));
            }
            let outranked = observe(&app, true, true, false, true);
            assert_exact_model_action(&model, "Decide", &dawn, &outranked);
            assert_eq!(outranked, dawn_look);

            // The retry comes due: the lapse is the lane's look, and the retire
            // starts again.
            let due = model.successors("RetryDue", &outranked)[0].clone();
            let verdict = app.auto_apply_structural_verdict.expect("the verdict");
            let retry = verdict.newer_retry.expect("{arc}: a retry waits");
            app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
                newer_retry: Some(crate::SupersedeRetry {
                    at: std::time::Instant::now() - std::time::Duration::from_secs(1),
                    ..retry
                }),
                ..verdict
            });
            assert!(!app.lapse_expired_auto_apply_manual_only());
            let retried = observe(&app, true, true, false, true);
            assert_exact_model_action(&model, "Decide", &due, &retried);
            let job = app
                .native_activation_supersede
                .clone()
                .expect("the retire of 11 for 12 is under way again");
            // …and this time it lands.
            let landed = model.successors("Supersede", &retried)[0].clone();
            land(&mut app, job, 7);
            let after_retire = observe(&app, true, true, false, true);
            assert_exact_model_action(&model, "Supersede", &retried, &after_retire);
            assert_eq!(after_retire, landed);
            assert_eq!(
                app.auto_apply_intent.map(|intent| intent.build),
                Some(12),
                "{arc}: the retry lands the newer build"
            );
        }
    }

    // The day's one re-sample, with nothing newer on disk: the lapse is the
    // lane's look (`DayPasses`, `Decide`: released for one attempt at 11), and
    // the re-sample failing the same way re-latches with nothing owed
    // (`AttemptFails`).
    {
        let bundle = InstalledUpdate {
            build: 11,
            commit: COMMIT.to_string(),
            version: None,
            receipt_build: None,
            receipt_dmg_sha256: None,
            trial_launches: 0,
        };
        let facts = |sequence: u64| crate::app_native::NativeUpdateReconcileFacts {
            _ticket: crate::app_native::NativeUpdateReconcileTicket::for_test(sequence),
            observation_sequence: sequence,
            observed_at: std::time::Instant::now(),
            durable: Some(status(None, 0)),
            installed: Some(bundle.clone()),
            installed_unproven: false,
        };
        let mut app = crate::App::headless_for_test();
        app.native_updater_service = NativeUpdaterService::new(10, "1.0.10", true);
        let _ = app.reconcile_native_update_facts(facts(1));
        let activation = crate::native_updater_service::installed_activation_digest(11, COMMIT);
        let fail = |app: &mut crate::App| {
            let ticket = ApplyAttemptTicket::for_test(11, COMMIT, &activation);
            ticket.make_current_apply_for_test(&mut app.native_updater_service);
            let _ = app.abort_reaped_native_apply_before_reconcile(
                &ticket,
                "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
                HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
            );
        };
        fail(&mut app);
        fail(&mut app);
        let observe = |app: &crate::App, day: bool| {
            let verdict = app.auto_apply_structural_verdict.expect("the verdict");
            project_structural_latch(
                &model,
                StructuralLatchObservation {
                    room: true,
                    unread: false,
                    unproven: app.native_installed_trial_unproven,
                    looks: verdict.unmeasured_looks,
                    doubts: verdict.unproven_looks,
                    latched: app.auto_apply_manual_only.is_some(),
                    pending: false,
                    day,
                    owed: verdict.resample_at.is_some(),
                    newer: false,
                    newer_spent: false,
                    said: false,
                    escaped: false,
                    superseding: false,
                    superseded: false,
                    refused: false,
                    backoff: false,
                    booted_old: false,
                },
            )
        };
        let mut counted = model.init_state();
        assert!(model.fire("TrialHasRoom", &mut counted));
        let _ = app.reconcile_native_update_facts(facts(2));
        let converged = observe(&app, false);
        assert_exact_model_action(&model, "Decide", &counted, &converged);
        let latch = app.auto_apply_manual_only.expect("latched");
        let verdict = app.auto_apply_structural_verdict.expect("the verdict");
        let past = std::time::Instant::now() - std::time::Duration::from_secs(1);
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            retry_at: Some(past),
            ..latch
        });
        app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
            resample_at: Some(past),
            ..verdict
        });
        let dawn = model.successors("DayPasses", &converged)[0].clone();
        assert!(app.lapse_expired_auto_apply_manual_only());
        let resampled = observe(&app, true);
        assert_exact_model_action(&model, "Decide", &dawn, &resampled);
        // The one re-sample fails the same way: re-latched, nothing owed.
        assert!(app.arm_native_auto_apply(11, &activation));
        fail(&mut app);
        let done = observe(&app, true);
        assert_exact_model_action(&model, "AttemptFails", &resampled, &done);
        assert_eq!(
            app.auto_apply_manual_only.map(|manual| manual.retry_at),
            Some(None),
            "after its one re-sample the verdict owes nothing"
        );
    }

    // Every anchored shipping function of the machine ran.
    let entered = aterm_spec::xref::entered_anchor_ids();
    aterm_spec::xref::disarm_entered_anchors();
    let anchors: Vec<_> = aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "NativeUpdateStructuralLatch")
        .collect();
    assert_eq!(
        anchors.len(),
        6,
        "Decide (the decision and the look that applies it), AttemptFails, ArmSameBuild, \
         Supersede and SupersedeRefused are anchored"
    );
    for anchor in anchors {
        assert!(
            !anchor.entry_id.is_empty() && entered.contains(anchor.entry_id),
            "{} was not entered by the arc: {entered:?}",
            anchor.entry_id
        );
    }
}

/// A latch whose activation leaves the installed facts is decided within a
/// bound, projected step by step onto the model. The real `App` converges on
/// the installed activation of build 11 and reads its first count
/// (`TrialHasRoom`, `Decide`); then the channel's floor rises above 11 and the
/// facts worker drops it from every observation (`installed: None`,
/// `TrialUnreadable`, `Decide`), so its boot trial is never measured again. The
/// day's re-sample comes due (`DayPasses`) and every look answers `Unmeasured`.
/// Each due look over a new observation counts (`Decide`, `looks`), and at
/// `STRUCTURAL_TRIAL_UNMEASURED_LOOKS` the re-sample is held, the latch loses
/// its deadline and the hold is said (`Decide`'s bound arm). A due look with no
/// new observation behind it (the facts worker saturated, or no observation
/// could be asked for) counts nothing. Negative controls: the `Buggy = 1` look
/// at the bound, which keeps the promise, is caught; and a reading that comes
/// back (`TrialReadAgain`) clears the count and, with room, releases the
/// re-sample.
#[test]
fn a_latch_whose_activation_leaves_the_installed_facts_is_decided_within_a_bound() {
    use crate::app_native::{
        HandoffFailureLane, PhysicalFailureShape, STRUCTURAL_TRIAL_UNMEASURED_LOOKS,
    };
    use crate::native_updater_service::{ApplyAttemptTicket, InstalledUpdate};
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let model = aterm_spec::derive::native_update_structural_latch_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for measured_again in [false, true] {
        let bundle = InstalledUpdate {
            build: 11,
            commit: COMMIT.to_string(),
            version: None,
            receipt_build: None,
            receipt_dmg_sha256: None,
            trial_launches: 0,
        };
        let facts = |sequence: u64, installed: Option<InstalledUpdate>| {
            crate::app_native::NativeUpdateReconcileFacts {
                _ticket: crate::app_native::NativeUpdateReconcileTicket::for_test(sequence),
                observation_sequence: sequence,
                observed_at: std::time::Instant::now(),
                durable: Some(status(None, 0)),
                installed,
                installed_unproven: false,
            }
        };
        let mut app = crate::App::headless_for_test();
        app.native_updater_service = NativeUpdaterService::new(10, "1.0.10", true);
        let _ = app.reconcile_native_update_facts(facts(1, Some(bundle.clone())));
        let activation = crate::native_updater_service::installed_activation_digest(11, COMMIT);
        for _ in 0..2 {
            let ticket = ApplyAttemptTicket::for_test(11, COMMIT, &activation);
            ticket.make_current_apply_for_test(&mut app.native_updater_service);
            let _ = app.abort_reaped_native_apply_before_reconcile(
                &ticket,
                "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
                HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
            );
        }
        let latch = app.auto_apply_manual_only.expect("converged on 11");
        assert!(latch.activation && latch.build == 11);
        let case = format!("measured_again={measured_again}");
        // `day` and `pending` are the test's bookkeeping (the clock it moves,
        // the look a counted look asked for); the rest is read off the latch,
        // the verdict, the installed count and the notice.
        let observe = |app: &crate::App, day: bool, pending: bool| {
            let verdict = app.auto_apply_structural_verdict.expect("the verdict");
            project_structural_latch(
                &model,
                StructuralLatchObservation {
                    room: true,
                    unread: app
                        .native_installed_trial
                        .is_none_or(|(build, _)| build != 11),
                    unproven: app.native_installed_trial_unproven,
                    looks: verdict.unmeasured_looks,
                    doubts: verdict.unproven_looks,
                    latched: app.auto_apply_manual_only.is_some(),
                    pending,
                    day,
                    owed: verdict.resample_at.is_some(),
                    newer: false,
                    newer_spent: false,
                    said: app.auto_apply_stranded_announced == Some((11, false)),
                    escaped: false,
                    superseding: false,
                    superseded: false,
                    refused: false,
                    backoff: false,
                    booted_old: false,
                },
            )
        };
        let past = || std::time::Instant::now() - std::time::Duration::from_secs(1);
        let due_look = |app: &mut crate::App| {
            let latch = app.auto_apply_manual_only.expect("still latched");
            app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                retry_at: Some(past()),
                ..latch
            });
            app.lapse_expired_auto_apply_manual_only()
        };

        // The first count, with room.
        let mut modeled = model.init_state();
        assert!(model.fire("TrialHasRoom", &mut modeled));
        let _ = app.reconcile_native_update_facts(facts(2, Some(bundle.clone())));
        let real = observe(&app, false, false);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        modeled = real;

        // The floor rises above 11: the facts no longer name it.
        assert!(model.fire("TrialUnreadable", &mut modeled));
        let mut sequence = 3;
        let _ = app.reconcile_native_update_facts(facts(sequence, None));
        assert!(app.native_installed_trial.is_none(), "{case}: unmeasured");
        let real = observe(&app, false, false);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        modeled = real;

        // The day passes.
        assert!(model.fire("DayPasses", &mut modeled));
        let verdict = app.auto_apply_structural_verdict.expect("the verdict");
        app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
            resample_at: Some(past()),
            ..verdict
        });
        let bound = STRUCTURAL_TRIAL_UNMEASURED_LOOKS;
        for look in 1..=bound {
            // A due look over the observation read since the last one counts,
            // and asks for the next.
            assert!(!due_look(&mut app), "{case}: look {look}");
            let real = observe(&app, true, true);
            assert_exact_model_action(&model, "Decide", &modeled, &real);
            assert_eq!(real["looks"], i64::from(look), "{case}");
            modeled = real;
            // A due look with nothing new behind it (the facts worker had no
            // observation for it) counts nothing: a stutter.
            assert!(!due_look(&mut app), "{case}: look {look} again");
            assert_eq!(
                observe(&app, true, true),
                modeled,
                "{case}: look {look} again"
            );
            if measured_again && look == 2 {
                break;
            }
            // The observation it asked for: still no count. The reconcile's
            // own look is not a due one, and counts nothing either.
            sequence += 1;
            let _ = app.reconcile_native_update_facts(facts(sequence, None));
            assert_eq!(
                observe(&app, true, true),
                modeled,
                "{case}: look {look} read"
            );
        }
        if measured_again {
            // Negative control: the count comes back with room, so it is
            // cleared and the re-sample released.
            assert!(model.fire("TrialReadAgain", &mut modeled));
            sequence += 1;
            let _ = app.reconcile_native_update_facts(facts(sequence, Some(bundle.clone())));
            let released = app.auto_apply_manual_only.is_none() || due_look(&mut app);
            assert!(
                released,
                "{case}: a measured count with room releases the re-sample"
            );
            let real = observe(&app, true, false);
            assert_exact_model_action(&model, "Decide", &modeled, &real);
            assert_eq!(
                (real["latched"], real["looks"], real["owed"]),
                (0, 0, 0),
                "{case}: the count is cleared, the one re-sample taken"
            );
            continue;
        }
        // The bound: the next due look over a new observation holds, loses
        // the deadline, and says so.
        let mutant = buggy.successors("Decide", &modeled)[0].clone();
        assert!(!due_look(&mut app), "{case}: the bound holds");
        let real = observe(&app, true, false);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        let latch = app.auto_apply_manual_only.expect("the latch stands");
        let verdict = app.auto_apply_structural_verdict.expect("the verdict");
        assert_eq!(
            (latch.retry_at, verdict.resample_at),
            (None, None),
            "{case}: nothing promised"
        );
        assert_eq!(
            app.auto_apply_stranded_announced,
            Some((11, false)),
            "{case}: the hold is said"
        );
        assert!(
            aterm_update::apply_lane_report(10).is_some_and(|report| report
                .last_refusal
                .contains("could not read how many times it has started")),
            "{case}: and on `update status`"
        );
        // Negative control: the promise kept forever is not what ships, and
        // the model catches it.
        assert_ne!(mutant, real, "{case}: the shipping look is not the mutant");
        assert!(!buggy.check_invariant("NoRetryPromisedPastTheBound", &mutant));
        // The count comes back spent: the bound's hold named the Version menu,
        // whose start is now the one the trial rolls back, so the hold is said
        // again, once, in the trial's words (as the staged record says it).
        let spent = InstalledUpdate {
            trial_launches: crate::app_native::BOOT_TRIAL_LAUNCH_LIMIT - 1,
            ..bundle.clone()
        };
        sequence += 1;
        let _ = app.reconcile_native_update_facts(facts(sequence, Some(spent)));
        assert!(app.latched_activation_trial_spent(11), "{case}: spent");
        let refusal = aterm_update::apply_lane_report(10)
            .expect("the ledger reads")
            .last_refusal;
        assert!(
            refusal.contains("would roll it back")
                && refusal.contains(crate::update_words::NEWER_RELEASE_REPLACES)
                && !refusal.contains(crate::update_words::INSTALL_FROM_MENU),
            "{case}: `apply_refusal=` names no start: {refusal}"
        );
        use crate::update_words::LiveHealth as _;
        let said = app
            .messages
            .live_health()
            .map(|live| live.msg.detail.join("; "))
            .expect("the warning is up");
        assert!(
            said.contains(crate::update_words::NEWER_RELEASE_REPLACES)
                && !said.contains(crate::update_words::TRIES_AGAIN)
                && !said.contains(crate::update_words::INSTALL_FROM_MENU),
            "{case}: the notice took the new words: {said}"
        );
        assert!(
            !app.auto_apply_structural_verdict
                .expect("the verdict")
                .held_unmeasured,
            "{case}: said once"
        );
    }
}

/// A COUNT THAT COULD NOT BE CHECKED IS NOT A COUNT THAT IS GONE (round eight,
/// #80, and its review), projected step by step onto the model. The latch on build
/// 11 has room and its day's re-sample comes due on a captive-portal network: every
/// facts read finds the installed bundle but cannot PROVE it (`spctl` past its
/// ceiling — a passing refusal, `installed_unproven`), so `installed` is `None`.
///
/// The bound on a count that is not coming (finding 55) counted those reads as
/// observations: three due looks dropped the re-sample for good and the latch held
/// with no deadline, so the network's recovery released nothing (L4; #80). #80 as
/// first landed made each such look a stutter, which re-armed the latch every 75 s
/// — a deep `codesign` and an `spctl` each time — for as long as the filter stood,
/// and said nothing (L5; its review). Now an unproven read is `TrialUnproven`: each
/// due look over a new one counts on its own bound (`doubts`), and at
/// `STRUCTURAL_TRIAL_UNPROVEN_LOOKS` the re-sample moves to the next day, still owed,
/// recorded on `update status` and raised nowhere. The next day counts afresh, and
/// the first PROVEN read with room releases the re-sample.
///
/// NEGATIVE CONTROLS: the `Buggy = 1` look at the bound, which keeps the day due and
/// asks again, is not the shipping one and is caught (`UnprovenLooksAreBounded`); and
/// the same reads PROVEN to name no bundle (the yank, as
/// `a_latch_whose_activation_leaves_the_installed_facts_is_decided_within_a_bound`
/// drives it) still count to the unread bound and hold.
#[test]
fn a_latch_whose_count_could_not_be_checked_keeps_its_re_sample() {
    use crate::app_native::{
        HandoffFailureLane, PhysicalFailureShape, STRUCTURAL_TRIAL_UNMEASURED_LOOKS,
        STRUCTURAL_TRIAL_UNPROVEN_LOOKS,
    };
    use crate::native_updater_service::{ApplyAttemptTicket, InstalledUpdate};
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let model = aterm_spec::derive::native_update_structural_latch_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for unproven in [true, false] {
        let case = format!("unproven={unproven}");
        let bundle = InstalledUpdate {
            build: 11,
            commit: COMMIT.to_string(),
            version: None,
            receipt_build: None,
            receipt_dmg_sha256: None,
            trial_launches: 0,
        };
        let facts = |sequence: u64, installed: Option<InstalledUpdate>, unproven: bool| {
            crate::app_native::NativeUpdateReconcileFacts {
                _ticket: crate::app_native::NativeUpdateReconcileTicket::for_test(sequence),
                observation_sequence: sequence,
                observed_at: std::time::Instant::now(),
                durable: Some(status(None, 0)),
                installed,
                installed_unproven: unproven,
            }
        };
        let mut app = crate::App::headless_for_test();
        app.native_updater_service = NativeUpdaterService::new(10, "1.0.10", true);
        let _ = app.reconcile_native_update_facts(facts(1, Some(bundle.clone()), false));
        let activation = crate::native_updater_service::installed_activation_digest(11, COMMIT);
        for _ in 0..2 {
            let ticket = ApplyAttemptTicket::for_test(11, COMMIT, &activation);
            ticket.make_current_apply_for_test(&mut app.native_updater_service);
            let _ = app.abort_reaped_native_apply_before_reconcile(
                &ticket,
                "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
                HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
            );
        }
        let latch = app.auto_apply_manual_only.expect("converged on 11");
        assert!(latch.activation && latch.build == 11, "{case}");
        let observe = |app: &crate::App, day: bool, pending: bool| {
            let verdict = app.auto_apply_structural_verdict.expect("the verdict");
            project_structural_latch(
                &model,
                StructuralLatchObservation {
                    room: true,
                    unread: app
                        .native_installed_trial
                        .is_none_or(|(build, _)| build != 11),
                    unproven: app.native_installed_trial_unproven,
                    looks: verdict.unmeasured_looks,
                    doubts: verdict.unproven_looks,
                    latched: app.auto_apply_manual_only.is_some(),
                    pending,
                    day,
                    owed: verdict.resample_at.is_some(),
                    newer: false,
                    newer_spent: false,
                    said: app.auto_apply_stranded_announced == Some((11, false)),
                    escaped: false,
                    superseding: false,
                    superseded: false,
                    refused: false,
                    backoff: false,
                    booted_old: false,
                },
            )
        };
        let past = || std::time::Instant::now() - std::time::Duration::from_secs(1);
        let due_look = |app: &mut crate::App| {
            let latch = app.auto_apply_manual_only.expect("still latched");
            app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                retry_at: Some(past()),
                ..latch
            });
            app.lapse_expired_auto_apply_manual_only()
        };

        // The first count, with room.
        let mut modeled = model.init_state();
        assert!(model.fire("TrialHasRoom", &mut modeled));
        let _ = app.reconcile_native_update_facts(facts(2, Some(bundle.clone()), false));
        let real = observe(&app, false, false);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        modeled = real;

        // The count can no longer be read: unproven (a captive portal), or proven
        // gone (the negative control's yank).
        assert!(model.fire(
            if unproven {
                "TrialUnproven"
            } else {
                "TrialUnreadable"
            },
            &mut modeled
        ));
        let mut sequence = 3;
        let _ = app.reconcile_native_update_facts(facts(sequence, None, unproven));
        let real = observe(&app, false, false);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        modeled = real;

        // The day passes.
        let day_passes = |app: &mut crate::App, modeled: &mut State| {
            assert!(model.fire("DayPasses", modeled));
            let verdict = app.auto_apply_structural_verdict.expect("the verdict");
            app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
                resample_at: Some(past()),
                ..verdict
            });
        };
        day_passes(&mut app, &mut modeled);
        let bound = if unproven {
            STRUCTURAL_TRIAL_UNPROVEN_LOOKS
        } else {
            STRUCTURAL_TRIAL_UNMEASURED_LOOKS
        };
        for look in 1..=bound {
            // A due look over the read since the last one counts, on its own
            // bound, and asks for the next.
            assert!(!due_look(&mut app), "{case}: look {look}");
            let real = observe(&app, true, true);
            assert_exact_model_action(&model, "Decide", &modeled, &real);
            let counted = if unproven { "doubts" } else { "looks" };
            assert_eq!(real[counted], i64::from(look), "{case}: look {look}");
            modeled = real;
            // A due look with nothing new behind it counts nothing: a stutter.
            assert!(!due_look(&mut app), "{case}: look {look} again");
            assert_eq!(
                observe(&app, true, true),
                modeled,
                "{case}: look {look} again"
            );
            // The observation it asked for, no better; the reconcile's own look is
            // not a due one.
            sequence += 1;
            let _ = app.reconcile_native_update_facts(facts(sequence, None, unproven));
            assert_eq!(
                observe(&app, true, true),
                modeled,
                "{case}: look {look} read"
            );
        }
        let mutant = buggy.successors("Decide", &modeled)[0].clone();
        assert!(!due_look(&mut app), "{case}: the look at the bound");
        if !unproven {
            // NEGATIVE CONTROL: proven gone, the bound holds for good, as before.
            let real = observe(&app, true, false);
            assert_exact_model_action(&model, "Decide", &modeled, &real);
            let latch = app.auto_apply_manual_only.expect("the latch stands");
            let verdict = app.auto_apply_structural_verdict.expect("the verdict");
            assert_eq!(
                (latch.retry_at, verdict.resample_at),
                (None, None),
                "{case}"
            );
            continue;
        }
        // THE UNPROVEN BOUND: the re-sample moves to the next day — still owed, the
        // latch's deadline with it — and is recorded, raised nowhere.
        let real = observe(&app, false, false);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        let latch = app.auto_apply_manual_only.expect("the latch stands");
        let verdict = app.auto_apply_structural_verdict.expect("the verdict");
        let tomorrow = std::time::Instant::now() + crate::app_native::STRUCTURAL_RESAMPLE_AFTER;
        let a_day_out = |at: Option<std::time::Instant>| {
            at.is_some_and(|at| {
                at <= tomorrow && at + std::time::Duration::from_secs(60) >= tomorrow
            })
        };
        assert!(
            a_day_out(verdict.resample_at) && a_day_out(latch.retry_at),
            "{case}: the re-sample and the latch's deadline move a day: {verdict:?} {latch:?}"
        );
        assert_eq!(
            (verdict.unproven_looks, verdict.unmeasured_looks),
            (0, 0),
            "{case}: a fresh count for the next day; the unread count untouched"
        );
        assert!(
            aterm_update::apply_lane_report(10).is_some_and(|report| report
                .last_refusal
                .contains("tries build 11 again tomorrow")),
            "{case}: said on `update status`"
        );
        assert_eq!(
            real["said"], 0,
            "{case}: nothing asked of a person, so no hold raised"
        );
        // NEGATIVE CONTROL: the re-promise with no bound is not what shipped, and the
        // model catches it.
        assert_ne!(mutant, real, "{case}: the shipping look is not the mutant");
        assert!(!buggy.check_invariant("UnprovenLooksAreBounded", &mutant));
        modeled = real;

        // The next day: still unproven, it counts afresh.
        day_passes(&mut app, &mut modeled);
        sequence += 1;
        let _ = app.reconcile_native_update_facts(facts(sequence, None, true));
        assert!(!due_look(&mut app), "{case}: the next day's first look");
        let real = observe(&app, true, true);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        assert_eq!(real["doubts"], 1, "{case}: counted afresh");
        modeled = real;

        // The network recovers: a PROVEN read with room releases the re-sample.
        assert!(model.fire("TrialReadAgain", &mut modeled));
        sequence += 1;
        let _ = app.reconcile_native_update_facts(facts(sequence, Some(bundle.clone()), false));
        let released = app.auto_apply_manual_only.is_none() || due_look(&mut app);
        assert!(
            released,
            "{case}: a count read once the bundle is proven again releases the re-sample"
        );
        let real = observe(&app, true, false);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        assert_eq!(
            (real["latched"], real["doubts"], real["owed"]),
            (0, 0, 0),
            "{case}: the one re-sample taken"
        );
    }
}

/// The machine is LINKED, not just stated: the decision and the look that
/// applies it (whose bound on a count that cannot be read is the host's; round
/// six, finding 55), the budget's verdict arm, the latch's build-wide fold and
/// the two answers of the activation's retire carry `#[refines]` anchors
/// naming the one projection, and the eight environment steps are waived on
/// the decision they feed.
#[test]
fn structural_latch_shipping_anchors_are_linked() {
    const PROJECT: &str = "aterm_gui::native_updater_conformance::project_structural_latch";
    let mut refinements: Vec<_> = aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "NativeUpdateStructuralLatch")
        .map(|anchor| (anchor.action, anchor.rust_method, anchor.project))
        .collect();
    refinements.sort_unstable();
    assert_eq!(
        refinements,
        [
            ("ArmSameBuild", "covers", PROJECT),
            ("AttemptFails", "spend_physical_failure_budget", PROJECT),
            ("Decide", "look_at_structural_latch", PROJECT),
            ("Decide", "structural_latch", PROJECT),
            ("Supersede", "finish_activation_supersede", PROJECT),
            (
                "SupersedeRefused",
                "say_activation_supersede_refused",
                PROJECT
            ),
        ]
    );
    let mut waivers: Vec<_> = aterm_spec::xref::waivers()
        .filter(|waiver| waiver.machine == "NativeUpdateStructuralLatch")
        .map(|waiver| (waiver.action, waiver.rust_method))
        .collect();
    waivers.sort_unstable();
    assert_eq!(
        waivers,
        [
            ("DayPasses", "structural_latch"),
            ("NewerArrives", "structural_latch"),
            ("RetryDue", "structural_latch"),
            ("TrialHasRoom", "structural_latch"),
            ("TrialIsSpent", "structural_latch"),
            ("TrialReadAgain", "structural_latch"),
            ("TrialUnproven", "structural_latch"),
            ("TrialUnreadable", "structural_latch"),
        ]
    );
    let model = aterm_spec::derive::native_update_structural_latch_model();
    let mut covered: Vec<_> = refinements
        .iter()
        .map(|(action, _, _)| *action)
        .chain(waivers.iter().map(|(action, _)| *action))
        .collect();
    covered.sort_unstable();
    covered.dedup();
    let mut actions: Vec<_> = model.actions.iter().map(|action| action.name).collect();
    actions.sort_unstable();
    assert_eq!(covered, actions, "every action is anchored or waived");
}

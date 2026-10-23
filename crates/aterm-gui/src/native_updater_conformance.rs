// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the native updater: the shipping service, predicates and `App` arcs, each bound to its `aterm_spec::derive` model.

#![cfg(test)]

use aterm_models_misc::native_update_disk_transaction_model;
#[cfg(unix)]
use aterm_models_update::native_update_auto_intent_model;
use aterm_models_update::{
    native_update_admission_model, native_update_apply_ladder_model,
    native_update_attempt_identity_model, native_update_hidden_output_quiet_model,
    native_updater_model,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

use crate::native_update_admission::{
    AdmissionBlock, AdmissionDecision, AdmissionFacts, ApplyLane, classify,
};
use crate::native_update_auto_intent::{ApplyPhase, PollDecision, PollFacts, poll};
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
        installable: Some(true),
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

    let running = service.model_state();
    assert_eq!(service.request_check(), CheckStart::Joined(check));
    assert_eq!(service.model_state(), running);
    assert!(service.last_transitions().is_empty());

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
/// first-present boundary.
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
    let proved = model.successors("ProveInstalledHealth", &presented)[0].clone();
    assert_exact_model_action(&model, "ProveInstalledHealth", &presented, &proved);
}

fn admission_before(model: &Model, facts: AdmissionFacts) -> State {
    let mut state = model.init_state();
    if facts.image_kept {
        state = model.successors("KeepImage", &state)[0].clone();
    }
    if !facts.staged_verified {
        state = model.successors("InvalidateArtifact", &state)[0].clone();
    }
    if facts.live_ptys > 0 || facts.foreground_jobs > 0 {
        state = model.successors("ObserveForegroundJob", &state)[0].clone();
    }
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
        AdmissionDecision::Block(AdmissionBlock::ForegroundProbeUnknown)
        | AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless) => {
            "BlockForegroundWithoutSeamless"
        }
        AdmissionDecision::Block(AdmissionBlock::KeptImageNeedsTerminal) => {
            "BlockKeptImageWithoutTerminal"
        }
    }
}

/// Exhaust the bounded admission fact matrix against the genuine shipping
/// classifier.
#[test]
fn real_update_admission_classifier_conforms_for_every_bounded_fact_combination() {
    let model = native_update_admission_model();
    for (staged_verified, seamless_capable, image_kept) in
        [false, true].into_iter().flat_map(|verified| {
            [false, true].into_iter().flat_map(move |seamless| {
                [false, true]
                    .into_iter()
                    .map(move |kept| (verified, seamless, kept))
            })
        })
    {
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
                            image_kept,
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

    let facts = AdmissionFacts {
        staged_verified: true,
        seamless_capable: true,
        native_state_certified: true,
        live_ptys: 1,
        foreground_jobs: 1,
        unknown_foregrounds: 0,
        image_kept: false,
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
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let v053 = buggy.successors("BlockForegroundDespiteSeamless", &before)[0].clone();
    assert_eq!(
        (v053["phase"], v053["decision"], v053["retry_eligible"]),
        (3, 3, 1)
    );
    assert_eq!(admits(&model, &before, &v053), None);
    assert!(!buggy.check_invariant("ForegroundJobsDoNotBlockSeamless", &v053));

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

    let facts = AdmissionFacts {
        live_ptys: 0,
        foreground_jobs: 0,
        image_kept: true,
        ..facts
    };
    assert_eq!(
        classify(facts),
        AdmissionDecision::Block(AdmissionBlock::KeptImageNeedsTerminal)
    );
    let before = admission_before(&model, facts);
    let exec_in_place = buggy.successors("ClassifyColdOverKeptImage", &before)[0].clone();
    assert_eq!(admits(&model, &before, &exec_in_place), None);
    assert!(!buggy.check_invariant("KeptImageNeverReexecsCold", &exec_in_place));
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

/// The real park gate, bound to the model step it implements.
#[cfg(unix)]
#[test]
fn real_park_gate_admits_exactly_the_model_s_reader_park() {
    use crate::app_update_handoff::{
        PRELAUNCH_LAND_MAX_WAITS, ParkGate, ParkGateFacts, handoff_masters_closed,
        handoff_masters_have_activity, prelaunch_hold_cap, prelaunch_park_admitted,
    };
    use crate::native_update_auto_intent::ActivityFacts;
    let model = native_update_auto_intent_model();
    let mut state = model.init_state();
    for action in ["StageWakeIdle", "QuietElapsed", "Attempt"] {
        let next = model.successors(action, &state)[0].clone();
        assert_exact_model_action(&model, action, &state, &next);
        state = next;
    }
    assert_eq!(state["phase"], 3);
    assert_eq!(state["parked"], 0);

    let calm = |quiet: bool| ActivityFacts {
        quiet,
        hands_off_keys: true,
        output_quiet: true,
        focused: true,
        consent_warmup: false,
        harness_restored_pending: false,
    };
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
}

/// The ladder, bound to its model (docs/DESIGN-auto-apply-ladder-2026-09-21.md).
#[test]
fn real_apply_ladder_admits_exactly_the_model_s_park() {
    use crate::native_update_auto_intent::{
        ActivityFacts, LANDS_WITHIN, PREFER_IDLE_WINDOW, PREFER_OUTPUT_GAP_WINDOW, apply_phase,
        automatic_park_refusal,
    };
    let model = native_update_apply_ladder_model();

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

    let mut warming = model.init_state();
    for action in ["Advance", "Advance", "Advance", "WarmupStarts"] {
        assert!(model.fire(action, &mut warming), "{action}");
    }
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let over_dialog = buggy.successors("ParkWithoutTheRule", &warming)[0].clone();
    assert!(!buggy.check_invariant("ParkedOnlyWhenTheLadderAdmits", &over_dialog));
}

/// The `Lapse` and `ParkMissed` model steps, bound to shipping code.
#[cfg(unix)]
#[test]
fn real_lapse_keeps_the_anchor_and_a_park_miss_stays_in_the_activity_lane() {
    use crate::app_native::HandoffFailureLane;
    use crate::app_update_handoff::{
        PRELAUNCH_MAX_PARK_MISSES, ParkMissDisposition, park_miss_disposition,
    };
    use crate::native_update_auto_intent::LANDS_WITHIN;
    let model = native_update_apply_ladder_model();

    let mut keys_only = model.init_state();
    for _ in 0..2 {
        keys_only = model.successors("Advance", &keys_only)[0].clone();
    }
    let latched = model.successors("PhysicalFailure", &keys_only)[0].clone();
    assert_exact_model_action(&model, "PhysicalFailure", &keys_only, &latched);
    let aged = model.successors("Advance", &latched)[0].clone();
    assert_exact_model_action(&model, "Advance", &latched, &aged);
    let due = model.successors("Due", &aged)[0].clone();
    assert_exact_model_action(&model, "Due", &aged, &due);
    let lapsed = model.successors("Lapse", &due)[0].clone();
    assert_exact_model_action(&model, "Lapse", &due, &lapsed);
    assert_eq!(lapsed["phase"], 3);

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

/// The `BundleSwap` model step: when the failed candidate had already boot-applied the
/// bundle, the next reconcile retires the download for the installed-bundle activation of the
/// same update and keeps the latch and its deadline.
#[test]
fn real_bundle_swap_keeps_the_latch_as_the_model_s_bundle_swap() {
    use crate::app_native::{
        HandoffFailureLane, NativeUpdateReconcileFacts, NativeUpdateReconcileTicket,
        PhysicalFailureShape,
    };
    use crate::native_updater_service::{ApplyAttemptTicket, InstalledUpdate};
    let model = native_update_apply_ladder_model();

    let mut keys_only = model.init_state();
    for _ in 0..2 {
        keys_only = model.successors("Advance", &keys_only)[0].clone();
    }
    let latched = model.successors("PhysicalFailure", &keys_only)[0].clone();

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
        installable: None,
    });
    assert!(
        app.native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .is_some_and(|staged| staged.build == build && staged.is_installed_activation()),
        "PRECONDITION: the download retired for the activation of the same build"
    );

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

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let cleared = buggy.successors("BundleSwapClearsLatch", &latched)[0].clone();
    assert!(!buggy.check_invariant("NoEarlyRelease", &cleared));
    assert_ne!(
        cleared, projected,
        "the shipping retire arm must not be the v0.87–v0.91 mutant"
    );
}

/// A capture failure is sorted into a miss or a refusal by its type, and a refusal never
/// reaches the activity lane.
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

    let mut land = model.init_state();
    for _ in 0..3 {
        land = model.successors("Advance", &land)[0].clone();
    }
    let quiet = model.successors("Quiet", &land)[0].clone();
    let refusing = model.successors("DeskRefuses", &quiet)[0].clone();
    assert_exact_model_action(&model, "DeskRefuses", &quiet, &refusing);

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
        let answered = model.successors("CaptureRefused", &refusing)[0].clone();
        assert_exact_model_action(&model, "CaptureRefused", &refusing, &answered);
        assert_eq!(answered["quiet"], refusing["quiet"]);
        assert_eq!(answered["refusals"], 0);

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
/// combining mark, and a parser left inside an unterminated OSC.
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

/// Bind the hidden-output model to the genuine monotonic admission predicate,
/// future-deadline constructor and auto-intent reducer.
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

/// The observable state of a structural latch, as the Tier-1 bind of `NativeUpdateStructuralLatch` reads it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StructuralLatchObservation {
    pub(crate) room: bool,
    /// The installed facts no longer report the latched build, so no count can be read (`App::native_installed_trial` is not the latch's); `room` is then no reading.
    pub(crate) unread: bool,
    /// …and the latest read could not prove the installed bundle at all (`App::native_installed_trial_unproven`): the model's `unread = 2`.
    pub(crate) unproven: bool,
    /// Due looks in a row that found no count (`AutoApplyStructuralVerdict::unmeasured_looks`).
    pub(crate) looks: u8,
    /// Due looks in a row over an unproven read (`AutoApplyStructuralVerdict::unproven_looks`; the model's `doubts`).
    pub(crate) doubts: u8,
    pub(crate) latched: bool,
    pub(crate) pending: bool,
    pub(crate) day: bool,
    pub(crate) owed: bool,
    pub(crate) newer: bool,
    pub(crate) newer_spent: bool,
    pub(crate) said: bool,
    pub(crate) escaped: bool,
    /// A retire of the latched activation is in flight (`App::native_activation_supersede`).
    pub(crate) superseding: bool,
    /// It landed: the newer release is the lane's stage in the activation's place.
    pub(crate) superseded: bool,
    /// It was refused, and said.
    pub(crate) refused: bool,
    /// A refused retire's release waits out its retry deadline (`AutoApplyStructuralVerdict::newer_retry`, not yet due).
    pub(crate) backoff: bool,
    /// The latched build was launched for an attempt while a newer release waited behind it.
    pub(crate) booted_old: bool,
}

/// Project one [`StructuralLatchObservation`] onto the model's variables.
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

/// The source commit the latch fixtures' builds were sealed from.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// `build` installed, `trial_launches` launches into its boot trial.
fn installed_bundle(
    build: u64,
    trial_launches: u32,
) -> crate::native_updater_service::InstalledUpdate {
    crate::native_updater_service::InstalledUpdate {
        build,
        commit: COMMIT.to_string(),
        version: None,
        receipt_build: None,
        receipt_dmg_sha256: None,
        trial_launches,
    }
}

/// One facts-worker observation: the installed bundle (`None`: not found, or
/// found but not `proven`) and a verified download of `newer`, if any.
fn observation(
    sequence: u64,
    installed: Option<crate::native_updater_service::InstalledUpdate>,
    newer: Option<u64>,
    unproven: bool,
) -> crate::app_native::NativeUpdateReconcileFacts {
    crate::app_native::NativeUpdateReconcileFacts {
        _ticket: crate::app_native::NativeUpdateReconcileTicket::for_test(sequence),
        observation_sequence: sequence,
        observed_at: std::time::Instant::now(),
        durable: Some(DurableUpdateStatus {
            staged_dmg_sha256: newer.map(|_| "cd".repeat(32)),
            ..status(newer, 0)
        }),
        installed,
        installed_unproven: unproven,
        installable: None,
    }
}

/// A structural failure of build 11's installed activation: an overlap
/// handoff that failed safely, returned through the completion lane.
fn fail_activation_11(app: &mut crate::App) {
    use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};
    let activation = crate::native_updater_service::installed_activation_digest(11, COMMIT);
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(11, COMMIT, &activation);
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    let _ = app.abort_reaped_native_apply_before_reconcile(
        &ticket,
        "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
        HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
    );
}

/// A headless App running build 10 whose lane converged on the installed
/// activation of build 11 (`installed`): two structural failures.
fn converged_on_11(installed: crate::native_updater_service::InstalledUpdate) -> crate::App {
    let mut app = crate::App::headless_for_test();
    app.native_updater_service = NativeUpdaterService::new(10, "1.0.10", true);
    let _ = app.reconcile_native_update_facts(observation(1, Some(installed), None, false));
    fail_activation_11(&mut app);
    fail_activation_11(&mut app);
    app
}

/// The real lane on build 11's latch with room in the trial and nothing
/// newer, projected onto the model.
fn observe_latch(model: &Model, app: &crate::App, day: bool, pending: bool) -> State {
    let verdict = app.auto_apply_structural_verdict.expect("the verdict");
    project_structural_latch(
        model,
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
}

/// A second ago: a deadline that has passed.
fn past() -> std::time::Instant {
    std::time::Instant::now() - std::time::Duration::from_secs(1)
}

/// The latch's deadline passes, and the lane looks at it.
fn due_look(app: &mut crate::App) -> bool {
    let latch = app.auto_apply_manual_only.expect("still latched");
    app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
        retry_at: Some(past()),
        ..latch
    });
    app.lapse_expired_auto_apply_manual_only()
}

/// The decision, exhaustively: for every reachable model state in which `Decide` is enabled,
/// the shipping `structural_latch` is fed the facts that state names (the day's re-sample due,
/// an unspent newer download, the trial's room), and its answer, applied as
/// `App::look_at_structural_latch` applies it, must be exactly the model's `Decide` successor.
#[test]
fn real_structural_latch_decision_is_exactly_the_model_s_decide() {
    use crate::native_update_auto_intent::{
        StructuralLatchDecision, StructuralLatchFacts, structural_latch,
    };
    let model = aterm_models_update::native_update_structural_latch_model();
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
        let mut looks_after = if unread { looks } else { 0 };
        let doubts = u8::try_from(state["doubts"]).expect("a small count");
        let mut doubts_after = if unproven { doubts } else { 0 };
        let mut moved = false;
        let mut asked = false;
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
                    unread_held += 1;
                    looks_after = 0;
                    (true, true, false, true, false)
                } else {
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
                booted_old: state["booted_old"] == 1 || (!latched && unspent),
            },
        );
        assert_exact_model_action(&model, "Decide", &state, &after);
        if facts.newer_download_waiting {
            waited += 1;
        }
        let mutant = buggy.successors("Decide", &state);
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
}

/// The shipping arc, projected step by step: the real `App` converges on build 11's installed
/// activation through the completion lane, and every later step, read off the real latch,
/// verdict, retire and notice, is checked against the model's own step:
#[test]
fn real_structural_latch_arc_is_the_model_s_arc() {
    use crate::app_native::ActivationSupersedeCompletion;
    let model = aterm_models_update::native_update_structural_latch_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window must open"
    );

    for room in [true, false] {
        for lands in [true, false] {
            let launches = if room {
                0
            } else {
                crate::app_native::BOOT_TRIAL_LAUNCH_LIMIT - 1
            };
            let facts = |sequence: u64, installed: u64, newer: Option<u64>| {
                let trial = if installed == 11 { launches } else { 0 };
                observation(
                    sequence,
                    Some(installed_bundle(installed, trial)),
                    newer,
                    false,
                )
            };
            let mut app = converged_on_11(installed_bundle(11, launches));
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
            let arc = format!("room={room} lands={lands}");

            assert!(
                app.native_installed_trial.is_none(),
                "{arc}: a returned attempt leaves the count unmeasured"
            );
            let mut counted = model.init_state();
            assert!(model.fire(
                if room { "TrialHasRoom" } else { "TrialIsSpent" },
                &mut counted
            ));
            let _ = app.reconcile_native_update_facts(facts(2, 11, None));
            let converged = observe(&app, false, false, false, false);
            assert_exact_model_action(&model, "Decide", &counted, &converged);
            if !room {
                let promise = buggy.successors("Decide", &counted)[0].clone();
                assert_ne!(promise, converged);
                assert!(!buggy.check_invariant("NoRetryPromisedPastTheTrial", &promise));
            }

            let escaped = app.arm_native_auto_apply(11, &"ef".repeat(32));
            let after = observe(&app, false, false, escaped, false);
            assert_exact_model_action(&model, "ArmSameBuild", &converged, &after);

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
                assert_eq!(
                    app.auto_apply_ladder.map(|ladder| ladder.build),
                    Some(12),
                    "{arc}"
                );
                assert_eq!(
                    app.automatic_apply_phase(std::time::Instant::now()),
                    ApplyPhase::PreferIdle,
                    "{arc}"
                );
                assert!(
                    app.native_updater_service.snapshot().active.is_none(),
                    "{arc}: build 11 was never launched for it"
                );
            };

            if lands {
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

            let refused = model.successors("SupersedeRefused", &decided)[0].clone();
            let relooked = model.successors("Decide", &refused)[0].clone();
            app.finish_activation_supersede(ActivationSupersedeCompletion {
                job,
                outcome: Err(aterm_update::SupersedeRefusal::ForNow(
                    "the installed build 11 stays in place: the staged bundle does not verify: \
                     the apply budget ran out"
                        .to_string(),
                )),
                facts: Some(facts(4, 11, Some(12))),
            });
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
            let _ = app.reconcile_native_update_facts(facts(5, 11, Some(12)));
            assert!(app.native_activation_supersede.is_none(), "{arc}");

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

    {
        let facts =
            |sequence: u64| observation(sequence, Some(installed_bundle(11, 0)), None, false);
        let mut app = converged_on_11(installed_bundle(11, 0));
        let observe = |app: &crate::App, day: bool| {
            let mut state = observe_latch(&model, app, day, false);
            state.insert("unread", 0);
            state.insert("said", 0);
            state
        };
        let mut counted = model.init_state();
        assert!(model.fire("TrialHasRoom", &mut counted));
        let _ = app.reconcile_native_update_facts(facts(2));
        let converged = observe(&app, false);
        assert_exact_model_action(&model, "Decide", &counted, &converged);
        let latch = app.auto_apply_manual_only.expect("latched");
        let verdict = app.auto_apply_structural_verdict.expect("the verdict");
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            retry_at: Some(past()),
            ..latch
        });
        app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
            resample_at: Some(past()),
            ..verdict
        });
        let dawn = model.successors("DayPasses", &converged)[0].clone();
        assert!(app.lapse_expired_auto_apply_manual_only());
        let resampled = observe(&app, true);
        assert_exact_model_action(&model, "Decide", &dawn, &resampled);
        let activation = crate::native_updater_service::installed_activation_digest(11, COMMIT);
        assert!(app.arm_native_auto_apply(11, &activation));
        fail_activation_11(&mut app);
        let done = observe(&app, true);
        assert_exact_model_action(&model, "AttemptFails", &resampled, &done);
        assert_eq!(
            app.auto_apply_manual_only.map(|manual| manual.retry_at),
            Some(None),
            "after its one re-sample the verdict owes nothing"
        );
    }

    let entered = aterm_spec::xref::entered_anchor_ids();
    aterm_spec::xref::disarm_entered_anchors();
    for anchor in aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "NativeUpdateStructuralLatch")
    {
        assert!(
            !anchor.entry_id.is_empty() && entered.contains(anchor.entry_id),
            "{} was not entered by the arc: {entered:?}",
            anchor.entry_id
        );
    }
}

/// A latch whose activation leaves the installed facts is decided within a bound, projected
/// step by step onto the model.
#[test]
fn a_latch_whose_activation_leaves_the_installed_facts_is_decided_within_a_bound() {
    use crate::app_native::STRUCTURAL_TRIAL_UNMEASURED_LOOKS;
    let model = aterm_models_update::native_update_structural_latch_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for measured_again in [false, true] {
        let bundle = installed_bundle(11, 0);
        let facts = |sequence: u64, installed| observation(sequence, installed, None, false);
        let mut app = converged_on_11(bundle.clone());
        let latch = app.auto_apply_manual_only.expect("converged on 11");
        assert!(latch.activation && latch.build == 11);
        let case = format!("measured_again={measured_again}");
        let observe =
            |app: &crate::App, day: bool, pending: bool| observe_latch(&model, app, day, pending);

        let mut modeled = model.init_state();
        assert!(model.fire("TrialHasRoom", &mut modeled));
        let _ = app.reconcile_native_update_facts(facts(2, Some(bundle.clone())));
        let real = observe(&app, false, false);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        modeled = real;

        assert!(model.fire("TrialUnreadable", &mut modeled));
        let mut sequence = 3;
        let _ = app.reconcile_native_update_facts(facts(sequence, None));
        assert!(app.native_installed_trial.is_none(), "{case}: unmeasured");
        let real = observe(&app, false, false);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        modeled = real;

        assert!(model.fire("DayPasses", &mut modeled));
        let verdict = app.auto_apply_structural_verdict.expect("the verdict");
        app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
            resample_at: Some(past()),
            ..verdict
        });
        let bound = STRUCTURAL_TRIAL_UNMEASURED_LOOKS;
        for look in 1..=bound {
            assert!(!due_look(&mut app), "{case}: look {look}");
            let real = observe(&app, true, true);
            assert_exact_model_action(&model, "Decide", &modeled, &real);
            assert_eq!(real["looks"], i64::from(look), "{case}");
            modeled = real;
            assert!(!due_look(&mut app), "{case}: look {look} again");
            assert_eq!(
                observe(&app, true, true),
                modeled,
                "{case}: look {look} again"
            );
            if measured_again && look == 2 {
                break;
            }
            sequence += 1;
            let _ = app.reconcile_native_update_facts(facts(sequence, None));
            assert_eq!(
                observe(&app, true, true),
                modeled,
                "{case}: look {look} read"
            );
        }
        if measured_again {
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
        assert_ne!(mutant, real, "{case}: the shipping look is not the mutant");
        assert!(!buggy.check_invariant("NoRetryPromisedPastTheBound", &mutant));
        let spent = installed_bundle(11, crate::app_native::BOOT_TRIAL_LAUNCH_LIMIT - 1);
        sequence += 1;
        let _ = app.reconcile_native_update_facts(facts(sequence, Some(spent)));
        assert!(app.latched_activation_trial_spent(11), "{case}: spent");
        use crate::update_words::LiveHealth as _;
        let said = app
            .messages
            .live_health()
            .map(|live| live.msg.detail.join("; "))
            .expect("the warning is up");
        assert!(
            !said.contains(crate::update_words::INSTALL_FROM_MENU),
            "{case}: the notice names no start the trial rolls back: {said}"
        );
        assert!(
            !app.auto_apply_structural_verdict
                .expect("the verdict")
                .held_unmeasured,
            "{case}: said once"
        );
    }
}

/// A count that could not be checked is not a count that is gone, projected step by step onto
/// the model.
#[test]
fn a_latch_whose_count_could_not_be_checked_keeps_its_re_sample() {
    use crate::app_native::STRUCTURAL_TRIAL_UNPROVEN_LOOKS;
    let model = aterm_models_update::native_update_structural_latch_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let case = "unproven";
    let bundle = installed_bundle(11, 0);
    let facts =
        |sequence: u64, installed, unproven: bool| observation(sequence, installed, None, unproven);
    let mut app = converged_on_11(bundle.clone());
    let latch = app.auto_apply_manual_only.expect("converged on 11");
    assert!(latch.activation && latch.build == 11, "{case}");
    let observe =
        |app: &crate::App, day: bool, pending: bool| observe_latch(&model, app, day, pending);

    let mut modeled = model.init_state();
    assert!(model.fire("TrialHasRoom", &mut modeled));
    let _ = app.reconcile_native_update_facts(facts(2, Some(bundle.clone()), false));
    let real = observe(&app, false, false);
    assert_exact_model_action(&model, "Decide", &modeled, &real);
    modeled = real;

    assert!(model.fire("TrialUnproven", &mut modeled));
    let mut sequence = 3;
    let _ = app.reconcile_native_update_facts(facts(sequence, None, true));
    let real = observe(&app, false, false);
    assert_exact_model_action(&model, "Decide", &modeled, &real);
    modeled = real;

    let day_passes = |app: &mut crate::App, modeled: &mut State| {
        assert!(model.fire("DayPasses", modeled));
        let verdict = app.auto_apply_structural_verdict.expect("the verdict");
        app.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
            resample_at: Some(past()),
            ..verdict
        });
    };
    day_passes(&mut app, &mut modeled);
    for look in 1..=STRUCTURAL_TRIAL_UNPROVEN_LOOKS {
        assert!(!due_look(&mut app), "{case}: look {look}");
        let real = observe(&app, true, true);
        assert_exact_model_action(&model, "Decide", &modeled, &real);
        assert_eq!(real["doubts"], i64::from(look), "{case}: look {look}");
        modeled = real;
        assert!(!due_look(&mut app), "{case}: look {look} again");
        assert_eq!(
            observe(&app, true, true),
            modeled,
            "{case}: look {look} again"
        );
        sequence += 1;
        let _ = app.reconcile_native_update_facts(facts(sequence, None, true));
        assert_eq!(
            observe(&app, true, true),
            modeled,
            "{case}: look {look} read"
        );
    }
    let mutant = buggy.successors("Decide", &modeled)[0].clone();
    assert!(!due_look(&mut app), "{case}: the look at the bound");
    let real = observe(&app, false, false);
    assert_exact_model_action(&model, "Decide", &modeled, &real);
    let latch = app.auto_apply_manual_only.expect("the latch stands");
    let verdict = app.auto_apply_structural_verdict.expect("the verdict");
    let tomorrow = std::time::Instant::now() + crate::app_native::STRUCTURAL_RESAMPLE_AFTER;
    let a_day_out = |at: Option<std::time::Instant>| {
        at.is_some_and(|at| at <= tomorrow && at + std::time::Duration::from_secs(60) >= tomorrow)
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
    assert_eq!(
        real["said"], 0,
        "{case}: nothing asked of a person, so no hold raised"
    );
    assert_ne!(mutant, real, "{case}: the shipping look is not the mutant");
    assert!(!buggy.check_invariant("UnprovenLooksAreBounded", &mutant));
    modeled = real;

    day_passes(&mut app, &mut modeled);
    sequence += 1;
    let _ = app.reconcile_native_update_facts(facts(sequence, None, true));
    assert!(!due_look(&mut app), "{case}: the next day's first look");
    let real = observe(&app, true, true);
    assert_exact_model_action(&model, "Decide", &modeled, &real);
    assert_eq!(real["doubts"], 1, "{case}: counted afresh");
    modeled = real;

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

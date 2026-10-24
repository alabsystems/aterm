// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::scripted_redraw_fixture::{feed, glass, has_damage, scripted_app, surface};
use super::{
    App, PRESENT_RETRY_BASE, PRESENT_RETRY_CAP, PresentDropAccounting, PresentRetry, WindowId,
    fold_owned_deadline, rearm_present_and_request, request_gpu_recovery_redraw,
    request_recovery_redraw, service_due_presentation_clocks,
};
use crate::app_render::{FailedPresentRoute, GpuRecoveryOutcome, failed_present_route};
use crate::input::{InputEvent, PixelOffset, Source};
use crate::metrics::{DeadlineOwner, PresentDropReason};
use aterm_core::selection::SelectionSide;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

#[derive(Clone, Copy, Debug)]
enum Drop {
    Cpu(crate::present::scripted::ScriptedOutcome),
    Gpu(PresentDropReason),
}

#[test]
fn a_dropped_echo_frame_reaches_glass_on_the_retry_for_every_drop_reason() {
    use crate::present::scripted::ScriptedOutcome;
    let wid = WindowId(0);
    let floor = Duration::from_millis(8);

    let mut control = scripted_app();
    control.redraw_window(wid);
    let blank = glass(&mut control);
    feed(&control, b"echo");
    control.redraw_window(wid);
    let expected = glass(&mut control);
    assert_eq!(surface(&mut control).commits(), 2);
    assert_ne!(
        expected, blank,
        "the control's echo frame changed the glass"
    );

    for case in [
        Drop::Cpu(ScriptedOutcome::FailAcquire),
        Drop::Cpu(ScriptedOutcome::FailCommit),
        Drop::Gpu(PresentDropReason::GpuTimeout),
        Drop::Gpu(PresentDropReason::GpuOccluded),
        Drop::Gpu(PresentDropReason::TargetMismatch),
    ] {
        let mut app = scripted_app();
        app.redraw_window(wid);
        assert_eq!(glass(&mut app), blank, "{case:?}: same starting glass");
        feed(&app, b"echo");
        let outcome = match case {
            Drop::Cpu(outcome) => outcome,
            Drop::Gpu(_) => ScriptedOutcome::FailAcquire,
        };
        surface(&mut app).fail_next(outcome);
        app.redraw_window(wid);
        assert!(
            !has_damage(&app),
            "{case:?}: the dropped frame consumed the echo"
        );
        assert_eq!(glass(&mut app), blank, "{case:?}: nothing reached glass");
        if let Drop::Gpu(reason) = case {
            app.windows.get_mut(&wid).unwrap().present_retry = PresentRetry::default();
            app.rearm_dropped_present(wid, reason);
        }

        app.redraw_window(wid);
        assert_eq!(
            surface(&mut app).commits(),
            1,
            "{case:?}: gated before the retry"
        );

        let ws = app.windows.get_mut(&wid).unwrap();
        let fired = match ws.present_retry.deadline {
            Some(deadline) => service_due_presentation_clocks(
                false,
                &mut ws.present_retry,
                &mut ws.redraw_pending,
                ws.last_present_at,
                floor,
                deadline,
            ),
            None => {
                assert!(
                    ws.present_retry.parked,
                    "{case:?}: no deadline means parked"
                );
                ws.present_retry.on_external_stimulus()
            }
        };
        assert!(fired, "{case:?}: the retry fires");
        app.redraw_window(wid);
        assert_eq!(
            surface(&mut app).commits(),
            2,
            "{case:?}: the retry presented"
        );
        assert!(
            glass(&mut app) == expected,
            "{case:?}: the retry put the consumed echo on glass, identical to the control"
        );
    }
}

#[test]
fn wait_cancelled_wake_flushes_due_frame_cap_level() {
    let presented = Instant::now();
    let floor = Duration::from_millis(8);
    let mut retry = PresentRetry::default();
    let mut pending = true;

    assert!(
        !service_due_presentation_clocks(
            false,
            &mut retry,
            &mut pending,
            Some(presented),
            floor,
            presented + floor - Duration::from_nanos(1),
        ),
        "an external wake before the pacing boundary must preserve the level"
    );
    assert!(pending);

    assert!(service_due_presentation_clocks(
        false,
        &mut retry,
        &mut pending,
        Some(presented),
        floor,
        presented + floor,
    ));
    assert!(!pending, "consume before request_redraw delivery");
}

#[test]
fn wait_cancelled_wake_consumes_due_present_retry_level() {
    let now = Instant::now();
    let mut retry = PresentRetry::default();
    let deadline = retry
        .on_drop(PresentDropReason::GpuTimeout, now)
        .expect("retryable drop");
    let mut pending = false;

    assert!(service_due_presentation_clocks(
        false,
        &mut retry,
        &mut pending,
        None,
        Duration::from_millis(8),
        deadline,
    ));
    assert_eq!(retry.deadline, None, "never re-arm a past WaitUntil");
    assert!(retry.recovery_redraw_outstanding);
    assert!(!pending);
}

fn project(
    retry: &PresentRetry,
    now: Instant,
    bootstrap_available: bool,
) -> BTreeMap<&'static str, i64> {
    let pending = u8::from(retry.deadline.is_some());
    let train = retry.autonomous_retries.saturating_sub(pending);
    let remaining = PRESENT_RETRY_CAP
        .saturating_sub(retry.autonomous_retries)
        .saturating_add(pending);
    let deadline = retry
        .deadline
        .map_or(0, |deadline| if deadline > now { 1 } else { 2 });
    BTreeMap::from([
        ("remaining", i64::from(remaining)),
        ("train", i64::from(train)),
        ("retry", deadline),
        (
            "ready",
            i64::from(u8::from(retry.present_attempt_allowed())),
        ),
        ("parked", i64::from(u8::from(retry.parked))),
        (
            "outstanding",
            i64::from(u8::from(retry.recovery_redraw_outstanding)),
        ),
        (
            "bootstrap_available",
            i64::from(u8::from(bootstrap_available)),
        ),
    ])
}

#[test]
fn failed_present_device_loss_route_conforms_to_derived_model() {
    let model = aterm_models_core::gpu_loss_route_model();
    let mut state = model.init_state();

    assert!(model.fire("FailHealthy", &mut state));
    assert_eq!(
        failed_present_route(true, false),
        FailedPresentRoute::RetrySurface
    );
    assert_eq!(state["lost"], 0);
    assert_eq!(state["route"], 1);

    assert!(model.fire("Reset", &mut state));
    assert!(model.fire("FailLost", &mut state));
    assert_eq!(
        failed_present_route(true, true),
        FailedPresentRoute::RecoverGpu
    );
    assert_eq!(state["lost"], 1);
    assert_eq!(state["route"], 2);

    let mut deleted = state;
    deleted.insert("route", 1);
    assert!(!model.check_invariant("LostUsesFallback", &deleted));
}

#[test]
fn successful_gpu_recovery_classifies_source_ready_only_after_redraw_request() {
    let model = aterm_models_core::gpu_loss_recovery_model();
    let wid = WindowId(0);
    for (failed_reason, action, expected_drops, expected_reason) in [
        (None, "SucceedFallbackAfterPresent", 0, 0),
        (
            Some(PresentDropReason::GpuTimeout),
            "SucceedFallbackAfterDrop",
            1,
            1,
        ),
    ] {
        let mut app = App::headless_for_test();
        app.use_gpu = true;
        app.backend.set_pad(app.cfg_pad_for_scale(2.0));
        app.backend.set_head(60);
        app.seed_headless_boot_metrics();
        let sealed = (app.backend.pad(), app.backend.pad_top(), app.backend.head());
        let mut requests = 0;
        let observed = app.complete_latched_gpu_loss_recovery(
            wid,
            failed_reason,
            Instant::now(),
            |app, source, counted| {
                assert_eq!(source, wid);
                assert_eq!(counted, failed_reason.is_some());
                app.recover_from_gpu_loss_with(
                    source,
                    counted,
                    |_, target| {
                        assert_eq!(target, wid);
                        Ok(())
                    },
                    |retry, target, window| {
                        assert_eq!(target, wid);
                        assert!(
                            window.is_none(),
                            "the fixture deliberately has no OS window"
                        );
                        request_gpu_recovery_redraw(retry, Some(&()), |_| requests += 1)
                    },
                )
            },
        );
        assert_eq!(observed, GpuRecoveryOutcome::SourceReadyRequested);
        assert_eq!(requests, 1);
        assert!(!app.use_gpu);
        assert!(!app.backend.is_gpu());
        assert_eq!(
            (app.backend.pad(), app.backend.pad_top(), app.backend.head()),
            sealed,
            "the CPU fallback composes at the sealed headless geometry"
        );
        assert!(app.windows[&wid].present_retry.recovery_redraw_outstanding);

        let mut state = model.init_state();
        assert!(model.fire(action, &mut state));
        assert_eq!(state["cpu_ready"], 1);
        assert_eq!(
            state["requested"],
            i64::from(app.windows[&wid].present_retry.recovery_redraw_outstanding),
            "the outstanding first CPU redraw is the shipping retry state's"
        );
        assert_eq!(state["cpu_presented"], 0);
        assert_eq!(state["drops"], expected_drops);
        assert_eq!(state["reason"], expected_reason);
        let mut frozen = state.clone();
        frozen.insert("requested", 0);
        assert!(!model.check_invariant("ReadyCpuOwnsRedrawUntilPresent", &frozen));

        assert!(model.fire("PresentCpu", &mut state));
        app.windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .on_presented();
        assert_eq!(state["cpu_presented"], 1);
        assert_eq!(app.windows[&wid].present_retry, PresentRetry::default());
    }

    let mut app = App::headless_for_test();
    app.use_gpu = true;
    let observed = app.complete_latched_gpu_loss_recovery(
        wid,
        None,
        Instant::now(),
        |app, source, counted| {
            app.recover_from_gpu_loss_with(
                source,
                counted,
                |_, _| Err(PresentDropReason::CpuAcquire),
                |_, _, _| panic!("a failed source target must not request redraw"),
            )
        },
    );
    let GpuRecoveryOutcome::SourceRetry(retry) = observed else {
        panic!("target failure after backend install must be classified as retry: {observed:?}");
    };
    assert_eq!(retry.reason, PresentDropReason::CpuAcquire);
    assert!(!retry.parked);
    assert!(
        !app.use_gpu,
        "the process-wide CPU fallback was still installed"
    );
    assert!(app.windows[&wid].present_retry.deadline.is_some());
    assert!(!app.windows[&wid].present_retry.recovery_redraw_outstanding);
}

fn recovery_reason_code(reason: PresentDropReason) -> i64 {
    match reason {
        PresentDropReason::None => 0,
        PresentDropReason::CpuAcquire => 2,
        _ => 1,
    }
}

#[test]
fn failed_gpu_recovery_retries_both_entry_paths_and_counts_once() {
    let model = aterm_models_core::gpu_loss_recovery_model();
    let wid = WindowId(0);

    let mut app = App::headless_for_test();
    let mut clock = Instant::now();
    {
        let retry = &mut app.windows.get_mut(&wid).unwrap().present_retry;
        for _ in 0..PRESENT_RETRY_CAP {
            let deadline = retry
                .on_drop(PresentDropReason::GpuTimeout, clock)
                .expect("retry fuel");
            assert!(retry.take_due(deadline));
            clock = deadline;
        }
        assert_eq!(retry.autonomous_retries, PRESENT_RETRY_CAP);
        retry.on_presented();
        assert_eq!(*retry, PresentRetry::default());
    }
    let mut after_present = model.init_state();
    assert!(model.fire("FailFallbackAfterPresent", &mut after_present));
    let observed =
        app.complete_latched_gpu_loss_recovery(wid, None, Instant::now(), |_, source, counted| {
            assert_eq!(source, wid);
            assert!(!counted);
            GpuRecoveryOutcome::BackendUnavailable
        });
    let GpuRecoveryOutcome::SourceRetry(observed) = observed else {
        panic!("failed CPU builder must schedule a source retry: {observed:?}");
    };
    assert_eq!(observed.accounting, PresentDropAccounting::Count);
    assert_eq!(observed.reason, PresentDropReason::CpuAcquire);
    assert!(!observed.parked);
    let deadline = app.windows[&wid]
        .present_retry
        .deadline
        .expect("post-present loss owns a future retry");
    assert_eq!(after_present["retry"], 1);
    assert_eq!(after_present["drops"], 1);
    assert_eq!(
        after_present["reason"],
        recovery_reason_code(observed.reason)
    );
    assert_eq!(after_present["parked"], i64::from(observed.parked));
    assert!(model.fire("Wake", &mut after_present));
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .take_due(deadline)
    );
    assert_eq!(after_present["delivered"], 1);
    assert_eq!(
        after_present["retry"],
        i64::from(app.windows[&wid].present_retry.deadline.is_some()),
        "take_due consumes the deadline it hands over"
    );
    let mut rearmed = after_present.clone();
    rearmed.insert("retry", 1);
    assert!(!model.check_invariant("DeliveredRetryHasNoDeadline", &rearmed));
    assert!(model.fire("BuildCpuAfterWake", &mut after_present));
    let mut redraw_requests = 0;
    request_recovery_redraw(
        &mut app.windows.get_mut(&wid).unwrap().present_retry,
        || redraw_requests += 1,
    );
    assert_eq!(redraw_requests, 1);
    assert!(app.windows[&wid].present_retry.recovery_redraw_outstanding);
    assert_eq!(after_present["cpu_ready"], 1);
    assert_eq!(after_present["requested"], 1);
    assert_eq!(after_present["cpu_presented"], 0);
    assert!(model.fire("PresentCpu", &mut after_present));
    app.windows
        .get_mut(&wid)
        .unwrap()
        .present_retry
        .on_presented();
    assert_eq!(after_present["cpu_presented"], 1);
    assert_eq!(app.windows[&wid].present_retry, PresentRetry::default());

    let mut app = App::headless_for_test();
    let mut after_drop = model.init_state();
    assert!(model.fire("FailFallbackAfterDropWithFuel", &mut after_drop));
    let observed = app.complete_latched_gpu_loss_recovery(
        wid,
        Some(PresentDropReason::GpuTimeout),
        Instant::now(),
        |_, source, counted| {
            assert_eq!(source, wid);
            assert!(counted);
            GpuRecoveryOutcome::BackendUnavailable
        },
    );
    let GpuRecoveryOutcome::SourceRetry(observed) = observed else {
        panic!("failed CPU builder must refine the source drop: {observed:?}");
    };
    assert_eq!(observed.accounting, PresentDropAccounting::Update);
    assert_eq!(observed.reason, PresentDropReason::CpuAcquire);
    assert!(!observed.parked);
    assert!(app.windows[&wid].present_retry.deadline.is_some());
    assert_eq!(after_drop["retry"], 1);
    assert_eq!(after_drop["drops"], 1);
    assert_eq!(after_drop["reason"], recovery_reason_code(observed.reason));
    let mut undiagnosed = after_drop.clone();
    undiagnosed.insert(
        "reason",
        recovery_reason_code(PresentDropReason::GpuTimeout),
    );
    assert!(!model.check_invariant("FailedFallbackIsDiagnosed", &undiagnosed));

    let mut app = App::headless_for_test();
    let mut clock = Instant::now();
    {
        let retry = &mut app.windows.get_mut(&wid).unwrap().present_retry;
        for _ in 0..PRESENT_RETRY_CAP {
            let deadline = retry
                .on_drop(PresentDropReason::GpuTimeout, clock)
                .expect("retry fuel");
            assert!(retry.take_due(deadline));
            clock = deadline;
        }
    }
    let observed = app.complete_latched_gpu_loss_recovery(
        wid,
        Some(PresentDropReason::GpuTimeout),
        Instant::now(),
        |_, _, _| GpuRecoveryOutcome::BackendUnavailable,
    );
    let GpuRecoveryOutcome::SourceRetry(observed) = observed else {
        panic!("exhausted recovery must remain diagnosed: {observed:?}");
    };
    assert_eq!(observed.accounting, PresentDropAccounting::Update);
    assert!(observed.parked);
    assert!(app.windows[&wid].present_retry.parked);
    let mut exhausted = model.init_state();
    assert!(model.fire("FailFallbackAfterDropExhausted", &mut exhausted));
    assert_eq!(
        exhausted["retry"],
        i64::from(app.windows[&wid].present_retry.deadline.is_some())
    );
    assert_eq!(exhausted["parked"], i64::from(observed.parked));
    let mut unparked = exhausted.clone();
    unparked.insert("retry", 1);
    unparked.insert("parked", 0);
    assert!(!model.check_invariant("ExhaustedFailureIsParked", &unparked));

    let mut no_retry = model.init_state();
    assert!(model.fire("FailFallbackAfterPresent", &mut no_retry));
    no_retry.insert("retry", 0);
    assert!(!model.check_invariant("UnexhaustedFailureOwnsRetryOrDeliveredAttempt", &no_retry));
    let mut counted_twice = after_drop;
    counted_twice.insert("drops", 2);
    assert!(!model.check_invariant("OneDropCountPerFrame", &counted_twice));
}

#[test]
fn unresolved_recovery_stimulus_delivers_redraw_and_conforms() {
    let model = aterm_models_core::recovery_redraw_model();
    let mut state = model.init_state();
    let mut retry = PresentRetry::default();
    retry.on_drop(PresentDropReason::GpuOccluded, Instant::now());

    assert!(model.fire("Stimulus", &mut state));
    let mut requests = 0;
    assert!(rearm_present_and_request(&mut retry, false, || requests += 1));
    assert_eq!(
        requests, 1,
        "the recovery stimulus delivers one redraw edge"
    );
    assert!(retry.recovery_redraw_outstanding);
    assert_eq!(state["unresolved"], 1);
    assert_eq!(state["stimulated"], 1);
    assert_eq!(state["requested"], i64::from(requests));

    let mut silent = state.clone();
    silent.insert("requested", 0);
    assert!(
        !model.check_invariant("RecoveryStimulusRequestsRedraw", &silent),
        "negative control: reset-without-request must be rejected"
    );

    assert!(model.fire("Suppress", &mut state));
    assert_eq!(state["unresolved"], 1);
    assert!(retry.recovery_redraw_outstanding);
    assert!(model.fire("Stimulus", &mut state));
    assert!(rearm_present_and_request(&mut retry, false, || requests += 1));
    assert_eq!(requests, 2);
    assert!(model.fire("Suppress", &mut state));
    assert!(model.fire("Stimulus", &mut state));
    assert!(rearm_present_and_request(&mut retry, false, || requests += 1));
    assert_eq!(requests, 3);
    assert!(model.fire("Present", &mut state));
    retry.on_presented();
    assert_eq!(state["unresolved"], 0);
    assert_eq!(state["presented"], 1);
    assert_eq!(retry, PresentRetry::default());

    let mut retry = PresentRetry::default();
    let deadline = retry
        .on_drop(PresentDropReason::GpuTimeout, Instant::now())
        .unwrap();
    assert!(retry.take_due(deadline));
    assert!(retry.present_attempt_allowed());
    assert!(retry.recovery_redraw_outstanding);
    let mut suppressed_recovery_requests = 0;
    assert!(rearm_present_and_request(&mut retry, false, || {
        suppressed_recovery_requests += 1;
    }));
    assert_eq!(suppressed_recovery_requests, 1);
    assert!(rearm_present_and_request(&mut retry, false, || {
        suppressed_recovery_requests += 1;
    }));
    assert_eq!(suppressed_recovery_requests, 2);
    retry.on_presented();

    let mut retry = PresentRetry::default();
    assert!(!rearm_present_and_request(&mut retry, false, || {
        panic!("idle input stimulus must not redraw")
    }));
    let mut monitor_requests = 0;
    assert!(rearm_present_and_request(&mut retry, true, || {
        monitor_requests += 1;
    }));
    assert_eq!(monitor_requests, 1);
    assert!(retry.recovery_redraw_outstanding);
    retry.on_presented();

    let mut retry = PresentRetry::default();
    let mut initial_requests = 0;
    request_recovery_redraw(&mut retry, || initial_requests += 1);
    assert_eq!(initial_requests, 1);
    assert!(retry.recovery_redraw_outstanding);
    let mut replacement_requests = 0;
    assert!(rearm_present_and_request(&mut retry, false, || {
        replacement_requests += 1;
    }));
    assert_eq!(replacement_requests, 1);
    assert!(retry.recovery_redraw_outstanding);
    assert!(rearm_present_and_request(&mut retry, false, || {
        replacement_requests += 1;
    }));
    assert_eq!(replacement_requests, 2);
    retry.on_presented();
    assert_eq!(retry, PresentRetry::default());
}

fn assert_matches(
    model_state: &BTreeMap<&'static str, i64>,
    retry: &PresentRetry,
    now: Instant,
    bootstrap_available: bool,
) {
    assert_eq!(project(retry, now, bootstrap_available), *model_state);
}

#[test]
fn bootstrap_occlusion_gets_exactly_one_future_retry() {
    let model = aterm_models_core::present_retry_model();
    let mut model_state = model.init_state();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    assert!(model.fire("DropBootstrapOccluded", &mut model_state));
    let before_drop = Instant::now();
    app.rearm_dropped_present(wid, PresentDropReason::GpuOccluded);
    let deadline = {
        let window = &app.windows[&wid];
        assert!(!window.bootstrap_present_retry_available);
        let deadline = window
            .present_retry
            .deadline
            .expect("the pristine occlusion must arm one retry");
        assert!(deadline > before_drop);
        assert_matches(&model_state, &window.present_retry, before_drop, false);
        deadline
    };

    assert!(model.fire("Wake", &mut model_state));
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .take_due(deadline)
    );
    assert_matches(
        &model_state,
        &app.windows[&wid].present_retry,
        deadline,
        false,
    );

    assert!(model.fire("DropPersistent", &mut model_state));
    app.rearm_dropped_present(wid, PresentDropReason::GpuOccluded);
    let window = &app.windows[&wid];
    assert!(window.present_retry.parked);
    assert_eq!(window.present_retry.deadline, None);
    assert!(!window.bootstrap_present_retry_available);
    assert_matches(&model_state, &window.present_retry, Instant::now(), false);
}

#[test]
fn successful_present_permanently_closes_bootstrap_retry() {
    let model = aterm_models_core::present_retry_model();
    let mut model_state = model.init_state();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    assert!(model.fire("Present", &mut model_state));
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.on_present_succeeded();
        assert!(!window.bootstrap_present_retry_available);
        assert_matches(&model_state, &window.present_retry, Instant::now(), false);
    }

    assert!(model.fire("ForcedStimulus", &mut model_state));
    let mut requests = 0;
    {
        let window = app.windows.get_mut(&wid).unwrap();
        assert!(rearm_present_and_request(
            &mut window.present_retry,
            true,
            || requests += 1,
        ));
        assert!(!window.bootstrap_present_retry_available);
        assert_matches(&model_state, &window.present_retry, Instant::now(), false);
    }
    assert_eq!(requests, 1);

    assert!(model.fire("DropPersistent", &mut model_state));
    app.rearm_dropped_present(wid, PresentDropReason::GpuOccluded);
    let window = &app.windows[&wid];
    assert!(window.present_retry.parked);
    assert!(!window.bootstrap_present_retry_available);
    assert_matches(&model_state, &window.present_retry, Instant::now(), false);
}

#[test]
fn synchronous_stimulus_preserves_but_never_replenishes_bootstrap_retry() {
    let model = aterm_models_core::present_retry_model();
    let mut model_state = model.init_state();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    assert!(model.fire("DropPersistent", &mut model_state));
    app.rearm_dropped_present(wid, PresentDropReason::GpuValidation);
    {
        let window = &app.windows[&wid];
        assert!(window.bootstrap_present_retry_available);
        assert_matches(&model_state, &window.present_retry, Instant::now(), true);
    }

    assert!(model.fire("SynchronousStimulus", &mut model_state));
    {
        let window = app.windows.get_mut(&wid).unwrap();
        assert!(window.present_retry.on_external_stimulus());
        assert!(window.bootstrap_present_retry_available);
        assert_matches(&model_state, &window.present_retry, Instant::now(), true);
    }

    assert!(model.fire("DropBootstrapOccluded", &mut model_state));
    let before_drop = Instant::now();
    app.rearm_dropped_present(wid, PresentDropReason::GpuOccluded);
    let deadline = {
        let window = &app.windows[&wid];
        assert!(!window.bootstrap_present_retry_available);
        let deadline = window
            .present_retry
            .deadline
            .expect("the preserved bootstrap permit arms one future retry");
        assert!(deadline > before_drop);
        assert_matches(&model_state, &window.present_retry, before_drop, false);
        deadline
    };

    assert!(model.fire("Wake", &mut model_state));
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .take_due(deadline)
    );
    assert!(model.fire("SynchronousStimulus", &mut model_state));
    {
        let window = app.windows.get_mut(&wid).unwrap();
        assert!(window.present_retry.on_external_stimulus());
        assert!(window.present_retry.recovery_redraw_outstanding);
        assert!(!window.bootstrap_present_retry_available);
        assert_matches(&model_state, &window.present_retry, deadline, false);
    }

    assert!(model.fire("DropPersistent", &mut model_state));
    app.rearm_dropped_present(wid, PresentDropReason::GpuOccluded);
    {
        let window = &app.windows[&wid];
        assert!(window.present_retry.parked);
        assert!(!window.bootstrap_present_retry_available);
        assert_matches(&model_state, &window.present_retry, deadline, false);
    }

    assert!(model.fire("SynchronousStimulus", &mut model_state));
    {
        let window = app.windows.get_mut(&wid).unwrap();
        assert!(window.present_retry.on_external_stimulus());
        assert!(!window.bootstrap_present_retry_available);
        assert_matches(&model_state, &window.present_retry, deadline, false);
    }

    assert!(model.fire("DropPersistent", &mut model_state));
    app.rearm_dropped_present(wid, PresentDropReason::GpuOccluded);
    let window = &app.windows[&wid];
    assert!(window.present_retry.parked);
    assert_eq!(window.present_retry.deadline, None);
    assert!(!window.bootstrap_present_retry_available);
    assert_matches(&model_state, &window.present_retry, Instant::now(), false);
}

#[test]
fn bounded_present_retry_conforms_to_derived_model() {
    let model = aterm_models_core::present_retry_model();
    let mut model_state = model.init_state();
    let mut retry = PresentRetry::default();
    let mut now = Instant::now();
    let mut bootstrap_available = true;
    assert_matches(&model_state, &retry, now, bootstrap_available);

    for slot in 0..PRESENT_RETRY_CAP {
        assert!(retry.present_attempt_allowed());
        assert!(model.fire("Drop", &mut model_state));
        let deadline = retry
            .on_drop(PresentDropReason::GpuTimeout, now)
            .expect("retry fuel arms a future deadline");
        assert!(!retry.present_attempt_allowed());
        assert!(
            deadline > now,
            "an autonomous retry is never immediate/past"
        );
        assert_eq!(
            deadline.duration_since(now),
            PRESENT_RETRY_BASE.saturating_mul(1u32 << u32::from(slot))
        );
        assert_matches(&model_state, &retry, now, bootstrap_available);

        assert!(model.fire("Wake", &mut model_state));
        assert!(retry.take_due(deadline));
        assert!(retry.present_attempt_allowed());
        now = deadline;
        assert_matches(&model_state, &retry, now, bootstrap_available);
    }

    assert!(model.fire("Drop", &mut model_state));
    assert_eq!(retry.on_drop(PresentDropReason::GpuTimeout, now), None);
    assert!(retry.parked);
    assert_eq!(retry.deadline, None);
    assert!(!retry.present_attempt_allowed());
    assert_matches(&model_state, &retry, now, bootstrap_available);

    assert!(model.fire("Stimulus", &mut model_state));
    let mut stimulus_requests = 0;
    assert!(rearm_present_and_request(&mut retry, false, || {
        stimulus_requests += 1;
    }));
    assert_eq!(stimulus_requests, 1);
    assert!(retry.present_attempt_allowed());
    assert_matches(&model_state, &retry, now, bootstrap_available);

    assert!(model.fire("Present", &mut model_state));
    retry.on_presented();
    bootstrap_available = false;
    assert_matches(&model_state, &retry, now, bootstrap_available);

    assert!(model.successors("Stimulus", &model_state).is_empty());
    assert!(!rearm_present_and_request(&mut retry, false, || {
        panic!("idle non-forced stimulus must not manufacture a redraw")
    }));
    assert_matches(&model_state, &retry, now, bootstrap_available);
    assert!(model.fire("ForcedStimulus", &mut model_state));
    let mut forced_requests = 0;
    assert!(rearm_present_and_request(&mut retry, true, || {
        forced_requests += 1;
    }));
    assert_eq!(forced_requests, 1);
    assert_matches(&model_state, &retry, now, bootstrap_available);
    assert!(model.fire("Present", &mut model_state));
    retry.on_presented();
    assert_matches(&model_state, &retry, now, bootstrap_available);

    assert!(model.fire("Drop", &mut model_state));
    let deadline = retry
        .on_drop(PresentDropReason::GpuTimeout, now)
        .expect("fresh retry episode arms a deadline");
    assert!(model.fire("Wake", &mut model_state));
    assert!(retry.take_due(deadline));
    now = deadline;
    assert_matches(&model_state, &retry, now, bootstrap_available);
    assert!(retry.present_attempt_allowed());
    assert_ne!(retry, PresentRetry::default());
    assert_eq!(model_state["remaining"], i64::from(PRESENT_RETRY_CAP - 1));
    assert_eq!(model_state["train"], 1);

    assert!(model.fire("Stimulus", &mut model_state));
    let mut replacement_requests = 0;
    assert!(rearm_present_and_request(&mut retry, false, || {
        replacement_requests += 1;
    }));
    assert_matches(&model_state, &retry, now, bootstrap_available);
    assert!(retry.recovery_redraw_outstanding);
    assert!(model.fire("Stimulus", &mut model_state));
    assert!(rearm_present_and_request(&mut retry, false, || {
        replacement_requests += 1;
    }));
    assert_eq!(replacement_requests, 2);
    assert_matches(&model_state, &retry, now, bootstrap_available);
    assert!(model.fire("Present", &mut model_state));
    retry.on_presented();
    assert_matches(&model_state, &retry, now, bootstrap_available);
    assert!(!rearm_present_and_request(&mut retry, false, || {
        panic!("idle state has no recovery redraw to replace")
    }));
}

#[test]
fn persistent_drop_parks_and_past_deadline_is_rejected() {
    let now = Instant::now();
    let model = aterm_models_core::present_retry_model();
    for reason in [
        PresentDropReason::GpuReconfigured,
        PresentDropReason::GpuTimeout,
        PresentDropReason::GpuUnavailable,
        PresentDropReason::CpuResize,
        PresentDropReason::CpuAcquire,
        PresentDropReason::CpuCommit,
    ] {
        let mut retry = PresentRetry::default();
        assert!(
            retry.on_drop(reason, now).is_some(),
            "{reason:?} must enter the bounded/future retry train"
        );
    }
    for reason in [
        PresentDropReason::GpuOccluded,
        PresentDropReason::GpuValidation,
        PresentDropReason::TargetMismatch,
    ] {
        let mut model_state = model.init_state();
        let mut retry = PresentRetry::default();
        assert!(model.fire("DropPersistent", &mut model_state));
        assert_eq!(retry.on_drop(reason, now), None);
        assert!(retry.parked);
        assert_eq!(retry.deadline, None);
        assert_matches(&model_state, &retry, now, true);
    }

    let immediate = PresentRetry {
        deadline: Some(now - Duration::from_nanos(1)),
        ..PresentRetry::default()
    };
    let bad = project(&immediate, now, true);
    assert_eq!(bad["retry"], 2);
    assert!(
        !model.check_invariant("RetryDeadlineIsStrictlyFuture", &bad),
        "negative control: the old immediate/past retry must be rejected"
    );
}

#[test]
fn parked_input_state_rearms_only_on_external_input_intent() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows
        .get_mut(&wid)
        .unwrap()
        .present_retry
        .on_drop(PresentDropReason::GpuOccluded, Instant::now());
    assert!(app.windows[&wid].present_retry.parked);

    let release = InputEvent::Key {
        key: Key::Character('x'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Release,
    };
    let _ = app.input(wid, release, Source::Human);
    assert!(app.windows[&wid].present_retry.parked);

    for buttons in [3, 0] {
        let motion = InputEvent::MouseMove {
            buttons,
            row: 0,
            col: 0,
            mods: 0,
            side: SelectionSide::Left,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        let _ = app.input(wid, motion, Source::Human);
        assert!(
            app.windows[&wid].present_retry.parked,
            "pointer motion (buttons={buttons}) must not mint retry fuel"
        );
    }

    let press = InputEvent::Key {
        key: Key::Character('x'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let _ = app.input(wid, press, Source::Human);
    let retry = app.windows[&wid].present_retry;
    assert!(retry.recovery_redraw_outstanding);
    assert!(retry.present_attempt_allowed());
}

#[test]
fn earliest_deadline_retains_its_diagnostic_owner() {
    let now = Instant::now();
    let mut deadline = None;
    let mut owner = crate::deadline_heal::DeadlineFold::default();
    fold_owned_deadline(
        &mut deadline,
        &mut owner,
        now + Duration::from_secs(2),
        DeadlineOwner::Blink,
    );
    fold_owned_deadline(
        &mut deadline,
        &mut owner,
        now + Duration::from_millis(16),
        DeadlineOwner::PresentRetry,
    );
    assert_eq!(deadline, Some(now + Duration::from_millis(16)));
    assert_eq!(owner.owner(), DeadlineOwner::PresentRetry);

    fold_owned_deadline(
        &mut deadline,
        &mut owner,
        now + Duration::from_millis(16),
        DeadlineOwner::Predictor,
    );
    assert_eq!(owner.owner(), DeadlineOwner::PresentRetry);
    fold_owned_deadline(
        &mut deadline,
        &mut owner,
        now + Duration::from_millis(8),
        DeadlineOwner::TitleDrift,
    );
    assert_eq!(deadline, Some(now + Duration::from_millis(8)));
    assert_eq!(owner.owner(), DeadlineOwner::TitleDrift);
}

#[test]
fn a_paint_the_window_owes_presents_the_unchanged_frame() {
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.redraw_window(wid);
    let (size, frame) = glass(&mut app);
    assert_eq!(surface(&mut app).commits(), 1);

    app.note_owed_repaint(wid);
    app.redraw_window(wid);
    assert_eq!(
        surface(&mut app).commits(),
        1,
        "a paint with nothing owed and nothing changed presents nothing"
    );

    surface(&mut app).owe_repaint(size.1 - 12..size.1);
    assert_ne!(glass(&mut app).1, frame, "the gained rows hold white");
    app.note_owed_repaint(wid);
    app.redraw_window(wid);
    assert_eq!(surface(&mut app).commits(), 2, "the owed paint presented");
    assert_eq!(
        glass(&mut app),
        (size, frame),
        "the gained rows hold the frame"
    );
    assert!(!app.windows[&wid].present_retry.recovery_redraw_outstanding);
}

#[cfg(target_os = "linux")]
#[test]
fn an_x11_exposure_presents_the_whole_unchanged_frame() {
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.redraw_window(wid);
    let (size, frame) = glass(&mut app);
    assert_eq!(surface(&mut app).commits(), 1);

    let redraw = |app: &mut App| {
        app.note_owed_repaint(wid);
        app.redraw_window(wid);
    };
    redraw(&mut app);
    assert_eq!(
        surface(&mut app).commits(),
        1,
        "aterm's redraw of an unchanged frame presents nothing"
    );

    surface(&mut app).lose_glass();
    aterm_window::platform::x11::dispatch_exposure(|| redraw(&mut app));
    assert_eq!(surface(&mut app).commits(), 2, "the exposure presented");
    assert_eq!(
        glass(&mut app),
        (size, frame),
        "the whole frame is back on the glass"
    );
    assert!(!app.windows[&wid].present_retry.recovery_redraw_outstanding);

    redraw(&mut app);
    assert_eq!(
        surface(&mut app).commits(),
        2,
        "the next redraw of the unchanged frame is gated again"
    );
}

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

/// How the dropped frame fails.
#[derive(Clone, Copy, Debug)]
enum Drop {
    /// A real scripted CPU-surface failure, through `cpu_surface_transaction`.
    Cpu(crate::present::scripted::ScriptedOutcome),
    /// A GPU-surface outcome. A unit test has no GPU, so the frame drops at the scripted
    /// CPU acquire (the composed, damage-consumed, nothing-on-glass state a failed GPU
    /// present leaves) and the retry is the one `rearm_dropped_present` arms for it.
    Gpu(PresentDropReason),
}

/// Every consumed echo is presented after a recoverable failure: glass is established,
/// an echo's frame consumes the damage and drops, and with no new PTY byte the retry
/// (a due deadline, or a stimulus when parked) puts it on glass pixel-for-pixel equal
/// to a control. Redraws before the retry is due stay gated.
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

        // Not yet due: a redraw (an animation tick, a stray request) is
        // gated and presents nothing.
        app.redraw_window(wid);
        assert_eq!(
            surface(&mut app).commits(),
            1,
            "{case:?}: gated before the retry"
        );

        // Fire the retry exactly as the event loop does.
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

    // The production caller invokes this transition before inspecting
    // StartCause, so this is the WaitCancelled/proxy-pressure path too.
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
    // The host increments `autonomous_retries` when it schedules; the model spends
    // `remaining` when that deadline wakes. Adding the pending slot back aligns them.
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

    // Negative control: routing a lost device to the surface retry is the forbidden
    // state, so the binding is not a vacuous enum comparison.
    let mut deleted = state;
    deleted.insert("route", 1);
    assert!(!model.check_invariant("LostUsesFallback", &deleted));
}

/// Tier-1 success binding for the routed production transaction: stand-ins for the OS
/// target and `request_redraw`, but the real method replaces the backend, classifies
/// the source target and updates its `PresentRetry`.
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
        // The seam begins after the route observed a live GPU loss; a CPU-backed
        // fixture suffices because the transaction replaces the backend with a freshly
        // built CPU renderer.
        app.use_gpu = true;
        // A `--scale 2` headless boot seal: nothing re-tunes a window without
        // glass (`apply_window_scale`), so the swap itself must keep it.
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
                        // The production redraw half, over a counting
                        // stand-in for the window this fixture lacks.
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
        // Negative control: `SourceReadyRequested` without its `request_redraw` leaves a
        // ready CPU target nothing will present.
        let mut frozen = state.clone();
        frozen.insert("requested", 0);
        assert!(!model.check_invariant("ReadyCpuOwnsRedrawUntilPresent", &frozen));

        // A successful CPU commit is the only acknowledgement edge in both
        // model and shipping retry state. The platform buffer itself is the
        // sole excluded OS seam in this deterministic conformance drive.
        assert!(model.fire("PresentCpu", &mut state));
        app.windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .on_presented();
        assert_eq!(state["cpu_presented"], 1);
        assert_eq!(app.windows[&wid].present_retry, PresentRetry::default());
    }

    // Load-bearing classification control: installing the global CPU
    // renderer is not success when the source target constructor fails.
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

/// `GpuLossRecovery`'s `reason`: 0 none, 1 the source surface's own drop,
/// 2 the failed CPU fallback (`CpuAcquire`, the builder the recovery ran).
fn recovery_reason_code(reason: PresentDropReason) -> i64 {
    match reason {
        PresentDropReason::None => 0,
        PresentDropReason::CpuAcquire => 2,
        _ => 1,
    }
}

/// Tier-1 for the exact post-route transaction: drive the production
/// accounting/abort/retry method with an injected CPU-builder failure for
/// both notification entry paths, then deliver its real future deadline.
#[test]
fn failed_gpu_recovery_retries_both_entry_paths_and_counts_once() {
    let model = aterm_models_core::gpu_loss_recovery_model();
    let wid = WindowId(0);

    // A successful present resets prior retry history before the device-lost callback
    // is sampled: start exhausted, present, then run the post-present
    // (`failed_reason=None`) transaction.
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
    // Negative control: a `take_due` without `self.deadline = None` hands
    // the wake over and keeps the spent deadline armed.
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

    // A failed-present entry with fuel refines the already-counted source
    // drop and schedules the same typed retry.
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
    // Negative control: the Update path refining the accounting but keeping
    // the source surface's reason never names the failed CPU fallback.
    let mut undiagnosed = after_drop.clone();
    undiagnosed.insert(
        "reason",
        recovery_reason_code(PresentDropReason::GpuTimeout),
    );
    assert!(!model.check_invariant("FailedFallbackIsDiagnosed", &undiagnosed));

    // A failed-present path that already spent all retry fuel parks; the model claims
    // no autonomous wake for it.
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
    // Negative control: spent fuel treated as a fresh drop arms a retry
    // past the cap instead of parking.
    let mut unparked = exhausted.clone();
    unparked.insert("retry", 1);
    unparked.insert("parked", 0);
    assert!(!model.check_invariant("ExhaustedFailureIsParked", &unparked));

    // Negative controls: no retry after a failed fallback, and one frame's drop counted
    // twice, both violate the model the helper just matched.
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

    // Suppression does not acknowledge recovery; only a real present clears both the
    // model and production acknowledgement bits.
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

    // A due retry was consumed and its winit redraw suppressed: the gate is open, but
    // the non-default episode still makes the next input or focus deliver another edge.
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

    // A truly idle default episode does not manufacture redraws, while a
    // real monitor/surface transition explicitly forces one.
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

    // A new CPU target has only requested its first redraw; keep that unresolved so a
    // suppressed request is replaced by the next stimulus.
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

/// Tier-1 binding for the macOS cold-start seam: drive the real App drop
/// reducer so one pristine per-window `GpuOccluded` result arms a future
/// retry, consumes its permit, and a second result parks.
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

/// Tier-1 binding for the other one-way edge: the genuine window-success
/// reducer closes bootstrap admission, and a later external stimulus cannot
/// replenish it or reclassify an occlusion as a startup retry.
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

/// Tier-1 binding for synchronous capture/internal redraws, whose shipping
/// call sites reset retry state directly and present in the same operation
/// without creating a new winit request. A request already in flight stays
/// outstanding until present/drop acknowledges it.
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

/// Tier-1: drive the genuine per-window retry state through every model
/// transition. Each deadline is future, delay doubles, the finite train
/// parks, an external stimulus grants a fresh finite train, and success
/// resets to idle.
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

    // Retry fuel is empty: this drop parks and owns no deadline.
    assert!(model.fire("Drop", &mut model_state));
    assert_eq!(retry.on_drop(PresentDropReason::GpuTimeout, now), None);
    assert!(retry.parked);
    assert_eq!(retry.deadline, None);
    assert!(!retry.present_attempt_allowed());
    assert_matches(&model_state, &retry, now, bootstrap_available);

    // A real surface stimulus starts a fresh bounded episode.
    assert!(model.fire("Stimulus", &mut model_state));
    let mut stimulus_requests = 0;
    assert!(rearm_present_and_request(&mut retry, false, || {
        stimulus_requests += 1;
    }));
    assert_eq!(stimulus_requests, 1);
    assert!(retry.present_attempt_allowed());
    assert_matches(&model_state, &retry, now, bootstrap_available);

    // And a successful present returns to the same idle projection.
    assert!(model.fire("Present", &mut model_state));
    retry.on_presented();
    bootstrap_available = false;
    assert_matches(&model_state, &retry, now, bootstrap_available);

    // Ordinary input while idle is a no-op in both; a genuine surface/expose event is
    // the forced edge and owns an outstanding redraw at once.
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

    // A stimulus while ready: first consume one autonomous wake so state is not idle,
    // then require Stimulus to restore the default episode and report a redraw.
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

/// Non-transient surface conditions park immediately. Negative control: an immediate
/// (past) retry projects as `retry=2`, which the derived invariant rejects.
#[test]
fn persistent_drop_parks_and_past_deadline_is_rejected() {
    let now = Instant::now();
    let model = aterm_models_core::present_retry_model();
    // Every transient production reason, including CPU-target construction
    // after GPU fallback, must take the model's bounded Drop route.
    for reason in [
        PresentDropReason::GpuReconfigured,
        PresentDropReason::GpuTimeout,
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

    // A release is bookkeeping for earlier input, not a new recovery
    // stimulus, so it cannot silently mint another retry episode.
    let release = InputEvent::Key {
        key: Key::Character('x'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Release,
    };
    let _ = app.input(wid, release, Source::Human);
    assert!(app.windows[&wid].present_retry.parked);

    // Hover and held-button motion can arrive once per pixel: neither may refill the
    // finite retry train while the drawable is unavailable; a discrete click, key or
    // wheel is the recovery stimulus.
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

    // The next genuine press is explicit user intent and must open exactly
    // one fresh bounded episode, so a recovered drawable cannot stay frozen.
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
    let mut owner = DeadlineOwner::None;
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
    assert_eq!(owner, DeadlineOwner::PresentRetry);

    // Equal candidates preserve the first stable label without changing
    // the min-folded instant.
    fold_owned_deadline(
        &mut deadline,
        &mut owner,
        now + Duration::from_millis(16),
        DeadlineOwner::Predictor,
    );
    assert_eq!(owner, DeadlineOwner::PresentRetry);
    // Title/cwd drift folds under its own owner.
    fold_owned_deadline(
        &mut deadline,
        &mut owner,
        now + Duration::from_millis(8),
        DeadlineOwner::TitleDrift,
    );
    assert_eq!(deadline, Some(now + Duration::from_millis(8)));
    assert_eq!(owner, DeadlineOwner::TitleDrift);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    App, CaptureRequest, DEFERRED_GPU_CAPTURE_LIMIT, DeferredGpuCapture,
    deferred_capture_identity_matches, enqueue_deferred_gpu_capture,
};
use crate::control::{CaptureCancellation, ImageReq, ReplyRetentionPermit};
use crate::{WindowId, control_auth};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock, mpsc};

fn capture(
    surface: u64,
) -> (
    DeferredGpuCapture,
    mpsc::Receiver<crate::control::ImageReply>,
    CaptureCancellation,
) {
    let (reply, receiver) = mpsc::channel();
    let cancel = CaptureCancellation::new();
    (
        DeferredGpuCapture {
            window: WindowId(0),
            surface,
            exact_tap: false,
            budget: super::CapturePresentBudget::default(),
            request: CaptureRequest::Image(ImageReq {
                target: control_auth::ConfinedImage::in_memory(),
                clean: false,
                session: None,
                want_bytes: true,
                want_metadata: false,
                frame_metadata: Arc::new(OnceLock::new()),
                cancel: cancel.clone(),
                reply,
                handoff: ReplyRetentionPermit::from_own_budget(),
            }),
        },
        receiver,
        cancel,
    )
}

fn capture_queue_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        DeferredGpuCaptureQueue {
            const Buggy = 0;
            const Cap = 8;
            var queued = 0;
            var current = 1;
            var cancelled = 0;
            var resumed = 0;
            action Push when (queued <= Cap) {
                queued = if queued <= Cap - 1 || Buggy == 1 { queued + 1 } else { queued };
            }
            action Close { current = 0; resumed = 0; }
            action Replace { current = 2; resumed = 0; }
            action Restore { current = 1; cancelled = 0; resumed = 0; }
            action Cancel { cancelled = 1; resumed = 0; }
            action Resume when (queued > 0 && (Buggy == 1 || (current == 1 && cancelled == 0))) {
                queued = queued - 1;
                resumed = 1;
            }
            invariant BoundedOwnership: queued <= Cap;
            invariant ResumeNeedsCurrentLiveRequest: resumed == 0 || (current == 1 && cancelled == 0);
        }
    }
}

#[test]
fn deferred_gpu_capture_queue_real_admission_and_identity_conform() {
    let model = capture_queue_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let mut state = model.init_state();
    let mut queue = VecDeque::new();
    let mut receivers = Vec::new();
    for surface in 1..=DEFERRED_GPU_CAPTURE_LIMIT + 1 {
        let (request, receiver, _) = capture(surface as u64);
        let admitted = enqueue_deferred_gpu_capture(&mut queue, request, false);
        assert!(model.fire("Push", &mut state));
        assert_eq!(queue.len() as i64, state["queued"]);
        assert!(model.check_invariant("BoundedOwnership", &state));
        if surface <= DEFERRED_GPU_CAPTURE_LIMIT {
            assert!(admitted.is_ok());
            assert!(
                matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "pending retains the original reply without answering early"
            );
            receivers.push(receiver);
        } else {
            let rejected = match admitted {
                Err(request) => request,
                Ok(()) => panic!("overflow was admitted"),
            };
            rejected.request.reject("queue full");
            assert_eq!(receiver.recv().unwrap().unwrap_err(), "queue full");
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            ));
        }
    }
    let head = queue.pop_front().unwrap();
    assert_eq!(head.surface, 1);
    assert!(enqueue_deferred_gpu_capture(&mut queue, head, true).is_ok());
    for (index, receiver) in receivers.into_iter().enumerate() {
        let request = queue.pop_front().unwrap();
        assert_eq!(
            request.surface,
            (index + 1) as u64,
            "later requests cannot overtake a resumed head"
        );
        request.request.reject("finished once");
        assert_eq!(receiver.recv().unwrap().unwrap_err(), "finished once");
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
    for current in [None, Some(1), Some(2)] {
        for cancelled in [false, true] {
            let mut observed = model.init_state();
            observed.insert("queued", 1);
            observed.insert("current", current.unwrap_or(0) as i64);
            observed.insert("cancelled", i64::from(cancelled));
            assert_eq!(
                deferred_capture_identity_matches(1, current, cancelled),
                model.action_enabled("Resume", &observed),
            );
        }
    }
    let mut bad = model.init_state();
    bad.insert("queued", DEFERRED_GPU_CAPTURE_LIMIT as i64 + 1);
    assert!(
        !model.check_invariant("BoundedOwnership", &bad),
        "negative control: an unbounded queue must fail"
    );
    bad.insert("queued", 1);
    bad.insert("current", 2);
    bad.insert("resumed", 1);
    assert!(
        !model.check_invariant("ResumeNeedsCurrentLiveRequest", &bad),
        "negative control: completion on a replaced surface must fail"
    );
}

fn capture_resume_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        CaptureReadyRedrawOwnership {
            const Buggy = 0;
            var submitted = 0;
            var pending = 0;
            var retry_open = 1;
            var decided = 0;
            var redraw = 0;
            action Reject { submitted = 0; pending = 0; retry_open = 1; decided = 0; }
            action Present { submitted = 1; pending = 0; retry_open = 1; decided = 0; }
            action Wait { submitted = 0; pending = 1; retry_open = 1; decided = 0; }
            action Fail { submitted = 0; pending = 0; retry_open = 0; decided = 0; }
            action Decide {
                decided = 1;
                redraw = if Buggy == 1 { 0 } else {
                    if submitted == 0 && pending == 0 && retry_open == 1 { 1 } else { 0 }
                };
            }
            invariant RejectionCannotStrandReadyFrame:
                decided == 0 || submitted == 1 || pending == 1 || retry_open == 0 || redraw == 1;
        }
    }
}

#[test]
fn capture_resume_rejection_redraw_guard_conforms_and_catches_stranding() {
    let model = capture_resume_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    for (action, after, pending, retry_open) in [
        ("Reject", 10, false, true),
        ("Present", 11, false, true),
        ("Wait", 10, true, true),
        ("Fail", 10, false, false),
    ] {
        let mut state = model.init_state();
        assert!(model.fire(action, &mut state));
        assert!(model.fire("Decide", &mut state));
        let redraw = !super::capture_resume_owns_next_frame(10, after, pending, retry_open);
        assert_eq!(i64::from(redraw), state["redraw"], "{action}");
    }
    let mut rejected = model.init_state();
    assert!(model.fire("Reject", &mut rejected));
    assert!(model.fire("Decide", &mut rejected));
    rejected.insert("redraw", 0);
    assert!(
        !model.check_invariant("RejectionCannotStrandReadyFrame", &rejected),
        "negative control: merely calling a rejected capture is not presentation progress"
    );
}

fn capture_failure_budget_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        CaptureCompletedFailureBudget {
            const Buggy = 0;
            var remaining = 3;
            var failures = 0;
            var waiting = 0;
            var done = 0;
            action Demand when (remaining > 0 && waiting == 0 && done == 0) {
                remaining = if Buggy == 1 { 3 } else { remaining };
                waiting = 1;
            }
            action Timeout when (waiting == 1) {
                remaining = remaining - 1;
                failures = failures + 1;
                waiting = 0;
                done = if remaining <= 1 { 1 } else { 0 };
            }
            action Success when (waiting == 1) { waiting = 0; done = 1; }
            invariant OriginalBudgetConserved: remaining + failures == 3;
            invariant CompletedFailuresBounded: failures <= 3;
            invariant ExhaustionTerminates: remaining > 0 || done == 1;
        }
    }
}

#[test]
fn original_capture_failure_budget_survives_pending_and_requeue() {
    let model = capture_failure_budget_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let mut state = model.init_state();
    let mut queue = VecDeque::new();
    let (mut original, original_reply, _) = capture(17);
    let (follower, follower_reply, _) = capture(18);
    queue.push_back(follower);
    for completed in 1..=crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT {
        assert!(model.fire("Demand", &mut state));
        let budget_before = original.budget;
        original.budget.record_attempt(true, false, true);
        assert_eq!(original.budget, budget_before);
        assert!(enqueue_deferred_gpu_capture(&mut queue, original, true).is_ok());
        original = queue.pop_front().unwrap();
        assert_eq!(
            original.surface, 17,
            "replaying head retains its ownership before followers"
        );
        assert_eq!(i64::from(original.budget.remaining), state["remaining"]);
        assert!(
            matches!(original_reply.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "the same caller still awaits this acquisition"
        );

        original.budget.record_attempt(false, false, true);
        assert!(model.fire("Timeout", &mut state));
        assert_eq!(i64::from(original.budget.remaining), state["remaining"]);
        assert_eq!(i64::from(completed), state["failures"]);
        assert!(model.check_invariant("OriginalBudgetConserved", &state));
    }
    assert_eq!(original.budget.remaining, 0);
    assert!(
        !crate::run_capture_present_barrier(original.budget.remaining, || {
            panic!("exhausted original request attempted another acquisition")
        }),
        "the real capture barrier must not execute at budget zero"
    );
    assert!(
        !model.action_enabled("Demand", &state),
        "snapshot has no caller timer: the exhausted original budget itself must stop retries"
    );
    original
        .request
        .reject("capture exhausted its completed present attempts");
    assert!(original_reply.recv().unwrap().is_err());
    assert!(matches!(
        original_reply.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    assert_eq!(
        queue.front().unwrap().surface,
        18,
        "exhausted head no longer starves its live follower"
    );
    assert!(matches!(
        follower_reply.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let mut bad = state.clone();
    bad.insert("remaining", 3);
    assert!(
        !model.check_invariant("OriginalBudgetConserved", &bad),
        "negative control: resetting the budget on replay recreates unlimited timeouts"
    );
    let mut success = super::CapturePresentBudget::default();
    success.record_attempt(false, true, true);
    assert_eq!(
        success,
        super::CapturePresentBudget::default(),
        "a successful serial is never charged as failure"
    );
}

#[test]
fn deferred_gpu_capture_original_timeout_and_closed_window_reply_once() {
    let mut app = App::headless_for_test();
    let (request, receiver, cancellation) = capture(17);
    assert!(enqueue_deferred_gpu_capture(&mut app.deferred_gpu_captures, request, false).is_ok());
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(cancellation.cancel());
    assert!(
        app.deferred_gpu_captures[0]
            .request
            .cancellation()
            .is_some(),
        "the original caller cancellation remains attached"
    );
    app.prune_deferred_gpu_captures();
    assert!(receiver.recv().unwrap().is_err());
    app.prune_deferred_gpu_captures();
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    assert!(app.deferred_gpu_captures.is_empty());

    let (request, receiver, _) = capture(17);
    app.deferred_gpu_captures.push_back(request);
    app.windows.remove(&WindowId(0));
    app.prune_deferred_gpu_captures();
    assert!(receiver.recv().unwrap().is_err());
    assert!(app.deferred_gpu_captures.is_empty());
}

#[test]
fn cancelling_a_queued_follower_preserves_the_heads_exact_tap() {
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WindowId(0)).unwrap();
    ws.capture_acquire_armed = true;
    ws.capture_client_requested = true;
    let (follower, receiver, cancellation) = capture(17);
    assert!(!follower.exact_tap);
    assert!(cancellation.cancel());
    app.deferred_gpu_captures.push_back(follower);
    app.prune_deferred_gpu_captures();
    assert!(receiver.recv().unwrap().is_err());
    assert!(app.windows[&WindowId(0)].capture_acquire_armed);
    assert!(app.windows[&WindowId(0)].capture_client_requested);
    let (mut owner, receiver, cancellation) = capture(17);
    owner.exact_tap = true;
    assert!(cancellation.cancel());
    app.deferred_gpu_captures.push_back(owner);
    app.prune_deferred_gpu_captures();
    assert!(receiver.recv().unwrap().is_err());
    assert!(app.windows[&WindowId(0)].capture_acquire_armed);
    assert!(app.windows[&WindowId(0)].capture_client_requested);
    assert!(!super::deferred_capture_owns_current_tap(
        false,
        17,
        Some(17)
    ));
    assert!(!super::deferred_capture_owns_current_tap(
        true,
        17,
        Some(18)
    ));
    assert!(!super::deferred_capture_owns_current_tap(true, 17, None));
    assert!(super::deferred_capture_owns_current_tap(true, 17, Some(17)));
    if super::deferred_capture_owns_current_tap(true, 17, Some(17)) {
        app.discard_presented_client_capture(WindowId(0));
    }
    assert!(!app.windows[&WindowId(0)].capture_acquire_armed);
    assert!(!app.windows[&WindowId(0)].capture_client_requested);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, GpuAcquireWait, PresentRetry, WindowId, service_due_presentation_clocks};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

fn acquire_wait_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        GpuAcquireWaitIdentity {
            const Buggy = 0;
            var current = 1;
            var pending = 0;
            var requested = 0;
            var matching = 0;
            action Begin when (current > 0) {
                pending = current;
                requested = 0;
                matching = 0;
            }
            action Replace {
                current = if current == 1 { 2 } else { 1 };
                requested = 0;
                matching = 0;
            }
            action Cpu {
                current = 0;
                requested = 0;
                matching = 0;
            }
            action ReadyFirst {
                matching = if current == 1 && pending == 1 { 1 } else { 0 };
                requested = if Buggy == 1 { 1 } else {
                    if current == 1 && pending == 1 { 1 } else { 0 }
                };
                pending = if Buggy == 1 || (current == 1 && pending == 1) { 0 } else { pending };
            }
            action ReadySecond {
                matching = if current == 2 && pending == 2 { 1 } else { 0 };
                requested = if Buggy == 1 { 1 } else {
                    if current == 2 && pending == 2 { 1 } else { 0 }
                };
                pending = if Buggy == 1 || (current == 2 && pending == 2) { 0 } else { pending };
            }
            invariant Bounds: current <= 2 && pending <= 2 && requested <= 1 && matching <= 1;
            invariant OnlyMatchingDemandWakes: requested <= matching;
        }
    }
}

#[test]
fn gpu_acquire_wait_model_proves_and_catches_stale_completion() {
    let model = acquire_wait_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn gpu_acquire_wait_real_transitions_conform_exhaustively() {
    let model = acquire_wait_model();
    let mut seen = BTreeSet::new();
    let mut todo = vec![model.init_state()];
    let mut checked = 0;
    while let Some(before) = todo.pop() {
        if !seen.insert(before.clone()) {
            continue;
        }
        for action in ["Begin", "Replace", "Cpu", "ReadyFirst", "ReadySecond"] {
            if !model.action_enabled(action, &before) {
                continue;
            }
            let mut current = u64::try_from(before["current"]).unwrap();
            let pending = u64::try_from(before["pending"]).unwrap();
            let mut real = GpuAcquireWait {
                pending_surface: (pending > 0).then_some(pending),
            };
            let mut requested = false;
            let mut matching = false;
            match action {
                "Begin" => real.begin(current),
                "Replace" => current = if current == 1 { 2 } else { 1 },
                "Cpu" => current = 0,
                "ReadyFirst" | "ReadySecond" => {
                    let reported = if action == "ReadyFirst" { 1 } else { 2 };
                    matching = current == reported && pending == reported;
                    requested = real.complete((current > 0).then_some(current), reported);
                }
                _ => unreachable!(),
            }
            let after: BTreeMap<_, _> = [
                ("current", i64::try_from(current).unwrap()),
                (
                    "pending",
                    i64::try_from(real.pending_surface.unwrap_or(0)).unwrap(),
                ),
                ("requested", i64::from(requested)),
                ("matching", i64::from(matching)),
            ]
            .into_iter()
            .collect();
            assert!(
                model.successors(action, &before).contains(&after),
                "{action}: {before:?} -> {after:?}"
            );
            assert!(model.check_invariant("OnlyMatchingDemandWakes", &after));
            todo.push(after);
            checked += 1;
        }
    }
    assert!(
        checked > 50,
        "exercise every reachable identity/ready combination"
    );
    let mut stale = model.init_state();
    stale.insert("requested", 1);
    assert!(
        !model.check_invariant("OnlyMatchingDemandWakes", &stale),
        "negative control: an unsolicited wake must fail"
    );
}

#[test]
fn gpu_acquire_pending_preserves_dirty_levels_without_timer_retries() {
    let now = Instant::now();
    let floor = Duration::from_millis(8);
    let mut retry = PresentRetry::default();
    let mut dirty = true;
    let mut acquire = GpuAcquireWait::default();
    acquire.begin(17);
    for step in 1..=100 {
        acquire.begin(17);
        assert!(!service_due_presentation_clocks(
            acquire.waiting_for(Some(17)),
            &mut retry,
            &mut dirty,
            Some(now),
            floor,
            now + floor * step,
        ));
        assert!(dirty, "a pending acquire cannot consume frame-cap demand");
        assert_eq!(
            retry,
            PresentRetry::default(),
            "no retries, deadlines, or failure fuel"
        );
    }
    // Negative control: the very same expired content clock is due once
    // the matching result arrives. It must neither spin nor be forgotten.
    assert!(!acquire.complete(Some(18), 17));
    assert!(!acquire.complete(None, 17));
    assert!(acquire.complete(Some(17), 17));
    assert!(
        !acquire.complete(Some(17), 17),
        "duplicate completion owns no demand"
    );
    assert!(service_due_presentation_clocks(
        acquire.waiting_for(Some(17)),
        &mut retry,
        &mut dirty,
        Some(now),
        floor,
        now + floor * 100,
    ));
    assert!(!dirty);
}

#[test]
fn gpu_acquire_window_completion_keeps_input_until_real_present() {
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WindowId(0)).unwrap();
    let key = {
        let mut term = ws.front_terminal().unwrap().term.lock().unwrap();
        super::early_out_tests::frame_key(&mut term, true, false, None)
    };
    let now = Instant::now();
    ws.last_present_at = Some(now);
    ws.content_pending = true;
    ws.redraw_pending = true;
    ws.input_hot = true;
    ws.input_hot_until = Some(now + Duration::from_secs(1));
    ws.stamp_present_decision(key);
    assert!(ws.last_present.is_some());
    let serial = ws.capture_present_serial;
    ws.on_gpu_acquire_pending(17);
    assert!(ws.last_present.is_none());
    let mut requests = 0;
    assert!(!ws.finish_gpu_acquire_wait(None, 17, true, || requests += 1));
    assert!(!ws.finish_gpu_acquire_wait(Some(18), 17, true, || requests += 1));
    assert_eq!(
        requests, 0,
        "CPU/replaced targets ignore the old completion"
    );
    assert!(ws.finish_gpu_acquire_wait(Some(17), 17, true, || requests += 1));
    assert_eq!(requests, 1);
    assert!(!ws.finish_gpu_acquire_wait(Some(17), 17, true, || requests += 1));
    assert_eq!(requests, 1);
    assert!(ws.content_pending && ws.redraw_pending && ws.input_hot);
    assert_eq!(ws.last_present_at, Some(now));
    assert_eq!(
        ws.capture_present_serial, serial,
        "ready did not present pixels"
    );
    assert_eq!(ws.present_retry, PresentRetry::default());
    ws.on_present_succeeded();
    ws.on_content_presented(now + Duration::from_millis(1));
    assert_eq!(ws.capture_present_serial, serial + 1);
    assert!(
        ws.input_hot,
        "an unrelated first present cannot retire a hot echo"
    );
    // A later content burst starts another independent acquisition episode.
    ws.content_pending = true;
    ws.on_gpu_acquire_pending(17);
    assert!(ws.finish_gpu_acquire_wait(Some(17), 17, true, || requests += 1));
    assert_eq!(requests, 2);
    assert!(ws.content_pending && ws.input_hot);
}

#[test]
fn gpu_acquire_wait_survives_real_input_rearming_and_later_input() {
    use crate::input::{InputEvent, Source};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let press = || InputEvent::Key {
        key: Key::Character('x'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    app.windows
        .get_mut(&wid)
        .unwrap()
        .on_gpu_acquire_pending(17);
    let _ = app.input(wid, press(), Source::Human);
    let ws = app.windows.get_mut(&wid).unwrap();
    assert!(
        ws.gpu_acquire_wait.waiting_for(Some(17)),
        "input may rearm failures, but cannot take a worker's pending drawable"
    );
    assert!(
        ws.input_hot,
        "real key egress arms echo priority while acquisition waits"
    );
    assert!(ws.finish_gpu_acquire_wait(Some(17), 17, true, || {}));
    ws.on_present_succeeded();
    let _ = app.input(wid, press(), Source::Human);
    let ws = app.windows.get_mut(&wid).unwrap();
    assert!(ws.input_hot);
    ws.on_gpu_acquire_pending(17);
    assert!(ws.finish_gpu_acquire_wait(Some(17), 17, true, || {}));
    assert!(
        ws.input_hot,
        "the second completion still is not the echo's presentation"
    );
}

#[test]
fn gpu_acquire_occluded_completion_parks_without_failure_fuel() {
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WindowId(0)).unwrap();
    ws.content_pending = true;
    ws.redraw_pending = true;
    ws.on_gpu_acquire_pending(17);
    let mut requests = 0;
    assert!(ws.finish_gpu_acquire_wait(Some(17), 17, false, || requests += 1));
    assert_eq!(requests, 0);
    assert!(ws.present_retry.parked);
    assert_eq!(ws.present_retry.autonomous_retries, 0);
    assert_eq!(ws.present_retry.deadline, None);
    assert!(ws.content_pending && ws.redraw_pending);
    assert!(!ws.gpu_acquire_wait.waiting_for(Some(17)));
    // The real expose/input rearm reopens the same finite surface gate.
    assert!(super::rearm_present_and_request(
        &mut ws.present_retry,
        true,
        || requests += 1
    ));
    assert_eq!(requests, 1);
    assert!(ws.present_retry.present_attempt_allowed());
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Metrics, PendingInputStamp, PresentClock, PresentObserver, WindowPresentState};
use aterm_models_session::dispatch_clock_ownership_model;
use aterm_spec::interp::State;
use aterm_spec::verify::{prove_and_catch_tiered, validate_transition_tiered};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct ClockTrace {
    depth: i64,
    saved: u64,
    outer_remaining: u64,
    inner_remaining: u64,
    worker_read: u64,
    worker_claim: u64,
    ui_claim: u64,
}

fn token(stamp: u64) -> i64 {
    match stamp {
        0 => 0,
        1_000 => 1,
        1_100 => 2,
        1_500 => 3,
        other => panic!("unexpected native clock stamp {other}"),
    }
}

fn ownership_transition(trace: &ClockTrace, current: u64) -> State {
    State::from([
        ("depth", trace.depth),
        ("current", token(current)),
        ("saved", token(trace.saved)),
        ("outer_remaining", token(trace.outer_remaining)),
        ("inner_remaining", token(trace.inner_remaining)),
        ("worker_read", token(trace.worker_read)),
        ("worker_claim", token(trace.worker_claim)),
        ("ui_claim", token(trace.ui_claim)),
    ])
}

fn assert_ownership(action: &str, before: &State, after: &State, admitted: bool) {
    let model = dispatch_clock_ownership_model();
    let label = format!("dispatch clock {action} admitted={admitted}");
    let (actual, evidence) =
        validate_transition_tiered(&model, &[], before, after, Some(action), &label);
    eprintln!("{label}: {evidence}");
    assert_eq!(actual, admitted, "{label}: {after:?}");
    if admitted {
        for law in &model.invariants {
            assert!(model.check_invariant(law.name, after), "{}", law.name);
        }
    }
}

fn record(trace: &ClockTrace, before: &mut State, action: &str, current: u64) {
    let after = ownership_transition(trace, current);
    assert_ownership(action, before, &after, true);
    *before = after;
}

fn initial(metrics: &Metrics) -> (ClockTrace, State) {
    let trace = ClockTrace {
        worker_read: metrics.input_arrival_ns(1_500),
        ..ClockTrace::default()
    };
    let state = ownership_transition(&trace, metrics.key_arrival_ns());
    assert_eq!(state, dispatch_clock_ownership_model().init_state());
    (trace, state)
}

fn old_shared_atomic_control() {
    let metrics = Metrics::new();
    let (mut trace, mut state) = initial(&metrics);
    let shared = AtomicU64::new(1_000);
    trace.depth = 1;
    trace.outer_remaining = shared.load(Ordering::Relaxed);
    record(
        &trace,
        &mut state,
        "EnterOuter",
        shared.load(Ordering::Relaxed),
    );
    let (observed, after_peek, claimed, after_claim) = std::thread::scope(|threads| {
        threads
            .spawn(|| {
                let key = shared.load(Ordering::Relaxed);
                let observed = if key != 0 && 1_500u64.saturating_sub(key) < 500_000_000 {
                    key
                } else {
                    1_500
                };
                let after_peek = shared.load(Ordering::Relaxed);
                let claimed = shared.swap(0, Ordering::Relaxed);
                (
                    observed,
                    after_peek,
                    claimed,
                    shared.load(Ordering::Relaxed),
                )
            })
            .join()
            .expect("old shared-clock observer")
    });
    assert_eq!((observed, claimed), (1_000, 1_000));
    trace.worker_read = observed;
    let after = ownership_transition(&trace, after_peek);
    assert_ownership("PeekWorker", &state, &after, false);
    state = after;
    trace.worker_claim = claimed;
    let after = ownership_transition(&trace, after_claim);
    assert_ownership("ClaimWorker", &state, &after, false);
    assert_eq!(
        shared.swap(0, Ordering::Relaxed),
        0,
        "the owner lost its stamp"
    );
}

#[test]
fn background_input_cannot_borrow_or_consume_a_hardware_dispatch() {
    let coverage = prove_and_catch_tiered(
        &dispatch_clock_ownership_model(),
        "dispatch clock ownership",
    )
    .expect("dispatch clock checker coverage");
    eprintln!("dispatch clock ownership: {coverage:?}");
    let metrics = Metrics::new();
    let (mut trace, mut state) = initial(&metrics);
    metrics.with_key_arrival_at(1_000, true, || {
        trace.depth = 1;
        trace.outer_remaining = metrics.key_arrival_ns();
        record(&trace, &mut state, "EnterOuter", metrics.key_arrival_ns());
        let observed = std::thread::scope(|threads| {
            threads
                .spawn(|| {
                    let mut pending = PendingInputStamp::default();
                    metrics.note_window_input(&mut pending, 1_500);
                    metrics.note_session_input(1_500);
                    (
                        pending.edges[0].arrival_ns,
                        metrics.window.input.session_stamp.take(),
                        metrics.take_key_arrival(),
                    )
                })
                .join()
                .expect("background observer")
        });
        assert_eq!(observed, (1_500, Some(1_500), 0));
        trace.worker_read = observed.0;
        record(&trace, &mut state, "PeekWorker", metrics.key_arrival_ns());
        trace.worker_claim = observed.2;
        record(&trace, &mut state, "ClaimWorker", metrics.key_arrival_ns());
        trace.ui_claim = metrics.take_key_arrival();
        trace.outer_remaining = metrics.key_arrival_ns();
        record(&trace, &mut state, "ClaimUi", metrics.key_arrival_ns());
        assert_eq!(metrics.take_key_arrival(), 0);
    });
    trace.depth = 0;
    trace.outer_remaining = 0;
    record(&trace, &mut state, "ExitOuter", metrics.key_arrival_ns());
    old_shared_atomic_control();
}

#[test]
fn local_metrics_do_not_share_an_arrival_on_the_same_thread() {
    let first = Metrics::new();
    let second = Metrics::new();
    let (mut trace, mut state) = initial(&first);
    first.with_key_arrival_at(1_000, true, || {
        trace.depth = 1;
        trace.outer_remaining = first.key_arrival_ns();
        record(&trace, &mut state, "EnterOuter", first.key_arrival_ns());
        assert_eq!(second.input_arrival_ns(1_500), 1_500);
        assert_eq!(second.take_key_arrival(), 0);
        trace.saved = first.key_arrival_ns();
        second.with_key_arrival_at(1_100, true, || {
            trace.depth = 2;
            trace.inner_remaining = second.key_arrival_ns();
            record(&trace, &mut state, "EnterInner", second.key_arrival_ns());
            assert_eq!(first.input_arrival_ns(1_500), 1_500);
            assert_eq!(first.take_key_arrival(), 0);
            trace.ui_claim = second.take_key_arrival();
            trace.inner_remaining = second.key_arrival_ns();
            record(&trace, &mut state, "ClaimUi", second.key_arrival_ns());
        });
        trace.depth = 1;
        trace.saved = 0;
        trace.inner_remaining = 0;
        record(&trace, &mut state, "ExitInner", first.key_arrival_ns());
        trace.ui_claim = first.take_key_arrival();
        trace.outer_remaining = first.key_arrival_ns();
        record(&trace, &mut state, "ClaimUi", first.key_arrival_ns());
    });
    trace.depth = 0;
    trace.outer_remaining = 0;
    record(&trace, &mut state, "ExitOuter", first.key_arrival_ns());
    assert_eq!(second.key_arrival_ns(), 0);
}

#[test]
fn nested_pressed_and_empty_scopes_restore_only_the_outer_remaining_stamp() {
    let metrics = Metrics::new();
    let (mut trace, mut state) = initial(&metrics);
    metrics.with_key_arrival_at(1_000, true, || {
        trace.depth = 1;
        trace.outer_remaining = metrics.key_arrival_ns();
        record(&trace, &mut state, "EnterOuter", metrics.key_arrival_ns());
        trace.saved = metrics.key_arrival_ns();
        metrics.with_key_arrival_at(1_100, true, || {
            trace.depth = 2;
            trace.inner_remaining = metrics.key_arrival_ns();
            record(&trace, &mut state, "EnterInner", metrics.key_arrival_ns());
            assert_eq!(metrics.input_arrival_ns(1_500), 1_100);
            trace.ui_claim = metrics.take_key_arrival();
            trace.inner_remaining = metrics.key_arrival_ns();
            record(&trace, &mut state, "ClaimUi", metrics.key_arrival_ns());
            assert_eq!(metrics.take_key_arrival(), 0);
        });
        trace.depth = 1;
        trace.saved = 0;
        trace.inner_remaining = 0;
        record(&trace, &mut state, "ExitInner", metrics.key_arrival_ns());
        metrics.with_key_arrival_at(0, false, || {
            assert_eq!(metrics.input_arrival_ns(1_500), 1_500);
            assert_eq!(metrics.take_key_arrival(), 0);
        });
        trace.ui_claim = metrics.take_key_arrival();
        trace.outer_remaining = metrics.key_arrival_ns();
        record(&trace, &mut state, "ClaimUi", metrics.key_arrival_ns());
        trace.saved = metrics.key_arrival_ns();
        metrics.with_key_arrival_at(1_100, true, || {
            trace.depth = 2;
            trace.inner_remaining = metrics.key_arrival_ns();
            record(&trace, &mut state, "EnterInner", metrics.key_arrival_ns());
        });
        trace.depth = 1;
        trace.saved = 0;
        trace.inner_remaining = 0;
        record(&trace, &mut state, "ExitInner", metrics.key_arrival_ns());
    });
    trace.depth = 0;
    trace.outer_remaining = 0;
    record(&trace, &mut state, "ExitOuter", metrics.key_arrival_ns());
    assert_eq!(metrics.input_arrival_ns(1_500), 1_500);
}

#[test]
fn unwinding_restores_the_outer_scope_and_then_retires_it() {
    let metrics = Metrics::new();
    let escaped = std::panic::catch_unwind(|| {
        metrics.with_key_arrival_at(1_000, true, || {
            let nested = std::panic::catch_unwind(|| {
                metrics.with_key_arrival_at(1_100, true, || {
                    assert_eq!(metrics.take_key_arrival(), 1_100);
                    panic!("nested dispatch");
                });
            });
            assert!(nested.is_err());
            assert_eq!(metrics.key_arrival_ns(), 1_000);
            panic!("outer dispatch");
        });
    });
    assert!(escaped.is_err());
    assert_eq!(metrics.take_key_arrival(), 0);
}

#[test]
fn an_owned_queued_stamp_outlives_its_dispatch_without_leaving_an_ambient_stamp() {
    let metrics = Metrics::new();
    let queued = metrics.with_key_arrival_at(1_000, true, || metrics.take_key_arrival());
    assert_eq!(metrics.key_arrival_ns(), 0);
    std::thread::scope(|threads| {
        threads
            .spawn(|| {
                assert_eq!(metrics.take_key_arrival(), 0);
                assert_eq!(metrics.record_pty_write_at(queued, 1_500), Some(500));
                assert_eq!(metrics.record_pty_write_at(0, 1_600), None);
            })
            .join()
            .expect("queued writer");
    });
    assert_eq!(metrics.window.input.key_write.copy_out().n(), 1);
    assert_eq!(metrics.window.input.key_write.max(), 500);
}

#[test]
fn an_owned_hardware_arrival_keeps_a_long_os_queue_wait() {
    const MS: u64 = 1_000_000;
    let metrics = Metrics::new();
    let mut window = WindowPresentState::default();
    let arrival = 1_000;
    metrics.with_key_arrival_at(arrival, true, || {
        metrics.note_window_input(&mut window.pending_input, arrival + 700 * MS);
    });
    let booked = metrics.record_present_at(
        PresentClock {
            now: arrival + 730 * MS,
            epoch: 0,
            capture_forced: false,
            tainted: false,
        },
        1,
        1,
        Some(&mut window),
        &mut PresentObserver::default(),
    );
    assert_eq!(booked.input_booked, 1);
    assert_eq!(metrics.window.input.input.copy_out().n(), 1);
    assert_eq!(metrics.window.input.input.max(), 730 * MS);
}

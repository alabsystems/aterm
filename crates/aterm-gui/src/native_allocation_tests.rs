// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(target_os = "macos")]
#[test]
fn monitor_age_seconds_enclose_fractional_bounds_and_refuse_overflow() {
    assert_eq!(
        whole_age_seconds(Duration::from_millis(100), Duration::from_millis(900)),
        Some((0, 1))
    );
    assert_eq!(
        whole_age_seconds(Duration::from_secs(1), Duration::from_secs(2)),
        Some((1, 2))
    );
    assert_eq!(
        whole_age_seconds(Duration::from_secs(2), Duration::from_secs(1)),
        None
    );
    assert_eq!(whole_age_seconds(Duration::ZERO, Duration::MAX), None);
}
use crate::native_app::{
    ActionInvocation, AppViewState, NativeApp, NativeRuntime, ViewId, ViewMotionCx,
};
use crate::native_ui::{LogicalRect, UiKey};

#[test]
fn control_scroll_moves_allocation_without_a_pty_and_keeps_session_pins() {
    use crate::control::{NativeControlPrincipal, sessionless_front_scroll_event};
    use crate::input::{InputEvent, InputOutcome, ScrollIntent, Source};
    use crate::{App, WindowId};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let terminal = app.pool.get(0).unwrap().term.clone();
    assert_eq!(app.pool.get(0).unwrap().master, -1);
    app.open_allocation(false).unwrap();
    assert!(app.front_terminal(wid).is_none());
    assert!(app.active_handle.lock().unwrap().is_none());
    let (instance, view) = app.active_native_view(wid).unwrap();
    let cx = ViewCx {
        viewport: LogicalRect::new(0.0, 0.0, 400.0, 120.0),
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 16.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    };
    app.native_runtime
        .render(instance, view, &cx)
        .unwrap()
        .compile(cx.viewport)
        .unwrap();
    let position = |app: &App| match app.native_runtime.view_state(view).unwrap() {
        AppViewState::Allocation(state) => (state.scroll, state.limit.get()),
        _ => panic!("expected Allocation"),
    };
    assert!(
        position(&app).1 > 28.0,
        "real rendered rows must exceed the viewport"
    );
    let deliver = |app: &mut App, rest| {
        let event =
            sessionless_front_scroll_event(rest, None, false, NativeControlPrincipal::Owner)
                .unwrap()
                .unwrap();
        assert_eq!(
            app.input_to_session(
                wid,
                event,
                Source::Controller,
                None,
                crate::app_input::PressPhase::Initial,
            ),
            InputOutcome::Ok
        );
    };
    deliver(&mut app, "down");
    assert!(position(&app).0 > 0.0);
    deliver(&mut app, "top");
    assert_eq!(position(&app).0, 0.0);
    deliver(&mut app, "-1");
    assert_eq!(position(&app).0, 28.0);
    deliver(&mut app, "up");
    assert_eq!(position(&app).0, 0.0);
    deliver(&mut app, "bottom");
    assert_eq!(position(&app).0, position(&app).1);
    deliver(&mut app, "top");
    deliver(&mut app, "-2147483648");
    assert_eq!(position(&app).0, position(&app).1);
    let before = position(&app);
    for session in [0, 999] {
        assert_eq!(
            app.input_to_session(
                wid,
                InputEvent::ScrollView(ScrollIntent::Top),
                Source::Controller,
                Some(session),
                crate::app_input::PressPhase::Initial,
            ),
            InputOutcome::Ok
        );
        assert_eq!(
            position(&app),
            before,
            "hidden or stale session pins cannot scroll the foreground native app"
        );
    }
    assert_eq!(crate::term_lock(&terminal).grid().display_offset(), 0);
    assert!(app.front_terminal(wid).is_none());
}

#[test]
fn lease_state_never_invents_capacity_or_running_work() {
    let now = Instant::now();
    let mut app = AllocationApp::default();
    for state in [
        CpuObservation::Absent,
        CpuObservation::Initializing,
        CpuObservation::Unreadable("refused".into()),
        CpuObservation::Unsupported,
    ] {
        app.leases = Some(Arc::new(ObservedCpu {
            at: now,
            value: state,
        }));
        let rows = app.rows(now);
        assert!(!rows.iter().any(|(_, text)| text.contains("0 held")));
        assert!(rows.iter().any(|(_, text)| text.contains("not connected")));
    }
    app.leases = Some(Arc::new(ObservedCpu {
        at: now,
        value: CpuObservation::Ready {
            total: 2,
            external: None,
            holders: vec![aterm_capacity::CpuHolder {
                slot: 1,
                reported_pid: None,
                reported_birth: None,
            }],
        },
    }));
    let rows = app.rows(now + Duration::from_secs(20));
    assert!(
        rows.iter()
            .any(|(_, text)| text.contains("1 held · 1 available · 2 total"))
    );
    assert!(
        rows.iter()
            .any(|(_, text)| text.contains("Stale · 20s ago"))
    );
    assert!(
        rows.iter()
            .any(|(_, text)| text.contains("reported PID unknown"))
    );
    assert!(rows.iter().any(|(_, text)| text == "CPU use: unavailable"));
}

#[test]
fn admission_control_is_disabled_and_dispatch_cannot_issue_work() {
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Allocation(Box::default()))
        .unwrap();
    let view = ViewId::from_stored(712);
    runtime
        .attach_view(view, instance, AppViewState::Allocation(Box::default()))
        .unwrap();
    let cx = ViewCx {
        viewport: LogicalRect::new(0.0, 0.0, 400.0, 300.0),
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 16.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    };
    let compiled = runtime
        .render(instance, view, &cx)
        .unwrap()
        .compile(cx.viewport)
        .unwrap();
    assert!(
        compiled
            .semantic(&UiKey::new("allocation/pause"))
            .unwrap()
            .state
            .is_some_and(|state| !state.enabled)
    );
    let outcome = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("allocation/pause"),
                value: None,
            }),
        )
        .unwrap();
    assert!(outcome.effects.is_empty());
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_types::macos_setup::Plan;
use aterm_window::event::{ElementState, KeyEvent};
use aterm_window::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
};

use crate::native_app::{AppEvent, ViewId};
use crate::native_ui::UiKey;
use crate::workstation_setup::{Request, SetupAnswer, WorkstationSetupDecision};
use crate::{App, WindowId};

fn review() -> (
    App,
    WindowId,
    ViewId,
    Request,
    mpsc::Receiver<SetupAnswer>,
    Instant,
) {
    let mut app = App::headless_for_test();
    // The windowed offer controller with the headless fixture's inert ports.
    app.headless = false;
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.os_window_for_test = true;
    window.rows = 90;
    window.cols = 160;
    app.set_system_state_handlers_for_test(
        crate::system_state_host::InitialSetupHandlers::inert_for_test(),
    );
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.offer_system_state(request.clone());
    let (_, view) = app.active_native_view(wid).unwrap();
    assert!(app.commit_glassless_native_frame(wid));
    let start = Instant::now();
    app.native_runtime
        .tick_system_state_at(start, true, true, &[view]);
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::FocusChanged(Some(UiKey::new("system-state/url"))),
    )
    .unwrap();
    assert!(
        app.native_selection_text(wid).is_none(),
        "Copy has no selected text and must never reach the clipboard"
    );
    (app, wid, view, request, receive, start)
}

fn press(app: &mut App, wid: WindowId, code: KeyCode, key: Key) {
    for state in [ElementState::Pressed, ElementState::Released] {
        app.on_key(
            wid,
            KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                key.clone(),
                None,
                KeyLocation::Standard,
                state,
                false,
            ),
        );
    }
}

fn copy(app: &mut App, wid: WindowId, configured: bool) {
    let (code, key, mods) = if configured {
        app.keybindings = crate::keybinding::Keybindings::from_config(Some(
            &[("f4".to_owned(), "copy".to_owned())].into_iter().collect(),
        ));
        (
            KeyCode::F4,
            Key::Named(NamedKey::F4),
            ModifiersState::empty(),
        )
    } else {
        app.keybindings = crate::keybinding::Keybindings::from_config(None);
        (
            KeyCode::KeyC,
            Key::Character(SmolStr::new("c")),
            ModifiersState::SUPER,
        )
    };
    app.windows.get_mut(&wid).unwrap().mods = mods;
    press(app, wid, code, key);
}

fn copy_stops_review(configured: bool) {
    for controller in [false, true] {
        let (mut app, wid, view, request, receive, start) = review();
        copy(&mut app, wid, configured);
        app.native_runtime.tick_system_state_at(
            start + Duration::from_secs(60),
            true,
            true,
            &[view],
        );
        app.drain_system_state_actions();
        assert!(
            request.pending(),
            "a native Copy shortcut stops automatic Apply even with no selection"
        );
        assert!(receive.try_recv().is_err());

        // A later explicit default Apply remains usable. Physical keys are
        // human input; a client uses the supported engine input route.
        app.dispatch_native_view_event(wid, view, AppEvent::FocusChanged(None))
            .unwrap();
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        if controller {
            assert_eq!(
                app.input(
                    wid,
                    crate::input::InputEvent::Key {
                        key: aterm_types::keyboard::Key::Named(
                            aterm_types::keyboard::NamedKey::Enter,
                        ),
                        mods: aterm_types::keyboard::Modifiers::empty(),
                        base_layout: None,
                        layout_text: None,
                        event_type: aterm_types::keyboard::KeyEventType::Press,
                    },
                    crate::input::Source::Controller,
                ),
                crate::input::InputOutcome::Ok
            );
        } else {
            press(&mut app, wid, KeyCode::Enter, Key::Named(NamedKey::Enter));
        }
        let mut answer = receive.try_recv().expect("explicit keyboard Apply works");
        assert!(matches!(
            answer.decision,
            WorkstationSetupDecision::Begin(_)
        ));
        assert_eq!(
            answer.receipt.as_ref().unwrap().admission(),
            if controller {
                crate::InitialSetupAdmission::Client
            } else {
                crate::InitialSetupAdmission::Person
            }
        );
        assert!(receive.try_recv().is_err());
        answer.receipt.take().unwrap().accept();
        app.resolve_system_state_apply();
    }
}

#[test]
fn startup_review_a_configured_copy_shortcut_stops_automatic_apply() {
    copy_stops_review(true);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn startup_review_command_copy_stops_automatic_apply() {
    copy_stops_review(false);
}

#[test]
fn startup_review_copy_in_another_terminal_does_not_stop_the_review() {
    let (mut app, _, view, request, receive, start) = review();
    let other = WindowId(1);
    app.install_window_state(other, crate::stub_session(99), 38, 132);
    assert!(app.window_selection_text(other).is_none());
    copy(&mut app, other, true);
    app.native_runtime
        .tick_system_state_at(start + Duration::from_secs(30), true, true, &[view]);
    app.drain_system_state_actions();
    let mut answer = receive
        .try_recv()
        .expect("the unrelated review still expires");
    assert!(matches!(
        answer.decision,
        WorkstationSetupDecision::Begin(_)
    ));
    assert!(!request.pending());
    assert!(receive.try_recv().is_err());
    answer.receipt.take().unwrap().accept();
    app.resolve_system_state_apply();
}

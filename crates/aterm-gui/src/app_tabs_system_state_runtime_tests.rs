// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn edit_review(app: &mut App, wid: WindowId, view: crate::tab_model::ViewId) {
    use crate::native_app::{ActionInvocation, AppEvent, SemanticInput};
    for (key, value) in [
        (
            "system-state/url",
            SemanticInput::Text("http://draft.local/v1/team".into()),
        ),
        (
            "system-state/password",
            SemanticInput::Text("inert-password".into()),
        ),
    ] {
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new(key),
                value: Some(value),
            }),
        )
        .unwrap();
    }
}

fn assert_review(app: &mut App, wid: WindowId) {
    use crate::native_ui::{SemanticValue, UiKey};
    assert!(app.open_system_state_tab_in_window(wid));
    let (_, view) = app.active_native_view(wid).unwrap();
    // A new view opens no line by itself: the person opens the Secrets line to see them.
    app.dispatch_native_view_event(
        wid,
        view,
        crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("system-state/line/secrets/disclose"),
            value: None,
        }),
    )
    .unwrap();
    assert!(app.commit_glassless_native_frame(wid));
    let ui = &app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap()
        .compiled;
    // The vault's fields show once the draft is open to edit.
    for (key, value) in [
        (
            "system-state/url",
            SemanticValue::Text("http://draft.local/v1/team".into()),
        ),
        ("system-state/password", SemanticValue::Text("•".repeat(14))),
    ] {
        assert_eq!(ui.semantic(&UiKey::new(key)).unwrap().value, value);
    }
}

#[test]
fn system_state_failed_window_close_preflight_keeps_the_shared_review() {
    use crate::native_app::{ActionInvocation, AppEvent, CloseScope, TextInputEvent};
    use crate::workstation_setup::Request;
    use aterm_types::macos_setup::Plan;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 90;
    window.cols = 160;
    assert!(app.open_system_state_tab());
    let (_, review) = app.active_native_view(wid).unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let request = Request::new(Plan::default(), send);
    assert!(app.native_runtime.offer_system_state(request.clone()));
    edit_review(&mut app, wid, review);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (_, settings) = app.active_native_view(wid).unwrap();
    for event in [
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new(format!(
                "settings/set/{}",
                crate::prefs::EDIT_FONT_FAMILY
            )),
            value: None,
        }),
        AppEvent::TextInput(TextInputEvent::SelectAll),
        AppEvent::TextInput(TextInputEvent::Commit("Inert Draft Mono".into())),
    ] {
        app.dispatch_native_view_event(wid, settings, event)
            .unwrap();
    }
    assert!(
        !app.prepare_window_native_shutdown(wid, CloseScope::Window)
            .unwrap()
    );
    assert!(!app.prepare_quit_native_shutdown().unwrap());
    assert!(request.pending());
    assert!(receive.try_recv().is_err());
    assert_review(&mut app, wid);
    assert!(app.structural_invariants_ok());
}

#[test]
fn system_state_second_window_close_keeps_the_offer_until_its_final_view_lets_it_go() {
    use crate::native_app::CloseScope;
    use crate::workstation_setup::{Request, WorkstationSetupDecision};
    use aterm_types::macos_setup::Plan;
    for window_close in [false, true] {
        let mut app = App::headless_for_test();
        let first_window = WindowId(0);
        let window = app.windows.get_mut(&first_window).unwrap();
        window.rows = 90;
        window.cols = 160;
        assert!(app.open_system_state_tab());
        let (instance, first) = app.active_native_view(first_window).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let request = Request::new(Plan::default(), send);
        assert!(app.native_runtime.offer_system_state(request.clone()));
        edit_review(&mut app, first_window, first);
        app.stage_system_state_tab(first_window).unwrap();
        let second_window = app.detach_active_tab_logical().unwrap();
        assert_eq!(app.active_native_view(second_window).unwrap().0, instance);
        assert!(
            app.prepare_window_native_shutdown(second_window, CloseScope::Window)
                .unwrap()
        );
        if !window_close {
            app.close_active_native_tab(second_window).unwrap();
        }
        app.close_window_logical(second_window);
        assert!(request.pending());
        assert!(receive.try_recv().is_err());
        assert_review(&mut app, first_window);
        assert!(app.structural_invariants_ok());
        // Closing never answers (init-on-by-default design §6.1): the offer is let go
        // either way, as one no window showed, so the worker makes it again.
        if window_close {
            assert!(
                app.prepare_window_native_shutdown(first_window, CloseScope::Window)
                    .unwrap()
            );
            app.close_window_logical(first_window);
        } else {
            app.close_active_native_tab(first_window).unwrap();
        }
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(!request.pending());
        assert!(app.native_runtime.app(instance).is_none());
    }
}

#[test]
fn system_state_opens_from_control_reuses_singleton_and_reopens_without_pty_churn() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert_eq!(
        app.open_app(crate::app_control::parse_open("app system-state").unwrap())
            .unwrap(),
        "app system-state"
    );
    let original = app.active_native_view(wid).unwrap();
    let tab_count = app.windows[&wid].tab_set.len();
    assert_eq!(
        app.native_runtime
            .app(original.0)
            .unwrap()
            .vtable()
            .restore_tag,
        "system-state"
    );
    assert!(matches!(
        app.native_runtime.view_state(original.1),
        Some(crate::native_app::AppViewState::SystemState(_))
    ));
    assert!(app.open_system_state_tab());
    assert_eq!(app.active_native_view(wid), Some(original));
    assert_eq!(app.windows[&wid].tab_set.len(), tab_count);
    let descriptor = app
        .native_restore_descriptor(app.windows[&wid].tab_set.active().unwrap())
        .unwrap();
    assert_eq!(descriptor, crate::restore::NativeTabRestore::SystemState);
    app.close_active_native_tab(wid).unwrap();
    app.reopen_last_closed_tab().unwrap();
    let reopened = app.active_native_view(wid).unwrap();
    assert_ne!(original, reopened, "closed runtime identities are retired");
    assert_eq!(
        app.pool.views(0),
        Some(1),
        "native opens and restore never create a PTY"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn the_system_state_menu_row_opens_the_one_system_state_tab() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let front_kind = |app: &App| {
        let (instance, _) = app.active_native_view(wid)?;
        Some(app.native_runtime.app(instance)?.kind())
    };
    let terminal_only = app.windows[&wid].tab_set.len();
    assert_eq!(front_kind(&app), None, "a terminal tab is in front");
    app.open_system_state_from_menu();
    assert_eq!(app.pending_action_refusal, None);
    assert_eq!(
        front_kind(&app),
        Some(crate::native_app::AppKind::SystemState)
    );
    let opened = app.active_native_view(wid).unwrap();
    assert_eq!(app.windows[&wid].tab_set.len(), terminal_only + 1);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert_eq!(front_kind(&app), Some(crate::native_app::AppKind::Settings));
    app.open_system_state_from_menu();
    assert_eq!(app.pending_action_refusal, None);
    assert_eq!(app.active_native_view(wid), Some(opened));
    assert_eq!(app.windows[&wid].tab_set.len(), terminal_only + 2);
    assert!(app.structural_invariants_ok());

    app.frontmost_window = None;
    app.open_system_state_from_menu();
    let refusal = app.pending_action_refusal.take().expect("no front window");
    assert!(refusal.contains("System State"), "{refusal}");
}

#[test]
fn system_state_restore_mints_a_view_and_shares_the_existing_controller() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_system_state_tab());
    let existing = app.active_native_view(wid).unwrap();
    let descriptor = crate::restore::NativeTabRestore::SystemState;
    app.closed_recovery.tabs.push(
        crate::closed_recovery::ClosedTab {
            original_window: wid,
            original_index: 0,
            tab: crate::restore::RestoredTab::from_legacy_native(&descriptor).unwrap(),
        },
        0,
    );
    app.reopen_last_closed_tab().unwrap();
    let reopened = app.active_native_view(wid).unwrap();
    assert_eq!(
        existing.0, reopened.0,
        "one System State controller across restored views"
    );
    assert_ne!(existing.1, reopened.1, "restore must never alias a view");
    assert!(app.view_store.get(existing.1).is_some());
    assert!(app.structural_invariants_ok());
}

#[test]
fn system_state_copies_the_url_selection_and_never_the_password() {
    use crate::native_app::{ActionInvocation, AppEvent, SemanticInput, TextInputEvent};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_system_state_tab());
    let (_, view) = app.active_native_view(wid).unwrap();
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::FocusChanged(Some(crate::native_ui::UiKey::new("system-state/url"))),
    )
    .unwrap();
    app.dispatch_native_view_event(wid, view, AppEvent::TextInput(TextInputEvent::SelectAll))
        .unwrap();
    assert_eq!(
        app.native_selection_text(wid),
        Some(aterm_types::macos_setup::Plan::default().vault_url)
    );
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("system-state/password"),
            value: Some(SemanticInput::Text("synthetic_password_only".into())),
        }),
    )
    .unwrap();
    app.dispatch_native_view_event(wid, view, AppEvent::TextInput(TextInputEvent::SelectAll))
        .unwrap();
    assert_eq!(
        app.native_selection_text(wid),
        None,
        "password selection never grants clipboard text"
    );
}

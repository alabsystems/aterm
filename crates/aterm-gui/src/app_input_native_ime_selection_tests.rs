// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_app::{ActionInvocation, AppEvent, AppViewState, TextInputEvent};
use crate::native_text_input::TextSelection;

#[test]
fn native_ime_preedit_preserves_platform_byte_selection_through_the_input_route() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();
    app.on_ime_commit(wid, "aβz".to_string());
    let (_, view) = app.active_native_view(wid).unwrap();
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state_mut(view) else {
        panic!("Settings view");
    };
    state.search_input.set_selection(1, 3);
    let original = state.search_input.clone();

    for (text, cursor, expected) in [
        ("e\u{301}日本", Some((3, 3)), 4..4),
        ("e\u{301}日本", Some((3, 6)), 4..7),
        ("e\u{301}日本", Some((1, 3)), 1..4),
        ("e\u{301}日本", Some((2, 3)), 10..10),
        ("e\u{301}日本", Some((6, 3)), 10..10),
        ("e\u{301}日本", Some((0, 99)), 10..10),
        ("e\u{301}日本", None, 10..10),
        ("👩‍💻日", Some((11, 11)), 12..12),
        ("👩‍💻日", Some((4, 7)), 1..1),
        ("👩‍💻日", Some((5, 7)), 15..15),
    ] {
        app.on_ime_preedit(wid, text.to_string(), cursor);
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
            panic!("Settings view");
        };
        let projection = state.search_input.projection();
        assert_eq!(state.search_input.value(), "aβz", "preedit is transient");
        assert_eq!(projection.text, format!("a{text}z"));
        assert_eq!(projection.preedit, Some(1..1 + text.len()));
        assert_eq!(
            projection.selection,
            TextSelection {
                anchor: expected.start,
                head: expected.end
            },
            "platform selection {cursor:?} in {text:?}"
        );
    }

    let mut lost_range = original;
    lost_range.set_preedit("e\u{301}日本".to_string(), None);
    app.on_ime_preedit(wid, "e\u{301}日本".to_string(), Some((3, 3)));
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
        panic!("Settings view");
    };
    assert_ne!(
        state.search_input.projection().selection,
        lost_range.projection().selection
    );

    app.on_ime_preedit(wid, String::new(), None);
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
        panic!("Settings view");
    };
    assert!(state.search_input.preedit().is_none());
    assert_eq!(state.search_input.value(), "aβz");
    assert_eq!(state.search_input.selection().range(), 1..3);
    assert!(app.windows[&wid].preedit.is_empty());

    app.on_ime_preedit(wid, "候補".to_string(), Some((0, 3)));
    app.on_ime_commit(wid, "語".to_string());
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
        panic!("Settings view");
    };
    assert_eq!(state.search_input.value(), "a語z");
    assert_eq!(state.search_input.selection(), &TextSelection::caret(4));
    assert!(state.search_input.preedit().is_none());
    app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::Undo))
        .unwrap();
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
        panic!("Settings view");
    };
    assert_eq!(
        state.search_input.value(),
        "aβz",
        "one commit is one undo frame"
    );
    assert_eq!(state.search_input.selection().range(), 1..3);

    use crate::native_app::{NativeApp, SemanticInput};
    use aterm_window::keyboard::{KeyCode, KeyLocation, PhysicalKey, SmolStr};
    let press = |app: &mut App| {
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        for state in [ElementState::Pressed, ElementState::Released] {
            app.on_key(
                wid,
                KeyEvent::synthetic_for_test(
                    PhysicalKey::Code(KeyCode::KeyN),
                    Key::Character(SmolStr::new("n")),
                    Some(SmolStr::new("n")),
                    KeyLocation::Standard,
                    state,
                    false,
                ),
            );
        }
    };
    assert!(app.open_system_state_tab());
    // The worker's read names the plan in force; a page opened by hand
    // submits nothing before it.
    app.publish_system_state(crate::system_state::SetupStatus {
        plan: Some(aterm_types::macos_setup::Plan::default()),
        ..crate::system_state::SetupStatus::default()
    });
    let (instance, _) = app.active_native_view(wid).unwrap();
    for field in ["system-state/url", "system-state/password"] {
        app.dispatch_native_event(
            wid,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new(field),
                value: Some(SemanticInput::Text(String::new())),
            }),
        )
        .unwrap();
        let projection = |app: &App| {
            let Some(NativeApp::SystemState(model)) = app.native_runtime.app(instance) else {
                panic!("System State app");
            };
            model.field_projection(field).unwrap()
        };
        app.on_ime_preedit(wid, "候補".into(), Some((0, 3)));
        press(&mut app);
        assert_eq!(
            projection(&app).text,
            if field.ends_with("password") {
                ""
            } else {
                "候補"
            },
            "composing physical keys do not insert into {field}"
        );
        if field.ends_with("password") {
            assert!(projection(&app).preedit.is_none());
        }
        app.on_ime_commit(wid, "語".into());
        press(&mut app);
        app.on_ime_preedit(wid, "候補".into(), Some((0, 3)));
        app.on_ime_preedit(wid, String::new(), None);
        press(&mut app);
        assert_eq!(
            projection(&app).text,
            if field.ends_with("password") {
                "•••"
            } else {
                "語nn"
            },
            "committed and non-composing input reaches {field} exactly once"
        );
    }
    thread_local! {
        static JOINED: std::cell::RefCell<Vec<(aterm_types::macos_setup::Plan, Option<aterm_secrets::SecretBytes>)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    app.set_system_state_handlers_for_test(crate::system_state_host::InitialSetupHandlers {
        apply: |plan, _, password, receipt| {
            JOINED.with(|joined| joined.borrow_mut().push((plan, password)));
            receipt.accept();
            true
        },
        ..crate::system_state_host::InitialSetupHandlers::inert_for_test()
    });
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("system-state/vault/join"),
            value: None,
        }),
    )
    .unwrap();
    let mut joined = JOINED.with(|joined| std::mem::take(&mut *joined.borrow_mut()));
    assert_eq!(joined.len(), 1, "Join delivers one edited plan");
    let (plan, password) = joined.pop().unwrap();
    assert_eq!(plan.vault_url, "語nn");
    assert_eq!(password.unwrap().as_str(), Some("語nn"));
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::input::{InputEvent, InputOutcome, Source};
use crate::native_app::AppKind;
use crate::native_settings::SettingsRoute;
use crate::{App, WindowId};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

fn key(character: char, mods: Modifiers) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(character),
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

#[test]
fn native_text_key_skips_the_terminal_lock_state_rpc() {
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{Key as WinitKey, KeyCode, KeyLocation, PhysicalKey, SmolStr};

    static RPC: AtomicUsize = AtomicUsize::new(0);
    fn counting_lock_modifiers() -> Modifiers {
        RPC.fetch_add(1, Ordering::Relaxed);
        Modifiers::CAPS_LOCK
    }

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();
    app.lock_modifiers = counting_lock_modifiers;
    RPC.store(0, Ordering::Relaxed);

    app.on_key(
        wid,
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyA),
            WinitKey::Character(SmolStr::new("a")),
            Some(SmolStr::new("a")),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        ),
    );

    assert_eq!(RPC.load(Ordering::Relaxed), 0);
    let (_, view) = app.active_native_view(wid).expect("Settings view");
    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(crate::native_app::AppViewState::Settings(state))
            if state.search_input.value() == "a"
    ));
}

fn file_uri(path: &std::path::Path) -> String {
    crate::native_document_host::path_to_file_uri(path).unwrap()
}

#[test]
fn command_scope_close_keys_keep_their_originating_window() {
    use crate::keybinding::Keybindings;
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{
        Key as WinitKey, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
    };

    fn tabs(app: &App, wid: WindowId) -> Vec<(crate::tab_model::TabId, crate::tab_model::ViewId)> {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| (tab.id, tab.focus))
            .collect()
    }

    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = dir.path().join("close.txt");
    fs::write(&path, "keep these bytes\n").unwrap();
    let mut failures = Vec::new();
    for configured in [true, false] {
        if !configured && !crate::app_input::HARDCODED_SUPER_CHORDS {
            continue;
        }
        for content in ["terminal", "editor", "last tab", "closed window"] {
            let mut app = App::headless_for_test();
            let (source, other) = (WindowId(0), WindowId(1));
            match content {
                "terminal" => {
                    app.push_stub_tab(source, crate::stub_session(app.next_session_id));
                    app.push_stub_tab(source, crate::stub_session(app.next_session_id));
                    assert_eq!(app.windows[&source].tabs.active, 2);
                }
                "editor" => {
                    app.open_document_tab(AppKind::Editor, &file_uri(&path))
                        .unwrap();
                    assert_eq!(app.windows[&source].tab_set.active_index(), Some(1));
                }
                _ => {}
            }
            let target = app.windows[&source].tab_set.active_id().unwrap();
            let source_tabs = tabs(&app, source);
            app.install_window_state(other, crate::stub_session(99), 38, 132);
            assert_eq!(app.frontmost_window, Some(other));
            assert_eq!(app.windows[&other].tabs.active, 0);
            let other_tabs = tabs(&app, other);
            let other_session = app.focused_session_id(other);
            let other_active = app.windows[&other].tab_set.active_id();
            if content == "closed window" {
                app.close_window_logical(source);
                assert!(!app.windows.contains_key(&source));
            }
            app.keybindings = Keybindings::resolved(Some(
                &[("f4".to_string(), "close_tab".to_string())]
                    .into_iter()
                    .collect(),
            ));
            let (code, key, mods) = if configured {
                (
                    KeyCode::F4,
                    WinitKey::Named(NamedKey::F4),
                    ModifiersState::empty(),
                )
            } else {
                (
                    KeyCode::KeyW,
                    WinitKey::Character(SmolStr::new("w")),
                    ModifiersState::SUPER,
                )
            };
            if let Some(window) = app.windows.get_mut(&source) {
                window.mods = mods;
            }
            for state in [ElementState::Pressed, ElementState::Released] {
                app.on_key(
                    source,
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
            let source_ok = match content {
                "last tab" => app.windows.get(&source).is_some_and(|window| {
                    window.pending_close && tabs(&app, source) == source_tabs
                }),
                "closed window" => !app.windows.contains_key(&source),
                _ => app.windows.get(&source).is_some_and(|window| {
                    !window.pending_close
                        && window.tab_set.get(target).is_none()
                        && window.tab_set.len() + 1 == source_tabs.len()
                }),
            };
            let other_ok = tabs(&app, other) == other_tabs
                && app.windows[&other].tab_set.active_id() == other_active
                && app.focused_session_id(other) == other_session
                && !app.windows[&other].pending_close;
            let front_ok = app.frontmost_window == Some(other);
            let structure_ok = app.structural_invariants_ok();
            let file_ok = fs::read_to_string(&path).unwrap() == "keep these bytes\n";
            if !(source_ok && other_ok && front_ok && structure_ok && file_ok) {
                failures.push((
                    configured,
                    content,
                    source_ok,
                    other_ok,
                    front_ok,
                    structure_ok,
                    file_ok,
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "(configured, content, source closed, other untouched, front kept, structure, file): {failures:?}"
    );
}

#[test]
fn command_scope_configured_tab_navigation_keys_keep_their_originating_window() {
    check_tab_navigation_keyboard_window_scope(true);
}

#[test]
fn command_scope_hardcoded_tab_navigation_keys_keep_their_originating_window() {
    if crate::app_input::HARDCODED_SUPER_CHORDS {
        check_tab_navigation_keyboard_window_scope(false);
    }
}

fn check_tab_navigation_keyboard_window_scope(configured: bool) {
    use crate::keybinding::Keybindings;
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{
        Key as WinitKey, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
    };

    fn tabs(app: &App, wid: WindowId) -> Vec<(crate::tab_model::TabId, crate::tab_model::ViewId)> {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| (tab.id, tab.focus))
            .collect()
    }

    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = dir.path().join("navigate.txt");
    fs::write(&path, "keep these bytes\n").unwrap();
    let mut failures = Vec::new();
    for (action, start, expected, closed) in [
        ("next_tab", 0, 1, false),
        ("next_tab", 1, 2, false),
        ("next_tab", 2, 0, false),
        ("prev_tab", 0, 2, false),
        ("prev_tab", 1, 0, false),
        ("prev_tab", 2, 1, false),
        ("switch_tab_2", 0, 1, false),
        ("switch_tab_3", 1, 2, false),
        ("switch_tab_1", 2, 0, false),
        ("switch_tab_9", 1, 1, false),
        ("next_tab", 1, 1, true),
        ("prev_tab", 1, 1, true),
        ("switch_tab_1", 1, 1, true),
    ] {
        let mut app = App::headless_for_test();
        let (source, other) = (WindowId(0), WindowId(1));
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        app.push_stub_tab(source, crate::stub_session(app.next_session_id));
        assert_eq!(app.windows[&source].tab_set.len(), 3);
        assert_eq!(app.windows[&source].layouts.len(), 2);
        app.switch_tab_in(source, start);
        let source_tabs = tabs(&app, source);
        let (expected_tab, expected_view) = source_tabs[expected];
        let (expected_session, expected_native) = match app.view_store.get(expected_view) {
            Some(crate::tab_model::View::Terminal(terminal)) => (Some(terminal.session), None),
            Some(crate::tab_model::View::Native(_)) => (None, Some(expected_view)),
            None => panic!("source tab has a live view"),
        };

        app.install_window_state(other, crate::stub_session(99), 38, 132);
        app.push_stub_tab(other, crate::stub_session(app.next_session_id));
        app.switch_tab_in(other, usize::from(action == "switch_tab_1"));
        assert_eq!(app.frontmost_window, Some(other));
        let other_tabs = tabs(&app, other);
        let other_active = app.windows[&other].tab_set.active_id();
        let other_projection = app.windows[&other].tabs.active;
        let other_session = app.focused_session_id(other);
        if closed {
            app.close_window_logical(source);
            assert!(!app.windows.contains_key(&source));
        }

        app.keybindings = Keybindings::resolved(Some(
            &[("f6".to_string(), action.to_string())]
                .into_iter()
                .collect(),
        ));
        let (code, key, mods) = if configured {
            (
                KeyCode::F6,
                WinitKey::Named(NamedKey::F6),
                ModifiersState::empty(),
            )
        } else {
            let (code, text, mods) = match action {
                "next_tab" => (
                    KeyCode::BracketRight,
                    "]",
                    ModifiersState::SUPER | ModifiersState::SHIFT,
                ),
                "prev_tab" => (
                    KeyCode::BracketLeft,
                    "[",
                    ModifiersState::SUPER | ModifiersState::SHIFT,
                ),
                "switch_tab_1" => (KeyCode::Digit1, "1", ModifiersState::SUPER),
                "switch_tab_2" => (KeyCode::Digit2, "2", ModifiersState::SUPER),
                "switch_tab_3" => (KeyCode::Digit3, "3", ModifiersState::SUPER),
                "switch_tab_9" => (KeyCode::Digit9, "9", ModifiersState::SUPER),
                _ => unreachable!(),
            };
            (code, WinitKey::Character(SmolStr::new(text)), mods)
        };
        if let Some(window) = app.windows.get_mut(&source) {
            window.mods = mods;
        }
        for state in [ElementState::Pressed, ElementState::Released] {
            app.on_key(
                source,
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

        let source_ok = if closed {
            !app.windows.contains_key(&source)
        } else {
            tabs(&app, source) == source_tabs
                && app.windows[&source].tab_set.active_id() == Some(expected_tab)
                && app.focused_session_id(source) == expected_session
                && app.active_native_view(source).map(|(_, view)| view) == expected_native
        };
        let other_ok = tabs(&app, other) == other_tabs
            && app.windows[&other].tab_set.active_id() == other_active
            && app.windows[&other].tabs.active == other_projection
            && app.focused_session_id(other) == other_session;
        let front_ok = app.frontmost_window == Some(other);
        let structure_ok = app.structural_invariants_ok();
        let file_ok = fs::read_to_string(&path).unwrap() == "keep these bytes\n";
        if !(source_ok && other_ok && front_ok && structure_ok && file_ok) {
            failures.push((
                action,
                start,
                expected,
                closed,
                source_ok,
                other_ok,
                front_ok,
                structure_ok,
                file_ok,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "(action, start, expected, closed, source selected, other untouched, front kept, structure, file): {failures:?}"
    );
}

#[test]
fn configured_find_keys_navigate_the_editor_before_and_after_search_is_accepted() {
    use crate::keybinding::Keybindings;
    use crate::native_app::AppViewState;
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{
        Key as WinitKey, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
    };

    fn press(app: &mut App, wid: WindowId, code: KeyCode, key: WinitKey, mods: ModifiersState) {
        app.windows.get_mut(&wid).unwrap().mods = mods;
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

    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = dir.path().join("find.txt");
    let source = "α needle β needle γ needle";
    fs::write(&path, source).unwrap();
    let matches: Vec<_> = source
        .match_indices("needle")
        .map(|(at, text)| at..at + text.len())
        .collect();
    for (next, previous, code, key, mods) in [
        (
            "f3",
            "shift+f3",
            KeyCode::F3,
            WinitKey::Named(NamedKey::F3),
            ModifiersState::empty(),
        ),
        (
            "cmd+g",
            "cmd+shift+g",
            KeyCode::KeyG,
            WinitKey::Character(SmolStr::new("g")),
            ModifiersState::SUPER,
        ),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.keybindings = Keybindings::resolved(Some(
            &[
                (next.to_string(), "find_next".to_string()),
                (previous.to_string(), "find_prev".to_string()),
            ]
            .into_iter()
            .collect(),
        ));
        app.search_last_query = "terminal-only".to_string();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let original = app.document_store.snapshot(document).unwrap();
        let selected = |app: &App| {
            let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
                panic!("editor view");
            };
            state.buffer.as_ref().unwrap().primary_selection().range()
        };
        press(&mut app, wid, code, key.clone(), mods);
        assert_eq!(selected(&app), 0..0);
        app.find_requested();
        assert_eq!(
            app.input(wid, InputEvent::Text("needle".to_string()), Source::Human),
            InputOutcome::Ok
        );
        assert_eq!(selected(&app), matches[0]);
        for accepted in [false, true] {
            if accepted {
                assert_eq!(
                    app.input(
                        wid,
                        InputEvent::Key {
                            key: Key::Named(aterm_types::keyboard::NamedKey::Enter),
                            mods: Modifiers::empty(),
                            base_layout: None,
                            layout_text: None,
                            event_type: KeyEventType::Press,
                        },
                        Source::Human
                    ),
                    InputOutcome::Ok
                );
            }
            for (backward, index) in [(false, 1), (true, 0), (true, 2), (false, 0)] {
                let held = if backward {
                    mods | ModifiersState::SHIFT
                } else {
                    mods
                };
                press(&mut app, wid, code, key.clone(), held);
                assert_eq!(
                    selected(&app),
                    matches[index],
                    "{next}, accepted={accepted}, backward={backward}"
                );
            }
        }
        assert!(app.windows[&wid].search.is_none());
        assert_eq!(app.search_last_query, "terminal-only");
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, original.seq);
        assert_eq!(after.text, original.text);
        assert!(app.open_settings_tab(SettingsRoute::Home));
        press(&mut app, wid, code, key.clone(), mods);
        press(
            &mut app,
            wid,
            code,
            key.clone(),
            mods | ModifiersState::SHIFT,
        );
        assert_eq!(selected(&app), matches[0]);
        assert!(app.windows[&wid].search.is_none());

        app.switch_tab_in(wid, 0);
        press(&mut app, wid, code, key, mods);
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().query,
            "terminal-only"
        );
    }
}

#[test]
fn routed_find_keys_keep_their_window_when_frontmost_has_changed() {
    use crate::app_search::SearchEdit;
    use crate::keybinding::Keybindings;
    use crate::native_app::AppViewState;
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{
        Key as WinitKey, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
    };

    fn press(app: &mut App, wid: WindowId, code: KeyCode, key: WinitKey, mods: ModifiersState) {
        app.windows.get_mut(&wid).unwrap().mods = mods;
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

    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = dir.path().join("find.md");
    let source = "α needle β needle";
    fs::write(&path, source).unwrap();
    let matches: Vec<_> = source
        .match_indices("needle")
        .map(|(at, text)| at..at + text.len())
        .collect();
    for kind in [
        None,
        Some(AppKind::Editor),
        Some(AppKind::Settings),
        Some(AppKind::Markdown),
    ] {
        for configured in [true, false] {
            if !configured && !cfg!(target_os = "macos") {
                continue;
            }
            let mut app = App::headless_for_test();
            let (a, b) = (WindowId(0), WindowId(1));
            crate::term_lock(&app.front_terminal(a).unwrap().term).process(source.as_bytes());
            match kind {
                Some(AppKind::Settings) => assert!(app.open_settings_tab(SettingsRoute::Home)),
                Some(kind) => {
                    app.open_document_tab(kind, &file_uri(&path)).unwrap();
                }
                None => {}
            }
            let editor = app
                .active_native_view(a)
                .and_then(|(instance, view)| {
                    app.native_runtime
                        .document_id(instance)
                        .map(|document| (view, document))
                })
                .filter(|_| kind == Some(AppKind::Editor));
            let original =
                editor.map(|(_, document)| app.document_store.snapshot(document).unwrap());
            app.install_window_state(b, crate::stub_session(99), 38, 132);
            crate::term_lock(&app.front_terminal(b).unwrap().term).process(b"other other");
            app.find_requested();
            app.search_edit_in(b, SearchEdit::Insert("other".to_string()));
            let other_search = |app: &App| {
                let search = app.windows[&b].search.as_ref().unwrap();
                (search.query.clone(), search.current)
            };
            let other_before = other_search(&app);
            app.keybindings = Keybindings::resolved(Some(
                &[
                    ("f2".to_string(), "find".to_string()),
                    ("f3".to_string(), "find_next".to_string()),
                    ("shift+f3".to_string(), "find_prev".to_string()),
                ]
                .into_iter()
                .collect(),
            ));
            let (code, key, mods) = if configured {
                (
                    KeyCode::F2,
                    WinitKey::Named(NamedKey::F2),
                    ModifiersState::empty(),
                )
            } else {
                (
                    KeyCode::KeyF,
                    WinitKey::Character(SmolStr::new("f")),
                    ModifiersState::SUPER,
                )
            };
            press(&mut app, a, code, key, mods);
            match kind {
                None => assert!(app.windows[&a].search.is_some()),
                Some(AppKind::Editor) => assert!(matches!(
                    app.native_runtime.view_state(editor.unwrap().0),
                    Some(AppViewState::Editor(state)) if state.buffer.as_ref().is_some_and(
                        |buffer| matches!(buffer.minibuffer, amacs::editor::Minibuffer::Search { .. })
                    )
                )),
                Some(AppKind::Settings) => assert!(matches!(
                    app.native_runtime.view_state(app.active_native_view(a).unwrap().1),
                    Some(AppViewState::Settings(state)) if state.common.last_focus.as_ref()
                        .is_some_and(|key| key.as_str() == "settings/search")
                )),
                _ => assert!(!app.can_find(a)),
            }
            assert_eq!(other_search(&app), other_before);

            if matches!(kind, None | Some(AppKind::Editor)) {
                if kind.is_none() {
                    app.search_edit_in(a, SearchEdit::Insert("needle".to_string()));
                } else {
                    assert_eq!(
                        app.input(a, InputEvent::Text("needle".to_string()), Source::Human),
                        InputOutcome::Ok
                    );
                }
                let selected = |app: &App| {
                    if let Some((view, _)) = editor {
                        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view)
                        else {
                            panic!("editor");
                        };
                        state
                            .buffer
                            .as_ref()
                            .unwrap()
                            .primary_selection()
                            .range()
                            .start
                    } else {
                        app.windows[&a].search.as_ref().unwrap().current
                    }
                };
                let first = selected(&app);
                for accepted in [false, true] {
                    if accepted {
                        if kind.is_none() {
                            app.search_accept_in(a);
                        } else {
                            press(
                                &mut app,
                                a,
                                KeyCode::Enter,
                                WinitKey::Named(NamedKey::Enter),
                                ModifiersState::empty(),
                            );
                        }
                    }
                    press(
                        &mut app,
                        a,
                        KeyCode::F3,
                        WinitKey::Named(NamedKey::F3),
                        ModifiersState::empty(),
                    );
                    assert_ne!(selected(&app), first, "{kind:?}, accepted={accepted}");
                    if editor.is_some() {
                        assert_eq!(selected(&app), matches[1].start);
                    }
                    press(
                        &mut app,
                        a,
                        KeyCode::F3,
                        WinitKey::Named(NamedKey::F3),
                        ModifiersState::SHIFT,
                    );
                    assert_eq!(selected(&app), first);
                    assert_eq!(other_search(&app), other_before);
                }
            } else {
                press(
                    &mut app,
                    a,
                    KeyCode::F3,
                    WinitKey::Named(NamedKey::F3),
                    ModifiersState::empty(),
                );
                press(
                    &mut app,
                    a,
                    KeyCode::F3,
                    WinitKey::Named(NamedKey::F3),
                    ModifiersState::SHIFT,
                );
            }
            assert_eq!(other_search(&app), other_before);
            assert_eq!(app.frontmost_window, Some(b));
            if let Some((_, document)) = editor {
                let after = app.document_store.snapshot(document).unwrap();
                assert_eq!(after.seq, original.as_ref().unwrap().seq);
                assert_eq!(after.text, original.as_ref().unwrap().text);
            }
        }
    }
}

#[test]
fn picker_path_opens_only_the_exact_local_document_grant() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-native-picker-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let chosen = dir.join("chosen.md");
    let unchosen = dir.join("unchosen.md");
    fs::write(&chosen, "# Chosen\n").unwrap();
    fs::write(&unchosen, "# Must remain unopened\n").unwrap();

    let mut app = App::headless_for_test();
    let opened = app
        .open_local_document_path(AppKind::Markdown, &chosen)
        .expect("picker-approved file opens");
    assert!(opened.starts_with("app markdown file://"), "{opened}");
    let (instance, _) = app
        .active_native_view(WindowId(0))
        .expect("Markdown became the active native tab");
    assert_eq!(
        app.native_runtime.app(instance).map(|app| app.kind()),
        Some(AppKind::Markdown)
    );

    let chosen_uri =
        crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&chosen).unwrap())
            .unwrap();
    let unchosen_uri =
        crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&unchosen).unwrap())
            .unwrap();
    assert!(app.document_store.id_for_uri(&chosen_uri).is_some());
    assert_eq!(
        app.document_store.id_for_uri(&unchosen_uri),
        None,
        "choosing one file must not acquire ambient directory authority"
    );
    assert!(
        app.open_local_document_path(AppKind::Editor, &dir).is_err(),
        "directories are never document grants"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn native_settings_cmd_f_focuses_its_own_search() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(SettingsRoute::Appearance));
    let _ = app.input(wid, key('f', Modifiers::SUPER), Source::Human);
    let (_, view) = app.active_native_view(wid).expect("Settings view");
    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(crate::native_app::AppViewState::Settings(state))
            if state.common.last_focus.as_ref().is_some_and(
                |key| key.as_str() == "settings/search"
            )
    ));
    assert!(
        app.windows[&wid].search.is_none(),
        "terminal find stays off"
    );
}

#[test]
fn editor_cmd_s_and_cmd_z_reduce_before_the_terminal_boundary() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-native-keyboard-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("keyboard.md");
    fs::write(&path, "before\n").unwrap();

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    assert_eq!(
        app.input(wid, InputEvent::Text("safe ".to_string()), Source::Human),
        InputOutcome::Ok
    );
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "safe before\n"
    );

    assert_eq!(
        app.input(wid, key('s', Modifiers::SUPER), Source::Human),
        InputOutcome::Ok
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "safe before\n");
    assert_eq!(app.document_store.dirty(document), Some(false));

    assert_eq!(
        app.input(wid, key('z', Modifiers::SUPER), Source::Human),
        InputOutcome::Ok
    );
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "before\n"
    );
    assert_eq!(app.document_store.dirty(document), Some(true));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn native_repeat_replays_exact_press_view_and_never_the_replacement_tab() {
    use aterm_window::keyboard::{KeyCode, PhysicalKey};

    let dir = std::env::temp_dir().join(format!(
        "aterm-native-repeat-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("repeat.md");
    fs::write(&path, "abc").unwrap();

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Editor, &file_uri(&path))
        .unwrap();
    let (instance, view) = app.active_native_view(wid).expect("Editor view");
    let document = app.native_runtime.document_id(instance).unwrap();
    let typed = InputEvent::Text("x".to_string());

    assert_eq!(
        app.input(wid, typed.clone(), Source::Human),
        InputOutcome::Ok
    );
    let physical = PhysicalKey::Code(KeyCode::KeyX);
    app.note_local_repeat_press(
        wid,
        physical,
        crate::LocalRepeatAction::Native { view, event: typed },
    );
    app.route_physical_repeat(wid, physical);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "xxabc",
        "the repeat is delivered to the captured Editor view"
    );

    assert!(app.open_settings_tab(SettingsRoute::Home));
    app.route_physical_repeat(wid, physical);
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        "xxabc",
        "a stale native repeat cannot mutate either replacement content"
    );
    assert!(app.take_local_repeat_release(wid, physical));
    assert!(!app.take_local_repeat_release(wid, physical));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn markdown_cmd_a_and_escape_own_exact_source_selection() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-native-markdown-keyboard-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reader.md");
    let source = "# Héllo\n\nBody 🦀 and [link](https://example.com).\n";
    fs::write(&path, source).unwrap();

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &file_uri(&path))
        .unwrap();
    assert_eq!(
        app.input(wid, key('a', Modifiers::SUPER), Source::Human),
        InputOutcome::Ok
    );
    let (_, view) = app.active_native_view(wid).expect("Markdown view");
    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(crate::native_app::AppViewState::Markdown(state))
            if state.selection == Some(0..source.len())
    ));
    assert_eq!(app.native_selection_text(wid).as_deref(), Some(source));

    assert_eq!(
        app.input(
            wid,
            InputEvent::Key {
                key: Key::Named(aterm_types::keyboard::NamedKey::Escape),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
            Source::Human,
        ),
        InputOutcome::Ok
    );
    assert!(matches!(
        app.native_runtime.view_state(view),
        Some(crate::native_app::AppViewState::Markdown(state))
            if state.selection.is_none()
    ));
    assert_eq!(app.native_selection_text(wid), None);
    let _ = fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn raw_key_sequence_never_reaches_parked_pty_under_native_tab() {
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows
        .get_mut(&wid)
        .unwrap()
        .active_terminal
        .as_mut()
        .unwrap()
        .sink = Arc::new(SinkWriter::new(pipe[1]));
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let raw = b"\x1b[99~".to_vec();
    assert_eq!(
        app.input(wid, InputEvent::KeySequence(raw.clone()), Source::Human),
        InputOutcome::Ok
    );
    let mut bytes = [0u8; 32];
    assert_eq!(
        unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) },
        -1
    );

    assert!(app.close_settings_tabs());
    app.windows
        .get_mut(&wid)
        .unwrap()
        .active_terminal
        .as_mut()
        .unwrap()
        .sink = Arc::new(SinkWriter::new(pipe[1]));
    assert_eq!(
        app.input(wid, InputEvent::KeySequence(raw.clone()), Source::Human),
        InputOutcome::Ok
    );
    let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
    assert_eq!(
        read,
        raw.len() as isize,
        "negative control must observe PTY bytes"
    );
    assert_eq!(&bytes[..read as usize], raw.as_slice());
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

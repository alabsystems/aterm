// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Option-as-Alt routing tests across terminal grid, native editor, and GUI text fields.

use crate::{App, WindowId};

#[test]
fn option_is_an_alt_only_while_the_typing_reaches_the_terminal() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(
        app.option_is_alt(wid),
        "Meta mode, the grid takes the typing"
    );

    app.search_enter_direction_in(wid, true);
    assert!(app.windows[&wid].search.is_some(), "fixture: the find bar");
    assert!(!app.option_is_alt(wid), "the find bar");
    app.search_cancel_in(wid);
    assert!(app.option_is_alt(wid), "the find bar closed");

    app.toggle_palette();
    assert!(
        app.windows[&wid].palette().is_some(),
        "fixture: the palette"
    );
    assert!(!app.option_is_alt(wid), "the palette");
    app.toggle_palette();
    assert!(app.option_is_alt(wid), "the palette closed");

    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));
    assert!(app.inline_rename_edit(wid).is_some(), "fixture: the rename");
    assert!(!app.option_is_alt(wid), "the inline rename");
    app.settle_rename_edit(wid);
    assert!(app.option_is_alt(wid), "the rename settled");

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(
        app.active_native_view(wid).is_some(),
        "fixture: a native page"
    );
    assert!(!app.option_is_alt(wid), "a native page");
    app.switch_tab_in(wid, 0);
    assert!(
        app.active_native_view(wid).is_none(),
        "fixture: back to the terminal"
    );
    assert!(app.option_is_alt(wid), "the terminal's tab again");

    app.option_as_meta = false;
    assert!(!app.option_is_alt(wid), "Meta mode off: Option composes");
}

#[test]
fn option_is_meta_in_native_editor_when_option_as_meta() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-option-meta-")
        .tempdir()
        .unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "one two").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
        .unwrap();
    let (_, view) = app
        .active_native_view(wid)
        .expect("fixture: the Editor in front");
    #[cfg(not(target_os = "macos"))]
    let _ = view;
    let editor_tab = app.windows[&wid].tab_set.active_id().unwrap();
    assert!(app.typing_reaches_editor(wid));
    assert!(!app.typing_reaches_grid(wid), "fixture: not the terminal");
    assert!(
        app.option_is_alt(wid),
        "Meta mode, the Editor takes the typing"
    );
    app.sync_option_as_alt();
    assert!(app.windows[&wid].option_as_alt.get(), "the window is told");

    app.toggle_palette();
    assert!(
        app.windows[&wid].palette().is_some(),
        "fixture: the palette"
    );
    assert!(!app.option_is_alt(wid), "the palette over the Editor");
    app.toggle_palette();
    assert!(app.option_is_alt(wid), "the palette closed");

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(
        !app.typing_reaches_editor(wid),
        "fixture: Settings in front"
    );
    assert!(
        !app.option_is_alt(wid),
        "Settings is no Editor: Option composes"
    );
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .switch_to(editor_tab)
    );
    app.sync_window(wid);
    assert!(app.option_is_alt(wid), "the Editor's tab again");

    app.option_as_meta = false;
    assert!(!app.option_is_alt(wid), "Meta mode off: Option composes");
    app.option_as_meta = true;

    #[cfg(target_os = "macos")]
    {
        use aterm_window::event::{ElementState, KeyEvent};
        use aterm_window::keyboard::{
            Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr,
        };
        let press = |code: KeyCode, letter: &str| {
            KeyEvent::synthetic_for_test_with_base(
                PhysicalKey::Code(code),
                Key::Character(SmolStr::new(letter)),
                Key::Character(SmolStr::new(letter)),
                Some(SmolStr::new(letter)),
                KeyLocation::Standard,
                ElementState::Pressed,
                false,
            )
        };
        let caret = |app: &App| match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Editor(state)) => {
                state.buffer.as_ref().unwrap().selections.clone()
            }
            _ => panic!("the Editor view"),
        };
        app.sync_option_as_alt();
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL;
        app.on_key(wid, press(KeyCode::KeyE, "e"));
        assert_eq!(
            caret(&app),
            vec![crate::native_editor::Selection::caret(7)],
            "fixture: C-e, the end of the line"
        );
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::ALT;
        app.on_key(wid, press(KeyCode::KeyB, "b"));
        assert_eq!(
            caret(&app),
            vec![crate::native_editor::Selection::caret(4)],
            "Option+b is M-b, backward-word"
        );
    }
}

#[test]
fn the_sync_carries_each_change_to_the_window() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let told = |app: &App| app.windows[&wid].option_as_alt.get();
    assert!(!told(&app), "a window's Option composes until it is told");
    app.sync_option_as_alt();
    assert!(told(&app), "Meta mode, the grid takes the typing");
    app.search_enter_direction_in(wid, true);
    assert!(told(&app), "nothing changes until the sync runs");
    app.sync_option_as_alt();
    assert!(!told(&app), "the find bar");
    app.search_cancel_in(wid);
    app.sync_option_as_alt();
    assert!(told(&app), "the find bar closed");
    app.option_as_meta = false;
    app.sync_option_as_alt();
    assert!(!told(&app), "a reload that turns Meta mode off");
}

/// An Option chord carries the layout's character to the encoder only while the
/// window's Option is an Alt, the event AppKit then hands over (German
/// Option+Shift+2 reads '"' without Option: ESC '"'). While Option composes, the
/// text is what it composed ('„'), so the chord goes out as ESC and the US
/// table's glyph, as before .
#[cfg(target_os = "macos")]
#[test]
fn an_option_chord_carries_the_layouts_character_only_while_option_is_an_alt() {
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr};

    let press = |text: &str| {
        KeyEvent::synthetic_for_test_with_base(
            PhysicalKey::Code(KeyCode::Digit2),
            Key::Character(SmolStr::new(text)),
            Key::Character(SmolStr::new("2")),
            Some(SmolStr::new(text)),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        )
    };
    let (mut wire, sink) = std::os::unix::net::UnixStream::pair().expect("observer");
    wire.set_nonblocking(true).unwrap();
    let mut app = App::headless_for_test_with_sink(std::sync::Arc::new(
        aterm_session::sink::SinkWriter::new_owned(sink.into()),
    ));
    let wid = WindowId(0);
    let mut drain = || {
        use std::io::Read;
        let mut bytes = [0u8; 64];
        match wire.read(&mut bytes) {
            Ok(n) => bytes[..n].to_vec(),
            Err(_) => Vec::new(),
        }
    };
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::ALT | ModifiersState::SHIFT;

    app.on_key(wid, press("„"));
    assert_eq!(drain(), b"\x1b@", "Option composes: ESC and the US glyph");
    app.sync_option_as_alt();
    app.on_key(wid, press("\""));
    assert_eq!(
        drain(),
        b"\x1b\"",
        "Option is an Alt: ESC and the layout's glyph"
    );
}

/// A CHARACTER ONLY OPTION REACHES IS TYPED IN META MODE. While
/// the window's Option is an Alt, an Option chord goes out as ESC and the key,
/// except one whose layout reaches its character only with Option
/// ([`crate::platform::OptionOnly`], here a German-shaped fake): Option+L types
/// `@`, Option+N `~` (the tilde dead key's accent), Option+Shift+7 `\`, each as
/// the plain character, while Option+U (no such character) is still ESC u and a
/// chord with Control keeps its ESC and control byte. The control is the inert
/// source every headless App holds, the U.S. layout's answer: Option+L is ESC l.
#[cfg(target_os = "macos")]
#[test]
fn an_option_chord_whose_character_only_option_reaches_types_it() {
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr};

    fn german(key: PhysicalKey, shift: bool) -> Option<char> {
        match (key, shift) {
            (PhysicalKey::Code(KeyCode::KeyL), false) => Some('@'),
            (PhysicalKey::Code(KeyCode::KeyN), false) => Some('~'),
            (PhysicalKey::Code(KeyCode::Digit7), true) => Some('\\'),
            _ => None,
        }
    }
    let key = |code: KeyCode, base: &str, text: &str, state: ElementState| {
        KeyEvent::synthetic_for_test_with_base(
            PhysicalKey::Code(code),
            Key::Character(SmolStr::new(text)),
            Key::Character(SmolStr::new(base)),
            Some(SmolStr::new(text)),
            KeyLocation::Standard,
            state,
            false,
        )
    };
    let (mut wire, sink) = std::os::unix::net::UnixStream::pair().expect("observer");
    wire.set_nonblocking(true).unwrap();
    let mut app = App::headless_for_test_with_sink(std::sync::Arc::new(
        aterm_session::sink::SinkWriter::new_owned(sink.into()),
    ));
    let wid = WindowId(0);
    let mut drain = || {
        use std::io::Read;
        let mut bytes = [0u8; 64];
        match wire.read(&mut bytes) {
            Ok(n) => bytes[..n].to_vec(),
            Err(_) => Vec::new(),
        }
    };
    let mut tap = |app: &mut App, mods: ModifiersState, code: KeyCode, text: &str| {
        let base = match code {
            KeyCode::Digit7 => "7",
            _ => text,
        };
        app.windows.get_mut(&wid).unwrap().mods = mods;
        app.on_key(wid, key(code, base, text, ElementState::Pressed));
        app.on_key(wid, key(code, base, text, ElementState::Released));
        drain()
    };
    app.sync_option_as_alt();
    assert!(
        app.windows[&wid].option_as_alt.get(),
        "fixture: Option is an Alt"
    );
    let alt = ModifiersState::ALT;

    assert_eq!(
        tap(&mut app, alt, KeyCode::KeyL, "l"),
        b"\x1bl",
        "control: the inert source (the U.S. layout's answer) leaves Meta"
    );
    app.option_only = german;
    assert_eq!(tap(&mut app, alt, KeyCode::KeyL, "l"), b"@");
    assert_eq!(
        tap(&mut app, alt, KeyCode::KeyN, "n"),
        b"~",
        "a dead key's accent"
    );
    assert_eq!(
        tap(&mut app, alt | ModifiersState::SHIFT, KeyCode::Digit7, "/"),
        b"\\",
        "with Shift"
    );
    assert_eq!(
        tap(&mut app, alt, KeyCode::KeyU, "u"),
        b"\x1bu",
        "no such character: Meta"
    );
    assert_eq!(
        tap(&mut app, alt | ModifiersState::CONTROL, KeyCode::KeyL, "l"),
        b"\x1b\x0c",
        "with Control the chord is left as it came"
    );

    app.option_as_meta = false;
    app.sync_option_as_alt();
    let mut composed = key(KeyCode::KeyL, "l", "@", ElementState::Pressed);
    app.windows.get_mut(&wid).unwrap().mods = alt;
    app.on_key(wid, composed.clone());
    composed.state = ElementState::Released;
    app.on_key(wid, composed);
    assert_eq!(
        drain(),
        b"@",
        "Meta mode off: Option composes, as AppKit typed it"
    );
}

fn many_route_windows() -> App {
    let mut app = App::headless_for_test();
    for _ in 0..48 {
        let sid = app.next_session_id;
        app.insert_logical_window(crate::stub_session(sid), 24, 80);
    }
    app.frontmost_window = Some(WindowId(0));
    app.sync_option_as_alt();
    app
}

fn route_work(app: &App) -> Vec<(WindowId, u64)> {
    app.windows
        .iter()
        .map(|(id, ws)| (*id, ws.routing.signal().evaluations()))
        .collect()
}

fn assert_only_route_owner(app: &App, before: &[(WindowId, u64)], owner: WindowId) {
    for (id, count) in before {
        assert_eq!(
            app.windows[id].routing.signal().evaluations(),
            count + u64::from(*id == owner),
            "route work for {id:?}"
        );
    }
}

#[test]
fn option_route_output_and_clean_epilogues_do_no_unrelated_window_work() {
    let mut app = many_route_windows();
    let before = route_work(&app);
    for _ in 0..4 {
        // The actual Wake::Output arm and the same trailing route publication.
        app.on_output_wake(0, std::time::Instant::now());
        app.sync_option_as_alt();
    }
    assert_eq!(route_work(&app), before);
    // Redraw scheduling mutates the actual output owner, not its key route.
    app.admit_output_redraws(0, std::time::Instant::now());
    app.sync_option_as_alt();
    assert_eq!(route_work(&app), before);
    // Negative control: the old full traversal violates this work bound even
    // though its platform writes were already equality guarded.
    for id in app.windows.keys() {
        app.option_is_alt(*id);
    }
    assert_ne!(route_work(&app), before);
}

#[test]
fn option_route_mutation_owners_coalesce_without_visiting_other_windows() {
    let mut app = many_route_windows();
    let wid = WindowId(0);
    let settle = |app: &mut App, expected: bool, change: &mut dyn FnMut(&mut App)| {
        let before = route_work(app);
        change(app);
        app.sync_option_as_alt();
        assert_eq!(app.windows[&wid].option_as_alt.get(), expected);
        assert_only_route_owner(app, &before, wid);
        let clean = route_work(app);
        app.sync_option_as_alt();
        assert_eq!(route_work(app), clean);
    };
    settle(&mut app, false, &mut |app| {
        app.search_enter_direction_in(wid, true)
    });
    settle(&mut app, true, &mut |app| app.search_cancel_in(wid));
    settle(&mut app, false, &mut |app| app.toggle_palette());
    settle(&mut app, true, &mut |app| app.toggle_palette());
    app.tab_strip_rows = 1;
    settle(&mut app, false, &mut |app| {
        assert!(app.begin_active_session_rename(wid));
    });
    settle(&mut app, true, &mut |app| app.settle_rename_edit(wid));
    settle(&mut app, false, &mut |app| {
        assert!(app.open_active_tab_context_menu(wid));
    });
    settle(&mut app, true, &mut |app| {
        assert!(app.close_tab_menu(wid));
    });
    // Multiple real route transitions are one pending owner, using final state.
    settle(&mut app, true, &mut |app| {
        app.search_enter_direction_in(wid, true);
        app.search_cancel_in(wid);
        app.toggle_palette();
        app.toggle_palette();
    });
    app.option_as_meta = false;
    app.sync_option_as_alt();
    assert!(app.windows.values().all(|ws| !ws.option_as_alt.get()));
    app.option_as_meta = true;
    app.sync_option_as_alt();
    assert!(app.windows.values().all(|ws| ws.option_as_alt.get()));
}

#[test]
fn option_route_native_mutable_authority_and_carry_follow_the_live_window() {
    use crate::native_app::{AppKind, AppViewState, MarkdownWorkspaceMode};
    let mut app = many_route_windows();
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("routing.md");
    std::fs::write(&path, "# Routing\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    app.open_document_tab(AppKind::Markdown, &uri).unwrap();
    let wid = WindowId(0);
    let (instance, view) = app.active_native_view(wid).unwrap();
    app.sync_option_as_alt();
    assert!(!app.windows[&wid].option_as_alt.get());
    let before = route_work(&app);
    assert!(app.native_runtime.focus_markdown_editor(instance, view));
    app.sync_option_as_alt();
    assert!(app.windows[&wid].option_as_alt.get());
    assert_only_route_owner(&app, &before, wid);
    let before = route_work(&app);
    assert!(!app.native_runtime.change_view_state(view, |state| {
        let AppViewState::Editor(editor) = state else {
            panic!("Editor");
        };
        editor.markdown.as_mut().unwrap().mode = MarkdownWorkspaceMode::Read;
        false
    }));
    app.sync_option_as_alt();
    assert!(!app.windows[&wid].option_as_alt.get());
    assert_only_route_owner(&app, &before, wid);
    let before = route_work(&app);
    let mut route_native_guard = app.native_runtime.view_state_mut(view);
    let Some(AppViewState::Editor(editor)) = route_native_guard.as_deref_mut() else {
        panic!("Editor");
    };
    editor.markdown.as_mut().unwrap().mode = MarkdownWorkspaceMode::Edit;
    drop(route_native_guard);
    app.sync_option_as_alt();
    assert!(app.windows[&wid].option_as_alt.get());
    assert_only_route_owner(&app, &before, wid);
    // Replacing the entire mutable value retains storage-owned notification authority.
    let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(view) else {
        panic!("Editor");
    };
    let replacement = AppViewState::Editor(editor.clone());
    *app.native_runtime.view_state_mut(view).unwrap() = replacement;
    app.sync_option_as_alt();
    let before = route_work(&app);
    app.native_runtime
        .set_editor_markdown_mode(instance, view, MarkdownWorkspaceMode::Read);
    app.sync_option_as_alt();
    assert!(!app.windows[&wid].option_as_alt.get());
    assert_only_route_owner(&app, &before, wid);
    app.native_runtime.focus_markdown_editor(instance, view);
    app.sync_option_as_alt();
    // Move-to-next-window binds the destination before the source releases it.
    app.migrate_active_tab_from_window(wid).unwrap();
    app.sync_option_as_alt();
    let migrated = WindowId(1);
    assert_eq!(app.active_native_view(migrated), Some((instance, view)));
    let before = route_work(&app);
    app.native_runtime
        .set_editor_markdown_mode(instance, view, MarkdownWorkspaceMode::Read);
    app.sync_option_as_alt();
    assert!(!app.windows[&migrated].option_as_alt.get());
    assert_only_route_owner(&app, &before, migrated);
    app.native_runtime.focus_markdown_editor(instance, view);
    app.sync_option_as_alt();
    app.frontmost_window = Some(migrated);
    let destination = app.detach_active_tab_logical().unwrap();
    app.sync_option_as_alt();
    assert_eq!(app.active_native_view(destination), Some((instance, view)));
    let before = route_work(&app);
    app.native_runtime
        .set_editor_markdown_mode(instance, view, MarkdownWorkspaceMode::Read);
    app.sync_option_as_alt();
    assert!(!app.windows[&destination].option_as_alt.get());
    assert_only_route_owner(&app, &before, destination);
    // A pending ticket cannot outlive its window or target its replacement.
    app.native_runtime.focus_markdown_editor(instance, view);
    let removed = app.windows.remove(&destination).unwrap();
    app.sync_option_as_alt();
    app.windows.insert(destination, removed);
    app.sync_window(destination);
    app.sync_option_as_alt();
    assert!(app.windows[&destination].option_as_alt.get());
    assert!(app.structural_invariants_ok());
}

fn fresh_route_root(app: &App, wid: WindowId) -> Box<crate::WindowState> {
    let old = &app.windows[&wid];
    let front = old.front_terminal().unwrap();
    let mut root = crate::WindowState::new_terminal(
        front.term.clone(),
        front.master,
        front.sink.clone(),
        front.ui_waiting.clone(),
        front.session,
        old.rows,
        old.cols,
        app.unattached_window_metrics(),
        old.tabs,
        old.layouts.clone(),
        old.tab_set.clone(),
    );
    root.routing
        .set_search(Some(crate::app_search::SearchState::default()));
    Box::new(root)
}

#[test]
fn option_route_fresh_root_custody_survives_every_mutable_membership_projection() {
    let wid = WindowId(0);
    for capability in 0..5 {
        let mut app = many_route_windows();
        let old_signal = app.windows[&wid].routing.signal().clone();
        let root = fresh_route_root(&app, wid);
        assert!(old_signal.evaluations() > 0);
        assert_eq!(
            root.routing.signal().evaluations(),
            0,
            "a genuinely fresh identity"
        );
        match capability {
            0 => *app.windows.get_mut(&wid).unwrap() = root,
            1 => *app.windows.values_mut().next().unwrap() = root,
            2 => *app.windows.iter_mut().next().unwrap().1 = root,
            3 => *(&mut app.windows).into_iter().next().unwrap().1 = root,
            4 => *app.front_mut().unwrap() = root,
            _ => unreachable!(),
        }
        // A late old native subscriber cannot overwrite the new root's pending ticket.
        old_signal.invalidate();
        app.sync_option_as_alt();
        assert!(
            !app.windows[&wid].option_as_alt.get(),
            "new root's find field"
        );
        assert_eq!(app.windows[&wid].routing.signal().evaluations(), 1);
        let before = route_work(&app);
        app.windows.get_mut(&wid).unwrap().routing.set_search(None);
        app.sync_option_as_alt();
        assert!(app.windows[&wid].option_as_alt.get());
        assert_only_route_owner(&app, &before, wid);
        assert!(app.structural_invariants_ok());
    }
    // Transferring the opaque aggregate is also a logical identity replacement.
    let mut app = many_route_windows();
    let old_signal = app.windows[&wid].routing.signal().clone();
    let mut replacement = crate::option_routing::Routing::new(app.windows[&wid].front_content);
    replacement.set_search(Some(crate::app_search::SearchState::default()));
    app.windows.get_mut(&wid).unwrap().routing = replacement;
    old_signal.invalidate();
    app.sync_option_as_alt();
    assert!(!app.windows[&wid].option_as_alt.get());
    assert_eq!(app.windows[&wid].routing.signal().evaluations(), 1);
}

#[test]
fn option_route_open_find_output_and_text_edits_do_no_projection_work() {
    use crate::app_search::SearchEdit;
    use crate::input::InputEvent;
    let mut app = many_route_windows();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).unwrap().term.clone();
    crate::term_lock(&term).process(b"needle one\r\nneedle two\r\n");
    app.search_enter_direction_in(wid, true);
    app.search_edit_in(wid, SearchEdit::Insert("needle".into()));
    app.sync_option_as_alt();
    assert!(!app.windows[&wid].option_as_alt.get());
    let before = route_work(&app);
    app.search_edit_in(wid, SearchEdit::MoveStart);
    app.search_edit_in(wid, SearchEdit::Insert("n".into()));
    app.search_edit_in(wid, SearchEdit::DeleteBack);
    crate::term_lock(&term).process(b"needle three\r\n");
    app.on_output_wake(0, std::time::Instant::now());
    app.sync_option_as_alt();
    assert_eq!(
        route_work(&app),
        before,
        "match/query/cursor changes retain find presence"
    );
    app.search_cancel_in(wid);
    app.sync_option_as_alt();
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));
    app.sync_option_as_alt();
    let before = route_work(&app);
    assert!(app.rename_input_event(wid, &InputEvent::Text("route text".into())));
    app.sync_option_as_alt();
    assert_eq!(
        route_work(&app),
        before,
        "rename text leaves its surface unchanged"
    );
    let before = route_work(&app);
    {
        let mut window = app.windows.get_mut(&wid).unwrap();
        let mut edit = window.routing.rename_mut().unwrap();
        edit.surface = crate::app_rename::RenameSurface::Native;
    }
    app.sync_option_as_alt();
    assert!(
        app.windows[&wid].option_as_alt.get(),
        "native rename remains excluded"
    );
    assert_only_route_owner(&app, &before, wid);
    let before = route_work(&app);
    app.windows.get_mut(&wid).unwrap().routing.take_rename();
    app.sync_option_as_alt();
    assert_eq!(
        route_work(&app),
        before,
        "closing native rename does not change the route"
    );
}

#[test]
fn option_route_native_reader_and_content_changes_keep_the_precise_subscription() {
    use crate::native_app::{
        ActionInvocation, AppEvent, AppKind, AppViewState, MarkdownWorkspaceMode,
    };
    use crate::native_ui::{ActionId, UiKey};
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("precise-routing.md");
    std::fs::write(&path, "## A paragraph\n\nText.\n\n".repeat(90)).unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut app = many_route_windows();
    let wid = WindowId(0);
    app.open_document_tab(AppKind::Markdown, &uri).unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    app.native_runtime
        .set_editor_markdown_mode(instance, view, MarkdownWorkspaceMode::Split);
    app.compiled_native_ui(wid).unwrap();
    app.sync_option_as_alt();
    assert!(app.windows[&wid].option_as_alt.get());
    let before = route_work(&app);
    app.dispatch_native_event(
        wid,
        AppEvent::ScrollAt {
            key: UiKey::new("editor/markdown/reader-pane"),
            lines: 8,
        },
    )
    .unwrap();
    assert!(!app.native_runtime.change_view_state(view, |state| {
        let AppViewState::Editor(editor) = state else {
            panic!("Editor");
        };
        editor.status = Some("content-only publication".into());
        false
    }));
    {
        let mut mutation = app.native_runtime.view_state_mut(view).unwrap();
        let AppViewState::Editor(editor) = &mut *mutation else {
            panic!("Editor");
        };
        editor.status = None;
    }
    let document = app.native_runtime.document_id(instance).unwrap();
    let revision = app.document_store.snapshot(document).unwrap().seq.0;
    // This production dispatch performs the Markdown reader CommonViewState clone round trip.
    app.dispatch_native_event(wid, AppEvent::DocumentChanged { document, revision })
        .unwrap();
    app.sync_option_as_alt();
    assert_eq!(
        route_work(&app),
        before,
        "reader scroll and content changes retain editor eligibility"
    );
    let before = route_work(&app);
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new("editor/markdown/focus-reader"),
            value: None,
        }),
    )
    .unwrap();
    app.sync_option_as_alt();
    assert!(!app.windows[&wid].option_as_alt.get());
    assert_only_route_owner(&app, &before, wid);
    let before = route_work(&app);
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new("editor/markdown/focus-editor"),
            value: None,
        }),
    )
    .unwrap();
    app.sync_option_as_alt();
    assert!(app.windows[&wid].option_as_alt.get());
    assert_only_route_owner(&app, &before, wid);
}

/// Each window's classifier work, and each view's routed generation.
type ClassifierWork = (
    Vec<(WindowId, crate::option_routing::WindowClassifierWork)>,
    Vec<(crate::tab_model::ViewId, u64)>,
);

fn route_classifier_work(app: &App) -> ClassifierWork {
    (
        app.windows
            .iter()
            .map(|(id, window)| (*id, window.routing.classifier_work()))
            .collect(),
        app.native_runtime.route_classifier_work(),
    )
}

#[test]
fn option_route_classifier_control_detects_general_window_snapshot_work() {
    let mut app = many_route_windows();
    let session = app.front_terminal_mirror(WindowId(0)).unwrap().session;
    let before = route_classifier_work(&app);
    let projections = route_work(&app);
    for mut window in app.windows.values_mut() {
        window.pending_deco_birth = None;
    }
    app.sync_option_as_alt();
    assert_eq!(route_classifier_work(&app), before);
    assert_eq!(route_work(&app), projections);
    // Inject the rejected behavior at the production membership boundary, then
    // drive the same real output admission rather than calling a counter stub.
    crate::option_routing::with_general_snapshot_mutant_for_negative_control(|| {
        assert!(app.admit_output_redraws(session, std::time::Instant::now()));
        app.sync_option_as_alt();
    });
    assert_ne!(route_classifier_work(&app), before);
    assert_eq!(
        route_work(&app),
        projections,
        "classifier accounting is independent of publication"
    );
}

#[test]
fn option_route_open_find_output_and_query_edits_are_clean() {
    use crate::app_search::SearchEdit;
    let mut app = many_route_windows();
    let wid = WindowId(0);
    let terminal = app.front_terminal_mirror(wid).unwrap();
    app.search_enter_direction_in(wid, true);
    app.search_edit_in(wid, SearchEdit::Insert("needle".into()));
    app.sync_option_as_alt();
    let before = route_work(&app);
    let classifiers = route_classifier_work(&app);
    // Actual content, with the terminal lock released before the wake handler.
    crate::term_lock(&terminal.term).process(b"needle after find opened\r\n");
    app.on_output_wake(terminal.session, std::time::Instant::now());
    app.sync_option_as_alt();
    assert_eq!(route_work(&app), before);
    assert_eq!(route_classifier_work(&app), classifiers);
    let before = route_work(&app);
    let classifiers = route_classifier_work(&app);
    app.search_edit_in(wid, SearchEdit::Insert(" more".into()));
    app.search_edit_in(wid, SearchEdit::MoveCharLeft);
    app.sync_option_as_alt();
    assert_eq!(route_work(&app), before);
    assert_eq!(route_classifier_work(&app), classifiers);
}

#[test]
fn option_route_rename_text_and_caret_edits_are_clean() {
    use crate::app_search::SearchEdit;
    let mut app = many_route_windows();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));
    app.sync_option_as_alt();
    let original = app.inline_rename_edit(wid).unwrap().text.clone();
    let before = route_work(&app);
    // A scoped rename guard may compare InGrid surface before/after;
    // text and caret edits must add no window-route publication.
    assert!(app.rename_field_edit(wid, SearchEdit::MoveEnd));
    assert!(app.rename_field_edit(wid, SearchEdit::Insert("x".into())));
    let edit = app.inline_rename_edit(wid).unwrap();
    assert_ne!(edit.text, original);
    let cursor = edit.cursor;
    assert!(app.rename_field_edit(wid, SearchEdit::MoveCharLeft));
    assert!(app.inline_rename_edit(wid).unwrap().cursor < cursor);
    app.sync_option_as_alt();
    assert_eq!(route_work(&app), before);
}

#[test]
fn option_route_native_false_return_is_not_a_route_change_signal() {
    use crate::native_app::{AppKind, AppViewState, MarkdownWorkspaceMode};
    let mut app = many_route_windows();
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("route-controls.md");
    std::fs::write(&path, "# Route controls\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    app.open_document_tab(AppKind::Markdown, &uri).unwrap();
    let wid = WindowId(0);
    let (_, view) = app.active_native_view(wid).unwrap();
    app.sync_option_as_alt();
    let before = route_work(&app);
    // Native entry guards may classify the borrowed native entry; only route
    // publication is required to stay clean for these native mutable accesses.
    assert!(!app.native_runtime.change_view_state(view, |_| false));
    app.sync_option_as_alt();
    assert_eq!(route_work(&app), before);
    assert!(!app.native_runtime.change_view_state(view, |state| {
        let AppViewState::Editor(editor) = state else {
            panic!("Editor");
        };
        editor.common.focus_visible = !editor.common.focus_visible;
        false
    }));
    app.sync_option_as_alt();
    assert_eq!(route_work(&app), before);
    // Both real eligibility transitions must notify despite reducer false.
    for mode in [MarkdownWorkspaceMode::Edit, MarkdownWorkspaceMode::Read] {
        let before = route_work(&app);
        assert!(!app.native_runtime.change_view_state(view, |state| {
            let AppViewState::Editor(editor) = state else {
                panic!("Editor");
            };
            editor.markdown.as_mut().unwrap().mode = mode;
            false
        }));
        app.sync_option_as_alt();
        assert_only_route_owner(&app, &before, wid);
        let clean = route_work(&app);
        app.sync_option_as_alt();
        assert_eq!(route_work(&app), clean);
    }
}

#[test]
fn option_route_general_output_admission_does_no_classifier_or_projection_work() {
    let mut app = many_route_windows();
    let wid = WindowId(0);
    let session = app.front_terminal_mirror(wid).unwrap().session;
    let before = route_work(&app);
    let classifiers = route_classifier_work(&app);
    // Real stats/redraw admission iterates mutable windows and updates the
    // visible owner's pending_deco_birth/rest without changing route inputs.
    assert!(app.admit_output_redraws(session, std::time::Instant::now()));
    app.sync_option_as_alt();
    assert_eq!(route_work(&app), before);
    assert_eq!(route_classifier_work(&app), classifiers);
}

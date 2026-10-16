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
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{Key as WinitKey, KeyCode, KeyLocation, PhysicalKey, SmolStr};

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
    // Built by the shipping encoder: a hand-rolled `format!` is malformed on
    // Windows (drive letter + backslashes after the authority slot).
    crate::native_document_host::path_to_file_uri(path).unwrap()
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
    use winit::keyboard::{KeyCode, PhysicalKey};

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

    // Model the genuine key-down, then capture the normalized repeat owner
    // exactly as `on_key_native_mode` does.
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

    // Replacing the active tab while the key remains held invalidates the
    // captured view. Later repeats are swallowed, never reinterpreted as a
    // Settings key or redirected back through current-content resolution.
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

/// Even a controller-supplied `KeySequence` cannot cross an active native tab
/// (the physical key path already swallows a configured raw sequence before
/// this seam). The terminal pass is the negative control: the pipe sees writes.
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
    // Closing the native tab re-mirrors the terminal session with its real
    // sink; restore the observing pipe for the negative control.
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

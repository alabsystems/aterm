// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who gets Option as an Alt. In Meta mode a press bound for
//! the terminal goes out as ESC + the key, so AppKit must not compose it; a GUI
//! text field (the find bar, the inline rename, the palette, a native page)
//! types what Option composes (German Option+L is '@', Option+e e is 'é'), as
//! every field did before Meta mode made Option an Alt.

use crate::{App, WindowId};

/// Option is an Alt only while the window's typing reaches its terminal grid:
/// every field that types takes Option's composition back while it owns the
/// keyboard, and Meta mode off leaves Option composing everywhere.
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

/// OPTION IS META IN THE NATIVE EDITOR (amacs, slice N4): in Meta mode a
/// window whose front view is an Editor buffer gets Option as an Alt, as its
/// terminal does, so every M- chord reaches the editor's keymap (on macOS no
/// Option dead key composes there). The palette over it and another native
/// page (Settings) still compose; Meta mode off composes everywhere. Then the
/// press itself, through the shipping `on_key`: with the caret at the end of
/// `one two`, Option+b moves it back a word, to `two`.
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
    // Only the macOS key path below reads the view.
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
        // What AppKit hands a window whose Option is an Alt: the key event's
        // characters are the layout's own, `b`, never `∫`.
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

/// The sync (what the event loop runs as every callback ends) carries each
/// change of who takes the typing to the window's setting, and its record is
/// what a press reads.
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
/// With Meta mode off Option composes and nothing here is asked.
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
    // What AppKit hands a window whose Option is an Alt: the key's own character.
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

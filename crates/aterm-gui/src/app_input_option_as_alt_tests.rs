// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who gets Option as an Alt (ATERM-KI-1015-9). In Meta mode a press bound for
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
/// table's glyph, as before ATERM-KI-1015-9.
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

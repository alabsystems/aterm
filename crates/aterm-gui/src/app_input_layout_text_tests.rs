// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layout typing through the shipping `on_key`: a press builds its key from the
//! layout's unshifted key, yet must type what the layout typed (Caps Lock, Shift
//! on a non-US layout), hold that character across auto-repeat, send Ctrl's
//! control byte on a non-Latin layout, and keep Option on the named keys when
//! `option_as_meta = false`.

use crate::{App, WindowId, term_lock};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr};

/// An App whose session writes to an observer pipe (nonblocking read end).
fn app_observing_pty() -> (App, i32) {
    use std::os::fd::{FromRawFd, OwnedFd};
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
    let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    (
        App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer))),
        pipe[0],
    )
}

fn drain(pipe: i32) -> Vec<u8> {
    let mut bytes = [0u8; 64];
    let read = unsafe { libc::read(pipe, bytes.as_mut_ptr().cast(), bytes.len()) };
    if read <= 0 {
        return Vec::new();
    }
    bytes[..read as usize].to_vec()
}

/// A press as the macOS backend builds it: `base` is `key_without_modifiers()`
/// (UCKeyTranslate with no modifiers), `logical`/`text` are `-characters`.
fn mac_press(code: KeyCode, logical: Key, base: &str, text: &str) -> KeyEvent {
    KeyEvent::synthetic_for_test_with_base(
        PhysicalKey::Code(code),
        logical,
        Key::Character(SmolStr::new(base)),
        (!text.is_empty()).then(|| SmolStr::new(text)),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    )
}

fn character(c: &str) -> Key {
    Key::Character(SmolStr::new(c))
}

fn set_mods(app: &mut App, wid: WindowId, mods: ModifiersState) {
    app.windows.get_mut(&wid).expect("window").mods = mods;
}

fn push_kitty(app: &App, wid: WindowId, push: &[u8]) {
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(push);
}

/// German Shift+2 types '"' and keeps typing it while held: the press record
/// carries the layout's character, so an auto-repeat replays it rather than
/// the US table's '@'. AZERTY Shift+& types '1' and Russian Shift+ф 'Ф'.
#[test]
fn a_held_layout_character_repeats_as_typed() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    set_mods(&mut app, wid, ModifiersState::SHIFT);

    app.on_key(wid, mac_press(KeyCode::Digit2, character("\""), "2", "\""));
    assert_eq!(drain(pipe), b"\"", "de-DE Shift+2");
    app.route_physical_repeat(wid, PhysicalKey::Code(KeyCode::Digit2));
    assert_eq!(drain(pipe), b"\"", "its auto-repeat");
    app.release_physical_press(wid, PhysicalKey::Code(KeyCode::Digit2));
    assert!(drain(pipe).is_empty(), "a legacy release writes nothing");

    app.on_key(wid, mac_press(KeyCode::Digit1, character("1"), "&", "1"));
    assert_eq!(drain(pipe), b"1", "AZERTY Shift+&");
    app.on_key(wid, mac_press(KeyCode::KeyA, character("Ф"), "ф", "Ф"));
    assert_eq!(drain(pipe), "Ф".as_bytes(), "Russian Shift+ф");
    unsafe { libc::close(pipe) };
}

/// Caps Lock + A with no lock state from the platform (the Linux backends
/// report none): the layout's 'A' is what reaches the PTY.
#[test]
fn caps_lock_letter_types_the_capital() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    app.on_key(wid, mac_press(KeyCode::KeyA, character("A"), "a", "A"));
    assert_eq!(drain(pipe), b"A");
    unsafe { libc::close(pipe) };
}

/// Russian Ctrl+С interrupts (ETX) and Ctrl+В is EOF; kitty keeps the
/// layout's key.
#[test]
fn ctrl_on_a_cyrillic_layout_writes_the_control_byte() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, mac_press(KeyCode::KeyC, character("с"), "с", "с"));
    assert_eq!(drain(pipe), [0x03], "Ctrl+С");
    app.on_key(wid, mac_press(KeyCode::KeyD, character("в"), "в", "в"));
    assert_eq!(drain(pipe), [0x04], "Ctrl+В");
    push_kitty(&app, wid, b"\x1b[>1u");
    app.on_key(wid, mac_press(KeyCode::KeyC, character("с"), "с", "с"));
    assert_eq!(
        drain(pipe),
        b"\x1b[1089;5u",
        "kitty reports the layout's key"
    );
    unsafe { libc::close(pipe) };
}

/// `option_as_meta = false` lets Option type the layout's characters, but winit
/// gives the named keys a text too ("\x08", "\r", "\t", "\x1b"); they keep Option
/// through the encoder, legacy and kitty. Only printable text takes the text
/// path: Option+a types 'å', and Option+Space a space, not Meta+Space (`ESC SP`
/// mid-word for a German or Nordic typist still holding Option from `|`).
#[test]
fn option_as_meta_off_keeps_option_on_the_named_keys() {
    let named = |name, code, text: &str| {
        KeyEvent::synthetic_for_test_with_base(
            PhysicalKey::Code(code),
            Key::Named(name),
            Key::Named(name),
            Some(SmolStr::new(text)),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        )
    };
    let keys = [
        (NamedKey::Backspace, KeyCode::Backspace, "\x08"),
        (NamedKey::Enter, KeyCode::Enter, "\r"),
        (NamedKey::Tab, KeyCode::Tab, "\t"),
        (NamedKey::Escape, KeyCode::Escape, "\x1b"),
    ];
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    app.option_as_meta = false;
    set_mods(&mut app, wid, ModifiersState::ALT);

    let legacy: [&[u8]; 4] = [b"\x1b\x7f", b"\x1b\r", b"\x1b\t", b"\x1b\x1b"];
    for ((name, code, text), want) in keys.iter().zip(legacy) {
        app.on_key(wid, named(*name, *code, text));
        assert_eq!(drain(pipe), want, "legacy Option+{name:?}");
    }
    app.on_key(wid, mac_press(KeyCode::KeyA, character("å"), "a", "å"));
    assert_eq!(
        drain(pipe),
        "å".as_bytes(),
        "Option+a still types the layout's å"
    );
    app.on_key(wid, named(NamedKey::Space, KeyCode::Space, " "));
    assert_eq!(drain(pipe), b" ", "legacy Option+Space types a space");

    push_kitty(&app, wid, b"\x1b[>1u");
    let kitty: [&[u8]; 4] = [b"\x1b[127;3u", b"\x1b[13;3u", b"\x1b[9;3u", b"\x1b[27;3u"];
    for ((name, code, text), want) in keys.iter().zip(kitty) {
        app.on_key(wid, named(*name, *code, text));
        assert_eq!(drain(pipe), want, "kitty >1 Option+{name:?}");
    }
    app.on_key(wid, named(NamedKey::Space, KeyCode::Space, " "));
    assert_eq!(drain(pipe), b" ", "kitty >1 Option+Space types a space");
    unsafe { libc::close(pipe) };
}

/// A native page's text field types what the terminal would (Shift+a 'A',
/// German Shift+2 '"'), though the press's key is the layout's unshifted key. A
/// keypad digit is its own glyph under Shift ('5'), never the main row's '%'.
#[test]
fn a_native_text_field_types_the_shifted_and_layout_character() {
    use crate::native_settings::SettingsRoute;

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
    set_mods(&mut app, wid, ModifiersState::SHIFT);
    app.on_key(wid, mac_press(KeyCode::KeyA, character("A"), "a", "A"));
    app.release_physical_press(wid, PhysicalKey::Code(KeyCode::KeyA));
    app.on_key(wid, mac_press(KeyCode::Digit2, character("\""), "2", "\""));
    app.release_physical_press(wid, PhysicalKey::Code(KeyCode::Digit2));
    let kp5 = KeyEvent::synthetic_for_test_with_base(
        PhysicalKey::Code(KeyCode::Numpad5),
        character("5"),
        character("5"),
        Some(SmolStr::new("5")),
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    app.on_key(wid, kp5);
    app.release_physical_press(wid, PhysicalKey::Code(KeyCode::Numpad5));
    let (_, view) = app.active_native_view(wid).expect("Settings view");
    let value = match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::Settings(state)) => {
            state.search_input.value().to_string()
        }
        _ => panic!("the Settings view state"),
    };
    assert_eq!(value, "A\"5");
}

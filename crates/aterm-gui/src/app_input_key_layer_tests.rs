// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window key-layer routing tests through `on_key`.

use crate::{App, PhysicalPressOwner, WindowId};
#[cfg(windows)]
use aterm_types::keyboard::KeyEventType;
use aterm_types::keyboard::{Key as TKey, Modifiers, NamedKey as TNamed};
use aterm_window::event::{ElementState, KeyEvent};
#[cfg(windows)]
use aterm_window::keyboard::SmolStr;
use aterm_window::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

fn named_press(code: KeyCode, key: NamedKey) -> KeyEvent {
    KeyEvent::synthetic_for_test(
        PhysicalKey::Code(code),
        Key::Named(key),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    )
}

fn app_with_defaults(mods: ModifiersState) -> (App, WindowId) {
    let mut app = App::headless_for_test();
    app.keybindings = crate::keybinding::Keybindings::resolved(None);
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("window").mods = mods;
    (app, wid)
}

fn forwarded(app: &App, code: KeyCode) -> Option<(TKey, Modifiers, Option<char>)> {
    match app.physical_press_owners.get(&PhysicalKey::Code(code)) {
        Some(PhysicalPressOwner::Forwarded {
            key,
            mods,
            layout_text,
            ..
        }) => Some((key.clone(), *mods, *layout_text)),
        _ => None,
    }
}

#[test]
fn scrollback_chords_reach_the_program_on_the_alternate_screen() {
    let (mut app, wid) = app_with_defaults(ModifiersState::SHIFT);
    let term = app.pool.get(0).expect("session 0").term.clone();

    app.on_key(wid, named_press(KeyCode::PageUp, NamedKey::PageUp));
    assert_eq!(
        forwarded(&app, KeyCode::PageUp),
        None,
        "on the main screen Shift+PageUp is the GUI's"
    );

    crate::term_lock(&term).process(b"\x1b[?1049h");
    app.on_key(wid, named_press(KeyCode::PageUp, NamedKey::PageUp));
    assert_eq!(
        forwarded(&app, KeyCode::PageUp),
        Some((TKey::Named(TNamed::PageUp), Modifiers::SHIFT, None)),
        "on the alternate screen it reaches the program"
    );
}

/// Windows leaves stock PSReadLine its keys: Ctrl+Shift+←/→ (select by word),
/// Ctrl+Shift+Enter (insert a line below) and Shift+Home/End (select to the line's
/// ends) reach the program. Windows Terminal's Ctrl+Shift+Home jumps the view, and
/// Ctrl+Shift+Z zooms a pane.
#[cfg(windows)]
#[test]
fn windows_defaults_leave_psreadline_its_chords() {
    let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let engine_ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
    for (code, key, engine) in [
        (KeyCode::ArrowLeft, NamedKey::ArrowLeft, TNamed::ArrowLeft),
        (
            KeyCode::ArrowRight,
            NamedKey::ArrowRight,
            TNamed::ArrowRight,
        ),
        (KeyCode::Enter, NamedKey::Enter, TNamed::Enter),
    ] {
        let (mut app, wid) = app_with_defaults(ctrl_shift);
        app.on_key(wid, named_press(code, key));
        assert_eq!(
            forwarded(&app, code),
            Some((TKey::Named(engine), engine_ctrl_shift, None)),
            "Ctrl+Shift+{key:?} reaches PSReadLine"
        );
    }
    for (code, key, engine) in [
        (KeyCode::Home, NamedKey::Home, TNamed::Home),
        (KeyCode::End, NamedKey::End, TNamed::End),
    ] {
        let (mut app, wid) = app_with_defaults(ModifiersState::SHIFT);
        app.on_key(wid, named_press(code, key));
        assert_eq!(
            forwarded(&app, code),
            Some((TKey::Named(engine), Modifiers::SHIFT, None)),
            "Shift+{key:?} reaches PSReadLine"
        );
        let (mut app, wid) = app_with_defaults(ctrl_shift);
        app.on_key(wid, named_press(code, key));
        assert_eq!(
            forwarded(&app, code),
            None,
            "Ctrl+Shift+{key:?} moves the view"
        );
    }
}

/// de-DE Shift+7 is `/`: winit hands the window the composed glyph with Shift
/// held, and the press carries the layout's character, so the program gets `/`,
/// not the US table's `?`. A dead key that did not compose (`'` then `s` on
/// US-International) types both characters.
#[cfg(windows)]
#[test]
fn windows_presses_type_the_layouts_characters() {
    let (mut app, wid) = app_with_defaults(ModifiersState::SHIFT);
    let de_7 = KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::Digit7),
        Key::Character(SmolStr::new("/")),
        Some(SmolStr::new("/")),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    app.on_key(wid, de_7);
    let (key, mods, text) = forwarded(&app, KeyCode::Digit7).expect("the press is the program's");
    assert_eq!((key.clone(), text), (TKey::Character('/'), Some('/')));
    assert_eq!(
        aterm_types::keyboard::encode_key_with_text(
            &key,
            mods,
            aterm_types::keyboard::KeyboardMode::WIN32_INPUT,
            KeyEventType::Press,
            Some('7'),
            text,
        ),
        b"/"
    );

    let (mut app, wid) = app_with_defaults(ModifiersState::empty());
    let dead_s = KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::KeyS),
        Key::Character(SmolStr::new("s")),
        Some(SmolStr::new("'s")),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    app.on_key(wid, dead_s);
    assert!(
        matches!(
            app.physical_press_owners.get(&PhysicalKey::Code(KeyCode::KeyS)),
            Some(PhysicalPressOwner::Literal {
                event: crate::input::InputEvent::Text(text),
                ..
            }) if text == "'s"
        ),
        "both characters go out as typed: {:?}",
        app.physical_press_owners
            .get(&PhysicalKey::Code(KeyCode::KeyS))
    );
    for tick in 0..2 {
        app.route_physical_repeat(wid, PhysicalKey::Code(KeyCode::KeyS));
        match crate::app_input::take_physical_release_trace() {
            Some(crate::app_input::PhysicalReleaseTrace::Literal {
                event: crate::input::InputEvent::Text(text),
                repeated: true,
                ..
            }) => assert_eq!(text, "s", "repeat {tick}"),
            other => panic!("repeat {tick} wrote {other:?}"),
        }
    }
}

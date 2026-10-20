// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows key path end to end on RECORDED input: the window messages Windows posts for a
//! hand on a German, French, US or US-International keyboard, or for a Japanese or Korean input
//! method, replayed through winit's real key path (`aterm_window::platform::windows::recorded`: the
//! key event builder, the modifier read, the Alt-code and IME handling, with Windows' answers
//! recorded), then through `App::on_key`, the modifier update and the IME arms, down to the
//! bytes the program reads. Nothing touches the desktop: no window, no `SendInput`, no layout
//! loaded into the session.
//!
//! The bytes of a forwarded key are the encoder's for the key the press left in
//! `physical_press_owners` (what the seam encodes), under the legacy keyboard mode a VT reader
//! sees and under win32-input-mode (DEC 9001), which every console host turns on; a literal press
//! is its text; a commit is `encode_committed_text`'s.

use aterm_window::event::{ElementState, Ime};
use aterm_window::keyboard::ModifiersState;
use aterm_window::platform::windows::recorded::keys::*;
use aterm_window::platform::windows::recorded::{
    Composition, RecordedLayout, Replayed, Typist, layouts, replay,
};

use aterm_types::keyboard::{self, KeyEventType, KeyboardMode};

use crate::{App, PhysicalPressOwner, WindowId};

const WM_IME_STARTCOMPOSITION: u32 = 0x010D;
const WM_IME_ENDCOMPOSITION: u32 = 0x010E;
const WM_IME_COMPOSITION: u32 = 0x010F;
const GCS_COMPATTR: u32 = 0x0010;
const GCS_COMPSTR: u32 = 0x0008;
const GCS_RESULTSTR: u32 = 0x0800;
const VK_NUMLOCK: u16 = 0x90;
const VK_CAPITAL: u16 = 0x14;

/// What `VkKeyScanExW` answers on German (`00000407`) for the characters the recordings type:
/// 1 Shift, 2 Ctrl, 4 Alt (measured on the shipped `KBDGR`, as `keymap::tests`' table).
fn de_de_shift_state(c: char) -> Option<u8> {
    match c {
        '@' | '{' | '[' | ']' | '}' | '\\' | '~' | '|' | '€' | '²' => Some(0b110),
        '/' | '=' | '?' | '"' | '(' | '*' | '>' | '°' => Some(1),
        'a'..='z' | '0'..='9' | 'ß' | '+' | '<' | ' ' => Some(0),
        'A'..='Z' => Some(1),
        _ => None,
    }
}

/// `VkKeyScanExW` on US (`00000409`): nothing needs Ctrl+Alt.
fn en_us_shift_state(c: char) -> Option<u8> {
    match c {
        'a'..='z' | '0'..='9' | '\'' | '/' | ' ' => Some(0),
        'A'..='Z' | '@' | '^' | '&' | '"' | '?' => Some(1),
        _ => None,
    }
}

/// `VkKeyScanExW` on French (`0000040C`) for the characters the recordings type.
fn fr_fr_shift_state(c: char) -> Option<u8> {
    match c {
        '@' | '#' | '[' | ']' | '€' | '~' => Some(0b110),
        '1' | '2' | '3' | '5' | '0' | '°' => Some(1),
        'a'..='z' | '&' | 'é' | '"' | '(' | 'à' | ')' | ' ' => Some(0),
        'A'..='Z' => Some(1),
        _ => None,
    }
}

/// The bytes the program reads for `typed` on `layout` (whose `VkKeyScanExW` is
/// `shift_state`), under keyboard `mode`.
fn bytes(
    layout: &RecordedLayout,
    shift_state: fn(char) -> Option<u8>,
    typed: &Typist,
    mode: KeyboardMode,
) -> Vec<u8> {
    crate::keymap::recorded_layout_shift_state::set(Some(shift_state));
    let mut app = App::headless_for_test();
    app.keybindings = crate::keybinding::Keybindings::resolved(None);
    let wid = WindowId(0);
    let mut out = Vec::new();
    for event in replay(layout, typed.messages()) {
        match event {
            Replayed::Modifiers(mods) => app.on_modifiers_changed(wid, mods),
            Replayed::Key(key) => {
                let physical = key.physical_key;
                let pressed = key.state == ElementState::Pressed;
                let repeat = key.repeat;
                app.on_key(wid, key);
                if !pressed {
                    continue;
                }
                match app.physical_press_owners.get(&physical) {
                    Some(PhysicalPressOwner::Forwarded {
                        key,
                        mods,
                        base_layout,
                        layout_text,
                        ..
                    }) => out.extend(keyboard::encode_key_with_text(
                        key,
                        *mods,
                        mode,
                        if repeat {
                            KeyEventType::Repeat
                        } else {
                            KeyEventType::Press
                        },
                        *base_layout,
                        *layout_text,
                    )),
                    // A repeat writes the press's repeat payload where it has one.
                    Some(PhysicalPressOwner::Literal {
                        repeat: Some(crate::input::InputEvent::Text(text)),
                        ..
                    }) if repeat => out.extend(crate::keymap::encode_committed_text(text, mode)),
                    Some(PhysicalPressOwner::Literal {
                        event: crate::input::InputEvent::Text(text),
                        ..
                    }) => out.extend(crate::keymap::encode_committed_text(text, mode)),
                    Some(PhysicalPressOwner::Consumed { .. }) | None => {}
                    Some(other) => panic!("a press the recordings never make: {other:?}"),
                }
            }
            Replayed::Ime(Ime::Commit(text)) => {
                assert!(!crate::keymap::committed_text_is_paste(&text), "{text:?}");
                out.extend(crate::keymap::encode_committed_text(&text, mode));
                app.on_ime_commit(wid, text);
            }
            Replayed::Ime(Ime::Preedit(text, caret)) => app.on_ime_preedit(wid, text, caret),
            Replayed::Ime(Ime::Enabled | Ime::Disabled) => app.on_ime_enablement_changed(wid),
        }
    }
    crate::keymap::recorded_layout_shift_state::set(None);
    out
}

/// `typed` reads as `want` both under the legacy mode and under win32-input-mode: these are
/// all keys that stay legacy VT under 9001 (`docs/WINDOWS_PARAGON_DESIGN.md` §2.2).
#[track_caller]
fn assert_types(
    layout: &RecordedLayout,
    shift_state: fn(char) -> Option<u8>,
    typed: &Typist,
    want: &[u8],
) {
    for mode in [KeyboardMode::empty(), KeyboardMode::WIN32_INPUT] {
        let got = bytes(layout, shift_state, typed, mode);
        assert_eq!(
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(want),
            "under {mode:?}: {got:?}"
        );
    }
}

fn altgr(key: Physical, text: &str) -> Typist {
    Typist::new()
        .press(LCTRL)
        .press(RALT)
        .tap(key, text)
        .release(LCTRL)
        .release(RALT)
}

fn shifted(key: Physical, text: &str) -> Typist {
    Typist::new().press(LSHIFT).tap(key, text).release(LSHIFT)
}

/// US: letters, Shift, Caps Lock (and Caps Lock with Shift, lowercase), Alt (ESC-prefixed,
/// from `WM_SYSCHAR`), Ctrl (its C0 byte), and Ctrl+Alt, which composes nothing on US and is
/// the chord ESC + C0.
#[test]
fn us_letters_and_chords() {
    let us = layouts::en_us();
    let us = &us;
    assert_types(us, en_us_shift_state, &Typist::new().tap(A, "a"), b"a");
    assert_types(us, en_us_shift_state, &shifted(A, "A"), b"A");
    assert_types(us, en_us_shift_state, &shifted(DIGIT2, "@"), b"@");
    let caps = Typist::new().toggled(VK_CAPITAL);
    assert_types(us, en_us_shift_state, &caps.clone().tap(A, "A"), b"A");
    let caps_shift = caps.press(LSHIFT).tap(A, "a").release(LSHIFT);
    assert_types(us, en_us_shift_state, &caps_shift, b"a");
    let alt = Typist::new().press(LALT).tap(A, "a").release(LALT);
    assert_types(us, en_us_shift_state, &alt, b"\x1ba");
    let ctrl = Typist::new().press(LCTRL).tap(C, "\u{3}").release(LCTRL);
    assert_types(us, en_us_shift_state, &ctrl, b"\x03");
    let ctrl_alt = Typist::new()
        .press(LCTRL)
        .press(LALT)
        .press(A)
        .release(A)
        .release(LALT)
        .release(LCTRL);
    assert_types(us, en_us_shift_state, &ctrl_alt, b"\x1b\x01");
}

/// An Alt code (Alt, 0, 2, 3, 3 on the NumLock-on keypad) types its character once; the
/// digits type nothing. A keypad digit without Alt types the digit to a VT reader, and under
/// win32-input-mode goes out as its `VK_NUMPAD2` record (§2.2's keypad rule).
#[test]
fn an_alt_code_types_its_character() {
    let mut typed = Typist::new().toggled(VK_NUMLOCK).press(LALT);
    for digit in [NUMPAD0, NUMPAD2, NUMPAD3, NUMPAD3] {
        typed = typed.press(digit).release(digit);
    }
    let typed = typed.release(LALT).posted("é");
    assert_types(&layouts::en_us(), en_us_shift_state, &typed, "é".as_bytes());
    let digit = Typist::new().toggled(VK_NUMLOCK).tap(NUMPAD2, "2");
    let us = layouts::en_us();
    assert_eq!(
        bytes(&us, en_us_shift_state, &digit, KeyboardMode::empty()),
        b"2"
    );
    assert_eq!(
        bytes(&us, en_us_shift_state, &digit, KeyboardMode::WIN32_INPUT),
        b"\x1b[98;80;50;1;32;1_\x1b[98;80;50;0;32;1_"
    );
}

/// Text `SendInput` types (`VK_PACKET`: an on-screen keyboard, the emoji panel) arrives as
/// typed text.
#[test]
fn packet_text_is_typed() {
    let typed = Typist::new().tap(PACKET, "😀").tap(PACKET, "é");
    assert_types(
        &layouts::en_us(),
        en_us_shift_state,
        &typed,
        "😀é".as_bytes(),
    );
}

/// German: AltGr (the right Alt, which Windows announces with a left Ctrl it then drops)
/// types `@`, `{`, `€` and `\`; so does left Ctrl + left Alt, which Windows takes as AltGr;
/// Shift types the layout's `/` and `=`, not the US table's `?` and `+`; Alt+Shift+7 is
/// `ESC /`; and the keypad's decimal key is the layout's `,`.
#[test]
fn german_altgr_and_shift() {
    let de = layouts::de_de();
    let de = &de;
    assert_types(de, de_de_shift_state, &altgr(Q, "@"), b"@");
    assert_types(de, de_de_shift_state, &altgr(DIGIT7, "{"), b"{");
    assert_types(de, de_de_shift_state, &altgr(E, "€"), "€".as_bytes());
    assert_types(de, de_de_shift_state, &altgr(DE_SHARP_S, "\\"), b"\\");
    let left_ctrl_alt = Typist::new()
        .press(LCTRL)
        .press(LALT)
        .tap(Q, "@")
        .release(LALT)
        .release(LCTRL);
    assert_types(de, de_de_shift_state, &left_ctrl_alt, b"@");
    assert_types(de, de_de_shift_state, &shifted(DIGIT7, "/"), b"/");
    assert_types(de, de_de_shift_state, &shifted(DIGIT0, "="), b"=");
    assert_types(de, de_de_shift_state, &shifted(DIGIT2, "\""), b"\"");
    let alt_shift_7 = Typist::new()
        .press(LALT)
        .press(LSHIFT)
        .tap(DIGIT7, "/")
        .release(LSHIFT)
        .release(LALT);
    assert_types(de, de_de_shift_state, &alt_shift_7, b"\x1b/");
    // The keypad's decimal key types the layout's `,`.
    let decimal = Typist::new().toggled(VK_NUMLOCK).tap(NUMPAD_DECIMAL, ",");
    assert_types(de, de_de_shift_state, &decimal, b",");
}

/// German dead keys: `^` then `e` is `ê`; `^` then Space is `^` (it typed a space, and the
/// accent was lost, before `keymap::windows_text_commit` took the Space press's text); held,
/// that Space repeats spaces; `^` twice is `^^`.
#[test]
fn german_dead_keys() {
    let de = layouts::de_de();
    let circumflex = || {
        Typist::new()
            .press(DE_CIRCUMFLEX)
            .dead('^')
            .release(DE_CIRCUMFLEX)
    };
    assert_types(
        &de,
        de_de_shift_state,
        &circumflex().tap(E, "ê"),
        "ê".as_bytes(),
    );
    assert_types(&de, de_de_shift_state, &circumflex().tap(SPACE, "^"), b"^");
    let held = circumflex()
        .press(SPACE)
        .chars("^")
        .press(SPACE)
        .chars(" ")
        .press(SPACE)
        .chars(" ")
        .release(SPACE);
    assert_types(&de, de_de_shift_state, &held, b"^  ");
    assert_types(
        &de,
        de_de_shift_state,
        &circumflex().tap(DE_CIRCUMFLEX, "^^"),
        b"^^",
    );
}

/// US-International: `'` then `e` is `é`, then `s` (which does not compose) is `'s`, then
/// Space is `'`; AltGr+Q is `ä`.
#[test]
fn us_international_dead_keys_and_altgr() {
    let intl = layouts::en_us_intl();
    let quote = || Typist::new().press(QUOTE).dead('\'').release(QUOTE);
    assert_types(
        &intl,
        en_us_shift_state,
        &quote().tap(E, "é"),
        "é".as_bytes(),
    );
    assert_types(&intl, en_us_shift_state, &quote().tap(S, "'s"), b"'s");
    assert_types(&intl, en_us_shift_state, &quote().tap(SPACE, "'"), b"'");
    let double_quote = Typist::new()
        .press(LSHIFT)
        .press(QUOTE)
        .dead('"')
        .release(QUOTE)
        .tap(SPACE, "\"")
        .release(LSHIFT);
    assert_types(&intl, en_us_shift_state, &double_quote, b"\"");
    assert_types(&intl, en_us_shift_state, &altgr(Q, "ä"), "ä".as_bytes());
}

/// French AZERTY: the digit row types `&` and `é`, Shift the digits; AltGr+à is `@`; the
/// US Q position types `a`; `^` then `e` is `ê`; AltGr+é is the dead `~`, then Space `~`.
#[test]
fn french_azerty() {
    let fr = layouts::fr_fr();
    let fr = &fr;
    assert_types(fr, fr_fr_shift_state, &Typist::new().tap(FR_1, "&"), b"&");
    assert_types(fr, fr_fr_shift_state, &shifted(FR_1, "1"), b"1");
    assert_types(
        fr,
        fr_fr_shift_state,
        &Typist::new().tap(DIGIT2, "é"),
        "é".as_bytes(),
    );
    assert_types(fr, fr_fr_shift_state, &shifted(DIGIT2, "2"), b"2");
    assert_types(fr, fr_fr_shift_state, &altgr(DIGIT0, "@"), b"@");
    assert_types(fr, fr_fr_shift_state, &Typist::new().tap(FR_A, "a"), b"a");
    let circumflex = Typist::new()
        .press(FR_CIRCUMFLEX)
        .dead('^')
        .release(FR_CIRCUMFLEX)
        .tap(E, "ê");
    assert_types(fr, fr_fr_shift_state, &circumflex, "ê".as_bytes());
    let tilde = Typist::new()
        .press(LCTRL)
        .press(RALT)
        .press(DIGIT2)
        .dead('~')
        .release(DIGIT2)
        .release(LCTRL)
        .release(RALT)
        .tap(SPACE, "~");
    assert_types(fr, fr_fr_shift_state, &tilde, b"~");
}

/// A Japanese IME: the keys it takes (`VK_PROCESSKEY`) and their releases type nothing, and
/// the converted text reaches the program once, when it is committed.
#[test]
fn a_japanese_commit_types_once() {
    let converted = "日本語";
    let typed = Typist::new()
        .ime(WM_IME_STARTCOMPOSITION, 0, Composition::default())
        .press(PROCESS_N)
        .ime(
            WM_IME_COMPOSITION,
            GCS_COMPSTR | GCS_COMPATTR,
            Composition {
                composing: Some("に".into()),
                attrs: vec![0],
                cursor: 1,
                ..Composition::default()
            },
        )
        .release(N)
        .press(PROCESS_SPACE)
        .ime(
            WM_IME_COMPOSITION,
            GCS_COMPSTR | GCS_COMPATTR,
            Composition {
                composing: Some(converted.into()),
                attrs: vec![1, 1, 1],
                cursor: 3,
                ..Composition::default()
            },
        )
        .release(SPACE)
        .press(PROCESS_ENTER)
        .ime(
            WM_IME_COMPOSITION,
            GCS_RESULTSTR,
            Composition {
                result: Some(converted.into()),
                ..Composition::default()
            },
        )
        .ime(WM_IME_ENDCOMPOSITION, 0, Composition::default())
        .release(ENTER);
    assert_types(
        &layouts::ja_jp(),
        en_us_shift_state,
        &typed,
        converted.as_bytes(),
    );
}

/// The Korean IME commits at the end of a composition and sends the result again after it:
/// the program reads the syllable once.
#[test]
fn a_hangul_commit_types_once() {
    let syllable = || Composition {
        result: Some("한".into()),
        ..Composition::default()
    };
    let typed = Typist::new()
        .ime(WM_IME_STARTCOMPOSITION, 0, Composition::default())
        .ime(
            WM_IME_COMPOSITION,
            GCS_COMPSTR,
            Composition {
                composing: Some("한".into()),
                cursor: 1,
                ..Composition::default()
            },
        )
        .ime(WM_IME_ENDCOMPOSITION, 0, syllable())
        .ime(WM_IME_COMPOSITION, GCS_RESULTSTR, syllable());
    assert_types(
        &layouts::ko_kr(),
        en_us_shift_state,
        &typed,
        "한".as_bytes(),
    );
    // Space ends the composition and then types itself; the composition held no key back.
    let then_space = typed.tap(SPACE, " ");
    assert_types(
        &layouts::ko_kr(),
        en_us_shift_state,
        &then_space,
        "한 ".as_bytes(),
    );
}

/// The modifiers the window reports while AltGr is down are none, so no Ctrl or Alt binding
/// can take an AltGr character (the left Ctrl Windows posts first does reach the window as a
/// modifier, for a moment, with no key event).
#[test]
fn altgr_holds_no_modifiers() {
    let events = replay(&layouts::de_de(), altgr(Q, "@").messages());
    let mods: Vec<ModifiersState> = events
        .iter()
        .filter_map(|event| match event {
            Replayed::Modifiers(mods) => Some(*mods),
            _ => None,
        })
        .collect();
    assert_eq!(
        mods,
        [ModifiersState::CONTROL, ModifiersState::empty()],
        "{events:#?}"
    );
}

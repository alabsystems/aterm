// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential tests comparing the input decoder against aterm's encoders.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_tty::decode::{
    Decoder, ESC_TIMEOUT, Event, MAX_PASTE_BYTES, ModeState, MouseKind, PASTE_TIMEOUT, Reply,
};
use aterm_types::keyboard::{
    Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key_with_text, shifted_character,
};
use aterm_types::mouse::{
    ALT_MASK, CTRL_MASK, MouseButton, MouseEncoding, SHIFT_MASK, WheelDir, encode_mouse, encode_sgr,
};

fn decode_all(bytes: &[u8]) -> Vec<Event> {
    let t = Instant::now();
    let mut decoder = Decoder::new();
    let mut events = decoder.feed(bytes, t);
    events.extend(decoder.flush(t + ESC_TIMEOUT));
    events
}

fn encode(key: &Key, mods: Modifiers, mode: KeyboardMode) -> Vec<u8> {
    encode_key_with_text(key, mods, mode, KeyEventType::Press, None, None)
}

fn key(key: Key, mods: Modifiers, text: Option<char>) -> Event {
    Event::Key { key, mods, text }
}

fn named(named: NamedKey, mods: Modifiers) -> Event {
    key(Key::Named(named), mods, None)
}

fn chord(c: char, mods: Modifiers) -> Event {
    key(Key::Character(c), mods, None)
}

fn typed(g: char) -> Event {
    match g {
        ' ' => key(Key::Named(NamedKey::Space), Modifiers::empty(), Some(' ')),
        'A'..='Z' => key(
            Key::Character(g.to_ascii_lowercase()),
            Modifiers::SHIFT,
            Some(g),
        ),
        _ => key(Key::Character(g), Modifiers::empty(), Some(g)),
    }
}

fn alt(event: Event) -> Event {
    match event {
        Event::Key { key: k, mods, .. } => key(k, mods | Modifiers::ALT, None),
        other => other,
    }
}

fn ctrl_byte(c: char) -> Option<u8> {
    if c.is_ascii_alphabetic() {
        return Some(c.to_ascii_uppercase() as u8 - b'@');
    }
    Some(match c {
        ' ' | '@' | '2' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' | '~' => 0x1e,
        '_' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

fn c0(byte: u8) -> Event {
    match byte {
        0x00 => named(NamedKey::Space, Modifiers::CTRL),
        0x08 | 0x7f => named(NamedKey::Backspace, Modifiers::empty()),
        0x09 => named(NamedKey::Tab, Modifiers::empty()),
        0x0d => named(NamedKey::Enter, Modifiers::empty()),
        0x1b => named(NamedKey::Escape, Modifiers::empty()),
        0x1c => chord('\\', Modifiers::CTRL),
        0x1d => chord(']', Modifiers::CTRL),
        0x1e => chord('^', Modifiers::CTRL),
        0x1f => chord('_', Modifiers::CTRL),
        b => chord(char::from(b + b'`'), Modifiers::CTRL),
    }
}

const LOCKS: Modifiers = Modifiers::CAPS_LOCK.union(Modifiers::NUM_LOCK);

fn kitty_reading(k: &Key, mods: Modifiers, alternates: bool) -> Event {
    let mods = mods - LOCKS;
    match k.main_block_twin() {
        Some(Key::Character(glyph)) if (mods - Modifiers::SHIFT).is_empty() => {
            return typed(glyph);
        }
        Some(twin) => return key(twin, mods, None),
        None => {}
    }
    match *k {
        Key::Character(c) => {
            if alternates && let Some(glyph) = shifted_character(c, mods).filter(|&g| g != c) {
                let Event::Key {
                    key: k,
                    mods: glyph_mods,
                    ..
                } = typed(glyph)
                else {
                    unreachable!("a glyph reads as a key");
                };
                return key(k, (mods - Modifiers::SHIFT) | glyph_mods, None);
            }
            let k = if c == ' ' {
                Key::Named(NamedKey::Space)
            } else {
                Key::Character(c)
            };
            key(k, mods, None)
        }
        _ => key(k.clone(), mods, None),
    }
}

fn expected(k: &Key, mods: Modifiers, mode: KeyboardMode) -> Event {
    let kitty = mode.contains(KITTY_FLAG_1);
    let alternates = mode.contains(KeyboardMode::REPORT_ALTERNATE_KEYS);
    let (shift, alt_held, ctrl) = (
        mods.contains(Modifiers::SHIFT),
        mods.contains(Modifiers::ALT),
        mods.contains(Modifiers::CTRL),
    );
    match *k {
        Key::Named(n) => match n {
            NamedKey::Space if !alt_held && !ctrl => typed(' '),
            NamedKey::Space if !kitty => named(n, mods - Modifiers::SHIFT),
            NamedKey::Backspace if kitty && !alt_held && !ctrl => named(n, Modifiers::empty()),
            _ if kitty => kitty_reading(k, mods, alternates),
            NamedKey::Enter if alt_held => named(n, Modifiers::ALT),
            NamedKey::Enter if shift && !ctrl => chord('j', Modifiers::CTRL),
            NamedKey::Enter => named(n, Modifiers::empty()),
            NamedKey::Tab if shift => named(n, Modifiers::SHIFT),
            NamedKey::Tab => named(n, mods & Modifiers::ALT),
            NamedKey::Escape | NamedKey::Backspace => named(n, mods & Modifiers::ALT),
            _ => named(n, mods),
        },
        Key::Character(c) => {
            let chorded = alt_held || ctrl;
            if kitty && chorded {
                return kitty_reading(k, mods, alternates);
            }
            let glyph = shifted_character(c, mods).unwrap_or(c);
            if ctrl && let Some(byte) = ctrl_byte(c).or_else(|| ctrl_byte(glyph)) {
                let event = c0(byte);
                return if alt_held { alt(event) } else { event };
            }
            if alt_held {
                return alt(typed(if shift { glyph } else { c.to_ascii_lowercase() }));
            }
            typed(glyph)
        }
        _ => panic!("no other key kind in the table: {k:?}"),
    }
}

fn mod_combos() -> Vec<Modifiers> {
    (0u8..8)
        .map(|bits| {
            let mut mods = Modifiers::empty();
            mods.set(Modifiers::SHIFT, bits & 1 != 0);
            mods.set(Modifiers::ALT, bits & 2 != 0);
            mods.set(Modifiers::CTRL, bits & 4 != 0);
            mods
        })
        .collect()
}

fn key_table() -> Vec<Key> {
    use NamedKey::*;
    let mut keys: Vec<Key> = (' '..='~').map(Key::Character).collect();
    keys.extend(['é', 'ß', 'ж', 'É', '日', '😀'].map(Key::Character));
    keys.extend(
        [
            F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, ArrowUp, ArrowDown, ArrowLeft,
            ArrowRight, Home, End, PageUp, PageDown, Insert, Delete, Tab, Enter, Backspace, Escape,
            Space,
        ]
        .map(Key::Named),
    );
    keys
}

const LEGACY: KeyboardMode = KeyboardMode::empty();
const KITTY_FLAG_1: KeyboardMode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
const KITTY_FLAGS_1_4: KeyboardMode = KITTY_FLAG_1.union(KeyboardMode::REPORT_ALTERNATE_KEYS);

#[test]
fn keys_round_trip_through_encode_key_with_text() {
    let modes = [
        ("legacy", LEGACY),
        ("application cursor", KeyboardMode::APP_CURSOR),
        ("kitty flag 1", KITTY_FLAG_1),
        ("kitty flags 1|4", KITTY_FLAGS_1_4),
    ];
    let mut exact = 0;
    let mut cases = 0;
    for (name, mode) in modes {
        for k in key_table() {
            for mods in mod_combos() {
                let bytes = encode(&k, mods, mode);
                assert!(
                    !bytes.is_empty(),
                    "{name}: {k:?} {mods:?} encodes to nothing"
                );
                let want = expected(&k, mods, mode);
                assert_eq!(
                    decode_all(&bytes),
                    std::slice::from_ref(&want),
                    "{name}: {k:?} with {mods:?}, sent as {bytes:?}"
                );
                let Event::Key {
                    key: got,
                    mods: got_mods,
                    ..
                } = &want
                else {
                    unreachable!("a key decodes to a key");
                };
                let respell = if bytes.contains(&0x08) {
                    mode | KeyboardMode::BACKARROW_SENDS_BS
                } else {
                    mode
                };
                let shifted_key = bytes.starts_with(b"\x1b[") && bytes.contains(&b':');
                if shifted_key {
                    if !mods.contains(Modifiers::CTRL) {
                        assert_eq!(
                            decode_all(&encode(&k, mods, LEGACY)),
                            std::slice::from_ref(&want),
                            "{name}: {k:?} with {mods:?} reads otherwise in legacy mode"
                        );
                    }
                } else {
                    assert_eq!(
                        encode(got, *got_mods, respell),
                        bytes,
                        "{name}: {k:?} with {mods:?} decodes to {want:?}, which aterm spells otherwise"
                    );
                }
                cases += 1;
                if *got == k && *got_mods == mods {
                    exact += 1;
                }
            }
        }
    }
    let table = key_table().len() * 8 * 4;
    assert_eq!(cases, table);
    assert!(
        exact * 2 > cases,
        "only {exact} of {cases} presses read back exactly"
    );
}

fn all_named_keys() -> [NamedKey; 110] {
    use NamedKey::*;
    [
        ArrowUp,
        ArrowDown,
        ArrowLeft,
        ArrowRight,
        Home,
        End,
        PageUp,
        PageDown,
        Backspace,
        Delete,
        Insert,
        Enter,
        Tab,
        Escape,
        Space,
        CapsLock,
        NumLock,
        ScrollLock,
        PrintScreen,
        Pause,
        ContextMenu,
        F1,
        F2,
        F3,
        F4,
        F5,
        F6,
        F7,
        F8,
        F9,
        F10,
        F11,
        F12,
        F13,
        F14,
        F15,
        F16,
        F17,
        F18,
        F19,
        F20,
        F21,
        F22,
        F23,
        F24,
        F25,
        F26,
        F27,
        F28,
        F29,
        F30,
        F31,
        F32,
        F33,
        F34,
        F35,
        MediaPlay,
        MediaPause,
        MediaPlayPause,
        MediaReverse,
        MediaStop,
        MediaFastForward,
        MediaRewind,
        MediaTrackNext,
        MediaTrackPrevious,
        MediaRecord,
        AudioVolumeDown,
        AudioVolumeUp,
        AudioVolumeMute,
        ShiftLeft,
        ShiftRight,
        ControlLeft,
        ControlRight,
        AltLeft,
        AltRight,
        SuperLeft,
        SuperRight,
        HyperLeft,
        HyperRight,
        MetaLeft,
        MetaRight,
        Numpad0,
        Numpad1,
        Numpad2,
        Numpad3,
        Numpad4,
        Numpad5,
        Numpad6,
        Numpad7,
        Numpad8,
        Numpad9,
        NumpadDecimal,
        NumpadDivide,
        NumpadMultiply,
        NumpadSubtract,
        NumpadAdd,
        NumpadEnter,
        NumpadEqual,
        NumpadSeparator,
        NumpadArrowLeft,
        NumpadArrowRight,
        NumpadArrowUp,
        NumpadArrowDown,
        NumpadPageUp,
        NumpadPageDown,
        NumpadHome,
        NumpadEnd,
        NumpadInsert,
        NumpadDelete,
        NumpadBegin,
    ]
}

#[test]
fn every_named_key_round_trips_under_kitty_flag_1() {
    use NamedKey::*;
    let all = all_named_keys();
    let mut codes: Vec<u32> = all.iter().map(|k| k.kitty_code()).collect();
    codes.sort_unstable();
    let mut want: Vec<u32> = vec![9, 13, 27, 32, 127];
    want.extend(57348..=57452);
    assert_eq!(codes, want);

    for k in all {
        let bare = if k == Space {
            typed(' ')
        } else {
            kitty_reading(&Key::Named(k), Modifiers::empty(), false)
        };
        assert_eq!(
            decode_all(format!("\x1b[{}u", k.kitty_code()).as_bytes()),
            [bare],
            "{k:?}"
        );
        for mods in mod_combos() {
            let bytes = encode(&Key::Named(k), mods, KITTY_FLAG_1);
            if bytes.is_empty() {
                assert!(
                    k.modifier_twin().is_some() || matches!(k, CapsLock | NumLock | ScrollLock),
                    "{k:?} encodes to nothing"
                );
                continue;
            }
            let want = expected(&Key::Named(k), mods, KITTY_FLAG_1);
            assert_eq!(decode_all(&bytes), [want], "{k:?} {mods:?} as {bytes:?}");
        }
    }
}

#[test]
fn every_named_key_aterm_encodes_in_legacy_mode_reads_back() {
    use NamedKey::*;
    let modes = [
        LEGACY,
        KeyboardMode::APP_CURSOR,
        KeyboardMode::APP_KEYPAD,
        KeyboardMode::APP_CURSOR.union(KeyboardMode::APP_KEYPAD),
    ];
    let mut read = 0;
    let mut ss3_keypad = 0;
    for mode in modes {
        for k in all_named_keys() {
            if encode(&Key::Named(k), Modifiers::empty(), mode).is_empty() {
                for mods in mod_combos() {
                    assert!(
                        encode(&Key::Named(k), mods, mode).is_empty(),
                        "{k:?} {mods:?}"
                    );
                }
                continue;
            }
            for mods in mod_combos() {
                let bytes = encode(&Key::Named(k), mods, mode);
                let keypad_glyph =
                    matches!(Key::Named(k).main_block_twin(), Some(Key::Character(_)));
                if bytes.starts_with(b"\x1bO") && (keypad_glyph || k == NumpadEnter) {
                    ss3_keypad += 1;
                }
                let want = match (k, Key::Named(k).main_block_twin()) {
                    (NumpadEqual, _) => expected(&Key::Character('='), mods, LEGACY),
                    (_, Some(Key::Character(glyph))) if mods.contains(Modifiers::ALT) => {
                        alt(typed(glyph))
                    }
                    (_, Some(Key::Character(glyph))) => typed(glyph),
                    (NumpadEnter, _) => expected(&Key::Named(Enter), mods, LEGACY),
                    (NumpadBegin, _) => named(NumpadBegin, mods & Modifiers::ALT),
                    (_, Some(twin)) => expected(&twin, mods, LEGACY),
                    _ => expected(&Key::Named(k), mods, LEGACY),
                };
                assert_eq!(
                    decode_all(&bytes),
                    std::slice::from_ref(&want),
                    "{k:?} with {mods:?} in {mode:?}, sent as {bytes:?}"
                );
                read += 1;
            }
        }
    }
    assert_eq!(read, 4 * 8 * (27 + 12 + 29));
    assert_eq!(ss3_keypad, 2 * 2 * 17);
}

#[test]
fn legacy_ambiguities_decode_canonically_and_kitty_flag_1_tells_them_apart() {
    let none = Modifiers::empty();
    let ctrl = Modifiers::CTRL;
    let pairs = [
        ('i', named(NamedKey::Tab, none)),
        ('m', named(NamedKey::Enter, none)),
        ('h', named(NamedKey::Backspace, none)),
        ('[', named(NamedKey::Escape, none)),
        ('@', named(NamedKey::Space, ctrl)),
    ];
    for (c, canonical) in pairs {
        let pressed = Key::Character(c);
        assert_eq!(
            decode_all(&encode(&pressed, ctrl, LEGACY)),
            std::slice::from_ref(&canonical)
        );
        assert_eq!(
            decode_all(&encode(&pressed, ctrl, KITTY_FLAG_1)),
            [chord(c, ctrl)]
        );
        let Event::Key { key: k, mods, .. } = &canonical else {
            unreachable!()
        };
        assert_eq!(
            decode_all(&encode(k, *mods, LEGACY)),
            std::slice::from_ref(&canonical)
        );
        assert_eq!(
            decode_all(&encode(k, *mods, KITTY_FLAG_1)),
            std::slice::from_ref(&canonical)
        );
    }
    let bs = Key::Named(NamedKey::Backspace);
    assert_eq!(encode(&bs, ctrl, LEGACY), [0x08]);
    assert_eq!(decode_all(&[0x08]), [named(NamedKey::Backspace, none)]);
    assert_eq!(
        decode_all(&encode(&bs, ctrl, KITTY_FLAG_1)),
        [named(NamedKey::Backspace, ctrl)]
    );
    let enter = Key::Named(NamedKey::Enter);
    assert_eq!(
        decode_all(&encode(&enter, Modifiers::SHIFT, LEGACY)),
        [chord('j', ctrl)]
    );
    assert_eq!(
        decode_all(&encode(&enter, Modifiers::SHIFT, KITTY_FLAG_1)),
        [named(NamedKey::Enter, Modifiers::SHIFT)]
    );
    assert_eq!(
        decode_all(b"\x1b\x08"),
        [named(NamedKey::Backspace, Modifiers::ALT)]
    );
}

#[test]
fn the_same_press_reads_the_same_in_every_mode() {
    use NamedKey::*;
    let none = Modifiers::empty();
    let modes = [
        ("legacy", LEGACY),
        ("application keypad", KeyboardMode::APP_KEYPAD),
        ("kitty flag 1", KITTY_FLAG_1),
        ("kitty flags 1|4", KITTY_FLAGS_1_4),
    ];
    let mut presses: Vec<(Key, Modifiers, Event)> = vec![
        (Key::Named(Numpad5), none, typed('5')),
        (Key::Named(Numpad5), Modifiers::SHIFT, typed('5')),
        (Key::Named(Numpad0), none, typed('0')),
        (Key::Named(NumpadAdd), none, typed('+')),
        (Key::Named(NumpadDecimal), none, typed('.')),
        (Key::Named(NumpadSeparator), none, typed(',')),
        (Key::Named(NumpadEqual), none, typed('=')),
        (Key::Named(NumpadEnter), none, named(Enter, none)),
        (Key::Named(NumpadArrowUp), none, named(ArrowUp, none)),
        (Key::Named(NumpadDelete), none, named(Delete, none)),
        (Key::Named(Numpad7), Modifiers::ALT, alt(typed('7'))),
        (
            Key::Character('a'),
            Modifiers::CTRL | Modifiers::CAPS_LOCK,
            chord('a', Modifiers::CTRL),
        ),
        (
            Key::Character('x'),
            Modifiers::ALT | Modifiers::NUM_LOCK,
            alt(typed('x')),
        ),
        (
            Key::Named(ArrowUp),
            Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK,
            named(ArrowUp, none),
        ),
        (Key::Named(Numpad5), Modifiers::NUM_LOCK, typed('5')),
    ];
    let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
    for (base, glyph) in [(',', '<'), ('.', '>'), ('5', '%'), ('1', '!'), ('[', '{')] {
        presses.push((
            Key::Character(base),
            alt_shift,
            chord(glyph, Modifiers::ALT),
        ));
    }
    for (name, mode) in modes {
        for (k, mods, want) in &presses {
            let bytes = encode(k, *mods, mode);
            let want = match (mode == KITTY_FLAG_1, k) {
                (true, Key::Character(base)) if mods.contains(Modifiers::SHIFT) => {
                    chord(*base, *mods)
                }
                _ => want.clone(),
            };
            assert_eq!(
                decode_all(&bytes),
                [want],
                "{name}: {k:?} with {mods:?}, sent as {bytes:?}"
            );
        }
    }
}

#[test]
fn every_modifier_and_lock_reads_back() {
    let modes = [
        ("legacy", LEGACY),
        ("application cursor", KeyboardMode::APP_CURSOR),
        ("application keypad", KeyboardMode::APP_KEYPAD),
        ("kitty flag 1", KITTY_FLAG_1),
        ("kitty flags 1|4", KITTY_FLAGS_1_4),
    ];
    let mut keys = key_table();
    keys.extend(all_named_keys().map(Key::Named));
    let mut read = 0;
    for (name, mode) in modes {
        let kitty = mode.contains(KITTY_FLAG_1);
        let alternates = mode.contains(KeyboardMode::REPORT_ALTERNATE_KEYS);
        for k in &keys {
            for bits in 0..u8::MAX {
                let mods = Modifiers::from_bits_retain(bits);
                let bytes = encode(k, mods, mode);
                if bytes.is_empty() {
                    continue;
                }
                let events = decode_all(&bytes);
                let [
                    Event::Key {
                        key: got,
                        mods: got_mods,
                        ..
                    },
                ] = events.as_slice()
                else {
                    panic!("{name}: {k:?} with {mods:?}, sent as {bytes:?}: {events:?}");
                };
                assert!(
                    !got_mods.intersects(LOCKS),
                    "{name}: {k:?} with {mods:?} reads with a lock: {events:?}"
                );
                if kitty && bytes.starts_with(b"\x1b[") {
                    assert_eq!(
                        events,
                        [kitty_reading(k, mods, alternates)],
                        "{name}: {k:?} with {mods:?}, sent as {bytes:?}"
                    );
                } else if mode.contains(KeyboardMode::APP_KEYPAD)
                    && (k.main_block_twin().is_some() || *k == Key::Named(NamedKey::NumpadBegin))
                {
                    let legacy = mode - KeyboardMode::APP_KEYPAD;
                    assert_eq!(
                        events,
                        decode_all(&encode(k, mods, legacy)),
                        "{name}: {k:?} with {mods:?}, sent as {bytes:?}"
                    );
                } else {
                    let respell = if bytes.contains(&0x08) {
                        mode | KeyboardMode::BACKARROW_SENDS_BS
                    } else {
                        mode
                    };
                    let locks = [mods & LOCKS, Modifiers::empty()];
                    assert!(
                        locks
                            .iter()
                            .any(|&locks| encode(got, *got_mods | locks, respell) == bytes),
                        "{name}: {k:?} with {mods:?}, sent as {bytes:?}, reads as {events:?}, \
                         which aterm spells otherwise"
                    );
                }
                read += 1;
            }
        }
    }
    assert!(read > 250_000, "only {read} presses encode to anything");
}

fn mask_mods(mask: u8) -> Modifiers {
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::SHIFT, mask & SHIFT_MASK != 0);
    mods.set(Modifiers::ALT, mask & ALT_MASK != 0);
    mods.set(Modifiers::CTRL, mask & CTRL_MASK != 0);
    mods
}

fn mouse(
    kind: MouseKind,
    button: Option<MouseButton>,
    col: u16,
    row: u16,
    mods: Modifiers,
) -> Event {
    Event::Mouse {
        kind,
        button,
        col,
        row,
        mods,
    }
}

#[test]
fn mouse_round_trips_through_encode_sgr_and_encode_mouse() {
    let buttons = [
        MouseButton::Left,
        MouseButton::Middle,
        MouseButton::Right,
        MouseButton::Back,
        MouseButton::Forward,
    ];
    let wheels = [
        WheelDir::Up,
        WheelDir::Down,
        WheelDir::Left,
        WheelDir::Right,
    ];
    let coords = [0u16, 1, 94, 95, 222, 223, 999, 1999, 2000];
    let masks: Vec<u8> = (0u8..8)
        .map(|b| {
            (if b & 1 != 0 { SHIFT_MASK } else { 0 })
                | (if b & 2 != 0 { ALT_MASK } else { 0 })
                | (if b & 4 != 0 { CTRL_MASK } else { 0 })
        })
        .collect();
    let mut checked = 0;
    for &mask in &masks {
        let mods = mask_mods(mask);
        for &col in &coords {
            for &row in &coords {
                let mut reports = Vec::new();
                for button in buttons {
                    let cb = button.code() | mask;
                    let b = Some(button);
                    reports.push((cb, false, mouse(MouseKind::Press, b, col, row, mods)));
                    reports.push((cb, true, mouse(MouseKind::Release, b, col, row, mods)));
                    reports.push((cb | 32, false, mouse(MouseKind::Motion, b, col, row, mods)));
                }
                reports.push((
                    3 | 32 | mask,
                    false,
                    mouse(MouseKind::Motion, None, col, row, mods),
                ));
                for dir in wheels {
                    let wheel = mouse(MouseKind::Wheel(dir), None, col, row, mods);
                    reports.push((dir.code() | mask, false, wheel));
                }
                for (cb, release, want) in reports {
                    let sgr = encode_sgr(cb, col, row, release);
                    assert_eq!(decode_all(&sgr), std::slice::from_ref(&want), "{sgr:?}");
                    assert_eq!(
                        encode_mouse(cb, col, row, MouseEncoding::Sgr, release),
                        sgr,
                        "encode_mouse's SGR is encode_sgr"
                    );
                    if col <= 222 && row <= 222 {
                        let x10 = encode_mouse(cb, col, row, MouseEncoding::X10, release);
                        let want = match want {
                            Event::Mouse {
                                kind: MouseKind::Release,
                                ..
                            } => mouse(MouseKind::Release, None, col, row, mods),
                            other => other,
                        };
                        assert_eq!(decode_all(&x10), [want], "{x10:?}");
                    }
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(
        checked,
        masks.len() * coords.len() * coords.len() * (5 * 3 + 1 + 4)
    );
}

#[test]
fn paste_and_focus_round_trip() {
    let term = Terminal::new(24, 80);
    for text in [
        "hello",
        "",
        "two\nlines\r\nand a bare cr\r",
        "tab\there",
        "unicode \u{65e5}\u{672c}\u{8a9e} \u{1f600}",
        "the end marker \x1b[201~ cannot end it early",
    ] {
        let framed = term.format_paste_framed(text, true);
        let body = framed
            .strip_prefix(b"\x1b[200~")
            .and_then(|rest| rest.strip_suffix(b"\x1b[201~"))
            .expect("aterm frames a bracketed paste");
        let want = String::from_utf8(body.to_vec()).expect("aterm pastes UTF-8");
        assert_eq!(
            decode_all(&framed),
            [Event::Paste(want.clone())],
            "{text:?}"
        );
        let unframed = want.replace('\r', "\n");
        let original = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\x1b', "");
        assert_eq!(unframed, original);
    }

    let body = "x\x1b[201y\x1b[20\x1b[2015~z\x1b[A\x03\x1b";
    let stream = [b"\x1b[200~".as_slice(), body.as_bytes(), b"\x1b[201~", b"a"].concat();
    let want = [Event::Paste(body.to_owned()), typed('a')];
    assert_eq!(decode_all(&stream), want);
    let t = Instant::now();
    for split in 0..=stream.len() {
        let (head, tail) = stream.split_at(split);
        let mut decoder = Decoder::new();
        let mut events = decoder.feed(head, t);
        let deadline = match split {
            1..=5 => Some(t + ESC_TIMEOUT),
            _ if (6..=stream.len() - 2).contains(&split) => Some(t + PASTE_TIMEOUT),
            _ => None,
        };
        assert_eq!(decoder.deadline(), deadline, "split at {split}");
        let later = t + Duration::from_secs(60);
        events.extend(decoder.feed(tail, later));
        events.extend(decoder.flush(later + PASTE_TIMEOUT));
        assert_eq!(events, want, "split at {split}");
    }

    assert_eq!(decode_all(b"\x1b[I"), [Event::Focus(true)]);
    assert_eq!(decode_all(b"\x1b[O"), [Event::Focus(false)]);
    assert_eq!(
        decode_all(b"\x1b[Ia\x1b[O"),
        [Event::Focus(true), typed('a'), Event::Focus(false)]
    );
}

#[test]
fn a_stalled_paste_is_delivered_after_its_timeout() {
    let t = Instant::now();
    let ms = Duration::from_millis;
    let mut decoder = Decoder::new();
    assert!(decoder.feed(b"\x1b[200~abc", t).is_empty());
    assert!(decoder.feed(b"\rdef\x1b[20", t + ms(1500)).is_empty());
    assert_eq!(decoder.deadline(), Some(t + ms(1500) + PASTE_TIMEOUT));
    assert!(decoder.flush(t + ms(3000)).is_empty());
    assert_eq!(
        decoder.flush(t + ms(3500)),
        [Event::Paste("abc\rdef\x1b[20".to_owned())]
    );
    assert_eq!(decoder.deadline(), None);
    assert_eq!(
        decoder.feed(b"\x07", t + ms(3600)),
        [chord('g', Modifiers::CTRL)]
    );
    assert!(decoder.feed(b"\x1b[200~", t).is_empty());
    assert_eq!(
        decoder.flush(t + PASTE_TIMEOUT),
        [Event::Paste(String::new())]
    );
}

#[test]
fn paste_is_capped_at_the_document_limit_on_a_character_boundary() {
    let body = format!("x{}", "\u{e9}".repeat(MAX_PASTE_BYTES / 2 + 5));
    let stream = [b"\x1b[200~".as_slice(), body.as_bytes(), b"\x1b[201~a"].concat();
    let events = decode_all(&stream);
    let [Event::Paste(kept), after] = events.as_slice() else {
        panic!("a paste and a key, got {} events", events.len());
    };
    assert_eq!(*after, typed('a'));
    assert_eq!(
        kept.len(),
        MAX_PASTE_BYTES - 1,
        "the cut character is dropped whole"
    );
    assert!(body.starts_with(kept.as_str()));
    drop((events, stream, body));

    let raw = b"x\xff\xe2\x82y\xc3\xa9";
    let stream = [b"\x1b[200~".as_slice(), raw, b"\x1b[201~"].concat();
    assert_eq!(
        decode_all(&stream),
        [Event::Paste(String::from_utf8_lossy(raw).into_owned())]
    );

    let replacements = MAX_PASTE_BYTES / char::REPLACEMENT_CHARACTER.len_utf8();
    for (tail, suffix, size) in [
        (b"\xff".as_slice(), "", replacements * 3),
        ("éé".as_bytes(), "é", MAX_PASTE_BYTES),
    ] {
        let mut raw = vec![0xff; replacements];
        raw.extend_from_slice(tail);
        let stream = [b"\x1b[200~".as_slice(), &raw, b"\x1b[201~a"].concat();
        let events = decode_all(&stream);
        let [Event::Paste(kept), after] = events.as_slice() else {
            panic!("a paste and a key, got {} events", events.len());
        };
        assert_eq!(*after, typed('a'));
        assert_eq!(kept.len(), size);
        let prefix = kept
            .strip_suffix(suffix)
            .expect("the valid suffix fits whole");
        assert_eq!(prefix.chars().count(), replacements);
        assert!(prefix.chars().all(|c| c == char::REPLACEMENT_CHARACTER));
    }
}

#[test]
fn a_broken_character_reads_as_from_utf8_lossy_reads_it() {
    for bytes in [
        &b"\xe0\x9f\x80x"[..],
        b"\xed\xa0\x80x",
        b"\xf4\x90\x80\x80x",
        b"\xe4\xb8(",
        b"\xf0\x9f\x98(",
        b"\xf0\x9f\xc0(",
        b"\xe1\x80\xe4\xb8\xadx",
    ] {
        let want: Vec<Event> = String::from_utf8_lossy(bytes).chars().map(typed).collect();
        assert_eq!(decode_all(bytes), want, "{bytes:02x?}");
    }
}

#[test]
fn chunk_boundary_invariance() {
    let mut segments: Vec<(Vec<u8>, Vec<Event>)> = vec![
        (b"a".to_vec(), vec![typed('a')]),
        (b"\x18".to_vec(), vec![chord('x', Modifiers::CTRL)]),
        (b"\x1bx".to_vec(), vec![alt(typed('x'))]),
        (
            b"\x1b\x1b[A".to_vec(),
            vec![named(NamedKey::ArrowUp, Modifiers::ALT)],
        ),
        (
            b"\x1b[1;5A".to_vec(),
            vec![named(NamedKey::ArrowUp, Modifiers::CTRL)],
        ),
        (
            b"\x1bOP".to_vec(),
            vec![named(NamedKey::F1, Modifiers::empty())],
        ),
        (
            b"\x1b[15;2~".to_vec(),
            vec![named(NamedKey::F5, Modifiers::SHIFT)],
        ),
        (b"\x1b[97;5u".to_vec(), vec![chord('a', Modifiers::CTRL)]),
        (
            "\u{e9}\u{65e5}\u{1f600}".as_bytes().to_vec(),
            vec![typed('\u{e9}'), typed('\u{65e5}'), typed('\u{1f600}')],
        ),
        (
            b"\xff\xc3(".to_vec(),
            vec![typed('\u{fffd}'), typed('\u{fffd}'), typed('(')],
        ),
        (
            b"\x1b[<0;10;5M".to_vec(),
            vec![mouse(
                MouseKind::Press,
                Some(MouseButton::Left),
                9,
                4,
                Modifiers::empty(),
            )],
        ),
        (
            b"\x1b[M\x20\x2a\x25".to_vec(),
            vec![mouse(
                MouseKind::Press,
                Some(MouseButton::Left),
                9,
                4,
                Modifiers::empty(),
            )],
        ),
        (
            b"\x1b[200~p\x1b[201q\x1b[201~".to_vec(),
            vec![Event::Paste("p\x1b[201q".to_owned())],
        ),
        (b"\x1b[I".to_vec(), vec![Event::Focus(true)]),
        (
            b"\x1b[48;24;80;0;0t".to_vec(),
            vec![Event::Resize { cols: 80, rows: 24 }],
        ),
        (
            b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(),
            vec![Event::Unknown(b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec())],
        ),
        (b"\x1bPx".to_vec(), vec![alt(typed('P')), typed('x')]),
        (
            b"\x1b]1\x07".to_vec(),
            vec![alt(typed(']')), typed('1'), chord('g', Modifiers::CTRL)],
        ),
        (
            b"\x1b]52;c;YQ==\x07".to_vec(),
            vec![Event::Unknown(b"\x1b]52;c;YQ==\x07".to_vec())],
        ),
        (b"\x1b[44:60;4u".to_vec(), vec![chord('<', Modifiers::ALT)]),
        (b"\x1b[57404u".to_vec(), vec![typed('5')]),
        (
            b"\x1b[1\r".to_vec(),
            vec![
                alt(typed('[')),
                typed('1'),
                named(NamedKey::Enter, Modifiers::empty()),
            ],
        ),
    ];
    let replies = probe_replies(&mut Terminal::new(24, 80));
    let reply_events = decode_all(&replies);
    assert_eq!(reply_events.len(), 7);
    segments.push((replies, reply_events));
    segments.push((
        b"\x1b".to_vec(),
        vec![named(NamedKey::Escape, Modifiers::empty())],
    ));

    let stream: Vec<u8> = segments
        .iter()
        .flat_map(|(bytes, _)| bytes.clone())
        .collect();
    let want: Vec<Event> = segments
        .iter()
        .flat_map(|(_, events)| events.clone())
        .collect();
    for (bytes, events) in &segments {
        assert_eq!(&decode_all(bytes), events, "{bytes:?} alone");
    }
    assert_eq!(decode_all(&stream), want);

    let t = Instant::now();
    for split in 0..=stream.len() {
        let (head, tail) = stream.split_at(split);
        let mut decoder = Decoder::new();
        let mut events = decoder.feed(head, t);
        events.extend(decoder.feed(tail, t));
        events.extend(decoder.flush(t + ESC_TIMEOUT));
        assert_eq!(events, want, "split at {split}");
    }
    let mut decoder = Decoder::new();
    let mut events: Vec<Event> = stream.iter().flat_map(|&b| decoder.feed(&[b], t)).collect();
    events.extend(decoder.flush(t + ESC_TIMEOUT));
    assert_eq!(events, want, "one byte per read");

    let mut decoder = Decoder::new();
    let mut now = t;
    let mut events = Vec::new();
    for &b in &stream {
        events.extend(decoder.feed(&[b], now));
        now += ESC_TIMEOUT * 4;
    }
    events.extend(decoder.flush(now + ESC_TIMEOUT));
    assert_eq!(events, want, "one byte per slow read");
}

#[test]
fn a_sequence_split_across_slow_reads_is_still_one_sequence() {
    let t = Instant::now();
    let late = t + Duration::from_millis(30);
    let cases: [(&[u8], &[u8], Vec<Event>); 5] = [
        (
            b"\x1b[<35;10;",
            b"5M",
            vec![mouse(MouseKind::Motion, None, 9, 4, Modifiers::empty())],
        ),
        (
            b"\x1b[200",
            b"~ls\rrm x\r\x1b[201~",
            vec![Event::Paste("ls\rrm x\r".to_owned())],
        ),
        (b"\xe6\x97", b"\xa5", vec![typed('\u{65e5}')]),
        (
            b"\x1bP>|ate",
            b"rm(1.0)\x1b\\",
            vec![Event::Reply(Reply::Version("aterm(1.0)".to_owned()))],
        ),
        (b"\x1b", b"x", vec![alt(typed('x'))]),
    ];
    for (head, tail, want) in cases {
        let mut decoder = Decoder::new();
        assert!(decoder.feed(head, t).is_empty(), "{head:?}");
        let mut events = decoder.feed(tail, late);
        events.extend(decoder.flush(late + PASTE_TIMEOUT));
        assert_eq!(events, want, "{head:?} then {tail:?}");
    }
    let mut decoder = Decoder::new();
    assert!(decoder.feed(b"\xe6\x97", t).is_empty());
    assert_eq!(decoder.flush(late), [typed('\u{fffd}')]);
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn never_panics_on_arbitrary_bytes() {
    for parameters in [4093, 4094, 4095] {
        let mut stream = b"\x1b[".to_vec();
        stream.extend(std::iter::repeat_n(b';', parameters));
        stream.extend_from_slice(b"~a");
        let events = decode_all(&stream);
        let [Event::Unknown(kept), after] = events.as_slice() else {
            panic!("an unknown sequence and a key, got {events:?}");
        };
        assert_eq!(kept, &stream[..stream.len().saturating_sub(1).min(4096)]);
        assert_eq!(*after, typed('a'));
    }

    const FRAGMENTS: [&[u8]; 14] = [
        b"\x1b[200~",
        b"\x1b[201~",
        b"\x1b[<",
        b"\x1bP>|",
        b"\x1b\\",
        b"\x1b]11;",
        b"\x1b[M",
        b"\x1b[?",
        b"\x1bO",
        b"\x1b\x1b[",
        b"$y",
        b";5",
        b"\xe2\x82",
        b"\xf0\x9f",
    ];
    const BYTES: &[u8] =
        b"\x1b[O;:0123456789<?>$~uMmAyctI\x07\x08\r\x00\x7f\xc3\xe2\xf0\x80\xbf\xff";
    let mut rng = Rng(0x00a7_e2a3_7ac5_2026);
    let t = Instant::now();
    for case in 0..100_000 {
        let mut buf = Vec::new();
        for _ in 0..rng.below(24) {
            match rng.below(4) {
                0 => buf.extend_from_slice(FRAGMENTS[rng.below(FRAGMENTS.len())]),
                1 => buf.push(rng.next() as u8),
                _ => buf.push(BYTES[rng.below(BYTES.len())]),
            }
        }
        let mut whole = Decoder::new();
        let mut want = whole.feed(&buf, t);
        want.extend(whole.flush(t + PASTE_TIMEOUT));
        assert_eq!(whole.deadline(), None, "case {case}: {buf:?}");

        for slow in [false, true] {
            let mut chunked = Decoder::new();
            let mut got = Vec::new();
            let mut now = t;
            let mut rest = buf.as_slice();
            while !rest.is_empty() {
                let (head, tail) = rest.split_at(1 + rng.below(rest.len()));
                got.extend(chunked.feed(head, now));
                if slow {
                    now += Duration::from_millis(rng.below(3000) as u64);
                }
                rest = tail;
            }
            got.extend(chunked.flush(now + PASTE_TIMEOUT));
            assert_eq!(got, want, "case {case} (slow: {slow}): {buf:?}");
        }

        let mut timed = Decoder::new();
        let mut now = t;
        let mut rest = buf.as_slice();
        while !rest.is_empty() {
            let (head, tail) = rest.split_at(1 + rng.below(rest.len()));
            let _ = timed.feed(head, now);
            now += Duration::from_millis(rng.below(40) as u64);
            if rng.below(8) == 0 {
                now += PASTE_TIMEOUT;
            }
            let _ = timed.flush(now);
            rest = tail;
        }
        let _ = timed.flush(now + PASTE_TIMEOUT);
        assert_eq!(timed.deadline(), None, "case {case}: {buf:?}");
    }
}

#[test]
fn lone_esc_flushes_as_escape_after_25ms() {
    assert_eq!(ESC_TIMEOUT, Duration::from_millis(25));
    let escape = named(NamedKey::Escape, Modifiers::empty());
    let t = Instant::now();
    let ms = Duration::from_millis;

    let mut decoder = Decoder::new();
    assert_eq!(decoder.deadline(), None);
    assert!(decoder.feed(b"\x1b", t).is_empty());
    assert_eq!(decoder.deadline(), Some(t + ms(25)));
    assert!(decoder.flush(t + ms(24)).is_empty());
    assert_eq!(decoder.flush(t + ms(25)), std::slice::from_ref(&escape));
    assert_eq!(decoder.deadline(), None);
    assert!(decoder.flush(t + ms(50)).is_empty());

    assert!(decoder.feed(b"\x1b", t).is_empty());
    assert_eq!(decoder.feed(b"x", t + ms(20)), [alt(typed('x'))]);
    assert!(decoder.feed(b"\x1b[", t).is_empty());
    assert!(decoder.feed(b"1;5", t + ms(20)).is_empty());
    assert_eq!(decoder.deadline(), Some(t + ms(45)));
    assert_eq!(
        decoder.feed(b"A", t + ms(40)),
        [named(NamedKey::ArrowUp, Modifiers::CTRL)]
    );

    assert!(decoder.feed(b"\x1b", t).is_empty());
    assert_eq!(decoder.feed(b"x", t + ms(30)), [alt(typed('x'))]);
    assert!(decoder.feed(b"\x1b", t).is_empty());
    assert_eq!(decoder.flush(t + ms(30)), std::slice::from_ref(&escape));
    assert_eq!(decoder.feed(b"x", t + ms(30)), [typed('x')]);

    for (bytes, want) in [
        (b"\x1b[".as_slice(), alt(typed('['))),
        (b"\x1bO", alt(typed('O'))),
        (b"\x1bP", alt(typed('P'))),
        (b"\x1b]", alt(typed(']'))),
        (b"\x1b\x1b", named(NamedKey::Escape, Modifiers::ALT)),
    ] {
        assert!(decoder.feed(bytes, t).is_empty(), "{bytes:?}");
        assert_eq!(decoder.flush(t + ms(25)), [want], "{bytes:?}");
    }
}

#[test]
fn esc_x_in_one_burst_is_alt_x() {
    let t = Instant::now();
    let mut decoder = Decoder::new();
    assert_eq!(decoder.feed(b"\x1bx", t), [alt(typed('x'))]);
    assert_eq!(decoder.deadline(), None);
    assert_eq!(
        decoder.feed(b"\x1bX", t),
        [chord('x', Modifiers::SHIFT | Modifiers::ALT)]
    );
    assert_eq!(
        decoder.feed(b"\x1b\x01", t),
        [chord('a', Modifiers::CTRL | Modifiers::ALT)]
    );
    assert_eq!(
        decoder.feed("\x1b\u{e9}".as_bytes(), t),
        [chord('\u{e9}', Modifiers::ALT)]
    );
    assert_eq!(
        decoder.feed(b"\x1b\r", t),
        [named(NamedKey::Enter, Modifiers::ALT)]
    );
    assert_eq!(
        decoder.feed(b"\x1b\x1bx", t),
        [named(NamedKey::Escape, Modifiers::ALT), typed('x')]
    );
    assert_eq!(
        decoder.feed(b"\x1b\x1b[I", t),
        [
            named(NamedKey::Escape, Modifiers::empty()),
            Event::Focus(true)
        ]
    );
}

fn probe_replies(term: &mut Terminal) -> Vec<u8> {
    term.process(aterm_tty::probe::QUERY);
    term.take_response().expect("aterm answers the probe")
}

#[test]
fn probe_replies_decode() {
    let mode = |mode, state| Event::Reply(Reply::Mode { mode, state });
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>1u\x1b[?1004h\x1b[?2004h");
    let events = decode_all(&probe_replies(&mut term));
    let [
        Event::Reply(Reply::Version(version)),
        flags,
        sync,
        resize,
        focus,
        paste,
        Event::Reply(Reply::PrimaryAttributes(da1)),
    ] = events.as_slice()
    else {
        panic!("seven replies, XTVERSION first and DA1 last: {events:?}");
    };
    assert!(
        version.starts_with("aterm(") && version.ends_with(')'),
        "{version}"
    );
    assert_eq!(*flags, Event::Reply(Reply::KittyFlags(1)));
    assert_eq!(*sync, mode(2026, ModeState::Reset));
    assert_eq!(*resize, mode(2048, ModeState::Reset));
    assert_eq!(*focus, mode(1004, ModeState::Set));
    assert_eq!(*paste, mode(2004, ModeState::Set));
    assert!(
        da1.first().is_some_and(|level| (62..=65).contains(level)),
        "DA1 leads with the VT level: {da1:?}"
    );

    for (bytes, want) in [
        (
            b"\x1bP>|XTerm(390)\x1b\\".as_slice(),
            Event::Reply(Reply::Version("XTerm(390)".to_owned())),
        ),
        (
            b"\x1bP>|kitty(0.36.4)\x1b\\",
            Event::Reply(Reply::Version("kitty(0.36.4)".to_owned())),
        ),
        (b"\x1b[?0u", Event::Reply(Reply::KittyFlags(0))),
        (b"\x1b[?31u", Event::Reply(Reply::KittyFlags(31))),
        (b"\x1b[?2048;0$y", mode(2048, ModeState::NotRecognized)),
        (b"\x1b[?2026;3$y", mode(2026, ModeState::PermanentlySet)),
        (b"\x1b[?2027;4$y", mode(2027, ModeState::PermanentlyReset)),
        (
            b"\x1b[?64;1;2;6;9;15;16;17;18;21;22;28c",
            Event::Reply(Reply::PrimaryAttributes(vec![
                64, 1, 2, 6, 9, 15, 16, 17, 18, 21, 22, 28,
            ])),
        ),
        (
            b"\x1b[?1;2c",
            Event::Reply(Reply::PrimaryAttributes(vec![1, 2])),
        ),
        (
            b"\x1b[48;50;132;800;1056t",
            Event::Resize {
                cols: 132,
                rows: 50,
            },
        ),
        (
            b"\x1bP1$r0m\x1b\\",
            Event::Unknown(b"\x1bP1$r0m\x1b\\".to_vec()),
        ),
        (b"\x1b[?997;1n", Event::Unknown(b"\x1b[?997;1n".to_vec())),
    ] {
        assert_eq!(decode_all(bytes), [want], "{bytes:?}");
    }

    let mut term = Terminal::new(30, 100);
    term.process(b"\x1b[?2048h");
    let report = term
        .take_response()
        .expect("aterm reports the size on 2048");
    assert_eq!(
        decode_all(&report),
        [Event::Resize {
            cols: 100,
            rows: 30
        }]
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The input decoder against aterm's own encoders (docs/DESIGN-amacs-2026-10-19.md
//! §5.3). Every key and mouse report aterm encodes in legacy or kitty mode (flag 1,
//! and flags 1|4 as amacs pushes them) decodes to the press that made it, or to the
//! canonical reading `aterm_tty::decode` documents. Where legacy bytes are ambiguous
//! that reading re-encodes to the same bytes; a keypad key, a lock bit and a shifted
//! key read the same in every mode. The replies aterm's engine gives amacs's probe
//! decode, and no input panics or decodes differently for how it was split into reads
//! or how long the reader took between them.

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

/// Decode one burst read at once, then let its deadline pass.
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

/// The press that types glyph `g` with no chord: a space is the Space key, an ASCII
/// capital its letter with Shift, any other glyph itself.
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

/// [`typed`] with Alt: a chord, typing nothing.
fn alt(event: Event) -> Event {
    match event {
        Event::Key { key: k, mods, .. } => key(k, mods | Modifiers::ALT, None),
        other => other,
    }
}

/// xterm's Ctrl table (kitty's legacy one too): the C0 byte a Ctrl chord on `c` sends.
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

/// The documented canonical reading of a lone C0 byte or DEL.
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
        // C-a .. C-z; LF is C-j.
        b => chord(char::from(b + b'`'), Modifiers::CTRL),
    }
}

const LOCKS: Modifiers = Modifiers::CAPS_LOCK.union(Modifiers::NUM_LOCK);

/// What a press kitty spells as a sequence reads as: the press itself, its locks
/// dropped, with a keypad key read as its main-block twin (its glyph typing itself when
/// no chord modifier is held) and, under flag 4 (`alternates`), a Shift chord read as
/// the shifted glyph the way legacy bytes spell it.
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
            // aterm's kitty encoder sends the US table's shifted key when no layout says
            // otherwise, and only when it differs from the key.
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
            // Code 32 is the Space key.
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

/// The event the decoder must return for `k` pressed with `mods` (Shift, Alt and Ctrl
/// only) in `mode`: the press itself where the bytes say everything, else the
/// canonical reading.
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
            // Shift+Space types a space in every mode.
            NamedKey::Space if !alt_held && !ctrl => typed(' '),
            // Legacy NUL, ESC SP or ESC NUL: Shift is lost.
            NamedKey::Space if !kitty => named(n, mods - Modifiers::SHIFT),
            // Kitty keeps a bare or Shift-only Backspace as DEL, so Shift is lost.
            NamedKey::Backspace if kitty && !alt_held && !ctrl => named(n, Modifiers::empty()),
            // Kitty spells every other press out in full.
            _ if kitty => kitty_reading(k, mods, alternates),
            // ESC CR; aterm's legacy Shift+Enter is LF, read as C-j; CR loses Ctrl.
            NamedKey::Enter if alt_held => named(n, Modifiers::ALT),
            NamedKey::Enter if shift && !ctrl => chord('j', Modifiers::CTRL),
            NamedKey::Enter => named(n, Modifiers::empty()),
            // CSI Z for any Shift; ESC TAB; TAB loses Ctrl.
            NamedKey::Tab if shift => named(n, Modifiers::SHIFT),
            NamedKey::Tab => named(n, mods & Modifiers::ALT),
            // ESC (ESC ESC with Alt), DEL or BS (ESC-prefixed with Alt): Shift and
            // Ctrl are lost.
            NamedKey::Escape | NamedKey::Backspace => named(n, mods & Modifiers::ALT),
            // The CSI and SS3 keys carry every modifier.
            _ => named(n, mods),
        },
        Key::Character(c) => {
            let chorded = alt_held || ctrl;
            if kitty && chorded {
                // `CSI code[:shifted] ; mods u`.
                return kitty_reading(k, mods, alternates);
            }
            let glyph = shifted_character(c, mods).unwrap_or(c);
            if ctrl && let Some(byte) = ctrl_byte(c).or_else(|| ctrl_byte(glyph)) {
                let event = c0(byte);
                return if alt_held { alt(event) } else { event };
            }
            if alt_held {
                // ESC and the glyph; without Shift, the key's own lowercase.
                return alt(typed(if shift { glyph } else { c.to_ascii_lowercase() }));
            }
            // Text, or a Ctrl chord with no control byte, which types its glyph.
            typed(glyph)
        }
        _ => panic!("no other key kind in the table: {k:?}"),
    }
}

/// Every combination of Shift, Alt and Ctrl.
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

/// The keys of §5.3's table: printable ASCII and non-ASCII characters, F1-F12, the
/// cursor and editing keys, and Tab, Enter, Backspace, Escape and Space.
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
/// What amacs pushes: disambiguate, and the shifted key of a Shift chord.
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
                // The reading is a press that makes these very bytes. BS reads as
                // Backspace, which aterm spells BS only under DECBKM (mode 67).
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
                // A shifted key read under flag 4 is the glyph's own chord, which kitty
                // spells by the glyph's code: it reads as the legacy bytes do instead.
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
    // Most presses come back as themselves; the rest are the documented readings.
    assert!(
        exact * 2 > cases,
        "only {exact} of {cases} presses read back exactly"
    );
}

/// Every [`NamedKey`], each once.
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

/// Under kitty flag 1 every named key reads back, with any modifiers: the decoder's key
/// set mirrors aterm-types', so the oracle mapping is total. Each reads as itself but a
/// keypad key, which reads as its main-block twin as it does in legacy mode.
#[test]
fn every_named_key_round_trips_under_kitty_flag_1() {
    use NamedKey::*;
    let all = all_named_keys();
    // The list is every key: kitty's codes, each once.
    let mut codes: Vec<u32> = all.iter().map(|k| k.kitty_code()).collect();
    codes.sort_unstable();
    let mut want: Vec<u32> = vec![9, 13, 27, 32, 127];
    want.extend(57348..=57452);
    assert_eq!(codes, want);

    for k in all {
        // The kitty code itself, as kitty's report-all mode sends any key: a space and a
        // keypad glyph type themselves.
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
            // Modifier and lock keys report only under flag 8.
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

/// Every named key aterm encodes in legacy mode reads back, F13-F24 and the keypad as
/// well as §5.3's table, with and without the application cursor and keypad modes. A
/// keypad key reads as its main-block twin ([`Key::main_block_twin`]), whose bytes it
/// shares outside DECKPAM and whose reading its SS3 form folds onto inside it; keypad
/// Begin reads as itself. The keys with no legacy form (F25-F35, the media, lock and
/// modifier keys, Print Screen, Pause, Menu) encode to nothing; kitty reports them.
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
                    // The main-block `=` key's bytes.
                    (NumpadEqual, _) => expected(&Key::Character('='), mods, LEGACY),
                    // The keypad's glyphs: Alt ESC-prefixes one, Shift and Ctrl are lost.
                    (_, Some(Key::Character(glyph))) if mods.contains(Modifiers::ALT) => {
                        alt(typed(glyph))
                    }
                    (_, Some(Key::Character(glyph))) => typed(glyph),
                    (NumpadEnter, _) => expected(&Key::Named(Enter), mods, LEGACY),
                    // CSI E or SS3 E, ESC-prefixed with Alt; Shift and Ctrl are lost.
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
    // §5.3's 27 keys, F13-F24 and the 29 keypad keys, in four modes.
    assert_eq!(read, 4 * 8 * (27 + 12 + 29));
    // DECKPAM's SS3 forms were among them: the 16 glyph keys that have one (keypad `=`
    // has none) and keypad Enter, each without Shift or Alt (2 of 8 combinations), in
    // two modes.
    assert_eq!(ss3_keypad, 2 * 2 * 17);
}

/// The legacy ambiguities, decided: C-i is Tab, C-m Enter, C-h Backspace (and so is
/// C-Backspace), C-[ Escape, C-@ C-SPC, and aterm's Shift+Enter LF is C-j. Kitty
/// flag 1 tells every pair apart.
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
        // The named key itself, in both modes.
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
    // C-Backspace is BS in legacy, as C-h is: both read as Backspace.
    let bs = Key::Named(NamedKey::Backspace);
    assert_eq!(encode(&bs, ctrl, LEGACY), [0x08]);
    assert_eq!(decode_all(&[0x08]), [named(NamedKey::Backspace, none)]);
    assert_eq!(
        decode_all(&encode(&bs, ctrl, KITTY_FLAG_1)),
        [named(NamedKey::Backspace, ctrl)]
    );
    // Shift+Enter is LF in legacy (C-j), CSI 13;2u under flag 1.
    let enter = Key::Named(NamedKey::Enter);
    assert_eq!(
        decode_all(&encode(&enter, Modifiers::SHIFT, LEGACY)),
        [chord('j', ctrl)]
    );
    assert_eq!(
        decode_all(&encode(&enter, Modifiers::SHIFT, KITTY_FLAG_1)),
        [named(NamedKey::Enter, Modifiers::SHIFT)]
    );
    // A terminal's own Backspace-as-BS reads as Backspace too, Alt with it.
    assert_eq!(
        decode_all(b"\x1b\x08"),
        [named(NamedKey::Backspace, Modifiers::ALT)]
    );
}

/// A press amacs binds reads the same whether the terminal speaks legacy bytes or the
/// kitty flags amacs pushes: keypad keys as their main-block twins, chords with Caps
/// Lock or Num Lock on as without, and Alt with a shifted glyph (M-<, M->, M-%, M-!)
/// as that glyph. Kitty flag 1 alone cannot say which glyph Shift made, and the decoder
/// does not guess a layout: that press keeps its base key and Shift.
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

/// Every modifier aterm encodes, Super, Hyper, Meta and the locks among them, in every
/// mode: each press reads as one key with no lock bit; a press kitty spells as a
/// sequence reads as itself (keypad and shifted keys as their canonical readings), a
/// keypad key in DECKPAM as it reads without it, and every other reading re-encodes to
/// the very bytes, with the press's locks or none.
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
            // All eight at once would be kitty's 256, which aterm's one-byte value
            // saturates to 255 (`Modifiers::kitty_encoded`): left out.
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
                    // DECKPAM's SS3 forms fold as their legacy bytes read (and Shift, which
                    // cancels DECKPAM, is lost as it is there).
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
                    // With the press's locks or none: Super+a with Caps Lock on writes
                    // `a`, which reads as a plain a, which Caps Lock would make `A`.
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
                // (Cb, release, the event it reports)
                let mut reports = Vec::new();
                for button in buttons {
                    let cb = button.code() | mask;
                    let b = Some(button);
                    reports.push((cb, false, mouse(MouseKind::Press, b, col, row, mods)));
                    reports.push((cb, true, mouse(MouseKind::Release, b, col, row, mods)));
                    // A drag: motion with the button held.
                    reports.push((cb | 32, false, mouse(MouseKind::Motion, b, col, row, mods)));
                }
                // A move: motion with no button held (3).
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
                    // X10, for a terminal that ignores 1006: one byte per value, so
                    // cells to 222 only, and a release that does not name its button.
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

/// The bracketed paste body is read raw: no key decoding inside.
#[test]
fn paste_and_focus_round_trip() {
    // aterm's own framing: the body loses ESC and C1, and line breaks become CR.
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
        // CR back to LF, the controls aterm strips aside, gives the text.
        let unframed = want.replace('\r', "\n");
        let original = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\x1b', "");
        assert_eq!(unframed, original);
    }

    // A body that holds keys, and ESC [ 201 that is not the end, split at every byte
    // (another terminal may not strip ESC).
    let body = "x\x1b[201y\x1b[20\x1b[2015~z\x1b[A\x03\x1b";
    let stream = [b"\x1b[200~".as_slice(), body.as_bytes(), b"\x1b[201~", b"a"].concat();
    let want = [Event::Paste(body.to_owned()), typed('a')];
    assert_eq!(decode_all(&stream), want);
    let t = Instant::now();
    for split in 0..=stream.len() {
        let (head, tail) = stream.split_at(split);
        let mut decoder = Decoder::new();
        let mut events = decoder.feed(head, t);
        // Inside the opening the sequence deadline runs, inside the paste (to the last
        // byte of its end) the paste's.
        let deadline = match split {
            1..=5 => Some(t + ESC_TIMEOUT),
            _ if (6..=stream.len() - 2).contains(&split) => Some(t + PASTE_TIMEOUT),
            _ => None,
        };
        assert_eq!(decoder.deadline(), deadline, "split at {split}");
        // The rest is read a minute later, but with no flush between (the caller's wait
        // never found the input idle): it continues what is held.
        let later = t + Duration::from_secs(60);
        events.extend(decoder.feed(tail, later));
        events.extend(decoder.flush(later + PASTE_TIMEOUT));
        assert_eq!(events, want, "split at {split}");
    }

    // Focus, as aterm-gui writes it (crates/aterm-gui/src/input.rs).
    assert_eq!(decode_all(b"\x1b[I"), [Event::Focus(true)]);
    assert_eq!(decode_all(b"\x1b[O"), [Event::Focus(false)]);
    assert_eq!(
        decode_all(b"\x1b[Ia\x1b[O"),
        [Event::Focus(true), typed('a'), Event::Focus(false)]
    );
}

/// A paste whose end never comes does not hold the keyboard: once the caller's wait has
/// found nothing to read for [`PASTE_TIMEOUT`], what came is delivered and keys read as
/// keys again.
#[test]
fn a_stalled_paste_is_delivered_after_its_timeout() {
    let t = Instant::now();
    let ms = Duration::from_millis;
    let mut decoder = Decoder::new();
    assert!(decoder.feed(b"\x1b[200~abc", t).is_empty());
    assert!(decoder.feed(b"\rdef\x1b[20", t + ms(1500)).is_empty());
    // Each read re-arms the wait.
    assert_eq!(decoder.deadline(), Some(t + ms(1500) + PASTE_TIMEOUT));
    assert!(decoder.flush(t + ms(3000)).is_empty());
    // A partial end marker was body after all.
    assert_eq!(
        decoder.flush(t + ms(3500)),
        [Event::Paste("abc\rdef\x1b[20".to_owned())]
    );
    assert_eq!(decoder.deadline(), None);
    assert_eq!(
        decoder.feed(b"\x07", t + ms(3600)),
        [chord('g', Modifiers::CTRL)]
    );
    // An empty stalled paste is an empty paste.
    assert!(decoder.feed(b"\x1b[200~", t).is_empty());
    assert_eq!(
        decoder.flush(t + PASTE_TIMEOUT),
        [Event::Paste(String::new())]
    );
}

#[test]
fn paste_is_capped_at_the_document_limit_on_a_character_boundary() {
    // One ASCII byte, then two-byte characters, so the cap falls inside one.
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

    // Invalid bytes remain ordinary lossy text when they fit.
    let raw = b"x\xff\xe2\x82y\xc3\xa9";
    let stream = [b"\x1b[200~".as_slice(), raw, b"\x1b[201~"].concat();
    assert_eq!(
        decode_all(&stream),
        [Event::Paste(String::from_utf8_lossy(raw).into_owned())]
    );

    // Each invalid byte expands to three UTF-8 bytes. The output cap applies
    // both when the next replacement will not fit and within a following valid run.
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

/// A mixed stream: every split into two reads, and one byte per read, decodes as the
/// whole burst does.
#[test]
fn chunk_boundary_invariance() {
    // (bytes, what they decode to on their own and inside the stream)
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
        // Alt+Shift+P and a typed x: ESC P begins no reply here.
        (b"\x1bPx".to_vec(), vec![alt(typed('P')), typed('x')]),
        // Alt+], 1 and C-g: an OSC needs its number and `;` before BEL can end it.
        (
            b"\x1b]1\x07".to_vec(),
            vec![alt(typed(']')), typed('1'), chord('g', Modifiers::CTRL)],
        ),
        (
            b"\x1b]52;c;YQ==\x07".to_vec(),
            vec![Event::Unknown(b"\x1b]52;c;YQ==\x07".to_vec())],
        ),
        // Kitty flags 1|4: Alt+Shift+, and keypad 5.
        (b"\x1b[44:60;4u".to_vec(), vec![chord('<', Modifiers::ALT)]),
        (b"\x1b[57404u".to_vec(), vec![typed('5')]),
        // Alt+[ and 1, cut off by Enter: no sequence.
        (
            b"\x1b[1\r".to_vec(),
            vec![
                alt(typed('[')),
                typed('1'),
                named(NamedKey::Enter, Modifiers::empty()),
            ],
        ),
    ];
    // aterm's own replies to the probe.
    let replies = probe_replies(&mut Terminal::new(24, 80));
    let reply_events = decode_all(&replies);
    assert_eq!(reply_events.len(), 7);
    segments.push((replies, reply_events));
    // A trailing lone ESC, resolved by the deadline.
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

    // One byte per read, each read well past the last one's deadline but with no flush
    // between: a slow reader still reads the same events.
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

/// What the reader does between reads is no part of the input: a sequence split across
/// reads further apart than [`ESC_TIMEOUT`], with no flush between them, is still one
/// sequence. A flush (the caller's wait found nothing) is what ends one.
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
    // The same head, flushed past its deadline before the tail: typed keys.
    let mut decoder = Decoder::new();
    assert!(decoder.feed(b"\xe6\x97", t).is_empty());
    assert_eq!(decoder.flush(late), [typed('\u{fffd}')]);
}

/// SplitMix64: a seeded generator, so a failure names its case.
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
    // The final byte is part of the retained sequence cap: a long unknown CSI
    // still leaves the following ordinary key outside it.
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

    // Bytes and fragments that steer the decoder into its deeper states.
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
        // Everything resolved: past every deadline, a paste's included.
        let mut whole = Decoder::new();
        let mut want = whole.feed(&buf, t);
        want.extend(whole.flush(t + PASTE_TIMEOUT));
        assert_eq!(whole.deadline(), None, "case {case}: {buf:?}");

        // The same bytes in random reads, at one instant, and again with the clock
        // running between them but no flush: the same events.
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

        // And with flushes between reads at random times, which may split sequences:
        // no panic, and nothing held once the last deadline has passed.
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

    // A byte read inside the window continues the sequence...
    assert!(decoder.feed(b"\x1b", t).is_empty());
    assert_eq!(decoder.feed(b"x", t + ms(20)), [alt(typed('x'))]);
    // ...and each read re-arms the window.
    assert!(decoder.feed(b"\x1b[", t).is_empty());
    assert!(decoder.feed(b"1;5", t + ms(20)).is_empty());
    assert_eq!(decoder.deadline(), Some(t + ms(45)));
    assert_eq!(
        decoder.feed(b"A", t + ms(40)),
        [named(NamedKey::ArrowUp, Modifiers::CTRL)]
    );

    // A byte read after the window with no flush between continues the sequence: it
    // may have come in time while the reader was busy. A flush ends the sequence.
    assert!(decoder.feed(b"\x1b", t).is_empty());
    assert_eq!(decoder.feed(b"x", t + ms(30)), [alt(typed('x'))]);
    assert!(decoder.feed(b"\x1b", t).is_empty());
    assert_eq!(decoder.flush(t + ms(30)), std::slice::from_ref(&escape));
    assert_eq!(decoder.feed(b"x", t + ms(30)), [typed('x')]);

    // What a trailing sequence introducer was: a legacy Alt chord.
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
    // ESC ESC x: Alt+Escape, then x.
    assert_eq!(
        decoder.feed(b"\x1b\x1bx", t),
        [named(NamedKey::Escape, Modifiers::ALT), typed('x')]
    );
    // An Escape just before a report is Escape, not Alt on the report.
    assert_eq!(
        decoder.feed(b"\x1b\x1b[I", t),
        [
            named(NamedKey::Escape, Modifiers::empty()),
            Event::Focus(true)
        ]
    );
}

/// What the probe writes (docs/DESIGN-amacs-2026-10-19.md §5.2), answered by aterm's
/// own engine.
fn probe_replies(term: &mut Terminal) -> Vec<u8> {
    term.process(aterm_tty::probe::QUERY);
    term.take_response().expect("aterm answers the probe")
}

#[test]
fn probe_replies_decode() {
    let mode = |mode, state| Event::Reply(Reply::Mode { mode, state });
    let mut term = Terminal::new(24, 80);
    // Kitty flag 1 pushed, focus and paste reporting on, 2026 and 2048 off.
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

    // The same replies as other terminals spell them.
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
        // A DCS that is not XTVERSION, and a reply this decoder does not read.
        (
            b"\x1bP1$r0m\x1b\\",
            Event::Unknown(b"\x1bP1$r0m\x1b\\".to_vec()),
        ),
        (b"\x1b[?997;1n", Event::Unknown(b"\x1b[?997;1n".to_vec())),
    ] {
        assert_eq!(decode_all(bytes), [want], "{bytes:?}");
    }

    // DEC 2048's report, as aterm's engine sends it on enable.
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

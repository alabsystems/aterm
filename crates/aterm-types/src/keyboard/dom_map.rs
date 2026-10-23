// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! DOM `KeyboardEvent.key` → engine [`Key`]/[`NamedKey`] mapping: the web sibling of
//! `aterm-winit-keymap`, here so the wasm bindings (`aterm-wasm`, `aterm-gpu-web`) share one
//! table. Pure `&str` matching, so no feature gate.

use super::{
    Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key_with_layout,
    encode_key_with_text, shifted_character,
};

/// Encode a DOM keyboard event (`event_type`: 0=Press, 1=Repeat, 2=Release) against `mode`.
///
/// Non-ASCII chords without a control byte fall back to `base_layout_key`, and level-shift
/// modifiers such as Windows `AltGraph` (`Ctrl+Alt`) must be cleared by the caller before encoding.
#[must_use]
pub fn encode_dom_key(
    key: &str,
    mods: u8,
    event_type: u8,
    base_layout_key: Option<char>,
    mode: KeyboardMode,
) -> Option<Vec<u8>> {
    let key = map_dom_key(key)?;
    let event_type = match event_type {
        0 => KeyEventType::Press,
        1 => KeyEventType::Repeat,
        2 => KeyEventType::Release,
        _ => return None,
    };
    let mods = Modifiers::from_bits_truncate(mods);
    let bytes = match unshifted_dom_key(&key, mods, mode, base_layout_key) {
        Some((base, glyph)) => encode_key_with_text(
            &Key::Character(base),
            mods,
            mode,
            event_type,
            base_layout_key,
            Some(glyph),
        ),
        None => encode_key_with_layout(&key, mods, mode, event_type, base_layout_key),
    };
    if bytes.is_empty() { None } else { Some(bytes) }
}

/// Recover the unshifted base character and shifted glyph for Kitty-protocol `Shift` presses.
fn unshifted_dom_key(
    key: &Key,
    mods: Modifiers,
    mode: KeyboardMode,
    base_layout_key: Option<char>,
) -> Option<(char, char)> {
    let Key::Character(glyph) = *key else {
        return None;
    };
    if !mods.contains(Modifiers::SHIFT)
        || !mode.intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS)
        || mode.contains(KeyboardMode::WIN32_INPUT)
    {
        return None;
    }
    if let Some(base) =
        base_layout_key.filter(|&b| b != glyph && shifted_character(b, mods) == Some(glyph))
    {
        return Some((base, glyph));
    }
    let mut lower = glyph.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(base), None) if base != glyph => Some((base, glyph)),
        _ => None,
    }
}

/// Map a DOM `KeyboardEvent.key` value into the engine's [`Key`]. `None` for values the
/// engine must not guess an encoding for: IME/composition values (`"Dead"`, `"Process"`,
/// `"Compose"`, multi-codepoint strings), `"Unidentified"`, and named keys no terminal sequence
/// covers.
#[must_use]
pub fn map_dom_key(key: &str) -> Option<Key> {
    let mut chars = key.chars();
    if let Some(c) = chars.next()
        && chars.next().is_none()
    {
        return Some(if c == ' ' {
            Key::Named(NamedKey::Space)
        } else {
            Key::Character(c)
        });
    }
    map_dom_named_key(key).map(Key::Named)
}

/// Map a multi-character DOM key name into [`NamedKey`], canonicalizing modifiers to Left.
#[must_use]
fn map_dom_named_key(key: &str) -> Option<NamedKey> {
    Some(match key {
        "ArrowUp" => NamedKey::ArrowUp,
        "ArrowDown" => NamedKey::ArrowDown,
        "ArrowLeft" => NamedKey::ArrowLeft,
        "ArrowRight" => NamedKey::ArrowRight,
        "Home" => NamedKey::Home,
        "End" => NamedKey::End,
        "PageUp" => NamedKey::PageUp,
        "PageDown" => NamedKey::PageDown,
        "Backspace" => NamedKey::Backspace,
        "Delete" => NamedKey::Delete,
        "Insert" => NamedKey::Insert,
        "Enter" => NamedKey::Enter,
        "Tab" => NamedKey::Tab,
        "Escape" => NamedKey::Escape,
        "CapsLock" => NamedKey::CapsLock,
        "NumLock" => NamedKey::NumLock,
        "ScrollLock" => NamedKey::ScrollLock,
        "PrintScreen" => NamedKey::PrintScreen,
        "Pause" => NamedKey::Pause,
        "ContextMenu" => NamedKey::ContextMenu,
        "F1" => NamedKey::F1,
        "F2" => NamedKey::F2,
        "F3" => NamedKey::F3,
        "F4" => NamedKey::F4,
        "F5" => NamedKey::F5,
        "F6" => NamedKey::F6,
        "F7" => NamedKey::F7,
        "F8" => NamedKey::F8,
        "F9" => NamedKey::F9,
        "F10" => NamedKey::F10,
        "F11" => NamedKey::F11,
        "F12" => NamedKey::F12,
        "F13" => NamedKey::F13,
        "F14" => NamedKey::F14,
        "F15" => NamedKey::F15,
        "F16" => NamedKey::F16,
        "F17" => NamedKey::F17,
        "F18" => NamedKey::F18,
        "F19" => NamedKey::F19,
        "F20" => NamedKey::F20,
        "F21" => NamedKey::F21,
        "F22" => NamedKey::F22,
        "F23" => NamedKey::F23,
        "F24" => NamedKey::F24,
        "F25" => NamedKey::F25,
        "F26" => NamedKey::F26,
        "F27" => NamedKey::F27,
        "F28" => NamedKey::F28,
        "F29" => NamedKey::F29,
        "F30" => NamedKey::F30,
        "F31" => NamedKey::F31,
        "F32" => NamedKey::F32,
        "F33" => NamedKey::F33,
        "F34" => NamedKey::F34,
        "F35" => NamedKey::F35,
        "Shift" => NamedKey::ShiftLeft,
        "Control" => NamedKey::ControlLeft,
        "Alt" => NamedKey::AltLeft,
        "Meta" => NamedKey::SuperLeft,
        "MediaPlay" => NamedKey::MediaPlay,
        "MediaPause" => NamedKey::MediaPause,
        "MediaPlayPause" => NamedKey::MediaPlayPause,
        "MediaStop" => NamedKey::MediaStop,
        "MediaFastForward" => NamedKey::MediaFastForward,
        "MediaRewind" => NamedKey::MediaRewind,
        "MediaTrackNext" => NamedKey::MediaTrackNext,
        "MediaTrackPrevious" => NamedKey::MediaTrackPrevious,
        "MediaRecord" => NamedKey::MediaRecord,
        "AudioVolumeDown" => NamedKey::AudioVolumeDown,
        "AudioVolumeUp" => NamedKey::AudioVolumeUp,
        "AudioVolumeMute" => NamedKey::AudioVolumeMute,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that a DOM `Ctrl` chord on a non-ASCII glyph without a control byte uses `base_layout_key`.
    #[test]
    fn a_dom_ctrl_chord_without_a_control_byte_takes_the_physical_keys() {
        let legacy = KeyboardMode::empty();
        let ctrl = Modifiers::CTRL.bits();
        let ctrl_alt = (Modifiers::CTRL | Modifiers::ALT).bits();
        let enc = |key, mods, base| encode_dom_key(key, mods, 0, base, legacy);
        for (key, mods, base, want, why) in [
            ("х", ctrl, Some('['), &b"\x1b"[..], "ru Ctrl+х, the [ key"),
            ("ü", ctrl, Some('['), b"\x1b", "de Ctrl+ü, the [ key"),
            ("с", ctrl, Some('c'), b"\x03", "ru Ctrl+с, the C key"),
            ("é", ctrl, Some('2'), b"\x00", "fr Ctrl+é, the 2 key"),
            ("€", ctrl, Some('e'), b"\x05", "Ctrl+€ on the E key"),
            ("€", ctrl_alt, Some('e'), b"\x1b\x05", "Ctrl+Alt is a chord"),
            ("@", ctrl_alt, Some('q'), b"\x1b\x00", "Ctrl+Alt is a chord"),
            (
                "€",
                0,
                Some('e'),
                "€".as_bytes(),
                "AltGr+E, Ctrl+Alt cleared",
            ),
            ("@", 0, Some('q'), b"@", "AltGr+Q, Ctrl+Alt cleared"),
            (
                "ö",
                ctrl,
                Some(';'),
                "ö".as_bytes(),
                "de Ctrl+ö: ; has no byte",
            ),
            ("€", ctrl, None, "€".as_bytes(), "no physical key"),
        ] {
            assert_eq!(enc(key, mods, base).as_deref(), Some(want), "{why}");
        }
    }

    #[test]
    fn map_dom_key_reads_the_dom_key_names() {
        let named = |k| Some(Key::Named(k));
        for (dom, want) in [
            ("a", Some(Key::Character('a'))),
            ("A", Some(Key::Character('A'))),
            ("@", Some(Key::Character('@'))),
            ("é", Some(Key::Character('é'))),
            (" ", named(NamedKey::Space)),
            ("ArrowUp", named(NamedKey::ArrowUp)),
            ("Enter", named(NamedKey::Enter)),
            ("Tab", named(NamedKey::Tab)),
            ("Backspace", named(NamedKey::Backspace)),
            ("Delete", named(NamedKey::Delete)),
            ("Home", named(NamedKey::Home)),
            ("End", named(NamedKey::End)),
            ("PageUp", named(NamedKey::PageUp)),
            ("PageDown", named(NamedKey::PageDown)),
            ("Insert", named(NamedKey::Insert)),
            ("Escape", named(NamedKey::Escape)),
            ("ContextMenu", named(NamedKey::ContextMenu)),
            ("Pause", named(NamedKey::Pause)),
            ("PrintScreen", named(NamedKey::PrintScreen)),
            ("ScrollLock", named(NamedKey::ScrollLock)),
            ("CapsLock", named(NamedKey::CapsLock)),
            ("NumLock", named(NamedKey::NumLock)),
            ("F1", named(NamedKey::F1)),
            ("F12", named(NamedKey::F12)),
            ("F24", named(NamedKey::F24)),
            ("F35", named(NamedKey::F35)),
            ("F36", None),
            ("F0", None),
            ("MediaPlayPause", named(NamedKey::MediaPlayPause)),
            ("AudioVolumeMute", named(NamedKey::AudioVolumeMute)),
            ("Shift", named(NamedKey::ShiftLeft)),
            ("Control", named(NamedKey::ControlLeft)),
            ("Alt", named(NamedKey::AltLeft)),
            ("Meta", named(NamedKey::SuperLeft)),
            ("AltGraph", None),
            ("Fn", None),
            ("Dead", None),
            ("Process", None),
            ("Compose", None),
            ("Unidentified", None),
            ("ab", None),
            ("こんにちは", None),
            ("", None),
        ] {
            assert_eq!(map_dom_key(dom), want, "{dom:?}");
        }
    }

    #[test]
    fn modifier_keys_encode_only_under_report_all() {
        for mode in [
            KeyboardMode::empty(),
            KeyboardMode::DISAMBIGUATE_ESC_CODES,
            KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES,
        ] {
            assert!(encode_dom_key("Shift", 1, 0, None, mode).is_none());
        }
        assert_eq!(
            encode_dom_key("Shift", 1, 0, None, KeyboardMode::REPORT_ALL_KEYS_AS_ESC).as_deref(),
            Some(&b"\x1b[57441;2u"[..])
        );
    }

    #[test]
    fn encode_dom_key_honours_the_explicit_mode() {
        assert_eq!(
            encode_dom_key("ArrowUp", 0, 0, None, KeyboardMode::empty()).as_deref(),
            Some(&b"\x1b[A"[..])
        );
        assert_eq!(
            encode_dom_key("ArrowUp", 0, 0, None, KeyboardMode::APP_CURSOR).as_deref(),
            Some(&b"\x1bOA"[..]),
            "APP_CURSOR mode bits must yield SS3"
        );
        assert_eq!(
            encode_dom_key("Enter", 1, 0, None, KeyboardMode::DISAMBIGUATE_ESC_CODES).as_deref(),
            Some(&b"\x1b[13;2u"[..])
        );
        assert!(encode_dom_key("ArrowUp", 0, 2, None, KeyboardMode::empty()).is_none());
        assert!(encode_dom_key("Shift", 0, 0, None, KeyboardMode::empty()).is_none());
        assert!(encode_dom_key("ArrowUp", 0, 3, None, KeyboardMode::empty()).is_none());
    }

    #[test]
    fn a_shifted_dom_key_reports_its_unshifted_kitty_code() {
        let kitty = KeyboardMode::DISAMBIGUATE_ESC_CODES;
        let ctrl_shift = (Modifiers::CTRL | Modifiers::SHIFT).bits();
        assert_eq!(
            encode_dom_key("A", ctrl_shift, 0, Some('a'), kitty).as_deref(),
            Some(&b"\x1b[97;6u"[..])
        );
        assert_eq!(
            encode_dom_key("@", ctrl_shift, 0, Some('2'), kitty).as_deref(),
            Some(&b"\x1b[50;6u"[..])
        );
        let alternates = kitty | KeyboardMode::REPORT_ALTERNATE_KEYS;
        assert_eq!(
            encode_dom_key("A", ctrl_shift, 0, Some('a'), alternates).as_deref(),
            Some(&b"\x1b[97:65;6u"[..])
        );
    }

    #[test]
    fn encode_dom_key_text_release_requires_report_all() {
        let events = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
        assert!(encode_dom_key("a", 0, 2, None, events).is_none());
        assert!(encode_dom_key("Enter", 0, 2, None, events).is_none());

        let report_all = events | KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
        assert_eq!(
            encode_dom_key("a", 0, 2, None, report_all).as_deref(),
            Some(&b"\x1b[97;1:3u"[..])
        );
    }
}

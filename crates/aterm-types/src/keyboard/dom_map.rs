// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! DOM (`KeyboardEvent.key`) → engine keyboard mapping.
//!
//! The reusable bridge from a browser's [UI Events `KeyboardEvent.key`] value to
//! the engine's bridge-agnostic [`Key`]/[`NamedKey`], the web sibling of
//! the `aterm-winit-keymap` crate (K-2): it lives here so the wasm bindings
//! (`aterm-wasm`, `aterm-gpu-web`) share ONE table instead of each host hand-rolling a
//! legacy-only TS encoder that drifts (the old embedder encoder dropped
//! Shift/Ctrl/Alt on arrows/nav keys and could never speak the Kitty protocol
//! the engine advertises).
//!
//! No feature gate: the mapping is pure `&str` matching with no platform deps.
//!
//! [UI Events `KeyboardEvent.key`]: https://www.w3.org/TR/uievents-key/

use super::{Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key_with_layout};

/// Encode a DOM keyboard event against an EXPLICIT [`KeyboardMode`] — the one
/// implementation behind both wasm bindings' instance `encode_key` (live
/// `Terminal::keyboard_mode()`) and their free `encode_key_with_mode` (a
/// host-mirrored mode snapshot), shared here so the two can never drift.
///
/// `key` is a DOM `KeyboardEvent.key` value (see [`map_dom_key`]); `mods` is
/// the engine [`Modifiers`] bitfield (SHIFT=1, ALT=2, CTRL=4, SUPER=8);
/// `event_type` is 0=Press, 1=Repeat, 2=Release (anything else is refused,
/// never guessed); `base_layout_key` is the physical key's US-QWERTY char for
/// Kitty `REPORT_ALTERNATE_KEYS`. Returns `None` when the event encodes to
/// nothing (e.g. a release without the Kitty protocol) or the key has no
/// terminal encoding (modifier-only / IME / unidentified DOM keys).
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
    let bytes = encode_key_with_layout(
        &key,
        Modifiers::from_bits_truncate(mods),
        mode,
        event_type,
        base_layout_key,
    );
    if bytes.is_empty() { None } else { Some(bytes) }
}

/// Map a DOM [`KeyboardEvent.key`] value into the engine's [`Key`].
///
/// Returns `None` for keys the engine must not guess an encoding for:
/// modifier-only keys (`"Shift"`, `"Control"`, `"Alt"`, `"Meta"`, …), IME /
/// composition values (`"Dead"`, `"Process"`, `"Compose"`, multi-codepoint
/// strings), `"Unidentified"`, and the long tail of named DOM keys (TV /
/// launch / browser keys) no terminal escape sequence covers.
///
/// `" "` maps to [`NamedKey::Space`], not `Key::Character(' ')`, mirroring
/// `aterm_winit_keymap::map_named_key` — the legacy encoder's named-Space arm carries
/// the Ctrl+Space → NUL and Alt+Space → ESC SP semantics the encoder tests
/// pin (`tests_legacy.rs`), and its Kitty code (32) matches the character form.
///
/// [`KeyboardEvent.key`]: https://www.w3.org/TR/uievents-key/
#[must_use]
pub fn map_dom_key(key: &str) -> Option<Key> {
    // A single-codepoint value is a printable character key ("a", "A", "@",
    // "é", "あ"…). Multi-codepoint strings are IME/composed text, not a single
    // key event.
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

/// Map a named (multi-character) DOM key value into the engine's [`NamedKey`].
///
/// Covers the full set the engine can encode and a browser can report:
/// navigation, editing, locks, system keys, `F1`–`F35` (browsers report at
/// least up to `F24`; the extended range costs nothing), and the media/audio
/// cluster. `None` for everything else — including the modifier keys, which
/// DOM reports as standalone `keydown`s that have no legacy encoding and are
/// only reportable under Kitty `REPORT_ALL_KEYS_AS_ESC` (a host that wants
/// them can add the mapping then; until that exists, never guess).
#[must_use]
fn map_dom_named_key(key: &str) -> Option<NamedKey> {
    Some(match key {
        // Navigation
        "ArrowUp" => NamedKey::ArrowUp,
        "ArrowDown" => NamedKey::ArrowDown,
        "ArrowLeft" => NamedKey::ArrowLeft,
        "ArrowRight" => NamedKey::ArrowRight,
        "Home" => NamedKey::Home,
        "End" => NamedKey::End,
        "PageUp" => NamedKey::PageUp,
        "PageDown" => NamedKey::PageDown,
        // Editing
        "Backspace" => NamedKey::Backspace,
        "Delete" => NamedKey::Delete,
        "Insert" => NamedKey::Insert,
        "Enter" => NamedKey::Enter,
        "Tab" => NamedKey::Tab,
        "Escape" => NamedKey::Escape,
        // Locks and system keys
        "CapsLock" => NamedKey::CapsLock,
        "NumLock" => NamedKey::NumLock,
        "ScrollLock" => NamedKey::ScrollLock,
        "PrintScreen" => NamedKey::PrintScreen,
        "Pause" => NamedKey::Pause,
        "ContextMenu" => NamedKey::ContextMenu,
        // Function keys F1-F35
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
        // Modifier keys, canonicalized to the Left variants (DOM `key` does
        // not carry the side; `location` would, but the winit path performs
        // the same Left-canonicalization). Safe to map for every mode: the
        // encoder reports modifier keys ONLY under Kitty
        // `REPORT_ALL_KEYS_AS_ESC` and encodes nothing otherwise.
        "Shift" => NamedKey::ShiftLeft,
        "Control" => NamedKey::ControlLeft,
        "Alt" => NamedKey::AltLeft,
        "Meta" => NamedKey::SuperLeft,
        // Media and audio keys
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
        // "AltGraph"/"Fn" (no NamedKey variants), IME/composition ("Dead",
        // "Process", "Compose"), "Unidentified", and TV/launch/browser keys:
        // no terminal encoding.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_single_char_keys_to_character() {
        assert_eq!(map_dom_key("a"), Some(Key::Character('a')));
        assert_eq!(map_dom_key("A"), Some(Key::Character('A')));
        assert_eq!(map_dom_key("@"), Some(Key::Character('@')));
        // Non-ASCII single codepoints are real character keys (layouts/IME).
        assert_eq!(map_dom_key("é"), Some(Key::Character('é')));
    }

    #[test]
    fn space_maps_to_the_named_form_legacy_expects() {
        // DOM reports the space bar as the literal " " string. The engine's
        // legacy encoder pins Ctrl+Space → NUL on the NAMED Space arm
        // (tests_legacy.rs), mirroring aterm-winit-keymap's Space → NamedKey::Space.
        assert_eq!(map_dom_key(" "), Some(Key::Named(NamedKey::Space)));
    }

    #[test]
    fn maps_named_navigation_and_editing_keys() {
        assert_eq!(map_dom_key("ArrowUp"), Some(Key::Named(NamedKey::ArrowUp)));
        assert_eq!(map_dom_key("Enter"), Some(Key::Named(NamedKey::Enter)));
        assert_eq!(map_dom_key("Tab"), Some(Key::Named(NamedKey::Tab)));
        assert_eq!(
            map_dom_key("Backspace"),
            Some(Key::Named(NamedKey::Backspace))
        );
        assert_eq!(map_dom_key("Delete"), Some(Key::Named(NamedKey::Delete)));
        assert_eq!(map_dom_key("Home"), Some(Key::Named(NamedKey::Home)));
        assert_eq!(map_dom_key("End"), Some(Key::Named(NamedKey::End)));
        assert_eq!(map_dom_key("PageUp"), Some(Key::Named(NamedKey::PageUp)));
        assert_eq!(
            map_dom_key("PageDown"),
            Some(Key::Named(NamedKey::PageDown))
        );
        assert_eq!(map_dom_key("Insert"), Some(Key::Named(NamedKey::Insert)));
        assert_eq!(map_dom_key("Escape"), Some(Key::Named(NamedKey::Escape)));
    }

    #[test]
    fn maps_system_and_lock_keys() {
        assert_eq!(
            map_dom_key("ContextMenu"),
            Some(Key::Named(NamedKey::ContextMenu))
        );
        assert_eq!(map_dom_key("Pause"), Some(Key::Named(NamedKey::Pause)));
        assert_eq!(
            map_dom_key("PrintScreen"),
            Some(Key::Named(NamedKey::PrintScreen))
        );
        assert_eq!(
            map_dom_key("ScrollLock"),
            Some(Key::Named(NamedKey::ScrollLock))
        );
        assert_eq!(
            map_dom_key("CapsLock"),
            Some(Key::Named(NamedKey::CapsLock))
        );
        assert_eq!(map_dom_key("NumLock"), Some(Key::Named(NamedKey::NumLock)));
    }

    #[test]
    fn maps_function_keys_through_f24_and_beyond() {
        assert_eq!(map_dom_key("F1"), Some(Key::Named(NamedKey::F1)));
        assert_eq!(map_dom_key("F12"), Some(Key::Named(NamedKey::F12)));
        assert_eq!(map_dom_key("F24"), Some(Key::Named(NamedKey::F24)));
        assert_eq!(map_dom_key("F35"), Some(Key::Named(NamedKey::F35)));
        // Not a function key the spec defines.
        assert_eq!(map_dom_key("F36"), None);
        assert_eq!(map_dom_key("F0"), None);
    }

    #[test]
    fn maps_media_keys() {
        assert_eq!(
            map_dom_key("MediaPlayPause"),
            Some(Key::Named(NamedKey::MediaPlayPause))
        );
        assert_eq!(
            map_dom_key("AudioVolumeMute"),
            Some(Key::Named(NamedKey::AudioVolumeMute))
        );
    }

    #[test]
    fn modifier_keys_map_left_canonical() {
        // Modifier keydowns map (Left-canonical) so Kitty
        // REPORT_ALL_KEYS_AS_ESC apps can receive them; the ENCODER keeps
        // them silent in every other mode, so no legacy byte can ever be
        // guessed for a bare "Shift".
        assert_eq!(map_dom_key("Shift"), Some(Key::Named(NamedKey::ShiftLeft)));
        assert_eq!(
            map_dom_key("Control"),
            Some(Key::Named(NamedKey::ControlLeft))
        );
        assert_eq!(map_dom_key("Alt"), Some(Key::Named(NamedKey::AltLeft)));
        assert_eq!(map_dom_key("Meta"), Some(Key::Named(NamedKey::SuperLeft)));
        // No NamedKey variants exist for these — still refused.
        for key in ["AltGraph", "Fn"] {
            assert_eq!(map_dom_key(key), None, "{key} must not map");
        }
    }

    #[test]
    fn modifier_keys_encode_only_under_report_all() {
        // Every non-report-all mode: silence (press AND release).
        for mode in [
            KeyboardMode::empty(),
            KeyboardMode::DISAMBIGUATE_ESC_CODES,
            KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES,
        ] {
            assert!(encode_dom_key("Shift", 1, 0, None, mode).is_none());
        }
        // Report-all: the kitty modifier-key report, with the press effect.
        assert_eq!(
            encode_dom_key("Shift", 1, 0, None, KeyboardMode::REPORT_ALL_KEYS_AS_ESC).as_deref(),
            Some(&b"\x1b[57441;2u"[..])
        );
    }

    #[test]
    fn ime_and_unidentified_keys_are_none() {
        for key in ["Dead", "Process", "Compose", "Unidentified"] {
            assert_eq!(map_dom_key(key), None, "{key} must not map");
        }
        // Multi-codepoint composed text is not a single key event.
        assert_eq!(map_dom_key("ab"), None);
        assert_eq!(map_dom_key("こんにちは"), None);
        assert_eq!(map_dom_key(""), None);
    }

    #[test]
    fn encode_dom_key_honours_the_explicit_mode() {
        // Empty mode: legacy CSI A; APP_CURSOR (DECCKM): SS3.
        assert_eq!(
            encode_dom_key("ArrowUp", 0, 0, None, KeyboardMode::empty()).as_deref(),
            Some(&b"\x1b[A"[..])
        );
        assert_eq!(
            encode_dom_key("ArrowUp", 0, 0, None, KeyboardMode::APP_CURSOR).as_deref(),
            Some(&b"\x1bOA"[..]),
            "APP_CURSOR mode bits must yield SS3"
        );
        // Kitty disambiguate: Shift+Enter is a CSI-u report.
        assert_eq!(
            encode_dom_key("Enter", 1, 0, None, KeyboardMode::DISAMBIGUATE_ESC_CODES).as_deref(),
            Some(&b"\x1b[13;2u"[..])
        );
        // Refused, never guessed: releases without Kitty, unmappable DOM keys,
        // and out-of-range event types.
        assert!(encode_dom_key("ArrowUp", 0, 2, None, KeyboardMode::empty()).is_none());
        assert!(encode_dom_key("Shift", 0, 0, None, KeyboardMode::empty()).is_none());
        assert!(encode_dom_key("ArrowUp", 0, 3, None, KeyboardMode::empty()).is_none());
    }

    #[test]
    fn encode_dom_key_text_release_requires_report_all() {
        // `REPORT_EVENT_TYPES` does not turn text-producing keys into escape
        // reports. Their releases remain unrepresentable until report-all is
        // negotiated; this is Codex's 1|2|4 mode.
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

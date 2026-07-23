// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Platform (winit) → engine keyboard mapping (K-2).
//!
//! The reusable bridge from winit's `Key`/`NamedKey`/`PhysicalKey` to the
//! engine's bridge-agnostic [`Key`]/[`NamedKey`], plus the US-QWERTY
//! `base_layout_key` derivation the Kitty `REPORT_ALTERNATE_KEYS` enhancement
//! needs. It lives in `aterm-types` (behind the `winit-keymap` feature) so the
//! GUI and the future native shell share ONE table instead of each hand-rolling
//! an inline match that drifts (the old GUI match stopped at F12, dropped
//! Super/Cmd, and had no numpad/media keys).
//!
//! Only built when the `winit-keymap` feature is on, so non-GUI consumers of
//! `aterm-types` (the FFI / Alacritty bridges) never link winit.

use winit::keyboard::{Key as WinitKey, KeyCode, NamedKey as WinitNamed, PhysicalKey};

use super::{Key, NamedKey};

/// Map a winit logical [`WinitKey`] (e.g. `ev.logical_key` or
/// `key_without_modifiers()`) into the engine's [`Key`].
///
/// Returns `None` for keys the engine has no encoding for (dead keys,
/// unidentified keys, and the long tail of winit `NamedKey` variants — TV/IME
/// composition/launch/browser keys — that no terminal escape sequence covers).
/// A `Character` logical key carries the single base codepoint; multi-grapheme
/// logical strings (rare, IME-ish) are not single-key events and yield `None`.
#[must_use]
pub fn map_logical_key(key: &WinitKey) -> Option<Key> {
    match key {
        WinitKey::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                // Not a single-codepoint key press (IME / composed text).
                return None;
            }
            Some(Key::Character(c))
        }
        WinitKey::Named(named) => map_named_key(*named).map(Key::Named),
        // Dead keys and unidentified keys have no direct terminal encoding;
        // they reach the PTY (if at all) via IME Commit, not as a key event.
        WinitKey::Dead(_) | WinitKey::Unidentified(_) => None,
    }
}

/// Map a winit [`WinitNamed`] into the engine's [`NamedKey`].
///
/// Covers the FULL set the engine can encode: navigation, editing, locks,
/// system keys, F1-F35, the media/audio cluster, and the modifier keys
/// (including Super/Cmd, which the old inline match dropped). The numpad keys
/// are NOT here — winit reports them as `NamedKey` only when they carry a
/// logical meaning; the physical numpad mapping lives in [`map_physical_numpad`].
/// Returns `None` for variants with no terminal encoding (TV, IME composition,
/// launch/browser/phone keys, etc.).
#[must_use]
pub fn map_named_key(named: WinitNamed) -> Option<NamedKey> {
    Some(match named {
        // Navigation
        WinitNamed::ArrowUp => NamedKey::ArrowUp,
        WinitNamed::ArrowDown => NamedKey::ArrowDown,
        WinitNamed::ArrowLeft => NamedKey::ArrowLeft,
        WinitNamed::ArrowRight => NamedKey::ArrowRight,
        WinitNamed::Home => NamedKey::Home,
        WinitNamed::End => NamedKey::End,
        WinitNamed::PageUp => NamedKey::PageUp,
        WinitNamed::PageDown => NamedKey::PageDown,
        // Editing
        WinitNamed::Backspace => NamedKey::Backspace,
        WinitNamed::Delete => NamedKey::Delete,
        WinitNamed::Insert => NamedKey::Insert,
        WinitNamed::Enter => NamedKey::Enter,
        WinitNamed::Tab => NamedKey::Tab,
        WinitNamed::Escape => NamedKey::Escape,
        WinitNamed::Space => NamedKey::Space,
        // Locks and system keys
        WinitNamed::CapsLock => NamedKey::CapsLock,
        WinitNamed::NumLock => NamedKey::NumLock,
        WinitNamed::ScrollLock => NamedKey::ScrollLock,
        WinitNamed::PrintScreen => NamedKey::PrintScreen,
        WinitNamed::Pause => NamedKey::Pause,
        WinitNamed::ContextMenu => NamedKey::ContextMenu,
        // Function keys F1-F35
        WinitNamed::F1 => NamedKey::F1,
        WinitNamed::F2 => NamedKey::F2,
        WinitNamed::F3 => NamedKey::F3,
        WinitNamed::F4 => NamedKey::F4,
        WinitNamed::F5 => NamedKey::F5,
        WinitNamed::F6 => NamedKey::F6,
        WinitNamed::F7 => NamedKey::F7,
        WinitNamed::F8 => NamedKey::F8,
        WinitNamed::F9 => NamedKey::F9,
        WinitNamed::F10 => NamedKey::F10,
        WinitNamed::F11 => NamedKey::F11,
        WinitNamed::F12 => NamedKey::F12,
        WinitNamed::F13 => NamedKey::F13,
        WinitNamed::F14 => NamedKey::F14,
        WinitNamed::F15 => NamedKey::F15,
        WinitNamed::F16 => NamedKey::F16,
        WinitNamed::F17 => NamedKey::F17,
        WinitNamed::F18 => NamedKey::F18,
        WinitNamed::F19 => NamedKey::F19,
        WinitNamed::F20 => NamedKey::F20,
        WinitNamed::F21 => NamedKey::F21,
        WinitNamed::F22 => NamedKey::F22,
        WinitNamed::F23 => NamedKey::F23,
        WinitNamed::F24 => NamedKey::F24,
        WinitNamed::F25 => NamedKey::F25,
        WinitNamed::F26 => NamedKey::F26,
        WinitNamed::F27 => NamedKey::F27,
        WinitNamed::F28 => NamedKey::F28,
        WinitNamed::F29 => NamedKey::F29,
        WinitNamed::F30 => NamedKey::F30,
        WinitNamed::F31 => NamedKey::F31,
        WinitNamed::F32 => NamedKey::F32,
        WinitNamed::F33 => NamedKey::F33,
        WinitNamed::F34 => NamedKey::F34,
        WinitNamed::F35 => NamedKey::F35,
        // Media and audio keys
        WinitNamed::MediaPlay => NamedKey::MediaPlay,
        WinitNamed::MediaPause => NamedKey::MediaPause,
        WinitNamed::MediaPlayPause => NamedKey::MediaPlayPause,
        WinitNamed::MediaStop => NamedKey::MediaStop,
        WinitNamed::MediaFastForward => NamedKey::MediaFastForward,
        WinitNamed::MediaRewind => NamedKey::MediaRewind,
        WinitNamed::MediaTrackNext => NamedKey::MediaTrackNext,
        WinitNamed::MediaTrackPrevious => NamedKey::MediaTrackPrevious,
        WinitNamed::MediaRecord => NamedKey::MediaRecord,
        WinitNamed::AudioVolumeDown => NamedKey::AudioVolumeDown,
        WinitNamed::AudioVolumeUp => NamedKey::AudioVolumeUp,
        WinitNamed::AudioVolumeMute => NamedKey::AudioVolumeMute,
        // Modifier keys reported as key events. winit reports `Alt`/`Control`/
        // `Shift`/`Super`/`Meta`/`Hyper` without a left/right distinction in the
        // logical key (the side lives in `KeyLocation`); map to the LEFT variant
        // as the canonical representative — the engine's Kitty modifier encoding
        // (`kitty_modifiers_for_event`) treats left/right identically.
        WinitNamed::Shift => NamedKey::ShiftLeft,
        WinitNamed::Control => NamedKey::ControlLeft,
        WinitNamed::Alt => NamedKey::AltLeft,
        WinitNamed::Super => NamedKey::SuperLeft,
        WinitNamed::Hyper => NamedKey::HyperLeft,
        WinitNamed::Meta => NamedKey::MetaLeft,
        // No terminal encoding: IME composition keys, TV/launch/browser/phone
        // keys, brightness/power, etc. fall through to None.
        _ => return None,
    })
}

/// Map a physical numpad [`KeyCode`] into the engine's numpad [`NamedKey`].
///
/// winit usually surfaces numpad keys via the LOGICAL key (a digit character,
/// or `NamedKey::Enter`), but the physical-key path lets the engine drive the
/// DECKPAM application-keypad sequences (SS3) that differ from the main row.
/// Returns `None` for any non-numpad physical key.
#[must_use]
pub fn map_physical_numpad(code: KeyCode) -> Option<NamedKey> {
    Some(match code {
        KeyCode::Numpad0 => NamedKey::Numpad0,
        KeyCode::Numpad1 => NamedKey::Numpad1,
        KeyCode::Numpad2 => NamedKey::Numpad2,
        KeyCode::Numpad3 => NamedKey::Numpad3,
        KeyCode::Numpad4 => NamedKey::Numpad4,
        KeyCode::Numpad5 => NamedKey::Numpad5,
        KeyCode::Numpad6 => NamedKey::Numpad6,
        KeyCode::Numpad7 => NamedKey::Numpad7,
        KeyCode::Numpad8 => NamedKey::Numpad8,
        KeyCode::Numpad9 => NamedKey::Numpad9,
        KeyCode::NumpadDecimal => NamedKey::NumpadDecimal,
        KeyCode::NumpadDivide => NamedKey::NumpadDivide,
        KeyCode::NumpadMultiply | KeyCode::NumpadStar => NamedKey::NumpadMultiply,
        KeyCode::NumpadSubtract => NamedKey::NumpadSubtract,
        KeyCode::NumpadAdd => NamedKey::NumpadAdd,
        KeyCode::NumpadEnter => NamedKey::NumpadEnter,
        KeyCode::NumpadEqual => NamedKey::NumpadEqual,
        KeyCode::NumpadComma => NamedKey::NumpadSeparator,
        _ => return None,
    })
}

/// The character a physical key produces on a US-QWERTY layout, for the Kitty
/// `REPORT_ALTERNATE_KEYS` `base_layout_key` (#7678).
///
/// The engine emits this as the third colon-delimited code so a remote app can
/// reason about the PHYSICAL key independent of the user's active layout (e.g. a
/// Dvorak or AZERTY user pressing the QWERTY-`a` position). Returns the
/// UNSHIFTED US-QWERTY character for the alphanumeric and symbol rows; `None`
/// for keys with no printable US-QWERTY character (function/navigation/modifier
/// keys), where `base_layout_key` is simply omitted.
#[must_use]
pub fn base_layout_key_for(physical: PhysicalKey) -> Option<char> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(match code {
        KeyCode::KeyA => 'a',
        KeyCode::KeyB => 'b',
        KeyCode::KeyC => 'c',
        KeyCode::KeyD => 'd',
        KeyCode::KeyE => 'e',
        KeyCode::KeyF => 'f',
        KeyCode::KeyG => 'g',
        KeyCode::KeyH => 'h',
        KeyCode::KeyI => 'i',
        KeyCode::KeyJ => 'j',
        KeyCode::KeyK => 'k',
        KeyCode::KeyL => 'l',
        KeyCode::KeyM => 'm',
        KeyCode::KeyN => 'n',
        KeyCode::KeyO => 'o',
        KeyCode::KeyP => 'p',
        KeyCode::KeyQ => 'q',
        KeyCode::KeyR => 'r',
        KeyCode::KeyS => 's',
        KeyCode::KeyT => 't',
        KeyCode::KeyU => 'u',
        KeyCode::KeyV => 'v',
        KeyCode::KeyW => 'w',
        KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y',
        KeyCode::KeyZ => 'z',
        KeyCode::Digit0 => '0',
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        KeyCode::Backquote => '`',
        KeyCode::Minus => '-',
        KeyCode::Equal => '=',
        KeyCode::BracketLeft => '[',
        KeyCode::BracketRight => ']',
        KeyCode::Backslash => '\\',
        KeyCode::Semicolon => ';',
        KeyCode::Quote => '\'',
        KeyCode::Comma => ',',
        KeyCode::Period => '.',
        KeyCode::Slash => '/',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    #[test]
    fn maps_super_cmd_modifier() {
        // The old inline GUI match dropped Super/Cmd entirely (K-2 bug).
        assert_eq!(map_named_key(WinitNamed::Super), Some(NamedKey::SuperLeft));
    }

    #[test]
    fn maps_function_keys_past_f12() {
        // The old inline match stopped at F12.
        assert_eq!(map_named_key(WinitNamed::F13), Some(NamedKey::F13));
        assert_eq!(map_named_key(WinitNamed::F24), Some(NamedKey::F24));
        assert_eq!(map_named_key(WinitNamed::F35), Some(NamedKey::F35));
    }

    #[test]
    fn maps_media_keys() {
        assert_eq!(
            map_named_key(WinitNamed::MediaPlayPause),
            Some(NamedKey::MediaPlayPause)
        );
        assert_eq!(
            map_named_key(WinitNamed::AudioVolumeMute),
            Some(NamedKey::AudioVolumeMute)
        );
    }

    #[test]
    fn maps_physical_numpad() {
        assert_eq!(
            map_physical_numpad(KeyCode::Numpad5),
            Some(NamedKey::Numpad5)
        );
        assert_eq!(
            map_physical_numpad(KeyCode::NumpadEnter),
            Some(NamedKey::NumpadEnter)
        );
        assert_eq!(map_physical_numpad(KeyCode::KeyA), None);
    }

    #[test]
    fn character_logical_key_maps_through() {
        let k = WinitKey::Character(SmolStr::new("a"));
        assert_eq!(map_logical_key(&k), Some(Key::Character('a')));
    }

    #[test]
    fn multi_codepoint_logical_key_is_not_a_single_key() {
        let k = WinitKey::Character(SmolStr::new("ab"));
        assert_eq!(map_logical_key(&k), None);
    }

    #[test]
    fn base_layout_key_is_us_qwerty() {
        assert_eq!(
            base_layout_key_for(PhysicalKey::Code(KeyCode::KeyA)),
            Some('a')
        );
        assert_eq!(
            base_layout_key_for(PhysicalKey::Code(KeyCode::Digit1)),
            Some('1')
        );
        assert_eq!(
            base_layout_key_for(PhysicalKey::Code(KeyCode::Slash)),
            Some('/')
        );
        // No printable US-QWERTY char for a function key.
        assert_eq!(base_layout_key_for(PhysicalKey::Code(KeyCode::F1)), None);
    }
}

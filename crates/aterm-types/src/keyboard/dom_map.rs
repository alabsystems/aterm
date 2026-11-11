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
#[path = "dom_map_tests.rs"]
mod tests;

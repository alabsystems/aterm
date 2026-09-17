// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Platform (winit) → engine keyboard mapping (K-2).
//!
//! The reusable bridge from winit's `Key`/`NamedKey`/`PhysicalKey` to the
//! engine's bridge-agnostic [`Key`]/[`NamedKey`], plus the US-QWERTY
//! `base_layout_key` derivation the Kitty `REPORT_ALTERNATE_KEYS` enhancement
//! needs. The GUI and the future native shell share ONE table instead of each
//! hand-rolling an inline match that drifts (the old GUI match stopped at F12,
//! dropped Super/Cmd, and had no numpad/media keys).
//!
//! WHY ITS OWN CRATE. This was `aterm-types/src/keyboard/winit_map.rs` behind an
//! optional `winit-keymap` feature whose stated job was "so non-GUI consumers of
//! `aterm-types` never link winit". Cargo unifies features across a workspace
//! resolve, so a plain `cargo build --workspace` turned that feature on for
//! everyone and `aterm-ctl` — aterm-types + aterm-uds, no third-party
//! dependency — shipped linking AppKit, Carbon, ApplicationServices,
//! CoreGraphics, CoreVideo, CoreFoundation, Foundation and libobjc. A separate
//! crate is the enforceable form of the same intent: `aterm-types` no longer
//! mentions winit at all, so nothing can unify an edge that does not exist.
//! See `crates/aterm-winit-keymap/Cargo.toml` and the measurement in
//! `crates/aterm-bench/benches/startup_exec.rs`.

use winit::keyboard::{Key as WinitKey, KeyCode, KeyLocation, NamedKey as WinitNamed, PhysicalKey};

use aterm_types::keyboard::{Key, NamedKey};

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
/// (including Super/Cmd, which the old inline match dropped). The keypad keys
/// are NOT here, because winit has none to map: a keypad press arrives as the
/// main block's logical key (`Character("5")`, `Named(Enter)`, `Named(End)`)
/// with the keypad recorded in `KeyLocation` alone. [`map_numpad_key`], which
/// `build_key_input` asks FIRST, is the one road to the engine's `Numpad*`.
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

/// The keypad key a winit press IS — the one road from a physical numpad to
/// the engine's `Numpad*` keys (DECKPAM's SS3 forms, kitty's
/// `CSI 57399..57427 u`) — or `None` when the press is not a keypad key the
/// engine names. winit has no keypad key of its own: KP_5 arrives as
/// `Character("5")` and KP_Enter as `Named(Enter)`, the very keys the main
/// block produces, with the keypad recorded in `location` alone. So this is
/// asked BEFORE [`map_logical_key`], which cannot see the keypad at all.
///
/// `location` says whether the key is on the keypad; `logical` — the event's
/// `logical_key`, which carries NumLock — says what it means there. The
/// identity is the GLYPH the layout typed, not the scancode: a de-DE
/// `keypad(comma)` decimal key types `,` and is KP_Separator (SS3 `l`), which
/// is how xterm and kitty read it, where a scancode table would type `.`.
/// Enter on the keypad is KP_Enter; a NumLock-off navigation key is its keypad
/// twin. `physical` settles only the nameless NumLock-off centre key — the one
/// keypad key winit names on no platform (`Unidentified` from xkb's KP_Begin,
/// `Clear` on Windows) — as NumpadBegin. A keypad key with no keypad identity
/// (KP_Tab, KP_Space, a key remapped to a letter) yields `None` and takes the
/// logical route as before.
///
/// The caller passes `logical_key`, never `key_without_modifiers()`: the Linux
/// backends derive the latter from xkb's LEVEL-0 keysym, and the KEYPAD type
/// keeps the NumLock-OFF symbol at level 0 (`types/numpad`: `map[None] =
/// Level1`; `symbols/keypad(x11)`: `<KP1> { [ KP_End, KP_1 ] }`), so a
/// NumLock-on keypad 1 reads back from it as End.
#[must_use]
pub fn map_numpad_key(
    physical: PhysicalKey,
    logical: &WinitKey,
    location: KeyLocation,
) -> Option<Key> {
    if location != KeyLocation::Numpad {
        return None;
    }
    let named = match logical {
        WinitKey::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                // Not a single-codepoint key press (IME / composed text).
                return None;
            }
            match c {
                '0' => NamedKey::Numpad0,
                '1' => NamedKey::Numpad1,
                '2' => NamedKey::Numpad2,
                '3' => NamedKey::Numpad3,
                '4' => NamedKey::Numpad4,
                '5' => NamedKey::Numpad5,
                '6' => NamedKey::Numpad6,
                '7' => NamedKey::Numpad7,
                '8' => NamedKey::Numpad8,
                '9' => NamedKey::Numpad9,
                '.' => NamedKey::NumpadDecimal,
                ',' => NamedKey::NumpadSeparator,
                '/' => NamedKey::NumpadDivide,
                '*' => NamedKey::NumpadMultiply,
                '-' => NamedKey::NumpadSubtract,
                '+' => NamedKey::NumpadAdd,
                '=' => NamedKey::NumpadEqual,
                _ => return None,
            }
        }
        WinitKey::Named(WinitNamed::Enter) => NamedKey::NumpadEnter,
        WinitKey::Named(WinitNamed::ArrowUp) => NamedKey::NumpadArrowUp,
        WinitKey::Named(WinitNamed::ArrowDown) => NamedKey::NumpadArrowDown,
        WinitKey::Named(WinitNamed::ArrowLeft) => NamedKey::NumpadArrowLeft,
        WinitKey::Named(WinitNamed::ArrowRight) => NamedKey::NumpadArrowRight,
        WinitKey::Named(WinitNamed::Home) => NamedKey::NumpadHome,
        WinitKey::Named(WinitNamed::End) => NamedKey::NumpadEnd,
        WinitKey::Named(WinitNamed::PageUp) => NamedKey::NumpadPageUp,
        WinitKey::Named(WinitNamed::PageDown) => NamedKey::NumpadPageDown,
        WinitKey::Named(WinitNamed::Insert) => NamedKey::NumpadInsert,
        WinitKey::Named(WinitNamed::Delete) => NamedKey::NumpadDelete,
        // The NumLock-off centre key: matched on "the engine has no name for
        // what winit reported" rather than on a winit variant, so xkb's
        // `Unidentified` and Windows' `Clear` take the same arm.
        _ if physical == PhysicalKey::Code(KeyCode::Numpad5)
            && map_logical_key(logical).is_none() =>
        {
            NamedKey::NumpadBegin
        }
        _ => return None,
    };
    Some(Key::Named(named))
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

    fn ch(c: &str) -> WinitKey {
        WinitKey::Character(SmolStr::new(c))
    }

    fn unidentified() -> WinitKey {
        WinitKey::Unidentified(winit::keyboard::NativeKey::Unidentified)
    }

    /// A digit at the keypad location is the keypad digit — the engine key
    /// that reaches DECKPAM's SS3 and kitty's KP codes — not the main row's
    /// `Character('5')` that the logical key alone would give.
    #[test]
    fn numpad_location_promotes_digit_to_numpad_key() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad5),
                &ch("5"),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::Numpad5))
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::NumpadAdd),
                &ch("+"),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadAdd))
        );
    }

    /// KP_Enter arrives from every platform as the main block's `Enter`; on
    /// the keypad it is NumpadEnter (CR / SS3 M / `CSI 57414 u`).
    #[test]
    fn numpad_enter_is_not_main_enter() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::NumpadEnter),
                &WinitKey::Named(WinitNamed::Enter),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadEnter))
        );
    }

    /// NumLock off: the navigation keysym is the keypad twin of the main-block
    /// key, and the centre key — which winit names on no platform — is
    /// NumpadBegin by its physical code, whether xkb reports `Unidentified` or
    /// Windows reports `Clear`. Only the centre key gets that rescue.
    #[test]
    fn numlock_off_navigation_gets_numpad_twin() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad4),
                &WinitKey::Named(WinitNamed::ArrowLeft),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadArrowLeft))
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad1),
                &WinitKey::Named(WinitNamed::End),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadEnd))
        );
        for centre in [unidentified(), WinitKey::Named(WinitNamed::Clear)] {
            assert_eq!(
                map_numpad_key(
                    PhysicalKey::Code(KeyCode::Numpad5),
                    &centre,
                    KeyLocation::Numpad
                ),
                Some(Key::Named(NamedKey::NumpadBegin)),
                "{centre:?} on the physical keypad 5 is KP_Begin"
            );
        }
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad4),
                &unidentified(),
                KeyLocation::Numpad
            ),
            None,
            "an unnamed key elsewhere on the keypad stays unmapped"
        );
    }

    /// The identity is the glyph the LAYOUT typed, not the scancode: the same
    /// physical decimal key is KP_Decimal on a US layout and KP_Separator on a
    /// de-DE `keypad(comma)` layout, and must type `,` there — never `.`.
    #[test]
    fn keypad_identity_is_the_glyph_not_the_scancode() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::NumpadDecimal),
                &ch("."),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadDecimal))
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::NumpadDecimal),
                &ch(","),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadSeparator))
        );
    }

    /// A keypad key with no keypad identity — KP_Tab, or a keypad key the user
    /// remapped to a letter — is left to the logical route.
    #[test]
    fn keypad_key_without_keypad_identity_takes_the_logical_route() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
                &WinitKey::Named(WinitNamed::Tab),
                KeyLocation::Numpad
            ),
            None
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad1),
                &ch("a"),
                KeyLocation::Numpad
            ),
            None
        );
    }

    /// The negative control: the main block is untouched. `5` above `R` and
    /// the main Return are not keypad keys, and the LOCATION — not the
    /// scancode — is what says a key is on the keypad.
    #[test]
    fn standard_location_is_untouched() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Digit5),
                &ch("5"),
                KeyLocation::Standard
            ),
            None
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Enter),
                &WinitKey::Named(WinitNamed::Enter),
                KeyLocation::Standard
            ),
            None
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad5),
                &ch("5"),
                KeyLocation::Standard
            ),
            None
        );
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

    /// THE REASON THIS CRATE EXISTS, as an always-on assertion rather than a
    /// comment: `aterm-types` must not name `winit` anywhere in its manifest.
    ///
    /// It used to, as `winit = { workspace = true, optional = true }` behind a
    /// `winit-keymap` feature whose comment promised non-GUI consumers would
    /// never pull winit. Cargo unifies features across a workspace resolve, so
    /// `cargo build --workspace` enabled it for everyone: `aterm-ctl`, which
    /// declares only aterm-types + aterm-uds and no third-party dependency at
    /// all, came out linking AppKit, Carbon, ApplicationServices, CoreGraphics,
    /// CoreVideo, CoreFoundation, Foundation and libobjc — 7 framework load
    /// commands dyld maps and initialises before the first instruction of
    /// `main`, on a route that cannot call a symbol in any of them. Measured on
    /// this tree with the same binary built both ways: 2.49 -> 1.15 ms per exec
    /// (`crates/aterm-bench/benches/startup_exec.rs`).
    ///
    /// The BENCH's Mach-O reach guard catches the same regression from the other
    /// end, but only when someone runs it. This catches it in `cargo test`.
    #[test]
    fn aterm_types_names_no_platform_dependency() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/aterm-winit-keymap sits under crates/")
            .join("aterm-types/Cargo.toml");
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
        // Comments explain the history and say the word; dependency lines and
        // feature lines are what must not.
        for (i, line) in text.lines().enumerate() {
            let code = line.split('#').next().unwrap_or("");
            assert!(
                !code.contains("winit"),
                "{}:{} names winit in code (not a comment): {line:?}\n\
                 aterm-types is the platform-free vocabulary crate every consumer shares, \
                 including binaries with no third-party dependencies at all. An optional \
                 winit dep here does NOT stay off for them — Cargo unifies features across \
                 the workspace resolve — it links AppKit into all of them. Put the platform \
                 map in its own crate, the way this one is.",
                manifest.display(),
                i + 1
            );
        }
    }
}

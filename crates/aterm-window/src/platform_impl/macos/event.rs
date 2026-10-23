// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/event.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::ffi::c_void;

use aterm_objc::send::{
    send_id, send_id_usize_point_usize_f64_isize_id_i16_isize_isize, send_u16, send_usize,
};

use crate::keyboard::SmolStr;
use aterm_objc::cf::{CFDataGetBytePtr, CFDataRef, CFRelease};
use aterm_objc::{autoreleasepool, class, run_on_main, sel, CGPoint, Id, Obj};

use super::aterm_objc_seam::{self as seam, consts};

use crate::event::{ElementState, KeyEvent, Modifiers};
use crate::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersKeys, ModifiersState, NamedKey, NativeKey, NativeKeyCode,
    PhysicalKey,
};
use crate::platform::macos::OptionAsAlt;
use crate::platform_impl::platform::ffi;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyEventExtra {
    pub text_with_all_modifiers: Option<SmolStr>,
    pub key_without_modifiers: Key,
}

/// Ignores ALL modifiers.
pub fn get_modifierless_char(scancode: u16) -> Key {
    let mut string = [0; 16];
    let input_source;
    let layout;
    unsafe {
        input_source = ffi::TISCopyCurrentKeyboardLayoutInputSource();
        if input_source.is_null() {
            tracing::error!("`TISCopyCurrentKeyboardLayoutInputSource` returned null ptr");
            return Key::Unidentified(NativeKey::MacOS(scancode));
        }
        let layout_data =
            ffi::TISGetInputSourceProperty(input_source, ffi::kTISPropertyUnicodeKeyLayoutData);
        if layout_data.is_null() {
            CFRelease(input_source as *mut c_void);
            tracing::error!("`TISGetInputSourceProperty` returned null ptr");
            return Key::Unidentified(NativeKey::MacOS(scancode));
        }
        layout = CFDataGetBytePtr(layout_data as CFDataRef) as *const ffi::UCKeyboardLayout;
    }
    let keyboard_type = run_on_main(|_mt| unsafe { ffi::LMGetKbdType() });

    let mut result_len = 0;
    let mut dead_keys = 0;
    let modifiers = 0;
    let translate_result = unsafe {
        ffi::UCKeyTranslate(
            layout,
            scancode,
            ffi::kUCKeyActionDisplay,
            modifiers,
            keyboard_type as u32,
            ffi::kUCKeyTranslateNoDeadKeysMask,
            &mut dead_keys,
            string.len() as ffi::UniCharCount,
            &mut result_len,
            string.as_mut_ptr(),
        )
    };
    unsafe {
        CFRelease(input_source as *mut c_void);
    }
    if translate_result != 0 {
        tracing::error!("`UCKeyTranslate` returned with the non-zero value: {}", translate_result);
        return Key::Unidentified(NativeKey::MacOS(scancode));
    }
    if result_len == 0 {
        // keyboard layouts will hit this code path.
        return Key::Unidentified(NativeKey::MacOS(scancode));
    }
    let chars = String::from_utf16_lossy(&string[0..result_len as usize]);
    Key::Character(SmolStr::new(chars))
}

/// LOCAL PATCH (aterm), 2026-10-20.
pub(crate) trait LayoutKeys {
    /// The character `scancode` types with `mods`, Carbon's `UCKeyTranslate` modifier byte.
    fn typed(&self, scancode: u16, mods: u32) -> Option<char>;
}

/// `UCKeyTranslate`'s modifier byte: Carbon's `shiftKey` and `optionKey` (`EventModifiers`) >> 8.
pub(crate) const UC_SHIFT: u32 = 0x02;
pub(crate) const UC_OPTION: u32 = 0x08;

/// The printable ASCII character the Option chord on `scancode` types (with Shift when `shift`).
pub(crate) fn option_only_ascii(
    keys: &impl LayoutKeys,
    scancode: u16,
    shift: bool,
) -> Option<char> {
    let typed = keys.typed(scancode, UC_OPTION | if shift { UC_SHIFT } else { 0 })?;
    if !typed.is_ascii_graphic() {
        return None;
    }
    let elsewhere = (0..128u16)
        .any(|code| [0, UC_SHIFT].into_iter().any(|m| keys.typed(code, m) == Some(typed)));
    (!elsewhere).then_some(typed)
}

/// The selected keyboard layout's key table, held while it is read.
struct CurrentLayout {
    source: ffi::TISInputSourceRef,
    layout: *const ffi::UCKeyboardLayout,
    keyboard_type: u32,
}

impl CurrentLayout {
    /// The selected keyboard layout (`TISCopyCurrentKeyboardLayoutInputSource`), or `None`
    /// when it has no Unicode key table.
    fn copy() -> Option<CurrentLayout> {
        // SAFETY: both calls take and return Carbon references: the source is +1.
        unsafe {
            let source = ffi::TISCopyCurrentKeyboardLayoutInputSource();
            if source.is_null() {
                return None;
            }
            let data =
                ffi::TISGetInputSourceProperty(source, ffi::kTISPropertyUnicodeKeyLayoutData);
            if data.is_null() {
                CFRelease(source as *mut c_void);
                return None;
            }
            let layout = CFDataGetBytePtr(data as CFDataRef) as *const ffi::UCKeyboardLayout;
            let keyboard_type = u32::from(run_on_main(|_mt| ffi::LMGetKbdType()));
            Some(CurrentLayout { source, layout, keyboard_type })
        }
    }
}

impl Drop for CurrentLayout {
    fn drop(&mut self) {
        // SAFETY: `source` is the +1 reference `copy` took.
        unsafe { CFRelease(self.source as *mut c_void) }
    }
}

impl LayoutKeys for CurrentLayout {
    fn typed(&self, scancode: u16, mods: u32) -> Option<char> {
        let mut string = [0; 4];
        let mut len = 0;
        let mut dead_keys = 0;
        // SAFETY: `layout` is the live table `source` owns; the out-pointers name locals whose.
        let status = unsafe {
            ffi::UCKeyTranslate(
                self.layout,
                scancode,
                ffi::kUCKeyActionDown,
                mods,
                self.keyboard_type,
                ffi::kUCKeyTranslateNoDeadKeysMask,
                &mut dead_keys,
                string.len() as ffi::UniCharCount,
                &mut len,
                string.as_mut_ptr(),
            )
        };
        if status != 0 {
            return None;
        }
        let mut chars = char::decode_utf16(string[..len as usize].iter().copied());
        match (chars.next(), chars.next()) {
            (Some(Ok(c)), None) => Some(c),
            _ => None,
        }
    }
}

/// [`option_only_ascii`] on the selected keyboard layout, for the key `key` names.
pub(crate) fn option_only_character(key: PhysicalKey, shift: bool) -> Option<char> {
    let scancode = (0..128u16).find(|&code| scancode_to_physicalkey(u32::from(code)) == key)?;
    option_only_ascii(&CurrentLayout::copy()?, scancode, shift)
}

fn get_logical_key_char(ns_event: Id, modifierless_chars: &str) -> Key {
    let string = autoreleasepool(|_| {
        // SAFETY: `ns_event` is the live `NSEvent` the caller borrows for the whole of this call.
        unsafe { seam::nsstring_to_rust(send_id(ns_event, sel!(charactersIgnoringModifiers))) }
    });
    if string.is_empty() {
        let first_char = modifierless_chars.chars().next();
        return Key::Dead(first_char);
    }
    Key::Character(SmolStr::new(string))
}

/// Create `KeyEvent` for the given `NSEvent`.
pub(crate) fn create_key_event(ns_event: Id, is_press: bool, is_repeat: bool) -> KeyEvent {
    use ElementState::{Pressed, Released};
    let state = if is_press { Pressed } else { Released };

    // SAFETY: `ns_event` is live for the call; `-keyCode` encodes `S16@0:8`, an UNSIGNED short.
    let scancode = unsafe { send_u16(ns_event, sel!(keyCode)) };
    let mut physical_key = scancode_to_physicalkey(scancode as u32);

    // SAFETY: as above; `-characters` answers a +0 autoreleased `NSString`.
    let characters =
        autoreleasepool(|_| unsafe { seam::nsstring_to_rust(send_id(ns_event, sel!(characters))) });
    let text_with_all_modifiers = if characters.is_empty() {
        None
    } else {
        if matches!(physical_key, PhysicalKey::Unidentified(_)) {
            // The key may be one of the funky function keys
            physical_key = extra_function_key_to_code(scancode, &characters);
        }
        Some(SmolStr::new(characters))
    };

    let key_from_code = code_to_key(physical_key, scancode);
    let (logical_key, key_without_modifiers) = if matches!(key_from_code, Key::Unidentified(_)) {
        let key_without_modifiers = get_modifierless_char(scancode);

        // SAFETY: as above; `-modifierFlags` encodes `Q16@0:8`.
        let modifiers = unsafe { send_usize(ns_event, sel!(modifierFlags)) };
        let has_ctrl = has_flag(modifiers, consts::NS_EVENT_MODIFIER_FLAG_CONTROL);
        let has_cmd = has_flag(modifiers, consts::NS_EVENT_MODIFIER_FLAG_COMMAND);

        let logical_key = match text_with_all_modifiers.as_ref() {
            Some(text) if !has_ctrl && !has_cmd => {
                // Character heeding both SHIFT and ALT.
                Key::Character(text.clone())
            },

            _ => match key_without_modifiers.as_ref() {
                Key::Character(ch) => get_logical_key_char(ns_event, ch),

                _ => key_without_modifiers.clone(),
            },
        };

        (logical_key, key_without_modifiers)
    } else {
        (key_from_code.clone(), key_from_code)
    };

    let text = if is_press { logical_key.to_text().map(SmolStr::new) } else { None };

    let location = code_to_location(physical_key);

    KeyEvent {
        location,
        logical_key,
        physical_key,
        repeat: is_repeat,
        state,
        text,
        platform_specific: KeyEventExtra { text_with_all_modifiers, key_without_modifiers },
    }
}

pub fn code_to_key(key: PhysicalKey, scancode: u16) -> Key {
    let code = match key {
        PhysicalKey::Code(code) => code,
        PhysicalKey::Unidentified(code) => return Key::Unidentified(code.into()),
    };

    Key::Named(match code {
        KeyCode::Enter => NamedKey::Enter,
        KeyCode::Tab => NamedKey::Tab,
        KeyCode::Space => NamedKey::Space,
        KeyCode::Backspace => NamedKey::Backspace,
        KeyCode::Escape => NamedKey::Escape,
        KeyCode::SuperRight => NamedKey::Super,
        KeyCode::SuperLeft => NamedKey::Super,
        KeyCode::ShiftLeft => NamedKey::Shift,
        KeyCode::AltLeft => NamedKey::Alt,
        KeyCode::ControlLeft => NamedKey::Control,
        KeyCode::ShiftRight => NamedKey::Shift,
        KeyCode::AltRight => NamedKey::Alt,
        KeyCode::ControlRight => NamedKey::Control,

        KeyCode::NumLock => NamedKey::NumLock,
        KeyCode::AudioVolumeUp => NamedKey::AudioVolumeUp,
        KeyCode::AudioVolumeDown => NamedKey::AudioVolumeDown,

        KeyCode::NumpadEnter => NamedKey::Enter,

        KeyCode::F1 => NamedKey::F1,
        KeyCode::F2 => NamedKey::F2,
        KeyCode::F3 => NamedKey::F3,
        KeyCode::F4 => NamedKey::F4,
        KeyCode::F5 => NamedKey::F5,
        KeyCode::F6 => NamedKey::F6,
        KeyCode::F7 => NamedKey::F7,
        KeyCode::F8 => NamedKey::F8,
        KeyCode::F9 => NamedKey::F9,
        KeyCode::F10 => NamedKey::F10,
        KeyCode::F11 => NamedKey::F11,
        KeyCode::F12 => NamedKey::F12,
        KeyCode::F13 => NamedKey::F13,
        KeyCode::F14 => NamedKey::F14,
        KeyCode::F15 => NamedKey::F15,
        KeyCode::F16 => NamedKey::F16,
        KeyCode::F17 => NamedKey::F17,
        KeyCode::F18 => NamedKey::F18,
        KeyCode::F19 => NamedKey::F19,
        KeyCode::F20 => NamedKey::F20,

        KeyCode::Insert => NamedKey::Insert,
        KeyCode::Home => NamedKey::Home,
        KeyCode::PageUp => NamedKey::PageUp,
        KeyCode::Delete => NamedKey::Delete,
        KeyCode::End => NamedKey::End,
        KeyCode::PageDown => NamedKey::PageDown,
        KeyCode::ArrowLeft => NamedKey::ArrowLeft,
        KeyCode::ArrowRight => NamedKey::ArrowRight,
        KeyCode::ArrowDown => NamedKey::ArrowDown,
        KeyCode::ArrowUp => NamedKey::ArrowUp,
        _ => return Key::Unidentified(NativeKey::MacOS(scancode)),
    })
}

pub const fn code_to_location(key: PhysicalKey) -> KeyLocation {
    let code = match key {
        PhysicalKey::Code(code) => code,
        PhysicalKey::Unidentified(_) => return KeyLocation::Standard,
    };

    match code {
        KeyCode::SuperRight => KeyLocation::Right,
        KeyCode::SuperLeft => KeyLocation::Left,
        KeyCode::ShiftLeft => KeyLocation::Left,
        KeyCode::AltLeft => KeyLocation::Left,
        KeyCode::ControlLeft => KeyLocation::Left,
        KeyCode::ShiftRight => KeyLocation::Right,
        KeyCode::AltRight => KeyLocation::Right,
        KeyCode::ControlRight => KeyLocation::Right,

        KeyCode::NumLock => KeyLocation::Numpad,
        KeyCode::NumpadDecimal => KeyLocation::Numpad,
        KeyCode::NumpadMultiply => KeyLocation::Numpad,
        KeyCode::NumpadAdd => KeyLocation::Numpad,
        KeyCode::NumpadDivide => KeyLocation::Numpad,
        KeyCode::NumpadEnter => KeyLocation::Numpad,
        KeyCode::NumpadSubtract => KeyLocation::Numpad,
        KeyCode::NumpadEqual => KeyLocation::Numpad,
        KeyCode::Numpad0 => KeyLocation::Numpad,
        KeyCode::Numpad1 => KeyLocation::Numpad,
        KeyCode::Numpad2 => KeyLocation::Numpad,
        KeyCode::Numpad3 => KeyLocation::Numpad,
        KeyCode::Numpad4 => KeyLocation::Numpad,
        KeyCode::Numpad5 => KeyLocation::Numpad,
        KeyCode::Numpad6 => KeyLocation::Numpad,
        KeyCode::Numpad7 => KeyLocation::Numpad,
        KeyCode::Numpad8 => KeyLocation::Numpad,
        KeyCode::Numpad9 => KeyLocation::Numpad,

        _ => KeyLocation::Standard,
    }
}

pub fn extra_function_key_to_code(scancode: u16, string: &str) -> PhysicalKey {
    if let Some(ch) = string.encode_utf16().next() {
        match ch {
            0xf718 => PhysicalKey::Code(KeyCode::F21),
            0xf719 => PhysicalKey::Code(KeyCode::F22),
            0xf71a => PhysicalKey::Code(KeyCode::F23),
            0xf71b => PhysicalKey::Code(KeyCode::F24),
            _ => PhysicalKey::Unidentified(NativeKeyCode::MacOS(scancode)),
        }
    } else {
        PhysicalKey::Unidentified(NativeKeyCode::MacOS(scancode))
    }
}

const NX_DEVICELCTLKEYMASK: usize = 0x0000_0001;
const NX_DEVICELSHIFTKEYMASK: usize = 0x0000_0002;
const NX_DEVICERSHIFTKEYMASK: usize = 0x0000_0004;
const NX_DEVICELCMDKEYMASK: usize = 0x0000_0008;
const NX_DEVICERCMDKEYMASK: usize = 0x0000_0010;
const NX_DEVICELALTKEYMASK: usize = 0x0000_0020;
const NX_DEVICERALTKEYMASK: usize = 0x0000_0040;
const NX_DEVICERCTLKEYMASK: usize = 0x0000_2000;

/// LOCAL PATCH (aterm): `NSEventModifierFlags::contains`, spelled out.
#[inline]
const fn has_flag(flags: usize, mask: usize) -> bool {
    flags & mask == mask
}

/// Whether `option_as_alt` makes the Option a key event's `-modifierFlags` hold an Alt.
pub(super) const fn option_is_alt(option_as_alt: OptionAsAlt, flags: usize) -> bool {
    let option = match option_as_alt {
        OptionAsAlt::OnlyLeft => has_flag(flags, NX_DEVICELALTKEYMASK),
        OptionAsAlt::OnlyRight => has_flag(flags, NX_DEVICERALTKEYMASK),
        OptionAsAlt::Both => has_flag(flags, consts::NS_EVENT_MODIFIER_FLAG_OPTION),
        OptionAsAlt::None => false,
    };
    option
        && !has_flag(flags, consts::NS_EVENT_MODIFIER_FLAG_CONTROL)
        && !has_flag(flags, consts::NS_EVENT_MODIFIER_FLAG_COMMAND)
}

pub(super) fn event_mods(event: Id) -> Modifiers {
    // SAFETY: `event` is the live `NSEvent` the caller borrows for this call.
    let flags = unsafe { send_usize(event, sel!(modifierFlags)) };
    let mut state = ModifiersState::empty();
    let mut pressed_mods = ModifiersKeys::empty();

    state.set(ModifiersState::SHIFT, has_flag(flags, consts::NS_EVENT_MODIFIER_FLAG_SHIFT));
    pressed_mods.set(ModifiersKeys::LSHIFT, has_flag(flags, NX_DEVICELSHIFTKEYMASK));
    pressed_mods.set(ModifiersKeys::RSHIFT, has_flag(flags, NX_DEVICERSHIFTKEYMASK));

    state.set(ModifiersState::CONTROL, has_flag(flags, consts::NS_EVENT_MODIFIER_FLAG_CONTROL));
    pressed_mods.set(ModifiersKeys::LCONTROL, has_flag(flags, NX_DEVICELCTLKEYMASK));
    pressed_mods.set(ModifiersKeys::RCONTROL, has_flag(flags, NX_DEVICERCTLKEYMASK));

    state.set(ModifiersState::ALT, has_flag(flags, consts::NS_EVENT_MODIFIER_FLAG_OPTION));
    pressed_mods.set(ModifiersKeys::LALT, has_flag(flags, NX_DEVICELALTKEYMASK));
    pressed_mods.set(ModifiersKeys::RALT, has_flag(flags, NX_DEVICERALTKEYMASK));

    state.set(ModifiersState::SUPER, has_flag(flags, consts::NS_EVENT_MODIFIER_FLAG_COMMAND));
    pressed_mods.set(ModifiersKeys::LSUPER, has_flag(flags, NX_DEVICELCMDKEYMASK));
    pressed_mods.set(ModifiersKeys::RSUPER, has_flag(flags, NX_DEVICERCMDKEYMASK));

    Modifiers { state, pressed_mods }
}

pub(super) fn dummy_event() -> Option<Obj> {
    // SAFETY: the receiver is the live `NSEvent` class object and the argument list is.
    let raw = unsafe {
        send_id_usize_point_usize_f64_isize_id_i16_isize_isize(
            class(c"NSEvent").as_id(),
            sel!(
                otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:
            ),
            consts::NS_EVENT_TYPE_APPLICATION_DEFINED,
            CGPoint { x: 0.0, y: 0.0 },
            0,
            0.0,
            0,
            Id::NIL,
            consts::NS_EVENT_SUBTYPE_WINDOW_EXPOSED,
            0,
            0,
        )
    };
    // SAFETY: `raw` is nil or the live autoreleased event just constructed; `Obj::retain` answers.
    unsafe { Obj::retain(raw) }
}

pub(crate) const fn scancode_to_physicalkey(scancode: u32) -> PhysicalKey {
    PhysicalKey::Code(match scancode {
        0x00 => KeyCode::KeyA,
        0x01 => KeyCode::KeyS,
        0x02 => KeyCode::KeyD,
        0x03 => KeyCode::KeyF,
        0x04 => KeyCode::KeyH,
        0x05 => KeyCode::KeyG,
        0x06 => KeyCode::KeyZ,
        0x07 => KeyCode::KeyX,
        0x08 => KeyCode::KeyC,
        0x09 => KeyCode::KeyV,
        0x0b => KeyCode::KeyB,
        0x0c => KeyCode::KeyQ,
        0x0d => KeyCode::KeyW,
        0x0e => KeyCode::KeyE,
        0x0f => KeyCode::KeyR,
        0x10 => KeyCode::KeyY,
        0x11 => KeyCode::KeyT,
        0x12 => KeyCode::Digit1,
        0x13 => KeyCode::Digit2,
        0x14 => KeyCode::Digit3,
        0x15 => KeyCode::Digit4,
        0x16 => KeyCode::Digit6,
        0x17 => KeyCode::Digit5,
        0x18 => KeyCode::Equal,
        0x19 => KeyCode::Digit9,
        0x1a => KeyCode::Digit7,
        0x1b => KeyCode::Minus,
        0x1c => KeyCode::Digit8,
        0x1d => KeyCode::Digit0,
        0x1e => KeyCode::BracketRight,
        0x1f => KeyCode::KeyO,
        0x20 => KeyCode::KeyU,
        0x21 => KeyCode::BracketLeft,
        0x22 => KeyCode::KeyI,
        0x23 => KeyCode::KeyP,
        0x24 => KeyCode::Enter,
        0x25 => KeyCode::KeyL,
        0x26 => KeyCode::KeyJ,
        0x27 => KeyCode::Quote,
        0x28 => KeyCode::KeyK,
        0x29 => KeyCode::Semicolon,
        0x2a => KeyCode::Backslash,
        0x2b => KeyCode::Comma,
        0x2c => KeyCode::Slash,
        0x2d => KeyCode::KeyN,
        0x2e => KeyCode::KeyM,
        0x2f => KeyCode::Period,
        0x30 => KeyCode::Tab,
        0x31 => KeyCode::Space,
        0x32 => KeyCode::Backquote,
        0x33 => KeyCode::Backspace,
        0x35 => KeyCode::Escape,
        0x36 => KeyCode::SuperRight,
        0x37 => KeyCode::SuperLeft,
        0x38 => KeyCode::ShiftLeft,
        0x39 => KeyCode::CapsLock,
        0x3a => KeyCode::AltLeft,
        0x3b => KeyCode::ControlLeft,
        0x3c => KeyCode::ShiftRight,
        0x3d => KeyCode::AltRight,
        0x3e => KeyCode::ControlRight,
        0x3f => KeyCode::Fn,
        0x40 => KeyCode::F17,
        0x41 => KeyCode::NumpadDecimal,
        0x43 => KeyCode::NumpadMultiply,
        0x45 => KeyCode::NumpadAdd,
        0x47 => KeyCode::NumLock,

        0x49 => KeyCode::AudioVolumeUp,
        0x4a => KeyCode::AudioVolumeDown,
        0x4b => KeyCode::NumpadDivide,
        0x4c => KeyCode::NumpadEnter,
        0x4e => KeyCode::NumpadSubtract,
        0x4f => KeyCode::F18,
        0x50 => KeyCode::F19,
        0x51 => KeyCode::NumpadEqual,
        0x52 => KeyCode::Numpad0,
        0x53 => KeyCode::Numpad1,
        0x54 => KeyCode::Numpad2,
        0x55 => KeyCode::Numpad3,
        0x56 => KeyCode::Numpad4,
        0x57 => KeyCode::Numpad5,
        0x58 => KeyCode::Numpad6,
        0x59 => KeyCode::Numpad7,
        0x5a => KeyCode::F20,
        0x5b => KeyCode::Numpad8,
        0x5c => KeyCode::Numpad9,
        0x5d => KeyCode::IntlYen,
        0x60 => KeyCode::F5,
        0x61 => KeyCode::F6,
        0x62 => KeyCode::F7,
        0x63 => KeyCode::F3,
        0x64 => KeyCode::F8,
        0x65 => KeyCode::F9,
        0x67 => KeyCode::F11,
        0x69 => KeyCode::F13,
        0x6a => KeyCode::F16,
        0x6b => KeyCode::F14,
        0x6d => KeyCode::F10,
        0x6f => KeyCode::F12,
        0x71 => KeyCode::F15,
        0x72 => KeyCode::Insert,
        0x73 => KeyCode::Home,
        0x74 => KeyCode::PageUp,
        0x75 => KeyCode::Delete,
        0x76 => KeyCode::F4,
        0x77 => KeyCode::End,
        0x78 => KeyCode::F2,
        0x79 => KeyCode::PageDown,
        0x7a => KeyCode::F1,
        0x7b => KeyCode::ArrowLeft,
        0x7c => KeyCode::ArrowRight,
        0x7d => KeyCode::ArrowDown,
        0x7e => KeyCode::ArrowUp,

        0xa => KeyCode::Backquote,
        _ => return PhysicalKey::Unidentified(NativeKeyCode::MacOS(scancode as u16)),
    })
}

#[cfg(test)]
mod option_is_alt_tests {
    use super::consts::{
        NS_EVENT_MODIFIER_FLAG_COMMAND as COMMAND, NS_EVENT_MODIFIER_FLAG_CONTROL as CONTROL,
        NS_EVENT_MODIFIER_FLAG_OPTION as OPTION, NS_EVENT_MODIFIER_FLAG_SHIFT as SHIFT,
    };
    use super::{option_is_alt, NX_DEVICELALTKEYMASK, NX_DEVICERALTKEYMASK};
    use crate::platform::macos::OptionAsAlt;

    const LEFT: usize = OPTION | NX_DEVICELALTKEYMASK;
    const RIGHT: usize = OPTION | NX_DEVICERALTKEYMASK;

    /// Which held Option a setting makes an Alt, and that a chord with Control or Command
    /// keeps AppKit's own reading (its text is already read without Option).
    #[test]
    fn an_option_is_alt_only_where_the_setting_names_it_and_never_under_control_or_command() {
        use OptionAsAlt::{Both, None, OnlyLeft, OnlyRight};
        let rows: &[(OptionAsAlt, usize, bool)] = &[
            (None, LEFT, false),
            (None, RIGHT, false),
            (Both, LEFT, true),
            (Both, RIGHT, true),
            (Both, LEFT | SHIFT, true),
            (Both, SHIFT, false),
            (Both, 0, false),
            (OnlyLeft, LEFT, true),
            (OnlyLeft, RIGHT, false),
            (OnlyRight, RIGHT, true),
            (OnlyRight, LEFT, false),
            (Both, LEFT | CONTROL, false),
            (Both, RIGHT | COMMAND, false),
            (OnlyLeft, LEFT | CONTROL | SHIFT, false),
        ];
        for &(setting, flags, alt) in rows {
            assert_eq!(option_is_alt(setting, flags), alt, "{setting:?} flags {flags:#x}");
        }
    }
}

#[cfg(test)]
mod option_only_tests {
    use std::collections::HashMap;

    use super::{option_only_ascii, LayoutKeys, UC_OPTION, UC_SHIFT};

    /// A layout as a table of (key code, modifier byte) to character.
    struct Table(HashMap<(u16, u32), char>);

    impl LayoutKeys for Table {
        fn typed(&self, scancode: u16, mods: u32) -> Option<char> {
            self.0.get(&(scancode, mods)).copied()
        }
    }

    const L: u16 = 37;
    const N: u16 = 45;
    const SIX: u16 = 22;
    const GRAVE: u16 = 50;
    const CIRCUMFLEX: u16 = 10;

    /// The keys these rows press, as the U.S.
    fn us() -> Table {
        Table(HashMap::from([
            ((L, 0), 'l'),
            ((L, UC_SHIFT), 'L'),
            ((L, UC_OPTION), '¬'),
            ((N, 0), 'n'),
            ((N, UC_OPTION), '˜'),
            ((SIX, 0), '6'),
            ((SIX, UC_SHIFT), '^'),
            ((SIX, UC_OPTION), '§'),
            ((GRAVE, 0), '`'),
            ((GRAVE, UC_SHIFT), '~'),
            ((GRAVE, UC_OPTION), '`'),
        ]))
    }

    /// The same keys as the German layout has them.
    fn german() -> Table {
        Table(HashMap::from([
            ((L, 0), 'l'),
            ((L, UC_OPTION), '@'),
            ((N, 0), 'n'),
            ((N, UC_OPTION), '~'),
            ((SIX, 0), '6'),
            ((SIX, UC_SHIFT), '&'),
            ((SIX, UC_OPTION), ']'),
            ((SIX, UC_OPTION | UC_SHIFT), '^'),
            ((CIRCUMFLEX, 0), '^'),
            ((GRAVE, 0), '<'),
            ((GRAVE, UC_SHIFT), '>'),
            ((GRAVE, UC_OPTION), '≤'),
        ]))
    }

    #[test]
    fn on_the_us_layout_no_option_chord_is_the_only_way_to_a_character() {
        let us = us();
        for (code, shift) in [(L, false), (N, false), (SIX, false), (GRAVE, false), (GRAVE, true)] {
            assert_eq!(option_only_ascii(&us, code, shift), None, "key {code} shift {shift}");
        }
    }

    #[test]
    fn a_german_ascii_character_only_option_reaches_is_that_chords() {
        let de = german();
        assert_eq!(option_only_ascii(&de, L, false), Some('@'));
        assert_eq!(option_only_ascii(&de, SIX, false), Some(']'));
        assert_eq!(option_only_ascii(&de, N, false), Some('~'), "a dead key, by its accent");
    }

    #[test]
    fn a_character_reached_without_option_or_not_ascii_is_none() {
        let de = german();
        assert_eq!(option_only_ascii(&de, SIX, true), None, "^ is the ^ key's, bare");
        assert_eq!(option_only_ascii(&de, GRAVE, false), None, "≤ is not ASCII");
        assert_eq!(option_only_ascii(&de, CIRCUMFLEX, false), None, "no Option character");
    }
}

// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/keyboard.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::AltCode;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_LMENU, VK_MENU, VK_NUMPAD0, VK_NUMPAD2, VK_NUMPAD3, VK_RETURN, VK_SHIFT,
};

#[test]
fn an_alt_code_hands_over_its_character() {
    let mut code = AltCode::default();
    code.key_down(VK_MENU, true, false);
    for digit in [VK_NUMPAD0, VK_NUMPAD2, VK_NUMPAD3, VK_NUMPAD3] {
        code.key_down(digit, true, false);
    }
    assert!(code.push(0x00e9));
    assert_eq!(code.finish().as_deref(), Some("é"));
    assert!(!code.push(0x00e9), "one character per code");
}

#[test]
fn a_surrogate_pair_is_one_character() {
    let mut code = AltCode::default();
    code.key_down(VK_NUMPAD2, true, false);
    assert!(code.push(0xd83d));
    assert!(code.push(0xde00));
    assert_eq!(code.finish().as_deref(), Some("😀"));
}

#[test]
fn only_alt_and_a_keypad_digit_arm_it() {
    let mut code = AltCode::default();
    code.key_down(VK_NUMPAD2, false, false);
    assert!(!code.push(u16::from(b'2')));
    code.key_down(VK_NUMPAD2, true, true);
    assert!(!code.push(u16::from(b'2')), "Ctrl+Alt is AltGr");
    code.key_down(VK_NUMPAD2, true, false);
    code.key_down(VK_SHIFT, true, false);
    code.key_down(VK_LMENU, true, false);
    assert!(code.push(0x00e9), "modifiers leave it armed");
    let _ = code.finish();
    code.key_down(VK_NUMPAD2, true, false);
    code.key_down(VK_RETURN, true, false);
    assert!(!code.push(u16::from(b'\r')), "another key disarms it");
}

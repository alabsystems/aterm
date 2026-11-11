// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/window.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::{attention_flash, UserAttentionType};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FLASHW_ALL, FLASHW_STOP, FLASHW_TIMERNOFG, FLASHW_TRAY,
};

#[test]
fn a_bell_flashes_the_taskbar_button_until_the_window_comes_forward() {
    let hwnd = 0x1234 as _;
    let bell = attention_flash(hwnd, Some(UserAttentionType::Informational));
    assert_eq!((bell.dwFlags, bell.uCount, bell.dwTimeout), (FLASHW_TRAY | FLASHW_TIMERNOFG, 0, 0));
    assert_eq!(bell.hwnd, hwnd);
    #[cfg(target_pointer_width = "64")]
    assert_eq!(bell.cbSize, 32);
    let critical = attention_flash(hwnd, Some(UserAttentionType::Critical));
    assert_eq!((critical.dwFlags, critical.uCount), (FLASHW_ALL | FLASHW_TIMERNOFG, u32::MAX));
    assert_eq!(attention_flash(hwnd, None).dwFlags, FLASHW_STOP);
}

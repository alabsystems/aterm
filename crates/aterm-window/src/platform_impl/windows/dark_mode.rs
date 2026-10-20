// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/dark_mode.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by Andrew Yates for the aterm project in 2026; see the repository NOTICE.
// Update BOOL imports and module-handle null checks for windows-sys 0.61.
// Theme the window's popup menus with its caption (`set_menu_theme`), re-asserted as
// a window activates (`reassert_menu_theme`).

/// This is a simple implementation of support for Windows Dark Mode,
/// which is inspired by the solution in https://github.com/ysc3839/win32-darkmode
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use std::{ffi::c_void, ptr};

use crate::utils::Lazy;
use windows_sys::core::BOOL;
use windows_sys::core::PCSTR;
use windows_sys::Win32::Foundation::{HWND, NTSTATUS, S_OK};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTA};
use windows_sys::Win32::UI::Controls::SetWindowTheme;
use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoA, SPI_GETHIGHCONTRAST};

use crate::window::Theme;

use super::util;

static WIN10_BUILD_VERSION: Lazy<Option<u32>> = Lazy::new(|| {
    type RtlGetVersion = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> NTSTATUS;
    let handle = get_function!("ntdll.dll", RtlGetVersion);

    if let Some(rtl_get_version) = handle {
        unsafe {
            let mut vi = OSVERSIONINFOW {
                dwOSVersionInfoSize: 0,
                dwMajorVersion: 0,
                dwMinorVersion: 0,
                dwBuildNumber: 0,
                dwPlatformId: 0,
                szCSDVersion: [0; 128],
            };

            let status = (rtl_get_version)(&mut vi);

            if status >= 0 && vi.dwMajorVersion == 10 && vi.dwMinorVersion == 0 {
                Some(vi.dwBuildNumber)
            } else {
                None
            }
        }
    } else {
        None
    }
});

static DARK_MODE_SUPPORTED: Lazy<bool> = Lazy::new(|| {
    // We won't try to do anything for windows versions < 17763
    // (Windows 10 October 2018 update)
    match *WIN10_BUILD_VERSION {
        Some(v) => v >= 17763,
        None => false,
    }
});

static DARK_THEME_NAME: Lazy<Vec<u16>> = Lazy::new(|| util::encode_wide("DarkMode_Explorer"));
static LIGHT_THEME_NAME: Lazy<Vec<u16>> = Lazy::new(|| util::encode_wide(""));

/// Attempt to set a theme on a window, if necessary.
/// Returns the theme that was picked
pub fn try_theme(hwnd: HWND, preferred_theme: Option<Theme>) -> Theme {
    if *DARK_MODE_SUPPORTED {
        let is_dark_mode = match preferred_theme {
            Some(theme) => theme == Theme::Dark,
            None => should_use_dark_mode(),
        };

        let theme = if is_dark_mode { Theme::Dark } else { Theme::Light };
        let theme_name = match theme {
            Theme::Dark => DARK_THEME_NAME.as_ptr(),
            Theme::Light => LIGHT_THEME_NAME.as_ptr(),
        };

        let status = unsafe { SetWindowTheme(hwnd, theme_name, ptr::null()) };

        if status == S_OK && set_dark_mode_for_window(hwnd, is_dark_mode) {
            set_menu_theme(hwnd, preferred_theme, is_dark_mode);
            return theme;
        }
    }

    Theme::Light
}

/// uxtheme's `PreferredAppMode` (ordinal 135, Windows 10 1903 and later).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
enum PreferredAppMode {
    /// Menus follow the OS "app mode" setting.
    AllowDark = 1,
    /// Menus are dark whatever the OS setting.
    ForceDark = 2,
    /// Menus are light whatever the OS setting.
    ForceLight = 3,
}

/// The app mode that draws a window's popup menus on the same side as its
/// caption: an explicit theme forces its side, so a dark caption over a light
/// desktop opens a dark system menu; a window that follows the OS lets the OS
/// decide.
fn preferred_app_mode(preferred_theme: Option<Theme>) -> PreferredAppMode {
    match preferred_theme {
        Some(Theme::Dark) => PreferredAppMode::ForceDark,
        Some(Theme::Light) => PreferredAppMode::ForceLight,
        None => PreferredAppMode::AllowDark,
    }
}

/// Theme the popup menus a window opens (the system menu, Alt+Space or a
/// right-click on the caption) to match `try_theme`'s answer.
///
/// `SetWindowTheme` and `WCA_USEDARKMODECOLORS` dark the caption but not its
/// menus: Win32 menus follow the process's preferred app mode. So, as Windows
/// Terminal and Explorer do: `AllowDarkModeForWindow` (ordinal 133), then
/// `SetPreferredAppMode` (ordinal 135) and `FlushMenuThemes` (ordinal 136), so a
/// menu Windows already built repaints. The mode is process-wide while each
/// window's caption takes the side of the surface under it, so two windows can
/// want opposite sides and the last to re-theme would set the other's menus: a
/// window re-asserts its own as it activates ([`reassert_menu_theme`]), and a
/// system menu opens only on the active window. Undocumented exports, looked up
/// by ordinal and skipped where absent or before build 18362 (where ordinal 135
/// was `AllowDarkModeForApp(BOOL)`); a miss leaves the menus as the OS draws them.
fn set_menu_theme(hwnd: HWND, preferred_theme: Option<Theme>, is_dark_mode: bool) {
    type AllowDarkModeForWindow = unsafe extern "system" fn(HWND, bool) -> bool;
    type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
    type FlushMenuThemes = unsafe extern "system" fn();

    struct MenuApi {
        allow_window: Option<AllowDarkModeForWindow>,
        set_mode: Option<SetPreferredAppMode>,
        flush: Option<FlushMenuThemes>,
    }

    static MENU_API: Lazy<MenuApi> = Lazy::new(|| unsafe {
        let none = MenuApi { allow_window: None, set_mode: None, flush: None };
        if !matches!(*WIN10_BUILD_VERSION, Some(build) if build >= 18362) {
            return none;
        }
        let module = LoadLibraryA(c"uxtheme.dll".as_ptr().cast());
        if module.is_null() {
            return none;
        }
        let ordinal = |n: usize| GetProcAddress(module, n as PCSTR);
        MenuApi {
            allow_window: ordinal(133).map(|f| std::mem::transmute(f)),
            set_mode: ordinal(135).map(|f| std::mem::transmute(f)),
            flush: ordinal(136).map(|f| std::mem::transmute(f)),
        }
    });

    let api = &*MENU_API;
    let mode = preferred_app_mode(preferred_theme);
    unsafe {
        if let Some(allow_window) = api.allow_window {
            allow_window(hwnd, is_dark_mode);
        }
        if let Some(set_mode) = api.set_mode {
            set_mode(mode as i32);
            MENU_MODE.store(mode as i32, Ordering::Relaxed);
        }
        if let Some(flush) = api.flush {
            flush();
        }
    }
    let mut menus = WINDOW_MENUS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = hwnd as usize;
    match menus.iter_mut().find(|(window, ..)| *window == key) {
        Some(entry) => *entry = (key, preferred_theme, is_dark_mode),
        None => menus.push((key, preferred_theme, is_dark_mode)),
    }
}

/// The app mode [`set_menu_theme`] last set for the process (`0`: none yet).
static MENU_MODE: AtomicI32 = AtomicI32::new(0);

/// Each live window's menu side as [`set_menu_theme`] last set it:
/// `(hwnd, preferred theme, dark)`. A window's `WindowState::preferred_theme` is
/// its creation attribute, not what `set_theme` gave it since, so it is kept here.
static WINDOW_MENUS: Mutex<Vec<(usize, Option<Theme>, bool)>> = Mutex::new(Vec::new());

/// The menu side a window must set again as it activates, if any: its own, as
/// last set (`window`), when another window has moved the process's app mode
/// (`last`) off it since. `None` for a window never themed.
fn menu_side_to_reassert(
    last: i32,
    window: Option<(Option<Theme>, bool)>,
) -> Option<(Option<Theme>, bool)> {
    window.filter(|(preferred, _)| last != preferred_app_mode(*preferred) as i32)
}

/// Re-assert the process-wide menu mode for `hwnd` as it activates
/// (`WM_NCACTIVATE`): its system menu (Alt+Space, a right-click on the caption)
/// opens only while it is active, and another window whose caption sits on the
/// other side may have set the mode since ([`set_menu_theme`]). A mode already
/// right is left alone.
pub fn reassert_menu_theme(hwnd: HWND) {
    if !*DARK_MODE_SUPPORTED {
        return;
    }
    let own = WINDOW_MENUS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(window, ..)| *window == hwnd as usize)
        .map(|&(_, preferred, dark)| (preferred, dark));
    if let Some((preferred, dark)) = menu_side_to_reassert(MENU_MODE.load(Ordering::Relaxed), own) {
        set_menu_theme(hwnd, preferred, dark);
    }
}

/// Drop a destroyed window's menu side (`WM_NCDESTROY`), so a later window
/// given the same handle starts unthemed.
pub fn forget_menu_theme(hwnd: HWND) {
    WINDOW_MENUS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(window, ..)| *window != hwnd as usize);
}

fn set_dark_mode_for_window(hwnd: HWND, is_dark_mode: bool) -> bool {
    // Uses Windows undocumented API SetWindowCompositionAttribute,
    // as seen in win32-darkmode example linked at top of file.

    type SetWindowCompositionAttribute =
        unsafe extern "system" fn(HWND, *mut WINDOWCOMPOSITIONATTRIBDATA) -> BOOL;

    #[allow(clippy::upper_case_acronyms)]
    type WINDOWCOMPOSITIONATTRIB = u32;
    const WCA_USEDARKMODECOLORS: WINDOWCOMPOSITIONATTRIB = 26;

    #[allow(non_snake_case)]
    #[allow(clippy::upper_case_acronyms)]
    #[repr(C)]
    struct WINDOWCOMPOSITIONATTRIBDATA {
        Attrib: WINDOWCOMPOSITIONATTRIB,
        pvData: *mut c_void,
        cbData: usize,
    }

    static SET_WINDOW_COMPOSITION_ATTRIBUTE: Lazy<Option<SetWindowCompositionAttribute>> =
        Lazy::new(|| get_function!("user32.dll", SetWindowCompositionAttribute));

    if let Some(set_window_composition_attribute) = *SET_WINDOW_COMPOSITION_ATTRIBUTE {
        unsafe {
            // SetWindowCompositionAttribute needs a bigbool (i32), not bool.
            let mut is_dark_mode_bigbool = BOOL::from(is_dark_mode);

            let mut data = WINDOWCOMPOSITIONATTRIBDATA {
                Attrib: WCA_USEDARKMODECOLORS,
                pvData: &mut is_dark_mode_bigbool as *mut _ as _,
                cbData: std::mem::size_of_val(&is_dark_mode_bigbool) as _,
            };

            let status = set_window_composition_attribute(hwnd, &mut data);

            status != false.into()
        }
    } else {
        false
    }
}

pub fn should_use_dark_mode() -> bool {
    should_apps_use_dark_mode() && !is_high_contrast()
}

fn should_apps_use_dark_mode() -> bool {
    type ShouldAppsUseDarkMode = unsafe extern "system" fn() -> bool;
    static SHOULD_APPS_USE_DARK_MODE: Lazy<Option<ShouldAppsUseDarkMode>> = Lazy::new(|| unsafe {
        const UXTHEME_SHOULDAPPSUSEDARKMODE_ORDINAL: PCSTR = 132 as PCSTR;

        // We won't try to do anything for windows versions < 17763
        // (Windows 10 October 2018 update)
        if !*DARK_MODE_SUPPORTED {
            return None;
        }

        let module = LoadLibraryA(c"uxtheme.dll".as_ptr().cast());

        if module.is_null() {
            return None;
        }

        let handle = GetProcAddress(module, UXTHEME_SHOULDAPPSUSEDARKMODE_ORDINAL);

        handle.map(|handle| std::mem::transmute(handle))
    });

    SHOULD_APPS_USE_DARK_MODE
        .map(|should_apps_use_dark_mode| unsafe { (should_apps_use_dark_mode)() })
        .unwrap_or(false)
}

fn is_high_contrast() -> bool {
    let mut hc = HIGHCONTRASTA { cbSize: 0, dwFlags: 0, lpszDefaultScheme: ptr::null_mut() };

    let ok = unsafe {
        SystemParametersInfoA(
            SPI_GETHIGHCONTRAST,
            std::mem::size_of_val(&hc) as _,
            &mut hc as *mut _ as _,
            0,
        )
    };

    ok != false.into() && util::has_flag(hc.dwFlags, HCF_HIGHCONTRASTON)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two windows on opposite sides: the one that activates sets its own side
    /// again only when the other moved the process's mode off it. Negative
    /// controls: a window on the side last set, and one never themed, change
    /// nothing.
    #[test]
    fn an_activating_window_takes_its_menus_back_only_when_they_moved() {
        let light = PreferredAppMode::ForceLight as i32;
        let dark_window = Some((Some(Theme::Dark), true));
        assert_eq!(menu_side_to_reassert(light, dark_window), dark_window);
        let light_window = Some((Some(Theme::Light), false));
        assert_eq!(menu_side_to_reassert(light, light_window), None);
        assert_eq!(menu_side_to_reassert(0, light_window), light_window, "none set yet");
        assert_eq!(menu_side_to_reassert(light, None), None, "never themed");
        let follows_os = Some((None, false));
        assert_eq!(menu_side_to_reassert(light, follows_os), follows_os);
        let allow = PreferredAppMode::AllowDark as i32;
        assert_eq!(menu_side_to_reassert(allow, follows_os), None);
    }

    #[test]
    fn menus_take_the_side_the_caption_was_given() {
        assert_eq!(preferred_app_mode(Some(Theme::Dark)), PreferredAppMode::ForceDark);
        assert_eq!(preferred_app_mode(Some(Theme::Light)), PreferredAppMode::ForceLight);
        assert_eq!(preferred_app_mode(None), PreferredAppMode::AllowDark);
    }
}

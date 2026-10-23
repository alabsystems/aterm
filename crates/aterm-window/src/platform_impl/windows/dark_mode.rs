// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/dark_mode.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by Andrew Yates for the aterm project in 2026; see the repository NOTICE.

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
    // Windows 10 build 17763 (1809) introduced dark mode.
    match *WIN10_BUILD_VERSION {
        Some(v) => v >= 17763,
        None => false,
    }
});

static DARK_THEME_NAME: Lazy<Vec<u16>> = Lazy::new(|| util::encode_wide("DarkMode_Explorer"));
static LIGHT_THEME_NAME: Lazy<Vec<u16>> = Lazy::new(|| util::encode_wide(""));

/// Applies the dark or light theme to `hwnd` and returns the selected theme.
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

fn preferred_app_mode(preferred_theme: Option<Theme>) -> PreferredAppMode {
    match preferred_theme {
        Some(Theme::Dark) => PreferredAppMode::ForceDark,
        Some(Theme::Light) => PreferredAppMode::ForceLight,
        None => PreferredAppMode::AllowDark,
    }
}

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

static MENU_MODE: AtomicI32 = AtomicI32::new(0);

static WINDOW_MENUS: Mutex<Vec<(usize, Option<Theme>, bool)>> = Mutex::new(Vec::new());

fn menu_side_to_reassert(
    last: i32,
    window: Option<(Option<Theme>, bool)>,
) -> Option<(Option<Theme>, bool)> {
    window.filter(|(preferred, _)| last != preferred_app_mode(*preferred) as i32)
}

/// Re-asserts the process-wide menu theme for `hwnd` when it activates (`WM_NCACTIVATE`).
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

/// Removes `hwnd`'s recorded menu theme on window destruction (`WM_NCDESTROY`).
pub fn forget_menu_theme(hwnd: HWND) {
    WINDOW_MENUS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(window, ..)| *window != hwnd as usize);
}

fn set_dark_mode_for_window(hwnd: HWND, is_dark_mode: bool) -> bool {
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

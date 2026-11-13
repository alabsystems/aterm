// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Windows application runtime ([`AppRtWindows`]) decorating winit `HWND`s with DWM chrome and WNDPROC hooks.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use aterm_window::event_loop::EventLoopProxy;
use aterm_window::window::Window;
use core::ffi::c_void;

use crate::app_config::{BackgroundMaterial, WindowTheme};
use crate::notify::NotifyMsg;
use crate::platform::{
    AppRt, ReduceMotionObserver, SurfaceColorspace, ToolbarTabsModel, window_theme_to_winit,
};
use crate::{Wake, WindowId, menu, toolbar};

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(hwnd: isize, attr: u32, pv: *const c_void, cb: u32) -> i32;
    fn DwmGetWindowAttribute(hwnd: isize, attr: u32, pv: *mut c_void, cb: u32) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SystemParametersInfoW(action: u32, ui_param: u32, pv_param: *mut c_void, fw_ini: u32)
    -> i32;
    fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(hwnd: isize, hdc: isize) -> i32;
    fn PrintWindow(hwnd: isize, hdc: isize, flags: u32) -> i32;
    fn SetClassLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn CallWindowProcW(prev: isize, hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn RedrawWindow(hwnd: isize, rect: *const Rect, region: isize, flags: u32) -> i32;
    fn GetSysColor(index: i32) -> u32;
    fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
    fn IsIconic(hwnd: isize) -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
}

const WM_SYSCOMMAND: u32 = 0x0112;
const SC_KEYMENU: usize = 0xF100;

/// Posts `WM_SYSCOMMAND` (`SC_KEYMENU`) to open `window`'s native Win32 system menu.
pub(crate) fn popup_system_menu(window: &Window) -> bool {
    let Some(hwnd) = hwnd_of(window) else {
        return false;
    };
    // SAFETY: `PostMessageW` queues `WM_SYSCOMMAND` on a live process-owned `HWND` with no pointers.
    unsafe { PostMessageW(hwnd, WM_SYSCOMMAND, SC_KEYMENU, isize::from(b' ')) != 0 }
}

#[repr(C)]
struct MonitorInfo {
    size: u32,
    monitor: Rect,
    work: Rect,
    flags: u32,
}

const MONITOR_DEFAULTTONEAREST: u32 = 0x0000_0002;
const SW_RESTORE: i32 = 9;

fn work_area_pts_from_rect(work: &Rect, scale: f64) -> Option<crate::app_window::WorkAreaPts> {
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let w = f64::from(work.right.checked_sub(work.left)?) / scale;
    let h = f64::from(work.bottom.checked_sub(work.top)?) / scale;
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    Some(crate::app_window::WorkAreaPts {
        x: f64::from(work.left) / scale,
        y: f64::from(work.top) / scale,
        w,
        h,
    })
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        key: isize,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        ty: *mut u32,
        data: *mut c_void,
        cb: *mut u32,
    ) -> i32;
}

const GCLP_HBRBACKGROUND: i32 = -10;

const RDW_INVALIDATE: u32 = 0x0001;
const RDW_ERASE: u32 = 0x0004;
const RDW_ERASENOW: u32 = 0x0200;

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateSolidBrush(color: u32) -> isize;
    fn CreateCompatibleDC(hdc: isize) -> isize;
    fn CreateCompatibleBitmap(hdc: isize, w: i32, h: i32) -> isize;
    fn SelectObject(hdc: isize, obj: isize) -> isize;
    fn DeleteObject(obj: isize) -> i32;
    fn DeleteDC(hdc: isize) -> i32;
    fn GetDIBits(
        hdc: isize,
        hbmp: isize,
        start: u32,
        lines: u32,
        bits: *mut c_void,
        bmi: *mut BitmapInfoHeader,
        usage: u32,
    ) -> i32;
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct HighContrastW {
    size: u32,
    flags: u32,
    default_scheme: *mut u16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_ppm: i32,
    y_ppm: i32,
    clr_used: u32,
    clr_important: u32,
}

const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;
const DIB_RGB_COLORS: u32 = 0;

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWCP_ROUND: u32 = 2;
const DWMWA_CAPTION_COLOR: u32 = 35;
const DWMWA_TEXT_COLOR: u32 = 36;
const DWMWA_COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
const DWMSBT_NONE: u32 = 1;
const DWMSBT_MAINWINDOW: u32 = 2;
const DWMSBT_TRANSIENTWINDOW: u32 = 3;
const DWMSBT_TABBEDWINDOW: u32 = 4;

const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
const SPI_GETHIGHCONTRAST: u32 = 0x0042;
const HCF_HIGHCONTRASTON: u32 = 0x0000_0001;

const COLOR_WINDOW: i32 = 5;
const COLOR_WINDOWTEXT: i32 = 8;
const COLOR_HIGHLIGHT: i32 = 13;
const COLOR_HIGHLIGHTTEXT: i32 = 14;
const COLOR_BTNFACE: i32 = 15;

const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as isize;
const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
const RRF_RT_REG_BINARY: u32 = 0x0000_0008;

const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const ENABLE_TRANSPARENCY_VALUE: &str = "EnableTransparency";

const ACCESSIBILITY_KEY: &str = r"Software\Microsoft\Accessibility";
const TEXT_SCALE_FACTOR_VALUE: &str = "TextScaleFactor";

const SPI_GETWHEELSCROLLLINES: u32 = 0x0068;
const WHEEL_PAGESCROLL: u32 = u32::MAX;
const DEFAULT_WHEEL_SCROLL_LINES: u32 = 3;

fn validate_window_capture_transfer(
    print_succeeded: bool,
    copied_lines: i32,
    expected_lines: i32,
) -> Result<(), String> {
    if !print_succeeded {
        return Err("PrintWindow failed".to_string());
    }
    if copied_lines != expected_lines {
        return Err(if copied_lines == 0 {
            "GetDIBits failed".to_string()
        } else {
            format!("GetDIBits copied {copied_lines} of {expected_lines} window-capture scanlines")
        });
    }
    Ok(())
}

/// The Win32 `HWND` (`isize`) of a winit [`Window`], if realized; `None` off its event loop's
/// thread.
pub(crate) fn hwnd_of(window: &Window) -> Option<isize> {
    use aterm_types::native_window::{HasNativeWindow, NativeWindow};
    match window.native_window()? {
        NativeWindow::Win32 { hwnd, .. } => Some(hwnd.as_ptr().addr().cast_signed()),
    }
}

fn colorref_swap(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

fn dwm_set_u32(hwnd: isize, attr: u32, value: u32) {
    // SAFETY: `value` outlives `DwmSetWindowAttribute` and `cb` is its exact byte size.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            attr,
            (&value as *const u32).cast::<c_void>(),
            core::mem::size_of::<u32>() as u32,
        );
    }
}

fn dwm_get_u32(hwnd: isize, attr: u32) -> Option<u32> {
    let mut value: u32 = 0;
    // SAFETY: `DwmGetWindowAttribute` writes into local `u32` `value` with exact byte size `cb`.
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            attr,
            (&mut value as *mut u32).cast::<c_void>(),
            core::mem::size_of::<u32>() as u32,
        )
    };
    if hr == 0 { Some(value) } else { None }
}

fn corner_label(v: u32) -> &'static str {
    match v {
        0 => "default",
        1 => "donotround",
        2 => "round",
        3 => "roundsmall",
        _ => "?",
    }
}

fn backdrop_label(v: u32) -> &'static str {
    match v {
        0 => "auto",
        1 => "none",
        2 => "mica",
        3 => "acrylic",
        4 => "tabbed",
        _ => "?",
    }
}

fn caption_color_label(read: Option<u32>, set: Option<u32>) -> String {
    let color = |v: u32| match v {
        DWMWA_COLOR_DEFAULT => "default".to_string(),
        v => format!("#{:06x}", colorref_swap(v)),
    };
    match (read, set) {
        (Some(v), _) => color(v),
        (None, Some(v)) => format!("{}(set)", color(v)),
        (None, None) => "n/a".to_string(),
    }
}

/// Formats the `chrome` control-verb inspection lines for a live Windows window.
pub(crate) fn read_chrome_lines(
    window: Option<&Window>,
    cfg_theme: WindowTheme,
    cfg_material: BackgroundMaterial,
) -> Vec<String> {
    let want_theme = match cfg_theme {
        WindowTheme::Auto => "auto",
        WindowTheme::Light => "light",
        WindowTheme::Dark => "dark",
    };
    let want_backdrop = backdrop_label(backdrop_for(cfg_material));
    let Some(window) = window else {
        return vec![format!(
            "windows: backend=AppRtWindows cfg_theme={want_theme} cfg_backdrop={want_backdrop} (no window — headless)"
        )];
    };
    let theme = match window.theme() {
        Some(aterm_window::window::Theme::Dark) => "dark",
        Some(aterm_window::window::Theme::Light) => "light",
        None => "auto",
    };
    let Some(hwnd) = hwnd_of(window) else {
        return vec![format!(
            "windows: backend=AppRtWindows cfg_theme={want_theme} theme={theme} (no HWND — not realized)"
        )];
    };
    let dark = match dwm_get_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE) {
        Some(v) => (v != 0).to_string(),
        None => "n/a".to_string(),
    };
    let corner = match dwm_get_u32(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE) {
        Some(v) => format!("{}({v})", corner_label(v)),
        None => "n/a".to_string(),
    };
    let backdrop = match dwm_get_u32(hwnd, DWMWA_SYSTEMBACKDROP_TYPE) {
        Some(v) => format!("{}({v})", backdrop_label(v)),
        None => "n/a".to_string(),
    };
    let written = caption_tint_written(hwnd);
    let caption_color_field =
        |attr: u32, set: Option<u32>| caption_color_label(dwm_get_u32(hwnd, attr), set);
    let caption = caption_color_field(DWMWA_CAPTION_COLOR, written.map(|w| w.0));
    let caption_text = caption_color_field(DWMWA_TEXT_COLOR, written.map(|w| w.1));
    let caption_surface = match caption_surface_of(Some(hwnd)) {
        CHROME_BG_UNKNOWN => "n/a".to_string(),
        surface => format!("#{surface:06x}"),
    };
    let client = if aterm_gpu::visual_swapchain_active() {
        "visual".to_string()
    } else {
        match client_backdrop_decline() {
            Some(reason) => format!("opaque({})", reason.token()),
            None => "opaque".to_string(),
        }
    };
    let high_contrast = high_contrast_active();
    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    let hc_chrome = match resolve_forced_chrome(high_contrast) {
        Some(p) => format!(
            "{}/{}/{}",
            hex(p.btn_face),
            hex(p.window_text),
            hex(p.highlight)
        ),
        None => "off".to_string(),
    };
    let transparency = if transparency_effects_enabled() {
        "on"
    } else {
        "off"
    };
    let reduce_motion = AppRtWindows {
        probe_preferences: true,
    }
    .reduce_motion();
    let text_scale = os_text_scale();
    let (settings_seen, settings_woke) = os_settings_counters();
    vec![format!(
        "windows: backend=AppRtWindows hwnd=0x{hwnd:x} cfg_theme={want_theme} cfg_backdrop={want_backdrop} \
         os_theme={theme} immersive_dark={dark} corner={corner} backdrop={backdrop} client={client} \
         caption_color={caption} caption_text={caption_text} caption_surface={caption_surface} \
         high_contrast={high_contrast} \
         hc_chrome={hc_chrome} transparency={transparency} reduce_motion={reduce_motion} \
         text_scale={text_scale:.2} os_settings={settings_seen}/{settings_woke} aumid={}",
        crate::win32::AUMID
    )]
}

fn backdrop_for(material: BackgroundMaterial) -> u32 {
    match material {
        BackgroundMaterial::None => DWMSBT_NONE,
        BackgroundMaterial::UnderWindow => DWMSBT_MAINWINDOW,
        BackgroundMaterial::Sidebar => DWMSBT_TABBEDWINDOW,
        BackgroundMaterial::Hud => DWMSBT_TRANSIENTWINDOW,
    }
}

const CHROME_BG_UNKNOWN: u32 = u32::MAX;

static CHROME_BG: AtomicU32 = AtomicU32::new(CHROME_BG_UNKNOWN);

static CHROME_POLICY: AtomicU8 = AtomicU8::new(0);

static CHROME_HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);

static CHROME_BACKDROP: AtomicU32 = AtomicU32::new(DWMSBT_NONE);

static CHROME_BACKDROP_REQUESTED: AtomicU32 = AtomicU32::new(DWMSBT_NONE);

static CLIENT_BACKDROP_DECLINE: AtomicU32 = AtomicU32::new(0);

/// Reason the Windows DirectComposition client-area backdrop was declined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientBackdropDecline {
    HdrGlow = 1,
    NoGpu = 2,
    DcompUnavailable = 3,
    GpuLost = 4,
    NotAtLaunch = 5,
}

impl ClientBackdropDecline {
    /// Short diagnostic token formatted inside `client=opaque(<token>)`.
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::HdrGlow => "hdr_glow",
            Self::NoGpu => "no_gpu",
            Self::DcompUnavailable => "dcomp_unavailable",
            Self::GpuLost => "gpu_lost",
            Self::NotAtLaunch => "not_at_launch",
        }
    }

    fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            1 => Self::HdrGlow,
            2 => Self::NoGpu,
            3 => Self::DcompUnavailable,
            4 => Self::GpuLost,
            5 => Self::NotAtLaunch,
            _ => return None,
        })
    }
}

/// Latches the first [`ClientBackdropDecline`] reason for this process.
pub(crate) fn note_client_backdrop_declined(reason: ClientBackdropDecline) {
    let _ = CLIENT_BACKDROP_DECLINE.compare_exchange(
        0,
        reason as u32,
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

/// Returns the latched [`ClientBackdropDecline`] reason, if any.
pub(crate) fn client_backdrop_decline() -> Option<ClientBackdropDecline> {
    ClientBackdropDecline::from_code(CLIENT_BACKDROP_DECLINE.load(Ordering::Relaxed))
}

fn high_contrast_active() -> bool {
    let mut high_contrast = HighContrastW {
        size: std::mem::size_of::<HighContrastW>() as u32,
        flags: 0,
        default_scheme: std::ptr::null_mut(),
    };
    // SAFETY: `SystemParametersInfoW` reads into a properly sized local `HighContrastW` struct.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            high_contrast.size,
            (&mut high_contrast as *mut HighContrastW).cast::<c_void>(),
            0,
        )
    };
    ok != 0 && high_contrast.flags & HCF_HIGHCONTRASTON != 0
}

fn hkcu_dword(subkey: &str, value: &str) -> Option<u32> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |s: &str| {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let subkey = wide(subkey);
    let value = wide(value);
    let mut data: u32 = 0;
    let mut cb: u32 = core::mem::size_of::<u32>() as u32;
    // SAFETY: `RegGetValueW` reads a 4-byte `DWORD` into local `data` using NUL-terminated UTF-16 names.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast::<c_void>(),
            &mut cb,
        )
    };
    (rc == 0).then_some(data)
}

fn hkcu_binary(subkey: &str, value: &str, buf: &mut [u8]) -> Option<usize> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |s: &str| {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let subkey = wide(subkey);
    let value = wide(value);
    let mut cb = u32::try_from(buf.len()).ok()?;
    // SAFETY: `RegGetValueW` reads at most `cb` binary bytes into stack buffer `buf`.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast::<c_void>(),
            &mut cb,
        )
    };
    (rc == 0).then_some(cb as usize)
}

const ACCENT_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
const ACCENT_PALETTE_VALUE: &str = "AccentPalette";
const DWM_KEY: &str = r"Software\Microsoft\Windows\DWM";
const DWM_ACCENT_VALUE: &str = "AccentColor";

fn system_accent_read() -> Option<crate::native_appearance::SystemAccent> {
    accent_from_palette(
        {
            let mut palette = [0u8; 32];
            hkcu_binary(ACCENT_KEY, ACCENT_PALETTE_VALUE, &mut palette)
                .filter(|&n| n >= 20)
                .map(|_| palette)
        },
        hkcu_dword(DWM_KEY, DWM_ACCENT_VALUE),
    )
}

fn accent_from_palette(
    palette: Option<[u8; 32]>,
    dwm_abgr: Option<u32>,
) -> Option<crate::native_appearance::SystemAccent> {
    if let Some(p) = palette {
        let quad = |i: usize| [p[i * 4], p[i * 4 + 1], p[i * 4 + 2]];
        return Some(crate::native_appearance::SystemAccent {
            on_dark: quad(1),
            on_light: quad(4),
        });
    }
    dwm_abgr.map(|abgr| {
        let rgb = [abgr as u8, (abgr >> 8) as u8, (abgr >> 16) as u8];
        crate::native_appearance::SystemAccent {
            on_dark: rgb,
            on_light: rgb,
        }
    })
}

fn transparency_effects_enabled() -> bool {
    hkcu_dword(PERSONALIZE_KEY, ENABLE_TRANSPARENCY_VALUE).is_none_or(|v| v != 0)
}

fn os_text_scale() -> f32 {
    match hkcu_dword(ACCESSIBILITY_KEY, TEXT_SCALE_FACTOR_VALUE) {
        Some(percent) if percent > 0 => percent as f32 / 100.0,
        _ => 1.0,
    }
}

fn sys_color(index: i32) -> [u8; 3] {
    // SAFETY: `GetSysColor` takes an integer index by value and returns a `COLORREF`.
    let rgb = colorref_swap(unsafe { GetSysColor(index) });
    [
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
    ]
}

fn resolve_forced_chrome(high_contrast: bool) -> Option<crate::chrome_band::ForcedChrome> {
    resolve_forced_chrome_with(high_contrast, force_hc_chrome_env())
}

fn resolve_forced_chrome_with(
    high_contrast: bool,
    force_override: bool,
) -> Option<crate::chrome_band::ForcedChrome> {
    (high_contrast || force_override).then(|| crate::chrome_band::ForcedChrome {
        window: sys_color(COLOR_WINDOW),
        window_text: sys_color(COLOR_WINDOWTEXT),
        highlight: sys_color(COLOR_HIGHLIGHT),
        highlight_text: sys_color(COLOR_HIGHLIGHTTEXT),
        btn_face: sys_color(COLOR_BTNFACE),
    })
}

fn force_hc_chrome_value(seam: Option<&std::ffi::OsStr>) -> bool {
    seam.is_some_and(|v| v != "0" && !v.is_empty())
}

fn force_hc_chrome_env() -> bool {
    force_hc_chrome_value(aterm_types::dev_seam!("ATERM_FORCE_HC_CHROME").as_deref())
}

/// Re-reads Windows High Contrast state and updates the forced chrome palette, returning whether it changed.
pub(crate) fn resync_forced_chrome_palette() -> bool {
    let high_contrast = high_contrast_active();
    CHROME_HIGH_CONTRAST.store(high_contrast, Ordering::Relaxed);
    let accent_moved = crate::native_appearance::install_system_accent(system_accent_read());
    crate::chrome_band::install_forced_chrome(resolve_forced_chrome(high_contrast)) || accent_moved
}

pub(crate) use crate::input::WheelNotch;

fn wheel_notch_from_raw(raw: u32) -> WheelNotch {
    if raw == WHEEL_PAGESCROLL {
        WheelNotch::Page
    } else {
        WheelNotch::Lines(raw)
    }
}

const WHEEL_NOTCH_TTL: std::time::Duration = std::time::Duration::from_secs(3);

static WHEEL_NOTCH_CACHE: Mutex<Option<(u32, std::time::Instant)>> = Mutex::new(None);

/// Returns the cached mouse-wheel scroll distance per notch (`SPI_GETWHEELSCROLLLINES`).
pub(crate) fn wheel_notch_scroll() -> WheelNotch {
    let now = std::time::Instant::now();
    let mut slot = WHEEL_NOTCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((raw, read_at)) = *slot
        && now.duration_since(read_at) < WHEEL_NOTCH_TTL
    {
        return wheel_notch_from_raw(raw);
    }
    let mut raw: u32 = DEFAULT_WHEEL_SCROLL_LINES;
    // SAFETY: `SystemParametersInfoW` reads a single `u32` into local `raw`.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            (&mut raw as *mut u32).cast::<c_void>(),
            0,
        )
    };
    let raw = if ok != 0 {
        raw
    } else {
        DEFAULT_WHEEL_SCROLL_LINES
    };
    *slot = Some((raw, now));
    wheel_notch_from_raw(raw)
}

fn chrome_policy_code(theme: WindowTheme) -> u8 {
    match theme {
        WindowTheme::Auto => 0,
        WindowTheme::Light => 1,
        WindowTheme::Dark => 2,
    }
}

fn chrome_policy_from_code(code: u8) -> WindowTheme {
    match code {
        1 => WindowTheme::Light,
        2 => WindowTheme::Dark,
        _ => WindowTheme::Auto,
    }
}

fn resolve_chrome_theme(policy: WindowTheme, bg: u32, high_contrast: bool) -> WindowTheme {
    match policy {
        WindowTheme::Light | WindowTheme::Dark => policy,
        WindowTheme::Auto if high_contrast || bg == CHROME_BG_UNKNOWN => WindowTheme::Auto,
        WindowTheme::Auto => {
            if crate::tab_bar::theme_is_dark(bg) {
                WindowTheme::Dark
            } else {
                WindowTheme::Light
            }
        }
    }
}

const CAPTION_TEXT_UNFOCUSED_DIM: f32 = 0.45;

fn caption_tint(
    policy: WindowTheme,
    surface: u32,
    backdrop: u32,
    high_contrast: bool,
    focused: bool,
) -> (u32, u32) {
    if !matches!(policy, WindowTheme::Auto)
        || high_contrast
        || backdrop != DWMSBT_NONE
        || surface == CHROME_BG_UNKNOWN
    {
        return (DWMWA_COLOR_DEFAULT, DWMWA_COLOR_DEFAULT);
    }
    let bg = surface;
    let bg_bytes = [
        ((bg >> 16) & 0xFF) as u8,
        ((bg >> 8) & 0xFF) as u8,
        (bg & 0xFF) as u8,
    ];
    let ink = if crate::tab_bar::theme_is_dark(bg) {
        [0xFF, 0xFF, 0xFF]
    } else {
        [0x00, 0x00, 0x00]
    };
    let ink = if focused {
        ink
    } else {
        crate::chrome_band::mix3(ink, bg_bytes, CAPTION_TEXT_UNFOCUSED_DIM)
    };
    let ink_rgb = (u32::from(ink[0]) << 16) | (u32::from(ink[1]) << 8) | u32::from(ink[2]);
    (colorref_swap(bg), colorref_swap(ink_rgb))
}

static CAPTION_TINT_APPLIED: Mutex<Vec<(isize, u64)>> = Mutex::new(Vec::new());

fn caption_tint_written(hwnd: isize) -> Option<(u32, u32)> {
    let applied = CAPTION_TINT_APPLIED
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    applied
        .iter()
        .find(|(h, _)| *h == hwnd)
        .map(|&(_, state)| ((state >> 32) as u32, state as u32))
}

static CAPTION_SURFACE: Mutex<Vec<(isize, u32)>> = Mutex::new(Vec::new());

fn caption_surface_of(hwnd: Option<isize>) -> u32 {
    let published = hwnd.and_then(|hwnd| {
        CAPTION_SURFACE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(h, _)| *h == hwnd)
            .map(|&(_, surface)| surface)
    });
    published.unwrap_or_else(|| CHROME_BG.load(Ordering::Relaxed))
}

/// Records the `0xRRGGBB` surface color beneath `window`'s caption for title-bar tinting.
pub(crate) fn note_caption_surface(window: &Window, surface: u32) {
    let Some(hwnd) = hwnd_of(window) else {
        return;
    };
    let surface = surface & 0x00FF_FFFF;
    {
        let mut published = CAPTION_SURFACE.lock().unwrap_or_else(|e| e.into_inner());
        match published.iter_mut().find(|(h, _)| *h == hwnd) {
            Some((_, s)) if *s == surface => return,
            Some((_, s)) => *s = surface,
            None => published.push((hwnd, surface)),
        }
    }
    resync_chrome_appearance(window);
}

fn apply_caption_tint(window: &Window, hwnd: isize, policy: WindowTheme, force: bool) {
    let (caption, text) = caption_tint(
        policy,
        caption_surface_of(Some(hwnd)),
        CHROME_BACKDROP.load(Ordering::Relaxed),
        CHROME_HIGH_CONTRAST.load(Ordering::Relaxed),
        window.has_focus(),
    );
    let state = (u64::from(caption) << 32) | u64::from(text);
    {
        let mut applied = CAPTION_TINT_APPLIED
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match applied.iter_mut().find(|(h, _)| *h == hwnd) {
            Some((_, s)) if *s == state && !force => return,
            Some((_, s)) => *s = state,
            None => applied.push((hwnd, state)),
        }
    }
    dwm_set_u32(hwnd, DWMWA_CAPTION_COLOR, caption);
    dwm_set_u32(hwnd, DWMWA_TEXT_COLOR, text);
}

fn reduced_transparency_from(high_contrast: bool, transparency_effects: bool) -> bool {
    high_contrast || !transparency_effects
}

fn effective_backdrop(requested: u32) -> u32 {
    if requested != DWMSBT_NONE
        && crate::native_appearance::current_preferences().reduced_transparency
    {
        DWMSBT_NONE
    } else {
        requested
    }
}

fn apply_backdrop(hwnd: isize) {
    let effective = effective_backdrop(CHROME_BACKDROP_REQUESTED.load(Ordering::Relaxed));
    CHROME_BACKDROP.store(effective, Ordering::Relaxed);
    dwm_set_u32(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, effective);
}

fn apply_chrome_appearance(window: &Window, policy: WindowTheme) {
    let high_contrast = high_contrast_active();
    CHROME_HIGH_CONTRAST.store(high_contrast, Ordering::Relaxed);
    let resolved = resolve_chrome_theme(policy, caption_surface_of(hwnd_of(window)), high_contrast);
    {
        let _no_self_wake = ChromeApplyGuard::new();
        window.set_theme(window_theme_to_winit(resolved));
    }
    let Some(hwnd) = hwnd_of(window) else {
        return;
    };
    match resolved {
        WindowTheme::Dark => dwm_set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 1),
        WindowTheme::Light => dwm_set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 0),
        WindowTheme::Auto if high_contrast => {
            dwm_set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 0);
        }
        WindowTheme::Auto => {}
    }
    dwm_set_u32(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
    apply_backdrop(hwnd);
    apply_caption_tint(window, hwnd, policy, /* force= */ true);
}

/// Re-applies DWM caption theme, tint, and backdrop attributes for `window`.
pub(crate) fn resync_chrome_appearance(window: &Window) {
    apply_chrome_appearance(
        window,
        chrome_policy_from_code(CHROME_POLICY.load(Ordering::Relaxed)),
    );
}

fn chrome_reassert_needed(want_dark: bool, applied: Option<u32>) -> bool {
    match applied {
        Some(v) => (v != 0) != want_dark,
        None => false,
    }
}

/// Verifies per frame that `window`'s DWM dark-mode attribute still matches the resolved chrome theme.
pub(crate) fn verify_chrome_appearance(window: &Window) {
    let Some(hwnd) = hwnd_of(window) else {
        return;
    };
    let policy = chrome_policy_from_code(CHROME_POLICY.load(Ordering::Relaxed));
    let high_contrast = CHROME_HIGH_CONTRAST.load(Ordering::Relaxed);
    apply_caption_tint(window, hwnd, policy, /* force= */ false);
    let want_dark =
        match resolve_chrome_theme(policy, caption_surface_of(Some(hwnd)), high_contrast) {
            WindowTheme::Dark => true,
            WindowTheme::Light => false,
            WindowTheme::Auto if high_contrast => false,
            WindowTheme::Auto => return,
        };
    if matches!(window.theme(), Some(aterm_window::window::Theme::Dark)) == want_dark {
        // Distinct branch.
        return;
    }
    if chrome_reassert_needed(want_dark, dwm_get_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE)) {
        apply_chrome_appearance(window, policy);
    }
}

static END_SESSION_SNAPSHOT: Mutex<Option<(crate::restore::RestoreManifest, u64)>> =
    Mutex::new(None);

static END_SESSION_GEN: AtomicU64 = AtomicU64::new(0);
static END_SESSION_FLUSHED: AtomicU64 = AtomicU64::new(0);

static PREV_WNDPROC: Mutex<Vec<(isize, isize)>> = Mutex::new(Vec::new());

const GWLP_WNDPROC: i32 = -4;
const WM_QUERYENDSESSION: u32 = 0x0011;
const WM_ENDSESSION: u32 = 0x0016;
const WM_NCDESTROY: u32 = 0x0082;
const WM_GETOBJECT: u32 = 0x003D;
const WM_SYSCOLORCHANGE: u32 = 0x0015;
const WM_SETTINGCHANGE: u32 = 0x001A;
const WM_THEMECHANGED: u32 = 0x031A;
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;

fn is_os_settings_message(msg: u32) -> bool {
    matches!(
        msg,
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED
    )
}

thread_local! {
    static CHROME_APPLY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct ChromeApplyGuard;

impl ChromeApplyGuard {
    fn new() -> Self {
        CHROME_APPLY_DEPTH.with(|d| d.set(d.get().saturating_add(1)));
        Self
    }
}

impl Drop for ChromeApplyGuard {
    fn drop(&mut self) {
        CHROME_APPLY_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

fn applying_chrome() -> bool {
    CHROME_APPLY_DEPTH.with(std::cell::Cell::get) > 0
}

fn watch_os_settings_message(msg: u32) -> bool {
    is_os_settings_message(msg) && !(msg == WM_THEMECHANGED && applying_chrome())
}

static SETTINGS_PROXY: Mutex<Option<EventLoopProxy<Wake>>> = Mutex::new(None);

static SETTINGS_WAKE_PENDING: AtomicBool = AtomicBool::new(false);

static SETTINGS_MSGS_SEEN: AtomicU64 = AtomicU64::new(0);
static SETTINGS_WAKES_POSTED: AtomicU64 = AtomicU64::new(0);

fn queue_os_settings_wake() {
    SETTINGS_MSGS_SEEN.fetch_add(1, Ordering::Relaxed);
    if !claim_os_settings_wake(&SETTINGS_WAKE_PENDING) {
        return;
    }
    let posted = SETTINGS_PROXY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|proxy| proxy.send_event(Wake::ReduceMotionChanged).is_ok());
    if posted {
        SETTINGS_WAKES_POSTED.fetch_add(1, Ordering::Relaxed);
    } else {
        SETTINGS_WAKE_PENDING.store(false, Ordering::Release);
    }
}

fn claim_os_settings_wake(pending: &AtomicBool) -> bool {
    !pending.swap(true, Ordering::AcqRel)
}

fn os_settings_counters() -> (u64, u64) {
    (
        SETTINGS_MSGS_SEEN.load(Ordering::Relaxed),
        SETTINGS_WAKES_POSTED.load(Ordering::Relaxed),
    )
}

/// Clears the settings-change coalescer before the event loop re-samples OS preferences.
pub(crate) fn note_os_settings_resample() {
    SETTINGS_WAKE_PENDING.store(false, Ordering::Release);
    *WHEEL_NOTCH_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Publishes or clears the [`crate::restore::RestoreManifest`] written on `WM_ENDSESSION`.
pub(crate) fn set_end_session_snapshot(manifest: Option<crate::restore::RestoreManifest>) {
    let mut slot = END_SESSION_SNAPSHOT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *slot = manifest.map(|m| (m, END_SESSION_GEN.fetch_add(1, Ordering::Relaxed) + 1));
}

fn flush_end_session_snapshot() {
    let slot = END_SESSION_SNAPSHOT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some((manifest, generation)) = slot.as_ref() else {
        return;
    };
    if END_SESSION_FLUSHED.load(Ordering::Relaxed) == *generation || manifest.is_empty() {
        return;
    }
    if crate::restore::write(manifest).is_ok() {
        END_SESSION_FLUSHED.store(*generation, Ordering::Relaxed);
    }
}

unsafe extern "system" fn aterm_subclass_wndproc(
    hwnd: isize,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    // UIA requests are sent messages too. Serve the retained publication from
    // this existing chain; the provider never re-enters the live App.
    if msg == WM_GETOBJECT
        && let Some(result) = aterm_a11y::uia::handle_wm_getobject(hwnd, wparam, lparam)
    {
        return result;
    }
    let prev = {
        let map = PREV_WNDPROC.lock().unwrap_or_else(|e| e.into_inner());
        map.iter().find(|(h, _)| *h == hwnd).map(|&(_, p)| p)
    };
    if msg == WM_QUERYENDSESSION || (msg == WM_ENDSESSION && wparam != 0) {
        flush_end_session_snapshot();
    }
    if watch_os_settings_message(msg) {
        queue_os_settings_wake();
    }
    // SAFETY: `prev` is the displaced `WNDPROC` for `hwnd` (or falls back to `DefWindowProcW`).
    let result = match prev {
        Some(prev) => unsafe { CallWindowProcW(prev, hwnd, msg, wparam, lparam) },
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    if msg == WM_NCDESTROY {
        aterm_a11y::uia::window_destroyed(hwnd);
        PREV_WNDPROC
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|&(h, _)| h != hwnd);
    }
    result
}

fn install_window_subclass(window: &Window, proxy: &EventLoopProxy<Wake>) {
    {
        let mut slot = SETTINGS_PROXY.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(proxy.clone());
        }
    }
    let Some(hwnd) = hwnd_of(window) else {
        return;
    };
    let mut map = PREV_WNDPROC.lock().unwrap_or_else(|e| e.into_inner());
    if map.iter().any(|&(h, _)| h == hwnd) {
        return;
    }
    // SAFETY: `hwnd` is a live winit window on this thread and `aterm_subclass_wndproc` is static.
    let prev = unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            aterm_subclass_wndproc as *const () as isize,
        )
    };
    if prev != 0 {
        map.push((hwnd, prev));
    }
}

/// Native Windows [`AppRt`] implementation decorating winit `HWND`s with DWM attributes and subclassing.
pub(crate) struct AppRtWindows {
    pub(crate) probe_preferences: bool,
}

impl AppRt for AppRtWindows {
    fn window_set_background_color(&self, window: &Window, bg: u32) {
        CHROME_BG.store(bg & 0x00FF_FFFF, Ordering::Relaxed);
        resync_chrome_appearance(window);
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        let colorref = colorref_swap(bg);
        // SAFETY: swaps the class background brush on live `hwnd` and deletes our prior custom brush.
        unsafe {
            let brush = CreateSolidBrush(colorref);
            if brush == 0 {
                return;
            }
            let old = SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush);
            if old != 0 {
                let _ = DeleteObject(old);
            }
        }
    }

    fn window_flush_backdrop(&self, window: &Window) {
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        // SAFETY: `RedrawWindow` with null rect/region invalidates the full client area of live `hwnd`.
        unsafe {
            let _ = RedrawWindow(
                hwnd,
                std::ptr::null(),
                0,
                RDW_INVALIDATE | RDW_ERASE | RDW_ERASENOW,
            );
        }
    }

    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        CHROME_POLICY.store(chrome_policy_code(theme), Ordering::Relaxed);
        apply_chrome_appearance(window, theme);
    }

    fn window_set_surface_colorspace(
        &self,
        _window: &Window,
        cs: SurfaceColorspace,
    ) -> Option<SurfaceColorspace> {
        Some(match cs {
            SurfaceColorspace::DisplayP3 => SurfaceColorspace::Srgb,
            SurfaceColorspace::Srgb | SurfaceColorspace::ExtendedLinearSrgb => cs,
        })
    }

    fn window_anchor_surface_top_left(&self, _window: &Window) {}

    fn window_surface_presentation(&self, _window: &Window) -> Option<(String, f64, bool)> {
        None
    }

    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: BackgroundMaterial,
        _translucent: bool,
        _bg: u32,
    ) {
        CHROME_BACKDROP_REQUESTED.store(backdrop_for(material), Ordering::Relaxed);
        if effective_backdrop(backdrop_for(material)) != DWMSBT_NONE
            && !aterm_gpu::visual_swapchain_active()
        {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                if aterm_gpu::visual_swapchain_withdrawn() {
                    crate::logging::stderr_line!(
                        "aterm-gui: background_material is styling the caption only \
                         (client=opaque): the client-area backdrop path was withdrawn \
                         this run — see the diagnostic above"
                    );
                } else {
                    crate::logging::stderr_line!(
                        "aterm-gui: background_material is styling the caption only \
                         (client=opaque): the client-area backdrop engages when the material \
                         is set at launch with the GPU renderer and hdr_glow off — restart \
                         aterm to see it in the padding"
                    );
                    if client_backdrop_decline().is_none() {
                        crate::message_inbox::queue_message(
                            crate::message_reporters::backdrop_declined(
                                "background_material is styling the title bar only",
                                "the client-area backdrop is chosen at launch; start aterm \
                                 with the material set (and hdr_glow off) to see it in the \
                                 window padding",
                            ),
                        );
                        note_client_backdrop_declined(ClientBackdropDecline::NotAtLaunch);
                    }
                }
            });
        }
        resync_chrome_appearance(window);
    }

    fn window_work_area_pts(&self, window: &Window) -> Option<crate::app_window::WorkAreaPts> {
        let hwnd = hwnd_of(window)?;
        let mut info = MonitorInfo {
            size: u32::try_from(std::mem::size_of::<MonitorInfo>()).ok()?,
            monitor: Rect::default(),
            work: Rect::default(),
            flags: 0,
        };
        // SAFETY: `MonitorFromWindow` and `GetMonitorInfoW` write into a sized stack `MonitorInfo`.
        let ok = unsafe {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            monitor != 0 && GetMonitorInfoW(monitor, &raw mut info) != 0
        };
        if !ok {
            return None;
        }
        work_area_pts_from_rect(&info.work, window.scale_factor())
    }

    fn window_bring_to_front(&self, window: &Window) -> bool {
        let Some(hwnd) = hwnd_of(window) else {
            return false;
        };
        // SAFETY: `IsIconic`, `ShowWindow`, and `SetForegroundWindow` on a live UI-thread `HWND`.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd) != 0
        }
    }

    fn screen_edr_max(&self, window: &Window) -> f32 {
        crate::hdr_win::edr_max(window)
    }

    fn screen_edr_potential(&self, window: &Window) -> f32 {
        crate::hdr_win::edr_max(window)
    }

    fn screen_sdr_white_scale(&self, window: &Window) -> f32 {
        crate::hdr_win::sdr_white_scale(window)
    }

    fn send_notification_init(
        &self,
        suppress: Arc<Mutex<HashSet<u64>>>,
        silent: Arc<AtomicBool>,
        center: crate::notice_center::NoticeCenter,
    ) -> SyncSender<NotifyMsg> {
        crate::notify::spawn_delivery(suppress, silent, center)
    }

    fn install_menu(&self, proxy: &EventLoopProxy<Wake>) -> Option<menu::MenuHandle> {
        menu::install(proxy)
    }

    fn install_toolbar(
        &self,
        window: &Window,
        proxy: &EventLoopProxy<Wake>,
        wid: WindowId,
    ) -> Option<toolbar::ToolbarHandle> {
        install_window_subclass(window, proxy);
        toolbar::install_window_toolbar(window, proxy, wid)
    }

    fn set_toolbar_tabs(&self, handle: &toolbar::ToolbarHandle, tabs: ToolbarTabsModel<'_>) {
        let ToolbarTabsModel {
            titles,
            ids,
            metadata,
            tooltips,
            ext,
            active,
        } = tabs;
        toolbar::set_window_tabs(handle, titles, ids, metadata, tooltips, ext, active);
    }

    fn read_toolbar_chrome(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        toolbar::read_tab_chrome(handle)
    }

    fn confirm(&self, title: &str, body: &str, proceed_label: &str) -> Option<bool> {
        crate::win32::confirm_proceed_cancel(title, body, proceed_label)
    }

    fn install_quit_confirm(&self) {}

    fn reduce_motion(&self) -> bool {
        if !self.probe_preferences {
            return false;
        }
        let mut animations_enabled: i32 = 1;
        // SAFETY: `SystemParametersInfoW` reads a single `i32` boolean into local `enabled`.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut animations_enabled as *mut i32).cast::<c_void>(),
                0,
            )
        };
        ok != 0 && animations_enabled == 0
    }

    fn native_appearance_preferences(&self) -> crate::native_appearance::AppearancePreferences {
        if !self.probe_preferences {
            return crate::native_appearance::AppearancePreferences::default();
        }
        let _ = resync_forced_chrome_palette();
        let high_contrast = CHROME_HIGH_CONTRAST.load(Ordering::Relaxed);
        crate::native_appearance::AppearancePreferences {
            high_contrast,
            reduced_transparency: reduced_transparency_from(
                high_contrast,
                transparency_effects_enabled(),
            ),
            differentiate_without_color: false,
            text_scale: os_text_scale(),
        }
    }

    fn observe_reduce_motion(&self, _proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver> {
        None
    }
}

/// Captures `window` (including caption chrome and client area) as tightly packed top-down BGRX pixels.
pub(crate) fn capture_window_bgrx(window: &Window) -> Result<(Vec<u8>, u32, u32), String> {
    let hwnd = hwnd_of(window).ok_or_else(|| "no window to capture (headless)".to_string())?;
    // SAFETY: GDI objects are freed on all paths and `GetDIBits` writes within the `w * h * 4` buffer.
    unsafe {
        let mut rect = Rect::default();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return Err("GetWindowRect failed".to_string());
        }
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w <= 0 || h <= 0 {
            return Err(format!("window has no area ({w}x{h})"));
        }
        let (width, height) = (
            usize::try_from(w).map_err(|_| "window capture width does not fit memory")?,
            usize::try_from(h).map_err(|_| "window capture height does not fit memory")?,
        );
        let buffer_len = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "window capture buffer dimensions overflow".to_string())?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(buffer_len)
            .map_err(|error| format!("window capture buffer allocation failed: {error}"))?;
        buf.resize(buffer_len, 0);

        let screen = GetDC(0);
        if screen == 0 {
            return Err("GetDC(screen) failed".to_string());
        }
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        if mem == 0 || bmp == 0 {
            if bmp != 0 {
                DeleteObject(bmp);
            }
            if mem != 0 {
                DeleteDC(mem);
            }
            ReleaseDC(0, screen);
            return Err("GDI DC/bitmap creation failed".to_string());
        }
        let old = SelectObject(mem, bmp);
        if old == 0 {
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(0, screen);
            return Err("SelectObject(bitmap) failed".to_string());
        }

        let print_succeeded = PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT) != 0;

        if SelectObject(mem, old) == 0 {
            DeleteDC(mem);
            DeleteObject(bmp);
            ReleaseDC(0, screen);
            return Err("SelectObject(restore) failed".to_string());
        }
        let mut bmi = BitmapInfoHeader {
            size: core::mem::size_of::<BitmapInfoHeader>() as u32,
            width: w,
            height: -h,
            planes: 1,
            bit_count: 32,
            compression: 0,
            ..Default::default()
        };
        let lines = if print_succeeded {
            GetDIBits(
                mem,
                bmp,
                0,
                h as u32,
                buf.as_mut_ptr().cast::<c_void>(),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(0, screen);
        validate_window_capture_transfer(print_succeeded, lines, h)?;
        Ok((buf, w as u32, h as u32))
    }
}

impl crate::App {
    /// Computes the `0xRRGGBB` surface color directly beneath window `wid`'s title bar.
    pub(crate) fn caption_surface_tone(&self, wid: WindowId) -> u32 {
        let header = self.chrome_palette(crate::app_render::ChromeSurface::Header);
        if self.tab_strip_rows > 0
            && let Some((band, _)) = crate::tab_bar::strip_bleed_tones(header)
        {
            return band;
        }
        self.windows
            .get(&wid)
            .map(|ws| ws.input_scratch.default_bg)
            .filter(|&bg| bg >> 24 == 0)
            .unwrap_or(self.theme.bg)
            & 0x00FF_FFFF
    }
}

#[cfg(test)]
#[path = "platform_win_tests.rs"]
mod tests;

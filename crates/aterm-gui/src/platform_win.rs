// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The native Windows application-runtime ([`AppRtWindows`]): the [`crate::platform::AppRt`]
//! seam with real OS chrome, as a thin decorator over the winit-owned `HWND`
//! (`docs/NATIVE_WINDOWS_DESIGN.md` §1: extend winit, do not build a second frontend).
//!
//! It is deliberately THIN — a decorator over the winit-owned `HWND`, exactly the
//! spine `docs/NATIVE_WINDOWS_DESIGN.md` §1 ratifies (extend winit; do not build a
//! second frontend). Everything here is either:
//!
//! * **hand-rolled leaf Win32** (the house style — see `hdr_win.rs` /
//!   `clipboard_win.rs` / `aterm-pty`'s `ffi.rs`): `DwmSetWindowAttribute` for the
//!   immersive-dark title bar, rounded corners, and the Mica/Acrylic system
//!   backdrop; `SystemParametersInfoW` for the Reduce-Motion accessibility read.
//!   These are a handful of well-understood flat-C calls, so no COM/`windows`-crate
//!   surface is pulled in for them; or
//! * a **verbatim delegation** to the same cross-platform modules `AppRtLinux`
//!   uses (`notify`/`menu`/`toolbar`) so the tab-strip model, notification thread,
//!   and menu stub behave identically on every non-macOS target; or
//! * a call into [`crate::hdr_win`] for the EDR headroom / SDR-white scale that
//!   feeds the (opt-in, HDR-only) cursor-aurora present.
//!
//! Preference reads have the same per-instance inert test arm as the other runtimes.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use aterm_window::event_loop::EventLoopProxy;
use aterm_window::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use aterm_window::window::Window;
use core::ffi::c_void;

use crate::app_config::{BackgroundMaterial, WindowTheme};
use crate::notify::NotifyMsg;
use crate::platform::{
    AppRt, ReduceMotionObserver, SurfaceColorspace, ToolbarTabsModel, window_theme_to_winit,
};
use crate::{Wake, WindowId, menu, toolbar};

// ---- Hand-rolled leaf Win32 (dwmapi + user32) -------------------------------------
//
// HWND is a pointer-sized handle, passed as `isize` (the MSVC `system` ABI; `hdr_win.rs`
// threads HMONITOR the same way). DWM writes are best-effort: an attribute an older Windows 10
// build lacks returns `E_INVALIDARG`, and that facet stays at its OS default.

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
    // The aterm window subclass below (end-session persistence + the OS-settings hook).
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn CallWindowProcW(prev: isize, hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    // The early-reveal themed-erase flush (`window_flush_backdrop`).
    fn RedrawWindow(hwnd: isize, rect: *const Rect, region: isize, flags: u32) -> i32;
    /// One entry of the live system palette, as a COLORREF: a table lookup inside user32,
    /// refreshed by the OS on a scheme change, cheap enough to re-read on every broadcast.
    fn GetSysColor(index: i32) -> u32;
    // The new-window cascade's work area (`window_work_area_pts`): HMONITOR is a
    // pointer-sized handle, threaded as `isize` like every other handle here.
    fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
    // The `attach` lane's raise (`window_bring_to_front`).
    fn IsIconic(hwnd: isize) -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    // The band's system menu (`popup_system_menu`).
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
}

/// `WM_SYSCOMMAND` / `SC_KEYMENU`: ask `DefWindowProc` to open the window menu, the Alt+Space
/// path, anchored at the caption's top-left. A posted `WM_POPUPSYSTEMMENU`,
/// `WM_NCRBUTTONUP`+HTCAPTION or `WM_CONTEXTMENU` opens nothing on Windows 11: `DefWindowProc`
/// validates those against real non-client input.
const WM_SYSCOMMAND: u32 = 0x0112;
const SC_KEYMENU: usize = 0xF100;

/// Post the window's own system menu, as a caption right-press does elsewhere on Windows (the
/// band is this window's caption). `false` when there is no HWND.
///
/// Posted, not sent: `DefWindowProc` tracks the menu in a modal loop, and entering it from this
/// winit callback would re-enter the event loop with `App` borrowed.
///
/// Call it on a release, never a press: winit holds `SetCapture` from button-down to button-up,
/// and `DefWindowProc` will not enter the menu's modal loop while the thread holds capture, so a
/// post made on the down is discarded. `App::on_mouse_input`'s `band_menu_release_pending`
/// latch is the caller's shape.
pub(crate) fn popup_system_menu(window: &Window) -> bool {
    let Some(hwnd) = hwnd_of(window) else {
        return false;
    };
    // SAFETY: `PostMessageW` queues one message on a live HWND owned by this
    // process; no pointers are carried.
    unsafe { PostMessageW(hwnd, WM_SYSCOMMAND, SC_KEYMENU, isize::from(b' ')) != 0 }
}

/// `MONITORINFO`. `size` must be preset to the struct's size: that is how `GetMonitorInfoW`
/// tells this from the longer `MONITORINFOEXW`.
#[repr(C)]
struct MonitorInfo {
    size: u32,
    monitor: Rect,
    work: Rect,
    flags: u32,
}

/// `MONITOR_DEFAULTTONEAREST`: answer with the closest monitor rather than null
/// for a window that is (partly) off every display.
const MONITOR_DEFAULTTONEAREST: u32 = 0x0000_0002;
/// `SW_RESTORE`: un-minimize to the size and position it had before.
const SW_RESTORE: i32 = 9;

/// `rcWork` (physical screen px) → the cascade's logical-point rectangle, or `None` for
/// geometry the cascade cannot use: a non-finite or non-positive scale, or an empty or inverted
/// work area (either would wrap every new window onto the same spot).
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

// `RegGetValueW` — the one-call registry read (open + query + close + type check).
// Used for the two OS preferences Win32 exposes NOWHERE else: "Transparency effects"
// and the accessibility text-scale factor.
//
// WHY THE REGISTRY AND NOT AN API. Both settings have exactly one public reader,
// `Windows.UI.ViewManagement.UISettings` (`AdvancedEffectsEnabled` /
// `TextScaleFactor`), which is WinRT: it needs `RoInitialize`, an activation factory,
// a COM apartment on the calling thread, and either the `windows` crate or several
// hundred lines of hand-rolled IInspectable vtable work. Every other Win32 surface in
// this file is a flat-C leaf call, and the registry values these properties read are
// stable, documented-by-use locations that winit's own dark-mode probe
// (`crates/aterm-window/.../dark_mode.rs`, `AppsUseLightTheme`) already depends on — one of
// them in the very same key. A missing value is not an error: both readers fall back
// to the Windows default, which is what an untouched machine has.
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

/// `GCLP_HBRBACKGROUND` — the window-class background brush slot.
const GCLP_HBRBACKGROUND: i32 = -10;

/// `RedrawWindow` flags for the early-reveal backdrop flush: invalidate and erase the client
/// area and deliver `WM_ERASEBKGND` before returning. Not `RDW_UPDATENOW`: that also delivers
/// `WM_PAINT` synchronously, re-entering winit's wndproc with a `RedrawRequested` inside
/// `attach_os_window`, before any present target exists.
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

/// `RECT`.
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// `HIGHCONTRASTW`, used only for the read-only `SPI_GETHIGHCONTRAST` query.
#[repr(C)]
struct HighContrastW {
    size: u32,
    flags: u32,
    default_scheme: *mut u16,
}

/// `BITMAPINFOHEADER` (the fixed head of `BITMAPINFO`; for a 32bpp `BI_RGB` DIB the
/// colour table is empty, so the header alone suffices for `GetDIBits`).
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

/// `PW_RENDERFULLCONTENT`: render the window's full content including its DirectComposition /
/// GPU (wgpu swapchain) surface, so the terminal grid shows in the capture.
const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;
/// `DIB_RGB_COLORS`.
const DIB_RGB_COLORS: u32 = 0;

/// `DWMWA_USE_IMMERSIVE_DARK_MODE` — the DOCUMENTED dark title-bar toggle, and the
/// value `aterm-ctl chrome` reads back as ground truth.
///
/// winit does **not** write it: `Window::set_theme` drives only `SetWindowTheme` plus
/// the undocumented `WCA_USEDARKMODECOLORS` composition attribute (see
/// `crates/aterm-window/src/platform_impl/windows/dark_mode.rs::try_theme`), which is exactly
/// why this attribute used to read back unset for `Auto`. [`apply_chrome_appearance`]
/// writes it explicitly for that reason — do not "simplify" it away on the assumption
/// that winit already covers it.
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
/// `DWMWA_WINDOW_CORNER_PREFERENCE` (Windows 11+): round the window corners.
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
/// `DWMWCP_ROUND` — the standard rounded-corner radius.
const DWMWCP_ROUND: u32 = 2;
/// `DWMWA_CAPTION_COLOR` (Windows 11+): a solid caption fill, as a COLORREF, so the caption
/// reads as part of the terminal surface. On Windows 10 the write fails with `E_INVALIDARG`,
/// leaving the immersive-dark-only caption.
const DWMWA_CAPTION_COLOR: u32 = 35;
/// `DWMWA_TEXT_COLOR` (Windows 11+): the caption title text colour, as a COLORREF.
/// Set alongside [`DWMWA_CAPTION_COLOR`]: a pinned caption fill also pins the text
/// the OS would otherwise contrast against its own palette.
const DWMWA_TEXT_COLOR: u32 = 36;
/// `DWMWA_COLOR_DEFAULT` — the documented reset sentinel for the two colour
/// attributes above: writing it returns the caption/text to DWM's own palette
/// (including DWM's native unfocused dimming).
const DWMWA_COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
/// `DWMWA_SYSTEMBACKDROP_TYPE` (Windows 11 22H2+): Mica / Acrylic / none.
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
/// `DWMSBT_NONE` — no system backdrop (the minimal, opaque default).
const DWMSBT_NONE: u32 = 1;
/// `DWMSBT_MAINWINDOW`: Mica (the desktop-tinted top-level backdrop).
const DWMSBT_MAINWINDOW: u32 = 2;
/// `DWMSBT_TRANSIENTWINDOW`: Acrylic (the blurred transient backdrop).
const DWMSBT_TRANSIENTWINDOW: u32 = 3;
/// `DWMSBT_TABBEDWINDOW` — the tabbed-window (Mica Alt) backdrop.
const DWMSBT_TABBEDWINDOW: u32 = 4;

/// `SPI_GETCLIENTAREAANIMATION` — the "Show animations in Windows" master switch
/// (the closest OS-wide Reduce-Motion signal Windows exposes).
const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
/// Windows High Contrast is also a reduced-transparency request.
const SPI_GETHIGHCONTRAST: u32 = 0x0042;
const HCF_HIGHCONTRASTON: u32 = 0x0000_0001;

// ---- The OS chrome palette (`GetSysColor`) -------------------------------------------
//
// The five system colours the chrome is painted from under a High-Contrast scheme: document
// (`WINDOW`/`WINDOWTEXT`), control (`BTNFACE`) and selection (`HIGHLIGHT`/`HIGHLIGHTTEXT`);
// `chrome_band::ForcedChrome` says which aterm surface takes which.

/// `COLOR_WINDOW` — the document / editable-field surface.
const COLOR_WINDOW: i32 = 5;
/// `COLOR_WINDOWTEXT` — the one ink of an HC palette.
const COLOR_WINDOWTEXT: i32 = 8;
/// `COLOR_HIGHLIGHT` — the selected surface.
const COLOR_HIGHLIGHT: i32 = 13;
/// `COLOR_HIGHLIGHTTEXT` — ink on `COLOR_HIGHLIGHT`.
const COLOR_HIGHLIGHTTEXT: i32 = 14;
/// `COLOR_BTNFACE` — the control-face surface every chrome band takes.
const COLOR_BTNFACE: i32 = 15;

/// `HKEY_CURRENT_USER`.
const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as isize;
/// `RRF_RT_REG_DWORD` — refuse to coerce; a value of another type reads as absent.
const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
/// `RRF_RT_REG_BINARY` — the same refusal for a byte blob.
const RRF_RT_REG_BINARY: u32 = 0x0000_0008;

/// The Personalize key, home of both the OS light/dark preference winit reads and
/// the "Transparency effects" switch. Settings ▸ Personalization ▸ Colours ▸
/// "Transparency effects" writes `EnableTransparency` here (1 = on, the default).
const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const ENABLE_TRANSPARENCY_VALUE: &str = "EnableTransparency";

/// Settings ▸ Accessibility ▸ Text size writes its slider here as a percent (100..=225);
/// absent means 100. `UISettings.TextScaleFactor` reports the same number as a ratio.
const ACCESSIBILITY_KEY: &str = r"Software\Microsoft\Accessibility";
const TEXT_SCALE_FACTOR_VALUE: &str = "TextScaleFactor";

/// `SPI_GETWHEELSCROLLLINES`: how many lines one wheel notch scrolls, the value behind
/// Settings ▸ Mouse ▸ "Lines to scroll at a time".
const SPI_GETWHEELSCROLLLINES: u32 = 0x0068;
/// `WHEEL_PAGESCROLL`: the sentinel that slider writes in its "One screen at a time" position,
/// meaning a notch scrolls a whole page rather than N lines.
const WHEEL_PAGESCROLL: u32 = u32::MAX;
/// The Windows default, and the fallback if the query fails. Every other window on
/// the desktop moves this far per notch.
const DEFAULT_WHEEL_SCROLL_LINES: u32 = 3;

/// Accept a GDI window photograph only when both fallible transfer stages
/// completed in full. `PrintWindow == 0` leaves the target bitmap unspecified;
/// likewise, a positive-but-short `GetDIBits` return copied only a prefix of the
/// requested image. Neither may be promoted to a successful exact capture.
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

/// The `HWND` behind a winit [`Window`], or `None` with no Win32 handle (headless or not yet
/// realized). Mirrors `App::window_hwnd`.
pub(crate) fn hwnd_of(window: &Window) -> Option<isize> {
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Theme byte order (`0x00RRGGBB`) ↔ COLORREF (`0x00BBGGRR`), which GDI brushes and the DWM
/// colour attributes take. The swap is its own inverse, so one helper serves writes and
/// readbacks and the caption and class brush cannot drift; a stray top byte is dropped.
fn colorref_swap(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

/// Best-effort `DwmSetWindowAttribute` for a 4-byte (`BOOL`/`DWORD`) attribute.
fn dwm_set_u32(hwnd: isize, attr: u32, value: u32) {
    // SAFETY: a single documented flat-C DWM call; `value` outlives the call, `cb`
    // is its exact byte size, and the return `HRESULT` is intentionally dropped
    // (unsupported attribute → E_INVALIDARG → leave the facet at its OS default).
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            attr,
            (&value as *const u32).cast::<c_void>(),
            core::mem::size_of::<u32>() as u32,
        );
    }
}

/// Best-effort `DwmGetWindowAttribute` read of a 4-byte (`BOOL`/`DWORD`) attribute: `Some`
/// when the OS supports getting it, else `None` (older Windows, or a write-only attribute).
/// [`read_chrome_lines`] uses it to report what is on the live window, not what we set.
fn dwm_get_u32(hwnd: isize, attr: u32) -> Option<u32> {
    let mut value: u32 = 0;
    // SAFETY: a single documented flat-C DWM read into a local `u32`; `cb` is its
    // exact byte size and the out-pointer is valid for the call. A non-zero HRESULT
    // (unsupported attribute) leaves `value` untouched and we return `None`.
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

/// Human label for a `DWMWA_WINDOW_CORNER_PREFERENCE` value.
fn corner_label(v: u32) -> &'static str {
    match v {
        0 => "default",
        1 => "donotround",
        2 => "round",
        3 => "roundsmall",
        _ => "?",
    }
}

/// Human label for a `DWMWA_SYSTEMBACKDROP_TYPE` value.
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

/// The `caption_color` / `caption_text` field of the `chrome` line from the DWM
/// readback (`read`) and the COLORREF last written (`set`): the readback where DWM
/// answers it; else the written value marked `(set)`; else `n/a`.
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

/// The `chrome` introspection lines for the live Windows window: the backend name, the DWM
/// attributes read back off the HWND (dark mode, corner preference, system backdrop) and the
/// effective winit theme, so `aterm-ctl chrome` reports ground truth, not intent. A headless or
/// not-yet-realized window says so.
pub(crate) fn read_chrome_lines(
    window: Option<&Window>,
    cfg_theme: WindowTheme,
    cfg_material: BackgroundMaterial,
) -> Vec<String> {
    // The configured intent, so introspection can tell "config didn't reach the window" from
    // "the DWM getter reports the effective, not the set, value".
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
    // Read the applied attributes back from the OS. Some are gettable only on newer
    // Windows; `n/a` means the OS did not return a value (not that it is unset).
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
    // The caption tint (H3), read back as ground truth where DWM answers:
    // `default` is the DWMWA_COLOR_DEFAULT sentinel (tint intentionally absent —
    // High Contrast, a backdrop material, an explicit theme policy, or no published
    // background yet), a hex value is the live tint in theme byte order. Windows 11
    // build 26200 refuses to read either attribute back (`E_INVALIDARG`) though it
    // paints them, so there the field is the pair last WRITTEN, marked `(set)`;
    // `n/a` only when nothing was written (a pre-Win11 build drops the write too).
    // The acceptance check: this value tracks the surface under the caption
    // (`caption_surface`, the tab band's tone with a strip), and `caption_text`
    // visibly dims while the window is unfocused.
    let written = caption_tint_written(hwnd);
    let caption_color_field =
        |attr: u32, set: Option<u32>| caption_color_label(dwm_get_u32(hwnd, attr), set);
    let caption = caption_color_field(DWMWA_CAPTION_COLOR, written.map(|w| w.0));
    let caption_text = caption_color_field(DWMWA_TEXT_COLOR, written.map(|w| w.1));
    let caption_surface = match caption_surface_of(Some(hwnd)) {
        CHROME_BG_UNKNOWN => "n/a".to_string(),
        surface => format!("#{surface:06x}"),
    };
    // Which client-area presentation path this process runs: `visual` (the DirectComposition
    // swapchain, whose padding and chrome-bleed margins carry per-pixel alpha so the DWM
    // backdrop shows through) or `opaque` (the HWND swapchain covers the backdrop, so
    // `backdrop=` styles only the caption). Read from the built GPU instance, not config; when
    // opaque despite a material, the qualifier names why (see [`CLIENT_BACKDROP_DECLINE`]).
    let client = if aterm_gpu::visual_swapchain_active() {
        "visual".to_string()
    } else {
        match client_backdrop_decline() {
            Some(reason) => format!("opaque({})", reason.token()),
            None => "opaque".to_string(),
        }
    };
    // The live OS preferences and the chrome palette they resolve to, read fresh rather than
    // from the latches, so a `chrome` read between a Settings flip and the next repaint tells
    // the OS's truth; disagreeing with the painted latches means the hook did not fire.
    // `hc_chrome` prints the three tones that differ between HC schemes (band, ink, selection).
    let high_contrast = high_contrast_active();
    // `high_contrast=false hc_chrome=…` (not `off`) is the ATERM_FORCE_HC_CHROME diagnostic;
    // the fields print separately so it does not read as an HC misdetection.
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
    // `reduce_motion` is the SPI answer, i.e. Settings ▸ Accessibility ▸ Visual
    // effects ▸ "Animation effects" turned off.
    let reduce_motion = AppRtWindows {
        probe_preferences: true,
    }
    .reduce_motion();
    let text_scale = os_text_scale();
    // The settings hook's own ground truth: broadcasts the subclass saw / wakes it posted.
    // `0/0` after a settings change means the subclass is not chained (the swap failed), the
    // one failure that is otherwise silent behind the focus-gain fallback.
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

/// The `DWMWA_SYSTEMBACKDROP_TYPE` value for a config [`BackgroundMaterial`]. `None` (the
/// default) maps to `DWMSBT_NONE`, so a stock config leaves the window a plain opaque grid.
fn backdrop_for(material: BackgroundMaterial) -> u32 {
    match material {
        BackgroundMaterial::None => DWMSBT_NONE,
        BackgroundMaterial::UnderWindow => DWMSBT_MAINWINDOW, // Mica
        BackgroundMaterial::Sidebar => DWMSBT_TABBEDWINDOW,   // Mica Alt / tabbed
        BackgroundMaterial::Hud => DWMSBT_TRANSIENTWINDOW,    // Acrylic
    }
}

// ---- Auto-theme caption resolution (Windows only) -----------------------------------
//
// `App::window_theme_for_chrome` passes `window_theme` through unchanged on every platform;
// AppKit blends a light titlebar over a dark view, but DWM paints the caption a flat system
// colour with a hard seam, so a light system theme over aterm's dark grid draws a white bar on
// a near-black terminal. Here, in the Windows arm, `Auto` resolves against the terminal
// background instead (as Windows Terminal's "Use system theme" caption follows the profile
// background), and macOS keeps receiving a raw `Auto`.
//
// The resolution lives HERE, in the Windows arm, rather than in the cross-platform
// accessor: `window_theme_for_chrome`'s pass-through semantics are upstream intent
// and macOS must keep receiving a raw `Auto`.
//
// The terminal background reaches this seam through the route the Windows arm
// ALREADY has for it — `window_set_background_color`, which every theme-publishing
// path (attach, `apply_theme_live`, the `reload_config` theme commit) calls with
// `theme.bg`. Rather than widen the `AppRt` trait with a Windows-only argument, the
// last published bg is latched here and read back when a later `window_set_appearance`
// has to resolve `Auto`. The latch shares the published background between chrome
// callbacks; both seams run on the winit main thread, so
// `Relaxed` is sufficient — the atomics buy interior mutability, not synchronization.

/// Sentinel for "no terminal background has been published yet". A theme bg is
/// `0x00RRGGBB`, so the top byte is always clear and `u32::MAX` cannot collide.
const CHROME_BG_UNKNOWN: u32 = u32::MAX;

/// The terminal theme background last handed to [`AppRtWindows::window_set_background_color`].
static CHROME_BG: AtomicU32 = AtomicU32::new(CHROME_BG_UNKNOWN);

/// The `window_theme` policy last handed to [`AppRtWindows::window_set_appearance`], encoded by
/// [`chrome_policy_code`], so the background-publishing seam can re-run the same resolution.
/// Seeded to the config default (`Auto`): attach publishes the background before the
/// appearance, so the first window's latch is one call stale, but that window is still hidden
/// (`set_visible` comes after both).
static CHROME_POLICY: AtomicU8 = AtomicU8::new(0); // 0 == Auto, the config default

/// The live Windows High Contrast state, latched by [`apply_chrome_appearance`] because the
/// per-frame guard [`verify_chrome_appearance`] resolves too and `SPI_GETHIGHCONTRAST` is a
/// syscall. Every seam that can follow an HC change runs `apply_chrome_appearance`.
static CHROME_HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);

/// The DWM backdrop last applied to the window, latched so the caption-tint resolver can honour
/// it (a solid caption colour paints over Mica/Acrylic). The effective value, not always the
/// configured one ([`CHROME_BACKDROP_REQUESTED`]); seeded to the config default.
static CHROME_BACKDROP: AtomicU32 = AtomicU32::new(DWMSBT_NONE);

/// The backdrop config asked for, before the reduced-transparency gate. Latched apart from
/// [`CHROME_BACKDROP`] because the gate is a live OS preference: with only the effective value
/// latched, turning "Transparency effects" off and on again would leave nothing to restore.
/// Seeded to the config default.
static CHROME_BACKDROP_REQUESTED: AtomicU32 = AtomicU32::new(DWMSBT_NONE);

/// Why the client-area backdrop path is not live, latched so `aterm-ctl chrome` can say it
/// (`client=opaque(hdr_glow)`) where the cause would otherwise be stderr a GUI launch discards.
/// The commonest cause: `hdr_glow` is on by default, and the scRGB EDR swapchain excludes the
/// DirectComposition visual.
///
/// First writer wins: once the visual request is never made or is withdrawn, the
/// `window_set_vibrancy` arm also sees "not active" and would overwrite the root cause with its
/// symptom. `0` = nothing declined (the visual path is live, or no material was configured).
static CLIENT_BACKDROP_DECLINE: AtomicU32 = AtomicU32::new(0);

/// The reasons [`CLIENT_BACKDROP_DECLINE`] can hold, each a short stable token for bug reports
/// and acceptance scripts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientBackdropDecline {
    /// `hdr_glow` is on: the scRGB EDR swapchain won, so the composition visual was never
    /// requested. The default-config case.
    HdrGlow = 1,
    /// The GPU renderer did not come up, so there is no per-pixel-alpha present
    /// path at all and the request was withdrawn.
    NoGpu = 2,
    /// The visual swapchain was requested and DirectComposition refused it at the
    /// first surface attach; the GPU stack was rebuilt on the opaque HWND path.
    DcompUnavailable = 3,
    /// The device was lost mid-session and the process downgraded to the CPU
    /// renderer, which has no translucent present path.
    GpuLost = 4,
    /// A material arrived after launch (a live config reload) on a process whose
    /// presentation system, an instance-level choice, was already opaque.
    NotAtLaunch = 5,
}

impl ClientBackdropDecline {
    /// The token that appears inside `client=opaque(...)`.
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

/// Record why the client-area backdrop is not live. Idempotent and first-writer-
/// wins (see [`CLIENT_BACKDROP_DECLINE`]); safe from any thread, including the
/// backend build thread.
pub(crate) fn note_client_backdrop_declined(reason: ClientBackdropDecline) {
    let _ = CLIENT_BACKDROP_DECLINE.compare_exchange(
        0,
        reason as u32,
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

/// The recorded decline, if any — the `client=opaque(...)` qualifier.
pub(crate) fn client_backdrop_decline() -> Option<ClientBackdropDecline> {
    ClientBackdropDecline::from_code(CLIENT_BACKDROP_DECLINE.load(Ordering::Relaxed))
}

/// The live Windows High Contrast state (`SPI_GETHIGHCONTRAST` → `HCF_HIGHCONTRASTON`), the one
/// reader of that preference here. `false` if the query fails.
fn high_contrast_active() -> bool {
    let mut high_contrast = HighContrastW {
        size: std::mem::size_of::<HighContrastW>() as u32,
        flags: 0,
        default_scheme: std::ptr::null_mut(),
    };
    // SAFETY: documented read-only SPI call with the required size and a valid
    // writable HIGHCONTRASTW pointer. The scheme pointer is output-only and is
    // not dereferenced or retained.
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

/// One `HKEY_CURRENT_USER` `REG_DWORD`, or `None` when absent, of another type, or unreadable.
/// Callers read `None` as the Windows default: neither value exists until the user moves the
/// switch.
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
    // SAFETY: one documented flat-C registry read. Both name buffers are
    // NUL-terminated UTF-16 and outlive the call; `RRF_RT_REG_DWORD` makes the
    // call refuse anything that is not a 4-byte DWORD (so `data` can only be
    // written as one `u32`), `cb` is its exact byte size in and out, and the
    // type out-parameter is not wanted. A non-zero return leaves `data` untouched.
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

/// One `HKEY_CURRENT_USER` `REG_BINARY` into `buf`: the bytes written, or `None`
/// when the value is absent, holds another type, does not fit, or is unreadable.
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
    // SAFETY: one documented flat-C registry read. Both name buffers are
    // NUL-terminated UTF-16 and outlive the call; `RRF_RT_REG_BINARY` refuses any
    // other type, `cb` is `buf`'s length in and the bytes written out, and a value
    // longer than `buf` fails (`ERROR_MORE_DATA`) without writing past it.
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

/// Where Explorer keeps the accent's eight shades (`AccentPalette`) and DWM the
/// accent itself (`AccentColor`, `0xAABBGGRR`).
const ACCENT_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
const ACCENT_PALETTE_VALUE: &str = "AccentPalette";
const DWM_KEY: &str = r"Software\Microsoft\Windows\DWM";
const DWM_ACCENT_VALUE: &str = "AccentColor";

/// The user's Windows accent colour as the chrome takes it
/// ([`crate::native_appearance::SystemAccent`]), from the registry for the same
/// reason as the two settings above: `UISettings.GetColorValue` is WinRT.
///
/// `AccentPalette` is eight RGBA quads, lightest first: Light3, Light2, Light1,
/// the accent, Dark1, Dark2, Dark3 (then a complement). WinUI fills accent
/// controls with Light2 on dark surfaces and Dark1 on light ones, so those are
/// the two shades (this machine: `#4cc2ff` / `#0067c0` for the default
/// `#0078d4`). Without the palette, DWM's `AccentColor` serves both sides; the
/// chrome then floors it per surface as it floors every accent. `None` (no
/// accent) only when neither value reads.
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

/// [`system_accent_read`]'s decoding, pure: the palette's Light2 and Dark1 quads,
/// else the DWM `0xAABBGGRR` accent on both sides.
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

/// The live Settings ▸ Personalization ▸ Colours ▸ "Transparency effects" switch: `true` (the
/// Windows default) when translucent surfaces are permitted.
fn transparency_effects_enabled() -> bool {
    hkcu_dword(PERSONALIZE_KEY, ENABLE_TRANSPARENCY_VALUE).is_none_or(|v| v != 0)
}

/// The live Settings ▸ Accessibility ▸ Text size slider as a ratio (the OS stores a percent;
/// 100 ⇒ `1.0`), or `1.0` when never moved. The slider reaches 225 %, but
/// [`crate::native_appearance::AppearancePreferences::normalized`] clamps to 2.0, the
/// cross-platform ceiling the consuming layouts were tuned against.
fn os_text_scale() -> f32 {
    match hkcu_dword(ACCESSIBILITY_KEY, TEXT_SCALE_FACTOR_VALUE) {
        // A percent, so anything at or below zero is not one; the upper end is left
        // to `normalized`'s clamp rather than duplicated here.
        Some(percent) if percent > 0 => percent as f32 / 100.0,
        _ => 1.0,
    }
}

/// One live system-palette entry as an RGB triple in theme byte order.
fn sys_color(index: i32) -> [u8; 3] {
    // SAFETY: a documented flat-C user32 palette lookup taking an integer index and
    // returning a COLORREF by value; no pointers are involved and an out-of-range
    // index returns zero rather than failing.
    let rgb = colorref_swap(unsafe { GetSysColor(index) });
    [
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
    ]
}

/// The chrome palette to force, given the live High-Contrast state: the five system colours
/// under an HC scheme, `None` otherwise. Off HC the system colours describe Explorer's chrome,
/// and painting the tab strip `COLOR_BTNFACE` would throw away the theme-derived band.
fn resolve_forced_chrome(high_contrast: bool) -> Option<crate::chrome_band::ForcedChrome> {
    (high_contrast || force_hc_chrome_env()).then(|| crate::chrome_band::ForcedChrome {
        window: sys_color(COLOR_WINDOW),
        window_text: sys_color(COLOR_WINDOWTEXT),
        highlight: sys_color(COLOR_HIGHLIGHT),
        highlight_text: sys_color(COLOR_HIGHLIGHTTEXT),
        btn_face: sys_color(COLOR_BTNFACE),
    })
}

/// `ATERM_FORCE_HC_CHROME=1`: force the chrome onto the OS system palette without a
/// High-Contrast scheme, so a reviewer or a check can see this path without turning High
/// Contrast on for the whole desktop. A development seam ([`aterm_types::dev_seam!`]; a shipped
/// binary never reads it), read fresh on every call.
///
/// Off High Contrast `GetSysColor` returns the ordinary desktop palette, so this exercises the
/// whole path (palette → strip, `ChromeBleed`, find bar and notice bands → strip-cache
/// invalidation → live re-resolve) but not an HC scheme's colours; the stock HC palettes are
/// asserted against `chrome_band::hc_fixtures`.
fn force_hc_chrome_env() -> bool {
    aterm_types::dev_seam!("ATERM_FORCE_HC_CHROME").is_some_and(|v| v != "0" && !v.is_empty())
}

/// Re-read High Contrast and republish the forced chrome palette; `true` when the palette moved
/// (HC turned on or off, or a switch between two HC schemes, which no accessibility preference
/// sees). Also refreshes [`CHROME_HIGH_CONTRAST`], so the palette, the caption resolver and
/// [`AppRtWindows::native_appearance_preferences`] share one `SPI_GETHIGHCONTRAST` read.
///
/// The OS accent colour is re-read here too ([`system_accent_read`]): every seam
/// that settles the OS chrome palette (startup, attach, the settings hook's
/// `WM_DWMCOLORIZATIONCOLORCHANGED` / `ImmersiveColorSet` re-sample) settles the
/// accent with it, and a move arms the same chrome-inputs latch.
pub(crate) fn resync_forced_chrome_palette() -> bool {
    let high_contrast = high_contrast_active();
    CHROME_HIGH_CONTRAST.store(high_contrast, Ordering::Relaxed);
    let accent_moved = crate::native_appearance::install_system_accent(system_accent_read());
    crate::chrome_band::install_forced_chrome(resolve_forced_chrome(high_contrast)) || accent_moved
}

/// How far ONE wheel notch travels, per the user's Windows mouse settings: the
/// platform-neutral [`crate::input::WheelNotch`], so Windows' wheel rule runs on
/// every host (`input::WheelPlatform`, session log phase 6, 6A-4).
pub(crate) use crate::input::WheelNotch;

/// Interpret a raw `SPI_GETWHEELSCROLLLINES` value. `0` is legal and means "wheel
/// scrolling off" — honoured as zero lines rather than clamped up to 1, because a
/// user who turned the wheel off means it.
fn wheel_notch_from_raw(raw: u32) -> WheelNotch {
    if raw == WHEEL_PAGESCROLL {
        WheelNotch::Page
    } else {
        WheelNotch::Lines(raw)
    }
}

/// Backstop refresh interval for [`WHEEL_NOTCH_CACHE`]: long enough that a flick's
/// worth of events shares one read, short enough that a machine whose settings hook
/// never installed still follows the slider within a few seconds.
const WHEEL_NOTCH_TTL: std::time::Duration = std::time::Duration::from_secs(3);

/// The cached `SPI_GETWHEELSCROLLLINES` answer and when it was read; module-level so the
/// settings hook can invalidate it ([`note_os_settings_resample`]).
static WHEEL_NOTCH_CACHE: Mutex<Option<(u32, std::time::Instant)>> = Mutex::new(None);

/// The user's wheel-notch distance, cached.
///
/// `SystemParametersInfoW` is cheap but not free and the wheel is a hot path, so the value is
/// cached. The settings hook invalidates it ([`note_os_settings_resample`]) when the Mouse
/// slider broadcasts `WM_SETTINGCHANGE`; the TTL is a backstop for a window whose subclass
/// failed to install.
pub(crate) fn wheel_notch_scroll() -> WheelNotch {
    let now = std::time::Instant::now();
    // A poisoned lock still holds a perfectly good number: a panic elsewhere must
    // not degrade scrolling.
    let mut slot = WHEEL_NOTCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((raw, read_at)) = *slot
        && now.duration_since(read_at) < WHEEL_NOTCH_TTL
    {
        return wheel_notch_from_raw(raw);
    }
    let mut raw: u32 = DEFAULT_WHEEL_SCROLL_LINES;
    // SAFETY: a single documented `SystemParametersInfoW` read into a local `UINT`;
    // `ui_param`/`fw_ini` are 0 (this is a get, not a set) and the out-pointer is
    // valid for the duration of the call. A zero return leaves `raw` untouched at
    // the documented default.
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

/// [`WindowTheme`] → the [`CHROME_POLICY`] byte. A tiny hand-rolled encoding because
/// `WindowTheme` is a plain config enum with no numeric repr to borrow.
fn chrome_policy_code(theme: WindowTheme) -> u8 {
    match theme {
        WindowTheme::Auto => 0,
        WindowTheme::Light => 1,
        WindowTheme::Dark => 2,
    }
}

/// The inverse of [`chrome_policy_code`]; an unknown byte cannot occur but decodes to
/// `Auto` (the config default) rather than panicking on a latch we do not control.
fn chrome_policy_from_code(code: u8) -> WindowTheme {
    match code {
        1 => WindowTheme::Light,
        2 => WindowTheme::Dark,
        _ => WindowTheme::Auto,
    }
}

/// Resolve the config `window_theme` policy into the CONCRETE caption variant to apply.
/// Explicit `Light`/`Dark` are hard overrides and pass straight through — only `Auto`
/// consults `bg`, the surface under the caption ([`caption_surface_of`]: the tab band,
/// else the terminal background), and only once one has been published (before that
/// there is nothing to match, so `Auto` stays `Auto` and the OS preference wins, exactly
/// as it did before this resolution existed).
///
/// The darkness predicate is [`crate::tab_bar::theme_is_dark`] — deliberately the SAME
/// one dark/light rule (the WCAG pole) that already drives the native toolbar strip's appearance
/// (`toolbar::set_strip_dark`). Reusing it, instead of adding a second luminance
/// function here, is what guarantees the caption and the tab strip directly beneath it
/// can never disagree about whether the window is a dark one.
///
/// Under High Contrast (`high_contrast`, from [`high_contrast_active`]) `Auto` defers to the OS:
/// the HC palette owns the caption. winit's own HC check never fires (see the HC arm of
/// [`apply_chrome_appearance`], which writes the deferral explicitly).
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

// ---- Caption tint (Windows 11) ------------------------------------------------------
//
// Immersive-dark alone still leaves the caption a SYSTEM gray (~#1F1F1F dark /
// ~#F3F3F3 light) with a visible seam against the window under it — a dark
// island on a light desktop, worse on hued schemes. `DWMWA_CAPTION_COLOR` +
// `DWMWA_TEXT_COLOR` paint the caption the colour of the SURFACE DIRECTLY UNDER
// IT ([`App::caption_surface_tone`]): the tab band's tone where the window shows a
// strip, so caption and tabs read as one chrome block over the grid (Windows
// Terminal's title bar and tab row, Edge's), else the window's live background
// (a session theme or OSC 11 included). Tinting with the terminal background
// while a lighter band sat between caption and grid drew a dark / grey / dark
// sandwich (the 2026-10-17 audit's `caption-band-sandwich`). The surface is per
// window ([`CAPTION_SURFACE`]), published before reveal and on every composed
// frame ([`note_caption_surface`]). Four deliberate gates keep this from doing
// damage:
//
//  * only `Auto` tints with the terminal colour; an explicit `window_theme = "light"/"dark"`
//    gets the stock OS caption in that variant, matching the forced tab band and native pages
//    (`App::chrome_palette_theme` → `resolve_chrome_palette`);
//  * High Contrast suppresses it: the HC palette owns the caption;
//  * an active backdrop material suppresses it: a solid caption colour paints over Mica/Acrylic;
//  * a pinned COLORREF disables DWM's unfocused treatment, so the resolver derives a dimmed
//    text tone and the redraw path re-applies on every focus flip. The fill stays the same in
//    both states (the grid does not dim on focus loss); only the title text dims, as stock DWM
//    does.

/// How far the caption title text blends toward the caption fill while unfocused: 0.45 lands
/// near DWM's own inactive-caption text (roughly #999 on white). Far stronger than the tab
/// strip's 0.15 label dim, since the strip label is content and the title duplicates identity.
const CAPTION_TEXT_UNFOCUSED_DIM: f32 = 0.45;

/// Resolve the caption tint into the exact `(DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR)`
/// attribute payloads: COLORREFs when the tint is active, the `DWMWA_COLOR_DEFAULT`
/// reset sentinel (returning the caption to DWM's palette, unfocused dimming
/// included) when any gate defers. Pure, so the whole gate ladder is testable
/// without a window.
///
/// `surface` is the colour under the caption ([`caption_surface_of`]). The ink is
/// white-on-dark / black-on-light by the SAME one dark/light rule
/// ([`crate::tab_bar::theme_is_dark`], the WCAG pole) that resolves the immersive-dark caption,
/// so the tint and the dark-mode caption buttons can never disagree about which
/// side of the split the window is on. Unfocused, the ink blends toward the
/// caption fill by [`CAPTION_TEXT_UNFOCUSED_DIM`] via the shared
/// [`crate::chrome_band::mix3`] (one blend for every chrome surface, per its doc).
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

/// The `(caption, text)` attribute pair last written per HWND, so the per-frame upkeep
/// ([`verify_chrome_appearance`]) costs a lock and a compare in the steady state. A linear Vec
/// for a handful of windows; entries are never removed, and a recycled HWND cannot mis-skip,
/// because every event seam calls [`apply_caption_tint`] with `force`.
static CAPTION_TINT_APPLIED: Mutex<Vec<(isize, u64)>> = Mutex::new(Vec::new());

/// The `(caption, text)` pair last written to `hwnd` ([`CAPTION_TINT_APPLIED`]),
/// as COLORREFs: what `aterm-ctl chrome` reports where DWM will not read the
/// attributes back (Windows 11 build 26200 answers `E_INVALIDARG` for both).
fn caption_tint_written(hwnd: isize) -> Option<(u32, u32)> {
    let applied = CAPTION_TINT_APPLIED
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    applied
        .iter()
        .find(|(h, _)| *h == hwnd)
        .map(|&(_, state)| ((state >> 32) as u32, state as u32))
}

/// The surface each window's caption sits on, `(HWND, 0x00RRGGBB)`
/// ([`App::caption_surface_tone`], published by [`note_caption_surface`]). A
/// linear Vec like [`CAPTION_TINT_APPLIED`], for the same reasons; a window with
/// no entry yet resolves from the theme background ([`CHROME_BG`]).
static CAPTION_SURFACE: Mutex<Vec<(isize, u32)>> = Mutex::new(Vec::new());

/// The surface under `hwnd`'s caption: its published [`CAPTION_SURFACE`], else
/// the last theme background ([`CHROME_BG`], [`CHROME_BG_UNKNOWN`] before any).
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

/// Publish the colour of the surface directly under `window`'s caption
/// ([`App::caption_surface_tone`]): before the window is revealed, and on every
/// composed frame. A lock and a compare in the steady state; a change (a theme
/// edit, the strip appearing or going, an OSC 11 on a strip-less window)
/// re-resolves the caption at once ([`resync_chrome_appearance`]), so the tint
/// lands in the same frame as the surface it matches.
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

/// Resolve and write the caption tint for `window`. `force` (the event seams: attach, theme
/// publish, config reload, OS flip) writes even when the cache says the state is current, to
/// heal a clobbered caption; the per-frame caller relies on the cache. On Windows 10 both
/// writes fail `E_INVALIDARG` and are dropped.
fn apply_caption_tint(window: &Window, hwnd: isize, policy: WindowTheme, force: bool) {
    let (caption, text) = caption_tint(
        policy,
        caption_surface_of(Some(hwnd)),
        CHROME_BACKDROP.load(Ordering::Relaxed),
        CHROME_HIGH_CONTRAST.load(Ordering::Relaxed),
        // winit-cached focus (a lock read, not a syscall). Focus flips reach this
        // seam because `on_focus` requests a redraw and the redraw path runs the
        // per-frame chrome upkeep.
        window.has_focus(),
    );
    let state = (u64::from(caption) << 32) | u64::from(text);
    {
        // A poisoned lock still holds a perfectly good cache: a panic elsewhere
        // must not stop caption upkeep.
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

// ---- The reduced-transparency gate on the system backdrop ---------------------------
//
// `reduced_transparency` (High Contrast, or "Transparency effects" off) gates the one place
// aterm asks the compositor for a see-through surface. DWM already drops Acrylic to a solid
// fill when transparency is off, but Mica (`DWMSBT_MAINWINDOW`) is a wallpaper tint that
// survives the switch, so only the app can honour it.

/// Windows has two independent "no transparency" sources: the Personalization ▸ Colours
/// toggle, and High Contrast (which suppresses composition effects regardless); either
/// suffices. A named rule so it can be asserted as a truth table.
fn reduced_transparency_from(high_contrast: bool, transparency_effects: bool) -> bool {
    high_contrast || !transparency_effects
}

/// The backdrop actually to apply, given what config asked for. `DWMSBT_NONE` is never gated:
/// there is no transparency to reduce.
fn effective_backdrop(requested: u32) -> u32 {
    if requested != DWMSBT_NONE
        && crate::native_appearance::current_preferences().reduced_transparency
    {
        DWMSBT_NONE
    } else {
        requested
    }
}

/// Resolve [`CHROME_BACKDROP_REQUESTED`] through [`effective_backdrop`], write it, and latch the
/// result for the caption-tint gate. Called from [`apply_chrome_appearance`] as well as
/// `window_set_vibrancy`: the config side moves on a reload, the preference side on a Settings
/// flip that reaches this file only through the settings hook. A no-op before 22H2.
fn apply_backdrop(hwnd: isize) {
    let effective = effective_backdrop(CHROME_BACKDROP_REQUESTED.load(Ordering::Relaxed));
    CHROME_BACKDROP.store(effective, Ordering::Relaxed);
    dwm_set_u32(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, effective);
}

/// Push the resolved caption variant at the window: winit's `set_theme` for the
/// uxtheme side (`SetWindowTheme` + the undocumented `WCA_USEDARKMODECOLORS`, which is
/// all vendored winit actually drives — see `crates/aterm-window/.../dark_mode.rs`) AND an
/// explicit `DWMWA_USE_IMMERSIVE_DARK_MODE` write, because winit never touches that
/// documented attribute and it is what `DwmGetWindowAttribute` — i.e. `aterm-ctl
/// chrome` — reads back as ground truth.
///
/// A resolved `Auto` (no published background yet) keeps the pre-existing behaviour:
/// winit `set_theme(None)` re-applies the OS preference and live-tracks it, and the
/// DWM attribute is left at its OS default rather than being pinned to a guess.
///
/// Idempotent, so every seam that can plausibly have been clobbered may just call it.
fn apply_chrome_appearance(window: &Window, policy: WindowTheme) {
    // Refresh the High-Contrast latch here, on the rare event-driven path, so the per-frame
    // guard resolves from an atomic instead of an `SPI_GETHIGHCONTRAST` syscall.
    let high_contrast = high_contrast_active();
    CHROME_HIGH_CONTRAST.store(high_contrast, Ordering::Relaxed);
    // The caption's side follows the surface it sits on, the same one its tint
    // takes ([`caption_surface_of`]), so fill and buttons cannot disagree.
    let resolved = resolve_chrome_theme(policy, caption_surface_of(hwnd_of(window)), high_contrast);
    {
        // `set_theme` is `SetWindowTheme`, which sends `WM_THEMECHANGED` back to this window
        // synchronously, into [`aterm_subclass_wndproc`]; the guard keeps the settings hook
        // from counting aterm's own write as an OS change (see [`watch_os_settings_message`]).
        let _no_self_wake = ChromeApplyGuard::new();
        window.set_theme(window_theme_to_winit(resolved));
    }
    let Some(hwnd) = hwnd_of(window) else {
        return;
    };
    match resolved {
        WindowTheme::Dark => dwm_set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 1),
        WindowTheme::Light => dwm_set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 0),
        // High Contrast: write the deferral EXPLICITLY, because the winit guard this
        // arm used to lean on is dead code. `set_theme(None)` was supposed to reach
        // winit's `should_use_dark_mode()` = `should_apps_use_dark_mode() &&
        // !is_high_contrast()` — but vendored winit's `is_high_contrast()` fills its
        // `HIGHCONTRASTA` with `cbSize: 0` (crates/aterm-window/.../dark_mode.rs), which
        // `SPI_GETHIGHCONTRAST` rejects (measured on this machine: returns FALSE,
        // struct untouched; the correct 16-byte size returns TRUE under an HC
        // scheme). So winit ALWAYS answers "not high contrast" and would happily
        // apply dark-mode chrome over an HC palette. The fix is deliberately
        // aterm-side, from aterm's own correct [`high_contrast_active`] reader:
        // patching winit's `cbSize` would also flip `Window::theme()` — which seeds
        // the TERMINAL PALETTE at attach — to Light under a dark HC scheme (Night
        // Sky), a worse outcome than the dead guard. What Microsoft's HC contract
        // asks of apps is "do not apply dark-mode styling; the HC palette owns
        // rendering", i.e. exactly the `false` winit's working guard would have
        // produced — so write immersive-dark OFF and let the HC system colours own
        // the caption. HONEST LIMIT: winit's uxtheme/WCA half still runs its broken
        // resolution; DWM keeps the caption bit in one place (see
        // [`verify_chrome_appearance`]'s gate-3 note), so the explicit attribute is
        // the word that sticks, and the frame guard re-asserts it if a later
        // `WM_SETTINGCHANGE` re-theme clobbers it.
        WindowTheme::Auto if high_contrast => {
            dwm_set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 0);
        }
        WindowTheme::Auto => {} // winit's set_theme(None) tracks the OS live
    }
    // Rounded corners: a Win11 top-level window is rounded by default, but state it
    // explicitly so intent survives any style change; a silent no-op (E_INVALIDARG)
    // on Windows 10.
    dwm_set_u32(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
    // The system backdrop, re-resolved through the live reduced-transparency preference (the
    // settings hook re-runs this function on a "Transparency effects" flip). Before the tint,
    // which gates on the effective backdrop.
    apply_backdrop(hwnd);
    // Caption tint: every theme publish re-runs this seam (`window_set_background_color` →
    // `resync_chrome_appearance` → here). `force`: the event seams also heal a tint clobbered
    // outside our view.
    apply_caption_tint(window, hwnd, policy, /* force= */ true);
}

/// Re-apply the caption appearance for `window` under the latched config policy: after a
/// terminal theme change, and after an OS light/dark flip, where winit's `WM_SETTINGCHANGE`
/// handler re-themes the non-client area from the OS preference (`preferred_theme` is `None`
/// for `Auto`), so our caption is re-asserted after winit's.
pub(crate) fn resync_chrome_appearance(window: &Window) {
    apply_chrome_appearance(
        window,
        chrome_policy_from_code(CHROME_POLICY.load(Ordering::Relaxed)),
    );
}

/// Does the caption need re-asserting, given the dark-mode value we want and what
/// `DWMWA_USE_IMMERSIVE_DARK_MODE` reads back? `None` (not gettable on this build) answers
/// `false`: with no evidence of a clobber, rewriting every frame is churn.
fn chrome_reassert_needed(want_dark: bool, applied: Option<u32>) -> bool {
    match applied {
        Some(v) => (v != 0) != want_dark,
        None => false,
    }
}

/// Per-frame guard that the caption on glass is still the one we resolved.
///
/// WHY A FRAME PATH AND NOT AN EVENT. winit's `WM_SETTINGCHANGE` handler
/// (`crates/aterm-window/.../event_loop.rs`) calls `try_theme()` on EVERY such message when
/// `preferred_theme` is `None` — which for `Auto` it always is — but emits
/// `ThemeChanged` only when the OS-resolved theme actually MOVED. Windows broadcasts
/// `WM_SETTINGCHANGE` for a long list of unrelated things (`SPI_SETWORKAREA` on monitor
/// hotplug or a taskbar change, `"Environment"`, accent colour / `"ImmersiveColorSet"`,
/// accessibility and input settings, an Explorer restart, any app calling
/// `SystemParametersInfoW` with `SPIF_SENDCHANGE`), and an OS light/dark switch itself
/// usually broadcasts more than once. Every one of those re-applies the OS preference to
/// the non-client area; all but the first produce NO event, so an event-gated re-assert
/// is not merely late — it never runs, and the OS caption is the last word. That is the
/// reported defect (white caption over a near-black grid) coming back minutes later with
/// nothing in the log to explain it.
///
/// It never requests a redraw. First the caption-tint upkeep ([`apply_caption_tint`] without
/// `force`), which is how a focus flip reaches the caption (`on_focus` requests a redraw); it
/// writes only when the desired state moved. Then the immersive-dark ladder:
///  1. a resolved `Auto` outside High Contrast means the OS owns the caption: return. Under HC
///     the deferral is an explicit immersive-dark off, defended like any override;
///  2. `window.theme()` (a lock read, refreshed by winit on every `WM_SETTINGCHANGE`) agreeing
///     with our resolution means a clobber writes what we want: return;
///  3. only while overriding the desktop does a frame pay one `DwmGetWindowAttribute`, and only
///     a disagreement writes.
///
/// Gate 3 sees the clobber only if DWM keeps `DWMWA_USE_IMMERSIVE_DARK_MODE` and winit's
/// `WCA_USEDARKMODECOLORS` in one per-window bit; otherwise it costs a read and cures nothing.
pub(crate) fn verify_chrome_appearance(window: &Window) {
    // `hwnd_of` reads winit's stored handle, not a syscall.
    let Some(hwnd) = hwnd_of(window) else {
        return;
    };
    let policy = chrome_policy_from_code(CHROME_POLICY.load(Ordering::Relaxed));
    let high_contrast = CHROME_HIGH_CONTRAST.load(Ordering::Relaxed);
    // Caption-tint upkeep first and unconditionally: focus flips arrive on this path (see the
    // fn doc), and the tint's gates are independent of the ladder's early returns below.
    apply_caption_tint(window, hwnd, policy, /* force= */ false);
    let want_dark =
        match resolve_chrome_theme(policy, caption_surface_of(Some(hwnd)), high_contrast) {
            WindowTheme::Dark => true,
            WindowTheme::Light => false,
            // The explicit High-Contrast deferral is an override like any other.
            WindowTheme::Auto if high_contrast => false,
            WindowTheme::Auto => return, // the OS owns it; winit's re-theme is the answer
        };
    if matches!(window.theme(), Some(aterm_window::window::Theme::Dark)) == want_dark {
        return; // OS preference already agrees — a clobber cannot do damage
    }
    if chrome_reassert_needed(want_dark, dwm_get_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE)) {
        apply_chrome_appearance(window, policy);
    }
}

// ---- The aterm window subclass: end-session + the OS-settings hook ------------------------
//
// One subclass, two consumers: `docs/NATIVE_WINDOWS_DESIGN.md` §3.2 forbids stacking
// `SetWindowLongPtrW` subclasses (a second displaces the first and the chain order depends on
// install order). `WM_SETTINGCHANGE` is broadcast and `WM_QUERYENDSESSION` sent; Win32 delivers
// both by calling the window procedure from inside the thread's message-retrieval call, so
// neither enters the `GetMessage` stream winit's `with_msg_hook` sees.
//
// The settings lane watches:
//   * `WM_SETTINGCHANGE`: the SPI-backed prefs (animations = Reduce Motion, High Contrast,
//     wheel scroll lines) and shell/theme notifications ("ImmersiveColorSet", …);
//   * `WM_SYSCOLORCHANGE`: the system palette the forced HC chrome is built from; switching
//     between two HC schemes changes no accessibility preference, so only this sees it;
//   * `WM_THEMECHANGED`: a visual-style change, including entering or leaving High Contrast;
//   * `WM_DWMCOLORIZATIONCOLORCHANGED`: the accent colour moved.
//
// These arrive in bursts (one light/dark flip delivers several `WM_SETTINGCHANGE`s and a
// `WM_THEMECHANGED`), so the proc does no work: it flips one atomic and posts one `Wake` on the
// 0→1 edge, and the flag clears when the loop re-resolves (`App::resample_os_preferences` →
// `note_os_settings_resample`). It does not filter by `lParam` section: the sections for
// "Transparency effects" and the text scale are undocumented, and a false positive costs five
// cheap reads and no repaint. The proc re-enters while `run_app` holds `&mut App` on this
// thread, so it may touch only statics; `send_event` is a `PostMessage`, safe from a wndproc.
//
// ---- End-session persistence (WM_QUERYENDSESSION / WM_ENDSESSION) -------------------------
//
// A Windows shutdown, restart or sign-out sends WM_QUERYENDSESSION then WM_ENDSESSION to every
// top-level window and then terminates the process: `run_app` never returns, so the graceful
// `restore::write` never runs. The handler writes the session instead, from a layout snapshot
// the event loop publishes every couple of seconds (`set_end_session_snapshot` from
// `about_to_wait`, behind `end_session_snapshot_due`), because `run_app` holds the live `App`.
//
// `SetWindowLongPtrW(GWLP_WNDPROC)`, not comctl32's `SetWindowSubclass`: importing any
// comctl32 export makes the loader resolve it at start against the activation context, and
// only the shipped `aterm.exe` carries the v6 manifest; test executables resolve classic 5.82.
// (`TaskDialogIndirect` avoids the same trap through GetProcAddress; see lib.rs `win32`.)
//
// ---- End-session persistence (WM_QUERYENDSESSION / WM_ENDSESSION) -------------------
//
// A Windows shutdown / restart / sign-out sends WM_QUERYENDSESSION and then
// WM_ENDSESSION to every top-level window and then TERMINATES the process — winit's
// `run_app` never returns, so the graceful-exit `restore::write` (lib.rs, after the
// event loop) never runs and every window, tab, split and cwd was silently discarded.
// macOS covers the same gesture through its `terminate:` handler; Windows had nothing.
//
// WHY A WNDPROC SUBCLASS AND NOT AN EVENT. WM_QUERYENDSESSION is a SENT message:
// CSRSS delivers it straight to the window procedure while the thread sits inside
// `GetMessage`, so it NEVER appears in the message stream winit's `with_msg_hook`
// inspects (the same delivery asymmetry `verify_chrome_appearance` documents for the
// broadcast `WM_SETTINGCHANGE`), and the §3.2 vendored-winit message hook that would
// carry it as a typed event is still unbuilt. Chaining the window procedure is the
// one seam that sees a sent message without patching vendored winit.
//
// WHY `SetWindowLongPtrW(GWLP_WNDPROC)` AND NOT comctl32's `SetWindowSubclass`. The
// comctl32 API is the tidier subclass — but importing ANY comctl32 export makes the
// loader resolve comctl32 at process start against the binary's activation context,
// and only the shipped `aterm.exe` carries the v6 side-by-side manifest; cargo TEST
// executables (and a resource-less dev build) resolve classic 5.82, and pinning test
// binaries' loadability to that DLL's export surface is a risk with zero upside when
// plain user32 chaining does the same job. (`TaskDialogIndirect` dodges the same trap
// via GetProcAddress — see lib.rs `win32`.)
//
// WHY THE HANDLER WRITES A PRE-CAPTURED SNAPSHOT instead of asking the live `App`.
// The subclass proc re-enters while `run_app` still holds `&mut App` on this very
// thread (the call stack is run_app → GetMessage → sent-message dispatch → here), so
// touching `App` from the handler would require aliasing that unique borrow — UB by
// construction, however briefly winit itself flirts with the same shape. Instead the
// event loop PUBLISHES a serializable layout snapshot every couple of seconds
// (`set_end_session_snapshot`, driven from `about_to_wait` by
// `App::end_session_snapshot`, whose cadence is `crash_journal::EndSessionCadence`),
// and the handler only does file I/O on data it owns: bounded staleness (≤
// `crash_journal::END_SESSION_SNAPSHOT_TTL`) against a shutdown that would otherwise
// lose EVERYTHING. A tab's standing words about typing (its Autopilot word and
// timer, its holds) are never stale: a change is published on the loop turn that
// made it, so a Land, Off or hold set just before a logoff is
// what the reopened tab carries.
//
// HONEST LIMITS. (1) The layout is up to ~2 s stale — a tab opened in the final
// second before "Restart now" may be missing; every prior window/tab/cwd survives.
// (2) A shutdown the user then CANCELS leaves session.toml on disk while aterm keeps
// running: harmless for this instance (the graceful-exit write later overwrites it),
// and at worst a SECOND instance launched before that exit restores a copy of the
// layout — rare, self-correcting, and strictly better than the data loss this block
// removes. (3) A hard crash gets no WM_ENDSESSION; that lane is the crash-marker
// banner (`logging::take_crash_evidence`) + `RegisterApplicationRestart`, not this.

/// The last layout snapshot the event loop published, plus its generation: the only data the
/// end-session handler touches. `None` until the first publish, or with session restore off.
static END_SESSION_SNAPSHOT: Mutex<Option<(crate::restore::RestoreManifest, u64)>> =
    Mutex::new(None);

/// Monotonic generation of the published snapshot; [`END_SESSION_FLUSHED`] records the
/// generation last written, so the per-window WM_QUERYENDSESSION + WM_ENDSESSION deliveries cost
/// one durable write.
static END_SESSION_GEN: AtomicU64 = AtomicU64::new(0);
static END_SESSION_FLUSHED: AtomicU64 = AtomicU64::new(0);

/// hwnd → the wndproc we displaced (winit's), for `CallWindowProcW` chaining. A
/// `Vec` because the window count is a handful and `Vec::new` is const (no lazy
/// init inside a wndproc). Entries are dropped on WM_NCDESTROY.
static PREV_WNDPROC: Mutex<Vec<(isize, isize)>> = Mutex::new(Vec::new());

/// `GWLP_WNDPROC`.
const GWLP_WNDPROC: i32 = -4;
const WM_QUERYENDSESSION: u32 = 0x0011;
const WM_ENDSESSION: u32 = 0x0016;
const WM_NCDESTROY: u32 = 0x0082;
const WM_GETOBJECT: u32 = 0x003D;
/// `WM_SYSCOLORCHANGE`: the system palette (the forced HC chrome's input) moved.
const WM_SYSCOLORCHANGE: u32 = 0x0015;
/// `WM_SETTINGCHANGE` (a.k.a. `WM_WININICHANGE`) — an SPI-backed or shell setting
/// moved. Broadcast, hence invisible to winit's `GetMessage` hook.
const WM_SETTINGCHANGE: u32 = 0x001A;
/// `WM_THEMECHANGED` — a visual-style swap, which entering or leaving High Contrast
/// also is.
const WM_THEMECHANGED: u32 = 0x031A;
/// `WM_DWMCOLORIZATIONCOLORCHANGED` — the DWM accent / colorization colour moved.
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;

/// Does this message mean an OS appearance or accessibility setting may have moved? The
/// watched messages, in one place.
fn is_os_settings_message(msg: u32) -> bool {
    matches!(
        msg,
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED
    )
}

thread_local! {
    /// Nesting depth of aterm's own chrome writes on this thread. Thread-local because
    /// [`apply_chrome_appearance`] and [`aterm_subclass_wndproc`] both run on the winit main
    /// thread (and a test can hold the guard without racing the suite); a depth so nesting
    /// cannot release the guard early.
    static CHROME_APPLY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Held for the duration of a chrome write aterm itself performs; see
/// [`watch_os_settings_message`] for what it suppresses and why.
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

/// Is aterm itself mid-chrome-write on this thread?
fn applying_chrome() -> bool {
    CHROME_APPLY_DEPTH.with(std::cell::Cell::get) > 0
}

/// Should the subclass treat `msg` as an OS settings broadcast right now?
///
/// THE HOOK MUST NOT FEED ITSELF. [`apply_chrome_appearance`] calls winit's
/// `Window::set_theme`, which is `try_theme` → `SetWindowTheme(hwnd, …)`
/// UNCONDITIONALLY (`crates/aterm-window/src/platform_impl/windows/dark_mode.rs`), and
/// `SetWindowTheme` sends `WM_THEMECHANGED` straight back to that HWND — which is on
/// the watch list. Because [`note_os_settings_resample`] clears the coalescer at the
/// TOP of the re-sample (deliberately, so a racing trailing broadcast is not
/// swallowed), that self-induced message arrived with the flag already re-armed and
/// posted a SECOND `Wake`. Every theme publish, config reload, `window_set_vibrancy`
/// and caption repair therefore cost an extra event-loop turn and — before the
/// repair frame was scoped — a full repaint of every window. It terminated after one
/// extra pass (nothing moved, so no second chrome write), but "one Wake per burst",
/// the property this hook is built to provide, was simply not true.
///
/// Not chosen: dropping `WM_THEMECHANGED` from the list (it is the one message a bare
/// visual-style swap sends), caching in winit's `try_theme` (a real `WM_THEMECHANGED` asks for
/// re-application), re-arming at the end of the re-sample (that swallows a trailing broadcast).
fn watch_os_settings_message(msg: u32) -> bool {
    is_os_settings_message(msg) && !(msg == WM_THEMECHANGED && applying_chrome())
}

/// The event-loop proxy the subclass posts through, published at window attach.
/// A `Mutex<Option<_>>` rather than a `OnceLock` because the wndproc may run
/// before any window has attached (the proc is installed mid-attach) and must
/// then simply drop the notification — there is nothing yet whose appearance
/// could be stale.
static SETTINGS_PROXY: Mutex<Option<EventLoopProxy<Wake>>> = Mutex::new(None);

/// `true` while a settings `Wake` is posted but not yet serviced: the coalescer. Cleared by
/// [`note_os_settings_resample`] when the loop re-resolves.
static SETTINGS_WAKE_PENDING: AtomicBool = AtomicBool::new(false);

/// Settings broadcasts the subclass has seen, and `Wake`s it posted, reported by
/// `aterm-ctl chrome` as `os_settings=seen/woke`. The ratio is the property: one Settings flip
/// moves `seen` by several and `woke` by one. Any process can
/// `SendMessageTimeout(HWND_BROADCAST, WM_SETTINGCHANGE, …)` and watch `seen` move, which
/// proves the subclass is chained.
static SETTINGS_MSGS_SEEN: AtomicU64 = AtomicU64::new(0);
static SETTINGS_WAKES_POSTED: AtomicU64 = AtomicU64::new(0);

/// Post one settings `Wake`, or do nothing if one is already in flight.
///
/// Runs inside the window procedure, so it touches only statics and a
/// `PostMessage`-backed proxy send. On a failed send (the loop is gone) the flag is
/// released again so a later broadcast can retry rather than latching the app into
/// permanently ignoring settings changes.
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

/// The coalescing rule: the first watched message of a burst claims the pending flag and posts;
/// every later one finds it set. Takes the flag by reference so a test can drive a whole burst
/// against its own flag (`one_settings_burst_costs_exactly_one_resample`).
fn claim_os_settings_wake(pending: &AtomicBool) -> bool {
    !pending.swap(true, Ordering::AcqRel)
}

/// `(broadcasts seen, wakes posted)` for the `chrome` introspection row.
fn os_settings_counters() -> (u64, u64) {
    (
        SETTINGS_MSGS_SEEN.load(Ordering::Relaxed),
        SETTINGS_WAKES_POSTED.load(Ordering::Relaxed),
    )
}

/// The event loop is about to re-resolve the OS preferences: re-arm the coalescer so the next
/// broadcast posts again. Called from `App::resample_os_preferences` before it reads anything,
/// so a broadcast racing the read (a trailing "ImmersiveColorSet" lands after the setting
/// changed) posts a second `Wake` rather than being swallowed.
pub(crate) fn note_os_settings_resample() {
    SETTINGS_WAKE_PENDING.store(false, Ordering::Release);
    // The wheel-notch distance is SPI-backed too: drop its cache so the next notch re-reads
    // (not re-read here: the wheel may go untouched for hours).
    *WHEEL_NOTCH_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Publish the layout snapshot the end-session handler may write ([`Some`]), or retract it
/// ([`None`]: session restore is off, so an end-session must not write a manifest).
pub(crate) fn set_end_session_snapshot(manifest: Option<crate::restore::RestoreManifest>) {
    let mut slot = END_SESSION_SNAPSHOT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *slot = manifest.map(|m| (m, END_SESSION_GEN.fetch_add(1, Ordering::Relaxed) + 1));
}

/// Durably write the published snapshot, once per generation, skipping empty layouts (the
/// graceful-exit path applies the same `is_empty` gate). Runs inside the wndproc, so it touches
/// only the statics above and the filesystem.
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

/// The chained window procedure — the ONE aterm subclass (§3.2 forbids a second).
/// Serve UIA root-object requests, persist the session on end-session, coalesce
/// OS-settings broadcasts into a single `Wake`, and forward other messages
/// (including end-session and settings) to winit's original proc.
///
/// WM_QUERYENDSESSION wants TRUE, which `DefWindowProc` returns (winit has no arm); the manifest
/// is written before forwarding so the answer never races the save. WM_ENDSESSION with
/// `wparam == TRUE` is the hard flush for a critical shutdown that skips the query (otherwise
/// the generation guard makes it a no-op). Settings messages are claimed before forwarding, so
/// winit's `ThemeChanged` and our `Wake` cannot both schedule a re-resolve for one flip.
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
    // [`watch_os_settings_message`], not the bare predicate: a `WM_THEMECHANGED` that
    // aterm's own `SetWindowTheme` just provoked is not news from the OS.
    if watch_os_settings_message(msg) {
        queue_os_settings_wake();
    }
    // SAFETY: `prev` is the exact WNDPROC value `SetWindowLongPtrW` displaced for
    // this hwnd (CallWindowProcW is the documented way to invoke it, handling the
    // A/W thunk case); the `None` arm (unreachable in practice — install stores
    // the pair before any message can arrive) degrades to DefWindowProcW rather
    // than dropping the message.
    let result = match prev {
        Some(prev) => unsafe { CallWindowProcW(prev, hwnd, msg, wparam, lparam) },
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    // Window gone (forwarded first — winit's own WM_NCDESTROY cleanup must run
    // with the chain intact): drop the chain entry.
    if msg == WM_NCDESTROY {
        aterm_a11y::uia::window_destroyed(hwnd);
        PREV_WNDPROC
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|&(h, _)| h != hwnd);
    }
    result
}

/// Chain [`aterm_subclass_wndproc`] in front of winit's window procedure for
/// `window`, and publish `proxy` as the route the settings lane posts through.
/// Idempotent per window (keyed on the hwnd), so any attach-time seam may call it.
/// Holding the map lock across `SetWindowLongPtrW` is deadlock-free: a GWLP_WNDPROC
/// swap sends no messages, and sent-message dispatch (the only path into the
/// wndproc) happens only inside message-retrieval calls, none of which occur under
/// this lock.
///
/// The proxy is published once (first writer wins: one event loop per process) and before the
/// HWND bail-out, so the static is warm even for a window with no HWND yet or a failed swap.
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
    // SAFETY: `hwnd` is a live winit window owned by this thread (the swap must
    // happen on the window's thread, and attach runs there); the new value is a
    // valid `extern "system"` WNDPROC for the window's lifetime (this module is
    // in the exe — it cannot unload). A zero return means the swap failed (the
    // window keeps winit's proc verbatim) and nothing is recorded.
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

/// The native Windows application-runtime. Decorates the winit-owned `HWND` with
/// DWM chrome (immersive dark via winit + rounded corners + system backdrop) and
/// wires the EDR/reduce-motion queries; every other method delegates to the shared
/// cross-platform modules.
pub(crate) struct AppRtWindows {
    pub(crate) probe_preferences: bool,
}

impl AppRt for AppRtWindows {
    /// Paint the window-class background brush the theme background. winit registers its class
    /// with a NULL `hbrBackground`, so between `set_visible` and the first present the client
    /// area erases to white, a flash on every dark launch; a theme-bg `HBRUSH` makes
    /// `WM_ERASEBKGND` paint theme colour. Class-wide (all windows share the theme) and
    /// repeat-safe: the previous brush we installed is deleted after each swap.
    fn window_set_background_color(&self, window: &Window, bg: u32) {
        // Latch the background for the caption resolver before any bail-out: this is the
        // Windows arm's only route to the theme bg, and the next window to realize resolves
        // from it. Masked to 24 bits so it can never equal `CHROME_BG_UNKNOWN`.
        CHROME_BG.store(bg & 0x00FF_FFFF, Ordering::Relaxed);
        // Every publisher of this value is a "background may have changed" event, so re-run
        // the `Auto` caption resolution: a dark→light edit repaints the caption too.
        resync_chrome_appearance(window);
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        let colorref = colorref_swap(bg);
        // SAFETY: two documented leaf calls. The window class owns the created brush after
        // the swap (the OS uses it for every later erase); the previously installed brush
        // (ours, from an earlier theme apply) is deleted exactly once here. A zero previous
        // value is winit's null brush, skipped.
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

    /// Force the class-brush themed erase onto glass now: during the early-reveal join the
    /// event-loop thread processes no messages, so the shown window would stay unpainted.
    /// `RDW_ERASENOW` delivers `WM_ERASEBKGND` synchronously and `DefWindowProc` fills with the
    /// brush [`Self::window_set_background_color`] installed, which must run first. No HWND is a
    /// no-op.
    fn window_flush_backdrop(&self, window: &Window) {
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        // SAFETY: one documented leaf call; a null rect + null region mean the
        // entire client area, and the HWND is live (we hold the winit Window).
        unsafe {
            let _ = RedrawWindow(
                hwnd,
                std::ptr::null(),
                0,
                RDW_INVALIDATE | RDW_ERASE | RDW_ERASENOW,
            );
        }
    }

    /// Apply the native title-bar appearance ([`apply_chrome_appearance`]): winit's
    /// `set_theme`, an explicit `DWMWA_USE_IMMERSIVE_DARK_MODE`, and the rounded-corner
    /// preference. `Light`/`Dark` are hard overrides; `Auto` resolves against the terminal
    /// background (see "Auto-theme caption resolution" above).
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        // Latch the policy so the background-publish and OS-flip seams can re-run this
        // same resolution later without the caller re-supplying it.
        CHROME_POLICY.store(chrome_policy_code(theme), Ordering::Relaxed);
        apply_chrome_appearance(window, theme);
    }

    /// No-op at the window level: the swapchain colour space (scRGB for the HDR present) is
    /// tagged in `aterm-gpu` at surface creation. Reports the effective source coordinates: the
    /// renderer confirmed scRGB before exposing an HDR surface, and Display-P3 stays sRGB here.
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

    /// A no-op, because nothing here can change it. Windows has the same stale
    /// frame macOS anchors: DWM keeps the last flip-model buffer presented to the
    /// HWND and, between a resize and the first present at the new size, scales
    /// it to the new client area. The scaling is the swapchain's
    /// `DXGI_SWAP_CHAIN_DESC1::Scaling`, fixed when wgpu-hal creates the
    /// swapchain, and wgpu-hal 29 always passes `DXGI_SCALING_STRETCH` (so the
    /// frame stretches); the top-left anchor is `DXGI_SCALING_NONE`, which
    /// Windows Terminal's HWND swapchain uses, and needs a wgpu-hal change.
    /// What aterm does instead is shorten the stretched interval: `Resized`
    /// reconfigures the swapchain at once, ahead of the throttled reflow, and
    /// `metrics`' `resize_present` fields time it.
    fn window_anchor_surface_top_left(&self, _window: &Window) {}

    /// No layer to read: the swapchain's scaling mode is not exposed back
    /// through wgpu, and reporting the constant wgpu-hal passes would not be a
    /// reading (see [`Self::window_anchor_surface_top_left`]).
    fn window_surface_presentation(&self, _window: &Window) -> Option<(String, f64, bool)> {
        None
    }

    /// Install / update / remove the DWM system backdrop (`DWMWA_SYSTEMBACKDROP_TYPE`):
    /// `background_material` maps to Mica (`UnderWindow`), Mica Alt (`Sidebar`) or Acrylic
    /// (`Hud`), and `None` (the default) to `DWMSBT_NONE`.
    ///
    /// Where the material shows depends on the client presentation path (`aterm-ctl chrome` →
    /// `client=visual | opaque | opaque(<reason>)`, the reason a [`ClientBackdropDecline`]
    /// token; a bare `opaque` means no material was configured):
    ///   * `client=opaque` (the default HWND swapchain): the caption only. The client area,
    ///     padding included, is the framebuffer, presented opaque.
    ///   * `client=visual` (material configured at launch, GPU renderer): the caption and the
    ///     client-area margins (padding gutters, chrome-bleed strip), whose per-pixel alpha
    ///     blends through the DirectComposition visual. The grid body stays opaque.
    ///
    /// `translucent` is not consulted: the alpha lives in the presented frame. Reduced
    /// transparency (High Contrast, or "Transparency effects" off) suppresses the material where
    /// the attribute is written ([`apply_backdrop`]); this method only latches the request.
    /// `cfg_backdrop=` is what config asked for, `backdrop=` what DWM has. A no-op before 22H2.
    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: BackgroundMaterial,
        _translucent: bool,
        _bg: u32,
    ) {
        // Latch the request before anything can bail out, so config's answer is on record even
        // for a window this call cannot reach. `apply_backdrop` (via `resync_chrome_appearance`
        // below) resolves it against the live reduced-transparency preference and latches the
        // effective `CHROME_BACKDROP`.
        CHROME_BACKDROP_REQUESTED.store(backdrop_for(material), Ordering::Relaxed);
        // A material on a window whose client path is the plain HWND swapchain can style only
        // the caption: the visual swapchain is chosen at launch. Warn once, naming the fix; when
        // the visual request was withdrawn this run (GPU init failed, DirectComposition
        // unavailable, device lost), that arm already printed the cause, so point at it rather
        // than advise a relaunch. Gated on the effective backdrop: under reduced transparency
        // there is no material on the window at all.
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
                    // The withdrawing arm already latched the cause (`no_gpu` /
                    // `dcomp_unavailable` / `gpu_lost`) and queued its own notice.
                } else {
                    crate::logging::stderr_line!(
                        "aterm-gui: background_material is styling the caption only \
                         (client=opaque): the client-area backdrop engages when the material \
                         is set at launch with the GPU renderer and hdr_glow off — restart \
                         aterm to see it in the padding"
                    );
                    // This arm is the live-reload path: editing `background_material` into a
                    // running aterm changes only the caption, with no console. Queue the notice
                    // only when nothing has explained the shortfall: at launch this arm also
                    // fires behind an `hdr_glow` decline, which must stay the one cause shown.
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
        // A material change flips the tint gate both ways (`none` → Mica clears the tint;
        // Mica → `none` restores it), so re-run the caption resolution under the latched policy.
        resync_chrome_appearance(window);
    }

    /// The work area of `window`'s monitor (`GetMonitorInfoW`'s `rcWork`: the display minus the
    /// taskbar and other appbars) in logical points by the window's scale factor, the divisor
    /// winit applies to `outer_position()`. `MONITOR_DEFAULTTONEAREST` keeps a window half
    /// off-screen answering with the monitor it is mostly on.
    fn window_work_area_pts(&self, window: &Window) -> Option<crate::app_window::WorkAreaPts> {
        let hwnd = hwnd_of(window)?;
        let mut info = MonitorInfo {
            size: u32::try_from(std::mem::size_of::<MonitorInfo>()).ok()?,
            monitor: Rect::default(),
            work: Rect::default(),
            flags: 0,
        };
        // SAFETY: `MonitorFromWindow` is a lookup on a live HWND that returns a
        // borrowed HMONITOR (nothing to free) and a null handle only if the
        // window is gone; `GetMonitorInfoW` fills the stack local whose `size`
        // was pre-set as it documents. No pointer outlives the call.
        let ok = unsafe {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            monitor != 0 && GetMonitorInfoW(monitor, &raw mut info) != 0
        };
        if !ok {
            return None;
        }
        work_area_pts_from_rect(&info.work, window.scale_factor())
    }

    /// Un-minimize and raise this window for an `attach` forward: `SW_RESTORE` first (a
    /// minimized window cannot take the foreground), then `SetForegroundWindow`, whose
    /// cross-process refusal the forwarding `aterm.exe` lifted with
    /// `AllowSetForegroundWindow(our pid)`. Without that grant Windows flashes the taskbar
    /// rather than stealing focus, as it should.
    fn window_bring_to_front(&self, window: &Window) -> bool {
        let Some(hwnd) = hwnd_of(window) else {
            return false;
        };
        // SAFETY: three documented one-shot calls on a live HWND owned by this
        // process's UI thread, which is the thread we are on.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd) != 0
        }
    }

    /// The panel's EDR headroom for `window`'s monitor when Windows HDR is on
    /// (`MaxLuminance / SDR-white`), or `1.0` (no headroom, so the aurora pass is inert) when
    /// HDR is off or any query fails. See [`crate::hdr_win`].
    fn screen_edr_max(&self, window: &Window) -> f32 {
        crate::hdr_win::edr_max(window)
    }

    /// With Windows HDR on, the monitor's headroom; `1.0` off HDR.
    fn screen_edr_potential(&self, window: &Window) -> f32 {
        crate::hdr_win::edr_max(window)
    }

    /// The display's reference-white scale (`SDR-white / 80`) for the scRGB EDR
    /// present, so the grid renders at the desktop's white level rather than 80 nits;
    /// `1.0` off HDR. See [`crate::hdr_win`].
    fn screen_sdr_white_scale(&self, window: &Window) -> f32 {
        crate::hdr_win::sdr_white_scale(window)
    }

    /// Spawn the shared notification-delivery thread (the balloon-tip sink lives in
    /// `notify.rs`, driven off this channel) — identical to every other target.
    fn send_notification_init(
        &self,
        suppress: Arc<Mutex<HashSet<u64>>>,
        silent: Arc<AtomicBool>,
        own_alerts: Arc<AtomicBool>,
        center: crate::notice_center::NoticeCenter,
    ) -> SyncSender<NotifyMsg> {
        crate::notify::spawn_delivery(suppress, silent, own_alerts, center)
    }

    /// Delegate to the shared menu module: `None` on Windows by design
    /// (docs/WINDOWS_PARAGON_DESIGN.md rejects a native menu bar; the own-rendered command
    /// palette and keybindings are the command surface).
    fn install_menu(&self, proxy: &EventLoopProxy<Wake>) -> Option<menu::MenuHandle> {
        menu::install(proxy)
    }

    /// Delegate to the shared toolbar module, and chain the aterm wndproc subclass here: this
    /// is the one seam reached once per window at attach with both the `&Window` and the
    /// `EventLoopProxy` the settings lane posts through.
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
            pill,
        } = tabs;
        toolbar::set_window_tabs(handle, titles, ids, metadata, tooltips, ext, active, pill);
    }

    fn read_toolbar_chrome(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        toolbar::read_tab_chrome(handle)
    }

    /// The native close/quit confirmation: a `TaskDialogIndirect` dialog whose
    /// affirmative button carries the prompt's REAL verb ("Close"/"Quit") with a
    /// stock Cancel — the custom labelling a fixed `MessageBoxW` Yes/No could not
    /// deliver, which is why this stayed a stub until the comctl32 v6 manifest
    /// shipped. Buttons mirror the macOS `NSAlert`: proceed is the Return
    /// default, Escape cancels. `None` (no v6 comctl32 in this process — a test
    /// binary or resource-less dev build — or a failed dialog) falls back to the
    /// caller's in-window confirm banner, exactly as before.
    ///
    /// Note the POLICY is not this method's: `quit_safety::confirm_prompt_windows`
    /// decides Windows prompts per the WT convention (multiple tabs or busy —
    /// never a single idle tab), so an idle one-tab Alt+F4 never even reaches here.
    fn confirm(&self, title: &str, body: &str, proceed_label: &str) -> Option<bool> {
        crate::win32::confirm_proceed_cancel(title, body, proceed_label)
    }

    /// A no-op: a user-driven close is confirmed at winit `CloseRequested`, and the OS-driven
    /// end (shutdown, restart, sign-out), which macOS's `terminate:` seam covers, arrives as a
    /// sent `WM_QUERYENDSESSION` that the end-session subclass answers by saving the session
    /// rather than prompting: blocking a shutdown on a dialog is hostile.
    fn install_quit_confirm(&self) {}

    /// The OS Reduce-Motion preference, read from `SPI_GETCLIENTAREAANIMATION` (the "Show
    /// animations in Windows" master switch): animations off ⇒ reduce motion. Feeds
    /// [`crate::motion::MotionPolicy`]; `false` (do not reduce) if the query fails.
    fn reduce_motion(&self) -> bool {
        if !self.probe_preferences {
            return false;
        }
        let mut animations_enabled: i32 = 1;
        // SAFETY: a single documented `SystemParametersInfoW` read into a local `BOOL`;
        // `ui_param`/`fw_ini` are 0 (no set), and the out-pointer is valid for the call.
        // Non-zero return = success; the `BOOL` is true when client-area animations are on.
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

    /// The three OS appearance preferences the shared native-UI palette conditions on, read
    /// live:
    ///
    /// Two of the three used to be fictions rather than merely stale —
    /// `reduced_transparency` was set to `high_contrast` and `text_scale` was
    /// hardcoded to `1.0`, so the real "Transparency effects" switch and the real
    /// `TextScaleFactor` were never read at all. They are now:
    ///
    /// Publishing the forced chrome palette here lets every `install_preferences` seam (attach,
    /// startup, settings preview) also settle the High-Contrast chrome.
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
            // Windows has no colour-independence setting of its own.
            differentiate_without_color: false,
            text_scale: os_text_scale(),
        }
    }

    /// `None`: `ReduceMotionObserver` models an object kept alive for a subscription (macOS's
    /// workspace observer). Windows delivers the change as a broadcast window message to the
    /// aterm subclass installed at attach, which coalesces the burst into one
    /// `Wake::ReduceMotionChanged`; `App::resample_os_preferences` then re-reads
    /// ([`resync_forced_chrome_palette`], `reduce_motion`, `native_appearance_preferences`,
    /// [`resync_chrome_appearance`]) and repaints only if a sampled fact moved. The
    /// `ThemeChanged` and focus-gain seams stay as a backstop before the first `install_toolbar`.
    fn observe_reduce_motion(&self, _proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver> {
        None
    }
}

/// Photograph the window — caption CHROME (dark/light title bar, buttons) plus the
/// GPU-rendered grid — to tightly-packed top-down BGRX via
/// `PrintWindow(PW_RENDERFULLCONTENT)`, as `GetDIBits` writes it: the fourth byte is
/// undefined, and the encode worker makes it opaque RGBA. This is the robust capture:
/// it renders the window itself (so it works even occluded / off-screen, unlike a
/// screen grab) and `PW_RENDERFULLCONTENT` pulls in the DirectComposition GPU
/// surface. Returns `(bgrx, width, height)`; serves the Windows `window` introspection verb for the
/// ordinary `background_material = "none"` case. This is explicitly window-local,
/// square, and opaque: DWM's rounded-corner clip and Mica/Acrylic backdrop exist only
/// in compositor output. The caller therefore refuses non-`none` materials instead of
/// mislabeling this bitmap as the native on-glass result.
pub(crate) fn capture_window_bgrx(window: &Window) -> Result<(Vec<u8>, u32, u32), String> {
    let hwnd = hwnd_of(window).ok_or_else(|| "no window to capture (headless)".to_string())?;
    // SAFETY: hand-rolled GDI capture. Every created GDI object (mem DC, bitmap) is
    // released on every return path; `hwnd` is winit-owned and only read from; the
    // pixel buffer is sized exactly `w*h*4` and 32bpp rows are DWORD-aligned (no
    // stride padding), so `GetDIBits` writes within bounds.
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

        // `PrintWindow` owns the only supported transition from live HWND pixels
        // into this bitmap. A zero BOOL means failure and leaves the bitmap
        // unspecified, so it can never be treated as a best-effort success.
        let print_succeeded = PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT) != 0;

        // Win32 requires `hbmp` not be selected into a DC while GetDIBits reads it. Restore
        // the old object first; if that fails, destroy the DC (which releases its selected
        // objects) before deleting the bitmap.
        if SelectObject(mem, old) == 0 {
            DeleteDC(mem);
            DeleteObject(bmp);
            ReleaseDC(0, screen);
            return Err("SelectObject(restore) failed".to_string());
        }
        let mut bmi = BitmapInfoHeader {
            size: core::mem::size_of::<BitmapInfoHeader>() as u32,
            width: w,
            height: -h, // top-down (row 0 first), so no post-flip needed
            planes: 1,
            bit_count: 32,
            compression: 0, // BI_RGB
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
    /// The colour of the surface directly under window `wid`'s caption, which the
    /// caption tint takes ([`note_caption_surface`]), `0x00RRGGBB`:
    ///
    ///  * with a tab strip, the band's tone ([`crate::tab_bar::strip_bleed_tones`]),
    ///    so caption and tabs read as one chrome block over the grid;
    ///  * without one but with the message band on top, that band's ground;
    ///  * else the window's live background: the last composed frame's default
    ///    background, which a session theme or OSC 11 moves and which paints the
    ///    top padding the caption meets; the theme's before the first frame.
    ///
    /// The band tones come from the header palette ([`crate::app_render::ChromeSurface::Header`]),
    /// the instance theme: window chrome does not follow a session theme
    /// (`session_theme`'s module doc), so neither does a caption over a strip.
    pub(crate) fn caption_surface_tone(&self, wid: WindowId) -> u32 {
        let header = self.chrome_palette(crate::app_render::ChromeSurface::Header);
        if self.tab_strip_rows > 0
            && let Some((band, _)) = crate::tab_bar::strip_bleed_tones(header)
        {
            return band;
        }
        let ws = self.windows.get(&wid);
        // The message band is the only chrome bar under the strip: presence
        // owns no row (owner surfaces, 2026-10-16).
        if self.message_band_rows > 0 {
            let [r, g, b] = crate::chrome_band::band_colors(header).bar_bg;
            return (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        }
        ws.map(|ws| ws.input_scratch.default_bg)
            .filter(|&bg| bg >> 24 == 0)
            .unwrap_or(self.theme.bg)
            & 0x00FF_FFFF
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::Rect;
    use super::{
        AppRtWindows, CHROME_BG_UNKNOWN, ChromeApplyGuard, ClientBackdropDecline,
        DEFAULT_WHEEL_SCROLL_LINES, DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TABBEDWINDOW,
        DWMSBT_TRANSIENTWINDOW, DWMWA_COLOR_DEFAULT, WHEEL_PAGESCROLL,
        WM_DWMCOLORIZATIONCOLORCHANGED, WM_ENDSESSION, WM_NCDESTROY, WM_QUERYENDSESSION,
        WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_THEMECHANGED, WheelNotch, backdrop_for,
        caption_color_label, caption_tint, chrome_policy_code, chrome_policy_from_code,
        chrome_reassert_needed, claim_os_settings_wake, colorref_swap, effective_backdrop,
        high_contrast_active, is_os_settings_message, os_text_scale, resolve_chrome_theme,
        resolve_forced_chrome, transparency_effects_enabled, validate_window_capture_transfer,
        watch_os_settings_message, wheel_notch_from_raw, work_area_pts_from_rect,
    };
    use crate::app_config::{BackgroundMaterial, WindowTheme};
    use crate::native_appearance::AppearancePreferences;
    use crate::platform::AppRt;

    /// The work area excludes the taskbar and is in the logical points winit reports window
    /// positions in: a 1920x1080 display with a 48 px bottom taskbar at 150% is 1280x688
    /// points, not 1280x720.
    #[test]
    fn the_work_area_drops_the_taskbar_and_lands_in_logical_points() {
        let taskbar_at_the_bottom = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1032,
        };
        let area = work_area_pts_from_rect(&taskbar_at_the_bottom, 1.5).expect("a usable area");
        assert_eq!((area.x, area.y), (0.0, 0.0));
        assert_eq!(area.w, 1280.0);
        assert_eq!(area.h, 688.0);
        // A taskbar docked left moves the origin too, and a second monitor's
        // rectangle is offset in the virtual desktop.
        let docked_left_on_a_second_display = Rect {
            left: 1920 + 72,
            top: 0,
            right: 1920 + 1280,
            bottom: 1024,
        };
        let area = work_area_pts_from_rect(&docked_left_on_a_second_display, 2.0).expect("usable");
        assert_eq!((area.x, area.y), (996.0, 0.0));
        assert_eq!((area.w, area.h), (604.0, 512.0));
        // Degenerate geometry is refused, so the caller falls back to the winit
        // monitor bounds instead of cascading onto a zero-sized rectangle.
        for (rect, scale) in [
            (Rect::default(), 1.0),
            (
                Rect {
                    left: 100,
                    top: 100,
                    right: 0,
                    bottom: 0,
                },
                1.0,
            ),
            (
                Rect {
                    left: 0,
                    top: 0,
                    right: 800,
                    bottom: 600,
                },
                0.0,
            ),
            (
                Rect {
                    left: 0,
                    top: 0,
                    right: 800,
                    bottom: 600,
                },
                f64::NAN,
            ),
        ] {
            assert!(
                work_area_pts_from_rect(&rect, scale).is_none(),
                "degenerate work area {rect:?}@{scale} must not be cascaded against"
            );
        }
    }

    /// The settings hook watches exactly the four appearance/accessibility messages: the
    /// wndproc sees every message, so a loose predicate would post a `Wake` on mouse moves.
    #[test]
    fn the_settings_hook_watches_exactly_the_appearance_messages() {
        for msg in [
            WM_SETTINGCHANGE,
            WM_SYSCOLORCHANGE,
            WM_THEMECHANGED,
            WM_DWMCOLORIZATIONCOLORCHANGED,
        ] {
            assert!(
                is_os_settings_message(msg),
                "0x{msg:04X} must reach the settings lane"
            );
        }
        for msg in [
            WM_QUERYENDSESSION,
            WM_ENDSESSION,
            WM_NCDESTROY,
            0x0200, // WM_MOUSEMOVE — the hot one; a false positive here is a Wake storm
            0x000F, // WM_PAINT
            0x0100, // WM_KEYDOWN
            0x0006, // WM_ACTIVATE
        ] {
            assert!(
                !is_os_settings_message(msg),
                "0x{msg:04X} must NOT post a settings wake"
            );
        }
        // The end-session lane and the settings lane are disjoint: they share one
        // subclass, and a message that triggered both would do a durable write on
        // every accent-colour broadcast.
        assert!(!is_os_settings_message(WM_QUERYENDSESSION));
    }

    /// THE INSTALLER IS THE OTHER END-SESSION CALLER, AND IT MUST STAY ONE.
    ///
    /// 1. The scripts spell the message as this module's numeric constant.
    /// 2. It is sent, not posted: a posted message would still be queued when the kill lands.
    /// 3. The ask precedes the force-kill, and the kill stays (an older build or a hung pump
    ///    would otherwise keep the exe's file lock).
    /// 4. Neither uses `CloseMainWindow()` (`WM_CLOSE`), which raises the multi-tab
    ///    close-confirm dialog and would block the installer on a modal nobody watches.
    /// 5. The two embedded helpers are byte-identical: `install.ps1 -Register` copies
    ///    uninstall.ps1 standalone as the Apps & Features UninstallString, so they cannot
    ///    share a module.
    #[test]
    fn the_windows_installer_asks_for_a_session_save_before_it_force_kills() {
        let scripts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .expect("the aterm-gui manifest dir has a workspace-root grandparent")
            .join("apps")
            .join("aterm-win");
        // The message aterm's own subclass answers, spelled exactly as the embedded
        // P/Invoke declares it.
        let declaration = format!("const uint WM_QUERYENDSESSION = 0x{WM_QUERYENDSESSION:04X};");
        let read = |name: &str| -> String {
            let path = scripts.join(name);
            std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        };
        for name in ["install.ps1", "uninstall.ps1"] {
            let script = read(name);
            // Comment-only lines are dropped first: both scripts explain the force-kill and
            // name the rejected `CloseMainWindow()` in prose, which must not satisfy or fail
            // the code assertions. `#` is PowerShell's comment; `//` is the embedded C#'s.
            let code = script
                .lines()
                .filter(|line| {
                    let trimmed = line.trim_start();
                    !trimmed.starts_with('#') && !trimmed.starts_with("//")
                })
                .collect::<Vec<_>>()
                .join("\n");

            assert!(
                code.contains(&declaration),
                "{name} must declare `{declaration}` — the constant is the only join \
                 between the script's P/Invoke and this module's wndproc"
            );
            // Sent, not posted: the durable write happens inside the wndproc turn, so the send
            // returning proves the manifest is on disk. `SMTO_ABORTIFHUNG` keeps a wedged pump
            // from stalling an install.
            assert!(
                code.contains("SendMessageTimeoutW(hwnd, WM_QUERYENDSESSION"),
                "{name} must SEND this message at the call site — the P/Invoke merely \
                 being declared proves nothing, and a POSTED message would still be \
                 sitting in the queue when the script force-kills"
            );
            assert!(
                // The argument, not the `const` that defines it: a declared-but-unpassed
                // flag reads as protection and provides none.
                code.contains("SMTO_NORMAL | SMTO_ABORTIFHUNG,"),
                "{name} must PASS SMTO_ABORTIFHUNG, so a wedged message pump costs the \
                 send its timeout instead of blocking the whole script"
            );

            let ask = code
                .find("Request-AtermSessionSave $running")
                .unwrap_or_else(|| {
                    panic!("{name} must ask the running instances to save before stopping them")
                });
            let kill = code
                .find("Stop-Process -Force")
                .unwrap_or_else(|| panic!("{name} must still force-kill what it stops"));
            assert!(
                ask < kill,
                "{name} asks for the save at byte {ask}, after the force-kill at \
                 {kill} — a terminated process saves nothing"
            );
            assert!(
                !code.contains("CloseMainWindow"),
                "{name} must not use CloseMainWindow(): WM_CLOSE raises aterm's \
                 multi-tab close-confirm dialog and would hang the installer"
            );
        }

        // The duplicated P/Invoke, from `using System;` to the here-string terminator: only
        // this equality keeps a fix to one script from missing the other.
        let embedded = |name: &str| -> String {
            let script = read(name);
            let start = script
                .find("using System;")
                .unwrap_or_else(|| panic!("{name} must embed the C# session-save helper"));
            let end = script[start..].find("'@").unwrap_or_else(|| {
                panic!("{name}'s embedded helper has no here-string terminator")
            }) + start;
            script[start..end].to_string()
        };
        assert_eq!(
            embedded("install.ps1"),
            embedded("uninstall.ps1"),
            "install.ps1 and uninstall.ps1 must embed the SAME helper — uninstall.ps1 \
             is copied standalone into the install dir, so it can share a module with \
             nothing and the copies can only be kept honest here"
        );
    }

    /// aterm's own chrome write is not news from the OS: the `WM_THEMECHANGED` its
    /// `SetWindowTheme` sends back is ignored while the guard is held; every other watched
    /// message still counts.
    #[test]
    fn aterms_own_chrome_write_does_not_feed_the_settings_hook() {
        assert!(
            watch_os_settings_message(WM_THEMECHANGED),
            "watched normally"
        );
        {
            let _applying = ChromeApplyGuard::new();
            assert!(
                !watch_os_settings_message(WM_THEMECHANGED),
                "our own SetWindowTheme echo must be ignored"
            );
            for msg in [
                WM_SETTINGCHANGE,
                WM_SYSCOLORCHANGE,
                WM_DWMCOLORIZATIONCOLORCHANGED,
            ] {
                assert!(
                    watch_os_settings_message(msg),
                    "0x{msg:04X} is the OS talking, not us — it must still count"
                );
            }
            // Nesting must not release the guard early.
            {
                let _inner = ChromeApplyGuard::new();
                assert!(!watch_os_settings_message(WM_THEMECHANGED));
            }
            assert!(
                !watch_os_settings_message(WM_THEMECHANGED),
                "the outer guard still holds after an inner one drops"
            );
        }
        assert!(
            watch_os_settings_message(WM_THEMECHANGED),
            "and the watch reopens the moment the write is done"
        );
    }

    /// One OS settings burst costs exactly one re-sample, including the `WM_THEMECHANGED` the
    /// re-sample's own chrome write provokes. Modelled against a local flag: the subclass side
    /// is `watch_os_settings_message` + `claim_os_settings_wake`, the loop side
    /// `note_os_settings_resample`'s re-arm, then the guarded `SetWindowTheme`.
    #[test]
    fn one_settings_burst_costs_exactly_one_resample() {
        let pending = AtomicBool::new(false);
        let mut wakes = 0_u32;
        let deliver = |msg: u32, wakes: &mut u32| {
            if watch_os_settings_message(msg) && claim_os_settings_wake(&pending) {
                *wakes += 1;
            }
        };

        // One Settings flip, as Windows actually broadcasts it: several messages,
        // some of them repeats.
        for msg in [
            WM_SETTINGCHANGE,
            WM_SETTINGCHANGE,
            WM_THEMECHANGED,
            WM_SYSCOLORCHANGE,
            WM_DWMCOLORIZATIONCOLORCHANGED,
            WM_SETTINGCHANGE,
        ] {
            deliver(msg, &mut wakes);
        }
        assert_eq!(wakes, 1, "a burst of six messages is one Wake");

        // The loop services that Wake. `note_os_settings_resample` re-arms the
        // coalescer first so a trailing broadcast is not swallowed…
        pending.store(false, Ordering::Release);
        // …and then the re-sample writes chrome, whose `SetWindowTheme` sends
        // `WM_THEMECHANGED` back into the wndproc on this very thread.
        {
            let _applying = ChromeApplyGuard::new();
            deliver(WM_THEMECHANGED, &mut wakes);
        }
        assert_eq!(
            wakes, 1,
            "aterm's own chrome write re-entered the hook and bought a second re-sample"
        );

        // A new flip afterwards still gets its own Wake: the guard must not latch the hook shut.
        deliver(WM_SETTINGCHANGE, &mut wakes);
        assert_eq!(wakes, 2, "the next real burst still wakes the loop");
    }

    /// The reduced-transparency gate suppresses a requested material and never invents one, so
    /// an opaque window (the default) is identical on both sides of the preference.
    #[test]
    fn reduced_transparency_suppresses_the_backdrop_but_never_adds_one() {
        let install = |reduced| {
            crate::native_appearance::install_preferences(AppearancePreferences {
                high_contrast: false,
                reduced_transparency: reduced,
                differentiate_without_color: false,
                text_scale: 1.0,
            });
        };
        let previous = crate::native_appearance::current_preferences();

        install(false);
        for material in [
            DWMSBT_NONE,
            DWMSBT_MAINWINDOW,
            DWMSBT_TRANSIENTWINDOW,
            DWMSBT_TABBEDWINDOW,
        ] {
            assert_eq!(
                effective_backdrop(material),
                material,
                "transparency on: the configured material passes through untouched"
            );
        }

        install(true);
        for material in [
            DWMSBT_MAINWINDOW,
            DWMSBT_TRANSIENTWINDOW,
            DWMSBT_TABBEDWINDOW,
        ] {
            assert_eq!(
                effective_backdrop(material),
                DWMSBT_NONE,
                "transparency off: every translucent material collapses to none"
            );
        }
        // The opaque default is not a material: nothing to reduce.
        assert_eq!(effective_backdrop(DWMSBT_NONE), DWMSBT_NONE);

        crate::native_appearance::install_preferences(previous);
    }

    /// The OS palette is forced only under High Contrast: off HC `GetSysColor` still answers,
    /// and forcing it would repaint every ordinary desktop's tab strip in `COLOR_BTNFACE`.
    #[test]
    fn the_chrome_palette_is_forced_only_under_high_contrast() {
        // The diagnostic override would make the negative arm a lie.
        if super::force_hc_chrome_env() {
            aterm_spec::host::could_not_run(
                "the high-contrast chrome override is set here, so the negative arm cannot hold",
            );
        }
        assert_eq!(resolve_forced_chrome(false), None);
        // Under HC the five slots come from the live machine, so assert only the shape
        // (`chrome_band` asserts real HC palettes against fixtures).
        assert!(resolve_forced_chrome(true).is_some());
    }

    /// The two registry-backed preferences answer the Windows default on a machine that never
    /// moved them, and survive `AppearancePreferences::normalized`. Against the live machine,
    /// so it asserts the contract, not a desktop's settings.
    #[test]
    fn the_registry_backed_preferences_stay_in_contract() {
        let scale = os_text_scale();
        assert!(
            scale.is_finite() && scale > 0.0,
            "text scale must be a usable ratio, got {scale}"
        );
        let normalized = AppearancePreferences {
            high_contrast: false,
            reduced_transparency: false,
            differentiate_without_color: false,
            text_scale: scale,
        }
        .normalized();
        assert!(
            (0.85..=2.0).contains(&normalized.text_scale),
            "the shared clamp bounds the OS value, got {}",
            normalized.text_scale
        );
        // The composition rule as a truth table, independent of the test machine: either OS
        // source suffices.
        for (hc, effects, want) in [
            (false, true, false),
            (false, false, true),
            (true, true, true),
            (true, false, true),
        ] {
            assert_eq!(
                super::reduced_transparency_from(hc, effects),
                want,
                "high_contrast={hc} transparency_effects={effects}"
            );
        }

        // …and the live reads are the two inputs that rule is applied to.
        // `native_appearance_preferences` republishes the forced chrome palette, which under
        // `cfg(test)` is thread-local: restore it so a machine running High Contrast cannot
        // leak an OS palette into the next test on this thread.
        let saved_palette = crate::chrome_band::forced_chrome();
        let published = AppRtWindows {
            probe_preferences: true,
        }
        .native_appearance_preferences();
        crate::chrome_band::install_forced_chrome(saved_palette);
        assert_eq!(
            published.reduced_transparency,
            super::reduced_transparency_from(
                high_contrast_active(),
                transparency_effects_enabled()
            ),
            "the published preference is the rule applied to the two OS reads"
        );
        assert_eq!(published.high_contrast, high_contrast_active());
    }

    /// The `client=opaque(...)` code↔token map is total and round-trips, and `0` never decodes
    /// to a reason. The first-writer-wins latch is process-global and deliberately not poked
    /// from a test.
    #[test]
    fn the_client_decline_tokens_round_trip_and_zero_means_nothing_declined() {
        for reason in [
            ClientBackdropDecline::HdrGlow,
            ClientBackdropDecline::NoGpu,
            ClientBackdropDecline::DcompUnavailable,
            ClientBackdropDecline::GpuLost,
            ClientBackdropDecline::NotAtLaunch,
        ] {
            let code = reason as u32;
            assert_ne!(code, 0, "no reason may collide with the empty sentinel");
            assert_eq!(ClientBackdropDecline::from_code(code), Some(reason));
            assert!(
                !reason.token().is_empty()
                    && reason
                        .token()
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_'),
                "tokens stay greppable: {:?}",
                reason.token()
            );
        }
        assert_eq!(ClientBackdropDecline::from_code(0), None);
        assert_eq!(ClientBackdropDecline::from_code(99), None);
        // The default-config case's token.
        assert_eq!(ClientBackdropDecline::HdrGlow.token(), "hdr_glow");
    }

    /// The material → DWM backdrop mapping is total; the default (`None`) maps to `DWMSBT_NONE`,
    /// so a stock config leaves the window a plain opaque grid.
    #[test]
    fn material_maps_to_the_dwm_backdrop() {
        assert_eq!(backdrop_for(BackgroundMaterial::None), DWMSBT_NONE);
        assert_eq!(
            backdrop_for(BackgroundMaterial::UnderWindow),
            DWMSBT_MAINWINDOW
        );
        assert_eq!(
            backdrop_for(BackgroundMaterial::Sidebar),
            DWMSBT_TABBEDWINDOW
        );
        assert_eq!(
            backdrop_for(BackgroundMaterial::Hud),
            DWMSBT_TRANSIENTWINDOW
        );
        // The values equal the stable DWM_SYSTEMBACKDROP_TYPE enum constants.
        assert_eq!(
            (
                DWMSBT_NONE,
                DWMSBT_MAINWINDOW,
                DWMSBT_TRANSIENTWINDOW,
                DWMSBT_TABBEDWINDOW
            ),
            (1, 2, 3, 4)
        );
    }

    /// On Windows `Auto` resolves against the terminal background, not the OS preference, so a
    /// light desktop never paints a white caption over a dark grid.
    #[test]
    fn auto_resolves_the_caption_from_the_terminal_background() {
        // Ghostty/aterm-ish dark grounds → dark caption.
        for bg in [0x00_0000, 0x1e_1e2e, 0x28_2c34, 0x0d_1117] {
            assert_eq!(
                resolve_chrome_theme(WindowTheme::Auto, bg, false),
                WindowTheme::Dark,
                "0x{bg:06x} is a dark terminal background"
            );
        }
        // Light grounds (solarized-light / plain paper) → light caption.
        for bg in [0xff_ffff, 0xfd_f6e3, 0xee_eeee] {
            assert_eq!(
                resolve_chrome_theme(WindowTheme::Auto, bg, false),
                WindowTheme::Light,
                "0x{bg:06x} is a light terminal background"
            );
        }
    }

    /// Under High Contrast `Auto` does not resolve from the terminal background: the HC palette
    /// owns the caption (`apply_chrome_appearance`'s HC arm writes immersive-dark off). Explicit
    /// `Light`/`Dark` remain hard overrides.
    #[test]
    fn high_contrast_returns_auto_to_the_os() {
        for bg in [0x00_0000, 0xff_ffff, 0x1e_1e2e] {
            assert_eq!(
                resolve_chrome_theme(WindowTheme::Auto, bg, true),
                WindowTheme::Auto,
                "high contrast must defer 0x{bg:06x} to the OS"
            );
            assert_eq!(
                resolve_chrome_theme(WindowTheme::Light, bg, true),
                WindowTheme::Light
            );
            assert_eq!(
                resolve_chrome_theme(WindowTheme::Dark, bg, true),
                WindowTheme::Dark
            );
        }
    }

    /// The per-frame guard writes only on a readback that disagrees with the resolved answer:
    /// agreement is the steady state (a write would re-theme the caption every frame), and an
    /// ungettable attribute is no evidence of a clobber.
    #[test]
    fn the_frame_guard_writes_only_on_a_disagreeing_readback() {
        assert!(
            chrome_reassert_needed(true, Some(0)),
            "dark wanted, light applied"
        );
        assert!(
            chrome_reassert_needed(false, Some(1)),
            "light wanted, dark applied"
        );
        assert!(!chrome_reassert_needed(true, Some(1)));
        assert!(!chrome_reassert_needed(false, Some(0)));
        assert!(
            !chrome_reassert_needed(true, None),
            "no readback ⇒ no evidence ⇒ no write"
        );
        assert!(!chrome_reassert_needed(false, None));
    }

    /// Explicit config values stay hard overrides: only `Auto` consults the background.
    #[test]
    fn explicit_light_and_dark_ignore_the_terminal_background() {
        for bg in [0x00_0000, 0xff_ffff, CHROME_BG_UNKNOWN] {
            assert_eq!(
                resolve_chrome_theme(WindowTheme::Light, bg, false),
                WindowTheme::Light
            );
            assert_eq!(
                resolve_chrome_theme(WindowTheme::Dark, bg, false),
                WindowTheme::Dark
            );
        }
    }

    /// Before any background is published there is nothing to match, so `Auto` stays `Auto`
    /// and a not-yet-themed window is never pinned to a guess.
    #[test]
    fn auto_without_a_published_background_stays_auto() {
        assert_eq!(
            resolve_chrome_theme(WindowTheme::Auto, CHROME_BG_UNKNOWN, false),
            WindowTheme::Auto
        );
    }

    /// The policy latch is a lossless round-trip for every variant (an out-of-range
    /// byte, which cannot occur, decodes to the config default rather than panicking).
    #[test]
    fn chrome_policy_latch_round_trips() {
        for theme in [WindowTheme::Auto, WindowTheme::Light, WindowTheme::Dark] {
            assert_eq!(chrome_policy_from_code(chrome_policy_code(theme)), theme);
        }
        assert_eq!(chrome_policy_from_code(0), WindowTheme::Auto);
        assert_eq!(chrome_policy_from_code(200), WindowTheme::Auto);
    }

    #[test]
    fn window_capture_transfer_requires_print_and_every_scanline() {
        assert_eq!(validate_window_capture_transfer(true, 720, 720), Ok(()));
        assert_eq!(
            validate_window_capture_transfer(false, 720, 720).unwrap_err(),
            "PrintWindow failed",
            "GetDIBits-looking bytes cannot redeem a failed PrintWindow"
        );
        assert_eq!(
            validate_window_capture_transfer(true, 0, 720).unwrap_err(),
            "GetDIBits failed"
        );
        assert_eq!(
            validate_window_capture_transfer(true, 719, 720).unwrap_err(),
            "GetDIBits copied 719 of 720 window-capture scanlines",
            "a nonzero partial copy is still an incomplete image"
        );
    }

    /// The wheel-notch decode: a line count, the all-ones "One screen at a time" sentinel, and
    /// an explicit 0 ("wheel scrolling off") honoured rather than clamped up.
    #[test]
    fn wheel_notch_decodes_lines_and_the_page_sentinel() {
        assert_eq!(
            wheel_notch_from_raw(DEFAULT_WHEEL_SCROLL_LINES),
            WheelNotch::Lines(3),
            "the Windows default is three lines per detent"
        );
        assert_eq!(wheel_notch_from_raw(1), WheelNotch::Lines(1));
        assert_eq!(wheel_notch_from_raw(0), WheelNotch::Lines(0));
        assert_eq!(wheel_notch_from_raw(WHEEL_PAGESCROLL), WheelNotch::Page);
    }

    /// COLORREF is `0x00BBGGRR` while themes store `0x00RRGGBB`: the swap moves R↔B, is its own
    /// inverse (one helper serves the write and the introspection readback), and drops a stray
    /// top byte.
    #[test]
    fn colorref_swap_reverses_r_and_b_and_is_an_involution() {
        assert_eq!(colorref_swap(0x11_1318), 0x18_1311);
        assert_eq!(colorref_swap(colorref_swap(0xAB_CDEF)), 0xAB_CDEF);
        assert_eq!(
            colorref_swap(0xFF00_0000),
            0,
            "top byte must not leak into a channel"
        );
    }

    /// Under `Auto` with no backdrop and no HC scheme, the caption fill is the terminal
    /// background in COLORREF order and the focused text is white on a dark grid, black on a
    /// light one.
    #[test]
    fn caption_tint_paints_the_terminal_background_under_auto() {
        // A dark default (#111318).
        let (caption, text) = caption_tint(WindowTheme::Auto, 0x11_1318, DWMSBT_NONE, false, true);
        assert_eq!(
            caption, 0x18_1311,
            "caption = theme bg in COLORREF byte order"
        );
        assert_eq!(text, 0x00FF_FFFF, "dark grid gets white title text");
        // Solarized-light paper → black ink.
        let (caption, text) = caption_tint(WindowTheme::Auto, 0xFD_F6E3, DWMSBT_NONE, false, true);
        assert_eq!(caption, 0xE3_F6FD);
        assert_eq!(text, 0x0000_0000, "light grid gets black title text");
    }

    /// A pinned COLORREF kills DWM's unfocused treatment, so the resolver dims the text only:
    /// the fill holds across the focus flip, and the text moves toward it without reaching it.
    #[test]
    fn caption_text_dims_unfocused_while_the_fill_holds() {
        let bg = 0x11_1318;
        let (cap_focused, text_focused) =
            caption_tint(WindowTheme::Auto, bg, DWMSBT_NONE, false, true);
        let (cap_unfocused, text_unfocused) =
            caption_tint(WindowTheme::Auto, bg, DWMSBT_NONE, false, false);
        assert_eq!(cap_focused, cap_unfocused, "the fill never dims");
        assert_ne!(text_focused, text_unfocused, "the text must dim");
        assert_ne!(text_unfocused, cap_unfocused, "dimmed is not erased");
        // The dim is the shared chrome blend of the full ink toward the fill.
        let expected = crate::chrome_band::mix3(
            [0xFF, 0xFF, 0xFF],
            [0x11, 0x13, 0x18],
            super::CAPTION_TEXT_UNFOCUSED_DIM,
        );
        let expected_rgb =
            (u32::from(expected[0]) << 16) | (u32::from(expected[1]) << 8) | u32::from(expected[2]);
        assert_eq!(text_unfocused, colorref_swap(expected_rgb));
    }

    /// Every other gate yields the full reset pair (DWM's palette and native dimming): High
    /// Contrast, an active backdrop material, an explicit `Light`/`Dark` policy, and no
    /// published background.
    #[test]
    fn caption_tint_defers_on_every_gate() {
        let default = (DWMWA_COLOR_DEFAULT, DWMWA_COLOR_DEFAULT);
        let bg = 0x11_1318;
        for focused in [true, false] {
            assert_eq!(
                caption_tint(WindowTheme::Auto, bg, DWMSBT_NONE, true, focused),
                default,
                "High Contrast owns the caption"
            );
            assert_eq!(
                caption_tint(WindowTheme::Auto, bg, DWMSBT_MAINWINDOW, false, focused),
                default,
                "a solid caption colour would override Mica"
            );
            assert_eq!(
                caption_tint(WindowTheme::Light, bg, DWMSBT_NONE, false, focused),
                default,
                "explicit light keeps the stock OS caption"
            );
            assert_eq!(
                caption_tint(WindowTheme::Dark, bg, DWMSBT_NONE, false, focused),
                default,
                "explicit dark keeps the stock OS caption"
            );
            assert_eq!(
                caption_tint(
                    WindowTheme::Auto,
                    CHROME_BG_UNKNOWN,
                    DWMSBT_NONE,
                    false,
                    focused
                ),
                default,
                "no published background yet — nothing to tint with"
            );
        }
    }

    /// The accent is WinUI's pair from Explorer's `AccentPalette` (Light2 for dark
    /// surfaces, Dark1 for light ones; this machine's default blue below), DWM's
    /// `AccentColor` on both sides without it, and nothing with neither.
    #[test]
    fn the_accent_palette_gives_the_winui_shades_and_dwm_backs_it_up() {
        let shades: [[u8; 3]; 8] = [
            [0x99, 0xeb, 0xff],
            [0x4c, 0xc2, 0xff],
            [0x00, 0x91, 0xf8],
            [0x00, 0x78, 0xd4],
            [0x00, 0x67, 0xc0],
            [0x00, 0x3e, 0x92],
            [0x00, 0x1a, 0x68],
            [0xf7, 0x63, 0x0c],
        ];
        let mut palette = [0u8; 32];
        for (i, rgb) in shades.iter().enumerate() {
            palette[i * 4..i * 4 + 3].copy_from_slice(rgb);
        }
        let accent = super::accent_from_palette(Some(palette), Some(0xffd4_7800))
            .expect("a palette is an accent");
        assert_eq!(accent.on_dark, [0x4c, 0xc2, 0xff]);
        assert_eq!(accent.on_light, [0x00, 0x67, 0xc0]);
        let dwm = super::accent_from_palette(None, Some(0xffd4_7800)).expect("DWM's accent");
        assert_eq!(
            (dwm.on_dark, dwm.on_light),
            ([0x00, 0x78, 0xd4], [0x00, 0x78, 0xd4])
        );
        assert_eq!(super::accent_from_palette(None, None), None);
    }

    /// `chrome`'s caption fields: the DWM readback where Windows answers it, the
    /// value aterm wrote, marked, where it does not (build 26200 refuses both
    /// colour attributes though it paints them), and `n/a` only with neither.
    #[test]
    fn the_caption_color_field_reports_what_was_written_when_dwm_will_not_read_back() {
        let tint = colorref_swap(0x30_3135);
        assert_eq!(caption_color_label(Some(tint), Some(0)), "#303135");
        assert_eq!(caption_color_label(None, Some(tint)), "#303135(set)");
        assert_eq!(
            caption_color_label(None, Some(DWMWA_COLOR_DEFAULT)),
            "default(set)"
        );
        assert_eq!(
            caption_color_label(Some(DWMWA_COLOR_DEFAULT), None),
            "default"
        );
        assert_eq!(caption_color_label(None, None), "n/a");
    }

    /// The caption takes the tone of what sits right under it. With the tab strip
    /// (the Windows default) that is the band, not the terminal background, so
    /// caption, band and grid stop reading dark / grey / dark; without a strip it
    /// is the window's live background, which an OSC 11 moves.
    #[test]
    fn the_caption_matches_the_band_under_it_and_the_live_background_without_one() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let band = crate::tab_bar::strip_bleed_tones(
            app.chrome_palette(crate::app_render::ChromeSurface::Header),
        )
        .expect("Windows paints a band")
        .0;
        assert_eq!(app.caption_surface_tone(wid), band);
        assert_ne!(band, app.theme.bg & 0x00FF_FFFF, "the band is its own tone");
        app.tab_strip_rows = 0;
        app.message_band_rows = 0;
        app.windows.get_mut(&wid).unwrap().input_scratch.default_bg = 0x002a_00ff;
        assert_eq!(app.caption_surface_tone(wid), 0x002a_00ff);
        app.windows.get_mut(&wid).unwrap().input_scratch.default_bg =
            aterm_core::render::COLOR_UNSET;
        assert_eq!(app.caption_surface_tone(wid), app.theme.bg & 0x00FF_FFFF);
    }
}

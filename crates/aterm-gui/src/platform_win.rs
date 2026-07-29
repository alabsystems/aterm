// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The native **Windows** application-runtime ([`AppRtWindows`]): the Windows
//! peer of `platform::AppRtMacOS`, implementing the same [`crate::platform::AppRt`]
//! seam with real OS chrome instead of the graceful `AppRtLinux` no-ops.
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
//! Zero-sized, like the other two `AppRt` impls — the `App.apprt` field costs
//! nothing.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use core::ffi::c_void;
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::app_config::{BackgroundMaterial, WindowTheme};
use crate::notify::NotifyMsg;
use crate::platform::{
    AppRt, ReduceMotionObserver, SurfaceColorspace, ToolbarTabsModel, window_theme_to_winit,
};
use crate::{Wake, WindowId, menu, toolbar};

// ---- Hand-rolled leaf Win32 (dwmapi + user32) -------------------------------------
//
// `DwmSetWindowAttribute(HWND, DWORD attr, LPCVOID pv, DWORD cb)`. HWND is a
// pointer-sized handle; passing it as `isize` matches the MSVC `system` ABI (and how
// `hdr_win.rs` already threads HMONITOR as `isize`). Every call is best-effort: the
// `HRESULT` is IGNORED because an unsupported attribute on an older Windows 10 build
// returns `E_INVALIDARG` and the correct response is simply to leave that facet at
// its OS default (§4 "degrade to immersive-dark-only on Windows 10").

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
}

/// `GCLP_HBRBACKGROUND` — the window-class background brush slot.
const GCLP_HBRBACKGROUND: i32 = -10;

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
#[derive(Default, Clone, Copy)]
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

/// `PW_RENDERFULLCONTENT` — render the window's full content INCLUDING its
/// DirectComposition / GPU (wgpu swapchain) surface, so the terminal grid shows in
/// the capture rather than a blank client area.
const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;
/// `DIB_RGB_COLORS`.
const DIB_RGB_COLORS: u32 = 0;

/// `DWMWA_USE_IMMERSIVE_DARK_MODE` — the dark title-bar toggle (winit sets this
/// via `set_theme`; we read it back for introspection).
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
/// `DWMWA_WINDOW_CORNER_PREFERENCE` (Windows 11+): round the window corners.
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
/// `DWMWCP_ROUND` — the standard rounded-corner radius.
const DWMWCP_ROUND: u32 = 2;
/// `DWMWA_SYSTEMBACKDROP_TYPE` (Windows 11 22H2+): Mica / Acrylic / none.
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
/// `DWMSBT_NONE` — no system backdrop (the minimal, opaque default).
const DWMSBT_NONE: u32 = 1;
/// `DWMSBT_MAINWINDOW` — **Mica** (the desktop-tinted top-level backdrop).
const DWMSBT_MAINWINDOW: u32 = 2;
/// `DWMSBT_TRANSIENTWINDOW` — **Acrylic** (the blurred transient backdrop).
const DWMSBT_TRANSIENTWINDOW: u32 = 3;
/// `DWMSBT_TABBEDWINDOW` — the tabbed-window (Mica Alt) backdrop.
const DWMSBT_TABBEDWINDOW: u32 = 4;

/// `SPI_GETCLIENTAREAANIMATION` — the "Show animations in Windows" master switch
/// (the closest OS-wide Reduce-Motion signal Windows exposes).
const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
/// Windows High Contrast is also a reduced-transparency request.
const SPI_GETHIGHCONTRAST: u32 = 0x0042;
const HCF_HIGHCONTRASTON: u32 = 0x0000_0001;

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

/// The `HWND` behind a winit [`Window`], or `None` when there is no Win32 handle
/// (headless / not-yet-realized). Mirrors `main.rs::window_hwnd`.
fn hwnd_of(window: &Window) -> Option<isize> {
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
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

/// Best-effort `DwmGetWindowAttribute` read of a 4-byte (`BOOL`/`DWORD`) attribute:
/// `Some(value)` when the OS supports getting it (`hr == 0`), else `None` (older
/// Windows, or a write-only attribute). The ground-truth counterpart of
/// [`dwm_set_u32`] — used by [`read_chrome_lines`] so introspection reports what is
/// ACTUALLY on the live window, not merely what we tried to set.
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

/// The `chrome` introspection lines for the live Windows window: the backend name
/// plus the ACTUAL DWM attributes read back off the HWND (`DwmGetWindowAttribute`)
/// — dark-mode, corner preference, system backdrop — and the effective winit theme.
/// This is the W0 acceptance surface: `aterm-ctl chrome` shows that `AppRtWindows`
/// really applied the native chrome, from ground truth, not from intent. A `None`
/// window (headless) or a not-yet-realized HWND reports that honestly.
pub(crate) fn read_chrome_lines(
    window: Option<&Window>,
    cfg_theme: WindowTheme,
    cfg_material: BackgroundMaterial,
) -> Vec<String> {
    // The CONFIGURED intent (what the App resolved from config), so introspection can
    // tell "config didn't reach the window" apart from "the DWM getter reports the
    // effective, not the set, value".
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
        Some(winit::window::Theme::Dark) => "dark",
        Some(winit::window::Theme::Light) => "light",
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
    vec![format!(
        "windows: backend=AppRtWindows hwnd=0x{hwnd:x} cfg_theme={want_theme} cfg_backdrop={want_backdrop} \
         os_theme={theme} immersive_dark={dark} corner={corner} backdrop={backdrop} aumid={}",
        crate::win32::AUMID
    )]
}

/// The `DWMWA_SYSTEMBACKDROP_TYPE` value for a config [`BackgroundMaterial`].
/// `None` maps to `DWMSBT_NONE` (the minimal opaque default — no backdrop), so a
/// stock config (material defaults to `None`) leaves the window a plain fast grid,
/// honouring the "minimal, effects-off-by-default" posture.
fn backdrop_for(material: BackgroundMaterial) -> u32 {
    match material {
        BackgroundMaterial::None => DWMSBT_NONE,
        BackgroundMaterial::UnderWindow => DWMSBT_MAINWINDOW, // Mica
        BackgroundMaterial::Sidebar => DWMSBT_TABBEDWINDOW,   // Mica Alt / tabbed
        BackgroundMaterial::Hud => DWMSBT_TRANSIENTWINDOW,    // Acrylic
    }
}

/// The native Windows application-runtime. Decorates the winit-owned `HWND` with
/// DWM chrome (immersive dark via winit + rounded corners + system backdrop) and
/// wires the EDR/reduce-motion queries; every other method delegates to the shared
/// cross-platform modules. Zero-sized.
pub(crate) struct AppRtWindows;

impl AppRt for AppRtWindows {
    /// Paint the window-class background brush the terminal's THEME background —
    /// the white-flash fix. winit registers its window class with a NULL
    /// `hbrBackground` (vendored winit `window.rs`), so between `set_visible` and
    /// the first swapchain present the unpainted client area erases to WHITE — a
    /// visible flash on every dark-theme launch (and the flash window covers the
    /// whole GPU-init tail on a cold start). Setting a theme-bg `HBRUSH` on the
    /// class makes `WM_ERASEBKGND` paint theme colour instead, so the window is
    /// born looking like the terminal. Class-wide (all aterm windows share the
    /// theme) and repeat-safe: the PREVIOUS brush we installed is deleted after
    /// each swap (the first swap returns winit's null brush → nothing to delete).
    /// COLORREF is 0x00BBGGRR, so the theme's 0x00RRGGBB is R/B-swapped.
    fn window_set_background_color(&self, window: &Window, bg: u32) {
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        let colorref = ((bg & 0xFF) << 16) | (bg & 0xFF00) | ((bg >> 16) & 0xFF);
        // SAFETY: two documented leaf calls. The created brush is OWNED by the
        // window class after the swap (the OS uses it for every later erase); the
        // previously-installed brush (ours, from an earlier theme apply) is deleted
        // exactly once here. A zero previous value is winit's null brush — skipped.
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

    /// Apply the native title-bar appearance: winit's `set_theme` drives
    /// `DWMWA_USE_IMMERSIVE_DARK_MODE` (it already tracks live OS light/dark for the
    /// `Auto` → `None` case and forces the variant for explicit `Light`/`Dark`), and
    /// we ADD the rounded-corner preference winit does not expose. Best-effort; a
    /// window with no `HWND` yet just gets the winit theme.
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        // Immersive dark / light title bar. `Auto` → winit `set_theme(None)`, which
        // applies the OS theme's dark-mode attribute AND keeps live-tracking OS
        // light/dark switches. For a FORCED `Light`/`Dark` we also set
        // `DWMWA_USE_IMMERSIVE_DARK_MODE` DIRECTLY, so the title bar matches config
        // regardless of winit's Windows theme internals (self-contained, and the
        // value the introspection reads back reflects the config).
        window.set_theme(window_theme_to_winit(theme));
        if let Some(hwnd) = hwnd_of(window) {
            match theme {
                WindowTheme::Dark => dwm_set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 1),
                WindowTheme::Light => dwm_set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 0),
                WindowTheme::Auto => {} // winit's set_theme(None) tracks the OS live
            }
            // Rounded corners: a Win11 top-level window is rounded by default, but
            // state it explicitly so intent survives any style change; a silent no-op
            // (E_INVALIDARG) on Windows 10.
            dwm_set_u32(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
        }
    }

    /// INTENTIONAL no-op on Windows: the swapchain colour space (scRGB for the HDR
    /// EDR present) is tagged in the renderer via wgpu `as_hal` at surface creation
    /// (`aterm-gpu`), not through this window-level seam — the CAMetalLayer tag this
    /// method models is a macOS concept. Report the effective source coordinates:
    /// the renderer already confirmed scRGB before exposing an HDR surface;
    /// Windows has no Display-P3 tag here, so that request remains ordinary sRGB.
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

    /// Install / update / remove the DWM **system backdrop** (`DWMWA_SYSTEMBACKDROP_TYPE`):
    /// `background_material` maps to Mica (`UnderWindow`) / Mica-Alt (`Sidebar`) /
    /// Acrylic (`Hud`), and `None` (the default) to `DWMSBT_NONE` — a plain opaque
    /// window. This is `background_material`'s first real Windows consumer.
    ///
    /// The grid body stays opaque (the renderer clears it), so the backdrop shows in
    /// the title-bar band / window padding — the Windows analogue of the macOS
    /// "titlebar-only vibrancy" intent. `translucent` (the GPU per-pixel-alpha
    /// present) is not required for Mica, so it is not consulted here; a translucent
    /// grid over Mica is future work (§4). Best-effort; a silent no-op pre-22H2.
    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: BackgroundMaterial,
        _translucent: bool,
        _bg: u32,
    ) {
        if let Some(hwnd) = hwnd_of(window) {
            dwm_set_u32(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, backdrop_for(material));
        }
    }

    /// The panel's EDR headroom for `window`'s monitor when Windows HDR is on
    /// (`MaxLuminance / SDR-white`), or `1.0` (no headroom → the aurora pass is
    /// provably inert) when HDR is off or any query fails. See [`crate::hdr_win`].
    fn screen_edr_max(&self, window: &Window) -> f32 {
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
    ) -> SyncSender<NotifyMsg> {
        crate::notify::spawn_delivery(suppress, silent)
    }

    /// Delegate to the shared menu module (a `None` stub today — a native Win32 menu
    /// bar is future work; the terminal drives its commands from keybindings).
    fn install_menu(&self, proxy: &EventLoopProxy<Wake>) -> Option<menu::MenuHandle> {
        menu::install(proxy)
    }

    /// Delegate to the shared toolbar module (the real in-memory tab-chrome model).
    fn install_toolbar(
        &self,
        window: &Window,
        proxy: &EventLoopProxy<Wake>,
        wid: WindowId,
    ) -> Option<toolbar::ToolbarHandle> {
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

    fn set_toolbar_update_available(&self, handle: &toolbar::ToolbarHandle, available: bool) {
        toolbar::set_update_available(handle, available);
    }

    fn read_toolbar_chrome(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        toolbar::read_tab_chrome(handle)
    }

    /// No native confirm dialog yet: `None` tells the caller to use the in-window
    /// titlebar-warning confirm (which honours the custom `proceed_label` a fixed
    /// `MessageBoxW` OK/Cancel could not). A `TaskDialog` with a custom button is
    /// the documented follow-up.
    fn confirm(&self, _title: &str, _body: &str, _proceed_label: &str) -> Option<bool> {
        None
    }

    /// No `terminate:`-style app-quit gesture on Windows (a window close routes
    /// through winit `CloseRequested`, already confirmed in-window): a no-op.
    fn install_quit_confirm(&self) {}

    /// The OS Reduce-Motion preference, read from `SPI_GETCLIENTAREAANIMATION` (the
    /// "Show animations in Windows" master switch): animations OFF ⇒ reduce motion.
    /// Feeds [`crate::motion::MotionPolicy`]. Fail-safe to `false` (do not reduce) if
    /// the query fails.
    fn reduce_motion(&self) -> bool {
        let mut animations_enabled: i32 = 1;
        // SAFETY: a single documented `SystemParametersInfoW` read into a local
        // `BOOL`; `ui_param`/`fw_ini` are 0 (no set), and the out-pointer is valid
        // for the call. Non-zero return = success; the `BOOL` is TRUE when
        // client-area animations are ENABLED.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut animations_enabled as *mut i32).cast::<c_void>(),
                0,
            )
        };
        // Reduce motion when the query succeeded AND animations are disabled.
        ok != 0 && animations_enabled == 0
    }

    fn native_appearance_preferences(&self) -> crate::native_appearance::AppearancePreferences {
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
        let high_contrast = ok != 0 && high_contrast.flags & HCF_HIGHCONTRASTON != 0;
        crate::native_appearance::AppearancePreferences {
            high_contrast,
            reduced_transparency: high_contrast,
            text_scale: 1.0,
        }
    }

    /// No live Reduce-Motion observer yet: `None`. The change signal is
    /// `WM_SETTINGCHANGE`, which needs the §3.2 winit message hook (deferred); the
    /// explicit config `motion = "reduced"` override works today, and a re-query is
    /// cheap.
    fn observe_reduce_motion(&self, _proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver> {
        None
    }
}

/// Photograph the window — caption CHROME (dark/light title bar, buttons) plus the
/// GPU-rendered grid — to tightly-packed top-down RGBA8 via
/// `PrintWindow(PW_RENDERFULLCONTENT)`. This is the robust capture: it renders the
/// window itself (so it works even occluded / off-screen, unlike a screen grab) and
/// `PW_RENDERFULLCONTENT` pulls in the DirectComposition GPU surface. Returns
/// `(rgba, width, height)`; serves the Windows `window` introspection verb for the
/// ordinary `background_material = "none"` case. This is explicitly window-local,
/// square, and opaque: DWM's rounded-corner clip and Mica/Acrylic backdrop exist only
/// in compositor output. The caller therefore refuses non-`none` materials instead of
/// mislabeling this bitmap as the native on-glass result.
pub(crate) fn capture_window_rgba(window: &Window) -> Result<(Vec<u8>, u32, u32), String> {
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

        // Win32 requires `hbmp` NOT to be selected into a DC while GetDIBits reads
        // it. Restore the old object first; if that fails, destroy the DC (which
        // releases its selected objects) before deleting the bitmap.
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
        // GetDIBits yields BGRX (blue,green,red, alpha undefined); make it opaque RGBA.
        for px in buf.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
            px[3] = 255;
        }
        Ok((buf, w as u32, h as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TABBEDWINDOW, DWMSBT_TRANSIENTWINDOW, backdrop_for,
        validate_window_capture_transfer,
    };
    use crate::app_config::BackgroundMaterial;

    /// The material → DWM backdrop mapping is total and hits each system backdrop;
    /// crucially the default (`None`) maps to `DWMSBT_NONE` so a stock config leaves
    /// the window a plain opaque grid.
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
}

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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
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
    // End-session persistence (the WM_QUERYENDSESSION subclass below).
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn CallWindowProcW(prev: isize, hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    // L1 early reveal: the synchronous themed-erase flush (`window_flush_backdrop`).
    fn RedrawWindow(hwnd: isize, rect: *const Rect, region: isize, flags: u32) -> i32;
}

/// `GCLP_HBRBACKGROUND` — the window-class background brush slot.
const GCLP_HBRBACKGROUND: i32 = -10;

/// `RedrawWindow` flags for the early-reveal backdrop flush: mark the whole
/// client area invalid and needing erase, then deliver the `WM_ERASEBKGND`
/// *before the call returns*. Deliberately NOT `RDW_UPDATENOW`: that would also
/// deliver `WM_PAINT` synchronously, re-entering winit's wndproc with a
/// `RedrawRequested` in the middle of `attach_os_window` — an event the app is
/// in no state to service (no present target exists yet). The erase alone is
/// what puts theme colour on glass; the real `WM_PAINT` arrives at the ordinary
/// time once the loop resumes pumping.
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

/// `DWMWA_USE_IMMERSIVE_DARK_MODE` — the DOCUMENTED dark title-bar toggle, and the
/// value `aterm-ctl chrome` reads back as ground truth.
///
/// winit does **not** write it: `Window::set_theme` drives only `SetWindowTheme` plus
/// the undocumented `WCA_USEDARKMODECOLORS` composition attribute (see
/// `vendor/winit/src/platform_impl/windows/dark_mode.rs::try_theme`), which is exactly
/// why this attribute used to read back unset for `Auto`. [`apply_chrome_appearance`]
/// writes it explicitly for that reason — do not "simplify" it away on the assumption
/// that winit already covers it.
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
/// `DWMWA_WINDOW_CORNER_PREFERENCE` (Windows 11+): round the window corners.
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
/// `DWMWCP_ROUND` — the standard rounded-corner radius.
const DWMWCP_ROUND: u32 = 2;
/// `DWMWA_CAPTION_COLOR` (Windows 11+): a solid caption (title-bar) fill, as a
/// COLORREF. Setting it makes the OS caption read as PART of the terminal surface
/// instead of a gray system bar bolted on — what Windows Terminal does. On
/// Windows 10 the write fails with `E_INVALIDARG` and is dropped, degrading to
/// the immersive-dark-only caption exactly as before this attribute was used.
const DWMWA_CAPTION_COLOR: u32 = 35;
/// `DWMWA_TEXT_COLOR` (Windows 11+): the caption TITLE TEXT colour, as a COLORREF.
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

/// `SPI_GETWHEELSCROLLLINES` — how many LINES one wheel notch scrolls, the value
/// behind Settings ▸ Mouse ▸ "Lines to scroll at a time".
const SPI_GETWHEELSCROLLLINES: u32 = 0x0068;
/// `WHEEL_PAGESCROLL` — the sentinel that slider writes in its "One screen at a
/// time" position, meaning a notch scrolls a whole PAGE rather than N lines.
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

/// The `HWND` behind a winit [`Window`], or `None` when there is no Win32 handle
/// (headless / not-yet-realized). Mirrors `main.rs::window_hwnd`.
fn hwnd_of(window: &Window) -> Option<isize> {
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Theme byte order (`0x00RRGGBB`) ↔ COLORREF (`0x00BBGGRR`). GDI brushes and the
/// DWM caption/text colour attributes all take COLORREF, whose R and B bytes are
/// reversed relative to how aterm's themes store colour. The swap is its own
/// inverse, so this ONE helper serves both directions (theme → attribute write,
/// attribute readback → introspection) — a second copy of the byte shuffle is how
/// the caption and the class brush would drift apart. Any stray top byte is
/// dropped, matching how the class-brush site always masked it.
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
    // The caption tint (H3), read back as ground truth like everything else here:
    // `default` is the DWMWA_COLOR_DEFAULT sentinel (tint intentionally absent —
    // High Contrast, a backdrop material, an explicit theme policy, or no published
    // background yet), a hex value is the live tint in theme byte order, and `n/a`
    // is a pre-Win11 build where the attribute cannot be read. The acceptance
    // check: this value tracks the terminal theme background, and `caption_text`
    // visibly dims while the window is unfocused.
    let caption_color_field = |attr: u32| match dwm_get_u32(hwnd, attr) {
        Some(DWMWA_COLOR_DEFAULT) => "default".to_string(),
        Some(v) => format!("#{:06x}", colorref_swap(v)),
        None => "n/a".to_string(),
    };
    let caption = caption_color_field(DWMWA_CAPTION_COLOR);
    let caption_text = caption_color_field(DWMWA_TEXT_COLOR);
    // H1: which CLIENT-AREA presentation path this process runs — `visual` (the
    // DirectComposition swapchain: padding/chrome-bleed margins carry per-pixel
    // alpha and the DWM backdrop shows through them) or `opaque` (the default
    // HWND swapchain: the client area covers the backdrop completely, so
    // `backdrop=` above styles ONLY the caption). Ground truth from the built
    // GPU instance, not from config intent — so introspection can tell "the
    // material is configured" apart from "the material can actually reach the
    // client pixels" (the distinction the pre-H1 doc got wrong).
    let client = if aterm_gpu::dx12_visual_swapchain_active() {
        "visual"
    } else {
        "opaque"
    };
    vec![format!(
        "windows: backend=AppRtWindows hwnd=0x{hwnd:x} cfg_theme={want_theme} cfg_backdrop={want_backdrop} \
         os_theme={theme} immersive_dark={dark} corner={corner} backdrop={backdrop} client={client} \
         caption_color={caption} caption_text={caption_text} aumid={}",
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

// ---- Auto-theme caption resolution (Windows only) -----------------------------------
//
// WHY THIS EXISTS. `App::window_theme_for_chrome` deliberately passes config
// `window_theme` through UNCHANGED on every platform — `Auto` stays `Auto` so
// AppKit/DWM can follow the live system appearance, and terminal-palette selection
// stays independent of chrome selection. That is right on macOS, where a light
// NSWindow titlebar over a dark content view is the platform's own look and AppKit
// blends the two. It is WRONG on Windows: DWM paints the caption a flat opaque
// system colour with a hard seam against the client area, so a LIGHT system theme
// over aterm's (default) DARK grid draws a white caption bar directly on top of a
// near-black terminal — the defect this block fixes. Windows Terminal resolves the
// same way (its "Use system theme" caption follows the PROFILE background), so this
// is parity, not invention.
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
// has to resolve `Auto`. `AppRtWindows` is a ZST behind `&self`, so a static latch is
// the only place the value can live; both seams run on the winit main thread, so
// `Relaxed` is sufficient — the atomics buy interior mutability, not synchronization.

/// Sentinel for "no terminal background has been published yet". A theme bg is
/// `0x00RRGGBB`, so the top byte is always clear and `u32::MAX` cannot collide.
const CHROME_BG_UNKNOWN: u32 = u32::MAX;

/// The terminal THEME background last handed to [`AppRtWindows::window_set_background_color`].
static CHROME_BG: AtomicU32 = AtomicU32::new(CHROME_BG_UNKNOWN);

/// The config `window_theme` policy last handed to [`AppRtWindows::window_set_appearance`],
/// encoded by [`chrome_policy_code`]. Latched so the background-publishing seam can
/// re-run the SAME resolution on a theme change without the caller threading the
/// policy through a second argument.
///
/// Seeded to the CONFIG DEFAULT (`Auto`) rather than to a "not yet known" state on
/// purpose. Window attach publishes the background before it applies the appearance,
/// so on the very first window this latch is momentarily one call stale — with an
/// explicit `window_theme = "light"` over a dark grid that resolves dark for the few
/// microseconds until `window_set_appearance` corrects it. That is invisible: the
/// first window is still hidden at that point (`set_visible` comes after both calls),
/// and every later window and every live reload finds the latch already warm.
static CHROME_POLICY: AtomicU8 = AtomicU8::new(0); // 0 == Auto, the config default

/// The live Windows **High Contrast** state, latched by [`apply_chrome_appearance`].
///
/// Latched rather than queried at every resolution because the per-frame guard
/// [`verify_chrome_appearance`] resolves too, and `SPI_GETHIGHCONTRAST` is a syscall
/// that has no business on a frame path. Every seam that can plausibly follow an HC
/// change (window attach, a theme publish, a config reload, an OS appearance flip)
/// runs `apply_chrome_appearance`, so the latch is refreshed exactly when the answer
/// can have moved.
static CHROME_HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);

/// The DWM backdrop value last applied by [`AppRtWindows::window_set_vibrancy`],
/// latched (like [`CHROME_BG`]) so the caption-tint resolver can honour it: a solid
/// `DWMWA_CAPTION_COLOR` paints OVER a Mica/Acrylic caption, so any configured
/// material must suppress the tint. Seeded to `DWMSBT_NONE` — the config default —
/// which matches the window's actual state before the first vibrancy call.
static CHROME_BACKDROP: AtomicU32 = AtomicU32::new(DWMSBT_NONE);

/// The live Windows **High Contrast** state (`SPI_GETHIGHCONTRAST` → `HCF_HIGHCONTRASTON`).
/// The ONE reader of that preference in this file; [`AppRtWindows::native_appearance_preferences`]
/// and the caption resolver share it so they can never disagree about whether an HC
/// scheme is active. Fail-safe to `false` (not in high contrast) if the query fails.
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

/// How far ONE wheel notch travels, per the user's Windows mouse settings.
/// [`WheelNotch::Page`] is the "One screen at a time" slider position, which has no
/// line count — the consumer supplies its own viewport height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelNotch {
    Lines(u32),
    Page,
}

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

/// The user's wheel-notch distance, cached.
///
/// aterm banked `LineDelta` straight through, and winit's Windows backend emits
/// exactly ±1.0 per detent — so aterm scrolled ONE line where the Windows default
/// (and every other window on the desktop) scrolls THREE. This is the missing
/// conversion, and it is the OS's number, not a new aterm knob.
///
/// The read is `SystemParametersInfoW`, which is cheap but not free, and the wheel
/// is a hot path (a fast flick delivers dozens of events a second), so the value is
/// cached. The refresh is a coarse TTL rather than the correct
/// `WM_SETTINGCHANGE`/`SPI_SETWHEELSCROLLLINES` invalidation because there is still
/// no Win32 message hook in this build (see `observe_reduce_motion`, which defers
/// its live signal for the same reason, §3.2). A user who drags the Mouse-settings
/// slider therefore sees aterm follow within a few seconds instead of instantly —
/// versus a one-shot `OnceLock`, which would have made it "after you restart aterm".
/// When the message hook lands, this becomes an invalidate + read.
pub(crate) fn wheel_notch_scroll() -> WheelNotch {
    /// Long enough that a flick's worth of events shares one read; short enough
    /// that changing the setting feels live.
    const TTL: std::time::Duration = std::time::Duration::from_secs(3);
    static CACHE: Mutex<Option<(u32, std::time::Instant)>> = Mutex::new(None);

    let now = std::time::Instant::now();
    // A poisoned lock still holds a perfectly good number: a panic elsewhere must
    // not degrade scrolling.
    let mut slot = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((raw, read_at)) = *slot
        && now.duration_since(read_at) < TTL
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
/// consults the terminal background, and only once one has been published (before that
/// there is nothing to match, so `Auto` stays `Auto` and the OS preference wins, exactly
/// as it did before this resolution existed).
///
/// The darkness predicate is [`crate::tab_bar::theme_is_dark`] — deliberately the SAME
/// Rec. 601 luma classifier that already drives the native toolbar strip's appearance
/// (`toolbar::set_strip_dark`). Reusing it, instead of adding a second luminance
/// function here, is what guarantees the caption and the tab strip directly beneath it
/// can never disagree about whether the window is a dark one.
///
/// `high_contrast` ([`high_contrast_active`]) makes `Auto` DEFER TO THE OS again. This
/// is an accessibility guard, not a style choice: under an HC scheme the OS palette owns
/// the caption. The deferral used to be delegated to winit — `try_theme(None)` consults
/// `should_use_dark_mode()` = `should_apps_use_dark_mode() && !is_high_contrast()` — but
/// winit's `is_high_contrast()` is DEAD CODE (its `HIGHCONTRASTA` carries `cbSize: 0`,
/// which `SPI_GETHIGHCONTRAST` rejects; measured always-FALSE on this machine), so the
/// returned `Auto` no longer buys the guard by itself. [`apply_chrome_appearance`]'s HC
/// arm therefore writes the deferral explicitly — see the comment there for why the fix
/// lives aterm-side instead of correcting winit's struct. Explicit `Light`/`Dark` stay
/// hard overrides because a user who typed one asked for it (and pre-fix they already
/// bypassed the same guard, via `set_theme(Some)`).
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
// ~#F3F3F3 light) with a visible seam against the terminal grid — a dark island
// on a light desktop, worse on hued schemes. `DWMWA_CAPTION_COLOR` +
// `DWMWA_TEXT_COLOR` paint the caption the terminal background itself, so the
// window reads as ONE surface (Windows Terminal's caption follows the profile
// background the same way). Four deliberate gates keep this from doing damage:
//
//  * only the `Auto` policy tints — an explicit `window_theme = "light"/"dark"`
//    is the user asking for the STOCK OS caption in that variant, and painting
//    the terminal colour over it would overrule exactly the override the policy
//    doc promises is hard;
//  * High Contrast suppresses it — the HC palette owns the caption (the same
//    accessibility deferral `resolve_chrome_theme` performs for immersive-dark);
//  * an active backdrop material suppresses it — a solid caption colour paints
//    over Mica/Acrylic, silently undoing the user's `background_material`;
//  * DWM's own unfocused caption treatment dies with a pinned COLORREF (a fixed
//    colour applies to BOTH focus states), so the resolver derives a second,
//    DIMMED text tone and the redraw path re-applies on every focus flip. The
//    caption FILL deliberately stays identical in both states: the grid beneath
//    it does not dim on focus loss, and a caption that darkened alone would
//    re-introduce the very seam the tint removes. Dimming the TITLE TEXT is what
//    stock DWM does for unfocused windows, and is what this reproduces.

/// How far the caption title text blends TOWARD the caption fill while the window
/// is unfocused. 0.45 lands near DWM's own inactive-caption text (roughly #999 on
/// white): clearly dimmed at a glance, still legible. Deliberately far stronger
/// than the tab strip's pinned 0.15 label dim — the strip label must stay readable
/// as CONTENT, while the caption title is duplicated identity the OS itself dims.
const CAPTION_TEXT_UNFOCUSED_DIM: f32 = 0.45;

/// Resolve the caption tint into the exact `(DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR)`
/// attribute payloads: COLORREFs when the tint is active, the `DWMWA_COLOR_DEFAULT`
/// reset sentinel (returning the caption to DWM's palette, unfocused dimming
/// included) when any gate defers. Pure, so the whole gate ladder is testable
/// without a window.
///
/// The ink is white-on-dark / black-on-light by the SAME Rec. 601 classifier
/// ([`crate::tab_bar::theme_is_dark`]) that resolves the immersive-dark caption,
/// so the tint and the dark-mode caption buttons can never disagree about which
/// side of the split the window is on. Unfocused, the ink blends toward the
/// caption fill by [`CAPTION_TEXT_UNFOCUSED_DIM`] via the shared
/// [`crate::chrome_band::mix3`] (one blend for every chrome surface, per its doc).
fn caption_tint(
    policy: WindowTheme,
    bg: u32,
    backdrop: u32,
    high_contrast: bool,
    focused: bool,
) -> (u32, u32) {
    if !matches!(policy, WindowTheme::Auto)
        || high_contrast
        || backdrop != DWMSBT_NONE
        || bg == CHROME_BG_UNKNOWN
    {
        return (DWMWA_COLOR_DEFAULT, DWMWA_COLOR_DEFAULT);
    }
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
    let ink_rgb =
        (u32::from(ink[0]) << 16) | (u32::from(ink[1]) << 8) | u32::from(ink[2]);
    (colorref_swap(bg), colorref_swap(ink_rgb))
}

/// The `(caption, text)` attribute pair last WRITTEN per HWND, so the per-frame
/// upkeep ([`verify_chrome_appearance`]) costs a lock and a compare in the steady
/// state and only pays the two `DwmSetWindowAttribute` calls when the desired
/// state actually moved (a theme change, a material change, or a focus flip). A
/// linear Vec, not a map: an aterm process has a handful of windows. Entries are
/// never removed — a few bytes per window ever created — and staleness across an
/// OS-recycled HWND value cannot mis-skip, because every event seam calls
/// [`apply_caption_tint`] with `force`, which writes unconditionally.
static CAPTION_TINT_APPLIED: Mutex<Vec<(isize, u64)>> = Mutex::new(Vec::new());

/// Resolve and write the caption tint for `window`. `force` (the event seams:
/// attach, theme publish, config reload, OS flip) writes even when the cache says
/// the state is current — those seams are rare and must also heal a caption some
/// other process state clobbered; the per-frame caller passes `false` and relies
/// on the cache. Best-effort: on Windows 10 both writes fail `E_INVALIDARG` and
/// are dropped, leaving the caption immersive-dark-only as before.
fn apply_caption_tint(window: &Window, hwnd: isize, policy: WindowTheme, force: bool) {
    let (caption, text) = caption_tint(
        policy,
        CHROME_BG.load(Ordering::Relaxed),
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

/// Push the resolved caption variant at the window: winit's `set_theme` for the
/// uxtheme side (`SetWindowTheme` + the undocumented `WCA_USEDARKMODECOLORS`, which is
/// all vendored winit actually drives — see `vendor/winit/.../dark_mode.rs`) AND an
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
    // Refresh the High-Contrast latch HERE, on the rare event-driven path, so the
    // per-frame guard can resolve from an atomic instead of an `SPI_GETHIGHCONTRAST`
    // syscall.
    let high_contrast = high_contrast_active();
    CHROME_HIGH_CONTRAST.store(high_contrast, Ordering::Relaxed);
    let resolved = resolve_chrome_theme(policy, CHROME_BG.load(Ordering::Relaxed), high_contrast);
    window.set_theme(window_theme_to_winit(resolved));
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
        // `HIGHCONTRASTA` with `cbSize: 0` (vendor/winit/.../dark_mode.rs), which
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
    // Caption tint (H3): this is the same seam every theme publish re-runs
    // (`window_set_background_color` → `resync_chrome_appearance` → here), so a
    // live theme edit re-tints the caption in the same breath that re-resolves
    // immersive-dark. `force` because the event seams must also heal a tint that
    // was clobbered outside our view.
    apply_caption_tint(window, hwnd, policy, /* force= */ true);
}

/// Re-apply the caption appearance for `window` under the LATCHED config policy.
/// The re-entry point for events that can invalidate an earlier resolution without
/// passing through `window_set_appearance`: a terminal theme change (new background →
/// possibly a new answer) and an OS light/dark flip (winit's own `WM_SETTINGCHANGE`
/// handler re-themes the non-client area from the OS preference whenever the window's
/// `preferred_theme` is `None`, which for `Auto` it always is, so our resolved caption
/// must be re-asserted AFTER winit has had its say).
pub(crate) fn resync_chrome_appearance(window: &Window) {
    apply_chrome_appearance(
        window,
        chrome_policy_from_code(CHROME_POLICY.load(Ordering::Relaxed)),
    );
}

/// Does the caption need re-asserting, given the dark-mode value we WANT and the value
/// `DWMWA_USE_IMMERSIVE_DARK_MODE` currently reads back as?
///
/// Split out as a pure function so the per-frame policy is testable without a window.
/// `None` (attribute not gettable on this Windows build) deliberately answers `false`:
/// with no evidence that anything was clobbered, re-writing the caption on every frame
/// would be pure churn — a not-gettable build falls back to the event-driven seams.
fn chrome_reassert_needed(want_dark: bool, applied: Option<u32>) -> bool {
    match applied {
        Some(v) => (v != 0) != want_dark,
        None => false,
    }
}

/// Per-frame guard that the caption on glass is still the one we resolved — the answer
/// to the ONE clobber the event-driven seams structurally cannot see.
///
/// WHY A FRAME PATH AND NOT AN EVENT. winit's `WM_SETTINGCHANGE` handler
/// (`vendor/winit/.../event_loop.rs`) calls `try_theme()` on EVERY such message when
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
/// WHY NOT THE `WM_SETTINGCHANGE` HOOK. That is the durable fix `docs/NATIVE_WINDOWS_DESIGN.md`
/// §3.2/§4 specifies and it stays the right end state, but it is a real piece of work
/// this change should not smuggle in: §3.2 forbids `SetWindowLongPtrW` double-subclassing
/// and routes every consumer through ONE hook patched into `vendor/winit`, and winit's
/// existing `with_msg_hook` cannot serve it (a broadcast `WM_SETTINGCHANGE` arrives by
/// `SendMessage`, so it is delivered straight to the `WndProc` and never appears in the
/// `GetMessage` stream the hook inspects) — and the hook fires BEFORE `DispatchMessageW`,
/// i.e. before the clobber it would have to undo.
///
/// WHY THIS IS CHEAP ENOUGH TO RUN PER FRAME. It never requests a redraw (it only
/// writes window attributes; re-introducing a wake loop here would be worse than the
/// bug). First the CAPTION TINT upkeep — [`apply_caption_tint`] without `force` —
/// which is how a FOCUS FLIP reaches the caption at all: `on_focus` requests a
/// redraw, the redraw lands here, and the tint's unfocused text dim gets applied.
/// That upkeep costs three relaxed atomics, winit's cached focus flag (a lock read),
/// and a compare against the per-HWND last-applied cache; it pays its two
/// `DwmSetWindowAttribute` calls only when the desired `(colour, focused)` state
/// actually moved. Then the immersive-dark ladder, three progressively narrower gates:
///  1. two relaxed atomic loads; a resolved `Auto` with no High-Contrast scheme (no
///     published background yet) means the OS legitimately owns the caption —
///     return. Under HIGH CONTRAST the deferral is now an EXPLICIT immersive-dark
///     OFF (winit's own HC probe is dead — see the HC arm in
///     [`apply_chrome_appearance`]), so HC keeps descending the ladder with
///     `want_dark = false` and gets re-asserted like any other override.
///  2. `window.theme()`, an uncontended lock read and NOT a syscall, is winit's
///     `current_theme`, which it refreshes from the OS on every `WM_SETTINGCHANGE`. When
///     it already agrees with our resolution, winit's clobber writes exactly the value we
///     want and there is nothing to defend — return. This is the common case (a dark grid
///     on a dark desktop), so a normal frame pays a handful of atomics and two lock reads.
///  3. only while we are actively OVERRIDING the desktop does the frame pay one
///     `DwmGetWindowAttribute` read, and only a disagreement writes.
///
/// HONEST LIMIT: gate 3 sees the clobber iff DWM keeps `DWMWA_USE_IMMERSIVE_DARK_MODE`
/// and the undocumented `WCA_USEDARKMODECOLORS` winit writes in ONE per-window dark bit.
/// They are widely interchangeable (winit gets dark captions on Win11 through WCA alone),
/// but if a build kept them apart this guard would read back our own value, decline to
/// write, and simply cost one read — never a regression, just no cure. And a window that
/// never presents another frame is healed only when it does. The §3.2 hook is what closes
/// both, exactly.
pub(crate) fn verify_chrome_appearance(window: &Window) {
    // Hoisted above the gates (it used to sit below them) because the tint upkeep
    // needs it too; `hwnd_of` reads winit's stored handle — cheap, not a syscall.
    let Some(hwnd) = hwnd_of(window) else {
        return;
    };
    let policy = chrome_policy_from_code(CHROME_POLICY.load(Ordering::Relaxed));
    let high_contrast = CHROME_HIGH_CONTRAST.load(Ordering::Relaxed);
    // Caption-tint upkeep FIRST and unconditionally: focus flips arrive on this
    // path (see the fn doc), and the tint has gates of its own that are
    // independent of the immersive-dark ladder's early returns below.
    apply_caption_tint(window, hwnd, policy, /* force= */ false);
    let want_dark = match resolve_chrome_theme(
        policy,
        CHROME_BG.load(Ordering::Relaxed),
        high_contrast,
    ) {
        WindowTheme::Dark => true,
        WindowTheme::Light => false,
        // The EXPLICIT High-Contrast deferral (immersive-dark OFF; winit's own HC
        // probe is dead — see the HC arm in `apply_chrome_appearance`) is an
        // override like any other and must survive winit's re-themes.
        WindowTheme::Auto if high_contrast => false,
        WindowTheme::Auto => return, // the OS owns it; winit's re-theme IS the answer
    };
    if matches!(window.theme(), Some(winit::window::Theme::Dark)) == want_dark {
        return; // OS preference already agrees — a clobber cannot do damage
    }
    if chrome_reassert_needed(want_dark, dwm_get_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE)) {
        apply_chrome_appearance(window, policy);
    }
}

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
// (`set_end_session_snapshot`, driven from `about_to_wait` behind the
// `end_session_snapshot_due` debounce), and the handler only does file I/O on data it
// owns: bounded staleness (≤ the TTL below) against a shutdown that would otherwise
// lose EVERYTHING.
//
// HONEST LIMITS. (1) The snapshot is up to ~2 s stale — a tab opened in the final
// second before "Restart now" may be missing; every prior window/tab/cwd survives.
// (2) A shutdown the user then CANCELS leaves session.toml on disk while aterm keeps
// running: harmless for this instance (the graceful-exit write later overwrites it),
// and at worst a SECOND instance launched before that exit restores a copy of the
// layout — rare, self-correcting, and strictly better than the data loss this block
// removes. (3) A hard crash gets no WM_ENDSESSION; that lane is the crash-marker
// banner (`logging::take_crash_evidence`) + `RegisterApplicationRestart`, not this.

/// How stale the published end-session snapshot may grow before `about_to_wait`
/// refreshes it. Long enough that a busy terminal pays one cheap layout walk every
/// couple of seconds, short enough that an end-session save is current for anything
/// a human does between opening a tab and clicking Start ▸ Restart.
const END_SESSION_SNAPSHOT_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// The last layout snapshot the event loop published, plus its generation — the
/// ONLY data the end-session handler touches. `None` until the first publish (or
/// when session restore is configured off).
static END_SESSION_SNAPSHOT: Mutex<Option<(crate::restore::RestoreManifest, u64)>> =
    Mutex::new(None);

/// Monotonic generation of the published snapshot; [`END_SESSION_FLUSHED`] records
/// the generation last written to disk so the flurry of per-window
/// WM_QUERYENDSESSION + WM_ENDSESSION deliveries costs ONE durable write, not one
/// per window per message.
static END_SESSION_GEN: AtomicU64 = AtomicU64::new(0);
static END_SESSION_FLUSHED: AtomicU64 = AtomicU64::new(0);

/// When the snapshot was last refreshed (the TTL debounce for `about_to_wait`).
static END_SESSION_REFRESHED: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// hwnd → the wndproc we displaced (winit's), for `CallWindowProcW` chaining. A
/// `Vec` because the window count is a handful and `Vec::new` is const (no lazy
/// init inside a wndproc). Entries are dropped on WM_NCDESTROY.
static PREV_WNDPROC: Mutex<Vec<(isize, isize)>> = Mutex::new(Vec::new());

/// `GWLP_WNDPROC`.
const GWLP_WNDPROC: i32 = -4;
const WM_QUERYENDSESSION: u32 = 0x0011;
const WM_ENDSESSION: u32 = 0x0016;
const WM_NCDESTROY: u32 = 0x0082;

/// Has the TTL lapsed since the last snapshot publish? `true` also ARMS the next
/// interval, so the caller refreshes at most once per TTL. Main-thread only (the
/// `about_to_wait` seam); the mutex is for interior mutability, not contention.
pub(crate) fn end_session_snapshot_due() -> bool {
    let now = std::time::Instant::now();
    let mut slot = END_SESSION_REFRESHED
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if slot.is_some_and(|at| now.duration_since(at) < END_SESSION_SNAPSHOT_TTL) {
        return false;
    }
    *slot = Some(now);
    true
}

/// Publish the layout snapshot the end-session handler may write ([`Some`]), or
/// retract it ([`None`] — session restore configured off, so an end-session must
/// NOT write a manifest the user asked not to keep).
pub(crate) fn set_end_session_snapshot(manifest: Option<crate::restore::RestoreManifest>) {
    let mut slot = END_SESSION_SNAPSHOT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *slot = manifest.map(|m| (m, END_SESSION_GEN.fetch_add(1, Ordering::Relaxed) + 1));
}

/// Durably write the published snapshot — once per generation, skipping empty
/// layouts (the graceful-exit path applies the same `is_empty` gate). Runs inside
/// the wndproc, so it touches ONLY the statics above plus the filesystem.
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

/// The chained window procedure: persist the session on end-session, forward
/// EVERYTHING (including those two messages) to winit's original proc.
///
/// WM_QUERYENDSESSION wants TRUE ("OK to end the session") — winit has no arm for
/// it, so the chain lands in `DefWindowProc`, which returns exactly that; writing
/// the manifest BEFORE forwarding means the answer never races the save.
/// WM_ENDSESSION with `wparam == TRUE` ("the session IS ending — the process can
/// be terminated any time after this returns") is the hard flush: a normal
/// shutdown already wrote at QUERY time and the generation guard makes this a
/// no-op, but a critical shutdown may skip the query phase entirely.
unsafe extern "system" fn end_session_wndproc(
    hwnd: isize,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    let prev = {
        let map = PREV_WNDPROC.lock().unwrap_or_else(|e| e.into_inner());
        map.iter().find(|(h, _)| *h == hwnd).map(|&(_, p)| p)
    };
    if msg == WM_QUERYENDSESSION || (msg == WM_ENDSESSION && wparam != 0) {
        flush_end_session_snapshot();
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
        PREV_WNDPROC
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|&(h, _)| h != hwnd);
    }
    result
}

/// Chain [`end_session_wndproc`] in front of winit's window procedure for
/// `window`. Idempotent per window (keyed on the hwnd), so any attach-time seam
/// may call it. Holding the map lock across `SetWindowLongPtrW` is deadlock-free:
/// a GWLP_WNDPROC swap sends no messages, and sent-message dispatch (the only
/// path into the wndproc) happens only inside message-retrieval calls, none of
/// which occur under this lock.
fn install_end_session_guard(window: &Window) {
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
            end_session_wndproc as *const () as isize,
        )
    };
    if prev != 0 {
        map.push((hwnd, prev));
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
        // Latch the terminal background for the caption resolver BEFORE anything can
        // bail out: this is the only route by which the Windows arm learns the theme
        // bg, and it must stay current even for a window with no HWND yet (headless /
        // not-yet-realized), because the NEXT window to realize resolves from it.
        // Masked to 24 bits so a caller passing a stray alpha byte can never look like
        // the `CHROME_BG_UNKNOWN` sentinel.
        CHROME_BG.store(bg & 0x00FF_FFFF, Ordering::Relaxed);
        // Every publisher of this value (attach, `apply_theme_live`, the `reload_config`
        // theme commit) is exactly a "the terminal background may have changed" event,
        // so it is also the right place to re-run the `Auto` caption resolution — a
        // dark→light theme edit must repaint the caption, not just the class brush.
        resync_chrome_appearance(window);
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        let colorref = colorref_swap(bg);
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

    /// L1 (early reveal): force the class-brush themed erase onto glass NOW.
    ///
    /// Between the early `set_visible` and the backend join's return, the
    /// event-loop thread is blocked and processes no queued messages — the
    /// freshly shown window would present its client area *unpainted* (white)
    /// for the whole join, which is exactly the flash the class brush exists to
    /// prevent. `RDW_ERASENOW` delivers `WM_ERASEBKGND` synchronously (a direct
    /// same-thread wndproc call, no queue involved); winit does not intercept
    /// that message, so `DefWindowProc` fills with the theme brush installed by
    /// [`Self::window_set_background_color`] — which therefore MUST run first.
    /// Best-effort like every chrome call here: no HWND yet → nothing to paint.
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

    /// Apply the native title-bar appearance: winit's `set_theme` for the uxtheme side,
    /// an explicit `DWMWA_USE_IMMERSIVE_DARK_MODE` write (winit drives only the
    /// undocumented `WCA_USEDARKMODECOLORS`, never that documented attribute, so the
    /// value `aterm-ctl chrome` reads back has to come from us), and the rounded-corner
    /// preference winit does not expose. Best-effort; a window with no `HWND` yet just
    /// gets the winit theme.
    ///
    /// Config `Light`/`Dark` are hard overrides. `Auto` is resolved HERE, on Windows
    /// only, against the terminal background rather than the OS preference — see the
    /// "Auto-theme caption resolution" block above for why a light DWM caption over a
    /// dark grid is a defect on this platform but not on macOS.
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        // Latch the policy so the background-publish and OS-flip seams can re-run this
        // same resolution later without the caller re-supplying it.
        CHROME_POLICY.store(chrome_policy_code(theme), Ordering::Relaxed);
        apply_chrome_appearance(window, theme);
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

    /// INTENTIONAL no-op: `contentsGravity` models CoreAnimation's rescale of a
    /// not-yet-repainted layer, which is macOS-specific. The DXGI swapchain is
    /// presented into the HWND's client area directly — there is no intermediate
    /// layer holding a stale drawable to anchor. Documented rather than silently
    /// empty.
    fn window_anchor_surface_top_left(&self, _window: &Window) {}

    /// No CoreAnimation layer to read: the DXGI swapchain is presented into the
    /// HWND's client area directly, so no intermediate layer holds a stale drawable
    /// whose gravity could rescale it — nothing to report.
    fn window_surface_presentation(&self, _window: &Window) -> Option<(String, f64, bool)> {
        None
    }

    /// Install / update / remove the DWM **system backdrop** (`DWMWA_SYSTEMBACKDROP_TYPE`):
    /// `background_material` maps to Mica (`UnderWindow`) / Mica-Alt (`Sidebar`) /
    /// Acrylic (`Hud`), and `None` (the default) to `DWMSBT_NONE` — a plain opaque
    /// window. This is `background_material`'s Windows DWM half.
    ///
    /// WHERE THE MATERIAL ACTUALLY SHOWS depends on the client presentation path
    /// (`aterm-ctl chrome` → `client=`):
    ///   * `client=opaque` (the default HWND swapchain): the CAPTION only. The
    ///     client area — window padding included — is the offscreen framebuffer,
    ///     presented through a swapchain whose sole composite mode is Opaque, so
    ///     it covers the backdrop completely. (An earlier version of this comment
    ///     claimed the material showed in "caption/padding"; the padding half was
    ///     false — H1 — because padding is interior to the framebuffer.)
    ///   * `client=visual` (H1: material configured at launch, GPU renderer):
    ///     the caption AND the client-area margins — the padding gutters and the
    ///     chrome-bleed strip carry per-pixel alpha through the DirectComposition
    ///     visual swapchain and the material really blends through them. The GRID
    ///     BODY stays opaque either way (the ratified design; renderer clears it).
    ///
    /// `translucent` (the M5 opacity flag) is not consulted here: Mica needs no
    /// window-level opacity flip on Windows (the alpha lives in the presented
    /// frame). Best-effort; a silent no-op pre-22H2.
    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: BackgroundMaterial,
        _translucent: bool,
        _bg: u32,
    ) {
        // Latch BEFORE the hwnd bail-out (mirroring `window_set_background_color`'s
        // ordering rationale): the caption-tint resolver must suppress its solid
        // `DWMWA_CAPTION_COLOR` whenever a backdrop material is configured, or the
        // tint would paint over the Mica/Acrylic caption the user asked for.
        CHROME_BACKDROP.store(backdrop_for(material), Ordering::Relaxed);
        if let Some(hwnd) = hwnd_of(window) {
            dwm_set_u32(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, backdrop_for(material));
        }
        // H1 honesty: a NON-none material reaching a window whose client path is
        // the plain HWND swapchain (material turned on by a live reload after an
        // opaque launch, or the GPU init failed) can only ever style the caption
        // — the visual swapchain is an instance-level choice made at launch.
        // Warn once so "I set Mica and only the title bar changed" has a printed
        // answer naming the fix, instead of a silent visual shortfall. When the
        // visual request was made and then WITHDRAWN this run (GPU init failed,
        // DirectComposition unavailable at the first attach, device lost), the
        // fix is NOT a restart — that arm already printed the real cause, so
        // point at it instead of advising a relaunch that repeats the failure.
        if material != BackgroundMaterial::None && !aterm_gpu::dx12_visual_swapchain_active() {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                if aterm_gpu::dx12_visual_swapchain_withdrawn() {
                    eprintln!(
                        "aterm-gui: background_material is styling the caption only \
                         (client=opaque): the client-area backdrop path was withdrawn \
                         this run — see the diagnostic above"
                    );
                } else {
                    eprintln!(
                        "aterm-gui: background_material is styling the caption only \
                         (client=opaque): the client-area backdrop engages when the material \
                         is set at launch with the GPU renderer and hdr_glow off — restart \
                         aterm to see it in the padding"
                    );
                }
            });
        }
        // A material change flips the tint gate in BOTH directions (`none` → Mica
        // must clear the tint; Mica → `none` must restore it), so re-run the
        // caption resolution under the latched policy.
        resync_chrome_appearance(window);
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
    ///
    /// Also chains the end-session wndproc onto this window here — piggy-backed
    /// because this is the ONE `AppRt` seam the Windows arm receives exactly once
    /// per window at attach with the `&Window` in hand (the alternative,
    /// `window_set_background_color`, re-runs on every theme publish; the install
    /// is idempotent either way, this is just the honest cadence).
    fn install_toolbar(
        &self,
        window: &Window,
        proxy: &EventLoopProxy<Wake>,
        wid: WindowId,
    ) -> Option<toolbar::ToolbarHandle> {
        install_end_session_guard(window);
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

    /// The native close/quit confirmation: a `TaskDialogIndirect` dialog whose
    /// affirmative button carries the prompt's REAL verb ("Close"/"Quit") with a
    /// stock Cancel — the custom labelling a fixed `MessageBoxW` Yes/No could not
    /// deliver, which is why this stayed a stub until the comctl32 v6 manifest
    /// shipped. Buttons mirror the macOS `NSAlert`: proceed is the Return
    /// default, Escape cancels. `None` (no v6 comctl32 in this process — a test
    /// binary or resource-less dev build — or a failed dialog) falls back to the
    /// caller's titlebar-warning confirm, exactly as before.
    ///
    /// Note the POLICY is not this method's: `quit_safety::confirm_prompt_windows`
    /// decides Windows prompts per the WT convention (multiple tabs or busy —
    /// never a single idle tab), so an idle one-tab Alt+F4 never even reaches here.
    fn confirm(&self, title: &str, body: &str, proceed_label: &str) -> Option<bool> {
        crate::win32::confirm_proceed_cancel(title, body, proceed_label)
    }

    /// A no-op — but NOT because Windows lacks the gesture class this hook exists
    /// for. A USER-driven close routes through winit `CloseRequested` and is
    /// confirmed there; the OS-driven end (shutdown / restart / sign-out), which
    /// this macOS `terminate:` seam also covers there, arrives as a SENT
    /// `WM_QUERYENDSESSION` that no winit event carries — on Windows it is
    /// handled by the end-session wndproc chain installed per window at attach
    /// (see `install_end_session_guard`), which persists the session rather than
    /// prompting: blocking a machine shutdown on a dialog is hostile, and with
    /// the layout saved there is nothing left to warn about.
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
        // ONE `SPI_GETHIGHCONTRAST` reader in this file — shared with the caption
        // resolver, which must defer `Auto` to the OS palette under an HC scheme.
        let high_contrast = high_contrast_active();
        // Refresh the caption resolver's latch with the fresh answer, keeping the
        // promise in the latch's doc that the two readers can never disagree. This
        // matters since the H5 partial: focus-gain re-samples these preferences, so
        // an HC flip made while aterm was in the background lands here FIRST —
        // without the store, the frame guard would keep defending a stale non-HC
        // caption until some other seam ran `apply_chrome_appearance`.
        CHROME_HIGH_CONTRAST.store(high_contrast, Ordering::Relaxed);
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
    ///
    /// PARTIAL, until that hook lands: `App::resample_os_preferences` re-runs the
    /// reduce-motion and appearance-preference queries on this platform's
    /// `ThemeChanged` and focus-gain seams (two `SystemParametersInfoW` reads on
    /// rare events), so flipping "Animation effects" / High Contrast / transparency
    /// reaches a RUNNING window on its next refocus instead of only the next
    /// window attach. The hook remains the durable fix (it also catches flips made
    /// while aterm stays focused).
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
        CHROME_BG_UNKNOWN, DEFAULT_WHEEL_SCROLL_LINES, DWMSBT_MAINWINDOW, DWMSBT_NONE,
        DWMSBT_TABBEDWINDOW, DWMSBT_TRANSIENTWINDOW, DWMWA_COLOR_DEFAULT, WHEEL_PAGESCROLL,
        WheelNotch, backdrop_for, caption_tint, chrome_policy_code, chrome_policy_from_code,
        chrome_reassert_needed, colorref_swap, resolve_chrome_theme,
        validate_window_capture_transfer, wheel_notch_from_raw, wheel_notch_scroll,
    };
    use crate::app_config::{BackgroundMaterial, WindowTheme};

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

    /// The defect this guards: with `window_theme = "auto"` and a LIGHT system theme,
    /// DWM used to paint a white caption directly above aterm's near-black grid. On
    /// Windows `Auto` therefore resolves against the TERMINAL background, not the OS
    /// preference — a dark bg gets the immersive-dark caption whatever the desktop is
    /// doing, and a light bg gets the light one.
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

    /// ACCESSIBILITY: under a Windows High Contrast scheme `Auto` refuses to resolve
    /// from the terminal background — the HC palette, not the terminal, owns the
    /// caption. (The returned `Auto` is no longer a delegation to winit's
    /// `is_high_contrast()` guard, which is dead code — `cbSize: 0` — but the marker
    /// `apply_chrome_appearance`'s HC arm turns into an EXPLICIT immersive-dark-off
    /// write.) Explicit `Light`/`Dark` remain hard overrides (they bypassed the same
    /// guard before this resolution existed, so nothing regresses for a user who typed one).
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

    /// The per-frame guard writes ONLY on evidence that the caption was clobbered: a
    /// readback that disagrees with the resolved answer. Agreement is the steady state
    /// and must not write (that would be a caption re-theme every frame), and a build
    /// where the attribute is not gettable must not write either — with no evidence,
    /// churning is strictly worse than leaving it to the event-driven seams.
    #[test]
    fn the_frame_guard_writes_only_on_a_disagreeing_readback() {
        assert!(chrome_reassert_needed(true, Some(0)), "dark wanted, light applied");
        assert!(chrome_reassert_needed(false, Some(1)), "light wanted, dark applied");
        assert!(!chrome_reassert_needed(true, Some(1)));
        assert!(!chrome_reassert_needed(false, Some(0)));
        assert!(!chrome_reassert_needed(true, None), "no readback ⇒ no evidence ⇒ no write");
        assert!(!chrome_reassert_needed(false, None));
    }

    /// Explicit config values stay HARD overrides: `Auto` is the only policy that ever
    /// consults the background, so a user who asked for a light title bar over a dark
    /// grid keeps getting one.
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

    /// Before any background has been published there is nothing to match, so `Auto`
    /// stays `Auto` and the OS preference keeps the window — the pre-fix behaviour,
    /// retained so a not-yet-themed window is never pinned to a guess.
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

    /// The wheel-notch decode: the ordinary case is a line count, the all-ones
    /// sentinel is the "One screen at a time" slider position (NOT 4 294 967 295
    /// lines, which is what banking it as a number would have meant), and an
    /// explicit 0 — "wheel scrolling off" — is honoured rather than clamped up.
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

    /// The live read is cached, and both reads agree. The VALUE belongs to the
    /// machine's mouse settings, so this asserts only that the query is answerable
    /// and stable — never a specific number.
    #[test]
    fn wheel_notch_scroll_is_stable_across_calls() {
        assert_eq!(wheel_notch_scroll(), wheel_notch_scroll());
    }

    /// COLORREF is `0x00BBGGRR` while themes store `0x00RRGGBB`: the swap must move
    /// R↔B, be its own inverse (one helper serves the write AND the introspection
    /// readback), and drop any stray top byte.
    #[test]
    fn colorref_swap_reverses_r_and_b_and_is_an_involution() {
        assert_eq!(colorref_swap(0x11_1318), 0x18_1311);
        assert_eq!(colorref_swap(colorref_swap(0xAB_CDEF)), 0xAB_CDEF);
        assert_eq!(colorref_swap(0xFF00_0000), 0, "top byte must not leak into a channel");
    }

    /// H3 happy path: under the `Auto` policy with no backdrop and no HC scheme, the
    /// caption fill IS the terminal background (R/B-swapped into COLORREF order) and
    /// the focused title text is the full-strength ink for that background's side of
    /// the light/dark split — white on a dark grid, black on a light one.
    #[test]
    fn caption_tint_paints_the_terminal_background_under_auto() {
        // The live audit machine's dark default (#111318).
        let (caption, text) =
            caption_tint(WindowTheme::Auto, 0x11_1318, DWMSBT_NONE, false, true);
        assert_eq!(caption, 0x18_1311, "caption = theme bg in COLORREF byte order");
        assert_eq!(text, 0x00FF_FFFF, "dark grid gets white title text");
        // Solarized-light paper → black ink.
        let (caption, text) =
            caption_tint(WindowTheme::Auto, 0xFD_F6E3, DWMSBT_NONE, false, true);
        assert_eq!(caption, 0xE3_F6FD);
        assert_eq!(text, 0x0000_0000, "light grid gets black title text");
    }

    /// H3 gate 4: a pinned COLORREF kills DWM's own unfocused treatment, so the
    /// resolver supplies the dim itself — on the TEXT only. The caption fill must
    /// stay byte-identical across the focus flip (the grid beneath it does not dim,
    /// and a caption that darkened alone would re-introduce the seam the tint
    /// removes), while the text moves strictly toward the fill without reaching it
    /// (dimmed, not erased).
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
        let expected_rgb = (u32::from(expected[0]) << 16)
            | (u32::from(expected[1]) << 8)
            | u32::from(expected[2]);
        assert_eq!(text_unfocused, colorref_swap(expected_rgb));
    }

    /// The other three gates, each of which must yield the FULL reset pair (both
    /// attributes back to `DWMWA_COLOR_DEFAULT`, restoring DWM's palette and its
    /// native dimming): a High Contrast scheme (the accessibility deferral), an
    /// active backdrop material (a solid caption would paint over Mica), an
    /// explicit `Light`/`Dark` policy (the user asked for the STOCK OS caption in
    /// that variant), and a not-yet-published background (nothing to tint with).
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
                caption_tint(WindowTheme::Auto, CHROME_BG_UNKNOWN, DWMSBT_NONE, false, focused),
                default,
                "no published background yet — nothing to tint with"
            );
        }
    }
}

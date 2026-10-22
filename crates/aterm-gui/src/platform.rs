// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Platform adapter for window chrome, menus, toolbar state, notifications and
//! appearance preferences. Shared application code calls [`AppRt`]; its target
//! implementation owns these native operations.
//!
//! Platform implementations back the one trait:
//!
//! * [`AppRtMacOS`] uses AppKit for window appearance and forwards menu, toolbar
//!   and notification work to [`crate::menu`], [`crate::toolbar`] and
//!   [`crate::notify`].
//! * `AppRtWindows` provides Windows-native chrome, notifications and preferences.
//! * [`AppRtLinux`] applies winit's window theme and maintains the in-memory
//!   toolbar model on Linux/BSD. Unsupported native chrome operations are inert;
//!   notification delivery uses `notify::spawn_delivery`: freedesktop
//!   `Notify` over the session bus on Linux, a channel-draining stub on the
//!   BSDs.
//!
//! [`PlatformAppRt`] is the target-selected concrete type the `App` stores. Its
//! preference-probe flag keeps test Apps on an inert arm of the same runtime seam.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use aterm_window::event_loop::EventLoopProxy;
use aterm_window::window::Window;

use crate::app_config::WindowTheme;
use crate::notify::NotifyMsg;
use crate::{Wake, WindowId, menu, toolbar};

/// One complete toolbar tab-strip model, borrowed for a single [`AppRt::set_toolbar_tabs`]
/// push: parallel per-tab slices paired by index (title, id, strip metadata, hover tooltip,
/// chrome extension), plus the active index.
#[derive(Clone, Copy)]
pub(crate) struct ToolbarTabsModel<'a> {
    pub(crate) titles: &'a [String],
    pub(crate) ids: &'a [crate::tab_model::TabId],
    pub(crate) metadata: &'a [crate::tab_bar::TabStripMetadata],
    pub(crate) tooltips: &'a [Option<String>],
    pub(crate) ext: &'a [crate::session_chrome::TabChromeExt],
    pub(crate) active: usize,
    /// The window's needs pill (§5), drawn at the strip's trailing end
    /// before "+": `None` while its border is unlit.
    pub(crate) pill: Option<&'a crate::app_presence::NeedsPillModel>,
}

/// One editing command a menu key equivalent must hand to the inline rename
/// field instead of to the terminal while an editor is open (see
/// [`AppRt::rename_editor_edit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenameEditorEdit {
    /// ⌘C — copy the field's selection.
    Copy,
    /// ⌘V — paste into the field.
    Paste,
    /// ⌘A — select the field's whole value.
    SelectAll,
}

/// The native application-runtime seam for each platform's OS integrations.
/// Preference reads have a per-instance inert test arm.
pub(crate) trait AppRt {
    /// Paint the OS window background the terminal's theme background colour
    /// (`bg`, as `0x00RRGGBB`) so the transparent titlebar / bare compact bar reads
    /// as a seamless extension of the terminal body (Windows: the class brush the
    /// client area erases to; X11: the background the server paints until a frame
    /// covers it). No-op on Wayland.
    fn window_set_background_color(&self, window: &Window, bg: u32);

    /// Apply the window-chrome appearance: match the NSWindow colour space to softbuffer's
    /// device-RGB content (dropping a per-frame CoreAnimation gamut conversion) and set the
    /// titlebar light/dark appearance from `theme`.
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme);

    /// Synchronously paint the window's themed backdrop, without waiting for the event loop to
    /// pump a paint message. The Windows warm-launch early reveal shows the first window, then
    /// blocks the event-loop thread on the backend join, and the themed erase (the class brush
    /// `window_set_background_color` installs) runs only on a paint/erase message; without this
    /// the revealed window is an unpainted rectangle for the whole join.
    #[cfg(windows)]
    fn window_flush_backdrop(&self, _window: &Window) {}

    /// Tag the window's GPU swapchain layer (the CAMetalLayer wgpu attaches at surface
    /// creation) with an explicit colour space, so ColorSync interprets the sRGB-encoded bytes
    /// instead of stretching them to the panel's P3 primaries; bytes and readback are untouched.
    /// Call after the wgpu surface exists. Returns the space in effect, or `None` when it cannot
    /// be established: capture then keeps prior metadata or refuses, never assuming the tag took.
    fn window_set_surface_colorspace(
        &self,
        window: &Window,
        cs: SurfaceColorspace,
    ) -> Option<SurfaceColorspace>;

    /// Stop the compositor rescaling the last presented frame during a live drag.
    ///
    /// wgpu (via `raw-window-metal`) renders into a `CAMetalLayer` sublayer that tracks the super
    /// layer by KVO with CoreAnimation's default `contentsGravity` (`resize`). In a drag the
    /// bounds change in AppKit's transaction while our present lands in a later one, so the
    /// compositor stretches the previous drawable and smears the text; anchoring at the top-left
    /// keeps a not-yet-repainted frame at 1:1. Across a Retina/non-Retina move a stale frame
    /// reads at the wrong size until the next one (gravity uses `physical / contentsScale`).
    ///
    /// `raw-window-metal` asks consumers not to set `contentsGravity`, but its KVO observer
    /// writes only `bounds` and `contentsScale`, so the value set at attach stands. Applied once
    /// per window after the wgpu surface exists; a no-op off macOS.
    fn window_anchor_surface_top_left(&self, window: &Window);

    /// Read back the live presentation state of the window's GPU swapchain layer:
    /// `(contents_gravity, contents_scale, contents_are_flipped)`. The anchor
    /// ([`AppRt::window_anchor_surface_top_left`]) prevents a compositor artifact no in-process
    /// capture can see, so `dims` reports whether it is in effect on the live layer, which
    /// `raw-window-metal` or a wgpu reconfigure could revert. `None` where there is no such layer
    /// (off macOS, CPU backend, no window). Read-only: it never establishes what it reports.
    fn window_surface_presentation(&self, window: &Window) -> Option<(String, f64, bool)>;

    /// A frame of `window` was just presented (its swap or its image put
    /// returned). On X11 this answers a window manager's pending resize sync
    /// request (EWMH `_NET_WM_SYNC_REQUEST`, which mutter holds a resize's
    /// picture on until the window has drawn its new size). Default no-op: the
    /// other platforms' compositors pace a resize themselves.
    fn window_frame_presented(&self, _window: &Window) {}

    /// Install / update / remove the window-level `NSVisualEffectView` blurred backdrop and
    /// flip the window and CAMetalLayer to non-opaque so the GPU's translucent (PostMultiplied)
    /// present composites over it. `material` is the resolved `background_material`;
    /// `translucent` is `background_opacity < 1.0` (`aterm_render::vibrancy::is_translucent`);
    /// `bg` (`0x00RRGGBB`) restores the opaque titlebar fill when translucency turns off.
    ///
    /// * `translucent && material != None`: a `behindWindow` effect view behind the Metal
    ///   layer, and a non-opaque window and layer; the desktop shows blurred.
    /// * `translucent && material == None`: the same flip with no blur view.
    /// * `!translucent`: any installed view removed, window and layer opaque with the `bg` fill.
    ///
    /// Best-effort; a no-op off macOS.
    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: crate::app_config::BackgroundMaterial,
        translucent: bool,
        bg: u32,
    );

    /// The EDR maximum of the screen `window` occupies
    /// (`NSScreen.maximumExtendedDynamicRangeColorComponentValue`: 1.0 on SDR, ~1.6 on a MacBook
    /// HDR panel, up to 16 on a Pro Display XDR). Queried at GPU attach and on monitor changes
    /// (`refresh_frame_interval`); the renderer sanitizes it through `aterm_render::hdr`, so
    /// `1.0` off macOS keeps the EDR pass inert.
    fn screen_edr_max(&self, window: &Window) -> f32;

    /// Extend the window's content view under the titlebar
    /// (`NSWindowStyleMaskFullSizeContentView`) so the GPU surface spans the full frame, giving
    /// the effects layer a band above the grid. Applied once at attach per chrome'd terminal
    /// window, never from the config-reload appearance path (which reaches every window,
    /// Settings included). No-op off macOS.
    fn window_set_fullsize_content(&self, _window: &Window) {}

    /// The titlebar band height in logical points, the chrome headroom the effects layer may
    /// draw into: content-view height minus `contentLayoutRect` height, so `0` before the
    /// full-size-content mask, in fullscreen, off macOS and for chromeless windows.
    ///
    /// This is the measured OS band only. Windows' synthetic tab band depends on live config and
    /// the cell box, which an `AppRt` lacks, so `App` derives it (`synthetic_strip_head_px`) and
    /// writes the same `WindowState::head_pts`; everything downstream sees one band.
    fn titlebar_band_pts(&self, _window: &Window) -> f64 {
        0.0
    }

    /// The usable placement area of `window`'s screen: the display minus permanent system
    /// chrome (on macOS the menu bar and Dock, `NSScreen.visibleFrame`), in logical points with a
    /// top-left origin and y growing down, the space `Window::outer_position().to_logical()`
    /// reports. The new-window cascade wraps against it.
    ///
    /// `None` when the platform cannot answer (no native window, an off-screen window, or no
    /// work-area concept); callers fall back to the winit monitor bounds, the full display, which
    /// wraps slightly later but never wrongly. See [`crate::app_window::WorkAreaPts`].
    fn window_work_area_pts(&self, _window: &Window) -> Option<crate::app_window::WorkAreaPts> {
        None
    }

    /// The EDR POTENTIAL of the screen `window` occupies: the most the panel
    /// can show above SDR white, whatever its brightness now (macOS
    /// `NSScreen.maximumPotentialExtendedDynamicRangeColorComponentValue`;
    /// Windows with HDR on, its headroom). `1.0` on an SDR-only screen, and
    /// by default — Linux presents SDR by design. Settings reads it as the
    /// fact "this display shows HDR glow", which an 8-bit surface (HDR glow
    /// off) cannot say.
    fn screen_edr_potential(&self, _window: &Window) -> f32 {
        1.0
    }

    /// Windows scRGB: the display's reference-white scale (`SDR-white / 80`) for the EDR
    /// present, so the grid is not dim (scRGB `1.0` is 80 nits). Default `1.0`, correct on
    /// macOS (the extended-linear layer maps `1.0` to SDR white) and Linux.
    fn screen_sdr_white_scale(&self, _window: &Window) -> f32 {
        1.0
    }

    /// Spawn the process-wide notification delivery thread and return the bounded
    /// `SyncSender` each tab clones into its engine callbacks. macOS, Windows and
    /// Linux deliver; elsewhere this is the channel-draining stub (senders never
    /// block; nothing is delivered).
    /// `own_alerts` is the live `desktop_alerts` switch: while it reads `false` the
    /// thread drops every notice aterm wrote itself ([`NotifyMsg::own`]). `center`
    /// is the instance's notice center: aterm's own voice on macOS (inert headless
    /// and in tests), nothing elsewhere.
    fn send_notification_init(
        &self,
        suppress: Arc<Mutex<HashSet<u64>>>,
        silent: Arc<AtomicBool>,
        own_alerts: Arc<AtomicBool>,
        center: crate::notice_center::NoticeCenter,
    ) -> SyncSender<NotifyMsg>;

    /// Build + install the native application menu bar, returning the retained
    /// action target the caller keeps alive. `None` off macOS (no menu installed).
    fn install_menu(&self, proxy: &EventLoopProxy<Wake>) -> Option<menu::MenuHandle>;

    /// Create the menu-bar operator status item (`status_item.rs`), rendered from `glance`,
    /// returning the handle the caller keeps for the process lifetime. Default `None`.
    fn install_status_item(
        &self,
        _proxy: &EventLoopProxy<Wake>,
        _glance: &crate::status_item::Glance,
    ) -> Option<crate::status_item::StatusItemHandle> {
        None
    }

    /// Bring another aterm instance's windows to the front by pid (the status menu's sibling
    /// rows). macOS only.
    #[cfg(target_os = "macos")]
    fn activate_instance(&self, _pid: u32) -> bool {
        false
    }

    /// Open `url` with the system's handler — the status item's Screen
    /// Sharing (`vnc://<host>`) and the Local Network privacy pane
    /// (`x-apple.systempreferences:`). Default `false`: nothing opened. A
    /// test build opens nothing on any platform: it records the URL
    /// ([`take_opened_urls`]), so no test reaches a handler or WindowServer.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    fn open_url(&self, _url: &str) -> bool {
        false
    }

    /// Put `text` on the system pasteboard (the status item's Copy "ssh
    /// <host>"). Default: the clipboard backend's own write
    /// (`control::pbcopy`), whose test build writes a thread-local board and
    /// never the system's.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    fn pasteboard_set(&self, text: &str) -> bool {
        crate::control::pbcopy(text)
    }

    /// Un-minimize and raise our own `window`: the receiving half of a
    /// `windowing_behavior = "attach"` forward, where `aterm new-tab` spawns a tab in an
    /// instance the user may not see. Default `false`. Not winit's `focus_window()`: on Windows
    /// it early-returns for a minimized window and steals the foreground with a synthetic Alt
    /// keypress, which a background tab-spawn must not inject.
    fn window_bring_to_front(&self, _window: &Window) -> bool {
        false
    }

    /// Install the per-window native toolbar (the full-width tab strip + "+"
    /// button) for logical window `wid`, returning the retained backing handle.
    /// `None` off macOS (no toolbar installed).
    fn install_toolbar(
        &self,
        window: &Window,
        proxy: &EventLoopProxy<Wake>,
        wid: WindowId,
    ) -> Option<toolbar::ToolbarHandle>;

    /// Re-sync a window's native toolbar tab strip to `tabs`. Each chip carries its stable
    /// `TabId`, so a right-click captures the clicked tab at menu-pop time (a positional index
    /// would re-bind), and its chrome extension (`ext`: the hover tooltip and the context-menu
    /// model; see `crate::session_chrome`). With no native strip the non-macOS model still
    /// records `ext`, so the `chrome` mirror reads one truth on every platform.
    fn set_toolbar_tabs(&self, handle: &toolbar::ToolbarHandle, tabs: ToolbarTabsModel<'_>);

    /// Push a window's needs pill alone (its age moved and nothing else),
    /// laid out where the last strip push put it, no tab chip touched.
    /// `true` when that held; `false` when the strip must be pushed whole
    /// (the pill's width moved). Default: `true` — no titlebar pill here (the
    /// in-grid strip draws it on its own repaint).
    fn set_toolbar_pill(
        &self,
        handle: &toolbar::ToolbarHandle,
        pill: Option<&crate::app_presence::NeedsPillModel>,
    ) -> bool {
        let _ = (handle, pill);
        true
    }

    /// Re-flow a window's native tab strip to its current width from the state its chips hold
    /// (no titles, no terminal reads), on every window resize. Default: nothing (no native
    /// strip, or one the toolkit lays out itself).
    fn reflow_toolbar_tabs(&self, _handle: &toolbar::ToolbarHandle) {}

    /// Whether this platform could present the native rename editor, without presenting it
    /// (`begin_tab_rename` installs the field as a side effect). Default `false`: the tab strip
    /// is the only surface, and a window with `tab_strip_rows = 0` has none.
    fn can_present_tab_rename(&self, handle: &toolbar::ToolbarHandle) -> bool {
        let _ = handle;
        false
    }

    /// Open the inline session-rename editor over the strip's `tab` chip, seeded with `seed`
    /// (the current pin, empty when unpinned) and placeheld with `placeholder` (the label the
    /// ladder falls back to). Returns whether an editor is on screen; `App` holds no edit state
    /// that nothing presents.
    ///
    /// The editor is owned by the strip handle, not a chip: chips are rebuilt whenever the tab
    /// count or the width changes, which background events cause mid-edit. `tab` is the stable
    /// id used to re-find the chip, or to cancel when it is gone. Default: `false`.
    fn begin_tab_rename(
        &self,
        handle: &toolbar::ToolbarHandle,
        tab: crate::tab_model::TabId,
        session: u64,
        seed: &str,
        placeholder: &str,
    ) -> bool {
        let _ = (handle, tab, session, seed, placeholder);
        false
    }

    /// Remove the inline rename editor and hand first responder back to the
    /// terminal view. Idempotent (no editor ⇒ no-op) and side-effect free with
    /// respect to metadata: `App` writes the pin itself, after this returns.
    fn end_tab_rename(&self, handle: &toolbar::ToolbarHandle) {
        let _ = handle;
    }

    /// The live rename editor's current text, or `None` when none is open. The native field
    /// owns the in-progress text, so a command that runs outside the editor (⌘W closing the tab)
    /// reads it here and commits first. Default: `None`.
    fn rename_editor_text(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        let _ = handle;
        None
    }

    /// Forward one editing command to the live rename field's field editor instead of to the
    /// terminal: macOS resolves a menu key equivalent before the first responder sees the key,
    /// so without this ⌘V would paste into the PTY behind an open editor. Returns whether the
    /// field editor took it. Default: `false`.
    fn rename_editor_edit(
        &self,
        handle: &toolbar::ToolbarHandle,
        action: RenameEditorEdit,
    ) -> bool {
        let _ = (handle, action);
        false
    }

    /// Read native title chrome as one introspection line (titles, selection, independent
    /// status, full tooltips), or `None` only for an empty tab set. macOS reads the retained
    /// native tab views; elsewhere it is the in-memory model `set_toolbar_tabs` maintains
    /// (`toolbar.rs`), in the same line shape.
    fn read_toolbar_chrome(&self, handle: &toolbar::ToolbarHandle) -> Option<String>;

    /// The strip's per-tab context-menu models as `chrome` introspection lines
    /// (`tab-menu tab=<i> items=[...]`, one per chip, none at ≤1 tab), read off the same stored
    /// models a right-click pops and serialised by `session_chrome::tab_menu_chrome_line`. Both
    /// `toolbar` modules expose `read_tab_menus`, so this default serves every impl.
    fn read_toolbar_tab_menus(&self, handle: &toolbar::ToolbarHandle) -> Vec<String> {
        toolbar::read_tab_menus(handle)
    }

    /// Show a native modal confirmation for a destructive quit/close gesture and
    /// block until the user answers. Returns `Some(true)` to PROCEED, `Some(false)`
    /// to CANCEL, or `None` when the platform has no native dialog — the caller then
    /// asks through the in-window confirm banner instead. `proceed_label` titles
    /// the affirmative button; a "Cancel" button is always added.
    fn confirm(&self, title: &str, body: &str, proceed_label: &str) -> Option<bool>;

    /// [`Self::confirm`] for an owner gesture that ends something, FAIL
    /// CLOSED (Stop Worker's sheet): its default button is Cancel and the
    /// destructive button takes only a click, so a Return the person was
    /// typing elsewhere never says yes; `None` when no alert could be shown
    /// (no native dialog, headless, off the main thread) — nobody was asked,
    /// which the caller never reads as a yes. Off macOS there is no sheet
    /// (Stop Worker then asks on the Overview's inline confirm).
    fn confirm_owner(&self, _title: &str, _body: &str, _proceed_label: &str) -> Option<bool> {
        None
    }

    /// Register the whole-app quit confirmation for AppKit `terminate:` gestures that bypass
    /// the application menu's Quit item: the Dock-icon "Quit", AppleScript `quit`, and
    /// logout/restart/shutdown. macOS wires winit's synchronous `applicationShouldTerminate:` to
    /// a typed event-loop deferral, where `App` runs the same dialog and document-durability
    /// barrier as ⌘Q; a no-op off macOS. Called once, when the menu is installed.
    fn install_quit_confirm(&self);

    /// Query the OS motion preference. macOS reads
    /// `NSWorkspace.accessibilityDisplayShouldReduceMotion`; Windows reads
    /// `SPI_GETCLIENTAREAANIMATION`; Linux reads GNOME's `enable-animations`
    /// from the Settings portal in a bounded read, answering `false` wherever
    /// that key cannot be read. The explicit `motion = "reduced"` override works
    /// everywhere. Feeds [`crate::motion::MotionPolicy`] (W11).
    fn reduce_motion(&self) -> bool;

    /// Snapshot the OS-native display accessibility preferences consumed by the
    /// semantic native-app appearance system. Platforms without a query return the
    /// legible opaque 1× default; app/config overrides can be folded above this seam.
    fn native_appearance_preferences(&self) -> crate::native_appearance::AppearancePreferences {
        crate::native_appearance::AppearancePreferences::default()
    }

    /// Subscribe to the OS reduce-motion change notification (macOS:
    /// `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification` on the workspace centre),
    /// posting [`Wake::ReduceMotionChanged`] so the main thread re-queries
    /// [`Self::reduce_motion`] and repaints. Returns the observer target the caller keeps for
    /// the process life (AppKit references it weakly); `None` elsewhere.
    fn observe_reduce_motion(&self, proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver>;

    /// Hold App Nap off for the rest of the process's life; the activity ends when the returned
    /// token is released. Called once, on the windowed lane only (`App::resumed`, after its
    /// headless return), so a headless or test `App` never reaches the platform service.
    /// Default `None`: App Nap is macOS's.
    fn hold_off_app_nap(&self) -> Option<AppNapActivity> {
        None
    }
}

/// The colour space a GPU window's CAMetalLayer is tagged with at surface attach. `Srgb` /
/// `DisplayP3` come from config `window_colorspace` ([`crate::app_config::WindowColorspace`]);
/// [`Self::cg_name`] maps each to its CoreGraphics name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceColorspace {
    /// The honest tag for aterm's sRGB-encoded bytes (config default).
    Srgb,
    /// The legacy stretched interpretation (bytes read as P3 coordinates).
    DisplayP3,
    /// The EDR tag for an `Rgba16Float` swapchain: linear-light sRGB primaries with >1.0
    /// headroom. Not a config value: [`resolve_surface_colorspace`] picks it for any HDR
    /// surface.
    ExtendedLinearSrgb,
}

impl SurfaceColorspace {
    /// The CoreGraphics named-colour-space identifier for this tag, as accepted by
    /// `CGColorSpaceCreateWithName`. The `kCGColorSpace*` constants' values equal their names
    /// (stable ABI), so the macOS impl builds the CFString from a toll-free-bridged NSString
    /// instead of linking the CG data symbols.
    // Consumed only by the macOS layer-tagging impl; unit-tested on every platform.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn cg_name(self) -> &'static str {
        match self {
            Self::Srgb => "kCGColorSpaceSRGB",
            Self::DisplayP3 => "kCGColorSpaceDisplayP3",
            Self::ExtendedLinearSrgb => "kCGColorSpaceExtendedLinearSRGB",
        }
    }

    /// The colour coordinates actually consumed by the platform compositor,
    /// frozen into GPU capture metadata after the platform confirms the tag.
    pub(crate) fn gpu_capture_space(self) -> aterm_gpu::video_tap::CaptureColorSpace {
        match self {
            Self::ExtendedLinearSrgb => aterm_gpu::video_tap::CaptureColorSpace::ExtendedLinearSrgb,
            Self::DisplayP3 => aterm_gpu::video_tap::CaptureColorSpace::DisplayP3,
            Self::Srgb => aterm_gpu::video_tap::CaptureColorSpace::Srgb,
        }
    }
}

/// Resolve capture metadata from a platform tagging attempt. At initial attach
/// callers pass `Unknown`; on a failed hot retag they pass the previously known
/// effective space because the layer keeps its old tag.
#[must_use]
pub(crate) fn capture_space_after_surface_tag(
    effective: Option<SurfaceColorspace>,
    previous: aterm_gpu::video_tap::CaptureColorSpace,
) -> aterm_gpu::video_tap::CaptureColorSpace {
    effective.map_or(previous, SurfaceColorspace::gpu_capture_space)
}

/// The layer tag for a just-attached (or re-tagged) GPU window: an HDR (`Rgba16Float`)
/// swapchain must be read as extended-linear-sRGB (its pixels are linear light with EDR
/// headroom), while an SDR swapchain takes the user's `window_colorspace` verbatim. The attach
/// seam and the hot-reload re-tag both route through it.
#[must_use]
pub(crate) fn resolve_surface_colorspace(
    cfg: crate::app_config::WindowColorspace,
    hdr_surface: bool,
) -> SurfaceColorspace {
    if hdr_surface {
        SurfaceColorspace::ExtendedLinearSrgb
    } else {
        cfg.into()
    }
}

impl From<crate::app_config::WindowColorspace> for SurfaceColorspace {
    /// Config value → layer tag, 1:1.
    fn from(cs: crate::app_config::WindowColorspace) -> Self {
        match cs {
            crate::app_config::WindowColorspace::Srgb => Self::Srgb,
            crate::app_config::WindowColorspace::DisplayP3 => Self::DisplayP3,
        }
    }
}

/// This window's live AppKit content `NSView`, or nil off the main thread.
///
/// The winit → `NSView` read every macOS chrome method below starts from,
/// written once: the window's first-party [`aterm_objc::HasNsView`], winit's
/// own `-contentView` read, which cannot raise. The view is borrowed (+0) and
/// lives as long as `window`.
#[cfg(target_os = "macos")]
pub(crate) fn ns_view_of(window: &Window) -> aterm_objc::Id {
    aterm_objc::HasNsView::ns_view(window)
}

/// This window's AppKit `NSWindow`, or nil.
///
/// `-window` is nil for a view that is not in a window yet, which every caller
/// here treats as "no chrome to configure" — the best-effort contract.
#[cfg(target_os = "macos")]
pub(crate) fn ns_window_of(window: &Window) -> aterm_objc::Id {
    let view = ns_view_of(window);
    if view.is_null() {
        return aterm_objc::Id::NIL;
    }
    // SAFETY: `view` is this window's live NSView (winit owns it for the
    // window's lifetime), borrowed on the main thread as AppKit requires;
    // `-window` is `-(id)`, a side-effect-free accessor returning a BORROWED
    // reference that the view keeps alive.
    unsafe { crate::appkit::send_id(view, aterm_objc::sel!(window)) }
}

/// macOS application-runtime: WRAPS the existing AppKit integration exactly. The
/// chrome methods carry the verbatim NSWindow colour-space + NSAppearance logic
/// relocated from `app_window.rs`; the rest forward to the `cfg(macos)`-guarded
/// menu/toolbar/notify modules.
#[cfg(target_os = "macos")]
pub(crate) struct AppRtMacOS {
    pub(crate) probe_preferences: bool,
}

#[cfg(target_os = "macos")]
impl AppRt for AppRtMacOS {
    fn can_present_tab_rename(&self, handle: &toolbar::ToolbarHandle) -> bool {
        toolbar::can_present_tab_rename(handle)
    }

    /// Fill the NSWindow background with the theme background (`bg`, `0x00RRGGBB`) so the
    /// transparent titlebar and the compact bar read as part of the terminal body; the content
    /// view paints over the rest. No AppKit window is a no-op.
    fn window_set_background_color(&self, window: &Window, bg: u32) {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return;
        }
        let r = f64::from((bg >> 16) & 0xff) / 255.0;
        let g = f64::from((bg >> 8) & 0xff) / 255.0;
        let b = f64::from(bg & 0xff) / 255.0;
        // SAFETY: `+colorWithSRGBRed:green:blue:alpha:` is
        // `-(id)(CGFloat, CGFloat, CGFloat, CGFloat)` and returns an
        // AUTORELEASED colour — borrowed for the length of `-setBackgroundColor:`,
        // which is `-(void)(NSColor *)` and RETAINS it. The pool is the caller's
        // (the winit event loop's), which is why the borrow is sound.
        unsafe {
            let make: unsafe extern "C-unwind" fn(
                aterm_objc::Id,
                aterm_objc::Sel,
                f64,
                f64,
                f64,
                f64,
            ) -> aterm_objc::Id = aterm_objc::checked_msg();
            let color = make(
                aterm_objc::class(c"NSColor").as_id(),
                aterm_objc::sel!(colorWithSRGBRed:green:blue:alpha:),
                r,
                g,
                b,
                1.0,
            );
            if color.is_null() {
                return;
            }
            crate::appkit::send_v_id(ns_window, aterm_objc::sel!(setBackgroundColor:), color);
        }
    }

    /// Match the NSWindow colour space to softbuffer's device-RGB content (its CGImage uses
    /// `CGColorSpace::new_device_rgb()`), so CoreAnimation skips a per-commit device-RGB →
    /// display-P3 conversion on wide-gamut displays (profiled at about half of present cost under
    /// heavy output), then set the titlebar appearance from `theme`. The pixels are unchanged.
    /// `$ATERM_NO_COLORSPACE_MATCH` opts out in a development build ([`aterm_types::dev_seam!`]).
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        use aterm_objc::sel;

        use crate::appkit;
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return;
        }
        // Colour-space match (device-RGB) — see fn doc.
        // SAFETY: `+deviceRGBColorSpace` is `-(id)`, a shared immortal singleton;
        // `-setColorSpace:` is `-(void)(NSColorSpace *)` and retains it.
        if aterm_types::dev_seam!("ATERM_NO_COLORSPACE_MATCH").is_none() {
            unsafe {
                let cs = appkit::send_id(
                    aterm_objc::class(c"NSColorSpace").as_id(),
                    sel!(deviceRGBColorSpace),
                );
                if !cs.is_null() {
                    appkit::send_v_id(ns_window, sel!(setColorSpace:), cs);
                }
            }
        }
        // A transparent titlebar, so the frame reads as an extension of the terminal body; its
        // light/dark appearance follows `window_theme`: Dark → DarkAqua, Light → Aqua, Auto →
        // unset, tracking the OS `effectiveAppearance` (live day-night switches included).
        // SAFETY: `appearanceNamed:`/`setAppearance:`/`setTitlebarAppearsTransparent:`
        // are standard NSWindow/NSAppearance calls on the main thread; the appearance
        // object is autoreleased and used immediately within this pool.
        let appearance_name: Option<&str> = match theme {
            WindowTheme::Auto => None,
            WindowTheme::Light => Some("NSAppearanceNameAqua"),
            WindowTheme::Dark => Some("NSAppearanceNameDarkAqua"),
        };
        // SAFETY: `+appearanceNamed:` is `-(id)(NSString *)` and returns an autoreleased,
        // framework-owned appearance (nil for a name this OS does not know, checked);
        // `-setAppearance:` is `-(void)(id)` and accepts nil (the Auto path);
        // `-setTitlebarAppearsTransparent:` is `-(void)(BOOL)`.
        unsafe {
            if let Some(name) = appearance_name {
                if let Some(name) = appkit::nsstring(name) {
                    let appearance = appkit::send_id_id(
                        aterm_objc::class(c"NSAppearance").as_id(),
                        sel!(appearanceNamed:),
                        name.id(),
                    );
                    if !appearance.is_null() {
                        appkit::send_v_id(ns_window, sel!(setAppearance:), appearance);
                    }
                }
            } else {
                // Auto: clear any forced appearance so the window tracks the OS
                // `effectiveAppearance` again after a live Dark/Light → Auto change.
                appkit::send_v_id(ns_window, sel!(setAppearance:), aterm_objc::Id::NIL);
            }
            appkit::send_v_bool(ns_window, sel!(setTitlebarAppearsTransparent:), true);
        }
        // The fullSizeContentView mask does not live here: config reload re-runs this on every
        // window, Settings included, which must keep normal chrome. It is applied once per
        // chrome'd terminal window at attach ([`AppRt::window_set_fullsize_content`]).
    }

    /// Tag the CAMetalLayer wgpu attached to this window's NSView with the named colour space
    /// (see [`layer_colorspace`]); no window or metal layer answers `None`.
    fn window_set_surface_colorspace(
        &self,
        window: &Window,
        cs: SurfaceColorspace,
    ) -> Option<SurfaceColorspace> {
        layer_colorspace::set(window, cs.cg_name()).then_some(cs)
    }

    /// Pin the CAMetalLayer's `contentsGravity` to the visual top-left (see
    /// [`AppRt::window_anchor_surface_top_left`]); no metal layer is a silent no-op.
    fn window_anchor_surface_top_left(&self, window: &Window) {
        layer_colorspace::anchor_contents_top_left(window);
    }

    /// Read back the CAMetalLayer's live gravity / scale / flip. See
    /// [`AppRt::window_surface_presentation`].
    fn window_surface_presentation(&self, window: &Window) -> Option<(String, f64, bool)> {
        layer_colorspace::read_contents_presentation(window)
    }

    /// See [`AppRt::window_set_vibrancy`]; the AppKit work lives in the `vibrancy` module.
    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: crate::app_config::BackgroundMaterial,
        translucent: bool,
        bg: u32,
    ) {
        vibrancy::set(window, material, translucent, bg);
    }

    /// `NSScreen.maximumExtendedDynamicRangeColorComponentValue` for this window's screen;
    /// `1.0` (no headroom, treated as SDR) with no AppKit window or an off-screen one.
    fn screen_edr_max(&self, window: &Window) -> f32 {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return 1.0;
        }
        // SAFETY: `-screen` is `-(id)` (nil for an off-screen window, checked)
        // and the EDR getter is `-(CGFloat)`, both side-effect-free reads.
        unsafe {
            let screen = crate::appkit::send_id(ns_window, aterm_objc::sel!(screen));
            if screen.is_null() {
                return 1.0;
            }
            crate::appkit::send_f64(
                screen,
                aterm_objc::sel!(maximumExtendedDynamicRangeColorComponentValue),
            ) as f32
        }
    }

    /// `NSScreen.maximumPotentialExtendedDynamicRangeColorComponentValue` for
    /// this window's screen; `1.0` where any link is missing, as
    /// [`Self::screen_edr_max`].
    fn screen_edr_potential(&self, window: &Window) -> f32 {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return 1.0;
        }
        // SAFETY: as `screen_edr_max`: `-screen` is a nil-checked `-(id)`
        // read and the potential getter (macOS 10.15+, the app requires 11)
        // a side-effect-free `-(CGFloat)`.
        unsafe {
            let screen = crate::appkit::send_id(ns_window, aterm_objc::sel!(screen));
            if screen.is_null() {
                return 1.0;
            }
            crate::appkit::send_f64(
                screen,
                aterm_objc::sel!(maximumPotentialExtendedDynamicRangeColorComponentValue),
            ) as f32
        }
    }

    /// Apply `NSWindowStyleMaskFullSizeContentView` (1<<15) so the content view, and the GPU
    /// surface with it, spans under the titlebar. No AppKit window is a no-op;
    /// `$ATERM_NO_FULLSIZE_CONTENT` turns it off in a development build
    /// ([`aterm_types::dev_seam!`]).
    fn window_set_fullsize_content(&self, window: &Window) {
        if aterm_types::dev_seam!("ATERM_NO_FULLSIZE_CONTENT").is_some() {
            return;
        }
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return;
        }
        // SAFETY: the style-mask read-modify-write is a standard NSWindow
        // property pair, `-(NSWindowStyleMask)` and `-(void)(NSWindowStyleMask)`,
        // where `NSWindowStyleMask` is `NSUInteger`.
        unsafe {
            let mask = crate::appkit::send_usize(ns_window, aterm_objc::sel!(styleMask));
            crate::appkit::send_v_usize(
                ns_window,
                aterm_objc::sel!(setStyleMask:),
                mask | (1usize << 15),
            );
        }
    }

    /// The titlebar band height in points: `contentView.bounds.height −
    /// contentLayoutRect.height`, so `0` before the full-size-content mask and in fullscreen,
    /// where the titlebar detaches. Any missing link returns `0.0`.
    fn titlebar_band_pts(&self, window: &Window) -> f64 {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return 0.0;
        }
        // SAFETY: every message below is a side-effect-free geometry read. `-bounds` and
        // `-contentLayoutRect` return `NSRect`, a 32-byte struct, which `msg` sends through
        // `objc_msgSend_stret` on the x86_64 slice.
        unsafe {
            // Callers measure after the mask and toolbar are in. The pre-mask
            // `frame − contentRectForFrameRect` form under-counts the unified toolbar and reports
            // a phantom band when the mask is never applied.
            let content_view = crate::appkit::send_id(ns_window, aterm_objc::sel!(contentView));
            if content_view.is_null() {
                return 0.0;
            }
            let bounds = crate::appkit::send_rect(content_view, aterm_objc::sel!(bounds));
            let layout = crate::appkit::send_rect(ns_window, aterm_objc::sel!(contentLayoutRect));
            (bounds.size.height - layout.size.height).max(0.0)
        }
    }

    /// `NSScreen.visibleFrame` of this window's screen, flipped into winit's top-left-origin
    /// point space (see [`AppRt::window_work_area_pts`]) with winit's own law
    /// (`flip_window_screen_coordinates`: `y_top = main_height - rect.height - rect.origin.y`).
    /// The main display's height comes from `CGMainDisplayID`, as winit's does, not
    /// `NSScreen.mainScreen` (the focused screen); any other law offsets the cascade on a
    /// multi-display desktop.
    fn window_work_area_pts(&self, window: &Window) -> Option<crate::app_window::WorkAreaPts> {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGMainDisplayID() -> u32;
            fn CGDisplayBounds(display: u32) -> aterm_objc::CGRect;
        }

        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return None;
        }
        // SAFETY: every message below is a side-effect-free geometry read, and
        // the CoreGraphics pair is a flat display-bounds getter that neither
        // allocates nor dispatches.
        unsafe {
            // Nil for a window that is not on any screen (fully off-screen, or a
            // hidden window the WindowServer has not placed yet) — fail closed to
            // the caller's monitor-bounds fallback rather than guess a screen.
            let screen = crate::appkit::send_id(ns_window, aterm_objc::sel!(screen));
            if screen.is_null() {
                return None;
            }
            let visible = crate::appkit::send_rect(screen, aterm_objc::sel!(visibleFrame));
            let main_height = CGDisplayBounds(CGMainDisplayID()).size.height;
            Some(crate::app_window::WorkAreaPts {
                x: visible.origin.x,
                y: main_height - visible.size.height - visible.origin.y,
                w: visible.size.width,
                h: visible.size.height,
            })
        }
    }

    fn send_notification_init(
        &self,
        suppress: Arc<Mutex<HashSet<u64>>>,
        silent: Arc<AtomicBool>,
        own_alerts: Arc<AtomicBool>,
        center: crate::notice_center::NoticeCenter,
    ) -> SyncSender<NotifyMsg> {
        crate::notify::spawn_delivery(suppress, silent, own_alerts, center)
    }

    fn install_menu(&self, proxy: &EventLoopProxy<Wake>) -> Option<menu::MenuHandle> {
        menu::install(proxy)
    }

    fn install_status_item(
        &self,
        proxy: &EventLoopProxy<Wake>,
        glance: &crate::status_item::Glance,
    ) -> Option<crate::status_item::StatusItemHandle> {
        crate::status_item::install(proxy, glance)
    }

    fn open_url(&self, url: &str) -> bool {
        #[cfg(test)]
        {
            OPENED_URLS.with(|urls| urls.borrow_mut().push(url.to_string()));
            true
        }
        #[cfg(not(test))]
        {
            // `/usr/bin/open <url>` (no shell): the scheme's registered
            // handler, never waited for; its child is reaped on drop.
            aterm_uds::spawnfd::command("/usr/bin/open")
                .arg(url)
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .is_ok()
        }
    }

    fn activate_instance(&self, pid: u32) -> bool {
        // Called only on the event-loop turn, which keeps the AppKit call ordered against our
        // own window state. Both flags, always: `IgnoringOtherApps` is deprecated and inert on
        // macOS 14+, but on macOS 13 and earlier `-activateWithOptions:` without it is refused
        // whenever another app is frontmost, the only time this row is used.
        /// `NSRunningApplication.h:23` — `NSApplicationActivateAllWindows = 1 << 0`.
        const ACTIVATE_ALL_WINDOWS: usize = 1 << 0;
        /// `NSRunningApplication.h:39` in MacOSX13.3.sdk —
        /// `NSApplicationActivateIgnoringOtherApps = 1 << 1`.
        const ACTIVATE_IGNORING_OTHER_APPS: usize = 1 << 1;
        if pid == 0 || pid == std::process::id() {
            return false;
        }
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: `+runningApplicationWithProcessIdentifier:` is `-(id)(pid_t)` (`pid_t` is
        // `i32`, not `NSInteger`, hence the written-out prototype) and returns nil for a dead
        // pid; `-activateWithOptions:` is `-(BOOL)(NSApplicationActivationOptions)`, an
        // `NSUInteger` bitmask.
        unsafe {
            let find: unsafe extern "C-unwind" fn(
                aterm_objc::Id,
                aterm_objc::Sel,
                libc::pid_t,
            ) -> aterm_objc::Id = aterm_objc::checked_msg();
            let app = find(
                aterm_objc::class(c"NSRunningApplication").as_id(),
                aterm_objc::sel!(runningApplicationWithProcessIdentifier:),
                pid,
            );
            if app.is_null() {
                return false;
            }
            let activate: unsafe extern "C-unwind" fn(
                aterm_objc::Id,
                aterm_objc::Sel,
                usize,
            ) -> aterm_objc::Bool = aterm_objc::checked_msg();
            activate(
                app,
                aterm_objc::sel!(activateWithOptions:),
                ACTIVATE_ALL_WINDOWS | ACTIVATE_IGNORING_OTHER_APPS,
            )
            .as_bool()
        }
    }

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
            pill,
        } = tabs;
        toolbar::set_window_tabs(handle, titles, ids, metadata, tooltips, ext, active, pill);
    }

    fn set_toolbar_pill(
        &self,
        handle: &toolbar::ToolbarHandle,
        pill: Option<&crate::app_presence::NeedsPillModel>,
    ) -> bool {
        toolbar::set_window_pill(handle, pill)
    }

    fn reflow_toolbar_tabs(&self, handle: &toolbar::ToolbarHandle) {
        toolbar::reflow_window_tabs(handle);
    }

    fn begin_tab_rename(
        &self,
        handle: &toolbar::ToolbarHandle,
        tab: crate::tab_model::TabId,
        session: u64,
        seed: &str,
        placeholder: &str,
    ) -> bool {
        toolbar::begin_tab_rename(handle, tab, session, seed, placeholder)
    }

    fn end_tab_rename(&self, handle: &toolbar::ToolbarHandle) {
        toolbar::end_tab_rename(handle);
    }

    fn rename_editor_text(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        toolbar::rename_editor_text(handle)
    }

    fn rename_editor_edit(
        &self,
        handle: &toolbar::ToolbarHandle,
        action: RenameEditorEdit,
    ) -> bool {
        toolbar::rename_editor_edit(handle, action)
    }

    fn read_toolbar_chrome(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        toolbar::read_tab_chrome(handle)
    }

    fn confirm(&self, title: &str, body: &str, proceed_label: &str) -> Option<bool> {
        Some(menu::confirm(title, body, proceed_label))
    }

    fn confirm_owner(&self, title: &str, body: &str, proceed_label: &str) -> Option<bool> {
        menu::confirm_owner(title, body, proceed_label)
    }

    fn install_quit_confirm(&self) {
        // winit owns the `NSApplication` delegate, so the `applicationShouldTerminate:`
        // veto is registered through a small vendored-winit seam. The callback has no
        // access to `App`, so it defers onto the typed event loop; the main-thread App
        // then runs the same confirmation + document durability barrier as ⌘Q.
        aterm_window::platform::macos::set_application_should_terminate_handler(
            menu::defer_quit_for_terminate,
        );
    }

    fn reduce_motion(&self) -> bool {
        self.probe_preferences && reduce_motion::query()
    }

    fn native_appearance_preferences(&self) -> crate::native_appearance::AppearancePreferences {
        if !self.probe_preferences {
            return crate::native_appearance::AppearancePreferences::default();
        }
        crate::native_appearance::AppearancePreferences {
            high_contrast: reduce_motion::increase_contrast(),
            reduced_transparency: reduce_motion::reduce_transparency(),
            differentiate_without_color: reduce_motion::differentiate_without_color(),
            text_scale: 1.0,
        }
    }

    fn observe_reduce_motion(&self, proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver> {
        if self.probe_preferences {
            reduce_motion::observe(proxy)
        } else {
            None
        }
    }

    fn hold_off_app_nap(&self) -> Option<AppNapActivity> {
        begin_app_nap_opt_out()
    }
}

/// Map aterm's config [`WindowTheme`] onto the winit window-theme override
/// [`aterm_window::window::Theme`] applied via [`Window::set_theme`]:
/// * `Auto`  → `None` — clear the override. On X11 winit defaults the
///   `_GTK_THEME_VARIANT` hint to dark; on Wayland the fork only stores the value
///   (it draws no frame since LW.5: a window the compositor leaves undecorated
///   has aterm's own header, `crate::client_frame`).
///   Neither backend reports the desktop preference back (`Window::theme()` is
///   the override or `None`, and no `ThemeChanged` arrives), which is why
///   `App::chrome_theme_for_apprt` resolves Linux `Auto` from the terminal theme
///   instead.
/// * `Light` → `Some(Theme::Light)` — force the light variant of the window
///   system's frame (X11's `_GTK_THEME_VARIANT`, a Windows title bar).
/// * `Dark`  → `Some(Theme::Dark)` — force the dark variant.
///
/// PURE: a total `match` with no I/O, so it is unit-tested directly (see the `tests`
/// module). The non-macOS [`AppRt::window_set_appearance`] is just this mapping fed
/// to `set_theme`.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub(crate) fn window_theme_to_winit(theme: WindowTheme) -> Option<aterm_window::window::Theme> {
    match theme {
        WindowTheme::Auto => None,
        WindowTheme::Light => Some(aterm_window::window::Theme::Light),
        WindowTheme::Dark => Some(aterm_window::window::Theme::Dark),
    }
}

/// Proof that this process launched WINDOWED: its event loop is AppKit's, whose
/// `+[NSApplication sharedApplication]` exists once the loop is built, so
/// sending it from the loop creates nothing.
///
/// A headless process must never send it: the send CREATES the application
/// object, and its `-init` checks the process in with LaunchServices and opens
/// a WindowServer connection (`app_menu_bar`'s module doc). So every function
/// that sends it takes this proof first — the menu bar reader and the pre-dial
/// check-in seam as an argument, the rest from [`Windowed::get`] — and answers
/// its headless value without one.
///
/// Minted only by [`Windowed::launch`], at the windowed launches: `main_entry`
/// before its handoff intake (the one send before the loop is built, the
/// development pre-dial check-in, creates the application object on purpose
/// and dies), and the toolbar and devtools-helper drives, windowed processes of
/// their own. A unit-test build mints none.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Windowed(());

/// Whether [`Windowed::launch`] ran in this process.
#[cfg(target_os = "macos")]
static WINDOWED_LAUNCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "macos")]
impl Windowed {
    /// Mint the proof at a windowed launch, before its AppKit event loop is
    /// built. Called by `main_entry`'s windowed branch and the toolbar drive,
    /// nowhere else.
    pub(crate) fn launch() -> Self {
        WINDOWED_LAUNCH.store(true, std::sync::atomic::Ordering::Release);
        Self(())
    }

    /// The proof, if this process launched windowed; `None` in a headless
    /// process and in a unit-test build.
    pub(crate) fn get() -> Option<Self> {
        WINDOWED_LAUNCH
            .load(std::sync::atomic::Ordering::Acquire)
            .then_some(Self(()))
    }
}

/// The modifier state winit last DERIVED FROM AN EVENT for `window` — its own
/// cache, which `update_modifiers` writes before it queues `ModifiersChanged`.
/// For re-syncing after an `NSException` was contained inside a
/// `flagsChanged:`-shaped row (see `App::note_objc_containments`): the cache
/// is current while the app's copy may be one event behind. NOT a hardware
/// probe: the class-level flags query on `NSEvent` is WindowServer-backed and
/// banned tree-wide (the aterm-census guard `input-probe`) — the first cut of
/// this hook reached for it and the guard caught it.
#[cfg(target_os = "macos")]
pub(crate) fn cached_modifiers(
    window: &aterm_window::window::Window,
) -> aterm_window::keyboard::ModifiersState {
    use aterm_window::platform::macos::WindowExtMacOS as _;
    window.cached_modifiers()
}

/// The window setting for an Option that is an Alt (`alt`, [`crate::App::option_is_alt`]):
/// both keys, so AppKit is handed each Option key event without its Option characters and
/// neither composes a glyph (Option+a, "å") nor starts a dead key (Option+e), and the key
/// event's text is the layout's character at the Shift level, which the encoder sends after
/// ESC; neither key otherwise, so Option types what the layout composes.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn option_as_alt_for(alt: bool) -> aterm_window::platform::macos::OptionAsAlt {
    use aterm_window::platform::macos::OptionAsAlt;
    if alt {
        OptionAsAlt::Both
    } else {
        OptionAsAlt::None
    }
}

/// Make `window`'s Option keys an Alt (`alt`) or let them compose: [`option_as_alt_for`],
/// written by `App::sync_option_as_alt` and at the window's creation. No-op off macOS (no
/// other backend composes on Alt: X11 and Wayland report AltGr as a level, not as Alt, and
/// the Windows backend encodes the composed key itself).
pub(crate) fn set_option_as_alt(window: &aterm_window::window::Window, alt: bool) {
    #[cfg(target_os = "macos")]
    {
        use aterm_window::platform::macos::WindowExtMacOS as _;
        window.set_option_as_alt(option_as_alt_for(alt));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (window, alt);
}

/// Where an Option chord that Meta mode would send as ESC and the key types a character
/// instead: the one its layout reaches only with Option (`App::on_key`; German Option+L is
/// `@`, Option+7 `|`, Option+N `~`, French Option+( `{`, British Option+3 `#`), as
/// [`aterm_window::platform::macos::option_only_character`] reads the selected layout. A
/// function of the key and Shift; `None` on every other chord, and on every chord of the
/// U.S. layout.
pub(crate) type OptionOnly = fn(aterm_window::keyboard::PhysicalKey, bool) -> Option<char>;

/// The [`OptionOnly`] source an `App` holds: the selected layout's windowed on macOS,
/// [`no_option_only`] headless, in unit tests and off macOS (no other backend reads Alt
/// chords as Meta past a layout's level-3 characters: X11 and Wayland report AltGr as a
/// level, and Windows encodes the composed key).
pub(crate) fn option_only_source(headless: bool) -> OptionOnly {
    #[cfg(target_os = "macos")]
    if !headless {
        return aterm_window::platform::macos::option_only_character;
    }
    let _ = headless;
    no_option_only
}

/// [`OptionOnly`] for HEADLESS instances and unit tests: never. A headless `App` has no
/// keyboard layout of its own, and a unit test must encode the same bytes on every machine
/// whatever layout its desk has selected, so neither reads one (which also keeps a test
/// binary off the input-source server).
#[must_use]
pub(crate) fn no_option_only(
    _key: aterm_window::keyboard::PhysicalKey,
    _shift: bool,
) -> Option<char> {
    None
}

/// Whether an Option chord's key-event text is what Option composed rather than the
/// layout's character at the Shift level: on macOS, whenever the window's Option is not an
/// Alt (`option_is_alt`, what `App::sync_option_as_alt` last set), so the keymap must not
/// hand that text to the encoder as the layout's character (`keymap::layout_text`). Never
/// off macOS.
#[must_use]
pub(crate) fn option_composes(option_is_alt: bool) -> bool {
    cfg!(target_os = "macos") && !option_is_alt
}

/// What a window told Option is an Alt (or not) puts on it and tells the keymap, without a
/// window: who gets it is `app_input_option_as_alt_tests.rs`, and the window half
/// (`replace_event` taking Option's characters away) is
/// `crates/aterm-gui/examples/objc_ime_drive.rs`'s stage 8, a windowed drive.
#[cfg(test)]
mod option_as_alt_tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn an_alt_window_is_both_option_keys_and_a_composing_one_neither() {
        use aterm_window::platform::macos::OptionAsAlt;
        assert_eq!(super::option_as_alt_for(true), OptionAsAlt::Both);
        assert_eq!(super::option_as_alt_for(false), OptionAsAlt::None);
    }

    #[test]
    fn only_a_macos_option_that_is_no_alt_composes() {
        assert!(!super::option_composes(true));
        assert_eq!(super::option_composes(false), cfg!(target_os = "macos"));
    }
}

/// How long the NSEvent currently being dispatched sat in the NSApp queue before
/// this code ran, in nanoseconds — the touch-to-glass slice NO post-dequeue stamp
/// can see. `NSEvent.timestamp` and `NSProcessInfo.systemUptime` share the
/// seconds-since-boot clock, so their difference IS the queue age (hardware
/// arrival → dispatch). A parked event loop (the FIFO drawable park, a long
/// handler) queues keyDowns for tens of milliseconds; backdating the latency
/// stamps by this age makes that visible to `key->write`, `input->present`, and
/// the video key ledger. Fail-closed `None`: in a headless process, off the main
/// thread, outside a dispatch, or on a suspicious age (negative beyond scheduler jitter, or > 2 s —
/// clock skew / a synthesized or replayed event) the caller falls back to "now",
/// which is exactly today's behaviour.
#[cfg(target_os = "macos")]
pub(crate) fn current_event_queue_age_ns() -> Option<u64> {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicPtr, Ordering};

    use aterm_objc::{Id, class, sel};

    use crate::appkit::{self, MainThread};

    static NS_APPLICATION_CLASS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static NS_PROCESS_INFO_CLASS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    #[inline]
    fn ns_application_class() -> Id {
        let cached = NS_APPLICATION_CLASS.load(Ordering::Relaxed);
        if !cached.is_null() {
            return Id::from_ptr(cached);
        }
        let cls = class(c"NSApplication").as_id();
        NS_APPLICATION_CLASS.store(cls.cast(), Ordering::Relaxed);
        cls
    }

    #[inline]
    fn ns_process_info_class() -> Id {
        let cached = NS_PROCESS_INFO_CLASS.load(Ordering::Relaxed);
        if !cached.is_null() {
            return Id::from_ptr(cached);
        }
        let cls = class(c"NSProcessInfo").as_id();
        NS_PROCESS_INFO_CLASS.store(cls.cast(), Ordering::Relaxed);
        cls
    }

    // A headless process has no event queue to age, and no application object
    // to ask: the send below would create one.
    let _windowed = Windowed::get()?;
    let _main_thread = MainThread::new()?;
    // SAFETY: `+sharedApplication` is `-(id)`, the main-thread AppKit singleton
    // (`_main_thread` is the witness); `-currentEvent` is `-(id)` and nil
    // outside a dispatch, which is the fail-closed case; `-timestamp` and
    // `-systemUptime` are `-(NSTimeInterval)`, i.e. `double`, side-effect-free
    // scalar reads; `+processInfo` is `-(id)`, another immortal singleton.
    let (ts, uptime) = unsafe {
        let app = appkit::send_id(ns_application_class(), sel!(sharedApplication));
        if app.is_null() {
            return None;
        }
        let ev = appkit::send_id(app, sel!(currentEvent));
        if ev.is_null() {
            return None;
        }
        let info = appkit::send_id(ns_process_info_class(), sel!(processInfo));
        if info.is_null() {
            return None;
        }
        (
            appkit::send_f64(ev, sel!(timestamp)),
            appkit::send_f64(info, sel!(systemUptime)),
        )
    };
    if ts <= 0.0 {
        return None; // synthesized events carry timestamp 0
    }
    let age_s = uptime - ts;
    // Negative up to ~1 ms is clock-read jitter (treat as "no queueing"); beyond
    // the window in either direction the pair can't be trusted — fail closed.
    if !(-0.001..=2.0).contains(&age_s) {
        return None;
    }
    Some((age_s.max(0.0) * 1e9) as u64)
}

/// This window's `-[NSWindow windowNumber]`, or `0` with no AppKit window. The `hwkey`
/// injector needs it from the main thread: `sendEvent:` routes a key to the window number baked
/// into the event, and `0` would dispatch to nothing. Read-only and allocation-free, so it does
/// not perturb the loop being measured.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn appkit_window_number(window: &Window) -> i64 {
    let ns_window = ns_window_of(window);
    if ns_window.is_null() {
        return 0;
    }
    // SAFETY: `-windowNumber` is `-(NSInteger)`, a side-effect-free scalar getter.
    unsafe { crate::appkit::send_isize(ns_window, aterm_objc::sel!(windowNumber)) as i64 }
}

/// Non-macOS peer of the macOS reader: there is no AppKit window number, and the
/// `hwkey` injector refuses on these platforms anyway.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub(crate) fn appkit_window_number(_window: &Window) -> i64 {
    0
}

/// Whether this window is the key window of the active application: where a key
/// press goes, and the only window whose keys the input method composes (an
/// Option dead key starts no composition in an inactive app, measured on macOS 27,
/// `docs/measured/desk-appkit-2026-10-19.md`). `hwkey` reports it, so a caller
/// knows whether its keys were pressed where a person's would land.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn appkit_window_is_key_in_active_app(window: &Window) -> bool {
    // A headless process has no application object to ask, and must not create one.
    let Some(_windowed) = Windowed::get() else {
        return false;
    };
    let ns_window = ns_window_of(window);
    if ns_window.is_null() {
        return false;
    }
    // SAFETY: `+sharedApplication` is `@16#0:8` (the windowed launch created it
    // before any window existed); `-isActive` and `-isKeyWindow` are `B16@0:8`
    // plain getters on live receivers. Main thread, as the caller's wake runs.
    unsafe {
        let app = crate::appkit::send_id(
            aterm_objc::class(c"NSApplication").as_id(),
            aterm_objc::sel!(sharedApplication),
        );
        !app.is_null()
            && crate::appkit::send_bool(app, aterm_objc::sel!(isActive))
            && crate::appkit::send_bool(ns_window, aterm_objc::sel!(isKeyWindow))
    }
}

/// Non-macOS peer: no AppKit, no key window to report.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub(crate) fn appkit_window_is_key_in_active_app(_window: &Window) -> bool {
    false
}

/// Whether a hardware-input event occurred inside `within`.
///
/// Reads the kernel's HID idle clock (`HIDIdleTime` on the `IOHIDSystem` registry entry,
/// nanoseconds since the last event from any HID device) through IOKit ([`hid_idle`]).
/// Machine-wide, so activity in another app postpones an automatic update, as "install when
/// quiet" wants.
///
/// LAW (the 2026-08-17 WindowServer watchdog incident): this probe MUST NOT go
/// through WindowServer. It used to read CoreGraphics' event-source clocks —
/// a "flat, non-dispatching C getter", and it was, but its FIRST call in a
/// process lazily opens a SkyLight connection and asks WindowServer for the
/// event shmem; WindowServer's MAIN THREAD then runs a synchronous TCC
/// Input-Monitoring preflight for the caller's code identity, and tccd's
/// identify step `readdir`s the executable's parent directory. From a unit-test
/// binary in a 1.1-million-entry `target/debug/deps` that scan outran the 40 s
/// watchdog. The kernel registry read has no such path, and no test or headless
/// instance's `App` reaches even that — they get [`no_recent_user_input_event`]
/// through the injected `App::user_input_recent` field. (It also never touches
/// AppKit's event queue: the crash-45791 concern this probe originally replaced
/// `nextEventMatchingMask` for still holds.)
///
/// Fails closed: an unreadable HID system reports input as recent.
#[cfg(target_os = "macos")]
fn recent_user_input_event(within: std::time::Duration) -> bool {
    match hid_idle::seconds_since_last_input() {
        Some(age_seconds) => user_input_age_is_recent(age_seconds, within.as_secs_f64()),
        None => true,
    }
}

// Fails open, unlike the macOS arm: harmless while the automatic apply lane is macOS-only
// (Linux carries read-only update facts, `native_updater_service`). A non-macOS automatic apply
// lane must make this fail closed (`true`) or read a real input clock.
#[cfg(not(target_os = "macos"))]
fn recent_user_input_event(_within: std::time::Duration) -> bool {
    false
}

/// The input-activity source an `App` holds: the platform read
/// ([`recent_user_input_event`]) windowed, [`no_recent_user_input_event`]
/// headless. The platform read is private to this module, so a
/// headless-constructible path can reach it only through this choice.
pub(crate) fn user_input_source(headless: bool) -> fn(std::time::Duration) -> bool {
    if headless {
        no_recent_user_input_event
    } else {
        recent_user_input_event
    }
}

/// The input-activity source for headless instances and unit tests: never recent. Neither has
/// an operator whose activity should postpone anything, and neither may consult the platform,
/// which keeps a test binary from reaching WindowServer through this path.
#[must_use]
pub(crate) fn no_recent_user_input_event(_within: std::time::Duration) -> bool {
    false
}

/// The kernel's HID idle clock, read from the IORegistry: the one input-activity source on
/// macOS that never involves WindowServer.
///
/// Tiny FFI (like `keymap::hid_lock_state`): IOKit and CoreFoundation are system frameworks,
/// with the SDK signatures declared inline. The `IOHIDSystem` service and the property key are
/// looked up once and cached; each read is one `IORegistryEntryCreateCFProperty` (a synchronous,
/// thread-agnostic kernel RPC that needs no entitlement) plus a `CFNumber` unwrap.
#[cfg(target_os = "macos")]
mod hid_idle {
    use std::ffi::{c_char, c_void};
    use std::sync::OnceLock;

    type MachPort = u32;
    type IoObject = MachPort;
    type IoService = IoObject;
    type IoRegistryEntry = IoObject;
    type CfTypeRef = *const c_void;
    type CfStringRef = *const c_void;
    type CfAllocatorRef = *const c_void;
    type CfTypeId = usize;
    type CfIndex = isize;
    type CfStringEncoding = u32;

    /// `IOKitLib.h`: MACH_PORT_NULL selects the default main port.
    const IO_MAIN_PORT_DEFAULT: MachPort = 0;
    /// `hidsystem/IOHIDShared.h`: `kIOHIDSystemClass "IOHIDSystem"`.
    const IOHID_SYSTEM_CLASS: &[u8] = b"IOHIDSystem\0";
    /// The registry property (`hidsystem/IOHIDParameter.h`: `kIOHIDIdleTimeKey`).
    const HID_IDLE_TIME_KEY: &[u8] = b"HIDIdleTime\0";
    /// `CFString.h`: `kCFStringEncodingUTF8`.
    const CF_STRING_ENCODING_UTF8: CfStringEncoding = 0x0800_0100;
    /// `CFNumber.h`: `kCFNumberSInt64Type`.
    const CF_NUMBER_SINT64_TYPE: CfIndex = 4;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        /// Returns a CFMutableDictionaryRef, consumed by `IOServiceGetMatchingService`.
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: MachPort, matching: *mut c_void) -> IoService;
        fn IOObjectRelease(object: IoObject) -> i32;
        /// Returns a +1 retained CF object (Create rule) or NULL.
        fn IORegistryEntryCreateCFProperty(
            entry: IoRegistryEntry,
            key: CfStringRef,
            allocator: CfAllocatorRef,
            options: u32,
        ) -> CfTypeRef;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: CfAllocatorRef,
            c_str: *const c_char,
            encoding: CfStringEncoding,
        ) -> CfStringRef;
        fn CFGetTypeID(cf: CfTypeRef) -> CfTypeId;
        fn CFNumberGetTypeID() -> CfTypeId;
        fn CFNumberGetValue(number: CfTypeRef, the_type: CfIndex, value_ptr: *mut c_void) -> u8;
        fn CFRelease(cf: CfTypeRef);
    }

    /// A raw pointer that is safe to share: the cached key is an immutable,
    /// never-released CFString; the service is a mach port name.
    struct Handles {
        service: IoService,
        key: CfStringRef,
    }
    // SAFETY: both fields are process-wide handles that IOKit/CF document as
    // usable from any thread; nothing here is dereferenced by Rust.
    unsafe impl Send for Handles {}
    unsafe impl Sync for Handles {}

    fn handles() -> Option<&'static Handles> {
        static HANDLES: OnceLock<Option<Handles>> = OnceLock::new();
        HANDLES
            .get_or_init(|| {
                // SAFETY: `IOServiceMatching` takes a NUL-terminated class name and returns a
                // dictionary `IOServiceGetMatchingService` consumes. The service and the
                // `CFStringCreateWithCString` key (a NUL-terminated UTF-8 literal) are the cache,
                // kept for the process; the service is released only if the key cannot be made.
                unsafe {
                    let matching = IOServiceMatching(IOHID_SYSTEM_CLASS.as_ptr().cast::<c_char>());
                    if matching.is_null() {
                        return None;
                    }
                    let service = IOServiceGetMatchingService(IO_MAIN_PORT_DEFAULT, matching);
                    if service == 0 {
                        return None;
                    }
                    let key = CFStringCreateWithCString(
                        std::ptr::null(),
                        HID_IDLE_TIME_KEY.as_ptr().cast::<c_char>(),
                        CF_STRING_ENCODING_UTF8,
                    );
                    if key.is_null() {
                        IOObjectRelease(service);
                        return None;
                    }
                    Some(Handles { service, key })
                }
            })
            .as_ref()
    }

    /// Seconds since the last HID event, `None` when the registry cannot be
    /// read (no `IOHIDSystem`, no property, or not a number).
    #[must_use]
    pub(super) fn seconds_since_last_input() -> Option<f64> {
        let h = handles()?;
        // SAFETY: `h.service` is a live registry entry and `h.key` a live
        // CFString (both process-cached); the Create rule gives us +1 on the
        // returned object, which we release on every path after use. The type
        // is checked before `CFNumberGetValue` writes into the `i64`.
        unsafe {
            let value = IORegistryEntryCreateCFProperty(h.service, h.key, std::ptr::null(), 0);
            if value.is_null() {
                return None;
            }
            let mut idle_ns: i64 = 0;
            let ok = CFGetTypeID(value) == CFNumberGetTypeID()
                && CFNumberGetValue(
                    value,
                    CF_NUMBER_SINT64_TYPE,
                    (&raw mut idle_ns).cast::<c_void>(),
                ) != 0;
            CFRelease(value);
            #[allow(clippy::cast_precision_loss)]
            // nanoseconds → seconds; sub-ns precision is irrelevant
            ok.then(|| idle_ns as f64 / 1e9)
        }
    }
}

/// Reduce one idle-clock age sample without platform state. Invalid negative
/// or NaN samples fail closed; positive infinity is the useful "never observed"
/// shape and therefore is not recent.
#[cfg(any(target_os = "macos", test))]
fn user_input_age_is_recent(age_seconds: f64, within_seconds: f64) -> bool {
    age_seconds.is_nan() || age_seconds < 0.0 || age_seconds <= within_seconds
}

#[cfg(test)]
mod user_input_probe_tests {
    use super::user_input_age_is_recent;

    #[test]
    fn input_age_reducer_is_boundary_exact_and_fail_closed() {
        assert!(user_input_age_is_recent(0.0, 0.5));
        assert!(user_input_age_is_recent(0.5, 0.5));
        assert!(!user_input_age_is_recent(0.500_001, 0.5));
        assert!(user_input_age_is_recent(f64::NAN, 0.5));
        assert!(user_input_age_is_recent(-0.001, 0.5));
        assert!(!user_input_age_is_recent(f64::INFINITY, 0.5));
    }

    /// The macOS probe is a kernel registry read (never WindowServer — see
    /// `hid_idle`), callable from a libtest worker thread without a window or
    /// event loop. It is allowed to find the HID system unreachable (a sandbox);
    /// when it can read, two consecutive samples come from the same clock.
    #[cfg(target_os = "macos")]
    #[test]
    fn hid_idle_input_probe_is_callable_off_the_appkit_main_thread() {
        let first = super::hid_idle::seconds_since_last_input();
        let _ = super::recent_user_input_event(std::time::Duration::from_millis(500));
        let second = super::hid_idle::seconds_since_last_input();
        assert_eq!(
            first.is_some(),
            second.is_some(),
            "the cached handles answer consistently"
        );
        if let (Some(a), Some(b)) = (first, second) {
            assert!(a >= 0.0 && b >= 0.0, "idle ages are non-negative: {a} {b}");
        }
    }
}

/// The X11 twin of the macOS queue age: how long the key event being dispatched
/// took from the X server stamping it to this code running, in nanoseconds.
///
/// A local Xorg's `time` is `CLOCK_MONOTONIC` in milliseconds, so the age is one subtraction.
/// `None` (the caller keeps its post-dequeue stamp) when no X11 key is being dispatched
/// (Wayland, a control-socket key, an IME commit) or the age is implausible (a replayed event,
/// or a remote `DISPLAY` on another machine's clock); [`x_event_max_age_ms`] sets the bound.
#[cfg(target_os = "linux")]
pub(crate) fn current_event_queue_age_ns() -> Option<u64> {
    static MAX_AGE_MS: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let server_ms = aterm_window::platform::x11::key_event_server_time()?;
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable `timespec`; CLOCK_MONOTONIC always exists.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
        return None;
    }
    let now_ns = u64::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(now.tv_nsec).ok()?)?;
    let max_age_ms =
        *MAX_AGE_MS.get_or_init(|| x_event_max_age_ms(std::env::var_os("DISPLAY").as_deref()));
    x_server_event_age_ns(server_ms, now_ns, max_age_ms)
}

/// The oldest X server stamp [`current_event_queue_age_ns`] believes, by `DISPLAY` (what
/// winit's `XOpenDisplay(NULL)` reads). A local server (`:N`, `unix:N`, a socket path) stamps
/// from this machine's `CLOCK_MONOTONIC`, so a large age is a real wait: 60 s, past the 5 s
/// stall watchdog. Any other display (`host:N`, an ssh-forwarded `localhost:10`) runs on another
/// machine's clock and keeps a 2 s window, so only a coincidental near-match can leak in.
#[cfg(any(target_os = "linux", test))]
fn x_event_max_age_ms(display: Option<&std::ffi::OsStr>) -> u32 {
    const LOCAL_MAX_AGE_MS: u32 = 60_000;
    const FOREIGN_MAX_AGE_MS: u32 = 2_000;
    let local = display.is_some_and(|d| {
        let d = d.as_encoded_bytes();
        d.starts_with(b":") || d.starts_with(b"unix:") || d.starts_with(b"/")
    });
    if local {
        LOCAL_MAX_AGE_MS
    } else {
        FOREIGN_MAX_AGE_MS
    }
}

/// The age of an event stamped `server_ms` (the X server's wrapping 32-bit millisecond clock)
/// at `now_ns` on the same clock in nanoseconds, the wrap undone against `now`'s low 32 bits.
///
/// The age reads high, never low, by up to about 2 ms on a `CONFIG_HZ=1000` kernel: the stamp
/// is a truncated millisecond, and Xorg's `GetTimeInMillis` reads `CLOCK_MONOTONIC_COARSE` (up
/// to one tick behind) when its resolution is 1 ms or finer. The kernel-to-Xorg leg is not in
/// the age: the server stamps when it processes the device event.
///
/// Up to 2 ms in the future reads as no queueing (slack for an unknown clock). Further ahead,
/// older than `max_age_ms`, or a stamp of `0` (X's `CurrentTime`, which `XSendEvent` may carry)
/// is a clock that is not ours: `None`.
#[cfg(any(target_os = "linux", test))]
fn x_server_event_age_ns(server_ms: u32, now_ns: u64, max_age_ms: u32) -> Option<u64> {
    const FUTURE_SLACK_MS: u32 = 2;
    if server_ms == 0 {
        return None;
    }
    let now_ms = now_ns / 1_000_000;
    // Wrapping difference in the server's 32-bit ms space: small positive is a
    // past stamp, near-2^32 is a stamp slightly in the future.
    let behind_ms = (now_ms as u32).wrapping_sub(server_ms);
    if behind_ms > max_age_ms {
        let ahead_ms = server_ms.wrapping_sub(now_ms as u32);
        return (ahead_ms <= FUTURE_SLACK_MS).then_some(0);
    }
    let stamp_ms = now_ms.checked_sub(u64::from(behind_ms))?;
    Some(now_ns.saturating_sub(stamp_ms * 1_000_000))
}

#[cfg(test)]
mod x_server_event_age_tests {
    use std::ffi::OsStr;

    use super::{x_event_max_age_ms, x_server_event_age_ns};

    const MS: u64 = 1_000_000;
    const LOCAL: u32 = 60_000;
    const FOREIGN: u32 = 2_000;

    #[test]
    fn age_is_now_minus_the_stamp_with_the_sub_millisecond_remainder_kept() {
        // Stamped at 1_000 ms; now is 1_004.25 ms.
        assert_eq!(
            x_server_event_age_ns(1_000, 1_004 * MS + MS / 4, LOCAL),
            Some(4 * MS + MS / 4)
        );
        // Same millisecond: the truncated stamp reads up to 1 ms high, never low.
        assert_eq!(
            x_server_event_age_ns(1_000, 1_000 * MS + 600_000, LOCAL),
            Some(600_000)
        );
    }

    #[test]
    fn the_32_bit_server_clock_wraps_about_every_49_days() {
        // now = 2^32 + 3 ms on a 64-bit clock; the server stamped 2^32 - 2 ms.
        let now_ns = ((1_u64 << 32) + 3) * MS;
        assert_eq!(
            x_server_event_age_ns(u32::MAX - 1, now_ns, LOCAL),
            Some(5 * MS)
        );
        // A multi-second park across the wrap still reads as the park.
        assert_eq!(
            x_server_event_age_ns(u32::MAX - 2_699, now_ns, LOCAL),
            Some(2_703 * MS)
        );
    }

    #[test]
    fn a_stamp_just_in_the_future_is_granularity_and_reads_as_no_queueing() {
        assert_eq!(x_server_event_age_ns(1_002, 1_000 * MS, LOCAL), Some(0));
        assert_eq!(x_server_event_age_ns(1_003, 1_000 * MS, LOCAL), None);
    }

    /// A local server prices a multi-second park (here 2.7 s) instead of refusing it; a
    /// foreign clock keeps the 2 s window.
    #[test]
    fn a_local_server_prices_a_multi_second_park() {
        let now_ns = 100_000 * MS;
        assert_eq!(
            x_server_event_age_ns(100_000 - 2_722, now_ns, LOCAL),
            Some(2_722 * MS)
        );
        assert_eq!(
            x_server_event_age_ns(100_000 - 60_000, now_ns, LOCAL),
            Some(60_000 * MS)
        );
        // Past the local cap is past the stall watchdog too: fail closed.
        assert_eq!(x_server_event_age_ns(100_000 - 60_001, now_ns, LOCAL), None);
        // The same 2.7 s stamp from a foreign clock is still refused.
        assert_eq!(
            x_server_event_age_ns(100_000 - 2_722, now_ns, FOREIGN),
            None
        );
    }

    #[test]
    fn a_foreign_server_keeps_the_two_second_window() {
        assert_eq!(x_server_event_age_ns(1_000, 3_001 * MS, FOREIGN), None);
        assert_eq!(
            x_server_event_age_ns(1_000, 3_000 * MS, FOREIGN),
            Some(2_000 * MS)
        );
        // Minutes in the future fails closed under either cap.
        assert_eq!(x_server_event_age_ns(500_000, 1_000 * MS, FOREIGN), None);
        assert_eq!(x_server_event_age_ns(500_000, 1_000 * MS, LOCAL), None);
    }

    #[test]
    fn current_time_is_not_a_stamp() {
        // `CurrentTime` (0) inside the first minute of the clock would otherwise
        // read as a real wait under the local cap.
        assert_eq!(x_server_event_age_ns(0, 30_000 * MS, LOCAL), None);
        assert_eq!(
            x_server_event_age_ns(1, 30_000 * MS, LOCAL),
            Some(29_999 * MS)
        );
    }

    #[test]
    fn the_cap_follows_the_display_the_process_connects_to() {
        for local in [":0", ":1", ":1.0", "unix:0", "/tmp/.X11-unix/X1"] {
            assert_eq!(
                x_event_max_age_ms(Some(OsStr::new(local))),
                LOCAL,
                "{local}"
            );
        }
        for foreign in ["localhost:10.0", "host.example:0", "10.0.0.2:0", ""] {
            assert_eq!(
                x_event_max_age_ms(Some(OsStr::new(foreign))),
                FOREIGN,
                "{foreign}"
            );
        }
        assert_eq!(x_event_max_age_ms(None), FOREIGN);
    }
}

/// Drives the seam: the stamp is published by the winit function the X11 event processor wraps
/// each non-synthetic key dispatch in, and read back through the real
/// [`current_event_queue_age_ns`]. Only the call site in winit's `event_processor.rs` needs an
/// X server.
#[cfg(all(test, target_os = "linux"))]
mod x11_key_stamp_seam_tests {
    use aterm_window::platform::x11::{dispatch_with_key_event_server_time, key_event_server_time};

    use super::current_event_queue_age_ns;

    fn monotonic_ms() -> u32 {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `now` is a valid, writable `timespec`.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) },
            0
        );
        (now.tv_sec as u64 * 1_000 + now.tv_nsec as u64 / 1_000_000) as u32
    }

    #[test]
    fn a_key_stamp_reaches_the_age_for_exactly_one_dispatch() {
        const MS: u64 = 1_000_000;
        assert_eq!(key_event_server_time(), None);
        assert_eq!(current_event_queue_age_ns(), None, "no key, no age");

        // A key the server stamped 40 ms ago: inside either display's cap.
        let stamp = monotonic_ms().wrapping_sub(40);
        let age = dispatch_with_key_event_server_time(stamp, current_event_queue_age_ns)
            .expect("the stamp must reach the age inside the dispatch");
        assert!(
            (40 * MS..1_000 * MS).contains(&age),
            "age {age} ns for a 40 ms old stamp"
        );
        assert_eq!(
            key_event_server_time(),
            None,
            "withdrawn after the dispatch"
        );
        assert_eq!(current_event_queue_age_ns(), None);

        // `CurrentTime` is present to winit and refused by the age.
        assert_eq!(
            dispatch_with_key_event_server_time(0, key_event_server_time),
            Some(0)
        );
        assert_eq!(
            dispatch_with_key_event_server_time(0, current_event_queue_age_ns),
            None
        );

        // A dispatch that unwinds withdraws the stamp too, so the next key (or a never-stamped
        // control-socket key) cannot inherit it.
        let unwound = std::panic::catch_unwind(|| {
            dispatch_with_key_event_server_time(stamp, || panic!("a key handler panicked"))
        });
        assert!(unwound.is_err());
        assert_eq!(key_event_server_time(), None);
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
pub(crate) fn current_event_queue_age_ns() -> Option<u64> {
    // winit exposes no event timestamp on this platform; the post-dequeue stamp stands.
    None
}

/// Whether the `RedrawRequested` being handled is one the X server asked for
/// with an `Expose` (`aterm_window::platform::x11::redraw_is_exposure`): the
/// server lost what the window showed, and the whole frame is owed
/// (`App::note_owed_repaint`). `false` for a redraw aterm asked for, on
/// Wayland, and off Linux.
#[cfg(target_os = "linux")]
pub(crate) fn redraw_is_exposure() -> bool {
    aterm_window::platform::x11::redraw_is_exposure()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn redraw_is_exposure() -> bool {
    false
}

/// Linux/BSD application-runtime using the **pure-winit** surface (NO new system
/// libraries — see [`AppRt::window_set_background_color`] and `toolbar.rs` for the
/// deferred GTK4 work): the appearance method drives `aterm_window::Window::set_theme` so
/// the client-side-decoration light/dark variant honours config `window_theme`; the
/// toolbar seam maintains a REAL in-memory tab-chrome model (`toolbar.rs`) and seeds
/// the window title from it; the menu install delegates to the menu stub; and
/// notification delivery forwards to `notify::spawn_delivery` (freedesktop
/// `Notify` on Linux, a channel-draining stub on the BSDs). The terminal renders +
/// input works; only the genuinely OS-native chrome a header bar would add (which
/// needs gtk4 system libs) is deferred.
#[cfg(all(not(target_os = "macos"), not(windows)))]
pub(crate) struct AppRtLinux {
    pub(crate) probe_preferences: bool,
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
impl AppRt for AppRtLinux {
    /// On X11, the window's background: what the X server paints where the window
    /// shows no frame yet (an area a resize just added), so it reads as the
    /// terminal's body rather than stale pixels. The window is created with it
    /// (`with_x11_background`, at attach); this follows a theme change. Wayland
    /// has no window background: a surface shows only what was posted to it.
    fn window_set_background_color(&self, window: &Window, bg: u32) {
        #[cfg(target_os = "linux")]
        aterm_window::platform::x11::WindowExtX11::set_x11_background(window, bg);
        #[cfg(not(target_os = "linux"))]
        let _ = (window, bg);
    }

    /// On X11, the window's word to its window manager that a frame is on it:
    /// a resize sync request that a size change left waiting for one is
    /// answered (`aterm-window`'s `sync_request.rs`). A no-op on Wayland and
    /// while no request waits.
    fn window_frame_presented(&self, window: &Window) {
        #[cfg(target_os = "linux")]
        aterm_window::platform::x11::WindowExtX11::x11_frame_presented(window);
        #[cfg(not(target_os = "linux"))]
        let _ = window;
    }

    /// Apply the window-chrome appearance by overriding winit's window theme: map
    /// config [`WindowTheme`] → [`aterm_window::window::Theme`] via [`window_theme_to_winit`]
    /// and hand it to [`Window::set_theme`]. On Wayland this themes the client-side
    /// decorations (titlebar/border); on X11 it sets the `_GTK_THEME_VARIANT` hint.
    /// The App never passes `Auto` here: `App::chrome_theme_for_apprt`
    /// resolves it to the terminal theme's side, so the title bar matches the
    /// terminal under it, the Windows caption's rule; that side follows an OS flip
    /// wherever the theme is a `dark:…,light:…` pair. `Auto` itself would take the
    /// X11 hint away (the window manager's default, the desktop's). GNOME 46's mutter
    /// draws X11 frames in the desktop's colour scheme whatever the hint says; window
    /// managers that read it (mutter before GNOME 44, Metacity) follow it. This is the
    /// real, buildable Linux analogue of the macOS `NSAppearance` override.
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        window.set_theme(window_theme_to_winit(theme));
    }

    /// CAMetalLayer colour-space tagging is a CoreAnimation concept. Linux
    /// presents SDR BY DESIGN (decided 2026-09-25 under the owner's standing
    /// direction): the GL arm's EGL window surface is presented directly as
    /// sRGB, and the extended-linear (EDR) tag is refused, so the aurora pass
    /// stays inert.
    /// HDR there would need a Wayland `color-management-v1` client plus an
    /// `Rgba16Float` swapchain — a lane nobody is building.
    fn window_set_surface_colorspace(
        &self,
        _window: &Window,
        cs: SurfaceColorspace,
    ) -> Option<SurfaceColorspace> {
        match cs {
            SurfaceColorspace::Srgb | SurfaceColorspace::DisplayP3 => Some(SurfaceColorspace::Srgb),
            SurfaceColorspace::ExtendedLinearSrgb => None,
        }
    }

    /// INTENTIONAL no-op: `contentsGravity` is a CoreAnimation concept, and the
    /// stale-frame rescale it defends against is specific to the `CAMetalLayer`
    /// SUBLAYER `raw-window-metal` installs. The EGL window surface on X11 and
    /// Wayland presents directly, so there is no intermediate layer to anchor.
    /// Documented rather than silently empty.
    fn window_anchor_surface_top_left(&self, _window: &Window) {}

    /// No CoreAnimation layer to read: the EGL window surface presents
    /// directly, so there is no intermediate layer whose gravity could
    /// rescale a stale frame — and therefore nothing to report.
    fn window_surface_presentation(&self, _window: &Window) -> Option<(String, f64, bool)> {
        None
    }

    /// No-op: the behind-window blur is an AppKit concept, so `background_material` has no
    /// window-level consumer here.
    fn window_set_vibrancy(
        &self,
        _window: &Window,
        _material: crate::app_config::BackgroundMaterial,
        _translucent: bool,
        _bg: u32,
    ) {
    }

    /// SDR by design (see `window_set_surface_colorspace`): no headroom, so the aurora pass is
    /// inert.
    fn screen_edr_max(&self, _window: &Window) -> f32 {
        1.0
    }

    /// No scaling: the scRGB reference-white scale is a Windows HDR concept.
    fn screen_sdr_white_scale(&self, _window: &Window) -> f32 {
        1.0
    }

    fn send_notification_init(
        &self,
        suppress: Arc<Mutex<HashSet<u64>>>,
        silent: Arc<AtomicBool>,
        own_alerts: Arc<AtomicBool>,
        center: crate::notice_center::NoticeCenter,
    ) -> SyncSender<NotifyMsg> {
        crate::notify::spawn_delivery(suppress, silent, own_alerts, center)
    }

    // The menu is `None` here by design: the own-rendered command palette and keybindings
    // are the cross-platform command surface (docs/INTROSPECTABLE_SURFACES_DESIGN.md), the
    // native macOS bar a mirror of it. The toolbar backs the in-memory tab-chrome model.
    fn install_menu(&self, proxy: &EventLoopProxy<Wake>) -> Option<menu::MenuHandle> {
        menu::install(proxy)
    }

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
            pill,
        } = tabs;
        toolbar::set_window_tabs(handle, titles, ids, metadata, tooltips, ext, active, pill);
    }

    fn read_toolbar_chrome(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        toolbar::read_tab_chrome(handle)
    }

    /// No native dialog off macOS: `None` tells the caller to ask through the
    /// in-window confirm banner (see `App::confirm_destructive_close`).
    fn confirm(&self, _title: &str, _body: &str, _proceed_label: &str) -> Option<bool> {
        None
    }

    /// No `terminate:`-style app-quit gesture to intercept off macOS: a no-op.
    fn install_quit_confirm(&self) {}

    /// The desktop's motion preference, GNOME's
    /// `org.gnome.desktop.interface enable-animations` key (the key GTK desktops
    /// share) read through the Settings portal — see [`gnome_reduce_motion`].
    /// Any failure answers `false`, so config `motion` alone decides.
    fn reduce_motion(&self) -> bool {
        self.probe_preferences && gnome_reduce_motion()
    }

    /// No OS notification to observe off macOS: `None` (the caller keeps the
    /// config-driven policy; a re-query costs nothing).
    fn observe_reduce_motion(&self, _proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver> {
        None
    }
}

/// The concrete application-runtime the `App` stores, selected at compile time:
/// [`AppRtMacOS`] on macOS, `AppRtWindows` on Windows, [`AppRtLinux`] elsewhere.
#[cfg(target_os = "macos")]
pub(crate) type PlatformAppRt = AppRtMacOS;

/// Windows: native DWM chrome (see `crate::platform_win`).
#[cfg(windows)]
pub(crate) type PlatformAppRt = crate::platform_win::AppRtWindows;

/// Every other target (Linux, the BSDs).
#[cfg(all(not(target_os = "macos"), not(windows)))]
pub(crate) type PlatformAppRt = AppRtLinux;

/// Construct the platform application-runtime for this build target. The single
/// place `App` mints its `apprt` field, so the cfg lives here, not at the
/// construction sites. Unit-test Apps keep native preference reads disabled;
/// explicit platform tests can construct a native instance themselves.
pub(crate) fn platform_apprt() -> PlatformAppRt {
    #[cfg(target_os = "macos")]
    {
        AppRtMacOS {
            probe_preferences: !cfg!(test),
        }
    }
    #[cfg(windows)]
    {
        crate::platform_win::AppRtWindows {
            probe_preferences: !cfg!(test),
        }
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        AppRtLinux {
            probe_preferences: !cfg!(test),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The URLs [`AppRt::open_url`] was asked to open on this thread
    /// (test-only): a test build opens none.
    static OPENED_URLS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The URLs this thread's [`AppRt::open_url`] and
/// `app_mouse::open_url_external` calls named, drained (test-only).
#[cfg(test)]
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "its readers, the status item's and the harness banner's press tests, are unix's"
    )
)]
pub(crate) fn take_opened_urls() -> Vec<String> {
    OPENED_URLS.with(|urls| std::mem::take(&mut *urls.borrow_mut()))
}

/// Note `url` as opened on this thread ([`take_opened_urls`]): what
/// `app_mouse::open_url_external` does in a test build, which opens none.
#[cfg(test)]
pub(crate) fn record_opened_url(url: &str) {
    OPENED_URLS.with(|urls| urls.borrow_mut().push(url.to_string()));
}

/// The runtime handed to a headless test or benchmark fixture. Its native
/// preference reads stay on the inert arm even when `bench-support` compiles
/// the fixture into a non-test library. The normal live factory is unchanged.
#[cfg(any(test, feature = "bench-support"))]
pub(crate) fn headless_fixture_apprt(mut apprt: PlatformAppRt) -> PlatformAppRt {
    apprt.probe_preferences = false;
    apprt
}

#[cfg(all(test, target_os = "macos"))]
mod headless_fixture_tests {
    use super::{AppRt as _, AppRtMacOS, headless_fixture_apprt};
    use crate::native_appearance::AppearancePreferences;

    #[test]
    fn a_headless_fixture_closes_the_native_preferences_arm() {
        // The existing test-build arm remains inert.
        let inert = headless_fixture_apprt(AppRtMacOS {
            probe_preferences: false,
        });
        assert!(!inert.probe_preferences);
        assert_eq!(
            inert.native_appearance_preferences(),
            AppearancePreferences::default()
        );
        assert!(!inert.reduce_motion());

        // A bench-support library is not cfg(test): its live factory hands
        // the fixture this armed state. Refuse it before calling any getter,
        // so a failed assertion never reaches NSWorkspace or another OS read.
        let fixture = headless_fixture_apprt(AppRtMacOS {
            probe_preferences: true,
        });
        assert!(
            !fixture.probe_preferences,
            "a headless fixture must not inherit the live native-preference arm"
        );
        assert_eq!(
            fixture.native_appearance_preferences(),
            AppearancePreferences::default()
        );
        assert!(!fixture.reduce_motion());
    }
}

/// The three display-accessibility reads [`AppRtMacOS`] answers
/// [`AppRt::reduce_motion`] and [`AppRt::native_appearance_preferences`] with, asked
/// once and dropped: the boot's off-thread warm (`warm_accessibility_reads` in
/// `lib.rs`), so AppKit's first, slow answer is made before the UI thread asks.
/// Its own pool: a worker thread has none of its own.
#[cfg(target_os = "macos")]
pub(crate) fn warm_accessibility_reads() {
    aterm_objc::autoreleasepool(|_| {
        let apprt = platform_apprt();
        let _ = apprt.native_appearance_preferences();
        let _ = apprt.reduce_motion();
    });
}

/// The retained reduce-motion notification observer [`AppRt::observe_reduce_motion`]
/// returns: the objc target on macOS, `()` elsewhere (so the `App` field type is
/// the same name on every platform, like `menu::MenuHandle`).
#[cfg(target_os = "macos")]
pub(crate) type ReduceMotionObserver = aterm_objc::Retained<reduce_motion::ReduceMotionTarget>;

/// See the macOS variant above — nothing to retain off macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) type ReduceMotionObserver = ();

/// The App Nap opt-out's activity token [`AppRt::hold_off_app_nap`] returns: the
/// retained `NSProcessInfo` activity on macOS, `()` elsewhere (so the `App`
/// field type is the same name on every platform, like [`ReduceMotionObserver`]).
#[cfg(target_os = "macos")]
pub(crate) type AppNapActivity = aterm_objc::Obj;

/// See the macOS variant above — nothing to hold off macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) type AppNapActivity = ();

/// App Nap, held off for the life of the windowed app: [`AppRt::hold_off_app_nap`]
/// on macOS. ONE activity, begun once; no config key and no environment switch.
///
/// 2026-09-28 (0.98.0): holding no activity, the whole process went unscheduled for 3 h 22 m on a
/// saturated Mac — agents alive, control socket dark — and thawed 5 ms after an App Nap assertion.
///
/// `-beginActivityWithOptions:reason:` returns the token at +0 and the activity ends when it is
/// deallocated (`NSProcessInfo.h`), so it is retained here and `App` keeps it for the process's
/// life; no `-endActivity:` is sent.
#[cfg(target_os = "macos")]
fn begin_app_nap_opt_out() -> Option<AppNapActivity> {
    use aterm_objc::{Obj, autoreleasepool, class, sel};

    use crate::appkit::{self, consts::NS_ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP};

    /// The activity's reason — "a string used in debugging to indicate the
    /// reason the activity began" (`NSProcessInfo.h`).
    const REASON: &str =
        "aterm hosts terminal sessions and the control socket agents drive them over";

    let token = autoreleasepool(|_| {
        let reason = appkit::nsstring(REASON)?;
        // SAFETY: `+processInfo` is `-(id)`, the immortal process singleton, and
        // `-beginActivityWithOptions:reason:` is `-(id<NSObject>)(NSActivityOptions, NSString *)`
        // (`@@:Q@`; the `uint64_t` options word is `NSUInteger`-wide on every Apple target, so
        // `send_id_usize_id` is its exact shape). `reason` is a live +1 NSString this frame owns.
        // The token comes back autoreleased (+0), so it is retained before the pool pops. This
        // runs on the main thread (`App::resumed`).
        unsafe {
            let info = appkit::send_id(class(c"NSProcessInfo").as_id(), sel!(processInfo));
            if info.is_null() {
                return None;
            }
            let token = appkit::send_id_usize_id(
                info,
                sel!(beginActivityWithOptions:reason:),
                NS_ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP,
                reason.id(),
            );
            Obj::retain(token)
        }
    });
    if token.is_some() {
        aterm_log::info!(
            "app nap: held off for the life of this process \
             (NSActivityUserInitiatedAllowingIdleSystemSleep; idle system sleep still allowed): \
             {REASON}"
        );
    } else {
        aterm_log::warn!(
            "app nap: NOT held off — -[NSProcessInfo beginActivityWithOptions:reason:] \
             answered nil, so macOS may nap this process and its control socket with it"
        );
    }
    token
}

/// How long the Linux motion read may take, the session bus's handshake
/// included. It runs once per window attach, on the main thread, so it is
/// bounded well under a frame budget's worth of visible delay; a desktop whose
/// portal is slower than this (one being started by its first call) simply
/// answers "not reduced" and config `motion` decides.
#[cfg(target_os = "linux")]
const MOTION_READ_BOUND: std::time::Duration = std::time::Duration::from_millis(200);

/// The GNOME motion preference, read from the desktop's Settings portal over
/// aterm-dbus ([`crate::portal_settings`]): the a11y ruling's optional step 7
/// (`docs/AUDIT-third-party-2026-10-15.md`, unit a11y), which replaced the
/// `gsettings get` child this read spawned. No session bus, no running portal
/// (the read never starts one), a timeout or any value but `false` answers
/// `false` (not reduced).
#[cfg(target_os = "linux")]
fn gnome_reduce_motion() -> bool {
    use crate::portal_settings::{ENABLE_ANIMATIONS, GNOME_INTERFACE};
    crate::portal_settings::animations_disabled(
        crate::portal_settings::read(GNOME_INTERFACE, ENABLE_ANIMATIONS, MOTION_READ_BOUND)
            .as_ref(),
    )
}

/// The other unixes aterm-gui builds for have no portal reader (aterm-dbus is
/// a Linux dependency): no preference, so config `motion` decides.
#[cfg(all(not(target_os = "macos"), not(windows), not(target_os = "linux")))]
fn gnome_reduce_motion() -> bool {
    false
}

/// Run `program args…` and return its stdout if it exits successfully within
/// `bound`; otherwise kill it and answer `None`. The child's stdin is null and
/// its stderr discarded, so nothing it does can reach the terminal.
#[cfg(any(all(not(target_os = "macos"), not(windows)), test))]
pub(crate) fn read_bounded_stdout(
    program: &str,
    args: &[&str],
    bound: std::time::Duration,
) -> Option<String> {
    use std::io::Read as _;
    let mut child = aterm_uds::spawnfd::command(program)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + bound;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod bounded_read_tests {
    // `read_bounded_stdout`'s own tests are unix's: they run `/bin/sh` and
    // `/bin/sleep`. On Windows the spawn fails, so the first asserted a stdout it
    // could not get and the second passed without a child.
    #[cfg(unix)]
    use super::read_bounded_stdout;

    #[cfg(unix)]
    #[test]
    fn the_bounded_read_returns_stdout_of_a_successful_child() {
        // The bound is a hang detector here: the appearance reader's 200 ms is the
        // product's latency budget for a desktop's `gdbus` and `gsettings`, and a
        // child spawned on a loaded machine can take longer than that to print one
        // word.
        assert_eq!(
            read_bounded_stdout(
                "/bin/sh",
                &["-c", "printf false"],
                aterm_spec::wait::MUST_HAPPEN
            )
            .as_deref(),
            Some("false")
        );
        assert_eq!(
            read_bounded_stdout(
                "/bin/sh",
                &["-c", "printf false; exit 3"],
                aterm_spec::wait::MUST_HAPPEN
            ),
            None,
            "a failing exit is not a preference"
        );
        assert_eq!(
            read_bounded_stdout(
                "/nonexistent/wbgui-no-such-gsettings",
                &[],
                aterm_spec::wait::MUST_HAPPEN
            ),
            None,
            "a missing binary is not a preference"
        );
    }

    /// The child sleeps ten minutes; the `MUST_HAPPEN` check is a hang detector, not a budget.
    #[cfg(unix)]
    #[test]
    fn the_bounded_read_kills_a_child_that_outlives_the_bound() {
        let started = std::time::Instant::now();
        assert_eq!(
            read_bounded_stdout("/bin/sleep", &["600"], std::time::Duration::from_millis(50)),
            None
        );
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the read must not wait for the child: took {:?}",
            started.elapsed()
        );
    }
}

/// macOS "Reduce Motion" integration (W11): the one query + the one observer.
/// Follows the [`crate::menu`] `MenuTarget` pattern — a declared `NSObject`
/// subclass owning the relay, registered on the WORKSPACE notification center for
/// `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification` and relaying each
/// post into the existing `Wake` channel (no policy logic here; the main thread
/// re-queries and resolves). The same observer relays
/// `NSWorkspaceDidWakeNotification` to the update checker
/// (`aterm_update::note_system_woke`), which ends its wait at once instead of
/// noticing the wake by the clock seconds later.
///
/// # THE W2 SEAM
///
/// W1 left this module HALF ported and named what remained. What was already
/// off `objc2` and onto [`aterm_objc`](aterm_objc) is the part W1 exists to
/// prove: the declared class, its ivars, its `dealloc`, its registration with
/// the notification centre, and the `Retained` handle `App` holds
/// ([`ReduceMotionObserver`]).
///
/// # THE W2 SEAM IS CLOSED
///
/// W1 left two things here and named them as W2's:
///
/// * [`query`] / [`increase_contrast`] / [`reduce_transparency`] — three plain
///   `msg_send!` BOOL getters, "the cheapest possible port (one typed cast
///   each)". They are ported, and the estimate was right: three lines each,
///   through [`crate::appkit`]. They are also three of the sites that `bool`
///   would have been wrong for on the x86_64 compat slice, which is why
///   `send_bool` returns Rust's `bool` only after asking the runtime for a
///   [`aterm_objc::Bool`].
/// * `MainThreadMarker` — objc2's proof-of-main-thread token, "a design
///   question, not a translation". The decision is
///   [`crate::appkit::MainThread`]: the same proof objc2 uses
///   (`+[NSThread isMainThread]`), asked at the boundary, carried in a
///   `!Send`/`!Sync` zero-sized witness, and NOT wired to `Send`/`Sync` on the
///   declared classes the way objc2 0.5's `Mutability` was —
///   [`aterm_objc::Retained`] is unconditionally `!Send` already, so the token
///   is left doing exactly one job. See that type for the reasoning.
///
/// This module now carries no `objc2` token at all.
#[cfg(target_os = "macos")]
mod reduce_motion {
    use aterm_objc::{class, sel};
    use aterm_window::event_loop::EventLoopProxy;

    use crate::Wake;
    use crate::appkit::{self, MainThread};

    /// The workspace notification this module observes, equal to AppKit's exported constant of
    /// the same name. It is posted on the workspace centre, not the default one.
    pub(crate) const DISPLAY_OPTIONS_NOTIFICATION: &str =
        "NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification";

    /// The workspace notification for a wake from sleep. Its value equals AppKit's
    /// exported `NSWorkspaceDidWakeNotification` constant; it too is posted on the
    /// WORKSPACE centre.
    pub(crate) const DID_WAKE_NOTIFICATION: &str = "NSWorkspaceDidWakeNotification";

    /// The system colours changed (the accent colour among them). Posted on
    /// the DEFAULT notification centre: the selected tab's automatic pill
    /// colour moves with it, and Settings names that colour.
    pub(crate) const SYSTEM_COLORS_NOTIFICATION: &str = "NSSystemColorsDidChangeNotification";

    /// The observer target's ivars.
    ///
    /// A boxed relay rather than the `EventLoopProxy<Wake>` itself: a proxy can
    /// only be minted from a live `aterm_window::EventLoop`, which on macOS must be
    /// built on the main thread and only once per process, so a class whose
    /// ivar IS the proxy cannot be driven from a test at all. Production still
    /// posts exactly `Wake::ReduceMotionChanged` and nothing else — see
    /// [`observe`] — and the seam is what lets the runtime test below fire the
    /// REAL class from a REAL AppKit notification.
    pub(crate) struct ReduceMotionIvars {
        relay: Box<dyn Fn()>,
        /// Called on each [`DID_WAKE_NOTIFICATION`]: production tells the update
        /// checker (`aterm_update::note_system_woke`), off the event loop — the post
        /// only notifies a condition variable.
        woke: Box<dyn Fn()>,
        /// Called on each [`SYSTEM_COLORS_NOTIFICATION`]: production posts
        /// `Wake::SystemColorsChanged`, so every window repaints the colours
        /// it reads off the system.
        colors: Box<dyn Fn()>,
    }

    aterm_objc::declare_class! {
        /// The notification-observer target: one `reduceMotionDidChange:` selector that posts
        /// [`Wake::ReduceMotionChanged`]. The main thread re-queries the flag, so a burst of
        /// notifications coalesces to the latest value.
        pub(crate) struct ReduceMotionTarget: NSObject {
            const NAME: &str = "ATermReduceMotionTarget";
            type Ivars = ReduceMotionIvars;

            /// The observer selector: runs the relay.
            @sel(reduceMotionDidChange:)
            fn reduce_motion_did_change(&self, _note: aterm_objc::Id) {
                (self.ivars().relay)();
            }

            /// The wake selector: the system woke from sleep.
            @sel(systemDidWake:)
            fn system_did_wake(&self, _note: aterm_objc::Id) {
                (self.ivars().woke)();
            }

            /// The system colours changed (the accent among them).
            @sel(systemColorsDidChange:)
            fn system_colors_did_change(&self, _note: aterm_objc::Id) {
                (self.ivars().colors)();
            }
        }
    }

    /// Build a target around arbitrary relays. `observe` is the production
    /// caller; the tests below are the other one.
    fn new_target(
        mtm: aterm_objc::MainThread,
        relay: Box<dyn Fn()>,
        woke: Box<dyn Fn()>,
        colors: Box<dyn Fn()>,
    ) -> Option<aterm_objc::Retained<ReduceMotionTarget>> {
        ReduceMotionTarget::alloc_init(
            mtm,
            ReduceMotionIvars {
                relay,
                woke,
                colors,
            },
        )
    }

    /// `[NSNotificationCenter defaultCenter]`, where AppKit posts
    /// [`SYSTEM_COLORS_NOTIFICATION`].
    fn default_center() -> aterm_objc::Id {
        // SAFETY: `+defaultCenter` is `-(id)`, the standard immortal singleton
        // accessor, only borrowed here.
        unsafe {
            let f: unsafe extern "C-unwind" fn(
                aterm_objc::ClassPtr,
                aterm_objc::Sel,
            ) -> aterm_objc::Id = aterm_objc::checked_msg();
            f(
                aterm_objc::class(c"NSNotificationCenter"),
                aterm_objc::sel!(defaultCenter),
            )
        }
    }

    /// `[[NSWorkspace sharedWorkspace] notificationCenter]`, or null.
    fn workspace_center() -> aterm_objc::Id {
        // SAFETY: `+sharedWorkspace` and `-notificationCenter` are `-(id)` accessors returning
        // autoreleased or immortal objects, only borrowed here.
        unsafe {
            let f: unsafe extern "C-unwind" fn(
                aterm_objc::ClassPtr,
                aterm_objc::Sel,
            ) -> aterm_objc::Id = aterm_objc::checked_msg();
            let ws = f(
                aterm_objc::class(c"NSWorkspace"),
                aterm_objc::sel!(sharedWorkspace),
            );
            if ws.is_null() {
                return aterm_objc::Id::NIL;
            }
            let g: unsafe extern "C-unwind" fn(aterm_objc::Id, aterm_objc::Sel) -> aterm_objc::Id =
                aterm_objc::checked_msg();
            g(ws, aterm_objc::sel!(notificationCenter))
        }
    }

    /// Register `target` on the workspace centre for the display-options
    /// notification and the wake notification. `false` if the workspace or its
    /// centre is unavailable.
    ///
    /// The centre holds the observer WEAKLY and this module never removes it,
    /// exactly as before: `App` keeps the returned handle in a field for the
    /// process lifetime.
    fn register(target: &aterm_objc::Retained<ReduceMotionTarget>) -> bool {
        // A scope, not an RAII token: a pool token dropped out of order is a use-after-free,
        // which is why `AutoreleasePool::new` is `unsafe`.
        aterm_objc::autoreleasepool(|_| {
            let center = workspace_center();
            if center.is_null() {
                return false;
            }
            let observations = [
                (
                    DISPLAY_OPTIONS_NOTIFICATION,
                    aterm_objc::sel!(reduceMotionDidChange:),
                ),
                (DID_WAKE_NOTIFICATION, aterm_objc::sel!(systemDidWake:)),
            ];
            for (notification, selector) in observations {
                let Some(name) = aterm_objc::ns_string(notification) else {
                    return false;
                };
                // SAFETY: `-addObserver:selector:name:object:` is
                // `-(void)(id, SEL, id, id)`, exactly the prototype cast below. The
                // observer is `target`, which the caller keeps alive; the selector
                // is one this class declares; `name` is a live +1 NSString the pool
                // outlives; nil `object` means "from any sender".
                unsafe {
                    let add: unsafe extern "C-unwind" fn(
                        aterm_objc::Id,
                        aterm_objc::Sel,
                        aterm_objc::Id,
                        aterm_objc::Sel,
                        aterm_objc::Id,
                        aterm_objc::Id,
                    ) = aterm_objc::checked_msg();
                    add(
                        center,
                        aterm_objc::sel!(addObserver:selector:name:object:),
                        target.as_id(),
                        selector,
                        name.id(),
                        aterm_objc::Id::NIL,
                    );
                }
            }
            // The system colours (the accent among them) change on the
            // default centre, not the workspace's.
            let center = default_center();
            if let (false, Some(name)) = (
                center.is_null(),
                aterm_objc::ns_string(SYSTEM_COLORS_NOTIFICATION),
            ) {
                // SAFETY: as above: `-addObserver:selector:name:object:` on the
                // live default centre, the kept `target`, a selector this class
                // declares, a live +1 NSString, and a nil sender.
                unsafe {
                    let add: unsafe extern "C-unwind" fn(
                        aterm_objc::Id,
                        aterm_objc::Sel,
                        aterm_objc::Id,
                        aterm_objc::Sel,
                        aterm_objc::Id,
                        aterm_objc::Id,
                    ) = aterm_objc::checked_msg();
                    add(
                        center,
                        aterm_objc::sel!(addObserver:selector:name:object:),
                        target.as_id(),
                        aterm_objc::sel!(systemColorsDidChange:),
                        name.id(),
                        aterm_objc::Id::NIL,
                    );
                }
            }
            true
        })
    }

    /// Query `NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion`
    /// — the live OS "Reduce Motion" accessibility preference.
    pub(crate) fn query() -> bool {
        workspace_flag(sel!(accessibilityDisplayShouldReduceMotion))
    }

    /// `[[NSWorkspace sharedWorkspace] <flag>]`, or `false` if there is no
    /// workspace. The one shape all three accessibility getters have.
    fn workspace_flag(flag: aterm_objc::Sel) -> bool {
        // SAFETY: `+sharedWorkspace` is `-(id)`, the standard singleton accessor, and each
        // `flag` is a side-effect-free `-(BOOL)` property read, converted once in `send_bool`.
        unsafe {
            let ws = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            if ws.is_null() {
                return false;
            }
            appkit::send_bool(ws, flag)
        }
    }

    /// Query the sibling display-accessibility values posted by the same workspace
    /// notification. Keeping all three under one observer makes a burst atomic at the
    /// next event-loop turn.
    pub(crate) fn increase_contrast() -> bool {
        workspace_flag(sel!(accessibilityDisplayShouldIncreaseContrast))
    }

    pub(crate) fn reduce_transparency() -> bool {
        workspace_flag(sel!(accessibilityDisplayShouldReduceTransparency))
    }

    pub(crate) fn differentiate_without_color() -> bool {
        workspace_flag(sel!(accessibilityDisplayShouldDifferentiateWithoutColor))
    }

    /// Register a workspace-notification observer for the accessibility
    /// display-options change and the wake from sleep, and return the retained
    /// target (the caller keeps it alive for the process life — the notification
    /// center references it weakly, and it is never removed, exactly like the menu
    /// target). `None` off the main thread (AppKit requirement; the winit loop
    /// guarantees it).
    pub(crate) fn observe(
        proxy: &EventLoopProxy<Wake>,
    ) -> Option<aterm_objc::Retained<ReduceMotionTarget>> {
        let main_thread = MainThread::new()?;
        let colors = proxy.clone();
        let proxy = proxy.clone();
        let target = new_target(
            main_thread,
            Box::new(move || {
                // Fire-and-forget: a closed loop (app shutting down) drops the event.
                let _ = proxy.send_event(Wake::ReduceMotionChanged);
            }),
            Box::new(aterm_update::note_system_woke),
            Box::new(move || {
                let _ = colors.send_event(Wake::SystemColorsChanged);
            }),
        )?;
        if register(&target) {
            Some(target)
        } else {
            None
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::{
            DID_WAKE_NOTIFICATION, DISPLAY_OPTIONS_NOTIFICATION, ReduceMotionTarget, new_target,
            register,
        };

        /// Remove `target` from the workspace centre. Production never does (the observer lives
        /// for the process); a test must, or the centre keeps a dangling weak reference.
        fn unregister(target: &aterm_objc::Retained<ReduceMotionTarget>) {
            for center in [super::workspace_center(), super::default_center()] {
                if center.is_null() {
                    continue;
                }
                // SAFETY: `-removeObserver:` is `-(void)(id)`; `center` is a
                // live centre and `target` a live observer registered on it.
                unsafe {
                    let f: unsafe extern "C-unwind" fn(
                        aterm_objc::Id,
                        aterm_objc::Sel,
                        aterm_objc::Id,
                    ) = aterm_objc::checked_msg();
                    f(center, aterm_objc::sel!(removeObserver:), target.as_id());
                }
            }
        }

        /// Post `notification` on the REAL default centre.
        fn post_default(notification: &str) {
            aterm_objc::autoreleasepool(|_| {
                let center = super::default_center();
                assert!(!center.is_null(), "no default notification centre");
                let name = aterm_objc::ns_string(notification).expect("NSString");
                // SAFETY: `-postNotificationName:object:` is `-(void)(id, id)`;
                // `center` is live, `name` a live +1 NSString, nil sender is
                // legal.
                unsafe {
                    let f: unsafe extern "C-unwind" fn(
                        aterm_objc::Id,
                        aterm_objc::Sel,
                        aterm_objc::Id,
                        aterm_objc::Id,
                    ) = aterm_objc::checked_msg();
                    f(
                        center,
                        aterm_objc::sel!(postNotificationName:object:),
                        name.id(),
                        aterm_objc::Id::NIL,
                    );
                }
            });
        }

        /// Post the REAL display-options notification on the REAL workspace centre.
        fn post() {
            post_named(DISPLAY_OPTIONS_NOTIFICATION);
        }

        /// Post the REAL notification `notification` on the REAL workspace centre.
        fn post_named(notification: &str) {
            aterm_objc::autoreleasepool(|_| {
                let center = super::workspace_center();
                assert!(!center.is_null(), "no workspace notification centre");
                let name = aterm_objc::ns_string(notification).expect("NSString");
                // SAFETY: `-postNotificationName:object:` is `-(void)(id, id)`;
                // `center` is live, `name` a live +1 NSString, nil sender is
                // legal.
                unsafe {
                    let f: unsafe extern "C-unwind" fn(
                        aterm_objc::Id,
                        aterm_objc::Sel,
                        aterm_objc::Id,
                        aterm_objc::Id,
                    ) = aterm_objc::checked_msg();
                    f(
                        center,
                        aterm_objc::sel!(postNotificationName:object:),
                        name.id(),
                        aterm_objc::Id::NIL,
                    );
                }
            });
        }

        /// The class is looked up by name in the Objective-C runtime, registered with the real
        /// `NSWorkspace` centre, and AppKit's dispatch of the real notification calls the Rust
        /// body.
        #[test]
        fn a_real_workspace_notification_reaches_the_declared_class() {
            use aterm_objc::{ClassType, class, class_name, superclass_of};

            let hits = Arc::new(AtomicUsize::new(0));
            let seen = Arc::clone(&hits);
            let wakes = Arc::new(AtomicUsize::new(0));
            let woke = Arc::clone(&wakes);
            let recolors = Arc::new(AtomicUsize::new(0));
            let recolored = Arc::clone(&recolors);
            let target = new_target(
                crate::appkit::test_witness(),
                Box::new(move || {
                    seen.fetch_add(1, Ordering::SeqCst);
                }),
                Box::new(move || {
                    woke.fetch_add(1, Ordering::SeqCst);
                }),
                Box::new(move || {
                    recolored.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .expect("+alloc/-init produced a ReduceMotionTarget");

            // The runtime's own view of what got registered.
            let cls = ReduceMotionTarget::class();
            assert!(!cls.is_null());
            assert_eq!(
                class(c"ATermReduceMotionTarget"),
                cls,
                "objc_getClass disagrees with the registered pair"
            );
            // SAFETY: `cls` is the class this module just registered, so it
            // and its superclass are live, immortal class objects.
            unsafe {
                assert_eq!(class_name(cls), c"ATermReduceMotionTarget");
                assert_eq!(class_name(superclass_of(cls)), c"NSObject");
            }
            // SAFETY: `target` is a live instance; `-respondsToSelector:` is a
            // side-effect-free query.
            unsafe {
                // `BOOL` is `signed char` on the x86_64 slice, so the return is `Bool`.
                let responds: unsafe extern "C-unwind" fn(
                    aterm_objc::Id,
                    aterm_objc::Sel,
                    aterm_objc::Sel,
                ) -> aterm_objc::Bool = aterm_objc::checked_msg();
                assert!(
                    responds(
                        target.as_id(),
                        aterm_objc::sel!(respondsToSelector:),
                        aterm_objc::sel!(reduceMotionDidChange:)
                    )
                    .as_bool()
                );
            }

            assert!(register(&target), "workspace centre registration failed");
            post();
            post();
            // The wake reaches the wake relay, and only it.
            post_named(DID_WAKE_NOTIFICATION);
            // The system colours (the accent) reach the colours relay, from
            // the default centre.
            post_default(super::SYSTEM_COLORS_NOTIFICATION);
            unregister(&target);
            // Removed: a further post must not reach it.
            post();
            post_named(DID_WAKE_NOTIFICATION);
            post_default(super::SYSTEM_COLORS_NOTIFICATION);
            assert_eq!(
                recolors.load(Ordering::SeqCst),
                1,
                "NSSystemColorsDidChangeNotification did not reach the colours relay \
                 exactly once"
            );

            assert_eq!(
                hits.load(Ordering::SeqCst),
                2,
                "AppKit did not deliver the notification to the declared class \
                 exactly as many times as it was posted"
            );
            assert_eq!(
                wakes.load(Ordering::SeqCst),
                1,
                "AppKit did not deliver NSWorkspaceDidWakeNotification to the wake \
                 selector exactly once"
            );
        }

        /// The generated `dealloc` drops the ivars (a boxed closure owning an `Arc`) with the
        /// last reference.
        #[test]
        fn dropping_the_target_drops_its_relay() {
            struct Spy(Arc<AtomicUsize>);
            impl Drop for Spy {
                fn drop(&mut self) {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
            let drops = Arc::new(AtomicUsize::new(0));
            let spy = Spy(Arc::clone(&drops));
            let target = new_target(
                crate::appkit::test_witness(),
                Box::new(move || {
                    let _ = &spy;
                }),
                Box::new(|| {}),
                Box::new(|| {}),
            )
            .expect("target");
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            drop(target);
            assert_eq!(
                drops.load(Ordering::SeqCst),
                1,
                "the generated -dealloc did not drop the relay"
            );
        }
    }
}

/// CAMetalLayer colour-space tagging: the one place that reaches the wgpu-created metal layer
/// and sets its `colorspace`. Hand-rolled CoreGraphics FFI in the style of `lib.rs`'s
/// `cg_capture`; AppKit already links CoreGraphics, so the `#[link]` adds no dependency.
#[cfg(target_os = "macos")]
mod layer_colorspace {
    use std::ffi::c_void;

    use aterm_objc::{Id, Sel, class, sel};
    use aterm_window::window::Window;

    use super::ns_view_of;
    use crate::appkit;

    /// An opaque `CGColorSpace`, as a bare pointer. `aterm-objc` checks no argument encodings:
    /// a send is a typed cast to the selector's exact C prototype, so a wrong cast is undefined
    /// behaviour, not a runtime abort (the posture of `aterm-gpu/src/metal/ffi.rs`).
    type CGColorSpaceRef = *mut c_void;

    // SAFETY: standard, stable CoreGraphics C entry points with the published
    // signatures; contracts honoured at the call site below (the one Create is
    // released exactly once, after the layer setter retained it).
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        /// Create one of the named colour spaces (`kCGColorSpace*`); NULL when
        /// the name is unknown to this OS. `name` is a `CFStringRef` — an
        /// `NSString` is toll-free-bridged and passed directly.
        fn CGColorSpaceCreateWithName(name: *const c_void) -> CGColorSpaceRef;
        fn CGColorSpaceRelease(space: CGColorSpaceRef);
    }

    /// `-[CALayer isKindOfClass:CAMetalLayer]`, with the class looked up dynamically so a
    /// process without QuartzCore degrades to a no-op instead of a panic.
    ///
    /// # Safety
    /// `obj` must be nil or a live object.
    unsafe fn is_metal_layer(obj: Id) -> bool {
        let metal = class(c"CAMetalLayer");
        if obj.is_null() || metal.is_null() {
            return false;
        }
        // SAFETY: `-isKindOfClass:` is `-(BOOL)(Class)` and side-effect free. The argument is a
        // Class (`@encode` `"#"`, not `"@"`), which `ClassPtr` keeps distinct; hence not
        // `send_bool_id`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::ClassPtr) -> aterm_objc::Bool =
                aterm_objc::checked_msg();
            f(obj, sel!(isKindOfClass:), metal).as_bool()
        }
    }

    /// Find the CAMetalLayer wgpu attached to `view` and set its `colorspace` to the named CG
    /// colour space; returns whether it did. On any missing link (no AppKit handle, no layer,
    /// unknown name) the layer keeps its previous tag.
    #[must_use]
    pub(crate) fn set(window: &Window, cg_name: &str) -> bool {
        let view = ns_view_of(window);
        let Some(name) = appkit::nsstring(cg_name) else {
            return false;
        };
        // SAFETY: `view` is this window's live NSView, borrowed on the main thread (only the
        // winit event loop reaches this); the layer walk reads CALayers that view retains.
        // `CGColorSpaceCreateWithName` takes a `CFStringRef`, which the toll-free-bridged
        // NSString is, and returns +1; `-setColorspace:` is `-(void)(CGColorSpaceRef)` (a bare
        // pointer) and retains, so releasing our reference right after is balanced.
        unsafe {
            let Some(layer) = find_metal_layer(view) else {
                return false;
            };
            let cs = CGColorSpaceCreateWithName(name.id().as_ptr().cast_const());
            if cs.is_null() {
                return false;
            }
            let set_cs: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) =
                aterm_objc::checked_msg();
            set_cs(layer, sel!(setColorspace:), cs);
            CGColorSpaceRelease(cs);
            true
        }
    }

    /// Set the window's CAMetalLayer `contentsGravity` to the visual top-left, so a bounds
    /// change not yet repainted leaves the previous drawable at 1:1 instead of stretching it
    /// (see [`super::AppRt::window_anchor_surface_top_left`]). No metal layer is a no-op.
    pub(crate) fn anchor_contents_top_left(window: &Window) {
        // SAFETY: the `CALayerContentsGravity` constants are stable exported
        // QuartzCore symbols; these are reads of immutable `CFStringRef` globals,
        // never freed or mutated here.
        #[link(name = "QuartzCore", kind = "framework")]
        unsafe extern "C" {
            /// Pins unrescaled contents to the layer's maximum-Y left corner.
            static kCAGravityTopLeft: *const c_void;
            /// Pins unrescaled contents to the layer's minimum-Y left corner.
            static kCAGravityBottomLeft: *const c_void;
        }
        let view = ns_view_of(window);
        // SAFETY: `view` is this window's live NSView, borrowed on the main thread;
        // `-contentsAreFlipped` is a side-effect-free `-(BOOL)` read and `-setContentsGravity:`
        // a `-(void)(NSString *)` property write of a framework constant.
        unsafe {
            let Some(layer) = find_metal_layer(view) else {
                return;
            };
            // Which corner is visually top is not a constant: CoreAnimation names gravity on
            // the layer's own Y axis ("'top' always means Maximum Y", `CALayer.h`), and this
            // layer is a sublayer of winit's flipped NSView. `-contentsAreFlipped` says
            // whether minimum-Y is the visual top; pick the constant that lands on the visual
            // top-left, the corner a terminal grows from.
            let flipped = appkit::send_bool(layer, sel!(contentsAreFlipped));
            let gravity = if flipped {
                kCAGravityBottomLeft
            } else {
                kCAGravityTopLeft
            };
            if gravity.is_null() {
                return;
            }
            appkit::send_v_id(
                layer,
                sel!(setContentsGravity:),
                Id::from_ptr(gravity.cast_mut()),
            );
        }
    }

    /// Read back the CAMetalLayer's live `contentsGravity`, `contentsScale` and
    /// `contentsAreFlipped`, the state [`anchor_contents_top_left`] establishes (see
    /// [`super::AppRt::window_surface_presentation`]). Pure reads; `None` with no metal layer.
    pub(crate) fn read_contents_presentation(window: &Window) -> Option<(String, f64, bool)> {
        let view = ns_view_of(window);
        // SAFETY: `view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires.
        // Every message below is a side-effect-free property read; the returned
        // `contentsGravity` is a borrowed NSString copied out immediately.
        unsafe {
            let layer = find_metal_layer(view)?;
            let gravity = appkit::send_id(layer, sel!(contentsGravity));
            let gravity = if gravity.is_null() {
                "none".to_string()
            } else {
                appkit::nsstring_to_rust(gravity)
            };
            let scale = appkit::send_f64(layer, sel!(contentsScale));
            let flipped = appkit::send_bool(layer, sel!(contentsAreFlipped));
            Some((gravity, scale, flipped))
        }
    }

    /// Set the window's CAMetalLayer `opaque` flag. `false` lets the GPU's translucent
    /// (PostMultiplied) present composite over the `NSVisualEffectView` behind it; `true`
    /// restores the opaque default. No metal layer is a silent no-op.
    pub(crate) fn set_metal_opaque(window: &Window, opaque: bool) {
        let view = ns_view_of(window);
        // SAFETY: `view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires;
        // `-setOpaque:` is `-(void)(BOOL)` on the CAMetalLayer.
        unsafe {
            let Some(layer) = find_metal_layer(view) else {
                return;
            };
            appkit::send_v_bool(layer, sel!(setOpaque:), opaque);
        }
    }

    /// The CAMetalLayer under `view`, if any. wgpu (via raw-window-metal) either re-types the
    /// view's backing layer or, the common winit case, installs a sublayer, so check the layer
    /// itself, then one level of sublayers.
    ///
    /// SAFETY: caller guarantees `view` is a live NSView borrowed on the main
    /// thread; every message below is a side-effect-free read.
    unsafe fn find_metal_layer(view: Id) -> Option<Id> {
        if view.is_null() {
            return None;
        }
        unsafe {
            let layer = appkit::send_id(view, sel!(layer));
            if layer.is_null() {
                return None;
            }
            if is_metal_layer(layer) {
                return Some(layer);
            }
            let subs = appkit::send_id(layer, sel!(sublayers));
            if subs.is_null() {
                return None;
            }
            let n = appkit::send_usize(subs, sel!(count));
            let at: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = aterm_objc::checked_msg();
            for i in 0..n {
                let l = at(subs, sel!(objectAtIndex:), i);
                if is_metal_layer(l) {
                    return Some(l);
                }
            }
        }
        None
    }
}

/// macOS vibrancy: install / update / remove the window-level `NSVisualEffectView` behind the
/// Metal layer and flip the window and layer opacity, so the GPU's translucent (PostMultiplied)
/// present composites over a live blurred backdrop. Main thread only, as AppKit requires.
#[cfg(target_os = "macos")]
mod vibrancy {
    use aterm_objc::{Id, Sel, class, sel};
    use aterm_window::window::Window;

    use super::{layer_colorspace, ns_window_of};
    use crate::app_config::BackgroundMaterial;
    use crate::appkit;

    // `NSVisualEffectMaterial` and friends at the SDK's own numbers (`NSVisualEffectView.h`;
    // `NSView.h` for the autoresize bits, `NSWindow.h` for the ordering mode): header
    // enumerators with no exported symbol. Only the three materials the config exposes.
    const MATERIAL_SIDEBAR: isize = 7; // NSVisualEffectMaterialSidebar
    const MATERIAL_HUD: isize = 13; // NSVisualEffectMaterialHUDWindow
    const MATERIAL_UNDER_WINDOW: isize = 21; // NSVisualEffectMaterialUnderWindowBackground
    // `NSVisualEffectBlendingModeBehindWindow` (blur the desktop behind the window).
    const BLENDING_BEHIND_WINDOW: isize = 0;
    // `NSVisualEffectStateActive` (always blur, don't track window key state — a
    // terminal reads its background the same whether or not it holds focus).
    const STATE_ACTIVE: isize = 1;
    // `NSViewWidthSizable | NSViewHeightSizable` — the backdrop tracks resizes.
    const AUTORESIZE_WH: usize = 2 | 16;
    // `NSWindowOrderingMode::NSWindowBelow` — put the backdrop under its siblings.
    const ORDER_BELOW: isize = -1;

    /// The `NSVisualEffectMaterial` value for a config material, or `None` for the
    /// `None` variant (translucency without a blur view).
    fn material_value(m: BackgroundMaterial) -> Option<isize> {
        match m {
            BackgroundMaterial::None => None,
            BackgroundMaterial::Hud => Some(MATERIAL_HUD),
            BackgroundMaterial::Sidebar => Some(MATERIAL_SIDEBAR),
            BackgroundMaterial::UnderWindow => Some(MATERIAL_UNDER_WINDOW),
        }
    }

    /// Apply the resolved vibrancy state to `window`. See
    /// [`super::AppRt::window_set_vibrancy`] for the full contract.
    pub(crate) fn set(window: &Window, material: BackgroundMaterial, translucent: bool, bg: u32) {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return;
        }
        // SAFETY: every message below is a standard AppKit call on the main
        // thread. `-contentView` is `-(id)`; `-setOpaque:` is `-(void)(BOOL)`;
        // `+clearColor` is `-(id)` returning a shared immortal colour and
        // `+colorWithSRGBRed:green:blue:alpha:` an AUTORELEASED one, both
        // borrowed only for the length of `-setBackgroundColor:`, which is
        // `-(void)(NSColor *)` and RETAINS.
        unsafe {
            let content = appkit::send_id(ns_window, sel!(contentView));
            if content.is_null() {
                return;
            }
            // Idempotent: drop any effect view a prior call installed (identified by class) and
            // recompute from a clean slate.
            remove_effect_views(content);

            if translucent {
                // A behind-window blur needs a non-opaque window with a clear fill, so the
                // blurred desktop shows through the translucent pixels the Metal layer composites.
                appkit::send_v_bool(ns_window, sel!(setOpaque:), false);
                let clear = appkit::send_id(class(c"NSColor").as_id(), sel!(clearColor));
                if !clear.is_null() {
                    appkit::send_v_id(ns_window, sel!(setBackgroundColor:), clear);
                }
                if let Some(mat) = material_value(material) {
                    install_effect_view(content, mat);
                }
                // Let the translucent (PostMultiplied) frame reach the backdrop.
                layer_colorspace::set_metal_opaque(window, false);
            } else {
                // Restore the opaque default: no backdrop, an opaque window painted
                // the theme bg (the seamless-titlebar fill), an opaque Metal layer.
                appkit::send_v_bool(ns_window, sel!(setOpaque:), true);
                let r = f64::from((bg >> 16) & 0xff) / 255.0;
                let g = f64::from((bg >> 8) & 0xff) / 255.0;
                let b = f64::from(bg & 0xff) / 255.0;
                let make: unsafe extern "C-unwind" fn(Id, Sel, f64, f64, f64, f64) -> Id =
                    aterm_objc::checked_msg();
                let color = make(
                    class(c"NSColor").as_id(),
                    sel!(colorWithSRGBRed:green:blue:alpha:),
                    r,
                    g,
                    b,
                    1.0,
                );
                if !color.is_null() {
                    appkit::send_v_id(ns_window, sel!(setBackgroundColor:), color);
                }
                layer_colorspace::set_metal_opaque(window, true);
            }
        }
    }

    /// Remove every `NSVisualEffectView` among `content`'s direct subviews (the ones this
    /// module installed), gathered first so the live `subviews` array is not mutated mid-walk.
    ///
    /// SAFETY: `content` is a live NSView on the main thread.
    unsafe fn remove_effect_views(content: Id) {
        let effect_cls = class(c"NSVisualEffectView");
        if effect_cls.is_null() {
            return;
        }
        unsafe {
            let subs = appkit::send_id(content, sel!(subviews));
            if subs.is_null() {
                return;
            }
            let n = appkit::send_usize(subs, sel!(count));
            let at: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = aterm_objc::checked_msg();
            // `-isKindOfClass:` takes a Class (`#`), not an object, hence the explicit prototype.
            let is_kind: unsafe extern "C-unwind" fn(
                Id,
                Sel,
                aterm_objc::ClassPtr,
            ) -> aterm_objc::Bool = aterm_objc::checked_msg();
            let mut ours: Vec<Id> = Vec::new();
            for i in 0..n {
                let sv = at(subs, sel!(objectAtIndex:), i);
                if sv.is_null() {
                    continue;
                }
                if is_kind(sv, sel!(isKindOfClass:), effect_cls).as_bool() {
                    ours.push(sv);
                }
            }
            for sv in ours {
                appkit::send_v(sv, sel!(removeFromSuperview));
            }
        }
    }

    /// Create an `NSVisualEffectView` filling `content`'s bounds with `material`, behind-window
    /// blending and active state, and add it below the existing subviews. The class is looked
    /// up dynamically, so a process without it gets a no-op.
    ///
    /// SAFETY: `content` is a live NSView on the main thread.
    ///
    /// Memory: `alloc`+`initWithFrame:` returns a +1 we own and `addSubview:` takes its own
    /// retain, so ours is released after adding, or every vibrancy re-apply leaks a GPU-backed
    /// view. On the nil path nothing is released: a failing `init` already released `self`.
    unsafe fn install_effect_view(content: Id, material: isize) {
        let effect_cls = class(c"NSVisualEffectView");
        if effect_cls.is_null() {
            return;
        }
        unsafe {
            let bounds = appkit::send_rect(content, sel!(bounds));
            let effect =
                appkit::send_id_rect(appkit::alloc(effect_cls), sel!(initWithFrame:), bounds);
            if effect.is_null() {
                return; // init consumed (released) the alloc — nothing for us to free
            }
            appkit::send_v_isize(effect, sel!(setMaterial:), material);
            appkit::send_v_isize(effect, sel!(setBlendingMode:), BLENDING_BEHIND_WINDOW);
            appkit::send_v_isize(effect, sel!(setState:), STATE_ACTIVE);
            appkit::send_v_usize(effect, sel!(setAutoresizingMask:), AUTORESIZE_WH);
            let add: unsafe extern "C-unwind" fn(Id, Sel, Id, isize, Id) =
                aterm_objc::checked_msg();
            add(
                content,
                sel!(addSubview:positioned:relativeTo:),
                effect,
                ORDER_BELOW,
                Id::NIL,
            );
            // Balance the create-rule +1; the superview now owns the view. Sent directly: the
            // +1 goes straight to `addSubview:` and is never held in Rust.
            appkit::send_v(effect, sel!(release));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{MATERIAL_HUD, MATERIAL_SIDEBAR, MATERIAL_UNDER_WINDOW, material_value};
        use crate::app_config::BackgroundMaterial;

        /// The config → `NSVisualEffectMaterial` mapping is total and hits the
        /// three semantic materials the terminal exposes; `None` installs no view.
        #[test]
        fn material_value_maps_the_exposed_materials() {
            assert_eq!(material_value(BackgroundMaterial::None), None);
            assert_eq!(material_value(BackgroundMaterial::Hud), Some(MATERIAL_HUD));
            assert_eq!(
                material_value(BackgroundMaterial::Sidebar),
                Some(MATERIAL_SIDEBAR)
            );
            assert_eq!(
                material_value(BackgroundMaterial::UnderWindow),
                Some(MATERIAL_UNDER_WINDOW)
            );
            // The values equal AppKit's stable NSVisualEffectMaterial constants.
            assert_eq!(
                (MATERIAL_SIDEBAR, MATERIAL_HUD, MATERIAL_UNDER_WINDOW),
                (7, 13, 21)
            );
        }
    }
}

/// The [`SurfaceColorspace`] mapping is pure and platform-independent, so pin it
/// everywhere (the macOS impl consumes it; the strings are stable CG ABI).
#[cfg(test)]
mod surface_colorspace_tests {
    use super::{SurfaceColorspace, capture_space_after_surface_tag, resolve_surface_colorspace};
    use crate::app_config::WindowColorspace;

    #[test]
    fn config_maps_one_to_one_and_names_are_the_cg_constants() {
        assert_eq!(
            SurfaceColorspace::from(WindowColorspace::Srgb),
            SurfaceColorspace::Srgb
        );
        assert_eq!(
            SurfaceColorspace::from(WindowColorspace::DisplayP3),
            SurfaceColorspace::DisplayP3
        );
        // The kCGColorSpace* constants' values equal their names (stable ABI).
        assert_eq!(SurfaceColorspace::Srgb.cg_name(), "kCGColorSpaceSRGB");
        assert_eq!(
            SurfaceColorspace::DisplayP3.cg_name(),
            "kCGColorSpaceDisplayP3"
        );
        assert_eq!(
            SurfaceColorspace::ExtendedLinearSrgb.cg_name(),
            "kCGColorSpaceExtendedLinearSRGB"
        );
    }

    /// An HDR (f16) surface is always tagged extended-linear (its pixels are linear light); an
    /// SDR surface takes the config verbatim.
    #[test]
    fn hdr_surface_outranks_config_tag() {
        for cfg in [WindowColorspace::Srgb, WindowColorspace::DisplayP3] {
            assert_eq!(
                resolve_surface_colorspace(cfg, true),
                SurfaceColorspace::ExtendedLinearSrgb,
                "{cfg:?}: an f16 swapchain must be read as extended-linear"
            );
            assert_eq!(
                resolve_surface_colorspace(cfg, false),
                SurfaceColorspace::from(cfg),
                "{cfg:?}: an SDR swapchain follows the config"
            );
        }
    }

    #[test]
    fn failed_surface_tag_never_claims_the_requested_space() {
        use aterm_gpu::video_tap::CaptureColorSpace;

        assert_eq!(
            capture_space_after_surface_tag(None, CaptureColorSpace::Unknown),
            CaptureColorSpace::Unknown,
            "initial best-effort failure must make exact capture refuse"
        );
        assert_eq!(
            capture_space_after_surface_tag(None, CaptureColorSpace::Srgb),
            CaptureColorSpace::Srgb,
            "failed hot retag leaves the prior compositor tag in effect"
        );
        assert_eq!(
            capture_space_after_surface_tag(
                Some(SurfaceColorspace::DisplayP3),
                CaptureColorSpace::Srgb,
            ),
            CaptureColorSpace::DisplayP3
        );
    }
}

/// Opt this application out of macOS diacritic press-and-hold, so a held key repeats instead
/// of opening the accent palette.
///
/// aterm opts every window into IME (`set_ime_allowed(true)`), so winit routes `keyDown:`
/// through `interpretKeyEvents` on an `NSTextInputClient` view, which macOS reads as wanting
/// press-and-hold: holding a diacritic-capable letter stops the autorepeat stream after ~500 ms,
/// and those repeats never reach `App::on_key`.
///
/// `registerDefaults:`, not an `Info.plist` key: `NSUserDefaults` does not read the bundle
/// plist, and the un-bundled `aterm` binary has none. It registers a default, so
/// `defaults write com.aterm.aterm ApplePressAndHoldEnabled -bool true` still wins. Must run
/// before the first window is created.
#[cfg(target_os = "macos")]
pub(crate) fn disable_press_and_hold() {
    use aterm_objc::{Bool, Id, Sel, autoreleasepool, class, sel};

    use crate::appkit;

    // SAFETY: standard NSUserDefaults registration. `+numberWithBool:` (`-(id)(BOOL)`) and
    // `+dictionaryWithObject:forKey:` (`-(id)(id, id)`) return autoreleased objects borrowed
    // inside this pool; `+standardUserDefaults` is the process singleton, and
    // `-registerDefaults:` copies the dictionary, so nothing has to outlive the pool.
    autoreleasepool(|_| unsafe {
        let Some(key) = appkit::nsstring("ApplePressAndHoldEnabled") else {
            return;
        };
        let boxed: unsafe extern "C-unwind" fn(Id, Sel, Bool) -> Id = aterm_objc::checked_msg();
        let value = boxed(class(c"NSNumber").as_id(), sel!(numberWithBool:), Bool::NO);
        if value.is_null() {
            return;
        }
        let dict: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Id = aterm_objc::checked_msg();
        let defaults = dict(
            class(c"NSDictionary").as_id(),
            sel!(dictionaryWithObject:forKey:),
            value,
            key.id(),
        );
        if defaults.is_null() {
            return;
        }
        let std_defaults =
            appkit::send_id(class(c"NSUserDefaults").as_id(), sel!(standardUserDefaults));
        if std_defaults.is_null() {
            return;
        }
        appkit::send_v_id(std_defaults, sel!(registerDefaults:), defaults);
    });
}

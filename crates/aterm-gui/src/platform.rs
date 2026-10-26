// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Platform adapter for window chrome, menus, toolbar state, notifications and appearance preferences.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use aterm_window::event_loop::EventLoopProxy;
use aterm_window::window::Window;

use crate::app_config::WindowTheme;
use crate::notify::NotifyMsg;
use crate::{Wake, WindowId, menu, toolbar};

/// One complete toolbar tab-strip model, borrowed for a single [`AppRt::set_toolbar_tabs`] push.
#[derive(Clone, Copy)]
pub(crate) struct ToolbarTabsModel<'a> {
    pub(crate) titles: &'a [String],
    pub(crate) ids: &'a [crate::tab_model::TabId],
    pub(crate) metadata: &'a [crate::tab_bar::TabStripMetadata],
    pub(crate) tooltips: &'a [Option<String>],
    pub(crate) ext: &'a [crate::session_chrome::TabChromeExt],
    pub(crate) active: usize,
    pub(crate) pill: Option<&'a crate::app_presence::NeedsPillModel>,
}

/// One editing command a menu key equivalent must hand to the inline rename field instead of to the terminal while an editor is open (see [`AppRt::rename_editor_edit`]).
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
pub(crate) trait AppRt {
    /// Paint the OS window background the terminal's theme background colour (`bg`, as `0x00RRGGBB`) so the transparent titlebar / bare compact bar reads as a seamless extension of the terminal body (Windows: the class brush the client area erases to; X11: the background the server paints until a frame covers it).
    fn window_set_background_color(&self, window: &Window, bg: u32);

    /// Apply the window-chrome appearance.
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme);

    /// Synchronously paint the window's themed backdrop, without waiting for the event loop to pump a paint message.
    #[cfg(windows)]
    fn window_flush_backdrop(&self, _window: &Window) {}

    /// Tag the window's GPU swapchain layer (the CAMetalLayer wgpu attaches at surface creation) with an explicit colour space, so ColorSync interprets the sRGB-encoded bytes instead of stretching them to the panel's P3 primaries.
    fn window_set_surface_colorspace(
        &self,
        window: &Window,
        cs: SurfaceColorspace,
    ) -> Option<SurfaceColorspace>;

    /// Stop the compositor rescaling the last presented frame during a live drag.
    fn window_anchor_surface_top_left(&self, window: &Window);

    /// Read back the live presentation state of the window's GPU swapchain layer.
    fn window_surface_presentation(&self, window: &Window) -> Option<(String, f64, bool)>;

    /// A frame of `window` was just presented (its swap or its image put returned).
    fn window_frame_presented(&self, _window: &Window) {}

    /// Install / update / remove the window-level `NSVisualEffectView` blurred backdrop and flip the window and CAMetalLayer to non-opaque so the GPU's translucent (PostMultiplied) present composites over it.
    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: crate::app_config::BackgroundMaterial,
        translucent: bool,
        bg: u32,
    );

    /// The EDR maximum of the screen `window` occupies (`NSScreen.maximumExtendedDynamicRangeColorComponentValue`: 1.0 on SDR, ~1.6 on a MacBook HDR panel, up to 16 on a Pro Display XDR).
    fn screen_edr_max(&self, window: &Window) -> f32;

    /// Extend the window's content view under the titlebar (`NSWindowStyleMaskFullSizeContentView`) so the GPU surface spans the full frame, giving the effects layer a band above the grid.
    fn window_set_fullsize_content(&self, _window: &Window) {}

    /// The titlebar band height in logical points, the chrome headroom the effects layer may draw into.
    fn titlebar_band_pts(&self, _window: &Window) -> f64 {
        0.0
    }

    /// The usable placement area of `window`'s screen.
    fn window_work_area_pts(&self, _window: &Window) -> Option<crate::app_window::WorkAreaPts> {
        None
    }

    /// The EDR POTENTIAL of the screen `window` occupies.
    fn screen_edr_potential(&self, _window: &Window) -> f32 {
        1.0
    }

    /// Windows scRGB: the display's reference-white scale (`SDR-white / 80`) for the EDR present.
    fn screen_sdr_white_scale(&self, _window: &Window) -> f32 {
        1.0
    }

    /// Spawn the process-wide notification delivery thread and return the bounded `SyncSender` each tab clones into its engine callbacks. macOS, Windows and Linux deliver.
    fn send_notification_init(
        &self,
        suppress: Arc<Mutex<HashSet<u64>>>,
        silent: Arc<AtomicBool>,
        own_alerts: Arc<AtomicBool>,
        center: crate::notice_center::NoticeCenter,
    ) -> SyncSender<NotifyMsg>;

    /// Build + install the native application menu bar, returning the retained action target the caller keeps alive.
    fn install_menu(&self, proxy: &EventLoopProxy<Wake>) -> Option<menu::MenuHandle>;

    /// Create the menu-bar operator status item (`status_item.rs`), rendered from `glance`, returning the handle the caller keeps for the process lifetime.
    fn install_status_item(
        &self,
        _proxy: &EventLoopProxy<Wake>,
        _glance: &crate::status_item::Glance,
    ) -> Option<crate::status_item::StatusItemHandle> {
        None
    }

    /// Bring another aterm instance's windows to the front by pid (the status menu's sibling rows). macOS only.
    #[cfg(target_os = "macos")]
    fn activate_instance(&self, _pid: u32) -> bool {
        false
    }

    /// Open `url` with the system's handler.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    fn open_url(&self, _url: &str) -> bool {
        false
    }

    /// Put `text` on the system pasteboard (the status item's Copy "`ssh <host>`").
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    fn pasteboard_set(&self, text: &str) -> bool {
        crate::control::pbcopy(text)
    }

    /// Un-minimize and raise our own `window`.
    fn window_bring_to_front(&self, _window: &Window) -> bool {
        false
    }

    /// Install the per-window native toolbar (the full-width tab strip + "+" button) for logical window `wid`, returning the retained backing handle.
    fn install_toolbar(
        &self,
        window: &Window,
        proxy: &EventLoopProxy<Wake>,
        wid: WindowId,
    ) -> Option<toolbar::ToolbarHandle>;

    /// Re-sync a window's native toolbar tab strip to `tabs`.
    fn set_toolbar_tabs(&self, handle: &toolbar::ToolbarHandle, tabs: ToolbarTabsModel<'_>);

    /// Push a window's needs pill alone (its age moved and nothing else), laid out where the last strip push put it, no tab chip touched.
    fn set_toolbar_pill(
        &self,
        handle: &toolbar::ToolbarHandle,
        pill: Option<&crate::app_presence::NeedsPillModel>,
    ) -> bool {
        let _ = (handle, pill);
        true
    }

    /// Re-flow a window's native tab strip to its current width from the state its chips hold (no titles, no terminal reads), on every window resize.
    fn reflow_toolbar_tabs(&self, _handle: &toolbar::ToolbarHandle) {}

    /// Whether this platform could present the native rename editor, without presenting it (`begin_tab_rename` installs the field as a side effect).
    fn can_present_tab_rename(&self, handle: &toolbar::ToolbarHandle) -> bool {
        let _ = handle;
        false
    }

    /// Open the inline session-rename editor over the strip's `tab` chip, seeded with `seed` (the current pin, empty when unpinned) and placeheld with `placeholder` (the label the ladder falls back to).
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

    /// Remove the inline rename editor and hand first responder back to the terminal view.
    fn end_tab_rename(&self, handle: &toolbar::ToolbarHandle) {
        let _ = handle;
    }

    /// The live rename editor's current text, or `None` when none is open.
    fn rename_editor_text(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        let _ = handle;
        None
    }

    /// Forward one editing command to the live rename field's field editor instead of to the terminal.
    fn rename_editor_edit(
        &self,
        handle: &toolbar::ToolbarHandle,
        action: RenameEditorEdit,
    ) -> bool {
        let _ = (handle, action);
        false
    }

    /// Read native title chrome as one introspection line (titles, selection, independent status, full tooltips), or `None` only for an empty tab set. macOS reads the retained native tab views.
    fn read_toolbar_chrome(&self, handle: &toolbar::ToolbarHandle) -> Option<String>;

    /// The strip's per-tab context-menu models as `chrome` introspection lines (`tab-menu tab=<i> items=[...]`, one per chip, none at ≤1 tab), read off the same stored models a right-click pops and serialised by `session_chrome::tab_menu_chrome_line`.
    fn read_toolbar_tab_menus(&self, handle: &toolbar::ToolbarHandle) -> Vec<String> {
        toolbar::read_tab_menus(handle)
    }

    /// Show a native modal confirmation for a destructive quit/close gesture and block until the user answers.
    fn confirm(&self, title: &str, body: &str, proceed_label: &str) -> Option<bool>;

    /// [`Self::confirm`] for an owner gesture that ends something, FAIL CLOSED (Stop Worker's sheet): its default button is Cancel and the destructive button takes only a click, so a Return the person was typing elsewhere never says yes.
    fn confirm_owner(&self, _title: &str, _body: &str, _proceed_label: &str) -> Option<bool> {
        None
    }

    /// Register the whole-app quit confirmation for AppKit `terminate:` gestures that bypass the application menu's Quit item: the Dock-icon "Quit", AppleScript `quit`, and logout/restart/shutdown. macOS wires winit's synchronous `applicationShouldTerminate:` to a typed event-loop deferral, where `App` runs the same dialog and document-durability barrier as ⌘Q.
    fn install_quit_confirm(&self);

    /// Query the OS motion preference. macOS reads `NSWorkspace.accessibilityDisplayShouldReduceMotion`.
    fn reduce_motion(&self) -> bool;

    /// Snapshot the OS-native display accessibility preferences consumed by the semantic native-app appearance system.
    fn native_appearance_preferences(&self) -> crate::native_appearance::AppearancePreferences {
        crate::native_appearance::AppearancePreferences::default()
    }

    /// Subscribe to the OS reduce-motion change notification (macOS: `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification` on the workspace centre), posting [`Wake::ReduceMotionChanged`] so the main thread re-queries [`Self::reduce_motion`] and repaints.
    fn observe_reduce_motion(&self, proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver>;

    /// Hold App Nap off for the rest of the process's life; the activity ends when the returned
    /// token is released.
    fn hold_off_app_nap(&self) -> Option<AppNapActivity> {
        None
    }
}

/// The colour space a GPU window's CAMetalLayer is tagged with at surface attach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceColorspace {
    /// The honest tag for aterm's sRGB-encoded bytes (config default).
    Srgb,
    /// The legacy stretched interpretation (bytes read as P3 coordinates).
    DisplayP3,
    /// The EDR tag for an `Rgba16Float` swapchain: linear-light sRGB primaries with >1.0 headroom.
    ExtendedLinearSrgb,
}

impl SurfaceColorspace {
    /// The CoreGraphics named-colour-space identifier for this tag, as accepted by `CGColorSpaceCreateWithName`.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const fn cg_name(self) -> &'static str {
        match self {
            Self::Srgb => "kCGColorSpaceSRGB",
            Self::DisplayP3 => "kCGColorSpaceDisplayP3",
            Self::ExtendedLinearSrgb => "kCGColorSpaceExtendedLinearSRGB",
        }
    }

    /// The colour coordinates actually consumed by the platform compositor, frozen into GPU capture metadata after the platform confirms the tag.
    pub(crate) const fn gpu_capture_space(self) -> aterm_gpu::video_tap::CaptureColorSpace {
        match self {
            Self::ExtendedLinearSrgb => aterm_gpu::video_tap::CaptureColorSpace::ExtendedLinearSrgb,
            Self::DisplayP3 => aterm_gpu::video_tap::CaptureColorSpace::DisplayP3,
            Self::Srgb => aterm_gpu::video_tap::CaptureColorSpace::Srgb,
        }
    }
}

/// Resolve capture metadata from a platform tagging attempt.
#[must_use]
pub(crate) fn capture_space_after_surface_tag(
    effective: Option<SurfaceColorspace>,
    previous: aterm_gpu::video_tap::CaptureColorSpace,
) -> aterm_gpu::video_tap::CaptureColorSpace {
    effective.map_or(previous, SurfaceColorspace::gpu_capture_space)
}

/// The layer tag for a just-attached (or re-tagged) GPU window.
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
    fn from(cs: crate::app_config::WindowColorspace) -> Self {
        match cs {
            crate::app_config::WindowColorspace::Srgb => Self::Srgb,
            crate::app_config::WindowColorspace::DisplayP3 => Self::DisplayP3,
        }
    }
}

/// This window's live AppKit content `NSView`, or nil off the main thread.
#[cfg(target_os = "macos")]
pub(crate) fn ns_view_of(window: &Window) -> aterm_objc::Id {
    aterm_objc::HasNsView::ns_view(window)
}

/// This window's AppKit `NSWindow`, or nil.
#[cfg(target_os = "macos")]
pub(crate) fn ns_window_of(window: &Window) -> aterm_objc::Id {
    let view = ns_view_of(window);
    if view.is_null() {
        return aterm_objc::Id::NIL;
    }
    // SAFETY: `view` is this window's live NSView (winit owns it for the window's lifetime), borrowed on the main thread as AppKit requires; `-window` is `-(id)`, a side-effect-free accessor returning a BORROWED reference that the view keeps alive.
    unsafe { crate::appkit::send_id(view, aterm_objc::sel!(window)) }
}

/// macOS application-runtime: WRAPS the existing AppKit integration exactly.
#[cfg(target_os = "macos")]
pub(crate) struct AppRtMacOS {
    pub(crate) probe_preferences: bool,
}

#[cfg(target_os = "macos")]
impl AppRt for AppRtMacOS {
    fn can_present_tab_rename(&self, handle: &toolbar::ToolbarHandle) -> bool {
        toolbar::can_present_tab_rename(handle)
    }

    fn window_set_background_color(&self, window: &Window, bg: u32) {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return;
        }
        let r = f64::from((bg >> 16) & 0xff) / 255.0;
        let g = f64::from((bg >> 8) & 0xff) / 255.0;
        let b = f64::from(bg & 0xff) / 255.0;
        // SAFETY: `+colorWithSRGBRed:green:blue:alpha:` is `-(id)(CGFloat, CGFloat, CGFloat, CGFloat)` and returns an AUTORELEASED colour — borrowed for the length of `-setBackgroundColor:`, which is `-(void)(NSColor *)` and RETAINS it. The pool is the caller's (the winit event loop's), which is why the borrow is sound.
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

    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        use aterm_objc::sel;

        use crate::appkit;
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return;
        }
        // SAFETY: `+deviceRGBColorSpace` is `-(id)`, a shared immortal singleton; `-setColorSpace:` is `-(void)(NSColorSpace *)` and retains it.
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
        // SAFETY: `appearanceNamed:`/`setAppearance:`/`setTitlebarAppearsTransparent:` are standard NSWindow/NSAppearance calls on the main thread; the appearance object is autoreleased and used immediately within this pool.
        let appearance_name: Option<&str> = match theme {
            WindowTheme::Auto => None,
            WindowTheme::Light => Some("NSAppearanceNameAqua"),
            WindowTheme::Dark => Some("NSAppearanceNameDarkAqua"),
        };
        // SAFETY: `+appearanceNamed:` is `-(id)(NSString *)` and returns an autoreleased, framework-owned appearance (nil for a name this OS does not know, checked); `-setAppearance:` is `-(void)(id)` and accepts nil (the Auto path); `-setTitlebarAppearsTransparent:` is `-(void)(BOOL)`.
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
                appkit::send_v_id(ns_window, sel!(setAppearance:), aterm_objc::Id::NIL);
            }
            appkit::send_v_bool(ns_window, sel!(setTitlebarAppearsTransparent:), true);
        }
    }

    fn window_set_surface_colorspace(
        &self,
        window: &Window,
        cs: SurfaceColorspace,
    ) -> Option<SurfaceColorspace> {
        layer_colorspace::set(window, cs.cg_name()).then_some(cs)
    }

    fn window_anchor_surface_top_left(&self, window: &Window) {
        layer_colorspace::anchor_contents_top_left(window);
    }

    fn window_surface_presentation(&self, window: &Window) -> Option<(String, f64, bool)> {
        layer_colorspace::read_contents_presentation(window)
    }

    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: crate::app_config::BackgroundMaterial,
        translucent: bool,
        bg: u32,
    ) {
        vibrancy::set(window, material, translucent, bg);
    }

    fn screen_edr_max(&self, window: &Window) -> f32 {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return 1.0;
        }
        // SAFETY: `-screen` is `-(id)` (nil for an off-screen window, checked) and the EDR getter is `-(CGFloat)`, both side-effect-free reads.
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

    fn screen_edr_potential(&self, window: &Window) -> f32 {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return 1.0;
        }
        // SAFETY: as `screen_edr_max`: `-screen` is a nil-checked `-(id)` read and the potential getter (macOS 10.15+, the app requires 11) a side-effect-free `-(CGFloat)`.
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

    fn window_set_fullsize_content(&self, window: &Window) {
        if aterm_types::dev_seam!("ATERM_NO_FULLSIZE_CONTENT").is_some() {
            return;
        }
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return;
        }
        // SAFETY: the style-mask read-modify-write is a standard NSWindow property pair, `-(NSWindowStyleMask)` and `-(void)(NSWindowStyleMask)`, where `NSWindowStyleMask` is `NSUInteger`.
        unsafe {
            let mask = crate::appkit::send_usize(ns_window, aterm_objc::sel!(styleMask));
            crate::appkit::send_v_usize(
                ns_window,
                aterm_objc::sel!(setStyleMask:),
                mask | (1usize << 15),
            );
        }
    }

    fn titlebar_band_pts(&self, window: &Window) -> f64 {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return 0.0;
        }
        // SAFETY: every message below is a side-effect-free geometry read. `-bounds` and `-contentLayoutRect` return `NSRect`, a 32-byte struct, which `msg` sends through `objc_msgSend_stret` on the x86_64 slice.
        unsafe {
            let content_view = crate::appkit::send_id(ns_window, aterm_objc::sel!(contentView));
            if content_view.is_null() {
                return 0.0;
            }
            let bounds = crate::appkit::send_rect(content_view, aterm_objc::sel!(bounds));
            let layout = crate::appkit::send_rect(ns_window, aterm_objc::sel!(contentLayoutRect));
            (bounds.size.height - layout.size.height).max(0.0)
        }
    }

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
        // SAFETY: every message below is a side-effect-free geometry read, and the CoreGraphics pair is a flat display-bounds getter that neither allocates nor dispatches.
        unsafe {
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
        const ACTIVATE_ALL_WINDOWS: usize = 1 << 0;
        const ACTIVATE_IGNORING_OTHER_APPS: usize = 1 << 1;
        if pid == 0 || pid == std::process::id() {
            return false;
        }
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: `+runningApplicationWithProcessIdentifier:` is `-(id)(pid_t)` (`pid_t` is `i32`, not `NSInteger`, hence the written-out prototype) and returns nil for a dead pid; `-activateWithOptions:` is `-(BOOL)(NSApplicationActivationOptions)`, an `NSUInteger` bitmask.
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

/// Map aterm's config [`WindowTheme`] onto the winit window-theme override [`aterm_window::window::Theme`] applied via [`Window::set_theme`].
#[cfg(not(target_os = "macos"))]
#[must_use]
pub(crate) fn window_theme_to_winit(theme: WindowTheme) -> Option<aterm_window::window::Theme> {
    match theme {
        WindowTheme::Auto => None,
        WindowTheme::Light => Some(aterm_window::window::Theme::Light),
        WindowTheme::Dark => Some(aterm_window::window::Theme::Dark),
    }
}

/// Proof that this process launched WINDOWED.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Windowed(());

#[cfg(target_os = "macos")]
static WINDOWED_LAUNCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "macos")]
impl Windowed {
    /// Mint the proof at a windowed launch, before its AppKit event loop is built.
    pub(crate) fn launch() -> Self {
        WINDOWED_LAUNCH.store(true, std::sync::atomic::Ordering::Release);
        Self(())
    }

    /// The proof, if this process launched windowed.
    pub(crate) fn get() -> Option<Self> {
        WINDOWED_LAUNCH
            .load(std::sync::atomic::Ordering::Acquire)
            .then_some(Self(()))
    }
}

/// The modifier state winit last DERIVED FROM AN EVENT for `window`.
#[cfg(target_os = "macos")]
pub(crate) fn cached_modifiers(
    window: &aterm_window::window::Window,
) -> aterm_window::keyboard::ModifiersState {
    use aterm_window::platform::macos::WindowExtMacOS as _;
    window.cached_modifiers()
}

/// The window setting for an Option that is an Alt (`alt`, [`crate::App::option_is_alt`]).
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) const fn option_as_alt_for(alt: bool) -> aterm_window::platform::macos::OptionAsAlt {
    use aterm_window::platform::macos::OptionAsAlt;
    if alt {
        OptionAsAlt::Both
    } else {
        OptionAsAlt::None
    }
}

/// Make `window`'s Option keys an Alt (`alt`) or let them compose.
pub(crate) fn set_option_as_alt(window: &aterm_window::window::Window, alt: bool) {
    #[cfg(target_os = "macos")]
    {
        use aterm_window::platform::macos::WindowExtMacOS as _;
        window.set_option_as_alt(option_as_alt_for(alt));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (window, alt);
}

/// Where an Option chord that Meta mode would send as ESC and the key types a character instead.
pub(crate) type OptionOnly = fn(aterm_window::keyboard::PhysicalKey, bool) -> Option<char>;

/// The [`OptionOnly`] source an `App` holds.
pub(crate) fn option_only_source(headless: bool) -> OptionOnly {
    #[cfg(target_os = "macos")]
    if !headless {
        return aterm_window::platform::macos::option_only_character;
    }
    let _ = headless;
    no_option_only
}

/// [`OptionOnly`] for HEADLESS instances and unit tests: never.
#[must_use]
pub(crate) const fn no_option_only(
    _key: aterm_window::keyboard::PhysicalKey,
    _shift: bool,
) -> Option<char> {
    None
}

/// Whether an Option chord's key-event text is what Option composed rather than the layout's character at the Shift level.
#[must_use]
pub(crate) const fn option_composes(option_is_alt: bool) -> bool {
    cfg!(target_os = "macos") && !option_is_alt
}

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

/// How long the NSEvent currently being dispatched sat in the NSApp queue before this code ran, in nanoseconds.
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

    let _windowed = Windowed::get()?;
    let _main_thread = MainThread::new()?;
    // SAFETY: `+sharedApplication` is `-(id)`, the main-thread AppKit singleton (`_main_thread` is the witness); `-currentEvent` is `-(id)` and nil outside a dispatch, which is the fail-closed case; `-timestamp` and `-systemUptime` are `-(NSTimeInterval)`, i.e. `double`, side-effect-free scalar reads; `+processInfo` is `-(id)`, another immortal singleton.
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
        return None;
    }
    let age_s = uptime - ts;
    if !(-0.001..=2.0).contains(&age_s) {
        return None;
    }
    Some((age_s.max(0.0) * 1e9) as u64)
}

/// This window's `-[NSWindow windowNumber]`, or `0` with no AppKit window.
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

/// Non-macOS peer of the macOS reader.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub(crate) fn appkit_window_number(_window: &Window) -> i64 {
    0
}

/// Whether this window is the key window of the active application.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn appkit_window_is_key_in_active_app(window: &Window) -> bool {
    let Some(_windowed) = Windowed::get() else {
        return false;
    };
    let ns_window = ns_window_of(window);
    if ns_window.is_null() {
        return false;
    }
    // SAFETY: `+sharedApplication` is `@16#0:8` (the windowed launch created it before any window existed); `-isActive` and `-isKeyWindow` are `B16@0:8` plain getters on live receivers. Main thread, as the caller's wake runs.
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

#[cfg(target_os = "macos")]
fn recent_user_input_event(within: std::time::Duration) -> bool {
    match hid_idle::seconds_since_last_input() {
        Some(age_seconds) => user_input_age_is_recent(age_seconds, within.as_secs_f64()),
        None => true,
    }
}

#[cfg(not(target_os = "macos"))]
fn recent_user_input_event(_within: std::time::Duration) -> bool {
    false
}

/// The input-activity source an `App` holds.
pub(crate) fn user_input_source(headless: bool) -> fn(std::time::Duration) -> bool {
    if headless {
        no_recent_user_input_event
    } else {
        recent_user_input_event
    }
}

/// The input-activity source for headless instances and unit tests: never recent.
#[must_use]
pub(crate) const fn no_recent_user_input_event(_within: std::time::Duration) -> bool {
    false
}

/// Whether this login's screen is out of a person's reach: locked
/// (`CGSessionCopyCurrentDictionary`'s `CGSSessionScreenIsLocked`, which the session carries
/// only while it is), or switched away for another login at this Mac (its
/// `kCGSessionOnConsoleKey` false: fast user switching, whose keyboard and mouse the HID idle
/// reading counts as input too). `false` when the session cannot be read.
#[cfg(target_os = "macos")]
fn screen_is_away() -> bool {
    screen_lock::away()
}

#[cfg(not(target_os = "macos"))]
fn screen_is_away() -> bool {
    false
}

/// The screen-away source an `App` holds: a headless instance never reaches the window server.
pub(crate) fn screen_away_source(headless: bool) -> fn() -> bool {
    if headless {
        screen_never_away
    } else {
        screen_is_away
    }
}

/// The screen-away source for headless instances and unit tests: never away.
#[must_use]
pub(crate) const fn screen_never_away() -> bool {
    false
}

#[cfg(target_os = "macos")]
mod screen_lock {
    use std::ffi::{c_char, c_void};
    use std::sync::OnceLock;

    type CfTypeRef = *const c_void;
    type CfTypeId = usize;

    const SCREEN_IS_LOCKED_KEY: &[u8] = b"CGSSessionScreenIsLocked\0";
    /// `CGSession.h`'s `kCGSessionOnConsoleKey`.
    const ON_CONSOLE_KEY: &[u8] = b"kCGSSessionOnConsoleKey\0";
    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> CfTypeRef;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: CfTypeRef,
            c_str: *const c_char,
            encoding: u32,
        ) -> CfTypeRef;
        fn CFDictionaryGetValue(dict: CfTypeRef, key: CfTypeRef) -> CfTypeRef;
        fn CFGetTypeID(cf: CfTypeRef) -> CfTypeId;
        fn CFBooleanGetTypeID() -> CfTypeId;
        fn CFBooleanGetValue(boolean: CfTypeRef) -> u8;
        fn CFRelease(cf: CfTypeRef);
    }

    /// The two session keys, made once for the process.
    struct Keys {
        locked: CfTypeRef,
        on_console: CfTypeRef,
    }
    // SAFETY: immutable CFStrings kept for the process, which CF documents as usable from any thread; nothing here is dereferenced by Rust.
    unsafe impl Send for Keys {}
    // SAFETY: as for `Send`: the strings are immutable and never released.
    unsafe impl Sync for Keys {}

    fn cf_string(text: &'static [u8]) -> CfTypeRef {
        // SAFETY: `text` is a NUL-terminated UTF-8 literal; the Create rule's +1 goes to the caller.
        unsafe {
            CFStringCreateWithCString(
                std::ptr::null(),
                text.as_ptr().cast::<c_char>(),
                CF_STRING_ENCODING_UTF8,
            )
        }
    }

    fn keys() -> Option<&'static Keys> {
        static KEYS: OnceLock<Option<Keys>> = OnceLock::new();
        KEYS.get_or_init(|| {
            let locked = cf_string(SCREEN_IS_LOCKED_KEY);
            let on_console = cf_string(ON_CONSOLE_KEY);
            if locked.is_null() || on_console.is_null() {
                for key in [locked, on_console] {
                    if !key.is_null() {
                        // SAFETY: a +1 string made just above that nothing else holds.
                        unsafe { CFRelease(key) };
                    }
                }
                return None;
            }
            // The Create rule's +1 on each is the process-wide cache, never released.
            Some(Keys { locked, on_console })
        })
        .as_ref()
    }

    /// Whether the session dictionary names its screen locked or its session off the console;
    /// `false` when there is none.
    #[must_use]
    pub(super) fn away() -> bool {
        let Some(keys) = keys() else {
            return false;
        };
        // SAFETY: the Copy rule gives +1 on the session dictionary, released on every path after use; `CFDictionaryGetValue` borrows (Get rule) values that live as long as the dictionary, and each one's type is checked before `CFBooleanGetValue` reads it.
        unsafe {
            let session = CGSessionCopyCurrentDictionary();
            if session.is_null() {
                return false;
            }
            let boolean = CFBooleanGetTypeID();
            let locked = CFDictionaryGetValue(session, keys.locked);
            let locked = !locked.is_null()
                && (CFGetTypeID(locked) != boolean || CFBooleanGetValue(locked) != 0);
            let on_console = CFDictionaryGetValue(session, keys.on_console);
            let switched_away = !on_console.is_null()
                && CFGetTypeID(on_console) == boolean
                && CFBooleanGetValue(on_console) == 0;
            CFRelease(session);
            locked || switched_away
        }
    }
}

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

    const IO_MAIN_PORT_DEFAULT: MachPort = 0;
    const IOHID_SYSTEM_CLASS: &[u8] = b"IOHIDSystem\0";
    const HID_IDLE_TIME_KEY: &[u8] = b"HIDIdleTime\0";
    const CF_STRING_ENCODING_UTF8: CfStringEncoding = 0x0800_0100;
    const CF_NUMBER_SINT64_TYPE: CfIndex = 4;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: MachPort, matching: *mut c_void) -> IoService;
        fn IOObjectRelease(object: IoObject) -> i32;
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

    struct Handles {
        service: IoService,
        key: CfStringRef,
    }
    // SAFETY: both fields are process-wide handles that IOKit/CF document as usable from any thread; nothing here is dereferenced by Rust.
    unsafe impl Send for Handles {}
    unsafe impl Sync for Handles {}

    fn handles() -> Option<&'static Handles> {
        static HANDLES: OnceLock<Option<Handles>> = OnceLock::new();
        HANDLES
            .get_or_init(|| {
                // SAFETY: `IOServiceMatching` takes a NUL-terminated class name and returns a dictionary `IOServiceGetMatchingService` consumes. The service and the `CFStringCreateWithCString` key (a NUL-terminated UTF-8 literal) are the cache, kept for the process; the service is released only if the key cannot be made.
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

    /// Seconds since the last HID event, `None` when the registry cannot be read (no `IOHIDSystem`, no property, or not a number).
    #[must_use]
    pub(super) fn seconds_since_last_input() -> Option<f64> {
        let h = handles()?;
        // SAFETY: `h.service` is a live registry entry and `h.key` a live CFString (both process-cached); the Create rule gives us +1 on the returned object, which we release on every path after use. The type is checked before `CFNumberGetValue` writes into the `i64`.
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
            ok.then(|| idle_ns as f64 / 1e9)
        }
    }
}

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

/// The X11 twin of the macOS queue age.
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

#[cfg(any(target_os = "linux", test))]
fn x_server_event_age_ns(server_ms: u32, now_ns: u64, max_age_ms: u32) -> Option<u64> {
    const FUTURE_SLACK_MS: u32 = 2;
    if server_ms == 0 {
        return None;
    }
    let now_ms = now_ns / 1_000_000;
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
        assert_eq!(
            x_server_event_age_ns(1_000, 1_004 * MS + MS / 4, LOCAL),
            Some(4 * MS + MS / 4)
        );
        assert_eq!(
            x_server_event_age_ns(1_000, 1_000 * MS + 600_000, LOCAL),
            Some(600_000)
        );
    }

    #[test]
    fn the_32_bit_server_clock_wraps_about_every_49_days() {
        let now_ns = ((1_u64 << 32) + 3) * MS;
        assert_eq!(
            x_server_event_age_ns(u32::MAX - 1, now_ns, LOCAL),
            Some(5 * MS)
        );
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
        assert_eq!(x_server_event_age_ns(100_000 - 60_001, now_ns, LOCAL), None);
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
        assert_eq!(x_server_event_age_ns(500_000, 1_000 * MS, FOREIGN), None);
        assert_eq!(x_server_event_age_ns(500_000, 1_000 * MS, LOCAL), None);
    }

    #[test]
    fn current_time_is_not_a_stamp() {
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

        assert_eq!(
            dispatch_with_key_event_server_time(0, key_event_server_time),
            Some(0)
        );
        assert_eq!(
            dispatch_with_key_event_server_time(0, current_event_queue_age_ns),
            None
        );

        let unwound = std::panic::catch_unwind(|| {
            dispatch_with_key_event_server_time(stamp, || panic!("a key handler panicked"))
        });
        assert!(unwound.is_err());
        assert_eq!(key_event_server_time(), None);
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
pub(crate) fn current_event_queue_age_ns() -> Option<u64> {
    None
}

/// Whether the `RedrawRequested` being handled is one the X server asked for with an `Expose` that no whole-frame post of the window's (`present::x11`) drew again since (`aterm_window::platform::x11::redraw_is_exposure`).
#[cfg(target_os = "linux")]
pub(crate) fn redraw_is_exposure() -> bool {
    aterm_window::platform::x11::redraw_is_exposure()
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn redraw_is_exposure() -> bool {
    false
}

/// Linux/BSD application-runtime using the **pure-winit** surface (NO new system libraries): the
/// appearance method drives `aterm_window::Window::set_theme` so the client-side-decoration
/// light/dark variant honours config `window_theme`.
#[cfg(all(not(target_os = "macos"), not(windows)))]
pub(crate) struct AppRtLinux {
    pub(crate) probe_preferences: bool,
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
impl AppRt for AppRtLinux {
    fn window_set_background_color(&self, window: &Window, bg: u32) {
        #[cfg(target_os = "linux")]
        aterm_window::platform::x11::WindowExtX11::set_x11_background(window, bg);
        #[cfg(not(target_os = "linux"))]
        let _ = (window, bg);
    }

    fn window_frame_presented(&self, window: &Window) {
        #[cfg(target_os = "linux")]
        aterm_window::platform::x11::WindowExtX11::x11_frame_presented(window);
        #[cfg(not(target_os = "linux"))]
        let _ = window;
    }

    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        window.set_theme(window_theme_to_winit(theme));
    }

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

    fn window_anchor_surface_top_left(&self, _window: &Window) {}

    fn window_surface_presentation(&self, _window: &Window) -> Option<(String, f64, bool)> {
        None
    }

    fn window_set_vibrancy(
        &self,
        _window: &Window,
        _material: crate::app_config::BackgroundMaterial,
        _translucent: bool,
        _bg: u32,
    ) {
    }

    fn screen_edr_max(&self, _window: &Window) -> f32 {
        1.0
    }

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

    fn confirm(&self, _title: &str, _body: &str, _proceed_label: &str) -> Option<bool> {
        None
    }

    fn install_quit_confirm(&self) {}

    fn reduce_motion(&self) -> bool {
        self.probe_preferences && gnome_reduce_motion()
    }

    fn observe_reduce_motion(&self, _proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver> {
        None
    }
}

/// The concrete application-runtime the `App` stores, selected at compile time.
#[cfg(target_os = "macos")]
pub(crate) type PlatformAppRt = AppRtMacOS;

/// Windows: native DWM chrome (see `crate::platform_win`).
#[cfg(windows)]
pub(crate) type PlatformAppRt = crate::platform_win::AppRtWindows;

/// Every other target (Linux, the BSDs).
#[cfg(all(not(target_os = "macos"), not(windows)))]
pub(crate) type PlatformAppRt = AppRtLinux;

/// Construct the platform application-runtime for this build target.
pub(crate) const fn platform_apprt() -> PlatformAppRt {
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
    static OPENED_URLS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The URLs this thread's [`AppRt::open_url`] and `app_mouse::open_url_external` calls named, drained (test-only).
#[cfg(test)]
pub(crate) fn take_opened_urls() -> Vec<String> {
    OPENED_URLS.with(|urls| std::mem::take(&mut *urls.borrow_mut()))
}

/// Note `url` as opened on this thread ([`take_opened_urls`]).
#[cfg(test)]
pub(crate) fn record_opened_url(url: &str) {
    OPENED_URLS.with(|urls| urls.borrow_mut().push(url.to_string()));
}

/// The runtime handed to a headless test or benchmark fixture.
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
        let inert = headless_fixture_apprt(AppRtMacOS {
            probe_preferences: false,
        });
        assert!(!inert.probe_preferences);
        assert_eq!(
            inert.native_appearance_preferences(),
            AppearancePreferences::default()
        );
        assert!(!inert.reduce_motion());

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

/// The three display-accessibility reads [`AppRtMacOS`] answers [`AppRt::reduce_motion`] and [`AppRt::native_appearance_preferences`] with, asked once and dropped.
#[cfg(target_os = "macos")]
pub(crate) fn warm_accessibility_reads() {
    aterm_objc::autoreleasepool(|_| {
        let apprt = platform_apprt();
        let _ = apprt.native_appearance_preferences();
        let _ = apprt.reduce_motion();
    });
}

/// The retained reduce-motion notification observer [`AppRt::observe_reduce_motion`] returns.
#[cfg(target_os = "macos")]
pub(crate) type ReduceMotionObserver = aterm_objc::Retained<reduce_motion::ReduceMotionTarget>;

/// See the macOS variant above — nothing to retain off macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) type ReduceMotionObserver = ();

/// The App Nap opt-out's activity token [`AppRt::hold_off_app_nap`] returns.
#[cfg(target_os = "macos")]
pub(crate) type AppNapActivity = aterm_objc::Obj;

/// See the macOS variant above — nothing to hold off macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) type AppNapActivity = ();

#[cfg(target_os = "macos")]
fn begin_app_nap_opt_out() -> Option<AppNapActivity> {
    use aterm_objc::{Obj, autoreleasepool, class, sel};

    use crate::appkit::{self, consts::NS_ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP};

    const REASON: &str =
        "aterm hosts terminal sessions and the control socket agents drive them over";

    let token = autoreleasepool(|_| {
        let reason = appkit::nsstring(REASON)?;
        // SAFETY: `+processInfo` is `-(id)`, the immortal process singleton, and `-beginActivityWithOptions:reason:` is `-(id<NSObject>)(NSActivityOptions, NSString *)` (`@@:Q@`; the `uint64_t` options word is `NSUInteger`-wide on every Apple target, so `send_id_usize_id` is its exact shape). `reason` is a live +1 NSString this frame owns. The token comes back autoreleased (+0), so it is retained before the pool pops. This runs on the main thread (`App::resumed`).
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

#[cfg(target_os = "linux")]
const MOTION_READ_BOUND: std::time::Duration = std::time::Duration::from_millis(200);

#[cfg(target_os = "linux")]
fn gnome_reduce_motion() -> bool {
    use crate::portal_settings::{ENABLE_ANIMATIONS, GNOME_INTERFACE};
    crate::portal_settings::animations_disabled(
        crate::portal_settings::read(GNOME_INTERFACE, ENABLE_ANIMATIONS, MOTION_READ_BOUND)
            .as_ref(),
    )
}

#[cfg(all(not(target_os = "macos"), not(windows), not(target_os = "linux")))]
fn gnome_reduce_motion() -> bool {
    false
}

/// Run `program args…` and return its stdout if it exits successfully within `bound`.
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
    #[cfg(unix)]
    use super::read_bounded_stdout;

    #[cfg(unix)]
    #[test]
    fn the_bounded_read_returns_stdout_of_a_successful_child() {
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

#[cfg(target_os = "macos")]
mod reduce_motion {
    use aterm_objc::{class, sel};
    use aterm_window::event_loop::EventLoopProxy;

    use crate::Wake;
    use crate::appkit::{self, MainThread};

    /// The workspace notification this module observes, equal to AppKit's exported constant of the same name.
    pub(crate) const DISPLAY_OPTIONS_NOTIFICATION: &str =
        "NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification";

    /// The workspace notification for a wake from sleep.
    pub(crate) const DID_WAKE_NOTIFICATION: &str = "NSWorkspaceDidWakeNotification";

    /// The system colours changed (the accent colour among them).
    pub(crate) const SYSTEM_COLORS_NOTIFICATION: &str = "NSSystemColorsDidChangeNotification";

    /// The observer target's ivars.
    pub(crate) struct ReduceMotionIvars {
        relay: Box<dyn Fn()>,
        woke: Box<dyn Fn()>,
        colors: Box<dyn Fn()>,
    }

    aterm_objc::declare_class! {
        /// The notification-observer target.
        pub(crate) struct ReduceMotionTarget: NSObject {
            const NAME: &str = "ATermReduceMotionTarget";
            type Ivars = ReduceMotionIvars;

            @sel(reduceMotionDidChange:)
            fn reduce_motion_did_change(&self, _note: aterm_objc::Id) {
                (self.ivars().relay)();
            }

            @sel(systemDidWake:)
            fn system_did_wake(&self, _note: aterm_objc::Id) {
                (self.ivars().woke)();
            }

            @sel(systemColorsDidChange:)
            fn system_colors_did_change(&self, _note: aterm_objc::Id) {
                (self.ivars().colors)();
            }
        }
    }

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

    fn default_center() -> aterm_objc::Id {
        // SAFETY: `+defaultCenter` is `-(id)`, the standard immortal singleton accessor, only borrowed here.
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

    fn workspace_center() -> aterm_objc::Id {
        // SAFETY: `+sharedWorkspace` and `-notificationCenter` are `-(id)` accessors returning autoreleased or immortal objects, only borrowed here.
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

    fn register(target: &aterm_objc::Retained<ReduceMotionTarget>) -> bool {
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
                // SAFETY: `-addObserver:selector:name:object:` is `-(void)(id, SEL, id, id)`, exactly the prototype cast below. The observer is `target`, which the caller keeps alive; the selector is one this class declares; `name` is a live +1 NSString the pool outlives; nil `object` means "from any sender".
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
            let center = default_center();
            if let (false, Some(name)) = (
                center.is_null(),
                aterm_objc::ns_string(SYSTEM_COLORS_NOTIFICATION),
            ) {
                // SAFETY: as above: `-addObserver:selector:name:object:` on the live default centre, the kept `target`, a selector this class declares, a live +1 NSString, and a nil sender.
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

    /// Query `NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion`.
    pub(crate) fn query() -> bool {
        workspace_flag(sel!(accessibilityDisplayShouldReduceMotion))
    }

    fn workspace_flag(flag: aterm_objc::Sel) -> bool {
        // SAFETY: `+sharedWorkspace` is `-(id)`, the standard singleton accessor, and each `flag` is a side-effect-free `-(BOOL)` property read, converted once in `send_bool`.
        unsafe {
            let ws = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            if ws.is_null() {
                return false;
            }
            appkit::send_bool(ws, flag)
        }
    }

    /// Query the sibling display-accessibility values posted by the same workspace notification.
    pub(crate) fn increase_contrast() -> bool {
        workspace_flag(sel!(accessibilityDisplayShouldIncreaseContrast))
    }

    pub(crate) fn reduce_transparency() -> bool {
        workspace_flag(sel!(accessibilityDisplayShouldReduceTransparency))
    }

    pub(crate) fn differentiate_without_color() -> bool {
        workspace_flag(sel!(accessibilityDisplayShouldDifferentiateWithoutColor))
    }

    /// Register a workspace-notification observer for the accessibility display-options change and the wake from sleep, and return the retained target (the caller keeps it alive for the process life — the notification center references it weakly, and it is never removed, exactly like the menu target).
    pub(crate) fn observe(
        proxy: &EventLoopProxy<Wake>,
    ) -> Option<aterm_objc::Retained<ReduceMotionTarget>> {
        let main_thread = MainThread::new()?;
        let colors = proxy.clone();
        let proxy = proxy.clone();
        let target = new_target(
            main_thread,
            Box::new(move || {
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

        fn unregister(target: &aterm_objc::Retained<ReduceMotionTarget>) {
            for center in [super::workspace_center(), super::default_center()] {
                if center.is_null() {
                    continue;
                }
                // SAFETY: `-removeObserver:` is `-(void)(id)`; `center` is a live centre and `target` a live observer registered on it.
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

        fn post_default(notification: &str) {
            aterm_objc::autoreleasepool(|_| {
                let center = super::default_center();
                assert!(!center.is_null(), "no default notification centre");
                let name = aterm_objc::ns_string(notification).expect("NSString");
                // SAFETY: `-postNotificationName:object:` is `-(void)(id, id)`; `center` is live, `name` a live +1 NSString, nil sender is legal.
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

        fn post() {
            post_named(DISPLAY_OPTIONS_NOTIFICATION);
        }

        fn post_named(notification: &str) {
            aterm_objc::autoreleasepool(|_| {
                let center = super::workspace_center();
                assert!(!center.is_null(), "no workspace notification centre");
                let name = aterm_objc::ns_string(notification).expect("NSString");
                // SAFETY: `-postNotificationName:object:` is `-(void)(id, id)`; `center` is live, `name` a live +1 NSString, nil sender is legal.
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

            let cls = ReduceMotionTarget::class();
            assert!(!cls.is_null());
            assert_eq!(
                class(c"ATermReduceMotionTarget"),
                cls,
                "objc_getClass disagrees with the registered pair"
            );
            // SAFETY: `cls` is the class this module just registered, so it and its superclass are live, immortal class objects.
            unsafe {
                assert_eq!(class_name(cls), c"ATermReduceMotionTarget");
                assert_eq!(class_name(superclass_of(cls)), c"NSObject");
            }
            // SAFETY: `target` is a live instance; `-respondsToSelector:` is a side-effect-free query.
            unsafe {
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
            post_named(DID_WAKE_NOTIFICATION);
            post_default(super::SYSTEM_COLORS_NOTIFICATION);
            unregister(&target);
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

#[cfg(target_os = "macos")]
mod layer_colorspace {
    use std::ffi::c_void;

    use aterm_objc::{Id, Sel, class, sel};
    use aterm_window::window::Window;

    use super::ns_view_of;
    use crate::appkit;

    type CGColorSpaceRef = *mut c_void;

    // SAFETY: standard, stable CoreGraphics C entry points with the published signatures; contracts honoured at the call site below (the one Create is released exactly once, after the layer setter retained it).
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGColorSpaceCreateWithName(name: *const c_void) -> CGColorSpaceRef;
        fn CGColorSpaceRelease(space: CGColorSpaceRef);
    }

    /// # Safety
    /// `obj` must be nil or a live object.
    unsafe fn is_metal_layer(obj: Id) -> bool {
        let metal = class(c"CAMetalLayer");
        if obj.is_null() || metal.is_null() {
            return false;
        }
        // SAFETY: `-isKindOfClass:` is `-(BOOL)(Class)` and side-effect free. The argument is a Class (`@encode` `"#"`, not `"@"`), which `ClassPtr` keeps distinct; hence not `send_bool_id`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::ClassPtr) -> aterm_objc::Bool =
                aterm_objc::checked_msg();
            f(obj, sel!(isKindOfClass:), metal).as_bool()
        }
    }

    /// Find the CAMetalLayer wgpu attached to `view` and set its `colorspace` to the named CG colour space.
    #[must_use]
    pub(crate) fn set(window: &Window, cg_name: &str) -> bool {
        let view = ns_view_of(window);
        let Some(name) = appkit::nsstring(cg_name) else {
            return false;
        };
        // SAFETY: `view` is this window's live NSView, borrowed on the main thread (only the winit event loop reaches this); the layer walk reads CALayers that view retains. `CGColorSpaceCreateWithName` takes a `CFStringRef`, which the toll-free-bridged NSString is, and returns +1; `-setColorspace:` is `-(void)(CGColorSpaceRef)` (a bare pointer) and retains, so releasing our reference right after is balanced.
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

    /// Set the window's CAMetalLayer `contentsGravity` to the visual top-left.
    pub(crate) fn anchor_contents_top_left(window: &Window) {
        // SAFETY: the `CALayerContentsGravity` constants are stable exported QuartzCore symbols; these are reads of immutable `CFStringRef` globals, never freed or mutated here.
        #[link(name = "QuartzCore", kind = "framework")]
        unsafe extern "C" {
            static kCAGravityTopLeft: *const c_void;
            static kCAGravityBottomLeft: *const c_void;
        }
        let view = ns_view_of(window);
        // SAFETY: `view` is this window's live NSView, borrowed on the main thread; `-contentsAreFlipped` is a side-effect-free `-(BOOL)` read and `-setContentsGravity:` a `-(void)(NSString *)` property write of a framework constant.
        unsafe {
            let Some(layer) = find_metal_layer(view) else {
                return;
            };
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

    /// Read back the CAMetalLayer's live `contentsGravity`, `contentsScale` and `contentsAreFlipped`, the state [`anchor_contents_top_left`] establishes (see [`super::AppRt::window_surface_presentation`]).
    pub(crate) fn read_contents_presentation(window: &Window) -> Option<(String, f64, bool)> {
        let view = ns_view_of(window);
        // SAFETY: `view` is this window's live NSView (winit owns it for the window's lifetime), borrowed on the main thread as AppKit requires. Every message below is a side-effect-free property read; the returned `contentsGravity` is a borrowed NSString copied out immediately.
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

    /// Set the window's CAMetalLayer `opaque` flag.
    pub(crate) fn set_metal_opaque(window: &Window, opaque: bool) {
        let view = ns_view_of(window);
        // SAFETY: `view` is this window's live NSView (winit owns it for the window's lifetime), borrowed on the main thread as AppKit requires; `-setOpaque:` is `-(void)(BOOL)` on the CAMetalLayer.
        unsafe {
            let Some(layer) = find_metal_layer(view) else {
                return;
            };
            appkit::send_v_bool(layer, sel!(setOpaque:), opaque);
        }
    }

    /// # Safety
    /// `view` is a live NSView borrowed on the main thread.
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

#[cfg(target_os = "macos")]
mod vibrancy {
    use aterm_objc::{Id, Sel, class, sel};
    use aterm_window::window::Window;

    use super::{layer_colorspace, ns_window_of};
    use crate::app_config::BackgroundMaterial;
    use crate::appkit;

    const MATERIAL_SIDEBAR: isize = 7;
    const MATERIAL_HUD: isize = 13;
    const MATERIAL_UNDER_WINDOW: isize = 21;
    const BLENDING_BEHIND_WINDOW: isize = 0;
    const STATE_ACTIVE: isize = 1;
    const AUTORESIZE_WH: usize = 2 | 16;
    const ORDER_BELOW: isize = -1;

    const fn material_value(m: BackgroundMaterial) -> Option<isize> {
        match m {
            BackgroundMaterial::None => None,
            BackgroundMaterial::Hud => Some(MATERIAL_HUD),
            BackgroundMaterial::Sidebar => Some(MATERIAL_SIDEBAR),
            BackgroundMaterial::UnderWindow => Some(MATERIAL_UNDER_WINDOW),
        }
    }

    /// Apply the resolved vibrancy state to `window`.
    pub(crate) fn set(window: &Window, material: BackgroundMaterial, translucent: bool, bg: u32) {
        let ns_window = ns_window_of(window);
        if ns_window.is_null() {
            return;
        }
        // SAFETY: every message below is a standard AppKit call on the main thread. `-contentView` is `-(id)`; `-setOpaque:` is `-(void)(BOOL)`; `+clearColor` is `-(id)` returning a shared immortal colour and `+colorWithSRGBRed:green:blue:alpha:` an AUTORELEASED one, both borrowed only for the length of `-setBackgroundColor:`, which is `-(void)(NSColor *)` and RETAINS.
        unsafe {
            let content = appkit::send_id(ns_window, sel!(contentView));
            if content.is_null() {
                return;
            }
            remove_effect_views(content);

            if translucent {
                appkit::send_v_bool(ns_window, sel!(setOpaque:), false);
                let clear = appkit::send_id(class(c"NSColor").as_id(), sel!(clearColor));
                if !clear.is_null() {
                    appkit::send_v_id(ns_window, sel!(setBackgroundColor:), clear);
                }
                if let Some(mat) = material_value(material) {
                    install_effect_view(content, mat);
                }
                layer_colorspace::set_metal_opaque(window, false);
            } else {
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

    /// # Safety
    /// `content` is a live NSView on the main thread.
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

    /// # Safety
    /// `content` is a live NSView on the main thread.
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
                return;
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
            appkit::send_v(effect, sel!(release));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{MATERIAL_HUD, MATERIAL_SIDEBAR, MATERIAL_UNDER_WINDOW, material_value};
        use crate::app_config::BackgroundMaterial;

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
            assert_eq!(
                (MATERIAL_SIDEBAR, MATERIAL_HUD, MATERIAL_UNDER_WINDOW),
                (7, 13, 21)
            );
        }
    }
}

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

/// Opt this application out of macOS diacritic press-and-hold.
#[cfg(target_os = "macos")]
pub(crate) fn disable_press_and_hold() {
    use aterm_objc::{Bool, Id, Sel, autoreleasepool, class, sel};

    use crate::appkit;

    // SAFETY: standard NSUserDefaults registration. `+numberWithBool:` (`-(id)(BOOL)`) and `+dictionaryWithObject:forKey:` (`-(id)(id, id)`) return autoreleased objects borrowed inside this pool; `+standardUserDefaults` is the process singleton, and `-registerDefaults:` copies the dictionary, so nothing has to outlive the pool.
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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The application-runtime (`apprt`) seam: the ONE place that names every native
//! OS-integration operation aterm performs (window chrome colour/appearance, the
//! menu bar, the per-window toolbar tab strip, and the desktop-notification
//! delivery thread). It exists so `main.rs` / `app_window.rs` / `app_tabs.rs`
//! stay platform-NEUTRAL: they call through an [`AppRt`] instance and never name
//! AppKit/objc2 directly.
//!
//! Two impls back the one trait:
//!
//! * [`AppRtMacOS`] WRAPS the existing objc2 calls EXACTLY — the NSWindow
//!   colour-space + NSAppearance/titlebar logic moved here verbatim from
//!   `app_window.rs`, and the menu/toolbar/notify methods forward straight to the
//!   already-`cfg(macos)`-guarded [`crate::menu`] / [`crate::toolbar`] /
//!   [`crate::notify`] modules. So the macOS binary is behaviorally identical: the
//!   same objc2 operations run, just reached through the trait.
//! * [`AppRtLinux`] is the no-op fallback for every non-macOS target: chrome
//!   colour/appearance do nothing, the menu/toolbar install nothing (`None`), the
//!   tab-strip sync + chrome read are no-ops, and notification delivery spins the
//!   same channel-draining stub `notify::spawn_delivery` already provides. The
//!   terminal renders + input works; native chrome is gracefully absent.
//!
//! [`PlatformAppRt`] is the cfg-selected concrete type the `App` stores: the macOS
//! impl on macOS, the Linux impl everywhere else. Both are zero-sized, so the
//! field costs nothing.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use winit::event_loop::EventLoopProxy;
use winit::window::Window;

use crate::app_config::WindowTheme;
use crate::notify::NotifyMsg;
use crate::{Wake, WindowId, menu, toolbar};

/// One complete toolbar tab-strip model, borrowed for a single
/// [`AppRt::set_toolbar_tabs`] push: parallel per-tab slices paired by index
/// (title, id, strip metadata, hover tooltip, chrome extension), plus the
/// active index. Bundled so the seam stays one argument as chrome stages
/// keep adding per-tab context.
#[derive(Clone, Copy)]
pub(crate) struct ToolbarTabsModel<'a> {
    pub(crate) titles: &'a [String],
    pub(crate) ids: &'a [crate::tab_model::TabId],
    pub(crate) metadata: &'a [crate::tab_bar::TabStripMetadata],
    pub(crate) tooltips: &'a [Option<String>],
    pub(crate) ext: &'a [crate::session_chrome::TabChromeExt],
    pub(crate) active: usize,
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

/// The native application-runtime seam. Every method is a platform OS-integration
/// operation aterm performs; the macOS impl runs the real objc2 calls, the Linux
/// impl is a graceful no-op. Implementors are zero-sized.
pub(crate) trait AppRt {
    /// Paint the OS window background the terminal's theme background colour
    /// (`bg`, as `0x00RRGGBB`) so the transparent titlebar / bare compact bar reads
    /// as a seamless extension of the terminal body. No-op off macOS.
    fn window_set_background_color(&self, window: &Window, bg: u32);

    /// Apply the window-CHROME appearance: match the NSWindow colour space to
    /// softbuffer's device-RGB content (dropping the per-frame CoreAnimation gamut
    /// conversion) and set the titlebar light/dark appearance from `theme`. No-op
    /// off macOS.
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme);

    /// L1 (early reveal): SYNCHRONOUSLY paint the window's themed backdrop onto
    /// glass, without waiting for the event loop to pump a paint message.
    ///
    /// The warm-launch early reveal shows the first window and then BLOCKS the
    /// event-loop thread joining the backend build — a wait that is only long on
    /// Windows, the one platform that takes this path (the macOS join measures
    /// 0.01 ms; see `app_window.rs`'s join note). On Windows the
    /// themed erase that makes the revealed window look like the terminal (the
    /// class brush `window_set_background_color` installs) only runs when a
    /// paint/erase message is processed — which the blocked loop will not do,
    /// so without this the "revealed" window would sit as an unpainted rectangle
    /// for the whole join, which is worse than staying hidden. The Windows impl
    /// forces the erase inline via `RedrawWindow(RDW_ERASENOW)`.
    ///
    /// Default no-op: macOS/Linux do not take the early-reveal path today, and
    /// their compositors clear fresh windows to the layer/window background
    /// without a client-side erase anyway.
    #[cfg_attr(not(windows), allow(dead_code))] // only the Windows early-reveal path calls it
    fn window_flush_backdrop(&self, _window: &Window) {}

    /// M3 (colour-managed present): tag the window's GPU swapchain layer (the
    /// CAMetalLayer wgpu attached at surface creation) with an EXPLICIT colour
    /// space, so ColorSync INTERPRETS the presented sRGB-encoded bytes instead of
    /// stretching them to the panel's native (P3) primaries. Interpretation only:
    /// the application-provided swapchain bytes and readback are untouched; the
    /// resulting compositor colour transform is outside our observation boundary.
    /// Called AFTER the wgpu surface exists (the layer is created there).
    /// Returns the colour space actually in effect, or `None` when it cannot be
    /// established; capture must retain prior known metadata or refuse rather
    /// than assuming a best-effort tag succeeded.
    fn window_set_surface_colorspace(
        &self,
        window: &Window,
        cs: SurfaceColorspace,
    ) -> Option<SurfaceColorspace>;

    /// LIVE-RESIZE ANCHOR: stop the compositor from RESCALING the last presented
    /// frame while the window is being dragged.
    ///
    /// wgpu (via `raw-window-metal`) does not render into the view's own backing
    /// layer — it installs a `CAMetalLayer` as a SUBLAYER and keeps its frame in
    /// sync with the super layer through a KVO observer on `bounds`. That layer
    /// keeps CoreAnimation's default `contentsGravity` (`resize`), and
    /// `raw-window-metal` deliberately leaves it there ("masks / alleviates issues
    /// with resizing", `observer.rs`).
    ///
    /// For a terminal that default is exactly wrong. Every step of a live drag
    /// changes the layer's bounds inside AppKit's own CoreAnimation transaction,
    /// while our next present lands in a LATER one — so for the frames in between,
    /// the compositor stretches the previous drawable onto the new bounds. Text
    /// rasterized for one pixel size and then non-uniformly rescaled is the
    /// smeared, shredded look the drag shows, and it snaps back the moment a real
    /// frame arrives. Anchoring at the top-left instead makes a not-yet-repainted
    /// frame stay at 1:1 exactly where it was drawn — the content sits still and
    /// the newly exposed strip is simply uncovered until the next present fills
    /// it, which is the behaviour iTerm2 gets from owning its view's layer.
    ///
    /// The cost is confined to the frames we have not repainted yet, and it is a
    /// scale-factor change (dragging a window between a Retina and a non-Retina
    /// display) that pays it: `raw-window-metal` syncs `contentsScale` from the
    /// root layer immediately, and gravity is computed on the LOGICAL size
    /// (`physical / contentsScale`), so until the next frame lands a stale image
    /// reads at the WRONG size rather than merely soft — 2x oversized and cropped
    /// to the anchored corner going Retina→non-Retina, half-sized going the other
    /// way. That is one artifact, for one frame, on a rare event, traded against a
    /// continuous artifact on a very common one. Upstream keeps the stretching
    /// default precisely to blur over that case; a terminal would rather the text
    /// it is showing be the right size than smoothly the wrong one.
    ///
    /// This deliberately goes against `raw-window-metal`'s documented request that
    /// consumers not set `contentsGravity` on the layer it owns (its `lib.rs`
    /// "Semantics" note). The reasoning there is that layer presentation belongs to
    /// the windowing library — but here aterm IS the windowing integration, it
    /// already sets `colorspace` and `opaque` on this same layer through
    /// [`layer_colorspace`], and the property is never written back by the crate's
    /// KVO observer (which syncs only `bounds` and `contentsScale`), so the value
    /// set at attach stands for the layer's life.
    ///
    /// Applied ONCE per window, right after the wgpu surface (and therefore the
    /// layer) exists. Best-effort and a no-op off macOS.
    fn window_anchor_surface_top_left(&self, window: &Window);

    /// READ BACK the live presentation state of the window's GPU swapchain layer:
    /// `(contents_gravity, contents_scale, contents_are_flipped)`.
    ///
    /// [`AppRt::window_anchor_surface_top_left`] is the one change in the resize
    /// path that NO in-process instrument can score, because the artifact it
    /// prevents happens in the compositor, after aterm has submitted its frame —
    /// `image`/`video`/`metrics` all stop at the WSI boundary by construction. What
    /// IS observable, and what this exposes, is whether the anchor is actually in
    /// effect on the live layer.
    ///
    /// That turns an unverifiable claim into a checkable one and, more usefully,
    /// into a regression guard: the layer belongs to `raw-window-metal`, whose docs
    /// reserve the right to overwrite common `CALayer` properties, and wgpu
    /// reconfigures the surface on every resize. If either ever reverts the gravity,
    /// `dims` says so instead of the smear quietly coming back.
    ///
    /// `None` where there is no such layer (off macOS, CPU backend, no window).
    /// Read-only: it must never be the thing that establishes the state it reports.
    fn window_surface_presentation(&self, window: &Window) -> Option<(String, f64, bool)>;

    /// M5 TRUE VIBRANCY: install / update / remove the window-level
    /// `NSVisualEffectView` blurred backdrop and flip the window + CAMetalLayer to
    /// non-opaque so the GPU's translucent (PostMultiplied) present composites over
    /// it. `material` is the resolved config `background_material`; `translucent`
    /// is `background_opacity < 1.0` (`aterm_render::vibrancy::is_translucent`);
    /// `bg` (`0x00RRGGBB`) is the theme background used to RESTORE the opaque
    /// seamless-titlebar fill when translucency turns off.
    ///
    /// * `translucent && material != None` → a `behindWindow` visual-effect view
    ///   (the mapped `NSVisualEffectMaterial`) is installed behind the Metal layer,
    ///   the window is made non-opaque with a clear background, and the Metal layer
    ///   is set non-opaque — the desktop shows blurred through the terminal bg.
    /// * `translucent && material == None` → the same non-opaque flip WITHOUT a
    ///   blur view — the desktop shows through unblurred (plain translucency).
    /// * `!translucent` → any installed view is removed and the window / Metal
    ///   layer restored to opaque with the `bg` fill (the byte-identical default).
    ///
    /// Best-effort like the other chrome methods; a no-op off macOS.
    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: crate::app_config::BackgroundMaterial,
        translucent: bool,
        bg: u32,
    );

    /// M3 phase B: the EDR maximum of the screen `window` currently occupies —
    /// `NSScreen.maximumExtendedDynamicRangeColorComponentValue` (1.0 on an SDR
    /// panel; ~1.6 on a MacBook HDR panel at typical brightness; up to 16 on a
    /// Pro Display XDR). Queried at GPU attach and RE-queried on monitor
    /// changes (`refresh_frame_interval`); fed per-window into the renderer,
    /// which sanitizes it through the PROVEN `aterm_render::hdr` chain — so the
    /// off-macOS `1.0` (no headroom) makes the EDR aurora pass provably inert.
    fn screen_edr_max(&self, window: &Window) -> f32;

    /// FIRE-INTO-CHROME: extend the window's content view under the titlebar
    /// (`NSWindowStyleMaskFullSizeContentView`) so the GPU surface spans the full
    /// frame — giving the window-space effects layer (fire, embers, meteors) a
    /// drawable band above the terminal grid. Applied ONCE at attach, per chrome'd
    /// terminal window (never from the config-reload appearance path, which hits
    /// every window including Settings). Default ON; `$ATERM_NO_FULLSIZE_CONTENT`
    /// is the escape hatch (checked inside the macOS impl). No-op off macOS.
    fn window_set_fullsize_content(&self, _window: &Window) {}

    /// FIRE-INTO-CHROME: the height of the window's titlebar band in POINTS
    /// (logical) — the chrome headroom the effects layer may draw into. Valid both
    /// BEFORE the full-size-content mask is applied (frame minus content-rect
    /// height) and after (content view height minus `contentLayoutRect` height,
    /// which correctly collapses to 0 in fullscreen). `0.0` off macOS and for
    /// chromeless windows.
    ///
    /// THIS SEAM IS THE *MEASURED* OS BAND, and stays that way. C3 gives Windows a
    /// real WinUI-height tab band through the same `head` MECHANISM, but it is
    /// deliberately NOT plumbed through here: the synthetic band's height is
    /// `target − pad_top − tab_strip_rows·cell_h`, i.e. a function of the live
    /// config and the window's cell box, and an `AppRt` has neither — it would have
    /// to be handed the answer through a side channel and then hand it back, which
    /// is a mailbox pretending to be a measurement. `App` derives it instead
    /// (`App::synthetic_strip_head_px`) and writes the SAME
    /// `WindowState::head_pts` this measurement writes, so everything downstream —
    /// the resize law, the pointer mapping, the chrome bleed, the pixel band — sees
    /// one band and cannot tell the two apart.
    fn titlebar_band_pts(&self, _window: &Window) -> f64 {
        0.0
    }

    /// The usable placement area of the screen `window` sits on — the display
    /// rectangle MINUS the permanent system chrome (on macOS the menu bar and the
    /// Dock: `NSScreen.visibleFrame`) — in LOGICAL POINTS with a TOP-LEFT origin and
    /// y growing DOWN, i.e. the same space `Window::outer_position().to_logical()`
    /// reports. This is what the new-window cascade wraps against, so a cascaded
    /// window never lands under the menu bar or behind the Dock.
    ///
    /// `None` when the platform cannot answer (no native window, an off-screen
    /// window whose `screen` is nil, or a backend with no work-area concept — every
    /// non-macOS apprt today). Callers then fall back to the winit monitor bounds,
    /// which are the FULL display rectangle — a slightly later wrap, never a wrong
    /// one. See [`crate::app_window::WorkAreaPts`].
    fn window_work_area_pts(&self, _window: &Window) -> Option<crate::app_window::WorkAreaPts> {
        None
    }

    /// M3 (Windows scRGB): the display's reference-white scale (`SDR-white / 80`) for
    /// the EDR present, so the grid isn't dim (scRGB `1.0` == 80 nits). Default `1.0`
    /// (no scaling) — correct on macOS (the extended-linear layer auto-maps `1.0` → SDR
    /// white) and on Linux; only the Windows impl queries a real value.
    fn screen_sdr_white_scale(&self, _window: &Window) -> f32 {
        1.0
    }

    /// Spawn the process-wide notification delivery thread and return the bounded
    /// `SyncSender` each tab clones into its engine callbacks. Off macOS this is the
    /// channel-draining stub (senders never block; nothing is delivered).
    fn send_notification_init(
        &self,
        suppress: Arc<Mutex<HashSet<u64>>>,
        silent: Arc<AtomicBool>,
    ) -> SyncSender<NotifyMsg>;

    /// Build + install the native application menu bar, returning the retained
    /// action target the caller keeps alive. `None` off macOS (no menu installed).
    fn install_menu(&self, proxy: &EventLoopProxy<Wake>) -> Option<menu::MenuHandle>;

    /// Create the menu-bar OPERATOR status item (status_item.rs), rendered from
    /// `glance`, returning the retained handle the caller keeps alive for the
    /// process lifetime. Default `None`: no status bar exists off macOS.
    fn install_status_item(
        &self,
        _proxy: &EventLoopProxy<Wake>,
        _glance: &crate::status_item::FleetGlance,
    ) -> Option<crate::status_item::StatusItemHandle> {
        None
    }

    /// Bring another aterm instance's windows to the front by pid (the status
    /// menu's sibling rows). Default `false`: no cross-instance activation
    /// exists off macOS yet.
    fn activate_instance(&self, _pid: u32) -> bool {
        false
    }

    /// Un-minimize and raise OUR OWN `window` — the receiving half of a
    /// `windowing_behavior = "attach"` forward, where `aterm new-tab` spawns a
    /// tab in an instance the user may not be able to see.
    ///
    /// `false` by default (nothing raised). winit's own `focus_window()` is
    /// deliberately NOT the seam: on Windows it early-returns for a MINIMIZED
    /// window — the case that matters most here — and steals the foreground by
    /// SendInput-ing a synthetic Alt keypress, which is not something a
    /// background tab-spawn may inject into the user's input stream.
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

    /// Re-sync a window's native toolbar tab strip to `titles` with the `active`
    /// index selected. `ids` is the canonical STABLE `TabId` per tab, paired by
    /// index — the strip stamps it on each chip so a right-click can capture the
    /// clicked tab's identity at menu-pop time (a positional index would re-bind
    /// to whatever tab later occupied the slot). `ext` is the per-tab chrome
    /// EXTENSION paired by index (session-metadata stage 2): the composed hover
    /// tooltip applied via the chip's `setToolTip:` and the right-click
    /// context-menu MODEL the chip pops as a native `NSMenu` (see
    /// `crate::session_chrome`). No-op off macOS / when no handle exists (the
    /// Linux model still RECORDS `ext` so the `chrome` mirror reads one truth on
    /// every platform).
    fn set_toolbar_tabs(&self, handle: &toolbar::ToolbarHandle, tabs: ToolbarTabsModel<'_>);

    /// Retired native-toolbar update seam. Current implementations are no-ops because
    /// update state and its action live in the version menu. See `toolbar.rs`.
    fn set_toolbar_update_available(&self, handle: &toolbar::ToolbarHandle, available: bool);

    /// Whether this platform could present the native rename editor, WITHOUT
    /// presenting it. `begin_tab_rename` installs the field as a side effect, so
    /// a surface asking "should this command be enabled?" cannot use it.
    /// Default: `false` — no native editor here, so the tab strip is the only
    /// surface, and a window with `tab_strip_rows = 0` has none.
    fn can_present_tab_rename(&self, handle: &toolbar::ToolbarHandle) -> bool {
        let _ = handle;
        false
    }

    /// Open the INLINE SESSION-RENAME editor over the strip's `tab` chip, seeded
    /// with `seed` (the current pin — EMPTY when unpinned) and placeheld with
    /// `placeholder` (the label the ladder falls back to, so an empty field
    /// visibly means "use that"). Returns whether an editor is actually on
    /// screen; `App` refuses to hold edit state that nothing is presenting.
    ///
    /// The editor is owned by the strip HANDLE, not by a tab chip: chips are
    /// destroyed and rebuilt whenever the tab count or the container width
    /// changes, which ordinary background events (a background session exiting,
    /// any window resize) cause mid-edit. `tab` is the STABLE id, used only to
    /// re-find the chip and reposition the editor over it — or, when it is gone,
    /// to cancel. Default: `false` (no inline editor on this platform yet).
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

    /// The live rename editor's current text, or `None` when none is open. The
    /// native field owns the in-progress text, so a command that must run
    /// OUTSIDE an open editor reads it here and commits before proceeding —
    /// otherwise closing a tab with ⌘W mid-rename would silently discard it.
    /// Default: `None`.
    fn rename_editor_text(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        let _ = handle;
        None
    }

    /// Forward one editing command to the live rename field's field editor
    /// instead of to the terminal. macOS resolves a menu key equivalent BEFORE
    /// the first responder sees the key, so without this ⌘V would paste into the
    /// PTY behind an open editor. Returns whether the field editor took it.
    /// Default: `false` (no editor to forward to).
    fn rename_editor_edit(
        &self,
        handle: &toolbar::ToolbarHandle,
        action: RenameEditorEdit,
    ) -> bool {
        let _ = (handle, action);
        false
    }

    /// Read native title chrome as one introspection line (titles, selection,
    /// independent status, and full tooltips), or `None` only for an empty tab set.
    /// On Linux this reflects the real in-memory tab-chrome model
    /// `set_toolbar_tabs` maintains (see `toolbar.rs`), in the SAME line shape macOS
    /// emits.
    ///
    /// The `chrome` verb calls this on every platform. Linux/other non-macOS hosts
    /// return their live in-memory model; macOS reads the retained native tab views.
    fn read_toolbar_chrome(&self, handle: &toolbar::ToolbarHandle) -> Option<String>;

    /// Read the native toolbar tab strip's per-tab CONTEXT-MENU models as
    /// introspection lines (`tab-menu tab=<i> items=[...]` — one per tab chip,
    /// none at ≤1 tab, mirroring the hide-when-≤1 strip rule) for the `chrome`
    /// verb. Ground truth: read off the SAME stored models a right-click pops
    /// (the live macOS `TabView`s' entries / the Linux in-memory chrome model),
    /// serialised by the one pure `session_chrome::tab_menu_chrome_line`, so
    /// the listing IS the on-screen menu. A provided default (both `toolbar`
    /// modules expose `read_tab_menus`) keeps every impl — including the
    /// Windows one, which shares the non-macOS toolbar model — in lockstep.
    fn read_toolbar_tab_menus(&self, handle: &toolbar::ToolbarHandle) -> Vec<String> {
        toolbar::read_tab_menus(handle)
    }

    /// Show a native modal confirmation for a destructive quit/close gesture and
    /// block until the user answers. Returns `Some(true)` to PROCEED, `Some(false)`
    /// to CANCEL, or `None` when the platform has no native dialog — the caller then
    /// falls back to the in-window titlebar-warning confirm. `proceed_label` titles
    /// the affirmative button; a "Cancel" button is always added.
    fn confirm(&self, title: &str, body: &str, proceed_label: &str) -> Option<bool>;

    /// Register the whole-app quit confirmation for AppKit `terminate:` gestures that
    /// BYPASS the application menu's Quit item — the Dock-icon "Quit", AppleScript
    /// `quit`, and logout/restart/shutdown. macOS wires winit's synchronous
    /// `applicationShouldTerminate:` to a typed event-loop deferral, where `App` runs
    /// the same dialog and document-durability barrier as ⌘Q; a no-op off macOS.
    /// Called once, when the menu is installed.
    fn install_quit_confirm(&self);

    /// Query the OS motion preference. macOS reads
    /// `NSWorkspace.accessibilityDisplayShouldReduceMotion`; Windows reads
    /// `SPI_GETCLIENTAREAANIMATION`; platforms without an implemented query
    /// return `false`. The explicit `motion = "reduced"` override works
    /// everywhere. Feeds [`crate::motion::MotionPolicy`] (W11).
    fn reduce_motion(&self) -> bool;

    /// Snapshot the OS-native display accessibility preferences consumed by the
    /// semantic native-app appearance system. Platforms without a query return the
    /// legible opaque 1× default; app/config overrides can be folded above this seam.
    fn native_appearance_preferences(&self) -> crate::native_appearance::AppearancePreferences {
        crate::native_appearance::AppearancePreferences::default()
    }

    /// Subscribe to the OS reduce-motion CHANGE notification (macOS:
    /// `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification` on the
    /// workspace notification center), posting [`Wake::ReduceMotionChanged`] so
    /// the main thread re-queries [`Self::reduce_motion`] and repaints. Returns
    /// the retained observer target the caller must keep alive for the process
    /// life (AppKit references it weakly). Windows currently samples only at
    /// window attach; other platforms have no query, so both return `None`.
    fn observe_reduce_motion(&self, proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver>;
}

/// The colour space a GPU window's CAMetalLayer is tagged with at surface attach
/// (M3). `Srgb` / `DisplayP3` come straight from config `window_colorspace`
/// ([`crate::app_config::WindowColorspace`], mapped via `From`). Pure data: the
/// variant → CoreGraphics-name mapping is [`Self::cg_name`], unit-tested below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceColorspace {
    /// The honest tag for aterm's sRGB-encoded bytes (config default).
    Srgb,
    /// The legacy stretched interpretation (bytes read as P3 coordinates).
    DisplayP3,
    /// M3 phase B: the EDR tag for an `Rgba16Float` swapchain — linear-light
    /// sRGB primaries with >1.0 headroom. NOT a config value: chosen by
    /// [`resolve_surface_colorspace`] whenever the attached surface is HDR
    /// (the f16 pixels ARE linear; any other tag would mis-render them).
    ExtendedLinearSrgb,
}

impl SurfaceColorspace {
    /// The CoreGraphics NAMED-colour-space identifier for this tag, as accepted
    /// by `CGColorSpaceCreateWithName`. These string VALUES equal the
    /// `kCGColorSpace*` constant names (stable ABI — they are serialized into
    /// ICC-tagged files), which lets the macOS impl build the CFString via a
    /// toll-free-bridged NSString instead of linking the CG data symbols.
    // Consumed only by the macOS layer-tagging impl; the mapping is still
    // unit-tested (below) on every platform.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

/// The layer tag for a just-attached (or re-tagged) GPU window: an HDR
/// (`Rgba16Float`) swapchain MUST be read as extended-linear-sRGB — its pixels
/// are linear light with EDR headroom, so the config tag would mis-render them
/// — while an SDR swapchain takes the user's `window_colorspace` verbatim.
/// PURE (a total match, unit-tested below); both the attach seam and the
/// hot-reload re-tag route through it so the precedence has one owner.
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

/// macOS application-runtime: WRAPS the existing objc2 integration exactly. The
/// chrome methods carry the verbatim NSWindow colour-space + NSAppearance logic
/// relocated from `app_window.rs`; the rest forward to the `cfg(macos)`-guarded
/// menu/toolbar/notify modules. Zero-sized.
#[cfg(target_os = "macos")]
pub(crate) struct AppRtMacOS;

#[cfg(target_os = "macos")]
impl AppRt for AppRtMacOS {
    fn can_present_tab_rename(&self, handle: &toolbar::ToolbarHandle) -> bool {
        toolbar::can_present_tab_rename(handle)
    }

    /// Paint the NSWindow background the terminal's theme background colour (`bg`,
    /// as `0x00RRGGBB`), so the transparent titlebar and the bare single-tab
    /// compact bar read as a SEAMLESS extension of the terminal body rather than a
    /// distinct, lighter chrome strip. This is the window-level half of the Ghostty
    /// "transparent" titlebar look (the toolbar.rs strip toggling is the other
    /// half). The terminal content view (softbuffer/Metal layer) paints its own
    /// background over the content area, so this colour only ever shows in the
    /// titlebar region the content view does not cover.
    ///
    /// Best-effort, mirroring [`Self::window_set_appearance`]: off the main thread
    /// or with no AppKit `NSWindow`, it is simply a no-op.
    fn window_set_background_color(&self, window: &Window, bg: u32) {
        use objc2_app_kit::{NSColor, NSView};
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        // SAFETY: `ns_view` points at this window's live NSView (owned by winit for
        // the window's lifetime); we only borrow it — on the main thread, as AppKit
        // requires — to reach its `window` and set the background colour.
        let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
        let Some(ns_window) = view.window() else {
            return;
        };
        let r = f64::from((bg >> 16) & 0xff) / 255.0;
        let g = f64::from((bg >> 8) & 0xff) / 255.0;
        let b = f64::from(bg & 0xff) / 255.0;
        // SAFETY: standard AppKit colour construction + a plain setter on the main
        // thread; the colour is autoreleased and consumed within this call.
        unsafe {
            let color = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0);
            ns_window.setBackgroundColor(Some(&color));
        }
    }

    /// Make the window's colour space match softbuffer's device-RGB content so
    /// CoreAnimation does NOT run a per-frame colour-space conversion on the main
    /// thread. softbuffer (`backends/cg.rs`) builds its CGImage with
    /// `CGColorSpace::new_device_rgb()`; on a wide-gamut (P3) display CoreAnimation
    /// otherwise converts device-RGB → display-P3 on *every* commit
    /// (`CA::Render::prepare_image` → `vImageConvert_AnyToAny`) — profiled at ~half
    /// of all present cost during heavy output. Tagging the NSWindow device-RGB
    /// makes content and window the same space, so the conversion is skipped; the
    /// final space→panel mapping is done once by the WindowServer, not per app
    /// frame. aterm's framebuffer pixels are unchanged — only the redundant gamut
    /// round-trip is removed. `$ATERM_NO_COLORSPACE_MATCH` opts out.
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        use objc2_app_kit::{NSColorSpace, NSView};
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        // SAFETY: `ns_view` points at this window's live NSView (owned by winit for
        // the window's lifetime); we only borrow it — on the main thread, as AppKit
        // requires — to read its `window` and configure it.
        let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
        let Some(ns_window) = view.window() else {
            return;
        };
        // Colour-space match (device-RGB) — see fn doc. SAFETY: standard AppKit calls.
        if std::env::var_os("ATERM_NO_COLORSPACE_MATCH").is_none() {
            unsafe {
                let cs = NSColorSpace::deviceRGBColorSpace();
                ns_window.setColorSpace(Some(&cs));
            }
        }
        // Ghostty-style unified chrome: a transparent titlebar so the window frame
        // (titlebar + traffic lights) reads as a seamless extension of the terminal
        // body. The titlebar's LIGHT/DARK appearance now follows config `window_theme`
        // ([`WindowTheme`]): Dark -> NSAppearanceNameDarkAqua, Light ->
        // NSAppearanceNameAqua, Auto -> leave the appearance UNSET so the window tracks
        // the OS `effectiveAppearance` (including live day-night switches). This
        // replaces the old unconditional dark force that left light-desktop users with
        // permanently dark chrome. `ATERM_NO_DARK_CHROME` still forces Auto (no
        // override) regardless of config, for callers that scripted the old opt-out.
        // SAFETY: `appearanceNamed:`/`setAppearance:`/`setTitlebarAppearsTransparent:`
        // are standard NSWindow/NSAppearance calls on the main thread; the appearance
        // object is autoreleased and used immediately within this pool.
        let resolved = if std::env::var_os("ATERM_NO_DARK_CHROME").is_some() {
            WindowTheme::Auto
        } else {
            theme
        };
        let appearance_name: Option<&str> = match resolved {
            WindowTheme::Auto => None,
            WindowTheme::Light => Some("NSAppearanceNameAqua"),
            WindowTheme::Dark => Some("NSAppearanceNameDarkAqua"),
        };
        unsafe {
            use objc2::runtime::AnyObject;
            use objc2::{class, msg_send};
            use objc2_foundation::NSString;
            if let Some(name) = appearance_name {
                let name = NSString::from_str(name);
                let appearance: *mut AnyObject =
                    msg_send![class!(NSAppearance), appearanceNamed: &*name];
                if !appearance.is_null() {
                    let _: () = msg_send![&*ns_window, setAppearance: appearance];
                }
            } else {
                // Auto: CLEAR any previously-forced appearance (`setAppearance: nil`)
                // so the window resumes tracking the OS `effectiveAppearance`. Without
                // this, a live Dark/Light -> Auto change would leave the last forced
                // appearance stuck instead of reverting. nil is harmless at attach
                // (a fresh NSWindow already tracks the OS).
                let nil_appearance: *const AnyObject = std::ptr::null();
                let _: () = msg_send![&*ns_window, setAppearance: nil_appearance];
            }
            // Transparent titlebar is desired in every mode (it is the
            // chrome-unification half, independent of light/dark).
            let _: () = msg_send![&*ns_window, setTitlebarAppearsTransparent: true];
        }
        // NOTE: the fullSizeContentView style mask deliberately does NOT live here —
        // config reload re-runs this method on EVERY window (including Settings),
        // which would leak the mask onto windows that must keep normal chrome. The
        // mask is applied once, per chrome'd terminal window, at attach — see
        // [`AppRt::window_set_fullsize_content`].
    }

    /// Tag the CAMetalLayer wgpu attached to this window's NSView with the named
    /// colour space (see [`layer_colorspace`]). Best-effort like the other chrome
    /// methods: no AppKit window / no metal layer → a silent no-op.
    fn window_set_surface_colorspace(
        &self,
        window: &Window,
        cs: SurfaceColorspace,
    ) -> Option<SurfaceColorspace> {
        layer_colorspace::set(window, cs.cg_name()).then_some(cs)
    }

    /// Pin the CAMetalLayer's `contentsGravity` to `topLeft` so a live drag never
    /// shows a rescaled stale frame (see [`AppRt::window_anchor_surface_top_left`]).
    /// Best-effort like the other chrome methods: no AppKit window / no metal
    /// layer → a silent no-op.
    fn window_anchor_surface_top_left(&self, window: &Window) {
        layer_colorspace::anchor_contents_top_left(window);
    }

    /// Read back the CAMetalLayer's live gravity / scale / flip. See
    /// [`AppRt::window_surface_presentation`].
    fn window_surface_presentation(&self, window: &Window) -> Option<(String, f64, bool)> {
        layer_colorspace::read_contents_presentation(window)
    }

    /// Install / update / remove the window-level `NSVisualEffectView` and flip the
    /// window + CAMetalLayer opacity for M5 true vibrancy. All the AppKit work lives
    /// in the `vibrancy` module (hand-rolled `msg_send`, like `layer_colorspace`).
    /// Best-effort: no AppKit window → a silent no-op.
    fn window_set_vibrancy(
        &self,
        window: &Window,
        material: crate::app_config::BackgroundMaterial,
        translucent: bool,
        bg: u32,
    ) {
        vibrancy::set(window, material, translucent, bg);
    }

    /// `NSScreen.maximumExtendedDynamicRangeColorComponentValue` for this
    /// window's screen. Best-effort: any missing link (no AppKit window, the
    /// window is off-screen so `screen` is nil) returns `1.0` — no headroom,
    /// which the proven sanitizer treats as SDR.
    fn screen_edr_max(&self, window: &Window) -> f32 {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        use objc2_app_kit::NSView;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = window.window_handle() else {
            return 1.0;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return 1.0;
        };
        // SAFETY: `ns_view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires;
        // `screen` / the EDR getter are plain side-effect-free property reads
        // (the getter returns CGFloat == f64).
        unsafe {
            let view: &NSView = &*(h.ns_view.as_ptr() as *const NSView);
            let Some(ns_window) = view.window() else {
                return 1.0;
            };
            let screen: *mut AnyObject = msg_send![&*ns_window, screen];
            if screen.is_null() {
                return 1.0;
            }
            let v: f64 = msg_send![screen, maximumExtendedDynamicRangeColorComponentValue];
            v as f32
        }
    }

    /// Apply `NSWindowStyleMaskFullSizeContentView` (1<<15) so the content view —
    /// and with it the GPU surface — spans under the titlebar. Best-effort like
    /// the other chrome methods: no AppKit window → a silent no-op. Default ON;
    /// `$ATERM_NO_FULLSIZE_CONTENT` is the escape hatch (a plain env opt-out, so
    /// a misbehaving band never needs a rebuild to disable).
    fn window_set_fullsize_content(&self, window: &Window) {
        use objc2::msg_send;
        use objc2_app_kit::NSView;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if std::env::var_os("ATERM_NO_FULLSIZE_CONTENT").is_some() {
            return;
        }
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        // SAFETY: `ns_view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires;
        // the style-mask read-modify-write is a standard NSWindow property pair.
        unsafe {
            let view: &NSView = &*(h.ns_view.as_ptr() as *const NSView);
            let Some(ns_window) = view.window() else {
                return;
            };
            let mask: usize = msg_send![&*ns_window, styleMask];
            let _: () = msg_send![&*ns_window, setStyleMask: mask | (1usize << 15)];
        }
    }

    /// The titlebar band height in POINTS: the max of the two AppKit derivations —
    /// `frame.height - contentRectForFrameRect(frame).height` (valid BEFORE the
    /// full-size-content mask, when the content rect excludes the titlebar) and
    /// `contentView.bounds.height - contentLayoutRect.height` (valid AFTER the
    /// mask, when the content view spans the full frame but `contentLayoutRect`
    /// still excludes the chrome — and correctly `0` in fullscreen, where the
    /// titlebar detaches). Best-effort: any missing link returns `0.0` (no band).
    fn titlebar_band_pts(&self, window: &Window) -> f64 {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        use objc2_app_kit::NSView;
        use objc2_foundation::NSRect;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = window.window_handle() else {
            return 0.0;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return 0.0;
        };
        // SAFETY: `ns_view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires;
        // every message below is a side-effect-free geometry read.
        unsafe {
            let view: &NSView = &*(h.ns_view.as_ptr() as *const NSView);
            let Some(ns_window) = view.window() else {
                return 0.0;
            };
            // ONE formula, post-mask only: `contentView.bounds − contentLayoutRect`
            // is the chrome band actually overlapping the drawable content. It is
            // 0 until fullSizeContentView is applied (so the ATERM_NO_FULLSIZE_CONTENT
            // escape hatch yields head 0 — the grid keeps the whole content view),
            // 0 again in native fullscreen (the titlebar hides — the band collapses
            // and on_resize re-derives), and the full titlebar+toolbar height once
            // the mask + toolbar are in (callers measure AFTER installing both).
            // The pre-mask `frame − contentRectForFrameRect` form was dropped: it
            // under-counts the unified toolbar AND reports a phantom band when the
            // mask was deliberately never applied (adversarial review).
            let content_view: *mut AnyObject = msg_send![&*ns_window, contentView];
            if content_view.is_null() {
                return 0.0;
            }
            let bounds: NSRect = msg_send![content_view, bounds];
            let layout: NSRect = msg_send![&*ns_window, contentLayoutRect];
            (bounds.size.height - layout.size.height).max(0.0)
        }
    }

    /// `NSScreen.visibleFrame` of the screen this window is on, flipped into
    /// winit's top-left-origin point space (see [`AppRt::window_work_area_pts`]).
    ///
    /// AppKit's screen space is BOTTOM-left origin; winit reports window positions
    /// relative to the TOP-left of the MAIN display (`CGMainDisplayID`), so the flip
    /// must use the identical law as winit's own `flip_window_screen_coordinates`
    /// (`y_top = main_height - rect.height - rect.origin.y`) — anything else would
    /// silently offset the whole cascade on a multi-display desktop. The main
    /// display's height therefore comes from the same CoreGraphics call winit uses,
    /// NOT from `NSScreen.mainScreen` (which is the FOCUSED screen, not the origin
    /// screen).
    fn window_work_area_pts(&self, window: &Window) -> Option<crate::app_window::WorkAreaPts> {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        use objc2_app_kit::NSView;
        use objc2_foundation::NSRect;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGMainDisplayID() -> u32;
            fn CGDisplayBounds(display: u32) -> NSRect;
        }

        let Ok(handle) = window.window_handle() else {
            return None;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        // SAFETY: `ns_view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires; every
        // message below is a side-effect-free geometry read, and the CoreGraphics
        // pair is a flat display-bounds getter that neither allocates nor dispatches.
        unsafe {
            let view: &NSView = &*(h.ns_view.as_ptr() as *const NSView);
            let ns_window = view.window()?;
            // Nil for a window that is not on any screen (fully off-screen, or a
            // hidden window the WindowServer has not placed yet) — fail closed to
            // the caller's monitor-bounds fallback rather than guess a screen.
            let screen: *mut AnyObject = msg_send![&*ns_window, screen];
            if screen.is_null() {
                return None;
            }
            let visible: NSRect = msg_send![screen, visibleFrame];
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
    ) -> SyncSender<NotifyMsg> {
        crate::notify::spawn_delivery(suppress, silent)
    }

    fn install_menu(&self, proxy: &EventLoopProxy<Wake>) -> Option<menu::MenuHandle> {
        menu::install(proxy)
    }

    fn install_status_item(
        &self,
        proxy: &EventLoopProxy<Wake>,
        glance: &crate::status_item::FleetGlance,
    ) -> Option<crate::status_item::StatusItemHandle> {
        crate::status_item::install(proxy, glance)
    }

    fn activate_instance(&self, pid: u32) -> bool {
        // NSRunningApplication is InteriorMutable (not MainThreadOnly), but
        // this is only ever called on the event-loop turn, keeping the AppKit
        // call ordered against our own window state. `ActivateAllWindows`
        // only — `IgnoringOtherApps` is deprecated and inert on macOS 14+.
        use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
        if pid == 0 || pid == std::process::id() {
            return false;
        }
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: plain AppKit lookup + activation request; a dead pid yields
        // None and reads as a failed activation, never a crash.
        unsafe {
            NSRunningApplication::runningApplicationWithProcessIdentifier(pid).is_some_and(
                |app| {
                    app.activateWithOptions(
                        NSApplicationActivationOptions::NSApplicationActivateAllWindows,
                    )
                },
            )
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
        } = tabs;
        toolbar::set_window_tabs(handle, titles, ids, metadata, tooltips, ext, active);
    }

    fn set_toolbar_update_available(&self, handle: &toolbar::ToolbarHandle, available: bool) {
        toolbar::set_update_available(handle, available);
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

    fn install_quit_confirm(&self) {
        // winit owns the `NSApplication` delegate, so the `applicationShouldTerminate:`
        // veto is registered through a small vendored-winit seam. The callback has no
        // access to `App`, so it defers onto the typed event loop; the main-thread App
        // then runs the same confirmation + document durability barrier as ⌘Q.
        winit::platform::macos::set_application_should_terminate_handler(
            menu::defer_quit_for_terminate,
        );
    }

    fn reduce_motion(&self) -> bool {
        reduce_motion::query()
    }

    fn native_appearance_preferences(&self) -> crate::native_appearance::AppearancePreferences {
        crate::native_appearance::AppearancePreferences {
            high_contrast: reduce_motion::increase_contrast(),
            reduced_transparency: reduce_motion::reduce_transparency(),
            text_scale: 1.0,
        }
    }

    fn observe_reduce_motion(&self, proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver> {
        reduce_motion::observe(proxy)
    }
}

/// Map aterm's config [`WindowTheme`] onto the winit window-theme override
/// [`winit::window::Theme`] applied via [`Window::set_theme`]:
/// * `Auto`  → `None` — reset to the system default so the chrome tracks the OS
///   light/dark preference (on Wayland winit reads it over D-Bus; on X11 winit
///   defaults the `_GTK_THEME_VARIANT` hint to dark).
/// * `Light` → `Some(Theme::Light)` — force the light client-side-decoration variant.
/// * `Dark`  → `Some(Theme::Dark)` — force the dark variant.
///
/// PURE: a total `match` with no I/O, so it is unit-tested directly (see the `tests`
/// module). The non-macOS [`AppRt::window_set_appearance`] is just this mapping fed
/// to `set_theme`.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub(crate) fn window_theme_to_winit(theme: WindowTheme) -> Option<winit::window::Theme> {
    match theme {
        WindowTheme::Auto => None,
        WindowTheme::Light => Some(winit::window::Theme::Light),
        WindowTheme::Dark => Some(winit::window::Theme::Dark),
    }
}

/// How long the NSEvent currently being dispatched sat in the NSApp queue before
/// this code ran, in nanoseconds — the touch-to-glass slice NO post-dequeue stamp
/// can see. `NSEvent.timestamp` and `NSProcessInfo.systemUptime` share the
/// seconds-since-boot clock, so their difference IS the queue age (hardware
/// arrival → dispatch). A parked event loop (the FIFO drawable park, a long
/// handler) queues keyDowns for tens of milliseconds; backdating the latency
/// stamps by this age makes that visible to `key->write`, `input->present`, and
/// the video key ledger. Fail-closed `None`: off the main thread, outside a
/// dispatch, or on a suspicious age (negative beyond scheduler jitter, or > 2 s —
/// clock skew / a synthesized or replayed event) the caller falls back to "now",
/// which is exactly today's behaviour.
#[cfg(target_os = "macos")]
pub(crate) fn current_event_queue_age_ns() -> Option<u64> {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{MainThreadMarker, NSProcessInfo};

    let mtm = MainThreadMarker::new()?;
    let ev = NSApplication::sharedApplication(mtm).currentEvent()?;
    let ts = unsafe { ev.timestamp() };
    if ts <= 0.0 {
        return None; // synthesized events carry timestamp 0
    }
    // SAFETY: `systemUptime` is a plain scalar getter with no preconditions
    // (marked unsafe only by objc2's blanket policy for unaudited methods).
    let age_s = unsafe { NSProcessInfo::processInfo().systemUptime() } - ts;
    // Negative up to ~1 ms is clock-read jitter (treat as "no queueing"); beyond
    // the window in either direction the pair can't be trusted — fail closed.
    if !(-0.001..=2.0).contains(&age_s) {
        return None;
    }
    Some((age_s.max(0.0) * 1e9) as u64)
}

/// Whether a hardware-input event occurred inside `within`.
///
/// Reads the KERNEL's HID idle clock — the `HIDIdleTime` property of the
/// `IOHIDSystem` registry entry, nanoseconds since the last event from any HID
/// device, physical or virtual — through IOKit (see [`hid_idle`]). Machine-wide
/// and therefore conservative (activity in another application postpones an
/// automatic update), which is what the "install when quiet" policy wants.
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
/// `nextEventMatchingMask` for still holds — see the guard test below.)
///
/// Fails CLOSED: if the HID system cannot be read, input is reported as recent
/// (the machine is treated as busy), the same posture as an invalid sample.
#[cfg(target_os = "macos")]
pub(crate) fn recent_user_input_event(within: std::time::Duration) -> bool {
    match hid_idle::seconds_since_last_input() {
        Some(age_seconds) => user_input_age_is_recent(age_seconds, within.as_secs_f64()),
        None => true,
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn recent_user_input_event(_within: std::time::Duration) -> bool {
    false
}

/// The input-activity source for HEADLESS instances and unit tests: never
/// recent. A headless `App` has no operator at a keyboard whose activity should
/// postpone anything, and a unit test must not depend on whether the machine
/// running it happens to be busy — so neither may consult the platform. This is
/// also the guarantee that keeps a test binary from ever reaching WindowServer
/// through the input-activity path (see [`recent_user_input_event`]).
#[must_use]
pub(crate) fn no_recent_user_input_event(_within: std::time::Duration) -> bool {
    false
}

/// The kernel's HID idle clock, read from the IORegistry — the one
/// input-activity source on macOS that never involves WindowServer.
///
/// Tiny-FFI posture (like `keymap::hid_lock_state`): IOKit + CoreFoundation
/// are system frameworks (no crate on the dependency surface); the SDK
/// signatures are declared inline. The `IOHIDSystem` service and the property
/// key are looked up ONCE and cached; each read is one `IORegistryEntry
/// CreateCFProperty` (a synchronous kernel RPC, thread-agnostic) plus a
/// `CFNumber` unwrap. Measured on macOS 26.5.1: unentitled read succeeds and the
/// value advances one second per second while the machine is idle.
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
        /// Returns a CFMutableDictionaryRef, CONSUMED by `IOServiceGetMatchingService`.
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
                // SAFETY: `IOServiceMatching` takes a NUL-terminated class name
                // and returns an owned dictionary consumed by
                // `IOServiceGetMatchingService`; the returned service is retained
                // for the life of the process (deliberately never released — it
                // is the cache) unless the key cannot be made, in which case it is
                // released. `CFStringCreateWithCString` takes a NUL-terminated
                // UTF-8 literal; the string is likewise kept for the process.
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
                && CFNumberGetValue(value, CF_NUMBER_SINT64_TYPE, (&raw mut idle_ns).cast::<c_void>())
                    != 0;
            CFRelease(value);
            #[allow(clippy::cast_precision_loss)] // nanoseconds → seconds; sub-ns precision is irrelevant
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

    /// Regression for crash-45791: even a non-dequeuing AppKit event query can
    /// run winit's queued observer closure and recursively enter its handler.
    /// Keep the event-pumping selector out of the platform layer completely.
    #[test]
    fn automatic_update_input_probe_never_polls_the_appkit_event_queue() {
        let forbidden_selector = ["nextEventMatchingMask_", "untilDate_inMode_dequeue("].concat();
        assert!(
            !include_str!("platform.rs").contains(&forbidden_selector),
            "winit callbacks must not query AppKit through its event-pumping selector"
        );
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
        assert_eq!(first.is_some(), second.is_some(), "the cached handles answer consistently");
        if let (Some(a), Some(b)) = (first, second) {
            assert!(a >= 0.0 && b >= 0.0, "idle ages are non-negative: {a} {b}");
        }
    }

    /// The headless/test source is inert: what keeps a unit test's admission
    /// decisions independent of the machine running it, and what keeps a test
    /// binary off the platform input-activity path (2026-08-17).
    #[test]
    fn no_recent_user_input_event_is_never_recent() {
        assert!(!super::no_recent_user_input_event(std::time::Duration::from_secs(3600)));
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn current_event_queue_age_ns() -> Option<u64> {
    // winit exposes no event timestamp off macOS; the post-dequeue stamp stands.
    None
}

/// Non-macOS application-runtime. Unlike the original dead no-op, every method now
/// does the most useful thing the **pure-winit** surface allows (NO new system
/// libraries — see [`AppRt::window_set_background_color`] and `toolbar.rs` for the
/// deferred GTK4 work): the appearance method drives `winit::Window::set_theme` so
/// the client-side-decoration light/dark variant honours config `window_theme`; the
/// toolbar seam maintains a REAL in-memory tab-chrome model (`toolbar.rs`) and seeds
/// the window title from it; the menu install delegates to the menu stub; and
/// notification delivery forwards to `notify::spawn_delivery`'s channel-draining
/// stub. The terminal renders + input works; only the genuinely OS-native chrome a
/// header bar would add (which needs gtk4 system libs) is deferred. Zero-sized.
#[cfg(all(not(target_os = "macos"), not(windows)))]
pub(crate) struct AppRtLinux;

#[cfg(all(not(target_os = "macos"), not(windows)))]
impl AppRt for AppRtLinux {
    /// INTENTIONAL no-op: winit exposes NO per-window background-COLOUR setter (only
    /// `set_transparent` / `set_blur`, which toggle the surface's transparency, not a
    /// fill colour). The terminal-body background colour is already painted by the
    /// renderer's own surface clear (softbuffer/wgpu), so there is nothing window-
    /// level to set here without a native toolkit. A full GTK4 header bar would paint
    /// its own background to match `bg` — deferred with the rest of the header bar
    /// (see `toolbar.rs`). Documented rather than silently empty.
    fn window_set_background_color(&self, _window: &Window, _bg: u32) {}

    /// Apply the window-chrome appearance by overriding winit's window theme: map
    /// config [`WindowTheme`] → [`winit::window::Theme`] via [`window_theme_to_winit`]
    /// and hand it to [`Window::set_theme`]. On Wayland this themes the client-side
    /// decorations (titlebar/border); on X11 it sets the `_GTK_THEME_VARIANT` hint.
    /// `Auto` resets to the OS preference. This is the real, buildable Linux analogue
    /// of the macOS `NSAppearance` override.
    fn window_set_appearance(&self, window: &Window, theme: WindowTheme) {
        window.set_theme(window_theme_to_winit(theme));
    }

    /// INTENTIONAL no-op: CAMetalLayer colour-space tagging is a CoreAnimation
    /// concept. The Linux compositor path presents wgpu's Vulkan swapchain
    /// directly; explicit surface colour management there (`VK_EXT_swapchain_
    /// colorspace` / Wayland `color-management-v1`) is deferred with the rest of
    /// the native chrome work. Documented rather than silently empty.
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
    /// SUBLAYER `raw-window-metal` installs. The Vulkan/Wayland path presents the
    /// swapchain directly, so there is no intermediate layer to anchor.
    /// Documented rather than silently empty.
    fn window_anchor_surface_top_left(&self, _window: &Window) {}

    /// No CoreAnimation layer to read: the Vulkan/Wayland path presents the
    /// swapchain directly, so there is no intermediate layer whose gravity could
    /// rescale a stale frame — and therefore nothing to report.
    fn window_surface_presentation(&self, _window: &Window) -> Option<(String, f64, bool)> {
        None
    }

    /// INTENTIONAL no-op: the `NSVisualEffectView` behind-window blur is an AppKit
    /// concept. The Linux analogue is winit `set_transparent` / `set_blur` plus a
    /// Wayland `blur` protocol (KDE) / GTK4 backdrop, deferred with the rest of the
    /// native chrome work — so `background_material` has no window-level consumer
    /// here yet (the GPU translucent-present path is macOS-first). Documented rather
    /// than silently empty.
    fn window_set_vibrancy(
        &self,
        _window: &Window,
        _material: crate::app_config::BackgroundMaterial,
        _translucent: bool,
        _bg: u32,
    ) {
    }

    /// Linux has no portable EDR-headroom query yet (a Wayland `color-management-v1`
    /// / `VK_EXT_hdr_metadata` read is the follow-up): `1.0` — no headroom, so the
    /// aurora pass is provably inert. (The Windows EDR query lives in the dedicated
    /// `platform_win::AppRtWindows`.)
    fn screen_edr_max(&self, _window: &Window) -> f32 {
        1.0
    }

    /// `1.0` (no scaling) on Linux — the scRGB reference-white scale is a
    /// Windows-HDR concept, handled in `platform_win::AppRtWindows`.
    fn screen_sdr_white_scale(&self, _window: &Window) -> f32 {
        1.0
    }

    fn send_notification_init(
        &self,
        suppress: Arc<Mutex<HashSet<u64>>>,
        silent: Arc<AtomicBool>,
    ) -> SyncSender<NotifyMsg> {
        crate::notify::spawn_delivery(suppress, silent)
    }

    // The branches below delegate to the `menu::`/`toolbar::` modules — the menu is
    // still a `None` stub (a native Linux menu bar needs a GTK4 `gtk::PopoverMenuBar`
    // / app-menu D-Bus export, deferred), while the toolbar now backs a REAL
    // in-memory tab-chrome model. One platform surface, no dead code on Linux.
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
        } = tabs;
        toolbar::set_window_tabs(handle, titles, ids, metadata, tooltips, ext, active);
    }

    fn set_toolbar_update_available(&self, handle: &toolbar::ToolbarHandle, available: bool) {
        toolbar::set_update_available(handle, available);
    }

    fn read_toolbar_chrome(&self, handle: &toolbar::ToolbarHandle) -> Option<String> {
        toolbar::read_tab_chrome(handle)
    }

    /// No native dialog off macOS: `None` tells the caller to fall back to the
    /// in-window titlebar-warning confirm (see `App::confirm_destructive_close`).
    fn confirm(&self, _title: &str, _body: &str, _proceed_label: &str) -> Option<bool> {
        None
    }

    /// No `terminate:`-style app-quit gesture to intercept off macOS: a no-op.
    fn install_quit_confirm(&self) {}

    /// No portable OS reduce-motion query off macOS yet (see the trait doc):
    /// `false`, so config `motion` alone decides. A GNOME
    /// `enable-animations` D-Bus read is the documented follow-up.
    fn reduce_motion(&self) -> bool {
        false
    }

    /// No OS notification to observe off macOS: `None` (the caller keeps the
    /// config-driven policy; a re-query costs nothing).
    fn observe_reduce_motion(&self, _proxy: &EventLoopProxy<Wake>) -> Option<ReduceMotionObserver> {
        None
    }
}

/// The concrete application-runtime the `App` stores, selected at compile time:
/// [`AppRtMacOS`] on macOS, [`AppRtLinux`] everywhere else. Zero-sized, so the
/// `App.apprt` field costs nothing.
#[cfg(target_os = "macos")]
pub(crate) type PlatformAppRt = AppRtMacOS;

/// The native Windows selection — real DWM chrome (see `crate::platform_win`).
#[cfg(windows)]
pub(crate) type PlatformAppRt = crate::platform_win::AppRtWindows;

/// The graceful no-op selection for every OTHER non-macOS target (Linux/BSD).
#[cfg(all(not(target_os = "macos"), not(windows)))]
pub(crate) type PlatformAppRt = AppRtLinux;

/// Construct the platform application-runtime for this build target. The single
/// place `App` mints its `apprt` field, so the cfg lives here, not at the
/// construction sites.
pub(crate) fn platform_apprt() -> PlatformAppRt {
    #[cfg(target_os = "macos")]
    {
        AppRtMacOS
    }
    #[cfg(windows)]
    {
        crate::platform_win::AppRtWindows
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        AppRtLinux
    }
}

/// The retained reduce-motion notification observer [`AppRt::observe_reduce_motion`]
/// returns: the objc target on macOS, `()` elsewhere (so the `App` field type is
/// the same name on every platform, like `menu::MenuHandle`).
#[cfg(target_os = "macos")]
pub(crate) type ReduceMotionObserver = objc2::rc::Retained<reduce_motion::ReduceMotionTarget>;

/// See the macOS variant above — nothing to retain off macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) type ReduceMotionObserver = ();

/// macOS "Reduce Motion" integration (W11): the one query + the one observer.
/// Follows the [`crate::menu`] `MenuTarget` pattern — a declared `NSObject`
/// subclass owning the `EventLoopProxy<Wake>`, registered on the WORKSPACE
/// notification center for `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`
/// and relaying each post into the existing `Wake` channel (no policy logic here;
/// the main thread re-queries and resolves).
#[cfg(target_os = "macos")]
mod reduce_motion {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{
        ClassType, DeclaredClass, class, declare_class, msg_send, msg_send_id, mutability, sel,
    };
    use objc2_foundation::{MainThreadMarker, NSString};
    use winit::event_loop::EventLoopProxy;

    use crate::Wake;

    declare_class!(
        /// The notification-observer target. Owns the `EventLoopProxy<Wake>` and
        /// exposes one `reduceMotionDidChange:` selector that posts
        /// [`Wake::ReduceMotionChanged`] — a pure relay from AppKit into the
        /// event loop (the main thread re-queries the flag there, so a burst of
        /// notifications coalesces to the latest value).
        pub(crate) struct ReduceMotionTarget;

        // SAFETY:
        // - NSObject imposes no subclassing requirements.
        // - InteriorMutable is the safe default; we never mutate the proxy.
        // - ReduceMotionTarget has no Drop impl beyond the auto-generated ivar drop.
        unsafe impl ClassType for ReduceMotionTarget {
            type Super = objc2::runtime::NSObject;
            type Mutability = mutability::InteriorMutable;
            const NAME: &'static str = "ATermReduceMotionTarget";
        }

        impl DeclaredClass for ReduceMotionTarget {
            type Ivars = EventLoopProxy<Wake>;
        }

        unsafe impl ReduceMotionTarget {
            /// The observer selector. Fire-and-forget: a closed loop (app
            /// shutting down) just drops the event, like every other relay.
            #[method(reduceMotionDidChange:)]
            fn reduce_motion_did_change(&self, _note: Option<&AnyObject>) {
                let _ = self.ivars().send_event(Wake::ReduceMotionChanged);
            }
        }
    );

    /// Query `NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion`
    /// — the live OS "Reduce Motion" accessibility preference.
    pub(crate) fn query() -> bool {
        // SAFETY: `sharedWorkspace` is a standard singleton accessor and the
        // getter is a plain side-effect-free BOOL read.
        unsafe {
            let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            if ws.is_null() {
                return false;
            }
            msg_send![ws, accessibilityDisplayShouldReduceMotion]
        }
    }

    /// Query the sibling display-accessibility values posted by the same workspace
    /// notification. Keeping all three under one observer makes a burst atomic at the
    /// next event-loop turn.
    pub(crate) fn increase_contrast() -> bool {
        unsafe {
            let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            if ws.is_null() {
                return false;
            }
            msg_send![ws, accessibilityDisplayShouldIncreaseContrast]
        }
    }

    pub(crate) fn reduce_transparency() -> bool {
        unsafe {
            let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            if ws.is_null() {
                return false;
            }
            msg_send![ws, accessibilityDisplayShouldReduceTransparency]
        }
    }

    /// Register a workspace-notification observer for the accessibility
    /// display-options change and return the retained target (the caller keeps
    /// it alive for the process life — the notification center references it
    /// weakly, and it is never removed, exactly like the menu target). `None`
    /// off the main thread (AppKit requirement; the winit loop guarantees it).
    pub(crate) fn observe(proxy: &EventLoopProxy<Wake>) -> Option<Retained<ReduceMotionTarget>> {
        let mtm = MainThreadMarker::new()?;
        let target: Retained<ReduceMotionTarget> = {
            let this = mtm.alloc().set_ivars(proxy.clone());
            // SAFETY: plain `[super init]` on a freshly allocated instance.
            unsafe { msg_send_id![super(this), init] }
        };
        // The accessibility display-options notification is posted on the
        // WORKSPACE notification center (not the default center). The name
        // string equals AppKit's exported constant value.
        let name =
            NSString::from_str("NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification");
        // SAFETY: standard main-thread AppKit calls; `addObserver:` retains
        // nothing (weak observer), the target outlives the run loop in `App`.
        unsafe {
            let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            if ws.is_null() {
                return None;
            }
            let center: *mut AnyObject = msg_send![ws, notificationCenter];
            if center.is_null() {
                return None;
            }
            let _: () = msg_send![
                center,
                addObserver: &*target,
                selector: sel!(reduceMotionDidChange:),
                name: &*name,
                object: std::ptr::null::<AnyObject>()
            ];
        }
        Some(target)
    }
}

/// CAMetalLayer colour-space tagging (M3, colour-managed present): the one
/// place that reaches the wgpu-created metal layer and sets its `colorspace`
/// property. Hand-rolled CoreGraphics FFI in the `main.rs::cg_capture` style —
/// CoreGraphics is already linked in-process (AppKit pulls it in), so the
/// `#[link]` adds bindings, not a dependency.
#[cfg(target_os = "macos")]
mod layer_colorspace {
    use std::ffi::c_void;

    use objc2::encode::{Encoding, RefEncode};
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    /// An OPAQUE `CGColorSpace` — never dereferenced in Rust; we only ever hold pointers
    /// to it (handed back to CG / CoreAnimation or released). Giving it the real struct
    /// encoding (rather than a bare `*mut c_void`, which encodes as `^v`) makes a
    /// `*mut CGColorSpace` encode as `^{CGColorSpace=}` — exactly the argument type
    /// `-[CAMetalLayer setColorspace:]` declares, so objc2's message-type check accepts
    /// it instead of aborting the process (a `^v` vs `^{CGColorSpace=}` mismatch panic on
    /// every windowed GPU launch).
    #[repr(C)]
    struct CGColorSpace {
        _opaque: [u8; 0],
    }
    // SAFETY: a pointer to an opaque CoreGraphics type; the encoding names the struct so
    // the pointer reads as `^{CGColorSpace=}` — what CoreAnimation's setter expects.
    unsafe impl RefEncode for CGColorSpace {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColorSpace", &[]));
    }
    type CGColorSpaceRef = *mut CGColorSpace;

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

    /// Find the CAMetalLayer wgpu attached to `view` and set its `colorspace`
    /// to the named CG colour space. Best-effort: any missing link in the chain
    /// (no AppKit handle, no layer, unknown name) is a silent no-op — the layer
    /// then keeps its previous tag (untagged = the legacy panel-native read).
    #[must_use]
    pub(crate) fn set(window: &Window, cg_name: &str) -> bool {
        let Ok(handle) = window.window_handle() else {
            return false;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return false;
        };
        // SAFETY: `ns_view` points at this window's live NSView (winit owns it
        // for the window's lifetime); borrowed on the main thread (this is only
        // reached from the winit event loop), as AppKit requires. The layer
        // walk reads retained CALayer objects owned by that view; the setter is
        // a plain property write that RETAINS the colour space, so releasing
        // our create-ref afterwards is balanced.
        unsafe {
            let view = h.ns_view.as_ptr() as *mut AnyObject;
            let Some(layer) = find_metal_layer(view) else {
                return false;
            };
            let name = NSString::from_str(cg_name);
            let cs = CGColorSpaceCreateWithName(&*name as *const NSString as *const c_void);
            if cs.is_null() {
                return false;
            }
            let _: () = msg_send![layer, setColorspace: cs];
            CGColorSpaceRelease(cs);
            true
        }
    }

    /// LIVE-RESIZE ANCHOR: set the window's CAMetalLayer `contentsGravity` to
    /// `topLeft`, so a bounds change the app has not yet repainted leaves the
    /// previous drawable at 1:1 in place instead of stretching it to fit (see
    /// [`super::AppRt::window_anchor_surface_top_left`] for the full rationale).
    /// Best-effort: no metal layer → a silent no-op. Shares [`find_metal_layer`]
    /// with the colour-space tagger.
    pub(crate) fn anchor_contents_top_left(window: &Window) {
        // SAFETY: the `CALayerContentsGravity` constants are stable exported
        // QuartzCore symbols; these are reads of immutable `CFStringRef` globals,
        // never freed or mutated here.
        #[link(name = "QuartzCore", kind = "framework")]
        unsafe extern "C" {
            /// Pins unrescaled contents to the layer's MAXIMUM-Y left corner.
            static kCAGravityTopLeft: *const AnyObject;
            /// Pins unrescaled contents to the layer's MINIMUM-Y left corner.
            static kCAGravityBottomLeft: *const AnyObject;
        }
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        // SAFETY: `ns_view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires;
        // `contentsAreFlipped` is a side-effect-free read and
        // `setContentsGravity:` is a plain property write that RETAINS the
        // string, which is a framework constant that outlives us.
        unsafe {
            let view = h.ns_view.as_ptr() as *mut AnyObject;
            let Some(layer) = find_metal_layer(view) else {
                return;
            };
            // WHICH CORNER IS "VISUALLY TOP" IS NOT A CONSTANT — ASK THE LAYER.
            //
            // CoreAnimation defines these names on the layer's own Y axis, not on
            // the screen: `CALayer.h` states "'bottom' always means Minimum Y and
            // 'top' always means Maximum Y". winit's NSView reports
            // `isFlipped == true` (it uses an upper-left origin), and the layer we
            // are writing is a `raw-window-metal` SUBLAYER of that flipped view's
            // backing layer — so assuming `kCAGravityTopLeft` is the visual top-left
            // is exactly the kind of guess that anchors a stale frame to the WRONG
            // edge and makes it slide during a drag instead of sitting still.
            //
            // `-[CALayer contentsAreFlipped]` is the API for precisely this: it
            // reports whether contents are implicitly flipped when rendered, i.e.
            // whether minimum-Y is the visual top. Read it and pick the constant
            // that lands on the visual TOP-LEFT either way, which is the corner a
            // terminal grows from.
            let flipped: bool = msg_send![layer, contentsAreFlipped];
            let gravity = if flipped {
                kCAGravityBottomLeft
            } else {
                kCAGravityTopLeft
            };
            if gravity.is_null() {
                return;
            }
            let _: () = msg_send![layer, setContentsGravity: gravity];
        }
    }

    /// Read back the CAMetalLayer's live `contentsGravity`, `contentsScale` and
    /// `contentsAreFlipped` — the state
    /// [`anchor_contents_top_left`](Self::anchor_contents_top_left) established, so
    /// it can be asserted rather than assumed (see
    /// [`super::AppRt::window_surface_presentation`]). Pure reads; `None` when there
    /// is no metal layer.
    pub(crate) fn read_contents_presentation(window: &Window) -> Option<(String, f64, bool)> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        // SAFETY: `ns_view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires. Every
        // message below is a side-effect-free property read; the returned
        // `contentsGravity` is an autoreleased NSString we copy out immediately.
        unsafe {
            let view = h.ns_view.as_ptr() as *mut AnyObject;
            let layer = find_metal_layer(view)?;
            let gravity: *mut AnyObject = msg_send![layer, contentsGravity];
            let gravity = if gravity.is_null() {
                "none".to_string()
            } else {
                let utf8: *const std::ffi::c_char = msg_send![gravity, UTF8String];
                if utf8.is_null() {
                    "none".to_string()
                } else {
                    std::ffi::CStr::from_ptr(utf8)
                        .to_string_lossy()
                        .into_owned()
                }
            };
            let scale: f64 = msg_send![layer, contentsScale];
            let flipped: bool = msg_send![layer, contentsAreFlipped];
            Some((gravity, scale, flipped))
        }
    }

    /// M5: set the window's CAMetalLayer `opaque` flag. `false` lets the GPU's
    /// translucent (PostMultiplied) present composite over the `NSVisualEffectView`
    /// behind it; `true` restores the opaque default. Best-effort: no metal layer →
    /// a silent no-op. Shares [`find_metal_layer`] with the colour-space tagger.
    pub(crate) fn set_metal_opaque(window: &Window, opaque: bool) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        // SAFETY: `ns_view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires;
        // `setOpaque:` is a plain BOOL property write on the CAMetalLayer.
        unsafe {
            let view = h.ns_view.as_ptr() as *mut AnyObject;
            let Some(layer) = find_metal_layer(view) else {
                return;
            };
            let _: () = msg_send![layer, setOpaque: opaque];
        }
    }

    /// The CAMetalLayer under `view`, if any. wgpu (via raw-window-metal) either
    /// re-types the view's own backing layer as a CAMetalLayer or — the common
    /// winit case — installs one as a SUBLAYER of the existing backing layer, so
    /// check the layer itself, then one level of sublayers. Class lookup is
    /// dynamic (`AnyClass::get`) so a hypothetical process without QuartzCore
    /// degrades to a no-op instead of a panic.
    ///
    /// SAFETY: caller guarantees `view` is a live NSView borrowed on the main
    /// thread; every message below is a side-effect-free read.
    unsafe fn find_metal_layer(view: *mut AnyObject) -> Option<*mut AnyObject> {
        let metal = AnyClass::get("CAMetalLayer")?;
        unsafe {
            let layer: *mut AnyObject = msg_send![view, layer];
            if layer.is_null() {
                return None;
            }
            let is_metal: bool = msg_send![layer, isKindOfClass: metal];
            if is_metal {
                return Some(layer);
            }
            let subs: *mut AnyObject = msg_send![layer, sublayers];
            if subs.is_null() {
                return None;
            }
            let n: usize = msg_send![subs, count];
            for i in 0..n {
                let l: *mut AnyObject = msg_send![subs, objectAtIndex: i];
                if l.is_null() {
                    continue;
                }
                let is_metal: bool = msg_send![l, isKindOfClass: metal];
                if is_metal {
                    return Some(l);
                }
            }
        }
        None
    }
}

/// M5 TRUE VIBRANCY (macOS): install / update / remove the window-level
/// `NSVisualEffectView` behind the Metal layer and flip the window + Metal-layer
/// opacity, so the GPU's translucent (PostMultiplied) present composites over a
/// live blurred backdrop. Hand-rolled `msg_send` in the `layer_colorspace` /
/// `reduce_motion` style — CoreAnimation + AppKit are already linked, so this
/// adds no dependency (and needs no extra objc2-app-kit feature). All calls run
/// on the main thread (the winit event loop guarantees it) as AppKit requires.
#[cfg(target_os = "macos")]
mod vibrancy {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{class, msg_send};
    use objc2_app_kit::NSColor;
    use objc2_foundation::NSRect;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    use super::layer_colorspace;
    use crate::app_config::BackgroundMaterial;

    // `NSVisualEffectMaterial` raw values (stable ABI — objc2-app-kit 0.2 pins the
    // same constants). Only the three semantic materials the config exposes.
    const MATERIAL_SIDEBAR: isize = 7; // NSVisualEffectMaterialSidebar
    const MATERIAL_HUD: isize = 13; // NSVisualEffectMaterialHUDWindow
    const MATERIAL_UNDER_WINDOW: isize = 21; // NSVisualEffectMaterialUnderWindowBackground
    // `NSVisualEffectBlendingModeBehindWindow` (blur the desktop BEHIND the window).
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
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        // SAFETY: `ns_view` is this window's live NSView (winit owns it for the
        // window's lifetime), borrowed on the main thread as AppKit requires. Every
        // message below is a standard AppKit call; the created view is retained by
        // `addSubview:` (AppKit owns it), and the colours are autoreleased.
        unsafe {
            let view = h.ns_view.as_ptr() as *mut AnyObject;
            let ns_window: *mut AnyObject = msg_send![view, window];
            if ns_window.is_null() {
                return;
            }
            let content: *mut AnyObject = msg_send![ns_window, contentView];
            if content.is_null() {
                return;
            }
            // Idempotent: drop any effect view we installed on a prior call, so
            // this recomputes from a clean slate (material change, or turning
            // translucency off). Identify ours purely by class.
            remove_effect_views(content);

            if translucent {
                // A behind-window blur needs a NON-opaque window with a clear fill,
                // so the desktop (blurred by the effect view) shows through the
                // translucent bg pixels the Metal layer composites.
                let _: () = msg_send![ns_window, setOpaque: false];
                let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
                let _: () = msg_send![ns_window, setBackgroundColor: clear];
                if let Some(mat) = material_value(material) {
                    install_effect_view(content, mat);
                }
                // Let the translucent (PostMultiplied) frame reach the backdrop.
                layer_colorspace::set_metal_opaque(window, false);
            } else {
                // Restore the opaque default: no backdrop, an opaque window painted
                // the theme bg (the seamless-titlebar fill), an opaque Metal layer.
                let _: () = msg_send![ns_window, setOpaque: true];
                let r = f64::from((bg >> 16) & 0xff) / 255.0;
                let g = f64::from((bg >> 8) & 0xff) / 255.0;
                let b = f64::from(bg & 0xff) / 255.0;
                let color: Retained<NSColor> =
                    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0);
                let _: () = msg_send![ns_window, setBackgroundColor: &*color];
                layer_colorspace::set_metal_opaque(window, true);
            }
        }
    }

    /// Remove every `NSVisualEffectView` currently among `content`'s direct
    /// subviews (the ones this module installed). Pointers are gathered FIRST, then
    /// removed, so the live `subviews` array is never mutated mid-walk.
    ///
    /// SAFETY: `content` is a live NSView on the main thread; the reads are
    /// side-effect-free and `removeFromSuperview` is a standard call.
    unsafe fn remove_effect_views(content: *mut AnyObject) {
        let Some(effect_cls) = AnyClass::get("NSVisualEffectView") else {
            return;
        };
        unsafe {
            let subs: *mut AnyObject = msg_send![content, subviews];
            if subs.is_null() {
                return;
            }
            let n: usize = msg_send![subs, count];
            let mut ours: Vec<*mut AnyObject> = Vec::new();
            for i in 0..n {
                let sv: *mut AnyObject = msg_send![subs, objectAtIndex: i];
                if sv.is_null() {
                    continue;
                }
                let is_effect: bool = msg_send![sv, isKindOfClass: effect_cls];
                if is_effect {
                    ours.push(sv);
                }
            }
            for sv in ours {
                let _: () = msg_send![sv, removeFromSuperview];
            }
        }
    }

    /// Create an `NSVisualEffectView` filling `content`'s bounds with the given
    /// `material`, behind-window blending, active state, and add it BELOW the
    /// existing subviews (so the Metal content draws over it).
    ///
    /// SAFETY: `content` is a live NSView on the main thread; the class is looked
    /// up dynamically so a process without AppKit's effect view degrades to a
    /// no-op instead of a panic.
    ///
    /// MEMORY: `alloc`+`initWithFrame:` hands back a +1 create-rule reference that
    /// we own. `addSubview:` takes its OWN retain (the superview's ownership), so
    /// our +1 is surplus and MUST be released or every vibrancy re-apply — GPU
    /// rebuild, opacity slider, material change, per-window attach — leaks a
    /// GPU-backed effect view (MEM-L1). We release right after `addSubview:`; the
    /// view stays alive under the superview's retain. On the nil path we do NOT
    /// release: a failing `init` already releases `self` per Cocoa convention, so
    /// touching it would be an over-release.
    unsafe fn install_effect_view(content: *mut AnyObject, material: isize) {
        let Some(effect_cls) = AnyClass::get("NSVisualEffectView") else {
            return;
        };
        unsafe {
            let bounds: NSRect = msg_send![content, bounds];
            let obj: *mut AnyObject = msg_send![effect_cls, alloc];
            let effect: *mut AnyObject = msg_send![obj, initWithFrame: bounds];
            if effect.is_null() {
                return; // init consumed (released) the alloc — nothing for us to free
            }
            let _: () = msg_send![effect, setMaterial: material];
            let _: () = msg_send![effect, setBlendingMode: BLENDING_BEHIND_WINDOW];
            let _: () = msg_send![effect, setState: STATE_ACTIVE];
            let _: () = msg_send![effect, setAutoresizingMask: AUTORESIZE_WH];
            let nil: *mut AnyObject = std::ptr::null_mut();
            let _: () = msg_send![
                content,
                addSubview: effect,
                positioned: ORDER_BELOW,
                relativeTo: nil,
            ];
            // Balance the create-rule +1 from alloc/init; the superview now owns it.
            let _: () = msg_send![effect, release];
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
        // The kCGColorSpace* constant VALUES equal their names (stable ABI).
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

    /// M3 phase B precedence, totally: an HDR (f16) surface is ALWAYS tagged
    /// extended-linear (its pixels ARE linear — any config tag would mis-render
    /// them); an SDR surface takes the config verbatim.
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
    fn gpu_capture_metadata_matches_the_effective_platform_tag() {
        use aterm_gpu::video_tap::CaptureColorSpace;

        assert_eq!(
            SurfaceColorspace::Srgb.gpu_capture_space(),
            CaptureColorSpace::Srgb
        );
        assert_eq!(
            SurfaceColorspace::ExtendedLinearSrgb.gpu_capture_space(),
            CaptureColorSpace::ExtendedLinearSrgb
        );
        assert_eq!(
            SurfaceColorspace::DisplayP3.gpu_capture_space(),
            CaptureColorSpace::DisplayP3
        );
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

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use winit::window::Theme;

    use super::window_theme_to_winit;
    use crate::app_config::WindowTheme;

    /// `Auto` resets the override (`None`) so the chrome follows the OS preference;
    /// `Light`/`Dark` force the matching winit theme variant. The full mapping is a
    /// total `match`, so this pins every arm.
    #[test]
    fn theme_maps_to_winit_override() {
        assert_eq!(window_theme_to_winit(WindowTheme::Auto), None);
        assert_eq!(
            window_theme_to_winit(WindowTheme::Light),
            Some(Theme::Light)
        );
        assert_eq!(window_theme_to_winit(WindowTheme::Dark), Some(Theme::Dark));
    }
}

/// Opt this application OUT of macOS diacritic press-and-hold, so a HELD key
/// REPEATS instead of opening the accent palette.
///
/// aterm opts every window into IME (`set_ime_allowed(true)` in `app_window.rs`) so
/// CJK/dead-key composition works. In winit that makes `keyDown:` route every key
/// through `interpretKeyEvents` on a view that implements `NSTextInputClient` and
/// `firstRectForCharacterRange:` — which is precisely the configuration macOS reads
/// as "this responder wants press-and-hold". So holding any diacritic-capable
/// letter (a e i o u n c s y z l …) suppresses the autorepeat keyDown stream after
/// the ~500 ms hold threshold and shows the accent palette instead. The 2nd..Nth
/// repeat never reaches `App::on_key` at all: for roughly half the alphabet, held
/// key repeat is not slow, it is ABSENT — and no amount of render or lock tuning
/// can touch it, because the events are never delivered.
///
/// `registerDefaults:` rather than an `Info.plist` key: `NSUserDefaults` does not
/// read the bundle Info.plist, so the plist route that circulates as folklore does
/// nothing. Registration also covers the UN-BUNDLED `aterm` binary, which has no
/// Info.plist to carry a key in the first place.
///
/// It registers a DEFAULT, not a value: a user who genuinely wants the accent
/// palette can still override it with
/// `defaults write com.aterm.aterm ApplePressAndHoldEnabled -bool true`, because an
/// explicitly-written user default outranks a registered one. Must run before the
/// first window is created, while AppKit is still reading the value.
#[cfg(target_os = "macos")]
pub(crate) fn disable_press_and_hold() {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSDictionary, NSNumber, NSString, NSUserDefaults};

    // SAFETY: standard NSUserDefaults registration with an owned dictionary of
    // owned objects; no raw pointers escape and nothing here is thread-affine
    // (`registerDefaults:` is documented as safe from any thread, and this is called
    // from the main thread at startup regardless).
    unsafe {
        let key = NSString::from_str("ApplePressAndHoldEnabled");
        // NSNumber -> NSValue -> NSObject -> AnyObject: three upcasts, because
        // `registerDefaults:` is typed on the erased element. Chained `into_super`
        // rather than a `Retained::cast`, so the upcast stays checked by the
        // class hierarchy instead of asserted.
        let boxed = NSNumber::new_bool(false);
        let value: Retained<AnyObject> =
            Retained::into_super(Retained::into_super(Retained::into_super(boxed)));
        let defaults = NSDictionary::from_vec(&[&*key], vec![value]);
        NSUserDefaults::standardUserDefaults().registerDefaults(&defaults);
    }
}

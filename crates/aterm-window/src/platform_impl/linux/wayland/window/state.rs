// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/window/state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! The state of the window, which is shared with the event-loop.
//!
//! LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, LW.5): the window has
//! no client-side frame. Upstream drew one (sctk-adwaita's, or sctk's `FallbackFrame`) as
//! subsurfaces around the surface whenever the compositor settled on client-side decorations;
//! here the application draws its own header inside the surface and drives the frame's
//! gestures through `drag_window`, `drag_resize_window` and `show_window_menu`. The window
//! geometry is the whole surface, and the negotiated mode is [`WindowState::client_decorations`].
//!
//! LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, LW.7 with winit-core
//! WC.8): the window's objects (its `wl_surface`, `xdg_surface`, `xdg_toplevel`, toplevel
//! decoration, viewport and fractional scale) are first-party `aterm-wayland` proxies this state
//! owns, and the configure is the backend's own [`WindowConfigure`], assembled from the events
//! in the order sctk's `WindowConfigure` was. CUT with sctk, each with no aterm caller: custom
//! cursor images (`set_cursor(Cursor::Custom)` keeps the named cursor and says so), cursor grabs
//! and `set_cursor_position` (no `zwp_pointer_constraints_v1`: `NotSupported`), and the
//! `wayland-kwin-blur` feature (`set_blur` says blur is unavailable, as it did without it).

use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::{Arc, Weak};

use aterm_hash::FxHashSet;
use aterm_wayland::output::Output;
use aterm_wayland::protocols::text_input_unstable_v3::zwp_text_input_v3;
use aterm_wayland::protocols::viewporter::wp_viewport;
use aterm_wayland::protocols::wayland::{wl_compositor, wl_region, wl_surface};
use aterm_wayland::protocols::xdg_decoration_unstable_v1::zxdg_toplevel_decoration_v1;
use aterm_wayland::protocols::xdg_shell::{xdg_surface, xdg_toplevel};
use aterm_wayland::proxy::Proxy;
use aterm_wayland::seat::Seats;
use aterm_wayland::xdg::Shell;
use tracing::{info, warn};

use crate::cursor::CustomCursor as RootCustomCursor;
use crate::dpi::{LogicalPosition, LogicalSize, PhysicalSize, Size};
use crate::error::{ExternalError, NotSupportedError};
use crate::platform::wayland::ClientDecorations;
use crate::platform_impl::wayland::logical_to_physical_rounded;
use crate::platform_impl::wayland::seat::{TextInput, WinitPointer};
use crate::platform_impl::wayland::state::Connection;
use crate::platform_impl::wayland::types::cursor::SelectedCursor;
use crate::platform_impl::wayland::wire::Owned;
use crate::window::{CursorGrabMode, CursorIcon, ImePurpose, ResizeDirection, Theme};

// Minimum window inner size.
const MIN_WINDOW_SIZE: LogicalSize<u32> = LogicalSize::new(2, 1);

aterm_types::bitflags! {
    /// The `xdg_toplevel.state` values a configure carried (sctk's `WindowState`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct XdgWindowState: u16 {
        const MAXIMIZED = 1 << 0;
        const FULLSCREEN = 1 << 1;
        const RESIZING = 1 << 2;
        const ACTIVATED = 1 << 3;
        const TILED_LEFT = 1 << 4;
        const TILED_RIGHT = 1 << 5;
        const TILED_TOP = 1 << 6;
        const TILED_BOTTOM = 1 << 7;
        const SUSPENDED = 1 << 8;
        const TILED = Self::TILED_LEFT.bits()
            | Self::TILED_RIGHT.bits()
            | Self::TILED_TOP.bits()
            | Self::TILED_BOTTOM.bits();
    }
}

impl XdgWindowState {
    /// The states a configure's array names; values this backend does not know (version 7's
    /// constrained edges) are left out, as sctk left them.
    pub fn from_wire(states: &[u32]) -> XdgWindowState {
        use xdg_toplevel::state;
        states.iter().fold(XdgWindowState::empty(), |mut acc, value| {
            let flag = match *value {
                state::MAXIMIZED => XdgWindowState::MAXIMIZED,
                state::FULLSCREEN => XdgWindowState::FULLSCREEN,
                state::RESIZING => XdgWindowState::RESIZING,
                state::ACTIVATED => XdgWindowState::ACTIVATED,
                state::TILED_LEFT => XdgWindowState::TILED_LEFT,
                state::TILED_RIGHT => XdgWindowState::TILED_RIGHT,
                state::TILED_TOP => XdgWindowState::TILED_TOP,
                state::TILED_BOTTOM => XdgWindowState::TILED_BOTTOM,
                state::SUSPENDED => XdgWindowState::SUSPENDED,
                _ => XdgWindowState::empty(),
            };
            acc.insert(flag);
            acc
        })
    }
}

aterm_types::bitflags! {
    /// What the compositor can do for the window (`xdg_toplevel.wm_capabilities`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct WindowManagerCapabilities: u16 {
        const WINDOW_MENU = 1 << 0;
        const MAXIMIZE = 1 << 1;
        const FULLSCREEN = 1 << 2;
        const MINIMIZE = 1 << 3;
    }
}

impl WindowManagerCapabilities {
    /// The capabilities an event's array names; unknown values are left out.
    pub fn from_wire(capabilities: &[u32]) -> WindowManagerCapabilities {
        use xdg_toplevel::wm_capabilities as cap;
        capabilities.iter().fold(WindowManagerCapabilities::empty(), |mut acc, value| {
            let flag = match *value {
                cap::WINDOW_MENU => WindowManagerCapabilities::WINDOW_MENU,
                cap::MAXIMIZE => WindowManagerCapabilities::MAXIMIZE,
                cap::FULLSCREEN => WindowManagerCapabilities::FULLSCREEN,
                cap::MINIMIZE => WindowManagerCapabilities::MINIMIZE,
                _ => WindowManagerCapabilities::empty(),
            };
            acc.insert(flag);
            acc
        })
    }
}

/// Who draws the window's frame, as the configure says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationMode {
    Client,
    Server,
}

/// A configure: the compositor's suggestion, as sctk 0.19.2's `WindowConfigure` assembled it.
/// Each event updates the window's pending one; `xdg_surface.configure` hands a copy to the
/// window, and the pending one carries on (a later configure changes only what it says).
#[derive(Debug, Clone)]
pub struct WindowConfigure {
    /// The suggested size; `None` leaves it to the window.
    pub new_size: (Option<NonZeroU32>, Option<NonZeroU32>),
    /// The compositor's bounds for the window (xdg-shell version 4).
    pub suggested_bounds: Option<(u32, u32)>,
    /// Client until the decoration's configure says otherwise.
    pub decoration_mode: DecorationMode,
    pub state: XdgWindowState,
    /// Every capability until a `wm_capabilities` event (version 5) says which.
    pub capabilities: WindowManagerCapabilities,
}

impl Default for WindowConfigure {
    fn default() -> Self {
        Self {
            new_size: (None, None),
            suggested_bounds: None,
            decoration_mode: DecorationMode::Client,
            state: XdgWindowState::empty(),
            capabilities: WindowManagerCapabilities::all(),
        }
    }
}

impl WindowConfigure {
    #[inline]
    pub fn is_maximized(&self) -> bool {
        self.state.contains(XdgWindowState::MAXIMIZED)
    }

    #[inline]
    pub fn is_fullscreen(&self) -> bool {
        self.state.contains(XdgWindowState::FULLSCREEN)
    }

    #[inline]
    pub fn is_resizing(&self) -> bool {
        self.state.contains(XdgWindowState::RESIZING)
    }

    #[inline]
    pub fn is_tiled(&self) -> bool {
        self.state.intersects(XdgWindowState::TILED)
    }
}

/// The window's objects, in the order they are destroyed (sctk's: the decoration, the
/// toplevel, the xdg surface, then the surface).
pub(crate) struct WindowObjects {
    pub(crate) frame_callback: Option<Owned>,
    pub(crate) attention_token: Option<Owned>,
    pub(crate) viewport: Option<Owned>,
    pub(crate) fractional_scale: Option<Owned>,
    pub(crate) decoration: Option<Owned>,
    pub(crate) toplevel: Owned,
    pub(crate) xdg_surface: Owned,
    pub(crate) surface: Owned,
}

/// The state of the window which is being updated from the [`WinitState`].
///
/// [`WinitState`]: crate::platform_impl::wayland::state::WinitState
pub struct WindowState {
    /// The seats, for the serials and the seat a move, a resize or a menu names.
    seats: Arc<Seats>,

    /// The configure being assembled from the events since the last one.
    pub(crate) pending_configure: WindowConfigure,

    /// The last received configure.
    pub last_configure: Option<WindowConfigure>,

    /// The pointers observed on the window.
    pub pointers: Vec<Weak<WinitPointer>>,

    selected_cursor: SelectedCursor,

    /// Whether the cursor is visible.
    pub cursor_visible: bool,

    /// Theme variant.
    theme: Option<Theme>,

    /// The current window title.
    title: String,

    /// Whether the frame is resizable.
    resizable: bool,

    // NOTE: we can't use simple counter, since it's racy when seat getting destroyed and new
    // is created, since add/removed stuff could be delivered a bit out of order.
    /// Seats that has keyboard focus on that window.
    seat_focus: FxHashSet<u32>,

    /// The scale factor of the window.
    scale_factor: f64,

    /// Whether the window is transparent.
    transparent: bool,

    /// Whether the IME input is allowed for that window.
    ime_allowed: bool,

    /// The current IME purpose.
    ime_purpose: ImePurpose,

    /// The text inputs observed on the window.
    text_inputs: Vec<Weak<TextInput>>,

    /// The outputs the surface is on, as it entered them.
    pub(crate) outputs: Vec<Output>,

    /// The inner size of the window: the whole surface, since the window has no client-side
    /// frame around it (LW.5).
    size: LogicalSize<u32>,

    /// Whether decorations were asked for (the attributes' default, `set_decorations`). Under
    /// server-side decorations `false` asks the compositor for client mode, which takes its
    /// decorations off; under client-side decorations it tells the application not to draw any
    /// ([`Self::client_decorations`]).
    decorate: bool,

    /// Min size.
    min_inner_size: LogicalSize<u32>,
    max_inner_size: Option<LogicalSize<u32>>,
    resize_increments: Option<LogicalSize<u32>>,

    /// The size of the window when no states were applied to it. The primary use for it
    /// is to fallback to original window size, before it was maximized, if the compositor
    /// sends `None` for the new size in the configure.
    stateless_size: LogicalSize<u32>,

    /// Initial window size provided by the user. Removed on the first
    /// configure.
    initial_size: Option<Size>,

    /// The state of the frame callback.
    frame_callback_state: FrameCallbackState,

    /// Whether an attention request waits for its activation token.
    pub(crate) attention_requested: bool,

    /// The window's objects; destroyed before their global and connection keepalives.
    pub(crate) objects: WindowObjects,

    /// Globals this window can still use after the event loop is gone. They drop
    /// after its objects, so xdg_wm_base outlives every child surface.
    compositor: Arc<Owned>,
    _shell: Arc<Shell>,

    /// The connection the objects are on, which outlives them: a window kept past the event
    /// loop keeps it.
    _connection: Arc<Connection>,
}

impl WindowState {
    /// Create new window state over `objects`.
    pub(crate) fn new(
        compositor: Arc<Owned>,
        shell: Arc<Shell>,
        seats: Arc<Seats>,
        connection: Arc<Connection>,
        initial_size: Size,
        objects: WindowObjects,
        theme: Option<Theme>,
    ) -> Self {
        Self {
            compositor,
            _shell: shell,
            seats,
            pending_configure: WindowConfigure::default(),
            selected_cursor: Default::default(),
            cursor_visible: true,
            decorate: true,
            frame_callback_state: FrameCallbackState::None,
            seat_focus: Default::default(),
            ime_allowed: false,
            ime_purpose: ImePurpose::Normal,
            last_configure: None,
            max_inner_size: None,
            min_inner_size: MIN_WINDOW_SIZE,
            resize_increments: None,
            pointers: Default::default(),
            resizable: true,
            scale_factor: 1.,
            size: initial_size.to_logical(1.),
            stateless_size: initial_size.to_logical(1.),
            initial_size: Some(initial_size),
            text_inputs: Vec::new(),
            outputs: Vec::new(),
            theme,
            title: String::default(),
            transparent: false,
            attention_requested: false,
            objects,
            _connection: connection,
        }
    }

    /// The `wl_compositor`, for regions.
    #[inline]
    pub(crate) fn compositor(&self) -> Proxy {
        self.compositor.proxy()
    }

    /// The window's `wl_surface`.
    #[inline]
    pub(crate) fn surface(&self) -> Proxy {
        self.objects.surface.proxy()
    }

    /// The window's `xdg_toplevel`.
    #[inline]
    pub(crate) fn xdg_toplevel(&self) -> Proxy {
        self.objects.toplevel.proxy()
    }

    /// Apply closure on the given pointer.
    fn apply_on_pointer<F: FnMut(&WinitPointer)>(&self, mut callback: F) {
        self.pointers.iter().filter_map(Weak::upgrade).for_each(|pointer| callback(&pointer))
    }

    /// Get the current state of the frame callback.
    pub fn frame_callback_state(&self) -> FrameCallbackState {
        self.frame_callback_state
    }

    /// The frame callback was received, but not yet sent to the user.
    pub fn frame_callback_received(&mut self) {
        self.frame_callback_state = FrameCallbackState::Received;
        // The callback ended with its event: its proxy goes now.
        self.objects.frame_callback = None;
    }

    /// Reset the frame callbacks state.
    pub fn frame_callback_reset(&mut self) {
        self.frame_callback_state = FrameCallbackState::None;
    }

    /// Request a frame callback if we don't have one for this window in flight.
    pub fn request_frame_callback(&mut self, make: impl FnOnce(Proxy) -> Option<Owned>) {
        match self.frame_callback_state {
            FrameCallbackState::None | FrameCallbackState::Received => {
                self.frame_callback_state = FrameCallbackState::Requested;
                self.objects.frame_callback = make(self.surface());
            },
            FrameCallbackState::Requested => (),
        }
    }

    pub fn configure(&mut self, configure: WindowConfigure) -> bool {
        // NOTE: when using fractional scaling or wl_compositor@v6 the scaling
        // should be delivered before the first configure, thus apply it to
        // properly scale the physical sizes provided by the users.
        if let Some(initial_size) = self.initial_size.take() {
            self.size = initial_size.to_logical(self.scale_factor());
            self.stateless_size = self.size;
        }

        let stateless = Self::is_stateless(&configure);

        // LOCAL PATCH (aterm), LW.5: no frame borders to subtract; the configure's size is the
        // surface's.
        let (mut new_size, constrain) = match configure.new_size {
            (Some(width), Some(height)) => ((width.get(), height.get()).into(), false),
            _ if stateless => (self.stateless_size, true),
            _ => (self.size, true),
        };

        // Apply configure bounds only when compositor let the user decide what size to pick.
        if constrain {
            let bounds = self.inner_size_bounds(&configure);
            new_size.width =
                bounds.0.map(|bound_w| new_size.width.min(bound_w.get())).unwrap_or(new_size.width);
            new_size.height = bounds
                .1
                .map(|bound_h| new_size.height.min(bound_h.get()))
                .unwrap_or(new_size.height);
        }

        // Apply size increments.
        //
        // We conditionally apply increments to avoid conflicts with the compositor's layout rules:
        // 1. If the window is floating (constrain == true), we snap to increments to ensure the
        //    app's grid alignment.
        // 2. If the user is interactively resizing (is_resizing), we snap the size to provide
        //    feedback.
        //
        // However, we MUST NOT snap if the compositor enforces a specific size (constrain == false,
        // or states like Maximized/Tiled). Snapping in these cases (e.g. corner tiling) would
        // shrink the window below the allocated area, creating visible gaps between valid
        // windows or screen edges.
        if (constrain || configure.is_resizing())
            && !configure.is_maximized()
            && !configure.is_fullscreen()
            && !configure.is_tiled()
        {
            if let Some(increments) = self.resize_increments {
                // We use min size as a base size for the increments, similar to how X11 does it.
                //
                // This ensures that we can always reach the min size and the increments are
                // calculated from it.
                let (delta_width, delta_height) = (
                    new_size.width.saturating_sub(self.min_inner_size.width),
                    new_size.height.saturating_sub(self.min_inner_size.height),
                );

                let width =
                    self.min_inner_size.width + (delta_width / increments.width) * increments.width;
                let height = self.min_inner_size.height
                    + (delta_height / increments.height) * increments.height;

                new_size = (width, height).into();
            }
        }

        let new_state = configure.state;
        let old_state = self.last_configure.as_ref().map(|configure| configure.state);

        // LOCAL PATCH (aterm), LW.5: a change of decoration mode or of the compositor's
        // capabilities also counts, so the application hears it (as a `Resized`, possibly of
        // the same size) and re-reads `client_decorations` before it draws its header again.
        let decorations_moved = self.last_configure.as_ref().is_some_and(|last| {
            last.decoration_mode != configure.decoration_mode
                || last.capabilities != configure.capabilities
        });
        let state_change_requires_resize = decorations_moved
            || old_state
                .map(|old_state| {
                    !XdgWindowState::from_bits_retain(old_state.bits() ^ new_state.bits())
                        .difference(XdgWindowState::ACTIVATED | XdgWindowState::SUSPENDED)
                        .is_empty()
                })
                // NOTE: `None` is present for the initial configure, thus we must always resize.
                .unwrap_or(true);

        // NOTE: Set the configure before doing a resize, since we query it during it.
        self.last_configure = Some(configure);

        if state_change_requires_resize || new_size != self.inner_size() {
            self.resize(new_size);
            true
        } else {
            false
        }
    }

    /// Compute the bounds for the inner size of the surface.
    fn inner_size_bounds(
        &self,
        configure: &WindowConfigure,
    ) -> (Option<NonZeroU32>, Option<NonZeroU32>) {
        match configure.suggested_bounds {
            Some((width, height)) => (NonZeroU32::new(width), NonZeroU32::new(height)),
            None => (None, None),
        }
    }

    #[inline]
    fn is_stateless(configure: &WindowConfigure) -> bool {
        !(configure.is_maximized() || configure.is_fullscreen() || configure.is_tiled())
    }

    /// Start interacting drag resize.
    pub fn drag_resize_window(&self, direction: ResizeDirection) -> Result<(), ExternalError> {
        let xdg_toplevel = self.xdg_toplevel();

        // TODO(kchibisov) handle touch serials.
        self.apply_on_pointer(|pointer| {
            self.seats.with_seat(pointer.seat(), |seat| {
                let serial = seat.pointer_button_serial.unwrap_or_default();
                // SAFETY: this window's live toplevel and the seat lent live.
                unsafe {
                    xdg_toplevel::resize(xdg_toplevel, seat.seat, serial, resize_edge(direction))
                };
            });
        });

        Ok(())
    }

    /// Start the window drag.
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        let xdg_toplevel = self.xdg_toplevel();
        // TODO(kchibisov) handle touch serials.
        self.apply_on_pointer(|pointer| {
            self.seats.with_seat(pointer.seat(), |seat| {
                let serial = seat.pointer_button_serial.unwrap_or_default();
                // SAFETY: this window's live toplevel and the seat lent live.
                unsafe { xdg_toplevel::r#move(xdg_toplevel, seat.seat, serial) };
            });
        });

        Ok(())
    }

    /// Get the stored resizable state.
    #[inline]
    pub fn resizable(&self) -> bool {
        self.resizable
    }

    /// Set the resizable state on the window.
    ///
    /// Returns `true` when the state was applied.
    #[inline]
    pub fn set_resizable(&mut self, resizable: bool) -> bool {
        if self.resizable == resizable {
            return false;
        }

        self.resizable = resizable;
        if resizable {
            // Restore min/max sizes of the window.
            self.reload_min_max_hints();
        } else {
            self.set_min_inner_size(Some(self.size));
            self.set_max_inner_size(Some(self.size));
        }

        true
    }

    /// Whether the window is focused by any seat.
    #[inline]
    pub fn has_focus(&self) -> bool {
        !self.seat_focus.is_empty()
    }

    /// Whether the IME is allowed.
    #[inline]
    pub fn ime_allowed(&self) -> bool {
        self.ime_allowed
    }

    /// Get the size of the window.
    #[inline]
    pub fn inner_size(&self) -> LogicalSize<u32> {
        self.size
    }

    /// Whether the window received initial configure event from the compositor.
    #[inline]
    pub fn is_configured(&self) -> bool {
        self.last_configure.is_some()
    }

    /// Whether the window system decorates the window: the compositor granted server-side
    /// decorations (and, before the first configure, the request for them stands).
    ///
    /// LOCAL PATCH (aterm), LW.5: `false` under client-side decorations, where upstream answered
    /// `true` while its own frame showed. This backend draws none, so a window in client mode is
    /// undecorated until the application draws its own ([`Self::client_decorations`]).
    #[inline]
    pub fn is_decorated(&self) -> bool {
        match self.last_configure.as_ref() {
            Some(configure) => configure.decoration_mode == DecorationMode::Server,
            None => self.decorate,
        }
    }

    /// The decorations the application must draw: `Some` once the compositor's configure has
    /// settled on client-side decorations (it offers no xdg-decoration manager, or chose client
    /// mode) while decorations are asked for, with what the compositor can do for the header's
    /// buttons and menu; `None` under server-side decorations, with decorations off, and before
    /// the first configure. A change arrives as a `Resized` (see [`Self::configure`]).
    pub fn client_decorations(&self) -> Option<ClientDecorations> {
        let configure = self.last_configure.as_ref()?;
        if !self.decorate || configure.decoration_mode != DecorationMode::Client {
            return None;
        }
        let can = |capability| configure.capabilities.contains(capability);
        Some(ClientDecorations {
            window_menu: can(WindowManagerCapabilities::WINDOW_MENU),
            maximize: can(WindowManagerCapabilities::MAXIMIZE),
            minimize: can(WindowManagerCapabilities::MINIMIZE),
            maximized: configure.is_maximized(),
            fullscreen: configure.is_fullscreen(),
            tiled: configure.is_tiled(),
            resizable: self.resizable,
        })
    }

    /// Get the outer size of the window: the inner size, as the window has no frame of its own.
    #[inline]
    pub fn outer_size(&self) -> LogicalSize<u32> {
        self.size
    }

    /// Register pointer on the top-level.
    pub fn pointer_entered(&mut self, added: Weak<WinitPointer>) {
        self.pointers.push(added);
        self.reload_cursor_style();
    }

    /// Pointer has left the top-level.
    pub fn pointer_left(&mut self, removed: &Weak<WinitPointer>) {
        self.pointers.retain(|pointer| pointer.strong_count() > 0 && !pointer.ptr_eq(removed));
    }

    /// Reload the cursor style on the given window.
    pub fn reload_cursor_style(&mut self) {
        if self.cursor_visible {
            match &self.selected_cursor {
                SelectedCursor::Named(icon) => self.set_cursor(*icon),
            }
        } else {
            self.set_cursor_visible(self.cursor_visible);
        }
    }

    /// Reissue the transparency hint to the compositor.
    pub fn reload_transparency_hint(&self) {
        let surface = self.surface();

        if self.transparent {
            // SAFETY: this window's live surface.
            unsafe { wl_surface::set_opaque_region(surface, None) };
        // SAFETY: the live compositor.
        } else if let Some(region) =
            unsafe { wl_compositor::create_region(self.compositor.proxy()) }
        {
            // SAFETY: the region made above, destroyed once here: the surface copies its
            // contents at `set_opaque_region` (it is double-buffered state, not a reference).
            unsafe {
                wl_region::add(region, 0, 0, i32::MAX, i32::MAX);
                wl_surface::set_opaque_region(surface, Some(region));
                wl_region::destroy(region);
            }
        } else {
            warn!("Failed to mark window opaque.");
        }
    }

    /// Try to resize the window when the user can do so.
    pub fn request_inner_size(&mut self, inner_size: Size) -> PhysicalSize<u32> {
        if self.last_configure.as_ref().map(Self::is_stateless).unwrap_or(true) {
            self.resize(inner_size.to_logical(self.scale_factor()))
        }

        logical_to_physical_rounded(self.inner_size(), self.scale_factor())
    }

    /// Resize the window to the new inner size.
    fn resize(&mut self, inner_size: LogicalSize<u32>) {
        self.size = inner_size;

        // Update the stateless size.
        if Some(true) == self.last_configure.as_ref().map(Self::is_stateless) {
            self.stateless_size = inner_size;
        }

        // Reload the hint.
        self.reload_transparency_hint();

        // Set the window geometry: the whole surface (LW.5: no frame around it).
        // SAFETY: this window's live xdg_surface.
        unsafe {
            xdg_surface::set_window_geometry(
                self.objects.xdg_surface.proxy(),
                0,
                0,
                self.size.width as i32,
                self.size.height as i32,
            )
        };

        // Update the target viewport, this is used if and only if fractional scaling is in use.
        if let Some(viewport) = self.objects.viewport.as_ref() {
            // SAFETY: this window's live viewport.
            unsafe {
                wp_viewport::set_destination(
                    viewport.proxy(),
                    self.size.width as _,
                    self.size.height as _,
                )
            };
        }
    }

    /// Get the scale factor of the window.
    #[inline]
    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    /// Set the cursor icon.
    pub fn set_cursor(&mut self, cursor_icon: CursorIcon) {
        self.selected_cursor = SelectedCursor::Named(cursor_icon);

        if !self.cursor_visible {
            return;
        }

        self.apply_on_pointer(|pointer| {
            if !pointer.set_cursor(&self.seats, cursor_icon) {
                warn!("Failed to set cursor to {:?}", cursor_icon);
            }
        })
    }

    /// Set the custom cursor icon.
    ///
    /// LOCAL PATCH (aterm), LW.7: custom cursor images are not drawn on Wayland (no aterm
    /// caller; their shm pool left with sctk). The named cursor stays.
    pub(crate) fn set_custom_cursor(&mut self, _cursor: RootCustomCursor) {
        warn!("custom cursors are not supported on Wayland; the named cursor stays");
    }

    /// Set the resize increments of the window.
    pub fn set_resize_increments(&mut self, increments: Option<LogicalSize<u32>>) {
        self.resize_increments = increments;
        // NOTE: We don't update the window size here, because it will be done on the next resize
        // or configure event.
    }

    /// Get the resize increments of the window.
    pub fn resize_increments(&self) -> Option<LogicalSize<u32>> {
        self.resize_increments
    }

    /// Set maximum inner window size.
    pub fn set_min_inner_size(&mut self, size: Option<LogicalSize<u32>>) {
        // Ensure that the window has the right minimum size.
        let mut size = size.unwrap_or(MIN_WINDOW_SIZE);
        size.width = size.width.max(MIN_WINDOW_SIZE.width);
        size.height = size.height.max(MIN_WINDOW_SIZE.height);

        self.min_inner_size = size;
        // SAFETY: this window's live toplevel.
        unsafe {
            xdg_toplevel::set_min_size(self.xdg_toplevel(), size.width as i32, size.height as i32)
        };
    }

    /// Set maximum inner window size.
    pub fn set_max_inner_size(&mut self, size: Option<LogicalSize<u32>>) {
        self.max_inner_size = size;
        let size = size.unwrap_or(LogicalSize::new(0, 0));
        // SAFETY: this window's live toplevel.
        unsafe {
            xdg_toplevel::set_max_size(self.xdg_toplevel(), size.width as i32, size.height as i32)
        };
    }

    /// Record the preferred theme. LW.5: there is no frame to recolour; the value is only
    /// answered back by [`Self::theme`].
    pub fn set_theme(&mut self, theme: Option<Theme>) {
        self.theme = theme;
    }

    /// The preferred theme the window was given.
    #[inline]
    pub fn theme(&self) -> Option<Theme> {
        self.theme
    }

    /// Set the cursor grabbing state on the top-level.
    ///
    /// LOCAL PATCH (aterm), LW.7: no `zwp_pointer_constraints_v1` (no aterm caller), so only
    /// [`CursorGrabMode::None`] is supported, as upstream answered on a compositor without it.
    pub fn set_cursor_grab(&mut self, mode: CursorGrabMode) -> Result<(), ExternalError> {
        match mode {
            CursorGrabMode::None => Ok(()),
            _ => Err(ExternalError::NotSupported(NotSupportedError::new())),
        }
    }

    /// Reload the hints for minimum and maximum sizes.
    pub fn reload_min_max_hints(&mut self) {
        self.set_min_inner_size(Some(self.min_inner_size));
        self.set_max_inner_size(self.max_inner_size);
    }

    pub fn show_window_menu(&self, position: LogicalPosition<u32>) {
        let xdg_toplevel = self.xdg_toplevel();
        // TODO(kchibisov) handle touch serials.
        self.apply_on_pointer(|pointer| {
            self.seats.with_seat(pointer.seat(), |seat| {
                let serial = seat.pointer_button_serial.unwrap_or_default();
                // SAFETY: this window's live toplevel and the seat lent live.
                unsafe {
                    xdg_toplevel::show_window_menu(
                        xdg_toplevel,
                        seat.seat,
                        serial,
                        position.x as i32,
                        position.y as i32,
                    )
                };
            });
        });
    }

    /// Set the position of the cursor: not supported (LW.7: no pointer constraints).
    pub fn set_cursor_position(
        &self,
        _position: LogicalPosition<f64>,
    ) -> Result<(), ExternalError> {
        Err(ExternalError::NotSupported(NotSupportedError::new()))
    }

    /// Set the visibility state of the cursor.
    pub fn set_cursor_visible(&mut self, cursor_visible: bool) {
        self.cursor_visible = cursor_visible;

        if self.cursor_visible {
            match &self.selected_cursor {
                SelectedCursor::Named(icon) => self.set_cursor(*icon),
            }
        } else {
            self.apply_on_pointer(|pointer| pointer.hide_cursor(&self.seats));
        }
    }

    /// Whether show or hide client side decorations.
    #[inline]
    pub fn set_decorate(&mut self, decorate: bool) {
        if decorate == self.decorate {
            return;
        }

        self.decorate = decorate;

        match self.last_configure.as_ref().map(|configure| configure.decoration_mode) {
            Some(DecorationMode::Server) if !self.decorate => {
                // To disable decorations we should request client mode, which has no frame.
                self.request_decoration_mode(zxdg_toplevel_decoration_v1::mode::CLIENT_SIDE)
            },
            _ if self.decorate => {
                self.request_decoration_mode(zxdg_toplevel_decoration_v1::mode::SERVER_SIDE)
            },
            _ => (),
        }
    }

    /// Ask the compositor for a decoration mode, where it negotiates one.
    fn request_decoration_mode(&self, mode: u32) {
        if let Some(decoration) = self.objects.decoration.as_ref() {
            // SAFETY: this window's live toplevel decoration.
            unsafe { zxdg_toplevel_decoration_v1::set_mode(decoration.proxy(), mode) };
        }
    }

    /// Add seat focus for the window.
    #[inline]
    pub fn add_seat_focus(&mut self, seat: u32) {
        self.seat_focus.insert(seat);
    }

    /// Remove seat focus from the window.
    #[inline]
    pub fn remove_seat_focus(&mut self, seat: u32) {
        self.seat_focus.remove(&seat);
    }

    /// Returns `true` if the requested state was applied.
    pub fn set_ime_allowed(&mut self, allowed: bool) -> bool {
        self.ime_allowed = allowed;

        let mut applied = false;
        for text_input in self.text_inputs.iter().filter_map(Weak::upgrade) {
            applied = true;
            if allowed {
                text_input.enable();
                text_input.set_content_type_by_purpose(self.ime_purpose);
            } else {
                text_input.disable();
            }
            text_input.commit();
        }

        applied
    }

    /// Set the IME position.
    pub fn set_ime_cursor_area(&self, position: LogicalPosition<u32>, size: LogicalSize<u32>) {
        // FIXME: This won't fly unless user will have a way to request IME window per seat, since
        // the ime windows will be overlapping, but winit doesn't expose API to specify for
        // which seat we're setting IME position.
        let (x, y) = (position.x as i32, position.y as i32);
        let (width, height) = (size.width as i32, size.height as i32);
        for text_input in self.text_inputs.iter().filter_map(Weak::upgrade) {
            text_input.request(|proxy| {
                // SAFETY: the seat's live text input, held by the upgrade.
                unsafe { zwp_text_input_v3::set_cursor_rectangle(proxy, x, y, width, height) }
            });
            text_input.commit();
        }
    }

    /// Set the IME purpose.
    pub fn set_ime_purpose(&mut self, purpose: ImePurpose) {
        self.ime_purpose = purpose;

        for text_input in self.text_inputs.iter().filter_map(Weak::upgrade) {
            text_input.set_content_type_by_purpose(purpose);
            text_input.commit();
        }
    }

    /// Get the IME purpose.
    pub fn ime_purpose(&self) -> ImePurpose {
        self.ime_purpose
    }

    /// Set the scale factor for the given window.
    #[inline]
    pub fn set_scale_factor(&mut self, scale_factor: f64) {
        self.scale_factor = scale_factor;

        // NOTE: When fractional scaling is not used update the buffer scale.
        let surface = self.surface();
        // SAFETY: this window's live surface; `set_buffer_scale` is version 3's.
        if self.objects.fractional_scale.is_none() && unsafe { surface.version() } >= 3 {
            // SAFETY: as above.
            unsafe { wl_surface::set_buffer_scale(surface, self.scale_factor as _) };
        }
    }

    /// Make window background blurred.
    ///
    /// LOCAL PATCH (aterm), LW.7: the `wayland-kwin-blur` feature left with
    /// wayland-protocols-plasma, so blur is never available, as it was without the feature.
    #[inline]
    pub fn set_blur(&mut self, blurred: bool) {
        if blurred {
            info!("Blur manager unavailable, unable to change blur")
        }
    }

    /// Set the window title to a new value.
    ///
    /// This will automatically truncate the title to something meaningful.
    pub fn set_title(&mut self, mut title: String) {
        // Truncate the title to at most 1024 bytes, so that it does not blow up the protocol
        // messages
        if title.len() > 1024 {
            let mut new_len = 1024;
            while !title.is_char_boundary(new_len) {
                new_len -= 1;
            }
            title.truncate(new_len);
        }

        // SAFETY: this window's live toplevel.
        unsafe { xdg_toplevel::set_title(self.xdg_toplevel(), &protocol_string(&title)) };
        self.title = title;
    }

    /// Set the window's app id (`xdg_toplevel.set_app_id`).
    pub fn set_app_id(&self, app_id: &str) {
        // SAFETY: this window's live toplevel.
        unsafe { xdg_toplevel::set_app_id(self.xdg_toplevel(), &protocol_string(app_id)) };
    }

    /// Mark the window as transparent.
    #[inline]
    pub fn set_transparent(&mut self, transparent: bool) {
        self.transparent = transparent;
        self.reload_transparency_hint();
    }

    /// The text inputs observed on the window (`retire_tests`).
    #[cfg(test)]
    pub(crate) fn text_inputs(&self) -> &[Weak<TextInput>] {
        &self.text_inputs
    }

    /// Register text input on the top-level.
    #[inline]
    pub fn text_input_entered(&mut self, text_input: &Arc<TextInput>) {
        let weak = Arc::downgrade(text_input);
        if !self.text_inputs.iter().any(|t| t.ptr_eq(&weak)) {
            self.text_inputs.push(weak);
        }
    }

    /// The text input left the top-level.
    #[inline]
    pub fn text_input_left(&mut self, text_input: &Arc<TextInput>) {
        let weak = Arc::downgrade(text_input);
        self.text_inputs.retain(|t| t.strong_count() > 0 && !t.ptr_eq(&weak));
    }

    /// Get the cached title.
    #[inline]
    pub fn title(&self) -> &str {
        &self.title
    }
}

/// A Rust string as a protocol string: up to its first NUL, which a protocol string cannot
/// carry (wayland-client would have refused the whole string).
pub(crate) fn protocol_string(s: &str) -> CString {
    let end = s.find('\0').unwrap_or(s.len());
    CString::new(&s[..end]).unwrap_or_default()
}

/// The state of the frame callback.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCallbackState {
    /// No frame callback was requested.
    #[default]
    None,
    /// The frame callback was requested, but not yet arrived, the redraw events are throttled.
    Requested,
    /// The callback was marked as done, and user could receive redraw requested
    Received,
}

/// `xdg_toplevel.resize`'s edge for `direction`.
fn resize_edge(direction: ResizeDirection) -> u32 {
    use xdg_toplevel::resize_edge as edge;
    match direction {
        ResizeDirection::North => edge::TOP,
        ResizeDirection::West => edge::LEFT,
        ResizeDirection::NorthWest => edge::TOP_LEFT,
        ResizeDirection::NorthEast => edge::TOP_RIGHT,
        ResizeDirection::East => edge::RIGHT,
        ResizeDirection::SouthWest => edge::BOTTOM_LEFT,
        ResizeDirection::SouthEast => edge::BOTTOM_RIGHT,
        ResizeDirection::South => edge::BOTTOM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configure_s_states_and_capabilities_are_read_and_unknown_values_left_out() {
        use xdg_toplevel::{state, wm_capabilities as cap};
        let states = XdgWindowState::from_wire(&[
            state::MAXIMIZED,
            state::ACTIVATED,
            state::TILED_TOP,
            state::CONSTRAINED_LEFT,
            0xffff,
        ]);
        assert_eq!(
            states,
            XdgWindowState::MAXIMIZED | XdgWindowState::ACTIVATED | XdgWindowState::TILED_TOP
        );
        let configure = WindowConfigure { state: states, ..WindowConfigure::default() };
        assert!(configure.is_maximized() && configure.is_tiled() && !configure.is_fullscreen());
        assert_eq!(
            WindowManagerCapabilities::from_wire(&[cap::MINIMIZE, cap::WINDOW_MENU, 9]),
            WindowManagerCapabilities::MINIMIZE | WindowManagerCapabilities::WINDOW_MENU
        );
        // Before a `wm_capabilities` event every capability is assumed, as sctk assumed it.
        assert!(WindowConfigure::default().capabilities.is_all());
    }

    #[test]
    fn every_resize_direction_names_its_edge() {
        use xdg_toplevel::resize_edge as edge;
        let all = [
            (ResizeDirection::North, edge::TOP),
            (ResizeDirection::South, edge::BOTTOM),
            (ResizeDirection::West, edge::LEFT),
            (ResizeDirection::East, edge::RIGHT),
            (ResizeDirection::NorthWest, edge::TOP_LEFT),
            (ResizeDirection::NorthEast, edge::TOP_RIGHT),
            (ResizeDirection::SouthWest, edge::BOTTOM_LEFT),
            (ResizeDirection::SouthEast, edge::BOTTOM_RIGHT),
        ];
        for (direction, want) in all {
            assert_eq!(resize_edge(direction), want, "{direction:?}");
        }
    }

    #[test]
    fn a_protocol_string_stops_at_a_nul() {
        assert_eq!(protocol_string("aterm").as_bytes(), b"aterm");
        assert_eq!(protocol_string("a\0b").as_bytes(), b"a");
    }
}

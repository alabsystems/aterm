// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/window/state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Wayland window state shared with the event loop.
//!
//! LOCAL PATCH (aterm): no client-side frame. Upstream drew sctk-adwaita's or sctk's fallback frame
//! around the surface under client-side decorations; here the application draws its own header
//! inside the surface and drives its gestures through `drag_window`, `drag_resize_window` and
//! `show_window_menu`, and only sctk-adwaita's shadow margin ([`super::frame::ShadowFrame`]) is
//! drawn outside it. The window's objects are first-party `aterm-wayland` proxies and the configure
//! is the backend's own [`WindowConfigure`], where upstream used smithay-client-toolkit; custom
//! cursors, cursor grabs, `set_cursor_position` and KWin blur are gone.

use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::{Arc, Weak};

use aterm_hash::FxHashSet;
use aterm_wayland::output::Output;
use aterm_wayland::protocols::text_input_unstable_v3::zwp_text_input_v3;
use aterm_wayland::protocols::viewporter::wp_viewport;
use aterm_wayland::protocols::wayland::{wl_compositor, wl_region, wl_surface};
use aterm_wayland::protocols::xdg_shell::{xdg_surface, xdg_toplevel};
use aterm_wayland::proxy::Proxy;
use aterm_wayland::seat::Seats;
use aterm_wayland::xdg::Shell;
use tracing::warn;

use crate::dpi::{LogicalPosition, LogicalSize, PhysicalSize, Size};
use crate::error::ExternalError;
use crate::frame_shadow::{self, Look, Part};
use crate::platform::wayland::ClientDecorations;
use crate::platform_impl::wayland::logical_to_physical_rounded;
use crate::platform_impl::wayland::seat::{TextInput, WinitPointer};
use crate::platform_impl::wayland::state::Connection;
use crate::platform_impl::wayland::wire::Owned;
use crate::platform_impl::wayland::WindowId;
use crate::window::{CursorIcon, ImePurpose, ResizeDirection, Theme};

use super::frame::{FrameGlobals, ShadowFrame};

const MIN_WINDOW_SIZE: LogicalSize<u32> = LogicalSize::new(2, 1);

aterm_types::bitflags! {
    /// `xdg_toplevel.state` flags carried by a configure event.
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
    /// Parses `xdg_toplevel.state` flags from wire values, ignoring unknown states.
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
    /// Compositor capabilities advertised via `xdg_toplevel.wm_capabilities`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct WindowManagerCapabilities: u16 {
        const WINDOW_MENU = 1 << 0;
        const MAXIMIZE = 1 << 1;
        const FULLSCREEN = 1 << 2;
        const MINIMIZE = 1 << 3;
    }
}

impl WindowManagerCapabilities {
    /// Parses compositor capabilities from wire values, ignoring unknown capabilities.
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

/// Window decoration mode reported by a configure event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationMode {
    Client,
    Server,
}

/// Pending or applied compositor configure state.
#[derive(Debug, Clone)]
pub struct WindowConfigure {
    /// Suggested surface size, or `None` when left to the client.
    pub new_size: (Option<NonZeroU32>, Option<NonZeroU32>),
    /// Compositor bounds for the window (`xdg-shell` v4).
    pub suggested_bounds: Option<(u32, u32)>,
    /// Negotiated decoration mode.
    pub decoration_mode: DecorationMode,
    pub state: XdgWindowState,
    /// Advertised window manager capabilities.
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

/// Wayland protocol objects owned by the window in destruction order.
pub(crate) struct WindowObjects {
    pub(crate) attention_token: Option<Owned>,
    pub(crate) viewport: Option<Owned>,
    pub(crate) fractional_scale: Option<Owned>,
    /// Optional `zxdg_toplevel_decoration_v1` handle.
    pub(crate) _decoration: Option<Owned>,
    pub(crate) toplevel: Owned,
    pub(crate) xdg_surface: Owned,
    pub(crate) surface: Owned,
}

/// Wayland window state updated from `WinitState`.
pub struct WindowState {
    /// The seats, for the serials and the seat a move, a resize or a menu names.
    seats: Arc<Seats>,

    /// Configure state being assembled from events since the last configure.
    pub(crate) pending_configure: WindowConfigure,

    /// Last applied configure event.
    pub last_configure: Option<WindowConfigure>,

    /// Pointers currently observed on the window.
    pub pointers: Vec<Weak<WinitPointer>>,

    /// The named cursor the window shows.
    selected_cursor: CursorIcon,

    /// Theme variant.
    theme: Option<Theme>,

    /// Seats that has keyboard focus on that window.
    seat_focus: FxHashSet<u32>,

    /// The scale factor of the window.
    scale_factor: f64,

    /// Whether the IME input is allowed for that window.
    ime_allowed: bool,

    /// The current IME purpose.
    ime_purpose: ImePurpose,

    /// The text inputs observed on the window.
    text_inputs: Vec<Weak<TextInput>>,

    /// Outputs the surface currently overlaps.
    pub(crate) outputs: Vec<Output>,

    /// The inner size of the window: the whole surface, since the window has no client-side
    /// frame around it (LW.5).
    size: LogicalSize<u32>,

    /// Min size.
    min_inner_size: LogicalSize<u32>,
    resize_increments: Option<LogicalSize<u32>>,

    /// The size of the window when no states were applied to it. The primary use for it
    /// is to fallback to original window size, before it was maximized, if the compositor
    /// sends `None` for the new size in the configure.
    stateless_size: LogicalSize<u32>,

    /// Initial window size provided by the user. Removed on the first
    /// configure.
    initial_size: Option<Size>,

    /// Whether an attention request awaits its activation token.
    pub(crate) attention_requested: bool,

    /// The shadow margin's border parts, made at the first client-decorated configure; destroyed
    /// before the window's own objects (they are subsurfaces of its surface).
    frame: Option<ShadowFrame>,

    /// The globals the frame is made from; `None` without a `wl_subcompositor`.
    frame_globals: Option<FrameGlobals>,

    /// Protocol objects owned by the window.
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
    /// Creates a new [`WindowState`] over `objects`.
    pub(crate) fn new(
        compositor: Arc<Owned>,
        shell: Arc<Shell>,
        seats: Arc<Seats>,
        connection: Arc<Connection>,
        initial_size: Size,
        objects: WindowObjects,
        frame_globals: Option<FrameGlobals>,
    ) -> Self {
        Self {
            compositor,
            _shell: shell,
            seats,
            pending_configure: WindowConfigure::default(),
            selected_cursor: Default::default(),
            seat_focus: Default::default(),
            ime_allowed: false,
            ime_purpose: ImePurpose::Normal,
            last_configure: None,
            min_inner_size: MIN_WINDOW_SIZE,
            resize_increments: None,
            pointers: Default::default(),
            scale_factor: 1.,
            size: initial_size.to_logical(1.),
            stateless_size: initial_size.to_logical(1.),
            initial_size: Some(initial_size),
            text_inputs: Vec::new(),
            outputs: Vec::new(),
            theme: None,
            attention_requested: false,
            frame: None,
            frame_globals,
            objects,
            _connection: connection,
        }
    }

    /// Returns the window's `wl_surface` proxy.
    #[inline]
    pub(crate) fn surface(&self) -> Proxy {
        self.objects.surface.proxy()
    }

    /// Returns the window's `xdg_toplevel` proxy.
    #[inline]
    pub(crate) fn xdg_toplevel(&self) -> Proxy {
        self.objects.toplevel.proxy()
    }

    /// Apply closure on the given pointer.
    fn apply_on_pointer<F: FnMut(&WinitPointer)>(&self, mut callback: F) {
        self.pointers.iter().filter_map(Weak::upgrade).for_each(|pointer| callback(&pointer))
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

        // LOCAL PATCH (aterm): no frame borders to subtract; the configure's size is the surface's.
        let (mut new_size, constrain) = match configure.new_size {
            (Some(width), Some(height)) => ((width.get(), height.get()).into(), false),
            _ if stateless => (self.stateless_size, true),
            _ => (self.size, true),
        };

        if constrain {
            let bounds = self.inner_size_bounds(&configure);
            new_size.width =
                bounds.0.map(|bound_w| new_size.width.min(bound_w.get())).unwrap_or(new_size.width);
            new_size.height = bounds
                .1
                .map(|bound_h| new_size.height.min(bound_h.get()))
                .unwrap_or(new_size.height);
        }

        if (constrain || configure.is_resizing())
            && !configure.is_maximized()
            && !configure.is_fullscreen()
            && !configure.is_tiled()
        {
            if let Some(increments) = self.resize_increments {
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

        // LOCAL PATCH (aterm): a change of decoration mode or compositor capabilities counts too,
        // so the application hears it (a `Resized`, perhaps of the same size) and re-reads
        // `client_decorations`.
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
                .unwrap_or(true);

        self.last_configure = Some(configure);

        let resized = if state_change_requires_resize || new_size != self.inner_size() {
            self.resize(new_size);
            true
        } else {
            false
        };
        self.refresh_frame();
        resized
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

    /// Starts an interactive drag resize in `direction`.
    pub fn drag_resize_window(&self, direction: ResizeDirection) -> Result<(), ExternalError> {
        let xdg_toplevel = self.xdg_toplevel();

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

    /// Starts an interactive window move.
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        let xdg_toplevel = self.xdg_toplevel();
        self.apply_on_pointer(|pointer| {
            self.seats.with_seat(pointer.seat(), |seat| {
                let serial = seat.pointer_button_serial.unwrap_or_default();
                // SAFETY: this window's live toplevel and the seat lent live.
                unsafe { xdg_toplevel::r#move(xdg_toplevel, seat.seat, serial) };
            });
        });

        Ok(())
    }

    /// Returns whether the window is focused by any seat.
    #[inline]
    pub fn has_focus(&self) -> bool {
        !self.seat_focus.is_empty()
    }

    /// Returns whether IME input is enabled.
    #[inline]
    pub fn ime_allowed(&self) -> bool {
        self.ime_allowed
    }

    /// Returns the logical inner size of the window.
    #[inline]
    pub fn inner_size(&self) -> LogicalSize<u32> {
        self.size
    }

    /// Returns whether the window has received its initial configure event.
    #[inline]
    pub fn is_configured(&self) -> bool {
        self.last_configure.is_some()
    }

    /// Returns whether the compositor provides server-side decorations.
    ///
    /// LOCAL PATCH (aterm): `false` under client-side decorations, where upstream answered `true`
    /// while its own frame showed.
    #[inline]
    pub fn is_decorated(&self) -> bool {
        match self.last_configure.as_ref() {
            Some(configure) => configure.decoration_mode == DecorationMode::Server,
            None => true,
        }
    }

    /// Returns the client-side decorations the application should draw, if settled on CSD.
    pub fn client_decorations(&self) -> Option<ClientDecorations> {
        let configure = self.last_configure.as_ref()?;
        if configure.decoration_mode != DecorationMode::Client {
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
            resizable: true,
        })
    }

    /// Returns the logical outer size of the window.
    #[inline]
    pub fn outer_size(&self) -> LogicalSize<u32> {
        self.size
    }

    /// Registers a pointer that entered the surface.
    pub fn pointer_entered(&mut self, added: Weak<WinitPointer>) {
        self.pointers.push(added);
        self.reload_cursor_style();
    }

    /// Removes a pointer that left the surface.
    pub fn pointer_left(&mut self, removed: &Weak<WinitPointer>) {
        self.pointers.retain(|pointer| pointer.strong_count() > 0 && !pointer.ptr_eq(removed));
    }

    /// Applies the selected cursor icon to all active pointers.
    pub fn reload_cursor_style(&mut self) {
        self.set_cursor(self.selected_cursor);
    }

    /// Updates the full-surface opaque region hint on the compositor.
    pub fn reload_transparency_hint(&self) {
        let surface = self.surface();

        // SAFETY: the live compositor.
        if let Some(region) = unsafe { wl_compositor::create_region(self.compositor.proxy()) } {
            // SAFETY: the region made above, destroyed once here after the surface copies it.
            unsafe {
                wl_region::add(region, 0, 0, i32::MAX, i32::MAX);
                wl_surface::set_opaque_region(surface, Some(region));
                wl_region::destroy(region);
            }
        } else {
            warn!("Failed to mark window opaque.");
        }
    }

    /// Resizes the window to `inner_size` and returns the new physical size.
    pub fn request_inner_size(&mut self, inner_size: Size) -> PhysicalSize<u32> {
        if self.last_configure.as_ref().map(Self::is_stateless).unwrap_or(true) {
            self.resize(inner_size.to_logical(self.scale_factor()))
        }

        logical_to_physical_rounded(self.inner_size(), self.scale_factor())
    }

    /// Resize the window to the new inner size.
    fn resize(&mut self, inner_size: LogicalSize<u32>) {
        self.size = inner_size;

        if Some(true) == self.last_configure.as_ref().map(Self::is_stateless) {
            self.stateless_size = inner_size;
        }

        self.reload_transparency_hint();

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

        self.refresh_frame();
    }

    /// How the shadow margin looks now, or `None` where it is hidden: under server-side
    /// decorations, with decorations off, before the first configure, and while maximized or
    /// fullscreen (sctk-adwaita hid its borders then).
    fn frame_look(&self) -> Option<Look> {
        let decorations = self.client_decorations()?;
        if decorations.maximized || decorations.fullscreen {
            return None;
        }
        let active = self
            .last_configure
            .as_ref()
            .is_some_and(|configure| configure.state.contains(XdgWindowState::ACTIVATED));
        Some(Look {
            width: self.size.width,
            height: self.size.height,
            scale: self.scale_factor.clamp(0.1, 64.).ceil() as u32,
            active,
            dark: self.theme == Some(Theme::Dark),
            tiled: decorations.tiled,
        })
    }

    /// Updates or hides the client-side shadow margin subsurfaces.
    pub(crate) fn refresh_frame(&mut self) {
        let look = self.frame_look();
        if self.frame.is_none() {
            let Some(globals) = self.frame_globals.as_ref() else { return };
            if look.is_none() {
                return;
            }
            self.frame = ShadowFrame::new(globals, self.objects.surface.proxy());
        }
        if let Some(frame) = self.frame.as_mut() {
            frame.show(look);
        }
    }

    /// Returns the shadow border [`Part`] corresponding to `surface`, if any.
    pub(crate) fn frame_part(&self, surface: WindowId) -> Option<Part> {
        self.frame.as_ref()?.part_of(surface)
    }

    /// Maps a pointer position `(x, y)` on shadow border `part` to a [`ResizeDirection`].
    pub(crate) fn frame_direction(&self, part: Part, x: f64, y: f64) -> Option<ResizeDirection> {
        let look = self.frame.as_ref()?.shown()?;
        Some(frame_shadow::direction(part, x, y, look.width, look.height))
    }

    /// Starts an interactive resize in `direction` triggered from `seat` at `serial`.
    pub(crate) fn resize_from_seat(&self, seat: u32, serial: u32, direction: ResizeDirection) {
        let xdg_toplevel = self.xdg_toplevel();
        self.seats.with_seat(seat, |seat| {
            // SAFETY: this window's live toplevel and the seat lent live.
            unsafe {
                xdg_toplevel::resize(xdg_toplevel, seat.seat, serial, resize_edge(direction))
            };
        });
    }

    /// Returns the current scale factor of the window.
    #[inline]
    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    /// Sets the window cursor icon.
    pub fn set_cursor(&mut self, cursor_icon: CursorIcon) {
        self.selected_cursor = cursor_icon;

        self.apply_on_pointer(|pointer| {
            if !pointer.set_cursor(&self.seats, cursor_icon) {
                warn!("Failed to set cursor to {:?}", cursor_icon);
            }
        })
    }

    /// Sets the window resize increments.
    pub fn set_resize_increments(&mut self, increments: Option<LogicalSize<u32>>) {
        self.resize_increments = increments;
    }

    /// Sets the minimum inner window size.
    pub fn set_min_inner_size(&mut self, size: Option<LogicalSize<u32>>) {
        let mut size = size.unwrap_or(MIN_WINDOW_SIZE);
        size.width = size.width.max(MIN_WINDOW_SIZE.width);
        size.height = size.height.max(MIN_WINDOW_SIZE.height);

        self.min_inner_size = size;
        // SAFETY: this window's live toplevel.
        unsafe {
            xdg_toplevel::set_min_size(self.xdg_toplevel(), size.width as i32, size.height as i32)
        };
    }

    /// Records the preferred window theme.
    pub fn set_theme(&mut self, theme: Option<Theme>) -> bool {
        self.theme = theme;
        let shown = self.frame.as_ref().and_then(ShadowFrame::shown);
        self.refresh_frame();
        shown != self.frame.as_ref().and_then(ShadowFrame::shown)
    }

    /// Returns the preferred window theme.
    #[inline]
    pub fn theme(&self) -> Option<Theme> {
        self.theme
    }

    pub fn show_window_menu(&self, position: LogicalPosition<u32>) {
        let xdg_toplevel = self.xdg_toplevel();
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

    /// Records keyboard focus from `seat`.
    #[inline]
    pub fn add_seat_focus(&mut self, seat: u32) {
        self.seat_focus.insert(seat);
    }

    /// Removes keyboard focus for `seat`.
    #[inline]
    pub fn remove_seat_focus(&mut self, seat: u32) {
        self.seat_focus.remove(&seat);
    }

    /// Sets whether IME input is enabled, returning `true` if the state changed.
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

    /// Updates the IME cursor rectangle.
    pub fn set_ime_cursor_area(&self, position: LogicalPosition<u32>, size: LogicalSize<u32>) {
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

    /// Sets the IME content purpose.
    pub fn set_ime_purpose(&mut self, purpose: ImePurpose) {
        self.ime_purpose = purpose;

        for text_input in self.text_inputs.iter().filter_map(Weak::upgrade) {
            text_input.set_content_type_by_purpose(purpose);
            text_input.commit();
        }
    }

    /// Returns the current IME content purpose.
    pub fn ime_purpose(&self) -> ImePurpose {
        self.ime_purpose
    }

    /// Updates the window scale factor.
    #[inline]
    pub fn set_scale_factor(&mut self, scale_factor: f64) {
        self.scale_factor = scale_factor;

        let surface = self.surface();
        // SAFETY: this window's live surface; `set_buffer_scale` is version 3's.
        if self.objects.fractional_scale.is_none() && unsafe { surface.version() } >= 3 {
            // SAFETY: as above.
            unsafe { wl_surface::set_buffer_scale(surface, self.scale_factor as _) };
        }

        self.refresh_frame();
    }

    /// Sets the window title, truncating at 1024 bytes.
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
    }

    /// Sets the window `xdg_toplevel` application ID.
    pub fn set_app_id(&self, app_id: &str) {
        // SAFETY: this window's live toplevel.
        unsafe { xdg_toplevel::set_app_id(self.xdg_toplevel(), &protocol_string(app_id)) };
    }

    /// Returns the text inputs observed on this window.
    #[cfg(test)]
    pub(crate) fn text_inputs(&self) -> &[Weak<TextInput>] {
        &self.text_inputs
    }

    /// Registers a text input that entered the window.
    #[inline]
    pub fn text_input_entered(&mut self, text_input: &Arc<TextInput>) {
        let weak = Arc::downgrade(text_input);
        if !self.text_inputs.iter().any(|t| t.ptr_eq(&weak)) {
            self.text_inputs.push(weak);
        }
    }

    /// Removes a text input that left the window.
    #[inline]
    pub fn text_input_left(&mut self, text_input: &Arc<TextInput>) {
        let weak = Arc::downgrade(text_input);
        self.text_inputs.retain(|t| t.strong_count() > 0 && !t.ptr_eq(&weak));
    }
}

/// Converts `s` into a NUL-terminated [`CString`], truncating at the first interior NUL.
pub(crate) fn protocol_string(s: &str) -> CString {
    let end = s.find('\0').unwrap_or(s.len());
    CString::new(&s[..end]).unwrap_or_default()
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

// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/window/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! The Wayland window.

use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aterm_types::native_window::NativeWindow;
use aterm_wayland::protocols::fractional_scale_v1::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
use aterm_wayland::protocols::viewporter::{wp_viewport, wp_viewporter};
use aterm_wayland::protocols::wayland::{wl_compositor, wl_surface};
use aterm_wayland::protocols::xdg_activation_v1::{xdg_activation_token_v1, xdg_activation_v1};
use aterm_wayland::protocols::xdg_decoration_unstable_v1::{
    zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use aterm_wayland::protocols::xdg_shell::{xdg_surface, xdg_toplevel, xdg_wm_base};
use aterm_wayland::proxy::{EventArg, Proxy};
use tracing::warn;

use crate::dpi::{LogicalSize, PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{ExternalError, NotSupportedError, OsError as RootOsError};
use crate::event::{Ime, WindowEvent};
use crate::platform_impl::common::poll::Waker;
use crate::platform_impl::{Fullscreen, MonitorHandle as PlatformMonitorHandle, OsError};
use crate::window::{
    Cursor, ImePurpose, ResizeDirection, Theme, UserAttentionType, WindowAttributes,
};

use super::event_loop::sink::EventSink;
use super::output::MonitorHandle;
use super::state::Connection;
use super::wire::{self, Inbox, Owned, ToplevelEvent, Wire};
use super::{ActiveEventLoop, WaylandError, WindowId};

pub(crate) mod frame;
pub(crate) mod state;

pub use state::WindowState;
use state::{protocol_string, WindowObjects};

/// The Wayland window.
pub struct Window {
    /// Window id.
    window_id: WindowId,

    /// The state of the window; let go first when the window drops (see `Drop`).
    window_state: ManuallyDrop<Arc<Mutex<WindowState>>>,

    /// The `wl_surface`, live as long as the window state.
    surface: Proxy,

    /// The `xdg_toplevel`, live as long as the window state.
    toplevel: Proxy,

    /// Xdg activation to request user attention.
    xdg_activation: Option<Arc<Owned>>,

    /// The loop's inbox, for the activation token's event.
    inbox: Inbox,

    /// Window requests to the event loop.
    window_requests: Arc<WindowRequests>,

    /// Observed monitors.
    monitors: Arc<Mutex<Vec<MonitorHandle>>>,

    /// Wakes the event loop for window requests.
    event_loop_awakener: Waker,

    /// The event sink to deliver synthetic events.
    window_events_sink: Arc<Mutex<EventSink>>,

    /// Retain the display through every global above, including activation.
    #[allow(dead_code)]
    connection: Arc<Connection>,
}

impl Window {
    pub(crate) fn new(
        event_loop_window_target: &ActiveEventLoop,
        attributes: WindowAttributes,
    ) -> Result<Self, RootOsError> {
        let mut state = event_loop_window_target.state.borrow_mut();
        let inbox = state.inbox.clone();
        let monitors = state.monitors.clone();
        let compositor = state.compositor();
        let xdg_activation = state.xdg_activation.clone();
        let connection = Arc::clone(&state.connection);

        let size: Size = attributes.inner_size.unwrap_or(LogicalSize::new(800., 600.).into());

        // SAFETY: the live compositor; every object below is this window's, owned by its
        // state, and each listener's events go to the loop's inbox.
        let surface = unsafe { wl_compositor::create_surface(compositor) }
            .ok_or_else(|| os_error!(OsError::from(WaylandError::Missing("a wl_surface"))))?;
        let window_id = super::make_wid(surface);
        // SAFETY: as above.
        let surface_owned = unsafe {
            Owned::listened(surface, wl_surface::destroy, &inbox, move |_, opcode, args| {
                wire::surface_event(opcode, args)
                    .map(|event| Wire::Surface { surface: window_id, event })
            })
        };

        let wm_base = state.xdg_shell.wm_base();
        // SAFETY: the shell's live wm_base and the role-less surface made above.
        let xdg = unsafe { xdg_wm_base::get_xdg_surface(wm_base, surface) }
            .ok_or_else(|| os_error!(OsError::from(WaylandError::Missing("an xdg_surface"))))?;
        let configured = move |xdg: Proxy, opcode: u32, args: &[EventArg<'_>]| match (opcode, args)
        {
            (xdg_surface::evt::CONFIGURE, [EventArg::Uint(serial)]) => {
                // SAFETY: the live xdg_surface the event came from.
                unsafe { xdg_surface::ack_configure(xdg, *serial) };
                Some(Wire::Toplevel { window: window_id, event: ToplevelEvent::Configured })
            },
            _ => None,
        };
        // SAFETY: as above.
        let xdg_owned = unsafe { Owned::listened(xdg, xdg_surface::destroy, &inbox, configured) };
        // SAFETY: the live xdg_surface.
        let toplevel = unsafe { xdg_surface::get_toplevel(xdg) }
            .ok_or_else(|| os_error!(OsError::from(WaylandError::Missing("an xdg_toplevel"))))?;
        // SAFETY: as above.
        let toplevel_owned = unsafe {
            Owned::listened(toplevel, xdg_toplevel::destroy, &inbox, move |_, opcode, args| {
                toplevel_event(opcode, args)
                    .map(|event| Wire::Toplevel { window: window_id, event })
            })
        };

        let decoration = state.xdg_shell.decoration_manager().and_then(|manager| {
            // SAFETY: the shell's live decoration manager and the toplevel made above.
            let decoration =
                unsafe { zxdg_decoration_manager_v1::get_toplevel_decoration(manager, toplevel) }?;
            // SAFETY: as above.
            let decoration = unsafe {
                Owned::listened(
                    decoration,
                    zxdg_toplevel_decoration_v1::destroy,
                    &inbox,
                    move |_, opcode, args| match (opcode, args) {
                        (zxdg_toplevel_decoration_v1::evt::CONFIGURE, [EventArg::Uint(mode)]) => {
                            Some(Wire::Toplevel {
                                window: window_id,
                                event: ToplevelEvent::DecorationMode(*mode),
                            })
                        },
                        _ => None,
                    },
                )
            };
            // SAFETY: the live decoration object.
            unsafe {
                zxdg_toplevel_decoration_v1::set_mode(
                    decoration.proxy(),
                    zxdg_toplevel_decoration_v1::mode::SERVER_SIDE,
                )
            };
            Some(decoration)
        });

        let viewport = state.viewporter.as_ref().and_then(|viewporter| {
            // SAFETY: the live viewporter and this window's surface.
            let viewport = unsafe { wp_viewporter::get_viewport(viewporter.proxy(), surface) }?;
            // SAFETY: a viewport made here, without events.
            Some(unsafe { Owned::new(viewport, wp_viewport::destroy) })
        });
        let fractional_scale = state.fractional_scaling_manager.as_ref().and_then(|manager| {
            // SAFETY: the live manager and this window's surface.
            let scale = unsafe {
                wp_fractional_scale_manager_v1::get_fractional_scale(manager.proxy(), surface)
            }?;
            // SAFETY: as above.
            Some(unsafe {
                Owned::listened(
                    scale,
                    wp_fractional_scale_v1::destroy,
                    &inbox,
                    move |_, opcode, args| match (opcode, args) {
                        (wp_fractional_scale_v1::evt::PREFERRED_SCALE, [EventArg::Uint(scale)]) => {
                            Some(Wire::PreferredScale { window: window_id, scale: *scale })
                        },
                        _ => None,
                    },
                )
            })
        });

        let objects = WindowObjects {
            attention_token: None,
            viewport,
            fractional_scale,
            _decoration: decoration,
            toplevel: toplevel_owned,
            xdg_surface: xdg_owned,
            surface: surface_owned,
        };
        let frame_globals = state.subcompositor.as_ref().map(|subcompositor| frame::FrameGlobals {
            compositor: Arc::clone(&state.compositor),
            subcompositor: Arc::clone(subcompositor),
            shm: state.shm(),
        });
        let mut window_state = WindowState::new(
            Arc::clone(&state.compositor),
            Arc::clone(&state.xdg_shell),
            Arc::clone(&state.seats_book),
            Arc::clone(&connection),
            size,
            objects,
            frame_globals,
        );

        window_state.reload_transparency_hint();

        if let Some(name) = attributes.platform_specific.name.map(|name| name.general) {
            window_state.set_app_id(&name);
        }

        window_state.set_title(attributes.title);

        let min_size = attributes.min_inner_size.map(|size| size.to_logical(1.));
        window_state.set_min_inner_size(min_size);

        if let (Some(xdg_activation), Some(token)) =
            (xdg_activation.as_ref(), attributes.platform_specific.activation_token)
        {
            // SAFETY: the live activation global and this window's surface.
            unsafe {
                xdg_activation_v1::activate(
                    xdg_activation.proxy(),
                    &protocol_string(&token),
                    surface,
                )
            };
        }

        // SAFETY: this window's live surface.
        unsafe { wl_surface::commit(surface) };

        let window_state = Arc::new(Mutex::new(window_state));
        state.windows.get_mut().insert(window_id, window_state.clone());

        let window_requests = WindowRequests {
            redraw_requested: AtomicBool::new(true),
            closed: AtomicBool::new(false),
        };
        let window_requests = Arc::new(window_requests);
        state.window_requests.get_mut().insert(window_id, window_requests.clone());

        let window_events_sink = state.window_events_sink.clone();

        let queue = &state.connection.queue;
        let result = queue.roundtrip();
        state.drain();
        result.map_err(|error| os_error!(OsError::from(error)))?;

        while !window_state.lock().unwrap().is_configured() {
            let result = state.connection.queue.dispatch();
            state.drain();
            result.map_err(|error| os_error!(OsError::from(error)))?;
        }

        let event_loop_awakener = event_loop_window_target.waker.clone();
        event_loop_awakener.wake();

        Ok(Self {
            window_id,
            window_state: ManuallyDrop::new(window_state),
            surface,
            toplevel,
            connection,
            xdg_activation,
            inbox,
            window_requests,
            monitors,
            event_loop_awakener,
            window_events_sink,
        })
    }
}

/// An `xdg_toplevel` event, decoded.
fn toplevel_event(opcode: u32, args: &[EventArg<'_>]) -> Option<ToplevelEvent> {
    use xdg_toplevel::evt;
    Some(match (opcode, args) {
        (
            evt::CONFIGURE,
            [EventArg::Int(width), EventArg::Int(height), EventArg::Array(states)],
        ) => {
            ToplevelEvent::Configure { width: *width, height: *height, states: wire::words(states) }
        },
        (evt::CLOSE, []) => ToplevelEvent::Close,
        (evt::CONFIGURE_BOUNDS, [EventArg::Int(width), EventArg::Int(height)]) => {
            ToplevelEvent::Bounds { width: *width, height: *height }
        },
        (evt::WM_CAPABILITIES, [EventArg::Array(capabilities)]) => {
            ToplevelEvent::Capabilities(wire::words(capabilities))
        },
        _ => return None,
    })
}

/// Make `request` with the output `monitor` names while it is still bound, or with `None`.
fn with_live_output(
    monitors: &Mutex<Vec<MonitorHandle>>,
    monitor: Option<PlatformMonitorHandle>,
    request: impl FnOnce(Option<Proxy>),
) {
    let monitors = monitors.lock().unwrap();
    let output = match monitor {
        Some(PlatformMonitorHandle::Wayland(monitor)) => {
            monitors.iter().find(|m| **m == monitor).map(|m| m.output.proxy())
        },
        _ => None,
    };
    request(output);
}

impl Window {
    #[inline]
    pub fn id(&self) -> WindowId {
        self.window_id
    }

    /// The window's state, which a request from another thread locks (`retire_tests`).
    #[cfg(test)]
    pub(crate) fn shared_state(&self) -> Arc<Mutex<WindowState>> {
        Arc::clone(&self.window_state)
    }

    /// The window's `wl_surface` (`retire_tests` gives it a buffer).
    #[cfg(test)]
    pub(crate) fn wl_surface(&self) -> Proxy {
        self.surface
    }

    #[inline]
    pub fn set_title(&self, title: impl ToString) {
        let new_title = title.to_string();
        self.window_state.lock().unwrap().set_title(new_title);
    }

    #[inline]
    pub fn set_visible(&self, _visible: bool) {}

    #[inline]
    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        Err(NotSupportedError::new())
    }

    #[inline]
    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        Err(NotSupportedError::new())
    }

    #[inline]
    pub fn set_outer_position(&self, _: Position) {}

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        let window_state = self.window_state.lock().unwrap();
        let scale_factor = window_state.scale_factor();
        super::logical_to_physical_rounded(window_state.inner_size(), scale_factor)
    }

    #[inline]
    pub fn request_redraw(&self) {
        if self
            .window_requests
            .redraw_requested
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.event_loop_awakener.wake();
        }
    }

    #[inline]
    pub fn outer_size(&self) -> PhysicalSize<u32> {
        let window_state = self.window_state.lock().unwrap();
        let scale_factor = window_state.scale_factor();
        super::logical_to_physical_rounded(window_state.outer_size(), scale_factor)
    }

    #[inline]
    pub fn request_inner_size(&self, size: Size) -> Option<PhysicalSize<u32>> {
        let mut window_state = self.window_state.lock().unwrap();
        let new_size = window_state.request_inner_size(size);
        self.request_redraw();
        Some(new_size)
    }

    /// Set the minimum inner size for the window.
    #[inline]
    pub fn set_min_inner_size(&self, min_size: Option<Size>) {
        let scale_factor = self.scale_factor();
        let min_size = min_size.map(|size| size.to_logical(scale_factor));
        self.window_state.lock().unwrap().set_min_inner_size(min_size);
        self.request_redraw();
    }

    #[inline]
    pub fn set_resize_increments(&self, increments: Option<Size>) {
        let mut window_state = self.window_state.lock().unwrap();
        let scale_factor = window_state.scale_factor();
        let increments = increments.map(|size| size.to_logical(scale_factor));
        window_state.set_resize_increments(increments);
    }

    #[inline]
    pub fn has_focus(&self) -> bool {
        self.window_state.lock().unwrap().has_focus()
    }

    #[inline]
    pub fn is_minimized(&self) -> Option<bool> {
        None
    }

    #[inline]
    pub fn show_window_menu(&self, position: Position) {
        let scale_factor = self.scale_factor();
        let position = position.to_logical(scale_factor);
        self.window_state.lock().unwrap().show_window_menu(position);
    }

    #[inline]
    pub fn drag_resize_window(&self, direction: ResizeDirection) -> Result<(), ExternalError> {
        self.window_state.lock().unwrap().drag_resize_window(direction)
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        self.window_state.lock().unwrap().scale_factor()
    }

    #[inline]
    pub fn is_decorated(&self) -> bool {
        self.window_state.lock().unwrap().is_decorated()
    }

    /// The decorations the application must draw ([`WindowState::client_decorations`]).
    #[inline]
    pub fn client_decorations(&self) -> Option<crate::platform::wayland::ClientDecorations> {
        self.window_state.lock().unwrap().client_decorations()
    }

    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        if !minimized {
            warn!("Unminimizing is ignored on Wayland.");
            return;
        }

        // SAFETY: the window's live toplevel (the state holds it while the window lives).
        unsafe { xdg_toplevel::set_minimized(self.toplevel) };
    }

    #[inline]
    pub fn is_maximized(&self) -> bool {
        self.window_state
            .lock()
            .unwrap()
            .last_configure
            .as_ref()
            .map(|last_configure| last_configure.is_maximized())
            .unwrap_or_default()
    }

    /// The compositor's `tiled_*` states without `maximized`: an edge tile.
    #[inline]
    pub fn is_tiled(&self) -> bool {
        self.window_state.lock().unwrap().last_configure.as_ref().is_some_and(|last_configure| {
            last_configure.is_tiled() && !last_configure.is_maximized()
        })
    }

    /// The compositor's `fullscreen` state, as [`Self::fullscreen`] reads it.
    #[inline]
    pub fn is_shown_fullscreen(&self) -> bool {
        self.window_state
            .lock()
            .unwrap()
            .last_configure
            .as_ref()
            .is_some_and(|last_configure| last_configure.is_fullscreen())
    }

    #[inline]
    pub fn set_maximized(&self, maximized: bool) {
        // SAFETY: the window's live toplevel.
        unsafe {
            if maximized {
                xdg_toplevel::set_maximized(self.toplevel)
            } else {
                xdg_toplevel::unset_maximized(self.toplevel)
            }
        }
    }

    #[inline]
    pub(crate) fn fullscreen(&self) -> Option<Fullscreen> {
        let is_fullscreen = self
            .window_state
            .lock()
            .unwrap()
            .last_configure
            .as_ref()
            .map(|last_configure| last_configure.is_fullscreen())
            .unwrap_or_default();

        if is_fullscreen {
            let current_monitor = self.current_monitor().map(PlatformMonitorHandle::Wayland);
            Some(Fullscreen::Borderless(current_monitor))
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        match fullscreen {
            Some(Fullscreen::Borderless(monitor)) => {
                with_live_output(&self.monitors, monitor, |output| {
                    // SAFETY: the window's live toplevel and an output still bound.
                    unsafe { xdg_toplevel::set_fullscreen(self.toplevel, output) }
                })
            },
            // SAFETY: the window's live toplevel.
            None => unsafe { xdg_toplevel::unset_fullscreen(self.toplevel) },
        }
    }

    #[inline]
    pub fn set_cursor(&self, cursor: Cursor) {
        let Cursor::Icon(icon) = cursor;
        self.window_state.lock().unwrap().set_cursor(icon);
    }

    pub fn request_user_attention(&self, request_type: Option<UserAttentionType>) {
        let xdg_activation = match self.xdg_activation.as_ref() {
            Some(xdg_activation) => xdg_activation.proxy(),
            None => {
                warn!("`request_user_attention` isn't supported");
                return;
            },
        };

        let mut window_state = self.window_state.lock().unwrap();
        if request_type.is_none() || window_state.attention_requested {
            return;
        }

        // SAFETY: the live activation global.
        let Some(token) = (unsafe { xdg_activation_v1::get_activation_token(xdg_activation) })
        else {
            return;
        };
        window_state.attention_requested = true;
        let window_id = self.window_id;
        // SAFETY: a token made here, without a listener; the window's state owns it until its
        // `done` is applied.
        let token = unsafe {
            Owned::listened(
                token,
                xdg_activation_token_v1::destroy,
                &self.inbox,
                move |_, opcode, args| match (opcode, args) {
                    (xdg_activation_token_v1::evt::DONE, [EventArg::Str(token)]) => {
                        Some(Wire::ActivationDone {
                            window: window_id,
                            token: wire::text(*token).unwrap_or_default(),
                        })
                    },
                    _ => None,
                },
            )
        };
        // SAFETY: the live token and the window's live surface.
        unsafe {
            xdg_activation_token_v1::set_surface(token.proxy(), self.surface);
            xdg_activation_token_v1::commit(token.proxy());
        }
        window_state.objects.attention_token = Some(token);
    }

    #[inline]
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        self.window_state.lock().unwrap().drag_window()
    }

    #[inline]
    pub fn set_ime_cursor_area(&self, position: Position, size: Size) {
        let window_state = self.window_state.lock().unwrap();
        if window_state.ime_allowed() {
            let scale_factor = window_state.scale_factor();
            let position = position.to_logical(scale_factor);
            let size = size.to_logical(scale_factor);
            window_state.set_ime_cursor_area(position, size);
        }
    }

    #[inline]
    pub fn set_ime_allowed(&self, allowed: bool) {
        let mut window_state = self.window_state.lock().unwrap();

        if window_state.ime_allowed() != allowed && window_state.set_ime_allowed(allowed) {
            let event = WindowEvent::Ime(if allowed { Ime::Enabled } else { Ime::Disabled });
            self.window_events_sink.lock().unwrap().push_window_event(event, self.window_id);
            self.event_loop_awakener.wake();
        }
    }

    #[inline]
    pub fn set_ime_purpose(&self, purpose: ImePurpose) {
        self.window_state.lock().unwrap().set_ime_purpose(purpose);
    }

    #[inline]
    pub fn focus_window(&self) {}

    /// Raise and focus the window with an `xdg_activation_v1` token.
    pub fn focus_window_with_activation_token(&self, token: &str) {
        let Some(xdg_activation) = self.xdg_activation.as_ref() else {
            return;
        };
        // SAFETY: the live activation global and this window's live surface.
        unsafe {
            xdg_activation_v1::activate(
                xdg_activation.proxy(),
                &protocol_string(token),
                self.surface,
            )
        };
    }

    #[inline]
    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        self.window_state.lock().unwrap().outputs.first().cloned().map(MonitorHandle::new)
    }

    #[inline]
    pub fn available_monitors(&self) -> Vec<MonitorHandle> {
        self.monitors.lock().unwrap().clone()
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        None
    }

    /// The window's handles for `aterm_types::native_window`.
    #[inline]
    pub fn native_window(&self) -> NativeWindow {
        NativeWindow::Wayland {
            display: NonNull::new(self.connection.display().as_ptr())
                .expect("wl_display is never null"),
            surface: NonNull::new(self.surface.as_ptr()).expect("wl_surface will never be null"),
        }
    }

    #[inline]
    pub fn set_theme(&self, theme: Option<Theme>) {
        if self.window_state.lock().unwrap().set_theme(theme) {
            self.request_redraw();
        }
    }

    #[inline]
    pub fn theme(&self) -> Option<Theme> {
        self.window_state.lock().unwrap().theme()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the field is not used again; this is the window's drop.
        unsafe { ManuallyDrop::drop(&mut self.window_state) };
        self.window_requests.closed.store(true, Ordering::Release);
        self.event_loop_awakener.wake();
    }
}

/// The request from the window to the event loop.
#[derive(Debug)]
pub struct WindowRequests {
    /// The window was closed.
    pub closed: AtomicBool,

    /// Redraw Requested.
    pub redraw_requested: AtomicBool,
}

impl WindowRequests {
    /// Whether the window closed.
    pub fn take_closed(&self) -> bool {
        self.closed.swap(false, Ordering::Acquire)
    }

    pub fn take_redraw_requested(&self) -> bool {
        self.redraw_requested.swap(false, Ordering::Relaxed)
    }
}

impl TryFrom<&str> for Theme {
    type Error = ();

    /// Parse `"dark"` or `"light"` case-insensitively into a [`Theme`].
    fn try_from(theme: &str) -> Result<Self, Self::Error> {
        if theme.eq_ignore_ascii_case("dark") {
            Ok(Self::Dark)
        } else if theme.eq_ignore_ascii_case("light") {
            Ok(Self::Light)
        } else {
            Err(())
        }
    }
}

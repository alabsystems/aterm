// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/wayland.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! # Wayland
//!
//! **Note:** Windows don't appear on Wayland until you draw/present to them.
//!
//! The system's libwayland-client is always opened at run time, through first-party
//! `aterm-wayland` (and libxkbcommon through `aterm-xkb`); there is no link-time option
//! (aterm, LW.7: upstream's `wayland-dlopen` feature chose between the two).
//!
//! ## Client-side decorations
//!
//! Upstream winit draws a client-side frame (its `wayland-csd-adwaita` feature, or
//! smithay-client-toolkit's fallback) when the compositor does not grant server-side
//! decorations. This fork draws none (docs/AUDIT-third-party-2026-10-15.md, LW.5): it
//! still asks for server-side decorations through xdg-decoration, and where the compositor
//! settles on client-side ones (GNOME's Mutter, or any compositor without xdg-decoration)
//! the application draws its own header and frame.
//! [`WindowExtWayland::client_decorations`] says when, and what the compositor can do for
//! the header's buttons; the gestures go through [`Window::drag_window`],
//! [`Window::drag_resize_window`] and [`Window::show_window_menu`].

use std::ffi::c_void;
use std::ptr::NonNull;

use crate::event_loop::{ActiveEventLoop, EventLoop, EventLoopBuilder};
use crate::monitor::MonitorHandle;
use crate::window::{Window, WindowAttributes};

#[cfg(wayland_platform)]
pub use crate::platform_impl::wayland::{WaylandClipboard, WaylandSelection};
pub use crate::window::Theme;

/// Additional methods on [`ActiveEventLoop`] that are specific to Wayland.
pub trait ActiveEventLoopExtWayland {
    /// True if the [`ActiveEventLoop`] uses Wayland.
    fn is_wayland(&self) -> bool;
}

impl ActiveEventLoopExtWayland for ActiveEventLoop {
    #[inline]
    fn is_wayland(&self) -> bool {
        self.p.is_wayland()
    }
}

/// Additional methods on [`EventLoop`] that are specific to Wayland.
pub trait EventLoopExtWayland {
    /// True if the [`EventLoop`] uses Wayland.
    fn is_wayland(&self) -> bool;

    /// The Wayland CLIPBOARD / PRIMARY selection handle — a cloneable, thread-safe
    /// [`WaylandClipboard`] backed by the event loop's own seat and data devices.
    /// `None` on any other backend. (An aterm addition: upstream winit has no
    /// clipboard, and a Wayland session without XWayland had none at all.)
    #[cfg(wayland_platform)]
    fn wayland_clipboard(&self) -> Option<WaylandClipboard>;
}

impl<T: 'static> EventLoopExtWayland for EventLoop<T> {
    #[inline]
    fn is_wayland(&self) -> bool {
        self.event_loop.is_wayland()
    }

    #[cfg(wayland_platform)]
    fn wayland_clipboard(&self) -> Option<WaylandClipboard> {
        self.event_loop.wayland_clipboard()
    }
}

/// Additional methods on [`EventLoopBuilder`] that are specific to Wayland.
pub trait EventLoopBuilderExtWayland {
    /// Force using Wayland.
    fn with_wayland(&mut self) -> &mut Self;

    /// Whether to allow the event loop to be created off of the main thread.
    ///
    /// By default, the window is only allowed to be created on the main
    /// thread, to make platform compatibility easier.
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self;
}

impl<T> EventLoopBuilderExtWayland for EventLoopBuilder<T> {
    #[inline]
    fn with_wayland(&mut self) -> &mut Self {
        self.platform_specific.forced_backend = Some(crate::platform_impl::Backend::Wayland);
        self
    }

    #[inline]
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self {
        self.platform_specific.any_thread = any_thread;
        self
    }
}

/// The client-side decorations a Wayland window's application must draw
/// ([`WindowExtWayland::client_decorations`]), with what the compositor offers the header
/// (xdg-shell's `wm_capabilities`; a compositor older than xdg_wm_base v5 sends none, and every
/// capability is then assumed) and the window's state from the same configure.
///
/// An aterm addition (docs/AUDIT-third-party-2026-10-15.md, LW.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClientDecorations {
    /// The compositor shows a window menu ([`Window::show_window_menu`]).
    pub window_menu: bool,
    /// The window can be maximized and restored.
    pub maximize: bool,
    /// The window can be minimized.
    pub minimize: bool,
    /// The window is maximized.
    pub maximized: bool,
    /// The window is fullscreen.
    pub fullscreen: bool,
    /// The window is tiled against at least one edge.
    pub tiled: bool,
    /// The window is resizable ([`Window::set_resizable`]).
    pub resizable: bool,
}

/// Additional methods on [`Window`] that are specific to Wayland.
///
/// [`Window`]: crate::window::Window
pub trait WindowExtWayland {
    /// Returns `xdg_toplevel` of the window or [`None`] if the window is X11 window.
    fn xdg_toplevel(&self) -> Option<NonNull<c_void>>;

    /// The decorations this window's application must draw itself: [`Some`] once the
    /// compositor has settled on client-side decorations (it offers no xdg-decoration manager,
    /// or chose client mode) while decorations are asked for. [`None`] under server-side
    /// decorations, with decorations turned off, before the first configure, and on X11.
    ///
    /// A change (of mode, of capabilities, or of maximized, fullscreen or tiled) arrives as a
    /// [`WindowEvent::Resized`](crate::event::WindowEvent::Resized), possibly of the same size.
    /// An aterm addition (LW.5): this backend draws no frame of its own.
    fn client_decorations(&self) -> Option<ClientDecorations>;
}

impl WindowExtWayland for Window {
    #[inline]
    fn xdg_toplevel(&self) -> Option<NonNull<c_void>> {
        #[allow(clippy::single_match)]
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => None,
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => window.xdg_toplevel(),
        }
    }

    #[inline]
    fn client_decorations(&self) -> Option<ClientDecorations> {
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => None,
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => window.client_decorations(),
        }
    }
}

/// Additional methods on [`WindowAttributes`] that are specific to Wayland.
pub trait WindowAttributesExtWayland {
    /// Build window with the given name.
    ///
    /// The `general` name sets an application ID, which should match the `.desktop`
    /// file distributed with your program. The `instance` is a `no-op`.
    ///
    /// For details about application ID conventions, see the
    /// [Desktop Entry Spec](https://specifications.freedesktop.org/desktop-entry-spec/desktop-entry-spec-latest.html#desktop-file-id)
    fn with_name(self, general: impl Into<String>, instance: impl Into<String>) -> Self;

    /// Build window with the launcher's startup-notification token: the value of
    /// `XDG_ACTIVATION_TOKEN` on Wayland, which activates the new surface with it
    /// (`xdg_activation_v1.activate`), or `DESKTOP_STARTUP_ID` on X11, which names it
    /// on the window and ends the launch sequence once the window maps. The caller
    /// reads the variable and removes it from the environment, so no child inherits
    /// it. (An aterm addition: the fork cut upstream's `startup_notify` module.)
    fn with_activation_token(self, token: impl Into<String>) -> Self;
}

impl WindowAttributesExtWayland for WindowAttributes {
    #[inline]
    fn with_name(mut self, general: impl Into<String>, instance: impl Into<String>) -> Self {
        self.platform_specific.name =
            Some(crate::platform_impl::ApplicationName::new(general.into(), instance.into()));
        self
    }

    #[inline]
    fn with_activation_token(mut self, token: impl Into<String>) -> Self {
        self.platform_specific.activation_token = Some(token.into());
        self
    }
}

/// Additional methods on `MonitorHandle` that are specific to Wayland.
pub trait MonitorHandleExtWayland {
    /// Returns the inner identifier of the monitor.
    fn native_id(&self) -> u32;
}

impl MonitorHandleExtWayland for MonitorHandle {
    #[inline]
    fn native_id(&self) -> u32 {
        self.inner.native_identifier()
    }
}

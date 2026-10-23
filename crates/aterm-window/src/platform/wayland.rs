// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/wayland.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.

use crate::event_loop::{ActiveEventLoop, EventLoop};
use crate::window::{Window, WindowAttributes};

#[cfg(wayland_platform)]
pub use crate::platform_impl::wayland::{WaylandClipboard, WaylandSelection};

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
    /// The Wayland CLIPBOARD / PRIMARY selection handle ([`WaylandClipboard`]).
    #[cfg(wayland_platform)]
    fn wayland_clipboard(&self) -> Option<WaylandClipboard>;
}

impl<T: 'static> EventLoopExtWayland for EventLoop<T> {
    #[cfg(wayland_platform)]
    fn wayland_clipboard(&self) -> Option<WaylandClipboard> {
        self.event_loop.wayland_clipboard()
    }
}

/// Client-side decorations to draw ([`WindowExtWayland::client_decorations`]).
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
    /// The window is resizable.
    pub resizable: bool,
}

/// Additional methods on [`Window`] that are specific to Wayland.
pub trait WindowExtWayland {
    /// Client-side decorations ([`WindowEvent::Resized`](crate::event::WindowEvent::Resized)).
    fn client_decorations(&self) -> Option<ClientDecorations>;

    /// [`Window::focus_window`] with an activation `token`.
    fn focus_window_with_activation_token(&self, token: &str);
}

impl WindowExtWayland for Window {
    #[inline]
    fn client_decorations(&self) -> Option<ClientDecorations> {
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => None,
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => window.client_decorations(),
        }
    }

    #[inline]
    fn focus_window_with_activation_token(&self, token: &str) {
        let _span =
            tracing::debug_span!("aterm_window::Window::focus_window_with_activation_token")
                .entered();
        self.window.focus_window_with_activation_token(token);
    }
}

/// Additional methods on [`WindowAttributes`] that are specific to Wayland.
pub trait WindowAttributesExtWayland {
    /// Build window with the given name.
    fn with_name(self, general: impl Into<String>, instance: impl Into<String>) -> Self;

    /// Build window with the launcher's startup-notification token.
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

// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/window.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! The [`Window`] struct and associated types.

use std::fmt;

use crate::dpi::{PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{ExternalError, NotSupportedError};
use crate::monitor::MonitorHandle;
#[cfg(windows_platform)]
use crate::monitor::VideoModeHandle;
use crate::platform_impl::{self, PlatformSpecificWindowAttributes};

pub use crate::cursor::Cursor;
#[cfg(windows_platform)]
pub use crate::cursor::{BadImage, CustomCursor, CustomCursorSource, MAX_CURSOR_SIZE};
#[cfg(not(macos_platform))]
pub use crate::icon::{BadIcon, Icon};

#[doc(inline)]
pub use crate::cursor_icon::CursorIcon;

/// Represents a window.
pub struct Window {
    pub(crate) window: platform_impl::Window,
}

impl fmt::Debug for Window {
    fn fmt(&self, fmtr: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmtr.pad("Window { .. }")
    }
}

#[cfg(windows_platform)]
impl Drop for Window {
    fn drop(&mut self) {
        self.window.maybe_wait_on_main(|w| {
            // such as when there are multiple windows)
            if let Some(Fullscreen::Exclusive(_)) = w.fullscreen().map(|f| f.into()) {
                w.set_fullscreen(None);
            }
        })
    }
}

/// Identifier of a window.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(pub(crate) platform_impl::WindowId);

impl WindowId {
    /// Returns a dummy id, useful for unit testing.
    #[inline]
    pub const fn dummy() -> Self {
        WindowId(platform_impl::WindowId::dummy())
    }
}

impl fmt::Debug for WindowId {
    fn fmt(&self, fmtr: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(fmtr)
    }
}

impl From<WindowId> for u64 {
    #[inline]
    fn from(window_id: WindowId) -> Self {
        window_id.0.into()
    }
}

impl From<u64> for WindowId {
    #[inline]
    fn from(raw_id: u64) -> Self {
        Self(raw_id.into())
    }
}

/// Attributes used when creating a window.
#[derive(Debug, Clone)]
pub struct WindowAttributes {
    pub(crate) inner_size: Option<Size>,
    pub(crate) min_inner_size: Option<Size>,
    #[cfg(any(macos_platform, windows_platform))]
    pub(crate) position: Option<Position>,
    pub(crate) title: String,
    pub(crate) visible: bool,
    #[allow(dead_code)]
    pub(crate) platform_specific: PlatformSpecificWindowAttributes,
    #[cfg(any(x11_platform, windows_platform))]
    pub(crate) window_icon: Option<Icon>,
    #[cfg(windows_platform)]
    pub(crate) max_inner_size: Option<Size>,
    #[cfg(windows_platform)]
    pub(crate) resizable: bool,
    #[cfg(windows_platform)]
    pub(crate) enabled_buttons: WindowButtons,
    #[cfg(windows_platform)]
    pub(crate) maximized: bool,
    #[cfg(windows_platform)]
    pub(crate) transparent: bool,
    #[cfg(windows_platform)]
    pub(crate) decorations: bool,
    #[cfg(windows_platform)]
    pub(crate) preferred_theme: Option<Theme>,
    #[cfg(windows_platform)]
    pub(crate) resize_increments: Option<Size>,
    #[cfg(windows_platform)]
    pub(crate) content_protected: bool,
    #[cfg(windows_platform)]
    pub(crate) window_level: WindowLevel,
    #[cfg(windows_platform)]
    pub(crate) active: bool,
    #[cfg(windows_platform)]
    pub(crate) cursor: Cursor,
    #[cfg(windows_platform)]
    pub(crate) fullscreen: Option<Fullscreen>,
    #[cfg(all(feature = "rwh_06", windows_platform))]
    pub(crate) parent_window: Option<SendSyncRawWindowHandle>,
}

impl Default for WindowAttributes {
    #[inline]
    fn default() -> WindowAttributes {
        WindowAttributes {
            inner_size: None,
            min_inner_size: None,
            #[cfg(any(macos_platform, windows_platform))]
            position: None,
            title: "winit window".to_owned(),
            visible: true,
            platform_specific: Default::default(),
            #[cfg(any(x11_platform, windows_platform))]
            window_icon: None,
            #[cfg(windows_platform)]
            max_inner_size: None,
            #[cfg(windows_platform)]
            resizable: true,
            #[cfg(windows_platform)]
            enabled_buttons: WindowButtons::all(),
            #[cfg(windows_platform)]
            maximized: false,
            #[cfg(windows_platform)]
            transparent: false,
            #[cfg(windows_platform)]
            decorations: true,
            #[cfg(windows_platform)]
            preferred_theme: None,
            #[cfg(windows_platform)]
            resize_increments: None,
            #[cfg(windows_platform)]
            content_protected: false,
            #[cfg(windows_platform)]
            window_level: Default::default(),
            #[cfg(windows_platform)]
            active: true,
            #[cfg(windows_platform)]
            cursor: Cursor::default(),
            #[cfg(windows_platform)]
            fullscreen: None,
            #[cfg(all(feature = "rwh_06", windows_platform))]
            parent_window: None,
        }
    }
}

/// Wrapper for [`rwh_06::RawWindowHandle`] for `WindowAttributes::parent_window`.
#[derive(Debug, Clone)]
#[cfg(all(feature = "rwh_06", windows_platform))]
pub(crate) struct SendSyncRawWindowHandle(pub(crate) rwh_06::RawWindowHandle);

#[cfg(all(feature = "rwh_06", windows_platform))]
unsafe impl Send for SendSyncRawWindowHandle {}
#[cfg(all(feature = "rwh_06", windows_platform))]
unsafe impl Sync for SendSyncRawWindowHandle {}

impl WindowAttributes {
    /// Requests the window to be of specific dimensions.
    #[inline]
    pub fn with_inner_size<S: Into<Size>>(mut self, size: S) -> Self {
        self.inner_size = Some(size.into());
        self
    }

    /// Sets the minimum dimensions a window.
    #[inline]
    pub fn with_min_inner_size<S: Into<Size>>(mut self, min_size: S) -> Self {
        self.min_inner_size = Some(min_size.into());
        self
    }

    /// Sets a desired initial position for the window.
    #[cfg(any(macos_platform, windows_platform))]
    #[inline]
    pub fn with_position<P: Into<Position>>(mut self, position: P) -> Self {
        self.position = Some(position.into());
        self
    }

    /// Sets the initial title of the window in the title bar.
    #[inline]
    pub fn with_title<T: Into<String>>(mut self, title: T) -> Self {
        self.title = title.into();
        self
    }

    /// Sets whether the window will be initially visible or hidden.
    #[inline]
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Sets the window icon.
    #[cfg(any(x11_platform, windows_platform))]
    #[inline]
    pub fn with_window_icon(mut self, window_icon: Option<Icon>) -> Self {
        self.window_icon = window_icon;
        self
    }
}

impl Window {
    /// Create a new [`WindowAttributes`] which allows modifying the window's attributes.
    #[inline]
    pub fn default_attributes() -> WindowAttributes {
        WindowAttributes::default()
    }

    /// Returns an identifier unique to the window.
    #[inline]
    pub fn id(&self) -> WindowId {
        let _span = tracing::debug_span!("aterm_window::Window::id",).entered();

        self.window.maybe_wait_on_main(|w| WindowId(w.id()))
    }

    /// Returns the scale factor that can be used to map logical pixels to physical pixels.
    #[inline]
    pub fn scale_factor(&self) -> f64 {
        let _span = tracing::debug_span!("aterm_window::Window::scale_factor",).entered();

        self.window.maybe_wait_on_main(|w| w.scale_factor())
    }

    /// Queues a [`WindowEvent::RedrawRequested`] event to be emitted that aligns.
    ///
    /// [`WindowEvent::RedrawRequested`]: crate::event::WindowEvent::RedrawRequested
    #[inline]
    pub fn request_redraw(&self) {
        let _span = tracing::debug_span!("aterm_window::Window::request_redraw",).entered();

        self.window.maybe_queue_on_main(|w| w.request_redraw())
    }

    /// Notify the windowing system before presenting to the window.
    ///
    /// ```no_run
    /// # use aterm_window::window::Window;
    /// # fn swap_buffers() {}
    /// # fn scope(window: &Window) {
    /// window.pre_present_notify();
    /// swap_buffers();
    /// # }
    /// ```
    #[cfg(windows_platform)]
    #[inline]
    pub fn pre_present_notify(&self) {
        let _span = tracing::debug_span!("aterm_window::Window::pre_present_notify",).entered();

        self.window.maybe_queue_on_main(|w| w.pre_present_notify());
    }

    /// Reset the dead key state of the keyboard.
    #[cfg(windows_platform)]
    pub fn reset_dead_keys(&self) {
        let _span = tracing::debug_span!("aterm_window::Window::reset_dead_keys",).entered();

        self.window.maybe_queue_on_main(|w| w.reset_dead_keys())
    }
}

impl Window {
    /// Returns the position of the top-left hand corner of the window's client area relative.
    #[inline]
    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        let _span = tracing::debug_span!("aterm_window::Window::inner_position",).entered();

        self.window.maybe_wait_on_main(|w| w.inner_position())
    }

    /// Returns the position of the top-left hand corner of the window relative to the top-left.
    #[inline]
    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        let _span = tracing::debug_span!("aterm_window::Window::outer_position",).entered();

        self.window.maybe_wait_on_main(|w| w.outer_position())
    }

    /// Modifies the position of the window.
    ///
    /// ```no_run
    /// # use aterm_window::dpi::{LogicalPosition, PhysicalPosition};
    /// # use aterm_window::window::Window;
    /// # fn scope(window: &Window) {
    /// window.set_outer_position(LogicalPosition::new(400.0, 200.0));
    /// window.set_outer_position(PhysicalPosition::new(400, 200));
    /// # }
    /// ```
    #[inline]
    pub fn set_outer_position<P: Into<Position>>(&self, position: P) {
        let position = position.into();
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_outer_position",
            position = ?position
        )
        .entered();

        self.window.maybe_queue_on_main(move |w| w.set_outer_position(position))
    }

    /// Returns the physical size of the window's client area.
    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        let _span = tracing::debug_span!("aterm_window::Window::inner_size",).entered();

        self.window.maybe_wait_on_main(|w| w.inner_size())
    }

    /// Request the new size for the window.
    ///
    /// ```no_run
    /// # use aterm_window::dpi::{LogicalSize, PhysicalSize};
    /// # use aterm_window::window::Window;
    /// # fn scope(window: &Window) {
    /// let _ = window.request_inner_size(LogicalSize::new(400.0, 200.0));
    /// let _ = window.request_inner_size(PhysicalSize::new(400, 200));
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn request_inner_size<S: Into<Size>>(&self, size: S) -> Option<PhysicalSize<u32>> {
        let size = size.into();
        let _span = tracing::debug_span!(
            "aterm_window::Window::request_inner_size",
            size = ?size
        )
        .entered();
        self.window.maybe_wait_on_main(|w| w.request_inner_size(size))
    }

    /// Returns the physical size of the entire window.
    #[inline]
    pub fn outer_size(&self) -> PhysicalSize<u32> {
        let _span = tracing::debug_span!("aterm_window::Window::outer_size",).entered();
        self.window.maybe_wait_on_main(|w| w.outer_size())
    }

    /// Sets a minimum dimension size for the window.
    ///
    /// ```no_run
    /// # use aterm_window::dpi::{LogicalSize, PhysicalSize};
    /// # use aterm_window::window::Window;
    /// # fn scope(window: &Window) {
    /// window.set_min_inner_size(Some(LogicalSize::new(400.0, 200.0)));
    /// window.set_min_inner_size(Some(PhysicalSize::new(400, 200)));
    /// # }
    /// ```
    #[inline]
    pub fn set_min_inner_size<S: Into<Size>>(&self, min_size: Option<S>) {
        let min_size = min_size.map(|s| s.into());
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_min_inner_size",
            min_size = ?min_size
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.set_min_inner_size(min_size))
    }

    /// Sets a maximum dimension size for the window.
    ///
    /// ```no_run
    /// # use aterm_window::dpi::{LogicalSize, PhysicalSize};
    /// # use aterm_window::window::Window;
    /// # fn scope(window: &Window) {
    /// window.set_max_inner_size(Some(LogicalSize::new(400.0, 200.0)));
    /// window.set_max_inner_size(Some(PhysicalSize::new(400, 200)));
    /// # }
    /// ```
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_max_inner_size<S: Into<Size>>(&self, max_size: Option<S>) {
        let max_size = max_size.map(|s| s.into());
        let _span = tracing::debug_span!(
            "aterm_window::Window::max_size",
            max_size = ?max_size
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.set_max_inner_size(max_size))
    }

    /// Returns window resize increments if any were set.
    #[cfg(windows_platform)]
    #[inline]
    pub fn resize_increments(&self) -> Option<PhysicalSize<u32>> {
        let _span = tracing::debug_span!("aterm_window::Window::resize_increments",).entered();
        self.window.maybe_wait_on_main(|w| w.resize_increments())
    }

    /// Sets window resize increments.
    #[inline]
    pub fn set_resize_increments<S: Into<Size>>(&self, increments: Option<S>) {
        let increments = increments.map(Into::into);
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_resize_increments",
            increments = ?increments
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.set_resize_increments(increments))
    }
}

impl Window {
    /// Modifies the title of the window.
    #[inline]
    pub fn set_title(&self, title: &str) {
        let _span = tracing::debug_span!("aterm_window::Window::set_title", title).entered();
        self.window.maybe_wait_on_main(|w| w.set_title(title))
    }

    /// Change the window transparency state.
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_transparent(&self, transparent: bool) {
        let _span =
            tracing::debug_span!("aterm_window::Window::set_transparent", transparent).entered();
        self.window.maybe_queue_on_main(move |w| w.set_transparent(transparent))
    }

    /// Change the window blur state.
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_blur(&self, blur: bool) {
        let _span = tracing::debug_span!("aterm_window::Window::set_blur", blur).entered();
        self.window.maybe_queue_on_main(move |w| w.set_blur(blur))
    }

    /// Modifies the window's visibility.
    #[inline]
    pub fn set_visible(&self, visible: bool) {
        let _span = tracing::debug_span!("aterm_window::Window::set_visible", visible).entered();
        self.window.maybe_queue_on_main(move |w| w.set_visible(visible))
    }

    /// Gets the window's current visibility state.
    #[cfg(any(macos_platform, windows_platform))]
    #[inline]
    pub fn is_visible(&self) -> Option<bool> {
        let _span = tracing::debug_span!("aterm_window::Window::is_visible",).entered();
        self.window.maybe_wait_on_main(|w| w.is_visible())
    }

    /// Sets whether the window is resizable.
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_resizable(&self, resizable: bool) {
        let _span =
            tracing::debug_span!("aterm_window::Window::set_resizable", resizable).entered();
        self.window.maybe_queue_on_main(move |w| w.set_resizable(resizable))
    }

    /// Gets the window's current resizable state.
    #[cfg(windows_platform)]
    #[inline]
    pub fn is_resizable(&self) -> bool {
        let _span = tracing::debug_span!("aterm_window::Window::is_resizable",).entered();
        self.window.maybe_wait_on_main(|w| w.is_resizable())
    }

    /// Sets the enabled window buttons.
    #[cfg(windows_platform)]
    pub fn set_enabled_buttons(&self, buttons: WindowButtons) {
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_enabled_buttons",
            buttons = ?buttons
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.set_enabled_buttons(buttons))
    }

    /// Gets the enabled window buttons.
    #[cfg(windows_platform)]
    pub fn enabled_buttons(&self) -> WindowButtons {
        let _span = tracing::debug_span!("aterm_window::Window::enabled_buttons",).entered();
        self.window.maybe_wait_on_main(|w| w.enabled_buttons())
    }

    /// Sets the window to minimized or back.
    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        let _span =
            tracing::debug_span!("aterm_window::Window::set_minimized", minimized).entered();
        self.window.maybe_queue_on_main(move |w| w.set_minimized(minimized))
    }

    /// Gets the window's current minimized state.
    #[inline]
    pub fn is_minimized(&self) -> Option<bool> {
        let _span = tracing::debug_span!("aterm_window::Window::is_minimized",).entered();
        self.window.maybe_wait_on_main(|w| w.is_minimized())
    }

    /// Sets the window to maximized or back.
    #[inline]
    pub fn set_maximized(&self, maximized: bool) {
        let _span =
            tracing::debug_span!("aterm_window::Window::set_maximized", maximized).entered();
        self.window.maybe_queue_on_main(move |w| w.set_maximized(maximized))
    }

    /// Gets the window's current maximized state.
    #[inline]
    pub fn is_maximized(&self) -> bool {
        let _span = tracing::debug_span!("aterm_window::Window::is_maximized",).entered();
        self.window.maybe_wait_on_main(|w| w.is_maximized())
    }

    /// Whether the window manager shows the window tiled.
    #[inline]
    pub fn is_tiled(&self) -> bool {
        let _span = tracing::debug_span!("aterm_window::Window::is_tiled",).entered();
        #[cfg(any(x11_platform, wayland_platform))]
        {
            self.window.maybe_wait_on_main(|w| w.is_tiled())
        }
        #[cfg(not(any(x11_platform, wayland_platform)))]
        {
            false
        }
    }

    /// Whether the window shows full screen now, however it got there.
    #[inline]
    pub fn is_shown_fullscreen(&self) -> bool {
        let _span = tracing::debug_span!("aterm_window::Window::is_shown_fullscreen",).entered();
        #[cfg(any(x11_platform, wayland_platform))]
        {
            self.window.maybe_wait_on_main(|w| w.is_shown_fullscreen())
        }
        #[cfg(not(any(x11_platform, wayland_platform)))]
        {
            self.window.maybe_wait_on_main(|w| w.fullscreen().is_some())
        }
    }

    /// Sets the window to fullscreen or back.
    #[inline]
    pub fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_fullscreen",
            fullscreen = ?fullscreen
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.set_fullscreen(fullscreen.map(|f| f.into())))
    }

    /// Gets the window's current fullscreen state.
    #[inline]
    pub fn fullscreen(&self) -> Option<Fullscreen> {
        let _span = tracing::debug_span!("aterm_window::Window::fullscreen",).entered();
        self.window.maybe_wait_on_main(|w| w.fullscreen().map(|f| f.into()))
    }

    /// Turn window decorations.
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_decorations(&self, decorations: bool) {
        let _span =
            tracing::debug_span!("aterm_window::Window::set_decorations", decorations).entered();
        self.window.maybe_queue_on_main(move |w| w.set_decorations(decorations))
    }

    /// Gets the window's current decorations state.
    #[inline]
    pub fn is_decorated(&self) -> bool {
        let _span = tracing::debug_span!("aterm_window::Window::is_decorated",).entered();
        self.window.maybe_wait_on_main(|w| w.is_decorated())
    }

    /// Change the window level.
    #[cfg(windows_platform)]
    pub fn set_window_level(&self, level: WindowLevel) {
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_window_level",
            level = ?level
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.set_window_level(level))
    }

    /// Sets the window icon.
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_window_icon(&self, window_icon: Option<Icon>) {
        let _span = tracing::debug_span!("aterm_window::Window::set_window_icon",).entered();
        self.window.maybe_queue_on_main(move |w| w.set_window_icon(window_icon))
    }

    /// Set the IME cursor editing area.
    ///
    /// ```no_run
    /// # use aterm_window::dpi::{LogicalPosition, PhysicalPosition, LogicalSize, PhysicalSize};
    /// # use aterm_window::window::Window;
    /// # fn scope(window: &Window) {
    /// window.set_ime_cursor_area(LogicalPosition::new(400.0, 200.0), LogicalSize::new(100, 100));
    /// window.set_ime_cursor_area(PhysicalPosition::new(400, 200), PhysicalSize::new(100, 100));
    /// # }
    /// ```
    #[inline]
    pub fn set_ime_cursor_area<P: Into<Position>, S: Into<Size>>(&self, position: P, size: S) {
        let position = position.into();
        let size = size.into();
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_ime_cursor_area",
            position = ?position,
            size = ?size,
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.set_ime_cursor_area(position, size))
    }

    /// Sets whether the window should get IME events.
    #[inline]
    pub fn set_ime_allowed(&self, allowed: bool) {
        let _span =
            tracing::debug_span!("aterm_window::Window::set_ime_allowed", allowed).entered();
        self.window.maybe_queue_on_main(move |w| w.set_ime_allowed(allowed))
    }

    /// Sets the IME purpose for the window using [`ImePurpose`].
    #[inline]
    pub fn set_ime_purpose(&self, purpose: ImePurpose) {
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_ime_purpose",
            purpose = ?purpose
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.set_ime_purpose(purpose))
    }

    /// Brings the window to the front and sets input focus.
    #[inline]
    pub fn focus_window(&self) {
        let _span = tracing::debug_span!("aterm_window::Window::focus_window",).entered();
        self.window.maybe_queue_on_main(|w| w.focus_window())
    }

    /// Gets whether the window has keyboard focus.
    #[inline]
    pub fn has_focus(&self) -> bool {
        let _span = tracing::debug_span!("aterm_window::Window::has_focus",).entered();
        self.window.maybe_wait_on_main(|w| w.has_focus())
    }

    /// Requests user attention to the window.
    #[inline]
    pub fn request_user_attention(&self, request_type: Option<UserAttentionType>) {
        let _span = tracing::debug_span!(
            "aterm_window::Window::request_user_attention",
            request_type = ?request_type
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.request_user_attention(request_type))
    }

    /// Set or override the window theme.
    #[inline]
    pub fn set_theme(&self, theme: Option<Theme>) {
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_theme",
            theme = ?theme
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.set_theme(theme))
    }

    /// Returns the current window theme.
    #[inline]
    pub fn theme(&self) -> Option<Theme> {
        let _span = tracing::debug_span!("aterm_window::Window::theme",).entered();
        self.window.maybe_wait_on_main(|w| w.theme())
    }

    /// Prevents the window contents from being captured by other apps.
    #[cfg(windows_platform)]
    pub fn set_content_protected(&self, protected: bool) {
        let _span = tracing::debug_span!("aterm_window::Window::set_content_protected", protected)
            .entered();
        self.window.maybe_queue_on_main(move |w| w.set_content_protected(protected))
    }

    /// Gets the current title of the window.
    #[cfg(any(macos_platform, windows_platform))]
    #[inline]
    pub fn title(&self) -> String {
        let _span = tracing::debug_span!("aterm_window::Window::title",).entered();
        self.window.maybe_wait_on_main(|w| w.title())
    }
}

impl Window {
    /// Modifies the cursor icon of the window.
    #[inline]
    pub fn set_cursor(&self, cursor: impl Into<Cursor>) {
        let cursor = cursor.into();
        let _span = tracing::debug_span!("aterm_window::Window::set_cursor",).entered();
        self.window.maybe_queue_on_main(move |w| w.set_cursor(cursor))
    }

    /// Changes the position of the cursor in window coordinates.
    ///
    /// ```no_run
    /// # use aterm_window::dpi::{LogicalPosition, PhysicalPosition};
    /// # use aterm_window::window::Window;
    /// # fn scope(window: &Window) {
    /// window.set_cursor_position(LogicalPosition::new(400.0, 200.0));
    /// window.set_cursor_position(PhysicalPosition::new(400, 200));
    /// # }
    /// ```
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_cursor_position<P: Into<Position>>(&self, position: P) -> Result<(), ExternalError> {
        let position = position.into();
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_cursor_position",
            position = ?position
        )
        .entered();
        self.window.maybe_wait_on_main(|w| w.set_cursor_position(position))
    }

    /// Set grabbing [mode][CursorGrabMode] on the cursor preventing it from leaving the window.
    ///
    /// ```no_run
    /// # use aterm_window::window::{CursorGrabMode, Window};
    /// # fn scope(window: &Window) {
    /// window
    ///     .set_cursor_grab(CursorGrabMode::Confined)
    ///     .or_else(|_e| window.set_cursor_grab(CursorGrabMode::Locked))
    ///     .unwrap();
    /// # }
    /// ```
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_cursor_grab(&self, mode: CursorGrabMode) -> Result<(), ExternalError> {
        let _span = tracing::debug_span!(
            "aterm_window::Window::set_cursor_grab",
            mode = ?mode
        )
        .entered();
        self.window.maybe_wait_on_main(|w| w.set_cursor_grab(mode))
    }

    /// Modifies the cursor's visibility.
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_cursor_visible(&self, visible: bool) {
        let _span =
            tracing::debug_span!("aterm_window::Window::set_cursor_visible", visible).entered();
        self.window.maybe_queue_on_main(move |w| w.set_cursor_visible(visible))
    }

    /// Moves the window with the left mouse button until the button is released.
    #[inline]
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        let _span = tracing::debug_span!("aterm_window::Window::drag_window",).entered();
        self.window.maybe_wait_on_main(|w| w.drag_window())
    }

    /// Resizes the window with the left mouse button until the button is released.
    #[cfg(not(macos_platform))]
    #[inline]
    pub fn drag_resize_window(&self, direction: ResizeDirection) -> Result<(), ExternalError> {
        let _span = tracing::debug_span!(
            "aterm_window::Window::drag_resize_window",
            direction = ?direction
        )
        .entered();
        self.window.maybe_wait_on_main(|w| w.drag_resize_window(direction))
    }

    /// Show window menu at a specified position .
    #[cfg(not(macos_platform))]
    pub fn show_window_menu(&self, position: impl Into<Position>) {
        let position = position.into();
        let _span = tracing::debug_span!(
            "aterm_window::Window::show_window_menu",
            position = ?position
        )
        .entered();
        self.window.maybe_queue_on_main(move |w| w.show_window_menu(position))
    }

    /// Modifies whether the window catches cursor events.
    #[cfg(windows_platform)]
    #[inline]
    pub fn set_cursor_hittest(&self, hittest: bool) -> Result<(), ExternalError> {
        let _span =
            tracing::debug_span!("aterm_window::Window::set_cursor_hittest", hittest).entered();
        self.window.maybe_wait_on_main(|w| w.set_cursor_hittest(hittest))
    }
}

impl Window {
    /// Returns the monitor on which the window currently resides.
    #[inline]
    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        let _span = tracing::debug_span!("aterm_window::Window::current_monitor",).entered();
        self.window.maybe_wait_on_main(|w| w.current_monitor().map(|inner| MonitorHandle { inner }))
    }

    /// Returns the list of all the monitors available on the system.
    #[inline]
    pub fn available_monitors(&self) -> impl Iterator<Item = MonitorHandle> {
        let _span = tracing::debug_span!("aterm_window::Window::available_monitors",).entered();
        self.window.maybe_wait_on_main(|w| {
            w.available_monitors().into_iter().map(|inner| MonitorHandle { inner })
        })
    }

    /// Returns the primary monitor of the system.
    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        let _span = tracing::debug_span!("aterm_window::Window::primary_monitor",).entered();
        self.window.maybe_wait_on_main(|w| w.primary_monitor().map(|inner| MonitorHandle { inner }))
    }
}

// SAFETY: the display is the event loop's connection, which every window holds a reference.
#[cfg(free_unix)]
unsafe impl aterm_types::native_window::HasNativeWindow for Window {
    #[inline]
    fn native_window(&self) -> Option<aterm_types::native_window::NativeWindow> {
        Some(self.window.native_window())
    }
}

#[cfg(feature = "rwh_06")]
impl rwh_06::HasWindowHandle for Window {
    fn window_handle(&self) -> Result<rwh_06::WindowHandle<'_>, rwh_06::HandleError> {
        let raw = self.window.raw_window_handle_rwh_06()?;

        // SAFETY: The window handle will never be deallocated while the window is alive.
        Ok(unsafe { rwh_06::WindowHandle::borrow_raw(raw) })
    }
}

#[cfg(feature = "rwh_06")]
impl rwh_06::HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        let raw = self.window.raw_display_handle_rwh_06()?;

        // SAFETY: The window handle will never be deallocated while the window is alive.
        Ok(unsafe { rwh_06::DisplayHandle::borrow_raw(raw) })
    }
}

/// The behavior of cursor grabbing.
#[cfg(windows_platform)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum CursorGrabMode {
    /// No grabbing of the cursor is performed.
    None,

    /// The cursor is confined to the window area.
    Confined,

    /// The cursor is locked inside the window area to the certain position.
    Locked,
}

/// Defines the orientation that a window resize will be performed.
#[cfg(not(macos_platform))]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum ResizeDirection {
    East,
    North,
    NorthEast,
    NorthWest,
    South,
    SouthEast,
    SouthWest,
    West,
}

/// Fullscreen modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fullscreen {
    /// Windows only: no aterm crate asks for a video mode.
    #[cfg(windows_platform)]
    Exclusive(VideoModeHandle),

    /// Providing `None` to `Borderless` will fullscreen on the current monitor.
    Borderless(Option<MonitorHandle>),
}

/// The theme variant to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    /// Use the light variant.
    Light,

    /// Use the dark variant.
    Dark,
}

/// **X11:** Sets the wm's `XUrgencyHint`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UserAttentionType {
    /// **macOS:** Bounces the dock icon until the application is in focus.
    Critical,

    /// **macOS:** Bounces the dock icon.
    #[default]
    Informational,
}

#[cfg(windows_platform)]
aterm_types::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct WindowButtons: u32 {
        const CLOSE  = 1 << 0;
        const MINIMIZE  = 1 << 1;
        const MAXIMIZE  = 1 << 2;
    }
}

/// A window level groups windows with respect to their z-position.
#[cfg(windows_platform)]
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum WindowLevel {
    /// The window will always be below normal windows.
    AlwaysOnBottom,

    /// The default.
    #[default]
    Normal,

    /// The window will always be on top of normal windows.
    AlwaysOnTop,
}

/// Generic IME purposes for use in [`Window::set_ime_purpose`].
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
#[non_exhaustive]
pub enum ImePurpose {
    /// No special hints for the IME (default).
    #[default]
    Normal,
    /// The IME is used for password input.
    Password,
    /// The IME is used to input into a terminal.
    Terminal,
}

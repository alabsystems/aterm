// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/event_loop.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE. (Cut the Android, iOS,
//! Orbital and Web arms and the `rwh_05` impls.)
//!
//! The [`EventLoop`] struct and assorted supporting types, including
//! [`ControlFlow`].
//!
//! If you want to send custom events to the event loop, use
//! [`EventLoop::create_proxy`] to acquire an [`EventLoopProxy`] and call its
//! [`send_event`][EventLoopProxy::send_event] method.
//!
//! See the root-level documentation for information on how to create and use an event loop to
//! handle events.
// LOCAL PATCH (aterm), 2026-10-16 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 5): an
// item here marked `#[cfg(windows_platform)]` is API no aterm crate calls (custom cursors,
// device-event filters, owned display handles, the control-flow getter). The macOS backend no
// longer implements it, nor (2026-10-21, WC.8) the Linux module; the Windows carve deletes the
// rest with its half.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{error, fmt};

use std::time::Instant;

use crate::application::ApplicationHandler;
use crate::error::{EventLoopError, OsError};
use crate::event::Event;
use crate::monitor::MonitorHandle;
use crate::platform_impl;
#[cfg(any(macos_platform, windows_platform))]
use crate::window::Theme;
#[cfg(windows_platform)]
use crate::window::{CustomCursor, CustomCursorSource};
use crate::window::{Window, WindowAttributes};

/// Provides a way to retrieve events from the system and from the windows that were registered to
/// the events loop.
///
/// An `EventLoop` can be seen more or less as a "context". Calling [`EventLoop::new`]
/// initializes everything that will be required to create windows. For example on Linux creating
/// an event loop opens a connection to the X or Wayland server.
///
/// To wake up an `EventLoop` from a another thread, see the [`EventLoopProxy`] docs.
///
/// Note that this cannot be shared across threads (due to platform-dependant logic
/// forbidding it), as such it is neither [`Send`] nor [`Sync`]. If you need cross-thread access,
/// the [`Window`] created from this _can_ be sent to an other thread, and the
/// [`EventLoopProxy`] allows you to wake up an `EventLoop` from another thread.
///
/// [`Window`]: crate::window::Window
pub struct EventLoop<T: 'static> {
    pub(crate) event_loop: platform_impl::EventLoop<T>,
    pub(crate) _marker: PhantomData<*mut ()>, // Not Send nor Sync
}

/// Target that associates windows with an [`EventLoop`].
///
/// This type exists to allow you to create new windows while Winit executes
/// your callback.
pub struct ActiveEventLoop {
    pub(crate) p: platform_impl::ActiveEventLoop,
    pub(crate) _marker: PhantomData<*mut ()>, // Not Send nor Sync
}

/// Object that allows building the event loop.
///
/// This is used to make specifying options that affect the whole application
/// easier. But note that constructing multiple event loops is not supported.
///
/// This can be created using [`EventLoop::new`] or [`EventLoop::with_user_event`].
#[derive(Default)]
pub struct EventLoopBuilder<T: 'static> {
    pub(crate) platform_specific: platform_impl::PlatformSpecificEventLoopAttributes,
    _p: PhantomData<T>,
}

static EVENT_LOOP_CREATED: AtomicBool = AtomicBool::new(false);

impl<T> EventLoopBuilder<T> {
    /// Builds a new event loop.
    ///
    /// ***For cross-platform compatibility, the [`EventLoop`] must be created on the main thread,
    /// and only once per application.***
    ///
    /// Calling this function will result in display backend initialisation.
    ///
    /// ## Panics
    ///
    /// Attempting to create the event loop off the main thread will panic. This
    /// restriction isn't strictly necessary on all platforms, but is imposed to
    /// eliminate any nasty surprises when porting to platforms that require it.
    /// On Windows, [`platform`]'s `EventLoopBuilderExtWindows::with_any_thread` lifts it;
    /// X11's and Wayland's `with_any_thread` had no aterm caller and are gone (WC.8).
    ///
    /// ## Platform-specific
    ///
    /// - **Wayland/X11:** to prevent running under `Wayland` or `X11` unset `WAYLAND_DISPLAY` or
    ///   `DISPLAY` respectively when building the event loop.
    ///
    /// [`platform`]: crate::platform
    #[inline]
    pub fn build(&mut self) -> Result<EventLoop<T>, EventLoopError> {
        let _span = tracing::debug_span!("aterm_window::EventLoopBuilder::build").entered();

        if EVENT_LOOP_CREATED.swap(true, Ordering::Relaxed) {
            return Err(EventLoopError::RecreationAttempt);
        }

        // Certain platforms accept a mutable reference in their API.
        #[allow(clippy::unnecessary_mut_passed)]
        Ok(EventLoop {
            event_loop: platform_impl::EventLoop::new(&mut self.platform_specific)?,
            _marker: PhantomData,
        })
    }
}

impl<T> fmt::Debug for EventLoop<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("EventLoop { .. }")
    }
}

impl fmt::Debug for ActiveEventLoop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("ActiveEventLoop { .. }")
    }
}

/// Set through [`ActiveEventLoop::set_control_flow()`].
///
/// Indicates the desired behavior of the event loop after [`Event::AboutToWait`] is emitted.
///
/// Defaults to [`Wait`].
///
/// [`Wait`]: Self::Wait
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ControlFlow {
    /// When the current loop iteration finishes, immediately begin a new iteration regardless of
    /// whether or not new events are available to process.
    Poll,

    /// When the current loop iteration finishes, suspend the thread until another event arrives.
    #[default]
    Wait,

    /// When the current loop iteration finishes, suspend the thread until either another event
    /// arrives or the given time is reached.
    ///
    /// Useful for implementing efficient timers. Applications which want to render at the
    /// display's native refresh rate should instead use [`Poll`] and the VSync functionality
    /// of a graphics API to reduce odds of missed frames.
    ///
    /// [`Poll`]: Self::Poll
    WaitUntil(Instant),
}

impl EventLoop<()> {
    /// Create the event loop.
    ///
    /// This is an alias of `EventLoop::builder().build()`.
    #[inline]
    #[expect(
        clippy::disallowed_methods,
        reason = "winit's own constructor chain: the clippy.toml ban routes aterm's callers \
                  to aterm_gui's driver_event_loop_builder, which is built on this"
    )]
    pub fn new() -> Result<EventLoop<()>, EventLoopError> {
        Self::builder().build()
    }

    /// Start building a new event loop.
    ///
    /// This returns an [`EventLoopBuilder`], to allow configuring the event loop before creation.
    ///
    /// To get the actual event loop, call [`build`][EventLoopBuilder::build] on that.
    #[inline]
    #[expect(
        clippy::disallowed_methods,
        reason = "winit's own constructor chain: the clippy.toml ban routes aterm's callers \
                  to aterm_gui's driver_event_loop_builder, which is built on this"
    )]
    pub fn builder() -> EventLoopBuilder<()> {
        Self::with_user_event()
    }
}

impl<T> EventLoop<T> {
    /// Start building a new event loop, with the given type as the user event
    /// type.
    pub fn with_user_event() -> EventLoopBuilder<T> {
        EventLoopBuilder { platform_specific: Default::default(), _p: PhantomData }
    }

    /// Run the application with the event loop on the calling thread.
    ///
    /// See the [`set_control_flow()`] docs on how to change the event loop's behavior.
    ///
    /// [`set_control_flow()`]: ActiveEventLoop::set_control_flow()
    #[inline]
    pub fn run_app<A: ApplicationHandler<T>>(self, app: &mut A) -> Result<(), EventLoopError> {
        self.event_loop.run(|event, event_loop| dispatch_event_for_app(app, event_loop, event))
    }

    /// Creates an [`EventLoopProxy`] that can be used to dispatch user events
    /// to the main event loop, possibly from another thread.
    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy { event_loop_proxy: self.event_loop.create_proxy() }
    }

    /// Gets a persistent reference to the underlying platform display.
    ///
    /// See the [`OwnedDisplayHandle`] type for more information.
    #[cfg(windows_platform)]
    pub fn owned_display_handle(&self) -> OwnedDisplayHandle {
        OwnedDisplayHandle { platform: self.event_loop.window_target().p.owned_display_handle() }
    }

    /// Change if or when [`DeviceEvent`]s are captured.
    ///
    /// See [`ActiveEventLoop::listen_device_events`] for details.
    ///
    /// [`DeviceEvent`]: crate::event::DeviceEvent
    #[cfg(windows_platform)]
    pub fn listen_device_events(&self, allowed: DeviceEvents) {
        let _span = tracing::debug_span!(
            "aterm_window::EventLoop::listen_device_events",
            allowed = ?allowed
        )
        .entered();

        self.event_loop.window_target().p.listen_device_events(allowed);
    }

    /// Sets the [`ControlFlow`].
    pub fn set_control_flow(&self, control_flow: ControlFlow) {
        self.event_loop.window_target().p.set_control_flow(control_flow)
    }

    /// Create custom cursor.
    #[cfg(windows_platform)]
    pub fn create_custom_cursor(&self, custom_cursor: CustomCursorSource) -> CustomCursor {
        self.event_loop.window_target().p.create_custom_cursor(custom_cursor)
    }
}

#[cfg(feature = "rwh_06")]
impl<T> rwh_06::HasDisplayHandle for EventLoop<T> {
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        rwh_06::HasDisplayHandle::display_handle(self.event_loop.window_target())
    }
}

// LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, LW.7): upstream's
// `AsFd` / `AsRawFd` for the X11 and Wayland loops stood here, the loop's one descriptor for
// an embedder's own poll. The first-party loops wait in a poll(2) over several descriptors,
// with no single epoll descriptor to hand out (calloop's was one), and no aterm crate embeds
// the loop: aterm-gui drives it through `pump_app_events`.

impl ActiveEventLoop {
    /// Create the window.
    ///
    /// Possible causes of error include denied permission, incompatible system, and lack of memory.
    ///
    /// ## Platform-specific
    ///
    /// - **Web:** The window is created but not inserted into the web page automatically. Please
    ///   see the web platform module for more information.
    #[inline]
    pub fn create_window(&self, window_attributes: WindowAttributes) -> Result<Window, OsError> {
        let _span = tracing::debug_span!(
            "aterm_window::ActiveEventLoop::create_window",
            window_attributes = ?window_attributes
        )
        .entered();

        let window = platform_impl::Window::new(&self.p, window_attributes)?;
        Ok(Window { window })
    }

    /// Create custom cursor.
    #[cfg(windows_platform)]
    pub fn create_custom_cursor(&self, custom_cursor: CustomCursorSource) -> CustomCursor {
        let _span =
            tracing::debug_span!("aterm_window::ActiveEventLoop::create_custom_cursor",).entered();

        self.p.create_custom_cursor(custom_cursor)
    }

    /// Returns the list of all the monitors available on the system.
    #[inline]
    pub fn available_monitors(&self) -> impl Iterator<Item = MonitorHandle> {
        let _span =
            tracing::debug_span!("aterm_window::ActiveEventLoop::available_monitors",).entered();

        #[allow(clippy::useless_conversion)] // false positive on some platforms
        self.p.available_monitors().into_iter().map(|inner| MonitorHandle { inner })
    }

    /// Returns the primary monitor of the system.
    ///
    /// Returns `None` if it can't identify any monitor as a primary one.
    ///
    /// ## Platform-specific
    ///
    /// **Wayland / Web:** Always returns `None`.
    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        let _span =
            tracing::debug_span!("aterm_window::ActiveEventLoop::primary_monitor",).entered();

        self.p.primary_monitor().map(|inner| MonitorHandle { inner })
    }

    /// Change if or when [`DeviceEvent`]s are captured.
    ///
    /// Since the [`DeviceEvent`] capture can lead to high CPU usage for unfocused windows, winit
    /// will ignore them by default for unfocused windows on Linux/BSD. This method allows changing
    /// this at runtime to explicitly capture them again.
    ///
    /// ## Platform-specific
    ///
    /// - **Wayland / macOS / iOS / Android / Orbital:** Unsupported.
    ///
    /// [`DeviceEvent`]: crate::event::DeviceEvent
    #[cfg(windows_platform)]
    pub fn listen_device_events(&self, allowed: DeviceEvents) {
        let _span = tracing::debug_span!(
            "aterm_window::ActiveEventLoop::listen_device_events",
            allowed = ?allowed
        )
        .entered();

        self.p.listen_device_events(allowed);
    }

    /// Returns the current system theme.
    ///
    /// Returns `None` if it cannot be determined on the current platform.
    ///
    /// ## Platform-specific
    ///
    /// - **X11, Wayland:** not built (WC.8: no aterm crate asks there, and the answer was
    ///   always `None`).
    #[cfg(any(macos_platform, windows_platform))]
    pub fn system_theme(&self) -> Option<Theme> {
        self.p.system_theme()
    }

    /// Sets the [`ControlFlow`].
    pub fn set_control_flow(&self, control_flow: ControlFlow) {
        self.p.set_control_flow(control_flow)
    }

    /// Gets the current [`ControlFlow`].
    #[cfg(windows_platform)]
    pub fn control_flow(&self) -> ControlFlow {
        self.p.control_flow()
    }

    /// This exits the event loop.
    ///
    /// See [`LoopExiting`][Event::LoopExiting].
    pub fn exit(&self) {
        let _span = tracing::debug_span!("aterm_window::ActiveEventLoop::exit",).entered();

        self.p.exit()
    }

    /// Returns if the [`EventLoop`] is about to stop.
    ///
    /// See [`exit()`][Self::exit].
    pub fn exiting(&self) -> bool {
        self.p.exiting()
    }

    /// Gets a persistent reference to the underlying platform display.
    ///
    /// See the [`OwnedDisplayHandle`] type for more information.
    #[cfg(windows_platform)]
    pub fn owned_display_handle(&self) -> OwnedDisplayHandle {
        OwnedDisplayHandle { platform: self.p.owned_display_handle() }
    }
}

#[cfg(feature = "rwh_06")]
impl rwh_06::HasDisplayHandle for ActiveEventLoop {
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        let raw = self.p.raw_display_handle_rwh_06()?;
        // SAFETY: The display will never be deallocated while the event loop is alive.
        Ok(unsafe { rwh_06::DisplayHandle::borrow_raw(raw) })
    }
}

/// A proxy for the underlying display handle.
///
/// The purpose of this type is to provide a cheaply cloneable handle to the underlying
/// display handle. This is often used by graphics APIs to connect to the underlying APIs.
/// It is difficult to keep a handle to the [`EventLoop`] type or the [`ActiveEventLoop`]
/// type. In contrast, this type involves no lifetimes and can be persisted for as long as
/// needed.
///
/// For all platforms, this is one of the following:
///
/// - A zero-sized type that is likely optimized out.
/// - A reference-counted pointer to the underlying type.
#[cfg(windows_platform)]
#[derive(Clone)]
pub struct OwnedDisplayHandle {
    #[cfg_attr(not(feature = "rwh_06"), allow(dead_code))]
    platform: platform_impl::OwnedDisplayHandle,
}

#[cfg(windows_platform)]
impl fmt::Debug for OwnedDisplayHandle {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedDisplayHandle").finish_non_exhaustive()
    }
}

#[cfg(all(feature = "rwh_06", windows_platform))]
impl rwh_06::HasDisplayHandle for OwnedDisplayHandle {
    #[inline]
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        let raw = self.platform.raw_display_handle_rwh_06()?;

        // SAFETY: The underlying display handle should be safe.
        let handle = unsafe { rwh_06::DisplayHandle::borrow_raw(raw) };

        Ok(handle)
    }
}

/// Used to send custom events to [`EventLoop`].
pub struct EventLoopProxy<T: 'static> {
    event_loop_proxy: platform_impl::EventLoopProxy<T>,
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        Self { event_loop_proxy: self.event_loop_proxy.clone() }
    }
}

impl<T: 'static> EventLoopProxy<T> {
    /// Send an event to the [`EventLoop`] from which this proxy was created. This emits a
    /// `UserEvent(event)` event in the event loop, where `event` is the value passed to this
    /// function.
    ///
    /// Returns an `Err` if the associated [`EventLoop`] no longer exists.
    ///
    /// [`UserEvent(event)`]: Event::UserEvent
    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        let _span = tracing::debug_span!("aterm_window::EventLoopProxy::send_event",).entered();

        self.event_loop_proxy.send_event(event)
    }
}

impl<T: 'static> fmt::Debug for EventLoopProxy<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("EventLoopProxy { .. }")
    }
}

/// The error that is returned when an [`EventLoopProxy`] attempts to wake up an [`EventLoop`] that
/// no longer exists.
///
/// Contains the original event given to [`EventLoopProxy::send_event`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EventLoopClosed<T>(pub T);

impl<T> fmt::Display for EventLoopClosed<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tried to wake up a closed `EventLoop`")
    }
}

impl<T: fmt::Debug> error::Error for EventLoopClosed<T> {}

/// Control when device events are captured.
#[cfg(windows_platform)]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum DeviceEvents {
    /// Report device events regardless of window focus.
    Always,
    /// Only capture device events while the window is focused.
    #[default]
    WhenFocused,
    /// Never capture device events.
    Never,
}

/// Shim for various run APIs.
#[inline(always)]
pub(crate) fn dispatch_event_for_app<T: 'static, A: ApplicationHandler<T>>(
    app: &mut A,
    event_loop: &ActiveEventLoop,
    event: Event<T>,
) {
    match event {
        Event::NewEvents(cause) => app.new_events(event_loop, cause),
        Event::WindowEvent { window_id, event } => app.window_event(event_loop, window_id, event),
        Event::DeviceEvent { device_id, event } => app.device_event(event_loop, device_id, event),
        Event::UserEvent(event) => app.user_event(event_loop, event),
        Event::Suspended => app.suspended(event_loop),
        Event::Resumed => app.resumed(event_loop),
        Event::AboutToWait => app.about_to_wait(event_loop),
        Event::LoopExiting => app.exiting(event_loop),
    }
}

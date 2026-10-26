// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/event_loop.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! LOCAL PATCH (aterm): the Android, iOS, Orbital and Web arms and the `rwh_05` impls are cut, and
//! so are custom cursors, device-event filters, owned display handles and the control-flow getter,
//! which no aterm crate calls.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{error, fmt};

use std::time::Instant;

use crate::application::ApplicationHandler;
use crate::error::{EventLoopError, OsError};
use crate::event::Event;
use crate::monitor::MonitorHandle;
use crate::platform_impl;
#[cfg(macos_platform)]
use crate::window::Theme;
use crate::window::{Window, WindowAttributes};

/// Provides a way to retrieve events from the system and from the windows that were registered to the events loop.
pub struct EventLoop<T: 'static> {
    pub(crate) event_loop: platform_impl::EventLoop<T>,
    pub(crate) _marker: PhantomData<*mut ()>,
}

/// Target that associates windows with an [`EventLoop`].
pub struct ActiveEventLoop {
    pub(crate) p: platform_impl::ActiveEventLoop,
    pub(crate) _marker: PhantomData<*mut ()>,
}

/// Object that allows building the event loop.
#[derive(Default)]
pub struct EventLoopBuilder<T: 'static> {
    pub(crate) platform_specific: platform_impl::PlatformSpecificEventLoopAttributes,
    _p: PhantomData<T>,
}

static EVENT_LOOP_CREATED: AtomicBool = AtomicBool::new(false);

impl<T> EventLoopBuilder<T> {
    /// Builds a new event loop.
    /// ## Panics
    ///
    /// Attempting to create the event loop off the main thread will panic.
    #[inline]
    pub fn build(&mut self) -> Result<EventLoop<T>, EventLoopError> {
        let _span = tracing::debug_span!("aterm_window::EventLoopBuilder::build").entered();

        if EVENT_LOOP_CREATED.swap(true, Ordering::Relaxed) {
            return Err(EventLoopError::RecreationAttempt);
        }

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
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ControlFlow {
    /// When the current loop iteration finishes, immediately begin a new iteration regardless of whether or not new events are available to process.
    Poll,

    /// When the current loop iteration finishes, suspend the thread until another event arrives.
    #[default]
    Wait,

    /// When the current loop iteration finishes, suspend the thread until either another event arrives or the given time is reached.
    WaitUntil(Instant),
}

impl EventLoop<()> {
    /// Create the event loop.
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
    /// Start building a new event loop, with the given type as the user event type.
    #[inline]
    pub fn with_user_event() -> EventLoopBuilder<T> {
        EventLoopBuilder { platform_specific: Default::default(), _p: PhantomData }
    }

    /// Run the application with the event loop on the calling thread.
    #[inline]
    pub fn run_app<A: ApplicationHandler<T>>(self, app: &mut A) -> Result<(), EventLoopError> {
        self.event_loop.run(|event, event_loop| dispatch_event_for_app(app, event_loop, event))
    }

    /// Creates an [`EventLoopProxy`] that can be used to dispatch user events to the main event loop, possibly from another thread.
    #[inline]
    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy { event_loop_proxy: self.event_loop.create_proxy() }
    }

    /// Sets the [`ControlFlow`].
    #[inline]
    pub fn set_control_flow(&self, control_flow: ControlFlow) {
        self.event_loop.window_target().p.set_control_flow(control_flow)
    }
}

#[cfg(feature = "rwh_06")]
impl<T> rwh_06::HasDisplayHandle for EventLoop<T> {
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        rwh_06::HasDisplayHandle::display_handle(self.event_loop.window_target())
    }
}

// LOCAL PATCH (aterm): no `AsFd`/`AsRawFd` for the X11 and Wayland loops: they wait in a `poll(2)`
// over several descriptors, and no aterm crate embeds the loop.
impl ActiveEventLoop {
    /// Create the window.
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

    /// Returns the list of all the monitors available on the system.
    #[inline]
    pub fn available_monitors(&self) -> impl Iterator<Item = MonitorHandle> {
        let _span =
            tracing::debug_span!("aterm_window::ActiveEventLoop::available_monitors",).entered();

        #[allow(clippy::useless_conversion)]
        self.p.available_monitors().into_iter().map(|inner| MonitorHandle { inner })
    }

    /// Returns the primary monitor of the system.
    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        let _span =
            tracing::debug_span!("aterm_window::ActiveEventLoop::primary_monitor",).entered();

        self.p.primary_monitor().map(|inner| MonitorHandle { inner })
    }

    /// Returns the current system theme.
    #[cfg(macos_platform)]
    #[inline]
    pub fn system_theme(&self) -> Option<Theme> {
        self.p.system_theme()
    }

    /// Sets the [`ControlFlow`].
    #[inline]
    pub fn set_control_flow(&self, control_flow: ControlFlow) {
        self.p.set_control_flow(control_flow)
    }

    /// This exits the event loop.
    #[inline]
    pub fn exit(&self) {
        let _span = tracing::debug_span!("aterm_window::ActiveEventLoop::exit",).entered();

        self.p.exit()
    }

    /// Returns if the [`EventLoop`] is about to stop.
    #[inline]
    pub fn exiting(&self) -> bool {
        self.p.exiting()
    }

    /// LOCAL PATCH (aterm): whether the user-event drain before this turn's
    /// [`ApplicationHandler::about_to_wait`] emptied the queue, so every event sent before it began
    /// reached [`ApplicationHandler::user_event`]. The macOS drain never waits on a sender: one
    /// that meets a send in progress stops there and this is `false`, leaving that send and any
    /// made before it queued for the turn the sender's wake brings. Every other backend's drain
    /// waits out a send in progress, so this is always `true` there.
    ///
    /// [`ApplicationHandler::about_to_wait`]: crate::application::ApplicationHandler::about_to_wait
    /// [`ApplicationHandler::user_event`]: crate::application::ApplicationHandler::user_event
    #[inline]
    pub fn user_events_drained(&self) -> bool {
        #[cfg(macos_platform)]
        {
            self.p.user_events_drained()
        }
        #[cfg(not(macos_platform))]
        {
            true
        }
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

/// Used to send custom events to [`EventLoop`].
pub struct EventLoopProxy<T: 'static> {
    event_loop_proxy: platform_impl::EventLoopProxy<T>,
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    #[inline]
    fn clone(&self) -> Self {
        Self { event_loop_proxy: self.event_loop_proxy.clone() }
    }
}

impl<T: 'static> EventLoopProxy<T> {
    /// Send an event to the [`EventLoop`] from which this proxy was created.
    #[inline]
    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        let _span = tracing::debug_span!("aterm_window::EventLoopProxy::send_event",).entered();

        self.event_loop_proxy.send_event(event)
    }

    /// LOCAL PATCH (aterm): [`Self::send_event`] by a sender held part-way through its push, so a
    /// test (`aterm-gui`'s `headless_event_loop`) can make a drain meet a send in progress
    /// ([`ActiveEventLoop::user_events_drained`]). With the queue's lock taken it sends on `held`
    /// and waits for `release` (a one-minute hang detector), then pushes `event` and wakes the
    /// loop as [`Self::send_event`] does.
    #[cfg(all(macos_platform, any(test, debug_assertions)))]
    #[doc(hidden)]
    pub fn send_event_held_for_test(
        &self,
        event: T,
        held: &std::sync::mpsc::Sender<()>,
        release: &std::sync::mpsc::Receiver<()>,
    ) -> Result<(), EventLoopClosed<T>> {
        self.event_loop_proxy.send_event_held_for_test(event, held, release)
    }
}

impl<T: 'static> fmt::Debug for EventLoopProxy<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("EventLoopProxy { .. }")
    }
}

/// The error that is returned when an [`EventLoopProxy`] attempts to wake up an [`EventLoop`] that no longer exists.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EventLoopClosed<T>(pub T);

impl<T> fmt::Display for EventLoopClosed<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tried to wake up a closed `EventLoop`")
    }
}

impl<T: fmt::Debug> error::Error for EventLoopClosed<T> {}

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

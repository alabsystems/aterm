// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/application.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! End user application handling.

use crate::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use crate::event_loop::ActiveEventLoop;
use crate::window::WindowId;

/// The handler of the application events.
pub trait ApplicationHandler<T: 'static = ()> {
    /// Emitted when new events arrive from the OS to be processed.
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        let _ = (event_loop, cause);
    }

    /// Emitted when the application has been resumed.
    fn resumed(&mut self, event_loop: &ActiveEventLoop);

    /// Emitted when an event is sent from
    /// [`EventLoopProxy::send_event`](crate::event_loop::EventLoopProxy::send_event).
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: T) {
        let _ = (event_loop, event);
    }

    /// Emitted when the OS sends an event to a winit window.
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    );

    /// Emitted when the OS sends an event to a device.
    ///
    /// LOCAL PATCH (aterm): only the Windows backend emits it; the macOS `sendEvent:` override and
    /// the X11 backend no longer read device events.
    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: DeviceId,
        event: DeviceEvent,
    ) {
        let _ = (event_loop, device_id, event);
    }

    /// Emitted when the event loop is about to block and wait for new events.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let _ = event_loop;
    }

    /// Emitted when the application has been suspended.
    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        let _ = event_loop;
    }

    /// Emitted when the event loop is being shut down.
    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        let _ = event_loop;
    }
}

impl<A: ?Sized + ApplicationHandler<T>, T: 'static> ApplicationHandler<T> for &mut A {
    #[inline]
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        (**self).new_events(event_loop, cause);
    }

    #[inline]
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        (**self).resumed(event_loop);
    }

    #[inline]
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: T) {
        (**self).user_event(event_loop, event);
    }

    #[inline]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        (**self).window_event(event_loop, window_id, event);
    }

    #[inline]
    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: DeviceId,
        event: DeviceEvent,
    ) {
        (**self).device_event(event_loop, device_id, event);
    }

    #[inline]
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        (**self).about_to_wait(event_loop);
    }

    #[inline]
    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        (**self).suspended(event_loop);
    }

    #[inline]
    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        (**self).exiting(event_loop);
    }
}

impl<A: ?Sized + ApplicationHandler<T>, T: 'static> ApplicationHandler<T> for Box<A> {
    #[inline]
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        (**self).new_events(event_loop, cause);
    }

    #[inline]
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        (**self).resumed(event_loop);
    }

    #[inline]
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: T) {
        (**self).user_event(event_loop, event);
    }

    #[inline]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        (**self).window_event(event_loop, window_id, event);
    }

    #[inline]
    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: DeviceId,
        event: DeviceEvent,
    ) {
        (**self).device_event(event_loop, device_id, event);
    }

    #[inline]
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        (**self).about_to_wait(event_loop);
    }

    #[inline]
    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        (**self).suspended(event_loop);
    }

    #[inline]
    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        (**self).exiting(event_loop);
    }
}

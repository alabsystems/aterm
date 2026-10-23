// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/pump_events.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::time::Duration;

use crate::application::ApplicationHandler;
use crate::event_loop::{self, EventLoop};

/// Additional methods on [`EventLoop`] for pumping events within an external event loop.
pub trait EventLoopExtPumpEvents {
    /// A type provided by the user that can be passed through
    /// [`Event::UserEvent`](crate::event::Event::UserEvent).
    type UserEvent: 'static;

    /// Pump the `EventLoop` to check for and dispatch pending events, waiting up to `timeout`.
    fn pump_app_events<A: ApplicationHandler<Self::UserEvent>>(
        &mut self,
        timeout: Option<Duration>,
        app: &mut A,
    ) -> PumpStatus;
}

// LOCAL PATCH (aterm): upstream's deprecated closure form, `pump_events`, is gone;
// `pump_app_events` drives the backend's pump directly.
impl<T> EventLoopExtPumpEvents for EventLoop<T> {
    type UserEvent = T;

    fn pump_app_events<A: ApplicationHandler<Self::UserEvent>>(
        &mut self,
        timeout: Option<Duration>,
        app: &mut A,
    ) -> PumpStatus {
        self.event_loop.pump_events(timeout, |event, event_loop| {
            event_loop::dispatch_event_for_app(app, event_loop, event)
        })
    }
}

/// The return status for `pump_app_events`.
pub enum PumpStatus {
    /// Continue running external loop.
    Continue,
    /// Exit external loop.
    Exit(i32),
}

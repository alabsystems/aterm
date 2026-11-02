// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/event_loop/proxy.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! An event loop proxy.
//!
//! Modified by the aterm project in 2026; see the repository NOTICE. (A channel and the loop's
//! `common::poll::Waker`, where upstream sent through calloop's channel:
//! docs/AUDIT-third-party-2026-10-15.md, LW.7.)

use std::sync::mpsc::{SendError, Sender};

use crate::event_loop::EventLoopClosed;
use crate::platform_impl::common::poll::Waker;

/// A handle that can be sent across the threads and used to wake up the `EventLoop`.
pub struct EventLoopProxy<T: 'static> {
    user_events_sender: Sender<T>,
    waker: Waker,
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        EventLoopProxy {
            user_events_sender: self.user_events_sender.clone(),
            waker: self.waker.clone(),
        }
    }
}

impl<T: 'static> EventLoopProxy<T> {
    pub fn new(user_events_sender: Sender<T>, waker: Waker) -> Self {
        Self { user_events_sender, waker }
    }

    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        self.user_events_sender.send(event).map_err(|SendError(error)| EventLoopClosed(error))?;
        self.waker.wake();
        Ok(())
    }
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! Explicit windowless event loop extension trait.

use crate::event_loop::{EventLoop, EventLoopBuilder};

/// Additional methods on [`EventLoopBuilder`] for running without any display server.
pub trait EventLoopBuilderExtHeadless {
    /// Builds an event loop that talks to no display server at all.
    fn with_headless(&mut self) -> &mut Self;

    /// Builds the platform's own event loop on the first display server the environment names
    /// that answers (Wayland's, then X11's), and the headless one where none answers: none is
    /// named, or nothing listens where each is named ([`EventLoopExtHeadless::headless_fallback`]
    /// says why). A display server that answers, on which the platform's backend still fails, is
    /// that failure, as without this. On macOS a windowless run needs no display server, so this
    /// is [`Self::with_headless`].
    fn with_headless_fallback(&mut self) -> &mut Self;
}

impl<T> EventLoopBuilderExtHeadless for EventLoopBuilder<T> {
    #[inline]
    fn with_headless(&mut self) -> &mut Self {
        #[cfg(free_unix)]
        {
            self.platform_specific.forced_backend = Some(crate::platform_impl::Backend::Headless);
        }
        #[cfg(macos_platform)]
        {
            self.platform_specific.headless = true;
        }
        self
    }

    #[inline]
    fn with_headless_fallback(&mut self) -> &mut Self {
        #[cfg(free_unix)]
        {
            self.platform_specific.headless_fallback = true;
        }
        #[cfg(macos_platform)]
        {
            self.platform_specific.headless = true;
        }
        self
    }
}

/// Additional methods on [`EventLoop`] built by [`EventLoopBuilderExtHeadless`].
pub trait EventLoopExtHeadless {
    /// Why this event loop runs without a display server though
    /// [`EventLoopBuilderExtHeadless::with_headless_fallback`] asked for the platform's own first:
    /// the error that backend gave. `None` where it runs on that backend, where it was asked for
    /// as headless, and on macOS.
    fn headless_fallback(&self) -> Option<&str>;
}

impl<T> EventLoopExtHeadless for EventLoop<T> {
    #[inline]
    fn headless_fallback(&self) -> Option<&str> {
        #[cfg(free_unix)]
        {
            self.event_loop.headless_fallback()
        }
        #[cfg(not(free_unix))]
        {
            None
        }
    }
}

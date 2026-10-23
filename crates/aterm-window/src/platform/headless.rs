// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! Explicit windowless event loop extension trait.

use crate::event_loop::EventLoopBuilder;

/// Additional methods on [`EventLoopBuilder`] for running without any display server.
pub trait EventLoopBuilderExtHeadless {
    /// Builds an event loop that talks to no display server at all.
    fn with_headless(&mut self) -> &mut Self;
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
}

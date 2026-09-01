//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! Windowless event loops.
//!
//! Selecting this backend is always EXPLICIT. `EventLoop::new` will never fall back to it
//! by probing the environment, because a windowed run that has lost its display must keep
//! failing loudly rather than coming up invisible and looking healthy.

use crate::event_loop::EventLoopBuilder;

/// Additional methods on [`EventLoopBuilder`] for running without any display server.
pub trait EventLoopBuilderExtHeadless {
    /// Build an event loop that talks to no display server at all.
    ///
    /// The loop still delivers user events from [`EventLoopProxy`], honours
    /// [`ControlFlow`] (including `WaitUntil` deadlines) and emits the same
    /// `NewEvents` / `Resumed` / `UserEvent` / `AboutToWait` / `LoopExiting` sequence as
    /// the X11 and Wayland backends, so an application handler cannot tell them apart.
    /// What it cannot do is create a window or enumerate a monitor: those need a surface,
    /// and a surface needs a display.
    ///
    /// This is what lets a windowless process run under CI, in a container, over a plain
    /// SSH session, or under `env -i` — none of which have `DISPLAY` or `WAYLAND_DISPLAY`
    /// set, and all of which would otherwise fail to build an event loop at all.
    ///
    /// [`EventLoopProxy`]: crate::event_loop::EventLoopProxy
    /// [`ControlFlow`]: crate::event_loop::ControlFlow
    fn with_headless(&mut self) -> &mut Self;
}

impl<T> EventLoopBuilderExtHeadless for EventLoopBuilder<T> {
    #[inline]
    fn with_headless(&mut self) -> &mut Self {
        self.platform_specific.forced_backend = Some(crate::platform_impl::Backend::Headless);
        self
    }
}

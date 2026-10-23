// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/event_handler.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::cell::RefCell;
use std::{fmt, mem};

use super::app_state::HandlePendingUserEvents;
use crate::event::Event;
use crate::event_loop::ActiveEventLoop as RootActiveEventLoop;

struct EventHandlerData {
    #[allow(clippy::type_complexity)]
    handler: Box<dyn FnMut(Event<HandlePendingUserEvents>, &RootActiveEventLoop) + 'static>,
}

impl fmt::Debug for EventHandlerData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventHandlerData").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct EventHandler {
    /// This can be in the following states:
    /// - Not registered by the event loop (None).
    /// - Present (Some(handler)).
    /// - Currently executing the handler / in use (RefCell borrowed).
    inner: RefCell<Option<EventHandlerData>>,
}

impl EventHandler {
    pub(crate) const fn new() -> Self {
        Self { inner: RefCell::new(None) }
    }

    /// Set the event loop handler for the duration of the given closure.
    pub(crate) fn set<'handler, R>(
        &self,
        handler: impl FnMut(Event<HandlePendingUserEvents>, &RootActiveEventLoop) + 'handler,
        closure: impl FnOnce() -> R,
    ) -> R {
        // SAFETY: We extend the lifetime of the handler here so that we can
        // store it in `EventHandler`'s `RefCell`.
        //
        // This is sound, since we make sure to unset the handler again at the
        // end of this function, and as such the lifetime isn't actually
        // extended beyond `'handler`.
        let handler = unsafe {
            mem::transmute::<
                Box<dyn FnMut(Event<HandlePendingUserEvents>, &RootActiveEventLoop) + 'handler>,
                Box<dyn FnMut(Event<HandlePendingUserEvents>, &RootActiveEventLoop) + 'static>,
            >(Box::new(handler))
        };

        match self.inner.try_borrow_mut().as_deref_mut() {
            Ok(Some(_)) => {
                unreachable!("tried to set handler while another was already set");
            },
            Ok(data @ None) => {
                *data = Some(EventHandlerData { handler });
            },
            Err(_) => {
                unreachable!("tried to set handler that is currently in use");
            },
        }

        struct ClearOnDrop<'a>(&'a EventHandler);

        impl Drop for ClearOnDrop<'_> {
            fn drop(&mut self) {
                match self.0.inner.try_borrow_mut().as_deref_mut() {
                    Ok(data @ Some(_)) => {
                        *data = None;
                    },
                    Ok(None) => {
                        tracing::error!("tried to clear handler, but no handler was set");
                    },
                    Err(_) => {
                        eprintln!("tried to clear handler that is currently in use");
                        #[expect(
                            clippy::disallowed_methods,
                            reason = "upstream's soundness backstop, unreachable while this module \
                                      owns the RefCell; the carve (winit-core step 5) decides \
                                      whether it dies by kill_self_now instead"
                        )]
                        std::process::abort();
                    },
                }
            }
        }

        let _clear_on_drop = ClearOnDrop(self);

        closure()
    }

    pub(crate) fn in_use(&self) -> bool {
        self.inner.try_borrow().is_err()
    }

    pub(crate) fn ready(&self) -> bool {
        matches!(self.inner.try_borrow().as_deref(), Ok(Some(_)))
    }

    pub(crate) fn handle_event(
        &self,
        event: Event<HandlePendingUserEvents>,
        event_loop: &RootActiveEventLoop,
    ) {
        match self.inner.try_borrow_mut().as_deref_mut() {
            Ok(Some(EventHandlerData { handler })) => {
                (handler)(event, event_loop);
            },
            Ok(None) => {
                tracing::error!("tried to run event handler, but no handler was set");
            },
            Err(_) => {
                panic!("tried to handle event while another event is currently being handled");
            },
        }
    }
}

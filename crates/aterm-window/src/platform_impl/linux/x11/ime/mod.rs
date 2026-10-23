// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/ime/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Important: all XIM calls need to happen from the same thread!

mod callbacks;
mod context;
mod inner;
mod input_method;

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use self::callbacks::*;
use self::context::ImeContext;
pub use self::context::ImeContextCreationError;
#[cfg(test)]
pub(crate) use self::context::{FOCUSES_SENT, SPOTS_SENT};
use self::inner::{close_im, ImeInner};
use self::input_method::PotentialInputMethods;
use super::{ffi, util, XConnection, XError};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImeEvent {
    Enabled,
    Start,
    Update(String, usize),
    End,
    Disabled,
}

pub type ImeReceiver = Receiver<ImeRequest>;
pub type ImeSender = Sender<ImeRequest>;
pub type ImeEventReceiver = Receiver<(ffi::Window, ImeEvent)>;
pub type ImeEventSender = Sender<(ffi::Window, ImeEvent)>;

/// Request to control XIM handler from the window.
pub enum ImeRequest {
    /// Set IME spot position for given `window_id`.
    Position(ffi::Window, i16, i16),

    /// Allow IME input for the given `window_id`.
    Allow(ffi::Window, bool),
}

#[derive(Debug)]
pub(crate) enum ImeCreationError {
    OpenFailure(Box<PotentialInputMethods>),
    SetDestroyCallbackFailed(#[allow(dead_code)] XError),
}

pub(crate) struct Ime {
    xconn: Arc<XConnection>,
    inner: Box<ImeInner>,
}

impl Ime {
    pub fn new(
        xconn: Arc<XConnection>,
        event_sender: ImeEventSender,
    ) -> Result<Self, ImeCreationError> {
        let potential_input_methods = PotentialInputMethods::new(&xconn);

        let (mut inner, client_data) = {
            let mut inner = Box::new(ImeInner::new(xconn, potential_input_methods, event_sender));
            let inner_ptr = Box::into_raw(inner);
            let client_data = inner_ptr as _;
            let destroy_callback =
                ffi::XIMCallback { client_data, callback: Some(xim_destroy_callback) };
            inner = unsafe { Box::from_raw(inner_ptr) };
            inner.destroy_callback = destroy_callback;
            (inner, client_data)
        };

        let xconn = Arc::clone(&inner.xconn);

        let input_method = inner.potential_input_methods.open_im(
            &xconn,
            Some(&|| {
                let _ = unsafe { set_instantiate_callback(&xconn, client_data) };
            }),
        );

        let is_fallback = input_method.is_fallback();
        if let Some(input_method) = input_method.ok() {
            inner.is_fallback = is_fallback;
            unsafe {
                let result = set_destroy_callback(&xconn, input_method.im, &inner)
                    .map_err(ImeCreationError::SetDestroyCallbackFailed);
                if result.is_err() {
                    let _ = close_im(&xconn, input_method.im);
                }
                result?;
            }
            inner.im = Some(input_method);
            Ok(Ime { xconn, inner })
        } else {
            Err(ImeCreationError::OpenFailure(Box::new(inner.potential_input_methods)))
        }
    }

    pub fn is_destroyed(&self) -> bool {
        self.inner.is_destroyed
    }

    pub fn create_context(
        &mut self,
        window: ffi::Window,
        with_ime: bool,
    ) -> Result<bool, ImeContextCreationError> {
        let context = if self.is_destroyed() {
            None
        } else {
            let im = self.inner.im.as_ref().unwrap();

            let context = unsafe {
                ImeContext::new(
                    &self.inner.xconn,
                    im,
                    window,
                    self.inner.spot_for_new_context(window),
                    self.inner.event_sender.clone(),
                    with_ime,
                )?
            };
            if self.inner.focused == Some(window) {
                if let Err(error) = context.focus(&self.xconn) {
                    self.xconn.report("Failed to focus input context", error);
                }
            }

            let event = if context.is_allowed() { ImeEvent::Enabled } else { ImeEvent::Disabled };
            self.inner.event_sender.send((window, event)).expect("Failed to send enabled event");

            Some(context)
        };

        self.inner.contexts.insert(window, context);
        Ok(!self.is_destroyed())
    }

    pub fn get_context(&self, window: ffi::Window) -> Option<ffi::XIC> {
        if self.is_destroyed() {
            return None;
        }
        if let Some(Some(context)) = self.inner.contexts.get(&window) {
            Some(context.ic)
        } else {
            None
        }
    }

    /// Destroy `window`'s input context and clear its tracked state.
    pub fn remove_context(&mut self, window: ffi::Window) -> Result<bool, XError> {
        self.inner.spots.remove(&window);
        if self.inner.focused == Some(window) {
            self.inner.focused = None;
        }
        self.destroy_context(window)
    }

    fn destroy_context(&mut self, window: ffi::Window) -> Result<bool, XError> {
        if let Some(Some(context)) = self.inner.contexts.remove(&window) {
            unsafe {
                self.inner.destroy_ic_if_necessary(context.ic)?;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Focus `window`'s input context and apply its latest queued spot.
    pub fn focus(&mut self, window: ffi::Window) -> Result<bool, XError> {
        self.inner.focused = Some(window);
        if self.is_destroyed() {
            return Ok(false);
        }
        let spot = self.inner.spots.get(&window).copied();
        if let Some(&mut Some(ref mut context)) = self.inner.contexts.get_mut(&window) {
            if let Some(spot) = spot {
                context.set_spot(&self.xconn, spot.x, spot.y);
            }
            context.focus(&self.xconn).map(|_| true)
        } else {
            Ok(false)
        }
    }

    pub fn unfocus(&mut self, window: ffi::Window) -> Result<bool, XError> {
        if self.inner.focused == Some(window) {
            self.inner.focused = None;
        }
        if self.is_destroyed() {
            return Ok(false);
        }
        if let Some(&mut Some(ref mut context)) = self.inner.contexts.get_mut(&window) {
            context.unfocus(&self.xconn).map(|_| true)
        } else {
            Ok(false)
        }
    }

    /// Record `window`'s caret spot and update its input context when focused.
    pub fn send_xim_spot(&mut self, window: ffi::Window, x: i16, y: i16) {
        self.inner.spots.insert(window, ffi::XPoint { x, y });
        if self.is_destroyed() || self.inner.focused != Some(window) {
            return;
        }
        if let Some(&mut Some(ref mut context)) = self.inner.contexts.get_mut(&window) {
            context.set_spot(&self.xconn, x as _, y as _);
        }
    }

    pub fn set_ime_allowed(&mut self, window: ffi::Window, allowed: bool) {
        if self.is_destroyed() {
            return;
        }

        if let Some(&mut Some(ref mut context)) = self.inner.contexts.get_mut(&window) {
            if allowed == context.is_allowed() {
                return;
            }
        }

        let _ = self.destroy_context(window);
        let _ = self.create_context(window, allowed);
    }

    pub fn is_ime_allowed(&self, window: ffi::Window) -> bool {
        if self.is_destroyed() {
            false
        } else if let Some(Some(context)) = self.inner.contexts.get(&window) {
            context.is_allowed()
        } else {
            false
        }
    }

    /// Replace the input method as its instantiate and destroy callbacks do (an IBus restart),
    /// for a test; whether the replacement opened.
    #[cfg(test)]
    pub(crate) fn replace_for_test(&mut self) -> bool {
        // SAFETY: the boxed inner the callbacks are handed, alive and borrowed by nothing else.
        unsafe { replace_im_for_test(&mut *self.inner) }
    }

    /// The sender the input method's callbacks queue their events on, for a test.
    #[cfg(test)]
    pub(crate) fn event_sender_for_test(&self) -> ImeEventSender {
        self.inner.event_sender.clone()
    }
}

impl Drop for Ime {
    fn drop(&mut self) {
        unsafe {
            let _ = self.inner.destroy_all_contexts_if_necessary();
            let _ = self.inner.close_im_if_necessary();
        }
    }
}

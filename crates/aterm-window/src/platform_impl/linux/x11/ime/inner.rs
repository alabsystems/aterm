// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/ime/inner.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::collections::HashMap;
use std::mem;
use std::sync::Arc;

use super::{ffi, XConnection, XError};

use super::context::ImeContext;
use super::input_method::{InputMethod, PotentialInputMethods};
use crate::platform_impl::platform::x11::ime::{ImeEvent, ImeEventSender};

pub(crate) unsafe fn close_im(xconn: &Arc<XConnection>, im: ffi::XIM) -> Result<(), XError> {
    unsafe { (xconn.xlib.XCloseIM)(im) };
    xconn.check_errors()
}

pub(crate) unsafe fn destroy_ic(xconn: &Arc<XConnection>, ic: ffi::XIC) -> Result<(), XError> {
    unsafe { (xconn.xlib.XDestroyIC)(ic) };
    xconn.check_errors()
}

pub(crate) struct ImeInner {
    pub xconn: Arc<XConnection>,
    pub im: Option<InputMethod>,
    pub potential_input_methods: PotentialInputMethods,
    pub contexts: HashMap<ffi::Window, Option<ImeContext>>,
    // WARNING: this is initially zeroed!
    pub destroy_callback: ffi::XIMCallback,
    pub event_sender: ImeEventSender,
    // Indicates whether or not the input method was destroyed on the server end
    // (i.e. if ibus/fcitx/etc. was terminated/restarted)
    pub is_destroyed: bool,
    pub is_fallback: bool,
    /// The window whose input context has the focus (`Ime::focus`), the one window whose caret
    /// the input method is told.
    pub focused: Option<ffi::Window>,
    /// Each window's latest caret spot (`Ime::send_xim_spot`), which its input context is told
    /// while the window has the focus.
    pub spots: HashMap<ffi::Window, ffi::XPoint>,
}

impl ImeInner {
    pub(crate) fn new(
        xconn: Arc<XConnection>,
        potential_input_methods: PotentialInputMethods,
        event_sender: ImeEventSender,
    ) -> Self {
        ImeInner {
            xconn,
            im: None,
            potential_input_methods,
            contexts: HashMap::new(),
            destroy_callback: unsafe { mem::zeroed() },
            event_sender,
            is_destroyed: false,
            is_fallback: false,
            focused: None,
            spots: HashMap::new(),
        }
    }

    /// The spot a new input context for `window` starts with: the window's latest while it has
    /// the focus. Another window's is sent when it gains the focus (`Ime::focus`).
    pub(crate) fn spot_for_new_context(&self, window: ffi::Window) -> Option<ffi::XPoint> {
        self.spots.get(&window).copied().filter(|_| self.focused == Some(window))
    }

    /// Give the focused window's input context the focus, which a context the input method
    /// remade (`replace_im`) starts without: IBus gives its engine, and so compositions, only
    /// to a focused input context.
    pub(crate) fn refocus(&self) -> Result<(), XError> {
        match self.focused.and_then(|window| self.contexts.get(&window)) {
            Some(Some(context)) if !self.is_destroyed => context.focus(&self.xconn),
            _ => Ok(()),
        }
    }

    /// Whether the input method's server side is gone: the input method was destroyed, or the
    /// display was lost (X11-GONE), which takes the input method and its contexts with it. Xlib's
    /// `XDestroyIC` and `XCloseIM` on a lost display send to the input method server through a
    /// connection with no output buffer, and libX11 dies of SIGSEGV in `XSendEvent` (seen on a
    /// GNOME desk with IBus, 2026-10-22, where Xvfb with no input method never made a context).
    fn server_side_gone(&self) -> bool {
        self.is_destroyed || self.xconn.is_broken()
    }

    pub unsafe fn close_im_if_necessary(&self) -> Result<bool, XError> {
        match self.im.as_ref() {
            Some(im) if !self.server_side_gone() => unsafe {
                close_im(&self.xconn, im.im).map(|_| true)
            },
            _ => Ok(false),
        }
    }

    pub unsafe fn destroy_ic_if_necessary(&self, ic: ffi::XIC) -> Result<bool, XError> {
        if !self.server_side_gone() {
            unsafe { destroy_ic(&self.xconn, ic) }.map(|_| true)
        } else {
            Ok(false)
        }
    }

    pub unsafe fn destroy_all_contexts_if_necessary(&self) -> Result<bool, XError> {
        for context in self.contexts.values().flatten() {
            unsafe { self.destroy_ic_if_necessary(context.ic)? };
        }
        Ok(!self.server_side_gone())
    }

    /// Tell the event loop that every window's input context was replaced, as
    /// `Ime::create_context` tells it of one it makes: `ImeEvent::Enabled` for a context that
    /// takes compositions, `ImeEvent::Disabled` for one that does not or is gone (`gone`). A
    /// composition open in a replaced context ended with it, and the input method that went
    /// said nothing of it: the event loop ends it, and the application drops its preedit.
    pub(crate) fn announce_contexts(&self, gone: bool) {
        for (&window, context) in &self.contexts {
            let allowed = !gone && context.as_ref().is_some_and(ImeContext::is_allowed);
            let event = if allowed { ImeEvent::Enabled } else { ImeEvent::Disabled };
            // Called from Xlib's input method callbacks: a closed loop is not a panic there.
            let _ = self.event_sender.send((window, event));
        }
    }
}

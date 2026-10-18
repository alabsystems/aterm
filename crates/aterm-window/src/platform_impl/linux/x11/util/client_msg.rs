// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/client_msg.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (The message is
// aterm-x11's `ClientMessage`, sent with its SendEvent encoder.)

use aterm_x11::proto::event::ClientMessage;

use super::*;

impl XConnection {
    pub fn send_client_msg(
        &self,
        window: xproto::Window, // The window this is "about"; not necessarily this window
        target_window: xproto::Window, // The window we're sending to
        message_type: xproto::Atom,
        event_mask: Option<u32>,
        data: [u32; 5],
    ) -> Result<VoidCookie<'_>, X11Error> {
        let event = ClientMessage::new32(window, message_type, data);
        self.send_void(xproto::send_event(
            false,
            target_window,
            event_mask.unwrap_or(xproto::event_mask::NO_EVENT),
            &event.to_bytes(),
        ))
    }
}

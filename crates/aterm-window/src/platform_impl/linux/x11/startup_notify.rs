// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/activation.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE. (Only the launchee's
//! half survives: naming the launch on the window and ending its sequence. Requesting a
//! token for a launch of our own stays cut with the rest of `startup_notify`. The messages
//! are aterm-x11's `ClientMessage`s, where upstream used x11rb's.)
//!
//! X11 startup notification, the launchee's side:
//! <https://specifications.freedesktop.org/startup-notification-spec/startup-notification-latest.txt>
//!
//! A launcher that reads `StartupNotify=true` in a desktop entry starts a sequence,
//! shows a busy cursor or spinner for it, and hands the launched program its id in
//! `DESKTOP_STARTUP_ID`. The program names that id on its window (`_NET_STARTUP_ID`,
//! which the window manager reads at map for the launch's timestamp and workspace)
//! and ends the sequence with a `remove` message once the window is shown.
//!
//! A running window activated by another client with such an id (GNOME Shell's
//! `ActivationToken` for a click on a notification) ends that sequence the same way, and
//! stamps its raise with the id's `_TIME` ([`startup_time`]).

use aterm_x11::proto::event::ClientMessage;
use aterm_x11::proto::xproto;

use super::atoms::*;
use super::{IgnoreError as _, VoidCookie, X11Error, XConnection};

impl XConnection {
    /// Name the launch `startup_id` on `window` (`_NET_STARTUP_ID`). Set before the
    /// window maps: the window manager reads it then.
    pub(crate) fn set_startup_id(
        &self,
        window: xproto::Window,
        startup_id: &str,
    ) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.atoms();
        self.change_property(
            window,
            atoms[_NET_STARTUP_ID],
            atoms[UTF8_STRING],
            xproto::PropMode::Replace,
            startup_id.as_bytes(),
        )
    }

    /// End the launch sequence `startup_id`: its `remove` message, broadcast on the
    /// root window from `window` (a window of ours that outlives the send).
    pub(crate) fn end_startup_sequence(
        &self,
        window: xproto::Window,
        startup_id: &str,
    ) -> Result<(), X11Error> {
        let atoms = self.atoms();
        let root = self.default_root().root;
        let mut message_type = atoms[_NET_STARTUP_INFO_BEGIN];
        for chunk in remove_message(startup_id).chunks(20) {
            let mut data = [0u8; 20];
            data[..chunk.len()].copy_from_slice(chunk);
            let event = ClientMessage { format: 8, sequence: 0, window, type_: message_type, data };
            // Every chunk after the first continues the message.
            message_type = atoms[_NET_STARTUP_INFO];
            self.send_void(xproto::send_event(
                false,
                root,
                xproto::event_mask::PROPERTY_CHANGE,
                &event.to_bytes(),
            ))?
            .ignore_error();
        }
        self.flush_requests()?;
        Ok(())
    }
}

/// The X server time of the user action behind `startup_id`: the decimal stamp after its
/// last `_TIME`, which a launcher appends (libstartup-notification's
/// `<launcher>/<launchee>/<pid>-<seq>-<host>_TIME<time>`) and GTK reads the same way.
/// `None` for an id with no stamp, or one past the server's 32-bit clock.
pub(crate) fn startup_time(startup_id: &str) -> Option<u32> {
    let (_, after) = startup_id.rsplit_once("_TIME")?;
    let digits = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
    after[..digits].parse().ok()
}

/// The `remove` message for `startup_id`, NUL-terminated as the spec frames it.
fn remove_message(startup_id: &str) -> Vec<u8> {
    let mut message = b"remove: ID=".to_vec();
    quote_value(startup_id, &mut message);
    message.push(0);
    message
}

/// Quote a value as the specification reads one: in double quotes, with `"` and `\`
/// escaped by a backslash.
fn quote_value(value: &str, target: &mut Vec<u8>) {
    target.push(b'"');
    for &byte in value.as_bytes() {
        if byte == b'"' || byte == b'\\' {
            target.push(b'\\');
        }
        target.push(byte);
    }
    target.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The message a launcher's monitor parses: the key, the quoted id with its
    /// quotes and backslashes escaped, and the terminating NUL.
    #[test]
    fn the_remove_message_quotes_the_id_and_ends_in_nul() {
        assert_eq!(remove_message(""), b"remove: ID=\"\"\0");
        assert_eq!(
            remove_message("gnome-shell/aterm/4242-0-host_TIME5678"),
            b"remove: ID=\"gnome-shell/aterm/4242-0-host_TIME5678\"\0"
        );
        assert_eq!(remove_message("a\"b\\c d"), b"remove: ID=\"a\\\"b\\\\c d\"\0");
    }

    /// The stamp a raise is made for: the digits after the id's last `_TIME`, as GNOME
    /// Shell's activation token for a notification click carries them. An id with no stamp,
    /// an empty one, or one past 32 bits has none, and the raise asks with `CURRENT_TIME`.
    #[test]
    fn a_startup_id_s_time_is_the_stamp_after_its_last_time() {
        assert_eq!(startup_time("gnome-shell/aterm/4242-0-host_TIME5678"), Some(5678));
        assert_eq!(startup_time("a_TIME1-b_TIME42"), Some(42));
        assert_eq!(startup_time("x_TIME77+junk"), Some(77));
        assert_eq!(startup_time("x_TIME4294967295"), Some(u32::MAX));
        for none in ["", "gnome-shell/aterm/4242-0-host", "x_TIME", "x_TIMEz9", "x_TIME4294967296"]
        {
            assert_eq!(startup_time(none), None, "{none:?}");
        }
    }
}

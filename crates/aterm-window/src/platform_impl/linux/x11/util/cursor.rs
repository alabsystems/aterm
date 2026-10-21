// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (A window shows one of the
// named icons aterm sets: custom cursor images and the hidden cursor, which only the cut
// `create_custom_cursor` and `set_cursor_visible` reached, left with WC.8. A cursor that cannot
// be set is reported, not a panic: `XConnection::report`; none is loaded once the connection is
// broken: `set_cursor_icon`.)

use std::ffi::CString;
use std::iter;

use aterm_x11::proto::xproto::cw;
use aterm_x11::proto::ValueList;

use crate::window::CursorIcon;

use super::*;

impl XConnection {
    /// Show `cursor` on `window`, its image loaded on first use (Xcursor, through Xlib). Not
    /// once the connection is broken: Xlib then stops emptying its request buffer (`_XSend`
    /// returns early once the display has had its IO error), a cursor's load fills it, and past
    /// its end `_XGetRequest` gives Xcursor a NULL request it writes through. That was a SIGSEGV
    /// inside libXcursor in the lost-display test (`gone_tests`, the Linux lane, 2026-10-21:
    /// "Xlib: request 98 length 8 would exceed buffer size."). A display that has gone shows no
    /// cursor, and the loop's next look ends the run with the loss.
    pub fn set_cursor_icon(&self, window: xproto::Window, cursor: CursorIcon) {
        if self.is_broken() {
            return;
        }
        let cursor = *self
            .cursor_cache
            .lock()
            .unwrap()
            .entry(cursor)
            .or_insert_with(|| self.get_cursor(cursor));

        if let Err(error) = self.update_cursor(window, cursor) {
            self.report("Failed to set cursor", error);
        }
    }

    fn get_cursor(&self, cursor: CursorIcon) -> ffi::Cursor {
        let mut xcursor = 0;
        for &name in iter::once(&cursor.name()).chain(cursor.alt_names().iter()) {
            let name = CString::new(name).unwrap();
            xcursor = unsafe {
                (self.xcursor.XcursorLibraryLoadCursor)(
                    self.display,
                    name.as_ptr() as *const c_char,
                )
            };

            if xcursor != 0 {
                break;
            }
        }

        xcursor
    }

    fn update_cursor(&self, window: xproto::Window, cursor: ffi::Cursor) -> Result<(), X11Error> {
        self.send_void(xproto::change_window_attributes(
            window,
            &ValueList::new().set(cw::CURSOR, cursor as xproto::Cursor),
        ))?
        .ignore_error();

        self.xcb_connection().flush()?;
        Ok(())
    }
}

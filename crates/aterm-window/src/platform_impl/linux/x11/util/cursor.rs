// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (A window shows one of the
// named icons aterm sets: custom cursor images and the hidden cursor are cut. A cursor that cannot
// be set is reported, not a panic: `XConnection::report`; none is loaded once the connection is
// broken: `set_cursor_icon`. The cursors follow the resource database's theme and size as it
// changes: `follow_cursor_settings`.)

use std::ffi::{CStr, CString};
use std::iter;

use aterm_x11::proto::xproto::cw;
use aterm_x11::proto::ValueList;
use aterm_x11::xrm::Database;

use crate::window::CursorIcon;

use super::*;

/// Pointer cursor theme and size read from the X resource database (`Xcursor.theme`, `Xcursor.size`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CursorSettings {
    theme: Option<String>,
    size: Option<c_int>,
}

impl CursorSettings {
    pub(crate) fn of(database: &Database) -> CursorSettings {
        let value = |name| database.get_string(name, "").map(str::trim).filter(|v| !v.is_empty());
        CursorSettings {
            theme: value("Xcursor.theme").map(str::to_owned),
            size: value("Xcursor.size").and_then(|size| size.parse().ok()).filter(|&s| s > 0),
        }
    }
}

/// Tracks changes to the X resource database's cursor settings ([`XConnection::follow_cursor_settings`]).
#[derive(Debug)]
pub(crate) struct CursorFollow {
    /// The settings at the database's last reading, which a change is measured against.
    seen: CursorSettings,
    /// libXcursor's own theme and size before the connection first changed them (taken from
    /// `XCURSOR_THEME` and `XCURSOR_SIZE`, else the database Xlib read at the connection's
    /// start, else the screen's size), which a setting the database drops goes back to.
    initial: Option<(Option<CString>, c_int)>,
}

impl CursorFollow {
    /// Following from the database the connection started with.
    pub(crate) fn new(database: &Database) -> CursorFollow {
        CursorFollow { seen: CursorSettings::of(database), initial: None }
    }
}

/// A cursor's load: its shape, and the size and theme libXcursor held at the load.
#[cfg(test)]
pub(crate) type CursorLoad = (CursorIcon, c_int, Option<CString>);
#[cfg(test)]
thread_local! {
    /// Each cursor loaded on this thread ([`XConnection::set_cursor_icon`]), for a test.
    pub(crate) static CURSOR_LOADS: std::cell::RefCell<Vec<CursorLoad>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl XConnection {
    /// Show `cursor` on `window`, loading its image on first use unless the connection is broken.
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

    /// Update libXcursor's theme and size from the reloaded resource database and return whether they changed.
    pub fn follow_cursor_settings(&self) -> bool {
        if self.is_broken() {
            return false;
        }
        let now = CursorSettings::of(&self.database());
        let mut follow = self.cursor_follow.lock().unwrap_or_else(|e| e.into_inner());
        if follow.seen == now {
            return false;
        }
        let (theme, size) = follow.initial.get_or_insert_with(|| self.xcursor_settings()).clone();
        if follow.seen.theme != now.theme {
            let theme = now.theme.as_deref().and_then(|theme| CString::new(theme).ok()).or(theme);
            let theme = theme.as_ref().map_or(std::ptr::null(), |theme| theme.as_ptr());
            // SAFETY: the live display; libXcursor copies the name.
            unsafe { (self.xcursor.XcursorSetTheme)(self.display, theme) };
        }
        if follow.seen.size != now.size {
            // SAFETY: the live display.
            unsafe { (self.xcursor.XcursorSetDefaultSize)(self.display, now.size.unwrap_or(size)) };
        }
        follow.seen = now;
        drop(follow);

        let loaded: Vec<ffi::Cursor> =
            self.cursor_cache.lock().unwrap().drain().map(|(_, cursor)| cursor).collect();
        for cursor in loaded.into_iter().filter(|&cursor| cursor != 0) {
            // SAFETY: the live display, and a cursor this connection loaded and no longer
            // hands out.
            unsafe { (self.xlib.XFreeCursor)(self.display, cursor) };
        }
        true
    }

    /// The theme and size libXcursor loads cursors at for this display.
    pub(crate) fn xcursor_settings(&self) -> (Option<CString>, c_int) {
        // SAFETY: the live display; the theme is libXcursor's string, copied before any other
        // call can free it.
        unsafe {
            let theme = (self.xcursor.XcursorGetTheme)(self.display);
            let theme = (!theme.is_null()).then(|| CStr::from_ptr(theme).to_owned());
            (theme, (self.xcursor.XcursorGetDefaultSize)(self.display))
        }
    }

    fn get_cursor(&self, cursor: CursorIcon) -> ffi::Cursor {
        #[cfg(test)]
        CURSOR_LOADS.with(|loads| {
            let (theme, size) = self.xcursor_settings();
            loads.borrow_mut().push((cursor, size, theme))
        });
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

// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// LOCAL PATCH (aterm): custom cursors are API no aterm crate uses, and no backend makes them any
// more: a cursor is one of the named icons.
use crate::cursor_icon::CursorIcon;

/// See [`Window::set_cursor()`][crate::window::Window::set_cursor] for more details.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Cursor {
    Icon(CursorIcon),
}

impl Default for Cursor {
    #[inline]
    fn default() -> Self {
        Self::Icon(CursorIcon::default())
    }
}

impl From<CursorIcon> for Cursor {
    #[inline]
    fn from(icon: CursorIcon) -> Self {
        Self::Icon(icon)
    }
}

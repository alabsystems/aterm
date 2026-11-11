// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/types/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Wayland cursor-shape-v1 shapes and cursor theme lookup names.
//!
//! LOCAL PATCH (aterm): the pointer hands these to first-party `aterm-wayland` (its cursor-shape
//! tables, and libwayland-cursor), where upstream converted to cursor-icon's type for
//! smithay-client-toolkit's themed pointer. The custom cursor image and its shm buffer are cut:
//! no aterm caller.

use std::ffi::CString;

use aterm_wayland::protocols::cursor_shape_v1::wp_cursor_shape_device_v1::shape;

use crate::window::CursorIcon;

/// The icon's `cursor-shape-v1` protocol shape.
pub(crate) fn shape(icon: CursorIcon) -> u32 {
    match icon {
        CursorIcon::Default => shape::DEFAULT,
        CursorIcon::Pointer => shape::POINTER,
        CursorIcon::Text => shape::TEXT,
        CursorIcon::Grabbing => shape::GRABBING,
        CursorIcon::ColResize => shape::COL_RESIZE,
        CursorIcon::RowResize => shape::ROW_RESIZE,
        CursorIcon::NResize => shape::N_RESIZE,
        CursorIcon::NeResize => shape::NE_RESIZE,
        CursorIcon::EResize => shape::E_RESIZE,
        CursorIcon::SeResize => shape::SE_RESIZE,
        CursorIcon::SResize => shape::S_RESIZE,
        CursorIcon::SwResize => shape::SW_RESIZE,
        CursorIcon::WResize => shape::W_RESIZE,
        CursorIcon::NwResize => shape::NW_RESIZE,
    }
}

/// Cursor theme names in lookup order (CSS name followed by legacy Xcursor names).
pub(crate) fn theme_names(icon: CursorIcon) -> Vec<CString> {
    std::iter::once(icon.name())
        .chain(icon.alt_names().iter().copied())
        .filter_map(|name| CString::new(name).ok())
        .collect()
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;

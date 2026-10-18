// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/types/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, LW.7): the named
//! cursors as cursor-shape-v1 shapes and as cursor theme names, which the pointer hands to
//! first-party `aterm-wayland` (its cursor-shape tables, and libwayland-cursor), where upstream
//! converted to cursor-icon's type for smithay-client-toolkit's themed pointer
//! (`cursor_icon_to_shape`; the names are cursor-icon 1.2.0's, which `CursorIcon` records).
//! The custom cursor image and its shm buffer are CUT: no aterm caller.

use std::ffi::CString;

use aterm_wayland::protocols::cursor_shape_v1::wp_cursor_shape_device_v1::shape;

use crate::window::CursorIcon;

#[derive(Debug)]
pub enum SelectedCursor {
    Named(CursorIcon),
}

impl Default for SelectedCursor {
    fn default() -> Self {
        Self::Named(Default::default())
    }
}

/// The icon's cursor-shape-v1 shape (sctk 0.19.2's `cursor_icon_to_shape`).
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

/// The names a cursor theme may hold the icon under, in the order sctk tried them: its CSS name,
/// then its legacy Xcursor names.
pub(crate) fn theme_names(icon: CursorIcon) -> Vec<CString> {
    std::iter::once(icon.name())
        .chain(icon.alt_names().iter().copied())
        .filter_map(|name| CString::new(name).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [CursorIcon; 14] = [
        CursorIcon::Default,
        CursorIcon::Pointer,
        CursorIcon::Text,
        CursorIcon::Grabbing,
        CursorIcon::ColResize,
        CursorIcon::RowResize,
        CursorIcon::NResize,
        CursorIcon::NeResize,
        CursorIcon::EResize,
        CursorIcon::SeResize,
        CursorIcon::SResize,
        CursorIcon::SwResize,
        CursorIcon::WResize,
        CursorIcon::NwResize,
    ];

    /// Each icon has a shape of its own, and the protocol names the shape by the icon's CSS
    /// name (`n-resize` is `N_RESIZE`).
    #[test]
    fn every_icon_has_its_own_shape_named_as_its_css_name() {
        let mut shapes: Vec<u32> = ALL.iter().map(|i| shape(*i)).collect();
        shapes.sort_unstable();
        shapes.dedup();
        assert_eq!(shapes.len(), ALL.len());
        assert_eq!(shape(CursorIcon::NResize), shape::N_RESIZE);
        assert_eq!(shape(CursorIcon::ColResize), shape::COL_RESIZE);
    }

    #[test]
    fn a_theme_is_asked_for_the_css_name_first() {
        let names = theme_names(CursorIcon::Text);
        assert_eq!(names[0].as_bytes(), b"text");
        assert_eq!(names[1].as_bytes(), b"xterm");
    }
}

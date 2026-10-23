// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux X11 window icon (`_NET_WM_ICON`) from the bundled hicolor app icon.

/// The window icon an X11 window manager shows (`_NET_WM_ICON`).
pub(crate) fn window_icon() -> Option<aterm_window::window::Icon> {
    let (_, png) = atpkg::selfsetup::HICOLOR_ICONS
        .iter()
        .find(|(size, _)| *size == "128x128")?;
    let (rgba, width, height) = aterm_png::decode_rgba8(png, 128)?;
    aterm_window::window::Icon::from_rgba(
        rgba,
        u32::try_from(width).ok()?,
        u32::try_from(height).ok()?,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_icon_decodes() {
        assert!(window_icon().is_some());
    }
}

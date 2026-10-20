// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux window's X11 icon (`_NET_WM_ICON`), from the hicolor app icon the binary
//! carries for its desktop entry (`atpkg::selfsetup::HICOLOR_ICONS`, which `aterm pkg
//! shell-setup --desktop` installs when no checkout's icons are named).

/// The window icon an X11 window manager shows (`_NET_WM_ICON`): the 128 px hicolor
/// icon, one size for the manager to scale from. `None` if it does not decode, which
/// leaves the window without one.
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

    /// The X11 window icon decodes to a 128 px RGBA image.
    #[test]
    fn the_window_icon_decodes() {
        assert!(window_icon().is_some());
    }
}

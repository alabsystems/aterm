// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Window and notification-area icons on Windows, loaded at each DPI's metric.

/// The window DPI a winit scale factor stands for (Windows reports 96 × scale).
pub(crate) fn dpi_for_scale(scale: f64) -> u32 {
    (scale * 96.0).round().max(1.0) as u32
}

/// `(small, big)` icon edge in px at `dpi`.
pub(crate) fn icon_px(dpi: u32) -> (u32, u32) {
    #[cfg(windows)]
    if let Some(sizes) = sys::system_icon_px(dpi) {
        return sizes;
    }
    (scale_px(16, dpi), scale_px(32, dpi))
}

fn scale_px(base: u32, dpi: u32) -> u32 {
    (base * dpi + 48) / 96
}

#[cfg(windows)]
pub(crate) use sys::{OwnedIcon, fit_new_window, fit_to, system_dpi, window_icons};

#[cfg(windows)]
mod sys {
    use super::{dpi_for_scale, icon_px};
    use aterm_window::dpi::PhysicalSize;
    use aterm_window::platform::windows::{IconExtWindows as _, WindowExtWindows as _};
    use aterm_window::window::{Icon, Window};

    const ICON_RESOURCE: u16 = 1;
    const SM_CXICON: i32 = 11;
    const SM_CXSMICON: i32 = 49;
    const IMAGE_ICON: u32 = 1;
    const LR_DEFAULTCOLOR: u32 = 0;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetSystemMetricsForDpi(index: i32, dpi: u32) -> i32;
        fn GetDpiForSystem() -> u32;
        fn LoadImageW(
            instance: isize,
            name: *const u16,
            kind: u32,
            cx: i32,
            cy: i32,
            load: u32,
        ) -> isize;
        fn DestroyIcon(icon: isize) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(module_name: *const u16) -> isize;
    }

    /// The system's `(SM_CXSMICON, SM_CXICON)` at `dpi`; `None` if it answers 0.
    pub(super) fn system_icon_px(dpi: u32) -> Option<(u32, u32)> {
        // SAFETY: plain metric queries with no pointers.
        let (small, big) = unsafe {
            (
                GetSystemMetricsForDpi(SM_CXSMICON, dpi),
                GetSystemMetricsForDpi(SM_CXICON, dpi),
            )
        };
        (small > 0 && big > 0).then_some((small as u32, big as u32))
    }

    /// The system DPI.
    pub(crate) fn system_dpi() -> u32 {
        // SAFETY: no arguments; never fails.
        match unsafe { GetDpiForSystem() } {
            0 => 96,
            dpi => dpi,
        }
    }

    /// The caption (small) and taskbar (big) icons for a window at `dpi`.
    pub(crate) fn window_icons(dpi: u32) -> (Option<Icon>, Option<Icon>) {
        let (small, big) = icon_px(dpi);
        let load =
            |px: u32| Icon::from_resource(ICON_RESOURCE, Some(PhysicalSize::new(px, px))).ok();
        (load(small), load(big))
    }

    /// Sends `window` the icons for `scale`.
    pub(crate) fn fit_to(window: &Window, scale: f64) {
        let (small, big) = window_icons(dpi_for_scale(scale));
        if small.is_some() {
            window.set_window_icon(small);
        }
        if big.is_some() {
            window.set_taskbar_icon(big);
        }
    }

    /// Updates `window`'s icons when its display DPI differs from [`system_dpi`].
    pub(crate) fn fit_new_window(window: &Window) {
        let scale = window.scale_factor();
        if dpi_for_scale(scale) != system_dpi() {
            fit_to(window, scale);
        }
    }

    /// An `HICON` loaded by this process and destroyed on drop.
    pub(crate) struct OwnedIcon(isize);

    impl OwnedIcon {
        /// Loads the `px`-square `.ico` frame from this executable.
        pub(crate) fn load(px: u32) -> Option<Self> {
            let px = px as i32;
            // SAFETY: a null module name is this process's exe; the resource is
            // named by ordinal (MAKEINTRESOURCE), never dereferenced as a string.
            let handle = unsafe {
                LoadImageW(
                    GetModuleHandleW(std::ptr::null()),
                    usize::from(ICON_RESOURCE) as *const u16,
                    IMAGE_ICON,
                    px,
                    px,
                    LR_DEFAULTCOLOR,
                )
            };
            (handle != 0).then(|| Self(handle))
        }

        pub(crate) fn handle(&self) -> isize {
            self.0
        }
    }

    impl Drop for OwnedIcon {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from LoadImageW without LR_SHARED, so it is
            // this process's to destroy, exactly once.
            unsafe { DestroyIcon(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{dpi_for_scale, icon_px};

    const ICO: &[u8] = include_bytes!("../assets/aterm.ico");

    fn frames() -> Vec<(u32, Vec<u8>)> {
        let u16_at = |o: usize| u16::from_le_bytes([ICO[o], ICO[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([ICO[o], ICO[o + 1], ICO[o + 2], ICO[o + 3]]);
        assert_eq!((u16_at(0), u16_at(2)), (0, 1), "not an .ico (ICONDIR)");
        (0..usize::from(u16_at(4)))
            .map(|i| {
                let e = 6 + 16 * i;
                let edge = if ICO[e] == 0 { 256 } else { u32::from(ICO[e]) };
                assert_eq!(ICO[e], ICO[e + 1], "frame {i} is not square");
                assert_eq!(u16_at(e + 6), 32, "frame {edge} is not 32 bpp");
                let (len, off) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
                let data = &ICO[off..off + len];
                if data.starts_with(b"\x89PNG") {
                    let (rgba, w, h) =
                        aterm_png::decode_rgba8(data, 256).expect("PNG frame decodes");
                    assert_eq!(
                        (w, h),
                        (edge as usize, edge as usize),
                        "PNG frame {edge} size"
                    );
                    return (edge, rgba);
                }
                let d32 =
                    |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
                assert_eq!(
                    (d32(4), d32(8)),
                    (edge, edge * 2),
                    "DIB frame {edge} header"
                );
                let px = &data[40..40 + (edge * edge * 4) as usize];
                let rgba = px
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[2], p[1], p[0], p[3]])
                    .collect();
                (edge, rgba)
            })
            .collect()
    }

    #[test]
    fn every_icon_size_windows_asks_for_is_a_frame() {
        let sizes: Vec<u32> = frames().iter().map(|(edge, _)| *edge).collect();
        for pct in [100u32, 125, 150, 175, 200, 225, 250, 300] {
            let (small, big) = icon_px(96 * pct / 100);
            assert!(
                sizes.contains(&small),
                "no {small} px small icon for {pct} % in {sizes:?}"
            );
            assert!(
                sizes.contains(&big),
                "no {big} px big icon for {pct} % in {sizes:?}"
            );
        }
        for shell in [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256] {
            assert!(sizes.contains(&shell), "no {shell} px frame in {sizes:?}");
        }
    }

    #[test]
    fn every_icon_frame_is_anti_aliased() {
        for (edge, rgba) in frames() {
            let alpha = || rgba.as_chunks::<4>().0.iter().map(|p| p[3]);
            let partial = alpha().filter(|a| (1..255).contains(a)).count();
            let clear = alpha().filter(|&a| a == 0).count();
            assert!(
                partial > 0,
                "{edge} px frame has no partial alpha (aliased edge)"
            );
            assert!(clear > 0, "{edge} px frame has no transparent corner");
        }
    }

    #[test]
    fn icon_sizes_follow_the_window_dpi() {
        for (scale, small, big) in [
            (1.0, 16, 32),
            (1.25, 20, 40),
            (1.5, 24, 48),
            (1.75, 28, 56),
            (2.0, 32, 64),
            (2.5, 40, 80),
            (3.0, 48, 96),
        ] {
            assert_eq!(
                icon_px(dpi_for_scale(scale)),
                (small, big),
                "at scale {scale}"
            );
        }
    }
}

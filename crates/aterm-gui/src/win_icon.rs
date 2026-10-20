// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! aterm's icon on Windows, loaded at the size each place draws it.
//!
//! `assets/aterm.ico` (the exe's icon resource, ordinal 1) carries a
//! hinted, anti-aliased frame for every size the shell asks for: the small icon
//! (`SM_CXSMICON`: caption, the notification area) and the big one (`SM_CXICON`:
//! taskbar button, Alt+Tab) at every scale from 100 % to 300 %, and the shell's
//! own 30/60/64/72/80/96/128/256. So a window loads both icons at exactly its
//! DPI's metric ([`window_icons`]) and loads them again when it moves to a
//! display of another DPI ([`fit_to`], from `WindowEvent::ScaleFactorChanged`);
//! a fixed 16 px small icon was resampled by the shell at any other scale. The
//! notification area takes the same frames at the system DPI ([`OwnedIcon`]).

/// The window DPI a winit scale factor stands for (Windows reports 96 × scale).
pub(crate) fn dpi_for_scale(scale: f64) -> u32 {
    (scale * 96.0).round().max(1.0) as u32
}

/// `(small, big)` icon edge in px at `dpi`: Windows' `SM_CXSMICON` and
/// `SM_CXICON` for that DPI, or, when the system does not answer, the 96-dpi
/// 16 and 32 scaled the way Windows scales them.
pub(crate) fn icon_px(dpi: u32) -> (u32, u32) {
    #[cfg(windows)]
    if let Some(sizes) = sys::system_icon_px(dpi) {
        return sizes;
    }
    (scale_px(16, dpi), scale_px(32, dpi))
}

/// `base` px at 96 dpi scaled to `dpi`, rounded half up as `MulDiv` rounds.
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

    /// Ordinal of the `1 ICON` resource: crates/aterm/build.rs compiles it into
    /// the shipped exe and `assets/aterm.rc` into the dev `aterm-gui.exe`.
    const ICON_RESOURCE: u16 = 1;
    const SM_CXICON: i32 = 11;
    const SM_CXSMICON: i32 = 49;
    const IMAGE_ICON: u32 = 1;
    const LR_DEFAULTCOLOR: u32 = 0;

    // `GetSystemMetricsForDpi` and `GetDpiForSystem` are Windows 10 1607 APIs;
    // aterm's floor is 1809 (ConPTY), so they are imported, not looked up.
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

    /// The system DPI: what a window on the primary display at sign-in has, and
    /// the DPI the notification area draws at.
    pub(crate) fn system_dpi() -> u32 {
        // SAFETY: no arguments; never fails.
        match unsafe { GetDpiForSystem() } {
            0 => 96,
            dpi => dpi,
        }
    }

    /// The caption (small) and taskbar (big) icons for a window at `dpi`, each
    /// the `.ico` frame of exactly that size. `None` for a binary built without
    /// the resource (build.rs already said so).
    pub(crate) fn window_icons(dpi: u32) -> (Option<Icon>, Option<Icon>) {
        let (small, big) = icon_px(dpi);
        let load =
            |px: u32| Icon::from_resource(ICON_RESOURCE, Some(PhysicalSize::new(px, px))).ok();
        (load(small), load(big))
    }

    /// Send `window` the icons for `scale` (the DPI it now has).
    pub(crate) fn fit_to(window: &Window, scale: f64) {
        let (small, big) = window_icons(dpi_for_scale(scale));
        if small.is_some() {
            window.set_window_icon(small);
        }
        if big.is_some() {
            window.set_taskbar_icon(big);
        }
    }

    /// A new window carries icons for the system DPI (its attributes are built
    /// before it has a monitor); one that opened on a display of another DPI
    /// takes that display's.
    pub(crate) fn fit_new_window(window: &Window) {
        let scale = window.scale_factor();
        if dpi_for_scale(scale) != system_dpi() {
            fit_to(window, scale);
        }
    }

    /// An `HICON` this process loaded and destroys on drop, for the raw Win32
    /// users (the notification area's icon and its notification's).
    pub(crate) struct OwnedIcon(isize);

    impl OwnedIcon {
        /// The `.ico` frame `px` square from this exe; `None` without the resource.
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
            // Lazy: `then_some` would build `Self(0)` and destroy it on drop.
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

    /// The `.ico` the build embeds (ordinal 1 of the exe).
    const ICO: &[u8] = include_bytes!("../assets/aterm.ico");

    /// One frame of the `.ico`: its edge and its RGBA pixels.
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
                // BITMAPINFOHEADER, 32 bpp BI_RGB, XOR rows bottom-up as BGRA.
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

    /// Every icon a window or the notification area asks for, at every scale
    /// Windows offers from 100 % to 300 %, is a frame of its own, and so are the
    /// shell's other sizes. The audit's icon had 16/32/48/64/128/256 only, so at
    /// 125 % and 150 % (and on the 24 px Windows 11 taskbar) the shell resampled
    /// a neighbour.
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

    /// Every frame is anti-aliased: its tile's rounded edge has partly
    /// transparent pixels. The audit's frames were all hard 0/255 alpha, which
    /// is what drew jagged corners at every size.
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

    /// The small and big icons at the common scales are the sizes Windows
    /// documents (16/32 at 100 %, 20/40 at 125 %, 24/48 at 150 %, …), whether
    /// the system answered or the fallback did.
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

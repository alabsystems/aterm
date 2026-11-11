// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Added by the aterm project in 2026; see the repository NOTICE.
// Implements DPI-aware minimum window sizing and resize increments on Windows.

//! THE LEAST WINDOW SIZE ON WINDOWS, AS PIXELS (`Window::set_min_inner_size`): the client area a
//! least asks for at the window's scale ([`least_client`]), the one Windows holds a window to
//! ([`track_least`]), the outer size the Windows backend answers `WM_GETMINMAXINFO` with for it
//! ([`min_track_size`]), a physical least carried to a new scale ([`rescaled`]), the size a window
//! under a new least grows to at once ([`grown`]), and where an edge drag that keeps resize
//! increments lands ([`snapped`]).
//! Pure arithmetic, tested on every host; only the Windows backend uses it.
//!
//! Windows answers every resize from the window's `MINMAXINFO`: an edge drag's tracking rectangle
//! stops at `ptMinTrackSize`, and `DefWindowProc`'s `WM_WINDOWPOSCHANGING` holds every
//! `SetWindowPos` (a restore from maximized, a monitor's DPI change, an app's own request) to it.
//! `ptMinTrackSize` is the outer size (the frame and caption around the client area included,
//! as `AdjustWindowRectExForDpi` gives them at the window's DPI), so the least client area is
//! converted to it at the window's own scale. A resize increment lattice is based at the least,
//! as an X11 window manager bases one at the minimum size its hints give: a drag lands on
//! `least + k * increment`, never under the least.

use crate::dpi::{PhysicalSize, Size};

/// The largest side a Win32 rectangle holds.
const REACH: u32 = i32::MAX as u32;

/// The least client area `min` asks for at `scale_factor`, in whole device pixels: a logical
/// size is scaled and rounded up, so the area is never a fraction of a pixel under it; a physical
/// one stands as given. Each side is held to what a Win32 rectangle reaches.
pub(crate) fn least_client(min: Size, scale_factor: f64) -> PhysicalSize<u32> {
    let side = |logical: f64| {
        let px = (logical * scale_factor).ceil();
        // `as` saturates (and a NaN is 0), so a side is never wrapped.
        (px.max(0.0) as u32).min(REACH)
    };
    match min {
        Size::Physical(size) => PhysicalSize::new(size.width.min(REACH), size.height.min(REACH)),
        Size::Logical(size) => PhysicalSize::new(side(size.width), side(size.height)),
    }
}

/// The least client area Windows holds a window to (`WM_GETMINMAXINFO`, and the lattice an edge
/// drag lands on): `min` at `scale_factor` ([`least_client`]); none for a full screen window, which
/// covers its monitor exactly, where a least larger than the monitor would push it past the
/// monitor's edges.
pub(crate) fn track_least(
    min: Option<Size>,
    scale_factor: f64,
    fullscreen: bool,
) -> Option<PhysicalSize<u32>> {
    min.filter(|_| !fullscreen).map(|min| least_client(min, scale_factor))
}

/// The `ptMinTrackSize` for a least client area `least` in a window whose frame adds `frame`
/// (the width and height `AdjustWindowRectExForDpi` grows an empty client rectangle by), where
/// Windows proposed `proposed` (its own least, which stands where it is larger): `(x, y)`, each
/// at most `i32::MAX`, never wrapped negative.
pub(crate) fn min_track_size(
    least: PhysicalSize<u32>,
    frame: PhysicalSize<u32>,
    proposed: (i32, i32),
) -> (i32, i32) {
    let outer = |client: u32, frame: u32, proposed: i32| {
        let side = i32::try_from(client.saturating_add(frame).min(REACH)).unwrap_or(i32::MAX);
        side.max(proposed)
    };
    (outer(least.width, frame.width, proposed.0), outer(least.height, frame.height, proposed.1))
}

/// The least `min` once the window's scale moves from `old` to `new`: a physical least was given
/// in the old scale's device pixels, so it is scaled with the window (rounded up) until its caller
/// gives one for the new scale; a logical least already follows the scale. A DPI change's new
/// rectangle is held to the least (`DefWindowProc`'s `WM_WINDOWPOSCHANGING`), so a window at its
/// least moved from a 200% monitor to a 100% one would otherwise stay twice its least.
pub(crate) fn rescaled(min: Size, old: f64, new: f64) -> Size {
    match min {
        Size::Physical(size) if old > 0.0 && new > 0.0 => {
            let side = |px: u32| ((f64::from(px) * new / old).ceil().max(0.0) as u32).min(REACH);
            Size::Physical(PhysicalSize::new(side(size.width), side(size.height)))
        },
        _ => min,
    }
}

/// How a window stands when it is given a new least, which says whether it grows to it at once
/// ([`grown`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Standing {
    /// In its normal state, its client area laid out at its scale: it grows to the least now.
    Normal,
    /// Minimized, maximized or full screen: `DefWindowProc`'s `WM_WINDOWPOSCHANGING` holds it to
    /// the least when it is restored.
    Placed,
    /// Taking a new scale (`WM_DPICHANGED`, whose `ScaleFactorChanged` an app answers with a least
    /// for the new scale): its client area is still the old scale's size on the monitor it is
    /// leaving, and the scale change's own new rectangle is held to the least, so growing it now
    /// would only resize it on the old monitor first.
    Rescaling,
}

/// The client area a window of client area `now`, standing as `standing`, grows to at once under
/// the least `least`, each side at least the least's; `None` when it already holds it, or when it
/// is held to the least later ([`Standing`]).
pub(crate) fn grown(
    now: PhysicalSize<u32>,
    least: PhysicalSize<u32>,
    standing: Standing,
) -> Option<PhysicalSize<u32>> {
    if standing != Standing::Normal {
        return None;
    }
    let grown = PhysicalSize::new(now.width.max(least.width), now.height.max(least.height));
    (grown != now).then_some(grown)
}

/// Where an edge drag that keeps the resize increment `increment` lands a client side of `size`
/// pixels over the least side `least` (0 for none): the nearest `least + k * increment`, `k` at
/// least 0 (half way, the larger), so never under the least. With no least, the nearest whole
/// increments, as the drag snapped before a window took a least.
pub(crate) fn snapped(size: i32, increment: i32, least: i32) -> i32 {
    if increment <= 0 {
        return size;
    }
    let (size, increment, least) = (i64::from(size), i64::from(increment), i64::from(least));
    let over = (size - least).max(0);
    let steps = (over + increment / 2) / increment;
    i32::try_from(least + steps * increment).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[path = "min_track_tests.rs"]
mod tests;

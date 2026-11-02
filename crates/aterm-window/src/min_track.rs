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
mod tests {
    use super::{grown, least_client, min_track_size, rescaled, snapped, track_least, Standing};
    use crate::dpi::{LogicalSize, PhysicalSize, Size};

    /// A LEAST IS CONVERTED AT THE WINDOW'S OWN SCALE: a physical least stands as given at any
    /// scale; a logical one is scaled and rounded up, never down (125.25 device pixels needs 126).
    #[test]
    fn a_least_is_whole_device_pixels_at_the_windows_scale() {
        let physical = Size::Physical(PhysicalSize::new(328, 292));
        assert_eq!(least_client(physical, 1.0), PhysicalSize::new(328, 292));
        assert_eq!(least_client(physical, 2.0), PhysicalSize::new(328, 292));
        let logical = Size::Logical(LogicalSize::new(147.0, 98.0));
        assert_eq!(least_client(logical, 1.0), PhysicalSize::new(147, 98));
        assert_eq!(least_client(logical, 1.5), PhysicalSize::new(221, 147));
        assert_eq!(least_client(logical, 2.0), PhysicalSize::new(294, 196));
        // 125.25 device pixels needs 126, where rounding to the nearest gave 125.
        let fraction = Size::Logical(LogicalSize::new(100.2, 98.0));
        assert_eq!(least_client(fraction, 1.25), PhysicalSize::new(126, 123));
    }

    /// THE TRACK SIZE IS THE OUTER SIZE: the least client area with the frame around it; where
    /// Windows' own least is larger, it stands.
    #[test]
    fn the_track_size_is_the_least_client_area_with_its_frame() {
        let least = PhysicalSize::new(328, 292);
        let frame = PhysicalSize::new(16, 39);
        assert_eq!(min_track_size(least, frame, (136, 39)), (344, 331));
        // A least under Windows' own (the caption's buttons) keeps Windows'.
        assert_eq!(min_track_size(PhysicalSize::new(20, 0), frame, (136, 39)), (136, 39));
        // No frame (a borderless window): the client area alone.
        assert_eq!(min_track_size(least, PhysicalSize::new(0, 0), (0, 0)), (328, 292));
    }

    /// A LEAST PAST A WIN32 RECTANGLE'S REACH IS HELD TO IT, NEVER WRAPPED NEGATIVE: `u32::MAX`
    /// (what a caller's overflowed size saturates to) cast to `i32` would read -1, a track size
    /// under every window.
    #[test]
    fn a_least_past_the_reach_of_a_rectangle_never_wraps_negative() {
        let huge = Size::Physical(PhysicalSize::new(u32::MAX, u32::MAX));
        let least = least_client(huge, 1.0);
        assert_eq!(least, PhysicalSize::new(i32::MAX as u32, i32::MAX as u32));
        let track = min_track_size(least, PhysicalSize::new(16, 39), (136, 39));
        assert_eq!(track, (i32::MAX, i32::MAX));
        let logical = Size::Logical(LogicalSize::new(f64::MAX, f64::NAN));
        assert_eq!(least_client(logical, 2.0), PhysicalSize::new(i32::MAX as u32, 0));
    }

    /// A PHYSICAL LEAST FOLLOWS THE WINDOW TO A NEW SCALE: given at 200%, it is half as many device
    /// pixels at 100% (rounded up) and the same again back at 200%; a logical least is left as it
    /// is, since it is converted at the window's scale wherever it is read.
    #[test]
    fn a_physical_least_follows_the_window_to_a_new_scale() {
        let at_two = Size::Physical(PhysicalSize::new(340, 201));
        let at_one = rescaled(at_two, 2.0, 1.0);
        assert_eq!(at_one, Size::Physical(PhysicalSize::new(170, 101)));
        assert_eq!(rescaled(at_one, 1.0, 2.0), Size::Physical(PhysicalSize::new(340, 202)));
        assert_eq!(rescaled(at_two, 2.0, 2.0), at_two);
        let logical = Size::Logical(LogicalSize::new(164.0, 98.0));
        assert_eq!(rescaled(logical, 2.0, 1.0), logical);
    }

    /// A WINDOW UNDER A NEW LEAST GROWS TO IT, EACH SIDE ON ITS OWN; ONE THAT HOLDS IT STAYS.
    #[test]
    fn a_window_under_its_least_grows_to_it_side_by_side() {
        let least = PhysicalSize::new(328, 292);
        let normal = Standing::Normal;
        assert_eq!(grown(PhysicalSize::new(164, 98), least, normal), Some(least));
        let wide = PhysicalSize::new(800, 98);
        assert_eq!(grown(wide, least, normal), Some(PhysicalSize::new(800, 292)));
        assert_eq!(grown(PhysicalSize::new(800, 600), least, normal), None);
    }

    /// ONLY A WINDOW IN ITS NORMAL STATE GROWS AT ONCE: a minimized one (its client area empty),
    /// a maximized or full screen one is held to the least when it is restored, and one taking a
    /// new scale is held to it by the scale change's own new rectangle. A window at its least of
    /// 180 by 120 at 100% moved to a 200% monitor is told a least of 360 by 240 while its client
    /// area is still 180 by 120: growing it then would double it on the monitor it is leaving.
    #[test]
    fn only_a_window_in_its_normal_state_grows_at_once() {
        let least = PhysicalSize::new(360, 240);
        let old_scale = PhysicalSize::new(180, 120);
        assert_eq!(grown(old_scale, least, Standing::Normal), Some(least));
        assert_eq!(grown(old_scale, least, Standing::Rescaling), None);
        assert_eq!(grown(PhysicalSize::new(0, 0), least, Standing::Placed), None);
    }

    /// A FULL SCREEN WINDOW IS HELD TO NO LEAST: it covers its monitor exactly, where a least of
    /// 2000 by 1200 pixels would push it past a 1920 by 1080 monitor's edges; out of full screen
    /// the least is converted at the window's scale.
    #[test]
    fn a_full_screen_window_is_held_to_no_least() {
        let min = Some(Size::Physical(PhysicalSize::new(2000, 1200)));
        assert_eq!(track_least(min, 1.0, true), None);
        assert_eq!(track_least(min, 1.0, false), Some(PhysicalSize::new(2000, 1200)));
        let logical = Some(Size::Logical(LogicalSize::new(164.0, 98.0)));
        assert_eq!(track_least(logical, 1.5, false), Some(PhysicalSize::new(246, 147)));
        assert_eq!(track_least(None, 1.0, false), None);
    }

    /// AN EDGE DRAG LANDS ON THE LATTICE BASED AT THE LEAST, NEVER UNDER IT: with a least of 226
    /// pixels and an increment of 20, a drag to 240 lands on 246 (226 + 20) and one to 230 on 226;
    /// whole increments from 0 (240) would leave the frame off the lattice the least sits on, and
    /// one to 218 would take the window under its least (220). With no least, the drag snaps to
    /// whole increments as before.
    #[test]
    fn an_edge_drag_lands_on_the_lattice_based_at_the_least() {
        assert_eq!(snapped(240, 20, 226), 246);
        assert_eq!(snapped(236, 20, 226), 246);
        assert_eq!(snapped(235, 20, 226), 226);
        assert_eq!(snapped(230, 20, 226), 226);
        assert_eq!(snapped(218, 20, 226), 226);
        for size in 0..2_000 {
            let landed = snapped(size, 17, 226);
            assert!(landed >= 226 && (landed - 226) % 17 == 0, "{size} landed on {landed}");
            assert!((landed - size.max(226)).abs() <= 17 / 2 + 1, "{size} landed on {landed}");
        }
        // No least: the nearest whole increment, half way the larger.
        assert_eq!(snapped(229, 20, 0), 220);
        assert_eq!(snapped(230, 20, 0), 240);
        assert_eq!(snapped(241, 21, 0), 231);
        assert_eq!(snapped(242, 21, 0), 252);
        // No increment: the size as dragged.
        assert_eq!(snapped(237, 0, 226), 237);
    }
}

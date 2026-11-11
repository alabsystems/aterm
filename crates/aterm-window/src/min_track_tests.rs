// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Added by the aterm project in 2026; see the repository NOTICE.
// Implements DPI-aware minimum window sizing and resize increments on Windows.

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

// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/window/state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

#[test]
fn a_configure_s_states_and_capabilities_are_read_and_unknown_values_left_out() {
    use xdg_toplevel::{state, wm_capabilities as cap};
    let states = XdgWindowState::from_wire(&[
        state::MAXIMIZED,
        state::ACTIVATED,
        state::TILED_TOP,
        state::CONSTRAINED_LEFT,
        0xffff,
    ]);
    assert_eq!(
        states,
        XdgWindowState::MAXIMIZED | XdgWindowState::ACTIVATED | XdgWindowState::TILED_TOP
    );
    let configure = WindowConfigure { state: states, ..WindowConfigure::default() };
    assert!(configure.is_maximized() && configure.is_tiled() && !configure.is_fullscreen());
    assert_eq!(
        WindowManagerCapabilities::from_wire(&[cap::MINIMIZE, cap::WINDOW_MENU, 9]),
        WindowManagerCapabilities::MINIMIZE | WindowManagerCapabilities::WINDOW_MENU
    );
    assert!(WindowConfigure::default().capabilities.is_all());
}

#[test]
fn every_resize_direction_names_its_edge() {
    use xdg_toplevel::resize_edge as edge;
    let all = [
        (ResizeDirection::North, edge::TOP),
        (ResizeDirection::South, edge::BOTTOM),
        (ResizeDirection::West, edge::LEFT),
        (ResizeDirection::East, edge::RIGHT),
        (ResizeDirection::NorthWest, edge::TOP_LEFT),
        (ResizeDirection::NorthEast, edge::TOP_RIGHT),
        (ResizeDirection::SouthWest, edge::BOTTOM_LEFT),
        (ResizeDirection::SouthEast, edge::BOTTOM_RIGHT),
    ];
    for (direction, want) in all {
        assert_eq!(resize_edge(direction), want, "{direction:?}");
    }
}

#[test]
fn a_protocol_string_stops_at_a_nul() {
    assert_eq!(protocol_string("aterm").as_bytes(), b"aterm");
    assert_eq!(protocol_string("a\0b").as_bytes(), b"a");
}

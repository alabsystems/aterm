// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/event_loop.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::cell::Cell;

use super::SIZE_MINIMIZED;
use windows_sys::Win32::UI::WindowsAndMessaging::SIZE_MAXIMIZED;

const SIZE_RESTORED: u32 = 0;

#[test]
fn only_the_edges_of_a_minimize_are_occlusion() {
    let was = Cell::new(false);
    let edge = |wparam: u32| super::minimize_edge(&was, wparam as usize);
    assert_eq!(edge(SIZE_RESTORED), None, "a drag step");
    assert_eq!(edge(SIZE_MAXIMIZED), None, "a maximize of a shown window");
    assert_eq!(edge(SIZE_MINIMIZED), Some(true));
    assert_eq!(edge(SIZE_MINIMIZED), None, "already minimized");
    assert_eq!(edge(SIZE_MAXIMIZED), Some(false), "back, maximized");
    assert_eq!(edge(SIZE_MINIMIZED), Some(true));
    assert_eq!(edge(SIZE_RESTORED), Some(false), "back, restored");
    assert_eq!(edge(SIZE_RESTORED), None);
}

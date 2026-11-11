// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/types/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

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

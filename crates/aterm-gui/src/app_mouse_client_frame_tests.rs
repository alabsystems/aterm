// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's own frame on the pointer path (`crate::client_frame`).

use aterm_window::dpi::PhysicalSize;
use aterm_window::event::{ElementState, MouseButton};

use crate::client_frame::{ClientFrame, FrameEdge};
#[cfg(any(windows, target_os = "linux"))]
use crate::tab_bar::{CaptionButton, TabHit};
use crate::{App, WindowId};

const FRAME: ClientFrame = ClientFrame {
    minimize: true,
    maximize: true,
    window_menu: true,
    maximized: false,
    edges: true,
    header: true,
    layout: crate::client_frame::ButtonLayout::FALLBACK,
};

/// A headless window at scale 1 that draws its own frame (or, with `None`, one the
/// window system decorates), with the default padding (12 px, 2 on top), sized
/// exactly around its grid, that padding and, as [`App::strip_rows_for`] gives a
/// framed window, its header row: the same geometry on every host (Linux pins the
/// frame to the window's top, macOS centres it).
fn framed(frame: Option<ClientFrame>) -> (App, WindowId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = u16::from(frame.is_some_and(ClientFrame::has_header));
    let ws = app.windows.get_mut(&wid).expect("the headless window");
    ws.client_frame = frame;
    ws.scale = 1.0;
    ws.metrics.pad = crate::logical_to_device_px(crate::PAD_LOGICAL_PX, 1.0);
    ws.metrics.pad_top = crate::logical_to_device_px(crate::PAD_TOP_LOGICAL_PX, 1.0);
    let (cw, ch) = app.win_cell_size(wid);
    let (pad, pad_top, head) = (app.win_pad(wid), app.win_pad_top(wid), app.win_head(wid));
    let (rows, cols) = (
        usize::from(app.windows[&wid].rows),
        usize::from(app.windows[&wid].cols),
    );
    let chrome = usize::from(app.chrome_rows(wid));
    let w = cols * cw + 2 * pad;
    let h = head + pad_top + (chrome + rows) * ch + pad;
    app.windows
        .get_mut(&wid)
        .expect("the headless window")
        .win_px = Some(PhysicalSize::new(
        u32::try_from(w).expect("width"),
        u32::try_from(h).expect("height"),
    ));
    (app, wid)
}

/// The window's size in px, as [`framed`] made it.
fn size(app: &App, wid: WindowId) -> (f64, f64) {
    let px = app.windows[&wid].win_px.expect("a sized window");
    (f64::from(px.width), f64::from(px.height))
}

#[test]
fn an_edge_takes_the_pointer_and_hands_it_back() {
    let (mut app, wid) = framed(Some(FRAME));
    let (w, h) = size(&app, wid);
    app.on_cursor_moved(wid, 1.0, h * 0.5);
    assert_eq!(app.windows[&wid].frame_edge, Some(FrameEdge::West));
    app.on_cursor_moved(wid, w - 2.0, h - 2.0);
    assert_eq!(app.windows[&wid].frame_edge, Some(FrameEdge::SouthEast));
    app.on_cursor_moved(wid, w * 0.5, h * 0.5);
    assert_eq!(
        app.windows[&wid].frame_edge, None,
        "the interior gives the pointer back"
    );
    let ws = &app.windows[&wid];
    assert!(
        !(ws.hover_pointer && ws.native_text_cursor),
        "the grid put its own cursor back over the handle's (the re-issue encoding is gone)"
    );
    app.on_cursor_moved(wid, w * 0.5, 1.0);
    assert_eq!(app.windows[&wid].frame_edge, Some(FrameEdge::North));
    app.on_cursor_left(wid);
    assert_eq!(app.windows[&wid].frame_edge, None);
}

#[test]
fn no_frame_or_a_maximized_one_has_no_handles() {
    let (mut app, wid) = framed(None);
    let mid = size(&app, wid).1 * 0.5;
    app.on_cursor_moved(wid, 1.0, mid);
    assert_eq!(app.windows[&wid].frame_edge, None);
    let (mut app, wid) = framed(Some(ClientFrame {
        maximized: true,
        edges: false,
        ..FRAME
    }));
    app.on_cursor_moved(wid, 1.0, mid);
    assert_eq!(app.windows[&wid].frame_edge, None);
}

#[test]
fn a_gesture_in_flight_keeps_the_pointer_off_the_handles() {
    let (mut app, wid) = framed(Some(FRAME));
    app.windows.get_mut(&wid).expect("window").selecting = true;
    app.on_cursor_moved(wid, 1.0, size(&app, wid).1 * 0.5);
    assert_eq!(
        app.windows[&wid].frame_edge, None,
        "a selection dragged to the edge stays a selection"
    );
}

#[test]
fn a_press_on_a_handle_goes_no_further() {
    let (mut app, wid) = framed(Some(FRAME));
    let (w, h) = size(&app, wid);
    app.on_cursor_moved(wid, w * 0.5, h - 2.0);
    assert_eq!(app.windows[&wid].frame_edge, Some(FrameEdge::South));
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    #[cfg(target_os = "linux")]
    assert!(
        !app.windows[&wid].selecting,
        "the handle's press is the resize's, not a selection's"
    );
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
}

/// `window_padding = 0`: the grid fills the window's sides and bottom exactly, so
/// its first and last columns and its last row reach the window's edges. They stay
/// the grid's (a selection, a program's mouse report), never a resize handle's.
#[test]
fn at_no_padding_the_edge_cells_stay_the_grids() {
    let (mut app, wid) = framed(Some(FRAME));
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.metrics.pad = 0;
        ws.metrics.pad_top = 0;
    }
    let (cw, ch) = app.win_cell_size(wid);
    let (rows, cols) = (
        usize::from(app.windows[&wid].rows),
        usize::from(app.windows[&wid].cols),
    );
    let above = app.win_head(wid) + usize::from(app.chrome_rows(wid)) * ch;
    let (w, h) = (cols * cw, above + rows * ch);
    app.windows.get_mut(&wid).expect("window").win_px = Some(PhysicalSize::new(
        u32::try_from(w).expect("width"),
        u32::try_from(h).expect("height"),
    ));
    let (w, h) = (w as f64, h as f64);
    let mid_y = above as f64 + (rows * ch) as f64 * 0.5;
    for (x, y, cell) in [
        (0.0, mid_y, "the first column"),
        (w - 0.5, mid_y, "the last column"),
        (w * 0.5, h - 0.5, "the last row"),
        (0.0, h - 0.5, "the bottom-left cell"),
    ] {
        app.on_cursor_moved(wid, x, y);
        assert_eq!(app.windows[&wid].frame_edge, None, "{cell} is the grid's");
    }
    app.on_cursor_moved(wid, 0.0, mid_y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert!(
        app.windows[&wid].selecting,
        "a press on the first column starts a selection"
    );
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.metrics.pad = 12;
        ws.win_px = Some(PhysicalSize::new(
            u32::try_from(cols * cw + 24).expect("width"),
            u32::try_from(above + rows * ch + 12).expect("height"),
        ));
    }
    app.on_cursor_moved(wid, 1.0, mid_y);
    assert_eq!(app.windows[&wid].frame_edge, Some(FrameEdge::West));
}

/// A window that draws its own frame needs its header, so the strip keeps a row at
/// `tab_strip_rows = 0` while one does, and gives it back with the frame.
#[test]
fn a_framed_window_keeps_its_header_row_at_no_strip() {
    let (mut app, wid) = framed(Some(FRAME));
    app.config.tab_strip_rows = Some(0);
    app.tab_strip_rows = 0;
    assert_eq!(app.strip_rows_for(0), 1);
    let _ = crate::app_presence::take_chrome_regrids_applied();
    assert!(app.sync_strip_header_row(wid), "the header's row rises");
    assert_eq!(app.tab_strip_rows, 1);
    assert!(
        crate::app_presence::take_chrome_regrids_applied().is_empty(),
        "the window whose frame changed is fitted by its caller (the settle first), not here"
    );
    #[cfg(any(windows, target_os = "linux"))]
    {
        app.splice_tab_strip_with(wid, 1);
        assert!(
            app.windows[&wid]
                .tab_segments
                .iter()
                .any(|s| matches!(s.kind, TabHit::Caption(CaptionButton::Close))),
            "the row is the header: it carries the window's buttons"
        );
    }
    assert!(!app.sync_strip_header_row(wid), "settled: nothing moves");
    app.windows.get_mut(&wid).expect("window").client_frame = Some(ClientFrame {
        edges: false,
        header: false,
        ..FRAME
    });
    assert!(app.sync_strip_header_row(wid));
    assert_eq!(app.tab_strip_rows, 0);
    app.config.tab_strip_rows = Some(2);
    app.windows.get_mut(&wid).expect("window").client_frame = Some(FRAME);
    assert!(app.sync_strip_header_row(wid));
    assert_eq!(app.tab_strip_rows, 2);
}

#[test]
fn a_window_the_system_decorates_keeps_no_strip() {
    let (mut app, wid) = framed(None);
    app.config.tab_strip_rows = Some(0);
    app.tab_strip_rows = 0;
    assert_eq!(app.strip_rows_for(0), 0);
    assert!(!app.sync_strip_header_row(wid));
    assert_eq!(app.tab_strip_rows, 0);
    app.splice_tab_strip_with(wid, 0);
    assert!(app.windows[&wid].tab_segments.is_empty());
}

/// The header strip of a framed window, laid out by the splice.
#[cfg(any(windows, target_os = "linux"))]
fn header(app: &mut App, wid: WindowId) -> Vec<crate::tab_bar::TabSegment> {
    app.tab_strip_rows = 1;
    app.splice_tab_strip_with(wid, 1);
    app.windows[&wid].tab_segments.clone()
}

/// The window pixel at the middle of strip column `col`.
#[cfg(any(windows, target_os = "linux"))]
fn strip_point(app: &App, wid: WindowId, col: u16) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    let pad = app.win_pad(wid) as f64;
    let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
    (
        pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
        top + ch as f64 * 0.5,
    )
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn the_header_lays_out_lights_and_presses_its_window_buttons() {
    let (mut app, wid) = framed(Some(FRAME));
    let segs = header(&mut app, wid);
    let kinds: Vec<_> = segs
        .iter()
        .filter_map(|s| match s.kind {
            TabHit::Caption(button) => Some(button),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [
            CaptionButton::Minimize,
            CaptionButton::Maximize,
            CaptionButton::Close
        ]
    );
    let close = segs.last().copied().expect("the close button");
    let (x, y) = strip_point(&app, wid, close.start_col + 1);
    app.on_cursor_moved(wid, x, y);
    assert_eq!(
        app.windows[&wid].strip_hover_caption,
        Some(CaptionButton::Close)
    );
    let resting_key = app.windows[&wid].last_strip_fp.clone();
    app.splice_tab_strip_with(wid, 1);
    assert_ne!(
        app.windows[&wid].last_strip_fp, resting_key,
        "the hovered button is a strip cache-key term"
    );
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert!(
        app.windows[&wid].caption_close,
        "close asks the event loop to close the window as CloseRequested does"
    );
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    app.on_cursor_left(wid);
    assert_eq!(app.windows[&wid].strip_hover_caption, None);
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn a_window_the_system_decorates_has_no_buttons() {
    let (mut app, wid) = framed(None);
    let segs = header(&mut app, wid);
    assert!(
        !segs.iter().any(|s| matches!(s.kind, TabHit::Caption(_))),
        "{segs:?}"
    );
}

/// The bare band of a framed window is caption: a press arms the move (which the
/// press's first real motion starts), its release disarms it, and a second press
/// within the double-click window toggles maximize instead of arming.
#[cfg(target_os = "linux")]
#[test]
fn the_bare_band_arms_a_move_and_a_double_click_does_not() {
    let (mut app, wid) = framed(Some(FRAME));
    let segs = header(&mut app, wid);
    let bare = (0..segs.last().expect("segments").start_col)
        .rev()
        .find(|&col| crate::tab_bar::hit_test(&segs, col).is_none())
        .expect("bare band between the tabs and the buttons");
    let (x, y) = strip_point(&app, wid, bare);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert_eq!(app.windows[&wid].caption_move, Some((x, y)));
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_eq!(app.windows[&wid].caption_move, None, "the release disarms");
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert_eq!(
        app.windows[&wid].caption_move, None,
        "the double-click's second press maximizes; it never moves"
    );
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{App, WindowId};
use aterm_window::dpi::PhysicalSize;

const WID: WindowId = WindowId(0);

fn fixture() -> App {
    let mut app = App::headless_for_test();
    let ws = &app.windows[&WID];
    let frame = app.window_frame_px_for(WID, ws.rows, ws.cols);
    let (cw, _) = app.win_cell_size(WID);
    app.set_side_inset_for_test(WID, 4);
    app.windows.get_mut(&WID).unwrap().win_px = Some(PhysicalSize::new(
        frame.width + (4 * cw) as u32,
        frame.height,
    ));
    app
}

fn inset_point(app: &App) -> (f64, f64) {
    let (_, y) = app.cell_centre_px(WID, 0, 0).unwrap();
    (app.win_side_inset_px(WID) as f64 / 2.0, y)
}

#[test]
fn side_inset_modifiers_cannot_resurrect_the_previous_cells_link() {
    let mut app = fixture();
    crate::term_lock(&app.front_terminal(WID).unwrap().term).process(b"https://example.com");
    let at = app.cell_centre_px(WID, 0, 7).unwrap();
    app.on_modifiers_changed(
        WID,
        aterm_window::keyboard::ModifiersState::SUPER
            | aterm_window::keyboard::ModifiersState::CONTROL,
    );
    app.on_cursor_moved(WID, at.0, at.1);
    assert!(
        app.windows[&WID].hover_pointer,
        "positive control: an offered URL"
    );
    let at = inset_point(&app);
    app.on_cursor_moved(WID, at.0, at.1);
    assert!(app.windows[&WID].link_hover.is_none());
    app.on_modifiers_changed(WID, aterm_window::keyboard::ModifiersState::empty());
    app.on_modifiers_changed(
        WID,
        aterm_window::keyboard::ModifiersState::SUPER
            | aterm_window::keyboard::ModifiersState::CONTROL,
    );
    let ws = &app.windows[&WID];
    assert!(ws.link_hover.is_none() && !ws.hover_pointer && !ws.native_text_cursor);
    app.windows.get_mut(&WID).unwrap().native_text_cursor = true;
    app.update_hover_cursor(WID);
    assert!(
        !app.windows[&WID].native_text_cursor,
        "a later resolver still owns the inset cursor"
    );
}

#[test]
fn side_inset_native_hover_requests_the_default_cursor() {
    let mut app = fixture();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::KeyboardInput));
    app.windows.get_mut(&WID).unwrap().native_text_cursor = true;
    CURSOR_REQUESTS_FOR_TEST.with_borrow_mut(Vec::clear);
    let at = inset_point(&app);
    app.on_cursor_moved(WID, at.0, at.1);
    CURSOR_REQUESTS_FOR_TEST
        .with_borrow(|requests| assert_eq!(requests.last(), Some(&(WID, CursorIcon::Default))));
}

#[test]
fn side_inset_outside_presses_reach_both_menu_dismissals() {
    let mut app = fixture();
    app.tab_strip_rows = 1;
    app.config.tab_strip_rows = Some(1);
    assert!(app.open_tab_context_menu(WID, 0, 3, false));
    let at = inset_point(&app);
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_input(WID, ElementState::Pressed, WinitMouseButton::Left);
    assert!(app.windows[&WID].tab_menu.is_none());
    app.on_mouse_input(WID, ElementState::Released, WinitMouseButton::Left);
    app.open_fact_menu(WID, 0, crate::claude_lights::Fact::Model, (0, 3), false);
    assert!(app.autopilot_menu.is_some());
    app.on_mouse_input(WID, ElementState::Pressed, WinitMouseButton::Left);
    assert!(app.autopilot_menu.is_none());
}

#[test]
fn side_inset_palette_keeps_its_window_wheel() {
    let mut app = fixture();
    app.palette_enter();
    let before = app.windows[&WID].palette().unwrap().scroll_extent();
    assert!(before.1 > before.2, "a scrollable palette");
    let at = inset_point(&app);
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_wheel(WID, MouseScrollDelta::LineDelta(0.0, -3.0));
    assert!(app.windows[&WID].palette().unwrap().scroll_extent().0 > before.0);
}

#[test]
fn side_inset_captured_gestures_keep_chorded_presses_and_wheels() {
    let mut app = fixture();
    crate::term_lock(&app.front_terminal(WID).unwrap().term).process(b"\x1b[?1000h\x1b[?1006h");
    let at = app.cell_centre_px(WID, 0, 8).unwrap();
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_input(WID, ElementState::Pressed, WinitMouseButton::Left);
    assert_ne!(app.windows[&WID].reported_buttons, 0);
    let at = inset_point(&app);
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_input(WID, ElementState::Pressed, WinitMouseButton::Right);
    assert_eq!(app.windows[&WID].reported_buttons.count_ones(), 2);
    app.on_mouse_wheel(WID, MouseScrollDelta::LineDelta(0.0, 0.25));
    assert_ne!(app.windows[&WID].scroll_residual, 0.0);
    app.on_mouse_input(WID, ElementState::Released, WinitMouseButton::Right);
    app.on_mouse_input(WID, ElementState::Released, WinitMouseButton::Left);
    assert_eq!(app.windows[&WID].reported_buttons, 0);
}

#[test]
fn side_inset_motion_does_not_postpone_a_lens_leave() {
    let mut app = fixture();
    app.windows.get_mut(&WID).unwrap().stats_lens.open = Some(crate::stats_lens::Lens {
        focus: crate::stats_paint::Instrument::Cpu,
        bar: crate::stats_paint::Bar::Global,
        how: crate::stats_lens::Opened::Hover,
    });
    let pane = app.cell_centre_px(WID, 0, 8).unwrap();
    app.on_cursor_moved(WID, pane.0, pane.1);
    let at = inset_point(&app);
    app.on_cursor_moved(WID, at.0, at.1);
    let deadline = app.windows[&WID].stats_lens.leave_due.expect("leave armed");
    app.on_cursor_moved(WID, at.0 + 1.0, at.1);
    assert_eq!(app.windows[&WID].stats_lens.leave_due, Some(deadline));
}

#[test]
fn side_inset_strip_drag_clamps_like_a_remainder_band() {
    let mut app = fixture();
    app.tab_strip_rows = 1;
    app.config.tab_strip_rows = Some(1);
    app.splice_tab_strip_with(WID, 1);
    let tab = app.windows[&WID].tab_set.active_id().unwrap();
    app.windows.get_mut(&WID).unwrap().strip_drag = Some(StripDrag { tab, origin_col: 3 });
    let geom = app.pointer_geometry(WID);
    let (ox, oy) = app.frame_origin(WID);
    let y = oy as f64 + (geom.head + geom.pad_top + geom.cell.1 / 2) as f64;
    assert_eq!(
        app.strip_col_at_with(
            WID,
            geom,
            ox as f64 + (geom.pad + geom.cell.0 / 2) as f64,
            y
        ),
        Some(0),
        "positive control: the strip's first cell"
    );
    assert_eq!(app.strip_col_at_with(WID, geom, 1.0, y), Some(0));
}

/// The product setter, [`App::set_dock_at`] (Show Navigator, `open app navigator dock` and the
/// waiting Hide all enter it), remaps a pointer that did not move: a press right after Show or
/// Hide reads the cell now under the pointer, not the one the frame held there before.
#[test]
fn side_inset_setter_remaps_a_still_pointer_before_a_press() {
    let mut app = App::headless_for_test();
    let size = app.window_frame_px_for(WID, 24, 140);
    app.windows.get_mut(&WID).unwrap().win_px = Some(size);
    app.on_resize(WID, size);
    let at = app.cell_centre_px(WID, 0, 60).unwrap();
    app.on_cursor_moved(WID, at.0, at.1);
    let before = app.windows[&WID].last_mouse_cell;
    let now = std::time::Instant::now();
    app.set_dock_at(WID, 30, now)
        .expect("the window has room for the dock");
    let wanted = app.pixel_to_cell(WID, at.0, at.1);
    assert_ne!(
        wanted, before,
        "positive control: Show moved the frame under the pointer"
    );
    assert_eq!(app.windows[&WID].last_mouse_cell, wanted);
    app.on_mouse_input(WID, ElementState::Pressed, WinitMouseButton::Left);
    assert_eq!(app.windows[&WID].last_mouse_cell, wanted);
    app.on_mouse_input(WID, ElementState::Released, WinitMouseButton::Left);
    app.set_dock_at(WID, 0, now + std::time::Duration::from_secs(1))
        .expect("the hide commits after the regrow floor");
    assert_eq!(app.windows[&WID].dock.cells, 0);
    let back = app.pixel_to_cell(WID, at.0, at.1);
    assert_eq!(back, before, "Hide gives the frame its place back");
    assert_eq!(app.windows[&WID].last_mouse_cell, back);
}

#[test]
fn side_inset_overlap_keeps_visible_frame_pixels_interactive() {
    let mut app = fixture();
    app.set_side_inset_for_test(WID, 10);
    let ws = &app.windows[&WID];
    let frame = app.frame_px(ws.rows + app.chrome_rows(WID), ws.cols);
    app.windows.get_mut(&WID).unwrap().win_px = Some(PhysicalSize::new(frame.width, frame.height));
    let (ox, oy) = app.frame_origin(WID);
    let cw = app.win_cell_size(WID).0;
    let x = ox as f64 + (app.win_pad(WID) + cw / 2) as f64;
    assert!(
        x < app.win_side_inset_px(WID) as f64,
        "visible frame overlaps the nominal inset"
    );
    assert!(!app.in_side_inset(WID, x));
    let y = oy as f64
        + (app.win_pad_top(WID) + app.win_head(WID) + app.win_cell_size(WID).1 / 2) as f64;
    app.on_cursor_moved(WID, x, y);
    app.on_mouse_input(WID, ElementState::Pressed, WinitMouseButton::Left);
    assert!(app.windows[&WID].selecting);
}

#[test]
fn side_inset_pending_backend_geometry_uses_the_seed_cell() {
    let mut app = App::headless_for_test();
    let expected = 3 * crate::seed_cell_px(app.windows[&WID].metrics.font_px).0;
    app.windows.get_mut(&WID).unwrap().dock.cells = 3;
    let ready = std::mem::replace(&mut app.backend, crate::BackendSlot::Pending(None));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(app.win_side_inset_px(WID), expected);
        assert_eq!(app.frame_origin(WID), (expected as i64, 0));
        let _ = app.dims_snapshot(0, 24, 80);
    }));
    app.backend = ready;
    assert!(
        result.is_ok(),
        "a held inset cannot read pending renderer metrics"
    );
}

#[test]
fn side_inset_local_selection_keeps_its_capture_until_release() {
    let mut app = fixture();
    let at = app.cell_centre_px(WID, 0, 8).unwrap();
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_input(WID, ElementState::Pressed, WinitMouseButton::Left);
    assert!(app.windows[&WID].selecting);
    let at = inset_point(&app);
    app.on_cursor_moved(WID, at.0, at.1);
    assert!(app.windows[&WID].selecting);
    assert_eq!(app.windows[&WID].last_mouse_cell.1, 0);
    assert!(!app.windows[&WID].inset_hover_cleared);
    app.on_mouse_input(WID, ElementState::Released, WinitMouseButton::Left);
    assert!(!app.windows[&WID].selecting);
}

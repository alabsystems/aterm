// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pointer routing between a System State page and its split siblings: a wheel or press
//! reaches the leaf under the pointer, never the focused one by default.

use aterm_window::event::MouseScrollDelta;

use crate::native_app::AppViewState;
use crate::native_settings::{SettingsRoute, SettingsViewState};
use crate::tab_model::{SplitAxis, ViewId};
use crate::{App, WindowId};

struct Split {
    app: App,
    window: WindowId,
    page: ViewId,
    terminal: u64,
    terminal_view: ViewId,
}

fn present_mixed(app: &mut App, window: WindowId) {
    assert!(app.prepare_heterogeneous_input_scratch(window).is_some());
    // The existing glassless pointer fixture's presentation boundary, applied
    // to every native raster of this real mixed layout rather than only focus.
    let leaves = app.active_visible_leaf_plan(window).unwrap().leaves;
    for leaf in leaves {
        if let Some(raster) = app
            .windows
            .get_mut(&window)
            .unwrap()
            .leaf_render_cache
            .get_mut(&leaf.view)
            .and_then(|cache| cache.native.as_mut())
        {
            raster.presented = true;
        }
    }
}

fn page_beside_terminal() -> Split {
    let mut app = App::headless_for_test();
    // A real window with inert headless platform ports.
    app.headless = false;
    let window = WindowId(0);
    let state = app.windows.get_mut(&window).unwrap();
    state.os_window_for_test = true;
    state.rows = 30;
    state.cols = 200;
    app.set_system_state_handlers_for_test(
        crate::system_state_host::InitialSetupHandlers::inert_for_test(),
    );
    assert!(app.open_system_state_tab());
    let (_, page) = app.active_native_view(window).unwrap();
    // A read Mac with every line open, so the page's list runs past the pane and a wheel has
    // somewhere to go (lines open to nothing while the plan is unknown).
    app.publish_system_state(crate::system_state::fixture_status("mixed").unwrap());
    let Some(AppViewState::SystemState(state)) = app.native_runtime.view_state(page) else {
        panic!("a System State view");
    };
    state.open_every_line();
    assert!(app.commit_glassless_native_frame(window));
    assert!(app.system_state_frontmost());

    let (terminal, terminal_view) =
        app.split_active_with_stub_terminal(window, SplitAxis::Horizontal);
    present_mixed(&mut app, window);
    assert_eq!(app.focused_session_id(window), Some(terminal));
    assert!(!app.system_state_frontmost());
    Split {
        app,
        window,
        page,
        terminal,
        terminal_view,
    }
}

fn point_at(app: &mut App, window: WindowId, view: ViewId) {
    let plan = app.active_visible_leaf_plan(window).unwrap();
    let leaf = plan.leaf(view).unwrap();
    let row = leaf.rect.origin.y as u16 + 5;
    let col = leaf.rect.origin.x as u16 + 5;
    let (x, y) = app.cell_centre_px(window, row, col).unwrap();
    app.on_cursor_moved(window, x, y);
}

fn page_scroll(app: &App, view: ViewId) -> f32 {
    let Some(AppViewState::SystemState(state)) = app.native_runtime.view_state(view) else {
        panic!("the System State view survives");
    };
    state.list_scroll().0
}

fn return_to_page(split: &mut Split) {
    assert!(
        split
            .app
            .focus_pane_in(split.window, crate::pane::FocusDir::Left)
    );
    assert_eq!(
        split.app.active_native_view(split.window).unwrap().1,
        split.page
    );
    present_mixed(&mut split.app, split.window);
    assert!(split.app.system_state_frontmost());
}

fn wheel_on_unfocused_page(delta: MouseScrollDelta, scrolls: bool) {
    let mut split = page_beside_terminal();
    let before = page_scroll(&split.app, split.page);
    let terminal_before = crate::term_lock(&split.app.pool.get(split.terminal).unwrap().term)
        .grid()
        .display_offset();
    point_at(&mut split.app, split.window, split.page);
    split.app.on_mouse_wheel(split.window, delta);
    let after = page_scroll(&split.app, split.page);
    assert_eq!(
        split.app.focused_session_id(split.window),
        Some(split.terminal)
    );
    assert_eq!(
        crate::term_lock(&split.app.pool.get(split.terminal).unwrap().term)
            .grid()
            .display_offset(),
        terminal_before,
        "a wheel over the page cannot scroll its terminal sibling"
    );
    return_to_page(&mut split);
    if scrolls {
        assert!(
            after > before,
            "the notched wheel scrolls the page it reached"
        );
    } else {
        assert_eq!(
            after, before,
            "a subnotch has not scrolled a whole line yet"
        );
    }
    assert!(split.app.structural_invariants_ok());
}

#[test]
fn split_wheel_on_unfocused_page_scrolls_that_page() {
    wheel_on_unfocused_page(MouseScrollDelta::LineDelta(0.0, -1.0), true);
}

#[test]
fn split_subnotch_on_unfocused_page_scrolls_no_line_yet() {
    wheel_on_unfocused_page(
        MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(0.0, -0.25)),
        false,
    );
}

#[test]
fn split_wheel_on_terminal_leaves_the_page_as_it_is() {
    let mut split = page_beside_terminal();
    let before = page_scroll(&split.app, split.page);
    point_at(&mut split.app, split.window, split.terminal_view);
    split
        .app
        .on_mouse_wheel(split.window, MouseScrollDelta::LineDelta(0.0, -1.0));
    assert_eq!(
        split.app.focused_session_id(split.window),
        Some(split.terminal)
    );
    assert_eq!(page_scroll(&split.app, split.page), before);
    return_to_page(&mut split);
    assert!(split.app.structural_invariants_ok());
}

#[test]
fn split_terminal_wheel_preserves_the_focused_page() {
    let mut split = page_beside_terminal();
    assert!(
        split
            .app
            .focus_pane_in(split.window, crate::pane::FocusDir::Left)
    );
    present_mixed(&mut split.app, split.window);
    let before = page_scroll(&split.app, split.page);
    point_at(&mut split.app, split.window, split.terminal_view);
    split
        .app
        .on_mouse_wheel(split.window, MouseScrollDelta::LineDelta(0.0, -1.0));
    assert_eq!(
        split.app.active_native_view(split.window).unwrap().1,
        split.page,
        "the wheel does not move keyboard focus to the terminal"
    );
    assert_eq!(page_scroll(&split.app, split.page), before);
    assert!(split.app.structural_invariants_ok());
}

fn page_with_settings_sibling() -> (Split, ViewId) {
    let mut split = page_beside_terminal();
    assert!(split.app.open_settings_tab(SettingsRoute::Security));
    let (settings, _) = split.app.active_native_view(split.window).unwrap();
    assert!(split.app.open_system_state_tab());
    let mut state = SettingsViewState::new(&split.app.config);
    state.route = SettingsRoute::Security;
    let settings_view = split
        .app
        .split_active_with_native(
            split.window,
            SplitAxis::Horizontal,
            settings,
            AppViewState::Settings(Box::new(state)),
        )
        .expect("Settings is a native sibling of the page");
    assert!(
        split
            .app
            .focus_pane_in(split.window, crate::pane::FocusDir::Left)
    );
    present_mixed(&mut split.app, split.window);
    assert_eq!(
        split.app.active_native_view(split.window).unwrap().1,
        split.page
    );
    (split, settings_view)
}

fn settings_scroll(app: &App, view: ViewId) -> usize {
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
        panic!("the Settings sibling survives");
    };
    state.page_scroll
}

fn point_at_settings_body(split: &mut Split, settings_view: ViewId) {
    let plan = split.app.active_visible_leaf_plan(split.window).unwrap();
    let leaf = plan.leaf(settings_view).unwrap();
    // Aim into the page body, away from Settings' navigation rail.
    let row = leaf.rect.origin.y as u16 + 10;
    let col = (leaf.rect.origin.x + leaf.rect.size.width * 0.75) as u16;
    let (x, y) = split.app.cell_centre_px(split.window, row, col).unwrap();
    split.app.on_cursor_moved(split.window, x, y);
}

#[test]
fn split_settings_wheels_before_a_frame_only_scroll_settings() {
    let (mut split, settings_view) = page_with_settings_sibling();
    let before = page_scroll(&split.app, split.page);
    let settings_before = settings_scroll(&split.app, settings_view);
    point_at_settings_body(&mut split, settings_view);
    for _ in 0..2 {
        // No frame or cursor move between events: the second wheel must not
        // fall back to the focused page when the Settings raster is dirty.
        split
            .app
            .on_mouse_wheel(split.window, MouseScrollDelta::LineDelta(0.0, -1.0));
    }
    assert!(settings_scroll(&split.app, settings_view) > settings_before);
    assert_eq!(page_scroll(&split.app, split.page), before);
    assert_eq!(
        split.app.active_native_view(split.window).unwrap().1,
        split.page,
        "only the leaf under the wheel scrolls; keyboard focus stays put"
    );
    assert!(split.app.structural_invariants_ok());
}

#[test]
fn split_press_sibling_hold_keeps_the_wheel_on_settings() {
    use aterm_window::event::{ElementState, MouseButton};

    let (mut split, settings_view) = page_with_settings_sibling();
    let before = page_scroll(&split.app, split.page);
    let settings_before = settings_scroll(&split.app, settings_view);
    point_at_settings_body(&mut split, settings_view);
    split
        .app
        .mouse_input_then_fold(split.window, ElementState::Pressed, MouseButton::Right);
    assert_eq!(
        split.app.active_native_view(split.window).unwrap().1,
        split.page,
        "the Settings press does not move keyboard focus"
    );
    // Keep the button down through the physical wheel; do not present a new
    // frame or move focus to hide which native leaf acquired the capture.
    split
        .app
        .on_mouse_wheel(split.window, MouseScrollDelta::LineDelta(0.0, -1.0));
    split
        .app
        .mouse_input_then_fold(split.window, ElementState::Released, MouseButton::Right);
    assert_eq!(page_scroll(&split.app, split.page), before);
    assert!(settings_scroll(&split.app, settings_view) > settings_before);
    assert_eq!(
        split.app.active_native_view(split.window).unwrap().1,
        split.page
    );
    assert_eq!(split.app.windows[&split.window].page_buttons, 0);
    assert!(split.app.structural_invariants_ok());
}

#[cfg(unix)]
mod presses {
    use std::io::{ErrorKind, Read};
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;
    use aterm_window::event::{ElementState, MouseButton};

    use super::*;

    fn observe_terminal(split: &mut Split) -> UnixStream {
        let (reader, writer) = UnixStream::pair().expect("private terminal input observer");
        reader.set_nonblocking(true).unwrap();
        let sink = Arc::new(SinkWriter::new_owned(writer.into()));
        split
            .app
            .pool
            .insert(crate::stub_session_with_sink(split.terminal, sink));
        split.app.resync_active_or_window(split.window);
        crate::term_lock(&split.app.pool.get(split.terminal).unwrap().term)
            .process(b"\x1b[?1002h\x1b[?1006h");
        present_mixed(&mut split.app, split.window);
        reader
    }

    fn drain(reader: &mut UnixStream) -> String {
        let mut out = Vec::new();
        let mut bytes = [0; 256];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => out.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("reading private terminal input: {error}"),
            }
        }
        String::from_utf8(out).expect("mouse reports are ASCII")
    }

    fn click(split: &mut Split, button: MouseButton) {
        // The physical event-loop boundary includes the post-dispatch fold.
        for state in [ElementState::Pressed, ElementState::Released] {
            split.app.mouse_input_then_fold(split.window, state, button);
        }
        let window = &split.app.windows[&split.window];
        assert_eq!(window.reported_buttons, 0);
        assert_eq!(window.page_buttons, 0);
        assert!(window.held_mouse_button.is_none());
    }

    fn press_on_unfocused_page(button: MouseButton) {
        let mut split = page_beside_terminal();
        let mut terminal = observe_terminal(&mut split);
        point_at(&mut split.app, split.window, split.page);
        click(&mut split, button);
        assert_eq!(drain(&mut terminal), "", "the page is not terminal input");
        assert_eq!(
            split.app.focused_session_id(split.window),
            Some(split.terminal),
            "a non-left press does not move keyboard focus"
        );
        return_to_page(&mut split);
        assert!(split.app.structural_invariants_ok());
    }

    fn press_on_terminal_preserves_page_focus(button: MouseButton, report_code: u8) {
        let mut split = page_beside_terminal();
        let mut terminal = observe_terminal(&mut split);
        assert!(
            split
                .app
                .focus_pane_in(split.window, crate::pane::FocusDir::Left)
        );
        present_mixed(&mut split.app, split.window);
        let (row, col, ..) = split
            .app
            .visible_session_rect(split.window, split.terminal)
            .expect("the terminal sibling is visible");
        let (x, y) = split
            .app
            .cell_centre_px(split.window, row + 3, col + 4)
            .unwrap();
        split.app.on_cursor_moved(split.window, x, y);
        click(&mut split, button);
        assert_eq!(
            drain(&mut terminal),
            format!("\x1b[<{report_code};5;4M\x1b[<{report_code};5;4m"),
            "the terminal receives its own press and release at its local cell"
        );
        assert_eq!(
            split.app.active_native_view(split.window).unwrap().1,
            split.page,
            "a non-left terminal press preserves the page's keyboard focus"
        );
        assert!(split.app.structural_invariants_ok());
    }

    #[test]
    fn split_press_right_on_unfocused_page_is_not_terminal_input() {
        press_on_unfocused_page(MouseButton::Right);
    }

    #[test]
    fn split_press_middle_on_unfocused_page_is_not_terminal_input() {
        press_on_unfocused_page(MouseButton::Middle);
    }

    #[test]
    fn split_press_right_on_terminal_preserves_page_focus_and_report() {
        press_on_terminal_preserves_page_focus(MouseButton::Right, 2);
    }

    #[test]
    fn split_press_middle_on_terminal_preserves_page_focus_and_report() {
        press_on_terminal_preserves_page_focus(MouseButton::Middle, 1);
    }

    #[test]
    fn split_press_left_body_miss_owns_the_wheel_over_an_older_terminal_right() {
        let mut split = page_beside_terminal();
        let mut terminal = observe_terminal(&mut split);
        assert!(
            split
                .app
                .focus_pane_in(split.window, crate::pane::FocusDir::Left)
        );
        present_mixed(&mut split.app, split.window);
        let (row, col, ..) = split
            .app
            .visible_session_rect(split.window, split.terminal)
            .unwrap();
        let (x, y) = split
            .app
            .cell_centre_px(split.window, row + 3, col + 4)
            .unwrap();
        split.app.on_cursor_moved(split.window, x, y);
        split
            .app
            .mouse_input_then_fold(split.window, ElementState::Pressed, MouseButton::Right);
        assert_eq!(drain(&mut terminal), "\x1b[<2;5;4M");

        point_at(&mut split.app, split.window, split.page);
        let _ = drain(&mut terminal); // The older right drag is still terminal-owned.
        let before = page_scroll(&split.app, split.page);
        split
            .app
            .mouse_input_then_fold(split.window, ElementState::Pressed, MouseButton::Left);
        assert!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .pressed
                .is_none(),
            "the left press reached an empty part of the page, not a control"
        );
        split
            .app
            .on_mouse_wheel(split.window, MouseScrollDelta::LineDelta(0.0, -1.0));
        assert!(
            page_scroll(&split.app, split.page) > before,
            "the page's lower left button owns this wheel despite the older terminal right"
        );
        assert_eq!(
            drain(&mut terminal),
            "",
            "the terminal receives neither the page's left press nor its wheel"
        );
        split
            .app
            .mouse_input_then_fold(split.window, ElementState::Released, MouseButton::Left);
        assert_eq!(drain(&mut terminal), "", "the left release is the page's");
        split
            .app
            .mouse_input_then_fold(split.window, ElementState::Released, MouseButton::Right);
        let release = drain(&mut terminal);
        assert!(
            release.starts_with("\x1b[<2;") && release.ends_with('m'),
            "the terminal retains its own right release: {release:?}"
        );
        let window = &split.app.windows[&split.window];
        assert_eq!((window.page_buttons, window.reported_buttons), (0, 0));
        assert!(window.held_mouse_button.is_none());
        assert_eq!(
            split.app.active_native_view(split.window).unwrap().1,
            split.page
        );
        assert!(split.app.structural_invariants_ok());
    }

    #[test]
    fn split_press_on_subtab_header_only_focuses_the_page() {
        let mut split = page_beside_terminal();
        let mut terminal = observe_terminal(&mut split);
        let header = split
            .app
            .active_visible_leaf_plan(split.window)
            .unwrap()
            .leaf(split.page)
            .unwrap()
            .header
            .expect("the split page has a host-owned title row");
        let (x, y) = split
            .app
            .cell_centre_px(
                split.window,
                header.origin.y as u16,
                header.origin.x as u16 + 4,
            )
            .unwrap();
        split.app.on_cursor_moved(split.window, x, y);
        click(&mut split, MouseButton::Left);
        assert_eq!(drain(&mut terminal), "");
        assert_eq!(
            split.app.active_native_view(split.window).unwrap().1,
            split.page,
            "the host-owned header focuses the canonical leaf"
        );
        assert!(split.app.structural_invariants_ok());
    }

    #[test]
    fn split_divider_hover_drag_and_release_with_focused_page() {
        let mut split = page_beside_terminal();
        assert!(
            split
                .app
                .focus_pane_in(split.window, crate::pane::FocusDir::Left)
        );
        present_mixed(&mut split.app, split.window);
        let plan_before = split.app.active_visible_leaf_plan(split.window).unwrap();
        let page_width_before = plan_before.leaf(split.page).unwrap().rect.size.width;
        let div_col = plan_before.dividers[0].rect.origin.x as u16;
        let (dx, dy) = split.app.cell_centre_px(split.window, 10, div_col).unwrap();

        split.app.on_cursor_moved(split.window, dx, dy);
        assert_eq!(
            split.app.divider_cursor_under_pointer(split.window),
            Some(aterm_window::window::CursorIcon::ColResize)
        );
        assert_eq!(
            (
                split.app.windows[&split.window].hover_pointer,
                split.app.windows[&split.window].native_text_cursor,
            ),
            (true, true),
            "hovering canonical divider while native page is focused sets resize cursor state"
        );

        split
            .app
            .mouse_input_then_fold(split.window, ElementState::Pressed, MouseButton::Left);
        assert!(
            split.app.windows[&split.window].divider_drag.is_some(),
            "pressing canonical divider begins divider drag"
        );

        let (drag_x, drag_y) = split
            .app
            .cell_centre_px(split.window, 10, div_col + 20)
            .unwrap();
        split.app.on_cursor_moved(split.window, drag_x, drag_y);

        split
            .app
            .mouse_input_then_fold(split.window, ElementState::Released, MouseButton::Left);
        assert!(
            split.app.windows[&split.window].divider_drag.is_none(),
            "releasing left button while native page is focused finishes divider drag"
        );

        let plan_after = split.app.active_visible_leaf_plan(split.window).unwrap();
        let page_width_after = plan_after.leaf(split.page).unwrap().rect.size.width;
        assert!(
            page_width_after > page_width_before,
            "dragging canonical divider right widens left page ({page_width_before} -> {page_width_after})"
        );
        assert_eq!(
            split.app.active_native_view(split.window).unwrap().1,
            split.page
        );
        assert!(split.app.structural_invariants_ok());
    }

    #[test]
    fn split_terminal_gap_scrollbar_with_focused_page_grabs_and_drags_instead_of_divider() {
        let mut app = App::headless_for_test();
        app.headless = false;
        app.config.scrollbar = Some(true);
        let wid = WindowId(0);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.os_window_for_test = true;
            ws.rows = 30;
            ws.cols = 200;
            ws.metrics.pad = 12;
        }
        let left_terminal = app.focused_session_id(wid).unwrap();
        let term = app.pool.get(left_terminal).unwrap().term.clone();
        {
            let mut t = crate::term_lock(&term);
            for i in 0..400 {
                t.process(format!("line {i}\r\n").as_bytes());
            }
        }
        app.open_allocation(true).expect("open allocation split");
        present_mixed(&mut app, wid);
        let (_, native_view) = app.active_native_view(wid).expect("focused native view");

        let plan = app.active_visible_leaf_plan(wid).unwrap();
        app.refresh_pane_overlay_for_capture(wid, &plan, std::time::Instant::now());
        let inputs = app.pane_overlay_inputs(wid, false).expect("inputs");
        let facts = app.windows[&wid].pane_overlay.facts()[0];
        assert_eq!(facts.session, left_terminal);
        let (cw, ch) = inputs.cell;
        let m = crate::scrollbar::Metrics::for_cell(cw as u32, ch as u32);
        let lane = crate::scrollbar::lane(
            (
                facts.rect.row,
                facts.rect.col,
                facts.rect.rows,
                facts.rect.cols,
            ),
            inputs.win_cols,
            inputs.cell,
            inputs.pad,
            m,
        )
        .expect("gap lane");
        assert!(
            !lane.edge,
            "left terminal pane uses a gap scrollbar on the divider column"
        );
        let (top, len) = crate::scrollbar::thumb(lane, facts.rect.rows as u32, facts.history, 0, m)
            .expect("thumb");
        let (sx_bar, sw_bar) = crate::scrollbar::span(lane, m.wide, m);
        let top_band =
            app.win_pad_top(wid) + app.win_head(wid) + usize::from(app.chrome_rows(wid)) * ch;
        let wx = f64::from(sx_bar + sw_bar / 2) + app.win_pad(wid) as f64;
        let wy = f64::from(top + len / 2) + top_band as f64;

        app.on_cursor_moved(wid, wx, wy);
        assert_eq!(
            app.windows[&wid].pane_overlay.hover,
            Some(left_terminal),
            "hovering gap scrollbar while native sibling is focused lights the bar"
        );

        let native_leaf = plan.leaf(native_view).unwrap();
        let (sx, sy) = app
            .cell_centre_px(
                wid,
                native_leaf.rect.origin.y as u16 + 5,
                native_leaf.rect.origin.x as u16 + 5,
            )
            .unwrap();
        app.on_cursor_moved(wid, sx, sy);
        assert_eq!(
            app.windows[&wid].pane_overlay.hover, None,
            "moving from terminal scrollbar onto focused native pane clears scrollbar hover"
        );

        app.on_cursor_moved(wid, wx, wy);
        assert_eq!(app.windows[&wid].pane_overlay.hover, Some(left_terminal));
        app.mouse_input_then_fold(wid, ElementState::Pressed, MouseButton::Left);
        assert!(
            app.windows[&wid].pane_overlay.drag.is_some(),
            "pressing gap scrollbar thumb in heterogeneous split grabs thumb"
        );
        assert!(
            app.windows[&wid].divider_drag.is_none(),
            "gap scrollbar press takes precedence over divider drag"
        );

        let wy_up = f64::from(lane.top + 4) + top_band as f64;
        app.on_cursor_moved(wid, wx, wy_up);
        assert!(
            crate::term_lock(&term).grid().display_offset() > 0,
            "dragging gap scrollbar thumb while native sibling is focused scrolls terminal"
        );

        app.mouse_input_then_fold(wid, ElementState::Released, MouseButton::Left);
        assert!(app.windows[&wid].pane_overlay.drag.is_none());
        assert_eq!(app.active_native_view(wid).unwrap().1, native_view);
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn split_subtab_header_and_divider_are_shielded_by_banner_and_stats_lens() {
        let mut split = page_beside_terminal();
        let wid = split.window;
        let plan = split.app.active_visible_leaf_plan(wid).unwrap();
        let header = plan
            .leaf(split.page)
            .unwrap()
            .header
            .expect("the split page has a host-owned title row");
        let (hx, hy) = split
            .app
            .cell_centre_px(wid, header.origin.y as u16, header.origin.x as u16 + 4)
            .unwrap();
        let div_col = plan.dividers[0].rect.origin.x as u16;
        let (dx, dy) = split
            .app
            .cell_centre_px(wid, header.origin.y as u16, div_col)
            .unwrap();

        let rows = usize::from(split.app.windows[&wid].rows);
        split
            .app
            .windows
            .get_mut(&wid)
            .unwrap()
            .input_scratch
            .cells
            .resize(rows, Vec::new());
        split.app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
            wid,
            Some(split.terminal),
            "one\ntwo\n".to_owned(),
            crate::input::Source::Controller,
            crate::input::PasteFraming::Gesture { bracketed: true },
        ));
        split.app.windows.get_mut(&wid).unwrap().last_cursor_px = (hx, hy);
        assert!(
            !split.app.pointer_is_over_pane_header(wid),
            "paste banner shields split pane header on row 0 even before terminal chrome_rows renders"
        );
        split.app.paste_banner = None;

        {
            let ws = split.app.windows.get_mut(&wid).unwrap();
            ws.pane_overlay.hover = Some(split.terminal);
            ws.stats_lens.open = Some(crate::stats_lens::Lens {
                focus: crate::stats_paint::Instrument::Cpu,
                bar: crate::stats_paint::Bar::Global,
                how: crate::stats_lens::Opened::Hover,
            });
            ws.stats_lens.rect = Some((0, 0, 2_000, 300));
        }
        assert!(
            !split.app.pointer_is_over_pane_header(wid),
            "open stats lens card shields split pane header under the card"
        );
        split.app.on_cursor_moved(wid, dx, dy);
        assert_eq!(
            split.app.windows[&wid].pane_overlay.hover, None,
            "moving pointer onto open stats lens clears hovered scrollbar"
        );
        assert_eq!(
            split.app.divider_cursor_under_pointer(wid),
            None,
            "open stats lens shields split divider cursor"
        );

        split.app.on_cursor_moved(wid, hx, hy);
        click(&mut split, MouseButton::Left);
        assert_eq!(
            split.app.windows[&wid].stats_lens.open.map(|l| l.how),
            Some(crate::stats_lens::Opened::Pinned),
            "clicking open hover stats lens over split pane header pins lens instead of focusing pane"
        );
        assert!(
            split.app.active_native_view(wid).is_none(),
            "terminal pane stays focused when click lands on stats lens card"
        );

        split.app.windows.get_mut(&wid).unwrap().stats_lens.rect = Some((1_500, 0, 200, 200));
        assert!(
            split.app.pointer_is_over_pane_header(wid),
            "uncovered split pane header is hit-testable outside stats lens card"
        );
        split.app.on_cursor_moved(wid, hx, hy);
        click(&mut split, MouseButton::Left);
        assert!(
            split.app.windows[&wid].stats_lens.open.is_none(),
            "clicking uncovered split pane header closes open stats lens"
        );
        assert_eq!(
            split.app.active_native_view(wid).unwrap().1,
            split.page,
            "clicking uncovered split pane header focuses the page leaf"
        );
        assert!(split.app.structural_invariants_ok());
    }

    #[test]
    fn native_control_press_settles_across_stats_lens_release_blur_and_pane_switch() {
        let mut split = page_beside_terminal();
        let wid = split.window;
        return_to_page(&mut split);
        let (cx, cy, key) = {
            let artifact = split
                .app
                .retained_native_leaf_artifact(wid, split.page, true)
                .expect("presented System State leaf");
            let hit = artifact
                .compiled
                .hits
                .iter()
                .find(|hit| hit.rect.y > 40.0)
                .expect("actionable System State control");
            let scale = artifact.scale;
            (
                artifact.device_x as f64 + f64::from(hit.rect.x + hit.rect.width * 0.5) * scale,
                artifact.device_y as f64 + f64::from(hit.rect.y + hit.rect.height * 0.5) * scale,
                hit.key.clone(),
            )
        };

        // 1. Press the native control, then open a stats lens covering the release location
        // and release the left button over the lens: the armed press must clear without firing.
        split.app.on_cursor_moved(wid, cx, cy);
        split
            .app
            .mouse_input_then_fold(wid, ElementState::Pressed, MouseButton::Left);
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .pressed,
            Some(key.clone()),
            "pressing the control arms state.common().pressed"
        );
        {
            let ws = split.app.windows.get_mut(&wid).unwrap();
            ws.stats_lens.open = Some(crate::stats_lens::Lens {
                focus: crate::stats_paint::Instrument::Cpu,
                bar: crate::stats_paint::Bar::Global,
                how: crate::stats_lens::Opened::Pinned,
            });
            ws.stats_lens.rect = Some((0, 0, 2_000, 1_000));
        }
        assert!(
            split.app.stats_owns_pointer(wid),
            "stats lens owns the pointer at the release position"
        );
        split
            .app
            .mouse_input_then_fold(wid, ElementState::Released, MouseButton::Left);
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .pressed,
            None,
            "releasing over the stats lens clears the armed native control press"
        );
        split.app.windows.get_mut(&wid).unwrap().stats_lens.open = None;
        split.app.windows.get_mut(&wid).unwrap().stats_lens.rect = None;
        present_mixed(&mut split.app, wid);

        // 2. Press the native control, then lose window focus: `on_focus(wid, false)` must clear
        // the armed press via `settle_selection_gesture`.
        split.app.on_cursor_moved(wid, cx, cy);
        split
            .app
            .mouse_input_then_fold(wid, ElementState::Pressed, MouseButton::Left);
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .pressed,
            Some(key.clone())
        );
        split.app.on_focus(wid, false);
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .pressed,
            None,
            "window blur settles an armed native control press"
        );
        assert_eq!(
            split.app.windows[&wid].page_buttons, 0,
            "window blur clears held page_buttons capture"
        );
        split.app.on_focus(wid, true);
        present_mixed(&mut split.app, wid);
        point_at(&mut split.app, wid, split.terminal_view);
        assert!(
            matches!(
                split.app.pointer_route(wid, None),
                crate::app_mouse::PointerRoute::Pane { session, .. } if session == split.terminal
            ),
            "after window blur settled page_buttons, pointer over terminal sibling routes to terminal pane"
        );

        // 3. Hovering a native control and opening a stats lens over it evicts common().hovered,
        // shields refresh_native_hover_for_frame while open, and restores hover when closed.
        split.app.on_cursor_moved(wid, cx, cy);
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .hovered,
            Some(key.clone()),
            "moving onto uncovered native control sets common().hovered"
        );
        {
            let ws = split.app.windows.get_mut(&wid).unwrap();
            ws.stats_lens.open = Some(crate::stats_lens::Lens {
                focus: crate::stats_paint::Instrument::Cpu,
                bar: crate::stats_paint::Bar::Global,
                how: crate::stats_lens::Opened::Pinned,
            });
            ws.stats_lens.rect = Some((0, 0, 2_000, 1_000));
            ws.native_rehover = true;
        }
        split.app.evict_native_hover_under_stats_lens(wid);
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .hovered,
            None,
            "opening stats lens over hovered native control evicts common().hovered"
        );
        present_mixed(&mut split.app, wid);
        split.app.refresh_native_hover_for_frame(wid);
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .hovered,
            None,
            "refresh_native_hover_for_frame keeps native control unhovered while stats lens owns pointer"
        );
        assert!(split.app.close_stats_lens(wid));
        split.app.windows.get_mut(&wid).unwrap().stats_lens.rect = None;
        present_mixed(&mut split.app, wid);
        split.app.refresh_native_hover_for_frame(wid);
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .hovered,
            Some(key.clone()),
            "closing stats lens and refreshing frame hover restores common().hovered under still pointer"
        );

        // 4. Press the native control, then switch focus to the terminal sibling: `sync_window`
        // must settle the armed native control press on the outgoing view.
        split.app.on_cursor_moved(wid, cx, cy);
        split
            .app
            .mouse_input_then_fold(wid, ElementState::Pressed, MouseButton::Left);
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .pressed,
            Some(key)
        );
        assert!(split.app.focus_pane_in(wid, crate::pane::FocusDir::Right));
        assert_eq!(
            split
                .app
                .native_runtime
                .view_state(split.page)
                .unwrap()
                .common()
                .pressed,
            None,
            "switching pane focus settles an armed native control press"
        );
        assert!(split.app.structural_invariants_ok());
    }
}

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
}

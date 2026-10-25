// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_types::macos_setup::Plan;
use aterm_window::event::MouseScrollDelta;

use crate::native_app::AppViewState;
use crate::native_settings::{SettingsRoute, SettingsViewState};
use crate::tab_model::{SplitAxis, ViewId};
use crate::workstation_setup::{Request, SetupAnswer, WorkstationSetupDecision};
use crate::{App, WindowId};

struct Review {
    app: App,
    window: WindowId,
    page: ViewId,
    terminal: u64,
    terminal_view: ViewId,
    request: Request,
    answer: mpsc::Receiver<SetupAnswer>,
    start: Instant,
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

fn review_beside_terminal() -> Review {
    let mut app = App::headless_for_test();
    // A real windowed offer with inert headless platform ports.
    app.headless = false;
    let window = WindowId(0);
    let state = app.windows.get_mut(&window).unwrap();
    state.os_window_for_test = true;
    state.rows = 30;
    state.cols = 200;
    app.set_system_state_handlers_for_test(
        crate::system_state_host::InitialSetupHandlers::inert_for_test(),
    );
    let (send, answer) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.offer_system_state(request.clone());
    let (_, page) = app.active_native_view(window).unwrap();
    assert!(app.commit_glassless_native_frame(window));
    let start = Instant::now();
    app.tick_system_state(start);
    assert!(app.system_state_frontmost());
    assert!(request.pending());

    let (terminal, terminal_view) =
        app.split_active_with_stub_terminal(window, SplitAxis::Horizontal);
    present_mixed(&mut app, window);
    assert_eq!(app.focused_session_id(window), Some(terminal));
    assert!(!app.system_state_frontmost());
    app.tick_system_state(start + Duration::from_secs(1));
    Review {
        app,
        window,
        page,
        terminal,
        terminal_view,
        request,
        answer,
        start,
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
        panic!("the startup review's view survives");
    };
    state.list_scroll().0
}

fn return_to_review(review: &mut Review) {
    assert!(
        review
            .app
            .focus_pane_in(review.window, crate::pane::FocusDir::Left)
    );
    assert_eq!(
        review.app.active_native_view(review.window).unwrap().1,
        review.page
    );
    advance_visible_review(review);
}

fn advance_visible_review(review: &mut Review) {
    present_mixed(&mut review.app, review.window);
    assert!(review.app.system_state_frontmost());
    // Real host visibility and focus decide active time; no direct active=true
    // model tick can conceal a misrouted physical wheel.
    review
        .app
        .tick_system_state(review.start + Duration::from_secs(2));
    review
        .app
        .tick_system_state(review.start + Duration::from_secs(33));
}

fn accept_automatic_review(review: &mut Review) {
    let mut answer = review
        .answer
        .try_recv()
        .expect("the untouched review still expires");
    assert_eq!(
        answer.decision,
        WorkstationSetupDecision::Begin(Plan::default())
    );
    assert_eq!(
        answer.receipt.as_ref().unwrap().admission(),
        crate::InitialSetupAdmission::Automatic
    );
    assert!(!review.request.pending());
    assert!(review.answer.try_recv().is_err());
    answer.receipt.take().unwrap().accept();
    review.app.resolve_system_state_apply();
    assert!(review.app.structural_invariants_ok());
}

fn wheel_on_unfocused_review_stops_apply(delta: MouseScrollDelta, scrolls: bool) {
    let mut review = review_beside_terminal();
    let before = page_scroll(&review.app, review.page);
    let terminal_before = crate::term_lock(&review.app.pool.get(review.terminal).unwrap().term)
        .grid()
        .display_offset();
    point_at(&mut review.app, review.window, review.page);
    review.app.on_mouse_wheel(review.window, delta);
    let after = page_scroll(&review.app, review.page);
    assert_eq!(
        review.app.focused_session_id(review.window),
        Some(review.terminal)
    );
    assert_eq!(
        crate::term_lock(&review.app.pool.get(review.terminal).unwrap().term)
            .grid()
            .display_offset(),
        terminal_before,
        "a wheel over the review cannot scroll its terminal sibling"
    );
    return_to_review(&mut review);
    assert!(
        review.request.pending(),
        "a wheel reaching the unfocused review permanently stops automatic Apply"
    );
    assert!(review.answer.try_recv().is_err());
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
    assert!(review.app.structural_invariants_ok());
}

#[test]
fn startup_review_split_wheel_on_unfocused_page_stops_apply_and_scrolls_that_page() {
    wheel_on_unfocused_review_stops_apply(MouseScrollDelta::LineDelta(0.0, -1.0), true);
}

#[test]
fn startup_review_split_subnotch_on_unfocused_page_stops_apply_before_a_scroll_line() {
    wheel_on_unfocused_review_stops_apply(
        MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(0.0, -0.25)),
        false,
    );
}

#[test]
fn startup_review_split_wheel_on_terminal_does_not_stop_the_review() {
    let mut review = review_beside_terminal();
    let before = page_scroll(&review.app, review.page);
    point_at(&mut review.app, review.window, review.terminal_view);
    review
        .app
        .on_mouse_wheel(review.window, MouseScrollDelta::LineDelta(0.0, -1.0));
    assert_eq!(
        review.app.focused_session_id(review.window),
        Some(review.terminal)
    );
    assert_eq!(page_scroll(&review.app, review.page), before);
    return_to_review(&mut review);
    accept_automatic_review(&mut review);
}

#[test]
fn startup_review_split_terminal_wheel_preserves_the_focused_review() {
    let mut review = review_beside_terminal();
    assert!(
        review
            .app
            .focus_pane_in(review.window, crate::pane::FocusDir::Left)
    );
    present_mixed(&mut review.app, review.window);
    let before = page_scroll(&review.app, review.page);
    point_at(&mut review.app, review.window, review.terminal_view);
    review
        .app
        .on_mouse_wheel(review.window, MouseScrollDelta::LineDelta(0.0, -1.0));
    assert_eq!(
        review.app.active_native_view(review.window).unwrap().1,
        review.page,
        "the wheel does not move keyboard focus to the terminal"
    );
    assert_eq!(page_scroll(&review.app, review.page), before);
    assert!(review.request.pending());
    advance_visible_review(&mut review);
    accept_automatic_review(&mut review);
}

fn review_with_settings_sibling() -> (Review, ViewId) {
    let mut review = review_beside_terminal();
    assert!(review.app.open_settings_tab(SettingsRoute::Security));
    let (settings, _) = review.app.active_native_view(review.window).unwrap();
    assert!(review.app.open_system_state_tab());
    let mut state = SettingsViewState::new(&review.app.config);
    state.route = SettingsRoute::Security;
    let settings_view = review
        .app
        .split_active_with_native(
            review.window,
            SplitAxis::Horizontal,
            settings,
            AppViewState::Settings(Box::new(state)),
        )
        .expect("Settings is a native sibling of the review");
    assert!(
        review
            .app
            .focus_pane_in(review.window, crate::pane::FocusDir::Left)
    );
    present_mixed(&mut review.app, review.window);
    assert_eq!(
        review.app.active_native_view(review.window).unwrap().1,
        review.page
    );
    (review, settings_view)
}

fn settings_scroll(app: &App, view: ViewId) -> usize {
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
        panic!("the Settings sibling survives");
    };
    state.page_scroll
}

fn point_at_settings_body(review: &mut Review, settings_view: ViewId) {
    let plan = review.app.active_visible_leaf_plan(review.window).unwrap();
    let leaf = plan.leaf(settings_view).unwrap();
    // Aim into the page body, away from Settings' navigation rail.
    let row = leaf.rect.origin.y as u16 + 10;
    let col = (leaf.rect.origin.x + leaf.rect.size.width * 0.75) as u16;
    let (x, y) = review.app.cell_centre_px(review.window, row, col).unwrap();
    review.app.on_cursor_moved(review.window, x, y);
}

#[test]
fn startup_review_split_settings_wheels_before_a_frame_only_scroll_settings() {
    let (mut review, settings_view) = review_with_settings_sibling();
    let before = page_scroll(&review.app, review.page);
    let settings_before = settings_scroll(&review.app, settings_view);
    point_at_settings_body(&mut review, settings_view);
    for _ in 0..2 {
        // No frame or cursor move between events: the second wheel must not
        // fall back to the focused review when the Settings raster is dirty.
        review
            .app
            .on_mouse_wheel(review.window, MouseScrollDelta::LineDelta(0.0, -1.0));
    }
    assert!(settings_scroll(&review.app, settings_view) > settings_before);
    assert_eq!(page_scroll(&review.app, review.page), before);
    assert_eq!(
        review.app.active_native_view(review.window).unwrap().1,
        review.page,
        "only the leaf under the wheel scrolls; keyboard focus stays put"
    );
    assert!(review.request.pending());
    assert!(review.answer.try_recv().is_err());
    advance_visible_review(&mut review);
    accept_automatic_review(&mut review);
}

#[test]
fn startup_review_split_press_sibling_hold_keeps_the_wheel_on_settings() {
    use aterm_window::event::{ElementState, MouseButton};

    let (mut review, settings_view) = review_with_settings_sibling();
    let before = page_scroll(&review.app, review.page);
    let settings_before = settings_scroll(&review.app, settings_view);
    point_at_settings_body(&mut review, settings_view);
    review
        .app
        .mouse_input_then_fold(review.window, ElementState::Pressed, MouseButton::Right);
    assert_eq!(
        review.app.active_native_view(review.window).unwrap().1,
        review.page,
        "the Settings press does not move keyboard focus"
    );
    let page = review.app.compiled_native_ui(review.window).unwrap();
    let apply = page
        .semantics
        .iter()
        .find(|node| node.key.as_str() == "system-state/apply")
        .expect("the review still has its Apply control");
    assert!(
        apply.description.is_some(),
        "the sibling press alone leaves the countdown running"
    );
    // Keep the button down through the physical wheel; do not present a new
    // frame or move focus to hide which native leaf acquired the capture.
    review
        .app
        .on_mouse_wheel(review.window, MouseScrollDelta::LineDelta(0.0, -1.0));
    review
        .app
        .mouse_input_then_fold(review.window, ElementState::Released, MouseButton::Right);
    assert_eq!(page_scroll(&review.app, review.page), before);
    assert!(settings_scroll(&review.app, settings_view) > settings_before);
    assert_eq!(
        review.app.active_native_view(review.window).unwrap().1,
        review.page
    );
    assert_eq!(review.app.windows[&review.window].page_buttons, 0);
    assert!(review.request.pending());
    assert!(review.answer.try_recv().is_err());
    advance_visible_review(&mut review);
    accept_automatic_review(&mut review);
}

#[cfg(unix)]
mod presses {
    use std::io::{ErrorKind, Read};
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;
    use aterm_window::event::{ElementState, MouseButton};

    use super::*;

    fn observe_terminal(review: &mut Review) -> UnixStream {
        let (reader, writer) = UnixStream::pair().expect("private terminal input observer");
        reader.set_nonblocking(true).unwrap();
        let sink = Arc::new(SinkWriter::new_owned(writer.into()));
        review
            .app
            .pool
            .insert(crate::stub_session_with_sink(review.terminal, sink));
        review.app.resync_active_or_window(review.window);
        crate::term_lock(&review.app.pool.get(review.terminal).unwrap().term)
            .process(b"\x1b[?1002h\x1b[?1006h");
        present_mixed(&mut review.app, review.window);
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

    fn click(review: &mut Review, button: MouseButton) {
        // The physical event-loop boundary includes the post-dispatch fold;
        // calling on_mouse_input alone misses its countdown attribution.
        for state in [ElementState::Pressed, ElementState::Released] {
            review
                .app
                .mouse_input_then_fold(review.window, state, button);
        }
        let window = &review.app.windows[&review.window];
        assert_eq!(window.reported_buttons, 0);
        assert_eq!(window.page_buttons, 0);
        assert!(window.held_mouse_button.is_none());
    }

    fn press_on_unfocused_review_stops_apply(button: MouseButton) {
        let mut review = review_beside_terminal();
        let mut terminal = observe_terminal(&mut review);
        point_at(&mut review.app, review.window, review.page);
        click(&mut review, button);
        assert_eq!(drain(&mut terminal), "", "the page is not terminal input");
        assert_eq!(
            review.app.focused_session_id(review.window),
            Some(review.terminal),
            "a non-left press does not move keyboard focus"
        );
        return_to_review(&mut review);
        assert!(
            review.request.pending(),
            "a press reaching the unfocused review permanently stops automatic Apply"
        );
        assert!(review.answer.try_recv().is_err());
        assert!(review.app.structural_invariants_ok());
    }

    fn press_on_terminal_preserves_review(button: MouseButton, report_code: u8) {
        let mut review = review_beside_terminal();
        let mut terminal = observe_terminal(&mut review);
        assert!(
            review
                .app
                .focus_pane_in(review.window, crate::pane::FocusDir::Left)
        );
        present_mixed(&mut review.app, review.window);
        let (row, col, ..) = review
            .app
            .visible_session_rect(review.window, review.terminal)
            .expect("the terminal sibling is visible");
        let (x, y) = review
            .app
            .cell_centre_px(review.window, row + 3, col + 4)
            .unwrap();
        review.app.on_cursor_moved(review.window, x, y);
        click(&mut review, button);
        assert_eq!(
            drain(&mut terminal),
            format!("\x1b[<{report_code};5;4M\x1b[<{report_code};5;4m"),
            "the terminal receives its own press and release at its local cell"
        );
        assert_eq!(
            review.app.active_native_view(review.window).unwrap().1,
            review.page,
            "a non-left terminal press preserves the review's keyboard focus"
        );
        assert!(review.request.pending());
        advance_visible_review(&mut review);
        accept_automatic_review(&mut review);
    }

    #[test]
    fn startup_review_split_press_right_on_unfocused_page_stops_apply() {
        press_on_unfocused_review_stops_apply(MouseButton::Right);
    }

    #[test]
    fn startup_review_split_press_middle_on_unfocused_page_stops_apply() {
        press_on_unfocused_review_stops_apply(MouseButton::Middle);
    }

    #[test]
    fn startup_review_split_press_right_on_terminal_preserves_review_and_report() {
        press_on_terminal_preserves_review(MouseButton::Right, 2);
    }

    #[test]
    fn startup_review_split_press_middle_on_terminal_preserves_review_and_report() {
        press_on_terminal_preserves_review(MouseButton::Middle, 1);
    }

    #[test]
    fn startup_review_split_press_left_body_miss_owns_the_wheel_over_an_older_terminal_right() {
        let mut review = review_beside_terminal();
        let mut terminal = observe_terminal(&mut review);
        assert!(
            review
                .app
                .focus_pane_in(review.window, crate::pane::FocusDir::Left)
        );
        present_mixed(&mut review.app, review.window);
        let (row, col, ..) = review
            .app
            .visible_session_rect(review.window, review.terminal)
            .unwrap();
        let (x, y) = review
            .app
            .cell_centre_px(review.window, row + 3, col + 4)
            .unwrap();
        review.app.on_cursor_moved(review.window, x, y);
        review
            .app
            .mouse_input_then_fold(review.window, ElementState::Pressed, MouseButton::Right);
        assert_eq!(drain(&mut terminal), "\x1b[<2;5;4M");

        point_at(&mut review.app, review.window, review.page);
        let _ = drain(&mut terminal); // The older right drag is still terminal-owned.
        let before = page_scroll(&review.app, review.page);
        review
            .app
            .mouse_input_then_fold(review.window, ElementState::Pressed, MouseButton::Left);
        assert!(
            review
                .app
                .native_runtime
                .view_state(review.page)
                .unwrap()
                .common()
                .pressed
                .is_none(),
            "the left press reached an empty part of the page, not a control"
        );
        review
            .app
            .on_mouse_wheel(review.window, MouseScrollDelta::LineDelta(0.0, -1.0));
        assert!(
            page_scroll(&review.app, review.page) > before,
            "the page's lower left button owns this wheel despite the older terminal right"
        );
        assert_eq!(
            drain(&mut terminal),
            "",
            "the terminal receives neither the page's left press nor its wheel"
        );
        review
            .app
            .mouse_input_then_fold(review.window, ElementState::Released, MouseButton::Left);
        assert_eq!(drain(&mut terminal), "", "the left release is the page's");
        review
            .app
            .mouse_input_then_fold(review.window, ElementState::Released, MouseButton::Right);
        let release = drain(&mut terminal);
        assert!(
            release.starts_with("\x1b[<2;") && release.ends_with('m'),
            "the terminal retains its own right release: {release:?}"
        );
        let window = &review.app.windows[&review.window];
        assert_eq!((window.page_buttons, window.reported_buttons), (0, 0));
        assert!(window.held_mouse_button.is_none());
        assert_eq!(
            review.app.active_native_view(review.window).unwrap().1,
            review.page
        );
        advance_visible_review(&mut review);
        assert!(review.request.pending(), "the page body miss stopped Apply");
        assert!(review.answer.try_recv().is_err());
        assert!(review.app.structural_invariants_ok());
    }

    #[test]
    fn startup_review_split_press_on_subtab_header_only_focuses_the_review() {
        let mut review = review_beside_terminal();
        let mut terminal = observe_terminal(&mut review);
        let header = review
            .app
            .active_visible_leaf_plan(review.window)
            .unwrap()
            .leaf(review.page)
            .unwrap()
            .header
            .expect("the split page has a host-owned title row");
        let (x, y) = review
            .app
            .cell_centre_px(
                review.window,
                header.origin.y as u16,
                header.origin.x as u16 + 4,
            )
            .unwrap();
        review.app.on_cursor_moved(review.window, x, y);
        click(&mut review, MouseButton::Left);
        assert_eq!(drain(&mut terminal), "");
        assert_eq!(
            review.app.active_native_view(review.window).unwrap().1,
            review.page,
            "the host-owned header focuses the canonical leaf"
        );
        assert!(review.request.pending());
        advance_visible_review(&mut review);
        accept_automatic_review(&mut review);
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a pointer report goes, read off each program's own wire: the pane
//! under the pointer for a gesture that begins nothing, the session a press
//! went to for its drag and release, one report per position, and the lowest
//! held button of a chord (bug hunt 8, the mouse lane).

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_session::sink::SinkWriter;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta};

use super::PointerRoute;
use crate::input::{InputEvent, PasteFraming, PixelOffset, Source};
use crate::{App, WindowId, pane, term_lock};

/// A private observer of one session's PTY input: the read end, and a sink
/// writing into the other end.
fn observer() -> (UnixStream, Arc<SinkWriter>) {
    let (reader, writer) = UnixStream::pair().expect("private input observer");
    reader.set_nonblocking(true).unwrap();
    (reader, Arc::new(SinkWriter::new_owned(writer.into())))
}

/// Everything written to `reader` so far, as text (the reports are SGR).
fn drain(reader: &mut UnixStream) -> String {
    let mut out = Vec::new();
    let mut bytes = [0u8; 256];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&bytes[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => panic!("reading input: {e}"),
        }
    }
    String::from_utf8(out).expect("SGR reports are ASCII")
}

/// Everything `session`'s program was sent before a marker pasted to it now.
/// A paste reaches the PTY through that session's one ordered writer, so the
/// marker lands after any paste already queued there: a paste that went to
/// this program is in the text, and one that did not cannot arrive later.
fn written_before_marker(
    app: &mut App,
    wid: WindowId,
    session: u64,
    wire: &mut UnixStream,
) -> String {
    const MARK: &str = "<marker>";
    app.input_to_session(
        wid,
        InputEvent::Paste(MARK.to_owned(), PasteFraming::Gesture { bracketed: false }),
        Source::Controller,
        Some(session),
        crate::app_input::PressPhase::Initial,
    );
    // A hang detector, not a latency budget.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = String::new();
    loop {
        seen += &drain(wire);
        if let Some(before) = seen.strip_suffix(MARK) {
            return before.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "the marker never reached session {session}: {seen:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn arm(app: &App, session: u64, modes: &[u8]) {
    let term = app.pool.get(session).expect("live session").term.clone();
    term_lock(&term).process(modes);
}

/// The pointer at window cell `(row, col)`, `dx` pixels right of its centre.
fn move_to(app: &mut App, wid: WindowId, (row, col): (u16, u16), dx: f64) {
    let (x, y) = app.cell_centre_px(wid, row, col).expect("cell on the grid");
    app.on_cursor_moved(wid, x + dx, y);
}

fn click(app: &mut App, wid: WindowId, button: MouseButton) {
    app.on_mouse_input(wid, ElementState::Pressed, button);
    app.on_mouse_input(wid, ElementState::Released, button);
}

/// Two stacked panes, each writing to its own observer: `top` (session 0) and
/// the focused `bottom`, both tracking any motion in SGR (`?1003h?1006h`).
struct Stacked {
    app: App,
    wid: WindowId,
    top: u64,
    bottom: u64,
    top_wire: UnixStream,
    bottom_wire: UnixStream,
}

fn stacked() -> Stacked {
    let (mut top_wire, top_sink) = observer();
    let (mut bottom_wire, bottom_sink) = observer();
    let mut app = App::headless_for_test_with_sink(top_sink);
    let wid = WindowId(0);
    let top = app.focused_session_id(wid).expect("top terminal");
    let bottom =
        app.split_active_stub_tab_dir_with_sink(wid, pane::SplitDir::Horizontal, bottom_sink);
    assert_eq!(app.focused_session_id(wid), Some(bottom));
    let (top_row, ..) = app.visible_session_rect(wid, top).expect("top pane");
    let (bottom_row, ..) = app.visible_session_rect(wid, bottom).expect("bottom pane");
    assert!(top_row < bottom_row, "the fixture stacks the panes");
    for session in [top, bottom] {
        arm(&app, session, b"\x1b[?1003h\x1b[?1006h");
    }
    let _ = drain(&mut top_wire);
    let _ = drain(&mut bottom_wire);
    Stacked {
        app,
        wid,
        top,
        bottom,
        top_wire,
        bottom_wire,
    }
}

/// The window cell of `session`'s own cell `(row, col)`.
fn window_cell(app: &App, wid: WindowId, session: u64, (row, col): (u16, u16)) -> (u16, u16) {
    let (ro, co, ..) = app
        .visible_session_rect(wid, session)
        .expect("visible pane");
    (ro + row, co + col)
}

/// Hover, the wheel and the right, middle and thumb buttons over the pane that
/// does NOT have focus reach THAT pane's program at its own cell, and the
/// focused pane's program sees nothing: before, all of it went to the focused
/// pane at a row clamped onto its top edge (`ESC[<35;6;1M`).
#[test]
fn a_pointer_over_an_unfocused_pane_reaches_that_panes_program_at_its_own_cell() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    let over_top = window_cell(app, wid, f.top, (4, 5));

    move_to(app, wid, over_top, 0.0);
    assert_eq!(drain(&mut f.top_wire), "\x1b[<35;6;5M", "hover");
    assert_eq!(drain(&mut f.bottom_wire), "");

    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    assert_eq!(drain(&mut f.top_wire), "\x1b[<64;6;5M", "wheel");
    assert_eq!(drain(&mut f.bottom_wire), "");

    click(app, wid, MouseButton::Right);
    assert_eq!(
        drain(&mut f.top_wire),
        "\x1b[<2;6;5M\x1b[<2;6;5m",
        "right button"
    );
    click(app, wid, MouseButton::Middle);
    assert_eq!(
        drain(&mut f.top_wire),
        "\x1b[<1;6;5M\x1b[<1;6;5m",
        "middle button"
    );
    click(app, wid, MouseButton::Back);
    assert_eq!(
        drain(&mut f.top_wire),
        "\x1b[<128;6;5M\x1b[<128;6;5m",
        "thumb button"
    );
    assert_eq!(drain(&mut f.bottom_wire), "");
    assert_eq!(
        app.focused_session_id(wid),
        Some(f.bottom),
        "only a left press moves focus"
    );
    assert_eq!(app.windows[&wid].reported_buttons, 0);

    // Negative control: over the focused pane the focused pane's program is
    // the one addressed, exactly as before.
    let over_bottom = window_cell(app, wid, f.bottom, (2, 3));
    move_to(app, wid, over_bottom, 0.0);
    click(app, wid, MouseButton::Right);
    assert_eq!(
        drain(&mut f.bottom_wire),
        "\x1b[<35;4;3M\x1b[<2;4;3M\x1b[<2;4;3m"
    );
    assert_eq!(drain(&mut f.top_wire), "");
}

/// Side by side, the unfocused RIGHT pane's report is at its own column: the
/// window cell less the pane's column origin as well as its row origin (the
/// stacked fixture has no column offset to get wrong).
#[test]
fn a_pointer_over_an_unfocused_pane_beside_the_focused_one_reports_its_own_column() {
    let (mut left_wire, left_sink) = observer();
    let (mut right_wire, right_sink) = observer();
    let mut app = App::headless_for_test_with_sink(left_sink);
    let wid = WindowId(0);
    let left = app.focused_session_id(wid).expect("left terminal");
    let right = app.split_active_stub_tab_dir_with_sink(wid, pane::SplitDir::Vertical, right_sink);
    let (_, left_col, ..) = app.visible_session_rect(wid, left).expect("left pane");
    let (_, right_col, ..) = app.visible_session_rect(wid, right).expect("right pane");
    assert!(left_col < right_col, "fixture: the panes are side by side");
    assert!(app.focus_pane_in(wid, pane::FocusDir::Left));
    assert_eq!(app.focused_session_id(wid), Some(left));
    for session in [left, right] {
        arm(&app, session, b"\x1b[?1003h\x1b[?1006h");
    }
    let _ = drain(&mut left_wire);
    let _ = drain(&mut right_wire);

    let over_right = window_cell(&app, wid, right, (4, 5));
    move_to(&mut app, wid, over_right, 0.0);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    assert_eq!(drain(&mut right_wire), "\x1b[<35;6;5M\x1b[<64;6;5M");
    assert_eq!(drain(&mut left_wire), "");
}

/// With no program tracking the mouse, the wheel over the unfocused pane scrolls
/// THAT pane's history, never the focused pane's.
#[test]
fn a_wheel_over_an_unfocused_pane_scrolls_that_panes_history() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    for session in [f.top, f.bottom] {
        let term = app.pool.get(session).unwrap().term.clone();
        let mut t = term_lock(&term);
        t.process(b"\x1b[?1003l");
        for line in 0..100 {
            t.process(format!("pane {session} line {line}\r\n").as_bytes());
        }
    }
    let over_top = window_cell(app, wid, f.top, (4, 5));
    move_to(app, wid, over_top, 0.0);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
    let top_term = app.pool.get(f.top).unwrap().term.clone();
    let bottom_term = app.pool.get(f.bottom).unwrap().term.clone();
    let glide_on = |term: &Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>| {
        app.windows[&wid]
            .scroll_glide
            .as_ref()
            .is_some_and(|glide| Arc::ptr_eq(&glide.term, term))
    };
    assert!(
        glide_on(&top_term) || term_lock(&top_term).grid().display_offset() > 0,
        "the top pane's history moves"
    );
    assert!(!glide_on(&bottom_term));
    assert_eq!(term_lock(&bottom_term).grid().display_offset(), 0);
    assert_eq!(drain(&mut f.top_wire), "");
    assert_eq!(drain(&mut f.bottom_wire), "");
}

/// A glide the wheel started on the unfocused pane survives what the FOCUSED
/// pane does next: a key typed there (its press runs
/// `drop_front_scroll_motion`) and its snap to live. Before, the window's one
/// glide was dropped whatever pane it scrolled, and the notch not yet eased in
/// was lost.
#[test]
fn a_glide_on_the_unfocused_pane_survives_typing_in_the_focused_one() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    for session in [f.top, f.bottom] {
        let term = app.pool.get(session).unwrap().term.clone();
        let mut t = term_lock(&term);
        t.process(b"\x1b[?1003l");
        for line in 0..100 {
            t.process(format!("pane {session} line {line}\r\n").as_bytes());
        }
    }
    let over_top = window_cell(app, wid, f.top, (4, 5));
    move_to(app, wid, over_top, 0.0);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
    let top_term = app.pool.get(f.top).unwrap().term.clone();
    let glide_on_top = |app: &App| {
        app.windows[&wid]
            .scroll_glide
            .as_ref()
            .is_some_and(|glide| Arc::ptr_eq(&glide.term, &top_term))
    };
    assert!(
        glide_on_top(app),
        "fixture: the wheel armed a glide on the top pane"
    );
    app.drop_front_scroll_motion(wid);
    assert!(glide_on_top(app), "a key in the bottom pane keeps it");
    app.snap_to_bottom(wid);
    assert!(glide_on_top(app), "the bottom pane's snap to live keeps it");
    // Control: the front pane's own glide is still dropped.
    let bottom_term = app.pool.get(f.bottom).unwrap().term.clone();
    let over_bottom = window_cell(app, wid, f.bottom, (4, 5));
    move_to(app, wid, over_bottom, 0.0);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
    assert!(
        app.windows[&wid]
            .scroll_glide
            .as_ref()
            .is_some_and(|glide| Arc::ptr_eq(&glide.term, &bottom_term)),
        "fixture: a glide on the focused pane"
    );
    app.drop_front_scroll_motion(wid);
    assert!(
        app.windows[&wid].scroll_glide.is_none(),
        "the front's glide is dropped"
    );
}

/// Over a native pane no terminal program is under the pointer: the event is no
/// one's, never the focused terminal's at a clamped cell.
#[test]
fn a_pointer_over_a_native_sibling_addresses_no_terminal() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native) = app.active_native_view(wid).expect("native front");
    let (terminal, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert_eq!(app.focused_session_id(wid), Some(terminal));
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let rect = plan.leaf(native).expect("native leaf").rect;
    let cell = (rect.origin.y as u16 + 2, rect.origin.x as u16 + 2);
    move_to(&mut app, wid, cell, 0.0);
    assert_eq!(app.pointer_route(wid, None), PointerRoute::Nowhere);
    // …and over the focused terminal it is the front, as always.
    let (ro, co, ..) = app.focused_pane_rect(wid);
    move_to(&mut app, wid, (ro + 1, co + 1), 0.0);
    assert_eq!(app.pointer_route(wid, None), PointerRoute::Front);
}

/// A press's drag and release follow the session it went to when the keyboard
/// moves focus to the other pane mid-drag: before, both went to the newly
/// focused pane, which never saw the press.
#[test]
fn a_held_buttons_drag_and_release_stay_with_its_pane_across_a_focus_move() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    for session in [f.top, f.bottom] {
        arm(app, session, b"\x1b[?1003l\x1b[?1002h");
    }
    let at = window_cell(app, wid, f.bottom, (2, 3));
    move_to(app, wid, at, 0.0);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert_eq!(drain(&mut f.bottom_wire), "\x1b[<0;4;3M");
    assert!(app.focus_pane_in(wid, pane::FocusDir::Up));
    assert_eq!(app.focused_session_id(wid), Some(f.top));
    let to = window_cell(app, wid, f.bottom, (3, 6));
    move_to(app, wid, to, 0.0);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_eq!(drain(&mut f.bottom_wire), "\x1b[<32;7;4M\x1b[<0;7;4m");
    assert_eq!(drain(&mut f.top_wire), "", "the new front saw no half pair");
    assert_eq!(app.windows[&wid].reported_buttons, 0);
}

/// A tab switch with a reported button held writes that button's release to
/// the program that got the press, at the cell it last saw, and the new tab's
/// program never gets the drag or the release.
#[test]
fn a_tab_switch_releases_a_held_button_to_the_tab_it_leaves() {
    let (mut wire_a, sink_a) = observer();
    let (mut wire_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session_with_sink(1, sink_b));
    app.switch_tab_in(wid, 0);
    for session in [0, 1] {
        arm(&app, session, b"\x1b[?1002h\x1b[?1006h");
    }
    let _ = drain(&mut wire_a);
    let _ = drain(&mut wire_b);

    move_to(&mut app, wid, (4, 7), 0.0);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert_eq!(drain(&mut wire_a), "\x1b[<0;8;5M");
    app.switch_tab_in(wid, 1);
    assert_eq!(
        drain(&mut wire_a),
        "\x1b[<0;8;5m",
        "the tab left is told the button went up"
    );
    move_to(&mut app, wid, (6, 9), 0.0);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_eq!(
        drain(&mut wire_b),
        "",
        "no drag or release it never pressed"
    );
    assert_eq!(drain(&mut wire_a), "", "and no second release");
    let ws = &app.windows[&wid];
    assert_eq!((ws.reported_buttons, ws.held_mouse_button), (0, None));
}

/// A two-button chord under DEC 1002 names the LOWEST held button in its motion
/// (xterm's `FirstBitN`), and releasing one button keeps the other's drag
/// reported: before, motion named the last press, and the first release made
/// every later motion buttonless, which 1002 drops.
#[test]
fn a_chords_motion_names_the_lowest_held_button_until_its_release() {
    let (mut wire, sink) = observer();
    let mut app = App::headless_for_test_with_sink(sink);
    let wid = WindowId(0);
    arm(&app, 0, b"\x1b[?1002h\x1b[?1006h");
    let _ = drain(&mut wire);
    move_to(&mut app, wid, (4, 7), 0.0);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    assert_eq!(drain(&mut wire), "\x1b[<0;8;5M\x1b[<2;8;5M");
    move_to(&mut app, wid, (4, 9), 0.0);
    assert_eq!(drain(&mut wire), "\x1b[<32;10;5M", "both held: Left");
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    assert_eq!(drain(&mut wire), "\x1b[<2;10;5m");
    move_to(&mut app, wid, (4, 12), 0.0);
    move_to(&mut app, wid, (5, 14), 0.0);
    assert_eq!(
        drain(&mut wire),
        "\x1b[<32;13;5M\x1b[<32;15;6M",
        "Left is still held"
    );
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_eq!(drain(&mut wire), "\x1b[<0;15;6m");
    assert_eq!(app.windows[&wid].held_mouse_button, None);
}

/// Motion is reported once per position (xterm: "only if the mouse pointer has
/// moved to a different character cell"): pixel moves inside one cell repeat
/// nothing, a drag that never leaves the pressed cell reports no motion, and
/// under DEC 1016 every new pixel IS a new position.
#[test]
fn motion_reports_once_per_position() {
    let (mut wire, sink) = observer();
    let mut app = App::headless_for_test_with_sink(sink);
    let wid = WindowId(0);
    arm(&app, 0, b"\x1b[?1003h\x1b[?1006h");
    let _ = drain(&mut wire);
    for dx in [0.0, 1.0, 2.0, -1.0] {
        move_to(&mut app, wid, (4, 7), dx);
    }
    assert_eq!(
        drain(&mut wire),
        "\x1b[<35;8;5M",
        "one hover report per cell"
    );
    move_to(&mut app, wid, (4, 8), 0.0);
    assert_eq!(drain(&mut wire), "\x1b[<35;9;5M", "a new cell reports");

    // A 1002 drag inside the pressed cell: the press named the position.
    arm(&app, 0, b"\x1b[?1003l\x1b[?1002h");
    move_to(&mut app, wid, (6, 2), 0.0);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    for dx in [1.0, 2.0, 3.0] {
        move_to(&mut app, wid, (6, 2), dx);
    }
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert_eq!(drain(&mut wire), "\x1b[<0;3;7M\x1b[<0;3;7m");

    // SGR-pixel: a sub-cell move changes the coordinate the report carries.
    arm(&app, 0, b"\x1b[?1002l\x1b[?1003h\x1b[?1016h");
    move_to(&mut app, wid, (4, 7), 0.0);
    let first = drain(&mut wire);
    assert!(first.starts_with("\x1b[<35;"), "{first:?}");
    move_to(&mut app, wid, (4, 7), 1.0);
    let second = drain(&mut wire);
    assert!(
        second.starts_with("\x1b[<35;") && second != first,
        "{first:?} then {second:?}"
    );
}

/// A right-click copy/paste over the UNFOCUSED pane is decided by that pane's
/// program and acts on that pane: the clipboard is pasted into it (focused
/// first, as Windows Terminal and kitty focus on any press), its selection is
/// what is copied, and the focused pane's mouse-tracking program gets neither
/// a paste nor a report. Before, the decision read the pane under the pointer
/// and the paste went to the focused pane.
#[test]
fn a_right_click_copy_or_paste_acts_on_the_pane_under_the_pointer() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    app.config.right_click = Some("copy_paste".to_owned());
    arm(app, f.top, b"\x1b[?1003l");
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("PROBE-CLIP".into()));
    let over_top = window_cell(app, wid, f.top, (4, 5));
    move_to(app, wid, over_top, 0.0);
    click(app, wid, MouseButton::Right);
    assert_eq!(app.focused_session_id(wid), Some(f.top), "the paste's pane");
    assert_eq!(
        written_before_marker(app, wid, f.top, &mut f.top_wire),
        "PROBE-CLIP"
    );
    assert_eq!(
        written_before_marker(app, wid, f.bottom, &mut f.bottom_wire),
        "",
        "the tracking program beside it gets no paste and no report"
    );

    // Copy: the selection under the pointer is the one taken.
    assert!(app.focus_pane_in(wid, pane::FocusDir::Down));
    let top_term = app.pool.get(f.top).unwrap().term.clone();
    {
        let mut t = term_lock(&top_term);
        t.process(b"\x1b[Hhello world");
        let side = aterm_core::selection::SelectionSide::Left;
        let selection = t.text_selection_mut();
        selection.start_selection(0, 0, side, aterm_core::selection::SelectionType::Simple);
        selection.update_selection(0, 4, aterm_core::selection::SelectionSide::Right);
        selection.complete_selection();
        assert_eq!(t.selection_to_string().as_deref(), Some("hello"));
    }
    move_to(app, wid, over_top, 0.0);
    click(app, wid, MouseButton::Right);
    let copied = crate::control::PBPASTE_STUB.with(|board| board.borrow().clone());
    assert_eq!(copied.as_deref(), Some("hello"));
    assert_eq!(
        term_lock(&top_term)
            .selection_to_string()
            .filter(|text| !text.is_empty()),
        None,
        "the copy takes the selection"
    );
    assert_eq!(written_before_marker(app, wid, f.top, &mut f.top_wire), "");
    assert_eq!(
        written_before_marker(app, wid, f.bottom, &mut f.bottom_wire),
        ""
    );
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
}

/// Over a native pane no terminal is under the pointer, so a right-click
/// copy/paste pastes into none: before, it pasted into the focused terminal.
#[test]
fn a_right_click_over_a_native_pane_pastes_into_no_terminal() {
    let (mut wire, sink) = observer();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.right_click = Some("copy_paste".to_owned());
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native) = app.active_native_view(wid).expect("native front");
    let (terminal, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    app.pool
        .insert(crate::stub_session_with_sink(terminal, sink));
    app.resync_active_or_window(wid);
    assert_eq!(app.focused_session_id(wid), Some(terminal));
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("PROBE-CLIP".into()));
    let rect = app
        .active_visible_leaf_plan(wid)
        .unwrap()
        .leaf(native)
        .expect("native leaf")
        .rect;
    move_to(
        &mut app,
        wid,
        (rect.origin.y as u16 + 2, rect.origin.x as u16 + 2),
        0.0,
    );
    click(&mut app, wid, MouseButton::Right);
    assert_eq!(app.focused_session_id(wid), Some(terminal));
    assert_eq!(
        written_before_marker(&mut app, wid, terminal, &mut wire),
        ""
    );
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
}

/// A gesture held over the UNFOCUSED pane stays that pane's: the second button
/// of its chord, a wheel notch during it and its drag all reach the program
/// that got the first press. Before, a held button routed everything to the
/// focused pane at a cell clamped onto its top edge.
#[test]
fn a_gesture_held_over_an_unfocused_pane_keeps_its_chord_wheel_and_drag() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    for session in [f.top, f.bottom] {
        arm(app, session, b"\x1b[?1003l\x1b[?1002h");
    }
    move_to(app, wid, window_cell(app, wid, f.top, (4, 5)), 0.0);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Middle);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    move_to(app, wid, window_cell(app, wid, f.top, (4, 8)), 0.0);
    // Into the focused pane: the drag still addresses the pane it began in,
    // at its edge.
    move_to(app, wid, window_cell(app, wid, f.bottom, (2, 8)), 0.0);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Middle);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    // The top pane's last row, 1-based: where its clamped drag stays.
    let (_, _, edge, _) = app.visible_session_rect(wid, f.top).unwrap();
    assert_eq!(
        drain(&mut f.top_wire),
        format!(
            "\x1b[<2;6;5M\x1b[<1;6;5M\x1b[<64;6;5M\x1b[<33;9;5M\x1b[<33;9;{edge}M\
             \x1b[<1;9;{edge}m\x1b[<2;9;{edge}m"
        )
    );
    assert_eq!(drain(&mut f.bottom_wire), "");
    assert_eq!(app.focused_session_id(wid), Some(f.bottom));
    assert_eq!(app.windows[&wid].reported_buttons, 0);
}

/// A release after the keyboard moved focus INTO the pane that has the press,
/// with the pointer still, lands on the cell the pointer is on, and so does a
/// wheel notch after it: before, both used a cell clamped into the pane focus
/// had left (`ESC[<2;6;1m`).
#[test]
fn a_release_after_a_keyboard_focus_move_lands_on_the_pointers_cell() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    for session in [f.top, f.bottom] {
        arm(app, session, b"\x1b[?1003l\x1b[?1002h");
    }
    move_to(app, wid, window_cell(app, wid, f.top, (4, 5)), 0.0);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    assert!(app.focus_pane_in(wid, pane::FocusDir::Up));
    assert_eq!(app.focused_session_id(wid), Some(f.top));
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    assert_eq!(
        drain(&mut f.top_wire),
        "\x1b[<2;6;5M\x1b[<2;6;5m\x1b[<64;6;5M"
    );
    assert_eq!(drain(&mut f.bottom_wire), "");
}

/// The release a tab switch writes for a held button names the last cell its
/// drag reported, not the cell it was pressed at.
#[test]
fn a_tab_switch_releases_at_the_last_cell_the_drag_reached() {
    let (mut wire_a, sink_a) = observer();
    let (mut wire_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session_with_sink(1, sink_b));
    app.switch_tab_in(wid, 0);
    for session in [0, 1] {
        arm(&app, session, b"\x1b[?1002h\x1b[?1006h");
    }
    let _ = drain(&mut wire_a);
    let _ = drain(&mut wire_b);
    move_to(&mut app, wid, (4, 7), 0.0);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Middle);
    move_to(&mut app, wid, (5, 9), 0.0);
    app.switch_tab_in(wid, 1);
    assert_eq!(
        drain(&mut wire_a),
        "\x1b[<1;8;5M\x1b[<33;10;6M\x1b[<1;10;6m"
    );
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Middle);
    assert_eq!(drain(&mut wire_b), "");
}

/// A trackpad's sub-row scroll over the UNFOCUSED pane moves that pane's
/// history band, as its whole rows do.
#[test]
fn a_trackpad_band_over_an_unfocused_pane_moves_that_panes_history() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    for session in [f.top, f.bottom] {
        let term = app.pool.get(session).unwrap().term.clone();
        let mut t = term_lock(&term);
        t.process(b"\x1b[?1003l");
        for line in 0..100 {
            t.process(format!("pane {session} line {line}\r\n").as_bytes());
        }
    }
    move_to(app, wid, window_cell(app, wid, f.top, (4, 5)), 0.0);
    let cell_h = app.win_cell_size(wid).1 as f64;
    app.on_mouse_wheel(
        wid,
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, cell_h / 3.0)),
    );
    let top_term = app.pool.get(f.top).unwrap().term.clone();
    let glide = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .expect("a sub-row precise delta arms the band");
    assert!(Arc::ptr_eq(&glide.term, &top_term), "on the top pane");
}

/// A wheel notch on the next pane lands the glide it interrupts on the other
/// pane at that glide's own target, instead of leaving that pane stopped
/// part-way: the window holds one glide.
#[test]
fn a_wheel_on_the_next_pane_lands_the_glide_it_interrupts() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    for session in [f.top, f.bottom] {
        let term = app.pool.get(session).unwrap().term.clone();
        let mut t = term_lock(&term);
        t.process(b"\x1b[?1003l");
        for line in 0..100 {
            t.process(format!("pane {session} line {line}\r\n").as_bytes());
        }
    }
    let top_term = app.pool.get(f.top).unwrap().term.clone();
    let bottom_term = app.pool.get(f.bottom).unwrap().term.clone();
    move_to(app, wid, window_cell(app, wid, f.top, (4, 5)), 0.0);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    let target_rows = {
        let glide = app.windows[&wid].scroll_glide.as_ref().expect("a glide");
        assert!(Arc::ptr_eq(&glide.term, &top_term));
        glide.glide.target_px() / glide.cell_h
    };
    assert!(target_rows > 0);
    assert_eq!(
        term_lock(&top_term).grid().display_offset(),
        0,
        "no tick has run: the glide is still on its way"
    );
    move_to(app, wid, window_cell(app, wid, f.bottom, (2, 3)), 0.0);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    let glide = app.windows[&wid].scroll_glide.as_ref().expect("a glide");
    assert!(Arc::ptr_eq(&glide.term, &bottom_term));
    assert_eq!(
        i64::try_from(term_lock(&top_term).grid().display_offset()).unwrap(),
        target_rows,
        "the top pane landed where its glide was going"
    );
}

/// A release that goes to a pane BEHIND the front (its press's owner, after a
/// keyboard focus move), from a program that has since stopped tracking the
/// mouse, runs no local gesture: it leaves the front's pointer cell alone, so
/// the next wheel notch over the front reports where the pointer is.
#[test]
fn a_release_to_a_pane_behind_the_front_leaves_the_fronts_pointer_cell() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    for session in [f.top, f.bottom] {
        arm(app, session, b"\x1b[?1003l\x1b[?1000h");
    }
    move_to(app, wid, window_cell(app, wid, f.bottom, (2, 3)), 0.0);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    assert_eq!(drain(&mut f.bottom_wire), "\x1b[<0;4;3M");
    assert!(app.focus_pane_in(wid, pane::FocusDir::Up));
    move_to(app, wid, window_cell(app, wid, f.top, (4, 5)), 0.0);
    arm(app, f.bottom, b"\x1b[?1000l");
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 1.0));
    assert_eq!(drain(&mut f.top_wire), "\x1b[<64;6;5M");
    assert_eq!(drain(&mut f.bottom_wire), "");
}

/// A wheel report names a position like a press does (xterm buttons 4-7), so
/// motion onto that same cell repeats nothing.
#[test]
fn a_wheel_report_is_a_position_for_the_next_motion() {
    let (mut wire, sink) = observer();
    let mut app = App::headless_for_test_with_sink(sink);
    let wid = WindowId(0);
    arm(&app, 0, b"\x1b[?1003h\x1b[?1006h");
    let _ = drain(&mut wire);
    let wheel = InputEvent::Wheel {
        dir: aterm_types::mouse::WheelDir::Up,
        lines: 1,
        row: 4,
        col: 7,
        mods: 0,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let hover = InputEvent::MouseMove {
        buttons: 3,
        row: 4,
        col: 7,
        mods: 0,
        side: aterm_core::selection::SelectionSide::Left,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    for ev in [wheel, hover] {
        let _ = app.input(wid, ev, Source::Controller);
    }
    assert_eq!(drain(&mut wire), "\x1b[<64;8;5M");
}

/// A hover report to the UNFOCUSED pane's program leaves the focused pane's
/// typed-move licence alive (that program's bytes cannot move the front's
/// caret); a report to the focused pane's own program still retires it.
#[test]
fn a_hover_report_to_a_sibling_keeps_the_fronts_typed_move_licence() {
    let mut f = stacked();
    let (app, wid) = (&mut f.app, f.wid);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_owned());
    app.recompute_sparkle();
    let geom = crate::cursor_glow::Geom {
        cw: 8,
        ch: 16,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 384,
        head: 0,
    };
    let cfg = app.glow_config();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .tick(Some((0, 0)), Instant::now(), &cfg, geom, &mut Vec::new());
    let licensed = |app: &App| {
        app.windows[&wid]
            .fx
            .cursor()
            .glow
            .move_licensed(Instant::now())
    };
    let _ = app.input(
        wid,
        InputEvent::Key {
            key: aterm_types::keyboard::Key::Character('a'),
            mods: aterm_types::keyboard::Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        },
        Source::Human,
    );
    assert_eq!(drain(&mut f.bottom_wire), "a");
    assert!(licensed(app), "the typed key licenses the front's move");
    move_to(app, wid, window_cell(app, wid, f.top, (4, 5)), 0.0);
    assert_eq!(drain(&mut f.top_wire), "\x1b[<35;6;5M");
    assert!(licensed(app), "a sibling's report is not the front's");
    move_to(app, wid, window_cell(app, wid, f.bottom, (2, 3)), 0.0);
    assert_eq!(drain(&mut f.bottom_wire), "\x1b[<35;4;3M");
    assert!(!licensed(app), "the front's own report retires it");
}

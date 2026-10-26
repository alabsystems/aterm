// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::time::Instant;

use aterm_session::sink::SinkWriter;

use super::{App, WindowId, pane, stub_session_with_sink, term_lock};

pub(crate) const FOCUS_IN: &[u8] = b"\x1b[I";
pub(crate) const FOCUS_OUT: &[u8] = b"\x1b[O";

/// A session sink whose writes land in the returned pipe's non-blocking read end.
pub(crate) fn observer() -> ([i32; 2], Arc<SinkWriter>) {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    (pipe, Arc::new(SinkWriter::new(pipe[1])))
}

/// What the program behind `pipe`'s sink has been written since the last drain.
pub(crate) fn drain(pipe: [i32; 2]) -> Vec<u8> {
    let mut bytes = [0u8; 64];
    let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
    if read <= 0 {
        return Vec::new();
    }
    bytes[..read as usize].to_vec()
}

fn arm_focus_reporting(app: &App, session: u64) {
    let term = app.pool.get(session).expect("live session").term.clone();
    term_lock(&term).process(b"\x1b[?1004h");
}

fn feed(app: &App, session: u64, bytes: &[u8]) {
    let term = app.pool.get(session).expect("live session").term.clone();
    term_lock(&term).process(bytes);
}

fn program_writes(app: &mut App, session: u64, bytes: &[u8]) {
    feed(app, session, bytes);
    app.on_output_wake(session, Instant::now());
}

#[test]
fn a_background_tab_that_enables_focus_reporting_is_told_it_is_unfocused() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session_with_sink(1, sink_b));
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);
    assert_eq!(
        app.focus_reported,
        Some(1),
        "tab 1 is in front, tab 0 behind"
    );

    program_writes(&mut app, 0, b"\x1b[?1004h");
    assert_eq!(
        drain(pipe_a),
        FOCUS_OUT.to_vec(),
        "the background program learns it is not focused"
    );
    assert!(drain(pipe_b).is_empty(), "the front tab is not written");

    program_writes(&mut app, 0, b"\x1b[?1004h");
    program_writes(&mut app, 0, b"frame");
    app.sync_window(wid);
    assert!(
        drain(pipe_a).is_empty(),
        "an enable already answered, a re-sent enable and plain output owe nothing"
    );

    program_writes(&mut app, 0, b"\x1b[?1004l\x1b[?1004h");
    assert_eq!(
        drain(pipe_a),
        FOCUS_OUT.to_vec(),
        "the second enable is answered"
    );

    app.switch_tab_in(wid, 0);
    assert_eq!(
        drain(pipe_a),
        FOCUS_IN.to_vec(),
        "brought forward: focused once"
    );
    program_writes(&mut app, 0, b"frame");
    assert!(drain(pipe_a).is_empty(), "and nothing after it");
}

#[test]
fn the_focused_session_enabling_focus_reporting_hears_nothing_until_it_loses_focus() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session_with_sink(1, sink_b));
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);

    program_writes(&mut app, 1, b"\x1b[?1004h");
    assert!(
        drain(pipe_b).is_empty(),
        "the focused program is written nothing at the enable"
    );

    feed(&app, 1, b"\x1b[?1004l\x1b[?1004h");
    app.switch_tab_in(wid, 0);
    assert_eq!(drain(pipe_b), FOCUS_OUT.to_vec(), "the switch blurs it");
    app.on_output_wake(1, Instant::now());
    assert!(
        drain(pipe_b).is_empty(),
        "the late wake finds the enable answered: one ESC[O, not two"
    );
    assert!(
        drain(pipe_a).is_empty(),
        "tab 0 never turned DEC 1004 on and is written nothing"
    );
}

#[test]
fn a_split_pane_without_the_keyboard_that_enables_focus_reporting_is_told_so() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    let right = app.split_active_stub_tab_dir_with_sink(wid, pane::SplitDir::Vertical, sink_b);
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);
    assert_eq!(
        app.focus_reported,
        Some(right),
        "the new pane has the keyboard"
    );

    program_writes(&mut app, 0, b"\x1b[?1004h");
    assert_eq!(drain(pipe_a), FOCUS_OUT.to_vec(), "the left pane is told");
    program_writes(&mut app, right, b"\x1b[?1004h");
    assert!(drain(pipe_b).is_empty(), "the focused pane is not");

    assert!(
        app.focus_pane_in(wid, pane::FocusDir::Left),
        "left neighbour"
    );
    assert_eq!(
        drain(pipe_b),
        FOCUS_OUT.to_vec(),
        "the right pane blurs once"
    );
    assert_eq!(
        drain(pipe_a),
        FOCUS_IN.to_vec(),
        "the left pane focuses once"
    );
}

#[test]
fn an_enable_while_the_window_is_away_is_answered_with_focus_out() {
    let (pipe_a, sink_a) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.on_focus(wid, false);
    assert!(drain(pipe_a).is_empty(), "nothing was armed yet");

    program_writes(&mut app, 0, b"\x1b[?1004h");
    assert_eq!(drain(pipe_a), FOCUS_OUT.to_vec(), "the window is away");

    app.on_focus(wid, true);
    assert_eq!(drain(pipe_a), FOCUS_IN.to_vec(), "the window is back");
}

#[test]
fn a_repeated_window_focus_edge_reports_nothing_twice() {
    let (pipe_a, sink_a) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    arm_focus_reporting(&app, 0);
    let _ = drain(pipe_a);

    app.on_focus(wid, true);
    assert!(drain(pipe_a).is_empty(), "already focused: nothing changed");
    app.on_focus(wid, false);
    assert_eq!(drain(pipe_a), FOCUS_OUT.to_vec());
    app.on_focus(wid, false);
    assert!(drain(pipe_a).is_empty(), "a second blur changes nothing");
    app.on_focus(wid, true);
    assert_eq!(drain(pipe_a), FOCUS_IN.to_vec());
    app.on_focus(wid, true);
    assert!(drain(pipe_a).is_empty(), "a second focus changes nothing");
}

#[test]
fn key_status_moving_between_windows_reports_each_session_once_in_either_order() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let (pipe_c, sink_c) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid_a = WindowId(0);
    app.push_stub_tab(wid_a, stub_session_with_sink(1, sink_b));
    app.push_stub_tab(wid_a, stub_session_with_sink(2, sink_c));
    for session in [0, 1, 2] {
        arm_focus_reporting(&app, session);
    }
    let quiet = |pipes: &[[i32; 2]]| {
        let held: Vec<Vec<u8>> = pipes.iter().map(|pipe| drain(*pipe)).collect();
        held.iter().all(Vec::is_empty)
    };
    let _ = quiet(&[pipe_a, pipe_b, pipe_c]);

    let wid_b = app.detach_active_tab_logical().expect("the tab tears out");
    app.window_focus_event(wid_a, false);
    app.window_focus_event(wid_b, true);
    assert!(
        quiet(&[pipe_a, pipe_b, pipe_c]),
        "session 2 kept the keyboard and A's front tab never had it: nothing to report"
    );

    app.window_focus_event(wid_a, true);
    app.window_focus_event(wid_b, false);
    assert_eq!(drain(pipe_c), FOCUS_OUT.to_vec(), "B's session blurs once");
    assert_eq!(
        drain(pipe_b),
        FOCUS_IN.to_vec(),
        "A's front session focuses once"
    );

    app.window_focus_event(wid_a, false);
    app.window_focus_event(wid_b, true);
    assert_eq!(drain(pipe_b), FOCUS_OUT.to_vec(), "A's session blurs once");
    assert_eq!(drain(pipe_c), FOCUS_IN.to_vec(), "B's session focuses once");
    assert!(
        drain(pipe_a).is_empty(),
        "A's background tab is written nothing through all of it"
    );
}

#[test]
fn switching_the_active_tab_blurs_the_tab_left_and_focuses_the_tab_arrived_at() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session_with_sink(1, sink_b));

    arm_focus_reporting(&app, 0);
    arm_focus_reporting(&app, 1);
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);

    app.switch_tab_in(wid, 0);
    assert_eq!(
        drain(pipe_b),
        FOCUS_OUT.to_vec(),
        "the tab being left must be told it lost the keyboard"
    );
    assert_eq!(
        drain(pipe_a),
        FOCUS_IN.to_vec(),
        "the tab arrived at must be told it has the keyboard"
    );

    app.cycle_tab_in(wid, true);
    assert_eq!(
        drain(pipe_a),
        FOCUS_OUT.to_vec(),
        "cycle: the old tab blurs"
    );
    assert_eq!(
        drain(pipe_b),
        FOCUS_IN.to_vec(),
        "cycle: the new tab focuses"
    );
}

#[test]
fn an_unarmed_session_and_a_switch_that_moves_nothing_both_write_no_bytes() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session_with_sink(1, sink_b));
    arm_focus_reporting(&app, 0);
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);

    app.switch_tab_in(wid, 0);
    assert_eq!(drain(pipe_a), FOCUS_IN.to_vec(), "the armed tab is told");
    assert!(
        drain(pipe_b).is_empty(),
        "a session that never set DEC 1004 is written nothing"
    );

    app.switch_tab_in(wid, 0);
    app.sync_window(wid);
    assert!(
        drain(pipe_a).is_empty(),
        "an unmoved keyboard reports nothing at all"
    );
}

#[test]
fn moving_focus_between_split_panes_reports_both_sides() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    arm_focus_reporting(&app, 0);
    let _ = drain(pipe_a);

    let right = app.split_active_stub_tab_dir_with_sink(wid, pane::SplitDir::Vertical, sink_b);
    assert_eq!(
        drain(pipe_a),
        FOCUS_OUT.to_vec(),
        "the pane the split moved focus OFF is told it lost the keyboard"
    );
    arm_focus_reporting(&app, right);
    let _ = drain(pipe_b);

    assert!(
        app.focus_pane_in(wid, pane::FocusDir::Left),
        "left neighbour"
    );
    assert_eq!(drain(pipe_b), FOCUS_OUT.to_vec(), "the right pane blurs");
    assert_eq!(drain(pipe_a), FOCUS_IN.to_vec(), "the left pane focuses");

    assert!(
        app.focus_pane_in(wid, pane::FocusDir::Right),
        "right neighbour"
    );
    assert_eq!(drain(pipe_a), FOCUS_OUT.to_vec(), "the left pane blurs");
    assert_eq!(drain(pipe_b), FOCUS_IN.to_vec(), "the right pane focuses");
}

#[test]
fn an_unfocused_windows_tab_switch_is_silent_and_regaining_focus_reports_once() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session_with_sink(1, sink_b));
    arm_focus_reporting(&app, 0);
    arm_focus_reporting(&app, 1);
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);

    app.on_focus(wid, false);
    assert_eq!(drain(pipe_b), FOCUS_OUT.to_vec(), "the front tab blurs");
    assert!(
        drain(pipe_a).is_empty(),
        "a background tab has nothing to lose"
    );

    app.switch_tab_in(wid, 0);
    assert!(
        drain(pipe_a).is_empty() && drain(pipe_b).is_empty(),
        "an unfocused window's tab switch hands no keyboard over"
    );

    app.on_focus(wid, true);
    assert_eq!(
        drain(pipe_a),
        FOCUS_IN.to_vec(),
        "the new front tab focuses"
    );
    assert!(
        drain(pipe_b).is_empty(),
        "the tab the user left is not re-blurred"
    );

    app.switch_tab_in(wid, 1);
    assert_eq!(drain(pipe_a), FOCUS_OUT.to_vec());
    assert_eq!(drain(pipe_b), FOCUS_IN.to_vec());
}

#[test]
fn only_the_front_window_ever_claims_the_keyboard() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let (pipe_c, sink_c) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid_a = WindowId(0);
    app.push_stub_tab(wid_a, stub_session_with_sink(1, sink_b));
    app.push_stub_tab(wid_a, stub_session_with_sink(2, sink_c));
    for session in [0, 1, 2] {
        arm_focus_reporting(&app, session);
    }
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);
    let _ = drain(pipe_c);

    let wid_b = app.detach_active_tab_logical().expect("the tab tears out");
    assert_ne!(wid_b, wid_a);
    assert!(
        app.windows[&wid_b].focused,
        "a logical window is born believing it has focus — the flag this law must not trust alone"
    );
    assert_eq!(app.frontmost_window, Some(wid_b));
    assert!(
        drain(pipe_a).is_empty() && drain(pipe_b).is_empty() && drain(pipe_c).is_empty(),
        "moving a tab to its own window hands the keyboard to the session that already had it: nothing to report"
    );

    app.switch_tab_in(wid_a, 0);
    assert!(
        drain(pipe_a).is_empty() && drain(pipe_b).is_empty() && drain(pipe_c).is_empty(),
        "a background window's tab switch hands no keyboard over"
    );

    app.frontmost_window = Some(wid_a);
    app.sync_active_session();
    assert_eq!(
        drain(pipe_c),
        FOCUS_OUT.to_vec(),
        "the other window's session is blurred first"
    );
    assert_eq!(
        drain(pipe_a),
        FOCUS_IN.to_vec(),
        "... and only then does the new front window's session claim it"
    );
    assert!(
        drain(pipe_b).is_empty(),
        "the tab A was showing before the switch never held the keyboard while A was away"
    );
}

#[test]
fn a_controller_focus_in_survives_the_next_resync() {
    let (pipe_a, sink_a) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    assert!(app.headless, "the production `--headless` shape");
    assert!(
        app.windows.values().all(|ws| ws.os_window.is_none()),
        "`resumed` returns before attaching any surface under --headless"
    );
    arm_focus_reporting(&app, 0);
    let _ = drain(pipe_a);

    app.on_focus(wid, false);
    assert_eq!(drain(pipe_a), FOCUS_OUT.to_vec(), "the verb blurs");
    app.on_focus(wid, true);
    assert_eq!(drain(pipe_a), FOCUS_IN.to_vec(), "the verb focuses");

    app.sync_window(wid);
    app.sync_active_session();
    assert!(
        drain(pipe_a).is_empty(),
        "a re-sync must not revoke a report the controller just forced"
    );
}

#[test]
fn a_tab_switch_reports_focus_without_taking_an_engine_lock() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session_with_sink(1, sink_b));
    let acq = crate::term_lock_acquisitions_on_this_thread;

    app.on_focus(wid, false);
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);
    let before = acq();
    app.switch_tab_in(wid, 0);
    let without_reports = acq() - before;
    assert!(
        drain(pipe_a).is_empty() && drain(pipe_b).is_empty(),
        "the control writes nothing, by construction"
    );

    app.on_focus(wid, true);
    arm_focus_reporting(&app, 0);
    arm_focus_reporting(&app, 1);
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);

    let before = acq();
    app.switch_tab_in(wid, 1);
    let with_reports = acq() - before;
    assert_eq!(
        drain(pipe_a),
        FOCUS_OUT.to_vec(),
        "the switch really did write the pair being measured"
    );
    assert_eq!(drain(pipe_b), FOCUS_IN.to_vec());
    assert_eq!(
        with_reports, without_reports,
        "writing both focus reports must cost the engine mutex NOTHING: \
         {with_reports} acquisitions against the {without_reports} of the \
         same switch with no report owed"
    );
}

#[test]
fn a_window_focus_edge_is_activity_to_the_automatic_update() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    for focused in [false, true] {
        let epoch = app.update_handoff_activity_epoch;
        app.on_focus(wid, focused);
        assert!(
            app.update_handoff_activity_epoch > epoch,
            "on_focus({focused}) moved the quiet clock"
        );
    }
}

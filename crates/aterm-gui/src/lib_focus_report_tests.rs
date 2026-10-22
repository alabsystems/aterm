// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::time::Instant;

use aterm_session::sink::SinkWriter;

use super::{App, WindowId, pane, stub_session_with_sink, term_lock};

const FOCUS_IN: &[u8] = b"\x1b[I";
const FOCUS_OUT: &[u8] = b"\x1b[O";

/// A non-blocking read end plus a write end, so `drain` answers "nothing"
/// instead of parking the suite when a lane is correctly silent.
fn observer() -> ([i32; 2], Arc<SinkWriter>) {
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

fn drain(pipe: [i32; 2]) -> Vec<u8> {
    let mut bytes = [0u8; 64];
    let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
    if read <= 0 {
        return Vec::new();
    }
    bytes[..read as usize].to_vec()
}

/// Arm DEC 1004 on `session`'s engine, as `ESC[?1004h` from nvim or tmux does.
fn arm_focus_reporting(app: &App, session: u64) {
    let term = app.pool.get(session).expect("live session").term.clone();
    term_lock(&term).process(b"\x1b[?1004h");
}

/// `bytes` as program output of `session`, fed to its engine as its reader feeds a batch,
/// before the reader's output wake.
fn feed(app: &App, session: u64, bytes: &[u8]) {
    let term = app.pool.get(session).expect("live session").term.clone();
    term_lock(&term).process(bytes);
}

/// [`feed`], then the output wake the reader posts after the batch: the real
/// `Wake::Output` arm, so whatever the bytes owe the program is paid.
fn program_writes(app: &mut App, session: u64, bytes: &[u8]) {
    feed(app, session, bytes);
    app.on_output_wake(session, Instant::now());
}

/// A program born in a background tab that turns DEC 1004 on is told at once that it does
/// not have the keyboard, once per enable: a re-sent `?1004h`, later output and a re-sync
/// write nothing more, and the switch that brings its tab forward focuses it once.
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

    // Off and on again behind: a new enable, a new answer.
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

/// In the session holding the keyboard an enable writes nothing (as in xterm): the
/// program's assumption that it is focused is right. Leaving it then blurs it once, and an
/// enable the blur already answered is not answered again by the late output wake.
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

    // An enable whose output wake has not run yet when the keyboard leaves: the switch's
    // blur is its answer.
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

/// A split's visible pane without the keyboard is as unfocused as a background tab: its
/// program's enable is answered `ESC[O`, and moving the keyboard to it focuses it once.
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

/// With no aterm window focused no session holds the keyboard, so even the front tab's
/// program is told it is unfocused when it enables reports; the window's return focuses it.
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

/// The window edge reports each change once: a repeated blur or focus, as a platform may
/// deliver, writes nothing the first one did not.
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

/// Key status moving between two aterm windows, through the real `WindowEvent::Focused`
/// handler in both orders macOS delivers it, tells the session the keyboard left `ESC[O`
/// once and the session it reached `ESC[I` once; a tear-out, whose session keeps the
/// keyboard, tells nobody anything.
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
    // Drains every pipe before judging, so a loud one cannot hide what the others hold.
    let quiet = |pipes: &[[i32; 2]]| {
        let held: Vec<Vec<u8>> = pipes.iter().map(|pipe| drain(*pipe)).collect();
        held.iter().all(Vec::is_empty)
    };
    let _ = quiet(&[pipe_a, pipe_b, pipe_c]);

    // Tear out session 2, then the OS events that follow: window A resigns key, B takes it.
    let wid_b = app.detach_active_tab_logical().expect("the tab tears out");
    app.window_focus_event(wid_a, false);
    app.window_focus_event(wid_b, true);
    assert!(
        quiet(&[pipe_a, pipe_b, pipe_c]),
        "session 2 kept the keyboard and A's front tab never had it: nothing to report"
    );

    // Back to A, the new key window's event first.
    app.window_focus_event(wid_a, true);
    app.window_focus_event(wid_b, false);
    assert_eq!(drain(pipe_c), FOCUS_OUT.to_vec(), "B's session blurs once");
    assert_eq!(
        drain(pipe_b),
        FOCUS_IN.to_vec(),
        "A's front session focuses once"
    );

    // And to B again, the old key window's event first.
    app.window_focus_event(wid_a, false);
    app.window_focus_event(wid_b, true);
    assert_eq!(drain(pipe_b), FOCUS_OUT.to_vec(), "A's session blurs once");
    assert_eq!(drain(pipe_c), FOCUS_IN.to_vec(), "B's session focuses once");
    assert!(
        drain(pipe_a).is_empty(),
        "A's background tab is written nothing through all of it"
    );
}

/// Switching the active tab reports `ESC[O` to the tab left and `ESC[I` to the tab
/// arrived at, with no `WindowEvent::Focused`.
#[test]
fn switching_the_active_tab_blurs_the_tab_left_and_focuses_the_tab_arrived_at() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session_with_sink(1, sink_b));

    // Arm the mode after the staging switch, then clear both pipes, so what follows is
    // measured on a quiet wire.
    arm_focus_reporting(&app, 0);
    arm_focus_reporting(&app, 1);
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);

    // Tab 1 is active (`push_stub_tab` switched to it). Go back to tab 0.
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

    // The same the other way through `cycle_tab_in`, which the next/prev-tab accelerator and
    // the Window menu dispatch.
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

/// A session that never armed DEC 1004 hears nothing, and a re-sync that moves no keyboard
/// writes nothing to a session that did: the report is a delta, not a heartbeat.
#[test]
fn an_unarmed_session_and_a_switch_that_moves_nothing_both_write_no_bytes() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session_with_sink(1, sink_b));
    // Only tab 0 arms the mode; tab 1 is an ordinary shell that never asked.
    arm_focus_reporting(&app, 0);
    let _ = drain(pipe_a);
    let _ = drain(pipe_b);

    app.switch_tab_in(wid, 0);
    assert_eq!(drain(pipe_a), FOCUS_IN.to_vec(), "the armed tab is told");
    assert!(
        drain(pipe_b).is_empty(),
        "a session that never set DEC 1004 is written nothing"
    );

    // Already active: `switch_tab_in` returns early, and even a bare
    // re-mirror of the same front must not re-report.
    app.switch_tab_in(wid, 0);
    app.sync_window(wid);
    assert!(
        drain(pipe_a).is_empty(),
        "an unmoved keyboard reports nothing at all"
    );
}

/// A split gives one pane the keyboard: the split blurs the pane that lost it, and each
/// `focus_pane_in` move reports both sides.
#[test]
fn moving_focus_between_split_panes_reports_both_sides() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    arm_focus_reporting(&app, 0);
    let _ = drain(pipe_a);

    // Splitting moves focus to the new pane; the original is still visible but no longer
    // has the keyboard.
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

/// The OS-window edge and the in-window delta share one account: an unfocused window's
/// tab switch reports nothing, and on refocus the front tab is told exactly once.
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

    // Alt-tab away: the front tab (1) is blurred, once, by the window edge.
    app.on_focus(wid, false);
    assert_eq!(drain(pipe_b), FOCUS_OUT.to_vec(), "the front tab blurs");
    assert!(
        drain(pipe_a).is_empty(),
        "a background tab has nothing to lose"
    );

    // Switching tabs while the window is away moves no keyboard.
    app.switch_tab_in(wid, 0);
    assert!(
        drain(pipe_a).is_empty() && drain(pipe_b).is_empty(),
        "an unfocused window's tab switch hands no keyboard over"
    );

    // Coming back tells the tab that is now in front, exactly once.
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

    // The next switch is a clean out/in pair, not a doubled arrival.
    app.switch_tab_in(wid, 1);
    assert_eq!(drain(pipe_a), FOCUS_OUT.to_vec());
    assert_eq!(drain(pipe_b), FOCUS_IN.to_vec());
}

/// At most one session in the process holds the keyboard. Windows are born `focused: true` and
/// some never get a `Focused` event, so the holder is resolved from `frontmost_window`: tearing
/// out the active tab and a tab switch in the window left behind write nothing; bringing that
/// window to the front blurs the other window's session, then focuses the arriving one.
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

    // Tear out the active tab (session 2): window B is born `focused: true`, with no OS
    // surface and no `WindowEvent::Focused` to come.
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

    // A tab switch in the window left behind, which is not where keystrokes go.
    app.switch_tab_in(wid_a, 0);
    assert!(
        drain(pipe_a).is_empty() && drain(pipe_b).is_empty() && drain(pipe_c).is_empty(),
        "a background window's tab switch hands no keyboard over"
    );

    // Now window A comes back to the front. One claim moves: out of the
    // session in B, into A's newly active tab.
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

/// The controller focus lane (`aterm ctl focus in|out` via [`App::on_focus`]) is the
/// only way to move `ws.focused` on a headless window, so a re-sync must not revoke its
/// report: the holder is not gated on `os_window.is_some()`.
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

    // `focus out` then `focus in`, exactly as `Wake::Input` dispatches them.
    app.on_focus(wid, false);
    assert_eq!(drain(pipe_a), FOCUS_OUT.to_vec(), "the verb blurs");
    app.on_focus(wid, true);
    assert_eq!(drain(pipe_a), FOCUS_IN.to_vec(), "the verb focuses");

    // A bare re-sync writes nothing: the account already names this session.
    app.sync_window(wid);
    app.sync_active_session();
    assert!(
        drain(pipe_a).is_empty(),
        "a re-sync must not revoke a report the controller just forced"
    );
}

/// [`App::sync_window`] must never take the engine mutex. The focus report's DEC 1004
/// gate is read from the lock-free `ModeMirror`, so a tab switch writing both reports
/// takes as many engine locks as one writing none (thread-local acquisition census).
#[test]
fn a_tab_switch_reports_focus_without_taking_an_engine_lock() {
    let (pipe_a, sink_a) = observer();
    let (pipe_b, sink_b) = observer();
    let mut app = App::headless_for_test_with_sink(sink_a);
    let wid = WindowId(0);
    app.push_stub_tab(wid, stub_session_with_sink(1, sink_b));
    let acq = crate::term_lock_acquisitions_on_this_thread;

    // Control: with the window blurred the holder is already `None`, so the delta returns
    // before any report; this switch's lock count is the baseline.
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

    // The same gesture between two sessions that both armed DEC 1004: both reports written.
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

/// A window focus edge, the OS's or the controller's, is still activity to the automatic
/// update (`input_event_handoff_class` tolerates `Focus`), as when the edge went through
/// the input seam: an automatic apply waits out a person arriving at or leaving a window.
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

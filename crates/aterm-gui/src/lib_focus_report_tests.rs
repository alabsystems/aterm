// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

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

/// Arm DEC 1004 on `session`'s engine — the app-set mode bit the egress is
/// gated on, exactly as `ESC[?1004h` from nvim/tmux sets it.
fn arm_focus_reporting(app: &App, session: u64) {
    let term = app.pool.get(session).expect("live session").term.clone();
    term_lock(&term).process(b"\x1b[?1004h");
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

    // ... and the same in the other direction, through the CYCLE lane the
    // next/prev-tab accelerator and the Window menu both dispatch.
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

/// A session that never armed DEC 1004 hears nothing, and a re-sync that
/// moves no keyboard writes nothing to a session that did. The report is a
/// DELTA, not a heartbeat.
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

    // And the account survives the round trip: the very next switch is a
    // clean out/in pair rather than a doubled arrival.
    app.switch_tab_in(wid, 1);
    assert_eq!(drain(pipe_a), FOCUS_OUT.to_vec());
    assert_eq!(drain(pipe_b), FOCUS_IN.to_vec());
}

/// At most one session in the process is told it holds the keyboard. Windows are born
/// `focused: true` and some never get a `Focused` event, so the holder is resolved from
/// `frontmost_window`. Pinned in order:
///   1. tearing the active tab into its own window writes nothing (same session keeps
///      the keyboard);
///   2. a tab switch in the window left behind writes nothing;
///   3. bringing that window to the front moves the claim: `ESC[O` to the other
///      window's session, `ESC[I` to the arriving one.
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

    // Tab 2 (session 2) is active and holds the keyboard. Tear it out into
    // its own window: window B is born `focused: true` with no OS surface
    // and no `WindowEvent::Focused` in its future.
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

    // A tab switch in the window left BEHIND. It is not where keystrokes go,
    // so it may not blur or focus anything.
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

    // A bare re-stabilization must now write nothing: the account already
    // names this session, and a holder gate that could not see a
    // controller-asserted focus would blur it here.
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

    // The control, chosen so it provably cannot reach the egress seam at
    // all: with the window blurred the holder is already `None`, so the
    // delta returns before any report. What this switch costs the engine
    // mutex is what a tab switch cost before focus reporting existed.
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

    // The MEASUREMENT: the same gesture, moving the keyboard between two
    // sessions that have both armed DEC 1004, so both reports are written.
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

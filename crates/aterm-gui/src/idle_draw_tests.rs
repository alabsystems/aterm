// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `docs/ATERM_DESIGN.md` §7.4 on a headless App: a minimised window composes and presents
//! nothing until it is visible again, a covered one at most a frame every 5 s, unless a capture,
//! a recording, an assistive client or the window's first frame waits on it, while the frame's
//! work that is not drawing still runs; a window raised and focused draws at once and stays
//! hidden until its occlusion changes; the frame after a park books no latency for the output
//! the park held.

use std::sync::atomic::Ordering;
use std::time::Instant;

use super::{App, WindowId};
use crate::idle_draw::{COVERED_FRAME_PERIOD, Sight};
use crate::scripted_redraw_fixture::{feed, scripted_app, surface};
use crate::work_counts;

const WID: WindowId = WindowId(0);

/// Hides window 0 as its occlusion change does, minimised (`off_glass`, which a headless window
/// has no OS window to read) or only covered.
fn hide(app: &mut App, off_glass: bool) {
    app.on_occlusion_changed(WID, true);
    app.windows.get_mut(&WID).expect("the window").off_glass = off_glass;
}

fn commits(app: &mut App) -> usize {
    surface(app).commits()
}

#[test]
fn a_minimised_window_presents_nothing_until_it_is_visible_unless_a_capture_waits() {
    let mut app = scripted_app();
    feed(&app, b"shown");
    app.redraw_window(WID);
    let first = commits(&mut app);
    assert!(first > 0, "the window's first present");
    // Minimised or its app hidden: output composes and presents nothing.
    hide(&mut app, true);
    for line in [b"one\r\n".as_slice(), b"two\r\n", b"three\r\n"] {
        feed(&app, line);
        app.on_output_wake(0, Instant::now());
        app.redraw_window(WID);
    }
    assert_eq!(
        commits(&mut app),
        first,
        "a minimised window submits no work"
    );
    assert_eq!(
        app.windows[&WID].covered_frame_deadline(),
        None,
        "and owes no paced frame"
    );
    // A capture waits on its frame: that one presents, and the window parks again after it.
    app.redraw_capture_frame(WID, None);
    let captured = commits(&mut app);
    assert!(captured > first, "the capture's frame presents");
    feed(&app, b"four\r\n");
    app.redraw_window(WID);
    assert_eq!(commits(&mut app), captured, "and the window parks again");
    // Visible again, it draws its current state once.
    app.on_occlusion_changed(WID, false);
    app.redraw_window(WID);
    assert!(commits(&mut app) > captured, "visible again, it draws");
}

/// A covered window, or one on another Space, can still be watched (a window share, a
/// single-window recording): it draws a changed frame at once, then at most one a period, the
/// frames between owed at the period's end, never frozen.
#[test]
fn a_covered_window_draws_at_most_a_frame_a_period_and_owes_the_rest() {
    let mut app = scripted_app();
    feed(&app, b"shown");
    app.redraw_window(WID);
    hide(&mut app, false);
    let t0 = Instant::now();
    let before = commits(&mut app);
    feed(&app, b"one\r\n");
    app.redraw_window_at(WID, t0);
    let paced = commits(&mut app);
    assert!(
        paced > before,
        "the first change of a covered spell draws at once"
    );
    feed(&app, b"two\r\n");
    let soon = t0 + COVERED_FRAME_PERIOD / 2;
    app.redraw_window_at(WID, soon);
    assert_eq!(commits(&mut app), paced, "the next one parks");
    let due = t0 + COVERED_FRAME_PERIOD;
    let mut ws = app.windows.get_mut(&WID).expect("the window");
    assert_eq!(
        ws.covered_frame_deadline(),
        Some(due),
        "owed at the period's end"
    );
    assert!(!ws.covered_frame_due(soon));
    assert!(ws.covered_frame_due(due), "the owed frame comes due");
    assert_eq!(ws.covered_frame_deadline(), None, "once");
    drop(ws);
    app.redraw_window_at(WID, due);
    assert!(commits(&mut app) > paced, "and draws the current state");
    // Minimised from there, it owes nothing more.
    feed(&app, b"three\r\n");
    app.windows.get_mut(&WID).expect("the window").off_glass = true;
    let drawn = commits(&mut app);
    app.redraw_window_at(WID, due + COVERED_FRAME_PERIOD * 2);
    assert_eq!(commits(&mut app), drawn);
    assert_eq!(app.windows[&WID].covered_frame_deadline(), None);
}

/// A window that never presented draws its first frame although it is hidden (a window opened
/// behind another, or carried by an update hand-off), as one waiting to be revealed does.
#[test]
fn a_hidden_window_still_draws_its_first_frame() {
    let mut app = scripted_app();
    assert_eq!(app.windows[&WID].capture_present_serial, 0);
    hide(&mut app, true);
    feed(&app, b"first");
    app.redraw_window(WID);
    let first = commits(&mut app);
    assert!(first > 0, "the first frame presents");
    assert!(app.windows[&WID].capture_present_serial > 0);
    feed(&app, b"second");
    app.redraw_window(WID);
    assert_eq!(commits(&mut app), first, "then it parks");
}

/// A window an assistive client reads builds the client's tree from the frames it draws, so it
/// keeps drawing while hidden.
#[cfg(a11y_tree)]
#[test]
fn a_hidden_window_an_assistive_client_reads_keeps_its_frames() {
    let mut app = scripted_app();
    feed(&app, b"before\r\n");
    app.redraw_window(WID);
    hide(&mut app, true);
    app.windows.get_mut(&WID).expect("the window").a11y_active = true;
    let (drawn, told) = (commits(&mut app), app.windows[&WID].a11y_sent.len());
    feed(&app, b"hidden-line\r\n");
    app.on_output_wake(0, Instant::now());
    app.redraw_window(WID);
    assert!(commits(&mut app) > drawn, "the client's window draws");
    assert!(
        app.windows[&WID].a11y_sent.len() > told,
        "and tells the client"
    );
    // CONTROL: with no client, the window parks.
    app.windows.get_mut(&WID).expect("the window").a11y_active = false;
    let drawn = commits(&mut app);
    feed(&app, b"unread-line\r\n");
    app.redraw_window(WID);
    assert_eq!(commits(&mut app), drawn);
}

/// A parked frame still does the frame's work that is not drawing: it pays the re-grid its pane
/// is owed (a cross-session `resize` leaves one, paid at the frame that draws its tab), so the
/// shell sees its pane's size, and it carries the program's title to the store.
#[test]
fn a_parked_frame_pays_its_regrid_and_carries_the_title() {
    let mut app = scripted_app();
    feed(&app, b"x");
    app.redraw_window(WID);
    let session = app.front_terminal(WID).expect("the front terminal").session;
    let size = |app: &App| {
        let t = crate::term_lock(&app.pool.get(session).expect("the shell").term);
        (t.rows(), t.cols())
    };
    let laid_out = size(&app);
    hide(&mut app, true);
    crate::term_lock(&app.pool.get(session).expect("the shell").term)
        .resize(laid_out.0 - 3, laid_out.1 - 5);
    app.owe_pane_regrid(session);
    feed(&app, b"\x1b]2;hidden-title\x07");
    let drawn = commits(&mut app);
    app.redraw_window(WID);
    assert_eq!(commits(&mut app), drawn, "PRECONDITION: the frame parked");
    assert_eq!(size(&app), laid_out, "the owed regrid is paid");
    assert!(!app.windows[&WID].panes_stale);
    assert_eq!(app.windows[&WID].store_title.1, "hidden-title");
}

/// A window raised and focused while hidden draws its current state at once, before the
/// occlusion change that follows the raise, so it never shows the frame it parked on. Focus alone
/// does not put it on screen: a key window behind a lock screen, display sleep or a screen saver
/// gets no occlusion change, so it stays hidden for the readers of `occluded` and parks again
/// after that one frame, until its occlusion changes.
#[test]
fn a_raised_window_draws_at_once_and_stays_hidden_until_its_occlusion_changes() {
    let mut app = scripted_app();
    feed(&app, b"shown");
    app.redraw_window(WID);
    app.on_focus(WID, false);
    hide(&mut app, true);
    feed(&app, b"while-hidden\r\n");
    app.on_output_wake(0, Instant::now());
    app.redraw_window(WID);
    let parked = commits(&mut app);
    app.window_focus_event(WID, true);
    assert!(
        app.windows[&WID].occluded,
        "focus alone does not put the window on screen"
    );
    assert_eq!(app.windows[&WID].sight(), Sight::OffGlass);
    app.redraw_window(WID);
    let raised = commits(&mut app);
    assert!(raised > parked, "it draws its current state at once");
    feed(&app, b"still-hidden\r\n");
    app.on_output_wake(0, Instant::now());
    app.redraw_window(WID);
    assert_eq!(commits(&mut app), raised, "then, still hidden, it parks");
    app.on_occlusion_changed(WID, false);
    app.redraw_window(WID);
    assert!(commits(&mut app) > raised, "visible, it draws");
}

/// Stamps every session's output as just printed, so the next present that shows it books an
/// output-to-present latency sample.
fn arm_output(app: &App) {
    let now = u64::try_from(app.lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
    for session in app.pool.iter() {
        session
            .last_output_ns
            .store(now.saturating_sub(1_000).max(1), Ordering::Relaxed);
    }
}

/// Output a hidden window's parked frame held reaches glass only when the window next draws (a
/// covered window's paced frame, a raise): its wait is the park's, not a slow present, so that
/// frame books no output-to-present latency sample for it, as a window shown again does
/// (`App::on_occlusion_changed`). A covered window's change that draws at once books its own.
#[test]
fn a_frame_after_a_park_books_no_latency_for_the_output_the_park_held() {
    let mut app = scripted_app();
    feed(&app, b"shown");
    app.redraw_window(WID);
    app.on_focus(WID, false);
    hide(&mut app, false);
    let t0 = Instant::now();
    let _ = work_counts::take_present_samples();
    arm_output(&app);
    feed(&app, b"one\r\n");
    app.redraw_window_at(WID, t0);
    assert_eq!(
        work_counts::take_present_samples(),
        (0, 1),
        "CONTROL: a covered window's change drawn at once books its sample"
    );
    arm_output(&app);
    feed(&app, b"two\r\n");
    let drawn = commits(&mut app);
    app.redraw_window_at(WID, t0 + COVERED_FRAME_PERIOD / 2);
    assert_eq!(commits(&mut app), drawn, "PRECONDITION: the frame parked");
    app.redraw_window_at(WID, t0 + COVERED_FRAME_PERIOD);
    assert!(
        commits(&mut app) > drawn,
        "PRECONDITION: the paced frame drew"
    );
    assert_eq!(
        work_counts::take_present_samples(),
        (0, 0),
        "the paced frame books none for the output the park held"
    );
    app.windows.get_mut(&WID).expect("the window").off_glass = true;
    arm_output(&app);
    feed(&app, b"three\r\n");
    let drawn = commits(&mut app);
    app.redraw_window(WID);
    assert_eq!(commits(&mut app), drawn, "PRECONDITION: the frame parked");
    app.window_focus_event(WID, true);
    app.redraw_window(WID);
    assert!(commits(&mut app) > drawn, "PRECONDITION: the raise drew");
    assert_eq!(
        work_counts::take_present_samples(),
        (0, 0),
        "nor does the frame of a raise"
    );
}

/// `help trail` names each field `trail status` appends to its standing row, and exactly the
/// words it prints as `sight=`, read from every occlusion and off-glass state a window can be in.
#[test]
fn help_trail_names_the_standing_rows_fields_and_its_sight_words() {
    use std::collections::BTreeSet;
    let help = aterm_types::control_verbs::spec("trail")
        .expect("the trail verb")
        .help_line();
    let app = App::headless_for_test();
    let row = app.trail_status().expect("a front window");
    let (_, tail) = row
        .split_once(" word_icons=")
        .expect("the standing row's appended fields");
    let appended = std::iter::once("word_icons").chain(
        tail.split(' ')
            .filter_map(|word| word.split_once('=').map(|(key, _)| key)),
    );
    for key in appended {
        assert!(help.contains(&format!("{key}=")), "help trail names {key}=");
    }
    let lists: Vec<&str> = help.split("`sight=").skip(1).collect();
    assert_eq!(lists.len(), 1, "help trail names one sight= vocabulary");
    let named: BTreeSet<&str> = lists[0]
        .split_once('`')
        .expect("the vocabulary closes")
        .0
        .split('|')
        .collect();
    let written: BTreeSet<&str> = [false, true]
        .into_iter()
        .flat_map(|occluded| [false, true].map(|off_glass| Sight::of(occluded, off_glass).name()))
        .collect();
    assert_eq!(named, written);
}

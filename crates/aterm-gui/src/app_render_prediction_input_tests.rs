// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A key's proven prediction on glass closes its input slice, end to end: the
//! press path tags the key's input edge with the op it armed
//! (`PendingInputStamp::tag_newest`), the render path marks the op its frame
//! showed whole and under its proof (`metrics::note_prediction_shown`), and
//! that frame's present books the key (`metrics::record_present`). Each half
//! was right alone and the whole was not (`aterm-lab predict-live`, 2026-10-20:
//! every predicted key's slice waited for its echo, 109 ms over a 100 ms
//! link), so these tests drive the shipping `App::input` and `redraw_window`
//! on a scripted CPU surface, through both render paths, and read the slices
//! the present booked on this thread (`work_counts::take_input_slices`).
//!
//! The echo latency `adaptive` needs is put in by the reconciles that prime
//! the predictor, at instants the test chooses, and the press and the frames
//! after it are stamped on that same timeline (`pin_input_clock`,
//! `redraw_window_at`): nothing sleeps, and no stall of the test thread ages
//! the key's guess past its glitch window.

use crate::input::{InputEvent, Source};
use crate::scripted_redraw_fixture::{scripted_app, surface};
use crate::{App, WindowId, term_lock, work_counts};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const WID: WindowId = WindowId(0);
/// A link slow enough for `adaptive` to display: two such echoes open it.
const RTT: Duration = Duration::from_millis(60);

fn glyph(c: char) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(c),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

/// The scripted app in `mode`, split when `split` (the composed path), with
/// `$ ` at the keyboard's terminal and one frame presented.
fn rig(mode: &str, split: bool) -> App {
    let mut app = scripted_app();
    if split {
        let _ = app.split_active_stub_tab_dir_with_sink(
            WID,
            crate::pane::SplitDir::Vertical,
            std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1)),
        );
        app.windows.get_mut(&WID).expect("window").focused = true;
    }
    app.config.predictive_echo = Some(mode.to_string());
    app.predict_mode_cache = None;
    feed(&app, "$ ");
    app.redraw_window(WID);
    let _ = work_counts::take_input_slices();
    app
}

fn feed(app: &App, text: &str) {
    let term = app
        .front_terminal(WID)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(text.as_bytes());
}

/// The keyboard terminal's cursor, coordinate generation and grid.
fn sample(app: &App) -> ((u16, u16), u64, (u16, u16)) {
    let term = app
        .front_terminal(WID)
        .expect("front terminal")
        .term
        .clone();
    let t = term_lock(&term);
    let cursor = t.cursor();
    (
        (cursor.row, cursor.col),
        t.content_scroll_state().coordinate_generation(),
        (t.cols(), t.rows()),
    )
}

/// Prime the window's predictor as a slow, echoing line does: `ab` typed at
/// the cursor, each echoed one RTT after its press (the program's echo fed to
/// the terminal, the reconcile at an instant the test chooses), so `adaptive`
/// displays the next key. Typed on the predictor itself: no input edge is armed
/// for these two. Returns the instant of the second echo's reconcile.
fn prime_slow_echoes(app: &mut App) -> Instant {
    let mut at = Instant::now();
    for ch in ['a', 'b'] {
        let (cursor, coords, grid) = sample(app);
        let predictor = &mut app.windows.get_mut(&WID).expect("window").predictor;
        predictor.set_mode(crate::predict::PredictMode::Adaptive);
        assert!(predictor.predict_char_in_grid(ch, cursor, coords, grid, at));
        feed(app, &ch.to_string());
        at += RTT;
        let (echoed, coords, _) = sample(app);
        let predictor = &mut app.windows.get_mut(&WID).expect("window").predictor;
        predictor.reconcile(Some(echoed), coords, false, at, |row, col| {
            ((row, col) == cursor).then_some(ch)
        });
        assert!(predictor.idle(), "fixture: `{ch}`'s echo retired its guess");
    }
    at
}

/// Press `ev` through the press path, stamped at `at`.
fn press_at(app: &mut App, ev: InputEvent, at: Instant) {
    crate::app_input::pin_input_clock(Some(at));
    let _ = app.input(WID, ev, Source::Human);
}

/// Stamp output on every session, so the next present is a CONTENT present
/// (one with PTY output behind it): what the echo's frame is.
fn arm_output(app: &App) {
    let now = u64::try_from(app.lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
    for session in app.pool.iter() {
        session
            .last_output_ns
            .store(now.saturating_sub(1_000).max(1), Ordering::Relaxed);
    }
}

/// Press `c` through the press path at `at`, present a frame with no output
/// there, then the echo's content present one RTT later: `(the slices the first
/// present booked, of them predicted, whether it painted a prediction, the
/// slices the echo's booked)`.
fn press_then_echo(app: &mut App, at: Instant) -> ((usize, usize), bool, (usize, usize)) {
    press_at(app, glyph('c'), at);
    let commits = surface(app).commits();
    // The frame presents whatever changed (the early-out compares the last
    // present's key): with prediction off nothing did, and that frame is
    // still a present the key's slice could close at.
    app.windows.get_mut(&WID).expect("window").last_present = None;
    app.redraw_window_at(WID, at);
    assert!(
        surface(app).commits() > commits,
        "fixture: the frame after the press presented"
    );
    let before_echo = work_counts::take_input_slices();
    let painted = app.windows[&WID].pred_shown;
    feed(app, "c");
    arm_output(app);
    app.redraw_window_at(WID, at + RTT);
    (before_echo, painted, work_counts::take_input_slices())
}

/// Before a present or echo, `c,d,Backspace,e` leaves only `c,e` pending.
/// The first frame shows both, but the cancelled `d` is no longer a shown
/// op. Its edge stays for the content present and stops the ordered run, as
/// does Backspace's untagged edge; only `c` may book as predicted. Every key is
/// pressed at `at`, the first frame presented there and the echo's one RTT later.
fn cancelled_key_then_echo(app: &mut App, at: Instant) -> ((usize, usize), (usize, usize)) {
    let commits = surface(app).commits();
    press_at(app, glyph('c'), at);
    let c = app.windows[&WID].predictor.newest_op().expect("c armed");
    press_at(app, glyph('d'), at);
    let d = app.windows[&WID].predictor.newest_op().expect("d armed");
    assert!(d > c);
    let backspace = InputEvent::Key {
        key: Key::Named(aterm_types::keyboard::NamedKey::Backspace),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    press_at(app, backspace, at);
    assert_eq!(app.windows[&WID].predictor.newest_op(), Some(c));
    assert!(!app.windows[&WID].predictor.op_is_pending(d));
    press_at(app, glyph('e'), at);
    let e = app.windows[&WID].predictor.newest_op().expect("e armed");
    assert!(e > d);
    assert_eq!(
        app.windows[&WID]
            .predictor
            .pending()
            .map(|op| op.ch)
            .collect::<Vec<_>>(),
        ['c', 'e'],
        "the cancelled key is absent before any frame or echo"
    );
    assert_eq!(
        surface(app).commits(),
        commits,
        "all four keys precede the first present"
    );
    app.windows.get_mut(&WID).expect("window").last_present = None;
    app.redraw_window_at(WID, at);
    assert!(surface(app).commits() > commits, "a real scripted present");
    assert!(app.windows[&WID].pred_shown, "the overlay painted");
    assert_eq!(app.windows[&WID].predictor.shown_through(), Some(e));
    let before_echo = work_counts::take_input_slices();
    // The actual final text is `ce`; the cancelled key and its erase both
    // echo before `e`, without changing which keys the earlier frame showed.
    feed(app, "cd\u{8} \u{8}e");
    arm_output(app);
    app.redraw_window_at(WID, at + RTT);
    (before_echo, work_counts::take_input_slices())
}

/// `adaptive` on a slow, echoing line, on the single pane's render path and on
/// the composed one: the frame after the press paints `c`'s prediction and
/// its present books `c`'s slice, counted as predicted; the echo's content
/// present then books nothing more. The controls: with prediction `off` the
/// frame paints nothing and the key waits for its echo's present; `always`
/// paints the ghost with no proof, and its key waits for its echo too.
#[test]
fn a_keys_proven_prediction_on_glass_closes_its_input_slice() {
    for split in [false, true] {
        let path = if split { "composed" } else { "single pane" };
        let mut app = rig("adaptive", split);
        let at = prime_slow_echoes(&mut app);
        assert_eq!(
            press_then_echo(&mut app, at),
            ((1, 1), true, (0, 0)),
            "{path}: `c` is on glass at its prediction's present, and booked there once"
        );

        let mut app = rig("off", split);
        assert_eq!(
            press_then_echo(&mut app, Instant::now()),
            ((0, 0), false, (1, 0)),
            "{path}, prediction off: the key waits for its echo's present"
        );

        let mut app = rig("always", split);
        assert_eq!(
            press_then_echo(&mut app, Instant::now()),
            ((0, 0), true, (1, 0)),
            "{path}, `always`: a ghost with no proof is not the key's effect"
        );

        let mut app = rig("adaptive", split);
        let at = prime_slow_echoes(&mut app);
        assert_eq!(
            cancelled_key_then_echo(&mut app, at),
            ((1, 1), (3, 0)),
            "{path}: only c was a shown pending key before the cancelled d; d, Backspace and e wait for the content present"
        );
    }
}

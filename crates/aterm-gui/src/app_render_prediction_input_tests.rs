// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A key's proven prediction on glass closes its input slice, end to end.

use crate::input::{InputEvent, Source};
use crate::scripted_redraw_fixture::{scripted_app, surface};
use crate::{App, WindowId, term_lock, work_counts};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const WID: WindowId = WindowId(0);
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

fn prime_slow_echoes(app: &mut App) -> Instant {
    let mut at = crate::app_input::past_timeline_start();
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

fn press_at(app: &mut App, ev: InputEvent, at: Instant) {
    crate::app_input::pin_input_clock(Some(at));
    let _ = app.input(WID, ev, Source::Human);
}

fn arm_output(app: &App) {
    let now = u64::try_from(app.lat_epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
    for session in app.pool.iter() {
        session
            .last_output_ns
            .store(now.saturating_sub(1_000).max(1), Ordering::Relaxed);
    }
}

fn press_then_echo(app: &mut App, at: Instant) -> ((usize, usize), bool, (usize, usize)) {
    press_at(app, glyph('c'), at);
    let commits = surface(app).commits();
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
    feed(app, "cd\u{8} \u{8}e");
    arm_output(app);
    app.redraw_window_at(WID, at + RTT);
    (before_echo, work_counts::take_input_slices())
}

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
            press_then_echo(&mut app, crate::app_input::past_timeline_start()),
            ((0, 0), false, (1, 0)),
            "{path}, prediction off: the key waits for its echo's present"
        );

        let mut app = rig("always", split);
        assert_eq!(
            press_then_echo(&mut app, crate::app_input::past_timeline_start()),
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

/// A mixed tab (a terminal leaf beside a Settings leaf) paints no predictions, so its keys arm
/// none: a guess nothing draws would only expire, owe and wake frames for nothing. The same key
/// in a terminal tab arms one.
#[test]
fn a_mixed_tab_arms_no_prediction_it_cannot_paint() {
    let at = crate::app_input::past_timeline_start();
    let mut control = rig("always", false);
    press_at(&mut control, glyph('c'), at);
    assert!(
        !control.windows[&WID].predictor.idle(),
        "precondition: a terminal tab's key arms a guess"
    );

    let mut app = scripted_app();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let _ = app.split_active_with_stub_terminal(WID, crate::tab_model::SplitAxis::Horizontal);
    app.windows.get_mut(&WID).expect("window").focused = true;
    app.config.predictive_echo = Some("always".to_string());
    app.predict_mode_cache = None;
    feed(&app, "$ ");
    app.redraw_window(WID);
    assert_eq!(
        app.active_visible_frame_layout(WID).map(|(_, route)| route),
        Some(crate::VisibleContentRoute::Heterogeneous),
        "precondition: a mixed tab with its terminal leaf focused"
    );
    press_at(&mut app, glyph('c'), at);
    assert!(
        app.windows[&WID].predictor.idle(),
        "the mixed tab's key arms no guess"
    );
}

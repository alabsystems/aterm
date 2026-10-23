// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One terminal hold per single-pane frame: effect probes, grid extraction, damage consume and cursor/colour facts come from one acquisition, so no PTY write lands between them.
use super::redraw_hold_hook;
use crate::scripted_redraw_fixture::{feed, has_damage, scripted_app};
use crate::{WindowId, term_lock, term_lock_acquisitions_on_this_thread};
use std::cell::Cell;
use std::rc::Rc;

/// `(damage epoch, caret)` as the frame's hold left them.
type HoldFacts = (u64, (u16, u16));

/// Every cell of the window's composed frame, row by row.
fn frame_text(app: &crate::App, wid: WindowId) -> String {
    let mut text = String::new();
    for row in &app.windows[&wid].input_scratch.cells {
        text.extend(row.iter().map(|cell| cell.ch));
        text.push('\n');
    }
    text
}

/// The scripted fixture with the shipped cursor effect (Rainbow Kitty) live.
fn lit_app() -> crate::App {
    let mut app = scripted_app();
    app.config.serious_mode = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    app
}

/// An echo frame takes the terminal mutex once, with effects off and with the cursor effect live.
#[test]
fn an_ordinary_echo_frame_takes_the_terminal_lock_once() {
    let wid = WindowId(0);
    for (name, mut app) in [
        ("effects off", scripted_app()),
        ("rainbow kitty", lit_app()),
    ] {
        app.redraw_window(wid);
        for echo in [&b"e"[..], b"c", b"h", b"o"] {
            feed(&app, echo);
            let before = term_lock_acquisitions_on_this_thread();
            app.redraw_window(wid);
            let taken = term_lock_acquisitions_on_this_thread() - before;
            assert!(
                !has_damage(&app),
                "{name}: the echo frame consumed its damage"
            );
            assert_eq!(
                taken, 1,
                "{name}: an ordinary echo frame takes ONE terminal hold (took {taken})"
            );
        }
    }
}

/// A PTY write landing right after the frame's hold is released leaves the frame coherent (key, extraction and glass all describe the grid before the write); the write stays damaged and the next frame presents it.
#[test]
fn a_write_landing_after_the_hold_leaves_the_frame_coherent_and_presents_next() {
    let wid = WindowId(0);
    let mut app = scripted_app();
    feed(&app, b"$ ");
    app.redraw_window(wid);
    feed(&app, b"x");
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let seen: Rc<Cell<Option<HoldFacts>>> = Rc::default();
    {
        let (term, seen) = (term.clone(), Rc::clone(&seen));
        redraw_hold_hook::install(move || {
            let mut t = term_lock(&term);
            let cursor = t.cursor();
            seen.set(Some((t.damage_epoch(), (cursor.row, cursor.col))));
            t.process(b"\r\nADVERSARY");
        });
    }
    app.redraw_window(wid);
    redraw_hold_hook::clear();
    let (epoch, cursor) = seen.get().expect("the hook ran after the hold");
    {
        let ws = &app.windows[&wid];
        let key = ws.last_present.as_ref().expect("the frame presented");
        assert_eq!(key.damage_epoch, epoch, "the key is the hold's epoch");
        assert_eq!(
            (key.cursor_row, key.cursor_col),
            (usize::from(cursor.0), usize::from(cursor.1)),
            "the key's caret is the hold's caret"
        );
        assert_eq!(
            ws.input_scratch.snapshot_seq, epoch,
            "the extraction is the hold's grid"
        );
    }
    assert!(
        !frame_text(&app, wid).contains("ADVERSARY"),
        "the late write is not in this frame"
    );
    assert!(has_damage(&app), "the late write is pending, not consumed");

    app.redraw_window(wid);
    assert!(!has_damage(&app), "the next frame consumed it");
    assert!(
        frame_text(&app, wid).contains("ADVERSARY"),
        "and presented it"
    );
    let now = term_lock(&term).cursor();
    let key = app.windows[&wid].last_present.as_ref().expect("presented");
    assert_eq!(
        (key.cursor_row, key.cursor_col),
        (usize::from(now.row), usize::from(now.col))
    );
}

/// With the cursor glow off an echo frame copies no cursor row into the probe buffers; with it on (control) it does.
#[test]
fn an_effects_off_echo_frame_copies_no_cursor_row() {
    let wid = WindowId(0);
    let mut off = scripted_app();
    off.redraw_window(wid);
    for echo in [&b"e"[..], b"c", b"h", b"o"] {
        feed(&off, echo);
        off.redraw_window(wid);
        let ws = &off.windows[&wid];
        assert!(
            ws.poof_row_buf.is_empty()
                && ws.poof_row_above_buf.is_empty()
                && ws.poof_row_below_buf.is_empty(),
            "an effects-off echo frame copies no row"
        );
    }

    let mut lit = lit_app();
    lit.redraw_window(wid);
    feed(&lit, b"echo");
    lit.redraw_window(wid);
    assert!(
        !lit.windows[&wid].poof_row_buf.is_empty(),
        "control: with the glow engine live the cursor row is probed"
    );
}

/// The single-pane present reuses the glow's RenderInput allocation across presents, and switching the effect off erases the glow.
#[test]
fn single_pane_glow_handoff_reuses_buffers_and_erases_on_disable() {
    let wid = WindowId(0);
    let mut app = lit_app();
    app.config.motion = Some("full".into());
    let (first_source, second_source) = {
        let ws = app.windows.get_mut(&wid).expect("scripted window");
        ws.focused = true;
        ws.fx.cursor_mut().glow_scratch.reserve(16_384);
        ws.input_scratch.cursor_glow_add.reserve(16_384);
        (
            ws.fx.cursor_mut().glow_scratch.as_ptr() as usize,
            ws.input_scratch.cursor_glow_add.as_ptr() as usize,
        )
    };
    assert_ne!(first_source, second_source, "two resident buffers");

    app.redraw_window(wid);
    let ws = &app.windows[&wid];
    assert!(
        !ws.input_scratch.cursor_glow_add.is_empty(),
        "control: a focused rainbow caret emits additive glow"
    );
    assert_eq!(
        ws.input_scratch.cursor_glow_add.as_ptr() as usize,
        first_source,
        "the renderer owns the emitted buffer, with no quad copy ({} quads, {} capacity)",
        ws.input_scratch.cursor_glow_add.len(),
        ws.input_scratch.cursor_glow_add.capacity()
    );

    feed(&app, b"x");
    app.redraw_window(wid);
    let ws = &app.windows[&wid];
    assert!(
        !ws.input_scratch.cursor_glow_add.is_empty(),
        "the next echo frame still receives fresh glow"
    );
    assert_eq!(
        ws.input_scratch.cursor_glow_add.as_ptr() as usize,
        second_source,
        "the second present reuses the prior input allocation"
    );

    app.config.cursor_trail = Some(false);
    app.redraw_window(wid);
    let ws = &app.windows[&wid];
    assert!(
        ws.input_scratch.cursor_glow_add.is_empty(),
        "disabling the effect must erase the prior glow on this frame"
    );
    let unchanged_before = ws.redraws_proved_unchanged;
    app.redraw_window(wid);
    assert!(
        app.windows[&wid].redraws_proved_unchanged > unchanged_before,
        "a settled frame still reaches the repaint early-out"
    );
}

/// Single-pane predictive local echo reconciles against the frame hold's own snapshot without taking a second terminal lock, so an echo arriving right after the hold is released never blanks the predicted glyph for a frame.
#[test]
fn a_predicted_frame_takes_one_terminal_lock_and_never_blanks_on_interleaved_echo() {
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.config.predictive_echo = Some("always".into());
    app.predict_mode_cache = None;
    feed(&app, b"$ ");
    app.redraw_window(wid);

    let now = crate::app_input::past_timeline_start();
    let coords = {
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let t = term_lock(&term);
        (
            t.content_scroll_state().coordinate_generation(),
            t.cols(),
            t.rows(),
        )
    };
    {
        let ws = app.windows.get_mut(&wid).expect("scripted window");
        ws.predictor.set_mode(crate::predict::PredictMode::Always);
        assert!(ws.predictor.predict_char_in_grid(
            'k',
            (0, 2),
            coords.0,
            (coords.1, coords.2),
            now,
        ));
    }
    let before = term_lock_acquisitions_on_this_thread();
    app.redraw_window_at(wid, now);
    let taken = term_lock_acquisitions_on_this_thread() - before;
    assert_eq!(
        taken, 1,
        "a single-pane frame with an active prediction takes ONE terminal hold (took {taken})"
    );
    assert!(
        app.windows[&wid].pred_shown,
        "the pending prediction was overlaid onto the frame"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.cells[0][2].ch, 'k',
        "the predicted glyph is visible at (0, 2)"
    );

    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let term = term.clone();
        redraw_hold_hook::install(move || {
            term_lock(&term).process(b"k");
        });
    }
    let before = term_lock_acquisitions_on_this_thread();
    app.redraw_window_at(wid, now);
    redraw_hold_hook::clear();
    let taken = term_lock_acquisitions_on_this_thread() - before;
    assert_eq!(
        taken, 2,
        "one hold by redraw_window + one hold by the test hook (took {taken})"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.cells[0][2].ch, 'k',
        "interleaved PTY echo after the frame hold must never blank the glyph"
    );
    assert!(
        has_damage(&app),
        "the interleaved echo's damage remains pending for the next frame"
    );

    let before = term_lock_acquisitions_on_this_thread();
    app.redraw_window_at(wid, now);
    let taken = term_lock_acquisitions_on_this_thread() - before;
    assert_eq!(taken, 1, "the confirmation frame takes ONE terminal hold");
    assert!(
        !has_damage(&app),
        "the confirmation frame consumed the echo damage"
    );
    assert!(
        app.windows[&wid].predictor.idle(),
        "the echoed glyph retired the prediction"
    );
    assert!(
        !app.windows[&wid].pred_shown,
        "no tentative overlay remains once the grid owns the glyph"
    );
    assert_eq!(
        app.windows[&wid].input_scratch.cells[0][2].ch, 'k',
        "the confirmed grid glyph is presented at (0, 2)"
    );
}

/// Type `ch` as a guess at the front terminal's cursor, at `born`.
fn arm_prediction(
    app: &mut crate::App,
    ch: char,
    born: std::time::Instant,
) -> crate::predict::LinkEstimate {
    arm_prediction_with(app, ch, born, None)
}

/// [`arm_prediction`] with `rule_off` switched off in both predictors it arms, through the predictor's debug-only negative-control seam.
fn arm_prediction_with(
    app: &mut crate::App,
    ch: char,
    born: std::time::Instant,
    rule_off: Option<crate::predict::Rule>,
) -> crate::predict::LinkEstimate {
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let (cursor, coords, grid) = {
        let term = term_lock(&term);
        let cur = term.cursor();
        (
            (cur.row, cur.col),
            term.content_scroll_state().coordinate_generation(),
            (term.cols(), term.rows()),
        )
    };
    let predictor = &mut app.windows.get_mut(&wid).unwrap().predictor;
    predictor.set_mode(crate::predict::PredictMode::Always);
    let mut unmeasured = crate::predict::Predictor::new(crate::predict::PredictMode::Always);
    if let Some(rule) = rule_off {
        predictor.disable_rule_for_test(rule);
        unmeasured.disable_rule_for_test(rule);
    }
    assert!(predictor.predict_char_in_grid(ch, cursor, coords, grid, born));
    assert!(unmeasured.predict_char_in_grid(ch, cursor, coords, grid, born));
    unmeasured.take_link_estimate()
}

/// The frame's observe reads a row the engine reorders (right-to-left text) at LOGICAL columns, so a guess's echo there is found where its key typed it, and confirms.
#[test]
fn prediction_reconcile_confirms_rtl_echo_in_logical_columns() {
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.config.predictive_echo = Some("always".into());
    app.predict_mode_cache = None;
    feed(&app, "אב".as_bytes());
    app.redraw_window(wid);
    let mut declining = crate::predict::Predictor::new(crate::predict::PredictMode::Always);
    assert!(
        !declining.predict_char_in_grid('ג', (0, 2), 0, (80, 24), std::time::Instant::now()),
        "a right-to-left glyph is declined"
    );
    assert!(declining.idle());
    let born = crate::app_input::past_timeline_start();
    arm_prediction_with(
        &mut app,
        'ג',
        born,
        Some(crate::predict::Rule::RightToLeftRows),
    );
    feed(&app, "ג".as_bytes());
    app.redraw_window_at(wid, born);
    let ws = app.windows.get_mut(&wid).unwrap();
    assert_eq!(
        ws.input_scratch.cells[0][0].ch, 'ג',
        "RTL echo is in visual order"
    );
    assert_eq!(
        ws.input_scratch.visual_col(0, 2),
        0,
        "logical and visual columns differ"
    );
    assert!(ws.predictor.idle());
    assert!(
        ws.predictor.take_link_estimate().has_sample(),
        "a real RTL echo confirms the prediction and records its RTT"
    );
}

#[test]
fn prediction_reconcile_never_confirms_uncommitted_ime_text() {
    let wid = WindowId(0);
    let mut app = scripted_app();
    app.config.predictive_echo = Some("always".into());
    app.predict_mode_cache = None;
    feed(&app, b"$ ");
    app.redraw_window(wid);
    let born = crate::app_input::past_timeline_start();
    let unmeasured = arm_prediction(&mut app, 'k', born);
    app.on_ime_preedit(wid, "k".into(), Some((1, 1)));
    app.redraw_window_at(wid, born);
    let ws = app.windows.get_mut(&wid).unwrap();
    assert!(!ws.predictor.idle(), "IME preedit has not reached the PTY");
    let estimate = ws.predictor.take_link_estimate();
    assert!(
        !estimate.has_sample(),
        "host-painted composition must not count as an echo RTT sample"
    );
    assert_eq!(
        estimate, unmeasured,
        "nothing past the pending guess's own echo debt"
    );
}

/// The single-pane path paints a state chain as the composed path does (`render-paint-prediction-chain`): every cell of its displayed state or none, with the chain's OWN cursor, which a pure motion moves with no cell painted and an erase leaves on the cell it vacated.
#[test]
fn a_single_pane_frame_paints_a_state_chain_with_its_cursor() {
    use crate::input::{InputEvent, Source};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let press = |key| InputEvent::Key {
        key,
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let wid = WindowId(0);
    for (key, on_glass) in [(NamedKey::ArrowLeft, 'b'), (NamedKey::Backspace, ' ')] {
        let mut app = scripted_app();
        app.config.predictive_echo = Some("always".into());
        app.predict_mode_cache = None;
        app.windows.get_mut(&wid).expect("window").focused = true;
        feed(&app, b"$ ");
        app.redraw_window(wid);
        let now = crate::app_input::past_timeline_start();
        crate::app_input::pin_input_clock(Some(now));
        for c in ['a', 'b'] {
            let _ = app.input(wid, press(Key::Character(c)), Source::Human);
            feed(&app, c.to_string().as_bytes());
            app.redraw_window_at(wid, now);
        }
        assert!(
            app.windows[&wid].predictor.idle(),
            "fixture: `ab` echoed and retired, the known line"
        );
        let _ = app.input(wid, press(Key::Named(key)), Source::Human);
        assert!(
            !app.windows[&wid].predictor.idle(),
            "{key:?}: armed as an edit op"
        );
        app.redraw_window_at(wid, now);
        let ws = &app.windows[&wid];
        assert!(ws.pred_shown, "{key:?}: the chain is on glass");
        assert_eq!(
            (ws.input_scratch.cursor_row, ws.input_scratch.cursor_col),
            (0, 3),
            "{key:?}: the chain's cursor, on `b`'s cell"
        );
        assert_eq!(
            ws.input_scratch.cells[0][3].ch, on_glass,
            "{key:?}: what the state shows on `b`'s cell"
        );
    }
}

/// The scroll's own frame reads the content witness's rows in the frame's one hold, after the translation (`CursorEffectScrollChange::translated_only`): a box redrawn in place under a scroll, as Claude Code's composer is when a tool result lands under a full transcript, keeps its band on the frame it is drawn, never on the row the scroll carried it to, still in one terminal hold.
#[test]
fn a_box_redrawn_in_place_under_a_scroll_keeps_its_band_in_the_scroll_s_one_hold() {
    let wid = WindowId(0);
    let mut app = lit_app();
    app.config.motion = Some("full".into());
    app.windows.get_mut(&wid).expect("scripted window").focused = true;
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let rows = term_lock(&term).grid().rows();
    let line = rows - 3;
    let row = line - 1;
    feed(&app, format!("\x1b[{line};1H").as_bytes());
    app.redraw_window(wid);
    let ribbon_cols = |app: &crate::App, row: u16| -> Vec<u16> {
        let mut cols: Vec<u16> = app.windows[&wid]
            .fx
            .cursor()
            .glow
            .v2_ribbon()
            .map(|r| {
                r.cells()
                    .iter()
                    .filter(|c| c.row == row && !c.leaving())
                    .map(|c| c.col)
                    .collect()
            })
            .unwrap_or_default();
        cols.sort_unstable();
        cols
    };
    for ch in "hello world".bytes() {
        app.windows
            .get_mut(&wid)
            .expect("scripted window")
            .fx
            .cursor_mut()
            .glow
            .note_typed_cells(std::time::Instant::now(), 1);
        feed(&app, &[ch]);
        app.redraw_window(wid);
    }
    app.redraw_window(wid);
    let lit: Vec<u16> = (0..11).collect();
    assert_eq!(ribbon_cols(&app, row), lit, "fixture: the typed band");
    feed(
        &app,
        format!(
            "\x1b[?2026h\x1b[{rows};1H\r\n\r\n\r\n\x1b[{};1H\x1b[2Kother text\
             \x1b[{line};1H\x1b[2Khello world\x1b[?2026l",
            line - 3
        )
        .as_bytes(),
    );
    let before = term_lock_acquisitions_on_this_thread();
    app.redraw_window(wid);
    let taken = term_lock_acquisitions_on_this_thread() - before;
    assert_eq!(
        ribbon_cols(&app, row),
        lit,
        "on the scroll's own frame the band stands under its text"
    );
    assert!(
        ribbon_cols(&app, row - 3).is_empty(),
        "nothing on the row the scroll carried it to: {:?}",
        ribbon_cols(&app, row - 3)
    );
    assert_eq!(
        taken, 1,
        "the scroll's frame takes ONE terminal hold (took {taken})"
    );
}

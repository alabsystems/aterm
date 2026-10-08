// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One terminal hold per single-pane frame: effect probes, grid extraction,
//! damage consume and cursor/colour facts come from one acquisition, so no
//! PTY write lands between them. Driven through `redraw_window` on a
//! scripted CPU surface.
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

/// An echo frame takes the terminal mutex once, with effects off and with
/// the cursor effect live.
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

/// A PTY write landing right after the frame's hold is released leaves the
/// frame coherent (key, extraction and glass all describe the grid before
/// the write); the write stays damaged and the next frame presents it.
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
            // Nothing has written since the hold: these are the frame's facts.
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

/// With the cursor glow off an echo frame copies no cursor row into the
/// probe buffers; with it on (control) it does.
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

/// The single-pane present reuses the glow's RenderInput allocation across
/// presents, and switching the effect off erases the glow.
#[test]
fn single_pane_glow_handoff_reuses_buffers_and_erases_on_disable() {
    let wid = WindowId(0);
    let mut app = lit_app();
    app.config.motion = Some("full".into());
    app.config.stream_fade = Some(false);
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

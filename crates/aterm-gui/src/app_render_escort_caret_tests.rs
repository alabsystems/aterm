// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flying head escorts the caret the pet escorts.
//!
//! A TUI like Claude Code hides the cursor once and repaints inside DEC-2026
//! brackets. Past the 150 ms hold a present shows a bracket still open, the
//! hidden cursor wherever the repaint stopped writing. The pet reads the last
//! finished present's caret there (`CompanionOwner::prepare`); the single-pane
//! host handed the flying head the raw cursor, so the head flew up the
//! transcript to the write position. Both bodies now read
//! `PetTick::escort_caret`, which also waits out a hidden cursor's move in a
//! program that brackets nothing until the cursor has rested.

use super::*;
use std::time::Duration;

const WID: WindowId = WindowId(0);
/// The composer's caret and the repaint's write position (0-based).
const CARET: (u16, u16) = (20, 10);
const WRITE: (u16, u16) = (3, 60);
const FRAME: Duration = Duration::from_millis(16);

fn feed(app: &App, bytes: &[u8], at: Instant) {
    let term = app
        .front_terminal(WID)
        .expect("front terminal")
        .term
        .clone();
    crate::term_lock(&term).process_at(
        bytes,
        aterm_core::terminal::ClockReading {
            monotonic: at,
            wall_ms: None,
        },
    );
}

/// The flying head's sprite: the one companion body (no words, no pet).
fn head(app: &App) -> Option<(i32, i32)> {
    app.windows[&WID]
        .input_scratch
        .free_sprites
        .iter()
        .max_by_key(|s| s.h)
        .map(|s| (s.x, s.y))
}

/// Present every frame from `*t` through `*t + span`.
fn run(app: &mut App, t: &mut Instant, span: Duration) {
    let end = *t + span;
    while *t + FRAME <= end {
        *t += FRAME;
        app.redraw_window_at(WID, *t);
    }
}

/// The head summoned (the collection hello) over a Claude-shaped screen: a
/// transcript, the composer's caret at [`CARET`], the cursor hidden. One
/// bracket opens, moves the hidden cursor to [`WRITE`] and writes there, and
/// stays open past the hold: every torn present keeps the head at the caret's
/// row. Negative control: the bracket closes with the hidden cursor left at
/// [`WRITE`] (a finished frame, where a hidden cursor is the caret), and the
/// head goes there.
#[test]
fn a_torn_repaint_keeps_the_flying_head_at_the_caret() {
    let mut app = App::headless_for_test();
    app.config.serious_mode = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty flying".into());
    app.config.motion = Some("full".into());
    app.config.trail_sounds = Some(false);
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    let _ = app.sparkle.take();
    {
        // The real redraw path end to end, through a scripted CPU surface.
        let ws = app.windows.get_mut(&WID).expect("window");
        ws.present = Some(crate::PresentTarget::Cpu {
            surface: crate::present::CpuSurface::Scripted(Default::default()),
        });
        ws.focused = true;
    }
    let (_, ch) = app.win_cell_size(WID);
    let ch = ch as i32;
    let row_of = |y: i32| (y + ch / 2).div_euclid(ch);

    let mut t = Instant::now();
    let mut screen = b"\x1b]11;#000000\x07".to_vec();
    for n in 0..19 {
        screen.extend_from_slice(format!("transcript line {n}\r\n").as_bytes());
    }
    screen.extend_from_slice(format!("\x1b[{};{}H\x1b[?25l", CARET.0 + 1, CARET.1 + 1).as_bytes());
    feed(&app, &screen, t);
    app.windows
        .get_mut(&WID)
        .expect("window")
        .fx
        .companions_mut()
        .cursor_cat
        .on_collect(t, aterm_effects::kitty_registry::KittyLook::default());
    run(&mut app, &mut t, Duration::from_millis(600));
    let (_, home_y) = head(&app).expect("the summoned head is on glass");
    let home = row_of(home_y);
    assert!(
        (home - i32::from(CARET.0)).abs() <= 2,
        "fixture: the head flies beside the hidden caret (row {home})"
    );

    feed(
        &app,
        format!(
            "\x1b[?2026h\x1b[{};{}Hrepainting the transcript",
            WRITE.0 + 1,
            WRITE.1 + 1
        )
        .as_bytes(),
        t,
    );
    // Past the hold every present is torn: the bracket is open and written.
    let opened = t;
    let mut torn = Vec::new();
    let end = t + Duration::from_millis(600);
    while t + FRAME <= end {
        t += FRAME;
        app.redraw_window_at(WID, t);
        if t.duration_since(opened) > SYNC_HOLD_CAP
            && let Some((_, y)) = head(&app)
        {
            torn.push(row_of(y));
        }
    }
    let term = app
        .front_terminal(WID)
        .expect("front terminal")
        .term
        .clone();
    assert!(
        crate::term_lock(&term).sync_frame_unfinished(),
        "fixture: the bracket is still open"
    );
    assert!(torn.len() > 10, "the open bracket presents torn ({torn:?})");
    assert!(
        torn.iter().all(|&row| (row - home).abs() <= 1),
        "a torn present moved the head off the caret toward the write \
         position (row {}): home row {home}, torn rows {torn:?}",
        WRITE.0
    );

    feed(&app, b"\x1b[?2026l", t);
    run(&mut app, &mut t, Duration::from_millis(600));
    let (_, y) = head(&app).expect("the head is still on glass");
    assert!(
        (row_of(y) - i32::from(WRITE.0)).abs() <= 2,
        "negative control: a finished frame's hidden cursor is the caret, \
         and the head flies to it (row {})",
        row_of(y)
    );
}

/// The same head over a program that never brackets its frames (an Ink app
/// without synchronized output, a TUI under tmux): the cursor hidden
/// throughout, each spinner repaint moves it to [`WRITE`], writes, and puts it
/// back, and a present lands mid-write once a repaint. The head took the
/// write position as the caret and flew seventeen rows up and back, one
/// present each way (review of `fix/kitty-jitter`, 2026-10-20). A hidden
/// cursor's move is taken once it has rested (`companion::HIDDEN_REST`): every
/// present keeps the head at the caret's row. Negative control: the hidden
/// cursor left at [`WRITE`] rests there, and the head goes to it.
#[test]
fn an_unbracketed_repaint_keeps_the_flying_head_at_the_caret() {
    let mut app = App::headless_for_test();
    app.config.serious_mode = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty flying".into());
    app.config.motion = Some("full".into());
    app.config.trail_sounds = Some(false);
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    let _ = app.sparkle.take();
    {
        let ws = app.windows.get_mut(&WID).expect("window");
        ws.present = Some(crate::PresentTarget::Cpu {
            surface: crate::present::CpuSurface::Scripted(Default::default()),
        });
        ws.focused = true;
    }
    let (_, ch) = app.win_cell_size(WID);
    let ch = ch as i32;
    let row_of = |y: i32| (y + ch / 2).div_euclid(ch);

    let mut t = Instant::now();
    let mut screen = b"\x1b]11;#000000\x07".to_vec();
    for n in 0..19 {
        screen.extend_from_slice(format!("transcript line {n}\r\n").as_bytes());
    }
    screen.extend_from_slice(format!("\x1b[{};{}H\x1b[?25l", CARET.0 + 1, CARET.1 + 1).as_bytes());
    feed(&app, &screen, t);
    app.windows
        .get_mut(&WID)
        .expect("window")
        .fx
        .companions_mut()
        .cursor_cat
        .on_collect(t, aterm_effects::kitty_registry::KittyLook::default());
    run(&mut app, &mut t, Duration::from_millis(600));
    let (_, home_y) = head(&app).expect("the summoned head is on glass");
    let home = row_of(home_y);
    assert!(
        (home - i32::from(CARET.0)).abs() <= 2,
        "fixture: the head flies beside the hidden caret (row {home})"
    );

    let mut rows = Vec::new();
    for _ in 0..10 {
        feed(
            &app,
            format!("\x1b[{};{}Hspinner", WRITE.0 + 1, WRITE.1 + 1).as_bytes(),
            t,
        );
        t += FRAME;
        app.redraw_window_at(WID, t);
        rows.extend(head(&app).map(|(_, y)| row_of(y)));
        feed(
            &app,
            format!("\x1b[{};{}H", CARET.0 + 1, CARET.1 + 1).as_bytes(),
            t,
        );
        for _ in 0..5 {
            t += FRAME;
            app.redraw_window_at(WID, t);
            rows.extend(head(&app).map(|(_, y)| row_of(y)));
        }
    }
    assert!(rows.len() >= 50, "the head stays on glass ({rows:?})");
    assert!(
        rows.iter().all(|&row| (row - home).abs() <= 1),
        "a mid-write present moved the head toward the write position (row {}): \
         home row {home}, rows {rows:?}",
        WRITE.0
    );

    feed(
        &app,
        format!("\x1b[{};{}Hspinner", WRITE.0 + 1, WRITE.1 + 1).as_bytes(),
        t,
    );
    run(&mut app, &mut t, Duration::from_millis(600));
    let (_, y) = head(&app).expect("the head is still on glass");
    assert!(
        (row_of(y) - i32::from(WRITE.0)).abs() <= 2,
        "negative control: a hidden cursor that rests is the caret, and the \
         head flies to it (row {})",
        row_of(y)
    );
}

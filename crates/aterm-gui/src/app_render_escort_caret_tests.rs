// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that single-pane, split-pane, and capture renders use the escort caret.

use super::*;
use std::time::Duration;

const WID: WindowId = WindowId(0);
const CARET: (u16, u16) = (20, 10);
const WRITE: (u16, u16) = (3, 30);
const FRAME: Duration = Duration::from_millis(16);

/// Which host arm draws the head.
#[derive(Clone, Copy, Debug)]
enum Arm {
    Single,
    Split,
}

const ARMS: [Arm; 2] = [Arm::Single, Arm::Split];

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

fn head(app: &App) -> Option<(i32, i32)> {
    app.windows[&WID]
        .input_scratch
        .free_sprites
        .iter()
        .max_by_key(|s| s.h)
        .map(|s| (s.x, s.y))
}

fn run(app: &mut App, t: &mut Instant, span: Duration) {
    let end = *t + span;
    while *t + FRAME <= end {
        *t += FRAME;
        app.redraw_window_at(WID, *t);
    }
}

fn unfinished(app: &App) -> bool {
    let term = app
        .front_terminal(WID)
        .expect("front terminal")
        .term
        .clone();
    crate::term_lock(&term).sync_frame_unfinished()
}

fn summoned(arm: Arm) -> (App, Instant, i32) {
    let mut app = App::headless_for_test();
    app.config.serious_mode = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty flying".into());
    app.config.motion = Some("full".into());
    app.config.trail_sounds = Some(false);
    app.kitty_cursor_enabled_cache = None;
    if let Arm::Split = arm {
        app.split_active_stub_tab(WID);
    }
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
    (app, t, ch as i32)
}

fn row_of(y: i32, ch: i32) -> i32 {
    (y + ch / 2).div_euclid(ch)
}

fn home_row(app: &App, ch: i32, arm: Arm) -> i32 {
    let (_, home_y) = head(app).unwrap_or_else(|| {
        panic!("{arm:?}: the summoned head is on glass beside the hidden caret")
    });
    let home = row_of(home_y, ch);
    assert!(
        (home - i32::from(CARET.0)).abs() <= 2,
        "{arm:?}: the head flies beside the hidden caret (row {home})"
    );
    home
}

#[test]
fn a_torn_repaint_keeps_the_flying_head_at_the_caret() {
    for arm in ARMS {
        let (mut app, mut t, ch) = summoned(arm);
        let home = home_row(&app, ch, arm);
        feed(
            &app,
            format!("\x1b[?2026h\x1b[{};{}Hrepaint", WRITE.0 + 1, WRITE.1 + 1).as_bytes(),
            t,
        );
        let opened = t;
        let mut torn = Vec::new();
        let end = t + Duration::from_millis(600);
        while t + FRAME <= end {
            t += FRAME;
            app.redraw_window_at(WID, t);
            if t.duration_since(opened) > SYNC_HOLD_CAP
                && let Some((_, y)) = head(&app)
            {
                torn.push(row_of(y, ch));
            }
        }
        assert!(
            unfinished(&app),
            "{arm:?}: fixture: the bracket is still open"
        );
        assert!(
            torn.len() > 10,
            "{arm:?}: the open bracket presents torn ({torn:?})"
        );
        assert!(
            torn.iter().all(|&row| (row - home).abs() <= 1),
            "{arm:?}: a torn present moved the head off the caret toward the write \
             position (row {}): home row {home}, torn rows {torn:?}",
            WRITE.0
        );

        feed(&app, b"\x1b[?2026l", t);
        run(&mut app, &mut t, Duration::from_millis(600));
        let (_, y) = head(&app).expect("the head is still on glass");
        assert!(
            (row_of(y, ch) - i32::from(WRITE.0)).abs() <= 2,
            "{arm:?}: negative control: a finished frame's hidden cursor is the caret, \
             and the head flies to it (row {})",
            row_of(y, ch)
        );
    }
}

#[test]
fn an_unbracketed_repaint_keeps_the_flying_head_at_the_caret() {
    for arm in ARMS {
        let (mut app, mut t, ch) = summoned(arm);
        let home = home_row(&app, ch, arm);
        let mut rows = Vec::new();
        for _ in 0..10 {
            feed(
                &app,
                format!("\x1b[{};{}Hspinner", WRITE.0 + 1, WRITE.1 + 1).as_bytes(),
                t,
            );
            t += FRAME;
            app.redraw_window_at(WID, t);
            rows.extend(head(&app).map(|(_, y)| row_of(y, ch)));
            feed(
                &app,
                format!("\x1b[{};{}H", CARET.0 + 1, CARET.1 + 1).as_bytes(),
                t,
            );
            for _ in 0..5 {
                t += FRAME;
                app.redraw_window_at(WID, t);
                rows.extend(head(&app).map(|(_, y)| row_of(y, ch)));
            }
        }
        assert!(
            rows.len() >= 50,
            "{arm:?}: the head stays on glass ({rows:?})"
        );
        assert!(
            rows.iter().all(|&row| (row - home).abs() <= 1),
            "{arm:?}: a mid-write present moved the head toward the write position \
             (row {}): home row {home}, rows {rows:?}",
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
            (row_of(y, ch) - i32::from(WRITE.0)).abs() <= 2,
            "{arm:?}: negative control: a hidden cursor that rests is the caret, and \
             the head flies to it (row {})",
            row_of(y, ch)
        );
    }
}

#[test]
fn a_capture_draws_the_flying_head_where_the_glass_has_it() {
    let capture = |app: &mut App, t: Instant| {
        app.windows
            .get_mut(&WID)
            .expect("window")
            .input_scratch
            .free_sprites
            .clear();
        app.capture_words(WID, t);
        head(app)
    };
    for arm in ARMS {
        let (mut app, mut t, ch) = summoned(arm);
        let home = home_row(&app, ch, arm);
        let (_, y) = capture(&mut app, t).unwrap_or_else(|| {
            panic!("{arm:?}: the capture draws the head the glass has beside the hidden caret")
        });
        assert_eq!(
            row_of(y, ch),
            home,
            "{arm:?}: the capture's head is the glass's"
        );

        feed(
            &app,
            format!("\x1b[?2026h\x1b[{};{}Hrepaint", WRITE.0 + 1, WRITE.1 + 1).as_bytes(),
            t,
        );
        run(&mut app, &mut t, SYNC_HOLD_CAP + Duration::from_millis(100));
        assert!(
            unfinished(&app),
            "{arm:?}: fixture: the bracket is still open"
        );
        let (_, y) = capture(&mut app, t).expect("the torn capture draws the head");
        assert!(
            (row_of(y, ch) - home).abs() <= 1,
            "{arm:?}: the torn capture's head left the caret (row {} vs {home})",
            row_of(y, ch)
        );

        feed(&app, b"\x1b[?2026l", t);
        run(&mut app, &mut t, Duration::from_millis(600));
        let (_, glass) = head(&app).expect("the head is still on glass");
        let (_, y) = capture(&mut app, t).expect("the capture draws the head");
        assert!(
            (row_of(y, ch) - i32::from(WRITE.0)).abs() <= 2 && row_of(y, ch) == row_of(glass, ch),
            "{arm:?}: negative control: the finished frame's caret is the write \
             position, for the capture as for the glass (capture row {}, glass row {})",
            row_of(y, ch),
            row_of(glass, ch)
        );
    }
}

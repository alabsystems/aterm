// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::scripted_redraw_fixture::{feed, scripted_app};
use crate::{App, WindowId};
use aterm_core::terminal::ClockReading;
use std::time::{Duration, Instant};

const CARET: (u16, u16) = (10, 3);
const WRITE_CURSOR: (u16, u16) = (2, 47);

fn lit_app() -> App {
    let mut app = scripted_app();
    app.config.serious_mode = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    app.config.motion = Some("full".into());
    app.config.trail_sounds = Some(false);
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    app.windows.get_mut(&WindowId(0)).expect("window").focused = true;
    app
}

fn feed_at(app: &App, bytes: &[u8], monotonic: Instant) {
    let term = app
        .front_terminal(WindowId(0))
        .expect("front terminal")
        .term
        .clone();
    crate::term_lock(&term).process_at(
        bytes,
        ClockReading {
            monotonic,
            wall_ms: None,
        },
    );
}

#[derive(Clone, Copy, Debug)]
enum Tear {
    PastTheHoldCap,
    PastTheTerminalTimeout,
    Finished,
}

type Facts = (Option<(u16, u16)>, u64, u64);

fn run(tear: Tear) -> (Facts, Facts) {
    let wid = WindowId(0);
    let mut app = lit_app();
    let t0 = Instant::now();
    feed_at(
        &app,
        format!("\x1b[2J\x1b[{};{}H", CARET.0 + 1, CARET.1 + 1).as_bytes(),
        t0,
    );
    app.redraw_window(wid);
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        let now = Instant::now();
        ws.fx.cursor_mut().glow.note_typed_cells(now, 1);
        ws.fx.cursor_mut().trail.note_typed(now);
    }
    let facts = |app: &App| -> Facts {
        let glow = &app.windows[&wid].fx.cursor().glow;
        (
            glow.cursor_anchor(),
            glow.spawns(),
            glow.admission_tally().licensed,
        )
    };
    let before = facts(&app);
    let repaint = format!(
        "\x1b[?2026h\x1b[?25l\x1b[H\x1b[{};1Htranscript row\x1b[{};{}H",
        WRITE_CURSOR.0 + 1,
        WRITE_CURSOR.0 + 1,
        WRITE_CURSOR.1 + 1
    );
    match tear {
        Tear::PastTheHoldCap => {
            feed(&app, repaint.as_bytes());
            app.redraw_window(wid); // the rising edge arms the hold
            assert_eq!(facts(&app), before, "{tear:?}: the held frame runs nothing");
            std::thread::sleep(super::SYNC_HOLD_CAP + Duration::from_millis(20));
        }
        Tear::PastTheTerminalTimeout => {
            feed_at(&app, repaint.as_bytes(), t0);
            feed_at(&app, b"x\x08", t0 + Duration::from_millis(1_200));
        }
        Tear::Finished => feed(&app, format!("{repaint}\x1b[?2026l").as_bytes()),
    }
    {
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let t = crate::term_lock(&term);
        let c = t.cursor();
        assert_eq!((c.row, c.col), WRITE_CURSOR, "{tear:?}: the write cursor");
        assert!(!t.cursor_visible(), "{tear:?}: hidden");
        match tear {
            Tear::PastTheHoldCap => assert!(t.modes().synchronized_output()),
            Tear::PastTheTerminalTimeout => assert!(
                !t.modes().synchronized_output(),
                "the terminal's timeout cleared the mode"
            ),
            Tear::Finished => assert!(!t.sync_frame_unfinished()),
        }
    }
    app.redraw_window(wid);
    assert!(
        app.windows[&wid].last_present.is_some(),
        "{tear:?}: the frame presented"
    );
    (before, facts(&app))
}

#[test]
fn an_unfinished_frames_hidden_write_cursor_moves_no_engine_through_the_present() {
    for tear in [
        Tear::PastTheHoldCap,
        Tear::PastTheTerminalTimeout,
        Tear::Finished,
    ] {
        let (before, after) = run(tear);
        assert_eq!(before.0, Some(CARET), "{tear:?}: seeded at the caret");
        if matches!(tear, Tear::Finished) {
            assert_eq!(
                after.0,
                Some(WRITE_CURSOR),
                "the hidden caret is the anchor"
            );
        } else {
            assert_eq!(
                after, before,
                "{tear:?}: the write cursor is no caret (anchor, spawns, licensed)"
            );
        }
    }
}

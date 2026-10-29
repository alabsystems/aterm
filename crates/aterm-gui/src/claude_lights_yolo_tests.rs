// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mode light as the mode's one home where the glass rewrites Claude Code's mode row
//! (`yolo`, `nag mode`), under the footer's gate.

use std::time::{Duration, Instant};

use aterm_agent::harness::lights::{self, Light};
use aterm_window::event::{ElementState, MouseButton};

use super::gesture_tests::{
    BYPASS, Hosted, PGID, app, cell_at, draw_claude, feed, footer_row, frame, hit, host, painted,
    pty, publish_facts, px_of, row_text,
};
use super::{Chip, cells_of};
use crate::{App, WindowId};

const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";

fn claude_app(
    cols: u16,
    rewrite: bool,
) -> (App, WindowId, u64, std::os::unix::net::UnixStream, Hosted) {
    let (mut app, wid, session, reader) = app();
    let hosted = host(&mut app);
    if cols != 80 {
        assert!(app.apply_term_resize(wid, 24, cols));
    }
    publish_facts(&app, session);
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
        timeline.set_program(PGID, Some("claude".into()));
    }
    if rewrite {
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("the claim");
    }
    (app, wid, session, reader, hosted)
}

/// `mode` as Claude Code draws it in a pane of `cols`: whole, or cut with `…` at its end.
fn mode_row_in(mode: &str, cols: u16) -> String {
    let room = usize::from(cols) - 1;
    if mode.chars().count() <= room {
        return mode.to_owned();
    }
    mode.chars()
        .take(room - 1)
        .chain(std::iter::once('\u{2026}'))
        .collect()
}

fn light_cells(app: &App, wid: WindowId) -> (Vec<(usize, usize)>, String) {
    let cells: Vec<(usize, usize)> = app.windows[&wid]
        .claude_lights
        .hits
        .iter()
        .filter(|h| h.chip == Some(Chip::Light(Light::Mode)))
        .map(|h| (h.frame_row, h.col))
        .collect();
    let text = cells.iter().map(|&at| cell_at(app, wid, at).ch).collect();
    (cells, text)
}

fn mode_glass(app: &App, wid: WindowId) -> String {
    row_text(app, wid, footer_row(app, wid) + 1)
}

/// THE MODE LIGHT IS THE MODE'S ONE HOME: where the glass rewrites the mode row (the pill and
/// the key hints dropped, no job to keep: a blank row) the light names manual mode `nag mode`
/// in one run of cells, at every width the whole row is drawn, pushing neither Autopilot nor
/// the model off, and its last cell is still the click that asks for the next mode. Bypass, an
/// expected mode no light shows at rest on Claude's own row, reads `yolo` alone there. A row
/// Claude cut where only a key hint can be cut (`? for shor…`) is rewritten as the whole one; a
/// row cut where a job may be stands as drawn, the light naming `manual`. CONTROL: unhosted,
/// Claude's row and the light's own word.
#[test]
fn the_mode_light_is_the_modes_one_home_on_a_rewritten_row() {
    for cols in [120_u16, 80, 50] {
        for rewrite in [true, false] {
            let (mut app, wid, session, mut reader, _host) = claude_app(cols, rewrite);
            draw_claude(&app, session, &mode_row_in(MANUAL, cols));
            frame(&mut app, wid);
            let what = format!("{cols} columns, rewrite={rewrite}");
            let row = mode_glass(&app, wid);
            assert_eq!(
                row.trim().is_empty(),
                rewrite,
                "{what}: the gate's witness, the mode row: {row:?}"
            );
            let word = if rewrite {
                "\u{23F8} nag mode"
            } else {
                "\u{23F8} manual"
            };
            let (cells, text) = light_cells(&app, wid);
            let rule = row_text(&app, wid, footer_row(&app, wid));
            assert_eq!(text, word, "{what}: {rule:?}");
            assert_eq!(cells.len(), cells_of(word), "{what}: one target per cell");
            assert!(
                cells.windows(2).all(|w| w[1].1 == w[0].1 + 1),
                "{what}: one run of cells"
            );
            assert!(
                painted(&app, wid, Chip::Autopilot),
                "{what}: the longer word pushes no chip off: {rule:?}"
            );
            assert!(
                rule.contains("\u{25C6} Opus 5.5"),
                "{what}: nor the model: {rule:?}"
            );
            let last = *cells.last().expect("the light");
            let (x, y) = px_of(&app, wid, last);
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
            assert_eq!(
                pty(&mut reader, lights::SHIFT_TAB.len()),
                lights::SHIFT_TAB,
                "{what}: the word's last cell is the click target"
            );
        }
    }
    for rewrite in [true, false] {
        let (mut app, wid, session, _reader, _host) = claude_app(120, rewrite);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let (_, text) = light_cells(&app, wid);
        assert_eq!(
            text,
            if rewrite { "yolo" } else { "" },
            "bypass at rest, rewrite={rewrite}"
        );
    }
    let jobs = "  \u{23F8} manual mode on \u{00B7} 2 monitors \u{00B7} 1 shell";
    for (mode, rewritten) in [(MANUAL, true), (jobs, false)] {
        let (mut app, wid, session, _reader, _host) = claude_app(33, true);
        let cut = mode_row_in(mode, 33);
        assert!(cut.ends_with('\u{2026}'), "Claude's own cut: {cut:?}");
        draw_claude(&app, session, &cut);
        frame(&mut app, wid);
        let glass = mode_glass(&app, wid);
        if rewritten {
            assert!(glass.trim().is_empty(), "a cut key hint goes: {glass:?}");
            assert_eq!(light_cells(&app, wid).1, "\u{23F8} nag mode", "{cut:?}");
        } else {
            assert_eq!(glass.trim_end(), cut, "a cut row stands");
            assert_eq!(light_cells(&app, wid).1, "\u{23F8} manual", "{cut:?}");
        }
    }
}

/// THE MODE LIGHT'S TITLES NAME BYPASS AND MANUAL AS THE LIGHT DOES: where the row is rewritten
/// the hover says `click for yolo or auto`, a stop says `stopped in nag mode`, and the arrival
/// reads `yolo`; on Claude's own row they say `bypass` and `manual mode`.
#[test]
fn the_mode_lights_titles_name_bypass_and_manual_as_the_light_does() {
    for rewrite in [true, false] {
        let (on, off, stopped, arrived) = if rewrite {
            ("yolo", "nag mode", "stopped in nag mode", "yolo")
        } else {
            (
                "bypass",
                "manual",
                "stopped in manual mode",
                "\u{23F5}\u{23F5} bypass",
            )
        };
        let (mut app, wid, session, mut reader, _host) = claude_app(120, rewrite);
        draw_claude(&app, session, MANUAL);
        frame(&mut app, wid);
        let (x, y) = px_of(&app, wid, hit(&app, wid, Light::Mode));
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        let rule = row_text(&app, wid, footer_row(&app, wid));
        assert!(
            rule.contains(&format!(
                "\u{23F8} {off}  Permission mode: click for {on} or auto"
            )),
            "rewrite={rewrite}: the hover: {rule:?}"
        );
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .and_then(|s| s.pending.as_mut())
            .expect("in flight")
            .deadline = Instant::now() - Duration::from_millis(1);
        app.on_claude_footer_changed(session);
        frame(&mut app, wid);
        let rule = row_text(&app, wid, footer_row(&app, wid));
        assert!(
            rule.contains(&format!("Permission mode: {stopped}")),
            "rewrite={rewrite}: the stop: {rule:?}"
        );
        let (x, y) = px_of(&app, wid, hit(&app, wid, Light::Mode));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
        feed(
            &app,
            session,
            format!("\x1b[24;1H\x1b[2K{BYPASS}\x1b[22;3H").as_bytes(),
        );
        frame(&mut app, wid);
        app.on_claude_footer_changed(session);
        frame(&mut app, wid);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "never pressed on from bypass"
        );
        let rule = row_text(&app, wid, footer_row(&app, wid));
        assert!(
            rule.contains(&format!("{arrived}  Permission mode: {on} ")),
            "rewrite={rewrite}: the arrival: {rule:?}"
        );
        let row = mode_glass(&app, wid);
        assert_eq!(
            row.trim().is_empty(),
            rewrite,
            "rewrite={rewrite}: the gate's witness: {row:?}"
        );
    }
}

/// THE MODE LIGHT'S WORD FOLLOWS THE REWRITE THE GLASS TAKES: with the cursor on the mode row
/// the row stands as Claude drew it and the light says `manual`; the cursor back at the caret,
/// the row is rewritten and the light says `nag mode`.
#[test]
fn the_mode_lights_word_follows_the_rewrite_the_glass_takes() {
    let (mut app, wid, session, _reader, _host) = claude_app(120, true);
    draw_claude(&app, session, MANUAL);
    feed(&app, session, b"\x1b[24;5H");
    frame(&mut app, wid);
    let row = mode_glass(&app, wid);
    assert!(
        row.starts_with("  \u{23F8} manual mode on"),
        "the row as Claude drew it: {row:?}"
    );
    let (_, text) = light_cells(&app, wid);
    assert_eq!(text, "\u{23F8} manual", "the light names it so too");
    feed(&app, session, b"\x1b[22;3H");
    frame(&mut app, wid);
    let row = mode_glass(&app, wid);
    assert!(row.trim().is_empty(), "{row:?}");
    let (_, text) = light_cells(&app, wid);
    assert_eq!(text, "\u{23F8} nag mode");
}

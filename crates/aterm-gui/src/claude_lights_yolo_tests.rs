// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mode-light YOLO relabeling tests under the Claude Code footer gate.

use std::time::{Duration, Instant};

use aterm_agent::harness::lights::{self, Light};
use aterm_window::event::{ElementState, MouseButton};

use super::gesture_tests::{
    Hosted, PGID, app, cell_at, draw_claude, feed, footer_row, frame, hit, host, painted, pty,
    publish_facts, px_of, row_text,
};
use super::{Chip, cells_of};
use crate::{App, WindowId};

const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";

fn claude_app(
    cols: u16,
    relabel: bool,
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
    if relabel {
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

fn mode_row_in(mode: &str, cols: u16) -> String {
    mode.chars().take(usize::from(cols) - 1).collect()
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

fn pill_glass(app: &App, wid: WindowId) -> String {
    row_text(app, wid, footer_row(app, wid) + 1)
}

#[test]
fn the_mode_light_names_manual_as_the_relabelled_pill_does_in_every_width() {
    for cols in [120_u16, 80, 50, 33] {
        for relabel in [true, false] {
            let (mut app, wid, session, mut reader, _host) = claude_app(cols, relabel);
            draw_claude(&app, session, &mode_row_in(MANUAL, cols));
            frame(&mut app, wid);
            let what = format!("{cols} columns, relabel={relabel}");
            let pill = pill_glass(&app, wid);
            assert_eq!(
                pill.starts_with("  YOLO off"),
                relabel,
                "{what}: the gate's witness, Claude's pill: {pill:?}"
            );
            let word = if relabel {
                "\u{23F8} YOLO off"
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
    for relabel in [true, false] {
        let (mut app, wid, session, _reader, _host) = claude_app(30, relabel);
        draw_claude(&app, session, &mode_row_in(MANUAL, 30));
        frame(&mut app, wid);
        assert_eq!(
            painted(&app, wid, Light::Mode),
            !relabel,
            "30 columns, relabel={relabel}"
        );
        assert!(painted(&app, wid, Chip::Autopilot), "relabel={relabel}");
    }
}

#[test]
fn the_mode_lights_titles_name_bypass_and_manual_as_the_pill_does() {
    for relabel in [true, false] {
        let (on, off, stopped) = if relabel {
            ("YOLO on", "YOLO off", "stopped in YOLO off")
        } else {
            ("bypass", "manual", "stopped in manual mode")
        };
        let (mut app, wid, session, mut reader, _host) = claude_app(120, relabel);
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
            "relabel={relabel}: the hover: {rule:?}"
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
            "relabel={relabel}: the stop: {rule:?}"
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
            rule.contains(&format!("\u{23F5}\u{23F5} {on}  Permission mode: {on} ")),
            "relabel={relabel}: the arrival: {rule:?}"
        );
        let pill = pill_glass(&app, wid);
        assert_eq!(
            pill.starts_with("  YOLO on"),
            relabel,
            "relabel={relabel}: the gate's witness: {pill:?}"
        );
    }
}

#[test]
fn the_mode_lights_word_follows_the_relabel_the_glass_takes() {
    let (mut app, wid, session, _reader, _host) = claude_app(120, true);
    draw_claude(&app, session, MANUAL);
    feed(&app, session, b"\x1b[24;5H");
    frame(&mut app, wid);
    let pill = pill_glass(&app, wid);
    assert!(
        pill.starts_with("  \u{23F8} manual mode on"),
        "the pill as Claude drew it: {pill:?}"
    );
    let (_, text) = light_cells(&app, wid);
    assert_eq!(text, "\u{23F8} manual", "the light names it so too");
    feed(&app, session, b"\x1b[22;3H");
    frame(&mut app, wid);
    let pill = pill_glass(&app, wid);
    assert!(pill.starts_with("  YOLO off"), "{pill:?}");
    let (_, text) = light_cells(&app, wid);
    assert_eq!(text, "\u{23F8} YOLO off");
}

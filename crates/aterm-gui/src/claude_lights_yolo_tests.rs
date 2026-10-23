// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mode light names the state as Claude's relabelled pill does (review
//! C8): under the pill relabel's gate (`claude_footer::relabels_pill`: a
//! Claude Code pane whose composer border aterm draws) bypass reads `⏵⏵ YOLO
//! on` and manual `⏸ YOLO off` on the light, its titles with it; outside the
//! gate, Claude's words.

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

/// A headless App `cols` wide whose one session is Claude Code's, its facts
/// published, under a harness host (the Autopilot chip beside the light) —
/// and, where `relabel`, this window's host holding the session's claim:
/// Cruise's glass is worn, so the composer border is drawn and the pill
/// relabel's gate is open. Without the claim the border, and the gate, are
/// shut.
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

/// Claude's `mode` row cut to a pane `cols` wide, as a narrow pane cuts its
/// hint (so the row never wraps the engine's last line).
fn mode_row_in(mode: &str, cols: u16) -> String {
    mode.chars().take(usize::from(cols) - 1).collect()
}

/// The light's painted cells this frame: their frame row and columns, and
/// the glass text under them.
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

/// Claude's own mode row as the glass shows it (the pane's last row).
fn pill_glass(app: &App, wid: WindowId) -> String {
    row_text(app, wid, footer_row(app, wid) + 1)
}

/// THE LIGHT'S WORD FOLLOWS THE PILL, AND KEEPS ITS ROOM (review C8). In
/// manual mode under the relabel's gate the pill reads `YOLO off` and the
/// light beside it `⏸ YOLO off` — ten cells for `⏸ manual`'s eight — at
/// 120, 80, 50 and 33 columns. MEASURED (this test's own setup: the
/// Autopilot chip `∞`, the light, the owner's facts): every width keeps both
/// chips and the model (`◆ Opus 5.5 xhigh` from 50 up, `◆ Opus 5.5` at 33),
/// the two extra cells taken from the rule's run — `─── ∞ ⏸ YOLO off ─────
/// ◆ Opus 5.5 ─` at 33. Narrower, the ladder gives the LIGHT up first, never
/// another chip: `⏸ YOLO off` stands down to 31 columns and `⏸ manual` to
/// 29, while `∞` stands down to 20 under either word. The click target is
/// the word as drawn: every painted cell of it, its last included, presses
/// the light. NEGATIVE CONTROL: outside the gate (no claim, no border) the
/// pill and the light keep Claude's `manual`.
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

/// THE LIGHT'S TITLES NAME BYPASS AND MANUAL AS THE PILL DOES. Hovered in
/// manual mode it offers `click for YOLO on or auto`; a press unanswered past
/// its deadline stops `in YOLO off`; and a return that reaches bypass says
/// `⏵⏵ YOLO on  Permission mode: YOLO on` on the chip as the pill reads
/// `YOLO on` under it. NEGATIVE CONTROL: outside the gate each says Claude's
/// words — `click for bypass or auto`, `stopped in manual mode`, `⏵⏵ bypass
/// Permission mode: bypass`.
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

/// THE LIGHT NAMES THE PILL AS THE GLASS SHOWS IT, NOT AS THE GATE ALLOWS
/// (review G2): under the relabel's gate a pill the glass leaves as Claude
/// drew it — here the cursor on its row, where the relabel would move cells
/// under the caret — keeps Claude's `⏸ manual mode on`, and the light beside
/// it says `⏸ manual` too: one state, one name on screen. CONTROL: the
/// cursor back at the composer, the same frame path relabels the pill and
/// the light says `⏸ YOLO off`.
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

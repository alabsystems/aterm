// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mode light as the mode's one home where the glass rewrites Claude Code's mode row
//! (`yolo`, `lame`), under the footer's gate: `yolo` in the glass's value ink, a click on it
//! turning it `lame`, and titles that say what each mode does.

use std::time::{Duration, Instant};

use aterm_agent::harness::lights::{self, Goal, Light, Mode};
use aterm_window::event::{ElementState, MouseButton};

use super::gesture_tests::{
    AUTO, BYPASS, Hosted, PGID, a_minute_ago, app, cell_at, check_mode_toggle_transition,
    draw_claude, drive_mode_toggle_action, feed, footer_row, frame, hit, host, interval, painted,
    press_at, press_not_due, pty, publish_facts, px_of, redraw_mode_row, row_text, value_ink,
};
use super::{Chip, Refusal, TITLE_CLICK, cells_of};
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
/// the key hints dropped, no job to keep: a blank row) the light names manual mode `lame`, a word
/// with no glyph, in one run of cells, at every width the whole row is drawn, pushing neither
/// Autopilot nor the model off, and its last cell is still the click that asks for the next mode. Bypass, an
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
            let word = if rewrite { "lame" } else { "\u{23F8} manual" };
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
                "{what}: the word pushes no chip off: {rule:?}"
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
        let (mut app, wid, session, _reader, _host) = claude_app(36, true);
        let cut = mode_row_in(mode, 36);
        assert!(cut.ends_with('\u{2026}'), "Claude's own cut: {cut:?}");
        draw_claude(&app, session, &cut);
        frame(&mut app, wid);
        let glass = mode_glass(&app, wid);
        if rewritten {
            assert!(glass.trim().is_empty(), "a cut key hint goes: {glass:?}");
            assert_eq!(light_cells(&app, wid).1, "lame", "{cut:?}");
        } else {
            assert_eq!(glass.trim_end(), cut, "a cut row stands");
            assert_eq!(light_cells(&app, wid).1, "\u{23F8} manual", "{cut:?}");
        }
    }
}

/// THE MODE LIGHT'S TITLES SAY WHAT EACH MODE DOES AND WHERE A CLICK GOES: the hover on manual
/// says what it does ([`Mode::gist`]) and where a click goes (the click clause and
/// [`Goal::words`]); a stop says its reason, the mode it stopped in named as the glass names it
/// (`Refusal::says`), and where a click goes from there; the arrival at bypass says what bypass
/// does and where a click goes. Where the row is rewritten, and only there, a title names Claude's
/// own mode ([`Mode::claude_name`]) beside aterm's word for it (`yolo`, `lame`): on Claude's own
/// row the chip already wears Claude's word, which no title repeats. Each expectation is built
/// from its producer, never from the sentence.
#[test]
fn the_mode_lights_titles_say_what_each_mode_does_and_where_a_click_goes() {
    let first = a_minute_ago();
    let apart = interval() + Duration::from_millis(1);
    for rewrite in [true, false] {
        let clicks = |goal: Goal| format!("{TITLE_CLICK}{}", goal.words(rewrite));
        let says = |rule: &str, mode: Mode, what: &str| {
            assert!(
                rule.contains(mode.gist()),
                "rewrite={rewrite}: {what} says what {mode:?} does: {rule:?}"
            );
            assert_eq!(
                rule.contains(mode.claude_name()),
                rewrite,
                "rewrite={rewrite}: {what} names Claude's {mode:?} where the row is rewritten, \
                 and only there: {rule:?}"
            );
            assert!(
                rule.contains(&clicks(Goal::of_mode(mode))),
                "rewrite={rewrite}: {what} says where a click goes: {rule:?}"
            );
        };
        let (mut app, wid, session, mut reader, _host) = claude_app(120, rewrite);
        draw_claude(&app, session, MANUAL);
        frame(&mut app, wid);
        let (x, y) = px_of(&app, wid, hit(&app, wid, Light::Mode));
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        says(
            &row_text(&app, wid, footer_row(&app, wid)),
            Mode::Manual,
            "the hover",
        );
        press_at(&mut app, wid, first);
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
        let stopped = format!(
            "{}{}",
            Refusal::StoppedIn(Mode::Manual).says(rewrite),
            clicks(Goal::On)
        );
        assert!(
            rule.contains(&stopped),
            "rewrite={rewrite}: the stop says its reason and where a click goes: {rule:?}"
        );
        let (x, y) = px_of(&app, wid, hit(&app, wid, Light::Mode));
        app.on_cursor_moved(wid, x, y);
        press_at(&mut app, wid, first + apart);
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
            "never pressed on from bypass, the goal"
        );
        says(
            &row_text(&app, wid, footer_row(&app, wid)),
            Mode::Bypass,
            "the arrival",
        );
        let row = mode_glass(&app, wid);
        assert_eq!(
            row.trim().is_empty(),
            rewrite,
            "rewrite={rewrite}: the gate's witness: {row:?}"
        );
    }
}

/// A CLICK ON YOLO TURNS IT LAME (the owner, 2026-11-02): on a rewritten row `yolo` stands at
/// rest in the glass's value ink whatever grey the rule it sits on wears (never Claude's red); a
/// click writes one shift+tab as the person's, the chip says `◌ yolo` while Claude answers, and
/// once Claude shows manual mode the chip reads `lame`, in the row's own ink, with nothing more
/// pressed. A second click from `lame` goes back to `yolo`. CONTROL: Claude's grey rule is not
/// that ink, so the rule's grey no longer decides it.
#[test]
fn a_click_on_yolo_turns_it_lame() {
    const GREY_RULE: &str = "\x1b[38;2;136;136;136m";
    for grey in [true, false] {
        let (mut app, wid, session, mut reader, _host) = claude_app(120, true);
        draw_claude(&app, session, BYPASS);
        if grey {
            let rule = "\u{2500}".repeat(120);
            feed(
                &app,
                session,
                format!("\x1b[23;1H{GREY_RULE}{rule}\x1b[0m\x1b[22;3H").as_bytes(),
            );
        }
        frame(&mut app, wid);
        let (cells, text) = light_cells(&app, wid);
        assert_eq!(text, "yolo", "grey={grey}: yolo at rest");
        let ink = value_ink(&app, wid);
        if grey {
            assert_ne!(ink, [136; 3], "CONTROL: the value ink is not Claude's grey");
        }
        for &at in &cells {
            assert_eq!(cell_at(&app, wid, at).fg, ink, "grey={grey}: the value ink");
            assert_ne!(cell_at(&app, wid, at).fg, [0xE0, 0x5A, 0x5A], "never red");
        }

        let (x, y) = px_of(&app, wid, cells[0]);
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        let rule = row_text(&app, wid, footer_row(&app, wid));
        assert!(
            rule.contains(Mode::Bypass.gist())
                && rule.contains(&format!("{TITLE_CLICK}{}", Goal::Off.words(true))),
            "the hover says what yolo is and what a click does: {rule:?}"
        );
        let first = a_minute_ago();
        press_at(&mut app, wid, first);
        assert_eq!(
            pty(&mut reader, lights::SHIFT_TAB.len()),
            lights::SHIFT_TAB,
            "one shift+tab, the person's"
        );
        let goal = app.windows[&wid].claude_lights.sessions[&session]
            .pending
            .as_ref()
            .map(|p| p.goal);
        assert_eq!(goal, Some(Goal::Off), "to manual");
        frame(&mut app, wid);
        assert_eq!(light_cells(&app, wid).1, "\u{25CC} yolo", "in flight");
        let rule = row_text(&app, wid, footer_row(&app, wid));
        assert!(rule.contains("switching to lame\u{2026}"), "{rule:?}");

        feed(
            &app,
            session,
            format!("\x1b[24;1H\x1b[2K{MANUAL}\x1b[22;3H").as_bytes(),
        );
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "manual: nothing more pressed"
        );
        frame(&mut app, wid);
        let (cells, text) = light_cells(&app, wid);
        assert_eq!(text, "lame", "grey={grey}: yolo turned off is lame");
        if grey {
            assert_ne!(
                cell_at(&app, wid, cells[0]).fg,
                ink,
                "lame is a deviation: the row's ink, not the value's"
            );
        }

        let (x, y) = px_of(&app, wid, cells[0]);
        app.on_cursor_moved(wid, x, y);
        press_at(&mut app, wid, first + interval() + Duration::from_millis(1));
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "lame: back to yolo");
        let goal = app.windows[&wid].claude_lights.sessions[&session]
            .pending
            .as_ref()
            .map(|p| p.goal);
        assert_eq!(goal, Some(Goal::On));
    }
}

/// A RUN OF CLICKS ON YOLO TURNS IT OVER ONCE (review F1): the first click presses shift+tab,
/// Claude answers auto, aterm presses on, Claude answers manual and the light reads `lame`, all
/// before the second click of the double click comes, at the interval's last instant, on `lame`:
/// that press is the double click's, and writes nothing, so the light stays `lame` rather than
/// turning back toward `yolo`. A slow triple click's third press, at the last instant of the
/// interval after the second though past the first's, is the same run of clicks and writes nothing
/// either. A click a millisecond after the interval of the run's last press is a click of its own:
/// the model's click back while the arrival's notice shows (`phase` 2), pressed at once.
#[test]
fn a_run_of_clicks_on_yolo_turns_it_over_once() {
    let (mut app, wid, session, mut reader, _host) = claude_app(120, true);
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let model = aterm_models_gui::claude_mode_toggle_model();
    let mut state = model.init_state();
    state.insert("mode", 0);
    let (cells, text) = light_cells(&app, wid);
    assert_eq!(text, "yolo", "yolo at rest");
    let (x, y) = px_of(&app, wid, cells[0]);
    app.on_cursor_moved(wid, x, y);
    let first = a_minute_ago();
    let since = Instant::now();
    drive_mode_toggle_action("Click", || press_at(&mut app, wid, first));
    press_not_due(&mut app, wid, session, since);
    assert_eq!(
        pty(&mut reader, 3),
        lights::SHIFT_TAB,
        "the first click presses"
    );
    check_mode_toggle_transition(&app, wid, session, "Click", &mut state, None);
    redraw_mode_row(&app, session, AUTO);
    check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
    let since = Instant::now();
    drive_mode_toggle_action("Press", || app.on_claude_footer_changed(session));
    press_not_due(&mut app, wid, session, since);
    assert_eq!(
        pty(&mut reader, 3),
        lights::SHIFT_TAB,
        "auto is short of manual: a press on"
    );
    check_mode_toggle_transition(&app, wid, session, "Press", &mut state, None);
    redraw_mode_row(&app, session, MANUAL);
    check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
    drive_mode_toggle_action("Arrive", || app.on_claude_footer_changed(session));
    assert!(pty(&mut reader, 0).is_empty(), "manual: arrived");
    check_mode_toggle_transition(&app, wid, session, "Arrive", &mut state, None);
    frame(&mut app, wid);
    let (cells, text) = light_cells(&app, wid);
    assert_eq!(text, "lame", "arrived before the second click");

    let (x, y) = px_of(&app, wid, cells[0]);
    app.on_cursor_moved(wid, x, y);
    press_at(&mut app, wid, first + interval());
    assert!(
        pty(&mut reader, 0).is_empty(),
        "the double click's second press writes nothing"
    );
    assert!(
        app.windows[&wid].claude_lights.sessions[&session]
            .pending
            .is_none(),
        "and arms no toggle"
    );
    frame(&mut app, wid);
    assert_eq!(light_cells(&app, wid).1, "lame", "the light stays lame");

    let (x, y) = px_of(&app, wid, light_cells(&app, wid).0[0]);
    app.on_cursor_moved(wid, x, y);
    press_at(&mut app, wid, first + interval() * 2);
    assert!(
        pty(&mut reader, 0).is_empty(),
        "a slow triple click's third press, within the interval of the second, writes nothing"
    );
    assert!(
        app.windows[&wid].claude_lights.sessions[&session]
            .pending
            .is_none(),
        "and arms no toggle"
    );
    frame(&mut app, wid);
    assert_eq!(
        light_cells(&app, wid).1,
        "lame",
        "the light still reads lame"
    );

    let (x, y) = px_of(&app, wid, light_cells(&app, wid).0[0]);
    app.on_cursor_moved(wid, x, y);
    let since = Instant::now();
    drive_mode_toggle_action("Click", || {
        press_at(
            &mut app,
            wid,
            first + interval() * 3 + Duration::from_millis(1),
        );
    });
    press_not_due(&mut app, wid, session, since);
    assert_eq!(
        pty(&mut reader, 3),
        lights::SHIFT_TAB,
        "a click a millisecond after the interval of the run's last press: the click back"
    );
    check_mode_toggle_transition(
        &app,
        wid,
        session,
        "Click",
        &mut state,
        Some(&[("phase", 2)][..]),
    );
    let goal = app.windows[&wid].claude_lights.sessions[&session]
        .pending
        .as_ref()
        .map(|p| p.goal);
    assert_eq!(goal, Some(Goal::On), "lame: back to yolo or auto");
}

/// THE MODE LIGHT'S WORD FOLLOWS THE REWRITE THE GLASS TAKES: with the cursor on the mode row
/// the row stands as Claude drew it and the light says `⏸ manual`; the cursor back at the caret,
/// the row is rewritten and the light says `lame`.
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
    assert_eq!(text, "lame");
}

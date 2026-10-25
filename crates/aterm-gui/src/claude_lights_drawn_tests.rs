// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the drawn Autopilot chip line and glass state.

use std::time::Instant;

use aterm_agent::supervise::Autopilot;

use super::super::gesture_tests::{
    BYPASS, Hosted, PGID, app, cols, draw_claude, feed, frame, host, publish_facts,
};
use super::{Drawn, WindowLights};
use crate::autopilot::AutopilotView;
use crate::{App, WindowId};

fn set_program(app: &App, session: u64, program: &str) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some(program.into()));
}

fn claude(app: &App, session: u64) {
    publish_facts(app, session);
    set_program(app, session, "claude");
    draw_claude(app, session, BYPASS);
}

fn set_word(app: &App, session: u64, word: Autopilot) {
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set("autopilot", Some(word.word().into()));
}

fn desk() -> (App, WindowId, u64, std::os::unix::net::UnixStream, Hosted) {
    let (mut app, wid, session, reader) = app();
    let hosted = host(&mut app);
    claude(&app, session);
    frame(&mut app, wid);
    (app, wid, session, reader, hosted)
}

fn lines_of(app: &App, session: u64) -> Vec<String> {
    let sid = app.pool.get(session).expect("session").ctx.self_id.clone();
    let head = format!("autopilot sid={} ", sid.as_str());
    app.drawn_chip_lines()
        .into_iter()
        .filter(|l| l.starts_with(&head))
        .collect()
}

fn line_of(app: &App, session: u64) -> String {
    let lines = lines_of(app, session);
    assert_eq!(lines.len(), 1, "one line for the pane: {lines:?}");
    lines[0].clone()
}

fn token<'a>(line: &'a str, key: &str) -> &'a str {
    let at = line
        .find(&format!(" {key}="))
        .unwrap_or_else(|| panic!("{key}= in {line:?}"));
    let rest = &line[at + key.len() + 2..];
    rest.split(' ').next().unwrap_or_default()
}

fn quoted(line: &str, key: &str) -> Option<String> {
    let at = line
        .find(&format!(" {key}="))
        .unwrap_or_else(|| panic!("{key}= in {line:?}"))
        + key.len()
        + 2;
    let rest = &line[at..];
    let quoted = rest.strip_prefix('"')?;
    let end = quoted.find('"').expect("a quoted value closes");
    Some(quoted[..end].to_string())
}

fn drawn(line: &str) -> Option<String> {
    quoted(line, "drawn")
}

fn on_glass(app: &App, wid: WindowId, session: u64) -> Option<String> {
    let ws = &app.windows[&wid];
    let mut hits: Vec<(usize, usize)> = ws
        .claude_lights
        .hits
        .iter()
        .filter(|h| h.session == session && h.chip == Some(super::Chip::Autopilot))
        .map(|h| (h.frame_row, h.col))
        .collect();
    hits.sort_unstable();
    (!hits.is_empty()).then(|| {
        hits.iter()
            .map(|&(row, col)| ws.input_scratch.cells[row][col])
            .filter(|c| !c.wide)
            .map(|c| c.ch)
            .collect()
    })
}

/// What the accessibility tree's node for `session`'s Autopilot chip on
/// window `wid` is called (`App::claude_chips_a11y`), `None` where the tree
/// has no such node.
#[cfg(a11y_tree)]
fn node_label(app: &App, wid: WindowId, session: u64) -> Option<String> {
    app.claude_chips_a11y(wid)
        .into_iter()
        .find(|c| c.session == session && c.chip == crate::a11y_tree::HarnessChip::Autopilot)
        .map(|c| c.label)
}

#[test]
fn chrome_reads_the_chip_the_frame_drew() {
    let (mut app, wid, session, _reader, _host) = desk();
    let line = line_of(&app, session);
    assert_eq!(token(&line, "window"), "0", "{line}");
    assert_eq!(token(&line, "program"), "claude", "{line}");
    assert_eq!(token(&line, "face"), "cruise", "{line}");
    assert_eq!(drawn(&line).as_deref(), Some("\u{221E}"), "{line}");
    assert_eq!(
        drawn(&line),
        on_glass(&app, wid, session),
        "drawn= is the frame's cells: {line}"
    );
    assert!(
        quoted(&line, "spoken").is_some(),
        "a drawn chip speaks: {line}"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        quoted(&line, "spoken"),
        node_label(&app, wid, session),
        "spoken= is the node's label: {line}"
    );
    let before = quoted(&line, "spoken");

    set_word(&app, session, Autopilot::Land);
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "land", "the face now: {line}");
    assert_eq!(
        drawn(&line).as_deref(),
        Some("\u{221E}"),
        "no frame since: the drawn face: {line}"
    );
    assert_eq!(drawn(&line), on_glass(&app, wid, session), "{line}");
    assert_eq!(
        quoted(&line, "spoken"),
        before,
        "no frame since: the drawn chip's words: {line}"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        quoted(&line, "spoken"),
        node_label(&app, wid, session),
        "{line}"
    );

    frame(&mut app, wid);
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "land", "{line}");
    assert_eq!(
        drawn(&line).as_deref(),
        Some("\u{2913} land"),
        "the frame drew the new face: {line}"
    );
    assert_eq!(drawn(&line), on_glass(&app, wid, session), "{line}");
    assert_ne!(
        quoted(&line, "spoken"),
        before,
        "the new face's words: {line}"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        quoted(&line, "spoken"),
        node_label(&app, wid, session),
        "{line}"
    );
}

#[test]
fn a_background_pane_names_no_drawn_chip() {
    let (mut app, wid, a, _reader, _host) = desk();
    assert_eq!(drawn(&line_of(&app, a)).as_deref(), Some("\u{221E}"));

    let b = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(b));
    assert_eq!(app.focused_session_id(wid), Some(b), "the new tab in front");
    claude(&app, b);
    let line = line_of(&app, a);
    assert_eq!(
        (drawn(&line), token(&line, "why")),
        (None, "not-drawn"),
        "behind, before a frame: {line}"
    );

    set_word(&app, a, Autopilot::Land);
    frame(&mut app, wid);
    let behind = line_of(&app, a);
    assert_eq!(token(&behind, "face"), "land", "the face now: {behind}");
    assert_eq!(
        (drawn(&behind), token(&behind, "why")),
        (None, "not-drawn"),
        "never the stale face: {behind}"
    );
    assert_eq!(token(&behind, "spoken"), "-", "no chip, no words: {behind}");
    let front = line_of(&app, b);
    assert_eq!(drawn(&front).as_deref(), Some("\u{221E}"), "{front}");
    assert_eq!(
        drawn(&front),
        on_glass(&app, wid, b),
        "the front tab's drawn= is the frame's cells: {front}"
    );

    app.switch_tab_in(wid, 0);
    assert_eq!(
        app.focused_session_id(wid),
        Some(a),
        "the first tab in front"
    );
    let line = line_of(&app, a);
    assert_eq!(
        (drawn(&line), token(&line, "why")),
        (None, "not-drawn"),
        "no frame has drawn it since: {line}"
    );
    let line = line_of(&app, b);
    assert_eq!(token(&line, "why"), "not-drawn", "now behind: {line}");

    frame(&mut app, wid);
    let line = line_of(&app, a);
    assert_eq!(drawn(&line).as_deref(), Some("\u{2913} land"), "{line}");
    assert_eq!(drawn(&line), on_glass(&app, wid, a), "{line}");
}

#[test]
fn a_chip_a_band_covers_names_no_cells() {
    let (mut app, wid, session, _reader, _host) = desk();
    assert_eq!(drawn(&line_of(&app, session)).as_deref(), Some("\u{221E}"));
    let row = app.windows[&wid]
        .claude_lights
        .hits
        .iter()
        .find(|h| h.session == session && h.chip == Some(super::Chip::Autopilot))
        .expect("the chip's hit")
        .term_row;
    app.windows
        .get_mut(&wid)
        .expect("window")
        .chrome_rows
        .push(row..row + 1);
    assert!(
        app.chrome_owns_terminal_row(wid, row),
        "the band owns the rule's row"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        node_label(&app, wid, session),
        None,
        "the tree drops it too"
    );
    let line = line_of(&app, session);
    assert_eq!(
        (drawn(&line), token(&line, "why"), token(&line, "spoken")),
        (None, "not-drawn", "-"),
        "a covered chip is not drawn: {line}"
    );
    assert_eq!(token(&line, "face"), "cruise", "the face stands: {line}");
}

#[test]
fn a_rule_with_no_run_names_no_room() {
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    set_program(&app, session, "claude");
    let n = cols(&app, session);
    let top = "\u{2500}".repeat(n);
    let marked = "\u{2500}\u{0301}".repeat(n);
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{top}\x1b[22;1H\u{276F} \
         \x1b[23;1H{marked}\x1b[24;1H{BYPASS}\x1b[22;3H"
    );
    feed(&app, session, screen.as_bytes());
    frame(&mut app, wid);
    let lights = &app.windows[&wid].claude_lights;
    assert!(
        lights
            .footers
            .iter()
            .any(|(s, room)| *s == session && *room == 0),
        "the rule is on the glass with no room: {:?}",
        lights.footers
    );
    assert!(
        !lights.fitted.iter().any(|f| f.session == session && f.used),
        "no block is fitted for it"
    );
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "cruise", "a live host: {line}");
    assert_eq!(
        (drawn(&line), token(&line, "why")),
        (None, "no-room"),
        "{line}"
    );
}

#[test]
fn a_co_viewed_session_names_each_window() {
    let (mut app, wid, session, _reader, _host) = desk();
    let other = app
        .open_active_session_in_new_window_logical()
        .expect("the session co-viewed in a second window");
    frame(&mut app, other);
    frame(&mut app, wid);
    let lines = lines_of(&app, session);
    assert_eq!(lines.len(), 2, "a line in each window: {lines:?}");
    let windows: Vec<&str> = lines.iter().map(|l| token(l, "window")).collect();
    let mut want = vec![wid.0.to_string(), other.0.to_string()];
    want.sort();
    let mut got: Vec<String> = windows.iter().map(|w| (*w).to_string()).collect();
    got.sort();
    assert_eq!(got, want, "{lines:?}");
    for line in &lines {
        let window = WindowId(token(line, "window").parse().expect("a window id"));
        assert_eq!(
            drawn(line),
            on_glass(&app, window, session),
            "each window's own frame: {line}"
        );
    }
}

/// A FOOTERLESS AGENT'S CHIP LINE NAMES NO CHIP, AND THE RETIRED GEMINI CLI GETS NO LINE AT ALL:
/// a tab running `gemini` is no agent (agy replaces it), so no chip line names it; the pane
/// running Claude Code is drawn as its face; a codex-alab that proved its claim is an agent aterm
/// admits whose program draws no footer, so its line reads `face=- drawn=- why=no-footer`.
#[test]
fn the_chip_line_names_no_chip_for_a_footerless_agent_and_none_for_the_retired_gemini_cli() {
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    for program in ["gemini", "gemini-cli"] {
        set_program(&app, session, program);
        frame(&mut app, wid);
        assert!(
            lines_of(&app, session).is_empty(),
            "{program}: the retired Gemini CLI is no agent"
        );
    }
    claude(&app, session);
    frame(&mut app, wid);
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "cruise", "{line}");

    set_program(&app, session, "codex");
    frame(&mut app, wid);
    assert!(
        lines_of(&app, session).is_empty(),
        "CONTROL: a codex with no proven claim is no agent aterm admits"
    );
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor_as(
        &ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(1),
        None,
        Some(crate::self_claim::stand_in::running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the proven claim");
    frame(&mut app, wid);
    let line = line_of(&app, session);
    assert_eq!(
        line,
        format!(
            "autopilot sid={} window=0 program=codex face=- drawn=- why=no-footer spoken=- \
             description=- title=-",
            ctx.self_id.as_str()
        )
    );
}

#[test]
fn a_footer_left_bare_under_an_off_switch_is_switch_only() {
    use crate::autopilot::{HarnessSwitch, SwitchOff};
    use aterm_phase::Program;
    let switch = |program, face, session| crate::claude_lights::BorderSwitch {
        frame_row: 7,
        term_row: 7,
        cols: 100..130,
        switch: 101..120,
        button: Some(121..127),
        session,
        face,
        tier: 0,
        program,
    };
    let read = |drew: &[crate::claude_lights::BorderSwitch], covered: Option<usize>| {
        let mut lights = WindowLights::default();
        lights.border_toggles.extend_from_slice(drew);
        lights.drawn_chip(3, 0, |row| Some(row) == covered)
    };
    let off = HarnessSwitch::Off(SwitchOff::Master);
    for face in [
        off,
        HarnessSwitch::Off(SwitchOff::Agent(Program::Agy)),
        HarnessSwitch::Turning,
    ] {
        assert_eq!(
            read(&[switch(Program::Agy, face, 3)], None),
            Drawn::SwitchOnly,
            "{face:?}"
        );
    }
    assert_eq!(Drawn::SwitchOnly.why(), Some("switch-only"));
    assert_eq!(
        read(&[], None),
        Drawn::NoRule,
        "no switch drawn: a tab behind"
    );
    for face in [HarnessSwitch::On, HarnessSwitch::TurningOff] {
        assert_eq!(
            read(&[switch(Program::Agy, face, 3)], None),
            Drawn::NoRule,
            "{face:?}: a switch the harness is on under leaves no footer bare"
        );
    }
    for program in [Program::Claude, Program::Muse] {
        assert_eq!(
            read(&[switch(program, off, 3)], None),
            Drawn::NoRule,
            "{program:?} keeps its footer and its chip"
        );
    }
    assert_eq!(
        read(&[switch(Program::Agy, off, 3)], Some(7)),
        Drawn::NoRule,
        "a switch a band covers is not drawn"
    );
    assert_eq!(
        read(&[switch(Program::Agy, off, 4)], None),
        Drawn::NoRule,
        "another session's switch"
    );
    let mut lights = WindowLights::default();
    lights.border_toggles.push(switch(Program::Agy, off, 3));
    lights.footers.push((3, 0));
    assert_eq!(
        lights.drawn_chip(3, 0, |_| false),
        Drawn::NoRoom,
        "a rule found with no room"
    );
}

fn need(app: &mut App, session: u64, entry: Option<&str>) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    ctx.meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_attention_kind(
            crate::harness_host::NEEDS_ATTENTION_OWNER,
            entry.map(str::to_string),
            entry.map(|_| aterm_types::needs::AttentionKind::Ask),
        )
        .expect("a slot");
    app.on_meta_changed(session);
}

/// What the accessibility tree's node for `session`'s Autopilot chip on
/// window `wid` describes it as, `None` where it has no description.
#[cfg(a11y_tree)]
fn node_description(app: &App, wid: WindowId, session: u64) -> Option<String> {
    app.claude_chips_a11y(wid)
        .into_iter()
        .find(|c| c.session == session && c.chip == crate::a11y_tree::HarnessChip::Autopilot)
        .and_then(|c| c.description)
}

#[test]
fn chrome_says_the_chips_description_as_its_node_does() {
    use crate::harness_glass::hold_tests::held;
    let (mut app, wid, session, _reader, _host) = desk();
    let line = line_of(&app, session);
    assert_eq!(token(&line, "description"), "-", "{line}");
    need(
        &mut app,
        session,
        Some(
            "claude needs you: grant Screen Recording in System Settings \u{25b8} Privacy & \
             Security \u{2014} carrying on",
        ),
    );
    let line = line_of(&app, session);
    assert_eq!(
        quoted(&line, "description").as_deref(),
        Some(
            "Needs you: grant Screen Recording in System Settings, Privacy and Security. Carrying on."
        ),
        "{line}"
    );
    need(
        &mut app,
        session,
        Some("claude needs you: approve the 0.48.0 release notes \u{2014} carrying on"),
    );
    frame(&mut app, wid);
    let line = line_of(&app, session);
    assert_eq!(
        quoted(&line, "spoken").as_deref(),
        Some("Autopilot: cruise.")
    );
    assert_eq!(
        quoted(&line, "description").as_deref(),
        Some("Needs you: approve the 0.48.0 release notes. Carrying on."),
        "{line}"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        quoted(&line, "description"),
        node_description(&app, wid, session),
        "{line}"
    );
    set_word(&app, session, Autopilot::Land);
    assert!(crate::harness_glass::on_land_held(
        &mut app,
        session,
        Some(held())
    ));
    frame(&mut app, wid);
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "land", "{line}");
    assert_eq!(quoted(&line, "spoken").as_deref(), Some("Autopilot: land."));
    let description = quoted(&line, "description").expect("described");
    assert!(
        description.starts_with(
            "Not landed yet. Needs you: approve the 0.48.0 release notes. 2 unpushed commits. "
        ),
        "{line}"
    );
    assert_eq!(description.matches("Needs you").count(), 1, "{line}");
    #[cfg(a11y_tree)]
    assert_eq!(
        Some(description),
        node_description(&app, wid, session),
        "{line}"
    );
    need(&mut app, session, None);
    let line = line_of(&app, session);
    assert!(
        quoted(&line, "description")
            .is_some_and(|d| d.starts_with("Not landed yet. A need named earlier in its reply.")),
        "{line}"
    );
}

#[test]
fn the_hovered_title_on_the_glass_says_the_need_and_the_hold() {
    use super::super::gesture_tests::{footer_row, row_text};
    use crate::harness_glass::{HoldForm, SessionGlass, hold_tests::held};
    let (mut app, wid, session, _reader, _host) = desk();
    assert!(app.apply_term_resize(wid, 24, 400));
    draw_claude(&app, session, BYPASS);
    need(
        &mut app,
        session,
        Some("claude needs you: approve the 0.48.0 release notes \u{2014} carrying on"),
    );
    let ask = app
        .pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .need()
        .harness_ask()
        .cloned()
        .expect("CONTROL: the need is a harness's ask");
    app.hover_claude_chip(wid, Some((session, super::Chip::Autopilot)));
    frame(&mut app, wid);
    let rule = row_text(&app, wid, footer_row(&app, wid));
    let said = crate::harness_glass::need_title(&ask);
    assert!(
        rule.contains(&said),
        "the need {said:?} on the glass: {rule:?}"
    );
    set_word(&app, session, Autopilot::Land);
    assert!(crate::harness_glass::on_land_held(
        &mut app,
        session,
        Some(held())
    ));
    frame(&mut app, wid);
    let rule = row_text(&app, wid, footer_row(&app, wid));
    let hold = app
        .harness_glass
        .get(&session)
        .and_then(SessionGlass::hold)
        .expect("CONTROL: the hold on the session's glass");
    let today = Some(crate::harness_glass::wall_today().1);
    let held_words = crate::harness_glass::hold_words(&hold, Some(&ask), HoldForm::Title, today)
        .expect("a read hold's words");
    assert!(
        rule.contains(&format!("autopilot: land \u{2014} {held_words}")),
        "the hold {held_words:?} on the glass: {rule:?}"
    );
}

/// `chrome`'s `title=` (a development build's) is the whole title the chip's hover draws, read
/// with no hover and no frame since: Cruise's sentence from `MODE_WORDS` at rest, the need beside
/// it, and a read Land hold's title form in place of Land's sentence (5d, the forever lab's read of
/// the title's words).
#[test]
fn chrome_says_the_title_its_hover_draws() {
    use super::super::gesture_tests::{footer_row, row_text};
    use crate::autopilot::mode_words;
    use crate::harness_glass::hold_tests::held;
    let (mut app, wid, session, _reader, _host) = desk();
    assert!(app.apply_term_resize(wid, 24, 600));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let title = |app: &App| {
        let line = line_of(app, session);
        quoted(&line, "title").unwrap_or_else(|| panic!("a drawn chip's title: {line}"))
    };
    let hovered = |app: &mut App| {
        app.hover_claude_chip(wid, Some((session, super::Chip::Autopilot)));
        frame(app, wid);
        let rule = row_text(app, wid, footer_row(app, wid));
        app.hover_claude_chip(wid, None);
        frame(app, wid);
        rule
    };
    let at_rest = title(&app);
    assert!(
        at_rest.starts_with(&format!(
            "autopilot: cruise \u{2014} {}",
            mode_words(Autopilot::Cruise).chip
        )),
        "Cruise's sentence: {at_rest:?}"
    );
    let rule = hovered(&mut app);
    assert!(
        rule.contains(&at_rest),
        "{at_rest:?} on the glass: {rule:?}"
    );
    need(
        &mut app,
        session,
        Some("claude needs you: approve the 0.48.0 release notes \u{2014} carrying on"),
    );
    frame(&mut app, wid);
    let with_need = title(&app);
    assert!(
        with_need.contains("needs you: approve the 0.48.0 release notes"),
        "the need beside the sentence: {with_need:?}"
    );
    let rule = hovered(&mut app);
    assert!(
        rule.contains(&with_need),
        "{with_need:?} on the glass: {rule:?}"
    );
    set_word(&app, session, Autopilot::Land);
    assert!(crate::harness_glass::on_land_held(
        &mut app,
        session,
        Some(held())
    ));
    frame(&mut app, wid);
    let land = title(&app);
    assert!(
        land.starts_with("autopilot: land \u{2014} not landed yet: ")
            && !land.contains(mode_words(Autopilot::Land).chip),
        "a read hold says why in place of Land's sentence: {land:?}"
    );
    let rule = hovered(&mut app);
    assert!(rule.contains(&land), "{land:?} on the glass: {rule:?}");
}

#[test]
fn a_shell_pane_prints_no_chip_line() {
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    set_program(&app, session, "zsh");
    frame(&mut app, wid);
    assert!(app.drawn_chip_lines().is_empty(), "no agent, no line");
    claude(&app, session);
    frame(&mut app, wid);
    assert_eq!(
        app.drawn_chip_lines().len(),
        1,
        "control: Claude Code's pane has one"
    );
}

#[test]
fn a_chip_the_rule_did_not_take_names_no_cells() {
    let blank = aterm_core::terminal::RenderCell {
        ch: ' ',
        ..aterm_core::terminal::RenderCell::default()
    };
    let now = Instant::now();
    let shown = |_: usize| false;
    let mut w = WindowLights::default();
    w.note_autopilot(7, None, AutopilotView::Cruise, now);
    w.clear_hits();
    let block = w
        .lend_block(7, 0, blank, [0, 128, 255], (None, None), 40, None)
        .expect("a chip fits in 40 cells");
    w.give_back(block);
    assert_eq!(
        w.drawn_chip(7, 0, shown),
        Drawn::NoRoom,
        "lent, never on the glass"
    );
    assert_eq!(
        w.drawn_chip(7, 1, shown),
        Drawn::NoRule,
        "another leaf: not drawn"
    );

    let painted = w.fitted[0].block.as_ref().expect("the block is home");
    let cols = painted.cols.clone();
    for (span, chip) in cols {
        for at in span {
            w.hits.push(super::super::LightHit {
                frame_row: 30,
                term_row: 22,
                col: 60 + at,
                session: 7,
                chip: Some(chip),
            });
        }
    }
    assert_eq!(w.drawn_chip(7, 0, shown), Drawn::Cells("\u{221E}".into()));
    assert_eq!(
        w.drawn_chip(7, 0, |row| row == 22),
        Drawn::NoRule,
        "a band over row 22 hides it"
    );

    let mut w = WindowLights::default();
    w.note_autopilot(8, None, AutopilotView::Unhosted, now);
    w.clear_hits();
    assert!(
        w.lend_block(8, 0, blank, [0, 128, 255], (None, None), 40, None)
            .is_none(),
        "control: no host, no chip"
    );
    assert_eq!(w.drawn_chip(8, 0, shown), Drawn::NoHost);
}

/// `chrome`'s `title=` is the hover's own reading, composed from the view and glass the last
/// frame noted (`title_parts`, as the hover's `TitleReading` is), never from a glass no frame has
/// drawn: a landing shows in the title once a frame draws it, as on the glass.
#[test]
fn chrome_reads_the_title_from_the_glass_the_frame_noted() {
    use aterm_agent::harness::acts::TaskFace;
    let (mut app, wid, session, _reader, _host) = desk();
    assert!(app.apply_term_resize(wid, 24, 600));
    set_word(&app, session, Autopilot::Land);
    frame(&mut app, wid);
    let title = |app: &App| {
        let line = line_of(app, session);
        quoted(&line, "title").unwrap_or_else(|| panic!("a drawn chip's title: {line}"))
    };
    let open = title(&app);
    assert!(open.starts_with("autopilot: land \u{2014} "), "{open:?}");
    assert!(crate::harness_glass::on_task(
        &mut app,
        session,
        Some(TaskFace::Landed),
        1_792_000_000_000
    ));
    assert_eq!(title(&app), open, "no frame has drawn the landing yet");
    frame(&mut app, wid);
    let landed = title(&app);
    assert!(
        landed.starts_with("autopilot: land, landed"),
        "the frame drew it: {landed:?}"
    );
}

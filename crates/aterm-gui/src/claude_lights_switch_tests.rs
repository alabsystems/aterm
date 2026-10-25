// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the Claude Code composer top-rule harness switch.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;
use aterm_core::render::{BandIcon, ChromeIcon};
use aterm_window::event::{ElementState, MouseButton};

use super::gesture_tests::{
    BYPASS, Hosted, PGID, WHOLE_FACTS, app, autopilot, draw_claude, feed, footer_fp, footer_row,
    frame, hit, host, pty, publish_facts, px_of, row_text,
};
use super::{BorderSwitch, Chip};
use crate::autopilot::{AutopilotView, HarnessSwitch, SwitchOff};
use crate::native_settings::SettingsRoute;
use crate::{App, WindowId};

fn claude_pane(cols: u16) -> (App, WindowId, u64, UnixStream, Hosted) {
    let (mut app, wid, session, reader) = app();
    assert!(app.apply_term_resize(wid, 24, cols));
    let hosted = host(&mut app);
    publish_facts(&app, session);
    set_program(&app, session, Some("claude"));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    (app, wid, session, reader, hosted)
}

/// Name the program of `session`'s foreground group, `PGID`, which it notes
/// first: a timeline names a program only for the group it holds in front.
fn set_program(app: &App, session: u64, program: Option<&str>) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, program.map(Into::into));
}

fn policy(app: &mut App, text: &str) {
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read(text),
    );
}

fn top_rule(app: &App, wid: WindowId) -> String {
    app.windows[&wid].input_scratch.cells[footer_row(app, wid) - 2]
        .iter()
        .map(|c| c.ch)
        .collect()
}

fn painted(app: &App, wid: WindowId, b: &BorderSwitch, cols: std::ops::Range<usize>) -> String {
    let row = &app.windows[&wid].input_scratch.cells[b.frame_row];
    assert!(
        cols.end <= row.len(),
        "{cols:?} past a row of {}: {b:?}",
        row.len()
    );
    row[cols].iter().map(|c| c.ch).collect()
}

fn switch(app: &App, wid: WindowId) -> Option<BorderSwitch> {
    let toggles = &app.windows[&wid].claude_lights.border_toggles;
    assert!(toggles.len() <= 1, "one pane, one switch: {toggles:?}");
    toggles.first().cloned()
}

fn click_at(app: &mut App, wid: WindowId, cell: (usize, usize)) {
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
}

fn on_button(app: &App, wid: WindowId) -> (usize, usize) {
    let b = switch(app, wid).expect("a switch");
    let button = b.button.expect("an off switch's on button");
    (b.frame_row, button.start + button.len() / 2)
}

fn settings_open(app: &App) -> bool {
    app.native_settings_route_view_target(SettingsRoute::Harness)
        .is_some()
}

/// Let a double click's interval pass since the switch's last press
/// (`SessionLights::switch_pressed`), as a person's next, separate click
/// would: the press is stamped that long ago.
pub(super) fn later(app: &mut App, wid: WindowId, session: u64) {
    let interval = Duration::from_millis(
        u64::try_from(crate::multi_click_ms()).expect("a double click's interval") + 1,
    );
    if let Some(s) = app
        .windows
        .get_mut(&wid)
        .expect("window")
        .claude_lights
        .sessions
        .get_mut(&session)
    {
        s.switch_pressed = s.switch_pressed.and_then(|at| at.checked_sub(interval));
    }
}

fn not_saved(app: &App, key: &str) -> Option<String> {
    let want = format!("harness.switch-save.{key}");
    app.messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some(want.as_str()))
        .map(|r| r.detail.join(" "))
}

const OFF_FACE: &str = " alab harness \u{25cb} off \u{2590} on \u{258c} ";

const MASTER_OFF: &str = "[harness]\nenabled = false\n";

/// AUTOPILOT OFF IS NOT THE HARNESS OFF: a session whose Autopilot is Off keeps the harness's
/// switch lit, `alab harness ● on`, in the white the rest of its glass wears (the moonbow, bold,
/// never the bare switch's accent), and a press on it turns the WHOLE harness off: it writes
/// `harness.enabled = false`, arms `◐ turning off` (the refused write here lets it go), and
/// writes no Autopilot word. CONTROL: the rule beside the switch is no button.
#[test]
fn an_autopilot_off_session_reads_on_in_white_and_its_press_turns_the_harness_off() {
    let (mut app, wid, session, mut reader, _host) = claude_pane(120);
    let lit = switch(&app, wid).expect("a hosted Claude Code pane draws the switch");
    assert_eq!((lit.face, lit.button.clone()), (HarnessSwitch::On, None));
    let row =
        |app: &App, b: &BorderSwitch| app.windows[&wid].input_scratch.cells[b.frame_row].clone();
    let cruise_ink: Vec<[u8; 3]> = row(&app, &lit)[lit.switch.clone()]
        .iter()
        .filter(|c| c.ch != ' ')
        .map(|c| c.fg)
        .collect();
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::Off)
    );
    let on = switch(&app, wid).expect("Autopilot Off draws the switch");
    assert_eq!(
        (on.face, on.button.clone()),
        (HarnessSwitch::On, None),
        "the harness is on: lit, no `on` button"
    );
    let rule = top_rule(&app, wid);
    assert!(
        rule.contains(" alab harness \u{25cf} on ") && !rule.contains("alab kitty"),
        "{rule:?}"
    );
    let cells = row(&app, &on);
    let glyphs: Vec<_> = cells[on.switch.clone()]
        .iter()
        .filter(|c| c.ch != ' ')
        .copied()
        .collect();
    assert_eq!(glyphs.len(), "alabharness\u{25cf}on".chars().count());
    for c in &glyphs {
        let spread = c.fg.iter().max().unwrap() - c.fg.iter().min().unwrap();
        assert!(
            c.bold && spread <= 24,
            "{:?}: white and bold, the moonbow: {:?}",
            c.ch,
            c.fg
        );
    }
    let mark_fg = crate::chrome_band::band_colors(app.theme).accent;
    let lit_state = glyphs.iter().find(|c| c.ch == '\u{25cf}').expect("`●`");
    assert_ne!(lit_state.fg, mark_fg, "CONTROL: `●` is not the bare accent");
    assert_ne!(
        glyphs.iter().map(|c| c.fg).collect::<Vec<_>>(),
        cruise_ink,
        "CONTROL: not Cruise's arc"
    );
    super::SWITCHES_LET_GO.with(|n| n.set(0));
    click_at(&mut app, wid, (on.frame_row, on.cols.start - 2));
    assert_eq!(
        not_saved(&app, "harness.enabled"),
        None,
        "CONTROL: the rule beside the switch is no button"
    );
    click_at(&mut app, wid, (on.frame_row, on.switch.start + 2));
    let said = not_saved(&app, "harness.enabled").expect("the master switch's write was made");
    assert!(said.contains("harness.enabled = false"), "{said:?}");
    let title = app
        .messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some("harness.switch-save.harness.enabled"))
        .map(|r| r.title.clone());
    assert_eq!(title.as_deref(), Some("Couldn't turn the harness off"));
    assert_eq!(
        super::SWITCHES_LET_GO.with(std::cell::Cell::get),
        1,
        "the press armed `◐ turning off`, and the refusal let it go"
    );
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::Off),
        "the switch writes no Autopilot word"
    );
    assert!(!settings_open(&app), "nothing opens");
    assert!(
        pty(&mut reader, 0).is_empty(),
        "nothing reached the program"
    );
}

/// THE OFF FACE'S `on` TURNS ON THE SWITCHES ALONE: under the master switch off, a session whose
/// own word is Off presses `on`, and the press writes `harness.enabled = true` and never the
/// Autopilot word `cruise` (the chip writes words); once the policy reads it back the switch is
/// lit over Autopilot Off.
#[test]
fn the_on_button_writes_the_switch_and_never_cruise() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    policy(&mut app, MASTER_OFF);
    frame(&mut app, wid);
    let off = switch(&app, wid).expect("the master switch off draws the switch");
    assert_eq!(off.face, HarnessSwitch::Off(SwitchOff::Master));
    assert_eq!(
        painted(&app, wid, &off, off.button.clone().unwrap()),
        "\u{2590} on \u{258c}"
    );
    let at = on_button(&app, wid);
    click_at(&mut app, wid, at);
    let said = not_saved(&app, "harness.enabled").expect("the master switch's write was made");
    assert!(said.contains("harness.enabled = true"), "{said:?}");
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::MasterOff),
        "no word written: the word stays Off"
    );
    policy(&mut app, "[harness]\n");
    frame(&mut app, wid);
    assert_eq!(autopilot(&app, session).1, AutopilotView::Off);
    assert_eq!(
        switch(&app, wid).map(|b| b.face),
        Some(HarnessSwitch::On),
        "read back: lit over Autopilot Off"
    );
}

/// A SWITCH TURNING OFF SAYS SO AND TAKES NO CLICK: while the master switch's off-write has not
/// read back, the lit switch reads `◐ turning off` (`◐ off` at its narrowest), the side it
/// turns to is in the repaint key, and a click on it writes nothing; the read-back lets it go.
#[test]
fn a_switch_turning_off_says_so_and_takes_no_click() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let at_rest = footer_fp(&app, wid);
    let arm = |app: &mut App, on: bool| {
        app.windows
            .get_mut(&wid)
            .expect("window")
            .claude_lights
            .sessions
            .entry(session)
            .or_default()
            .switch_turning = Some((on, Instant::now() + Duration::from_secs(60)));
    };
    arm(&mut app, false);
    let turning_off = footer_fp(&app, wid);
    assert_ne!(turning_off, at_rest, "turning off moves the key");
    arm(&mut app, true);
    assert_ne!(
        footer_fp(&app, wid),
        turning_off,
        "the side it turns to is in the key"
    );
    arm(&mut app, false);
    frame(&mut app, wid);
    let turning = switch(&app, wid).expect("the switch, turning off");
    assert_eq!(
        (turning.face, turning.button.clone()),
        (HarnessSwitch::TurningOff, None)
    );
    assert!(
        top_rule(&app, wid).contains(" alab harness \u{25d0} turning off "),
        "{:?}",
        top_rule(&app, wid)
    );
    click_at(&mut app, wid, (turning.frame_row, turning.switch.start + 2));
    assert_eq!(not_saved(&app, "harness.enabled"), None, "no write");
    assert!(!settings_open(&app));
    let mut narrowest = None;
    for width in (6..=40u16).rev() {
        assert!(app.apply_term_resize(wid, 24, width));
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        if let Some(b) = switch(&app, wid).filter(|b| b.tier == 3) {
            assert_eq!(b.face, HarnessSwitch::TurningOff, "{width}");
            narrowest = Some(painted(&app, wid, &b, b.switch.clone()));
        }
    }
    assert_eq!(
        narrowest.as_deref(),
        Some("\u{25d0} off"),
        "the narrowest form is met"
    );
    assert!(app.apply_term_resize(wid, 24, 120));
    draw_claude(&app, session, BYPASS);
    policy(&mut app, MASTER_OFF);
    frame(&mut app, wid);
    assert_eq!(
        switch(&app, wid).map(|b| b.face),
        Some(HarnessSwitch::Off(SwitchOff::Master)),
        "the read-back lets `turning off` go"
    );
    assert!(
        app.windows[&wid].claude_lights.sessions[&session]
            .switch_turning
            .is_none()
    );
}

#[test]
fn a_double_click_on_on_leaves_the_harness_on() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let lit = switch(&app, wid).expect("lit");
    let at = (lit.frame_row, lit.switch.start + 2);
    super::SWITCHES_LET_GO.with(|n| n.set(0));
    click_at(&mut app, wid, at);
    let said = not_saved(&app, "harness.enabled").expect("first click: the off-write");
    assert!(said.contains("harness.enabled = false"), "{said:?}");
    policy(&mut app, MASTER_OFF);
    frame(&mut app, wid);
    let off = switch(&app, wid).expect("read back: off");
    assert_eq!(
        off.face,
        HarnessSwitch::Off(SwitchOff::Master),
        "the frame between put the off face under the pointer"
    );
    assert!(
        off.cols.contains(&at.1),
        "the second click lands on the off face"
    );
    click_at(&mut app, wid, at);
    let said = not_saved(&app, "harness.enabled").expect("the off-write's record");
    assert!(
        said.contains("harness.enabled = false"),
        "the second click of the double click turned nothing back on: {said:?}"
    );
    assert_eq!(
        super::SWITCHES_LET_GO.with(std::cell::Cell::get),
        1,
        "one write for a double click"
    );
    later(&mut app, wid, session);
    let at = on_button(&app, wid);
    click_at(&mut app, wid, at);
    let said = not_saved(&app, "harness.enabled").expect("a later click's write");
    assert!(
        said.contains("harness.enabled = true"),
        "a later click: {said:?}"
    );
    assert_eq!(autopilot(&app, session), (None, AutopilotView::MasterOff));

    for off in [
        "[harness]\nenabled = false\n",
        "[harness.claude]\nenabled = false\n",
    ] {
        let (mut app, wid, session, mut reader, _host) = claude_pane(120);
        policy(&mut app, off);
        frame(&mut app, wid);
        assert!(matches!(
            autopilot(&app, session).1,
            AutopilotView::MasterOff | AutopilotView::AgentOff(_)
        ));
        let at = super::gesture_tests::hit(&app, wid, super::Chip::Autopilot);
        click_at(&mut app, wid, at);
        assert!(
            app.windows[&wid].claude_lights.sessions[&session]
                .switch_pressed
                .is_some(),
            "the enable gesture was recorded"
        );
        policy(&mut app, "[harness]\n");
        frame(&mut app, wid);
        assert_eq!(autopilot(&app, session), (None, AutopilotView::Cruise));
        let at = super::gesture_tests::hit(&app, wid, super::Chip::Autopilot);
        click_at(&mut app, wid, at);
        assert_eq!(
            autopilot(&app, session),
            (None, AutopilotView::Cruise),
            "readback did not turn the double click's second press into land"
        );
        later(&mut app, wid, session);
        click_at(&mut app, wid, at);
        assert_eq!(autopilot(&app, session).1, AutopilotView::Land);
        frame(&mut app, wid);
        let at = super::gesture_tests::hit(&app, wid, super::Chip::Autopilot);
        click_at(&mut app, wid, at);
        assert_eq!(autopilot(&app, session).1, AutopilotView::Off);
        frame(&mut app, wid);
        let at = super::gesture_tests::hit(&app, wid, super::Chip::Autopilot);
        click_at(&mut app, wid, at);
        assert_eq!(autopilot(&app, session).1, AutopilotView::Cruise);
        assert!(!settings_open(&app), "nothing opened");
        assert!(pty(&mut reader, 0).is_empty(), "no program input");
    }
}

#[test]
fn under_the_master_switch_off_on_turns_the_master_switch_on() {
    let (mut app, wid, session, mut reader, _host) = claude_pane(120);
    policy(&mut app, "[harness]\nenabled = false\n");
    frame(&mut app, wid);
    let off = switch(&app, wid).expect("the master switch off draws the switch");
    assert_eq!(off.face, HarnessSwitch::Off(SwitchOff::Master));
    let rule = top_rule(&app, wid);
    assert!(
        rule.contains(OFF_FACE) && !rule.contains("alab kitty"),
        "{rule:?}"
    );
    assert_eq!(
        not_saved(&app, "harness.enabled"),
        None,
        "control: no write yet"
    );
    super::SWITCHES_LET_GO.with(|n| n.set(0));
    let at = on_button(&app, wid);
    click_at(&mut app, wid, at);
    assert!(!settings_open(&app), "the on button opens nothing");
    assert_eq!(
        super::SWITCHES_LET_GO.with(std::cell::Cell::get),
        1,
        "the click armed `◐ turning on`, and the refusal let it go"
    );
    assert_eq!(
        autopilot(&app, session),
        (None, AutopilotView::MasterOff),
        "no word written: the switch is aterm.toml's"
    );
    let said = not_saved(&app, "harness.enabled").expect("the master switch's write was made");
    assert!(said.contains("harness.enabled = true"), "{said:?}");
    frame(&mut app, wid);
    assert_eq!(
        switch(&app, wid).map(|b| b.face),
        Some(HarnessSwitch::Off(SwitchOff::Master)),
        "the refused write lets `turning on` go"
    );
    policy(&mut app, "[harness]\n");
    frame(&mut app, wid);
    assert_eq!(autopilot(&app, session), (None, AutopilotView::Cruise));
    assert_eq!(switch(&app, wid).map(|b| b.face), Some(HarnessSwitch::On));
    assert!(pty(&mut reader, 0).is_empty());
}

#[test]
fn under_the_agents_switch_off_on_turns_the_agents_switch_on() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    policy(&mut app, "[harness.claude]\nenabled = false\n");
    frame(&mut app, wid);
    let off = switch(&app, wid).expect("the agent's switch off draws the switch");
    assert_eq!(
        off.face,
        HarnessSwitch::Off(SwitchOff::Agent(aterm_phase::Program::Claude))
    );
    assert!(top_rule(&app, wid).contains(OFF_FACE));
    super::SWITCHES_LET_GO.with(|n| n.set(0));
    let at = on_button(&app, wid);
    click_at(&mut app, wid, at);
    assert!(!settings_open(&app), "the on button opens nothing");
    assert_eq!(
        super::SWITCHES_LET_GO.with(std::cell::Cell::get),
        1,
        "the click armed `◐ turning on`, and the refusal let it go"
    );
    assert!(
        not_saved(&app, "harness.claude.enabled").is_some(),
        "Claude Code's own switch was written"
    );
    assert_eq!(not_saved(&app, "harness.enabled"), None, "not the master's");
    assert_eq!(
        autopilot(&app, session),
        (None, AutopilotView::AgentOff(aterm_phase::Program::Claude))
    );
}

#[test]
fn a_switch_turning_on_says_so_and_takes_no_click() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    policy(&mut app, "[harness]\nenabled = false\n");
    frame(&mut app, wid);
    let at_rest = footer_fp(&app, wid);
    assert_eq!(footer_fp(&app, wid), at_rest, "the key stands at rest");
    app.windows
        .get_mut(&wid)
        .expect("window")
        .claude_lights
        .sessions
        .entry(session)
        .or_default()
        .switch_turning = Some((true, Instant::now() + Duration::from_secs(60)));
    assert_ne!(footer_fp(&app, wid), at_rest, "turning moves the key");
    frame(&mut app, wid);
    let turning = switch(&app, wid).expect("the switch, turning");
    assert_eq!(
        (turning.face, turning.button.clone()),
        (HarnessSwitch::Turning, None)
    );
    assert!(
        top_rule(&app, wid).contains(" alab harness \u{25d0} turning on "),
        "{:?}",
        top_rule(&app, wid)
    );
    click_at(&mut app, wid, (turning.frame_row, turning.switch.start + 2));
    assert_eq!(not_saved(&app, "harness.enabled"), None, "no second write");
    assert!(!settings_open(&app));
    policy(&mut app, "[harness]\n");
    frame(&mut app, wid);
    assert_eq!(switch(&app, wid).map(|b| b.face), Some(HarnessSwitch::On));
}

fn drawn_marks(app: &App, wid: WindowId, row: usize) -> Vec<ChromeIcon> {
    app.windows[&wid]
        .input_scratch
        .chrome_rasters
        .iter()
        .filter(|raster| usize::from(raster.row) == row)
        .flat_map(|raster| raster.icons.iter().copied())
        .filter(|i| BandIcon::glass_mark(i.icon.ch()).is_some())
        .collect()
}

#[test]
fn the_switch_draws_its_mark_as_one_set_in_every_state() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let drawn = |app: &App, want: BandIcon, what: &str| {
        let b = switch(app, wid).unwrap_or_else(|| panic!("{what}: a switch"));
        let row = &app.windows[&wid].input_scratch.cells[b.frame_row];
        let at = b
            .switch
            .clone()
            .find(|&c| row[c].ch == want.ch())
            .unwrap_or_else(|| panic!("{what}: the cell keeps {:?}", want.ch()));
        assert_eq!(
            drawn_marks(app, wid, b.frame_row),
            [ChromeIcon {
                col: u16::try_from(at).expect("a frame column"),
                icon: want,
            }],
            "{what}: the mark drawn over its own cell, once"
        );
    };
    drawn(&app, BandIcon::Disc, "on");
    frame(&mut app, wid);
    drawn(&app, BandIcon::Disc, "on, painted again");
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    drawn(&app, BandIcon::Disc, "Autopilot off: the harness on");
    let arm = |app: &mut App, on: bool| {
        app.windows
            .get_mut(&wid)
            .expect("window")
            .claude_lights
            .sessions
            .entry(session)
            .or_default()
            .switch_turning = Some((on, Instant::now() + Duration::from_secs(60)));
    };
    arm(&mut app, false);
    frame(&mut app, wid);
    assert_eq!(
        switch(&app, wid).map(|b| b.face),
        Some(HarnessSwitch::TurningOff)
    );
    drawn(&app, BandIcon::HalfDisc, "turning off");
    policy(&mut app, "[harness]\nenabled = false\n");
    frame(&mut app, wid);
    drawn(&app, BandIcon::Ring, "off");
    arm(&mut app, true);
    frame(&mut app, wid);
    assert_eq!(
        switch(&app, wid).map(|b| b.face),
        Some(HarnessSwitch::Turning)
    );
    drawn(&app, BandIcon::HalfDisc, "turning on");
    frame(&mut app, wid);
    drawn(&app, BandIcon::HalfDisc, "turning on, painted again");
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    for _ in 0..3 {
        app.splice_claude_footer(wid, &plan, route);
        drawn(&app, BandIcon::HalfDisc, "turning on, spliced again");
    }
    let row = switch(&app, wid).expect("the switch").frame_row;
    let ws = app.windows.get_mut(&wid).expect("window");
    crate::claude_card::strip(&mut ws.claude_card, &mut ws.input_scratch);
    assert_eq!(drawn_marks(&app, wid, row), [], "the strip takes it back");
    frame(&mut app, wid);
    set_program(&app, session, None);
    frame(&mut app, wid);
    assert_eq!(switch(&app, wid), None, "control: no switch");
    let rule = footer_row(&app, wid);
    let rows = app.windows[&wid].input_scratch.cells.len();
    assert!(
        (0..rows)
            .filter(|&r| r != rule)
            .all(|r| drawn_marks(&app, wid, r).is_empty()),
        "control: no switch draws no mark"
    );
    assert!(
        drawn_marks(&app, wid, rule)
            .iter()
            .all(|i| i.icon == BandIcon::SlashedRing),
        "control: the rule's chip alone, {:?}",
        row_text(&app, wid, rule)
    );
}

#[test]
fn the_chips_marks_are_drawn_as_the_switchs_are() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let rule = footer_row(&app, wid);
    assert!(
        row_text(&app, wid, rule).contains(WHOLE_FACTS),
        "the facts are on the rule: {:?}",
        row_text(&app, wid, rule)
    );
    assert_eq!(
        drawn_marks(&app, wid, rule),
        [],
        "control: Cruise's ∞ and the facts"
    );
    let at = |app: &App, ch: char| {
        let cells = &app.windows[&wid].input_scratch.cells[rule];
        let col = cells.iter().position(|c| c.ch == ch);
        u16::try_from(col.unwrap_or_else(|| panic!("the rule keeps {ch:?}"))).expect("a column")
    };
    for (mode, icon) in [
        (Autopilot::Off, BandIcon::Ring),
        (Autopilot::Land, BandIcon::ToBar),
    ] {
        assert!(app.set_session_autopilot(session, Some(mode), crate::autopilot::Surface::Chip));
        frame(&mut app, wid);
        let want = [ChromeIcon {
            col: at(&app, icon.ch()),
            icon,
        }];
        assert_eq!(
            drawn_marks(&app, wid, rule),
            want,
            "{mode:?}: the chip's mark"
        );
        frame(&mut app, wid);
        assert_eq!(
            drawn_marks(&app, wid, rule),
            want,
            "{mode:?}: painted again"
        );
        let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
        for k in 0..3 {
            let (_, allocations) = crate::work_counts::allocations_in(|| {
                app.splice_claude_footer(wid, &plan, route);
            });
            assert_eq!(
                drawn_marks(&app, wid, rule),
                want,
                "{mode:?}: spliced again"
            );
            assert!(
                k == 0 || allocations == 0,
                "{mode:?}: splice {k} allocated {allocations}"
            );
        }
        let ws = app.windows.get_mut(&wid).expect("window");
        crate::claude_card::strip(&mut ws.claude_card, &mut ws.input_scratch);
        assert_eq!(
            drawn_marks(&app, wid, rule),
            [],
            "{mode:?}: the strip takes it back"
        );
    }
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    assert_eq!(drawn_marks(&app, wid, rule), [], "control: Cruise again");
}

#[test]
fn the_glass_draws_its_marks_by_the_character_alone() {
    let (mut app, wid, _session, _reader, _host) = claude_pane(120);
    let rule = footer_row(&app, wid);
    let ws = app.windows.get_mut(&wid).expect("window");
    crate::claude_card::strip(&mut ws.claude_card, &mut ws.input_scratch);
    let blank = aterm_core::terminal::RenderCell {
        ch: ' ',
        ..ws.input_scratch.cells[rule][0]
    };
    let cell = |ch: char| aterm_core::terminal::RenderCell { ch, ..blank };
    let mut cells: Vec<_> =
        "\u{25cf}\u{25cb}\u{25d0}\u{25cc}\u{2298}\u{2913}\u{221e}\u{2713}\u{23f8}\u{b7}x"
            .chars()
            .map(cell)
            .collect();
    cells.push(aterm_core::terminal::RenderCell {
        script: aterm_core::terminal::Script::Sub { nudge: 0 },
        ..cell('\u{25d0}')
    });
    cells.push(aterm_core::terminal::RenderCell {
        wide: true,
        ..cell('\u{25cb}')
    });
    ws.claude_card
        .paint_marks(&mut ws.input_scratch, rule, 4, &cells);
    let icon = |col: u16, icon| ChromeIcon { col, icon };
    assert_eq!(
        drawn_marks(&app, wid, rule),
        [
            icon(4, BandIcon::Disc),
            icon(5, BandIcon::Ring),
            icon(6, BandIcon::HalfDisc),
            icon(7, BandIcon::DottedRing),
            icon(8, BandIcon::SlashedRing),
            icon(9, BandIcon::ToBar),
        ]
    );
}

#[test]
fn a_refilled_frames_marks_allocate_only_their_lists() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let refill = |app: &mut App| {
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
    };
    for (mode, rows) in [
        (Autopilot::Cruise, 1),
        (Autopilot::Off, 2),
        (Autopilot::Land, 2),
    ] {
        assert!(app.set_session_autopilot(session, Some(mode), crate::autopilot::Surface::Chip));
        let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
        refill(&mut app);
        let (_, composed) = crate::work_counts::allocations_in(|| {
            app.splice_claude_footer(wid, &plan, route);
        });
        let marked = (0..app.windows[&wid].input_scratch.cells.len())
            .filter(|&r| !drawn_marks(&app, wid, r).is_empty())
            .count();
        assert_eq!(marked, rows, "{mode:?}: the rows that draw a mark");
        assert!(
            composed > marked,
            "{mode:?}: control, the composing pass allocated {composed}"
        );
        for i in 0..6 {
            refill(&mut app);
            let (_, allocations) = crate::work_counts::allocations_in(|| {
                app.splice_claude_footer(wid, &plan, route);
            });
            assert_eq!(
                allocations, marked,
                "{mode:?}: refilled frame {i}, {marked} marked rows"
            );
        }
        let (_, again) = crate::work_counts::allocations_in(|| {
            app.splice_claude_footer(wid, &plan, route);
        });
        assert_eq!(again, 0, "{mode:?}: the same frame spliced again");
    }
}

#[test]
fn a_menu_over_the_top_rule_keeps_the_switch_mark_beside_it() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let b = switch(&app, wid).expect("a switch");
    let row = b.frame_row;
    let col = b
        .switch
        .clone()
        .find(|&c| app.windows[&wid].input_scratch.cells[row][c].ch == BandIcon::Disc.ch())
        .expect("the switch's mark");
    let mark = [ChromeIcon {
        col: u16::try_from(col).expect("a frame column"),
        icon: BandIcon::Disc,
    }];
    assert_eq!(
        drawn_marks(&app, wid, row),
        mark,
        "the mark before the menu"
    );
    let chip = hit(&app, wid, Chip::Autopilot);
    app.open_autopilot_menu(wid, session, chip, false);
    frame(&mut app, wid);
    app.splice_autopilot_menu(wid);
    let rect = app
        .autopilot_menu
        .as_ref()
        .and_then(|m| m.rect)
        .expect("the menu is painted");
    assert!(
        rect.contains(row, rect.col) && !rect.contains(row, col),
        "the card covers the top rule's row, not the mark: {rect:?}, row {row}, mark {col}"
    );
    let cells = &app.windows[&wid].input_scratch.cells[row];
    assert_eq!(
        cells[col].ch,
        BandIcon::Disc.ch(),
        "the cell keeps its mark"
    );
    assert_eq!(
        drawn_marks(&app, wid, row),
        mark,
        "the mark beside the card is still drawn, once"
    );
    let over_card: Vec<ChromeIcon> = app.windows[&wid]
        .input_scratch
        .chrome_rasters
        .iter()
        .filter(|raster| usize::from(raster.row) == row)
        .flat_map(|raster| raster.icons.iter().copied())
        .filter(|i| rect.contains(row, usize::from(i.col)))
        .collect();
    assert_eq!(over_card, [], "control: no icon over the card");
    assert!(app.close_autopilot_menu());
    frame(&mut app, wid);
    app.splice_autopilot_menu(wid);
    assert_eq!(drawn_marks(&app, wid, row), mark, "the mark once it closes");
}

#[test]
fn hovering_on_moves_the_key_and_leaving_puts_it_back() {
    let (mut app, wid, _session, _reader, _host) = claude_pane(120);
    policy(&mut app, MASTER_OFF);
    frame(&mut app, wid);
    let at_rest = footer_fp(&app, wid);
    let row = footer_row(&app, wid) - 2;
    let glass: Vec<_> = app.windows[&wid].input_scratch.cells[row].clone();
    let (r, c) = on_button(&app, wid);
    let (x, y) = px_of(&app, wid, (r, c));
    app.on_cursor_moved(wid, x, y);
    assert_ne!(footer_fp(&app, wid), at_rest, "hover moves the key");
    frame(&mut app, wid);
    let cell = app.windows[&wid].input_scratch.cells[r][c];
    assert!(
        cell.ch != ' ' && cell.underline == aterm_core::terminal::UnderlineStyle::Single,
        "the hovered button is underlined: {cell:?}"
    );
    assert_ne!(cell.bg, glass[c].bg, "its fill lifted");
    assert_ne!(
        app.windows[&wid].input_scratch.cells[row], glass,
        "hover drew"
    );
    app.clear_claude_light_hover(wid);
    assert_eq!(footer_fp(&app, wid), at_rest, "off it, the key is back");
    frame(&mut app, wid);
    assert_eq!(
        app.windows[&wid].input_scratch.cells[row], glass,
        "and the glass"
    );
}

type Case = (&'static str, fn(&mut App), HarnessSwitch);

#[test]
fn the_switch_stands_in_every_hosted_state_and_on_no_other_pane() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let word = |app: &mut App, w: Autopilot| {
        app.set_session_autopilot(session, Some(w), crate::autopilot::Surface::Chip);
    };
    let cases: [Case; 5] = [
        ("cruise", |_| {}, HarnessSwitch::On),
        ("land", |_| {}, HarnessSwitch::On),
        ("off", |_| {}, HarnessSwitch::On),
        (
            "master off",
            |app| policy(app, "[harness]\nenabled = false\n"),
            HarnessSwitch::Off(SwitchOff::Master),
        ),
        (
            "agent off",
            |app| policy(app, "[harness.claude]\nenabled = false\n"),
            HarnessSwitch::Off(SwitchOff::Agent(aterm_phase::Program::Claude)),
        ),
    ];
    for (what, setup, face) in cases {
        policy(&mut app, "[harness]\n");
        word(
            &mut app,
            match what {
                "land" => Autopilot::Land,
                "off" => Autopilot::Off,
                _ => Autopilot::Cruise,
            },
        );
        setup(&mut app);
        frame(&mut app, wid);
        let b = switch(&app, wid).unwrap_or_else(|| panic!("{what}: a switch"));
        assert_eq!(b.face, face, "{what}");
        assert_eq!(b.button.is_some(), !face.is_on(), "{what}: the on button");
        let rule = top_rule(&app, wid);
        assert!(
            rule.contains(if face.is_on() {
                " alab harness \u{25cf} on "
            } else {
                OFF_FACE
            }),
            "{what}: {rule:?}"
        );
    }
    policy(&mut app, "[harness]\n");
    let held = app.harness.take();
    frame(&mut app, wid);
    assert_eq!(autopilot(&app, session).1, AutopilotView::Unhosted);
    assert_eq!(switch(&app, wid), None, "unhosted: no switch");
    let rule = top_rule(&app, wid);
    assert!(
        rule.starts_with("\u{2500}\u{2500}    claude \u{2500}")
            && rule.chars().skip(13).all(|c| c == '\u{2500}'),
        "unhosted: the lockup on the rule as Claude drew it: {rule:?}"
    );
    app.harness = held;
    frame(&mut app, wid);
    assert!(switch(&app, wid).is_some(), "control: hosted again");
    set_program(&app, session, None);
    frame(&mut app, wid);
    assert_eq!(switch(&app, wid), None, "no agent: no switch");
    assert!(
        top_rule(&app, wid).chars().all(|c| c == '\u{2500}'),
        "no agent: the rule as the shell drew it: {:?}",
        top_rule(&app, wid)
    );
}

#[test]
fn a_narrow_pane_keeps_the_on_button_to_the_last() {
    let (mut app, wid, session, _reader, _host) = claude_pane(121);
    policy(&mut app, MASTER_OFF);
    let mut seen: Vec<(u16, Option<usize>)> = Vec::new();
    for width in (6..=120u16).rev() {
        assert!(app.apply_term_resize(wid, 24, width));
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let tier = switch(&app, wid).map(|b| {
            let button = b.button.clone().expect("an off form keeps its button");
            assert_eq!(
                painted(&app, wid, &b, button),
                "\u{2590} on \u{258c}",
                "{width}"
            );
            b.tier
        });
        if let Some(&(wider, Some(last))) = seen.last() {
            assert!(
                tier.is_none_or(|t| t >= last),
                "{width} after {wider}: a form came back: {seen:?}"
            );
        }
        if let Some(&(wider, None)) = seen.last() {
            assert_eq!(tier, None, "{width} after {wider}: the switch came back");
        }
        seen.push((width, tier));
    }
    for tier in 0..4 {
        assert!(
            seen.iter().any(|(_, t)| *t == Some(tier)),
            "CONTROL: form {tier} is met: {seen:?}"
        );
    }
    let narrowest = seen
        .iter()
        .filter(|(_, t)| t.is_some())
        .map(|(w, _)| *w)
        .min();
    assert!(
        narrowest.is_some_and(|w| w <= 12),
        "the button stands in a pane of a dozen columns: {seen:?}"
    );
}

#[cfg(a11y_tree)]
#[test]
fn the_switch_is_a_switch_a_screen_reader_can_press() {
    use crate::a11y_tree::HarnessChip;
    use aterm_a11y::model::Toggled;
    let words = |text: &str| {
        text.split_whitespace()
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let nodes = |app: &App| -> Vec<crate::a11y_tree::GridChip> {
        app.claude_chips_a11y(wid)
            .into_iter()
            .filter(|c| matches!(c.chip, HarnessChip::Switch | HarnessChip::SwitchOn))
            .collect()
    };
    let lit = nodes(&app);
    let b = switch(&app, wid).expect("lit");
    assert_eq!(lit.len(), 1, "{lit:?}");
    assert_eq!(
        (lit[0].chip, lit[0].toggled),
        (HarnessChip::Switch, Some(Toggled::True))
    );
    let visual = words(&painted(&app, wid, &b, b.switch.clone()));
    assert_eq!(visual, "alab harness on");
    assert!(
        crate::native_ui::visible_label_in_name(&visual, &lit[0].label),
        "{visual:?} in {:?}",
        lit[0].label
    );
    let said = lit[0]
        .description
        .clone()
        .expect("the lit switch says what it is");
    assert!(
        said.starts_with("The aterm harness is on (Supervise coding agents).")
            && said.contains("off for every session"),
        "{said:?}"
    );
    let before = autopilot(&app, session);
    assert_eq!(before.1, AutopilotView::Cruise);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::SwitchOn);
    assert_eq!(
        (autopilot(&app, session), not_saved(&app, "harness.enabled")),
        (before, None),
        "`on` on a lit switch is no press"
    );
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    let under_off = nodes(&app);
    assert_eq!(
        under_off
            .iter()
            .map(|n| (n.chip, n.toggled))
            .collect::<Vec<_>>(),
        [(HarnessChip::Switch, Some(Toggled::True))],
        "Autopilot Off: the harness is on, one switch toggled on and no `on` button"
    );
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Switch);
    let wrote = not_saved(&app, "harness.enabled").expect("the reader's press wrote the master");
    assert!(wrote.contains("harness.enabled = false"), "{wrote:?}");
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::Off),
        "the press turned the harness off, not the word"
    );
    later(&mut app, wid, session);
    policy(&mut app, MASTER_OFF);
    frame(&mut app, wid);
    let off = nodes(&app);
    let b = switch(&app, wid).expect("off");
    assert_eq!(off.len(), 2, "{off:?}");
    assert_eq!(
        (off[0].chip, off[0].toggled),
        (HarnessChip::Switch, Some(Toggled::False))
    );
    assert_eq!((off[1].chip, off[1].toggled), (HarnessChip::SwitchOn, None));
    for (node, cols) in [
        (&off[0], b.switch.clone()),
        (&off[1], b.button.clone().unwrap()),
    ] {
        assert_eq!((node.col, node.width), (cols.start, cols.len()), "{node:?}");
        let visual = words(&painted(&app, wid, &b, cols));
        assert!(
            crate::native_ui::visible_label_in_name(&visual, &node.label),
            "{visual:?} in {:?}",
            node.label
        );
        assert!(node.description.is_some(), "{node:?}");
    }
    assert_eq!(
        off[1].label, "on: turn the harness on (Supervise coding agents)",
        "the master switch's `on` turns on the whole harness"
    );
    assert!(app.apply_term_resize(wid, 24, 8));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    assert!(nodes(&app).is_empty(), "no switch on the glass");
    app.press_claude_chip_for_reader(wid, session, HarnessChip::SwitchOn);
    assert!(
        not_saved(&app, "harness.enabled").is_some_and(|said| said.contains("= false")),
        "no switch, no press"
    );
    assert!(app.apply_term_resize(wid, 24, 120));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::SwitchOn);
    let wrote = not_saved(&app, "harness.enabled").expect("the master switch's write");
    assert!(wrote.contains("harness.enabled = true"), "{wrote:?}");
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::MasterOff),
        "`on` writes the switch, never `cruise`"
    );
}

/// THE SWITCH NAMES THE HARNESS IT SWITCHES: lit, or held off by the master switch, the
/// `Switch` and the off face's `on` name the whole harness and no product (`alab harness on`,
/// `on: turn the harness on (Supervise coding agents)`), since a press there turns every agent's
/// harness. NEGATIVE CONTROL: under the agent's own switch off they name the agent's harness.
#[cfg(a11y_tree)]
#[test]
fn the_switch_names_the_harness_it_switches() {
    use crate::a11y_tree::HarnessChip;
    let (mut app, wid, _session, _reader, _host) = claude_pane(120);
    let labels = |app: &App| -> Vec<(HarnessChip, String)> {
        app.claude_chips_a11y(wid)
            .into_iter()
            .filter(|c| matches!(c.chip, HarnessChip::Switch | HarnessChip::SwitchOn))
            .map(|c| (c.chip, c.label))
            .collect()
    };
    let named = |pairs: &[(HarnessChip, &str)]| -> Vec<(HarnessChip, String)> {
        pairs.iter().map(|&(c, l)| (c, l.to_owned())).collect()
    };
    assert_eq!(
        labels(&app),
        named(&[(HarnessChip::Switch, "alab harness on")])
    );
    policy(&mut app, MASTER_OFF);
    frame(&mut app, wid);
    assert_eq!(
        labels(&app),
        named(&[
            (HarnessChip::Switch, "alab harness off"),
            (
                HarnessChip::SwitchOn,
                "on: turn the harness on (Supervise coding agents)"
            ),
        ]),
        "the master switch off"
    );
    policy(&mut app, "[harness.claude]\nenabled = false\n");
    frame(&mut app, wid);
    assert_eq!(
        labels(&app),
        named(&[
            (HarnessChip::Switch, "alab harness off: Claude Code"),
            (HarnessChip::SwitchOn, "on: turn the Claude Code harness on"),
        ]),
        "NEGATIVE CONTROL: the agent's own switch off"
    );
}

#[cfg(a11y_tree)]
#[test]
fn decoration_has_no_node() {
    use crate::a11y_tree::HarnessChip;
    let (app, wid, session, _reader, _host) = claude_pane(120);
    let b = switch(&app, wid).expect("lit");
    let strip = usize::from(app.chrome_rows(wid));
    let rule = top_rule(&app, wid);
    let lockup = "    claude ";
    let at = rule.find(lockup).expect("the lockup on the glass");
    let cols = rule[..at].chars().count()..rule[..at].chars().count() + lockup.chars().count();
    assert_eq!(cols, 2..13, "two `\u{2500}` in: {rule:?}");
    let product = b.program.product();
    let (words, _) = crate::claude_footer::switch_words(&b);
    let names = |label: &str| crate::native_ui::visible_label_in_name(&words, label);
    let chips = app.claude_chips_a11y(wid);
    let switches: Vec<_> = chips
        .iter()
        .filter(|c| c.session == session && c.chip == HarnessChip::Switch)
        .collect();
    assert_eq!(switches.len(), 1, "{chips:?}");
    assert_eq!(
        switches[0].label, "alab harness on",
        "the whole harness, no product"
    );
    assert!(names(&switches[0].label), "{:?}", switches[0].label);
    assert_eq!(
        crate::claude_lights::switch_label(&words, b.face, product),
        switches[0].label
    );
    let over: Vec<_> = chips
        .iter()
        .filter(|c| {
            c.row + strip == b.frame_row && c.col < cols.end && c.col + c.width.max(1) > cols.start
        })
        .collect();
    assert!(over.is_empty(), "no node over the lockup: {over:?}");
    let tree = app.grid_a11y_tree(wid, None).expect("the grid's tree");
    let held: Vec<_> = tree
        .nodes
        .iter()
        .filter(|(_, n)| {
            let holds =
                |s: &str| s.contains("\u{2500}    claude") || s.contains("    claude \u{2500}");
            n.label().is_some_and(holds) || n.value().is_some_and(holds)
        })
        .map(|(_, n)| n.role())
        .collect();
    assert!(held.is_empty(), "no node names the lockup: {held:?}");
    assert!(
        !names("Claude Code harness: on."),
        "CONTROL: the old badge's name, which leaves out the words drawn, is refused"
    );
}

#[test]
fn the_harness_tint_is_the_rules_and_only_its_mark_and_switch_are_held() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    frame(&mut app, wid);
    app.splice_autopilot_menu(wid);
    let b = switch(&app, wid).expect("a hosted Claude Code pane draws the switch");
    assert!(
        b.face == HarnessSwitch::On,
        "fixture: the harness carries the session: {b:?}"
    );
    let row: Vec<char> = app.windows[&wid].input_scratch.cells[b.frame_row]
        .iter()
        .map(|c| c.ch)
        .collect();
    let held = app.windows[&wid].pet_chrome.held().to_vec();
    let covered = |col: usize| {
        let (r, c) = (b.term_row as f32 + 0.5, col as f32 + 0.5);
        held.iter().any(|rect| {
            rect.row <= r && r < rect.row + rect.rows && rect.col <= c && c < rect.col + rect.cols
        })
    };
    let strokes: Vec<usize> = (0..row.len()).filter(|&c| row[c] == '\u{2500}').collect();
    let marks: Vec<usize> = (0..row.len())
        .filter(|&c| row[c] != '\u{2500}' && row[c] != ' ')
        .collect();
    assert!(
        strokes.len() > 60 && marks.len() >= 20 && marks.iter().any(|&c| c < 20),
        "fixture: the tinted rule, the lockup at its left, the switch: {:?}",
        row.iter().collect::<String>()
    );
    let held_strokes: Vec<usize> = strokes.iter().copied().filter(|&c| covered(c)).collect();
    assert!(
        held_strokes.is_empty(),
        "{} of the rule's {} strokes held as the host's chrome, from column {:?}",
        held_strokes.len(),
        strokes.len(),
        held_strokes.first()
    );
    let free: Vec<usize> = marks.iter().copied().filter(|&c| !covered(c)).collect();
    assert!(
        free.is_empty(),
        "the mark's and the switch's cells are the host's: {free:?} free"
    );
    assert!(
        (b.cols.start..b.cols.end).all(&covered),
        "the switch's cells, spaces and all"
    );
}

#[test]
fn a_pointed_agents_panel_row_keeps_the_switch_the_facts_and_the_chip() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let rule = "\u{2500}".repeat(120);
    let strip = footer_row(&app, wid) - 22;
    let (main, agent) = ("\u{23FA} main", "\u{25EF} general-purpose  Searching");
    for (main, agent) in [
        (format!("  {main}"), format!("  {agent}")),
        (format!("  {main}"), format!("\u{276F} {agent}")),
        (format!("\u{276F} {main}"), format!("  {agent}")),
    ] {
        let screen = format!(
            "\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[2J\x1b[16;1H\u{25CF} done\x1b[18;1H{rule}\
             \x1b[19;1H\u{276F} \x1b[20;1H{rule}\x1b[21;1H  \u{23F5}\u{23F5} bypass permissions \
             on \u{B7} 2 shells \u{B7} \u{2193} to manage\x1b[23;1H{main}\x1b[24;1H{agent}\
             \x1b[19;3H"
        );
        feed(&app, session, screen.as_bytes());
        frame(&mut app, wid);
        let b = switch(&app, wid).unwrap_or_else(|| panic!("{main:?} / {agent:?}: no switch"));
        assert_eq!(
            b.frame_row,
            strip + 17,
            "{main:?} / {agent:?}: the top rule"
        );
        let face = painted(&app, wid, &b, b.switch.clone());
        assert!(face.contains("alab harness \u{25cf} on"), "{face:?}");
        let facts = row_text(&app, wid, strip + 19);
        assert!(
            facts.contains(WHOLE_FACTS),
            "{main:?} / {agent:?}: {facts:?}"
        );
        assert_eq!(
            hit(&app, wid, Chip::Autopilot).0,
            strip + 19,
            "{main:?} / {agent:?}: the chip in the bottom rule"
        );
    }
}

#[test]
fn the_switch_reads_on_a_tab_theme_the_instance_did_not_choose() {
    use aterm_effects::color_math::contrast_ratio;
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    policy(&mut app, "[harness]\nenabled = false\n");
    let dracula = aterm_types::scheme::builtin("Dracula").expect("a built-in");
    app.theme = aterm_render::Theme::from(&dracula);
    app.set_session_theme(
        session,
        Some(crate::session_theme::SessionTheme {
            selector: "Default Light".into(),
            origin: crate::session_theme::SessionThemeOrigin::Ctl,
        }),
    );
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let off = switch(&app, wid).expect("the master switch off draws the switch");
    assert_eq!(off.face, HarnessSwitch::Off(SwitchOff::Master));
    assert!(
        top_rule(&app, wid).contains(OFF_FACE),
        "{:?}",
        top_rule(&app, wid)
    );
    let paper = aterm_types::scheme::builtin("Default Light")
        .expect("a built-in")
        .background;
    let ground = [paper.r, paper.g, paper.b];
    let row = &app.windows[&wid].input_scratch.cells[off.frame_row];
    assert_eq!(
        row[off.cols.start].bg, ground,
        "the tab's own paper is the switch's ground"
    );
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let accent = crate::chrome_band::band_colors(app.theme).accent;
    assert!(
        reads(accent, ground) < 1.5,
        "CONTROL: the instance's accent reads {:.2} on the tab's paper",
        reads(accent, ground)
    );
    let button = off.button.clone().expect("the off switch's button");
    let floor = aterm_effects::cli_marks::NAME_CONTRAST;
    let mut letters = 0;
    for (col, c) in row.iter().enumerate().take(button.end).skip(off.cols.start) {
        if c.ch == ' ' {
            continue;
        }
        let what = format!("{:?} at column {col}", c.ch);
        if button.contains(&col) && c.bg == ground {
            assert!(
                reads(c.fg, ground) >= 3.0,
                "{what}: a cap at {:.2}",
                reads(c.fg, ground)
            );
        } else {
            letters += 1;
            assert!(
                reads(c.fg, c.bg) >= floor,
                "{what}: a letter at {:.2}",
                reads(c.fg, c.bg)
            );
            if button.contains(&col) {
                assert!(
                    reads(c.bg, ground) >= 3.0,
                    "{what}: the pill at {:.2}",
                    reads(c.bg, ground)
                );
            }
        }
    }
    assert_eq!(
        letters,
        "alabharness○offon".chars().count(),
        "every letter read"
    );
}

#[test]
fn the_facts_and_the_chip_read_on_a_tab_theme_the_instance_did_not_choose() {
    use aterm_effects::color_math::contrast_ratio;
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let dracula = aterm_types::scheme::builtin("Dracula").expect("a built-in");
    app.theme = aterm_render::Theme::from(&dracula);
    app.set_session_theme(
        session,
        Some(crate::session_theme::SessionTheme {
            selector: "Default Light".into(),
            origin: crate::session_theme::SessionThemeOrigin::Ctl,
        }),
    );
    let grey = "\x1b[38;2;153;153;153m";
    let rule = "\u{2500}".repeat(120);
    feed(
        &app,
        session,
        format!(
            "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{grey}{rule}\x1b[39m\
             \x1b[22;1H\u{276F} \x1b[23;1H{grey}{rule}\x1b[39m\x1b[24;1H{BYPASS}\x1b[22;3H"
        )
        .as_bytes(),
    );
    frame(&mut app, wid);
    let paper = aterm_types::scheme::builtin("Default Light")
        .expect("a built-in")
        .background;
    let ground = [paper.r, paper.g, paper.b];
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let row = &app.windows[&wid].input_scratch.cells[footer_row(&app, wid)];
    let text = row_text(&app, wid, footer_row(&app, wid));
    assert!(
        text.contains("\u{221e}") && text.contains("Opus 5.5"),
        "the chip and the facts are on the rule: {text:?}"
    );
    let marks = ['\u{25C6}', '\u{2302}', '\u{2387}'];
    let mut seen = 0;
    for c in row.iter().filter(|c| c.ch != ' ') {
        if marks.contains(&c.ch) {
            assert_eq!(c.bg, ground, "the tab's own paper is the facts' ground");
            assert!(
                reads(c.fg, ground) >= aterm_effects::cli_marks::ICON_CONTRAST,
                "{:?} at {:.2}:1: {text:?}",
                c.ch,
                reads(c.fg, ground)
            );
            seen += 1;
        } else if c.ch == '\u{221e}' {
            assert_eq!(c.bg, ground, "the tab's own paper is the chip's ground");
            assert!(
                reads(c.fg, ground) >= aterm_effects::cli_marks::NAME_CONTRAST,
                "`\u{221e}` at {:.2}:1",
                reads(c.fg, ground)
            );
            seen += 1;
        }
    }
    assert_eq!(
        seen,
        marks.len() + 1,
        "every mark and the chip read: {text:?}"
    );
    let accent = crate::chrome_band::band_colors(app.theme).accent;
    let old: [u8; 3] =
        std::array::from_fn(|i| ((f32::from(accent[i]) + f32::from(0x99_u8)) / 2.0).round() as u8);
    assert!(
        reads(old, ground) < aterm_effects::cli_marks::ICON_CONTRAST,
        "CONTROL: the old mark at {:.2}:1 on the tab's paper",
        reads(old, ground)
    );
    assert!(
        reads(accent, ground) < 1.5,
        "CONTROL: the accent at {:.2}:1 on the tab's paper",
        reads(accent, ground)
    );
}

#[test]
fn the_facts_words_read_on_a_tab_theme_of_its_own() {
    use aterm_agent::harness::footer::{FactKind, FactMask};
    use aterm_effects::cli_marks::{ICON_CONTRAST, NAME_CONTRAST};
    use aterm_effects::color_math::contrast_ratio;
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    app.set_session_theme(
        session,
        Some(crate::session_theme::SessionTheme {
            selector: "Default Light".into(),
            origin: crate::session_theme::SessionThemeOrigin::Ctl,
        }),
    );
    let grey = "\x1b[38;2;153;153;153m";
    let rule = "\u{2500}".repeat(120);
    feed(
        &app,
        session,
        format!(
            "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{grey}{rule}\x1b[39m\
             \x1b[22;1H\u{276F} \x1b[23;1H{grey}{rule}\x1b[39m\x1b[24;1H{BYPASS}\x1b[22;3H"
        )
        .as_bytes(),
    );
    app.set_fact_mask(session, FactMask::default().with(FactKind::Branch, true));
    frame(&mut app, wid);
    let light = aterm_types::scheme::builtin("Default Light").expect("a built-in");
    let ground = [light.background.r, light.background.g, light.background.b];
    let ink = [light.foreground.r, light.foreground.g, light.foreground.b];
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let at = footer_row(&app, wid);
    let text = row_text(&app, wid, at);
    assert!(
        text.contains("Opus 5.5") && text.contains("\u{2387} main off"),
        "the facts and the branch's stub are on the rule: {text:?}"
    );
    let row = &app.windows[&wid].input_scratch.cells[at];
    let marks = ['\u{25C6}', '\u{2302}', '\u{2387}'];
    let words: Vec<_> = row
        .iter()
        .filter(|c| {
            c.ch != ' '
                && !marks.contains(&c.ch)
                && matches!(c.script, aterm_core::terminal::Script::Sub { .. })
        })
        .collect();
    let values: Vec<f32> = words
        .iter()
        .filter(|c| c.bold)
        .map(|c| reads(c.fg, ground))
        .collect();
    let stubs: Vec<(char, f32)> = words
        .iter()
        .filter(|c| !c.bold)
        .map(|c| (c.ch, reads(c.fg, ground)))
        .collect();
    assert!(
        words.iter().all(|c| c.bg == ground),
        "the tab's own paper is the facts' ground"
    );
    let quietest = values.iter().copied().fold(f32::INFINITY, f32::min);
    assert!(
        !values.is_empty() && quietest >= NAME_CONTRAST,
        "every value at 4.5:1 or more, the quietest at {quietest:.2}:1: {text:?}"
    );
    assert_eq!(
        stubs.iter().map(|&(ch, _)| ch).collect::<String>(),
        "mainoff",
        "the stub's letters: {text:?}"
    );
    for (ch, k) in stubs {
        assert!(
            k >= ICON_CONTRAST && k < quietest,
            "{ch:?} of the stub at {k:.2}:1, the quietest value at {quietest:.2}:1"
        );
    }
    let old_value: [u8; 3] = std::array::from_fn(|i| {
        (f32::from(0x99_u8) + (f32::from(ink[i]) - f32::from(0x99_u8)) / 3.0).round() as u8
    });
    assert!(
        reads(old_value, ground) < NAME_CONTRAST,
        "CONTROL: the old value at {:.2}:1",
        reads(old_value, ground)
    );
    assert!(
        reads([0x99; 3], ground) < ICON_CONTRAST,
        "CONTROL: the old stub at {:.2}:1",
        reads([0x99; 3], ground)
    );
}

/// THE KEYBOARD REACHES THE HARNESS SWITCH (review 13): ctrl+shift+tab
/// walks a pane's chips and facts and then its top rule's switch, lit as
/// the pointer lights it, and Return there presses it as a click does: the
/// whole harness turns off (`harness.enabled = false` is written, no
/// Autopilot word) and nothing is typed. A Gemini pane, whose rule draws
/// the switch and no chip or fact, reaches it too. NEGATIVE CONTROL: Escape
/// on the switch lets it go and turns nothing; a shell's pane, no switch,
/// selects nothing.
#[cfg(target_os = "macos")]
#[test]
fn the_keyboard_walks_to_the_switch_and_return_presses_it() {
    use super::Target;
    use super::gesture_tests::{chord, tap};
    use aterm_window::keyboard::{KeyCode, NamedKey};
    let selected = |app: &App, wid: WindowId| app.windows[&wid].claude_lights.selected;
    let walk = |app: &mut App, wid: WindowId, session: u64| {
        let order = app.windows[&wid].claude_lights.revealable(session);
        assert_eq!(
            order.last(),
            Some(&Target::Switch),
            "the walk's last stop: {order:?}"
        );
        for _ in 0..order.len() {
            chord(app, wid);
            frame(app, wid);
            if selected(&*app, wid) == Some((session, Target::Switch)) {
                break;
            }
        }
        assert_eq!(
            selected(&*app, wid),
            Some((session, Target::Switch)),
            "{order:?}"
        );
        assert!(
            app.windows[&wid]
                .claude_lights
                .switch_shown(session, HarnessSwitch::On)
                .1,
            "lit as the pointer lights it"
        );
    };
    let (mut app, wid, session, mut reader, _host) = claude_pane(120);
    assert_eq!(
        switch(&app, wid).map(|b| b.face),
        Some(HarnessSwitch::On),
        "CONTROL"
    );
    let before = autopilot(&app, session);
    walk(&mut app, wid, session);
    tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
    assert_eq!(
        selected(&app, wid),
        None,
        "NEGATIVE CONTROL: Escape lets it go"
    );
    assert_eq!(
        autopilot(&app, session),
        before,
        "NEGATIVE CONTROL: nothing turned"
    );
    frame(&mut app, wid);
    assert_eq!(
        not_saved(&app, "harness.enabled"),
        None,
        "NEGATIVE CONTROL: nothing written"
    );
    walk(&mut app, wid, session);
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert!(
        not_saved(&app, "harness.enabled").is_some_and(|said| said.contains("= false")),
        "Return is its click"
    );
    assert_eq!(autopilot(&app, session), before, "no word written");
    assert_eq!(selected(&app, wid), None);
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");

    let (mut app, wid, session, mut reader) = super::gesture_tests::app();
    assert!(app.apply_term_resize(wid, 50, 144));
    let _host = host(&mut app);
    set_program(&app, session, Some("gemini"));
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    let rows: Vec<String> = include_str!("../../aterm-phase/src/fixtures/gemini-0.46.0-idle.txt")
        .lines()
        .filter(|l| !l.starts_with("# "))
        .map(str::to_owned)
        .collect();
    crate::claude_footer::probe::feed(&app, session, &rows);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let b = switch(&app, wid).expect("Gemini's rule draws the switch");
    assert_eq!(
        (b.program, b.face),
        (aterm_phase::Program::Gemini, HarnessSwitch::On),
        "CONTROL"
    );
    let before = autopilot(&app, session);
    walk(&mut app, wid, session);
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert!(
        not_saved(&app, "harness.enabled").is_some_and(|said| said.contains("= false")),
        "Gemini: its click"
    );
    assert_eq!(autopilot(&app, session), before, "Gemini: no word written");
    assert!(pty(&mut reader, 0).is_empty(), "Gemini: nothing typed");

    set_program(&app, session, None);
    frame(&mut app, wid);
    assert_eq!(
        switch(&app, wid),
        None,
        "CONTROL: a shell's pane has no switch"
    );
    chord(&mut app, wid);
    assert_eq!(
        selected(&app, wid),
        None,
        "NEGATIVE CONTROL: nothing to select"
    );
}

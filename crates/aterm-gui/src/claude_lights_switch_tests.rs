// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE HARNESS SWITCH ON THE COMPOSER'S TOP RULE (owner, 2026-10-20: "there
//! needs to be an \"on\" button drawn for the claude harness"; design §2.3):
//! wherever a host is, Claude Code's top rule draws ONE switch at its right,
//! `alab harness ● on` — lit while the harness carries the session, and in
//! every off state (the session's own word, its agent's switch, the master
//! switch) `alab harness ○ off ▐ on ▌`, whose `on`
//! button turns the harness on from whichever switch holds it off, opening
//! nothing. Driven through the window's own pointer path on the real
//! engine, read off the frame the capture route splices.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;
use aterm_window::event::{ElementState, MouseButton};

use super::gesture_tests::{
    BYPASS, Hosted, PGID, WHOLE_FACTS, app, autopilot, draw_claude, feed, footer_fp, footer_row,
    frame, hit, host, pty, publish_facts, px_of, row_text,
};
use super::{BorderSwitch, Chip};
use crate::autopilot::{AutopilotView, HarnessSwitch, SwitchOff};
use crate::native_settings::SettingsRoute;
use crate::{App, WindowId};

/// A Claude Code pane `cols` wide in a hosted window: its program read, its
/// facts published, its composer drawn, one frame painted.
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

fn set_program(app: &App, session: u64, program: Option<&str>) {
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_program(PGID, program.map(Into::into));
}

/// The policy in force, as the config watcher would post `text`.
fn policy(app: &mut App, text: &str) {
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read(text),
    );
}

/// The composer's top rule as the frame paints it, glyph by glyph.
fn top_rule(app: &App, wid: WindowId) -> String {
    app.windows[&wid].input_scratch.cells[footer_row(app, wid) - 2]
        .iter()
        .map(|c| c.ch)
        .collect()
}

/// What the cells `cols` of switch `b`'s row paint.
fn painted(app: &App, wid: WindowId, b: &BorderSwitch, cols: std::ops::Range<usize>) -> String {
    let row = &app.windows[&wid].input_scratch.cells[b.frame_row];
    assert!(
        cols.end <= row.len(),
        "{cols:?} past a row of {}: {b:?}",
        row.len()
    );
    row[cols].iter().map(|c| c.ch).collect()
}

/// The switch the frame drew, if any (one pane, one switch at most).
fn switch(app: &App, wid: WindowId) -> Option<BorderSwitch> {
    let toggles = &app.windows[&wid].claude_lights.border_toggles;
    assert!(toggles.len() <= 1, "one pane, one switch: {toggles:?}");
    toggles.first().cloned()
}

/// A left click at frame cell `cell`, as a person makes one.
fn click_at(app: &mut App, wid: WindowId, cell: (usize, usize)) {
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
}

/// The middle cell of the drawn switch's `on` button.
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

/// The message the switch's refused write posted for `key`, if any.
fn not_saved(app: &App, key: &str) -> Option<String> {
    let want = format!("harness.switch-save.{key}");
    app.messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some(want.as_str()))
        .map(|r| r.detail.join(" "))
}

/// The off switch's face, every form: label, state, the `on` button.
const OFF_FACE: &str = " alab harness \u{25cb} off \u{2590} on \u{258c} ";

/// AN OFF SESSION'S `on` TURNS IT ON (the session's own word `off`): the
/// lit switch reads `alab harness ● on`; Off draws `alab harness ○ off
/// ▐ on ▌` — no wordmark, the switch alone — and a click on `on` writes the
/// session's word `cruise`, so the next frame's switch is lit again; and
/// the lit switch's click turns it off, as before. Nothing reaches the PTY
/// and nothing opens. NEGATIVE CONTROL: a click on the rule beside the
/// switch changes nothing (on main an Off session's rule drew no button,
/// and its invisible pill turned it on from any of its cells).
#[test]
fn an_off_sessions_on_button_turns_it_on() {
    let (mut app, wid, session, mut reader, _host) = claude_pane(120);
    let lit = switch(&app, wid).expect("a hosted Claude Code pane draws the switch");
    assert_eq!((lit.face, lit.button.clone()), (HarnessSwitch::On, None));
    assert!(
        top_rule(&app, wid).contains(" alab harness \u{25cf} on "),
        "{:?}",
        top_rule(&app, wid)
    );
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    let off = switch(&app, wid).expect("Off draws the switch");
    assert_eq!(off.face, HarnessSwitch::Off(SwitchOff::Session));
    let rule = top_rule(&app, wid);
    assert!(
        rule.contains(OFF_FACE) && !rule.contains("alab kitty"),
        "{rule:?}"
    );
    assert_eq!(
        painted(&app, wid, &off, off.button.clone().unwrap()),
        "\u{2590} on \u{258c}"
    );
    // NEGATIVE CONTROL: the rule beside the switch.
    click_at(&mut app, wid, (off.frame_row, off.cols.start - 2));
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::Off),
        "control: the rule beside the switch is no button"
    );
    // The `on` button.
    let at = on_button(&app, wid);
    click_at(&mut app, wid, at);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Cruise), AutopilotView::Cruise),
        "on: the session's word is cruise"
    );
    assert!(!settings_open(&app), "nothing opens");
    frame(&mut app, wid);
    let lit = switch(&app, wid).expect("the switch, lit");
    assert_eq!(lit.face, HarnessSwitch::On);
    // The lit switch's click, a separate click later, turns it off again.
    later(&mut app, wid, session);
    click_at(&mut app, wid, (lit.frame_row, lit.switch.start + 2));
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::Off)
    );
    assert!(
        pty(&mut reader, 0).is_empty(),
        "nothing reached the program"
    );
}

/// A DOUBLE CLICK ON `on` LEAVES THE HARNESS ON (review of k2/harnesson,
/// finding 1): in a real window a frame paints between a double click's
/// two clicks, so the second lands on the switch the first lit; it is the
/// same gesture's and does nothing, and the session stays in cruise. The
/// same holds the other way: a double click on the lit switch leaves it
/// off. NEGATIVE CONTROL: a separate click after a double click's interval
/// flips it (on the branch before the fix the second click wrote `off`).
#[test]
fn a_double_click_on_on_leaves_the_harness_on() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    let at = on_button(&app, wid);
    click_at(&mut app, wid, at);
    assert_eq!(
        autopilot(&app, session).1,
        AutopilotView::Cruise,
        "first click"
    );
    frame(&mut app, wid);
    assert_eq!(
        switch(&app, wid).map(|b| b.face),
        Some(HarnessSwitch::On),
        "the frame between lit the switch under the pointer"
    );
    click_at(&mut app, wid, at);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Cruise), AutopilotView::Cruise),
        "the second click of the double click turned nothing back"
    );
    // NEGATIVE CONTROL: a separate click later turns it off…
    later(&mut app, wid, session);
    frame(&mut app, wid);
    let lit = switch(&app, wid).expect("lit");
    let at = (lit.frame_row, lit.switch.start + 2);
    click_at(&mut app, wid, at);
    assert_eq!(
        autopilot(&app, session).1,
        AutopilotView::Off,
        "a later click"
    );
    // …and a double click on the lit switch leaves it off, its second click
    // on the `on` the frame between drew.
    frame(&mut app, wid);
    let at = on_button(&app, wid);
    click_at(&mut app, wid, at);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::Off),
        "the double click's second click on the off switch turned nothing on"
    );

    // The bottom off chip uses the same enable gesture. A watcher readback
    // between its two clicks must not turn the second into Cruise -> Land.
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
        // The captured policy adoption is successful readback, unlike the
        // immediate no-proxy write refusal used by the older chip controls.
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
        // A genuinely separate click still cycles; mode-only clicks do not
        // stamp a new enable gesture and may continue cycling immediately.
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

/// THE MASTER SWITCH OFF (`[harness] enabled = false`): the same switch,
/// `○ off ▐ on ▌`, and a click on `on` writes aterm.toml's master switch
/// `true` through the config lane — the switch the owner sees off is the
/// switch the click turns on, so one click is all it takes (the bottom
/// rule's `⊘ off` chip does the same, `claude_lights`' gesture tests) —
/// never the session's word, and Settings stays shut. This window has no
/// event loop, so the lane refuses the write at once: the switch's own
/// message names the key, and the `◐ turning on` the click armed lets go.
/// The policy read back with the switch on (what the watcher posts once a
/// write lands) lights it, the session under the table's default.
/// NEGATIVE CONTROL: with no click, no write and no message.
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

/// CLAUDE CODE'S OWN SWITCH OFF (`[harness.claude] enabled = false`, the
/// master on): `○ off ▐ on ▌` again, and `on` writes that switch `true` —
/// not the master, not the word — opening nothing.
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

/// A WRITE IN FLIGHT (`HarnessSwitch::Turning`): while the config lane
/// writes the switch, the face says `◐ turning on`, has no button, and a
/// click does nothing — a double click never writes twice; the policy read
/// back lights it. The repaint key moves with it and stands at rest.
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
        .switch_turning = Some(Instant::now() + Duration::from_secs(60));
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

/// THE POINTER OVER `on` (hover): the button is drawn as what a press acts
/// on (underlined, its fill lifted) and the repaint key moves; off it again, the key
/// and the glass are what they were. Idle to zero: nothing else moves it.
#[test]
fn hovering_on_moves_the_key_and_leaving_puts_it_back() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
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

/// One state a hosted pane can be in: its name, what puts the policy there,
/// and the switch it draws.
type Case = (&'static str, fn(&mut App), HarnessSwitch);

/// WHERE THE SWITCH STANDS: in every state a host gives a Claude Code pane —
/// Cruise and Land lit, Off, the master switch off and the agent's off with
/// the `on` button — and on no pane without a host (Unhosted: nothing
/// supervises, so no switch, the lockup alone on Claude's rule) or without
/// an agent (a shell's rule, as the shell drew it). NEGATIVE CONTROL: the same pane with its host back draws
/// it again.
#[test]
fn the_switch_stands_in_every_hosted_state_and_on_no_other_pane() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    // The word each case runs under (a word already standing writes
    // nothing, so its answer is not asked).
    let word = |app: &mut App, w: Autopilot| {
        app.set_session_autopilot(session, Some(w), crate::autopilot::Surface::Chip);
    };
    let cases: [Case; 5] = [
        ("cruise", |_| {}, HarnessSwitch::On),
        ("land", |_| {}, HarnessSwitch::On),
        ("off", |_| {}, HarnessSwitch::Off(SwitchOff::Session)),
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
    // No host: nothing supervises — no switch, a plain rule.
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
    // NEGATIVE CONTROL: the host back.
    app.harness = held;
    frame(&mut app, wid);
    assert!(switch(&app, wid).is_some(), "control: hosted again");
    // No agent: a shell's rule.
    set_program(&app, session, None);
    frame(&mut app, wid);
    assert_eq!(switch(&app, wid), None, "no agent: no switch");
    assert!(
        top_rule(&app, wid).chars().all(|c| c == '\u{2500}'),
        "no agent: the rule as the shell drew it: {:?}",
        top_rule(&app, wid)
    );
}

/// A NARROW PANE (`footer::fit_rule`'s rule for the top rule: the switch
/// gives way LATE, after its words): narrowing column by column from 120,
/// the off switch sheds `harness`, then `alab`, then `○ off`, and keeps
/// its `on` button to the last — never a form back once a wider pane gave
/// it up — and goes only where the rule has no room for `▐ on ▌` with a
/// rule cell either side. CONTROL: every form is met.
#[test]
fn a_narrow_pane_keeps_the_on_button_to_the_last() {
    let (mut app, wid, session, _reader, _host) = claude_pane(121);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
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

/// A SCREEN READER'S SWITCH (WCAG 2.5.3; `a11y_tree::HarnessChip`): the
/// lit switch is one `Switch` node, toggled on, named by the words it
/// paints; the off switch is a `Switch` node toggled off and an `on`
/// button whose name begins with the word it paints (`on: turn the Claude
/// Code harness on`), each name holding the painted words whole and in
/// order, and the reader's press of `on` turns the session on — and,
/// pressed while the switch is lit, does nothing. NEGATIVE
/// CONTROL: a pane too narrow for the switch has no node, and the reader's
/// press for it does nothing.
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
    // A press of `on` while the switch is lit — a stale or repeated request
    // for a button the glass no longer shows — does nothing (review of
    // k2/harnesson, finding 3: it turned the harness OFF).
    let before = autopilot(&app, session);
    assert_eq!(before.1, AutopilotView::Cruise);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::SwitchOn);
    assert_eq!(
        autopilot(&app, session),
        before,
        "`on` on a lit switch is no press"
    );
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
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
    assert_eq!(off[1].label, "on: turn the Claude Code harness on");
    // NEGATIVE CONTROL: too narrow for the switch.
    assert!(app.apply_term_resize(wid, 24, 8));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    assert!(nodes(&app).is_empty(), "no switch on the glass");
    app.press_claude_chip_for_reader(wid, session, HarnessChip::SwitchOn);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::Off)
    );
    // The reader's press of `on`.
    assert!(app.apply_term_resize(wid, 24, 120));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::SwitchOn);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Cruise), AutopilotView::Cruise)
    );
}

/// DECORATION HAS NO NODE (spec §8; WCAG 1.1.1 and 2.5.3): a Claude Code
/// pane wearing the lockup — the CLI's icon, a sprite over two blank cells,
/// and its name — publishes the switch as one node named by the words it
/// paints and the product it no longer spells, `alab harness on: Claude
/// Code`, and no node over the lockup's cells, nor any node of the tree that
/// names or holds them: the lockup is decoration, the switch the control.
///
/// NEGATIVE CONTROL: hb-int's law (a name that holds `harness` is the
/// painted words alone) names the switch `alab harness on`, which lacks the
/// product, and the name check refuses it.
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
    // The name holds the painted words whole and in order, then the product.
    let names = |label: &str| {
        crate::native_ui::visible_label_in_name(&words, label) && label.ends_with(product)
    };
    let chips = app.claude_chips_a11y(wid);
    let switches: Vec<_> = chips
        .iter()
        .filter(|c| c.session == session && c.chip == HarnessChip::Switch)
        .collect();
    assert_eq!(switches.len(), 1, "{chips:?}");
    assert_eq!(switches[0].label, "alab harness on: Claude Code");
    assert!(names(&switches[0].label), "{:?}", switches[0].label);
    assert_eq!(
        crate::claude_lights::switch_label(&words, product),
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
    // NEGATIVE CONTROL: hb-int's law.
    let hb_int = |words: &str, product: &str| {
        if words.contains("harness") {
            words.to_owned()
        } else {
            format!("{words}: {product} harness")
        }
    };
    let old = hb_int(&words, product);
    assert_eq!(old, "alab harness on", "CONTROL: hb-int's name");
    assert!(
        !names(&old),
        "CONTROL: a name without the product is refused"
    );
}

/// THE HARNESS'S TINT IS THE RULE'S; ITS LOCKUP AND SWITCH ARE THE HOST'S,
/// for the resident pet (`crate::pet_chrome`). With the harness carrying the
/// session the composer's top rule is repainted whole in its tint, the CLI's
/// lockup at its left and the lit switch at its right. The strokes are the
/// rule they were (the pet reads the program's own rule as ink); only the
/// lockup's cells (its pads, the icon's two cells under its sprite, the
/// name) and the switch's are protected ground. Noted whole, the rule
/// refused every stand on the composer's first line (the pet's 1.7-row body
/// reaches into it), and the cat waited off the line, up to fifty columns
/// behind the caret, and bounded back at every line (2026-10-20, real Claude
/// Code 2.1.305 with the harness on, windowed).
///
/// NEGATIVE CONTROL (as run on hb-int): the write noted whole
/// (`PetChrome::note` over its length), every one of the rule's strokes is
/// held.
#[test]
fn the_harness_tint_is_the_rules_and_only_its_mark_and_switch_are_held() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    // The host holds the session's live supervisor claim: the harness
    // carries it, and its tint, lockup and switch stand on the rule.
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
    // The menu's splice ends the pet's pass (`crate::pet_chrome`).
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
    // Pane-local: one pane, at the window's first column.
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

/// A POINTED AGENTS-PANEL ROW LEAVES THE OVERLAY STANDING (owner, 2026-10-21:
/// the facts, the chips and the switch went when the pointer reached the
/// bottom of the screen): Claude Code 2.1.307's full-screen renderer (the
/// alternate screen, `1003` mouse tracking) hangs its agents panel under the
/// mode row and a blank, and points at the row the pointer hovers with `❯ `
/// in column 0. Pointed or not, the switch stands on the composer's top rule
/// and the facts and the Autopilot chip in its bottom rule, on the same rows.
/// RED with a876b81f9d's reader reverted, which read the pointed row as the
/// caret, lost the frame and drew none of them. CONTROL: the panel unpointed.
#[test]
fn a_pointed_agents_panel_row_keeps_the_switch_the_facts_and_the_chip() {
    let (mut app, wid, session, _reader, _host) = claude_pane(120);
    let rule = "\u{2500}".repeat(120);
    // The pane's row 0 in the frame (`footer_row` is `draw_claude`'s row 22).
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

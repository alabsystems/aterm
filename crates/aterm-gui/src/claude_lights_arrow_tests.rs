// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the composer top rule's ⬆️ of a newer CLI build: what it shows, what a press asks,
//! and what a refusal says.

use std::os::unix::net::UnixStream;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use aterm_agent::harness::upgrade::{Phase, Request};
use aterm_agent::harness::upgrade_drive::Row;
use aterm_messages::UpgradeWord;
use aterm_window::event::{ElementState, MouseButton};

use super::gesture_tests::{
    BYPASS, Hosted, PGID, app, draw_claude, feed, frame, host, pty, publish_facts, px_of,
};
use super::{BorderSwitch, REFUSAL_SHOWN};
use crate::upgrade_host::{PressAsk, WordAsk, WordWriter};
use crate::{App, WindowId};

fn claude_pane(cols: u16) -> (App, WindowId, u64, UnixStream, Hosted) {
    let (mut app, wid, session, reader) = app();
    let _ = app.apply_term_resize(wid, 24, cols);
    assert_eq!(app.windows[&wid].cols, cols, "the pane {cols} columns wide");
    let hosted = host(&mut app);
    publish_facts(&app, session);
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
        timeline.set_program(PGID, Some("claude".into()));
    }
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    (app, wid, session, reader, hosted)
}

/// `session`'s upgrade row: Claude Code 2.1.305 behind 2.1.310, waiting on its background work.
fn behind(app: &App, session: u64, request: Request) -> Row {
    Row {
        session: "conv-up".to_string(),
        tab: app.session_sid(session).expect("a registered tab"),
        from: "2.1.305".to_string(),
        to: "2.1.310".to_string(),
        source: "managed".to_string(),
        phase: Phase::Pending,
        wait: "background".to_string(),
        request,
        ..Row::default()
    }
}

fn switch(app: &App, wid: WindowId) -> BorderSwitch {
    let toggles = &app.windows[&wid].claude_lights.border_toggles;
    assert_eq!(toggles.len(), 1, "one pane, one switch: {toggles:?}");
    toggles[0].clone()
}

fn text(app: &App, wid: WindowId, row: usize, cols: std::ops::Range<usize>) -> String {
    app.windows[&wid].input_scratch.cells[row][cols]
        .iter()
        .map(|c| c.ch)
        .collect()
}

/// The words in the label's cells.
fn slot(app: &App, wid: WindowId) -> String {
    let b = switch(app, wid);
    let cols = b.label.clone().expect("a label");
    text(app, wid, b.frame_row, cols).trim_start().to_string()
}

fn recording(
    answer: impl Fn(&WordAsk) -> Result<Row, String> + Send + Sync + 'static,
) -> (WordWriter, Receiver<WordAsk>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let writer = WordWriter::new(move |ask| {
        let _ = tx.send(ask.clone());
        answer(ask)
    });
    (writer, rx)
}

fn click_at(app: &mut App, wid: WindowId, cell: (usize, usize)) {
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    let at = Instant::now();
    app.on_mouse_input_at(wid, ElementState::Pressed, MouseButton::Left, at);
    app.on_mouse_input_at(wid, ElementState::Released, MouseButton::Left, at);
}

/// THE ARROW ASKS FOR THE MOVE AND SAYS WHY IT WAS REFUSED: a tab whose upgrade row has a later
/// build waiting shows `⬆️` before the close; the pointer on it puts when the press's restart
/// would land in the label's slot, read off the live screen (at once: the background work the row
/// waits on is no wait of a press's); a press writes the one-shot press of that build; a refused
/// word says why in the slot until the refusal's few seconds pass; an accepted one turns the
/// arrow to `◐`, the label as it was, and a second press asks nothing. CONTROL: a skipped build
/// shows no arrow.
#[test]
fn the_arrow_asks_for_the_move_and_says_why_it_was_refused() {
    let (mut app, wid, session, _reader, _host) = claude_pane(158);
    let tab = app.session_sid(session).expect("a tab");
    let row = behind(&app, session, Request::None);
    app.apply_agent_upgrades(vec![row.clone()]);
    frame(&mut app, wid);
    let b = switch(&app, wid);
    let arrow = b.arrow.clone().expect("the arrow shows");
    let lead = app.windows[&wid].input_scratch.cells[b.frame_row][arrow.start + 1];
    assert_eq!(
        (lead.ch, lead.emoji_presentation),
        (crate::claude_footer::UP_ARROW, true),
        "an emoji-presented ⬆️"
    );
    let label = slot(&app, wid);
    assert!(
        label.starts_with("alab claude harness"),
        "at rest, the label: {label:?}"
    );
    let words = "update to v2.1.310 \u{b7} now";
    let on = (b.frame_row, arrow.start + 1);
    let (x, y) = px_of(&app, wid, on);
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    assert_eq!(
        slot(&app, wid),
        words,
        "the pointer on it: the build and when the press's restart lands, in the label's own \
         cells"
    );
    let (writer, asked) = recording(|_| Err("busy:another-sweep".to_string()));
    app.upgrade_view.set_writer(writer);
    click_at(&mut app, wid, on);
    let ask = asked.try_recv().expect("the press wrote a word");
    assert_eq!(
        (ask.tab.as_str(), ask.to.as_str(), ask.word, ask.press),
        (
            tab.as_str(),
            "2.1.310",
            UpgradeWord::Now,
            Some(PressAsk { off: false })
        ),
        "the one-shot press of the build it offers, the harness on"
    );
    frame(&mut app, wid);
    assert_eq!(
        slot(&app, wid),
        "couldn't update \u{b7} press it again",
        "the refusal's words"
    );
    let later = Instant::now() + REFUSAL_SHOWN + Duration::from_secs(1);
    let _ = app
        .windows
        .get_mut(&wid)
        .expect("window")
        .claude_lights
        .expire(later);
    frame(&mut app, wid);
    // The refusal is said on the messages row too, which may move the rule under a still pointer:
    // the pointer finds the arrow again where it now is.
    let b = switch(&app, wid);
    let still = (
        b.frame_row,
        b.arrow.clone().expect("the arrow stays").start + 1,
    );
    if still == on {
        assert_eq!(
            slot(&app, wid),
            words,
            "a still arrow under a still pointer"
        );
    }
    let on = still;
    let (x, y) = px_of(&app, wid, on);
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    assert_eq!(slot(&app, wid), words, "the refusal let go");
    let asked_row = Row {
        request: Request::Now,
        request_at: crate::upgrade_host::now_s(),
        ..row.clone()
    };
    let (writer, asked) = recording(move |_| Ok(asked_row.clone()));
    app.upgrade_view.set_writer(writer);
    click_at(&mut app, wid, on);
    assert!(asked.try_recv().is_ok(), "the press wrote a word");
    let away = (b.frame_row - 3, 0);
    let (x, y) = px_of(&app, wid, away);
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let b = switch(&app, wid);
    let arrow = b.arrow.clone().expect("the asked arrow");
    assert_eq!(
        app.windows[&wid].input_scratch.cells[b.frame_row][arrow.start + 1].ch,
        '\u{25d0}',
        "asked: ◐"
    );
    assert_eq!(
        slot(&app, wid),
        label,
        "asked: the label as it was, nothing moved"
    );
    click_at(&mut app, wid, (b.frame_row, arrow.start + 1));
    assert!(
        asked.try_recv().is_err(),
        "an asked move takes no second press"
    );
    app.apply_agent_upgrades(vec![Row {
        request: Request::Skip("2.1.310".to_string()),
        ..row
    }]);
    frame(&mut app, wid);
    assert_eq!(
        switch(&app, wid).arrow,
        None,
        "CONTROL: a skipped build, no arrow"
    );
}

/// THE ARROW IS A BUTTON A SCREEN READER CAN PRESS: it is named for the build it moves to and
/// described by when the press's restart would land, and pressing it asks as a click does. CONTROL: with
/// no later build there is no such button.
#[cfg(a11y_tree)]
#[test]
fn the_arrow_is_a_button_a_screen_reader_can_press() {
    use crate::a11y_tree::HarnessChip;
    let (mut app, wid, session, _reader, _host) = claude_pane(158);
    let upgrade = |app: &App| {
        app.claude_chips_a11y(wid)
            .into_iter()
            .find(|c| c.chip == HarnessChip::Upgrade)
    };
    assert!(
        upgrade(&app).is_none(),
        "CONTROL: no later build, no button"
    );
    let row = behind(&app, session, Request::None);
    app.apply_agent_upgrades(vec![row]);
    frame(&mut app, wid);
    let node = upgrade(&app).expect("the arrow's button");
    assert_eq!(node.label, "Update Claude Code to 2.1.310");
    assert_eq!(
        node.description.as_deref(),
        Some("update to v2.1.310 \u{b7} now"),
        "a press passes the background work the row waits on"
    );
    let (writer, asked) = recording(|_| Err("busy:another-sweep".to_string()));
    app.upgrade_view.set_writer(writer);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Upgrade);
    assert_eq!(
        asked.try_recv().map(|ask| (ask.to, ask.word)),
        Ok(("2.1.310".to_string(), UpgradeWord::Now))
    );
}

/// The press of the arrow at `on`, recorded: what it asked, if anything.
fn press_recorded(app: &mut App, wid: WindowId, on: (usize, usize)) -> Option<WordAsk> {
    let (writer, asked) = recording(|_| Err("busy:another-sweep".to_string()));
    app.upgrade_view.set_writer(writer);
    click_at(app, wid, on);
    asked.try_recv().ok()
}

/// AN OFF TAB'S ARROW OFFERS A ONE-SHOT PRESS: a Claude Code tab whose Autopilot is off is set
/// aside, so no worker steps it; but a press of its ⬆️ moves it once by its own lane, so the
/// arrow is the colour ⬆️, the pointer on it says when the press's restart lands, and the press
/// writes a press whose restart types only its relaunch line. CONTROL: a tab another supervisor
/// holds keeps the quiet `↑` (never the colour ⬆️ a press acts on), and its press writes nothing
/// and says why in the slot.
#[test]
fn an_off_tabs_arrow_offers_a_one_shot_press() {
    use aterm_agent::harness::upgrade_drive::{AUTOPILOT_OFF, CLAIMED};
    let (mut app, wid, session, _reader, _host) = claude_pane(158);
    let row = behind(&app, session, Request::None);
    app.apply_agent_upgrades(vec![Row {
        aside: AUTOPILOT_OFF.to_string(),
        wait: AUTOPILOT_OFF.to_string(),
        ..row.clone()
    }]);
    frame(&mut app, wid);
    let b = switch(&app, wid);
    let on = (
        b.frame_row,
        b.arrow.clone().expect("the arrow shows").start + 1,
    );
    let lead = app.windows[&wid].input_scratch.cells[on.0][on.1];
    assert_eq!(
        (lead.ch, lead.emoji_presentation),
        (crate::claude_footer::UP_ARROW, true),
        "Autopilot off: the colour ⬆️ a press acts on"
    );
    let (x, y) = px_of(&app, wid, on);
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    assert_eq!(slot(&app, wid), "update to v2.1.310 \u{b7} now");
    let ask = press_recorded(&mut app, wid, on).expect("the press wrote a word");
    assert_eq!(
        (ask.word, ask.press),
        (UpgradeWord::Now, Some(PressAsk { off: true })),
        "a press whose restart types only its relaunch line"
    );
    let later = Instant::now() + REFUSAL_SHOWN + Duration::from_secs(1);
    let _ = app
        .windows
        .get_mut(&wid)
        .expect("window")
        .claude_lights
        .expire(later);
    app.apply_agent_upgrades(vec![Row {
        aside: CLAIMED.to_string(),
        wait: CLAIMED.to_string(),
        ..row
    }]);
    frame(&mut app, wid);
    let b = switch(&app, wid);
    let on = (
        b.frame_row,
        b.arrow.clone().expect("the held arrow").start + 1,
    );
    let lead = app.windows[&wid].input_scratch.cells[on.0][on.1];
    assert_eq!(
        (lead.ch, lead.emoji_presentation),
        (crate::claude_footer::HELD_ARROW, false),
        "CONTROL: claimed, a quiet \u{2191}, no colour emoji"
    );
    let (x, y) = px_of(&app, wid, on);
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    assert_eq!(
        press_recorded(&mut app, wid, on),
        None,
        "CONTROL: claimed, nothing written"
    );
    frame(&mut app, wid);
    let why = held_why(CLAIMED);
    let said = slot(&app, wid);
    assert!(said.ends_with(why), "CONTROL: the press says why: {said:?}");
    let said = held_said(&app, why).expect("the press says why on the messages row too");
    assert!(said.contains(why), "{said}");
}

/// THE ARROW PRESSES ONCE WITH THE HARNESS OFF: the master switch off stands the host down and its
/// rows go, but the row it last sent still names a later build than the one running, so the ⬆️
/// stays; a Claude Code tab's press moves it once by its own lane, so the press writes a press
/// whose restart types only its relaunch line, and the harness stays off. CONTROL: with the
/// harness on, a tab the host sends no row for shows no arrow.
#[test]
fn the_arrow_presses_once_with_the_harness_off() {
    let (mut app, wid, session, _reader, _host) = claude_pane(158);
    app.apply_agent_upgrades(vec![behind(&app, session, Request::None)]);
    frame(&mut app, wid);
    assert!(
        switch(&app, wid).arrow.is_some(),
        "the arrow, the harness on"
    );
    app.apply_agent_upgrades(Vec::new());
    frame(&mut app, wid);
    assert_eq!(
        switch(&app, wid).arrow,
        None,
        "CONTROL: the harness on and no row, no arrow"
    );
    app.apply_agent_upgrades(vec![behind(&app, session, Request::None)]);
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    app.apply_agent_upgrades(Vec::new());
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let b = switch(&app, wid);
    let on = (
        b.frame_row,
        b.arrow.clone().expect("the arrow, the harness off").start + 1,
    );
    let lead = app.windows[&wid].input_scratch.cells[on.0][on.1];
    assert_eq!(
        (lead.ch, lead.emoji_presentation),
        (crate::claude_footer::UP_ARROW, true),
        "the harness off: the colour ⬆️ a press acts on"
    );
    let (x, y) = px_of(&app, wid, on);
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let said = slot(&app, wid);
    assert!(
        said.ends_with("v2.1.310 \u{b7} now"),
        "when the press's restart lands: {said:?}"
    );
    let ask = press_recorded(&mut app, wid, on).expect("the press wrote a word");
    assert_eq!(
        (ask.word, ask.press),
        (UpgradeWord::Now, Some(PressAsk { off: true })),
        "a press whose restart types only its relaunch line"
    );
    assert!(
        !app.harness_policy.get().policy.enabled,
        "the press leaves the harness off"
    );
}

/// A CONTROL-CLICK ON THE ARROW ASKS NOTHING: on macOS a control-click is the secondary click, a
/// right-click, which opens no move; so a control-click on the ⬆️ writes no Upgrade Now. CONTROL:
/// the same press with no modifier asks for the move.
#[cfg(target_os = "macos")]
#[test]
fn a_control_click_on_the_arrow_asks_nothing() {
    use aterm_window::keyboard::ModifiersState;
    let (mut app, wid, session, _reader, _host) = claude_pane(158);
    app.apply_agent_upgrades(vec![behind(&app, session, Request::None)]);
    frame(&mut app, wid);
    let b = switch(&app, wid);
    let on = (b.frame_row, b.arrow.clone().expect("the arrow").start + 1);
    app.windows.get_mut(&wid).expect("window").mods = ModifiersState::CONTROL;
    assert_eq!(
        press_recorded(&mut app, wid, on),
        None,
        "a control-click writes no Upgrade Now"
    );
    app.windows.get_mut(&wid).expect("window").mods = ModifiersState::empty();
    frame(&mut app, wid);
    assert_eq!(
        press_recorded(&mut app, wid, on).map(|ask| ask.word),
        Some(UpgradeWord::Now),
        "CONTROL: a plain press asks"
    );
}

/// Why a press of a ⬆️ its tab's `aside` holds cannot ask for the move, as the rule says it.
fn held_why(aside: &str) -> &'static str {
    crate::claude_footer::held_words(aside)
        .expect("a held aside")
        .1
}

/// The detail of the messages row a press of a ⬆️ held for `why` posted: the row whose step is
/// what lets that move be asked for ([`crate::claude_footer::held_then`]).
fn held_said(app: &App, why: &str) -> Option<String> {
    let then = crate::claude_footer::held_then(why);
    app.messages
        .live_rows()
        .find(|l| {
            matches!(l.msg.need(), aterm_messages::need::Need::Now(ask)
                if ask.then().words() == then)
        })
        .map(|l| l.msg.detail.join(" \u{b7} "))
}

/// A HELD PRESS SAYS WHY AT EVERY WIDTH: a press of a held ⬆️ (another supervisor holds its tab)
/// writes no Upgrade Now and posts on the messages row why the move cannot be asked for and what
/// lets it be, whether or not the rule has a label to say it in. CONTROL: before the press the
/// row is not there.
#[test]
fn a_held_press_says_why_at_every_width() {
    use aterm_agent::harness::upgrade_drive::CLAIMED;
    for cols in [158, 80, 30] {
        let (mut app, wid, session, _reader, _host) = claude_pane(cols);
        let row = behind(&app, session, Request::None);
        app.apply_agent_upgrades(vec![Row {
            aside: CLAIMED.to_string(),
            wait: CLAIMED.to_string(),
            ..row
        }]);
        frame(&mut app, wid);
        let b = switch(&app, wid);
        let on = (
            b.frame_row,
            b.arrow.clone().expect("the held arrow shows").start + 1,
        );
        let why = held_why(CLAIMED);
        assert_eq!(held_said(&app, why), None, "{cols}: CONTROL: no row before");
        assert_eq!(
            press_recorded(&mut app, wid, on),
            None,
            "{cols}: nothing asked"
        );
        let said = held_said(&app, why).unwrap_or_else(|| panic!("{cols}: the row"));
        assert!(
            said.contains(why) && said.contains("2.1.310"),
            "{cols}: why and the target build: {said}"
        );
        if cols == 30 {
            assert!(b.label.is_none(), "30: no label to say it in");
        }
    }
}

/// EVERY BUTTON ON THE LABEL GOES NOWHERE: a right, middle or left press on the harness label
/// opens nothing and reaches no program (no mouse report, no paste), only the glass. CONTROL:
/// the same right press on Claude's own rows reports to the program that asked for the mouse.
#[test]
fn every_button_on_the_label_goes_nowhere() {
    let (mut app, wid, session, mut reader, _host) = claude_pane(158);
    feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
    frame(&mut app, wid);
    let b = switch(&app, wid);
    let label = b.label.clone().expect("a label");
    let press = |app: &mut App, cell: (usize, usize), button: MouseButton| {
        let (x, y) = px_of(app, wid, cell);
        app.on_cursor_moved(wid, x, y);
        let at = Instant::now();
        app.on_mouse_input_at(wid, ElementState::Pressed, button, at);
        app.on_mouse_input_at(wid, ElementState::Released, button, at);
    };
    for button in [MouseButton::Right, MouseButton::Middle, MouseButton::Left] {
        press(&mut app, (b.frame_row, label.end - 2), button);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "{button:?}: nothing reaches the program"
        );
    }
    press(&mut app, (b.frame_row - 3, 4), MouseButton::Right);
    assert!(
        !pty(&mut reader, 1).is_empty(),
        "CONTROL: Claude's own rows report the press"
    );
}

/// THE LABEL IS GLASS TO THE POINTER TOO: with the program tracking every motion (1003), the
/// pointer moving over the harness label reports nothing to it and shows the arrow cursor, not
/// the pane's I-beam, while the label's hover is on, as its presses go nowhere
/// ([`every_button_on_the_label_goes_nowhere`]). CONTROL: motion over Claude's own rows reports.
#[test]
fn the_label_is_glass_to_the_pointer_too() {
    let (mut app, wid, session, mut reader, _host) = claude_pane(158);
    feed(&app, session, b"\x1b[?1003h\x1b[?1006h");
    frame(&mut app, wid);
    let b = switch(&app, wid);
    let label = b.label.clone().expect("a label");
    for col in [label.end - 2, label.end - 4] {
        let (x, y) = px_of(&app, wid, (b.frame_row, col));
        app.on_cursor_moved(wid, x, y);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "{col}: no motion reaches the program"
        );
        let ws = &app.windows[&wid];
        assert!(
            !ws.native_text_cursor && !ws.hover_pointer,
            "{col}: the arrow cursor"
        );
        assert_eq!(
            ws.claude_lights.part_hover,
            Some((session, crate::claude_footer::HoverAt::Label)),
            "{col}: the label's hover"
        );
    }
    let (x, y) = px_of(&app, wid, (b.frame_row - 3, 4));
    app.on_cursor_moved(wid, x, y);
    assert!(
        !pty(&mut reader, 1).is_empty(),
        "CONTROL: Claude's own rows report the motion"
    );
}

/// Where a person points at the ⬆️ of `wid`'s one top rule, as `pointer move` names a cell: its
/// grid row and the arrow's middle column.
fn arrow_cell(app: &App, wid: WindowId) -> (u16, u16) {
    let b = switch(app, wid);
    let arrow = b.arrow.clone().expect("the arrow shows");
    let row = u16::try_from(b.term_row).expect("a grid row");
    let col = u16::try_from(arrow.start + 1).expect("a grid column");
    (row, col)
}

fn point(app: &mut App, (row, col): (u16, u16)) {
    app.pointer_cmd(crate::app_mouse::PointerAction::Move { row, col })
        .expect("the pointer moves");
}

fn top_hover(app: &App, wid: WindowId, session: u64) -> crate::claude_footer::HoverAt {
    app.windows[&wid].claude_lights.top_hover(session)
}

/// A `pointer move` HIT-TESTS WHAT THE NEXT CAPTURE SHOWS: in a windowless window, a move made
/// after the pane was re-laid (here a resize, the composer redrawn at the new width, nothing
/// composed since) onto where the ⬆️ now is hovers it, and the next capture draws the arrow's
/// words in the label's slot. CONTROLS: a move to a rule cell off the arrow hovers nothing; in a
/// pane laid as it was last composed, the first move already hovers it.
#[test]
fn a_pointer_move_after_the_rule_moved_hovers_the_arrow() {
    let words = "update to v2.1.310 \u{b7} now";
    let (mut laid, laid_wid, laid_session, _laid_reader, _laid_host) = claude_pane(140);
    laid.apply_agent_upgrades(vec![behind(&laid, laid_session, Request::None)]);
    frame(&mut laid, laid_wid);
    let there = arrow_cell(&laid, laid_wid);
    point(&mut laid, there);
    assert_eq!(
        top_hover(&laid, laid_wid, laid_session),
        crate::claude_footer::HoverAt::Arrow,
        "CONTROL: nothing changed since the last composition"
    );

    let (mut app, wid, session, _reader, _host) = claude_pane(158);
    app.apply_agent_upgrades(vec![behind(&app, session, Request::None)]);
    frame(&mut app, wid);
    let before = arrow_cell(&app, wid);
    assert_ne!(before, there, "PRECONDITION: the resize moves the arrow");
    assert!(app.apply_term_resize(wid, 24, 140));
    draw_claude(&app, session, BYPASS);
    point(&mut app, there);
    assert_eq!(
        top_hover(&app, wid, session),
        crate::claude_footer::HoverAt::Arrow,
        "the arrow where the re-laid rule put it"
    );
    frame(&mut app, wid);
    assert_eq!(slot(&app, wid), words, "the next capture draws its words");

    let b = switch(&app, wid);
    let marks: Vec<std::ops::Range<usize>> = app.windows[&wid]
        .claude_lights
        .mark_hits
        .iter()
        .filter(|m| m.frame_row == b.frame_row)
        .map(|m| m.cols.clone())
        .collect();
    let off = (0..140)
        .find(|c| {
            ![Some(b.cols.clone()), b.label.clone(), b.arrow.clone()]
                .iter()
                .flatten()
                .chain(&marks)
                .any(|r| r.contains(c))
        })
        .expect("a rule cell off the glass");
    point(&mut app, (there.0, u16::try_from(off).expect("a column")));
    assert_eq!(
        top_hover(&app, wid, session),
        crate::claude_footer::HoverAt::None,
        "CONTROL: off the arrow, no hover"
    );
}

/// Draw Claude Code's bottom block `cols` wide and six rows higher than [`draw_claude`] does, as a fresh Claude
/// draws it below its banner: the rows from 19 down are blank.
fn draw_claude_higher(app: &App, session: u64, cols: usize) {
    let rule = "\u{2500}".repeat(cols);
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[13;1H\u{25CF} done\x1b[15;1H{rule}\x1b[16;1H\u{276F} \
         \x1b[17;1H{rule}\x1b[18;1H{BYPASS}\x1b[16;3H"
    );
    feed(app, session, screen.as_bytes());
}

/// A STILL POINTER HOVERS THE ⬆️ LAID UNDER IT: the pointer rests on a blank row of Claude's
/// screen, and the next composition lays the rule's ⬆️ there (Claude redrew its block lower); a
/// second move to the same cell, which hit-tests nothing anew, finds the arrow hovered.
/// CONTROL: with nothing laid between the two moves, the pointer still hovers nothing.
#[test]
fn a_still_pointer_hovers_an_arrow_laid_under_it() {
    let (mut probe, probe_wid, probe_session, _probe_reader, _probe_host) = claude_pane(158);
    probe.apply_agent_upgrades(vec![behind(&probe, probe_session, Request::None)]);
    frame(&mut probe, probe_wid);
    let there = arrow_cell(&probe, probe_wid);
    for laid in [true, false] {
        let (mut app, wid, session, _reader, _host) = claude_pane(158);
        app.apply_agent_upgrades(vec![behind(&app, session, Request::None)]);
        draw_claude_higher(&app, session, 158);
        frame(&mut app, wid);
        point(&mut app, there);
        assert_eq!(
            top_hover(&app, wid, session),
            crate::claude_footer::HoverAt::None,
            "PRECONDITION: a blank row under the pointer (laid={laid})"
        );
        if laid {
            draw_claude(&app, session, BYPASS);
        }
        frame(&mut app, wid);
        point(&mut app, there);
        let want = if laid {
            crate::claude_footer::HoverAt::Arrow
        } else {
            crate::claude_footer::HoverAt::None
        };
        assert_eq!(top_hover(&app, wid, session), want, "laid={laid}");
    }
}

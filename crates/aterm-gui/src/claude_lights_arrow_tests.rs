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
    BYPASS, Hosted, PGID, app, draw_claude, frame, host, publish_facts, px_of,
};
use super::{BorderSwitch, REFUSAL_SHOWN};
use crate::upgrade_host::{WordAsk, WordWriter};
use crate::{App, WindowId};

fn claude_pane(cols: u16) -> (App, WindowId, u64, UnixStream, Hosted) {
    let (mut app, wid, session, reader) = app();
    assert!(app.apply_term_resize(wid, 24, cols));
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

/// `words` in a slot of `width` cells: whole, or cut with `…` at their left.
fn fitted(words: &str, width: usize) -> String {
    let n = words.chars().count();
    if n <= width {
        return words.to_string();
    }
    std::iter::once('\u{2026}')
        .chain(words.chars().skip(n - (width - 1)))
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
/// build waiting shows `⬆️` before the close; the pointer on it puts when the move would land in
/// the label's slot; a press writes the tab menu's Upgrade Now for that build; a refused word
/// says why in the slot until the refusal's few seconds pass; an accepted one turns the arrow to
/// `◐`, the label naming both builds, and a second press asks nothing. CONTROL: a skipped build
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
    let width = label.chars().count();
    let words = fitted("update to v2.1.310 \u{b7} after background jobs", width);
    let on = (b.frame_row, arrow.start + 1);
    let (x, y) = px_of(&app, wid, on);
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    assert_eq!(
        slot(&app, wid),
        words,
        "the pointer on it: when the move lands, in the label's cells"
    );
    let (writer, asked) = recording(|_| Err("busy:another-sweep".to_string()));
    app.upgrade_view.set_writer(writer);
    click_at(&mut app, wid, on);
    let ask = asked.try_recv().expect("the press wrote a word");
    assert_eq!(
        (ask.tab.as_str(), ask.to.as_str(), ask.word),
        (tab.as_str(), "2.1.310", UpgradeWord::Now),
        "the tab menu's Upgrade Now for the build it offers"
    );
    frame(&mut app, wid);
    assert_eq!(
        slot(&app, wid),
        fitted("couldn't update \u{b7} press it again", width),
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
    assert!(
        slot(&app, wid).contains("\u{2192} v2.1.310)"),
        "the label names both builds: {:?}",
        slot(&app, wid)
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
/// described by when the move would land, and pressing it asks as a click does. CONTROL: with
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
        Some("update to v2.1.310 \u{b7} after background jobs")
    );
    let (writer, asked) = recording(|_| Err("busy:another-sweep".to_string()));
    app.upgrade_view.set_writer(writer);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Upgrade);
    assert_eq!(
        asked.try_recv().map(|ask| (ask.to, ask.word)),
        Ok(("2.1.310".to_string(), UpgradeWord::Now))
    );
}

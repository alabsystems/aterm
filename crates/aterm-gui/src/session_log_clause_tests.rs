// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the session log's clause in a tab's tooltip and context menu (`[session_log] clause`).

use std::sync::{Arc, Mutex};

use aterm_slog::{Actor, Conf, Fields, Key, Kind, Node, Open, Src};

use crate::session_timeline::SessionTimeline;
use crate::{App, WindowId};

/// Turn `[session_log] clause` on or off for `app`.
pub(crate) fn set_clause(app: &mut App, on: bool) {
    app.config.session_log = Some(crate::app_config::SessionLogConfig {
        journal: None,
        clause: Some(on),
    });
}

/// An app with the switch `on` or off, and its session 0's timeline.
pub(crate) fn clause_app(on: bool) -> (App, Arc<Mutex<SessionTimeline>>) {
    let mut app = App::headless_for_test();
    set_clause(&mut app, on);
    let timeline = Arc::clone(&app.pool.get(0).expect("session 0").ctx.timeline);
    (app, timeline)
}

/// [`clause_app`] with session 0's log on a hand clock that starts at the
/// process clock's now.
fn hand_app(on: bool) -> (App, Arc<Mutex<SessionTimeline>>, super::tests::Hand) {
    let (app, timeline) = clause_app(on);
    let hand = super::tests::hand_clock_from(&timeline, crate::turn_ledger::now_ms());
    (app, timeline, hand)
}

/// Open an agent run and a turn under it in `timeline`'s log; the turn.
pub(crate) fn open_turn(timeline: &Mutex<SessionTimeline>) -> Node {
    let mut g = timeline.lock().unwrap();
    let now = g.slog().now();
    let log = g.slog_mut().log_mut_for_journal();
    let run = log
        .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), now)
        .expect("a run opens");
    log.open(
        Open::new(Kind::Turn, Src::Screen, Conf::Strong)
            .under(run)
            .by(Actor::Vendor),
        now,
    )
    .expect("a turn opens")
}

/// Open a tool step of class `cls` under `turn` (`running shell` for
/// `shell`, `editing` for `edit`); the step.
pub(crate) fn open_step(timeline: &Mutex<SessionTimeline>, turn: Node, cls: &str) -> Node {
    let mut g = timeline.lock().unwrap();
    let now = g.slog().now();
    g.slog_mut()
        .log_mut_for_journal()
        .open(
            Open::new(Kind::Tool, Src::Transcript, Conf::Exact)
                .under(turn)
                .by(Actor::Vendor)
                .fields(Fields::new().word(Key::Cls, cls).expect("a class")),
            now,
        )
        .expect("a step opens")
}

/// Close `step` `ok`.
fn close_step(timeline: &Mutex<SessionTimeline>, step: Node) {
    let mut g = timeline.lock().unwrap();
    let now = g.slog().now();
    g.slog_mut()
        .log_mut_for_journal()
        .close(
            step,
            aterm_slog::Close::new(
                aterm_slog::Out::Step(aterm_slog::StepOut::Ok),
                Src::Transcript,
                Conf::Exact,
            ),
            now,
        )
        .expect("the step closes");
}

/// The `mark` verb's label, written as `by` (`ttl` seconds).
fn label(app: &App, by: Actor, text: &str, ttl: u32) {
    let ctx = &app.pool.get(0).expect("session 0").ctx;
    let (reply, _) = super::cmd_mark(ctx, by, true, &format!("- label text={text} ttl={ttl}"));
    assert!(reply.starts_with("OK "), "{reply}");
}

/// Session 0's composed chrome, through the strip's own pass.
fn chrome(app: &mut App) -> crate::session_chrome::TabChromeExt {
    let wid = WindowId(0);
    let titles = app.tab_titles(wid);
    app.tab_chrome_ext(wid, &titles).swap_remove(0)
}

/// The menu as it pops, mirrored (`chrome` verb words).
fn menu_words(ext: &crate::session_chrome::TabChromeExt) -> Vec<String> {
    crate::session_chrome::menu_shown_at(&ext.menu, crate::turn_ledger::now_ms())
        .iter()
        .map(crate::session_chrome::menu_entry_mirror)
        .collect()
}

fn tooltip(ext: &crate::session_chrome::TabChromeExt) -> &str {
    ext.tooltip.as_deref().unwrap_or_default()
}

/// With `[session_log] clause = false`, log activity leaves tooltip, menu, and expiry unchanged without locking the timeline.
#[test]
fn the_clause_off_leaves_the_tooltip_and_menu_byte_identical() {
    let (mut app, timeline, _hand) = hand_app(false);
    let before = chrome(&mut app);
    let expiry = app.session_chrome[&0].expires_ms;
    let turn = open_turn(&timeline);
    open_step(&timeline, turn, "shell");
    label(&app, Actor::Human, "compiling", 60);
    assert_eq!(chrome(&mut app), before, "a cached compose");
    app.session_chrome.clear();
    assert_eq!(chrome(&mut app), before, "a fresh compose");
    assert_eq!(app.session_chrome[&0].expires_ms, expiry);
    assert_eq!(app.session_chrome[&0].clause, None);
    assert!(!app.clause_moved(0), "off reads nothing");
    {
        let _held = timeline.lock().unwrap();
        assert_eq!(
            app.chrome_clause_read(WindowId(0), 0, &timeline, crate::turn_ledger::now_ms()),
            None
        );
        assert!(
            !app.session_chrome_retry.contains(&0),
            "off: no acquisition, so no contention"
        );
    }
    set_clause(&mut app, true);
    let on = chrome(&mut app);
    assert!(tooltip(&on).contains("\nnow: compiling"), "{on:?}");
    assert_ne!(on, before);
}

/// A log move recomposes the tab's clause once when `[session_log] clause` is enabled.
#[test]
fn a_log_move_recomposes_the_tabs_clause_with_the_switch_on() {
    let (mut app, timeline, hand) = hand_app(true);
    let quiet = chrome(&mut app);
    assert!(!tooltip(&quiet).contains("now:"), "{quiet:?}");
    let turn = open_turn(&timeline);
    let step = open_step(&timeline, turn, "shell");
    assert!(app.clause_moved(0), "the step moved the clause");
    app.follow_clause(0);
    assert!(!app.clause_moved(0), "one refresh took the move");
    let ext = app.session_chrome[&0].ext.clone();
    assert!(tooltip(&ext).contains("\nnow: running shell"), "{ext:?}");
    assert!(
        menu_words(&ext)
            .iter()
            .any(|w| w.starts_with("now: running shell \u{b7} ")),
        "{:?}",
        menu_words(&ext)
    );
    hand.advance(120_000);
    assert!(!app.clause_moved(0), "an older step is the same clause");
    assert_eq!(chrome(&mut app), ext);
    close_step(&timeline, step);
    open_step(&timeline, turn, "edit");
    let edit = chrome(&mut app);
    assert!(tooltip(&edit).contains("\nnow: editing"), "{edit:?}");
}

/// The default config shows the clause in the tooltip and menu while keeping the tab title unchanged.
#[test]
fn the_default_config_shows_the_clause() {
    let mut app = App::headless_for_test();
    assert_eq!(app.config.session_log, None, "the default config");
    let timeline = Arc::clone(&app.pool.get(0).expect("session 0").ctx.timeline);
    let titles = app.tab_titles(WindowId(0));
    let turn = open_turn(&timeline);
    open_step(&timeline, turn, "shell");
    let ext = chrome(&mut app);
    assert!(tooltip(&ext).contains("\nnow: running shell"), "{ext:?}");
    assert!(
        menu_words(&ext)
            .iter()
            .any(|w| w.starts_with("now: running shell \u{b7} ")),
        "{:?}",
        menu_words(&ext)
    );
    assert_eq!(
        app.tab_titles(WindowId(0)),
        titles,
        "the label is the title"
    );
}

/// A shown label expires at its `until` timestamp and names its author while visible.
#[test]
fn a_shown_label_leaves_the_chrome_at_its_until() {
    let (mut app, timeline, hand) = hand_app(true);
    let turn = open_turn(&timeline);
    open_step(&timeline, turn, "shell");
    label(&app, Actor::Harness, "compiling", 10);
    let now = crate::turn_ledger::now_ms();
    let shown = chrome(&mut app);
    assert!(
        tooltip(&shown).contains("\nnow: compiling \u{b7} by the harness"),
        "{shown:?}"
    );
    assert!(
        menu_words(&shown)
            .iter()
            .any(|w| w.starts_with("now: compiling \u{b7} by the harness \u{b7} ")),
        "{:?}",
        menu_words(&shown)
    );
    let until = app.session_chrome[&0]
        .expires_ms
        .expect("a shown label arms its until");
    assert!(
        until > now && until <= crate::turn_ledger::now_ms() + 10_000,
        "the label's until on the process clock: {until} vs {now}"
    );
    hand.advance(10_001);
    let progress = app.advance_session_chrome_expiry(until);
    assert_eq!(progress.admitted_session, Some(0), "due at its until");
    let after = app.session_chrome[&0].ext.clone();
    assert!(
        tooltip(&after).contains("\nnow: running shell"),
        "{after:?}"
    );
    assert!(!tooltip(&after).contains("compiling"));
    assert_eq!(
        app.session_chrome[&0].expires_ms, None,
        "a step arms nothing"
    );
}

/// A `ctl` label is recorded in the log without appearing in the tooltip or menu.
#[test]
fn a_ctl_label_is_absent_from_the_chrome_and_present_in_the_log() {
    let (mut app, timeline, _hand) = hand_app(true);
    let turn = open_turn(&timeline);
    open_step(&timeline, turn, "shell");
    label(&app, Actor::Ctl, "ctlwords", 60);
    let ext = chrome(&mut app);
    assert!(tooltip(&ext).contains("\nnow: running shell"), "{ext:?}");
    assert!(!tooltip(&ext).contains("ctlwords"), "{ext:?}");
    assert!(
        !menu_words(&ext).iter().any(|w| w.contains("ctlwords")),
        "{:?}",
        menu_words(&ext)
    );
    assert!(
        super::tests::lines_of(&timeline)
            .iter()
            .any(|l| l.contains(" label ") && l.contains("ctlwords") && l.contains(" by=ctl ")),
        "the label is the log's: {:?}",
        super::tests::lines_of(&timeline)
    );
    label(&app, Actor::Human, "humanwords", 60);
    let ext = chrome(&mut app);
    assert!(
        tooltip(&ext).contains("\nnow: humanwords \u{b7} by you"),
        "{ext:?}"
    );
}

/// An open usage wall takes precedence over a harness label until the wall closes.
#[test]
fn a_harness_label_under_an_open_usage_wall_shows_the_wall() {
    let (mut app, timeline, _hand) = hand_app(true);
    let turn = open_turn(&timeline);
    let wall = {
        let mut g = timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut()
            .log_mut_for_journal()
            .open(
                Open::new(Kind::Wall, Src::Screen, Conf::Strong)
                    .under(turn)
                    .fields(
                        Fields::new()
                            .word(Key::Wall, "usage-session")
                            .expect("a wall")
                            .tok(Key::Detail, "resets 14:05")
                            .expect("its reset"),
                    ),
                now,
            )
            .expect("the wall opens")
    };
    label(&app, Actor::Harness, "resuming", 60);
    let ext = chrome(&mut app);
    assert!(
        tooltip(&ext).contains("\nnow: limit \u{b7} resets 14:05\n")
            || tooltip(&ext).ends_with("\nnow: limit \u{b7} resets 14:05"),
        "{ext:?}"
    );
    assert!(!tooltip(&ext).contains("resuming"), "{ext:?}");
    assert!(
        !tooltip(&ext).contains("\u{b7} by "),
        "a need names no author"
    );
    assert!(
        super::tests::lines_of(&timeline)
            .iter()
            .any(|l| l.contains(" label ") && l.contains("resuming") && l.contains(" by=harness ")),
        "the label is the log's: {:?}",
        super::tests::lines_of(&timeline)
    );
    close_step(&timeline, wall);
    let ext = chrome(&mut app);
    assert!(
        tooltip(&ext).contains("\nnow: resuming \u{b7} by the harness"),
        "{ext:?}"
    );
}

/// Need and work rows in the tooltip and menu omit the author suffix.
#[test]
fn a_need_or_work_row_names_no_author() {
    let (mut app, timeline, _hand) = hand_app(true);
    let open = |under: Node, kind: Kind, src: Src, by: Actor, fields: Fields| {
        let mut g = timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut()
            .log_mut_for_journal()
            .open(
                Open::new(kind, src, Conf::Strong)
                    .under(under)
                    .by(by)
                    .fields(fields),
                now,
            )
            .expect("opens")
    };
    let run = {
        let mut g = timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut()
            .log_mut_for_journal()
            .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), now)
            .expect("a run opens")
    };
    let no_author = |app: &mut App, row: &str| {
        let ext = chrome(app);
        assert!(
            tooltip(&ext).contains(&format!("\nnow: {row}")),
            "the {row} row shows: {ext:?}"
        );
        assert!(
            !tooltip(&ext).contains("\u{b7} by "),
            "the {row} row names no author: {ext:?}"
        );
        let menu = menu_words(&ext);
        assert!(
            menu.iter().any(|w| w.starts_with(&format!("now: {row}"))),
            "{menu:?}"
        );
        assert!(
            menu.iter().all(|w| !w.contains("\u{b7} by ")),
            "the menu's {row} row names no author: {menu:?}"
        );
    };
    let turn = open(run, Kind::Turn, Src::Screen, Actor::Human, Fields::new());
    no_author(&mut app, "working");
    open(
        turn,
        Kind::Tool,
        Src::Transcript,
        Actor::Human,
        Fields::new().word(Key::Cls, "shell").expect("a class"),
    );
    no_author(&mut app, "running shell");
    open(
        turn,
        Kind::Wall,
        Src::Screen,
        Actor::Harness,
        Fields::new()
            .word(Key::Wall, "usage-session")
            .expect("a wall")
            .tok(Key::Detail, "resets 14:05")
            .expect("its reset"),
    );
    no_author(&mut app, "limit \u{b7} resets 14:05");
}

/// Updating the clause adds no timeline row and leaves the tab label unchanged.
#[test]
fn the_clause_moves_no_row_and_no_label() {
    let (mut app, timeline, _hand) = hand_app(true);
    let wid = WindowId(0);
    let titles = app.tab_titles(wid);
    let high = timeline.lock().unwrap().high_id();
    let quiet = chrome(&mut app);
    let turn = open_turn(&timeline);
    open_step(&timeline, turn, "shell");
    label(&app, Actor::Human, "compiling", 60);
    app.follow_clause(0);
    assert_eq!(timeline.lock().unwrap().high_id(), high, "no timeline row");
    assert_eq!(app.tab_titles(wid), titles, "the label is the title");
    let moved = app.session_chrome[&0].ext.clone();
    let rows = |ext: &crate::session_chrome::TabChromeExt| -> Vec<String> {
        menu_words(ext)
            .into_iter()
            .filter(|w| !w.starts_with("now: "))
            .collect()
    };
    assert_eq!(rows(&moved), rows(&quiet), "every other row is today's");
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS' MENUS ON THE GLASS (`crate::fact_menu`): a real engine drawing
//! Claude Code's bottom block, one frame through the capture route's
//! splices and the menu's, a real right press on the cell of the fact the
//! painter RECORDED — the card hung from it, what each row does, and the
//! bytes the session's PTY sink received (none); and the usage's Traffic
//! card, opened and closed by a click on `Σ`. It drives the lights' gesture
//! rig (`claude_lights::gesture_tests`).

use std::sync::Arc;

use aterm_agent::harness::footer::{FactKind, FactMask, FooterFacts};
use aterm_agent::harness::session_usage::{ModelTokens, TrafficView, UsageFacts};
use aterm_window::event::{ElementState, MouseButton};

use super::Fact;
use super::gesture_tests::*;
use crate::autopilot_menu::{MenuOwner, MenuRow};
use crate::claude_footer::FactSide;
use crate::fact_menu::NEW_TABS_ASKED;
use crate::{App, WindowId};

/// One frame through the capture route's splices, the menu's last.
fn menu_frame(app: &mut App, wid: WindowId) {
    frame(app, wid);
    app.splice_autopilot_menu(wid);
}

/// Publish the absolute working directory and the repository's root beside
/// the session's facts, as the resolver does.
fn publish_side(app: &App, session: u64) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.set_claude_side(
        PGID,
        Some(Arc::new(FactSide::at(
            Some("/Users//someone/aterm".into()),
            Some("/Users//someone".into()),
        ))),
    );
}

/// The rig at 160 columns, the facts published (with their side where
/// `side`), Claude's block drawn and one frame painted.
fn glass(side: bool) -> (App, WindowId, u64, std::os::unix::net::UnixStream) {
    let (mut app, wid, session, reader) = wider_app();
    publish_facts(&app, session);
    if side {
        publish_side(&app, session);
    }
    draw_claude(&app, session, BYPASS);
    menu_frame(&mut app, wid);
    (app, wid, session, reader)
}

/// A press and release of `button` on the first cell of `fact` as the
/// painter put it.
fn press_fact(app: &mut App, wid: WindowId, session: u64, fact: Fact, button: MouseButton) {
    let cell = app.fact_anchor(wid, session, fact).expect("on the glass");
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, button);
    app.on_mouse_input(wid, ElementState::Released, button);
}

/// The open menu's row whose stable key is `key`.
fn row_of(app: &App, key: &str) -> usize {
    app.autopilot_menu
        .as_ref()
        .expect("a menu is open")
        .rows
        .iter()
        .position(|row| matches!(row, MenuRow::Item { act, .. } if act.key() == key))
        .unwrap_or_else(|| panic!("{key} is a row"))
}

/// The open menu's owner.
fn owner(app: &App) -> Option<MenuOwner> {
    app.autopilot_menu.as_ref().map(|menu| menu.owner)
}

/// A RIGHT-CLICK ON A FACT OPENS ITS MENU HUNG FROM IT: the card above the
/// fact's row, `controls fact-menu` naming the fact and its rows, the
/// Autopilot menu's controls reading it as not theirs — and nothing typed,
/// no person's gesture stamped, nothing hidden. CONTROL: a left click on the
/// same fact opens no menu (it hides the fact).
#[test]
fn a_right_click_on_a_fact_opens_its_menu_hung_from_it() {
    let (mut app, wid, session, mut reader) = glass(true);
    let human = human_seq(&app, session);
    assert_eq!(app.fact_menu_controls(), ["fact-menu open=false"]);
    press_fact(&mut app, wid, session, Fact::Path, MouseButton::Right);
    assert_eq!(owner(&app), Some(MenuOwner::Fact(Fact::Path)));
    let anchor = app.autopilot_menu.as_ref().expect("open").anchor;
    assert_eq!(Some(anchor), app.fact_anchor(wid, session, Fact::Path));
    menu_frame(&mut app, wid);
    let rect = app
        .autopilot_menu
        .as_ref()
        .and_then(|m| m.rect)
        .expect("painted");
    assert_eq!(rect.row + rect.h, anchor.0, "hung above the fact's row");
    let lines = app.fact_menu_controls();
    assert!(
        lines[0].starts_with("fact-menu open=true session=") && lines[0].contains(" fact=path "),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("fact-menu item ")
            && l.contains("key=fact-menu/path/copy-path enabled=true")),
        "{lines:?}"
    );
    assert_eq!(app.autopilot_menu_controls(), ["autopilot-menu open=false"]);
    assert_eq!(
        app.fact_mask(session),
        FactMask::default(),
        "nothing hidden"
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    assert_eq!(human_seq(&app, session), human, "no person's gesture");
    // Esc closes it, and is the menu's.
    assert!(app.autopilot_menu_nav(wid, crate::autopilot_menu::MenuKey::Close));
    assert!(app.autopilot_menu.is_none());
    // CONTROL: a left click.
    menu_frame(&mut app, wid);
    press_fact(&mut app, wid, session, Fact::Path, MouseButton::Left);
    assert!(app.autopilot_menu.is_none(), "a left click opens no menu");
    assert_eq!(
        app.fact_mask(session),
        FactMask::default().with(FactKind::Path, true)
    );
}

/// COPY PATH PUTS THE ABSOLUTE WORKING DIRECTORY ON THE CLIPBOARD — never the
/// rule's `~/aterm` — and says so in the lights' title slot for a moment
/// (a block of chips drawn: a host here). NEGATIVE CONTROL: with no side
/// published the row is off, and choosing it leaves the clipboard untouched.
#[test]
fn copy_path_writes_the_absolute_cwd() {
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
    let (mut app, wid, session, mut reader) = glass(true);
    let _host = host(&mut app);
    menu_frame(&mut app, wid);
    press_fact(&mut app, wid, session, Fact::Path, MouseButton::Right);
    let i = row_of(&app, "fact-menu/path/copy-path");
    app.choose_autopilot_menu_row(i);
    assert!(app.autopilot_menu.is_none(), "chosen and closed");
    assert_eq!(
        crate::control::PBPASTE_STUB
            .with(|b| b.borrow().clone())
            .as_deref(),
        Some("/Users//someone/aterm")
    );
    // The pointer off the folder (it chose a row of the card above): a
    // fact's title under it outranks the notice.
    let (x, y) = px_of(&app, wid, (2, 2));
    app.on_cursor_moved(wid, x, y);
    menu_frame(&mut app, wid);
    let text = row_text(&app, wid, footer_row(&app, wid));
    assert!(text.contains("\u{2713} copied"), "{text:?}");
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    // CONTROL: no side.
    crate::control::PBPASTE_STUB.with(|b| *b.borrow_mut() = None);
    let (mut app, wid, session, _reader) = glass(false);
    press_fact(&mut app, wid, session, Fact::Path, MouseButton::Right);
    let i = row_of(&app, "fact-menu/path/copy-path");
    assert!(!app.autopilot_menu.as_ref().expect("open").rows[i].selectable());
    app.choose_autopilot_menu_row(i);
    assert_eq!(
        crate::control::PBPASTE_STUB.with(|b| b.borrow().clone()),
        None
    );
}

/// NEW TAB HERE ASKS FOR A TAB IN THE WORKING DIRECTORY, and the branch's
/// row for one at the repository's root — what a headless App can show of
/// a tab it has no event loop to spawn. Neither types into the session.
#[test]
fn new_tab_here_asks_for_the_cwd() {
    NEW_TABS_ASKED.with(|asked| asked.borrow_mut().clear());
    let (mut app, wid, session, mut reader) = glass(true);
    for (fact, key) in [
        (Fact::Path, "fact-menu/path/new-tab-here"),
        (Fact::Branch, "fact-menu/branch/new-tab-at-root"),
    ] {
        press_fact(&mut app, wid, session, fact, MouseButton::Right);
        let i = row_of(&app, key);
        app.choose_autopilot_menu_row(i);
        menu_frame(&mut app, wid);
    }
    assert_eq!(
        NEW_TABS_ASKED.with(|asked| asked.borrow().clone()),
        ["/Users//someone/aterm", "/Users//someone"]
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
}

/// HIDE AND SHOW FROM A MENU: `Hide folder` hides it as a click does, and the
/// branch's menu then offers `Show folder`, which shows it again.
#[test]
fn hide_and_show_from_a_menu_flip_the_sessions_mask() {
    let (mut app, wid, session, _reader) = glass(true);
    press_fact(&mut app, wid, session, Fact::Path, MouseButton::Right);
    app.choose_autopilot_menu_row(row_of(&app, "fact-menu/path/hide/path"));
    assert_eq!(
        app.fact_mask(session),
        FactMask::default().with(FactKind::Path, true)
    );
    menu_frame(&mut app, wid);
    press_fact(&mut app, wid, session, Fact::Branch, MouseButton::Right);
    app.choose_autopilot_menu_row(row_of(&app, "fact-menu/branch/show/path"));
    assert_eq!(app.fact_mask(session), FactMask::default());
}

/// THE MENU STAYS WHILE ITS FACT SLIDES ALONG ITS ROW (the folder before the
/// model's place shrank, so the model moved) and closes at the first frame
/// whose glass no longer shows it there (a box over the composer).
/// NEGATIVE CONTROL: a focus change closes it too.
#[test]
fn a_fact_menu_stays_while_its_fact_slides_and_closes_when_it_leaves() {
    let (mut app, wid, session, _reader) = glass(true);
    let before = app.fact_anchor(wid, session, Fact::Model).expect("shown");
    press_fact(&mut app, wid, session, Fact::Model, MouseButton::Right);
    menu_frame(&mut app, wid);
    publish_facts_at(&app, session, "~/a");
    app.on_claude_footer_changed(session);
    menu_frame(&mut app, wid);
    let after = app.fact_anchor(wid, session, Fact::Model).expect("shown");
    assert_ne!(after, before, "CONTROL: the model slid");
    assert_eq!(after.0, before.0, "along its row");
    assert_eq!(owner(&app), Some(MenuOwner::Fact(Fact::Model)), "it stays");
    // A box over the composer: no rule, no fact.
    feed(
        &app,
        session,
        "\x1b[2J\x1b[18;1H Bash command\x1b[20;1H Do you want to proceed?\x1b[21;1H \u{276F} 1. Yes\x1b[22;1H   2. No"
            .as_bytes(),
    );
    menu_frame(&mut app, wid);
    assert!(app.autopilot_menu.is_none(), "closed with its fact");
    // NEGATIVE CONTROL: a focus change.
    draw_claude(&app, session, BYPASS);
    menu_frame(&mut app, wid);
    press_fact(&mut app, wid, session, Fact::Branch, MouseButton::Right);
    menu_frame(&mut app, wid);
    assert!(app.autopilot_menu.is_some(), "the same focus: it stays");
    app.autopilot_menu.as_mut().expect("open").focus = Some(session + 1);
    menu_frame(&mut app, wid);
    assert!(app.autopilot_menu.is_none(), "the focus moved: closed");
}

/// ONE MENU AT A TIME: a fact's menu opened over the Autopilot menu closes
/// it, and the Autopilot menu opened over a fact's closes that; each one's
/// `controls` reads the other's as shut.
#[test]
fn one_menu_at_a_time() {
    let (mut app, wid, _session, _reader) = glass(true);
    let _host = host(&mut app);
    menu_frame(&mut app, wid);
    app.open_front_autopilot_menu()
        .expect("the chip is on the glass");
    assert_eq!(owner(&app), Some(MenuOwner::Autopilot));
    app.open_front_fact_menu(Fact::Branch)
        .expect("the branch is on the glass");
    assert_eq!(owner(&app), Some(MenuOwner::Fact(Fact::Branch)));
    assert_eq!(app.autopilot_menu_controls(), ["autopilot-menu open=false"]);
    app.open_front_autopilot_menu()
        .expect("the chip is on the glass");
    assert_eq!(owner(&app), Some(MenuOwner::Autopilot));
    assert_eq!(app.fact_menu_controls(), ["fact-menu open=false"]);
    // `open fact-menu close` leaves the Autopilot menu open.
    app.close_fact_menu();
    assert_eq!(owner(&app), Some(MenuOwner::Autopilot));
}

/// THE KEYBOARD OPENS A SELECTED FACT'S MENU: `↓` on the folder the keyboard
/// selected opens its menu hung from it, its first choice lit. CONTROL:
/// `↓` with nothing selected is Claude's (no menu).
#[cfg(target_os = "macos")]
#[test]
fn down_on_a_selected_fact_opens_its_menu() {
    use aterm_window::keyboard::{KeyCode, NamedKey};
    let (mut app, wid, session, _reader) = glass(true);
    tap(&mut app, wid, NamedKey::ArrowDown, KeyCode::ArrowDown, "");
    assert!(app.autopilot_menu.is_none(), "CONTROL: nothing selected");
    app.windows
        .get_mut(&wid)
        .expect("window")
        .claude_lights
        .select(Some((session, super::Target::Fact(Fact::Path))));
    menu_frame(&mut app, wid);
    tap(&mut app, wid, NamedKey::ArrowDown, KeyCode::ArrowDown, "");
    let menu = app.autopilot_menu.as_ref().expect("opened");
    assert_eq!(menu.owner, MenuOwner::Fact(Fact::Path));
    assert_eq!(Some(menu.anchor), app.fact_anchor(wid, session, Fact::Path));
    assert_eq!(
        menu.highlight,
        Some(row_of(&app, "fact-menu/path/new-tab-here")),
        "the first choice lit"
    );
}

/// Publish the owner's facts with the session's usage (`Σ 6m33s ↑20M ↓82k`)
/// and, beside them, the side with four minutes of traffic ending now, as
/// the resolver does.
fn publish_usage(app: &App, session: u64) {
    let now = crate::autopilot_timers::wall_ms().div_euclid(1000);
    let start = now.div_euclid(60) * 60 - 180;
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    assert!(timeline.set_claude_footer(
        PGID,
        None,
        Some(FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            usage: Some(UsageFacts {
                models: vec![ModelTokens {
                    label: "opus".into(),
                    input: 20_000_000,
                    output: 82_000,
                }],
                runtime: Some(393),
                ..UsageFacts::default()
            }),
            ..FooterFacts::default()
        }),
    ));
    timeline.set_claude_side(
        PGID,
        Some(Arc::new(
            FactSide::at(
                Some("/Users//someone/aterm".into()),
                Some("/Users//someone".into()),
            )
            .with_traffic(TrafficView {
                start_s: start,
                width_s: 60,
                buckets: vec![(1000, 10), (0, 0), (4000, 40), (2000, 0)].into(),
                busy: vec![(start + 120, start + 180)].into(),
                runtime_s: Some(393),
                unread: 0,
                totals: (20_000_000, 82_000),
            }),
        )),
    );
}

/// The rig at 160 columns with [`publish_usage`]'s facts, Claude's block
/// drawn and one frame painted.
fn usage_glass() -> (App, WindowId, u64, std::os::unix::net::UnixStream) {
    let (mut app, wid, session, reader) = wider_app();
    publish_usage(&app, session);
    draw_claude(&app, session, BYPASS);
    menu_frame(&mut app, wid);
    (app, wid, session, reader)
}

/// A CLICK ON THE USAGE OPENS THE TRAFFIC CARD AND A SECOND CLICK CLOSES IT
/// (owner: "the traffic shows a graph over time"): the card hung from `Σ`,
/// its two bands and axis inert rows `controls fact-menu` reads as plots;
/// the second click on `Σ` spent on the closing — nothing hidden, nothing
/// typed, no person's gesture. CONTROL: a third click opens it again.
#[test]
fn a_click_on_usage_opens_the_card_and_a_second_click_closes_it() {
    let (mut app, wid, session, mut reader) = usage_glass();
    let human = human_seq(&app, session);
    let anchor = app
        .fact_anchor(wid, session, Fact::Usage)
        .expect("\u{3a3} is on the glass");
    press_fact(&mut app, wid, session, Fact::Usage, MouseButton::Left);
    assert_eq!(owner(&app), Some(MenuOwner::Fact(Fact::Usage)));
    let menu = app.autopilot_menu.as_ref().expect("open");
    assert_eq!(menu.anchor, anchor);
    assert_eq!(
        menu.rows
            .iter()
            .filter(|row| matches!(row, MenuRow::Plot(_)))
            .count(),
        5,
        "two bands of two rows and the axis"
    );
    assert!(menu.rows.iter().all(|row| !row.selectable()), "a card");
    menu_frame(&mut app, wid);
    let rect = app
        .autopilot_menu
        .as_ref()
        .and_then(|m| m.rect)
        .expect("painted");
    assert_eq!(rect.row + rect.h, anchor.0, "hung above the rule");
    let lines = app.fact_menu_controls();
    assert!(lines[0].contains(" fact=usage "), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("fact-menu plot index=") && l.contains("\u{2191} in")),
        "{lines:?}"
    );
    // The second click on Σ closes the card.
    press_fact(&mut app, wid, session, Fact::Usage, MouseButton::Left);
    assert!(app.autopilot_menu.is_none(), "closed");
    assert_eq!(
        app.fact_mask(session),
        FactMask::default(),
        "nothing hidden"
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    assert_eq!(human_seq(&app, session), human, "no person's gesture");
    // CONTROL: the next click opens it again.
    menu_frame(&mut app, wid);
    press_fact(&mut app, wid, session, Fact::Usage, MouseButton::Left);
    assert_eq!(owner(&app), Some(MenuOwner::Fact(Fact::Usage)));
}

/// A CLOSED CARD READS NO SIDE: frames, a hover over `Σ` (its title built)
/// and a wake of the facts with the card closed never read the traffic
/// published beside them — the card's read is an open's and an open card's
/// wake's. NEGATIVE CONTROL: the open reads it, and so does a wake while it
/// is open.
#[test]
fn a_closed_card_reads_no_side() {
    let (mut app, wid, session, _reader) = usage_glass();
    crate::work_counts::take_traffic_side_reads();
    let cell = app
        .fact_anchor(wid, session, Fact::Usage)
        .expect("\u{3a3} is on the glass");
    let (x, y) = px_of(&app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    menu_frame(&mut app, wid);
    app.on_claude_footer_changed(session);
    menu_frame(&mut app, wid);
    assert_eq!(crate::work_counts::take_traffic_side_reads(), 0);
    // CONTROL: open, then a wake while open.
    press_fact(&mut app, wid, session, Fact::Usage, MouseButton::Left);
    assert_eq!(crate::work_counts::take_traffic_side_reads(), 1, "the open");
    menu_frame(&mut app, wid);
    app.on_claude_footer_changed(session);
    assert_eq!(crate::work_counts::take_traffic_side_reads(), 1, "the wake");
    menu_frame(&mut app, wid);
    assert_eq!(crate::work_counts::take_traffic_side_reads(), 0, "a frame");
}

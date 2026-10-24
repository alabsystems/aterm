// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Glass-level gesture tests for [`crate::fact_menu`] and the usage traffic card.

use std::sync::Arc;

use aterm_agent::harness::footer::{FactKind, FactMask, FooterFacts};
use aterm_agent::harness::session_usage::{ModelTokens, TrafficView, UsageFacts};
use aterm_core::render::{BandIcon, ChromeIcon};
use aterm_window::event::{ElementState, MouseButton};

use super::Fact;
use super::gesture_tests::*;
use crate::autopilot_menu::{MenuOwner, MenuRow};
use crate::claude_footer::FactSide;
use crate::fact_menu::NEW_TABS_ASKED;
use crate::tab_menu::MenuRect;
use crate::{App, WindowId};

fn menu_frame(app: &mut App, wid: WindowId) {
    frame(app, wid);
    app.splice_autopilot_menu(wid);
}

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

fn press_fact(app: &mut App, wid: WindowId, session: u64, fact: Fact, button: MouseButton) {
    let cell = app.fact_anchor(wid, session, fact).expect("on the glass");
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, button);
    app.on_mouse_input(wid, ElementState::Released, button);
}

fn row_of(app: &App, key: &str) -> usize {
    app.autopilot_menu
        .as_ref()
        .expect("a menu is open")
        .rows
        .iter()
        .position(|row| matches!(row, MenuRow::Item { act, .. } if act.key() == key))
        .unwrap_or_else(|| panic!("{key} is a row"))
}

fn owner(app: &App) -> Option<MenuOwner> {
    app.autopilot_menu.as_ref().map(|menu| menu.owner)
}

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
    assert!(app.autopilot_menu_nav(wid, crate::autopilot_menu::MenuKey::Close));
    assert!(app.autopilot_menu.is_none());
    menu_frame(&mut app, wid);
    press_fact(&mut app, wid, session, Fact::Path, MouseButton::Left);
    assert!(app.autopilot_menu.is_none(), "a left click opens no menu");
    assert_eq!(
        app.fact_mask(session),
        FactMask::default().with(FactKind::Path, true)
    );
}

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
    let (x, y) = px_of(&app, wid, (2, 2));
    app.on_cursor_moved(wid, x, y);
    menu_frame(&mut app, wid);
    let text = row_text(&app, wid, footer_row(&app, wid));
    assert!(text.contains("\u{2713} copied"), "{text:?}");
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
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
    feed(
        &app,
        session,
        "\x1b[2J\x1b[18;1H Bash command\x1b[20;1H Do you want to proceed?\x1b[21;1H \u{276F} 1. Yes\x1b[22;1H   2. No"
            .as_bytes(),
    );
    menu_frame(&mut app, wid);
    assert!(app.autopilot_menu.is_none(), "closed with its fact");
    draw_claude(&app, session, BYPASS);
    menu_frame(&mut app, wid);
    press_fact(&mut app, wid, session, Fact::Branch, MouseButton::Right);
    menu_frame(&mut app, wid);
    assert!(app.autopilot_menu.is_some(), "the same focus: it stays");
    app.autopilot_menu.as_mut().expect("open").focus = Some(session + 1);
    menu_frame(&mut app, wid);
    assert!(app.autopilot_menu.is_none(), "the focus moved: closed");
}

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

fn usage_glass() -> (App, WindowId, u64, std::os::unix::net::UnixStream) {
    let (mut app, wid, session, reader) = wider_app();
    publish_usage(&app, session);
    draw_claude(&app, session, BYPASS);
    menu_frame(&mut app, wid);
    (app, wid, session, reader)
}

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
    press_fact(&mut app, wid, session, Fact::Usage, MouseButton::Left);
    assert!(app.autopilot_menu.is_none(), "closed");
    assert_eq!(
        app.fact_mask(session),
        FactMask::default(),
        "nothing hidden"
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    assert_eq!(human_seq(&app, session), human, "no person's gesture");
    menu_frame(&mut app, wid);
    press_fact(&mut app, wid, session, Fact::Usage, MouseButton::Left);
    assert_eq!(owner(&app), Some(MenuOwner::Fact(Fact::Usage)));
}

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
    press_fact(&mut app, wid, session, Fact::Usage, MouseButton::Left);
    assert_eq!(crate::work_counts::take_traffic_side_reads(), 1, "the open");
    menu_frame(&mut app, wid);
    app.on_claude_footer_changed(session);
    assert_eq!(crate::work_counts::take_traffic_side_reads(), 1, "the wake");
    menu_frame(&mut app, wid);
    assert_eq!(crate::work_counts::take_traffic_side_reads(), 0, "a frame");
}

fn drawn_marks(app: &App, wid: WindowId) -> Vec<(usize, ChromeIcon)> {
    let mut marks: Vec<(usize, ChromeIcon)> = app.windows[&wid]
        .input_scratch
        .chrome_rasters
        .iter()
        .flat_map(|raster| raster.icons.iter().map(|i| (usize::from(raster.row), *i)))
        .filter(|(_, i)| BandIcon::glass_mark(i.icon.ch()).is_some())
        .collect();
    marks.sort_unstable_by_key(|&(row, i)| (row, i.col));
    marks
}

fn card_and_its_marks(app: &App, wid: WindowId) -> (MenuRect, Vec<(usize, ChromeIcon)>) {
    let rect = app
        .autopilot_menu
        .as_ref()
        .and_then(|m| m.rect)
        .expect("the card is painted");
    let cells = &app.windows[&wid].input_scratch.cells;
    let printed = (rect.row..rect.row + rect.h)
        .flat_map(|row| (rect.col..rect.col + rect.w).map(move |col| (row, col)))
        .filter_map(|(row, col)| {
            let cell = cells[row][col];
            let normal = cell.script == aterm_core::terminal::Script::Normal;
            let icon = BandIcon::glass_mark(cell.ch).filter(|_| normal && !cell.wide)?;
            let col = u16::try_from(col).expect("a frame column");
            Some((row, ChromeIcon { col, icon }))
        })
        .collect();
    (rect, printed)
}

#[test]
fn a_cards_marks_are_drawn_while_it_is_open_and_gone_once_it_closes() {
    use crate::fact_menu::FactMenuFacts;
    let (mut app, wid, _session, _reader) = glass(true);
    let _host = host(&mut app);
    menu_frame(&mut app, wid);
    let before = drawn_marks(&app, wid);
    let autopilot = |app: &mut App| {
        app.open_front_autopilot_menu()
            .expect("the chip is on the glass");
    };
    let effort = |app: &mut App| {
        app.open_front_fact_menu(Fact::Effort)
            .expect("the effort is on the glass");
        let facts = FactMenuFacts {
            title: "Effort \u{b7} tab 1".into(),
            room: 44,
            effort: Some("xhigh".into()),
            key_row: Some(true),
            ..FactMenuFacts::default()
        };
        app.autopilot_menu.as_mut().expect("open").rows =
            crate::fact_menu::rows(Fact::Effort, &facts);
    };
    let folder = |app: &mut App| {
        app.open_front_fact_menu(Fact::Path)
            .expect("the folder is on the glass");
    };
    type Case = (
        &'static str,
        fn(&mut App),
        &'static [BandIcon],
        &'static [char],
    );
    let cases: [Case; 3] = [
        (
            "Autopilot",
            autopilot,
            &[BandIcon::ToBar, BandIcon::Ring, BandIcon::Disc],
            &['\u{221e}', '\u{2699}'],
        ),
        (
            "effort",
            effort,
            &[BandIcon::Fisheye, BandIcon::InsetDiamond, BandIcon::Ring],
            &['\u{22ef}'],
        ),
        (
            "folder",
            folder,
            &[BandIcon::House, BandIcon::Ring],
            &['+', '\u{29c9}'],
        ),
    ];
    for (what, open, want, plain) in cases {
        open(&mut app);
        menu_frame(&mut app, wid);
        let (rect, printed) = card_and_its_marks(&app, wid);
        for icon in want {
            assert!(
                printed.iter().any(|(_, i)| i.icon == *icon),
                "{what}: the card prints {:?}: {printed:?}",
                icon.ch()
            );
        }
        let on_card = |app: &App| {
            drawn_marks(app, wid)
                .into_iter()
                .filter(|(row, i)| rect.contains(*row, usize::from(i.col)))
                .collect::<Vec<_>>()
        };
        assert_eq!(on_card(&app), printed, "{what}: each mark drawn, once");
        menu_frame(&mut app, wid);
        assert_eq!(on_card(&app), printed, "{what}: painted again, once");
        let cells = &app.windows[&wid].input_scratch.cells;
        for &ch in plain {
            let at = (rect.row..rect.row + rect.h)
                .flat_map(|row| (rect.col..rect.col + rect.w).map(move |col| (row, col)))
                .find(|&(row, col)| cells[row][col].ch == ch)
                .unwrap_or_else(|| panic!("{what}: the card prints {ch:?}"));
            assert!(
                !printed
                    .iter()
                    .any(|&(row, i)| (row, usize::from(i.col)) == at),
                "{what}: control, {ch:?} draws nothing"
            );
        }
        assert!(app.close_autopilot_menu(), "{what}: closed");
        menu_frame(&mut app, wid);
        assert_eq!(
            drawn_marks(&app, wid),
            before,
            "{what}: closed, the glass's marks alone"
        );
    }
}

/// The name a screen reader hears for the card open on `wid`.
#[cfg(a11y_tree)]
fn card_name(app: &App, wid: WindowId) -> String {
    let tree = app.grid_a11y_tree(wid, None).expect("the grid's tree");
    tree.nodes
        .iter()
        .find(|(_, n)| n.role() == aterm_a11y::model::Role::Menu)
        .and_then(|(_, n)| n.label().map(str::to_owned))
        .expect("the card is published, named")
}

/// A FACT'S CARD IS NAMED IN THE WORDS IT STATES (review 13): a card's
/// lines, the model's `◆  Opus 5.5` and the traffic's `6m33s working ·
/// ↑20M in · ↓82k out` and `peak …` under a graph a reader cannot see,
/// reach the card's name in words: every line, in order, its marks and
/// arrows left out, a `·` a comma, the counts and times said as a reader
/// says them, and the graph named by what it spans. CONTROL: the glass
/// draws the marks the name leaves out.
#[cfg(a11y_tree)]
#[test]
fn a_fact_card_is_named_in_its_words_never_its_glyphs() {
    let marks = ['\u{25c6}', '\u{2191}', '\u{2193}', '\u{b7}', '\u{2581}'];
    let (mut app, wid, session, _reader) = usage_glass();
    press_fact(&mut app, wid, session, Fact::Model, MouseButton::Right);
    menu_frame(&mut app, wid);
    assert_eq!(owner(&app), Some(MenuOwner::Fact(Fact::Model)));
    let model = card_name(&app, wid);
    assert!(model.ends_with(", Opus 5.5"), "{model}");
    let drawn: Vec<String> = app
        .autopilot_menu
        .as_ref()
        .expect("open")
        .rows
        .iter()
        .filter_map(|row| match row {
            MenuRow::Note { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        drawn,
        ["\u{25c6}  Opus 5.5"],
        "CONTROL: the glass draws the mark"
    );
    app.close_autopilot_menu();
    menu_frame(&mut app, wid);
    press_fact(&mut app, wid, session, Fact::Usage, MouseButton::Left);
    menu_frame(&mut app, wid);
    assert_eq!(owner(&app), Some(MenuOwner::Fact(Fact::Usage)));
    let usage = card_name(&app, wid);
    let worked = usage
        .find(", 6 minutes 33 seconds working, 20 million in, 82 thousand out, graph from ")
        .unwrap_or_else(|| panic!("both lines, in order: {usage}"));
    assert!(usage[worked..].contains(" to now, peak "), "{usage}");
    assert!(usage.contains(" out per "), "{usage}");
    assert!(!usage.contains(char::is_control), "{usage}");
    for name in [&model, &usage] {
        assert!(!name.contains(marks), "{name}");
    }
    let lines = app.fact_menu_controls();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\u{2191}20M in \u{b7} \u{2193}82k out")),
        "CONTROL: the glass draws the arrows and the dot: {lines:?}"
    );
}

/// A LINE'S SPOKEN WORDS (review 13): a mark or a meter's glyphs left out,
/// a `·` or `—` between two parts a comma, the words between kept as they
/// stand. CONTROL: a line of words alone is said as it reads.
#[test]
fn a_lines_spoken_words_leave_its_glyphs_out() {
    use crate::autopilot_menu::spoken_words;
    for (line, said) in [
        (
            "\u{2386}  sol@example.test \u{b7} max 20x",
            "sol@example.test, max 20x",
        ),
        (
            "\u{25f7}  \u{25b0}\u{25b0}\u{25b1} 64% weekly",
            "64% weekly",
        ),
        (
            "No rotation accounts configured \u{2014} run aterm init",
            "No rotation accounts configured, run aterm init",
        ),
        ("\u{2191}20M in \u{b7} \u{2193}82k out", "20M in, 82k out"),
        ("+3 unread not graphed", "+3 unread not graphed"),
        ("\u{2387}  main", "main"),
        ("\u{2302}", ""),
    ] {
        assert_eq!(spoken_words(line), said, "{line:?}");
    }
}

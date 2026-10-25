// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unit tests for `crate::fact_menu` rows and the traffic card.

use std::path::PathBuf;

use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_agent::harness::session_usage::TrafficView;

use super::{
    AccountLabel, AccountMenuRow, FactAct, FactMenuFacts, SessionWeek, account_menu_rows_from,
    band_glyphs, band_level, rows, traffic_rows, weekly_facts, weekly_gap_why, weekly_meter_bar,
};
use crate::autopilot_menu::{MenuAct, MenuRow, PlotInk};
use crate::claude_lights::Fact;

fn mask(kinds: &[FactKind]) -> FactMask {
    kinds
        .iter()
        .fold(FactMask::default(), |m, k| m.with(*k, true))
}

fn menu_facts(off: FactMask, side: bool) -> FactMenuFacts {
    FactMenuFacts {
        title: "Folder \u{b7} tab 1".into(),
        name: None,
        room: 44,
        spoken: "Folder, tab 1".into(),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        context: Some("ctx 84% left".into()),
        subscription: None,
        weekly_meter: None,
        weekly_gap: None,
        cli: None,
        accounts: Vec::new(),
        auto_rotate: true,
        off,
        cwd: side.then(|| PathBuf::from("/Users//someone/aterm")),
        repo_root: side.then(|| PathBuf::from("/Users//someone")),
        git_hygiene: Vec::new(),
        folder_hygiene: Vec::new(),
        key_row: Some(true),
        note: None,
        typing: false,
    }
}

fn shown(rows: &[MenuRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            MenuRow::Item {
                act,
                label,
                enabled,
                glyph,
                ..
            } => format!("{glyph} {label} [{}] {enabled}", act.key()),
            MenuRow::Header(t) => format!("header {t}"),
            MenuRow::Note { text, .. } => format!("note {text}"),
            MenuRow::Rule => "rule".into(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn each_fact_menu_offers_its_rows_and_its_hide() {
    let on = menu_facts(FactMask::default(), true);
    assert_eq!(
        shown(&rows(Fact::Path, &on)),
        [
            "header Folder \u{b7} tab 1",
            "note \u{2302}  ~/aterm",
            "+ New tab here [fact-menu/path/new-tab-here] true",
            "\u{29c9} Copy path [fact-menu/path/copy-path] true",
            "rule",
            "\u{25cb} Hide folder [fact-menu/path/hide/path] true",
        ]
    );
    assert_eq!(
        shown(&rows(Fact::Branch, &on))[1..],
        [
            "note \u{2387}  main",
            "\u{29c9} Copy branch name [fact-menu/branch/copy-branch] true",
            "+ New tab at the repository root [fact-menu/branch/new-tab-at-root] true",
            "rule",
            "\u{25cb} Hide branch [fact-menu/branch/hide/branch] true",
        ]
    );
    assert_eq!(
        shown(&rows(Fact::Model, &on))[1..],
        [
            "note \u{25c6}  Opus 5.5",
            "\u{22ef} Change model\u{2026}  /model [fact-menu/model/type/model] true",
            "rule",
            "\u{25cb} Hide model [fact-menu/model/hide/model] true",
        ]
    );
    assert_eq!(
        shown(&rows(Fact::Effort, &on))[1..],
        [
            "note \u{25c9}  xhigh".to_owned(),
            format!(
                "\u{25c8} Turn {} on for this session [fact-menu/effort/type/key-on] true",
                aterm_primer::CLAUDE_TOP_EFFORT_KEY
            ),
            "\u{22ef} Effort slider\u{2026}  /effort [fact-menu/effort/type/effort] true"
                .to_owned(),
            "rule".to_owned(),
            "\u{25cb} Hide effort [fact-menu/effort/hide/effort] true".to_owned(),
        ]
    );
    assert_eq!(
        shown(&rows(Fact::Context, &on))[1..],
        [
            "note \u{25d0}  ctx 84% left",
            "\u{22ef} Compact context\u{2026}  /compact [fact-menu/context/type/compact] true",
            "rule",
            "\u{25cb} Hide context [fact-menu/context/hide/context] true",
        ]
    );
    let with_hygiene = FactMenuFacts {
        git_hygiene: vec![
            "\u{2191}2 ahead \u{b7} \u{2193}1 behind origin/main".into(),
            "\u{2387}  2 worktrees \u{b7} 1 unmerged (feat-a) \u{b7} 1 stale (old-done)".into(),
        ],
        folder_hygiene: vec![
            "\u{26c1}  2 worktrees \u{b7} worktrees 512.0 MiB \u{b7} build/temp 2.0 GiB \u{b7} 120.0 GiB free".into(),
        ],
        ..menu_facts(FactMask::default(), true)
    };
    assert_eq!(
        shown(&rows(Fact::Branch, &with_hygiene))[1..4],
        [
            "note \u{2387}  main",
            "note \u{2191}2 ahead \u{b7} \u{2193}1 behind origin/main",
            "note \u{2387}  2 worktrees \u{b7} 1 unmerged (feat-a) \u{b7} 1 stale (old-done)",
        ]
    );
    assert_eq!(
        shown(&rows(Fact::Path, &with_hygiene))[1..3],
        [
            "note \u{2302}  ~/aterm",
            "note \u{26c1}  2 worktrees \u{b7} worktrees 512.0 MiB \u{b7} build/temp 2.0 GiB \u{b7} 120.0 GiB free",
        ]
    );
    for fact in [Fact::Path, Fact::Branch, Fact::Usage] {
        assert!(
            !shown(&rows(fact, &on)).iter().any(|r| r.contains("/type/")),
            "{fact:?}"
        );
    }
    let off = menu_facts(FactMask::default(), false);
    assert_eq!(
        shown(&rows(Fact::Path, &off))[2..4],
        [
            "+ New tab here [fact-menu/path/new-tab-here] false",
            "\u{29c9} Copy path [fact-menu/path/copy-path] false",
        ]
    );
    assert!(
        !shown(&rows(Fact::Branch, &off))
            .iter()
            .any(|r| r.contains("repository root"))
    );
    assert!(rows(Fact::Usage, &on).iter().all(|r| !r.selectable()));
}

#[test]
fn the_typed_rows_say_what_they_type_and_wait_under_a_note() {
    let key = aterm_primer::CLAUDE_TOP_EFFORT_KEY;
    let tagged = FactMenuFacts {
        key_row: Some(false),
        ..menu_facts(FactMask::default(), true)
    };
    assert_eq!(
        shown(&rows(Fact::Effort, &tagged))[2],
        format!("\u{25c8} Turn {key} off for this session [fact-menu/effort/type/key-off] true")
    );
    let noted = FactMenuFacts {
        note: Some("Claude is working; click when the turn ends".into()),
        ..menu_facts(FactMask::default(), true)
    };
    assert_eq!(
        shown(&rows(Fact::Model, &noted))[1..4],
        [
            "note \u{25c6}  Opus 5.5",
            "note Claude is working; click when the turn ends",
            "\u{22ef} Change model\u{2026}  /model [fact-menu/model/type/model] false",
        ]
    );
    let typing = FactMenuFacts {
        typing: true,
        ..menu_facts(FactMask::default(), true)
    };
    let effort = shown(&rows(Fact::Effort, &typing));
    assert!(effort[2].ends_with("[fact-menu/effort/type/key-on] false"));
    assert!(effort[3].ends_with("[fact-menu/effort/type/effort] false"));
    assert!(effort[5].ends_with("[fact-menu/effort/hide/effort] true"));
    let unkeyed = FactMenuFacts {
        key_row: None,
        ..menu_facts(FactMask::default(), true)
    };
    let effort = shown(&rows(Fact::Effort, &unkeyed));
    assert!(!effort.iter().any(|r| r.contains(key)));
    assert!(effort[2].ends_with("[fact-menu/effort/type/effort] true"));
}

#[test]
fn a_menu_shows_every_hidden_fact_again() {
    let hid = menu_facts(mask(&[FactKind::Path, FactKind::Model]), true);
    assert_eq!(
        shown(&rows(Fact::Path, &hid))[5..],
        [
            "\u{25cf} Show folder [fact-menu/path/show/path] true",
            "\u{25cf} Show model [fact-menu/path/show/model] true",
        ]
    );
    let branch = shown(&rows(Fact::Branch, &hid));
    assert_eq!(
        branch[branch.len() - 3..],
        [
            "\u{25cb} Hide branch [fact-menu/branch/hide/branch] true",
            "\u{25cf} Show model [fact-menu/branch/show/model] true",
            "\u{25cf} Show folder [fact-menu/branch/show/path] true",
        ]
    );
}

const T0: i64 = 86_400 * 19_800 + 14 * 3600 + 2 * 60;

fn traffic() -> TrafficView {
    TrafficView {
        start_s: T0,
        width_s: 60,
        buckets: vec![(1000, 10), (0, 0), (4000, 40), (2000, 0)].into(),
        busy: vec![(T0 + 120, T0 + 180)].into(),
        runtime_s: Some(393),
        unread: 0,
        totals: (20_000_000, 82_000),
    }
}

fn texts(rows: &[MenuRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            MenuRow::Plot(cells) => cells.iter().map(|c| c.ch).collect(),
            MenuRow::Note { text, .. } => format!("note {text}"),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn the_traffic_card_paints_two_bands_and_its_axis() {
    let rows = traffic_rows(&traffic(), T0 + 240, Some(0), 30);
    let text = texts(&rows);
    assert_eq!(text.len(), 7, "{text:#?}");
    assert!(
        rows[..5]
            .iter()
            .all(|r| matches!(r, MenuRow::Plot(c) if c.len() == 30)),
        "{text:#?}"
    );
    assert!(text[0].starts_with("\u{2191} in  "), "{:?}", text[0]);
    assert!(text[1].starts_with("      "), "{:?}", text[1]);
    assert!(text[2].starts_with("\u{2193} out "), "{:?}", text[2]);
    assert!(text[3].starts_with("      "), "{:?}", text[3]);
    assert_eq!(text[4], "      14:02    4m00s       now");
    assert_eq!(
        text[5..],
        [
            "note 6m33s working \u{b7} \u{2191}20M in \u{b7} \u{2193}82k out",
            "note peak \u{2191}667 \u{b7} \u{2193}7 per 10s",
        ]
    );
    let ink = |row: usize| match &rows[row] {
        MenuRow::Plot(cells) => cells[10].ink,
        _ => unreachable!("a graph row"),
    };
    assert_eq!(
        (ink(0), ink(2), ink(4)),
        (PlotInk::In, PlotInk::Out, PlotInk::Axis)
    );
    for (row, line) in text.iter().enumerate().take(4) {
        assert_eq!(line.chars().nth(6 + 7), Some(' '), "row {row}");
    }
    let unread = TrafficView {
        unread: 3,
        ..traffic()
    };
    assert_eq!(
        texts(&traffic_rows(&unread, T0 + 240, Some(0), 30)).last(),
        Some(&"note +3 unread not graphed".to_string())
    );
    assert_eq!(
        texts(&traffic_rows(&TrafficView::default(), T0, Some(0), 30)),
        ["note No traffic yet"]
    );
}

#[test]
fn a_band_peak_fills_both_rows_and_a_trickle_is_at_least_one_eighth() {
    assert_eq!(band_level(667, 667), 16);
    assert_eq!(band_glyphs(16), ('\u{2588}', '\u{2588}'));
    assert_eq!(band_level(1, 1_000_000), 1, "a trickle");
    assert_eq!(band_glyphs(1), (' ', '\u{2581}'));
    assert_eq!(band_glyphs(8), (' ', '\u{2588}'));
    assert_eq!(band_glyphs(9), ('\u{2581}', '\u{2588}'));
    assert_eq!(band_level(166, 667), 4);
    assert_eq!(band_glyphs(4), (' ', '\u{2584}'));
    assert_eq!(band_level(0, 667), 0);
    assert_eq!(band_glyphs(0), (' ', ' '));
    let text = texts(&traffic_rows(&traffic(), T0 + 240, Some(0), 30));
    let at = |row: usize, col: usize| text[row].chars().nth(6 + col);
    assert_eq!((at(0, 12), at(1, 12)), (Some('\u{2588}'), Some('\u{2588}')));
    assert_eq!((at(0, 0), at(1, 0)), (Some(' '), Some('\u{2584}')));
}

#[test]
fn busy_columns_take_the_shaded_ground() {
    use crate::autopilot_menu::{paint, place};
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let c = crate::tab_menu::menu_colors(theme);
    let mut card = vec![MenuRow::Header("Traffic \u{b7} tab 1".into())];
    card.extend(traffic_rows(&traffic(), T0 + 240, Some(0), 30));
    let rect = place(&card, (20, 0), (0..20, 0..80), (80, 21), 0).expect("a card");
    let painted = paint(rect, &card, None, theme);
    let row = &painted[3];
    let text = row.iter().map(|cell| cell.ch).collect::<String>();
    let lead = text
        .chars()
        .position(|ch| ch == '\u{2584}')
        .expect("the first minute's column");
    let busy = &row[lead + 12];
    assert_eq!(busy.ch, '\u{2588}', "{text:?}");
    assert_eq!(
        busy.bg,
        crate::chrome_band::mix3(c.card_bg, c.band.accent, 0.10)
    );
    assert_eq!(busy.fg, c.band.accent);
    assert_eq!(row[lead].bg, c.card_bg);
}

/// THE ACCOUNT MENU GIVES THE FULL WHY of a weekly figure the footer has not got, under the
/// footer's own short word, in lines the card's widest content holds; a meter, where there is
/// one, says it instead. A reader hears the word as the chip speaks it and the why whole, once,
/// never cut at each line the card wraps it to.
#[test]
fn the_account_menu_says_why_the_week_is_missing() {
    use aterm_agent::harness::profile::WeeklyGap;
    use aterm_agent::harness::tap::Refusal;
    for gap in [
        WeeklyGap::TapOff,
        WeeklyGap::Unoffered(Refusal::Exe),
        WeeklyGap::Unoffered(Refusal::Home),
        WeeklyGap::Unoffered(Refusal::Login),
        WeeklyGap::Unoffered(Refusal::Line),
        WeeklyGap::Unoffered(Refusal::Argv),
        WeeklyGap::Unoffered(Refusal::Settings),
        WeeklyGap::Untapped,
        WeeklyGap::Unrestarted,
        WeeklyGap::NoReply,
        WeeklyGap::Stale,
    ] {
        let facts = FactMenuFacts {
            title: "Accounts \u{b7} tab 1".into(),
            weekly_gap: Some(gap),
            ..menu_facts(FactMask::default(), true)
        };
        let menu = shown(&rows(Fact::Account, &facts));
        let head = format!("note \u{25f7}  {}", gap.words());
        let at = menu
            .iter()
            .position(|row| *row == head)
            .expect("the footer's word");
        let why: Vec<String> = weekly_gap_why(gap)
            .iter()
            .map(|line| format!("note {line}"))
            .collect();
        assert_eq!(menu[at + 1..at + 1 + why.len()], why[..], "{gap:?}");
        let built = rows(Fact::Account, &facts);
        let heard: Vec<&str> = built[at..=at + why.len()]
            .iter()
            .map(|row| match row {
                MenuRow::Note { spoken, .. } => spoken.as_str(),
                other => panic!("{gap:?}: a note, not {other:?}"),
            })
            .collect();
        let whole = weekly_gap_why(gap).join(" ");
        let mut said = vec![gap.spoken(), whole.as_str()];
        said.resize(heard.len(), "");
        assert_eq!(
            heard, said,
            "{gap:?}: the word in the chip's words, then the why whole, once"
        );
        for line in weekly_gap_why(gap) {
            assert!(
                crate::tab_menu::display_cells(line) <= crate::autopilot_menu::MAX_CONTENT,
                "{line:?}"
            );
        }
        let metered = FactMenuFacts {
            weekly_meter: Some(weekly_meter_bar("weekly 62% left")),
            ..facts
        };
        assert!(
            !shown(&rows(Fact::Account, &metered)).contains(&head),
            "a meter says the week instead"
        );
    }
}

/// THE ACCOUNT MENU TAKES THE FOOTER'S WEEK as the footer says it: a figure's words (tenths
/// too) give the meter and the session's row its whole percent, an aged figure its age as a
/// store row's, a 7d wall gives `weekly 0% left`, and with neither the footer's reason goes
/// to the menu's why.
#[test]
fn the_account_menu_takes_the_footers_week_or_its_reason() {
    use aterm_agent::harness::profile::{SubscriptionDisplay, WeeklyFigure, WeeklyGap};
    const NOW: i64 = 1_790_000_000;
    let sub = |weekly: Option<&str>, gap: Option<WeeklyGap>| SubscriptionDisplay {
        weekly: weekly.map(str::to_owned),
        weekly_gap: gap,
        ..SubscriptionDisplay::default()
    };
    let left = |pct: u32, age: &str| Some((pct, age.to_owned()));
    assert_eq!(
        weekly_facts(&sub(Some("weekly 61.5% left"), None), false, NOW),
        (
            left(61, ""),
            Some("\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}  weekly 61.5% left".into()),
            None
        ),
        "tenths keep the meter and the row's percent"
    );
    assert_eq!(
        weekly_facts(
            &sub(Some("weekly 62% left \u{b7} 16m ago"), None),
            false,
            NOW
        )
        .0,
        left(62, ""),
        "an aged figure's words with no figure behind them"
    );
    let aged = SubscriptionDisplay {
        weekly_figure: Some(WeeklyFigure {
            used_x10: 380,
            resets_at: NOW + 86_400,
            at: NOW - 40 * 60,
        }),
        ..sub(Some("weekly 62% left \u{b7} 40m ago"), None)
    };
    assert_eq!(
        weekly_facts(&aged, false, NOW).0,
        left(62, " \u{b7} 40m ago"),
        "the session's own aged figure keeps its age in its row"
    );
    assert_eq!(
        weekly_facts(&aged, false, NOW - 30 * 60).0,
        left(62, ""),
        "the control: the same figure while current"
    );
    assert_eq!(
        weekly_facts(&aged, false, NOW + 86_400),
        (None, None, None),
        "its week reset since the footer's read: no row figure and no meter of the expired week"
    );
    assert_eq!(
        weekly_facts(&aged, true, NOW + 86_400).0,
        left(0, ""),
        "a 7d wall still says its own"
    );
    assert_eq!(
        weekly_facts(&sub(None, Some(WeeklyGap::NoReply)), false, NOW),
        (None, None, Some(WeeklyGap::NoReply)),
        "the footer's reason"
    );
    assert_eq!(
        weekly_facts(&sub(None, Some(WeeklyGap::NoReply)), true, NOW).0,
        left(0, ""),
        "a 7d wall over every reason"
    );
    assert_eq!(
        weekly_meter_bar("weekly 61.5% left"),
        "\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}  weekly 61.5% left"
    );
}

/// AN ACCOUNT ROW SAYS HOW OLD ITS STORE FIGURE IS (the footer's one age rule): current for
/// fifteen minutes, then with its age, however old, while its week runs; never an old
/// figure read as current, nor dropped to the `100% left` of an account nobody read.
#[test]
fn an_account_rows_store_figure_carries_its_age() {
    use aterm_agent::harness::accounts;
    const NOW: i64 = 1_790_000_000;
    let roster = accounts::Roster {
        rows: vec![accounts::Row {
            label: "main".into(),
            class: accounts::Class::Dedicated,
            dir: None,
            ..accounts::Row::default()
        }],
    };
    let enrolled = std::iter::once("main".to_string()).collect();
    let label_with = |at: i64, sample: Option<accounts::Sample>| {
        let mut state = accounts::State::default();
        let mut acct = accounts::Account {
            sample,
            ..accounts::Account::default()
        };
        acct.windows.insert(
            "seven_day".into(),
            accounts::WindowFig {
                pct_x10: 385,
                reset: NOW + 86_400,
                at,
                by: "m22".into(),
                source: accounts::SOURCE_TAP.into(),
                epoch_at: None,
            },
        );
        state.accounts.insert("main".into(), acct);
        let (rows, _) = account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            None,
            None,
            SessionWeek::default(),
            NOW,
        );
        rows[0].label.clone()
    };
    let label = |at: i64| label_with(at, None);
    assert_eq!(label(NOW - 60), "main \u{b7} dedicated \u{b7} 61% left");
    assert_eq!(
        label(NOW - 20 * 60),
        "main \u{b7} dedicated \u{b7} 61% left \u{b7} 20m ago"
    );
    assert_eq!(
        label(NOW - 3 * 3600),
        "main \u{b7} dedicated \u{b7} 61% left \u{b7} 3h ago"
    );
    assert_eq!(
        label(NOW - 2 * 86_400),
        "main \u{b7} dedicated \u{b7} 61% left \u{b7} 2d ago",
        "a two-day-old reading whose week still runs"
    );
    let restamped = accounts::Sample {
        week_pct: Some(39),
        week_reset: Some(NOW + 86_400),
        week_read_at: Some(NOW - 3 * 3600),
        at: NOW - 60,
        ..accounts::Sample::default()
    };
    assert_eq!(
        label_with(NOW - 3 * 3600, Some(restamped)),
        "main \u{b7} dedicated \u{b7} 61% left \u{b7} 3h ago",
        "a tap's note re-stamps the week it holds: as old as the window's reading"
    );
    let (rows, _) = account_menu_rows_from(
        &roster,
        &accounts::State::default(),
        &enrolled,
        Some("main"),
        None,
        SessionWeek {
            label: None,
            figure: None,
            left: Some((62, " \u{b7} 40m ago".into())),
        },
        NOW,
    );
    assert_eq!(
        rows[0].label, "main \u{b7} dedicated \u{b7} 62% left \u{b7} 40m ago",
        "no store figure: the session's own, with its age"
    );
    let beside = |session: Option<&str>, at: i64| {
        let mut state = accounts::State::default();
        let mut acct = accounts::Account::default();
        acct.windows.insert(
            "seven_day".into(),
            accounts::WindowFig {
                pct_x10: 385,
                reset: NOW + 86_400,
                at: NOW - 2 * 86_400,
                by: "m22".into(),
                source: accounts::SOURCE_TAP.into(),
                epoch_at: None,
            },
        );
        state.accounts.insert("main".into(), acct);
        let figure = aterm_agent::harness::profile::WeeklyFigure {
            used_x10: 600,
            resets_at: NOW + 86_400,
            at,
        };
        let (rows, _) = account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            session,
            None,
            SessionWeek {
                label: None,
                figure: Some(figure),
                left: Some((40, String::new())),
            },
            NOW,
        );
        rows[0].label.clone()
    };
    assert_eq!(
        beside(Some("main"), NOW - 60),
        "main \u{b7} dedicated \u{b7} 40% left",
        "the session's own figure, read since the store's, on its own account's row"
    );
    assert_eq!(
        beside(Some("main"), NOW - 3 * 86_400),
        "main \u{b7} dedicated \u{b7} 61% left \u{b7} 2d ago",
        "the store's, read since the session's"
    );
    assert_eq!(
        beside(Some("other"), NOW - 60),
        "main \u{b7} dedicated \u{b7} 61% left \u{b7} 2d ago",
        "the control: another account's session figure never reaches this row"
    );
    {
        let mut state = accounts::State::default();
        let acct = accounts::Account {
            warned_week: Some(accounts::WindowSaid {
                pct: Some(20),
                reset: Some(NOW + 86_400),
                at: NOW - 2 * 86_400,
            }),
            ..accounts::Account::default()
        };
        state.accounts.insert("main".into(), acct);
        let figure = aterm_agent::harness::profile::WeeklyFigure {
            used_x10: 600,
            resets_at: NOW + 86_400,
            at: NOW - 25 * 60,
        };
        let (rows, _) = account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            Some("main"),
            None,
            SessionWeek {
                label: None,
                figure: Some(figure),
                left: None,
            },
            NOW,
        );
        assert_eq!(
            rows[0].label, "main \u{b7} dedicated \u{b7} 40% left \u{b7} 25m ago",
            "the session's own figure wins over a warned_week when no store window figure exists"
        );
    }

    let roster = accounts::Roster {
        rows: vec![
            accounts::Row {
                label: "main".into(),
                email: Some("main@example.test".into()),
                dir: Some("/logins/main".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "work".into(),
                email: Some("work@example.test".into()),
                dir: Some("/logins/work".into()),
                ..accounts::Row::default()
            },
        ],
    };
    let enrolled = roster.rows.iter().map(|row| row.label.clone()).collect();
    let menu = |label, email, with_store| {
        let mut state = accounts::State {
            active: Some("work".into()),
            ..accounts::State::default()
        };
        if with_store {
            for (label, used) in [("main", 385), ("work", 200)] {
                let mut acct = accounts::Account::default();
                acct.windows.insert(
                    accounts::WEEK_LIMIT.into(),
                    accounts::WindowFig {
                        pct_x10: used,
                        reset: NOW + 86_400,
                        at: NOW - 40 * 60,
                        by: "m22".into(),
                        source: accounts::SOURCE_TAP.into(),
                        epoch_at: None,
                    },
                );
                state.accounts.insert(label.into(), acct);
            }
        }
        account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            label,
            email,
            SessionWeek {
                label: None,
                figure: Some(aterm_agent::harness::profile::WeeklyFigure {
                    used_x10: 600,
                    resets_at: NOW + 86_400,
                    at: NOW - 60,
                }),
                left: Some((40, String::new())),
            },
            NOW,
        )
        .0
    };
    let rows = menu(None, Some("main@example.test"), true);
    assert!(rows[0].active && !rows[1].active);
    assert!(rows[0].label.ends_with("40% left"));
    assert!(
        rows[1].label.ends_with("80% left \u{b7} 40m ago"),
        "a custom-home session identified only by email cannot replace machine-active work's figure"
    );
    for (label, email) in [
        (None, None),
        (None, Some("unlisted@example.test")),
        (Some("work"), Some("main@example.test")),
    ] {
        let rows = menu(label, email, true);
        assert!(
            !rows[0].active && rows[1].active,
            "the machine's UI fallback"
        );
        assert!(rows[0].label.ends_with("61% left \u{b7} 40m ago"));
        assert!(
            rows[1].label.ends_with("80% left \u{b7} 40m ago"),
            "an unknown or contradictory session identity lends no figure to the active row"
        );
        let rows = menu(label, email, false);
        assert!(
            rows.iter().all(|row| row.label.ends_with("100% left")),
            "nor can its weekly words fill an unobserved active row"
        );
    }
    let rows = menu(Some("main"), None, true);
    assert!(
        rows[0].label.ends_with("40% left"),
        "its own explicit label"
    );
    let rows = menu(None, Some("main@example.test"), false);
    assert!(rows[0].label.ends_with("40% left"));
    assert!(
        rows[1].label.ends_with("100% left"),
        "only the email's own row takes its words"
    );
}

/// THE SESSION'S WEEK REACHES THE ROW ITS LOGIN IS JUDGED TO (`SubscriptionDisplay::label`),
/// on a one-row roster too, where the footer names no account; never one whose email
/// contradicts the session's profile, nor the store's active fallback.
#[test]
fn the_sessions_week_reaches_the_row_its_login_is_judged_to() {
    use aterm_agent::harness::accounts;
    const NOW: i64 = 1_790_000_000;
    let roster = accounts::Roster {
        rows: vec![accounts::Row {
            label: "main".into(),
            class: accounts::Class::Dedicated,
            dir: None,
            email: Some("main@example.test".into()),
            ..accounts::Row::default()
        }],
    };
    let enrolled = std::iter::once("main".to_string()).collect();
    let mut state = accounts::State {
        active: Some("main".into()),
        ..accounts::State::default()
    };
    let mut acct = accounts::Account::default();
    acct.windows.insert(
        accounts::WEEK_LIMIT.into(),
        accounts::WindowFig {
            pct_x10: 385,
            reset: NOW + 86_400,
            at: NOW - 30 * 60,
            by: "m22".into(),
            source: accounts::SOURCE_TAP.into(),
            epoch_at: None,
        },
    );
    state.accounts.insert("main".into(), acct);
    let row = |label: Option<&str>, email: Option<&str>| {
        let (rows, _) = account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            None,
            email,
            SessionWeek {
                label: label.map(str::to_owned),
                figure: Some(aterm_agent::harness::profile::WeeklyFigure {
                    used_x10: 900,
                    resets_at: NOW + 86_400,
                    at: NOW - 60,
                }),
                left: Some((10, String::new())),
            },
            NOW,
        );
        (rows[0].label.clone(), rows[0].active)
    };
    assert_eq!(
        row(Some("main"), None),
        ("main \u{b7} dedicated \u{b7} 10% left".into(), true),
        "the row its login is judged to takes its newer figure, with no email to match"
    );
    let kept = (
        "main \u{b7} dedicated \u{b7} 61% left \u{b7} 30m ago".to_string(),
        true,
    );
    assert_eq!(
        row(Some("main"), Some("other@example.test")),
        kept,
        "a judged row whose email contradicts the session's profile keeps its own week"
    );
    assert_eq!(
        row(None, Some("other@example.test")),
        kept,
        "another account's session, the store's active row: the row keeps its own week"
    );
}

#[test]
fn account_menu_shows_weekly_meter_and_rotation_accounts() {
    assert_eq!(
        weekly_meter_bar("weekly 0% left"),
        "\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}  weekly 0% left"
    );
    assert_eq!(
        weekly_meter_bar("weekly 73% left"),
        "\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}  weekly 73% left"
    );
    assert_eq!(
        weekly_meter_bar("weekly 100% left"),
        "\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}  weekly 100% left"
    );
    let facts = FactMenuFacts {
        title: "Accounts \u{b7} tab 1".into(),
        spoken: "Accounts, tab 1".into(),
        subscription: Some("sol-m27@example.test \u{b7} max 20x".into()),
        weekly_meter: Some(weekly_meter_bar("weekly 0% left")),
        accounts: vec![
            AccountMenuRow {
                account: AccountLabel::new("m27").unwrap(),
                label: "m27 \u{b7} dedicated \u{b7} 0% left (limited)".into(),
                active: true,
                enabled: true,
            },
            AccountMenuRow {
                account: AccountLabel::new("m27-2").unwrap(),
                label: "m27-2 \u{b7} dedicated \u{b7} 100% left (auto-login)".into(),
                active: false,
                enabled: true,
            },
            AccountMenuRow {
                account: AccountLabel::new("m27-3").unwrap(),
                label: "m27-3 \u{b7} pool \u{b7} 100% left (auto-login)".into(),
                active: false,
                enabled: true,
            },
        ],
        auto_rotate: true,
        ..menu_facts(FactMask::default(), true)
    };
    let menu = rows(Fact::Account, &facts);
    assert_eq!(
        shown(&menu),
        [
            "header Accounts \u{b7} tab 1",
            "note \u{2386}  sol-m27@example.test \u{b7} max 20x",
            "note \u{25f7}  \u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}  weekly 0% left",
            "rule",
            "\u{2386} m27 \u{b7} dedicated \u{b7} 0% left (limited) [fact-menu/account/use/m27] true",
            "\u{2386} m27-2 \u{b7} dedicated \u{b7} 100% left (auto-login) [fact-menu/account/use/m27-2] true",
            "\u{2386} m27-3 \u{b7} pool \u{b7} 100% left (auto-login) [fact-menu/account/use/m27-3] true",
            "rule",
            "\u{21bb} Auto-rotate (dedicated \u{2192} pool) [fact-menu/account/auto-rotate] true",
            "\u{22ef} Account settings\u{2026} [fact-menu/account/account-settings] true",
        ]
    );

    use aterm_agent::harness::accounts;
    let chosen = menu
        .iter()
        .find_map(|row| match row {
            MenuRow::Item {
                act: MenuAct::Fact(Fact::Account, FactAct::UseAccount(account)),
                enabled: true,
                ..
            } => Some(*account),
            _ => None,
        })
        .expect("the first displayed account is selectable");
    let labels = ["m27", "m27-2", "m27-3"];
    let roster = accounts::Roster {
        rows: labels
            .iter()
            .map(|label| accounts::Row {
                label: (*label).to_string(),
                dir: Some(format!("/synthetic/{label}")),
                ..accounts::Row::default()
            })
            .collect(),
    };
    let enrolled = labels.iter().map(|label| (*label).to_string()).collect();
    const NOW: i64 = 1_790_000_000;
    let mut before = accounts::State::default();
    for label in labels {
        let _ = accounts::note_wall(
            &mut before,
            label,
            "five_hour",
            Some(NOW + 3600),
            NOW - 10,
            NOW,
        );
    }
    for reordered in [false, true] {
        let mut current = roster.clone();
        if reordered {
            current.rows.reverse();
        }
        let row = chosen
            .row(&current)
            .expect("the selected account remains listed");
        let mut state = before.clone();
        accounts::pin(&mut state, &current, &enrolled, &row.label, NOW).unwrap();
        assert_eq!(state.active.as_deref(), Some("m27"));
        assert!(state.pinned);
        assert!(state.accounts["m27"].walls.is_empty());
        for other in ["m27-2", "m27-3"] {
            assert_eq!(state.accounts[other], before.accounts[other]);
        }
    }
    let mut removed = roster;
    removed.rows.retain(|row| row.label != "m27");
    let mut state = before.clone();
    let result = chosen
        .row(&removed)
        .ok_or_else(|| "the selected account is no longer listed".to_string())
        .and_then(|row| accounts::pin(&mut state, &removed, &enrolled, &row.label, NOW));
    assert!(result.is_err());
    assert_eq!(state, before);
}

#[test]
fn account_menu_rows_from_distinguishes_auto_login_and_disabled_rows() {
    use aterm_agent::harness::accounts;
    const NOW: i64 = 1_790_000_000;
    let roster = accounts::Roster {
        rows: vec![
            accounts::Row {
                label: "m15-2".into(),
                class: accounts::Class::Dedicated,
                dir: None,
                email: Some("m15-2@example.test".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m15-3".into(),
                class: accounts::Class::Dedicated,
                dir: Some("/synthetic/m15-3".into()),
                email: Some("m15-3@example.test".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m15-4".into(),
                class: accounts::Class::Dedicated,
                dir: Some("/synthetic/m15-4".into()),
                email: None,
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m15-5".into(),
                class: accounts::Class::Shared,
                dir: Some("/synthetic/m15-5".into()),
                email: Some("m15-5@example.test".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m15-6".into(),
                class: accounts::Class::Shared,
                dir: Some("/synthetic/m15-6".into()),
                email: None,
                ..accounts::Row::default()
            },
        ],
    };
    let enrolled = ["m15-2", "m15-5", "m15-6"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut state = accounts::State::default();
    let _ = accounts::note_wall(
        &mut state,
        "m15-2",
        "seven_day",
        Some(NOW + 86_400),
        NOW - 60,
        NOW,
    );
    let _ = accounts::note_auth(&mut state, "m15-5", accounts::Auth::SignedOut, NOW);
    let _ = accounts::note_auth(&mut state, "m15-6", accounts::Auth::SignedOut, NOW);

    let (menu_rows, auto_rotate) = account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        Some("m15-2"),
        Some("m15-2@example.test"),
        SessionWeek {
            label: None,
            figure: None,
            left: Some((0, String::new())),
        },
        NOW,
    );
    assert!(auto_rotate);
    assert_eq!(
        menu_rows,
        vec![
            AccountMenuRow {
                account: AccountLabel::new("m15-2").unwrap(),
                label: "m15-2 \u{b7} dedicated \u{b7} 0% left (limited)".into(),
                active: true,
                enabled: true,
            },
            AccountMenuRow {
                account: AccountLabel::new("m15-3").unwrap(),
                label: "m15-3 \u{b7} dedicated \u{b7} 100% left (auto-login)".into(),
                active: false,
                enabled: true,
            },
            AccountMenuRow {
                account: AccountLabel::new("m15-4").unwrap(),
                label: "m15-4 \u{b7} dedicated \u{b7} not enrolled".into(),
                active: false,
                enabled: false,
            },
            AccountMenuRow {
                account: AccountLabel::new("m15-5").unwrap(),
                label: "m15-5 \u{b7} pool \u{b7} signed out (auto-login)".into(),
                active: false,
                enabled: true,
            },
            AccountMenuRow {
                account: AccountLabel::new("m15-6").unwrap(),
                label: "m15-6 \u{b7} pool \u{b7} signed out".into(),
                active: false,
                enabled: false,
            },
        ]
    );
}

/// A KIMI SESSION'S ACCOUNT MENU lists the rows of Kimi's own roster (`accounts-kimi.toml`),
/// never Claude Code's beside it, and an empty pool names `aterm harness accounts add --cli
/// kimi`. CONTROL: Claude Code's empty state is `No rotation accounts configured — run aterm
/// init`, as it always was, whether the facts name Claude Code or no CLI.
#[test]
fn a_kimi_sessions_account_menu_reads_its_own_pool() {
    use aterm_agent::harness::accounts::Cli;
    const NOW: i64 = 1_792_660_000;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-pool-menu-")
        .tempdir()
        .expect("tempdir");
    let root = dir.path();
    let (home, config, state) = (root.join("home"), root.join("config"), root.join("state"));
    for d in [&home, &config, &state] {
        std::fs::create_dir_all(d).unwrap();
    }
    let aterm_toml = config.join("aterm.toml");
    std::fs::write(&aterm_toml, "[fleet]\nmachine = \"m16\"\n").unwrap();
    std::fs::write(
        config.join("accounts.toml"),
        "[[account]]\nlabel = \"m16\"\nemail = \"claude-one@example.com\"\n",
    )
    .unwrap();
    let (none, _) =
        super::pool_menu_rows(Cli::Kimi, (&aterm_toml, &state, &home), (None, None), NOW);
    assert!(none.is_empty(), "no Kimi roster, no rows: {none:?}");
    let login = home.join(".aterm/logins/kimi/kimi-1");
    std::fs::write(
        config.join("accounts-kimi.toml"),
        format!(
            "[[account]]\nlabel = \"kimi-1\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"kimi-one@example.com\"\nregion = \"global\"\n",
            login.display()
        ),
    )
    .unwrap();
    let (listed, _) =
        super::pool_menu_rows(Cli::Kimi, (&aterm_toml, &state, &home), (None, None), NOW);
    let labels: Vec<&str> = listed.iter().map(|row| row.account.as_str()).collect();
    assert_eq!(labels, ["kimi-1"], "Kimi's roster alone: {listed:?}");

    let empty = FactMenuFacts {
        title: "Accounts \u{b7} tab 1".into(),
        cli: Some(Cli::Kimi),
        ..menu_facts(FactMask::default(), true)
    };
    let menu = shown(&rows(Fact::Account, &empty));
    assert!(
        menu.contains(&"note No Kimi Code CLI rotation accounts listed".to_string()),
        "{menu:?}"
    );
    assert!(
        menu.contains(&"note run aterm harness accounts add --cli kimi <label>".to_string()),
        "{menu:?}"
    );
    assert!(
        !menu
            .iter()
            .any(|row| row.contains("aterm init") || row.contains("Auto-rotate")),
        "{menu:?}"
    );
    let claude_empty = "note No rotation accounts configured \u{2014} run aterm init".to_string();
    for cli in [None, Some(Cli::Claude)] {
        let claude = FactMenuFacts {
            title: "Accounts \u{b7} tab 1".into(),
            cli,
            ..menu_facts(FactMask::default(), true)
        };
        assert!(
            shown(&rows(Fact::Account, &claude)).contains(&claude_empty),
            "CONTROL {cli:?}: Claude Code's empty state"
        );
    }
}

#[test]
fn select_menu_account_for_pins_enrolled_rows_and_dispatches_auto_login_per_cli_pool() {
    use aterm_agent::harness::accounts::{self, Cli};
    const NOW: i64 = 1_792_660_000;

    let tmp = aterm_tempfile::tempdir().unwrap();
    let (config, state, home) = (
        tmp.path().join("config"),
        tmp.path().join("state"),
        tmp.path().join("home"),
    );
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let login_1 = home.join(".aterm/logins/codex/codex-1");
    let login_2 = home.join(".aterm/logins/codex/codex-2");
    let login_3 = home.join(".aterm/logins/codex/codex-3");
    for d in [&login_1, &login_2, &login_3] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(login_1.join("auth.json"), "{}").unwrap();
    let roster_path = config.join("accounts-codex.toml");
    std::fs::write(
        &roster_path,
        format!(
            "[[account]]\nlabel = \"codex-1\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"codex-one@example.com\"\n\n\
             [[account]]\nlabel = \"codex-2\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"codex-two@example.com\"\n\n\
             [[account]]\nlabel = \"codex-3\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            login_1.display(),
            login_2.display(),
            login_3.display(),
        ),
    )
    .unwrap();
    let codex_store = accounts::Store::at_for(&state, Cli::Codex, &home);
    let claude_store = accounts::Store::at(&state);

    // 1. Enrolled vendor row pins immediately without dispatching a sign-in worker, and never
    // touches Claude Code's store.
    let mut dispatched = Vec::new();
    assert!(super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-1").unwrap(),
        NOW,
        true,
        |req, _| {
            dispatched.push(req);
            Ok(())
        },
    ));
    assert!(dispatched.is_empty());
    let (s1, _) = codex_store.load(NOW).unwrap();
    assert_eq!(s1.active.as_deref(), Some("codex-1"));
    assert!(s1.pinned);
    assert!(claude_store.load(NOW).unwrap().0.active.is_none());

    // 2. CONTROL: unenrolled vendor row under `Inert` upkeep (`upkeep_live = false`) cancels
    // `pending_pin` immediately and dispatches nothing.
    assert!(super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-2").unwrap(),
        NOW + 1,
        false,
        |req, _| {
            dispatched.push(req);
            Ok(())
        },
    ));
    assert!(dispatched.is_empty());
    assert!(codex_store.load(NOW + 1).unwrap().0.pending_pin.is_none());

    // 3. Unenrolled `(auto-login)` vendor row under `Live` upkeep saves `pending_pin` in Codex's
    // store and dispatches the exact `PinRequest`.
    assert!(super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-2").unwrap(),
        NOW + 2,
        true,
        |req, h| {
            assert_eq!(h, home);
            dispatched.push(req);
            Ok(())
        },
    ));
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].label(), "codex-2");
    assert_eq!(
        codex_store.load(NOW + 2).unwrap().0.pending_pin.as_ref(),
        Some(&dispatched[0])
    );

    // 4. CONTROL: dispatch failure cancels `pending_pin`; no-email row never saves one.
    assert!(super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-2").unwrap(),
        NOW + 3,
        true,
        |_, _| Err(()),
    ));
    assert!(codex_store.load(NOW + 3).unwrap().0.pending_pin.is_none());
    assert!(super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-3").unwrap(),
        NOW + 4,
        true,
        |req, _| {
            dispatched.push(req);
            Ok(())
        },
    ));
    assert_eq!(dispatched.len(), 1);
    assert!(codex_store.load(NOW + 4).unwrap().0.pending_pin.is_none());
}

/// THE ACCOUNT MENU'S WHY NEVER ASKS THE PERSON FOR THE WEEK (owner ruling R-12): no line under
/// the footer's reason names `/usage` or a command for the person to type; it says why the
/// harness has no figure.
#[test]
fn no_weekly_why_tells_the_person_to_open_usage() {
    use aterm_agent::harness::profile::WeeklyGap;
    use aterm_agent::harness::tap::Refusal;
    for gap in [
        WeeklyGap::TapOff,
        WeeklyGap::Unoffered(Refusal::Exe),
        WeeklyGap::Unoffered(Refusal::Home),
        WeeklyGap::Unoffered(Refusal::Login),
        WeeklyGap::Unoffered(Refusal::Line),
        WeeklyGap::Unoffered(Refusal::Argv),
        WeeklyGap::Unoffered(Refusal::Settings),
        WeeklyGap::Untapped,
        WeeklyGap::Unrestarted,
        WeeklyGap::NoReply,
        WeeklyGap::Stale,
    ] {
        let whole = weekly_gap_why(gap).join(" ");
        for asks in ["/usage", "open ", "aterm ctl"] {
            assert!(!whole.contains(asks), "{gap:?}: {whole:?} asks {asks:?}");
        }
    }
}

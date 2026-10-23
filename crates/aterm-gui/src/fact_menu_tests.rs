// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The facts' menus' rows, pure (`crate::fact_menu`): each fact's value, the
//! rows that act on it, its Hide, and a Show for every fact hidden; and the
//! usage's Traffic card, its bands, axis and ground. The menus on the glass
//! are `claude_lights::fact_menu_glass_tests`'.

use std::path::PathBuf;

use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_agent::harness::session_usage::TrafficView;

use super::{
    AccountLabel, AccountMenuRow, FactAct, FactMenuFacts, account_menu_rows_from, band_glyphs,
    band_level, rows, traffic_rows, weekly_meter_bar,
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

/// Each row as `controls fact-menu` keys it: its glyph and words, its key and
/// whether it is on; a row that is no choice by its kind and words.
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
            MenuRow::Note(t) => format!("note {t}"),
            MenuRow::Rule => "rule".into(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// THE MENUS' ROWS (design §10.6.14): each fact's value, the rows that act on it, its
/// own Hide; only the model's and the effort's type, and only Claude's own
/// commands. NEGATIVE CONTROL: with no side published the folder's rows are
/// off, and the branch offers no root.
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
    // Nothing else types: every other row's act is no `Type`.
    for fact in [Fact::Path, Fact::Branch, Fact::Usage] {
        assert!(
            !shown(&rows(fact, &on)).iter().any(|r| r.contains("/type/")),
            "{fact:?}"
        );
    }
    // CONTROL: no side.
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

/// THE TYPED ROWS FOLLOW WHAT THEY TYPE INTO: the key's row says off while
/// the engine's top rule shows the tag; a refusal's note stands over the
/// typed rows and disables them, as a press in flight does. NEGATIVE
/// CONTROL: no key row (an older build, a latch) offers no key at all, and
/// the slider stays.
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
    // The Hide row is no typed row: it stays on.
    assert!(effort[5].ends_with("[fact-menu/effort/hide/effort] true"));
    // CONTROL: no key row.
    let unkeyed = FactMenuFacts {
        key_row: None,
        ..menu_facts(FactMask::default(), true)
    };
    let effort = shown(&rows(Fact::Effort, &unkeyed));
    assert!(!effort.iter().any(|r| r.contains(key)));
    assert!(effort[2].ends_with("[fact-menu/effort/type/effort] true"));
}

/// EVERY MENU SHOWS WHAT IS HIDDEN: a fact's own Hide turns to Show while it
/// is hidden, and a Show for every other fact hidden in the session follows
/// it — so a stub the rule gave up for room is still shown again.
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

/// 14:02 UTC on a day of 2024: the first bucket's start.
const T0: i64 = 86_400 * 19_800 + 14 * 3600 + 2 * 60;

/// Four minutes of traffic, a minute a bucket: a thousand in, then none, then
/// four thousand (the peak, while Claude was working), then two thousand.
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

/// Each row's text, a graph row's cells as they read.
fn texts(rows: &[MenuRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            MenuRow::Plot(cells) => cells.iter().map(|c| c.ch).collect(),
            MenuRow::Note(t) => format!("note {t}"),
            other => format!("{other:?}"),
        })
        .collect()
}

/// THE TRAFFIC CARD (owner: "the traffic shows a graph over time"): the in
/// band and the out band, two rows each, labelled; the axis from the first
/// bucket's local time through the span to `now`; the working time and
/// totals; the peaks per column's slice. NEGATIVE CONTROL: an empty series
/// is `No traffic yet` and no graph.
#[test]
fn the_traffic_card_paints_two_bands_and_its_axis() {
    // 30 content cells: a 6-cell label and 24 columns of 10 s each.
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
    // Each band in its own ink, the axis in the secondary.
    let ink = |row: usize| match &rows[row] {
        MenuRow::Plot(cells) => cells[10].ink,
        _ => unreachable!("a graph row"),
    };
    assert_eq!(
        (ink(0), ink(2), ink(4)),
        (PlotInk::In, PlotInk::Out, PlotInk::Axis)
    );
    // The minute with no traffic is blank in both bands.
    for (row, line) in text.iter().enumerate().take(4) {
        assert_eq!(line.chars().nth(6 + 7), Some(' '), "row {row}");
    }
    // A pending row the graph could not place is said.
    let unread = TrafficView {
        unread: 3,
        ..traffic()
    };
    assert_eq!(
        texts(&traffic_rows(&unread, T0 + 240, Some(0), 30)).last(),
        Some(&"note +3 unread not graphed".to_string())
    );
    // CONTROL: no traffic.
    assert_eq!(
        texts(&traffic_rows(&TrafficView::default(), T0, Some(0), 30)),
        ["note No traffic yet"]
    );
}

/// A BAND'S PEAK FILLS BOTH ITS ROWS AND A TRICKLE IS AT LEAST AN EIGHTH: the
/// level is linear in the band's own peak over sixteen eighths, the bottom
/// row filling first; a token is never drawn as nothing. CONTROL: none is
/// blank.
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
    // On the card: the peak minute's columns are full in both rows of the in
    // band, the first minute's a quarter of the way up.
    let text = texts(&traffic_rows(&traffic(), T0 + 240, Some(0), 30));
    let at = |row: usize, col: usize| text[row].chars().nth(6 + col);
    assert_eq!((at(0, 12), at(1, 12)), (Some('\u{2588}'), Some('\u{2588}')));
    assert_eq!((at(0, 0), at(1, 0)), (Some(' '), Some('\u{2584}')));
}

/// THE COLUMNS CLAUDE WAS WORKING IN TAKE THE SHADED GROUND: the card paints
/// a busy column on the card's ground a tenth of the way to its accent, in
/// the band's ink. CONTROL: an idle column stands on the card's ground.
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
    // The in band's bottom row: the card's second row, under the header.
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
    // CONTROL: the first minute's column was idle.
    assert_eq!(row[lead].bg, c.card_bg);
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

    // The click carries the displayed identity across a roster reload. It
    // must never clear another account's walls because file order changed.
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
    // Enrolled facts only: this aggregate never examines login directories.
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
        Some(0),
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

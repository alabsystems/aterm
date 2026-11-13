// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the in-grid card's model, geometry, hit testing, paint, and accessibility, hosted
//! on a fact's menu (the folder's).

use std::time::Instant;

use aterm_agent::harness::footer::{FactKind, FactMask, FooterFacts};

use super::*;
use crate::{App, WindowId, term_lock};

#[path = "autopilot_menu_viewport_tests.rs"]
mod viewport_tests;

fn keys(rows: &[MenuRow]) -> Vec<Option<String>> {
    rows.iter()
        .map(|r| match r {
            MenuRow::Item { act, .. } => Some(act.key()),
            _ => None,
        })
        .collect()
}

fn choice(glyph: &'static str, label: &str, act: FactAct, enabled: bool, checked: bool) -> MenuRow {
    MenuRow::Item {
        glyph,
        label: label.to_string(),
        spoken: label.to_string(),
        act: MenuAct::Fact(Fact::Path, act),
        enabled,
        checked,
        trail: String::new(),
    }
}

/// The longest label of [`card`]'s choices, as the card lays it.
const LONGEST: &str = "+  New tab at the repository root";

/// A card of every row kind the engine lays: a header, three choices (the first checked), a
/// rule, three more, a rule and a last choice that is always live. With `live` off a note
/// stands under the header and every choice but the last is disabled.
fn card_titled(title: &str, name: Option<&str>, room: usize, live: bool) -> Vec<MenuRow> {
    let mut out = header_rows(title, name, room);
    if !live {
        out.push(MenuRow::note("no working directory"));
    }
    out.extend([
        choice("+", "New tab here", FactAct::NewTabHere, live, live),
        choice("\u{29c9}", "Copy path", FactAct::CopyPath, live, false),
        choice(
            "\u{2387}",
            "Copy branch name",
            FactAct::CopyBranch,
            live,
            false,
        ),
        MenuRow::Rule,
        choice(
            "+",
            "New tab at the repository root",
            FactAct::NewTabAtRoot,
            live,
            false,
        ),
        choice(
            "\u{25cb}",
            "Hide folder",
            FactAct::Hide(FactKind::Path),
            live,
            false,
        ),
        choice(
            "\u{25cf}",
            "Show branch",
            FactAct::Show(FactKind::Branch),
            live,
            false,
        ),
        MenuRow::Rule,
        choice(
            "\u{2386}",
            "Account settings\u{2026}",
            FactAct::AccountSettings,
            true,
            false,
        ),
    ]);
    out
}

fn card(live: bool) -> Vec<MenuRow> {
    card_titled(
        "Folder \u{b7} tab 3",
        Some("fix the footer"),
        MAX_CONTENT,
        live,
    )
}

#[test]
fn rows_built_again_keep_their_places_and_a_row_that_left_leaves_a_gap() {
    let fresh = card(true);
    let mut shown = fresh.clone();
    let leaving = 8;
    shown.insert(
        leaving,
        choice(
            "\u{21ba}",
            "Refresh usage",
            FactAct::RefreshUsage,
            true,
            false,
        ),
    );
    let laid = keep_places(&shown, &fresh).expect("only a row left");
    assert_eq!(laid.len(), shown.len());
    assert_eq!(
        laid[leaving],
        MenuRow::Gap(super::row_width(&shown[leaving]))
    );
    for (i, (was, now)) in keys(&shown).iter().zip(keys(&laid)).enumerate() {
        if i != leaving {
            assert_eq!(*was, now, "row {i} kept its place");
        }
    }
    assert!(!laid[leaving].selectable());
    assert_eq!(super::content_width(&laid), super::content_width(&shown));
    assert_eq!(
        place(&laid, (30, 4), (0..40, 0..120), (120, 40), 1),
        place(&shown, (30, 4), (0..40, 0..120), (120, 40), 1)
    );
    assert!(
        (0..laid.len()).all(|from| step(&laid, Some(from), true) != Some(leaving)),
        "the keys skip the gap"
    );
    assert_eq!(keep_places(&shown, &shown).as_deref(), Some(&shown[..]));
    assert_eq!(keep_places(&laid, &fresh).as_deref(), Some(&laid[..]));
    assert_eq!(keep_places(&fresh, &shown), None, "a row arrived");
    assert_eq!(keep_places(&laid, &shown), None, "a gap is no place");
}

#[test]
fn the_card_hangs_above_its_anchor_and_opens_down_only_where_fewer_rows_are_above() {
    let rows = card(true);
    let h = rows.len() + 2;
    let rect = place(&rows, (30, 10), (0..40, 0..120), (120, 40), 1).unwrap();
    assert_eq!(
        (rect.row, rect.col, rect.h),
        (30 - h, 10, h),
        "above, at the anchor"
    );
    assert!(rect.w >= LONGEST.chars().count());
    let right = place(&rows, (30, 115), (0..40, 0..120), (120, 40), 1).unwrap();
    assert_eq!(right.col + right.w, 120, "shifted left, whole");
    let low = place(&rows, (3, 10), (0..40, 0..120), (120, 40), 1).unwrap();
    assert_eq!(
        (low.row, low.h),
        (4, h),
        "fewer rows above: below the anchor"
    );
    let middle = place(&rows, (10, 0), (0..20, 0..120), (120, 20), 1).unwrap();
    assert_eq!(
        (middle.row, middle.h),
        (1, h),
        "neither side holds it: whole, over the anchor's row"
    );
    let short = place(&rows, (6, 0), (0..10, 0..120), (120, 10), 1).unwrap();
    assert_eq!(
        (short.row, short.h),
        (1, 9),
        "shorter than the card: a prefix"
    );
    assert_eq!(
        row_at(short, MenuViewport::default(), rows.len(), 1),
        None,
        "the top border"
    );
    assert_eq!(
        row_at(short, MenuViewport::default(), rows.len(), 2),
        Some(0)
    );
    assert_eq!(
        row_at(short, MenuViewport::default(), rows.len(), 8),
        Some(6)
    );
    assert_eq!(
        row_at(short, MenuViewport::default(), rows.len(), 9),
        None,
        "the bottom border"
    );
    let ruled = place(&rows, (6, 0), (0..8, 0..120), (120, 8), 1).unwrap();
    assert_eq!(
        (ruled.row, ruled.h),
        (1, 6),
        "the first choices, not the rule under them"
    );
    assert_eq!(
        place(&rows, (30, 0), (0..40, 0..3), (3, 40), 1),
        None,
        "narrower than a border pair"
    );
    assert_eq!(
        place(&rows, (1, 0), (0..3, 0..120), (120, 3), 1),
        None,
        "no room either side"
    );
    assert_eq!(place(&[], (30, 0), (0..40, 0..120), (120, 40), 1), None);
}

#[test]
fn the_card_keeps_to_its_panes_rows_and_gives_up_its_padding_first() {
    let rows = card(true);
    let want = rows.len() + 2;
    let top = place(&rows, (10, 4), (1..12, 0..100), (100, 30), 1).unwrap();
    assert_eq!((top.row, top.h), (1, 10), "its pane's rows, a prefix");
    let frame = place(&rows, (10, 4), (0..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!(frame.row, 11, "control: the frame alone opens it downward");
    let bottom = place(&rows, (24, 4), (17..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!((bottom.row, bottom.h), (17, want), "whole in its pane");
    let frame = place(&rows, (24, 4), (0..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!(
        frame.row,
        24 - want,
        "control: the frame alone, over the pane above"
    );
    let low = place(&rows, (3, 4), (1..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!((low.row, low.h), (4, want), "below, whole");
    let lower = place(&rows, (5, 4), (1..16, 0..100), (100, 30), 1).unwrap();
    assert_eq!(
        (lower.row, lower.h),
        (16 - want, want),
        "over the anchor, from below"
    );
    assert!(
        lower.row <= 5 && 5 < lower.row + lower.h,
        "{lower:?} covers the anchor"
    );
    let lent = place(&rows, (28, 4), (25..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!((lent.row, lent.h), (28 - want, want), "lent the frame");
    // A title of 29 cells: as wide as a pane of 31 leaves it, border to border.
    const TITLE: &str = "Workspace \u{b7} tab 1, right pane";
    assert_eq!(display_cells(TITLE), 29);
    let named = |cols: usize| {
        card_titled(
            TITLE,
            Some("\u{2733} Claude Code"),
            content_room(cols),
            true,
        )
    };
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let text = |r: &[RenderCell]| r.iter().map(|c| c.ch).collect::<String>();
    let tight = named(31);
    let rect = place(&tight, (28, 40), (1..30, 33..64), (64, 30), 1).unwrap();
    assert_eq!((rect.col, rect.w), (33, 31), "as wide as its pane");
    let painted: Vec<String> = paint(rect, &tight, MenuViewport::default(), None, theme, None)
        .iter()
        .map(|r| text(r))
        .collect();
    assert_eq!(painted[1], format!("\u{2502}{TITLE}\u{2502}"));
    assert!(
        painted.iter().any(|row| row.contains("Copy branch name")),
        "{painted:#?}"
    );
    let tighter = named(29);
    assert_eq!(
        tighter[..3],
        [
            MenuRow::Header("Workspace \u{b7} tab 1,".to_string()),
            MenuRow::Header("right pane".to_string()),
            MenuRow::Name("\u{2733} Claude Code".to_string()),
        ]
    );
    let wide = named(100);
    assert_eq!(wide[0], MenuRow::Header(TITLE.to_string()));
    let rect = place(&wide, (28, 40), (1..30, 0..100), (100, 30), 1).unwrap();
    let painted = paint(rect, &wide, MenuViewport::default(), None, theme, None);
    assert_eq!(text(&painted[1]).chars().nth(1), Some(' '), "padded");
}

#[test]
fn the_hit_test_and_the_keys_land_only_on_live_choices() {
    let rows = card(true);
    let rect = place(&rows, (30, 10), (0..40, 0..120), (120, 40), 1).unwrap();
    let at = |i: usize| {
        item_at(
            rect,
            &rows,
            MenuViewport::default(),
            rect.row + 1 + i,
            rect.col + 3,
        )
    };
    assert_eq!(at(0), None, "the header");
    assert_eq!(at(1), Some(1), "the first choice");
    assert_eq!(at(4), None, "a rule");
    assert_eq!(
        item_at(rect, &rows, MenuViewport::default(), rect.row, rect.col + 3),
        None,
        "a border"
    );
    assert_eq!(
        item_at(
            rect,
            &rows,
            MenuViewport::default(),
            rect.row + 2,
            rect.col + rect.w
        ),
        None,
        "off the card"
    );
    let n = rows.len();
    assert_eq!(step(&rows, None, true), Some(1));
    assert_eq!(step(&rows, Some(3), true), Some(5), "over the rule");
    assert_eq!(step(&rows, Some(n - 1), true), Some(1), "wraps");
    assert_eq!(step(&rows, Some(1), false), Some(n - 1), "wraps back");
    let off = card(false);
    let last = off.len() - 1;
    assert_eq!(
        step(&off, None, true),
        Some(last),
        "only the last choice is live"
    );
    assert_eq!(step(&off, Some(last), true), Some(last));
    use aterm_types::keyboard::{Key, NamedKey as Nk};
    for (key, want) in [
        (Key::Named(Nk::ArrowUp), MenuKey::Up),
        (Key::Named(Nk::ArrowDown), MenuKey::Down),
        (Key::Named(Nk::ArrowLeft), MenuKey::Left),
        (Key::Named(Nk::ArrowRight), MenuKey::Right),
        (Key::Named(Nk::Enter), MenuKey::Choose),
        (Key::Named(Nk::Space), MenuKey::Choose),
        (Key::Character(' '), MenuKey::Choose),
        (Key::Named(Nk::Escape), MenuKey::Close),
        (Key::Named(Nk::ShiftLeft), MenuKey::Inert),
        (Key::Character('a'), MenuKey::PassThrough),
        (Key::Named(Nk::Tab), MenuKey::PassThrough),
    ] {
        assert_eq!(key_of(&key), want, "{key:?}");
    }
}

#[test]
fn the_card_paints_its_rect_its_check_and_its_disabled_rows() {
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let rows = card(true);
    let rect = place(&rows, (30, 4), (0..40, 0..120), (120, 40), 1).unwrap();
    let painted = paint(rect, &rows, MenuViewport::default(), Some(2), theme, None);
    assert_eq!(painted.len(), rect.h);
    assert!(painted.iter().all(|r| r.len() == rect.w));
    let text = |r: &[RenderCell]| r.iter().map(|c| c.ch).collect::<String>();
    assert!(
        text(&painted[2]).contains("New tab here"),
        "{}",
        text(&painted[2])
    );
    assert_eq!(
        painted[2][rect.w - 3].ch,
        '\u{25cf}',
        "the check, flush right"
    );
    assert_ne!(painted[3][3].bg, painted[2][3].bg, "the highlight's ground");
    let off = card(false);
    let rect = place(&off, (30, 4), (0..40, 0..120), (120, 40), 1).unwrap();
    let painted = paint(rect, &off, MenuViewport::default(), None, theme, None);
    let first = 1 + 2;
    let last = off.len();
    assert_ne!(
        painted[first][3].fg, painted[last][3].fg,
        "a disabled row reads as one"
    );
}

fn stop(label: &'static str, enabled: bool) -> Stop {
    Stop {
        label: label.to_string(),
        act: MenuAct::Fact(Fact::Effort, FactAct::SetEffort(label)),
        enabled,
    }
}

/// A card of a note, a slider of `medium`, `high` and `xhigh` with its thumb on `at`, a rule
/// and one choice.
fn slider_card(at: Option<usize>, xhigh: bool) -> Vec<MenuRow> {
    vec![
        MenuRow::Header("Effort".to_string()),
        MenuRow::note("effort high"),
        MenuRow::stops(
            "Effort",
            [
                stop("medium", true),
                stop("high", true),
                stop("xhigh", xhigh),
            ],
            at,
        ),
        MenuRow::Rule,
        choice("", "Hide effort", FactAct::CopyPath, true, false),
    ]
}

/// A SLIDER'S STOPS ARE LAID ONCE FOR THE PAINTER AND THE POINTER: the first stop at the row's
/// left end and the last at its right, a press snapping to the stop whose cells hold it or the
/// nearer of the two it lies between, and the keys stepping to the next stop that can be
/// picked without wrapping at an end.
#[test]
fn a_sliders_stops_span_its_row_and_a_cell_snaps_to_the_nearer() {
    let stops = [
        stop("medium", true),
        stop("high", true),
        stop("xhigh", true),
    ];
    // `○ medium` 8 cells, `○ high` 6, `○ xhigh` 7: 21 of the 31, so two gaps of five.
    let spans = stop_spans(&stops, 31);
    assert_eq!(spans, [0..8, 13..19, 24..31]);
    assert_eq!(
        stop_spans(&stops, 32),
        [0..8, 14..20, 25..32],
        "the odd cell goes left"
    );
    let tight = stop_spans(&stops, 12);
    assert_eq!(
        tight,
        [0..8, 9..12, 12..12],
        "a narrow row keeps a cell between"
    );
    for (cell, want) in [
        (0, 0),
        (7, 0),
        (10, 0),
        (11, 1),
        (13, 1),
        (18, 1),
        (21, 1),
        (22, 2),
        (30, 2),
        (90, 2),
    ] {
        assert_eq!(stop_near(&spans, cell), Some(want), "cell {cell}");
    }
    assert_eq!(stop_near(&[], 3), None);
    assert_eq!(stop_step(&stops, Some(0), true), Some(1));
    assert_eq!(
        stop_step(&stops, Some(2), true),
        None,
        "the high end does not wrap"
    );
    assert_eq!(stop_step(&stops, Some(0), false), None, "nor the low end");
    assert_eq!(
        stop_step(&stops, None, true),
        Some(0),
        "from no stop, the low end"
    );
    assert_eq!(stop_step(&stops, None, false), Some(2));
    let gated = [
        stop("medium", true),
        stop("high", false),
        stop("xhigh", true),
    ];
    assert_eq!(
        stop_step(&gated, Some(0), true),
        Some(2),
        "over a stop not offered"
    );
    assert_eq!(stop_step(&gated, Some(2), false), Some(0));

    let rows = slider_card(Some(1), true);
    let rect = place(&rows, (30, 10), (0..40, 0..120), (120, 40), 1).unwrap();
    let (text_start, text_end) = super::text_span(rect.w, &rows);
    let spans = stop_spans(&stops, text_end - text_start);
    let slider = rect.row + 1 + 2;
    let hit = |row, col| hit_at(rect, &rows, MenuViewport::default(), row, col);
    for (j, span) in spans.iter().enumerate() {
        assert_eq!(
            hit(slider, rect.col + text_start + span.start),
            Some((2, Some(j))),
            "stop {j}'s mark"
        );
        assert_eq!(
            hit(slider, rect.col + text_start + span.end - 1),
            Some((2, Some(j)))
        );
    }
    assert_eq!(
        hit(slider, rect.col),
        Some((2, Some(0))),
        "the border snaps to the end"
    );
    assert_eq!(
        hit(slider + 2, rect.col + 3),
        Some((4, None)),
        "a choice has no stop"
    );
    assert_eq!(hit(slider - 1, rect.col + 3), None, "the note");
    assert_eq!(hit(slider + 1, rect.col + 3), None, "the rule");
    let none = slider_card(Some(0), false);
    let off: Vec<MenuRow> = none
        .iter()
        .map(|row| match row {
            MenuRow::Stops {
                name, stops, at, ..
            } => MenuRow::stops(
                name.clone(),
                stops
                    .iter()
                    .map(|s| Stop {
                        enabled: false,
                        ..s.clone()
                    })
                    .collect::<Vec<_>>(),
                *at,
            ),
            row => row.clone(),
        })
        .collect();
    assert!(none[2].selectable(), "one stop to pick is a row to choose");
    assert!(!off[2].selectable(), "none is not");
    assert_eq!(
        step(&off, None, true),
        Some(4),
        "the keys pass a slider with no stop"
    );
}

/// THE SLIDER IS DRAWN AS ITS STOPS: a filled mark and bold word where the thumb stands, a
/// hollow mark at every other stop, the track between them, the rested stop alone on the
/// highlight's ground and a stop not offered in the disabled ink. Control: with the row not
/// selected nothing stands on the highlight's ground.
#[test]
fn the_slider_paints_its_thumb_its_track_and_its_rested_stop() {
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let mut rows = slider_card(Some(1), false);
    let rect = place(&rows, (30, 10), (0..40, 0..120), (120, 40), 1).unwrap();
    let (text_start, text_end) = super::text_span(rect.w, &rows);
    let MenuRow::Stops { stops, focus, .. } = &mut rows[2] else {
        panic!("the slider");
    };
    *focus = Some(0);
    let spans = stop_spans(stops, text_end - text_start);
    let painted = paint(rect, &rows, MenuViewport::default(), Some(2), theme, None);
    let line = &painted[1 + 2];
    assert_eq!(line.len(), rect.w);
    let at = |j: usize| &line[text_start + spans[j].start];
    assert_eq!(
        (at(0).ch, at(1).ch, at(2).ch),
        ('\u{25cb}', '\u{25cf}', '\u{25cb}'),
        "the thumb is the filled mark"
    );
    let word = |j: usize| -> String {
        spans[j]
            .clone()
            .map(|col| line[text_start + col].ch)
            .collect()
    };
    assert_eq!(
        [word(0), word(1), word(2)],
        ["\u{25cb} medium", "\u{25cf} high", "\u{25cb} xhigh"]
    );
    assert!(at(1).bold && !at(0).bold, "the thumb's word is bold");
    let card_bg = line[text_start + spans[0].end].bg;
    assert_ne!(at(0).bg, card_bg, "the rested stop stands on the highlight");
    assert_eq!(at(1).bg, card_bg, "the thumb's own stop does not");
    assert_ne!(at(2).fg, at(1).fg, "a stop not offered reads as one");
    let track: String = (spans[0].end..spans[1].start)
        .map(|col| line[text_start + col].ch)
        .collect();
    assert_eq!(
        track
            .trim_matches(' ')
            .chars()
            .collect::<std::collections::BTreeSet<_>>(),
        ['\u{2500}'].into(),
        "the track between two stops: {track:?}"
    );
    let unlit = paint(rect, &rows, MenuViewport::default(), Some(4), theme, None);
    assert!(
        unlit[1 + 2][1..rect.w - 1].iter().all(|c| c.bg == card_bg),
        "control: no rest is drawn on a row the highlight left"
    );
}

/// WHERE THE KEYS REST ON A SLIDER IS THE CARD'S, NOT THE ROW'S IDENTITY: a row whose rest
/// moved is the same row to a reader (its `Hash`), the same choice and the same place, so
/// rebuilt rows keep the highlight and their layout; a thumb that moved is the same place and
/// a new reader identity.
#[test]
fn a_sliders_rest_is_no_part_of_what_the_row_is() {
    use std::hash::{Hash, Hasher};
    let id = |row: &MenuRow| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        row.hash(&mut h);
        h.finish()
    };
    let shown = slider_card(Some(1), true);
    let mut rested = shown.clone();
    let MenuRow::Stops { focus, .. } = &mut rested[2] else {
        panic!("the slider");
    };
    *focus = Some(2);
    assert_eq!(
        id(&shown[2]),
        id(&rested[2]),
        "a rest moves no reader identity"
    );
    assert!(super::same_choice(&shown[2], &rested[2]));
    assert_eq!(rested[2].rested_stop(), Some(2));
    assert_eq!(
        shown[2].rested_stop(),
        Some(1),
        "with no rest, the thumb's stop"
    );
    let moved = slider_card(Some(2), true);
    assert_ne!(id(&shown[2]), id(&moved[2]), "the thumb's stop is said");
    assert!(
        super::same_choice(&shown[2], &moved[2]),
        "and is the same slider"
    );
    assert_eq!(
        keep_places(&shown, &moved).as_deref(),
        Some(&moved[..]),
        "a thumb that moved moves no row"
    );
    assert_eq!(super::row_width(&shown[2]), super::row_width(&moved[2]));
    let gated = slider_card(Some(1), false);
    assert!(
        !super::same_choice(&shown[2], &gated[2]),
        "a slider whose stops changed is another choice"
    );
    assert_eq!(slider_card(None, true)[2].rested_stop(), None);
}

/// The fact the engine's window tests hang their card from: the model's, in a session that is
/// no Claude Code, whose card hides and shows facts and types nothing.
const FACT: Fact = Fact::Model;
/// The facts hidden before those tests open the model's card, so it is three choices a test
/// can tell apart by the session's mask alone, one row after another.
const BASE: FactMask = FactMask::PATH.with(FactKind::Branch, true);
const HIDE: &str = "fact-menu/model/hide/model";
const SHOW_FOLDER: &str = "fact-menu/model/show/path";
const SHOW_BRANCH: &str = "fact-menu/model/show/branch";

/// The session's mask once the choice `key` of the card is taken from [`BASE`].
fn taken(key: &str) -> FactMask {
    match key {
        HIDE => BASE.with(FactKind::Model, true),
        SHOW_FOLDER => BASE.with(FactKind::Path, false),
        SHOW_BRANCH => BASE.with(FactKind::Branch, false),
        other => panic!("{other} moves no mask"),
    }
}

/// The mask row `i` of `rows` leaves the session with, taken from [`BASE`]; `None` for a row
/// that is no hide or show choice.
fn mask_of(rows: &[MenuRow], i: usize) -> Option<FactMask> {
    match rows.get(i)? {
        MenuRow::Item {
            act: MenuAct::Fact(_, FactAct::Hide(kind)),
            enabled: true,
            ..
        } => Some(BASE.with(*kind, true)),
        MenuRow::Item {
            act: MenuAct::Fact(_, FactAct::Show(kind)),
            enabled: true,
            ..
        } => Some(BASE.with(*kind, false)),
        _ => None,
    }
}

fn publish(app: &App, session: u64) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap();
    timeline.note_foreground_group(4242);
    assert!(timeline.set_claude_footer(
        4242,
        None,
        Some(FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        }),
    ));
    timeline.set_claude_side(
        4242,
        Some(std::sync::Arc::new(crate::claude_footer::FactSide::at(
            Some("/Users//someone/aterm".into()),
            Some("/Users//someone".into()),
        ))),
    );
}

fn claude_app() -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.apply_term_resize(wid, 24, 120));
    let session = app.focused_session_id(wid).expect("the front session");
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    publish(&app, session);
    app.set_fact_mask(session, BASE);
    draw_composer(&app, session);
    (app, wid, session, host)
}

fn feed(app: &App, session: u64, bytes: &[u8]) {
    let term = app.pool.get(session).expect("session").term.clone();
    term_lock(&term).process(bytes);
}

fn draw_composer(app: &App, session: u64) {
    let rule = "\u{2500}".repeat(120);
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
         \x1b[23;1H{rule}\x1b[24;1H  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)\x1b[22;3H"
    );
    feed(app, session, screen.as_bytes());
}

fn frame(app: &mut App, wid: WindowId) {
    frame_with(app, wid, |_| {});
}

fn frame_with(app: &mut App, wid: WindowId, before_menu: impl FnOnce(&mut App)) {
    if let Some((plan, crate::VisibleContentRoute::Heterogeneous)) =
        app.active_visible_frame_layout(wid)
    {
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        app.splice_claude_footer(wid, &plan, crate::VisibleContentRoute::Heterogeneous);
        before_menu(app);
        app.splice_autopilot_menu(wid);
        return;
    }
    let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(Instant::now()),
    );
    let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
        panic!("the headless capture must produce a frame");
    };
    app.splice_tab_strip(wid);
    app.splice_claude_footer(
        wid,
        &plan,
        crate::VisibleContentRoute::Terminal {
            composed: grid.composed,
        },
    );
    before_menu(app);
    app.splice_autopilot_menu(wid);
}

fn row_text(app: &App, wid: WindowId, row: usize) -> String {
    app.windows[&wid].input_scratch.cells[row]
        .iter()
        .map(|c| c.ch)
        .collect()
}

/// The window pixel at the centre of frame cell `(row, col)`.
fn cell_centre(app: &App, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    let (cw, ch) = (cw.max(1), ch.max(1));
    let pad = app.win_pad(wid);
    let top = app.win_pad_top(wid) + app.win_head(wid);
    let (ox, oy) = app.frame_origin(wid);
    (
        ox as f64 + (pad + col * cw) as f64 + cw as f64 / 2.0,
        oy as f64 + (top + row * ch) as f64 + ch as f64 / 2.0,
    )
}

/// Open the folder's card as a key does: its first choice lit.
fn open_card(app: &mut App) {
    app.open_front_fact_menu(FACT)
        .expect("the folder is on the glass");
}

/// Open the folder's card as a pointer does, hung over the pane's left where the facts' own
/// column (against the pane's right edge) leaves no room beside the card: no choice lit.
fn open_card_at_the_left(app: &mut App, wid: WindowId, session: u64, keyboard: bool) {
    let (row, _) = app
        .fact_anchor(wid, session, FACT)
        .expect("the folder is on the glass");
    app.open_fact_menu(wid, session, FACT, (row, 2), keyboard);
}

/// Open the folder's card as a pointer does: no choice lit.
fn open_card_by_pointer(app: &mut App, wid: WindowId, session: u64) {
    let anchor = app
        .fact_anchor(wid, session, FACT)
        .expect("the folder is on the glass");
    app.open_fact_menu(wid, session, FACT, anchor, false);
}

/// The open card's rows built again as `edit` leaves them: what a session's facts moving under
/// an open card does ([`AutopilotMenu::refresh_rows`]).
fn relay(app: &mut App, wid: WindowId, edit: impl FnOnce(&mut Vec<MenuRow>)) {
    let menu = app.autopilot_menu.as_mut().expect("open");
    let mut fresh: Vec<MenuRow> = menu
        .rows
        .iter()
        .filter(|row| !matches!(row, MenuRow::Gap(_)))
        .cloned()
        .collect();
    edit(&mut fresh);
    let spoken = menu.spoken.clone();
    assert!(menu.refresh_rows(fresh, spoken), "the rows moved");
    app.request_menu_frame(wid);
}

fn row_of(app: &App, key: &str) -> Option<usize> {
    app.autopilot_menu.as_ref().and_then(|menu| {
        menu.rows
            .iter()
            .position(|r| matches!(r, MenuRow::Item { act, .. } if act.key() == key))
    })
}

struct MenuTestHost(crate::harness_host::HostHandle);

impl Drop for MenuTestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

#[test]
fn the_card_opens_from_its_fact_steps_and_closes_with_it() {
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    assert_eq!(app.fact_menu_controls(), ["fact-menu open=false"]);
    assert_eq!(
        super::fingerprint(app.autopilot_menu.as_ref(), wid),
        0,
        "0 at rest"
    );
    open_card(&mut app);
    let anchor = app.autopilot_menu.as_ref().unwrap().anchor;
    let open_fp = super::fingerprint(app.autopilot_menu.as_ref(), wid);
    assert_ne!(open_fp, 0, "the open repaints");
    frame(&mut app, wid);
    let rect = app
        .autopilot_menu
        .as_ref()
        .and_then(|m| m.rect)
        .expect("painted");
    assert_eq!(rect.row + rect.h, anchor.0, "hung above the fact");
    assert!(
        (rect.col..rect.col + rect.w).contains(&anchor.1),
        "over the fact's column, kept inside the pane: {rect:?}, {anchor:?}"
    );
    assert!(row_text(&app, wid, rect.row + 2).contains("Opus 5.5"));
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_some(), "the fact is there: it stays");
    let lines = app.fact_menu_controls();
    assert!(
        lines[0].starts_with("fact-menu open=true session=s-"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l
            .contains("key=fact-menu/model/hide/model enabled=true checked=false selected=true")),
        "{lines:?}"
    );
    let hide = row_of(&app, HIDE).expect("the hide row");
    while app.autopilot_menu.as_ref().unwrap().highlight != Some(hide + 1) {
        assert!(app.autopilot_menu_nav(wid, MenuKey::Down));
    }
    assert_ne!(
        super::fingerprint(app.autopilot_menu.as_ref(), wid),
        open_fp,
        "a step repaints"
    );
    assert!(app.autopilot_menu_nav(wid, MenuKey::Up));
    assert!(app.autopilot_menu_nav(wid, MenuKey::Choose));
    assert!(app.autopilot_menu.is_none(), "chosen and closed");
    assert_eq!(app.fact_mask(session), taken(HIDE), "Hide model");
    app.set_fact_mask(session, BASE);

    frame(&mut app, wid);
    open_card_by_pointer(&mut app, wid, session);
    assert_eq!(
        app.autopilot_menu.as_ref().unwrap().highlight,
        None,
        "a pointer lights none"
    );
    assert!(app.autopilot_menu_nav(wid, MenuKey::Close));
    assert!(app.autopilot_menu.is_none());
    assert!(!app.autopilot_menu_nav(wid, MenuKey::Close), "nothing open");

    open_card(&mut app);
    assert!(!app.autopilot_menu_nav(wid, MenuKey::PassThrough));
    assert!(app.autopilot_menu.is_none());

    open_card(&mut app);
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_some(), "the same focus: it stays");
    app.autopilot_menu.as_mut().unwrap().focus = Some(session + 1);
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_none(), "the focus moved: closed");

    open_card(&mut app);
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_some());
    feed(
        &app,
        session,
        "\x1b[2J\x1b[18;1H Bash command\x1b[20;1H Do you want to proceed?\x1b[21;1H \u{276F} 1. Yes\x1b[22;1H   2. No"
            .as_bytes(),
    );
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_none(), "closed with its fact");
    assert_eq!(super::fingerprint(app.autopilot_menu.as_ref(), wid), 0);
}

#[test]
fn a_controller_focus_out_closes_the_card_as_the_os_edge_does() {
    let (mut app, wid, _session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    open_card(&mut app);
    app.on_focus(wid, true);
    assert!(app.autopilot_menu.is_some(), "the same focus: it stays");
    app.on_focus(wid, false);
    assert!(app.autopilot_menu.is_none(), "focus out: closed");
}

#[test]
fn a_controllers_keys_step_and_choose_on_the_open_card() {
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    open_card(&mut app);
    frame(&mut app, wid);
    let key = |name: aterm_types::keyboard::NamedKey| crate::input::InputEvent::Key {
        key: aterm_types::keyboard::Key::Named(name),
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    };
    let hide = row_of(&app, HIDE).expect("the hide row");
    while app.autopilot_menu.as_ref().unwrap().highlight != Some(hide) {
        assert!(
            app.autopilot_menu_input_event(wid, &key(aterm_types::keyboard::NamedKey::ArrowDown))
        );
    }
    assert!(app.autopilot_menu_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter)));
    assert!(app.autopilot_menu.is_none());
    assert_eq!(app.fact_mask(session), taken(HIDE), "Hide model");
}

#[test]
fn the_cards_name_drops_the_agents_spinner_frame() {
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    for (title, name) in [
        (
            "\u{2826} Add tested calculator functions",
            "Add tested calculator functions",
        ),
        (
            "\u{2733} Calc.py small tested module",
            "\u{2733} Calc.py small tested module",
        ),
    ] {
        app.pool
            .get(session)
            .expect("the agent's pane")
            .term
            .lock()
            .unwrap()
            .process(format!("\x1b]2;{title}\x07").as_bytes());
        open_card(&mut app);
        let labels: Vec<String> = app
            .fact_menu_controls()
            .into_iter()
            .filter(|l| l.starts_with("fact-menu label"))
            .collect();
        assert!(
            labels
                .iter()
                .take(2)
                .any(|l| l.ends_with(&format!("\u{201c}{name}\u{201d}\""))),
            "{title:?}: {labels:?}"
        );
        assert!(app.autopilot_menu_nav(wid, MenuKey::Close));
    }
}

/// THE OPEN CARD IS PUBLISHED IN WORDS: a menu node named in the card's spoken title and its
/// notes, its choices menu items in words, top down, each where the card paints it, the
/// tree's focus the highlighted row, which follows `↓`; a reader's click on one chooses it.
#[cfg(a11y_tree)]
#[test]
fn the_open_card_is_published_in_words_its_highlighted_row_focused() {
    use aterm_a11y::model::{Action, ActionRequest, NodeId, Role, TreeId, TreeUpdate};
    let menu_nodes = |update: &TreeUpdate| -> Vec<NodeId> {
        update
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.role(), Role::Menu | Role::MenuItem | Role::MenuItemRadio))
            .map(|(id, _)| *id)
            .collect()
    };
    let node = |update: &TreeUpdate, id: NodeId| {
        update
            .nodes
            .iter()
            .find(|(n, _)| *n == id)
            .map(|(_, n)| n.clone())
            .expect("the node")
    };
    let words = |text: &str| {
        text.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | ':' | ','))
    };
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    let bare = app.grid_a11y_tree(wid, None).expect("the grid's tree");
    assert!(menu_nodes(&bare).is_empty(), "no menu, no menu node");
    assert_eq!(crate::a11y_tree::menu_item_for(bare.focus), None);
    open_card(&mut app);
    let unpainted = app.grid_a11y_tree(wid, None).unwrap();
    assert!(
        menu_nodes(&unpainted).is_empty(),
        "not on the glass yet: not in the tree"
    );
    frame(&mut app, wid);
    let rect = app.autopilot_menu.as_ref().unwrap().rect.expect("painted");
    let first = row_of(&app, HIDE).expect("the first choice");
    let tree = app.grid_a11y_tree(wid, None).unwrap();
    let menu = tree
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::Menu)
        .map(|(_, n)| n.clone())
        .expect("the menu is published");
    let label = menu.label().expect("named");
    assert!(label.starts_with("Model, tab 1"), "{label}");
    assert!(
        !label.contains('\u{b7}') && !label.contains('\u{201c}') && !label.contains('\u{25c6}'),
        "{label}"
    );
    let items: Vec<_> = menu
        .children()
        .iter()
        .map(|id| (*id, node(&tree, *id)))
        .collect();
    let said: Vec<&str> = items.iter().map(|(_, n)| n.label().unwrap()).collect();
    assert_eq!(said, ["Hide model", "Show folder", "Show branch"]);
    for (i, (id, n)) in items.iter().enumerate() {
        let label = n.label().unwrap();
        assert!(words(label), "in words: {label}");
        assert_eq!(
            crate::a11y_tree::menu_item_for(*id).map(|(_, item)| item),
            Some(i)
        );
        assert!(
            !n.is_disabled() && n.supports_action(Action::Click),
            "{label}"
        );
        assert_eq!(n.role(), Role::MenuItem, "{label}");
        assert_eq!(n.toggled(), None, "{label}");
    }
    if let (Some(menu_box), Some(top)) = (menu.bounds(), items[0].1.bounds()) {
        let cell_h = (menu_box.y1 - menu_box.y0) / rect.h as f64;
        assert!(
            (top.y0 - (menu_box.y0 + cell_h * (1 + first) as f64)).abs() < 0.5,
            "the first choice is on the row the card paints it on"
        );
    }
    assert_eq!(tree.focus, items[0].0, "the highlighted row: the first");
    while app.autopilot_menu.as_ref().unwrap().highlight != row_of(&app, SHOW_FOLDER) {
        assert!(app.autopilot_menu_nav(wid, MenuKey::Down));
    }
    frame(&mut app, wid);
    let stepped = app.grid_a11y_tree(wid, None).unwrap();
    assert_eq!(stepped.focus, items[1].0, "↓: Show folder");
    app.on_accessibility_action(
        wid,
        ActionRequest {
            context: None,
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: items[1].0,
            data: None,
        },
    );
    assert!(app.autopilot_menu.is_none(), "chosen and closed");
    assert_eq!(app.fact_mask(session), taken(SHOW_FOLDER), "Show folder");
    frame(&mut app, wid);
    let closed = app.grid_a11y_tree(wid, None).unwrap();
    assert!(menu_nodes(&closed).is_empty(), "closed: no menu node");
    assert_eq!(crate::a11y_tree::menu_item_for(closed.focus), None);
}

/// The open menu's items as window `wid` publishes them (`App::grid_a11y_tree`):
/// each item's id and its words, top down; empty with no menu node.
#[cfg(a11y_tree)]
fn published_items(app: &App, wid: WindowId) -> Vec<(aterm_a11y::model::NodeId, String)> {
    let Some(tree) = app.grid_a11y_tree(wid, None) else {
        return Vec::new();
    };
    let label = |id: &aterm_a11y::model::NodeId| {
        tree.nodes
            .iter()
            .find(|(n, _)| n == id)
            .and_then(|(_, n)| n.label())
            .unwrap_or_default()
            .to_string()
    };
    tree.nodes
        .iter()
        .find(|(_, n)| n.role() == aterm_a11y::model::Role::Menu)
        .map(|(_, menu)| menu.children().iter().map(|id| (*id, label(id))).collect())
        .unwrap_or_default()
}

/// A screen reader's `action` on node `node` of window `wid`'s tree.
#[cfg(a11y_tree)]
fn reader(
    app: &mut App,
    wid: WindowId,
    action: aterm_a11y::model::Action,
    node: aterm_a11y::model::NodeId,
) {
    app.on_accessibility_action(
        wid,
        aterm_a11y::model::ActionRequest {
            context: None,
            action,
            target_tree: aterm_a11y::model::TreeId::ROOT,
            target_node: node,
            data: None,
        },
    );
}

/// A READER'S CLICK HEARD FROM ROWS THAT MOVED CHOOSES NOTHING: once the card's rows are built
/// again every item has a new id, and a click on an id heard before is refused, before the new
/// rows are painted and after; so is a click from another window, and one on a row the card
/// does not show. A click on an id of the rows on the glass chooses it.
#[cfg(a11y_tree)]
#[test]
fn a_readers_click_heard_from_rows_that_moved_chooses_nothing() {
    use aterm_a11y::model::Action;
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    let named = |items: &[(aterm_a11y::model::NodeId, String)], prefix: &str| {
        items
            .iter()
            .find(|(_, label)| label.starts_with(prefix))
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("no item {prefix}: {items:?}"))
    };
    frame(&mut app, wid);
    open_card(&mut app);
    frame(&mut app, wid);
    let heard = published_items(&app, wid);
    let last = named(&heard, "Show branch");
    assert_eq!(heard.last().map(|(id, _)| *id), Some(last), "{heard:?}");

    relay(&mut app, wid, |rows| {
        let at = rows
            .iter()
            .rposition(|row| matches!(row, MenuRow::Rule))
            .expect("the rule");
        rows.insert(
            at,
            choice(
                "\u{21ba}",
                "Refresh usage",
                FactAct::RefreshUsage,
                true,
                false,
            ),
        );
    });
    reader(&mut app, wid, Action::Click, last);
    assert!(
        app.autopilot_menu.is_some(),
        "unpainted rows: nothing chosen"
    );
    assert_eq!(app.fact_mask(session), BASE, "nothing hidden or shown");
    frame(&mut app, wid);
    let now = published_items(&app, wid);
    assert_eq!(now.len(), heard.len() + 1, "{now:?}");
    assert!(
        now.iter()
            .all(|(id, _)| heard.iter().all(|(old, _)| old != id)),
        "every id names the new rows"
    );
    reader(&mut app, wid, Action::Click, last);
    assert!(
        app.autopilot_menu.is_some(),
        "a click heard from the old rows is refused"
    );
    assert_eq!(app.fact_mask(session), BASE);

    let show = named(&now, "Show branch");
    let other = WindowId(wid.0 + 1);
    assert!(app.autopilot_grid_menu(other).is_none());
    reader(&mut app, other, Action::Click, show);
    assert!(app.autopilot_menu.is_some(), "another window's: nothing");
    assert_eq!(app.fact_mask(session), BASE);
    {
        let menu = app.autopilot_menu.as_mut().unwrap();
        let rect = menu.rect.expect("painted");
        menu.rect = Some(MenuRect { h: 2 + 3, ..rect });
    }
    reader(&mut app, wid, Action::Click, show);
    assert!(
        app.autopilot_menu.is_some(),
        "a row it does not show: refused"
    );
    assert_eq!(app.fact_mask(session), BASE);

    frame(&mut app, wid);
    assert_eq!(
        published_items(&app, wid),
        now,
        "the same rows, the same ids"
    );
    reader(&mut app, wid, Action::Click, show);
    assert!(app.autopilot_menu.is_none(), "chosen and closed");
    assert_eq!(app.fact_mask(session), taken(SHOW_BRANCH), "Show branch");
}

fn beside_settings(app: &mut App, wid: WindowId) -> u64 {
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(app.apply_term_resize(wid, 60, 120));
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    publish(app, session);
    app.set_fact_mask(session, BASE);
    let entry = app.pool.get(session).expect("the new stub terminal");
    let (rows, cols) = {
        let term = term_lock(&entry.term);
        (usize::from(term.rows()), usize::from(term.cols()))
    };
    assert!(
        rows >= 8 && cols >= 80,
        "the real terminal pane fits the composer"
    );
    let rule = "\u{2500}".repeat(cols);
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[{};1H\u{25CF} done\x1b[{};1H{rule}\x1b[{};1H\u{276F} \
         \x1b[{};1H{rule}\x1b[{};1H  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)\x1b[{};3H",
        rows - 5,
        rows - 3,
        rows - 2,
        rows - 1,
        rows,
        rows - 2,
    );
    feed(app, session, screen.as_bytes());
    frame(app, wid);
    assert!(matches!(
        app.active_visible_frame_layout(wid),
        Some((_, crate::VisibleContentRoute::Heterogeneous))
    ));
    session
}

/// SECONDARY ACTIVATION MEANS THE SAME ON TERMINAL-ONLY AND MIXED TABS: beside a native page
/// (the heterogeneous route) as on a terminal alone, a plain press on the Autopilot chip
/// cycles it and opens nothing; a Control-click there opens nothing and writes no word; a
/// Control-click on `yolo` opens the permission chip's box and stands no click; and a
/// Control-click on a fact opens its card.
#[cfg(target_os = "macos")]
#[test]
fn a_control_click_means_the_same_in_terminal_and_mixed_tabs() {
    use crate::claude_lights::Chip;
    use aterm_agent::harness::lights::Light;
    use aterm_window::event::{ElementState, MouseButton};
    use aterm_window::keyboard::ModifiersState;
    for mixed in [false, true] {
        let (mut app, wid, mut session, host) = claude_app();
        let _host = MenuTestHost(host);
        if mixed {
            session = beside_settings(&mut app, wid);
        } else {
            frame(&mut app, wid);
        }
        // The chip's first cell on the glass: the light's from its laid cells, the Autopilot
        // chip's the first cell of the light's row the glass answers as it.
        let chip_at = |app: &App, chip: Chip| {
            let (row, light) = app.windows[&wid]
                .claude_lights
                .light_cells(session)
                .map(|(frame_row, _, col)| (frame_row, col))
                .min()
                .unwrap_or_else(|| panic!("mixed={mixed}: the light is on the glass"));
            (0..=light)
                .map(|col| (row, col))
                .find(|&cell| {
                    let (x, y) = cell_centre(app, wid, cell);
                    app.claude_glass_at(wid, x, y)
                        == Some((session, crate::claude_lights::GlassAt::Chip(Some(chip))))
                })
                .unwrap_or_else(|| panic!("mixed={mixed}: {chip:?} is on the glass"))
        };
        let press = |app: &mut App, cell: (usize, usize), mods: ModifiersState| {
            let (x, y) = cell_centre(app, wid, cell);
            app.windows.get_mut(&wid).unwrap().mods = mods;
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
            app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        };
        let word = |app: &App| app.session_autopilot(session).0;
        let before = word(&app);
        let chip = chip_at(&app, Chip::Autopilot);
        press(&mut app, chip, ModifiersState::empty());
        assert!(app.autopilot_menu.is_none(), "mixed={mixed}: no menu");
        assert_ne!(word(&app), before, "mixed={mixed}: a plain press cycles");
        frame(&mut app, wid);

        let before = word(&app);
        let chip = chip_at(&app, Chip::Autopilot);
        press(&mut app, chip, ModifiersState::CONTROL);
        assert!(
            app.autopilot_menu.is_none(),
            "mixed={mixed}: a Control-click on the Autopilot chip opens nothing"
        );
        assert_eq!(word(&app), before, "mixed={mixed}: and writes no word");

        let light = chip_at(&app, Chip::Light(Light::Mode));
        press(&mut app, light, ModifiersState::CONTROL);
        assert_eq!(
            app.autopilot_menu.as_ref().map(|m| (m.owner, m.session)),
            Some((MenuOwner::Permission, session)),
            "mixed={mixed}: a Control-click on yolo opens its box"
        );
        let sid = app
            .pool
            .get(session)
            .unwrap()
            .ctx
            .self_id
            .as_str()
            .to_owned();
        assert!(
            app.permission_clicks.clicks.get(&sid).is_none(),
            "mixed={mixed}: and stands no click"
        );
        assert!(app.close_autopilot_menu());
        frame(&mut app, wid);

        let fact = app.fact_anchor(wid, session, FACT).expect("the model");
        press(&mut app, fact, ModifiersState::CONTROL);
        assert_eq!(
            app.autopilot_menu.as_ref().map(|m| (m.owner, m.session)),
            Some((MenuOwner::Fact(FACT), session)),
            "mixed={mixed}: a Control-click on a fact opens its card"
        );
    }
}

#[test]
fn the_cards_header_names_its_pane_where_two_agents_share_a_tab() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let right = app.split_active_stub_tab(wid);
    let claude = |app: &App, session: u64| {
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    };
    claude(&app, 0);
    let (alone, _) = app.menu_place_and_name(0);
    assert!(!alone.contains("pane"), "{alone}");
    claude(&app, right);
    let ((left, _), (right, _)) = (app.menu_place_and_name(0), app.menu_place_and_name(right));
    assert!(left.starts_with("tab 1, left pane"), "{left}");
    assert!(right.starts_with("tab 1, right pane"), "{right}");
    assert_ne!(left, right);
}

fn claude_split(
    (rows, cols): (u16, u16),
    dir: crate::pane::SplitDir,
) -> (App, WindowId, u64, u64, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.apply_term_resize(wid, rows, cols);
    assert_eq!(
        app.windows.get(&wid).map(|ws| (ws.rows, ws.cols)),
        Some((rows, cols))
    );
    let first = app.focused_session_id(wid).expect("the front session");
    let second = app.split_active_stub_tab_dir(wid, dir);
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    for session in [first, second] {
        publish(&app, session);
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            timeline.set_program(4242, Some("claude".into()));
        }
        app.set_fact_mask(session, BASE);
        let (_, _, rows, width) = app
            .visible_session_rect(wid, session)
            .expect("both panes are shown");
        let (rows, width) = (usize::from(rows), usize::from(width));
        let rule = "\u{2500}".repeat(width);
        let screen = format!(
            "\x1b]2;\u{2733} Claude Code\x07\x1b[?2004h\x1b[2J\x1b[{};1H{rule}\x1b[{};1H\u{276F} \
             \x1b[{};1H{rule}\x1b[{};3H",
            rows - 3,
            rows - 2,
            rows - 1,
            rows - 2,
        );
        feed(&app, session, screen.as_bytes());
    }
    assert_eq!(app.focused_session_id(wid), Some(second));
    (app, wid, first, second, host)
}

#[test]
fn the_card_keeps_its_name_whole_and_stays_in_its_pane() {
    use crate::pane::SplitDir;
    let cells = |app: &App, wid: WindowId, row: usize, span: std::ops::Range<usize>| -> String {
        row_text(app, wid, row)
            .chars()
            .skip(span.start)
            .take(span.len())
            .collect()
    };
    for (size, dir) in [
        ((30, 48), SplitDir::Vertical),
        ((30, 52), SplitDir::Vertical),
        ((30, 60), SplitDir::Vertical),
        ((30, 64), SplitDir::Vertical),
        ((30, 80), SplitDir::Vertical),
        ((30, 144), SplitDir::Vertical),
        ((30, 200), SplitDir::Vertical),
        ((30, 100), SplitDir::Horizontal),
        ((40, 100), SplitDir::Horizontal),
        ((24, 80), SplitDir::Horizontal),
    ] {
        let (mut app, wid, first, second, host) = claude_split(size, dir);
        let _host = MenuTestHost(host);
        frame(&mut app, wid);
        let sides = match dir {
            SplitDir::Vertical => ["left", "right"],
            SplitDir::Horizontal => ["top", "bottom"],
        };
        for (session, other, side) in [(second, first, sides[1]), (first, second, sides[0])] {
            let context = format!("{size:?} {dir:?}, the {side} pane");
            if session == first {
                assert!(app.raise_session_pane(first));
                assert_eq!(app.focused_session_id(wid), Some(first));
                frame(&mut app, wid);
            }
            let chrome = usize::from(app.chrome_rows(wid));
            let pane_of = |app: &App, session: u64| {
                let (row, col, height, width) = app.visible_session_rect(wid, session).unwrap();
                let (row, col) = (chrome + usize::from(row), usize::from(col));
                (
                    row..row + usize::from(height),
                    col..col + usize::from(width),
                )
            };
            let (rows, pane) = pane_of(&app, session);
            let (other_rows, other_cols) = pane_of(&app, other);
            let the_other = |app: &App| -> Vec<String> {
                other_rows
                    .clone()
                    .map(|row| cells(app, wid, row, other_cols.clone()))
                    .collect()
            };
            let before = the_other(&app);
            // Whichever fact the pane's width leaves on its rule: the card's place is the same.
            let fact = [Fact::Path, Fact::Effort, Fact::Model]
                .into_iter()
                .find(|fact| app.fact_anchor(wid, session, *fact).is_some())
                .unwrap_or_else(|| panic!("{context}: a fact on the glass"));
            app.open_front_fact_menu(fact)
                .unwrap_or_else(|why| panic!("{context}: {why}"));
            frame(&mut app, wid);
            let rect = rect_of(&app).unwrap_or_else(|| panic!("{context}: painted"));
            assert!(
                rect.col >= pane.start && rect.col + rect.w <= pane.end,
                "{context}: the card {rect:?} inside its pane's columns {pane:?}"
            );
            assert!(
                rect.row >= rows.start && rect.row + rect.h <= rows.end,
                "{context}: the card {rect:?} inside its pane's rows {rows:?}"
            );
            assert_eq!(
                the_other(&app),
                before,
                "{context}: the other pane is as it was"
            );
            let menu = app.autopilot_menu.as_ref().unwrap();
            let shown: Vec<String> = (rect.row + 1..rect.row + rect.h - 1)
                .map(|row| cells(&app, wid, row, rect.col..rect.col + rect.w))
                .collect();
            assert!(
                shown
                    .iter()
                    .any(|row| row.contains(&format!("{side} pane"))),
                "{context}: the pane word whole: {shown:?}"
            );
            assert!(
                shown
                    .iter()
                    .any(|row| row.contains(&format!("{} \u{b7} tab 1", fact.name()))),
                "{context}: the place: {shown:?}"
            );
            assert!(
                shown
                    .iter()
                    .any(|row| row.contains("\u{201c}\u{2733} Claude Code\u{201d}")),
                "{context}: the name whole: {shown:?}"
            );
            let want = menu.rows.len() + 2;
            if rows.len() >= want {
                assert_eq!(rect.h, want, "{context}: whole");
                let last = menu
                    .rows
                    .iter()
                    .rev()
                    .find_map(|row| match row {
                        MenuRow::Item { label, .. } => Some(label.as_str()),
                        _ => None,
                    })
                    .expect("a choice");
                assert!(
                    shown.iter().any(|row| row.contains(last)),
                    "{context}: its last choice {last:?}: {shown:?}"
                );
            } else {
                assert!(rect.h < want, "{context}: a prefix {rect:?}");
            }
            if pane.len() < menu.rows.iter().map(row_width).max().unwrap() + CHROME_W {
                assert_eq!(rect.w, pane.len(), "{context}: as narrow as its pane");
            }
            assert!(app.close_autopilot_menu());
            frame(&mut app, wid);
        }
    }
}

#[test]
fn the_open_cards_rect_is_the_pets_overlay_not_its_ground() {
    let (mut app, wid, _session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    open_card(&mut app);
    frame(&mut app, wid);
    let rect = app
        .autopilot_menu
        .as_ref()
        .and_then(|m| m.rect)
        .expect("painted");
    let dims = |r: &aterm_effects::pet_world::PetRect| (r.rows, r.cols);
    let card = (rect.h as f32, rect.w as f32);
    let chrome = &app.windows[&wid].pet_chrome;
    assert_eq!(
        chrome.held_overlays().iter().map(dims).collect::<Vec<_>>(),
        [card],
        "the card is the one overlay"
    );
    assert!(
        chrome.held().iter().all(|r| dims(r) != card),
        "and no chrome: {:?}",
        chrome.held()
    );
    assert!(app.close_autopilot_menu());
    frame(&mut app, wid);
    assert!(
        app.windows[&wid].pet_chrome.held_overlays().is_empty(),
        "closed, no overlay"
    );
}

#[test]
fn the_open_card_hides_the_caret_and_cuts_the_pet_under_it_on_both_routes() {
    use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers, is_sprite_layer, published};
    for mixed in [false, true] {
        let (mut app, wid, mut session, host) = claude_app();
        let _host = MenuTestHost(host);
        if mixed {
            session = beside_settings(&mut app, wid);
        } else {
            frame(&mut app, wid);
        }
        open_card_at_the_left(&mut app, wid, session, true);
        assert_eq!(app.autopilot_menu.as_ref().unwrap().session, session);
        frame(&mut app, wid);
        let rect = app
            .autopilot_menu
            .as_ref()
            .and_then(|m| m.rect)
            .expect("painted");
        let (cw, ch) = app.win_cell_size(wid);
        let px = |cells: usize, size: usize| i32::try_from(cells * size).unwrap();
        let right = rect.col + rect.w;
        assert!(
            right + 3 < app.windows[&wid].input_scratch.cols,
            "mixed={mixed}: room beside the card"
        );
        let body = aterm_render::fx::bitmap(
            aterm_core::render::FxTexels::Rgba8Straight,
            u16::try_from(4 * cw).unwrap(),
            u16::try_from(ch).unwrap(),
            vec![255; 4 * cw * ch * 4],
        )
        .expect("a body");
        let pet = FreeSprite {
            z: FreeZ::UnderText,
            ..FreeSprite::whole(px(right - 2, cw), px(rect.row + 2, ch), body)
        };
        let publish = |app: &mut App, caret: (usize, usize), seq: &mut u64| {
            let s = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
            (s.cursor_row, s.cursor_col, s.cursor_visible) = (caret.0, caret.1, true);
            s.fx_layers.retain(|l| !is_sprite_layer(l));
            SpriteLayers::new(0x6D65_6E75).publish(std::slice::from_ref(&pet), &mut s.fx_layers);
            *seq = s.snapshot_seq;
        };
        let mut seq = 0;
        frame_with(&mut app, wid, |app| {
            publish(app, (rect.row + 2, rect.col + 3), &mut seq);
        });
        let s = &app.windows[&wid].input_scratch;
        assert!(
            !s.cursor_visible,
            "mixed={mixed}: the caret under the card is hidden"
        );
        assert_ne!(
            s.snapshot_seq, seq,
            "mixed={mixed}: the card is a host write"
        );
        let drawn = published(&s.fx_layers);
        assert_eq!(
            drawn
                .iter()
                .map(|q| (q.x, q.y, q.w, q.h))
                .collect::<Vec<_>>(),
            [(pet.x, pet.y, pet.w, pet.h)],
            "mixed={mixed}: the pet's layer stands whole"
        );
        let occ = s
            .fx_occluders
            .iter()
            .find(|o| o.x1 - o.x0 == px(rect.w, cw) && o.y1 - o.y0 == px(rect.h, ch))
            .expect("the card occludes every fx layer");
        let (pad, top) = (occ.x0 - px(rect.col, cw), occ.y0 - px(rect.row, ch));
        let (x0, x1) = (pad + pet.x, pad + pet.x + i32::from(pet.w));
        let (y0, y1) = (top + pet.y, top + pet.y + i32::from(pet.h));
        assert!(
            occ.x0 <= x0 && occ.x1 == pad + px(right, cw) && occ.x1 < x1,
            "mixed={mixed}: the card cuts the part of the pet on it ({x0}..{}) and leaves \
             the part beside it ({}..{x1})",
            occ.x1,
            occ.x1
        );
        assert!(
            occ.y0 <= y0 && y1 <= occ.y1,
            "mixed={mixed}: over all of the pet's rows"
        );
        frame_with(&mut app, wid, |app| {
            publish(app, (rect.row + 2, right + 1), &mut seq);
        });
        assert!(
            app.windows[&wid].input_scratch.cursor_visible,
            "mixed={mixed}: CONTROL, the caret beside the card stands"
        );
    }
}

fn repainted(
    prev: &aterm_render::RenderInput,
    now: &aterm_render::RenderInput,
    cell_h: usize,
) -> Vec<usize> {
    let mut dirty = Vec::new();
    match aterm_render::compute_dirty_rows(
        prev,
        now,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, cell_h),
        &mut dirty,
    ) {
        aterm_render::DirtyDecision::FullRepaint => (0..now.rows).collect(),
        aterm_render::DirtyDecision::Rows(_) => (0..dirty.len()).filter(|&r| dirty[r]).collect(),
    }
}

#[test]
fn the_open_card_is_repainted_whole_where_the_program_drew_nothing() {
    for mixed in [false, true] {
        let (mut app, wid, _, host) = claude_app();
        let _host = MenuTestHost(host);
        if mixed {
            beside_settings(&mut app, wid);
        }
        frame(&mut app, wid);
        frame(&mut app, wid);
        let ch = app.win_cell_size(wid).1;
        let scratch = |app: &App| app.windows[&wid].input_scratch.clone();
        let closed = scratch(&app);
        open_card(&mut app);
        frame(&mut app, wid);
        let open = scratch(&app);
        let rect = rect_of(&app).expect("painted");
        let card: Vec<usize> = (rect.row..rect.row + rect.h).collect();
        let shown = repainted(&closed, &open, ch);
        assert!(
            card.iter().all(|r| shown.contains(r)),
            "mixed={mixed}: the open repaints every card row {card:?}, not only {shown:?}"
        );
        frame(&mut app, wid);
        let held = scratch(&app);
        let still = repainted(&open, &held, ch);
        assert!(
            !card.iter().all(|r| still.contains(r)),
            "mixed={mixed}: CONTROL, a frame the card did not move in does not repaint it \
             whole: {still:?}"
        );
        assert!(app.close_autopilot_menu());
        frame(&mut app, wid);
        let gone = repainted(&held, &scratch(&app), ch);
        assert!(
            card.iter().all(|r| gone.contains(r)),
            "mixed={mixed}: the close repaints every card row {card:?}, not only {gone:?}"
        );
    }
}

/// How a press reaches the open menu in a lifecycle test, always at the
/// pixel the pointer last moved to.
#[derive(Clone, Copy, Debug)]
enum Press {
    /// The event loop's `MouseInput` alone, as Linux and Windows deliver it.
    Bare,
    /// As AppKit delivers a click: `mouseDown:` (and `mouseUp:`) queue a
    /// `CursorMoved` at the press point ahead of the button
    /// (`aterm-window`'s macOS `view.rs`, `mouse_down`) — a motion event
    /// in which the pointer did not move.
    AppKit,
    /// `ctl pointer click`, which runs what the platform delivers.
    Ctl,
}

fn click(app: &mut App, wid: WindowId, (x, y): (f64, f64), how: Press) {
    use aterm_window::event::{ElementState, MouseButton};
    assert_eq!(app.windows[&wid].last_cursor_px, (x, y), "{how:?}: pointer");
    match how {
        Press::Bare | Press::AppKit => {
            for state in [ElementState::Pressed, ElementState::Released] {
                if matches!(how, Press::AppKit) {
                    app.on_cursor_moved(wid, x, y);
                }
                app.mouse_input_then_fold(wid, state, MouseButton::Left);
            }
        }
        Press::Ctl => {
            app.pointer_cmd(crate::app_mouse::PointerAction::Click)
                .expect("the pointer has a position");
        }
    }
}

fn rect_of(app: &App) -> Option<MenuRect> {
    app.autopilot_menu.as_ref().and_then(|m| m.rect)
}

fn row_px(app: &App, wid: WindowId, i: usize) -> ((f64, f64), usize) {
    let rect = rect_of(app).expect("painted");
    let menu = app.autopilot_menu.as_ref().unwrap();
    let visible = menu
        .viewport
        .range(menu.rows.len(), rect.h.saturating_sub(2));
    assert!(visible.contains(&i), "logical row {i} outside {visible:?}");
    let row = rect.row + 1 + i - visible.start;
    (cell_centre(app, wid, (row, rect.col + 3)), row)
}

fn key_on(app: &App, row: usize) -> Option<String> {
    let rect = rect_of(app)?;
    let menu = app.autopilot_menu.as_ref()?;
    let i = row_at(rect, menu.viewport, menu.rows.len(), row)?;
    keys(&menu.rows).get(i)?.clone()
}

/// A note arrives under the card's last choice: the card, hung above its fact, grows upward,
/// so every row above the note stands one frame row higher.
fn a_note_arrives(app: &mut App, wid: WindowId) {
    relay(app, wid, |rows| rows.push(MenuRow::note("a note arrived")));
}

/// A ROW NEVER MOVES UNDER A STILL POINTER TO ANOTHER CHOICE. A row that leaves a card leaves a
/// gap, so the choice under a resting pointer keeps its row and a press there takes it at once,
/// before the next frame and after. Where rows arrive and another choice comes to stand under a
/// pointer that has not moved (whether it came to rest over this card, an earlier one, or the
/// bare pane before the open), or the card widens under a pointer beside it, the first press
/// chooses nothing and lights what is there; the next press takes it. A pointer that moved onto
/// a choice presses at once.
#[test]
fn a_row_never_moves_under_a_still_pointer_to_another_choice() {
    let menu_open = |app: &App| app.autopilot_menu.is_some();
    const LEAVING: &str = "fact-menu/path/refresh-usage";
    for how in [Press::Bare, Press::Ctl, Press::AppKit] {
        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        frame(&mut app, wid);
        for before_the_frame in [true, false] {
            app.set_fact_mask(session, BASE);
            frame(&mut app, wid);
            open_card(&mut app);
            relay(&mut app, wid, |rows| {
                let at = rows
                    .iter()
                    .position(|row| matches!(row, MenuRow::Rule))
                    .expect("the rule");
                rows.insert(
                    at,
                    choice(
                        "\u{21ba}",
                        "Refresh usage",
                        FactAct::RefreshUsage,
                        true,
                        false,
                    ),
                );
            });
            frame(&mut app, wid);
            let rect = rect_of(&app).expect("painted");
            let target = if before_the_frame { HIDE } else { SHOW_FOLDER };
            let i = row_of(&app, target).expect("the target row");
            let (at, frame_row) = row_px(&app, wid, i);
            app.on_cursor_moved(wid, at.0, at.1);
            assert_eq!(app.autopilot_menu.as_ref().unwrap().highlight, Some(i));
            relay(&mut app, wid, |rows| {
                rows.retain(
                    |row| !matches!(row, MenuRow::Item { act, .. } if act.key() == LEAVING),
                );
            });
            assert_eq!(rect_of(&app), Some(rect), "{how:?}: the card kept its rect");
            if !before_the_frame {
                frame(&mut app, wid);
                let shown = app.fact_menu_controls().join("\n");
                assert!(!shown.contains(LEAVING), "{shown}");
                assert!(shown.contains("fact-menu gap index="), "{shown}");
                assert_eq!(rect_of(&app), Some(rect), "{how:?}: and kept it painted");
            }
            assert_eq!(
                key_on(&app, frame_row).as_deref(),
                Some(target),
                "{how:?}: {target} kept its row"
            );
            click(&mut app, wid, at, how);
            assert!(!menu_open(&app), "{how:?}: chosen");
            assert_eq!(
                app.fact_mask(session),
                taken(target),
                "{how:?}, before the frame={before_the_frame}: the press chose {target}, as \
                 the pointer saw"
            );
            app.on_cursor_left(wid);
        }

        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        frame(&mut app, wid);
        open_card(&mut app);
        frame(&mut app, wid);
        let rect = rect_of(&app).expect("painted");
        let (at, frame_row) = row_px(&app, wid, row_of(&app, HIDE).unwrap());
        app.on_cursor_moved(wid, at.0, at.1);
        a_note_arrives(&mut app, wid);
        frame(&mut app, wid);
        let grown = rect_of(&app).expect("painted");
        assert_eq!(grown.h, rect.h + 1, "the note arrived: the card grew");
        assert_eq!(
            key_on(&app, frame_row).as_deref(),
            Some(SHOW_FOLDER),
            "Show model now stands where the pointer rests"
        );
        click(&mut app, wid, at, how);
        assert!(
            menu_open(&app),
            "{how:?}: the press the rows moved under chose nothing"
        );
        assert_eq!(app.fact_mask(session), BASE, "{how:?}: nothing written");
        assert_eq!(
            app.autopilot_menu.as_ref().unwrap().highlight,
            row_of(&app, SHOW_FOLDER),
            "{how:?}: it lit the choice under the pointer"
        );
        click(&mut app, wid, at, how);
        assert!(!menu_open(&app), "{how:?}: the next press chose it");
        assert_eq!(app.fact_mask(session), taken(SHOW_FOLDER));

        for over_a_menu in [false, true] {
            app.set_fact_mask(session, BASE);
            frame(&mut app, wid);
            open_card(&mut app);
            frame(&mut app, wid);
            let (at, frame_row) = row_px(&app, wid, row_of(&app, HIDE).unwrap());
            if over_a_menu {
                app.on_cursor_moved(wid, at.0, at.1);
            }
            assert!(app.close_autopilot_menu());
            frame(&mut app, wid);
            if !over_a_menu {
                app.on_cursor_moved(wid, at.0, at.1);
            }
            open_card(&mut app);
            frame(&mut app, wid);
            assert_eq!(key_on(&app, frame_row).as_deref(), Some(HIDE));
            a_note_arrives(&mut app, wid);
            frame(&mut app, wid);
            assert_eq!(key_on(&app, frame_row).as_deref(), Some(SHOW_FOLDER));
            click(&mut app, wid, at, how);
            assert!(
                menu_open(&app),
                "{how:?}, over a menu={over_a_menu}: a pointer still since before the open \
                 chose nothing"
            );
            assert_eq!(app.fact_mask(session), BASE, "{how:?}: nothing written");
            click(&mut app, wid, at, how);
            assert!(!menu_open(&app), "{how:?}: the next press chose it");
            assert_eq!(app.fact_mask(session), taken(SHOW_FOLDER));
            app.on_cursor_left(wid);
        }

        let (mut wide_app, wwid, wsession, whost) = claude_app();
        let _whost = MenuTestHost(whost);
        frame(&mut wide_app, wwid);
        open_card(&mut wide_app);
        frame(&mut wide_app, wwid);
        let rect = rect_of(&wide_app).expect("painted");
        // The card hangs from the model at the rule's left end, so it widens to its right.
        let beside_cell = (
            rect.row + 1 + row_of(&wide_app, HIDE).unwrap(),
            rect.col + rect.w + 1,
        );
        let beside = cell_centre(&wide_app, wwid, beside_cell);
        wide_app.on_cursor_moved(wwid, beside.0, beside.1);
        relay(&mut wide_app, wwid, |rows| {
            let note = rows
                .iter_mut()
                .find(|row| matches!(row, MenuRow::Note { .. }))
                .expect("the model's note");
            *note = MenuRow::note("\u{25c6}  a far longer model name past the card's edge");
        });
        frame(&mut wide_app, wwid);
        let wide = rect_of(&wide_app).expect("painted");
        assert!(
            wide.contains(beside_cell.0, beside_cell.1),
            "the card widened under the pointer: {rect:?} -> {wide:?}"
        );
        assert_eq!(key_on(&wide_app, beside_cell.0).as_deref(), Some(HIDE));
        click(&mut wide_app, wwid, beside, how);
        assert!(
            wide_app.autopilot_menu.is_some(),
            "{how:?}: a press beside the card it grew under chose nothing"
        );
        assert_eq!(
            wide_app.fact_mask(wsession),
            BASE,
            "{how:?}: nothing written"
        );
        click(&mut wide_app, wwid, beside, how);
        assert!(
            wide_app.autopilot_menu.is_none(),
            "{how:?}: the next press chose it"
        );
        assert_eq!(wide_app.fact_mask(wsession), taken(HIDE));
        drop(wide_app);

        app.set_fact_mask(session, BASE);
        frame(&mut app, wid);
        open_card(&mut app);
        frame(&mut app, wid);
        let (at, _) = row_px(&app, wid, row_of(&app, HIDE).unwrap());
        app.on_cursor_moved(wid, at.0, at.1);
        a_note_arrives(&mut app, wid);
        frame(&mut app, wid);
        let (moved, _) = row_px(&app, wid, row_of(&app, HIDE).unwrap());
        assert_ne!(moved, at);
        app.on_cursor_moved(wid, moved.0, moved.1);
        click(&mut app, wid, moved, how);
        assert!(!menu_open(&app), "{how:?}: a moved pointer presses at once");
        assert_eq!(app.fact_mask(session), taken(HIDE));
    }
}

/// A CARD PAINTED UNDER A RESTING POINTER TAKES NO PRESS IT DID NOT SEE: in a pane too short to
/// hang the card clear of its fact, a right press opens it over the pointer; the first press
/// there chooses nothing and lights the choice, the next takes it. The same holds for a card a
/// key opened under the pointer. A pointer that moved since the card came up presses at once.
#[test]
fn a_card_painted_under_a_resting_pointer_takes_no_press_it_did_not_see() {
    use crate::pane::SplitDir;
    use aterm_window::event::{ElementState, MouseButton};
    for how in [Press::Bare, Press::Ctl, Press::AppKit] {
        // Panes too short to hang the card above its fact: it is laid over it.
        let (mut app, wid, _first, session, host) = claude_split((16, 80), SplitDir::Horizontal);
        let _host = MenuTestHost(host);
        // No Claude Code: the model's card hides and shows facts, each choice a mask of its own.
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            timeline.set_program(4242, None);
        }
        frame(&mut app, wid);
        let fact = app
            .fact_anchor(wid, session, FACT)
            .expect("the folder is on the glass");
        let at = cell_centre(&app, wid, fact);
        let open_by_hand = |app: &mut App| {
            app.on_cursor_moved(wid, at.0, at.1);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
            assert!(
                app.autopilot_menu.is_some(),
                "{how:?}: the right press opened it"
            );
            frame(app, wid);
        };
        open_by_hand(&mut app);
        let rect = rect_of(&app).expect("painted");
        assert!(
            rect.contains(fact.0, fact.1),
            "{how:?}: the card {rect:?} covers its fact {fact:?}"
        );
        let under = key_on(&app, fact.0).expect("a choice under the pointer");
        let rows = app.autopilot_menu.as_ref().unwrap().rows.clone();
        let want = mask_of(&rows, row_of(&app, &under).unwrap())
            .unwrap_or_else(|| panic!("{under} hides or shows a fact"));
        click(&mut app, wid, at, how);
        assert!(
            app.autopilot_menu.is_some(),
            "{how:?}: a press on {under}, which came up under a still pointer, chose nothing"
        );
        assert_eq!(app.fact_mask(session), BASE, "{how:?}: nothing written");
        assert_eq!(
            app.autopilot_menu.as_ref().unwrap().highlight,
            row_of(&app, &under),
            "{how:?}: it lit {under}"
        );
        click(&mut app, wid, at, how);
        assert!(
            app.autopilot_menu.is_none(),
            "{how:?}: the next press chose it"
        );
        assert_eq!(app.fact_mask(session), want, "{how:?}: {under} written");
        app.set_fact_mask(session, BASE);
        frame(&mut app, wid);
        open_card(&mut app);
        frame(&mut app, wid);
        assert!(rect_of(&app).is_some_and(|r| r.contains(fact.0, fact.1)));
        click(&mut app, wid, at, how);
        assert!(
            app.autopilot_menu.is_some(),
            "{how:?}: keyboard open, nothing chosen"
        );
        assert_eq!(
            app.fact_mask(session),
            BASE,
            "{how:?}: keyboard open, nothing written"
        );
        assert!(app.close_autopilot_menu());
        frame(&mut app, wid);
        open_by_hand(&mut app);
        let moved = (at.0 + 1.0, at.1);
        app.on_cursor_moved(wid, moved.0, moved.1);
        assert_eq!(key_on(&app, fact.0), Some(under.clone()));
        click(&mut app, wid, moved, how);
        assert!(
            app.autopilot_menu.is_none(),
            "{how:?}: a moved pointer chooses at once"
        );
        assert_eq!(app.fact_mask(session), want, "{how:?}: {under} written");
        app.on_cursor_left(wid);
    }
}

#[test]
fn fx_occluders_compose_over_a_fact_card_and_the_paste_banner() {
    use crate::app_render::fx_occluder_tests::{
        Shape, assert_chrome_cuts_the_light, chrome_written, trace_splice,
    };
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    open_card(&mut app);
    let mut before = None;
    frame_with(&mut app, wid, |app| {
        before = Some(app.windows[&wid].input_scratch.clone());
    });
    assert!(
        app.autopilot_menu.as_ref().and_then(|m| m.rect).is_some(),
        "the card painted"
    );
    let menu = chrome_written(
        &mut app,
        wid,
        "fact card",
        Shape::Card,
        &before.expect("the frame reached the card's splice"),
    );
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(session),
        "one\ntwo\nthree".to_owned(),
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
    let banner = trace_splice(&mut app, wid, "paste banner", Shape::Band, |app| {
        app.splice_paste_banner(wid);
    });
    assert_chrome_cuts_the_light(&mut app, wid, &[menu, banner]);
}

#[test]
fn fx_occluders_compose_over_a_fact_card_the_question_and_the_caption() {
    use crate::app_render::fx_occluder_tests::{
        Shape, assert_chrome_cuts_the_light, chrome_written, trace_splice,
    };
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    feed(
        &app,
        session,
        b"\x1b[7;1HVisit \x1b]8;;https://example.com/a\x1b\\example.com\x1b]8;;\x1b\\ now\x1b[22;3H",
    );
    frame(&mut app, wid);
    let (x, y) = cell_centre(&app, wid, (6, 8));
    app.on_cursor_moved(wid, x, y);
    open_card(&mut app);
    let mut before = None;
    frame_with(&mut app, wid, |app| {
        before = Some(app.windows[&wid].input_scratch.clone());
    });
    assert!(
        app.autopilot_menu.as_ref().and_then(|m| m.rect).is_some(),
        "the card painted"
    );
    let menu = chrome_written(
        &mut app,
        wid,
        "fact card",
        Shape::Card,
        &before.expect("the frame reached the card's splice"),
    );
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(session),
        "one\ntwo\nthree".to_owned(),
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
    let banner = trace_splice(&mut app, wid, "paste banner", Shape::Band, |app| {
        app.splice_paste_banner(wid);
    });
    let caption = trace_splice(&mut app, wid, "link caption", Shape::Band, |app| {
        app.splice_link_target(wid);
    });
    assert_chrome_cuts_the_light(&mut app, wid, &[menu, banner, caption]);
    app.on_cursor_left(wid);
}

fn zoom(app: &mut App, size: f32) {
    assert!(app.set_font_size_with(size, |_| true), "zoomed to {size}");
}

/// A card's menu takes the keys, so the session field it opens over gives them up: a client
/// told of a row would otherwise press Enter on it and commit the name.
#[cfg(a11y_tree)]
#[test]
fn a_cards_menu_settles_the_session_field_it_opens_over() {
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    app.tab_strip_rows = 1;
    frame(&mut app, wid);
    let tab = app.windows[&wid].tab_set.active().unwrap().id;

    assert!(app.begin_session_rename(wid, tab));
    app.windows.get_mut(&wid).unwrap().strip_press = Some((Instant::now(), tab));
    assert!(app.windows[&wid].rename_edit.is_some(), "CONTROL");
    let fact = crate::claude_lights::Fact::TopEffort;
    app.open_fact_menu_keeping_note(wid, session, fact, (3, 3), false);
    assert!(app.windows[&wid].rename_edit.is_none(), "a fact's");
    assert!(
        app.windows[&wid].strip_press.is_none(),
        "a fact's menu breaks the strip's double-click streak"
    );
}

/// Both cards up, the tab's is on top and takes the keys, so it is the one a client reads and
/// presses: the tree may not announce one menu while Enter acts on another.
#[cfg(a11y_tree)]
#[test]
fn a_tab_menu_over_a_fact_card_is_the_menu_a_client_reads() {
    let (mut app, wid, _, host) = claude_app();
    let _host = MenuTestHost(host);
    app.tab_strip_rows = 1;
    frame(&mut app, wid);
    open_card(&mut app);
    frame(&mut app, wid);

    assert!(app.open_tab_context_menu(wid, 0, 3, true));
    frame(&mut app, wid);
    app.splice_tab_menu(wid);
    assert!(app.autopilot_grid_menu(wid).is_some(), "CONTROL: both open");

    let heard = published_items(&app, wid);
    let rename = heard
        .iter()
        .find(|(_, words)| words.starts_with("Rename Session"))
        .unwrap_or_else(|| panic!("the tab's rows: {heard:?}"))
        .0;
    reader(&mut app, wid, aterm_a11y::model::Action::Click, rename);
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "the press lands on the menu it read"
    );
}

/// Open the folder's card over the pane's left and find a zoom that puts one of its hide and show
/// choices under the pixel of another: the zoom's size, the two rows and the pixel.
fn open_where_a_zoom_moves_a_choice(
    app: &mut App,
    wid: WindowId,
    session: u64,
) -> (f32, usize, usize, (f64, f64)) {
    frame(app, wid);
    open_card_at_the_left(app, wid, session, false);
    frame(app, wid);
    let rect = rect_of(app).expect("painted");
    let rows = app.autopilot_menu.as_ref().expect("open").rows.clone();
    let cells: Vec<(usize, (f64, f64))> = (0..rows.len())
        .filter(|&i| mask_of(&rows, i).is_some())
        .map(|i| (i, row_px(app, wid, i).0))
        .collect();
    let base = app.font_size;
    let found = (1..=16u8)
        .find_map(|step| {
            let size = base + f32::from(step);
            zoom(app, size);
            let found = cells.iter().find_map(|&(i, (x, y))| {
                let (row, col) = app.frame_cell_at(wid, x, y)?;
                let j = item_at(rect, &rows, MenuViewport::default(), row, col)
                    .filter(|&j| mask_of(&rows, j).is_some())?;
                (j != i).then_some((size, i, j, (x, y)))
            });
            zoom(app, base);
            found
        })
        .expect("a zoom puts another hide or show choice under one's pixel");
    assert_eq!(
        app.autopilot_menu.as_ref().and_then(|m| m.laid),
        app.glass_grid(wid),
        "the zoom undone: the grid the card was laid on"
    );
    found
}

fn lit(app: &App) -> Option<usize> {
    app.autopilot_menu.as_ref().and_then(|m| m.highlight)
}

#[test]
fn a_regrid_leaves_the_cards_last_rect_answering_nothing() {
    for forged in [true, false] {
        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        let (size, was, now, (x, y)) = open_where_a_zoom_moves_a_choice(&mut app, wid, session);
        let rows = app.autopilot_menu.as_ref().expect("open").rows.clone();
        app.on_cursor_moved(wid, x, y);
        assert_eq!(
            lit(&app),
            Some(was),
            "on the grid it was laid on, its choice"
        );
        zoom(&mut app, size);
        let laid = app.autopilot_menu.as_ref().and_then(|m| m.laid);
        assert_ne!(app.glass_grid(wid), laid, "the zoom re-grids the window");
        if forged {
            app.autopilot_menu.as_mut().expect("open").laid = app.glass_grid(wid);
        }
        app.on_cursor_moved(wid, x, y);
        if forged {
            assert_eq!(
                lit(&app),
                Some(now),
                "CONTROL: unstamped, another choice lit"
            );
        } else {
            assert_eq!(
                lit(&app),
                Some(was),
                "nothing lit from a rect laid on another grid"
            );
        }
        click(&mut app, wid, (x, y), Press::Bare);
        assert!(
            app.autopilot_menu.is_none(),
            "forged={forged}: the press closes it"
        );
        if forged {
            assert_eq!(
                Some(app.fact_mask(session)),
                mask_of(&rows, now),
                "CONTROL: the choice the pointer never rested on is chosen"
            );
        } else {
            assert_eq!(app.fact_mask(session), BASE, "nothing is chosen");
        }
    }
}

#[test]
fn a_card_laid_again_on_a_new_grid_takes_no_press_it_did_not_see() {
    for moved in [false, true] {
        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        let (size, was, now, (x, y)) = open_where_a_zoom_moves_a_choice(&mut app, wid, session);
        let rows = app.autopilot_menu.as_ref().expect("open").rows.clone();
        let rect = rect_of(&app).expect("painted");
        app.on_cursor_moved(wid, x, y);
        assert_eq!(lit(&app), Some(was));
        zoom(&mut app, size);
        frame(&mut app, wid);
        assert_eq!(rect_of(&app), Some(rect), "laid again at the same cells");
        assert_eq!(
            app.autopilot_menu.as_ref().and_then(|m| m.laid),
            app.glass_grid(wid),
            "on the zoomed grid"
        );
        let (row, col) = app.frame_cell_at(wid, x, y).expect("on the frame");
        assert_eq!(
            item_at(rect, &rows, MenuViewport::default(), row, col),
            Some(now),
            "another choice under the pixel"
        );
        if moved {
            let (cx, cy) = cell_centre(&app, wid, (row, col));
            app.on_cursor_moved(wid, cx, cy + 1.0);
            app.on_cursor_moved(wid, x, y);
        }
        click(&mut app, wid, (x, y), Press::Bare);
        if moved {
            assert!(app.autopilot_menu.is_none(), "CONTROL: chosen at once");
            assert_eq!(Some(app.fact_mask(session)), mask_of(&rows, now), "CONTROL");
            continue;
        }
        assert!(
            app.autopilot_menu.is_some(),
            "the press the rows moved under is refused"
        );
        assert_eq!(app.fact_mask(session), BASE, "nothing is chosen");
        assert_eq!(
            lit(&app),
            Some(now),
            "the choice now under the pointer is lit"
        );
        click(&mut app, wid, (x, y), Press::Bare);
        assert!(app.autopilot_menu.is_none(), "the next press takes it");
        assert_eq!(Some(app.fact_mask(session)), mask_of(&rows, now));
    }
}

#[test]
fn a_regrid_between_a_double_clicks_presses_moves_no_pixel_of_it() {
    for on_card in [true, false] {
        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        frame(&mut app, wid);
        open_card_by_pointer(&mut app, wid, session);
        frame(&mut app, wid);
        let rect = rect_of(&app).expect("painted");
        let rows = app.autopilot_menu.as_ref().expect("open").rows.clone();
        let i = (0..rows.len())
            .find(|&i| mask_of(&rows, i).is_some())
            .expect("a hide or show choice");
        let ((x, y), _) = row_px(&app, wid, i);
        let off = cell_centre(&app, wid, (0, 0));
        assert!(!rect.contains(0, 0), "the corner is off the card");
        app.on_cursor_moved(wid, x, y);
        click(&mut app, wid, (x, y), Press::Bare);
        assert!(app.autopilot_menu.is_none(), "chosen and closed");
        let chosen = app
            .autopilot_menu_spent
            .as_ref()
            .expect("the card a choice closed")
            .at;
        let doubled = app.font_size * 2.0;
        zoom(&mut app, doubled);
        assert!(
            app.frame_cell_at(wid, x, y)
                .is_none_or(|(row, col)| !rect.contains(row, col)),
            "the zoomed grid puts the pixel off the card's rect"
        );
        if !on_card {
            app.on_cursor_moved(wid, off.0, off.1);
        }
        let second = chosen
            + std::time::Duration::from_millis(
                u64::try_from(crate::multi_click_ms()).expect("a double click's interval"),
            );
        assert_eq!(
            app.spends_menu_follow_up(wid, second),
            on_card,
            "on_card={on_card}: spent only where it fell on the card as laid"
        );
    }
}

/// THE CARD BORDER IS THE STATIC ARC (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4):
/// a fact card under a palette paints each border cell, the corners and both sides included, in
/// [`GlassInks::border`] at its place round the perimeter (the launch card's border, held
/// still), and draws its rules inside the border so the arc runs whole; the same card painted
/// again paints the same border. With no palette the border is the card's rule ink and a rule
/// meets it (`├─┤`). Negative controls: a border in the card's rule ink (the card before S6) is
/// one colour; a border hued by column alone paints each side one colour down its height.
#[test]
fn the_card_border_is_the_static_arc() {
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let rows = vec![
        MenuRow::Header("Account \u{b7} tab 1".to_string()),
        MenuRow::note("continuous for the next 7 days"),
        MenuRow::Rule,
        MenuRow::note("main   25% left  Max 20x"),
        MenuRow::note("alt    60% left  Max 20x"),
        MenuRow::note("pool   88% left  Max 5x"),
        MenuRow::Rule,
        MenuRow::note("Not pinned: the next agent here takes the freshest account."),
    ];
    let rect = MenuRect {
        row: 0,
        col: 0,
        w: 36,
        h: rows.len() + 2,
    };
    let c = crate::tab_menu::glass_menu_colors(theme);
    let (w, h) = (rect.w, rect.h);
    let border = |r: usize, col: usize| r == 0 || r + 1 == h || col == 0 || col + 1 == w;
    for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
        let glass = GlassInks::of(Some(palette), pack(c.card_bg), theme.fg, pack(c.rule));
        let painted = paint(
            rect,
            &rows,
            MenuViewport::default(),
            None,
            theme,
            Some(palette),
        );
        assert_eq!(painted.len(), h);
        let mut inks = std::collections::BTreeSet::new();
        for (r, line) in painted.iter().enumerate() {
            for (col, cell) in line.iter().enumerate().filter(|(col, _)| border(r, *col)) {
                let want = unpack(glass.border(r, col, h, w));
                assert_eq!(cell.fg, want, "{palette:?}: ({r}, {col}) {:?}", cell.ch);
                inks.insert(cell.fg);
            }
        }
        if palette != CardPalette::Off {
            assert!(
                inks.len() > 4,
                "{palette:?}: the arc, not one ink: {inks:?}"
            );
            for col in [0, w - 1] {
                let side: std::collections::BTreeSet<_> =
                    (1..h - 1).map(|r| painted[r][col].fg).collect();
                assert!(side.len() > 1, "{palette:?}: column {col} keeps one ink");
            }
        }
        for r in [3, 7] {
            assert_eq!(
                painted[r][0].ch, '\u{2502}',
                "{palette:?}: a rule inside the border"
            );
            assert_eq!(painted[r][w - 1].ch, '\u{2502}');
            assert!(
                painted[r][1..w - 1]
                    .iter()
                    .any(|cell| cell.ch == '\u{2500}' && cell.fg == c.rule),
                "{palette:?}: the rule in the card's rule ink"
            );
        }
        let again = paint(
            rect,
            &rows,
            MenuViewport::default(),
            None,
            theme,
            Some(palette),
        );
        assert_eq!(painted, again, "{palette:?}: a static border");
    }
    let plain = paint(rect, &rows, MenuViewport::default(), None, theme, None);
    let rule = crate::tab_menu::menu_colors(theme).rule;
    for (r, line) in plain.iter().enumerate() {
        for (col, cell) in line.iter().enumerate().filter(|(col, _)| border(r, *col)) {
            assert_eq!(cell.fg, rule, "no palette: ({r}, {col})");
        }
    }
    assert_eq!(
        plain[3][0].ch, '\u{251c}',
        "no palette: a rule meets the border"
    );
}

/// A ROW'S WIDTH IS ITS TEXT'S, COUNTED WITHOUT BUILDING IT: over rows of every kind (a card's
/// head, a note, a drawn row, a name in quotes, an item behind a glyph narrower and one wider
/// than its column, a gap), [`row_width`] is the cells of the row's text, an item's check and a
/// group heading's blank and first cell of rule included, and reading a whole card's width
/// allocates nothing. Negative control: the width read off [`MenuRow::text`] allocates once a
/// row.
#[test]
fn a_rows_width_is_its_texts_counted_without_building_it() {
    let mut rows = header_rows("Account \u{b7} tab 1", None, 60);
    rows.push(MenuRow::note("61% left \u{b7} resets Sat 10:00"));
    rows.push(MenuRow::plot(
        "8 days   \u{2588}\u{2587}\u{2586}\u{21ba}\u{2504}\u{2573}"
            .chars()
            .map(|ch| PlotCell {
                ch,
                ink: PlotInk::Read,
                busy: false,
            })
            .collect::<Vec<_>>(),
        "Week left over the last 8 days",
    ));
    let item = |glyph: &'static str, label: &str| MenuRow::Item {
        glyph,
        label: label.into(),
        spoken: label.into(),
        act: MenuAct::Fact(Fact::Account, FactAct::AccountSettings),
        enabled: true,
        checked: false,
        trail: String::new(),
    };
    rows.extend([
        MenuRow::Name("caf\u{e9} \u{4e2d}".into()),
        item("\u{2386}", "main  75%"),
        item("\u{1f600}\u{1f600}\u{1f600}", "wide"),
        MenuRow::Gap(7),
    ]);
    rows.extend(grouped_card());
    rows.push(fact("Token", "", NoteTone::Label));
    for row in &rows {
        let text = display_cells(&row.text());
        let want = match row {
            MenuRow::Item { .. } => text + CHECK_CELLS,
            MenuRow::Section(_) => text + 2,
            MenuRow::Gap(cells) => *cells,
            _ => text,
        };
        assert_eq!(row_width(row), want, "{row:?}");
    }
    let (width, allocations) = crate::work_counts::allocations_in(|| content_width(&rows));
    assert_eq!(allocations, 0, "a card's width allocates nothing");
    assert_eq!(
        width,
        TRAFFIC_MAX_CONTENT.min(rows.iter().map(row_width).max().unwrap_or(0))
    );
}

fn fact(label: &str, value: &str, tone: NoteTone) -> MenuRow {
    MenuRow::Fact {
        label: label.into(),
        value: value.into(),
        spoken: if label.is_empty() {
            String::new()
        } else {
            format!("{label}: {value}")
        },
        tone,
    }
}

fn trailed(label: &str, trail: &str, enabled: bool, checked: bool) -> MenuRow {
    MenuRow::Item {
        glyph: "\u{2386}",
        label: label.into(),
        spoken: format!("{label}, {trail}"),
        act: MenuAct::Fact(Fact::Account, FactAct::AccountSettings),
        enabled,
        checked,
        trail: trail.into(),
    }
}

/// The rows of [`grouped_card`], by index.
const GROUP_TOKEN: usize = 1;
const GROUP_BUDGET: usize = 2;
const GROUP_MACHINE: usize = 3;
const GROUP_CONTINUED: usize = 4;
const GROUP_HEADING: usize = 5;
const GROUP_CHECKED: usize = 6;
const GROUP_PLAIN: usize = 7;
const GROUP_DISABLED: usize = 8;

/// A card of groups: a header, a fact in each tone, a row that continues the last, a group
/// heading, and three choices with trailing words (one checked, one plain, one disabled).
fn grouped_card() -> Vec<MenuRow> {
    vec![
        MenuRow::Header("Account \u{b7} tab 2".into()),
        fact("Token", "none in the vault", NoteTone::Value),
        fact("Budget", "out until Sat 10:00", NoteTone::Warn),
        fact("Machine", "use there not known:", NoteTone::Label),
        fact("", "no other Mac is read", NoteTone::Label),
        MenuRow::Section("Switch this Mac to".into()),
        trailed("alpha@example.test", "61% left", true, true),
        trailed("beta@example.test", "\u{2264}88% left", true, false),
        trailed("gamma@example.test", "not signed in", false, false),
    ]
}

fn every_shipped_theme() -> Vec<(String, aterm_render::Theme)> {
    let builtins = aterm_types::scheme::builtin_names()
        .into_iter()
        .filter_map(|name| Some((name.to_owned(), aterm_types::scheme::builtin(name)?)));
    let packs = aterm_theme_pack::themes().iter().map(|theme| {
        let scheme = aterm_types::scheme::parse_scheme_str(theme.text).expect("a pack theme");
        (theme.id.to_owned(), scheme)
    });
    builtins
        .chain(packs)
        .map(|(name, scheme)| (name, aterm_render::Theme::from(&scheme)))
        .collect()
}

/// A GROUP HEADING AND A NAMED FACT ON THE CELL CARD. A heading is its title, bold in the value
/// ink, a blank, then the rule to the text's end, inside the border on a glass card and a plain
/// one alike. A fact is its name in the label ink from the first text cell and its value from
/// the name column's end ([`FACT_LABEL_CELLS`]) in its tone's ink, the warn one bold; a row that
/// continues a fact leaves the name column blank. On every shipped theme and every glass the
/// name's ink reads at 4.5:1 on the card.
#[test]
fn a_group_heading_and_a_fact_paint_their_title_rule_and_two_columns() {
    let rows = grouped_card();
    let rect = MenuRect {
        row: 0,
        col: 0,
        w: 48,
        h: rows.len() + 2,
    };
    let (text_start, text_end) = text_span(rect.w, &rows);
    assert_eq!((text_start, text_end), (2, 46), "padded: the card has room");
    let value_at = text_start + FACT_LABEL_CELLS;
    let said = |cells: &[RenderCell]| cells.iter().map(|c| c.ch).collect::<String>();
    let themes = every_shipped_theme();
    assert!(themes.len() > 37, "{} themes swept", themes.len());
    for (name, theme) in themes {
        for palette in [
            None,
            Some(CardPalette::Cruise),
            Some(CardPalette::Land),
            Some(CardPalette::Off),
        ] {
            let at = format!("{name} {palette:?}");
            let (c, inks) = card_inks(theme, palette);
            let painted = paint(rect, &rows, MenuViewport::default(), None, theme, palette);
            assert_eq!(painted.len(), rect.h, "{at}");
            let line = |i: usize| &painted[1 + i];
            assert!(
                crate::native_appearance::contrast_ratio(c.band.label, c.card_bg) >= 4.5,
                "{at}: a fact's name reads on the card"
            );

            let heading = line(GROUP_HEADING);
            let title = "Switch this Mac to";
            let rule_from = text_start + title.len() + 1;
            assert_eq!(said(&heading[text_start..rule_from - 1]), title, "{at}");
            for cell in &heading[text_start..rule_from - 1] {
                assert_eq!((cell.fg, cell.bold), (c.band.value, true), "{at}");
            }
            assert_eq!(heading[rule_from - 1].ch, ' ', "{at}: one blank");
            for cell in &heading[rule_from..text_end] {
                assert_eq!((cell.ch, cell.fg), ('\u{2500}', c.rule), "{at}");
            }
            assert_eq!(
                (heading[0].ch, heading[rect.w - 1].ch, heading[text_end].ch),
                ('\u{2502}', '\u{2502}', ' '),
                "{at}: the rule stays inside the border"
            );

            for (i, label, value, ink, bold) in [
                (
                    GROUP_TOKEN,
                    "Token",
                    "none in the vault",
                    c.band.value,
                    false,
                ),
                (
                    GROUP_BUDGET,
                    "Budget",
                    "out until Sat 10:00",
                    unpack(inks.warn()),
                    true,
                ),
                (
                    GROUP_MACHINE,
                    "Machine",
                    "use there not known:",
                    c.band.label,
                    false,
                ),
                (
                    GROUP_CONTINUED,
                    "",
                    "no other Mac is read",
                    c.band.label,
                    false,
                ),
            ] {
                let cells = line(i);
                assert_eq!(
                    said(&cells[text_start..value_at]).trim_end(),
                    label,
                    "{at}: the name column"
                );
                for cell in &cells[text_start..text_start + label.len()] {
                    assert_eq!((cell.fg, cell.bold), (c.band.label, false), "{at} {label}");
                }
                let end = value_at + value.len();
                assert_eq!(said(&cells[value_at..end]), value, "{at}");
                for cell in &cells[value_at..end] {
                    assert_eq!((cell.fg, cell.bold), (ink, bold), "{at} {label}");
                }
                assert!(cells.iter().all(|cell| cell.bg == c.card_bg), "{at}");
            }
        }
    }
}

/// AN ITEM'S TRAILING WORDS: right-aligned to one edge, the cell a check begins at, on a
/// checked row and an unchecked one alike, in the row's own ink: the selection's on the
/// highlighted row, the disabled ink on a row that cannot be chosen. In a card too narrow for
/// both, the label gives way and the trail stays whole.
#[test]
fn an_items_trail_ends_where_a_check_begins_in_the_rows_own_ink() {
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let rows = grouped_card();
    let said = |cells: &[RenderCell]| cells.iter().map(|c| c.ch).collect::<String>();
    let rect = MenuRect {
        row: 0,
        col: 0,
        w: 48,
        h: rows.len() + 2,
    };
    let (text_start, text_end) = text_span(rect.w, &rows);
    let edge = text_end - CHECK_CELLS;
    let (c, _) = card_inks(theme, Some(CardPalette::Cruise));
    let painted = paint(
        rect,
        &rows,
        MenuViewport::default(),
        Some(GROUP_PLAIN),
        theme,
        Some(CardPalette::Cruise),
    );
    for (i, label, trail, (fg, bg, bold)) in [
        (
            GROUP_CHECKED,
            "\u{2386}  alpha@example.test",
            "61% left",
            (c.band.value, c.card_bg, false),
        ),
        (
            GROUP_PLAIN,
            "\u{2386}  beta@example.test",
            "\u{2264}88% left",
            (c.sel_fg, c.sel_bg, true),
        ),
        (
            GROUP_DISABLED,
            "\u{2386}  gamma@example.test",
            "not signed in",
            (c.disabled, c.card_bg, false),
        ),
    ] {
        let cells = &painted[1 + i];
        let from = edge - trail.chars().count();
        assert_eq!(said(&cells[from..edge]), trail, "row {i}");
        for cell in &cells[from..edge] {
            assert_eq!((cell.fg, cell.bg, cell.bold), (fg, bg, bold), "row {i}");
        }
        assert_eq!(
            said(&cells[text_start..from]).trim_end(),
            label,
            "row {i}: the label, whole"
        );
        assert_eq!(
            cells[text_end - 1].ch,
            if i == GROUP_CHECKED { '\u{25cf}' } else { ' ' },
            "row {i}: the check's own cell"
        );
    }

    let narrow = MenuRect { w: 30, ..rect };
    let (text_start, text_end) = text_span(narrow.w, &rows);
    assert_eq!((text_start, text_end), (1, 29), "no room: no padding");
    let painted = paint(narrow, &rows, MenuViewport::default(), None, theme, None);
    let cells = &painted[1 + GROUP_DISABLED];
    let trail = "not signed in";
    let from = text_end - CHECK_CELLS - trail.len();
    assert_eq!(said(&cells[from..text_end - CHECK_CELLS]), trail);
    let label = said(&cells[text_start..from]);
    assert!(
        label.starts_with("\u{2386}  gamma") && label.trim_end().ends_with('\u{2026}'),
        "{label:?}"
    );
    assert!(
        label.ends_with("  "),
        "{label:?}: two blanks before the trail"
    );
}

/// WHAT A GROUP HEADING, A FACT AND A TRAIL ARE TO THE CARD'S MODEL. A heading keeps its place
/// while its title is the same and a fact while its name is, whatever its value, so a card
/// built again with a new figure moves no row; a row that left leaves a gap of its width. An
/// item whose trail changed is the same place and another choice. A row's reader identity is a
/// heading's title, a fact's name, value and tone, and an item's trail. Neither a heading nor a
/// fact is a row to choose, and a card opens in no fewer rows than stand before its first
/// heading.
#[test]
fn a_group_heading_a_fact_and_a_trail_in_the_cards_model() {
    use std::hash::{Hash, Hasher};
    let hash = |row: &MenuRow| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        row.hash(&mut h);
        h.finish()
    };
    let shown = grouped_card();
    assert!(
        shown[..=GROUP_HEADING].iter().all(|row| !row.selectable()),
        "a heading and a fact are no choice"
    );
    assert_eq!(
        shown[GROUP_PLAIN].text(),
        "\u{2386}  beta@example.test  \u{2264}88% left"
    );
    assert_eq!(shown[GROUP_TOKEN].text(), "Token    none in the vault");
    assert_eq!(
        shown[GROUP_CONTINUED].text(),
        "         no other Mac is read"
    );
    assert_eq!(fact("Token", "", NoteTone::Label).text(), "Token");
    assert_eq!(
        fact("A long name", "its value", NoteTone::Label).text(),
        "A long name its value",
        "a name past its column keeps a blank before the value"
    );

    let mut fresh = shown.clone();
    fresh[GROUP_TOKEN] = fact("Token", "in the vault", NoteTone::Value);
    fresh[GROUP_PLAIN] = trailed("beta@example.test", "out", true, false);
    assert!(same_place(&shown[GROUP_TOKEN], &fresh[GROUP_TOKEN]));
    assert!(same_place(&shown[GROUP_PLAIN], &fresh[GROUP_PLAIN]));
    assert!(
        !same_choice(&shown[GROUP_PLAIN], &fresh[GROUP_PLAIN]),
        "another trail is another choice"
    );
    assert!(same_choice(&shown[GROUP_PLAIN], &shown[GROUP_PLAIN]));
    assert_eq!(
        keep_places(&shown, &fresh).as_deref(),
        Some(&fresh[..]),
        "a new value and a new trail move no row"
    );
    for i in [GROUP_TOKEN, GROUP_PLAIN] {
        assert_ne!(hash(&shown[i]), hash(&fresh[i]), "row {i} reads anew");
    }

    let mut left = shown.clone();
    left.remove(GROUP_BUDGET);
    let laid = keep_places(&shown, &left).expect("only a row left");
    assert_eq!(
        laid[GROUP_BUDGET],
        MenuRow::Gap(row_width(&shown[GROUP_BUDGET]))
    );
    assert_eq!(laid[GROUP_HEADING], shown[GROUP_HEADING]);

    let mut retitled = shown.clone();
    retitled[GROUP_HEADING] = MenuRow::Section("Switch to".into());
    assert!(!same_place(&shown[GROUP_HEADING], &retitled[GROUP_HEADING]));
    assert_ne!(hash(&shown[GROUP_HEADING]), hash(&retitled[GROUP_HEADING]));
    assert!(!same_place(
        &shown[GROUP_TOKEN],
        &fact("Login", "none in the vault", NoteTone::Value)
    ));
    assert!(!same_place(&shown[GROUP_HEADING], &MenuRow::Rule));
    assert_ne!(
        hash(&shown[GROUP_TOKEN]),
        hash(&fact("Token", "none in the vault", NoteTone::Warn)),
        "a tone is read"
    );
    let MenuRow::Fact {
        label, value, tone, ..
    } = shown[GROUP_TOKEN].clone()
    else {
        unreachable!("a fact")
    };
    let respoken = MenuRow::Fact {
        label,
        value,
        tone,
        spoken: "said another way".into(),
    };
    assert_eq!(
        hash(&shown[GROUP_TOKEN]),
        hash(&respoken),
        "as a note's: its words are its identity"
    );

    assert_eq!(least_rows(&shown), GROUP_HEADING + 2, "its head and border");
    let mut ruled = shown.clone();
    ruled.insert(2, MenuRow::Rule);
    assert_eq!(least_rows(&ruled), 2 + 2, "the first rule or heading");
}

/// A GROUPED CARD AS A CLIENT AND A SCREEN READER READ IT. `controls fact-menu` gives a heading
/// a `section` line and a fact a `fact` line with its name, tone and value, and an item's
/// `text=` carries its trail. The menu's name folds each heading's title and each fact's words
/// in, in row order; a row that continues a fact adds nothing.
#[test]
fn a_grouped_card_is_read_by_controls_and_named_in_row_order() {
    let (mut app, wid, _session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    open_card(&mut app);
    let before = app.autopilot_menu.as_ref().unwrap().rows.len();
    relay(&mut app, wid, |rows| {
        rows.extend(grouped_card().into_iter().skip(GROUP_BUDGET));
    });
    frame(&mut app, wid);
    let at = |i: usize| before + i - GROUP_BUDGET;
    let lines = app.autopilot_menu.as_ref().unwrap().controls_lines("7");
    let line = |i: usize| lines[1 + at(i)].clone();
    assert_eq!(
        line(GROUP_BUDGET),
        format!(
            "fact-menu fact index={} label=\"Budget\" tone=warn text=\"out until Sat 10:00\"",
            at(GROUP_BUDGET)
        )
    );
    assert_eq!(
        line(GROUP_MACHINE),
        format!(
            "fact-menu fact index={} label=\"Machine\" tone=label text=\"use there not known:\"",
            at(GROUP_MACHINE)
        )
    );
    assert_eq!(
        line(GROUP_CONTINUED),
        format!(
            "fact-menu fact index={} label=\"\" tone=label text=\"no other Mac is read\"",
            at(GROUP_CONTINUED)
        )
    );
    assert_eq!(
        line(GROUP_HEADING),
        format!(
            "fact-menu section index={} text=\"Switch this Mac to\"",
            at(GROUP_HEADING)
        )
    );
    assert_eq!(
        line(GROUP_CHECKED),
        format!(
            "fact-menu item index={} key=fact-menu/account/account-settings enabled=true \
             checked=true selected=false text=\"\u{2386}  alpha@example.test  61% left\"",
            at(GROUP_CHECKED)
        )
    );
    #[cfg(a11y_tree)]
    {
        let menu = app.autopilot_grid_menu(wid).expect("painted");
        assert!(
            menu.label.ends_with(
                ", Budget: out until Sat 10:00, Machine: use there not known:, Switch this Mac to"
            ),
            "{}",
            menu.label
        );
        let said: Vec<&str> = menu.items.iter().map(|item| item.label.as_str()).collect();
        assert!(
            said.ends_with(&[
                "alpha@example.test, 61% left",
                "beta@example.test, \u{2264}88% left",
                "gamma@example.test, not signed in"
            ]),
            "{said:?}"
        );
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::menu::MenuAction;

fn model() -> Vec<TabMenuEntry> {
    vec![
        TabMenuEntry::Header("build agent".into()),
        TabMenuEntry::Separator,
        TabMenuEntry::Header("spawned \u{00B7} 4m".into()),
        TabMenuEntry::Separator,
        TabMenuEntry::Action {
            label: "Rename Session\u{2026}".into(),
            action: MenuAction::RenameSession,
            enabled: true,
        },
        TabMenuEntry::Action {
            label: "Copy Session ID".into(),
            action: MenuAction::CopySessionId,
            enabled: true,
        },
        TabMenuEntry::Action {
            label: "Copy CWD".into(),
            action: MenuAction::CopyCwd,
            enabled: false,
        },
        TabMenuEntry::Action {
            label: "Close Tab".into(),
            action: MenuAction::CloseTab,
            enabled: true,
        },
    ]
}

#[test]
fn card_hangs_under_the_strip_at_the_clicked_column() {
    let m = model();
    let rect = place(&m, 7, 100, 30, 1).expect("a 100x30 frame hosts a menu");
    assert_eq!(
        rect.row, 1,
        "the card starts on the first row BELOW the band"
    );
    assert_eq!(rect.col, 7, "and its left edge is the clicked column");
    assert_eq!(rect.h, m.len() + 2, "every entry plus two border rows");
    assert!(rect.w >= "Rename Session\u{2026}".chars().count() + 4);
}

#[test]
fn a_card_near_the_right_edge_shifts_left_instead_of_hanging_off() {
    let m = model();
    let rect = place(&m, 96, 100, 30, 1).expect("still placeable");
    assert_eq!(
        rect.col + rect.w,
        100,
        "the card is flush with the right edge, whole"
    );
    assert!(rect.col < 96, "which means it moved left of the anchor");
}

#[test]
fn a_short_frame_truncates_rather_than_flipping_above_the_band() {
    let m = model();
    let rect = place(&m, 0, 100, 6, 1).expect("placeable in a 6-row frame");
    assert_eq!(rect.row, 1);
    assert_eq!(rect.h, 5, "clamped to the rows below the band");
    assert_eq!(visible_entries(rect), 3);
    assert_eq!(entry_at_row(rect, 4), Some(2), "the last shown entry");
    assert_eq!(
        entry_at_row(rect, 5),
        None,
        "the bottom edge shows no entry"
    );
}

#[test]
fn a_frame_with_no_room_declines_to_open() {
    let m = model();
    assert_eq!(place(&m, 0, 3, 30, 1), None, "too narrow for a border pair");
    assert_eq!(place(&m, 0, 100, 3, 2), None, "no rows below the band");
    assert_eq!(place(&[], 0, 100, 30, 1), None, "an empty model is no menu");
}

/// A click at a pixel-derived cell resolves through the painter's rect to the same
/// model entry, carrying the action the dispatcher runs.
#[test]
fn hit_test_resolves_to_the_models_own_action() {
    let m = model();
    let rect = place(&m, 4, 100, 30, 1).expect("placeable");
    let copy_id_row = rect.row + 1 + 5;
    let hit = action_at(rect, &m, copy_id_row, rect.col + 3).expect("an action row");
    assert_eq!(hit, 5);
    match &m[hit] {
        TabMenuEntry::Action { action, label, .. } => {
            assert_eq!(*action, MenuAction::CopySessionId);
            assert_eq!(label, "Copy Session ID");
        }
        other => panic!("expected an action, got {other:?}"),
    }
}

#[test]
fn borders_headers_separators_and_disabled_rows_are_not_activatable() {
    let m = model();
    let rect = place(&m, 4, 100, 30, 1).expect("placeable");
    let at = |i: usize| action_at(rect, &m, rect.row + 1 + i, rect.col + 3);
    assert_eq!(at(0), None, "a header is inert");
    assert_eq!(at(1), None, "a separator is inert");
    assert_eq!(at(6), None, "Copy CWD is disabled here");
    assert_eq!(at(7), Some(7), "Close Tab is live");
    assert_eq!(
        action_at(rect, &m, rect.row, rect.col + 3),
        None,
        "the top border activates nothing"
    );
    assert_eq!(
        action_at(rect, &m, rect.row + 1, rect.col),
        None,
        "nor the left rule"
    );
    for (r, cc) in [
        (rect.row, rect.col + 3),
        (rect.row + 1, rect.col),
        (rect.row + 1 + 6, rect.col + 3),
    ] {
        assert!(rect.contains(r, cc), "({r},{cc}) is on the card");
    }
    assert!(
        !rect.contains(rect.row + rect.h, rect.col),
        "one past is off"
    );
}

#[test]
fn keyboard_stepping_visits_only_enabled_actions_and_wraps() {
    let m = model();
    let limit = m.len();
    assert_eq!(first_selectable(&m, limit), Some(4), "Rename is the first");
    let mut at = first_selectable(&m, limit);
    at = step_selectable(&m, at, true, limit);
    assert_eq!(at, Some(5), "Copy Session ID");
    at = step_selectable(&m, at, true, limit);
    assert_eq!(at, Some(7), "Copy CWD is disabled — skipped");
    at = step_selectable(&m, at, true, limit);
    assert_eq!(at, Some(4), "and it wraps to the top");
    at = step_selectable(&m, at, false, limit);
    assert_eq!(at, Some(7), "backwards wraps to the bottom");
    assert_eq!(
        step_selectable(&m, None, true, 4),
        None,
        "a card truncated above the actions highlights nothing"
    );
}

#[test]
fn paint_is_exactly_the_rect_and_marks_the_highlighted_row() {
    let m = model();
    let theme = Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let rect = place(&m, 2, 80, 24, 1).expect("placeable");
    let rows = paint(rect, &m, Some(7), theme);
    assert_eq!(rows.len(), rect.h);
    for row in &rows {
        assert_eq!(row.len(), rect.w, "every row is exactly the card width");
    }
    assert_eq!(rows[0][0].ch, '\u{250C}');
    assert_eq!(rows[0][rect.w - 1].ch, '\u{2510}');
    assert_eq!(rows[rect.h - 1][0].ch, '\u{2514}');
    assert_eq!(rows[1 + 1][0].ch, '\u{251C}', "entry 1 is a separator");
    let close = &rows[1 + 7];
    let text: String = close.iter().map(|c| c.ch).collect();
    assert!(text.contains("Close Tab"), "got {text:?}");
    let sel_bg = close[3].bg;
    let plain_bg = rows[1 + 5][3].bg;
    assert_ne!(sel_bg, plain_bg, "the highlighted row takes its own ground");
    assert!(close[3].bold, "and its ink is emphasised");
}

#[test]
fn a_long_header_ellipsizes_instead_of_widening_the_card() {
    let mut m = model();
    m[0] = TabMenuEntry::Header("x".repeat(400));
    let rect = place(&m, 0, 200, 30, 1).expect("placeable");
    assert!(
        rect.w <= MAX_CONTENT + 2 + 2 * H_PAD,
        "the card is capped at {MAX_CONTENT} content cells, got {}",
        rect.w
    );
    let theme = Theme {
        fg: 0x00FF_FFFF,
        bg: 0x0000_0000,
        cursor: 0x00FF_FFFF,
        selection: 0x0033_3333,
    };
    let rows = paint(rect, &m, None, theme);
    let header: String = rows[1].iter().map(|c| c.ch).collect();
    assert!(header.contains('\u{2026}'), "got {header:?}");
}

#[test]
fn a_wide_glyph_paints_a_real_lead_and_continuation() {
    let m = vec![TabMenuEntry::Header("\u{4F60}\u{597D} agent".into())];
    let theme = Theme {
        fg: 0x00FF_FFFF,
        bg: 0x0000_0000,
        cursor: 0x00FF_FFFF,
        selection: 0x0033_3333,
    };
    let rect = place(&m, 0, 60, 20, 1).expect("placeable");
    let rows = paint(rect, &m, None, theme);
    let row = &rows[1];
    assert_eq!(row[1 + H_PAD].ch, '\u{4F60}');
    assert!(!row[1 + H_PAD].wide, "the lead carries the glyph");
    assert!(row[2 + H_PAD].wide, "and its continuation is flagged");
    assert_eq!(row[2 + H_PAD].ch, ' ', "the continuation has no glyph");
}

/// NEGATIVE CONTROL: closed is `None`, open is `Some(...)`, and `highlight` is compared structurally.
#[test]
fn fingerprint_is_zero_closed_nonzero_open_and_moves_with_the_highlight() {
    let rect = place(&model(), 0, 80, 24, 1);
    assert_eq!(fingerprint(None, rect), None, "closed is None");
    let mut menu = TabMenu {
        tab: crate::tab_model::TabId::from_stored(1),
        index: 0,
        entries: model(),
        anchor_col: 0,
        highlight: None,
        keyboard: false,
    };
    let open = fingerprint(Some(&menu), rect);
    assert_ne!(open, None);
    menu.highlight = Some(4);
    let lit = fingerprint(Some(&menu), rect);
    assert_ne!(open, lit, "a highlight step must force the repaint");
    menu.highlight = None;
    assert_eq!(fingerprint(Some(&menu), rect), open, "and it is stable");
}

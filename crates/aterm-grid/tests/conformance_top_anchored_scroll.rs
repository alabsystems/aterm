// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding for top-anchored partial-scroll history retention.

use std::collections::BTreeMap;

use aterm_grid::{AbsoluteRowUpdate, Grid};
use aterm_selection::{SelectionSide, SelectionType, TextSelection};
use aterm_spec::derive::{Model, top_anchored_scroll_history_model};
use aterm_spec::verify;

const ROWS: u16 = 5;
const COLS: u16 = 5;

fn seeded_grid(history_enabled: bool) -> Grid {
    let mut grid = Grid::with_scrollback(ROWS, COLS, usize::from(history_enabled) * 10);
    for row in 0..ROWS {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid
}

fn validate_scroll(
    model: &Model,
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
) -> (bool, String) {
    verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        prev,
        next,
        Some("Scroll"),
        "top-anchored partial-scroll Tier-1 binding",
    )
}

fn real_scroll_transition(
    choice: &'static str,
    top: u16,
    full_width: bool,
    history_enabled: bool,
) -> (BTreeMap<&'static str, i64>, BTreeMap<&'static str, i64>) {
    let model = top_anchored_scroll_history_model();
    let mut prev = model.init_state();
    assert!(model.fire(choice, &mut prev));

    let mut grid = seeded_grid(history_enabled);
    grid.set_scroll_region(top, 2 + top);
    let history_before = grid.scrollback_lines();
    if full_width {
        grid.scroll_region_up(1);
    } else {
        // DECLRMM-style rectangle: top-anchored vertically, but not full-width.
        grid.scroll_region_up_margined(1, 1, COLS - 2);
    }

    let history_delta = grid.scrollback_lines() - history_before;
    let footer_anchor_delta = match grid.take_absolute_row_update() {
        Some(AbsoluteRowUpdate::Splice { inserted, .. }) => inserted as i64,
        Some(AbsoluteRowUpdate::Invalidate) => -1,
        None => 0,
    };
    let footer_preserved = grid
        .cell(ROWS - 1, 0)
        .is_some_and(|cell| cell.char() == 'E');
    let mut selection = TextSelection::new();
    selection.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(4, 0, SelectionSide::Right);
    selection.complete_selection();
    let selection_update = grid.take_selection_row_update();
    let selection_delta = grid.take_content_scroll_delta();
    match (selection_update, selection_delta) {
        (Some(AbsoluteRowUpdate::Splice { at, inserted }), 0) => {
            let new_live_top = grid
                .absolute_row_counter()
                .saturating_sub(u64::from(grid.rows()));
            let old_live_top = new_live_top.checked_sub(inserted).unwrap();
            let boundary = i32::try_from(at.checked_sub(old_live_top).unwrap()).unwrap();
            assert!(selection.adjust_for_row_splice(
                boundary,
                i32::try_from(inserted).unwrap(),
                i32::from(grid.rows()),
                i32::try_from(grid.scrollback_lines()).unwrap(),
            ));
        }
        (None, delta) => {
            selection.adjust_for_scroll(
                delta,
                i32::from(grid.rows()),
                i32::try_from(grid.scrollback_lines()).unwrap(),
            );
        }
        (Some(_), _) => selection.clear(),
    }
    let mut next = prev.clone();
    next.insert("phase", 2);
    next.insert("history_len", history_delta as i64);
    next.insert("footer", i64::from(footer_preserved));
    next.insert("footer_anchor", footer_anchor_delta);
    next.insert("selection_alive", i64::from(selection.has_selection()));
    next.insert("selection_region_row", i64::from(selection.start().row));
    next.insert("selection_footer_row", i64::from(selection.end().row));

    grid.assert_structural_invariants();
    (prev, next)
}

#[test]
fn real_grid_top_anchored_scroll_regimes_conform_to_derived_model() {
    let model = top_anchored_scroll_history_model();
    for (choice, top, full_width, history_enabled, expected_history) in [
        ("ChooseArchival", 0, true, true, 1),
        ("ChooseInterior", 1, true, true, 0),
        ("ChooseMargined", 0, false, true, 0),
        ("ChooseEphemeral", 0, true, false, 0),
    ] {
        let (prev, next) = real_scroll_transition(choice, top, full_width, history_enabled);
        assert_eq!(next["history_len"], expected_history, "{choice}");
        assert_eq!(next["footer"], 1, "{choice}");
        assert_eq!(next["footer_anchor"], expected_history, "{choice}");
        let (accepted, diagnostic) = validate_scroll(&model, &prev, &next);
        assert!(
            accepted,
            "model rejected real {choice} transition\nprev={prev:?}\nnext={next:?}\n{diagnostic}"
        );
    }

    // Negative controls: the model must reject both the historical silent drop
    // and corruption of a row below the scrolling margin.
    let (prev, next) = real_scroll_transition("ChooseArchival", 0, true, true);
    let mut dropped = next.clone();
    dropped.insert("history_len", 0);
    assert!(!validate_scroll(&model, &prev, &dropped).0);

    let mut footer_corrupted = next;
    footer_corrupted.insert("footer", 0);
    assert!(!validate_scroll(&model, &prev, &footer_corrupted).0);

    let (prev, mut anchor_lost) = real_scroll_transition("ChooseArchival", 0, true, true);
    anchor_lost.insert("footer_anchor", 0);
    assert!(!validate_scroll(&model, &prev, &anchor_lost).0);

    let (prev, mut old_clear_all) = real_scroll_transition("ChooseArchival", 0, true, true);
    old_clear_all.insert("selection_alive", 0);
    old_clear_all.insert("selection_region_row", 2);
    assert!(
        !validate_scroll(&model, &prev, &old_clear_all).0,
        "negative control: the old generic region clear must be rejected"
    );

    let (prev, mut footer_shifted) = real_scroll_transition("ChooseArchival", 0, true, true);
    footer_shifted.insert("selection_footer_row", 3);
    assert!(!validate_scroll(&model, &prev, &footer_shifted).0);
}

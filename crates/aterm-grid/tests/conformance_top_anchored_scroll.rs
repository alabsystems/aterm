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

/// [`seeded_grid`] at an explicit height. SELECTION CUSTODY Phase 4 needs a grid tall
/// enough to hold an INTERIOR scroll region that does not overlap the fixture
/// selection at rows 2..4; the default 5-row grid admits no such region, because a
/// legal region needs `bottom > top` and anything interior reaches row 2.
fn seeded_grid_rows(history_enabled: bool, rows: u16) -> Grid {
    let mut grid = Grid::with_scrollback(rows, COLS, usize::from(history_enabled) * 10);
    for row in 0..rows {
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
    real_scroll_transition_in_region(choice, top, 2 + top, full_width, history_enabled, ROWS)
}

/// [`real_scroll_transition`] with an explicit region BOTTOM.
///
/// SELECTION CUSTODY Phase 4 needs a regime where the scrolled region does not
/// overlap the fixture selection (rows 2..4), which the default `top..top+2` region
/// always does. An interior single-row region at row 1 is the only disjoint shape
/// this 5-row grid admits while still being interior (`top != 0`).
fn real_scroll_transition_in_region(
    choice: &'static str,
    top: u16,
    bottom: u16,
    full_width: bool,
    history_enabled: bool,
    rows: u16,
) -> (BTreeMap<&'static str, i64>, BTreeMap<&'static str, i64>) {
    let model = top_anchored_scroll_history_model();
    let mut prev = model.init_state();
    assert!(model.fire(choice, &mut prev));

    let mut grid = seeded_grid_rows(history_enabled, rows);
    grid.set_scroll_region(top, bottom);
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
        .cell(rows - 1, 0)
        .is_some_and(|cell| cell.char() == (b'A' + (rows - 1) as u8) as char);
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
    // SELECTION CUSTODY Phase 4: mirror `Terminal::post_process`'s damage test, which
    // runs after the geometric transform above. Without this the binding would model
    // a `post_process` that no longer exists, and would accept a stale highlight over
    // rewritten rows.
    match grid.take_selection_damage() {
        aterm_grid::SelectionDamage::None => {}
        aterm_grid::SelectionDamage::All => selection.clear(),
        aterm_grid::SelectionDamage::Band { lo_abs, hi_abs } => {
            let live_top_abs = grid
                .absolute_row_counter()
                .saturating_sub(u64::from(grid.rows()));
            if selection.intersects_absolute_band(live_top_abs, lo_abs, hi_abs) {
                selection.clear();
            }
        }
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
        ("ChooseArchivalOverlapping", 0, true, true, 1),
        ("ChooseInteriorOverlapping", 1, true, true, 0),
        ("ChooseMarginedOverlapping", 0, false, true, 0),
        ("ChooseEphemeralOverlapping", 0, true, false, 0),
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
    let (prev, next) = real_scroll_transition("ChooseArchivalOverlapping", 0, true, true);
    let mut dropped = next.clone();
    dropped.insert("history_len", 0);
    assert!(!validate_scroll(&model, &prev, &dropped).0);

    let mut footer_corrupted = next;
    footer_corrupted.insert("footer", 0);
    assert!(!validate_scroll(&model, &prev, &footer_corrupted).0);

    let (prev, mut anchor_lost) =
        real_scroll_transition("ChooseArchivalOverlapping", 0, true, true);
    anchor_lost.insert("footer_anchor", 0);
    assert!(!validate_scroll(&model, &prev, &anchor_lost).0);

    let (prev, mut old_clear_all) =
        real_scroll_transition("ChooseArchivalOverlapping", 0, true, true);
    old_clear_all.insert("selection_alive", 0);
    old_clear_all.insert("selection_region_row", 2);
    assert!(
        !validate_scroll(&model, &prev, &old_clear_all).0,
        "negative control: the old generic region clear must be rejected"
    );

    let (prev, mut footer_shifted) =
        real_scroll_transition("ChooseArchivalOverlapping", 0, true, true);
    footer_shifted.insert("selection_footer_row", 3);
    assert!(!validate_scroll(&model, &prev, &footer_shifted).0);

    // SELECTION CUSTODY Phase 4 — the DISJOINT regime, bound to the real grid.
    //
    // An interior region at row 1 scrolls; the selection sits at rows 2..4, which the
    // region never touches. Before the damage lattice the grid set
    // `content_scroll_delta = i32::MAX` here and the selection died — the reported
    // bug. Now the band is row 1 alone and the highlight survives.
    let (prev, next) =
        real_scroll_transition_in_region("ChooseInteriorDisjoint", 6, 8, true, true, 10);
    assert_eq!(
        next["selection_alive"], 1,
        "a real interior scroll must spare a selection outside its rows"
    );
    assert_eq!(next["selection_region_row"], 2, "…and must not remap it");
    let (accepted, diagnostic) = validate_scroll(&model, &prev, &next);
    assert!(
        accepted,
        "model rejected the real disjoint interior transition\nprev={prev:?}\nnext={next:?}\n{diagnostic}"
    );

    // Negative control for the NEW direction: an OVER-clear — the grid killing a
    // selection its damage never reached — must be rejected just as firmly as the
    // historical under-retention above.
    let (prev, mut over_cleared) =
        real_scroll_transition_in_region("ChooseInteriorDisjoint", 6, 8, true, true, 10);
    over_cleared.insert("selection_alive", 0);
    assert!(
        !validate_scroll(&model, &prev, &over_cleared).0,
        "negative control: clearing a disjoint selection must be rejected"
    );
}

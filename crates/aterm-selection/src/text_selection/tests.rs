// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unit tests for text selection state machine.

use super::*;

#[test]
fn test_contains_simple() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    assert!(sel.contains(0, 5));
    assert!(sel.contains(0, 7));
    assert!(sel.contains(0, 10));
    assert!(!sel.contains(0, 4));
    assert!(!sel.contains(0, 11));
    assert!(!sel.contains(1, 7));
}

#[test]
fn test_contains_multiline() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 3, SelectionSide::Right);
    sel.complete_selection();

    // Row 0: from col 5 to end
    assert!(!sel.contains(0, 4));
    assert!(sel.contains(0, 5));
    assert!(sel.contains(0, 80)); // Full line selected after start

    // Row 1: full line
    assert!(sel.contains(1, 0));
    assert!(sel.contains(1, 80));

    // Row 2: from start to col 3
    assert!(sel.contains(2, 0));
    assert!(sel.contains(2, 3));
    assert!(!sel.contains(2, 4));
}

#[test]
fn test_contains_block() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 10, SelectionSide::Right);
    sel.complete_selection();

    // Rectangular region: rows 0-2, cols 5-10
    assert!(sel.contains(0, 5));
    assert!(sel.contains(1, 7));
    assert!(sel.contains(2, 10));
    assert!(!sel.contains(0, 4));
    assert!(!sel.contains(0, 11));
    assert!(!sel.contains(3, 7));
}

#[test]
fn test_extend_selection() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    // Shift-click to extend
    sel.extend_selection(2, 15, SelectionSide::Right);
    assert_eq!(sel.state(), SelectionState::InProgress);
    assert_eq!(sel.end().row, 2);
    assert_eq!(sel.end().col, 15);
}

#[test]
fn test_extend_selection_preserves_anchor_cell_when_crossing_left() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 3, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 7, SelectionSide::Right);
    sel.complete_selection();

    sel.extend_selection(0, 0, SelectionSide::Left);
    sel.complete_selection();

    let bounds = sel
        .side_adjusted_bounds()
        .expect("cross-anchor extension should remain non-empty");
    assert_eq!(bounds, (0, 0, 0, 3));
    assert_eq!(sel.start().side, SelectionSide::Right);
}

#[test]
fn test_extend_selection_preserves_anchor_cell_when_crossing_right() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 10, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(0, 5, SelectionSide::Left);
    sel.complete_selection();

    sel.extend_selection(0, 15, SelectionSide::Right);
    sel.complete_selection();

    let bounds = sel
        .side_adjusted_bounds()
        .expect("cross-anchor extension should remain non-empty");
    assert_eq!(bounds, (0, 10, 0, 15));
    assert_eq!(sel.start().side, SelectionSide::Left);
}

#[test]
fn test_anchor_ordering() {
    let a1 = SelectionAnchor::new(0, 5, SelectionSide::Left);
    let a2 = SelectionAnchor::new(0, 5, SelectionSide::Right);
    let a3 = SelectionAnchor::new(0, 6, SelectionSide::Left);
    let a4 = SelectionAnchor::new(1, 0, SelectionSide::Left);

    assert!(a1 < a2);
    assert!(a2 < a3);
    assert!(a3 < a4);
}

/// `adjust_for_scroll` shifts both anchors by `-delta` and keeps the selection
/// while every row stays in `-floor..max_rows` (`max_rows` is the live row
/// count, so row `max_rows` is one past the last visible row); a row past
/// either bound clears it. `i32::MAX` (the region-scroll sentinel) saturates
/// instead of overflowing.
#[test]
fn adjust_for_scroll_shifts_or_clears_at_the_bounds() {
    // (start row, end row, delta, max_rows, floor, rows after or None = cleared)
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: [(i32, i32, i32, i32, i32, Option<(i32, i32)>); 12] = [
        (5, 7, 2, 24, 23, Some((3, 5))),
        (5, 7, -3, 24, 23, Some((8, 10))),
        (5, 10, 8, 24, 23, Some((-3, 2))),
        (0, 0, 23, 24, 23, Some((-23, -23))),
        (0, 0, 24, 24, 23, None),
        (23, 23, 0, 24, 23, Some((23, 23))),
        (20, 20, -4, 24, 23, None),
        (23, 24, 0, 24, 23, None),
        (20, 22, -5, 24, 23, None),
        (1, 2, 100, 24, 23, None),
        (-5, -3, i32::MAX, 24, 23, None),
        (0, 0, 1, 1, 0, None),
    ];
    for (start, end, delta, max_rows, floor, want) in cases {
        let mut sel = TextSelection::new();
        sel.start_selection(start, 3, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(end, 10, SelectionSide::Right);
        sel.complete_selection();
        let at = format!("{start}..{end} delta {delta} max {max_rows} floor {floor}");
        let visible = sel.adjust_for_scroll(delta, max_rows, floor);
        assert_eq!(visible, want.is_some(), "{at}");
        assert_eq!(sel.has_selection(), want.is_some(), "{at}");
        if let Some((s, e)) = want {
            assert_eq!(
                (sel.normalized_start().row, sel.normalized_end().row),
                (s, e),
                "{at}"
            );
            assert_eq!(
                (sel.normalized_start().col, sel.normalized_end().col),
                (3, 10),
                "{at}"
            );
        }
    }
}

#[test]
fn test_translate_rows_for_presentation_preserves_selection_semantics() {
    let mut sel = TextSelection::new();
    sel.start_selection(7, 19, SelectionSide::Right, SelectionType::Semantic);
    sel.update_selection(-3, 2, SelectionSide::Left);
    sel.complete_selection();

    sel.translate_rows_for_presentation(11);

    assert_eq!(sel.start().row, 18);
    assert_eq!(sel.end().row, 8);
    assert_eq!(sel.start().col, 19);
    assert_eq!(sel.end().col, 2);
    assert_eq!(sel.start().side, SelectionSide::Right);
    assert_eq!(sel.end().side, SelectionSide::Left);
    assert_eq!(sel.selection_type(), SelectionType::Semantic);
    assert_eq!(sel.state(), SelectionState::Complete);
}

#[test]
fn test_translate_rows_for_presentation_saturates_at_i32_boundaries() {
    let mut upper = TextSelection::new();
    upper.start_selection(i32::MAX - 1, 3, SelectionSide::Left, SelectionType::Block);
    upper.update_selection(i32::MAX, 9, SelectionSide::Right);
    upper.translate_rows_for_presentation(2);
    assert_eq!(upper.start().row, i32::MAX);
    assert_eq!(upper.end().row, i32::MAX);
    assert_eq!((upper.start().col, upper.end().col), (3, 9));

    let mut lower = TextSelection::new();
    lower.start_selection(i32::MIN + 1, 4, SelectionSide::Right, SelectionType::Lines);
    lower.update_selection(i32::MIN, 10, SelectionSide::Left);
    lower.translate_rows_for_presentation(-2);
    assert_eq!(lower.start().row, i32::MIN);
    assert_eq!(lower.end().row, i32::MIN);
    assert_eq!((lower.start().col, lower.end().col), (4, 10));
}

#[test]
fn test_adjust_for_scroll_in_progress_selection() {
    // Scroll adjustment must work on InProgress selections too,
    // not just Complete ones.
    let mut sel = TextSelection::new();
    sel.start_selection(5, 3, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(7, 10, SelectionSide::Right);
    // Deliberately do NOT call complete_selection()
    assert!(sel.is_in_progress());

    let visible = sel.adjust_for_scroll(2, 24, 23);
    assert!(visible);
    assert_eq!(sel.normalized_start().row, 3); // 5 - 2
    assert_eq!(sel.normalized_end().row, 5); // 7 - 2
    assert!(sel.is_in_progress(), "state should remain InProgress");
}

#[test]
fn test_adjust_for_scroll_deep_scrollback_survives_then_evicts() {
    // A selection made deep in scrollback uses
    // live-screen coords where scrollback is negative. With the lower clear
    // bound derived from the visible height (-(max_rows-1) = -23), a single
    // line of background output would scroll the selection past -23 and wipe
    // it even though view pinning keeps the exact same text on screen.
    // The clear bound must instead be the retained-history floor (-floor).
    //
    // Scenario: 24-row screen, 500 lines of scrollback, selection anchored
    // ~40 lines back into history (rows -40 and -38).
    let max_rows = 24;
    let floor = 500;
    let mut sel = TextSelection::new();
    sel.start_selection(-40, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-38, 10, SelectionSide::Right);
    sel.complete_selection();

    // A single line of output (delta = 1) must NOT clear the selection:
    // -41 and -39 are still far above the history floor (-500).
    let visible = sel.adjust_for_scroll(1, max_rows, floor);
    assert!(
        visible,
        "deep-scrollback selection must survive a 1-line content scroll"
    );
    assert!(sel.has_selection());
    assert_eq!(sel.normalized_start().row, -41); // -40 - 1
    assert_eq!(sel.normalized_end().row, -39); // -38 - 1

    // Scrolling the START endpoint below the retained-history floor (its row would
    // drop to -41 - 460 = -501 < -500) evicts that endpoint's content — but NOT the
    // end's, which is still retained at -499: the head clamps onto the
    // oldest retained row and reports the loss.
    let visible = sel.adjust_for_scroll(460, max_rows, floor);
    assert!(
        visible,
        "one evicted endpoint is PARTIAL eviction: the retained half survives"
    );
    assert!(sel.has_selection());
    assert_eq!(
        sel.normalized_start().row,
        -500,
        "the evicted head clamps onto the oldest retained row"
    );
    assert_eq!(
        sel.normalized_start().col,
        0,
        "the clamped head starts at the beginning of that row"
    );
    assert_eq!(
        sel.normalized_end().row,
        -499,
        "the retained end does not move"
    );
    assert!(sel.truncated(), "the partial loss is reported, not hidden");
}

/// The evicted endpoint is not necessarily `start`: an upward drag leaves the older
/// row in `end`, and the clamp must follow the row order, not the field name.
#[test]
fn test_adjust_for_scroll_clamps_the_evicted_end_anchor_on_an_upward_drag() {
    let mut sel = TextSelection::new();
    sel.start_selection(-38, 10, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(-40, 4, SelectionSide::Right);
    sel.complete_selection();

    assert!(sel.adjust_for_scroll(461, 24, 500));
    assert_eq!(sel.start().row, -499, "the newer anchor rides the scroll");
    assert_eq!(
        sel.end().row,
        -500,
        "the older anchor clamps onto the floor"
    );
    assert_eq!(sel.end().col, 0);
    assert_eq!(
        sel.end().side,
        SelectionSide::Left,
        "a Right side on the clamped head would eat the first cell of the row"
    );
    assert!(sel.truncated());
}

/// A clamp that leaves nothing selectable is a TOTAL loss, not a partial one, and
/// must not survive as a selection nothing paints and nothing copies. The surviving
/// anchor here is itself `(min_row, 0, Left)`, so side adjustment retreats the end to
/// `(min_row - 1, u16::MAX)` and the span is empty the instant the head clamps onto it.
#[test]
fn test_adjust_for_scroll_clears_when_the_clamp_collapses_the_span() {
    let mut sel = TextSelection::new();
    sel.start_selection(-49, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-50, 0, SelectionSide::Left);
    sel.complete_selection();

    // delta 1 => -50 and -51 against a floor of 50: the older anchor clamps onto
    // -50, which is exactly where the surviving one already sits.
    assert!(!sel.adjust_for_scroll(1, 24, 50));
    assert!(
        !sel.has_selection(),
        "an empty clamped span is the honest total-loss clear, not a ghost selection"
    );
    assert!(!sel.truncated());
}

/// The truncation report is per-SPAN, not sticky: `aterm-control`'s conformance check
/// compares the `selection` reply header exactly, so a flag that outlived its
/// selection would append ` incomplete` to every later reply.
#[test]
fn test_truncated_resets_on_new_selection_and_on_clear() {
    let mut sel = TextSelection::new();
    sel.start_selection(-40, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-38, 10, SelectionSide::Right);
    sel.complete_selection();
    assert!(sel.adjust_for_scroll(461, 24, 500));
    assert!(sel.truncated());

    sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
    assert!(!sel.truncated(), "a fresh span has lost nothing");

    sel.update_selection(4, 4, SelectionSide::Right);
    sel.complete_selection();
    assert!(sel.adjust_for_scroll(503, 24, 500));
    assert!(sel.truncated());
    sel.clear();
    assert!(!sel.truncated(), "no span left to be partial");
}

#[test]
fn test_adjust_for_row_splice_moves_history_endpoint_but_keeps_footer_endpoint() {
    let mut sel = TextSelection::new();
    sel.start_selection(1, 3, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(4, 10, SelectionSide::Right);
    sel.complete_selection();

    let mut old_clear_all = sel.clone();
    assert!(!old_clear_all.adjust_for_scroll(i32::MAX, 5, 8));
    assert!(
        !old_clear_all.has_selection(),
        "negative control reproduces the old generic region-scroll clear"
    );

    assert!(sel.adjust_for_row_splice(3, 1, 5, 8));
    assert_eq!(
        sel.start().row,
        0,
        "scroll-region content moves toward history"
    );
    assert_eq!(
        sel.end().row,
        4,
        "protected-footer content stays on its screen row"
    );
    assert_eq!(sel.start().col, 3);
    assert_eq!(sel.end().col, 10);
    assert!(
        sel.is_complete(),
        "the selection lifecycle state is preserved"
    );
}

#[test]
fn test_adjust_for_row_splice_handles_reverse_and_in_progress_anchors() {
    let mut sel = TextSelection::new();
    sel.start_selection(4, 9, SelectionSide::Right, SelectionType::Block);
    sel.update_selection(-2, 1, SelectionSide::Left);

    assert!(sel.adjust_for_row_splice(3, 2, 5, 8));
    assert_eq!(sel.start().row, 4, "raw footer start anchor stays fixed");
    assert_eq!(
        sel.end().row,
        -4,
        "raw history end anchor moves by the insertion"
    );
    assert!(sel.is_in_progress());
    assert_eq!(sel.selection_type(), SelectionType::Block);
}

#[test]
fn test_adjust_for_row_splice_clears_only_after_real_history_eviction() {
    let mut retained = TextSelection::new();
    retained.start_selection(-4, 0, SelectionSide::Left, SelectionType::Simple);
    retained.update_selection(-2, 2, SelectionSide::Right);
    retained.complete_selection();
    assert!(retained.adjust_for_row_splice(3, 1, 5, 5));
    assert_eq!(retained.normalized_start().row, -5);

    // One endpoint evicted by the splice is PARTIAL eviction and clamps, exactly as
    // in `adjust_for_scroll`. Routing the splice through a byte-identical below-floor
    // clear would have left a top-anchored archival splice destroying a whole
    // selection for one evicted endpoint.
    let mut partly_evicted = TextSelection::new();
    partly_evicted.start_selection(-5, 0, SelectionSide::Left, SelectionType::Simple);
    partly_evicted.update_selection(-2, 2, SelectionSide::Right);
    partly_evicted.complete_selection();
    assert!(partly_evicted.adjust_for_row_splice(3, 1, 5, 5));
    assert!(partly_evicted.has_selection());
    assert_eq!(partly_evicted.normalized_start().row, -5);
    assert_eq!(partly_evicted.normalized_end().row, -3);
    assert!(partly_evicted.truncated());

    // Both endpoints gone is still a clear — the non-vacuity control.
    let mut evicted = TextSelection::new();
    evicted.start_selection(-5, 0, SelectionSide::Left, SelectionType::Simple);
    evicted.update_selection(-4, 2, SelectionSide::Right);
    evicted.complete_selection();
    assert!(!evicted.adjust_for_row_splice(3, 2, 5, 5));
    assert!(!evicted.has_selection());

    // Row `max_rows` is one past the last live row: a splice leaving an anchor
    // there clears, as `adjust_for_scroll` does.
    let mut outside = TextSelection::new();
    outside.start_selection(23, 0, SelectionSide::Left, SelectionType::Simple);
    outside.update_selection(24, 10, SelectionSide::Right);
    assert!(!outside.adjust_for_row_splice(10, 1, 24, 23));
    assert!(!outside.has_selection());
}

/// The render loop's precomputed bounds (`normalized_bounds` +
/// `selection_contains_linear`/`_block`) agree with `contains` on every cell,
/// for a forward, a backward and a block selection.
///
/// These anchors make `normalized_bounds()` equal `side_adjusted_bounds()`.
/// A renderer caching arbitrary anchors must use `side_adjusted_bounds()` so
/// Right-sided starts and Left-sided ends keep the same selected cells.
#[test]
fn precomputed_bounds_equivalent_to_contains() {
    use aterm_types::selection::{selection_contains_block, selection_contains_linear};
    for (start, end, kind) in [
        (
            (3, 10, SelectionSide::Left),
            (8, 25, SelectionSide::Right),
            SelectionType::Simple,
        ),
        (
            (6, 20, SelectionSide::Right),
            (1, 5, SelectionSide::Left),
            SelectionType::Simple,
        ),
        (
            (2, 35, SelectionSide::Left),
            (7, 10, SelectionSide::Right),
            SelectionType::Block,
        ),
    ] {
        let mut sel = TextSelection::new();
        sel.start_selection(start.0, start.1, start.2, kind);
        sel.update_selection(end.0, end.1, end.2);
        sel.complete_selection();
        let (sr, sc, er, ec) = sel.normalized_bounds();
        let (sc, ec) = (usize::from(sc), usize::from(ec));
        for row in 0..12 {
            for col in 0..50u16 {
                let c = usize::from(col);
                let via_precomputed = if kind == SelectionType::Block {
                    selection_contains_block(row, c, sr, sc, er, ec)
                } else {
                    selection_contains_linear(row, c, sr, sc, er, ec)
                };
                assert_eq!(
                    sel.contains(row, col),
                    via_precomputed,
                    "{kind:?} {start:?} ({row}, {col})"
                );
            }
        }
    }
}

// ── project_range tests ──

/// `project_range` over each selection shape: side adjustment moves a Right
/// start forward and a Left end back (to the previous row at column 0), Lines
/// spans whole rows, Block keeps its columns, a backward drag is normalized,
/// and a span the side adjustment empties projects to nothing.
#[test]
fn project_range_projects_every_shape() {
    use SelectionSide::{Left, Right};
    type End = (i32, u16, SelectionSide);
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: [(
        &str,
        End,
        End,
        SelectionType,
        Option<(i32, u16, i32, u16, bool)>,
    ); 10] = [
        (
            "simple",
            (0, 5, Left),
            (0, 10, Right),
            SelectionType::Simple,
            Some((0, 5, 0, 10, false)),
        ),
        (
            "right start",
            (0, 5, Right),
            (0, 10, Right),
            SelectionType::Simple,
            Some((0, 6, 0, 10, false)),
        ),
        (
            "left end",
            (0, 5, Left),
            (0, 10, Left),
            SelectionType::Simple,
            Some((0, 5, 0, 9, false)),
        ),
        (
            "emptied",
            (0, 5, Right),
            (0, 6, Left),
            SelectionType::Simple,
            None,
        ),
        (
            "one anchor",
            (0, 0, Left),
            (0, 0, Left),
            SelectionType::Simple,
            None,
        ),
        (
            "lines",
            (1, 5, Left),
            (3, 2, Right),
            SelectionType::Lines,
            Some((1, 0, 3, 79, false)),
        ),
        (
            "block",
            (0, 5, Left),
            (2, 10, Right),
            SelectionType::Block,
            Some((0, 5, 2, 10, true)),
        ),
        (
            "backward",
            (5, 10, Right),
            (2, 3, Left),
            SelectionType::Simple,
            Some((2, 3, 5, 10, false)),
        ),
        (
            "multiline",
            (0, 5, Left),
            (2, 3, Right),
            SelectionType::Simple,
            Some((0, 5, 2, 3, false)),
        ),
        // Left at column 0 ends before the row, on the previous one.
        (
            "left end at col 0",
            (0, 5, Left),
            (2, 0, Left),
            SelectionType::Simple,
            Some((0, 5, 1, u16::MAX, false)),
        ),
    ];
    assert_eq!(TextSelection::new().project_range(79), None, "no selection");
    for (name, start, end, kind, want) in cases {
        let mut sel = TextSelection::new();
        sel.start_selection(start.0, start.1, start.2, kind);
        sel.update_selection(end.0, end.1, end.2);
        let got = sel
            .project_range(79)
            .map(|p| (p.start_row, p.start_col, p.end_row, p.end_col, p.is_block));
        assert_eq!(got, want, "{name}");
    }
}

// ── include_all tests ──

#[test]
fn test_include_all_expands_sides() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Left);

    // Before include_all: Right start → col 6, Left end → col 9
    let proj_before = sel.project_range(79).expect("before");
    assert_eq!(proj_before.start_col, 6);
    assert_eq!(proj_before.end_col, 9);

    sel.include_all();

    // After include_all: Left start → col 5, Right end → col 10
    let proj_after = sel.project_range(79).expect("after");
    assert_eq!(proj_after.start_col, 5);
    assert_eq!(proj_after.end_col, 10);
}

#[test]
fn include_all_covers_both_endpoints_for_either_drag_direction() {
    for (start, end) in [((0, 5), (0, 10)), ((0, 10), (0, 5)), ((2, 5), (0, 10))] {
        let mut sel = TextSelection::new();
        sel.start_selection(start.0, start.1, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(end.0, end.1, SelectionSide::Right);
        sel.include_all();
        assert!(sel.contains(start.0, start.1), "the original start cell");
        assert!(sel.contains(end.0, end.1), "the original end cell");
        let (first, last) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        assert_eq!(
            sel.side_adjusted_bounds(),
            Some((first.0, first.1, last.0, last.1))
        );
    }
}

// ── contains_cell wide character tests ──

/// In a Block over columns 5..=10, a wide glyph is selected when either of its
/// cells is inside: a lead at 4 (continuation at 5) and a continuation at 11
/// (lead at 10) are in, and glyphs wholly outside are not.
#[test]
fn a_block_selects_a_wide_glyph_either_of_whose_cells_is_inside() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 10, SelectionSide::Right);
    sel.complete_selection();
    // (col, is_wide lead, is_wide_continuation, selected)
    for (col, wide, cont, want) in [
        (4u16, true, false, true),
        (5, false, true, true),
        (3, true, false, false),
        (11, false, true, true),
        (12, false, true, false),
        (7, true, false, true),
        (8, false, true, true),
        (12, true, false, false),
        (13, false, true, false),
    ] {
        assert_eq!(
            sel.contains_cell(1, col, wide, cont),
            want,
            "col {col} wide={wide} cont={cont}"
        );
    }
    assert!(
        !TextSelection::new().contains_cell(0, 5, true, false),
        "no selection"
    );
}

/// A drag that STARTS on the right half of a wide glyph must still paint the
/// left half.
///
/// The copy path admits the whole glyph whenever either of its cells is in
/// range — it has no way to emit half a cluster — so a highlight that stopped at
/// the bare start column would put a character on the clipboard that the user
/// never saw highlighted. A linear selection snaps a glyph whole for that
/// reason, exactly as Block does: both ask `glyph_cell_span`, so the selection
/// kind cannot change where a glyph's boundary lies.
#[test]
fn a_linear_drag_starting_on_a_wide_glyphs_continuation_still_paints_its_lead() {
    let mut sel = TextSelection::new();
    // The glyph occupies cols 4-5; the drag starts on its continuation (col 5).
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    assert!(
        sel.contains_cell(0, 4, true, false),
        "the lead of a glyph whose continuation is selected is selected too — \
         the copy takes that character whether or not it is painted"
    );
    assert!(
        sel.contains_cell(0, 5, false, true),
        "and its continuation, which is what the drag actually landed on"
    );
    // A whole glyph one column further left is untouched: cols 2 and 3 are both
    // outside, so nothing snaps it in.
    assert!(
        !sel.contains_cell(0, 2, true, false),
        "snapping reaches exactly one cell, never a glyph that is fully outside"
    );
    assert!(sel.contains_cell(0, 6, false, false));
}

/// The mirror at the far edge: a drag that STOPS on the left half of a wide
/// glyph must still paint the right half, or the band ends mid-character while
/// the clipboard receives the whole one.
#[test]
fn a_linear_drag_ending_on_a_wide_glyphs_lead_still_paints_its_continuation() {
    let mut sel = TextSelection::new();
    // The glyph occupies cols 10-11; the drag stops on its lead (col 10).
    sel.start_selection(0, 2, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    assert!(
        sel.contains_cell(0, 10, true, false),
        "the lead is selected"
    );
    assert!(
        sel.contains_cell(0, 11, false, true),
        "and so is the continuation of the glyph the drag stopped inside"
    );
    assert!(
        !sel.contains_cell(0, 12, true, false),
        "the glyph after it is fully outside and stays that way"
    );
}

/// Anchor order is not selection order: dragging RIGHT-to-LEFT lands the same
/// mid-glyph edges on the opposite anchors, and `normalized_start`/`_end` order
/// them before the snap. Both edges must behave exactly as in the forward drag.
#[test]
fn a_backwards_drag_snaps_the_same_wide_glyph_edges_as_a_forwards_one() {
    let mut forwards = TextSelection::new();
    forwards.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    forwards.update_selection(0, 10, SelectionSide::Right);
    forwards.complete_selection();

    let mut backwards = TextSelection::new();
    backwards.start_selection(0, 10, SelectionSide::Right, SelectionType::Simple);
    backwards.update_selection(0, 5, SelectionSide::Left);
    backwards.complete_selection();

    for (col, wide, cont) in [
        (4u16, true, false),
        (5, false, true),
        (10, true, false),
        (11, false, true),
        (2, true, false),
        (12, true, false),
    ] {
        assert_eq!(
            backwards.contains_cell(0, col, wide, cont),
            forwards.contains_cell(0, col, wide, cont),
            "col {col} must paint the same whichever direction the drag ran"
        );
    }
    assert!(backwards.contains_cell(0, 4, true, false));
    assert!(backwards.contains_cell(0, 11, false, true));
}

/// A `Lines` selection owns whole rows, so column snapping cannot change its
/// answer — pinned so the shared span helper never leaks a column condition into
/// the one selection kind that has none.
#[test]
fn a_lines_selection_paints_every_half_of_every_glyph_on_its_rows() {
    let mut sel = TextSelection::new();
    sel.start_selection(1, 0, SelectionSide::Left, SelectionType::Lines);
    sel.update_selection(2, 79, SelectionSide::Right);
    sel.complete_selection();

    assert!(sel.contains_cell(1, 0, true, false));
    assert!(sel.contains_cell(1, 1, false, true));
    assert!(sel.contains_cell(2, 79, true, false));
    assert!(!sel.contains_cell(3, 0, true, false), "rows below are not");
    assert!(!sel.contains_cell(0, 79, true, false), "nor rows above");
}

/// Full-row geometry must agree before gesture adapters expand the columns,
/// including shift-click endpoints on the left side of column zero.
#[test]
fn lines_bounds_projection_and_hits_ignore_anchor_columns_and_sides() {
    for (start_row, end_row) in [(1, 2), (2, 1), (1, 1), (i32::MIN, i32::MIN)] {
        for start_side in [SelectionSide::Left, SelectionSide::Right] {
            for end_side in [SelectionSide::Left, SelectionSide::Right] {
                for (start_col, end_col) in [(0, 0), (3, 0), (0, 3), (3, 7)] {
                    let mut sel = TextSelection::new();
                    sel.start_selection(start_row, start_col, start_side, SelectionType::Lines);
                    sel.update_selection(end_row, end_col, end_side);
                    let first = start_row.min(end_row);
                    let last = start_row.max(end_row);
                    assert_eq!(sel.side_adjusted_bounds(), Some((first, 0, last, u16::MAX)));
                    assert_eq!(
                        sel.project_range(79),
                        Some(SelectionProjection {
                            start_row: first,
                            start_col: 0,
                            end_row: last,
                            end_col: 79,
                            is_block: false,
                        })
                    );
                    for row in [first, last] {
                        assert!(sel.contains(row, 0));
                        assert!(sel.contains(row, 79));
                    }
                }
            }
        }
    }
}

#[test]
fn a_linear_end_before_the_first_representable_row_is_empty() {
    for kind in [SelectionType::Simple, SelectionType::Semantic] {
        let mut sel = TextSelection::new();
        sel.start_selection(i32::MIN, 0, SelectionSide::Left, kind);
        sel.update_selection(i32::MIN, 0, SelectionSide::Left);
        assert_eq!(sel.side_adjusted_bounds(), None);
        assert_eq!(sel.project_range(79), None);
        assert!(!sel.contains(i32::MIN, 0));
        assert!(!sel.contains(i32::MAX, 0));
    }
}

#[test]
fn test_contains_cell_block_continuation_at_col_zero() {
    // Edge case: continuation at column 0 (shouldn't happen in practice,
    // but must not underflow).
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 5, SelectionSide::Right);
    sel.complete_selection();

    // A continuation at col 0 has no lead to back up to; `glyph_cell_span`
    // saturates to (0, 0) rather than wrapping to u16::MAX.
    assert!(sel.contains_cell(1, 0, false, true));
}

// ── column 0 boundary tests ──

/// Selection ending at col 0 with Left side retreats to the previous row.
#[test]
fn test_contains_end_left_side_col_zero_multiline() {
    // Selection from (0, 5, Left) to (2, 0, Left).
    // Left-sided end at col 0 means "stop before col 0 on row 2",
    // so nothing on row 2 should be selected.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 0, SelectionSide::Left);
    sel.complete_selection();

    // Row 0: cols 5+ selected
    assert!(!sel.contains(0, 4));
    assert!(sel.contains(0, 5));
    assert!(sel.contains(0, 80));

    // Row 1: entirely selected (middle row)
    assert!(sel.contains(1, 0));
    assert!(sel.contains(1, 40));
    assert!(sel.contains(1, 80));

    // Row 2: nothing selected (end is "before col 0")
    assert!(
        !sel.contains(2, 0),
        "col 0 on end row must NOT be selected when end side is Left at col 0"
    );
    assert!(!sel.contains(2, 1));

    // Row 3: not selected
    assert!(!sel.contains(3, 0));
}

/// Block selection: normalized_end always uses Right side, so user-provided
/// Left side on the end anchor does not cause a col 0 off-by-one for blocks.
/// The block normalization takes min/max of columns and forces Left/Right
/// sides, making the block from col 0 to col 0 (1-column wide).
#[test]
fn test_contains_block_end_left_side_col_zero() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 0, SelectionSide::Left);
    sel.complete_selection();

    // Block normalization forces start=Left, end=Right on the min/max columns.
    // Both anchors have col 0, so the block is 1 column wide at col 0.
    assert!(
        sel.contains(1, 0),
        "block selection col 0..=0 should include col 0 (normalized_end forces Right side)"
    );
    assert!(
        !sel.contains(1, 1),
        "col 1 should not be in a col 0..=0 block"
    );
}

#[test]
fn test_adjust_for_rows_shrink_relabels_every_row_by_the_same_amount() {
    // 10 visible rows shrinking to 6. Since the rows-only shapes anchor at the
    // CURSOR, the viewport gives up its top four rows to history and EVERY row —
    // live or already-archived — moves by exactly -4. Measured against the engine
    // in `probe`-style traces: rel 0 (line-55) -> rel -4, rel 7 (line-62) -> rel 3.
    let mut live = TextSelection::new();
    live.start_selection(2, 1, SelectionSide::Left, SelectionType::Simple);
    live.update_selection(5, 7, SelectionSide::Right);
    live.complete_selection();
    assert!(live.adjust_for_rows_shrink(6, 4, 50));
    assert_eq!(
        live.start().row,
        -2,
        "a live row moves by the rows given up"
    );
    assert_eq!(live.end().row, 1);

    let mut bottom = TextSelection::new();
    bottom.start_selection(6, 0, SelectionSide::Left, SelectionType::Simple);
    bottom.update_selection(9, 3, SelectionSide::Right);
    bottom.complete_selection();
    assert!(bottom.adjust_for_rows_shrink(6, 4, 50));
    assert_eq!(
        bottom.start().row,
        2,
        "the bottom rows stay live under the cursor"
    );
    assert_eq!(bottom.end().row, 5);
    assert_eq!(bottom.start().col, 0, "columns are untouched");
    assert!(bottom.is_complete(), "and so is the lifecycle state");

    let mut archived = TextSelection::new();
    archived.start_selection(-20, 0, SelectionSide::Left, SelectionType::Block);
    archived.update_selection(-7, 2, SelectionSide::Right);
    assert!(archived.adjust_for_rows_shrink(6, 4, 50));
    assert_eq!(archived.start().row, -24, "history gains four newer lines");
    assert_eq!(archived.end().row, -11);
    assert!(archived.is_in_progress());
    assert_eq!(archived.selection_type(), SelectionType::Block);

    // A live span and an archived span move by the same 4 rows: one relabel.
    let mut degenerate = TextSelection::new();
    degenerate.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
    degenerate.update_selection(1, 1, SelectionSide::Right);
    degenerate.complete_selection();
    assert!(!degenerate.adjust_for_rows_shrink(0, 4, 50));
    assert!(!degenerate.has_selection());

    // An empty selection is a no-op in every direction.
    let mut empty = TextSelection::new();
    assert!(empty.adjust_for_rows_shrink(6, 4, 50));
    assert!(!empty.has_selection());
}

/// A rows-only SHRINK is MONOTONIC, so no span can be torn by it.
///
/// Anchors are a row RANGE (the copy walk is `first_row..=last_row`), so a
/// non-monotonic map would reverse a span crossing the cut and copy rows the user
/// never selected. Should rows-only shapes stop anchoring at the cursor, this
/// test fails.
#[test]
fn a_shrink_spares_a_span_that_crosses_the_cut() {
    // 10 visible rows -> 6. A span over live rows 4..=7 crosses the old cut.
    let mut sel = TextSelection::new();
    sel.start_selection(4, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(7, 5, SelectionSide::Right);
    sel.complete_selection();
    assert!(
        sel.adjust_for_rows_shrink(6, 4, 100),
        "the span survives the shrink"
    );
    assert_eq!(sel.start().row, 0, "…relabelled by the rows given up");
    assert_eq!(sel.end().row, 3);
    assert!(
        sel.start().row <= sel.end().row,
        "a reversed range copies rows the user never selected"
    );

    // …and a span from scrollback into the live rows.
    let mut across = TextSelection::new();
    across.start_selection(-3, 0, SelectionSide::Left, SelectionType::Simple);
    across.update_selection(2, 5, SelectionSide::Right);
    across.complete_selection();
    assert!(
        across.adjust_for_rows_shrink(6, 4, 100),
        "a span from scrollback into the live rows is continuous under a relabel"
    );
    assert_eq!(across.start().row, -7);
    assert_eq!(across.end().row, -2);
    assert!(across.start().row <= across.end().row, "order again");
}

/// `expand_semantic` and `expand_lines` act only on an in-progress selection of
/// their own type, and then cover the whole span: for every selection type,
/// in progress and complete, and every column pair 0..=4 — the expansion lands
/// on exactly those columns, Left to Right (`expand_lines` from column 0 to the
/// width), and anything else leaves the anchors as they were.
#[test]
fn expansion_acts_only_on_an_in_progress_selection_of_its_type() {
    let types = [
        SelectionType::Simple,
        SelectionType::Block,
        SelectionType::Semantic,
        SelectionType::Lines,
    ];
    for kind in types {
        for complete in [false, true] {
            for start_col in 0..=4u16 {
                for end_col in 0..=4u16 {
                    let fresh = || {
                        let mut sel = TextSelection::new();
                        sel.start_selection(1, 2, SelectionSide::Right, kind);
                        sel.update_selection(1, 3, SelectionSide::Left);
                        if complete {
                            sel.complete_selection();
                        }
                        sel
                    };
                    let at = format!("{kind:?} complete={complete} {start_col}..{end_col}");

                    let mut sel = fresh();
                    sel.expand_semantic(start_col, end_col);
                    if !complete && kind == SelectionType::Semantic {
                        assert_eq!(
                            (sel.start, sel.end),
                            (
                                SelectionAnchor::new(1, start_col, SelectionSide::Left),
                                SelectionAnchor::new(1, end_col, SelectionSide::Right)
                            ),
                            "{at}"
                        );
                    } else {
                        assert_eq!(sel, fresh(), "{at}: expand_semantic must not act");
                    }

                    let mut sel = fresh();
                    sel.expand_lines(end_col);
                    if !complete && kind == SelectionType::Lines {
                        assert_eq!(
                            (sel.start, sel.end),
                            (
                                SelectionAnchor::new(1, 0, SelectionSide::Left),
                                SelectionAnchor::new(1, end_col, SelectionSide::Right)
                            ),
                            "{at}"
                        );
                    } else {
                        assert_eq!(sel, fresh(), "{at}: expand_lines must not act");
                    }
                }
            }
        }
    }
}

/// `truncate_to_floor` over every small span, Simple and Block: every pair of
/// endpoint rows -6..=3 (either drag direction), columns 0 and 2, and a history
/// floor of 0..=6 rows. The outcomes are exhaustive: nothing evicted leaves the
/// rows untouched and reports nothing; both endpoints evicted, or no history
/// to clamp onto, clears; one endpoint evicted clamps it onto the floor row —
/// keeping the other row exactly, reporting the loss and leaving the state
/// machine where it was, and for a Block moving the ROW only — unless the
/// clamp leaves nothing selectable, which clears.
#[test]
fn truncate_to_floor_over_every_small_span() {
    let oldest_abs = 1000u64;
    for kind in [SelectionType::Simple, SelectionType::Block] {
        for r1 in -6..=3i32 {
            for r2 in -6..=3i32 {
                for (c1, c2) in [(0, 0), (0, 2), (2, 0), (2, 2)] {
                    for floor in 0..=6u64 {
                        let mut sel = TextSelection::new();
                        sel.start_selection(r1, c1, SelectionSide::Left, kind);
                        sel.update_selection(r2, c2, SelectionSide::Right);
                        sel.complete_selection();
                        let min_row = -i32::try_from(floor).unwrap();
                        let (lo, hi) = (r1.min(r2), r1.max(r2));
                        let at = format!("{kind:?} ({r1},{c1})..({r2},{c2}) floor {floor}");

                        let survived = sel.truncate_to_floor(oldest_abs, oldest_abs + floor);
                        if lo >= min_row {
                            assert!(survived && !sel.truncated(), "{at}");
                            assert_eq!((sel.start().row, sel.end().row), (r1, r2), "{at}");
                        } else if hi < min_row || min_row == 0 {
                            assert!(!survived, "{at}");
                            assert_eq!(sel.state(), SelectionState::None, "{at}");
                            assert!(!sel.truncated(), "{at}");
                        } else if survived {
                            assert!(sel.truncated(), "{at}");
                            assert_eq!(sel.state(), SelectionState::Complete, "{at}");
                            assert_eq!(sel.normalized_start().row, min_row, "{at}");
                            assert_eq!(sel.normalized_end().row, hi, "{at}");
                            if kind == SelectionType::Block {
                                assert_eq!((sel.start().col, sel.end().col), (c1, c2), "{at}");
                            }
                        } else {
                            assert_eq!(sel.state(), SelectionState::None, "{at}");
                        }
                    }
                }
            }
        }
    }
}

/// `is_empty` is "no selection, or both anchors on the same point": right
/// after a press, after a drag to every point of a 2 x 4 patch on both sides
/// (empty exactly when the drag lands back on the press), after the release,
/// and after a clear.
#[test]
fn is_empty_iff_none_or_the_anchors_coincide() {
    let press = SelectionAnchor::new(0, 1, SelectionSide::Left);
    for row in 0..=1 {
        for col in 0..=3u16 {
            for side in [SelectionSide::Left, SelectionSide::Right] {
                let mut sel = TextSelection::new();
                assert!(sel.is_empty());
                sel.start_selection(press.row, press.col, press.side, SelectionType::Simple);
                assert!(sel.is_empty(), "right after the press");
                let drag = SelectionAnchor::new(row, col, side);
                sel.update_selection(row, col, side);
                assert_eq!(sel.is_empty(), drag == press, "drag to {drag:?}");
                sel.complete_selection();
                assert_eq!(sel.is_empty(), drag == press, "released at {drag:?}");
                sel.clear();
                assert!(sel.is_empty(), "cleared");
            }
        }
    }
}

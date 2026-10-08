// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ActivePaneEdge, ActivePaneMark, active_pane_edge_cell, divider_cell, fill_divider_grid,
};
use aterm_render::{RenderInput, Theme};

/// Every cell of a composed frame that is not the plain divider seam, as
/// `(row, col, glyph)`. The mark is the only thing that puts a glyph in a
/// divider gap, so this is the complete inventory of what it painted.
fn stripes(dst: &RenderInput, theme: Theme) -> Vec<(usize, usize, char)> {
    let seam = divider_cell(theme);
    let mut out = Vec::new();
    for (r, row) in dst.cells.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if *cell != seam {
                out.push((r, c, cell.ch));
            }
        }
    }
    out
}

/// The mark hugs exactly the divider lines that touch the active pane, on the
/// side facing it, and reaches nothing else — not the far divider, not one
/// cell of any pane's rectangle.
#[test]
fn the_mark_inks_the_divider_lines_that_touch_the_active_pane_and_no_others() {
    let theme = Theme::default();
    // 5x11: [pane 0..5 cols 0..5][divider col 5][pane cols 6..11], with a
    // horizontal divider at row 2 splitting the right column of panes only.
    // The active pane is the bottom-right one: rows 3..5, cols 6..11.
    let mut dst = RenderInput::empty();
    fill_divider_grid(
        &mut dst,
        5,
        11,
        theme,
        Some(ActivePaneMark {
            row: 3,
            col: 6,
            rows: 2,
            cols: 5,
        }),
    );
    let mut inked = stripes(&dst, theme);
    inked.sort_unstable();

    let top: Vec<_> = (5..11).map(|c| (2, c, '▁')).collect();
    let left: Vec<_> = (3..5).map(|r| (r, 5, '▐')).collect();
    let mut expected = top;
    expected.extend(left);
    expected.sort_unstable();
    assert_eq!(
        inked, expected,
        "the mark is the divider row above the pane (reaching one cell left, to \
         the corner) plus the divider column beside it — and nothing else"
    );
    assert_eq!(
        dst.cells[2][4],
        divider_cell(theme),
        "the divider row LEFT of the active pane's corner is a different pane's \
         border and stays plain"
    );
}

/// The other two sides of the same layout, so all four are reached: the
/// Top-left pane (rows 0..2, cols 0..5) has a divider below it and one to its
/// Right, and the ink lands on the near side of each — the top of the row
/// below, the left of the column beside.
#[test]
fn a_pane_with_a_divider_below_and_beside_it_is_inked_on_those_two_sides() {
    let theme = Theme::default();
    let mut dst = RenderInput::empty();
    fill_divider_grid(
        &mut dst,
        5,
        11,
        theme,
        Some(ActivePaneMark {
            row: 0,
            col: 0,
            rows: 2,
            cols: 5,
        }),
    );
    let mut inked = stripes(&dst, theme);
    inked.sort_unstable();

    let mut expected: Vec<_> = (0..6).map(|c| (2, c, '▔')).collect();
    expected.extend((0..2).map(|r| (r, 5, '▌')));
    expected.sort_unstable();
    assert_eq!(
        inked, expected,
        "the divider row below the pane is inked along its TOP edge and the \
         column beside it along its LEFT edge — both facing the pane"
    );
    assert!(
        dst.cells[0][..5].iter().all(|cell| cell.ch == ' '),
        "and the pane's own first row is untouched: the mark never enters a \
         rectangle a pane owns"
    );
}

/// A frame that marks no pane is byte-identical to the plain seam grid: the
/// mark can only ever ADD ink to a divider gap.
#[test]
fn an_unmarked_composite_is_the_plain_seam_grid() {
    let theme = Theme::default();
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 3, 7, theme, None);
    assert!(
        stripes(&dst, theme).is_empty(),
        "no mark, no ink: {:?}",
        stripes(&dst, theme)
    );
}

/// A pane that owns the whole composite has no divider to mark, so a
/// single-pane (or zoomed) frame is the plain seam grid even with the mark
/// resolved and the window focused. This is the property that keeps a
/// single-pane window completely unchanged.
#[test]
fn a_pane_that_fills_the_window_stamps_nothing() {
    let theme = Theme::default();
    let mut dst = RenderInput::empty();
    fill_divider_grid(
        &mut dst,
        3,
        7,
        theme,
        ActivePaneMark::resolve(true, true, (0, 0, 3, 7)),
    );
    assert!(
        stripes(&dst, theme).is_empty(),
        "every edge line falls off the grid"
    );
}

/// The two gates that must silence the mark, and the geometry that must not.
#[test]
fn the_setting_and_the_windows_own_focus_each_withhold_the_mark() {
    let rect = (0, 0, 2, 3);
    assert_eq!(
        ActivePaneMark::resolve(false, true, rect),
        None,
        "the setting off marks nothing"
    );
    assert_eq!(
        ActivePaneMark::resolve(true, false, rect),
        None,
        "a window that does not have the keyboard claims no pane"
    );
    assert_eq!(
        ActivePaneMark::resolve(true, true, rect),
        Some(ActivePaneMark {
            row: 0,
            col: 0,
            rows: 2,
            cols: 3
        })
    );
    assert_eq!(
        ActivePaneMark::resolve(true, true, (0, 0, 0, 3)),
        None,
        "a degenerate pane has no edge to hug"
    );
}

/// Each side's stripe sits on the half of the divider cell that FACES the
/// pane — the whole point of the mark, since a 1-cell divider is shared by
/// two panes and only the side the ink is on says which of them is live.
#[test]
fn each_edge_inks_the_side_of_the_divider_cell_that_faces_the_pane() {
    let theme = Theme::default();
    let seam = divider_cell(theme);
    for (edge, ch) in [
        (ActivePaneEdge::Top, '▁'),
        (ActivePaneEdge::Bottom, '▔'),
        (ActivePaneEdge::Left, '▐'),
        (ActivePaneEdge::Right, '▌'),
    ] {
        let cell = active_pane_edge_cell(theme, edge);
        assert_eq!(cell.ch, ch, "{edge:?} inks the edge touching the pane");
        assert_eq!(
            cell.bg, seam.bg,
            "{edge:?} keeps the seam as its GROUND, so the contrast floor \
             resolves every side of the ring the same way"
        );
        assert_ne!(cell.fg, seam.fg, "{edge:?} has ink to show");
        assert_eq!(
            (cell.wide, cell.bold, cell.underline),
            (seam.wide, seam.bold, seam.underline),
            "{edge:?} is a divider cell with a glyph, nothing more"
        );
    }
}

/// The RepaintKey term: `0` while nothing is marked (the idle invariant), and
/// a value that MOVES when the marked rectangle does — otherwise the present
/// early-out swallows the frame that moves the ring to the pane the user just
/// focused, and the mark would point at the previous pane until unrelated
/// output arrived.
#[test]
fn the_repaint_key_term_is_zero_for_no_mark_and_moves_with_the_rectangle() {
    assert_eq!(ActivePaneMark::key_term(None), 0);
    let left = ActivePaneMark::resolve(true, true, (0, 0, 4, 5));
    let right = ActivePaneMark::resolve(true, true, (0, 6, 4, 5));
    assert_ne!(ActivePaneMark::key_term(left), 0);
    assert_ne!(
        ActivePaneMark::key_term(left),
        ActivePaneMark::key_term(right),
        "focus moving to the sibling pane must reach glass"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::selection::{SelectionSide, SelectionType};

fn selected_pane(rows: usize, cols: usize) -> RenderInput {
    let mut src = RenderInput::empty();
    src.rows = rows;
    src.cols = cols;
    src.selection
        .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    src.selection.update_selection(
        i32::try_from(rows).unwrap() - 1,
        u16::try_from(cols).unwrap() - 1,
        SelectionSide::Right,
    );
    src.selection.complete_selection();
    src
}

#[test]
fn off_frame_focused_pane_selection_fails_closed() {
    let src = selected_pane(2, 2);
    let mut dst = RenderInput::empty();
    dst.rows = 2;
    dst.cols = 4;
    push_pane_selection(&mut dst, &src, 0, 8, 2, 2, true);

    assert_eq!(
        dst.selection_clip,
        Some(aterm_core::render::SelectionClip::new(0, 2, 4, 4)),
        "active but clipped-away selection keeps explicit empty authority"
    );
    for row in 0..dst.rows {
        for col in 0..dst.cols {
            assert!(
                !dst.selection_contains_cell(row, col, false, false),
                "off-frame pane selection leaked to frame cell ({row}, {col})"
            );
        }
    }
}

#[test]
fn both_panes_paint_and_the_focused_one_keeps_the_scalar_anchor() {
    let mut dst = RenderInput::empty();
    dst.rows = 2;
    dst.cols = 9;
    let mut focused = selected_pane(2, 4);
    focused.selection_bg = 0x0011_2233;
    push_pane_selection(&mut dst, &focused, 0, 0, 2, 4, true);
    let mut sibling = selected_pane(2, 4);
    sibling.selection_bg = 0x0044_5566;
    push_pane_selection(&mut dst, &sibling, 0, 5, 2, 4, false);

    assert_eq!(
        dst.selection_bg, 0x0011_2233,
        "the scalar colour is the FOCUSED pane's, unchanged"
    );
    assert_eq!(dst.selections.len(), 2);
    assert_eq!(
        (dst.selections[0].bg, dst.selections[1].bg),
        (0x0011_2233, 0x0044_5566),
        "each entry carries its OWN live selection colour"
    );
    assert_eq!(
        (dst.selections[0].inactive, dst.selections[1].inactive),
        (false, true)
    );
    for col in 0..dst.cols {
        let hit = dst.selection_hit(0, col, false, false);
        let expected = match col {
            0..=3 => Some(0),
            4 => None, // the divider column belongs to no pane
            _ => Some(1),
        };
        assert_eq!(hit, expected, "column {col} resolved to the wrong pane");
    }
}

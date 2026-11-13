// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::selection::{SelectionSide, SelectionType};

#[test]
fn scrub_detects_block_selection_snapped_onto_wide_lead() {
    let blank = crate::chrome_band::blank_cell(aterm_render::Theme::default());
    let mut continuation = blank;
    continuation.wide = true;

    let mut input = RenderInput::empty();
    input.rows = 1;
    input.cols = 1;
    input.cells = vec![vec![blank, continuation]];
    input
        .selection
        .start_selection(0, 1, SelectionSide::Left, SelectionType::Block);
    input.selection.update_selection(0, 1, SelectionSide::Right);
    input.selection.complete_selection();
    assert!(
        input.selection_contains_cell(0, 0, true, false),
        "negative-control precondition: wide snapping reaches the visible lead"
    );
    assert!(
        !input.selection_contains_cell(0, 0, false, false),
        "a scalar-only probe would miss this intersection"
    );

    scrub_overlay_row_band(&mut input, 0..1, aterm_render::RowBands::new(0, 16));

    assert!(
        !input.selection.has_selection(),
        "chrome replacement must scrub the same wide-aware selection the renderer paints"
    );
}

#[test]
fn scrub_still_clears_overlays_when_selection_clip_is_disjoint_from_band() {
    let blank = crate::chrome_band::blank_cell(aterm_render::Theme::default());
    let mut input = RenderInput::empty();
    input.rows = 4;
    input.cols = 4;
    input.cells = vec![vec![blank; 4]; 4];

    let mut selection = aterm_core::selection::TextSelection::new();
    selection.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(2, 2, SelectionSide::Right);
    selection.complete_selection();
    input.selections.push(aterm_core::render::PaneSelection {
        selection,
        clip: aterm_core::render::SelectionClip::new(2, 4, 0, 4),
        bg: 0x334455,
        fg: 0xffffff,
        inactive: false,
    });

    input.cursor_trail.push(aterm_core::render::TrailCell {
        row: 0,
        col: 1,
        alpha: 255,
    });
    input.cursor_glow_add.push(aterm_core::render::GlowQuad {
        x: 0,
        y: 0,
        w: 8,
        h: 16,
        color: 0xffffff,
        alpha: 0,
        color2: 0xffffff,
        alpha2: 0,
        row: 0,
    });
    scrub_overlay_row_band(&mut input, 0..1, aterm_render::RowBands::new(0, 16));

    assert_eq!(
        input.selections.len(),
        1,
        "a selection in rows 2..4 is untouched by a top-row chrome band"
    );
    assert!(
        input.cursor_trail.is_empty() && input.cursor_glow_add.is_empty(),
        "overlay effects in the spliced band must still be scrubbed when selection clips are disjoint"
    );
    assert_eq!(
        input.fx_occluders,
        [aterm_core::render::PxRect::new(i32::MIN, 0, i32::MAX, 16)],
        "…and the band occludes every fx layer (the companions, the rain)"
    );
}

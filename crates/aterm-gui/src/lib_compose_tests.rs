// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::app_render::{
    blit_pane_into, divider_cell, fill_blank_pane_into, fill_divider_grid, terminal_blank_cell,
};
use aterm_core::terminal::Terminal;
use aterm_render::{RenderInput, Theme};

/// A pane snapshot of `text` rendered into the top row of a `rows`x`cols` grid.
fn pane_snapshot(
    text: &str,
    rows: usize,
    cols: usize,
) -> (RenderInput, aterm_core::terminal::RenderCell) {
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(text.as_bytes());
    let mut snap = RenderInput::empty();
    term.cell_frame_into(&mut snap, rows, cols);
    (snap, terminal_blank_cell(&term))
}

/// `fill_divider_grid` produces a full `rows`x`cols` grid of identical seam
/// cells, no cursor, single-width rows.
#[test]
fn divider_grid_is_uniform_seam() {
    let theme = Theme::default();
    let seam = divider_cell(theme);
    let mut dst = RenderInput::empty();
    // Host visuals and present transforms belong to the frame that made them: a single-pane ->
    // split/native transition must not carry any of them in the reused scratch.
    dst.ink.push(aterm_render::InkCell {
        row: 0,
        col: 0,
        color: [9, 9, 9],
    });
    let atlas = std::sync::Arc::new(aterm_render::SceneAtlas {
        width: 1,
        height: 1,
        rgba: vec![255, 255, 255, 255],
        version: 1,
    });
    dst.cat_quads.push(aterm_render::SpriteQuad::default());
    dst.cat_atlas = Some(std::sync::Arc::clone(&atlas));
    dst.free_sprites
        .push(aterm_core::render::FreeSprite::default());
    dst.free_atlas = Some(std::sync::Arc::clone(&atlas));
    dst.rain_quads.push(aterm_render::SpriteQuad::default());
    dst.rain_atlas = Some(atlas);
    dst.scroll_frac_px = 3;
    dst.grid_top_row = 1;
    dst.grid_bot_row = 3;
    dst.base_y = 91;
    dst.absolute_row_revision = 92;
    dst.input_hot = true;
    dst.selection.start_selection(
        0,
        0,
        aterm_core::selection::SelectionSide::Left,
        aterm_core::selection::SelectionType::Simple,
    );
    dst.selection
        .update_selection(0, 1, aterm_core::selection::SelectionSide::Right);
    dst.selection.complete_selection();
    dst.selection_bg = 0x0012_3456;
    dst.selection_fg = 0x0065_4321;
    dst.selections.push(aterm_core::render::PaneSelection {
        selection: dst.selection.clone(),
        clip: aterm_core::render::SelectionClip::new(0, 1, 0, 2),
        bg: 0x0012_3456,
        fg: 0x0065_4321,
        inactive: true,
    });
    assert!(
        dst.selection.has_selection(),
        "fixture must seed stale selection state"
    );
    fill_divider_grid(&mut dst, 3, 5, theme, None);
    assert!(dst.ink.is_empty(), "fill_divider_grid paints cells only");
    assert!(
        dst.cat_quads.is_empty(),
        "compose path carries no stale cats"
    );
    assert!(dst.cat_atlas.is_none(), "compose path drops the cat atlas");
    assert!(
        dst.free_sprites.is_empty(),
        "compose path carries no stale free sprites"
    );
    assert!(
        dst.free_atlas.is_none(),
        "compose path drops the free-sprite atlas"
    );
    assert!(
        dst.rain_quads.is_empty(),
        "compose path carries no stale rain sprites"
    );
    assert!(
        dst.rain_atlas.is_none(),
        "compose path drops the rain atlas"
    );
    assert_eq!(
        (
            dst.scroll_frac_px,
            dst.grid_top_row,
            dst.grid_bot_row,
            dst.base_y,
            dst.absolute_row_revision,
            dst.input_hot,
        ),
        (0, 0, 0, 0, 0, false),
        "compose fill neutralizes stale present transforms and metadata"
    );
    assert_eq!(dst.rows, 3);
    assert_eq!(dst.cols, 5);
    assert_eq!(dst.cells.len(), 3);
    for row in &dst.cells {
        assert_eq!(row.len(), 5);
        assert!(
            row.iter().all(|c| *c == seam),
            "every cell is the seam colour"
        );
    }
    assert!(!dst.cursor_visible);
    assert!(
        !dst.selection.has_selection(),
        "divider-only frames carry no stale terminal selection"
    );
    assert_eq!(
        (dst.selection_bg, dst.selection_fg),
        (
            aterm_core::render::COLOR_UNSET,
            aterm_core::render::COLOR_UNSET,
        ),
        "divider-only frames carry no stale terminal selection colours"
    );
    assert!(
        dst.selections.is_empty(),
        "…and no stale PANE selections, which would out-rank the scalars above"
    );
    assert!(
        dst.line_size_spans.iter().all(Vec::is_empty),
        "divider-only rows carry no pane geometry"
    );
    assert!(
        dst.default_bg_spans.iter().all(Vec::is_empty),
        "divider-only rows carry no pane background provenance"
    );
}

/// An implicit blank is resolved from the pane's live terminal defaults, not
/// the host theme. OSC 10/11 update these defaults, and DECSCNM swaps them.
#[test]
fn terminal_blank_tracks_live_defaults_and_decscnm() {
    let mut term = Terminal::new(1, 2);
    term.set_default_foreground(aterm_core::terminal::Rgb {
        r: 16,
        g: 32,
        b: 48,
    });
    term.set_default_background(aterm_core::terminal::Rgb {
        r: 64,
        g: 80,
        b: 96,
    });

    let normal = terminal_blank_cell(&term);
    assert_eq!(normal.fg, [16, 32, 48]);
    assert_eq!(normal.bg, [64, 80, 96]);

    term.process(b"\x1b[?5h");
    let reversed = terminal_blank_cell(&term);
    assert_eq!(reversed.fg, normal.bg, "DECSCNM swaps default fg");
    assert_eq!(reversed.bg, normal.fg, "DECSCNM swaps default bg");
}

/// `blit_pane_into` places a pane's cells at the given offset and leaves the divider column a
/// seam.
#[test]
fn blit_places_pane_and_keeps_seam() {
    let theme = Theme::default();
    let seam = divider_cell(theme);
    // 1x5 window: [pane A (cols 0..2)] [divider col 2] [pane B (cols 3..5)].
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 1, 5, theme, None);
    let (left, left_blank) = pane_snapshot("AB", 1, 2);
    let (right, right_blank) = pane_snapshot("CD", 1, 2);
    blit_pane_into(&mut dst, &left, 0, 0, left_blank);
    blit_pane_into(&mut dst, &right, 0, 3, right_blank);
    let row = &dst.cells[0];
    assert_eq!(row[0].ch, 'A');
    assert_eq!(row[1].ch, 'B');
    assert_eq!(row[2], seam, "the divider column is left as a seam cell");
    assert_eq!(row[3].ch, 'C');
    assert_eq!(row[4].ch, 'D');
}

/// A split pane's BiDi row keeps its visual→logical map at the pane's columns, so its
/// relocated selection lights the cells it copies; the left pane and divider stay unpermuted.
#[test]
fn a_composed_bidi_pane_lights_the_cells_its_selection_copies() {
    use crate::app_render::push_pane_selection;
    use aterm_core::selection::{SelectionSide, SelectionType};
    let theme = Theme::default();
    // 1x20 window: [left pane 0..6] [divider 6] [right pane 7..20].
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 1, 20, theme, None);
    let (left, left_blank) = pane_snapshot("LEFT", 1, 6);
    // `ab אבג cd`, painted `ab גבא cd`: logical 3 (א) sits at pane column 5.
    let mut term = Terminal::new(1, 13);
    term.process("ab \u{05D0}\u{05D1}\u{05D2} cd".as_bytes());
    let sel = term.text_selection_mut();
    sel.start_selection(0, 3, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 3, SelectionSide::Right);
    sel.complete_selection();
    assert_eq!(term.selection_to_string().as_deref(), Some("\u{05D0}"));
    let mut right = RenderInput::empty();
    term.cell_frame_into(&mut right, 1, 13);
    blit_pane_into(&mut dst, &left, 0, 0, left_blank);
    blit_pane_into(&mut dst, &right, 0, 7, terminal_blank_cell(&term));
    push_pane_selection(&mut dst, &left, 0, 0, 1, 6, false);
    push_pane_selection(&mut dst, &right, 0, 7, 1, 13, true);

    let lit: Vec<usize> = (0..20)
        .filter(|&c| dst.selection_contains_cell(0, c, false, false))
        .collect();
    assert_eq!(lit, vec![12], "the composite lights the cell painting א");
    assert_eq!(dst.cells[0][12].ch, '\u{05D0}');
    assert_eq!(dst.logical_col(0, 3), 3, "the left pane is unpermuted");
    assert_eq!(dst.logical_col(0, 6), 6, "so is the divider");
}

/// DEC line size is per pane, and a composite row can hold several panes: each pane's
/// line size must stay confined to its own columns, not win the whole row.
#[test]
fn blit_confines_each_pane_line_size_to_its_own_columns() {
    use aterm_core::grid::LineSize;
    let theme = Theme::default();
    // 1x11 window: [left pane 0..5] [divider 5] [right pane 6..11].
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 1, 11, theme, None);

    let (left, left_blank) = pane_snapshot("AB", 1, 5);
    // ESC # 6 = DECDWL: this pane's line really is double-width.
    let (right, right_blank) = pane_snapshot("\x1b#6CD", 1, 5);
    assert_eq!(
        left.line_sizes[0],
        LineSize::SingleWidth,
        "control: the left pane is an ordinary line"
    );
    assert_eq!(
        right.line_sizes[0],
        LineSize::DoubleWidth,
        "control: the right pane really did take DECDWL"
    );

    // Blit the double-width pane last, the order that would otherwise win the row.
    blit_pane_into(&mut dst, &left, 0, 0, left_blank);
    blit_pane_into(&mut dst, &right, 0, 6, right_blank);

    for c in 0..6 {
        assert_eq!(
            dst.line_size_run_at(0, c).0,
            LineSize::SingleWidth,
            "column {c} is left-pane/divider and must not inherit the right pane's DECDWL"
        );
    }
    for c in 6..11 {
        let (ls, start_col, end_col) = dst.line_size_run_at(0, c);
        assert_eq!(ls, LineSize::DoubleWidth, "right-pane column {c}");
        assert_eq!(
            (start_col, end_col),
            (6, 11),
            "the run is the clip box: a double-width pane must not bleed left of its own columns"
        );
    }
}

/// An all-single-width split records no spans, so the renderers keep their uniform
/// `col * cell_w` path.
#[test]
fn blit_leaves_ordinary_splits_uniform() {
    let theme = Theme::default();
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 2, 11, theme, None);
    let (left, left_blank) = pane_snapshot("AB", 2, 5);
    let (right, right_blank) = pane_snapshot("CD", 2, 5);
    blit_pane_into(&mut dst, &left, 0, 0, left_blank);
    blit_pane_into(&mut dst, &right, 0, 6, right_blank);
    assert!(
        dst.line_size_spans.iter().all(std::vec::Vec::is_empty),
        "an all-single-width split must record no runs"
    );
    assert!(
        aterm_render::row_is_uniform(&dst, 0),
        "the renderers must still see row 0 as uniform"
    );
}

/// Panes need not blit left to right: the DEC pane records exactly one run over its own
/// columns in either order.
#[test]
fn blit_records_pane_local_line_size_spans() {
    use aterm_core::grid::LineSize;
    use aterm_core::render::LineSizeSpan;

    let theme = Theme::default();
    let (left, left_blank) = pane_snapshot("AB", 1, 2);
    let mut right_term = Terminal::new(1, 2);
    // Set only the row mode: a 2-column DECDWL row has one visible position, so text would
    // wrap and scroll this one-row terminal onto a fresh SingleWidth line.
    right_term.process(b"\x1b#6");
    let mut right = RenderInput::empty();
    right_term.cell_frame_into(&mut right, 1, 2);
    let right_blank = terminal_blank_cell(&right_term);
    assert_eq!(right.line_sizes, vec![LineSize::DoubleWidth]);

    // Only the DEC pane records a run; an unclaimed column renders SingleWidth, which keeps
    // ordinary splits on the uniform path.
    let expected = vec![LineSizeSpan::new(3, 5, LineSize::DoubleWidth)];
    for reverse in [false, true] {
        let mut dst = RenderInput::empty();
        fill_divider_grid(&mut dst, 1, 5, theme, None);
        if reverse {
            blit_pane_into(&mut dst, &right, 0, 3, right_blank);
            blit_pane_into(&mut dst, &left, 0, 0, left_blank);
        } else {
            blit_pane_into(&mut dst, &left, 0, 0, left_blank);
            blit_pane_into(&mut dst, &right, 0, 3, right_blank);
        }
        assert_eq!(
            dst.line_size_spans[0], expected,
            "pane spans are stable regardless of blit order"
        );
        assert_eq!(
            dst.line_sizes,
            vec![LineSize::DoubleWidth],
            "the row-level value survives only as the summary `some pane on \
             this row is a DEC line` — placement reads the runs"
        );
        assert_eq!(
            dst.line_size_run_at(0, 0).0,
            LineSize::SingleWidth,
            "the ordinary pane beside it is still an ordinary line"
        );
    }
}

/// One scalar cannot hold each pane's OSC 11/DECSCNM default: each pane row records its own
/// implicit background span, the divider gap keeps the scalar, and blit order cannot change it.
#[test]
fn blit_records_pane_local_default_background_spans() {
    use aterm_core::render::{COLOR_UNSET, DefaultBgSpan};

    let theme = Theme::default();
    let (left, mut left_blank) = pane_snapshot("AB", 1, 2);
    left_blank.bg = [0x11, 0x22, 0x33];
    let (right, mut right_blank) = pane_snapshot("CD", 1, 2);
    right_blank.bg = [0x44, 0x55, 0x66];
    let expected = vec![
        DefaultBgSpan::new(0, 2, 0x0011_2233),
        DefaultBgSpan::new(3, 5, 0x0044_5566),
    ];

    for reverse in [false, true] {
        let mut dst = RenderInput::empty();
        fill_divider_grid(&mut dst, 1, 5, theme, None);
        if reverse {
            blit_pane_into(&mut dst, &right, 0, 3, right_blank);
            blit_pane_into(&mut dst, &left, 0, 0, left_blank);
        } else {
            blit_pane_into(&mut dst, &left, 0, 0, left_blank);
            blit_pane_into(&mut dst, &right, 0, 3, right_blank);
        }
        assert_eq!(dst.default_bg_spans[0], expected);
        assert_eq!(dst.default_bg_at(0, 0), 0x0011_2233);
        assert_eq!(dst.default_bg_at(0, 1), 0x0011_2233);
        assert_eq!(
            dst.default_bg_at(0, 2),
            COLOR_UNSET,
            "divider gap uses the frame scalar"
        );
        assert_eq!(dst.default_bg_at(0, 3), 0x0044_5566);
        assert_eq!(dst.default_bg_at(0, 4), 0x0044_5566);
    }
}

/// A native tray's underlay (it may antialias transparently) is a themed blank pane, never the
/// divider sentinel, with the same SingleWidth pane clip as a terminal leaf.
#[test]
fn native_blank_underlay_replaces_only_its_pane_rectangle() {
    use aterm_core::grid::LineSize;
    use aterm_core::render::LineSizeSpan;

    let theme = Theme::default();
    let seam = divider_cell(theme);
    let mut blank = seam;
    blank.bg = [3, 5, 7];
    blank.fg = [211, 212, 213];
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 3, 5, theme, None);
    fill_blank_pane_into(&mut dst, 1, 2, 2, 2, blank);

    assert!(dst.cells[0].iter().all(|cell| *cell == seam));
    for row in 1..3 {
        assert_eq!(dst.cells[row][0], seam);
        assert_eq!(dst.cells[row][1], seam);
        assert_eq!(dst.cells[row][2], blank);
        assert_eq!(dst.cells[row][3], blank);
        assert_eq!(dst.cells[row][4], seam);
        assert_eq!(
            dst.line_size_spans[row],
            vec![LineSizeSpan::new(2, 4, LineSize::SingleWidth)]
        );
        assert_eq!(
            dst.default_bg_spans[row],
            vec![aterm_core::render::DefaultBgSpan::new(2, 4, 0x0003_0507)]
        );
    }
}

/// A snapshot keeps unwritten row tails sparse; the blit still paints the pane's full rectangle
/// with the terminal's implicit blank, or the divider sentinel shows through as gray bands.
#[test]
fn blit_ragged_rows_paints_full_pane_rectangles() {
    let theme = Theme::default();
    let seam = divider_cell(theme);
    let mut term = Terminal::new(3, 5);
    term.set_default_foreground(aterm_core::terminal::Rgb {
        r: 211,
        g: 212,
        b: 213,
    });
    term.set_default_background(aterm_core::terminal::Rgb { r: 7, g: 11, b: 13 });
    // The final materialized cell has a non-default background. Padding must
    // use the implicit terminal blank, never clone this cell's SGR style.
    term.process(b"AB\x1b[1;41mC");
    let mut pane = RenderInput::empty();
    term.cell_frame_into(&mut pane, 3, 5);
    let blank = terminal_blank_cell(&term);
    assert_eq!(pane.cells[0].len(), 3, "row 0 is intentionally ragged");
    assert_ne!(
        pane.cells[0][2].bg, blank.bg,
        "the last materialized cell is a negative control with styled bg"
    );
    assert!(pane.cells[1].is_empty(), "row 1 is intentionally sparse");
    assert!(pane.cells[2].is_empty(), "row 2 is intentionally sparse");

    // 3x11 window: [left pane 0..5] [divider 5] [right pane 6..11].
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 3, 11, theme, None);
    blit_pane_into(&mut dst, &pane, 0, 0, blank);
    blit_pane_into(&mut dst, &pane, 0, 6, blank);

    for (r, row) in dst.cells.iter().enumerate() {
        assert_eq!(row[5], seam, "only the separator column remains seam");
        for c in 0..5 {
            let expected = pane.cells[r].get(c).copied().unwrap_or(blank);
            assert_eq!(row[c], expected, "left pane mismatch at ({r},{c})");
            assert_eq!(row[6 + c], expected, "right pane mismatch at ({r},{c})");
        }
    }
}

/// A blit that would overflow the composite (a degenerate too-small window) is
/// bounds-checked: it writes only the cells that fit, never past the row.
#[test]
fn blit_is_bounds_checked() {
    let theme = Theme::default();
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 1, 3, theme, None);
    // A 1x5 pane blitted at col_off 1 into a 3-wide composite: only cols 1,2 fit.
    let (wide, blank) = pane_snapshot("VWXYZ", 1, 5);
    blit_pane_into(&mut dst, &wide, 0, 1, blank);
    assert_eq!(dst.cells[0].len(), 3, "the composite row is not grown");
    assert_eq!(dst.cells[0][1].ch, 'V');
    assert_eq!(dst.cells[0][2].ch, 'W');
}

/// A 1x1 inline-image ref for the compose-path image tests.
fn test_image_ref() -> aterm_core::grid::extra::ImageRef {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
    use std::sync::Arc;
    ImageRef {
        image: Arc::new(ImageData {
            bytes: vec![1, 2, 3, 4],
            format: ImageFormat::RawRgba8 {
                width: 1,
                height: 1,
            },
            cols: 1,
            rows: 1,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        }),
        cell_row: 0,
        cell_col: 0,
        kitty: None,
    }
}

/// `fill_divider_grid` rebuilds `images` to exactly `rows` empty rows, keeping it aligned with
/// `cells` for the per-row damage gate.
#[test]
fn divider_grid_resets_stale_images_to_window_rows() {
    let theme = Theme::default();
    let mut dst = RenderInput::empty();
    // A stale, oversized images vec from an earlier compose.
    dst.images = vec![Vec::new(); 7];
    dst.images[2].push((1, test_image_ref()));
    fill_divider_grid(&mut dst, 3, 5, theme, None);
    assert_eq!(
        dst.images.len(),
        3,
        "images is rebuilt to exactly `rows`, not left at the stale length"
    );
    assert_eq!(
        dst.cells.len(),
        dst.images.len(),
        "images stays cells-aligned"
    );
    assert!(
        dst.images.iter().all(Vec::is_empty),
        "every image row is reset to empty (no stale placements)"
    );
}

/// `blit_pane_into` copies a pane's inline-image placements into the composite,
/// shifted by `col_off` like clusters and combining marks.
#[test]
fn blit_copies_pane_images_col_shifted() {
    let theme = Theme::default();
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 1, 5, theme, None);
    // A 1x2 right pane (blitted at col_off 3) carrying an image at its local col 0.
    let (mut right, blank) = pane_snapshot("CD", 1, 2);
    let im = test_image_ref();
    right.images[0].push((0, im.clone()));
    blit_pane_into(&mut dst, &right, 0, 3, blank);
    assert_eq!(
        dst.images.len(),
        1,
        "blit does not change the composite's images length"
    );
    assert_eq!(
        dst.images[0],
        vec![(3, im)],
        "the pane image lands at composite col 3 (col_off 3 + local 0)"
    );
}

/// During resize convergence a snapshot can carry metadata from a wider grid: every sparse
/// channel clips at `src.cols` like the cells, or it lands on the divider or neighbour.
#[test]
fn blit_clips_sparse_metadata_to_pane_rectangle() {
    let theme = Theme::default();
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 1, 5, theme, None);
    let (mut pane, blank) = pane_snapshot("AB", 1, 2);
    pane.clusters[0] = vec![(1, "inside".into()), (2, "outside".into())];
    pane.combining[0] = vec![
        (1, vec!['\u{301}'].into_boxed_slice()),
        (2, vec!['\u{308}'].into_boxed_slice()),
    ];
    let inside_image = test_image_ref();
    let outside_image = test_image_ref();
    pane.images[0] = vec![(1, inside_image.clone()), (2, outside_image)];

    blit_pane_into(&mut dst, &pane, 0, 0, blank);

    assert_eq!(
        dst.clusters[0]
            .iter()
            .map(|(col, _)| *col)
            .collect::<Vec<_>>(),
        vec![1],
        "cluster metadata is clipped before the divider at local col 2"
    );
    assert_eq!(
        dst.combining[0]
            .iter()
            .map(|(col, _)| *col)
            .collect::<Vec<_>>(),
        vec![1],
        "combining metadata is clipped before the divider at local col 2"
    );
    assert_eq!(
        dst.images[0],
        vec![(1, inside_image)],
        "image metadata is clipped before the divider at local col 2"
    );
}

/// A wider snapshot can cut a wide glyph at the pane boundary: the orphaned lead goes too, or
/// both renderers paint its two-cell glyph over the divider.
#[test]
fn blit_drops_wide_glyph_cut_by_pane_boundary() {
    let theme = Theme::default();
    let seam = divider_cell(theme);
    let mut term = Terminal::new(1, 3);
    term.process("A😀".as_bytes());
    let mut pane = RenderInput::empty();
    term.cell_frame_into(&mut pane, 1, 2);
    let blank = terminal_blank_cell(&term);
    assert_eq!(pane.cols, 2);
    assert!(
        pane.cells[0].get(2).is_some_and(|cell| cell.wide),
        "fixture has a clipped wide-glyph continuation at local col 2"
    );

    // [two-column pane] [divider] [two-column sibling].
    let mut dst = RenderInput::empty();
    fill_divider_grid(&mut dst, 1, 5, theme, None);
    blit_pane_into(&mut dst, &pane, 0, 0, blank);

    assert_eq!(dst.cells[0][0].ch, 'A');
    assert_eq!(
        dst.cells[0][1], blank,
        "the orphaned wide lead is replaced by the pane's implicit blank"
    );
    assert_eq!(dst.cells[0][2], seam, "the divider remains untouched");
    assert!(
        dst.clusters[0].iter().all(|(col, _)| *col != 1),
        "wide-glyph cluster metadata is dropped with the orphaned lead"
    );
}

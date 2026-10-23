// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::app_render::{pixel_to_term_cell, prepend_strip_rows, strip_col_for_pixel};
use crate::tab_bar;
use aterm_core::terminal::Terminal;
use aterm_render::RenderInput;

#[test]
fn pixel_to_term_cell_composes_strip_pad_top_pad_and_headroom() {
    type Row = (&'static str, f64, f64, u16, usize, usize, usize, (u16, u16));
    #[rustfmt::skip]
    let rows: [Row; 13] = [
        ("no strip: the origin", 0.0, 0.0, 0, 0, 0, 0, (0, 0)),
        ("no strip: y=32 → row 2 (32/16), x=24 → col 3 (24/8)", 24.0, 32.0, 0, 0, 0, 0, (2, 3)),
        ("1-row strip: y just past the strip (16px) → row 0", 0.0, 16.0, 1, 0, 0, 0, (0, 0)),
        ("1-row strip: y=48 (window row 3) → row 2", 0.0, 48.0, 1, 0, 0, 0, (2, 0)),
        ("2-row strip: y=80 (window row 5) → row 3", 0.0, 80.0, 2, 0, 0, 0, (3, 0)),
        ("inside the 1-row strip (y=8) clamps to row 0", 0.0, 8.0, 1, 0, 0, 0, (0, 0)),
        ("head 30: a click in the band clamps to row 0, col from raw x", 24.0, 10.0, 0, 0, 0, 30, (0, 3)),
        ("head 30: y = head + 2·ch → row 2", 0.0, 62.0, 0, 0, 0, 30, (2, 0)),
        ("pad 8 + 1-row strip: the first terminal cell", 8.0, 24.0, 1, 8, 8, 0, (0, 0)),
        ("pad 8 + 1-row strip: one row deeper, three cols right", 32.0, 40.0, 1, 8, 8, 0, (1, 3)),
        ("pad_top 8 < pad 12: y=8 is the grid top, x=12 the first column", 12.0, 8.0, 0, 12, 8, 0, (0, 0)),
        ("pad_top 8 < pad 12: one cell down, two cols right", 28.0, 24.0, 0, 12, 8, 0, (1, 2)),
        ("pad_top 8 < pad 12: x inside the left pad saturates to col 0", 8.0, 24.0, 0, 12, 8, 0, (1, 0)),
    ];
    for (label, x, y, strip, pad, pad_top, head, expected) in rows {
        assert_eq!(
            pixel_to_term_cell(x, y, 8, 16, 24, 80, strip, pad, pad_top, head),
            expected,
            "{label}"
        );
    }
}

#[test]
fn strip_col_for_pixel_hits_only_the_strip_band() {
    type Row = (
        &'static str,
        f64,
        f64,
        u16,
        usize,
        usize,
        usize,
        Option<u16>,
    );
    #[rustfmt::skip]
    let rows: [Row; 10] = [
        ("1-row strip: y=0, x=24 → col 3", 24.0, 0.0, 1, 0, 0, 0, Some(3)),
        ("1-row strip: y=16, the strip's bottom edge, is the terminal", 0.0, 16.0, 1, 0, 0, 0, None),
        ("2-row strip: y=20 is still inside (< 32px)", 0.0, 20.0, 2, 0, 0, 0, Some(0)),
        ("the column clamps to the last grid column", 10_000.0, 0.0, 1, 0, 0, 0, Some(79)),
        ("head 30: inside the band → its col", 24.0, 38.0, 1, 0, 0, 30, Some(3)),
        ("head 30: at the band's bottom edge → the terminal", 0.0, 46.0, 1, 0, 0, 30, None),
        ("pad 8: the top pad over the strip → strip column", 32.0, 0.0, 1, 8, 8, 0, Some(3)),
        ("pad 8: y = pad (top of the strip), x = pad → col 0", 8.0, 8.0, 1, 8, 8, 0, Some(0)),
        ("pad 8: y = pad + ch, where the terminal begins", 0.0, 24.0, 1, 8, 8, 0, None),
        ("pad 8: the top-left pad corner clamps to col 0, not None", 0.0, 0.0, 1, 8, 8, 0, Some(0)),
    ];
    for (label, x, y, strip, pad, pad_top, head, expected) in rows {
        assert_eq!(
            strip_col_for_pixel(x, y, 8, 16, 80, strip, pad, pad_top, head),
            expected,
            "{label}"
        );
    }
}

#[test]
fn prepend_shifts_content_and_cursor_down() {
    let mut term = Terminal::new(2, 4);
    term.process(b"AB\r\nCD");
    let mut frame = RenderInput::empty();
    term.cell_frame_into(&mut frame, 2, 4);
    let before_rows = frame.rows;
    let before_cursor = frame.cursor_row;
    let theme = aterm_render::Theme::default();
    let strip_row = vec![tab_bar::blank_cell(theme); 4];
    frame.cursor_glow_add.push(aterm_render::GlowQuad {
        row: 0,
        x: 0,
        y: 5,
        w: 1,
        h: 1,
        color: 0,
        alpha: 0,
        color2: 0,
        alpha2: 0,
    });
    frame.cursor_glow_add.push(aterm_render::GlowQuad {
        row: 1,
        x: 0,
        y: 40,
        w: 1,
        h: 1,
        color: 0,
        alpha: 0,
        color2: 0,
        alpha2: 0,
    });
    frame.cursor_trail.push(aterm_render::TrailCell {
        row: 1,
        col: 0,
        alpha: 255,
    });
    frame.ink.push(aterm_render::InkCell {
        row: 1,
        col: 2,
        color: [1, 2, 3],
    });
    frame.cat_quads.push(aterm_render::SpriteQuad {
        row: 1,
        x: 3,
        y: 26,
        w: 8,
        h: 4,
        ax: 0,
        ay: 8,
        aw: 8,
        ah: 4,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    });
    frame.free_sprites.push(aterm_core::render::FreeSprite {
        x: 3,
        y: -7, // peeking in from above row 0: the signed case survives
        w: 8,
        h: 40,
        ax: 0,
        ay: 8,
        aw: 8,
        ah: 40,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: aterm_core::render::FreeZ::UnderText,
        sampler: aterm_core::render::FreeSampler::Nearest,
    });
    prepend_strip_rows(&mut frame, &[strip_row], 16, 0, &mut Vec::new());
    assert_eq!(frame.rows, before_rows + 1);
    assert_eq!(frame.cells.len(), before_rows + 1);
    assert_eq!(frame.cursor_row, before_cursor + 1);
    assert_eq!(frame.cursor_glow_add[0].row, 0);
    assert_eq!(frame.cursor_glow_add[0].y, 5);
    assert_eq!(frame.cursor_glow_add[1].row, 2);
    assert_eq!(frame.cursor_glow_add[1].y, 40);
    assert_eq!(frame.cursor_trail[0].row, 2);
    assert_eq!(
        frame.ink[0],
        aterm_render::InkCell {
            row: 2,
            col: 2,
            color: [1, 2, 3]
        }
    );
    assert_eq!(frame.cat_quads[0].row, 2);
    assert_eq!(frame.cat_quads[0].y, 26 + 16);
    assert_eq!(
        (frame.cat_quads[0].ax, frame.cat_quads[0].ay),
        (0, 8),
        "splice never touches source texels"
    );
    assert_eq!(frame.free_sprites[0].y, -7 + 16);
    assert_eq!(frame.free_sprites[0].x, 3, "x is not a strip axis");
    assert_eq!(
        (frame.free_sprites[0].ay, frame.free_sprites[0].h),
        (8, 40),
        "splice never touches free source texels/extents"
    );
    assert_eq!(frame.cells[1][0].ch, 'A');
    assert_eq!(frame.cells[1][1].ch, 'B');
    assert_eq!(frame.cells[2][0].ch, 'C');
    assert_eq!(frame.clusters.len(), frame.cells.len());
    assert_eq!(frame.combining.len(), frame.cells.len());
    assert_eq!(frame.images.len(), frame.cells.len());
    assert_eq!(frame.line_sizes.len(), frame.cells.len());
    assert_eq!(frame.line_size_spans.len(), frame.cells.len());
    assert_eq!(frame.default_bg_spans.len(), frame.cells.len());
    assert!(
        frame.line_size_spans[0].is_empty(),
        "strip chrome uses the row-wide SingleWidth fallback"
    );
    assert!(
        frame.default_bg_spans[0].is_empty(),
        "strip chrome uses the frame-wide background fallback"
    );
}

#[test]
fn prepend_multi_row_strip_tags_by_pixel_band() {
    let mut term = Terminal::new(2, 4);
    term.process(b"AB");
    let mut frame = RenderInput::empty();
    term.cell_frame_into(&mut frame, 2, 4);
    let theme = aterm_render::Theme::default();
    let strip: Vec<Vec<_>> = (0..2)
        .map(|_| vec![tab_bar::blank_cell(theme); 4])
        .collect();
    for y in [5u16, 20, 40] {
        frame.cursor_glow_add.push(aterm_render::GlowQuad {
            row: 0, // producer hint — the splice re-derives from the pixel
            x: 0,
            y,
            w: 1,
            h: 1,
            color: 0,
            alpha: 0,
            color2: 0,
            alpha2: 0,
        });
    }
    prepend_strip_rows(&mut frame, &strip, 16, 0, &mut Vec::new());
    let rows: Vec<u16> = frame.cursor_glow_add.iter().map(|q| q.row).collect();
    assert_eq!(
        rows,
        vec![0, 1, 2],
        "tags derive from the pixel band: window-top, strip row 1, first terminal row"
    );
    let ys: Vec<u16> = frame.cursor_glow_add.iter().map(|q| q.y).collect();
    assert_eq!(ys, vec![5, 20, 40]);
}

#[test]
fn end_to_end_strip_above_terminal() {
    let theme = aterm_render::Theme::default();
    let cols = 40usize;
    let mut term = Terminal::new(3, cols as u16);
    term.process(b"prompt$ ");
    let mut frame = RenderInput::empty();
    term.cell_frame_into(&mut frame, 3, cols);
    let term_cursor_row = frame.cursor_row;
    let segments = tab_bar::layout_segments(cols as u16, 2, 0, false);
    let titles = vec!["zsh".to_string(), "vim".to_string()];
    let mut strip_row = vec![tab_bar::blank_cell(theme); cols];
    tab_bar::paint_strip(&mut strip_row, &segments, &titles, None, 0, theme);
    prepend_strip_rows(&mut frame, &[strip_row], 16, 0, &mut Vec::new());
    assert_eq!(frame.rows, 4);
    let t0 = &segments[0];
    let ts = t0.start_col as usize + 1 + usize::from(tab_bar::STRIP_CHIP_CARDS);
    assert_eq!(frame.cells[0][ts].ch, 'z');
    let plus = segments.last().unwrap();
    assert_eq!(frame.cells[0][(plus.start_col + 1) as usize].ch, '+');
    assert_eq!(frame.cells[1][0].ch, 'p'); // "prompt$ "
    assert_eq!(frame.cursor_row, term_cursor_row + 1);
}

#[test]
fn prepend_empty_is_noop() {
    let mut term = Terminal::new(2, 4);
    term.process(b"AB\r\nCD");
    let mut frame = RenderInput::empty();
    term.cell_frame_into(&mut frame, 2, 4);
    let snapshot = frame.cells.clone();
    let rows = frame.rows;
    let cursor = frame.cursor_row;
    prepend_strip_rows(&mut frame, &[], 16, 0, &mut Vec::new());
    assert_eq!(frame.cells, snapshot, "no strip → grid unchanged");
    assert_eq!(frame.rows, rows);
    assert_eq!(frame.cursor_row, cursor);
}

#[test]
fn strip_row_pool_reuses_buffer_byte_identical() {
    let theme = aterm_render::Theme::default();
    let cols = 4usize;
    let build = || {
        let mut term = Terminal::new(2, cols as u16);
        term.process(b"AB\r\nCD");
        let mut frame = RenderInput::empty();
        term.cell_frame_into(&mut frame, 2, cols);
        frame
    };
    let strip_row = vec![tab_bar::blank_cell(theme); cols];
    let mut frame_a = build();
    prepend_strip_rows(
        &mut frame_a,
        std::slice::from_ref(&strip_row),
        16,
        0,
        &mut Vec::new(),
    );
    let mut frame_b = build();
    let mut pooled = Vec::with_capacity(cols + 32);
    pooled.push(tab_bar::blank_cell(theme));
    let pooled_ptr = pooled.as_ptr();
    let mut pool = vec![pooled];
    prepend_strip_rows(&mut frame_b, &[strip_row], 16, 0, &mut pool);
    assert!(pool.is_empty(), "the pooled buffer was consumed");
    assert_eq!(
        frame_b.cells[0].as_ptr(),
        pooled_ptr,
        "row 0 reused the pooled allocation"
    );
    assert_eq!(frame_a.cells, frame_b.cells);
    assert_eq!(frame_a.rows, frame_b.rows);
    assert_eq!(frame_a.cursor_row, frame_b.cursor_row);
}

#[test]
fn selection_autoscroll_edges() {
    use crate::app_render::selection_autoscroll_lines;
    let (ch, pad, strip_px, rows) = (16usize, 0usize, 0usize, 24u16);
    assert_eq!(selection_autoscroll_lines(0.0, pad, strip_px, ch, rows), 0);
    assert_eq!(
        selection_autoscroll_lines(200.0, pad, strip_px, ch, rows),
        0
    );
    assert_eq!(
        selection_autoscroll_lines(383.0, pad, strip_px, ch, rows),
        0
    );
    assert_eq!(selection_autoscroll_lines(-1.0, pad, strip_px, ch, rows), 1);
    assert_eq!(
        selection_autoscroll_lines(-16.0, pad, strip_px, ch, rows),
        2
    );
    assert_eq!(
        selection_autoscroll_lines(384.0, pad, strip_px, ch, rows),
        -1
    );
    assert_eq!(
        selection_autoscroll_lines(400.0, pad, strip_px, ch, rows),
        -2
    );
}

#[test]
fn an_absurd_overshoot_pins_the_step_count_without_flipping_direction() {
    use crate::app_render::selection_autoscroll_lines;
    let (ch, pad, strip_px, rows) = (16usize, 0usize, 0usize, 24u16);
    let bottom = (rows as usize * ch) as f64;
    assert_eq!(
        selection_autoscroll_lines(-1e12, pad, strip_px, ch, rows),
        i32::MAX,
        "far above the top edge still scrolls INTO history"
    );
    assert_eq!(
        selection_autoscroll_lines(bottom + 1e12, pad, strip_px, ch, rows),
        -i32::MAX,
        "far below the bottom edge still scrolls TOWARD the live bottom"
    );
    for ch in [0, 1] {
        assert_eq!(
            selection_autoscroll_lines(f64::NEG_INFINITY, pad, strip_px, ch, rows),
            i32::MAX,
            "unbounded top overshoot saturates even at the minimum cell height"
        );
        assert_eq!(
            selection_autoscroll_lines(f64::INFINITY, pad, strip_px, ch, rows),
            -i32::MAX,
            "unbounded bottom overshoot saturates even at the minimum cell height"
        );
    }
}

#[test]
fn selection_autoscroll_respects_pad_and_strip() {
    use crate::app_render::selection_autoscroll_lines;
    let (ch, pad, rows) = (16usize, 8usize, 10u16);
    let strip_px = ch; // a 1-row strip
    let top = (pad + strip_px) as f64; // = 24.0
    assert_eq!(selection_autoscroll_lines(top, pad, strip_px, ch, rows), 0);
    assert_eq!(
        selection_autoscroll_lines(top - 1.0, pad, strip_px, ch, rows),
        1
    );
    let bottom = top + (rows as usize * ch) as f64;
    assert_eq!(
        selection_autoscroll_lines(bottom, pad, strip_px, ch, rows),
        -1
    );
    assert_eq!(
        selection_autoscroll_lines(bottom - 1.0, pad, strip_px, ch, rows),
        0
    );
}

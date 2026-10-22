// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::app_render::{pixel_to_term_cell, prepend_strip_rows, strip_col_for_pixel};
use crate::tab_bar;
use aterm_core::terminal::Terminal;
use aterm_render::RenderInput;

/// `pixel_to_term_cell` over an 8×16 cell, 24×80 grid: the strip, the padding, the
/// independent top pad and the chrome headroom each compose on the axes they own.
#[test]
fn pixel_to_term_cell_composes_strip_pad_top_pad_and_headroom() {
    // (label, x, y, strip_rows, pad, pad_top, head) → (row, col)
    type Row = (&'static str, f64, f64, u16, usize, usize, usize, (u16, u16));
    #[rustfmt::skip]
    let rows: [Row; 13] = [
        // No strip, pad or headroom: plain division.
        ("no strip: the origin", 0.0, 0.0, 0, 0, 0, 0, (0, 0)),
        ("no strip: y=32 → row 2 (32/16), x=24 → col 3 (24/8)", 24.0, 32.0, 0, 0, 0, 0, (2, 3)),
        // A strip shifts the terminal region down by strip·ch.
        ("1-row strip: y just past the strip (16px) → row 0", 0.0, 16.0, 1, 0, 0, 0, (0, 0)),
        ("1-row strip: y=48 (window row 3) → row 2", 0.0, 48.0, 1, 0, 0, 0, (2, 0)),
        ("2-row strip: y=80 (window row 5) → row 3", 0.0, 80.0, 2, 0, 0, 0, (3, 0)),
        // Inside the strip clamps to row 0 — the safety net; the caller
        // intercepts strip clicks through `strip_col_for_pixel` first.
        ("inside the 1-row strip (y=8) clamps to row 0", 0.0, 8.0, 1, 0, 0, 0, (0, 0)),
        // The chrome headroom composes on y only.
        ("head 30: a click in the band clamps to row 0, col from raw x", 24.0, 10.0, 0, 0, 0, 30, (0, 3)),
        ("head 30: y = head + 2·ch → row 2", 0.0, 62.0, 0, 0, 0, 30, (2, 0)),
        // Padding comes off both axes before the strip offset: the first terminal cell
        // sits at y = pad + strip·ch, x = pad.
        ("pad 8 + 1-row strip: the first terminal cell", 8.0, 24.0, 1, 8, 8, 0, (0, 0)),
        ("pad 8 + 1-row strip: one row deeper, three cols right", 32.0, 40.0, 1, 8, 8, 0, (1, 3)),
        // The top pad is independent: pad_top 8 < pad 12 moves the grid's
        // Y-origin to 8 while the X-origin stays at pad.
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

/// `strip_col_for_pixel` over an 8×16 cell, 80-column grid: a pixel in the strip's band
/// returns its column (clamped to the last), one below it `None`; pad and headroom move
/// the band.
#[test]
fn strip_col_for_pixel_hits_only_the_strip_band() {
    // (label, x, y, strip_rows, pad, pad_top, head) → column
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
        // Headroom 30: the band is [head, head + ch).
        ("head 30: inside the band → its col", 24.0, 38.0, 1, 0, 0, 30, Some(3)),
        ("head 30: at the band's bottom edge → the terminal", 0.0, 46.0, 1, 0, 0, 30, None),
        // Pad 8: the band is [pad, pad + ch) in y, and gx = x - pad.
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

/// `prepend_strip_rows` shifts a terminal frame down by the strip rows: content and cursor
/// move down, the row count grows, and every per-row vector gains `strip` leading rows.
#[test]
fn prepend_shifts_content_and_cursor_down() {
    // A 2x4 terminal frame with a cursor at row 1.
    let mut term = Terminal::new(2, 4);
    term.process(b"AB\r\nCD");
    let mut frame = RenderInput::empty();
    term.cell_frame_into(&mut frame, 2, 4);
    let before_rows = frame.rows;
    let before_cursor = frame.cursor_row;
    // Build one strip row and splice it on top.
    let theme = aterm_render::Theme::default();
    let strip_row = vec![tab_bar::blank_cell(theme); 4];
    // A glow quad (row 0, pixel y=5): a window-space stream, so its pixel y does not
    // move, and a quad above terminal row 0 (grid_top + strip·cell_h = 16) tags row 0.
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
    // A second glow quad inside the grid (y=40 ≥ 16): pixel y still untouched, but its
    // row tag shifts with the grid content it lights.
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
    // An animated-ink override (cell-row tagged, emitted pre-splice) must
    // shift down with its glyph too.
    frame.ink.push(aterm_render::InkCell {
        row: 1,
        col: 2,
        color: [1, 2, 3],
    });
    // A peeking-cat quad (row band plus pixel-y dest rect) shifts both.
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
    // A free-overlay sprite (a pixel rect, no row tag, signed origin) shifts its dest y
    // alone.
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
    // The frame grew by one row, the terminal content shifted down by one, and
    // the cursor followed.
    assert_eq!(frame.rows, before_rows + 1);
    assert_eq!(frame.cells.len(), before_rows + 1);
    assert_eq!(frame.cursor_row, before_cursor + 1);
    // The glow quad's pixel y is window-absolute, and its above-grid band (y=5 < 16) tags
    // composed row 0, so the top band's damage is not stranded.
    assert_eq!(frame.cursor_glow_add[0].row, 0);
    assert_eq!(frame.cursor_glow_add[0].y, 5);
    // The in-grid quad keeps its window-absolute y and shifts only its tag.
    assert_eq!(frame.cursor_glow_add[1].row, 2);
    assert_eq!(frame.cursor_glow_add[1].y, 40);
    assert_eq!(frame.cursor_trail[0].row, 2);
    // The ink override followed its row down (col + color untouched).
    assert_eq!(
        frame.ink[0],
        aterm_render::InkCell {
            row: 2,
            col: 2,
            color: [1, 2, 3]
        }
    );
    // The cat quad's row band and pixel y both moved by the strip; the
    // source rect (atlas texels) is untouched.
    assert_eq!(frame.cat_quads[0].row, 2);
    assert_eq!(frame.cat_quads[0].y, 26 + 16);
    assert_eq!(
        (frame.cat_quads[0].ax, frame.cat_quads[0].ay),
        (0, 8),
        "splice never touches source texels"
    );
    // The free sprite's signed dest y moved by `strip · cell_h` (−7 → 9); extents and
    // source texels are untouched.
    assert_eq!(frame.free_sprites[0].y, -7 + 16);
    assert_eq!(frame.free_sprites[0].x, 3, "x is not a strip axis");
    assert_eq!(
        (frame.free_sprites[0].ay, frame.free_sprites[0].h),
        (8, 40),
        "splice never touches free source texels/extents"
    );
    // Row 0 is now the strip; the original first terminal row ('A','B') is at row 1.
    assert_eq!(frame.cells[1][0].ch, 'A');
    assert_eq!(frame.cells[1][1].ch, 'B');
    assert_eq!(frame.cells[2][0].ch, 'C');
    // Per-row vectors stayed aligned with `cells` (one leading row each).
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

/// With a two-row strip, a window-space effect quad in strip row 1's band is tagged
/// composed row 1 (the tag derives from pixels), so that band's damage is refreshed.
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
    // grid_top = 0 (no pad/head in this fixture): composed bands are
    // row 0 = [0,16), row 1 = [16,32) (strip row 1), terminal rows from 32.
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
    // Pixel coordinates ride untouched (window-absolute streams).
    let ys: Vec<u16> = frame.cursor_glow_add.iter().map(|q| q.y).collect();
    assert_eq!(ys, vec![5, 20, 40]);
}

/// End-to-end composition (as `splice_tab_strip` does it): lay out the strip,
/// paint it, and splice it above a terminal frame. The composed frame is
/// `terminal_rows + 1` tall, the strip row carries a tab title + the `+`, and the
/// terminal content + cursor sit one row lower.
#[test]
fn end_to_end_strip_above_terminal() {
    let theme = aterm_render::Theme::default();
    let cols = 40usize;
    // Terminal frame: 3x40 with some text + a cursor.
    let mut term = Terminal::new(3, cols as u16);
    term.process(b"prompt$ ");
    let mut frame = RenderInput::empty();
    term.cell_frame_into(&mut frame, 3, cols);
    let term_cursor_row = frame.cursor_row;
    // The strip as `splice_tab_strip` builds it: 2 tabs, tab 0 active.
    let segments = tab_bar::layout_segments(cols as u16, 2, 0, false);
    let titles = vec!["zsh".to_string(), "vim".to_string()];
    let mut strip_row = vec![tab_bar::blank_cell(theme); cols];
    tab_bar::paint_strip(&mut strip_row, &segments, &titles, None, 0, theme);
    prepend_strip_rows(&mut frame, &[strip_row], 16, 0, &mut Vec::new());
    // The composed frame is one row taller; the strip is row 0.
    assert_eq!(frame.rows, 4);
    // The active tab's title starts after the leading pad (plus a chip card's interior
    // pad).
    let t0 = &segments[0];
    let ts = t0.start_col as usize + 1 + usize::from(tab_bar::STRIP_CHIP_CARDS);
    assert_eq!(frame.cells[0][ts].ch, 'z');
    // The `+` affordance is present in the strip row.
    let plus = segments.last().unwrap();
    assert_eq!(frame.cells[0][(plus.start_col + 1) as usize].ch, '+');
    // The terminal content shifted down to row 1, and the cursor followed.
    assert_eq!(frame.cells[1][0].ch, 'p'); // "prompt$ "
    assert_eq!(frame.cursor_row, term_cursor_row + 1);
}

/// An empty strip (`strip_rows == 0`) is a no-op: the frame is unchanged.
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

/// The strip-row pool: `prepend_strip_rows` pops a reclaimed buffer and `clone_from`s
/// the cached row over it (same backing pointer), byte-identical to the empty-pool clone.
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
    // Fallback: an empty pool clones a fresh row.
    let mut frame_a = build();
    prepend_strip_rows(
        &mut frame_a,
        std::slice::from_ref(&strip_row),
        16,
        0,
        &mut Vec::new(),
    );
    // Pooled: an over-capacity, non-empty buffer whose allocation row 0 must reuse.
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
    // Byte parity: pooled present is indistinguishable from the clone fallback.
    assert_eq!(frame_a.cells, frame_b.cells);
    assert_eq!(frame_a.rows, frame_b.rows);
    assert_eq!(frame_a.cursor_row, frame_b.cursor_row);
}

/// Selection autoscroll: inside the grid never scrolls (`0`); above the top scrolls into
/// history (positive); below the bottom toward live (negative); one line per cell-height
/// of overshoot.
#[test]
fn selection_autoscroll_edges() {
    use crate::app_render::selection_autoscroll_lines;
    // 16px cells, no pad, no strip, 24 rows → grid spans y ∈ [0, 384).
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
    // A full cell-height above → +2 (overshoot/ch + 1).
    assert_eq!(
        selection_autoscroll_lines(-16.0, pad, strip_px, ch, rows),
        2
    );
    // At/below the bottom edge (y == 384) → -1 (toward the live bottom).
    assert_eq!(
        selection_autoscroll_lines(384.0, pad, strip_px, ch, rows),
        -1
    );
    assert_eq!(
        selection_autoscroll_lines(400.0, pad, strip_px, ch, rows),
        -2
    );
}

/// Nothing bounds the pointer coordinate a drag outside the window reports, so the step
/// count saturates; a wrapped magnitude would flip the direction.
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

/// The autoscroll edges shift by the interior `pad` and the tab-strip band: the grid
/// starts at `pad + strip_px`.
#[test]
fn selection_autoscroll_respects_pad_and_strip() {
    use crate::app_render::selection_autoscroll_lines;
    let (ch, pad, rows) = (16usize, 8usize, 10u16);
    let strip_px = ch; // a 1-row strip
    let top = (pad + strip_px) as f64; // = 24.0
    // A pixel exactly at the grid top is inside (not above) → 0.
    assert_eq!(selection_autoscroll_lines(top, pad, strip_px, ch, rows), 0);
    assert_eq!(
        selection_autoscroll_lines(top - 1.0, pad, strip_px, ch, rows),
        1
    );
    // The bottom edge is top + rows*ch = 24 + 160 = 184; at/below → negative.
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

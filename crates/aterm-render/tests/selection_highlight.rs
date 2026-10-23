// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selection highlight tests for the CPU renderer.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{Terminal, TerminalBuilder};
use aterm_render::{Frame, Renderer, SelectionClip, Theme};

fn r(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn g(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn b(p: u32) -> i32 {
    (p & 0xff) as i32
}

fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

fn count_eq(px: &[u32], color: u32) -> usize {
    px.iter().filter(|&&p| p == color).count()
}

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

fn selected_term() -> Terminal {
    let mut term = Terminal::new(4, 10);
    term.process(b"hello\r\nworld");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 3, SelectionSide::Right);
    sel.complete_selection();
    term
}

#[test]
fn selected_cells_get_selection_background() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = selected_term();
    let f = rend.render_input(&term.cell_frame(4, 10));
    let sel = Theme::default().selection;

    for c in 1..=3usize {
        let px = cell_pixels(&f, cw, ch, 0, c);
        let n = count_eq(&px, sel);
        assert!(
            n > px.len() / 3,
            "selected cell (0,{c}) should be filled with theme.selection ({n}/{})",
            px.len()
        );
    }

    let px = cell_pixels(&f, cw, ch, 0, 2); // 'l'
    let fg_drawn = px.iter().any(|&p| r(p) > 150 && g(p) > 150 && b(p) > 150);
    assert!(
        fg_drawn,
        "selected cell (0,2) should keep its foreground glyph"
    );
}

#[test]
fn osc_selection_background_and_foreground_reach_selected_pixels() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = selected_term();
    term.process(b"\x1b]17;rgb:12/34/56\x07\x1b]19;rgb:fe/cd/32\x07");
    let input = term.cell_frame(4, 10);
    assert_eq!(input.selection_bg, 0x0012_3456);
    assert_eq!(input.selection_fg, 0x00fe_cd32);

    let frame = rend.render_input(&input);
    let pixels = cell_pixels(&frame, cw, ch, 0, 2);
    assert!(
        count_eq(&pixels, 0x0012_3456) > pixels.len() / 3,
        "OSC 17 must replace the static theme selection fill"
    );
    assert!(
        pixels
            .iter()
            .any(|&p| r(p) > 220 && g(p) > 160 && b(p) < 100),
        "OSC 19 must replace selected-text ink"
    );
    assert_eq!(
        count_eq(&pixels, Theme::default().selection),
        0,
        "the old theme selection colour must not leak through"
    );
}

#[test]
fn selected_sparse_tail_paints_implicit_blanks_and_covers_deepest_image() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"x");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 4, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 6, SelectionSide::Right);
    sel.complete_selection();

    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false;
    assert_eq!(
        input.cells[0].len(),
        1,
        "fixture requires cols 4..=6 to be omitted sparse-tail cells"
    );
    let image = std::sync::Arc::new(aterm_core::grid::extra::ImageData {
        bytes: vec![240, 20, 80, 255],
        format: aterm_core::grid::extra::ImageFormat::RawRgba8 {
            width: 1,
            height: 1,
        },
        cols: 1,
        rows: 1,
        z_index: aterm_render::KITTY_IMAGE_BELOW_BG_Z_THRESHOLD - 1,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: aterm_core::grid::extra::ImageScaling::Fit,
        source_rect: None,
    });
    input.images[0].push((
        5,
        aterm_core::grid::extra::ImageRef {
            image,
            cell_row: 0,
            cell_col: 0,
            kitty: None,
        },
    ));

    let frame = rend.render_input(&input);
    let selection = Theme::default().selection;
    for col in 4..=6 {
        assert!(
            input.cells[0].get(col).is_none(),
            "col {col} must remain unmaterialized"
        );
        let pixels = cell_pixels(&frame, cw, ch, 0, col);
        assert_eq!(
            count_eq(&pixels, selection),
            pixels.len(),
            "selected implicit blank col {col} must be entirely selection-filled"
        );
    }
    assert_eq!(
        count_eq(&cell_pixels(&frame, cw, ch, 0, 5), 0x00f0_1450),
        0,
        "selection must cover a deepest-tier image in an omitted cell"
    );
    let untouched = cell_pixels(&frame, cw, ch, 0, 7);
    assert_eq!(
        count_eq(&untouched, Theme::default().bg),
        untouched.len(),
        "an unselected omitted cell remains the frame default background"
    );
}

#[test]
fn multiline_selection_clip_blocks_sibling_and_divider_backgrounds() {
    let mut rend = Renderer::from_bytes(
        include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf"),
        18.0,
        Theme::default(),
    )
    .expect("the committed mono face");
    let _ = rend.set_runtime_font_discovery(false);
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 9usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let selection = term.text_selection_mut();
    selection.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(2, 7, SelectionSide::Right);
    selection.complete_selection();

    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false;
    input.selection_bg = 0x0021_4365;
    input.selection_clip = Some(SelectionClip::new(0, rows, 5, cols));
    let frame = rend.render_input(&input);

    for (row, col) in [(0, 5), (0, 8), (1, 5), (1, 8), (2, 5), (2, 7)] {
        let pixels = cell_pixels(&frame, cw, ch, row, col);
        assert_eq!(
            count_eq(&pixels, input.selection_bg),
            pixels.len(),
            "focused-pane selected cell ({row},{col}) must be fully highlighted"
        );
    }
    for (row, col) in [(0, 4), (1, 0), (1, 4), (2, 4), (2, 8)] {
        let pixels = cell_pixels(&frame, cw, ch, row, col);
        assert_eq!(
            count_eq(&pixels, Theme::default().bg),
            pixels.len(),
            "sibling/divider cell ({row},{col}) must not receive selection tint"
        );
        assert_eq!(count_eq(&pixels, input.selection_bg), 0);
    }

    let mut term = Terminal::new(1, 6);
    term.process("日 ".as_bytes());
    let selection = term.text_selection_mut();
    selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(0, 0, SelectionSide::Right);
    selection.complete_selection();
    let mut wide = term.cell_frame(1, 6);
    wide.cursor_visible = false;
    wide.selection_bg = 0x0021_4365;
    wide.cells[0][0].ch = ' ';
    assert!(wide.cells[0][1].wide);
    for (clip, lead, continuation) in [
        (Some(SelectionClip::new(0, 1, 1, 6)), false, true),
        (None, true, true),
        (Some(SelectionClip::new(0, 1, 0, 1)), true, false),
    ] {
        wide.selection_clip = clip;
        assert_eq!(wide.selection_hit(0, 0, true, false).is_some(), lead);
        assert_eq!(
            wide.selection_hit(0, 1, false, true).is_some(),
            continuation
        );
        if !lead {
            assert_eq!(wide.selection_row_span(0), None, "the sparse-tail window");
        }
        let frame = rend.render_input(&wide);
        for (col, selected) in [(0, lead), (1, continuation)] {
            let pixels = cell_pixels(&frame, cw, ch, 0, col);
            assert_eq!(
                count_eq(&pixels, wide.selection_bg),
                if selected { pixels.len() } else { 0 },
                "clipped wide half at column {col}: {clip:?}"
            );
        }
    }
    let mut cache = aterm_render::WindowCpu::new();
    let cached = |renderer: &mut Renderer,
                  cache: &mut aterm_render::WindowCpu,
                  input: &aterm_render::RenderInput| {
        let view = renderer.render_input_cached(cache, input);
        Frame {
            width: view.width(),
            height: view.height(),
            pixels: view.pixels().to_vec(),
        }
    };
    wide.selection_clip = Some(SelectionClip::new(0, 1, 1, 6));
    let first = cached(&mut rend, &mut cache, &wide);
    let pixels = cell_pixels(&first, cw, ch, 0, 1);
    assert_eq!(count_eq(&pixels, wide.selection_bg), pixels.len());
    wide.selection_bg = 0x0065_4321;
    let recoloured = cached(&mut rend, &mut cache, &wide);
    let pixels = cell_pixels(&recoloured, cw, ch, 0, 1);
    assert_eq!(count_eq(&pixels, wide.selection_bg), pixels.len());
    assert_eq!(recoloured.pixels, rend.render_input(&wide).pixels);
    wide.selection_clip = Some(SelectionClip::new(0, 1, 2, 6));
    assert_eq!(wide.selection_row_span(0), None);
    let cleared = cached(&mut rend, &mut cache, &wide);
    assert_eq!(
        count_eq(&cell_pixels(&cleared, cw, ch, 0, 1), wide.selection_bg),
        0
    );
    assert_eq!(cleared.pixels, rend.render_input(&wide).pixels);

    wide.cells[0].resize(6, term.implicit_blank_render_cell());
    wide.cells[0].swap(0, 2);
    wide.cells[0].swap(1, 3);
    wide.visual_to_logical = vec![vec![2, 3, 0, 1, 4, 5]];
    wide.selection_clip = Some(SelectionClip::new(0, 1, 3, 6));
    assert_eq!(wide.selection_row_span(0), None);
    assert_eq!(wide.selection_hit(0, 3, false, true), Some(0));
    let moved = cached(&mut rend, &mut cache, &wide);
    let pixels = cell_pixels(&moved, cw, ch, 0, 3);
    assert_eq!(count_eq(&pixels, wide.selection_bg), pixels.len());
    wide.selection_clip = Some(SelectionClip::new(0, 1, 4, 6));
    let moved_clear = cached(&mut rend, &mut cache, &wide);
    assert_eq!(
        count_eq(&cell_pixels(&moved_clear, cw, ch, 0, 3), wide.selection_bg),
        0
    );
    assert_eq!(moved_clear.pixels, rend.render_input(&wide).pixels);

    wide.selection_clip = None;
    assert_eq!(wide.selection_hit(0, 2, true, false), Some(0));
    wide.selection.clear();
    let frame = rend.render_input(&wide);
    for col in 2..=3 {
        assert_eq!(
            count_eq(&cell_pixels(&frame, cw, ch, 0, col), wide.selection_bg),
            0
        );
    }
}

#[test]
fn wide_char_block_selection_highlights_both_halves() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(2, 6);
    term.process("日x".as_bytes()); // 日 occupies cols 0-1, 'x' col 2
    let sel = term.text_selection_mut();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(0, 0, SelectionSide::Right);
    sel.complete_selection();
    let f = rend.render_input(&term.cell_frame(2, 6));
    let selc = Theme::default().selection;

    for c in 0..=1usize {
        let px = cell_pixels(&f, cw, ch, 0, c);
        let n = count_eq(&px, selc);
        assert!(
            n > px.len() / 4,
            "wide-char half (0,{c}) should be selection-filled ({n}/{})",
            px.len()
        );
    }
    let px = cell_pixels(&f, cw, ch, 0, 2); // 'x' is outside the block
    assert_eq!(count_eq(&px, selc), 0, "cell (0,2) must stay unhighlighted");
}

#[test]
fn scrolled_viewport_maps_selection_rows() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = TerminalBuilder::new()
        .size(3, 8)
        .ring_buffer_size(64)
        .build();
    term.process(b"aa\r\nbb\r\ncc\r\ndd");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 1, SelectionSide::Right);
    sel.complete_selection();
    let selc = Theme::default().selection;

    let live = rend.render_input(&term.cell_frame(3, 8));
    assert!(
        count_eq(&cell_pixels(&live, cw, ch, 0, 0), selc) > 0,
        "live: highlight on row 0"
    );
    assert_eq!(
        count_eq(&cell_pixels(&live, cw, ch, 1, 0), selc),
        0,
        "live: row 1 clean"
    );

    term.grid_mut().scroll_display(1);
    assert_eq!(
        term.grid().display_offset(),
        1,
        "scrollback should be active"
    );
    let back = rend.render_input(&term.cell_frame(3, 8));
    assert_eq!(
        count_eq(&cell_pixels(&back, cw, ch, 0, 0), selc),
        0,
        "scrolled: row 0 clean"
    );
    assert!(
        count_eq(&cell_pixels(&back, cw, ch, 1, 0), selc) > 0,
        "scrolled: highlight on row 1"
    );
}

#[test]
fn per_pane_selection_colours_resolve_per_entry_on_the_cpu() {
    use aterm_core::render::{COLOR_UNSET, PaneSelection};

    let theme = Theme::default();
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();

    let (rows, cols) = (2usize, 17usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lleft one|right on\r\nleft two|right tw");
    let mut input = term.cell_frame(rows, cols);

    let pane = |lo: u16, hi: u16, clip: SelectionClip, bg: u32, fg: u32, inactive: bool| {
        let mut selection = aterm_core::selection::TextSelection::new();
        selection.start_selection(0, lo, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(1, hi, SelectionSide::Right);
        selection.complete_selection();
        PaneSelection {
            selection,
            clip,
            bg,
            fg,
            inactive,
        }
    };
    input.selections = vec![
        pane(
            0,
            7,
            SelectionClip::new(0, 2, 0, 8),
            0x0021_4365,
            0x00fe_dcba,
            false,
        ),
        pane(
            9,
            16,
            SelectionClip::new(0, 2, 9, 17),
            COLOR_UNSET,
            COLOR_UNSET,
            true,
        ),
    ];

    let frame = rend.render_input(&input);
    let live = 0x0021_4365;
    let dim = aterm_render::derive_inactive_selection_bg(theme.selection, theme.bg);
    assert_ne!(live, dim, "the fixture's two bands must differ");

    assert!(
        count_eq(&cell_pixels(&frame, cw, ch, 0, 7), live) > 0,
        "the focused pane's band is its own live OSC 17 colour"
    );
    assert!(
        count_eq(&cell_pixels(&frame, cw, ch, 0, 16), dim) > 0,
        "the unfocused pane's band is the derived inactive colour"
    );
    let divider = cell_pixels(&frame, cw, ch, 0, 8);
    assert_eq!(
        (count_eq(&divider, live), count_eq(&divider, dim)),
        (0, 0),
        "the divider between them takes neither band"
    );

    let plain = term.cell_frame(rows, cols);
    assert_ne!(frame.pixels, rend.render_input(&plain).pixels);
}

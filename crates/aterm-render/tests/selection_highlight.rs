// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Selection-highlight regression for the CPU renderer: cells inside the active
// text selection are filled with `Theme::selection` INSTEAD of their own
// background, the glyph keeps its foreground, everything outside the selection
// is untouched, and the viewport->selection row mapping honours the grid's
// display offset (scrollback). CPU fills are exact, so these checks use exact
// pixel equality — this locks the highlight rule even where no GPU exists.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{Terminal, TerminalBuilder};
use aterm_render::{Frame, Renderer, Theme};

fn r(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn g(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn b(p: u32) -> i32 {
    (p & 0xff) as i32
}

/// All pixels inside cell (row, col), given cell size.
fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

/// Pixels exactly equal to a packed colour (CPU fills are exact).
fn count_eq(px: &[u32], color: u32) -> usize {
    px.iter().filter(|&&p| p == color).count()
}

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

/// "hello" / "world" on a 4x10 grid with row 0 cols 1..=3 selected.
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
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = selected_term();
    let f = rend.render_input(&term.cell_frame(4, 10));
    let sel = Theme::default().selection;

    // Every selected cell (0,1)..(0,3) is dominated by the selection fill.
    for c in 1..=3usize {
        let px = cell_pixels(&f, cw, ch, 0, c);
        let n = count_eq(&px, sel);
        assert!(
            n > px.len() / 3,
            "selected cell (0,{c}) should be filled with theme.selection ({n}/{})",
            px.len()
        );
    }

    // The glyph is still drawn in the (light) foreground over the highlight.
    let px = cell_pixels(&f, cw, ch, 0, 2); // 'l'
    let fg_drawn = px.iter().any(|&p| r(p) > 150 && g(p) > 150 && b(p) > 150);
    assert!(
        fg_drawn,
        "selected cell (0,2) should keep its foreground glyph"
    );
}

#[test]
fn unselected_cells_keep_their_background() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = selected_term();
    let f = rend.render_input(&term.cell_frame(4, 10));
    let theme = Theme::default();

    // Cells outside the selection carry zero selection-coloured pixels:
    // (0,0) 'h' before the range, (0,4) 'o' after it, (1,2) on another row,
    // and a blank cell (0,8).
    for (row, col) in [(0usize, 0usize), (0, 4), (1, 2), (0, 8)] {
        let px = cell_pixels(&f, cw, ch, row, col);
        assert_eq!(
            count_eq(&px, theme.selection),
            0,
            "unselected cell ({row},{col}) must not show the selection colour"
        );
    }
    // The blank unselected cell is pure theme background.
    let blank = cell_pixels(&f, cw, ch, 0, 8);
    assert_eq!(
        count_eq(&blank, theme.bg),
        blank.len(),
        "blank cell (0,8) should stay theme bg"
    );

    // Clearing the selection restores the unhighlighted frame exactly.
    term.text_selection_mut().clear();
    let cleared = rend.render_input(&term.cell_frame(4, 10));
    let mut plain_term = Terminal::new(4, 10);
    plain_term.process(b"hello\r\nworld");
    let plain = rend.render_input(&plain_term.cell_frame(4, 10));
    assert_eq!(
        cleared.pixels, plain.pixels,
        "no selection must render exactly as before"
    );
}

#[test]
fn wide_char_block_selection_highlights_both_halves() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(2, 6);
    term.process("日x".as_bytes()); // 日 occupies cols 0-1, 'x' col 2
    let sel = term.text_selection_mut();
    // Block-select ONLY column 0 — the wide lead. contains_cell must snap the
    // highlight to whole-character boundaries: both halves light up.
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
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    // 3 visible rows + scrollback; 4 lines pushes "aa" off the live screen.
    let mut term = TerminalBuilder::new()
        .size(3, 8)
        .ring_buffer_size(64)
        .build();
    term.process(b"aa\r\nbb\r\ncc\r\ndd");
    // Select live row 0 ("bb"), cols 0..=1.
    let sel = term.text_selection_mut();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 1, SelectionSide::Right);
    sel.complete_selection();
    let selc = Theme::default().selection;

    // Live view: the highlight sits on viewport row 0.
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

    // Scroll back one line: viewport row 0 now shows scrollback ("aa"), and the
    // selected live row 0 is displayed at viewport row 1.
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

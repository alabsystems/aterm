// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scrollback preservation tests for cell extras across live and scrolled-back rows.

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;

/// Push `top` onto row 0, then 10 filler lines so it lands in RAM scrollback.
fn scrolled_off(top: &[u8]) -> Terminal {
    let mut t = Terminal::new(6, 80);
    t.process(top);
    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t
}

/// Locate the rendered row whose first cell is `start`.
fn render_row(frame: &RenderInput, start: char) -> Option<usize> {
    frame
        .cells
        .iter()
        .position(|row| row.first().is_some_and(|c| c.ch == start))
}

#[test]
fn partial_scroll_live_row_reads_correct_extras() {
    let mut t = Terminal::new(4, 80);
    for line in ["H0", "H1", "H2", "H3"] {
        t.process(format!("{line}\r\n").as_bytes());
    }
    t.process(b"L\xf0\x9f\x98\x80\r\n"); // "L😀"
    for line in ["M1", "M2", "M3"] {
        t.process(format!("{line}\r\n").as_bytes());
    }
    t.scroll_display(2); // 0 < display_offset(=2) < visible_rows(=4)
    let frame = t.cell_frame(4, 80);
    let r = render_row(&frame, 'L').expect("live L row visible in partial scroll");
    assert_eq!(
        frame.cells[r][1].ch, '\u{1F600}',
        "live row's emoji must survive partial scroll (screen_row mapping), not read a neighbor's extras"
    );
}

#[test]
fn row_text_screen_ignores_scroll_position() {
    let mut t = Terminal::new(4, 80);
    for i in 0..12 {
        t.process(format!("L{i:02}\r\n").as_bytes());
    }
    let live0 = t
        .grid()
        .row_text_screen(0)
        .map(|s| s.trim_end().to_string());
    t.scroll_to_top();
    let live0_scrolled = t
        .grid()
        .row_text_screen(0)
        .map(|s| s.trim_end().to_string());
    assert_eq!(
        live0, live0_scrolled,
        "row_text_screen must be display-offset-independent"
    );
    assert_eq!(t.row_text(0).as_deref().map(str::trim_end), Some("L00"));
    assert_ne!(live0_scrolled.as_deref(), Some("L00"));
}

/// A row rendered live (display_offset 0) renders identically once scrolled into history.
fn assert_scrollback_parity(input: &[u8], find: char) {
    assert_scrollback_parity_dims(6, 80, input, find);
}

/// As [`assert_scrollback_parity`] but for a specific grid size.
fn assert_scrollback_parity_dims(rows: u16, cols: u16, input: &[u8], find: char) {
    let mut t = Terminal::new(rows, cols);
    t.process(input);
    let live = t.cell_frame(usize::from(rows), usize::from(cols));
    let live_cells = live.cells[0].clone();
    let live_clusters = live.clusters[0].clone();
    let live_combining = live.combining[0].clone();
    let live_text = t.row_text(0);

    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    let frame = t.cell_frame(usize::from(rows), usize::from(cols));
    let r = render_row(&frame, find).expect("row visible after scroll_to_top");
    assert_eq!(
        t.row_text(r),
        live_text,
        "scrolled-back row_text must equal live"
    );

    assert_eq!(
        frame.cells[r], live_cells,
        "scrolled-back RenderCells must equal live"
    );
    assert_eq!(
        frame.clusters[r], live_clusters,
        "scrolled-back clusters must equal live"
    );
    assert_eq!(
        frame.combining[r], live_combining,
        "scrolled-back combining must equal live"
    );
}

#[test]
fn scrollback_parity_rows() {
    let rows: &[(u16, u16, &str, char)] = &[
        (6, 80, "Q\u{1F600}Z", 'Q'),
        (6, 80, "Qe\u{301}Z", 'Q'),
        (6, 80, "X\x1b[38;2;10;20;30mY\x1b[0m", 'X'),
        (6, 80, "Q\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}Z", 'Q'),
        (6, 80, "Q\u{1F1FA}\u{1F1F8}Z", 'Q'),
        (6, 80, "Q\u{1F44D}\u{1F3FD}Z", 'Q'),
        (6, 80, "\u{4E2D}\u{1F3FD}Z", '\u{4E2D}'),
        (6, 80, "Q\u{1F44B}\u{FE0F}\u{1F3FD}Z", 'Q'),
        (6, 80, "Q\u{2764}\u{FE0F}Z", 'Q'),
        (6, 80, "Q1\u{FE0F}\u{20E3}Z", 'Q'),
        (6, 80, "\u{261D}\u{1F3FD}Z", '\u{261D}'),
        (6, 80, "Q\u{261D}\u{FE0F}\u{1F3FD}Z", 'Q'),
        (6, 80, "Q\u{1F600}\u{FE0E}Z", 'Q'),
        (6, 80, "Q\u{2460}\u{FE0F}Z", 'Q'),
        (4, 3, "ab\u{2764}\u{FE0F}", 'a'),
        (6, 80, "Q\x1b[48;2;10;20;30m    \x1b[0m", 'Q'),
        (6, 80, "Q\u{1F600}e\u{0301}Z", 'Q'),
    ];
    for &(rows, cols, input, find) in rows {
        assert_scrollback_parity_dims(rows, cols, input.as_bytes(), find);
    }
}

/// SGR 58 underline-colour parity: assert the live cell at `col` carries `expect_color`, then
/// that the row renders identically once scrolled into history.
fn assert_scrollback_parity_underline(input: &[u8], find: char, col: usize, expect_color: [u8; 3]) {
    let mut t = Terminal::new(6, 80);
    t.process(input);
    let live = t.cell_frame(6, 80);
    let live_cells = live.cells[0].clone();
    let live_clusters = live.clusters[0].clone();
    let live_combining = live.combining[0].clone();
    assert_eq!(
        live_cells[col].underline_color,
        Some(expect_color),
        "live SGR 58 underline colour must be set (non-vacuous guard)"
    );

    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, find).expect("row visible after scroll_to_top");
    assert_eq!(
        frame.cells[r], live_cells,
        "scrolled-back RenderCells must equal live"
    );
    assert_eq!(
        frame.clusters[r], live_clusters,
        "scrolled-back clusters must equal live"
    );
    assert_eq!(
        frame.combining[r], live_combining,
        "scrolled-back combining must equal live"
    );
}

#[test]
fn scrollback_parity_underline_colour_rows() {
    let red = Terminal::new(6, 80).color_palette().get(1);
    let rows: &[(&str, usize, [u8; 3])] = &[
        ("Q\x1b[4;58:2::255:0:0mU\x1b[0mZ", 1, [255, 0, 0]),
        ("Q\x1b[4;58:5:1mU\x1b[0mZ", 1, [red.r, red.g, red.b]),
        ("Q\u{1b}[4;58:2::0:255:0m\u{4E2D}\u{1b}[0mZ", 1, [0, 255, 0]),
        (
            "Q\u{1b}[4;58:2::0:0:255m\u{2764}\u{FE0F}\u{1b}[0mZ",
            1,
            [0, 0, 255],
        ),
        ("Q\x1b[4;58:2::200:100:50mABC\x1b[0mZ", 1, [200, 100, 50]),
    ];
    for &(input, col, colour) in rows {
        assert_scrollback_parity_underline(input.as_bytes(), 'Q', col, colour);
    }
}

#[test]
fn scrollback_parity_inverse_truecolor_wide_char() {
    let input = "Q\u{1b}[38;2;255;0;0;48;2;0;0;255;7m\u{4E2D}\u{1b}[0mZ".as_bytes();

    let mut t = Terminal::new(6, 80);
    t.process(input);
    let live = t.cell_frame(6, 80);
    let spacer = &live.cells[0][2];
    assert!(spacer.wide, "cell 2 must be the wide continuation");
    assert_eq!(
        (spacer.fg, spacer.bg),
        ([0, 0, 255], [255, 0, 0]),
        "live spacer must carry the lead's SGR 7-swapped truecolor pair"
    );

    assert_scrollback_parity(input, 'Q');
}

#[test]
fn scrollback_spacer_keeps_lead_underline_colour() {
    let mut t = scrolled_off("Q\u{1b}[4;58:2::0:255:0m\u{4E2D}\u{1b}[0mZ".as_bytes());
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible after scroll_to_top");
    assert!(
        frame.cells[r][2].wide,
        "cell 2 must be the wide continuation"
    );
    assert_eq!(
        frame.cells[r][1].underline_color,
        Some([0, 255, 0]),
        "the lead keeps its SGR 58 colour through scrollback"
    );
    assert_eq!(
        frame.cells[r][2].underline_color,
        Some([0, 255, 0]),
        "the re-derived spacer must carry the lead's underline COLOUR, not just its UNDERLINE bit"
    );
}

#[test]
fn scrollback_spacer_underline_index_reresolves_after_palette_change() {
    let mut t = Terminal::new(6, 80);
    t.process("Q\u{1b}[4;58:5:1m\u{4E2D}\u{1b}[0mZ".as_bytes());
    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t.set_palette_color_components(1, 0x11, 0x22, 0x33);
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible after scroll_to_top");
    assert_eq!(
        frame.cells[r][1].underline_color,
        Some([0x11, 0x22, 0x33]),
        "the lead re-resolves against the changed palette"
    );
    assert_eq!(
        frame.cells[r][2].underline_color,
        Some([0x11, 0x22, 0x33]),
        "the spacer must carry the INDEX, so it re-resolves with the lead"
    );
}

#[test]
fn vs16_widened_spacer_carries_the_lead_underline_colour_live() {
    let mut t = Terminal::new(6, 80);
    t.process("Q\u{1b}[4;58:2::0:0:255m\u{2764}\u{FE0F}\u{1b}[0mZ".as_bytes());
    let live = t.cell_frame(6, 80);
    assert!(
        live.cells[0][2].wide,
        "cell 2 must be the VS16 widening's continuation"
    );
    assert_eq!(
        live.cells[0][2].underline_color,
        Some([0, 0, 255]),
        "a VS16-widened pair must answer the same colour for its right half as a naturally-wide one"
    );
}

#[test]
fn row_ansi_text_screen_ignores_scroll_position() {
    let mut t = Terminal::new(4, 80);
    for i in 0..12 {
        t.process(format!("H{i:02}\r\n").as_bytes());
    }
    t.process(b"r \xf0\x9f\x98\x80 \x1b[38;2;10;20;30mC\x1b[0m");
    let live: Vec<Option<String>> = (0..4).map(|r| t.grid().row_ansi_text_screen(r)).collect();
    t.scroll_to_top();
    let scrolled: Vec<Option<String>> = (0..4).map(|r| t.grid().row_ansi_text_screen(r)).collect();
    assert_eq!(
        scrolled, live,
        "row_ansi_text_screen must be display-offset-independent"
    );
    let joined: String = scrolled.iter().flatten().cloned().collect();
    assert!(
        joined.contains('\u{1F600}'),
        "scrolled serialize keeps the emoji, not U+FFFD"
    );
    assert!(
        joined.contains("38;2;10;20;30"),
        "scrolled serialize keeps the truecolor SGR"
    );
}

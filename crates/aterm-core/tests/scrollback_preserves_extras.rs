// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reading a scrolled-back row (display_offset > 0) recovers the row's extras
//! (emoji / CJK-SMP, combining marks, truecolor RGB), not just its cells, at
//! both the render (`cell_frame`) and text (`row_text`) level: scrolled-off
//! extras live in ring/lazy/tiered scrollback, not the live extras map. During
//! a partial scroll the still-live bottom rows read extras at
//! `visible_row - display_offset`, like their cells.

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
    // During a partial scroll the still-live bottom rows read extras at
    // screen_row = visible_row - display_offset, not the raw visible_row.
    let mut t = Terminal::new(4, 80);
    for line in ["H0", "H1", "H2", "H3"] {
        t.process(format!("{line}\r\n").as_bytes());
    }
    // L0 carries an emoji; it will be a live bottom row after a 2-line scroll.
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
    // The offset-independent accessor (used by block text extraction) reads the
    // live on-screen row regardless of display_offset; the offset-aware
    // row_text follows the scroll.
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
    // The offset-AWARE row_text(0) shows the oldest history line after scroll_to_top,
    // and must differ from the live screen row.
    assert_eq!(t.row_text(0).as_deref().map(str::trim_end), Some("L00"));
    assert_ne!(live0_scrolled.as_deref(), Some("L00"));
}

/// A row rendered live (display_offset 0) renders identically once scrolled into history:
/// the full `RenderCell` plus the cluster and combining overlays. `input` is one line (no
/// newline); `find`, its first char, relocates the row after `scroll_to_top`.
fn assert_scrollback_parity(input: &[u8], find: char) {
    assert_scrollback_parity_dims(6, 80, input, find);
}

/// As [`assert_scrollback_parity`] but for a specific grid size — needed to land a
/// glyph at the last column, where the live writer's wide-widen bails.
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
    // (rows, cols, input, first char of the row).
    let rows: &[(u16, u16, &str, char)] = &[
        // An SMP emoji, a combining mark, a truecolor foreground and a ZWJ
        // family: the extras live in scrollback, not the live extras map, so
        // a read that consulted only the latter shows U+FFFD, a bare base or
        // the default colour.
        (6, 80, "Q\u{1F600}Z", 'Q'),
        (6, 80, "Qe\u{301}Z", 'Q'),
        (6, 80, "X\x1b[38;2;10;20;30mY\x1b[0m", 'X'),
        (6, 80, "Q\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}Z", 'Q'),
        // A flag pair and a skin-toned emoji stay one cell; a skin tone after
        // a wide non-modifier-base (中) stays a cell of its own (no over-fold);
        // a VS16 between base and skin tone still folds (no under-fold).
        (6, 80, "Q\u{1F1FA}\u{1F1F8}Z", 'Q'),
        (6, 80, "Q\u{1F44D}\u{1F3FD}Z", 'Q'),
        (6, 80, "\u{4E2D}\u{1F3FD}Z", '\u{4E2D}'),
        (6, 80, "Q\u{1F44B}\u{FE0F}\u{1F3FD}Z", 'Q'),
        // ❤️ = U+2764 U+FE0F: VS16 widens a text-presentation base to 2 cells and
        // sets emoji_presentation; must not collapse to 1 column on scrollback.
        (6, 80, "Q\u{2764}\u{FE0F}Z", 'Q'),
        // 1️⃣ = '1' U+FE0F U+20E3: a keycap is width 2 live; must not collapse.
        (6, 80, "Q1\u{FE0F}\u{20E3}Z", 'Q'),
        // ☝🏽 with no VS16: U+261D is a narrow modifier-base, so the live writer
        // keeps the skin tone as its own wide cell. The segmenter must not over-fold.
        (6, 80, "\u{261D}\u{1F3FD}Z", '\u{261D}'),
        // ☝️🏽 = U+261D U+FE0F U+1F3FD: VS16 widens the narrow base, so the skin
        // tone folds.
        (6, 80, "Q\u{261D}\u{FE0F}\u{1F3FD}Z", 'Q'),
        // 😀︎ = U+1F600 U+FE0E: VS15 narrows the wide emoji to 1 cell live;
        // materialize must narrow it too, not re-widen from the base char.
        (6, 80, "Q\u{1F600}\u{FE0E}Z", 'Q'),
        // ①️ = U+2460 + VS16. U+2460 is not VS16-emoji-capable, so live does not
        // widen it (VS16 rides as a combining mark, 1 col); materialize must match.
        (6, 80, "Q\u{2460}\u{FE0F}Z", 'Q'),
        // A VS16-capable base at the last column cannot widen, so live keeps it
        // narrow; materialize must place it narrow, not drop it: in a 3-col grid
        // "ab❤️" puts ❤ at col 2.
        (4, 3, "ab\u{2764}\u{FE0F}", 'a'),
        // A truecolor-background bar whose last occupied cell is a coloured
        // blank (a status bar): the materialized row keeps the trailing coloured
        // blanks, as the live row does.
        (6, 80, "Q\x1b[48;2;10;20;30m    \x1b[0m", 'Q'),
        // Naturally wide + combining (😀 é).
        (6, 80, "Q\u{1F600}e\u{0301}Z", 'Q'),
    ];
    for &(rows, cols, input, find) in rows {
        assert_scrollback_parity_dims(rows, cols, input.as_bytes(), find);
    }
}

/// SGR 58 underline-colour parity: first assert the live cell at `col` carries
/// `expect_color` — so the live==scrolled comparison below can't pass vacuously
/// with both sides `None` (e.g. if the SGR sequence failed to parse) — then
/// assert the full-`RenderCell` parity after the row scrolls into history.
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
    // (input, column of the underlined cell, its live colour); every row starts with 'Q'.
    // SGR 58:5:1 resolves against the live palette, and scrolled back against the same entry.
    let red = Terminal::new(6, 80).color_palette().get(1);
    let rows: &[(&str, usize, [u8; 3])] = &[
        // SGR 58:2 explicit RGB (packed 0x01) + SGR 4; 'U' sits at col 1.
        ("Q\x1b[4;58:2::255:0:0mU\x1b[0mZ", 1, [255, 0, 0]),
        // SGR 58:5:1, indexed (packed 0x02).
        ("Q\x1b[4;58:5:1mU\x1b[0mZ", 1, [red.r, red.g, red.b]),
        // A WIDE char (中, cols 1-2): the physical-to-cell mapping and the wide
        // continuation must match live, as hyperlinks do across wide cells.
        ("Q\u{1b}[4;58:2::0:255:0m\u{4E2D}\u{1b}[0mZ", 1, [0, 255, 0]),
        // A VS16-widened emoji (❤️): the colour sidecar and the width replay
        // together.
        (
            "Q\u{1b}[4;58:2::0:0:255m\u{2764}\u{FE0F}\u{1b}[0mZ",
            1,
            [0, 0, 255],
        ),
        // A contiguous run over several narrow cells coalesces to one span and
        // restores each cell.
        ("Q\x1b[4;58:2::200:100:50mABC\x1b[0mZ", 1, [200, 100, 50]),
    ];
    for &(input, col, colour) in rows {
        assert_scrollback_parity_underline(input.as_bytes(), 'Q', col, colour);
    }
}

#[test]
fn scrollback_parity_inverse_truecolor_wide_char() {
    // Reverse video over a truecolor wide char, live vs history. The spacer
    // takes the lead's inverse bit, and `color_resolve::resolve_both` swaps per
    // cell, so the 24-bit pair (in the cell's `CellExtra`, too big for a
    // `PackedColor`) must cross the scroll-off boundary with it, or the right
    // half renders as a near-white block.
    let input = "Q\u{1b}[38;2;255;0;0;48;2;0;0;255;7m\u{4E2D}\u{1b}[0mZ".as_bytes();

    // Non-vacuous guard: pin the live spacer's swapped pair first, so the parity
    // assertion below cannot be satisfied by both sides being equally wrong.
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
    // The flag and the colour it selects cross the boundary together:
    // `render_cells` publishes `underline_color` only when the underline bit is
    // set, so a dropped colour shows as a two-tone underline under one wide
    // character.
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
    // The spacer's copy is the packed form, not a frozen RGB triple, so both halves
    // re-resolve after palette entry 1 changes. A mirror reading `underline_color()` (RGB
    // only) would leave the spacer `None`, since storing an index clears
    // `HAS_UNDERLINE_COLOR`; one that resolved the index eagerly would pin the old colour.
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
    // VS16 promotes a narrow base to two columns after `apply_cell_extras_preflagged` ran
    // with `cols = 1`; the right half still answers the SGR 58 colour, as a CJK char's does.
    // The history mirror copies the lead unconditionally, so an empty live spacer would make
    // history disagree with the screen.
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
    // serialize()'s viewport paint reads the live screen row via
    // row_ansi_text_screen (offset-independent), so a scrolled emoji/RGB row is
    // not re-emitted from history nor stranded as U+FFFD / stripped colour.
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

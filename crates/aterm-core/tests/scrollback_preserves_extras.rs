// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Regression: reading a SCROLLED-BACK row (display_offset > 0) must recover the
//! row's extras — emoji / CJK-SMP, combining marks, truecolor RGB — not just its
//! cells. Before the `visible_row_view` fix, `cell_frame` (render) and `row_text`
//! read the LIVE extras map at the raw visible row, but scrolled-off extras had
//! moved into ring/lazy/tiered scrollback; so every scrolled emoji became U+FFFD
//! and marks/colors were dropped. A PARTIAL scroll also misaligned the still-live
//! bottom rows (cell from `visible_row - display_offset`, extras from raw
//! `visible_row`). Both are pinned here at the render AND text level.

use aterm_core::prelude::Terminal;
use aterm_core::render::RenderInput;

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

/// Locate the visible row (via row_text) that begins with `start`.
fn text_row(t: &Terminal, max_rows: u16, start: char) -> Option<String> {
    (0..max_rows).find_map(|r| {
        let s = t.row_text(usize::from(r))?;
        s.trim_end()
            .starts_with(start)
            .then(|| s.trim_end().to_string())
    })
}

#[test]
fn scrollback_render_preserves_emoji() {
    let mut t = scrolled_off(b"Q\xf0\x9f\x98\x80Z"); // Q😀Z
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible after scroll_to_top");
    assert_eq!(
        frame.cells[r][1].ch, '\u{1F600}',
        "scrolled-back emoji must render as 😀, not U+FFFD"
    );
}

#[test]
fn scrollback_row_text_preserves_emoji() {
    let mut t = scrolled_off(b"Q\xf0\x9f\x98\x80Z");
    t.scroll_to_top();
    assert_eq!(text_row(&t, 6, 'Q').as_deref(), Some("Q\u{1F600}Z"));
}

#[test]
fn scrollback_render_preserves_combining_mark() {
    let mut t = scrolled_off(b"Qe\xcc\x81Z"); // Q é(e+U+0301) Z
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible");
    assert_eq!(frame.cells[r][1].ch, 'e', "base char survives");
    let marks: Vec<char> = frame.combining[r]
        .iter()
        .find(|(col, _)| *col == 1)
        .map(|(_, m)| m.to_vec())
        .unwrap_or_default();
    assert_eq!(
        marks,
        vec!['\u{0301}'],
        "combining mark must not be stripped"
    );
}

#[test]
fn scrollback_row_text_preserves_combining_mark() {
    let mut t = scrolled_off(b"Qe\xcc\x81Z");
    t.scroll_to_top();
    assert_eq!(text_row(&t, 6, 'Q').as_deref(), Some("Qe\u{301}Z"));
}

#[test]
fn scrollback_render_preserves_truecolor() {
    // Truecolor X on the scrolled-off row.
    let mut t = scrolled_off(b"X\x1b[38;2;10;20;30mY\x1b[0m");
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'X').expect("X row visible");
    assert_eq!(
        frame.cells[r][1].fg,
        [10, 20, 30],
        "scrolled-back truecolor fg must be preserved"
    );
}

#[test]
fn scrollback_render_preserves_zwj_cluster() {
    // Family ZWJ sequence 👨‍👩‍👧 — must surface the whole grapheme, not just the base.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let mut t = Terminal::new(6, 80);
    t.process(format!("Q{family}Z").as_bytes());
    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible");
    let cluster = frame.clusters[r]
        .iter()
        .find(|(col, _)| *col == 1)
        .map(|(_, s)| s.to_string());
    assert_eq!(
        cluster.as_deref(),
        Some(family),
        "scrolled-back ZWJ family must surface the full grapheme cluster"
    );
    // row_text emits the full family too.
    assert_eq!(
        text_row(&t, 6, 'Q').as_deref(),
        Some(format!("Q{family}Z").as_str())
    );
}

#[test]
fn partial_scroll_live_row_reads_correct_extras() {
    // The SECOND bug: during a partial scroll the still-live bottom rows must read
    // extras at screen_row = visible_row - display_offset, not the raw visible_row.
    let mut t = Terminal::new(4, 80);
    for line in ["H0", "H1", "H2", "H3"] {
        t.process(format!("{line}\r\n").as_bytes());
    }
    // L0 carries an emoji; it will be a LIVE bottom row after a 2-line scroll.
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
fn scroll_to_top_shows_oldest_line_first() {
    // Boundary: at display_offset == history_line_count, visible row 0 == oldest line.
    let mut t = Terminal::new(4, 80);
    for i in 0..12 {
        t.process(format!("L{i:02}\r\n").as_bytes());
    }
    t.scroll_to_top();
    let top = t.row_text(0).map(|s| s.trim_end().to_string());
    assert_eq!(
        top.as_deref(),
        Some("L00"),
        "top of scroll_to_top is the oldest line"
    );
}

#[test]
fn live_view_unchanged_at_offset_zero() {
    // display_offset == 0 must be byte-identical to the pre-fix live read: an emoji
    // on a LIVE row still renders and row_text still resolves it.
    let mut t = Terminal::new(6, 80);
    t.process(b"Q\xf0\x9f\x98\x80Z");
    assert_eq!(t.grid().display_offset(), 0);
    let frame = t.cell_frame(6, 80);
    assert_eq!(frame.cells[0][1].ch, '\u{1F600}');
    assert_eq!(
        t.row_text(0).as_deref().map(str::trim_end),
        Some("Q\u{1F600}Z")
    );
}

#[test]
fn scrollback_render_preserves_flag_pair() {
    // 🇺🇸 = U+1F1FA U+1F1F8. The materialize grapheme segmenter must fold the
    // regional-indicator PAIR into one cell so it stays one cluster (not two
    // split letters across doubled columns) when scrolled back.
    let flag = "\u{1F1FA}\u{1F1F8}";
    let mut t = Terminal::new(6, 80);
    t.process(format!("Q{flag}Z").as_bytes());
    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible");
    let cluster = frame.clusters[r]
        .iter()
        .find(|(col, _)| *col == 1)
        .map(|(_, s)| s.to_string());
    assert_eq!(
        cluster.as_deref(),
        Some(flag),
        "scrolled-back flag must be one cluster"
    );
    // Z must sit at col 3 (flag occupies one wide cell: col 1 lead + col 2 cont),
    // NOT be pushed out by a split into 4 columns.
    assert_eq!(
        frame.cells[r][3].ch, 'Z',
        "flag must occupy one cell, not split columns"
    );
}

#[test]
fn scrollback_render_preserves_skin_tone() {
    // 👍🏽 = U+1F44D U+1F3FD. The skin-tone modifier must fold onto the emoji base.
    let thumb = "\u{1F44D}\u{1F3FD}";
    let mut t = Terminal::new(6, 80);
    t.process(format!("Q{thumb}Z").as_bytes());
    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible");
    let cluster = frame.clusters[r]
        .iter()
        .find(|(col, _)| *col == 1)
        .map(|(_, s)| s.to_string());
    assert_eq!(
        cluster.as_deref(),
        Some(thumb),
        "scrolled-back skin-toned emoji must be one cluster"
    );
    assert_eq!(
        frame.cells[r][3].ch, 'Z',
        "skin-toned emoji must occupy one cell"
    );
}

#[test]
fn scrollback_wide_nonbase_keeps_skin_tone_split() {
    // Over-fold guard: a skin-tone modifier after a WIDE but NON-modifier-base char
    // (中, U+4E2D) must NOT fold. The live writer gates folding on
    // is_emoji_modifier_base(base), not mere cell width, so it renders two cells;
    // the materializer must match, or scrolling back would regress a correct split.
    let mut t = Terminal::new(6, 80);
    t.process("中\u{1F3FD}Z".as_bytes()); // 中 + skin-tone (Type-4) + Z
    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, '中').expect("中 row visible");
    // 中 wide at col0-1, the orphan skin-tone its OWN wide cell at col2-3, Z at col4.
    assert_eq!(frame.cells[r][0].ch, '中');
    assert_eq!(
        frame.cells[r][2].ch, '\u{1F3FD}',
        "skin-tone after a non-modifier-base must stay a separate cell (no over-fold)"
    );
    assert_eq!(
        frame.cells[r][4].ch, 'Z',
        "Z must not be pulled left by an over-fold"
    );
}

#[test]
fn scrollback_vs16_base_folds_skin_tone() {
    // Under-fold guard: 👋️🏽 = U+1F44B U+FE0F U+1F3FD. The VS16 sits between the
    // emoji base and the skin-tone modifier. Folding must key on the retained unit
    // BASE (is_emoji_modifier_base(👋)), not the intervening zero-width VS16 — else
    // the modifier splits off. Live renders one cell; scrolled back must too.
    let seq = "\u{1F44B}\u{FE0F}\u{1F3FD}";
    let mut t = Terminal::new(6, 80);
    t.process(format!("Q{seq}Z").as_bytes());
    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible");
    let cluster = frame.clusters[r]
        .iter()
        .find(|(col, _)| *col == 1)
        .map(|(_, s)| s.to_string());
    assert_eq!(
        cluster.as_deref(),
        Some(seq),
        "VS16-separated skin-tone must fold onto the base into one cluster"
    );
    // One wide cell (col1 lead + col2 cont) → Z at col3, not pushed to col5 by a split.
    assert_eq!(
        frame.cells[r][3].ch, 'Z',
        "folded grapheme occupies one cell"
    );
}

#[test]
fn row_text_screen_ignores_scroll_position() {
    // Finding 2 guard: the offset-INDEPENDENT accessor (used by block text
    // extraction) must read the live on-screen row regardless of display_offset,
    // where the offset-AWARE row_text follows the scroll.
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
    // and must DIFFER from the live screen row.
    assert_eq!(t.row_text(0).as_deref().map(str::trim_end), Some("L00"));
    assert_ne!(live0_scrolled.as_deref(), Some("L00"));
}

#[test]
fn render_matches_row_text_across_offsets() {
    // The render glyph stream and row_text must agree for the scrolled emoji row
    // (no channel dropped on one path but not the other).
    let mut t = scrolled_off(b"Q\xf0\x9f\x98\x80Z");
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible");
    // Skip wide-continuation cells (the blank right half of 😀; RenderCell.wide
    // marks the continuation itself), which row_text omits, then compare streams.
    let mut from_render = String::new();
    for cell in &frame.cells[r] {
        if cell.wide {
            continue;
        }
        from_render.push(cell.ch);
    }
    let from_render = from_render.trim_end().to_string();
    let from_text = t.row_text(r).map(|s| s.trim_end().to_string());
    assert_eq!(Some(from_render), from_text);
}

/// The strongest scrollback invariant: a row rendered LIVE (display_offset==0)
/// must render byte-for-byte IDENTICALLY once it has scrolled into history — the
/// full `RenderCell` (glyph, fg/bg, wide, emoji_presentation, bold/italic,
/// underline + colour, strike/overline) plus the cluster and combining overlays.
/// `input` is one line (no newline); `find` is its first char, used to relocate
/// the row after `scroll_to_top`.
fn assert_scrollback_parity(input: &[u8], find: char) {
    assert_scrollback_parity_dims(6, 80, input, find);
}

/// As [`assert_scrollback_parity`] but for a specific grid size — needed to land a
/// glyph at the LAST column, where the live writer's wide-widen bails.
fn assert_scrollback_parity_dims(rows: u16, cols: u16, input: &[u8], find: char) {
    let mut t = Terminal::new(rows, cols);
    t.process(input);
    let live = t.cell_frame(usize::from(rows), usize::from(cols));
    let live_cells = live.cells[0].clone();
    let live_clusters = live.clusters[0].clone();
    let live_combining = live.combining[0].clone();

    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    let frame = t.cell_frame(usize::from(rows), usize::from(cols));
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
fn scrollback_parity_vs16_widened_heart() {
    // ❤️ = U+2764 U+FE0F: VS16 widens a text-presentation base to 2 cells + sets
    // emoji_presentation; must not collapse to 1 column on scrollback.
    assert_scrollback_parity("Q\u{2764}\u{FE0F}Z".as_bytes(), 'Q');
}

#[test]
fn scrollback_parity_keycap() {
    // 1️⃣ = '1' U+FE0F U+20E3: a keycap sequence is width 2 live; must not collapse.
    assert_scrollback_parity("Q1\u{FE0F}\u{20E3}Z".as_bytes(), 'Q');
}

#[test]
fn scrollback_parity_narrow_base_skin_splits() {
    // ☝🏽 with NO VS16: U+261D is a NARROW modifier-base, so the live writer keeps
    // the skin-tone as its own wide cell (2 cells). The segmenter must not over-fold.
    assert_scrollback_parity("\u{261D}\u{1F3FD}Z".as_bytes(), '\u{261D}');
}

#[test]
fn scrollback_parity_vs16_narrow_base_skin_folds() {
    // ☝️🏽 = U+261D U+FE0F U+1F3FD: VS16 widens the narrow base, so the skin-tone folds.
    assert_scrollback_parity("Q\u{261D}\u{FE0F}\u{1F3FD}Z".as_bytes(), 'Q');
}

#[test]
fn scrollback_parity_vs15_narrowed_emoji() {
    // 😀︎ = U+1F600 U+FE0E: VS15 (text presentation) NARROWS the wide emoji to 1
    // cell live; materialize must narrow it too, not re-widen from the base char.
    assert_scrollback_parity("Q\u{1F600}\u{FE0E}Z".as_bytes(), 'Q');
}

#[test]
fn scrollback_parity_vs16_on_incapable_base_stays_narrow() {
    // ①️ = U+2460 + VS16. U+2460 is NOT is_vs16_emoji_capable, so live does not
    // widen it (VS16 rides as a combining mark, stays 1 col); materialize must match.
    assert_scrollback_parity("Q\u{2460}\u{FE0F}Z".as_bytes(), 'Q');
}

#[test]
fn scrollback_parity_last_column_vs16_base_not_dropped() {
    // A VS16-capable base at the LAST column can't widen (no room for the
    // continuation), so the live writer keeps it NARROW. Materialize must place it
    // narrow too, not DROP the glyph: 3-col grid, "ab❤️" → ❤ lands at col2 (last).
    assert_scrollback_parity_dims(4, 3, "ab\u{2764}\u{FE0F}".as_bytes(), 'a');
}

#[test]
fn scrollback_parity_trailing_colored_blanks() {
    // A truecolor-background bar whose LAST occupied cell is a coloured blank (a
    // status bar). MaterializedRow::len clipped trailing coloured blanks that the
    // live Row::len (write high-water mark) keeps, so the bar vanished on scrollback.
    assert_scrollback_parity(b"Q\x1b[48;2;10;20;30m    \x1b[0m", 'Q');
}

#[test]
fn scrollback_parity_plain_emoji_and_combining() {
    // Regression guard: naturally-wide + combining stay at full parity under the
    // stricter whole-RenderCell check.
    assert_scrollback_parity("Q\u{1F600}e\u{0301}Z".as_bytes(), 'Q'); // 😀 é
}

/// SGR 58 underline-colour parity: first assert the LIVE cell at `col` carries
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
fn scrollback_parity_underline_color_rgb() {
    // SGR 58:2 explicit RGB underline colour (packed 0x01) + SGR 4 underline must
    // round-trip through scrollback — the storage-format extension this fix adds.
    // 'U' sits at col 1 (after 'Q').
    assert_scrollback_parity_underline(b"Q\x1b[4;58:2::255:0:0mU\x1b[0mZ", 'Q', 1, [255, 0, 0]);
}

#[test]
fn scrollback_parity_underline_color_indexed() {
    // SGR 58:5:1 (indexed, packed 0x02) resolves against the live palette; once
    // scrolled back it must resolve to the SAME entry (the index is preserved).
    let red = Terminal::new(6, 80).color_palette().get(1);
    assert_scrollback_parity_underline(b"Q\x1b[4;58:5:1mU\x1b[0mZ", 'Q', 1, [red.r, red.g, red.b]);
}

#[test]
fn scrollback_parity_underline_color_wide_char() {
    // Underline colour on a WIDE char (中, U+4E2D, cols 1-2): the physical→cell
    // column mapping and the wide-continuation cell must match live exactly. This
    // probes that the sidecar restore aligns like hyperlinks do across wide cells.
    assert_scrollback_parity_underline(
        "Q\u{1b}[4;58:2::0:255:0m\u{4E2D}\u{1b}[0mZ".as_bytes(),
        'Q',
        1,
        [0, 255, 0],
    );
}

#[test]
fn scrollback_parity_underline_color_emoji() {
    // Underline colour on a VS16-widened emoji (❤️): full parity across the
    // colour sidecar AND the width replay together.
    assert_scrollback_parity_underline(
        "Q\u{1b}[4;58:2::0:0:255m\u{2764}\u{FE0F}\u{1b}[0mZ".as_bytes(),
        'Q',
        1,
        [0, 0, 255],
    );
}

#[test]
fn scrollback_parity_underline_color_multi_cell_run() {
    // A contiguous underline-coloured run over several narrow cells must coalesce
    // to one span and restore each cell — parity across the whole run.
    assert_scrollback_parity_underline(
        b"Q\x1b[4;58:2::200:100:50mABC\x1b[0mZ",
        'Q',
        1,
        [200, 100, 50],
    );
}

#[test]
fn scrollback_underline_indexed_reresolves_after_palette_change() {
    // The packed INDEX (not a frozen RGB triple) must survive scrollback: redefine
    // palette entry 1 AFTER the row scrolls into history, and the materialized
    // underline must re-resolve to the NEW colour. This fails if scrollback stored
    // a resolved RGB — the reason underline colour is carried in packed form.
    let mut t = Terminal::new(6, 80);
    t.process(b"Q\x1b[4;58:5:1mU\x1b[0mZ");
    t.process(b"\r\n");
    for i in 0..10 {
        t.process(format!("f{i}\r\n").as_bytes());
    }
    // Direct palette API (no OSC-4 reconfigure policy gate).
    t.set_palette_color_components(1, 0x11, 0x22, 0x33);
    t.scroll_to_top();
    let frame = t.cell_frame(6, 80);
    let r = render_row(&frame, 'Q').expect("Q row visible after scroll_to_top");
    assert_eq!(
        frame.cells[r][1].underline_color,
        Some([0x11, 0x22, 0x33]),
        "scrolled-back indexed underline must re-resolve against the changed palette"
    );
}

#[test]
fn row_ansi_text_screen_ignores_scroll_position() {
    // serialize()'s viewport paint reads the LIVE screen row via row_ansi_text_screen
    // (offset-INDEPENDENT), so a scrolled emoji/RGB row is NOT re-emitted from
    // scrolled history nor stranded as U+FFFD / stripped colour.
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

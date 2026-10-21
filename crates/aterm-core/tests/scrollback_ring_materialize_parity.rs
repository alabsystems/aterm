// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parity: the ring-tier fast materializer against the `Line` round trip.
//!
//! `Grid::materialize_scrollback_row_full` reads a ring-tier history row
//! straight from its stored `Row` + `ScrolledRowExtras` rather than serializing
//! to a `Line` and parsing back. Both must produce the same row, including the
//! round trip's normalizations that consumers rely on: combining marks fold into
//! the cell's complex string, a complex cell holding one BMP scalar demotes to
//! an inline char, and a wide unit is re-created with its spacer.
//!
//! It lives in aterm-core so the corpus goes through the real parser (OSC 8,
//! SGR 58, truecolor, ZWJ/VS16/skin-tone/flag emoji, CJK).
//!
//! `take_ring_fast_materialize` is asserted to fire on every ring row, so the
//! parity is never the round trip compared with itself.

use aterm_core::grid::materialize_from_line;
use aterm_core::terminal::Terminal;
use aterm_grid::test_counters::take_ring_fast_materialize;

const ROWS: u16 = 6;
const COLS: u16 = 40;

/// One physical row per line, each on a different edge of the serializer/parser pair.
fn corpus() -> Vec<String> {
    vec![
        // Plain ASCII — the common cell, and the closed-form fast path.
        "plain ascii row 0123456789".to_owned(),
        // Trailing coloured blanks (a status bar / `\x1b[K` fill): the cells are
        // spaces but not default, so `len` must keep them.
        "\x1b[44mstatus\x1b[K\x1b[0m".to_owned(),
        // Wide CJK: wide main cells with spacers the serializer omits and the
        // parser re-creates.
        "cjk 日本語テキスト end".to_owned(),
        // Non-BMP emoji (complex cells), plus a ZWJ family and a skin tone.
        "emoji 🎉 👨‍👩‍👧 👍🏽".to_owned(),
        // Printable ASCII after a cell ending in a ZWJ opens a cell of its own (the ASCII
        // bulk path never folds), and the round trip must not join it.
        "zwj then ascii \u{2764}\u{FE0F}\u{200D}Qa".to_owned(),
        // Presentation selectors: VS16 widens, VS15 narrows.
        "vs ❤\u{FE0F} ⌚\u{FE0E} done".to_owned(),
        // A regional-indicator flag pair (folds into one cell).
        "flag 🇯🇵 tail".to_owned(),
        // Combining marks — these fold into the complex string on the way back.
        "combining e\u{0301} a\u{0308} o\u{0302}".to_owned(),
        // Truecolor foreground and background (RGB overflow on both channels).
        "\x1b[38;2;255;100;50m\x1b[48;2;10;20;30mtruecolor\x1b[0m".to_owned(),
        // 256-colour indexed + the SGR attribute family.
        "\x1b[38;5;123m\x1b[1;3;4;9;53mattrs\x1b[0m".to_owned(),
        // OSC 8 hyperlink with an id, then unlinked text after it.
        "\x1b]8;id=x1;https://example.com/a\x1b\\linked\x1b]8;;\x1b\\ after".to_owned(),
        // SGR 58 underline colour: explicit RGB, then indexed (which must stay
        // indexed so it re-resolves against the live palette).
        "\x1b[4;58;2;10;200;30munder\x1b[59m \x1b[4;58;5;9midx\x1b[0m".to_owned(),
        // Mixed: a hyperlink over CJK with truecolor under it.
        "\x1b[38;2;9;9;9m\x1b]8;;https://e.x/b\x1b\\日本\x1b]8;;\x1b\\\x1b[0m".to_owned(),
        // A row that fills the grid width exactly.
        "X".repeat(COLS as usize),
        // A row whose last cell is wide (the row-edge case).
        format!("{}日", "y".repeat(COLS as usize - 2)),
        // Cells the grid drew apart that a walk of their text joins: the grid
        // folds a character into the cell before only when that cell is the one
        // before the cursor (`OUT_OF_ORDER`).
        OUT_OF_ORDER[0].1.to_owned(),
        OUT_OF_ORDER[1].1.to_owned(),
        OUT_OF_ORDER[2].1.to_owned(),
        OUT_OF_ORDER[3].1.to_owned(),
        OUT_OF_ORDER[4].1.to_owned(),
        // Empty row (zero length).
        String::new(),
    ]
}

/// Rows whose cells the grid drew apart where a walk of the row's text joins
/// them, each ending in `XYZ`: written out of order, or brought together by a
/// DCH. Each starts with `CR` and draws on the cursor's row only.
const OUT_OF_ORDER: [(&str, &str); 5] = [
    (
        "flag halves, right one first",
        "\r\x1b[3G\u{1f1e7}\x1b[1G\u{1f1e6}\x1b[5GXYZ",
    ),
    (
        "skin tone before its base",
        "\r\x1b[3G\u{1f3fd}\x1b[1G\u{1f44d}\x1b[5GXYZ",
    ),
    (
        "a cell ending in ZWJ, the next drawn first",
        "\r\x1b[2G\u{e9}\x1b[1Ga\u{200d}\x1b[3GXYZ",
    ),
    (
        "flag halves brought together by DCH",
        "\r\u{1f1e6}q\u{1f1e7}XYZ\x1b[3G\x1b[P",
    ),
    (
        "skin tone brought to its base by DCH",
        "\r\u{1f44d}q\u{1f3fd}XYZ\x1b[3G\x1b[P",
    ),
];

/// Feed the corpus, then enough blank lines to push every corpus row off the
/// screen and into history.
fn fill(term: &mut Terminal) {
    let mut bytes = Vec::new();
    for line in corpus() {
        bytes.extend_from_slice(line.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    for i in 0..u32::from(ROWS) + 2 {
        bytes.extend_from_slice(format!("tail{i}\r\n").as_bytes());
    }
    term.process(&bytes);
}

/// Compare one history row both ways, field by field. `MaterializedRow`'s
/// public surface is `cells` + `get_extra`, which together carry every channel
/// a renderer or a text read can observe.
fn assert_row_parity(term: &Terminal, rev_idx: usize) {
    let grid = term.grid();
    let cols = grid.cols();
    let reference = {
        let line = grid
            .history_line_rev(rev_idx)
            .unwrap_or_else(|| panic!("history row {rev_idx} exists"));
        materialize_from_line(&line, cols)
    };
    let actual = grid
        .materialize_scrollback_row_full(rev_idx, cols)
        .unwrap_or_else(|| panic!("history row {rev_idx} materializes"));

    assert_eq!(
        actual.cells, reference.cells,
        "rev_idx {rev_idx}: cells differ between the direct read and the Line round trip"
    );
    for col in 0..cols {
        assert_eq!(
            actual.get_extra(col),
            reference.get_extra(col),
            "rev_idx {rev_idx}, col {col}: extras differ between the direct read and \
             the Line round trip"
        );
    }
}

/// Every ring-tier history row materializes identically both ways, through the fast path.
#[test]
fn ring_rows_materialize_identically_to_the_line_round_trip() {
    let mut term = Terminal::new(ROWS, COLS);
    fill(&mut term);
    let depth = term.grid().scrollback_lines();
    assert!(
        depth >= corpus().len(),
        "fixture pushed only {depth} lines into history"
    );

    // Per row, so a decline names the row that caused it.
    let _ = take_ring_fast_materialize();
    for rev_idx in 0..depth {
        assert_row_parity(&term, rev_idx);
        let text = term
            .grid()
            .history_line_rev(rev_idx)
            .and_then(|l| l.as_str().map(str::to_owned))
            .unwrap_or_default();
        assert_eq!(
            take_ring_fast_materialize(),
            1,
            "rev_idx {rev_idx}: the ring fast path declined a ring row, so the parity \
             assertion for it compared the round trip with itself. Row text: {text:?}"
        );
    }
}

/// Every display column of the top row as drawn: `(grapheme, wide)`.
fn top_row(term: &Terminal) -> Vec<(String, bool)> {
    term.display_row_grapheme_cells(0).expect("the top row")
}

/// Cells the grid drew apart stay apart in history: the ring tier draws the
/// row from its own cells, and the stored line it becomes past the ring, and
/// the line a history rewrap builds from it, keep the cells apart too, so the
/// row is drawn the same in every tier and its `XYZ` stays where it was drawn
/// for every reader of a history row's cells (search, a double-click, a copy).
/// The stored line used to join them (`advance_grapheme_unit_wide`), and the
/// row's layout changed when it left the ring.
#[test]
fn cells_drawn_apart_stay_apart_in_every_history_tier() {
    use aterm_core::scrollback::Scrollback;
    for (label, input) in OUT_OF_ORDER {
        let mut term = Terminal::with_scrollback(3, COLS, 2, Scrollback::with_defaults());
        term.process(input.as_bytes());
        let drawn = top_row(&term);
        let x = drawn
            .iter()
            .position(|(g, _)| g == "X")
            .expect("X is drawn");
        let col_of_x = |term: &Terminal, row: i32| {
            let (text, cells) = term.written_line_cells(row).expect("the row");
            let x = text.find('X').expect("X in the text");
            cells.columns_of(&text, x, x + 1).0
        };
        assert_eq!(col_of_x(&term, 0), x, "{label}: on screen");

        // Into the ring, then past it into the store.
        for (tier, newer) in [("ring", 1usize), ("store", 4)] {
            term.scroll_to_bottom();
            while term.grid().scrollback_lines() < newer + 1 {
                term.process(b"\r\n");
            }
            let in_ring = term.grid().ring_buffer_scrollback() > newer;
            assert_eq!(in_ring, tier == "ring", "{label}: precondition, {tier}");
            term.scroll_to_top();
            assert_eq!(top_row(&term), drawn, "{label}: drawn in the {tier}");
            let row = -i32::try_from(newer).expect("small") - 1;
            assert_eq!(col_of_x(&term, row), x, "{label}: read in the {tier}");
            let step = term
                .search_budgeted("XYZ", false, false, None, 100)
                .expect("budgeted search");
            assert!(
                step.results.matches.iter().any(|m| m.start_col == x),
                "{label}: found in the {tier}: {:?}",
                step.results.matches
            );
        }

        // A history rewrap there and back builds the line from those cells.
        term.resize(3, COLS + 7);
        term.resize(3, COLS);
        term.scroll_to_top();
        assert_eq!(top_row(&term), drawn, "{label}: after a history rewrap");
    }
}

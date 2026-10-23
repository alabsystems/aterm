// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parity tests comparing `Grid::materialize_scrollback_row_full` on ring-tier rows against the `Line` round trip.

use aterm_core::grid::materialize_from_line;
use aterm_core::terminal::Terminal;
use aterm_grid::test_counters::take_ring_fast_materialize;

const ROWS: u16 = 6;
const COLS: u16 = 40;

/// One physical row per line, each on a different edge of the serializer/parser pair.
fn corpus() -> Vec<String> {
    vec![
        "plain ascii row 0123456789".to_owned(),
        "\x1b[44mstatus\x1b[K\x1b[0m".to_owned(),
        "cjk 日本語テキスト end".to_owned(),
        "emoji 🎉 👨‍👩‍👧 👍🏽".to_owned(),
        "zwj then ascii \u{2764}\u{FE0F}\u{200D}Qa".to_owned(),
        "vs ❤\u{FE0F} ⌚\u{FE0E} done".to_owned(),
        "flag 🇯🇵 tail".to_owned(),
        "combining e\u{0301} a\u{0308} o\u{0302}".to_owned(),
        "\x1b[38;2;255;100;50m\x1b[48;2;10;20;30mtruecolor\x1b[0m".to_owned(),
        "\x1b[38;5;123m\x1b[1;3;4;9;53mattrs\x1b[0m".to_owned(),
        "\x1b]8;id=x1;https://example.com/a\x1b\\linked\x1b]8;;\x1b\\ after".to_owned(),
        "\x1b[4;58;2;10;200;30munder\x1b[59m \x1b[4;58;5;9midx\x1b[0m".to_owned(),
        "\x1b[38;2;9;9;9m\x1b]8;;https://e.x/b\x1b\\日本\x1b]8;;\x1b\\\x1b[0m".to_owned(),
        "X".repeat(COLS as usize),
        format!("{}日", "y".repeat(COLS as usize - 2)),
        OUT_OF_ORDER[0].1.to_owned(),
        OUT_OF_ORDER[1].1.to_owned(),
        OUT_OF_ORDER[2].1.to_owned(),
        OUT_OF_ORDER[3].1.to_owned(),
        OUT_OF_ORDER[4].1.to_owned(),
        String::new(),
    ]
}

/// Rows whose adjacent grapheme components were drawn out of order or brought together by DCH so they remain separate cells.
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

/// Feeds `corpus()` followed by enough lines to push every corpus row into scrollback.
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

/// Asserts that `materialize_scrollback_row_full` matches `materialize_from_line` on `cells` and `get_extra`.
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

/// Grapheme components drawn into separate cells remain separate across the ring tier, the backing store, and history rewrap.
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

        term.resize(3, COLS + 7);
        term.resize(3, COLS);
        term.scroll_to_top();
        assert_eq!(top_row(&term), drawn, "{label}: after a history rewrap");
    }
}

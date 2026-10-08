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
//! `take_ring_fast_materialize` is asserted non-zero on the ring corpus (the
//! fast path fired) and zero on a read from the tiered store.

use aterm_core::grid::materialize_from_line;
use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;
use aterm_grid::test_counters::take_ring_fast_materialize;

const ROWS: u16 = 6;
const COLS: u16 = 40;

/// The corpus. Every line is one physical row's worth of content and exercises
/// a different edge of the serializer/parser pair.
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
        // Empty row (zero length).
        String::new(),
    ]
}

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

/// Every RING-tier history row materializes identically both ways, and the fast
/// path really is the one producing the direct reads.
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

/// The mirror image: a row that has left the ring for the tiered store is not
/// claimed by the fast path, and still materializes identically.
#[test]
fn tiered_rows_fall_back_to_the_line_path() {
    const RING: usize = 8;
    let mut term = Terminal::with_scrollback(ROWS, COLS, RING, Scrollback::with_defaults());
    fill(&mut term);
    // Settle the lazy buffer into the tiers so the deep reads are real tier reads.
    let _ = term.grid_mut().scrollback_mut();
    let depth = term.grid().scrollback_lines();
    assert!(
        depth > RING + 2,
        "fixture history ({depth}) does not reach past the {RING}-line ring"
    );

    // The oldest rows are the tiered ones (the ring holds the newest).
    let _ = take_ring_fast_materialize();
    let deepest = depth - 1;
    for rev_idx in [deepest, deepest - 1, deepest - 2] {
        assert_row_parity(&term, rev_idx);
    }
    assert_eq!(
        take_ring_fast_materialize(),
        0,
        "the ring fast path claimed a row that has left the ring"
    );

    // ...while the newest rows, still in the ring, DO take it.
    for rev_idx in 0..RING {
        assert_row_parity(&term, rev_idx);
    }
    assert_eq!(
        take_ring_fast_materialize(),
        RING,
        "the newest {RING} rows are in the ring and must take the fast path"
    );
}

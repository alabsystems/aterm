// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn scrollback_selection_reads_each_history_line_once() {
    use crate::selection::{SelectionSide, SelectionType};
    use crate::terminal::Terminal;

    const ROWS: u16 = 4;
    const COLS: u16 = 16;
    const FILL: usize = 60;
    const SELECTED: usize = 40;

    let mut term = Terminal::new(ROWS, COLS);
    let mut corpus = String::new();
    for i in 0..FILL {
        corpus.push_str(&format!("row{i:03}\r\n"));
    }
    term.process(corpus.as_bytes());
    let _ = aterm_grid::test_counters::take_row_to_line_ops();

    let selected_rows = i32::try_from(SELECTED).expect("SELECTED fits i32");
    let sel = term.text_selection_mut();
    sel.start_selection(
        -selected_rows,
        0,
        SelectionSide::Left,
        SelectionType::Simple,
    );
    sel.update_selection(-1, COLS - 1, SelectionSide::Right);
    sel.complete_selection();

    let text = term
        .selection_to_string()
        .expect("a 40-row scrollback selection copies text");
    let reads = aterm_grid::test_counters::take_row_to_line_ops();

    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        SELECTED,
        "a {SELECTED}-row hard-broken selection must copy {SELECTED} lines"
    );
    let newest = FILL - usize::from(ROWS);
    assert_eq!(
        lines[SELECTED - 1].trim_end(),
        format!("row{newest:03}"),
        "the last copied line is not the newest selected history row"
    );
    assert_eq!(
        lines[0].trim_end(),
        format!("row{:03}", newest + 1 - SELECTED),
        "the first copied line is not the oldest selected history row"
    );

    assert_eq!(
        reads, SELECTED,
        "a {SELECTED}-row scrollback selection performed {reads} tier line \
         constructions; one per selected row is the whole point of the hoist \
         (the double fetch cost 2N-1)"
    );
}

fn copy_rows(term: &mut crate::terminal::Terminal, first: i32, last: i32) -> Option<String> {
    use crate::selection::{SelectionSide, SelectionType};
    let cols = term.cols();
    let sel = term.text_selection_mut();
    sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(last, cols - 1, SelectionSide::Right);
    sel.complete_selection();
    term.selection_to_string()
}

#[test]
fn line_selection_copies_every_highlighted_row_before_column_expansion() {
    use crate::selection::{SelectionSide, SelectionType};
    let mut term = crate::terminal::Terminal::new(3, 10);
    term.process(b"first row\r\nsecond row");
    let selection = term.text_selection_mut();
    selection.start_selection(0, 5, SelectionSide::Right, SelectionType::Lines);
    selection.update_selection(1, 0, SelectionSide::Left);
    selection.complete_selection();
    assert!(
        selection.contains(1, 0),
        "the endpoint row is highlighted whole"
    );
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("first row\nsecond row")
    );
}

#[test]
fn copy_keeps_a_space_printed_in_a_wrapped_rows_last_column() {
    use crate::selection::{SelectionSide, SelectionType};
    let mut term = crate::terminal::Terminal::new(4, 10);
    term.process(b"abcdefghi jklmn\r\npad   ");
    assert_eq!(term.get_line_text(0, None).as_deref(), Some("abcdefghi"));
    assert!(term.row_continues_previous(1), "precondition: a soft wrap");
    assert_eq!(
        copy_rows(&mut term, 0, 2).as_deref(),
        Some("abcdefghi jklmn\npad"),
        "drag over the wrapped line and the next"
    );

    let (first, last) = term.logical_line_span(0);
    let sel = term.text_selection_mut();
    sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Lines);
    sel.update_selection(last, 9, SelectionSide::Right);
    sel.complete_selection();
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("abcdefghi jklmn"),
        "triple-click's line selection"
    );

    term.process(b"\r\n1\r\n2\r\n3\r\n4");
    assert_eq!(term.get_line_text(-3, None).as_deref(), Some("abcdefghi"));
    assert_eq!(
        copy_rows(&mut term, -3, -1).as_deref(),
        Some("abcdefghi jklmn\npad"),
        "the same rows copied from history"
    );
}

#[test]
fn copy_trims_a_line_that_ends_in_blanks_across_a_wrap() {
    use crate::selection::{SelectionSide, SelectionType};
    let mut term = crate::terminal::Terminal::new(4, 10);
    term.process(format!("foo{:12}\r\nnext", "").as_bytes());
    assert!(term.row_continues_previous(1), "precondition: a soft wrap");
    assert_eq!(
        copy_rows(&mut term, 0, 2).as_deref(),
        Some("foo\nnext"),
        "drag"
    );

    let (first, last) = term.logical_line_span(0);
    let sel = term.text_selection_mut();
    sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Lines);
    sel.update_selection(last, 9, SelectionSide::Right);
    sel.complete_selection();
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("foo"),
        "triple-click"
    );

    term.process(b"\r\n1\r\n2\r\n3\r\n4");
    assert_eq!(
        copy_rows(&mut term, -3, -1).as_deref(),
        Some("foo\nnext"),
        "from history"
    );

    let mut term = crate::terminal::Terminal::new(4, 10);
    term.process(format!("a\r\n{:15}\r\nb", "").as_bytes());
    assert_eq!(
        copy_rows(&mut term, 0, 3).as_deref(),
        Some("a\n\nb"),
        "a blank line"
    );
}

#[test]
fn copy_adds_no_blank_for_a_wide_glyphs_early_wrap() {
    let mut term = crate::terminal::Terminal::new(4, 10);
    term.process("x中中中中中y\r\n".as_bytes());
    assert!(term.row_continues_previous(1), "precondition: a soft wrap");
    assert_eq!(
        copy_rows(&mut term, 0, 1).as_deref(),
        Some("x中中中中中y"),
        "on screen"
    );
    term.process(b"1\r\n2\r\n3\r\n");
    assert_eq!(
        copy_rows(&mut term, -2, -1).as_deref(),
        Some("x中中中中中y"),
        "from history"
    );
}

#[test]
fn a_scrollback_copy_measures_ambiguous_characters_as_the_grid_draws_them() {
    use crate::selection::{SelectionSide, SelectionType};
    let mut term = crate::terminal::Terminal::new(4, 20);
    term.apply_config(&crate::config::TerminalConfig {
        ambiguous_width_double: true,
        ..Default::default()
    });
    term.process("→→foo bar\r\n".as_bytes());
    let copy = |term: &mut crate::terminal::Terminal, row: i32| {
        let sel = term.text_selection_mut();
        sel.start_selection(row, 4, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(row, 6, SelectionSide::Right);
        sel.complete_selection();
        term.selection_to_string()
    };
    let smart = crate::selection::SmartSelection::with_builtin_rules();
    assert_eq!(copy(&mut term, 0).as_deref(), Some("foo"), "on screen");
    assert_eq!(
        term.smart_word_at(0, 5, &smart),
        Some((4, 7)),
        "the word under cell 5 on screen"
    );
    term.process(b"1\r\n2\r\n3\r\n4");
    assert_eq!(term.get_line_text(-1, None).as_deref(), Some("→→foo bar"));
    assert_eq!(copy(&mut term, -1).as_deref(), Some("foo"), "from history");
    let sel = term.text_selection_mut();
    sel.start_selection(-1, 4, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(-1, 6, SelectionSide::Right);
    sel.complete_selection();
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("foo"),
        "a block copy from history"
    );
    term.scroll_display(1);
    assert_eq!(
        term.smart_word_at(0, 5, &smart),
        Some((4, 7)),
        "the word under cell 5 scrolled back"
    );
}

#[test]
fn smart_word_at_finds_the_word_in_the_cells_the_grid_drew() {
    let mut term = crate::terminal::Terminal::new(4, 40);
    term.process(
        "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5Hwx https://example.com/a b".as_bytes(),
    );
    assert_eq!(
        term.cell_grapheme(0, 2).as_deref(),
        Some("\u{1f1e7}"),
        "precondition: the flag halves stay two cells"
    );
    assert_eq!(term.cell_grapheme(0, 7).as_deref(), Some("h"));
    let smart = crate::selection::SmartSelection::with_builtin_rules();
    for (col, want) in [
        (4, Some((4, 6))),
        (5, Some((4, 6))),
        (6, None),
        (7, Some((7, 28))),
        (27, Some((7, 28))),
        (29, Some((29, 30))),
        (30, None),
    ] {
        assert_eq!(term.smart_word_at(0, col, &smart), want, "column {col}");
    }
    term.process(b"\r\n1\r\n2\r\n3\r\n4");
    term.scroll_display(1);
    assert_eq!(
        term.display_cell_grapheme(0, 2).as_deref(),
        Some("\u{1f1e7}"),
        "precondition: the ring tier draws the halves as two cells"
    );
    for (col, want) in [
        (2, None),
        (4, Some((4, 6))),
        (5, Some((4, 6))),
        (7, Some((7, 28))),
        (29, Some((29, 30))),
    ] {
        assert_eq!(
            term.smart_word_at(0, col, &smart),
            want,
            "scrollback column {col}"
        );
    }
    assert_eq!(
        term.smart_word_text_at(0, 10, &smart),
        Some(("https://example.com/a".to_string(), 7, 28))
    );
}

const HISTORY_TIERS: [(&str, Option<usize>); 2] = [("ring tier", None), ("past the ring", Some(1))];

fn row_in_history_tier(
    tier: &str,
    ring: Option<usize>,
    (rows, cols): (u16, u16),
    write: &str,
    before_scroll: impl FnOnce(&mut crate::terminal::Terminal),
) -> (crate::terminal::Terminal, i32) {
    use aterm_grid::test_counters::take_ring_fast_materialize;
    let mut term = match ring {
        None => crate::terminal::Terminal::new(rows, cols),
        Some(ring) => crate::terminal::Terminal::with_scrollback(
            rows,
            cols,
            ring,
            crate::scrollback::Scrollback::with_defaults(),
        ),
    };
    term.process(write.as_bytes());
    before_scroll(&mut term);
    term.process(format!("\x1b[{rows};1H{}", "\r\n".repeat(usize::from(rows))).as_bytes());
    let oldest = term.grid().scrollback_lines() - 1;
    let _ = take_ring_fast_materialize();
    let _ = term
        .grid()
        .materialize_scrollback_row_full(oldest, term.cols());
    assert_eq!(
        take_ring_fast_materialize(),
        usize::from(ring.is_none()),
        "{write:?}: precondition: the row is drawn from the {tier}"
    );
    let row = -i32::try_from(oldest + 1).expect("a short history");
    (term, row)
}

#[test]
fn a_history_copy_after_a_live_ambiguous_switch_copies_the_cells_drawn() {
    use crate::selection::{SelectionSide, SelectionType};
    for (tier, ring) in HISTORY_TIERS {
        let (mut term, row) = row_in_history_tier(tier, ring, (4, 20), "→→foo bar", |term| {
            term.apply_config(&crate::config::TerminalConfig {
                ambiguous_width_double: true,
                ..Default::default()
            });
        });
        assert_eq!(term.get_line_text(row, None).as_deref(), Some("→→foo bar"));
        term.scroll_to_top();
        assert_eq!(
            term.display_cell_grapheme(0, 2).as_deref(),
            Some("f"),
            "{tier}: precondition: the row keeps the cells it was drawn in"
        );
        term.scroll_to_bottom();
        assert_eq!(
            term.get_line_text(row, Some((2, 4))).as_deref(),
            Some("foo"),
            "{tier}: a block row"
        );
        for kind in [SelectionType::Simple, SelectionType::Block] {
            let sel = term.text_selection_mut();
            sel.start_selection(row, 2, SelectionSide::Left, kind);
            sel.update_selection(row, 4, SelectionSide::Right);
            sel.complete_selection();
            assert_eq!(
                term.selection_to_string().as_deref(),
                Some("foo"),
                "{tier}: {kind:?}"
            );
        }
    }
}

#[test]
fn a_history_row_draws_a_flag_around_a_mark_as_the_screen_did() {
    let line = "\u{1f1fa}\u{301}\u{1f1f8} ZEBRA";
    let mut live = crate::terminal::Terminal::new(4, 20);
    live.process(line.as_bytes());
    let drawn = |term: &crate::terminal::Terminal, display: bool| -> Vec<String> {
        (0..10)
            .map(|col| {
                if display {
                    term.display_cell_grapheme(0, col)
                } else {
                    term.cell_grapheme(0, col)
                }
                .expect("in range")
            })
            .collect()
    };
    let on_screen = drawn(&live, false);
    assert_eq!(
        on_screen[3], "Z",
        "precondition: one flag cell, then a space"
    );
    for (tier, ring) in HISTORY_TIERS {
        let (mut term, row) = row_in_history_tier(tier, ring, (4, 20), line, |_| {});
        assert_eq!(
            term.get_line_text(row, Some((3, 7))).as_deref(),
            Some("ZEBRA"),
            "{tier}: a copy of the cells ZEBRA is drawn in"
        );
        term.scroll_to_top();
        assert_eq!(drawn(&term, true), on_screen, "{tier}");
    }
}

#[test]
fn a_history_rows_cells_name_the_cells_it_is_drawn_in() {
    let shapes = [
        "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5Hwx yz",
        "\x1b[1;3H\u{1f3fd}\x1b[1;1H\u{1f44d}\x1b[1;5Hwx yz",
        "\x1b[1;2H\u{e9}\x1b[1;1Ha\u{200d}\x1b[1;3Hwx yz",
        "\u{1f1e6}q\u{1f1e7}wx\x1b[1;3H\x1b[P",
        "\u{231a}\u{fe0e}wx \u{2764}\u{fe0f}\u{200d}Qa e\u{301}\u{302}f",
        "abcdefghi\u{2714}\u{fe0f}",
        "\u{4e00}\u{4e8c} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} z",
        "\u{1f1fa}\u{301}\u{1f1f8} wx",
        "\u{e9}\u{200d}\u{1f1fa}\u{1f1f8} wx",
        "plain ascii",
    ];
    for (tier, ring) in HISTORY_TIERS {
        for shape in shapes {
            let (mut term, row) = row_in_history_tier(tier, ring, (4, 10), shape, |_| {});
            let (text, cells) = term.written_line_cells(row).expect("a history row");
            term.scroll_to_top();
            for col in 0..10 {
                let drawn = term.display_cell_grapheme(0, col).expect("in range");
                let Some(byte) = cells.byte_at_column(col) else {
                    assert!(
                        drawn.trim().is_empty(),
                        "{tier} {shape:?}: column {col} draws {drawn:?}, no cell is read there"
                    );
                    continue;
                };
                if drawn.is_empty() {
                    assert_eq!(
                        col.checked_sub(1)
                            .and_then(|left| cells.byte_at_column(left)),
                        Some(byte),
                        "{tier} {shape:?}: the right half at column {col}"
                    );
                    continue;
                }
                assert!(
                    text[byte..].starts_with(&drawn),
                    "{tier} {shape:?}: column {col} names {:?}, the glass draws {drawn:?}",
                    &text[byte..]
                );
            }
        }
    }
}

fn stored_range_text(
    bytes: &[u8],
    ceiling: usize,
    cols: u16,
    range: (u16, u16),
    trailing: super::TrailingBlanks,
) -> String {
    let line = aterm_scrollback::Line::from_bytes(bytes);
    super::scrollback_line_range_text(
        bytes,
        ceiling,
        |text| crate::terminal::search_rows::stored_line_text_cells(&line, text, cols),
        range,
        trailing,
    )
}

#[test]
fn scrollback_line_range_text_rows() {
    let rows = [
        ("Hello", 0, 4, "Hello"),
        ("Hello World", 0, 4, "Hello"),
        ("Hello World", 6, 10, "World"),
        ("你好", 0, 1, "你"),
        ("你好", 0, 3, "你好"),
        ("你好", 1, 2, "你好"),
        ("A你B", 1, 2, "你"),
        ("A你B", 2, 2, "你"),
        ("abc", 1, 1, "b"),
        ("Hi", 10, 20, ""),
        ("", 0, 5, ""),
    ];
    for (text, start_col, end_col, want) in rows {
        assert_eq!(
            stored_range_text(
                text.as_bytes(),
                1024,
                80,
                (start_col, end_col),
                super::TrailingBlanks::Written
            ),
            want,
            "{text:?} cols {start_col}..={end_col}"
        );
    }
}

fn selection_grid() -> crate::terminal::Terminal {
    use crate::selection::{SelectionSide, SelectionType};
    let mut term = crate::terminal::Terminal::new(6, 10);
    term.process(b"R0\r\nR1\r\nR2\r\nR3\r\nR4\r\nR5");
    let sel = term.text_selection_mut();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(5, 9, SelectionSide::Right);
    sel.complete_selection();
    term
}

#[test]
fn selection_cap_rows_truncates_tail_preserving_start() {
    let term = selection_grid();
    let (text, truncated) = term.selection_to_string_capped(3, usize::MAX);
    let copied = text.expect("selection has text");
    assert!(truncated, "hitting the row cap sets the truncation flag");
    assert_eq!(
        copied.split('\n').count(),
        3,
        "row cap bounds the emitted rows to 3"
    );
    assert!(copied.starts_with("R0"), "oldest row (start) preserved");
    assert!(
        !copied.contains("R3"),
        "tail rows (R3..) dropped by the cap"
    );
}

#[test]
fn push_row_capped_enforces_hard_byte_cap() {
    use super::push_row_capped;
    let mut r = String::new();
    assert!(
        !push_row_capped(&mut r, "abc", 10),
        "a row that fits is not clamped"
    );
    assert_eq!(r, "abc");
    assert!(
        push_row_capped(&mut r, "0123456789", 10),
        "an overflowing row is clamped"
    );
    assert_eq!(r.len(), 10, "hard cap: result never exceeds max_bytes");
    let mut r2 = String::from("ab");
    assert!(push_row_capped(&mut r2, "你好", 4));
    assert_eq!(
        r2, "ab",
        "no partial multi-byte char pushed when budget < char width"
    );
    assert!(r2.is_char_boundary(r2.len()));
}

#[test]
fn selection_byte_cap_is_hard_within_a_single_row() {
    let term = selection_grid();
    let (text, truncated) = term.selection_to_string_capped(usize::MAX, 1);
    let copied = text.expect("selection has text");
    assert_eq!(
        copied, "R",
        "mid-row hard cap: exactly max_bytes, not the whole row"
    );
    assert!(truncated, "clamping a single row sets the truncation flag");
}

#[test]
fn scrollback_line_range_text_bounds_scan_to_ceiling() {
    use super::TrailingBlanks;
    let bytes = vec![b'A'; 2000];

    let read = |start, end, ceiling| {
        stored_range_text(&bytes, ceiling, 2048, (start, end), TrailingBlanks::Trim)
    };
    assert_eq!(read(0, 9, 100).len(), 10);

    assert!(
        read(500, 509, 100).is_empty(),
        "columns past the scan ceiling must not be materialized"
    );

    assert_eq!(read(500, 509, 4096).len(), 10);
}

#[test]
fn scrollback_line_range_text_lossy_prefix_is_bounded() {
    use super::TrailingBlanks;
    let mut bytes = vec![0xFFu8; 4096];
    bytes.extend_from_slice(b"tail");
    let out = stored_range_text(&bytes, 64, 80, (0, 3), TrailingBlanks::Trim);
    assert!(out.chars().count() <= 4, "bounded to the requested columns");
    assert!(
        !out.contains("tail"),
        "content past the ceiling is never scanned"
    );
}

#[test]
fn get_line_text_extracts_from_large_injected_scrollback_line() {
    use crate::grid::Grid;
    use crate::scrollback::{Line, Scrollback};
    let big = vec![b'A'; 2 * 1024 * 1024];
    let mut scrollback = Scrollback::with_defaults();
    scrollback.push_line(Line::from_bytes(&big));
    let grid = Grid::with_tiered_scrollback(4, 80, 1000, scrollback);
    let term = crate::terminal::Terminal::with_grid(grid);

    let text = term
        .get_line_text(-1, Some((0, 79)))
        .expect("scrollback line has text");
    assert_eq!(text.len(), 80, "extraction bounded to the 80-column range");
    assert!(
        text.bytes().all(|b| b == b'A'),
        "content preserved within the range"
    );
}

#[test]
fn get_line_text_positive_rows_are_scroll_invariant() {
    let mut term = crate::terminal::Terminal::new(5, 20);
    for i in 0..30 {
        term.process(format!("line{i}\r\n").as_bytes());
    }
    let live: Vec<Option<String>> = (0..5).map(|r| term.get_line_text(r, None)).collect();
    assert_eq!(live[0].as_deref(), Some("line26"), "precondition: live top");

    term.scroll_display(3);
    for (r, want) in live.iter().enumerate() {
        assert_eq!(
            &term.get_line_text(r as i32, None),
            want,
            "live row {r} must read the same while scrolled back"
        );
    }
}

#[test]
fn push_line_text_appends_get_line_text_and_keeps_the_prefix() {
    let mut term = crate::terminal::Terminal::new(6, 12);
    for i in 0..20 {
        term.process(format!("old{i}\r\n").as_bytes());
    }
    term.process("界e\u{301}👨\u{200d}👩 x   \r\n".as_bytes());
    term.process("pad\u{a0}  \r\n\r\n".as_bytes());
    term.scroll_display(4);
    for row in 0..8 {
        let mut out = String::from("pre ");
        let on_screen = term.push_line_text(row, &mut out);
        let want = i32::try_from(row)
            .ok()
            .and_then(|r| term.get_line_text(r, None));
        assert_eq!(on_screen, want.is_some(), "row {row}");
        assert_eq!(
            out,
            format!("pre {}", want.unwrap_or_default()),
            "row {row}"
        );
    }
    assert_eq!(
        term.get_line_text(2, None).as_deref(),
        Some("界e\u{301}👨\u{200d}👩 x"),
        "precondition: the fixture row reads whole"
    );

    assert_eq!(term.get_line_text(5, None).as_deref(), Some(""));
    let mut out = String::from("pre  ");
    assert!(term.push_line_text(5, &mut out));
    assert_eq!(out, "pre  ", "the trim never reaches the prefix");
    assert_ne!(out.trim_end(), out, "control: a whole-buffer trim would");
}

#[test]
fn selection_copy_is_scroll_invariant() {
    use crate::selection::{SelectionSide, SelectionType};
    let mut term = crate::terminal::Terminal::new(5, 20);
    for i in 0..30 {
        term.process(format!("line{i}\r\n").as_bytes());
    }
    let sel = term.text_selection_mut();
    sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 19, SelectionSide::Right);
    sel.complete_selection();
    assert_eq!(term.selection_to_string().as_deref(), Some("line28"));

    term.scroll_display(2);
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("line28"),
        "a viewport scroll must not change what an existing selection copies"
    );
}

fn wrapped_docker_fixture() -> (crate::terminal::Terminal, String) {
    const HEAD: &str =
        "docker run --rm -v $PWD:/w -w /w --network host --name bld-boxy aterm-build rust";
    const TAIL: &str = ":1.85 cargo build --release --features gpu,wayland";
    let cmd = format!("{HEAD}{TAIL}");
    assert_eq!(
        HEAD.len(),
        80,
        "the head must fill the 80-column row exactly"
    );
    let mut term = crate::terminal::Terminal::new(6, 80);
    term.process(cmd.as_bytes());
    term.process(b"\r\nAFTER");
    (term, cmd)
}

fn logical_line(term: &crate::terminal::Terminal, row: i32) -> Option<(String, i32, i32)> {
    let (first, last) = term.logical_line_span(row);
    let (text, _) = term.line_range_cells(first, last)?;
    Some((text, first, last))
}

fn assert_join_names_the_drawn_cells(term: &crate::terminal::Terminal, first: i32, last: i32) {
    let cols = usize::from(term.cols());
    let (text, cells) = term.line_range_cells(first, last).expect("rows to join");
    let rows = usize::try_from(last - first + 1).expect("an ordered span");
    for g in 0..rows * cols {
        let row = usize::try_from(first).expect("a live row") + g / cols;
        let drawn = term.cell_grapheme(row, g % cols).expect("in range");
        let byte = cells.byte_at_column(g);
        if drawn.is_empty() {
            assert_eq!(
                byte,
                cells.byte_at_column(g - 1),
                "column {g}: a right half names its left half"
            );
            continue;
        }
        let Some(byte) = byte else {
            assert!(
                g >= cells.end_col() && drawn == " ",
                "column {g}: no cell in {text:?}"
            );
            continue;
        };
        assert!(
            text[byte..].starts_with(&drawn),
            "column {g} of {text:?} must name cell ({row}, {}) holding {drawn:?}",
            g % cols
        );
    }
}

#[test]
fn line_range_cells_name_the_cells_across_the_wrap() {
    let (term, cmd) = wrapped_docker_fixture();
    let (text, first, last) = logical_line(&term, 0).expect("row 0 has text");
    assert_eq!((first, last), (0, 1));
    assert_eq!(text, cmd, "the joined line must be the whole command");
    assert_join_names_the_drawn_cells(&term, first, last);
    assert_eq!(
        logical_line(&term, 2),
        Some(("AFTER".to_string(), 2, 2)),
        "an unwrapped row is its own logical line, text unchanged"
    );
}

#[test]
fn line_range_cells_name_the_cells_where_a_row_measures_otherwise() {
    let mut term = crate::terminal::Terminal::new(4, 10);
    term.process(
        "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5Hwx yzpqr abcdef\u{2714}\u{fe0f}gh ij"
            .as_bytes(),
    );
    assert_eq!(term.cell_grapheme(0, 2).as_deref(), Some("\u{1f1e7}"));
    assert_eq!(
        term.cell_grapheme(1, 9).as_deref(),
        Some("\u{2714}\u{fe0f}"),
        "precondition: the base and its VS16 are one cell in the last column"
    );
    assert_eq!(term.logical_line_span(0), (0, 2), "precondition: one run");
    assert_join_names_the_drawn_cells(&term, 0, 2);
    assert_join_names_the_drawn_cells(&term, 1, 2);
    assert_join_names_the_drawn_cells(&term, 0, 1);

    let mut term = crate::terminal::Terminal::new(3, 10);
    term.process("abcdefghi\u{2714}\u{fe0f}jk lm".as_bytes());
    let live = term.line_range_cells(0, 1).expect("a live run");
    term.process(b"\r\n1\r\n2\r\n3");
    assert_eq!(term.logical_line_span(-1), (-2, -1));
    assert_eq!(term.line_range_cells(-2, -1), Some(live));
}

#[test]
fn line_range_cells_reads_only_the_text_a_rows_cells_draw() {
    use crate::grid::Grid;
    use crate::scrollback::{Line, Scrollback};
    let big = vec![b'A'; 2 * 1024 * 1024];
    let mut scrollback = Scrollback::with_defaults();
    scrollback.push_line(Line::from_bytes(&big));
    let grid = Grid::with_tiered_scrollback(4, 80, 1000, scrollback);
    let mut term = crate::terminal::Terminal::with_grid(grid);
    let (text, cells) = term.line_range_cells(-1, -1).expect("a history row");
    assert_eq!(text, "A".repeat(80), "the 80 cells the row draws");
    assert_eq!((cells.end_col(), cells.end_byte()), (80, 80));
    term.scroll_display(1);
    let smart = crate::selection::SmartSelection::with_builtin_rules();
    assert_eq!(
        term.smart_word_text_at(0, 5, &smart),
        Some(("A".repeat(80), 0, 80))
    );
}

#[test]
fn logical_line_span_joins_across_the_scrollback_boundary() {
    let mut term = crate::terminal::Terminal::new(2, 10);
    term.process(b"ABCDEFGHIJKL");
    term.process(b"\r\nsecond\r\nthird");
    assert_eq!(
        term.get_line_text(-1, None).as_deref(),
        Some("KL"),
        "precondition: the wrapped TAIL is the newest history row"
    );
    assert_eq!(
        term.logical_line_span(-1),
        (-2, -1),
        "the span must reach back into scrollback for the wrapped head"
    );
    assert_eq!(
        logical_line(&term, -1).map(|(t, ..)| t).as_deref(),
        Some("ABCDEFGHIJKL"),
        "and join the two history rows into the whole logical line"
    );
}

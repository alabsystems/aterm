// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::selection::SelectionType;
use aterm_core::terminal::Terminal;

use super::{select_line, select_word, word_span};

const HEAD: &str =
    "docker run --rm -v $PWD:/w -w /w --network host --name bld-boxy aterm-build rust";
const TAIL: &str = ":1.85 cargo build --release --features gpu,wayland";

fn wrapped_command() -> (Terminal, String) {
    let cmd = format!("{HEAD}{TAIL}");
    assert_eq!(HEAD.len(), 80, "the head must fill the row exactly");
    let mut term = Terminal::new(6, 80);
    term.process(cmd.as_bytes());
    term.process(b"\r\necho AFTER");
    (term, cmd)
}

fn wrapped_word() -> Terminal {
    let mut term = Terminal::new(6, 80);
    let filler = "word ".repeat(14) + "abc ";
    assert_eq!(filler.len(), 74);
    term.process(format!("{filler}STRADDLEWORD tail").as_bytes());
    term
}

fn highlighted_text(term: &Terminal) -> String {
    highlighted_text_from(term, 0)
}

fn highlighted_text_from(term: &Terminal, first_row: i32) -> String {
    let cols = term.cols();
    let rows = i32::from(term.rows());
    let mut out = String::new();
    let mut first = true;
    for row in first_row..rows {
        let sel = term.text_selection();
        let lo = (0..cols).find(|&c| sel.contains(row, c));
        let hi = (0..cols).rev().find(|&c| sel.contains(row, c));
        let (Some(lo), Some(hi)) = (lo, hi) else {
            continue;
        };
        if !first && !term.row_continues_previous(row) {
            out.push('\n');
        }
        first = false;
        out.push_str(&term.get_line_text(row, Some((lo, hi))).unwrap_or_default());
    }
    out
}

#[test]
fn a_line_click_selects_the_whole_soft_wrapped_logical_line() {
    let (mut term, cmd) = wrapped_command();
    let span = select_line(&mut term, 0);
    assert_eq!(span, (0, 1), "the gesture must cover both physical rows");

    let copied = term.selection_to_string().expect("a selection was made");
    assert_eq!(
        copied.len(),
        cmd.len(),
        "the clipboard must carry all {} bytes, not the {} that fit on the row",
        cmd.len(),
        HEAD.len()
    );
    assert_eq!(copied, cmd);
    assert!(
        !copied.contains('\n'),
        "a SOFT wrap must not invent a newline mid-line: {copied:?}"
    );
    assert_eq!(
        highlighted_text(&term),
        copied,
        "the highlight the user sees must be byte-for-byte what they copy"
    );
}

#[test]
fn a_line_click_on_the_continuation_row_selects_the_same_logical_line() {
    let (mut term, cmd) = wrapped_command();
    assert_eq!(select_line(&mut term, 1), (0, 1));
    let copied = term.selection_to_string().expect("a selection was made");
    assert_eq!(copied, cmd);
    assert_eq!(highlighted_text(&term), copied);
}

#[test]
fn a_hard_newline_still_bounds_a_line_click() {
    let (mut term, cmd) = wrapped_command();
    select_line(&mut term, 0);
    let copied = term.selection_to_string().expect("a selection was made");
    assert!(
        !copied.contains("echo AFTER"),
        "the next LOGICAL line must not be swallowed: {copied:?}"
    );
    assert_eq!(copied, cmd);

    assert_eq!(
        select_line(&mut term, 2),
        (2, 2),
        "an unwrapped row is its own logical line"
    );
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("echo AFTER"),
        "and selecting it must not reach back over the hard newline"
    );
}

#[test]
fn a_word_click_selects_a_word_straddling_the_wrap_whole() {
    let mut term = wrapped_word();
    let (start, end) = select_word(&mut term, 0, 76);
    assert_eq!(
        (start, end),
        ((0, 74), (1, 5)),
        "the word's cells must span both rows"
    );
    let copied = term.selection_to_string().expect("a selection was made");
    assert_eq!(copied, "STRADDLEWORD");
    assert_eq!(highlighted_text(&term), copied);

    let (start2, end2) = select_word(&mut term, 1, 2);
    assert_eq!((start2, end2), (start, end));
    assert_eq!(term.selection_to_string().as_deref(), Some("STRADDLEWORD"));
}

#[test]
fn a_word_click_after_an_indic_conjunct_selects_the_word() {
    let c = "\u{0928}\u{094D}\u{0926}\u{0940}";
    let mut term = Terminal::new(6, 40);
    term.process(format!("{c}{c}{c}{c}{c} tail").as_bytes());
    assert_eq!(select_word(&mut term, 0, 17), ((0, 16), (0, 19)));
    assert_eq!(term.selection_to_string().as_deref(), Some("tail"));
    assert_eq!(highlighted_text(&term), "tail");

    let mut term = Terminal::new(6, 20);
    term.process(format!("{c} {c} {c} {c} WORDXYZWORD tail").as_bytes());
    for (row, col) in [(0, 17), (1, 2)] {
        assert_eq!(
            select_word(&mut term, row, col),
            ((0, 16), (1, 6)),
            "click at ({row}, {col})"
        );
        assert_eq!(term.selection_to_string().as_deref(), Some("WORDXYZWORD"));
        assert_eq!(highlighted_text(&term), "WORDXYZWORD");
    }
}

#[test]
fn a_word_click_after_a_sinhala_zwj_conjunct_selects_the_word() {
    let sri = "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}";
    let mut term = Terminal::new(6, 40);
    term.process(format!("{sri}{sri}{sri}{sri}{sri} tail end").as_bytes());
    assert_eq!(select_word(&mut term, 0, 7), ((0, 6), (0, 9)));
    assert_eq!(term.selection_to_string().as_deref(), Some("tail"));
    assert_eq!(highlighted_text(&term), "tail");
}

#[test]
fn a_word_click_on_a_word_a_joiner_spells_selects_it_whole() {
    for (word, last) in [
        (
            "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
            8,
        ),
        ("\u{09B0}\u{200D}\u{09CD}\u{09AF}\u{09BE}\u{09AC}", 5),
        ("\u{182C}\u{1820}\u{1837}\u{180E}\u{1820}", 5),
        ("\u{13000}\u{13431}\u{13001}\u{13430}\u{13002}", 4),
    ] {
        let mut term = Terminal::new(6, 40);
        term.process(format!("x {word} y").as_bytes());
        for col in 2..=last {
            assert_eq!(
                select_word(&mut term, 0, col),
                ((0, 2), (0, last)),
                "{word:?} clicked at column {col}"
            );
            assert_eq!(term.selection_to_string().as_deref(), Some(word));
        }
    }
}

#[test]
fn a_word_wider_than_the_window_is_still_selected_whole() {
    let cols = 80usize;
    let rows = super::WORD_WINDOW_ROWS * 4 + 3;
    let token = "x".repeat(cols * rows);
    let mut term = Terminal::new(
        u16::try_from(rows + 3).unwrap(),
        u16::try_from(cols).unwrap(),
    );
    term.process(format!("{token} tail").as_bytes());
    let mid = i32::try_from(rows / 2).unwrap();
    select_word(&mut term, mid, 10);
    let picked = term.selection_to_string().expect("a selection was made");
    assert_eq!(
        picked.len(),
        token.len(),
        "a token spanning {rows} rows came back {} bytes instead of {} — the window \
         widened too little",
        picked.len(),
        token.len()
    );
    assert!(
        picked.chars().all(|c| c == 'x'),
        "the widened span picked up neighbours"
    );
}

#[test]
fn widening_stops_at_the_end_of_the_logical_line() {
    let mut term = Terminal::new(6, 80);
    term.process(format!("{}ENDTOKEN", "y".repeat(80)).as_bytes());
    term.process(b"\r\nNEXTWORD");
    select_word(&mut term, 1, 2);
    let picked = term.selection_to_string().expect("a selection was made");
    assert!(
        !picked.contains("NEXTWORD"),
        "widening walked past the hard newline into the next logical line: {picked:?}"
    );
}

#[test]
fn a_double_click_measures_ambiguous_characters_as_the_grid_draws_them() {
    let mut term = Terminal::new(4, 20);
    term.apply_config(&aterm_core::config::TerminalConfig {
        ambiguous_width_double: true,
        ..Default::default()
    });
    term.process("→→foo bar\r\n".as_bytes());
    assert!(
        term.grid().is_wide_continuation_at(0, 3),
        "precondition: the second arrow takes cells 2-3"
    );
    for col in 4..=6 {
        assert_eq!(
            select_word(&mut term, 0, col),
            ((0, 4), (0, 6)),
            "col {col}"
        );
        assert_eq!(term.selection_to_string().as_deref(), Some("foo"));
    }
    assert_eq!(select_word(&mut term, 0, 8), ((0, 8), (0, 10)));
    assert_eq!(term.selection_to_string().as_deref(), Some("bar"));

    term.process(b"1\r\n2\r\n3\r\n4");
    let row = (1..=4)
        .map(|n| -n)
        .find(|&r| {
            term.get_line_text(r, None)
                .is_some_and(|t| t.ends_with("foo bar"))
        })
        .expect("the line reached scrollback");
    assert_eq!(select_word(&mut term, row, 5), ((row, 4), (row, 6)));
    assert_eq!(term.selection_to_string().as_deref(), Some("foo"));

    let mut term = Terminal::new(4, 10);
    term.apply_config(&aterm_core::config::TerminalConfig {
        ambiguous_width_double: true,
        ..Default::default()
    });
    term.process("→→→→abcd efgh".as_bytes());
    assert!(term.row_continues_previous(1), "precondition: a soft wrap");
    assert_eq!(select_word(&mut term, 1, 0), ((0, 8), (1, 1)));
    assert_eq!(term.selection_to_string().as_deref(), Some("abcd"));
}

#[test]
fn a_word_click_after_a_last_column_vs16_base_selects_the_word_drawn_there() {
    let mut term = Terminal::new(4, 10);
    term.process("abcd efgh\u{2714}\u{fe0f}ij klm".as_bytes());
    assert_eq!(
        term.cell_grapheme(0, 9).as_deref(),
        Some("\u{2714}\u{fe0f}"),
        "precondition: the base and its VS16 are in the last column"
    );
    assert!(term.row_continues_previous(1), "precondition: a soft wrap");
    assert_eq!(term.cell_grapheme(1, 0).as_deref(), Some("i"));
    assert_eq!(term.cell_grapheme(1, 3).as_deref(), Some("k"));
    let words = |row: i32| {
        [
            ((row - 1, 5), ((row - 1, 5), (row - 1, 8)), "efgh"),
            ((row, 0), ((row, 0), (row, 1)), "ij"),
            ((row, 1), ((row, 0), (row, 1)), "ij"),
            ((row, 3), ((row, 3), (row, 5)), "klm"),
            ((row, 5), ((row, 3), (row, 5)), "klm"),
        ]
    };
    for ((row, col), span, word) in words(1) {
        assert_eq!(
            select_word(&mut term, row, col),
            span,
            "click at ({row}, {col})"
        );
        assert_eq!(term.selection_to_string().as_deref(), Some(word));
        assert_eq!(highlighted_text(&term), word);
    }
    term.process(b"\r\n1\r\n2\r\n3\r\n4");
    assert_eq!(term.get_line_text(-1, None).as_deref(), Some("ij klm"));
    assert!(term.row_continues_previous(-1), "precondition: a soft wrap");
    for ((row, col), span, word) in words(-1) {
        assert_eq!(
            select_word(&mut term, row, col),
            span,
            "click at ({row}, {col})"
        );
        assert_eq!(term.selection_to_string().as_deref(), Some(word));
    }
}

#[test]
fn a_word_click_after_regional_indicators_drawn_apart_selects_the_word_drawn_there() {
    let mut term = Terminal::new(4, 10);
    term.process("\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5Hwx yzpqr st".as_bytes());
    assert_eq!(term.cell_grapheme(0, 0).as_deref(), Some("\u{1f1e6}"));
    assert_eq!(
        term.cell_grapheme(0, 2).as_deref(),
        Some("\u{1f1e7}"),
        "precondition: the flag halves stay two cells"
    );
    assert_eq!(term.cell_grapheme(0, 4).as_deref(), Some("w"));
    assert_eq!(term.cell_grapheme(0, 9).as_deref(), Some("p"));
    assert!(term.row_continues_previous(1), "precondition: a soft wrap");
    assert_eq!(term.cell_grapheme(1, 0).as_deref(), Some("q"));
    for ((row, col), span, word) in [
        ((0, 4), ((0, 4), (0, 5)), "wx"),
        ((0, 5), ((0, 4), (0, 5)), "wx"),
        ((0, 7), ((0, 7), (1, 1)), "yzpqr"),
        ((0, 9), ((0, 7), (1, 1)), "yzpqr"),
        ((1, 0), ((0, 7), (1, 1)), "yzpqr"),
        ((1, 4), ((1, 3), (1, 4)), "st"),
    ] {
        assert_eq!(
            select_word(&mut term, row, col),
            span,
            "click at ({row}, {col})"
        );
        assert_eq!(term.selection_to_string().as_deref(), Some(word));
        assert_eq!(highlighted_text(&term), word);
    }
    assert!(word_span(&term, 0, 2).is_none());

    term.process(b"\r\n1\r\n2\r\n3\r\n4");
    assert_eq!(term.get_line_text(-1, None).as_deref(), Some("qr st"));
    assert!(term.row_continues_previous(-1), "precondition: a soft wrap");
    term.scroll_display(2);
    assert_eq!(
        term.display_cell_grapheme(0, 2).as_deref(),
        Some("\u{1f1e7}"),
        "precondition: the ring tier draws the halves as two cells"
    );
    assert_eq!(term.display_cell_grapheme(0, 4).as_deref(), Some("w"));
    term.scroll_to_bottom();
    for ((row, col), span, word) in [
        ((-2, 4), ((-2, 4), (-2, 5)), "wx"),
        ((-2, 5), ((-2, 4), (-2, 5)), "wx"),
        ((-2, 7), ((-2, 7), (-1, 1)), "yzpqr"),
        ((-2, 9), ((-2, 7), (-1, 1)), "yzpqr"),
        ((-1, 0), ((-2, 7), (-1, 1)), "yzpqr"),
        ((-1, 4), ((-1, 3), (-1, 4)), "st"),
    ] {
        assert_eq!(
            select_word(&mut term, row, col),
            span,
            "scrollback click at ({row}, {col})"
        );
        assert_eq!(term.selection_to_string().as_deref(), Some(word));
    }
    assert!(word_span(&term, -2, 2).is_none());
}

#[test]
fn a_word_copied_from_history_after_a_live_ambiguous_switch_is_the_word_drawn_there() {
    let mut term = Terminal::new(4, 20);
    term.process("→→foo bar".as_bytes());
    term.apply_config(&aterm_core::config::TerminalConfig {
        ambiguous_width_double: true,
        ..Default::default()
    });
    term.process(b"\r\n1\r\n2\r\n3\r\n4");
    assert_eq!(term.get_line_text(-1, None).as_deref(), Some("→→foo bar"));
    term.scroll_display(1);
    assert_eq!(
        term.display_cell_grapheme(0, 2).as_deref(),
        Some("f"),
        "precondition: the row keeps the cells it was drawn in"
    );
    term.scroll_to_bottom();
    for col in 2..=4 {
        assert_eq!(
            select_word(&mut term, -1, col),
            ((-1, 2), (-1, 4)),
            "col {col}"
        );
        assert_eq!(term.selection_to_string().as_deref(), Some("foo"));
        assert_eq!(highlighted_text_from(&term, -1), "foo");
    }
    assert_eq!(select_word(&mut term, -1, 6), ((-1, 6), (-1, 8)));
    assert_eq!(term.selection_to_string().as_deref(), Some("bar"));
}

#[test]
fn unwrapped_lines_and_words_are_unchanged() {
    let mut term = Terminal::new(6, 80);
    term.process(b"alpha beta gamma\r\ndelta epsilon");
    assert_eq!(select_line(&mut term, 0), (0, 0));
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("alpha beta gamma")
    );
    assert_eq!(term.text_selection().selection_type(), SelectionType::Lines);

    assert_eq!(select_word(&mut term, 0, 7), ((0, 6), (0, 9)));
    assert_eq!(term.selection_to_string().as_deref(), Some("beta"));
    assert_eq!(
        term.text_selection().selection_type(),
        SelectionType::Semantic
    );

    assert_eq!(select_word(&mut term, 0, 40), ((0, 40), (0, 40)));
    assert!(word_span(&term, 0, 40).is_none());
}

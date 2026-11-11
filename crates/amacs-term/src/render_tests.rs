// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_layout_keeps_the_echo_area_then_the_modeline() {
    let at = |body, modeline, echo| Layout {
        body,
        modeline,
        echo,
    };
    assert_eq!(layout(0, 1), at(0, None, 0..0));
    assert_eq!(layout(1, 1), at(0, None, 0..1));
    assert_eq!(layout(2, 1), at(0, Some(0), 1..2));
    assert_eq!(layout(24, 0), at(22, Some(22), 23..24));
    assert_eq!(layout(24, 1), at(22, Some(22), 23..24));
    assert_eq!(layout(24, 3), at(20, Some(20), 21..24));
    assert_eq!(layout(3, 9), at(0, None, 0..3));
    assert_eq!(echo_limit(24), 6);
    assert_eq!(echo_limit(3), 1);
}

#[test]
fn a_long_message_wraps_and_completions_are_cut() {
    let size = Size { cols: 10, rows: 8 };
    let echo = EchoLine {
        text: "0123456789abcdefghij-tail".to_string(),
        ..EchoLine::default()
    };
    assert_eq!(
        echo_rows(size, AmbiguousWidth::Single, &echo),
        2,
        "the limit"
    );
    let mut screen = Screen::new(size, AmbiguousWidth::Single);
    let (rows, cursor) = lay_echo(&mut screen, 5..8, &echo);
    assert_eq!((rows, cursor), (3, None));
    assert_eq!(screen.row_text(5).trim_end(), "0123456789");
    assert_eq!(screen.row_text(6).trim_end(), "abcdefghij");
    assert_eq!(screen.row_text(7).trim_end(), "-tail");
    let echo = EchoLine {
        text: "123456789日".to_string(),
        prompt: true,
        ..EchoLine::default()
    };
    let mut screen = Screen::new(size, AmbiguousWidth::Single);
    let (rows, cursor) = lay_echo(&mut screen, 6..8, &echo);
    assert_eq!((rows, cursor), (2, Some(Pos { row: 7, col: 2 })));
    assert_eq!(screen.row_text(7).trim_end(), "日");
    let echo = EchoLine {
        text: "M-x f".to_string(),
        completions: vec!["forward-char".to_string(), "fill".to_string()],
        prompt: true,
    };
    let mut screen = Screen::new(size, AmbiguousWidth::Single);
    assert_eq!(
        lay_echo(&mut screen, 7..8, &echo),
        (1, Some(Pos { row: 7, col: 5 }))
    );
    assert_eq!(screen.row_text(7), "M-x f  {fo");
}

#[test]
fn a_tab_reaches_the_next_four_column_stop() {
    for column in 0..32 {
        let cells = usize::from(tab_cells(column));
        assert_eq!(
            cells,
            amacs::editor::editor_grapheme_columns("\t", column),
            "{column}"
        );
    }
}

#[test]
fn the_modeline_reads_as_emacs_writes_it() {
    let mut modeline = Modeline {
        name: "COMMIT_EDITMSG",
        dirty: true,
        language: Language::GitCommit,
        line: 12,
        column: 4,
        for_word: Some("git"),
    };
    assert_eq!(
        modeline_text(&modeline),
        "-UU-:**- COMMIT_EDITMSG  (GitCommit) L12 C4  [for git · C-c C-c done · C-c C-k abort]"
    );
    modeline.dirty = false;
    modeline.for_word = None;
    assert_eq!(
        modeline_text(&modeline),
        "-UU-:--- COMMIT_EDITMSG  (GitCommit) L12 C4"
    );
}

#[test]
fn a_press_finds_the_byte_drawn_under_it() {
    let row = DrawnRow {
        source: 10..15,
        cells: vec![(0, 0), (1, 1), (4, 3)],
        end: 4,
    };
    assert_eq!(row.byte_at(0), 10);
    assert_eq!(row.byte_at(1), 11);
    assert_eq!(row.byte_at(2), 11);
    assert_eq!(row.byte_at(3), 14);
    assert_eq!(row.byte_at(9), 15);
    assert_eq!(row.column_of(0), 0);
    assert_eq!(row.column_of(4), 3);
    assert_eq!(row.column_of(5), 4);
}

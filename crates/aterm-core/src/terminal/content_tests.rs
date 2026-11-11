// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::Terminal;

#[test]
fn trimmed_row_ranges_match_a_full_cell_scan() {
    for text in [
        "",
        "a",
        "a  ",
        "日e\u{301}",
        "\tZ",
        "abc\r\x1b[K",
        "\x1b[7Gx",
        "👩🏽‍💻a",
        "            ",
        " \u{301}  ",
        " \u{fe0f}  ",
        " \u{200d}🦀  ",
        "\u{3000}\u{a0} ",
        "\x1b[41m\x1b[2K",
        "x\x1b]8;;https://example.com\x1b\\ \x1b]8;;\x1b\\",
    ] {
        let mut term = Terminal::new(4, 12);
        term.process(text.as_bytes());
        for row in 0..4 {
            let cells = term.grid.row_at_screen(row).unwrap();
            for first in 0u16..14 {
                for last in 0u16..14 {
                    let start = if cells.is_cell_wide_continuation(first) {
                        first.saturating_sub(1)
                    } else {
                        first
                    };
                    let end = last.min(11);
                    let end = if cells.is_cell_wide_continuation(end + 1) {
                        (end + 1).min(11)
                    } else {
                        end
                    };
                    let mut expected = String::new();
                    for col in start..=end {
                        super::push_row_cell_text(&term.grid, row, cells, col, &mut expected);
                    }
                    let mut actual = String::from("prefix  ");
                    super::push_visible_row_bounds(
                        &term.grid,
                        row,
                        first,
                        last,
                        super::TrailingBlanks::Trim,
                        &mut actual,
                    );
                    assert_eq!(
                        actual,
                        format!("prefix  {}", expected.trim_end()),
                        "{text:?} row={row} {first}..={last}"
                    );
                }
            }
        }
    }
}

#[test]
fn plain_overwrite_removes_stale_hyperlink_extras_entries() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]8;;https://example.com\x1b\\LINK\x1b]8;;\x1b\\");
    assert_eq!(
        term.hyperlink_at(0, 0),
        Some("https://example.com"),
        "linked text must carry the hyperlink"
    );
    assert_eq!(term.grid().extras().len(), 4, "one entry per linked cell");

    term.process(b"\rplain");

    assert_eq!(term.row_text(0).as_deref(), Some("plain"));
    assert_eq!(term.hyperlink_at(0, 0), None);
    assert_eq!(
        term.grid().extras().len(),
        0,
        "stale extras entries must be removed on overwrite (#7456)"
    );
}

#[test]
fn plain_overwrite_removes_stale_combining_mark_entry() {
    let mut term = Terminal::new(24, 80);
    term.process("e\u{0301}".as_bytes());
    assert_eq!(term.row_text(0).as_deref(), Some("e\u{0301}"));
    assert_eq!(term.grid().extras().len(), 1, "combining mark entry");

    term.process(b"\rx");

    assert_eq!(
        term.row_text(0).as_deref(),
        Some("x"),
        "text API must show the plain char, not the stale mark"
    );
    assert_eq!(
        term.grid().extras().len(),
        0,
        "stale combining-mark entry must be removed on overwrite (#7456)"
    );
}

#[test]
fn styled_overwrite_does_not_resurrect_stale_hyperlink() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]8;;https://example.com\x1b\\L\x1b]8;;\x1b\\");
    assert!(term.hyperlink_at(0, 0).is_some());

    term.process(b"\r\x1b[58:2::255:0:0m\x1b[4mZ\x1b[0m");

    assert_eq!(term.row_text(0).as_deref(), Some("Z"));
    assert_eq!(
        term.hyperlink_at(0, 0),
        None,
        "old hyperlink must not attach to the new styled char (#7456)"
    );
}

#[test]
fn display_row_text_straddling_viewport_reads_each_displayed_row() {
    let mut term = Terminal::new(5, 20);
    for i in 0..30 {
        term.process(format!("line{i}\r\n").as_bytes());
    }
    for (r, want) in ["line26", "line27", "line28", "line29", ""]
        .iter()
        .enumerate()
    {
        assert_eq!(term.display_row_text(r).as_deref(), Some(*want));
    }

    term.grid_mut().scroll_display(2);
    for (r, want) in ["line24", "line25", "line26", "line27", "line28"]
        .iter()
        .enumerate()
    {
        assert_eq!(
            term.display_row_text(r).as_deref(),
            Some(*want),
            "display row {r} of a straddling viewport"
        );
    }

    term.grid_mut().scroll_display(24);
    assert_eq!(term.display_row_text(0).as_deref(), Some("line0"));
    assert_eq!(term.display_row_text(4).as_deref(), Some("line4"));

    term.scroll_to_bottom();
    for r in 0..5 {
        assert_eq!(term.display_row_text(r), term.row_text(r));
    }
}

#[test]
fn display_row_text_straddling_keeps_combining_marks_paired() {
    let mut term = Terminal::new(5, 20);
    for i in 0..28 {
        term.process(format!("line{i}\r\n").as_bytes());
    }
    term.process("acce\u{0301}nt\r\n".as_bytes());
    assert_eq!(term.row_text(3).as_deref(), Some("acce\u{0301}nt"));

    term.grid_mut().scroll_display(1);
    assert_eq!(
        term.display_row_text(4).as_deref(),
        Some("acce\u{0301}nt"),
        "the live accent row keeps its combining mark while straddling"
    );
}

#[test]
fn display_cell_grapheme_reads_the_displayed_row() {
    let mut term = Terminal::new(5, 20);
    for i in 0..30 {
        term.process(format!("line{i}\r\n").as_bytes());
    }
    for r in 0..5 {
        for c in 0..8 {
            assert_eq!(
                term.display_cell_grapheme(r, c),
                term.cell_grapheme(r, c),
                "offset-0 identity at ({r},{c})"
            );
        }
    }

    term.scroll_to_top();
    let spelled: String = (0..5)
        .map(|c| term.display_cell_grapheme(0, c).unwrap())
        .collect();
    assert_eq!(spelled, "line0", "top display row spells the oldest line");

    term.scroll_to_bottom();
    term.grid_mut().scroll_display(2);
    for r in 0..5 {
        let spelled: String = (0..6)
            .map(|c| term.display_cell_grapheme(r, c).unwrap())
            .collect();
        assert_eq!(
            spelled.trim_end(),
            term.display_row_text(r).unwrap().trim_end(),
            "straddling display row {r}"
        );
    }

    assert_eq!(term.display_cell_grapheme(999, 0), None);
    assert_eq!(term.display_cell_grapheme(0, 999), None);
}

#[test]
fn display_cell_grapheme_keeps_history_extras_paired() {
    let mut term = Terminal::new(5, 20);
    term.process("\u{1F980}e\u{0301}x\r\n".as_bytes());
    for i in 0..10 {
        term.process(format!("fill{i}\r\n").as_bytes());
    }
    term.scroll_to_top();
    assert_eq!(
        term.display_cell_grapheme(0, 0).as_deref(),
        Some("\u{1F980}"),
        "history complex cluster stays on its lead cell"
    );
    assert_eq!(
        term.display_cell_grapheme(0, 1).as_deref(),
        Some(""),
        "wide continuation spacer reads as empty"
    );
    assert_eq!(
        term.display_cell_grapheme(0, 2).as_deref(),
        Some("e\u{0301}"),
        "history combining mark stays paired with its base"
    );
    assert_eq!(term.display_cell_grapheme(0, 3).as_deref(), Some("x"));
}

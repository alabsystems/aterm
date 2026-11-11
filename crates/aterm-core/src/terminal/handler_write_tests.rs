// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::char_width;
use crate::terminal::Terminal;

const ENGLAND: &str = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";

fn whole_and_fragmented(text: &str) -> [Terminal; 2] {
    let mut whole = Terminal::new(1, 8);
    whole.process(text.as_bytes());
    let mut fragmented = Terminal::new(1, 8);
    let mut buf = [0u8; 4];
    for c in text.chars() {
        fragmented.process(c.encode_utf8(&mut buf).as_bytes());
    }
    [whole, fragmented]
}

#[test]
fn tag_and_vs_supplement_attach_to_the_previous_cell() {
    for (text, mark, x_col) in [
        ("A\u{E0001}X", '\u{E0001}', 1u16),
        ("A\u{E007F}X", '\u{E007F}', 1),
        ("e\u{E0100}X", '\u{E0100}', 1),
        ("e\u{E01EF}X", '\u{E01EF}', 1),
        ("\u{00E9}\u{E0001}X", '\u{E0001}', 1),
        ("\u{4E00}\u{E0100}X", '\u{E0100}', 2),
    ] {
        for term in whole_and_fragmented(text) {
            let grid = term.grid();
            assert_eq!(
                grid.cell(0, x_col).map(|c| c.char()),
                Some('X'),
                "{text:?}: X is adjacent to the base"
            );
            assert_eq!(term.cursor().col, x_col + 1, "{text:?}: cursor after X");
            assert_eq!(
                grid.cell_extra(0, 0).map(|e| e.combining()),
                Some([mark].as_slice()),
                "{text:?}: the mark attached to the base cell"
            );
            assert_eq!(
                grid.extras().len(),
                1,
                "{text:?}: only the base cell carries an extra"
            );
        }
    }
}

#[test]
fn a_combining_mark_with_no_cell_to_its_left_lands_on_the_cursor_cell() {
    for (input, col, base) in [
        ("abc\r\u{301}", 0u16, 'a'),
        ("abc\u{1b}[1;1H\u{301}", 0, 'a'),
        ("\u{301}", 0, ' '),
    ] {
        let mut term = Terminal::new(1, 8);
        term.process(input.as_bytes());
        assert_eq!(
            term.grid().cell(0, col).map(|c| c.char()),
            Some(base),
            "{input:?}: the base cell is untouched"
        );
        assert_eq!(
            term.grid().cell_extra(0, col).map(|e| e.combining()),
            Some(['\u{301}'].as_slice()),
            "{input:?}: the mark attached to the cell under the cursor \
             instead of being discarded"
        );
        assert_eq!(
            term.cursor().col,
            col,
            "{input:?}: a zero-width mark never moves the cursor"
        );
    }
}

#[test]
fn a_combining_mark_after_a_cursor_motion_uses_the_cell_to_the_left() {
    let mut term = Terminal::new(1, 8);
    term.process("abc\u{1b}[2D\u{301}".as_bytes());
    assert_eq!(term.cursor().col, 1, "the cursor sits over the 'b'");
    assert_eq!(
        term.grid().cell_extra(0, 0).map(|e| e.combining()),
        Some(['\u{301}'].as_slice()),
        "the mark accents the 'a', the cell to the LEFT (iTerm2/Ghostty/kitty); \
         xterm would accent the 'b' here"
    );
    assert!(
        term.grid().cell_extra(0, 1).is_none(),
        "the cell under the cursor is untouched while a left cell exists"
    );
}

#[test]
fn a_c1_control_from_utf8_changes_nothing() {
    // `a` then one control: the single-glyph lane; between two U+00E9s: the bulk lane.
    for text in ["a\u{9b}b", "\u{e9}\u{9b}\u{e9}\u{85}b"] {
        for term in whole_and_fragmented(text) {
            assert_eq!(
                term.cell_grapheme(0, 0).as_deref(),
                Some(&text[..text.chars().next().map_or(0, char::len_utf8)]),
                "{text:?}: no cell holds the control"
            );
            assert!(
                (0..4).all(|col| term.grid().cell_extra(0, col).is_none()),
                "{text:?}"
            );
        }
    }
    // And it leaves a joiner open, as it leaves everything.
    for term in whole_and_fragmented("\u{1F468}\u{200D}\u{9b}\u{1F4BB}") {
        assert_eq!(
            term.grid().cell_extra(0, 0).map(|e| e.combining()),
            Some(['\u{200D}', '\u{1F4BB}'].as_slice())
        );
    }
}

#[test]
fn a_mark_at_column_0_of_a_continued_row_accents_that_cell() {
    let mut term = Terminal::new(2, 4);
    term.process("abcde\r\u{301}".as_bytes());
    assert!(
        term.grid().row(1).is_some_and(aterm_grid::Row::is_wrapped),
        "precondition: the row continues the one above"
    );
    assert!(
        term.grid().cell_extra(0, 3).is_none(),
        "the glyph that ends the row above keeps its text (xterm, Alacritty)"
    );
    assert_eq!(
        term.grid().cell_extra(1, 0).map(|e| e.combining()),
        Some(['\u{301}'].as_slice())
    );
}

#[test]
fn a_mark_at_a_left_margin_stays_inside_it() {
    let mut term = Terminal::new(1, 10);
    term.process("abc\x1b[?69h\x1b[3;6s\x1b[1;3H\u{301}".as_bytes());
    assert!(
        term.grid().cell_extra(0, 1).is_none(),
        "the cell outside the margin is another region's"
    );
    assert_eq!(
        term.grid().cell_extra(0, 2).map(|e| e.combining()),
        Some(['\u{301}'].as_slice())
    );
}

/// `text` printed from column `at` (0-based), left of a 3;6 margin on a row ten wide: whole,
/// and a code point to a chunk, so neither the bulk lane nor a chunk boundary changes it.
fn printed_from_left_of_the_margin(at: u16, text: &str) -> [Terminal; 2] {
    let setup = format!("\x1b[?69h\x1b[3;6s\x1b[1;{}H", at + 1);
    let mut whole = Terminal::new(2, 10);
    whole.process(setup.as_bytes());
    whole.process(text.as_bytes());
    let mut fragmented = Terminal::new(2, 10);
    fragmented.process(setup.as_bytes());
    let mut buf = [0u8; 4];
    for c in text.chars() {
        fragmented.process(c.encode_utf8(&mut buf).as_bytes());
    }
    [whole, fragmented]
}

#[test]
fn a_cluster_continues_the_glyph_printed_up_to_a_left_margin() {
    for (at, text, base, x_col) in [
        (1, "e\u{301}X", "e\u{301}", 2u16),
        (1, "\u{E9}\u{301}X", "\u{E9}\u{301}", 2),
        (1, "\u{2600}\u{FE0F}X", "\u{2600}\u{FE0F}", 3),
        (
            0,
            "\u{1F468}\u{200D}\u{1F4BB}X",
            "\u{1F468}\u{200D}\u{1F4BB}",
            2,
        ),
        (0, "\u{1F44D}\u{1F3FD}X", "\u{1F44D}\u{1F3FD}", 2),
        (0, "\u{1F1FA}\u{1F1F8}X", "\u{1F1FA}\u{1F1F8}", 2),
    ] {
        for term in printed_from_left_of_the_margin(at, text) {
            assert_eq!(
                term.cell_grapheme(0, usize::from(at)).as_deref(),
                Some(base),
                "{text:?}"
            );
            assert_eq!(
                term.grid().cell(0, x_col).map(|c| c.char()),
                Some('X'),
                "{text:?}"
            );
            assert_eq!(term.cursor().col, x_col + 1, "{text:?}");
        }
    }
}

#[test]
fn a_motion_onto_a_left_margin_ends_the_cluster_a_print_there_left_open() {
    for between in [
        "\r",
        "\x08",
        "\x1b[D",
        "\x1b[1;3H",
        "\x1b[3G",
        "\x1b[3`",
        "\x1b7\x1b8",
        "\x1b[I\x1b[Z\x1b[3G",
        "\x1b[K",
    ] {
        let mut term = Terminal::new(2, 10);
        term.process(format!("\x1b[?69h\x1b[3;6s\x1b[1;2He{between}\u{301}").as_bytes());
        assert_eq!(term.cursor().col, 2, "{between:?}: on the margin");
        assert_eq!(
            term.cell_grapheme(0, 1).as_deref(),
            Some("e"),
            "{between:?}"
        );
        assert_eq!(
            term.grid().cell_extra(0, 2).map(|e| e.combining()),
            Some(['\u{301}'].as_slice()),
            "{between:?}: the cursor's own cell takes it"
        );
    }
    // A rendition change moves nothing, so the glyph is still the one before the cursor.
    let mut term = Terminal::new(2, 10);
    term.process("\x1b[?69h\x1b[3;6s\x1b[1;2He\x1b[31m\u{301}".as_bytes());
    assert_eq!(term.cell_grapheme(0, 1).as_deref(), Some("e\u{301}"));
}

#[test]
fn a_rewrap_keeps_the_glyph_before_the_cursor_across_the_row_it_opened() {
    let rewrapped = |before: &str, cols: u16| {
        let mut term = Terminal::new(3, 8);
        term.process(before.as_bytes());
        term.resize(3, cols);
        assert_eq!(term.cursor().col, 0, "{before:?}: on column 0");
        assert!(
            term.grid().row(1).is_some_and(aterm_grid::Row::is_wrapped),
            "{before:?}: of a row that continues the one above"
        );
        term
    };
    let mut term = rewrapped("abcd", 4);
    term.process("\u{301}".as_bytes());
    assert_eq!(term.cell_grapheme(0, 3).as_deref(), Some("d\u{301}"));
    assert!(term.grid().cell_extra(1, 0).is_none());

    for (before, after, joined) in [
        ("\u{1F44D}", "\u{1F3FD}", "\u{1F44D}\u{1F3FD}"),
        (
            "\u{1F468}\u{200D}",
            "\u{1F4BB}",
            "\u{1F468}\u{200D}\u{1F4BB}",
        ),
        ("\u{1F1FA}", "\u{1F1F8}", "\u{1F1FA}\u{1F1F8}"),
    ] {
        let mut term = rewrapped(before, 2);
        term.process(after.as_bytes());
        assert_eq!(
            term.cell_grapheme(0, 0).as_deref(),
            Some(joined),
            "{before:?}"
        );
        assert_eq!(term.cursor().col, 0, "{before:?}: and the cursor stays");
    }

    // A wide glyph that would not fit moved down and left a hole on the row above: the glyph
    // before the cursor is the last one on that row, not the hole.
    let mut term = Terminal::new(3, 8);
    term.process("abc\u{4E2D}\x1b[1;4H".as_bytes());
    term.resize(3, 4);
    assert_eq!((term.cursor().row, term.cursor().col), (1, 0));
    term.process("\u{301}".as_bytes());
    assert_eq!(term.cell_grapheme(0, 2).as_deref(), Some("c\u{301}"));
    assert!(term.grid().cell_extra(0, 3).is_none());

    // A selector can't widen a glyph on the row above, and leaves the cursor where it is.
    let mut term = Terminal::new(3, 8);
    term.process("abc\u{2600}".as_bytes());
    term.resize(3, 4);
    assert_eq!((term.cursor().row, term.cursor().col), (1, 0));
    term.process("\u{FE0F}".as_bytes());
    assert!(!term.grid().cell(0, 3).is_some_and(|c| c.is_wide()));
    assert_eq!((term.cursor().row, term.cursor().col), (1, 0));
    assert_eq!(
        term.cell_grapheme(0, 3).as_deref(),
        Some("\u{2600}\u{FE0F}")
    );

    // A CR there is a motion: it is the column-0 rule again, and so is a cursor a CR had put
    // on a continued column 0 before the rewrap.
    let mut term = rewrapped("abcd", 4);
    term.process("\r\u{301}".as_bytes());
    assert!(term.grid().cell_extra(0, 3).is_none());
    assert_eq!(
        term.grid().cell_extra(1, 0).map(|e| e.combining()),
        Some(['\u{301}'].as_slice())
    );
    let mut term = Terminal::new(4, 4);
    term.process("abcdefgh\r".as_bytes());
    term.resize(4, 2);
    term.process("\u{301}".as_bytes());
    assert_eq!(term.cursor().col, 0);
    assert!(
        term.grid().cell_extra(term.cursor().row - 1, 1).is_none(),
        "no glyph precedes a cursor a CR placed"
    );
}

#[test]
fn a_checkpoint_keeps_the_glyph_before_the_cursor() {
    let restored = |term: &Terminal| {
        let cp = term.checkpoint();
        let mut back = Terminal::new(2, 10);
        back.restore_checkpoint(&cp);
        back
    };
    let mut term = Terminal::new(2, 10);
    term.process("\x1b[?69h\x1b[3;6s\x1b[1;2He".as_bytes());
    let mut back = restored(&term);
    back.process("\u{301}".as_bytes());
    assert_eq!(back.cell_grapheme(0, 1).as_deref(), Some("e\u{301}"));
    // Negative control: the same cursor after a CR is a cursor a motion placed.
    term.process(b"\r");
    let mut back = restored(&term);
    back.process("\u{301}".as_bytes());
    assert_eq!(back.cell_grapheme(0, 1).as_deref(), Some("e"));
}

#[test]
fn a_selector_stays_inside_a_double_width_row() {
    let mut term = Terminal::new(2, 20);
    term.process("\x1b#6\x1b[?69h\x1b[1;15s\x1b[1;10H\u{2600}\u{FE0F}".as_bytes());
    let grid = term.grid();
    assert_eq!(grid.effective_cols_for_row(0), 10);
    assert!(
        !grid.cell(0, 9).is_some_and(|c| c.is_wide()),
        "no room for a spacer on the row's usable last column"
    );
    assert_eq!(grid.cell(0, 10).map(|c| c.char()), Some(' '));
    assert!(!grid.cell(0, 10).is_some_and(|c| c.is_wide_continuation()));
}

/// A motion onto a spacer at a left margin has no glyph before it, and the cursor's own cell is
/// half of one, so the lead takes the mark though it lies outside the margin: nothing reads a
/// spacer's marks.
#[test]
fn a_mark_on_a_spacer_at_a_left_margin_rides_the_lead() {
    let mut term = Terminal::new(1, 10);
    term.process("\u{4E2D}\x1b[?69h\x1b[2;6s\x1b[1;2H\u{301}".as_bytes());
    assert_eq!(term.cell_grapheme(0, 0).as_deref(), Some("\u{4E2D}\u{301}"));
    assert!(term.grid().cell_extra(0, 1).is_none());
}

#[test]
fn a_selector_changes_no_width_across_a_right_margin() {
    let mut term = Terminal::new(1, 8);
    term.process("\x1b[?69h\x1b[1;2sa\u{2600}\u{FE0F}".as_bytes());
    let grid = term.grid();
    assert!(
        !grid.cell(0, 1).is_some_and(|c| c.is_wide()),
        "the VS16 glyph stays narrow on the margin's last column"
    );
    assert!(grid.pending_wrap(), "and its wrap stays pending");
    assert_eq!(
        term.cell_grapheme(0, 1).as_deref(),
        Some("\u{2600}\u{FE0F}")
    );

    let mut term = Terminal::new(1, 8);
    term.process("x\u{1F600}\x1b[?69h\x1b[1;2s\x1b[1;3H\u{FE0E}".as_bytes());
    assert!(
        term.grid().cell(0, 1).is_some_and(|c| c.is_wide()),
        "a glyph straddling the margin keeps both cells, as a VS15 on the line's edge does"
    );
}

#[test]
fn a_vs16_that_cuts_a_wide_glyph_blanks_its_other_half_in_the_pen() {
    let mut term = Terminal::new(1, 8);
    term.process("\u{2600}\u{4E2D}\x1b[44m\x1b[1;2H\u{FE0F}".as_bytes());
    let orphan = *term.grid().cell(0, 2).expect("the orphan's cell");
    assert_eq!(orphan.char(), ' ');
    assert!(
        orphan.colors().bg_is_indexed() && orphan.colors().bg_index() == 4,
        "a print over half a wide glyph blanks the other half with the BCE blank"
    );

    let mut term = Terminal::new(1, 8);
    term.process("\u{2600}\u{4E2D}\x1b[48;2;1;2;3m\x1b[1;2H\u{FE0F}".as_bytes());
    assert_eq!(term.grid().bg_rgb_at(0, 2), Some([1, 2, 3]));
}

#[test]
fn subdivision_flag_occupies_two_cells() {
    for term in whole_and_fragmented(&format!("{ENGLAND}X")) {
        let grid = term.grid();
        assert!(
            grid.cell(0, 0).is_some_and(|c| c.is_wide()),
            "the flag's lead cell is wide"
        );
        assert_eq!(grid.cell(0, 2).map(|c| c.char()), Some('X'));
        assert_eq!(term.cursor().col, 3);
        assert_eq!(
            grid.cell_extra(0, 0).map(|e| e.combining().len()),
            Some(6),
            "all six tags attached to the lead cell"
        );
    }
}

#[test]
fn vs16_widens_exactly_the_text_default_emoji() {
    for (text, wide, x_col) in [
        ("\u{2764}\u{FE0F}X", true, 2u16),
        ("\u{2B06}\u{FE0F}X", true, 2),
        ("\u{2B05}\u{FE0F}X", true, 2),
        ("\u{2713}\u{FE0F}X", false, 1),
        ("\u{2605}\u{FE0F}X", false, 1),
        ("\u{1FB00}\u{FE0F}X", false, 1),
    ] {
        for term in whole_and_fragmented(text) {
            let grid = term.grid();
            assert_eq!(
                grid.cell(0, 0).is_some_and(|c| c.is_wide()),
                wide,
                "{text:?}"
            );
            assert_eq!(grid.cell(0, x_col).map(|c| c.char()), Some('X'), "{text:?}");
            assert_eq!(term.cursor().col, x_col + 1, "{text:?}");
        }
    }
}

#[test]
fn extended_b_vowels_and_finals_join_the_syllable() {
    for text in [
        "\u{1100}\u{1161}\u{11A8}|",
        "\u{A960}\u{D7B0}\u{D7CB}|",
        "\u{1100}\u{D7C6}\u{D7FB}|",
    ] {
        for term in whole_and_fragmented(text) {
            let grid = term.grid();
            assert!(grid.cell(0, 0).is_some_and(|c| c.is_wide()), "{text:?}");
            assert_eq!(grid.cell(0, 2).map(|c| c.char()), Some('|'), "{text:?}");
            assert_eq!(term.cursor().col, 3, "{text:?}");
        }
    }
}

#[test]
fn fast_path_width_matches_table() {
    let blocks: &[(&str, std::ops::Range<u32>)] = &[
        ("Misc Symbols and Dingbats", 0x2600..0x27C0),
        ("CJK", 0x3000..0xA000),
        ("Hangul Jamo", 0x1100..0x1200),
        ("Hangul Jamo Extended-A", 0xA960..0xA980),
        ("Hangul Syllables", 0xAC00..0xD7B0),
        ("Hangul Jamo Extended-B", 0xD7B0..0xD800),
    ];
    for (block, range) in blocks {
        for cp in range.clone() {
            let c = char::from_u32(cp).expect("every block is BMP scalar values");
            assert_eq!(
                char_width(c, false),
                aterm_grapheme::char_width(c),
                "{block}: non-CJK (ambiguous=narrow) width mismatch at U+{cp:04X}"
            );
            assert_eq!(
                char_width(c, true),
                aterm_grapheme::char_width_cjk(c),
                "{block}: CJK (ambiguous=wide) width mismatch at U+{cp:04X}"
            );
        }
    }
    assert_eq!((char_width('★', false), char_width('★', true)), (1, 2));
    assert_eq!((char_width('✨', false), char_width('✨', true)), (2, 2));
    for c in ['\u{AC00}', '\u{D55C}', '\u{AD6D}', '\u{C5B4}', '\u{D7A3}'] {
        assert_eq!(
            char_width(c, false),
            2,
            "U+{:04X} must occupy two cells (ambiguous=narrow)",
            c as u32
        );
        assert_eq!(
            char_width(c, true),
            2,
            "U+{:04X} must occupy two cells (ambiguous=wide)",
            c as u32
        );
    }
}

#[test]
fn known_cjk_width_regressions() {
    assert_eq!(char_width('\u{4DC0}', false), 2);
    assert_eq!(char_width('\u{4DC0}', true), 2);
    assert_eq!(char_width('\u{4DFF}', false), 2);

    assert_eq!(char_width('\u{303F}', false), 1);
    assert_eq!(char_width('\u{303F}', true), 1);

    assert_eq!(char_width('\u{3248}', false), 1);
    assert_eq!(char_width('\u{3248}', true), 2);
    assert_eq!(char_width('\u{324F}', false), 1);
    assert_eq!(char_width('\u{324F}', true), 2);

    assert_eq!(char_width('\u{302E}', false), 2);
    assert_eq!(char_width('\u{302F}', false), 2);

    assert_eq!(char_width('\u{302A}', false), 0);
    assert_eq!(char_width('\u{302D}', false), 0);
    assert_eq!(char_width('\u{3099}', false), 0);
    assert_eq!(char_width('\u{309A}', false), 0);

    assert_eq!(char_width('\u{3040}', false), 1);
    assert_eq!(char_width('\u{4E00}', false), 2);
    assert_eq!(char_width('\u{9FFF}', false), 2);
}

#[test]
fn ambiguous_misc_symbols_take_two_cells_in_wide_mode() {
    use crate::config::TerminalConfig;
    let config = TerminalConfig {
        ambiguous_width_double: true,
        ..TerminalConfig::default()
    };
    for (text, col) in [("★|", 3u16), ("°|", 3), ("♪♥❶|", 7), ("中★中★|", 9)] {
        let mut term = Terminal::new(1, 20);
        term.apply_config(&config);
        term.process(text.as_bytes());
        assert_eq!(term.cursor().col, col, "{text:?}");
    }
    let mut term = Terminal::new(1, 20);
    term.process("★|".as_bytes());
    assert_eq!(term.cursor().col, 2);
}

#[test]
fn skin_tone_folds_onto_an_emoji_presentation_base_in_both_width_modes() {
    use crate::config::TerminalConfig;
    let bar_col = |term: &Terminal| {
        (0..20).find(|&c| term.grid().cell(0, c).is_some_and(|x| x.char() == '|'))
    };
    for wide in [false, true] {
        let config = TerminalConfig {
            ambiguous_width_double: wide,
            ..TerminalConfig::default()
        };
        for (text, bar) in [
            ("\u{26F9}\u{1F3FD}|", if wide { 4u16 } else { 3 }),
            (
                "a\u{26F9}\u{1F3FD}\u{26F9}\u{1F3FD}|",
                if wide { 9 } else { 7 },
            ),
            ("\u{26F9}\u{FE0F}\u{1F3FD}|", 2),
            ("\u{1F44D}\u{1F3FD}|", 2),
        ] {
            let mut live = Terminal::new(1, 20);
            live.apply_config(&config);
            live.process(text.as_bytes());
            assert_eq!(bar_col(&live), Some(bar), "live {text:?} wide={wide}");
            assert_eq!(live.cursor().col, bar + 1, "live {text:?} wide={wide}");

            let cp = live.checkpoint_carry(0).expect("Ground");
            let mut adopted = Terminal::new(1, 20);
            adopted.apply_config(&config);
            adopted.restore_checkpoint(&cp);
            let fresh = Terminal::from_checkpoint(&cp);
            for (at, restored) in [("restore", &adopted), ("from_checkpoint", &fresh)] {
                assert_eq!(bar_col(restored), Some(bar), "{at} {text:?} wide={wide}");
            }

            let mut scrolled = Terminal::new(1, 20);
            scrolled.apply_config(&config);
            scrolled.process(text.as_bytes());
            scrolled.process(b"\r\n");
            scrolled.resize(4, 19);
            let back = (0..4).find_map(|r| {
                (0..19).find(|&c| scrolled.grid().cell(r, c).is_some_and(|x| x.char() == '|'))
            });
            assert_eq!(back, Some(bar), "history {text:?} wide={wide}");
        }
        let mut live = Terminal::new(1, 20);
        live.apply_config(&config);
        live.process("\u{1F590}\u{FE0F}".as_bytes());
        let cp = live.checkpoint_carry(0).expect("Ground");
        let mut adopted = Terminal::new(1, 20);
        adopted.apply_config(&config);
        adopted.restore_checkpoint(&cp);
        let mut fresh = Terminal::from_checkpoint(&cp);
        fresh.apply_config(&config);
        for (at, restored) in [("restore", &mut adopted), ("from_checkpoint", &mut fresh)] {
            restored.process("\u{1F3FD}|".as_bytes());
            assert_eq!(bar_col(restored), Some(2), "{at} split carry wide={wide}");
        }
    }
}

#[test]
fn a_wide_glyph_wrapping_off_the_last_column_repaints_the_orphan_it_bisects() {
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[48;2;10;20;30m\x1b[1;9H\xe4\xb8\xad");
    assert_eq!(
        term.grid().bg_rgb_at(0, 8),
        Some([10, 20, 30]),
        "precondition: the first glyph sits at cols 8-9 on its own bg"
    );

    term.process(b"\x1b[48;2;200;100;50m\x1b[1;10H\xe4\xb8\xad");

    let grid = term.grid();
    assert_eq!(
        grid.cell(1, 0).map(|c| c.char()),
        Some('\u{4E2D}'),
        "precondition: the second glyph wrapped to the next row"
    );
    let tail = grid.cell(0, 9).expect("the blanked tail cell exists");
    let orphan = grid.cell(0, 8).expect("the orphaned head exists");
    assert_eq!(orphan.char(), ' ', "the orphaned wide head is blanked");
    assert!(
        !orphan.is_empty(),
        "a default-coloured orphan renders as a hole beside the blanked tail"
    );
    assert_eq!(
        orphan.colors(),
        tail.colors(),
        "the orphan must carry the same BCE blank as the tail that bisected it"
    );
    assert_eq!(
        grid.bg_rgb_at(0, 8),
        Some([200, 100, 50]),
        "the orphan must resolve the NEW background; without the ring \
         extension it resolves the dead glyph's [10, 20, 30]"
    );
    assert_eq!(
        grid.bg_rgb_at(0, 9),
        Some([200, 100, 50]),
        "the blanked tail must resolve it too — this site kept no ring at \
         all, so even the cell inside the blanked span was stale"
    );
}

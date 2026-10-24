// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content-only readers must observe Unicode writes through the same clock as
//! ASCII, whether the parser receives a whole run or fragmented UTF-8 bytes.

use aterm_core::terminal::Terminal;

fn assert_write_advances(initial: &str, replacement: &str, setup: &str, fragmented: bool) {
    let mut term = Terminal::new(8, 48);
    term.process(setup.as_bytes());
    term.process(b"\x1b[3;7H");
    for text in [initial, replacement] {
        term.process(b"\x1b[3;7H");
        let before = term.content_seq();
        if fragmented {
            for byte in text.as_bytes() {
                term.process(std::slice::from_ref(byte));
            }
        } else {
            term.process(text.as_bytes());
        }
        let frame = term.cell_frame(8, 48);
        assert_eq!(
            frame.cells[2][6].ch,
            text.chars().next().unwrap(),
            "the real terminal must have changed its rendered glyph"
        );
        assert!(
            term.content_seq() > before,
            "rendered {text:?} without advancing the content clock: setup={setup:?}, fragmented={fragmented}, seq={before}"
        );
        assert_eq!(frame.content_seq, term.content_seq());
    }
}

#[test]
fn unicode_bulk_and_fragmented_writes_advance_content_generation() {
    for (initial, replacement) in [
        ("你", "好"),
        ("⠁", "⠂"),
        ("😀", "😁"),
        ("é", "ñ"),
        ("你好", "世界"),
        ("😀😁", "😂😃"),
        ("你😀", "好😁"),
        ("éñ", "øä"),
        ("⠁⠂", "⠄⠈"),
    ] {
        for fragmented in [false, true] {
            assert_write_advances(initial, replacement, "", fragmented);
        }
    }
}

#[test]
fn styled_and_no_wrap_writes_use_the_same_content_clock() {
    for setup in ["\x1b[?7l", "\x1b[38;2;20;40;60m"] {
        for (initial, replacement) in [("你", "好"), ("⠁", "⠂"), ("😀", "😁"), ("ab", "cd")]
        {
            assert_write_advances(initial, replacement, setup, false);
        }
    }
}

#[test]
fn combining_and_presentation_changes_advance_content_generation() {
    for (base, suffix) in [
        ("e", "\u{301}"),
        ("❤", "\u{fe0f}"),
        ("❤\u{fe0f}", "\u{fe0e}"),
        ("👍", "🏽"),
        ("🇺", "🇸"),
        ("👨", "\u{200d}💻"),
    ] {
        let mut term = Terminal::new(8, 48);
        term.process(base.as_bytes());
        let before = term.content_seq();
        let old = term.cell_grapheme(0, 0);
        term.process(suffix.as_bytes());
        assert_ne!(term.cell_grapheme(0, 0), old, "the grapheme really changed");
        assert!(
            term.content_seq() > before,
            "appending {suffix:?} to {base:?} changed the grapheme without invalidating content readers"
        );
    }
}

#[test]
fn combining_without_a_preceding_cell_is_kept_and_advances_content() {
    let mut term = Terminal::new(8, 48);
    let before = term.content_seq();
    term.process("\u{301}\u{fe0f}".as_bytes());
    assert_eq!(
        term.cell_grapheme(0, 0).as_deref(),
        Some(" \u{301}\u{fe0f}"),
        "the mark is not discarded: it lands on the cell under the cursor"
    );
    assert!(
        term.content_seq() > before,
        "a retained mark changed the grapheme: content readers must be invalidated"
    );
    assert_eq!(
        (term.cursor().row, term.cursor().col),
        (0, 0),
        "a zero-width mark still never advances the cursor"
    );
}

#[test]
fn combining_after_carriage_return_accents_the_first_cell() {
    let mut term = Terminal::new(8, 48);
    term.process(b"abc\r");
    let before = term.content_seq();
    term.process("\u{301}".as_bytes());
    assert_eq!(
        term.cell_grapheme(0, 0).as_deref(),
        Some("a\u{301}"),
        "the accent lands on the 'a', as it does in xterm"
    );
    assert_eq!(
        term.row_text(0).as_deref(),
        Some("a\u{301}bc"),
        "and the rest of the row is untouched"
    );
    assert!(
        term.content_seq() > before,
        "the grapheme changed, so content readers are invalidated"
    );
}

#[test]
fn cursor_motion_empty_input_and_rejected_wide_write_do_not_advance_content() {
    let mut term = Terminal::new(8, 48);
    term.process(b"existing");
    let before = term.content_seq();
    term.process(b"\r\x1b[3;7H\x1b[2D\x1b[2C");
    term.process(b"");
    assert_eq!(term.content_seq(), before, "cursor-only work is not output");

    term.process(b"\x1b[?7l\x1b[1;48H");
    let before = term.content_seq();
    term.process("好".as_bytes());
    assert_eq!(
        term.content_seq(),
        before,
        "a wide glyph that cannot fit was not written"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A seamless update carries the parser's partial state.
//!
//! The outgoing engine stops wherever the last PTY read ended (inside an SGR,
//! mid-glyph, halfway through an OSC title) and the rest of that sequence
//! reaches the successor's parser, which must continue from the same state.
//!
//! These drive the GUI's seamless path (`checkpoint_carry_abandoning_partial`
//! on the outgoing engine, `restore_checkpoint` into a fresh successor, then
//! the queued tail) and compare against one engine fed the whole stream.

use aterm_core::terminal::Terminal;

const ROWS: u16 = 4;
const COLS: u16 = 20;

/// Park `head` in an outgoing engine, hand it to a successor the way the
/// seamless update does, and feed the successor `tail`.
fn handed_over(head: &[u8], tail: &[u8]) -> Terminal {
    let mut outgoing = Terminal::new(ROWS, COLS);
    outgoing.process(head);
    let (carry, _) = outgoing.checkpoint_carry_abandoning_partial(0);
    let mut successor = Terminal::new(ROWS, COLS);
    successor.restore_checkpoint(&carry);
    successor.process(tail);
    successor
}

/// As [`handed_over`], but the successor is rebuilt whole from the checkpoint
/// (`Terminal::from_checkpoint`, the other restore door) rather than adopted
/// into a configured engine.
fn rebuilt_from_carry(head: &[u8], tail: &[u8]) -> Terminal {
    let mut outgoing = Terminal::new(ROWS, COLS);
    outgoing.process(head);
    let (carry, _) = outgoing.checkpoint_carry_abandoning_partial(0);
    let mut successor = Terminal::from_checkpoint(&carry);
    successor.process(tail);
    successor
}

/// The same bytes through one engine, never handed over.
fn never_handed_over(head: &[u8], tail: &[u8]) -> Terminal {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(head);
    t.process(tail);
    t
}

fn frame(t: &mut Terminal) -> aterm_core::render::RenderInput {
    t.cell_frame(usize::from(ROWS), usize::from(COLS))
}

fn row(t: &Terminal, row: usize) -> String {
    t.row_text(row).unwrap_or_default().trim_end().to_string()
}

#[test]
fn a_split_sgr_across_the_handoff_applies() {
    let mut successor = handed_over(b"\x1b[38;5;19", b"6mX");
    assert_eq!(row(&successor, 0), "X", "the tail finished the SGR");
    let mut reference = never_handed_over(b"\x1b[38;5;19", b"6mX");
    let (got, want) = (frame(&mut successor), frame(&mut reference));
    assert_eq!(got.cells[0][0].ch, 'X');
    assert_eq!(
        got.cells[0][0].fg, want.cells[0][0].fg,
        "X has the palette-196 foreground"
    );
    assert_eq!(want.cells[0][0].fg, [0xff, 0x00, 0x00], "xterm palette 196");
}

#[test]
fn a_split_utf8_across_the_handoff_is_one_glyph() {
    // 漢 is E6 BC A2 and 😀 is F0 9F 98 80: split after each glyph's first
    // and second bytes.
    for (head, tail) in [
        (&b"a\xe6"[..], &b"\xbc\xa2b"[..]),
        (&b"a\xe6\xbc"[..], &b"\xa2b"[..]),
        (&b"a\xf0\x9f"[..], &b"\x98\x80b"[..]),
        (&b"a\xc3"[..], &b"\xa9b"[..]),
    ] {
        let successor = handed_over(head, tail);
        assert_eq!(
            row(&successor, 0),
            row(&never_handed_over(head, tail), 0),
            "{head:?} | {tail:?}"
        );
        assert!(
            !row(&successor, 0).contains('\u{FFFD}'),
            "{head:?} | {tail:?}: no replacement character"
        );
    }
}

#[test]
fn an_unterminated_osc_title_completes_after_the_handoff() {
    for (head, tail) in [
        (&b"$ \x1b]0;vim ma"[..], &b"in.rs\x07"[..]),
        (&b"$ \x1b]2;vim main.rs"[..], &b"\x1b\\"[..]),
        (&b"$ \x1b]"[..], &b"0;vim main.rs\x07"[..]),
        (&b"$ \x1b"[..], &b"]0;vim main.rs\x07"[..]),
    ] {
        let successor = handed_over(head, tail);
        assert_eq!(successor.title(), "vim main.rs", "{head:?} | {tail:?}");
        assert_eq!(row(&successor, 0), "$", "{head:?}: nothing printed as text");
    }
}

/// Every split point of a stream that mixes the shapes, carried the seamless
/// way, lands on exactly the screen one engine fed the whole stream draws.
#[test]
fn every_split_of_a_mixed_stream_lands_on_the_same_screen() {
    let stream: &[u8] = b"\x1b[1;38;5;196mred\x1b[0m \x1b]0;t\x07\xe6\xbc\xa2\x1b[?25l\
\x1b[4:3mU\x1b[0m\x1b(0q\x1b(B\x1b7\x1b[2;3HZ\x1b8!";
    let mut reference = never_handed_over(stream, b"");
    let want = frame(&mut reference);
    for split in 0..=stream.len() {
        let (head, tail) = stream.split_at(split);
        let mut successor = handed_over(head, tail);
        assert_eq!(frame(&mut successor).cells, want.cells, "split {split}");
        assert_eq!(successor.title(), reference.title(), "split {split}");
        assert_eq!(successor.cursor(), reference.cursor(), "split {split}");
        assert_eq!(
            successor.cursor_visible(),
            reference.cursor_visible(),
            "split {split}"
        );
    }
}

/// A hostile carry — one that reads but that the parser refuses — costs the
/// partial sequence and nothing else: the successor resumes at Ground.
#[test]
fn a_refused_carry_resumes_at_ground() {
    let mut outgoing = Terminal::new(ROWS, COLS);
    outgoing.process(b"\x1b[38;5;19");
    let (mut carry, _) = outgoing.checkpoint_carry_abandoning_partial(0);
    let parser = carry.parser.as_mut().expect("carried");
    parser.params = vec![1; 64];
    let mut successor = Terminal::new(ROWS, COLS);
    successor.restore_checkpoint(&carry);
    assert!(successor.parser_is_ground());
    successor.process(b"6mX");
    assert_eq!(
        row(&successor, 0),
        "6mX",
        "today's Ground behaviour, no worse"
    );
}

/// Engine state that output still to come relies on (update audit round
/// eight, finding 502). The parser carry
/// continues a sequence split at the park, but these states live in the
/// engine: the character REP (`CSI b`) repeats, an open OSC 8 link, an SGR 58
/// underline colour, a VT52 `ESC Y` waiting for its address bytes, a ZWJ
/// waiting for the emoji it joins, the XTPUSHSGR stack, the XTSAVE'd modes,
/// and a complete flag (else the next regional indicator joins its cell).
/// Each split, handed over through either restore door, must land on the
/// screen one engine fed the whole stream draws: same cells (underline colour
/// included), text, link on every cell and cursor.
#[test]
fn split_at_park_matches_unsplit() {
    let mut lost = Vec::new();
    for (state, head, tail) in [
        ("the REP character", &b"X"[..], &b"\x1b[5b"[..]),
        (
            "an open OSC 8 link",
            b"\x1b]8;id=p;http://a\x1b\\ab",
            b"cd\x1b]8;;\x1b\\",
        ),
        (
            "an SGR 58 underline colour",
            b"\x1b[58:2::255:0:0m\x1b[4m",
            b"err",
        ),
        ("a VT52 address", b"\x1b[?2l\x1bY", b"%%"),
        ("a ZWJ", ZWJ_HEAD.as_bytes(), ZWJ_TAIL.as_bytes()),
        ("a flag", FLAG.as_bytes(), "\u{1F1EC}\u{1F1E7}x".as_bytes()),
        (
            "a flag, then one RI",
            FLAG.as_bytes(),
            "\u{1F1EC}x".as_bytes(),
        ),
        (
            "the XTPUSHSGR stack",
            b"\x1b[31m\x1b[#{\x1b[32mA",
            b"\x1b[#}B",
        ),
        (
            "a selective XTPUSHSGR of the underline",
            b"\x1b[58:2::0:0:255m\x1b[4m\x1b[4#{\x1b[58:2::255:0:0mA",
            b"\x1b[#}B",
        ),
        (
            "an XTSAVE'd mode",
            b"\x1b[?7s\x1b[?7l",
            b"\x1b[?7rABCDEFGHIJKLMNOPQRSTUVWXY",
        ),
    ] {
        let mut reference = never_handed_over(head, tail);
        let links = |t: &Terminal| -> Vec<(Option<String>, Option<String>)> {
            (0..ROWS)
                .flat_map(|r| (0..COLS).map(move |c| (r, c)))
                .map(|(r, c)| {
                    (
                        t.hyperlink_at(r, c).map(str::to_owned),
                        t.hyperlink_id_at(r, c).map(str::to_owned),
                    )
                })
                .collect()
        };
        let text = |t: &Terminal| {
            (0..usize::from(ROWS))
                .map(|r| row(t, r))
                .collect::<Vec<_>>()
        };
        for (door, mut successor) in [
            ("restore_checkpoint", handed_over(head, tail)),
            ("from_checkpoint", rebuilt_from_carry(head, tail)),
        ] {
            if frame(&mut successor).cells != frame(&mut reference).cells
                || text(&successor) != text(&reference)
                || links(&successor) != links(&reference)
                || successor.cursor() != reference.cursor()
            {
                lost.push((state, door));
            }
        }
    }
    assert!(
        lost.is_empty(),
        "the handed-over screen differs from the unsplit one for: {lost:?}"
    );

    // NON-VACUITY: each reference really shows the state the split must keep.
    assert_eq!(row(&never_handed_over(b"X", b"\x1b[5b"), 0), "XXXXXX");
    let link = never_handed_over(b"\x1b]8;id=p;http://a\x1b\\ab", b"cd\x1b]8;;\x1b\\");
    assert_eq!(link.hyperlink_at(0, 3), Some("http://a"), "d is linked");
    assert_eq!(link.hyperlink_id_at(0, 3), Some("p"));
    let mut tinted = never_handed_over(b"\x1b[58:2::255:0:0m\x1b[4m", b"err");
    assert_eq!(
        frame(&mut tinted).cells[0][0].underline_color,
        Some([255, 0, 0])
    );
    let addressed = never_handed_over(b"\x1b[?2l\x1bY", b"%%");
    assert_eq!(row(&addressed, 0), "", "the address prints nothing");
    assert_eq!(addressed.cursor().col, 5, "it moves the cursor");
    let joined = never_handed_over(ZWJ_HEAD.as_bytes(), ZWJ_TAIL.as_bytes());
    assert_eq!(
        joined.cursor().col,
        3,
        "one two-cell glyph, then the x: {:?}",
        row(&joined, 0)
    );
    for tail in ["\u{1F1EC}\u{1F1E7}x", "\u{1F1EC}x"] {
        let flags = never_handed_over(FLAG.as_bytes(), tail.as_bytes());
        assert_eq!(
            (row(&flags, 0), flags.cursor().col),
            (format!("{FLAG}{tail}"), 5),
            "the flag stays whole, the next RI starts a glyph of its own"
        );
    }
    let mut popped = never_handed_over(b"\x1b[31m\x1b[#{\x1b[32mA", b"\x1b[#}B");
    let cells = &frame(&mut popped).cells[0];
    assert_ne!(cells[1].fg, cells[0].fg, "B is red again, A green");
    let mut underline = never_handed_over(
        b"\x1b[58:2::0:0:255m\x1b[4m\x1b[4#{\x1b[58:2::255:0:0mA",
        b"\x1b[#}B",
    );
    let cells = &frame(&mut underline).cells[0];
    assert_eq!(cells[0].underline_color, Some([255, 0, 0]), "A's is red");
    assert_eq!(
        cells[1].underline_color,
        Some([0, 0, 255]),
        "B has the pushed underline colour back, blue"
    );
    let wrapped = never_handed_over(b"\x1b[?7s\x1b[?7l", b"\x1b[?7rABCDEFGHIJKLMNOPQRSTUVWXY");
    assert_eq!(row(&wrapped, 1), "UVWXY", "autowrap is back on");
}

/// 👨 then a ZWJ (U+200D); the tail's 💻 joins them into one glyph.
const ZWJ_HEAD: &str = "\u{1F468}\u{200D}";
const ZWJ_TAIL: &str = "\u{1F4BB}x";

/// A complete flag, two regional indicators (U+1F1FA U+1F1F8): an RI after it
/// starts a pair of its own.
const FLAG: &str = "\u{1F1FA}\u{1F1F8}";

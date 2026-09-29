// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A seamless update carries the parser's partial state (the round-five plan's
//! item 12).
//!
//! The outgoing engine is parked wherever the last PTY read ended, and a read
//! can end anywhere: inside an SGR, between the bytes of a glyph, halfway
//! through an OSC title. The rest of that sequence is still queued on the PTY
//! and reaches the SUCCESSOR's parser. Before the carry the successor started
//! at Ground, so the tail of a split `ESC [ 38;5;196 m` printed `6m`, a split
//! glyph printed replacement characters and a half-arrived title was lost.
//!
//! These drive the seamless path the GUI takes — the capture's
//! `checkpoint_carry_abandoning_partial` on the outgoing engine,
//! `restore_checkpoint` into a fresh successor, then the queued tail fed to
//! the successor — and compare against one engine fed the whole stream.

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
